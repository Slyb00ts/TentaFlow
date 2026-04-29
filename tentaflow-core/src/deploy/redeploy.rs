// =============================================================================
// File: deploy/redeploy.rs - Rebuild already-deployed service from refreshed
// source tree: stop the running instance, rebuild (docker/python/binary),
// start the replacement, refresh ServiceManager registration, persist the new
// source_hash on the `services` row. Logs stream through the same log_bus as
// fresh deploys so GUI can reuse DeploymentLogStreamRequest.
// =============================================================================

use std::sync::Arc;

use anyhow::{anyhow, Context};
use serde::Deserialize;
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::db::repository::deployments as deployments_repo;
use crate::db::{repository, DbPool};
use crate::deploy::log_bus::{self, fail, finish_success, log_line, phase, BusMessage};
use crate::routing::service_manager::ServiceManager;

/// Parsed subset of `services.config_json` needed to re-spawn a deployed
/// instance without re-running the wizard.
#[derive(Debug, Default, Deserialize)]
struct StoredDeployConfig {
    #[serde(default)]
    deploy_mode: Option<String>,
    #[serde(default)]
    image: Option<String>,
    #[serde(default)]
    container_name: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    on_demand: Option<bool>,
}

/// High-level outcome returned to the handler. Maps 1:1 to the
/// `REDEPLOY_STATUS_*` protocol constants.
#[derive(Debug, Clone)]
pub enum RedeployOutcome {
    /// Background task was started; logs stream via `deploy_id`.
    Started { deploy_id: String },
    /// Service exists but `force_if_active_sessions=false` and the engine has
    /// live meeting sessions. No work done.
    ActiveSessions { count: u32 },
    /// Manifest hash is empty — nothing to compare against / nothing to
    /// rebuild from (embedded runtime, external daemon).
    NoSource,
    /// Deploy mode not supported by redeploy (external, embedded, python-bundle,
    /// binary). User must redeploy manually.
    Unsupported { reason: String },
    /// `services.id` does not resolve.
    NotFound,
    /// Precondition failure before background task could start (bad config,
    /// unknown engine in manifest). Failures during the async rebuild surface
    /// through the log stream's `StreamEnd{final_status="failure"}`.
    Failed { error: String },
}

/// Synchronously validates the request and — if everything is in order —
/// starts a background task that drives the redeploy. Returns before the
/// actual rebuild begins so the caller can hand the `deploy_id` back to the
/// client and let it subscribe to the log stream.
pub async fn start_redeploy(
    db: DbPool,
    service_manager: Arc<ServiceManager>,
    service_id: i64,
    force_if_active_sessions: bool,
) -> RedeployOutcome {
    let service = match repository::get_service(&db, service_id) {
        Ok(Some(s)) => s,
        Ok(None) => return RedeployOutcome::NotFound,
        Err(e) => {
            return RedeployOutcome::Failed {
                error: format!("load service row: {}", e),
            }
        }
    };

    let engine_id = engine_id_for_service(&service);

    let registry = crate::services::manifest::registry();
    let manifest = match registry.by_id(&engine_id) {
        Some(m) => m,
        None => {
            return RedeployOutcome::Failed {
                error: format!("engine '{}' not present in manifest", engine_id),
            }
        }
    };

    let stored_config: StoredDeployConfig =
        serde_json::from_str(&service.config_json).unwrap_or_default();
    let deploy_mode = stored_config
        .deploy_mode
        .clone()
        .unwrap_or_else(|| infer_deploy_mode(&service));

    let new_hash = match deploy_mode.as_str() {
        "docker" => manifest.docker_source_hash.as_str(),
        "native" => manifest.native_source_hash.as_str(),
        "external" => {
            return RedeployOutcome::Unsupported {
                reason: "external deployments track an out-of-process daemon".to_string(),
            }
        }
        other => {
            return RedeployOutcome::Unsupported {
                reason: format!("unknown deploy_mode '{}'", other),
            }
        }
    };
    if new_hash.is_empty() {
        return RedeployOutcome::NoSource;
    }

    // Meeting-bot special case: the engine runs ephemeral containers per live
    // session, so we must either refuse when sessions are live or drain them
    // first. The backend never re-asks the user; the GUI must call this with
    // `force=true` after confirming.
    if engine_id == "teams-bot" {
        let count = count_active_meeting_sessions(&db).unwrap_or(0);
        if count > 0 && !force_if_active_sessions {
            return RedeployOutcome::ActiveSessions { count };
        }
    }

    // Docker is the only mode with a full redeploy pipeline. Native/embedded
    // bundles don't have a stop/start model wired through the runner yet, so
    // they surface as Unsupported rather than pretending to rebuild.
    if deploy_mode == "native" {
        let runtime = manifest
            .deploy
            .native
            .as_ref()
            .map(|n| format!("{:?}", n.runtime).to_lowercase().replace('_', "-"))
            .unwrap_or_default();
        return RedeployOutcome::Unsupported {
            reason: format!(
                "native runtime='{}' redeploy must be performed through a fresh deploy",
                runtime
            ),
        };
    }

    let deploy_id = format!("redeploy-{}-{}", service_id, log_bus::now_ms());
    let config_json = service.config_json.clone();
    let user_id: Option<i64> = None;
    if let Err(e) = deployments_repo::create(
        &db,
        &deploy_id,
        &engine_id,
        &deploy_mode,
        service.node_id.as_deref().unwrap_or(""),
        &config_json,
        user_id,
    ) {
        return RedeployOutcome::Failed {
            error: format!("record deployment row: {}", e),
        };
    }

    let start_ms = log_bus::now_ms();
    let tx = log_bus::sender_for(&deploy_id);

    let db_task = db.clone();
    let sm_task = service_manager.clone();
    let deploy_id_task = deploy_id.clone();
    let engine_id_task = engine_id.clone();
    let new_hash_task = new_hash.to_string();
    let service_name = service.name.clone();
    let stored_config_task = stored_config;
    let service_id_task = service_id;

    tokio::spawn(async move {
        let outcome = run_docker_redeploy(
            &db_task,
            &sm_task,
            &deploy_id_task,
            &tx,
            &engine_id_task,
            &service_name,
            service_id_task,
            &stored_config_task,
            &new_hash_task,
        )
        .await;

        match outcome {
            Ok((image_tag, container_name)) => {
                if let Err(e) =
                    repository::set_deployed_source_hash(&db_task, service_id_task, &new_hash_task)
                {
                    warn!("set_deployed_source_hash: {}", e);
                }
                finish_success(
                    &db_task,
                    &deploy_id_task,
                    &tx,
                    start_ms,
                    image_tag,
                    container_name,
                )
                .await;
            }
            Err(err) => {
                let msg = format!("{:#}", err);
                warn!(deploy_id = %deploy_id_task, error = %msg, "redeploy failed");
                fail(&db_task, &deploy_id_task, &tx, start_ms, &msg).await;
            }
        }
    });

    RedeployOutcome::Started { deploy_id }
}

/// Finds the manifest engine id that produced a given service row. Prefers
/// `config_json.manifest_engine_id`; falls back to the row's service name.
fn engine_id_for_service(service: &crate::db::models::DbService) -> String {
    #[derive(Deserialize, Default)]
    struct Cfg {
        #[serde(default)]
        manifest_engine_id: Option<String>,
    }
    let parsed: Cfg = serde_json::from_str(&service.config_json).unwrap_or_default();
    parsed
        .manifest_engine_id
        .unwrap_or_else(|| service.name.clone())
}

/// When config_json has no explicit `deploy_mode`, infer one from the
/// presence of an `image` field (docker legacy) vs. absence (native).
fn infer_deploy_mode(service: &crate::db::models::DbService) -> String {
    if service.config_json.contains("\"image\"") {
        "docker".to_string()
    } else {
        "native".to_string()
    }
}

fn count_active_meeting_sessions(db: &DbPool) -> anyhow::Result<u32> {
    let rows = repository::transcripts::list_sessions(db, None)?;
    let count = rows
        .iter()
        .filter(|s| {
            matches!(
                s.status.as_str(),
                "active" | "joining" | "leaving" | "paired"
            )
        })
        .count();
    Ok(count as u32)
}

#[cfg(feature = "docker")]
#[allow(clippy::too_many_arguments)]
async fn run_docker_redeploy(
    db: &DbPool,
    service_manager: &Arc<ServiceManager>,
    deploy_id: &str,
    tx: &broadcast::Sender<BusMessage>,
    engine_id: &str,
    service_name: &str,
    service_id: i64,
    stored_config: &StoredDeployConfig,
    new_hash: &str,
) -> anyhow::Result<(String, String)> {
    info!(
        deploy_id,
        engine_id, service_name, "starting docker redeploy"
    );

    let registry = crate::services::manifest::registry();
    let manifest = registry
        .by_id(engine_id)
        .ok_or_else(|| anyhow!("engine '{}' vanished from manifest mid-flight", engine_id))?;
    let docker = manifest
        .deploy
        .docker
        .as_ref()
        .ok_or_else(|| anyhow!("engine '{}' has no deploy.docker section", engine_id))?;
    let context_path = docker
        .context_path
        .clone()
        .ok_or_else(|| anyhow!("engine '{}' has no docker.context_path", engine_id))?;

    let image_tag = stored_config
        .image
        .clone()
        .unwrap_or_else(|| format!("tentaflow/{}:latest", engine_id));
    let _ = deployments_repo::set_image_tag(db, deploy_id, &image_tag);

    let container_name = stored_config
        .container_name
        .clone()
        .unwrap_or_else(|| format!("tentaflow-{}", engine_id));

    // ---- phase 1: rebuild image from refreshed source tree ----
    phase(db, deploy_id, tx, "building", 5, "build");
    log_line(
        db,
        deploy_id,
        tx,
        "log",
        &format!(
            "rebuild from {} (new_hash={})",
            context_path,
            short_hash(new_hash)
        ),
    );

    run_docker_build(db, deploy_id, tx, &context_path, &image_tag)
        .await
        .context("rebuild image")?;

    // ---- phase 2: stop the running container (if any) ----
    phase(db, deploy_id, tx, "building", 90, "stop_old");
    if let Err(e) = crate::deploy::docker::stop(&container_name).await {
        // Absent container is not fatal — we still want to run the new one.
        log_line(
            db,
            deploy_id,
            tx,
            "log",
            &format!("stop_old: {} ({})", container_name, e),
        );
    } else {
        log_line(
            db,
            deploy_id,
            tx,
            "log",
            &format!("stopped and removed container '{}'", container_name),
        );
    }

    // on-demand engines (teams-bot) have no persistent container to restart.
    // The bot container is spawned per meeting session by MeetingManager.
    if stored_config.on_demand.unwrap_or(false) {
        phase(db, deploy_id, tx, "registering", 95, "register");
        log_line(
            db,
            deploy_id,
            tx,
            "log",
            "engine is on-demand — image refreshed, no persistent container to restart",
        );
        let _ = deployments_repo::set_container_name(db, deploy_id, &container_name);
        let _ = service_id; // not used for on-demand
        return Ok((image_tag, container_name));
    }

    // ---- phase 3: start replacement container ----
    let default_port = manifest.engine.default_port;
    let host_port = stored_config.port.unwrap_or(default_port);
    phase(db, deploy_id, tx, "running", 93, "start_new");

    // Re-uzywamy klucza Ed25519 wygenerowanego przy pierwszym deployu (idempotencja).
    // Bez tego `EndpointId` zmienialby sie po kazdym redeployu i ServiceManager
    // musialby renegocjowac klienta.
    let sidecar = crate::deploy::runner::provision_docker_sidecar(
        service_name,
        engine_id,
        default_port,
        None,
    )
    .context("provision sidecar (key + config.toml) for redeploy")?;

    let req = crate::deploy::docker::DeployRequest {
        container: context_path.clone(),
        image_tag: Some(image_tag.clone()),
        instance_name: Some(container_name.clone()),
        // Sidecar wystawia QUIC na 5000/udp; wewnetrzny port silnika nie jest mapowany.
        ports: vec![(host_port.to_string(), "5000/udp".to_string())],
        volumes: vec![(sidecar.dir.display().to_string(), "/data".to_string())],
        env: std::collections::HashMap::new(),
        gpu: false,
    };
    let created = crate::deploy::docker::deploy(&req)
        .await
        .context("start new container")?;
    let _ = deployments_repo::set_container_name(db, deploy_id, &created);
    log_line(
        db,
        deploy_id,
        tx,
        "log",
        &format!("started replacement container '{}'", created),
    );

    // ---- phase 4: refresh service registration ----
    phase(db, deploy_id, tx, "registering", 97, "register");
    refresh_service_manager(
        service_manager,
        &manifest.engine.category,
        service_name,
        &sidecar.endpoint_id_hex,
        host_port,
    );
    log_line(db, deploy_id, tx, "log", "ServiceManager re-registered");

    Ok((image_tag, created))
}

#[cfg(not(feature = "docker"))]
#[allow(clippy::too_many_arguments)]
async fn run_docker_redeploy(
    _db: &DbPool,
    _sm: &Arc<ServiceManager>,
    _deploy_id: &str,
    _tx: &broadcast::Sender<BusMessage>,
    _engine_id: &str,
    _service_name: &str,
    _service_id: i64,
    _stored_config: &StoredDeployConfig,
    _new_hash: &str,
) -> anyhow::Result<(String, String)> {
    Err(anyhow!("feature 'docker' disabled at build time"))
}

fn short_hash(h: &str) -> String {
    h.chars().take(12).collect()
}

fn refresh_service_manager(
    service_manager: &Arc<ServiceManager>,
    category: &crate::services::manifest::Category,
    service_name: &str,
    endpoint_id_hex: &str,
    host_port: u16,
) {
    let category_str = format!("{:?}", category).to_lowercase();
    let service_type = match category_str.as_str() {
        "llm" => "llm",
        "stt" => "stt",
        "tts" => "tts",
        "embeddings" => "embedding",
        _ => return,
    };
    service_manager.remove_quic_service(service_name, service_type);
    crate::deploy::runner::register_docker_quic_service(
        service_manager,
        service_name,
        &category_str,
        endpoint_id_hex,
        host_port,
    );
}

// =============================================================================
// Docker build driver — same `docker build --progress=plain` streamer that
// runner.rs uses. Extracted to a helper because redeploy exercises the exact
// same flow (extract bundle → docker build → stream stdout/stderr → parse
// progress). Runner will call through here in a follow-up refactor.
// =============================================================================

#[cfg(feature = "docker")]
async fn run_docker_build(
    db: &DbPool,
    deploy_id: &str,
    tx: &broadcast::Sender<BusMessage>,
    context_path: &str,
    image_tag: &str,
) -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::Command;

    let workdir = tempfile::tempdir().context("tmpdir for build context")?;
    crate::deploy::extract_to(workdir.path()).context("extract container bundle")?;
    let dockerfile_rel = format!("tentaflow-containers/{}/Dockerfile", context_path);
    let dockerfile_abs = workdir.path().join(&dockerfile_rel);
    if !dockerfile_abs.exists() {
        return Err(anyhow!(
            "Dockerfile missing from bundle: {} (cwd={})",
            dockerfile_rel,
            workdir.path().display()
        ));
    }

    log_line(db, deploy_id, tx, "log", "docker build (BuildKit)...");

    let mut cmd = Command::new("docker");
    cmd.env("DOCKER_BUILDKIT", "1")
        .arg("build")
        .arg("--progress=plain")
        .arg("-t")
        .arg(image_tag)
        .arg("-f")
        .arg(&dockerfile_abs)
        .arg(workdir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .context("spawn `docker build` - is docker on PATH?")?;
    let stdout = child.stdout.take().expect("stdout");
    let stderr = child.stderr.take().expect("stderr");
    let mut stdout_lines = BufReader::new(stdout).lines();
    let mut stderr_lines = BufReader::new(stderr).lines();

    loop {
        tokio::select! {
            line = stdout_lines.next_line() => {
                match line {
                    Ok(Some(l)) => log_line(db, deploy_id, tx, "log", l.trim_end()),
                    Ok(None) => break,
                    Err(e) => {
                        warn!("stdout read: {}", e);
                        break;
                    }
                }
            }
            line = stderr_lines.next_line() => {
                match line {
                    Ok(Some(l)) => log_line(db, deploy_id, tx, "log", l.trim_end()),
                    Ok(None) => {}
                    Err(e) => warn!("stderr read: {}", e),
                }
            }
        }
    }
    while let Ok(Some(l)) = stderr_lines.next_line().await {
        log_line(db, deploy_id, tx, "log", l.trim_end());
    }

    let status = child.wait().await.context("docker build wait")?;
    if !status.success() {
        return Err(anyhow!("docker build exit code {:?}", status.code()));
    }
    Ok(())
}
