// ============ File: coding_agent.rs — Validated proxy to node-owned coding-agent CLI bridges. ============
//
// Defect D1 of the Code Studio plan (§1.2) is a property of THIS file as much as
// of the bridge: a model list that reaches the CLI is a model list that can cost
// a vendor session, so the question has to stop here whenever the answer is
// already known. `models.list` is answered from a Core-side cache with a TTL;
// only an explicit `refresh` travels to the bridge, and only a caller who meant
// it sets that flag.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::code_studio::cli_bridge::{turn_state, BridgeEvent, TurnState};
use crate::services::transport::Transport;
use crate::services_repo::services::ServiceRow;

/// How long a discovered model list is served without asking the bridge again.
/// A CLI gains models when the vendor ships a release, which also restarts the
/// service — so this bound exists for the case where nothing restarts for days,
/// not as the mechanism that keeps the list fresh.
const MODELS_CACHE_TTL: Duration = Duration::from_secs(6 * 60 * 60);

struct CachedModels {
    /// Raw bridge response, replayed verbatim so every caller sees exactly what
    /// the bridge said.
    response_json: String,
    fetched_at: Instant,
}

fn models_cache() -> &'static Mutex<HashMap<i64, CachedModels>> {
    static CACHE: OnceLock<Mutex<HashMap<i64, CachedModels>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Serves a cached model list, if one is fresh for this service.
fn cached_models(service_id: i64) -> Option<String> {
    let cache = models_cache().lock().ok()?;
    let entry = cache.get(&service_id)?;
    (entry.fetched_at.elapsed() < MODELS_CACHE_TTL).then(|| entry.response_json.clone())
}

fn store_models(service_id: i64, response_json: &str) {
    if let Ok(mut cache) = models_cache().lock() {
        cache.insert(
            service_id,
            CachedModels {
                response_json: response_json.to_string(),
                fetched_at: Instant::now(),
            },
        );
    }
}

/// Drops the cached list of one service. Called when the service is removed or
/// redeployed, so a stale list cannot outlive the bridge that produced it.
pub fn forget_models(service_id: i64) {
    if let Ok(mut cache) = models_cache().lock() {
        cache.remove(&service_id);
    }
}

pub fn sync_models(
    db: &crate::db::DbPool,
    service: &ServiceRow,
    result_json: &str,
) -> Result<usize, String> {
    let result: Value =
        serde_json::from_str(result_json).map_err(|e| format!("invalid models response: {e}"))?;
    let entries = result
        .get("models")
        .or_else(|| result.get("data"))
        .and_then(Value::as_array)
        .ok_or_else(|| "models response does not contain an array".to_string())?;
    let mut discovered = Vec::with_capacity(entries.len());
    for entry in entries {
        let raw_id = entry
            .get("id")
            .or_else(|| entry.get("model"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty() && id.len() <= 256)
            .ok_or_else(|| "models response contains an invalid id".to_string())?;
        if raw_id.chars().any(char::is_control) {
            return Err("models response contains a control character in id".to_string());
        }
        let display_name = entry
            .get("display_name")
            .or_else(|| entry.get("displayName"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(raw_id);
        let is_default = entry
            .get("selected")
            .or_else(|| entry.get("isDefault"))
            .or_else(|| entry.get("is_default"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        discovered.push(crate::services_repo::models::NewModel {
            service_id: service.id,
            model_name: format!("{}/{}", service.engine_id, raw_id),
            display_name: Some(format!("{} — {}", service.display_name, display_name)),
            capabilities: "[\"chat\"]".to_string(),
            context_length: None,
            quantization: None,
            is_default,
        });
    }
    let conn = db
        .write()
        .map_err(|_| "database pool is poisoned".to_string())?;
    crate::services_repo::models::replace_discovered(&conn, service.id, &discovered)
        .map_err(|e| e.to_string())?;
    Ok(discovered.len())
}

pub async fn execute(
    service: &ServiceRow,
    operation: &str,
    payload_json: &str,
) -> Result<String, String> {
    if operation.len() > 64 || payload_json.len() > 2 * 1024 * 1024 {
        return Err("coding-agent request exceeds the size limit".to_string());
    }
    if !matches!(service.engine_id.as_str(), "codex" | "claude-code")
        || service.transport != Transport::AgentRpc
    {
        return Err("service is not a Codex or Claude Code CLI service".to_string());
    }
    let base = service
        .endpoint_url
        .as_deref()
        .ok_or("coding-agent service has no endpoint")?
        .trim_end_matches('/');
    let parsed = reqwest::Url::parse(base).map_err(|e| format!("invalid service endpoint: {e}"))?;
    if !matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "::1")) {
        return Err("coding-agent bridge endpoint must be loopback-only".to_string());
    }

    let (method, path, body) = route(operation, payload_json)?;
    // A cached model list never reaches the bridge, so it can never reach the
    // CLI. `refresh` is encoded in the routed path, which is also what makes the
    // check impossible to bypass by spelling the payload differently.
    let serve_models_from_cache = operation == "models.list" && !path.contains("refresh=1");
    if serve_models_from_cache {
        if let Some(cached) = cached_models(service.id) {
            return Ok(cached);
        }
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(65))
        .build()
        .map_err(|e| e.to_string())?;
    let mut request = client.request(method, format!("{base}{path}"));
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request
        .send()
        .await
        .map_err(|e| format!("coding-agent bridge: {e}"))?;
    let status = response.status();
    let text = response.text().await.map_err(|e| e.to_string())?;
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err("session_expired".to_string());
    }
    if !status.is_success() {
        return Err(format!("coding-agent bridge returned {status}: {text}"));
    }
    serde_json::from_str::<Value>(&text)
        .map_err(|e| format!("bridge returned invalid JSON: {e}"))?;
    if operation == "models.list" {
        store_models(service.id, &text);
    }
    Ok(text)
}

/// Maps a protocol operation onto the bridge's HTTP surface. Pure, so the
/// mapping is testable without a running bridge.
fn route(
    operation: &str,
    payload_json: &str,
) -> Result<(reqwest::Method, String, Option<Value>), String> {
    let payload: Value = if payload_json.trim().is_empty() {
        Value::Object(Default::default())
    } else {
        serde_json::from_str(payload_json).map_err(|e| format!("invalid payload JSON: {e}"))?
    };
    // The bridge answers both of these from a cache; `refresh` is the only way
    // to make it drive the CLI again, so it stays a deliberate, user-triggered
    // act rather than a side effect of opening a window.
    let refreshed = |path: &str| {
        if payload
            .get("refresh")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            format!("{path}?refresh=1")
        } else {
            path.to_string()
        }
    };
    let session_id = || {
        payload
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|id| {
                !id.is_empty()
                    && id.len() <= 128
                    && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            })
            .ok_or_else(|| "payload requires a valid session_id".to_string())
    };
    let routed = match operation {
        "auth.status" => (reqwest::Method::GET, "/auth/status".to_string(), None),
        "auth.start" => (
            reqwest::Method::POST,
            "/auth/start".to_string(),
            Some(payload),
        ),
        "models.list" => (reqwest::Method::GET, refreshed("/models"), None),
        "usage.read" => (reqwest::Method::GET, refreshed("/usage"), None),
        "sessions.list" => (reqwest::Method::GET, "/sessions".to_string(), None),
        "session.create" => (
            reqwest::Method::POST,
            "/sessions".to_string(),
            Some(payload),
        ),
        "session.turn" => (
            reqwest::Method::POST,
            format!("/sessions/{}/turn", session_id()?),
            Some(payload),
        ),
        "session.input" => (
            reqwest::Method::POST,
            format!("/sessions/{}/input", session_id()?),
            Some(payload),
        ),
        "session.approval" => (
            reqwest::Method::POST,
            format!("/sessions/{}/approval", session_id()?),
            Some(payload),
        ),
        "session.close" => (
            reqwest::Method::DELETE,
            format!("/sessions/{}", session_id()?),
            None,
        ),
        "session.events" => {
            let after = payload
                .get("after_seq")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            (
                reqwest::Method::GET,
                format!("/sessions/{}/events?after_seq={after}", session_id()?),
                None,
            )
        }
        _ => return Err(format!("unsupported coding-agent operation: {operation}")),
    };
    Ok(routed)
}

pub async fn execute_chat(
    service: &ServiceRow,
    model_name: &str,
    prompt: &str,
) -> Result<String, String> {
    let workspace = serde_json::from_str::<Value>(&service.config_json)
        .ok()
        .and_then(|config| {
            config
                .get("workspace_root")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| ".".to_string());
    let model = model_name
        .strip_prefix(&format!("{}/", service.engine_id))
        .unwrap_or(model_name);
    let created = execute(
        service,
        "session.create",
        &serde_json::json!({"workspace": workspace, "model": model}).to_string(),
    )
    .await?;
    let session_id = serde_json::from_str::<Value>(&created)
        .ok()
        .and_then(|value| {
            value
                .pointer("/session/id")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .ok_or_else(|| "coding-agent session response has no session.id".to_string())?;
    execute(
        service,
        "session.turn",
        &serde_json::json!({"session_id": session_id, "prompt": prompt}).to_string(),
    )
    .await?;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(600);
    let mut after_seq = 0_u64;
    let mut output = String::new();
    let mut last_event = tokio::time::Instant::now();
    let mut received_output = false;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err("coding-agent turn timed out after 600 seconds".to_string());
        }
        let response = execute(
            service,
            "session.events",
            &serde_json::json!({"session_id": session_id, "after_seq": after_seq}).to_string(),
        )
        .await?;
        let value = serde_json::from_str::<Value>(&response)
            .map_err(|e| format!("invalid coding-agent events response: {e}"))?;
        let events = value
            .get("events")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut completed = false;
        for event in events {
            after_seq = after_seq.max(event.get("seq").and_then(Value::as_u64).unwrap_or(0));
            last_event = tokio::time::Instant::now();
            let kind = event.get("kind").and_then(Value::as_str).unwrap_or("");
            let data = event.get("data").cloned().unwrap_or(Value::Null);
            if kind == "terminal" {
                if let Some(text) = data.get("text").and_then(Value::as_str) {
                    output.push_str(text);
                    received_output = true;
                }
            } else if kind == "codex" || kind == "claude" {
                collect_agent_text(&data, &mut output);
                received_output = !output.is_empty();
                // The end of the turn is read by the one function that knows
                // both vendors' vocabularies, so a chat request and a Code
                // Studio delegation cannot disagree about whether a turn
                // finished — and a failed turn is not answered with the text it
                // managed to produce before failing.
                let structured = match kind {
                    "claude" => Some(BridgeEvent::StreamObject {
                        seq: 0,
                        object: data.clone(),
                    }),
                    _ => data.get("method").and_then(Value::as_str).map(|method| {
                        BridgeEvent::Notification {
                            seq: 0,
                            method: method.to_string(),
                            params: data.get("params").cloned().unwrap_or(Value::Null),
                        }
                    }),
                };
                match structured.as_ref().and_then(turn_state) {
                    Some(TurnState::Completed) => completed = true,
                    Some(TurnState::Failed(reason)) => {
                        return Err(format!("coding-agent turn failed: {reason}"))
                    }
                    None => {}
                }
            }
        }
        if completed
            || (received_output
                && last_event.elapsed()
                    >= std::time::Duration::from_secs(if service.engine_id == "codex" {
                        2
                    } else {
                        5
                    }))
        {
            let text = terminal_text(&output);
            if text.is_empty() {
                return Err("coding-agent turn completed without text output".to_string());
            }
            return Ok(text);
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

fn collect_agent_text(value: &Value, output: &mut String) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if matches!(key.as_str(), "delta" | "text") {
                    if let Some(text) = value.as_str() {
                        output.push_str(text);
                    }
                } else {
                    collect_agent_text(value, output);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_agent_text(value, output);
            }
        }
        _ => {}
    }
}

fn terminal_text(raw: &str) -> String {
    let mut output = String::with_capacity(raw.len());
    let mut bytes = raw.bytes().peekable();
    while let Some(byte) = bytes.next() {
        if byte == 0x1b {
            if bytes.next_if_eq(&b'[').is_some() {
                for next in bytes.by_ref() {
                    if (0x40..=0x7e).contains(&next) {
                        break;
                    }
                }
            }
            continue;
        }
        if byte == b'\r' {
            continue;
        }
        if byte == b'\n' || byte == b'\t' || byte.is_ascii_graphic() || byte == b' ' {
            output.push(byte as char);
        }
    }
    output.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path_of(operation: &str, payload: &str) -> String {
        route(operation, payload).unwrap().1
    }

    #[test]
    fn discovery_hits_the_bridge_cache_unless_refresh_is_requested() {
        // Driving the CLI is what creates vendor sessions, so the expensive
        // path must never be the default one.
        assert_eq!(path_of("models.list", "{}"), "/models");
        assert_eq!(path_of("usage.read", "{}"), "/usage");
        assert_eq!(
            path_of("models.list", r#"{"refresh":false}"#),
            "/models",
            "an explicit false must not force a probe"
        );
        assert_eq!(
            path_of("models.list", r#"{"refresh":true}"#),
            "/models?refresh=1"
        );
        assert_eq!(
            path_of("usage.read", r#"{"refresh":true}"#),
            "/usage?refresh=1"
        );
    }

    #[test]
    fn approval_and_close_are_routed_and_scoped_to_a_session() {
        let payload = r#"{"session_id":"auth-2f1c","request_id":7,"decision":"approved"}"#;
        let (method, path, body) = route("session.approval", payload).unwrap();
        assert_eq!(method, reqwest::Method::POST);
        assert_eq!(path, "/sessions/auth-2f1c/approval");
        assert_eq!(
            body.expect("approval carries the decision")["decision"],
            "approved"
        );

        let (method, path, body) = route("session.close", r#"{"session_id":"abc-123"}"#).unwrap();
        assert_eq!(method, reqwest::Method::DELETE);
        assert_eq!(path, "/sessions/abc-123");
        assert!(body.is_none());
    }

    #[test]
    fn session_scoped_operations_reject_a_forged_id() {
        // The id lands in a URL path; anything outside [A-Za-z0-9-] could reach
        // another endpoint of the bridge.
        for payload in [
            r#"{"session_id":"../auth/start"}"#,
            r#"{"session_id":""}"#,
            r#"{}"#,
        ] {
            assert!(
                route("session.close", payload).is_err(),
                "accepted {payload}"
            );
            assert!(
                route("session.approval", payload).is_err(),
                "accepted {payload}"
            );
        }
    }

    #[test]
    fn a_cached_model_list_is_served_without_reaching_the_bridge() {
        // The service id is local to this test, so the process-global cache is
        // not shared with any other test.
        let service_id = -4_242;
        assert!(cached_models(service_id).is_none());
        store_models(service_id, r#"{"models":[{"id":"sonnet"}]}"#);
        assert_eq!(
            cached_models(service_id).as_deref(),
            Some(r#"{"models":[{"id":"sonnet"}]}"#),
            "a fresh list must be answered from Core, never from the CLI"
        );

        // An explicit refresh routes to a path the cache check refuses to
        // match, which is what lets a deliberate probe through.
        assert!(!path_of("models.list", "{}").contains("refresh=1"));
        assert!(path_of("models.list", r#"{"refresh":true}"#).contains("refresh=1"));

        forget_models(service_id);
        assert!(
            cached_models(service_id).is_none(),
            "a restarted bridge must not be answered from the previous instance's list"
        );
    }

    #[test]
    fn unknown_operations_are_refused() {
        assert!(route("session.kill", r#"{"session_id":"abc"}"#).is_err());
    }

    /// Defect D1, measured the way the plan asks for it: count what the bridge —
    /// and therefore the CLI — is asked to do. Repeated discovery must cost the
    /// bridge exactly one call, because every call to a Claude Code bridge is a
    /// call that used to end in a vendor session.
    #[tokio::test]
    async fn repeated_discovery_reaches_the_bridge_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind stub bridge");
        let port = listener.local_addr().expect("addr").port();
        let hits = std::sync::Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let counter = counter.clone();
                tokio::spawn(async move {
                    let mut buffer = [0_u8; 4096];
                    let read = socket.read(&mut buffer).await.unwrap_or(0);
                    if String::from_utf8_lossy(&buffer[..read]).contains("/models") {
                        counter.fetch_add(1, Ordering::SeqCst);
                    }
                    let body = r#"{"models":[{"id":"sonnet","display_name":"Sonnet"}]}"#;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });

        let file = tempfile::NamedTempFile::new().unwrap();
        let db = crate::db::init(file.path()).unwrap();
        let service_id = {
            let conn = db.write().unwrap();
            conn.execute(
                "INSERT INTO services (engine_id, category, display_name, deploy_method, \
                    transport, status, endpoint_url) VALUES ('claude-code', 'agents', \
                    'Claude Code', 'native_managed_cli', 'agent_rpc', 'running', ?1)",
                rusqlite::params![format!("http://127.0.0.1:{port}")],
            )
            .unwrap();
            conn.last_insert_rowid()
        };
        let service = {
            let conn = db.read().unwrap();
            crate::services_repo::services::get(&conn, service_id)
                .unwrap()
                .unwrap()
        };
        forget_models(service.id);

        for _ in 0..5 {
            execute(&service, "models.list", "{}")
                .await
                .expect("models.list");
        }
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "the supervisor tick, the sessions window and the login path must share one answer"
        );

        // An explicit refresh is the deliberate exception, and it refills the
        // cache rather than bypassing it forever.
        execute(&service, "models.list", r#"{"refresh":true}"#)
            .await
            .expect("refresh");
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        execute(&service, "models.list", "{}")
            .await
            .expect("cached again");
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        forget_models(service.id);
    }

    #[test]
    fn discovered_models_are_namespaced_and_reconciled() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let db = crate::db::init(file.path()).unwrap();
        let service_id = {
            let conn = db.write().unwrap();
            conn.execute(
                "INSERT INTO services (engine_id, category, display_name, deploy_method, \
                    transport, status) VALUES ('claude-code', 'agents', 'Claude Code', \
                    'native_managed_cli', 'agent_rpc', 'running')",
                [],
            )
            .unwrap();
            conn.last_insert_rowid()
        };
        let service = {
            let conn = db.read().unwrap();
            crate::services_repo::services::get(&conn, service_id)
                .unwrap()
                .unwrap()
        };

        sync_models(
            &db,
            &service,
            r#"{"models":[
                {"id":"opus","display_name":"Opus","selected":true},
                {"id":"haiku","display_name":"Haiku","selected":false}
            ]}"#,
        )
        .unwrap();
        let first = {
            let conn = db.read().unwrap();
            crate::services_repo::models::list_for_service(&conn, service_id).unwrap()
        };
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].model_name, "claude-code/opus");
        assert_eq!(first[0].capabilities, "[\"chat\"]");
        assert!(first[0].is_default);
        assert_eq!(first[1].model_name, "claude-code/haiku");

        sync_models(
            &db,
            &service,
            r#"{"models":[{"id":"haiku","display_name":"Haiku 4.5","selected":true}]}"#,
        )
        .unwrap();
        let second = {
            let conn = db.read().unwrap();
            crate::services_repo::models::list_for_service(&conn, service_id).unwrap()
        };
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].model_name, "claude-code/haiku");
        assert_eq!(
            second[0].display_name.as_deref(),
            Some("Claude Code — Haiku 4.5")
        );
        assert!(second[0].is_default);
    }
}
