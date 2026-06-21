// =============================================================================
// File: protocol/global_pose.rs — canonical global-pose output frame.
// Purpose: the SINGLE output of the localization engine (UNIFIED_SLAM_ARCHITECTURE
// §2): a device's pose with honest uncertainty, in a FIXED little-endian binary
// layout (no JSON, like the LiDAR frame). `state` says how global the fix is:
// `Lost` (unobservable), `SceneLocal` (metric but not georeferenced → position is
// scene-frame metres), or `Global` (georeferenced → position is WGS84 lat/lon/alt).
// `source` is a bitmask of which sensors contributed, so consumers know what it
// rests on. Dependency-free so it also compiles into the browser wasm decoder.
// =============================================================================

/// Frame format version. Bump on any incompatible layout change.
pub const GLOBAL_POSE_VERSION: u8 = 1;

/// How global the fix is (the `state` byte).
pub const POSE_STATE_LOST: u8 = 0; // unobservable — position meaningless, trust covariance
pub const POSE_STATE_SCENE_LOCAL: u8 = 1; // metric, NOT georeferenced — position = scene metres
pub const POSE_STATE_GLOBAL: u8 = 2; // georeferenced — position = WGS84 lat/lon/alt

/// `source` bitmask bits — which sensors/processes contributed to this fix.
pub const POSE_SRC_LIDAR: u8 = 1 << 0;
pub const POSE_SRC_VISION: u8 = 1 << 1;
pub const POSE_SRC_IMU: u8 = 1 << 2;
pub const POSE_SRC_GNSS: u8 = 1 << 3;
pub const POSE_SRC_WIFI: u8 = 1 << 4;
pub const POSE_SRC_MAP: u8 = 1 << 5; // map-relative relocalization

/// Fixed header length (no body).
pub const GLOBAL_POSE_LEN: usize = 100;

/// Parsed/serializable global pose.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlobalPoseFrame {
    pub version: u8,
    pub state: u8,
    pub source: u8,
    pub timestamp_us: i64,
    pub scene_id: u64,
    /// `Global` → `[lat_deg, lon_deg, alt_m]`; otherwise scene-local `[x, y, z]` m.
    pub position: [f64; 3],
    /// Orientation in the scene frame as a unit quaternion `[x, y, z, w]`.
    pub quat_xyzw: [f64; 4],
    /// Diagonal covariance `[σ²x, σ²y, σ²z, σ²roll, σ²pitch, σ²yaw]` — the honest
    /// uncertainty (large / `Lost` ⇒ do not trust the position).
    pub cov_diag: [f32; 6],
}

impl GlobalPoseFrame {
    pub fn encode(&self) -> [u8; GLOBAL_POSE_LEN] {
        let mut b = [0u8; GLOBAL_POSE_LEN];
        b[0] = self.version;
        b[1] = self.state;
        b[2] = self.source;
        // byte 3 reserved.
        b[4..12].copy_from_slice(&self.timestamp_us.to_le_bytes());
        b[12..20].copy_from_slice(&self.scene_id.to_le_bytes());
        for (i, v) in self.position.iter().enumerate() {
            b[20 + i * 8..28 + i * 8].copy_from_slice(&v.to_le_bytes());
        }
        for (i, v) in self.quat_xyzw.iter().enumerate() {
            b[44 + i * 8..52 + i * 8].copy_from_slice(&v.to_le_bytes());
        }
        for (i, v) in self.cov_diag.iter().enumerate() {
            b[76 + i * 4..80 + i * 4].copy_from_slice(&v.to_le_bytes());
        }
        b
    }

    /// Parse, returning `None` on a short slice, unknown version, unknown state, or
    /// a non-zero reserved byte (fail closed, never misread).
    pub fn decode(bytes: &[u8]) -> Option<GlobalPoseFrame> {
        if bytes.len() < GLOBAL_POSE_LEN {
            return None;
        }
        if bytes[0] != GLOBAL_POSE_VERSION {
            return None;
        }
        let state = bytes[1];
        if state > POSE_STATE_GLOBAL {
            return None;
        }
        if bytes[3] != 0 {
            return None;
        }
        let rd8 = |o: usize| {
            let mut a = [0u8; 8];
            a.copy_from_slice(&bytes[o..o + 8]);
            a
        };
        let timestamp_us = i64::from_le_bytes(rd8(4));
        let scene_id = u64::from_le_bytes(rd8(12));
        let mut position = [0.0f64; 3];
        for (i, p) in position.iter_mut().enumerate() {
            *p = f64::from_le_bytes(rd8(20 + i * 8));
        }
        let mut quat_xyzw = [0.0f64; 4];
        for (i, q) in quat_xyzw.iter_mut().enumerate() {
            *q = f64::from_le_bytes(rd8(44 + i * 8));
        }
        let mut cov_diag = [0.0f32; 6];
        for (i, c) in cov_diag.iter_mut().enumerate() {
            let mut a = [0u8; 4];
            a.copy_from_slice(&bytes[76 + i * 4..80 + i * 4]);
            *c = f32::from_le_bytes(a);
        }
        Some(GlobalPoseFrame {
            version: bytes[0],
            state,
            source: bytes[2],
            timestamp_us,
            scene_id,
            position,
            quat_xyzw,
            cov_diag,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_pose_round_trips() {
        let f = GlobalPoseFrame {
            version: GLOBAL_POSE_VERSION,
            state: POSE_STATE_GLOBAL,
            source: POSE_SRC_LIDAR | POSE_SRC_MAP | POSE_SRC_IMU,
            timestamp_us: 1_700_000_000_000_123,
            scene_id: 42,
            position: [52.2297, 21.0122, 118.5],
            quat_xyzw: [0.0, 0.0, 0.382_683, 0.923_88],
            cov_diag: [0.01, 0.01, 0.02, 0.001, 0.001, 0.003],
        };
        let back = GlobalPoseFrame::decode(&f.encode()).expect("decode");
        assert_eq!(back, f);
        assert_eq!(GLOBAL_POSE_LEN, 100);
    }

    #[test]
    fn global_pose_rejects_bad_header() {
        let f = GlobalPoseFrame {
            version: GLOBAL_POSE_VERSION,
            state: POSE_STATE_SCENE_LOCAL,
            source: 0,
            timestamp_us: 1,
            scene_id: 1,
            position: [0.0; 3],
            quat_xyzw: [0.0, 0.0, 0.0, 1.0],
            cov_diag: [1.0; 6],
        };
        assert!(GlobalPoseFrame::decode(&[0u8; 10]).is_none()); // short
        let mut b = f.encode();
        b[0] = 9; // bad version
        assert!(GlobalPoseFrame::decode(&b).is_none());
        let mut b = f.encode();
        b[1] = 7; // unknown state
        assert!(GlobalPoseFrame::decode(&b).is_none());
        let mut b = f.encode();
        b[3] = 1; // reserved non-zero
        assert!(GlobalPoseFrame::decode(&b).is_none());
    }
}
