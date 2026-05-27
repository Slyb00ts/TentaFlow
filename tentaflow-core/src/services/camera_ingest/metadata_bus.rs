// =============================================================================
// File: services/camera_ingest/metadata_bus.rs — fan-out bus for ONVIF
// analytics-metadata frames (F2 P6.a).
// =============================================================================
//
// Mirrors `services/streaming/bus.rs` but ships `MetadataFrame` payloads
// (lists of detected objects with bounding boxes) instead of raw video
// frames. The two buses are intentionally separate:
//   * Frame rate / cadence differs (motion events fire bursty, video flows
//     at fixed FPS) so a single bounded channel would amplify backpressure
//     across both.
//   * Subscribers care about different streams — an addon may want
//     metadata-only or video-only.
//
// Per-subscriber capacity: 64 messages. At a typical analytics rate of
// 1 event/s this absorbs ~1 minute of backpressure. The producer is the
// PullPoint poll loop (one task per camera with metadata_supported = 1);
// the consumer is an addon-side reader. `try_send` semantics — never blocks
// the producer.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use tokio::sync::mpsc;

use crate::services::camera_ingest::onvif_metadata_parser::MetadataItem;

/// Per-subscriber channel capacity for the metadata bus.
pub const METADATA_SUBSCRIBER_CAPACITY: usize = 64;

/// Opaque subscriber identifier. Format: `meta_<uuid-v4>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MetadataStreamId(String);

impl MetadataStreamId {
    pub fn new() -> Self {
        Self(format!("meta_{}", uuid::Uuid::new_v4()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Reconstruct a stream id from the wire-form string. Used by host
    /// functions that received the opaque id from the addon and need to
    /// look it up in the bus. The bus never validates the inner shape; a
    /// non-existent id yields a no-op on `unsubscribe`.
    pub fn new_from_raw(raw: String) -> Self {
        Self(raw)
    }
}

impl Default for MetadataStreamId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for MetadataStreamId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A single batch of analytics objects detected at `ts_unix` for `camera_id`.
/// Multiple items can ride the same frame (one analytics tick produces one
/// `MetadataFrame` carrying every concurrent detection).
#[derive(Debug, Clone, PartialEq)]
pub struct MetadataFrame {
    pub camera_id: String,
    pub ts_unix: i64,
    pub items: Vec<MetadataItem>,
}

#[derive(Debug, Clone)]
pub enum MetadataMessage {
    Frame(MetadataFrame),
    /// Backpressure signal — N frames dropped since the previous delivery.
    Drop {
        count: u64,
    },
    /// Camera removed or pull loop exited. Subscribers should stop polling.
    CameraOffline {
        reason: String,
    },
}

#[derive(Debug)]
pub enum NextOutcome {
    Message(MetadataMessage),
    Closed,
    Timeout,
}

pub struct MetadataSubscriber {
    pub stream_id: MetadataStreamId,
    pub camera_id: String,
    rx: mpsc::Receiver<MetadataMessage>,
    drop_counter: Arc<AtomicU64>,
}

impl MetadataSubscriber {
    pub async fn next(&mut self, timeout: Duration) -> NextOutcome {
        match tokio::time::timeout(timeout, self.rx.recv()).await {
            Ok(Some(m)) => NextOutcome::Message(m),
            Ok(None) => NextOutcome::Closed,
            Err(_) => NextOutcome::Timeout,
        }
    }

    pub fn dropped_pending(&self) -> u64 {
        self.drop_counter.load(Ordering::SeqCst)
    }
}

struct BusEntry {
    stream_id: MetadataStreamId,
    tx: mpsc::Sender<MetadataMessage>,
    drop_counter: Arc<AtomicU64>,
}

#[derive(Default)]
pub struct MetadataBus {
    inner: DashMap<String, Vec<BusEntry>>,
}

impl MetadataBus {
    pub fn new() -> Self {
        Self {
            inner: DashMap::new(),
        }
    }

    pub fn subscribe(&self, camera_id: &str) -> MetadataSubscriber {
        self.subscribe_with_capacity(camera_id, METADATA_SUBSCRIBER_CAPACITY)
    }

    pub fn subscribe_with_capacity(&self, camera_id: &str, capacity: usize) -> MetadataSubscriber {
        let stream_id = MetadataStreamId::new();
        let (tx, rx) = mpsc::channel(capacity.max(1));
        let drop_counter = Arc::new(AtomicU64::new(0));
        let entry = BusEntry {
            stream_id: stream_id.clone(),
            tx,
            drop_counter: drop_counter.clone(),
        };
        self.inner
            .entry(camera_id.to_string())
            .or_default()
            .push(entry);
        MetadataSubscriber {
            stream_id,
            camera_id: camera_id.to_string(),
            rx,
            drop_counter,
        }
    }

    pub fn unsubscribe(&self, camera_id: &str, stream_id: &MetadataStreamId) {
        if let Some(mut entries) = self.inner.get_mut(camera_id) {
            entries.retain(|e| &e.stream_id != stream_id);
        }
    }

    /// F2 P6.b — addon-facing unsubscribe path. The addon holds the opaque
    /// `MetadataStreamId` (`meta_<uuid>`) but does not retain the camera_id.
    /// Walks every per-camera entry list looking for the stream and, on hit,
    /// returns the camera_id so the caller (the host fn) can release the
    /// pull-supervisor refcount. `None` means the stream was already
    /// unsubscribed or never existed.
    pub fn unsubscribe_by_stream_id(&self, stream_id: &MetadataStreamId) -> Option<String> {
        for mut entry in self.inner.iter_mut() {
            let camera_id = entry.key().clone();
            let entries = entry.value_mut();
            let before = entries.len();
            entries.retain(|e| &e.stream_id != stream_id);
            if entries.len() != before {
                return Some(camera_id);
            }
        }
        None
    }

    /// Fan out one `MetadataFrame` to every subscriber on `frame.camera_id`.
    /// Backpressure: if the channel is full we increment a per-subscriber
    /// drop counter and deliver a `Drop { count }` on the next successful
    /// send. Closed channels prune the registry slot.
    pub fn publish(&self, frame: MetadataFrame) {
        let Some(mut entries) = self.inner.get_mut(&frame.camera_id) else {
            return;
        };
        let mut dead: Vec<MetadataStreamId> = Vec::new();
        for entry in entries.iter() {
            let pending = entry.drop_counter.load(Ordering::SeqCst);
            if pending > 0 {
                match entry.tx.try_send(MetadataMessage::Drop { count: pending }) {
                    Ok(()) => {
                        entry.drop_counter.fetch_sub(pending, Ordering::SeqCst);
                    }
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        entry.drop_counter.fetch_add(1, Ordering::SeqCst);
                        continue;
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        dead.push(entry.stream_id.clone());
                        continue;
                    }
                }
            }
            let msg = MetadataMessage::Frame(frame.clone());
            match entry.tx.try_send(msg) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    entry.drop_counter.fetch_add(1, Ordering::SeqCst);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    dead.push(entry.stream_id.clone());
                }
            }
        }
        if !dead.is_empty() {
            entries.retain(|e| !dead.contains(&e.stream_id));
        }
    }

    pub async fn close_camera(&self, camera_id: &str, reason: &str) {
        let entries = match self.inner.remove(camera_id) {
            Some((_k, v)) => v,
            None => return,
        };
        for entry in entries.into_iter() {
            let msg = MetadataMessage::CameraOffline {
                reason: reason.to_string(),
            };
            let _ = tokio::time::timeout(Duration::from_millis(100), entry.tx.send(msg)).await;
        }
    }

    pub fn list_subscribers(&self, camera_id: &str) -> Vec<MetadataStreamId> {
        self.inner
            .get(camera_id)
            .map(|v| v.iter().map(|e| e.stream_id.clone()).collect())
            .unwrap_or_default()
    }
}

/// Process-wide singleton — same pattern as `StreamingBus`. The bus is
/// instantiated on first use and lives for the process lifetime; nothing
/// owns it directly.
pub fn metadata_bus() -> &'static MetadataBus {
    use std::sync::OnceLock;
    static BUS: OnceLock<MetadataBus> = OnceLock::new();
    BUS.get_or_init(MetadataBus::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_frame(cam: &str, ts: i64) -> MetadataFrame {
        MetadataFrame {
            camera_id: cam.into(),
            ts_unix: ts,
            items: Vec::new(),
        }
    }

    #[tokio::test]
    async fn subscribe_and_publish_delivers_frame() {
        let bus = MetadataBus::new();
        let mut sub = bus.subscribe("camA");
        bus.publish(mk_frame("camA", 100));
        let m = match sub.next(Duration::from_millis(100)).await {
            NextOutcome::Message(m) => m,
            other => panic!("expected message, got {other:?}"),
        };
        match m {
            MetadataMessage::Frame(f) => {
                assert_eq!(f.camera_id, "camA");
                assert_eq!(f.ts_unix, 100);
            }
            other => panic!("expected Frame, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn publish_to_unknown_camera_is_noop() {
        let bus = MetadataBus::new();
        // No subscriber on this camera — publish must not panic, no entries
        // are created.
        bus.publish(mk_frame("ghost", 1));
        assert!(bus.list_subscribers("ghost").is_empty());
    }

    #[tokio::test]
    async fn backpressure_emits_drop_signal() {
        let bus = MetadataBus::new();
        let mut sub = bus.subscribe_with_capacity("camA", 2);
        for i in 0..10 {
            bus.publish(mk_frame("camA", i));
        }
        // 2 frames buffered, 8 dropped.
        assert_eq!(sub.dropped_pending(), 8);
        // Drain the 2 buffered.
        for _ in 0..2 {
            match sub.next(Duration::from_millis(50)).await {
                NextOutcome::Message(MetadataMessage::Frame(_)) => {}
                other => panic!("expected Frame, got {other:?}"),
            }
        }
        // Next publish: a Drop signal first, then the frame.
        bus.publish(mk_frame("camA", 99));
        match sub.next(Duration::from_millis(50)).await {
            NextOutcome::Message(MetadataMessage::Drop { count }) => assert_eq!(count, 8),
            other => panic!("expected Drop, got {other:?}"),
        }
        match sub.next(Duration::from_millis(50)).await {
            NextOutcome::Message(MetadataMessage::Frame(f)) => assert_eq!(f.ts_unix, 99),
            other => panic!("expected Frame, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unsubscribe_stops_delivery() {
        let bus = MetadataBus::new();
        let sub = bus.subscribe("camA");
        let sid = sub.stream_id.clone();
        bus.unsubscribe("camA", &sid);
        bus.publish(mk_frame("camA", 1));
        // No subscribers remain.
        assert!(bus.list_subscribers("camA").is_empty());
    }
}
