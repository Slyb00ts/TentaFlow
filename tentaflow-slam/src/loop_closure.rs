// =============================================================================
// File: loop_closure.rs — loop-closure verification + gating (phase-0 chunk 0d).
// Purpose: turn a CANDIDATE submap pair into a gated `LoopClosure` constraint per
// the §5 correctness contract: (1) geometric verification by ICP with an inlier
// ratio, (2) a χ² consistency gate against the current graph estimate. A pair that
// passes becomes a `Confirmed` constraint; otherwise it is rejected (never fused).
// Candidate SELECTION (which pairs to test — VPR / proximity) is a later chunk; here
// we verify+gate a proposed pair.
// =============================================================================

use nalgebra::{Matrix6, Point3};

use crate::graph::{
    Constraint, ConstraintId, ConstraintKind, ConstraintSource, ConstraintStatus, Scene,
};
use crate::lidar::icp::{register, IcpConfig};
use crate::lidar::voxel::VoxelMap;
use crate::submap::SubmapId;

/// Gating thresholds for accepting a loop closure (§5).
#[derive(Debug, Clone, Copy)]
pub struct LoopGate {
    /// Minimum fraction of `from` points that found an inlier correspondence in `to`.
    pub min_inlier_ratio: f64,
    /// Maximum Mahalanobis distance between the ICP relative and the current graph
    /// estimate (χ² consistency). A large value here means "trust ICP even if the
    /// current (possibly drifted) estimate disagrees a lot" — set per use.
    pub max_mahalanobis: f64,
    pub icp: IcpConfig,
    /// Map voxel size for the `to`-submap NN structure used during verification.
    pub map_voxel_size: f64,
    pub max_points_per_voxel: usize,
}

impl Default for LoopGate {
    fn default() -> Self {
        LoopGate {
            min_inlier_ratio: 0.6,
            max_mahalanobis: 8.0,
            icp: IcpConfig { max_corr_dist: 0.5, ..IcpConfig::default() },
            map_voxel_size: 0.5,
            max_points_per_voxel: 50,
        }
    }
}

/// Outcome of verifying a candidate pair.
#[derive(Debug, Clone)]
pub struct LoopVerification {
    /// The gated constraint (status `Confirmed` if accepted, `Rejected` if not).
    pub constraint: Constraint,
    pub accepted: bool,
    pub inlier_ratio: f64,
    pub mahalanobis: f64,
}

/// Verify a candidate loop closure between submaps `from` and `to` (both must exist
/// in the scene). Registers `from`'s geometry against `to`'s via ICP, seeded by the
/// current graph estimate, then applies the gates. `edge_id` stamps the produced
/// constraint. Returns `None` only if a submap is missing/empty.
pub fn verify_loop_closure(
    scene: &Scene,
    from: SubmapId,
    to: SubmapId,
    edge_id: ConstraintId,
    gate: &LoopGate,
) -> Option<LoopVerification> {
    let from_sm = scene.submap(from)?;
    let to_sm = scene.submap(to)?;
    if from_sm.point_count() == 0 || to_sm.point_count() == 0 {
        return None;
    }
    let from_node = scene.graph.node(from)?;
    let to_node = scene.graph.node(to)?;

    // Build a NN map from `to`'s local geometry.
    let mut to_map = VoxelMap::new(gate.map_voxel_size, gate.max_points_per_voxel);
    for p in to_sm.geometry().points() {
        to_map.add_point(p.pos);
    }

    // Source = `from`'s local points. ICP finds T (from_local → to_local).
    let source: Vec<Point3<f32>> = from_sm.geometry().points().iter().map(|p| p.pos).collect();

    // Initial guess from the current graph estimate: T_from→to = T_to⁻¹ · T_from
    // (so that applying it to a from-local point lands it in to-local).
    let est = to_node.pose.inverse().compose(&from_node.pose);
    let res = register(&source, &to_map, est, &gate.icp);

    let inlier_ratio = res.inliers as f64 / source.len().max(1) as f64;

    // χ² gate: how far the measured relative is from the current estimate, scaled by
    // the (diagonal) information we will assign. Cheap Mahalanobis on the SE(3) error.
    let info = information_from_inliers(res.inliers);
    let err = est.inverse().compose(&res.pose).log();
    let mahalanobis = (err.transpose() * info * err)[(0, 0)].sqrt();

    let accepted = res.converged
        && inlier_ratio >= gate.min_inlier_ratio
        && mahalanobis <= gate.max_mahalanobis;

    // ICP returns R_ft = to-local ← from-local = T_to⁻¹·T_from. The optimizer's
    // relative edge convention is z = T_from⁻¹·T_to (= R_ft⁻¹), so store the inverse.
    let relative = res.pose.inverse();
    let constraint = Constraint {
        id: edge_id,
        kind: ConstraintKind::LoopClosure {
            from,
            to,
            relative,
            information: info,
        },
        status: if accepted { ConstraintStatus::Confirmed } else { ConstraintStatus::Rejected },
        source: ConstraintSource::LidarLoopClosure,
    };

    Some(LoopVerification { constraint, accepted, inlier_ratio, mahalanobis })
}

/// Information (inverse covariance) for a loop edge, scaled by inlier support: more
/// inliers → tighter (more trusted) constraint. Diagonal, isotropic for now.
fn information_from_inliers(inliers: usize) -> Matrix6<f64> {
    let w = (inliers as f64).clamp(1.0, 5000.0) / 100.0;
    Matrix6::identity() * w
}
