// =============================================================================
// Plik: api/dashboard/auto_register.rs
// Opis: Automatyczna rejestracja serwisu po udanym deploy kontenera Docker.
//       Health check polling, odpytanie modeli, tworzenie service/backend w DB,
//       rejestracja w service_manager i mesh.
// =============================================================================

use crate::db::{self, DbPool};
use crate::db::models::NewBackend;
use crate::config::{ConnectionType, ServiceBackend};
use crate::routing::backend::BackendClient;
use crate::routing::Router;
use anyhow::{Context, Result};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{info, warn};

/// Parametry deploy potrzebne do auto-rejestracji
#[derive(Debug, Clone)]
pub struct DeployedServiceInfo {
    pub service_name: String,
    pub service_type: String,
    pub port: u16,
    pub deployed_model: Option<String>,
    pub node_id: String,
    pub node_ip: Option<String>,
    /// Protokol polaczenia: "http" (domyslny) lub "quic"
    pub protocol: String,
}

/// Automatycznie rejestruje serwis po udanym deploy kontenera.
/// Odpytuje health check, pobiera modele, tworzy service/backend w DB,
/// rejestruje w service_manager i mesh.
pub async fn auto_register_deployed_service(
    pool: DbPool,
    router: Arc<Router>,
    info: DeployedServiceInfo,
    progress_tx: Option<mpsc::Sender<DeployProgress>>,
) -> Result<String> {
    let is_quic = info.protocol == "quic";

    if is_quic {
        return auto_register_quic_service(pool, router, info, progress_tx).await;
    }

    let base_url = resolve_base_url(&info, &router);

    // 1. Health check polling
    send_progress(&progress_tx, DeployProgress::phase("health_check_waiting", "Oczekiwanie na kontener...")).await;
    wait_for_health(&base_url).await?;
    send_progress(&progress_tx, DeployProgress::phase("health_check_ready", "Kontener gotowy")).await;

    // 2. Odpytaj modele
    send_progress(&progress_tx, DeployProgress::phase("discovering_models", "Wykrywanie modeli...")).await;
    let model_name = discover_model(&base_url, info.deployed_model.as_deref()).await?;
    info!("Wykryty model: {}", model_name);

    // 3. Sprawdz duplikaty
    let existing = db::repository::list_services(&pool)?;
    let found = existing.iter().find(|s| s.name == info.service_name);
    if let Some(existing_svc) = found {
        info!("Serwis '{}' juz istnieje (id={}), pomijam tworzenie", info.service_name, existing_svc.id);
        send_progress(&progress_tx, DeployProgress::done(true, &format!("Serwis juz istnieje: {}", model_name))).await;
        return Ok(model_name);
    }

    // 4. Utworz service w DB
    send_progress(&progress_tx, DeployProgress::phase("registering_service", "Rejestracja serwisu...")).await;

    let config_json = serde_json::json!({
        "deployed_model": model_name,
        "deploy_mode": "docker",
        "port": info.port,
        "node_id": info.node_id,
    }).to_string();

    let service_id = db::repository::create_service(
        &pool,
        &info.service_name,
        &info.service_type,
        "single",
        None,
        &config_json,
    )?;
    info!("Utworzono serwis '{}' (id={})", info.service_name, service_id);

    // 5. Utworz backend w DB
    let backend_config = serde_json::json!({
        "url": format!("{}/v1", base_url),
    }).to_string();

    let new_backend = NewBackend {
        service_id,
        connection_type: "openai_api",
        config_json: &backend_config,
        max_concurrent: 50,
        timeout_ms: 120000,
        weight: 1,
        model_name_override: None,
        health_check_path: Some("/v1/health/ready"),
    };
    let backend_id = db::repository::create_backend(&pool, &new_backend)?;
    info!("Utworzono backend (id={}) dla serwisu '{}'", backend_id, info.service_name);

    // 6. Zarejestruj w service_manager (dynamiczny HTTP backend)
    let backend_url = format!("{}/v1", base_url);
    let service_backend = ServiceBackend {
        connection: ConnectionType::OpenAIApi {
            url: backend_url,
            api_key: None,
            api_key_env: None,
            extra_headers: vec![],
            custom_endpoint: None,
            request_format: None,
            tts_config: None,
        },
        max_concurrent: 50,
        timeout_ms: 120000,
        weight: 1,
        model_name_override: None,
        health_check_path: Some("/v1/health/ready".to_string()),
    };

    match BackendClient::new(service_backend, None) {
        Ok(client) => {
            let client = Arc::new(client);
            router.service_manager.register_dynamic_http_backend(&info.service_name, client);
        }
        Err(e) => {
            warn!("Nie udalo sie utworzyc BackendClient: {}", e);
        }
    }

    // 7. Zarejestruj w model_pool
    router.service_manager.register_model_mapping(&model_name, &info.service_name);

    // Ustaw service_type w model_pool
    {
        let mut pool_guard = router.service_manager.model_pool.write();
        if let Some(entry) = pool_guard.get_mut(&model_name) {
            entry.service_type = info.service_type.clone();
        }
    }

    // 8. Zarejestruj w mesh
    router.register_native_service_in_mesh(
        &info.service_name,
        &info.service_type,
        vec![model_name.clone()],
    );

    send_progress(&progress_tx, DeployProgress::done(true, &format!("Serwis zarejestrowany: {}", model_name))).await;
    info!("Auto-rejestracja zakonczona: serwis='{}', model='{}'", info.service_name, model_name);

    Ok(model_name)
}

/// Auto-rejestracja serwisu QUIC (np. teams-bot).
/// Zamiast HTTP health check, probuje nawiazac polaczenie QUIC.
async fn auto_register_quic_service(
    pool: DbPool,
    router: Arc<Router>,
    info: DeployedServiceInfo,
    progress_tx: Option<mpsc::Sender<DeployProgress>>,
) -> Result<String> {
    let quic_host = resolve_quic_host(&info, &router);
    let quic_url = format!("quic://{}:{}", quic_host, info.port);

    // 1. QUIC health check — probuj nawiazac polaczenie
    send_progress(&progress_tx, DeployProgress::phase("health_check_waiting", "Oczekiwanie na kontener QUIC...")).await;
    wait_for_quic_health(&quic_url).await?;
    send_progress(&progress_tx, DeployProgress::phase("health_check_ready", "Kontener QUIC gotowy")).await;

    // Nazwa modelu — dla meeting-bot uzywamy service_name
    let model_name = info.deployed_model.clone()
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| info.service_name.clone());

    // 2. Sprawdz duplikaty
    let existing = db::repository::list_services(&pool)?;
    let found = existing.iter().find(|s| s.name == info.service_name);
    if let Some(existing_svc) = found {
        info!("Serwis QUIC '{}' juz istnieje (id={}), pomijam tworzenie", info.service_name, existing_svc.id);
        send_progress(&progress_tx, DeployProgress::done(true, &format!("Serwis juz istnieje: {}", model_name))).await;
        return Ok(model_name);
    }

    // 3. Utworz service w DB
    send_progress(&progress_tx, DeployProgress::phase("registering_service", "Rejestracja serwisu QUIC...")).await;

    let config_json = serde_json::json!({
        "deployed_model": model_name,
        "deploy_mode": "docker",
        "protocol": "quic",
        "port": info.port,
        "node_id": info.node_id,
    }).to_string();

    let service_id = db::repository::create_service(
        &pool,
        &info.service_name,
        &info.service_type,
        "single",
        None,
        &config_json,
    )?;
    info!("Utworzono serwis QUIC '{}' (id={})", info.service_name, service_id);

    // 4. Utworz backend w DB (QUIC)
    let backend_config = serde_json::json!({
        "quic_url": quic_url,
        "protocol": "quic",
    }).to_string();

    let new_backend = NewBackend {
        service_id,
        connection_type: "quic",
        config_json: &backend_config,
        max_concurrent: 50,
        timeout_ms: 120000,
        weight: 1,
        model_name_override: None,
        health_check_path: None,
    };
    let backend_id = db::repository::create_backend(&pool, &new_backend)?;
    info!("Utworzono backend QUIC (id={}) dla serwisu '{}'", backend_id, info.service_name);

    // 5. Zarejestruj w service_manager jako QUIC
    router.service_manager.register_quic_service(
        info.service_name.clone(),
        &info.service_type,
        quic_url,
        None,
        None,
    );

    // 6. Zarejestruj w model_pool
    router.service_manager.register_model_mapping(&model_name, &info.service_name);
    {
        let mut pool_guard = router.service_manager.model_pool.write();
        if let Some(entry) = pool_guard.get_mut(&model_name) {
            entry.service_type = info.service_type.clone();
        }
    }

    // 7. Zarejestruj w mesh
    router.register_native_service_in_mesh(
        &info.service_name,
        &info.service_type,
        vec![model_name.clone()],
    );

    send_progress(&progress_tx, DeployProgress::done(true, &format!("Serwis QUIC zarejestrowany: {}", model_name))).await;
    info!("Auto-rejestracja QUIC zakonczona: serwis='{}', model='{}'", info.service_name, model_name);

    Ok(model_name)
}

/// Okresla host QUIC na podstawie node_id (bez schematu i portu)
fn resolve_quic_host(info: &DeployedServiceInfo, router: &Router) -> String {
    let mesh_guard = router.mesh_manager.read();
    let is_local = match mesh_guard.as_ref() {
        Some(mesh) => mesh.node_id() == info.node_id,
        None => true,
    };

    if is_local {
        "127.0.0.1".to_string()
    } else if let Some(ref ip) = info.node_ip {
        ip.clone()
    } else {
        "localhost".to_string()
    }
}

/// QUIC health check — probuje nawiazac polaczenie QUIC co 3s, max 5 minut
async fn wait_for_quic_health(quic_url: &str) -> Result<()> {
    let max_attempts = 100;
    let interval = Duration::from_secs(3);

    let config = crate::net::quic::QuicConfig {
        name: "health-check".to_string(),
        url: quic_url.to_string(),
        tls_ca: None,
        server_name: None,
        alpn: "tentaflow".to_string(),
        timeout_ms: 8000,
        auto_reconnect: false,
        reconnect_interval_ms: 0,
        keepalive_interval_ms: 0,
        skip_tls_verify: true,
    };

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    for attempt in 1..=max_attempts {
        match crate::net::quic::QuicClient::connect(config.clone(), shutdown_rx.clone()).await {
            Ok(_client) => {
                info!("QUIC health check OK po {} probach", attempt);
                let _ = shutdown_tx.send(true);
                return Ok(());
            }
            Err(e) => {
                if attempt % 10 == 0 {
                    info!("QUIC health check proba {}/{}: {}", attempt, max_attempts, e);
                }
            }
        }
        tokio::time::sleep(interval).await;
    }

    let _ = shutdown_tx.send(true);
    Err(anyhow::anyhow!("QUIC health check timeout — kontener nie odpowiedzial w ciagu 5 minut"))
}

/// Okresla bazowy URL na podstawie node_id
fn resolve_base_url(info: &DeployedServiceInfo, router: &Router) -> String {
    // Sprawdz czy to lokalny node
    let mesh_guard = router.mesh_manager.read();
    let is_local = match mesh_guard.as_ref() {
        Some(mesh) => mesh.node_id() == info.node_id,
        None => true,
    };

    if is_local {
        format!("http://localhost:{}", info.port)
    } else if let Some(ref ip) = info.node_ip {
        format!("http://{}:{}", ip, info.port)
    } else {
        // Fallback na localhost
        format!("http://localhost:{}", info.port)
    }
}

/// Polling health check — odpytuje /v1/health/ready co 3s, max 5 minut
async fn wait_for_health(base_url: &str) -> Result<()> {
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(Duration::from_secs(10))
        .build()?;

    let health_url = format!("{}/v1/health/ready", base_url);
    let max_attempts = 100; // 100 * 3s = 5 min
    let interval = Duration::from_secs(3);

    for attempt in 1..=max_attempts {
        match client.get(&health_url).send().await {
            Ok(resp) if resp.status().is_success() => {
                info!("Health check OK po {} probach", attempt);
                return Ok(());
            }
            Ok(resp) => {
                if attempt % 10 == 0 {
                    info!("Health check proba {}/{}: status {}", attempt, max_attempts, resp.status());
                }
            }
            Err(e) => {
                if attempt % 10 == 0 {
                    info!("Health check proba {}/{}: {}", attempt, max_attempts, e);
                }
            }
        }
        tokio::time::sleep(interval).await;
    }

    Err(anyhow::anyhow!("Health check timeout — kontener nie odpowiedzial w ciagu 5 minut"))
}

/// Odpytuje /v1/models i zwraca nazwe modelu
async fn discover_model(base_url: &str, deployed_model: Option<&str>) -> Result<String> {
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(Duration::from_secs(15))
        .build()?;

    let models_url = format!("{}/v1/models", base_url);

    match client.get(&models_url).send().await {
        Ok(resp) if resp.status().is_success() => {
            let body: serde_json::Value = resp.json().await
                .context("Blad parsowania odpowiedzi /v1/models")?;

            if let Some(models) = body.get("data").and_then(|d| d.as_array()) {
                if let Some(first) = models.first() {
                    if let Some(id) = first.get("id").and_then(|v| v.as_str()) {
                        return Ok(id.to_string());
                    }
                }
            }
        }
        Ok(resp) => {
            warn!("/v1/models zwrocil status {}", resp.status());
        }
        Err(e) => {
            warn!("Blad odpytania /v1/models: {}", e);
        }
    }

    // Fallback na deployed_model z config
    if let Some(model) = deployed_model {
        if !model.is_empty() {
            return Ok(model.to_string());
        }
    }

    Err(anyhow::anyhow!("Nie udalo sie wykryc modelu — brak odpowiedzi z /v1/models"))
}

/// Wiadomosc postepu deploy
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeployProgress {
    pub phase: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl DeployProgress {
    pub fn phase(phase: &str, message: &str) -> Self {
        Self {
            phase: phase.to_string(),
            message: message.to_string(),
            success: None,
            error: None,
        }
    }

    pub fn done(success: bool, message: &str) -> Self {
        Self {
            phase: "done".to_string(),
            message: message.to_string(),
            success: Some(success),
            error: if success { None } else { Some(message.to_string()) },
        }
    }
}

async fn send_progress(tx: &Option<mpsc::Sender<DeployProgress>>, progress: DeployProgress) {
    if let Some(ref tx) = tx {
        let _ = tx.send(progress).await;
    }
}
