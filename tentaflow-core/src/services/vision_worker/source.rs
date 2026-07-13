// ===== File: services/vision_worker/source.rs — core-side relay source for worker cameras =====
//
// A `BinaryStreamSource` that republishes the fMP4 feed a vision worker pumps
// over the UDS link into the core's StreamHub. The dashboard tile subscribes
// to `camera:<id>` exactly as for a local camera — it never learns the
// pipeline runs in a worker process. The push model mirrors
// `camera_relay::source::RemoteCameraStreamSource`, except frames arrive via
// the link read loop (`fleet::handle_worker_frame`) instead of an owned QUIC
// relay task, and the init frame carries `base_pts_ns` so the detection
// overlay anchors on the SAME media timeline the worker's mux produced.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use parking_lot::Mutex;
use tokio::sync::{broadcast, Notify};

use crate::services::camera_relay::source::FMP4_H264_MIME;
use crate::services::stream_hub::{BinaryStreamSource, BROADCAST_CAPACITY};

use super::fleet::WorkerFleet;

/// Maximum time `init_segment()` waits for the worker's first `is_init`
/// frame. Covers the worker-side mux-branch attach + first IDR (the local
/// publisher allows 10 s for the same warmup), after which the hub surfaces
/// a clean failure instead of hanging the subscriber.
const INIT_SEGMENT_TIMEOUT: Duration = Duration::from_secs(10);

/// Core-side source for one worker-camera stream pump. Owned by the
/// StreamHub between the first and last subscriber; `Drop` closes the link
/// stream so the worker detaches its mux branch (lazy end-to-end).
pub struct WorkerCameraStreamSource {
    /// Public hub topic (`camera:<id>` or `camera:<id>#preview`).
    topic: String,
    /// Core-minted id scoping frames on the link.
    link_stream_id: u64,
    /// For `Drop` → `close_stream` (Weak — the fleet's stream router holds a
    /// Weak back to this source, so a strong pointer would be a cycle).
    fleet: Weak<WorkerFleet>,
    init_segment: Mutex<Option<Bytes>>,
    base_pts_ns: Mutex<Option<u64>>,
    init_ready: Notify,
    /// `None` once terminally failed — the hub then collapses the subscribe
    /// to a clean failure instead of a hung empty stream.
    chunks_tx: Mutex<Option<broadcast::Sender<Bytes>>>,
    terminal: AtomicBool,
}

impl std::fmt::Debug for WorkerCameraStreamSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerCameraStreamSource")
            .field("topic", &self.topic)
            .field("link_stream_id", &self.link_stream_id)
            .field("terminal", &self.terminal.load(Ordering::Acquire))
            .finish()
    }
}

impl WorkerCameraStreamSource {
    pub(super) fn new(
        camera_id: &str,
        preview: bool,
        link_stream_id: u64,
        fleet: Weak<WorkerFleet>,
    ) -> Arc<Self> {
        let (chunks_tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);
        Arc::new(Self {
            topic: if preview {
                format!("camera:{camera_id}#preview")
            } else {
                format!("camera:{camera_id}")
            },
            link_stream_id,
            fleet,
            init_segment: Mutex::new(None),
            base_pts_ns: Mutex::new(None),
            init_ready: Notify::new(),
            chunks_tx: Mutex::new(Some(chunks_tx)),
            terminal: AtomicBool::new(false),
        })
    }

    /// One relayed frame from the link read loop. The init frame seals the
    /// preamble + base PTS and unblocks `init_segment()`; media frames fan
    /// out to hub subscribers (zero live subscribers is fine — the hub keeps
    /// this source only while at least one exists, so that window is brief).
    pub(super) fn push_frame(&self, is_init: bool, base_pts_ns: Option<u64>, data: Vec<u8>) {
        if is_init {
            let mut guard = self.init_segment.lock();
            if guard.is_none() {
                *guard = Some(Bytes::from(data));
                drop(guard);
                *self.base_pts_ns.lock() = base_pts_ns;
                self.init_ready.notify_waiters();
            }
            return;
        }
        if let Some(tx) = self.chunks_tx.lock().as_ref() {
            let _ = tx.send(Bytes::from(data));
        }
    }

    /// Terminal failure (worker ended the pump or its link dropped): drop the
    /// broadcast sender so live receivers observe `Closed`, and wake init
    /// waiters so a pending `init_segment()` returns `None` immediately.
    /// Idempotent.
    pub(super) fn mark_terminal(&self) {
        self.terminal.store(true, Ordering::Release);
        *self.chunks_tx.lock() = None;
        self.init_ready.notify_waiters();
    }
}

#[async_trait]
impl BinaryStreamSource for WorkerCameraStreamSource {
    fn id(&self) -> &str {
        &self.topic
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
        if self.terminal.load(Ordering::Acquire) {
            return None;
        }
        match tokio::time::timeout(INIT_SEGMENT_TIMEOUT, notified).await {
            Ok(()) => self.init_segment.lock().clone(),
            Err(_) => {
                // Init never arrived: make the source terminal so the hub
                // fails the subscribe cleanly and the next tile subscribe
                // opens a fresh pump.
                self.mark_terminal();
                None
            }
        }
    }

    fn base_pts_ns(&self) -> Option<u64> {
        *self.base_pts_ns.lock()
    }

    fn chunk_broadcaster(&self) -> Option<broadcast::Sender<Bytes>> {
        self.chunks_tx.lock().clone()
    }
}

impl Drop for WorkerCameraStreamSource {
    fn drop(&mut self) {
        // Last hub subscriber gone → stop the worker-side pump so its local
        // StreamHub handle drops and the mux branch detaches when unused.
        if let Some(fleet) = self.fleet.upgrade() {
            fleet.close_stream(self.link_stream_id);
        }
    }
}
