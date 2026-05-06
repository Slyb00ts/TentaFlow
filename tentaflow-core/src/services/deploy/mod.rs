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
use crate::services::manifest::{
    ApiKind, BindingTarget, DeployTarget, EngineParameter, ParameterKind, ServiceManifest,
};
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

    /// Emits a phase boundary (e.g. "downloading-vision", "starting",
    /// "health-check"). Frontend uses `phase` to switch the step indicator;
    /// `line` is the human-readable label.
    pub fn phase(&self, phase: &str, line: &str) {
        let _ = deployments_repo::append_log_line(&self.db, &self.slug, line);
        let _ = self.sender.send(BusMessage::Line(LogLine {
            deploy_id: self.slug.clone(),
            kind: "phase".to_string(),
            line: line.to_string(),
            phase: phase.to_string(),
            progress_pct: 0,
            ts_ms: now_ms(),
        }));
    }

    /// Emits a progress update within a phase. `pct` clamped to 0..=100.
    /// Frontend ties this update to the most recent `phase()` call so a
    /// multi-step deploy can drive multiple progress bars.
    pub fn progress(&self, phase: &str, pct: u8, line: &str) {
        let _ = deployments_repo::append_log_line(&self.db, &self.slug, line);
        let _ = self.sender.send(BusMessage::Line(LogLine {
            deploy_id: self.slug.clone(),
            kind: "progress".to_string(),
            line: line.to_string(),
            phase: phase.to_string(),
            progress_pct: pct.min(100) as u32,
            ts_ms: now_ms(),
        }));
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

    // 1b. Reject before we start spawning processes if any of the model
    // names this deploy will register collides with an active alias or a
    // published flow. The catalog id space is shared (D.1); silently
    // letting a deploy overwrite an existing publish would leave clients
    // unable to tell which owner answers a chat request.
    let planned_models = models_from_manifest(manifest, user_config);
    for model in &planned_models {
        if let Err(err) =
            crate::services::catalog::guards::check_service_deploy_collision(db, &model.model_name)
        {
            if let Some(s) = &sink {
                s.info(&format!("[prepare] aborting: {}", err));
            }
            with_tx(db, |tx| {
                deployments_repo::mark_finished(
                    tx,
                    deployment_id,
                    DeploymentStatus::Failed,
                    Some(&err.to_string()),
                )
                .map_err(|e| DeployError::Database(format!("mark_finished: {}", e)))
            })?;
            return Err(DeployError::Manifest(err.to_string()));
        }
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
/// Shutdown wszystkich supervised services przy zamykaniu tentaflow.
/// Iteruje po `services` rzedach w DB ze statusem != stopped i wola `stop()`
/// dla kazdego (docker container stop+rm, native PID terminate). Bez tego
/// vLLM/sglang/llama-cpp subprocessy zostawaly zombie po Ctrl+C, trzymajac
/// VRAM i blokujac port 5000-6000 dla nowych deployow.
pub async fn stop_all_supervised(
    db: &crate::db::DbPool,
    ports: Arc<PortAllocator>,
) -> Vec<(i64, String)> {
    let services = match db.lock() {
        Ok(conn) => crate::services_repo::services::list_supervised(&conn).unwrap_or_default(),
        Err(_) => return vec![],
    };
    let mut errors: Vec<(i64, String)> = Vec::new();
    for svc in services {
        let id = svc.id;
        let engine_id = svc.engine_id.clone();
        if let Err(e) = stop(&svc, ports.clone()).await {
            errors.push((id, format!("{}: {}", engine_id, e)));
        }
    }
    errors
}

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
        // Domyslnie pinned: po Ctrl+C tentaflow stop_all_supervised terminuje
        // procesy (zwalnia VRAM/porty), a przy starcie supervisor.first_tick
        // → auto_start_pinned respawnuje serwis. Bez pin user musialby recznie
        // klikac Start po kazdym restarcie. Odpinanie zostaje pod kontrola
        // usera (przycisk pin w GUI).
        pinned: true,
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

/// Probe outcome. `Ready` is success; `ProcessExited` is deploy-fatal.
#[derive(Debug)]
pub enum SmartProbeOutcome {
    /// HTTP readiness URL responded 2xx.
    Ready,
    /// Process / container died before becoming ready. Carries the OS exit
    /// code if the strategy could fetch one (None when only liveness is
    /// observable, e.g. via `kill(pid, 0)`).
    ProcessExited(Option<i32>),
}

/// Probe configuration. `readiness_urls` are raced; the first 2xx wins.
pub struct SmartProbeConfig {
    pub readiness_urls: Vec<String>,
    /// How often the probe emits a "still starting…" line through
    /// `log_sink` so the dashboard sees progress.
    pub status_report_interval: std::time::Duration,
    pub log_sink: Option<LogSink>,
}

/// Smart liveness+readiness probe with no hard timeout. Loops until one of:
///
/// * a readiness URL answers 2xx → `Ready`;
/// * `is_alive_check` reports the process gone → `ProcessExited`.
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
    let mut last_status_emit = Instant::now();
    let probe_interval = Duration::from_millis(500);

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => {
            // Without an HTTP client we cannot observe readiness; treat as
            // an immediate exit so the caller can roll back cleanly.
            return SmartProbeOutcome::ProcessExited(None);
        }
    };

    loop {
        if let Some(exit) = is_alive_check().await {
            return SmartProbeOutcome::ProcessExited(exit);
        }

        for url in &cfg.readiness_urls {
            if let Ok(resp) = client.get(url).send().await {
                if resp.status().is_success() {
                    return SmartProbeOutcome::Ready;
                }
            }
        }

        if last_status_emit.elapsed() >= cfg.status_report_interval {
            if let Some(sink) = &cfg.log_sink {
                sink.info(&format!(
                    "[health] still starting (alive {}s, waiting for ready)",
                    started.elapsed().as_secs()
                ));
            }
            last_status_emit = Instant::now();
        }

        tokio::time::sleep(probe_interval).await;
    }
}

/// Builds `NewModel` rows from the manifest filtered by user wizard choice.
/// `service_id` is filled by the dispatcher after commit.
///
/// Selection priority:
///   1. `user_config.model_repo` — custom HF repo, single row, no preset.
///   2. `user_config.model_preset_id` — single preset matched by id.
///   3. Recommended preset (or first) — fallback when wizard sent neither.
///   4. Empty Vec — engines without presets at all (e.g. teams-bot).
pub(crate) fn models_from_manifest(
    manifest: &ServiceManifest,
    user_config: &serde_json::Value,
) -> Vec<NewModel> {
    let capabilities = format!("[\"{}\"]", manifest.engine.capability_tag());

    // 1. Custom HF repo from the wizard wins outright.
    if let Some(repo) = user_config
        .get("model_repo")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return vec![NewModel {
            service_id: 0,
            model_name: repo.to_string(),
            display_name: Some(repo.to_string()),
            capabilities,
            context_length: None,
            quantization: None,
            is_default: true,
        }];
    }

    // 2. Explicit preset selection by id.
    if let Some(id) = user_config
        .get("model_preset_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if let Some(p) = manifest.model_presets.iter().find(|m| m.id == id) {
            return vec![NewModel {
                service_id: 0,
                model_name: p.id.clone(),
                display_name: Some(p.display_name.clone()),
                capabilities,
                context_length: None,
                quantization: p.quantization.clone(),
                is_default: true,
            }];
        }
        // Unknown id — fall through to default fallback so the deploy still
        // produces a usable row instead of failing silently.
    }

    // 3. Fallback to recommended (or first) preset.
    if manifest.model_presets.is_empty() {
        return Vec::new();
    }
    let chosen = manifest
        .model_presets
        .iter()
        .find(|p| p.recommended)
        .unwrap_or(&manifest.model_presets[0]);
    vec![NewModel {
        service_id: 0,
        model_name: chosen.id.clone(),
        display_name: Some(chosen.display_name.clone()),
        capabilities,
        context_length: None,
        quantization: chosen.quantization.clone(),
        is_default: true,
    }]
}

/// Resolves the actual model repository identifier (e.g. `Qwen/Qwen3.5-0.8B`)
/// the engine should load. Mirrors `models_from_manifest` selection rules but
/// returns the *repo string* the engine consumes via env (`${MODEL}`):
///   1. `user_config.model_repo` — custom HF repo.
///   2. `user_config.model_preset_id` — preset.repo lookup.
///   3. Recommended preset's repo (or first preset's repo as fallback).
///   4. None — manifest has no presets and wizard sent no repo.
pub(crate) fn resolve_model_repo(
    manifest: &ServiceManifest,
    user_config: &serde_json::Value,
) -> Option<String> {
    if let Some(repo) = user_config
        .get("model_repo")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(repo.to_string());
    }
    if let Some(id) = user_config
        .get("model_preset_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if let Some(p) = manifest.model_presets.iter().find(|m| m.id == id) {
            return Some(p.repo.clone());
        }
    }
    if manifest.model_presets.is_empty() {
        return None;
    }
    let chosen = manifest
        .model_presets
        .iter()
        .find(|p| p.recommended)
        .unwrap_or(&manifest.model_presets[0]);
    Some(chosen.repo.clone())
}

/// Builds the canonical base URL we persist as `services.endpoint_url` for
/// HTTP transports. `BackendClient` (in `services/backend/client.rs`) appends
/// `/chat/completions`, `/embeddings`, `/audio/{transcriptions,speech}` to
/// whatever we hand it — so for OpenAI-compatible engines the base URL must
/// already include the `/v1` prefix or every request lands on a 404. Other
/// API families (Ollama `/api/...`, sherpa native, comfyui) keep the bare
/// `host:port` and rely on `custom_endpoint` overrides downstream.
pub(crate) fn build_endpoint_url(host: &str, port: u16, api: ApiKind) -> String {
    let base = format!("http://{}:{}", host, port);
    match api {
        ApiKind::OpenaiCompatible => format!("{}/v1", base),
        ApiKind::OllamaNative
        | ApiKind::SherpaTts
        | ApiKind::SherpaStt
        | ApiKind::Comfyui
        | ApiKind::Custom => base,
    }
}

#[cfg(test)]
mod build_endpoint_url_tests {
    use super::*;

    #[test]
    fn openai_compatible_appends_v1() {
        assert_eq!(
            build_endpoint_url("127.0.0.1", 5001, ApiKind::OpenaiCompatible),
            "http://127.0.0.1:5001/v1"
        );
    }

    #[test]
    fn ollama_keeps_bare_base() {
        assert_eq!(
            build_endpoint_url("127.0.0.1", 11434, ApiKind::OllamaNative),
            "http://127.0.0.1:11434"
        );
    }

    #[test]
    fn sherpa_keeps_bare_base() {
        assert_eq!(
            build_endpoint_url("127.0.0.1", 5002, ApiKind::SherpaTts),
            "http://127.0.0.1:5002"
        );
        assert_eq!(
            build_endpoint_url("127.0.0.1", 5003, ApiKind::SherpaStt),
            "http://127.0.0.1:5003"
        );
    }
}

#[cfg(test)]
mod apply_parameters_deploy_tests {
    use super::*;
    use crate::services::manifest::{
        BindingTarget, Category, DeploySection, DockerDeploy, DockerTransport, Engine,
        EngineParameter, NumRange, ParameterBinding, ParameterKind, TargetOs,
    };
    use serde_json::json;

    fn make_engine(id: &str) -> Engine {
        Engine {
            id: id.into(),
            category: Category::Llm,
            name: id.into(),
            description_pl: String::new(),
            description_en: String::new(),
            homepage: String::new(),
            license: String::new(),
            icon: None,
            resource_kind: None,
            requires_model: Some(true),
            gpu_supported: None,
            default_port: 8000,
            api: ApiKind::OpenaiCompatible,
            version: "0.1.0".into(),
            service_surfaces: None,
            input_modalities: None,
            output_modalities: None,
        }
    }

    fn docker_deploy() -> DeploySection {
        DeploySection {
            docker: Some(DockerDeploy {
                context_path: Some("docker/test".into()),
                compose_path: None,
                platforms: vec![TargetOs::Linux],
                download_image: None,
                download_size_mb: None,
                transport: Some(DockerTransport::SidecarQuic),
            }),
            native: None,
            external: None,
        }
    }

    fn manifest_with_params(parameters: Vec<EngineParameter>) -> ServiceManifest {
        ServiceManifest {
            engine: make_engine("test"),
            deploy: docker_deploy(),
            model_presets: vec![],
            parameters,
            docker_source_hash: String::new(),
            native_source_hash: String::new(),
        }
    }

    fn float_param(key: &str, env: &str, default: f64) -> EngineParameter {
        EngineParameter {
            key: key.into(),
            label_pl: key.into(),
            label_en: key.into(),
            kind: ParameterKind::Float,
            range: Some(NumRange {
                min: 0.1,
                max: 0.95,
                step: Some(0.05),
            }),
            options: None,
            default: json!(default),
            bindings: vec![ParameterBinding {
                when: DeployTarget::Docker,
                target: BindingTarget::Env { name: env.into() },
            }],
        }
    }

    #[test]
    fn empty_parameters_returns_empty_application() {
        let m = manifest_with_params(vec![]);
        let (app, req) =
            apply_parameters_deploy(&m, &json!({}), DeployTarget::Docker).unwrap();
        assert!(app.env.is_empty());
        assert!(req.ollama_options.is_empty());
    }

    #[test]
    fn user_value_overrides_default() {
        let m = manifest_with_params(vec![float_param(
            "gpu_memory_utilization",
            "GPU_MEMORY_UTILIZATION",
            0.9,
        )]);
        let user_config = json!({ "parameters": { "gpu_memory_utilization": 0.6 } });
        let (app, _) =
            apply_parameters_deploy(&m, &user_config, DeployTarget::Docker).unwrap();
        assert_eq!(app.env.get("GPU_MEMORY_UTILIZATION").unwrap(), "0.6");
    }

    #[test]
    fn missing_user_value_uses_default() {
        let m = manifest_with_params(vec![float_param(
            "gpu_memory_utilization",
            "GPU_MEMORY_UTILIZATION",
            0.9,
        )]);
        let (app, _) =
            apply_parameters_deploy(&m, &json!({}), DeployTarget::Docker).unwrap();
        assert_eq!(app.env.get("GPU_MEMORY_UTILIZATION").unwrap(), "0.9");
    }

    #[test]
    fn out_of_range_returns_error() {
        let m = manifest_with_params(vec![float_param(
            "gpu_memory_utilization",
            "GPU_MEMORY_UTILIZATION",
            0.9,
        )]);
        let user_config = json!({ "parameters": { "gpu_memory_utilization": 2.0 } });
        let err =
            apply_parameters_deploy(&m, &user_config, DeployTarget::Docker).unwrap_err();
        assert!(matches!(err, ParameterError::OutOfRange { .. }));
    }

    #[test]
    fn type_mismatch_returns_error() {
        let m = manifest_with_params(vec![float_param(
            "gpu_memory_utilization",
            "GPU_MEMORY_UTILIZATION",
            0.9,
        )]);
        let user_config = json!({ "parameters": { "gpu_memory_utilization": "not a float" } });
        let err =
            apply_parameters_deploy(&m, &user_config, DeployTarget::Docker).unwrap_err();
        assert!(matches!(err, ParameterError::TypeMismatch { .. }));
    }

    #[test]
    fn binding_for_other_target_is_skipped() {
        let m = manifest_with_params(vec![float_param(
            "gpu_memory_utilization",
            "GPU_MEMORY_UTILIZATION",
            0.9,
        )]);
        // Manifest ma binding when="docker", pytamy o native_embedded.
        // Backend rozsadnie nic nie zwraca dla tej deploy method.
        let (app, _) =
            apply_parameters_deploy(&m, &json!({}), DeployTarget::NativeEmbedded).unwrap();
        assert!(app.env.is_empty());
    }

    #[test]
    fn dual_binding_dispatches_per_target() {
        let p = EngineParameter {
            key: "ctx_size".into(),
            label_pl: "ctx".into(),
            label_en: "ctx".into(),
            kind: ParameterKind::Int,
            range: Some(NumRange {
                min: 512.0,
                max: 131072.0,
                step: Some(512.0),
            }),
            options: None,
            default: json!(8192),
            bindings: vec![
                ParameterBinding {
                    when: DeployTarget::NativeEmbedded,
                    target: BindingTarget::LlamacppField {
                        field: "ctx_size".into(),
                    },
                },
                ParameterBinding {
                    when: DeployTarget::Docker,
                    target: BindingTarget::Env {
                        name: "CTX_SIZE".into(),
                    },
                },
            ],
        };
        let m = manifest_with_params(vec![p]);
        let user_config = json!({ "parameters": { "ctx_size": 32768 } });

        let (app_docker, _) =
            apply_parameters_deploy(&m, &user_config, DeployTarget::Docker).unwrap();
        assert_eq!(app_docker.env.get("CTX_SIZE").unwrap(), "32768");
        assert!(app_docker.llamacpp.is_empty());

        let (app_emb, _) =
            apply_parameters_deploy(&m, &user_config, DeployTarget::NativeEmbedded).unwrap();
        assert!(app_emb.env.is_empty());
        assert_eq!(app_emb.llamacpp.get("ctx_size").unwrap(), &json!(32768));
    }

    #[test]
    fn whisper_field_with_request_override_populates_both_maps() {
        let p = EngineParameter {
            key: "beam_size".into(),
            label_pl: "beam".into(),
            label_en: "beam".into(),
            kind: ParameterKind::Int,
            range: Some(NumRange {
                min: 1.0,
                max: 16.0,
                step: None,
            }),
            options: None,
            default: json!(5),
            bindings: vec![ParameterBinding {
                when: DeployTarget::NativeEmbedded,
                target: BindingTarget::WhisperField {
                    field: "default_beam_size".into(),
                    request_override: true,
                },
            }],
        };
        let mut m = manifest_with_params(vec![p]);
        // Manifest musi mieć [deploy.native] z runtime=embedded zeby
        // walidacja w build.rs przeszla, ale ten test nie odpala
        // walidacji — deploy section w manifestcie tylko dispatch, my
        // pytamy o NativeEmbedded.
        m.deploy.native = Some(crate::services::manifest::NativeDeploy {
            runtime: crate::services::manifest::NativeRuntime::Embedded,
            platforms: vec![TargetOs::Linux],
            feature_flag: Some("inference-whisper".into()),
            binary_path: None,
            bundle_path: None,
        });
        m.deploy.docker = None;

        let user_config = json!({ "parameters": { "beam_size": 8 } });
        let (app, req) =
            apply_parameters_deploy(&m, &user_config, DeployTarget::NativeEmbedded).unwrap();
        assert_eq!(app.whisper.get("default_beam_size").unwrap(), &json!(8));
        assert_eq!(req.whisper_overridable.get("default_beam_size").unwrap(), &json!(8));
    }

    #[test]
    fn ollama_options_goes_to_request_time() {
        let p = EngineParameter {
            key: "context_size".into(),
            label_pl: "ctx".into(),
            label_en: "ctx".into(),
            kind: ParameterKind::Int,
            range: Some(NumRange {
                min: 512.0,
                max: 131072.0,
                step: None,
            }),
            options: None,
            default: json!(8192),
            bindings: vec![ParameterBinding {
                when: DeployTarget::External,
                target: BindingTarget::OllamaOptions {
                    key: "num_ctx".into(),
                },
            }],
        };
        let mut m = manifest_with_params(vec![p]);
        m.deploy.docker = None;
        m.deploy.external = Some(crate::services::manifest::ExternalDeploy {
            platforms: vec![TargetOs::Linux],
            detection_binary: "ollama".into(),
            detection_endpoint: "http://localhost:11434".into(),
            detection_health_path: "/api/tags".into(),
        });

        let user_config = json!({ "parameters": { "context_size": 16384 } });
        let (app, req) =
            apply_parameters_deploy(&m, &user_config, DeployTarget::External).unwrap();
        assert!(app.env.is_empty());
        assert_eq!(req.ollama_options.get("num_ctx").unwrap(), &json!(16384));
    }
}

/// Merguje `user_config` z typed `request_time_parameters` i serializuje
/// do JSON do zapisu w `services.config_json`. Snapshot builder czyta to
/// pole obratem i propaguje do `BackendClient` przez `LiveHandlesCache`.
/// Bez tego wywoływania typed overrides z `apply_parameters_deploy`
/// nigdy nie docierałyby do request body.
pub fn merge_config_json(
    user_config: &serde_json::Value,
    request_time: &RequestTimeParameters,
) -> Result<String, serde_json::Error> {
    let mut value = user_config.clone();
    if !value.is_object() {
        value = serde_json::Value::Object(serde_json::Map::new());
    }
    let to_value_map = |m: &HashMap<String, serde_json::Value>| -> serde_json::Map<String, serde_json::Value> {
        m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    };
    let rtp = serde_json::json!({
        "ollama_options": to_value_map(&request_time.ollama_options),
        "python_request": to_value_map(&request_time.python_request),
        "whisper_overridable": to_value_map(&request_time.whisper_overridable),
        "mlx_overridable": to_value_map(&request_time.mlx_overridable),
    });
    if let Some(obj) = value.as_object_mut() {
        obj.insert("request_time_parameters".into(), rtp);
    }
    serde_json::to_string(&value)
}

/// Reads `(free_mib, total_mib)` for cuda:0 via `nvidia-smi`. Returns `None`
/// when the binary is missing or fails (e.g. AMD-only / Apple host). vLLM
/// default targets device 0 unless `CUDA_VISIBLE_DEVICES` reorders things,
/// so we report the first row.
pub(crate) fn query_cuda0_vram_mib() -> Option<(u64, u64)> {
    let output = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=memory.free,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first = stdout.lines().next()?;
    let parts: Vec<&str> = first.split(',').map(str::trim).collect();
    if parts.len() < 2 {
        return None;
    }
    let free = parts[0].parse().ok()?;
    let total = parts[1].parse().ok()?;
    Some((free, total))
}

/// Computes a safe `--gpu-memory-utilization` ratio so the resulting allocation
/// fits in currently free VRAM with a headroom buffer. vLLM checks
/// `total_mib * ratio <= free_mib` at startup and crashes otherwise. We aim at
/// `min(0.92, 0.94 * free/total)` — leaves ~6% headroom for fragmentation,
/// torch allocator slack, kernel JIT scratch. Returns `None` when nvidia-smi
/// is unavailable (caller should keep the manifest default).
pub(crate) fn auto_gpu_memory_utilization() -> Option<f64> {
    let (free_mib, total_mib) = query_cuda0_vram_mib()?;
    if total_mib == 0 {
        return None;
    }
    let free_ratio = free_mib as f64 / total_mib as f64;
    let ratio = (0.94 * free_ratio).min(0.92);
    if ratio < 0.10 {
        return Some(ratio);
    }
    let rounded = (ratio * 100.0).floor() / 100.0;
    Some(rounded)
}


/// Wynik aplikacji typed schemy parametrów dla konkretnego deployu.
/// **Deploy-time** wartości — konsumowane raz przy spawnie procesu albo
/// load modelu. Per-binding-type rozsiane do osobnych map zeby caller mial
/// to zone (env idzie do procesu/dockera, llamacpp/whisper/mlx do loadera).
#[derive(Debug, Default, Clone)]
pub struct ParameterApplication {
    /// Env vars dla python-bundle/docker/binary engines.
    pub env: HashMap<String, String>,
    /// Pola `LlamaCppDeployParams` dla embedded llama-cpp.
    pub llamacpp: HashMap<String, serde_json::Value>,
    /// Pola `WhisperDeployParams` dla embedded whisper / mlx-whisper.
    pub whisper: HashMap<String, serde_json::Value>,
    /// Pola `MlxDeployParams` dla embedded mlx LLM.
    pub mlx: HashMap<String, serde_json::Value>,
}

/// Wynik aplikacji typed schemy dla **request-time** wartosci.
/// Persystowane w `services.config_json` jako typed JSON; przy kazdym
/// requestcie do silnika materializowane (Ollama options w POST body,
/// extra fields w multipart `data`, deploy defaults dla MLX/Whisper z
/// per-request override).
#[derive(Debug, Default, Clone)]
pub struct RequestTimeParameters {
    /// Klucz=wartosc dla Ollama API `options` mapy.
    pub ollama_options: HashMap<String, serde_json::Value>,
    /// Pola POST body do generic Python wrappera (qwen-asr, kyutai-tts,
    /// xtts, voxcpm, chatterbox).
    pub python_request: HashMap<String, serde_json::Value>,
    /// Whisper deploy defaults z `request_override = true` — backend
    /// przy `transcribe()` uzywa jako baseline; klient API moze nadpisac.
    pub whisper_overridable: HashMap<String, serde_json::Value>,
    /// MLX deploy defaults z `request_override = true` — analogicznie.
    pub mlx_overridable: HashMap<String, serde_json::Value>,
}

/// Bledy walidacji parametrow na ktore deploy powinien upasc zanim
/// alokuje zasoby (port, container, venv).
#[derive(Debug, thiserror::Error)]
pub enum ParameterError {
    #[error("parameter '{key}' not in manifest schema")]
    UnknownKey { key: String },
    #[error("parameter '{key}' value type {actual} does not match kind {expected:?}")]
    TypeMismatch {
        key: String,
        expected: ParameterKind,
        actual: &'static str,
    },
    #[error("parameter '{key}' value {value} out of range [{min}, {max}]")]
    OutOfRange {
        key: String,
        value: f64,
        min: f64,
        max: f64,
    },
    #[error("parameter '{key}' value '{value}' not in options {options:?}")]
    NotInOptions {
        key: String,
        value: String,
        options: Vec<String>,
    },
    #[error("parameter '{key}' has no binding for deploy target {target:?}")]
    NoBindingForTarget {
        key: String,
        target: DeployTarget,
    },
}

/// Aplikuje typed schemę parametrów z manifestu do `user_config.parameters`
/// mapy, produkując osobno deploy-time bindings (`ParameterApplication`)
/// i request-time bindings (`RequestTimeParameters`).
///
/// Algorytm per parametr w manifeście:
///   1. Czytaj wartość z `user_config.parameters[p.key]` lub `p.default`.
///   2. Waliduj zgodność z `kind`, `range`, `options`. Niezgodność → error.
///   3. Z `p.bindings[]` wybierz ten z `when == deploy_target`. Brak → skip.
///   4. Dispatch po `binding.target`:
///      - `Env` → `app.env`
///      - `LlamacppField` → `app.llamacpp`
///      - `WhisperField` → `app.whisper` (+ `req.whisper_overridable` gdy
///        `request_override = true`)
///      - `MlxField` → `app.mlx` (+ `req.mlx_overridable` gdy
///        `request_override = true`)
///      - `OllamaOptions` → `req.ollama_options`
///      - `PythonRequestBody` → `req.python_request`
///
/// Wizard wysyła `parameters: { key: value, ... }` jako mapę top-level.
/// Klucze nieznane manifestowi są ignorowane (nie błąd — schema mogła się
/// zmienić, redeploy starym configiem nie powinien failować).
pub fn apply_parameters_deploy(
    manifest: &ServiceManifest,
    user_config: &serde_json::Value,
    deploy_target: DeployTarget,
) -> Result<(ParameterApplication, RequestTimeParameters), ParameterError> {
    let mut app = ParameterApplication::default();
    let mut req = RequestTimeParameters::default();

    let user_params = user_config
        .get("parameters")
        .and_then(|v| v.as_object());

    for p in &manifest.parameters {
        let value = user_params
            .and_then(|m| m.get(&p.key))
            .cloned()
            .unwrap_or_else(|| p.default.clone());

        validate_parameter_value(p, &value)?;

        let Some(binding) = p.bindings.iter().find(|b| b.when == deploy_target) else {
            continue;
        };

        match &binding.target {
            BindingTarget::Env { name } => {
                let s = json_to_env_string(&value);
                app.env.insert(name.clone(), s);
            }
            BindingTarget::LlamacppField { field } => {
                app.llamacpp.insert(field.clone(), value);
            }
            BindingTarget::WhisperField {
                field,
                request_override,
            } => {
                app.whisper.insert(field.clone(), value.clone());
                if *request_override {
                    req.whisper_overridable.insert(field.clone(), value);
                }
            }
            BindingTarget::MlxField {
                field,
                request_override,
            } => {
                app.mlx.insert(field.clone(), value.clone());
                if *request_override {
                    req.mlx_overridable.insert(field.clone(), value);
                }
            }
            BindingTarget::OllamaOptions { key } => {
                req.ollama_options.insert(key.clone(), value);
            }
            BindingTarget::PythonRequestBody { field } => {
                req.python_request.insert(field.clone(), value);
            }
        }
    }

    Ok((app, req))
}

fn validate_parameter_value(
    p: &EngineParameter,
    value: &serde_json::Value,
) -> Result<(), ParameterError> {
    let actual = match value {
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(n) if n.is_f64() => "float",
        serde_json::Value::Number(_) => "int",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Null => "null",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    };

    let kind_ok = match p.kind {
        ParameterKind::Float => value.is_f64() || value.is_i64(),
        ParameterKind::Int => value.is_i64() || value.is_u64(),
        ParameterKind::Bool => value.is_boolean(),
        ParameterKind::Enum => value.is_string(),
        ParameterKind::String => value.is_string(),
    };
    if !kind_ok {
        return Err(ParameterError::TypeMismatch {
            key: p.key.clone(),
            expected: p.kind,
            actual,
        });
    }

    if let Some(range) = p.range {
        let v = value
            .as_f64()
            .or_else(|| value.as_i64().map(|i| i as f64))
            .or_else(|| value.as_u64().map(|u| u as f64));
        if let Some(num) = v {
            if num < range.min || num > range.max {
                return Err(ParameterError::OutOfRange {
                    key: p.key.clone(),
                    value: num,
                    min: range.min,
                    max: range.max,
                });
            }
        }
    }

    if let (ParameterKind::Enum, Some(opts)) = (p.kind, p.options.as_ref()) {
        let s = value.as_str().unwrap_or_default();
        if !opts.iter().any(|o| o == s) {
            return Err(ParameterError::NotInOptions {
                key: p.key.clone(),
                value: s.to_string(),
                options: opts.clone(),
            });
        }
    }

    Ok(())
}

fn json_to_env_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => v.to_string(),
    }
}

/// Reads the optional `transport_explicit` hint from user_config. Used by
/// docker strategy as a Phase 6 preview (bypass sidecar for `direct_http`).
#[cfg_attr(not(feature = "docker"), allow(dead_code))]
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
                service_surfaces: None,
                input_modalities: None,
                output_modalities: None,
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
                service_surfaces: None,
                input_modalities: None,
                output_modalities: None,
            }],
            parameters: vec![],
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
            status_report_interval: Duration::from_secs(60),
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
            status_report_interval: Duration::from_secs(60),
            log_sink: None,
        };
        let outcome = smart_health_probe(cfg, || async { Some(Some(137)) }).await;
        match outcome {
            SmartProbeOutcome::ProcessExited(Some(137)) => {}
            other => panic!("expected ProcessExited(137), got {:?}", other),
        }
    }

    /// Catalog id space is shared across services / flows / aliases. A
    /// deploy whose model name collides with an active alias must abort
    /// before the strategy spawns anything — pre-fix the guard was only
    /// callable from tests, so the deploy would have succeeded and the
    /// catalog would publish two owners for the same id.
    #[tokio::test]
    async fn deploy_aborts_when_model_name_collides_with_alias() {
        let db = open_db();
        // Plant a colliding alias before the deploy. The dummy manifest's
        // preset id is "preset-a", so that becomes the planned model name.
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO model_aliases (alias, target_model, is_active) \
                 VALUES (?1, ?2, 1)",
                rusqlite::params!["preset-a", "some-target"],
            )
            .unwrap();
        }

        let ports = Arc::new(PortAllocator::new((46_700, 46_799), Default::default()).unwrap());
        let manifest = dummy_manifest("emb-collide", NativeRuntime::Embedded);
        let cfg = serde_json::json!({});
        let result = deploy(
            DeployMethod::NativeEmbedded,
            &manifest,
            &cfg,
            &ports,
            &db,
            None,
            None,
        )
        .await;

        match result {
            Err(DeployError::Manifest(msg)) => {
                assert!(
                    msg.contains("preset-a") && msg.contains("alias"),
                    "guard error should mention the colliding name and 'alias': {msg}"
                );
            }
            other => panic!("expected DeployError::Manifest, got {:?}", other),
        }

        // Audit row was created and marked failed — paper trail must
        // exist even when the deploy is rejected pre-strategy.
        let conn = db.lock().unwrap();
        let (status, error_text): (String, Option<String>) = conn
            .query_row(
                "SELECT status, error_text FROM deployments WHERE engine_id = 'emb-collide'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "failed");
        assert!(error_text.unwrap().contains("preset-a"));
    }

    #[tokio::test]
    async fn deploy_returns_service_id_on_success_for_embedded() {
        let db = open_db();
        let ports = Arc::new(PortAllocator::new((45_900, 45_999), Default::default()).unwrap());
        // engine.id "llama-cpp" maps to a local inference backend; other ids
        // (e.g. "emb-ok") are rejected by prepare_embedded_llm — see
        // embedded.rs:145.
        let manifest = dummy_manifest("llama-cpp", NativeRuntime::Embedded);
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
        let manifest = dummy_manifest("llama-cpp", NativeRuntime::Embedded);
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
        let manifest = dummy_manifest("llama-cpp", NativeRuntime::Embedded);
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
        assert_eq!(row.engine_id, "llama-cpp");
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
        let manifest = dummy_manifest("llama-cpp", NativeRuntime::Embedded);
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
