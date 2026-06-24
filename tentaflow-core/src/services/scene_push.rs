// =============================================================================
// File: services/scene_push.rs — server SHARED-MAP push source for the StreamHub
// =============================================================================
//
// A `BinaryStreamSource` that streams the server-side accumulated SCENE MAP
// (`SlamSceneManager`) to the browser on the generic StreamHub rails, under the
// stream id `scene:<robot_id>`. This is the "one shared map" source of truth: the
// browser renders THIS instead of accumulating client-side, so every viewer sees
// the same persistent map and a late joiner gets the whole room at once.
//
// Payload reuse: the scene map is just world-frame points on a grid, so it is
// encoded as a canonical `LidarFrame` (f32 XYZ) — the browser decodes it with the
// SAME `decodeLidarFrame` it already uses for the live `lidar:` stream. The
// difference is purely semantic: a `scene:` frame REPLACES the rendered map (it is
// the full accumulation), while a `lidar:` frame is the momentary live view.
//
// Cadence: unlike the live lidar stream (per-frame, watch-driven), the full map is
// large and grows slowly, so this pushes a full snapshot on a ~1 Hz timer and only
// when the map's change-`generation` advanced — a static scene never re-sends.
// (Delta streaming is a later optimization; v1 ships the full snapshot, LZ4'd.)

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use bytes::Bytes;
use parking_lot::Mutex;
use tentaflow_sdk_spec::{LidarFrameHeader, LIDAR_FRAME_VERSION, LIDAR_LAYOUT_XYZ};
use tokio::sync::broadcast;

use crate::services::lidar_push::prepare_wire_frame;
use crate::services::slam_scene::{SceneMapSnapshot, SlamSceneManager};
use crate::services::stream_hub::{BinaryStreamSource, BROADCAST_CAPACITY};

/// Throttle on the EVENT-DRIVEN push: a fold wakes the pump immediately, but the
/// (large) full-map snapshot is never re-broadcast faster than this — a burst of
/// frames coalesces into one trailing snapshot. Small enough to feel real-time
/// (camera depth lands within ~one interval of being processed), bounded enough that
/// a growing map can't saturate the wire. The push is also change-gated, so a static
/// scene sends nothing between heartbeats regardless.
const SCENE_MIN_PUSH_INTERVAL: Duration = Duration::from_millis(50);

/// Fallback poll: the per-frame `Notify` covers the hot path, but the map can also
/// change WITHOUT a frame (clear, pose-driven rebuild, grid change) and those sites
/// don't signal. A cheap 1 s generation re-check catches them within the same worst
/// case the old fixed-timer pump had — it only BROADCASTS if the generation moved.
const SCENE_CHANGE_POLL: Duration = Duration::from_millis(1000);

/// Re-broadcast the current map at least this often EVEN IF unchanged. Scene frames
/// are delivered latest-wins/lossy (a full snapshot supersedes any backlog), so a
/// snapshot dropped under backpressure would otherwise leave a viewer stuck on a
/// stale map once the scene goes static. The heartbeat self-heals that within its
/// window while still skipping per-tick re-sends of an unchanged large map.
const SCENE_HEARTBEAT: Duration = Duration::from_secs(5);

/// Scene-map frames are self-describing canonical LiDAR frames, opaque on the wire.
const SCENE_MIME: &str = "application/octet-stream";

fn now_us() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

/// Encode a scene-map snapshot as a canonical f32-XYZ `LidarFrame` (uncompressed;
/// `prepare_wire_frame` LZ4s the body + stamps send time before broadcast). `None`
/// only if the point count overflows the header's `u32`. An EMPTY map encodes a valid
/// 0-point frame (NOT `None`) so a `clear_map` propagates as an authoritative empty
/// snapshot — the renderer clears on `setMapPoints(..., 0)`; without this a viewer
/// would keep showing stale geometry after a clear. `frame_seq` carries the map
/// generation for renderer freshness.
fn encode_scene_frame(snap: &SceneMapSnapshot) -> Option<Bytes> {
    let point_count = snap.points.len() / 3;
    let header = LidarFrameHeader {
        version: LIDAR_FRAME_VERSION,
        layout: LIDAR_LAYOUT_XYZ,
        flags: 0,
        point_count: u32::try_from(point_count).ok()?,
        frame_seq: snap.generation as u32,
        timestamp_us: now_us(),
        host_send_us: 0,
        resolution: snap.resolution,
        origin: [0.0, 0.0, 0.0],
    };
    let mut buf = Vec::with_capacity(header.frame_len()?);
    buf.extend_from_slice(&header.encode_header());
    for v in &snap.points {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    Some(prepare_wire_frame(&Bytes::from(buf)))
}

/// Server scene-map push source. Periodically broadcasts the full accumulated map
/// for `robot_id` from the `SlamSceneManager` whenever it has changed.
pub struct SceneMapStreamSource {
    stream_id: String,
    robot_id: String,
    chunks_tx: Mutex<Option<broadcast::Sender<Bytes>>>,
    terminal: AtomicBool,
    pump_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl std::fmt::Debug for SceneMapStreamSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SceneMapStreamSource")
            .field("stream_id", &self.stream_id)
            .field("terminal", &self.terminal.load(Ordering::Acquire))
            .finish()
    }
}

impl SceneMapStreamSource {
    /// Construct the source for `robot_id` and start the timer pump. Stream id is
    /// `scene:<robot_id>`.
    pub fn new(robot_id: String) -> Arc<Self> {
        let (chunks_tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);
        let source = Arc::new(Self {
            stream_id: format!("scene:{}", robot_id),
            robot_id: robot_id.clone(),
            chunks_tx: Mutex::new(Some(chunks_tx)),
            terminal: AtomicBool::new(false),
            pump_task: Mutex::new(None),
        });
        // The pump holds a Weak so the hub owns the source's refcount: when the last
        // subscriber drops, the hub releases its Arc and the pump exits on the next
        // tick (upgrade fails); Drop also aborts it immediately.
        let weak = Arc::downgrade(&source);
        let task = tokio::spawn(spawn_pump(weak, robot_id));
        *source.pump_task.lock() = Some(task);
        source
    }

    fn broadcast(&self, frame: Bytes) {
        if let Some(tx) = self.chunks_tx.lock().as_ref() {
            let _ = tx.send(frame);
        }
    }

    /// Mark the source terminally failed: drop the broadcast sender so live receivers
    /// observe `Closed`. Idempotent. Mirrors `LocalLidarStreamSource::mark_terminal`.
    fn mark_terminal(&self) {
        self.terminal.store(true, Ordering::Release);
        *self.chunks_tx.lock() = None;
    }
}

/// Timer pump: every `SCENE_PUSH_INTERVAL`, snapshot the robot's shared map and
/// broadcast it if the change-generation advanced since the last send. Holds only a
/// `Weak<SceneMapStreamSource>`, so it never keeps the source alive; it exits when
/// the hub drops the source (upgrade fails).
async fn spawn_pump(source: Weak<SceneMapStreamSource>, robot_id: String) {
    let mgr = SlamSceneManager::global();
    let notify = mgr.change_notifier(&robot_id);
    let mut last_gen: Option<u64> = None;
    let mut last_sent = tokio::time::Instant::now();
    // True once the robot has produced a snapshot. After that, a `None` snapshot means
    // the robot was removed (uninstalled / last instance gone) → terminate the stream
    // so subscribers see `Closed` instead of hanging on stale state. Before the first
    // snapshot, `None` just means "not started yet" — keep waiting.
    let mut seen = false;
    loop {
        // Wake the moment a fold changes the map (fast path), else poll every second
        // to catch non-frame map changes + drive the heartbeat re-send.
        tokio::select! {
            _ = notify.notified() => {}
            _ = tokio::time::sleep(SCENE_CHANGE_POLL) => {}
        }
        // Throttle: coalesce a burst into one trailing snapshot so the large full-map
        // frame never re-broadcasts faster than the min interval.
        let since = last_sent.elapsed();
        if since < SCENE_MIN_PUSH_INTERVAL {
            tokio::time::sleep(SCENE_MIN_PUSH_INTERVAL - since).await;
        }
        let Some(src) = source.upgrade() else {
            return;
        };
        let Some(snap) = mgr.snapshot(&robot_id) else {
            if seen {
                src.mark_terminal();
                return;
            }
            continue;
        };
        seen = true;
        // Send when the map changed (`!=`, so a generation reset / grid rebuild still
        // re-sends), OR on the heartbeat.
        let changed = last_gen != Some(snap.generation);
        let heartbeat_due = last_sent.elapsed() >= SCENE_HEARTBEAT;
        if !changed && !heartbeat_due {
            continue;
        }
        if let Some(frame) = encode_scene_frame(&snap) {
            last_gen = Some(snap.generation);
            last_sent = tokio::time::Instant::now();
            src.broadcast(frame);
        }
    }
}

#[async_trait]
impl BinaryStreamSource for SceneMapStreamSource {
    fn id(&self) -> &str {
        &self.stream_id
    }

    fn mime_type(&self) -> &str {
        SCENE_MIME
    }

    async fn init_segment(&self) -> Option<Bytes> {
        // A fresh subscriber's init IS the current full map, so it renders the whole
        // room immediately instead of waiting for the next tick. `None` (empty map)
        // is valid — the subscriber waits for the first broadcast.
        let snap = SlamSceneManager::global().snapshot(&self.robot_id)?;
        encode_scene_frame(&snap)
    }

    fn chunk_broadcaster(&self) -> Option<broadcast::Sender<Bytes>> {
        self.chunks_tx.lock().clone()
    }

    fn dynamic_init(&self) -> bool {
        // The init (full map) changes over time, so a late joiner must re-fetch the
        // CURRENT snapshot, not a cached preamble.
        true
    }
}

impl Drop for SceneMapStreamSource {
    fn drop(&mut self) {
        if let Some(task) = self.pump_task.lock().take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tentaflow_sdk_spec::{LidarFrameHeader, LIDAR_HEADER_LEN, LIDAR_LAYOUT_XYZ};

    fn frame(points: &[[f32; 3]], resolution: f32, seq: u32) -> Vec<u8> {
        let header = LidarFrameHeader {
            version: LIDAR_FRAME_VERSION,
            layout: LIDAR_LAYOUT_XYZ,
            flags: 0,
            point_count: points.len() as u32,
            frame_seq: seq,
            timestamp_us: 1_000,
            host_send_us: 0,
            resolution,
            origin: [0.0, 0.0, 0.0],
        };
        let mut out = header.encode_header().to_vec();
        for p in points {
            for c in p {
                out.extend_from_slice(&c.to_le_bytes());
            }
        }
        out
    }

    #[test]
    fn encode_scene_frame_roundtrips_as_canonical_lidar_frame() {
        let snap = SceneMapSnapshot {
            resolution: 0.05,
            points: vec![0.0, 0.0, 0.0, 0.05, 0.10, 0.15],
            pose: None,
            last_frame_us: 0,
            generation: 7,
        };
        let wire = encode_scene_frame(&snap).expect("non-empty map encodes");
        let h = LidarFrameHeader::decode_header(&wire).expect("valid header");
        assert_eq!(h.point_count, 2);
        assert_eq!(h.frame_seq, 7, "generation carried as frame_seq");
        assert!((h.resolution - 0.05).abs() < 1e-6);
        assert!(h.host_send_us > 0, "stamped on the way out");
        assert!(wire.len() >= LIDAR_HEADER_LEN);
    }

    #[test]
    fn encode_scene_frame_empty_map_is_a_clear_frame() {
        // An empty map must still encode (a 0-point frame) so a `clear_map` reaches
        // subscribers and the renderer clears, rather than lingering on stale cells.
        let snap = SceneMapSnapshot {
            resolution: 0.05,
            points: vec![],
            pose: None,
            last_frame_us: 0,
            generation: 9,
        };
        let wire = encode_scene_frame(&snap).expect("empty map encodes a clear frame");
        let h = LidarFrameHeader::decode_header(&wire).expect("valid header");
        assert_eq!(h.point_count, 0, "0-point authoritative empty snapshot");
        assert_eq!(h.frame_seq, 9);
    }

    /// The source seeds the full map to a fresh subscriber and pushes again after the
    /// map changes (driven via the real SlamSceneManager + StreamHub broadcast).
    #[tokio::test]
    async fn source_seeds_and_pushes_on_change() {
        let mgr = SlamSceneManager::global();
        let id = "go2-scene-push-a";
        mgr.remove(id);
        mgr.on_lidar_frame(id, &frame(&[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]], 0.05, 1));

        let source = SceneMapStreamSource::new(id.to_string());
        // init = current full map (2 cells).
        let init = source.init_segment().await.expect("seed = current map");
        assert_eq!(LidarFrameHeader::decode_header(&init).unwrap().point_count, 2);

        let mut rx = source
            .chunk_broadcaster()
            .expect("live broadcaster")
            .subscribe();

        // A new cell changes the generation → the pump broadcasts the updated map.
        mgr.on_lidar_frame(id, &frame(&[[2.0, 0.0, 0.0]], 0.05, 2));
        let got = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("a frame within the push interval")
            .expect("broadcast frame");
        assert_eq!(
            LidarFrameHeader::decode_header(&got).unwrap().point_count,
            3,
            "pushed snapshot reflects the 3-cell map"
        );
        mgr.remove(id);
    }

    #[test]
    fn unchanged_map_does_not_bump_generation() {
        // A pre-fused sensor re-sending the SAME world cells must not advance the
        // generation, so the push source stays quiet on a static scene.
        let mgr = SlamSceneManager::global();
        let id = "go2-scene-push-gen";
        mgr.remove(id);
        mgr.on_lidar_frame(id, &frame(&[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]], 0.05, 1));
        let g1 = mgr.generation(id);
        mgr.on_lidar_frame(id, &frame(&[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]], 0.05, 2));
        assert_eq!(mgr.generation(id), g1, "re-sent identical cells → no generation bump");
        mgr.on_lidar_frame(id, &frame(&[[2.0, 0.0, 0.0]], 0.05, 3));
        assert!(mgr.generation(id) > g1, "a new cell bumps generation");
        mgr.remove(id);
    }

    /// Removing the robot terminates the scene source: subscribers observe `Closed`
    /// instead of hanging on a stale map.
    #[tokio::test]
    async fn removal_terminates_source() {
        let mgr = SlamSceneManager::global();
        let id = "go2-scene-push-rm";
        mgr.remove(id);
        mgr.on_lidar_frame(id, &frame(&[[0.0, 0.0, 0.0]], 0.05, 1));

        let source = SceneMapStreamSource::new(id.to_string());
        let mut rx = source
            .chunk_broadcaster()
            .expect("live broadcaster")
            .subscribe();
        // Wait for the first push so the pump has marked the robot `seen`.
        tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("seed push within interval")
            .expect("frame");

        // Robot removed → next tick finds no snapshot and terminates the source.
        mgr.remove(id);
        let closed = loop {
            match tokio::time::timeout(Duration::from_secs(4), rx.recv()).await {
                Ok(Ok(_)) => continue,
                Ok(Err(broadcast::error::RecvError::Closed)) => break true,
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Err(_) => break false,
            }
        };
        assert!(closed, "subscriber observes Closed after robot removal");
        assert!(source.chunk_broadcaster().is_none(), "source marked terminal");
    }
}
