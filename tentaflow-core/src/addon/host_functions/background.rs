// ===== File: addon/host_functions/background.rs — run one of the addon's own tools in the background =====
//
// ABI: (tool_ptr, tool_len, params_json_ptr, params_json_len) -> i32
//   ABI_OK              — the run started; it reports through the addon's own state
//   ABI_ERR_RATE_LIMIT  — that tool is still running from an earlier request
//   ABI_ERR_OPERATION   — bad arguments, or this process cannot run background work
//
// Only the calling addon's own tools: the run is a system call into code the
// addon could already execute itself, so no permission is involved — what it
// gains is a thread that is not its service tick (see `addon::background`).

use super::{audit_log, get_memory, read_guest_string, AddonState, WasmCaller, ABI_ERR_OPERATION, ABI_ERR_RATE_LIMIT, ABI_OK};
use crate::addon::background::{run_own_tool, Spawn};

pub fn tool_run_in_background_v1(
    mut caller: WasmCaller<'_, AddonState>,
    tool_ptr: i32,
    tool_len: i32,
    params_ptr: i32,
    params_len: i32,
) -> i32 {
    let Some(memory) = get_memory(&mut caller) else {
        return ABI_ERR_OPERATION;
    };
    let Some(tool) = read_guest_string(&memory, &caller, tool_ptr, tool_len).map(str::to_string) else {
        return ABI_ERR_OPERATION;
    };
    let params = if params_len > 0 {
        match read_guest_string(&memory, &caller, params_ptr, params_len)
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        {
            Some(v) => v,
            None => return ABI_ERR_OPERATION,
        }
    } else {
        serde_json::json!({})
    };
    let state = caller.data();
    match run_own_tool(&state.addon_id, &tool, params) {
        Spawn::Started => {
            audit_log(state, "tool.run_in_background", Some("tool"), Some(&tool), "started", None);
            ABI_OK
        }
        Spawn::AlreadyRunning => ABI_ERR_RATE_LIMIT,
        Spawn::Unavailable => ABI_ERR_OPERATION,
    }
}
