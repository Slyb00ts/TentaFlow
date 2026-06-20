// =============================================================================
// File: services/lidar_push.rs — local robot LiDAR push source for the StreamHub
// =============================================================================
//
// A `BinaryStreamSource` that turns the L2 `LidarStreamHub` (latest-frame-per-
// robot + watch notify) into a pushed binary stream on the generic StreamHub
// rails — the SAME path camera video uses. A browser subscribes to
// `lidar:<robot_id>` via the generic `StreamSubscribeRequest` handler and
// receives each canonical L1 frame as a `StreamFrame` — the real-time PUSH path
// that replaced the former on-demand poll.
//
// LiDAR frames are self-describing (36-byte `LidarFrameHeader` + packed f32), so
// there is no fMP4-style codec preamble: `init_segment` carries the CURRENT
// latest frame so a fresh subscriber renders immediately instead of waiting for
// the next ~200ms publish tick, and `mime_type` is raw `application/octet-stream`.
//
// Lifecycle (mirrors `camera_relay::source`): the StreamHub factory builds the
// source and spawns ONE pump task holding only a `Weak<Self>`, so the hub owns
// the source's refcount. The pump seeds the current latest frame, then loops on
// the hub's `watch::Receiver` and broadcasts every newer frame (latest-wins —
// real-time 3D wants the freshest frame, not a backlog). When the last
// subscriber drops, the hub drops its `Arc<Self>`; `Drop` aborts the pump task
// and the next `Weak::upgrade` would fail anyway — no self-retaining cycle.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use async_trait::async_trait;
use bytes::Bytes;
use parking_lot::Mutex;
use tokio::sync::broadcast;

use crate::services::lidar_hub::LidarStreamHub;
use crate::services::stream_hub::{BinaryStreamSource, BROADCAST_CAPACITY};

/// Canonical LiDAR frames are self-describing, so the wire is opaque bytes; the
/// browser parses the 36-byte header itself rather than trusting this string.
const LIDAR_MIME: &str = "application/octet-stream";

/// Local-robot LiDAR push source. Republishes a robot's canonical frames from
/// the `LidarStreamHub` into the StreamHub broadcast for that robot.
///
/// `init_segment()` returns the latest retained frame so a new subscriber starts
/// rendering at once. Subsequent frames fan out via `broadcast` (latest-wins).
pub struct LocalLidarStreamSource {
    stream_id: String,
    robot_id: String,
    /// `None` once the source has terminally failed (the robot's slot was
    /// removed): `chunk_broadcaster` reports that so subscribe fails cleanly
    /// instead of registering a hung empty stream. Live sources return `Some`.
    chunks_tx: Mutex<Option<broadcast::Sender<Bytes>>>,
    /// Set true on terminal teardown; gates duplicate teardown.
    terminal: AtomicBool,
    pump_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl std::fmt::Debug for LocalLidarStreamSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalLidarStreamSource")
            .field("stream_id", &self.stream_id)
            .field("terminal", &self.terminal.load(Ordering::Acquire))
            .finish()
    }
}

impl LocalLidarStreamSource {
    /// Construct the source for `robot_id` and start the pump task. The StreamHub
    /// stream id is `lidar:<robot_id>`.
    pub fn new(robot_id: String) -> Arc<Self> {
        let (chunks_tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);
        let source = Arc::new(Self {
            stream_id: format!("lidar:{}", robot_id),
            robot_id: robot_id.clone(),
            chunks_tx: Mutex::new(Some(chunks_tx)),
            terminal: AtomicBool::new(false),
            pump_task: Mutex::new(None),
        });
        // Attach the watch receiver SYNCHRONOUSLY, before spawning the pump. If we
        // subscribed inside the spawned task, a `LidarStreamHub::remove()` landing
        // between source creation and the task's first poll would let the pump's
        // own `subscribe()` lazily recreate an empty slot — the pump would then
        // wait forever on a slot nobody publishes to instead of going terminal.
        // Subscribing here closes that window: the receiver observes the existing
        // slot (or the lazily-created one) and any subsequent remove() closes it.
        let rx = LidarStreamHub::global().subscribe(&robot_id);
        // The pump holds a Weak — not an Arc — so the source's refcount is owned
        // solely by the hub. When the hub drops it (last subscriber gone), the
        // pump can no longer upgrade and exits (Drop also aborts it immediately).
        let weak = Arc::downgrade(&source);
        let task = tokio::spawn(spawn_pump(weak, robot_id, rx));
        *source.pump_task.lock() = Some(task);
        source
    }

    /// Mark the source terminally failed: drop the broadcast sender so live
    /// receivers observe `Closed`. Idempotent.
    fn mark_terminal(&self) {
        self.terminal.store(true, Ordering::Release);
        *self.chunks_tx.lock() = None;
    }

    /// Broadcast a frame if there is still a live sender. A send error only means
    /// zero live subscribers; the pump keeps running so a late subscriber still
    /// gets live frames (and the hub will evict the source once the count hits 0).
    fn broadcast(&self, frame: Bytes) {
        if let Some(tx) = self.chunks_tx.lock().as_ref() {
            let _ = tx.send(frame);
        }
    }
}

/// Pump task body. Holds only a `Weak<LocalLidarStreamSource>` plus the robot id
/// and a watch `Receiver` attached SYNCHRONOUSLY in `new()` (no subscribe gap, so
/// a removal can never be missed), so it never keeps the source alive. Seeds the
/// current latest frame, then wakes on every `LidarStreamHub` notify and
/// broadcasts the newer frame. Exits when the hub drops the source (upgrade
/// fails) or the robot's slot is removed (watch sender closed) — in the latter
/// case it marks the source terminal first.
async fn spawn_pump(
    source: Weak<LocalLidarStreamSource>,
    robot_id: String,
    mut rx: tokio::sync::watch::Receiver<u32>,
) {
    let hub = LidarStreamHub::global();

    // Seed the current latest frame so the first subscriber renders immediately.
    // A brand-new robot may have no frame yet (empty slot at seq 0); skip until
    // the first real frame arrives. `init_segment()` also returns this latest for
    // subscribers that attach after the source is already live.
    if let Some(frame) = hub.latest(&robot_id) {
        if !frame.is_empty() {
            let Some(src) = source.upgrade() else {
                return;
            };
            src.broadcast(frame);
        }
    }

    loop {
        // `changed()` resolves on a new `frame_seq` notify, or errors when the
        // robot's slot (and its watch sender) is removed — a terminal condition.
        if rx.changed().await.is_err() {
            if let Some(src) = source.upgrade() {
                src.mark_terminal();
            }
            return;
        }
        // Upgrade per wake: if the hub dropped the source (last subscriber gone)
        // the pump exits here rather than broadcasting into a dead source.
        let Some(src) = source.upgrade() else {
            return;
        };
        // Latest-wins: pull the newest retained bytes, not a per-frame queue. A
        // slow subscriber can never build backpressure — it just sees the freshest
        // frame (lag is handled by the broadcast `Lagged` path upstream).
        if let Some(frame) = hub.latest(&robot_id) {
            if !frame.is_empty() {
                src.broadcast(frame);
            }
        }
    }
}

#[async_trait]
impl BinaryStreamSource for LocalLidarStreamSource {
    fn id(&self) -> &str {
        &self.stream_id
    }

    fn mime_type(&self) -> &str {
        LIDAR_MIME
    }

    async fn init_segment(&self) -> Option<Bytes> {
        // The latest retained frame IS the seed: there is no codec preamble, so a
        // new subscriber's "init" is simply the current frame, delivered before
        // the live broadcast so it renders without waiting for the next tick.
        // `None` (robot has not published) is valid: the handler tolerates a
        // missing init and the subscriber waits for the first broadcast frame.
        LidarStreamHub::global()
            .latest(&self.robot_id)
            .filter(|b| !b.is_empty())
    }

    fn chunk_broadcaster(&self) -> Option<broadcast::Sender<Bytes>> {
        self.chunks_tx.lock().clone()
    }

    fn dynamic_init(&self) -> bool {
        // Latest-wins self-describing stream: every subscriber's init is the
        // CURRENT frame, not a fixed codec preamble. A late joiner must get a
        // fresh `init_segment()` so it renders immediately even if publishing has
        // paused (the cached init may be `None` or a stale frame).
        true
    }
}

impl Drop for LocalLidarStreamSource {
    fn drop(&mut self) {
        // Last subscriber gone → abort the pump task, which drops its watch
        // receiver. The pump held only a Weak<Self>, so this Drop is reached as
        // soon as the hub releases its Arc — no cycle keeps the source resident.
        if let Some(task) = self.pump_task.lock().take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tentaflow_sdk_spec::{
        LidarFrameHeader, LIDAR_FRAME_VERSION, LIDAR_LAYOUT_XYZ,
    };

    fn build_frame(points: &[[f32; 3]], seq: u32) -> Bytes {
        let header = LidarFrameHeader {
            version: LIDAR_FRAME_VERSION,
            layout: LIDAR_LAYOUT_XYZ,
            point_count: points.len() as u32,
            frame_seq: seq,
            timestamp_us: 1_000,
            resolution: 0.05,
            origin: [0.0, 0.0, 0.0],
        };
        let mut buf = Vec::with_capacity(header.frame_len().unwrap());
        buf.extend_from_slice(&header.encode_header());
        for p in points {
            for c in p {
                buf.extend_from_slice(&c.to_le_bytes());
            }
        }
        Bytes::from(buf)
    }

    /// Drive the full source lifecycle against the real `LidarStreamHub`:
    ///   (a) a fresh subscriber gets the SEEDED latest frame (the source pump
    ///       broadcasts the current frame on start),
    ///   (b) a subsequent `publish` of a newer frame is received on the broadcast,
    ///   (c) both delivered chunks round-trip a valid `LidarFrameHeader`,
    ///   (d) `init_segment()` returns the current latest frame.
    #[tokio::test]
    async fn source_seeds_latest_then_pushes_new_frames() {
        let robot_id = "go2-push-test-a";
        let hub = LidarStreamHub::global();
        // Clean slate so a prior test run cannot leak a slot into this one.
        hub.remove(robot_id);

        // A latest frame already exists before the source is built (the seed case).
        let seed = build_frame(&[[1.0, 2.0, 3.0]], 1);
        hub.publish(robot_id, 1, seed.clone());

        let source = LocalLidarStreamSource::new(robot_id.to_string());

        // (d) init_segment reflects the current latest frame.
        let init = source.init_segment().await.expect("init = latest frame");
        assert_eq!(&init[..], &seed[..]);

        // Subscribe to the source's broadcast as the StreamHub would.
        let mut rx = source
            .chunk_broadcaster()
            .expect("live source has a broadcaster")
            .subscribe();

        // (a) The pump seeds the current latest frame onto the broadcast.
        let got_seed = rx.recv().await.expect("seeded frame");
        assert_eq!(&got_seed[..], &seed[..]);
        // (c) round-trips a valid header.
        let h = LidarFrameHeader::decode_header(&got_seed).expect("header decodes");
        assert_eq!(h.frame_seq, 1);
        assert_eq!(h.point_count, 1);

        // (b) A newer publish is received on the broadcast via the watch notify.
        let next = build_frame(&[[4.0, 5.0, 6.0], [7.0, 8.0, 9.0]], 2);
        hub.publish(robot_id, 2, next.clone());
        let got_next = rx.recv().await.expect("pushed frame");
        assert_eq!(&got_next[..], &next[..]);
        // (c) round-trips a valid header.
        let h2 = LidarFrameHeader::decode_header(&got_next).expect("header decodes");
        assert_eq!(h2.frame_seq, 2);
        assert_eq!(h2.point_count, 2);

        hub.remove(robot_id);
    }

    /// A source built before the robot ever published: `init_segment()` is `None`
    /// (no frame yet) and the FIRST published frame is pushed on the broadcast.
    #[tokio::test]
    async fn source_with_no_initial_frame_pushes_first_publish() {
        let robot_id = "go2-push-test-b";
        let hub = LidarStreamHub::global();
        hub.remove(robot_id);

        let source = LocalLidarStreamSource::new(robot_id.to_string());

        // No frame yet → no seed.
        assert!(source.init_segment().await.is_none());

        let mut rx = source
            .chunk_broadcaster()
            .expect("live source has a broadcaster")
            .subscribe();

        let first = build_frame(&[[1.0, 1.0, 1.0]], 1);
        hub.publish(robot_id, 1, first.clone());
        let got = rx.recv().await.expect("first frame pushed");
        assert_eq!(&got[..], &first[..]);
        assert!(LidarFrameHeader::decode_header(&got).is_some());

        hub.remove(robot_id);
    }

    /// Late joiner with PAUSED publishing: a source built with NO initial frame
    /// caches `None` as its init, but after a publish a late subscriber must get
    /// the CURRENT latest frame as its init via the dynamic-init mechanism — not
    /// the stale cached `None`. Proves `dynamic_init()==true` and that a fresh
    /// `init_segment()` reflects the latest frame on a second call after publish,
    /// so the StreamHub re-fetches it for a subsequent subscriber.
    #[tokio::test]
    async fn dynamic_init_serves_current_frame_to_late_joiner() {
        let robot_id = "go2-push-test-d";
        let hub = LidarStreamHub::global();
        hub.remove(robot_id);

        // Source created BEFORE any frame: the StreamHub would cache init = None.
        let source = LocalLidarStreamSource::new(robot_id.to_string());
        assert!(source.dynamic_init(), "lidar source opts into dynamic init");
        assert!(
            source.init_segment().await.is_none(),
            "no frame yet → cached init would be None"
        );

        // Frame 1 is published, then publishing PAUSES (no further frames).
        let frame1 = build_frame(&[[1.0, 2.0, 3.0]], 1);
        hub.publish(robot_id, 1, frame1.clone());

        // A late joiner re-fetching the init (what the hub does for a dynamic-init
        // source on a subsequent subscribe) gets the CURRENT frame, not None —
        // so it renders immediately despite publishing being paused.
        let late_init = source
            .init_segment()
            .await
            .expect("dynamic init = current latest frame for late joiner");
        assert_eq!(&late_init[..], &frame1[..]);
        let h = LidarFrameHeader::decode_header(&late_init).expect("header decodes");
        assert_eq!(h.frame_seq, 1);

        hub.remove(robot_id);
    }

    /// Removing the robot's hub slot closes the pump's watch and marks the source
    /// terminal, so `chunk_broadcaster()` reports failure (clean subscribe denial)
    /// and live receivers observe `Closed`.
    #[tokio::test]
    async fn slot_removal_marks_source_terminal() {
        let robot_id = "go2-push-test-c";
        let hub = LidarStreamHub::global();
        hub.remove(robot_id);
        hub.publish(robot_id, 1, build_frame(&[[1.0, 2.0, 3.0]], 1));

        let source = LocalLidarStreamSource::new(robot_id.to_string());
        let mut rx = source
            .chunk_broadcaster()
            .expect("live broadcaster")
            .subscribe();
        // Drain the seed so we are parked on the next recv.
        let _ = rx.recv().await.expect("seed");

        // Remove the slot: the pump's `changed()` errors → mark_terminal.
        hub.remove(robot_id);

        // The broadcast sender is dropped → receiver observes Closed.
        let closed = loop {
            match rx.recv().await {
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Closed) => break true,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
            }
        };
        assert!(closed);
        assert!(source.chunk_broadcaster().is_none(), "terminal source");
    }
}
