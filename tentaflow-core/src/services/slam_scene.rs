// =============================================================================
// File: services/slam_scene.rs
// Purpose: SlamSceneManager — the server-side SHARED scene map. It folds every
//          robot's pre-fused, world-frame LiDAR frames (Go2 `voxel_map_compressed`,
//          option B per UNIFIED_SLAM_ARCHITECTURE §15) into ONE persistent,
//          deduplicated occupancy map per robot, and tracks the robot's GlobalPose
//          from its `robot_pose` telemetry. This moves map accumulation from each
//          browser client (ephemeral, per-tab) to the server (one source of truth,
//          survives refresh, ready to stream to every viewer and — later — to merge
//          across robots into a single common map).
//
//          Sync, lock-light: `on_lidar_frame` runs at frame rate from the robot
//          drain tick (the `lidar.publish` host-fn), so it must never block or take
//          a global lock — state is a per-robot `parking_lot::Mutex`, never awaited.
//          One `SlamService` per robot (keyed by robot_id == addon_id, same key the
//          `LidarStreamHub` uses).
// =============================================================================

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

use dashmap::DashMap;
use parking_lot::Mutex;
use tentaflow_sdk_spec::{GlobalPoseFrame, LidarFrameHeader};
use tentaflow_slam::{GeoAnchor, LioConfig, Pose, SealPolicy, SlamService};

/// Fallback grid resolution (metres) for a robot whose pose arrives before its
/// first LiDAR frame. The Go2 voxel map is 0.05 m; a frame-first robot (the common
/// case at ~7 Hz vs the slower pose cadence) overrides this with the header value.
const DEFAULT_RESOLUTION_M: f32 = 0.05;

/// Per-robot SLAM state: the accumulating scene map + the latest world pose.
struct RobotSlam {
    service: SlamService,
    /// Grid resolution the `SlamService`'s voxel map was created with.
    resolution: f32,
    /// Last adopted device pose; re-applied per frame so the map-fold path keeps the
    /// pose consistent without waiting for the next (slower) telemetry read.
    last_pose: Pose,
    /// True once a real `robot_pose` has been adopted — until then the GlobalPose is
    /// at identity and must NOT be published as a confident fix.
    has_pose: bool,
    /// Latest GlobalPose (only set once `has_pose`).
    latest_global: Option<GlobalPoseFrame>,
    last_frame_us: i64,
    /// Monotonic map version the scene push source polls so it only re-broadcasts the
    /// full snapshot when the map ACTUALLY changed (never on a static scene). Bumped
    /// only when the voxel map's `revision` advances (a cell added/evicted), so a
    /// pre-fused sensor re-sending the same world cells does not churn it.
    generation: u64,
    /// Last observed `SceneVoxelMap::revision` — drives the change-only `generation`.
    last_revision: u64,
}

impl RobotSlam {
    /// Advance the monotonic `generation` iff the underlying voxel set changed
    /// (revision moved). Keeps `generation` change-only AND monotonic, so the push
    /// source skips a static map and a browser's freshness check never sees it go back.
    fn sync_generation(&mut self) {
        let rev = self.service.scene_voxel_map().revision();
        if rev != self.last_revision {
            self.last_revision = rev;
            self.generation = self.generation.wrapping_add(1);
        }
    }

    /// Re-emit the GlobalPose from the last known pose so a geo-anchor change takes
    /// effect IMMEDIATELY (Global after a set, scene-local after a clear) instead of
    /// leaving a stale cached pose until the next telemetry tick.
    fn refresh_global_pose(&mut self) {
        if self.has_pose {
            self.latest_global = Some(self.service.adopt_pose(self.last_pose, self.last_frame_us));
        }
    }
}

/// A renderable snapshot of one robot's shared scene map.
#[derive(Debug, Clone)]
pub struct SceneMapSnapshot {
    pub resolution: f32,
    /// Occupied cell centres in the scene frame, packed `[x,y,z,x,y,z,...]`.
    pub points: Vec<f32>,
    /// Latest world pose, if a `robot_pose` has been seen.
    pub pose: Option<GlobalPoseFrame>,
    pub last_frame_us: i64,
    /// Map change-generation at snapshot time (see `RobotSlam::generation`).
    pub generation: u64,
}

/// Process-wide shared-scene manager.
pub struct SlamSceneManager {
    robots: DashMap<String, Mutex<RobotSlam>>,
    /// Durable-intent geo anchors keyed by robot_id. Survives the lazy creation of a
    /// robot's `SlamService` (set before the robot ever streams) and is applied to the
    /// service on creation; also the source the persistence layer serializes.
    anchors: DashMap<String, GeoAnchor>,
}

impl SlamSceneManager {
    fn new() -> Self {
        Self { robots: DashMap::new(), anchors: DashMap::new() }
    }

    /// Process-wide singleton.
    pub fn global() -> &'static SlamSceneManager {
        static INSTANCE: OnceLock<SlamSceneManager> = OnceLock::new();
        INSTANCE.get_or_init(SlamSceneManager::new)
    }

    /// Deterministic scene id for a robot. For now each robot gets its OWN scene
    /// (its odom frame IS its scene frame); cross-robot fusion into one shared scene
    /// id with per-robot alignment is the next phase. Deterministic so a robot keeps
    /// the same scene id across reconnects and (later) across nodes.
    fn scene_id(robot_id: &str) -> u64 {
        let mut h = DefaultHasher::new();
        robot_id.hash(&mut h);
        h.finish()
    }

    fn make_service(&self, robot_id: &str, resolution: f32) -> RobotSlam {
        let scene = Self::scene_id(robot_id);
        // origin_node 0: single-node phase. submap_voxel = grid resolution so the
        // pre-fused dedup grid matches the sensor (Go2 0.05 m).
        let mut service = SlamService::new(
            0,
            scene,
            LioConfig::default(),
            resolution,
            SealPolicy::default(),
        );
        // Apply a geo anchor pinned before the robot ever streamed (durable intent).
        if let Some(a) = self.anchors.get(robot_id) {
            service.set_geo_anchor(*a);
        }
        RobotSlam {
            service,
            resolution,
            last_pose: Pose::identity(),
            has_pose: false,
            latest_global: None,
            last_frame_us: 0,
            generation: 0,
            last_revision: 0,
        }
    }

    /// Fold one canonical world-frame LiDAR frame into the robot's shared scene map.
    /// Called at frame rate from the `lidar.publish` host-fn. Cheap + non-blocking:
    /// decodes once, dedups into the voxel grid. A malformed frame is dropped.
    pub fn on_lidar_frame(&self, robot_id: &str, frame: &[u8]) {
        // Peek the header to learn the grid resolution BEFORE creating the service
        // (so the dedup grid matches the sensor). A bad header → drop the frame.
        let Some(header) = LidarFrameHeader::decode_header(frame) else {
            return;
        };
        let resolution = if header.resolution.is_finite() && header.resolution > 0.0 {
            header.resolution
        } else {
            DEFAULT_RESOLUTION_M
        };

        let entry = self
            .robots
            .entry(robot_id.to_string())
            .or_insert_with(|| Mutex::new(self.make_service(robot_id, resolution)));
        let mut slam = entry.lock();
        // If the service was seeded pose-first at the default grid and the real frame
        // declares a different resolution, rebuild it on the sensor's true grid —
        // safe because no geometry has accumulated yet (pose-first leaves the map
        // empty), so nothing is lost. The frame header is authoritative for a
        // pre-fused sensor; without this the map would dedup on the wrong grid.
        if (slam.resolution - resolution).abs() > 1e-6 && slam.service.scene_voxel_map().is_empty() {
            let prev_pose = slam.last_pose;
            let had_pose = slam.has_pose;
            let prev_us = slam.last_frame_us;
            let mut fresh = self.make_service(robot_id, resolution);
            if had_pose {
                let gp = fresh.service.adopt_pose(prev_pose, prev_us);
                fresh.last_pose = prev_pose;
                fresh.has_pose = true;
                fresh.latest_global = Some(gp);
            }
            *slam = fresh;
        }
        let pose = slam.last_pose;
        if let Some(gp) = slam.service.ingest_world_frame(frame, pose, None) {
            slam.last_frame_us = gp.timestamp_us;
            // Bump generation only if this frame actually changed the occupied set; a
            // pre-fused sensor re-sending the same cells must not churn the push.
            slam.sync_generation();
            // Only surface a GlobalPose once a real device pose has been adopted; the
            // frame path otherwise carries the identity seed pose.
            if slam.has_pose {
                slam.latest_global = Some(gp);
            }
        }
    }

    /// Adopt a robot's latest world pose from `robot_pose` telemetry (option B: the
    /// device's own odometry, trusted, no ICP). Updates the GlobalPose + the pose the
    /// next frame-fold re-applies. Ignores a non-finite / wrong-shape pose.
    pub fn on_pose(&self, robot_id: &str, position: &[f64], quat_xyzw: &[f64], timestamp_us: i64) {
        if position.len() != 3 || quat_xyzw.len() != 4 {
            return;
        }
        if position.iter().any(|v| !v.is_finite()) || quat_xyzw.iter().any(|v| !v.is_finite()) {
            return;
        }
        // A zero quaternion cannot be normalized to a rotation — reject it.
        if quat_xyzw.iter().all(|&v| v == 0.0) {
            return;
        }
        let pose = Pose::from_parts(
            [position[0], position[1], position[2]],
            [quat_xyzw[0], quat_xyzw[1], quat_xyzw[2], quat_xyzw[3]],
        );

        let entry = self
            .robots
            .entry(robot_id.to_string())
            .or_insert_with(|| Mutex::new(self.make_service(robot_id, DEFAULT_RESOLUTION_M)));
        let mut slam = entry.lock();
        let gp = slam.service.adopt_pose(pose, timestamp_us);
        slam.last_pose = pose;
        slam.has_pose = true;
        slam.latest_global = Some(gp);
    }

    /// Snapshot the robot's shared scene map for streaming/rendering. `None` if the
    /// robot has never produced a frame or pose.
    pub fn snapshot(&self, robot_id: &str) -> Option<SceneMapSnapshot> {
        let entry = self.robots.get(robot_id)?;
        let slam = entry.lock();
        Some(SceneMapSnapshot {
            resolution: slam.resolution,
            points: slam.service.scene_voxel_map().to_packed_xyz(),
            pose: slam.latest_global,
            last_frame_us: slam.last_frame_us,
            generation: slam.generation,
        })
    }

    /// Latest GlobalPose for a robot, if a `robot_pose` has been adopted.
    pub fn latest_pose(&self, robot_id: &str) -> Option<GlobalPoseFrame> {
        let entry = self.robots.get(robot_id)?;
        let slam = entry.lock();
        slam.latest_global
    }

    /// The robot's latest pose in the SCENE frame (metric, never WGS84) — the frame
    /// the voxel map and `on_lidar_frame` points live in. Unlike [`Self::latest_pose`]
    /// (which reports WGS84 once geo-anchored), this is always scene-local, so the
    /// depth consumer can transform camera points into the map frame regardless of
    /// anchoring. `None` until a real pose has been adopted.
    pub fn latest_scene_pose(&self, robot_id: &str) -> Option<Pose> {
        let entry = self.robots.get(robot_id)?;
        let slam = entry.lock();
        if slam.has_pose {
            Some(slam.last_pose)
        } else {
            None
        }
    }

    /// Number of occupied cells currently retained for a robot (0 if unknown).
    pub fn cell_count(&self, robot_id: &str) -> usize {
        self.robots
            .get(robot_id)
            .map(|e| e.lock().service.scene_voxel_map().len())
            .unwrap_or(0)
    }

    /// Clear a robot's accumulated map (operator "clear map" / relocation). Keeps the
    /// pose so the marker stays placed.
    pub fn clear_map(&self, robot_id: &str) {
        if let Some(entry) = self.robots.get(robot_id) {
            let mut slam = entry.lock();
            slam.service.clear_voxel_map();
            slam.sync_generation();
        }
    }

    /// Pin (or replace) a robot's geo anchor: poses for that robot become real-world
    /// WGS84. Stores the durable intent AND applies to the live service if it exists.
    /// Rejects an invalid anchor (out-of-range / non-finite) → returns false, unchanged.
    /// Persisting to settings is the caller's job (the handler has the DB pool).
    pub fn set_geo_anchor(&self, robot_id: &str, anchor: GeoAnchor) -> bool {
        if !anchor.is_valid() {
            return false;
        }
        self.anchors.insert(robot_id.to_string(), anchor);
        if let Some(e) = self.robots.get(robot_id) {
            let mut slam = e.lock();
            slam.service.set_geo_anchor(anchor);
            slam.refresh_global_pose();
        }
        true
    }

    /// Remove a robot's geo anchor; its poses revert to scene-local metres.
    pub fn clear_geo_anchor(&self, robot_id: &str) {
        self.anchors.remove(robot_id);
        if let Some(e) = self.robots.get(robot_id) {
            let mut slam = e.lock();
            slam.service.clear_geo_anchor();
            slam.refresh_global_pose();
        }
    }

    /// The robot's current geo anchor, if pinned.
    pub fn geo_anchor(&self, robot_id: &str) -> Option<GeoAnchor> {
        self.anchors.get(robot_id).map(|a| *a)
    }

    /// All pinned anchors `(robot_id, anchor)` — the set the persistence layer writes.
    pub fn all_anchors(&self) -> Vec<(String, GeoAnchor)> {
        self.anchors.iter().map(|e| (e.key().clone(), *e.value())).collect()
    }

    /// Latest map change-generation for a robot (0 if unknown). Lets the scene push
    /// source skip re-broadcasting an unchanged map.
    pub fn generation(&self, robot_id: &str) -> u64 {
        self.robots
            .get(robot_id)
            .map(|e| e.lock().generation)
            .unwrap_or(0)
    }

    /// Drop all state for a robot (uninstall / no instances left).
    pub fn remove(&self, robot_id: &str) {
        self.robots.remove(robot_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tentaflow_sdk_spec::{
        LidarFrameHeader, LIDAR_FRAME_VERSION, LIDAR_HEADER_LEN, LIDAR_LAYOUT_XYZ,
    };

    // Build a minimal canonical f32-XYZ frame (no LZ4) with the given world points.
    fn frame(points: &[[f32; 3]], resolution: f32, seq: u32) -> Vec<u8> {
        let header = LidarFrameHeader {
            version: LIDAR_FRAME_VERSION,
            layout: LIDAR_LAYOUT_XYZ,
            flags: 0,
            point_count: points.len() as u32,
            frame_seq: seq,
            timestamp_us: 1_000 * seq as i64,
            host_send_us: 0,
            resolution,
            origin: [0.0, 0.0, 0.0],
        };
        let mut out = header.encode_header().to_vec();
        assert_eq!(out.len(), LIDAR_HEADER_LEN);
        for p in points {
            out.extend_from_slice(&p[0].to_le_bytes());
            out.extend_from_slice(&p[1].to_le_bytes());
            out.extend_from_slice(&p[2].to_le_bytes());
        }
        out
    }

    #[test]
    fn frames_accumulate_and_dedup_server_side() {
        let mgr = SlamSceneManager::new();
        let id = "go2-test-a";
        mgr.on_lidar_frame(id, &frame(&[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]], 0.05, 1));
        assert_eq!(mgr.cell_count(id), 2);
        // Re-send same cells + one new → dedup, total 3.
        mgr.on_lidar_frame(
            id,
            &frame(&[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]], 0.05, 2),
        );
        assert_eq!(mgr.cell_count(id), 3);
        let snap = mgr.snapshot(id).unwrap();
        assert_eq!(snap.points.len(), 3 * 3);
        assert!((snap.resolution - 0.05).abs() < 1e-6);
        // No pose adopted yet → no GlobalPose surfaced.
        assert!(snap.pose.is_none());
    }

    #[test]
    fn pose_surfaces_global_pose_and_places_marker() {
        let mgr = SlamSceneManager::new();
        let id = "go2-test-b";
        mgr.on_lidar_frame(id, &frame(&[[0.0, 0.0, 0.0]], 0.05, 1));
        mgr.on_pose(id, &[0.6, 0.2, 0.31], &[0.0, 0.0, 0.0, 1.0], 5000);
        let gp = mgr.latest_pose(id).unwrap();
        assert_eq!(gp.position, [0.6, 0.2, 0.31]);
        let snap = mgr.snapshot(id).unwrap();
        assert!(snap.pose.is_some());
    }

    #[test]
    fn malformed_frame_and_bad_pose_are_dropped() {
        let mgr = SlamSceneManager::new();
        let id = "go2-test-c";
        mgr.on_lidar_frame(id, b"not a frame");
        assert_eq!(mgr.cell_count(id), 0);
        // wrong-shape + non-finite + zero-quat poses are all ignored
        mgr.on_pose(id, &[1.0, 2.0], &[0.0, 0.0, 0.0, 1.0], 1);
        mgr.on_pose(id, &[f64::NAN, 0.0, 0.0], &[0.0, 0.0, 0.0, 1.0], 1);
        mgr.on_pose(id, &[0.0, 0.0, 0.0], &[0.0, 0.0, 0.0, 0.0], 1);
        assert!(mgr.latest_pose(id).is_none());
    }

    #[test]
    fn pose_first_then_frame_upgrades_to_sensor_grid() {
        let mgr = SlamSceneManager::new();
        let id = "lidar-test-e";
        // Pose arrives before any frame → service seeded at the 0.05 default.
        mgr.on_pose(id, &[0.0, 0.0, 0.0], &[0.0, 0.0, 0.0, 1.0], 1);
        assert!((mgr.snapshot(id).unwrap().resolution - DEFAULT_RESOLUTION_M).abs() < 1e-6);
        // First frame declares a COARSER 0.10 grid → service rebuilt on it (no
        // geometry lost; the map was empty), and accumulation uses the new grid.
        mgr.on_lidar_frame(id, &frame(&[[0.0, 0.0, 0.0], [0.04, 0.0, 0.0]], 0.10, 1));
        let snap = mgr.snapshot(id).unwrap();
        assert!((snap.resolution - 0.10).abs() < 1e-6, "grid upgraded to sensor resolution");
        // [0,0,0] and [0.04,0,0] share one cell on the 0.10 grid (both round to 0) but
        // would be two cells on the stale 0.05 grid — so a single cell proves the upgrade.
        assert_eq!(mgr.cell_count(id), 1, "dedup uses the upgraded 0.10 grid");
        // The pose survived the rebuild.
        assert!(snap.pose.is_some());
    }

    #[test]
    fn geo_anchor_pin_apply_and_clear() {
        use tentaflow_sdk_spec::{POSE_STATE_GLOBAL, POSE_STATE_SCENE_LOCAL};
        let mgr = SlamSceneManager::new();
        let id = "go2-geo-test";
        // Pin BEFORE the robot streams: durable intent applied on service creation.
        assert!(mgr.set_geo_anchor(id, GeoAnchor::new(52.0, 21.0, 100.0, 0.0)));
        assert!(mgr.geo_anchor(id).is_some());
        mgr.on_lidar_frame(id, &frame(&[[0.0, 0.0, 0.0]], 0.05, 1));
        mgr.on_pose(id, &[1.0, 0.0, 0.0], &[0.0, 0.0, 0.0, 1.0], 5);
        let gp = mgr.latest_pose(id).unwrap();
        assert_eq!(gp.state, POSE_STATE_GLOBAL, "anchored robot reports WGS84");
        assert!((gp.position[0] - 52.0).abs() < 0.01 && gp.position[0] > 52.0);
        // Invalid anchor is rejected, leaving the current one intact.
        assert!(!mgr.set_geo_anchor(id, GeoAnchor::new(999.0, 0.0, 0.0, 0.0)));
        assert!(mgr.geo_anchor(id).is_some());
        // Clear → the cached pose flips to scene-local IMMEDIATELY (no new telemetry
        // needed), so a stale Global pose can never linger.
        mgr.clear_geo_anchor(id);
        assert!(mgr.geo_anchor(id).is_none());
        assert_eq!(
            mgr.latest_pose(id).unwrap().state,
            POSE_STATE_SCENE_LOCAL,
            "clearing the anchor refreshes the cached pose at once"
        );
        mgr.remove(id);
    }

    #[test]
    fn remove_drops_state() {
        let mgr = SlamSceneManager::new();
        let id = "go2-test-d";
        mgr.on_lidar_frame(id, &frame(&[[0.0, 0.0, 0.0]], 0.05, 1));
        assert_eq!(mgr.cell_count(id), 1);
        mgr.remove(id);
        assert_eq!(mgr.cell_count(id), 0);
        assert!(mgr.snapshot(id).is_none());
    }
}
