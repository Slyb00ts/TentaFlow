mod grok;
mod muse;
mod process;
#[path = "../../process_sandbox.rs"]
mod process_sandbox;
mod rpc;
mod transfer;

use std::{
    collections::{HashMap, HashSet},
    env,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use anyhow::{anyhow, Context, Result};
use axum::{
    extract::Request,
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    middleware::{self, Next},
    response::Response,
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
    sync::Mutex,
};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Provider {
    Codex,
    ClaudeCode,
    MuseCode,
    GrokBuild,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SessionMeta {
    id: String,
    vendor_session_id: String,
    workspace: String,
    status: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    profile_id: Option<String>,
    #[serde(default)]
    login_completed: Option<bool>,
    #[serde(default)]
    request_hash: Option<String>,
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
    Muse(muse::MuseRuntime),
    Grok(grok::GrokRuntime),
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
    bridge_token: Arc<String>,
    shutting_down: Arc<std::sync::atomic::AtomicBool>,
    lease: Arc<Mutex<Option<String>>>,
    provider: Provider,
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

/// Discovery is cached without creating subscription conversations.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct ProbeCache {
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
    rpc: rpc::JsonRpc,
    approvals: Arc<SyncMutex<HashSet<u64>>>,
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
    /// Permission requests the CLI is BLOCKED on, as `bridge id -> vendor
    /// request id`. Claude Code names its control requests with opaque strings
    /// while this bridge's approval API speaks numbers, so the translation lives
    /// here — the same set that tells `answer_approval` an id is real and
    /// `shutdown` what it still owes an answer to (D3).
    approvals: Arc<SyncMutex<HashMap<u64, String>>>,
    /// Killed as a GROUP: the CLI starts helpers (MCP servers, tools) of its
    /// own, and killing the direct child alone orphans them (D2).
    handle: process::Handle,
    child: Child,
}

/// One control frame of Claude Code's `stream-json` channel.
///
/// `--permission-prompt-tool stdio` is what puts them there: the CLI then asks
/// permission over the SAME newline-delimited stream it answers on, instead of
/// calling an MCP tool. Everything else on that stream is session output.
#[derive(Debug, Clone, PartialEq)]
enum ClaudeControl {
    /// "may I use this tool" — the request the session's policy engine decides.
    Permission {
        request_id: String,
        tool_name: String,
        input: Value,
    },
    /// A control request this bridge has no channel for. It is answered with an
    /// error rather than ignored: an unanswered control request leaves the turn
    /// blocked, which is defect D3 in another costume.
    Unsupported { request_id: String, subtype: String },
    /// The CLI withdrew a request it had made (its own timeout, an interrupt).
    Cancelled { request_id: String },
}

/// What the model is told when the policy engine refuses a tool call. It
/// reaches the model as the tool result, so it says who refused — a model that
/// reads "denied" with no source retries the same call.
const PERMISSION_DENIED_MESSAGE: &str =
    "The workspace policy engine refused this tool call. Do not retry it; \
     continue with what the session already allows, or explain what you need.";

/// Reads one line of the CLI's stream as a control frame, or `None` when it is
/// ordinary session output.
fn claude_control(value: &Value) -> Option<ClaudeControl> {
    match value.get("type").and_then(Value::as_str) {
        Some("control_request") => {
            let request_id = value.get("request_id").and_then(Value::as_str)?.to_string();
            let request = value.get("request")?;
            let subtype = request
                .get("subtype")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if subtype != "can_use_tool" {
                return Some(ClaudeControl::Unsupported {
                    request_id,
                    subtype: subtype.to_string(),
                });
            }
            Some(ClaudeControl::Permission {
                request_id,
                // A request that names no tool still gets forwarded: naming the
                // capability is the caller's job, and it refuses what it cannot
                // name. Dropping it here would block the turn instead.
                tool_name: request
                    .get("tool_name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                input: request.get("input").cloned().unwrap_or_else(|| json!({})),
            })
        }
        Some("control_cancel_request") => Some(ClaudeControl::Cancelled {
            request_id: value.get("request_id").and_then(Value::as_str)?.to_string(),
        }),
        _ => None,
    }
}

/// The frame that answers one permission request.
///
/// Both approving decisions become a plain `allow` for THIS call. Claude Code
/// would also accept `updatedPermissions`, which writes a standing rule into its
/// own settings — and that is exactly what must not happen: the standing grant
/// lives in the session's own tables, so the next call asks again and the policy
/// engine answers from the one place that holds the rules.
fn claude_permission_response(vendor_request_id: &str, decision: &str) -> Value {
    let body = if matches!(decision, "approved" | "approved_for_session") {
        json!({"behavior": "allow"})
    } else {
        json!({"behavior": "deny", "message": PERMISSION_DENIED_MESSAGE})
    };
    json!({
        "type": "control_response",
        "response": {"subtype": "success", "request_id": vendor_request_id, "response": body},
    })
}

/// The frame that refuses a control request this bridge cannot answer.
fn claude_control_error(vendor_request_id: &str, error: &str) -> Value {
    json!({
        "type": "control_response",
        "response": {"subtype": "error", "request_id": vendor_request_id, "error": error},
    })
}

/// Writes one frame to the CLI's stdin. Every line this bridge sends Claude
/// Code — a turn, a permission answer, a refusal — goes through here, so the
/// closed-stream case has one answer instead of three.
async fn write_claude_frame(stdin: &Arc<Mutex<Option<ChildStdin>>>, frame: &Value) -> Result<()> {
    let mut guard = stdin.lock().await;
    let stdin = guard
        .as_mut()
        .ok_or_else(|| anyhow!("the Claude Code input stream is closed"))?;
    stdin.write_all(format!("{frame}\n").as_bytes()).await?;
    stdin.flush().await?;
    Ok(())
}

struct TerminalRuntime {
    writer: Arc<SyncMutex<Box<dyn Write + Send>>>,
    _master: Box<dyn MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    /// Pid record + group kill. Dropping the PTY master does not terminate the
    /// CLI — it keeps running against a closed terminal — and killing the direct
    /// child leaves everything the CLI spawned attached to nothing (D2).
    handle: process::Handle,
    reader_thread: Option<std::thread::JoinHandle<()>>,
}

#[derive(Deserialize, Serialize)]
struct CreateSession {
    session_id: String,
    #[serde(default)]
    private_workspace: Option<String>,
    #[serde(default)]
    workspace_authorized: bool,
    #[serde(default)]
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

fn main() -> Result<()> {
    if let Some(code) = process_sandbox::maybe_run_supervisor() {
        std::process::exit(code);
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run())
}

async fn run() -> Result<()> {
    let provider = match env::var("TENTAFLOW_ENGINE_ID").as_deref() {
        Ok("codex") => Provider::Codex,
        Ok("claude-code") => Provider::ClaudeCode,
        Ok("muse-code") => Provider::MuseCode,
        Ok("grok-build") => Provider::GrokBuild,
        Ok(other) => return Err(anyhow!("unsupported TENTAFLOW_ENGINE_ID {other:?}")),
        Err(_) => return Err(anyhow!("TENTAFLOW_ENGINE_ID is required")),
    };
    let data_dir = PathBuf::from(
        env::var("TENTAFLOW_CODING_AGENT_DATA_DIR")
            .unwrap_or_else(|_| ".tentaflow-coding-agent".into()),
    );
    std::fs::create_dir_all(&data_dir)?;
    let account_lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(data_dir.join("account.lock"))?;
    fs2::FileExt::try_lock_exclusive(&account_lock)
        .context("account is already running in another bridge")?;
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
    // Before anything is served: a CLI from a crashed bridge still holds the
    // workspace and its vendor session, and a second one started next to it
    // would fight over both (D2).
    let processes = process::Registry::new(&data_dir)?;
    for orphan in processes.reap_orphans()? {
        if orphan.state == process::ProcessState::Running {
            return Err(anyhow!("previous account process could not be stopped"));
        }
        eprintln!(
            "coding-agent-bridge: orphan {} (pid {}) from a previous life is {}",
            orphan.kind,
            orphan.pid,
            orphan.state.as_str()
        );
    }
    let state = AppState {
        shutting_down: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        lease: Arc::new(Mutex::new(None)),
        bridge_token: Arc::new(
            std::fs::read_to_string(data_dir.join("bridge-token"))?
                .trim()
                .to_string(),
        ),
        provider,
        state_file,
        probe_file,
        models_file,
        probe: Arc::new(Mutex::new(probe)),
        probe_lock: Arc::new(Mutex::new(())),
        sessions: Arc::new(Mutex::new(sessions)),
        processes: Arc::new(processes),
    };
    let active_profile = data_dir.join("active-profile");
    match std::fs::read_to_string(&active_profile) {
        Ok(profile_id) => {
            reconcile_session_credential(&state, profile_id.trim())?;
            std::fs::remove_file(&active_profile)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    {
        let mut sessions = state.sessions.lock().await;
        for session in sessions
            .values_mut()
            .filter(|session| session.meta.status != "closed")
        {
            session.meta.status = "closed".into();
        }
    }
    persist(&state).await?;
    let app = Router::new()
        .route("/runtime/status", get(health))
        .route("/runtime/shutdown", post(shutdown_runtime))
        .route("/auth/status", get(auth_status))
        .route("/auth/start", post(auth_start))
        .route("/models", get(list_models))
        .route("/usage", get(usage))
        .route("/account/transfer/freeze", post(transfer::freeze))
        .route("/account/transfer/retire", post(transfer::retire))
        .route("/account/transfer/activate", post(transfer::activate))
        .route("/sessions", get(list_sessions).post(create_session))
        .route("/sessions/{id}", delete(close_session))
        .route("/sessions/{id}/turn", post(start_turn))
        .route("/sessions/{id}/input", post(send_input))
        .route("/sessions/{id}/approval", post(send_approval))
        .route("/sessions/{id}/events", get(list_events))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            authenticate_bridge,
        ))
        .route("/health", get(health))
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

async fn authenticate_bridge(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let supplied = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    let expected = state.bridge_token.as_bytes();
    let mismatch = supplied
        .as_bytes()
        .iter()
        .zip(expected)
        .fold(0u8, |difference, (a, b)| difference | (a ^ b));
    if expected.len() != 64 || supplied.len() != expected.len() || mismatch != 0 {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(request).await)
}

fn require_binary(provider: Provider) -> Result<()> {
    let binary = match provider {
        Provider::Codex => "codex",
        Provider::ClaudeCode => "claude",
        Provider::MuseCode => "muse",
        Provider::GrokBuild => "grok",
    };
    std::process::Command::new(binary)
        .arg("--version")
        .output()
        .with_context(|| format!("{binary} CLI is not installed"))?;
    Ok(())
}

fn cli_environment(overrides: &[(String, String)]) -> Vec<(String, String)> {
    let mut values: HashMap<String, String> = [
        "PATH",
        "LANG",
        "LC_ALL",
        "TZ",
        "SystemRoot",
        "WINDIR",
        "HOME",
        "TMPDIR",
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
        "GROK_HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "HTTP_PROXY",
        "HTTPS_PROXY",
    ]
    .into_iter()
    .filter_map(|name| env::var(name).ok().map(|value| (name.to_string(), value)))
    .collect();
    for (name, value) in overrides {
        if !name.starts_with("TENTAFLOW_") {
            values.insert(name.clone(), value.clone());
        }
    }
    if overrides
        .iter()
        .any(|(name, _)| name == "TENTAFLOW_AGENT_ADAPTER_ADDR")
    {
        values.remove("HTTP_PROXY");
        values.remove("HTTPS_PROXY");
    }
    values.into_iter().collect()
}

fn sandbox_argv(
    argv: Vec<String>,
    workspace: &Path,
    overrides: &[(String, String)],
) -> Result<Vec<String>> {
    match env::var("TENTAFLOW_AGENT_EXECUTION").as_deref() {
        Ok("container") => {
            return Err(anyhow!(
                "per-session container account isolation is unavailable"
            ))
        }
        Ok("process") => {}
        _ => return Err(anyhow!("managed agent execution policy is required")),
    }
    let find = |name: &str| {
        overrides
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .or_else(|| env::var(name).ok())
    };
    let private = find("TENTAFLOW_AGENT_PRIVATE_ROOT")
        .or_else(|| find("TMPDIR"))
        .context("agent private directory missing")?;
    let mut reads = vec![PathBuf::from(
        env::var("TENTAFLOW_AGENT_RUNTIME_ROOT").context("agent runtime installation missing")?,
    )];
    for binary in ["node", "codex", "claude", "grok", "muse"] {
        if let Some(path) = env::var_os("PATH").and_then(|paths| {
            env::split_paths(&paths)
                .map(|directory| directory.join(binary))
                .find(|path| path.is_file())
        }) {
            let canonical = std::fs::canonicalize(path)?;
            if let Some(parent) = canonical.parent() {
                reads.push(parent.to_path_buf());
            }
            #[cfg(target_os = "macos")]
            if binary == "node" {
                let mut pending = vec![canonical];
                let mut visited = HashSet::new();
                while let Some(binary) = pending.pop() {
                    if !visited.insert(binary.clone()) {
                        continue;
                    }
                    let output = std::process::Command::new("/usr/bin/otool")
                        .arg("-L")
                        .arg(&binary)
                        .output()
                        .context("inspect managed Node libraries")?;
                    if !output.status.success() {
                        return Err(anyhow!("could not inspect managed Node libraries"));
                    }
                    for line in String::from_utf8_lossy(&output.stdout).lines().skip(1) {
                        let Some(path) = line
                            .split_whitespace()
                            .next()
                            .filter(|path| path.starts_with('/'))
                        else {
                            continue;
                        };
                        if !Path::new(path).is_file() {
                            continue;
                        }
                        let library = std::fs::canonicalize(path)?;
                        if let Some(parent) = library.parent() {
                            reads.push(parent.to_path_buf());
                        }
                        pending.push(library);
                    }
                }
            }
        }
    }
    for path in [
        "/opt/homebrew/etc/openssl@3",
        "/opt/homebrew/opt/openssl@3/lib",
        "/opt/homebrew/opt/icu4c/lib",
    ] {
        if Path::new(path).is_dir() {
            reads.push(PathBuf::from(path));
        }
    }
    let mut writes = Vec::new();
    for name in [
        "HOME",
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
        "GROK_HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
    ] {
        if let Some(path) = find(name) {
            writes.push(PathBuf::from(path));
        }
    }
    for name in ["SSL_CERT_FILE", "NODE_EXTRA_CA_CERTS"] {
        if let Some(path) = find(name) {
            if let Some(parent) = Path::new(&path).parent() {
                reads.push(parent.to_path_buf());
            }
        }
    }
    let endpoint: std::net::SocketAddr = if let Some(value) = find("TENTAFLOW_AGENT_ADAPTER_ADDR") {
        value.parse().context("invalid agent adapter endpoint")?
    } else {
        format!(
            "127.0.0.1:{}",
            env::var("TENTAFLOW_AGENT_PROXY_PORT").context("agent proxy port missing")?
        )
        .parse()?
    };
    process_sandbox::ProcessSandbox::new(workspace, Path::new(&private), false, &reads, &writes)?
        .with_proxy(endpoint)?
        .wrap(&argv, workspace)
}

fn cli_command(
    argv: Vec<String>,
    workspace: &Path,
    overrides: &[(String, String)],
) -> Result<(Command, Option<PathBuf>)> {
    let argv = sandbox_argv(argv, workspace, overrides)?;
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(workspace)
        .env_clear()
        .envs(cli_environment(overrides));
    Ok((command, process_sandbox::supervisor_root(&argv)?))
}

fn spawn_cli(command: &mut Command) -> Result<tokio::process::Child> {
    match command.spawn() {
        Ok(child) => Ok(child),
        Err(error) => {
            let argv = std::iter::once(command.as_std().get_program())
                .chain(command.as_std().get_args())
                .map(|value| value.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            process_sandbox::cancel_supervisor_launch(&argv)?;
            Err(error.into())
        }
    }
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
    transfer::write_private(&state.state_file, &serde_json::to_value(metas)?)
}

async fn shutdown_runtime(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    state.shutting_down.store(true, std::sync::atomic::Ordering::SeqCst);
    let ids = {
        let _lease=state.lease.lock().await;
        state.sessions.lock().await.iter().filter(|(_,session)|session.runtime.is_some() || session.meta.status!="closed").map(|(id,_)|id.clone()).collect::<Vec<_>>()
    };
    for id in ids { let _=close_session(State(state.clone()), AxumPath(id)).await?; }
    let _lease=state.lease.lock().await;
    let _probe=state.probe_lock.lock().await;
    if state.processes.reap_orphans()?.iter().any(|record|record.state!=process::ProcessState::Reaped) {
        return Err(ApiError::internal("account process cleanup remains unconfirmed"));
    }
    Ok(Json(json!({"process_state":"reaped","bridge_pid":std::process::id()})))
}

async fn health(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"ok": true, "provider": state.provider}))
}

async fn auth_status(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    let (status, Json(mut value)) = auth_status_snapshot(State(state.clone())).await;
    let sessions = state.sessions.lock().await;
    if let Some(session) = sessions
        .values()
        .filter(|session| session.meta.id.starts_with("auth-"))
        .max_by_key(|session| session.meta.created_at_ms)
    {
        value["login_flow_id"] = json!(session.meta.id);
        if let Some(completed) = session.meta.login_completed {
            value["login_completed"] = json!(completed);
            if !completed {
                value["status"] = json!("login_failed");
                value["authenticated"] = json!(false);
            }
        }
    }
    (status, Json(value))
}

async fn auth_status_snapshot(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    let lease = state.lease.lock().await;
    if let Some(id) = lease.as_deref() {
        let finished = {
            let mut sessions = state.sessions.lock().await;
            sessions
                .get_mut(id)
                .and_then(|session| session.runtime.as_mut())
                .and_then(|runtime| match runtime {
                    Runtime::Terminal(terminal) => terminal
                        .child
                        .try_wait()
                        .ok()
                        .flatten()
                        .map(|status| status.success()),
                    _ => None,
                })
        };
        if let Some(success) = finished {
            let id = id.to_owned();
            drop(lease);
            if let Err(error) = close_session(State(state.clone()), AxumPath(id)).await {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"authenticated":false,"status":"cleanup_failed","error":error.1})),
                );
            }
            let (status, Json(mut value)) = Box::pin(auth_status_snapshot(State(state))).await;
            value["login_completed"] = json!(success);
            if !success {
                value["status"] = json!("login_failed");
                value["authenticated"] = json!(false);
            }
            return (status, Json(value));
        }
        return (
            StatusCode::OK,
            Json(
                json!({"authenticated":false,"status":if id.starts_with("auth-") {"authenticating"} else {"account_busy"}}),
            ),
        );
    }
    if let Err(error) = transfer::available(&state) { return (StatusCode::OK,Json(json!({"authenticated":false,"status":"account_moving","output":error.to_string()}))); }
    if let Err(error) = ensure_idle_runtime(&state) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(
                json!({"authenticated":false,"status":"cleanup_failed","error":error.to_string()}),
            ),
        );
    }
    if matches!(state.provider, Provider::MuseCode | Provider::GrokBuild) {
        return match stored_credential_available(state.provider) {
            Ok(present) => (
                StatusCode::OK,
                Json(
                    json!({"authenticated":false,"credential_present":present,"status":if present {"credentials_present_unverified"} else {"authentication_required"}}),
                ),
            ),
            Err(error) => (
                StatusCode::OK,
                Json(
                    json!({"authenticated":false,"credential_present":false,"status":"credential_verification_required","output":error.to_string()}),
                ),
            ),
        };
    }
    match authentication_status(state.provider).await {
        Ok((authenticated, output)) => (
            StatusCode::OK,
            Json(json!({
                "authenticated": authenticated,
                "status": if authenticated { "authenticated" } else if output.starts_with("credential_verification_required") { "credential_verification_required" } else { "session_expired" },
                "output": output,
            })),
        ),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"authenticated": false, "status": "error", "error": error.to_string()})),
        ),
    }
}

fn stored_credential_available(provider: Provider) -> Result<bool> {
    let account = PathBuf::from(env::var("TENTAFLOW_CODING_AGENT_DATA_DIR")?);
    if account.join("credential-review-required").exists() {
        return Err(anyhow!("Sign in again to verify changed credentials"));
    }
    let (directory, file) = credential_name(provider);
    let path = account.join(directory).join(file);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 1024 * 1024 {
        return Err(anyhow!("invalid provider credential file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(anyhow!("provider credential must not have hardlinks"));
        }
    }
    let value: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    Ok(value.as_object().is_some_and(|value| !value.is_empty()))
}

async fn authentication_status(provider: Provider) -> Result<(bool, String)> {
    let account = PathBuf::from(env::var("TENTAFLOW_CODING_AGENT_DATA_DIR")?);
    if account.join("credential-review-required").exists() {
        return Ok((
            false,
            "credential_verification_required: sign in again to verify refreshed credentials"
                .into(),
        ));
    }
    let argv = if provider == Provider::Codex {
        vec!["codex".into(), "login".into(), "status".into()]
    } else {
        vec!["claude".into(), "auth".into(), "status".into()]
    };
    let overrides = if provider == Provider::ClaudeCode {
        let path = PathBuf::from(env::var("CLAUDE_CONFIG_DIR")?).join("setup-token.json");
        if !path.exists() {
            return Ok((
                false,
                "Claude subscription token is not configured; use account sign-in".into(),
            ));
        }
        vec![("CLAUDE_CODE_OAUTH_TOKEN".into(), read_claude_token(&path)?)]
    } else {
        Vec::new()
    };
    let (mut command, supervisor_root) =
        cli_command(argv, Path::new(&env::var("HOME")?), &overrides)?;
    command
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = spawn_cli(&mut command)?;
    let output =
        tokio::time::timeout(std::time::Duration::from_secs(10), child.wait_with_output()).await;
    if let Some(root) = supervisor_root {
        process_sandbox::wait_for_supervisor(&root, std::time::Duration::from_secs(10))?;
    }
    let output = output.context("authentication status timed out")??;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok((
        output.status.success(),
        if provider == Provider::ClaudeCode {
            "Claude subscription automation token".into()
        } else {
            text
        },
    ))
}

async fn require_authenticated(provider: Provider) -> Result<(), ApiError> {
    if matches!(provider, Provider::MuseCode | Provider::GrokBuild) {
        return if stored_credential_available(provider)? {
            Ok(())
        } else {
            Err(ApiError::unauthorized("authentication_required"))
        };
    }
    match authentication_status(provider).await {
        Ok((true, _)) => Ok(()),
        Ok((false, _)) => Err(ApiError::unauthorized("session_expired")),
        Err(error) => Err(ApiError::internal(&format!(
            "authentication status failed: {error}"
        ))),
    }
}

fn ensure_idle_runtime(state: &AppState) -> Result<()> {
    if state.shutting_down.load(std::sync::atomic::Ordering::SeqCst) { return Err(anyhow!("account runtime is stopping")); }
    if state
        .processes
        .reap_orphans()?
        .iter()
        .any(|entry| entry.state != process::ProcessState::Reaped)
    {
        return Err(anyhow!(
            "previous account process cleanup remains unconfirmed"
        ));
    }
    Ok(())
}

async fn auth_start(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let mut lease = state.lease.lock().await;
    if lease.is_some() {
        return Err(ApiError::bad_request(
            "account_busy: close the active session before signing in",
        ));
    }
    ensure_idle_runtime(&state)?;
    transfer::available(&state)?;
    let workspace = env::var("HOME").context("account HOME missing")?;
    let id = format!("auth-{}", uuid::Uuid::new_v4());
    let events = Arc::new(SyncMutex::new(Vec::new()));
    let runtime = spawn_terminal(
        TerminalSpawn {
            provider: state.provider,
            workspace: &workspace,
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
        profile_id: None,
        login_completed: None,
        request_hash: None,
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
    *lease = Some(id.clone());
    Ok(Json(json!({"flow_id": id})))
}

async fn list_sessions(State(state): State<AppState>) -> Json<Value> {
    let sessions = state.sessions.lock().await;
    Json(
        json!({"sessions": sessions.values().filter(|s| !s.meta.id.starts_with("auth-") && s.meta.status != "closed").map(|s| &s.meta).collect::<Vec<_>>() }),
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
    transfer::available(&state)?;
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
    let lease = state.lease.lock().await;
    if lease.is_some() {
        return Err(ApiError::bad_request(
            "account_busy: discovery cannot refresh credentials while a session is active",
        ));
    }
    ensure_idle_runtime(&state)?;
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
    if state.provider == Provider::GrokBuild {
        let response = grok::GrokRuntime::discover(
            &env::var("HOME").context("account HOME missing")?,
            &[],
            &state.processes,
        )
        .await?;
        let models = response.pointer("/result/_meta/modelState/availableModels").and_then(Value::as_array).context("Grok initialize omitted models")?.iter().map(|model| json!({"id":model["modelId"],"name":model["name"],"isDefault":model["modelId"]==response["result"]["_meta"]["modelState"]["currentModelId"]})).collect::<Vec<_>>();
        let mut cache = state.probe.lock().await;
        cache.models = models.clone();
        cache.models_fetched_at_ms = now_ms();
        drop(cache);
        persist_probe_cache(&state).await?;
        return Ok(Json(json!({"models":models,"cached":false,"source":"cli"})));
    }
    if state.provider == Provider::MuseCode {
        let models = muse::MuseRuntime::discover(
            &env::var("HOME").context("account HOME missing")?,
            &[],
            &state.processes,
        )
        .await?;
        let models = models.as_array().context("Muse model/list omitted model array")?.iter().map(|model| json!({"id":model["modelId"],"name":model["displayLabel"],"isDefault":model["isDefault"]})).collect::<Vec<_>>();
        let mut cache = state.probe.lock().await;
        cache.models = models.clone();
        cache.models_fetched_at_ms = now_ms();
        drop(cache);
        persist_probe_cache(&state).await?;
        return Ok(Json(json!({"models":models,"cached":false,"source":"cli"})));
    }
    let events = Arc::new(SyncMutex::new(Vec::new()));
    let runtime = CodexRuntime::connect(
        &env::var("HOME").context("account HOME missing")?,
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
const CLAUDE_CODE_PINNED_VERSION: &str = "2.1.258";
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
    transfer::available(&state)?;
    if !query.refresh {
        let cache = state.probe.lock().await;
        if cache.usage_is_fresh(now_ms()) {
            return Ok(Json(
                cache.usage.clone().expect("freshness implies a value"),
            ));
        }
    }
    let lease = state.lease.lock().await;
    if lease.is_some() {
        return Err(ApiError::bad_request(
            "account_busy: discovery cannot refresh credentials while a session is active",
        ));
    }
    ensure_idle_runtime(&state)?;
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
                &env::var("HOME").context("account HOME missing")?,
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
        Provider::ClaudeCode | Provider::MuseCode | Provider::GrokBuild => {
            json!({"available":false,"reason":"subscription_token_usage_unavailable","detail":"Subscription automation tokens support model requests; usage discovery is unavailable."})
        }
    };
    {
        let mut cache = state.probe.lock().await;
        cache.usage = Some(usage.clone());
        cache.usage_fetched_at_ms = now_ms();
    }
    Ok(Json(usage))
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

async fn create_session(
    State(state): State<AppState>,
    Json(req): Json<CreateSession>,
) -> Result<Json<Value>, ApiError> {
    if env::var("TENTAFLOW_AGENT_EXECUTION").as_deref() == Ok("container") {
        return Err(ApiError::bad_request(
            "per-session account isolation requires a native process sandbox",
        ));
    }
    if !uuid::Uuid::parse_str(&req.session_id).is_ok_and(|value|value.to_string()==req.session_id) {return Err(ApiError::bad_request("invalid session identifier"));}
    let id=req.session_id.clone();
    use sha2::Digest;
    let request_hash = format!("{:x}", sha2::Sha256::digest(serde_json::to_vec(&req).context("serialize session request")?));
    let mut lease = state.lease.lock().await;
    if let Some(existing) = state.sessions.lock().await.get(&id) {
        if existing.meta.status == "closed" { return Err(ApiError::bad_request("session_closed: canceled session cannot be started")); }
        if existing.meta.request_hash.as_deref()!=Some(&request_hash) { return Err(ApiError::bad_request("session identifier belongs to a different request")); }
        return Ok(Json(json!({"session":existing.meta})));
    }
    if lease.is_some() {
        return Err(ApiError::bad_request("account_busy: this account already has an active session"));
    }
    transfer::available(&state)?;
    ensure_idle_runtime(&state)?;
    if req.env.is_empty() {
        require_authenticated(state.provider).await?;
    }
    let workspace = if let Some(actor) = &req.private_workspace {
        let actor = uuid::Uuid::parse_str(actor).context("invalid workspace actor")?;
        let path = state
            .state_file
            .parent()
            .context("account root missing")?
            .join("scratch")
            .join(actor.to_string());
        std::fs::create_dir_all(&path)?;
        std::fs::canonicalize(path)?.to_string_lossy().into_owned()
    } else if req.workspace_authorized {
        let path = std::fs::canonicalize(&req.workspace)?;
        if !path.is_dir() {
            return Err(ApiError::bad_request("workspace is not a directory"));
        }
        path.to_string_lossy().into_owned()
    } else {
        return Err(ApiError::bad_request("workspace authorization is required"));
    };
    let model = req
        .model
        .as_deref()
        .map(|value| normalize_model_id(state.provider, value))
        .transpose()?;
    let mut env = validated_env(&req.env)?;
    let args = validated_args(&req.args)?;
    let profile_id = if env.is_empty() {
        let previous = if let Some(resume) = &req.resume_vendor_session_id {
            let sessions = state.sessions.lock().await;
            Some(
                sessions
                    .values()
                    .find(|session| &session.meta.vendor_session_id == resume)
                    .and_then(|session| session.meta.profile_id.clone())
                    .ok_or_else(|| {
                        ApiError::bad_request("resume profile is unavailable; start a new session")
                    })?,
            )
        } else {
            None
        };
        let profile_id = previous.unwrap_or_else(|| id.clone());
        env = prepare_session_profile(&state, &profile_id)?;
        Some(profile_id)
    } else {
        None
    };
    let requested_vendor_id = if req.fork || req.resume_vendor_session_id.is_none() {
        uuid::Uuid::new_v4().to_string()
    } else {
        req.resume_vendor_session_id
            .clone()
            .expect("resume id checked")
    };
    if let Some(profile_id) = &profile_id {
        let marker = state
            .state_file
            .parent()
            .context("account root missing")?
            .join("active-profile");
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(marker)?;
        file.write_all(profile_id.as_bytes())?;
        file.sync_all()?;
    }
    *lease = Some(id.clone());
    let events = Arc::new(SyncMutex::new(Vec::new()));
    let started: Result<Runtime> = async {
        Ok(match state.provider {
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
            Provider::GrokBuild => {
                if req.fork {
                    return Err(anyhow!(
                        "Grok ACP fork is not supported by the negotiated contract"
                    ));
                }
                Runtime::Grok(
                    grok::GrokRuntime::spawn(
                        &workspace,
                        req.resume_vendor_session_id.as_deref(),
                        model.as_deref(),
                        &env,
                        &args,
                        events.clone(),
                        &state.processes,
                    )
                    .await?,
                )
            }
            Provider::MuseCode => Runtime::Muse(
                muse::MuseRuntime::spawn(
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
        })
    }
    .await;
    let runtime = match started {
        Ok(runtime) => runtime,
        Err(error) => {
            rollback_session_start(
                &state,
                profile_id.as_deref(),
                req.resume_vendor_session_id.is_none(),
            )?;
            *lease = None;
            return Err(error.into());
        }
    };
    let vendor_id = match &runtime {
        Runtime::Codex(runtime) => runtime.thread_id.clone(),
        Runtime::Muse(runtime) => runtime.session_id.clone(),
        Runtime::Grok(runtime) => runtime.session_id.clone(),
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
        profile_id,
        login_completed: None,
        request_hash: Some(request_hash),
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
    *lease = Some(id.clone());
    if let Err(error) = persist(&state).await {
        drop(lease);
        let _ = close_session(State(state), AxumPath(id)).await?;
        return Err(error.into());
    }
    Ok(Json(json!({"session": meta})))
}

fn credential_name(provider: Provider) -> (&'static str, &'static str) {
    match provider {
        Provider::Codex => ("codex", "auth.json"),
        Provider::ClaudeCode => ("claude", "setup-token.json"),
        Provider::MuseCode => ("config/muse", "auth.json"),
        Provider::GrokBuild => ("grok", "auth.json"),
    }
}

fn valid_claude_token(token: &str) -> bool {
    token.starts_with("sk-ant-oat01-")
        && (64..=4096).contains(&token.len())
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn read_claude_token(path: &Path) -> Result<String> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 8192 {
        return Err(anyhow!("invalid Claude subscription token file"));
    }
    let value: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let token = value
        .get("oauth_token")
        .and_then(Value::as_str)
        .filter(|token| valid_claude_token(token))
        .context("invalid Claude subscription token")?;
    Ok(token.to_string())
}

fn extract_claude_token(plain: &str) -> Option<String> {
    let start = plain.find("sk-ant-oat01-")?;
    let mut token = String::new();
    for line in plain[start..].lines() {
        let line = line.trim();
        if line.is_empty()
            || !line
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            break;
        }
        token.push_str(line);
    }
    valid_claude_token(&token).then_some(token)
}

fn store_claude_token(path: &Path, token: &str) -> Result<()> {
    if !valid_claude_token(token) {
        return Err(anyhow!("invalid Claude subscription token"));
    }
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(&serde_json::to_vec(&json!({"oauth_token":token}))?)?;
    file.sync_all()?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

fn copy_credential(source: &Path, destination: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(source).context("exportable provider credential is unavailable; sign in using a file-backed provider profile")?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 1024 * 1024 {
        return Err(anyhow!(
            "provider credential must be a bounded regular file"
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(anyhow!("provider credential must not have hardlinks"));
        }
    }
    let bytes = std::fs::read(source)?;
    serde_json::from_slice::<Value>(&bytes).context("invalid provider credential JSON")?;
    let temporary = destination.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    std::fs::rename(&temporary, destination)?;
    Ok(())
}

fn session_profile_root(state: &AppState, id: &str) -> Result<PathBuf> {
    if uuid::Uuid::parse_str(id)
        .map(|uuid| uuid.to_string())
        .ok()
        .as_deref()
        != Some(id)
    {
        return Err(anyhow!("invalid private profile identifier"));
    }
    let account =
        std::fs::canonicalize(state.state_file.parent().context("account root missing")?)?;
    let instances = account.join("instances");
    let profile = instances.join(id);
    for path in [&instances, &profile] {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Err(anyhow!("private profile directory must not be a symlink")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(profile)
}

fn prepare_session_profile(state: &AppState, id: &str) -> Result<Vec<(String, String)>> {
    if state
        .state_file
        .parent()
        .context("account root missing")?
        .join("credential-review-required")
        .exists()
    {
        return Err(anyhow!(
            "credential_verification_required: sign in again before opening another session"
        ));
    }
    let root = session_profile_root(state, id)?;
    for name in [
        "home",
        "tmp",
        "codex",
        "claude",
        "grok",
        "config",
        "config/muse",
        "data",
        "data/muse",
    ] {
        let path = root.join(name);
        if let Ok(metadata) = std::fs::symlink_metadata(&path) {
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(anyhow!("private profile directory must not be a symlink"));
            }
        }
        std::fs::create_dir_all(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let (directory, file) = credential_name(state.provider);
    let source = state
        .state_file
        .parent()
        .context("account root missing")?
        .join(directory)
        .join(file);
    copy_credential(&source, &root.join(directory).join(file))?;
    let mut values: Vec<(String, String)> = [
        ("HOME", root.join("home")),
        ("TMPDIR", root.join("tmp")),
        ("CODEX_HOME", root.join("codex")),
        ("CLAUDE_CONFIG_DIR", root.join("claude")),
        ("GROK_HOME", root.join("grok")),
        ("XDG_CONFIG_HOME", root.join("config")),
        ("XDG_DATA_HOME", root.join("data")),
        ("TENTAFLOW_AGENT_PRIVATE_ROOT", root),
    ]
    .into_iter()
    .map(|(name, path)| (name.to_string(), path.to_string_lossy().into_owned()))
    .collect();
    if state.provider == Provider::ClaudeCode {
        values.push((
            "CLAUDE_CODE_OAUTH_TOKEN".into(),
            read_claude_token(&session_profile_root(state, id)?.join(directory).join(file))?,
        ));
        values.push((
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(),
            "1".into(),
        ));
    }
    Ok(values)
}

fn reconcile_session_credential(state: &AppState, id: &str) -> Result<()> {
    if state.provider == Provider::ClaudeCode {
        return Ok(());
    }
    let (directory, file) = credential_name(state.provider);
    let source = session_profile_root(state, id)?.join(directory).join(file);
    let account = state.state_file.parent().context("account root missing")?;
    let destination = account.join(directory).join(file);
    let profile = session_profile_root(state, id)?;
    let safe_parent =
        std::fs::canonicalize(source.parent().context("credential directory missing")?)
            .map(|path| path == profile.join(directory))
            .unwrap_or(false);
    if !safe_parent {
        std::fs::write(
            account.join("credential-review-required"),
            b"Private credential directory changed; sign in again",
        )?;
        return Ok(());
    }
    let previous = std::fs::read(&destination)?;
    let metadata = std::fs::symlink_metadata(&source)?;
    #[cfg(unix)]
    let has_hardlinks = {
        use std::os::unix::fs::MetadataExt;
        metadata.nlink() != 1
    };
    #[cfg(not(unix))]
    let has_hardlinks = false;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > 1024 * 1024
        || has_hardlinks
    {
        std::fs::write(
            account.join("credential-review-required"),
            b"Private credential path changed; sign in again",
        )?;
        return Ok(());
    }
    let refreshed = std::fs::read(&source)?;
    if previous == refreshed {
        return Ok(());
    }
    // Project processes can edit their private profile. Only a provider-verified
    // identity may authorize replacing the canonical credential after refresh.
    let pending = account.join("pending-credential.json");
    copy_credential(&source, &pending)?;
    std::fs::write(account.join("credential-review-required"), b"Provider identity verification is required after credential changes. Sign in again to confirm this account.")?;
    Ok(())
}

fn rollback_session_start(
    state: &AppState,
    profile_id: Option<&str>,
    discard_profile: bool,
) -> Result<()> {
    if state
        .processes
        .reap_orphans()?
        .iter()
        .any(|entry| entry.state != process::ProcessState::Reaped)
    {
        return Err(anyhow!(
            "failed session process termination is unconfirmed; account remains leased"
        ));
    }
    if let Some(profile_id) = profile_id {
        reconcile_session_credential(state, profile_id)?;
        let marker = state
            .state_file
            .parent()
            .context("account root missing")?
            .join("active-profile");
        std::fs::remove_file(marker)?;
        if discard_profile {
            std::fs::remove_dir_all(session_profile_root(state, profile_id)?)?;
        }
    }
    Ok(())
}

async fn start_turn(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(req): Json<TurnRequest>,
) -> Result<Json<Value>, ApiError> {
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
        return Err(ApiError::bad_request(
            "session is closed; create a new session with its explicit resume identifier",
        ));
    }
    match session.runtime.as_mut().expect("runtime initialized") {
        Runtime::Codex(runtime) => {
            runtime.request("turn/start", json!({"threadId": session.meta.vendor_session_id, "input": [{"type":"text","text":req.prompt}]})).await?;
        }
        Runtime::Claude(runtime) => runtime.turn(&req.prompt).await?,
        Runtime::Muse(runtime) => runtime.turn(&req.prompt).await?,
        Runtime::Grok(runtime) => runtime.turn(&req.prompt).await?,
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

/// Answers a server→client approval request. Both engines block on one: Codex
/// threads are started with `approvalPolicy: "on-request"`, and Claude Code runs
/// with `--permission-prompt-tool stdio`. Without this path every turn that
/// wants to touch the filesystem or run a command waits until it times out.
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
        Some(Runtime::Claude(runtime)) => {
            runtime
                .answer_approval(req.request_id, &req.decision)
                .await?;
        }
        Some(Runtime::Muse(runtime)) => {
            runtime
                .answer_approval(req.request_id, &req.decision)
                .await?
        }
        Some(Runtime::Grok(runtime)) => {
            runtime
                .answer_approval(req.request_id, &req.decision)
                .await?
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
    let mut lease = state.lease.lock().await;
    let session = {
        let mut sessions = state.sessions.lock().await;
        sessions.remove(&id)
    };
    let Some(mut session) = session else {
        if !uuid::Uuid::parse_str(&id).is_ok_and(|value| value.to_string()==id) {
            return Err(ApiError::bad_request("invalid session identifier"));
        }
        let meta=SessionMeta{id:id.clone(),vendor_session_id:String::new(),workspace:String::new(),status:"closed".into(),model:None,profile_id:None,login_completed:None,request_hash:None,created_at_ms:now_ms()};
        state.sessions.lock().await.insert(id,Session{meta,runtime:None,events:Arc::new(SyncMutex::new(Vec::new()))});
        persist(&state).await?;
        return Ok(Json(json!({"closed":true,"process_state":"reaped"})));
    };
    let state_after = match session.runtime.as_mut() {
        Some(Runtime::Terminal(runtime)) => runtime.shutdown().await,
        Some(Runtime::Codex(runtime)) => runtime.shutdown().await,
        Some(Runtime::Claude(runtime)) => runtime.shutdown().await,
        Some(Runtime::Muse(runtime)) => runtime.shutdown().await,
        Some(Runtime::Grok(runtime)) => runtime.shutdown().await,
        // A session whose runtime was never restarted after a bridge restart
        // has nothing running; the process it once had was reaped at startup.
        None => process::ProcessState::Reaped,
    };
    if state_after == process::ProcessState::Running {
        state.sessions.lock().await.insert(id, session);
        return Err(ApiError::internal(
            "account process termination is unconfirmed; lease retained",
        ));
    }
    let settlement = (|| -> Result<()> {
        if let Some(Runtime::Terminal(runtime)) = session.runtime.as_mut() {
            if let Some(reader) = runtime.reader_thread.take() {
                reader
                    .join()
                    .map_err(|_| anyhow!("login output capture failed"))?;
            }
        }
        if let Some(profile_id) = session
            .meta
            .profile_id
            .as_ref()
            .filter(|_| session.meta.status != "closed")
        {
            reconcile_session_credential(&state, profile_id)?;
            let marker = state
                .state_file
                .parent()
                .context("account root missing")?
                .join("active-profile");
            match std::fs::remove_file(marker) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    })();
    if let Err(error) = settlement {
        state.sessions.lock().await.insert(id, session);
        return Err(error.into());
    }
    if session.meta.id.starts_with("auth-") {
        if let Some(Runtime::Terminal(runtime)) = session.runtime.as_mut() {
            let succeeded = runtime
                .child
                .try_wait()
                .ok()
                .flatten()
                .is_some_and(|status| status.success());
            session.meta.login_completed = Some(succeeded);
            if succeeded {
                let root = state.state_file.parent().context("account root missing")?;
                for name in ["credential-review-required", "pending-credential.json"] {
                    match std::fs::remove_file(root.join(name)) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            state.sessions.lock().await.insert(id, session);
                            return Err(error.into());
                        }
                    }
                }
            }
        }
    }
    session.runtime = None;
    session.meta.status = "closed".into();
    state.sessions.lock().await.insert(id.clone(), session);
    if lease.as_deref() == Some(&id) {
        *lease = None;
    }
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
    Ok(Json(
        json!({"events": events, "status": session.meta.status}),
    ))
}

impl CodexRuntime {
    async fn connect(
        workspace: &str,
        env: &[(String, String)],
        args: &[String],
        events: Arc<SyncMutex<Vec<Event>>>,
        processes: &process::Registry,
    ) -> Result<Self> {
        let mut argv = vec!["codex".into(), "app-server".into()];
        argv.extend_from_slice(args);
        let (rpc, mut inbound) =
            rpc::JsonRpc::spawn(argv, workspace, env, processes, "codex-app-server")?;
        let approvals = Arc::new(SyncMutex::<HashSet<u64>>::new(HashSet::new()));
        let reader_approvals = approvals.clone();
        tokio::spawn(async move {
            while let Some(message) = inbound.recv().await {
                let value = match message {
                    rpc::Inbound::Frame(value) => value,
                    rpc::Inbound::Closed(reason) => {
                        push_event(&events, "error", json!({"message":reason}));
                        break;
                    }
                };
                if let Some(id) = value.get("id").and_then(Value::as_u64) {
                    reader_approvals.lock().insert(id);
                    push_event(
                        &events,
                        "approval_request",
                        json!({
                            "request_id":id,
                            "method":value.get("method").cloned().unwrap_or(Value::Null),
                            "params":value.get("params").cloned().unwrap_or(Value::Null),
                        }),
                    );
                } else {
                    push_event(&events, "codex", value);
                }
            }
        });
        let runtime = Self {
            thread_id: String::new(),
            rpc,
            approvals,
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
        self.rpc
            .client()
            .request(method, params, std::time::Duration::from_secs(60))
            .await
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.rpc.client().notify(method, params).await
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
        self.rpc.shutdown().await
    }

    async fn write(&self, value: Value) -> Result<()> {
        self.rpc.client().write(value).await
    }
}

impl ClaudeRuntime {
    /// Starts the CLI with its permission channel pointed at this bridge.
    ///
    /// `--permission-prompt-tool stdio` is what makes that possible without an
    /// MCP server: the value is a sentinel, and the CLI then raises every
    /// permission question as a `control_request` on the stream-json channel it
    /// already reads answers from. Each one is forwarded as an
    /// `approval_request` event, so a Claude Code tool call is decided by the
    /// same policy engine as a Codex one and lands in the same timeline.
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
        let mut argv = vec!["claude".into()];
        argv.extend(claude_args(resume, fork, new_session_id, model, args)?);
        let (mut command, supervisor_root) = cli_command(argv, Path::new(workspace), env)?;
        command
            .current_dir(workspace)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = spawn_cli(&mut command)?;
        let handle = processes.track(
            "claude-print",
            child.id().context("claude has no pid")?,
            supervisor_root,
        )?;
        let stdin = Arc::new(Mutex::new(Some(
            child.stdin.take().context("claude stdin missing")?,
        )));
        let stdout = child.stdout.take().context("claude stdout missing")?;
        let approvals = Arc::new(SyncMutex::<HashMap<u64, String>>::new(HashMap::new()));
        let reader_approvals = approvals.clone();
        // Lives in the reader alone: it is the only minter of these ids, and
        // the numbers mean nothing outside the map they key.
        let next_approval_id = AtomicU64::new(1);
        let reader_stdin = stdin.clone();
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
                match claude_control(&value) {
                    Some(ClaudeControl::Permission {
                        request_id,
                        tool_name,
                        input,
                    }) => {
                        let id = next_approval_id.fetch_add(1, Ordering::Relaxed);
                        reader_approvals.lock().insert(id, request_id);
                        push_event(
                            &events,
                            "approval_request",
                            json!({"request_id": id, "method": tool_name, "params": input}),
                        );
                        continue;
                    }
                    Some(ClaudeControl::Unsupported {
                        request_id,
                        subtype,
                    }) => {
                        let refusal = format!(
                            "this bridge answers permission requests only; it has no channel \
                             for control request '{subtype}'"
                        );
                        if let Err(error) = write_claude_frame(
                            &reader_stdin,
                            &claude_control_error(&request_id, &refusal),
                        )
                        .await
                        {
                            eprintln!("coding-agent-bridge: refusing '{subtype}' failed: {error}");
                        }
                        push_event(&events, "terminal", json!({ "text": refusal }));
                        continue;
                    }
                    Some(ClaudeControl::Cancelled { request_id }) => {
                        reader_approvals
                            .lock()
                            .retain(|_, vendor| *vendor != request_id);
                        continue;
                    }
                    None => {}
                }
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
            approvals,
            handle,
            child,
        })
    }

    /// Answers one outstanding permission request. An id that is not
    /// outstanding is refused rather than written: the CLI would ignore the
    /// response, and the caller would believe a turn was unblocked when it was
    /// not (D3).
    async fn answer_approval(&self, request_id: u64, decision: &str) -> Result<(), ApiError> {
        let Some(vendor_request_id) = self.approvals.lock().remove(&request_id) else {
            return Err(ApiError::not_found(
                "no approval request is outstanding under that id",
            ));
        };
        let frame = claude_permission_response(&vendor_request_id, decision);
        if let Err(error) = write_claude_frame(&self.stdin, &frame).await {
            // Put it back: the turn is still blocked, so the operator must be
            // able to answer again.
            self.approvals.lock().insert(request_id, vendor_request_id);
            return Err(ApiError::internal(&format!(
                "claude permission response failed: {error}"
            )));
        }
        Ok(())
    }

    /// Denies whatever is still outstanding. Called when a session goes away:
    /// an unanswered request leaves the CLI blocked, and a blocked CLI is a
    /// process that never exits.
    async fn settle_pending_approvals(&self) {
        let outstanding: Vec<String> = self.approvals.lock().drain().map(|(_, id)| id).collect();
        for vendor_request_id in outstanding {
            let frame = claude_permission_response(&vendor_request_id, "denied");
            if let Err(error) = write_claude_frame(&self.stdin, &frame).await {
                eprintln!(
                    "coding-agent-bridge: settling permission {vendor_request_id} failed: {error}"
                );
            }
        }
    }

    /// Starts one turn by writing a single user message. The process stays
    /// alive between turns, which is what `session.turn` on an open session
    /// means (§17.2: one long-lived instance per `cli_instances` row).
    async fn turn(&self, prompt: &str) -> Result<(), ApiError> {
        let message = json!({
            "type": "user",
            "message": {"role": "user", "content": [{"type": "text", "text": prompt}]},
        });
        write_claude_frame(&self.stdin, &message)
            .await
            .map_err(|error| ApiError::internal(&format!("claude stdin write failed: {error}")))
    }

    /// Denies what is outstanding, closes the input stream, gives the CLI its
    /// chance to finish and exit, and kills the group if it does not. The polite
    /// step is not politeness: the session transcript `--resume` reads is
    /// written on exit, and a straight SIGKILL loses it.
    async fn shutdown(&mut self) -> process::ProcessState {
        self.settle_pending_approvals().await;
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
///
/// `--permission-prompt-tool stdio` is the permission channel. The value is a
/// sentinel rather than a tool name (`claude 2.1.233` routes it to the control
/// protocol of this very stream instead of looking for an MCP tool), and it is
/// what makes a Claude Code tool call answerable by the session's policy engine.
/// Without it the CLI decides alone what it may do.
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
        "--permission-prompt-tool".to_string(),
        "stdio".to_string(),
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

/// Login flows use a private terminal; Claude token output stays in the bridge.
struct TerminalSpawn<'a> {
    provider: Provider,
    workspace: &'a str,
}

fn spawn_terminal(
    spawn: TerminalSpawn<'_>,
    events: Arc<SyncMutex<Vec<Event>>>,
    processes: &process::Registry,
) -> Result<TerminalRuntime> {
    let TerminalSpawn {
        provider,
        workspace,
    } = spawn;
    let pty = native_pty_system().openpty(PtySize {
        rows: 40,
        cols: 120,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let argv = match provider {
        Provider::Codex => vec!["codex".into(), "login".into(), "--device-auth".into()],
        Provider::ClaudeCode => vec!["claude".into(), "setup-token".into()],
        Provider::MuseCode => vec!["muse".into(), "login".into()],
        Provider::GrokBuild => vec![
            "grok".into(),
            "--no-auto-update".into(),
            "login".into(),
            "--device-auth".into(),
        ],
    };
    let argv = sandbox_argv(argv, Path::new(workspace), &[])?;
    let mut command = CommandBuilder::new(&argv[0]);
    if process_sandbox::supervisor_root(&argv)?.is_some() {
        command.set_controlling_tty(false);
    }
    command.args(&argv[1..]);
    command.cwd(workspace);
    command.env_clear();
    for (name, value) in cli_environment(&[]) {
        command.env(name, value);
    }
    let child = match pty.slave.spawn_command(command) {
        Ok(child) => child,
        Err(error) => {
            process_sandbox::cancel_supervisor_launch(&argv)?;
            return Err(error);
        }
    };
    // The PTY backend makes the child a session leader, so its pid is also its
    // process group id: one `killpg` reaches the helpers the CLI spawns.
    let handle = processes.track(
        "cli-login",
        child
            .process_id()
            .context("the PTY backend returned a child without a pid")?,
        process_sandbox::supervisor_root(&argv)?,
    )?;
    let mut reader = pty.master.try_clone_reader()?;
    let writer = Arc::new(SyncMutex::new(pty.master.take_writer()?));
    let claude_token_path = if provider == Provider::ClaudeCode {
        Some(PathBuf::from(env::var("CLAUDE_CONFIG_DIR")?).join("setup-token.json"))
    } else {
        None
    };
    let reader_thread = std::thread::spawn(move || {
        let mut buf = [0_u8; 4096];
        let mut startup = String::new();
        let mut shown_auth_url = String::new();
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let text = String::from_utf8_lossy(&buf[..n]);
                    startup.push_str(&text);
                    if startup.len() > 65_536 {
                        let boundary = startup
                            .char_indices()
                            .map(|(index, _)| index)
                            .find(|index| *index >= 32_768)
                            .unwrap_or(startup.len());
                        startup.drain(..boundary);
                    }
                    let plain = terminal_plain_text(&startup);
                    if claude_token_path.is_some() {
                        for candidate in plain.split_whitespace() {
                            if (candidate.starts_with("https://claude.ai/oauth/")
                                || candidate.starts_with("https://console.anthropic.com/oauth/"))
                                && candidate != shown_auth_url
                                && !candidate.contains("sk-ant-")
                            {
                                shown_auth_url = candidate.to_string();
                                push_event(
                                    &events,
                                    "terminal",
                                    json!({"text":format!("Open this authorization URL in your browser, then paste the returned code here:\n{candidate}\n")}),
                                );
                            }
                        }
                        continue;
                    }
                    push_event(&events, "terminal", json!({"text":text}));
                }
            }
        }
        if let Some(path) = claude_token_path {
            let result = extract_claude_token(&terminal_plain_text(&startup))
                .ok_or_else(|| anyhow!("Claude did not return a complete subscription token"))
                .and_then(|token| store_claude_token(&path, &token));
            let text = if result.is_ok() {
                "Claude subscription token saved privately. Authentication complete.\n"
            } else {
                "Claude subscription token was not saved. Close this flow and retry sign-in.\n"
            };
            push_event(&events, "terminal", json!({"text":text}));
        }
    });
    Ok(TerminalRuntime {
        writer,
        _master: pty.master,
        child,
        handle,
        reader_thread: Some(reader_thread),
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
                return self.handle.mark_exited();
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        // Group first, `wait` second: signalling has to happen while the leader
        // still holds its pid, and `Handle::terminate` reaps it on the way out.
        let state = self.handle.terminate();
        let _ = self.child.wait();
        state
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

fn normalize_model_id(provider: Provider, raw: &str) -> Result<String, ApiError> {
    let trimmed = raw.trim();
    let prefix = match provider {
        Provider::Codex => "codex/",
        Provider::ClaudeCode => "claude-code/",
        Provider::MuseCode => "muse-code/",
        Provider::GrokBuild => "grok-build/",
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

    fn account_fixture(root: &Path) -> AppState {
        std::fs::create_dir_all(root.join("codex")).unwrap();
        std::fs::write(
            root.join("codex/auth.json"),
            br#"{"tokens":{"account_id":"synthetic","refresh_token":"first"}}"#,
        )
        .unwrap();
        AppState {
            bridge_token: Arc::new("a".repeat(64)),
            shutting_down: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            lease: Arc::new(Mutex::new(None)),
            provider: Provider::Codex,
            state_file: root.join("sessions.json"),
            probe_file: root.join("probe.json"),
            models_file: root.join("models.json"),
            probe: Arc::new(Mutex::new(ProbeCache::default())),
            probe_lock: Arc::new(Mutex::new(())),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            processes: Arc::new(process::Registry::new(root).unwrap()),
        }
    }

    #[tokio::test]
    #[ignore = "requires TENTAFLOW_TEST_CODEX_BINARY and explicit managed sandbox environment"]
    async fn real_installed_codex_initializes_without_credentials() {
        let binary = env::var("TENTAFLOW_TEST_CODEX_BINARY").expect("test Codex binary");
        let temporary = tempfile::tempdir().unwrap();
        let project = temporary.path().join("project");
        let private = temporary.path().join("private");
        for path in [
            &project,
            &private,
            &private.join("home"),
            &private.join("tmp"),
            &private.join("codex"),
            &private.join("claude"),
        ] {
            std::fs::create_dir_all(path).unwrap();
        }
        let overrides = [
            ("HOME", private.join("home")),
            ("TMPDIR", private.join("tmp")),
            ("CODEX_HOME", private.join("codex")),
            ("CLAUDE_CONFIG_DIR", private.join("claude")),
            ("TENTAFLOW_AGENT_PRIVATE_ROOT", private.clone()),
        ]
        .into_iter()
        .map(|(name, path)| (name.to_string(), path.display().to_string()))
        .collect::<Vec<_>>();
        let output = cli_command(
            vec![binary.clone(), "--version".into()],
            &project,
            &overrides,
        )
        .unwrap()
        .0
        .output()
        .await
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        eprintln!("{}", String::from_utf8_lossy(&output.stdout).trim());
        let (mut command, supervisor_root) =
            cli_command(vec![binary, "app-server".into()], &project, &overrides).unwrap();
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true);
        let mut child = command.spawn().unwrap();
        child.stdin.as_mut().unwrap().write_all(b"{\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"tentaflow-isolation-test\",\"version\":\"1\"}}}\n").await.unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let line = tokio::time::timeout(std::time::Duration::from_secs(10), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response.get("id"), Some(&json!(1)));
        assert!(response.get("result").is_some(), "{response}");
        child.kill().await.unwrap();
        if let Some(root) = supervisor_root {
            process_sandbox::wait_for_supervisor(&root, std::time::Duration::from_secs(10))
                .unwrap();
        }
        assert!(!private.join("codex/auth.json").exists());
    }

    #[tokio::test]
    #[ignore = "requires TENTAFLOW_TEST_CLAUDE_BINARY and explicit managed sandbox environment"]
    async fn real_installed_claude_version_uses_empty_profile() {
        let binary = env::var("TENTAFLOW_TEST_CLAUDE_BINARY").expect("test Claude binary");
        let temporary = tempfile::tempdir().unwrap();
        let project = temporary.path().join("project");
        let private = temporary.path().join("private");
        for path in [
            &project,
            &private,
            &private.join("home"),
            &private.join("tmp"),
            &private.join("codex"),
            &private.join("claude"),
        ] {
            std::fs::create_dir_all(path).unwrap();
        }
        let overrides = [
            ("HOME", private.join("home")),
            ("TMPDIR", private.join("tmp")),
            ("CODEX_HOME", private.join("codex")),
            ("CLAUDE_CONFIG_DIR", private.join("claude")),
            ("TENTAFLOW_AGENT_PRIVATE_ROOT", private.clone()),
        ]
        .into_iter()
        .map(|(name, path)| (name.to_string(), path.display().to_string()))
        .collect::<Vec<_>>();
        let output = cli_command(vec![binary, "--version".into()], &project, &overrides)
            .unwrap()
            .0
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        eprintln!("{}", String::from_utf8_lossy(&output.stdout).trim());
        assert!(!private.join("claude/.credentials.json").exists());
    }

    #[tokio::test]
    async fn shutdown_proof_blocks_further_account_processes() {
        let temporary=tempfile::tempdir().unwrap();
        let state=account_fixture(temporary.path());
        let proof=shutdown_runtime(State(state.clone())).await.unwrap().0;
        assert_eq!(proof["process_state"],"reaped");
        assert_eq!(proof["bridge_pid"],std::process::id());
        assert!(transfer::available(&state).is_err());
        assert!(ensure_idle_runtime(&state).is_err());
        let _=shutdown_runtime(State(state)).await.unwrap();
    }

    #[tokio::test]
    async fn closing_a_pending_client_session_prevents_its_delayed_start() {
        let temporary=tempfile::tempdir().unwrap();
        let state=account_fixture(temporary.path());
        let id=uuid::Uuid::new_v4().to_string();
        let closed=close_session(State(state.clone()),AxumPath(id.clone())).await.unwrap().0;
        assert_eq!(closed["process_state"],"reaped");
        let req:CreateSession=serde_json::from_value(json!({"session_id":id,"workspace_authorized":true,"workspace":temporary.path()})).unwrap();
        assert!(create_session(State(state.clone()),Json(req)).await.is_err());
        assert!(state.lease.lock().await.is_none());
        let persisted:Value=serde_json::from_slice(&std::fs::read(&state.state_file).unwrap()).unwrap();
        assert!(persisted.to_string().contains(&id));
        assert_eq!(state.sessions.lock().await[&id].meta.status,"closed");
    }

    #[tokio::test]
    async fn relocation_barriers_are_idempotent_and_never_activate_the_source() {
        let temporary = tempfile::tempdir().unwrap();
        let state = account_fixture(temporary.path());
        let id = uuid::Uuid::new_v4().to_string();
        let request = json!({"transfer_id":id,"manifest":{"account_id":"test-account"}});
        let first = transfer::freeze(State(state.clone()), Json(request.clone())).await.unwrap().0;
        let second = transfer::freeze(State(state.clone()), Json(request.clone())).await.unwrap().0;
        assert_eq!(first, second);
        assert!(transfer::available(&state).is_err());
        let _ = transfer::retire(State(state.clone()), Json(json!({"transfer_id":id}))).await.unwrap();
        let _ = transfer::retire(State(state.clone()), Json(json!({"transfer_id":id}))).await.unwrap();
        assert!(transfer::freeze(State(state.clone()), Json(request)).await.is_err());
        assert!(transfer::activate(State(state.clone()), Json(json!({"transfer_id":id}))).await.is_err());
        assert!(transfer::available(&state).is_err());
        transfer::write_private(&temporary.path().join("transfer.json"), &json!({"transfer_id":id,"phase":"target_staged"})).unwrap();
        let _ = transfer::activate(State(state.clone()), Json(json!({"transfer_id":id}))).await.unwrap();
        let _ = transfer::activate(State(state.clone()), Json(json!({"transfer_id":id}))).await.unwrap();
        assert!(transfer::available(&state).is_ok());
    }

    #[test]
    fn session_profiles_keep_history_and_unverified_refresh_out_of_the_account() {
        let temporary = tempfile::tempdir().unwrap();
        let state = account_fixture(temporary.path());
        std::fs::write(
            temporary.path().join("codex/history.jsonl"),
            "other actor history",
        )
        .unwrap();
        let first = uuid::Uuid::new_v4().to_string();
        let second = uuid::Uuid::new_v4().to_string();
        prepare_session_profile(&state, &first).unwrap();
        let first_root = session_profile_root(&state, &first).unwrap();
        assert!(!first_root.join("codex/history.jsonl").exists());
        std::fs::write(
            first_root.join("codex/history.jsonl"),
            "first actor private history",
        )
        .unwrap();
        std::fs::write(
            first_root.join("codex/auth.json"),
            br#"{"tokens":{"account_id":"synthetic","refresh_token":"refreshed"}}"#,
        )
        .unwrap();
        prepare_session_profile(&state, &second).unwrap();
        reconcile_session_credential(&state, &first).unwrap();
        let second_root = session_profile_root(&state, &second).unwrap();
        assert!(!second_root.join("codex/history.jsonl").exists());
        assert!(std::fs::read_to_string(second_root.join("codex/auth.json"))
            .unwrap()
            .contains("first"));
        assert!(
            std::fs::read_to_string(temporary.path().join("pending-credential.json"))
                .unwrap()
                .contains("refreshed")
        );
        assert!(prepare_session_profile(&state, &uuid::Uuid::new_v4().to_string()).is_err());
        assert_ne!(first_root, second_root);
        std::fs::write(
            first_root.join("codex/auth.json"),
            br#"{"tokens":{"account_id":"another","refresh_token":"foreign"}}"#,
        )
        .unwrap();
        reconcile_session_credential(&state, &first).unwrap();
        assert!(
            !std::fs::read_to_string(temporary.path().join("codex/auth.json"))
                .unwrap()
                .contains("foreign")
        );
    }

    #[test]
    fn claude_setup_token_is_extracted_and_stored_without_a_keychain() {
        let token = format!("sk-ant-oat01-{}", "x".repeat(96));
        let output = format!(
            "Your OAuth token:\n\n{}\n{}\n\nStore this token securely.\n",
            &token[..60],
            &token[60..]
        );
        assert_eq!(extract_claude_token(&output), Some(token.clone()));
        assert!(extract_claude_token("sk-ant-oat01-incomplete").is_none());
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("setup-token.json");
        store_claude_token(&path, &token).unwrap();
        assert_eq!(read_claude_token(&path).unwrap(), token);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn failed_session_start_preserves_refresh_and_removes_its_lease_marker() {
        let temporary = tempfile::tempdir().unwrap();
        let state = account_fixture(temporary.path());
        let id = uuid::Uuid::new_v4().to_string();
        prepare_session_profile(&state, &id).unwrap();
        std::fs::write(temporary.path().join("active-profile"), &id).unwrap();
        let profile = session_profile_root(&state, &id).unwrap();
        std::fs::write(
            profile.join("codex/auth.json"),
            br#"{"tokens":{"account_id":"synthetic","refresh_token":"after-failed-start"}}"#,
        )
        .unwrap();
        rollback_session_start(&state, Some(&id), true).unwrap();
        assert!(!temporary.path().join("active-profile").exists());
        assert!(!profile.exists());
        assert!(
            std::fs::read_to_string(temporary.path().join("pending-credential.json"))
                .unwrap()
                .contains("after-failed-start")
        );
        assert!(
            std::fs::read_to_string(temporary.path().join("codex/auth.json"))
                .unwrap()
                .contains("first")
        );
        assert!(prepare_session_profile(&state, &uuid::Uuid::new_v4().to_string()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn private_profile_symlink_cannot_make_the_broker_read_another_account() {
        let temporary = tempfile::tempdir().unwrap();
        let state = account_fixture(temporary.path());
        let id = uuid::Uuid::new_v4().to_string();
        prepare_session_profile(&state, &id).unwrap();
        let profile = session_profile_root(&state, &id).unwrap();
        let foreign = tempfile::tempdir().unwrap();
        std::fs::write(
            foreign.path().join("auth.json"),
            br#"{"secret":"foreign-account"}"#,
        )
        .unwrap();
        std::fs::remove_dir_all(profile.join("codex")).unwrap();
        std::os::unix::fs::symlink(foreign.path(), profile.join("codex")).unwrap();
        assert!(prepare_session_profile(&state, &id).is_err());
        reconcile_session_credential(&state, &id).unwrap();
        assert!(temporary.path().join("credential-review-required").exists());
        assert!(!temporary.path().join("pending-credential.json").exists());
        assert!(
            !std::fs::read_to_string(temporary.path().join("codex/auth.json"))
                .unwrap()
                .contains("foreign-account")
        );
    }

    #[test]
    fn missing_exportable_credential_and_hardlink_are_refused() {
        let temporary = tempfile::tempdir().unwrap();
        let destination = temporary.path().join("destination");
        assert!(copy_credential(&temporary.path().join("missing"), &destination).is_err());
        let source = temporary.path().join("credential");
        std::fs::write(&source, "{}").unwrap();
        std::fs::hard_link(&source, temporary.path().join("alias")).unwrap();
        #[cfg(unix)]
        assert!(copy_credential(&source, &destination).is_err());
    }

    #[test]
    fn a_second_bridge_cannot_acquire_the_same_account_lease() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("account.lock");
        let first = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let second = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        fs2::FileExt::try_lock_exclusive(&first).unwrap();
        assert!(fs2::FileExt::try_lock_exclusive(&second).is_err());
        fs2::FileExt::unlock(&first).unwrap();
        fs2::FileExt::try_lock_exclusive(&second).unwrap();
    }

    #[tokio::test]
    async fn bridge_ipc_rejects_missing_and_wrong_credentials() {
        use tower::ServiceExt;
        let temporary = tempfile::tempdir().unwrap();
        let state = account_fixture(temporary.path());
        let app = Router::new()
            .route("/sessions", get(|| async { "private" }))
            .route_layer(middleware::from_fn_with_state(state, authenticate_bridge));
        for (token, status) in [
            (None, StatusCode::UNAUTHORIZED),
            (Some("wrong".into()), StatusCode::UNAUTHORIZED),
            (Some("a".repeat(64)), StatusCode::OK),
        ] {
            let mut request = axum::http::Request::builder().uri("/sessions");
            if let Some(token) = token {
                request = request.header("authorization", format!("Bearer {token}"));
            }
            let response = app
                .clone()
                .oneshot(request.body(axum::body::Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), status);
        }
    }

    #[test]
    fn a_fresh_model_cache_keeps_the_cli_untouched() {
        let now = 10 * MODELS_TTL_MS;
        let cache = ProbeCache {
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
    fn the_persisted_cache_keeps_models_but_not_the_limits() {
        // Rate limits are per-moment; only the model list and the reusable probe
        // session id are worth carrying across a restart.
        let cache = ProbeCache {
            models: vec![json!({"id":"opus"})],
            models_fetched_at_ms: 5,
            usage: Some(json!({"x":1})),
            usage_fetched_at_ms: 5,
        };
        let restored: ProbeCache =
            serde_json::from_slice(&serde_json::to_vec(&cache).unwrap()).unwrap();
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
        // The permission channel. Without the sentinel value the CLI answers
        // its own permission questions and nothing this bridge forwards is
        // decided by the session's policy engine.
        assert_eq!(
            fresh
                .windows(2)
                .find(|w| w[0] == "--permission-prompt-tool")
                .expect("permission channel")[1],
            "stdio"
        );
        assert_eq!(
            fresh.windows(2).find(|w| w[0] == "--model").expect("model")[1],
            "sonnet"
        );
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
        assert_eq!(
            &wired[wired.len() - 2..],
            &["-c".to_string(), "x=1".to_string()]
        );
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

    /// The recorded shape of `claude 2.1.233`'s permission channel. A tool call
    /// arrives as a `control_request` whose `subtype` is `can_use_tool`, and
    /// everything the decision needs (the tool and its input) is in it.
    #[test]
    fn a_permission_question_is_read_off_the_control_channel() {
        let request = json!({
            "type": "control_request",
            "request_id": "req_014f",
            "request": {
                "subtype": "can_use_tool",
                "tool_name": "Bash",
                "display_name": "Bash",
                "tool_use_id": "toolu_01",
                "description": "Run the test suite",
                "input": {"command": "cargo test", "description": "Run the test suite"}
            }
        });
        assert_eq!(
            claude_control(&request),
            Some(ClaudeControl::Permission {
                request_id: "req_014f".into(),
                tool_name: "Bash".into(),
                input: json!({"command": "cargo test", "description": "Run the test suite"}),
            })
        );

        // A control request of any other kind is answered with an error, never
        // dropped: an unanswered one leaves the turn blocked forever.
        assert_eq!(
            claude_control(&json!({
                "type": "control_request",
                "request_id": "req_02",
                "request": {"subtype": "request_user_dialog", "dialog_kind": "select"}
            })),
            Some(ClaudeControl::Unsupported {
                request_id: "req_02".into(),
                subtype: "request_user_dialog".into(),
            })
        );
        assert_eq!(
            claude_control(&json!({"type": "control_cancel_request", "request_id": "req_014f"})),
            Some(ClaudeControl::Cancelled {
                request_id: "req_014f".into()
            })
        );

        // Session output is not a control frame and must reach the timeline.
        for output in [
            json!({"type": "assistant", "message": {"content": []}}),
            json!({"type": "system", "subtype": "init", "session_id": "s-1"}),
            json!({"type": "result", "subtype": "success"}),
            // A control request without an id could never be answered; treating
            // it as one would only lose the line.
            json!({"type": "control_request", "request": {"subtype": "can_use_tool"}}),
        ] {
            assert_eq!(
                claude_control(&output),
                None,
                "{output} is not a control frame"
            );
        }
    }

    /// The answer the CLI acts on. `behavior` is the whole decision: `deny`
    /// means the tool is never executed and the model gets the message as its
    /// tool result.
    #[test]
    fn a_refusal_reaches_the_cli_as_a_deny_and_never_as_a_standing_rule() {
        let denied = claude_permission_response("req_1", "denied");
        assert_eq!(denied["type"], "control_response");
        assert_eq!(denied["response"]["subtype"], "success");
        assert_eq!(denied["response"]["request_id"], "req_1");
        assert_eq!(denied["response"]["response"]["behavior"], "deny");
        assert!(denied["response"]["response"]["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty()));
        // An abort is a refusal too; anything that is not an approval denies.
        assert_eq!(
            claude_permission_response("req_1", "abort")["response"]["response"]["behavior"],
            "deny"
        );

        for approving in ["approved", "approved_for_session"] {
            let allowed = claude_permission_response("req_2", approving);
            assert_eq!(allowed["response"]["response"]["behavior"], "allow");
            // The standing grant lives in the session's own tables. Writing a
            // rule into the CLI's settings would give the vendor a second,
            // unreadable copy of the policy.
            assert!(
                allowed["response"]["response"]
                    .get("updatedPermissions")
                    .is_none(),
                "{approving} must not install a rule inside the CLI"
            );
        }

        let refused = claude_control_error("req_3", "no channel");
        assert_eq!(refused["response"]["subtype"], "error");
        assert_eq!(refused["response"]["error"], "no channel");
    }

    #[test]
    fn a_cache_written_by_an_older_build_still_loads() {
        let cache: ProbeCache = serde_json::from_str(r#"{"models":[{"id":"o3"}]}"#).unwrap();
        assert_eq!(cache.models.len(), 1);
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
