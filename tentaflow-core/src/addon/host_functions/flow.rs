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
// Wire format: TOML on both sides. The output of invoke/status mirrors
// the `InvocationStatus` shape returned by `FlowScheduler` so the addon
// SDK can deserialize a single struct regardless of which call produced
// the row.

use serde::{Deserialize, Serialize};

use super::abi_helpers::{enforce_payload_size, write_output_with_retry_semantics, PayloadKind};
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
// Wire payloads
// =============================================================================

#[derive(Debug, Deserialize)]
struct FlowInvokeInput {
    flow_id: String,
    /// Opaque payload forwarded to operators as `OperatorContext.input_toml`.
    /// Defaults to an empty TOML table when omitted so an addon can fire a
    /// trigger-only flow with `{ flow_id = "..." }`.
    #[serde(default = "empty_toml_table")]
    input: toml::Value,
    /// 0 = async (returns immediately with `status='running'`).
    /// >0 = sync up to `MAX_SYNC_WAIT_MS` (clamped silently).
    #[serde(default)]
    wait_ms: u32,
}

fn empty_toml_table() -> toml::Value {
    toml::Value::Table(toml::value::Table::new())
}

#[derive(Debug, Deserialize)]
struct FlowIdInput {
    invocation_id: String,
}

#[derive(Debug, Serialize)]
pub struct FlowInvocationOutput {
    pub invocation_id: String,
    pub status: String,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    pub operators_completed: i64,
    pub operators_total: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_toml: Option<String>,
}

impl From<InvocationStatus> for FlowInvocationOutput {
    fn from(s: InvocationStatus) -> Self {
        Self {
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
}

#[derive(Debug, Serialize)]
pub struct FlowCancelOutput {
    pub cancelled: bool,
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
// + status text); the Secret bucket's 64 KiB ceiling is the right default.
// Input is read once into a string; output is encoded via the standard
// out_cap retry helper so the SDK can resize on `OutputBufferTooSmall`.
// =============================================================================

fn read_toml_input(
    memory: &super::super::runtime::WasmMemory,
    caller: &WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
) -> Result<String, AbiError> {
    if input_len < 0 {
        return Err(AbiError::Operation);
    }
    if enforce_payload_size(input_len as usize, PayloadKind::Secret).is_err() {
        return Err(AbiError::PayloadTooLarge);
    }
    let bytes =
        read_guest_bytes(memory, caller, input_ptr, input_len).ok_or(AbiError::Operation)?;
    std::str::from_utf8(bytes)
        .map(|s| s.to_string())
        .map_err(|_| AbiError::Operation)
}

fn write_toml_output<T: Serialize>(
    memory: &super::super::runtime::WasmMemory,
    caller: &mut WasmCaller<'_, AddonState>,
    value: &T,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let serialized = match toml::to_string(value) {
        Ok(s) => s,
        Err(_) => return AbiError::Operation.as_i32(),
    };
    if enforce_payload_size(serialized.len(), PayloadKind::Secret).is_err() {
        return AbiError::PayloadTooLarge.as_i32();
    }
    write_output_with_retry_semantics(
        memory,
        caller,
        serialized.as_bytes(),
        out_ptr,
        out_cap,
        out_len_ptr,
    )
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
// The host-fn shells reduce to: get memory → read TOML → dispatch → write
// TOML → return ABI code.

/// Outcome of a dispatch: either a typed AbiError (already audited) or a
/// concrete output struct ready to be serialized to TOML.
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
    toml_str: &str,
) -> DispatchOutcome<FlowInvocationOutput> {
    let input: FlowInvokeInput = match toml::from_str(toml_str) {
        Ok(v) => v,
        Err(_) => {
            audit(state, ACTION_INVOKE, None, "denied", Some("bad_input"));
            return DispatchOutcome::Err(AbiError::Operation);
        }
    };

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

    // P7: per-user audit attribution — propagate the operator's user_id from
    // AddonState into flow_invocations so DoD-9 / DoD-10 reports can attribute
    // the invocation to the human actor instead of `actor=system`. System
    // callers (is_system_call=true with no user_id) record NULL.
    let actor_user_id = state.user_id;
    let org_id = state.org_id.clone();
    match run_invoke(
        scheduler,
        &state.addon_id,
        &input.flow_id,
        input.input.clone(),
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
    actor_user_id: Option<i64>,
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
        Ok(status) => Ok(FlowInvocationOutput::from(status)),
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
        Ok(status) => Ok(FlowInvocationOutput::from(status)),
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
    toml_str: &str,
) -> DispatchOutcome<FlowInvocationOutput> {
    let input: FlowIdInput = match toml::from_str(toml_str) {
        Ok(v) => v,
        Err(_) => {
            audit(state, ACTION_STATUS, None, "denied", Some("bad_input"));
            return DispatchOutcome::Err(AbiError::Operation);
        }
    };

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
    toml_str: &str,
) -> DispatchOutcome<FlowCancelOutput> {
    let input: FlowIdInput = match toml::from_str(toml_str) {
        Ok(v) => v,
        Err(_) => {
            audit(state, ACTION_CANCEL, None, "denied", Some("bad_input"));
            return DispatchOutcome::Err(AbiError::Operation);
        }
    };

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

    let toml_str = match read_toml_input(&memory, &caller, input_ptr, input_len) {
        Ok(s) => s,
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
    match dispatch_invoke(caller.data(), &scheduler, &toml_str) {
        DispatchOutcome::Ok(out) => {
            write_toml_output(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
        }
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

    let toml_str = match read_toml_input(&memory, &caller, input_ptr, input_len) {
        Ok(s) => s,
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
    match dispatch_status(caller.data(), &scheduler, &toml_str) {
        DispatchOutcome::Ok(out) => {
            write_toml_output(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
        }
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

    let toml_str = match read_toml_input(&memory, &caller, input_ptr, input_len) {
        Ok(s) => s,
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
    match dispatch_cancel(caller.data(), &scheduler, &toml_str) {
        DispatchOutcome::Ok(out) => {
            write_toml_output(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
        }
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
        run_invoke, run_status, DispatchOutcome, FlowCancelOutput, FlowInvocationOutput,
        PERM_FLOW_INVOKE,
    };
}
