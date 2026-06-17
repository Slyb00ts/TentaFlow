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
    /// One of: "move", "stop", "estop", "reset_estop", "recovery_stand",
    /// "stand_up", "stand_down", "sit", "hello", "stretch", "status".
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
}

impl RobotActionWire {
    pub fn simple(kind: &str) -> Self {
        Self { kind: kind.into(), vx: 0.0, vy: 0.0, vyaw: 0.0 }
    }
    pub fn move_to(vx: f64, vy: f64, vyaw: f64) -> Self {
        Self { kind: "move".into(), vx, vy, vyaw }
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
}
