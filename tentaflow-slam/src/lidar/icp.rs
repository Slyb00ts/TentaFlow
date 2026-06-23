// =============================================================================
// File: lidar/icp.rs — robust point-to-point ICP on SE(3) (KISS-ICP style).
// Purpose: align a downsampled scan to the local VoxelMap. Point-to-point (no
// surface normals needed) with a Huber robust weight and a max-correspondence
// gate. Gauss-Newton on the se(3) tangent with a LEFT perturbation:
//   T' = exp(δ) · T,   δ = [ρ (translation); φ (rotation)]
// Per-correspondence residual r = (T·s) − t_nn, Jacobian J = [ I₃ | −[ (T·s) ]_× ].
// =============================================================================

use nalgebra::{
    Isometry3, Matrix3, Matrix3x6, Matrix6, Point3, Translation3, UnitQuaternion, Vector3, Vector6,
};

use crate::lidar::voxel::VoxelMap;
use crate::pose::Pose;

/// ICP tuning. `max_corr_dist` doubles as the Huber transition by default; keep
/// `max_corr_dist <= map.voxel_size * k` is NOT required (the map search widens to
/// stay complete), but smaller distances reject more outliers.
#[derive(Debug, Clone, Copy)]
pub struct IcpConfig {
    pub max_iters: usize,
    pub max_corr_dist: f64,
    pub huber_delta: f64,
    /// Stop when the increment's norm drops below this (metres+radians mixed).
    pub convergence_eps: f64,
    /// Minimum correspondences to attempt a solve (under this → give up, not crash).
    pub min_correspondences: usize,
}

impl Default for IcpConfig {
    fn default() -> Self {
        IcpConfig {
            max_iters: 30,
            max_corr_dist: 1.0,
            huber_delta: 0.3,
            convergence_eps: 1e-5,
            min_correspondences: 10,
        }
    }
}

/// Outcome of a registration.
#[derive(Debug, Clone, Copy)]
pub struct IcpResult {
    pub pose: Pose,
    pub iterations: usize,
    pub inliers: usize,
    pub converged: bool,
    pub mean_residual: f64,
}

/// 3×3 skew-symmetric matrix of `v` (so `skew(v)·w = v × w`).
#[inline]
fn skew(v: Vector3<f64>) -> Matrix3<f64> {
    Matrix3::new(0.0, -v.z, v.y, v.z, 0.0, -v.x, -v.y, v.x, 0.0)
}

/// Register `source` (scan points, sensor frame) to `map` starting from `init`
/// (the source→map pose guess). Returns the refined pose.
pub fn register(
    source: &[Point3<f32>],
    map: &VoxelMap,
    init: Pose,
    cfg: &IcpConfig,
) -> IcpResult {
    let mut t: Isometry3<f64> = init.0;
    let mut converged = false;
    let mut last_inliers = 0;
    let mut last_mean = f64::INFINITY;
    let mut iter_done = 0;

    for iter in 0..cfg.max_iters {
        iter_done = iter + 1;
        let mut h = Matrix6::<f64>::zeros(); // Σ w·JᵀJ
        let mut g = Vector6::<f64>::zeros(); // Σ w·Jᵀr
        let mut inliers = 0usize;
        let mut res_sum = 0.0;

        for sp in source {
            let s = Point3::new(sp.x as f64, sp.y as f64, sp.z as f64);
            let world = t * s;
            let Some(nn) = map.nearest(&world, cfg.max_corr_dist) else {
                continue;
            };
            let r: Vector3<f64> = world.coords - nn.coords;
            let n = r.norm();
            // Huber weight: full weight inside delta, downweighted ∝ 1/‖r‖ outside.
            let w = if n <= cfg.huber_delta { 1.0 } else { cfg.huber_delta / n };

            let mut j = Matrix3x6::<f64>::zeros();
            j.fixed_view_mut::<3, 3>(0, 0).copy_from(&Matrix3::identity());
            j.fixed_view_mut::<3, 3>(0, 3).copy_from(&(-skew(world.coords)));

            let jt = j.transpose();
            h += w * jt * j;
            g += w * jt * r;
            inliers += 1;
            res_sum += n;
        }

        last_inliers = inliers;
        last_mean = if inliers > 0 { res_sum / inliers as f64 } else { f64::INFINITY };
        if inliers < cfg.min_correspondences {
            break;
        }

        // Solve H·δ = −g for the increment. Levenberg damping (tiny) keeps H SPD
        // even on near-degenerate geometry so Cholesky succeeds.
        let damped = h + Matrix6::identity() * 1e-9;
        let Some(chol) = damped.cholesky() else {
            break;
        };
        let dx: Vector6<f64> = chol.solve(&(-g));

        let rho = Vector3::new(dx[0], dx[1], dx[2]);
        let phi = Vector3::new(dx[3], dx[4], dx[5]);
        let delta = Isometry3::from_parts(
            Translation3::from(rho),
            UnitQuaternion::from_scaled_axis(phi),
        );
        t = delta * t;

        if dx.norm() < cfg.convergence_eps {
            converged = true;
            break;
        }
    }

    IcpResult {
        pose: Pose(t),
        iterations: iter_done,
        inliers: last_inliers,
        converged,
        mean_residual: last_mean,
    }
}
