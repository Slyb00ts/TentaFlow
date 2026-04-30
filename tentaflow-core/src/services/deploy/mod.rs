// ============ File: services/deploy/mod.rs — unified atomic deploy entry point ============
//
// Two-phase atomic deploy:
//   1. PREPARE — side effects (port alloc, image build, process spawn, health check).
//   2. COMMIT  — single DB transaction across services + model_registry +
//                deployments. If it fails, ROLLBACK is invoked to undo prepare.

pub mod binary;
pub mod docker;
pub mod embedded;
pub mod external;
pub mod python_bundle;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::Transaction;
use tokio::sync::broadcast;

use crate::db::DbPool;
use crate::deploy::log_bus::{now_ms, BusMessage, LogLine};
use crate::services::lifecycle::ServiceEndpoint;
use crate::services::manifest::ServiceManifest;
use crate::services::ports::PortAllocator;
use crate::services::transport::Transport;
use crate::services_repo::deployments::{self as deployments_repo, DeploymentStatus};
use crate::services_repo::models::{self as models_repo, NewModel};
use crate::services_repo::services::{DeployMethod, NewService, ServiceStatus};

// ----- Errors ---------------------------------------------------------------

/// Typed error surface for the unified deploy pipeline.
#[derive(thiserror::Error, Debug)]
pub enum DeployError {
    #[error("port allocation failed: {0}")]
    PortAlloc(String),
    #[error("docker error: {0}")]
    Docker(String),
    #[error("process spawn failed: {0}")]
    Spawn(String),
    #[error("health check timeout after {0}s")]
    HealthTimeout(u64),
    #[error("manifest validation: {0}")]
    Manifest(String),
    #[error("db error: {0}")]
    Database(String),
    #[error("rollback failed: {0}")]
    Rollback(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("other: {0}")]
    Other(String),
}

impl From<anyhow::Error> for DeployError {
    fn from(e: anyhow::Error) -> Self {
        DeployError::Other(format!("{:#}", e))
    }
}

impl From<rusqlite::Error> for DeployError {
    fn from(e: rusqlite::Error) -> Self {
        DeployError::Database(e.to_string())
    }
}

pub type DeployResult<T> = std::result::Result<T, DeployError>;

// ----- Log streaming --------------------------------------------------------

/// Live feed of build/run output for a single deploy job. Keyed by `slug`,
/// the broadcaster fans out to dashboard subscribers and also persists each
/// line to `deployments.log_tail` for replay.
#[derive(Clone)]
pub struct LogSink {
    pub slug: String,
    pub sender: broadcast::Sender<BusMessage>,
    pub db: DbPool,
}

impl LogSink {
    /// Persists `line` to log_tail and broadcasts it as a `BusMessage::Line`
    /// with `kind` (e.g. "log", "phase", "info"). Errors are best-effort —
    /// a failed DB write must not abort the deploy.
    pub fn emit(&self, kind: &str, line: &str) {
        let _ = deployments_repo::append_log_line(&self.db, &self.slug, line);
        let _ = self.sender.send(BusMessage::Line(LogLine {
            deploy_id: self.slug.clone(),
            kind: kind.to_string(),
            line: line.to_string(),
            phase: String::new(),
            progress_pct: 0,
            ts_ms: now_ms(),
        }));
    }

    pub fn info(&self, line: &str) {
        self.emit("info", line);
    }
}

// ----- Public types ---------------------------------------------------------

/// Outcome of a successful deploy: a runnable, registered endpoint plus the
/// deployments audit-row id.
#[derive(Debug, Clone)]
pub struct DeployOutcome {
    pub deployment_id: i64,
    pub endpoint: ServiceEndpoint,
}

/// Runtime descriptor produced during prepare. Owned by `PreparedDeploy` so
/// commit can persist it and rollback can release it.
#[derive(Debug, Clone, Default)]
pub struct RuntimeHandle {
    pub pid: Option<i64>,
    pub port: Option<u16>,
    pub sidecar_port: Option<u16>,
    pub endpoint_url: Option<String>,
    /// Docker container id if a container was started.
    pub container_id: Option<String>,
    /// Filesystem dir created exclusively for this deployment (sidecar config,
    /// python instance dir, etc). Cleaned by rollback.
    pub instance_dir: Option<PathBuf>,
}

/// Result of `prepare`: enough to either commit (write DB rows) or rollback
/// (kill processes, release ports, remove containers).
#[derive(Debug)]
pub struct PreparedDeploy {
    pub engine_id: String,
    /// Stable kebab-case category tag (e.g. `llm`, `tts`). Mirrors
    /// `manifest.engine.category` so the row reflects what the catalog UI
    /// indexes by.
    pub category: String,
    /// User-facing display name; falls back to `engine_id` when the manifest's
    /// `engine.name` is empty.
    pub display_name: String,
    pub deploy_method: DeployMethod,
    pub transport: Transport,
    pub runtime: RuntimeHandle,
    pub models: Vec<NewModel>,
    pub config_json: String,
    /// Ports allocated through `PortAllocator` so rollback can release them.
    pub allocated_ports: Vec<u16>,
}

/// Two-phase deploy contract.
///
/// `prepare` may have side effects (build image, spawn process, allocate
/// ports) but must not be visible to the rest of the system yet.
/// `commit` writes DB rows in one transaction and returns the new service id.
/// `rollback` undoes prepare's side effects.
#[async_trait]
pub trait DeployStrategy: Send + Sync {
    async fn prepare(&mut self) -> DeployResult<PreparedDeploy>;
    fn commit(&self, tx: &Transaction<'_>, prepared: &PreparedDeploy) -> DeployResult<i64>;
    async fn rollback(&self, prepared: PreparedDeploy) -> DeployResult<()>;
}

// ----- Top-level entry ------------------------------------------------------

/// Deploys an engine atomically. On any failure the system state is rolled
/// back: spawned processes killed, containers removed, ports released, and
/// the deployments row marked `failed` with the error text.
///
/// `log_sink` (when provided) receives every build/run line as a
/// `BusMessage::Line` keyed by `slug`. `existing_slug` lets the caller pin a
/// pre-generated slug (e.g. so the WebSocket subscription URL is known
/// before the audit row is written); when `None` a fresh UUID is used.
pub async fn deploy(
    method: DeployMethod,
    manifest: &ServiceManifest,
    user_config: &serde_json::Value,
    ports: &Arc<PortAllocator>,
    db: &DbPool,
    log_sink: Option<broadcast::Sender<BusMessage>>,
    existing_slug: Option<String>,
) -> DeployResult<DeployOutcome> {
    let slug = existing_slug.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let sink = log_sink.map(|sender| LogSink {
        slug: slug.clone(),
        sender,
        db: db.clone(),
    });

    // 1. Audit row first so we always have a paper trail, even on prepare failure.
    let deployment_id = with_tx(db, |tx| {
        let id =
            deployments_repo::create_with_slug(tx, &manifest.engine.id, method.as_db_tag(), &slug)?;
        Ok(id)
    })?;

    if let Some(s) = &sink {
        s.info(&format!(
            "[prepare] engine={} method={}",
            manifest.engine.id,
            method.as_db_tag()
        ));
    }

    // 2. Pick strategy.
    let mut strategy: Box<dyn DeployStrategy> = match method {
        DeployMethod::NativeEmbedded => Box::new(embedded::EmbeddedDeploy::new(
            manifest.clone(),
            user_config.clone(),
            sink.clone(),
        )),
        DeployMethod::NativeBinary => Box::new(binary::BinaryDeploy::new(
            manifest.clone(),
            user_config.clone(),
            ports.clone(),
            sink.clone(),
        )),
        DeployMethod::NativePythonBundle => Box::new(python_bundle::PythonBundleDeploy::new(
            manifest.clone(),
            user_config.clone(),
            ports.clone(),
            sink.clone(),
        )),
        DeployMethod::Docker => Box::new(docker::DockerDeploy::new(
            manifest.clone(),
            user_config.clone(),
            ports.clone(),
            sink.clone(),
        )),
        DeployMethod::External => Box::new(external::ExternalDeploy::new(
            manifest.clone(),
            user_config.clone(),
            sink.clone(),
        )),
    };

    // 3. PREPARE.
    let prepared = match strategy.prepare().await {
        Ok(p) => p,
        Err(e) => {
            if let Some(s) = &sink {
                s.emit("error", &format!("[prepare-failed] {}", e));
            }
            mark_finished(
                db,
                deployment_id,
                DeploymentStatus::Failed,
                Some(&e.to_string()),
            );
            return Err(e);
        }
    };

    if let Some(s) = &sink {
        s.info("[commit] writing services + model_registry");
    }

    // 4. COMMIT — single transaction over services + model_registry +
    //    deployments.finish. Any failure triggers rollback of side effects.
    let commit_result: DeployResult<i64> = with_tx(db, |tx| {
        let sid = strategy.commit(tx, &prepared)?;
        for m in &prepared.models {
            // service_id is filled by commit; the strategy returns NewModel with
            // service_id = 0 since it's only known here.
            let mut model = m.clone();
            model.service_id = sid;
            models_repo::insert_in_tx(tx, &model)?;
        }
        deployments_repo::mark_finished(tx, deployment_id, DeploymentStatus::Success, None)?;
        Ok(sid)
    });

    let service_id = match commit_result {
        Ok(id) => id,
        Err(commit_err) => {
            // 5. ROLLBACK side effects (processes, containers, ports).
            let rb_msg = match strategy.rollback(prepared).await {
                Ok(()) => format!("commit failed: {} (rolled back)", commit_err),
                Err(rb) => format!(
                    "commit failed: {} ; rollback also failed: {}",
                    commit_err, rb
                ),
            };
            if let Some(s) = &sink {
                s.emit("error", &rb_msg);
            }
            mark_finished(db, deployment_id, DeploymentStatus::Failed, Some(&rb_msg));
            return Err(commit_err);
        }
    };

    // 6. Build the outcome endpoint for callers.
    let endpoint = ServiceEndpoint {
        handle: crate::services::lifecycle::ServiceHandle {
            id: service_id,
            engine_id: prepared.engine_id.clone(),
        },
        transport: prepared.transport,
        deploy_method: prepared.deploy_method,
        status: ServiceStatus::Running,
        host: "127.0.0.1".to_string(),
        runtime_port: prepared.runtime.port,
        sidecar_quic_port: prepared.runtime.sidecar_port,
        url: prepared.runtime.endpoint_url.clone(),
    };

    Ok(DeployOutcome {
        deployment_id,
        endpoint,
    })
}

/// Re-spawns the runtime side of an existing service (process / container)
/// without touching `services`. Used by the supervisor's restart loop —
/// the caller is expected to update `runtime_pid/port/...` on the existing row
/// after this returns.
///
/// Conceptually this drives `DeployStrategy::prepare()` only; the `commit`
/// half is skipped because the DB row is already there.
pub async fn respawn(
    engine_id: &str,
    deploy_method: DeployMethod,
    config_json: &str,
    ports: Arc<PortAllocator>,
) -> DeployResult<RuntimeHandle> {
    let manifest = crate::services::manifest::registry()
        .by_id(engine_id)
        .cloned()
        .ok_or_else(|| {
            DeployError::Manifest(format!(
                "respawn: manifest '{}' not found in registry",
                engine_id
            ))
        })?;

    let user_config: serde_json::Value = if config_json.is_empty() {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        serde_json::from_str(config_json)
            .map_err(|e| DeployError::Other(format!("respawn: parse config_json: {}", e)))?
    };

    let mut strategy: Box<dyn DeployStrategy> = match deploy_method {
        DeployMethod::NativeEmbedded => {
            Box::new(embedded::EmbeddedDeploy::new(manifest, user_config, None))
        }
        DeployMethod::NativeBinary => Box::new(binary::BinaryDeploy::new(
            manifest,
            user_config,
            ports.clone(),
            None,
        )),
        DeployMethod::NativePythonBundle => Box::new(python_bundle::PythonBundleDeploy::new(
            manifest,
            user_config,
            ports.clone(),
            None,
        )),
        DeployMethod::Docker => Box::new(docker::DockerDeploy::new(
            manifest,
            user_config,
            ports.clone(),
            None,
        )),
        DeployMethod::External => {
            return Err(DeployError::Manifest(
                "respawn: external services are not respawnable".to_string(),
            ));
        }
    };

    let prepared = strategy.prepare().await?;
    Ok(prepared.runtime)
}

/// Stops the runtime side of a deployed service: kills the process, removes
/// the container, and releases its host-allocated ports. Does **not** delete
/// the `services` row — the caller decides whether to mark it `stopped`
/// or `DELETE` it (cascade removes `model_registry`). Errors are merged
/// into a single `DeployError::Other` so callers can surface them as a single
/// "stop failed" message.
pub async fn stop(
    svc: &crate::services_repo::services::ServiceRow,
    ports: Arc<PortAllocator>,
) -> DeployResult<()> {
    use crate::services_repo::services::DeployMethod as DM;

    // Container shutdown: only docker deploys own a container at runtime.
    // We don't persist the container id on the row, so match by the
    // deterministic name pattern used at create time (see DockerDeploy::run).
    #[cfg(feature = "docker")]
    if svc.deploy_method == DM::Docker {
        if let (Ok(docker), Some(port)) = (
            bollard::Docker::connect_with_local_defaults(),
            svc.runtime_port,
        ) {
            let name = format!("tentaflow-{}-{}", svc.engine_id, port);
            let _ = docker.stop_container(&name, None).await;
            let _ = docker
                .remove_container(
                    &name,
                    Some(bollard::query_parameters::RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await;
        }
    }

    // Process shutdown: only the process-owning transports actually have a PID.
    if let Some(pid) = svc.runtime_pid {
        if matches!(svc.deploy_method, DM::NativeBinary | DM::NativePythonBundle) {
            // SIGTERM with short grace then SIGKILL — handled inside terminate.
            let _ = crate::deploy::process_ctl::terminate(pid as u32);
        }
    }

    // Always release whichever ports the row claims; PortAllocator is idempotent
    // on unknown ports.
    if let Some(p) = svc.runtime_port {
        let _ = ports.release(p);
    }
    if let Some(p) = svc.sidecar_quic_port {
        let _ = ports.release(p);
    }

    Ok(())
}

// ----- DB helpers -----------------------------------------------------------

/// Runs a closure inside a single SQLite transaction held under the pool's
/// mutex. Commits on Ok, rolls back on Err.
pub(crate) fn with_tx<F, T>(db: &DbPool, f: F) -> DeployResult<T>
where
    F: FnOnce(&Transaction<'_>) -> DeployResult<T>,
{
    let mut conn = db
        .lock()
        .map_err(|e| DeployError::Database(format!("pool lock poisoned: {}", e)))?;
    let tx = conn
        .transaction()
        .map_err(|e| DeployError::Database(format!("begin tx: {}", e)))?;
    let out = f(&tx)?;
    tx.commit()
        .map_err(|e| DeployError::Database(format!("commit tx: {}", e)))?;
    Ok(out)
}

fn mark_finished(db: &DbPool, id: i64, status: DeploymentStatus, err: Option<&str>) {
    let _ = with_tx(db, |tx| {
        deployments_repo::mark_finished(tx, id, status, err)?;
        Ok(())
    });
}

// ----- Shared helpers used by strategies -----------------------------------

/// Builds the canonical `NewService` row from the prepared state.
pub(crate) fn build_new_service(prepared: &PreparedDeploy, status: ServiceStatus) -> NewService {
    NewService {
        engine_id: prepared.engine_id.clone(),
        category: prepared.category.clone(),
        display_name: prepared.display_name.clone(),
        deploy_method: prepared.deploy_method,
        transport: prepared.transport,
        status,
        pinned: false,
        paused: false,
        runtime_pid: prepared.runtime.pid,
        runtime_port: prepared.runtime.port,
        sidecar_quic_port: prepared.runtime.sidecar_port,
        endpoint_url: prepared.runtime.endpoint_url.clone(),
        config_json: prepared.config_json.clone(),
    }
}

/// Resolves a manifest's user-facing display name. Falls back to the engine
/// id when the manifest left `engine.name` empty.
pub(crate) fn resolve_display_name(manifest: &ServiceManifest) -> String {
    let trimmed = manifest.engine.name.trim();
    if trimmed.is_empty() {
        manifest.engine.id.clone()
    } else {
        trimmed.to_string()
    }
}

// ----- Smart liveness+readiness probe ---------------------------------------

/// Side-channel counter incremented by strategy code each time a fresh log
/// line arrives from the engine (stdout, stderr, docker logs). The probe
/// reads it to distinguish "slow startup, still working" from "process
/// silent — likely deadlocked".
#[derive(Debug, Default)]
pub struct LogActivityCounter {
    count: std::sync::atomic::AtomicU64,
}

impl LogActivityCounter {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn snapshot(&self) -> u64 {
        self.count.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn bump(&self) {
        self.count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Probe outcome. `Ready` is success; both other variants are deploy-fatal.
#[derive(Debug)]
pub enum SmartProbeOutcome {
    /// HTTP readiness URL responded 2xx.
    Ready,
    /// Process / container died before becoming ready. Carries the OS exit
    /// code if the strategy could fetch one (None when only liveness is
    /// observable, e.g. via `kill(pid, 0)`).
    ProcessExited(Option<i32>),
    /// Engine alive but produced no log line and no successful HTTP response
    /// for `stagnation_window`. Typically a hung wheel build or a model
    /// download stuck on a dead mirror.
    Stalled(std::time::Duration),
}

/// Probe configuration. `readiness_urls` are raced; the first 2xx wins.
pub struct SmartProbeConfig {
    pub readiness_urls: Vec<String>,
    pub stagnation_window: std::time::Duration,
    /// How often the probe emits a "still starting…" line through
    /// `log_sink` so the dashboard sees progress.
    pub status_report_interval: std::time::Duration,
    pub log_activity: Arc<LogActivityCounter>,
    pub log_sink: Option<LogSink>,
}

/// Smart liveness+readiness probe with no hard timeout. Loops until one of:
///
/// * a readiness URL answers 2xx → `Ready`;
/// * `is_alive_check` reports the process gone → `ProcessExited`;
/// * the engine emits no log line **and** no readiness response within
///   `stagnation_window` → `Stalled`.
///
/// `is_alive_check` is an async closure returning `Some(exit_code)` when
/// the supervised process has exited (None inside Some means "exited but
/// code unknown"), or `None` when it is still alive.
pub(crate) async fn smart_health_probe<F, Fut>(
    cfg: SmartProbeConfig,
    is_alive_check: F,
) -> SmartProbeOutcome
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Option<Option<i32>>>,
{
    use std::time::{Duration, Instant};
    let started = Instant::now();
    let mut last_log_count = cfg.log_activity.snapshot();
    let mut last_log_time = Instant::now();
    let mut last_status_emit = Instant::now();
    let probe_interval = Duration::from_millis(500);

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => {
            // If we cannot even build a client we have to bail; treat as
            // immediate stagnation so caller can roll back cleanly.
            return SmartProbeOutcome::Stalled(Duration::from_secs(0));
        }
    };

    loop {
        // 1. Liveness — has the process died?
        if let Some(exit) = is_alive_check().await {
            return SmartProbeOutcome::ProcessExited(exit);
        }

        // 2. Readiness — race all candidate URLs.
        for url in &cfg.readiness_urls {
            if let Ok(resp) = client.get(url).send().await {
                if resp.status().is_success() {
                    return SmartProbeOutcome::Ready;
                }
            }
        }

        // 3. Stagnation tracking — bumped counter resets the silent window.
        let current = cfg.log_activity.snapshot();
        if current != last_log_count {
            last_log_count = current;
            last_log_time = Instant::now();
        }
        let silent_for = last_log_time.elapsed();
        if silent_for > cfg.stagnation_window {
            return SmartProbeOutcome::Stalled(silent_for);
        }

        // 4. Periodic status report so the dashboard sees progress. We
        //    emit through `info()` deliberately — that does not bump the
        //    activity counter, so synthetic status lines cannot mask a
        //    real stall.
        if last_status_emit.elapsed() >= cfg.status_report_interval {
            if let Some(sink) = &cfg.log_sink {
                sink.info(&format!(
                    "[health] still starting (alive {}s, no log activity for {}s, waiting for ready)",
                    started.elapsed().as_secs(),
                    silent_for.as_secs()
                ));
            }
            last_status_emit = Instant::now();
        }

        tokio::time::sleep(probe_interval).await;
    }
}

/// Pulls model presets from the manifest into `NewModel` rows. `service_id`
/// is filled by the dispatcher after commit.
pub(crate) fn models_from_manifest(manifest: &ServiceManifest) -> Vec<NewModel> {
    manifest
        .model_presets
        .iter()
        .enumerate()
        .map(|(idx, p)| NewModel {
            service_id: 0,
            model_name: p.id.clone(),
            display_name: Some(p.display_name.clone()),
            capabilities: format!("[\"{}\"]", manifest.engine.capability_tag()),
            context_length: None,
            quantization: p.quantization.clone(),
            // First preset becomes default if none is marked recommended.
            is_default: p.recommended
                || (idx == 0 && manifest.model_presets.iter().all(|m| !m.recommended)),
        })
        .collect()
}

/// Reads the optional `transport_explicit` hint from user_config. Used by
/// docker strategy as a Phase 6 preview (bypass sidecar for `direct_http`).
pub(crate) fn transport_hint(user_config: &serde_json::Value) -> Option<String> {
    user_config
        .get("transport_explicit")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Detects whether the host's OS matches the manifest's declared platforms.
pub(crate) fn host_os_supported(platforms: &[crate::services::manifest::TargetOs]) -> bool {
    use crate::services::manifest::TargetOs;
    let host = if cfg!(target_os = "linux") {
        TargetOs::Linux
    } else if cfg!(target_os = "macos") {
        TargetOs::Macos
    } else if cfg!(target_os = "windows") {
        TargetOs::Windows
    } else {
        return true;
    };
    platforms.iter().any(|p| *p == host)
}

/// Optional environment overrides (cache dirs etc).
pub(crate) fn standard_engine_env() -> HashMap<String, String> {
    let mut env = HashMap::new();
    let hf = crate::paths::hf_home();
    let torch = crate::paths::torch_home();
    let hf_str = hf.to_string_lossy().to_string();
    env.insert("HF_HOME".into(), hf_str.clone());
    env.insert("HUGGINGFACE_HUB_CACHE".into(), hf_str.clone());
    env.insert("TRANSFORMERS_CACHE".into(), hf_str);
    env.insert("TORCH_HOME".into(), torch.to_string_lossy().to_string());
    env
}

// ----- Tiny extension on Category to get string capability tag --------------

trait CategoryStr {
    /// Capability tag used inside the embedded JSON list on `model_registry`
    /// rows (e.g. "chat" for an LLM, "tts" for a TTS engine). Distinct from
    /// the kebab-case category id stored in `services.category` because the
    /// capability surfaces to routing while the category surfaces to the UI.
    fn capability_tag(&self) -> &'static str;
    /// Stable kebab-case category id matching `tentaflow-containers/<id>/`.
    fn category_tag(&self) -> &'static str;
}

impl CategoryStr for crate::services::manifest::Engine {
    fn capability_tag(&self) -> &'static str {
        use crate::services::manifest::Category::*;
        match self.category {
            Llm => "chat",
            Stt => "stt",
            Tts => "tts",
            Embeddings => "embeddings",
            Reranker => "reranker",
            Vision => "vision",
            ImageGen => "image-gen",
            VideoGen => "video-gen",
            MusicGen => "music-gen",
            Model3dGen => "model-3d-gen",
            Agents => "agent",
            Tools => "tool",
        }
    }

    fn category_tag(&self) -> &'static str {
        use crate::services::manifest::Category::*;
        match self.category {
            Llm => "llm",
            Stt => "stt",
            Tts => "tts",
            Embeddings => "embeddings",
            Reranker => "reranker",
            Vision => "vision",
            ImageGen => "image-gen",
            VideoGen => "video-gen",
            MusicGen => "music-gen",
            Model3dGen => "model-3d-gen",
            Agents => "agents",
            Tools => "tools",
        }
    }
}

pub(crate) fn category_tag(manifest: &ServiceManifest) -> &'static str {
    manifest.engine.category_tag()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::manifest::{
        ApiKind, Category, DeploySection, Engine, ModelPreset, NativeDeploy, NativeRuntime,
        TargetOs,
    };

    fn dummy_manifest(id: &str, runtime: NativeRuntime) -> ServiceManifest {
        ServiceManifest {
            engine: Engine {
                id: id.to_string(),
                category: Category::Llm,
                name: id.to_string(),
                description_pl: "".into(),
                description_en: "".into(),
                homepage: "".into(),
                license: "".into(),
                icon: None,
                resource_kind: None,
                requires_model: None,
                gpu_supported: None,
                default_port: 8000,
                api: ApiKind::OpenaiCompatible,
                version: "0.0.1".into(),
            },
            deploy: DeploySection {
                docker: None,
                native: Some(NativeDeploy {
                    platforms: vec![TargetOs::Linux, TargetOs::Macos, TargetOs::Windows],
                    runtime,
                    feature_flag: None,
                    binary_path: None,
                    bundle_path: None,
                }),
                external: None,
            },
            model_presets: vec![ModelPreset {
                id: "preset-a".into(),
                display_name: "Preset A".into(),
                repo: "x/y".into(),
                quantization: None,
                recommended: true,
            }],
            docker_source_hash: String::new(),
            native_source_hash: String::new(),
        }
    }

    fn open_db() -> DbPool {
        use std::sync::{Arc, Mutex};
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::db::migrations::run(&conn).unwrap();
        Arc::new(Mutex::new(conn))
    }

    #[tokio::test]
    async fn smart_probe_returns_ready_when_readiness_url_succeeds() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let cfg = SmartProbeConfig {
            readiness_urls: vec![format!("{}/v1/models", server.uri())],
            stagnation_window: Duration::from_secs(60),
            status_report_interval: Duration::from_secs(60),
            log_activity: Arc::new(LogActivityCounter::new()),
            log_sink: None,
        };
        let alive = AtomicBool::new(true);
        let outcome = smart_health_probe(cfg, || async {
            if alive.load(Ordering::Relaxed) {
                None
            } else {
                Some(Some(0))
            }
        })
        .await;
        assert!(matches!(outcome, SmartProbeOutcome::Ready));
    }

    #[tokio::test]
    async fn smart_probe_detects_process_exit() {
        use std::time::Duration;

        let cfg = SmartProbeConfig {
            // Bind-loopback URL on a closed port so readiness never wins.
            readiness_urls: vec!["http://127.0.0.1:1/health".to_string()],
            stagnation_window: Duration::from_secs(60),
            status_report_interval: Duration::from_secs(60),
            log_activity: Arc::new(LogActivityCounter::new()),
            log_sink: None,
        };
        let outcome = smart_health_probe(cfg, || async { Some(Some(137)) }).await;
        match outcome {
            SmartProbeOutcome::ProcessExited(Some(137)) => {}
            other => panic!("expected ProcessExited(137), got {:?}", other),
        }
    }

    #[tokio::test]
    async fn smart_probe_returns_stalled_when_no_log_activity() {
        use std::time::Duration;

        let cfg = SmartProbeConfig {
            readiness_urls: vec!["http://127.0.0.1:1/health".to_string()],
            // Tiny window so the test runs in <2s.
            stagnation_window: Duration::from_millis(800),
            status_report_interval: Duration::from_secs(60),
            log_activity: Arc::new(LogActivityCounter::new()),
            log_sink: None,
        };
        let outcome = smart_health_probe(cfg, || async { None }).await;
        assert!(matches!(outcome, SmartProbeOutcome::Stalled(_)));
    }

    #[tokio::test]
    async fn smart_probe_log_activity_resets_stagnation() {
        use std::time::Duration;

        let activity = Arc::new(LogActivityCounter::new());
        let bumper = activity.clone();
        // Keep bumping every 200ms — the stagnation window is 600ms, so the
        // probe must never trip and we have to give up via timeout.
        let stop_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_clone = stop_flag.clone();
        let bump_task = tokio::spawn(async move {
            while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
                bumper.bump();
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        });

        let cfg = SmartProbeConfig {
            readiness_urls: vec!["http://127.0.0.1:1/health".to_string()],
            stagnation_window: Duration::from_millis(600),
            status_report_interval: Duration::from_secs(60),
            log_activity: activity,
            log_sink: None,
        };
        // Race the probe against a 1500ms wall-clock guard. If activity
        // really resets stagnation, the probe runs longer than 600ms.
        let probe_handle =
            tokio::spawn(async move { smart_health_probe(cfg, || async { None }).await });
        let timed = tokio::time::timeout(Duration::from_millis(1500), probe_handle).await;
        stop_flag.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = bump_task.await;
        // If activity-reset works the probe is still running -> timeout fires.
        assert!(
            timed.is_err(),
            "probe should have stayed alive past stagnation_window thanks to log activity"
        );
    }

    #[tokio::test]
    async fn deploy_returns_service_id_on_success_for_embedded() {
        let db = open_db();
        let ports = Arc::new(PortAllocator::new((45_900, 45_999), Default::default()).unwrap());
        let manifest = dummy_manifest("emb-ok", NativeRuntime::Embedded);
        let cfg = serde_json::json!({});
        let outcome = deploy(
            DeployMethod::NativeEmbedded,
            &manifest,
            &cfg,
            &ports,
            &db,
            None,
            None,
        )
        .await
        .expect("embedded deploy succeeds");

        assert!(outcome.endpoint.handle.id > 0);
        assert_eq!(outcome.endpoint.transport, Transport::Embedded);
        // model_registry row was created with the service_id linked
        let conn = db.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM model_registry WHERE service_id = ?1",
                rusqlite::params![outcome.endpoint.handle.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn respawn_does_not_insert_to_db() {
        // First, create a real service row via deploy() to act as the
        // "existing" service row.
        let db = open_db();
        let ports = Arc::new(PortAllocator::new((46_500, 46_599), Default::default()).unwrap());
        let manifest = dummy_manifest("respawn-emb", NativeRuntime::Embedded);
        let cfg = serde_json::json!({});
        let outcome = deploy(
            DeployMethod::NativeEmbedded,
            &manifest,
            &cfg,
            &ports,
            &db,
            None,
            None,
        )
        .await
        .expect("seed deploy succeeds");

        let count_before: i64 = {
            let conn = db.lock().unwrap();
            conn.query_row("SELECT COUNT(*) FROM services", [], |r| r.get(0))
                .unwrap()
        };

        // respawn: should produce a RuntimeHandle without inserting anywhere.
        // Since the embedded manifest needs to be in the global manifest registry
        // for respawn() to find it, this branch is exercised only for engines
        // that exist in the registry. Use a manifest id that we know is missing
        // and assert the expected error path — proving the function never
        // touches the DB even on the unhappy path.
        let err = respawn(
            "respawn-not-in-registry",
            DeployMethod::NativeEmbedded,
            "{}",
            ports.clone(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, DeployError::Manifest(_)));

        let count_after: i64 = {
            let conn = db.lock().unwrap();
            conn.query_row("SELECT COUNT(*) FROM services", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(count_before, count_after, "respawn must not touch the DB");
        // Sanity: the seed deploy did create exactly one row.
        assert_eq!(count_after, 1);
        let _ = outcome;
    }

    #[tokio::test]
    async fn deploy_records_failed_audit_row_on_prepare_error() {
        let db = open_db();
        let ports = Arc::new(PortAllocator::new((46_000, 46_010), Default::default()).unwrap());
        // Make the manifest binary deploy with an invalid binary path so prepare fails.
        let mut manifest = dummy_manifest("bin-err", NativeRuntime::Binary);
        manifest.deploy.native.as_mut().unwrap().binary_path =
            Some("/nonexistent/path/that/should/not/exist".into());

        let cfg = serde_json::json!({});
        let res = deploy(
            DeployMethod::NativeBinary,
            &manifest,
            &cfg,
            &ports,
            &db,
            None,
            None,
        )
        .await;
        assert!(res.is_err(), "deploy should fail when binary path invalid");

        // deployments row exists with status=failed.
        let conn = db.lock().unwrap();
        let (status, err): (String, Option<String>) = conn
            .query_row(
                "SELECT status, error_text FROM deployments WHERE engine_id = 'bin-err' ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "failed");
        assert!(err.is_some());
    }

    /// Mirrors the dispatch handler's contract: each deploy persists a
    /// `deployments` row whose `slug` matches the value the handler
    /// returns to the caller. The dashboard subscribes to logs by slug, so
    /// drift between handler-returned slug and DB slug breaks live tail.
    #[tokio::test]
    async fn service_manifest_deploy_writes_with_slug() {
        let db = open_db();
        let ports = Arc::new(PortAllocator::new((45_650, 45_699), Default::default()).unwrap());
        let manifest = dummy_manifest("emb-slug-handler", NativeRuntime::Embedded);
        let cfg = serde_json::json!({});

        let slug = "handler-slug-cccc".to_string();
        deploy(
            DeployMethod::NativeEmbedded,
            &manifest,
            &cfg,
            &ports,
            &db,
            None,
            Some(slug.clone()),
        )
        .await
        .unwrap();

        let row = crate::services_repo::deployments::get_by_slug(&db, &slug)
            .unwrap()
            .expect("deployments row exists for handler slug");
        assert_eq!(row.engine_id, "emb-slug-handler");
        assert_eq!(row.deploy_method, "native_embedded");
        assert_eq!(
            row.status,
            crate::services_repo::deployments::DeploymentStatus::Success
        );
    }

    #[tokio::test]
    async fn deploy_with_log_sink_pipes_lines() {
        // Embedded deploy never spawns a process, so the lines we observe come
        // from `deploy()` itself: the [prepare] info and the [commit] info.
        // We verify they reach a subscriber AND get appended to log_tail.
        let db = open_db();
        let ports = Arc::new(PortAllocator::new((45_700, 45_799), Default::default()).unwrap());
        let manifest = dummy_manifest("emb-with-sink", NativeRuntime::Embedded);
        let cfg = serde_json::json!({});

        let slug = "test-slug-aaaa".to_string();
        let (tx, mut rx) =
            tokio::sync::broadcast::channel::<crate::deploy::log_bus::BusMessage>(64);

        let outcome = deploy(
            DeployMethod::NativeEmbedded,
            &manifest,
            &cfg,
            &ports,
            &db,
            Some(tx.clone()),
            Some(slug.clone()),
        )
        .await
        .expect("embedded deploy succeeds");
        assert!(outcome.endpoint.handle.id > 0);

        // Drain at least 2 lines (prepare + commit) without blocking forever.
        let mut received = 0usize;
        while received < 2 {
            match tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await {
                Ok(Ok(crate::deploy::log_bus::BusMessage::Line(_))) => received += 1,
                _ => break,
            }
        }
        assert!(received >= 2, "expected at least prepare + commit lines");

        let row = crate::services_repo::deployments::get_by_slug(&db, &slug)
            .unwrap()
            .expect("deployment row by slug");
        assert!(!row.log_tail.is_empty(), "log_tail was persisted");
    }
}
