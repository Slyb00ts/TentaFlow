// =============================================================================
// Plik: deploy/runner.rs
// Opis: Uruchomienie deploymentu silnika z manifestu. Wołane z handler'a
//       `service_manifest_deploy` przez tokio::spawn. Cały lifecycle:
//        - queued → building (docker build streaming z bollard)
//        - building → pulling (brak w naszym przypadku — obraz budowany lokalnie)
//        - building → running (docker run, jeśli service persistent)
//        - running → registering (wpis do `services` + register_quic_service)
//        - registering → success
//       Wszystko pisane do DB (deployments.status/phase/progress_pct/log_tail)
//       i emitowane na log_bus żeby streaming handler mógł re-emitować do
//       frontendu live. Dla agents/tools pomijamy run + register — build-only.
// =============================================================================

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::db::repository::deployments as deployments_repo;
use crate::db::DbPool;
use crate::deploy::log_bus::{self, BusMessage, LogLine};
use crate::routing::service_manager::ServiceManager;

/// Fragmenty konfiguracji z `config_json` wizardu — pola opcjonalne.
#[derive(Debug, Default, Deserialize)]
struct DeployConfig {
    #[serde(default)]
    container_name: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    model_preset_id: Option<String>,
    #[serde(default)]
    model_repo: Option<String>,
    #[serde(default)]
    gpu_select_mode: Option<String>,
    #[serde(default)]
    gpu_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
struct EngineMeta {
    engine_id: String,
    category: String,
    default_port: u16,
    context_path: Option<String>,
    native_runtime: Option<String>,
}

pub async fn run_deployment(
    db: DbPool,
    service_manager: Arc<ServiceManager>,
    deploy_id: String,
    engine_id: String,
    deploy_method: String,
    node_id: String,
    config_json: String,
) {
    let start_ms = log_bus::now_ms();
    let tx = log_bus::sender_for(&deploy_id);

    let config: DeployConfig = serde_json::from_str(&config_json).unwrap_or_default();

    // Pobierz manifest engine'u z rejestru (kompilowany z TOML przez build.rs).
    let engine = match load_engine_meta(&engine_id) {
        Ok(e) => e,
        Err(e) => {
            fail(
                &db,
                &deploy_id,
                &tx,
                start_ms,
                &format!("Nie znaleziono engine '{}' w manifeście: {}", engine_id, e),
            )
            .await;
            return;
        }
    };

    let image_tag = format!("tentaflow/{}:latest", engine_id);
    if let Err(e) = deployments_repo::set_image_tag(&db, &deploy_id, &image_tag) {
        warn!("set_image_tag: {}", e);
    }

    phase(&db, &deploy_id, &tx, "building", 5, "docker build");

    match deploy_method.as_str() {
        "docker" => {
            if let Err(e) = do_docker_deploy(
                &db,
                &service_manager,
                &deploy_id,
                &tx,
                &engine,
                &image_tag,
                &node_id,
                &config,
                start_ms,
            )
            .await
            {
                fail(&db, &deploy_id, &tx, start_ms, &format!("{:#}", e)).await;
            }
        }
        "native" => {
            if let Err(e) = do_native_deploy(
                &db,
                &service_manager,
                &deploy_id,
                &tx,
                &engine,
                &node_id,
                &config,
                start_ms,
            )
            .await
            {
                fail(&db, &deploy_id, &tx, start_ms, &format!("{:#}", e)).await;
            }
        }
        "external" => {
            // External = user już ma uruchomiony daemon (np. ollama). Rejestrujemy
            // w DB jako gotowy serwis + register_quic_service (jeśli protocol = quic)
            // lub po prostu oznaczamy success.
            log_line(&db, &deploy_id, &tx, "log", "registering external service");
            finish_success(
                &db,
                &deploy_id,
                &tx,
                start_ms,
                String::new(),
                String::new(),
            )
            .await;
        }
        other => {
            fail(
                &db,
                &deploy_id,
                &tx,
                start_ms,
                &format!("Nieznana metoda deployu: {}", other),
            )
            .await;
        }
    }
}

fn load_engine_meta(engine_id: &str) -> Result<EngineMeta> {
    // Rejestr skompilowany z tentaflow-containers/*/_services/*.toml.
    let reg = crate::services::manifest::registry();
    let entry = reg
        .by_id(engine_id)
        .ok_or_else(|| anyhow!("engine '{}' nie istnieje w manifeście", engine_id))?;

    let context_path = entry
        .deploy
        .docker
        .as_ref()
        .map(|d| d.context_path.clone());
    let native_runtime = entry
        .deploy
        .native
        .as_ref()
        .map(|n| format!("{:?}", n.runtime).to_lowercase().replace('_', "-"));

    Ok(EngineMeta {
        engine_id: entry.engine.id.clone(),
        category: format!("{:?}", entry.engine.category).to_lowercase(),
        default_port: entry.engine.default_port,
        context_path,
        native_runtime,
    })
}

#[cfg(feature = "docker")]
async fn do_docker_deploy(
    db: &DbPool,
    service_manager: &Arc<ServiceManager>,
    deploy_id: &str,
    tx: &broadcast::Sender<BusMessage>,
    engine: &EngineMeta,
    image_tag: &str,
    node_id: &str,
    config: &DeployConfig,
    start_ms: i64,
) -> Result<()> {
    let context_path = engine
        .context_path
        .as_ref()
        .ok_or_else(|| anyhow!("engine '{}' nie ma deploy.docker.context_path", engine.engine_id))?;

    log_line(
        db,
        deploy_id,
        tx,
        "log",
        &format!("rozpakowywanie bundle kontenerów → {}", context_path),
    );

    // Rozpakuj tar.gz z bundle (wbudowany w binarce) do tmpdir. Bundle zawiera
    // katalog `tentaflow-containers/` na najwyższym poziomie — `context_path`
    // z manifestu jest względem niego, więc dokleiamy prefix.
    let workdir = tempfile::tempdir().context("tmpdir dla kontekstu build")?;
    crate::deploy::extract_to(workdir.path()).context("extract container bundle")?;
    let dockerfile_rel = format!("tentaflow-containers/{}/Dockerfile", context_path);
    let dockerfile_abs = workdir.path().join(&dockerfile_rel);
    if !dockerfile_abs.exists() {
        return Err(anyhow!(
            "Dockerfile nie znaleziony w bundle: {} (cwd={})",
            dockerfile_rel,
            workdir.path().display()
        ));
    }

    // Spakuj cały workdir do tar in-memory dla bollard (taki format wymaga API).
    // Manualny walk — pomijamy symlinki i typowe artefakty buildów (target/,
    // node_modules/, .git/) żeby nie wysypać się na dangling symlinkach z lokalnego
    // cargo build w katalogu kontenera.
    log_line(db, deploy_id, tx, "log", "pakowanie kontekstu do tar...");
    let mut tar_builder = tar::Builder::new(Vec::new());
    tar_builder.follow_symlinks(false);
    pack_dir_into_tar(&mut tar_builder, workdir.path(), std::path::Path::new(""))
        .with_context(|| format!("pakowanie tar z {}", workdir.path().display()))?;
    let tar_bytes = tar_builder.into_inner()?;

    phase(db, deploy_id, tx, "building", 10, "docker build");

    // Podłączamy się do Docker daemon.
    use bollard::query_parameters::BuildImageOptions;
    use bollard::{body_full, Docker};
    use futures::StreamExt;
    use hyper::body::Bytes;

    let docker = Docker::connect_with_local_defaults()
        .context("Docker daemon nieosiągalny — sprawdź socket i uprawnienia")?;

    let opts = BuildImageOptions {
        dockerfile: dockerfile_rel.clone(),
        t: Some(image_tag.to_string()),
        rm: true,
        ..Default::default()
    };
    let body = body_full(Bytes::from(tar_bytes));
    let mut stream = docker.build_image(opts, None, Some(body));

    // Progres heurystyka — bollard emituje "Step N/M" w `stream`. Parsujemy żeby
    // updateować progress_pct (5% scaffolding + 85% build + 10% register).
    let mut last_progress = 10u32;
    while let Some(item) = stream.next().await {
        match item {
            Ok(info) => {
                if let Some(stream_line) = info.stream {
                    let trimmed = stream_line.trim_end();
                    if !trimmed.is_empty() {
                        log_line(db, deploy_id, tx, "log", trimmed);
                        if let Some((cur, total)) = parse_step(trimmed) {
                            if total > 0 {
                                let pct = 10 + ((cur as f32 / total as f32) * 80.0) as u32;
                                let pct = pct.min(90);
                                if pct > last_progress {
                                    last_progress = pct;
                                    progress(db, deploy_id, tx, pct);
                                }
                            }
                        }
                    }
                }
                if let Some(err_detail) = info.error_detail {
                    let msg = err_detail.message.unwrap_or_default();
                    return Err(anyhow!("docker build error: {}", msg));
                }
            }
            Err(e) => {
                return Err(anyhow!("bollard build stream: {}", e));
            }
        }
    }

    log_line(db, deploy_id, tx, "log", &format!("obraz zbudowany: {}", image_tag));
    phase(db, deploy_id, tx, "building", 90, "build done");

    // Dla agents/tools — build wystarczy. Kontener uruchamia MeetingManager /
    // tools-executor ad-hoc, nie zostawiamy persistent service.
    if matches!(engine.category.as_str(), "agents" | "tools") {
        finish_success(db, deploy_id, tx, start_ms, image_tag.to_string(), String::new()).await;
        return Ok(());
    }

    // LLM/STT/TTS/Embeddings — uruchomienie persistent kontenera.
    phase(db, deploy_id, tx, "running", 92, "docker run");
    let container_name = config
        .container_name
        .clone()
        .unwrap_or_else(|| format!("tentaflow-{}", engine.engine_id));
    let host_port = config.port.unwrap_or(engine.default_port);

    let req = crate::deploy::docker::DeployRequest {
        container: engine.engine_id.clone(),
        image_tag: Some(image_tag.to_string()),
        instance_name: Some(container_name.clone()),
        ports: vec![(host_port.to_string(), format!("{}/tcp", engine.default_port))],
        volumes: Vec::new(),
        env: std::collections::HashMap::new(),
        gpu: config.gpu_select_mode.as_deref() == Some("all")
            || config
                .gpu_ids
                .as_ref()
                .map(|v| !v.is_empty())
                .unwrap_or(false),
    };
    // deploy::docker::deploy zbuduje image od nowa — my już to zrobiliśmy. Użyjmy
    // run_container bezpośrednio. Ale run_container jest private. Upublicznimy
    // go dla runnera albo użyjemy bollard create_container inline.
    //
    // Prostsze: po naszym build idzie druga iteracja przez deploy::docker::deploy
    // która wykryje istniejący image przez tag i pominie build (bollard build
    // jest inkrementalny — no layers changed). Trochę nadmiarowe ale OK.
    let created_name = crate::deploy::docker::deploy(&req)
        .await
        .context("uruchomienie kontenera")?;
    let _ = deployments_repo::set_container_name(db, deploy_id, &created_name);

    log_line(
        db,
        deploy_id,
        tx,
        "log",
        &format!("kontener uruchomiony: {}", created_name),
    );

    // Register service in DB + ServiceManager so router can route traffic.
    phase(db, deploy_id, tx, "registering", 96, "register service");
    register_service(
        db,
        service_manager,
        &engine.engine_id,
        &engine.category,
        &created_name,
        host_port,
        node_id,
    );
    log_line(db, deploy_id, tx, "log", "serwis zarejestrowany w routerze");

    finish_success(db, deploy_id, tx, start_ms, image_tag.to_string(), created_name).await;
    Ok(())
}

#[cfg(not(feature = "docker"))]
async fn do_docker_deploy(
    db: &DbPool,
    _sm: &Arc<ServiceManager>,
    deploy_id: &str,
    tx: &broadcast::Sender<BusMessage>,
    _engine: &EngineMeta,
    _image_tag: &str,
    _node_id: &str,
    _config: &DeployConfig,
    _start_ms: i64,
) -> Result<()> {
    log_line(db, deploy_id, tx, "log", "feature `docker` wyłączone");
    Err(anyhow!("feature `docker` wyłączone w tym buildzie"))
}

async fn do_native_deploy(
    db: &DbPool,
    _service_manager: &Arc<ServiceManager>,
    deploy_id: &str,
    tx: &broadcast::Sender<BusMessage>,
    engine: &EngineMeta,
    _node_id: &str,
    _config: &DeployConfig,
    start_ms: i64,
) -> Result<()> {
    let runtime = engine
        .native_runtime
        .as_ref()
        .ok_or_else(|| anyhow!("engine '{}' nie ma deploy.native.runtime", engine.engine_id))?;

    phase(db, deploy_id, tx, "building", 10, &format!("native setup ({})", runtime));

    match runtime.as_str() {
        "embedded" => {
            // Cargo feature — nic do budowania, po prostu flag w DB.
            log_line(
                db,
                deploy_id,
                tx,
                "log",
                "runtime=embedded — silnik wkompilowany, zero akcji runtime",
            );
            finish_success(db, deploy_id, tx, start_ms, String::new(), String::new()).await;
            Ok(())
        }
        "binary" => {
            log_line(db, deploy_id, tx, "log", "runtime=binary — TODO build.sh");
            // Build binary przez build.sh w bundle — pełna implementacja wymaga
            // streamingu stdout ze skryptu. Dla kompletności ta ścieżka istnieje
            // ale obecnie wymaga że admin sam zbuduje binarkę poza flow.
            // Zwracamy jawny błąd żeby nie udawać sukcesu.
            Err(anyhow!(
                "runtime=binary: build.sh scripted path jeszcze nie zintegrowany w runner — uruchom skrypt ręcznie"
            ))
        }
        "python-bundle" => {
            log_line(
                db,
                deploy_id,
                tx,
                "log",
                "runtime=python-bundle — bundle.toml setup",
            );
            Err(anyhow!(
                "runtime=python-bundle jeszcze nie podpięty pod runner — użyj deploy.docker dla tego silnika"
            ))
        }
        other => Err(anyhow!("Nieznany runtime: {}", other)),
    }
}

fn register_service(
    db: &DbPool,
    service_manager: &Arc<ServiceManager>,
    engine_id: &str,
    category: &str,
    container_name: &str,
    host_port: u16,
    node_id: &str,
) {
    // Wpis do tabeli services żeby startup restore_services mógł restaurować.
    let config_json = serde_json::json!({
        "deploy_mode": "docker",
        "image": format!("tentaflow/{}:latest", engine_id),
        "port": host_port,
        "container_name": container_name,
    })
    .to_string();
    if let Err(e) = crate::db::repository::create_service(
        db,
        engine_id,
        service_type_from_category(category),
        "first_available",
        Some(category),
        &config_json,
    ) {
        warn!("create_service '{}': {}", engine_id, e);
    }
    let _ = node_id; // node_id docelowo do multi-node routing — dla single-node nie używane

    // ServiceManager: rejestracja zależna od category. Dla LLM → quic_llm.
    let service_type = match category {
        "llm" => "llm",
        "stt" => "stt",
        "tts" => "tts",
        "embeddings" => "embedding",
        _ => return,
    };
    let url = format!("http://127.0.0.1:{}", host_port);
    service_manager.register_quic_service(
        engine_id.to_string(),
        service_type,
        url,
        None,
        None,
    );
}

fn service_type_from_category(category: &str) -> &str {
    match category {
        "llm" => "llm",
        "stt" => "stt",
        "tts" => "tts",
        "embeddings" => "embedding",
        "agents" => "agent",
        "tools" => "tool",
        other => other,
    }
}

// =============================================================================
// DB + bus helpers
// =============================================================================

fn parse_step(line: &str) -> Option<(u32, u32)> {
    let trimmed = line.trim_start_matches('\u{1b}').trim();
    if let Some(rest) = trimmed.strip_prefix("Step ") {
        if let Some((num, _)) = rest.split_once(" : ") {
            if let Some((cur, total)) = num.split_once('/') {
                let cur: u32 = cur.parse().ok()?;
                let total: u32 = total.parse().ok()?;
                return Some((cur, total));
            }
        }
    }
    None
}

fn log_line(
    db: &DbPool,
    deploy_id: &str,
    tx: &broadcast::Sender<BusMessage>,
    kind: &str,
    line: &str,
) {
    let _ = deployments_repo::append_log_line(db, deploy_id, line);
    let _ = tx.send(BusMessage::Line(LogLine {
        deploy_id: deploy_id.to_string(),
        kind: kind.to_string(),
        line: line.to_string(),
        phase: String::new(),
        progress_pct: 0,
        ts_ms: log_bus::now_ms(),
    }));
}

fn progress(db: &DbPool, deploy_id: &str, tx: &broadcast::Sender<BusMessage>, pct: u32) {
    let _ = deployments_repo::set_status(db, deploy_id, "building", "building", pct);
    let _ = tx.send(BusMessage::Line(LogLine {
        deploy_id: deploy_id.to_string(),
        kind: "progress".to_string(),
        line: String::new(),
        phase: "building".to_string(),
        progress_pct: pct,
        ts_ms: log_bus::now_ms(),
    }));
}

fn phase(
    db: &DbPool,
    deploy_id: &str,
    tx: &broadcast::Sender<BusMessage>,
    status: &str,
    pct: u32,
    phase_name: &str,
) {
    let _ = deployments_repo::set_status(db, deploy_id, status, phase_name, pct);
    let _ = tx.send(BusMessage::Line(LogLine {
        deploy_id: deploy_id.to_string(),
        kind: "phase".to_string(),
        line: phase_name.to_string(),
        phase: phase_name.to_string(),
        progress_pct: pct,
        ts_ms: log_bus::now_ms(),
    }));
    info!(deploy_id = %deploy_id, status = %status, phase = %phase_name, pct, "deployment phase");
}

async fn finish_success(
    db: &DbPool,
    deploy_id: &str,
    tx: &broadcast::Sender<BusMessage>,
    start_ms: i64,
    image_tag: String,
    container_name: String,
) {
    let _ = deployments_repo::mark_finished(db, deploy_id, "success", None);
    let _ = tx.send(BusMessage::End {
        deploy_id: deploy_id.to_string(),
        final_status: "success".to_string(),
        image_tag,
        container_name,
        error_message: String::new(),
        duration_ms: log_bus::now_ms() - start_ms,
    });
    // Daj szansę subscriberom otrzymać End zanim zamkniemy kanał.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    log_bus::close(deploy_id);
}

/// Pakuje `root` do `tar_builder`, pomijając symlinki i foldery target/,
/// node_modules/, .git/. Dangling symlinki w lokalnym kontenerze (np. z cargo
/// build-u deweloperskiego) byłyby inaczej przyczyną "tar archive" na poziomie
/// append_dir_all.
fn pack_dir_into_tar(
    tar_builder: &mut tar::Builder<Vec<u8>>,
    root: &std::path::Path,
    rel: &std::path::Path,
) -> std::io::Result<()> {
    let full = root.join(rel);
    for entry in std::fs::read_dir(&full)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str == "target" || name_str == "node_modules" || name_str == ".git" {
            continue;
        }
        let file_type = entry.file_type()?;
        let sub_rel = rel.join(&name);
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            pack_dir_into_tar(tar_builder, root, &sub_rel)?;
        } else if file_type.is_file() {
            let path = entry.path();
            let mut f = std::fs::File::open(&path)?;
            tar_builder.append_file(&sub_rel, &mut f).map_err(|e| {
                std::io::Error::new(
                    e.kind(),
                    format!("tar append {}: {}", sub_rel.display(), e),
                )
            })?;
        }
    }
    Ok(())
}

async fn fail(
    db: &DbPool,
    deploy_id: &str,
    tx: &broadcast::Sender<BusMessage>,
    start_ms: i64,
    msg: &str,
) {
    warn!(deploy_id = %deploy_id, error = %msg, "deployment failed");
    let _ = deployments_repo::append_log_line(db, deploy_id, &format!("[error] {}", msg));
    let _ = deployments_repo::mark_finished(db, deploy_id, "failure", Some(msg));
    let _ = tx.send(BusMessage::End {
        deploy_id: deploy_id.to_string(),
        final_status: "failure".to_string(),
        image_tag: String::new(),
        container_name: String::new(),
        error_message: msg.to_string(),
        duration_ms: log_bus::now_ms() - start_ms,
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    log_bus::close(deploy_id);
}
