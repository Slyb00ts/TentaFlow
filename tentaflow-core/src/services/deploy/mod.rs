// ============ File: services/deploy/mod.rs — unified atomic deploy entry point (Phase 2) ============
//
// Two-phase atomic deploy:
//   1. PREPARE — side effects (port alloc, image build, process spawn, health check).
//   2. COMMIT  — single DB transaction across services_v2 + model_registry_v2 +
//                deployments_v2. If it fails, ROLLBACK is invoked to undo prepare.
//
// The legacy `crate::deploy::runner` path is left untouched; this module writes only
// to the *_v2 tables and is wired to call sites in Phase 5.

pub mod binary;
pub mod docker;
pub mod embedded;
pub mod python_bundle;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::Transaction;

use crate::db::DbPool;
use crate::services::lifecycle::ServiceEndpoint;
use crate::services::manifest::ServiceManifest;
use crate::services::ports::PortAllocator;
use crate::services::transport::Transport;
use crate::services_repo::deployments::{
    self as deployments_repo, DeploymentStatus, NewDeployment,
};
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

// ----- Public types ---------------------------------------------------------

/// Outcome of a successful deploy: a runnable, registered endpoint plus the
/// deployments_v2 audit-row id.
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
/// the deployments_v2 row marked `failed` with the error text.
pub async fn deploy(
    method: DeployMethod,
    manifest: &ServiceManifest,
    user_config: &serde_json::Value,
    ports: Arc<PortAllocator>,
    db: DbPool,
) -> DeployResult<DeployOutcome> {
    // 1. Audit row first so we always have a paper trail, even on prepare failure.
    let deployment_id = with_tx(&db, |tx| {
        let id = deployments_repo::insert(
            tx,
            &NewDeployment {
                engine_id: manifest.engine.id.clone(),
                deploy_method: method.as_db_tag().to_string(),
                status: DeploymentStatus::Running,
                config_json: serde_json::to_string(user_config).ok(),
            },
        )?;
        Ok(id)
    })?;

    // 2. Pick strategy.
    let mut strategy: Box<dyn DeployStrategy> = match method {
        DeployMethod::NativeEmbedded => Box::new(embedded::EmbeddedDeploy::new(
            manifest.clone(),
            user_config.clone(),
        )),
        DeployMethod::NativeBinary => Box::new(binary::BinaryDeploy::new(
            manifest.clone(),
            user_config.clone(),
            ports.clone(),
        )),
        DeployMethod::NativePythonBundle => Box::new(python_bundle::PythonBundleDeploy::new(
            manifest.clone(),
            user_config.clone(),
            ports.clone(),
        )),
        DeployMethod::Docker => Box::new(docker::DockerDeploy::new(
            manifest.clone(),
            user_config.clone(),
            ports.clone(),
        )),
        DeployMethod::External => {
            // External engines are detected, not deployed; closing the audit row
            // immediately keeps the table consistent.
            mark_finished(
                &db,
                deployment_id,
                DeploymentStatus::Failed,
                Some("external method has no deploy step"),
            );
            return Err(DeployError::Manifest(
                "External deploy method is not handled by services::deploy::deploy".to_string(),
            ));
        }
    };

    // 3. PREPARE.
    let prepared = match strategy.prepare().await {
        Ok(p) => p,
        Err(e) => {
            mark_finished(
                &db,
                deployment_id,
                DeploymentStatus::Failed,
                Some(&e.to_string()),
            );
            return Err(e);
        }
    };

    // 4. COMMIT — single transaction over services_v2 + model_registry_v2 +
    //    deployments_v2.finish. Any failure triggers rollback of side effects.
    let commit_result: DeployResult<i64> = with_tx(&db, |tx| {
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
            mark_finished(&db, deployment_id, DeploymentStatus::Failed, Some(&rb_msg));
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
/// without touching `services_v2`. Used by the supervisor's restart loop —
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
            Box::new(embedded::EmbeddedDeploy::new(manifest, user_config))
        }
        DeployMethod::NativeBinary => Box::new(binary::BinaryDeploy::new(
            manifest,
            user_config,
            ports.clone(),
        )),
        DeployMethod::NativePythonBundle => Box::new(python_bundle::PythonBundleDeploy::new(
            manifest,
            user_config,
            ports.clone(),
        )),
        DeployMethod::Docker => Box::new(docker::DockerDeploy::new(
            manifest,
            user_config,
            ports.clone(),
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
        deploy_method: prepared.deploy_method,
        transport: prepared.transport,
        status,
        runtime_pid: prepared.runtime.pid,
        runtime_port: prepared.runtime.port,
        sidecar_quic_port: prepared.runtime.sidecar_port,
        endpoint_url: prepared.runtime.endpoint_url.clone(),
        config_json: prepared.config_json.clone(),
    }
}

/// Probes an HTTP URL until it returns 2xx or the deadline elapses.
pub(crate) async fn http_health_wait(url: &str, timeout_secs: u64) -> DeployResult<()> {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| DeployError::Other(format!("reqwest builder: {}", e)))?;
    loop {
        if Instant::now() >= deadline {
            return Err(DeployError::HealthTimeout(timeout_secs));
        }
        match client.get(url).send().await {
            Ok(resp) if resp.status().is_success() => return Ok(()),
            _ => tokio::time::sleep(Duration::from_millis(500)).await,
        }
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
            capabilities: format!("[\"{}\"]", manifest.engine.category_str()),
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
    fn category_str(&self) -> &'static str;
}

impl CategoryStr for crate::services::manifest::Engine {
    fn category_str(&self) -> &'static str {
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
    async fn deploy_returns_service_id_on_success_for_embedded() {
        let db = open_db();
        let ports = Arc::new(PortAllocator::new((45_900, 45_999), Default::default()).unwrap());
        let manifest = dummy_manifest("emb-ok", NativeRuntime::Embedded);
        let cfg = serde_json::json!({});
        let outcome = deploy(
            DeployMethod::NativeEmbedded,
            &manifest,
            &cfg,
            ports,
            db.clone(),
        )
        .await
        .expect("embedded deploy succeeds");

        assert!(outcome.endpoint.handle.id > 0);
        assert_eq!(outcome.endpoint.transport, Transport::Embedded);
        // model_registry row was created with the service_id linked
        let conn = db.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM model_registry_v2 WHERE service_id = ?1",
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
            ports.clone(),
            db.clone(),
        )
        .await
        .expect("seed deploy succeeds");

        let count_before: i64 = {
            let conn = db.lock().unwrap();
            conn.query_row("SELECT COUNT(*) FROM services_v2", [], |r| r.get(0))
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
            conn.query_row("SELECT COUNT(*) FROM services_v2", [], |r| r.get(0))
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
            ports,
            db.clone(),
        )
        .await;
        assert!(res.is_err(), "deploy should fail when binary path invalid");

        // deployments_v2 row exists with status=failed.
        let conn = db.lock().unwrap();
        let (status, err): (String, Option<String>) = conn
            .query_row(
                "SELECT status, error_text FROM deployments_v2 WHERE engine_id = 'bin-err' ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "failed");
        assert!(err.is_some());
    }
}
