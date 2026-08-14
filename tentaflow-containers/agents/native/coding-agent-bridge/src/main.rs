use std::{
    collections::HashMap,
    env,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
};

use anyhow::{anyhow, Context, Result};
use axum::{
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use parking_lot::Mutex as SyncMutex;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{oneshot, Mutex},
};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Provider {
    Codex,
    ClaudeCode,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SessionMeta {
    id: String,
    vendor_session_id: String,
    workspace: String,
    status: String,
    #[serde(default)]
    model: Option<String>,
    created_at_ms: u128,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Event {
    seq: u64,
    kind: String,
    data: Value,
}

enum Runtime {
    Codex(CodexRuntime),
    Terminal(TerminalRuntime),
}

struct Session {
    meta: SessionMeta,
    runtime: Option<Runtime>,
    events: Arc<SyncMutex<Vec<Event>>>,
}

#[derive(Clone)]
struct AppState {
    provider: Provider,
    workspace_root: PathBuf,
    state_file: PathBuf,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
}

struct CodexRuntime {
    thread_id: String,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Arc<SyncMutex<HashMap<u64, oneshot::Sender<Value>>>>,
    next_id: AtomicU64,
    _child: Child,
}

struct TerminalRuntime {
    writer: Arc<SyncMutex<Box<dyn Write + Send>>>,
    ready: Arc<AtomicBool>,
    _master: Box<dyn MasterPty + Send>,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
}

#[derive(Deserialize)]
struct CreateSession {
    workspace: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    resume_vendor_session_id: Option<String>,
    #[serde(default)]
    fork: bool,
}

#[derive(Deserialize)]
struct TurnRequest {
    prompt: String,
}

#[derive(Deserialize)]
struct InputRequest {
    text: String,
}

#[derive(Deserialize)]
struct EventQuery {
    #[serde(default)]
    after_seq: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let provider = match env::var("TENTAFLOW_ENGINE_ID").as_deref() {
        Ok("codex") => Provider::Codex,
        Ok("claude-code") => Provider::ClaudeCode,
        Ok(other) => return Err(anyhow!("unsupported TENTAFLOW_ENGINE_ID {other:?}")),
        Err(_) => return Err(anyhow!("TENTAFLOW_ENGINE_ID is required")),
    };
    let data_dir = PathBuf::from(
        env::var("TENTAFLOW_CODING_AGENT_DATA_DIR")
            .unwrap_or_else(|_| ".tentaflow-coding-agent".into()),
    );
    std::fs::create_dir_all(&data_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&data_dir, std::fs::Permissions::from_mode(0o700))?;
    }
    require_binary(provider)?;
    let state_file = data_dir.join("sessions.json");
    let sessions = load_sessions(&state_file)?;
    let workspace_root =
        std::fs::canonicalize(env::var("TENTAFLOW_WORKSPACE_ROOT").unwrap_or_else(|_| ".".into()))?;
    let state = AppState {
        provider,
        workspace_root,
        state_file,
        sessions: Arc::new(Mutex::new(sessions)),
    };
    let app = Router::new()
        .route("/health", get(health))
        .route("/auth/status", get(auth_status))
        .route("/auth/start", post(auth_start))
        .route("/models", get(list_models))
        .route("/usage", get(usage))
        .route("/sessions", get(list_sessions).post(create_session))
        .route("/sessions/{id}/turn", post(start_turn))
        .route("/sessions/{id}/input", post(send_input))
        .route("/sessions/{id}/events", get(list_events))
        .with_state(state);
    let port: u16 = env::var("PORT").unwrap_or_else(|_| "8765".into()).parse()?;
    let bind_host = env::var("TENTAFLOW_BIND_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    if !matches!(bind_host.as_str(), "127.0.0.1" | "0.0.0.0") {
        return Err(anyhow!("unsupported TENTAFLOW_BIND_HOST {bind_host:?}"));
    }
    let listener = tokio::net::TcpListener::bind((bind_host.as_str(), port)).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn require_binary(provider: Provider) -> Result<()> {
    let binary = if provider == Provider::Codex {
        "codex"
    } else {
        "claude"
    };
    std::process::Command::new(binary)
        .arg("--version")
        .output()
        .with_context(|| format!("{binary} CLI is not installed"))?;
    Ok(())
}

fn load_sessions(path: &Path) -> Result<HashMap<String, Session>> {
    let metas: Vec<SessionMeta> = match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    Ok(metas
        .into_iter()
        .map(|meta| {
            (
                meta.id.clone(),
                Session {
                    meta,
                    runtime: None,
                    events: Arc::new(SyncMutex::new(Vec::new())),
                },
            )
        })
        .collect())
}

async fn persist(state: &AppState) -> Result<()> {
    let sessions = state.sessions.lock().await;
    let metas: Vec<_> = sessions.values().map(|s| s.meta.clone()).collect();
    let tmp = state.state_file.with_extension("tmp");
    tokio::fs::write(&tmp, serde_json::to_vec(&metas)?).await?;
    tokio::fs::rename(tmp, &state.state_file).await?;
    Ok(())
}

async fn health(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"ok": true, "provider": state.provider}))
}

async fn auth_status(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    match authentication_status(state.provider).await {
        Ok((authenticated, output)) => (
            StatusCode::OK,
            Json(json!({
                "authenticated": authenticated,
                "status": if authenticated { "authenticated" } else { "session_expired" },
                "output": output,
            })),
        ),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"authenticated": false, "status": "error", "error": error.to_string()})),
        ),
    }
}

async fn authentication_status(provider: Provider) -> Result<(bool, String)> {
    let mut command = if provider == Provider::Codex {
        let mut command = Command::new("codex");
        command.args(["login", "status"]);
        command
    } else {
        let mut command = Command::new("claude");
        command.args(["auth", "status"]);
        command
    };
    command
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(10), command.output())
        .await
        .context("authentication status timed out")??;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok((output.status.success(), text))
}

async fn require_authenticated(provider: Provider) -> Result<(), ApiError> {
    match authentication_status(provider).await {
        Ok((true, _)) => Ok(()),
        Ok((false, _)) => Err(ApiError::unauthorized("session_expired")),
        Err(error) => Err(ApiError::internal(&format!(
            "authentication status failed: {error}"
        ))),
    }
}

async fn auth_start(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let workspace = state.workspace_root.to_string_lossy().into_owned();
    let id = format!("auth-{}", uuid::Uuid::new_v4());
    let events = Arc::new(SyncMutex::new(Vec::new()));
    let runtime = spawn_terminal(
        state.provider,
        None,
        false,
        &workspace,
        events.clone(),
        true,
        None,
        None,
    )?;
    let meta = SessionMeta {
        id: id.clone(),
        vendor_session_id: id.clone(),
        workspace,
        status: "authenticating".into(),
        model: None,
        created_at_ms: now_ms(),
    };
    state.sessions.lock().await.insert(
        id.clone(),
        Session {
            meta,
            runtime: Some(Runtime::Terminal(runtime)),
            events,
        },
    );
    Ok(Json(json!({"flow_id": id})))
}

async fn list_sessions(State(state): State<AppState>) -> Json<Value> {
    let sessions = state.sessions.lock().await;
    Json(
        json!({"sessions": sessions.values().filter(|s| !s.meta.id.starts_with("auth-")).map(|s| &s.meta).collect::<Vec<_>>() }),
    )
}

async fn list_models(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    require_authenticated(state.provider).await?;
    match state.provider {
        Provider::Codex => {
            let events = Arc::new(SyncMutex::new(Vec::new()));
            let runtime =
                CodexRuntime::connect(&state.workspace_root.to_string_lossy(), events).await?;
            let response = runtime
                .request("model/list", json!({"includeHidden": false}))
                .await?;
            Ok(Json(response.get("result").cloned().unwrap_or(response)))
        }
        Provider::ClaudeCode => {
            let events = Arc::new(SyncMutex::new(Vec::new()));
            let mut runtime = spawn_terminal(
                state.provider,
                None,
                false,
                &state.workspace_root.to_string_lossy(),
                events.clone(),
                false,
                Some(&uuid::Uuid::new_v4().to_string()),
                None,
            )?;
            runtime.wait_ready().await?;
            events.lock().clear();
            runtime.writer.lock().write_all(b"/model\r")?;
            runtime.writer.lock().flush()?;
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                let raw = events
                    .lock()
                    .iter()
                    .filter_map(|event| event.data.get("text").and_then(Value::as_str))
                    .collect::<String>();
                let models = parse_claude_models(raw.as_bytes());
                if !models.is_empty() {
                    runtime.writer.lock().write_all(b"\x1b")?;
                    runtime.writer.lock().flush()?;
                    return Ok(Json(json!({"models": models})));
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(ApiError::internal(
                        "Claude Code /model menu did not produce a model list",
                    ));
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

async fn usage(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    require_authenticated(state.provider).await?;
    match state.provider {
        Provider::Codex => {
            let events = Arc::new(SyncMutex::new(Vec::new()));
            let runtime =
                CodexRuntime::connect(&state.workspace_root.to_string_lossy(), events).await?;
            let response = runtime
                .request("account/rateLimits/read", Value::Null)
                .await?;
            let result = response
                .get("result")
                .ok_or_else(|| ApiError::internal("Codex usage response has no result"))?;
            Ok(Json(normalize_codex_usage(result)))
        }
        Provider::ClaudeCode => {
            let events = Arc::new(SyncMutex::new(Vec::new()));
            let mut runtime = spawn_terminal(
                state.provider,
                None,
                false,
                &state.workspace_root.to_string_lossy(),
                events.clone(),
                false,
                Some(&uuid::Uuid::new_v4().to_string()),
                None,
            )?;
            runtime.wait_ready().await?;
            events.lock().clear();
            runtime.writer.lock().write_all(b"/usage\r")?;
            runtime.writer.lock().flush()?;
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
            loop {
                let raw = events
                    .lock()
                    .iter()
                    .filter_map(|event| event.data.get("text").and_then(Value::as_str))
                    .collect::<String>();
                if let Some(result) = parse_claude_usage(raw.as_bytes()) {
                    runtime.writer.lock().write_all(b"\x1b")?;
                    runtime.writer.lock().flush()?;
                    return Ok(Json(result));
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(ApiError::internal(
                        "Claude Code /usage did not produce session and weekly limits",
                    ));
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

fn normalize_codex_usage(result: &Value) -> Value {
    let snapshot = result.get("rateLimits").unwrap_or(&Value::Null);
    let mut current_session = Value::Null;
    let mut weekly = Value::Null;
    for window in [snapshot.get("primary"), snapshot.get("secondary")]
        .into_iter()
        .flatten()
        .filter(|value| !value.is_null())
    {
        let duration = window.get("windowDurationMins").and_then(Value::as_u64);
        let normalized = normalize_usage_window(window);
        if duration.is_some_and(|minutes| minutes >= 6 * 24 * 60) {
            weekly = normalized;
        } else {
            current_session = normalized;
        }
    }
    let mut model_limits = Vec::new();
    if let Some(buckets) = result.get("rateLimitsByLimitId").and_then(Value::as_object) {
        for (id, bucket) in buckets {
            if id
                == snapshot
                    .get("limitId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            {
                continue;
            }
            for window in [bucket.get("primary"), bucket.get("secondary")]
                .into_iter()
                .flatten()
                .filter(|value| !value.is_null())
            {
                model_limits.push(json!({
                    "id": id,
                    "name": bucket.get("limitName").and_then(Value::as_str).unwrap_or(id),
                    "window": normalize_usage_window(window),
                }));
            }
        }
    }
    json!({
        "current_session": current_session,
        "weekly": weekly,
        "model_limits": model_limits,
        "updated_at_ms": now_ms(),
    })
}

fn normalize_usage_window(window: &Value) -> Value {
    let used = window
        .get("usedPercent")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(100);
    json!({
        "used_percent": used,
        "remaining_percent": 100 - used,
        "resets_at_unix": window.get("resetsAt").and_then(Value::as_i64),
        "resets_at_label": Value::Null,
        "window_minutes": window.get("windowDurationMins").and_then(Value::as_u64),
    })
}

fn parse_claude_usage(raw: &[u8]) -> Option<Value> {
    let mut parser = vt100::Parser::new(40, 120, 0);
    parser.process(raw);
    let lines = parser
        .screen()
        .contents()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let current_session = parse_claude_usage_window(&lines, "Current session")?;
    let weekly = parse_claude_usage_window(&lines, "Current week (all models)")?;
    let model_limits = lines
        .iter()
        .filter(|line| {
            line.starts_with("Current week (") && !line.starts_with("Current week (all models)")
        })
        .filter_map(|line| {
            let name = line
                .strip_prefix("Current week (")?
                .split(')')
                .next()?
                .to_string();
            let window = parse_percent_from_line(line)?;
            Some(json!({"id": name.to_ascii_lowercase(), "name": name, "window": window}))
        })
        .collect::<Vec<_>>();
    Some(json!({
        "current_session": current_session,
        "weekly": weekly,
        "model_limits": model_limits,
        "updated_at_ms": now_ms(),
    }))
}

fn parse_claude_usage_window(lines: &[String], label: &str) -> Option<Value> {
    let index = lines.iter().position(|line| line.starts_with(label))?;
    let mut window = parse_percent_from_line(&lines[index])?;
    let reset = lines
        .iter()
        .skip(index + 1)
        .take(3)
        .find_map(|line| line.strip_prefix("Resets "));
    if let Some(object) = window.as_object_mut() {
        object.insert("resets_at_label".to_string(), json!(reset));
    }
    Some(window)
}

fn parse_percent_from_line(line: &str) -> Option<Value> {
    let marker = line.find("% used")?;
    let used = line[..marker]
        .split(|ch: char| !ch.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .next_back()?
        .parse::<u64>()
        .ok()?
        .min(100);
    Some(json!({
        "used_percent": used,
        "remaining_percent": 100 - used,
        "resets_at_unix": Value::Null,
        "resets_at_label": Value::Null,
        "window_minutes": Value::Null,
    }))
}

async fn create_session(
    State(state): State<AppState>,
    Json(req): Json<CreateSession>,
) -> Result<Json<Value>, ApiError> {
    require_authenticated(state.provider).await?;
    let workspace = canonical_workspace(&req.workspace, &state.workspace_root)?;
    let model = req
        .model
        .as_deref()
        .map(|value| normalize_model_id(state.provider, value))
        .transpose()?;
    let id = uuid::Uuid::new_v4().to_string();
    let requested_vendor_id = if req.fork || req.resume_vendor_session_id.is_none() {
        uuid::Uuid::new_v4().to_string()
    } else {
        req.resume_vendor_session_id
            .clone()
            .expect("resume id checked")
    };
    let events = Arc::new(SyncMutex::new(Vec::new()));
    let runtime = match state.provider {
        Provider::Codex => Runtime::Codex(
            CodexRuntime::spawn(
                &workspace,
                req.resume_vendor_session_id.as_deref(),
                req.fork,
                model.as_deref(),
                events.clone(),
            )
            .await?,
        ),
        Provider::ClaudeCode => {
            let mut runtime = spawn_terminal(
                state.provider,
                req.resume_vendor_session_id.as_deref(),
                req.fork,
                &workspace,
                events.clone(),
                false,
                Some(&requested_vendor_id),
                model.as_deref(),
            )?;
            runtime.wait_ready().await?;
            Runtime::Terminal(runtime)
        }
    };
    let vendor_id = match &runtime {
        Runtime::Codex(runtime) => runtime.thread_id.clone(),
        Runtime::Terminal(_) => requested_vendor_id,
    };
    let meta = SessionMeta {
        id: id.clone(),
        vendor_session_id: vendor_id,
        workspace,
        status: "idle".into(),
        model,
        created_at_ms: now_ms(),
    };
    state.sessions.lock().await.insert(
        id.clone(),
        Session {
            meta: meta.clone(),
            runtime: Some(runtime),
            events,
        },
    );
    persist(&state).await?;
    Ok(Json(json!({"session": meta})))
}

async fn start_turn(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(req): Json<TurnRequest>,
) -> Result<Json<Value>, ApiError> {
    require_authenticated(state.provider).await?;
    if req.prompt.trim().is_empty() {
        return Err(ApiError::bad_request("prompt is empty"));
    }
    if req.prompt.len() > 1024 * 1024 {
        return Err(ApiError::bad_request("prompt exceeds 1 MiB"));
    }
    let mut sessions = state.sessions.lock().await;
    let session = sessions
        .get_mut(&id)
        .ok_or_else(|| ApiError::not_found("session not found"))?;
    if session.runtime.is_none() {
        session.runtime = Some(match state.provider {
            Provider::Codex => Runtime::Codex(
                CodexRuntime::spawn(
                    &session.meta.workspace,
                    Some(&session.meta.vendor_session_id),
                    false,
                    session.meta.model.as_deref(),
                    session.events.clone(),
                )
                .await?,
            ),
            Provider::ClaudeCode => {
                let mut runtime = spawn_terminal(
                    state.provider,
                    Some(&session.meta.vendor_session_id),
                    false,
                    &session.meta.workspace,
                    session.events.clone(),
                    false,
                    None,
                    session.meta.model.as_deref(),
                )?;
                runtime.wait_ready().await?;
                Runtime::Terminal(runtime)
            }
        });
    }
    match session.runtime.as_mut().expect("runtime initialized") {
        Runtime::Codex(runtime) => {
            runtime.request("turn/start", json!({"threadId": session.meta.vendor_session_id, "input": [{"type":"text","text":req.prompt}]})).await?;
        }
        Runtime::Terminal(runtime) => {
            let pasted = format!("\x1b[200~{}\x1b[201~\r", req.prompt);
            runtime.writer.lock().write_all(pasted.as_bytes())?;
            runtime.writer.lock().flush()?;
        }
    }
    session.meta.status = "running".into();
    Ok(Json(json!({"accepted": true})))
}

async fn send_input(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(req): Json<InputRequest>,
) -> Result<Json<Value>, ApiError> {
    if req.text.len() > 64 * 1024 {
        return Err(ApiError::bad_request("terminal input exceeds 64 KiB"));
    }
    let mut sessions = state.sessions.lock().await;
    let session = sessions
        .get_mut(&id)
        .ok_or_else(|| ApiError::not_found("session not found"))?;
    match session.runtime.as_mut() {
        Some(Runtime::Terminal(runtime)) => {
            runtime.writer.lock().write_all(req.text.as_bytes())?;
            runtime.writer.lock().flush()?;
        }
        _ => {
            return Err(ApiError::bad_request(
                "session does not accept terminal input",
            ))
        }
    }
    Ok(Json(json!({"accepted": true})))
}

async fn list_events(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<EventQuery>,
) -> Result<Json<Value>, ApiError> {
    let sessions = state.sessions.lock().await;
    let session = sessions
        .get(&id)
        .ok_or_else(|| ApiError::not_found("session not found"))?;
    let events = session
        .events
        .lock()
        .iter()
        .filter(|event| event.seq > query.after_seq)
        .take(500)
        .cloned()
        .collect::<Vec<_>>();
    Ok(Json(json!({"events": events})))
}

impl CodexRuntime {
    async fn connect(workspace: &str, events: Arc<SyncMutex<Vec<Event>>>) -> Result<Self> {
        let mut child = Command::new("codex")
            .arg("app-server")
            .current_dir(workspace)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()?;
        let stdin = Arc::new(Mutex::new(
            child.stdin.take().context("codex stdin missing")?,
        ));
        let stdout = child.stdout.take().context("codex stdout missing")?;
        let pending = Arc::new(SyncMutex::<HashMap<u64, oneshot::Sender<Value>>>::new(
            HashMap::new(),
        ));
        let reader_pending = pending.clone();
        let reader_events = events.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if let Some(id) = value.get("id").and_then(Value::as_u64) {
                    if value.get("method").is_none() {
                        if let Some(tx) = reader_pending.lock().remove(&id) {
                            let _ = tx.send(value);
                        }
                        continue;
                    }
                }
                push_event(&reader_events, "codex", value);
            }
        });
        let runtime = Self {
            thread_id: String::new(),
            stdin,
            pending,
            next_id: AtomicU64::new(1),
            _child: child,
        };
        runtime.request("initialize", json!({"clientInfo":{"name":"tentaflow","title":"TentaFlow","version":"0.1.0"},"capabilities":{"experimentalApi":true}})).await?;
        runtime.notify("initialized", json!({})).await?;
        Ok(runtime)
    }

    async fn spawn(
        workspace: &str,
        resume: Option<&str>,
        fork: bool,
        model: Option<&str>,
        events: Arc<SyncMutex<Vec<Event>>>,
    ) -> Result<Self> {
        let mut runtime = Self::connect(workspace, events.clone()).await?;
        let response = if let Some(thread_id) = resume {
            let method = if fork { "thread/fork" } else { "thread/resume" };
            runtime
                .request(method, json!({"threadId": thread_id, "model": model}))
                .await?
        } else {
            runtime.request("thread/start", json!({"cwd": workspace, "model": model, "approvalPolicy":"on-request", "sandbox":"workspace-write"})).await?
        };
        let actual_id = response
            .pointer("/result/thread/id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("codex thread response does not contain result.thread.id"))?;
        runtime.thread_id = actual_id.to_owned();
        push_event(&events, "vendor_session", json!({"id":actual_id}));
        Ok(runtime)
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id, tx);
        self.write(json!({"id":id,"method":method,"params":params}))
            .await?;
        let response = tokio::time::timeout(std::time::Duration::from_secs(60), rx)
            .await
            .context("codex RPC timeout")??;
        if let Some(error) = response.get("error") {
            return Err(anyhow!("codex {method}: {error}"));
        }
        Ok(response)
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.write(json!({"method":method,"params":params})).await
    }
    async fn write(&self, value: Value) -> Result<()> {
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(format!("{}\n", value).as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }
}

fn spawn_terminal(
    provider: Provider,
    resume: Option<&str>,
    fork: bool,
    workspace: &str,
    events: Arc<SyncMutex<Vec<Event>>>,
    auth: bool,
    new_session_id: Option<&str>,
    model: Option<&str>,
) -> Result<TerminalRuntime> {
    let pty = native_pty_system().openpty(PtySize {
        rows: 40,
        cols: 120,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut command = CommandBuilder::new(if provider == Provider::Codex {
        "codex"
    } else {
        "claude"
    });
    command.cwd(workspace);
    if let Some(model) = model {
        command.arg("--model");
        command.arg(model);
    }
    if auth {
        if provider == Provider::Codex {
            command.arg("login");
            command.arg("--device-auth");
        } else {
            command.arg("auth");
            command.arg("login");
        }
    } else if let Some(id) = resume {
        command.arg("--resume");
        command.arg(id);
        if fork {
            command.arg("--fork-session");
            command.arg("--session-id");
            command.arg(new_session_id.context("forked Claude session id missing")?);
        }
    } else {
        command.arg("--session-id");
        command.arg(new_session_id.context("new Claude session id missing")?);
    }
    let child = pty.slave.spawn_command(command)?;
    let mut reader = pty.master.try_clone_reader()?;
    let writer = Arc::new(SyncMutex::new(pty.master.take_writer()?));
    let ready = Arc::new(AtomicBool::new(provider == Provider::Codex || auth));
    let reader_writer = writer.clone();
    let reader_ready = ready.clone();
    std::thread::spawn(move || {
        let mut buf = [0_u8; 4096];
        let mut startup = String::new();
        let mut trust_confirmed = false;
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let text = String::from_utf8_lossy(&buf[..n]);
                    startup.push_str(&text);
                    if startup.len() > 65_536 {
                        startup.drain(..32_768);
                    }
                    let plain = terminal_plain_text(&startup);
                    if !trust_confirmed && plain.contains("Quick safety check") {
                        let mut input = reader_writer.lock();
                        let _ = input.write_all(b"\r");
                        let _ = input.flush();
                        trust_confirmed = true;
                    }
                    if plain.contains("for shortcuts") || plain.contains("bypass permissions on") {
                        reader_ready.store(true, Ordering::Release);
                    }
                    push_event(&events, "terminal", json!({"text":text}));
                }
            }
        }
    });
    Ok(TerminalRuntime {
        writer,
        ready,
        _master: pty.master,
        _child: child,
    })
}

fn terminal_plain_text(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut text = String::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == 0x1b {
            index += 1;
            if index < bytes.len() && bytes[index] == b'[' {
                index += 1;
                while index < bytes.len() {
                    let byte = bytes[index];
                    index += 1;
                    if (0x40..=0x7e).contains(&byte) {
                        break;
                    }
                }
            } else if index < bytes.len() && bytes[index] == b']' {
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == 0x07 {
                        index += 1;
                        break;
                    }
                    if bytes[index] == 0x1b && index + 1 < bytes.len() && bytes[index + 1] == b'\\'
                    {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
            } else {
                index += usize::from(index < bytes.len());
            }
            text.push(' ');
        } else {
            let byte = bytes[index];
            index += 1;
            if byte.is_ascii_graphic() || byte == b' ' {
                text.push(byte as char);
            } else {
                text.push(' ');
            }
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse_claude_models(raw: &[u8]) -> Vec<Value> {
    let mut parser = vt100::Parser::new(40, 120, 0);
    parser.process(raw);
    parser
        .screen()
        .contents()
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim().trim_start_matches('❯').trim();
            let (number, label) = trimmed.split_once(". ")?;
            let index = number.parse::<usize>().ok()?;
            let selected = label.contains('✔');
            let display_name = label
                .split("  ")
                .next()?
                .trim_end_matches('✔')
                .trim()
                .to_string();
            let model = display_name.split_whitespace().next()?.to_ascii_lowercase();
            Some(json!({
                "id": model,
                "display_name": display_name,
                "selected": selected,
                "selector_index": index,
            }))
        })
        .collect()
}

impl TerminalRuntime {
    async fn wait_ready(&mut self) -> Result<()> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        while !self.ready.load(Ordering::Acquire) {
            if tokio::time::Instant::now() >= deadline {
                return Err(anyhow!(
                    "Claude Code did not reach the prompt within 30 seconds"
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        Ok(())
    }
}

fn push_event(events: &Arc<SyncMutex<Vec<Event>>>, kind: &str, data: Value) {
    let mut list = events.lock();
    let seq = list.last().map_or(1, |e| e.seq + 1);
    list.push(Event {
        seq,
        kind: kind.into(),
        data,
    });
    if list.len() > 10_000 {
        list.drain(..1_000);
    }
}

fn canonical_workspace(raw: &str, allowed_root: &Path) -> Result<String, ApiError> {
    let path = std::fs::canonicalize(raw)
        .map_err(|e| ApiError::bad_request(&format!("invalid workspace: {e}")))?;
    if !path.is_dir() {
        return Err(ApiError::bad_request("workspace is not a directory"));
    }
    if !path.starts_with(allowed_root) {
        return Err(ApiError::bad_request(
            "workspace is outside the configured workspace root",
        ));
    }
    Ok(path.to_string_lossy().into_owned())
}

fn normalize_model_id(provider: Provider, raw: &str) -> Result<String, ApiError> {
    let trimmed = raw.trim();
    let prefix = if provider == Provider::Codex {
        "codex/"
    } else {
        "claude-code/"
    };
    let model = trimmed.strip_prefix(prefix).unwrap_or(trimmed);
    if model.is_empty() || model.len() > 256 || model.chars().any(char::is_control) {
        return Err(ApiError::bad_request("invalid model id"));
    }
    Ok(model.to_string())
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

struct ApiError(StatusCode, String);
impl ApiError {
    fn bad_request(s: &str) -> Self {
        Self(StatusCode::BAD_REQUEST, s.into())
    }
    fn not_found(s: &str) -> Self {
        Self(StatusCode::NOT_FOUND, s.into())
    }
    fn internal(s: &str) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, s.into())
    }
    fn unauthorized(s: &str) -> Self {
        Self(StatusCode::UNAUTHORIZED, s.into())
    }
}
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    }
}
impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    }
}
impl axum::response::IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (self.0, Json(json!({"error":self.1}))).into_response()
    }
}
