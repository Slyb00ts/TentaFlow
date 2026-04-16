// =============================================================================
// Plik: api/dashboard/ws_deploy.rs
// Opis: WebSocket handler deploy — odbiera compose_yaml, deployuje stack
//       przez Portainer, po udanym deploy automatycznie rejestruje serwis.
// =============================================================================

use crate::api::dashboard::auto_register::{DeployProgress, DeployedServiceInfo, auto_register_deployed_service};
use crate::crypto::SettingsCipher;
use crate::db::DbPool;
use crate::routing::Router;

use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn, error};

/// Request deploy przychodzacy z frontendu
#[derive(Deserialize)]
struct DeployRequest {
    node_id: String,
    stack_name: String,
    compose_yaml: String,
    service_name: String,
    config_json: String,
}

/// Sparsowana konfiguracja z config_json
#[derive(Deserialize, Default)]
struct DeployConfig {
    #[serde(default)]
    engine: String,
    #[serde(default)]
    model_id: String,
    #[serde(default)]
    port: u16,
    #[serde(default)]
    container_name: String,
    #[serde(default)]
    service_type: String,
    #[serde(default)]
    image: String,
    /// Protokol polaczenia: "http" (domyslny) lub "quic"
    #[serde(default)]
    protocol: String,
    /// Tryb deployu wybrany przez wizard: "docker" (domyslny) lub "native"
    #[serde(default)]
    deploy_mode: String,
}

/// Obsluguje polaczenie WebSocket /ws/deploy
pub async fn handle_ws_connection<S>(
    stream: S,
    db: DbPool,
    cipher: Arc<SettingsCipher>,
    router: Arc<Router>,
    local_node_id: Arc<str>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let ws = WebSocketStream::from_raw_socket(
        stream,
        tokio_tungstenite::tungstenite::protocol::Role::Server,
        None,
    )
    .await;

    let (mut sink, mut stream) = ws.split();

    // Czekaj na pierwsza wiadomosc z danymi deploy
    let mut deploy_req: DeployRequest = match stream.next().await {
        Some(Ok(Message::Text(text))) => {
            match serde_json::from_str(&text) {
                Ok(req) => req,
                Err(e) => {
                    let _ = send_ws_progress(&mut sink, DeployProgress::done(false, &format!("Niepoprawny JSON: {}", e))).await;
                    return;
                }
            }
        }
        _ => {
            let _ = send_ws_progress(&mut sink, DeployProgress::done(false, "Brak danych deploy")).await;
            return;
        }
    };

    let config: DeployConfig = serde_json::from_str(&deploy_req.config_json).unwrap_or_default();

    // Sanityzacja stack name — Docker Compose wymaga lowercase alphanumeric + hyphen + underscore
    deploy_req.stack_name = deploy_req.stack_name
        .to_lowercase()
        .replace('/', "-")
        .replace('\\', "-")
        .replace(' ', "-")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect::<String>()
        .trim_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_string();

    info!(
        "Deploy request: stack='{}', node='{}', model='{}', port={}",
        deploy_req.stack_name, deploy_req.node_id, config.model_id, config.port
    );

    // Faza 1: Deploy stack przez Portainer lub lokalne Docker API
    let _ = send_ws_progress(&mut sink, DeployProgress::phase("deploying", "Wdrazanie kontenera...")).await;

    let deploy_result = deploy_stack(
        &db,
        &cipher,
        &deploy_req,
        &config,
        &local_node_id,
    ).await;

    match deploy_result {
        Ok(()) => {
            let _ = send_ws_progress(&mut sink, DeployProgress::phase("deployed", "Kontener wdrozony")).await;
        }
        Err(e) => {
            let err_str = e.to_string();
            error!("Deploy stack failed: {}", err_str);
            let user_msg = if err_str.contains("accept license") || err_str.contains("DENIED") {
                let model_id = config.model_id.replace('/', "%2F");
                format!(
                    "EULA not accepted. Go to https://build.nvidia.com/{} and click 'Get Container' to accept the license, then retry.",
                    model_id
                )
            } else if err_str.contains("401") || err_str.contains("unauthorized") {
                "NGC API key invalid or expired. Check your key in Settings.".to_string()
            } else {
                format!("Deploy failed: {}", err_str)
            };
            let _ = send_ws_progress(&mut sink, DeployProgress::done(false, &user_msg)).await;
            return;
        }
    }

    // Faza 2: Auto-rejestracja serwisu
    let port = if config.port > 0 { config.port } else { 8000 };
    let service_type = if config.service_type.is_empty() { "llm".to_string() } else { config.service_type.clone() };

    let protocol = if config.protocol.is_empty() { "http".to_string() } else { config.protocol.clone() };

    let info = DeployedServiceInfo {
        service_name: deploy_req.service_name.clone(),
        service_type,
        port,
        deployed_model: if config.model_id.is_empty() { None } else { Some(config.model_id.clone()) },
        node_id: deploy_req.node_id.clone(),
        node_ip: None,
        protocol,
    };

    // Kanal postepu — przekazuj do WebSocket
    let (progress_tx, mut progress_rx) = mpsc::channel::<DeployProgress>(32);

    let pool_clone = db.clone();
    let router_clone = router.clone();
    let info_clone = info.clone();

    let register_handle = tokio::spawn(async move {
        auto_register_deployed_service(pool_clone, router_clone, info_clone, Some(progress_tx)).await
    });

    // Przekazuj progress do WebSocket
    while let Some(progress) = progress_rx.recv().await {
        if send_ws_progress(&mut sink, progress).await.is_err() {
            break;
        }
    }

    // Czekaj na wynik rejestracji
    match register_handle.await {
        Ok(Ok(model_name)) => {
            info!("Auto-rejestracja zakonczona: {}", model_name);
        }
        Ok(Err(e)) => {
            warn!("Auto-rejestracja nieudana: {}", e);
            let _ = send_ws_progress(&mut sink, DeployProgress::done(false, &format!("Rejestracja nieudana: {}", e))).await;
        }
        Err(e) => {
            error!("Auto-rejestracja panic: {}", e);
            let _ = send_ws_progress(&mut sink, DeployProgress::done(false, "Blad wewnetrzny")).await;
        }
    }
}

/// Mapuje nazwe silnika z wizarda na nazwe embedowanego kontenera w bundle.
/// Gdy zwraca Some — uzywamy build z embedded bundle (bollard) zamiast pull
/// z registry. Gdy None — fallback do starego compose CLI z compose_yaml.
fn engine_to_bundle_name(engine: &str, service_type: &str) -> Option<&'static str> {
    match engine {
        "sglang" => Some("llm-sglang"),
        "vllm" => Some("llm-vllm"),
        "ollama" => Some("llm-ollama"),
        "llamacpp" => Some("llm-llamacpp"),
        "whisper" | "faster-whisper" => Some("stt-whisper"),
        "parakeet" => Some("stt-parakeet"),
        "qwen-asr" => Some("stt-qwen-asr"),
        "sherpa" => Some("tts-sherpa"),
        "xtts" => Some("tts-xtts"),
        "voxcpm" => Some("tts-voxcpm"),
        "comfyui" => Some("comfyui"),
        _ => match service_type {
            "embeddings" | "embedding" => Some("embeddings"),
            "reranker" | "rerank" => Some("reranker"),
            "tts" => Some("tts-sherpa"),
            "stt" => Some("stt-whisper"),
            _ => None,
        },
    }
}

/// Deployuje stack — Docker CLI (lokalnie) lub MeshCommand (zdalnie).
/// Jesli silnik ma odpowiednik w embedowanym bundle, builduje obraz z bundle
/// zamiast pullowac z registry.
async fn deploy_stack(
    db: &DbPool,
    cipher: &Arc<SettingsCipher>,
    req: &DeployRequest,
    config: &DeployConfig,
    local_node_id: &str,
) -> Result<(), anyhow::Error> {
    if req.node_id == local_node_id || req.node_id.is_empty() {
        // Tryb native — Pythonowy bundle bez Dockera (vLLM/SGLang/XTTS/...)
        if config.deploy_mode == "native" {
            return deploy_native_python(req, config).await;
        }

        // Sprobuj uzyc embedowanego bundle dla znanych silnikow (Docker)
        if let Some(bundle_name) = engine_to_bundle_name(&config.engine, &config.service_type) {
            info!(
                engine = %config.engine,
                bundle = bundle_name,
                "Deploy z embedowanego bundle (zamiast registry)"
            );
            return deploy_bundled_container(bundle_name, req, config).await;
        }

        // Fallback: stary flow z compose_yaml + registry pull
        deploy_with_docker_cli(&req.stack_name, &req.compose_yaml, db, cipher).await?;
        return Ok(());
    }

    // TODO: deploy na zdalny node przez MeshCommand
    Err(anyhow::anyhow!("Deploy na zdalnym nodzie wymaga MeshCommand (jeszcze niezaimplementowane)"))
}

/// Buduje obraz z embedowanego kontekstu Dockera i uruchamia kontener.
/// Wszystkie parametry (porty, env, volumes, GPU id, container_name) sa
/// wyciagane z `compose_yaml` ktory wizard wygenerowal — dzieki temu wybor
/// uzytkownika (gpuId, hfToken, modelId, shmSize, dataDir itp.) nie ginie.
#[cfg(feature = "docker")]
async fn deploy_bundled_container(
    bundle_name: &str,
    req: &DeployRequest,
    config: &DeployConfig,
) -> Result<(), anyhow::Error> {
    use std::collections::HashMap;

    let parsed = parse_compose_for_bundle(&req.compose_yaml).unwrap_or_default();

    // Porty: priorytet z compose_yaml, fallback do config.port + 5000/udp
    let mut ports = parsed.ports;
    if ports.is_empty() {
        let port = if config.port > 0 { config.port } else { 5000 };
        let proto_suffix = if config.protocol == "quic" { "/udp" } else { "/tcp" };
        ports.push((format!("{}", port), format!("5000{}", proto_suffix)));
    }

    // Env: laczymy compose_yaml + MODEL/MODEL_ID z config.model_id
    let mut env: HashMap<String, String> = parsed.env;
    if !config.model_id.is_empty() {
        env.entry("MODEL".to_string()).or_insert_with(|| config.model_id.clone());
        env.entry("MODEL_ID".to_string()).or_insert_with(|| config.model_id.clone());
    }

    let instance_name = if !parsed.container_name.is_empty() {
        Some(parsed.container_name)
    } else if !req.stack_name.is_empty() {
        Some(req.stack_name.clone())
    } else if !config.container_name.is_empty() {
        Some(config.container_name.clone())
    } else {
        None
    };

    // One shared /data/models mount — Docker and native deploys both point
    // HF at the same root so downloads (Bielik, Llama etc.) happen once.
    let _ = crate::paths::ensure_models_dirs();
    let models_root = crate::paths::models_root();
    let container_path = crate::paths::CONTAINER_MODELS_PATH; // "/data/models"
    let mut volumes = parsed.volumes;
    if !volumes.iter().any(|(_, c)| c == container_path) {
        volumes.push((models_root.display().to_string(), container_path.to_string()));
    }
    // HF_HOME points AT the mount root; HF itself manages `hub/models--*`.
    // TORCH_HOME gets a subdir so torch's `hub/` can't collide with HF's.
    env.entry("HF_HOME".into()).or_insert_with(|| container_path.to_string());
    env.entry("HUGGINGFACE_HUB_CACHE".into()).or_insert_with(|| container_path.to_string());
    env.entry("TRANSFORMERS_CACHE".into()).or_insert_with(|| container_path.to_string());
    env.entry("TORCH_HOME".into()).or_insert_with(|| format!("{}/torch", container_path));

    let deploy_req = crate::deploy::docker::DeployRequest {
        container: bundle_name.to_string(),
        image_tag: Some(format!("tentaflow/{}:latest", bundle_name)),
        instance_name,
        ports,
        volumes,
        env,
        gpu: parsed.gpu,
    };

    let _ = crate::deploy::docker::deploy(&deploy_req).await?;
    Ok(())
}

/// Sparsowane parametry z compose_yaml (dla bundled deploy).
#[cfg(feature = "docker")]
#[derive(Default)]
struct ComposeParsed {
    container_name: String,
    ports: Vec<(String, String)>,
    volumes: Vec<(String, String)>,
    env: std::collections::HashMap<String, String>,
    gpu: bool,
}

/// Wyciaga z compose_yaml pierwszy serwis i czyta z niego porty/volumes/env/gpu.
/// Compose generowany przez ComposeTemplates.js ma stala strukture.
#[cfg(feature = "docker")]
fn parse_compose_for_bundle(yaml: &str) -> Option<ComposeParsed> {
    use serde_yaml::Value;
    use std::collections::HashMap;

    let root: Value = serde_yaml::from_str(yaml).ok()?;
    let services = root.get("services")?.as_mapping()?;
    let (_svc_name, svc) = services.iter().next()?;

    let mut out = ComposeParsed::default();

    if let Some(cn) = svc.get("container_name").and_then(|v| v.as_str()) {
        out.container_name = cn.to_string();
    }

    if let Some(ports) = svc.get("ports").and_then(|v| v.as_sequence()) {
        for p in ports {
            let s = p.as_str().unwrap_or("").trim();
            // format "HOST:CONTAINER" lub "HOST:CONTAINER/proto"
            let (host, rest) = match s.split_once(':') {
                Some((h, r)) => (h.trim().to_string(), r.trim().to_string()),
                None => continue,
            };
            let container = if rest.contains('/') { rest } else { format!("{}/tcp", rest) };
            out.ports.push((host, container));
        }
    }

    if let Some(vols) = svc.get("volumes").and_then(|v| v.as_sequence()) {
        for v in vols {
            let s = v.as_str().unwrap_or("").trim();
            // format "HOST:CONTAINER" lub "HOST:CONTAINER:ro"
            let parts: Vec<&str> = s.splitn(3, ':').collect();
            if parts.len() >= 2 {
                out.volumes.push((parts[0].to_string(), parts[1].to_string()));
            }
        }
    }

    if let Some(envs) = svc.get("environment").and_then(|v| v.as_sequence()) {
        let mut map = HashMap::new();
        for e in envs {
            let s = e.as_str().unwrap_or("").trim().trim_start_matches("- ").trim();
            if let Some((k, v)) = s.split_once('=') {
                map.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
        out.env = map;
    }

    // GPU: szukamy deploy.resources.reservations.devices[].driver==nvidia
    // Jesli jest device_ids: ['0'] przekazujemy je jako NVIDIA_VISIBLE_DEVICES
    // (bollard --gpus all = wszystkie, env zawez do konkretnych kart).
    if let Some(deploy) = svc.get("deploy") {
        let devices_opt = deploy
            .get("resources")
            .and_then(|r| r.get("reservations"))
            .and_then(|r| r.get("devices"))
            .and_then(|d| d.as_sequence());
        if let Some(devices) = devices_opt {
            for d in devices {
                if d.get("driver").and_then(|v| v.as_str()) == Some("nvidia") {
                    out.gpu = true;
                    if let Some(ids) = d.get("device_ids").and_then(|v| v.as_sequence()) {
                        let ids_str: Vec<String> = ids
                            .iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect();
                        if !ids_str.is_empty() {
                            out.env
                                .entry("NVIDIA_VISIBLE_DEVICES".to_string())
                                .or_insert_with(|| ids_str.join(","));
                        }
                    }
                }
            }
        }
    }

    // shm_size jako env (bollard nie ma osobnego pola, pass through dla referencji)
    if let Some(shm) = svc.get("shm_size").and_then(|v| v.as_str()) {
        out.env.entry("SHM_SIZE".to_string()).or_insert_with(|| shm.to_string());
    }

    Some(out)
}

#[cfg(all(test, feature = "docker"))]
mod tests {
    use super::*;

    const SAMPLE_VLLM_YAML: &str = r#"
services:
  tentaflow-llm:
    image: registry.nextapp.pl/tentaflow-llm-vllm:latest
    container_name: tentaflow-llm
    restart: unless-stopped
    ports:
      - "5010:5010"
      - "5010:5000/udp"
    environment:
      - HF_TOKEN=hf_xxxxx
      - MODEL_ID=meta-llama/Llama-3.1-8B
      - GPU_MEMORY_UTILIZATION=0.9
    volumes:
      - /opt/tentaflow/llm/tentaflow-llm:/data
      - /opt/tentaflow/certs:/data/certs:ro
      - /opt/tentaflow/models:/app/models
    shm_size: '16g'
    deploy:
      resources:
        reservations:
          devices:
            - driver: nvidia
              device_ids: ['0']
              capabilities: [gpu]
networks:
  tentaflow-ai:
    name: tentaflow-ai
"#;

    #[test]
    fn parse_compose_extracts_all_wizard_fields() {
        let p = parse_compose_for_bundle(SAMPLE_VLLM_YAML).expect("parse");
        assert_eq!(p.container_name, "tentaflow-llm");
        assert_eq!(p.ports.len(), 2);
        let pair_a: (String, String) = ("5010".into(), "5010/tcp".into());
        let pair_b: (String, String) = ("5010".into(), "5000/udp".into());
        assert!(p.ports.contains(&pair_a));
        assert!(p.ports.contains(&pair_b));
        assert_eq!(p.volumes.len(), 3);
        assert_eq!(p.env.get("HF_TOKEN").unwrap(), "hf_xxxxx");
        assert_eq!(p.env.get("MODEL_ID").unwrap(), "meta-llama/Llama-3.1-8B");
        assert_eq!(p.env.get("GPU_MEMORY_UTILIZATION").unwrap(), "0.9");
        assert_eq!(p.env.get("NVIDIA_VISIBLE_DEVICES").unwrap(), "0");
        assert_eq!(p.env.get("SHM_SIZE").unwrap(), "16g");
        assert!(p.gpu);
    }

    #[test]
    fn parse_compose_handles_multiple_device_ids() {
        // Wizard z multi-select: 2 z 6 GPU wybrane (0 i 4)
        let yaml = r#"
services:
  llm:
    image: x
    container_name: x
    deploy:
      resources:
        reservations:
          devices:
            - driver: nvidia
              device_ids: ['0', '4']
              capabilities: [gpu]
"#;
        let p = parse_compose_for_bundle(yaml).expect("parse");
        assert!(p.gpu);
        assert_eq!(p.env.get("NVIDIA_VISIBLE_DEVICES").unwrap(), "0,4");
    }

    #[test]
    fn parse_compose_handles_gpu_all() {
        let yaml = r#"
services:
  tts:
    image: x
    container_name: x
    deploy:
      resources:
        reservations:
          devices:
            - driver: nvidia
              count: all
              capabilities: [gpu]
"#;
        let p = parse_compose_for_bundle(yaml).expect("parse");
        assert!(p.gpu);
        assert!(!p.env.contains_key("NVIDIA_VISIBLE_DEVICES"));
    }

    #[test]
    fn engine_to_bundle_covers_all_wizard_engines() {
        assert_eq!(engine_to_bundle_name("vllm", ""), Some("llm-vllm"));
        assert_eq!(engine_to_bundle_name("sglang", ""), Some("llm-sglang"));
        assert_eq!(engine_to_bundle_name("ollama", ""), Some("llm-ollama"));
        assert_eq!(engine_to_bundle_name("llamacpp", ""), Some("llm-llamacpp"));
        assert_eq!(engine_to_bundle_name("whisper", ""), Some("stt-whisper"));
        assert_eq!(engine_to_bundle_name("", "embeddings"), Some("embeddings"));
        assert_eq!(engine_to_bundle_name("", "reranker"), Some("reranker"));
        assert_eq!(engine_to_bundle_name("", "tts"), Some("tts-sherpa"));
        assert_eq!(engine_to_bundle_name("mlx", ""), None); // native, nie z bundle
    }
}

#[cfg(not(feature = "docker"))]
async fn deploy_bundled_container(
    _bundle_name: &str,
    _req: &DeployRequest,
    _config: &DeployConfig,
) -> Result<(), anyhow::Error> {
    Err(anyhow::anyhow!(
        "feature 'docker' wylaczone — embed bundle deploy niedostepny"
    ))
}

/// Mapuje silnik wizarda na nazwe bundla Pythona w `tentaflow-containers/<kategoria>/python/`.
fn engine_to_python_bundle(engine: &str) -> Option<&'static str> {
    match engine {
        "vllm"               => Some("vllm"),
        "sglang"             => Some("sglang"),
        "xtts"               => Some("xtts"),
        "voxcpm"             => Some("voxcpm"),
        "parakeet"           => Some("parakeet"),
        "qwen-asr"           => Some("qwen-asr"),
        "comfyui"            => Some("comfyui"),
        _ => None,
    }
}

/// Deploy pythonowego silnika na maszynie hosta (bez Dockera).
/// Rozpakowuje bundle, pobiera python-build-standalone + uv, tworzy venv,
/// instaluje wheels i startuje subprocess.
async fn deploy_native_python(
    req: &DeployRequest,
    config: &DeployConfig,
) -> Result<(), anyhow::Error> {
    use std::collections::HashMap;

    let bundle = engine_to_python_bundle(&config.engine)
        .ok_or_else(|| anyhow::anyhow!("silnik '{}' nie ma Python bundle", config.engine))?;

    // env przekazywane do procesu silnika — z wizarda (np. MODEL, GPU_MEMORY_UTILIZATION)
    let mut env: HashMap<String, String> = HashMap::new();
    if !config.model_id.is_empty() {
        env.insert("MODEL".into(), config.model_id.clone());
        env.insert("MODEL_ID".into(), config.model_id.clone());
    }

    // Parse compose_yaml zeby wyciagnac HF_TOKEN/GPU_MEMORY_UTILIZATION/itp.
    // (#[cfg(feature = "docker")] branch ma to zaimplementowane — reuzyj)
    #[cfg(feature = "docker")]
    if let Some(parsed) = parse_compose_for_bundle(&req.compose_yaml) {
        for (k, v) in parsed.env {
            env.entry(k).or_insert(v);
        }
    }

    let instance_name = if !req.stack_name.is_empty() {
        Some(req.stack_name.clone())
    } else if !config.container_name.is_empty() {
        Some(config.container_name.clone())
    } else {
        None
    };

    let native_req = crate::deploy::python_venv::NativeDeployRequest {
        engine: bundle.to_string(),
        instance_name,
        env,
    };

    // python_venv::deploy() jest blocking (spawnuje procesy, pobiera archiwa) —
    // uruchamiamy na blocking threadpool zeby nie blokowac tokio runtime.
    let handle = tokio::task::spawn_blocking(move || {
        crate::deploy::python_venv::deploy(&native_req)
    });
    let running = handle.await??;
    info!(
        engine = %running.engine,
        pid = running.child.id(),
        port = running.internal_port,
        venv = %running.venv_dir.display(),
        "Python bundle wystartowal"
    );
    // TODO: zapisac RunningEngine w globalnym registry zeby mozna bylo zatrzymac
    Ok(())
}

/// Deploy przez docker compose CLI (lokalnie)
async fn deploy_with_docker_cli(
    stack_name: &str,
    compose_yaml: &str,
    db: &DbPool,
    cipher: &SettingsCipher,
) -> Result<(), anyhow::Error> {
    use tokio::process::Command;

    // Pobierz NGC API key z DB i deszyfruj
    let ngc_key = crate::db::repository::get_setting_secure(db, "ngc_api_key", cipher)?
        .unwrap_or_default();

    // Zaloguj Docker do nvcr.io jesli mamy klucz NGC
    if !ngc_key.is_empty() {
        let login_output = Command::new("docker")
            .args(["login", "nvcr.io", "--username", "$oauthtoken", "--password-stdin"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn();

        if let Ok(mut child) = login_output {
            if let Some(ref mut stdin) = child.stdin {
                use tokio::io::AsyncWriteExt;
                let _ = stdin.write_all(ngc_key.as_bytes()).await;
                let _ = stdin.shutdown().await;
            }
            let output = child.wait_with_output().await?;
            if output.status.success() {
                info!("Docker zalogowany do nvcr.io");
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!("Docker login do nvcr.io nieudany: {}", stderr);
            }
        }
    }

    // Zapisz compose do tymczasowego pliku
    let tmp_dir = std::env::temp_dir().join(format!("tentaflow-deploy-{}", stack_name));
    tokio::fs::create_dir_all(&tmp_dir).await?;
    let compose_path = tmp_dir.join("docker-compose.yml");
    tokio::fs::write(&compose_path, compose_yaml).await?;

    let output = Command::new("docker")
        .args(["compose", "-f", compose_path.to_str().unwrap_or(""), "-p", stack_name, "up", "-d"])
        .env("NGC_API_KEY", &ngc_key)
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("docker compose failed: {}", stderr));
    }

    info!("Stack '{}' wdrozony przez docker compose CLI", stack_name);
    Ok(())
}

/// Wysyla progress przez WebSocket
async fn send_ws_progress<S>(sink: &mut S, progress: DeployProgress) -> Result<(), anyhow::Error>
where
    S: futures::Sink<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    let json = serde_json::to_string(&progress)?;
    sink.send(Message::Text(json.into()))
        .await
        .map_err(|e| anyhow::anyhow!("WebSocket send error: {}", e))
}
