// =============================================================================
// Plik: addon-sdk/sdk/src/lib.rs
// Opis: TentaFlow Addon SDK — bindingi do host functions, helpery pamieciowe,
//       wysokopoziomowe wrappery do komunikacji z Core (LLM, storage, HTTP,
//       eventy, UI, sekrety, logi, rejestracja narzedzi).
// =============================================================================

use base64::Engine as _;
use serde::{Deserialize, Serialize};

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
    /// in F1a). Payload format is TOML for all inputs/outputs. Requires
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
pub fn render_panel(panel_id: &str, content: serde_json::Value) -> Result<(), String> {
    let ui_json = serde_json::to_string(&content)
        .map_err(|e| format!("Blad serializacji panelu UI: {}", e))?;
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
    pub use crate::{
        read_string, write_string,
        generate,
        store_get, store_set,
        http_get, http_post, http_send, HttpRequest, HttpResponse,
        publish_event, subscribe_event, Event,
        render_panel, notify, notify_with_level,
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
// Wrapper-y woke host functions camera_*_v1. Payload to TOML; bledy mapowane na
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
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CameraAddResult {
    pub camera_id: String,
    pub status: String,
}

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Deserialize)]
struct CameraListResponse {
    #[serde(default)]
    camera: Vec<CameraInfo>,
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

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Deserialize)]
struct SnapshotRaw {
    camera_id: String,
    width: u32,
    height: u32,
    pixel_format: String,
    timestamp_unix_ms: u64,
    data_b64: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CameraTestResult {
    pub ok: bool,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CameraRemoveOutRaw {
    #[allow(dead_code)]
    removed: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct CameraDiscoverRaw {
    #[serde(default)]
    discovered: Vec<CameraInfo>,
}

#[derive(Debug, Clone, Deserialize)]
struct CameraCredentialsRotateRaw {
    rotated: bool,
    reason: String,
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

fn camera_add_payload(spec: &CameraAddSpec) -> String {
    let mut s = String::new();
    s.push_str(&format!("display_name = {}\n", toml::Value::String(spec.display_name.clone())));
    s.push_str(&format!("vendor = {}\n", toml::Value::String(spec.vendor.clone())));
    s.push_str(&format!("url = {}\n", toml::Value::String(spec.url.clone())));
    s.push_str(&format!("target_fps = {}\n", spec.target_fps));
    if let Some((w, h)) = spec.resolution {
        s.push_str(&format!("resolution_width = {}\n", w));
        s.push_str(&format!("resolution_height = {}\n", h));
    }
    s.push_str(&format!("retention_class = {}\n", toml::Value::String(spec.retention_class.clone())));
    s.push_str(&format!("profile = {}\n", toml::Value::String(spec.profile.clone())));
    s
}

/// Rejestruje nowa kamere w supervisor + DB. F1a vendor whitelist: `fake_file`.
pub fn camera_add(spec: &CameraAddSpec) -> Result<CameraAddResult, AbiError> {
    let payload = camera_add_payload(spec);
    let bytes = call_sql_with_one_input(camera_add_v1, payload.as_bytes())?;
    parse_toml(&bytes)
}

/// Zwraca wszystkie kamery nalezace do wywolujacego addona. Kazdy wpis zawiera
/// runtime metryki (`fps_actual`, `status`) z supervisora gdy session jest
/// aktywna; w przeciwnym razie wartosci z DB (po restarcie hosta).
pub fn camera_list() -> Result<Vec<CameraInfo>, AbiError> {
    let bytes = call_host_no_input(camera_list_v1)?;
    let resp: CameraListResponse = parse_toml(&bytes)?;
    Ok(resp.camera)
}

/// Pobiera pojedynczy `CameraInfo`. Zwraca `NotFound` gdy kamera nie istnieje
/// lub nalezy do innego addona (kanalu bocznego nie ma — nie da sie wnioskowac
/// o istnieniu cudzych camera_id).
pub fn camera_get(camera_id: &str) -> Result<CameraInfo, AbiError> {
    let payload = format!("camera_id = {}\n", toml::Value::String(camera_id.to_string()));
    let bytes = call_sql_with_one_input(camera_get_v1, payload.as_bytes())?;
    parse_toml(&bytes)
}

/// Patch on-the-fly. Vendor + URL sa niezmienne — change them by remove + add.
pub fn camera_update(spec: &CameraUpdateSpec) -> Result<CameraInfo, AbiError> {
    let mut s = String::new();
    s.push_str(&format!("camera_id = {}\n", toml::Value::String(spec.camera_id.clone())));
    if let Some(v) = &spec.display_name {
        s.push_str(&format!("display_name = {}\n", toml::Value::String(v.clone())));
    }
    if let Some(v) = spec.target_fps {
        s.push_str(&format!("target_fps = {}\n", v));
    }
    if let Some(v) = spec.resolution_width {
        s.push_str(&format!("resolution_width = {}\n", v));
    }
    if let Some(v) = spec.resolution_height {
        s.push_str(&format!("resolution_height = {}\n", v));
    }
    if let Some(v) = &spec.retention_class {
        s.push_str(&format!("retention_class = {}\n", toml::Value::String(v.clone())));
    }
    if let Some(v) = &spec.profile {
        s.push_str(&format!("profile = {}\n", toml::Value::String(v.clone())));
    }
    let bytes = call_sql_with_one_input(camera_update_v1, s.as_bytes())?;
    parse_toml(&bytes)
}

/// Soft-delete (stamps `removed_at`). Idempotent w sensie ABI: druga proba na
/// tym samym camera_id zwraca `NotFound`.
pub fn camera_remove(camera_id: &str) -> Result<(), AbiError> {
    let payload = format!("camera_id = {}\n", toml::Value::String(camera_id.to_string()));
    let bytes = call_sql_with_one_input(camera_remove_v1, payload.as_bytes())?;
    let _: CameraRemoveOutRaw = parse_toml(&bytes)?;
    Ok(())
}

/// Snapshot ostatniej ramki — RGB24 zdekodowany z base64. Maks ~5.5MB raw
/// (1280x720 mieci sie w PayloadKind::ServiceCall; 1920x1080 przekroczy limit
/// i zwroci `PayloadTooLarge`).
///
/// Requires TentaFlow core built with `--features camera`. Without it
/// addon instantiation fails at module-link time with "missing import".
pub fn camera_snapshot(camera_id: &str) -> Result<SnapshotInfo, AbiError> {
    let payload = format!("camera_id = {}\n", toml::Value::String(camera_id.to_string()));
    let bytes = call_sql_with_one_input_capped(
        camera_snapshot_v1,
        payload.as_bytes(),
        MAX_OUT_CAP_SNAPSHOT,
    )?;
    let raw: SnapshotRaw = parse_toml(&bytes)?;
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
    let payload = format!("camera_id = {}\n", toml::Value::String(camera_id.to_string()));
    let bytes = call_sql_with_one_input(camera_health_v1, payload.as_bytes())?;
    parse_toml(&bytes)
}

/// F1a stub — zawsze zwraca pusty vector. F1b doda RTSP/ONVIF scan.
pub fn camera_discover() -> Result<Vec<CameraInfo>, AbiError> {
    let bytes = call_host_no_input(camera_discover_v1)?;
    let resp: CameraDiscoverRaw = parse_toml(&bytes)?;
    Ok(resp.discovered)
}

/// Lightweight probe — sprawdza czy URL kamery jest osiagalny dla danego
/// vendora. Dla `fake_file` sprawdza ze plik istnieje, jest plikiem regularnym
/// i nie zawiera symlinkow w sciezce.
pub fn camera_test_connection(vendor: &str, url: &str) -> Result<CameraTestResult, AbiError> {
    let payload = format!(
        "vendor = {}\nurl = {}\n",
        toml::Value::String(vendor.to_string()),
        toml::Value::String(url.to_string())
    );
    let bytes = call_sql_with_one_input(camera_test_connection_v1, payload.as_bytes())?;
    parse_toml(&bytes)
}

/// F1a stub dla credentialow vendorow wymagajacych auth (RTSP/ONVIF w F1b).
/// `fake_file` nie ma credentiali — zwraca `(false, "f1a_noop_...")`.
pub fn camera_credentials_rotate(
    camera_id: &str,
    new_credentials_b64: Option<&str>,
) -> Result<(bool, String), AbiError> {
    let mut s = format!("camera_id = {}\n", toml::Value::String(camera_id.to_string()));
    if let Some(c) = new_credentials_b64 {
        s.push_str(&format!("new_credentials_b64 = {}\n", toml::Value::String(c.to_string())));
    }
    let bytes = call_sql_with_one_input(camera_credentials_rotate_v1, s.as_bytes())?;
    let raw: CameraCredentialsRotateRaw = parse_toml(&bytes)?;
    Ok((raw.rotated, raw.reason))
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
#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StreamNextRaw {
    Frame {
        frame_ref: String,
        camera_id: String,
        width: u32,
        height: u32,
        pixel_format: String,
        timestamp_unix_ms: u64,
    },
    Drop {
        count: u64,
    },
    CameraOffline {
        reason: String,
    },
    StreamClosed,
    Timeout,
}

#[derive(Debug, Clone, Deserialize)]
struct StreamSubscribeOut {
    stream_id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct StreamCloseOut {
    #[allow(dead_code)]
    closed: bool,
}

/// Subscribe to a camera's frame bus. F1a target format: `camera:<camera_id>`.
/// Ownership is enforced — addons cannot subscribe to cameras owned by other
/// addons (returns `NotFound`).
pub fn stream_subscribe(target: &str, max_fps: Option<u32>) -> Result<String, AbiError> {
    let mut s = format!("target = {}\n", toml::Value::String(target.to_string()));
    if let Some(fps) = max_fps {
        s.push_str(&format!("[filter]\nmax_fps = {}\nskip_frames = 0\n", fps));
    }
    let bytes = call_sql_with_one_input_capped(stream_subscribe_v1, s.as_bytes(), MAX_OUT_CAP_STREAM)?;
    let out: StreamSubscribeOut = parse_toml(&bytes)?;
    Ok(out.stream_id)
}

/// Bounded-await poll for the next stream message. `timeout_ms` is clamped to
/// 5000 ms by the host.
pub fn stream_next(stream_id: &str, timeout_ms: u64) -> Result<StreamNextMessage, AbiError> {
    let payload = format!(
        "stream_id = {}\ntimeout_ms = {}\n",
        toml::Value::String(stream_id.to_string()),
        timeout_ms,
    );
    let bytes = call_sql_with_one_input_capped(stream_next_v1, payload.as_bytes(), MAX_OUT_CAP_STREAM)?;
    let raw: StreamNextRaw = parse_toml(&bytes)?;
    Ok(match raw {
        StreamNextRaw::Frame {
            frame_ref,
            camera_id,
            width,
            height,
            pixel_format,
            timestamp_unix_ms,
        } => StreamNextMessage::Frame(StreamFrameMeta {
            frame_ref,
            camera_id,
            width,
            height,
            pixel_format,
            timestamp_unix_ms,
        }),
        StreamNextRaw::Drop { count } => StreamNextMessage::Drop { count },
        StreamNextRaw::CameraOffline { reason } => StreamNextMessage::CameraOffline { reason },
        StreamNextRaw::StreamClosed => StreamNextMessage::StreamClosed,
        StreamNextRaw::Timeout => StreamNextMessage::Timeout,
    })
}

/// Drop the subscription. Subsequent `stream_next` calls for the same id
/// return `StreamNotFound`.
pub fn stream_close(stream_id: &str) -> Result<(), AbiError> {
    let payload = format!("stream_id = {}\n", toml::Value::String(stream_id.to_string()));
    let bytes = call_sql_with_one_input_capped(stream_close_v1, payload.as_bytes(), MAX_OUT_CAP_STREAM)?;
    let _: StreamCloseOut = parse_toml(&bytes)?;
    Ok(())
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

#[derive(Debug, Clone, Deserialize)]
struct RecordingGetStreamRaw {
    data_b64: String,
    #[allow(dead_code)]
    file_size_bytes: u64,
    #[allow(dead_code)]
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

/// Capture `duration_secs` of `source_url` (file://) into an MP4 segment.
/// Requires TentaFlow core built with `--features camera`.
pub fn recording_save_segment(
    camera_id: &str,
    source_url: &str,
    duration_secs: u32,
    retention_class: Option<&str>,
) -> Result<SavedRecordingInfo, AbiError> {
    let mut s = String::new();
    push_kv_str(&mut s, "camera_id", camera_id);
    push_kv_str(&mut s, "source_url", source_url);
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

/// Fetch the raw bytes (PNG or MP4) of a stored recording inline. Hard-capped
/// at 8 MiB by the host — larger artifacts must be fetched via the signed URL
/// + HTTP handler.
/// Requires TentaFlow core built with `--features camera`.
pub fn recording_get_stream(recording_ref: &str) -> Result<Vec<u8>, AbiError> {
    let mut s = String::new();
    push_kv_str(&mut s, "recording_ref", recording_ref);
    let bytes = call_sql_with_one_input_capped(
        recording_get_stream_v1,
        s.as_bytes(),
        MAX_OUT_CAP_SNAPSHOT,
    )?;
    let raw: RecordingGetStreamRaw = parse_toml(&bytes)?;
    base64::engine::general_purpose::STANDARD
        .decode(raw.data_b64.as_bytes())
        .map_err(|_| AbiError::Operation)
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
