// =============================================================================
// Plik: addons/mcp-core/src/lib.rs
// Opis: Wspolny klient MCP (Model Context Protocol) dla addonow `mcp` oraz
//       `ibm-mcp`. Transport: Streamable HTTP (JSON-RPC 2.0 przez POST,
//       odpowiedzi JSON albo SSE). Konfiguracja serwerow w KV store addonu,
//       tokeny w secrets. Zdalne narzedzia MCP moga byc rejestrowane
//       dynamicznie jako narzedzia LLM przez tool_register.
// =============================================================================

use std::collections::HashMap;

use tentaflow_addon_sdk::prelude::*;

/// Branding addonu-hosta: prefiks nazw narzedzi i nazwa klienta wysylana
/// w MCP initialize. `mcp` i `ibm-mcp` dziela cala logike, roznia sie tylko
/// tymi wartosciami (osobne addon_id daje im osobny KV store i secrets).
pub struct Brand {
    pub tool_prefix: &'static str,
    pub client_name: &'static str,
}

const PROTOCOL_VERSION: &str = "2025-06-18";
const CLIENT_VERSION: &str = "1.0.0";
const SERVERS_KEY: &str = "servers";
const DYN_TOOLS_KEY: &str = "dynamic_tools";
const MAX_TOOL_PAGES: usize = 10;
const MAX_TOOL_NAME_LEN: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ServerConfig {
    url: String,
    /// Naglowek auth (domyslnie "Authorization"; token bez schematu dostaje
    /// prefiks "Bearer " tylko dla tego naglowka).
    #[serde(default = "default_auth_header")]
    auth_header: String,
}

fn default_auth_header() -> String {
    "Authorization".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DynamicTool {
    server: String,
    tool: String,
}

// =============================================================================
// Entrypoint glue — wolane przez cienkie addony z ich extern "C" on_request
// =============================================================================

pub fn on_request_raw(
    brand: &Brand,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let input_json = read_string(input_ptr, input_len);
    let response = match serde_json::from_str::<Value>(&input_json) {
        Ok(request) => {
            let tool_name = request
                .get("tool")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
            handle_request(brand, &tool_name, &params)
        }
        Err(e) => error(format!("Niepoprawny request JSON: {}", e)),
    };
    write_response(out_ptr, out_cap, out_len_ptr, &response)
}

pub fn handle_request(brand: &Brand, tool_name: &str, params: &Value) -> Value {
    let action = tool_name
        .strip_prefix(brand.tool_prefix)
        .and_then(|rest| rest.strip_prefix('_'));
    let result = match action {
        Some("add_server") => handle_add_server(brand, params),
        Some("remove_server") => handle_remove_server(params),
        Some("list_servers") => handle_list_servers(),
        Some("list_tools") => handle_list_tools(brand, params),
        Some("call_tool") => handle_call_tool(brand, params),
        _ => handle_dynamic_tool(brand, tool_name, params),
    };
    match result {
        Ok(v) => v,
        Err(e) => error(e),
    }
}

// =============================================================================
// Handlery narzedzi statycznych
// =============================================================================

fn handle_add_server(brand: &Brand, params: &Value) -> Result<Value, String> {
    let name = read_server_name(params, "name")?;
    let url = params
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .ok_or("Parametr url jest wymagany")?
        .to_string();
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err("URL serwera MCP musi zaczynac sie od http:// albo https://".into());
    }

    let config = ServerConfig {
        url,
        auth_header: params
            .get("auth_header")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .map(str::to_string)
            .unwrap_or_else(default_auth_header),
    };
    let token = params
        .get("auth_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string);

    // Najpierw weryfikacja handshakem — zapis dopiero po udanym initialize,
    // zeby nie zostawiac martwych wpisow po literowce w URL/tokenie.
    let (session, server_info) = initialize(brand, &config, token.as_deref())?;

    let mut servers = load_servers()?;
    servers.insert(name.clone(), config.clone());
    save_servers(&servers)?;
    if let Some(token) = &token {
        secret_set_value(&token_key(&name), token)
            .map_err(|e| format!("Zapis tokenu nie powiodl sie: {}", e))?;
    }

    let register = params
        .get("register_tools")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let mut registered: Vec<String> = Vec::new();
    let mut tools_count = 0usize;
    if register {
        let tools = list_tools_remote(&config, token.as_deref(), session.as_deref())?;
        tools_count = tools.len();
        registered = register_remote_tools(brand, &name, &tools)?;
    }

    Ok(json!({
        "ok": true,
        "server": name,
        "server_info": server_info,
        "tools_count": tools_count,
        "registered_tools": registered,
    }))
}

fn handle_remove_server(params: &Value) -> Result<Value, String> {
    let name = read_server_name(params, "name")?;
    let mut servers = load_servers()?;
    if servers.remove(&name).is_none() {
        return Err(format!("Serwer '{}' nie istnieje", name));
    }
    save_servers(&servers)?;
    // SDK nie ma usuwania sekretow — nadpisujemy pustym stringiem.
    let _ = secret_set_value(&token_key(&name), "");

    // Usun mapowania dynamicznych narzedzi tego serwera. Wpisy w globalnej
    // tabeli narzedzi LLM znikaja dopiero przy reinstalacji addonu — do tego
    // czasu wywolanie zwroci czytelny blad "serwer nie istnieje".
    let mut dyn_tools = load_dynamic_tools()?;
    let before = dyn_tools.len();
    dyn_tools.retain(|_, target| target.server != name);
    let removed_tools = before - dyn_tools.len();
    save_dynamic_tools(&dyn_tools)?;

    Ok(json!({
        "ok": true,
        "removed": name,
        "removed_tool_mappings": removed_tools,
    }))
}

fn handle_list_servers() -> Result<Value, String> {
    let servers = load_servers()?;
    let list: Vec<Value> = servers
        .iter()
        .map(|(name, cfg)| {
            let has_token = secret_get_value(&token_key(name))
                .ok()
                .flatten()
                .map(|t| !t.is_empty())
                .unwrap_or(false);
            json!({
                "name": name,
                "url": cfg.url,
                "auth_header": cfg.auth_header,
                "has_token": has_token,
            })
        })
        .collect();
    Ok(json!({"ok": true, "servers": list}))
}

fn handle_list_tools(brand: &Brand, params: &Value) -> Result<Value, String> {
    let (name, config) = resolve_server(params)?;
    let token = load_token(&name);
    let (session, _) = initialize(brand, &config, token.as_deref())?;
    let tools = list_tools_remote(&config, token.as_deref(), session.as_deref())?;

    let register = params
        .get("register_tools")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let registered = if register {
        register_remote_tools(brand, &name, &tools)?
    } else {
        Vec::new()
    };

    Ok(json!({
        "ok": true,
        "server": name,
        "tools": tools,
        "registered_tools": registered,
    }))
}

fn handle_call_tool(brand: &Brand, params: &Value) -> Result<Value, String> {
    let tool = params
        .get("tool")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or("Parametr tool jest wymagany")?
        .to_string();

    // Akceptuj zarowno surowa nazwe narzedzia MCP, jak i zarejestrowana
    // dynamiczna nazwe LLM (mapowanie wskazuje wtedy serwer i nazwe zdalna).
    let dyn_tools = load_dynamic_tools()?;
    let (name, config, remote_tool) = if let Some(target) = dyn_tools.get(&tool) {
        let servers = load_servers()?;
        let config = servers
            .get(&target.server)
            .cloned()
            .ok_or_else(|| format!("Serwer '{}' nie istnieje", target.server))?;
        (target.server.clone(), config, target.tool.clone())
    } else {
        let (name, config) = resolve_server(params)?;
        (name, config, tool)
    };

    let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    let token = load_token(&name);
    let result = call_tool_remote(brand, &config, token.as_deref(), &remote_tool, &arguments)?;
    Ok(json!({
        "ok": true,
        "server": name,
        "tool": remote_tool,
        "result": result,
    }))
}

/// Dispatch dynamicznie zarejestrowanego narzedzia — LLM wola je pelna nazwa,
/// a parametry sa argumentami narzedzia MCP wprost.
fn handle_dynamic_tool(brand: &Brand, tool_name: &str, params: &Value) -> Result<Value, String> {
    let dyn_tools = load_dynamic_tools()?;
    let target = dyn_tools
        .get(tool_name)
        .ok_or_else(|| format!("Nieznane narzedzie: {}", tool_name))?;
    let servers = load_servers()?;
    let config = servers
        .get(&target.server)
        .ok_or_else(|| format!("Serwer '{}' nie istnieje", target.server))?;
    let token = load_token(&target.server);
    let result = call_tool_remote(brand, config, token.as_deref(), &target.tool, params)?;
    Ok(json!({
        "ok": true,
        "server": target.server,
        "tool": target.tool,
        "result": result,
    }))
}

// =============================================================================
// Klient MCP — JSON-RPC 2.0 przez Streamable HTTP
// =============================================================================

fn initialize(
    brand: &Brand,
    config: &ServerConfig,
    token: Option<&str>,
) -> Result<(Option<String>, Value), String> {
    let payload = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": brand.client_name, "version": CLIENT_VERSION},
        }
    });
    let response = rpc_post(config, token, None, &payload)?;
    if response.status < 200 || response.status >= 300 {
        return Err(format!(
            "MCP initialize: serwer zwrocil HTTP {} ({})",
            response.status,
            truncate(&response.body, 300)
        ));
    }
    let session = header_value(&response.headers, "mcp-session-id");
    let message = parse_rpc_body(&response.body)?;
    let result = unwrap_rpc_result(&message, "initialize")?;

    // Spec wymaga notyfikacji initialized przed normalnymi requestami.
    // Brak odpowiedzi/4xx tolerujemy — czesc serwerow stateless ja ignoruje.
    let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    let _ = rpc_post(config, token, session.as_deref(), &initialized);

    Ok((session, result))
}

fn list_tools_remote(
    config: &ServerConfig,
    token: Option<&str>,
    session: Option<&str>,
) -> Result<Vec<Value>, String> {
    let mut tools: Vec<Value> = Vec::new();
    let mut cursor: Option<String> = None;
    for page in 0..MAX_TOOL_PAGES {
        let mut params = json!({});
        if let Some(cursor) = &cursor {
            params = json!({"cursor": cursor});
        }
        let payload = json!({
            "jsonrpc": "2.0",
            "id": page as i64 + 2,
            "method": "tools/list",
            "params": params,
        });
        let response = rpc_post(config, token, session, &payload)?;
        if response.status < 200 || response.status >= 300 {
            return Err(format!(
                "MCP tools/list: serwer zwrocil HTTP {} ({})",
                response.status,
                truncate(&response.body, 300)
            ));
        }
        let message = parse_rpc_body(&response.body)?;
        let result = unwrap_rpc_result(&message, "tools/list")?;
        if let Some(page_tools) = result.get("tools").and_then(Value::as_array) {
            tools.extend(page_tools.iter().cloned());
        }
        cursor = result
            .get("nextCursor")
            .and_then(Value::as_str)
            .filter(|c| !c.is_empty())
            .map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    Ok(tools)
}

fn call_tool_remote(
    brand: &Brand,
    config: &ServerConfig,
    token: Option<&str>,
    tool: &str,
    arguments: &Value,
) -> Result<Value, String> {
    let (session, _) = initialize(brand, config, token)?;
    let payload = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {"name": tool, "arguments": arguments},
    });
    let response = rpc_post(config, token, session.as_deref(), &payload)?;
    if response.status < 200 || response.status >= 300 {
        return Err(format!(
            "MCP tools/call: serwer zwrocil HTTP {} ({})",
            response.status,
            truncate(&response.body, 300)
        ));
    }
    let message = parse_rpc_body(&response.body)?;
    unwrap_rpc_result(&message, "tools/call")
}

fn rpc_post(
    config: &ServerConfig,
    token: Option<&str>,
    session: Option<&str>,
    payload: &Value,
) -> Result<HttpResponse, String> {
    let mut headers = HashMap::new();
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    headers.insert(
        "Accept".to_string(),
        "application/json, text/event-stream".to_string(),
    );
    headers.insert(
        "MCP-Protocol-Version".to_string(),
        PROTOCOL_VERSION.to_string(),
    );
    if let Some(token) = token {
        headers.insert(config.auth_header.clone(), auth_value(&config.auth_header, token));
    }
    if let Some(session) = session {
        headers.insert("Mcp-Session-Id".to_string(), session.to_string());
    }
    http_send(&HttpRequest {
        method: "POST".to_string(),
        url: config.url.clone(),
        headers,
        body: Some(payload.to_string()),
    })
}

/// Tokeny bez schematu dostaja "Bearer " tylko dla naglowka Authorization;
/// custom naglowki (np. X-API-Key) ida doslownie.
fn auth_value(header: &str, token: &str) -> String {
    let is_authorization = header.eq_ignore_ascii_case("authorization");
    let has_scheme = token.contains(' ');
    if is_authorization && !has_scheme {
        format!("Bearer {}", token)
    } else {
        token.to_string()
    }
}

/// Streamable HTTP zwraca albo czysty JSON, albo strumien SSE — wtedy
/// odpowiedz JSON-RPC siedzi w liniach `data:`.
fn parse_rpc_body(body: &str) -> Result<Value, String> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err("MCP: pusta odpowiedz serwera".into());
    }
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return serde_json::from_str(trimmed)
            .map_err(|e| format!("MCP: niepoprawny JSON w odpowiedzi: {}", e));
    }
    let mut last_message: Option<Value> = None;
    for line in trimmed.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(data.trim()) else {
            continue;
        };
        if value.get("jsonrpc").is_some() && (value.get("result").is_some() || value.get("error").is_some()) {
            last_message = Some(value);
        }
    }
    last_message.ok_or_else(|| {
        format!(
            "MCP: brak odpowiedzi JSON-RPC w strumieniu SSE ({})",
            truncate(body, 200)
        )
    })
}

fn unwrap_rpc_result(message: &Value, method: &str) -> Result<Value, String> {
    if let Some(err) = message.get("error") {
        let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
        let msg = err.get("message").and_then(Value::as_str).unwrap_or("");
        return Err(format!("MCP {}: blad serwera {} {}", method, code, msg));
    }
    message
        .get("result")
        .cloned()
        .ok_or_else(|| format!("MCP {}: odpowiedz bez pola result", method))
}

fn header_value(headers: &HashMap<String, String>, name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
        .filter(|v| !v.is_empty())
}

// =============================================================================
// Dynamiczna rejestracja zdalnych narzedzi jako narzedzia LLM
// =============================================================================

fn register_remote_tools(
    brand: &Brand,
    server_name: &str,
    tools: &[Value],
) -> Result<Vec<String>, String> {
    let mut dyn_tools = load_dynamic_tools()?;
    let mut registered = Vec::with_capacity(tools.len());
    for tool in tools {
        let Some(remote_name) = tool.get("name").and_then(Value::as_str) else {
            continue;
        };
        let description = tool
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("");
        let schema = tool
            .get("inputSchema")
            .cloned()
            .unwrap_or_else(|| json!({"type": "object", "properties": {}}));

        let target = DynamicTool {
            server: server_name.to_string(),
            tool: remote_name.to_string(),
        };
        let local_name = unique_tool_name(brand, server_name, remote_name, &target, &dyn_tools);
        register_tool(
            &local_name,
            &format!("[MCP {}] {}", server_name, description),
            schema,
        );
        dyn_tools.insert(local_name.clone(), target);
        registered.push(local_name);
    }
    save_dynamic_tools(&dyn_tools)?;
    Ok(registered)
}

fn unique_tool_name(
    brand: &Brand,
    server_name: &str,
    remote_name: &str,
    target: &DynamicTool,
    existing: &HashMap<String, DynamicTool>,
) -> String {
    let base = format!(
        "{}_{}_{}",
        brand.tool_prefix,
        sanitize(server_name),
        sanitize(remote_name)
    );
    let mut base: String = base.chars().take(MAX_TOOL_NAME_LEN).collect();
    while base.ends_with('_') {
        base.pop();
    }
    let mut candidate = base.clone();
    let mut suffix = 2usize;
    while let Some(taken) = existing.get(&candidate) {
        if taken.server == target.server && taken.tool == target.tool {
            break;
        }
        candidate = format!("{}_{}", base, suffix);
        suffix += 1;
    }
    candidate
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect()
}

// =============================================================================
// Konfiguracja serwerow — KV store + secrets
// =============================================================================

fn load_servers() -> Result<HashMap<String, ServerConfig>, String> {
    match store_get(SERVERS_KEY)? {
        Some(raw) => serde_json::from_str(&raw)
            .map_err(|e| format!("Uszkodzona konfiguracja serwerow: {}", e)),
        None => Ok(HashMap::new()),
    }
}

fn save_servers(servers: &HashMap<String, ServerConfig>) -> Result<(), String> {
    let raw = serde_json::to_string(servers)
        .map_err(|e| format!("Serializacja konfiguracji serwerow: {}", e))?;
    store_set(SERVERS_KEY, &raw)
}

fn load_dynamic_tools() -> Result<HashMap<String, DynamicTool>, String> {
    match store_get(DYN_TOOLS_KEY)? {
        Some(raw) => serde_json::from_str(&raw)
            .map_err(|e| format!("Uszkodzona mapa dynamicznych narzedzi: {}", e)),
        None => Ok(HashMap::new()),
    }
}

fn save_dynamic_tools(tools: &HashMap<String, DynamicTool>) -> Result<(), String> {
    let raw = serde_json::to_string(tools)
        .map_err(|e| format!("Serializacja mapy dynamicznych narzedzi: {}", e))?;
    store_set(DYN_TOOLS_KEY, &raw)
}

fn load_token(server_name: &str) -> Option<String> {
    secret_get_value(&token_key(server_name))
        .ok()
        .flatten()
        .filter(|t| !t.is_empty())
}

fn token_key(server_name: &str) -> String {
    format!("{}_auth_token", server_name)
}

fn resolve_server(params: &Value) -> Result<(String, ServerConfig), String> {
    let servers = load_servers()?;
    if let Some(name) = params
        .get("server")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        let config = servers
            .get(name)
            .cloned()
            .ok_or_else(|| format!("Serwer '{}' nie istnieje", name))?;
        return Ok((name.to_string(), config));
    }
    match servers.len() {
        0 => Err("Brak skonfigurowanych serwerow MCP — uzyj add_server".into()),
        1 => {
            let (name, config) = servers.into_iter().next().expect("len==1");
            Ok((name, config))
        }
        _ => {
            let mut names: Vec<&String> = servers.keys().collect();
            names.sort();
            Err(format!(
                "Wiele serwerow MCP — podaj parametr server. Dostepne: {}",
                names
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }
    }
}

fn read_server_name(params: &Value, key: &str) -> Result<String, String> {
    let name = params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .ok_or("Parametr name jest wymagany")?
        .to_lowercase();
    if name.len() > 32
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err("Nazwa serwera: max 32 znaki [a-z0-9_-]".into());
    }
    Ok(name)
}

// =============================================================================
// Pomocnicze
// =============================================================================

fn truncate(value: &str, max: usize) -> String {
    if value.len() <= max {
        value.to_string()
    } else {
        let cut: String = value.chars().take(max).collect();
        format!("{}…", cut)
    }
}

fn error(message: impl Into<String>) -> Value {
    json!({"ok": false, "error": message.into()})
}

pub fn write_response(out_ptr: i32, out_cap: i32, out_len_ptr: i32, value: &Value) -> i32 {
    let response_str = match serde_json::to_string(value) {
        Ok(s) => s,
        Err(_) => return 1,
    };
    let written = write_string(out_ptr, out_cap, out_len_ptr, &response_str);
    if written < 0 {
        log::error("Bufor wyjsciowy za maly na odpowiedz MCP");
        return ABI_OUTPUT_BUFFER_TOO_SMALL;
    }
    let len_bytes = written.to_le_bytes();
    let dest = unsafe { std::slice::from_raw_parts_mut(out_len_ptr as *mut u8, 4) };
    dest.copy_from_slice(&len_bytes);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_lowercases_and_replaces_specials() {
        assert_eq!(sanitize("My-Server.v2"), "my_server_v2");
    }

    #[test]
    fn auth_value_adds_bearer_only_for_authorization() {
        assert_eq!(auth_value("Authorization", "abc"), "Bearer abc");
        assert_eq!(auth_value("Authorization", "Basic abc"), "Basic abc");
        assert_eq!(auth_value("X-API-Key", "abc"), "abc");
    }

    #[test]
    fn parse_rpc_body_reads_plain_json() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#;
        let message = parse_rpc_body(body).expect("json body");
        assert!(message.get("result").is_some());
    }

    #[test]
    fn parse_rpc_body_reads_sse_data_lines() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n\n";
        let message = parse_rpc_body(body).expect("sse body");
        assert_eq!(
            message.pointer("/result/ok").and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn unwrap_rpc_result_maps_error_object() {
        let message = json!({"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"no method"}});
        let err = unwrap_rpc_result(&message, "tools/list").unwrap_err();
        assert!(err.contains("-32601"));
        assert!(err.contains("no method"));
    }

    #[test]
    fn unique_tool_name_appends_suffix_on_collision() {
        let brand = Brand {
            tool_prefix: "mcp",
            client_name: "t",
        };
        let mut existing = HashMap::new();
        existing.insert(
            "mcp_srv_search".to_string(),
            DynamicTool {
                server: "other".to_string(),
                tool: "search".to_string(),
            },
        );
        let target = DynamicTool {
            server: "srv".to_string(),
            tool: "search".to_string(),
        };
        assert_eq!(
            unique_tool_name(&brand, "srv", "search", &target, &existing),
            "mcp_srv_search_2"
        );
    }

    #[test]
    fn unique_tool_name_is_stable_for_same_target() {
        let brand = Brand {
            tool_prefix: "mcp",
            client_name: "t",
        };
        let target = DynamicTool {
            server: "srv".to_string(),
            tool: "search".to_string(),
        };
        let mut existing = HashMap::new();
        existing.insert("mcp_srv_search".to_string(), target.clone());
        assert_eq!(
            unique_tool_name(&brand, "srv", "search", &target, &existing),
            "mcp_srv_search"
        );
    }

    #[test]
    fn read_server_name_validates_charset() {
        assert_eq!(
            read_server_name(&json!({"name": "IBM-Prod"}), "name").unwrap(),
            "ibm-prod"
        );
        assert!(read_server_name(&json!({"name": "zle imie"}), "name").is_err());
        assert!(read_server_name(&json!({}), "name").is_err());
    }
}
