// =============================================================================
// File: addon/host_functions/robot.rs
// Purpose: robot_dispatch.* host ABI — lets an addon that does NOT own a robot
//          locally route a typed, allowlisted control action to the node that
//          physically owns it. A thin wrapper over the existing
//          `mesh::robot_dispatch::dispatch_robot_action` router: Core stays a
//          dumb pipe, all robot semantics live in the addon + the shared robot
//          control/dispatch modules. There is no free-form "call any tool" path;
//          only the `RobotAction` allowlist crosses the boundary.
// =============================================================================

#![allow(clippy::too_many_arguments)]

use tentaflow_sdk_spec::{RobotControlResponseWire, RobotDispatchInput};

use super::abi_helpers::PayloadKind;
use super::cbor_io::{read_input_cbor, write_cbor_capped};
use super::{audit_log_with_risk, check_permission, get_memory, AddonState, WasmCaller};
use crate::addon::errors::AbiError;
use crate::audit::RiskClass;
use crate::mesh::robot_control::{RejectReason, RobotAction, RobotControlResponse};

/// Addon capability to route robot actions across the mesh to the owning node.
/// Distinct from `webrtc.connect` (local-channel control only) — cross-node
/// dispatch is a separate high-risk surface the admin approves explicitly.
const PERM_ROBOT_CONTROL: &str = "robot.control";

fn audit(state: &AddonState, resource: Option<&str>, result: &str, reason: Option<&str>) {
    audit_log_with_risk(
        state,
        "robot.dispatch",
        Some("robot"),
        resource,
        RiskClass::A,
        None,
        None,
        result,
        reason,
    );
}

/// Stable refusal tag for the wire response (greppable, language-neutral).
fn reject_tag(reason: &RejectReason) -> &'static str {
    match reason {
        RejectReason::Expired => "expired",
        RejectReason::FutureDated => "future_dated",
        RejectReason::Malformed => "malformed",
        RejectReason::MoveDurationTooLong => "move_duration_too_long",
        RejectReason::PermissionDenied => "permission_denied",
        RejectReason::EstopActive => "estop_active",
        RejectReason::UntrustedPeer => "untrusted_peer",
        RejectReason::UnknownRobot => "unknown_robot",
    }
}

fn to_wire_response(resp: RobotControlResponse) -> RobotControlResponseWire {
    RobotControlResponseWire {
        ok: resp.ok,
        result_json: resp.result_json,
        rejected: resp.rejected.as_ref().map(|r| reject_tag(r).to_string()),
        error: resp.error,
    }
}

pub fn robot_dispatch_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    // Decode BEFORE the permission gate so an e-stop is never silently dropped by
    // the addon-level `robot.control` check: an e-stop must always reach the owner
    // (the receiver still enforces `robot.estop` RBAC on the acting user).
    let input: RobotDispatchInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::ServiceCall,
    ) {
        Ok(v) => v,
        Err(e) => {
            audit(caller.data(), None, "error", Some("invalid_payload"));
            return e.as_i32();
        }
    };

    let action = match RobotAction::from_wire(&input.action) {
        Some(a) => a,
        None => {
            audit(
                caller.data(),
                Some(&input.robot_id),
                "denied",
                Some("unknown_action_kind"),
            );
            return AbiError::Operation.as_i32();
        }
    };

    // Non-e-stop actions require the addon's high-risk `robot.control` capability.
    // E-stop-class actions bypass this sender-side gate by design — the owner node
    // re-authorizes them against the acting user's `robot.estop` permission, which
    // is the authoritative check; the addon gate must not be able to block a stop.
    if !action.is_estop_class() && !check_permission(caller.data(), PERM_ROBOT_CONTROL, None) {
        audit(
            caller.data(),
            Some(&input.robot_id),
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }

    // Identity must be a REAL acting user + org — never fabricated. The receiver's
    // RBAC (`PermissionMatrix::has_permission`) keys on these, so a fabricated
    // addon-id/default-org identity could wrongly deny a valid command or, worse,
    // authorize a system call. Panel/flow actions always carry the user (call_tool
    // threads it); a system/service-tick call (no user) must not actuate a robot.
    let actor_user_id = match caller.data().user_id.clone() {
        Some(u) => u,
        None => {
            audit(
                caller.data(),
                Some(&input.robot_id),
                "denied",
                Some("no_user_context"),
            );
            return AbiError::Permission.as_i32();
        }
    };
    let org_id = match caller.data().org_id.clone() {
        Some(o) => o,
        None => {
            audit(
                caller.data(),
                Some(&input.robot_id),
                "error",
                Some("no_org_context"),
            );
            return AbiError::Operation.as_i32();
        }
    };
    let db = caller.data().db.clone();

    // The dispatch router itself runs the wasmtime local-execute on a blocking
    // task, so this host call must not be holding the wasmtime store across an
    // await — `run_async` drives the future to completion on the current thread.
    let resp = run_async(async {
        crate::mesh::robot_dispatch::dispatch_robot_action_global(
            action,
            &input.robot_id,
            &actor_user_id,
            &org_id,
            &db,
        )
        .await
    });
    let resp = match resp {
        Some(r) => r,
        None => {
            // Context not wired (mesh not started) — there is no node to route to.
            audit(
                caller.data(),
                Some(&input.robot_id),
                "error",
                Some("dispatch_context_unavailable"),
            );
            return AbiError::Operation.as_i32();
        }
    };

    let outcome = if resp.ok {
        "ok"
    } else if resp.rejected.is_some() {
        "rejected"
    } else {
        "error"
    };
    audit(caller.data(), Some(&input.robot_id), outcome, None);

    let out = to_wire_response(resp);
    write_cbor_capped(
        &memory,
        &mut caller,
        &out,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::ServiceCall,
    )
}

fn run_async<F, T>(fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
}
