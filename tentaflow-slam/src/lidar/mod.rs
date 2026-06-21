// =============================================================================
// File: lidar/mod.rs — LiDAR-inertial odometry (phase-0 chunk 0b).
// The fast-path tracker: voxel downsample + voxel-hash local map + robust
// point-to-point ICP, frame-to-map. See docs/UNIFIED_SLAM_ARCHITECTURE.md §3/§4.
// =============================================================================

pub mod icp;
pub mod tracker;
pub mod voxel;

pub use icp::{register, IcpConfig, IcpResult};
pub use tracker::{LioConfig, LioTracker, TrackResult};
pub use voxel::{voxel_downsample, VoxelMap};
