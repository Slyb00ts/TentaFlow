// =============================================================================
// File: protocol/robot.rs — robot_dispatch.* host-function ABI payloads
// Wire contract for an addon that does NOT own a robot locally to route a typed,
// allowlisted control action to the node that physically owns it. The host turns
// `RobotActionWire` into the core `mesh::robot_control::RobotAction` and runs the
// existing `dispatch_robot_action` router (Local-execute / Remote-over-mesh).
//
// Encoded with minicbor (same lib as every other host-function ABI payload), so
// the addon SDK and the host share ONE serialization and there is no duplicated
// tool→action mapping outside `RobotAction::to_go2_call`.
// =============================================================================

use minicbor::{Decode, Encode};

/// The complete remote-control action surface, mirrored from the core
/// `RobotAction` allowlist. A flat `kind` discriminant plus the optional `Move`
/// axes keeps the CBOR map stable across SDK languages (no enum-index coupling).
/// The host rejects an unknown `kind`, so this can never become a free-form
/// "run any command" escape hatch.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct RobotActionWire {
    /// The action discriminant; see the core `RobotAction::from_kind_params`
    /// allowlist for the full closed set (move/stop/estop/poses/actions/etc.).
    #[n(0)]
    pub kind: String,
    /// `Move` body velocity (normalized -1..1). Ignored for non-move kinds; the
    /// host clamps these sender-side and the owner clamps again to its safety cap.
    #[n(1)]
    pub vx: f64,
    #[n(2)]
    pub vy: f64,
    #[n(3)]
    pub vyaw: f64,
    /// Generic numeric params for parametered poses/levels. Their meaning is keyed
    /// by `kind`: euler → (roll,pitch,yaw); body_height → (height,_,_);
    /// foot_raise → (height,_,_); speed_level → (level,_,_); pose →
    /// (roll,pitch,yaw) with `p4` carrying the body-height delta. Defaulted to 0
    /// for older senders / parameterless kinds. The owner clamps each to the
    /// documented Go2 range, so an out-of-range value never reaches the robot raw.
    #[n(4)]
    #[cbor(default)]
    pub p1: f64,
    #[n(5)]
    #[cbor(default)]
    pub p2: f64,
    #[n(6)]
    #[cbor(default)]
    pub p3: f64,
    #[n(7)]
    #[cbor(default)]
    pub p4: f64,
}

impl RobotActionWire {
    pub fn simple(kind: &str) -> Self {
        Self {
            kind: kind.into(),
            vx: 0.0,
            vy: 0.0,
            vyaw: 0.0,
            p1: 0.0,
            p2: 0.0,
            p3: 0.0,
            p4: 0.0,
        }
    }
    pub fn move_to(vx: f64, vy: f64, vyaw: f64) -> Self {
        Self {
            kind: "move".into(),
            vx,
            vy,
            vyaw,
            p1: 0.0,
            p2: 0.0,
            p3: 0.0,
            p4: 0.0,
        }
    }
    /// A parametered pose/level action carrying up to four generic numeric params.
    pub fn params(kind: &str, p1: f64, p2: f64, p3: f64, p4: f64) -> Self {
        Self {
            kind: kind.into(),
            vx: 0.0,
            vy: 0.0,
            vyaw: 0.0,
            p1,
            p2,
            p3,
            p4,
        }
    }
}

/// Input for `robot_dispatch_v1`: the logical robot id (the owning addon's robot)
/// and the action to apply. The host resolves the owner via the mesh robot
/// registry and either executes locally or forwards over the mesh.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct RobotDispatchInput {
    #[n(0)]
    pub robot_id: String,
    #[n(1)]
    pub action: RobotActionWire,
}

/// Output of `robot_dispatch_v1`, mirroring the core `RobotControlResponse`. A
/// transport / robot-level refusal is still a successful host call carrying the
/// reason; only an ABI-level failure (bad payload, missing permission) returns a
/// negative ABI error code.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct RobotControlResponseWire {
    #[n(0)]
    pub ok: bool,
    /// Present for `Status` (a JSON snapshot) or other result payloads.
    #[n(1)]
    pub result_json: Option<String>,
    /// Stable refusal tag (e.g. "unknown_robot", "permission_denied",
    /// "untrusted_peer", "expired") when the command was refused before execution.
    #[n(2)]
    pub rejected: Option<String>,
    /// Execution error message when the command ran but failed.
    #[n(3)]
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_input_roundtrip() {
        let v = RobotDispatchInput {
            robot_id: "go2".into(),
            action: RobotActionWire::move_to(0.3, -0.1, 0.2),
        };
        let mut buf = Vec::new();
        minicbor::encode(&v, &mut buf).unwrap();
        let back: RobotDispatchInput = minicbor::decode(&buf).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn response_roundtrip() {
        let v = RobotControlResponseWire {
            ok: false,
            result_json: None,
            rejected: Some("unknown_robot".into()),
            error: None,
        };
        let mut buf = Vec::new();
        minicbor::encode(&v, &mut buf).unwrap();
        let back: RobotControlResponseWire = minicbor::decode(&buf).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn simple_action_has_zero_velocity() {
        let a = RobotActionWire::simple("sit");
        assert_eq!(a.kind, "sit");
        assert_eq!((a.vx, a.vy, a.vyaw), (0.0, 0.0, 0.0));
    }

    #[test]
    fn params_roundtrip_preserves_all_four() {
        let v = RobotActionWire::params("pose", 0.1, -0.2, 0.3, -0.04);
        let mut buf = Vec::new();
        minicbor::encode(&v, &mut buf).unwrap();
        let back: RobotActionWire = minicbor::decode(&buf).unwrap();
        assert_eq!(v, back);
        assert_eq!(
            (back.p1, back.p2, back.p3, back.p4),
            (0.1, -0.2, 0.3, -0.04)
        );
    }

    /// An OLD sender that predates the p1..p4 fields only encodes keys 0..=3. The
    /// `#[cbor(default)]` on the new fields MUST let it decode with p1..p4 = 0.0
    /// (forward-compatible wire). We model the old wire with a struct that has only
    /// the original four keys, encoded with the SAME minicbor map layout.
    #[test]
    fn old_payload_without_params_decodes_with_zero_defaults() {
        #[derive(Encode)]
        #[cbor(map)]
        struct OldRobotActionWire {
            #[n(0)]
            kind: String,
            #[n(1)]
            vx: f64,
            #[n(2)]
            vy: f64,
            #[n(3)]
            vyaw: f64,
        }

        let old = OldRobotActionWire {
            kind: "euler".into(),
            vx: 0.0,
            vy: 0.0,
            vyaw: 0.0,
        };
        let mut buf = Vec::new();
        minicbor::encode(&old, &mut buf).unwrap();

        let back: RobotActionWire = minicbor::decode(&buf).unwrap();
        assert_eq!(back.kind, "euler");
        assert_eq!((back.p1, back.p2, back.p3, back.p4), (0.0, 0.0, 0.0, 0.0));
    }
}
