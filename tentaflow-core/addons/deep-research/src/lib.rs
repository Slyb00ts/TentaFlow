// =============================================================================
// Plik: addons/deep-research/src/lib.rs
// Opis: Addon WASM exposing Core web research SDK operations as LLM tools and
//       Flow Builder blocks.
// =============================================================================

use tentaflow_addon_sdk::prelude::*;

#[no_mangle]
pub extern "C" fn on_install() -> i32 {
    log::info("deep-research addon zainstalowany");
    0
}

#[no_mangle]
pub extern "C" fn on_start() -> i32 {
    log::info("deep-research addon uruchomiony");
    0
}

#[no_mangle]
pub extern "C" fn on_stop() -> i32 {
    log::info("deep-research addon zatrzymany");
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
    let input_json = read_string(input_ptr, input_len);
    let request: Value = match serde_json::from_str(&input_json) {
        Ok(v) => v,
        Err(e) => {
            return write_response(
                out_ptr,
                out_cap,
                out_len_ptr,
                &json!({"ok": false, "error": format!("Niepoprawny request JSON: {}", e)}),
            );
        }
    };

    let tool_name = request.get("tool").and_then(Value::as_str).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    let result = if let Some(block_type) = tool_name.strip_prefix("block.") {
        handle_flow_block(block_type, &params)
    } else {
        dispatch_tool(tool_name, &params)
    };

    write_response(out_ptr, out_cap, out_len_ptr, &result)
}

fn handle_flow_block(block_type: &str, params: &Value) -> Value {
    let payload = params
        .get("payload")
        .cloned()
        .unwrap_or_else(|| params.clone());
    let result = dispatch_tool(block_type, &payload);

    if params.get("payload").is_some() {
        let mut response = params.clone();
        if let Some(obj) = response.as_object_mut() {
            obj.insert("payload".to_string(), result);
        }
        response
    } else {
        result
    }
}

fn dispatch_tool(tool_name: &str, params: &Value) -> Value {
    match tool_name {
        "search_web" => handle_search_web(params),
        "fetch_url" => handle_fetch_url(params),
        "read_search_results" => handle_read_search_results(params),
        _ => json!({"ok": false, "error": format!("Nieznane narzedzie: {}", tool_name)}),
    }
}

fn handle_search_web(params: &Value) -> Value {
    let query = match required_string(params, "query") {
        Ok(v) => v,
        Err(e) => return error(e),
    };
    let request = WebSearchRequest {
        query,
        limit: bounded_usize(params, "limit", 10, 1, 50),
        provider: provider_from_params(params),
        language: optional_string(params, "language"),
        time_range: optional_string(params, "time_range"),
    };

    call_sdk(|| web_search(&request))
}

fn handle_fetch_url(params: &Value) -> Value {
    let url = match required_string(params, "url") {
        Ok(v) => v,
        Err(e) => return error(e),
    };
    let request = WebReadUrlRequest {
        url,
        max_chars: bounded_usize(params, "max_chars", 30_000, 500, 200_000),
        mode: optional_string(params, "mode").unwrap_or_else(|| "auto".to_string()),
    };

    call_sdk(|| web_read_url(&request))
}

fn handle_read_search_results(params: &Value) -> Value {
    let query = match required_string(params, "query") {
        Ok(v) => v,
        Err(e) => return error(e),
    };
    let request = WebReadSearchResultsRequest {
        query,
        search_limit: bounded_usize(params, "search_limit", 10, 1, 50),
        read_limit: bounded_usize(params, "read_limit", 5, 1, 25),
        max_chars_per_page: bounded_usize(params, "max_chars_per_page", 30_000, 500, 200_000),
        provider: provider_from_params(params),
        mode: optional_string(params, "mode").unwrap_or_else(|| "auto".to_string()),
    };

    call_sdk(|| web_read_search_results(&request))
}

fn provider_from_params(params: &Value) -> Value {
    if let Some(provider) = params.get("provider") {
        return normalize_provider(provider);
    }
    if let Some(base_url) = params.get("searxng_base_url").and_then(Value::as_str) {
        return json!({"kind": "searxng", "base_url": base_url});
    }
    if let Some(api_key) = params.get("brave_api_key").and_then(Value::as_str) {
        return json!({
            "kind": "brave",
            "endpoint": params.get("brave_endpoint").and_then(Value::as_str),
            "api_key": api_key
        });
    }
    if let Some(api_key) = params.get("tavily_api_key").and_then(Value::as_str) {
        return json!({
            "kind": "tavily",
            "endpoint": params.get("tavily_endpoint").and_then(Value::as_str),
            "api_key": api_key,
            "search_depth": params.get("tavily_search_depth").and_then(Value::as_str)
        });
    }
    if params
        .get("duckduckgo")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || params.get("duckduckgo_endpoint").is_some()
    {
        return json!({
            "kind": "duckduckgo",
            "endpoint": params.get("duckduckgo_endpoint").and_then(Value::as_str)
        });
    }
    Value::Null
}

fn call_sdk(call: impl FnOnce() -> Result<Value, AbiError>) -> Value {
    match call() {
        Ok(mut value) => {
            if value.get("type").and_then(Value::as_str) == Some("error") {
                return error(
                    value
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("Blad web research"),
                );
            }
            if let Some(obj) = value.as_object_mut() {
                obj.insert("ok".to_string(), Value::Bool(true));
            }
            value
        }
        Err(e) => error(format!("Blad web research SDK: {}", e)),
    }
}

/// Turns whatever the model put in `provider` into the tagged shape Core
/// deserializes, or drops it so Core falls back to local/mesh/public search.
///
/// `SearchProviderConfig` is tagged by `kind`, and a model asked for "the
/// duckduckgo provider" naturally writes `"duckduckgo"` or `{"name": ...}` —
/// both of which used to travel to Core untouched and die there as
/// `missing field \`kind\``, three times in a row, with no way for the model to
/// recover. A JSON null is likewise not a provider: it means "you choose".
fn normalize_provider(provider: &Value) -> Value {
    match provider {
        // Explicit null / empty string = no preference.
        Value::Null => Value::Null,
        Value::String(name) => match name.trim().to_ascii_lowercase().as_str() {
            "" => Value::Null,
            // Only the two providers that need no credentials can be named by a
            // bare string; brave and tavily require an api key, so they can only
            // arrive as a full object.
            "duckduckgo" | "ddg" => json!({"kind": "duckduckgo"}),
            "searxng" => json!({"kind": "searxng"}),
            _ => Value::Null,
        },
        Value::Object(obj) => {
            if obj.is_empty() {
                return Value::Null;
            }
            if obj.contains_key("kind") {
                return provider.clone();
            }
            // Accept the near-misses a model produces instead of failing deep in
            // Core: the tag under another name, or a bare searxng base_url.
            for alias in ["type", "name", "provider"] {
                if let Some(kind) = obj.get(alias).and_then(Value::as_str) {
                    let mut normalized = obj.clone();
                    normalized.remove(alias);
                    normalized.insert("kind".to_string(), Value::String(kind.to_string()));
                    return Value::Object(normalized);
                }
            }
            if obj.contains_key("base_url") {
                let mut normalized = obj.clone();
                normalized.insert("kind".to_string(), Value::String("searxng".to_string()));
                return Value::Object(normalized);
            }
            Value::Null
        }
        _ => Value::Null,
    }
}

fn required_string(params: &Value, name: &str) -> Result<String, String> {
    let value = params
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if value.is_empty() {
        Err(format!("Parametr {} jest wymagany", name))
    } else {
        Ok(value.to_string())
    }
}

fn optional_string(params: &Value, name: &str) -> Option<String> {
    params
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn bounded_usize(params: &Value, name: &str, default: usize, min: usize, max: usize) -> usize {
    params
        .get(name)
        .and_then(Value::as_u64)
        .map(|v| (v as usize).clamp(min, max))
        .unwrap_or(default)
}

fn error(message: impl Into<String>) -> Value {
    json!({"ok": false, "error": message.into()})
}

fn write_response(out_ptr: i32, out_cap: i32, out_len_ptr: i32, value: &Value) -> i32 {
    let response_str = match serde_json::to_string(value) {
        Ok(s) => s,
        Err(_) => return 1,
    };
    let written = write_string(out_ptr, out_cap, out_len_ptr, &response_str);
    if written < 0 {
        log::error("Bufor wyjsciowy za maly na odpowiedz deep-research");
        return ABI_OUTPUT_BUFFER_TOO_SMALL;
    }
    let len_bytes = written.to_le_bytes();
    let dest = unsafe { std::slice::from_raw_parts_mut(out_len_ptr as *mut u8, 4) };
    dest.copy_from_slice(&len_bytes);
    0
}
