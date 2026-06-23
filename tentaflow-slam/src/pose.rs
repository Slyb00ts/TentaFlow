// =============================================================================
// File: pose.rs — SE(3) pose wrapper (f64) for the unified SLAM state.
// Purpose: one rigid-transform type used for every pose in the system (submap →
// scene, body → submap, scene → ECEF). f64 because global poses span large
// metric extents where f32 loses millimetre precision.
// =============================================================================

use nalgebra::{Isometry3, Matrix3, Translation3, UnitQuaternion, Vector3, Vector6};

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

    /// SE(3) logarithm: the 6-vector `[ρ (translation part); φ (rotation part)]` in
    /// the Lie algebra se(3) such that `Pose::se3_exp(&self.log()) == self`. Used as
    /// the pose-graph residual `log(zᵀ·error)`.
    pub fn log(&self) -> Vector6<f64> {
        let phi = self.0.rotation.scaled_axis(); // SO(3) log
        let theta = phi.norm();
        let t = self.0.translation.vector;
        let phi_x = skew(phi);
        // Left-Jacobian inverse V⁻¹ so that ρ = V⁻¹·t.
        let v_inv = if theta < 1e-8 {
            Matrix3::identity() - 0.5 * phi_x
        } else {
            let half = 0.5 * theta;
            let coeff = (1.0 - theta * half.cos() / (2.0 * half.sin())) / (theta * theta);
            Matrix3::identity() - 0.5 * phi_x + coeff * (phi_x * phi_x)
        };
        let rho = v_inv * t;
        Vector6::new(rho.x, rho.y, rho.z, phi.x, phi.y, phi.z)
    }

    /// SE(3) exponential: map a se(3) tangent `[ρ; φ]` to a `Pose`. Inverse of
    /// [`Pose::log`]. Used to apply optimizer increments (`exp(δ)·T`).
    pub fn se3_exp(v: &Vector6<f64>) -> Pose {
        let rho = Vector3::new(v[0], v[1], v[2]);
        let phi = Vector3::new(v[3], v[4], v[5]);
        let theta = phi.norm();
        let r = UnitQuaternion::from_scaled_axis(phi);
        let phi_x = skew(phi);
        // Left Jacobian V so translation = V·ρ.
        let v_mat = if theta < 1e-8 {
            Matrix3::identity() + 0.5 * phi_x
        } else {
            let t2 = theta * theta;
            let b = (1.0 - theta.cos()) / t2;
            let c = (theta - theta.sin()) / (t2 * theta);
            Matrix3::identity() + b * phi_x + c * (phi_x * phi_x)
        };
        let t = v_mat * rho;
        Pose(Isometry3::from_parts(Translation3::from(t), r))
    }
}

/// 3×3 skew-symmetric matrix of `v` (`skew(v)·w = v × w`).
#[inline]
fn skew(v: Vector3<f64>) -> Matrix3<f64> {
    Matrix3::new(0.0, -v.z, v.y, v.z, 0.0, -v.x, -v.y, v.x, 0.0)
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
