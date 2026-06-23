// =============================================================================
// File: submap.rs — frozen submaps (the core correctness/speed invariant).
// Purpose: a submap accumulates geometry while ACTIVE, then is SEALED into an
// immutable unit. Sealed geometry is shared (Arc) and has NO mutation API, so
// loop closure / relocalization / mesh merge can only REPOSITION a submap (move
// its pose in the graph), never rewrite its voxels. See UNIFIED_SLAM_ARCHITECTURE
// §1. Geometry here is a point list placeholder; chunk 0c swaps it for the
// block-hashed TSDF behind this SAME sealed API without touching callers.
// =============================================================================

use std::sync::Arc;

use nalgebra::Point3;

use crate::pose::Pose;

/// Stable, content-independent submap identity: the node that created it + a
/// per-node monotonic sequence. Globally unique without coordination, so it is a
/// safe key for mesh replication (UNIFIED_SLAM_ARCHITECTURE §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SubmapId {
    pub origin_node: u64,
    pub seq: u32,
}

impl SubmapId {
    pub fn new(origin_node: u64, seq: u32) -> Self {
        SubmapId { origin_node, seq }
    }
}

/// One point in a submap's LOCAL frame. f32 — local extents are small, so f32 is
/// plenty and halves memory. Optional color unifies camera (XYZRGB) with lidar
/// (XYZ) at the geometry level (UNIFIED_SLAM_ARCHITECTURE §11).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LocalPoint {
    pub pos: Point3<f32>,
    pub color: Option<[u8; 3]>,
}

impl LocalPoint {
    pub fn xyz(x: f32, y: f32, z: f32) -> Self {
        LocalPoint { pos: Point3::new(x, y, z), color: None }
    }
    pub fn xyz_rgb(x: f32, y: f32, z: f32, rgb: [u8; 3]) -> Self {
        LocalPoint { pos: Point3::new(x, y, z), color: Some(rgb) }
    }
}

/// Mutable, ACTIVE submap under construction by the fast path. The only place
/// geometry may grow. `seal()` consumes it into an immutable [`Submap`].
#[derive(Debug, Clone)]
pub struct SubmapBuilder {
    id: SubmapId,
    points: Vec<LocalPoint>,
}

impl SubmapBuilder {
    pub fn new(id: SubmapId) -> Self {
        SubmapBuilder { id, points: Vec::new() }
    }

    pub fn id(&self) -> SubmapId {
        self.id
    }

    /// Add geometry while active. Allowed ONLY before sealing.
    pub fn push(&mut self, p: LocalPoint) {
        self.points.push(p);
    }

    pub fn extend(&mut self, pts: impl IntoIterator<Item = LocalPoint>) {
        self.points.extend(pts);
    }

    pub fn len(&self) -> usize {
        self.points.len()
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Freeze into an immutable submap. After this the geometry can never change;
    /// only the submap's POSE moves in the graph.
    pub fn seal(self) -> Submap {
        Submap {
            id: self.id,
            geometry: Arc::new(SubmapGeometry { points: self.points }),
        }
    }
}

/// Immutable, sealed geometry. Behind an `Arc` so cloning a submap is O(1) and
/// snapshots handed to the fast path / renderer share the same frozen bytes.
#[derive(Debug)]
pub struct SubmapGeometry {
    points: Vec<LocalPoint>,
}

impl SubmapGeometry {
    pub fn points(&self) -> &[LocalPoint] {
        &self.points
    }
}

/// A SEALED submap: frozen local geometry + identity. There is deliberately NO
/// method that mutates `geometry` — immutability is enforced by the type, which is
/// the whole correctness argument (§1). A submap's position in the world lives in
/// the pose graph, not here.
#[derive(Debug, Clone)]
pub struct Submap {
    id: SubmapId,
    geometry: Arc<SubmapGeometry>,
}

impl Submap {
    pub fn id(&self) -> SubmapId {
        self.id
    }

    /// Read-only access to the frozen geometry (shared, cheap to hold).
    pub fn geometry(&self) -> &Arc<SubmapGeometry> {
        &self.geometry
    }

    pub fn point_count(&self) -> usize {
        self.geometry.points.len()
    }

    /// Project this submap's local geometry into a parent frame given the submap's
    /// pose (e.g. submap→scene from the graph). Allocates a fresh Vec — the source
    /// geometry stays frozen. Used by the renderer / scan-to-map registration.
    pub fn points_in(&self, submap_pose: &Pose) -> Vec<[f64; 3]> {
        self.geometry
            .points
            .iter()
            .map(|p| submap_pose.transform_point([p.pos.x as f64, p.pos.y as f64, p.pos.z as f64]))
            .collect()
    }
}
