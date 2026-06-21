// =============================================================================
// File: graph.rs — pose graph + constraints + the Scene (shared SLAM state).
// Purpose: the global state is a pose graph whose NODES are submap poses and whose
// EDGES are constraints. Optimization (chunk 0d) moves node poses only; geometry
// stays frozen in the submaps (submap.rs). Constraints are an ADD-ONLY, id-keyed
// set so multi-node mesh merge is just a union (UNIFIED_SLAM_ARCHITECTURE §6/§13);
// gating lives in `status`. The gauge is deterministic so every node derives the
// same poses from the same facts.
// =============================================================================

use std::collections::BTreeMap;

use nalgebra::{Matrix3, Matrix6};

use crate::pose::Pose;
use crate::submap::{Submap, SubmapId};

/// Stable, coordination-free constraint identity (mirrors `SubmapId`): the node
/// that observed it + a per-node monotonic sequence. Lets the add-only set dedupe
/// on merge so re-delivering the same fact is a no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConstraintId {
    pub origin_node: u64,
    pub seq: u32,
}

impl ConstraintId {
    pub fn new(origin_node: u64, seq: u32) -> Self {
        ConstraintId { origin_node, seq }
    }
}

/// What produced a constraint — provenance for debugging + gating policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstraintSource {
    LidarOdometry,
    LidarLoopClosure,
    VisualLoopClosure,
    Gnss,
    Georef,
    Anchor,
    InterDevice,
}

/// Gating lifecycle (UNIFIED_SLAM_ARCHITECTURE §5). Only `Confirmed` constraints
/// enter optimization; a later-rejected one is flipped to `Rejected` and dropped
/// from the solve WITHOUT touching any geometry (frozen submaps make that safe).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstraintStatus {
    Candidate,
    Confirmed,
    Rejected,
}

/// The measurement carried by a constraint, plus its information (inverse
/// covariance). Relative edges carry a 6-DoF SE(3) measurement + 6×6 information;
/// absolute position priors (GNSS) carry a 3-DoF point + 3×3 information.
#[derive(Debug, Clone)]
pub enum ConstraintKind {
    /// Sequential odometry between consecutive submaps — always trusted (no gating).
    Odometry { from: SubmapId, to: SubmapId, relative: Pose, information: Matrix6<f64> },
    /// Loop closure / inter-submap registration — gated (may be wrong).
    LoopClosure { from: SubmapId, to: SubmapId, relative: Pose, information: Matrix6<f64> },
    /// Absolute global position prior on a submap origin, in ECEF metres (GNSS).
    Gnss { submap: SubmapId, position_ecef: [f64; 3], information: Matrix3<f64> },
    /// Georeference: fixes a submap's local frame into ECEF (submap → ECEF).
    Georef { submap: SubmapId, submap_to_ecef: Pose, information: Matrix6<f64> },
    /// Known fixed anchor / marker / UWB: absolute pose prior on a submap.
    Anchor { submap: SubmapId, submap_pose: Pose, information: Matrix6<f64> },
}

/// One immutable observation in the graph. `kind` carries the measurement,
/// `status` carries gating, `source` carries provenance.
#[derive(Debug, Clone)]
pub struct Constraint {
    pub id: ConstraintId,
    pub kind: ConstraintKind,
    pub status: ConstraintStatus,
    pub source: ConstraintSource,
}

impl Constraint {
    /// `true` if this constraint's measurement is a hard global anchor (georef /
    /// gnss / anchor) — used by the gauge to prefer a georeferenced fix.
    pub fn is_global_anchor(&self) -> bool {
        matches!(
            self.kind,
            ConstraintKind::Gnss { .. }
                | ConstraintKind::Georef { .. }
                | ConstraintKind::Anchor { .. }
        )
    }
}

/// A node in the pose graph: a submap's pose in the scene frame + its marginal
/// covariance. The optimizer overwrites `pose`/`covariance`; nothing else here
/// references geometry, so optimization never risks the frozen submaps.
#[derive(Debug, Clone, Copy)]
pub struct PoseNode {
    pub pose: Pose,
    pub covariance: Matrix6<f64>,
}

impl PoseNode {
    /// A fresh node at identity with large (uninformative) covariance — its true
    /// pose is pinned later by constraints + optimization.
    pub fn unconstrained() -> Self {
        PoseNode { pose: Pose::identity(), covariance: Matrix6::identity() * 1.0e6 }
    }
}

/// The pose graph: submap pose nodes (deterministically ordered) + an add-only,
/// id-keyed constraint set. This is the unit that replicates and is optimized.
#[derive(Debug, Clone, Default)]
pub struct PoseGraph {
    nodes: BTreeMap<SubmapId, PoseNode>,
    constraints: BTreeMap<ConstraintId, Constraint>,
}

impl PoseGraph {
    pub fn new() -> Self {
        PoseGraph { nodes: BTreeMap::new(), constraints: BTreeMap::new() }
    }

    /// Ensure a node exists for `id` (inserts an unconstrained node if absent).
    pub fn ensure_node(&mut self, id: SubmapId) {
        self.nodes.entry(id).or_insert_with(PoseNode::unconstrained);
    }

    pub fn node(&self, id: SubmapId) -> Option<&PoseNode> {
        self.nodes.get(&id)
    }

    /// Set a node's pose + covariance. This is the ONLY pose mutation in the system
    /// — called by the optimizer (chunk 0d). Geometry is never touched here.
    pub fn set_node(&mut self, id: SubmapId, pose: Pose, covariance: Matrix6<f64>) {
        self.nodes.insert(id, PoseNode { pose, covariance });
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Deterministic iteration over nodes (BTree order by SubmapId) — every node in
    /// the mesh sees the same order, so the solve is reproducible.
    pub fn nodes(&self) -> impl Iterator<Item = (&SubmapId, &PoseNode)> {
        self.nodes.iter()
    }

    /// Add a constraint. ADD-ONLY + idempotent: re-adding the same `ConstraintId`
    /// is a no-op, which is exactly what makes mesh merge a conflict-free union.
    /// Returns `true` if it was newly inserted.
    pub fn add_constraint(&mut self, c: Constraint) -> bool {
        use std::collections::btree_map::Entry;
        match self.constraints.entry(c.id) {
            Entry::Occupied(_) => false,
            Entry::Vacant(v) => {
                v.insert(c);
                true
            }
        }
    }

    /// Flip a constraint's gating status (Candidate→Confirmed→Rejected). Returns
    /// `false` if the id is unknown. Status is the ONLY mutable field of a
    /// constraint; the measurement itself is immutable.
    pub fn set_status(&mut self, id: ConstraintId, status: ConstraintStatus) -> bool {
        match self.constraints.get_mut(&id) {
            Some(c) => {
                c.status = status;
                true
            }
            None => false,
        }
    }

    pub fn constraint(&self, id: ConstraintId) -> Option<&Constraint> {
        self.constraints.get(&id)
    }

    pub fn constraint_count(&self) -> usize {
        self.constraints.len()
    }

    /// All constraints in deterministic id order.
    pub fn constraints(&self) -> impl Iterator<Item = &Constraint> {
        self.constraints.values()
    }

    /// Only the constraints the optimizer should use. Odometry is trusted by
    /// default (active even while `Candidate`), every other kind must be
    /// `Confirmed` — but an EXPLICIT `Rejected` always excludes the edge, including
    /// rejected odometry (a bad sequential edge must not keep corrupting the graph).
    pub fn active_constraints(&self) -> impl Iterator<Item = &Constraint> {
        self.constraints.values().filter(|c| {
            c.status != ConstraintStatus::Rejected
                && (matches!(c.status, ConstraintStatus::Confirmed)
                    || matches!(c.kind, ConstraintKind::Odometry { .. }))
        })
    }

    /// The deterministic gauge anchor: the node every solver fixes to break the
    /// global gauge freedom, IDENTICALLY on every mesh node. Rule:
    ///   1. if any Confirmed global anchor (georef/gnss/anchor) exists, the
    ///      lowest-id anchored submap is the gauge (so the scene is georeferenced);
    ///   2. else the lowest-id node, pinned at identity (scene-local until anchored).
    /// Returns `None` only for an empty graph.
    pub fn gauge_anchor(&self) -> Option<SubmapId> {
        let mut anchored: Option<SubmapId> = None;
        for c in self.constraints.values() {
            if c.status == ConstraintStatus::Confirmed && c.is_global_anchor() {
                let sid = match c.kind {
                    ConstraintKind::Gnss { submap, .. }
                    | ConstraintKind::Georef { submap, .. }
                    | ConstraintKind::Anchor { submap, .. } => submap,
                    _ => continue,
                };
                anchored = Some(match anchored {
                    Some(prev) => prev.min(sid),
                    None => sid,
                });
            }
        }
        anchored.or_else(|| self.nodes.keys().next().copied())
    }
}

/// A Scene = one coherent reconstruction (a building/area, §SPATIAL-11): its frozen
/// submaps + the pose graph that positions them + an optional georeference. This is
/// the single owner of state for one map; cross-scene data is never fused.
#[derive(Debug, Clone)]
pub struct Scene {
    pub id: u64,
    submaps: BTreeMap<SubmapId, Submap>,
    pub graph: PoseGraph,
}

impl Scene {
    pub fn new(id: u64) -> Self {
        Scene { id, submaps: BTreeMap::new(), graph: PoseGraph::new() }
    }

    /// Insert a sealed submap and ensure it has a pose-graph node. **Keep-first /
    /// idempotent**: a duplicate `SubmapId` does NOT overwrite the existing frozen
    /// geometry. Submaps are immutable and the id is coordination-free unique, so
    /// the first sealed copy wins; this keeps the scene order-independent under mesh
    /// replication / retries (two peers delivering the same id converge identically).
    /// Content verification (hash equality) is enforced at the mesh ingest layer.
    pub fn insert_submap(&mut self, submap: Submap) {
        let id = submap.id();
        self.submaps.entry(id).or_insert(submap);
        self.graph.ensure_node(id);
    }

    pub fn submap(&self, id: SubmapId) -> Option<&Submap> {
        self.submaps.get(&id)
    }

    pub fn submap_count(&self) -> usize {
        self.submaps.len()
    }

    /// Submaps in deterministic id order.
    pub fn submaps(&self) -> impl Iterator<Item = (&SubmapId, &Submap)> {
        self.submaps.iter()
    }
}
