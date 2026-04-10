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

/// Deployuje stack — Docker CLI (lokalnie) lub MeshCommand (zdalnie)
async fn deploy_stack(
    db: &DbPool,
    cipher: &Arc<SettingsCipher>,
    req: &DeployRequest,
    _config: &DeployConfig,
    local_node_id: &str,
) -> Result<(), anyhow::Error> {
    if req.node_id == local_node_id || req.node_id.is_empty() {
        deploy_with_docker_cli(&req.stack_name, &req.compose_yaml, db, cipher).await?;
        return Ok(());
    }

    // TODO: deploy na zdalny node przez MeshCommand
    Err(anyhow::anyhow!("Deploy na zdalnym nodzie wymaga MeshCommand (jeszcze niezaimplementowane)"))
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
