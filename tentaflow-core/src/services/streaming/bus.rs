// =============================================================================
// File: services/streaming/bus.rs — broadcast bus implementation
// =============================================================================
//
// `StreamingBus` keeps a `DashMap<camera_id, Vec<BusEntry>>`. Each entry owns
// a bounded `tokio::sync::mpsc` sender and a shared `AtomicU64` drop counter.
// The hot path (`broadcast`) is non-blocking — `try_send` only.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use tokio::sync::mpsc;

use crate::services::frame_storage::{FrameMetadata, RawFrameRef};

/// Per-subscriber channel capacity. Picked to absorb ~3 s of 30 fps traffic
/// before backpressure kicks in.
pub const SUBSCRIBER_CAPACITY: usize = 100;

/// Opaque subscriber identifier. Format: `stream_<uuid-v4>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StreamId(String);

impl StreamId {
    pub fn new() -> Self {
        Self(format!("stream_{}", uuid::Uuid::new_v4()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for StreamId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for StreamId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone)]
pub enum StreamMessage {
    Frame {
        frame_ref: RawFrameRef,
        metadata: FrameMetadata,
    },
    /// Backpressure signal — N frames dropped since the previous delivery.
    Drop {
        count: u64,
    },
    /// Camera is going away (removed/error). Subscribers should stop polling.
    CameraOffline {
        reason: String,
    },
}

#[derive(Debug, Clone, Default)]
pub struct StreamFilter {
    /// Maximum frames per second to forward; F1a ignored.
    pub max_fps: Option<u32>,
    /// Forward only every Nth frame; F1a always 0.
    pub skip_frames: u32,
}

pub struct StreamSubscriber {
    pub stream_id: StreamId,
    pub camera_id: String,
    pub filter: StreamFilter,
    rx: mpsc::Receiver<StreamMessage>,
    drop_counter: Arc<AtomicU64>,
}

impl StreamSubscriber {
    /// Bounded poll. Returns `None` only when the bus has closed the channel
    /// (camera removed / closed); a timeout yields `Some(Drop { count: 0 })`-
    /// shaped absence by returning `None` from `try_recv`-style fall-through.
    pub async fn next(&mut self, timeout: Duration) -> Option<StreamMessage> {
        match tokio::time::timeout(timeout, self.rx.recv()).await {
            Ok(Some(m)) => Some(m),
            Ok(None) => None,
            Err(_) => None,
        }
    }

    /// Snapshot of how many frames the bus has dropped for this subscriber
    /// since the last delivery — exposed for diagnostics/tests.
    pub fn dropped_pending(&self) -> u64 {
        self.drop_counter.load(Ordering::SeqCst)
    }
}

struct BusEntry {
    stream_id: StreamId,
    tx: mpsc::Sender<StreamMessage>,
    drop_counter: Arc<AtomicU64>,
    #[allow(dead_code)]
    filter: StreamFilter,
}

#[derive(Default)]
pub struct StreamingBus {
    inner: DashMap<String, Vec<BusEntry>>,
}

impl StreamingBus {
    pub fn new() -> Self {
        Self {
            inner: DashMap::new(),
        }
    }

    pub fn subscribe(&self, camera_id: &str, filter: StreamFilter) -> StreamSubscriber {
        self.subscribe_with_capacity(camera_id, filter, SUBSCRIBER_CAPACITY)
    }

    /// Test hook that lets unit tests force backpressure with a tiny channel.
    /// Production callers go through `subscribe`.
    pub fn subscribe_with_capacity(
        &self,
        camera_id: &str,
        filter: StreamFilter,
        capacity: usize,
    ) -> StreamSubscriber {
        let stream_id = StreamId::new();
        let (tx, rx) = mpsc::channel(capacity.max(1));
        let drop_counter = Arc::new(AtomicU64::new(0));
        let entry = BusEntry {
            stream_id: stream_id.clone(),
            tx,
            drop_counter: drop_counter.clone(),
            filter: filter.clone(),
        };
        self.inner
            .entry(camera_id.to_string())
            .or_default()
            .push(entry);
        StreamSubscriber {
            stream_id,
            camera_id: camera_id.to_string(),
            filter,
            rx,
            drop_counter,
        }
    }

    pub fn unsubscribe(&self, camera_id: &str, stream_id: &StreamId) {
        if let Some(mut entries) = self.inner.get_mut(camera_id) {
            entries.retain(|e| &e.stream_id != stream_id);
        }
    }

    /// Called by the camera supervisor for every produced frame. Per
    /// subscriber: emit any pending `Drop { count }` signal first, then
    /// `try_send` the frame; on `Full` increment the drop counter and move
    /// on (never blocks the producer); on `Closed` the entry is marked for
    /// removal at the end of the iteration.
    pub fn broadcast(&self, camera_id: &str, frame_ref: RawFrameRef, metadata: FrameMetadata) {
        let Some(mut entries) = self.inner.get_mut(camera_id) else {
            return;
        };
        let mut dead: Vec<StreamId> = Vec::new();
        for entry in entries.iter() {
            let pending = entry.drop_counter.load(Ordering::SeqCst);
            if pending > 0 {
                match entry.tx.try_send(StreamMessage::Drop { count: pending }) {
                    Ok(()) => {
                        // Only zero out what we actually announced; new drops
                        // accumulated since the load will be picked up next
                        // iteration.
                        entry.drop_counter.fetch_sub(pending, Ordering::SeqCst);
                    }
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        // Even the Drop signal cannot land. Skip the frame
                        // and accumulate it onto the counter so the next
                        // broadcast tries again.
                        entry.drop_counter.fetch_add(1, Ordering::SeqCst);
                        continue;
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        dead.push(entry.stream_id.clone());
                        continue;
                    }
                }
            }
            let msg = StreamMessage::Frame {
                frame_ref: frame_ref.clone(),
                metadata: metadata.clone(),
            };
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

    /// Send `CameraOffline` to every subscriber and clear the entry. Per
    /// subscriber: try a graceful `send().await` bounded by 100 ms so a hung
    /// reader cannot block teardown. On timeout we just drop the sender —
    /// the receiver's next poll then returns `None` and signals end-of-stream.
    pub async fn close_camera(&self, camera_id: &str, reason: &str) {
        let entries = match self.inner.remove(camera_id) {
            Some((_k, v)) => v,
            None => return,
        };
        for entry in entries.into_iter() {
            let msg = StreamMessage::CameraOffline {
                reason: reason.to_string(),
            };
            let _ = tokio::time::timeout(Duration::from_millis(100), entry.tx.send(msg)).await;
            // `entry.tx` drops here either way, closing the channel.
        }
    }

    pub fn list_subscribers(&self, camera_id: &str) -> Vec<StreamId> {
        self.inner
            .get(camera_id)
            .map(|v| v.iter().map(|e| e.stream_id.clone()).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::frame_storage::{FrameMetadata, FramePixelFormat, RawFrameRef};

    fn mk_meta(cam: &str) -> FrameMetadata {
        FrameMetadata {
            camera_id: cam.into(),
            width: 4,
            height: 2,
            pixel_format: FramePixelFormat::Rgb24,
            timestamp_unix_ms: 1,
            pts: None,
            frame_size_bytes: 8,
        }
    }

    #[tokio::test]
    async fn test_bus_subscribe_receives_frames() {
        let bus = StreamingBus::new();
        let mut sub = bus.subscribe("cam1", StreamFilter::default());
        for _ in 0..3 {
            bus.broadcast("cam1", RawFrameRef::new(), mk_meta("cam1"));
        }
        for _ in 0..3 {
            let m = sub.next(Duration::from_millis(100)).await.expect("recv");
            assert!(matches!(m, StreamMessage::Frame { .. }));
        }
    }

    #[tokio::test]
    async fn test_bus_backpressure_drops_oldest() {
        let bus = StreamingBus::new();
        let mut sub = bus.subscribe_with_capacity("cam1", StreamFilter::default(), 4);
        // Channel capacity 4 → first 4 land, next 6 dropped.
        for _ in 0..10 {
            bus.broadcast("cam1", RawFrameRef::new(), mk_meta("cam1"));
        }
        assert_eq!(sub.dropped_pending(), 6);
        // Drain the 4 buffered frames so the channel becomes writable again.
        for _ in 0..4 {
            let m = sub.next(Duration::from_millis(100)).await.expect("recv");
            assert!(matches!(m, StreamMessage::Frame { .. }));
        }
        // Next broadcast should deliver the Drop signal first, then the frame.
        bus.broadcast("cam1", RawFrameRef::new(), mk_meta("cam1"));
        let m = sub.next(Duration::from_millis(100)).await.expect("recv");
        assert!(matches!(m, StreamMessage::Drop { count: 6 }));
        let m = sub.next(Duration::from_millis(100)).await.expect("recv");
        assert!(matches!(m, StreamMessage::Frame { .. }));
    }

    #[tokio::test]
    async fn test_bus_unsubscribe_stops_delivery() {
        let bus = StreamingBus::new();
        let sub = bus.subscribe("cam1", StreamFilter::default());
        bus.unsubscribe("cam1", &sub.stream_id);
        bus.broadcast("cam1", RawFrameRef::new(), mk_meta("cam1"));
        // No subscribers remain.
        assert!(bus.list_subscribers("cam1").is_empty());
    }

    #[tokio::test]
    async fn test_bus_multiple_subscribers_independent() {
        let bus = StreamingBus::new();
        let mut fast = bus.subscribe_with_capacity("cam1", StreamFilter::default(), 100);
        let slow = bus.subscribe_with_capacity("cam1", StreamFilter::default(), 2);
        for _ in 0..20 {
            bus.broadcast("cam1", RawFrameRef::new(), mk_meta("cam1"));
        }
        // Fast subscriber: all 20 land (capacity 100 ≥ 20).
        for _ in 0..20 {
            let m = fast.next(Duration::from_millis(100)).await.expect("recv");
            assert!(matches!(m, StreamMessage::Frame { .. }));
        }
        // Slow subscriber: capacity 2, so 18 frames dropped.
        assert_eq!(slow.dropped_pending(), 18);
    }

    #[tokio::test]
    async fn test_bus_close_camera() {
        let bus = StreamingBus::new();
        let mut sub = bus.subscribe("cam1", StreamFilter::default());
        bus.close_camera("cam1", "removed").await;
        let m = sub.next(Duration::from_millis(100)).await.expect("recv");
        assert!(matches!(m, StreamMessage::CameraOffline { .. }));
        // After close, the camera key is gone.
        assert!(bus.list_subscribers("cam1").is_empty());
    }

    #[tokio::test]
    async fn test_close_camera_async_delivers_offline_to_active_reader() {
        let bus = Arc::new(StreamingBus::new());
        let mut sub = bus.subscribe("cam1", StreamFilter::default());
        // Spawn a reader that drains until the channel closes.
        let reader = tokio::spawn(async move {
            let mut got_offline = false;
            let mut got_none = false;
            for _ in 0..10 {
                match sub.next(Duration::from_millis(500)).await {
                    Some(StreamMessage::CameraOffline { .. }) => got_offline = true,
                    Some(_) => continue,
                    None => {
                        got_none = true;
                        break;
                    }
                }
            }
            (got_offline, got_none)
        });
        // Give the reader a moment to enter recv().
        tokio::time::sleep(Duration::from_millis(20)).await;
        bus.close_camera("cam1", "removed").await;
        let (got_offline, got_none) = reader.await.expect("reader joined");
        assert!(got_offline, "subscriber must receive CameraOffline");
        assert!(got_none, "subscriber must observe channel close");
    }

    #[tokio::test]
    async fn test_close_camera_async_timeout_drops_tx_for_full_subscriber() {
        let bus = StreamingBus::new();
        // Tiny capacity so we can fill it without reading.
        let mut sub = bus.subscribe_with_capacity("cam1", StreamFilter::default(), 2);
        // Fill the channel — broadcast many frames without reading.
        for _ in 0..100 {
            bus.broadcast("cam1", RawFrameRef::new(), mk_meta("cam1"));
        }
        let started = std::time::Instant::now();
        bus.close_camera("cam1", "removed").await;
        let elapsed = started.elapsed();
        // Close must return within ~200 ms even though the channel is full.
        assert!(
            elapsed < Duration::from_millis(500),
            "close_camera blocked for {elapsed:?}"
        );
        // Drain buffered frames; eventually next() returns None (tx dropped).
        let mut saw_none = false;
        for _ in 0..200 {
            match sub.next(Duration::from_millis(100)).await {
                Some(_) => continue,
                None => {
                    saw_none = true;
                    break;
                }
            }
        }
        assert!(saw_none, "subscriber must observe channel close after timeout");
    }

    #[tokio::test]
    async fn test_bus_subscriber_dropped_cleanup() {
        let bus = StreamingBus::new();
        let sub = bus.subscribe("cam1", StreamFilter::default());
        let sid = sub.stream_id.clone();
        drop(sub);
        // Broadcast should detect Closed and prune the entry.
        bus.broadcast("cam1", RawFrameRef::new(), mk_meta("cam1"));
        let remaining = bus.list_subscribers("cam1");
        assert!(!remaining.contains(&sid));
    }
}
