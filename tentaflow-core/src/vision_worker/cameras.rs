// ===== File: vision_worker/cameras.rs — worker-side camera runtime (Stage B) =====
//
// Executes the core's camera link commands inside a vision worker process:
//
//   * AssignCamera → local `CameraIngestSupervisor::add_camera` (or
//     `restart_camera` when the session already runs — that is also how a
//     credentials rotation reaches a live worker session) + always-on
//     `vision_analysis::ensure_analysis`. Credentials arrive ENCRYPTED and
//     are decrypted lazily by the RTSP session through the shared
//     `<home>/keys/cameras.key`, exactly like on the core.
//   * RemoveCamera → session teardown (which also clears tracker +
//     enrichment state, mirroring the core's remove path).
//   * Detections: one forwarder task per assigned camera drains the LOCAL
//     `detection_bus` into a latest-wins-per-camera coalescing buffer; a
//     single flush task ships one `DetectionsBatch` per tick. Everything is
//     `try_send` past the buffer — link backpressure can NEVER stall the
//     analysis engine (frames are simply dropped; overlays are latest-wins).
//   * Health: one task reports every local session's `CameraHealth` on the
//     heartbeat cadence. The worker never writes the DB — the core merges
//     these reports into its read paths.
//   * StreamStart/StreamStop: per-tile pumps mirroring
//     `camera_relay::server` — subscribe the worker-local StreamHub topic,
//     ship the init segment (awaited — it must not drop) with its
//     `base_pts_ns`, then media frames via `try_send` (a full link queue
//     cuts the pump; the core-side source turns terminal and the tile
//     resubscribes from a fresh init segment).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tracing::{debug, info, warn};

use crate::services::camera_ingest::fakefile::ensure_gst_initialized;
use crate::services::camera_ingest::supervisor::{CameraIngestSupervisor, MAX_CAMERAS_GLOBAL};
use crate::services::camera_ingest::vision_analysis;
use crate::services::detection_bus;
use crate::services::stream_hub::StreamHub;
use crate::services::vision_worker::link::{
    CameraAssignment, DetectionsWire, LinkFrame, HEARTBEAT_INTERVAL,
};

/// Detections leave the worker at most this often, coalesced latest-wins per
/// camera. Slightly above one 25 fps frame period, so a tick usually carries
/// exactly the freshest frame per camera.
const DETECTIONS_FLUSH_INTERVAL: Duration = Duration::from_millis(33);

/// Worker-side owner of camera sessions, detection forwarding and stream
/// pumps. One per worker process, created after the link handshake.
pub struct WorkerCameraRuntime {
    worker_id: u32,
    sup: Arc<CameraIngestSupervisor>,
    /// Shared outbound link queue (drained by the single writer task in
    /// `run_vision_worker`).
    out_tx: mpsc::Sender<LinkFrame>,
    /// Per-camera detection forwarder tasks.
    det_tasks: parking_lot::Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    /// Latest-wins coalescing buffer feeding the flush task.
    pending: Arc<parking_lot::Mutex<HashMap<String, DetectionsWire>>>,
    /// Active per-tile stream pumps keyed by the core-minted stream id.
    stream_pumps: Arc<parking_lot::Mutex<HashMap<u64, tokio::task::JoinHandle<()>>>>,
    /// Flush + health loops (aborted on shutdown).
    background: parking_lot::Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl WorkerCameraRuntime {
    /// Boots the local ingest supervisor and the flush/health loops.
    pub async fn start(worker_id: u32, out_tx: mpsc::Sender<LinkFrame>) -> Result<Arc<Self>> {
        ensure_gst_initialized()
            .map_err(|e| anyhow::anyhow!("gstreamer init: {e}"))
            .context("start worker camera supervisor")?;
        // Per-addon quota is lifted to the global cap: admission control for
        // worker cameras already ran on the core (its own per-addon quota at
        // add time), and one addon routinely owns a worker's whole shard —
        // 120 cameras / 3 workers is 40 sessions per worker for one owner.
        let sup = Arc::new(CameraIngestSupervisor::with_caps(
            MAX_CAMERAS_GLOBAL,
            MAX_CAMERAS_GLOBAL,
        ));
        let runtime = Arc::new(Self {
            worker_id,
            sup,
            out_tx,
            det_tasks: parking_lot::Mutex::new(HashMap::new()),
            pending: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            stream_pumps: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            background: parking_lot::Mutex::new(Vec::new()),
        });

        // Detections flush loop — the ONLY producer of DetectionsBatch.
        {
            let pending = Arc::clone(&runtime.pending);
            let out_tx = runtime.out_tx.clone();
            runtime.background.lock().push(tokio::spawn(async move {
                let mut tick = tokio::time::interval(DETECTIONS_FLUSH_INTERVAL);
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    tick.tick().await;
                    let frames: Vec<DetectionsWire> = {
                        let mut p = pending.lock();
                        if p.is_empty() {
                            continue;
                        }
                        p.drain().map(|(_, v)| v).collect()
                    };
                    match out_tx.try_send(LinkFrame::DetectionsBatch { frames }) {
                        Ok(()) => {}
                        // Link congested: drop this tick — the next one
                        // carries fresher frames anyway (latest-wins).
                        Err(TrySendError::Full(_)) => {
                            debug!("[vision-worker] detections tick dropped (link full)")
                        }
                        Err(TrySendError::Closed(_)) => return,
                    }
                }
            }));
        }

        // Health report loop — heartbeat cadence, only while sessions exist.
        {
            let sup = Arc::clone(&runtime.sup);
            let out_tx = runtime.out_tx.clone();
            runtime.background.lock().push(tokio::spawn(async move {
                let mut tick = tokio::time::interval(HEARTBEAT_INTERVAL);
                loop {
                    tick.tick().await;
                    let cameras = sup.list_handles().await;
                    if cameras.is_empty() {
                        continue;
                    }
                    match out_tx.try_send(LinkFrame::CameraHealthReport { cameras }) {
                        Ok(()) => {}
                        Err(TrySendError::Full(_)) => {}
                        Err(TrySendError::Closed(_)) => return,
                    }
                }
            }));
        }

        Ok(runtime)
    }

    /// Dispatches one core→worker camera frame. Non-blocking: session work is
    /// spawned so the link read loop stays responsive.
    pub fn handle_frame(self: &Arc<Self>, frame: LinkFrame) {
        match frame {
            LinkFrame::AssignCamera { camera } => {
                let rt = Arc::clone(self);
                tokio::spawn(async move { rt.assign(camera).await });
            }
            LinkFrame::RemoveCamera { camera_id } => {
                let rt = Arc::clone(self);
                tokio::spawn(async move { rt.remove(&camera_id).await });
            }
            LinkFrame::StreamStart {
                stream_id,
                camera_id,
                preview,
            } => self.start_stream(stream_id, camera_id, preview),
            LinkFrame::StreamStop { stream_id } => self.stop_stream(stream_id),
            other => debug!(
                worker_id = self.worker_id,
                ?other,
                "[vision-worker] unexpected camera frame"
            ),
        }
    }

    /// AssignCamera: start (or restart with the fresh config) the ingest
    /// session, then the always-on analysis + the detection forwarder.
    async fn assign(self: &Arc<Self>, camera: CameraAssignment) {
        let camera_id = camera.camera_id.clone();
        let cfg = camera.into_config();
        let already_running = self.sup.get_health(&camera_id).await.is_ok();
        let result = if already_running {
            self.sup.restart_camera(&camera_id, cfg).await
        } else {
            self.sup.add_camera(cfg).await
        };
        match result {
            Ok(()) => {
                vision_analysis::ensure_analysis(&camera_id);
                self.ensure_detection_forwarder(&camera_id);
                info!(
                    worker_id = self.worker_id,
                    camera_id = %camera_id,
                    restarted = already_running,
                    "[vision-worker] camera assigned"
                );
            }
            Err(e) => warn!(
                worker_id = self.worker_id,
                camera_id = %camera_id,
                "[vision-worker] camera assign failed: {e}"
            ),
        }
    }

    /// RemoveCamera: tear the session down (this also clears tracker and
    /// enrichment state) and stop forwarding its detections.
    async fn remove(self: &Arc<Self>, camera_id: &str) {
        if let Some(task) = self.det_tasks.lock().remove(camera_id) {
            task.abort();
        }
        self.pending.lock().remove(camera_id);
        match self.sup.remove_camera(camera_id).await {
            Ok(()) => info!(
                worker_id = self.worker_id,
                camera_id, "[vision-worker] camera removed"
            ),
            Err(e) => warn!(
                worker_id = self.worker_id,
                camera_id, "[vision-worker] camera remove failed: {e}"
            ),
        }
    }

    /// Spawns the per-camera detection forwarder if it is not already live.
    /// The task drains the worker-local `detection_bus` into the coalescing
    /// buffer; it never blocks the engine (broadcast lag just skips frames).
    fn ensure_detection_forwarder(self: &Arc<Self>, camera_id: &str) {
        let mut tasks = self.det_tasks.lock();
        if let Some(task) = tasks.get(camera_id) {
            if !task.is_finished() {
                return;
            }
        }
        let pending = Arc::clone(&self.pending);
        let camera_id_owned = camera_id.to_string();
        tasks.insert(
            camera_id.to_string(),
            tokio::spawn(async move {
                let mut rx = detection_bus::subscribe(&camera_id_owned);
                loop {
                    match rx.recv().await {
                        Ok(msg) => {
                            let wire = DetectionsWire {
                                camera_id: msg.camera_id,
                                ts_ms: msg.ts_ms,
                                pts_ns: msg.pts_ns,
                                proc_ms: msg.proc_ms,
                                enriched: msg.enriched,
                                items: msg.items,
                            };
                            pending.lock().insert(camera_id_owned.clone(), wire);
                        }
                        // Fell behind the ring — skip to the freshest frame.
                        Err(RecvError::Lagged(_)) => continue,
                        Err(RecvError::Closed) => return,
                    }
                }
            }),
        );
    }

    /// StreamStart: pump the worker-local StreamHub topic over the link.
    fn start_stream(self: &Arc<Self>, stream_id: u64, camera_id: String, preview: bool) {
        let out_tx = self.out_tx.clone();
        let pumps = Arc::clone(&self.stream_pumps);
        let worker_id = self.worker_id;
        let task = tokio::spawn(async move {
            pump_stream(stream_id, &camera_id, preview, &out_tx, worker_id).await;
            // Pump over (source gone / lag cut / link full): tell the core so
            // its relay source turns terminal, then drop the map entry.
            let _ = out_tx.send(LinkFrame::StreamEnd { stream_id }).await;
            pumps.lock().remove(&stream_id);
        });
        self.stream_pumps.lock().insert(stream_id, task);
    }

    /// StreamStop: abort the pump — dropping its StreamHub handle releases
    /// the subscriber refcount so the mux branch detaches when unused.
    fn stop_stream(&self, stream_id: u64) {
        if let Some(task) = self.stream_pumps.lock().remove(&stream_id) {
            task.abort();
        }
    }

    /// Stops everything this runtime owns: pumps, forwarders, loops, then the
    /// ingest sessions (bounded per-session inside `drain`).
    pub async fn shutdown(&self) {
        for (_, task) in self.stream_pumps.lock().drain() {
            task.abort();
        }
        for (_, task) in self.det_tasks.lock().drain() {
            task.abort();
        }
        for task in self.background.lock().drain(..) {
            task.abort();
        }
        self.sup.drain().await;
    }
}

/// One per-tile pump: worker-local StreamHub → link frames. Mirrors the
/// camera relay's owner side: the init segment is awaited (mandatory), media
/// rides `try_send`, and any lag/backpressure CUTS the pump so the observer
/// resubscribes from a fresh init segment instead of receiving a torn one.
async fn pump_stream(
    stream_id: u64,
    camera_id: &str,
    preview: bool,
    out_tx: &mpsc::Sender<LinkFrame>,
    worker_id: u32,
) {
    let topic = if preview {
        format!("camera:{camera_id}#preview")
    } else {
        format!("camera:{camera_id}")
    };
    let handle = match StreamHub::global().subscribe(&topic).await {
        Ok(h) => h,
        Err(e) => {
            debug!(
                worker_id,
                stream_id,
                topic = %topic,
                "[vision-worker] stream subscribe failed: {e}"
            );
            return;
        }
    };

    // fMP4 needs the preamble — a source that produced none is unusable.
    let Some(init) = handle.init_segment.clone() else {
        debug!(
            worker_id,
            stream_id,
            topic = %topic,
            "[vision-worker] no init segment — pump aborted"
        );
        return;
    };
    let init_frame = LinkFrame::StreamFrame {
        stream_id,
        is_init: true,
        base_pts_ns: handle.base_pts_ns,
        data: init.to_vec(),
    };
    if out_tx.send(init_frame).await.is_err() {
        return;
    }

    let mut receiver = handle.receiver;
    loop {
        match receiver.recv().await {
            Ok(chunk) => {
                let frame = LinkFrame::StreamFrame {
                    stream_id,
                    is_init: false,
                    base_pts_ns: None,
                    data: chunk.to_vec(),
                };
                match out_tx.try_send(frame) {
                    Ok(()) => {}
                    // Link congested: cut this pump rather than buffering a
                    // torn stream — the tile resubscribes from a fresh init.
                    Err(TrySendError::Full(_)) => {
                        debug!(
                            worker_id,
                            stream_id, "[vision-worker] stream pump cut (link full)"
                        );
                        return;
                    }
                    Err(TrySendError::Closed(_)) => return,
                }
            }
            // Fell behind the broadcast ring — a torn fMP4 stream is useless,
            // cut it (mirrors the relay server).
            Err(RecvError::Lagged(_)) => return,
            // Source unregistered (camera session gone) → close.
            Err(RecvError::Closed) => return,
        }
    }
}
