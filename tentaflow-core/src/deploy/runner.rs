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

use std::io::BufRead;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use tokio::sync::{broadcast, mpsc};
use tracing::{info, warn};

use crate::crypto::SettingsCipher;
use crate::db::repository as repository;
use crate::db::repository::deployments as deployments_repo;
use crate::db::DbPool;
use crate::deploy::log_bus::{self, BusMessage};
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
    /// Zaawansowane VLLM_ARGS z deploy wizard (Advanced section). Gdy
    /// ustawione, nadpisuja AUTO_PARALLEL i default VLLM_ARGS w entrypoint.sh.
    /// Format: gotowy CLI args string, np. "--tensor-parallel-size 2
    /// --pipeline-parallel-size 3 --max-model-len 16384 --kv-cache-dtype fp8"
    #[serde(default)]
    vllm_args: Option<String>,
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
    settings_cipher: Arc<SettingsCipher>,
    router: Arc<crate::routing::router::Router>,
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
                &settings_cipher,
                &router,
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
        .map(|n| n.runtime.as_kebab_str().to_string());

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
    // Rozwiazujemy model przed startem kontenera, zeby:
    //  1) nazwa serwisu zawierala slug modelu (analog do native — dwa modele
    //     na tym samym engine, np. dwa vllm z roznymi modelami, nie koliduja),
    //  2) po starcie miec czym zarejestrowac wpis w `models` i mapping
    //     model_name -> service_name w ServiceManager (router routuje po HF
    //     repo name, np. "Qwen/Qwen3.5-0.8B").
    let model_repo = resolve_model_repo(engine, config).ok();
    let service_name = docker_service_name(engine, config, model_repo.as_deref());
    let container_name = config
        .container_name
        .clone()
        .unwrap_or_else(|| service_name.clone());
    let host_port = config.port.unwrap_or(engine.default_port);

    // Sidecar provisioning: generujemy (lub re-uzywamy z poprzedniego deployu)
    // Ed25519 secret key dla iroh endpointa sidecara, oraz piszemy /data/config.toml
    // ktory mountujemy do kontenera. Reuse klucza przy redeployu zachowuje stabilne
    // EndpointId — bez tego ServiceManager dropowalby polaczenie po kazdym restarcie.
    let sidecar = provision_docker_sidecar(
        &service_name,
        &engine.engine_id,
        engine.default_port,
        model_repo.as_deref(),
    )
    .context("przygotowanie sidecara (klucz iroh + config.toml)")?;
    log_line(
        db,
        deploy_id,
        tx,
        "log",
        &format!(
            "sidecar zainicjalizowany — endpoint_id={} (klucz w {})",
            sidecar.endpoint_id_hex,
            sidecar.dir.display()
        ),
    );

    let req = crate::deploy::docker::DeployRequest {
        // context_path z manifestu (np. "llm/docker/vllm") — docker::deploy
        // sklada to z prefixem "tentaflow-containers/" w sciezke do Dockerfile.
        container: context_path.clone(),
        image_tag: Some(image_tag.to_string()),
        instance_name: Some(container_name.clone()),
        // Eksponujemy WYLACZNIE port sidecara (5000/udp). Wewnetrzny port silnika
        // (np. 8000/tcp dla vLLM) jest niewidoczny z hosta — sidecar w kontenerze
        // forwarduje do `127.0.0.1:<engine.default_port>` w tej samej sieci kontenera.
        ports: vec![(host_port.to_string(), "5000/udp".to_string())],
        // Mount klucza + config sidecara jako /data RO. Sidecar czyta /data/config.toml
        // i /data/endpoint-key.bin (zgodnie z `tentaflow-sidecar`).
        volumes: vec![(sidecar.dir.display().to_string(), "/data".to_string())],
        // Env dla silnika w kontenerze: entrypoint.sh dla LLM (vllm/sglang/...) i
        // wielu STT/TTS oczekuje `MODEL` lub `MODEL_ID` z HF repo. Przekazujemy
        // oba zeby pasowalo do roznych konwencji entrypointow. Plus VLLM_ARGS
        // gdy user ustawil zaawansowana konfiguracje w deploy wizard
        // (Advanced section z kalkulatorem VRAM).
        env: {
            let mut e = std::collections::HashMap::new();
            if let Some(m) = model_repo.as_deref() {
                e.insert("MODEL".to_string(), m.to_string());
                e.insert("MODEL_ID".to_string(), m.to_string());
            }
            if let Some(args) = config.vllm_args.as_deref() {
                let trimmed = args.trim();
                if !trimmed.is_empty() {
                    e.insert("VLLM_ARGS".to_string(), trimmed.to_string());
                }
            }
            e
        },
        gpu: config.gpu_select_mode.as_deref() == Some("all")
            || config
                .gpu_ids
                .as_ref()
                .map(|v| !v.is_empty())
                .unwrap_or(false),
    };
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
    let service_id = register_service(
        db,
        &service_name,
        &engine.engine_id,
        &engine.category,
        &created_name,
        host_port,
        model_repo.as_deref(),
    );
    register_docker_quic_service(
        service_manager,
        &service_name,
        &engine.category,
        &sidecar.endpoint_id_hex,
        host_port,
    );
    let _ = node_id;

    // Spawn `docker logs --follow` w tle - linie kontenera (sidecar + engine)
    // streamowane do deploy_log z prefixem [docker]. Bez tego user widzi tylko
    // 'kontener uruchomiony' i potem cisze az QUIC peer się polaczy. Dla 31B
    // multimodal vllm load + warmup zajmuje 3-5min - bez logow wyglada jak hang.
    spawn_docker_log_tailer(db.clone(), deploy_id.to_string(), tx.clone(), created_name.clone());
    // Symetria z native LLM/STT/TTS/Embeddings: po register_service wpinamy
    // model do tabeli `models` i mappingu routera. Bez tego zakladka Models
    // jest pusta po docker deploy, a /v1/chat/completions z model="Qwen/..."
    // zwraca "Model not found" (tylko nazwa serwisu trafia w routera).
    if let (Some(model_name), Some(svc_id)) = (model_repo.as_deref(), service_id) {
        let display_name = format!("{} ({})", model_name, engine.engine_id);
        let registry_config = serde_json::json!({
            "deploy_mode": "docker",
            "engine": engine.engine_id,
            "deployed_model": model_name,
            "service_type": service_type_from_category(&engine.category),
            "port": host_port,
            "container_name": created_name,
        })
        .to_string();
        ensure_model_registry_entry(
            db,
            model_name,
            &display_name,
            service_type_from_category(&engine.category),
            svc_id,
            &registry_config,
        );
        // Mapping HF repo -> service_name; QUIC LLM service jest juz zarejestrowany
        // przez register_docker_quic_service(), wiec router znajdzie go po nazwie
        // serwisu albo po nazwie modelu (model_aliases sidecar publikuje rowniez).
        // NIE rejestrujemy `register_local_inference_model` — local inference
        // jest dla embedded native (in-process MLX/llama.cpp); silnik w kontenerze
        // jest osiagalny tylko przez QUIC sidecar.
        service_manager.register_model_mapping(model_name, &service_name);
    }
    // Alias service_name -> service_name w model_pool. GUI dispatchuje
    // chat completion z model=service_name (np. "tentaflow-vllm-ec7sj"),
    // nie z HF repo. Bez tego routera 'Model X nie znaleziony w konfiguracji'.
    // Parytet z python-bundle (runner.rs:1750+).
    service_manager.register_model_mapping(&service_name, &service_name);
    persist_source_hash(db, &engine.engine_id, "docker", &service_name);
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
    settings_cipher: &Arc<SettingsCipher>,
    router: &Arc<crate::routing::router::Router>,
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
                router,
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
            do_binary_native_deploy(
                db,
                deploy_id,
                tx,
                engine,
                node_id,
                config,
                start_ms,
            )
            .await
        }
        "python-bundle" => {
            do_python_bundle_native_deploy(
                db,
                service_manager,
                settings_cipher,
                deploy_id,
                tx,
                engine,
                node_id,
                config,
                start_ms,
            )
            .await
        }
        other => Err(anyhow!("Nieznany runtime: {}", other)),
    }
}

/// Deploy native runtime=binary: zaklada ze binarka jest juz zbudowana i lezy
/// obok `tentaflow` (zasluga `tentaflow/build.rs`). Funkcja sprawdza obecnosc
/// binarki na dysku i rejestruje serwis w DB. Faktyczne uruchomienie procesu
/// dzieje sie per-zadanie — np. dla teams-bota MeetingManager spawnuje
/// `tentaflow-meeting` osobno per spotkanie.
async fn do_binary_native_deploy(
    db: &DbPool,
    deploy_id: &str,
    tx: &broadcast::Sender<BusMessage>,
    engine: &EngineMeta,
    node_id: &str,
    config: &DeployConfig,
    start_ms: i64,
) -> Result<()> {
    // Mapowanie engine_id -> nazwa binarki. Aktualnie tylko teams-bot, ale
    // dodawanie kolejnych engineow runtime=binary sprowadza sie do dorzucenia
    // entry w tym matchu i odpowiedniej zaleznosci w `tentaflow/build.rs`.
    let bin_name: &str = match engine.engine_id.as_str() {
        "teams-bot" => {
            if cfg!(target_os = "windows") {
                "tentaflow-meeting.exe"
            } else {
                "tentaflow-meeting"
            }
        }
        other => {
            anyhow::bail!(
                "runtime=binary: brak mapowania engine_id '{}' na binarke",
                other
            );
        }
    };

    phase(db, deploy_id, tx, "building", 30, "weryfikacja binarki natywnej");

    let exe = std::env::current_exe()
        .context("nie udalo sie ustalic sciezki biezacej binarki tentaflow")?;
    let exe_dir = exe
        .parent()
        .ok_or_else(|| anyhow!("biezaca binarka nie ma katalogu nadrzednego"))?;
    let bin_path = exe_dir.join(bin_name);
    if !bin_path.is_file() {
        anyhow::bail!(
            "Binarka {} nie istnieje obok tentaflow ({}). Zbuduj projekt 'cargo build --release' \
             zeby tentaflow/build.rs zbudowal sidecar bota.",
            bin_name,
            bin_path.display()
        );
    }
    log_line(
        db,
        deploy_id,
        tx,
        "log",
        &format!("Binarka znaleziona: {}", bin_path.display()),
    );

    let service_name = config
        .container_name
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("{}-native", slugify_name(&engine.engine_id)));

    phase(db, deploy_id, tx, "registering", 92, "rejestracja serwisu native");

    // services.service_type ma CHECK constraint na lp ('agent', 'tool', ...).
    // engine.category z manifestu jest plural ('agents', 'tools') wiec mapujemy
    // przez service_type_from_category zanim trafi do DB.
    let svc_type = service_type_from_category(&engine.category);
    let config_json = serde_json::json!({
        "deploy_mode": "native",
        "runtime": "binary",
        "engine": engine.engine_id,
        "manifest_engine_id": engine.engine_id,
        "service_type": svc_type,
        "binary_path": bin_path.to_string_lossy(),
    })
    .to_string();

    upsert_native_service(
        db,
        node_id,
        &service_name,
        svc_type,
        None,
        &config_json,
        "first_available",
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

async fn do_embedded_native_deploy(
    db: &DbPool,
    service_manager: &Arc<ServiceManager>,
    router: &Arc<crate::routing::router::Router>,
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

            let service_id = upsert_native_service(
                db,
                node_id,
                &service_name,
                "stt",
                Some("stt"),
                &config_json,
                "single",
            )?;
            ensure_model_registry_entry(
                db,
                "whisper-large-v3-turbo",
                "Whisper Large V3 Turbo",
                "stt",
                service_id,
                &config_json,
            );

            // Rejestruj w mesh — bez tego router widzi serwis w DB ale nie
            // wie ze jest live na tym nodzie (Mesh STT serwis nie polaczony).
            router.register_native_service_in_mesh(
                &service_name, "stt",
                vec!["whisper-large-v3-turbo".to_string()],
                Some("whisper".to_string()),
                vec![stt_info.size_bytes / (1024 * 1024)],
            );
            // Auto-bind alias `teams-stt` jezeli pusty.
            auto_bind_teams_alias_if_empty(db, "teams-stt", &service_name, router);

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
        #[cfg(feature = "inference-mlx-kokoro")]
        ("tts", "kokoro") if std::env::consts::OS == "macos" => {
            let model_repo = resolve_model_repo(engine, config)
                .unwrap_or_else(|_| "mlx-community/Kokoro-82M-bf16".to_string());
            let service_name = config
                .container_name
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "kokoro-tts-native".to_string());

            phase(db, deploy_id, tx, "building", 20, "download kokoro");
            let resolved = crate::tts::mlx_kokoro::prepare_model(&model_repo)
                .await
                .with_context(|| format!("prepare kokoro model '{}'", model_repo))?;

            phase(db, deploy_id, tx, "running", 75, "load kokoro");
            let mut engine_impl = crate::tts::mlx_kokoro::MlxKokoroEngine::new();
            let info = <crate::tts::mlx_kokoro::MlxKokoroEngine as crate::tts::TtsEngine>::load_model(
                &mut engine_impl,
                &resolved,
            )
            .context("load kokoro w Swift bridge")?;

            {
                let shared = crate::tts::shared_tts_manager();
                let mut mgr = shared.write().await;
                mgr.register(service_name.clone(), Box::new(engine_impl));
            }

            phase(db, deploy_id, tx, "registering", 92, "register service");
            let config_json = serde_json::json!({
                "deploy_mode": "native",
                "engine": "kokoro",
                "manifest_engine_id": engine.engine_id,
                "deployed_model": model_repo,
                "model_path": resolved.to_string_lossy(),
                "service_type": "tts",
                "sample_rate": info.sample_rate,
            })
            .to_string();
            let service_id = upsert_native_service(
                db, node_id, &service_name, "tts", Some("tts"),
                &config_json, "single",
            )?;
            ensure_model_registry_entry(
                db,
                &model_repo,
                &format!("Kokoro 82M MLX ({})", model_repo),
                "tts",
                service_id,
                &config_json,
            );
            router.register_native_service_in_mesh(
                &service_name, "tts",
                vec![model_repo.clone()],
                Some("kokoro".to_string()),
                vec![info.sample_rate as u64 / 1000],  // placeholder
            );
            auto_bind_teams_alias_if_empty(db, "teams-tts", &service_name, router);
            persist_source_hash(db, &engine.engine_id, "native", &service_name);
            log_line(db, deploy_id, tx, "log", &format!("Kokoro TTS gotowe: {}", service_name));
            let _ = service_manager;
            finish_success(db, deploy_id, tx, start_ms, String::new(), service_name).await;
            Ok(())
        }
        #[cfg(feature = "inference-sherpa")]
        ("tts", "sherpa-onnx") => {
            // Cross-platform embedded TTS przez sherpa-rs (sherpa-onnx + ORT).
            // Pobieramy sherpa-onnx-compatible bundle z HF, ladujemy do
            // shared_tts_manager() i rejestrujemy native service w mesh.
            let model_repo = resolve_model_repo(engine, config)
                .unwrap_or_else(|_| "csukuangfj/vits-piper-en_US-amy-medium".to_string());
            let service_name = config
                .container_name
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "sherpa-tts-native".to_string());

            phase(db, deploy_id, tx, "building", 20, "download VITS Piper");
            let model_dir = crate::tts::sherpa::prepare_model(&model_repo)
                .await
                .with_context(|| format!("prepare sherpa model '{}'", model_repo))?;

            phase(db, deploy_id, tx, "running", 75, "load sherpa-onnx VITS");
            let mut engine_impl = crate::tts::sherpa::SherpaTtsEngine::new();
            let info = <crate::tts::sherpa::SherpaTtsEngine as crate::tts::TtsEngine>::load_model(
                &mut engine_impl,
                &model_dir,
            )
            .context("load sherpa-onnx VITS model")?;

            {
                let shared = crate::tts::shared_tts_manager();
                let mut mgr = shared.write().await;
                mgr.register(service_name.clone(), Box::new(engine_impl));
            }

            phase(db, deploy_id, tx, "registering", 92, "register service");
            let config_json = serde_json::json!({
                "deploy_mode": "native",
                "engine": "sherpa-onnx",
                "manifest_engine_id": engine.engine_id,
                "deployed_model": model_repo,
                "model_path": model_dir.to_string_lossy(),
                "service_type": "tts",
                "sample_rate": info.sample_rate,
            })
            .to_string();
            let service_id = upsert_native_service(
                db, node_id, &service_name, "tts", Some("tts"),
                &config_json, "single",
            )?;
            ensure_model_registry_entry(
                db,
                &model_repo,
                &format!("sherpa-onnx VITS ({})", model_repo),
                "tts",
                service_id,
                &config_json,
            );
            router.register_native_service_in_mesh(
                &service_name, "tts",
                vec![model_repo.clone()],
                Some("sherpa-onnx".to_string()),
                vec![info.sample_rate as u64 / 1000],
            );
            auto_bind_teams_alias_if_empty(db, "teams-tts", &service_name, router);
            persist_source_hash(db, &engine.engine_id, "native", &service_name);
            log_line(
                db,
                deploy_id,
                tx,
                "log",
                &format!("sherpa-onnx TTS gotowe: {}", service_name),
            );
            let _ = service_manager;
            finish_success(db, deploy_id, tx, start_ms, String::new(), service_name).await;
            Ok(())
        }
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        ("tts", "apple-tts") => {
            let service_name = config
                .container_name
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "apple-tts-native".to_string());
            // Glos wybierany przez `model_repo` (`zosia-pl`, `samantha-en`...).
            // Apple nie pobiera modelu z dysku (uzywa głosów systemowych macOS) —
            // przekazujemy stalą "apple-tts" jako logiczny identyfikator silnika;
            // placeholder "system" powodowal mylące target=system w logach routera i w GUI.
            let voice_id = resolve_model_repo(engine, config)
                .unwrap_or_else(|_| "zosia-pl".to_string());

            phase(db, deploy_id, tx, "running", 75, "init apple tts");
            let mut engine_impl = crate::tts::apple_tts::AppleTtsEngine::new();
            let info = <crate::tts::apple_tts::AppleTtsEngine as crate::tts::TtsEngine>::load_model(
                &mut engine_impl,
                std::path::Path::new("apple-tts"),
            )
            .context("init apple-tts (brak libMLXBridge.dylib?)")?;
            // Rejestracja w globalnym TtsManager pod kluczem service_name —
            // router znajduje silnik po nazwie serwisu albo po backend_name.
            {
                let shared = crate::tts::shared_tts_manager();
                let mut mgr = shared.write().await;
                mgr.register(service_name.clone(), Box::new(engine_impl));
            }

            phase(db, deploy_id, tx, "registering", 92, "register service");
            let config_json = serde_json::json!({
                "deploy_mode": "native",
                "engine": "apple-tts",
                "manifest_engine_id": engine.engine_id,
                "deployed_model": voice_id,
                "model_path": "apple-tts",
                "service_type": "tts",
                "sample_rate": info.sample_rate,
            })
            .to_string();
            let service_id = upsert_native_service(
                db,
                node_id,
                &service_name,
                "tts",
                Some("tts"),
                &config_json,
                "single",
            )?;
            ensure_model_registry_entry(
                db,
                &format!("apple-tts-{}", voice_id),
                &format!("Apple TTS ({})", voice_id),
                "tts",
                service_id,
                &config_json,
            );
            router.register_native_service_in_mesh(
                &service_name, "tts",
                vec![voice_id.clone()],
                Some("apple-tts".to_string()),
                vec![0],
            );
            auto_bind_teams_alias_if_empty(db, "teams-tts", &service_name, router);
            persist_source_hash(db, &engine.engine_id, "native", &service_name);
            log_line(
                db,
                deploy_id,
                tx,
                "log",
                &format!("Apple TTS gotowe: {}", service_name),
            );
            let _ = service_manager;
            finish_success(db, deploy_id, tx, start_ms, String::new(), service_name).await;
            Ok(())
        }
        #[cfg(feature = "inference-mlx-whisper")]
        ("stt", "mlx-whisper") => {
            let model_repo = resolve_model_repo(engine, config)
                .unwrap_or_else(|_| "mlx-community/whisper-large-v3-turbo-4bit".to_string());
            let service_name = config
                .container_name
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "mlx-whisper-stt-native".to_string());

            phase(db, deploy_id, tx, "building", 20, "download mlx whisper");
            // `prepare_model` pobiera oba HF repo (mlx-community + openai
            // tokenizer) do scalonego cache i zwraca sciezke. Synchroniczne
            // hf-hub w spawn_blocking jest po stronie funkcji.
            let resolved = crate::stt::mlx_whisper::prepare_model(&model_repo)
                .await
                .with_context(|| format!("prepare mlx-whisper model '{}'", model_repo))?;

            phase(db, deploy_id, tx, "running", 75, "load mlx whisper");
            let shared = crate::stt::shared_stt_manager();
            let stt_info = {
                let mut mgr = shared.write().await;
                mgr.load_model(&resolved, None, Some("mlx-whisper")).await
            }
            .context("load mlx-whisper model")?;

            phase(db, deploy_id, tx, "registering", 92, "register service");
            let config_json = serde_json::json!({
                "deploy_mode": "native",
                "engine": "mlx-whisper",
                "manifest_engine_id": engine.engine_id,
                "deployed_model": model_repo,
                "model_path": stt_info.path,
                "service_type": "stt",
            })
            .to_string();

            let service_id = upsert_native_service(
                db,
                node_id,
                &service_name,
                "stt",
                Some("stt"),
                &config_json,
                "single",
            )?;
            // mlx-whisper bug fix: dodaj wpis w model_registry zeby model byl
            // widoczny w panelu Modele (wczesniej tylko Serwisy).
            ensure_model_registry_entry(
                db,
                &model_repo,
                &format!("MLX Whisper ({})", model_repo),
                "stt",
                service_id,
                &config_json,
            );

            // Rejestracja mesh — bez tego router zwraca "Mesh STT serwis nie
            // polaczony". Wczesniej tylko `restore_native_services` przy
            // starcie tentaflow to robil; po deployu trzeba wprost.
            router.register_native_service_in_mesh(
                &service_name, "stt",
                vec![model_repo.clone()],
                Some("mlx-whisper".to_string()),
                vec![stt_info.size_bytes / (1024 * 1024)],
            );
            auto_bind_teams_alias_if_empty(db, "teams-stt", &service_name, router);

            persist_source_hash(db, &engine.engine_id, "native", &service_name);

            log_line(
                db,
                deploy_id,
                tx,
                "log",
                &format!("natywny serwis zarejestrowany: {}", service_name),
            );
            // service_manager nieuzywany dla STT — istniejacy whisper case
            // tez go pomija. Adresacja jako `meeting-bot` -> mesh -> stt
            // dziala przez `routing/handlers/stt.rs`, ktore wybiera serwis
            // po `service_type=stt` z tabeli `services`.
            let _ = service_manager;
            finish_success(db, deploy_id, tx, start_ms, String::new(), service_name).await;
            Ok(())
        }
        ("vision", engine_id) => {
            let kind = crate::vision::VisionEngineKind::from_id(engine_id)
                .ok_or_else(|| anyhow!("vision: nieznany engine_id '{}'", engine_id))?;

            let service_name = config
                .container_name
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| {
                    format!("tentaflow-{}-{}", engine_id, slugify_name(&random_suffix()))
                });

            phase(db, deploy_id, tx, "running", 70, "load vision model");

            let model_path = crate::vision::model_path_for(kind).ok_or_else(|| {
                anyhow!(
                    "vision/{}: ONNX nie embedowany w binarce (uruchom setup.sh + cargo build)",
                    engine_id
                )
            })?;

            let loaded = crate::vision::load_engine(kind, &model_path)
                .with_context(|| format!("load vision/{} z {}", engine_id, model_path.display()))?;

            crate::vision::register_engine(service_name.clone(), loaded);

            phase(db, deploy_id, tx, "registering", 92, "register service");

            let config_json = serde_json::json!({
                "deploy_mode": "native",
                "engine": "tract-onnx",
                "manifest_engine_id": engine.engine_id,
                "deployed_model": engine_id,
                "model_path": model_path.display().to_string(),
                "service_type": "vision",
            })
            .to_string();

            let _service_id = upsert_native_service(
                db,
                node_id,
                &service_name,
                "vision",
                Some("vision"),
                &config_json,
                "single",
            )?;

            // Mesh registration — przy multi-node deploy router widzi serwis
            // jako resolvable. Uzywamy `engine_id` jako 'model name' w mesh
            // — caller rozpoznaje vision po `service_type=vision`.
            router.register_native_service_in_mesh(
                &service_name,
                "vision",
                vec![engine_id.to_string()],
                Some("tract-onnx".to_string()),
                vec![std::fs::metadata(&model_path).map(|m| m.len() / (1024 * 1024)).unwrap_or(0)],
            );

            // Auto-wiązanie aliasów teams-vision-* — pipeline meeting bota
            // (`reverse_request.rs::VideoFrame`) rozwiązuje te aliasy do
            // konkretnego service_name. Bez tego wymagałoby ręcznej konfiguracji
            // po każdym deployu silnika vision. Mapowanie engine_id → alias:
            //   SCRFD / YOLOv8-Face → teams-vision-face
            //   HSEmotion / EmoNet  → teams-vision-emotion
            //   MiVOLO              → teams-vision-age (zawiera też płeć)
            match engine_id {
                "scrfd" | "yolov8-face" => {
                    auto_bind_teams_alias_if_empty(
                        db,
                        crate::meeting::manager::DEFAULT_VISION_FACE_ALIAS,
                        &service_name,
                        router,
                    );
                }
                "hsemotion" | "emonet" => {
                    auto_bind_teams_alias_if_empty(
                        db,
                        crate::meeting::manager::DEFAULT_VISION_EMOTION_ALIAS,
                        &service_name,
                        router,
                    );
                }
                "mivolo" => {
                    auto_bind_teams_alias_if_empty(
                        db,
                        crate::meeting::manager::DEFAULT_VISION_AGE_ALIAS,
                        &service_name,
                        router,
                    );
                }
                _ => {}
            }

            persist_source_hash(db, &engine.engine_id, "native", &service_name);

            log_line(
                db,
                deploy_id,
                tx,
                "log",
                &format!("vision serwis zarejestrowany: {}", service_name),
            );
            let _ = service_manager;
            finish_success(db, deploy_id, tx, start_ms, String::new(), service_name).await;
            Ok(())
        }
        _ => Err(anyhow!(
            "runtime=embedded dla '{}' nie ma jeszcze zintegrowanego flow deploymentu",
            engine.engine_id
        )),
    }
}

fn random_suffix() -> String {
    use rand::distr::{Alphanumeric, SampleString};
    Alphanumeric.sample_string(&mut rand::rng(), 5).to_lowercase()
}

/// Deploy native runtime=python-bundle: wywoluje `deploy::python_venv::deploy_with_logs`
/// w blocking thread pool, streamuje kazda linie stdout/stderr z subprocesu (uv
/// pip install, python -m venv, git clone, wlasciwy silnik) do broadcast_bus zeby
/// GUI widzial progress. Po sukcesie rejestruje serwis w DB `services` z PID +
/// venv_dir w config_json zeby backend mogl zrestorowac state po restarcie.
/// Zablokowane na iOS/Android — tam Python-bundle nie dziala (sandboxing, brak
/// Pythona w systemie), silniki mobilne uzywaja wylacznie embedded FFI.
async fn do_python_bundle_native_deploy(
    db: &DbPool,
    _service_manager: &Arc<ServiceManager>,
    settings_cipher: &Arc<SettingsCipher>,
    deploy_id: &str,
    tx: &broadcast::Sender<BusMessage>,
    engine: &EngineMeta,
    node_id: &str,
    config: &DeployConfig,
    start_ms: i64,
) -> Result<()> {
    match std::env::consts::OS {
        "linux" | "macos" | "windows" => {}
        other => {
            anyhow::bail!(
                "runtime=python-bundle nieobslugiwany na platformie {} — tylko linux/macos/windows",
                other
            );
        }
    }

    let model_repo = resolve_model_repo(engine, config).unwrap_or_default();
    let service_name = if model_repo.is_empty() {
        config
            .container_name
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| format!("{}-native", slugify_name(&engine.engine_id)))
    } else {
        native_service_name(engine, config, &model_repo)
    };
    let host_port = config.port.unwrap_or(engine.default_port);

    phase(db, deploy_id, tx, "building", 10, "przygotowywanie bundla");

    let mut env: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    env.insert("PORT".to_string(), host_port.to_string());
    if !model_repo.is_empty() {
        env.insert("MODEL".to_string(), model_repo.clone());
    }
    if let Some(ids) = config.gpu_ids.as_ref().filter(|v| !v.is_empty()) {
        env.insert("GPU_IDS".to_string(), ids.join(","));
    }

    // VLLM_ARGS z deploy wizard Advanced (kalkulator VRAM) - dla bundle
    // python jest podawane jako env do bundle.toml ${VLLM_ARGS:-...}.
    let mut vllm_args_explicit = false;
    if let Some(args) = config.vllm_args.as_deref() {
        let trimmed = args.trim();
        if !trimmed.is_empty() {
            env.insert("VLLM_ARGS".to_string(), trimmed.to_string());
            vllm_args_explicit = true;
        }
    }

    // AUTO-DEFAULTS dla bundle vllm gdy user NIE ustawil Advanced wizard:
    // wykryj liczbe GPU + VRAM, pobierz HF config modelu, smart-pick TP/PP/
    // ctx/seqs/kv_dtype zgodnie z heads/layers + VRAM. Bez tego user pisze
    // model + GPU = 'all' i dostaje OOM (np. 31B na 1 GPU bez TP) lub
    // multimodal crash (--max-num-batched-tokens default 2048).
    if !vllm_args_explicit && engine.engine_id == "vllm" && !model_repo.is_empty() {
        match build_auto_vllm_args(&model_repo, config.gpu_ids.as_deref(), db, settings_cipher).await {
            Ok(Some(auto_args)) => {
                log_line(
                    db,
                    deploy_id,
                    tx,
                    "log",
                    &format!("auto-tuned VLLM_ARGS (kalkulator VRAM): {}", auto_args),
                );
                env.insert("VLLM_ARGS".to_string(), auto_args);
            }
            Ok(None) => {
                log_line(db, deploy_id, tx, "log",
                    "auto-tune skip: brak GPU lub HF config niedostepny - uzywam default args z bundle.toml");
            }
            Err(e) => {
                log_line(db, deploy_id, tx, "log",
                    &format!("auto-tune fail: {} - uzywam default args z bundle.toml", e));
            }
        }
    }

    // Hugging Face token z zaszyfrowanego ustawienia `hf_token` w DB —
    // uzywany i przy install (uv pip sciaga wheels z HF), i przy runtime
    // (pobieranie modeli przez transformers/HF Hub dla gated repo).
    let hf_token = repository::get_setting_secure(db, "hf_token", settings_cipher)
        .unwrap_or_default()
        .unwrap_or_default();
    if !hf_token.is_empty() {
        env.insert("HF_TOKEN".to_string(), hf_token.clone());
        env.insert("HUGGING_FACE_HUB_TOKEN".to_string(), hf_token);
    }

    // Wspolny katalog modeli dla Docker + native — model pobrany raz, widziany
    // wszedzie. crate::paths::ensure_models_dirs tworzy <tentaflow_home>/models/
    // i podkatalogi hf/torch.
    let _ = crate::paths::ensure_models_dirs();
    let hf_home = crate::paths::hf_home();
    let torch_home = crate::paths::torch_home();
    env.insert("HF_HOME".to_string(), hf_home.to_string_lossy().into_owned());
    env.insert(
        "HUGGINGFACE_HUB_CACHE".to_string(),
        hf_home.to_string_lossy().into_owned(),
    );
    env.insert(
        "TRANSFORMERS_CACHE".to_string(),
        hf_home.to_string_lossy().into_owned(),
    );
    env.insert(
        "TORCH_HOME".to_string(),
        torch_home.to_string_lossy().into_owned(),
    );

    // Klonujemy env przed konstrukcja native_req — native_req trafia do
    // spawn_blocking (move), env_for_guard zostaje na pozniejszy register
    // w MemoryGuard.
    let env_for_guard = env.clone();
    let native_req = crate::deploy::python_venv::NativeDeployRequest {
        engine: engine.engine_id.clone(),
        instance_name: Some(service_name.clone()),
        env,
    };

    // Streaming stdout/stderr z subprocesow (pobieranie Pythona, uv pip install,
    // git clone, spawn silnika). `python_venv::deploy_with_logs` pracuje w
    // blocking threadpool — kanal mpsc przekazuje linie do async forwardera.
    let (log_tx_sync, mut log_rx_async) = tokio::sync::mpsc::unbounded_channel::<String>();
    let sink: crate::deploy::python_venv::LogSink = Arc::new(move |line: &str| {
        let _ = log_tx_sync.send(line.to_string());
    });

    let db_forward = db.clone();
    let deploy_forward = deploy_id.to_string();
    let tx_forward = tx.clone();
    let forwarder = tokio::spawn(async move {
        while let Some(line) = log_rx_async.recv().await {
            log_line(&db_forward, &deploy_forward, &tx_forward, "log", &line);
        }
    });

    phase(
        db,
        deploy_id,
        tx,
        "building",
        30,
        "pobieranie Pythona + instalacja zaleznosci",
    );

    let sink_blocking = Arc::clone(&sink);
    let mut running = tokio::task::spawn_blocking(move || {
        crate::deploy::python_venv::deploy_with_logs(&native_req, &sink_blocking)
    })
    .await
    .context("spawn_blocking python_venv::deploy_with_logs")?
    .context("python_venv::deploy_with_logs")?;

    let pid = running.child.id();

    phase(
        db,
        deploy_id,
        tx,
        "starting",
        85,
        "silnik wystartowany — czekam na gotowosc",
    );

    // Pipeline stdout/stderr silnika do deploy_log — m.in. HuggingFace model
    // download ktory vLLM robi po `python -m vllm...` startuje. Watki odczytuja
    // do zamkniecia pipe'ow (gdy engine padnie albo zostanie zabity).
    let stdout_handle = running.child.stdout.take();
    let stderr_handle = running.child.stderr.take();
    let db_c = db.clone();
    let dep_c = deploy_id.to_string();
    let tx_c = tx.clone();
    std::thread::spawn(move || {
        if let Some(o) = stdout_handle {
            for line in std::io::BufReader::new(o)
                .lines()
                .map_while(Result::ok)
            {
                log_line(&db_c, &dep_c, &tx_c, "log", &line);
            }
        }
    });
    let db_c = db.clone();
    let dep_c = deploy_id.to_string();
    let tx_c = tx.clone();
    std::thread::spawn(move || {
        if let Some(e) = stderr_handle {
            for line in std::io::BufReader::new(e)
                .lines()
                .map_while(Result::ok)
            {
                log_line(&db_c, &dep_c, &tx_c, "log", &line);
            }
        }
    });

    // Child przekazujemy do `std::mem::forget` zeby Rust drop nie zrobil wait
    // (w Unixie drop nie zabija, ale bez wait zombie przy exit tentaflow —
    // proces kernela zyje niezaleznie). Zarzadzanie cyklem zycia: PID zapisany
    // w config_json services + `kill <pid>` przez osobny endpoint.
    std::mem::forget(running.child);

    // Zamknij log-sink instalacyjny — forwarder z mpsc zakonczy po drop.
    drop(sink);
    let _ = forwarder.await;

    // Health poll: czekamy az silnik odpowie na `/v1/models` (OpenAI-compatible
    // engines — vllm, vllm-metal, xtts w trybie OAI itd.) albo `/health` na
    // standardowym porcie. Podczas czekania live stream stdout/stderr leci juz
    // do deploy_log — user widzi m.in. "Downloading model..." z HF Hub.
    let health_timeout_secs: u64 = std::env::var("TENTAFLOW_DEPLOY_HEALTH_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(900);
    let poll_interval_secs: u64 = 3;
    let max_attempts = health_timeout_secs / poll_interval_secs;
    let health_url = format!("http://127.0.0.1:{}/v1/models", host_port);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok();
    let mut ready = false;
    let mut crashed = false;
    // Tail engine.log: co kazda iteracje czytamy NOWE linie (od last_offset).
    // Dzieki temu user widzi w GUI vllm download progress, model loading
    // ('Loading checkpoint shards: 50%'), HF Hub download speed itp. zamiast
    // patrzec na 'czekam na /v1/models (300s)' przez 10 minut bez sygnalu zycia.
    // Bez tego wyglada jakby tentaflow stal w miejscu chociaz vllm pracuje.
    let log_path = running.venv_dir.join("engine.log");
    let mut last_log_offset: u64 = 0;
    let mut last_engine_activity_at = std::time::Instant::now();
    let mut last_heartbeat_at = std::time::Instant::now();
    for attempt in 0..max_attempts {
        tokio::time::sleep(std::time::Duration::from_secs(poll_interval_secs)).await;
        // Najpierw: czy proces vllm zyje? Jesli padl (np. ImportError, OOM,
        // multimodal config bug), nie ma sensu czekac 15min - failuj natychmiast.
        // libc::kill(pid, 0) zwraca 0 gdy proces zyje, -1 gdy zombie/dead.
        let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
        if !alive {
            crashed = true;
            break;
        }
        if let Some(c) = client.as_ref() {
            if let Ok(resp) = c.get(&health_url).send().await {
                if resp.status().is_success() {
                    ready = true;
                    break;
                }
            }
        }
        // Tail engine.log od last_offset - wyslij nowe linie do GUI.
        // Robione co petla (3s) zeby user widzial dowloads/loading na biezaco.
        if let Ok(mut file) = std::fs::File::open(&log_path) {
            use std::io::{Read, Seek, SeekFrom};
            if let Ok(file_len) = file.seek(SeekFrom::End(0)) {
                if file_len > last_log_offset {
                    let to_read = (file_len - last_log_offset).min(64 * 1024);
                    if file.seek(SeekFrom::Start(last_log_offset)).is_ok() {
                        let mut buf = vec![0u8; to_read as usize];
                        if let Ok(n) = file.read(&mut buf) {
                            if n > 0 {
                                let chunk = String::from_utf8_lossy(&buf[..n]);
                                for line in chunk.lines() {
                                    let trimmed = line.trim_end();
                                    if !trimmed.is_empty() {
                                        // Prefix [engine] zeby uzytkownik
                                        // odroznil log silnika od logow tentaflow.
                                        log_line(db, deploy_id, tx, "log",
                                            &format!("[engine] {}", trimmed));
                                    }
                                }
                                last_log_offset += n as u64;
                                last_engine_activity_at = std::time::Instant::now();
                            }
                        }
                    }
                }
            }
        }
        // Heartbeat: gdy engine.log jest cichy >30s ale proces zyje, pokaz
        // metryki (RSS, GPU util) zeby user wiedzial ze cos sie dzieje.
        // Vllm w fazie torch.compile + load shards potrafi przez 3-5min
        // nie pisac na stdout - bez tego wyglada jak zawieszenie.
        let cichy_sec = last_engine_activity_at.elapsed().as_secs();
        let od_heartbeat = last_heartbeat_at.elapsed().as_secs();
        if cichy_sec >= 30 && od_heartbeat >= 30 {
            last_heartbeat_at = std::time::Instant::now();
            let metrics = read_proc_metrics(pid);
            let gpu_snap = nvidia_smi_snapshot();
            log_line(
                db,
                deploy_id,
                tx,
                "log",
                &format!(
                    "[heartbeat] engine cichy {}s, ale dziala: PID={} RSS={} | {}",
                    cichy_sec,
                    pid,
                    metrics.unwrap_or_else(|| "unknown".into()),
                    gpu_snap.unwrap_or_else(|| "(no nvidia-smi)".into()),
                ),
            );
        }
        // Progress bar 85..92 w trakcie czekania, co 30s aktualizuj wiadomosc
        if attempt > 0 && attempt % 10 == 0 {
            let pct = 85 + ((attempt * 7) / max_attempts.max(1)).min(7) as u32;
            phase(
                db,
                deploy_id,
                tx,
                "starting",
                pct,
                &format!(
                    "czekam na /v1/models na porcie {} ({}s)",
                    host_port,
                    attempt * poll_interval_secs
                ),
            );
        }
    }
    if ready {
        log_line(
            db,
            deploy_id,
            tx,
            "log",
            &format!("silnik odpowiedzial na {} — gotowy", health_url),
        );
    } else {
        // BLAD: silnik nie odpowiedzial w timeoucie ALBO zcrashowal. Nie rejestrujemy
        // HTTP backendu bo router bedzie strzelal w martwy port (ECONNREFUSED) i
        // wszystkie requesty bedą padaly. User musi zobaczyc deploy=failed z
        // czytelnym komunikatem - 80 linii engine.log z konca (multimodal init
        // / OOM / brak modelu wymaga pelnego stack trace).
        let log_path = running.venv_dir.join("engine.log");
        let last_log = std::fs::read_to_string(&log_path)
            .ok()
            .map(|s| {
                let lines: Vec<&str> = s.lines().rev().take(80).collect();
                lines.into_iter().rev().collect::<Vec<_>>().join("\n")
            })
            .unwrap_or_else(|| format!("(brak {})", log_path.display()));
        let reason = if crashed {
            format!("Engine zcrashowal (PID {} nie zyje). Ostatnie 80 linii engine.log:\n{}", pid, last_log)
        } else {
            format!(
                "Bundle nie odpowiedzial na {} po {}s. Ostatnie 80 linii engine.log:\n{}",
                health_url, health_timeout_secs, last_log
            )
        };
        log_line(db, deploy_id, tx, "log", &reason);
        // Zabij proces zeby nie zostawic zombie zajmujacego port (no-op gdy juz dead)
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
        return Err(anyhow!(reason));
    }

    phase(
        db,
        deploy_id,
        tx,
        "registering",
        95,
        "rejestracja serwisu python-bundle",
    );

    let config_json = serde_json::json!({
        "deploy_mode": "native",
        "runtime": "python-bundle",
        "engine": engine.engine_id,
        "manifest_engine_id": engine.engine_id,
        "deployed_model": model_repo,
        "service_type": engine.category,
        "port": host_port,
        "internal_port": running.internal_port,
        "venv_dir": running.venv_dir.to_string_lossy(),
        "pid": pid,
        "instance_name": running.instance_name,
    })
    .to_string();

    let model_category = if engine.category == "llm" {
        Some("llm")
    } else {
        None
    };
    let service_id = upsert_native_service(
        db,
        node_id,
        &service_name,
        &engine.category,
        model_category,
        &config_json,
        "first_available",
    )?;

    // Wpis w tabeli `models` zeby GUI -> Models pokazal silnik po deployu.
    // TYLKO gdy mamy prawdziwa nazwe modelu (HF repo). Bez tego GUI Models
    // pokazywal nazwy serwisow (np. "tentaflow-vllm-9k5p0") jako modele -
    // mylace dla usera, bo to jest service identifier nie model.
    if !model_repo.is_empty() {
        let display = format!("{} ({})", engine.engine_id, model_repo);
        ensure_model_registry_entry(
            db,
            &model_repo,
            &display,
            &engine.category,
            service_id,
            &config_json,
        );
    }

    // Natychmiastowa rejestracja w ServiceManager — router zacznie routowac
    // /v1/chat/completions (i inne OpenAI endpointy) do naszego vLLM-Metal
    // od razu, bez potrzeby restartu tentaflow. Idempotentne — jesli ten sam
    // (service_id, URL) juz istnieje w DB, create_backend jest pominiety.
    let model_override = if model_repo.is_empty() {
        None
    } else {
        Some(model_repo.as_str())
    };
    if let Err(e) = register_native_http_backend(
        db,
        _service_manager,
        service_id,
        &service_name,
        host_port,
        model_override,
    ) {
        log_line(
            db,
            deploy_id,
            tx,
            "log",
            &format!(
                "WARN: rejestracja HTTP backendu nie powiodla sie ({}): {:#}",
                service_name, e
            ),
        );
    } else {
        // ModelPool mapping - bez tego router nie znajdzie backendu po model_name.
        // Dla LLM bundle (vllm/sglang/...) rejestrujemy alias model_repo -> service.
        // Embedded native robi to samo (runner.rs:921); python-bundle musi tez.
        if !model_repo.is_empty() {
            _service_manager.register_model_mapping(&model_repo, &service_name);
        }
        _service_manager.register_model_mapping(&service_name, &service_name);

        // Rejestracja w MemoryGuard — process juz zyje (PID > 0), wiec
        // guard od razu zna ten serwis jako warm.
        let vram_estimate = crate::memory::estimate_vram_for_model(&model_repo);
        let guard_engine = std::sync::Arc::new(crate::memory::PythonBundleEngine::new(
            engine.engine_id.clone(),
            service_name.clone(),
            service_name.clone(),
            model_repo.clone(),
            host_port,
            vram_estimate,
            env_for_guard.clone(),
            pid,
        ));
        let auto_pin = is_orchestrator_model(&engine.engine_id, &model_repo);
        let affinity = parse_gpu_affinity(config.gpu_ids.as_deref());
        crate::memory::guard_global().register(
            service_name.clone(),
            guard_engine,
            vram_estimate,
            auto_pin,
            false,
            affinity,
        );

        log_line(
            db,
            deploy_id,
            tx,
            "log",
            &format!(
                "HTTP backend zarejestrowany: http://127.0.0.1:{}/v1 → {}",
                host_port, service_name
            ),
        );
    }

    log_line(
        db,
        deploy_id,
        tx,
        "log",
        &format!(
            "python-bundle serwis uruchomiony: {} (pid={}, port={})",
            service_name, pid, host_port
        ),
    );
    finish_success(db, deploy_id, tx, start_ms, String::new(), service_name).await;
    Ok(())
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

/// Symetryczny do `native_service_name`: stabilna nazwa serwisu dla docker
/// deploy. Bez tego dwa modele na tym samym engine (np. dwa vllm z roznymi
/// model_repo) kolidowalyby pod nazwa = engine_id. Gdy nie znamy modelu (np.
/// silnik bez `model_preset`), wracamy do `tentaflow-<engine_id>` zachowujac
/// stare zachowanie.
fn docker_service_name(
    engine: &EngineMeta,
    config: &DeployConfig,
    model_repo: Option<&str>,
) -> String {
    if let Some(name) = config
        .container_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return name.to_string();
    }
    let engine_slug = slugify_name(&engine.engine_id);
    match model_repo {
        Some(repo) => format!("{}-docker-{}", engine_slug, slugify_name(repo)),
        None => format!("tentaflow-{}", engine.engine_id),
    }
}

/// Konwersja `config.gpu_ids: Option<Vec<String>>` -> GpuAffinity.
/// Brak / pusta lista / "all" -> All. Pojedynczy idx -> Single. Wiele -> Multi.
fn parse_gpu_affinity(gpu_ids: Option<&[String]>) -> crate::memory::GpuAffinity {
    use crate::memory::GpuAffinity;
    let ids = match gpu_ids {
        Some(v) if !v.is_empty() => v,
        _ => return GpuAffinity::All,
    };
    if ids.iter().any(|s| s.eq_ignore_ascii_case("all")) {
        return GpuAffinity::All;
    }
    if ids.iter().any(|s| s.eq_ignore_ascii_case("cpu")) {
        return GpuAffinity::Cpu;
    }
    let parsed: Vec<usize> = ids.iter().filter_map(|s| s.parse().ok()).collect();
    match parsed.len() {
        0 => GpuAffinity::All,
        1 => GpuAffinity::Single(parsed[0]),
        _ => GpuAffinity::Multi(parsed),
    }
}

/// Czy model powinien byc auto-pinned w MemoryGuard (zawsze warm, nie evict).
/// Domyslnie: maly orchestrator Qwen 0.8B + Whisper STT + sherpa TTS — uzywane
/// w jarvis voice loop, musza byc dostepne natychmiast. User moze nadpisac
/// (toggle Pin w Services).
fn is_orchestrator_model(engine_id: &str, model_repo: &str) -> bool {
    let m = model_repo.to_ascii_lowercase();
    let e = engine_id.to_ascii_lowercase();
    // Maly Qwen 0.8B jako orchestrator (jakikolwiek backend).
    if m.contains("qwen3.5-0.8b") || m.contains("qwen3-5-0-8b") {
        return true;
    }
    // Whisper STT i sherpa TTS — pinned bo jarvis voice loop.
    if e == "whisper" || e == "sherpa-onnx" {
        return true;
    }
    false
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
) -> Result<i64> {
    let existing = crate::db::repository::list_services(db)?
        .into_iter()
        .find(|svc| svc.name == service_name);

    // Schema `services.status` CHECK: 'active','disabled','maintenance','on_demand'.
    // Nowy/restartowany deployment → 'active'. Runtime health (czy proces zyje,
    // czy port odpowiada) jest osobnym sygnalem w service_manager, nie status w DB.
    let row_id = if let Some(existing) = existing {
        crate::db::repository::update_service(
            db,
            existing.id,
            service_name,
            service_type,
            strategy,
            model_category,
            "active",
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
            "active",
            config_json,
        )?;
        id
    };

    if !node_id.is_empty() {
        crate::db::repository::set_service_node_id(db, row_id, Some(node_id))?;
    }

    Ok(row_id)
}

/// Auto-wiazanie aliasu teams-* do swiezo zdeployowanego serwisu, jezeli alias
/// ma puste `target_model` (defaultowy stan po `ensure_teams_bot_defaults`).
/// Bez tego router nie umie zresolvowac `teams-stt` -> konkretny model i bot
/// dostaje "Mesh STT serwis nie polaczony". User wciaz moze zmienic w UI.
fn auto_bind_teams_alias_if_empty(
    db: &DbPool,
    alias_name: &str,
    target_service: &str,
    router: &Arc<crate::routing::router::Router>,
) {
    use crate::db::repository;
    let alias = match repository::resolve_model_alias(db, alias_name) {
        Ok(Some(a)) => a,
        _ => return,
    };
    let current = alias.target_model.trim();
    if !current.is_empty() {
        // Nadpisuj tylko jezeli aktualny target jest pusty albo wskazuje na
        // serwis ktory NIE istnieje w DB (np. "system" zostawione po starym
        // apple-tts deployu, albo nazwa serwisu usunietego rezcznie z bazy).
        // Jezeli serwis o takiej nazwie istnieje — szanuj wybor uzytkownika.
        let services_in_db = repository::list_services(db).unwrap_or_default();
        let target_exists = services_in_db.iter().any(|s| s.name == current);
        if target_exists {
            return;
        }
        tracing::info!(
            alias = %alias_name,
            stale_target = %current,
            new_target = %target_service,
            "auto_bind_teams_alias: alias wskazywal na nieistniejacy serwis — nadpisuje"
        );
    }
    if let Err(e) = repository::update_model_alias(
        db,
        alias.id,
        &alias.alias,
        target_service,
        true,
        alias.fallback_targets.as_deref(),
        alias.strategy.as_deref(),
    ) {
        tracing::warn!("auto_bind_teams_alias({}): {}", alias_name, e);
        return;
    }
    // Wymus przeladowanie alias_cache w routerze (in-memory) zeby zmiana
    // dzialala od razu, a nie po nastepnym sync z DB.
    router.reload_alias_cache();
    tracing::info!(
        "Auto-wiazany alias '{}' -> '{}' (deployed STT/TTS service)",
        alias_name,
        target_service
    );
}

/// Rejestruje wpis w `model_registry` dla nowo zdeployowanego embedded silnika.
/// Wczesniej deploy embedded (Whisper, Apple TTS, Kokoro, llama.cpp...) tworzyl
/// tylko `services` row — w panelu "Modele" silnik byl niewidoczny mimo ze w
/// "Serwisach" istnial. Po tej funkcji model pojawia sie w obu zakladkach.
/// Idempotentne: jezeli wpis o `model_name` juz istnieje, nie duplikuje.
fn ensure_model_registry_entry(
    db: &DbPool,
    model_name: &str,
    display_name: &str,
    service_type: &str,
    service_id: i64,
    config_json: &str,
) {
    use crate::db::models::NewModelEntry;
    // Re-deploy po wczesniejszym delete: model_registry sierota (service_id=NULL)
    // musi byc przepiety na nowy service_id. Jezeli wpis juz wskazuje na ten
    // sam service_id — relink jest no-op (idempotencja przez UPDATE).
    match crate::db::repository::get_model_by_name(db, model_name) {
        Ok(Some(_)) => {
            if let Err(e) = crate::db::repository::relink_model_entry_to_service(
                db, model_name, service_id, service_type, config_json,
            ) {
                tracing::warn!(
                    "ensure_model_registry_entry({}): relink failed: {}",
                    model_name,
                    e
                );
            }
            return;
        }
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(
                "ensure_model_registry_entry({}): get_model_by_name: {}",
                model_name,
                e
            );
            return;
        }
    }
    let params = NewModelEntry {
        model_name,
        display_name: Some(display_name),
        service_type,
        connection_type: "internal",
        service_id: Some(service_id),
        flow_id: None,
        is_public: true,
        config_json,
    };
    if let Err(e) = crate::db::repository::create_model_entry(db, &params) {
        tracing::warn!(
            "ensure_model_registry_entry({}): {}",
            model_name,
            e
        );
    }
}

/// Rejestruje HTTP backend (OpenAI-compatible) dla natywnie uruchomionego
/// silnika python-bundle (vllm, vllm-metal, sglang, xtts itd.). Zapisuje
/// rekord w `service_backends` + live rejestracja w ServiceManager zeby router
/// potrafil dispatche'owac /v1/chat/completions, /v1/embeddings itd. do
/// procesu na 127.0.0.1:<port>. Analog do auto_register::... dla docker.
fn register_native_http_backend(
    db: &DbPool,
    service_manager: &Arc<ServiceManager>,
    service_id: i64,
    service_name: &str,
    port: u16,
    model_override: Option<&str>,
) -> Result<()> {
    use crate::config::{ConnectionType, ServiceBackend};
    use crate::db::models::NewBackend;
    use crate::routing::backend::BackendClient;

    let base_url = format!("http://127.0.0.1:{}/v1", port);
    let backend_config = serde_json::json!({ "url": base_url.clone() }).to_string();

    // Idempotencja: jesli ten sam service_id juz ma backend z tym samym URL,
    // pomin insert (zdarza sie po ponownym deploy tej samej instancji).
    let existing = crate::db::repository::list_backends_for_service(db, service_id)
        .unwrap_or_default();
    let already = existing.iter().any(|b| b.config_json.contains(&base_url));

    if !already {
        let new_backend = NewBackend {
            service_id,
            connection_type: "openai_api",
            config_json: &backend_config,
            max_concurrent: 50,
            timeout_ms: 600_000,
            weight: 1,
            model_name_override: model_override,
            health_check_path: Some("/v1/models"),
        };
        crate::db::repository::create_backend(db, &new_backend)?;
    }

    let sb = ServiceBackend {
        connection: ConnectionType::OpenAIApi {
            url: base_url,
            // Lokalne silniki OSS (vllm, vllm-metal, sglang) nie wymagaja auth,
            // ale BackendClient::new wymaga *jakiegos* api_key do zbudowania
            // `Bearer ...` headera. Dummy token — backend go ignoruje.
            api_key: Some("sk-tentaflow-local".to_string()),
            api_key_env: None,
            extra_headers: vec![],
            custom_endpoint: None,
            request_format: None,
            tts_config: None,
        },
        max_concurrent: 50,
        timeout_ms: 600_000,
        weight: 1,
        // vLLM zna model pod HF repo name (np. "Qwen/Qwen3.5-0.8B"), GUI
        // dispatchuje pod service name (np. "tentaflow-vllm-metal-2izlb").
        // Override podmienia nazwe tuz przed wyslaniem requestu do silnika.
        model_name_override: model_override.map(String::from),
        health_check_path: Some("/v1/models".to_string()),
    };

    let client = BackendClient::new(sb, None).context("BackendClient::new for native python-bundle")?;
    service_manager.register_dynamic_http_backend(service_name, Arc::new(client));
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
    service_name: &str,
    engine_id: &str,
    category: &str,
    container_name: &str,
    host_port: u16,
    deployed_model: Option<&str>,
) -> Option<i64> {
    // Wpis do tabeli services żeby startup restore_services mógł restaurować.
    // `deployed_model` jest KONIECZNE: ServiceManager::init_model_pool po
    // restarcie czyta to pole z services.config_json zeby przywrocic mapping
    // model_name -> service_name. Bez tego zakladka Models w GUI jest pusta
    // i router nie potrafi wyrouter chat completion po nazwie modelu HF.
    let mut cfg = serde_json::json!({
        "deploy_mode": "docker",
        "image": format!("tentaflow/{}:latest", engine_id),
        "manifest_engine_id": engine_id,
        "port": host_port,
        "container_name": container_name,
    });
    if let Some(m) = deployed_model {
        if !m.is_empty() {
            cfg["deployed_model"] = serde_json::Value::String(m.to_string());
        }
    }
    let config_json = cfg.to_string();
    match crate::db::repository::create_service(
        db,
        service_name,
        service_type_from_category(category),
        "first_available",
        Some(category),
        &config_json,
    ) {
        Ok(id) => Some(id),
        Err(e) => {
            warn!("create_service '{}': {}", service_name, e);
            None
        }
    }
}

/// Rejestruje QUIC backend silnika w ServiceManager. Identyfikator iroh peera
/// (`endpoint_id_hex`) pochodzi z klucza wygenerowanego przez `provision_docker_sidecar`,
/// a `host_port` jest mapowanym portem UDP `5000/udp` w kontenerze. Direct addr
/// `127.0.0.1:<host_port>` jest niezbedny — kontener Dockera nie jest discoverable
/// przez LAN mDNS/DHT hosta.
pub(crate) fn register_docker_quic_service(
    service_manager: &Arc<ServiceManager>,
    service_name: &str,
    category: &str,
    endpoint_id_hex: &str,
    host_port: u16,
) {
    let service_type = match category {
        "llm" => "llm",
        "stt" => "stt",
        "tts" => "tts",
        "embeddings" => "embedding",
        _ => return,
    };
    service_manager.register_quic_service_with_addrs(
        service_name.to_string(),
        service_type,
        endpoint_id_hex.to_string(),
        None,
        None,
        vec![format!("127.0.0.1:{}", host_port)],
    );
}

/// Wynik provisioning'u sidecara: katalog z kluczem + configiem (mountowany do
/// `/data` w kontenerze) oraz hex `EndpointId` ktorym ServiceManager pingnie peera.
pub(crate) struct SidecarProvision {
    pub dir: std::path::PathBuf,
    pub endpoint_id_hex: String,
}

/// Generuje (albo re-uzywa) Ed25519 secret key dla sidecara i zapisuje obok
/// `config.toml` opisujacy role ReverseProxy do silnika. Reuse klucza miedzy
/// redeployami jest istotny — zmiana `EndpointId` rozspojnia rejestracje w
/// ServiceManager i wymusza pelny reconnect cycle.
pub(crate) fn provision_docker_sidecar(
    service_name: &str,
    engine_id: &str,
    engine_internal_port: u16,
    model_repo: Option<&str>,
) -> Result<SidecarProvision> {
    let sidecar_dir = crate::paths::tentaflow_home()
        .join("sidecar-keys")
        .join(service_name);
    std::fs::create_dir_all(&sidecar_dir)
        .with_context(|| format!("mkdir sidecar dir {}", sidecar_dir.display()))?;

    let key_path = sidecar_dir.join("endpoint-key.bin");
    let secret = if key_path.exists() {
        let bytes = std::fs::read(&key_path)
            .with_context(|| format!("read sidecar key {}", key_path.display()))?;
        if bytes.len() != 32 {
            return Err(anyhow!(
                "plik klucza {} ma {} bajtow, wymagane 32",
                key_path.display(),
                bytes.len()
            ));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        iroh::SecretKey::from_bytes(&arr)
    } else {
        let s = iroh::SecretKey::generate();
        std::fs::write(&key_path, s.to_bytes())
            .with_context(|| format!("write sidecar key {}", key_path.display()))?;
        s
    };
    let endpoint_id_hex = hex::encode(secret.public().as_bytes());

    let upstream_api = match engine_id {
        "llama-cpp" => "llama_cpp",
        "sherpa-onnx" => "sherpa",
        "vllm" | "sglang" | "xtts" | "voxcpm" | "parakeet" | "qwen-asr" | "comfyui"
        | "whisper" | "ollama" => "open_ai",
        _ => "raw_http",
    };
    // OpenAI-kompatybilne backendy maja prefix /v1, llama.cpp i sherpa wystawiaja
    // wlasne sciezki na rooty - sidecar dba o tlumaczenie. Upstream dziala w tej
    // samej network namespace co sidecar (loopback).
    let upstream_url = if upstream_api == "open_ai" {
        format!("http://127.0.0.1:{}/v1", engine_internal_port)
    } else {
        format!("http://127.0.0.1:{}", engine_internal_port)
    };

    let aliases_toml = match model_repo {
        Some(repo) => format!("model_aliases = [\"{}\"]\n", repo.replace('"', "\\\"")),
        None => "model_aliases = []\n".to_string(),
    };
    // enable_lan_discovery / enable_dht_discovery = false: kontener jest lokalny,
    // ServiceManager dial'uje przez explicit direct_addrs (127.0.0.1:host_port),
    // wiec mDNS/DHT ze srodka kontenera tylko marnuja CPU i mogliby reklamowac
    // peera na zewnatrz hosta.
    let config_toml = format!(
        "service_name = \"{}\"\n\
         {}\n\
         [transport]\n\
         port = 5000\n\
         secret_key_path = \"/data/endpoint-key.bin\"\n\
         enable_lan_discovery = false\n\
         enable_dht_discovery = false\n\
         \n\
         [role]\n\
         kind = \"reverse_proxy\"\n\
         upstream_url = \"{}\"\n\
         api = \"{}\"\n\
         timeout_ms = 600000\n",
        service_name.replace('"', "\\\""),
        aliases_toml,
        upstream_url,
        upstream_api,
    );
    let config_path = sidecar_dir.join("config.toml");
    std::fs::write(&config_path, config_toml)
        .with_context(|| format!("write sidecar config {}", config_path.display()))?;

    Ok(SidecarProvision {
        dir: sidecar_dir,
        endpoint_id_hex,
    })
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

use log_bus::{finish_success, log_line, progress};
// Wraps `log_bus::phase` so the runner also emits an `info!` span — helpful
// when tailing tentaflow logs. Other callers can use `log_bus::phase` directly.
fn phase(
    db: &DbPool,
    deploy_id: &str,
    tx: &broadcast::Sender<BusMessage>,
    status: &str,
    pct: u32,
    phase_name: &str,
) {
    log_bus::phase(db, deploy_id, tx, status, pct, phase_name);
    info!(deploy_id = %deploy_id, status = %status, phase = %phase_name, pct, "deployment phase");
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

/// Auto-detect liczby GPU + VRAM, fetch HF config dla modelu, smart-pick
/// TP/PP/ctx/seqs/kv_dtype zgodnie z architektura modelu i dostepnym sprzetem.
/// Wynik: gotowy VLLM_ARGS string do wstrzykniecia w env bundle/docker.
/// Zwraca None gdy brak GPU lub HF config niedostepny (caller uzyje
/// bundle.toml defaultow).
pub(crate) async fn build_auto_vllm_args(
    model_repo: &str,
    gpu_ids_filter: Option<&[String]>,
    db: &DbPool,
    settings_cipher: &Arc<crate::crypto::SettingsCipher>,
) -> Result<Option<String>> {
    use crate::deploy::vram_calculator::{
        build_vllm_args_string, estimate_vllm_vram, fetch_hf_config, max_concurrent_seqs_for_budget,
        max_context_for_budget, parse_hf_config, recommend_parallelism, VramEstimateInput,
    };

    // Wykryj GPU lokalne. detect_gpus_cached() jest cached 60s wiec nie
    // robi nvidia-smi przy kazdym deploy.
    let all_gpus = crate::mesh::node_info_collector::detect_gpus_cached();
    let nvidia: Vec<&crate::mesh::peer_store::PeerGpuInfo> = all_gpus
        .iter()
        .filter(|g| g.vram_total_mb > 0)
        .collect();
    if nvidia.is_empty() {
        return Ok(None);
    }

    // Filter po wybranych GPU_IDS gdy user wybral konkretne (gpu_select_mode=specific).
    let selected: Vec<&crate::mesh::peer_store::PeerGpuInfo> = match gpu_ids_filter {
        Some(ids) if !ids.is_empty() => {
            let id_set: std::collections::HashSet<u32> = ids
                .iter()
                .filter_map(|s| s.parse::<u32>().ok())
                .collect();
            nvidia.iter().enumerate()
                .filter(|(idx, _)| id_set.contains(&(*idx as u32)))
                .map(|(_, g)| *g)
                .collect()
        }
        _ => nvidia,
    };
    if selected.is_empty() {
        return Ok(None);
    }

    let gpu_count = selected.len() as u32;
    let gpu_memory_gb_each = selected
        .iter()
        .map(|g| g.vram_total_mb as f64 / 1024.0)
        .fold(f64::INFINITY, f64::min);

    // HF token dla gated modeli (Llama, Gemma niektore wersje).
    let hf_token = repository::get_setting_secure(db, "hf_token", settings_cipher)
        .unwrap_or_default()
        .unwrap_or_default();
    let token_opt = if hf_token.is_empty() { None } else { Some(hf_token.as_str()) };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .ok();
    let client = match client {
        Some(c) => c,
        None => return Ok(None),
    };

    let cfg_json = fetch_hf_config(&client, model_repo, token_opt).await?;
    let spec = parse_hf_config(&cfg_json, model_repo)?;

    let (tp, pp) = recommend_parallelism(&spec, gpu_count);

    // Defaulty dla initial deploy: ctx 8192 (lub model max jesli mniej),
    // batch 16, fp8 KV jesli model duzy (>20B param), gpu_mem_util 0.9.
    let initial_ctx = spec
        .max_position_embeddings
        .min(8192)
        .max(2048);
    let estimated_b = spec.estimated_params() as f64 / 1_000_000_000.0;
    let kv_dtype = if estimated_b > 20.0 { "fp8" } else { "auto" };

    let mut input = VramEstimateInput {
        gpu_count,
        gpu_memory_gb_each,
        tensor_parallel: tp,
        pipeline_parallel: pp,
        max_model_len: initial_ctx,
        max_num_seqs: 16,
        kv_cache_dtype: kv_dtype.into(),
        gpu_memory_utilization: 0.9,
        activation_overhead_pct: 10.0,
    };

    // Sprawdz czy fits. Jesli nie - obetnij ctx+seqs do max ktore fits.
    let est = estimate_vllm_vram(&spec, &input);
    if !est.fits_per_gpu {
        // Spróbuj zmniejszyć batch do 4
        input.max_num_seqs = 4;
        let est2 = estimate_vllm_vram(&spec, &input);
        if !est2.fits_per_gpu {
            // Ogranicz ctx do max ktore fits
            let max_ctx = max_context_for_budget(&spec, &input);
            if max_ctx >= 1024 {
                input.max_model_len = max_ctx;
            } else {
                // Nawet 1k ctx nie fits - return None, zostaw bundle defaults
                return Ok(None);
            }
        }
    }

    // Final adjust: max_num_seqs do limitu fits (na wypadek gdy ctx zmalal).
    let max_seqs = max_concurrent_seqs_for_budget(&spec, &input);
    if max_seqs < input.max_num_seqs {
        input.max_num_seqs = max_seqs.max(1);
    }

    Ok(Some(build_vllm_args_string(&spec, &input)))
}

/// Czyta /proc/<pid>/status -> VmRSS dla heartbeat. Linux only; macOS i
/// inne os zwracaja None i heartbeat pokazuje tylko PID.
fn read_proc_metrics(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let path = format!("/proc/{}/status", pid);
        let content = std::fs::read_to_string(&path).ok()?;
        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                let trimmed = rest.trim();
                let kb: u64 = trimmed.split_whitespace().next()?.parse().ok()?;
                let mb = kb / 1024;
                return Some(format!("{} MB ({:.1} GB)", mb, mb as f64 / 1024.0));
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

/// Snapshot nvidia-smi do heartbeat - GPU index, util%, memory used. Cached
/// 5s zeby nie spawnowac procesu co petle. Zwraca None gdy nvidia-smi brak
/// (macOS, AMD-only).
fn nvidia_smi_snapshot() -> Option<String> {
    use std::process::Command;
    use std::time::{Duration, Instant};
    use std::sync::Mutex;
    static CACHE: std::sync::OnceLock<Mutex<(Instant, Option<String>)>> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new((Instant::now() - Duration::from_secs(60), None)));
    {
        let guard = cache.lock().ok()?;
        if guard.0.elapsed() < Duration::from_secs(5) {
            return guard.1.clone();
        }
    }
    let output = Command::new("nvidia-smi")
        .args(&[
            "--query-gpu=index,utilization.gpu,memory.used,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<String> = text
        .lines()
        .map(|l| {
            let cells: Vec<&str> = l.split(',').map(str::trim).collect();
            if cells.len() == 4 {
                format!("GPU{}:{}%{}/{}MB", cells[0], cells[1], cells[2], cells[3])
            } else {
                l.trim().to_string()
            }
        })
        .collect();
    let snap = if parts.is_empty() { None } else { Some(parts.join(" ")) };
    if let Ok(mut guard) = cache.lock() {
        *guard = (Instant::now(), snap.clone());
    }
    snap
}

/// Spawn `docker logs --follow <container>` w tle, kazda linia trafia do
/// deploy_log z prefixem [docker]. Task konczy sie automatycznie gdy kontener
/// stops (docker logs --follow zamyka stdout). Bez fail handling - log
/// streaming jest best-effort, deploy nie zalezy od tego.
fn spawn_docker_log_tailer(
    db: DbPool,
    deploy_id: String,
    tx: broadcast::Sender<BusMessage>,
    container: String,
) {
    tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, BufReader};
        use tokio::process::Command;
        let mut cmd = Command::new("docker");
        cmd.arg("logs")
            .arg("--follow")
            .arg("--tail")
            .arg("0")
            .arg(&container)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                log_line(&db, &deploy_id, &tx, "log",
                    &format!("[docker-log] spawn fail: {e}"));
                return;
            }
        };
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let db_o = db.clone();
        let dep_o = deploy_id.clone();
        let tx_o = tx.clone();
        let stdout_task = tokio::spawn(async move {
            if let Some(out) = stdout {
                let mut lines = BufReader::new(out).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let trimmed = line.trim_end();
                    if !trimmed.is_empty() {
                        log_line(&db_o, &dep_o, &tx_o, "log",
                            &format!("[docker] {}", trimmed));
                    }
                }
            }
        });
        let stderr_task = tokio::spawn(async move {
            if let Some(err) = stderr {
                let mut lines = BufReader::new(err).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let trimmed = line.trim_end();
                    if !trimmed.is_empty() {
                        log_line(&db, &deploy_id, &tx, "log",
                            &format!("[docker] {}", trimmed));
                    }
                }
            }
        });
        let _ = tokio::join!(stdout_task, stderr_task, async { let _ = child.wait().await; });
    });
}
