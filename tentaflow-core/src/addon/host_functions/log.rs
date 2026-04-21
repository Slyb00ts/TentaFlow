// =============================================================================
// Plik: addon/host_functions/log.rs
// Opis: Host functions Log API — logowanie z poziomu addonu WASM.
//       Logi addonu trafiaja do tracing z odpowiednim poziomem i kontekstem.
// =============================================================================

use tracing::{error, info, warn};

use super::{get_memory, read_guest_string, AddonState, WasmCaller, ABI_ERR_OPERATION, ABI_OK};

// =============================================================================
// log_info — log na poziomie INFO
// =============================================================================

/// Host function: loguje wiadomosc na poziomie INFO.
///
/// ABI:
/// - msg_ptr/msg_len: wiadomosc UTF-8
/// - Zwraca: ABI_OK lub kod bledu
pub fn log_info(mut caller: WasmCaller<'_, AddonState>, msg_ptr: i32, msg_len: i32) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return ABI_ERR_OPERATION,
    };

    let msg = match read_guest_string(&memory, &caller, msg_ptr, msg_len) {
        Some(s) => s.to_string(),
        None => return ABI_ERR_OPERATION,
    };

    let addon_id = &caller.data().addon_id;
    let instance_id = &caller.data().instance_id;

    info!(
        addon_id = %addon_id,
        instance_id = %instance_id,
        "[ADDON] {}",
        msg
    );

    ABI_OK
}

// =============================================================================
// log_warn — log na poziomie WARN
// =============================================================================

/// Host function: loguje wiadomosc na poziomie WARN.
///
/// ABI:
/// - msg_ptr/msg_len: wiadomosc UTF-8
/// - Zwraca: ABI_OK lub kod bledu
pub fn log_warn(mut caller: WasmCaller<'_, AddonState>, msg_ptr: i32, msg_len: i32) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return ABI_ERR_OPERATION,
    };

    let msg = match read_guest_string(&memory, &caller, msg_ptr, msg_len) {
        Some(s) => s.to_string(),
        None => return ABI_ERR_OPERATION,
    };

    let addon_id = &caller.data().addon_id;
    let instance_id = &caller.data().instance_id;

    warn!(
        addon_id = %addon_id,
        instance_id = %instance_id,
        "[ADDON] {}",
        msg
    );

    ABI_OK
}

// =============================================================================
// log_error — log na poziomie ERROR
// =============================================================================

/// Host function: loguje wiadomosc na poziomie ERROR.
///
/// ABI:
/// - msg_ptr/msg_len: wiadomosc UTF-8
/// - Zwraca: ABI_OK lub kod bledu
pub fn log_error(mut caller: WasmCaller<'_, AddonState>, msg_ptr: i32, msg_len: i32) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return ABI_ERR_OPERATION,
    };

    let msg = match read_guest_string(&memory, &caller, msg_ptr, msg_len) {
        Some(s) => s.to_string(),
        None => return ABI_ERR_OPERATION,
    };

    let addon_id = &caller.data().addon_id;
    let instance_id = &caller.data().instance_id;

    error!(
        addon_id = %addon_id,
        instance_id = %instance_id,
        "[ADDON] {}",
        msg
    );

    ABI_OK
}
