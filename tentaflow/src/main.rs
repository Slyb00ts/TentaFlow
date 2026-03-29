// =============================================================================
// Plik: main.rs
// Opis: Thin binary TentaFlow Router — punkt wejscia. Cala logika biznesowa
//       pochodzi z tentaflow-core. Ten plik odpowiada wylacznie za parsowanie
//       CLI, inicjalizacje komponentow i zarzadzanie cyklem zycia procesu.
// =============================================================================

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use tracing::{error, info};

use tentaflow_core::config::NodeConfig;
use tentaflow_core::db;
use tentaflow_core::metrics::{collector::MetricsCollector, RouterMetrics};
use tentaflow_core::routing::Router;


// =============================================================================
// Argumenty CLI
// =============================================================================

#[derive(Parser, Debug)]
#[command(name = "tentaflow")]
#[command(about = "TentaFlow Router — API Gateway i mesh node")]
#[command(version)]
struct Args {
    /// Sciezka do pliku konfiguracji
    #[arg(short = 'c', long = "config", default_value = "config.toml")]
    config: PathBuf,

    /// Port HTTP API (nadpisuje wartosc z config.toml)
    #[arg(short = 'p', long = "port")]
    port: Option<u16>,

    /// Port QUIC (nadpisuje wartosc z config.toml)
    #[arg(short = 'q', long = "quic-port")]
    quic_port: Option<u16>,

    /// Sciezka do bazy SQLite
    #[arg(long = "db", default_value = "router.db")]
    db_path: PathBuf,

    /// Wylacz mesh networking
    #[arg(long = "no-mesh")]
    no_mesh: bool,

    /// Verbose logging (ustawia RUST_LOG=debug)
    #[arg(short = 'v', long = "verbose")]
    verbose: bool,
}

use tentaflow_core::mesh::pipeline::{MeshPipelineConfig, start_mesh_pipeline};

// =============================================================================
// Punkt wejscia
// =============================================================================

#[tokio::main]
async fn main() -> Result<()> {
    // Zainstaluj domyslny CryptoProvider dla rustls (wymagane przed uzyciem QUIC)
    let _ = rustls::crypto::ring::default_provider().install_default();

    let args = Args::parse();

    // Inicjalizacja loggingu
    setup_logging(args.verbose)?;

    info!("Uruchamianie TentaFlow.Router...");
    info!("Konfiguracja: {:?}", args.config);

    // Wczytaj konfiguracje lub utworz domyslna
    let mut config = if args.config.exists() {
        info!("Wczytywanie konfiguracji z: {:?}", args.config);
        NodeConfig::from_file(&args.config).map_err(|e| {
            error!("Blad wczytywania konfiguracji: {}", e);
            anyhow::anyhow!("{}", e)
        })?
    } else {
        info!("Plik konfiguracji {:?} nie istnieje — tworzenie domyslnej konfiguracji", args.config);
        let config = NodeConfig::default();
        let toml_str = config.to_toml_string().map_err(|e| anyhow::anyhow!("{}", e))?;
        std::fs::write(&args.config, &toml_str)?;
        info!("Zapisano domyslna konfiguracje do: {:?}", args.config);
        config
    };

    // Nadpisz porty z CLI jesli podane
    apply_cli_overrides(&mut config, &args);

    info!("Konfiguracja wczytana pomyslnie");

    // Inicjalizacja bazy danych
    info!("Inicjalizacja bazy danych: {:?}", args.db_path);
    let db = db::init(&args.db_path).map_err(|e| {
        error!("Blad inicjalizacji bazy danych: {}", e);
        e
    })?;

    log_config_summary(&config, &args.db_path);

    // Store peerow mesh — wspoldzielony miedzy mDNS discovery a dashboard API
    let mesh_peer_store = tentaflow_core::mesh::peer_store::MeshPeerStore::new();

    // Inicjalizacja routera (non-blocking)
    info!("Inicjalizacja routera...");
    let router: Arc<Router> = Arc::new(Router::new(config.clone(), Some(db.clone()))?);
    router.start();

    // Zaladuj serwisy QUIC z bazy danych (metoda w Core)
    router.load_db_services();

    // Przywroc natywne serwisy (in-process MLX/llama.cpp) z bazy
    router.restore_native_services().await;

    // Zainstaluj wbudowane addony
    if let Err(e) = tentaflow_core::addon::bundled::install_bundled_addons(&db) {
        tracing::warn!("Blad instalacji wbudowanych addonow: {}", e);
    }

    // Mesh networking — mDNS discovery + QUIC mesh (wspoldzielony pipeline z Core)
    let mut quic_mesh_for_server: Option<Arc<tentaflow_core::mesh::quic_mesh::QuicMeshManager>> = None;
    let mut mesh_security_for_server: Option<Arc<tentaflow_core::mesh::security::MeshSecurity>> = None;
    let mut local_node_id_for_server: Arc<str> = Arc::from("");
    let _mesh_handles;

    if let Some(ref mesh_config) = config.mesh {
        if mesh_config.enabled {
            // Persystentny node_id — generowany raz i zapisywany w bazie
            let node_id = db::repository::get_setting(&db, "node_id")
                .ok()
                .flatten()
                .unwrap_or_else(|| {
                    let id = uuid::Uuid::new_v4().to_string();
                    let _ = db::repository::set_setting(&db, "node_id", &id);
                    info!("Wygenerowano nowy node_id: {}", id);
                    id
                });
            local_node_id_for_server = Arc::from(node_id.as_str());

            let pipeline_config = MeshPipelineConfig {
                node_id: node_id.clone(),
                role: "router".to_string(),
                mesh_config: mesh_config.clone(),
            };

            match start_mesh_pipeline(pipeline_config, &mesh_peer_store, Some(db.clone())).await {
                Ok(handles) => {
                    quic_mesh_for_server = handles.quic_mesh.clone();
                    mesh_security_for_server = handles.security.clone();

                    // Podepnij mesh do routera — umozliwia forwarding requestow do zdalnych nodow
                    if let Some(ref mesh_mgr) = handles.quic_mesh {
                        router.set_mesh_manager(mesh_mgr.clone());
                        router.service_manager().set_mesh_registry(mesh_mgr.service_registry().clone());

                        // Ustaw forward handler — zdalny node uzywa routera do obslugi forwardowanych requestow
                        let router_for_forward = router.clone();
                        mesh_mgr.set_forward_handler(std::sync::Arc::new(move |payload: Vec<u8>| {
                            let router = router_for_forward.clone();
                            Box::pin(async move {
                                use tentaflow_core::net::quic::server::RouterHandler;
                                match router.route_model_request(&payload, true).await {
                                    Ok(response) => response,
                                    Err(e) => {
                                        tracing::error!("Forward handler: blad routingu: {}", e);
                                        use tentaflow_protocol::*;
                                        let error_response = ModelResponse {
                                            request_id: String::new(),
                                            result: ModelResult::Error(ErrorInfo {
                                                error_type: ErrorType::InternalError,
                                                message: format!("Forward handler: {}", e),
                                                details: None,
                                            }),
                                            metrics: None,
                                        };
                                        rkyv::to_bytes::<rkyv::rancor::Error>(&error_response)
                                            .map(|b| b.into_vec())
                                            .unwrap_or_default()
                                    }
                                }
                            })
                        })).await;

                        info!("Mesh routing podlaczony do routera");
                    }

                    _mesh_handles = Some(handles);
                }
                Err(e) => {
                    error!("Blad uruchomienia mesh pipeline: {}", e);
                    _mesh_handles = None;
                }
            }
        } else {
            info!("Mesh networking wylaczony w konfiguracji");
            _mesh_handles = None;
        }
    } else {
        info!("Brak konfiguracji mesh");
        _mesh_handles = None;
    }

    // Inicjalizacja metryk
    let metrics = RouterMetrics::new();
    let collector = MetricsCollector::new(metrics.clone(), Some(db.clone()));
    collector.start().await;

    // Uruchom serwer HTTPS (OpenAI API + Dashboard na jednym porcie) — z Core
    tentaflow_core::api::unified_server::start_unified_server(&config, &db, &metrics, &router, &mesh_peer_store, quic_mesh_for_server, local_node_id_for_server, mesh_security_for_server)?;

    info!("Wszystkie serwery uruchomione. Nacisnij Ctrl+C aby zakonczyc...");

    // Czekaj na sygnal zakonczenia
    tokio::signal::ctrl_c().await?;

    info!("Otrzymano sygnal shutdown, zamykanie routera...");
    router.shutdown();

    // Graceful shutdown mesh — zamyka QUIC endpoint (zwalnia port UDP) i wyrejestruje mDNS
    if let Some(mesh) = _mesh_handles {
        mesh.shutdown().await;
    }

    info!("Router zamkniety.");
    Ok(())
}

// =============================================================================
// Setup loggingu
// =============================================================================

fn setup_logging(verbose: bool) -> Result<()> {
    use tracing_subscriber::{fmt, EnvFilter};

    let filter = if verbose {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug,mdns_sd=off"))
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,mdns_sd=off"))
    };

    fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_thread_ids(false)
        .with_file(true)
        .with_line_number(true)
        .init();

    Ok(())
}

// =============================================================================
// Nadpisywanie konfiguracji z CLI
// =============================================================================

fn apply_cli_overrides(config: &mut NodeConfig, args: &Args) {
    if let Some(port) = args.port {
        config.protocols.openai_api.bind = format!("0.0.0.0:{}", port);
        // QUIC na tym samym porcie co HTTPS (UDP vs TCP)
        if let Some(ref mut quic) = config.protocols.quic {
            quic.bind = format!("0.0.0.0:{}", port);
        }
        // Mesh port tez synchronizuj
        if let Some(ref mut mesh) = config.mesh {
            mesh.port = port;
        }
    }

    if let Some(quic_port) = args.quic_port {
        if let Some(ref mut quic) = config.protocols.quic {
            quic.bind = format!("0.0.0.0:{}", quic_port);
        }
    }

    if args.no_mesh {
        if let Some(ref mut mesh) = config.mesh {
            mesh.enabled = false;
        }
    }

}

// =============================================================================
// Logowanie podsumowania konfiguracji
// =============================================================================

fn log_config_summary(config: &NodeConfig, db_path: &PathBuf) {
    info!("   - Serwisy: {}", config.services.len());
    info!(
        "   - OpenAI API: {} ({})",
        if config.protocols.openai_api.enabled { "wlaczony" } else { "wylaczony" },
        config.protocols.openai_api.bind
    );
    if let Some(ref quic) = config.protocols.quic {
        info!(
            "   - QUIC: {} ({})",
            if quic.enabled { "wlaczony" } else { "wylaczony" },
            quic.bind
        );
    }
    if let Some(ref mesh) = config.mesh {
        info!(
            "   - Mesh QUIC: {} (port {})",
            if mesh.enabled { "wlaczony" } else { "wylaczony" },
            mesh.port
        );
    }
    info!("   - Baza danych: {:?}", db_path);
}

