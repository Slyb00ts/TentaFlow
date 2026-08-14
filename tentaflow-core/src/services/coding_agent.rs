// ============ File: coding_agent.rs — Validated proxy to node-owned coding-agent CLI bridges. ============

use serde_json::Value;

use crate::services::transport::Transport;
use crate::services_repo::services::ServiceRow;

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

    let payload: Value = if payload_json.trim().is_empty() {
        Value::Object(Default::default())
    } else {
        serde_json::from_str(payload_json).map_err(|e| format!("invalid payload JSON: {e}"))?
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
    let (method, path, body) = match operation {
        "auth.status" => (reqwest::Method::GET, "/auth/status".to_string(), None),
        "auth.start" => (
            reqwest::Method::POST,
            "/auth/start".to_string(),
            Some(payload),
        ),
        "models.list" => (reqwest::Method::GET, "/models".to_string(), None),
        "usage.read" => (reqwest::Method::GET, "/usage".to_string(), None),
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
    Ok(text)
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
            } else if kind == "codex" {
                collect_agent_text(&data, &mut output);
                received_output = !output.is_empty();
                completed |= data.to_string().contains("turn/completed");
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
