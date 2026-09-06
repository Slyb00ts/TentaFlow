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

pub fn ensure_account_config(config: &mut Value) -> Result<String, String> {
    let config = config
        .as_object_mut()
        .ok_or("agent configuration must be an object")?;
    if !config.contains_key("account_id") {
        config.insert(
            "account_id".into(),
            Value::String(uuid::Uuid::new_v4().to_string()),
        );
    }
    let id = config
        .get("account_id")
        .and_then(Value::as_str)
        .ok_or("invalid account_id")?;
    let parsed = uuid::Uuid::parse_str(id).map_err(|_| "invalid account_id")?;
    if parsed.to_string() != id {
        return Err("account_id must be a canonical UUID".into());
    }
    Ok(id.to_string())
}

pub fn account_directory(config: &Value) -> Result<std::path::PathBuf, String> {
    let id = config
        .get("account_id")
        .and_then(Value::as_str)
        .ok_or("agent account requires redeployment before use")?;
    let parsed = uuid::Uuid::parse_str(id).map_err(|_| "invalid account_id")?;
    if parsed.to_string() != id {
        return Err("account_id must be a canonical UUID".into());
    }
    Ok(crate::paths::keys_dir()
        .join("coding-agents")
        .join("accounts")
        .join(id))
}

pub fn prepare_account_directory(config: &Value) -> Result<std::path::PathBuf, String> {
    use std::io::Write;
    let root = account_directory(config)?;
    for path in [
        root.clone(),
        root.join("home"),
        root.join("codex"),
        root.join("claude"),
        root.join("grok"),
        root.join("config"),
        root.join("config/muse"),
        root.join("data"),
        root.join("data/muse"),
        root.join("tmp"),
    ] {
        std::fs::create_dir_all(&path).map_err(|e| format!("create account profile: {e}"))?;
        let metadata = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("account profile must be a real directory".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
    }
    let path = root.join("bridge-token");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&path) {
        Ok(mut file) => file
            .write_all(
                format!(
                    "{}{}",
                    uuid::Uuid::new_v4().simple(),
                    uuid::Uuid::new_v4().simple()
                )
                .as_bytes(),
            )
            .map_err(|e| e.to_string())?,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            read_bridge_token(config)?;
        }
        Err(e) => return Err(format!("create bridge credential: {e}")),
    }
    Ok(root)
}

fn read_bridge_token(config: &Value) -> Result<String, String> {
    let path = account_directory(config)?.join("bridge-token");
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|e| format!("read bridge credential metadata: {e}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() != 64 {
        return Err("invalid bridge credential file".into());
    }
    let token =
        std::fs::read_to_string(path).map_err(|e| format!("read bridge credential: {e}"))?;
    if !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid bridge credential".into());
    }
    Ok(token)
}

pub fn account_permission(
    db: &crate::db::DbPool,
    service_id: i64,
    user_id: &str,
) -> Result<(bool, bool), String> {
    use rusqlite::OptionalExtension;
    let conn = db.read().map_err(|e| e.to_string())?;
    let role: Option<String> = conn
        .query_row(
            "SELECT CASE WHEN is_admin = 1 THEN 'admin' ELSE role END FROM user_accounts WHERE id = ?1 AND is_active = 1",
            [user_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let role = role.ok_or("agent account actor does not exist on this node")?;
    let admin = role == "admin";
    let service = crate::services_repo::services::get(&conn, service_id)
        .map_err(|e| e.to_string())?
        .ok_or("agent account service does not exist")?;
    if !matches!(
        service.engine_id.as_str(),
        "codex" | "claude-code" | "grok-build" | "muse-code"
    ) || service.transport != Transport::AgentRpc
    {
        return Err("service is not an agent account".into());
    }
    let blocked:bool=conn.query_row("SELECT COALESCE((SELECT (phase<>'target_active' OR activation_complete=0) FROM coding_agent_account_moves WHERE service_id=?1 ORDER BY rowid DESC LIMIT 1),0)",[service_id],|row|row.get(0)).map_err(|error|error.to_string())?;
    if blocked { return Ok((false,admin)); }
    let config: Value = serde_json::from_str(&service.config_json).map_err(|e| e.to_string())?;
    if account_directory(&config).is_err() {
        return Ok((false, admin));
    }
    let grant: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM coding_agent_account_grants WHERE service_id = ?1 AND user_id = ?2)", rusqlite::params![service_id, user_id], |row| row.get(0)).map_err(|e| e.to_string())?;
    Ok((admin || grant, admin))
}

fn owned_sessions(
    db: &crate::db::DbPool,
    service_id: i64,
    user_id: &str,
) -> Result<Vec<String>, String> {
    let conn = db.read().map_err(|e| e.to_string())?;
    let mut statement = conn.prepare("SELECT session_id FROM coding_agent_session_owners WHERE service_id = ?1 AND user_id = ?2").map_err(|e| e.to_string())?;
    let rows = statement
        .query_map(rusqlite::params![service_id, user_id], |row| row.get(0))
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<String>, _>>()
        .map_err(|e| e.to_string())
}

pub(crate) async fn lock_account(service_id:i64)->Result<tokio::sync::OwnedMutexGuard<()>,String> {
    static LOCKS: OnceLock<Mutex<HashMap<i64, std::sync::Arc<tokio::sync::Mutex<()>>>>> =
        OnceLock::new();
    let lock = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|e| e.to_string())?
        .entry(service_id)
        .or_default()
        .clone();
    Ok(lock.lock_owned().await)
}

pub async fn execute_authorized(
    db: &crate::db::DbPool,
    service: &ServiceRow,
    user_id: &str,
    operation: &str,
    payload_json: &str,
) -> Result<String, String> {
    let _guard = lock_account(service.id).await?;
    let (can_use, can_manage) = account_permission(db, service.id, user_id)?;
    let config: Value = serde_json::from_str(&service.config_json).map_err(|e| e.to_string())?;
    let account_id = config.get("account_id").and_then(Value::as_str);
    let access = |name: &str| {
        serde_json::json!({"account_id":account_id,"service_id":service.id,"display_name":name,"can_use":can_use,"can_manage":can_manage}).to_string()
    };
    if operation == "account.access" {
        return Ok(access(&service.display_name));
    }
    if matches!(operation,"account.rename"|"account.grants.set") {
        let blocked:bool=db.read().map_err(|error|error.to_string())?.query_row("SELECT COALESCE((SELECT (phase<>'target_active' OR activation_complete=0) FROM coding_agent_account_moves WHERE service_id=?1 ORDER BY rowid DESC LIMIT 1),0)",[service.id],|row|row.get(0)).map_err(|error|error.to_string())?;
        if blocked {return Err("account_moving: account settings are frozen during relocation".into());}
    }
    let mut payload: Value = serde_json::from_str(if payload_json.trim().is_empty() {
        "{}"
    } else {
        payload_json
    })
    .map_err(|e| e.to_string())?;
    if operation.starts_with("account.") {
        if !can_manage {
            return Err("administrator_required_for_account_management".into());
        }
        if operation == "account.rename" {
            let name = payload
                .get("display_name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control))
                .ok_or("invalid account name")?;
            db.write()
                .map_err(|e| e.to_string())?
                .execute(
                    "UPDATE services SET display_name = ?1 WHERE id = ?2",
                    rusqlite::params![name, service.id],
                )
                .map_err(|e| e.to_string())?;
            return Ok(access(name));
        }
        if operation == "account.grants.set" {
            let target = payload
                .get("user_id")
                .and_then(Value::as_str)
                .ok_or("user_id is required")?;
            let permitted = payload
                .get("can_use")
                .and_then(Value::as_bool)
                .ok_or("can_use is required")?;
            {
                let conn = db.write().map_err(|e| e.to_string())?;
                if permitted {
                    conn.execute("INSERT INTO coding_agent_account_grants(service_id,user_id,granted_by) VALUES(?1,?2,?3) ON CONFLICT(service_id,user_id) DO UPDATE SET granted_by=excluded.granted_by", rusqlite::params![service.id,target,user_id]).map_err(|e| e.to_string())?;
                } else {
                    conn.execute("DELETE FROM coding_agent_account_grants WHERE service_id=?1 AND user_id=?2", rusqlite::params![service.id,target]).map_err(|e| e.to_string())?;
                }
            }
            if !permitted {
                for session in owned_sessions(db, service.id, target)? {
                    execute(
                        service,
                        "session.close",
                        &serde_json::json!({"session_id":session}).to_string(),
                    )
                    .await?;
                }
            }
        } else if operation != "account.grants.list" {
            return Err("unknown account operation".into());
        }
        let conn = db.read().map_err(|e| e.to_string())?;
        let mut statement = conn.prepare("SELECT user_id FROM coding_agent_account_grants WHERE service_id=?1 ORDER BY user_id").map_err(|e| e.to_string())?;
        let grants = statement
            .query_map([service.id], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        return Ok(serde_json::json!({"grants":grants.into_iter().map(|id| serde_json::json!({"user_id":id,"can_use":true})).collect::<Vec<_>>()}).to_string());
    }
    if operation.starts_with("runtime.") { return Err("private bridge lifecycle operation".into()); }
    if !can_use {
        return Err("agent_account_access_denied".into());
    }
    if operation == "session.create" {
        if let Some(resume) = payload
            .get("resume_vendor_session_id")
            .and_then(Value::as_str)
        {
            let conn = db.read().map_err(|e| e.to_string())?;
            let owned: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM coding_agent_session_owners WHERE service_id=?1 AND user_id=?2 AND vendor_session_id=?3)", rusqlite::params![service.id,user_id,resume], |row| row.get(0)).map_err(|e| e.to_string())?;
            if !owned {
                return Err("agent_session_resume_access_denied".into());
            }
        }
    }
    if operation == "auth.start" && !can_manage {
        return Err("administrator_required_for_login".into());
    }
    if operation.starts_with("session.") && operation != "session.create" {
        let session_id = payload
            .get("session_id")
            .and_then(Value::as_str)
            .ok_or("session_id is required")?;
        if !owned_sessions(db, service.id, user_id)?
            .iter()
            .any(|id| id == session_id)
        {
            return Err("agent_session_access_denied".into());
        }
    }
    if operation == "session.create" {
        let object=payload.as_object_mut().ok_or("session payload must be an object")?;
        let id=object.entry("session_id").or_insert_with(|| Value::String(uuid::Uuid::new_v4().to_string())).as_str().ok_or("invalid session identifier")?;
        if !uuid::Uuid::parse_str(id).is_ok_and(|value| value.to_string()==id) { return Err("invalid session identifier".into()); }
        let conn=db.write().map_err(|error|error.to_string())?;
        conn.execute("INSERT INTO coding_agent_session_owners(service_id,session_id,user_id) VALUES(?1,?2,?3) ON CONFLICT(service_id,session_id) DO NOTHING",rusqlite::params![service.id,id,user_id]).map_err(|error|error.to_string())?;
        let owner:String=conn.query_row("SELECT user_id FROM coding_agent_session_owners WHERE service_id=?1 AND session_id=?2",rusqlite::params![service.id,id],|row|row.get(0)).map_err(|error|error.to_string())?;
        if owner!=user_id { return Err("agent_session_access_denied".into()); }
    }
    let result = execute(service, operation, &payload.to_string()).await?;

    if operation == "session.create" || operation == "auth.start" {
        let parsed: Value = serde_json::from_str(&result).map_err(|e| e.to_string())?;
        if let Some(id) = parsed
            .pointer("/session/id")
            .or_else(|| parsed.get("flow_id"))
            .and_then(Value::as_str)
        {
            if operation == "session.create" && payload.get("session_id").and_then(Value::as_str) != Some(id) {
                return Err("bridge returned a different reserved session identifier".into());
            }
            let vendor = parsed
                .pointer("/session/vendor_session_id")
                .and_then(Value::as_str);
            let persisted = (|| {
                db.write().map_err(|e| e.to_string())?.execute("INSERT INTO coding_agent_session_owners(service_id,session_id,user_id,vendor_session_id) VALUES(?1,?2,?3,?4) ON CONFLICT(service_id,session_id) DO UPDATE SET vendor_session_id=excluded.vendor_session_id WHERE coding_agent_session_owners.user_id=excluded.user_id",rusqlite::params![service.id,id,user_id,vendor]).map_err(|e| e.to_string())
            })();
            if let Err(error) = persisted {
                let cleanup = execute(
                    service,
                    "session.close",
                    &serde_json::json!({"session_id":id}).to_string(),
                )
                .await;
                return Err(format!(
                    "persist account session owner: {error}; cleanup: {}",
                    if cleanup.is_ok() {
                        "confirmed"
                    } else {
                        "failed; stop the account service"
                    }
                ));
            }
            monitor_session(
                db.clone(),
                service.clone(),
                user_id.to_owned(),
                id.to_owned(),
                operation == "auth.start",
            );
        }
    }
    if operation == "sessions.list" {
        let owned = owned_sessions(db, service.id, user_id)?;
        let mut parsed: Value = serde_json::from_str(&result).map_err(|e| e.to_string())?;
        if let Some(sessions) = parsed.get_mut("sessions").and_then(Value::as_array_mut) {
            sessions.retain(|session| {
                session
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| owned.iter().any(|own| own == id))
            });
        }
        return Ok(parsed.to_string());
    }
    Ok(result)
}

fn monitor_session(
    db: crate::db::DbPool,
    service: ServiceRow,
    user_id: String,
    session_id: String,
    login: bool,
) {
    tokio::spawn(async move {
        let payload = serde_json::json!({"session_id":session_id,"after_seq":u64::MAX}).to_string();
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let authorized = account_permission(&db, service.id, &user_id)
                .map(|(can_use, can_manage)| if login { can_manage } else { can_use })
                .unwrap_or(false);
            let active = if authorized {
                execute(&service, "session.events", &payload)
                    .await
                    .ok()
                    .and_then(|response| serde_json::from_str::<Value>(&response).ok())
                    .and_then(|response| {
                        response
                            .get("status")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
            } else {
                None
            };
            if active.as_deref() == Some("closed") {
                break;
            }
            if !authorized || active.is_none() {
                match execute(&service, "session.close", &payload).await {
                    Ok(_) => break,
                    Err(error) => {
                        tracing::error!(service_id=service.id,%session_id,%error,"agent permission revocation awaits process cleanup")
                    }
                }
            }
        }
    });
}

pub async fn execute_public(
    db: &crate::db::DbPool,
    service: &ServiceRow,
    user_id: &str,
    operation: &str,
    payload_json: &str,
) -> Result<String, String> {
    let mut payload: Value = serde_json::from_str(if payload_json.trim().is_empty() {
        "{}"
    } else {
        payload_json
    })
    .map_err(|e| e.to_string())?;
    if operation == "session.create" {
        let object = payload
            .as_object_mut()
            .ok_or("session payload must be an object")?;
        object.remove("workspace");
        object.remove("env");
        object.remove("args");
        object.insert(
            "private_workspace".into(),
            Value::String(user_id.to_string()),
        );
    }
    execute_authorized(db, service, user_id, operation, &payload.to_string()).await
}

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
    if !matches!(
        service.engine_id.as_str(),
        "codex" | "claude-code" | "grok-build" | "muse-code"
    ) || service.transport != Transport::AgentRpc
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
    let config: Value = serde_json::from_str(&service.config_json)
        .map_err(|e| format!("invalid agent configuration: {e}"))?;
    let token = read_bridge_token(&config)?;
    let mut request = client
        .request(method, format!("{base}{path}"))
        .bearer_auth(token);
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
        "runtime.status" => (reqwest::Method::GET,"/runtime/status".into(),None),
        "runtime.shutdown" => (reqwest::Method::POST,"/runtime/shutdown".into(),Some(payload)),
        "account.transfer.freeze" => (reqwest::Method::POST,"/account/transfer/freeze".into(),Some(payload)),
        "account.transfer.retire" => (reqwest::Method::POST,"/account/transfer/retire".into(),Some(payload)),
        "account.transfer.activate" => (reqwest::Method::POST,"/account/transfer/activate".into(),Some(payload)),
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
    db: &crate::db::DbPool,
    service: &ServiceRow,
    user_id: &str,
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
    let created = execute_public(
        db,
        service,
        user_id,
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
    let result = async {
        execute_authorized(
            db,
            service,
            user_id,
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
            let response = execute_authorized(
                db,
                service,
                user_id,
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
    .await;
    let closed = execute_authorized(
        db,
        service,
        user_id,
        "session.close",
        &serde_json::json!({"session_id":session_id}).to_string(),
    )
    .await;
    match (result, closed) {
        (Ok(text), Ok(_)) => Ok(text),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(format!("agent session cleanup failed: {error}")),
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

    #[tokio::test]
    async fn account_grants_do_not_grant_other_actors_sessions() {
        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        let service = {
            let conn = db.write().unwrap();
            for (id, role) in [
                ("account-admin", "admin"),
                ("account-first", "viewer"),
                ("account-second", "viewer"),
            ] {
                conn.execute("INSERT INTO user_accounts(id,username,password_hash,role) VALUES(?1,?1,'synthetic',?2)",rusqlite::params![id,role]).unwrap();
            }
            let mut new = crate::services_repo::services::NewService::minimal(
                "codex",
                crate::services_repo::services::DeployMethod::NativeManagedCli,
                Transport::AgentRpc,
            );
            let mut config = serde_json::json!({});
            ensure_account_config(&mut config).unwrap();
            new.config_json = config.to_string();
            let id = crate::services_repo::services::insert(&conn, &new).unwrap();
            conn.execute("INSERT INTO coding_agent_session_owners(service_id,session_id,user_id,vendor_session_id) VALUES(?1,'private-session','account-first','vendor-private')",[id]).unwrap();
            crate::services_repo::services::get(&conn, id)
                .unwrap()
                .unwrap()
        };
        assert_eq!(
            account_permission(&db, service.id, "account-second").unwrap(),
            (false, false)
        );
        assert!(execute_authorized(
            &db,
            &service,
            "account-second",
            "account.grants.set",
            r#"{"user_id":"account-second","can_use":true}"#
        )
        .await
        .is_err());
        execute_authorized(
            &db,
            &service,
            "account-admin",
            "account.grants.set",
            r#"{"user_id":"account-second","can_use":true}"#,
        )
        .await
        .unwrap();
        assert_eq!(
            account_permission(&db, service.id, "account-second").unwrap(),
            (true, false)
        );
        assert!(execute_authorized(
            &db,
            &service,
            "account-second",
            "session.events",
            r#"{"session_id":"private-session"}"#
        )
        .await
        .unwrap_err()
        .contains("session_access_denied"));
        assert!(execute_authorized(
            &db,
            &service,
            "account-second",
            "session.create",
            r#"{"resume_vendor_session_id":"vendor-private"}"#
        )
        .await
        .unwrap_err()
        .contains("resume_access_denied"));
        execute_authorized(
            &db,
            &service,
            "account-admin",
            "account.grants.set",
            r#"{"user_id":"account-second","can_use":false}"#,
        )
        .await
        .unwrap();
        assert!(
            !account_permission(&db, service.id, "account-second")
                .unwrap()
                .0
        );
    }

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

    #[tokio::test]
    async fn inactive_account_actor_is_stopped_without_requiring_another_request() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (sent, received) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = vec![0; 8192];
            let count = socket.read(&mut bytes).await.unwrap();
            let request = String::from_utf8(bytes[..count].to_vec()).unwrap();
            let body = r#"{"closed":true,"process_state":"reaped"}"#;
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            sent.send(request).unwrap();
        });
        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        let mut config = serde_json::json!({});
        ensure_account_config(&mut config).unwrap();
        let directory = prepare_account_directory(&config).unwrap();
        let service = {
            let conn = db.write().unwrap();
            conn.execute("INSERT INTO user_accounts(id,username,password_hash,role,is_active) VALUES('inactive-agent-user','inactive-agent-user','synthetic','admin',0)",[]).unwrap();
            let mut new = crate::services_repo::services::NewService::minimal(
                "codex",
                crate::services_repo::services::DeployMethod::NativeManagedCli,
                Transport::AgentRpc,
            );
            new.config_json = config.to_string();
            new.endpoint_url = Some(endpoint);
            let id = crate::services_repo::services::insert(&conn, &new).unwrap();
            crate::services_repo::services::get(&conn, id)
                .unwrap()
                .unwrap()
        };
        monitor_session(
            db,
            service,
            "inactive-agent-user".into(),
            "owned-session".into(),
            false,
        );
        let request = tokio::time::timeout(Duration::from_secs(3), received)
            .await
            .unwrap()
            .unwrap();
        assert!(request.starts_with("DELETE /sessions/owned-session "));
        assert!(request
            .to_ascii_lowercase()
            .contains("authorization: bearer "));
        server.await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
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
        let mut service = {
            let conn = db.read().unwrap();
            crate::services_repo::services::get(&conn, service_id)
                .unwrap()
                .unwrap()
        };
        let mut config = serde_json::json!({});
        ensure_account_config(&mut config).unwrap();
        let profile = prepare_account_directory(&config).unwrap();
        service.config_json = config.to_string();
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
        std::fs::remove_dir_all(profile).unwrap();
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
