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
use tracing::{error, info, warn};

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
    #[command(subcommand)]
    command: Option<Subcommand>,

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

#[derive(clap::Subcommand, Debug)]
enum Subcommand {
    /// Sprawdza czy jest nowsza wersja na GitHub Releases i podmienia binarke
    Update {
        /// Tylko sprawdz, nie aktualizuj
        #[arg(long)]
        check: bool,
        /// Wymus aktualizacje nawet jesli juz na najnowszej
        #[arg(long)]
        force: bool,
    },
    /// Wypisuje informacje o systemie + wykrytych GPU + dostepnych silnikach
    SystemCheck,
}

use tentaflow_core::mesh::pipeline::{start_mesh_pipeline, MeshPipelineConfig};

// =============================================================================
// Punkt wejscia
// =============================================================================

// Sync entry point — zeby `tentaflow update` mogl spokojnie uruchomic axoupdater
// (axoupdater::run_sync / is_update_needed_sync sa BLOCKING i panikuja w
// srodku tokio runtime). Dla normalnego startu serwera tworzymy tokio runtime
// recznie pod `run_server`.
fn main() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let args = Args::parse();

    if let Some(cmd) = &args.command {
        return run_subcommand(cmd, args.verbose);
    }

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(run_server(args))
}

async fn run_server(args: Args) -> Result<()> {
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
        info!(
            "Plik konfiguracji {:?} nie istnieje — tworzenie domyslnej konfiguracji",
            args.config
        );
        let config = NodeConfig::default();
        let toml_str = config
            .to_toml_string()
            .map_err(|e| anyhow::anyhow!("{}", e))?;
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

    // Czyszczenie osieroconego settings.node_id (legacy UUID) — zastapiony
    // iroh EndpointId z MeshSecurity.public_key_hex().
    let _ = db::repository::delete_setting(&db, "node_id");

    log_config_summary(&config, &args.db_path);

    // Ladowanie master key z pliku i inicjalizacja SettingsCipher
    let file_master_key = tentaflow_core::crypto::load_or_create_master_key()
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
    // Dashboard mesh i routing peerow uzywaja Ed25519 hex jako node_id.
    let mesh_security = Arc::new(
        tentaflow_core::mesh::security::MeshSecurity::new(db.clone(), settings_cipher.clone())
            .map_err(|e| {
                error!("MeshSecurity init: {}", e);
                e
            })?,
    );
    let local_node_id_str = mesh_security.ed25519_public_key_hex();
    info!(
        "Mesh identity: {}",
        &local_node_id_str[..16.min(local_node_id_str.len())]
    );

    // Store peerow mesh — wspoldzielony miedzy mDNS discovery a dashboard API
    let mesh_peer_store = tentaflow_core::mesh::peer_store::MeshPeerStore::new();

    // Seed lokalnego noda w peer_store — synchronicznie, przed startupem mesh.
    // Dzieki temu catalog/services/mesh GUI zawsze ma target "local" do dyspozycji.
    {
        use tentaflow_core::mesh::node_info_collector;
        let info = node_info_collector::collect_node_info(&local_node_id_str);
        let hostname = info.hostname.clone();
        let platform = node_info_collector::detect_platform();
        let os_info = node_info_collector::collect_os_distro();
        let (docker_available, docker_version) = node_info_collector::collect_docker_info();
        let addresses = node_info_collector::collect_local_addresses();
        mesh_peer_store.seed_local(
            &local_node_id_str,
            hostname,
            if os_info.is_empty() {
                info.os_info.clone()
            } else {
                os_info
            },
            platform,
            info.cpu_count,
            info.ram_total_mb,
            info.gpu_info.clone(),
            addresses,
            docker_available,
            docker_version,
        );
        info!(node_id = %local_node_id_str, "Local node seeded in peer_store");
    }

    // Inicjalizacja routera (non-blocking)
    info!("Inicjalizacja routera...");
    let router: Arc<Router> = Arc::new(Router::new(config.clone(), Some(db.clone()))?);
    router.start();

    // Zaladuj serwisy QUIC z bazy danych (metoda w Core)
    router.load_db_services();

    // Native service restoration is deferred until mesh is attached below.
    // Calling restore_native_services() here would silently skip mesh
    // registration because Router.mesh_manager is still None.

    // Zainstaluj wbudowane addony
    if let Err(e) = tentaflow_core::addon::bundled::install_bundled_addons(&db) {
        tracing::warn!("Blad instalacji wbudowanych addonow: {}", e);
    }

    // Inicjalizacja AddonManager z dostepem do routera (host function llm_generate)
    let addon_manager = Arc::new(
        tentaflow_core::addon::AddonManager::new(db.clone(), settings_cipher.clone())
            .expect("Blad inicjalizacji AddonManager"),
    );
    addon_manager.set_router(router.clone());
    router
        .service_manager()
        .set_event_bus(addon_manager.event_bus().clone());

    // Mesh networking — iroh (LAN mDNS + DHT + relay), wspoldzielony pipeline z Core
    let mut quic_mesh_for_server: Option<Arc<tentaflow_core::mesh::iroh_manager::IrohMeshManager>> =
        None;
    let mut mesh_security_for_server: Option<Arc<tentaflow_core::mesh::security::MeshSecurity>> =
        None;
    let local_node_id_for_server: Arc<str> = Arc::from(local_node_id_str.as_str());
    let _mesh_handles;

    if let Some(ref mesh_config) = config.mesh {
        if mesh_config.enabled {
            let node_id = local_node_id_str.clone();

            let pipeline_config = MeshPipelineConfig {
                node_id: node_id.clone(),
                role: "router".to_string(),
                mesh_config: mesh_config.clone(),
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
                    quic_mesh_for_server = handles.quic_mesh.clone();
                    mesh_security_for_server = handles.security.clone();

                    // Podepnij mesh do routera — umozliwia forwarding requestow do zdalnych nodow.
                    // node_id = mesh_security.public_key_hex() juz na starcie, wiec quic_mesh
                    // zwraca ten sam hex — nie ma potrzeby podmieniac peer_store entry.
                    if let Some(ref mesh_mgr) = handles.quic_mesh {
                        router.set_mesh_manager(mesh_mgr.clone());
                        router
                            .service_manager()
                            .set_mesh_registry(mesh_mgr.service_registry().clone());

                        // Ustaw forward handler — zdalny node uzywa routera do obslugi forwardowanych requestow
                        let router_for_forward = router.clone();
                        mesh_mgr.set_forward_handler(std::sync::Arc::new(move |payload: Vec<u8>| {
                            let router = router_for_forward.clone();
                            Box::pin(async move {
                                use tentaflow_protocol::*;
                                let request: ModelRequest = match rkyv::access::<ArchivedModelRequest, rkyv::rancor::Error>(&payload)
                                    .and_then(|archived| rkyv::deserialize::<ModelRequest, rkyv::rancor::Error>(archived))
                                {
                                    Ok(r) => r,
                                    Err(e) => {
                                        tracing::error!("Forward handler: blad deserializacji ModelRequest: {}", e);
                                        let error_response = ModelResponse {
                                            request_id: String::new(),
                                            result: ModelResult::Error(ErrorInfo {
                                                error_type: ErrorType::InternalError,
                                                message: format!("Forward handler deserialize: {}", e),
                                                details: None,
                                            }),
                                            metrics: None,
                                        };
                                        return rkyv::to_bytes::<rkyv::rancor::Error>(&error_response)
                                            .map(|b| b.into_vec())
                                            .unwrap_or_default();
                                    }
                                };

                                let response = tentaflow_core::routing::reverse_request::dispatch_reverse_request(
                                    &router,
                                    request,
                                ).await;

                                rkyv::to_bytes::<rkyv::rancor::Error>(&response)
                                    .map(|b| b.into_vec())
                                    .unwrap_or_default()
                            })
                        })).await;

                        // Obsluga przychodzacych alias sync od zdalnych nodow
                        let router_for_alias = router.clone();
                        let mut alias_rx = mesh_mgr.subscribe();
                        tokio::spawn(async move {
                            loop {
                                match alias_rx.recv().await {
                                    Ok(tentaflow_core::mesh::iroh_manager::IrohMeshEvent::AliasSyncReceived { from_node_id, data }) => {
                                        match serde_json::from_slice::<Vec<tentaflow_core::db::models::DbModelAlias>>(&data) {
                                            Ok(aliases) => {
                                                tracing::debug!(from = %from_node_id, count = aliases.len(), "Alias cache zsynchronizowany z peera");
                                                router_for_alias.update_alias_cache_from_sync(aliases);
                                            }
                                            Err(e) => {
                                                tracing::warn!(from = %from_node_id, "Blad deserializacji AliasSync: {}", e);
                                            }
                                        }
                                    }
                                    Ok(_) => {}
                                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                        tracing::warn!("Alias sync listener opuscil {} wiadomosci", n);
                                    }
                                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                                }
                            }
                        });

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

    // Restore native services (in-process MLX/llama.cpp) from DB. Done after
    // mesh attachment so register_native_service_in_mesh can publish them to
    // the mesh service registry.
    router.restore_native_services().await;

    // Inicjalizacja metryk
    let metrics = RouterMetrics::new();
    let collector = MetricsCollector::new(metrics.clone(), Some(db.clone()));
    collector
        .start(router.service_manager().shutdown_rx.clone())
        .await;

    // Sprzątanie ephemeral kontenerów Meeting Bot po unclean shutdown — stare wiersze
    // meeting_sessions ze status=active/joining dostają ended_at, porty sa zwalniane,
    // docker containers z labelem tentaflow.kind=meeting-bot force-removed.
    {
        // Cleanup nie potrzebuje ServiceManagera — tylko DB i Docker API.
        let meeting_mgr = tentaflow_core::meeting::MeetingManager::new(db.clone(), None);
        if let Err(e) = meeting_mgr.cleanup_on_startup().await {
            warn!("Meeting Bot cleanup_on_startup: {}", e);
        }
    }

    // Reset stale deploymentów po unclean shutdown — wiersze pozostawione jako
    // 'building'/'running' dostają status='failure' z error='aborted'. Runner
    // tokio-task który je produkował nie żyje po restarcie.
    match tentaflow_core::db::repository::deployments::reset_stale(&db) {
        Ok(n) if n > 0 => info!("Deployments cleanup: {} stale rows marked as failure", n),
        Ok(_) => {}
        Err(e) => warn!("Deployments cleanup: {}", e),
    }

    // Uruchom serwer HTTPS (OpenAI API + Dashboard na jednym porcie) — z Core
    tentaflow_core::api::unified_server::start_unified_server(
        &config,
        &db,
        &metrics,
        &router,
        &mesh_peer_store,
        quic_mesh_for_server,
        local_node_id_for_server,
        mesh_security_for_server,
    )?;

    info!("Wszystkie serwery uruchomione. Nacisnij Ctrl+C aby zakonczyc...");

    // Czekaj na SIGINT (Ctrl+C) lub SIGTERM (docker stop / systemd). Oba sa
    // obslugiwane identycznie — graceful shutdown. Bez SIGTERM docker stop
    // wysyla SIGKILL po 10s a WAL SQLite moze zostac rozjechane.
    wait_for_shutdown_signal().await?;

    info!("Otrzymano sygnal shutdown, zamykanie routera...");
    router.shutdown();

    // Graceful shutdown mesh — zamyka QUIC endpoint (zwalnia port UDP) i wyrejestruje mDNS
    if let Some(mesh) = _mesh_handles {
        mesh.shutdown().await;
    }

    // Wymusz WAL checkpoint — bez tego baza moze zostac z niesfl ushowanym WAL
    // (zwlaszcza po SIGKILL w docker stop)
    if let Err(e) = tentaflow_core::db::checkpoint_wal(&db) {
        tracing::warn!("Checkpoint WAL nieudany: {}", e);
    }

    info!("Router zamkniety.");
    Ok(())
}

/// Czeka rownolegle na SIGINT (Ctrl+C) i SIGTERM. Pierwszy wygrywa.
/// Na Windowsie (gdzie SIGTERM nie istnieje) czeka tylko na Ctrl+C.
async fn wait_for_shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate())?;
        let mut sigint = signal(SignalKind::interrupt())?;
        tokio::select! {
            _ = sigint.recv() => info!("SIGINT odebrany"),
            _ = sigterm.recv() => info!("SIGTERM odebrany"),
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await
    }
}

// =============================================================================
// Setup loggingu
// =============================================================================

fn setup_logging(verbose: bool) -> Result<()> {
    use tracing_subscriber::{fmt, EnvFilter};

    // Chcemy widziec tylko NASZE logi (iroh_mesh:, mesh:, meeting:, ...), a nic
    // z samego stacka iroh/netwatch/mdns/wgpu. Wszystko z tych modulow spada do
    // `error` albo `off` — w razie realnego bledu dalej zobaczymy, ale nie ma
    // spamu INFO/WARN na kazdy rediscover/dial/relay-retry.
    const BASE_FILTER: &str = "iroh=error,\
        iroh_base=error,\
        iroh_quinn=error,\
        iroh_quinn_proto=error,\
        iroh_relay=error,\
        iroh_metrics=error,\
        swarm_discovery=error,\
        netwatch=error,\
        portmapper=error,\
        mdns_sd=off,\
        noq_proto=error,\
        noq_udp=error,\
        wgpu_hal=error,\
        wgpu_core=error";
    let filter = if verbose {
        EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(format!("debug,{}", BASE_FILTER)))
    } else {
        EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(format!("info,{}", BASE_FILTER)))
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
        if config.protocols.openai_api.enabled {
            "wlaczony"
        } else {
            "wylaczony"
        },
        config.protocols.openai_api.bind
    );
    if let Some(ref quic) = config.protocols.quic {
        info!(
            "   - QUIC: {} ({})",
            if quic.enabled {
                "wlaczony"
            } else {
                "wylaczony"
            },
            quic.bind
        );
    }
    if let Some(ref mesh) = config.mesh {
        info!(
            "   - Mesh QUIC: {} (port {})",
            if mesh.enabled {
                "wlaczony"
            } else {
                "wylaczony"
            },
            mesh.port
        );
    }
    info!("   - Baza danych: {:?}", db_path);
}

// =============================================================================
// Subkomendy CLI (update / system-check)
// =============================================================================

fn run_subcommand(cmd: &Subcommand, verbose: bool) -> Result<()> {
    setup_logging(verbose)?;
    match cmd {
        Subcommand::SystemCheck => {
            let caps = tentaflow_core::system_check::collect();
            println!("{}", serde_json::to_string_pretty(&caps)?);
            Ok(())
        }
        Subcommand::Update { check, force } => run_update(*check, *force),
    }
}

fn run_update(check_only: bool, force: bool) -> Result<()> {
    use axoupdater::AxoUpdater;

    let mut updater = AxoUpdater::new_for("tentaflow");
    // Zrodlo: GitHub Releases tego repo (env override w razie potrzeby).
    updater.set_release_source(axoupdater::ReleaseSource {
        release_type: axoupdater::ReleaseSourceType::GitHub,
        owner: std::env::var("TENTAFLOW_REPO_OWNER").unwrap_or_else(|_| "Slyb00ts".to_string()),
        name: std::env::var("TENTAFLOW_REPO_NAME").unwrap_or_else(|_| "TentaFlow".to_string()),
        app_name: "tentaflow".to_string(),
    });

    info!("Sprawdzam najnowsza wersje na GitHub Releases...");
    let outcome = if check_only {
        match updater.is_update_needed_sync()? {
            true => {
                println!("Dostepna nowa wersja TentaFlow (uruchom: `tentaflow update`)");
                Ok::<_, anyhow::Error>(())
            }
            false => {
                println!("Aktualna wersja jest najnowsza.");
                Ok(())
            }
        }
    } else {
        if force {
            updater.always_update(true);
        }
        match updater.run_sync()? {
            Some(result) => {
                let old = result
                    .old_version
                    .as_ref()
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "?".into());
                println!("Zaktualizowano: {} -> {}", old, result.new_version);
                println!(
                    "Restartuj usluge: systemctl restart tentaflow  (lub launchctl unload/load)."
                );
                Ok(())
            }
            None => {
                println!("Brak nowej wersji do pobrania.");
                Ok(())
            }
        }
    };
    outcome.map_err(|e| anyhow::anyhow!("Update nieudany: {}", e))
}
