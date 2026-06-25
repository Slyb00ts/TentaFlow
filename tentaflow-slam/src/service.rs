// =============================================================================
// File: service.rs — the core-facing SLAM service (chunk 0e).
// Purpose: one object that core wires per robot/scene: feed it canonical LiDAR
// frame bytes (exactly what the Go2 addon publishes), get back a canonical
// `GlobalPoseFrame`. Internally it decodes (frame.rs) → tracks/maps
// (MappingFrontend) → emits the pose with an honest state/covariance. Until a
// georeference exists the state is `SceneLocal` (metric scene metres); a rejected
// scan yields `Lost` with inflated covariance — never a confident wrong pose (§5b).
// =============================================================================

use nalgebra::Point3;
use tentaflow_sdk_spec::{
    GlobalPoseFrame, GLOBAL_POSE_VERSION, POSE_SRC_IMU, POSE_SRC_LIDAR, POSE_SRC_ODOM,
    POSE_STATE_GLOBAL, POSE_STATE_LOST, POSE_STATE_SCENE_LOCAL,
};

use crate::frame::decode_lidar_frame;
use crate::geo::GeoAnchor;
use crate::lidar::{IcpResult, LioConfig, TrackResult};
use crate::mapping::{MappingFrontend, SealPolicy};
use crate::pose::Pose;
use crate::voxel_map::SceneVoxelMap;

/// Default occupied-cell cap for the pre-fused world map, mirroring the browser
/// viewer's accumulation budget (400k instanced cells render comfortably).
pub const DEFAULT_SCENE_VOXEL_CAP: usize = 400_000;

/// Per-robot/scene SLAM driver. Owns the mapping front-end and produces poses.
#[derive(Debug, Clone)]
pub struct SlamService {
    fe: MappingFrontend,
    scene_id: u64,
    last_timestamp_us: i64,
    /// Accumulated world-frame occupancy for PRE-FUSED sources (Go2 option B). Empty
    /// for raw-scan SLAM, whose geometry lives in the front-end's frozen submaps.
    voxel_map: SceneVoxelMap,
    /// Manual georeference: when set (operator pinned the scene origin to a real-world
    /// lat/lon/alt + heading), emitted poses become `Global` (WGS84). `None` →
    /// `SceneLocal` (metric scene metres).
    geo_anchor: Option<GeoAnchor>,
}

impl SlamService {
    pub fn new(
        origin_node: u64,
        scene_id: u64,
        lio: LioConfig,
        submap_voxel: f32,
        seal: SealPolicy,
    ) -> Self {
        SlamService {
            fe: MappingFrontend::new(origin_node, scene_id, lio, submap_voxel, seal),
            scene_id,
            last_timestamp_us: 0,
            // Pre-fused sources dedup on their own grid; `submap_voxel` is that grid
            // resolution for a Go2-style service (core constructs it at the frame's
            // 0.05 m). Raw-scan services never touch this map.
            voxel_map: SceneVoxelMap::new(submap_voxel.max(1.0e-3), DEFAULT_SCENE_VOXEL_CAP),
            geo_anchor: None,
        }
    }

    /// Pin the scene to a real-world position so emitted poses become `Global` (WGS84).
    /// A malformed anchor (out-of-range / non-finite) is rejected and ignored, leaving
    /// the service scene-local — never map every pose to nonsense. Returns whether it
    /// was accepted.
    pub fn set_geo_anchor(&mut self, anchor: GeoAnchor) -> bool {
        if !anchor.is_valid() {
            return false;
        }
        self.geo_anchor = Some(anchor);
        true
    }

    /// The current georeference anchor, if pinned.
    pub fn geo_anchor(&self) -> Option<GeoAnchor> {
        self.geo_anchor
    }

    /// Remove the georeference; emitted poses revert to `SceneLocal`.
    pub fn clear_geo_anchor(&mut self) {
        self.geo_anchor = None;
    }

    pub fn current_pose(&self) -> Pose {
        self.fe.current_pose()
    }

    pub fn scene(&self) -> &crate::graph::Scene {
        self.fe.scene()
    }

    /// Ingest one canonical LiDAR frame (+ optional IMU-predicted pose prior).
    /// Returns the resulting global pose, or `None` if the frame failed to decode.
    pub fn ingest_lidar_frame(
        &mut self,
        bytes: &[u8],
        prior: Option<Pose>,
    ) -> Option<GlobalPoseFrame> {
        let decoded = decode_lidar_frame(bytes)?;
        self.last_timestamp_us = decoded.timestamp_us;
        let step = self.fe.process_scan(&decoded.points, prior);
        Some(self.pose_frame(&step.track, lidar_source(prior.is_some())))
    }

    /// OPTION B (UNIFIED_SLAM_ARCHITECTURE §15): adopt a trusted EXTERNAL device pose
    /// (the Go2's own `sportmodestate` odometry) and emit it as a `GlobalPose`,
    /// bypassing ICP. For a device that self-localizes against its own fused map we
    /// trust its pose; its map is streamed to the renderer separately, so no geometry
    /// is accumulated here (cross-robot fusion of such maps is a later phase).
    pub fn adopt_pose(&mut self, pose: Pose, timestamp_us: i64) -> GlobalPoseFrame {
        self.last_timestamp_us = timestamp_us;
        let step = self.fe.ingest_posed(pose);
        // The pose is the device's own odometry/fused estimate — NOT LiDAR/ICP.
        self.pose_frame(&step.track, POSE_SRC_ODOM)
    }

    /// The accumulated world-frame occupancy map for a pre-fused source. Empty
    /// unless `ingest_world_voxels` has been fed (Go2 option B).
    pub fn scene_voxel_map(&self) -> &SceneVoxelMap {
        &self.voxel_map
    }

    /// Reset the accumulated world map (e.g. operator "clear map" / robot relocated
    /// to a new scene origin). Does not touch the pose graph.
    pub fn clear_voxel_map(&mut self) {
        self.voxel_map.clear();
    }

    /// PRE-FUSED ingest (Go2 option B, the shared-map path): adopt the device's own
    /// trusted world pose (no ICP) AND mirror its world-frame occupancy voxels into the
    /// shared scene map. The device re-sends its WHOLE current map every frame, so the
    /// scene map is REPLACED with exactly this frame's cells (dedup per cell) rather than
    /// accumulated — geometry the device dropped (a moved/removed object) disappears here
    /// too, while a re-sent identical frame leaves the set (and `revision`) untouched.
    /// `placement` maps the device's odom frame into the shared scene frame — `None` for
    /// a single robot whose odom frame IS the scene frame; the robot→scene alignment for
    /// cross-robot fusion otherwise. Returns the device-odometry `GlobalPose`.
    pub fn ingest_world_voxels(
        &mut self,
        pose: Pose,
        world_points: &[Point3<f32>],
        placement: Option<&Pose>,
        timestamp_us: i64,
    ) -> GlobalPoseFrame {
        self.last_timestamp_us = timestamp_us;
        match placement {
            Some(t) => {
                self.voxel_map.replace_points_via(world_points, t);
                let step = self.fe.ingest_posed(t.compose(&pose));
                self.pose_frame(&step.track, POSE_SRC_ODOM)
            }
            None => {
                self.voxel_map.replace_world_points(world_points);
                let step = self.fe.ingest_posed(pose);
                self.pose_frame(&step.track, POSE_SRC_ODOM)
            }
        }
    }

    /// Pre-fused ingest straight from canonical frame BYTES (the Go2 hub frame):
    /// decode → fold world voxels + adopt `pose` (option B). `None` if the frame is
    /// malformed (same fail-closed contract as [`decode_lidar_frame`]). Convenience
    /// over `decode_lidar_frame` + [`Self::ingest_world_voxels`] so core decodes once
    /// through the tested path.
    pub fn ingest_world_frame(
        &mut self,
        bytes: &[u8],
        pose: Pose,
        placement: Option<&Pose>,
    ) -> Option<GlobalPoseFrame> {
        let decoded = decode_lidar_frame(bytes)?;
        Some(self.ingest_world_voxels(pose, &decoded.points, placement, decoded.timestamp_us))
    }

    /// Ingest already-decoded points (e.g. core read them from the hub directly).
    pub fn ingest_points(
        &mut self,
        points: &[Point3<f32>],
        timestamp_us: i64,
        prior: Option<Pose>,
    ) -> GlobalPoseFrame {
        self.last_timestamp_us = timestamp_us;
        let step = self.fe.process_scan(points, prior);
        self.pose_frame(&step.track, lidar_source(prior.is_some()))
    }

    /// Source bits for a LiDAR-tracked pose: LiDAR always, plus IMU when an IMU
    /// prior fed the prediction.
    fn pose_frame(&self, track: &TrackResult, source: u8) -> GlobalPoseFrame {
        let pose = self.fe.current_pose();
        // Honest uncertainty: a rejected scan → Lost with huge covariance; an accepted
        // track → SceneLocal (metric, not georeferenced) with covariance DERIVED from
        // ICP quality (residual + inlier support), so a weak-but-accepted track is not
        // reported as confidently as a strong one. The seed frame (no ICP) gets a
        // modest default. Still a coarse model until the optimizer's true marginals
        // are wired through, but it varies with evidence rather than a constant.
        let (mut state, cov_diag) = if !track.ok {
            (POSE_STATE_LOST, [1.0e6_f32; 6])
        } else {
            let (tvar, rvar) = match &track.icp {
                Some(r) => cov_from_icp(r),
                None => (4.0e-4, 2.0e-4), // seed/anchor frame
            };
            (POSE_STATE_SCENE_LOCAL, [tvar, tvar, tvar, rvar, rvar, rvar])
        };
        // Default: scene-local metres. With a georeference anchor AND a usable track,
        // convert to real-world WGS84 and report `Global`. A `Lost` pose stays Lost —
        // we never dress an unobservable estimate up as a confident global fix.
        let position = match self.geo_anchor {
            Some(anchor) if state == POSE_STATE_SCENE_LOCAL => {
                let (lat, lon, alt) = anchor.scene_to_wgs84(pose.translation());
                state = POSE_STATE_GLOBAL;
                [lat, lon, alt]
            }
            _ => pose.translation(),
        };
        GlobalPoseFrame {
            version: GLOBAL_POSE_VERSION,
            state,
            source,
            timestamp_us: self.last_timestamp_us,
            scene_id: self.scene_id,
            position,
            quat_xyzw: pose.quat_xyzw(),
            cov_diag,
        }
    }
}

/// Source bitmask for a LiDAR-tracked pose: LiDAR always, plus IMU when an IMU
/// prior contributed the motion prediction.
fn lidar_source(had_imu_prior: bool) -> u8 {
    let mut s = POSE_SRC_LIDAR;
    if had_imu_prior {
        s |= POSE_SRC_IMU;
    }
    s
}

/// Translational + rotational variance (m², rad²) from ICP quality: more residual
/// → looser, more inliers → tighter. Monotonic and floored/capped so a strong track
/// reports ~cm and a weak (high-residual / low-inlier) accepted track reports a
/// visibly larger, honest uncertainty.
fn cov_from_icp(r: &IcpResult) -> (f32, f32) {
    let resid = (r.mean_residual as f32).max(0.02); // floor at 2 cm
    let inlier_penalty = (200.0 / r.inliers.max(1) as f32).clamp(1.0, 1.0e4);
    let tvar = (resid * resid * inlier_penalty).clamp(4.0e-4, 1.0e4);
    (tvar, tvar * 0.5)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pose::{translation_pose, Pose};
    use nalgebra::Point3;

    fn svc() -> SlamService {
        // 0.05 m grid (Go2), default seal/lio — only the option-B path is exercised.
        SlamService::new(1, 7, LioConfig::default(), 0.05, SealPolicy::default())
    }

    fn pts(raw: &[[f32; 3]]) -> Vec<Point3<f32>> {
        raw.iter().map(|p| Point3::new(p[0], p[1], p[2])).collect()
    }

    #[test]
    fn ingest_world_voxels_mirrors_latest_frame() {
        let mut s = svc();
        let frame_a = pts(&[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]]);
        let f = s.ingest_world_voxels(translation_pose([0.5, 0.0, 0.31]), &frame_a, None, 1000);
        assert_eq!(s.scene_voxel_map().len(), 2);
        // The pose is the device's own odometry (option B), not LiDAR/ICP.
        assert_eq!(f.source, POSE_SRC_ODOM);
        assert_eq!(f.position, [0.5, 0.0, 0.31]);

        // A pre-fused source re-sends its WHOLE map each frame; this one keeps both cells
        // and adds one → the map mirrors it at 3 (dedup per cell).
        let frame_b = pts(&[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]]);
        s.ingest_world_voxels(translation_pose([0.6, 0.0, 0.31]), &frame_b, None, 2000);
        assert_eq!(s.scene_voxel_map().len(), 3, "map mirrors the frame's cells");

        // Next frame DROPS the first two cells (object moved away) → they disappear; the
        // map equals exactly the latest frame, never an accumulation of stale geometry.
        let frame_c = pts(&[[2.0, 0.0, 0.0]]);
        s.ingest_world_voxels(translation_pose([0.6, 0.0, 0.31]), &frame_c, None, 3000);
        assert_eq!(s.scene_voxel_map().len(), 1, "dropped cells disappear, no stale buildup");
    }

    #[test]
    fn placement_transform_shifts_into_shared_scene_frame() {
        let mut s = svc();
        // Robot B's odom origin is offset +10 m X from the shared scene frame.
        let placement = translation_pose([10.0, 0.0, 0.0]);
        let f = s.ingest_world_voxels(
            Pose::identity(),
            &pts(&[[0.0, 0.0, 0.0]]),
            Some(&placement),
            500,
        );
        let c = s.scene_voxel_map().iter_world().next().unwrap();
        assert!((c[0] - 10.0).abs() < 1e-3, "voxel placed into scene frame");
        assert!((f.position[0] - 10.0).abs() < 1e-3, "pose placed into scene frame");
    }

    #[test]
    fn geo_anchor_makes_pose_global_wgs84() {
        use crate::geo::GeoAnchor;
        let mut s = svc();
        // Scene-local first: a pose adopt reports SceneLocal scene metres.
        let local = s.adopt_pose(translation_pose([1.0, 2.0, 0.31]), 100);
        assert_eq!(local.state, POSE_STATE_SCENE_LOCAL);
        assert_eq!(local.position, [1.0, 2.0, 0.31]);

        // Pin the scene origin to Warsaw, heading 0 (scene +X = North).
        assert!(s.set_geo_anchor(GeoAnchor::new(52.2297, 21.0122, 118.5, 0.0)));
        let global = s.adopt_pose(translation_pose([1.0, 2.0, 0.31]), 200);
        assert_eq!(global.state, POSE_STATE_GLOBAL, "anchored → Global");
        // Position is now WGS84 near the anchor (a few metres away).
        assert!((global.position[0] - 52.2297).abs() < 1e-3, "lat near anchor");
        assert!((global.position[1] - 21.0122).abs() < 1e-3, "lon near anchor");
        // +X (1 m North) nudges latitude up from the anchor.
        assert!(global.position[0] > 52.2297, "scene +X moved north");

        // Clearing reverts to scene-local metres.
        s.clear_geo_anchor();
        let again = s.adopt_pose(translation_pose([1.0, 2.0, 0.31]), 300);
        assert_eq!(again.state, POSE_STATE_SCENE_LOCAL);
        assert_eq!(again.position, [1.0, 2.0, 0.31]);
    }

    #[test]
    fn invalid_geo_anchor_is_rejected() {
        use crate::geo::GeoAnchor;
        let mut s = svc();
        assert!(!s.set_geo_anchor(GeoAnchor::new(999.0, 21.0, 0.0, 0.0)));
        assert!(s.geo_anchor().is_none());
        let f = s.adopt_pose(translation_pose([1.0, 0.0, 0.0]), 1);
        assert_eq!(f.state, POSE_STATE_SCENE_LOCAL, "rejected anchor → still scene-local");
    }

    #[test]
    fn clear_voxel_map_empties_geometry_only() {
        let mut s = svc();
        s.ingest_world_voxels(Pose::identity(), &pts(&[[0.0, 0.0, 0.0]]), None, 1);
        assert_eq!(s.scene_voxel_map().len(), 1);
        s.clear_voxel_map();
        assert!(s.scene_voxel_map().is_empty());
    }
}
