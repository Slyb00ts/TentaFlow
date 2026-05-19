// =============================================================================
// File: services/camera_ingest/stream_publisher.rs — fMP4 publisher per camera
// =============================================================================
//
// Bridges the RTSP session pipeline (Branch B: rtph264depay → h264parse →
// mp4mux → appsink) with the generic `stream_hub::BinaryStreamSource` contract
// so any consumer (WS handler, addon) can subscribe to a live fragmented MP4
// feed. The publisher is built lazily by the `StreamHub` factory the first
// time a consumer subscribes to `camera:<id>`. It then asks the camera
// session to attach an on-demand mux branch to the running pipeline; the
// branch produces fMP4 fragments (one `ftyp+moov` init segment followed by
// rolling `moof+mdat` media chunks).
//
// Init segment delivery uses a `Notify` gate: subscribers waiting on
// `init_segment()` block until the appsink callback observes the first
// buffer (which mp4mux emits as the ftyp+moov "init" segment per its
// `streamable=true` contract). A 3 s timeout protects against dormant
// publishers (e.g. H.265 camera where attach refuses and the branch never
// produces bytes) — `init_segment()` returns `None` so the hub surfaces a
// clean `FactoryFailed` instead of hanging the WS consumer indefinitely.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use parking_lot::Mutex;
use tokio::sync::{broadcast, mpsc, Notify};

use super::session::SessionCommand;
use crate::services::stream_hub::{BinaryStreamSource, BROADCAST_CAPACITY};

/// MIME type advertised to the browser MediaSource layer. AVC level/profile
/// are intentionally not pinned here — the browser tolerates a generic
/// `video/mp4` MIME for fMP4 as long as the init segment carries the avcC
/// box. Browser MSE will refuse if the actual codec inside the init segment
/// is not H.264 (which is enforced upstream by `attach_mp4_branch_supported`).
const FMP4_H264_MIME: &str = "video/mp4; codecs=\"avc1.42E01E\"";

/// Maximum time `init_segment()` waits for the appsink to observe the first
/// fMP4 chunk after `AttachMp4Branch`. Beyond this we assume the publisher
/// will never produce (e.g. H.265 camera, branch refused) and return `None`.
const INIT_SEGMENT_TIMEOUT: Duration = Duration::from_secs(3);

/// Publishes fragmented MP4 chunks produced by a camera session's Branch B
/// mux to any number of WS subscribers.
///
/// Lifecycle:
///   1. `StreamHub` factory constructs the publisher and posts
///      `SessionCommand::AttachMp4Branch(Arc<Self>)` to the session task,
///      then drops its strong reference so only the hub keeps a strong Arc.
///   2. The session attaches the on-demand mux branch and installs an
///      appsink callback that calls `push_chunk` through a `Weak<Self>`.
///   3. The first chunk seeds the init segment and unblocks
///      `init_segment()`. Subsequent chunks fan out via `broadcast`.
///   4. When the hub's strong reference drops (last subscriber unsubscribed),
///      `Drop` posts `SessionCommand::DetachMp4Branch` to the session so the
///      mux branch is torn down and pipeline CPU returns to baseline.
pub struct Mp4StreamPublisher {
    stream_id: String,
    init_segment: Mutex<Option<Bytes>>,
    init_ready: Notify,
    chunks_tx: broadcast::Sender<Bytes>,
    cmd_tx: mpsc::Sender<SessionCommand>,
}

impl std::fmt::Debug for Mp4StreamPublisher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mp4StreamPublisher")
            .field("stream_id", &self.stream_id)
            .field(
                "init_segment_len",
                &self.init_segment.lock().as_ref().map(|b| b.len()),
            )
            .field("subscribers", &self.chunks_tx.receiver_count())
            .finish()
    }
}

impl Mp4StreamPublisher {
    /// Construct a fresh publisher. The hub-facing `Arc` is created by the
    /// caller (`Arc::new(Mp4StreamPublisher::new(...))`) so the strong ref
    /// count is well-defined from the start.
    pub fn new(camera_id: String, cmd_tx: mpsc::Sender<SessionCommand>) -> Self {
        let (chunks_tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            stream_id: format!("camera:{}", camera_id),
            init_segment: Mutex::new(None),
            init_ready: Notify::new(),
            chunks_tx,
            cmd_tx,
        }
    }

    /// Push one mux fragment from the appsink callback. The first call seeds
    /// the init segment and wakes any waiter blocked in `init_segment()`;
    /// every later call broadcasts to live subscribers. We do not signal an
    /// error when the broadcast channel has no receivers — that simply means
    /// nobody is listening yet (the publisher exists between the factory
    /// call and the first subscriber receiver attach).
    pub fn push_chunk(&self, bytes: Vec<u8>) {
        let chunk = Bytes::from(bytes);
        let mut guard = self.init_segment.lock();
        if guard.is_none() {
            *guard = Some(chunk);
            drop(guard);
            self.init_ready.notify_waiters();
            return;
        }
        drop(guard);
        let _ = self.chunks_tx.send(chunk);
    }

    /// Mark the publisher as permanently undeliverable. Called by the session
    /// when the attach refuses (non-H.264 codec, mux build failure) so that
    /// `init_segment()` does not block waiters for the full timeout. Idempotent.
    pub fn mark_unsupported(&self) {
        // Wake every waiter — they will observe `init_segment` still None and
        // return `None`. Equivalent to the timeout path but immediate.
        self.init_ready.notify_waiters();
    }
}

#[async_trait]
impl BinaryStreamSource for Mp4StreamPublisher {
    fn id(&self) -> &str {
        &self.stream_id
    }

    fn mime_type(&self) -> &str {
        FMP4_H264_MIME
    }

    async fn init_segment(&self) -> Option<Bytes> {
        // Fast path: appsink has already produced the first fragment.
        if let Some(b) = self.init_segment.lock().clone() {
            return Some(b);
        }
        // Slow path: subscribe BEFORE re-checking so we cannot race a notify
        // that fires between the lock release and `notified()`.
        let notified = self.init_ready.notified();
        if let Some(b) = self.init_segment.lock().clone() {
            return Some(b);
        }
        match tokio::time::timeout(INIT_SEGMENT_TIMEOUT, notified).await {
            Ok(()) => self.init_segment.lock().clone(),
            Err(_) => None,
        }
    }

    fn chunk_broadcaster(&self) -> &broadcast::Sender<Bytes> {
        &self.chunks_tx
    }
}

impl Drop for Mp4StreamPublisher {
    fn drop(&mut self) {
        // Best-effort detach. `try_send` so the drop never blocks; if the
        // session's command queue is saturated the branch will be cleaned
        // up when the session itself shuts down (which always tears the
        // whole pipeline down anyway). The session is the only entity that
        // can untangle the mux elements from the running pipeline, so this
        // is the canonical teardown path.
        let _ = self.cmd_tx.try_send(SessionCommand::DetachMp4Branch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_publisher() -> (Arc<Mp4StreamPublisher>, mpsc::Receiver<SessionCommand>) {
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let pub_ = Arc::new(Mp4StreamPublisher::new("cam_test".into(), cmd_tx));
        (pub_, cmd_rx)
    }

    #[tokio::test]
    async fn init_segment_cached_on_first_chunk() {
        let (pub_, _cmd_rx) = make_publisher();
        pub_.push_chunk(vec![0xDE, 0xAD, 0xBE, 0xEF]);
        let init = pub_.init_segment().await.expect("init present");
        assert_eq!(&init[..], &[0xDE, 0xAD, 0xBE, 0xEF]);
        // Re-calling returns the same cached buffer (no re-allocation).
        let init2 = pub_.init_segment().await.expect("init still present");
        assert_eq!(&init2[..], &[0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[tokio::test]
    async fn init_segment_notify_unblocks_waiters() {
        let (pub_, _cmd_rx) = make_publisher();
        let pub_for_push = Arc::clone(&pub_);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            pub_for_push.push_chunk(vec![1, 2, 3]);
        });
        let init = pub_.init_segment().await.expect("init via notify");
        assert_eq!(&init[..], &[1, 2, 3]);
    }

    #[tokio::test]
    async fn init_segment_timeout_returns_none_for_dormant() {
        let (pub_, _cmd_rx) = make_publisher();
        // Pause is the only knob to keep the test fast; otherwise we wait
        // the full 3 s timeout.
        let start = tokio::time::Instant::now();
        let result = pub_.init_segment().await;
        let elapsed = start.elapsed();
        assert!(result.is_none(), "dormant publisher must yield None");
        // We do not pin the upper bound tightly — 3 s ± scheduler jitter.
        assert!(
            elapsed >= INIT_SEGMENT_TIMEOUT,
            "must wait at least the timeout window, got {elapsed:?}"
        );
        assert!(
            elapsed < INIT_SEGMENT_TIMEOUT + Duration::from_secs(2),
            "must not overshoot the timeout by more than 2 s, got {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn subsequent_chunks_broadcast_to_subscribers() {
        let (pub_, _cmd_rx) = make_publisher();
        // Seed the init segment first — only chunks pushed AFTER the init
        // segment travel through the broadcast channel.
        pub_.push_chunk(vec![0xFF]);
        let _ = pub_.init_segment().await.expect("init seeded");
        let mut rx = pub_.chunk_broadcaster().subscribe();
        pub_.push_chunk(vec![1, 2, 3]);
        pub_.push_chunk(vec![4, 5, 6]);
        let first = rx.recv().await.expect("first chunk");
        let second = rx.recv().await.expect("second chunk");
        assert_eq!(&first[..], &[1, 2, 3]);
        assert_eq!(&second[..], &[4, 5, 6]);
    }

    #[tokio::test]
    async fn multiple_subscribers_receive_same_chunks() {
        let (pub_, _cmd_rx) = make_publisher();
        pub_.push_chunk(vec![0xAA]);
        let _ = pub_.init_segment().await.expect("init seeded");
        let mut rx1 = pub_.chunk_broadcaster().subscribe();
        let mut rx2 = pub_.chunk_broadcaster().subscribe();
        pub_.push_chunk(vec![9, 9, 9]);
        let a = rx1.recv().await.expect("rx1");
        let b = rx2.recv().await.expect("rx2");
        assert_eq!(&a[..], &[9, 9, 9]);
        assert_eq!(&b[..], &[9, 9, 9]);
    }

    #[tokio::test]
    async fn drop_posts_detach_command() {
        let (pub_, mut cmd_rx) = make_publisher();
        // While the strong Arc lives the channel must be empty.
        assert!(cmd_rx.try_recv().is_err());
        drop(pub_);
        let cmd = cmd_rx.recv().await.expect("detach command on drop");
        assert!(matches!(cmd, SessionCommand::DetachMp4Branch));
    }

    #[tokio::test]
    async fn mark_unsupported_unblocks_waiters_with_none() {
        let (pub_, _cmd_rx) = make_publisher();
        let pub_for_mark = Arc::clone(&pub_);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            pub_for_mark.mark_unsupported();
        });
        let init = pub_.init_segment().await;
        assert!(init.is_none(), "unsupported publisher must yield None");
    }
}
