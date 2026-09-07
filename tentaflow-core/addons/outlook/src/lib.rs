// =============================================================================
// Plik: addons/outlook/src/lib.rs
// Opis: Addon Microsoft Outlook dla TentaFlow — pelna integracja z Microsoft
//       Graph API: odczyt, wyszukiwanie, wysylanie maili, foldery, zalaczniki.
//       Kompilowany do WASM (cdylib). Delegated OAuth per uzytkownik.
// =============================================================================

use tentaflow_addon_sdk::prelude::*;

// =============================================================================
// Stale — endpointy Microsoft Graph API i klucze konfiguracji
// =============================================================================

/// Bazowy URL Microsoft Graph API v1.0
const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";

/// Endpoint autoryzacji OAuth2 (Azure AD / Entra ID)
const AUTH_BASE: &str = "https://login.microsoftonline.com";

/// Klucz sekretu OAuth access token
const SECRET_OAUTH_TOKEN: &str = "oauth_token";

/// Klucz sekretu refresh token
const SECRET_REFRESH_TOKEN: &str = "refresh_token";

/// Klucz sekretu client ID (Azure AD)
const SECRET_CLIENT_ID: &str = "client_id";

/// Klucz sekretu client secret (Azure AD)
const SECRET_CLIENT_SECRET: &str = "client_secret";

/// Klucz konfiguracji tenant ID w storage
const STORAGE_TENANT_ID: &str = "config.tenant_id";

/// Klucz konfiguracji domyslnej liczby wynikow
const STORAGE_MAX_RESULTS: &str = "config.max_results";

/// Klucz konfiguracji automatycznych powiadomien
const STORAGE_AUTO_NOTIFICATIONS: &str = "config.auto_notifications";

/// Domyslna liczba wynikow na strone
const DEFAULT_MAX_RESULTS: u64 = 20;

/// Maksymalna liczba znakow w podgladzie maila
const PREVIEW_LENGTH: usize = 200;

// =============================================================================
// Lifecycle hooks — eksporty WASM
// =============================================================================

/// Wywolywane przy instalacji addonu.
/// Inicjalizuje domyslna konfiguracje w storage.
#[no_mangle]
pub extern "C" fn on_install() -> i32 {
    log::info("Outlook addon: instalacja rozpoczeta");

    // Ustaw domyslna konfiguracje
    let defaults: &[(&str, &str)] = &[
        (STORAGE_MAX_RESULTS, "20"),
        (STORAGE_AUTO_NOTIFICATIONS, "false"),
        ("initialized", "true"),
    ];

    for (key, value) in defaults {
        if let Err(e) = store_set(key, value) {
            log::error(&format!("Blad inicjalizacji storage [{}]: {}", key, e));
            return 1;
        }
    }

    log::info("Outlook addon: instalacja zakonczona pomyslnie");
    0
}

/// Wywolywane przy uruchomieniu instancji addonu.
/// Rejestruje narzedzia LLM i subskrybuje eventy.
#[no_mangle]
pub extern "C" fn on_start() -> i32 {
    log::info("Outlook addon: uruchamianie");

    // -------------------------------------------------------------------------
    // Rejestracja narzedzi LLM tool calling
    // -------------------------------------------------------------------------

    register_tool(
        "outlook.list_emails",
        "Lista maili z folderu poczty (domyslnie Inbox). Zwraca uproszczone obiekty z tematem, nadawca, data, podgladem.",
        json!({
            "type": "object",
            "properties": {
                "folder": {
                    "type": "string",
                    "description": "Nazwa folderu (domyslnie inbox)"
                },
                "limit": {
                    "type": "number",
                    "description": "Ile maili pobrac (domyslnie 20)"
                },
                "skip": {
                    "type": "number",
                    "description": "Ile maili pominac (paginacja)"
                },
                "filter": {
                    "type": "string",
                    "description": "Filtr OData (np. isRead eq false)"
                }
            }
        }),
    );

    register_tool(
        "outlook.read_email",
        "Odczyt konkretnego maila z pelna trescia, zalacznikami i metadanymi. Wymaga ID wiadomosci.",
        json!({
            "type": "object",
            "required": ["message_id"],
            "properties": {
                "message_id": {
                    "type": "string",
                    "description": "ID wiadomosci email"
                }
            }
        }),
    );

    register_tool(
        "outlook.search_emails",
        "Wyszukiwanie maili po tresci, temacie, nadawcy. Mozna filtrowac po dacie i zalacznikach.",
        json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Fraza wyszukiwania"
                },
                "folder": {
                    "type": "string",
                    "description": "Folder do przeszukania (domyslnie wszystkie)"
                },
                "from_date": {
                    "type": "string",
                    "description": "Data poczatkowa w formacie ISO 8601 (np. 2026-03-01)"
                },
                "has_attachment": {
                    "type": "boolean",
                    "description": "Filtruj tylko maile z zalacznikami"
                }
            }
        }),
    );

    register_tool(
        "outlook.send_email",
        "Wysylanie wiadomosci email. Wymaga odbiorcy, tematu i tresci. Opcjonalnie CC i format HTML.",
        json!({
            "type": "object",
            "required": ["to", "subject", "body"],
            "properties": {
                "to": {
                    "type": "string",
                    "description": "Adres email odbiorcy (lub lista oddzielona przecinkami)"
                },
                "subject": {
                    "type": "string",
                    "description": "Temat wiadomosci"
                },
                "body": {
                    "type": "string",
                    "description": "Tresc wiadomosci"
                },
                "cc": {
                    "type": "string",
                    "description": "Adresy CC (oddzielone przecinkami)"
                },
                "is_html": {
                    "type": "boolean",
                    "description": "Czy tresc jest w formacie HTML (domyslnie false)"
                }
            }
        }),
    );

    register_tool(
        "outlook.reply_email",
        "Odpowiedz na wiadomosc email. Wymaga ID wiadomosci i tresci odpowiedzi.",
        json!({
            "type": "object",
            "required": ["message_id", "body"],
            "properties": {
                "message_id": {
                    "type": "string",
                    "description": "ID wiadomosci na ktora odpowiadamy"
                },
                "body": {
                    "type": "string",
                    "description": "Tresc odpowiedzi"
                }
            }
        }),
    );

    register_tool(
        "outlook.list_folders",
        "Lista folderow poczty uzytkownika (Inbox, Sent, Drafts, itp.).",
        json!({
            "type": "object",
            "properties": {}
        }),
    );

    register_tool(
        "outlook.get_attachment",
        "Pobieranie zalacznika z wiadomosci email. Wymaga ID wiadomosci i ID zalacznika.",
        json!({
            "type": "object",
            "required": ["message_id", "attachment_id"],
            "properties": {
                "message_id": {
                    "type": "string",
                    "description": "ID wiadomosci email"
                },
                "attachment_id": {
                    "type": "string",
                    "description": "ID zalacznika"
                }
            }
        }),
    );

    // -------------------------------------------------------------------------
    // Subskrypcja eventow z Core
    // -------------------------------------------------------------------------

    let subscriptions = json!({
        "addon_id": "outlook",
        "events": [
            "mail_received"
        ]
    });

    if let Err(e) = publish_event("addon.subscribe", subscriptions) {
        log::warn(&format!("Nie udalo sie zasubskrybowac eventow: {}", e));
    }

    // -------------------------------------------------------------------------
    // Panel UI — status addonu
    // -------------------------------------------------------------------------

    let panel = json!({
        "type": "column",
        "children": [
            {
                "type": "text",
                "props": {
                    "content": "Microsoft Outlook",
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
                "type": "button",
                "props": {
                    "label": "Zaloguj do Microsoft",
                    "action_id": "outlook_oauth_login"
                }
            }
        ]
    });

    if let Err(e) = render_panel("outlook_main", panel) {
        log::warn(&format!("Blad renderowania panelu UI: {}", e));
    }

    log::info("Outlook addon: uruchomiony pomyslnie");
    0
}

/// Wywolywane przy zatrzymaniu instancji addonu.
/// Wyczysc subskrypcje i zasoby.
#[no_mangle]
pub extern "C" fn on_stop() -> i32 {
    log::info("Outlook addon: zatrzymywanie");

    // Anuluj subskrypcje eventow
    let unsubscribe = json!({
        "addon_id": "outlook",
        "events": []
    });

    if let Err(e) = publish_event("addon.unsubscribe", unsubscribe) {
        log::warn(&format!("Blad anulowania subskrypcji eventow: {}", e));
    }

    log::info("Outlook addon: zatrzymany");
    0
}

// =============================================================================
// Obsluga eventow
// =============================================================================

/// Handler eventow z event bus Core.
/// Obsluguje eventy: mail_received (powiadomienia o nowych mailach).
#[no_mangle]
pub extern "C" fn on_event(event_ptr: i32, event_len: i32) -> i32 {
    let event_json = tentaflow_addon_sdk::read_string(event_ptr, event_len);

    let event: Value = match serde_json::from_str(&event_json) {
        Ok(v) => v,
        Err(e) => {
            log::error(&format!("Blad parsowania eventu: {}", e));
            return 1;
        }
    };

    let event_type = event
        .get("event_type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let payload = event.get("payload").cloned().unwrap_or(json!({}));

    match event_type {
        "mail_received" => handle_mail_received(&payload),
        _ => {
            log::info(&format!("Outlook: nieobslugiwany event: {}", event_type));
            0
        }
    }
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
            let error = json!({"ok": false, "error": format!("Blad parsowania requestu: {}", e)});
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
        "outlook.list_emails" => handle_list_emails(&params),
        "outlook.read_email" => handle_read_email(&params),
        "outlook.search_emails" => handle_search_emails(&params),
        "outlook.send_email" => handle_send_email(&params),
        "outlook.reply_email" => handle_reply_email(&params),
        "outlook.list_folders" => handle_list_folders(&params),
        "outlook.get_attachment" => handle_get_attachment(&params),
        _ => json!({"ok": false, "error": format!("Nieznane narzedzie: {}", tool_name)}),
    };

    write_response(out_ptr, out_cap, out_len_ptr, &result)
}

// =============================================================================
// Helpery — OAuth i Graph API
// =============================================================================

/// Pobiera aktywny OAuth token z sekretow.
/// Jesli token wygasl — probuje odswiezyc przez refresh token.
/// Zwraca token lub komunikat bledu z instrukcja reautoryzacji.
fn get_oauth_token() -> Result<String, Value> {
    // Pobierz aktualny token
    let token = match secret_get_value(SECRET_OAUTH_TOKEN) {
        Ok(Some(t)) if !t.is_empty() => t,
        Ok(_) => {
            return Err(json!({
                "ok": false,
                "error": "Brak tokenu OAuth. Zaloguj sie przez ustawienia addonu Outlook (przycisk 'Zaloguj do Microsoft').",
                "code": "AUTH_REQUIRED"
            }));
        }
        Err(e) => {
            return Err(json!({
                "ok": false,
                "error": format!("Blad odczytu tokenu: {}", e),
                "code": "SECRET_ERROR"
            }));
        }
    };

    Ok(token)
}

/// Probuje odswiezyc OAuth token przez refresh token.
/// Wykonuje POST do /oauth2/v2.0/token z grant_type=refresh_token.
/// Zwraca nowy access token lub blad.
fn refresh_oauth_token() -> Result<String, Value> {
    let refresh_token = match secret_get_value(SECRET_REFRESH_TOKEN) {
        Ok(Some(t)) if !t.is_empty() => t,
        _ => {
            return Err(json!({
                "ok": false,
                "error": "Brak refresh tokenu. Wymagana ponowna autoryzacja OAuth.",
                "code": "AUTH_REQUIRED"
            }));
        }
    };

    let client_id = match secret_get_value(SECRET_CLIENT_ID) {
        Ok(Some(v)) => v,
        _ => {
            return Err(json!({
                "ok": false,
                "error": "Brak Client ID. Skonfiguruj addon w ustawieniach.",
                "code": "CONFIG_MISSING"
            }));
        }
    };

    let client_secret = match secret_get_value(SECRET_CLIENT_SECRET) {
        Ok(Some(v)) => v,
        _ => {
            return Err(json!({
                "ok": false,
                "error": "Brak Client Secret. Skonfiguruj addon w ustawieniach.",
                "code": "CONFIG_MISSING"
            }));
        }
    };

    let tenant_id = match store_get(STORAGE_TENANT_ID) {
        Ok(Some(v)) => v,
        _ => "common".to_string(),
    };

    let token_url = format!("{}/{}/oauth2/v2.0/token", AUTH_BASE, tenant_id);

    let body = format!(
        "grant_type=refresh_token&client_id={}&client_secret={}&refresh_token={}&scope=https://graph.microsoft.com/Mail.Read https://graph.microsoft.com/Mail.ReadWrite https://graph.microsoft.com/Mail.Send offline_access",
        client_id, client_secret, refresh_token
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
            return Err(json!({
                "ok": false,
                "error": format!("Blad HTTP przy odswiezaniu tokenu: {}", e),
                "code": "HTTP_ERROR"
            }));
        }
    };

    if response.status != 200 {
        log::error(&format!(
            "Blad odswiezania tokenu OAuth, status: {}, body: {}",
            response.status, response.body
        ));
        return Err(json!({
            "ok": false,
            "error": "Nie udalo sie odswiezyc tokenu. Wymagana ponowna autoryzacja.",
            "code": "AUTH_REQUIRED"
        }));
    }

    // Parsuj odpowiedz tokenowa
    let token_data: Value = match serde_json::from_str(&response.body) {
        Ok(v) => v,
        Err(e) => {
            return Err(json!({
                "ok": false,
                "error": format!("Blad parsowania odpowiedzi tokenowej: {}", e),
                "code": "PARSE_ERROR"
            }));
        }
    };

    let new_access_token = token_data
        .get("access_token")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if new_access_token.is_empty() {
        return Err(json!({
            "ok": false,
            "error": "Odpowiedz tokenowa nie zawiera access_token",
            "code": "AUTH_ERROR"
        }));
    }

    // Zapisz nowy access token
    if let Err(e) = secret_set_value(SECRET_OAUTH_TOKEN, &new_access_token) {
        log::error(&format!("Blad zapisu nowego tokenu: {}", e));
    }

    // Zapisz nowy refresh token jesli zwrocony
    if let Some(new_refresh) = token_data.get("refresh_token").and_then(|v| v.as_str()) {
        if let Err(e) = secret_set_value(SECRET_REFRESH_TOKEN, new_refresh) {
            log::error(&format!("Blad zapisu nowego refresh tokenu: {}", e));
        }
    }

    Ok(new_access_token)
}

/// Wykonuje request do Microsoft Graph API z autoryzacja Bearer token.
/// Automatycznie probuje odswiezyc token jesli dostanie 401.
fn graph_request(method: &str, endpoint: &str, body: Option<&str>) -> Result<Value, Value> {
    let token = get_oauth_token()?;
    let result = graph_request_with_token(method, endpoint, body, &token);

    // Jesli 401 — probuj odswiezyc token i powtorz request
    if let Err(ref err) = result {
        if let Some(code) = err.get("code").and_then(|v| v.as_str()) {
            if code == "UNAUTHORIZED" {
                log::info("Token wygasl, probuje odswiezyc...");
                let new_token = refresh_oauth_token()?;
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
            // 204 No Content — zwroc pusty obiekt
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
            "error": "Brak autoryzacji (401). Token mogl wygasnac.",
            "code": "UNAUTHORIZED"
        })),
        403 => Err(json!({
            "ok": false,
            "error": "Brak uprawnien (403). Sprawdz uprawnienia Mail.Read / Mail.Send w Azure AD.",
            "code": "FORBIDDEN"
        })),
        404 => Err(json!({
            "ok": false,
            "error": "Zasob nie znaleziony (404). Sprawdz ID wiadomosci lub folderu.",
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

// =============================================================================
// Helpery — pobieranie konfiguracji
// =============================================================================

/// Pobiera domyslna liczbe wynikow z konfiguracji lub zwraca wartosc domyslna.
fn get_max_results() -> u64 {
    match store_get(STORAGE_MAX_RESULTS) {
        Ok(Some(v)) => v.parse::<u64>().unwrap_or(DEFAULT_MAX_RESULTS),
        _ => DEFAULT_MAX_RESULTS,
    }
}

/// Mapuje nazwe folderu na ID folderu Graph API.
/// Obsluguje standardowe nazwy (inbox, sent, drafts, itp.) i dowolne ID folderow.
fn resolve_folder_id(folder: &str) -> String {
    match folder.to_lowercase().as_str() {
        "" | "inbox" => "inbox".to_string(),
        "sent" | "sentitems" | "sent items" => "sentitems".to_string(),
        "drafts" | "draft" => "drafts".to_string(),
        "deleted" | "deleteditems" | "trash" | "kosz" => "deleteditems".to_string(),
        "junk" | "spam" | "junkemail" => "junkemail".to_string(),
        "archive" | "archiwum" => "archive".to_string(),
        // Jesli nie pasuje do znanych nazw — traktuj jako ID folderu
        other => other.to_string(),
    }
}

/// Obcina tekst do podanej dlugosci, dodajac wielokropek.
fn truncate_preview(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        text.to_string()
    } else {
        // Znajdz bezpieczna pozycje obciecia (granica znaku UTF-8)
        let mut end = max_len;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &text[..end])
    }
}

/// Parsuje liste adresow email oddzielonych przecinkami na tablice obiektow Graph API.
fn parse_recipients(addresses: &str) -> Vec<Value> {
    addresses
        .split(',')
        .map(|addr| addr.trim())
        .filter(|addr| !addr.is_empty())
        .map(|addr| {
            json!({
                "emailAddress": {
                    "address": addr
                }
            })
        })
        .collect()
}

// =============================================================================
// Implementacje narzedzi — Lista maili
// =============================================================================

/// Lista maili z podanego folderu. Zwraca uproszczone obiekty z najwazniejszymi polami.
///
/// Parametry:
/// - folder: nazwa folderu (domyslnie inbox)
/// - limit: ile maili pobrac (domyslnie wartosc z konfiguracji)
/// - skip: ile maili pominac (paginacja)
/// - filter: filtr OData (np. "isRead eq false")
fn handle_list_emails(params: &Value) -> Value {
    let folder = params
        .get("folder")
        .and_then(|v| v.as_str())
        .unwrap_or("inbox");

    let folder_id = resolve_folder_id(folder);

    let default_limit = get_max_results();
    let limit = params
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(default_limit)
        .min(50); // Ograniczenie do max 50

    let skip = params
        .get("skip")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let filter = params
        .get("filter")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Zbuduj endpoint z parametrami OData
    let select = "id,subject,from,receivedDateTime,isRead,hasAttachments,bodyPreview";
    let mut endpoint = format!(
        "/me/mailFolders/{}/messages?$top={}&$skip={}&$orderby=receivedDateTime desc&$select={}",
        folder_id, limit, skip, select
    );

    // Dodaj filtr OData jesli podany
    if !filter.is_empty() {
        endpoint.push_str(&format!("&$filter={}", filter));
    }

    match graph_request("GET", &endpoint, None) {
        Ok(response) => {
            let messages = response
                .get("value")
                .cloned()
                .unwrap_or(json!([]));

            // Zmapuj wiadomosci do uproszczonego formatu
            let simplified: Vec<Value> = messages
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .map(|msg| {
                    let from_email = msg
                        .pointer("/from/emailAddress/address")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    let preview_raw = msg
                        .get("bodyPreview")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    json!({
                        "id": msg.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                        "subject": msg.get("subject").and_then(|v| v.as_str()).unwrap_or("(brak tematu)"),
                        "from": from_email,
                        "received_at": msg.get("receivedDateTime")
                            .and_then(|v| v.as_str())
                            .unwrap_or(""),
                        "is_read": msg.get("isRead").and_then(|v| v.as_bool()).unwrap_or(false),
                        "has_attachments": msg.get("hasAttachments")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                        "preview": truncate_preview(preview_raw, PREVIEW_LENGTH)
                    })
                })
                .collect();

            json!({
                "ok": true,
                "data": {
                    "folder": folder_id,
                    "count": simplified.len(),
                    "skip": skip,
                    "emails": simplified
                }
            })
        }
        Err(e) => e,
    }
}

// =============================================================================
// Implementacje narzedzi — Odczyt maila
// =============================================================================

/// Odczyt konkretnego maila z pelna trescia i metadanymi.
/// Pobiera rowniez liste zalacznikow (bez zawartosci — do pobrania osobno).
///
/// Parametry:
/// - message_id: ID wiadomosci (wymagany)
fn handle_read_email(params: &Value) -> Value {
    let message_id = match params.get("message_id").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return json!({"ok": false, "error": "Brak parametru 'message_id'"}),
    };

    // Pobierz pelna wiadomosc z rozwinietymi zalacznikami
    let endpoint = format!(
        "/me/messages/{}?$expand=attachments($select=id,name,size,contentType)&$select=id,subject,from,toRecipients,ccRecipients,receivedDateTime,body,isRead,importance,hasAttachments",
        message_id
    );

    match graph_request("GET", &endpoint, None) {
        Ok(msg) => {
            // Nadawca
            let from = json!({
                "name": msg.pointer("/from/emailAddress/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                "email": msg.pointer("/from/emailAddress/address")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
            });

            // Odbiorcy TO
            let to_recipients: Vec<Value> = msg
                .get("toRecipients")
                .and_then(|v| v.as_array())
                .unwrap_or(&Vec::new())
                .iter()
                .map(|r| {
                    json!({
                        "name": r.pointer("/emailAddress/name")
                            .and_then(|v| v.as_str())
                            .unwrap_or(""),
                        "email": r.pointer("/emailAddress/address")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                    })
                })
                .collect();

            // Odbiorcy CC
            let cc_recipients: Vec<Value> = msg
                .get("ccRecipients")
                .and_then(|v| v.as_array())
                .unwrap_or(&Vec::new())
                .iter()
                .map(|r| {
                    json!({
                        "name": r.pointer("/emailAddress/name")
                            .and_then(|v| v.as_str())
                            .unwrap_or(""),
                        "email": r.pointer("/emailAddress/address")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                    })
                })
                .collect();

            // Tresc wiadomosci — Graph zwraca body.content i body.contentType
            let body_content = msg
                .pointer("/body/content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let body_content_type = msg
                .pointer("/body/contentType")
                .and_then(|v| v.as_str())
                .unwrap_or("text");

            // Rozdziel na body (text) i body_html w zaleznosci od contentType
            let (body_text, body_html) = if body_content_type == "html" {
                ("", body_content)
            } else {
                (body_content, "")
            };

            // Zalaczniki — lista metadanych (bez zawartosci binarnej)
            let attachments: Vec<Value> = msg
                .get("attachments")
                .and_then(|v| v.as_array())
                .unwrap_or(&Vec::new())
                .iter()
                .map(|att| {
                    json!({
                        "id": att.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                        "name": att.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                        "size": att.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
                        "content_type": att.get("contentType")
                            .and_then(|v| v.as_str())
                            .unwrap_or("application/octet-stream")
                    })
                })
                .collect();

            log::info(&format!(
                "Odczytano maila: '{}' od {}",
                msg.get("subject").and_then(|v| v.as_str()).unwrap_or(""),
                msg.pointer("/from/emailAddress/address")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
            ));

            json!({
                "ok": true,
                "data": {
                    "id": msg.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                    "subject": msg.get("subject").and_then(|v| v.as_str()).unwrap_or("(brak tematu)"),
                    "from": from,
                    "to": to_recipients,
                    "cc": cc_recipients,
                    "received_at": msg.get("receivedDateTime")
                        .and_then(|v| v.as_str())
                        .unwrap_or(""),
                    "body": body_text,
                    "body_html": body_html,
                    "is_read": msg.get("isRead").and_then(|v| v.as_bool()).unwrap_or(false),
                    "importance": msg.get("importance")
                        .and_then(|v| v.as_str())
                        .unwrap_or("normal"),
                    "attachments": attachments
                }
            })
        }
        Err(e) => e,
    }
}

// =============================================================================
// Implementacje narzedzi — Wyszukiwanie maili
// =============================================================================

/// Wyszukiwanie maili po tresci, temacie, nadawcy.
/// Uzywa endpointu $search Graph API lub buduje filtr OData.
///
/// Parametry:
/// - query: fraza wyszukiwania (wymagany)
/// - folder: folder do przeszukania (opcjonalny)
/// - from_date: data poczatkowa ISO 8601 (opcjonalny)
/// - has_attachment: filtruj maile z zalacznikami (opcjonalny)
fn handle_search_emails(params: &Value) -> Value {
    let query = match params.get("query").and_then(|v| v.as_str()) {
        Some(v) if !v.is_empty() => v,
        _ => return json!({"ok": false, "error": "Brak parametru 'query' (fraza wyszukiwania)"}),
    };

    let folder = params
        .get("folder")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let from_date = params
        .get("from_date")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let has_attachment = params
        .get("has_attachment")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let default_limit = get_max_results();
    let select = "id,subject,from,receivedDateTime,isRead,hasAttachments,bodyPreview";

    // Zbuduj endpoint wyszukiwania
    let base_path = if folder.is_empty() {
        "/me/messages".to_string()
    } else {
        let folder_id = resolve_folder_id(folder);
        format!("/me/mailFolders/{}/messages", folder_id)
    };

    // Zbuduj parametry filtrowania
    let mut filter_parts: Vec<String> = Vec::new();

    if !from_date.is_empty() {
        filter_parts.push(format!("receivedDateTime ge {}", from_date));
    }

    if has_attachment {
        filter_parts.push("hasAttachments eq true".to_string());
    }

    // Zbuduj pelny endpoint — $search nie jest kompatybilny z $filter,
    // wiec jesli mamy dodatkowe filtry, uzywamy $filter z contains
    let endpoint = if filter_parts.is_empty() {
        // Proste wyszukiwanie — uzyj $search
        format!(
            "{}?$search=\"{}\"&$top={}&$select={}&$orderby=receivedDateTime desc",
            base_path, query, default_limit, select
        )
    } else {
        // Wyszukiwanie z filtrami — dodaj query jako czesc filtra
        filter_parts.insert(
            0,
            format!(
                "(contains(subject,'{}') or contains(from/emailAddress/address,'{}'))",
                query, query
            ),
        );
        let filter = filter_parts.join(" and ");
        format!(
            "{}?$filter={}&$top={}&$select={}&$orderby=receivedDateTime desc",
            base_path, filter, default_limit, select
        )
    };

    match graph_request("GET", &endpoint, None) {
        Ok(response) => {
            let messages = response
                .get("value")
                .cloned()
                .unwrap_or(json!([]));

            let simplified: Vec<Value> = messages
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .map(|msg| {
                    let from_email = msg
                        .pointer("/from/emailAddress/address")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    let preview_raw = msg
                        .get("bodyPreview")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    json!({
                        "id": msg.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                        "subject": msg.get("subject").and_then(|v| v.as_str()).unwrap_or("(brak tematu)"),
                        "from": from_email,
                        "received_at": msg.get("receivedDateTime")
                            .and_then(|v| v.as_str())
                            .unwrap_or(""),
                        "is_read": msg.get("isRead").and_then(|v| v.as_bool()).unwrap_or(false),
                        "has_attachments": msg.get("hasAttachments")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                        "preview": truncate_preview(preview_raw, PREVIEW_LENGTH)
                    })
                })
                .collect();

            log::info(&format!(
                "Wyszukiwanie '{}': znaleziono {} wynikow",
                query,
                simplified.len()
            ));

            json!({
                "ok": true,
                "data": {
                    "query": query,
                    "count": simplified.len(),
                    "emails": simplified
                }
            })
        }
        Err(e) => e,
    }
}

// =============================================================================
// Implementacje narzedzi — Wysylanie maila
// =============================================================================

/// Wysylanie wiadomosci email przez Microsoft Graph API.
/// Uzywa POST /me/sendMail z obiektem message.
///
/// Parametry:
/// - to: adres email odbiorcy (lub lista oddzielona przecinkami) — wymagany
/// - subject: temat wiadomosci — wymagany
/// - body: tresc wiadomosci — wymagany
/// - cc: adresy CC (opcjonalny)
/// - is_html: czy tresc jest HTML (opcjonalny, domyslnie false)
fn handle_send_email(params: &Value) -> Value {
    let to = match params.get("to").and_then(|v| v.as_str()) {
        Some(v) if !v.is_empty() => v,
        _ => return json!({"ok": false, "error": "Brak parametru 'to' (adresat wiadomosci)"}),
    };

    let subject = match params.get("subject").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return json!({"ok": false, "error": "Brak parametru 'subject' (temat wiadomosci)"}),
    };

    let body = match params.get("body").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return json!({"ok": false, "error": "Brak parametru 'body' (tresc wiadomosci)"}),
    };

    let cc = params
        .get("cc")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let is_html = params
        .get("is_html")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let content_type = if is_html { "HTML" } else { "Text" };

    // Zbuduj obiekt wiadomosci
    let to_recipients = parse_recipients(to);

    if to_recipients.is_empty() {
        return json!({"ok": false, "error": "Lista odbiorcow jest pusta"});
    }

    let mut message = json!({
        "subject": subject,
        "body": {
            "contentType": content_type,
            "content": body
        },
        "toRecipients": to_recipients
    });

    // Dodaj CC jesli podano
    if !cc.is_empty() {
        let cc_recipients = parse_recipients(cc);
        if !cc_recipients.is_empty() {
            message["ccRecipients"] = json!(cc_recipients);
        }
    }

    let send_body = json!({
        "message": message,
        "saveToSentItems": true
    });

    let body_str = serde_json::to_string(&send_body).unwrap_or_default();

    match graph_request("POST", "/me/sendMail", Some(&body_str)) {
        Ok(_) => {
            log::info(&format!(
                "Mail wyslany do: {}, temat: '{}'",
                to, subject
            ));

            json!({
                "ok": true,
                "data": {
                    "sent": true,
                    "to": to,
                    "subject": subject
                }
            })
        }
        Err(e) => e,
    }
}

// =============================================================================
// Implementacje narzedzi — Odpowiedz na maila
// =============================================================================

/// Odpowiedz na wiadomosc email. Uzywa POST /me/messages/{id}/reply.
///
/// Parametry:
/// - message_id: ID wiadomosci na ktora odpowiadamy — wymagany
/// - body: tresc odpowiedzi — wymagany
fn handle_reply_email(params: &Value) -> Value {
    let message_id = match params.get("message_id").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return json!({"ok": false, "error": "Brak parametru 'message_id'"}),
    };

    let body = match params.get("body").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return json!({"ok": false, "error": "Brak parametru 'body' (tresc odpowiedzi)"}),
    };

    let endpoint = format!("/me/messages/{}/reply", message_id);

    let reply_body = json!({
        "comment": body
    });

    let body_str = serde_json::to_string(&reply_body).unwrap_or_default();

    match graph_request("POST", &endpoint, Some(&body_str)) {
        Ok(_) => {
            log::info(&format!(
                "Odpowiedz wyslana na wiadomosc: {}",
                message_id
            ));

            json!({
                "ok": true,
                "data": {
                    "replied": true,
                    "message_id": message_id
                }
            })
        }
        Err(e) => e,
    }
}

// =============================================================================
// Implementacje narzedzi — Lista folderow
// =============================================================================

/// Lista folderow poczty uzytkownika. Zwraca foldery z liczba wiadomosci.
/// Pobiera do 100 folderow z informacjami o liczbie wiadomosci i nieprzeczytanych.
fn handle_list_folders(_params: &Value) -> Value {
    let endpoint = "/me/mailFolders?$top=100&$select=id,displayName,totalItemCount,unreadItemCount,childFolderCount";

    match graph_request("GET", endpoint, None) {
        Ok(response) => {
            let folders = response
                .get("value")
                .cloned()
                .unwrap_or(json!([]));

            let simplified: Vec<Value> = folders
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .map(|f| {
                    json!({
                        "id": f.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                        "name": f.get("displayName").and_then(|v| v.as_str()).unwrap_or(""),
                        "total_count": f.get("totalItemCount")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0),
                        "unread_count": f.get("unreadItemCount")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0),
                        "child_folder_count": f.get("childFolderCount")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0)
                    })
                })
                .collect();

            json!({
                "ok": true,
                "data": {
                    "count": simplified.len(),
                    "folders": simplified
                }
            })
        }
        Err(e) => e,
    }
}

// =============================================================================
// Implementacje narzedzi — Zalaczniki
// =============================================================================

/// Pobieranie zalacznika z wiadomosci email.
/// Zwraca metadane zalacznika i zawartosc zakodowana w base64.
///
/// Parametry:
/// - message_id: ID wiadomosci — wymagany
/// - attachment_id: ID zalacznika — wymagany
fn handle_get_attachment(params: &Value) -> Value {
    let message_id = match params.get("message_id").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return json!({"ok": false, "error": "Brak parametru 'message_id'"}),
    };

    let attachment_id = match params.get("attachment_id").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return json!({"ok": false, "error": "Brak parametru 'attachment_id'"}),
    };

    let endpoint = format!(
        "/me/messages/{}/attachments/{}",
        message_id, attachment_id
    );

    match graph_request("GET", &endpoint, None) {
        Ok(att) => {
            log::info(&format!(
                "Pobrano zalacznik '{}' z wiadomosci {}",
                att.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                message_id
            ));

            json!({
                "ok": true,
                "data": {
                    "id": att.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                    "name": att.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                    "content_type": att.get("contentType")
                        .and_then(|v| v.as_str())
                        .unwrap_or("application/octet-stream"),
                    "size": att.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
                    "content_bytes": att.get("contentBytes")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                }
            })
        }
        Err(e) => e,
    }
}

// =============================================================================
// Obsluga eventow — powiadomienia o mailach
// =============================================================================

/// Obsluguje event nowej wiadomosci email.
/// Jesli wlaczone powiadomienia — wysyla notyfikacje do uzytkownika.
fn handle_mail_received(payload: &Value) -> i32 {
    let subject = payload
        .get("subject")
        .and_then(|v| v.as_str())
        .unwrap_or("(brak tematu)");

    let from = payload
        .get("from")
        .and_then(|v| v.as_str())
        .unwrap_or("Nieznany nadawca");

    log::info(&format!(
        "Event: nowy mail od '{}': '{}'",
        from, subject
    ));

    // Sprawdz czy powiadomienia sa wlaczone
    let auto_notifications = match store_get(STORAGE_AUTO_NOTIFICATIONS) {
        Ok(Some(v)) => v == "true",
        _ => false,
    };

    if auto_notifications {
        let notification = json!({
            "title": format!("Nowy mail od {}", from),
            "body": subject,
            "addon_id": "outlook",
            "action": "read_email",
            "data": payload
        });

        if let Err(e) = publish_event("notification.show", notification) {
            log::warn(&format!("Blad wysylania powiadomienia: {}", e));
        }
    }

    0
}

// =============================================================================
// Helpery — zapis odpowiedzi
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
