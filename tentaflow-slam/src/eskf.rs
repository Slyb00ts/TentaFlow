// =============================================================================
// File: eskf.rs — Error-State Kalman Filter localization engine (positioning P-A).
// Purpose: the lightweight fusion profile of UNIVERSAL_POSITIONING_PLAN §7 — fuse a
// high-rate IMU (relative motion backbone) with drift-free absolute fixes (GNSS, and
// altitude from a barometer) into ONE probabilistic state, emitting a canonical
// `GlobalPose` with honest covariance + a source bitmask. Heterogeneous by design: an
// absent sensor is just a missing update, never a crash — the estimate simply widens.
//
// 15-state ESKF (Sola, "Quaternion kinematics for the error-state KF"):
//   nominal: position p, velocity v (ENU m, m/s), orientation q (body→nav), accel
//   bias b_a, gyro bias b_g. Error state δx ∈ ℝ¹⁵ = [δp δv δθ δb_a δb_g].
// Nav frame = local ENU anchored at the FIRST GNSS fix (geo.rs converts fixes ↔ ENU
// and the ENU pose ↔ WGS84). Before any fix the engine is scene-local (metric ENU,
// arbitrary origin) and reports `SceneLocal`; once anchored it reports `Global`.
// Pure Rust, no device deps — off-device testable on synthetic trajectories.
// =============================================================================

use nalgebra::{Matrix3, SMatrix, SVector, UnitQuaternion, Vector3};
use tentaflow_sdk_spec::{
    BaroSample, GnssFix, GlobalPoseFrame, ImuSample, GLOBAL_POSE_VERSION, POSE_SRC_GNSS,
    POSE_SRC_IMU, POSE_STATE_GLOBAL, POSE_STATE_SCENE_LOCAL,
};

use crate::geo::{enu_to_geodetic, geodetic_to_enu};

type Mat15 = SMatrix<f64, 15, 15>;
type Vec15 = SVector<f64, 15>;

// Error-state block offsets.
const IP: usize = 0; // position
const IV: usize = 3; // velocity
const ITH: usize = 6; // orientation error
const IBA: usize = 9; // accel bias
const IBG: usize = 12; // gyro bias

/// Noise densities + gravity for the ESKF process model. Defaults are sane
/// consumer-MEMS values; an addon may tune per device from datasheet specs.
#[derive(Debug, Clone, Copy)]
pub struct EskfConfig {
    /// Accelerometer white-noise density (m/s² / √Hz).
    pub accel_noise: f64,
    /// Gyroscope white-noise density (rad/s / √Hz).
    pub gyro_noise: f64,
    /// Accelerometer bias random-walk (m/s³ / √Hz).
    pub accel_bias_rw: f64,
    /// Gyroscope bias random-walk (rad/s² / √Hz).
    pub gyro_bias_rw: f64,
    /// Gravity magnitude (m/s²) in the ENU nav frame (acts along −Up).
    pub gravity: f64,
}

impl Default for EskfConfig {
    fn default() -> Self {
        EskfConfig {
            accel_noise: 0.02,
            gyro_noise: 0.002,
            accel_bias_rw: 1.0e-4,
            gyro_bias_rw: 1.0e-5,
            gravity: 9.81,
        }
    }
}

/// 3×3 skew-symmetric matrix (`skew(v)·w = v × w`).
#[inline]
fn skew(v: Vector3<f64>) -> Matrix3<f64> {
    Matrix3::new(0.0, -v.z, v.y, v.z, 0.0, -v.x, -v.y, v.x, 0.0)
}

/// Error-State KF localization engine. Feed IMU + absolute fixes; read `GlobalPose`.
#[derive(Debug, Clone)]
pub struct EskfEngine {
    cfg: EskfConfig,
    // Nominal state.
    p: Vector3<f64>,
    v: Vector3<f64>,
    q: UnitQuaternion<f64>,
    ba: Vector3<f64>,
    bg: Vector3<f64>,
    // Error covariance.
    cov: Mat15,
    /// Geodetic anchor of the ENU nav frame, set on the first GNSS fix. `None` →
    /// scene-local (no global frame yet).
    origin: Option<(f64, f64, f64)>,
    last_us: Option<i64>,
    /// Sensors that have contributed (POSE_SRC_* bitmask).
    source: u8,
}

impl EskfEngine {
    pub fn new(cfg: EskfConfig) -> Self {
        // Initial covariance: position arbitrary (no global fix yet) → wide; velocity
        // moderate; roll/pitch will be observed from gravity → moderate, yaw
        // unobservable without mag/GNSS-motion → very wide; biases datasheet-ish.
        let mut cov = Mat15::zeros();
        let set = |c: &mut Mat15, i: usize, var: f64| {
            for k in 0..3 {
                c[(i + k, i + k)] = var;
            }
        };
        set(&mut cov, IP, 100.0 * 100.0);
        set(&mut cov, IV, 10.0 * 10.0);
        cov[(ITH, ITH)] = (10.0_f64.to_radians()).powi(2);
        cov[(ITH + 1, ITH + 1)] = (10.0_f64.to_radians()).powi(2);
        cov[(ITH + 2, ITH + 2)] = (180.0_f64.to_radians()).powi(2);
        set(&mut cov, IBA, 0.5 * 0.5);
        set(&mut cov, IBG, 0.05 * 0.05);
        EskfEngine {
            cfg,
            p: Vector3::zeros(),
            v: Vector3::zeros(),
            q: UnitQuaternion::identity(),
            ba: Vector3::zeros(),
            bg: Vector3::zeros(),
            cov,
            origin: None,
            last_us: None,
            source: 0,
        }
    }

    pub fn position_enu(&self) -> [f64; 3] {
        [self.p.x, self.p.y, self.p.z]
    }

    pub fn velocity_enu(&self) -> [f64; 3] {
        [self.v.x, self.v.y, self.v.z]
    }

    pub fn quat_xyzw(&self) -> [f64; 4] {
        let q = self.q.quaternion();
        [q.i, q.j, q.k, q.w]
    }

    pub fn is_georeferenced(&self) -> bool {
        self.origin.is_some()
    }

    /// The ENU nav-frame origin's geodetic position `(lat°, lon°, alt m)`, set on the
    /// first GNSS fix. `None` until georeferenced. Used to convert an ENU-aligned
    /// device-local map into WGS84 (see `GeoAnchor::from_alignment`).
    pub fn origin(&self) -> Option<(f64, f64, f64)> {
        self.origin
    }

    /// IMU predict step. `dt` is derived from consecutive sample timestamps; the first
    /// sample only seeds the clock (no integration). Non-finite samples are dropped.
    /// Returns `true` iff the state was actually advanced (a prediction ran) — callers
    /// must not publish a pose for a dropped/seed-only sample.
    pub fn ingest_imu(&mut self, s: &ImuSample) -> bool {
        if !s.is_finite() {
            return false;
        }
        self.source |= POSE_SRC_IMU;
        let last = match self.last_us.replace(s.timestamp_us) {
            Some(t) => t,
            None => return false, // first sample: seed clock only
        };
        let dt = (s.timestamp_us - last) as f64 * 1.0e-6;
        if !(dt > 0.0 && dt < 1.0) {
            return false; // out-of-order / implausible gap → skip integration
        }
        self.predict(
            Vector3::new(s.accel[0] as f64, s.accel[1] as f64, s.accel[2] as f64),
            Vector3::new(s.gyro[0] as f64, s.gyro[1] as f64, s.gyro[2] as f64),
            s.accel_noise_std.max(1.0e-4) as f64,
            s.gyro_noise_std.max(1.0e-6) as f64,
            dt,
        );
        true
    }

    fn predict(
        &mut self,
        accel_meas: Vector3<f64>,
        gyro_meas: Vector3<f64>,
        accel_noise: f64,
        gyro_noise: f64,
        dt: f64,
    ) {
        let a = accel_meas - self.ba;
        let w = gyro_meas - self.bg;
        let r = self.q.to_rotation_matrix();
        let g = Vector3::new(0.0, 0.0, -self.cfg.gravity);
        let acc_nav = r * a + g;

        // Nominal integration (Euler; dt is small at IMU rate).
        self.p += self.v * dt + 0.5 * acc_nav * dt * dt;
        self.v += acc_nav * dt;
        self.q *= UnitQuaternion::from_scaled_axis(w * dt);

        // Error-state transition Fx (discrete, first order).
        let mut fx = Mat15::identity();
        let rm = r.matrix();
        let i3 = Matrix3::identity();
        fx.fixed_view_mut::<3, 3>(IP, IV).copy_from(&(i3 * dt));
        fx.fixed_view_mut::<3, 3>(IV, ITH)
            .copy_from(&(-rm * skew(a) * dt));
        fx.fixed_view_mut::<3, 3>(IV, IBA).copy_from(&(-rm * dt));
        fx.fixed_view_mut::<3, 3>(ITH, ITH)
            .copy_from(&(i3 - skew(w) * dt));
        fx.fixed_view_mut::<3, 3>(ITH, IBG).copy_from(&(-i3 * dt));

        // Process-noise impulses (Sola eqs.): velocity ← accel WN, θ ← gyro WN,
        // biases ← random walk.
        let mut q = Mat15::zeros();
        let an = accel_noise * accel_noise * dt * dt;
        let gn = gyro_noise * gyro_noise * dt * dt;
        let baw = self.cfg.accel_bias_rw * self.cfg.accel_bias_rw * dt;
        let bgw = self.cfg.gyro_bias_rw * self.cfg.gyro_bias_rw * dt;
        for k in 0..3 {
            q[(IV + k, IV + k)] = an;
            q[(ITH + k, ITH + k)] = gn;
            q[(IBA + k, IBA + k)] = baw;
            q[(IBG + k, IBG + k)] = bgw;
        }
        self.cov = fx * self.cov * fx.transpose() + q;
    }

    /// Generic measurement update (Joseph form for covariance stability), then inject
    /// the error into the nominal state and reset.
    fn update<const M: usize>(
        &mut self,
        h: SMatrix<f64, M, 15>,
        residual: SVector<f64, M>,
        r: SMatrix<f64, M, M>,
    ) {
        let s = h * self.cov * h.transpose() + r;
        let Some(s_inv) = s.try_inverse() else {
            return; // singular innovation → skip rather than corrupt the state
        };
        let k = self.cov * h.transpose() * s_inv;
        let dx: Vec15 = k * residual;
        self.inject(&dx);
        let i_kh = Mat15::identity() - k * h;
        // Joseph: P = (I-KH)P(I-KH)ᵀ + K R Kᵀ — stays symmetric positive-definite.
        self.cov = i_kh * self.cov * i_kh.transpose() + k * r * k.transpose();
    }

    fn inject(&mut self, dx: &Vec15) {
        self.p += dx.fixed_rows::<3>(IP);
        self.v += dx.fixed_rows::<3>(IV);
        let dth: Vector3<f64> = dx.fixed_rows::<3>(ITH).into();
        self.q *= UnitQuaternion::from_scaled_axis(dth);
        self.ba += dx.fixed_rows::<3>(IBA);
        self.bg += dx.fixed_rows::<3>(IBG);
    }

    /// Fold a GNSS fix. The FIRST valid fix anchors the ENU nav frame at its position
    /// (establishing the global frame); subsequent fixes are position (and, when
    /// present, velocity) updates. Invalid fixes are dropped.
    /// Returns `true` iff the fix was accepted (valid) and updated the state.
    pub fn ingest_gnss(&mut self, fix: &GnssFix) -> bool {
        if !fix.is_valid() {
            return false;
        }
        self.source |= POSE_SRC_GNSS;
        let Some((lat0, lon0, alt0)) = self.origin else {
            // First fix: anchor ENU origin here, place the state AT the origin, and
            // size the position uncertainty from the fix accuracy.
            self.origin = Some((fix.lat_deg, fix.lon_deg, fix.alt_m));
            self.p = Vector3::zeros();
            let (h, vv) = (fix.h_acc_m.max(0.5) as f64, fix.v_acc_m.max(1.0) as f64);
            self.cov[(IP, IP)] = h * h;
            self.cov[(IP + 1, IP + 1)] = h * h;
            self.cov[(IP + 2, IP + 2)] = vv * vv;
            if fix.has_velocity() {
                self.apply_velocity(fix);
            }
            return true;
        };
        let enu = geodetic_to_enu(fix.lat_deg, fix.lon_deg, fix.alt_m, lat0, lon0, alt0);
        let z = Vector3::new(enu[0], enu[1], enu[2]);
        let mut h = SMatrix::<f64, 3, 15>::zeros();
        h.fixed_view_mut::<3, 3>(0, IP).copy_from(&Matrix3::identity());
        let residual = z - self.p;
        let (ha, va) = (fix.h_acc_m.max(0.1) as f64, fix.v_acc_m.max(0.1) as f64);
        let mut rr = Matrix3::<f64>::zeros();
        rr[(0, 0)] = ha * ha;
        rr[(1, 1)] = ha * ha;
        rr[(2, 2)] = va * va;
        self.update(h, residual, rr);
        if fix.has_velocity() {
            self.apply_velocity(fix);
        }
        true
    }

    fn apply_velocity(&mut self, fix: &GnssFix) {
        let z = Vector3::new(
            fix.vel_enu[0] as f64,
            fix.vel_enu[1] as f64,
            fix.vel_enu[2] as f64,
        );
        let mut h = SMatrix::<f64, 3, 15>::zeros();
        h.fixed_view_mut::<3, 3>(0, IV).copy_from(&Matrix3::identity());
        let residual = z - self.v;
        let va = (fix.vel_acc_m_s.max(0.05) as f64).powi(2);
        self.update(h, residual, Matrix3::identity() * va);
    }

    /// Fold a barometer altitude into the Up (z) channel. NOTE: this trusts the
    /// barometer's relative altitude as an absolute Up measurement against the ENU
    /// origin (a coarse aid, weather-biased); a dedicated baro-bias state is a later
    /// refinement. Useful to bound vertical drift between GNSS fixes.
    pub fn ingest_baro(&mut self, s: &BaroSample) -> bool {
        if !s.is_finite() {
            return false;
        }
        let mut h = SMatrix::<f64, 1, 15>::zeros();
        h[(0, IP + 2)] = 1.0;
        let residual = SVector::<f64, 1>::new(s.relative_altitude_m as f64 - self.p.z);
        let var = (s.noise_std_m.max(0.5) as f64).powi(2);
        self.update(h, residual, SVector::<f64, 1>::new(var).into());
        true
    }

    /// Diagonal covariance `[σ²x σ²y σ²z σ²roll σ²pitch σ²yaw]` for the output frame.
    fn cov_diag(&self) -> [f32; 6] {
        [
            self.cov[(IP, IP)] as f32,
            self.cov[(IP + 1, IP + 1)] as f32,
            self.cov[(IP + 2, IP + 2)] as f32,
            self.cov[(ITH, ITH)] as f32,
            self.cov[(ITH + 1, ITH + 1)] as f32,
            self.cov[(ITH + 2, ITH + 2)] as f32,
        ]
    }

    /// Emit the canonical `GlobalPose`: WGS84 lat/lon/alt + `Global` once anchored by a
    /// GNSS fix, else scene-local ENU metres + `SceneLocal`. Orientation is the nav-frame
    /// quaternion; covariance + source bitmask report what the fix rests on.
    pub fn global_pose(&self, timestamp_us: i64, scene_id: u64) -> GlobalPoseFrame {
        let (state, position) = match self.origin {
            Some((lat0, lon0, alt0)) => {
                let (lat, lon, alt) = enu_to_geodetic(self.position_enu(), lat0, lon0, alt0);
                (POSE_STATE_GLOBAL, [lat, lon, alt])
            }
            None => (POSE_STATE_SCENE_LOCAL, self.position_enu()),
        };
        GlobalPoseFrame {
            version: GLOBAL_POSE_VERSION,
            state,
            source: self.source,
            timestamp_us,
            scene_id,
            position,
            quat_xyzw: self.quat_xyzw(),
            cov_diag: self.cov_diag(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tentaflow_sdk_spec::{GNSS_FIX_VERSION, GNSS_FLAG_HAS_VELOCITY, IMU_SAMPLE_VERSION};

    fn imu(ts_us: i64, accel: [f32; 3], gyro: [f32; 3]) -> ImuSample {
        ImuSample {
            version: IMU_SAMPLE_VERSION,
            flags: 0,
            timestamp_us: ts_us,
            accel,
            gyro,
            accel_noise_std: 0.02,
            gyro_noise_std: 0.002,
        }
    }

    fn gnss(ts_us: i64, lat: f64, lon: f64, alt: f64) -> GnssFix {
        GnssFix {
            version: GNSS_FIX_VERSION,
            flags: 0,
            timestamp_us: ts_us,
            lat_deg: lat,
            lon_deg: lon,
            alt_m: alt,
            h_acc_m: 1.0,
            v_acc_m: 2.0,
            vel_enu: [0.0, 0.0, 0.0],
            vel_acc_m_s: 0.0,
        }
    }

    // A level, static device reads specific force = +g on Up (accel = [0,0,g]).
    const LEVEL_ACCEL: [f32; 3] = [0.0, 0.0, 9.81];

    #[test]
    fn imu_predict_grows_position_covariance() {
        let mut e = EskfEngine::new(EskfConfig::default());
        e.ingest_imu(&imu(0, LEVEL_ACCEL, [0.0; 3])); // seed clock
        let p0 = e.cov[(IP, IP)];
        for k in 1..=50 {
            e.ingest_imu(&imu(k * 10_000, LEVEL_ACCEL, [0.0; 3])); // 100 Hz, 0.5 s
        }
        assert!(e.cov[(IP, IP)] > p0, "unanchored prediction widens position covariance");
        // A level static device stays put (gravity cancels) — position near zero.
        assert!(e.position_enu().iter().all(|c| c.abs() < 0.5));
    }

    #[test]
    fn gnss_anchors_global_frame_and_reports_wgs84() {
        let mut e = EskfEngine::new(EskfConfig::default());
        assert!(!e.is_georeferenced());
        assert_eq!(e.global_pose(0, 1).state, POSE_STATE_SCENE_LOCAL);

        e.ingest_imu(&imu(0, LEVEL_ACCEL, [0.0; 3]));
        e.ingest_gnss(&gnss(0, 52.2297, 21.0122, 118.5));
        assert!(e.is_georeferenced());
        let gp = e.global_pose(0, 1);
        assert_eq!(gp.state, POSE_STATE_GLOBAL);
        assert!((gp.position[0] - 52.2297).abs() < 1e-5, "anchored at the fix lat");
        assert!((gp.position[1] - 21.0122).abs() < 1e-5);
        assert!(gp.source & POSE_SRC_GNSS != 0 && gp.source & POSE_SRC_IMU != 0);
    }

    #[test]
    fn gnss_updates_pull_estimate_toward_fix() {
        // Anchor at origin, then deliver fixes ~10 m East; the ENU estimate must move
        // toward +E and stay bounded (absolute fixes kill the unanchored drift).
        let mut e = EskfEngine::new(EskfConfig::default());
        let (lat0, lon0, alt0) = (52.0, 21.0, 100.0);
        e.ingest_imu(&imu(0, LEVEL_ACCEL, [0.0; 3]));
        e.ingest_gnss(&gnss(0, lat0, lon0, alt0));
        // 10 m East of the origin in WGS84.
        let east10 = crate::geo::enu_to_geodetic([10.0, 0.0, 0.0], lat0, lon0, alt0);
        let mut ts = 0;
        for _ in 0..30 {
            ts += 100_000;
            e.ingest_imu(&imu(ts, LEVEL_ACCEL, [0.0; 3]));
            e.ingest_gnss(&gnss(ts, east10.0, east10.1, east10.2));
        }
        let enu = e.position_enu();
        assert!((enu[0] - 10.0).abs() < 1.0, "estimate converged to +10 m East: {}", enu[0]);
        assert!(e.cov[(IP, IP)] < 25.0, "absolute fixes bound position covariance");
    }

    #[test]
    fn constant_velocity_dead_reckons_between_fixes() {
        // Seed eastward velocity via a GNSS velocity fix, then dead-reckon on IMU only
        // (level → gravity cancels) and check the position advances ~v*t East.
        let mut e = EskfEngine::new(EskfConfig::default());
        let (lat0, lon0, alt0) = (0.0, 0.0, 0.0);
        e.ingest_imu(&imu(0, LEVEL_ACCEL, [0.0; 3]));
        let mut anchor = gnss(0, lat0, lon0, alt0);
        anchor.flags = GNSS_FLAG_HAS_VELOCITY;
        anchor.vel_enu = [2.0, 0.0, 0.0]; // 2 m/s East
        anchor.vel_acc_m_s = 0.05;
        e.ingest_gnss(&anchor);
        // 1 s of IMU-only dead reckoning at 100 Hz.
        let mut ts = 0;
        for _ in 0..100 {
            ts += 10_000;
            e.ingest_imu(&imu(ts, LEVEL_ACCEL, [0.0; 3]));
        }
        let enu = e.position_enu();
        assert!((enu[0] - 2.0).abs() < 0.3, "dead-reckoned ~2 m East: {}", enu[0]);
        assert!(enu[1].abs() < 0.3 && enu[2].abs() < 0.3, "no lateral/vertical drift");
    }

    #[test]
    fn baro_bounds_vertical_channel() {
        let mut e = EskfEngine::new(EskfConfig::default());
        e.ingest_imu(&imu(0, LEVEL_ACCEL, [0.0; 3]));
        e.ingest_gnss(&gnss(0, 0.0, 0.0, 0.0));
        // Baro says we're 5 m up; the Up estimate should move toward it.
        let baro = BaroSample {
            version: tentaflow_sdk_spec::BARO_SAMPLE_VERSION,
            flags: 0,
            timestamp_us: 1,
            pressure_pa: 101_325.0,
            relative_altitude_m: 5.0,
            noise_std_m: 0.5,
        };
        for _ in 0..10 {
            e.ingest_baro(&baro);
        }
        assert!((e.position_enu()[2] - 5.0).abs() < 1.0, "baro pulls Up toward 5 m");
    }

    #[test]
    fn covariance_stays_symmetric_finite() {
        let mut e = EskfEngine::new(EskfConfig::default());
        let mut ts = 0;
        e.ingest_imu(&imu(ts, LEVEL_ACCEL, [0.01, -0.02, 0.0]));
        for _ in 0..50 {
            ts += 10_000;
            e.ingest_imu(&imu(ts, [0.1, 0.0, 9.81], [0.0, 0.0, 0.05]));
            if ts % 100_000 == 0 {
                e.ingest_gnss(&gnss(ts, 0.0, 0.0, 0.0));
            }
        }
        for i in 0..15 {
            for j in 0..15 {
                assert!(e.cov[(i, j)].is_finite(), "cov finite");
                assert!((e.cov[(i, j)] - e.cov[(j, i)]).abs() < 1e-6, "cov symmetric");
            }
            assert!(e.cov[(i, i)] >= 0.0, "non-negative variance");
        }
    }
}
