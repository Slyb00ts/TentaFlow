// =============================================================================
// File: services/stream_hub/subscription.rs — per-peer subscription handle
// =============================================================================
//
// `SubscriptionHandle` is the RAII contract returned by `StreamHub::subscribe`.
// It carries everything a downstream WS handler needs to push a fresh peer
// into a live stream:
//   * the MIME type (for `MediaSource.addSourceBuffer`),
//   * the init segment (delivered once before any media chunk),
//   * a broadcast receiver yielding media chunks for the lifetime of the peer.
//
// Dropping the handle decrements the source's subscriber counter. When the
// counter reaches zero the hub drops the underlying `Arc<dyn BinaryStreamSource>`
// so the producing pipeline can detach (close cameras, stop muxers, ...).

use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::broadcast;

use super::manager::StreamHub;

/// Live subscription to a binary stream. Always obtain through
/// [`StreamHub::subscribe`]; the inner token enforces refcount cleanup.
pub struct SubscriptionHandle {
    pub stream_id: String,
    pub mime_type: String,
    pub init_segment: Option<Bytes>,
    /// Bazowy PTS osi mediów (ns) zrodla fMP4 — przekazywany klientowi razem z
    /// init-segmentem, by odjac offset osi mediów od PTS detekcji. `None` dla
    /// zrodel bez wspolnej osi czasu z detekcjami.
    pub base_pts_ns: Option<u64>,
    pub receiver: broadcast::Receiver<Bytes>,
    // Held purely for the `Drop` side effect: decrements the hub's
    // per-source subscriber counter and removes the source when it hits 0.
    _token: SubscriberToken,
}

impl std::fmt::Debug for SubscriptionHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubscriptionHandle")
            .field("stream_id", &self.stream_id)
            .field("mime_type", &self.mime_type)
            .field(
                "init_segment_len",
                &self.init_segment.as_ref().map(|b| b.len()),
            )
            .finish()
    }
}

impl SubscriptionHandle {
    pub(super) fn new(
        stream_id: String,
        mime_type: String,
        init_segment: Option<Bytes>,
        base_pts_ns: Option<u64>,
        receiver: broadcast::Receiver<Bytes>,
        token: SubscriberToken,
    ) -> Self {
        Self {
            stream_id,
            mime_type,
            init_segment,
            base_pts_ns,
            receiver,
            _token: token,
        }
    }
}

/// RAII guard. Constructed by the hub after it has incremented the per-source
/// counter; its `Drop` impl mirrors that increment with a decrement and
/// triggers source teardown when the counter reaches zero.
pub(super) struct SubscriberToken {
    hub: Arc<StreamHub>,
    stream_id: String,
}

impl SubscriberToken {
    pub(super) fn new(hub: Arc<StreamHub>, stream_id: String) -> Self {
        Self { hub, stream_id }
    }
}

impl Drop for SubscriberToken {
    fn drop(&mut self) {
        self.hub.decrement_subscriber(&self.stream_id);
    }
}
