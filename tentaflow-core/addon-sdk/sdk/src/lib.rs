// =============================================================================
// Plik: addon-sdk/sdk/src/lib.rs
// Opis: TentaFlow Addon SDK — bindingi do host functions, helpery pamieciowe,
//       wysokopoziomowe wrappery do komunikacji z Core (LLM, storage, HTTP,
//       eventy, UI, sekrety, logi, rejestracja narzedzi).
// =============================================================================

//! # Typed UI primitives
//!
//! For new addons, use [`render_panel_typed`] together with `ui::PanelTree`
//! and the typed component sub-enums (`ui::layout::*`, `ui::container::*`,
//! `ui::data_display::*`, etc.). This eliminates the intermediate
//! `serde_json::Value` allocation that `render_panel(panel_id, json!({}))`
//! performs — addons measured a ~5× reduction in guest CPU time and ~3×
//! fewer heap allocations per render. See `notes/addon-ui-perf-plan.md` in
//! the TentaFlow repo for the diagnosis and migration plan.
//!
//! The legacy [`render_panel`] entry point that accepts `serde_json::Value`
//! is preserved for addons that have not yet migrated.

use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// Typed UI schema (PanelTree, UiComponent, theme tokens, layout/container/
/// form/feedback/action/data_display/specialized sub-enums). Re-exported
/// 1:1 from `tentaflow-ui-schema`, the same crate the host links against —
/// so a `PanelTree` value produced here serializes byte-for-byte to what
/// the host expects.
pub mod ui {
    pub use tentaflow_ui_schema::*;
}

// =============================================================================
// AbiError — kanoniczne kody bledow ABI dla F1a host functions
// =============================================================================
//
// MUST stay in sync with `tentaflow-core/src/addon/errors.rs`. The SDK is
// compiled for `wasm32-wasip1` and cannot depend on `tentaflow-core` (the
// core crate pulls in rusqlite, wasmtime, axum, tokio — none of which
// build for that target). Duplicating the enum is the only viable path.
//
// Numeric values are part of the ABI: if you change one, both the host
// and every shipped addon WASM must be rebuilt. The test
// `abi_error_codes_match_plan_spec` in core/errors.rs anchors the
// canonical values (0, 1, 6, 21, 24); the rest are sequential.

/// Kanoniczne kody bledow ABI zwracane przez host functions F1a (SQL,
/// Alias, Camera, Streaming, Recording). Wartosci 0..=24, gdzie 0 = sukces.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiError {
    Ok = 0,
    Permission = 1,
    NotFound = 2,
    NoAvailableTarget = 3,
    Timeout = 4,
    Operation = 5,
    OutputBufferTooSmall = 6,
    Conflict = 7,
    SqlSyntax = 8,
    SqlConstraint = 9,
    SqlNoResult = 10,
    QuotaExceeded = 11,
    CameraUnreachable = 12,
    CameraAuthFailed = 13,
    CameraVendorUnsupported = 14,
    StreamNotFound = 15,
    StreamClosed = 16,
    Backpressure = 17,
    RecordingNotFound = 18,
    RecordingPurged = 19,
    RecordingTimeOutOfRing = 20,
    PayloadTooLarge = 21,
    GateNotSatisfied = 22,
    FrameTokenInvalid = 23,
    FramePurged = 24,
}

impl AbiError {
    /// Wartosc i32 do return z host functions.
    #[inline]
    pub const fn as_i32(self) -> i32 {
        self as i32
    }

    /// Decodes a raw i32 returned by a host function. Unknown codes fall
    /// back to `Operation` so callers never see a phantom variant after
    /// a host/SDK version skew.
    pub fn from_i32(rc: i32) -> Self {
        match rc {
            0 => Self::Ok,
            1 => Self::Permission,
            2 => Self::NotFound,
            3 => Self::NoAvailableTarget,
            4 => Self::Timeout,
            5 => Self::Operation,
            6 => Self::OutputBufferTooSmall,
            7 => Self::Conflict,
            8 => Self::SqlSyntax,
            9 => Self::SqlConstraint,
            10 => Self::SqlNoResult,
            11 => Self::QuotaExceeded,
            12 => Self::CameraUnreachable,
            13 => Self::CameraAuthFailed,
            14 => Self::CameraVendorUnsupported,
            15 => Self::StreamNotFound,
            16 => Self::StreamClosed,
            17 => Self::Backpressure,
            18 => Self::RecordingNotFound,
            19 => Self::RecordingPurged,
            20 => Self::RecordingTimeOutOfRing,
            21 => Self::PayloadTooLarge,
            22 => Self::GateNotSatisfied,
            23 => Self::FrameTokenInvalid,
            24 => Self::FramePurged,
            _ => Self::Operation,
        }
    }
}

impl From<AbiError> for i32 {
    #[inline]
    fn from(e: AbiError) -> Self {
        e as i32
    }
}

impl core::fmt::Display for AbiError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "AbiError({})", *self as i32)
    }
}

// =============================================================================
// Bindingi do host functions (importowane z Core przez WASM)
// =============================================================================

#[link(wasm_import_module = "tentaflow")]
extern "C" {
    /// Generowanie tekstu przez LLM
    /// ABI: (prompt_ptr, prompt_len, model_ptr, model_len, options_ptr, options_len, out_ptr, out_cap, out_len_ptr) -> i32
    fn llm_generate(
        prompt_ptr: i32, prompt_len: i32,
        model_ptr: i32, model_len: i32,
        options_ptr: i32, options_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    /// Odczyt z klucz-wartosc storage
    /// ABI: (key_ptr, key_len, out_ptr, out_cap, out_len_ptr) -> i32
    /// Zgodne z host function w host_functions/storage.rs
    fn storage_get(key_ptr: i32, key_len: i32, out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;

    /// Zapis do klucz-wartosc storage
    fn storage_set(key_ptr: i32, key_len: i32, val_ptr: i32, val_len: i32) -> i32;

    fn sync_acl_upsert_v1(payload_ptr: i32, payload_len: i32) -> i32;

    fn sync_acl_delete_v1(payload_ptr: i32, payload_len: i32) -> i32;

    fn sync_share_grant_v1(payload_ptr: i32, payload_len: i32) -> i32;

    fn sync_share_revoke_v1(payload_ptr: i32, payload_len: i32) -> i32;

    /// Wykonanie requestu HTTP
    /// ABI: (req_ptr, req_len, out_ptr, out_cap, out_len_ptr) -> i32
    fn http_request(req_ptr: i32, req_len: i32, out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;

    /// Publikacja eventu na event bus
    /// ABI: (event_type_ptr, event_type_len, payload_json_ptr, payload_json_len) -> i32
    /// Zgodne z host function w host_functions/events.rs::event_publish
    fn event_publish(
        event_type_ptr: i32, event_type_len: i32,
        payload_json_ptr: i32, payload_json_len: i32,
    ) -> i32;

    /// Subskrypcja eventu — Core wywola guest export `on_event(ptr, len)` przy dostarczeniu.
    /// ABI: (event_type_ptr, event_type_len, filter_json_ptr, filter_json_len) -> i32
    /// Zwraca: subscription_id (>0) lub kod bledu (<0). Filtr opcjonalny — przekaz (0,0).
    fn event_subscribe(
        event_type_ptr: i32, event_type_len: i32,
        filter_json_ptr: i32, filter_json_len: i32,
    ) -> i32;

    /// Renderowanie panelu UI (deklaratywny JSON)
    /// ABI: (panel_id_ptr, panel_id_len, ui_json_ptr, ui_json_len) -> i32
    /// Zgodne z host function w host_functions/ui.rs::ui_render
    fn ui_render(
        panel_id_ptr: i32, panel_id_len: i32,
        ui_json_ptr: i32, ui_json_len: i32,
    ) -> i32;

    /// Renderowanie panelu UI — binary protocol (bincode-encoded PanelTree).
    /// Pozwala pominac JSON serialize/parse w sciezce addon→host
    /// (ok. 20× szybsze, payload ~2-3× mniejszy).
    /// ABI: (panel_id_ptr, panel_id_len, binary_ptr, binary_len) -> i32
    /// Zgodne z host function w host_functions/ui.rs::ui_render_binary
    fn ui_render_binary(
        panel_id_ptr: i32, panel_id_len: i32,
        binary_ptr: i32, binary_len: i32,
    ) -> i32;

    /// Wyswietlenie powiadomienia
    /// ABI: (title_ptr, title_len, body_ptr, body_len, level_ptr, level_len) -> i32
    /// Zgodne z host function w host_functions/ui.rs::ui_notify
    fn ui_notify(
        title_ptr: i32, title_len: i32,
        body_ptr: i32, body_len: i32,
        level_ptr: i32, level_len: i32,
    ) -> i32;

    /// Odczyt sekretu (szyfrowany w Core)
    /// ABI: (key_ptr, key_len, out_ptr, out_cap, out_len_ptr) -> i32
    fn secret_get(key_ptr: i32, key_len: i32, out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;

    /// Zapis sekretu
    fn secret_set(key_ptr: i32, key_len: i32, val_ptr: i32, val_len: i32) -> i32;

    /// Logowanie — poziom info
    fn log_info(msg_ptr: i32, msg_len: i32) -> i32;

    /// Logowanie — poziom warn
    fn log_warn(msg_ptr: i32, msg_len: i32) -> i32;

    /// Logowanie — poziom error
    fn log_error(msg_ptr: i32, msg_len: i32) -> i32;

    /// Pobranie danych aktualnego uzytkownika (JSON)
    /// ABI: (out_ptr, out_cap, out_len_ptr) -> i32
    fn user_get_current(out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;

    /// Rejestracja narzedzia (tool) dla LLM
    fn tool_register(def_ptr: i32, def_len: i32) -> i32;

    /// Nawiazanie polaczenia sieciowego TCP/UDP wedlug reguly z manifestu
    fn net_connect(rule_id_ptr: i32, rule_id_len: i32) -> i32;

    /// Wyslanie danych przez aktywne polaczenie sieciowe
    fn net_send(conn_id: i32, data_ptr: i32, data_len: i32) -> i32;

    /// Odebranie danych z aktywnego polaczenia sieciowego
    /// Zwraca packed i64: (status << 32) | bytes_read
    fn net_recv(conn_id: i32, out_ptr: i32, out_capacity: i32) -> i64;

    /// Zamkniecie aktywnego polaczenia sieciowego
    fn net_close(conn_id: i32) -> i32;

    /// Wyslanie requestu do zarejestrowanego serwisu QUIC przez router
    /// ABI: (service_ptr, service_len, request_ptr, request_len, out_ptr, out_cap, out_len_ptr) -> i32
    fn service_request(
        service_ptr: i32, service_len: i32,
        request_ptr: i32, request_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    /// SQL API (F1a M1.W4) — per-addon SQLite z bindowanymi parametrami.
    /// Zob. `docs/ADDON_HOST_FUNCTIONS.md` sekcja 11 dla pelnej specyfikacji.
    fn sql_exec_v1(
        query_ptr: i32, query_len: i32,
        params_json_ptr: i32, params_json_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    fn sql_query_v1(
        query_ptr: i32, query_len: i32,
        params_json_ptr: i32, params_json_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    fn sql_query_one_v1(
        query_ptr: i32, query_len: i32,
        params_json_ptr: i32, params_json_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    fn sql_transaction_v1(
        statements_json_ptr: i32, statements_json_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    /// Vector API (F1c P3) — embedded HNSW per-addon per-namespace vector
    /// indexes (usearch + mmap on disk). Requires `vector.read` (search) /
    /// `vector.write` (upsert/delete) permissions. Wire format is TOML;
    /// vector payloads are base64-encoded little-endian f32 bytes.
    fn vector_upsert_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    fn vector_search_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    fn vector_delete_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    /// Policy / Gate API (F1c P4) — verify a DPIA / FRIA claim against a
    /// `[[gate]]` declaration before running a gated operation. Requires
    /// `policy.read` permission.
    fn gate_check_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    /// Flow API (F1c P5) — invoke addon-declared flow templates, poll status,
    /// request cooperative cancellation. Requires `flow.invoke` permission.
    fn flow_invoke_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn flow_status_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn flow_cancel_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    /// Services catalog API (F2 P2.a) — read-only view of the mesh-wide
    /// service registry. Requires `service.read` permission.
    fn service_list_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn node_resources_get_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    /// Alias API (F1a M1.W5) — readonly inspection of aliases.
    /// Requires `alias.read` permission. Lifecycle (create/deactivate) is
    /// driven implicitly by addon install/uninstall from the manifest.
    fn alias_get_v1(
        alias_id_ptr: i32, alias_id_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    fn alias_list_owned_v1(
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    /// Camera API (F1a M1.W6) — camera ingest layer (fake_file vendor only
    /// in F1a). Payload format is CBOR for all inputs/outputs. Requires
    /// `cameras.read` / `cameras.write` / `cameras.snapshot` permissions.
    fn camera_add_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_list_v1(out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
    fn camera_get_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_update_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_remove_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_snapshot_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_health_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_discover_v1(out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
    fn camera_test_connection_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_credentials_rotate_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    /// Streaming API (F1a M1.W7) — frame bus + PickupToken. Frame bytes are
    /// NOT inlined in `stream_next` output; the addon receives `frame_ref`
    /// + metadata and uses `service_call` to hand the frame to a service.
    fn stream_subscribe_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn stream_next_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn stream_close_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    /// Recording API (F1a M1.W8) — snapshot PNG, segment MP4, signed URLs.
    /// All inputs / outputs are TOML. Requires `recording.read` / `recording.write`.
    fn recording_save_snapshot_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn recording_save_segment_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn recording_get_url_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn recording_get_stream_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn recording_purge_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn recording_stats_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn frame_url_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;

    /// ONVIF analytics metadata (F2 P6.b) — subscribe/poll/unsubscribe to
    /// per-camera object detection events pulled from the camera's
    /// PullPoint events service. All payloads are TOML.
    fn camera_metadata_subscribe_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_metadata_unsubscribe_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_metadata_poll_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
}

// =============================================================================
// Helpery pamieciowe — odczyt/zapis stringow z pamieci WASM
// =============================================================================

/// Odczytuje string z pamieci guest WASM pod podanym adresem i dlugosci.
/// Uzywane do dekodowania danych przekazanych z hosta.
pub fn read_string(ptr: i32, len: i32) -> String {
    if len <= 0 {
        return String::new();
    }
    let slice = unsafe {
        std::slice::from_raw_parts(ptr as *const u8, len as usize)
    };
    String::from_utf8_lossy(slice).to_string()
}

/// Zapisuje string do bufora w pamieci guest WASM.
/// Zwraca liczbe zapisanych bajtow lub -1 jesli bufor za maly.
pub fn write_string(ptr: i32, max: i32, s: &str) -> i32 {
    let bytes = s.as_bytes();
    if bytes.len() > max as usize {
        return -1;
    }
    let dest = unsafe {
        std::slice::from_raw_parts_mut(ptr as *mut u8, max as usize)
    };
    dest[..bytes.len()].copy_from_slice(bytes);
    bytes.len() as i32
}

// =============================================================================
// Wewnetrzne helpery do wywolywania host functions
// =============================================================================

/// Bufor roboczy na odpowiedzi z hosta (64KB)
const RESPONSE_BUFFER_SIZE: usize = 65536;

/// Wywoluje host function ktora przyjmuje (ptr, len, out_ptr, out_cap, out_len_ptr) -> i32.
/// ABI 5-param: wejscie + bufor wyjsciowy z out_len_ptr.
fn call_host_with_input_and_output_5(
    host_fn: unsafe extern "C" fn(i32, i32, i32, i32, i32) -> i32,
    input: &str,
) -> Result<String, String> {
    let input_bytes = input.as_bytes();
    let mut buffer = vec![0u8; RESPONSE_BUFFER_SIZE];
    let mut out_len: i32 = 0;

    let result_code = unsafe {
        host_fn(
            input_bytes.as_ptr() as i32,
            input_bytes.len() as i32,
            buffer.as_mut_ptr() as i32,
            RESPONSE_BUFFER_SIZE as i32,
            &mut out_len as *mut i32 as i32,
        )
    };

    if result_code < 0 {
        return Err(format!("Host function zwrocila blad: {}", result_code));
    }

    if out_len <= 0 {
        return Ok(String::new());
    }

    let output = String::from_utf8_lossy(&buffer[..out_len as usize]).to_string();
    Ok(output)
}

/// Wywoluje host function ktora przyjmuje dwa pary (ptr, len) (klucz + wartosc).
fn call_host_kv_set(
    host_fn: unsafe extern "C" fn(i32, i32, i32, i32) -> i32,
    key: &str,
    value: &str,
) -> Result<(), String> {
    let key_bytes = key.as_bytes();
    let val_bytes = value.as_bytes();

    let result = unsafe {
        host_fn(
            key_bytes.as_ptr() as i32,
            key_bytes.len() as i32,
            val_bytes.as_ptr() as i32,
            val_bytes.len() as i32,
        )
    };

    if result != 0 {
        return Err(format!("Host function zwrocila blad: {}", result));
    }

    Ok(())
}

fn call_host_binary_status(
    host_fn: unsafe extern "C" fn(i32, i32) -> i32,
    payload: &[u8],
) -> Result<(), AbiError> {
    let rc = unsafe { host_fn(payload.as_ptr() as i32, payload.len() as i32) };
    if rc == 0 {
        Ok(())
    } else {
        Err(AbiError::from_i32(rc))
    }
}

/// Wywoluje host function ktora przyjmuje klucz i zwraca wartosc do bufora.
/// ABI 5-param: (key_ptr, key_len, out_ptr, out_cap, out_len_ptr) -> i32
/// Host zapisuje dane do out_ptr i dlugosc do out_len_ptr (4 bajty LE).
/// Zwraca ABI_OK (0), ABI_ERR_NOT_FOUND (-5) lub inny kod bledu.
fn call_host_kv_get_5(
    host_fn: unsafe extern "C" fn(i32, i32, i32, i32, i32) -> i32,
    key: &str,
) -> Result<Option<String>, String> {
    let key_bytes = key.as_bytes();
    let mut buffer = vec![0u8; RESPONSE_BUFFER_SIZE];
    let mut out_len: i32 = 0;

    let result_code = unsafe {
        host_fn(
            key_bytes.as_ptr() as i32,
            key_bytes.len() as i32,
            buffer.as_mut_ptr() as i32,
            RESPONSE_BUFFER_SIZE as i32,
            &mut out_len as *mut i32 as i32,
        )
    };

    // ABI_ERR_NOT_FOUND = -5
    if result_code == -5 {
        return Ok(None);
    }

    if result_code < 0 {
        return Err(format!("Host function zwrocila blad: {}", result_code));
    }

    // ABI_OK = 0, dlugosc w out_len
    if out_len <= 0 {
        return Ok(None);
    }

    let output = String::from_utf8_lossy(&buffer[..out_len as usize]).to_string();
    Ok(Some(output))
}

/// Wywoluje host function ktora przyjmuje klucz i zwraca wartosc do bufora (4-param ABI).
/// Uzywane przez secret_get ktory ma ABI 4-param.
fn call_host_kv_get(
    host_fn: unsafe extern "C" fn(i32, i32, i32, i32) -> i32,
    key: &str,
) -> Result<Option<String>, String> {
    let key_bytes = key.as_bytes();
    let mut buffer = vec![0u8; RESPONSE_BUFFER_SIZE];

    let result_len = unsafe {
        host_fn(
            key_bytes.as_ptr() as i32,
            key_bytes.len() as i32,
            buffer.as_mut_ptr() as i32,
            RESPONSE_BUFFER_SIZE as i32,
        )
    };

    if result_len < 0 {
        // -1 = klucz nie znaleziony (nie blad)
        if result_len == -1 {
            return Ok(None);
        }
        return Err(format!("Host function zwrocila blad: {}", result_len));
    }

    if result_len == 0 {
        return Ok(None);
    }

    let output = String::from_utf8_lossy(&buffer[..result_len as usize]).to_string();
    Ok(Some(output))
}

/// Wywoluje host function do logowania (ptr, len).
fn call_host_log(
    host_fn: unsafe extern "C" fn(i32, i32) -> i32,
    message: &str,
) {
    let bytes = message.as_bytes();
    unsafe {
        host_fn(bytes.as_ptr() as i32, bytes.len() as i32);
    }
}

// =============================================================================
// Wysokopoziomowe wrappery — LLM
// =============================================================================

/// Generuje tekst przez LLM dostepny w Core.
/// Wymaga uprawnienia "llm" w manifescie addonu.
pub fn generate(prompt: &str) -> Result<String, String> {
    let prompt_bytes = prompt.as_bytes();
    let mut buffer = vec![0u8; RESPONSE_BUFFER_SIZE];
    let mut out_len: i32 = 0;

    let result_code = unsafe {
        llm_generate(
            prompt_bytes.as_ptr() as i32,
            prompt_bytes.len() as i32,
            0, 0,   // model_ptr, model_len — domyslny model
            0, 0,   // options_ptr, options_len — domyslne opcje
            buffer.as_mut_ptr() as i32,
            RESPONSE_BUFFER_SIZE as i32,
            &mut out_len as *mut i32 as i32,
        )
    };

    if result_code < 0 {
        return Err(format!("Host function llm_generate zwrocila blad: {}", result_code));
    }

    if out_len <= 0 {
        return Ok(String::new());
    }

    Ok(String::from_utf8_lossy(&buffer[..out_len as usize]).to_string())
}

// =============================================================================
// Wysokopoziomowe wrappery — Storage (klucz-wartosc)
// =============================================================================

/// Odczytuje wartosc z storage addonu.
/// Zwraca None jesli klucz nie istnieje.
/// Wymaga uprawnienia "storage" w manifescie addonu.
pub fn store_get(key: &str) -> Result<Option<String>, String> {
    call_host_kv_get_5(storage_get, key)
}

/// Zapisuje wartosc do storage addonu.
/// Wymaga uprawnienia "storage" z access_level "rw".
pub fn store_set(key: &str, value: &str) -> Result<(), String> {
    call_host_kv_set(storage_set, key, value)
}

// =============================================================================
// Wysokopoziomowe wrappery — Sync ACL
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncAclUpsert {
    pub resource_type: String,
    pub resource_id: String,
    pub owner_user_id: Option<i64>,
    pub assigned_user_id: Option<i64>,
    pub department_id: Option<String>,
    pub manager_user_id: Option<i64>,
    pub visibility_scope: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncAclDelete {
    pub resource_type: String,
    pub resource_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncShare {
    pub resource_type: String,
    pub resource_id: String,
    pub subject_type: String,
    pub subject_id: String,
    pub action: String,
}

pub fn sync_acl_upsert(request: &SyncAclUpsert) -> Result<(), AbiError> {
    let payload = encode_cbor(request)?;
    call_host_binary_status(sync_acl_upsert_v1, &payload)
}

pub fn sync_acl_delete(request: &SyncAclDelete) -> Result<(), AbiError> {
    let payload = encode_cbor(request)?;
    call_host_binary_status(sync_acl_delete_v1, &payload)
}

pub fn sync_share_grant(request: &SyncShare) -> Result<(), AbiError> {
    let payload = encode_cbor(request)?;
    call_host_binary_status(sync_share_grant_v1, &payload)
}

pub fn sync_share_revoke(request: &SyncShare) -> Result<(), AbiError> {
    let payload = encode_cbor(request)?;
    call_host_binary_status(sync_share_revoke_v1, &payload)
}

// =============================================================================
// Wysokopoziomowe wrappery — HTTP
// =============================================================================

/// Definicja requestu HTTP
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub body: Option<String>,
}

/// Odpowiedz HTTP z hosta
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: std::collections::HashMap<String, String>,
    pub body: String,
}

/// Wykonuje request HTTP GET.
/// Wymaga uprawnienia "http" w manifescie addonu.
pub fn http_get(url: &str) -> Result<String, String> {
    let req = HttpRequest {
        method: "GET".to_string(),
        url: url.to_string(),
        headers: std::collections::HashMap::new(),
        body: None,
    };
    let req_json = serde_json::to_string(&req)
        .map_err(|e| format!("Blad serializacji requestu HTTP: {}", e))?;
    call_host_with_input_and_output_5(http_request, &req_json)
}

/// Wykonuje request HTTP POST z podanym body.
/// Wymaga uprawnienia "http" w manifescie addonu.
pub fn http_post(url: &str, body: &str, content_type: &str) -> Result<String, String> {
    let mut headers = std::collections::HashMap::new();
    headers.insert("Content-Type".to_string(), content_type.to_string());

    let req = HttpRequest {
        method: "POST".to_string(),
        url: url.to_string(),
        headers,
        body: Some(body.to_string()),
    };
    let req_json = serde_json::to_string(&req)
        .map_err(|e| format!("Blad serializacji requestu HTTP: {}", e))?;
    call_host_with_input_and_output_5(http_request, &req_json)
}

/// Wykonuje dowolny request HTTP.
/// Wymaga uprawnienia "http" w manifescie addonu.
pub fn http_send(request: &HttpRequest) -> Result<HttpResponse, String> {
    let req_json = serde_json::to_string(request)
        .map_err(|e| format!("Blad serializacji requestu HTTP: {}", e))?;
    let response_str = call_host_with_input_and_output_5(http_request, &req_json)?;
    serde_json::from_str(&response_str)
        .map_err(|e| format!("Blad deserializacji odpowiedzi HTTP: {}", e))
}

// =============================================================================
// Wysokopoziomowe wrappery — Eventy
// =============================================================================

/// Definicja eventu do publikacji
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub event_type: String,
    pub payload: serde_json::Value,
}

/// Publikuje event na event bus Core.
/// Wymaga uprawnienia "events" z resource = event_type w manifescie addonu.
pub fn publish_event(event_type: &str, payload: serde_json::Value) -> Result<(), String> {
    let payload_json = serde_json::to_string(&payload)
        .map_err(|e| format!("Blad serializacji payload eventu: {}", e))?;
    let et = event_type.as_bytes();
    let pl = payload_json.as_bytes();
    let result = unsafe {
        event_publish(
            et.as_ptr() as i32, et.len() as i32,
            pl.as_ptr() as i32, pl.len() as i32,
        )
    };
    if result < 0 {
        return Err(format!("Blad publikacji eventu: {}", result));
    }
    Ok(())
}

/// Subskrybuje event — Core wywola guest export `on_event(ptr, len)` przy dostarczeniu.
/// Wymaga uprawnienia "events" z resource = event_type w manifescie addonu.
/// `filter` to opcjonalny filtr JSON (np. dopasowanie polu w payloadzie); `None` = brak filtra.
/// Zwraca `subscription_id` przyznane przez Core.
pub fn subscribe_event(
    event_type: &str,
    filter: Option<serde_json::Value>,
) -> Result<i64, String> {
    let filter_json = match &filter {
        Some(v) => serde_json::to_string(v)
            .map_err(|e| format!("Blad serializacji filtra eventu: {}", e))?,
        None => String::new(),
    };
    let et = event_type.as_bytes();
    let (filter_ptr, filter_len) = if filter.is_some() {
        let fb = filter_json.as_bytes();
        (fb.as_ptr() as i32, fb.len() as i32)
    } else {
        (0i32, 0i32)
    };
    let result = unsafe {
        event_subscribe(
            et.as_ptr() as i32, et.len() as i32,
            filter_ptr, filter_len,
        )
    };
    if result < 0 {
        return Err(format!("Blad subskrypcji eventu: {}", result));
    }
    Ok(result as i64)
}

// =============================================================================
// Wysokopoziomowe wrappery — UI
// =============================================================================

/// Renderuje panel UI addonu (deklaratywny JSON).
/// `content` to drzewo komponentow UI (zgodne z `UiComponent` w Core); panel jest
/// przekazywany do GUI przez event "ui.panel_rendered".
/// Wymaga uprawnienia "ui" w manifescie addonu.
///
/// Legacy entry point — accepts `serde_json::Value`, which forces an extra
/// allocation pass per render. New code should use [`render_panel_typed`]
/// with a typed [`ui::PanelTree`] instead.
pub fn render_panel(panel_id: &str, content: serde_json::Value) -> Result<(), String> {
    let ui_json = serde_json::to_string(&content)
        .map_err(|e| format!("Blad serializacji panelu UI: {}", e))?;
    ui_render_raw(panel_id, &ui_json)
}

/// Renders an addon UI panel from a typed [`ui::PanelTree`].
///
/// Skips the intermediate `serde_json::Value` allocation that
/// [`render_panel`] performs — the tree is serialized straight to JSON in
/// a single pass. Cuts guest-side CPU and allocations versus the legacy
/// `json!({...})` macro pattern (see `notes/addon-ui-perf-plan.md` §2).
/// Requires the `ui` permission in the addon manifest.
pub fn render_panel_typed(panel_id: &str, tree: &ui::PanelTree) -> Result<(), String> {
    let ui_json = serde_json::to_string(tree)
        .map_err(|e| format!("Blad serializacji panelu UI: {}", e))?;
    ui_render_raw(panel_id, &ui_json)
}

/// Renders an addon UI panel by shipping a CBOR-encoded `PanelTree`
/// directly across the addon↔host ABI. No JSON anywhere on the addon side.
///
/// On the host side this drops the `parse_and_validate_panel_tree(&str)`
/// JSON parser entirely — the host decodes CBOR once and
/// goes straight into `validate_panel_tree`. End-to-end this is several
/// times faster than [`render_panel_typed`] for non-trivial trees and the
/// wire payload is ~2-3× smaller (no whitespace, no quoting, integer-
/// tagged field names). See `notes/addon-ui-perf-plan.md` §2 P3.
///
/// Requires the `ui` permission in the addon manifest. Frontend wire
/// format is unchanged (host still serves panels as JSON via `panel_get`).
pub fn render_panel_binary(panel_id: &str, tree: &ui::PanelTree) -> Result<(), String> {
    let encoded = encode_cbor(tree).map_err(|e| format!("Blad serializacji CBOR panelu UI: {e}"))?;
    let pid = panel_id.as_bytes();
    let result = unsafe {
        ui_render_binary(
            pid.as_ptr() as i32, pid.len() as i32,
            encoded.as_ptr() as i32, encoded.len() as i32,
        )
    };
    if result < 0 {
        return Err(format!("Blad renderowania panelu UI (binary): {}", result));
    }
    Ok(())
}

fn encode_cbor<T: Serialize>(value: &T) -> Result<Vec<u8>, AbiError> {
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(value, &mut bytes).map_err(|_| AbiError::Operation)?;
    Ok(bytes)
}

/// Internal: ship a pre-serialized panel JSON across the ABI boundary.
/// Shared by [`render_panel`] and [`render_panel_typed`] so the unsafe
/// pointer dance lives in one place.
fn ui_render_raw(panel_id: &str, ui_json: &str) -> Result<(), String> {
    let pid = panel_id.as_bytes();
    let uj = ui_json.as_bytes();
    let result = unsafe {
        ui_render(
            pid.as_ptr() as i32, pid.len() as i32,
            uj.as_ptr() as i32, uj.len() as i32,
        )
    };
    if result < 0 {
        return Err(format!("Blad renderowania panelu UI: {}", result));
    }
    Ok(())
}

/// Wyswietla powiadomienie z poziomem "info".
/// Wymaga uprawnienia "notifications" w manifescie addonu.
pub fn notify(title: &str, body: &str) {
    notify_with_level(title, body, "info");
}

/// Wyswietla powiadomienie z okreslonym poziomem (info, warn, error, success).
pub fn notify_with_level(title: &str, body: &str, level: &str) {
    let t = title.as_bytes();
    let b = body.as_bytes();
    let l = level.as_bytes();
    unsafe {
        ui_notify(
            t.as_ptr() as i32, t.len() as i32,
            b.as_ptr() as i32, b.len() as i32,
            l.as_ptr() as i32, l.len() as i32,
        );
    }
}

// =============================================================================
// Wysokopoziomowe wrappery — Sekrety
// =============================================================================

/// Odczytuje sekret z zaszyfrowanego storage Core.
/// Wymaga uprawnienia "secrets" w manifescie addonu.
pub fn secret_get_value(key: &str) -> Result<Option<String>, String> {
    call_host_kv_get_5(secret_get, key)
}

/// Zapisuje sekret do zaszyfrowanego storage Core.
/// Wymaga uprawnienia "secrets" z access_level "rw".
pub fn secret_set_value(key: &str, value: &str) -> Result<(), String> {
    call_host_kv_set(secret_set, key, value)
}

// =============================================================================
// Wysokopoziomowe wrappery — Logowanie
// =============================================================================

/// Modul logowania — wygodne wrappery do host functions log_*
pub mod log {
    /// Loguje wiadomosc na poziomie INFO
    pub fn info(message: &str) {
        super::call_host_log(super::log_info, message);
    }

    /// Loguje wiadomosc na poziomie WARN
    pub fn warn(message: &str) {
        super::call_host_log(super::log_warn, message);
    }

    /// Loguje wiadomosc na poziomie ERROR
    pub fn error(message: &str) {
        super::call_host_log(super::log_error, message);
    }
}

// =============================================================================
// Wysokopoziomowe wrappery — Uzytkownik
// =============================================================================

/// Dane aktualnego uzytkownika
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurrentUser {
    pub user_id: i64,
    pub username: String,
    pub display_name: String,
    pub email: String,
}

/// Pobiera dane aktualnego uzytkownika.
/// Wymaga uprawnienia "user_info" w manifescie addonu.
pub fn get_current_user() -> Result<CurrentUser, String> {
    let mut buffer = vec![0u8; RESPONSE_BUFFER_SIZE];
    let mut out_len: i32 = 0;

    let result_code = unsafe {
        user_get_current(
            buffer.as_mut_ptr() as i32,
            RESPONSE_BUFFER_SIZE as i32,
            &mut out_len as *mut i32 as i32,
        )
    };

    if result_code < 0 {
        return Err(format!("Blad pobierania danych uzytkownika: {}", result_code));
    }

    if out_len <= 0 {
        return Err("Brak danych uzytkownika".to_string());
    }

    let json_str = String::from_utf8_lossy(&buffer[..out_len as usize]).to_string();
    serde_json::from_str(&json_str)
        .map_err(|e| format!("Blad deserializacji danych uzytkownika: {}", e))
}

// =============================================================================
// Wysokopoziomowe wrappery — Rejestracja narzedzi (tool calling)
// =============================================================================

/// Rejestruje narzedzie (tool) dla LLM tool calling.
/// Narzedzie bedzie dostepne w LLM jako function call.
pub fn register_tool(name: &str, description: &str, params_schema: serde_json::Value) {
    let tool_def = serde_json::json!({
        "name": name,
        "description": description,
        "parameters": params_schema,
    });

    if let Ok(json_str) = serde_json::to_string(&tool_def) {
        let bytes = json_str.as_bytes();
        unsafe {
            tool_register(bytes.as_ptr() as i32, bytes.len() as i32);
        }
    }
}

// =============================================================================
// Wysokopoziomowe wrappery — Siec (TCP/UDP proxy)
// =============================================================================

/// Nawiazuje polaczenie sieciowe TCP/UDP wedlug reguly z manifestu.
/// Wymaga uprawnienia "network" w manifescie i zatwierdzenia reguly przez admina.
/// Zwraca conn_id (u32) do uzytku z network_send/network_recv/network_close.
pub fn network_connect(rule_id: &str) -> Result<u32, i32> {
    let bytes = rule_id.as_bytes();
    let result = unsafe {
        net_connect(bytes.as_ptr() as i32, bytes.len() as i32)
    };
    if result < 0 {
        Err(result)
    } else {
        Ok(result as u32)
    }
}

/// Wysyla dane przez aktywne polaczenie sieciowe.
/// Zwraca liczbe wyslanych bajtow.
pub fn network_send(conn_id: u32, data: &[u8]) -> Result<usize, i32> {
    let result = unsafe {
        net_send(conn_id as i32, data.as_ptr() as i32, data.len() as i32)
    };
    if result < 0 {
        Err(result)
    } else {
        Ok(result as usize)
    }
}

/// Odbiera dane z aktywnego polaczenia sieciowego.
/// Dane sa zapisywane do podanego bufora. Zwraca liczbe odebranych bajtow.
pub fn network_recv(conn_id: u32, buf: &mut [u8]) -> Result<usize, i32> {
    let packed = unsafe {
        net_recv(conn_id as i32, buf.as_mut_ptr() as i32, buf.len() as i32)
    };
    // Rozpakuj: status = gorne 32 bity, bytes_read = dolne 32 bity
    let status = (packed >> 32) as i32;
    let bytes_read = (packed & 0xFFFFFFFF) as usize;
    if status < 0 {
        Err(status)
    } else {
        Ok(bytes_read)
    }
}

/// Zamyka aktywne polaczenie sieciowe.
pub fn network_close(conn_id: u32) -> Result<(), i32> {
    let result = unsafe {
        net_close(conn_id as i32)
    };
    if result != 0 {
        Err(result)
    } else {
        Ok(())
    }
}

// =============================================================================
// Wysokopoziomowe wrappery — Service Request (QUIC przez router)
// =============================================================================

/// Wysyla request do zarejestrowanego serwisu QUIC przez router.
/// Wymaga uprawnienia "service" w manifescie.
/// service_name: nazwa serwisu (np. "teams-bot")
/// request_json: JSON payload requestu
/// Zwraca JSON odpowiedzi z serwisu.
pub fn service_request_call(service_name: &str, request_json: &str) -> Result<String, i32> {
    let svc_bytes = service_name.as_bytes();
    let req_bytes = request_json.as_bytes();
    let mut out_buf = vec![0u8; 65536];
    let mut out_len: i32 = 0;

    let result = unsafe {
        service_request(
            svc_bytes.as_ptr() as i32, svc_bytes.len() as i32,
            req_bytes.as_ptr() as i32, req_bytes.len() as i32,
            out_buf.as_mut_ptr() as i32, out_buf.len() as i32,
            &mut out_len as *mut i32 as i32,
        )
    };

    if result != 0 {
        return Err(result);
    }

    let response = String::from_utf8_lossy(&out_buf[..out_len as usize]).to_string();
    Ok(response)
}

// =============================================================================
// Wysokopoziomowe wrappery — SQL API (F1a M1.W4)
// =============================================================================

/// Reprezentacja wartosci SQL przekazywanej jako parametr lub odebranej
/// z wiersza. Mapowanie 1:1 z ABI (zob. docs sekcja 11):
/// String -> TEXT, I64 -> INTEGER, F64 -> REAL, Bool -> INTEGER 0/1,
/// Null -> NULL, Bytes -> BLOB (przekazywane jako base64 JSON `{"$bytes":"..."}`).
#[derive(Debug, Clone, PartialEq)]
pub enum SqlValue {
    Null,
    Bool(bool),
    I64(i64),
    F64(f64),
    String(String),
    Bytes(Vec<u8>),
}

impl SqlValue {
    /// Reprezentacja JSON kompatybilna z host ABI.
    fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Null => serde_json::Value::Null,
            Self::Bool(b) => serde_json::Value::Bool(*b),
            Self::I64(i) => serde_json::Value::from(*i),
            Self::F64(f) => serde_json::Number::from_f64(*f)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            Self::String(s) => serde_json::Value::String(s.clone()),
            Self::Bytes(b) => {
                use base64::Engine;
                let encoded = base64::engine::general_purpose::STANDARD.encode(b);
                serde_json::json!({ "$bytes": encoded })
            }
        }
    }

    fn from_json(v: &serde_json::Value) -> Self {
        match v {
            serde_json::Value::Null => Self::Null,
            serde_json::Value::Bool(b) => Self::Bool(*b),
            serde_json::Value::Number(n) => n
                .as_i64()
                .map(Self::I64)
                .or_else(|| n.as_f64().map(Self::F64))
                .unwrap_or(Self::Null),
            serde_json::Value::String(s) => Self::String(s.clone()),
            serde_json::Value::Object(obj) => {
                if let Some(serde_json::Value::String(b64)) = obj.get("$bytes") {
                    use base64::Engine;
                    if let Ok(raw) =
                        base64::engine::general_purpose::STANDARD.decode(b64.as_bytes())
                    {
                        return Self::Bytes(raw);
                    }
                }
                Self::Null
            }
            serde_json::Value::Array(_) => Self::Null,
        }
    }

    /// Wygodny dostep do wartosci int.
    pub fn as_i64(&self) -> Option<i64> {
        if let Self::I64(v) = self {
            Some(*v)
        } else {
            None
        }
    }

    /// Wygodny dostep do wartosci string.
    pub fn as_str(&self) -> Option<&str> {
        if let Self::String(s) = self {
            Some(s.as_str())
        } else {
            None
        }
    }
}

/// Wiersz wynikowy SQL — wartosci w kolejnosci kolumn.
pub type SqlRow = Vec<SqlValue>;

/// Wynik DML (sql_exec).
#[derive(Debug, Clone)]
pub struct SqlExecResult {
    pub rows_affected: u64,
    pub last_insert_id: i64,
}

/// Initial buffer for SQL/Alias response (1 KiB — kept small because most
/// responses fit and a retry pulls the actual required size from out_len).
const INITIAL_CAP: usize = 1024;

/// Hard cap on the output buffer for SQL/Alias responses. Matches
/// `PayloadKind::SqlCombined` on the host side. If the response would not
/// fit in this size, the host has misbehaved and we surface PayloadTooLarge
/// rather than allocating unboundedly inside the guest.
const MAX_OUT_CAP: usize = 4 * 1024 * 1024;

/// Hard cap for camera_snapshot responses (RGB24 + base64 expansion). Matches
/// `PayloadKind::ServiceCall` (8 MiB) on the host side. A 1280x720 RGB24 frame
/// is ~3.7 MiB raw → ~4.9 MiB base64-encoded, which would overshoot
/// `MAX_OUT_CAP`; the per-API cap allows the snapshot wrapper to land legit
/// payloads without raising the cap for every other call.
const MAX_OUT_CAP_SNAPSHOT: usize = 8 * 1024 * 1024;

/// Stream subscribe/next/close responses carry only small metadata payloads
/// (stream_id, frame_ref + a few numeric fields, never frame bytes). 4 KiB is
/// well above the realistic ceiling and keeps the guest from following a
/// misbehaving host into a multi-megabyte allocation.
const MAX_OUT_CAP_STREAM: usize = 4 * 1024;

/// Maksymalna liczba prob retry (bez bedu) na pojedynczym callu.
/// W praktyce 1 attempt = sukces, 2 attempt = sukces po znalezieniu rozmiaru.
/// Trzecia proba sugeruje host bug — zwracamy OutputBufferTooSmall.
const MAX_RETRY_ATTEMPTS: u32 = 2;

/// Wykonuje host function SQL/Alias z retry semantics (out_cap → re-alloc).
/// Retry jest ograniczony przez `MAX_RETRY_ATTEMPTS` i hard-cap `MAX_OUT_CAP`,
/// chroniac guest przed nieograniczonymi alokacjami w przypadku bledu host.
fn call_sql_with_two_inputs(
    host_fn: unsafe extern "C" fn(i32, i32, i32, i32, i32, i32, i32) -> i32,
    a: &[u8],
    b: &[u8],
) -> Result<Vec<u8>, AbiError> {
    let mut cap = INITIAL_CAP;
    let mut attempts: u32 = 0;
    loop {
        attempts += 1;
        let mut buffer = vec![0u8; cap];
        let mut out_len: u32 = 0;
        let rc = unsafe {
            host_fn(
                a.as_ptr() as i32,
                a.len() as i32,
                b.as_ptr() as i32,
                b.len() as i32,
                buffer.as_mut_ptr() as i32,
                cap as i32,
                &mut out_len as *mut u32 as i32,
            )
        };
        if rc == 0 {
            buffer.truncate(out_len as usize);
            return Ok(buffer);
        }
        if rc == AbiError::OutputBufferTooSmall.as_i32() {
            // Stop retrying after the second attempt: a correct host gives
            // us the required size on the first try, so any further loop
            // is a host bug — fail rather than spin.
            if attempts > MAX_RETRY_ATTEMPTS {
                return Err(AbiError::OutputBufferTooSmall);
            }
            let required = out_len as usize;
            if required <= cap {
                // Host claims too-small but we already meet the size —
                // protocol violation.
                return Err(AbiError::OutputBufferTooSmall);
            }
            if required > MAX_OUT_CAP {
                // Response would exceed the per-API payload limit. Surface
                // PayloadTooLarge so callers can distinguish from a real
                // out_cap negotiation failure.
                return Err(AbiError::PayloadTooLarge);
            }
            cap = required;
            continue;
        }
        return Err(AbiError::from_i32(rc));
    }
}

fn call_sql_with_one_input(
    host_fn: unsafe extern "C" fn(i32, i32, i32, i32, i32) -> i32,
    a: &[u8],
) -> Result<Vec<u8>, AbiError> {
    call_sql_with_one_input_capped(host_fn, a, MAX_OUT_CAP)
}

fn call_sql_with_one_input_capped(
    host_fn: unsafe extern "C" fn(i32, i32, i32, i32, i32) -> i32,
    a: &[u8],
    max_out_cap: usize,
) -> Result<Vec<u8>, AbiError> {
    let mut cap = INITIAL_CAP;
    let mut attempts: u32 = 0;
    loop {
        attempts += 1;
        let mut buffer = vec![0u8; cap];
        let mut out_len: u32 = 0;
        let rc = unsafe {
            host_fn(
                a.as_ptr() as i32,
                a.len() as i32,
                buffer.as_mut_ptr() as i32,
                cap as i32,
                &mut out_len as *mut u32 as i32,
            )
        };
        if rc == 0 {
            buffer.truncate(out_len as usize);
            return Ok(buffer);
        }
        if rc == AbiError::OutputBufferTooSmall.as_i32() {
            if attempts > MAX_RETRY_ATTEMPTS {
                return Err(AbiError::OutputBufferTooSmall);
            }
            let required = out_len as usize;
            if required <= cap {
                return Err(AbiError::OutputBufferTooSmall);
            }
            if required > max_out_cap {
                return Err(AbiError::PayloadTooLarge);
            }
            cap = required;
            continue;
        }
        return Err(AbiError::from_i32(rc));
    }
}

fn params_to_json(params: &[SqlValue]) -> String {
    let arr: Vec<serde_json::Value> = params.iter().map(|v| v.to_json()).collect();
    serde_json::to_string(&arr).unwrap_or_else(|_| "[]".to_string())
}

/// Wykonuje DML (INSERT/UPDATE/DELETE) z bindowanymi parametrami.
///
/// Wymaga uprawnienia `sql.write` w manifescie oraz `[storage] sql=true`.
/// Bledy zwracane jako `AbiError` (Permission, SqlSyntax, SqlConstraint,
/// Timeout, PayloadTooLarge, ...).
pub fn sql_exec(query: &str, params: &[SqlValue]) -> Result<SqlExecResult, AbiError> {
    let params_json = params_to_json(params);
    let bytes = call_sql_with_two_inputs(sql_exec_v1, query.as_bytes(), params_json.as_bytes())?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| AbiError::Operation)?;
    Ok(SqlExecResult {
        rows_affected: v.get("rows_affected").and_then(|x| x.as_u64()).unwrap_or(0),
        last_insert_id: v.get("last_insert_id").and_then(|x| x.as_i64()).unwrap_or(0),
    })
}

/// Wykonuje SELECT (lub WITH/EXPLAIN) i zwraca wszystkie wiersze.
///
/// Wymaga uprawnienia `sql.read` w manifescie oraz `[storage] sql=true`.
pub fn sql_query(query: &str, params: &[SqlValue]) -> Result<Vec<SqlRow>, AbiError> {
    let params_json = params_to_json(params);
    let bytes = call_sql_with_two_inputs(sql_query_v1, query.as_bytes(), params_json.as_bytes())?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| AbiError::Operation)?;
    let rows = v.get("rows").and_then(|x| x.as_array()).cloned().unwrap_or_default();
    let out: Vec<SqlRow> = rows
        .into_iter()
        .map(|row| {
            row.as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(SqlValue::from_json)
                .collect()
        })
        .collect();
    Ok(out)
}

/// Wykonuje SELECT i zwraca pierwszy wiersz lub None.
pub fn sql_query_one(query: &str, params: &[SqlValue]) -> Result<Option<SqlRow>, AbiError> {
    let params_json = params_to_json(params);
    let bytes =
        call_sql_with_two_inputs(sql_query_one_v1, query.as_bytes(), params_json.as_bytes())?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| AbiError::Operation)?;
    match v.get("row") {
        Some(serde_json::Value::Null) | None => Ok(None),
        Some(serde_json::Value::Array(arr)) => Ok(Some(arr.iter().map(SqlValue::from_json).collect())),
        _ => Err(AbiError::Operation),
    }
}

/// Wykonuje liste statementow atomowo. Wszystkie commited lub wszystkie rolled back.
/// Zwraca laczna liczbe `rows_affected` wszystkich statementow.
pub fn sql_transaction(statements: &[(&str, &[SqlValue])]) -> Result<u64, AbiError> {
    let payload = serde_json::json!({
        "statements": statements.iter().map(|(q, p)| {
            serde_json::json!({
                "query": q,
                "params": p.iter().map(|v| v.to_json()).collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>(),
    });
    let payload_str = serde_json::to_string(&payload).map_err(|_| AbiError::Operation)?;
    let bytes = call_sql_with_one_input(sql_transaction_v1, payload_str.as_bytes())?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| AbiError::Operation)?;
    Ok(v.get("rows_affected_total").and_then(|x| x.as_u64()).unwrap_or(0))
}

// =============================================================================
// Prelude — wygodny re-eksport dla autorow addonow
// =============================================================================

/// Prelude — importuj wszystkie najczesciej uzywane typy i funkcje
pub mod prelude {
    pub use crate::ui;
    pub use crate::{
        read_string, write_string,
        generate,
        store_get, store_set,
        http_get, http_post, http_send, HttpRequest, HttpResponse,
        publish_event, subscribe_event, Event,
        render_panel, render_panel_typed, render_panel_binary, notify, notify_with_level,
        secret_get_value, secret_set_value,
        get_current_user, CurrentUser,
        register_tool,
        network_connect, network_send, network_recv, network_close,
        service_request_call,
        sql_exec, sql_query, sql_query_one, sql_transaction,
        SqlValue, SqlRow, SqlExecResult,
        alias_get, alias_list_owned,
        AliasInfo,
        camera_add, camera_list, camera_get, camera_update, camera_remove,
        camera_snapshot, camera_health, camera_discover, camera_test_connection,
        camera_credentials_rotate,
        CameraAddSpec, CameraAddResult, CameraInfo, CameraUpdateSpec,
        CameraHealthInfo, SnapshotInfo, CameraTestResult,
        stream_subscribe, stream_next, stream_close,
        StreamNextMessage, StreamFrameMeta,
        camera_metadata_subscribe, camera_metadata_poll, camera_metadata_unsubscribe,
        MetadataItem, MetadataFrame, MetadataPollResult,
        recording_save_snapshot, recording_save_segment, recording_get_url,
        recording_get_stream, recording_purge, recording_stats, frame_url,
        SavedRecordingInfo, RecordingUrl, RecordingStream, RecordingStats, FrameUrl,
        vector_upsert, vector_search, vector_delete, encode_vector_b64, VectorHit,
        gate_check, gate_check_scoped, GateCheckResult, GateSigner,
        flow_invoke, flow_status, flow_cancel, FlowInvocation,
        service_list, node_resources_get, ServiceInfo, NodeResources, NodeGpu,
        AbiError,
        log,
    };
    pub use serde::{Deserialize, Serialize};
    pub use serde_json::{self, json, Value};
}

// =============================================================================
// Wysokopoziomowe wrappery — Aliases API (F1a M1.W5, readonly)
// =============================================================================

/// Pelne info o aliasie zwracane przez `alias_get_v1` i `alias_list_owned_v1`.
#[derive(Debug, Clone, Deserialize)]
pub struct AliasInfo {
    pub id: String,
    /// "addon:<id>" lub "manual" lub None gdy brak owner row.
    pub owner: Option<String>,
    pub current_target: String,
    pub fallback_targets: Vec<String>,
    pub strategy: String,
    pub is_active: bool,
    pub last_used_target: Option<String>,
    pub last_used_at: Option<i64>,
    pub calls_24h: u64,
    pub fallback_calls_24h: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct AliasListResponse {
    aliases: Vec<AliasInfo>,
}

/// Pobiera pelne info o aliasie razem ze statystykami (last_used_*,
/// calls_24h, fallback_calls_24h).
///
/// Read access: dowolny addon z `alias.read` (bez ograniczenia
/// ownership). Stats sa widoczne wylacznie dla wlasciciela aliasu i dla
/// manual-owned aliasow — cross-addon caller dostanie metadata + counters
/// = 0 / last_used_* = null.
pub fn alias_get(alias_id: &str) -> Result<AliasInfo, AbiError> {
    let bytes = call_sql_with_one_input(alias_get_v1, alias_id.as_bytes())?;
    serde_json::from_slice(&bytes).map_err(|_| AbiError::Operation)
}

/// Zwraca liste aliasow nalezacych do biezacego addona (owner_id =
/// caller). Inne aliasy (manual, owned by innym addonem) sa pomijane.
pub fn alias_list_owned() -> Result<Vec<AliasInfo>, AbiError> {
    // Host function bez argumentow wejsciowych: invoke direct z retry pattern
    // chronionym przez te same gwarancje (MAX_OUT_CAP, MAX_RETRY_ATTEMPTS) co
    // call_sql_with_*.
    let mut cap = INITIAL_CAP;
    let mut attempts: u32 = 0;
    loop {
        attempts += 1;
        let mut buffer = vec![0u8; cap];
        let mut out_len: u32 = 0;
        let rc = unsafe {
            alias_list_owned_v1(
                buffer.as_mut_ptr() as i32,
                cap as i32,
                &mut out_len as *mut u32 as i32,
            )
        };
        if rc == 0 {
            buffer.truncate(out_len as usize);
            let resp: AliasListResponse =
                serde_json::from_slice(&buffer).map_err(|_| AbiError::Operation)?;
            return Ok(resp.aliases);
        }
        if rc == AbiError::OutputBufferTooSmall.as_i32() {
            if attempts > MAX_RETRY_ATTEMPTS {
                return Err(AbiError::OutputBufferTooSmall);
            }
            let required = out_len as usize;
            if required <= cap {
                return Err(AbiError::OutputBufferTooSmall);
            }
            if required > MAX_OUT_CAP {
                return Err(AbiError::PayloadTooLarge);
            }
            cap = required;
            continue;
        }
        return Err(AbiError::from_i32(rc));
    }
}

// =============================================================================
// Camera API (F1a M1.W6) — TentaVision camera ingest
// =============================================================================
//
// Wrapper-y woke host functions camera_*_v1. Payload to CBOR; bledy mapowane na
// `AbiError`. Pelna specyfikacja: `docs/ADDON_HOST_FUNCTIONS.md` sekcja 13.
//
// **All `camera_*` wrappers require TentaFlow core built with
// `--features camera`.** Without that feature the host does not register the
// imports and addon instantiation fails at module-link time with a
// "missing import" error from wasmtime — there is no silent-fail path.

/// Specyfikacja nowej kamery do `camera_add`. F1a obsluguje wylacznie
/// `vendor = "fake_file"`; pozostale vendor-y dadza `CameraVendorUnsupported`.
#[derive(Debug, Clone)]
pub struct CameraAddSpec {
    pub display_name: String,
    pub vendor: String,
    pub url: String,
    pub target_fps: u32,
    pub resolution: Option<(u32, u32)>,
    pub retention_class: String,
    pub profile: String,
    /// Base64-encoded `user:pass` for vendors requiring auth (RTSP/ONVIF).
    /// Required for `vendor = "onvif"`; the host also uses it for the SOAP
    /// UsernameToken digest. `None` for credential-less vendors (`fake_file`).
    pub credentials_b64: Option<String>,
    /// `vendor = "onvif"` only: pins a specific media profile. `None` selects
    /// the first profile returned by `GetProfiles`.
    pub onvif_profile_token: Option<String>,
}

impl Default for CameraAddSpec {
    fn default() -> Self {
        Self {
            display_name: String::new(),
            vendor: "fake_file".to_string(),
            url: String::new(),
            target_fps: 30,
            resolution: None,
            retention_class: "C".to_string(),
            profile: "default".to_string(),
            credentials_b64: None,
            onvif_profile_token: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CameraAddResult {
    pub camera_id: String,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct CameraInfo {
    pub camera_id: String,
    pub display_name: String,
    pub vendor: String,
    pub url: String,
    pub target_fps: i64,
    pub resolution_width: Option<i64>,
    pub resolution_height: Option<i64>,
    pub status: String,
    pub status_message: Option<String>,
    pub fps_actual: Option<f64>,
    pub last_frame_at: Option<i64>,
    pub retention_class: String,
    pub profile: String,
}

impl From<tentaflow_sdk_spec::CameraInfoOut> for CameraInfo {
    fn from(o: tentaflow_sdk_spec::CameraInfoOut) -> Self {
        Self {
            camera_id: o.camera_id,
            display_name: o.display_name,
            vendor: o.vendor,
            url: o.url,
            target_fps: o.target_fps,
            resolution_width: o.resolution_width,
            resolution_height: o.resolution_height,
            status: o.status,
            status_message: o.status_message,
            fps_actual: o.fps_actual,
            last_frame_at: o.last_frame_at,
            retention_class: o.retention_class,
            profile: o.profile,
        }
    }
}

/// Partial update for `camera_update`. URL i vendor sa nie do zmiany w F1a —
/// rebind wymaga remove + add.
#[derive(Debug, Default, Clone)]
pub struct CameraUpdateSpec {
    pub camera_id: String,
    pub display_name: Option<String>,
    pub target_fps: Option<u32>,
    pub resolution_width: Option<u32>,
    pub resolution_height: Option<u32>,
    pub retention_class: Option<String>,
    pub profile: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CameraHealthInfo {
    pub camera_id: String,
    pub status: String,
    pub status_message: String,
    pub fps_actual: f64,
    pub last_frame_at: i64,
    pub frames_total: u64,
    pub frames_dropped: u64,
}

/// Wynik `camera_snapshot` — RGB24 frame zdekodowany z base64.
#[derive(Debug, Clone)]
pub struct SnapshotInfo {
    pub camera_id: String,
    pub width: u32,
    pub height: u32,
    pub pixel_format: String,
    pub timestamp_unix_ms: u64,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct CameraTestResult {
    pub ok: bool,
    pub message: String,
}

/// Decodes a CBOR host response into a shared `tentaflow-sdk-spec` ABI struct.
/// `minicbor::decode` stops at the first complete value, so a valid prefix
/// followed by trailing bytes would otherwise be accepted; this requires the
/// whole response to be consumed.
fn decode_cbor<T>(bytes: &[u8]) -> Result<T, AbiError>
where
    T: for<'b> minicbor::Decode<'b, ()>,
{
    let mut decoder = minicbor::Decoder::new(bytes);
    let value = decoder.decode::<T>().map_err(|_| AbiError::Operation)?;
    if decoder.position() != bytes.len() {
        return Err(AbiError::Operation);
    }
    Ok(value)
}

fn parse_toml<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, AbiError> {
    let s = std::str::from_utf8(bytes).map_err(|_| AbiError::Operation)?;
    toml::from_str::<T>(s).map_err(|_| AbiError::Operation)
}

fn call_host_no_input(
    host_fn: unsafe extern "C" fn(i32, i32, i32) -> i32,
) -> Result<Vec<u8>, AbiError> {
    let mut cap = INITIAL_CAP;
    let mut attempts: u32 = 0;
    loop {
        attempts += 1;
        let mut buffer = vec![0u8; cap];
        let mut out_len: u32 = 0;
        let rc = unsafe {
            host_fn(
                buffer.as_mut_ptr() as i32,
                cap as i32,
                &mut out_len as *mut u32 as i32,
            )
        };
        if rc == 0 {
            buffer.truncate(out_len as usize);
            return Ok(buffer);
        }
        if rc == AbiError::OutputBufferTooSmall.as_i32() {
            if attempts > MAX_RETRY_ATTEMPTS {
                return Err(AbiError::OutputBufferTooSmall);
            }
            let required = out_len as usize;
            if required <= cap {
                return Err(AbiError::OutputBufferTooSmall);
            }
            if required > MAX_OUT_CAP {
                return Err(AbiError::PayloadTooLarge);
            }
            cap = required;
            continue;
        }
        return Err(AbiError::from_i32(rc));
    }
}

/// Encodes a shared `tentaflow-sdk-spec` ABI input struct to CBOR for the
/// host call.
fn encode_cbor_input<T: minicbor::Encode<()>>(value: &T) -> Result<Vec<u8>, AbiError> {
    let mut buf = Vec::new();
    minicbor::encode(value, &mut buf).map_err(|_| AbiError::Operation)?;
    Ok(buf)
}

fn camera_add_payload(spec: &CameraAddSpec) -> tentaflow_sdk_spec::CameraAddInput {
    let (resolution_width, resolution_height) = match spec.resolution {
        Some((w, h)) => (Some(w), Some(h)),
        None => (None, None),
    };
    tentaflow_sdk_spec::CameraAddInput {
        display_name: spec.display_name.clone(),
        vendor: spec.vendor.clone(),
        url: spec.url.clone(),
        target_fps: Some(spec.target_fps),
        resolution_width,
        resolution_height,
        retention_class: Some(spec.retention_class.clone()),
        profile: Some(spec.profile.clone()),
        credentials_b64: spec.credentials_b64.clone(),
        onvif_profile_token: spec.onvif_profile_token.clone(),
    }
}

/// Rejestruje nowa kamere w supervisor + DB. F1a vendor whitelist: `fake_file`.
pub fn camera_add(spec: &CameraAddSpec) -> Result<CameraAddResult, AbiError> {
    let payload = encode_cbor_input(&camera_add_payload(spec))?;
    let bytes = call_sql_with_one_input(camera_add_v1, &payload)?;
    let out: tentaflow_sdk_spec::CameraAddOutput = decode_cbor(&bytes)?;
    Ok(CameraAddResult {
        camera_id: out.camera_id,
        status: out.status,
    })
}

/// Zwraca wszystkie kamery nalezace do wywolujacego addona. Kazdy wpis zawiera
/// runtime metryki (`fps_actual`, `status`) z supervisora gdy session jest
/// aktywna; w przeciwnym razie wartosci z DB (po restarcie hosta).
pub fn camera_list() -> Result<Vec<CameraInfo>, AbiError> {
    let bytes = call_host_no_input(camera_list_v1)?;
    let resp: tentaflow_sdk_spec::CameraListOut = decode_cbor(&bytes)?;
    Ok(resp.camera.into_iter().map(CameraInfo::from).collect())
}

/// Pobiera pojedynczy `CameraInfo`. Zwraca `NotFound` gdy kamera nie istnieje
/// lub nalezy do innego addona (kanalu bocznego nie ma — nie da sie wnioskowac
/// o istnieniu cudzych camera_id).
pub fn camera_get(camera_id: &str) -> Result<CameraInfo, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::CameraIdInput {
        camera_id: camera_id.to_string(),
    })?;
    let bytes = call_sql_with_one_input(camera_get_v1, &payload)?;
    let out: tentaflow_sdk_spec::CameraInfoOut = decode_cbor(&bytes)?;
    Ok(CameraInfo::from(out))
}

/// Patch on-the-fly. Vendor + URL sa niezmienne — change them by remove + add.
pub fn camera_update(spec: &CameraUpdateSpec) -> Result<CameraInfo, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::CameraUpdateInput {
        camera_id: spec.camera_id.clone(),
        display_name: spec.display_name.clone(),
        target_fps: spec.target_fps,
        resolution_width: spec.resolution_width,
        resolution_height: spec.resolution_height,
        retention_class: spec.retention_class.clone(),
        profile: spec.profile.clone(),
    })?;
    let bytes = call_sql_with_one_input(camera_update_v1, &payload)?;
    let out: tentaflow_sdk_spec::CameraInfoOut = decode_cbor(&bytes)?;
    Ok(CameraInfo::from(out))
}

/// Soft-delete (stamps `removed_at`). Idempotent w sensie ABI: druga proba na
/// tym samym camera_id zwraca `NotFound`.
pub fn camera_remove(camera_id: &str) -> Result<(), AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::CameraIdInput {
        camera_id: camera_id.to_string(),
    })?;
    let bytes = call_sql_with_one_input(camera_remove_v1, &payload)?;
    let _: tentaflow_sdk_spec::CameraRemoveOut = decode_cbor(&bytes)?;
    Ok(())
}

/// Snapshot ostatniej ramki — RGB24 zdekodowany z base64. Maks ~5.5MB raw
/// (1280x720 mieci sie w PayloadKind::ServiceCall; 1920x1080 przekroczy limit
/// i zwroci `PayloadTooLarge`).
///
/// Requires TentaFlow core built with `--features camera`. Without it
/// addon instantiation fails at module-link time with "missing import".
pub fn camera_snapshot(camera_id: &str) -> Result<SnapshotInfo, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::CameraIdInput {
        camera_id: camera_id.to_string(),
    })?;
    let bytes = call_sql_with_one_input_capped(
        camera_snapshot_v1,
        &payload,
        MAX_OUT_CAP_SNAPSHOT,
    )?;
    let raw: tentaflow_sdk_spec::CameraSnapshotOut = decode_cbor(&bytes)?;
    let data = base64::engine::general_purpose::STANDARD
        .decode(raw.data_b64.as_bytes())
        .map_err(|_| AbiError::Operation)?;
    Ok(SnapshotInfo {
        camera_id: raw.camera_id,
        width: raw.width,
        height: raw.height,
        pixel_format: raw.pixel_format,
        timestamp_unix_ms: raw.timestamp_unix_ms,
        data,
    })
}

/// Health + runtime metryki z supervisora. Gdy session zniknal (np. restart
/// hosta przed Issue #8 fix), zwraca `status_message = "session missing"` +
/// metryki = 0.
pub fn camera_health(camera_id: &str) -> Result<CameraHealthInfo, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::CameraIdInput {
        camera_id: camera_id.to_string(),
    })?;
    let bytes = call_sql_with_one_input(camera_health_v1, &payload)?;
    let out: tentaflow_sdk_spec::CameraHealthOut = decode_cbor(&bytes)?;
    Ok(CameraHealthInfo {
        camera_id: out.camera_id,
        status: out.status,
        status_message: out.status_message,
        fps_actual: out.fps_actual,
        last_frame_at: out.last_frame_at,
        frames_total: out.frames_total,
        frames_dropped: out.frames_dropped,
    })
}

/// WS-Discovery na lokalnej sieci LAN — zwraca wykryte urzadzenia ONVIF
/// zmapowane na `CameraInfo` (jeszcze bez `camera_id`).
pub fn camera_discover() -> Result<Vec<CameraInfo>, AbiError> {
    let bytes = call_host_no_input(camera_discover_v1)?;
    let resp: tentaflow_sdk_spec::CameraDiscoverOut = decode_cbor(&bytes)?;
    Ok(resp
        .discovered
        .into_iter()
        .map(|d| CameraInfo {
            camera_id: String::new(),
            display_name: d.model.clone(),
            vendor: "onvif".to_string(),
            url: d.xaddrs.first().cloned().unwrap_or_default(),
            target_fps: 0,
            resolution_width: None,
            resolution_height: None,
            status: "discovered".to_string(),
            status_message: Some(format!("{} {}", d.manufacturer, d.model)),
            fps_actual: None,
            last_frame_at: None,
            retention_class: String::new(),
            profile: String::new(),
        })
        .collect())
}

/// Lightweight probe — sprawdza czy URL kamery jest osiagalny dla danego
/// vendora. Dla `fake_file` sprawdza ze plik istnieje, jest plikiem regularnym
/// i nie zawiera symlinkow w sciezce.
pub fn camera_test_connection(vendor: &str, url: &str) -> Result<CameraTestResult, AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::CameraTestConnectionInput {
        vendor: vendor.to_string(),
        url: url.to_string(),
    })?;
    let bytes = call_sql_with_one_input(camera_test_connection_v1, &payload)?;
    let out: tentaflow_sdk_spec::CameraTestConnectionOut = decode_cbor(&bytes)?;
    Ok(CameraTestResult {
        ok: out.ok,
        message: out.message,
    })
}

/// Rotacja credentiali dla vendorow wymagajacych auth (RTSP/ONVIF).
/// `new_credentials_b64 = None` czysci credential. Zwraca `(rotated, reason)`.
pub fn camera_credentials_rotate(
    camera_id: &str,
    new_credentials_b64: Option<&str>,
) -> Result<(bool, String), AbiError> {
    let payload = encode_cbor_input(&tentaflow_sdk_spec::CameraCredentialsRotateInput {
        camera_id: camera_id.to_string(),
        new_credentials_b64: new_credentials_b64.map(|s| s.to_string()),
    })?;
    let bytes = call_sql_with_one_input(camera_credentials_rotate_v1, &payload)?;
    let out: tentaflow_sdk_spec::CameraCredentialsRotateOut = decode_cbor(&bytes)?;
    Ok((out.rotated, out.reason))
}

// =============================================================================
// Streaming API wrappers (F1a M1.W7) — `stream_subscribe / next / close`.
//
// **All `stream_*` wrappers require TentaFlow core built with
// `--features camera`.** Without it the host functions are not registered and
// module instantiation fails at link time with "missing import".
// =============================================================================

/// Payload metadata for a Frame message returned by `stream_next`. Bytes live
/// in the core LRU and travel to a service via `service_call` + PickupToken —
/// the addon never receives them inline.
#[derive(Debug, Clone)]
pub struct StreamFrameMeta {
    pub frame_ref: String,
    pub camera_id: String,
    pub width: u32,
    pub height: u32,
    pub pixel_format: String,
    pub timestamp_unix_ms: u64,
}

/// Message variants the addon can observe on a subscribed stream.
#[derive(Debug, Clone)]
pub enum StreamNextMessage {
    Frame(StreamFrameMeta),
    Drop { count: u64 },
    CameraOffline { reason: String },
    StreamClosed,
    Timeout,
}

/// Subscribe to a camera's frame bus. F1a target format: `camera:<camera_id>`.
/// Ownership is enforced — addons cannot subscribe to cameras owned by other
/// addons (returns `NotFound`).
pub fn stream_subscribe(target: &str, max_fps: Option<u32>) -> Result<String, AbiError> {
    let input = tentaflow_sdk_spec::StreamSubscribeInput {
        target: target.to_string(),
        filter: max_fps.map(|fps| tentaflow_sdk_spec::StreamSubscribeFilter {
            max_fps: Some(fps),
            skip_frames: Some(0),
        }),
    };
    let payload = encode_cbor_input(&input)?;
    let bytes = call_sql_with_one_input_capped(stream_subscribe_v1, &payload, MAX_OUT_CAP_STREAM)?;
    let out: tentaflow_sdk_spec::StreamSubscribeOutput = decode_cbor(&bytes)?;
    Ok(out.stream_id)
}

/// Bounded-await poll for the next stream message. `timeout_ms` is clamped to
/// 5000 ms by the host.
pub fn stream_next(stream_id: &str, timeout_ms: u64) -> Result<StreamNextMessage, AbiError> {
    let input = tentaflow_sdk_spec::StreamNextInput {
        stream_id: stream_id.to_string(),
        timeout_ms,
    };
    let payload = encode_cbor_input(&input)?;
    let bytes = call_sql_with_one_input_capped(stream_next_v1, &payload, MAX_OUT_CAP_STREAM)?;
    let out: tentaflow_sdk_spec::StreamNextOutput = decode_cbor(&bytes)?;
    // Map the host's tagged output onto the SDK enum. A missing field for the
    // declared `kind` means the host produced a malformed frame, which we
    // surface as `Operation` rather than silently dropping it.
    match out.kind.as_str() {
        "frame" => Ok(StreamNextMessage::Frame(StreamFrameMeta {
            frame_ref: out.frame_ref.ok_or(AbiError::Operation)?,
            camera_id: out.camera_id.ok_or(AbiError::Operation)?,
            width: out.width.ok_or(AbiError::Operation)?,
            height: out.height.ok_or(AbiError::Operation)?,
            pixel_format: out.pixel_format.ok_or(AbiError::Operation)?,
            timestamp_unix_ms: out.timestamp_unix_ms.ok_or(AbiError::Operation)?,
        })),
        "drop" => Ok(StreamNextMessage::Drop {
            count: out.count.ok_or(AbiError::Operation)?,
        }),
        "camera_offline" => Ok(StreamNextMessage::CameraOffline {
            reason: out.reason.ok_or(AbiError::Operation)?,
        }),
        "stream_closed" => Ok(StreamNextMessage::StreamClosed),
        "timeout" => Ok(StreamNextMessage::Timeout),
        _ => Err(AbiError::Operation),
    }
}

/// Drop the subscription. Subsequent `stream_next` calls for the same id
/// return `StreamNotFound`.
pub fn stream_close(stream_id: &str) -> Result<(), AbiError> {
    let input = tentaflow_sdk_spec::StreamCloseInput {
        stream_id: stream_id.to_string(),
    };
    let payload = encode_cbor_input(&input)?;
    let bytes = call_sql_with_one_input_capped(stream_close_v1, &payload, MAX_OUT_CAP_STREAM)?;
    let _: tentaflow_sdk_spec::StreamCloseOutput = decode_cbor(&bytes)?;
    Ok(())
}

// =============================================================================
// Camera metadata API wrappers (F2 P6.b) — ONVIF analytics events.
//
// Subscribe to a camera that already has `metadata_supported = true` (set
// when ONVIF discovery found a non-empty Media2 metadata configuration).
// Each `camera_metadata_poll` returns up to `max_items` analytics frames
// pulled from the camera's PullPoint stream, plus backpressure / offline
// signals. The host fn enforces `camera.metadata` permission, org isolation
// and the metadata_supported gate.
//
// Requires TentaFlow core built with `--features camera`.
// =============================================================================

/// One detected object inside a `MetadataFrame`. Fields mirror the ONVIF
/// analytics schema: `class` (e.g. "Vehicle"), confidence in 0..1,
/// optional `bbox` as `[left, top, right, bottom]` in normalised 0..1
/// device coordinates, and an optional `track_id` for cross-frame
/// correlation.
#[derive(Debug, Clone, Deserialize)]
pub struct MetadataItem {
    pub class: String,
    pub confidence: f64,
    #[serde(default)]
    pub bbox: Option<[f64; 4]>,
    #[serde(default)]
    pub track_id: Option<String>,
}

/// One analytics frame — a single device tick that produced one or more
/// detections.
#[derive(Debug, Clone, Deserialize)]
pub struct MetadataFrame {
    pub camera_id: String,
    /// Event timestamp in unix milliseconds (as supplied by the device,
    /// times 1000). May be 0 if the camera omitted the `UtcTime` attribute.
    pub ts_unix_ms: i64,
    pub items: Vec<MetadataItem>,
}

#[derive(Debug, Clone, Deserialize)]
struct MetadataPollRaw {
    #[serde(default)]
    frames: Vec<MetadataFrame>,
    #[serde(default)]
    camera_offline: bool,
    #[serde(default)]
    dropped: u64,
}

/// Aggregate poll outcome — frames plus optional backpressure / offline
/// signals.
#[derive(Debug, Clone)]
pub struct MetadataPollResult {
    pub frames: Vec<MetadataFrame>,
    /// True iff the camera went offline mid-poll (the supervisor task
    /// exited or `CameraOffline` was raised on the bus). The addon should
    /// stop polling this subscription.
    pub camera_offline: bool,
    /// Number of frames the host dropped due to bus backpressure since the
    /// last successful poll.
    pub dropped: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct MetadataSubscribeOut {
    subscription_id: String,
    #[allow(dead_code)]
    status: String,
}

#[derive(Debug, Clone, Deserialize)]
struct MetadataUnsubscribeOut {
    unsubscribed: bool,
}

/// Subscribe to a camera's ONVIF analytics-metadata stream. The host spawns
/// (or refcounts) a per-camera PullPoint task; the first subscriber pays
/// the cost of `CreatePullPointSubscription` against the camera. Errors:
///
///   * `Permission` — addon lacks `camera.metadata` or the camera lives in
///                    another org.
///   * `NotFound`   — camera does not exist (or is not owned by this addon).
///   * `Operation`  — camera does not advertise metadata
///                    (`metadata_supported = false`), or ONVIF credentials
///                    are missing.
///   * `CameraUnreachable` — transport error talking to the events service.
pub fn camera_metadata_subscribe(camera_id: &str) -> Result<String, AbiError> {
    let payload = format!(
        "camera_id = {}\n",
        toml::Value::String(camera_id.to_string()),
    );
    let bytes = call_sql_with_one_input_capped(
        camera_metadata_subscribe_v1,
        payload.as_bytes(),
        MAX_OUT_CAP_STREAM,
    )?;
    let out: MetadataSubscribeOut = parse_toml(&bytes)?;
    Ok(out.subscription_id)
}

/// Bounded-await poll for the next batch of analytics frames. `timeout_ms`
/// is clamped to 30 000 ms host-side; `max_items` is clamped to 100.
pub fn camera_metadata_poll(
    subscription_id: &str,
    max_items: u32,
    timeout_ms: u32,
) -> Result<MetadataPollResult, AbiError> {
    let payload = format!(
        "subscription_id = {}\nmax_items = {}\ntimeout_ms = {}\n",
        toml::Value::String(subscription_id.to_string()),
        max_items,
        timeout_ms,
    );
    let bytes = call_sql_with_one_input_capped(
        camera_metadata_poll_v1,
        payload.as_bytes(),
        MAX_OUT_CAP_STREAM,
    )?;
    let raw: MetadataPollRaw = parse_toml(&bytes)?;
    Ok(MetadataPollResult {
        frames: raw.frames,
        camera_offline: raw.camera_offline,
        dropped: raw.dropped,
    })
}

/// Drop the subscription. Idempotent: a second call for the same id
/// (or one for an unknown id) returns `Ok(false)`. The supervisor pull
/// task is cancelled when the last addon unsubscribes from the camera.
pub fn camera_metadata_unsubscribe(subscription_id: &str) -> Result<bool, AbiError> {
    let payload = format!(
        "subscription_id = {}\n",
        toml::Value::String(subscription_id.to_string()),
    );
    let bytes = call_sql_with_one_input_capped(
        camera_metadata_unsubscribe_v1,
        payload.as_bytes(),
        MAX_OUT_CAP_STREAM,
    )?;
    let out: MetadataUnsubscribeOut = parse_toml(&bytes)?;
    Ok(out.unsubscribed)
}

// =============================================================================
// Recording API wrappers (F1a M1.W8) — snapshots, segments, signed URLs.
//
// All wrappers require TentaFlow core built with `--features camera`.
// =============================================================================

/// Metadata for a recording artifact persisted on the host (PNG snapshot or
/// MP4 segment). `recording_ref` is the public handle (`snap_<uuid>` /
/// `clip_<uuid>`) used by the other recording APIs.
#[derive(Debug, Clone, Deserialize)]
pub struct SavedRecordingInfo {
    pub recording_ref: String,
    pub file_path: String,
    pub file_size_bytes: u64,
    #[serde(default)]
    pub duration_ms: Option<u32>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    pub hash_sha256: String,
    pub created_at: u64,
}

/// Signed URL for a stored recording or a raw frame. Multi-use until expiry.
#[derive(Debug, Clone, Deserialize)]
pub struct RecordingUrl {
    pub url: String,
    pub expires_unix_ms: u64,
}

/// Signed URL for a raw frame in the LRU. Shape mirrors `RecordingUrl` so the
/// SDK surface stays symmetric; lives as its own type for self-documenting
/// call sites.
#[derive(Debug, Clone, Deserialize)]
pub struct FrameUrl {
    pub url: String,
    pub expires_unix_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecordingStatsPerCamera {
    pub camera_id: String,
    pub snapshots: u64,
    pub segments: u64,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecordingStats {
    pub total_snapshots: u64,
    pub total_segments: u64,
    pub total_size_bytes: u64,
    #[serde(default)]
    pub per_camera: Vec<RecordingStatsPerCamera>,
}

#[derive(Debug, Clone, Deserialize)]
struct RecordingStatsRaw {
    stats: RecordingStatsTotalsRaw,
    #[serde(default)]
    per_camera: Vec<RecordingStatsPerCamera>,
}

#[derive(Debug, Clone, Deserialize)]
struct RecordingStatsTotalsRaw {
    total_snapshots: u64,
    total_segments: u64,
    total_size_bytes: u64,
}

/// Inline raw bytes of a stored recording plus integrity metadata so the addon
/// can verify the payload against the host's SHA-256 hash before consuming it.
#[derive(Debug, Clone)]
pub struct RecordingStream {
    pub bytes: Vec<u8>,
    pub file_size_bytes: u64,
    pub hash_sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
struct RecordingGetStreamRaw {
    data_b64: String,
    file_size_bytes: u64,
    hash_sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
struct RecordingPurgeRaw {
    #[allow(dead_code)]
    purged: bool,
}

fn push_kv_str(s: &mut String, key: &str, value: &str) {
    s.push_str(&format!("{} = {}\n", key, toml::Value::String(value.to_string())));
}

/// Persist a PNG snapshot for a frame already living in the host's LRU.
/// Requires TentaFlow core built with `--features camera`.
pub fn recording_save_snapshot(
    camera_id: &str,
    frame_ref: &str,
    retention_class: Option<&str>,
) -> Result<SavedRecordingInfo, AbiError> {
    let mut s = String::new();
    push_kv_str(&mut s, "camera_id", camera_id);
    push_kv_str(&mut s, "frame_ref", frame_ref);
    if let Some(rc) = retention_class {
        push_kv_str(&mut s, "retention_class", rc);
    }
    let bytes = call_sql_with_one_input(recording_save_snapshot_v1, s.as_bytes())?;
    parse_toml(&bytes)
}

/// Capture `duration_secs` of the camera's bound source into an MP4 segment.
/// The source is always derived host-side from the owning camera row — addons
/// cannot supply an arbitrary `source_url`. F1a accepts only cameras with
/// `vendor='fake_file'`.
/// Requires TentaFlow core built with `--features camera`.
pub fn recording_save_segment(
    camera_id: &str,
    duration_secs: u32,
    retention_class: Option<&str>,
) -> Result<SavedRecordingInfo, AbiError> {
    let mut s = String::new();
    push_kv_str(&mut s, "camera_id", camera_id);
    s.push_str(&format!("duration_secs = {}\n", duration_secs));
    if let Some(rc) = retention_class {
        push_kv_str(&mut s, "retention_class", rc);
    }
    let bytes = call_sql_with_one_input(recording_save_segment_v1, s.as_bytes())?;
    parse_toml(&bytes)
}

/// Issue a multi-use signed URL for a stored recording. TTL must be in
/// `60..=3600` seconds.
/// Requires TentaFlow core built with `--features camera`.
pub fn recording_get_url(recording_ref: &str, ttl_secs: u64) -> Result<RecordingUrl, AbiError> {
    let mut s = String::new();
    push_kv_str(&mut s, "recording_ref", recording_ref);
    s.push_str(&format!("ttl_secs = {}\n", ttl_secs));
    let bytes = call_sql_with_one_input(recording_get_url_v1, s.as_bytes())?;
    parse_toml(&bytes)
}

/// Fetch the raw bytes (PNG or MP4) of a stored recording inline together with
/// the host's reported size and SHA-256 hash. The TOML envelope is hard-capped
/// at 8 MiB; after base64 expansion this admits files up to ~6 MiB raw. Larger
/// artifacts must be fetched via the signed URL + HTTP handler.
/// Requires TentaFlow core built with `--features camera`.
pub fn recording_get_stream(recording_ref: &str) -> Result<RecordingStream, AbiError> {
    let mut s = String::new();
    push_kv_str(&mut s, "recording_ref", recording_ref);
    let bytes = call_sql_with_one_input_capped(
        recording_get_stream_v1,
        s.as_bytes(),
        MAX_OUT_CAP_SNAPSHOT,
    )?;
    let raw: RecordingGetStreamRaw = parse_toml(&bytes)?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(raw.data_b64.as_bytes())
        .map_err(|_| AbiError::Operation)?;
    Ok(RecordingStream {
        bytes: decoded,
        file_size_bytes: raw.file_size_bytes,
        hash_sha256: raw.hash_sha256,
    })
}

/// Soft-delete + filesystem purge. Idempotent: a second call on the same ref
/// returns `NotFound`.
/// Requires TentaFlow core built with `--features camera`.
pub fn recording_purge(recording_ref: &str) -> Result<(), AbiError> {
    let mut s = String::new();
    push_kv_str(&mut s, "recording_ref", recording_ref);
    let bytes = call_sql_with_one_input(recording_purge_v1, s.as_bytes())?;
    let _: RecordingPurgeRaw = parse_toml(&bytes)?;
    Ok(())
}

/// Aggregate recording counts + size per addon (optionally narrowed to a
/// single camera).
/// Requires TentaFlow core built with `--features camera`.
pub fn recording_stats(camera_id: Option<&str>) -> Result<RecordingStats, AbiError> {
    let mut s = String::new();
    if let Some(cam) = camera_id {
        push_kv_str(&mut s, "camera_id", cam);
    }
    let bytes = call_sql_with_one_input(recording_stats_v1, s.as_bytes())?;
    let raw: RecordingStatsRaw = parse_toml(&bytes)?;
    Ok(RecordingStats {
        total_snapshots: raw.stats.total_snapshots,
        total_segments: raw.stats.total_segments,
        total_size_bytes: raw.stats.total_size_bytes,
        per_camera: raw.per_camera,
    })
}

/// Issue a multi-use signed URL for a raw frame in the host LRU. TTL must be
/// in `60..=600` seconds. Frame must belong to a camera owned by the calling
/// addon.
/// Requires TentaFlow core built with `--features camera`.
pub fn frame_url(frame_ref: &str, ttl_secs: u64) -> Result<FrameUrl, AbiError> {
    let mut s = String::new();
    push_kv_str(&mut s, "frame_ref", frame_ref);
    s.push_str(&format!("ttl_secs = {}\n", ttl_secs));
    let bytes = call_sql_with_one_input(frame_url_v1, s.as_bytes())?;
    parse_toml(&bytes)
}

// =============================================================================
// Vector API wrappers (F1c P3) — embedded HNSW per-namespace storage
// =============================================================================

/// One hit returned by `vector_search`. `ref_id` is the key the addon supplied
/// during `vector_upsert`; `score` is the raw metric distance (lower = closer
/// for cosine/euclidean; `1 - dot` for dot).
#[derive(Debug, Clone, Deserialize)]
pub struct VectorHit {
    pub ref_id: u64,
    pub score: f32,
}

#[derive(Debug, Deserialize)]
struct VectorUpsertResponse {
    pub namespace: String,
    pub ref_id: u64,
    pub count: u64,
}

#[derive(Debug, Deserialize)]
struct VectorSearchResponse {
    pub namespace: String,
    #[serde(default)]
    pub hits: Vec<VectorHit>,
}

#[derive(Debug, Deserialize)]
struct VectorDeleteResponse {
    pub namespace: String,
    pub ref_id: u64,
    pub removed: bool,
    pub count: u64,
}

/// Encode a `&[f32]` slice as base64(little-endian f32 bytes) for the vector
/// host functions. Exposed publicly so addons can pre-encode embeddings once
/// and reuse the string across upsert/search calls.
pub fn encode_vector_b64(vector: &[f32]) -> String {
    let mut raw = Vec::with_capacity(vector.len() * 4);
    for f in vector {
        raw.extend_from_slice(&f.to_le_bytes());
    }
    base64::engine::general_purpose::STANDARD.encode(&raw)
}

/// Insert or replace a vector under `ref_id` in `namespace`. Returns the
/// total vector count after the upsert. Requires `vector.write` permission
/// and the namespace must be declared in the addon manifest under
/// `[[vector_namespace]]`.
pub fn vector_upsert(namespace: &str, ref_id: u64, vector: &[f32]) -> Result<u64, AbiError> {
    let mut s = String::new();
    push_kv_str(&mut s, "namespace", namespace);
    s.push_str(&format!("ref_id = {}\n", ref_id));
    push_kv_str(&mut s, "vector_b64", &encode_vector_b64(vector));
    let bytes = call_sql_with_one_input(vector_upsert_v1, s.as_bytes())?;
    let resp: VectorUpsertResponse = parse_toml(&bytes)?;
    Ok(resp.count)
}

/// Top-k k-NN search. Pass `gate_claim_id = Some(...)` when the namespace
/// declares a `gate` in the manifest (P4 policy/claims engine validates the
/// claim; P3 only enforces the structural presence).
pub fn vector_search(
    namespace: &str,
    query: &[f32],
    k: u32,
    gate_claim_id: Option<&str>,
) -> Result<Vec<VectorHit>, AbiError> {
    let mut s = String::new();
    push_kv_str(&mut s, "namespace", namespace);
    push_kv_str(&mut s, "query_b64", &encode_vector_b64(query));
    s.push_str(&format!("k = {}\n", k));
    if let Some(c) = gate_claim_id {
        push_kv_str(&mut s, "gate_claim_id", c);
    }
    let bytes = call_sql_with_one_input(vector_search_v1, s.as_bytes())?;
    let resp: VectorSearchResponse = parse_toml(&bytes)?;
    Ok(resp.hits)
}

/// Remove the vector under `ref_id`. Returns `true` if the key existed.
pub fn vector_delete(namespace: &str, ref_id: u64) -> Result<bool, AbiError> {
    let mut s = String::new();
    push_kv_str(&mut s, "namespace", namespace);
    s.push_str(&format!("ref_id = {}\n", ref_id));
    let bytes = call_sql_with_one_input(vector_delete_v1, s.as_bytes())?;
    let resp: VectorDeleteResponse = parse_toml(&bytes)?;
    Ok(resp.removed)
}

// =============================================================================
// Policy / Gate API wrappers (F1c P4) — DPIA / FRIA claim verification
// =============================================================================

/// One signer entry on a verified claim. `role` matches the manifest gate
/// requirement (`dpo`, `supervisor`, ...) and `user` is the admin identity
/// recorded when the claim was issued.
#[derive(Debug, Clone, Deserialize)]
pub struct GateSigner {
    pub role: String,
    pub user: String,
}

/// Result of `gate_check`. `valid=true` means the claim satisfied every
/// requirement of the named gate (validity window, type, scope, signer
/// roles). When `valid=false`, `reason` carries a human-readable denial
/// message from the policy engine.
#[derive(Debug, Clone, Deserialize)]
pub struct GateCheckResult {
    pub valid: bool,
    pub claim_id: String,
    #[serde(default)]
    pub claim_type: String,
    #[serde(default)]
    pub valid_until: String,
    #[serde(default)]
    pub signers: Vec<GateSigner>,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Verify a policy claim against the gate id declared in the addon manifest.
/// Requires `policy.read` permission. A `valid=false` result is NOT an
/// AbiError — it is a soft decision the addon can react to. Hard errors
/// (no permission, gate id unknown, malformed payload) return AbiError.
pub fn gate_check(gate_id: &str, claim_id: &str) -> Result<GateCheckResult, AbiError> {
    gate_check_scoped(gate_id, claim_id, None)
}

/// Like `gate_check` but with an optional resource scope hint — required
/// when the claim was issued for a specific resource (vector namespace,
/// alias id) and the gate enforces namespace-level narrowing.
pub fn gate_check_scoped(
    gate_id: &str,
    claim_id: &str,
    resource_scope: Option<&str>,
) -> Result<GateCheckResult, AbiError> {
    let mut s = String::new();
    push_kv_str(&mut s, "gate_id", gate_id);
    push_kv_str(&mut s, "claim_id", claim_id);
    if let Some(rs) = resource_scope {
        push_kv_str(&mut s, "resource_scope", rs);
    }
    let bytes = call_sql_with_one_input(gate_check_v1, s.as_bytes())?;
    parse_toml(&bytes)
}

// =============================================================================
// Wysokopoziomowe wrappery — Flow API (F1c P5)
// =============================================================================

/// Status row returned by `flow_invoke` / `flow_status`. Mirrors the
/// `InvocationStatus` shape produced by the core scheduler.
#[derive(Debug, Clone, Deserialize)]
pub struct FlowInvocation {
    pub invocation_id: String,
    pub status: String,
    pub started_at: String,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub operators_completed: i64,
    #[serde(default)]
    pub operators_total: i64,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub result_toml: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct FlowCancelRaw {
    cancelled: bool,
}

/// Invoke a manifest-declared flow. `wait_ms == 0` returns immediately with
/// `status = "running"`; `wait_ms > 0` awaits the DAG up to 30 s (silently
/// clamped by the host). `input` is forwarded verbatim to every operator
/// as `OperatorContext.input_toml`.
pub fn flow_invoke(
    flow_id: &str,
    input_toml: &str,
    wait_ms: u32,
) -> Result<FlowInvocation, AbiError> {
    let mut s = String::new();
    push_kv_str(&mut s, "flow_id", flow_id);
    s.push_str(&format!("wait_ms = {}\n", wait_ms));
    // `input` is rendered as an inline TOML expression so callers can pass
    // a nested table — push it last to avoid leaking the table header into
    // subsequent scalar keys.
    s.push_str("input = ");
    s.push_str(input_toml.trim());
    s.push('\n');
    let bytes = call_sql_with_one_input(flow_invoke_v1, s.as_bytes())?;
    parse_toml(&bytes)
}

/// Read the authoritative DB row for an invocation. The host filters by
/// the calling addon id, so an invocation owned by a different addon is
/// reported as `AbiError::NotFound`.
pub fn flow_status(invocation_id: &str) -> Result<FlowInvocation, AbiError> {
    let mut s = String::new();
    push_kv_str(&mut s, "invocation_id", invocation_id);
    let bytes = call_sql_with_one_input(flow_status_v1, s.as_bytes())?;
    parse_toml(&bytes)
}

/// Request cooperative cancellation of a running invocation. Idempotent:
/// cancelling a finished invocation returns `cancelled = true` as long as
/// the invocation belongs to the calling addon.
pub fn flow_cancel(invocation_id: &str) -> Result<bool, AbiError> {
    let mut s = String::new();
    push_kv_str(&mut s, "invocation_id", invocation_id);
    let bytes = call_sql_with_one_input(flow_cancel_v1, s.as_bytes())?;
    let raw: FlowCancelRaw = parse_toml(&bytes)?;
    Ok(raw.cancelled)
}

// =============================================================================
// Services catalog wrappers (F2 P2.a) — read-only mesh service inspection
// =============================================================================

/// One row from `service_list`. `service_id` is the cross-node stable id
/// `<node>:<local_id>`; `service_local_id` carries the numeric id that the
/// router APIs expect when addressing the service directly.
#[derive(Debug, Clone, Deserialize)]
pub struct ServiceInfo {
    pub service_id: String,
    pub service_local_id: i64,
    pub display_name: String,
    pub kind: String,
    pub status: String,
    pub node_id: String,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ServiceListResponse {
    #[serde(default)]
    services: Vec<ServiceInfo>,
}

/// Filtered view of every service visible in the mesh (local node + every
/// reachable peer). Pass `None` for any filter to include everything.
/// Requires the `service.read` permission.
pub fn service_list(
    kind: Option<&str>,
    status: Option<&str>,
    node_id: Option<&str>,
) -> Result<Vec<ServiceInfo>, AbiError> {
    let mut s = String::new();
    if let Some(k) = kind {
        push_kv_str(&mut s, "kind", k);
    }
    if let Some(st) = status {
        push_kv_str(&mut s, "status", st);
    }
    if let Some(n) = node_id {
        push_kv_str(&mut s, "node_id", n);
    }
    let bytes = call_sql_with_one_input(service_list_v1, s.as_bytes())?;
    let resp: ServiceListResponse = parse_toml(&bytes)?;
    Ok(resp.services)
}

/// Live hardware snapshot for one node. Local node only today — passing an
/// unknown / remote `node_id` returns `AbiError::NotFound`. Requires the
/// `service.read` permission.
#[derive(Debug, Clone, Deserialize)]
pub struct NodeResources {
    pub node_id: String,
    pub cpu_cores: u32,
    pub cpu_load_pct: f64,
    pub ram_total_mb: u64,
    pub ram_used_mb: u64,
    #[serde(default)]
    pub gpu: Option<NodeGpu>,
    pub gpu_count: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NodeGpu {
    pub name: String,
    pub vram_total_mb: u64,
    pub vram_used_mb: u64,
    pub utilization_pct: f64,
}

pub fn node_resources_get(node_id: &str) -> Result<NodeResources, AbiError> {
    let mut s = String::new();
    push_kv_str(&mut s, "node_id", node_id);
    let bytes = call_sql_with_one_input(node_resources_get_v1, s.as_bytes())?;
    parse_toml(&bytes)
}

// =============================================================================
// Alokator pamieci WASM — eksportowany dla hosta
// =============================================================================

/// Alokuje bufor w pamieci guest WASM.
/// Eksportowane jako funkcja WASM "alloc" dla hosta.
#[no_mangle]
pub extern "C" fn alloc(size: i32) -> i32 {
    let layout = std::alloc::Layout::from_size_align(size as usize, 1)
        .expect("Niepoprawny layout alokacji");
    let ptr = unsafe { std::alloc::alloc(layout) };
    if ptr.is_null() {
        return 0;
    }
    ptr as i32
}

/// Zwalnia bufor w pamieci guest WASM.
/// Eksportowane jako funkcja WASM "dealloc" dla hosta.
#[no_mangle]
pub extern "C" fn dealloc(ptr: i32, size: i32) {
    if ptr == 0 || size <= 0 {
        return;
    }
    let layout = std::alloc::Layout::from_size_align(size as usize, 1)
        .expect("Niepoprawny layout dealokacji");
    unsafe {
        std::alloc::dealloc(ptr as *mut u8, layout);
    }
}
