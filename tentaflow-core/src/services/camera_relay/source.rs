// =============================================================================
// File: services/camera_relay/source.rs — observer side (B) relay source
// =============================================================================
//
// A `BinaryStreamSource` that republishes fMP4 frames relayed from the owner
// node over the camera relay bi-stream into THIS node's local StreamHub. The
// dashboard tile subscribes to `camera:<id>` and is completely unaware the
// camera physically lives on another node.
//
// Init-segment delivery copies `Mp4StreamPublisher`'s mechanic: a sealed
// `Mutex<Option<Bytes>>` + `Notify` gate so `init_segment().await` blocks until
// the first relayed `is_init` frame arrives (or times out for a dormant relay),
// then media frames fan out via a `broadcast` channel.

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

/// Same generic fMP4/H.264 MIME the local publisher advertises — the relayed
/// init segment carries the real `avcC` box, so the browser validates the codec
/// from the bytes, not this string. Shared with the vision-worker relay source,
/// which republishes the same fMP4 stream shape.
pub(crate) const FMP4_H264_MIME: &str = "video/mp4; codecs=\"avc1.42E01E\"";

/// Maximum time `init_segment()` waits for the first relayed `is_init` frame
/// before giving up. Beyond this we assume the relay never started (owner gate
/// refused, owner camera dormant) so the hub surfaces a clean failure instead
/// of hanging the WS consumer.
const INIT_SEGMENT_TIMEOUT: Duration = Duration::from_secs(5);

/// Observer-side source republishing a remote camera's fMP4 feed.
///
/// Lifecycle:
///   1. The StreamHub factory constructs the source and spawns the relay task,
///      which opens the bi-stream to the owner. The task holds only a
///      `Weak<Self>` (plus cloned ids / handle), so it NEVER keeps the source
///      alive on its own.
///   2. The first `is_init` frame seals the init segment and unblocks
///      `init_segment()`. Subsequent frames fan out via `broadcast`.
///   3. When the last subscriber drops, the hub drops its `Arc<Self>`; `Drop`
///      aborts the relay task, which closes the bi-stream. The owner detects the
///      close and drops ITS StreamHub handle (detaching the mux branch if it
///      was the last subscriber there too). Even without the abort, the task's
///      next `Weak::upgrade` fails and it exits — there is no self-retaining
///      cycle keeping the source resident forever.
///   4. On terminal failure (relay open refused, owner closed before init, init
///      timed out) the task marks the source terminal and drops the broadcast
///      sender so live receivers observe `Closed` and the next subscribe gets a
///      clean failure instead of a hung empty stream.
pub struct RemoteCameraStreamSource {
    stream_id: String,
    init_segment: Mutex<Option<Bytes>>,
    init_ready: Notify,
    /// `None` once the source has terminally failed — `chunk_broadcaster`
    /// reports that to the hub so the subscribe collapses to a clean failure.
    chunks_tx: Mutex<Option<broadcast::Sender<Bytes>>>,
    /// Set true on any terminal relay outcome; gates duplicate teardown.
    terminal: AtomicBool,
    relay_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl std::fmt::Debug for RemoteCameraStreamSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteCameraStreamSource")
            .field("stream_id", &self.stream_id)
            .field(
                "init_segment_len",
                &self.init_segment.lock().as_ref().map(|b| b.len()),
            )
            .field("terminal", &self.terminal.load(Ordering::Acquire))
            .finish()
    }
}

impl RemoteCameraStreamSource {
    /// Construct the source and start the relay task targeting `owner_node`.
    /// `camera_id` is the owner-local camera id (no `camera:` prefix); the
    /// StreamHub stream id is `camera:<camera_id>`.
    pub fn new(
        iroh: Arc<IrohMeshManager>,
        owner_node: String,
        camera_id: String,
        org_id: String,
    ) -> Arc<Self> {
        let (chunks_tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);
        let source = Arc::new(Self {
            stream_id: format!("camera:{}", camera_id),
            init_segment: Mutex::new(None),
            init_ready: Notify::new(),
            chunks_tx: Mutex::new(Some(chunks_tx)),
            terminal: AtomicBool::new(false),
            relay_task: Mutex::new(None),
        });
        // The task holds a Weak — not an Arc — so the source's refcount is owned
        // solely by the hub. When the hub drops it, the task can no longer
        // upgrade and exits (and Drop aborts it immediately regardless).
        let weak = Arc::downgrade(&source);
        let task = tokio::spawn(spawn_relay(weak, iroh, owner_node, camera_id, org_id));
        *source.relay_task.lock() = Some(task);
        source
    }

    fn seal_init(&self, data: Vec<u8>) {
        let mut guard = self.init_segment.lock();
        if guard.is_none() {
            *guard = Some(Bytes::from(data));
            drop(guard);
            self.init_ready.notify_waiters();
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
        self.init_ready.notify_waiters();
    }
}

/// Relay task body. Holds only a `Weak<RemoteCameraStreamSource>` plus the
/// cloned ids and iroh handle — never an `Arc<Self>`, so the task does not keep
/// the source alive. Opens the bi-stream, seals the init segment from the first
/// `is_init` frame, and broadcasts every subsequent media frame. On any stream
/// end/error or open failure it marks the source terminal (drops the broadcast
/// sender) so subscribers observe `Closed` and the subscribe path fails cleanly.
async fn spawn_relay(
    source: Weak<RemoteCameraStreamSource>,
    iroh: Arc<IrohMeshManager>,
    owner_node: String,
    camera_id: String,
    org_id: String,
) {
    let mut stream = match iroh
        .camera_stream_request(&owner_node, &camera_id, &org_id)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(
                target: "camera::relay",
                owner = %owner_node,
                camera_id = %camera_id,
                "observer: relay open failed: {e}"
            );
            if let Some(src) = source.upgrade() {
                src.mark_terminal();
            }
            return;
        }
    };

    while let Some(item) = stream.next().await {
        let bytes = match item {
            Ok(b) => b,
            Err(e) => {
                tracing::debug!(target: "camera::relay", camera_id = %camera_id, "observer: relay frame error: {e}");
                break;
            }
        };
        let frame: tentaflow_protocol::mesh::CameraStreamFrame =
            match tentaflow_protocol::cbor::decode(&bytes) {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!(target: "camera::relay", "observer: bad relay frame: {e}");
                    break;
                }
            };
        // Upgrade per frame: if the hub has dropped the source (last subscriber
        // gone) the task exits here rather than relaying into a dead source.
        let Some(src) = source.upgrade() else {
            return;
        };
        if frame.is_init {
            src.seal_init(frame.data);
        } else {
            // A send error here only means zero live subscribers; keep relaying
            // so a late subscriber still gets live media. Lag is handled by
            // subscribers (broadcast Lagged).
            if let Some(tx) = src.chunks_tx.lock().as_ref() {
                let _ = tx.send(Bytes::from(frame.data));
            }
        }
    }

    // Relay ended (owner closed / error). Mark terminal so any init waiter
    // returns None and live receivers observe Closed.
    if let Some(src) = source.upgrade() {
        src.mark_terminal();
    }
}

#[async_trait]
impl BinaryStreamSource for RemoteCameraStreamSource {
    fn id(&self) -> &str {
        &self.stream_id
    }

    fn mime_type(&self) -> &str {
        FMP4_H264_MIME
    }

    async fn init_segment(&self) -> Option<Bytes> {
        if let Some(b) = self.init_segment.lock().clone() {
            return Some(b);
        }
        // Subscribe BEFORE re-checking so a notify firing between the lock
        // release and `notified()` is not missed.
        let notified = self.init_ready.notified();
        if let Some(b) = self.init_segment.lock().clone() {
            return Some(b);
        }
        // A relay that already failed before we started waiting: bail now.
        if self.terminal.load(Ordering::Acquire) {
            return None;
        }
        match tokio::time::timeout(INIT_SEGMENT_TIMEOUT, notified).await {
            Ok(()) => self.init_segment.lock().clone(),
            Err(_) => {
                // Init never arrived in time: make the source terminal so
                // `chunk_broadcaster()` returns None and subscribe fails cleanly
                // (live receivers get Closed) instead of registering a hung empty
                // stream that never delivers an init segment.
                self.mark_terminal();
                None
            }
        }
    }

    fn chunk_broadcaster(&self) -> Option<broadcast::Sender<Bytes>> {
        self.chunks_tx.lock().clone()
    }
}

impl Drop for RemoteCameraStreamSource {
    fn drop(&mut self) {
        // Last subscriber gone → abort the relay task, which drops the bi-stream
        // recv half and closes the QUIC stream. The owner observes the close and
        // tears down its own StreamHub subscription (detaching the mux branch if
        // it was the last subscriber). The task held only a Weak<Self>, so this
        // Drop is reached as soon as the hub releases its Arc — no cycle.
        if let Some(task) = self.relay_task.lock().take() {
            task.abort();
        }
    }
}
