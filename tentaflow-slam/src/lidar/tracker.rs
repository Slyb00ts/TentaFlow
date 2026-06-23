// =============================================================================
// File: lidar/tracker.rs — frame-to-map LiDAR odometry (the fast-path tracker).
// Purpose: per scan: PREDICT an initial pose (IMU prior if given, else constant
// velocity), REGISTER the downsampled scan to the rolling local map via ICP, then
// FOLD the registered scan into the map. Emits the new pose + the odometry delta
// (the edge chunk 0d feeds to the pose graph). The rolling map IS the active
// submap's geometry accumulation; chunk 0c seals it into a frozen `Submap`.
// =============================================================================

use nalgebra::Point3;

use crate::lidar::icp::{register, IcpConfig, IcpResult};
use crate::lidar::voxel::{voxel_downsample, VoxelMap};
use crate::pose::Pose;

/// Tracker tuning.
#[derive(Debug, Clone, Copy)]
pub struct LioConfig {
    /// Voxel size for downsampling each incoming scan (metres).
    pub scan_voxel_size: f32,
    /// Voxel size of the rolling local map (metres).
    pub map_voxel_size: f64,
    /// Max points kept per local-map voxel.
    pub max_points_per_voxel: usize,
    pub icp: IcpConfig,
}

impl Default for LioConfig {
    fn default() -> Self {
        LioConfig {
            scan_voxel_size: 0.25,
            map_voxel_size: 0.5,
            max_points_per_voxel: 20,
            icp: IcpConfig::default(),
        }
    }
}

/// Per-scan output.
#[derive(Debug, Clone, Copy)]
pub struct TrackResult {
    /// New absolute pose (sensor→map).
    pub pose: Pose,
    /// Relative motion since the previous scan (the odometry edge measurement).
    pub delta: Pose,
    /// ICP stats (`None` for the very first scan, which only seeds the map).
    pub icp: Option<IcpResult>,
    /// `false` when registration was REJECTED (too few inliers): the pose is a
    /// dead-reckoned prediction and the scan was NOT folded into the map (so a bad
    /// frame can't corrupt it). Callers should treat the pose as degraded and the
    /// odometry edge as low-confidence (or skip it).
    pub ok: bool,
}

/// Frame-to-map LiDAR odometry. Holds the rolling local map + last pose/velocity.
#[derive(Debug, Clone)]
pub struct LioTracker {
    cfg: LioConfig,
    map: VoxelMap,
    pose: Pose,
    last_delta: Pose,
    initialized: bool,
}

impl LioTracker {
    pub fn new(cfg: LioConfig) -> Self {
        let map = VoxelMap::new(cfg.map_voxel_size, cfg.max_points_per_voxel);
        LioTracker {
            cfg,
            map,
            pose: Pose::identity(),
            last_delta: Pose::identity(),
            initialized: false,
        }
    }

    pub fn pose(&self) -> Pose {
        self.pose
    }

    pub fn map(&self) -> &VoxelMap {
        &self.map
    }

    /// Process one scan (sensor-frame points). `prior` is an optional predicted
    /// absolute pose (e.g. from IMU preintegration); when `None` the tracker uses a
    /// constant-velocity prediction. Returns the new pose + odometry delta.
    pub fn process_scan(&mut self, scan: &[Point3<f32>], prior: Option<Pose>) -> TrackResult {
        let down = voxel_downsample(scan, self.cfg.scan_voxel_size);

        // First scan (or empty map): seed the map, no registration possible yet.
        if !self.initialized || self.map.is_empty() {
            self.pose = prior.unwrap_or_else(Pose::identity);
            self.fold_into_map(&down, &self.pose.clone());
            self.last_delta = Pose::identity();
            self.initialized = true;
            return TrackResult { pose: self.pose, delta: Pose::identity(), icp: None, ok: true };
        }

        // Predict: IMU prior if supplied, else constant velocity (apply last delta).
        let guess = prior.unwrap_or_else(|| self.pose.compose(&self.last_delta));
        let result = register(&down, &self.map, guess, &self.cfg.icp);

        // Accept only if ICP found enough correspondences. A REJECTED registration
        // (lost overlap / degenerate geometry / bad prior) must NOT commit a jumped
        // pose nor fold a misaligned scan into the map — that would corrupt every
        // future match. Instead dead-reckon on the prediction and flag degraded.
        if result.inliers < self.cfg.icp.min_correspondences {
            let delta = self.pose.relative_to(&guess);
            self.pose = guess; // last_delta unchanged → constant velocity continues
            return TrackResult { pose: guess, delta, icp: Some(result), ok: false };
        }

        let new_pose = result.pose;
        let delta = self.pose.relative_to(&new_pose);
        self.pose = new_pose;
        self.last_delta = delta;

        self.fold_into_map(&down, &new_pose);
        TrackResult { pose: new_pose, delta, icp: Some(result), ok: true }
    }

    /// Transform downsampled sensor-frame points by `pose` and add them to the
    /// rolling local map (frame-to-map accumulation).
    fn fold_into_map(&mut self, down: &[Point3<f32>], pose: &Pose) {
        for p in down {
            let w = pose.transform_point([p.x as f64, p.y as f64, p.z as f64]);
            self.map.add_point(Point3::new(w[0] as f32, w[1] as f32, w[2] as f32));
        }
    }
}
