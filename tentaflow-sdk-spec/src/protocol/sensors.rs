// =============================================================================
// File: protocol/sensors.rs — canonical positioning-sensor measurement messages.
// Purpose: the fixed-binary inputs to the Localization Engine (UNIVERSAL_POSITIONING
// _PLAN §9), same philosophy as the LiDAR frame / GlobalPose: a versioned LE header
// + packed body, NO JSON on the hot path, each carrying a capture timestamp and a
// noise model so the fusion engine reads only constraints, never device specifics.
// A per-device addon emits these from raw device data (the "different data" boundary);
// the engine consumes any subset and reports the resulting covariance.
//
// One message per sensor kind: `ImuSample` (motion backbone), `GnssFix` (absolute
// global), `BaroSample` (altitude), `MagSample` (heading). LiDAR reuses the existing
// `LidarFrameHeader`; visual messages land in a later phase. Decode is fail-closed
// (length / version / reserved checks); numeric validity is the engine's gate.
// =============================================================================

/// Read a little-endian `f32` at `o` (caller guarantees `o + 4 <= len`).
#[inline]
fn rd_f32(b: &[u8], o: usize) -> f32 {
    f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// Read a little-endian `f64` at `o`.
#[inline]
fn rd_f64(b: &[u8], o: usize) -> f64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    f64::from_le_bytes(a)
}

/// Read a little-endian `i64` at `o`.
#[inline]
fn rd_i64(b: &[u8], o: usize) -> i64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    i64::from_le_bytes(a)
}

// ----------------------------------------------------------------------------
// IMU — gyro + accel, the high-rate relative-motion backbone.
// ----------------------------------------------------------------------------

pub const IMU_SAMPLE_VERSION: u8 = 1;
pub const IMU_SAMPLE_LEN: usize = 44;

/// One IMU sample: linear acceleration (m/s², INCLUDING gravity, body frame) + angular
/// velocity (rad/s, body frame), with per-stream white-noise std devs. Streams at the
/// device IMU rate; the engine preintegrates between state nodes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImuSample {
    pub version: u8,
    pub flags: u8,
    pub timestamp_us: i64,
    pub accel: [f32; 3],
    pub gyro: [f32; 3],
    /// Accelerometer white-noise std (m/s²) and gyro white-noise std (rad/s).
    pub accel_noise_std: f32,
    pub gyro_noise_std: f32,
}

impl ImuSample {
    pub fn encode(&self) -> [u8; IMU_SAMPLE_LEN] {
        let mut b = [0u8; IMU_SAMPLE_LEN];
        b[0] = self.version;
        b[1] = self.flags;
        // bytes 2-3 reserved (zero).
        b[4..12].copy_from_slice(&self.timestamp_us.to_le_bytes());
        for (i, v) in self.accel.iter().enumerate() {
            b[12 + i * 4..16 + i * 4].copy_from_slice(&v.to_le_bytes());
        }
        for (i, v) in self.gyro.iter().enumerate() {
            b[24 + i * 4..28 + i * 4].copy_from_slice(&v.to_le_bytes());
        }
        b[36..40].copy_from_slice(&self.accel_noise_std.to_le_bytes());
        b[40..44].copy_from_slice(&self.gyro_noise_std.to_le_bytes());
        b
    }

    pub fn decode(b: &[u8]) -> Option<ImuSample> {
        if b.len() < IMU_SAMPLE_LEN || b[0] != IMU_SAMPLE_VERSION || b[2] != 0 || b[3] != 0 {
            return None;
        }
        Some(ImuSample {
            version: b[0],
            flags: b[1],
            timestamp_us: rd_i64(b, 4),
            accel: [rd_f32(b, 12), rd_f32(b, 16), rd_f32(b, 20)],
            gyro: [rd_f32(b, 24), rd_f32(b, 28), rd_f32(b, 32)],
            accel_noise_std: rd_f32(b, 36),
            gyro_noise_std: rd_f32(b, 40),
        })
    }

    /// All numeric fields finite — the engine drops a sample that fails this.
    pub fn is_finite(&self) -> bool {
        self.accel.iter().chain(self.gyro.iter()).all(|v| v.is_finite())
            && self.accel_noise_std.is_finite()
            && self.gyro_noise_std.is_finite()
    }
}

// ----------------------------------------------------------------------------
// GNSS — absolute global position fix (drift-free anchor when sky is visible).
// ----------------------------------------------------------------------------

pub const GNSS_FIX_VERSION: u8 = 1;
pub const GNSS_FIX_LEN: usize = 60;
/// Flag: the velocity block (`vel_enu` + `vel_acc`) is valid.
pub const GNSS_FLAG_HAS_VELOCITY: u8 = 1 << 0;

/// One GNSS fix: WGS84 position + accuracies, optional ENU velocity. Accuracies are
/// 1σ std devs (metres / m·s⁻¹), the engine's noise model for the absolute constraint.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GnssFix {
    pub version: u8,
    pub flags: u8,
    pub timestamp_us: i64,
    pub lat_deg: f64,
    pub lon_deg: f64,
    pub alt_m: f64,
    pub h_acc_m: f32,
    pub v_acc_m: f32,
    /// East/North/Up velocity (m/s); valid only with `GNSS_FLAG_HAS_VELOCITY`.
    pub vel_enu: [f32; 3],
    pub vel_acc_m_s: f32,
}

impl GnssFix {
    pub fn has_velocity(&self) -> bool {
        self.flags & GNSS_FLAG_HAS_VELOCITY != 0
    }

    pub fn encode(&self) -> [u8; GNSS_FIX_LEN] {
        let mut b = [0u8; GNSS_FIX_LEN];
        b[0] = self.version;
        b[1] = self.flags;
        b[4..12].copy_from_slice(&self.timestamp_us.to_le_bytes());
        b[12..20].copy_from_slice(&self.lat_deg.to_le_bytes());
        b[20..28].copy_from_slice(&self.lon_deg.to_le_bytes());
        b[28..36].copy_from_slice(&self.alt_m.to_le_bytes());
        b[36..40].copy_from_slice(&self.h_acc_m.to_le_bytes());
        b[40..44].copy_from_slice(&self.v_acc_m.to_le_bytes());
        for (i, v) in self.vel_enu.iter().enumerate() {
            b[44 + i * 4..48 + i * 4].copy_from_slice(&v.to_le_bytes());
        }
        b[56..60].copy_from_slice(&self.vel_acc_m_s.to_le_bytes());
        b
    }

    pub fn decode(b: &[u8]) -> Option<GnssFix> {
        if b.len() < GNSS_FIX_LEN || b[0] != GNSS_FIX_VERSION || b[2] != 0 || b[3] != 0 {
            return None;
        }
        Some(GnssFix {
            version: b[0],
            flags: b[1],
            timestamp_us: rd_i64(b, 4),
            lat_deg: rd_f64(b, 12),
            lon_deg: rd_f64(b, 20),
            alt_m: rd_f64(b, 28),
            h_acc_m: rd_f32(b, 36),
            v_acc_m: rd_f32(b, 40),
            vel_enu: [rd_f32(b, 44), rd_f32(b, 48), rd_f32(b, 52)],
            vel_acc_m_s: rd_f32(b, 56),
        })
    }

    /// Finite + in-range lat/lon — the engine rejects an out-of-range/NaN fix.
    pub fn is_valid(&self) -> bool {
        self.lat_deg.is_finite()
            && self.lon_deg.is_finite()
            && self.alt_m.is_finite()
            && (-90.0..=90.0).contains(&self.lat_deg)
            && (-180.0..=180.0).contains(&self.lon_deg)
            && self.h_acc_m.is_finite()
            && self.v_acc_m.is_finite()
    }
}

// ----------------------------------------------------------------------------
// Barometer — semi-absolute altitude (relative, weather-biased).
// ----------------------------------------------------------------------------

pub const BARO_SAMPLE_VERSION: u8 = 1;
pub const BARO_SAMPLE_LEN: usize = 24;

/// One barometer sample: raw pressure (Pa) + the OS-provided relative altitude (m)
/// when available, with a 1σ altitude noise std. Recalibrated against GNSS/map.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BaroSample {
    pub version: u8,
    pub flags: u8,
    pub timestamp_us: i64,
    pub pressure_pa: f32,
    pub relative_altitude_m: f32,
    pub noise_std_m: f32,
}

impl BaroSample {
    pub fn encode(&self) -> [u8; BARO_SAMPLE_LEN] {
        let mut b = [0u8; BARO_SAMPLE_LEN];
        b[0] = self.version;
        b[1] = self.flags;
        b[4..12].copy_from_slice(&self.timestamp_us.to_le_bytes());
        b[12..16].copy_from_slice(&self.pressure_pa.to_le_bytes());
        b[16..20].copy_from_slice(&self.relative_altitude_m.to_le_bytes());
        b[20..24].copy_from_slice(&self.noise_std_m.to_le_bytes());
        b
    }

    pub fn decode(b: &[u8]) -> Option<BaroSample> {
        if b.len() < BARO_SAMPLE_LEN || b[0] != BARO_SAMPLE_VERSION || b[2] != 0 || b[3] != 0 {
            return None;
        }
        Some(BaroSample {
            version: b[0],
            flags: b[1],
            timestamp_us: rd_i64(b, 4),
            pressure_pa: rd_f32(b, 12),
            relative_altitude_m: rd_f32(b, 16),
            noise_std_m: rd_f32(b, 20),
        })
    }

    pub fn is_finite(&self) -> bool {
        self.pressure_pa.is_finite()
            && self.relative_altitude_m.is_finite()
            && self.noise_std_m.is_finite()
    }
}

// ----------------------------------------------------------------------------
// Magnetometer — heading cue (good outdoors, poor in cluttered magnetic env).
// ----------------------------------------------------------------------------

pub const MAG_SAMPLE_VERSION: u8 = 1;
pub const MAG_SAMPLE_LEN: usize = 28;

/// One magnetometer sample: field vector (µT, body frame) + a 1σ noise std. The engine
/// derives heading and weights it by environment (the addon may inflate `noise_std_ut`
/// indoors).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MagSample {
    pub version: u8,
    pub flags: u8,
    pub timestamp_us: i64,
    pub field_ut: [f32; 3],
    pub noise_std_ut: f32,
}

impl MagSample {
    pub fn encode(&self) -> [u8; MAG_SAMPLE_LEN] {
        let mut b = [0u8; MAG_SAMPLE_LEN];
        b[0] = self.version;
        b[1] = self.flags;
        b[4..12].copy_from_slice(&self.timestamp_us.to_le_bytes());
        for (i, v) in self.field_ut.iter().enumerate() {
            b[12 + i * 4..16 + i * 4].copy_from_slice(&v.to_le_bytes());
        }
        b[24..28].copy_from_slice(&self.noise_std_ut.to_le_bytes());
        b
    }

    pub fn decode(b: &[u8]) -> Option<MagSample> {
        if b.len() < MAG_SAMPLE_LEN || b[0] != MAG_SAMPLE_VERSION || b[2] != 0 || b[3] != 0 {
            return None;
        }
        Some(MagSample {
            version: b[0],
            flags: b[1],
            timestamp_us: rd_i64(b, 4),
            field_ut: [rd_f32(b, 12), rd_f32(b, 16), rd_f32(b, 20)],
            noise_std_ut: rd_f32(b, 24),
        })
    }

    pub fn is_finite(&self) -> bool {
        self.field_ut.iter().all(|v| v.is_finite()) && self.noise_std_ut.is_finite()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imu_round_trips_and_rejects_bad_header() {
        let s = ImuSample {
            version: IMU_SAMPLE_VERSION,
            flags: 0,
            timestamp_us: 1_700_000_000_000_123,
            accel: [0.01, -0.02, 9.81],
            gyro: [0.001, 0.002, -0.003],
            accel_noise_std: 0.02,
            gyro_noise_std: 0.001,
        };
        assert_eq!(ImuSample::decode(&s.encode()), Some(s));
        assert!(s.is_finite());
        assert_eq!(IMU_SAMPLE_LEN, 44);
        // short / bad version / non-zero reserved → None.
        assert!(ImuSample::decode(&[0u8; 10]).is_none());
        let mut b = s.encode();
        b[0] = 9;
        assert!(ImuSample::decode(&b).is_none());
        let mut b = s.encode();
        b[2] = 1;
        assert!(ImuSample::decode(&b).is_none());
        // NaN survives decode (parse) but fails the validity gate.
        let mut nan = s;
        nan.accel[0] = f32::NAN;
        assert!(!nan.is_finite());
        assert_eq!(ImuSample::decode(&nan.encode()).map(|d| d.is_finite()), Some(false));
    }

    #[test]
    fn gnss_round_trips_with_and_without_velocity() {
        let mut f = GnssFix {
            version: GNSS_FIX_VERSION,
            flags: 0,
            timestamp_us: 42,
            lat_deg: 52.2297,
            lon_deg: 21.0122,
            alt_m: 118.5,
            h_acc_m: 3.5,
            v_acc_m: 6.0,
            vel_enu: [0.0, 0.0, 0.0],
            vel_acc_m_s: 0.0,
        };
        assert_eq!(GnssFix::decode(&f.encode()), Some(f));
        assert!(!f.has_velocity());
        assert!(f.is_valid());
        // With velocity flagged.
        f.flags = GNSS_FLAG_HAS_VELOCITY;
        f.vel_enu = [1.2, -0.4, 0.1];
        f.vel_acc_m_s = 0.3;
        let d = GnssFix::decode(&f.encode()).unwrap();
        assert!(d.has_velocity());
        assert_eq!(d.vel_enu, [1.2, -0.4, 0.1]);
        assert_eq!(GNSS_FIX_LEN, 60);
        // out-of-range lat → invalid.
        let mut bad = f;
        bad.lat_deg = 91.0;
        assert!(!bad.is_valid());
    }

    #[test]
    fn baro_and_mag_round_trip() {
        let b = BaroSample {
            version: BARO_SAMPLE_VERSION,
            flags: 0,
            timestamp_us: 7,
            pressure_pa: 101_325.0,
            relative_altitude_m: 1.5,
            noise_std_m: 0.8,
        };
        assert_eq!(BaroSample::decode(&b.encode()), Some(b));
        assert!(b.is_finite());
        assert_eq!(BARO_SAMPLE_LEN, 24);

        let m = MagSample {
            version: MAG_SAMPLE_VERSION,
            flags: 0,
            timestamp_us: 9,
            field_ut: [22.0, -5.0, 41.0],
            noise_std_ut: 1.0,
        };
        assert_eq!(MagSample::decode(&m.encode()), Some(m));
        assert!(m.is_finite());
        assert_eq!(MAG_SAMPLE_LEN, 28);
        // bad reserved byte rejected.
        let mut bb = m.encode();
        bb[3] = 1;
        assert!(MagSample::decode(&bb).is_none());
    }
}
