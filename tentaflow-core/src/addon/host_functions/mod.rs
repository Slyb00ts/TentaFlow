// =============================================================================
// Plik: addon/host_functions/mod.rs
// Opis: Rejestracja host functions w Wasmtime Linker — definiuje API dostepne
//       dla addonow WASM. Kazda host function sprawdza uprawnienia, loguje
//       do audit trail i operuje na liniowej pamieci WASM.
// =============================================================================

pub mod abi_helpers;
pub mod aliases;
#[cfg(feature = "camera")]
pub mod camera;
pub mod events;
pub mod http;
pub mod llm;
pub mod log;
pub mod network;
pub mod oauth;
#[cfg(feature = "camera")]
pub mod recording;
pub mod secrets;
pub mod service;
pub mod sql;
pub mod storage;
#[cfg(feature = "camera")]
pub mod streaming;
pub mod ui;
pub mod user;

use anyhow::Result;

use super::runtime::{AsContext, AsContextMut, WasmCaller, WasmLinker, WasmMemory};
use super::AddonState;

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
    linker
        .func_wrap("tentaflow", "llm_generate", llm::llm_generate)
        .map_err(|e| anyhow::anyhow!("Rejestracja llm_generate: {e}"))?;

    linker
        .func_wrap(
            "tentaflow",
            "llm_generate_stream_start",
            llm::llm_generate_stream_start,
        )
        .map_err(|e| anyhow::anyhow!("Rejestracja llm_generate_stream_start: {e}"))?;

    linker
        .func_wrap(
            "tentaflow",
            "llm_generate_stream_next",
            llm::llm_generate_stream_next,
        )
        .map_err(|e| anyhow::anyhow!("Rejestracja llm_generate_stream_next: {e}"))?;

    // --- Storage API ---
    linker
        .func_wrap("tentaflow", "storage_get", storage::storage_get)
        .map_err(|e| anyhow::anyhow!("Rejestracja storage_get: {e}"))?;

    linker
        .func_wrap("tentaflow", "storage_set", storage::storage_set)
        .map_err(|e| anyhow::anyhow!("Rejestracja storage_set: {e}"))?;

    linker
        .func_wrap("tentaflow", "storage_delete", storage::storage_delete)
        .map_err(|e| anyhow::anyhow!("Rejestracja storage_delete: {e}"))?;

    linker
        .func_wrap("tentaflow", "storage_list", storage::storage_list)
        .map_err(|e| anyhow::anyhow!("Rejestracja storage_list: {e}"))?;

    // --- HTTP API ---
    linker
        .func_wrap("tentaflow", "http_request", http::http_request)
        .map_err(|e| anyhow::anyhow!("Rejestracja http_request: {e}"))?;

    // --- Event API ---
    linker
        .func_wrap("tentaflow", "event_subscribe", events::event_subscribe)
        .map_err(|e| anyhow::anyhow!("Rejestracja event_subscribe: {e}"))?;

    linker
        .func_wrap("tentaflow", "event_publish", events::event_publish)
        .map_err(|e| anyhow::anyhow!("Rejestracja event_publish: {e}"))?;

    // --- UI API ---
    linker
        .func_wrap("tentaflow", "ui_render", ui::ui_render)
        .map_err(|e| anyhow::anyhow!("Rejestracja ui_render: {e}"))?;

    linker
        .func_wrap("tentaflow", "ui_notify", ui::ui_notify)
        .map_err(|e| anyhow::anyhow!("Rejestracja ui_notify: {e}"))?;

    // --- User API ---
    linker
        .func_wrap("tentaflow", "user_get_current", user::user_get_current)
        .map_err(|e| anyhow::anyhow!("Rejestracja user_get_current: {e}"))?;

    linker
        .func_wrap(
            "tentaflow",
            "user_check_permission",
            user::user_check_permission,
        )
        .map_err(|e| anyhow::anyhow!("Rejestracja user_check_permission: {e}"))?;

    // --- Secrets API ---
    linker
        .func_wrap("tentaflow", "secret_get", secrets::secret_get)
        .map_err(|e| anyhow::anyhow!("Rejestracja secret_get: {e}"))?;

    linker
        .func_wrap("tentaflow", "secret_set", secrets::secret_set)
        .map_err(|e| anyhow::anyhow!("Rejestracja secret_set: {e}"))?;

    // --- Log API ---
    linker
        .func_wrap("tentaflow", "log_info", log::log_info)
        .map_err(|e| anyhow::anyhow!("Rejestracja log_info: {e}"))?;

    linker
        .func_wrap("tentaflow", "log_warn", log::log_warn)
        .map_err(|e| anyhow::anyhow!("Rejestracja log_warn: {e}"))?;

    linker
        .func_wrap("tentaflow", "log_error", log::log_error)
        .map_err(|e| anyhow::anyhow!("Rejestracja log_error: {e}"))?;

    // --- Tool API ---
    linker
        .func_wrap("tentaflow", "tool_register", tool_register)
        .map_err(|e| anyhow::anyhow!("Rejestracja tool_register: {e}"))?;

    // --- Network API (proxy TCP/UDP) ---
    linker
        .func_wrap("tentaflow", "net_connect", network::host_net_connect)
        .map_err(|e| anyhow::anyhow!("Rejestracja net_connect: {e}"))?;

    linker
        .func_wrap("tentaflow", "net_send", network::host_net_send)
        .map_err(|e| anyhow::anyhow!("Rejestracja net_send: {e}"))?;

    linker
        .func_wrap("tentaflow", "net_recv", network::host_net_recv)
        .map_err(|e| anyhow::anyhow!("Rejestracja net_recv: {e}"))?;

    linker
        .func_wrap("tentaflow", "net_close", network::host_net_close)
        .map_err(|e| anyhow::anyhow!("Rejestracja net_close: {e}"))?;

    // --- Service API (QUIC proxy do zarejestrowanych serwisow) ---
    linker
        .func_wrap("tentaflow", "service_request", service::service_request)
        .map_err(|e| anyhow::anyhow!("Rejestracja service_request: {e}"))?;

    // --- OAuth API ---
    linker
        .func_wrap("tentaflow", "oauth_get_token", oauth::oauth_get_token)
        .map_err(|e| anyhow::anyhow!("Rejestracja oauth_get_token: {e}"))?;

    // --- SQL API (F1a M1.W4 — per-addon SQLite z migracjami) ---
    linker
        .func_wrap("tentaflow", "sql_exec_v1", sql::sql_exec_v1)
        .map_err(|e| anyhow::anyhow!("Rejestracja sql_exec_v1: {e}"))?;
    linker
        .func_wrap("tentaflow", "sql_query_v1", sql::sql_query_v1)
        .map_err(|e| anyhow::anyhow!("Rejestracja sql_query_v1: {e}"))?;
    linker
        .func_wrap("tentaflow", "sql_query_one_v1", sql::sql_query_one_v1)
        .map_err(|e| anyhow::anyhow!("Rejestracja sql_query_one_v1: {e}"))?;
    linker
        .func_wrap("tentaflow", "sql_transaction_v1", sql::sql_transaction_v1)
        .map_err(|e| anyhow::anyhow!("Rejestracja sql_transaction_v1: {e}"))?;

    // --- Alias API (F1a M1.W5 — readonly: alias_get / alias_list_owned) ---
    linker
        .func_wrap("tentaflow", "alias_get_v1", aliases::alias_get_v1)
        .map_err(|e| anyhow::anyhow!("Rejestracja alias_get_v1: {e}"))?;
    linker
        .func_wrap(
            "tentaflow",
            "alias_list_owned_v1",
            aliases::alias_list_owned_v1,
        )
        .map_err(|e| anyhow::anyhow!("Rejestracja alias_list_owned_v1: {e}"))?;

    // --- Camera API (F1a M1.W6 — TentaVision camera ingest) ---
    #[cfg(feature = "camera")]
    {
        linker
            .func_wrap("tentaflow", "camera_add_v1", camera::camera_add_v1)
            .map_err(|e| anyhow::anyhow!("Rejestracja camera_add_v1: {e}"))?;
        linker
            .func_wrap("tentaflow", "camera_list_v1", camera::camera_list_v1)
            .map_err(|e| anyhow::anyhow!("Rejestracja camera_list_v1: {e}"))?;
        linker
            .func_wrap("tentaflow", "camera_get_v1", camera::camera_get_v1)
            .map_err(|e| anyhow::anyhow!("Rejestracja camera_get_v1: {e}"))?;
        linker
            .func_wrap("tentaflow", "camera_update_v1", camera::camera_update_v1)
            .map_err(|e| anyhow::anyhow!("Rejestracja camera_update_v1: {e}"))?;
        linker
            .func_wrap("tentaflow", "camera_remove_v1", camera::camera_remove_v1)
            .map_err(|e| anyhow::anyhow!("Rejestracja camera_remove_v1: {e}"))?;
        linker
            .func_wrap("tentaflow", "camera_snapshot_v1", camera::camera_snapshot_v1)
            .map_err(|e| anyhow::anyhow!("Rejestracja camera_snapshot_v1: {e}"))?;
        linker
            .func_wrap("tentaflow", "camera_health_v1", camera::camera_health_v1)
            .map_err(|e| anyhow::anyhow!("Rejestracja camera_health_v1: {e}"))?;
        linker
            .func_wrap("tentaflow", "camera_discover_v1", camera::camera_discover_v1)
            .map_err(|e| anyhow::anyhow!("Rejestracja camera_discover_v1: {e}"))?;
        linker
            .func_wrap(
                "tentaflow",
                "camera_test_connection_v1",
                camera::camera_test_connection_v1,
            )
            .map_err(|e| anyhow::anyhow!("Rejestracja camera_test_connection_v1: {e}"))?;
        linker
            .func_wrap(
                "tentaflow",
                "camera_credentials_rotate_v1",
                camera::camera_credentials_rotate_v1,
            )
            .map_err(|e| anyhow::anyhow!("Rejestracja camera_credentials_rotate_v1: {e}"))?;

        // --- Streaming API (F1a M1.W7 — TentaVision frame bus + PickupToken) ---
        linker
            .func_wrap("tentaflow", "stream_subscribe_v1", streaming::stream_subscribe_v1)
            .map_err(|e| anyhow::anyhow!("Rejestracja stream_subscribe_v1: {e}"))?;
        linker
            .func_wrap("tentaflow", "stream_next_v1", streaming::stream_next_v1)
            .map_err(|e| anyhow::anyhow!("Rejestracja stream_next_v1: {e}"))?;
        linker
            .func_wrap("tentaflow", "stream_close_v1", streaming::stream_close_v1)
            .map_err(|e| anyhow::anyhow!("Rejestracja stream_close_v1: {e}"))?;

        // --- Recording API (F1a M1.W8 — TentaVision recording manager + frame_url) ---
        linker
            .func_wrap(
                "tentaflow",
                "recording_save_snapshot_v1",
                recording::recording_save_snapshot_v1,
            )
            .map_err(|e| anyhow::anyhow!("Rejestracja recording_save_snapshot_v1: {e}"))?;
        linker
            .func_wrap(
                "tentaflow",
                "recording_save_segment_v1",
                recording::recording_save_segment_v1,
            )
            .map_err(|e| anyhow::anyhow!("Rejestracja recording_save_segment_v1: {e}"))?;
        linker
            .func_wrap(
                "tentaflow",
                "recording_get_url_v1",
                recording::recording_get_url_v1,
            )
            .map_err(|e| anyhow::anyhow!("Rejestracja recording_get_url_v1: {e}"))?;
        linker
            .func_wrap(
                "tentaflow",
                "recording_get_stream_v1",
                recording::recording_get_stream_v1,
            )
            .map_err(|e| anyhow::anyhow!("Rejestracja recording_get_stream_v1: {e}"))?;
        linker
            .func_wrap(
                "tentaflow",
                "recording_purge_v1",
                recording::recording_purge_v1,
            )
            .map_err(|e| anyhow::anyhow!("Rejestracja recording_purge_v1: {e}"))?;
        linker
            .func_wrap(
                "tentaflow",
                "recording_stats_v1",
                recording::recording_stats_v1,
            )
            .map_err(|e| anyhow::anyhow!("Rejestracja recording_stats_v1: {e}"))?;
        linker
            .func_wrap("tentaflow", "frame_url_v1", recording::frame_url_v1)
            .map_err(|e| anyhow::anyhow!("Rejestracja frame_url_v1: {e}"))?;
    }

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

/// Loguje operacje do audit log w DB (backward-compat — deleguje do
/// `audit_log_with_risk` z RiskClass::Unclassified). Uzywane przez host
/// functions sprzed F1a (storage, http, llm, ui, events, secrets, ...).
pub fn audit_log(
    state: &AddonState,
    action: &str,
    resource_type: Option<&str>,
    resource_id: Option<&str>,
    result: &str,
    error_message: Option<&str>,
) {
    audit_log_with_risk(
        state,
        action,
        resource_type,
        resource_id,
        crate::audit::RiskClass::Unclassified,
        None,
        None,
        result,
        error_message,
    );
}

/// Loguje operacje do audit log z pelnym kontekstem F1a:
/// - `risk_class` — klasyfikacja RODO wpisu (A/B/C/unclassified).
/// - `related_claim_id` — powiazany claim (gate evaluation, F2).
/// - `request_id` — korelacja wielu wpisow w obrebie jednego wywolania.
///
/// Wpisy klasy B/C maja indeks partial w DB — szybkie kwerendy zgodnosciowe.
#[allow(clippy::too_many_arguments)]
pub fn audit_log_with_risk(
    state: &AddonState,
    action: &str,
    resource_type: Option<&str>,
    resource_id: Option<&str>,
    risk_class: crate::audit::RiskClass,
    related_claim_id: Option<&str>,
    request_id: Option<&str>,
    result: &str,
    error_message: Option<&str>,
) {
    let action_hash = fnv1a_hash(action);
    if let Ok(conn) = state.db.lock() {
        // F1b P4 (DoD-15) — extend each row with a Merkle hash linked to the
        // previous row's hash. The shared `DbPool` Mutex serializes us against
        // every other writer, so the SELECT(latest hash) + INSERT pair is
        // atomic without an explicit transaction. Pre-bind the timestamp the
        // same way SQLite's `datetime('now')` default would render it
        // ("YYYY-MM-DD HH:MM:SS" UTC) so the hash input matches the value
        // the verifier reads back from the row.
        let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let risk_class_db = risk_class.as_db_str();
        let hash_input = crate::audit::chain::AuditRowHashInput {
            user_id: state.user_id,
            addon_id: Some(state.addon_id.as_str()),
            instance_id: Some(state.instance_id.as_str()),
            action,
            resource: None,
            resource_type,
            resource_id,
            result: Some(result),
            error_message,
            details: None,
            ip_address: None,
            node_id: None,
            severity: Some("info"),
            risk_class: risk_class_db,
            related_claim_id,
            request_id,
            timestamp: &timestamp,
        };
        let (prev_hash_blob, hash_blob) =
            match crate::audit::chain::compute_chain_for_insert(&conn, &hash_input) {
                Ok(pair) => pair,
                Err(e) => {
                    tracing::warn!("audit chain: compute_chain_for_insert failed: {e}");
                    return;
                }
            };

        let _ = conn.execute(
            "INSERT INTO audit_log (user_id, addon_id, instance_id, action, resource_type, resource_id, result, error_message, action_hash, risk_class, related_claim_id, request_id, timestamp, prev_hash, hash) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            rusqlite::params![
                state.user_id, &state.addon_id, &state.instance_id,
                action, resource_type, resource_id,
                result, error_message, action_hash,
                risk_class_db, related_claim_id, request_id,
                timestamp, prev_hash_blob, hash_blob
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
pub fn check_permission(state: &AddonState, permission_type: &str, resource: Option<&str>) -> bool {
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

    state
        .permission_checker
        .check(&state.addon_id, user_id, permission_type, resource)
        .is_granted()
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
        let description = tool_def
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let params_schema = tool_def
            .get("parameters_schema")
            .map(|v| v.to_string())
            .unwrap_or_else(|| "{}".to_string());
        let return_schema = tool_def.get("return_schema").map(|v| v.to_string());
        let keywords_json = tool_def
            .get("keywords")
            .map(|v| v.to_string())
            .unwrap_or_else(|| "[]".to_string());

        let _ = conn.execute(
            "INSERT OR REPLACE INTO addon_tools (addon_id, tool_name, description, parameters_schema_json, return_schema_json, is_active, keywords_json) \
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)",
            rusqlite::params![&addon_id, tool_name, description, &params_schema, &return_schema, &keywords_json],
        );
    }

    audit_log(
        caller.data(),
        "tool.register",
        Some("tool"),
        None,
        "ok",
        None,
    );

    ABI_OK
}
