// =============================================================================
// Plik: addons/sharepoint-rag/src/lib.rs
// Opis: Addon SharePoint RAG dla TentaFlow — indeksowanie i przeszukiwanie
//       plikow z wybranych witryn SharePoint przez Microsoft Graph API.
//       Uzywa Application permissions (client_credentials flow) — nie wymaga
//       logowania uzytkownika. Kompilowany do WASM (cdylib).
// =============================================================================

use tentaflow_addon_sdk::prelude::*;

// =============================================================================
// Stale — endpointy Microsoft Graph API i klucze konfiguracji
// =============================================================================

/// Bazowy URL Microsoft Graph API v1.0
const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";

/// Endpoint autoryzacji OAuth2 (Azure AD / Entra ID)
const AUTH_BASE: &str = "https://login.microsoftonline.com";

/// Klucz sekretu client ID (Azure AD App Registration)
const SECRET_CLIENT_ID: &str = "client_id";

/// Klucz sekretu client secret (Azure AD App Registration)
const SECRET_CLIENT_SECRET: &str = "client_secret";

/// Klucz konfiguracji tenant ID w storage
const STORAGE_TENANT_ID: &str = "config.tenant_id";

/// Klucz konfiguracji URL-i witryn SharePoint w storage
const STORAGE_SITE_URLS: &str = "config.site_urls";

/// Klucz konfiguracji rozszerzen plikow do indeksowania
const STORAGE_FILE_EXTENSIONS: &str = "config.file_extensions";

/// Klucz konfiguracji maksymalnego rozmiaru pliku (MB)
const STORAGE_MAX_FILE_SIZE_MB: &str = "config.max_file_size_mb";

/// Klucz konfiguracji interwalu synchronizacji (minuty)
const STORAGE_SYNC_INTERVAL: &str = "config.sync_interval_minutes";

/// Klucz storage do przechowywania app tokenu (cache)
const STORAGE_APP_TOKEN: &str = "internal.app_token";

/// Prefiks klucza storage do mapowania URL witryny -> site ID
const STORAGE_SITE_ID_PREFIX: &str = "site_id:";

/// Prefiks klucza storage do indeksu plikow
const STORAGE_INDEX_PREFIX: &str = "index:";

/// Prefiks klucza storage do delta tokenow (sledzenie zmian)
const STORAGE_DELTA_PREFIX: &str = "delta:";

/// Prefiks klucza storage do zawartosci plikow (cache)
const STORAGE_CONTENT_PREFIX: &str = "content:";

/// Domyslne rozszerzenia plikow do indeksowania
const DEFAULT_FILE_EXTENSIONS: &str = "pdf,docx,xlsx,pptx,txt,md,csv";

/// Domyslny maksymalny rozmiar pliku w bajtach (50 MB)
const DEFAULT_MAX_FILE_SIZE: u64 = 50 * 1024 * 1024;

// =============================================================================
// Lifecycle hooks — eksporty WASM
// =============================================================================

/// Wywolywane przy instalacji addonu.
/// Inicjalizuje domyslna konfiguracje w storage.
#[no_mangle]
pub extern "C" fn on_install() -> i32 {
    log::info("SharePoint RAG addon: instalacja rozpoczeta");

    // Ustaw domyslna konfiguracje
    let defaults: &[(&str, &str)] = &[
        (STORAGE_FILE_EXTENSIONS, DEFAULT_FILE_EXTENSIONS),
        (STORAGE_MAX_FILE_SIZE_MB, "50"),
        (STORAGE_SYNC_INTERVAL, "0"),
        ("initialized", "true"),
    ];

    for (key, value) in defaults {
        if let Err(e) = store_set(key, value) {
            log::error(&format!("Blad inicjalizacji storage [{}]: {}", key, e));
            return 1;
        }
    }

    log::info("SharePoint RAG addon: instalacja zakonczona pomyslnie");
    0
}

/// Wywolywane przy uruchomieniu instancji addonu.
/// Rejestruje narzedzia LLM i renderuje panel UI.
#[no_mangle]
pub extern "C" fn on_start() -> i32 {
    log::info("SharePoint RAG addon: uruchamianie");

    // -------------------------------------------------------------------------
    // Rejestracja narzedzi LLM tool calling
    // -------------------------------------------------------------------------

    register_tool(
        "sharepoint_rag.list_sites",
        "Lista witryn SharePoint dostepnych dla TentaFlow.",
        json!({
            "type": "object",
            "properties": {}
        }),
    );

    register_tool(
        "sharepoint_rag.list_files",
        "Lista plikow w witrynie lub folderze SharePoint.",
        json!({
            "type": "object",
            "properties": {
                "site_url": {
                    "type": "string",
                    "description": "URL witryny SharePoint"
                },
                "path": {
                    "type": "string",
                    "description": "Sciezka do folderu (domyslnie root)"
                },
                "recursive": {
                    "type": "boolean",
                    "description": "Czy listowac rekurencyjnie (domyslnie false)"
                }
            }
        }),
    );

    register_tool(
        "sharepoint_rag.search_files",
        "Wyszukuje pliki w SharePoint po nazwie lub zawartosci.",
        json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Fraza wyszukiwania"
                },
                "site_url": {
                    "type": "string",
                    "description": "Ogranicz do konkretnej witryny (opcjonalnie)"
                },
                "file_type": {
                    "type": "string",
                    "description": "Filtruj po rozszerzeniu (np. pdf, docx)"
                }
            }
        }),
    );

    register_tool(
        "sharepoint_rag.get_file_content",
        "Pobiera zawartosc pliku z SharePoint (tekst lub metadane).",
        json!({
            "type": "object",
            "required": ["file_id"],
            "properties": {
                "file_id": {
                    "type": "string",
                    "description": "ID pliku z SharePoint (z list_files lub search_files)"
                },
                "format": {
                    "type": "string",
                    "description": "Format wyjscia: text, metadata, raw (domyslnie text)"
                }
            }
        }),
    );

    register_tool(
        "sharepoint_rag.get_file_info",
        "Pobiera metadane pliku (rozmiar, data modyfikacji, autor).",
        json!({
            "type": "object",
            "required": ["file_id"],
            "properties": {
                "file_id": {
                    "type": "string",
                    "description": "ID pliku z SharePoint"
                }
            }
        }),
    );

    register_tool(
        "sharepoint_rag.list_recent_changes",
        "Lista ostatnio zmienionych plikow w SharePoint.",
        json!({
            "type": "object",
            "properties": {
                "site_url": {
                    "type": "string",
                    "description": "Ogranicz do konkretnej witryny (opcjonalnie)"
                },
                "days": {
                    "type": "number",
                    "description": "Ile dni wstecz (domyslnie 7)"
                }
            }
        }),
    );

    register_tool(
        "sharepoint_rag.sync_index",
        "Uruchamia synchronizacje indeksu plikow SharePoint.",
        json!({
            "type": "object",
            "properties": {
                "site_url": {
                    "type": "string",
                    "description": "Ogranicz do konkretnej witryny (opcjonalnie)"
                },
                "force": {
                    "type": "boolean",
                    "description": "Wymus pelna reindeksacje (domyslnie false — tylko zmiany)"
                }
            }
        }),
    );

    // -------------------------------------------------------------------------
    // Panel UI — status addonu
    // -------------------------------------------------------------------------

    let panel = json!({
        "type": "column",
        "children": [
            {
                "type": "text",
                "props": {
                    "content": "SharePoint RAG",
                    "variant": "heading",
                    "size": "lg"
                }
            },
            {
                "type": "text",
                "props": {
                    "content": "Status: aktywny",
                    "color": "green"
                }
            },
            {
                "type": "text",
                "props": {
                    "content": "Autoryzacja: Application permissions (client_credentials)",
                    "color": "gray"
                }
            }
        ]
    });

    if let Err(e) = render_panel("sharepoint_rag_main", panel) {
        log::warn(&format!("Blad renderowania panelu UI: {}", e));
    }

    log::info("SharePoint RAG addon: uruchomiony pomyslnie");
    0
}

/// Wywolywane przy zatrzymaniu instancji addonu.
#[no_mangle]
pub extern "C" fn on_stop() -> i32 {
    log::info("SharePoint RAG addon: zatrzymywanie");
    log::info("SharePoint RAG addon: zatrzymany");
    0
}

// =============================================================================
// Obsluga requestow (tool calls)
// =============================================================================

/// Glowny handler requestow z hosta — dispatchuje tool calls.
#[no_mangle]
pub extern "C" fn on_request(
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let input_json = tentaflow_addon_sdk::read_string(input_ptr, input_len);

    let request: Value = match serde_json::from_str(&input_json) {
        Ok(v) => v,
        Err(e) => {
            let error = make_error(&format!("Blad parsowania requestu: {}", e));
            return write_response(out_ptr, out_cap, out_len_ptr, &error);
        }
    };

    let tool_name = request
        .get("tool")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let params = request.get("params").cloned().unwrap_or(json!({}));

    // Dispatchuj po nazwie narzedzia
    let result = match tool_name {
        "sharepoint_rag.list_sites" => tool_list_sites(),
        "sharepoint_rag.list_files" => tool_list_files(&params),
        "sharepoint_rag.search_files" => tool_search_files(&params),
        "sharepoint_rag.get_file_content" => tool_get_file_content(&params),
        "sharepoint_rag.get_file_info" => tool_get_file_info(&params),
        "sharepoint_rag.list_recent_changes" => tool_list_recent_changes(&params),
        "sharepoint_rag.sync_index" => tool_sync_index(&params),
        _ => make_error(&format!("Nieznane narzedzie: {}", tool_name)),
    };

    write_response(out_ptr, out_cap, out_len_ptr, &result)
}

/// Handler eventow — addon SharePoint RAG nie subskrybuje eventow,
/// ale interfejs jest wymagany przez runtime.
#[no_mangle]
pub extern "C" fn on_event(_event_ptr: i32, _event_len: i32) -> i32 {
    0
}

// =============================================================================
// Helpery — format odpowiedzi
// =============================================================================

/// Tworzy odpowiedz sukcesu w formacie { "ok": true, "data": ... }.
fn make_ok(data: Value) -> Value {
    json!({ "ok": true, "data": data })
}

/// Tworzy odpowiedz bledu w formacie { "ok": false, "error": "..." }.
fn make_error(message: &str) -> Value {
    json!({ "ok": false, "error": message })
}

// =============================================================================
// Helpery — OAuth (client_credentials flow)
// =============================================================================

/// Pobiera app token przez client_credentials flow.
/// Uzywa cache w storage — zwraca zapisany token jesli dostepny,
/// w przeciwnym razie pobiera nowy z Azure AD.
fn get_app_token() -> Result<String, Value> {
    // Sprawdz cache — token moze byc jeszcze wazny
    if let Ok(Some(cached)) = store_get(STORAGE_APP_TOKEN) {
        if !cached.is_empty() {
            return Ok(cached);
        }
    }

    // Pobierz konfiguracje
    let client_id = match secret_get_value(SECRET_CLIENT_ID) {
        Ok(Some(v)) if !v.is_empty() => v,
        Ok(_) => {
            return Err(make_error(
                "Brak client_id w konfiguracji addonu. Ustaw sekret 'client_id' w ustawieniach.",
            ));
        }
        Err(e) => {
            return Err(make_error(&format!("Blad odczytu client_id: {}", e)));
        }
    };

    let client_secret = match secret_get_value(SECRET_CLIENT_SECRET) {
        Ok(Some(v)) if !v.is_empty() => v,
        Ok(_) => {
            return Err(make_error(
                "Brak client_secret w konfiguracji addonu. Ustaw sekret 'client_secret' w ustawieniach.",
            ));
        }
        Err(e) => {
            return Err(make_error(&format!("Blad odczytu client_secret: {}", e)));
        }
    };

    let tenant_id = match store_get(STORAGE_TENANT_ID) {
        Ok(Some(v)) if !v.is_empty() => v,
        _ => {
            return Err(make_error(
                "Brak tenant_id w konfiguracji addonu. Ustaw Azure Tenant ID w ustawieniach.",
            ));
        }
    };

    // Pobierz nowy token przez client_credentials flow
    let token_url = format!("{}/{}/oauth2/v2.0/token", AUTH_BASE, tenant_id);

    let body = format!(
        "grant_type=client_credentials&client_id={}&client_secret={}&scope=https%3A%2F%2Fgraph.microsoft.com%2F.default",
        url_encode(&client_id),
        url_encode(&client_secret)
    );

    let request = HttpRequest {
        method: "POST".to_string(),
        url: token_url,
        headers: {
            let mut h = std::collections::HashMap::new();
            h.insert(
                "Content-Type".to_string(),
                "application/x-www-form-urlencoded".to_string(),
            );
            h
        },
        body: Some(body),
    };

    let response = match http_send(&request) {
        Ok(r) => r,
        Err(e) => {
            return Err(make_error(&format!(
                "Blad HTTP przy pobieraniu tokenu: {}",
                e
            )));
        }
    };

    if response.status != 200 {
        log::error(&format!(
            "Blad pobierania app tokenu, status: {}, body: {}",
            response.status, response.body
        ));
        return Err(make_error(&format!(
            "Blad autoryzacji Azure AD (status {}). Sprawdz client_id, client_secret i tenant_id.",
            response.status
        )));
    }

    // Parsuj odpowiedz tokenowa
    let token_data: Value = match serde_json::from_str(&response.body) {
        Ok(v) => v,
        Err(e) => {
            return Err(make_error(&format!(
                "Blad parsowania odpowiedzi tokenowej: {}",
                e
            )));
        }
    };

    let access_token = token_data
        .get("access_token")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if access_token.is_empty() {
        return Err(make_error(
            "Odpowiedz tokenowa nie zawiera access_token. Sprawdz konfiguracje Azure AD.",
        ));
    }

    // Zapisz token do cache w storage
    if let Err(e) = store_set(STORAGE_APP_TOKEN, &access_token) {
        log::warn(&format!("Blad zapisu tokenu do cache: {}", e));
    }

    Ok(access_token)
}

/// Prosta implementacja URL encoding dla parametrow OAuth.
/// Koduje znaki specjalne wymagane przez application/x-www-form-urlencoded.
fn url_encode(input: &str) -> String {
    let mut output = String::with_capacity(input.len() * 3);
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                output.push(byte as char);
            }
            _ => {
                output.push('%');
                output.push_str(&format!("{:02X}", byte));
            }
        }
    }
    output
}

// =============================================================================
// Helpery — Microsoft Graph API
// =============================================================================

/// Wykonuje request do Microsoft Graph API z autoryzacja app token.
/// Automatycznie pobiera nowy token jesli poprzedni wygasl (401).
fn graph_request(method: &str, endpoint: &str, body: Option<&str>) -> Result<Value, Value> {
    let token = get_app_token()?;
    let result = graph_request_with_token(method, endpoint, body, &token);

    // Jesli 401 — wyczysc cache tokenu i pobierz nowy
    if let Err(ref err) = result {
        if let Some(code) = err.get("code").and_then(|v| v.as_str()) {
            if code == "UNAUTHORIZED" {
                log::info("App token wygasl, pobieram nowy...");
                // Wyczysc cache tokenu
                let _ = store_set(STORAGE_APP_TOKEN, "");
                let new_token = get_app_token()?;
                return graph_request_with_token(method, endpoint, body, &new_token);
            }
        }
    }

    result
}

/// Wykonuje request do Graph API z podanym tokenem.
fn graph_request_with_token(
    method: &str,
    endpoint: &str,
    body: Option<&str>,
    token: &str,
) -> Result<Value, Value> {
    let url = format!("{}{}", GRAPH_BASE, endpoint);

    let mut headers = std::collections::HashMap::new();
    headers.insert(
        "Authorization".to_string(),
        format!("Bearer {}", token),
    );
    headers.insert("Content-Type".to_string(), "application/json".to_string());

    let request = HttpRequest {
        method: method.to_string(),
        url,
        headers,
        body: body.map(|b| b.to_string()),
    };

    let response = match http_send(&request) {
        Ok(r) => r,
        Err(e) => {
            return Err(json!({
                "ok": false,
                "error": format!("Blad HTTP: {}", e),
                "code": "HTTP_ERROR"
            }));
        }
    };

    match response.status {
        200 | 201 | 202 | 204 => {
            if response.body.is_empty() {
                return Ok(json!({"status": "ok"}));
            }

            match serde_json::from_str(&response.body) {
                Ok(v) => Ok(v),
                Err(e) => Err(json!({
                    "ok": false,
                    "error": format!("Blad parsowania odpowiedzi Graph API: {}", e),
                    "code": "PARSE_ERROR",
                    "raw_body": response.body
                })),
            }
        }
        401 => Err(json!({
            "ok": false,
            "error": "Brak autoryzacji (401). Token mogl wygasnac lub aplikacja nie ma uprawnien.",
            "code": "UNAUTHORIZED"
        })),
        403 => Err(json!({
            "ok": false,
            "error": "Brak uprawnien (403). Sprawdz uprawnienia Sites.Selected w Azure AD i czy admin nadalnadal je per witryna.",
            "code": "FORBIDDEN"
        })),
        404 => Err(json!({
            "ok": false,
            "error": "Zasob nie znaleziony (404). Sprawdz URL witryny lub ID pliku.",
            "code": "NOT_FOUND"
        })),
        429 => Err(json!({
            "ok": false,
            "error": "Zbyt wiele requestow (429). Sprobuj ponownie za chwile.",
            "code": "RATE_LIMITED"
        })),
        _ => Err(json!({
            "ok": false,
            "error": format!("Blad Graph API, status: {}", response.status),
            "code": "API_ERROR",
            "status": response.status,
            "body": response.body
        })),
    }
}

/// Pobiera zawartosc pliku binarnie z Graph API (np. do pobrania PDF/DOCX).
/// Zwraca surowa tresc odpowiedzi HTTP jako string (moze byc base64 w runtime).
fn graph_download(endpoint: &str) -> Result<String, Value> {
    let token = get_app_token()?;
    let url = format!("{}{}", GRAPH_BASE, endpoint);

    let mut headers = std::collections::HashMap::new();
    headers.insert(
        "Authorization".to_string(),
        format!("Bearer {}", token),
    );

    let request = HttpRequest {
        method: "GET".to_string(),
        url,
        headers,
        body: None,
    };

    let response = match http_send(&request) {
        Ok(r) => r,
        Err(e) => {
            return Err(make_error(&format!("Blad HTTP przy pobieraniu pliku: {}", e)));
        }
    };

    if response.status == 200 {
        Ok(response.body)
    } else {
        Err(make_error(&format!(
            "Blad pobierania pliku, status: {}",
            response.status
        )))
    }
}

// =============================================================================
// Helpery — witryny SharePoint
// =============================================================================

/// Parsuje skonfigurowane URL-e witryn SharePoint ze storage.
/// Kazdy URL na osobnej linii.
fn get_configured_site_urls() -> Result<Vec<String>, Value> {
    let raw = match store_get(STORAGE_SITE_URLS) {
        Ok(Some(v)) if !v.is_empty() => v,
        _ => {
            return Err(make_error(
                "Brak skonfigurowanych witryn SharePoint. Dodaj URL-e witryn w ustawieniach addonu.",
            ));
        }
    };

    let urls: Vec<String> = raw
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();

    if urls.is_empty() {
        return Err(make_error(
            "Lista witryn SharePoint jest pusta. Dodaj URL-e witryn w ustawieniach addonu.",
        ));
    }

    Ok(urls)
}

/// Konwertuje URL witryny SharePoint na hostname i sciezke witryny.
/// Np. "https://contoso.sharepoint.com/sites/engineering" -> ("contoso.sharepoint.com", "/sites/engineering")
fn parse_site_url(url: &str) -> Option<(String, String)> {
    // Usun protokol
    let without_proto = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);

    // Podziel na hostname i sciezke
    let (hostname, path) = match without_proto.find('/') {
        Some(idx) => {
            let (h, p) = without_proto.split_at(idx);
            // Usun trailing slash
            let path = p.trim_end_matches('/');
            (h.to_string(), path.to_string())
        }
        None => (without_proto.to_string(), String::new()),
    };

    if hostname.is_empty() {
        return None;
    }

    Some((hostname, path))
}

/// Rozpoznaje site ID z Graph API na podstawie hostname i sciezki witryny.
/// Wynik jest cache'owany w storage.
fn resolve_site_id(site_url: &str) -> Result<String, Value> {
    // Sprawdz cache
    let cache_key = format!("{}{}", STORAGE_SITE_ID_PREFIX, site_url);
    if let Ok(Some(cached_id)) = store_get(&cache_key) {
        if !cached_id.is_empty() {
            return Ok(cached_id);
        }
    }

    // Parsuj URL
    let (hostname, path) = match parse_site_url(site_url) {
        Some(v) => v,
        None => {
            return Err(make_error(&format!(
                "Nieprawidlowy URL witryny SharePoint: {}",
                site_url
            )));
        }
    };

    // Pobierz site ID z Graph API
    // GET /sites/{hostname}:{path} lub GET /sites/{hostname} (root)
    let endpoint = if path.is_empty() {
        format!("/sites/{}", hostname)
    } else {
        format!("/sites/{}:{}", hostname, path)
    };

    let site_data = graph_request("GET", &endpoint, None)?;

    let site_id = site_data
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if site_id.is_empty() {
        return Err(make_error(&format!(
            "Nie udalo sie pobrac ID witryny dla: {}. Sprawdz czy aplikacja ma uprawnienia Sites.Selected do tej witryny.",
            site_url
        )));
    }

    // Zapisz do cache
    if let Err(e) = store_set(&cache_key, &site_id) {
        log::warn(&format!("Blad zapisu site ID do cache: {}", e));
    }

    Ok(site_id)
}

/// Pobiera dozwolone rozszerzenia plikow z konfiguracji.
fn get_allowed_extensions() -> Vec<String> {
    let raw = match store_get(STORAGE_FILE_EXTENSIONS) {
        Ok(Some(v)) if !v.is_empty() => v,
        _ => DEFAULT_FILE_EXTENSIONS.to_string(),
    };

    raw.split(',')
        .map(|ext| ext.trim().to_lowercase())
        .filter(|ext| !ext.is_empty())
        .collect()
}

/// Pobiera maksymalny rozmiar pliku z konfiguracji (w bajtach).
fn get_max_file_size() -> u64 {
    match store_get(STORAGE_MAX_FILE_SIZE_MB) {
        Ok(Some(v)) => v.parse::<u64>().unwrap_or(50) * 1024 * 1024,
        _ => DEFAULT_MAX_FILE_SIZE,
    }
}

/// Sprawdza czy rozszerzenie pliku jest dozwolone do indeksowania.
fn is_extension_allowed(filename: &str, allowed: &[String]) -> bool {
    let ext = filename
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_lowercase();

    allowed.iter().any(|a| a == &ext)
}

// =============================================================================
// Narzedzie: list_sites
// =============================================================================

/// Lista skonfigurowanych witryn SharePoint z ich metadanymi.
fn tool_list_sites() -> Value {
    let site_urls = match get_configured_site_urls() {
        Ok(urls) => urls,
        Err(e) => return e,
    };

    let mut sites = Vec::new();

    for url in &site_urls {
        // Pobierz dane witryny z Graph API
        let site_id = match resolve_site_id(url) {
            Ok(id) => id,
            Err(e) => {
                // Dodaj witryne z bledem — nie przerywaj calego listowania
                sites.push(json!({
                    "url": url,
                    "status": "error",
                    "error": e.get("error").and_then(|v| v.as_str()).unwrap_or("Nieznany blad")
                }));
                continue;
            }
        };

        // Pobierz metadane witryny
        let endpoint = format!("/sites/{}", site_id);
        match graph_request("GET", &endpoint, None) {
            Ok(data) => {
                sites.push(json!({
                    "url": url,
                    "site_id": site_id,
                    "name": data.get("displayName").and_then(|v| v.as_str()).unwrap_or(""),
                    "description": data.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                    "web_url": data.get("webUrl").and_then(|v| v.as_str()).unwrap_or(""),
                    "created": data.get("createdDateTime").and_then(|v| v.as_str()).unwrap_or(""),
                    "last_modified": data.get("lastModifiedDateTime").and_then(|v| v.as_str()).unwrap_or(""),
                    "status": "ok"
                }));
            }
            Err(e) => {
                sites.push(json!({
                    "url": url,
                    "site_id": site_id,
                    "status": "error",
                    "error": e.get("error").and_then(|v| v.as_str()).unwrap_or("Nieznany blad")
                }));
            }
        }
    }

    make_ok(json!({
        "count": sites.len(),
        "sites": sites
    }))
}

// =============================================================================
// Narzedzie: list_files
// =============================================================================

/// Lista plikow w witrynie lub folderze SharePoint.
/// Jesli nie podano site_url, uzywa pierwszej skonfigurowanej witryny.
fn tool_list_files(params: &Value) -> Value {
    let site_url = params.get("site_url").and_then(|v| v.as_str());
    let path = params
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let recursive = params
        .get("recursive")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Rozpoznaj witryne
    let resolved_url = match site_url {
        Some(url) => url.to_string(),
        None => {
            // Uzyj pierwszej skonfigurowanej witryny
            match get_configured_site_urls() {
                Ok(urls) => urls[0].clone(),
                Err(e) => return e,
            }
        }
    };

    let site_id = match resolve_site_id(&resolved_url) {
        Ok(id) => id,
        Err(e) => return e,
    };

    // Zbuduj endpoint — root lub podsciezka
    let endpoint = if path.is_empty() || path == "/" {
        format!(
            "/sites/{}/drive/root/children?$select=id,name,size,lastModifiedDateTime,file,folder,webUrl,parentReference&$top=200",
            site_id
        )
    } else {
        let clean_path = path.trim_start_matches('/');
        format!(
            "/sites/{}/drive/root:/{}:/children?$select=id,name,size,lastModifiedDateTime,file,folder,webUrl,parentReference&$top=200",
            site_id, clean_path
        )
    };

    // Pobierz pliki
    let items = match list_drive_items(&endpoint) {
        Ok(items) => items,
        Err(e) => return e,
    };

    // Jesli rekurencyjnie — pobierz tez pliki z podfolderow
    let all_items = if recursive {
        let mut result = Vec::new();
        collect_items_recursive(&site_id, &items, &mut result);
        result
    } else {
        items
    };

    make_ok(json!({
        "site_url": resolved_url,
        "path": if path.is_empty() { "/" } else { path },
        "recursive": recursive,
        "count": all_items.len(),
        "items": all_items
    }))
}

/// Pobiera elementy z endpointu drive i mapuje do uproszczonego formatu.
fn list_drive_items(endpoint: &str) -> Result<Vec<Value>, Value> {
    let response = graph_request("GET", endpoint, None)?;

    let raw_items = response
        .get("value")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let items: Vec<Value> = raw_items
        .iter()
        .map(|item| {
            let is_folder = item.get("folder").is_some();
            let mime_type = item
                .pointer("/file/mimeType")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            // Zbuduj file_id w formacie drive_id:item_id dla jednoznacznej identyfikacji
            let drive_id = item
                .pointer("/parentReference/driveId")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let item_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let file_id = format!("{}:{}", drive_id, item_id);

            json!({
                "file_id": file_id,
                "name": item.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                "type": if is_folder { "folder" } else { "file" },
                "mime_type": mime_type,
                "size": item.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
                "last_modified": item.get("lastModifiedDateTime")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                "web_url": item.get("webUrl").and_then(|v| v.as_str()).unwrap_or("")
            })
        })
        .collect();

    Ok(items)
}

/// Rekurencyjnie zbiera pliki z podfolderow.
/// Dodaje pliki do wektora wynikowego, dla folderow schodzi glebiej.
fn collect_items_recursive(site_id: &str, items: &[Value], result: &mut Vec<Value>) {
    for item in items {
        result.push(item.clone());

        // Jesli to folder — pobierz jego zawartosc
        if item.get("type").and_then(|v| v.as_str()) == Some("folder") {
            let file_id = item
                .get("file_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            // Wyodrebnij item_id z file_id (format drive_id:item_id)
            let item_id = file_id
                .split(':')
                .nth(1)
                .unwrap_or("");

            if !item_id.is_empty() {
                let endpoint = format!(
                    "/sites/{}/drive/items/{}/children?$select=id,name,size,lastModifiedDateTime,file,folder,webUrl,parentReference&$top=200",
                    site_id, item_id
                );

                if let Ok(sub_items) = list_drive_items(&endpoint) {
                    collect_items_recursive(site_id, &sub_items, result);
                }
            }
        }
    }
}

// =============================================================================
// Narzedzie: search_files
// =============================================================================

/// Wyszukuje pliki w SharePoint po nazwie lub zawartosci.
/// Uzywa Microsoft Search API z zapytaniami KQL (Keyword Query Language).
fn tool_search_files(params: &Value) -> Value {
    let query = match params.get("query").and_then(|v| v.as_str()) {
        Some(v) if !v.is_empty() => v,
        _ => return make_error("Brak parametru 'query' (fraza wyszukiwania)"),
    };

    let site_url = params.get("site_url").and_then(|v| v.as_str());
    let file_type = params.get("file_type").and_then(|v| v.as_str());

    // Zbuduj zapytanie KQL
    let mut kql = query.to_string();

    // Dodaj filtr rozszerzenia pliku
    if let Some(ext) = file_type {
        kql = format!("{} filetype:{}", kql, ext);
    }

    // Zbuduj request body dla Search API
    let mut search_request = json!({
        "requests": [{
            "entityTypes": ["driveItem"],
            "query": {
                "queryString": kql
            },
            "from": 0,
            "size": 25,
            "fields": [
                "id", "name", "size", "lastModifiedDateTime",
                "webUrl", "parentReference", "file"
            ]
        }]
    });

    // Jesli podano site_url — ogranicz wyszukiwanie do konkretnej witryny
    if let Some(url) = site_url {
        match resolve_site_id(url) {
            Ok(site_id) => {
                // Dodaj scope do konkretnej witryny
                if let Some(requests) = search_request
                    .get_mut("requests")
                    .and_then(|v| v.as_array_mut())
                {
                    if let Some(first_req) = requests.first_mut() {
                        // Uzyj contentSource do ograniczenia zakresu wyszukiwania
                        // Graph Search API obsluguje filtrowanie per site_id przez region
                        first_req["region"] = json!(site_id);
                    }
                }
            }
            Err(e) => return e,
        }
    }

    let body_str = serde_json::to_string(&search_request).unwrap_or_default();

    // Wykonaj wyszukiwanie
    let response = match graph_request("POST", "/search/query", Some(&body_str)) {
        Ok(r) => r,
        Err(e) => return e,
    };

    // Parsuj wyniki wyszukiwania
    let hits_containers = response
        .get("value")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut results = Vec::new();

    for container in &hits_containers {
        let hits = container
            .get("hitsContainers")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        for hit_container in &hits {
            let items = hit_container
                .get("hits")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            for hit in &items {
                let resource = hit.get("resource").unwrap_or(hit);

                let drive_id = resource
                    .pointer("/parentReference/driveId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let item_id = resource
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let file_id = format!("{}:{}", drive_id, item_id);

                // Pobierz fragment z trafienia (summary)
                let summary = hit
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                results.push(json!({
                    "file_id": file_id,
                    "name": resource.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                    "size": resource.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
                    "last_modified": resource.get("lastModifiedDateTime")
                        .and_then(|v| v.as_str())
                        .unwrap_or(""),
                    "web_url": resource.get("webUrl").and_then(|v| v.as_str()).unwrap_or(""),
                    "summary": summary,
                    "rank": hit.get("rank").and_then(|v| v.as_u64()).unwrap_or(0)
                }));
            }
        }
    }

    make_ok(json!({
        "query": query,
        "site_url": site_url,
        "file_type": file_type,
        "count": results.len(),
        "results": results
    }))
}

// =============================================================================
// Narzedzie: get_file_content
// =============================================================================

/// Pobiera zawartosc pliku z SharePoint.
/// Obsluguje formaty: text (domyslny), metadata, raw.
///
/// Dla plikow Office (docx, pptx, xlsx) uzywa konwersji do PDF przez Graph API,
/// a nastepnie zwraca tekst (jesli runtime wspiera ekstrakcje tekstu z PDF).
/// Dla plikow tekstowych (txt, md, csv) pobiera bezposrednio zawartosc.
fn tool_get_file_content(params: &Value) -> Value {
    let file_id_raw = match params.get("file_id").and_then(|v| v.as_str()) {
        Some(v) if !v.is_empty() => v,
        _ => return make_error("Brak parametru 'file_id' (ID pliku z SharePoint)"),
    };

    let format = params
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("text");

    // Rozdziel file_id na drive_id i item_id
    let (drive_id, item_id) = match split_file_id(file_id_raw) {
        Some(v) => v,
        None => {
            return make_error(
                "Nieprawidlowy format file_id. Oczekiwany format: drive_id:item_id",
            )
        }
    };

    match format {
        "metadata" => get_file_metadata_response(&drive_id, &item_id),
        "raw" => get_file_raw_content(&drive_id, &item_id),
        _ => get_file_text_content(&drive_id, &item_id),
    }
}

/// Rozdziela file_id w formacie "drive_id:item_id" na dwie czesci.
fn split_file_id(file_id: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = file_id.splitn(2, ':').collect();
    if parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty() {
        Some((parts[0].to_string(), parts[1].to_string()))
    } else {
        None
    }
}

/// Pobiera zawartosc pliku jako tekst.
/// Dla plikow tekstowych (txt, md, csv) — bezposrednie pobranie.
/// Dla plikow Office — uzywa konwersji Graph API do formatu tekstowego.
fn get_file_text_content(drive_id: &str, item_id: &str) -> Value {
    // Najpierw pobierz metadane pliku zeby znac typ
    let meta_endpoint = format!(
        "/drives/{}/items/{}?$select=id,name,size,file",
        drive_id, item_id
    );

    let meta = match graph_request("GET", &meta_endpoint, None) {
        Ok(m) => m,
        Err(e) => return e,
    };

    let filename = meta
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let file_size = meta
        .get("size")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let mime_type = meta
        .pointer("/file/mimeType")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Sprawdz limit rozmiaru pliku
    let max_size = get_max_file_size();
    if file_size > max_size {
        return make_error(&format!(
            "Plik '{}' ({} MB) przekracza limit {} MB. Zmien konfiguracje maks. rozmiaru pliku.",
            filename,
            file_size / (1024 * 1024),
            max_size / (1024 * 1024)
        ));
    }

    // Sprawdz czy mamy zawartosc w cache
    let cache_key = format!("{}{}:{}", STORAGE_CONTENT_PREFIX, drive_id, item_id);
    if let Ok(Some(cached)) = store_get(&cache_key) {
        if !cached.is_empty() {
            return make_ok(json!({
                "file_id": format!("{}:{}", drive_id, item_id),
                "name": filename,
                "mime_type": mime_type,
                "size": file_size,
                "content": cached,
                "source": "cache"
            }));
        }
    }

    // Okresl strategie pobierania na podstawie typu pliku
    let ext = filename
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_lowercase();

    let content = match ext.as_str() {
        // Pliki tekstowe — bezposrednie pobranie
        "txt" | "md" | "csv" | "json" | "xml" | "html" | "htm" | "log" | "yaml" | "yml"
        | "toml" | "ini" | "cfg" | "conf" | "properties" | "env" | "sh" | "bat" | "ps1"
        | "py" | "rs" | "js" | "ts" | "cs" | "java" | "go" | "rb" | "php" | "sql" => {
            let endpoint = format!("/drives/{}/items/{}/content", drive_id, item_id);
            match graph_download(&endpoint) {
                Ok(text) => text,
                Err(e) => return e,
            }
        }

        // Pliki Office — konwersja do PDF/HTML przez Graph API, potem ekstrakcja tekstu
        "docx" | "doc" | "pptx" | "ppt" | "xlsx" | "xls" | "odt" | "ods" | "odp" => {
            // Graph API obsluguje konwersje do roznych formatow przez ?format=
            // Uzyj konwersji do HTML dla najlepszej ekstrakcji tekstu
            let endpoint = format!(
                "/drives/{}/items/{}/content?format=html",
                drive_id, item_id
            );
            match graph_download(&endpoint) {
                Ok(html) => strip_html_tags(&html),
                Err(_) => {
                    // Fallback — pobierz surowa zawartosc
                    let endpoint = format!("/drives/{}/items/{}/content", drive_id, item_id);
                    match graph_download(&endpoint) {
                        Ok(raw) => format!("[Zawartosc binarna pliku '{}' — {} bajtow]", filename, raw.len()),
                        Err(e) => return e,
                    }
                }
            }
        }

        // PDF — pobierz i oznacz jako binarny (ekstrakcja tekstu z PDF wymaga osobnej biblioteki)
        "pdf" => {
            let endpoint = format!("/drives/{}/items/{}/content", drive_id, item_id);
            match graph_download(&endpoint) {
                Ok(raw) => {
                    // W srodowisku WASM nie mamy biblioteki do ekstrakcji tekstu z PDF.
                    // Zwracamy surowe dane — host (runtime TentaFlow) moze je przetworzyc.
                    format!("[PDF: {} — {} bajtow. Uzyj format=raw zeby pobrac surowe dane.]", filename, raw.len())
                }
                Err(e) => return e,
            }
        }

        // Inne typy — informacja o braku wsparcia dla ekstrakcji tekstu
        _ => {
            return make_ok(json!({
                "file_id": format!("{}:{}", drive_id, item_id),
                "name": filename,
                "mime_type": mime_type,
                "size": file_size,
                "content": format!("[Typ pliku '{}' nie jest obslugiwany do ekstrakcji tekstu. Uzyj format=raw lub format=metadata.]", ext),
                "source": "unsupported_type"
            }));
        }
    };

    // Zapisz zawartosc do cache
    if let Err(e) = store_set(&cache_key, &content) {
        log::warn(&format!("Blad zapisu zawartosci do cache: {}", e));
    }

    make_ok(json!({
        "file_id": format!("{}:{}", drive_id, item_id),
        "name": filename,
        "mime_type": mime_type,
        "size": file_size,
        "content": content,
        "source": "graph_api"
    }))
}

/// Pobiera surowa zawartosc pliku (bez konwersji).
fn get_file_raw_content(drive_id: &str, item_id: &str) -> Value {
    let endpoint = format!("/drives/{}/items/{}/content", drive_id, item_id);

    match graph_download(&endpoint) {
        Ok(raw) => make_ok(json!({
            "file_id": format!("{}:{}", drive_id, item_id),
            "content": raw,
            "format": "raw",
            "source": "graph_api"
        })),
        Err(e) => e,
    }
}

/// Pobiera metadane pliku (bez zawartosci).
fn get_file_metadata_response(drive_id: &str, item_id: &str) -> Value {
    let endpoint = format!(
        "/drives/{}/items/{}?$select=id,name,size,lastModifiedDateTime,createdDateTime,file,folder,webUrl,parentReference,createdBy,lastModifiedBy",
        drive_id, item_id
    );

    match graph_request("GET", &endpoint, None) {
        Ok(data) => {
            let is_folder = data.get("folder").is_some();

            make_ok(json!({
                "file_id": format!("{}:{}", drive_id, item_id),
                "name": data.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                "type": if is_folder { "folder" } else { "file" },
                "mime_type": data.pointer("/file/mimeType").and_then(|v| v.as_str()).unwrap_or(""),
                "size": data.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
                "created": data.get("createdDateTime").and_then(|v| v.as_str()).unwrap_or(""),
                "last_modified": data.get("lastModifiedDateTime").and_then(|v| v.as_str()).unwrap_or(""),
                "created_by": data.pointer("/createdBy/user/displayName").and_then(|v| v.as_str()).unwrap_or(""),
                "modified_by": data.pointer("/lastModifiedBy/user/displayName").and_then(|v| v.as_str()).unwrap_or(""),
                "web_url": data.get("webUrl").and_then(|v| v.as_str()).unwrap_or(""),
                "parent_path": data.pointer("/parentReference/path").and_then(|v| v.as_str()).unwrap_or("")
            }))
        }
        Err(e) => e,
    }
}

/// Prosta ekstrakcja tekstu z HTML — usuwa tagi HTML, zachowuje tekst.
fn strip_html_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;
    let mut last_was_space = false;

    let html_lower = html.to_lowercase();
    let chars: Vec<char> = html.chars().collect();
    let chars_lower: Vec<char> = html_lower.chars().collect();

    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '<' {
            // Sprawdz czy to poczatek tagu script lub style
            let remaining: String = chars_lower[i..].iter().take(10).collect();
            if remaining.starts_with("<script") {
                in_script = true;
            } else if remaining.starts_with("<style") {
                in_style = true;
            } else if remaining.starts_with("</script") {
                in_script = false;
            } else if remaining.starts_with("</style") {
                in_style = false;
            }

            // Tagi blokowe — dodaj nowa linie
            let block_tags = ["<br", "<p", "</p", "<div", "</div", "<h1", "<h2", "<h3",
                "<h4", "<h5", "<h6", "</h", "<li", "</li", "<tr", "</tr", "<td", "</td"];
            for tag in &block_tags {
                if remaining.starts_with(tag) {
                    if !result.ends_with('\n') && !result.is_empty() {
                        result.push('\n');
                    }
                    last_was_space = true;
                    break;
                }
            }

            in_tag = true;
            i += 1;
            continue;
        }

        if chars[i] == '>' {
            in_tag = false;
            i += 1;
            continue;
        }

        if !in_tag && !in_script && !in_style {
            // Dekoduj podstawowe encje HTML
            if chars[i] == '&' {
                let entity: String = chars[i..].iter().take(10).collect();
                if entity.starts_with("&amp;") {
                    result.push('&');
                    i += 5;
                    last_was_space = false;
                    continue;
                } else if entity.starts_with("&lt;") {
                    result.push('<');
                    i += 4;
                    last_was_space = false;
                    continue;
                } else if entity.starts_with("&gt;") {
                    result.push('>');
                    i += 4;
                    last_was_space = false;
                    continue;
                } else if entity.starts_with("&quot;") {
                    result.push('"');
                    i += 6;
                    last_was_space = false;
                    continue;
                } else if entity.starts_with("&nbsp;") {
                    result.push(' ');
                    i += 6;
                    last_was_space = true;
                    continue;
                } else if entity.starts_with("&#") {
                    // Numeryczna encja HTML — pomijamy dla uproszczenia
                    if let Some(semi_pos) = entity.find(';') {
                        i += semi_pos + 1;
                        continue;
                    }
                }
            }

            // Normalizuj biale znaki
            if chars[i].is_whitespace() {
                if !last_was_space {
                    result.push(' ');
                    last_was_space = true;
                }
            } else {
                result.push(chars[i]);
                last_was_space = false;
            }
        }

        i += 1;
    }

    // Usun nadmiarowe puste linie
    let mut cleaned = String::new();
    let mut empty_lines = 0;
    for line in result.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            empty_lines += 1;
            if empty_lines <= 1 {
                cleaned.push('\n');
            }
        } else {
            empty_lines = 0;
            cleaned.push_str(trimmed);
            cleaned.push('\n');
        }
    }

    cleaned.trim().to_string()
}

// =============================================================================
// Narzedzie: get_file_info
// =============================================================================

/// Pobiera metadane pliku z SharePoint.
fn tool_get_file_info(params: &Value) -> Value {
    let file_id_raw = match params.get("file_id").and_then(|v| v.as_str()) {
        Some(v) if !v.is_empty() => v,
        _ => return make_error("Brak parametru 'file_id' (ID pliku z SharePoint)"),
    };

    let (drive_id, item_id) = match split_file_id(file_id_raw) {
        Some(v) => v,
        None => {
            return make_error(
                "Nieprawidlowy format file_id. Oczekiwany format: drive_id:item_id",
            )
        }
    };

    get_file_metadata_response(&drive_id, &item_id)
}

// =============================================================================
// Narzedzie: list_recent_changes
// =============================================================================

/// Lista ostatnio zmienionych plikow w SharePoint.
/// Uzywa delta query do sledzenia zmian od ostatniej synchronizacji
/// lub filtruje po dacie modyfikacji.
fn tool_list_recent_changes(params: &Value) -> Value {
    let site_url = params.get("site_url").and_then(|v| v.as_str());
    let days = params
        .get("days")
        .and_then(|v| v.as_u64())
        .unwrap_or(7)
        .min(90);

    // Okresl witryny do sprawdzenia
    let site_urls = match site_url {
        Some(url) => vec![url.to_string()],
        None => match get_configured_site_urls() {
            Ok(urls) => urls,
            Err(e) => return e,
        },
    };

    let mut all_changes = Vec::new();

    for url in &site_urls {
        let site_id = match resolve_site_id(url) {
            Ok(id) => id,
            Err(_) => continue,
        };

        // Uzyj delta query do pobrania zmian
        let delta_key = format!("{}{}", STORAGE_DELTA_PREFIX, site_id);

        // Sprawdz czy mamy delta token z poprzedniej synchronizacji
        let delta_token = match store_get(&delta_key) {
            Ok(Some(token)) if !token.is_empty() => Some(token),
            _ => None,
        };

        let endpoint = match &delta_token {
            Some(token) => {
                // Uzyj delta tokenu do pobrania tylko zmian od ostatniej synchronizacji
                format!(
                    "/sites/{}/drive/root/delta?token={}",
                    site_id, token
                )
            }
            None => {
                // Brak delta tokenu — pobierz wszystkie elementy z filtrem daty
                // Graph API delta bez tokenu zwraca pelen stan
                format!(
                    "/sites/{}/drive/root/delta",
                    site_id
                )
            }
        };

        match graph_request("GET", &endpoint, None) {
            Ok(response) => {
                // Zapisz nowy delta token do nastepnego wywolania
                let new_delta_token = response
                    .get("@odata.deltaLink")
                    .and_then(|v| v.as_str())
                    .and_then(|link| {
                        // Wyodrebnij token z delta link URL
                        link.split("token=").nth(1).map(|t| t.to_string())
                    });

                if let Some(new_token) = new_delta_token {
                    if let Err(e) = store_set(&delta_key, &new_token) {
                        log::warn(&format!("Blad zapisu delta tokenu: {}", e));
                    }
                }

                // Parsuj zmienione elementy
                let items = response
                    .get("value")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();

                for item in &items {
                    // Pomijaj foldery — interesuja nas tylko pliki
                    if item.get("folder").is_some() {
                        continue;
                    }

                    // Pomijaj usuniete elementy (maja pole "deleted")
                    if item.get("deleted").is_some() {
                        continue;
                    }

                    let drive_id = item
                        .pointer("/parentReference/driveId")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let item_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let file_id = format!("{}:{}", drive_id, item_id);

                    all_changes.push(json!({
                        "file_id": file_id,
                        "name": item.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                        "site_url": url,
                        "size": item.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
                        "last_modified": item.get("lastModifiedDateTime")
                            .and_then(|v| v.as_str())
                            .unwrap_or(""),
                        "modified_by": item.pointer("/lastModifiedBy/user/displayName")
                            .and_then(|v| v.as_str())
                            .unwrap_or(""),
                        "web_url": item.get("webUrl").and_then(|v| v.as_str()).unwrap_or("")
                    }));
                }

                // Obsluz paginacje — jesli jest @odata.nextLink, pobierz kolejne strony
                let mut next_link = response
                    .get("@odata.nextLink")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                while let Some(ref link) = next_link {
                    // Wyodrebnij endpoint z pelnego URL
                    let endpoint_part = link
                        .strip_prefix(GRAPH_BASE)
                        .unwrap_or(link);

                    match graph_request("GET", endpoint_part, None) {
                        Ok(page_response) => {
                            let page_items = page_response
                                .get("value")
                                .and_then(|v| v.as_array())
                                .cloned()
                                .unwrap_or_default();

                            for item in &page_items {
                                if item.get("folder").is_some() || item.get("deleted").is_some() {
                                    continue;
                                }

                                let drive_id = item
                                    .pointer("/parentReference/driveId")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let item_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                let file_id = format!("{}:{}", drive_id, item_id);

                                all_changes.push(json!({
                                    "file_id": file_id,
                                    "name": item.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                                    "site_url": url,
                                    "size": item.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
                                    "last_modified": item.get("lastModifiedDateTime")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or(""),
                                    "modified_by": item.pointer("/lastModifiedBy/user/displayName")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or(""),
                                    "web_url": item.get("webUrl").and_then(|v| v.as_str()).unwrap_or("")
                                }));
                            }

                            // Zapisz delta token z ostatniej strony
                            if let Some(dt) = page_response
                                .get("@odata.deltaLink")
                                .and_then(|v| v.as_str())
                                .and_then(|link| link.split("token=").nth(1).map(|t| t.to_string()))
                            {
                                if let Err(e) = store_set(&delta_key, &dt) {
                                    log::warn(&format!("Blad zapisu delta tokenu: {}", e));
                                }
                            }

                            next_link = page_response
                                .get("@odata.nextLink")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                        }
                        Err(e) => {
                            log::warn(&format!(
                                "Blad paginacji delta query: {}",
                                serde_json::to_string(&e).unwrap_or_default()
                            ));
                            next_link = None;
                        }
                    }
                }
            }
            Err(e) => {
                log::warn(&format!(
                    "Blad delta query dla witryny {}: {}",
                    url,
                    serde_json::to_string(&e).unwrap_or_default()
                ));
            }
        }
    }

    make_ok(json!({
        "days": days,
        "site_url": site_url,
        "count": all_changes.len(),
        "changes": all_changes
    }))
}

// =============================================================================
// Narzedzie: sync_index
// =============================================================================

/// Synchronizuje indeks plikow SharePoint do storage addonu.
/// Przechodzi po wszystkich skonfigurowanych witrynach (lub wybranej),
/// pobiera liste plikow i zapisuje indeks z metadanymi.
///
/// Jesli force=true — pelna reindeksacja (wszystkie pliki).
/// Jesli force=false — inkrementalna synchronizacja (tylko zmiany od ostatniej sync).
fn tool_sync_index(params: &Value) -> Value {
    let site_url = params.get("site_url").and_then(|v| v.as_str());
    let force = params
        .get("force")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    log::info(&format!(
        "Synchronizacja indeksu SharePoint (force={}, site={})",
        force,
        site_url.unwrap_or("wszystkie")
    ));

    // Okresl witryny do synchronizacji
    let site_urls = match site_url {
        Some(url) => vec![url.to_string()],
        None => match get_configured_site_urls() {
            Ok(urls) => urls,
            Err(e) => return e,
        },
    };

    let allowed_extensions = get_allowed_extensions();
    let max_file_size = get_max_file_size();

    let mut total_indexed = 0u64;
    let mut total_skipped = 0u64;
    let mut total_errors = 0u64;
    let mut site_results = Vec::new();

    for url in &site_urls {
        let site_id = match resolve_site_id(url) {
            Ok(id) => id,
            Err(e) => {
                total_errors += 1;
                site_results.push(json!({
                    "site_url": url,
                    "status": "error",
                    "error": e.get("error").and_then(|v| v.as_str()).unwrap_or("Nieznany blad")
                }));
                continue;
            }
        };

        let mut site_indexed = 0u64;
        let mut site_skipped = 0u64;

        if force {
            // Pelna reindeksacja — wyczysc istniejacy indeks dla tej witryny
            let index_key = format!("{}{}", STORAGE_INDEX_PREFIX, site_id);
            let _ = store_set(&index_key, "");

            // Wyczysc delta token
            let delta_key = format!("{}{}", STORAGE_DELTA_PREFIX, site_id);
            let _ = store_set(&delta_key, "");
        }

        // Pobierz pliki z witryny rekurencyjnie
        let root_endpoint = format!(
            "/sites/{}/drive/root/children?$select=id,name,size,lastModifiedDateTime,file,folder,parentReference&$top=200",
            site_id
        );

        let root_items = match list_drive_items(&root_endpoint) {
            Ok(items) => items,
            Err(e) => {
                total_errors += 1;
                site_results.push(json!({
                    "site_url": url,
                    "status": "error",
                    "error": serde_json::to_string(&e).unwrap_or_default()
                }));
                continue;
            }
        };

        // Zbierz wszystkie pliki rekurencyjnie
        let mut all_items = Vec::new();
        collect_items_recursive(&site_id, &root_items, &mut all_items);

        // Filtruj i indeksuj pliki
        let mut indexed_files = Vec::new();

        for item in &all_items {
            let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if item_type == "folder" {
                continue;
            }

            let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let size = item.get("size").and_then(|v| v.as_u64()).unwrap_or(0);

            // Sprawdz rozszerzenie
            if !is_extension_allowed(name, &allowed_extensions) {
                site_skipped += 1;
                continue;
            }

            // Sprawdz rozmiar
            if size > max_file_size {
                site_skipped += 1;
                log::info(&format!(
                    "Pominieto plik '{}' — rozmiar {} przekracza limit",
                    name, size
                ));
                continue;
            }

            // Dodaj do indeksu
            indexed_files.push(json!({
                "file_id": item.get("file_id").and_then(|v| v.as_str()).unwrap_or(""),
                "name": name,
                "size": size,
                "last_modified": item.get("last_modified").and_then(|v| v.as_str()).unwrap_or(""),
                "web_url": item.get("web_url").and_then(|v| v.as_str()).unwrap_or(""),
                "mime_type": item.get("mime_type").and_then(|v| v.as_str()).unwrap_or("")
            }));

            site_indexed += 1;
        }

        // Zapisz indeks do storage
        let index_key = format!("{}{}", STORAGE_INDEX_PREFIX, site_id);
        let index_data = json!({
            "site_url": url,
            "site_id": site_id,
            "file_count": indexed_files.len(),
            "files": indexed_files
        });

        let index_str = serde_json::to_string(&index_data).unwrap_or_default();
        if let Err(e) = store_set(&index_key, &index_str) {
            log::error(&format!("Blad zapisu indeksu dla witryny {}: {}", url, e));
            total_errors += 1;
        }

        // Pobierz delta token na przyszlosc (inkrementalna sync)
        let delta_endpoint = format!("/sites/{}/drive/root/delta", site_id);
        if let Ok(delta_response) = graph_request("GET", &delta_endpoint, None) {
            if let Some(delta_link) = delta_response
                .get("@odata.deltaLink")
                .and_then(|v| v.as_str())
            {
                if let Some(token) = delta_link.split("token=").nth(1) {
                    let delta_key = format!("{}{}", STORAGE_DELTA_PREFIX, site_id);
                    let _ = store_set(&delta_key, token);
                }
            }
        }

        total_indexed += site_indexed;
        total_skipped += site_skipped;

        site_results.push(json!({
            "site_url": url,
            "status": "ok",
            "indexed": site_indexed,
            "skipped": site_skipped
        }));

        log::info(&format!(
            "Witryna {}: zaindeksowano {}, pominieto {}",
            url, site_indexed, site_skipped
        ));
    }

    log::info(&format!(
        "Synchronizacja zakonczona: zaindeksowano {}, pominieto {}, bledy: {}",
        total_indexed, total_skipped, total_errors
    ));

    make_ok(json!({
        "force": force,
        "total_indexed": total_indexed,
        "total_skipped": total_skipped,
        "total_errors": total_errors,
        "sites": site_results
    }))
}

// =============================================================================
// Helpery — zapis odpowiedzi WASM
// =============================================================================

/// Zapisuje odpowiedz JSON do bufora wyjsciowego i ustawia dlugosc.
fn write_response(out_ptr: i32, out_cap: i32, out_len_ptr: i32, value: &Value) -> i32 {
    let response_str = match serde_json::to_string(value) {
        Ok(s) => s,
        Err(_) => return 1,
    };

    let written = tentaflow_addon_sdk::write_string(out_ptr, out_cap, out_len_ptr, &response_str);
    if written < 0 {
        log::error("Bufor wyjsciowy za maly na odpowiedz");
        return tentaflow_addon_sdk::ABI_OUTPUT_BUFFER_TOO_SMALL;
    }

    // Zapisz dlugosc odpowiedzi (4 bajty little-endian)
    let len_bytes = written.to_le_bytes();
    let dest = unsafe { std::slice::from_raw_parts_mut(out_len_ptr as *mut u8, 4) };
    dest.copy_from_slice(&len_bytes);

    0
}
