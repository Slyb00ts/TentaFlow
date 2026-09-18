// ===== File: services/agent_runtime.rs — the CLI runtime a node offers, and the bridge it starts on demand =====
//
// Two things used to be one `services` row, and separating them is the whole
// point of this module (design §D.1/§D.2):
//
//   * a RUNTIME is "engine E can run on node N" — the vendor CLI in the shared
//     version cache plus this repository's bridge executable. It carries no
//     identity, an administrator installs it from the node matrix (N01), and
//     `agent_runtime_engines` records it. Installing one creates no account;
//     uninstalling one deletes no account.
//   * a BRIDGE PROCESS is one account's isolated home. It is started when an
//     account is first used on this node and released when nothing uses it.
//
// Why one bridge PER ACCOUNT rather than per engine: the bridge's isolation
// model is the account directory. It takes an exclusive `account.lock` on
// `TENTAFLOW_CODING_AGENT_DATA_DIR`, keeps the canonical credential, the login
// home and every session profile under it, and reads `TENTAFLOW_ENGINE_ID`
// once at startup. A bridge serving several accounts would have to multiplex
// all of that behind an account parameter on every route — and the one
// guarantee this feature rests on is that a session can never name a path
// belonging to another account. Per account, that guarantee is a directory
// boundary; per engine it would be an argument check in every handler.
//
// The process is NOT a `services` row: nothing here writes to `services`, and
// the account list the dashboard renders comes from `provider_accounts` alone.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::db::DbPool;
use crate::provider_accounts::repository as store;
use crate::services::coding_agent;

/// How long a bridge with no session and no login in flight is kept alive.
///
/// A turn costs one process start when it expires; keeping it forever costs a
/// resident CLI supervisor per account on a node that may hold dozens.
const IDLE_GRACE: Duration = Duration::from_secs(15 * 60);

/// How often idleness is checked. Coarse against `IDLE_GRACE` on purpose: the
/// cost of keeping a finished bridge a minute longer is one idle process, and
/// the sweep takes the registry lock every single tick.
const IDLE_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// How long to wait for a freshly started bridge to answer `/health`. It builds
/// nothing at this point (the CLI and the bridge are already installed), so
/// this is process startup, not a compile.
const START_TIMEOUT: Duration = Duration::from_secs(30);

/// A running bridge for one account on this node.
#[derive(Clone, Debug)]
pub struct BridgeHandle {
    pub account_id: String,
    pub engine_id: String,
    pub port: u16,
    token: String,
}

impl BridgeHandle {
    fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Addresses a bridge surface a test is serving on loopback itself, so the
    /// conversations Core has with a bridge can be exercised without installing
    /// a vendor CLI. The token is part of the handle in production and there is
    /// no other way to construct one.
    #[cfg(test)]
    pub(crate) fn for_test(engine_id: &str, port: u16) -> Self {
        Self {
            account_id: "test-account".to_string(),
            engine_id: engine_id.to_string(),
            port,
            token: "t".repeat(64),
        }
    }

    /// One call on this account's bridge. `path` is a routed path, exactly as
    /// `coding_agent::route` produces for a service-addressed bridge.
    pub async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        let text = coding_agent::call_bridge(&self.base(), &self.token, method, path, body)
            .await
            .map_err(|error| anyhow!("agent bridge {path}: {error}"))?;
        serde_json::from_str(&text).with_context(|| format!("agent bridge {path} returned no JSON"))
    }

    pub async fn get(&self, path: &str) -> Result<Value> {
        self.call(reqwest::Method::GET, path, None).await
    }

    pub async fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.call(reqwest::Method::POST, path, Some(body)).await
    }
}

/// One supervised bridge process.
struct Running {
    handle: BridgeHandle,
    child: tokio::process::Child,
    /// Kept alive for as long as the bridge is: dropping it closes the egress
    /// proxy the sandboxed CLI reaches the network through.
    _proxy: crate::services::coding_agent_proxy::AgentProxy,
    /// The write end of the bridge's stdin. Nothing is ever written to it: the
    /// bridge exits when it reads EOF, so this handle IS the liveness signal —
    /// it closes when this process exits, however it exits, including a SIGKILL
    /// that runs no shutdown code at all.
    _parent_pipe: tokio::process::ChildStdin,
    last_used: Instant,
}

type Bridges = Arc<Mutex<HashMap<String, Running>>>;

fn bridges() -> &'static Bridges {
    static BRIDGES: OnceLock<Bridges> = OnceLock::new();
    BRIDGES.get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
}

/// One start slot per account.
///
/// Starting a bridge installs a CLI and then polls `/health` for up to
/// `START_TIMEOUT`; doing that under the registry lock froze every other reader
/// of it — including the credential-event pump, which calls `running_bridge` on
/// every poll of every OTHER account. The registry lock is now held only for
/// lookups and inserts, and two starts of the SAME account are serialized by
/// this slot instead.
type StartSlots = std::sync::Mutex<HashMap<String, Arc<Mutex<()>>>>;

fn start_slot(account_id: &str) -> Arc<Mutex<()>> {
    static SLOTS: OnceLock<StartSlots> = OnceLock::new();
    let slots = SLOTS.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut slots = slots
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // A slot per account, and it stays: the map is bounded by the number of
    // accounts this node ever ran, and an entry is one `Arc<Mutex<()>>`.
    Arc::clone(
        slots
            .entry(account_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(()))),
    )
}

// =============================================================================
// Runtime installation (N01)
// =============================================================================

/// What the manifest says an engine is on this node.
struct EngineBuild {
    version: String,
    source_hash: String,
    source_root: PathBuf,
}

fn engine_build(engine_id: &str) -> Result<EngineBuild> {
    let manifest = crate::services::manifest::registry()
        .by_id(engine_id)
        .ok_or_else(|| anyhow!("engine '{engine_id}' is not in the service catalog"))?;
    let native = manifest
        .deploy
        .native
        .as_ref()
        .ok_or_else(|| anyhow!("engine '{engine_id}' has no native deployment"))?;
    if native.runtime != crate::services::manifest::NativeRuntime::ManagedCli {
        return Err(anyhow!("engine '{engine_id}' is not a managed CLI"));
    }
    if !crate::services::deploy::host_os_supported(&native.platforms) {
        return Err(anyhow!(
            "engine '{engine_id}' is not supported on {}",
            host_platform()
        ));
    }
    Ok(EngineBuild {
        version: manifest.engine.version.clone(),
        source_hash: manifest.native_source_hash.clone(),
        source_root: crate::paths::containers_root().join(
            native
                .binary_path
                .as_deref()
                .ok_or_else(|| anyhow!("engine '{engine_id}' has no binary_path"))?,
        ),
    })
}

/// The name the manifests use for this operating system. The catalog spells
/// every platform the way Rust does, so this is `consts::OS` and exists to say
/// so at the one place a mismatch would be a silently unsupported engine.
pub fn host_platform() -> &'static str {
    std::env::consts::OS
}

/// Installs the vendor CLI and this repository's bridge for one engine on THIS
/// node, and records the result in the node matrix.
///
/// No account is created, named or needed: the artifacts are shared by every
/// account that will ever run on this engine. A node that cannot isolate a
/// process is refused before anything is downloaded — installing a CLI there
/// would produce a runtime that refuses every turn.
pub async fn install_engine(db: &DbPool, engine_id: &str, actor: &str) -> Result<String> {
    if crate::provider_accounts::engine(engine_id).is_none() {
        return Err(anyhow!("unknown agent engine '{engine_id}'"));
    }
    crate::code_studio::process_sandbox::ProcessSandbox::check_available()
        .map_err(|error| anyhow!("this node cannot isolate an agent process: {error:#}"))?;
    let build = engine_build(engine_id)?;
    store::set_engine_state(
        db,
        engine_id,
        "installing",
        Some(&build.version),
        None,
        Some(actor),
    )?;
    let installed = async {
        crate::services::deploy::managed_cli_install(engine_id, &build.version).await?;
        crate::services::deploy::managed_cli_bridge(
            engine_id,
            &build.source_hash,
            &build.source_root,
        )
        .await
    }
    .await;
    match installed {
        Ok(_) => {
            store::set_engine_state(
                db,
                engine_id,
                "installed",
                Some(&build.version),
                None,
                Some(actor),
            )?;
            Ok(build.version)
        }
        Err(error) => {
            let detail = format!("{error:#}");
            store::set_engine_state(
                db,
                engine_id,
                "error",
                Some(&build.version),
                Some(&detail),
                Some(actor),
            )?;
            Err(anyhow!(detail))
        }
    }
}

/// Removes the engine from the node matrix and stops every bridge that was
/// running on it.
///
/// The shared version cache is deliberately left alone: it is content-addressed
/// by version and shared with any other node-local user of the same CLI, and
/// deleting it would turn "this node no longer offers Codex" into "re-download
/// 200 MB the next time somebody installs it". The same goes for the bridge
/// executable built from this repository's sources. What the uninstall DOES
/// guarantee is that no account keeps running on the engine afterwards — it
/// reclaims no disk, and `docs/agent-accounts-operations.md` says so.
pub async fn uninstall_engine(db: &DbPool, engine_id: &str, actor: &str) -> Result<()> {
    if crate::provider_accounts::engine(engine_id).is_none() {
        return Err(anyhow!("unknown agent engine '{engine_id}'"));
    }
    let running: Vec<String> = {
        let bridges = bridges().lock().await;
        bridges
            .values()
            .filter(|running| running.handle.engine_id == engine_id)
            .map(|running| running.handle.account_id.clone())
            .collect()
    };
    for account_id in running {
        stop(&account_id).await;
    }
    store::clear_engine_state(db, engine_id, Some(actor))?;
    Ok(())
}

// =============================================================================
// On-demand bridge
// =============================================================================

/// What a node that may not hold account credentials answers, and the i18n key
/// the dashboard renders for it.
///
/// A bridge IS the credential: it holds the account's canonical file for as
/// long as it runs. So "this node does not receive accounts" has to be checked
/// before a process is started, not only before a row is replicated — the
/// matrix toggle is otherwise a label rather than a decision.
pub const NOT_RECEIVING_ACCOUNTS: &str = "this node is not configured to receive agent accounts";
pub const NOT_RECEIVING_ACCOUNTS_KEY: &str = "agent_accounts.login.node_not_receiving";

/// The gate every credential-bearing operation on THIS node passes first.
fn require_receiving_accounts(db: &DbPool, node_id: &str) -> Result<()> {
    if store::receives_accounts(db, node_id)? {
        return Ok(());
    }
    Err(anyhow!(NOT_RECEIVING_ACCOUNTS))
}

/// Starts, or reuses, the bridge of one account on this node.
///
/// Two refusals come before any process: a node that may not receive accounts,
/// and an engine that is not installed here — a bridge started against a CLI
/// that is not on this node comes up, answers `/health`, and then fails the
/// first turn with a message about a missing binary, which is a worse answer
/// than saying so now.
pub async fn ensure_runtime(
    db: &DbPool,
    node_id: &str,
    account_id: &str,
    engine_id: &str,
) -> Result<BridgeHandle> {
    if let Some(handle) = current_bridge(account_id, Some(engine_id)).await? {
        return Ok(handle);
    }
    require_receiving_accounts(db, node_id)?;
    let state = store::engine_state(db, node_id, engine_id)?;
    if state.as_ref().map(|row| row.install_state.as_str()) != Some("installed") {
        return Err(anyhow!(
            "engine '{engine_id}' is not installed on this node"
        ));
    }
    // From here on only THIS account is serialized; the registry lock is taken
    // for the two lookups and the insert, never across the start.
    let slot = start_slot(account_id);
    let _starting = slot.lock().await;
    if let Some(handle) = current_bridge(account_id, Some(engine_id)).await? {
        return Ok(handle);
    }
    let running = start_bridge(account_id, engine_id).await?;
    let handle = running.handle.clone();
    bridges()
        .lock()
        .await
        .insert(account_id.to_string(), running);
    spawn_idle_sweeper();
    Ok(handle)
}

/// The live bridge of one account, dropping a registry entry whose process is
/// gone. `engine_id` is the engine the caller expects; a mismatch is an error
/// rather than a second process, because one account has ONE home directory and
/// one `account.lock`.
async fn current_bridge(account_id: &str, engine_id: Option<&str>) -> Result<Option<BridgeHandle>> {
    let mut bridges = bridges().lock().await;
    let Some(running) = bridges.get_mut(account_id) else {
        return Ok(None);
    };
    if let Some(engine_id) = engine_id {
        if running.handle.engine_id != engine_id {
            return Err(anyhow!(
                "account '{account_id}' is already running as '{}' on this node",
                running.handle.engine_id
            ));
        }
    }
    if running.child.try_wait()?.is_some() {
        bridges.remove(account_id);
        return Ok(None);
    }
    running.last_used = Instant::now();
    Ok(Some(running.handle.clone()))
}

/// Starts the loop that calls `release_idle`, once per process.
///
/// It is started from the first bridge rather than from startup because a node
/// that never runs an agent has nothing to sweep, and because this way the
/// sweeper cannot exist without the tokio runtime the bridges live in.
fn spawn_idle_sweeper() {
    static SWEEPER: OnceLock<()> = OnceLock::new();
    SWEEPER.get_or_init(|| {
        tokio::spawn(async {
            let mut tick = tokio::time::interval(IDLE_SWEEP_INTERVAL);
            loop {
                tick.tick().await;
                match release_idle().await {
                    Ok(0) => {}
                    Ok(count) => tracing::info!(count, "released idle agent bridges"),
                    Err(error) => tracing::warn!(%error, "the idle agent bridge sweep failed"),
                }
            }
        });
    });
}

/// The handle of an already running bridge, without starting one.
///
/// Used by the paths that react to something the bridge said: there is no
/// reason to resurrect a process in order to ask it about an event it emitted
/// before it went away.
pub async fn running_bridge(account_id: &str) -> Option<BridgeHandle> {
    #[cfg(test)]
    if let Some(handle) = testing::registered_bridge(account_id) {
        return Some(handle);
    }
    current_bridge(account_id, None).await.ok().flatten()
}

async fn start_bridge(account_id: &str, engine_id: &str) -> Result<Running> {
    let build = engine_build(engine_id)?;
    let (install_root, bin_dir) =
        crate::services::deploy::managed_cli_install(engine_id, &build.version).await?;
    let executable = crate::services::deploy::managed_cli_bridge(
        engine_id,
        &build.source_hash,
        &build.source_root,
    )
    .await?;
    let data_dir =
        coding_agent::prepare_account_root(account_id).map_err(|error| anyhow!("{error}"))?;
    let token =
        coding_agent::read_account_bridge_token(account_id).map_err(|error| anyhow!("{error}"))?;
    let port = free_loopback_port()?;
    let proxy = crate::services::coding_agent_proxy::start(engine_id, account_id).await?;

    let mut command = tokio::process::Command::new(&executable);
    command.env_clear();
    for name in ["LANG", "LC_ALL", "TZ"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let mut path_entries = vec![bin_dir];
    path_entries.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    command.env(
        "PATH",
        std::env::join_paths(path_entries).context("build the managed-CLI PATH")?,
    );
    command.env("PORT", port.to_string());
    command.env("TENTAFLOW_ENGINE_ID", engine_id);
    command.env("TENTAFLOW_AGENT_RUNTIME_ROOT", &install_root);
    command.env("TENTAFLOW_AGENT_EXECUTION", "process");
    command.env("TENTAFLOW_CODING_AGENT_DATA_DIR", &data_dir);
    // The bridge process's OWN private directories; the engine credential homes
    // are not among them, because the bridge points every invocation at the
    // account's canonical directory or at a session profile.
    for (name, directory) in [
        ("HOME", "home"),
        ("XDG_DATA_HOME", "data"),
        ("TMPDIR", "tmp"),
    ] {
        command.env(name, data_dir.join(directory));
    }
    command.env("TENTAFLOW_AGENT_PROXY_PORT", proxy.port().to_string());
    if let Some(socket) = proxy.socket_path() {
        command.env("TENTAFLOW_AGENT_PROXY_SOCKET", socket);
    }
    command.env("HTTP_PROXY", proxy.url());
    command.env("HTTPS_PROXY", proxy.url());
    // The bridge outlives the request that started it but not this process. Two
    // independent mechanisms say so, because one of them is not enough:
    //   * `kill_on_drop` and the shutdown path cover an orderly exit, and
    //   * this pipe covers the rest. Core writes nothing to it; the bridge
    //     reads EOF the moment this process's last descriptor closes — after a
    //     SIGKILL too, which runs no code here at all — and exits. Without it a
    //     killed Core left a bridge holding `account.lock` and the canonical
    //     credential directory, and the next Core could never start one.
    command.env("TENTAFLOW_BRIDGE_PARENT_PIPE", "1");
    command.stdin(std::process::Stdio::piped());
    command.stdout(std::process::Stdio::null());
    command.stderr(std::process::Stdio::piped());
    command.kill_on_drop(true);

    let mut child = command.spawn().context("start the agent bridge")?;
    let parent_pipe = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("the agent bridge was started without its parent pipe"))?;
    let handle = BridgeHandle {
        account_id: account_id.to_string(),
        engine_id: engine_id.to_string(),
        port,
        token,
    };
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait()? {
            let mut detail = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                use tokio::io::AsyncReadExt;
                let _ = stderr.read_to_string(&mut detail).await;
            }
            return Err(anyhow!(
                "the agent bridge for this account exited with {status}: {}",
                detail.trim()
            ));
        }
        if handle.get("/health").await.is_ok() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill().await;
            return Err(anyhow!("the agent bridge did not become ready in time"));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Ok(Running {
        handle,
        child,
        _proxy: proxy,
        _parent_pipe: parent_pipe,
        last_used: Instant::now(),
    })
}

/// A loopback port nothing is listening on. Bound and released immediately —
/// the bridge binds it for real a moment later, and a race there surfaces as a
/// failed start rather than as a bridge answering on somebody else's port,
/// because the bearer token is per account.
fn free_loopback_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

/// Stops one account's bridge, if it is running here.
pub async fn stop(account_id: &str) {
    let Some(mut running) = bridges().lock().await.remove(account_id) else {
        return;
    };
    // The bridge owns CLI child processes and confirms their termination, so it
    // is asked to shut down before it is killed.
    let _ = running.handle.post("/runtime/shutdown", json!({})).await;
    let _ = tokio::time::timeout(Duration::from_secs(10), running.child.wait()).await;
    let _ = running.child.start_kill();
}

/// Stops every bridge this process started. Returns how many there were.
///
/// Called from the shutdown path: a bridge holds the account's `account.lock`
/// and its canonical credential directory, so one left behind blocks the next
/// Core from ever starting that account. Each stop is bounded (the `/runtime`
/// call, then a 10 s wait, then a kill), and they run in sequence because a
/// shutdown that stops on a wedged bridge is worse than one that takes longer.
pub async fn stop_all() -> usize {
    let accounts: Vec<String> = bridges().lock().await.keys().cloned().collect();
    let count = accounts.len();
    for account_id in accounts {
        stop(&account_id).await;
    }
    count
}

/// Releases bridges nothing has used for `IDLE_GRACE`. Returns how many were
/// stopped.
///
/// "Used" is a call through `ensure_runtime` or `running_bridge`, not a session
/// count read from the bridge: an account with an open session is being polled
/// by whatever owns that session, and one that is not polled is one nobody is
/// waiting on.
pub async fn release_idle() -> Result<usize> {
    let expired: Vec<String> = {
        let bridges = bridges().lock().await;
        bridges
            .iter()
            .filter(|(_, running)| running.last_used.elapsed() >= IDLE_GRACE)
            .map(|(account_id, _)| account_id.clone())
            .collect()
    };
    let count = expired.len();
    for account_id in expired {
        stop(&account_id).await;
    }
    Ok(count)
}

// =============================================================================
// Materialization (design §D.2)
// =============================================================================

/// Writes the stored credential of `account_id` into its bridge on this node,
/// when this node is not already holding that revision.
///
/// The bridge is the only writer of the canonical credential file, so the
/// material is handed to it over its authenticated loopback surface instead of
/// being written to the account directory from here: a second writer would race
/// the bridge's own atomic replacement, and the one invariant the bridge rests
/// on is that it alone decides what the account's credential is.
pub async fn ensure_account_materialized(
    db: &DbPool,
    cipher: &crate::crypto::SettingsCipher,
    node_id: &str,
    account_id: &str,
) -> Result<()> {
    // Before anything is read out of the store: writing a credential to a node
    // the operator excluded is the one thing the matrix toggle exists to
    // prevent, and it is checked here rather than only at the bridge because
    // this is where the material leaves the store.
    require_receiving_accounts(db, node_id)?;
    let account = store::get_account(db, account_id)?
        .ok_or_else(|| anyhow!("provider account '{account_id}' does not exist"))?;
    let Some(summary) = store::credential_summary(db, account_id)? else {
        // Nothing to write is not an error: this is exactly the state a first
        // login starts from, and it is the login that fills it.
        store::set_node_state(db, account_id, node_id, 0, "absent", None)?;
        return Ok(());
    };
    let current = store::node_state(db, account_id, node_id)?;
    if current
        .as_ref()
        .is_some_and(|row| row.applied_revision == summary.revision && row.runtime_state == "ready")
    {
        return Ok(());
    }
    store::set_node_state(
        db,
        account_id,
        node_id,
        current.map(|row| row.applied_revision).unwrap_or(0),
        "materializing",
        None,
    )?;
    let applied = async {
        let material = store::credential_material(db, cipher, account_id)?
            .ok_or_else(|| anyhow!("the stored credential could not be opened on this node"))?;
        let bridge = ensure_runtime(db, node_id, account_id, &account.engine_id).await?;
        bridge
            .call(
                reqwest::Method::PUT,
                "/account/credential",
                Some(json!({"material": material})),
            )
            .await
    }
    .await;
    match applied {
        Ok(_) => {
            store::set_node_state(db, account_id, node_id, summary.revision, "ready", None)?;
            Ok(())
        }
        Err(error) => {
            let detail = format!("{error:#}");
            // The revision this node already holds stays: a failure to apply a
            // NEWER one says nothing about the older one it is still running on.
            store::set_node_error(db, account_id, node_id, &detail)?;
            Err(anyhow!(detail))
        }
    }
}

/// Reads the account's canonical credential back out of its bridge.
///
/// Core is the trust root of the pair: the bridge holds the file, Core holds
/// the encrypted record every other node will be given, and there is no other
/// way for a sign-in that happened inside the bridge to reach the store. The
/// material is returned and never logged — the caller seals it immediately.
pub async fn read_bridge_credential(
    bridge: &BridgeHandle,
) -> Result<Option<(String, String, Option<String>)>> {
    let answer = bridge.get("/account/credential").await?;
    if !answer
        .get("present")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(None);
    }
    let material = answer
        .get("material")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("the bridge reported a credential without its material"))?;
    let sha256 = answer
        .get("sha256")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("the bridge reported a credential without its digest"))?;
    Ok(Some((
        material.to_string(),
        sha256.to_string(),
        answer
            .get("identity")
            .and_then(Value::as_str)
            .map(str::to_string),
    )))
}

/// A bridge surface served by the test itself.
///
/// The conversations Core has with a bridge are HTTP over loopback with a
/// bearer token, and every one of them is worth exercising without installing a
/// vendor CLI — the alternative is that the credential-adoption paths are only
/// ever run by hand. Registering a handle here makes `running_bridge` answer
/// with it; nothing else in this module changes behaviour under `cfg(test)`.
#[cfg(test)]
pub(crate) mod testing {
    use super::BridgeHandle;
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    fn registry() -> &'static Mutex<HashMap<String, BridgeHandle>> {
        static REGISTRY: OnceLock<Mutex<HashMap<String, BridgeHandle>>> = OnceLock::new();
        REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
    }

    pub(crate) fn registered_bridge(account_id: &str) -> Option<BridgeHandle> {
        registry().lock().ok()?.get(account_id).cloned()
    }

    pub(crate) fn register(account_id: &str, handle: BridgeHandle) {
        registry()
            .lock()
            .expect("test bridge registry")
            .insert(account_id.to_string(), handle);
    }

    pub(crate) fn forget(account_id: &str) {
        registry()
            .lock()
            .expect("test bridge registry")
            .remove(account_id);
    }

    /// Serves fixed JSON bodies by path prefix on loopback, closing every
    /// connection — which is all the client this crate uses needs.
    pub(crate) async fn fake_bridge(
        engine_id: &str,
        answers: Vec<(&'static str, String)>,
    ) -> BridgeHandle {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a fake bridge");
        let port = listener.local_addr().expect("local addr").port();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let answers = answers.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buffer = [0_u8; 1024];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        match socket.read(&mut buffer).await {
                            Ok(0) | Err(_) => return,
                            Ok(read) => request.extend_from_slice(&buffer[..read]),
                        }
                    }
                    let text = String::from_utf8_lossy(&request).to_string();
                    let path = text.split_whitespace().nth(1).unwrap_or("").to_string();
                    let body = answers
                        .iter()
                        .find(|(prefix, _)| path.starts_with(prefix))
                        .map(|(_, body)| body.clone())
                        .unwrap_or_else(|| "{}".to_string());
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        BridgeHandle::for_test(engine_id, port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receiving_node(db: &DbPool, node_id: &str) {
        crate::provider_accounts::repository::set_receives_accounts(db, node_id, true, None)
            .expect("runtime node");
    }

    /// An engine nobody installed is refused BEFORE a process is started: the
    /// alternative is a bridge that comes up, answers its health probe and
    /// fails the first turn on a missing binary.
    #[tokio::test]
    async fn a_runtime_is_refused_on_a_node_the_engine_is_not_installed_on() {
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("db");
        receiving_node(&db, "node-1");
        let error = ensure_runtime(&db, "node-1", "acc-1", "codex")
            .await
            .expect_err("no engine row exists");
        assert!(error.to_string().contains("not installed"), "{error}");
        assert!(bridges().lock().await.is_empty());
    }

    /// The node matrix toggle is a decision about credentials, so it is checked
    /// where a credential would reach a process — before the engine is even
    /// looked at, because "not installed" would tell the caller to install one.
    #[tokio::test]
    async fn a_node_that_may_not_receive_accounts_starts_no_bridge() {
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("db");
        let error = ensure_runtime(&db, "node-1", "acc-1", "codex")
            .await
            .expect_err("this node holds no accounts");
        assert_eq!(error.to_string(), NOT_RECEIVING_ACCOUNTS);
        assert!(bridges().lock().await.is_empty());

        store::create_account(
            &db,
            &crate::provider_accounts::NewAccount {
                account_id: "acc-1".to_string(),
                org_id: crate::services::org::DEFAULT_ORG_ID.to_string(),
                engine_id: "codex".to_string(),
                display_name: "Codex".to_string(),
                scope: "global".to_string(),
                owner_user_id: None,
                credential_kind: "provider_login".to_string(),
                created_by: "admin".to_string(),
            },
        )
        .expect("account");
        let cipher = crate::crypto::SettingsCipher::new(&[9u8; 32]);
        let error = ensure_account_materialized(&db, &cipher, "node-1", "acc-1")
            .await
            .expect_err("no credential may be written out here");
        assert_eq!(error.to_string(), NOT_RECEIVING_ACCOUNTS);
        assert!(
            store::node_state(&db, "acc-1", "node-1")
                .expect("state")
                .is_none(),
            "a refused node must not even be recorded as holding the account"
        );
    }

    #[tokio::test]
    async fn an_unknown_engine_is_never_installed() {
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("db");
        assert!(install_engine(&db, "not-an-engine", "admin").await.is_err());
        assert!(uninstall_engine(&db, "not-an-engine", "admin")
            .await
            .is_err());
    }

    /// Materializing an account that has no credential is the ordinary state
    /// before a first login, and it records exactly that instead of failing.
    #[tokio::test]
    async fn an_account_without_a_credential_materializes_as_absent() {
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("db");
        let cipher = crate::crypto::SettingsCipher::new(&[3u8; 32]);
        receiving_node(&db, "node-1");
        store::create_account(
            &db,
            &crate::provider_accounts::NewAccount {
                account_id: "acc-1".to_string(),
                org_id: crate::services::org::DEFAULT_ORG_ID.to_string(),
                engine_id: "codex".to_string(),
                display_name: "Codex".to_string(),
                scope: "global".to_string(),
                owner_user_id: None,
                credential_kind: "provider_login".to_string(),
                created_by: "admin".to_string(),
            },
        )
        .expect("account");
        ensure_account_materialized(&db, &cipher, "node-1", "acc-1")
            .await
            .expect("materialize");
        let state = store::node_state(&db, "acc-1", "node-1")
            .expect("state")
            .expect("row");
        assert_eq!(state.runtime_state, "absent");
        assert_eq!(state.applied_revision, 0);
    }

    /// A node already holding the stored revision does nothing — no bridge is
    /// started, which is what makes an ordinary turn on a materialized account
    /// free. A rotation moves the stored revision past the applied one and the
    /// same call then has to reach the bridge, so on a node without a runtime
    /// it fails and says so in the row.
    #[tokio::test]
    async fn a_node_holding_the_stored_revision_is_left_alone() {
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("db");
        let cipher = crate::crypto::SettingsCipher::new(&[4u8; 32]);
        receiving_node(&db, "node-1");
        store::create_account(
            &db,
            &crate::provider_accounts::NewAccount {
                account_id: "acc-2".to_string(),
                org_id: crate::services::org::DEFAULT_ORG_ID.to_string(),
                engine_id: "codex".to_string(),
                display_name: "Codex".to_string(),
                scope: "global".to_string(),
                owner_user_id: None,
                credential_kind: "api_key".to_string(),
                created_by: "admin".to_string(),
            },
        )
        .expect("account");
        store::mint_credential(
            &db,
            &cipher,
            "acc-2",
            "sk-one",
            &crate::provider_accounts::CredentialMeta::default(),
        )
        .expect("credential");
        store::set_node_state(&db, "acc-2", "node-1", 1, "ready", None).expect("state");

        ensure_account_materialized(&db, &cipher, "node-1", "acc-2")
            .await
            .expect("a node at the stored revision has nothing to do");
        assert!(
            bridges().lock().await.is_empty(),
            "no bridge may be started for a credential this node already holds"
        );

        store::mint_credential(
            &db,
            &cipher,
            "acc-2",
            "sk-two",
            &crate::provider_accounts::CredentialMeta::default(),
        )
        .expect("rotation");
        let error = ensure_account_materialized(&db, &cipher, "node-1", "acc-2")
            .await
            .expect_err("the new revision has to reach a bridge this node cannot start");
        assert!(error.to_string().contains("not installed"), "{error}");
        let state = store::node_state(&db, "acc-2", "node-1")
            .expect("state")
            .expect("row");
        assert_eq!(
            state.runtime_state, "error",
            "a failed materialization must not read as ready"
        );
        assert!(state.last_error.is_some());
        assert_eq!(
            state.applied_revision, 1,
            "the node still holds revision 1; a failure to apply 2 does not un-apply it"
        );
    }

    /// The registry is a map of processes, and a lookup in it must not wait on
    /// a start. The start slot is what serializes two starts of ONE account, so
    /// a failed start releases it and the next attempt runs instead of
    /// deadlocking behind it.
    #[tokio::test]
    async fn a_failed_start_leaves_the_account_startable_again() {
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("db");
        receiving_node(&db, "node-9");
        for _ in 0..2 {
            let error = ensure_runtime(&db, "node-9", "acc-9", "codex")
                .await
                .expect_err("no engine installed");
            assert!(error.to_string().contains("not installed"), "{error}");
        }
        assert!(
            running_bridge("acc-9").await.is_none(),
            "a failed start registers nothing"
        );
    }

    /// The material and the identity come back exactly as the bridge holds
    /// them, and a bridge with no credential answers that rather than failing —
    /// which is what a first sign-in starts from.
    #[tokio::test]
    async fn the_bridge_credential_is_read_verbatim() {
        let bridge = testing::fake_bridge(
            "codex",
            vec![(
                "/account/credential",
                serde_json::json!({
                    "present": true,
                    "engine": "codex",
                    "sha256": "ab".repeat(32),
                    "identity": "account:acct-7",
                    "material": "{\"tokens\":{\"account_id\":\"acct-7\"}}",
                })
                .to_string(),
            )],
        )
        .await;
        let (material, sha256, identity) = read_bridge_credential(&bridge)
            .await
            .expect("read")
            .expect("present");
        assert_eq!(material, "{\"tokens\":{\"account_id\":\"acct-7\"}}");
        assert_eq!(sha256, "ab".repeat(32));
        assert_eq!(identity.as_deref(), Some("account:acct-7"));

        let empty = testing::fake_bridge(
            "codex",
            vec![(
                "/account/credential",
                serde_json::json!({"present": false, "engine": "codex"}).to_string(),
            )],
        )
        .await;
        assert!(read_bridge_credential(&empty)
            .await
            .expect("read")
            .is_none());
    }
}
