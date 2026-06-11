// =============================================================================
// Plik: addons/mcp/src/lib.rs
// Opis: Generyczny klient MCP (Model Context Protocol) jako narzedzia LLM.
//       Cala logika w tentaflow-addon-mcp-core; ten crate to entrypointy WASM
//       i branding "mcp".
// =============================================================================

use tentaflow_addon_mcp_core::{on_request_raw, Brand};
use tentaflow_addon_sdk::log;

const BRAND: Brand = Brand {
    tool_prefix: "mcp",
    client_name: "TentaFlow MCP",
};

#[no_mangle]
pub extern "C" fn on_install() -> i32 {
    log::info("mcp addon zainstalowany");
    0
}

#[no_mangle]
pub extern "C" fn on_start() -> i32 {
    log::info("mcp addon uruchomiony");
    0
}

#[no_mangle]
pub extern "C" fn on_stop() -> i32 {
    log::info("mcp addon zatrzymany");
    0
}

#[no_mangle]
pub extern "C" fn on_event(_event_ptr: i32, _event_len: i32) -> i32 {
    0
}

#[no_mangle]
pub extern "C" fn on_request(
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    on_request_raw(&BRAND, input_ptr, input_len, out_ptr, out_cap, out_len_ptr)
}
