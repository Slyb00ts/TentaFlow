// ============ File: addon/host_functions/flow.rs — F1c P5 flow_invoke / status / cancel ============
//
// Three host functions exposing the `flow_runtime` DAG executor to addons:
//
//   * `flow_invoke_v1(flow_id, input, wait_ms)` — start a flow declared in
//     the addon manifest. `wait_ms = 0` returns the live `running` status
//     immediately; values > 0 await completion up to `MAX_SYNC_WAIT_MS`
//     (30 s) before returning the current row from `flow_invocations`.
//   * `flow_status_v1(invocation_id)` — read the authoritative DB row for
//     one invocation. Cross-addon lookups return NotFound (the scheduler
//     filters every query by `addon_id`).
//   * `flow_cancel_v1(invocation_id)` — request cooperative cancellation.
//     Idempotent: cancelling a finished invocation is a quiet success.
//
// All three calls require the `flow.invoke` permission (manifest-declared
// like every other addon permission — no global catalog seed). Risk class
// is `B` for invoke (flow operators may emit events / call services) and
// `B` for status/cancel as well (the operations expose RODO-tracked DAG
// state).
//
// Wire format: CBOR on both sides (structs in `tentaflow-sdk-spec`). The opaque
// flow-operator payload still rides inside the CBOR input/output as TOML text
// (`input_toml` / `result_toml`) — that is the flow runtime's own data-plane
// contract, not the host-fn ABI format. The output of invoke/status mirrors the
// `InvocationStatus` shape returned by `FlowScheduler` so the addon SDK can
// decode a single struct regardless of which call produced the row.

use tentaflow_sdk_spec::{
    FlowCancelOutput, FlowInvocationIdInput, FlowInvocationOutput, FlowInvokeInput,
};

use super::abi_helpers::PayloadKind;
use super::cbor_io::{decode_cbor_exact, write_cbor_capped};
use super::{
    audit_log_with_risk, check_permission, get_memory, read_guest_bytes, AddonState, WasmCaller,
};
use crate::addon::errors::AbiError;
use crate::audit::RiskClass;
use crate::flow_runtime::scheduler::{
    FlowScheduler, InvocationStatus, InvokeError, MAX_SYNC_WAIT_MS,
};

// =============================================================================
// Permission + audit constants
// =============================================================================

pub const PERM_FLOW_INVOKE: &str = "flow.invoke";

const ACTION_INVOKE: &str = "flow.invoke";
const ACTION_STATUS: &str = "flow.status";
const ACTION_CANCEL: &str = "flow.cancel";

// =============================================================================
// Wire payloads — defined in `tentaflow-sdk-spec` (CBOR). The flow-operator
// data plane (`input_toml` / `result_toml`) crosses inside those structs as
// opaque TOML text; it is the flow runtime's own contract, not the host-fn ABI
// serialization format. The conversion to the scheduler's `toml::Value`
// happens at this boundary only.
// =============================================================================

fn invocation_output_from_status(s: InvocationStatus) -> FlowInvocationOutput {
    FlowInvocationOutput {
        invocation_id: s.invocation_id,
        status: s.status,
        started_at: s.started_at,
        finished_at: s.finished_at,
        operators_completed: s.operators_completed,
        operators_total: s.operators_total,
        error: s.error,
        result_toml: s.result_toml,
    }
}

// =============================================================================
// Audit helpers
// =============================================================================

fn audit(
    state: &AddonState,
    action: &str,
    resource_id: Option<&str>,
    result: &str,
    reason: Option<&str>,
) {
    audit_log_with_risk(
        state,
        action,
        Some("flow"),
        resource_id,
        RiskClass::B,
        None,
        None,
        result,
        reason,
    );
}

// =============================================================================
// Wire framing — payloads are typically a few hundred bytes (invocation ids
// + status text); the Secret bucket's 64 KiB ceiling is the right input cap.
// Input is read + decoded from CBOR by the size-checking shared helper; output
// is encoded via the standard out_cap retry helper so the SDK can resize on
// `OutputBufferTooSmall`. The Secret-bucket input check is applied here before
// decoding because the shared `read_input_cbor` ServiceCall ceiling would be
// looser than the historical 64 KiB flow input cap.
// =============================================================================

fn read_flow_input<T>(
    memory: &super::super::runtime::WasmMemory,
    caller: &WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
) -> Result<T, AbiError>
where
    T: for<'b> minicbor::Decode<'b, ()>,
{
    if input_len < 0 {
        return Err(AbiError::Operation);
    }
    if super::abi_helpers::enforce_payload_size(input_len as usize, PayloadKind::Secret).is_err() {
        return Err(AbiError::PayloadTooLarge);
    }
    let bytes =
        read_guest_bytes(memory, caller, input_ptr, input_len).ok_or(AbiError::Operation)?;
    decode_cbor_exact(bytes)
}

// =============================================================================
// Pure dispatch helpers
// =============================================================================
//
// The four `dispatch_*` functions encapsulate every step that does NOT touch
// the wasmtime caller: input parsing, permission check, scheduler call, and
// audit emission. Keeping them out of the host-fn body lets integration
// tests exercise the full flow path without needing a real WasmCaller (see
// `tests/flow_host_functions.rs`).
//
// The host-fn shells reduce to: get memory → permission → decode CBOR →
// dispatch → write CBOR → return ABI code.

/// Outcome of a dispatch: either a typed AbiError (already audited) or a
/// concrete output struct ready to be encoded to CBOR.
pub enum DispatchOutcome<T> {
    Ok(T),
    Err(AbiError),
}

/// Maps an `InvokeError` raised by the scheduler to an `AbiError` + audit
/// reason. Keeps the error → audit mapping in one place so the three host
/// functions cannot drift.
pub fn map_invoke_error(err: &InvokeError) -> (AbiError, &'static str) {
    match err {
        InvokeError::FlowNotFound { .. } => (AbiError::NotFound, "flow_not_found"),
        InvokeError::NotFound(_) => (AbiError::NotFound, "invocation_not_found"),
        InvokeError::ConcurrencyCapExceeded { .. } => (AbiError::QuotaExceeded, "concurrency_cap"),
        InvokeError::Db(_) => (AbiError::Operation, "db_error"),
        InvokeError::Internal(_) => (AbiError::Operation, "internal"),
    }
}

/// Runs the scheduler call on the current tokio runtime. The host-fn entry
/// points are sync wasmtime callbacks executing on a tokio worker thread —
/// `block_in_place` lets us re-enter the runtime without parking the worker.
fn block_on_runtime<F, T>(fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
}

pub fn dispatch_invoke(
    state: &AddonState,
    scheduler: &std::sync::Arc<FlowScheduler>,
    input: &FlowInvokeInput,
) -> DispatchOutcome<FlowInvocationOutput> {
    if !check_permission(state, PERM_FLOW_INVOKE, None) {
        audit(
            state,
            ACTION_INVOKE,
            Some(&input.flow_id),
            "denied",
            Some("missing_permission"),
        );
        return DispatchOutcome::Err(AbiError::Permission);
    }

    // The opaque operator payload arrives as TOML text inside the CBOR input;
    // parse it into the scheduler's `toml::Value` here at the ABI boundary. An
    // empty/absent blob collapses to an empty TOML table (trigger-only flow).
    let input_text = input.input_toml_or_empty().trim();
    let operator_input = if input_text.is_empty() {
        toml::Value::Table(toml::value::Table::new())
    } else {
        match toml::from_str::<toml::Value>(input_text) {
            Ok(v) => v,
            Err(_) => {
                audit(
                    state,
                    ACTION_INVOKE,
                    Some(&input.flow_id),
                    "denied",
                    Some("bad_input"),
                );
                return DispatchOutcome::Err(AbiError::Operation);
            }
        }
    };

    // P7: per-user audit attribution — propagate the operator's user_id from
    // AddonState into flow_invocations so DoD-9 / DoD-10 reports can attribute
    // the invocation to the human actor instead of `actor=system`. System
    // callers (is_system_call=true with no user_id) record NULL.
    let actor_user_id = state.user_id.clone();
    let org_id = state.org_id.clone();
    match run_invoke(
        scheduler,
        &state.addon_id,
        &input.flow_id,
        operator_input,
        input.wait_ms,
        actor_user_id,
        org_id,
    ) {
        Ok(out) => {
            audit(
                state,
                ACTION_INVOKE,
                Some(&input.flow_id),
                "ok",
                Some(out.status.as_str()),
            );
            DispatchOutcome::Ok(out)
        }
        Err((abi, reason)) => {
            audit(
                state,
                ACTION_INVOKE,
                Some(&input.flow_id),
                "denied",
                Some(reason),
            );
            DispatchOutcome::Err(abi)
        }
    }
}

/// Pure invoke path — used by the host-fn dispatcher AND by integration
/// tests that build their own scheduler. Skips permission + audit so tests
/// can target the scheduler→ABI mapping in isolation.
pub fn run_invoke(
    scheduler: &std::sync::Arc<FlowScheduler>,
    addon_id: &str,
    flow_id: &str,
    input: toml::Value,
    wait_ms: u32,
    actor_user_id: Option<String>,
    org_id: Option<String>,
) -> Result<FlowInvocationOutput, (AbiError, &'static str)> {
    let wait_ms = wait_ms.min(MAX_SYNC_WAIT_MS);
    let res = block_on_runtime(scheduler.invoke(
        addon_id,
        flow_id,
        input,
        wait_ms,
        actor_user_id,
        org_id,
    ));
    match res {
        Ok(status) => Ok(invocation_output_from_status(status)),
        Err(e) => Err(map_invoke_error(&e)),
    }
}

/// Pure status path — see `run_invoke` for the rationale.
pub fn run_status(
    scheduler: &std::sync::Arc<FlowScheduler>,
    invocation_id: &str,
    addon_id: &str,
) -> Result<FlowInvocationOutput, (AbiError, &'static str)> {
    let res = block_on_runtime(scheduler.status(invocation_id, addon_id));
    match res {
        Ok(status) => Ok(invocation_output_from_status(status)),
        Err(e) => Err(map_invoke_error(&e)),
    }
}

/// Pure cancel path — see `run_invoke` for the rationale.
pub fn run_cancel(
    scheduler: &std::sync::Arc<FlowScheduler>,
    invocation_id: &str,
    addon_id: &str,
) -> Result<FlowCancelOutput, (AbiError, &'static str)> {
    let res = block_on_runtime(scheduler.cancel(invocation_id, addon_id));
    match res {
        Ok(()) => Ok(FlowCancelOutput { cancelled: true }),
        Err(e) => Err(map_invoke_error(&e)),
    }
}

pub fn dispatch_status(
    state: &AddonState,
    scheduler: &std::sync::Arc<FlowScheduler>,
    input: &FlowInvocationIdInput,
) -> DispatchOutcome<FlowInvocationOutput> {
    if !check_permission(state, PERM_FLOW_INVOKE, None) {
        audit(
            state,
            ACTION_STATUS,
            Some(&input.invocation_id),
            "denied",
            Some("missing_permission"),
        );
        return DispatchOutcome::Err(AbiError::Permission);
    }

    match run_status(scheduler, &input.invocation_id, &state.addon_id) {
        Ok(out) => {
            audit(state, ACTION_STATUS, Some(&input.invocation_id), "ok", None);
            DispatchOutcome::Ok(out)
        }
        Err((abi, reason)) => {
            audit(
                state,
                ACTION_STATUS,
                Some(&input.invocation_id),
                "denied",
                Some(reason),
            );
            DispatchOutcome::Err(abi)
        }
    }
}

pub fn dispatch_cancel(
    state: &AddonState,
    scheduler: &std::sync::Arc<FlowScheduler>,
    input: &FlowInvocationIdInput,
) -> DispatchOutcome<FlowCancelOutput> {
    if !check_permission(state, PERM_FLOW_INVOKE, None) {
        audit(
            state,
            ACTION_CANCEL,
            Some(&input.invocation_id),
            "denied",
            Some("missing_permission"),
        );
        return DispatchOutcome::Err(AbiError::Permission);
    }

    match run_cancel(scheduler, &input.invocation_id, &state.addon_id) {
        Ok(out) => {
            audit(state, ACTION_CANCEL, Some(&input.invocation_id), "ok", None);
            DispatchOutcome::Ok(out)
        }
        Err((abi, reason)) => {
            audit(
                state,
                ACTION_CANCEL,
                Some(&input.invocation_id),
                "denied",
                Some(reason),
            );
            DispatchOutcome::Err(abi)
        }
    }
}

// =============================================================================
// Host function entry points
// =============================================================================

/// ABI: (input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32
pub fn flow_invoke_v1(
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

    // Capability check is decode-independent (it never inspects the flow id),
    // so run it before reading/decoding the guest CBOR. This keeps the
    // permission boundary ahead of attacker-controlled payload parsing, matching
    // camera/streaming/recording/vector host fns. The decode-dependent ownership
    // checks (flow_id resolution, invocation ownership) still run later inside
    // dispatch_invoke. dispatch_invoke re-checks the same permission because it
    // is also called directly from integration tests; the early return here
    // means the denied path never reaches that second check, so no double audit.
    if !check_permission(caller.data(), PERM_FLOW_INVOKE, None) {
        audit(
            caller.data(),
            ACTION_INVOKE,
            None,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }

    let input: FlowInvokeInput = match read_flow_input(&memory, &caller, input_ptr, input_len) {
        Ok(v) => v,
        Err(e) => {
            audit(
                caller.data(),
                ACTION_INVOKE,
                None,
                "denied",
                Some("payload_invalid"),
            );
            return e.as_i32();
        }
    };

    let scheduler = FlowScheduler::global();
    match dispatch_invoke(caller.data(), &scheduler, &input) {
        DispatchOutcome::Ok(out) => write_cbor_capped(
            &memory,
            &mut caller,
            &out,
            out_ptr,
            out_cap,
            out_len_ptr,
            PayloadKind::Secret,
        ),
        DispatchOutcome::Err(e) => e.as_i32(),
    }
}

/// ABI: (input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32
pub fn flow_status_v1(
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

    // Permission boundary ahead of CBOR decode (see flow_invoke_v1). The
    // decode-dependent ownership filter (per-addon invocation lookup) stays in
    // dispatch_status. The early return prevents the denied path from reaching
    // dispatch_status's own check, so there is no double audit.
    if !check_permission(caller.data(), PERM_FLOW_INVOKE, None) {
        audit(
            caller.data(),
            ACTION_STATUS,
            None,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }

    let input: FlowInvocationIdInput = match read_flow_input(&memory, &caller, input_ptr, input_len)
    {
        Ok(v) => v,
        Err(e) => {
            audit(
                caller.data(),
                ACTION_STATUS,
                None,
                "denied",
                Some("payload_invalid"),
            );
            return e.as_i32();
        }
    };

    let scheduler = FlowScheduler::global();
    match dispatch_status(caller.data(), &scheduler, &input) {
        DispatchOutcome::Ok(out) => write_cbor_capped(
            &memory,
            &mut caller,
            &out,
            out_ptr,
            out_cap,
            out_len_ptr,
            PayloadKind::Secret,
        ),
        DispatchOutcome::Err(e) => e.as_i32(),
    }
}

/// ABI: (input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32
pub fn flow_cancel_v1(
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

    // Permission boundary ahead of CBOR decode (see flow_invoke_v1). The
    // decode-dependent ownership filter (per-addon invocation lookup) stays in
    // dispatch_cancel. The early return prevents the denied path from reaching
    // dispatch_cancel's own check, so there is no double audit.
    if !check_permission(caller.data(), PERM_FLOW_INVOKE, None) {
        audit(
            caller.data(),
            ACTION_CANCEL,
            None,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }

    let input: FlowInvocationIdInput = match read_flow_input(&memory, &caller, input_ptr, input_len)
    {
        Ok(v) => v,
        Err(e) => {
            audit(
                caller.data(),
                ACTION_CANCEL,
                None,
                "denied",
                Some("payload_invalid"),
            );
            return e.as_i32();
        }
    };

    let scheduler = FlowScheduler::global();
    match dispatch_cancel(caller.data(), &scheduler, &input) {
        DispatchOutcome::Ok(out) => write_cbor_capped(
            &memory,
            &mut caller,
            &out,
            out_ptr,
            out_cap,
            out_len_ptr,
            PayloadKind::Secret,
        ),
        DispatchOutcome::Err(e) => e.as_i32(),
    }
}

// =============================================================================
// Test surface — exposes the pure dispatch helpers + error-mapping for
// integration tests that bypass the wasmtime caller.
// =============================================================================

#[doc(hidden)]
pub mod test_api {
    pub use super::{
        dispatch_cancel, dispatch_invoke, dispatch_status, map_invoke_error, run_cancel,
        run_invoke, run_status, DispatchOutcome, PERM_FLOW_INVOKE,
    };
    pub use tentaflow_sdk_spec::{
        FlowCancelOutput, FlowInvocationIdInput, FlowInvocationOutput, FlowInvokeInput,
    };
}
