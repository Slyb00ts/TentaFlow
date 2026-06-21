// =============================================================================
// File: pose.rs — SE(3) pose wrapper (f64) for the unified SLAM state.
// Purpose: one rigid-transform type used for every pose in the system (submap →
// scene, body → submap, scene → ECEF). f64 because global poses span large
// metric extents where f32 loses millimetre precision.
// =============================================================================

use nalgebra::{Isometry3, Translation3, UnitQuaternion, Vector3};

/// A rigid SE(3) transform. Composition reads left-to-right as frame chaining:
/// `a.then(b)` maps a point in A's child frame through A then B.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pose(pub Isometry3<f64>);

impl Pose {
    /// Identity transform.
    pub fn identity() -> Self {
        Pose(Isometry3::identity())
    }

    /// Build from a translation and a (already-normalized) unit quaternion `[x,y,z,w]`.
    pub fn from_parts(translation: [f64; 3], quat_xyzw: [f64; 4]) -> Self {
        let t = Translation3::new(translation[0], translation[1], translation[2]);
        let q = UnitQuaternion::from_quaternion(nalgebra::Quaternion::new(
            quat_xyzw[3], // w
            quat_xyzw[0], // x
            quat_xyzw[1], // y
            quat_xyzw[2], // z
        ));
        Pose(Isometry3::from_parts(t, q))
    }

    /// Translation component `[x, y, z]`.
    pub fn translation(&self) -> [f64; 3] {
        let v = self.0.translation.vector;
        [v.x, v.y, v.z]
    }

    /// Rotation as a unit quaternion `[x, y, z, w]`.
    pub fn quat_xyzw(&self) -> [f64; 4] {
        let q = self.0.rotation.quaternion();
        [q.i, q.j, q.k, q.w]
    }

    /// Inverse transform.
    pub fn inverse(&self) -> Self {
        Pose(self.0.inverse())
    }

    /// Standard SE(3) composition `self ∘ other` (matrix `self.0 * other.0`), with
    /// the convention that a pose is a child→parent transform (`world = T * local`).
    /// So `scene_to_ecef.compose(&submap_to_scene)` = submap→ECEF, and it satisfies
    /// the relative-edge identity `a.compose(&a.relative_to(&b)) == b`.
    pub fn compose(&self, other: &Pose) -> Self {
        Pose(self.0 * other.0)
    }

    /// Relative transform such that `self.compose(&self.relative_to(&other)) == other`
    /// (i.e. `self.inverse() * other`) — the measurement an odometry/loop edge stores
    /// between two pose nodes.
    pub fn relative_to(&self, other: &Pose) -> Self {
        Pose(self.0.inverse() * other.0)
    }

    /// Transform a point expressed in this pose's child frame into the parent frame.
    pub fn transform_point(&self, p: [f64; 3]) -> [f64; 3] {
        let out = self.0 * nalgebra::Point3::new(p[0], p[1], p[2]);
        [out.x, out.y, out.z]
    }

    /// Geodesic-ish magnitude of the transform: translation norm + rotation angle
    /// (radians). Used by sealing/observability thresholds, NOT as a true metric.
    pub fn magnitude(&self) -> (f64, f64) {
        let t = self.0.translation.vector.norm();
        let angle = self.0.rotation.angle();
        (t, angle)
    }
}

impl Default for Pose {
    fn default() -> Self {
        Pose::identity()
    }
}

impl From<Isometry3<f64>> for Pose {
    fn from(iso: Isometry3<f64>) -> Self {
        Pose(iso)
    }
}

/// Convenience: a pure translation pose (no rotation).
pub fn translation_pose(xyz: [f64; 3]) -> Pose {
    Pose(Isometry3::translation(xyz[0], xyz[1], xyz[2]))
}

/// Convenience: a pure rotation about an axis by `angle` radians.
pub fn rotation_pose(axis: [f64; 3], angle: f64) -> Pose {
    let q = UnitQuaternion::from_axis_angle(
        &nalgebra::Unit::new_normalize(Vector3::new(axis[0], axis[1], axis[2])),
        angle,
    );
    Pose(Isometry3::from_parts(Translation3::identity(), q))
}
