// ===== File: services/vision_worker/link.rs — UDS control link between core and vision workers =====
//
// Link v0 (Stage A of docs/VISION_WORKER_SHARDING.md): one loopback
// Unix-domain socket bound by the core; every worker connects, authenticates
// with a token-carrying Hello, then exchanges length-prefixed CBOR frames
// (4-byte big-endian length + serde-CBOR body — the same framing family the
// mesh bi-streams use). The frame enum is `#[non_exhaustive]` and the Hello
// carries a protocol version so Stage B can add AssignCamera / RemoveCamera /
// DetectionsBatch / CameraHealth without a wire break: worker and core are
// the SAME binary, so a version mismatch only means a stale worker survived a
// core binary swap — reject + respawn is the correct outcome.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Wire version carried in Hello. Bump on any incompatible frame change.
pub const LINK_PROTO_VERSION: u32 = 1;

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

/// One link frame. `#[non_exhaustive]` so Stage B variants extend the enum in
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
/// between the accept loop (auth + heartbeat bookkeeping) and the supervisor
/// (health decisions + graceful shutdown).
pub struct LinkState {
    workers: parking_lot::Mutex<HashMap<u32, WorkerEntry>>,
}

impl LinkState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            workers: parking_lot::Mutex::new(HashMap::new()),
        })
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
    fn detach(&self, worker_id: u32, tx: &mpsc::Sender<LinkFrame>) {
        if let Some(entry) = self.workers.lock().get_mut(&worker_id) {
            if entry
                .outbound
                .as_ref()
                .map(|cur| cur.same_channel(tx))
                .unwrap_or(false)
            {
                entry.connected = false;
                entry.outbound = None;
            }
        }
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
    let (tx, mut rx) = mpsc::channel::<LinkFrame>(8);
    state.attach(worker_id, tx.clone());

    // Writer half — owned by its own task so a Shutdown can be sent while the
    // read loop below is blocked in read_frame.
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

    state.detach(worker_id, &tx);
    writer.abort();
    info!(worker_id, "[vision-worker link] worker disconnected");
}
