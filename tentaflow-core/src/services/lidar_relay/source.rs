// =============================================================================
// File: services/lidar_relay/source.rs — observer side (B) relay source
// =============================================================================
//
// A `BinaryStreamSource` that republishes canonical LiDAR frames relayed from the
// owner node over the LiDAR relay bi-stream into THIS node's local StreamHub. The
// dashboard tile subscribes to `lidar:<robot_id>` and is completely unaware the
// robot physically lives on another node.
//
// Mirror of `camera_relay::source` MINUS the init-segment sealing: LiDAR frames
// are self-describing, so there is no fMP4-style codec preamble. Instead the
// source CACHES the latest frame it has relayed and serves it as a dynamic init
// segment, so a late local subscriber renders immediately — exactly the
// latest-wins behaviour of the local `LocalLidarStreamSource`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use parking_lot::Mutex;
use tokio::sync::{broadcast, Notify};

use crate::mesh::iroh_manager::IrohMeshManager;
use crate::services::stream_hub::{BinaryStreamSource, BROADCAST_CAPACITY};

/// Canonical LiDAR frames are self-describing, so the wire is opaque bytes; the
/// browser parses the 36-byte header itself rather than trusting this string.
const LIDAR_MIME: &str = "application/octet-stream";

/// Maximum time `init_segment()` waits for the relay to either cache its first
/// frame or go terminal before giving up. This single deadline covers the WHOLE
/// relay bring-up (bi-stream open + first frame), so a stalled `open_bi` / request
/// write can never ack-then-hang the subscriber. Beyond it we assume the relay
/// never started (owner gate refused, owner robot dormant, peer hung) and the hub
/// surfaces a clean failure. Mirrors the camera relay's `INIT_SEGMENT_TIMEOUT`.
const INIT_SEGMENT_TIMEOUT: Duration = Duration::from_secs(5);

/// Observer-side source republishing a remote robot's LiDAR feed.
///
/// Lifecycle (mirror of `RemoteCameraStreamSource`):
///   1. The StreamHub factory constructs the source and spawns the relay task,
///      which opens the bi-stream to the owner. The task holds only a
///      `Weak<Self>` (plus cloned ids / handle), so it NEVER keeps the source
///      alive on its own.
///   2. Every relayed frame is cached as `latest_frame` (the dynamic init seed)
///      and fanned out via `broadcast` (latest-wins).
///   3. When the last subscriber drops, the hub drops its `Arc<Self>`; `Drop`
///      aborts the relay task, which closes the bi-stream. The owner detects the
///      close and drops ITS StreamHub handle. Even without the abort, the task's
///      next `Weak::upgrade` fails and it exits — no self-retaining cycle.
///   4. On terminal failure (relay open refused, owner closed before a frame, no
///      first frame within the timeout) the task marks the source terminal and
///      drops the broadcast sender so live receivers observe `Closed` and the
///      next subscribe gets a clean failure instead of a hung empty stream.
pub struct RemoteLidarStreamSource {
    stream_id: String,
    /// The latest relayed frame, served as the dynamic init segment so a late
    /// local subscriber renders immediately. `None` until the first frame lands.
    latest_frame: Mutex<Option<Bytes>>,
    /// Woken when the relay bring-up RESOLVES: either the first frame is cached
    /// (`latest_frame` set) or the source goes terminal. A cold `init_segment()`
    /// awaits this under `INIT_SEGMENT_TIMEOUT`, so a stalled open/first-frame
    /// never acks-then-hangs the subscriber.
    first_ready: Notify,
    /// `None` once the source has terminally failed — `chunk_broadcaster` reports
    /// that to the hub so the subscribe collapses to a clean failure.
    chunks_tx: Mutex<Option<broadcast::Sender<Bytes>>>,
    /// Set true on any terminal relay outcome; gates duplicate teardown.
    terminal: AtomicBool,
    relay_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl std::fmt::Debug for RemoteLidarStreamSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteLidarStreamSource")
            .field("stream_id", &self.stream_id)
            .field(
                "latest_frame_len",
                &self.latest_frame.lock().as_ref().map(|b| b.len()),
            )
            .field("terminal", &self.terminal.load(Ordering::Acquire))
            .finish()
    }
}

impl RemoteLidarStreamSource {
    /// Construct the source and start the relay task targeting `owner_node`.
    /// `robot_id` is the globally-unique robot/addon id (no `lidar:` prefix); the
    /// StreamHub stream id is `lidar:<robot_id>`.
    pub fn new(
        iroh: Arc<IrohMeshManager>,
        owner_node: String,
        robot_id: String,
        org_id: String,
    ) -> Arc<Self> {
        let (chunks_tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);
        let source = Arc::new(Self {
            stream_id: format!("lidar:{}", robot_id),
            latest_frame: Mutex::new(None),
            first_ready: Notify::new(),
            chunks_tx: Mutex::new(Some(chunks_tx)),
            terminal: AtomicBool::new(false),
            relay_task: Mutex::new(None),
        });
        // The task holds a Weak — not an Arc — so the source's refcount is owned
        // solely by the hub. When the hub drops it, the task can no longer upgrade
        // and exits (and Drop aborts it immediately regardless).
        let weak = Arc::downgrade(&source);
        let task = tokio::spawn(spawn_relay(weak, iroh, owner_node, robot_id, org_id));
        *source.relay_task.lock() = Some(task);
        source
    }

    /// Cache the latest frame (dynamic init seed) and fan it out. A send error
    /// only means zero live subscribers; the task keeps relaying so a late
    /// subscriber still gets live frames. The first cached frame wakes any cold
    /// `init_segment()` waiter so it returns the seed immediately.
    fn deliver(&self, frame: Bytes) {
        let was_first = {
            let mut guard = self.latest_frame.lock();
            let first = guard.is_none();
            *guard = Some(frame.clone());
            first
        };
        if let Some(tx) = self.chunks_tx.lock().as_ref() {
            let _ = tx.send(frame);
        }
        if was_first {
            self.first_ready.notify_waiters();
        }
    }

    /// Mark the source terminally failed: drop the broadcast sender so live
    /// receivers observe `Closed`, and wake init waiters so a pending
    /// `init_segment()` returns `None` immediately instead of waiting out the
    /// timeout. Idempotent.
    fn mark_terminal(&self) {
        self.terminal.store(true, Ordering::Release);
        // Dropping the only Sender closes every outstanding Receiver.
        *self.chunks_tx.lock() = None;
        self.first_ready.notify_waiters();
    }
}

/// Relay task body. Holds only a `Weak<RemoteLidarStreamSource>` plus the cloned
/// ids and iroh handle — never an `Arc<Self>`, so the task does not keep the
/// source alive. Opens the bi-stream, caches+broadcasts every relayed frame. On
/// any open failure, stream end, or frame error it marks the source terminal
/// (drops the broadcast sender + wakes init waiters) so a cold `init_segment()`
/// resolves IMMEDIATELY (not just via its outer timeout) and subscribers observe
/// `Closed`. The first-frame DEADLINE lives in `init_segment()` (a single timeout
/// covering open + first frame); this task itself has no per-frame deadline, since
/// LiDAR is steady-state once the first frame lands.
async fn spawn_relay(
    source: Weak<RemoteLidarStreamSource>,
    iroh: Arc<IrohMeshManager>,
    owner_node: String,
    robot_id: String,
    org_id: String,
) {
    let mut stream = match iroh
        .lidar_stream_request(&owner_node, &robot_id, &org_id)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(
                target: "lidar::relay",
                owner = %owner_node,
                robot_id = %robot_id,
                "observer: relay open failed: {e}"
            );
            if let Some(src) = source.upgrade() {
                src.mark_terminal();
            }
            return;
        }
    };

    // A hung peer that opened the bi-stream yet never serves a frame does NOT park
    // here forever: the first-frame deadline is enforced by `init_segment()` (which
    // awaits `first_ready` under `INIT_SEGMENT_TIMEOUT` and marks the source
    // terminal on expiry). When that happens, `mark_terminal` drops the broadcast
    // sender; this loop then exits on the next `Weak::upgrade` (hub released the
    // source) or its own `Drop`-driven abort.
    while let Some(item) = stream.next().await {
        let bytes = match item {
            Ok(b) => b,
            Err(e) => {
                tracing::debug!(target: "lidar::relay", robot_id = %robot_id, "observer: relay frame error: {e}");
                break;
            }
        };
        let frame: tentaflow_protocol::mesh::LidarStreamFrame =
            match tentaflow_protocol::cbor::decode(&bytes) {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!(target: "lidar::relay", "observer: bad relay frame: {e}");
                    break;
                }
            };
        // Upgrade per frame: if the hub has dropped the source (last subscriber
        // gone) the task exits here rather than relaying into a dead source.
        let Some(src) = source.upgrade() else {
            return;
        };
        src.deliver(Bytes::from(frame.data));
    }

    // Relay ended (owner closed / error). Mark terminal so any init waiter returns
    // None immediately, live receivers observe Closed, and the next subscribe
    // fails cleanly.
    if let Some(src) = source.upgrade() {
        src.mark_terminal();
    }
}

#[async_trait]
impl BinaryStreamSource for RemoteLidarStreamSource {
    fn id(&self) -> &str {
        &self.stream_id
    }

    fn mime_type(&self) -> &str {
        LIDAR_MIME
    }

    async fn init_segment(&self) -> Option<Bytes> {
        // The latest relayed frame IS the seed: there is no codec preamble, so a
        // subscriber's "init" is the current frame, delivered before the live
        // broadcast so it renders without waiting for the next relay tick.
        //
        // A frame is already cached → return it (a LATER subscriber's seed; the
        // relay is already live). Otherwise this is a COLD subscribe: we must NOT
        // ack-then-hand-out an empty broadcast before the relay has produced or
        // failed, or a stalled bi-stream open would leave the browser holding the
        // stream slot forever. So we AWAIT relay readiness under a SINGLE timeout
        // covering the whole bring-up (open + first frame) — mirroring how the
        // camera relay gates cold init.
        if let Some(b) = self.latest_frame.lock().clone() {
            return Some(b);
        }
        // Subscribe to the readiness signal BEFORE re-checking so a notify firing
        // between the lock release and `notified()` is not missed.
        let notified = self.first_ready.notified();
        if let Some(b) = self.latest_frame.lock().clone() {
            return Some(b);
        }
        // A relay that already failed before we started waiting: bail now.
        if self.terminal.load(Ordering::Acquire) {
            return None;
        }
        match tokio::time::timeout(INIT_SEGMENT_TIMEOUT, notified).await {
            // Readiness resolved: either the first frame is cached (return it) or
            // the relay went terminal (latest_frame still None → return None and
            // the subscribe fails cleanly).
            Ok(()) => self.latest_frame.lock().clone(),
            Err(_) => {
                // Bring-up never resolved in time (hung open / silent owner): make
                // the source terminal so `chunk_broadcaster()` returns None and the
                // cold subscribe fails cleanly instead of registering a hung empty
                // stream that never delivers a frame.
                self.mark_terminal();
                None
            }
        }
    }

    fn chunk_broadcaster(&self) -> Option<broadcast::Sender<Bytes>> {
        self.chunks_tx.lock().clone()
    }

    fn dynamic_init(&self) -> bool {
        // Latest-wins self-describing stream: every subscriber's init is the
        // CURRENT frame, not a fixed codec preamble, so a late joiner must get a
        // fresh `init_segment()` rather than a stale cached one.
        true
    }
}

impl Drop for RemoteLidarStreamSource {
    fn drop(&mut self) {
        // Last subscriber gone → abort the relay task, which drops the bi-stream
        // recv half and closes the QUIC stream. The owner observes the close and
        // tears down its own StreamHub subscription. The task held only a
        // Weak<Self>, so this Drop is reached as soon as the hub releases its
        // Arc — no cycle.
        if let Some(task) = self.relay_task.lock().take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::broadcast::error::RecvError;

    /// Drive the source's broadcast/terminal logic directly (no two-node mesh):
    ///   (a) `deliver` caches the latest frame (dynamic init seed) and fans it
    ///       out to a live receiver,
    ///   (b) a second `deliver` overwrites the cached init and is broadcast,
    ///   (c) `mark_terminal` drops the sender so the receiver observes `Closed`
    ///       and `chunk_broadcaster()` reports the source is terminal.
    #[tokio::test]
    async fn deliver_caches_init_then_terminal_closes() {
        // Build the source struct WITHOUT a relay task — we drive deliver/terminal
        // directly. `new()` would spawn a relay to a non-existent peer; here we
        // exercise only the broadcast/terminal state machine.
        let (chunks_tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);
        let source = RemoteLidarStreamSource {
            stream_id: "lidar:relay-test".to_string(),
            latest_frame: Mutex::new(None),
            first_ready: Notify::new(),
            chunks_tx: Mutex::new(Some(chunks_tx)),
            terminal: AtomicBool::new(false),
            relay_task: Mutex::new(None),
        };

        let mut rx = source
            .chunk_broadcaster()
            .expect("live broadcaster")
            .subscribe();

        // (a) First frame: cached as init + broadcast.
        let f1 = Bytes::from_static(b"frame-one");
        source.deliver(f1.clone());
        assert_eq!(source.init_segment().await.as_deref(), Some(&f1[..]));
        assert_eq!(rx.recv().await.expect("f1"), f1);

        // (b) Second frame overwrites the cached init and is broadcast.
        let f2 = Bytes::from_static(b"frame-two-longer");
        source.deliver(f2.clone());
        assert_eq!(source.init_segment().await.as_deref(), Some(&f2[..]));
        assert_eq!(rx.recv().await.expect("f2"), f2);

        // (c) Terminal: sender dropped → receiver observes Closed (after draining),
        // and the broadcaster reports the source is dead.
        source.mark_terminal();
        let closed = loop {
            match rx.recv().await {
                Ok(_) => continue,
                Err(RecvError::Closed) => break true,
                Err(RecvError::Lagged(_)) => continue,
            }
        };
        assert!(closed);
        assert!(source.chunk_broadcaster().is_none(), "terminal source");
        assert!(source.terminal.load(Ordering::Acquire));
    }

    /// A cold subscribe must NOT ack-then-hang: if the relay marks the source
    /// terminal before any frame is cached, `init_segment()` returns `None`
    /// PROMPTLY (woken by `mark_terminal`), not after the full timeout and not
    /// blocking forever. This is the no-first-frame path (hung/refusing owner).
    #[tokio::test]
    async fn cold_init_returns_none_promptly_when_terminal_before_first_frame() {
        let (chunks_tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);
        let source = Arc::new(RemoteLidarStreamSource {
            stream_id: "lidar:relay-cold".to_string(),
            latest_frame: Mutex::new(None),
            first_ready: Notify::new(),
            chunks_tx: Mutex::new(Some(chunks_tx)),
            terminal: AtomicBool::new(false),
            relay_task: Mutex::new(None),
        });

        // Simulate the relay task failing bring-up (open refused / no first frame)
        // shortly after a cold subscriber started awaiting init.
        let waker = Arc::clone(&source);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            waker.mark_terminal();
        });

        // Must resolve to None well before INIT_SEGMENT_TIMEOUT (woken by the
        // terminal notify), and must not hang.
        let init = tokio::time::timeout(Duration::from_secs(1), source.init_segment())
            .await
            .expect("init_segment must not hang past the terminal wake");
        assert!(init.is_none(), "terminal-before-frame → no init seed");
        assert!(source.terminal.load(Ordering::Acquire));
    }

    /// Marking terminal BEFORE any cold subscribe makes `init_segment()` return
    /// `None` immediately (the early `terminal` check), never blocking on the
    /// readiness notify.
    #[tokio::test]
    async fn init_returns_none_immediately_if_already_terminal() {
        let (chunks_tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);
        let source = RemoteLidarStreamSource {
            stream_id: "lidar:relay-dead".to_string(),
            latest_frame: Mutex::new(None),
            first_ready: Notify::new(),
            chunks_tx: Mutex::new(Some(chunks_tx)),
            terminal: AtomicBool::new(false),
            relay_task: Mutex::new(None),
        };
        source.mark_terminal();

        let init = tokio::time::timeout(Duration::from_millis(200), source.init_segment())
            .await
            .expect("already-terminal init must return immediately");
        assert!(init.is_none());
    }
}
