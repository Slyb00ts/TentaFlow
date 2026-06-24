// =============================================================================
// File: services/camera_ingest/vision_analysis.rs — always-on RF-DETR loop
// =============================================================================
//
// Per-camera always-on CV analysis for the Orlen PoC (Phase B). One task per
// camera pulls the latest decoded RGB frame from the running session (via the
// supervisor snapshot path), runs the shared RF-DETR detector, and publishes
// real detections into `detection_bus` — the same contract the dev stub used.
//
// The detector (one 119 MB ONNX session) is a process-wide singleton shared by
// every camera task behind a mutex: analysis is paced at a low fixed rate, so
// serializing inference across cameras keeps a single CPU session predictable.
// A load failure degrades gracefully — the task logs once and exits, leaving
// the camera session and the dashboard untouched (no detections, no crash).

#![cfg(feature = "inference-vision-gpu")]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, OnceCell};
use tracing::{debug, info, warn};

use crate::services::detection_bus::Detection;

use crate::services::detection_bus;
use crate::vision::classifier_stan::StateClassifier;
use crate::vision::detector_rfdetr::RfDetrDetector;
use crate::vision::ocr_plate::PlateOcr;

/// Floor interval for `analysis_fps = 0` (unlimited). ~30 fps native cadence —
/// a hard floor so the loop never busy-spins waiting on frames; inference on CPU
/// is the real ceiling anyway.
const UNLIMITED_INTERVAL: Duration = Duration::from_millis(33);

/// Default analysis cadence when no per-camera value is resolvable (10 fps).
const DEFAULT_ANALYSIS_FPS: u32 = 10;

/// Resolves the loop tick interval from a configured analysis FPS. `0` is
/// unlimited (native cadence floored at [`UNLIMITED_INTERVAL`]); any other
/// value maps to `1000 / fps` ms.
fn interval_for_fps(fps: u32) -> Duration {
    if fps == 0 {
        UNLIMITED_INTERVAL
    } else {
        Duration::from_millis((1000 / fps.max(1)) as u64)
    }
}

/// Reads the per-camera analysis FPS from the core DB, falling back to the
/// default when no pool / row is available.
fn resolve_analysis_fps(camera_id: &str) -> u32 {
    match crate::db::global_pool() {
        Some(pool) => {
            crate::db::repository::camera_analysis_fps(&pool, camera_id).unwrap_or(DEFAULT_ANALYSIS_FPS)
        }
        None => DEFAULT_ANALYSIS_FPS,
    }
}

/// Process-wide RF-DETR detector, loaded on first use. `tokio::sync::OnceCell`
/// so a slow load (~hundreds of ms) does not block the async runtime, and a
/// failed load is retried on the next process start rather than poisoning.
/// `None` inside the `OnceCell` Ok means the load failed once and analysis is
/// disabled for the process lifetime.
fn detector() -> &'static OnceCell<Option<std::sync::Arc<Mutex<RfDetrDetector>>>> {
    static DETECTOR: OnceCell<Option<std::sync::Arc<Mutex<RfDetrDetector>>>> = OnceCell::const_new();
    &DETECTOR
}

async fn get_detector() -> Option<std::sync::Arc<Mutex<RfDetrDetector>>> {
    detector()
        .get_or_init(|| async {
            // Loading touches the filesystem + builds an ONNX session; keep it
            // off the async worker thread.
            tokio::task::spawn_blocking(|| match RfDetrDetector::load() {
                Ok(d) => Some(std::sync::Arc::new(Mutex::new(d))),
                Err(e) => {
                    warn!("[vision_analysis] RF-DETR load failed, analysis disabled: {e:#}");
                    None
                }
            })
            .await
            .unwrap_or(None)
        })
        .await
        .clone()
}

/// Process-wide state classifier, loaded on first use with the same lazy
/// `OnceCell` + `spawn_blocking` pattern as the detector. A failed load is
/// `None` for the process lifetime: detections still publish, just without a
/// `stan` (condition is skipped, never a crash).
fn classifier() -> &'static OnceCell<Option<std::sync::Arc<Mutex<StateClassifier>>>> {
    static CLASSIFIER: OnceCell<Option<std::sync::Arc<Mutex<StateClassifier>>>> =
        OnceCell::const_new();
    &CLASSIFIER
}

pub(crate) async fn get_classifier() -> Option<std::sync::Arc<Mutex<StateClassifier>>> {
    classifier()
        .get_or_init(|| async {
            tokio::task::spawn_blocking(|| match StateClassifier::load() {
                Ok(c) => Some(std::sync::Arc::new(Mutex::new(c))),
                Err(e) => {
                    warn!("[vision_analysis] state classifier load failed, stan skipped: {e:#}");
                    None
                }
            })
            .await
            .unwrap_or(None)
        })
        .await
        .clone()
}

/// Process-wide plate OCR runner, loaded on first use with the same lazy
/// `OnceCell` + `spawn_blocking` pattern as the detector. A failed load is
/// `None` for the process lifetime: detections still publish, just without
/// `tekst` (OCR is skipped, never a crash).
fn ocr() -> &'static OnceCell<Option<std::sync::Arc<Mutex<PlateOcr>>>> {
    static OCR: OnceCell<Option<std::sync::Arc<Mutex<PlateOcr>>>> = OnceCell::const_new();
    &OCR
}

pub(crate) async fn get_ocr() -> Option<std::sync::Arc<Mutex<PlateOcr>>> {
    ocr()
        .get_or_init(|| async {
            tokio::task::spawn_blocking(|| match PlateOcr::load() {
                Ok(o) => Some(std::sync::Arc::new(Mutex::new(o))),
                Err(e) => {
                    warn!("[vision_analysis] plate OCR load failed, tekst skipped: {e:#}");
                    None
                }
            })
            .await
            .unwrap_or(None)
        })
        .await
        .clone()
}

/// True for detection classes whose condition we classify (placards/labels and
/// the environmental/temperature marks). License plates (`tablica_*`) are
/// skipped here — they go to OCR later.
fn wants_state(klasa: &str) -> bool {
    klasa.starts_with("nalepka") || klasa == "znak_srodowiskowy" || klasa == "termometr"
}

/// Extracts an RGB24 rectangle from a tightly packed RGB frame (stride = w*3).
/// `x0`/`y0`/`cw`/`ch` are already pixel coordinates clamped to the frame.
fn crop_rgb(frame: &[u8], frame_w: u32, x0: u32, y0: u32, cw: u32, ch: u32) -> Vec<u8> {
    let stride = frame_w as usize * 3;
    let mut out = Vec::with_capacity(cw as usize * ch as usize * 3);
    for row in 0..ch as usize {
        let src_y = y0 as usize + row;
        let start = src_y * stride + x0 as usize * 3;
        out.extend_from_slice(&frame[start..start + cw as usize * 3]);
    }
    out
}

/// Max cameras stacked into a single detector `Session::run`. One GPU launch is
/// amortized across the batch — the fleet throughput lever.
const MAX_BATCH: usize = 16;

/// Longest idle nap when no camera is due. The loop normally runs back-to-back
/// (continuous batching); this only caps how stale the "nothing due" wait can be
/// (e.g. when the registry is empty) so newly added cameras start promptly.
const IDLE_POLL_MAX: Duration = Duration::from_millis(20);

/// Shortest idle nap, to avoid busy-spinning when the next deadline is imminent.
const IDLE_POLL_MIN: Duration = Duration::from_millis(1);

/// How often each camera's `analysis_fps` is re-read from the core DB so an
/// operator's runtime change applies without restarting anything.
const FPS_RECHECK: Duration = Duration::from_secs(3);

/// One registered camera's scheduling state inside the shared engine.
struct CamSlot {
    fps: u32,
    next_due: std::time::Instant,
    next_fps_check: std::time::Instant,
}

/// Active-camera registry driven by the single engine task. Cameras join via
/// `ensure_analysis` and leave via `drain`.
fn cameras() -> &'static Mutex<HashMap<String, CamSlot>> {
    static CAMS: OnceLock<Mutex<HashMap<String, CamSlot>>> = OnceLock::new();
    CAMS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Handle to the single engine task, so `drain` can abort it.
fn engine_handle() -> &'static Mutex<Option<tokio::task::JoinHandle<()>>> {
    static H: OnceLock<Mutex<Option<tokio::task::JoinHandle<()>>>> = OnceLock::new();
    H.get_or_init(|| Mutex::new(None))
}

/// Registers `camera_id` for always-on analysis and starts the shared engine
/// once. Idempotent: re-subscribing a tile does not duplicate the camera.
pub fn ensure_analysis(camera_id: &str) {
    {
        let mut reg = cameras().lock().unwrap();
        if !reg.contains_key(camera_id) {
            let now = std::time::Instant::now();
            let fps = resolve_analysis_fps(camera_id);
            reg.insert(
                camera_id.to_string(),
                CamSlot {
                    fps,
                    next_due: now,
                    next_fps_check: now + FPS_RECHECK,
                },
            );
            info!("[vision_analysis] camera {camera_id} registered (analysis_fps={fps})");
        }
    }
    start_engine_once();
}

/// Removes every camera and aborts the engine task. Wired into camera shutdown
/// so the ONNX sessions and frame-pull loop stop before GStreamer tears down.
pub fn drain() {
    cameras().lock().unwrap().clear();
    if let Some(handle) = engine_handle().lock().unwrap().take() {
        handle.abort();
    }
    // Tear down the cold path too, so its classifier/OCR runners stop and a later
    // `ensure_analysis` restarts a fresh consumer (the channel is recreated).
    if let Some(handle) = cold_handle().lock().unwrap().take() {
        handle.abort();
    }
    *cold_chan().lock().unwrap() = None;
    // Reset coalescer state + byte counter: the aborted consumer never released
    // its in-flight slots, so stale `in_flight = true` would permanently block
    // those cameras after a restart.
    cold_state().lock().unwrap().clear();
    cold_bytes().store(0, AtomicOrdering::Relaxed);
}

/// Spawns the single engine task if it is not already running.
fn start_engine_once() {
    let mut h = engine_handle().lock().unwrap();
    if h.as_ref().map(|j| j.is_finished()).unwrap_or(true) {
        *h = Some(tokio::spawn(engine_loop()));
    }
}

/// The one process-wide analysis engine: collects cameras whose per-FPS
/// deadline elapsed, batches up to [`MAX_BATCH`] latest frames into a single
/// detector run, then per camera classifies state + reads plates on crops and
/// publishes. Cross-camera batching is what scales to thousands of cameras.
async fn engine_loop() {
    let detector = match get_detector().await {
        Some(d) => d,
        None => return, // load failed earlier — overlay still works, no detections
    };
    let cold = ensure_cold_started();
    let mut last_metrics = Instant::now();
    info!("[vision_analysis] cross-camera inference engine started (max_batch={MAX_BATCH})");

    // Continuous (adaptive) batching: after each batch we loop IMMEDIATELY and
    // form the next one from whatever became due while the previous inference ran,
    // so the GPU stays back-to-back under load instead of idling to a fixed timer.
    // A fixed tick would cap throughput at MAX_BATCH/tick and waste GPU whenever a
    // batch finishes faster than the tick — which is exactly what FP16/TensorRT do.
    loop {
        let now = std::time::Instant::now();

        if now.duration_since(last_metrics) >= Duration::from_secs(30) {
            let m = metrics();
            info!(
                "[vision_analysis] cold metrics: emitted={} coalesced={} drop_inflight={} drop_full={} drop_budget={} bytes_inflight={}",
                m.emitted.load(AtomicOrdering::Relaxed),
                m.coalesced.load(AtomicOrdering::Relaxed),
                m.dropped_inflight.load(AtomicOrdering::Relaxed),
                m.dropped_full.load(AtomicOrdering::Relaxed),
                m.dropped_budget.load(AtomicOrdering::Relaxed),
                cold_bytes().load(AtomicOrdering::Relaxed),
            );
            last_metrics = now;
        }

        // Collect due cameras + which need an FPS re-read (no DB under the lock).
        // Also track the soonest upcoming deadline so an idle wait sleeps exactly
        // until the next camera is due, not a fixed interval.
        let mut due: Vec<String> = Vec::new();
        let mut recheck: Vec<String> = Vec::new();
        let mut earliest_next: Option<std::time::Instant> = None;
        {
            let reg = cameras().lock().unwrap();
            for (id, slot) in reg.iter() {
                if slot.next_due <= now {
                    due.push(id.clone());
                    if slot.next_fps_check <= now {
                        recheck.push(id.clone());
                    }
                } else {
                    earliest_next = Some(match earliest_next {
                        Some(e) => e.min(slot.next_due),
                        None => slot.next_due,
                    });
                }
            }
        }
        if due.is_empty() {
            let wait = earliest_next
                .map(|t| t.saturating_duration_since(now))
                .unwrap_or(IDLE_POLL_MAX)
                .clamp(IDLE_POLL_MIN, IDLE_POLL_MAX);
            tokio::time::sleep(wait).await;
            continue;
        }
        // Re-read changed FPS values outside the lock.
        for id in &recheck {
            let fps = resolve_analysis_fps(id);
            let mut reg = cameras().lock().unwrap();
            if let Some(slot) = reg.get_mut(id) {
                slot.fps = fps;
                slot.next_fps_check = now + FPS_RECHECK;
            }
        }

        // Take this cycle's batch; cameras beyond MAX_BATCH stay overdue and are
        // picked next tick (drop-nothing, just deferred).
        let batch_ids: Vec<String> = due.iter().take(MAX_BATCH).cloned().collect();

        // Pull the latest frame for each batched camera (async snapshot).
        let mut frames: Vec<(String, std::sync::Arc<[u8]>, u32, u32)> = Vec::new();
        for id in &batch_ids {
            if let Some((rgb, w, h)) =
                crate::addon::host_functions::camera::latest_frame_global(id).await
            {
                frames.push((id.clone(), rgb, w, h));
            }
        }

        // Reschedule every batched camera by its own FPS interval.
        {
            let mut reg = cameras().lock().unwrap();
            for id in &batch_ids {
                if let Some(slot) = reg.get_mut(id) {
                    slot.next_due = now + interval_for_fps(slot.fps);
                }
            }
        }
        if frames.is_empty() {
            continue;
        }

        // HOT PATH: one batched detector run only (no OCR/classify here). EVERY
        // frame — empty or not — flows through the single cold FIFO so overlay
        // clears stay ordered with enriched frames (no stale-frame resurrection).
        // Empty events drop the frame buffer so they cost no memory.
        let detector = detector.clone();
        let detected =
            crate::vision::burn_backend::run_blocking(move || detect_only(detector, frames)).await;
        match detected {
            Ok(per_cam) => {
                for (id, frame, w, h, dets) in per_cam {
                    let sig = detection_sig(&dets);
                    // Empty events drop the frame buffer (no enrichment needed).
                    let frame = if dets.is_empty() {
                        Arc::<[u8]>::from(Vec::new())
                    } else {
                        frame
                    };
                    let bytes = frame.len();
                    // Coalesce / rate-limit / byte-budget gate (reserves the slot).
                    if admit_cold(&id, sig, bytes).is_none() {
                        continue;
                    }
                    let ev = DetectionEvent {
                        camera_id: id.clone(),
                        frame,
                        w,
                        h,
                        detections: dets,
                    };
                    match cold.try_send(ev) {
                        Ok(()) => {
                            commit_cold(&id, sig);
                            metrics().emitted.fetch_add(1, AtomicOrdering::Relaxed);
                        }
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            metrics().dropped_full.fetch_add(1, AtomicOrdering::Relaxed);
                            release_cold(&id, bytes);
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            warn!("[vision_analysis] cold path closed; detections dropped");
                            release_cold(&id, bytes);
                        }
                    }
                }
            }
            Err(e) => warn!("[vision_analysis] detect task panicked: {e}"),
        }
    }
}

/// Bounded cold-path queue capacity. Each NON-empty event carries a full RGB
/// frame (≈6 MB @1080p), so the cap is deliberately small to bound memory; empty
/// events drop the frame and are cheap. Chunk 0b replaces this with a byte-budget
/// + per-camera coalescing.
const COLD_QUEUE_CAP: usize = 32;

/// One detection frame handed from the hot detector to the cold enrichment path.
/// Empty-detection events carry an empty `frame` (no enrichment needed) so they
/// cost no memory; they still flow through the same FIFO so overlay clears stay
/// ordered relative to enriched frames (no stale-frame resurrection).
struct DetectionEvent {
    camera_id: String,
    frame: Arc<[u8]>,
    w: u32,
    h: u32,
    detections: Vec<crate::services::detection_bus::Detection>,
}

/// Live cold-path sender + consumer handle, so `drain` can tear the cold path
/// down and a later `ensure_analysis` can restart it cleanly (an `OnceLock` could
/// not be reset, leaving a dead sender after drain).
fn cold_chan() -> &'static Mutex<Option<mpsc::Sender<DetectionEvent>>> {
    static C: OnceLock<Mutex<Option<mpsc::Sender<DetectionEvent>>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}
fn cold_handle() -> &'static Mutex<Option<tokio::task::JoinHandle<()>>> {
    static H: OnceLock<Mutex<Option<tokio::task::JoinHandle<()>>>> = OnceLock::new();
    H.get_or_init(|| Mutex::new(None))
}

/// Returns a sender into the cold path, spawning the consumer if absent or dead.
fn ensure_cold_started() -> mpsc::Sender<DetectionEvent> {
    let mut chan = cold_chan().lock().unwrap();
    let dead = cold_handle()
        .lock()
        .unwrap()
        .as_ref()
        .map(|j| j.is_finished())
        .unwrap_or(true);
    if let (Some(tx), false) = (chan.as_ref(), dead) {
        return tx.clone();
    }
    let (tx, rx) = mpsc::channel::<DetectionEvent>(COLD_QUEUE_CAP);
    *cold_handle().lock().unwrap() = Some(tokio::spawn(cold_consumer(rx)));
    *chan = Some(tx.clone());
    tx
}

/// Coalescing knobs. Unchanged scenes re-emit at most every `COALESCE_REFRESH`
/// (a parked truck is not re-OCR'd every frame); changed scenes emit as fast as
/// the cold path drains (per-camera in-flight = 1). `BBOX_BUCKET` quantizes boxes
/// so sub-pixel jitter doesn't count as a change.
const COALESCE_REFRESH: Duration = Duration::from_secs(2);
const BBOX_BUCKET: f32 = 0.02;
/// Total frame bytes allowed in flight on the cold path (memory bound that makes
/// the queue cap meaningful — the count cap alone could pin GiBs of RGB frames).
const COLD_BYTE_BUDGET: usize = 256 * 1024 * 1024;

/// Per-camera cold-path scheduling state.
struct ColdCamState {
    last_sig: u64,
    last_emit: Instant,
    in_flight: bool,
}

fn cold_state() -> &'static Mutex<HashMap<String, ColdCamState>> {
    static S: OnceLock<Mutex<HashMap<String, ColdCamState>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Bytes of frame buffers currently queued/in-flight on the cold path.
fn cold_bytes() -> &'static AtomicUsize {
    static B: AtomicUsize = AtomicUsize::new(0);
    &B
}

/// Backpressure counters (cumulative). Logged periodically by the engine loop.
#[derive(Default)]
struct ColdMetrics {
    emitted: AtomicU64,
    coalesced: AtomicU64,
    dropped_inflight: AtomicU64,
    dropped_full: AtomicU64,
    dropped_budget: AtomicU64,
}
fn metrics() -> &'static ColdMetrics {
    static M: OnceLock<ColdMetrics> = OnceLock::new();
    M.get_or_init(ColdMetrics::default)
}

/// Order-independent signature of a frame's worthy detections (class + bucketed
/// box). Same signature ⇒ "same scene" ⇒ coalesce.
fn detection_sig(dets: &[Detection]) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut keys: Vec<(u32, u32, u32, u32, &str)> = dets
        .iter()
        .map(|d| {
            (
                (d.bbox[0] / BBOX_BUCKET) as u32,
                (d.bbox[1] / BBOX_BUCKET) as u32,
                (d.bbox[2] / BBOX_BUCKET) as u32,
                (d.bbox[3] / BBOX_BUCKET) as u32,
                d.klasa.as_str(),
            )
        })
        .collect();
    keys.sort_unstable();
    let mut h = DefaultHasher::new();
    keys.hash(&mut h);
    h.finish()
}

/// Coalesce + rate-limit + byte-budget gate. Decides whether this frame's
/// detections become a cold event. Returns the bytes reserved (to release on
/// failure / completion) or None when the event is dropped. Per-camera ordering
/// is guaranteed by `in_flight = 1`: no second event for a camera is admitted
/// until the consumer clears the flag.
fn admit_cold(camera_id: &str, sig: u64, frame_bytes: usize) -> Option<()> {
    let now = Instant::now();
    let mut st = cold_state().lock().unwrap();
    let entry = st.entry(camera_id.to_string()).or_insert(ColdCamState {
        last_sig: u64::MAX,
        last_emit: now - COALESCE_REFRESH * 2,
        in_flight: false,
    });
    if entry.in_flight {
        // A frame arriving while this camera's previous event is still processing
        // is dropped (not queued) → overlay may show the prior scene until the
        // next frame is admitted (~one cold cycle). Acceptable for a slow ADR
        // gate; a per-camera "pending latest" replacement is a future refinement.
        metrics().dropped_inflight.fetch_add(1, AtomicOrdering::Relaxed);
        return None;
    }
    let unchanged = sig == entry.last_sig;
    if unchanged && now.duration_since(entry.last_emit) < COALESCE_REFRESH {
        metrics().coalesced.fetch_add(1, AtomicOrdering::Relaxed);
        return None;
    }
    if cold_bytes().load(AtomicOrdering::Relaxed) + frame_bytes > COLD_BYTE_BUDGET {
        metrics().dropped_budget.fetch_add(1, AtomicOrdering::Relaxed);
        return None;
    }
    // Reserve only. `last_sig`/`last_emit` are committed by `commit_cold` AFTER a
    // successful send, so a dropped (Full/Closed) event does not record its scene
    // as "recently emitted" and starve the next identical/clear frame.
    entry.in_flight = true;
    cold_bytes().fetch_add(frame_bytes, AtomicOrdering::Relaxed);
    Some(())
}

/// Records a successfully-sent event's scene signature + emit time (coalesce base).
fn commit_cold(camera_id: &str, sig: u64) {
    if let Some(slot) = cold_state().lock().unwrap().get_mut(camera_id) {
        slot.last_sig = sig;
        slot.last_emit = Instant::now();
    }
}

/// Releases a camera's in-flight slot + its reserved bytes. `saturating_sub` so a
/// release racing a `drain()` reset can never underflow the byte counter (which
/// would wrap huge and permanently reject the budget gate).
fn release_cold(camera_id: &str, frame_bytes: usize) {
    if let Some(slot) = cold_state().lock().unwrap().get_mut(camera_id) {
        slot.in_flight = false;
    }
    let _ = cold_bytes().fetch_update(AtomicOrdering::Relaxed, AtomicOrdering::Relaxed, |v| {
        Some(v.saturating_sub(frame_bytes))
    });
}

/// RAII release: guarantees a received cold event's slot + bytes are released
/// exactly once, even if enrichment or publish panics (else the camera leaks its
/// in-flight slot forever and is never analyzed again).
struct ColdSlot {
    camera_id: String,
    bytes: usize,
    released: bool,
}
impl Drop for ColdSlot {
    fn drop(&mut self) {
        if !self.released {
            release_cold(&self.camera_id, self.bytes);
            self.released = true;
        }
    }
}

/// Cold path: enriches each detection frame (state classify + plate OCR on crops)
/// off the hot detector loop, then publishes. Owns the classifier/OCR runners.
async fn cold_consumer(mut rx: mpsc::Receiver<DetectionEvent>) {
    let classifier = get_classifier().await;
    let ocr = get_ocr().await;
    info!("[vision_analysis] cold enrichment consumer started");
    while let Some(ev) = rx.recv().await {
        let DetectionEvent {
            camera_id,
            frame,
            w,
            h,
            detections,
        } = ev;
        let bytes = frame.len();
        // RAII: releases this camera's in-flight slot + bytes on drop — including
        // an unwind if publish/enrich panics — so a camera can never be wedged.
        let slot = ColdSlot {
            camera_id: camera_id.clone(),
            bytes,
            released: false,
        };
        // If the camera has an assigned analysis Flow, run it (it owns the
        // OCR/classify/verdict/alert logic); otherwise fall back to the default
        // hardcoded enrichment. Empty-detection events carry a dropped (0-byte)
        // frame and have nothing to enrich/decide, so they skip the flow and
        // fall through to publish an empty set (clearing the overlay) — running
        // the flow on a dropped frame would only fail the vision nodes.
        if !detections.is_empty() {
            if let (Some(flow_id), Some(disp)) = (
                camera_flow_id(&camera_id),
                crate::flow_engine::dispatcher::global_flow_dispatcher(),
            ) {
                // Detach: a flow can run up to its per-frame deadline, so awaiting
                // it here would head-of-line block every other camera's enrichment.
                // The spawned task owns `slot`, releasing this camera's in-flight
                // slot + bytes when the flow finishes. Per-camera in-flight = 1
                // (admit_cold) bounds a camera to one concurrent run; the byte
                // budget bounds the fleet — so detaching stays bounded.
                tokio::spawn(async move {
                    let _slot = slot;
                    run_camera_flow(disp, flow_id, camera_id, frame, w, h, detections).await;
                });
                continue;
            }
        }
        let _slot = slot;
        let classifier = classifier.clone();
        let ocr = ocr.clone();
        let res = crate::vision::burn_backend::run_blocking(move || {
            let mut dets = detections;
            enrich_detections(&classifier, &ocr, &frame, w, h, &mut dets);
            (camera_id, dets)
        })
        .await;
        match res {
            Ok((id, dets)) => detection_bus::publish_detections(&id, dets),
            Err(e) => warn!("[vision_analysis] enrich task panicked: {e}"),
        }
    }
}

/// Cached per-camera analysis-flow assignment. The cold path consults this for
/// every detection event; the short TTL keeps a UI re-assignment visible within
/// a few seconds without acquiring the SQLite mutex on every event (events are
/// already coalesced, but a per-event DB hit across 100 cameras is avoidable
/// load on the tokio worker). `None` is cached too, so an unassigned camera
/// does not re-query each frame.
struct FlowIdCacheEntry {
    flow_id: Option<String>,
    fetched: Instant,
}
const FLOW_ID_TTL: Duration = Duration::from_secs(5);

/// Per-frame deadline for a camera analysis flow. Well under the dispatcher's
/// 120 s default so a misbehaving flow releases the camera's in-flight slot in
/// seconds, not minutes (the slot is held for the whole detached run).
const CAMERA_FLOW_DEADLINE: Duration = Duration::from_secs(15);

fn flow_id_cache() -> &'static Mutex<HashMap<String, FlowIdCacheEntry>> {
    static C: OnceLock<Mutex<HashMap<String, FlowIdCacheEntry>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Returns the camera's assigned analysis flow id, or `None` to fall back to the
/// built-in enrichment path. Cached with [`FLOW_ID_TTL`].
fn camera_flow_id(camera_id: &str) -> Option<String> {
    if let Some(e) = flow_id_cache().lock().unwrap().get(camera_id) {
        if e.fetched.elapsed() < FLOW_ID_TTL {
            return e.flow_id.clone();
        }
    }
    let flow_id = crate::db::global_pool()
        .and_then(|pool| crate::db::repository::camera_analysis_flow_id(&pool, camera_id).ok())
        .flatten();
    flow_id_cache().lock().unwrap().insert(
        camera_id.to_string(),
        FlowIdCacheEntry {
            flow_id: flow_id.clone(),
            fetched: Instant::now(),
        },
    );
    flow_id
}

/// Runs a camera's assigned analysis Flow on one detection frame: stores the raw
/// RGB frame as an Image blob, builds the initial envelope (payload = Image, meta
/// carries the hot detector's detections + camera id), dispatches by flow id and
/// publishes the resulting detections back to the bus so the live overlay
/// reflects the flow's enrichment/verdict. Errors are logged, never fatal — the
/// cold path keeps draining and the slot is released by the caller's `ColdSlot`.
async fn run_camera_flow(
    disp: Arc<crate::flow_engine::dispatcher::FlowDispatcher>,
    flow_id: String,
    camera_id: String,
    frame: Arc<[u8]>,
    w: u32,
    h: u32,
    detections: Vec<Detection>,
) {
    use crate::flow_engine::dispatcher::FlowRequestMeta;
    use crate::flow_engine::envelope::{FlowEnvelope, FlowValue};

    let dets_json = match serde_json::to_value(&detections) {
        Ok(v) => v,
        Err(e) => {
            warn!("[vision_analysis] flow {flow_id}: serialize detections failed: {e}");
            return;
        }
    };
    // Ephemeral frame store: the frame lives in memory only for the duration of
    // this flow run (read by the flow's nodes via the composite ctx.blobs) and is
    // deleted below — it never touches the durable blob store / disk.
    let frame_blobs = disp.frame_blobs();
    let blob_ref = match frame_blobs.put(frame.to_vec(), "image/x-rgb24").await {
        Ok(r) => r,
        Err(e) => {
            warn!("[vision_analysis] flow {flow_id}: blob put failed: {e:#}");
            return;
        }
    };
    let mut env = FlowEnvelope::with_payload(FlowValue::Image {
        blob_ref: blob_ref.clone(),
        mime: "image/x-rgb24".to_string(),
        dims: Some((w, h)),
    });
    env.meta.insert(
        "camera_id".into(),
        serde_json::Value::String(camera_id.clone()),
    );
    env.meta.insert("detections".into(), dets_json);

    // System-triggered execution: there is no user in the loop, so no per-user
    // ACL applies here (the dispatcher treats `user_id = None` as allow). The
    // real authorization gate is the camera→flow assignment write path, which is
    // admin-only and validates flow access before persisting `analysis_flow_id`.
    // A per-frame deadline (well under the dispatcher's 120 s cap) bounds how
    // long one camera's slot stays held if its flow hangs.
    let mut meta = FlowRequestMeta::new(format!("cam-{camera_id}"));
    meta.deadline = Some(Instant::now() + CAMERA_FLOW_DEADLINE);
    match disp.dispatch_by_flow_id(flow_id.clone(), env, meta).await {
        Ok(outcome) => {
            // One concise per-run line so operators can see the assigned flow
            // actually executed on a detection frame + its verdict.
            let verdict = outcome
                .final_envelope
                .meta
                .get("verdict")
                .and_then(|v| v.get("decision"))
                .and_then(|d| d.as_str())
                .unwrap_or("-");
            debug!(
                "[vision_analysis] flow {flow_id} ran for {camera_id}: {} detections, verdict={verdict}",
                detections.len()
            );
            publish_flow_detections(&camera_id, detections, outcome);
        }
        Err(e) => warn!("[vision_analysis] flow {flow_id} dispatch failed: {e}"),
    }
    // Ephemeral frame: drop the in-memory blob now that the flow has read it, so
    // the cold path never accumulates per-frame frames in memory.
    if let Err(e) = frame_blobs.delete(&blob_ref).await {
        warn!("[vision_analysis] flow {flow_id}: frame blob cleanup failed: {e:#}");
    }
}

/// Publishes a finished flow's detections to the live overlay. A flow that
/// enriches (OCR/classify/verdict) writes the updated detection array back into
/// `meta["detections"]`; when present and parseable we publish those, otherwise
/// we publish the original detector boxes so the overlay still shows them.
fn publish_flow_detections(
    camera_id: &str,
    original: Vec<Detection>,
    outcome: crate::flow_engine::envelope::FlowExecutionOutcome,
) {
    let enriched = match outcome.final_envelope.meta.get("detections") {
        Some(v) => match serde_json::from_value::<Vec<Detection>>(v.clone()) {
            Ok(dets) => Some(dets),
            Err(e) => {
                // The flow left a detections value the overlay can't read — fall
                // back to the detector boxes rather than silently dropping them.
                warn!("[vision_analysis] flow {camera_id}: meta[detections] unparseable, publishing original: {e}");
                None
            }
        },
        None => None,
    };
    detection_bus::publish_detections(camera_id, enriched.unwrap_or(original));
}

/// HOT, blocking: one `detect_batch` across the frames. Returns the raw boxes per
/// frame plus the frame buffer, for the cold path to enrich. No OCR/classify here.
fn detect_only(
    detector: Arc<Mutex<RfDetrDetector>>,
    frames: Vec<(String, Arc<[u8]>, u32, u32)>,
) -> Vec<(String, Arc<[u8]>, u32, u32, Vec<crate::services::detection_bus::Detection>)> {
    let batch = {
        let refs: Vec<(&[u8], u32, u32)> =
            frames.iter().map(|(_, rgb, w, h)| (&rgb[..], *w, *h)).collect();
        let mut guard = detector.lock().unwrap();
        match guard.detect_batch(&refs) {
            Ok(b) => b,
            Err(e) => {
                warn!("[vision_analysis] detect_batch failed (n={}): {e:#}", refs.len());
                return Vec::new();
            }
        }
    };
    frames
        .into_iter()
        .zip(batch.into_iter())
        .map(|((id, rgb, w, h), items)| (id, rgb, w, h, items))
        .collect()
}

/// COLD, blocking: per-detection state classify (labels) + plate OCR, mutating
/// `items` in place. Runs off the hot detector loop so its latency never paces
/// detection throughput. Missing runners (`None`) skip that stage, never crash.
fn enrich_detections(
    classifier: &Option<Arc<Mutex<StateClassifier>>>,
    ocr: &Option<Arc<Mutex<PlateOcr>>>,
    rgb: &[u8],
    w: u32,
    h: u32,
    items: &mut [crate::services::detection_bus::Detection],
) {
    for det in items.iter_mut() {
        let fw = w as f32;
        let fh = h as f32;
        let x0 = (det.bbox[0] * fw).round().clamp(0.0, fw) as u32;
        let y0 = (det.bbox[1] * fh).round().clamp(0.0, fh) as u32;
        let cw = (det.bbox[2] * fw).round().max(0.0) as u32;
        let ch = (det.bbox[3] * fh).round().max(0.0) as u32;
        let cw = cw.min(w.saturating_sub(x0));
        let ch = ch.min(h.saturating_sub(y0));
        if cw < 8 || ch < 8 {
            continue;
        }
        if wants_state(&det.klasa) {
            if let Some(classifier) = classifier.as_ref() {
                let crop = crop_rgb(rgb, w, x0, y0, cw, ch);
                match classifier.lock().unwrap().classify(&crop, cw, ch) {
                    Ok(stany) => det.stan = stany,
                    Err(e) => warn!("[vision_analysis] classify failed for {}: {e:#}", det.klasa),
                }
            }
        }
        if det.klasa == "tablica_rejestracyjna" {
            if let Some(ocr) = ocr.as_ref() {
                let crop = crop_rgb(rgb, w, x0, y0, cw, ch);
                match ocr.lock().unwrap().read(&crop, cw, ch) {
                    Ok(Some(plate)) => det.tekst = Some(plate),
                    Ok(None) => {}
                    Err(e) => warn!("[vision_analysis] OCR failed for {}: {e:#}", det.klasa),
                }
            }
        }
    }
}
