// =============================================================================
// Plik: runtime.rs
// Opis: Modul uruchamiajacy serwisy Core w tle — Router, OpenAI API server,
//       Dashboard server, QUIC server, mesh (gossip + mDNS), inference manager,
//       metryki. Graceful shutdown przez CancellationToken.
// =============================================================================

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tentaflow_core::config::NodeConfig;
use tentaflow_core::db;
use tentaflow_core::db::DbPool;
use tentaflow_core::mesh::peer_store::MeshPeerStore;
use tentaflow_core::mesh::pipeline::{
    start_mesh_pipeline, MeshPipelineConfig, MeshPipelineHandles,
};
use tentaflow_core::mesh::security::MeshSecurity;
use tentaflow_core::metrics::{collector::MetricsCollector, RouterMetrics};
use tentaflow_core::routing::Router;
use tentaflow_ui::state::{self as ui_state, SharedAppState, UiCommand};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{error, info};

/// Uchwyty do uruchomionych serwisow — potrzebne do graceful shutdown
pub struct ServiceHandles {
    /// Kanal sygnalizujacy zamkniecie
    shutdown_tx: watch::Sender<bool>,
    /// Router — do wywolania shutdown()
    router: Option<Arc<Router>>,
    /// Uchwyty mesh pipeline — MUSZA zyc, bo Drop wyrejestruje mDNS
    mesh_handles: Option<MeshPipelineHandles>,
    /// Uchwyt do zadania aktualizacji stanu UI
    state_sync_handle: Option<JoinHandle<()>>,
    /// Uchwyt do zadania przetwarzania komend UI
    cmd_handle: Option<JoinHandle<()>>,
}

/// Uruchamia wszystkie serwisy Core w tle
pub async fn start_services(config: NodeConfig, state: SharedAppState) -> Result<ServiceHandles> {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    info!("Uruchamianie serwisow Core...");

    // Inicjalizacja bazy danych SQLite
    let data_dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("tentaflow-ai");
    std::fs::create_dir_all(&data_dir)?;

    let db_path = data_dir.join("desktop.db");
    info!(path = %db_path.display(), "Baza danych SQLite");

    let db = db::init(&db_path).map_err(|e| {
        error!("Blad inicjalizacji bazy danych: {}", e);
        e
    })?;
    info!("Baza danych zainicjalizowana");

    // Czyszczenie osieroconego settings.node_id (legacy UUID) — zastapiony
    // iroh EndpointId z MeshSecurity.public_key_hex().
    let _ = db::repository::delete_setting(&db, "node_id");

    // Inicjalizacja routera
    info!("Inicjalizacja routera...");
    let router = Arc::new(Router::new(config.clone(), Some(db.clone()))?);
    router.start();
    info!("Router uruchomiony");

    // Zaladuj serwisy QUIC z bazy danych (metoda w Core — wspolna dla Router.New, Desktop, Mobile)
    router.load_db_services();

    // Przywroc natywne serwisy (in-process MLX/llama.cpp) z bazy
    router.restore_native_services().await;

    // Zainstaluj wbudowane addony
    if let Err(e) = tentaflow_core::addon::bundled::install_bundled_addons(&db) {
        tracing::warn!("Blad instalacji wbudowanych addonow: {}", e);
    }

    // Inicjalizacja metryk
    let metrics = RouterMetrics::new();
    let collector = MetricsCollector::new(metrics.clone(), Some(db.clone()));
    collector.start().await;

    // Kanal komend UI → Core
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<UiCommand>();

    // Ladowanie master key z pliku i inicjalizacja SettingsCipher
    let file_master_key = tentaflow_core::crypto::load_or_create_master_key_in(Some(&data_dir))
        .expect("Nie udalo sie zaladowac master key z pliku");
    let settings_cipher = Arc::new(tentaflow_core::crypto::SettingsCipher::new(
        &file_master_key,
    ));

    // Migracja istniejacych plaintextowych sekretow
    match tentaflow_core::crypto::migrate_plaintext_secrets(&db, &settings_cipher) {
        Ok(n) if n > 0 => info!("Zaszyfrowano {} plaintextowych sekretow w bazie", n),
        Err(e) => error!("Blad migracji sekretow: {}", e),
        _ => {}
    }

    // MeshSecurity — single source of truth dla tozsamosci. Ed25519 keypair
    // zapisany zaszyfrowany w settings; iroh uzywa tego klucza jako EndpointId.
    // Dashboard mesh porownuje node_id po Ed25519 hex.
    let mesh_security = Arc::new(
        MeshSecurity::new(db.clone(), settings_cipher.clone())
            .map_err(|e| {
                error!("MeshSecurity init: {}", e);
                e
            })?,
    );
    let node_id = mesh_security.ed25519_public_key_hex();
    info!("Mesh identity: {}", &node_id[..16.min(node_id.len())]);

    {
        let mut s = state.write().unwrap_or_else(|e| e.into_inner());
        s.node_id = node_id.clone();
        s.node_role = "desktop".to_string();
        s.router_running = true;
        s.set_command_sender(cmd_tx);
    }

    // Store peerow mesh — wspoldzielony miedzy mDNS, QUIC, dashboard i UI sync
    let mesh_peer_store = MeshPeerStore::new();

    // Mesh networking — mDNS discovery + QUIC mesh (wspoldzielony pipeline z Core)
    // PRZED Dashboard server, zeby Dashboard mial dostep do quic_mesh
    let mesh_handles: Option<MeshPipelineHandles>;
    let mesh_enabled = config.mesh.as_ref().map_or(false, |m| m.enabled);

    if mesh_enabled {
        let pipeline_config = MeshPipelineConfig {
            node_id: node_id.clone(),
            role: "desktop".to_string(),
            mesh_config: config.mesh.as_ref().unwrap().clone(),
        };

        match start_mesh_pipeline(
            pipeline_config,
            &mesh_peer_store,
            Some(db.clone()),
            settings_cipher.clone(),
            mesh_security.clone(),
        )
        .await
        {
            Ok(handles) => {
                {
                    let mut s = state.write().unwrap_or_else(|e| e.into_inner());
                    s.mesh_connected = handles.quic_mesh.is_some();
                }
                mesh_handles = Some(handles);
            }
            Err(e) => {
                error!("Blad uruchomienia mesh pipeline: {}", e);
                mesh_handles = None;
            }
        }
    } else {
        info!("Mesh networking wylaczony w konfiguracji");
        mesh_handles = None;
    };

    // Zaladuj wszystkie dane z bazy do stanu UI (initial sync)
    sync_all_to_state(&db, &state, &metrics, &mesh_peer_store);
    info!("Dane z bazy zaladowane do stanu UI");

    // Unified HTTPS server (OpenAI API + Dashboard na jednym porcie) — z Core
    let quic_mesh_for_server = mesh_handles.as_ref().and_then(|h| h.quic_mesh.clone());
    let mesh_security_for_server = mesh_handles.as_ref().and_then(|h| h.security.clone());
    let local_node_id: Arc<str> = Arc::from(node_id.as_str());

    tentaflow_core::api::unified_server::start_unified_server(
        &config,
        &db,
        &metrics,
        &router,
        &mesh_peer_store,
        quic_mesh_for_server,
        local_node_id,
        mesh_security_for_server,
    )?;

    // Inference manager — juz obslugiwany przez restore_native_services() wyzej

    // Periodyczna synchronizacja stanu z Core do UI (co 3s)
    let state_sync_handle = {
        let state = state.clone();
        let metrics_sync = metrics.clone();
        let db_sync = db.clone();
        let mps_sync = mesh_peer_store.clone();
        let mut shutdown_rx = shutdown_rx.clone();

        Some(tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(3));

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        sync_all_to_state(&db_sync, &state, &metrics_sync, &mps_sync);
                    }
                    _ = shutdown_rx.changed() => {
                        info!("Synchronizacja stanu zatrzymana");
                        break;
                    }
                }
            }
        }))
    };

    // Przetwarzanie komend UI (CRUD)
    let cmd_handle = {
        let db_cmd = db.clone();
        let state_cmd = state.clone();
        let metrics_cmd = metrics.clone();
        let mps_cmd = mesh_peer_store.clone();
        let shutdown_rx_cmd = shutdown_rx.clone();

        Some(tokio::spawn(async move {
            process_ui_commands(
                cmd_rx,
                db_cmd,
                state_cmd,
                metrics_cmd,
                mps_cmd,
                shutdown_rx_cmd,
            )
            .await;
        }))
    };

    info!("Wszystkie serwisy uruchomione");

    Ok(ServiceHandles {
        shutdown_tx,
        router: Some(router),
        mesh_handles,
        state_sync_handle,
        cmd_handle,
    })
}

/// Zatrzymuje wszystkie serwisy (graceful shutdown)
pub async fn shutdown(handles: ServiceHandles) {
    info!("Rozpoczynanie graceful shutdown...");

    // Zatrzymaj router
    if let Some(ref router) = handles.router {
        router.shutdown();
    }

    // Wyslij sygnal zamkniecia do wszystkich taskow
    let _ = handles.shutdown_tx.send(true);

    // Graceful shutdown mesh — zamyka QUIC endpoint i wyrejestruje mDNS
    if let Some(mesh) = handles.mesh_handles {
        mesh.shutdown().await;
    }

    if let Some(h) = handles.state_sync_handle {
        if let Err(e) = h.await {
            error!("Blad zamykania synchronizacji stanu: {}", e);
        }
    }

    if let Some(h) = handles.cmd_handle {
        if let Err(e) = h.await {
            error!("Blad zamykania przetwarzania komend: {}", e);
        }
    }

    info!("Graceful shutdown zakonczony");
}

// =============================================================================
// Synchronizacja danych z bazy do SharedAppState
// =============================================================================

fn sync_all_to_state(
    db: &DbPool,
    state: &SharedAppState,
    metrics: &Arc<RouterMetrics>,
    mesh_peer_store: &MeshPeerStore,
) {
    let mut s = state.write().unwrap_or_else(|e| e.into_inner());

    // Metryki
    let total = metrics
        .total_requests
        .load(std::sync::atomic::Ordering::Relaxed);
    s.total_requests = total;
    s.metrics.total_requests = total;

    // Services
    if let Ok(services) = db::repository::list_services_with_backends(db) {
        s.services = services
            .iter()
            .map(|(svc, backends)| {
                let backend_names: Vec<String> = backends
                    .iter()
                    .map(|b| {
                        b.model_name_override
                            .clone()
                            .unwrap_or_else(|| b.connection_type.clone())
                    })
                    .collect();
                ui_state::ServiceInfo {
                    id: svc.id,
                    name: svc.name.clone(),
                    service_type: parse_service_type(&svc.service_type),
                    status: parse_service_status(&svc.status),
                    quic_status: ui_state::QuicStatus::Disconnected,
                    quic_address: String::new(),
                    backends: backend_names,
                    strategy: svc.strategy.clone(),
                    avg_latency_ms: 0.0,
                    created_at: Some(svc.created_at.clone()),
                }
            })
            .collect();
        s.metrics.active_services = s.services.len() as u64;
    }

    // Models
    if let Ok(models) = db::repository::list_model_entries(db, 0, 1000) {
        s.models = models
            .iter()
            .map(|m| ui_state::ModelInfo {
                id: m.id,
                name: m.model_name.clone(),
                display_name: m
                    .display_name
                    .clone()
                    .unwrap_or_else(|| m.model_name.clone()),
                service_type: parse_service_type(&m.service_type),
                strategy: m.connection_type.clone(),
                service_count: if m.service_id.is_some() { 1 } else { 0 },
                flow_id: m.flow_id.map(|id| id.to_string()),
                is_public: m.is_public,
                is_active: m.is_active,
                loaded: false,
                backend: m.connection_type.clone(),
                tokens_per_second: 0.0,
            })
            .collect();
    }

    // Model Aliases
    if let Ok(aliases) = db::repository::list_model_aliases(db) {
        s.model_aliases = aliases
            .iter()
            .map(|a| ui_state::ModelAlias {
                id: a.id,
                alias: a.alias.clone(),
                target_model: a.target_model.clone(),
                is_active: a.is_active,
            })
            .collect();
    }

    // API Keys
    if let Ok(keys) = db::repository::list_api_keys(db) {
        s.api_keys = keys
            .iter()
            .map(|k| ui_state::ApiKeyInfo {
                id: k.id,
                key_prefix: k.key_prefix.clone(),
                name: k.name.clone(),
                rate_limit_rps: k.rate_limit_rps as u32,
                is_active: k.is_active,
                created_at: k.created_at.clone(),
                last_used_at: k.last_used_at.clone(),
            })
            .collect();
    }

    // Prompts
    if let Ok(prompts) = db::repository::list_prompts(db, 0, 1000) {
        s.prompts = prompts
            .iter()
            .map(|p| ui_state::PromptInfo {
                id: p.id,
                name: p.name.clone(),
                prompt_id: p.prompt_id.clone(),
                prompt_type: parse_prompt_type(&p.prompt_type),
                content: p.content.clone(),
                default_model: p.default_model.clone().unwrap_or_default(),
                version: p.version as u32,
                is_active: p.is_active,
            })
            .collect();
    }

    // Flows
    if let Ok(flows) = db::repository::list_flows(db, 0, 1000) {
        s.flows = flows
            .iter()
            .map(|f| ui_state::FlowInfo {
                id: f.id,
                name: f.name.clone(),
                description: f.description.clone().unwrap_or_default(),
                service_type: f.service_type.clone().unwrap_or_default(),
                status: parse_flow_status(&f.status),
                last_run: None,
                flow_json: f.flow_json.clone(),
            })
            .collect();
    }

    // PII Rules
    if let Ok(rules) = db::repository::list_pii_rules(db, 0, 1000) {
        s.pii_rules = rules
            .iter()
            .map(|r| ui_state::PiiRule {
                id: r.id,
                name: r.name.clone(),
                category: r.category.clone(),
                pattern: r.pattern.clone(),
                replacement: r.replacement.clone(),
                priority: r.priority as i32,
                is_active: r.is_active,
            })
            .collect();
    }

    // TTS Cleaning Rules
    if let Ok(rules) = db::repository::list_tts_cleaning_rules(db, 0, 1000) {
        s.tts_cleaning_rules = rules
            .iter()
            .map(|r| ui_state::TtsCleaningRule {
                id: r.id,
                name: r.rule_type.clone(),
                pattern: r.pattern.clone(),
                replacement: r.replacement.clone().unwrap_or_default(),
                priority: r.priority as i32,
                is_active: r.is_active,
            })
            .collect();
    }

    // Fast Path Patterns
    if let Ok(patterns) = db::repository::list_fast_path_patterns(db, 0, 1000) {
        s.fast_path_patterns = patterns
            .iter()
            .map(|p| ui_state::FastPathPattern {
                id: p.id,
                name: format!("{}/{}", p.module, p.pattern_type),
                pattern: p.pattern.clone(),
                response: p.result_json.clone(),
                priority: p.priority as i32,
                is_active: p.is_active,
            })
            .collect();
    }

    // Settings
    if let Ok(settings) = db::repository::list_settings(db) {
        s.settings = settings
            .iter()
            .map(|st| ui_state::SettingEntry {
                key: st.key.clone(),
                value: st.value.clone(),
                updated_at: Some(st.updated_at.clone()),
            })
            .collect();
    }

    // Portainer Instances
    if let Ok(instances) = db::repository::list_portainer_instances(db) {
        s.portainer_instances = instances
            .iter()
            .map(|inst| ui_state::PortainerInstance {
                id: inst.id,
                name: inst.name.clone(),
                url: inst.url.clone(),
                auth_type: if !inst.api_key.is_empty() {
                    "API Key".to_string()
                } else {
                    "Credentials".to_string()
                },
            })
            .collect();
    }

    // Peers z MeshPeerStore
    let mesh_peers = mesh_peer_store.list();
    s.peers = mesh_peers
        .iter()
        .map(|p| {
            let ram_pct = if p.ram_total_mb > 0 {
                p.ram_used_mb as f64 / p.ram_total_mb as f64 * 100.0
            } else {
                0.0
            };
            let (net_rx, net_tx) = if !p.networks.is_empty() {
                let rx: u64 = p.networks.iter().map(|n| n.rx_bytes_per_sec).sum();
                let tx: u64 = p.networks.iter().map(|n| n.tx_bytes_per_sec).sum();
                (rx as f64, tx as f64)
            } else {
                (0.0, 0.0)
            };
            ui_state::PeerInfo {
                node_id: p.node_id.clone(),
                hostname: p.hostname.clone(),
                address: p
                    .addresses
                    .first()
                    .map(|a| a.to_string())
                    .unwrap_or_default(),
                ip_addresses: p.addresses.iter().map(|a| a.to_string()).collect(),
                role: p.role.clone(),
                status: p.status.clone(),
                quic_connected: p.quic_connected,
                services: vec![],
                cpu_usage: p.cpu_usage_percent as f64,
                ram_usage: ram_pct,
                ram_used_mb: p.ram_used_mb,
                ram_total_mb: p.ram_total_mb,
                gpu_info: if p.gpu_info.is_empty() {
                    None
                } else {
                    Some(
                        p.gpu_info
                            .iter()
                            .map(|g| g.name.clone())
                            .collect::<Vec<_>>()
                            .join(", "),
                    )
                },
                gpus: p
                    .gpu_info
                    .iter()
                    .map(|g| ui_state::GpuInfo {
                        name: g.name.clone(),
                        usage_percent: g.usage_percent as f64,
                        vram_used_mb: g.vram_used_mb,
                        vram_total_mb: g.vram_total_mb,
                    })
                    .collect(),
                models: vec![],
                containers: p
                    .containers
                    .iter()
                    .map(|c| ui_state::PeerContainerInfo {
                        name: c.name.clone(),
                        image: c.image.clone(),
                        status: c.status.clone(),
                        cpu_percent: c.cpu_percent,
                        memory_mb: c.memory_mb,
                    })
                    .collect(),
                labels: vec![],
                network_rx_bytes_sec: net_rx,
                network_tx_bytes_sec: net_tx,
            }
        })
        .collect();
    // Aktualizuj historie metryk
    s.push_metrics_point();
}

fn parse_service_type(s: &str) -> ui_state::ServiceType {
    match s.to_lowercase().as_str() {
        "llm" => ui_state::ServiceType::Llm,
        "tts" => ui_state::ServiceType::Tts,
        "stt" => ui_state::ServiceType::Stt,
        "rag" => ui_state::ServiceType::Rag,
        "embedding" => ui_state::ServiceType::Embedding,
        "vision" => ui_state::ServiceType::Vision,
        "router" => ui_state::ServiceType::Router,
        "memory" => ui_state::ServiceType::Memory,
        "reranker" => ui_state::ServiceType::Reranker,
        _ => ui_state::ServiceType::Llm,
    }
}

fn parse_service_status(s: &str) -> ui_state::ServiceStatus {
    match s.to_lowercase().as_str() {
        "running" | "active" => ui_state::ServiceStatus::Running,
        "error" | "failed" => ui_state::ServiceStatus::Error,
        "starting" => ui_state::ServiceStatus::Starting,
        _ => ui_state::ServiceStatus::Stopped,
    }
}

fn parse_prompt_type(s: &str) -> ui_state::PromptType {
    match s.to_lowercase().as_str() {
        "system" => ui_state::PromptType::System,
        "suffix" => ui_state::PromptType::Suffix,
        "template" => ui_state::PromptType::Template,
        "user" => ui_state::PromptType::User,
        _ => ui_state::PromptType::System,
    }
}

fn parse_flow_status(s: &str) -> ui_state::FlowStatus {
    match s.to_lowercase().as_str() {
        "active" => ui_state::FlowStatus::Active,
        "inactive" => ui_state::FlowStatus::Inactive,
        "failed" | "error" => ui_state::FlowStatus::Failed,
        "draft" => ui_state::FlowStatus::Draft,
        "archived" => ui_state::FlowStatus::Archived,
        _ => ui_state::FlowStatus::Draft,
    }
}

// =============================================================================
// Przetwarzanie komend UI (CRUD na bazie)
// =============================================================================

async fn process_ui_commands(
    mut cmd_rx: mpsc::UnboundedReceiver<UiCommand>,
    db: DbPool,
    state: SharedAppState,
    metrics: Arc<RouterMetrics>,
    mesh_peer_store: MeshPeerStore,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(cmd) => {
                        if let Err(e) = handle_ui_command(&db, &cmd) {
                            error!("Blad przetwarzania komendy UI: {}", e);
                            let mut s = state.write().unwrap_or_else(|e| e.into_inner());
                            s.add_notification(ui_state::NotificationType::Error, format!("Blad: {}", e));
                        } else {
                            // Po udanej operacji — odswierz stan
                            sync_all_to_state(&db, &state, &metrics, &mesh_peer_store);
                        }
                    }
                    None => break,
                }
            }
            _ = shutdown_rx.changed() => {
                info!("Przetwarzanie komend UI zatrzymane");
                break;
            }
        }
    }
}

fn handle_ui_command(db: &DbPool, cmd: &UiCommand) -> Result<()> {
    use tentaflow_core::db::models::*;

    match cmd {
        // --- Prompts ---
        UiCommand::CreatePrompt {
            prompt_id,
            name,
            content,
            prompt_type,
            default_model,
        } => {
            db::repository::create_prompt(
                db,
                &NewPrompt {
                    prompt_id,
                    name,
                    description: None,
                    content,
                    prompt_type,
                    default_model: if default_model.is_empty() {
                        None
                    } else {
                        Some(default_model)
                    },
                    variables: None,
                    cache_priority: 0,
                },
            )?;
        }
        UiCommand::UpdatePrompt {
            id,
            name,
            content,
            prompt_type,
            default_model,
            is_active,
        } => {
            db::repository::update_prompt(
                db,
                &UpdatePrompt {
                    id: *id,
                    name,
                    description: None,
                    content,
                    prompt_type,
                    default_model: if default_model.is_empty() {
                        None
                    } else {
                        Some(default_model)
                    },
                    variables: None,
                    cache_priority: 0,
                    is_active: *is_active,
                },
            )?;
        }
        UiCommand::DeletePrompt(id) => {
            db::repository::delete_prompt(db, *id)?;
        }

        // --- Services ---
        UiCommand::CreateService {
            name,
            service_type,
            strategy,
            config_json,
        } => {
            db::repository::create_service(db, name, service_type, strategy, None, config_json)?;
        }
        UiCommand::DeleteService(id) => {
            db::repository::delete_service(db, *id)?;
        }

        // --- Models ---
        UiCommand::CreateModelEntry {
            model_name,
            display_name,
            service_type,
            connection_type,
            is_public,
            config_json,
        } => {
            db::repository::create_model_entry(
                db,
                &NewModelEntry {
                    model_name,
                    display_name: if display_name.is_empty() {
                        None
                    } else {
                        Some(display_name)
                    },
                    service_type,
                    connection_type,
                    service_id: None,
                    flow_id: None,
                    is_public: *is_public,
                    config_json,
                },
            )?;
        }
        UiCommand::DeleteModelEntry(id) => {
            db::repository::delete_model_entry(db, *id)?;
        }

        // --- Model Aliases ---
        UiCommand::CreateModelAlias {
            alias,
            target_model,
        } => {
            db::repository::create_model_alias(db, alias, target_model)?;
        }
        UiCommand::DeleteModelAlias(id) => {
            db::repository::delete_model_alias(db, *id)?;
        }

        // --- API Keys ---
        UiCommand::CreateApiKey {
            name,
            rate_limit_rps,
        } => {
            use std::hash::{Hash, Hasher};
            let key_raw = format!("sk-{}", uuid::Uuid::new_v4());
            let key_prefix = key_raw[..12].to_string();
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            key_raw.hash(&mut hasher);
            let key_hash = format!("{:x}", hasher.finish());
            db::repository::create_api_key(db, &key_hash, &key_prefix, name, *rate_limit_rps)?;
        }
        UiCommand::DeleteApiKey(id) => {
            db::repository::delete_api_key(db, *id)?;
        }

        // --- Flows ---
        UiCommand::CreateFlow {
            name,
            description,
            service_type,
            flow_json,
        } => {
            db::repository::create_flow(
                db,
                &FlowParams {
                    name,
                    description: if description.is_empty() {
                        None
                    } else {
                        Some(description)
                    },
                    is_default: false,
                    service_type: if service_type.is_empty() {
                        None
                    } else {
                        Some(service_type)
                    },
                    flow_json,
                    status: "draft",
                },
            )?;
        }
        UiCommand::DeleteFlow(id) => {
            db::repository::delete_flow(db, *id)?;
        }

        // --- PII Rules ---
        UiCommand::CreatePiiRule {
            name,
            category,
            pattern,
            replacement,
            priority,
        } => {
            db::repository::create_pii_rule(
                db,
                &NewPiiRule {
                    name,
                    category,
                    pattern,
                    replacement,
                    priority: *priority,
                    description: None,
                    test_examples: None,
                },
            )?;
        }
        UiCommand::UpdatePiiRule {
            id,
            name,
            category,
            pattern,
            replacement,
            is_active,
            priority,
        } => {
            db::repository::update_pii_rule(
                db,
                &UpdatePiiRule {
                    id: *id,
                    name,
                    category,
                    pattern,
                    replacement,
                    is_active: *is_active,
                    priority: *priority as i64,
                    description: None,
                    test_examples: None,
                },
            )?;
        }
        UiCommand::DeletePiiRule(id) => {
            db::repository::delete_pii_rule(db, *id)?;
        }

        // --- TTS Cleaning Rules ---
        UiCommand::CreateTtsRule {
            rule_type,
            pattern,
            replacement,
            language,
            priority,
        } => {
            db::repository::create_tts_cleaning_rule(
                db,
                rule_type,
                pattern,
                if replacement.is_empty() {
                    None
                } else {
                    Some(replacement.as_str())
                },
                language,
                *priority,
            )?;
        }
        UiCommand::DeleteTtsRule(id) => {
            db::repository::delete_tts_cleaning_rule(db, *id)?;
        }

        // --- Fast Path Patterns ---
        UiCommand::CreateFastPath {
            module,
            pattern_type,
            pattern,
            match_type,
            result_json,
            priority,
        } => {
            db::repository::create_fast_path_pattern(
                db,
                module,
                pattern_type,
                pattern,
                match_type,
                result_json,
                *priority,
            )?;
        }
        UiCommand::DeleteFastPath(id) => {
            db::repository::delete_fast_path_pattern(db, *id)?;
        }

        // --- Settings ---
        UiCommand::SetSetting { key, value } => {
            db::repository::set_setting(db, key, value)?;
        }

        // --- Portainer ---
        UiCommand::CreatePortainerInstance {
            name,
            url,
            api_key,
            username,
            password,
        } => {
            db::repository::create_portainer_instance(db, name, url, api_key, username, password)?;
        }
        UiCommand::DeletePortainerInstance(id) => {
            db::repository::delete_portainer_instance(db, *id)?;
        }

        // --- Refresh ---
        UiCommand::RefreshAll => {
            // sync_db_to_state zostanie wywolany po tej funkcji
        }

        // --- Hub / Deploy (handled async, skip in sync handler) ---
        UiCommand::FetchEngines { .. }
        | UiCommand::SearchHfModels { .. }
        | UiCommand::FetchDefaultModels { .. }
        | UiCommand::DeployLlm { .. } => {
            // Te komendy sa obslugiwane asynchronicznie w osobnym tasku
        }

        // --- Model management (handled async in mobile, skip in desktop sync handler) ---
        UiCommand::DownloadModel { .. }
        | UiCommand::LoadModel { .. }
        | UiCommand::UnloadModel
        | UiCommand::DeleteLocalModel { .. } => {
            // Obslugiwane asynchronicznie w runtime mobilnym
        }
    }

    Ok(())
}
