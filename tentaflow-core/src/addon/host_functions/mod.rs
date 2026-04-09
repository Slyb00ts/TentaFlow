// =============================================================================
// Plik: addon/host_functions/mod.rs
// Opis: Rejestracja host functions w Wasmtime Linker — definiuje API dostepne
//       dla addonow WASM. Kazda host function sprawdza uprawnienia, loguje
//       do audit trail i operuje na liniowej pamieci WASM.
// =============================================================================

pub mod llm;
pub mod storage;
pub mod http;
pub mod events;
pub mod ui;
pub mod user;
pub mod secrets;
pub mod log;
pub mod network;
pub mod service;

use anyhow::{Context, Result};

use super::AddonState;
use super::runtime::{WasmLinker, WasmCaller, WasmMemory, AsContext, AsContextMut};

// =============================================================================
// Kody bledow ABI (zwracane przez host functions)
// =============================================================================

/// Sukces
pub const ABI_OK: i32 = 0;
/// Brak uprawnien
pub const ABI_ERR_PERMISSION: i32 = -1;
/// Blad operacji
pub const ABI_ERR_OPERATION: i32 = -2;
/// Timeout
pub const ABI_ERR_TIMEOUT: i32 = -3;
/// Rate limit exceeded
pub const ABI_ERR_RATE_LIMIT: i32 = -4;
/// Zasob nie znaleziony
pub const ABI_ERR_NOT_FOUND: i32 = -5;
/// Bufor za maly — wartosc zwrotna to wymagany rozmiar
pub const ABI_ERR_BUFFER_TOO_SMALL: i32 = -6;

// =============================================================================
// Rejestracja host functions
// =============================================================================

/// Rejestruje wszystkie host functions w Wasmtime Linker.
/// Kazda funkcja jest dostepna dla addonu pod namespace "tentaflow".
pub fn register_host_functions(linker: &mut WasmLinker<AddonState>) -> Result<()> {
    // --- LLM API ---
    linker.func_wrap(
        "tentaflow", "llm_generate",
        llm::llm_generate,
    ).context("Rejestracja llm_generate")?;

    linker.func_wrap(
        "tentaflow", "llm_generate_stream_start",
        llm::llm_generate_stream_start,
    ).context("Rejestracja llm_generate_stream_start")?;

    linker.func_wrap(
        "tentaflow", "llm_generate_stream_next",
        llm::llm_generate_stream_next,
    ).context("Rejestracja llm_generate_stream_next")?;

    // --- Storage API ---
    linker.func_wrap(
        "tentaflow", "storage_get",
        storage::storage_get,
    ).context("Rejestracja storage_get")?;

    linker.func_wrap(
        "tentaflow", "storage_set",
        storage::storage_set,
    ).context("Rejestracja storage_set")?;

    linker.func_wrap(
        "tentaflow", "storage_delete",
        storage::storage_delete,
    ).context("Rejestracja storage_delete")?;

    linker.func_wrap(
        "tentaflow", "storage_list",
        storage::storage_list,
    ).context("Rejestracja storage_list")?;

    // --- HTTP API ---
    linker.func_wrap(
        "tentaflow", "http_request",
        http::http_request,
    ).context("Rejestracja http_request")?;

    // --- Event API ---
    linker.func_wrap(
        "tentaflow", "event_subscribe",
        events::event_subscribe,
    ).context("Rejestracja event_subscribe")?;

    linker.func_wrap(
        "tentaflow", "event_publish",
        events::event_publish,
    ).context("Rejestracja event_publish")?;

    // --- UI API ---
    linker.func_wrap(
        "tentaflow", "ui_render",
        ui::ui_render,
    ).context("Rejestracja ui_render")?;

    linker.func_wrap(
        "tentaflow", "ui_notify",
        ui::ui_notify,
    ).context("Rejestracja ui_notify")?;

    // --- User API ---
    linker.func_wrap(
        "tentaflow", "user_get_current",
        user::user_get_current,
    ).context("Rejestracja user_get_current")?;

    linker.func_wrap(
        "tentaflow", "user_check_permission",
        user::user_check_permission,
    ).context("Rejestracja user_check_permission")?;

    // --- Secrets API ---
    linker.func_wrap(
        "tentaflow", "secret_get",
        secrets::secret_get,
    ).context("Rejestracja secret_get")?;

    linker.func_wrap(
        "tentaflow", "secret_set",
        secrets::secret_set,
    ).context("Rejestracja secret_set")?;

    // --- Log API ---
    linker.func_wrap(
        "tentaflow", "log_info",
        log::log_info,
    ).context("Rejestracja log_info")?;

    linker.func_wrap(
        "tentaflow", "log_warn",
        log::log_warn,
    ).context("Rejestracja log_warn")?;

    linker.func_wrap(
        "tentaflow", "log_error",
        log::log_error,
    ).context("Rejestracja log_error")?;

    // --- Tool API ---
    linker.func_wrap(
        "tentaflow", "tool_register",
        tool_register,
    ).context("Rejestracja tool_register")?;

    // --- Network API (proxy TCP/UDP) ---
    linker.func_wrap(
        "tentaflow", "net_connect",
        network::host_net_connect,
    ).context("Rejestracja net_connect")?;

    linker.func_wrap(
        "tentaflow", "net_send",
        network::host_net_send,
    ).context("Rejestracja net_send")?;

    linker.func_wrap(
        "tentaflow", "net_recv",
        network::host_net_recv,
    ).context("Rejestracja net_recv")?;

    linker.func_wrap(
        "tentaflow", "net_close",
        network::host_net_close,
    ).context("Rejestracja net_close")?;

    // --- Service API (QUIC proxy do zarejestrowanych serwisow) ---
    linker.func_wrap(
        "tentaflow", "service_request",
        service::service_request,
    ).context("Rejestracja service_request")?;

    Ok(())
}

// =============================================================================
// Pomocnicze funkcje do operacji na pamieci WASM
// =============================================================================

/// Odczytuje slice bajtow z pamieci guest WASM
pub fn read_guest_bytes<'a>(
    memory: &'a WasmMemory,
    store: &'a impl AsContext,
    ptr: i32,
    len: i32,
) -> Option<&'a [u8]> {
    if ptr < 0 || len < 0 {
        return None;
    }
    let start = ptr as usize;
    let end = start + len as usize;
    let data = memory.data(store);
    if end > data.len() {
        return None;
    }
    Some(&data[start..end])
}

/// Odczytuje string UTF-8 z pamieci guest WASM
pub fn read_guest_string<'a>(
    memory: &'a WasmMemory,
    store: &'a impl AsContext,
    ptr: i32,
    len: i32,
) -> Option<&'a str> {
    let bytes = read_guest_bytes(memory, store, ptr, len)?;
    std::str::from_utf8(bytes).ok()
}

/// Zapisuje bajty do pamieci guest WASM, zwraca ilosc zapisanych bajtow
pub fn write_guest_bytes(
    memory: &WasmMemory,
    store: &mut impl AsContextMut,
    ptr: i32,
    max_len: i32,
    data: &[u8],
) -> i32 {
    if ptr < 0 || max_len < 0 {
        return ABI_ERR_OPERATION;
    }
    let start = ptr as usize;
    let write_len = data.len().min(max_len as usize);
    let end = start + write_len;
    let mem = memory.data_mut(store);
    if end > mem.len() {
        return ABI_ERR_OPERATION;
    }
    mem[start..end].copy_from_slice(&data[..write_len]);
    write_len as i32
}

/// Zapisuje bajty do bufora guest i dlugosc do out_len_ptr.
/// Zwraca ABI_OK jesli sukces, ABI_ERR_BUFFER_TOO_SMALL jesli bufor za maly.
pub fn write_guest_output(
    memory: &WasmMemory,
    store: &mut impl AsContextMut,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
    data: &[u8],
) -> i32 {
    if data.len() > out_cap as usize {
        // Zapisz wymagany rozmiar w out_len_ptr
        let required = (data.len() as i32).to_le_bytes();
        let mem = memory.data_mut(store);
        if (out_len_ptr as usize + 4) <= mem.len() {
            mem[out_len_ptr as usize..out_len_ptr as usize + 4].copy_from_slice(&required);
        }
        return data.len() as i32; // Zwroc wymagany rozmiar (addon realokuje)
    }

    let written = write_guest_bytes(memory, store, out_ptr, out_cap, data);
    if written < 0 {
        return written;
    }

    // Zapisz faktyczna dlugosc
    let len_bytes = written.to_le_bytes();
    let mem = memory.data_mut(store);
    if (out_len_ptr as usize + 4) <= mem.len() {
        mem[out_len_ptr as usize..out_len_ptr as usize + 4].copy_from_slice(&len_bytes);
    }

    ABI_OK
}

/// Pobiera obiekt memory z instancji WASM przez Caller
pub fn get_memory(caller: &mut WasmCaller<'_, AddonState>) -> Option<WasmMemory> {
    caller.get_export("memory")?.into_memory()
}

/// Loguje operacje do audit log w DB
pub fn audit_log(
    state: &AddonState,
    action: &str,
    resource_type: Option<&str>,
    resource_id: Option<&str>,
    result: &str,
    error_message: Option<&str>,
) {
    let action_hash = fnv1a_hash(action);
    if let Ok(conn) = state.db.lock() {
        let _ = conn.execute(
            "INSERT INTO audit_log (user_id, addon_id, instance_id, action, resource_type, resource_id, result, error_message, action_hash) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                state.user_id, &state.addon_id, &state.instance_id,
                action, resource_type, resource_id,
                result, error_message, action_hash
            ],
        );
    }
}

/// D5: Reuzywany hash FNV-1a z utils
fn fnv1a_hash(s: &str) -> i64 {
    super::utils::fnv1a_hash(s)
}

/// Sprawdza uprawnienie addonu — zwraca true jesli przyznane.
/// Uprawnienia sa boolean (przyznane/nieprzyznane) — bez poziomow dostepu.
/// CR-006: Brak user_id nie powoduje automatycznego przyznania uprawnien —
/// wymaga jawnego ustawienia flagi is_system_call.
pub fn check_permission(
    state: &AddonState,
    permission_type: &str,
    resource: Option<&str>,
) -> bool {
    // Najpierw sprawdz czy addon deklaruje to uprawnienie
    if !state.permissions.iter().any(|p| p == permission_type) {
        return false;
    }

    // Jesli brak user_id — sprawdz czy to jawne wywolanie systemowe
    let user_id = match state.user_id {
        Some(id) => id,
        None => {
            // CR-006: Tylko jawne wywolania systemowe (is_system_call=true) omijaja
            // sprawdzanie user_id. Zapobiega permission bypass przy braku user_id.
            return state.is_system_call;
        }
    };

    state.permission_checker.check(
        &state.addon_id,
        user_id,
        permission_type,
        resource,
    ).is_granted()
}

// =============================================================================
// Tool register — host function
// =============================================================================

/// Host function: rejestruje narzedzie addonu (dla LLM tool calling)
fn tool_register(
    mut caller: WasmCaller<'_, AddonState>,
    tool_json_ptr: i32,
    tool_json_len: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return ABI_ERR_OPERATION,
    };

    let tool_json = match read_guest_string(&memory, &caller, tool_json_ptr, tool_json_len) {
        Some(s) => s.to_string(),
        None => return ABI_ERR_OPERATION,
    };

    let tool_def: serde_json::Value = match serde_json::from_str(&tool_json) {
        Ok(v) => v,
        Err(_) => return ABI_ERR_OPERATION,
    };

    let state = caller.data();
    let addon_id = state.addon_id.clone();

    // Zapisz narzedzie w DB
    if let Ok(conn) = state.db.lock() {
        let tool_name = tool_def.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let description = tool_def.get("description").and_then(|v| v.as_str()).unwrap_or("");
        let params_schema = tool_def.get("parameters_schema")
            .map(|v| v.to_string())
            .unwrap_or_else(|| "{}".to_string());
        let return_schema = tool_def.get("return_schema")
            .map(|v| v.to_string());
        let keywords_json = tool_def.get("keywords")
            .map(|v| v.to_string())
            .unwrap_or_else(|| "[]".to_string());

        let _ = conn.execute(
            "INSERT OR REPLACE INTO addon_tools (addon_id, tool_name, description, parameters_schema_json, return_schema_json, is_active, keywords_json) \
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)",
            rusqlite::params![&addon_id, tool_name, description, &params_schema, &return_schema, &keywords_json],
        );
    }

    audit_log(caller.data(), "tool.register", Some("tool"), None, "ok", None);

    ABI_OK
}
