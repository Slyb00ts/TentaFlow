// =============================================================================
// File: protocol/robot_joints.rs — canonical live joint state for robot animation.
// Purpose: the per-frame articulated pose of a robot (UNIFIED_SLAM_ARCHITECTURE
// §15). A device addon parses its vendor state (e.g. Go2 `rt/lf/lowstate`
// `motor_state[i].q`) into THIS fixed little-endian frame; the generic renderer
// drives forward kinematics from it against the addon-bundled glTF + joint
// manifest. Variable length: a fixed header + `joint_count` f32 angles (radians),
// in the manifest's joint order. Dependency-free (compiles into the wasm decoder).
// =============================================================================

/// Frame format version.
pub const ROBOT_JOINTS_VERSION: u8 = 1;
/// Fixed header length; the body is `joint_count * 4` little-endian f32 after it.
pub const ROBOT_JOINTS_HEADER_LEN: usize = 12;
/// Upper bound on joints (header field is a u8) — also a sanity cap on decode.
pub const ROBOT_JOINTS_MAX: usize = 255;

/// Parsed joint-state frame. Angles are radians, ordered to match the robot's joint
/// manifest (`robot_model.toml`).
#[derive(Debug, Clone, PartialEq)]
pub struct RobotJointsFrame {
    pub version: u8,
    pub timestamp_us: i64,
    /// Joint angles (radians), in manifest order. `len() == joint_count`.
    pub angles: Vec<f32>,
}

impl RobotJointsFrame {
    /// Serialize to the fixed header + packed f32 body. Panics only if `angles`
    /// exceeds `ROBOT_JOINTS_MAX` (a programming error — the count is a u8).
    pub fn encode(&self) -> Vec<u8> {
        assert!(self.angles.len() <= ROBOT_JOINTS_MAX, "too many joints");
        let mut b = Vec::with_capacity(ROBOT_JOINTS_HEADER_LEN + self.angles.len() * 4);
        b.push(self.version);
        b.push(self.angles.len() as u8);
        b.push(0); // reserved
        b.push(0); // reserved
        b.extend_from_slice(&self.timestamp_us.to_le_bytes());
        for a in &self.angles {
            b.extend_from_slice(&a.to_le_bytes());
        }
        b
    }

    /// Parse. `None` on a short slice, unknown version, non-zero reserved, or a body
    /// that does not match the declared `joint_count` (fail closed).
    pub fn decode(bytes: &[u8]) -> Option<RobotJointsFrame> {
        if bytes.len() < ROBOT_JOINTS_HEADER_LEN {
            return None;
        }
        if bytes[0] != ROBOT_JOINTS_VERSION {
            return None;
        }
        let joint_count = bytes[1] as usize;
        if bytes[2] != 0 || bytes[3] != 0 {
            return None;
        }
        let body_len = joint_count * 4;
        if bytes.len() != ROBOT_JOINTS_HEADER_LEN + body_len {
            return None;
        }
        let timestamp_us = i64::from_le_bytes([
            bytes[4], bytes[5], bytes[6], bytes[7], bytes[8], bytes[9], bytes[10], bytes[11],
        ]);
        let mut angles = Vec::with_capacity(joint_count);
        for i in 0..joint_count {
            let o = ROBOT_JOINTS_HEADER_LEN + i * 4;
            angles.push(f32::from_le_bytes([
                bytes[o],
                bytes[o + 1],
                bytes[o + 2],
                bytes[o + 3],
            ]));
        }
        Some(RobotJointsFrame {
            version: bytes[0],
            timestamp_us,
            angles,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn robot_joints_round_trip() {
        // Go2: 12 joints (FR/FL/RR/RL × hip/thigh/calf).
        let f = RobotJointsFrame {
            version: ROBOT_JOINTS_VERSION,
            timestamp_us: 1_700_000_000_000_001,
            angles: vec![
                0.1, -0.8, 1.5, -0.1, -0.8, 1.5, 0.1, -0.9, 1.4, -0.1, -0.9, 1.4,
            ],
        };
        let bytes = f.encode();
        assert_eq!(bytes.len(), ROBOT_JOINTS_HEADER_LEN + 12 * 4);
        assert_eq!(RobotJointsFrame::decode(&bytes), Some(f));
    }

    #[test]
    fn robot_joints_rejects_malformed() {
        let f = RobotJointsFrame {
            version: ROBOT_JOINTS_VERSION,
            timestamp_us: 1,
            angles: vec![0.0, 1.0, 2.0],
        };
        assert!(RobotJointsFrame::decode(&[0u8; 4]).is_none()); // short
        let mut b = f.encode();
        b[0] = 9; // bad version
        assert!(RobotJointsFrame::decode(&b).is_none());
        let mut b = f.encode();
        b[2] = 1; // reserved non-zero
        assert!(RobotJointsFrame::decode(&b).is_none());
        let mut b = f.encode();
        b.push(0); // body length mismatch vs joint_count
        assert!(RobotJointsFrame::decode(&b).is_none());
        let mut b = f.encode();
        b[1] = 7; // joint_count says 7 but body has 3
        assert!(RobotJointsFrame::decode(&b).is_none());
    }
}
