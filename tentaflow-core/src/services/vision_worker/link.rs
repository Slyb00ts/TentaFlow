// ===== File: services/vision_worker/link.rs — UDS control link between core and vision workers =====
//
// One loopback Unix-domain socket bound by the core; every worker connects,
// authenticates with a token-carrying Hello, then exchanges length-prefixed
// CBOR frames (4-byte big-endian length + serde-CBOR body — the same framing
// family the mesh bi-streams use). The frame enum is `#[non_exhaustive]` and
// the Hello carries a protocol version: worker and core are the SAME binary,
// so a version mismatch only means a stale worker survived a core binary
// swap — reject + respawn is the correct outcome.
//
// Stage B (docs/VISION_WORKER_SHARDING.md) rides camera assignment
// (AssignCamera/RemoveCamera), coalesced detection batches, camera health and
// the per-tile video relay (StreamStart/StreamStop core→worker,
// StreamFrame/StreamEnd worker→core) over this link. Worker-originated data
// frames are routed to the core-side [`fleet::WorkerFleet`].

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::services::camera_ingest::session::CameraHealth;
use crate::services::detection_bus::Detection;

use super::fleet::WorkerFleet;

/// Wire version carried in Hello. Bump on any incompatible frame change.
/// v2: Stage B camera assignment + detections + health + stream relay frames.
pub const LINK_PROTO_VERSION: u32 = 2;

/// Upper bound for one frame. Stage-A frames are tiny; the cap guards the
/// length-prefix allocation against a corrupt peer and is sized ahead for
/// Stage B detection batches.
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

/// Cadence at which a worker sends `Heartbeat`.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);

/// How long the core waits for the authenticating Hello on a fresh connection.
const HELLO_READ_TIMEOUT: Duration = Duration::from_secs(10);

// =============================================================================
// Frames
// =============================================================================

/// One link frame. `#[non_exhaustive]` so future variants extend the enum in
/// place; unknown variants fail the CBOR decode of that frame, which drops the
/// connection and lets the supervisor respawn a version-matched worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum LinkFrame {
    /// Worker → core, first frame after connect. Authenticated against the
    /// token the supervisor generated for this worker's current incarnation.
    Hello {
        worker_id: u32,
        token: String,
        proto_version: u32,
    },
    /// Core → worker, auth verdict. Rejected connections are closed right
    /// after this frame.
    HelloAck { accepted: bool },
    /// Worker → core, every [`HEARTBEAT_INTERVAL`].
    Heartbeat { stats: WorkerStats },
    /// Core → worker: drain vision analysis and exit.
    Shutdown,
    /// Core → worker: own this camera — start ingest + always-on analysis.
    /// Idempotent: a worker that already runs the camera restarts its session
    /// with the fresh config (this is also how credential rotation reaches a
    /// live worker session).
    AssignCamera { camera: CameraAssignment },
    /// Core → worker: stop ingest + analysis for this camera.
    RemoveCamera { camera_id: String },
    /// Worker → core: one flush tick of coalesced detection frames
    /// (latest-wins per camera). The core republishes each frame verbatim
    /// into its own `detection_bus`, so the dashboard overlay consumer is
    /// unchanged.
    DetectionsBatch { frames: Vec<DetectionsWire> },
    /// Worker → core: periodic health of every camera session the worker
    /// runs. The core merges it into the same read paths the local ingest
    /// registry feeds (the worker never writes the DB).
    CameraHealthReport { cameras: Vec<CameraHealth> },
    /// Core → worker: a dashboard tile subscribed to this camera — pump the
    /// worker-local StreamHub topic over the link. `stream_id` is minted by
    /// the core and scopes every relayed frame.
    StreamStart {
        stream_id: u64,
        camera_id: String,
        preview: bool,
    },
    /// Core → worker: last dashboard subscriber left — stop the pump.
    StreamStop { stream_id: u64 },
    /// Worker → core: one fMP4 frame of an active stream pump. `is_init`
    /// carries the ftyp+moov preamble exactly once per pump, together with
    /// the media-timeline base PTS the overlay anchors on.
    StreamFrame {
        stream_id: u64,
        is_init: bool,
        base_pts_ns: Option<u64>,
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },
    /// Worker → core: the pump ended (source gone, lag cut, or subscribe
    /// failure). The core marks its relay source terminal so tiles
    /// resubscribe cleanly.
    StreamEnd { stream_id: u64 },
}

/// Camera row fields a worker needs to run ingest + analysis — mirrors the
/// core's own hydrate path (`CameraConfig` minus the process-local
/// `decoder_override`). Credentials stay ENCRYPTED on the wire; the worker's
/// RTSP session decrypts them on demand through the shared
/// `<home>/keys/cameras.key` exactly like the core does. Per-camera analysis
/// config (`analysis_fps`, CV pipeline) is not carried: the worker reads it
/// from its read-only DB like the in-process engine does.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraAssignment {
    pub camera_id: String,
    /// Session vendor — ONVIF rows are already translated to `rtsp` (the
    /// persisted URL is the SOAP-derived RTSP URI), matching the session
    /// contract enforced by `spawn_session`.
    pub vendor: String,
    pub url: String,
    pub target_fps: u32,
    pub resolution_width: Option<u32>,
    pub resolution_height: Option<u32>,
    pub owner_addon_id: Option<String>,
    #[serde(with = "serde_bytes")]
    pub credentials_encrypted: Option<Vec<u8>>,
}

impl CameraAssignment {
    /// Builds an assignment from a persisted camera row, mirroring the
    /// hydrate path's `CameraConfig` construction (plus the ONVIF→RTSP
    /// session-vendor translation the runtime add path applies).
    #[cfg(feature = "camera")]
    pub fn from_row(row: &crate::db::repository::CameraRow) -> Self {
        let vendor = if row.vendor == "onvif" {
            "rtsp"
        } else {
            row.vendor.as_str()
        };
        let (resolution_width, resolution_height) =
            match (row.resolution_width, row.resolution_height) {
                (Some(w), Some(h)) if w > 0 && h > 0 => (Some(w as u32), Some(h as u32)),
                _ => (None, None),
            };
        Self {
            camera_id: row.camera_id.clone(),
            vendor: vendor.to_string(),
            url: row.url.clone(),
            target_fps: row.target_fps.max(1) as u32,
            resolution_width,
            resolution_height,
            owner_addon_id: Some(row.owner_addon_id.clone()),
            credentials_encrypted: row.credentials_encrypted.clone(),
        }
    }

    /// Builds an assignment from an in-memory session config (the runtime
    /// camera-add / credentials-rotate paths, which already hold the exact
    /// `CameraConfig` the session would have been started with locally).
    pub fn from_config(cfg: &crate::services::camera_ingest::session::CameraConfig) -> Self {
        Self {
            camera_id: cfg.camera_id.clone(),
            vendor: cfg.vendor.clone(),
            url: cfg.url.clone(),
            target_fps: cfg.target_fps,
            resolution_width: cfg.resolution.map(|(w, _)| w),
            resolution_height: cfg.resolution.map(|(_, h)| h),
            owner_addon_id: cfg.owner_addon_id.clone(),
            credentials_encrypted: cfg.credentials_encrypted.clone(),
        }
    }

    /// The worker-side session config. `decoder_override` stays `None` — the
    /// worker auto-detects its own hardware decoder like the core does.
    pub fn into_config(self) -> crate::services::camera_ingest::session::CameraConfig {
        crate::services::camera_ingest::session::CameraConfig {
            camera_id: self.camera_id,
            vendor: self.vendor,
            url: self.url,
            target_fps: self.target_fps,
            resolution: match (self.resolution_width, self.resolution_height) {
                (Some(w), Some(h)) => Some((w, h)),
                _ => None,
            },
            owner_addon_id: self.owner_addon_id,
            credentials_encrypted: self.credentials_encrypted,
            decoder_override: None,
        }
    }
}

/// One overlay frame on the wire — `detection_bus::DetectionsMessage` minus
/// the constant `type` discriminator (re-added by `publish_detections` on the
/// core side).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectionsWire {
    pub camera_id: String,
    pub ts_ms: u64,
    pub pts_ns: Option<u64>,
    pub proc_ms: u32,
    pub items: Vec<Detection>,
}

/// Basic worker health stats carried on every heartbeat.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkerStats {
    /// CUDA device this worker is pinned to.
    pub gpu: i32,
    /// Detector session-pool size; 0 until the lazy detector load (which can
    /// take minutes on a first-ever TRT engine build) completes.
    pub detector_sessions: u32,
    /// Seconds since the worker process booted.
    pub uptime_secs: u64,
}

// =============================================================================
// Framing helpers (shared by core and worker sides)
// =============================================================================

/// Encode + write one length-prefixed CBOR frame.
pub async fn write_frame<W>(w: &mut W, frame: &LinkFrame) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let bytes =
        tentaflow_protocol::cbor::encode(frame).map_err(|e| anyhow!("link frame encode: {e}"))?;
    if bytes.len() > MAX_FRAME_LEN {
        bail!("link frame too large: {} bytes", bytes.len());
    }
    w.write_all(&(bytes.len() as u32).to_be_bytes()).await?;
    w.write_all(&bytes).await?;
    w.flush().await?;
    Ok(())
}

/// Read + decode one length-prefixed CBOR frame.
pub async fn read_frame<R>(r: &mut R) -> Result<LinkFrame>
where
    R: AsyncRead + Unpin,
{
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)
        .await
        .context("link read length")?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 || len > MAX_FRAME_LEN {
        bail!("link frame length out of bounds: {len}");
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await.context("link read body")?;
    tentaflow_protocol::cbor::decode(&buf).map_err(|e| anyhow!("link frame decode: {e}"))
}

// =============================================================================
// Core-side per-worker link state
// =============================================================================

/// Read-only view of one worker's link health, consumed by the supervisor.
#[derive(Debug, Clone)]
pub struct WorkerLinkStatus {
    pub connected: bool,
    pub last_heartbeat: Option<Instant>,
    pub stats: WorkerStats,
}

struct WorkerEntry {
    /// Token generated by the supervisor for the CURRENT incarnation. Replaced
    /// on every (re)spawn, which invalidates any stale connection from a
    /// previous, already-killed process.
    expected_token: String,
    connected: bool,
    last_heartbeat: Option<Instant>,
    stats: WorkerStats,
    /// Outbound frame channel of the live connection (`Shutdown` rides here).
    outbound: Option<mpsc::Sender<LinkFrame>>,
}

/// Core-side registry of expected workers and their live link state. Shared
/// between the accept loop (auth + heartbeat bookkeeping), the supervisor
/// (health decisions + graceful shutdown) and the worker fleet (camera
/// assignment replay + worker-originated data frames).
pub struct LinkState {
    workers: parking_lot::Mutex<HashMap<u32, WorkerEntry>>,
    /// Stage-B frame router. `Weak` — the fleet owns an `Arc<LinkState>`, so
    /// a strong pointer here would be a reference cycle.
    fleet: parking_lot::RwLock<Option<Weak<WorkerFleet>>>,
}

impl LinkState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            workers: parking_lot::Mutex::new(HashMap::new()),
            fleet: parking_lot::RwLock::new(None),
        })
    }

    /// Wires the worker fleet in as the consumer of worker-originated data
    /// frames and the source of assignment replay. Called once by the
    /// supervisor right after the fleet is constructed.
    pub fn set_fleet(&self, fleet: Weak<WorkerFleet>) {
        *self.fleet.write() = Some(fleet);
    }

    fn fleet(&self) -> Option<Arc<WorkerFleet>> {
        self.fleet.read().as_ref().and_then(Weak::upgrade)
    }

    /// Outbound frame channel of `worker_id`'s live connection, if any.
    pub(crate) fn sender(&self, worker_id: u32) -> Option<mpsc::Sender<LinkFrame>> {
        self.workers
            .lock()
            .get(&worker_id)
            .and_then(|e| e.outbound.clone())
    }

    /// Registers (or re-arms) the expected token for `worker_id`. Called by
    /// the supervisor right before every (re)spawn; resets link state so a
    /// stale heartbeat from the previous incarnation cannot mask a dead spawn.
    pub fn register_worker(&self, worker_id: u32, token: String) {
        self.workers.lock().insert(
            worker_id,
            WorkerEntry {
                expected_token: token,
                connected: false,
                last_heartbeat: None,
                stats: WorkerStats::default(),
                outbound: None,
            },
        );
    }

    /// Constant-time token check (mirrors the pickup-token discipline —
    /// `subtle::ConstantTimeEq`, length compared first).
    fn authenticate(&self, worker_id: u32, token: &str) -> bool {
        let guard = self.workers.lock();
        match guard.get(&worker_id) {
            Some(entry) => {
                let provided = token.as_bytes();
                let expected = entry.expected_token.as_bytes();
                provided.len() == expected.len() && bool::from(provided.ct_eq(expected))
            }
            None => false,
        }
    }

    fn attach(&self, worker_id: u32, tx: mpsc::Sender<LinkFrame>) {
        if let Some(entry) = self.workers.lock().get_mut(&worker_id) {
            entry.connected = true;
            entry.last_heartbeat = Some(Instant::now());
            entry.outbound = Some(tx);
        }
    }

    /// Clears connection state, but only if `tx` still is the registered
    /// channel — a respawned worker may already have attached a new one.
    /// Returns whether THIS connection was the live one (gates the fleet's
    /// disconnect teardown against stale-connection races).
    fn detach(&self, worker_id: u32, tx: &mpsc::Sender<LinkFrame>) -> bool {
        if let Some(entry) = self.workers.lock().get_mut(&worker_id) {
            if entry
                .outbound
                .as_ref()
                .map(|cur| cur.same_channel(tx))
                .unwrap_or(false)
            {
                entry.connected = false;
                entry.outbound = None;
                return true;
            }
        }
        false
    }

    fn record_heartbeat(&self, worker_id: u32, stats: WorkerStats) {
        if let Some(entry) = self.workers.lock().get_mut(&worker_id) {
            entry.last_heartbeat = Some(Instant::now());
            entry.stats = stats;
        }
    }

    /// Current link health for `worker_id` (None = never registered).
    pub fn status(&self, worker_id: u32) -> Option<WorkerLinkStatus> {
        self.workers
            .lock()
            .get(&worker_id)
            .map(|e| WorkerLinkStatus {
                connected: e.connected,
                last_heartbeat: e.last_heartbeat,
                stats: e.stats.clone(),
            })
    }

    /// Sends `Shutdown` to the worker's live connection. Returns `false` when
    /// the worker is not connected (caller falls back to a group kill).
    pub async fn send_shutdown(&self, worker_id: u32) -> bool {
        let tx = self
            .workers
            .lock()
            .get(&worker_id)
            .and_then(|e| e.outbound.clone());
        match tx {
            Some(tx) => tx.send(LinkFrame::Shutdown).await.is_ok(),
            None => false,
        }
    }
}

// =============================================================================
// Core-side listener
// =============================================================================

/// Binds the link socket and spawns the accept loop. The socket file is
/// re-created on every boot and restricted to the owning user (the token rides
/// in cleartext over this loopback-only transport).
pub fn serve(path: &Path, state: Arc<LinkState>) -> Result<tokio::task::JoinHandle<()>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create link socket dir {}", parent.display()))?;
    }
    if path.exists() {
        std::fs::remove_file(path)
            .with_context(|| format!("remove stale link socket {}", path.display()))?;
    }
    let listener =
        UnixListener::bind(path).with_context(|| format!("bind link socket {}", path.display()))?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod link socket {}", path.display()))?;
    }
    info!("[vision-worker link] listening on {}", path.display());

    let handle = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    let state = state.clone();
                    tokio::spawn(async move {
                        handle_connection(stream, state).await;
                    });
                }
                Err(e) => {
                    warn!("[vision-worker link] accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
    });
    Ok(handle)
}

/// One accepted connection: authenticate the Hello, then pump heartbeats in
/// and supervisor frames (Shutdown) out until either side closes.
async fn handle_connection(stream: UnixStream, state: Arc<LinkState>) {
    let (mut rd, mut wr) = stream.into_split();

    let worker_id = match tokio::time::timeout(HELLO_READ_TIMEOUT, read_frame(&mut rd)).await {
        Ok(Ok(LinkFrame::Hello {
            worker_id,
            token,
            proto_version,
        })) => {
            if proto_version != LINK_PROTO_VERSION || !state.authenticate(worker_id, &token) {
                warn!(
                    worker_id,
                    proto_version, "[vision-worker link] Hello rejected (bad token or version)"
                );
                let _ = write_frame(&mut wr, &LinkFrame::HelloAck { accepted: false }).await;
                return;
            }
            if write_frame(&mut wr, &LinkFrame::HelloAck { accepted: true })
                .await
                .is_err()
            {
                return;
            }
            worker_id
        }
        Ok(Ok(_)) => {
            warn!("[vision-worker link] first frame was not Hello; dropping connection");
            return;
        }
        Ok(Err(e)) => {
            debug!("[vision-worker link] Hello read failed: {e:#}");
            return;
        }
        Err(_) => {
            warn!("[vision-worker link] Hello timeout; dropping connection");
            return;
        }
    };

    info!(worker_id, "[vision-worker link] worker connected");
    // Capacity sized for the Stage-B control plane: an assignment replay can
    // burst one AssignCamera per owned camera while stream control frames
    // interleave; senders await, the writer task drains continuously.
    let (tx, mut rx) = mpsc::channel::<LinkFrame>(64);
    state.attach(worker_id, tx.clone());
    if let Some(fleet) = state.fleet() {
        // Replay this worker's camera assignments (workers are stateless — a
        // respawn comes up empty and re-learns its subset here).
        fleet.on_worker_connected(worker_id);
    }

    // Writer half — owned by its own task so supervisor/fleet frames can be
    // sent while the read loop below is blocked in read_frame.
    let writer = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if write_frame(&mut wr, &frame).await.is_err() {
                break;
            }
        }
    });

    loop {
        match read_frame(&mut rd).await {
            Ok(LinkFrame::Heartbeat { stats }) => state.record_heartbeat(worker_id, stats),
            Ok(
                frame @ (LinkFrame::DetectionsBatch { .. }
                | LinkFrame::CameraHealthReport { .. }
                | LinkFrame::StreamFrame { .. }
                | LinkFrame::StreamEnd { .. }),
            ) => match state.fleet() {
                Some(fleet) => fleet.handle_worker_frame(worker_id, frame),
                None => debug!(
                    worker_id,
                    "[vision-worker link] data frame dropped — no fleet router installed"
                ),
            },
            Ok(other) => {
                debug!(
                    worker_id,
                    ?other,
                    "[vision-worker link] ignoring unexpected frame"
                )
            }
            Err(e) => {
                debug!(worker_id, "[vision-worker link] read ended: {e:#}");
                break;
            }
        }
    }

    if state.detach(worker_id, &tx) {
        if let Some(fleet) = state.fleet() {
            fleet.on_worker_disconnected(worker_id);
        }
    }
    writer.abort();
    info!(worker_id, "[vision-worker link] worker disconnected");
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn roundtrip(frame: &LinkFrame) -> LinkFrame {
        let mut buf = Vec::new();
        write_frame(&mut buf, frame).await.expect("encode");
        let mut cursor = std::io::Cursor::new(buf);
        read_frame(&mut cursor).await.expect("decode")
    }

    #[tokio::test]
    async fn assign_camera_roundtrips_with_encrypted_credentials() {
        let frame = LinkFrame::AssignCamera {
            camera: CameraAssignment {
                camera_id: "cam_550e8400-e29b-41d4-a716-446655440000".into(),
                vendor: "rtsp".into(),
                url: "rtsp://10.0.0.5:554/stream1".into(),
                target_fps: 25,
                resolution_width: Some(1920),
                resolution_height: Some(1080),
                owner_addon_id: Some("tentavision".into()),
                credentials_encrypted: Some(vec![0u8, 1, 2, 250, 251, 252]),
            },
        };
        match roundtrip(&frame).await {
            LinkFrame::AssignCamera { camera } => {
                assert_eq!(camera.camera_id, "cam_550e8400-e29b-41d4-a716-446655440000");
                assert_eq!(camera.vendor, "rtsp");
                assert_eq!(camera.target_fps, 25);
                assert_eq!(camera.resolution_width, Some(1920));
                assert_eq!(
                    camera.credentials_encrypted.as_deref(),
                    Some(&[0u8, 1, 2, 250, 251, 252][..])
                );
                let cfg = camera.into_config();
                assert_eq!(cfg.resolution, Some((1920, 1080)));
                assert!(cfg.decoder_override.is_none());
            }
            other => panic!("expected AssignCamera, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn detections_batch_roundtrips() {
        let frame = LinkFrame::DetectionsBatch {
            frames: vec![DetectionsWire {
                camera_id: "cam_x".into(),
                ts_ms: 1_700_000_000_123,
                pts_ns: Some(42_000_000),
                proc_ms: 17,
                items: vec![Detection {
                    klasa: "tablica_adr".into(),
                    bbox: [0.1, 0.2, 0.3, 0.4],
                    score: 0.9,
                    stan: vec!["uszkodzona".into()],
                    tekst: Some("30/1202".into()),
                    tekst_conf: None,
                    tekst_thumb_ref: None,
                    track_id: 7,
                    vehicle_id: 0,
                    vx: 0.5,
                    vy: -0.5,
                }],
            }],
        };
        match roundtrip(&frame).await {
            LinkFrame::DetectionsBatch { frames } => {
                assert_eq!(frames.len(), 1);
                assert_eq!(frames[0].pts_ns, Some(42_000_000));
                assert_eq!(frames[0].items[0].klasa, "tablica_adr");
                assert_eq!(frames[0].items[0].track_id, 7);
            }
            other => panic!("expected DetectionsBatch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn stream_frame_roundtrips_binary_payload() {
        let data: Vec<u8> = (0..255u8).collect();
        let frame = LinkFrame::StreamFrame {
            stream_id: 9,
            is_init: true,
            base_pts_ns: Some(123_456_789),
            data: data.clone(),
        };
        match roundtrip(&frame).await {
            LinkFrame::StreamFrame {
                stream_id,
                is_init,
                base_pts_ns,
                data: got,
            } => {
                assert_eq!(stream_id, 9);
                assert!(is_init);
                assert_eq!(base_pts_ns, Some(123_456_789));
                assert_eq!(got, data);
            }
            other => panic!("expected StreamFrame, got {other:?}"),
        }
    }
}
