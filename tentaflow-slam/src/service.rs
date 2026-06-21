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
    GlobalPoseFrame, GLOBAL_POSE_VERSION, POSE_SRC_IMU, POSE_SRC_LIDAR, POSE_STATE_LOST,
    POSE_STATE_SCENE_LOCAL,
};

use crate::frame::decode_lidar_frame;
use crate::lidar::{IcpResult, LioConfig, TrackResult};
use crate::mapping::{MappingFrontend, SealPolicy};
use crate::pose::Pose;

/// Per-robot/scene SLAM driver. Owns the mapping front-end and produces poses.
#[derive(Debug, Clone)]
pub struct SlamService {
    fe: MappingFrontend,
    scene_id: u64,
    last_timestamp_us: i64,
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
        }
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
        Some(self.pose_frame(&step.track, prior.is_some()))
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
        self.pose_frame(&step.track, prior.is_some())
    }

    fn pose_frame(&self, track: &TrackResult, had_prior: bool) -> GlobalPoseFrame {
        let pose = self.fe.current_pose();
        let mut source = POSE_SRC_LIDAR;
        if had_prior {
            source |= POSE_SRC_IMU;
        }
        // Honest uncertainty: a rejected scan → Lost with huge covariance; an accepted
        // track → SceneLocal (metric, not georeferenced) with covariance DERIVED from
        // ICP quality (residual + inlier support), so a weak-but-accepted track is not
        // reported as confidently as a strong one. The seed frame (no ICP) gets a
        // modest default. Still a coarse model until the optimizer's true marginals
        // are wired through, but it varies with evidence rather than a constant.
        let (state, cov_diag) = if !track.ok {
            (POSE_STATE_LOST, [1.0e6_f32; 6])
        } else {
            let (tvar, rvar) = match &track.icp {
                Some(r) => cov_from_icp(r),
                None => (4.0e-4, 2.0e-4), // seed/anchor frame
            };
            (POSE_STATE_SCENE_LOCAL, [tvar, tvar, tvar, rvar, rvar, rvar])
        };
        GlobalPoseFrame {
            version: GLOBAL_POSE_VERSION,
            state,
            source,
            timestamp_us: self.last_timestamp_us,
            scene_id: self.scene_id,
            position: pose.translation(),
            quat_xyzw: pose.quat_xyzw(),
            cov_diag,
        }
    }
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
