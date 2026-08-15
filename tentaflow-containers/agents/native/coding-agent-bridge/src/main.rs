mod process;

use std::{
    collections::{HashMap, HashSet},
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
    routing::{delete, get, post},
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
    /// Whether this session's CLI was started with caller-supplied wiring
    /// (§7.5). Only the FACT is persisted: the wiring carries a ticket, which
    /// is a bearer secret that dies with its run and has no business in a state
    /// file. A wired session therefore cannot be lazily re-spawned after a
    /// bridge restart — see `start_turn`.
    #[serde(default)]
    env_wired: bool,
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
    Claude(ClaudeRuntime),
    /// A PTY. It is what the vendor login flow needs (a device code typed into
    /// a real terminal) and what reading Claude Code's `/usage` still costs —
    /// that slash command exists only inside an interactive session. No
    /// delegated turn runs here any more.
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
    probe_file: PathBuf,
    models_file: PathBuf,
    probe: Arc<Mutex<ProbeCache>>,
    /// Serializes every probe. Held across the whole CLI interaction, so
    /// concurrent callers queue up and the second one finds the cache filled
    /// instead of starting a second session.
    probe_lock: Arc<Mutex<()>>,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    processes: Arc<process::Registry>,
}

/// Asking Claude Code for its rate limits means driving an interactive session —
/// there is no non-interactive readout. Two things keep that from piling up in
/// the user's session history: answers are cached, and every probe reuses ONE
/// session id instead of minting a fresh UUID. The model list does not appear
/// here at all any more: it is configuration (see `configured_claude_models`),
/// because a list that costs a vendor session is not a list worth having.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct ProbeCache {
    #[serde(default)]
    probe_session_id: Option<String>,
    #[serde(default)]
    models: Vec<Value>,
    #[serde(default)]
    models_fetched_at_ms: u128,
    /// Rate limits move, so they are never persisted and expire in a minute.
    #[serde(default, skip)]
    usage: Option<Value>,
    #[serde(default, skip)]
    usage_fetched_at_ms: u128,
}

/// A CLI gains models when the vendor ships a release, not during the day.
const MODELS_TTL_MS: u128 = 24 * 60 * 60 * 1000;
const USAGE_TTL_MS: u128 = 60 * 1000;

impl ProbeCache {
    fn models_are_fresh(&self, now_ms: u128) -> bool {
        !self.models.is_empty() && now_ms.saturating_sub(self.models_fetched_at_ms) < MODELS_TTL_MS
    }

    fn usage_is_fresh(&self, now_ms: u128) -> bool {
        self.usage.is_some() && now_ms.saturating_sub(self.usage_fetched_at_ms) < USAGE_TTL_MS
    }
}

struct CodexRuntime {
    thread_id: String,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Arc<SyncMutex<HashMap<u64, oneshot::Sender<Value>>>>,
    /// Server→client requests the app-server is BLOCKED on. A turn does not
    /// continue until each of them is answered, so the set is what tells
    /// `send_approval` that an id is real and `close_session` what it still owes
    /// an answer to (defect D3 of §1.2).
    approvals: Arc<SyncMutex<HashSet<u64>>>,
    next_id: AtomicU64,
    /// Kept so the app-server is killed as a GROUP and reaped, not merely
    /// dropped: `codex` starts helpers of its own.
    handle: process::Handle,
    _child: Child,
}

/// Claude Code driven through its programmatic mode.
///
/// `--print --output-format=stream-json --input-format=stream-json --verbose`
/// reads one JSON user message per line from stdin and writes one JSON object
/// per line to stdout, closing every turn with a `result` object. The session
/// used to be a PTY running the interactive TUI instead, which had two
/// consequences the caller could not work around: the transcript was a stream
/// of ANSI frames, and NOTHING in it said whether the turn was over — so a
/// delegation ran to its timeout even when the CLI had long since answered.
struct ClaudeRuntime {
    /// `None` once the turn stream has been closed; EOF on stdin is how this
    /// mode is asked to stop.
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    /// Killed as a GROUP: the CLI starts helpers (MCP servers, tools) of its
    /// own, and killing the direct child alone orphans them (D2).
    handle: process::Handle,
    child: Child,
}

struct TerminalRuntime {
    writer: Arc<SyncMutex<Box<dyn Write + Send>>>,
    ready: Arc<AtomicBool>,
    _master: Box<dyn MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    /// Pid record + group kill. Dropping the PTY master does not terminate the
    /// CLI — it keeps running against a closed terminal — and killing the direct
    /// child leaves everything the CLI spawned attached to nothing (D2).
    handle: process::Handle,
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
    /// Environment the CLI process is started with, on top of the bridge's own.
    ///
    /// This is how Core points the CLI at its provider adapter and hands it a
    /// ticket instead of a credential (plan §7.5): the base URL override, the
    /// ticket as the API key and the session CA all arrive here. The bridge
    /// does not interpret any of it — it is opaque wiring owned by the caller,
    /// which is loopback-only Core.
    #[serde(default)]
    env: std::collections::BTreeMap<String, String>,
    /// Arguments the CLI is started with, on top of the ones the bridge needs
    /// for its own protocol.
    ///
    /// The second half of the same wiring: codex ignores `OPENAI_BASE_URL`, and
    /// the only thing that moves it onto the adapter is a provider configured
    /// with `-c model_providers.*` at startup. As with `env`, the bridge does
    /// not interpret any of it.
    #[serde(default)]
    args: Vec<String>,
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

#[derive(Deserialize)]
struct RefreshQuery {
    #[serde(default)]
    refresh: bool,
}

#[derive(Deserialize)]
struct ApprovalRequest {
    request_id: u64,
    decision: String,
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
    let probe_file = data_dir.join("probe-cache.json");
    let models_file = data_dir.join("models.json");
    let sessions = load_sessions(&state_file)?;
    let probe = load_probe_cache(&probe_file);
    let workspace_root =
        std::fs::canonicalize(env::var("TENTAFLOW_WORKSPACE_ROOT").unwrap_or_else(|_| ".".into()))?;
    // Before anything is served: a CLI from a crashed bridge still holds the
    // workspace and its vendor session, and a second one started next to it
    // would fight over both (D2).
    let processes = process::Registry::new(&data_dir)?;
    for orphan in processes.reap_orphans() {
        eprintln!(
            "coding-agent-bridge: orphan {} (pid {}) from a previous life is {}",
            orphan.kind,
            orphan.pid,
            orphan.state.as_str()
        );
    }
    let state = AppState {
        provider,
        workspace_root,
        state_file,
        probe_file,
        models_file,
        probe: Arc::new(Mutex::new(probe)),
        probe_lock: Arc::new(Mutex::new(())),
        sessions: Arc::new(Mutex::new(sessions)),
        processes: Arc::new(processes),
    };
    let app = Router::new()
        .route("/health", get(health))
        .route("/auth/status", get(auth_status))
        .route("/auth/start", post(auth_start))
        .route("/models", get(list_models))
        .route("/usage", get(usage))
        .route("/sessions", get(list_sessions).post(create_session))
        .route("/sessions/{id}", delete(close_session))
        .route("/sessions/{id}/turn", post(start_turn))
        .route("/sessions/{id}/input", post(send_input))
        .route("/sessions/{id}/approval", post(send_approval))
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

fn load_probe_cache(path: &Path) -> ProbeCache {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => ProbeCache::default(),
    }
}

async fn persist_probe_cache(state: &AppState) -> Result<()> {
    let cache = state.probe.lock().await.clone();
    let tmp = state.probe_file.with_extension("tmp");
    tokio::fs::write(&tmp, serde_json::to_vec(&cache)?).await?;
    tokio::fs::rename(tmp, &state.probe_file).await?;
    Ok(())
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
        TerminalSpawn {
            provider: state.provider,
            workspace: &workspace,
            resume: None,
            auth: true,
            new_session_id: None,
        },
        events.clone(),
        &state.processes,
    )?;
    let meta = SessionMeta {
        id: id.clone(),
        vendor_session_id: id.clone(),
        workspace,
        status: "authenticating".into(),
        model: None,
        env_wired: false,
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

/// Lists the models the engine can be asked for. **This call never creates a
/// vendor session** (defect D1 of §1.2), and the two providers reach that the
/// same promise by different routes:
///
///   * Codex answers `model/list` on the app-server, which is a process, not a
///     thread — nothing appears in the user's history. The answer is still
///     cached, because starting an app-server per call is waste, not a session.
///   * Claude Code has no non-interactive listing at all: `/model` is a slash
///     command inside a running session, and driving it is exactly what used to
///     add ~12 sessions an hour. Until Phase 0B (§17.1 point 5) proves a
///     session-free command exists, the list is CONFIGURATION — see
///     `configured_claude_models`. `refresh=1` re-reads that file; it does not
///     start a CLI.
async fn list_models(
    State(state): State<AppState>,
    Query(query): Query<RefreshQuery>,
) -> Result<Json<Value>, ApiError> {
    if state.provider == Provider::ClaudeCode {
        let (models, source) = configured_claude_models(&state.models_file)?;
        return Ok(Json(
            json!({"models": models, "cached": true, "source": source}),
        ));
    }
    if !query.refresh && state.probe.lock().await.models_are_fresh(now_ms()) {
        let cache = state.probe.lock().await;
        return Ok(Json(
            json!({"models": cache.models, "cached": true, "source": "cli"}),
        ));
    }
    require_authenticated(state.provider).await?;
    let _probe = state.probe_lock.lock().await;
    // Whoever waited on the lock may have been waiting for the probe that just
    // filled the cache; asking the CLI again would only spend another process.
    if !query.refresh && state.probe.lock().await.models_are_fresh(now_ms()) {
        let cache = state.probe.lock().await;
        return Ok(Json(
            json!({"models": cache.models, "cached": true, "source": "cli"}),
        ));
    }
    let events = Arc::new(SyncMutex::new(Vec::new()));
    let runtime = CodexRuntime::connect(
        &state.workspace_root.to_string_lossy(),
        &[],
        &[],
        events,
        &state.processes,
    )
    .await?;
    let response = runtime
        .request("model/list", json!({"includeHidden": false}))
        .await?;
    let result = response.get("result").unwrap_or(&response);
    let models = result
        .get("models")
        .or_else(|| result.get("data"))
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| ApiError::internal("codex model/list did not return an array"))?;
    {
        let mut cache = state.probe.lock().await;
        cache.models = models.clone();
        cache.models_fetched_at_ms = now_ms();
    }
    if let Err(error) = persist_probe_cache(&state).await {
        eprintln!("coding-agent-bridge: probe cache write failed: {error}");
    }
    Ok(Json(
        json!({"models": models, "cached": false, "source": "cli"}),
    ))
}

/// Model aliases the pinned Claude Code release accepts on `--model`. They are
/// aliases rather than dated ids on purpose: the alias is what the CLI resolves
/// against the account's entitlements, so it stays correct when the vendor ships
/// a new snapshot, and it is byte for byte what the previous screen-scraper
/// produced — the ids already stored in `models` do not move.
const CLAUDE_CODE_PINNED_VERSION: &str = "2.1.221";
const CLAUDE_CODE_MODEL_ALIASES: [(&str, &str, bool); 4] = [
    ("opus", "Opus", false),
    ("sonnet", "Sonnet", true),
    ("haiku", "Haiku", false),
    ("opusplan", "Opus plan / Sonnet execute", false),
];

/// The configured Claude Code catalog: the operator's `models.json` when the
/// deployment has one, otherwise the aliases of the pinned release.
///
/// This is deliberately not a probe. The honest statement is in the return
/// value: `source` says `file` or `pinned:<version>`, so nobody reads this list
/// as "what the CLI reported". An entitlement the account does not have fails at
/// the turn, where the vendor's own error is the accurate answer — that is
/// strictly better than minting a session per refresh to find out.
fn configured_claude_models(models_file: &Path) -> Result<(Vec<Value>, String), ApiError> {
    match std::fs::read(models_file) {
        Ok(bytes) => {
            let parsed: Vec<Value> = serde_json::from_slice(&bytes).map_err(|error| {
                ApiError::internal(&format!(
                    "{} is not a JSON array of models: {error}",
                    models_file.display()
                ))
            })?;
            if parsed.is_empty() {
                return Err(ApiError::internal(&format!(
                    "{} lists no models",
                    models_file.display()
                )));
            }
            for model in &parsed {
                let id = model.get("id").and_then(Value::as_str).unwrap_or_default();
                if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
                    return Err(ApiError::internal(&format!(
                        "{} contains a model without a usable id",
                        models_file.display()
                    )));
                }
            }
            Ok((parsed, "file".to_string()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok((
            CLAUDE_CODE_MODEL_ALIASES
                .iter()
                .map(|(id, display_name, selected)| {
                    json!({"id": id, "display_name": display_name, "selected": selected})
                })
                .collect(),
            format!("pinned:{CLAUDE_CODE_PINNED_VERSION}"),
        )),
        Err(error) => Err(ApiError::internal(&format!(
            "cannot read {}: {error}",
            models_file.display()
        ))),
    }
}

async fn usage(
    State(state): State<AppState>,
    Query(query): Query<RefreshQuery>,
) -> Result<Json<Value>, ApiError> {
    if !query.refresh {
        let cache = state.probe.lock().await;
        if cache.usage_is_fresh(now_ms()) {
            return Ok(Json(
                cache.usage.clone().expect("freshness implies a value"),
            ));
        }
    }
    require_authenticated(state.provider).await?;
    let _probe = state.probe_lock.lock().await;
    if !query.refresh {
        let cache = state.probe.lock().await;
        if cache.usage_is_fresh(now_ms()) {
            return Ok(Json(
                cache.usage.clone().expect("freshness implies a value"),
            ));
        }
    }
    let usage = match state.provider {
        Provider::Codex => {
            let events = Arc::new(SyncMutex::new(Vec::new()));
            let runtime = CodexRuntime::connect(
                &state.workspace_root.to_string_lossy(),
                &[],
                &[],
                events,
                &state.processes,
            )
            .await?;
            let response = runtime
                .request("account/rateLimits/read", Value::Null)
                .await?;
            let result = response
                .get("result")
                .ok_or_else(|| ApiError::internal("Codex usage response has no result"))?;
            normalize_codex_usage(result)
        }
        Provider::ClaudeCode => {
            // `/usage` is a slash command, so reading it means being IN a
            // session. That cost is only ever paid on an explicit request: a
            // caller that merely wants to display limits gets the honest
            // "unavailable" instead of a session created behind the user's back.
            if !query.refresh {
                return Ok(Json(json!({
                    "available": false,
                    "reason": "claude_code_usage_needs_a_session",
                    "detail": "Claude Code reports rate limits only through the /usage slash \
                               command inside a running session. Ask again with refresh=1 to \
                               accept that one session in the vendor history.",
                    "updated_at_ms": now_ms(),
                })));
            }
            claude_probe(&state, "/usage\r", 15, parse_claude_usage).await?
        }
    };
    {
        let mut cache = state.probe.lock().await;
        cache.usage = Some(usage.clone());
        cache.usage_fetched_at_ms = now_ms();
    }
    Ok(Json(usage))
}

/// Drives a Claude Code slash command in the **reused probe session**, so a
/// repeated explicit read does not add an entry to the vendor's session history
/// every time. A probe id the CLI no longer knows falls back to a fresh session
/// and is remembered for next time. The only caller is `/usage?refresh=1`.
async fn claude_probe<T>(
    state: &AppState,
    slash_command: &str,
    timeout_secs: u64,
    parse: impl Fn(&[u8]) -> Option<T> + Copy,
) -> Result<T, ApiError> {
    let previous = state.probe.lock().await.probe_session_id.clone();
    if let Some(id) = previous {
        match claude_slash_command(state, Some(&id), None, slash_command, timeout_secs, parse).await
        {
            Ok(value) => return Ok(value),
            Err(ApiError(_, error)) => eprintln!(
                "coding-agent-bridge: probe session {id} unusable ({error}), starting a new one"
            ),
        }
    }
    let fresh = uuid::Uuid::new_v4().to_string();
    let value = claude_slash_command(
        state,
        None,
        Some(&fresh),
        slash_command,
        timeout_secs,
        parse,
    )
    .await?;
    state.probe.lock().await.probe_session_id = Some(fresh);
    Ok(value)
}

async fn claude_slash_command<T>(
    state: &AppState,
    resume: Option<&str>,
    new_session_id: Option<&str>,
    slash_command: &str,
    timeout_secs: u64,
    parse: impl Fn(&[u8]) -> Option<T>,
) -> Result<T, ApiError> {
    let events = Arc::new(SyncMutex::new(Vec::new()));
    let mut runtime = spawn_terminal(
        TerminalSpawn {
            provider: state.provider,
            workspace: &state.workspace_root.to_string_lossy(),
            resume,
            auth: false,
            new_session_id,
        },
        events.clone(),
        &state.processes,
    )?;
    runtime.wait_ready().await?;
    events.lock().clear();
    runtime.writer.lock().write_all(slash_command.as_bytes())?;
    runtime.writer.lock().flush()?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        let raw = events
            .lock()
            .iter()
            .filter_map(|event| event.data.get("text").and_then(Value::as_str))
            .collect::<String>();
        if let Some(value) = parse(raw.as_bytes()) {
            runtime.shutdown().await;
            return Ok(value);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(ApiError::internal(&format!(
                "Claude Code {} produced no usable answer",
                slash_command.trim_end()
            )));
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
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
    let env = validated_env(&req.env)?;
    let args = validated_args(&req.args)?;
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
                &env,
                &args,
                events.clone(),
                &state.processes,
            )
            .await?,
        ),
        Provider::ClaudeCode => Runtime::Claude(
            ClaudeRuntime::spawn(ClaudeSpawn {
                workspace: &workspace,
                resume: req.resume_vendor_session_id.as_deref(),
                fork: req.fork,
                new_session_id: Some(&requested_vendor_id),
                model: model.as_deref(),
                env: &env,
                args: &args,
                events: events.clone(),
                processes: &state.processes,
            })
            .await?,
        ),
    };
    let vendor_id = match &runtime {
        Runtime::Codex(runtime) => runtime.thread_id.clone(),
        // The id we asked for. Claude Code confirms the one it really used in
        // its `system/init` object, which the reader forwards as a
        // `vendor_session` event, so a CLI that chose differently still lands in
        // the caller's record.
        Runtime::Claude(_) | Runtime::Terminal(_) => requested_vendor_id,
    };
    let meta = SessionMeta {
        id: id.clone(),
        vendor_session_id: vendor_id,
        workspace,
        status: "idle".into(),
        model,
        env_wired: !env.is_empty(),
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
        // A session started with caller-supplied wiring cannot be revived here:
        // the wiring held a ticket, the ticket was never persisted, and a CLI
        // brought back without it would talk to the provider through whatever
        // credential this bridge's own environment happens to carry. The caller
        // opens a new session — and mints a new ticket — instead.
        if session.meta.env_wired {
            return Err(ApiError::bad_request(
                "this session was started with caller-supplied wiring, which is not persisted;                  open a new session",
            ));
        }
        session.runtime = Some(match state.provider {
            Provider::Codex => Runtime::Codex(
                CodexRuntime::spawn(
                    &session.meta.workspace,
                    Some(&session.meta.vendor_session_id),
                    false,
                    session.meta.model.as_deref(),
                    &[],
                    &[],
                    session.events.clone(),
                    &state.processes,
                )
                .await?,
            ),
            Provider::ClaudeCode => Runtime::Claude(
                ClaudeRuntime::spawn(ClaudeSpawn {
                    workspace: &session.meta.workspace,
                    resume: Some(&session.meta.vendor_session_id),
                    fork: false,
                    new_session_id: None,
                    model: session.meta.model.as_deref(),
                    env: &[],
                    args: &[],
                    events: session.events.clone(),
                    processes: &state.processes,
                })
                .await?,
            ),
        });
    }
    match session.runtime.as_mut().expect("runtime initialized") {
        Runtime::Codex(runtime) => {
            runtime.request("turn/start", json!({"threadId": session.meta.vendor_session_id, "input": [{"type":"text","text":req.prompt}]})).await?;
        }
        Runtime::Claude(runtime) => runtime.turn(&req.prompt).await?,
        // A PTY session is a login, and a login has no turns.
        Runtime::Terminal(_) => {
            return Err(ApiError::bad_request("this session does not accept turns"))
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

/// Answers a server→client approval request. Codex threads are started with
/// `approvalPolicy: "on-request"`, so without this path every turn that wants
/// to touch the filesystem or run a command waits until it times out.
async fn send_approval(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(req): Json<ApprovalRequest>,
) -> Result<Json<Value>, ApiError> {
    const DECISIONS: [&str; 4] = ["approved", "approved_for_session", "denied", "abort"];
    if !DECISIONS.contains(&req.decision.as_str()) {
        return Err(ApiError::bad_request("unsupported approval decision"));
    }
    let mut sessions = state.sessions.lock().await;
    let session = sessions
        .get_mut(&id)
        .ok_or_else(|| ApiError::not_found("session not found"))?;
    match session.runtime.as_mut() {
        Some(Runtime::Codex(runtime)) => {
            runtime
                .answer_approval(req.request_id, &req.decision)
                .await?;
        }
        _ => return Err(ApiError::bad_request("session does not use approvals")),
    }
    Ok(Json(json!({"accepted": true})))
}

/// Terminates the session's CLI process and forgets it. Without an explicit
/// close, a login window or a finished session keeps its child alive for the
/// lifetime of the bridge.
///
/// The reply carries the settled process state, so the caller can record
/// `reaped` in `cli_instances` instead of assuming it (§5.3, D2).
async fn close_session(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, ApiError> {
    let session = {
        let mut sessions = state.sessions.lock().await;
        sessions.remove(&id)
    };
    let Some(mut session) = session else {
        return Ok(Json(json!({"closed": false})));
    };
    let state_after = match session.runtime.as_mut() {
        Some(Runtime::Terminal(runtime)) => runtime.shutdown().await,
        Some(Runtime::Codex(runtime)) => runtime.shutdown().await,
        Some(Runtime::Claude(runtime)) => runtime.shutdown().await,
        // A session whose runtime was never restarted after a bridge restart
        // has nothing running; the process it once had was reaped at startup.
        None => process::ProcessState::Reaped,
    };
    persist(&state).await?;
    Ok(Json(
        json!({"closed": true, "process_state": state_after.as_str()}),
    ))
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
    async fn connect(
        workspace: &str,
        env: &[(String, String)],
        args: &[String],
        events: Arc<SyncMutex<Vec<Event>>>,
        processes: &process::Registry,
    ) -> Result<Self> {
        let mut command = Command::new("codex");
        command
            .arg("app-server")
            // The caller's provider configuration, given where the app-server
            // reads it: `-c` overrides at startup. Passing it as environment
            // would be passing it nowhere — `OPENAI_BASE_URL` is ignored.
            .args(args)
            .envs(env.iter().map(|(name, value)| (name, value)))
            .current_dir(workspace)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            // A dropped runtime (finished probe, closed session) must take the
            // app-server with it; tokio kills and reaps it in the background.
            .kill_on_drop(true);
        // Its own group, so the sandboxed tools the app-server starts are
        // reachable by one signal instead of being orphaned (D2).
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn()?;
        let handle = processes.track(
            "codex-app-server",
            child.id().context("codex app-server has no pid")?,
        );
        let stdin = Arc::new(Mutex::new(
            child.stdin.take().context("codex stdin missing")?,
        ));
        let stdout = child.stdout.take().context("codex stdout missing")?;
        let pending = Arc::new(SyncMutex::<HashMap<u64, oneshot::Sender<Value>>>::new(
            HashMap::new(),
        ));
        let approvals = Arc::new(SyncMutex::<HashSet<u64>>::new(HashSet::new()));
        let reader_pending = pending.clone();
        let reader_approvals = approvals.clone();
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
                    // Server→client request. It BLOCKS the vendor turn until we
                    // answer, so it gets its own event kind carrying the id the
                    // operator has to answer on — as a plain `codex` event the
                    // turn would wait forever. The id is remembered so an answer
                    // can be matched to a request that is really outstanding,
                    // and so closing the session can settle the rest (D3).
                    reader_approvals.lock().insert(id);
                    push_event(
                        &reader_events,
                        "approval_request",
                        json!({
                            "request_id": id,
                            "method": value.get("method").cloned().unwrap_or(Value::Null),
                            "params": value.get("params").cloned().unwrap_or(Value::Null),
                        }),
                    );
                    continue;
                }
                push_event(&reader_events, "codex", value);
            }
        });
        let runtime = Self {
            thread_id: String::new(),
            stdin,
            pending,
            approvals,
            next_id: AtomicU64::new(1),
            handle,
            _child: child,
        };
        runtime.request("initialize", json!({"clientInfo":{"name":"tentaflow","title":"TentaFlow","version":"0.1.0"},"capabilities":{"experimentalApi":true}})).await?;
        runtime.notify("initialized", json!({})).await?;
        Ok(runtime)
    }

    #[allow(clippy::too_many_arguments)]
    async fn spawn(
        workspace: &str,
        resume: Option<&str>,
        fork: bool,
        model: Option<&str>,
        env: &[(String, String)],
        args: &[String],
        events: Arc<SyncMutex<Vec<Event>>>,
        processes: &process::Registry,
    ) -> Result<Self> {
        let mut runtime = Self::connect(workspace, env, args, events.clone(), processes).await?;
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

    /// Answers one outstanding server→client request. An id that is not
    /// outstanding is refused rather than written: the app-server would ignore
    /// the response, and the caller would believe a turn was unblocked when it
    /// was not (D3).
    async fn answer_approval(&self, request_id: u64, decision: &str) -> Result<(), ApiError> {
        if !self.approvals.lock().remove(&request_id) {
            return Err(ApiError::not_found(
                "no approval request is outstanding under that id",
            ));
        }
        if let Err(error) = self
            .write(json!({"id": request_id, "result": {"decision": decision}}))
            .await
        {
            // Put it back: the turn is still blocked, so the operator must be
            // able to answer again.
            self.approvals.lock().insert(request_id);
            return Err(ApiError::internal(&format!(
                "codex approval response failed: {error}"
            )));
        }
        Ok(())
    }

    /// Denies whatever is still outstanding. Called when a session goes away:
    /// an unanswered request leaves the CLI blocked, and a blocked CLI is a
    /// process that never exits.
    async fn settle_pending_approvals(&self) {
        let outstanding: Vec<u64> = self.approvals.lock().drain().collect();
        for request_id in outstanding {
            if let Err(error) = self
                .write(json!({"id": request_id, "result": {"decision": "denied"}}))
                .await
            {
                eprintln!("coding-agent-bridge: settling approval {request_id} failed: {error}");
            }
        }
    }

    /// Denies what is outstanding, then kills and reaps the app-server group.
    async fn shutdown(&mut self) -> process::ProcessState {
        self.settle_pending_approvals().await;
        self.handle.terminate()
    }
    async fn write(&self, value: Value) -> Result<()> {
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(format!("{}\n", value).as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }
}

impl ClaudeRuntime {
    /// Starts the CLI. What it may DO is the vendor's own decision here: this
    /// mode routes tool permissions only to a `--permission-prompt-tool`, which
    /// is an MCP server the bridge does not run, so no approval of this engine
    /// reaches the session's PEP the way a Codex `approval_request` does. The
    /// turn ends either way — its `result` object says what happened — and no
    /// flag is passed that would widen what the CLI may do without a person
    /// deciding it.
    async fn spawn(spawn: ClaudeSpawn<'_>) -> Result<Self> {
        let ClaudeSpawn {
            workspace,
            resume,
            fork,
            new_session_id,
            model,
            env,
            args,
            events,
            processes,
        } = spawn;
        let mut command = Command::new("claude");
        command.args(claude_args(resume, fork, new_session_id, model, args)?);
        command
            .envs(env.iter().map(|(name, value)| (name, value)))
            .current_dir(workspace)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn()?;
        let handle = processes.track("claude-print", child.id().context("claude has no pid")?);
        let stdin = Arc::new(Mutex::new(Some(
            child.stdin.take().context("claude stdin missing")?,
        )));
        let stdout = child.stdout.take().context("claude stdout missing")?;
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    // A line that is not JSON is not part of the protocol. It is
                    // recorded as terminal output rather than dropped, so a
                    // startup complaint the CLI prints on stdout is still
                    // visible to whoever is reading the session.
                    push_event(&events, "terminal", json!({ "text": line }));
                    continue;
                };
                // The CLI announces the id it actually used; it is what
                // `--resume` needs, and taking it from the vendor rather than
                // from our own request is what makes a resume survive a CLI that
                // chose differently.
                if value.get("type").and_then(Value::as_str) == Some("system")
                    && value.get("subtype").and_then(Value::as_str) == Some("init")
                {
                    if let Some(id) = value.get("session_id").and_then(Value::as_str) {
                        push_event(&events, "vendor_session", json!({ "id": id }));
                    }
                }
                push_event(&events, "claude", value);
            }
        });
        Ok(Self {
            stdin,
            handle,
            child,
        })
    }

    /// Starts one turn by writing a single user message. The process stays
    /// alive between turns, which is what `session.turn` on an open session
    /// means (§17.2: one long-lived instance per `cli_instances` row).
    async fn turn(&self, prompt: &str) -> Result<(), ApiError> {
        let message = json!({
            "type": "user",
            "message": {"role": "user", "content": [{"type": "text", "text": prompt}]},
        });
        let mut guard = self.stdin.lock().await;
        let stdin = guard
            .as_mut()
            .ok_or_else(|| ApiError::bad_request("this session's input stream is closed"))?;
        let write = async {
            stdin.write_all(format!("{message}\n").as_bytes()).await?;
            stdin.flush().await
        };
        write
            .await
            .map_err(|error| ApiError::internal(&format!("claude stdin write failed: {error}")))
    }

    /// Closes the input stream, gives the CLI its chance to finish and exit, and
    /// kills the group if it does not. The polite step is not politeness: the
    /// session transcript `--resume` reads is written on exit, and a straight
    /// SIGKILL loses it.
    async fn shutdown(&mut self) -> process::ProcessState {
        self.stdin.lock().await.take();
        match tokio::time::timeout(std::time::Duration::from_secs(5), self.child.wait()).await {
            Ok(Ok(_)) => {
                self.handle.mark_exited();
                process::ProcessState::Exited
            }
            // Either the wait failed or the CLI is still running; both are
            // settled the same way, while the pid still names our group.
            _ => self.handle.terminate(),
        }
    }
}

/// The command line one Claude Code session runs on.
///
/// A function of its own so the protocol flags can be asserted without starting
/// a process: they ARE the contract with `cli_bridge`, which parses the stream
/// they produce. `--verbose` is not optional here — without it the stream
/// carries only the final result, and the session timeline would show a turn
/// with no work in it.
fn claude_args(
    resume: Option<&str>,
    fork: bool,
    new_session_id: Option<&str>,
    model: Option<&str>,
    extra: &[String],
) -> Result<Vec<String>> {
    let mut args = vec![
        "--print".to_string(),
        "--output-format=stream-json".to_string(),
        "--input-format=stream-json".to_string(),
        "--verbose".to_string(),
    ];
    if let Some(model) = model {
        args.extend(["--model".to_string(), model.to_string()]);
    }
    match (resume, fork) {
        (Some(id), false) => args.extend(["--resume".to_string(), id.to_string()]),
        (Some(id), true) => args.extend([
            "--resume".to_string(),
            id.to_string(),
            "--fork-session".to_string(),
            "--session-id".to_string(),
            new_session_id
                .context("forked Claude session id missing")?
                .to_string(),
        ]),
        (None, _) => args.extend([
            "--session-id".to_string(),
            new_session_id
                .context("new Claude session id missing")?
                .to_string(),
        ]),
    }
    args.extend(extra.iter().cloned());
    Ok(args)
}

/// Everything one Claude Code start needs.
struct ClaudeSpawn<'a> {
    workspace: &'a str,
    resume: Option<&'a str>,
    fork: bool,
    new_session_id: Option<&'a str>,
    model: Option<&'a str>,
    /// Caller-owned wiring for the CLI process (§7.5): the adapter as base URL,
    /// the ticket as the API key, a session-private configuration directory.
    env: &'a [(String, String)],
    /// Caller-owned startup arguments (§7.5). Empty for Claude Code today, whose
    /// provider is an environment variable; the bridge does not decide which of
    /// the two an engine needs.
    args: &'a [String],
    events: Arc<SyncMutex<Vec<Event>>>,
    processes: &'a process::Registry,
}

/// Everything one PTY start needs.
///
/// Only two callers are left, and both drive a real terminal on purpose: the
/// vendor login flow (a device code typed by a person) and the `/usage` probe
/// (a slash command that exists nowhere else). Neither carries caller wiring,
/// so there is no `env` here — a ticket has no business in a login window.
struct TerminalSpawn<'a> {
    provider: Provider,
    workspace: &'a str,
    resume: Option<&'a str>,
    auth: bool,
    new_session_id: Option<&'a str>,
}

fn spawn_terminal(
    spawn: TerminalSpawn<'_>,
    events: Arc<SyncMutex<Vec<Event>>>,
    processes: &process::Registry,
) -> Result<TerminalRuntime> {
    let TerminalSpawn {
        provider,
        workspace,
        resume,
        auth,
        new_session_id,
    } = spawn;
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
    } else {
        command.arg("--session-id");
        command.arg(new_session_id.context("new Claude session id missing")?);
    }
    let child = pty.slave.spawn_command(command)?;
    // The PTY backend makes the child a session leader, so its pid is also its
    // process group id: one `killpg` reaches the helpers the CLI spawns.
    let handle = processes.track(
        if auth { "cli-login" } else { "cli-session" },
        child
            .process_id()
            .context("the PTY backend returned a child without a pid")?,
    );
    let mut reader = pty.master.try_clone_reader()?;
    let writer = Arc::new(SyncMutex::new(pty.master.take_writer()?));
    // A login window is driven by a person, so it is ready as soon as it exists.
    // The one PTY anybody waits on is the `/usage` probe, and the only signal
    // the TUI gives is what it prints — a fragile reading, kept because that
    // slash command exists nowhere else, and confined to this one probe: no
    // delegated turn depends on it.
    let ready = Arc::new(AtomicBool::new(auth));
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
        child,
        handle,
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

impl TerminalRuntime {
    /// Asks the CLI to quit on its own, waits for it, and kills the whole group
    /// if it does not go. Two reasons the polite step comes first: a straight
    /// SIGKILL takes the process down before it writes its session file — and an
    /// unwritten session cannot be resumed, which is what the reused probe
    /// session and every user session depend on — and a CLI given the chance to
    /// exit takes its own helpers with it.
    ///
    /// Returns the settled state, so a caller can record `reaped` rather than
    /// guess (D2).
    async fn shutdown(&mut self) -> process::ProcessState {
        {
            let mut writer = self.writer.lock();
            let _ = writer.write_all(b"\x1b");
            let _ = writer.write_all(b"/exit\r");
            let _ = writer.flush();
        }
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                // The leader is already reaped here, so its pid must NOT be
                // signalled again: once nothing holds the group, the number can
                // name a different process. A CLI that exited on request took
                // its own helpers with it; a CLI that did not is handled below,
                // while the group id is still guaranteed to be ours.
                let _ = self.child.wait();
                self.handle.mark_exited();
                return process::ProcessState::Exited;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        // Group first, `wait` second: signalling has to happen while the leader
        // still holds its pid, and `Handle::terminate` reaps it on the way out.
        let state = self.handle.terminate();
        let _ = self.child.wait();
        state
    }

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

/// Bounds the environment a session may be started with.
///
/// Names are restricted to the shape a shell variable actually has, and both
/// the count and the sizes are capped: this endpoint hands strings straight to
/// `execve`, so an unbounded map would be an unbounded process environment. The
/// values are NEVER logged — one of them is the ticket.
fn validated_env(
    raw: &std::collections::BTreeMap<String, String>,
) -> Result<Vec<(String, String)>, ApiError> {
    const MAX_VARS: usize = 32;
    const MAX_NAME: usize = 128;
    const MAX_VALUE: usize = 8192;
    if raw.len() > MAX_VARS {
        return Err(ApiError::bad_request("too many environment variables"));
    }
    let mut env = Vec::with_capacity(raw.len());
    for (name, value) in raw {
        let named_ok = !name.is_empty()
            && name.len() <= MAX_NAME
            && name
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            && !name.chars().next().is_some_and(|c| c.is_ascii_digit());
        if !named_ok {
            return Err(ApiError::bad_request(&format!(
                "invalid environment variable name '{name}'"
            )));
        }
        if value.len() > MAX_VALUE || value.chars().any(|c| c == '\0' || c == '\n') {
            return Err(ApiError::bad_request(&format!(
                "invalid value for environment variable '{name}'"
            )));
        }
        env.push((name.clone(), value.clone()));
    }
    Ok(env)
}

/// Bounds the arguments a session may be started with.
///
/// The same rule as `validated_env`: these strings go straight to `execve`, so
/// their count and size are capped and anything that is not a plain argument is
/// refused. An empty argument or one carrying a NUL would not mean what the
/// caller wrote, and a shell is never involved — the CLI is spawned directly, so
/// there is no quoting to get wrong.
fn validated_args(raw: &[String]) -> Result<Vec<String>, ApiError> {
    const MAX_ARGS: usize = 32;
    const MAX_ARG: usize = 2048;
    if raw.len() > MAX_ARGS {
        return Err(ApiError::bad_request("too many CLI arguments"));
    }
    for argument in raw {
        if argument.is_empty() || argument.len() > MAX_ARG {
            return Err(ApiError::bad_request("invalid CLI argument length"));
        }
        if argument.chars().any(|c| c == '\0' || c.is_control()) {
            return Err(ApiError::bad_request(
                "a CLI argument contains a control character",
            ));
        }
    }
    Ok(raw.to_vec())
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

/// `Debug` because the tests assert on `Result<_, ApiError>`; without it the
/// crate's own test build does not compile.
#[derive(Debug)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_model_cache_keeps_the_cli_untouched() {
        let now = 10 * MODELS_TTL_MS;
        let cache = ProbeCache {
            probe_session_id: Some("probe".into()),
            models: vec![json!({"id":"opus"})],
            models_fetched_at_ms: now - 1,
            ..ProbeCache::default()
        };
        assert!(cache.models_are_fresh(now));

        let stale = ProbeCache {
            models_fetched_at_ms: now - MODELS_TTL_MS,
            ..cache.clone()
        };
        assert!(!stale.models_are_fresh(now), "TTL boundary must expire");

        let empty = ProbeCache {
            models: Vec::new(),
            ..cache
        };
        assert!(
            !empty.models_are_fresh(now),
            "an empty list is not an answer, it must be re-probed"
        );
    }

    #[test]
    fn usage_expires_far_sooner_than_models() {
        let now = 10 * MODELS_TTL_MS;
        let cache = ProbeCache {
            usage: Some(json!({"current_session": {}})),
            usage_fetched_at_ms: now - USAGE_TTL_MS + 1,
            ..ProbeCache::default()
        };
        assert!(cache.usage_is_fresh(now));
        assert!(
            !ProbeCache {
                usage_fetched_at_ms: now - USAGE_TTL_MS,
                ..cache
            }
            .usage_is_fresh(now),
            "rate limits move, they must not be served stale"
        );
    }

    #[test]
    fn the_persisted_cache_keeps_the_probe_session_but_not_the_limits() {
        // Rate limits are per-moment; only the model list and the reusable probe
        // session id are worth carrying across a restart.
        let cache = ProbeCache {
            probe_session_id: Some("probe-1".into()),
            models: vec![json!({"id":"opus"})],
            models_fetched_at_ms: 5,
            usage: Some(json!({"x":1})),
            usage_fetched_at_ms: 5,
        };
        let restored: ProbeCache =
            serde_json::from_slice(&serde_json::to_vec(&cache).unwrap()).unwrap();
        assert_eq!(restored.probe_session_id.as_deref(), Some("probe-1"));
        assert_eq!(restored.models.len(), 1);
        assert!(restored.usage.is_none());
    }

    /// Claude Code runs in its programmatic mode, never in the TUI. The four
    /// protocol flags are the contract Core's `cli_bridge` parses: without them
    /// the session emits ANSI frames, and nothing in the stream says whether the
    /// turn is over.
    #[test]
    fn a_claude_session_is_started_in_the_programmatic_mode() {
        let fresh = claude_args(None, false, Some("s-1"), Some("sonnet"), &[]).expect("args");
        for flag in [
            "--print",
            "--output-format=stream-json",
            "--input-format=stream-json",
            "--verbose",
        ] {
            assert!(fresh.contains(&flag.to_string()), "{flag} is missing");
        }
        assert_eq!(fresh.windows(2).find(|w| w[0] == "--model").expect("model")[1], "sonnet");
        assert_eq!(
            fresh
                .windows(2)
                .find(|w| w[0] == "--session-id")
                .expect("session id")[1],
            "s-1"
        );

        // Resuming names the session the vendor already knows; forking names
        // both, and refuses without an id for the fork.
        let resumed = claude_args(Some("s-1"), false, None, None, &[]).expect("args");
        assert!(resumed.contains(&"--resume".to_string()));
        assert!(!resumed.contains(&"--session-id".to_string()));
        let forked = claude_args(Some("s-1"), true, Some("s-2"), None, &[]).expect("args");
        assert!(forked.contains(&"--fork-session".to_string()));
        assert!(claude_args(Some("s-1"), true, None, None, &[]).is_err());
        assert!(claude_args(None, false, None, None, &[]).is_err());

        // The caller's own arguments come last and are passed through as given.
        let wired = claude_args(None, false, Some("s-1"), None, &["-c".into(), "x=1".into()])
            .expect("args");
        assert_eq!(&wired[wired.len() - 2..], &["-c".to_string(), "x=1".to_string()]);
    }

    /// Caller-supplied arguments reach `execve` directly, so they are bounded
    /// exactly like the environment is.
    #[test]
    fn caller_arguments_are_bounded_before_they_reach_a_process() {
        assert_eq!(
            validated_args(&["-c".to_string(), "model_provider=tfadapter".to_string()])
                .expect("plain arguments are accepted"),
            vec!["-c".to_string(), "model_provider=tfadapter".to_string()]
        );
        assert!(validated_args(&[String::new()]).is_err());
        assert!(validated_args(&["a\nb".to_string()]).is_err());
        assert!(validated_args(&["a\0b".to_string()]).is_err());
        assert!(validated_args(&["x".repeat(4096)]).is_err());
        assert!(validated_args(&vec!["-c".to_string(); 33]).is_err());
    }

    #[test]
    fn a_cache_written_by_an_older_build_still_loads() {
        let cache: ProbeCache = serde_json::from_str(r#"{"models":[{"id":"o3"}]}"#).unwrap();
        assert_eq!(cache.models.len(), 1);
        assert_eq!(cache.probe_session_id, None);
        assert!(!cache.models_are_fresh(MODELS_TTL_MS));
    }

    #[test]
    fn the_claude_model_list_is_configuration_and_never_a_probe() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("models.json");

        // No file: the aliases of the pinned release, labelled as such so a
        // reader cannot mistake them for something the CLI reported.
        let (models, source) = configured_claude_models(&missing).expect("pinned catalog");
        assert_eq!(models.len(), CLAUDE_CODE_MODEL_ALIASES.len());
        assert_eq!(models[0]["id"], "opus");
        assert_eq!(source, format!("pinned:{CLAUDE_CODE_PINNED_VERSION}"));

        // An operator-supplied catalog wins, and says so.
        std::fs::write(&missing, r#"[{"id":"sonnet","display_name":"Sonnet"}]"#).expect("write");
        let (models, source) = configured_claude_models(&missing).expect("file catalog");
        assert_eq!(models.len(), 1);
        assert_eq!(source, "file");

        // A catalog that cannot name a model is an error, not an empty list
        // that would silently disable the engine.
        std::fs::write(&missing, "[]").expect("write");
        assert!(configured_claude_models(&missing).is_err());
        std::fs::write(&missing, r#"[{"display_name":"nameless"}]"#).expect("write");
        assert!(configured_claude_models(&missing).is_err());
    }
}
