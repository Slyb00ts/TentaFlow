// =============================================================================
// Plik: addon-sdk/sdk/src/lib.rs
// Opis: TentaFlow Addon SDK — bindingi do host functions, helpery pamieciowe,
//       wysokopoziomowe wrappery do komunikacji z Core (LLM, storage, HTTP,
//       eventy, UI, sekrety, logi, rejestracja narzedzi).
// =============================================================================

use serde::{Deserialize, Serialize};

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
        log,
    };
    pub use serde::{Deserialize, Serialize};
    pub use serde_json::{self, json, Value};
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
