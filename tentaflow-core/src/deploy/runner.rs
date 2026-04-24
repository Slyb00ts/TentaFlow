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
use tokio::sync::{broadcast, mpsc};
use tracing::{info, warn};

use crate::db::repository::deployments as deployments_repo;
use crate::db::DbPool;
use crate::deploy::log_bus::{self, BusMessage, LogLine};
use crate::routing::service_manager::ServiceManager;
use crate::services::manifest::ModelPreset;

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
    compose_path: Option<String>,
    native_runtime: Option<String>,
    model_presets: Vec<ModelPreset>,
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
            finish_success(&db, &deploy_id, &tx, start_ms, String::new(), String::new()).await;
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

    let context_path = entry.deploy.docker.as_ref().map(|d| d.context_path.clone());
    let compose_path = entry.deploy.docker.as_ref().map(|d| d.compose_path.clone()).flatten();
    let native_runtime = entry
        .deploy
        .native
        .as_ref()
        .map(|n| format!("{:?}", n.runtime).to_lowercase().replace('_', "-"));

    Ok(EngineMeta {
        engine_id: entry.engine.id.clone(),
        category: format!("{:?}", entry.engine.category).to_lowercase(),
        default_port: entry.engine.default_port,
        context_path: context_path.flatten(),
        compose_path,
        native_runtime,
        model_presets: entry.model_presets.clone(),
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
    if engine.compose_path.is_some() {
        return do_docker_compose_deploy(
            db,
            deploy_id,
            tx,
            engine,
            config,
            start_ms,
        )
        .await;
    }

    let context_path = engine.context_path.as_ref().ok_or_else(|| {
        anyhow!(
            "engine '{}' nie ma deploy.docker.context_path",
            engine.engine_id
        )
    })?;

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

    phase(db, deploy_id, tx, "building", 10, "docker build");

    // UWAGA: bollard domyślnie używa legacy build API (/build v1), który NIE
    // wspiera `--mount=type=cache` w Dockerfile (wymaga BuildKit). Większość
    // naszych Dockerfile'ów polega na cache mount dla /cargo/registry, /target
    // itd. (pierwsza budowa ~3-5 min, następne ~30s zamiast ~3min).
    //
    // Zamiast wdrażać bollard feature=buildkit (wymaga gRPC session + dodatkowej
    // biblioteki), wywołujemy systemowe `docker build` z DOCKER_BUILDKIT=1 env —
    // to ta sama komenda którą user odpalilby ręcznie. Streaming stdout linia-po-
    // linii do log_bus + parsing `Step N/M` (legacy) i `#N [step]` (BuildKit).
    log_line(db, deploy_id, tx, "log", "uruchamiam `docker build` (BuildKit)...");
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::Command;

    let mut cmd = Command::new("docker");
    cmd.env("DOCKER_BUILDKIT", "1")
        .arg("build")
        .arg("--progress=plain")
        .arg("-t")
        .arg(image_tag)
        .arg("-f")
        .arg(workdir.path().join(&dockerfile_rel))
        .arg(workdir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .context("nie mozna uruchomic `docker build` — sprawdź czy docker jest w PATH")?;
    let stdout = child.stdout.take().expect("stdout captured");
    let stderr = child.stderr.take().expect("stderr captured");
    let mut stdout_lines = BufReader::new(stdout).lines();
    let mut stderr_lines = BufReader::new(stderr).lines();

    let mut last_progress = 10u32;
    let mut max_step_seen = 0u32;
    let mut total_steps: Option<u32> = None;

    loop {
        tokio::select! {
            line = stdout_lines.next_line() => {
                match line {
                    Ok(Some(l)) => {
                        log_line(db, deploy_id, tx, "log", l.trim_end());
                        if let Some(pct) = parse_progress_line(&l, &mut max_step_seen, &mut total_steps) {
                            if pct > last_progress {
                                last_progress = pct;
                                progress(db, deploy_id, tx, pct);
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        warn!("stdout read: {}", e);
                        break;
                    }
                }
            }
            line = stderr_lines.next_line() => {
                // docker build --progress=plain pisze wiekszosc output na stderr.
                match line {
                    Ok(Some(l)) => {
                        log_line(db, deploy_id, tx, "log", l.trim_end());
                        if let Some(pct) = parse_progress_line(&l, &mut max_step_seen, &mut total_steps) {
                            if pct > last_progress {
                                last_progress = pct;
                                progress(db, deploy_id, tx, pct);
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        warn!("stderr read: {}", e);
                    }
                }
            }
        }
    }

    // Dodrenuj stderr jeśli cokolwiek zostało.
    while let Ok(Some(l)) = stderr_lines.next_line().await {
        log_line(db, deploy_id, tx, "log", l.trim_end());
    }

    let status = child
        .wait()
        .await
        .context("docker build wait")?;
    if !status.success() {
        return Err(anyhow!(
            "docker build zwrocil exit code {:?}",
            status.code()
        ));
    }

    log_line(
        db,
        deploy_id,
        tx,
        "log",
        &format!("obraz zbudowany: {}", image_tag),
    );
    phase(db, deploy_id, tx, "building", 90, "build done");

    // Dla agents/tools — build wystarczy. Kontener uruchamia MeetingManager /
    // tools-executor ad-hoc, nie zostawiamy persistent service. Mimo to rejestru­
    // jemy wpis w services (status=on_demand) zeby zakladka Services pokazywala
    // ze silnik jest zainstalowany i gotowy na spawn per-zadanie.
    if matches!(engine.category.as_str(), "agents" | "tools") {
        if engine.engine_id == "teams-bot" {
            if let Err(e) = crate::services::teams_bot_bootstrap::ensure_teams_bot_defaults(db).await {
                warn!("ensure_teams_bot_defaults nie powiodło się: {}", e);
            } else {
                info!("domyślne aliasy i flow dla teams-bota zainicjalizowane");
            }
        }
        register_on_demand_service(db, &engine.engine_id, &engine.category, &image_tag);
        persist_source_hash(db, &engine.engine_id, "docker", &engine.engine_id);
        log_line(db, deploy_id, tx, "log", "serwis zarejestrowany (on_demand)");
        finish_success(
            db,
            deploy_id,
            tx,
            start_ms,
            image_tag.to_string(),
            String::new(),
        )
        .await;
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
        ports: vec![(
            host_port.to_string(),
            format!("{}/tcp", engine.default_port),
        )],
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
    persist_source_hash(db, &engine.engine_id, "docker", &engine.engine_id);
    log_line(db, deploy_id, tx, "log", "serwis zarejestrowany w routerze");

    finish_success(
        db,
        deploy_id,
        tx,
        start_ms,
        image_tag.to_string(),
        created_name,
    )
    .await;
    Ok(())
}

#[cfg(feature = "docker")]
async fn do_docker_compose_deploy(
    db: &DbPool,
    deploy_id: &str,
    tx: &broadcast::Sender<BusMessage>,
    engine: &EngineMeta,
    config: &DeployConfig,
    start_ms: i64,
) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::Command;

    let compose_path = engine.compose_path.as_ref().ok_or_else(|| {
        anyhow!(
            "engine '{}' does not define deploy.docker.compose_path",
            engine.engine_id
        )
    })?;

    log_line(
        db,
        deploy_id,
        tx,
        "log",
        &format!("extracting container bundle → {}", compose_path),
    );

    let workdir = tempfile::tempdir().context("tmpdir for compose deploy")?;
    crate::deploy::extract_to(workdir.path()).context("extract container bundle")?;

    let compose_rel = format!("tentaflow-containers/{}", compose_path);
    let compose_abs = workdir.path().join(&compose_rel);
    if !compose_abs.exists() {
        return Err(anyhow!(
            "Compose file not found in bundle: {} (cwd={})",
            compose_rel,
            workdir.path().display()
        ));
    }

    let project_name = config
        .container_name
        .as_deref()
        .map(slugify_name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("tentaflow-{}", slugify_name(&engine.engine_id)));

    let _ = deployments_repo::set_container_name(db, deploy_id, &project_name);

    phase(db, deploy_id, tx, "building", 10, "docker compose build/up");
    log_line(
        db,
        deploy_id,
        tx,
        "log",
        &format!("running `docker compose` project '{}'", project_name),
    );

    let mut cmd = Command::new("docker");
    cmd.arg("compose")
        .arg("-p")
        .arg(&project_name)
        .arg("-f")
        .arg(&compose_abs)
        .arg("up")
        .arg("-d")
        .arg("--build")
        .current_dir(
            compose_abs
                .parent()
                .ok_or_else(|| anyhow!("compose file has no parent directory"))?,
        )
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .context("failed to start `docker compose` - check whether docker is in PATH")?;
    let stdout = child.stdout.take().expect("stdout captured");
    let stderr = child.stderr.take().expect("stderr captured");
    let mut stdout_lines = BufReader::new(stdout).lines();
    let mut stderr_lines = BufReader::new(stderr).lines();

    let mut last_progress = 10u32;
    let mut max_step_seen = 0u32;
    let mut total_steps: Option<u32> = None;

    loop {
        tokio::select! {
            line = stdout_lines.next_line() => {
                match line {
                    Ok(Some(l)) => {
                        log_line(db, deploy_id, tx, "log", l.trim_end());
                        if let Some(pct) = parse_progress_line(&l, &mut max_step_seen, &mut total_steps) {
                            if pct > last_progress {
                                last_progress = pct;
                                progress(db, deploy_id, tx, pct);
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        warn!("stdout read: {}", e);
                        break;
                    }
                }
            }
            line = stderr_lines.next_line() => {
                match line {
                    Ok(Some(l)) => {
                        log_line(db, deploy_id, tx, "log", l.trim_end());
                        if let Some(pct) = parse_progress_line(&l, &mut max_step_seen, &mut total_steps) {
                            if pct > last_progress {
                                last_progress = pct;
                                progress(db, deploy_id, tx, pct);
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        warn!("stderr read: {}", e);
                    }
                }
            }
        }
    }

    while let Ok(Some(l)) = stderr_lines.next_line().await {
        log_line(db, deploy_id, tx, "log", l.trim_end());
    }

    let status = child.wait().await.context("docker compose wait")?;
    if !status.success() {
        return Err(anyhow!(
            "docker compose returned exit code {:?}",
            status.code()
        ));
    }

    phase(db, deploy_id, tx, "running", 96, "compose stack deployed");
    log_line(
        db,
        deploy_id,
        tx,
        "log",
        &format!("compose stack deployed: {}", project_name),
    );

    finish_success(
        db,
        deploy_id,
        tx,
        start_ms,
        String::new(),
        project_name,
    )
    .await;
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
    service_manager: &Arc<ServiceManager>,
    deploy_id: &str,
    tx: &broadcast::Sender<BusMessage>,
    engine: &EngineMeta,
    node_id: &str,
    config: &DeployConfig,
    start_ms: i64,
) -> Result<()> {
    let runtime = engine
        .native_runtime
        .as_ref()
        .ok_or_else(|| anyhow!("engine '{}' nie ma deploy.native.runtime", engine.engine_id))?;

    phase(
        db,
        deploy_id,
        tx,
        "building",
        10,
        &format!("native setup ({})", runtime),
    );

    match runtime.as_str() {
        "embedded" => {
            do_embedded_native_deploy(
                db,
                service_manager,
                deploy_id,
                tx,
                engine,
                node_id,
                config,
                start_ms,
            )
            .await
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

async fn do_embedded_native_deploy(
    db: &DbPool,
    service_manager: &Arc<ServiceManager>,
    deploy_id: &str,
    tx: &broadcast::Sender<BusMessage>,
    engine: &EngineMeta,
    node_id: &str,
    config: &DeployConfig,
    start_ms: i64,
) -> Result<()> {
    match (engine.category.as_str(), engine.engine_id.as_str()) {
        ("llm", "llama-cpp") | ("llm", "mlx") => {
            let model_repo = resolve_model_repo(engine, config)?;
            let service_name = native_service_name(engine, config, &model_repo);
            let host_port = config.port.unwrap_or(engine.default_port);

            phase(db, deploy_id, tx, "building", 20, "download model");
            let model_path = ensure_llm_model(db, deploy_id, tx, engine, &model_repo).await?;

            phase(db, deploy_id, tx, "running", 75, "load model");
            let preferred_backend = runtime_backend_id(&engine.engine_id);
            let shared = crate::inference::shared_inference_manager();
            let model_info = {
                let mut mgr = shared.write().await;
                mgr.load_model(&model_path, None, Some(preferred_backend))
                    .await
            }
            .with_context(|| {
                format!(
                    "load embedded model '{}' via {}",
                    model_repo, preferred_backend
                )
            })?;

            phase(db, deploy_id, tx, "registering", 92, "register service");
            let config_json = serde_json::json!({
                "deploy_mode": "native",
                "engine": preferred_backend,
                "manifest_engine_id": engine.engine_id,
                "deployed_model": model_repo,
                "model_path": model_info.path,
                "service_type": "llm",
                "port": host_port,
            })
            .to_string();

            upsert_native_service(
                db,
                node_id,
                &service_name,
                "llm",
                Some("llm"),
                &config_json,
                "first_available",
            )?;

            service_manager.register_model_mapping(&model_repo, &service_name);
            service_manager.register_local_inference_model(&model_repo);
            service_manager.register_local_inference_model(&service_name);

            persist_source_hash(db, &engine.engine_id, "native", &service_name);

            log_line(
                db,
                deploy_id,
                tx,
                "log",
                &format!("natywny serwis zarejestrowany: {}", service_name),
            );
            finish_success(db, deploy_id, tx, start_ms, String::new(), service_name).await;
            Ok(())
        }
        ("stt", "whisper") => {
            let service_name = config
                .container_name
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "whisper-stt-native".to_string());

            phase(db, deploy_id, tx, "running", 70, "load whisper");
            let shared = crate::stt::shared_stt_manager();
            let stt_info = {
                let mut mgr = shared.write().await;
                mgr.ensure_and_load(None).await
            }
            .context("load whisper model")?;

            phase(db, deploy_id, tx, "registering", 92, "register service");
            let config_json = serde_json::json!({
                "deploy_mode": "native",
                "engine": "whisper",
                "manifest_engine_id": engine.engine_id,
                "deployed_model": "whisper-large-v3-turbo",
                "model_path": stt_info.path,
                "service_type": "stt",
            })
            .to_string();

            upsert_native_service(
                db,
                node_id,
                &service_name,
                "stt",
                Some("stt"),
                &config_json,
                "single",
            )?;

            persist_source_hash(db, &engine.engine_id, "native", &service_name);

            log_line(
                db,
                deploy_id,
                tx,
                "log",
                &format!("natywny serwis zarejestrowany: {}", service_name),
            );
            finish_success(db, deploy_id, tx, start_ms, String::new(), service_name).await;
            Ok(())
        }
        _ => Err(anyhow!(
            "runtime=embedded dla '{}' nie ma jeszcze zintegrowanego flow deploymentu",
            engine.engine_id
        )),
    }
}

async fn ensure_llm_model(
    db: &DbPool,
    deploy_id: &str,
    tx: &broadcast::Sender<BusMessage>,
    engine: &EngineMeta,
    model_repo: &str,
) -> Result<std::path::PathBuf> {
    let store = crate::hub::model_store::ModelStore::default_for_platform();
    let model_dir = store.model_dir(model_repo);

    if !store.is_downloaded(model_repo, "") {
        log_line(
            db,
            deploy_id,
            tx,
            "log",
            &format!("pobieranie modelu {}", model_repo),
        );
        let (progress_tx, mut progress_rx) =
            mpsc::channel::<crate::hub::model_store::DownloadProgress>(32);
        let db_clone = db.clone();
        let deploy_id_owned = deploy_id.to_string();
        let tx_clone = tx.clone();
        let progress_forward = tokio::spawn(async move {
            while let Some(p) = progress_rx.recv().await {
                log_line(
                    &db_clone,
                    &deploy_id_owned,
                    &tx_clone,
                    "log",
                    &format!(
                        "{}: {:.1}% ({}/{})",
                        p.file_name, p.percent, p.bytes_downloaded, p.bytes_total
                    ),
                );
            }
        });
        store
            .download_model(model_repo, None, progress_tx)
            .await
            .map_err(|e| anyhow!("download model '{}': {}", model_repo, e))?;
        let _ = progress_forward.await;
    } else {
        log_line(
            db,
            deploy_id,
            tx,
            "log",
            &format!("model juz jest w cache: {}", model_repo),
        );
    }

    match engine.engine_id.as_str() {
        "llama-cpp" => find_gguf_file(&model_dir).ok_or_else(|| {
            anyhow!(
                "model '{}' pobrany, ale nie znaleziono pliku .gguf w {}",
                model_repo,
                model_dir.display()
            )
        }),
        "mlx" => Ok(model_dir),
        other => Err(anyhow!(
            "nieobslugiwany embedded LLM '{}' dla pobierania modelu",
            other
        )),
    }
}

fn resolve_model_repo(engine: &EngineMeta, config: &DeployConfig) -> Result<String> {
    if let Some(repo) = config
        .model_repo
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Ok(repo.to_string());
    }

    if let Some(preset_id) = config
        .model_preset_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if let Some(preset) = engine.model_presets.iter().find(|p| p.id == preset_id) {
            return Ok(preset.repo.clone());
        }
        return Err(anyhow!(
            "preset '{}' nie istnieje dla silnika '{}'",
            preset_id,
            engine.engine_id
        ));
    }

    engine
        .model_presets
        .iter()
        .find(|p| p.recommended)
        .or_else(|| engine.model_presets.first())
        .map(|p| p.repo.clone())
        .ok_or_else(|| anyhow!("silnik '{}' nie ma zadnego model_preset", engine.engine_id))
}

fn native_service_name(engine: &EngineMeta, config: &DeployConfig, model_repo: &str) -> String {
    if let Some(name) = config
        .container_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return name.to_string();
    }

    let engine_slug = slugify_name(&engine.engine_id);
    let model_slug = slugify_name(model_repo);
    format!("{}-native-{}", engine_slug, model_slug)
}

fn slugify_name(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut last_dash = false;
    for ch in value.chars() {
        let next = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else if matches!(ch, '-' | '_' | '/' | '.' | ' ') {
            Some('-')
        } else {
            None
        };
        let Some(next) = next else {
            continue;
        };
        if next == '-' {
            if last_dash || out.is_empty() {
                continue;
            }
            last_dash = true;
            out.push('-');
        } else {
            last_dash = false;
            out.push(next);
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "service".to_string()
    } else {
        out
    }
}

fn runtime_backend_id(engine_id: &str) -> &str {
    match engine_id {
        "llama-cpp" => "llamacpp",
        other => other,
    }
}

fn upsert_native_service(
    db: &DbPool,
    node_id: &str,
    service_name: &str,
    service_type: &str,
    model_category: Option<&str>,
    config_json: &str,
    strategy: &str,
) -> Result<()> {
    let existing = crate::db::repository::list_services(db)?
        .into_iter()
        .find(|svc| svc.name == service_name);

    let row_id = if let Some(existing) = existing {
        crate::db::repository::update_service(
            db,
            existing.id,
            service_name,
            service_type,
            strategy,
            model_category,
            "running",
            config_json,
        )?;
        existing.id
    } else {
        let id = crate::db::repository::create_service(
            db,
            service_name,
            service_type,
            strategy,
            model_category,
            config_json,
        )?;
        crate::db::repository::update_service(
            db,
            id,
            service_name,
            service_type,
            strategy,
            model_category,
            "running",
            config_json,
        )?;
        id
    };

    if !node_id.is_empty() {
        crate::db::repository::set_service_node_id(db, row_id, Some(node_id))?;
    }

    Ok(())
}

fn find_gguf_file(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "gguf") {
            return Some(path);
        }
    }
    None
}

/// Stores the source-tree hash of the just-deployed engine against the row
/// in `services` identified by `service_name`. Failures are warned and
/// swallowed — the deployment itself already succeeded and this bookkeeping
/// must not fail the caller.
fn persist_source_hash(db: &DbPool, engine_id: &str, deploy_method: &str, service_name: &str) {
    let registry = crate::services::manifest::registry();
    let Some(manifest) = registry.by_id(engine_id) else {
        return;
    };
    let hash = match deploy_method {
        "docker" => manifest.docker_source_hash.as_str(),
        "native" => manifest.native_source_hash.as_str(),
        _ => return,
    };
    if hash.is_empty() {
        return;
    }
    let services = match crate::db::repository::list_services(db) {
        Ok(s) => s,
        Err(e) => {
            warn!("persist_source_hash: list_services: {}", e);
            return;
        }
    };
    let Some(row) = services.into_iter().find(|s| s.name == service_name) else {
        return;
    };
    if let Err(e) = crate::db::repository::set_deployed_source_hash(db, row.id, hash) {
        warn!(
            "persist_source_hash({}): set_deployed_source_hash: {}",
            engine_id, e
        );
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
    service_manager.register_quic_service(engine_id.to_string(), service_type, url, None, None);
}

/// Rejestruje wpis `services` dla silnika ktory nie utrzymuje persistent
/// kontenera (agents/tools). Status=on_demand informuje GUI ze instancje sa
/// spawnowane per-zadanie (teams-bot: per spotkanie przez MeetingManager).
/// Idempotentne: jesli wpis z ta sama nazwa juz istnieje, nie nadpisujemy go.
fn register_on_demand_service(db: &DbPool, engine_id: &str, category: &str, image_tag: &str) {
    let config_json = serde_json::json!({
        "deploy_mode": "docker",
        "image": image_tag,
        "on_demand": true,
    })
    .to_string();
    if let Err(e) = crate::db::repository::upsert_service_on_demand(
        db,
        engine_id,
        service_type_from_category(category),
        Some(category),
        &config_json,
    ) {
        warn!("register_on_demand_service '{}': {}", engine_id, e);
    }
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

/// Parsuje linie progress z `docker build --progress=plain` (BuildKit) LUB legacy.
/// BuildKit: `#N [step M/K name]` gdzie N rosnie monotonicznie (numer task-a),
///   dodatkowo `#M N.Nss` timing. Aktualizujemy max_step_seen jako heurystyke.
/// Legacy: `Step N/M : ...` (stary format).
/// Zwraca pct w zakresie 10..90.
fn parse_progress_line(
    line: &str,
    max_step_seen: &mut u32,
    total_steps: &mut Option<u32>,
) -> Option<u32> {
    let trimmed = line.trim_start_matches('\u{1b}').trim();

    // Legacy "Step N/M"
    if let Some((cur, total)) = parse_step(trimmed) {
        if total > 0 {
            let pct = 10 + ((cur as f32 / total as f32) * 80.0) as u32;
            return Some(pct.min(90));
        }
    }

    // BuildKit "#N [step X/Y ...]" albo "#N [stage-name X/Y name]"
    if let Some(rest) = trimmed.strip_prefix('#') {
        let num_end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if num_end > 0 {
            if let Ok(step_no) = rest[..num_end].parse::<u32>() {
                if step_no > *max_step_seen {
                    *max_step_seen = step_no;
                }
            }
        }
        // Szukamy "X/Y" w nawiasie kwadratowym — np. "[4/8]" lub "[stage-0 4/8]"
        if let Some(start) = rest.find('[') {
            if let Some(end) = rest[start..].find(']') {
                let inside = &rest[start + 1..start + end];
                for tok in inside.split_whitespace() {
                    if let Some((cur_s, tot_s)) = tok.split_once('/') {
                        if let (Ok(cur), Ok(total)) = (cur_s.parse::<u32>(), tot_s.parse::<u32>()) {
                            if total > 0 {
                                *total_steps = Some(total);
                                let pct = 10 + ((cur as f32 / total as f32) * 80.0) as u32;
                                return Some(pct.min(90));
                            }
                        }
                    }
                }
            }
        }
        // Fallback — monotoniczne max_step_seen mapujemy logarytmicznie
        // (docker build ma zwykle 10-40 tasks — zalozmy 30 jako medium).
        if *max_step_seen > 0 {
            let approx = (*max_step_seen).min(30);
            let pct = 10 + (approx as f32 / 30.0 * 80.0) as u32;
            return Some(pct.min(90));
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
                std::io::Error::new(e.kind(), format!("tar append {}: {}", sub_rel.display(), e))
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
