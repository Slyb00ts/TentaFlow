// =============================================================================
// File: optimize.rs — deterministic pose-graph Gauss-Newton (phase-0 chunk 0d).
// Purpose: optimize submap POSES (never geometry — frozen submaps, §1) to satisfy
// the active constraints. Hand-rolled (not factrs) so every mesh node computes
// byte-identical poses from the same facts — the deterministic-derivation guarantee
// (§6). Left-perturbation `T' = exp(δ)·T`, residual `log(zᵀ·error)`, numerical
// Jacobians (correct + simple; analytic Lie Jacobians are a later perf win). The
// gauge node is held FIXED to remove the global gauge freedom.
// =============================================================================

use std::collections::BTreeMap;

use nalgebra::{DMatrix, DVector, Matrix6, Vector6};

use crate::graph::{ConstraintKind, ConstraintStatus, PoseGraph};
use crate::pose::Pose;
use crate::submap::SubmapId;

/// Optimizer tuning.
#[derive(Debug, Clone, Copy)]
pub struct OptConfig {
    pub max_iters: usize,
    /// Stop when the full increment's norm drops below this.
    pub convergence_eps: f64,
    /// Levenberg damping added to the diagonal (keeps H SPD / robust to weak DoF).
    pub damping: f64,
    /// Finite-difference step for numerical Jacobians.
    pub fd_eps: f64,
}

impl Default for OptConfig {
    fn default() -> Self {
        OptConfig {
            max_iters: 25,
            convergence_eps: 1e-7,
            damping: 1e-6,
            fd_eps: 1e-6,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct OptReport {
    pub iterations: usize,
    pub converged: bool,
    pub final_cost: f64,
    pub free_nodes: usize,
}

/// One relative or absolute constraint reduced to (touched nodes, residual fn).
enum Edge {
    /// Relative SE(3): residual = log(zᵀ · (Ta⁻¹·Tb)).
    Relative { a: SubmapId, b: SubmapId, z: Pose, info: Matrix6<f64> },
    /// Absolute SE(3) prior on a single node: residual = log(zᵀ · Ts).
    Absolute { s: SubmapId, z: Pose, info: Matrix6<f64> },
}

fn residual(edge: &Edge, poses: &BTreeMap<SubmapId, Pose>) -> Vector6<f64> {
    match edge {
        Edge::Relative { a, b, z, .. } => {
            let ta = poses[a];
            let tb = poses[b];
            let err = z.inverse().compose(&ta.inverse().compose(&tb));
            err.log()
        }
        Edge::Absolute { s, z, .. } => {
            let ts = poses[s];
            z.inverse().compose(&ts).log()
        }
    }
}

fn edge_info(edge: &Edge) -> Matrix6<f64> {
    match edge {
        Edge::Relative { info, .. } | Edge::Absolute { info, .. } => *info,
    }
}

fn edge_touches(edge: &Edge, node: SubmapId) -> bool {
    match edge {
        Edge::Relative { a, b, .. } => *a == node || *b == node,
        Edge::Absolute { s, .. } => *s == node,
    }
}

/// Optimize the pose graph in place. Fixes the deterministic gauge node and solves
/// for every other node's pose. No-op (Ok) when there are < 1 free nodes or no
/// constraints. Geometry is never touched.
pub fn optimize(graph: &mut PoseGraph, cfg: &OptConfig) -> OptReport {
    if graph.node_count() == 0 {
        return OptReport { iterations: 0, converged: true, final_cost: 0.0, free_nodes: 0 };
    }

    // Whether any active ABSOLUTE constraint pins the graph. A full SE(3) anchor
    // removes the global gauge freedom by itself, so we must NOT also fix a node
    // (fixing the anchored node would make the anchor unable to move it). Only a
    // pure-relative graph needs an explicit fixed gauge node.
    let has_absolute = graph
        .active_constraints()
        .any(|c| matches!(c.kind, ConstraintKind::Anchor { .. }));
    let fixed: Option<SubmapId> = if has_absolute { None } else { graph.gauge_anchor() };

    // Snapshot poses; collect free nodes in deterministic (sorted) order.
    let mut poses: BTreeMap<SubmapId, Pose> = BTreeMap::new();
    for (id, node) in graph.nodes() {
        poses.insert(*id, node.pose);
    }
    let free: Vec<SubmapId> = poses.keys().copied().filter(|id| Some(*id) != fixed).collect();
    let col: BTreeMap<SubmapId, usize> =
        free.iter().enumerate().map(|(i, id)| (*id, i)).collect();

    // Build solver edges from the ACTIVE constraints the solver understands.
    let mut edges: Vec<Edge> = Vec::new();
    for c in graph.active_constraints() {
        if c.status == ConstraintStatus::Rejected {
            continue;
        }
        match &c.kind {
            ConstraintKind::Odometry { from, to, relative, information }
            | ConstraintKind::LoopClosure { from, to, relative, information } => {
                if poses.contains_key(from) && poses.contains_key(to) {
                    edges.push(Edge::Relative { a: *from, b: *to, z: *relative, info: *information });
                }
            }
            ConstraintKind::Anchor { submap, submap_pose, information } => {
                if poses.contains_key(submap) {
                    edges.push(Edge::Absolute { s: *submap, z: *submap_pose, info: *information });
                }
            }
            // Gnss/Georef ECEF coupling is handled in the georeference chunk, not here.
            ConstraintKind::Gnss { .. } | ConstraintKind::Georef { .. } => {}
        }
    }

    let n = free.len();
    if n == 0 || edges.is_empty() {
        let cost = edges
            .iter()
            .map(|e| {
                let r = residual(e, &poses);
                (r.transpose() * edge_info(e) * r)[(0, 0)]
            })
            .sum();
        return OptReport { iterations: 0, converged: true, final_cost: cost, free_nodes: n };
    }

    let dim = 6 * n;
    let mut converged = false;
    let mut iters = 0;

    for _ in 0..cfg.max_iters {
        iters += 1;
        let mut h = DMatrix::<f64>::zeros(dim, dim);
        let mut g = DVector::<f64>::zeros(dim);

        for edge in &edges {
            let info = edge_info(edge);
            let r0 = residual(edge, &poses);

            // Numerical Jacobian columns for each FREE node this edge touches.
            // Per touched node k: J_k (6x6), perturbing T_k ← exp(ε e_i)·T_k.
            let touched: Vec<SubmapId> = free
                .iter()
                .copied()
                .filter(|id| edge_touches(edge, *id))
                .collect();

            // Compute each node's 6x6 Jacobian, then accumulate H/g blocks.
            let mut jacs: Vec<(usize, [Vector6<f64>; 6])> = Vec::new();
            for node in touched {
                let orig = poses[&node];
                let mut cols = [Vector6::zeros(); 6];
                for i in 0..6 {
                    let mut dv = Vector6::zeros();
                    dv[i] = cfg.fd_eps;
                    poses.insert(node, Pose::se3_exp(&dv).compose(&orig));
                    let rp = residual(edge, &poses);
                    cols[i] = (rp - r0) / cfg.fd_eps;
                }
                poses.insert(node, orig); // restore
                jacs.push((col[&node], cols));
            }

            // Accumulate H += JᵀΩJ and g += JᵀΩr over the touched-node block pairs.
            for (ci, jc) in &jacs {
                let jc_mat = Matrix6::from_columns(jc);
                let jt_omega = jc_mat.transpose() * info; // 6x6
                let gblk = jt_omega * r0; // 6
                for i in 0..6 {
                    g[6 * ci + i] += gblk[i];
                }
                for (cj, jd) in &jacs {
                    let jd_mat = Matrix6::from_columns(jd);
                    let block = jt_omega * jd_mat; // 6x6
                    for i in 0..6 {
                        for j in 0..6 {
                            h[(6 * ci + i, 6 * cj + j)] += block[(i, j)];
                        }
                    }
                }
            }
        }

        // Levenberg damping → solve (H + λI) δ = −g.
        for d in 0..dim {
            h[(d, d)] += cfg.damping;
        }
        let Some(chol) = h.clone().cholesky() else {
            break;
        };
        let dx = chol.solve(&(-&g));

        // Apply increments (left perturbation) to each free node.
        for (k, id) in free.iter().enumerate() {
            let mut dv = Vector6::zeros();
            for i in 0..6 {
                dv[i] = dx[6 * k + i];
            }
            let updated = Pose::se3_exp(&dv).compose(&poses[id]);
            poses.insert(*id, updated);
        }

        if dx.norm() < cfg.convergence_eps {
            converged = true;
            break;
        }
    }

    // Write optimized poses back (geometry untouched; covariance left as-is for now).
    for id in &free {
        let cov = graph.node(*id).map(|n| n.covariance).unwrap_or_else(Matrix6::identity);
        graph.set_node(*id, poses[id], cov);
    }

    let final_cost = edges
        .iter()
        .map(|e| {
            let r = residual(e, &poses);
            (r.transpose() * edge_info(e) * r)[(0, 0)]
        })
        .sum();

    OptReport { iterations: iters, converged, final_cost, free_nodes: n }
}
