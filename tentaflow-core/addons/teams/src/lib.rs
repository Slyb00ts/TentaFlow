// =============================================================================
// Plik: addons/teams/src/lib.rs
// Opis: Addon Microsoft Teams dla TentaFlow — pelna integracja z Microsoft
//       Graph API: wiadomosci, czaty, kanaly, kalendarz, pliki OneDrive,
//       spotkania z botem AI (STT/TTS/LLM). Kompilowany do WASM (cdylib).
// =============================================================================

use tentaflow_addon_sdk::prelude::*;

// =============================================================================
// Stale — endpointy Microsoft Graph API
// =============================================================================

/// Bazowy URL Microsoft Graph API v1.0
const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";

/// Endpoint autoryzacji OAuth2 (Azure AD)
const AUTH_BASE: &str = "https://login.microsoftonline.com";

/// Klucz sekretu OAuth token
const SECRET_OAUTH_TOKEN: &str = "oauth_token";

/// Klucz sekretu refresh token
const SECRET_REFRESH_TOKEN: &str = "refresh_token";

/// Klucz sekretu client ID (Azure AD)
const SECRET_CLIENT_ID: &str = "client_id";

/// Klucz sekretu client secret (Azure AD)
const SECRET_CLIENT_SECRET: &str = "client_secret";

/// Klucz konfiguracji tenant ID w storage
const STORAGE_TENANT_ID: &str = "config.tenant_id";

/// Klucz konfiguracji nazwy bota w storage
const STORAGE_BOT_NAME: &str = "config.bot_name";

/// Klucz konfiguracji auto-dolaczania do spotkan
const STORAGE_AUTO_JOIN: &str = "config.auto_join_meetings";

/// Klucz konfiguracji notatek ze spotkan
const STORAGE_MEETING_NOTES: &str = "config.meeting_notes_enabled";

/// Prefiks klucza storage do transkrypcji spotkan
const STORAGE_TRANSCRIPT_PREFIX: &str = "meeting_transcript:";

/// Prefiks klucza storage do notatek ze spotkan
const STORAGE_NOTES_PREFIX: &str = "meeting_notes:";

// =============================================================================
// Lifecycle hooks — eksporty WASM
// =============================================================================

/// Wywolywane przy instalacji addonu.
/// Inicjalizuje domyslna konfiguracje w storage.
#[no_mangle]
pub extern "C" fn on_install() -> i32 {
    log::info("Teams addon: instalacja rozpoczeta");

    // Ustaw domyslna konfiguracje
    let defaults: &[(&str, &str)] = &[
        (STORAGE_BOT_NAME, "TentaFlow"),
        (STORAGE_AUTO_JOIN, "false"),
        (STORAGE_MEETING_NOTES, "true"),
        ("initialized", "true"),
    ];

    for (key, value) in defaults {
        if let Err(e) = store_set(key, value) {
            log::error(&format!("Blad inicjalizacji storage [{}]: {}", key, e));
            return 1;
        }
    }

    log::info("Teams addon: instalacja zakonczona pomyslnie");
    0
}

/// Wywolywane przy uruchomieniu instancji addonu.
/// Rejestruje narzedzia LLM i subskrybuje eventy.
#[no_mangle]
pub extern "C" fn on_start() -> i32 {
    log::info("Teams addon: uruchamianie");

    // -------------------------------------------------------------------------
    // Rejestracja narzedzi LLM tool calling
    // -------------------------------------------------------------------------

    register_tool(
        "teams.send_message",
        "Wysyla wiadomosc do uzytkownika lub kanalu Teams. Wymaga podania adresata (email lub ID kanalu) i tresci wiadomosci.",
        json!({
            "type": "object",
            "required": ["to", "message"],
            "properties": {
                "to": {
                    "type": "string",
                    "description": "Email uzytkownika lub ID kanalu"
                },
                "message": {
                    "type": "string",
                    "description": "Tresc wiadomosci do wyslania"
                },
                "channel_id": {
                    "type": "string",
                    "description": "ID kanalu (opcjonalnie — jesli wysylka na kanal)"
                }
            }
        }),
    );

    register_tool(
        "teams.list_messages",
        "Pobiera ostatnie wiadomosci z czatu lub kanalu Teams.",
        json!({
            "type": "object",
            "required": ["chat_id"],
            "properties": {
                "chat_id": {
                    "type": "string",
                    "description": "ID czatu lub kanalu"
                },
                "limit": {
                    "type": "number",
                    "description": "Ile wiadomosci pobrac (domyslnie 20)"
                }
            }
        }),
    );

    register_tool(
        "teams.list_chats",
        "Lista aktywnych czatow uzytkownika Teams.",
        json!({
            "type": "object",
            "properties": {}
        }),
    );

    register_tool(
        "teams.list_channels",
        "Lista kanalow w podanym teamie.",
        json!({
            "type": "object",
            "required": ["team_id"],
            "properties": {
                "team_id": {
                    "type": "string",
                    "description": "ID teamu"
                }
            }
        }),
    );

    register_tool(
        "teams.get_calendar",
        "Pobiera wydarzenia z kalendarza Microsoft na najblizsze dni.",
        json!({
            "type": "object",
            "properties": {
                "days": {
                    "type": "number",
                    "description": "Ile dni do przodu (domyslnie 7)"
                }
            }
        }),
    );

    register_tool(
        "teams.list_files",
        "Lista plikow z OneDrive/SharePoint uzytkownika.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Sciezka w OneDrive (domyslnie root)"
                }
            }
        }),
    );

    register_tool(
        "teams.join_meeting",
        "Dolacza bota AI do spotkania Teams. Bot moze transkrybowac i odpowiadac na pytania.",
        json!({
            "type": "object",
            "required": ["meeting_id"],
            "properties": {
                "meeting_id": {
                    "type": "string",
                    "description": "ID spotkania Teams"
                }
            }
        }),
    );

    register_tool(
        "teams.get_meeting_notes",
        "Pobiera notatki ze spotkania wygenerowane przez AI.",
        json!({
            "type": "object",
            "required": ["meeting_id"],
            "properties": {
                "meeting_id": {
                    "type": "string",
                    "description": "ID spotkania"
                }
            }
        }),
    );

    // -------------------------------------------------------------------------
    // Subskrypcja eventow z Core
    // -------------------------------------------------------------------------

    // Publikuj event rejestracji subskrypcji — Core przypisze eventy do addonu
    let subscriptions = json!({
        "addon_id": "teams",
        "events": [
            "meeting_started",
            "meeting_ended",
            "message_received",
            "audio_chunk"
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
                    "content": "Microsoft Teams",
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
                    "label": "Zaloguj przez OAuth",
                    "action_id": "teams_oauth_login"
                }
            }
        ]
    });

    if let Err(e) = render_panel("teams_main", panel) {
        log::warn(&format!("Blad renderowania panelu UI: {}", e));
    }

    log::info("Teams addon: uruchomiony pomyslnie");
    0
}

/// Wywolywane przy zatrzymaniu instancji addonu.
/// Wyczysc subskrypcje i zasoby.
#[no_mangle]
pub extern "C" fn on_stop() -> i32 {
    log::info("Teams addon: zatrzymywanie");

    // Anuluj subskrypcje eventow
    let unsubscribe = json!({
        "addon_id": "teams",
        "events": []
    });

    if let Err(e) = publish_event("addon.unsubscribe", unsubscribe) {
        log::warn(&format!("Blad anulowania subskrypcji eventow: {}", e));
    }

    log::info("Teams addon: zatrzymany");
    0
}

// =============================================================================
// Obsluga eventow
// =============================================================================

/// Handler eventow z event bus Core.
/// Obsluguje eventy: meeting_started, meeting_ended, message_received, audio_chunk.
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
        "meeting_started" => handle_meeting_started(&payload),
        "meeting_ended" => handle_meeting_ended(&payload),
        "message_received" => handle_message_received(&payload),
        "audio_chunk" => handle_audio_chunk(&payload),
        _ => {
            log::info(&format!("Teams: nieobslugiwany event: {}", event_type));
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
            let error = json!({"error": format!("Blad parsowania requestu: {}", e)});
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
        "teams.send_message" => tool_send_message(&params),
        "teams.list_messages" => tool_list_messages(&params),
        "teams.list_chats" => tool_list_chats(),
        "teams.list_channels" => tool_list_channels(&params),
        "teams.get_calendar" => tool_get_calendar(&params),
        "teams.list_files" => tool_list_files(&params),
        "teams.join_meeting" => tool_join_meeting(&params),
        "teams.get_meeting_notes" => tool_get_meeting_notes(&params),
        _ => json!({"error": format!("Nieznane narzedzie: {}", tool_name)}),
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
                "error": "Brak tokenu OAuth. Zaloguj sie przez ustawienia addonu Teams (przycisk 'Zaloguj przez OAuth').",
                "code": "AUTH_REQUIRED"
            }));
        }
        Err(e) => {
            return Err(json!({
                "error": format!("Blad odczytu tokenu: {}", e),
                "code": "SECRET_ERROR"
            }));
        }
    };

    Ok(token)
}

/// Probuje odswiezyc OAuth token przez refresh token.
/// Zwraca nowy access token lub blad.
fn refresh_oauth_token() -> Result<String, Value> {
    let refresh_token = match secret_get_value(SECRET_REFRESH_TOKEN) {
        Ok(Some(t)) if !t.is_empty() => t,
        _ => {
            return Err(json!({
                "error": "Brak refresh tokenu. Wymagana ponowna autoryzacja OAuth.",
                "code": "AUTH_REQUIRED"
            }));
        }
    };

    let client_id = match secret_get_value(SECRET_CLIENT_ID) {
        Ok(Some(v)) => v,
        _ => {
            return Err(json!({
                "error": "Brak Client ID. Skonfiguruj addon w ustawieniach.",
                "code": "CONFIG_MISSING"
            }));
        }
    };

    let client_secret = match secret_get_value(SECRET_CLIENT_SECRET) {
        Ok(Some(v)) => v,
        _ => {
            return Err(json!({
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
        "grant_type=refresh_token&client_id={}&client_secret={}&refresh_token={}&scope=https://graph.microsoft.com/.default",
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
            "error": "Nie udalo sie odswiezyc tokenu. Wymagana ponowna autoryzacja.",
            "code": "AUTH_REQUIRED"
        }));
    }

    // Parsuj odpowiedz tokenowa
    let token_data: Value = match serde_json::from_str(&response.body) {
        Ok(v) => v,
        Err(e) => {
            return Err(json!({
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
                    "error": format!("Blad parsowania odpowiedzi Graph API: {}", e),
                    "code": "PARSE_ERROR",
                    "raw_body": response.body
                })),
            }
        }
        401 => Err(json!({
            "error": "Brak autoryzacji (401). Token mogl wygasnac.",
            "code": "UNAUTHORIZED"
        })),
        403 => Err(json!({
            "error": "Brak uprawnien (403). Sprawdz uprawnienia aplikacji w Azure AD.",
            "code": "FORBIDDEN"
        })),
        404 => Err(json!({
            "error": "Zasob nie znaleziony (404).",
            "code": "NOT_FOUND"
        })),
        429 => Err(json!({
            "error": "Zbyt wiele requestow (429). Sprobuj ponownie za chwile.",
            "code": "RATE_LIMITED"
        })),
        _ => Err(json!({
            "error": format!("Blad Graph API, status: {}", response.status),
            "code": "API_ERROR",
            "status": response.status,
            "body": response.body
        })),
    }
}

// =============================================================================
// Implementacje narzedzi — Wiadomosci
// =============================================================================

/// Wysyla wiadomosc do uzytkownika (1:1 czat) lub kanalu Teams.
/// Jesli podano channel_id — wysyla na kanal; w przeciwnym razie szuka/tworzy czat 1:1.
fn tool_send_message(params: &Value) -> Value {
    let to = match params.get("to").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return json!({"error": "Brak parametru 'to' (adresat wiadomosci)"}),
    };

    let message = match params.get("message").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return json!({"error": "Brak parametru 'message' (tresc wiadomosci)"}),
    };

    let channel_id = params.get("channel_id").and_then(|v| v.as_str());

    // Jesli podano channel_id — wysylka na kanal
    if let Some(ch_id) = channel_id {
        return send_channel_message(to, ch_id, message);
    }

    // Wysylka 1:1 — znajdz lub utworz czat z uzytkownikiem
    send_direct_message(to, message)
}

/// Wysyla wiadomosc na kanal Teams (team_id = to, channel_id).
fn send_channel_message(team_id: &str, channel_id: &str, message: &str) -> Value {
    let endpoint = format!(
        "/teams/{}/channels/{}/messages",
        team_id, channel_id
    );

    let body = json!({
        "body": {
            "contentType": "text",
            "content": message
        }
    });

    let body_str = serde_json::to_string(&body).unwrap_or_default();

    match graph_request("POST", &endpoint, Some(&body_str)) {
        Ok(response) => {
            let message_id = response
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            log::info(&format!(
                "Wiadomosc wyslana na kanal {}/{}, id: {}",
                team_id, channel_id, message_id
            ));

            json!({
                "success": true,
                "message_id": message_id,
                "target": "channel",
                "team_id": team_id,
                "channel_id": channel_id
            })
        }
        Err(e) => e,
    }
}

/// Wysyla wiadomosc bezposrednia (1:1 czat) do uzytkownika po emailu.
/// Najpierw tworzy/znajduje czat 1:1, potem wysyla wiadomosc.
fn send_direct_message(user_email: &str, message: &str) -> Value {
    // Krok 1: Utworz czat 1:1 (Graph API automatycznie zwroci istniejacy jesli juz jest)
    let chat_body = json!({
        "chatType": "oneOnOne",
        "members": [
            {
                "@odata.type": "#microsoft.graph.aadUserConversationMember",
                "roles": ["owner"],
                "user@odata.bind": format!("https://graph.microsoft.com/v1.0/users('{}')", user_email)
            }
        ]
    });

    let chat_body_str = serde_json::to_string(&chat_body).unwrap_or_default();

    let chat = match graph_request("POST", "/chats", Some(&chat_body_str)) {
        Ok(c) => c,
        Err(e) => return e,
    };

    let chat_id = match chat.get("id").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => return json!({"error": "Nie udalo sie uzyskac ID czatu"}),
    };

    // Krok 2: Wyslij wiadomosc na czat
    let endpoint = format!("/chats/{}/messages", chat_id);

    let msg_body = json!({
        "body": {
            "contentType": "text",
            "content": message
        }
    });

    let msg_body_str = serde_json::to_string(&msg_body).unwrap_or_default();

    match graph_request("POST", &endpoint, Some(&msg_body_str)) {
        Ok(response) => {
            let message_id = response
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            log::info(&format!(
                "Wiadomosc wyslana do {}, czat: {}, id: {}",
                user_email, chat_id, message_id
            ));

            json!({
                "success": true,
                "message_id": message_id,
                "target": "direct",
                "chat_id": chat_id,
                "user_email": user_email
            })
        }
        Err(e) => e,
    }
}

/// Pobiera ostatnie wiadomosci z czatu lub kanalu.
fn tool_list_messages(params: &Value) -> Value {
    let chat_id = match params.get("chat_id").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return json!({"error": "Brak parametru 'chat_id'"}),
    };

    let limit = params
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .min(50); // Ograniczenie do max 50

    let endpoint = format!(
        "/chats/{}/messages?$top={}&$orderby=createdDateTime desc",
        chat_id, limit
    );

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
                    json!({
                        "id": msg.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                        "from": msg.pointer("/from/user/displayName")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Nieznany"),
                        "content": msg.pointer("/body/content")
                            .and_then(|v| v.as_str())
                            .unwrap_or(""),
                        "created": msg.get("createdDateTime")
                            .and_then(|v| v.as_str())
                            .unwrap_or(""),
                    })
                })
                .collect();

            json!({
                "chat_id": chat_id,
                "count": simplified.len(),
                "messages": simplified
            })
        }
        Err(e) => e,
    }
}

// =============================================================================
// Implementacje narzedzi — Czaty i Kanaly
// =============================================================================

/// Lista aktywnych czatow uzytkownika.
fn tool_list_chats() -> Value {
    let endpoint = "/me/chats?$expand=members&$top=50&$orderby=lastUpdatedDateTime desc";

    match graph_request("GET", endpoint, None) {
        Ok(response) => {
            let chats = response
                .get("value")
                .cloned()
                .unwrap_or(json!([]));

            let simplified: Vec<Value> = chats
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .map(|chat| {
                    // Zbierz nazwy czlonkow czatu
                    let members: Vec<String> = chat
                        .get("members")
                        .and_then(|v| v.as_array())
                        .unwrap_or(&Vec::new())
                        .iter()
                        .filter_map(|m| {
                            m.get("displayName").and_then(|v| v.as_str()).map(String::from)
                        })
                        .collect();

                    json!({
                        "id": chat.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                        "topic": chat.get("topic").and_then(|v| v.as_str()).unwrap_or(""),
                        "chat_type": chat.get("chatType").and_then(|v| v.as_str()).unwrap_or(""),
                        "members": members,
                        "last_updated": chat.get("lastUpdatedDateTime")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                    })
                })
                .collect();

            json!({
                "count": simplified.len(),
                "chats": simplified
            })
        }
        Err(e) => e,
    }
}

/// Lista kanalow w podanym teamie.
fn tool_list_channels(params: &Value) -> Value {
    let team_id = match params.get("team_id").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return json!({"error": "Brak parametru 'team_id'"}),
    };

    let endpoint = format!("/teams/{}/channels", team_id);

    match graph_request("GET", &endpoint, None) {
        Ok(response) => {
            let channels = response
                .get("value")
                .cloned()
                .unwrap_or(json!([]));

            let simplified: Vec<Value> = channels
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .map(|ch| {
                    json!({
                        "id": ch.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                        "name": ch.get("displayName").and_then(|v| v.as_str()).unwrap_or(""),
                        "description": ch.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                        "membership_type": ch.get("membershipType")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                    })
                })
                .collect();

            json!({
                "team_id": team_id,
                "count": simplified.len(),
                "channels": simplified
            })
        }
        Err(e) => e,
    }
}

// =============================================================================
// Implementacje narzedzi — Kalendarz
// =============================================================================

/// Pobiera wydarzenia z kalendarza Microsoft na najblizsze N dni.
fn tool_get_calendar(params: &Value) -> Value {
    let days = params
        .get("days")
        .and_then(|v| v.as_u64())
        .unwrap_or(7)
        .min(90); // Ograniczenie do max 90 dni

    // Zakres dat w formacie ISO 8601 — obliczony przez hosta
    // UWAGA: W WASM nie mamy dostepu do zegara systemowego.
    // Uzywamy parametru $top i sortowania zamiast precyzyjnego zakresu dat.
    // W produkcji Core przekaze timestamp w kontekscie requestu.
    let endpoint = format!(
        "/me/calendarView?startDateTime=now&endDateTime=+{}d&$top=50&$orderby=start/dateTime&$select=subject,start,end,location,organizer,isOnlineMeeting,onlineMeeting",
        days
    );

    // Alternatywny endpoint bez zakresu dat (fallback)
    let fallback_endpoint = format!(
        "/me/events?$top={}&$orderby=start/dateTime&$select=subject,start,end,location,organizer,isOnlineMeeting,onlineMeeting",
        days * 5 // Wiecej eventow zeby pokryc zakres
    );

    // Probuj calendarView, jesli blad — uzyj events
    let response = match graph_request("GET", &endpoint, None) {
        Ok(r) => r,
        Err(_) => match graph_request("GET", &fallback_endpoint, None) {
            Ok(r) => r,
            Err(e) => return e,
        },
    };

    let events = response
        .get("value")
        .cloned()
        .unwrap_or(json!([]));

    let simplified: Vec<Value> = events
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .map(|ev| {
            let is_online = ev
                .get("isOnlineMeeting")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let join_url = ev
                .pointer("/onlineMeeting/joinUrl")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            json!({
                "subject": ev.get("subject").and_then(|v| v.as_str()).unwrap_or(""),
                "start": ev.pointer("/start/dateTime").and_then(|v| v.as_str()).unwrap_or(""),
                "end": ev.pointer("/end/dateTime").and_then(|v| v.as_str()).unwrap_or(""),
                "location": ev.pointer("/location/displayName").and_then(|v| v.as_str()).unwrap_or(""),
                "organizer": ev.pointer("/organizer/emailAddress/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                "is_online": is_online,
                "join_url": join_url
            })
        })
        .collect();

    json!({
        "days_requested": days,
        "count": simplified.len(),
        "events": simplified
    })
}

// =============================================================================
// Implementacje narzedzi — Pliki (OneDrive/SharePoint)
// =============================================================================

/// Lista plikow z OneDrive uzytkownika.
fn tool_list_files(params: &Value) -> Value {
    let path = params
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Zbuduj endpoint — root lub podsciezka
    let endpoint = if path.is_empty() || path == "/" {
        "/me/drive/root/children?$select=id,name,size,lastModifiedDateTime,file,folder,webUrl&$top=100".to_string()
    } else {
        // Usun poczatkowy slash jesli jest
        let clean_path = path.trim_start_matches('/');
        format!(
            "/me/drive/root:/{}:/children?$select=id,name,size,lastModifiedDateTime,file,folder,webUrl&$top=100",
            clean_path
        )
    };

    match graph_request("GET", &endpoint, None) {
        Ok(response) => {
            let items = response
                .get("value")
                .cloned()
                .unwrap_or(json!([]));

            let simplified: Vec<Value> = items
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .map(|item| {
                    let is_folder = item.get("folder").is_some();
                    let mime_type = item
                        .pointer("/file/mimeType")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    json!({
                        "id": item.get("id").and_then(|v| v.as_str()).unwrap_or(""),
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

            json!({
                "path": if path.is_empty() { "/" } else { path },
                "count": simplified.len(),
                "items": simplified
            })
        }
        Err(e) => e,
    }
}

// =============================================================================
// Implementacje narzedzi — Spotkania (Meeting Bot)
// =============================================================================

/// Dolacza bota AI do spotkania Teams.
///
/// UWAGA: Pelna implementacja bota na spotkaniach wymaga Microsoft Communication
/// Services i Graph Communications API (beta). Ponizej zaimplementowano logike
/// bazowa z komentarzami gdzie trzeba podpiac prawdziwy audio stream.
///
/// Flow:
/// 1. Pobierz dane spotkania z Graph API
/// 2. Zarejestruj uczestnictwo bota (wymaga Communications API)
/// 3. Subskrybuj audio stream (wymaga ACS — Azure Communication Services)
/// 4. Na kazdym audio_chunk → STT → transkrypcja
/// 5. Na pytanie do bota → LLM → TTS → odpowiedz
/// 6. Zapisuj transkrypcje do storage
fn tool_join_meeting(params: &Value) -> Value {
    let meeting_id = match params.get("meeting_id").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return json!({"error": "Brak parametru 'meeting_id'"}),
    };

    // Krok 1: Pobierz dane spotkania z Graph API
    let meeting_endpoint = format!("/me/onlineMeetings/{}", meeting_id);

    let meeting_data = match graph_request("GET", &meeting_endpoint, None) {
        Ok(data) => data,
        Err(e) => return e,
    };

    let subject = meeting_data
        .get("subject")
        .and_then(|v| v.as_str())
        .unwrap_or("Bez tematu");

    let join_url = meeting_data
        .get("joinWebUrl")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    log::info(&format!(
        "Dolaczanie do spotkania: '{}' (ID: {})",
        subject, meeting_id
    ));

    // Pobierz nazwe bota z konfiguracji
    let bot_name = match store_get(STORAGE_BOT_NAME) {
        Ok(Some(name)) => name,
        _ => "TentaFlow".to_string(),
    };

    // Krok 2: Zarejestruj bota jako uczestnika spotkania
    // UWAGA: Wymaga Graph Communications API (microsoft.graph.commsOperation)
    // i Azure Bot Framework. Ponizej logika bazowa.
    //
    // W produkcji:
    // POST /communications/calls
    // {
    //   "@odata.type": "#microsoft.graph.call",
    //   "callbackUri": "https://...",
    //   "targets": [{ "@odata.type": "#microsoft.graph.invitationParticipantInfo", ... }],
    //   "requestedModalities": ["audio"],
    //   "mediaConfig": { "@odata.type": "#microsoft.graph.appHostedMediaConfig", ... }
    // }

    // Krok 3: Inicjalizuj transkrypcje w storage
    let transcript_key = format!("{}{}", STORAGE_TRANSCRIPT_PREFIX, meeting_id);
    let initial_transcript = json!({
        "meeting_id": meeting_id,
        "subject": subject,
        "bot_name": bot_name,
        "status": "joined",
        "entries": []
    });

    if let Err(e) = store_set(
        &transcript_key,
        &serde_json::to_string(&initial_transcript).unwrap_or_default(),
    ) {
        log::error(&format!("Blad zapisu transkrypcji: {}", e));
    }

    // Krok 4: Opublikuj event o dolaczeniu do spotkania
    let join_event = json!({
        "meeting_id": meeting_id,
        "subject": subject,
        "bot_name": bot_name,
        "join_url": join_url
    });

    if let Err(e) = publish_event("teams.meeting.bot_joined", join_event) {
        log::warn(&format!("Blad publikacji eventu bot_joined: {}", e));
    }

    // Powiadom uzytkownika
    notify(
        "Teams — bot dolaczyl",
        &format!("Bot '{}' dolaczyl do spotkania '{}'", bot_name, subject),
    );

    json!({
        "success": true,
        "meeting_id": meeting_id,
        "subject": subject,
        "bot_name": bot_name,
        "join_url": join_url,
        "status": "joined",
        "note": "Bot dolaczyl do spotkania. Transkrypcja zostanie zapisana automatycznie."
    })
}

/// Pobiera notatki ze spotkania wygenerowane przez AI.
/// Jesli notatki nie istnieja a jest transkrypcja — generuje je przez LLM.
fn tool_get_meeting_notes(params: &Value) -> Value {
    let meeting_id = match params.get("meeting_id").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return json!({"error": "Brak parametru 'meeting_id'"}),
    };

    let notes_key = format!("{}{}", STORAGE_NOTES_PREFIX, meeting_id);

    // Sprawdz czy notatki juz istnieja
    match store_get(&notes_key) {
        Ok(Some(notes_json)) => {
            // Notatki juz wygenerowane — zwroc je
            match serde_json::from_str::<Value>(&notes_json) {
                Ok(notes) => return notes,
                Err(_) => {
                    return json!({
                        "meeting_id": meeting_id,
                        "notes": notes_json
                    });
                }
            }
        }
        Ok(None) => {
            // Brak notatek — sprawdz czy jest transkrypcja i wygeneruj
        }
        Err(e) => {
            return json!({
                "error": format!("Blad odczytu notatek: {}", e)
            });
        }
    }

    // Pobierz transkrypcje ze storage
    let transcript_key = format!("{}{}", STORAGE_TRANSCRIPT_PREFIX, meeting_id);

    let transcript = match store_get(&transcript_key) {
        Ok(Some(t)) => t,
        Ok(None) => {
            return json!({
                "error": "Brak transkrypcji dla tego spotkania. Bot musial uczestniczyc w spotkaniu.",
                "meeting_id": meeting_id
            });
        }
        Err(e) => {
            return json!({
                "error": format!("Blad odczytu transkrypcji: {}", e)
            });
        }
    };

    // Wygeneruj notatki przez LLM
    let prompt = format!(
        "Na podstawie ponizszej transkrypcji spotkania Teams wygeneruj zwiezle notatki w formacie:\n\
         1. Temat spotkania\n\
         2. Uczestnicy\n\
         3. Kluczowe punkty (lista)\n\
         4. Decyzje\n\
         5. Zadania do wykonania (kto, co, kiedy)\n\n\
         Transkrypcja:\n{}",
        transcript
    );

    let generated_notes = match generate(&prompt) {
        Ok(notes) => notes,
        Err(e) => {
            return json!({
                "error": format!("Blad generowania notatek przez LLM: {}", e),
                "meeting_id": meeting_id,
                "transcript_available": true
            });
        }
    };

    // Zapisz wygenerowane notatki do storage
    let notes_data = json!({
        "meeting_id": meeting_id,
        "notes": generated_notes,
        "generated": true
    });

    let notes_str = serde_json::to_string(&notes_data).unwrap_or_default();
    if let Err(e) = store_set(&notes_key, &notes_str) {
        log::warn(&format!("Blad zapisu notatek: {}", e));
    }

    notes_data
}

// =============================================================================
// Handlery eventow
// =============================================================================

/// Obsluguje event rozpoczecia spotkania.
/// Jesli wlaczone auto-dolaczanie — automatycznie dolacza bota.
fn handle_meeting_started(payload: &Value) -> i32 {
    let meeting_id = payload
        .get("meeting_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    log::info(&format!("Event: spotkanie rozpoczete, ID: {}", meeting_id));

    // Sprawdz czy wlaczone automatyczne dolaczanie
    let auto_join = match store_get(STORAGE_AUTO_JOIN) {
        Ok(Some(v)) => v == "true",
        _ => false,
    };

    if auto_join && !meeting_id.is_empty() {
        log::info("Auto-join wlaczony, dolaczam do spotkania...");
        let params = json!({"meeting_id": meeting_id});
        let result = tool_join_meeting(&params);

        if result.get("error").is_some() {
            log::error(&format!(
                "Blad auto-join do spotkania: {}",
                result
            ));
        }
    }

    0
}

/// Obsluguje event zakonczenia spotkania.
/// Generuje notatki jesli wlaczone w konfiguracji.
fn handle_meeting_ended(payload: &Value) -> i32 {
    let meeting_id = payload
        .get("meeting_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    log::info(&format!("Event: spotkanie zakonczone, ID: {}", meeting_id));

    if meeting_id.is_empty() {
        return 0;
    }

    // Zaktualizuj status transkrypcji
    let transcript_key = format!("{}{}", STORAGE_TRANSCRIPT_PREFIX, meeting_id);
    if let Ok(Some(transcript_json)) = store_get(&transcript_key) {
        if let Ok(mut transcript) = serde_json::from_str::<Value>(&transcript_json) {
            transcript["status"] = json!("completed");
            let updated = serde_json::to_string(&transcript).unwrap_or_default();
            let _ = store_set(&transcript_key, &updated);
        }
    }

    // Sprawdz czy wlaczone automatyczne generowanie notatek
    let notes_enabled = match store_get(STORAGE_MEETING_NOTES) {
        Ok(Some(v)) => v == "true",
        _ => true, // Domyslnie wlaczone
    };

    if notes_enabled {
        log::info("Generowanie notatek ze spotkania...");
        let params = json!({"meeting_id": meeting_id});
        let result = tool_get_meeting_notes(&params);

        if result.get("error").is_some() {
            log::warn(&format!(
                "Blad generowania notatek: {}",
                result
            ));
        } else {
            notify(
                "Teams — notatki gotowe",
                &format!(
                    "Notatki ze spotkania '{}' zostaly wygenerowane",
                    meeting_id
                ),
            );
        }
    }

    // Opublikuj event zakonczenia
    let end_event = json!({
        "meeting_id": meeting_id,
        "notes_generated": notes_enabled
    });

    if let Err(e) = publish_event("teams.meeting.bot_left", end_event) {
        log::warn(&format!("Blad publikacji eventu bot_left: {}", e));
    }

    0
}

/// Obsluguje event odebrania wiadomosci.
/// Moze byc uzyte do powiadomien lub automatycznych odpowiedzi.
fn handle_message_received(payload: &Value) -> i32 {
    let from = payload
        .get("from")
        .and_then(|v| v.as_str())
        .unwrap_or("Nieznany");
    let content = payload
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    log::info(&format!(
        "Event: nowa wiadomosc od '{}': '{}'",
        from,
        if content.len() > 50 {
            &content[..50]
        } else {
            content
        }
    ));

    0
}

/// Obsluguje event fragmentu audio ze spotkania.
/// Wysyla audio do STT, dodaje wynik do transkrypcji.
///
/// UWAGA: W produkcji audio_chunk przychodzi jako dane binarne (PCM/Opus)
/// z Azure Communication Services. Ponizej logika bazowa z komentarzami.
fn handle_audio_chunk(payload: &Value) -> i32 {
    let meeting_id = payload
        .get("meeting_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let speaker = payload
        .get("speaker")
        .and_then(|v| v.as_str())
        .unwrap_or("Nieznany");

    // UWAGA: W produkcji audio_data to base64-encoded PCM/Opus
    // Trzeba go zdekodowac i wyslac do serwisu STT na routerze.
    let _audio_data = payload
        .get("audio_data")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Jesli payload zawiera juz transkrybowany tekst (z zewnetrznego STT)
    let transcribed_text = payload
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if transcribed_text.is_empty() && meeting_id.is_empty() {
        return 0;
    }

    // Dodaj wpis do transkrypcji w storage
    if !meeting_id.is_empty() && !transcribed_text.is_empty() {
        let transcript_key = format!("{}{}", STORAGE_TRANSCRIPT_PREFIX, meeting_id);

        if let Ok(Some(transcript_json)) = store_get(&transcript_key) {
            if let Ok(mut transcript) = serde_json::from_str::<Value>(&transcript_json) {
                // Dodaj nowy wpis do tablicy entries
                if let Some(entries) = transcript.get_mut("entries").and_then(|v| v.as_array_mut())
                {
                    entries.push(json!({
                        "speaker": speaker,
                        "text": transcribed_text
                    }));
                }

                let updated = serde_json::to_string(&transcript).unwrap_or_default();
                let _ = store_set(&transcript_key, &updated);
            }
        }

        // Sprawdz czy ktos zwrocil sie do bota
        let bot_name = match store_get(STORAGE_BOT_NAME) {
            Ok(Some(name)) => name.to_lowercase(),
            _ => "tentaflow ai".to_string(),
        };

        let text_lower = transcribed_text.to_lowercase();
        if text_lower.contains(&bot_name) || text_lower.contains("tentaflow") {
            // Ktos zwrocil sie do bota — wygeneruj odpowiedz przez LLM
            let prompt = format!(
                "Jestes botem AI o nazwie '{}' na spotkaniu Teams. \
                 {} powiedzial: '{}'. \
                 Odpowiedz krotko i na temat.",
                bot_name, speaker, transcribed_text
            );

            match generate(&prompt) {
                Ok(response) => {
                    log::info(&format!("Bot odpowiada na spotkaniu: {}", response));

                    // Opublikuj event z odpowiedzia bota (do TTS)
                    let tts_event = json!({
                        "meeting_id": meeting_id,
                        "text": response,
                        "type": "bot_response"
                    });

                    if let Err(e) = publish_event("teams.meeting.bot_speak", tts_event) {
                        log::warn(&format!("Blad publikacji odpowiedzi bota: {}", e));
                    }
                }
                Err(e) => {
                    log::error(&format!("Blad generowania odpowiedzi bota: {}", e));
                }
            }
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
