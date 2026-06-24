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
use crate::services_repo::services::{
    self as services_repo, DeployMethod, NewService, ServiceStatus,
};

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
    pub service_id: Option<i64>,
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
        self.persist_progress(phase, 0, Some(line));
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
        self.persist_progress(phase, pct.min(100) as u32, Some(line));
        let _ = self.sender.send(BusMessage::Line(LogLine {
            deploy_id: self.slug.clone(),
            kind: "progress".to_string(),
            line: line.to_string(),
            phase: phase.to_string(),
            progress_pct: pct.min(100) as u32,
            ts_ms: now_ms(),
        }));
    }

    fn persist_progress(&self, phase: &str, pct: u32, message: Option<&str>) {
        if let Ok(conn) = self.db.write() {
            let _ = deployments_repo::set_progress(
                &conn,
                &self.slug,
                DeploymentStatus::Deploying,
                phase,
                pct,
            );
            if let Some(service_id) = self.service_id {
                let _ = services_repo::update_deploy_progress(&conn, service_id, pct, message);
            }
        }
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

#[derive(Debug, Clone)]
pub struct DeployJob {
    pub deploy_id: String,
    pub deployment_id: i64,
    pub service_id: i64,
    /// In-place redeploy istniejącego serwisu (`create_redeploy_job`) vs świeży
    /// deploy placeholdera (`create_deploy_job`). Ścieżka błędu `deploy()` musi
    /// rozróżnić oba: dla redeployu stary runtime został już UBITY przed startem
    /// workera, więc po nieudanym `deploy()` pola runtime'u (pid/port/endpoint)
    /// na wierszu są STALE i muszą zostać wyzerowane (`mark_failed_clear_runtime`),
    /// inaczej resolver routowałby ruch do martwego endpointu. Dla świeżego
    /// deployu placeholder nigdy nie miał żywego runtime'u → generyczny
    /// `mark_deploy_failed` (zachowanie bez zmian).
    pub is_redeploy: bool,
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
    fn commit(
        &self,
        tx: &Transaction<'_>,
        service_id: i64,
        prepared: &PreparedDeploy,
    ) -> DeployResult<()>;
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
/// Klucz w `user_config` przez ktory wizard/frontend moze (omylkowo) przeslac
/// surowy token HF. Sekret rozwiazujemy lokalnie z secure setting i wstrzykujemy
/// tylko do ENV procesu silnika — w `config_json` (services + deployments) nie
/// moze sie nigdy znalezc, bo te wiersze ida do bazy plaintextem i replikuja sie
/// przez sync. Stad usuwamy ten klucz z configu przy KAZDEJ serializacji.
const HF_TOKEN_CONFIG_KEY: &str = "hf_token";

/// Zwraca kopie `user_config` bez klucza `hf_token`. Uzywane wszedzie tam gdzie
/// config trafia do `config_json` (placeholder service + deployments row + commit
/// kazdej strategii), zeby sekret z secure setting albo z payloadu frontendu nie
/// wyciekl do bazy/sync. Token do silnika idzie osobnym kanalem (ENV).
pub(crate) fn strip_hf_token(user_config: &serde_json::Value) -> serde_json::Value {
    let mut sanitized = user_config.clone();
    if let Some(map) = sanitized.as_object_mut() {
        map.remove(HF_TOKEN_CONFIG_KEY);
    }
    sanitized
}

/// Config key holding a cloud external provider's API key (OpenAI, Anthropic, …).
pub const API_KEY_CONFIG_KEY: &str = "api_key";

/// Returns a copy of `user_config` with a plaintext `api_key` encrypted in place
/// (`enc:…`). Unlike `hf_token` (which is stripped and resolved per-node from a
/// secure setting), an external provider's key has no global setting to fall
/// back on — it must be persisted with the service. We keep it encrypted at rest
/// so it never lands in `config_json` (services + deployments rows, sync) in the
/// clear. No-op when the key is absent, blank, or already encrypted. Called at
/// the deploy ingestion boundary so the owning node encrypts with ITS cipher.
pub fn encrypt_api_key_in_config(
    user_config: &serde_json::Value,
    settings_cipher: &crate::crypto::SettingsCipher,
) -> serde_json::Value {
    let mut out = user_config.clone();
    if let Some(map) = out.as_object_mut() {
        if let Some(serde_json::Value::String(key)) = map.get(API_KEY_CONFIG_KEY) {
            let trimmed = key.trim();
            if !trimmed.is_empty() && !trimmed.starts_with("enc:") {
                if let Ok(encrypted) = settings_cipher.encrypt(trimmed) {
                    map.insert(
                        API_KEY_CONFIG_KEY.to_string(),
                        serde_json::Value::String(encrypted),
                    );
                }
            }
        }
    }
    out
}

pub fn create_deploy_job(
    method: DeployMethod,
    manifest: &ServiceManifest,
    user_config: &serde_json::Value,
    db: &DbPool,
    local_node_id: &str,
    user_id: Option<&str>,
    existing_slug: Option<String>,
) -> DeployResult<DeployJob> {
    let slug = existing_slug.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    // Sekret nigdy do config_json — strip przed serializacja placeholdera i
    // deployments row.
    let sanitized_config = strip_hf_token(user_config);
    let config_json = serde_json::to_string(&sanitized_config)
        .map_err(|e| DeployError::Other(format!("serialize config: {}", e)))?;
    let placeholder = build_placeholder_service(method, manifest, &config_json, &slug);
    let (service_id, deployment_id) = with_tx(db, |tx| {
        let sid = services_repo::insert_in_tx(tx, &placeholder)?;
        let did = deployments_repo::create_with_slug(
            tx,
            &manifest.engine.id,
            method.as_db_tag(),
            &slug,
            local_node_id,
            sid,
            &config_json,
        )?;
        if let Some(uid) = user_id {
            tx.execute(
                "UPDATE deployments SET user_id = ?2 WHERE id = ?1",
                rusqlite::params![did, uid],
            )
            .map_err(DeployError::from)?;
        }
        Ok((sid, did))
    })?;
    Ok(DeployJob {
        deploy_id: slug,
        deployment_id,
        service_id,
        is_redeploy: false,
    })
}

/// In-place redeploy: tworzy nowy slug deployu wskazujący na ISTNIEJĄCY wiersz
/// serwisu (`existing_service_id`) zamiast insertować duplikat. Wiersz services
/// jest aktualizowany w miejscu (`begin_redeploy_in_tx`): nowy `active_deploy_id`,
/// nadpisany `config_json`, status zostaje `deploying`. Reszta potoku (`deploy`
/// → `commit` → `finish_deploy_in_tx`) trafia w TEN sam `service_id`, więc po
/// sukcesie wiersz przechodzi w `running`, a po błędzie zostaje `failed` —
/// serwis NIGDY nie znika (odwrotnie niż delete+create_deploy_job).
pub fn create_redeploy_job(
    method: DeployMethod,
    manifest: &ServiceManifest,
    user_config: &serde_json::Value,
    db: &DbPool,
    local_node_id: &str,
    user_id: Option<&str>,
    existing_service_id: i64,
) -> DeployResult<DeployJob> {
    let slug = uuid::Uuid::new_v4().to_string();
    // Sekret nigdy do config_json — strip przed serializacją do services +
    // deployments (token HF leci dalej tylko jako ENV w `deploy()`).
    let sanitized_config = strip_hf_token(user_config);
    let config_json = serde_json::to_string(&sanitized_config)
        .map_err(|e| DeployError::Other(format!("serialize config: {}", e)))?;
    let deployment_id = with_tx(db, |tx| {
        services_repo::begin_redeploy_in_tx(tx, existing_service_id, &slug, &config_json)
            .map_err(|e| DeployError::Database(e.to_string()))?;
        let did = deployments_repo::create_with_slug(
            tx,
            &manifest.engine.id,
            method.as_db_tag(),
            &slug,
            local_node_id,
            existing_service_id,
            &config_json,
        )?;
        if let Some(uid) = user_id {
            tx.execute(
                "UPDATE deployments SET user_id = ?2 WHERE id = ?1",
                rusqlite::params![did, uid],
            )
            .map_err(DeployError::from)?;
        }
        Ok(did)
    })?;
    Ok(DeployJob {
        deploy_id: slug,
        deployment_id,
        service_id: existing_service_id,
        is_redeploy: true,
    })
}

pub async fn deploy(
    job: DeployJob,
    method: DeployMethod,
    manifest: &ServiceManifest,
    user_config: &serde_json::Value,
    ports: &Arc<PortAllocator>,
    db: &DbPool,
    settings_cipher: &crate::crypto::SettingsCipher,
    log_sink: Option<broadcast::Sender<BusMessage>>,
) -> DeployResult<DeployOutcome> {
    let slug = job.deploy_id.clone();

    // Punkt wspolny dla deployu lokalnego (dispatch handler) i zdalnego
    // (handle_service_deploy_remote). Token HF rozwiazujemy TU, z secure setting
    // TEGO noda — nigdy nie jest forwardowany przez mesh, wiec odbiorca uzywa
    // wlasnego. Wartosci nie logujemy. Jednoczesnie usuwamy `hf_token` z configu
    // przekazywanego strategiom, bo ich `prepare/commit` serializuje go do
    // config_json (services + deployments). Sekret leci dalej tylko jako ENV.
    let hf_token =
        crate::db::repository::get_setting_secure(db, HF_TOKEN_CONFIG_KEY, settings_cipher)
            .ok()
            .flatten()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
    let user_config = &strip_hf_token(user_config);
    let sink = log_sink.map(|sender| LogSink {
        slug: slug.clone(),
        sender,
        db: db.clone(),
        service_id: Some(job.service_id),
    });

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
                    job.deployment_id,
                    DeploymentStatus::Failed,
                    Some(&err.to_string()),
                )
                .map_err(|e| DeployError::Database(format!("mark_finished: {}", e)))
            })?;
            mark_worker_deploy_failed(db, &job, &slug, &err.to_string());
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
            hf_token.clone(),
            sink.clone(),
        )),
        DeployMethod::NativePythonBundle => Box::new(python_bundle::PythonBundleDeploy::new(
            manifest.clone(),
            user_config.clone(),
            ports.clone(),
            hf_token.clone(),
            sink.clone(),
        )),
        DeployMethod::Docker => Box::new(docker::DockerDeploy::new(
            manifest.clone(),
            user_config.clone(),
            ports.clone(),
            hf_token.clone(),
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
                job.deployment_id,
                DeploymentStatus::Failed,
                Some(&e.to_string()),
            );
            mark_worker_deploy_failed(db, &job, &slug, &e.to_string());
            return Err(e);
        }
    };

    if let Some(s) = &sink {
        s.info("[commit] writing services + model_registry");
    }

    // 4. COMMIT — single transaction over services + model_registry +
    //    deployments.finish. Any failure triggers rollback of side effects.
    let commit_result: DeployResult<()> = with_tx(db, |tx| {
        strategy.commit(tx, job.service_id, &prepared)?;
        for m in &prepared.models {
            let mut model = m.clone();
            model.service_id = job.service_id;
            models_repo::insert_in_tx(tx, &model)?;
        }
        deployments_repo::mark_finished(tx, job.deployment_id, DeploymentStatus::Success, None)?;
        Ok(())
    });

    match commit_result {
        Ok(()) => {}
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
            mark_finished(
                db,
                job.deployment_id,
                DeploymentStatus::Failed,
                Some(&rb_msg),
            );
            mark_worker_deploy_failed(db, &job, &slug, &rb_msg);
            return Err(commit_err);
        }
    };

    // 6. Build the outcome endpoint for callers.
    let endpoint = ServiceEndpoint {
        handle: crate::services::lifecycle::ServiceHandle {
            id: job.service_id,
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
        deployment_id: job.deployment_id,
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
    db: &DbPool,
    settings_cipher: &crate::crypto::SettingsCipher,
    preserved_port: Option<u16>,
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

    // Pre-kill: jeśli na preserved_port siedzi nasz wlasny stary proces
    // (zombie po crash, OR run_loop respawnuje serwis ktory wciaz dziala
    // bo health probe zwrocil Failed na chwile), strategy.prepare()
    // probowalby zabindowac port i dostal "port zajety". Probujemy
    // znalezc PID slychający na tym porcie i go zabic. Bez tego
    // respawn pinned services walil sie nieskonczenie.
    if let Some(port) = preserved_port {
        kill_listener_on_port(port).await;
    }

    let parsed: serde_json::Value = if config_json.is_empty() {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        serde_json::from_str(config_json)
            .map_err(|e| DeployError::Other(format!("respawn: parse config_json: {}", e)))?
    };
    // Defensywnie: stare wiersze sprzed fixu moga miec `hf_token` w config_json.
    // Strip i tak, zeby respawn nie reintrodukowal sekretu do strategii/configu.
    let user_config = strip_hf_token(&parsed);
    // Respawn gated-repo (vLLM/Bielik) tez musi miec HF_TOKEN — config_json juz
    // go nie niesie, wiec rozwiazujemy z secure setting TEGO noda.
    let hf_token =
        crate::db::repository::get_setting_secure(db, HF_TOKEN_CONFIG_KEY, settings_cipher)
            .ok()
            .flatten()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

    let mut strategy: Box<dyn DeployStrategy> = match deploy_method {
        DeployMethod::NativeEmbedded => {
            Box::new(embedded::EmbeddedDeploy::new(manifest, user_config, None))
        }
        DeployMethod::NativeBinary => Box::new(binary::BinaryDeploy::new_with_port(
            manifest,
            user_config,
            ports.clone(),
            hf_token.clone(),
            None,
            preserved_port,
        )),
        DeployMethod::NativePythonBundle => {
            Box::new(python_bundle::PythonBundleDeploy::new_with_port(
                manifest,
                user_config,
                ports.clone(),
                hf_token.clone(),
                None,
                preserved_port,
            ))
        }
        DeployMethod::Docker => Box::new(docker::DockerDeploy::new_with_port(
            manifest,
            user_config,
            ports.clone(),
            hf_token.clone(),
            None,
            preserved_port,
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
    let services = match db.read() {
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
        // Compose stacks (infra like Milvus / iroh-relay) are torn down as a
        // whole project, not a single named container.
        let is_compose = crate::services::manifest::registry()
            .by_id(&svc.engine_id)
            .and_then(|m| m.deploy.docker.as_ref())
            .map(|d| d.compose_path.is_some() && d.context_path.is_none())
            .unwrap_or(false);
        if is_compose {
            // Per-instance project name (engine + host port), matching
            // prepare_compose, so the right stack is torn down. `down` (no `-v`)
            // removes the containers but keeps the project's named volumes, so a
            // later restart preserves data — same contract as the single-container
            // path which leaves Docker volumes intact.
            if let Some(port) = svc.runtime_port {
                let project = docker::compose_project_name(&svc.engine_id, port);
                let _ = tokio::process::Command::new("docker")
                    .arg("compose")
                    .arg("-p")
                    .arg(&project)
                    .arg("down")
                    .output()
                    .await;
            }
        } else if let Ok(docker) = bollard::Docker::connect_with_local_defaults() {
            // Names to tear down. Normally the exact `tentaflow-<engine>-<port>`
            // from the row. When `runtime_port` is missing (row never finished
            // deploy / legacy), sweep every container for this engine by name
            // prefix so a delete can't silently orphan a running container that
            // keeps eating GPU/RAM — the observed bug.
            let prefix = format!("tentaflow-{}-", svc.engine_id);
            let mut names: Vec<String> = match svc.runtime_port {
                Some(port) => vec![format!("tentaflow-{}-{}", svc.engine_id, port)],
                None => Vec::new(),
            };
            if names.is_empty() {
                if let Ok(o) = tokio::process::Command::new("docker")
                    .args(["ps", "-a", "--format", "{{.Names}}"])
                    .output()
                    .await
                {
                    for line in String::from_utf8_lossy(&o.stdout).lines() {
                        let n = line.trim();
                        if n.starts_with(&prefix) {
                            names.push(n.to_string());
                        }
                    }
                }
            }
            for name in names {
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
    }

    // Process shutdown: only the process-owning transports actually have a PID.
    if let Some(pid) = svc.runtime_pid {
        if matches!(svc.deploy_method, DM::NativeBinary | DM::NativePythonBundle) {
            // SIGTERM with short grace then SIGKILL — handled inside terminate.
            let _ = crate::deploy::process_ctl::terminate(pid as u32);
        }
    }

    // Embedded shutdown: in-process STT/TTS engines zyja w shared managerach —
    // nie maja kontenera ani PID. Bez wyladowania tutaj delete usuwa tylko row
    // z DB, a silnik dalej obsluguje requesty (objaw: "usunalem serwis a dalej
    // dzialal"). Backend routingu STT zdejmuje supervisor (`unregister_backend`),
    // ale sam zaladowany model embedded trzeba zwolnic tu.
    if svc.deploy_method == DM::NativeEmbedded {
        match svc.category.as_str() {
            "tts" => {
                crate::tts::shared_tts_manager()
                    .write()
                    .await
                    .unregister(&svc.engine_id);
            }
            "stt" => {
                let _ = crate::stt::shared_stt_manager()
                    .write()
                    .await
                    .unload_model()
                    .await;
            }
            _ => {}
        }
    }

    // NIE zwalniamy portow przy stop(). Port to permanentny atrybut serwisu
    // — przyznany przy `deploy()`, zwalniany dopiero przy delete (gdy row
    // znika z DB). Restart / pause / crash zostawia port w `leased`,
    // zeby kolejny respawn dostal dokladnie ten sam port przez
    // `acquire_or_specific(svc.runtime_port)`.
    let _ = ports;

    Ok(())
}

/// Wariant `stop` z weryfikacją wyniku dla ścieżki redeploy. `stop` jest
/// best-effort (świadomie zjada błędy Dockera, bo delete-row ma się udać nawet
/// gdy daemon nie odpowiada), ale przy redeployu MUSIMY wiedzieć, czy stary
/// kontener naprawdę zniknął — inaczej nowy deploy na tej samej maszynie GPU
/// wystartuje obok osieroconego kontenera i wpadną razem w OOM. Najpierw
/// wykonujemy normalny `stop`, potem dla docker-deployów potwierdzamy w
/// daemonie, że żaden kontener pasujący do wzorca nazwy już nie istnieje.
pub async fn stop_checked(
    svc: &crate::services_repo::services::ServiceRow,
    ports: Arc<PortAllocator>,
) -> DeployResult<()> {
    use crate::services_repo::services::DeployMethod as DM;

    stop(svc, ports).await?;

    // Native (binary / python-bundle): `stop()` woła `terminate()` best-effort i
    // ZJADA jego błąd, więc sam stop nie gwarantuje, że proces naprawdę zniknął.
    // Przy redeployu to krytyczne — żywy stary proces trzymałby port/VRAM i nowy
    // deploy wpadłby w duplikat runtime'u / OOM. Reużywamy detektora żywotności
    // supervisora (`process_ctl::is_alive`, który wykrywa też zombie przez
    // /proc/<pid>/status), żeby potwierdzić śmierć po stopie.
    if matches!(svc.deploy_method, DM::NativeBinary | DM::NativePythonBundle) {
        if let Some(pid) = svc.runtime_pid {
            let pid = pid as u32;
            if crate::deploy::process_ctl::is_alive(pid) {
                return Err(DeployError::Other(format!(
                    "stop_checked: native process pid={} still alive after stop",
                    pid
                )));
            }
        }
    }

    #[cfg(feature = "docker")]
    if svc.deploy_method == DM::Docker {
        // Compose-stacki znikają jako cały projekt — `docker compose down`
        // wyżej już to zrobił i nie mamy stabilnej nazwy pojedynczego
        // kontenera do sprawdzenia, więc weryfikujemy tylko single-container.
        let is_compose = crate::services::manifest::registry()
            .by_id(&svc.engine_id)
            .and_then(|m| m.deploy.docker.as_ref())
            .map(|d| d.compose_path.is_some() && d.context_path.is_none())
            .unwrap_or(false);
        if !is_compose {
            let docker = bollard::Docker::connect_with_local_defaults().map_err(|e| {
                DeployError::Other(format!("stop_checked: cannot reach docker daemon: {}", e))
            })?;
            let prefix = format!("tentaflow-{}-", svc.engine_id);
            let expected = svc
                .runtime_port
                .map(|port| format!("tentaflow-{}-{}", svc.engine_id, port));
            let listed = docker
                .list_containers(Some(bollard::query_parameters::ListContainersOptions {
                    all: true,
                    ..Default::default()
                }))
                .await
                .map_err(|e| {
                    DeployError::Other(format!("stop_checked: list containers: {}", e))
                })?;
            // Nazwy w bollard mają wiodący `/`; normalizujemy przed porównaniem.
            let lingering: Vec<String> = listed
                .into_iter()
                .filter_map(|c| c.names)
                .flatten()
                .map(|n| n.trim_start_matches('/').to_string())
                .filter(|n| match &expected {
                    Some(name) => n == name,
                    None => n.starts_with(&prefix),
                })
                .collect();
            if !lingering.is_empty() {
                return Err(DeployError::Other(format!(
                    "stop_checked: container still present after stop: {}",
                    lingering.join(", ")
                )));
            }
        }
    }

    Ok(())
}

/// Znajduje PID nasłuchujacy na danym porcie (TCP, 127.0.0.1) i wysyla
/// SIGTERM, po 1.5s SIGKILL. Uzywane przed respawn — gdy stary proces
/// serwisu zyje na preserved_port (zombie po crash tentaflow albo
/// run_loop respawn na zywym serwisie), strategy.prepare() dostalby
/// "port zajety". No-op gdy nikt nie nasluchuje.
async fn kill_listener_on_port(port: u16) {
    let pid_opt = tokio::task::spawn_blocking(move || find_listener_pid(port))
        .await
        .ok()
        .flatten();
    let Some(pid) = pid_opt else {
        return;
    };
    tracing::info!(
        "respawn: killing leftover process pid={} on port {}",
        pid,
        port
    );
    let _ = crate::deploy::process_ctl::terminate(pid);
    // Krotki grace na zwolnienie portu w jadrze (TCP_LISTEN -> CLOSED).
    for _ in 0..20 {
        if is_listener_gone(port) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

fn find_listener_pid(port: u16) -> Option<u32> {
    let out = std::process::Command::new("ss")
        .args(["-Hlntp", &format!("sport = :{}", port)])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if let Some(start) = line.find("pid=") {
            let rest = &line[start + 4..];
            let end = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            if let Ok(pid) = rest[..end].parse::<u32>() {
                return Some(pid);
            }
        }
    }
    None
}

fn is_listener_gone(port: u16) -> bool {
    use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
    TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).is_ok()
}

// ----- DB helpers -----------------------------------------------------------

/// Runs a closure inside a single SQLite transaction on the pool's writer
/// connection. Commits on Ok, rolls back on Err.
pub(crate) fn with_tx<F, T>(db: &DbPool, f: F) -> DeployResult<T>
where
    F: FnOnce(&Transaction<'_>) -> DeployResult<T>,
{
    let mut conn = db
        .write()
        .map_err(|e| DeployError::Database(format!("db write: {}", e)))?;
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

/// Marks a service row `failed` after the async `deploy()` worker failed.
///
/// Redeploy vs świeży deploy rozchodzą się TUTAJ. Dla redeployu stary runtime
/// został UBITY (`stop_checked`) zanim worker wystartował — gdyby `deploy()`
/// padło, pola runtime'u (pid/port/sidecar/endpoint) na wierszu wskazują na ten
/// martwy runtime. Bez ich wyzerowania `reconcile` dalej trzyma live handle, a
/// resolver routuje ruch do martwego/stale endpointu. Stąd
/// `mark_failed_clear_runtime` (failed + NULL na polach runtime'u). Dla świeżego
/// deployu placeholder nigdy nie miał żywego runtime'u, więc generyczny
/// `mark_deploy_failed` (zachowuje pola/active_deploy_id) jest poprawny — zero
/// regresji.
fn mark_worker_deploy_failed(db: &DbPool, job: &DeployJob, deploy_id: &str, message: &str) {
    if let Ok(conn) = db.write() {
        if job.is_redeploy {
            let _ = services_repo::mark_failed_clear_runtime(&conn, job.service_id, message);
        } else {
            let _ = services_repo::mark_deploy_failed(
                &conn,
                job.service_id,
                deploy_id,
                ServiceStatus::Failed,
                Some(message),
            );
        }
    }
}

// ----- Shared helpers used by strategies -----------------------------------

/// Builds the canonical `NewService` row from the prepared state.
pub(crate) fn build_new_service(prepared: &PreparedDeploy, status: ServiceStatus) -> NewService {
    // Zapamietujemy hash drzewa zrodel z momentu deployu, aby pozniej wykryc, ze
    // wbudowany bundle zostal zaktualizowany (snapshot porownuje go z aktualnym
    // hashem manifestu). embedded/external nie maja buildowalnego drzewa -> pusty.
    let deployed_source_hash = crate::services::manifest::registry()
        .by_id(&prepared.engine_id)
        .map(|m| match prepared.deploy_method {
            DeployMethod::Docker => m.docker_source_hash.clone(),
            DeployMethod::NativeBinary | DeployMethod::NativePythonBundle => {
                m.native_source_hash.clone()
            }
            _ => String::new(),
        })
        .unwrap_or_default();
    NewService {
        engine_id: prepared.engine_id.clone(),
        category: prepared.category.clone(),
        display_name: prepared.display_name.clone(),
        deploy_method: prepared.deploy_method,
        transport: prepared.transport,
        status,
        // Domyslnie pinned na desktop/serwerze: po Ctrl+C stop_all_supervised
        // terminuje procesy (zwalnia VRAM/porty), a przy starcie
        // supervisor.first_tick → auto_start_pinned respawnuje serwis. Na mobile
        // domyslnie UNPINNED (lazy load + memory guard) — patrz default_pinned().
        // Odpinanie/przypinanie zostaje pod kontrola usera (przycisk pin w GUI).
        pinned: default_pinned(),
        paused: false,
        runtime_pid: prepared.runtime.pid,
        runtime_port: prepared.runtime.port,
        sidecar_quic_port: prepared.runtime.sidecar_port,
        endpoint_url: prepared.runtime.endpoint_url.clone(),
        config_json: prepared.config_json.clone(),
        active_deploy_id: String::new(),
        last_deploy_id: String::new(),
        deployment_progress_pct: if status == ServiceStatus::Running {
            100
        } else {
            0
        },
        deployed_source_hash,
    }
}

/// Domyslny `pinned` przy deployu zalezny od platformy.
///
/// Mobile (iOS/Android) → `false`: pamiec aplikacji jest ograniczona, wiec
/// modele domyslnie sa UNPINNED — przygotowane (pobrane, routowalne) ale NIE
/// ladowane przy boocie. Laduja sie leniwie na pierwsze zadanie, a memory guard
/// zwalnia je gdy idle / przy wymianie (supervisor boot = pinned-only +
/// eviction single-resident). User moze recznie przypiac.
///
/// Pozostale nody → `true`: zachowanie jak dotad (boot-load + rezydentnie).
/// Lazy loading dziala tam tez, ale wlaczany recznie przez odpiecie serwisu.
pub(crate) fn default_pinned() -> bool {
    !cfg!(any(target_os = "ios", target_os = "android"))
}

fn build_placeholder_service(
    method: DeployMethod,
    manifest: &ServiceManifest,
    config_json: &str,
    deploy_id: &str,
) -> NewService {
    NewService {
        engine_id: manifest.engine.id.clone(),
        category: category_tag(manifest).to_string(),
        display_name: resolve_display_name(manifest),
        deploy_method: method,
        transport: placeholder_transport(method),
        status: ServiceStatus::Deploying,
        pinned: default_pinned(),
        paused: false,
        runtime_pid: None,
        runtime_port: None,
        sidecar_quic_port: None,
        endpoint_url: None,
        config_json: config_json.to_string(),
        active_deploy_id: deploy_id.to_string(),
        last_deploy_id: deploy_id.to_string(),
        deployment_progress_pct: 0,
        deployed_source_hash: String::new(),
    }
}

fn placeholder_transport(method: DeployMethod) -> Transport {
    match method {
        DeployMethod::NativeEmbedded => Transport::Embedded,
        DeployMethod::External => Transport::ExternalHttp,
        DeployMethod::Docker => Transport::SidecarQuic,
        DeployMethod::NativeBinary | DeployMethod::NativePythonBundle => Transport::HttpDirect,
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
    /// Opcjonalny hard ceiling czasu warmupu. `None` = no timeout (default
    /// zachowanie — duze modele 70B+ moga ladowac 10-30 min). Operator
    /// moze ustawic explicit deadline (np. dla CI/CD) — po przekroczeniu
    /// probe zwraca `ProcessExited(None)`, caller robi rollback.
    ///
    /// W produkcji preferujemy `None` zeby nie zabijac legitnych dlugich
    /// loadow. User widzi PROGRES przez `progress_message` heartbeat w
    /// supervisor i moze recznie anulowac deploy w GUI gdy uzna ze cos
    /// wisi (np. CUDA OOM gdzie parent uvicorn pozostaje alive ale nie
    /// odpowiada na readiness).
    pub max_wait: Option<std::time::Duration>,
}

/// Smart liveness+readiness probe. Loops until one of:
///
/// * a readiness URL answers 2xx → `Ready`;
/// * `is_alive_check` reports the process gone → `ProcessExited`;
/// * `max_wait` upłynął (gdy ustawiony) → `ProcessExited(None)`.
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
        if let Some(deadline) = cfg.max_wait {
            if started.elapsed() >= deadline {
                if let Some(sink) = &cfg.log_sink {
                    sink.info(&format!(
                        "[health] timeout after {}s — engine alive but not ready (likely crashed worker / CUDA OOM); failing deploy",
                        started.elapsed().as_secs()
                    ));
                }
                return SmartProbeOutcome::ProcessExited(None);
            }
        }

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

    // Cloud external providers (requires_api_key) are addressed by the real API
    // model id — the preset's `repo` (e.g. "gpt-5.5") — not the catalog preset id
    // ("gpt-5-5"), so chat sends a name the provider accepts and the editor's
    // live-model picker can pre-check it.
    let cloud_external = manifest
        .deploy
        .external
        .as_ref()
        .map(|e| e.requires_api_key)
        .unwrap_or(false);
    let preset_model_name = |p: &crate::services::manifest::ModelPreset| -> String {
        if cloud_external {
            p.repo.clone()
        } else {
            p.id.clone()
        }
    };

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
                model_name: preset_model_name(p),
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
        model_name: preset_model_name(chosen),
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
/// Czy ten deploy faktycznie pobiera model z Hugging Face. Single source dla
/// bramkowania `HF_TOKEN` (docker + native): silniki infra bez modelu (searxng,
/// browser-renderer, tools) NIE rozwiazuja repo modelu, wiec nie moga dostac
/// sekretu w env. Token leci wylacznie do silnikow, ktore realnie sciagaja wagi
/// z HF — inaczej sekret byl czytelny w env niezwiazanych kontenerow/procesow.
pub(crate) fn engine_uses_hf_model(
    manifest: &ServiceManifest,
    user_config: &serde_json::Value,
) -> bool {
    // Allow-list, nie deny-list: token leci wylacznie do silnikow, ktore
    // POZYTYWNIE deklaruja model HF. Inaczej silnik kategorii Agents
    // (np. teams-bot — brak `requires_model`, brak `model_presets`) ze
    // spreparowanym `model_repo` w config_json przeszedlby bramke i dostal
    // HF_TOKEN do env. Sam `model_repo` w configu NIE wystarcza.

    // Silniki bez rejestru modeli (Infra, Agents) NIGDY nie dostaja tokenu —
    // ten sam predykat co przy budowie wierszy modeli (single source).
    if manifest.engine.is_model_less() {
        return false;
    }
    // Silniki ciagnace wagi z WLASNEGO rejestru (Ollama -> rejestr Ollama,
    // ComfyUI -> civitai/wlasny mechanizm) NIGDY nie odpytuja HF, wiec token
    // do nich nie leci — nawet jesli ich `model_presets`/`model_repo` wygladaja
    // jak repo HF. Niepusta lista presetow nie dowodzi pobierania z HF.
    if manifest.engine.uses_own_model_registry() {
        return false;
    }
    // Silnik jawnie deklarujacy brak modelu — to samo: brak tokenu.
    if matches!(manifest.engine.requires_model, Some(false)) {
        return false;
    }
    // Wymagana POZYTYWNA deklaracja modelu HF z manifestu: albo jawne
    // `requires_model = true`, albo niepusta lista `model_presets`. Bez tego
    // manifest nie deklaruje realnie modelu i token nie wycieka.
    let declares_hf_model =
        matches!(manifest.engine.requires_model, Some(true)) || !manifest.model_presets.is_empty();
    if !declares_hf_model {
        return false;
    }
    // I dopiero gdy faktycznie da sie rozwiazac repo modelu do sciagniecia.
    resolve_model_repo(manifest, user_config).is_some()
}

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

/// Resolves the `[[model_preset]]` selected for this deploy, mirroring
/// `resolve_model_repo` precedence: explicit `model_preset_id`, else the
/// recommended preset, else the first. `None` when the manifest has no presets.
pub(crate) fn resolve_selected_preset<'a>(
    manifest: &'a ServiceManifest,
    user_config: &serde_json::Value,
) -> Option<&'a crate::services::manifest::ModelPreset> {
    if let Some(id) = user_config
        .get("model_preset_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if let Some(p) = manifest.model_presets.iter().find(|m| m.id == id) {
            return Some(p);
        }
    }
    if manifest.model_presets.is_empty() {
        return None;
    }
    Some(
        manifest
            .model_presets
            .iter()
            .find(|p| p.recommended)
            .unwrap_or(&manifest.model_presets[0]),
    )
}

/// Resolves the name the service advertises for its default model — the same
/// value `models_from_manifest` writes as `model_name` and the executor
/// rewrites `request.model` to before dispatch. For OpenAI-compatible HTTP
/// engines (vLLM) this MUST be passed to the backend as `--served-model-name`,
/// otherwise vLLM serves the model under its repo path (`--model ${MODEL}`)
/// while we route by the preset id slug — a guaranteed 404 whenever
/// `preset.id != preset.repo`. Selection mirrors `resolve_model_repo`:
///   1. custom `model_repo` → the repo (model_name == repo in that path).
///   2. `model_preset_id` → `preset.id`.
///   3. recommended (or first) preset → `preset.id`.
pub(crate) fn resolve_served_model_name(
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
            return Some(p.id.clone());
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
    Some(chosen.id.clone())
}

/// Maps a preset's `speculator_method` to the vLLM container's `VLLM_SPEC_METHOD`
/// vocabulary (`ngram` / `mtp` / `draft`). `None` for methods the entrypoint
/// contract does not assemble (e.g. eagle/medusa flow straight through
/// `vllm_args`).
fn vllm_spec_method(method: &str) -> Option<&'static str> {
    match method.trim().to_ascii_lowercase().as_str() {
        "ngram" => Some("ngram"),
        "mtp" => Some("mtp"),
        "draft" | "draft_model" => Some("draft"),
        _ => None,
    }
}

/// Engine env passthrough from the deploy config `engine_env` object (e.g. the
/// vLLM recipe's `VLLM_USE_FLASHINFER_MOE_FP4`). Shared by the native and docker
/// deploy paths. Reserved runtime keys owned by the deploy flow are never
/// overridden — recipes only set `VLLM_*`-style tuning vars, guarded defensively.
pub(crate) fn apply_engine_env(user_config: &serde_json::Value, env: &mut HashMap<String, String>) {
    const RESERVED: &[&str] = &[
        "PORT",
        "MODEL",
        "SERVED_MODEL_NAME",
        "HF_TOKEN",
        "HF_HOME",
        "GPU_MEMORY_UTILIZATION",
        "VLLM_ARGS",
    ];
    let Some(obj) = user_config.get("engine_env").and_then(|v| v.as_object()) else {
        return;
    };
    for (k, v) in obj {
        let key = k.trim();
        if key.is_empty() || RESERVED.contains(&key) {
            continue;
        }
        if let Some(val) = v.as_str() {
            env.insert(key.to_string(), val.to_string());
        } else if !v.is_null() {
            env.insert(key.to_string(), v.to_string());
        }
    }
}

/// GPU card selection for native (python-bundle) deploys. Docker has `--gpus`
/// to scope visible devices; native spawns the engine process directly, so the
/// only portable knob is the vendor visibility env (`CUDA_VISIBLE_DEVICES` and
/// the AMD/ROCm equivalents). Without this the engine grabs card 0 / all cards
/// regardless of the wizard's GPU selection. Runs AFTER `apply_engine_env`, so an
/// explicit `engine_env.CUDA_VISIBLE_DEVICES` wins — we only fill the gap.
pub(crate) fn apply_gpu_selection_env(
    user_config: &serde_json::Value,
    env: &mut HashMap<String, String>,
) {
    const VISIBILITY_KEYS: &[&str] = &[
        "CUDA_VISIBLE_DEVICES",
        "HIP_VISIBLE_DEVICES",
        "ROCR_VISIBLE_DEVICES",
    ];
    let set_visibility = |env: &mut HashMap<String, String>, value: &str| {
        for key in VISIBILITY_KEYS {
            env.entry((*key).to_string())
                .or_insert_with(|| value.to_string());
        }
    };

    let mode = user_config
        .get("gpu_select_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("all");
    match mode {
        // Empty visibility string = no cards visible, forcing CPU execution.
        "none" => set_visibility(env, ""),
        "specific" => {
            let ids: Vec<String> = user_config
                .get("gpu_ids")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|id| match id {
                            serde_json::Value::String(s) => {
                                let t = s.trim();
                                (!t.is_empty()).then(|| t.to_string())
                            }
                            serde_json::Value::Number(n) => Some(n.to_string()),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            // Empty / missing list behaves like "all" — inherit the environment.
            if !ids.is_empty() {
                set_visibility(env, &ids.join(","));
            }
        }
        // "all" / unknown / missing: inherit the ambient environment.
        _ => {}
    }
}

/// Builds the `VLLM_*` container env for a vLLM featured preset: NVFP4
/// self-quantization (`VLLM_MODEL_QUANTIZE` / `VLLM_SPEC_DRAFT_QUANTIZE`) and
/// speculative decoding (`VLLM_SPEC_METHOD` / `VLLM_SPEC_REPO` /
/// `VLLM_SPEC_NUM_TOKENS`). The entrypoint quantizes before serving and
/// assembles `--speculative-config` with resolved local paths. `HF_TOKEN`
/// (gated repos like Bielik) jest przekazywany jawnie z `deploy()` (rozwiazany
/// per-node z secure setting), NIGDY z `user_config` — sekret nie moze trafic do
/// config_json. Empty when the preset carries no vLLM speculative/quantize config
/// AND no token.
pub(crate) fn vllm_deploy_env(
    manifest: &ServiceManifest,
    user_config: &serde_json::Value,
    hf_token: Option<&str>,
) -> HashMap<String, String> {
    let mut env = HashMap::new();

    // Token gated repo (Bielik) musi byc widoczny niezaleznie od tego czy preset
    // niesie speculative/quantize — inaczej pobieranie wag dla zwyklego gated
    // modelu leci nieuwierzytelnione i CDN throttluje do zawieszenia.
    if let Some(token) = hf_token.map(str::trim).filter(|s| !s.is_empty()) {
        env.insert("HF_TOKEN".into(), token.to_string());
    }

    let Some(preset) = resolve_selected_preset(manifest, user_config) else {
        return env;
    };

    if let Some(scheme) = preset.vllm.as_ref().and_then(|v| v.quantize.as_ref()) {
        env.insert("VLLM_MODEL_QUANTIZE".into(), scheme.clone());
    }

    if let Some(method) = preset
        .speculator_method
        .as_deref()
        .and_then(vllm_spec_method)
    {
        env.insert("VLLM_SPEC_METHOD".into(), method.to_string());
        let ntok = preset.speculator_num_tokens.unwrap_or(4);
        env.insert("VLLM_SPEC_NUM_TOKENS".into(), ntok.to_string());
        if method == "draft" {
            if let Some(repo) = &preset.speculator_repo {
                env.insert("VLLM_SPEC_REPO".into(), repo.clone());
            }
            if let Some(scheme) = preset.vllm.as_ref().and_then(|v| v.quantize_draft.as_ref()) {
                env.insert("VLLM_SPEC_DRAFT_QUANTIZE".into(), scheme.clone());
            }
        }
    }

    env
}

/// Builds the `--speculative-config <json>` argument for the NATIVE python-bundle
/// path (which has no entrypoint to assemble it from env). The draft method uses
/// the `speculator_repo` HF repo directly. Returns `None` when the preset has no
/// supported speculative method.
pub(crate) fn vllm_native_speculative_arg(
    manifest: &ServiceManifest,
    user_config: &serde_json::Value,
) -> Option<Vec<String>> {
    let preset = resolve_selected_preset(manifest, user_config)?;
    let method = vllm_spec_method(preset.speculator_method.as_deref()?)?;
    let ntok = preset.speculator_num_tokens.unwrap_or(4);
    let json = match method {
        "ngram" => format!(
            "{{\"method\":\"ngram\",\"num_speculative_tokens\":{ntok},\"prompt_lookup_max\":4,\"prompt_lookup_min\":2}}"
        ),
        "mtp" => format!("{{\"method\":\"mtp\",\"num_speculative_tokens\":{ntok}}}"),
        "draft" => {
            let repo = preset.speculator_repo.as_ref()?;
            format!("{{\"model\":\"{repo}\",\"num_speculative_tokens\":{ntok}}}")
        }
        _ => return None,
    };
    // Flaga i JSON jako DWA osobne elementy argv. JSON nigdy nie przechodzi
    // przez shlex/xargs, wiec wewnetrzne cudzyslowy zostaja nietkniete i
    // vLLM dostaje poprawny `--speculative-config {"model":...}`.
    Some(vec!["--speculative-config".to_string(), json])
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
        // Cloud API families (Anthropic/Azure/ElevenLabs/Soniox) are reached via
        // the manifest's explicit `detection_endpoint`, never through this
        // host:port builder; they keep the bare base for completeness.
        ApiKind::OllamaNative
        | ApiKind::SherpaTts
        | ApiKind::SherpaStt
        | ApiKind::Comfyui
        | ApiKind::Anthropic
        | ApiKind::AzureOpenai
        | ApiKind::Elevenlabs
        | ApiKind::Soniox
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
            provider: None,
            resource_kind: None,
            requires_model: Some(true),
            gpu_supported: None,
            default_port: 8000,
            dgx_spark: None,
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
                gpus: None,
                ..Default::default()
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
        let (app, req) = apply_parameters_deploy(&m, &json!({}), DeployTarget::Docker).unwrap();
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
        let (app, _) = apply_parameters_deploy(&m, &user_config, DeployTarget::Docker).unwrap();
        assert_eq!(app.env.get("GPU_MEMORY_UTILIZATION").unwrap(), "0.6");
    }

    #[test]
    fn missing_user_value_uses_default() {
        let m = manifest_with_params(vec![float_param(
            "gpu_memory_utilization",
            "GPU_MEMORY_UTILIZATION",
            0.9,
        )]);
        let (app, _) = apply_parameters_deploy(&m, &json!({}), DeployTarget::Docker).unwrap();
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
        let err = apply_parameters_deploy(&m, &user_config, DeployTarget::Docker).unwrap_err();
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
        let err = apply_parameters_deploy(&m, &user_config, DeployTarget::Docker).unwrap_err();
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
        assert_eq!(
            req.whisper_overridable.get("default_beam_size").unwrap(),
            &json!(8)
        );
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
            requires_api_key: false,
        });

        let user_config = json!({ "parameters": { "context_size": 16384 } });
        let (app, req) = apply_parameters_deploy(&m, &user_config, DeployTarget::External).unwrap();
        assert!(app.env.is_empty());
        assert_eq!(req.ollama_options.get("num_ctx").unwrap(), &json!(16384));
    }
}

#[cfg(test)]
mod hf_token_gate_tests {
    use super::*;
    use crate::services::manifest::{
        ApiKind, Category, DeploySection, DockerDeploy, DockerTransport, Engine, ModelPreset,
        ResourceKind, TargetOs,
    };
    use serde_json::json;

    fn engine(
        category: Category,
        resource_kind: Option<ResourceKind>,
        requires_model: Option<bool>,
        api: ApiKind,
    ) -> Engine {
        Engine {
            id: "test".into(),
            category,
            name: "test".into(),
            description_pl: String::new(),
            description_en: String::new(),
            homepage: String::new(),
            license: String::new(),
            icon: None,
            provider: None,
            resource_kind,
            requires_model,
            gpu_supported: None,
            default_port: 8000,
            dgx_spark: None,
            api,
            version: "0.1.0".into(),
            service_surfaces: None,
            input_modalities: None,
            output_modalities: None,
        }
    }

    fn docker() -> DeploySection {
        DeploySection {
            docker: Some(DockerDeploy {
                context_path: Some("docker/test".into()),
                compose_path: None,
                platforms: vec![TargetOs::Linux],
                download_image: None,
                download_size_mb: None,
                transport: Some(DockerTransport::SidecarQuic),
                gpus: None,
                ..Default::default()
            }),
            native: None,
            external: None,
        }
    }

    fn preset(repo: &str) -> ModelPreset {
        serde_json::from_value(json!({
            "id": "p",
            "display_name": "p",
            "repo": repo,
            "recommended": true,
        }))
        .expect("ModelPreset")
    }

    fn manifest(
        resource_kind: Option<ResourceKind>,
        requires_model: Option<bool>,
        presets: Vec<ModelPreset>,
    ) -> ServiceManifest {
        manifest_api(
            Category::Llm,
            resource_kind,
            requires_model,
            presets,
            ApiKind::OpenaiCompatible,
        )
    }

    fn manifest_cat(
        category: Category,
        resource_kind: Option<ResourceKind>,
        requires_model: Option<bool>,
        presets: Vec<ModelPreset>,
    ) -> ServiceManifest {
        manifest_api(
            category,
            resource_kind,
            requires_model,
            presets,
            ApiKind::OpenaiCompatible,
        )
    }

    fn manifest_api(
        category: Category,
        resource_kind: Option<ResourceKind>,
        requires_model: Option<bool>,
        presets: Vec<ModelPreset>,
        api: ApiKind,
    ) -> ServiceManifest {
        ServiceManifest {
            engine: engine(category, resource_kind, requires_model, api),
            deploy: docker(),
            model_presets: presets,
            parameters: vec![],
            docker_source_hash: String::new(),
            native_source_hash: String::new(),
        }
    }

    #[test]
    fn model_capable_engine_with_repo_gets_token() {
        // Pozytywna deklaracja modelu: niepusta lista presetow.
        let m = manifest(
            Some(ResourceKind::Ai),
            Some(true),
            vec![preset("Qwen/Qwen3-0.6B")],
        );
        assert!(engine_uses_hf_model(&m, &json!({})));
        // Pozytywna deklaracja przez `requires_model = true` + custom repo z wizarda.
        let m2 = manifest(None, Some(true), vec![]);
        assert!(engine_uses_hf_model(
            &m2,
            &json!({ "model_repo": "Qwen/Qwen3-0.6B" })
        ));
    }

    #[test]
    fn engine_without_positive_model_declaration_gets_no_token() {
        // Brak `requires_model`, brak presetow — sam `model_repo` w configu
        // (np. spreparowany payload) NIE wystarcza do bramki allow-list.
        let m = manifest(None, None, vec![]);
        assert!(!engine_uses_hf_model(
            &m,
            &json!({ "model_repo": "Qwen/Qwen3-0.6B" })
        ));
    }

    #[test]
    fn agents_engine_never_gets_token_even_with_model_repo() {
        // Silnik kategorii Agents (np. teams-bot: brak model_presets, brak
        // requires_model=false) ze spreparowanym `model_repo` NIE moze dostac
        // HF_TOKEN — kategoria Agents jest model-less.
        let m = manifest_cat(Category::Agents, None, None, vec![]);
        assert!(!engine_uses_hf_model(
            &m,
            &json!({ "model_repo": "Qwen/Qwen3-0.6B" })
        ));
        // Nawet jesli ktos doda presety do manifestu Agents — wciaz brak tokenu.
        let m2 = manifest_cat(
            Category::Agents,
            None,
            None,
            vec![preset("Qwen/Qwen3-0.6B")],
        );
        assert!(!engine_uses_hf_model(&m2, &json!({})));
    }

    #[test]
    fn infra_engine_never_gets_token_even_with_model_repo() {
        // Spreparowany/stary config z niepustym model_repo nie moze przeciec
        // HF_TOKEN do silnika infra.
        let m = manifest(Some(ResourceKind::Infra), None, vec![]);
        assert!(!engine_uses_hf_model(
            &m,
            &json!({ "model_repo": "Qwen/Qwen3-0.6B" })
        ));
    }

    #[test]
    fn requires_model_false_never_gets_token_even_with_model_repo() {
        let m = manifest(None, Some(false), vec![]);
        assert!(!engine_uses_hf_model(
            &m,
            &json!({ "model_repo": "Qwen/Qwen3-0.6B" })
        ));
    }

    #[test]
    fn model_capable_engine_without_repo_gets_no_token() {
        let m = manifest(Some(ResourceKind::Ai), Some(true), vec![]);
        assert!(!engine_uses_hf_model(&m, &json!({})));
    }

    #[test]
    fn ollama_engine_never_gets_token_despite_presets_and_repo() {
        // Ollama deklaruje model (presety + requires_model) ale ciagnie wagi z
        // rejestru Ollama (`ollama pull`), nie z HF. Bramka musi wykluczyc
        // silnik po `ApiKind::OllamaNative`, nawet gdy preset/`model_repo`
        // wygladaja jak repo HF.
        let m = manifest_api(
            Category::Llm,
            Some(ResourceKind::Ai),
            Some(true),
            vec![preset("qwen3.5:0.8b")],
            ApiKind::OllamaNative,
        );
        assert!(!engine_uses_hf_model(&m, &json!({})));
        assert!(!engine_uses_hf_model(
            &m,
            &json!({ "model_repo": "Qwen/Qwen3-0.6B" })
        ));
    }

    #[test]
    fn comfyui_engine_never_gets_token_despite_hf_looking_preset() {
        // ComfyUI preset repo wyglada jak HF (`runwayml/...`) ale model
        // pochodzi z wlasnego mechanizmu / civitai — token nie leci.
        let m = manifest_api(
            Category::ImageGen,
            Some(ResourceKind::Ai),
            Some(true),
            vec![preset("runwayml/stable-diffusion-v1-5")],
            ApiKind::Comfyui,
        );
        assert!(!engine_uses_hf_model(&m, &json!({})));
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
    let to_value_map =
        |m: &HashMap<String, serde_json::Value>| -> serde_json::Map<String, serde_json::Value> {
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
    // Cap 0.85 (nie 0.92): vLLM przy profilowaniu pamieci (zwlaszcza duzy
    // max_model_len) przekracza ustawiony budzet o kilka % i alokuje peaki
    // aktywacji ponad KV-cache; przy 0.92 brakowalo ~1.5GB zapasu -> CUDA OOM.
    let ratio = (0.94 * free_ratio).min(0.85);
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
    /// xtts, voxcpm).
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
    NoBindingForTarget { key: String, target: DeployTarget },
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

    let user_params = user_config.get("parameters").and_then(|v| v.as_object());

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
    // VLLM_CACHE_ROOT — shared persistent cache for Triton kernels,
    // torch.compile artifacts and FlashInfer JIT. Set unconditionally
    // (harmless for non-vLLM engines, big win for any vLLM family).
    // For Docker the container path is used; native deploys override
    // with the host path in `python_venv::spawn_engine`.
    env.insert(
        "VLLM_CACHE_ROOT".into(),
        crate::paths::CONTAINER_VLLM_CACHE_PATH.to_string(),
    );
    // Read-timeout (sekundy) dla huggingface_hub. Bez niego martwe/throttled
    // polaczenie z HF CDN wisi w nieskonczonosc przy pobieraniu wag — po
    // timeoucie hub retryuje + resume. Dotyczy wszystkich silnikow pobierajacych
    // z HF (docker + binary). NIE wlaczamy HF_HUB_ENABLE_HF_TRANSFER — wymaga
    // pakietu hf_transfer w obrazie (ImportError gdy go brak).
    env.insert("HF_HUB_DOWNLOAD_TIMEOUT".into(), "30".into());
    env
}

pub(crate) fn is_cuda_vllm_engine(engine_id: &str) -> bool {
    matches!(engine_id, "vllm" | "vllm-spark")
}

pub(crate) fn strip_gpu_memory_utilization(raw: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let tokens: Vec<&str> = raw.split_whitespace().collect();
    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i];
        if tok == "--gpu-memory-utilization" {
            i += 2;
            continue;
        }
        if tok.starts_with("--gpu-memory-utilization=") {
            i += 1;
            continue;
        }
        out.push(tok.to_string());
        i += 1;
    }
    out.join(" ")
}

pub(crate) fn parse_gpu_memory_utilization_arg(raw: &str) -> Option<f64> {
    let mut iter = raw.split_whitespace();
    while let Some(tok) = iter.next() {
        if tok == "--gpu-memory-utilization" {
            return iter.next().and_then(|v| v.parse::<f64>().ok());
        }
        if let Some(rest) = tok.strip_prefix("--gpu-memory-utilization=") {
            return rest.parse::<f64>().ok();
        }
    }
    None
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
            Training => "training",
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
            Training => "training",
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
                provider: None,
                resource_kind: None,
                requires_model: None,
                gpu_supported: None,
                default_port: 8000,
                dgx_spark: None,
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
                featured: false,
                service_surfaces: None,
                input_modalities: None,
                output_modalities: None,
                speculator_repo: None,
                speculator_method: None,
                speculator_num_tokens: None,
                vllm: None,
                checkpoint_file: None,
            }],
            parameters: vec![],
            docker_source_hash: String::new(),
            native_source_hash: String::new(),
        }
    }

    fn open_db() -> DbPool {
        use std::sync::Arc;
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::db::migrations::run(&conn).unwrap();
        Arc::new(crate::db::Db::from_connection(conn))
    }

    fn make_job(
        db: &DbPool,
        method: DeployMethod,
        manifest: &ServiceManifest,
        cfg: &serde_json::Value,
        slug: Option<String>,
    ) -> DeployJob {
        create_deploy_job(method, manifest, cfg, db, "node-test", Some("1"), slug).unwrap()
    }

    fn test_cipher() -> crate::crypto::SettingsCipher {
        crate::crypto::SettingsCipher::new(&[0u8; 32])
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
            max_wait: None,
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
            max_wait: None,
        };
        let outcome = smart_health_probe(cfg, || async { Some(Some(137)) }).await;
        match outcome {
            SmartProbeOutcome::ProcessExited(Some(137)) => {}
            other => panic!("expected ProcessExited(137), got {:?}", other),
        }
    }

    /// Process zywy ale readiness URL nieosiagalny → po max_wait probe
    /// kończy z ProcessExited(None). Bez tego deploy::respawn wisial w
    /// nieskonczonosc gdy parent uvicorn alive ale child engine core
    /// crashowal (CUDA OOM).
    #[tokio::test]
    async fn smart_probe_times_out_when_alive_but_never_ready() {
        use std::time::Duration;

        let cfg = SmartProbeConfig {
            readiness_urls: vec!["http://127.0.0.1:1/health".to_string()],
            status_report_interval: Duration::from_secs(60),
            log_sink: None,
            max_wait: Some(Duration::from_millis(800)),
        };
        let outcome = smart_health_probe(cfg, || async { None }).await;
        match outcome {
            SmartProbeOutcome::ProcessExited(None) => {}
            other => panic!("expected ProcessExited(None) on timeout, got {:?}", other),
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
            let conn = db.write().unwrap();
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
        let job = make_job(&db, DeployMethod::NativeEmbedded, &manifest, &cfg, None);
        let result = deploy(
            job,
            DeployMethod::NativeEmbedded,
            &manifest,
            &cfg,
            &ports,
            &db,
            &test_cipher(),
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
        let conn = db.read().unwrap();
        let (status, error_text): (String, Option<String>) = conn
            .query_row(
                "SELECT status, error_message FROM deployments WHERE engine_id = 'emb-collide'",
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
        let job = make_job(&db, DeployMethod::NativeEmbedded, &manifest, &cfg, None);
        let outcome = deploy(
            job,
            DeployMethod::NativeEmbedded,
            &manifest,
            &cfg,
            &ports,
            &db,
            &test_cipher(),
            None,
        )
        .await
        .expect("embedded deploy succeeds");

        assert!(outcome.endpoint.handle.id > 0);
        assert_eq!(outcome.endpoint.transport, Transport::Embedded);
        // model_registry row was created with the service_id linked
        let conn = db.read().unwrap();
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
        let job = make_job(&db, DeployMethod::NativeEmbedded, &manifest, &cfg, None);
        let outcome = deploy(
            job,
            DeployMethod::NativeEmbedded,
            &manifest,
            &cfg,
            &ports,
            &db,
            &test_cipher(),
            None,
        )
        .await
        .expect("seed deploy succeeds");

        let count_before: i64 = {
            let conn = db.read().unwrap();
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
            &db,
            &test_cipher(),
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, DeployError::Manifest(_)));

        let count_after: i64 = {
            let conn = db.read().unwrap();
            conn.query_row("SELECT COUNT(*) FROM services", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(count_before, count_after, "respawn must not touch the DB");
        // Sanity: the seed deploy did create exactly one row.
        assert_eq!(count_after, 1);
        let _ = outcome;
    }

    // P2.2 / kanal ENV: vllm_deploy_env wstawia HF_TOKEN z jawnie podanego
    // tokenu (rozwiazanego per-node w deploy()), nie z user_config — nawet gdy
    // manifest nie ma presetu speculative/quantize (zwykly gated repo).
    #[test]
    fn vllm_deploy_env_injects_hf_token_from_explicit_arg() {
        let manifest = dummy_manifest("vllm", NativeRuntime::PythonBundle);
        let user_config = serde_json::json!({});
        let env = vllm_deploy_env(&manifest, &user_config, Some("hf_resolved_secret"));
        assert_eq!(
            env.get("HF_TOKEN").map(String::as_str),
            Some("hf_resolved_secret")
        );
    }

    #[test]
    fn vllm_deploy_env_skips_hf_token_when_none() {
        let manifest = dummy_manifest("vllm", NativeRuntime::PythonBundle);
        let user_config = serde_json::json!({});
        let env = vllm_deploy_env(&manifest, &user_config, None);
        assert!(!env.contains_key("HF_TOKEN"));
    }

    fn manifest_without_model(id: &str, runtime: NativeRuntime) -> ServiceManifest {
        let mut m = dummy_manifest(id, runtime);
        m.model_presets.clear();
        m
    }

    // P2.1/P2.2 bramka: deploy z modelem HF rozwiazuje repo, wiec dostaje token.
    #[test]
    fn engine_uses_hf_model_true_when_model_resolves() {
        let manifest = dummy_manifest("vllm", NativeRuntime::PythonBundle);
        let user_config = serde_json::json!({});
        assert!(engine_uses_hf_model(&manifest, &user_config));
    }

    // P2.1/P2.2 bramka: silnik infra (searxng/browser-renderer) bez presetow i
    // bez model_repo NIE rozwiazuje modelu — nie moze dostac HF_TOKEN w env.
    #[test]
    fn engine_uses_hf_model_false_without_model() {
        let manifest = manifest_without_model("searxng", NativeRuntime::PythonBundle);
        let user_config = serde_json::json!({});
        assert!(!engine_uses_hf_model(&manifest, &user_config));
    }

    // Custom model_repo z wizarda liczy sie jako model HF tylko gdy manifest
    // POZYTYWNIE deklaruje model (`requires_model = true`) — bramka allow-list.
    #[test]
    fn engine_uses_hf_model_true_with_custom_repo() {
        let mut manifest = manifest_without_model("vllm", NativeRuntime::PythonBundle);
        manifest.engine.requires_model = Some(true);
        let user_config = serde_json::json!({ "model_repo": "speakleash/Bielik" });
        assert!(engine_uses_hf_model(&manifest, &user_config));
    }

    // Sam custom model_repo bez pozytywnej deklaracji modelu w manifescie NIE
    // otwiera bramki — spreparowany payload nie wycieka HF_TOKEN.
    #[test]
    fn engine_uses_hf_model_false_with_custom_repo_but_no_declaration() {
        let manifest = manifest_without_model("vllm", NativeRuntime::PythonBundle);
        let user_config = serde_json::json!({ "model_repo": "speakleash/Bielik" });
        assert!(!engine_uses_hf_model(&manifest, &user_config));
    }

    // P1.1: forward cross-node zdejmuje hf_token z config_json zanim trafi do
    // mesh command. Mirror logiki w dispatch::handlers — sekret nie opuszcza noda.
    #[test]
    fn cross_node_forward_strips_hf_token_from_config_json() {
        let raw = serde_json::json!({
            "hf_token": "hf_node_secret_value",
            "model_repo": "speakleash/Bielik",
        });
        let raw_str = serde_json::to_string(&raw).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw_str).unwrap();
        let sanitized = strip_hf_token(&parsed);
        let forwarded = serde_json::to_string(&sanitized).unwrap();
        assert!(
            !forwarded.contains("hf_token"),
            "mesh command config_json leaked token key: {forwarded}"
        );
        assert!(
            !forwarded.contains("hf_node_secret_value"),
            "mesh command config_json leaked token value: {forwarded}"
        );
        assert!(forwarded.contains("speakleash/Bielik"));
    }

    #[test]
    fn strip_hf_token_removes_secret_but_keeps_rest() {
        let cfg = serde_json::json!({
            "hf_token": "hf_super_secret",
            "model_repo": "speakleash/Bielik",
            "gpu_memory_utilization": 0.9,
        });
        let stripped = strip_hf_token(&cfg);
        assert!(stripped.get("hf_token").is_none(), "token must be removed");
        assert_eq!(
            stripped.get("model_repo").and_then(|v| v.as_str()),
            Some("speakleash/Bielik")
        );
        assert_eq!(
            stripped
                .get("gpu_memory_utilization")
                .and_then(|v| v.as_f64()),
            Some(0.9)
        );
    }

    // Klucz test bezpieczenstwa P2.1: nawet gdy frontend wysle `hf_token` w
    // payloadzie deployu, `create_deploy_job` NIE moze go zapisac do
    // services.config_json ani deployments.config_json (oba ida do bazy
    // plaintextem i replikuja sie przez sync).
    #[test]
    fn create_deploy_job_never_persists_hf_token_in_config_json() {
        let db = open_db();
        let manifest = dummy_manifest("llama-cpp", NativeRuntime::Embedded);
        let cfg = serde_json::json!({
            "hf_token": "hf_leaky_secret_value",
            "model_repo": "speakleash/Bielik",
        });
        let job = create_deploy_job(
            DeployMethod::NativeEmbedded,
            &manifest,
            &cfg,
            &db,
            "node-test",
            Some("1"),
            None,
        )
        .unwrap();

        let conn = db.read().unwrap();
        let svc_config: String = conn
            .query_row(
                "SELECT config_json FROM services WHERE id = ?1",
                rusqlite::params![job.service_id],
                |r| r.get(0),
            )
            .unwrap();
        let dep_config: String = conn
            .query_row(
                "SELECT config_json FROM deployments WHERE id = ?1",
                rusqlite::params![job.deployment_id],
                |r| r.get(0),
            )
            .unwrap();

        assert!(
            !svc_config.contains("hf_token"),
            "services.config_json leaked token: {svc_config}"
        );
        assert!(
            !svc_config.contains("hf_leaky_secret_value"),
            "services.config_json leaked token value: {svc_config}"
        );
        assert!(
            !dep_config.contains("hf_token"),
            "deployments.config_json leaked token: {dep_config}"
        );
        assert!(
            !dep_config.contains("hf_leaky_secret_value"),
            "deployments.config_json leaked token value: {dep_config}"
        );
        // Reszta configu musi przetrwac.
        assert!(svc_config.contains("speakleash/Bielik"));
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
        let job = make_job(&db, DeployMethod::NativeBinary, &manifest, &cfg, None);
        let res = deploy(
            job,
            DeployMethod::NativeBinary,
            &manifest,
            &cfg,
            &ports,
            &db,
            &test_cipher(),
            None,
        )
        .await;
        assert!(res.is_err(), "deploy should fail when binary path invalid");

        // deployments row exists with status=failed.
        let conn = db.read().unwrap();
        let (status, err): (String, Option<String>) = conn
            .query_row(
                "SELECT status, error_message FROM deployments WHERE engine_id = 'bin-err' ORDER BY id DESC LIMIT 1",
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
        let job = make_job(
            &db,
            DeployMethod::NativeEmbedded,
            &manifest,
            &cfg,
            Some(slug.clone()),
        );
        deploy(
            job,
            DeployMethod::NativeEmbedded,
            &manifest,
            &cfg,
            &ports,
            &db,
            &test_cipher(),
            None,
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
        let job = make_job(
            &db,
            DeployMethod::NativeEmbedded,
            &manifest,
            &cfg,
            Some(slug.clone()),
        );

        let outcome = deploy(
            job,
            DeployMethod::NativeEmbedded,
            &manifest,
            &cfg,
            &ports,
            &db,
            &test_cipher(),
            Some(tx.clone()),
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
