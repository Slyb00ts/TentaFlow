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
use tentaflow_core::paths;
use tentaflow_core::routing::Router;

#[cfg(target_os = "macos")]
mod mlx_swift_init;

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

    /// Sciezka do bazy SQLite (domyslnie <tentaflow_home>/data/router.db)
    #[arg(long = "db")]
    db_path: Option<PathBuf>,

    /// Override portable home directory (domyslnie katalog binarki). Ustawia
    /// TENTAFLOW_HOME zanim pliki zostana wyliczone — przydatne dla
    /// deploymentow systemd / docker volume.
    #[arg(long = "home")]
    home: Option<PathBuf>,

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

    // Apply --home BEFORE first call to paths::tentaflow_home() so the
    // OnceLock captures the override.
    if let Some(home) = args.home.as_ref() {
        std::env::set_var("TENTAFLOW_HOME", home);
    }

    if let Some(cmd) = &args.command {
        return run_subcommand(cmd, args.verbose);
    }

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(run_server(args))
}

async fn run_server(args: Args) -> Result<()> {
    // Inicjalizacja loggingu
    setup_logging(args.verbose)?;

    // Windows Firewall self-check — przy braku regul Allow Inbound dla
    // 8090 TCP+UDP odpala UAC z PowerShell New-NetFirewallRule. Blad nie
    // przerywa startu — server moze dzialac lokalnie nawet bez regul.
    #[cfg(target_os = "windows")]
    tentaflow_core::firewall_check::ensure_firewall_rules();

    // Bootstrap Swift MLX bridge (macOS) — musi sie wykonac PRZED router init,
    // zeby InferenceManager::new() zauwazyl ze MlxSwiftEngine jest dostepny i
    // dal mu priorytet nad mlx-models. Bledy nie blokuja startu — fallback na
    // inne backendy (mlx-models, llama.cpp).
    #[cfg(target_os = "macos")]
    {
        if let Err(e) = mlx_swift_init::init() {
            tracing::warn!(
                "[mlx-swift] Bootstrap nieudany — kontynuuje bez Swift MLX: {:#}",
                e
            );
        }
    }

    info!("Uruchamianie TentaFlow.Router...");
    info!("Tentaflow home: {}", paths::tentaflow_home().display());
    info!("Konfiguracja: {:?}", args.config);

    // Materializuj portable layout: data/, models/, cache/, containers/.
    // Bez tego deploy strategie (python-bundle, binary, docker context) nie
    // znajda manifestow i nie wystartuja.
    if let Err(e) = paths::ensure_app_dirs() {
        error!("ensure_app_dirs nieudany: {}", e);
        return Err(anyhow::anyhow!("ensure_app_dirs: {}", e));
    }

    // Audio modele (Silero VAD, WeSpeaker embedding) pobierane w tle.
    // Aplikacja startuje natychmiast; STT/diarization audio dostępne po
    // ukończeniu (zwykle <30s na pierwszym uruchomieniu, instant na kolejnych).
    tokio::spawn(tentaflow_core::audio_models::bootstrap());

    let db_path: PathBuf = args
        .db_path
        .clone()
        .unwrap_or_else(paths::database_path);

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
    info!("Inicjalizacja bazy danych: {:?}", db_path);
    let db = db::init(&db_path).map_err(|e| {
        error!("Blad inicjalizacji bazy danych: {}", e);
        e
    })?;

    // Czyszczenie osieroconego settings.node_id (legacy UUID) — zastapiony
    // iroh EndpointId z MeshSecurity.public_key_hex().
    let _ = db::repository::delete_setting(&db, "node_id");

    log_config_summary(&config, &db_path);

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
    let mut mesh_peer_store = tentaflow_core::mesh::peer_store::MeshPeerStore::new();
    // PR2: parallel peer registry — receives shadow writes from every
    // peer_store mutator so PR3 can flip reads onto it without missing state.
    let peer_registry = tentaflow_core::mesh::peer_registry::PeerRegistry::new(4096);
    mesh_peer_store.set_registry(peer_registry.clone());

    // PR5: hydrate registry from peer_persisted + peer_hints (single source of
    // truth). The startup migration in db::init copies legacy trusted_nodes /
    // settings.trusted_contact:* rows into the new tables, so this call alone
    // restores trust state, hostname, platform AND transport hints for every
    // peer the user previously paired with.
    match peer_registry.hydrate_from_db(&db) {
        Ok(n) => info!("PeerRegistry hydrated {} peers from peer_persisted", n),
        Err(e) => tracing::warn!("PeerRegistry hydrate failed: {}", e),
    }

    // Install PersistenceWriter — mutators in the registry now schedule
    // debounced batched writes through this channel. Must be set AFTER hydrate
    // so the hydrate path itself does not re-emit writes.
    {
        use tentaflow_core::mesh::peer_registry::persistence::{
            DbSink, PersistenceWriter, CHANNEL_CAPACITY,
        };
        let sink = std::sync::Arc::new(DbSink::new(db.clone()));
        let (writer, persist_tx) = PersistenceWriter::new(sink, CHANNEL_CAPACITY);
        peer_registry.set_persistence(persist_tx);
        let _writer_handle = writer.spawn();
    }

    // Mesh services registry — agregator widokow `services` ze wszystkich
    // zaufanych peerow. Pisze do niego pipeline mesh (handlery
    // `MeshServicesGet/Announce/Update`); czyta GUI/forwarding (krok N3b).
    let mesh_services_registry =
        Arc::new(tentaflow_core::services::mesh_registry::MeshServicesRegistry::new());

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

    // === Phase 4: port allocator (services supervisor instantiated after the
    // router so it can share the same `LiveHandlesCache` instance). ===
    let services_port_allocator: Option<Arc<tentaflow_core::services::ports::PortAllocator>> = {
        use std::collections::HashSet;
        use tentaflow_core::services::ports::PortAllocator;

        let services_runtime_cfg = config.services_runtime.clone();

        // Excluded set zostaje pusty — porty istniejących serwisów (z DB)
        // sa pre-rezerwowane PONIZEJ przez `ports.reserve(p)` co dodaje je
        // do `leased` (zwalniane przy stop/delete) zamiast do `excluded`
        // (permanentne, blokuje takze wlasciciela portu przy respawn).
        let excluded: HashSet<u16> = HashSet::new();

        match PortAllocator::new(services_runtime_cfg.port_range, excluded) {
            Ok(allocator) => Some(Arc::new(allocator)),
            Err(e) => {
                tracing::warn!(
                    "Services supervisor disabled: invalid port_range {:?}: {}",
                    services_runtime_cfg.port_range,
                    e
                );
                None
            }
        }
    };

    // Pre-rezerwacja portów już zapisanych w DB (runtime_port każdego
    // serwisu). Bez tego świeży `acquire()` w równoległym deploy mógł
    // dostać port który należy do istniejącego serwisu (allocator nic o
    // nim nie wie po restarcie procesu) → respawn pinned dostawał konflikt
    // i wpadał w fallback z innym portem, czyli "magiczna" zmiana portu.
    if let Some(port_allocator) = services_port_allocator.clone() {
        match db.lock() {
            Ok(conn) => {
                match tentaflow_core::services_repo::services::list_all(&conn) {
                    Ok(services) => {
                        for svc in services {
                            for port in [svc.runtime_port, svc.sidecar_quic_port]
                                .into_iter()
                                .flatten()
                            {
                                if let Err(e) = port_allocator.reserve(port) {
                                    tracing::warn!(
                                        service_id = svc.id,
                                        port,
                                        "boot port reserve skipped: {}",
                                        e
                                    );
                                }
                            }
                        }
                    }
                    Err(e) => tracing::warn!("boot port reserve: list_all failed: {}", e),
                }
            }
            Err(e) => tracing::warn!("boot port reserve: db lock poisoned: {}", e),
        }
    }

    // Inicjalizacja routera (non-blocking)
    info!("Inicjalizacja routera...");
    let router: Arc<Router> = Arc::new(Router::new(config.clone(), Some(db.clone()))?);

    // === Phase 4 (cont.): wire the supervisor against the router's
    // `LiveHandlesCache` so reconcile() updates the same cache the routing
    // call sites read. Order matters: router first, supervisor second. ===
    let services_snapshot_rx_for_router: Option<
        tokio::sync::watch::Receiver<Arc<tentaflow_core::services::supervisor::ServicesSnapshot>>,
    > = if let Some(port_allocator) = services_port_allocator.clone() {
        use tentaflow_core::services::supervisor::{DefaultEmbeddedProbe, Supervisor};
        let services_runtime_cfg = config.services_runtime.clone();
        let live_handles = router.service_manager().live_handles.clone();
        let (supervisor, snapshot_rx) = Supervisor::new(
            &services_runtime_cfg,
            db.clone(),
            port_allocator,
            local_node_id_str.clone(),
            mesh_services_registry.clone(),
            live_handles,
        );
        let supervisor = supervisor
            .with_embedded_probe(Arc::new(DefaultEmbeddedProbe))
            .with_catalog_provider(router.catalog_provider().clone());

        // First tick is synchronous so the initial snapshot is non-empty
        // before the router goes online. Failures are logged but not fatal.
        if let Err(e) = supervisor.run_first_tick().await {
            tracing::warn!("services supervisor: first_tick failed: {}", e);
        }

        let supervisor_handle = supervisor.spawn();
        info!(
            "Services supervisor started (interval={}ms, port_range={:?})",
            services_runtime_cfg.health_check_interval_ms, services_runtime_cfg.port_range
        );
        // Keep the supervisor task alive for the lifetime of the process.
        let _supervisor_handle = supervisor_handle;
        Some(snapshot_rx)
    } else {
        None
    };

    if let Some(rx) = services_snapshot_rx_for_router {
        router.set_services_snapshot_rx(rx);
    }

    // Best-effort discovery of user-managed external daemons (Ollama). Runs in
    // the background so a slow probe does not block the rest of startup; any
    // failure is logged and ignored — auto-detect is a convenience, not a
    // requirement.
    if let Some(port_allocator) = services_port_allocator.clone() {
        let db_for_detect = db.clone();
        tokio::spawn(async move {
            if let Err(e) = tentaflow_core::services::auto_detect::auto_register_ollama(
                &db_for_detect,
                port_allocator,
            )
            .await
            {
                tracing::warn!("auto_detect ollama failed: {}", e);
            }
        });
    }
    // Wire the shared V2 mesh registry into the service manager so the routing
    // path can call `find_live_handle_for_model` to resolve handles across
    // local + remote nodes (krok N7.3).
    router
        .service_manager()
        .set_mesh_services_registry(mesh_services_registry.clone());
    router.start();

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

    addon_manager.clone().start_event_dispatcher();

    // Wpiecie addon block resolverem do flow_engine — od tego momentu flow
    // z node_type "addon.{id}.{block}" dostaje AddonNodeAdapter z resolvera
    // zamiast bledu "no adapter for node".
    if let Some(dispatcher) = router.flow_dispatcher() {
        dispatcher.set_addon_resolver(addon_manager.clone());
        tracing::info!("FlowDispatcher: addon block resolver wpiety");
    }

    // Auto-start wszystkich service-mode addonow ktore byly enabled przed
    // reboot'em — bez tego service mode dzialalby tylko w sesji w ktorej
    // admin explicit kliknal Start.
    addon_manager.auto_start_services();

    // Mesh networking — iroh (LAN mDNS + DHT + relay), wspoldzielony pipeline z Core
    let mut quic_mesh_for_server: Option<Arc<tentaflow_core::mesh::iroh_manager::IrohMeshManager>> =
        None;
    let mut mesh_security_for_server: Option<Arc<tentaflow_core::mesh::security::MeshSecurity>> =
        None;
    let mut mesh_relay_health_for_server: Option<
        Arc<parking_lot::RwLock<tentaflow_core::mesh::relay_health::RelayHealth>>,
    > = None;
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
                mesh_services_registry.clone(),
            )
            .await
            {
                Ok(handles) => {
                    quic_mesh_for_server = handles.quic_mesh.clone();
                    mesh_security_for_server = handles.security.clone();
                    mesh_relay_health_for_server = Some(handles.relay_health.clone());

                    // Podepnij mesh do routera — umozliwia forwarding requestow do zdalnych nodow.
                    // node_id = mesh_security.public_key_hex() juz na starcie, wiec quic_mesh
                    // zwraca ten sam hex — nie ma potrzeby podmieniac peer_store entry.
                    if let Some(ref mesh_mgr) = handles.quic_mesh {
                        router.set_mesh_manager(mesh_mgr.clone());

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

                                let response = tentaflow_core::mesh::inference_proxy::dispatch_reverse_request(
                                    &router,
                                    request,
                                ).await;

                                rkyv::to_bytes::<rkyv::rancor::Error>(&response)
                                    .map(|b| b.into_vec())
                                    .unwrap_or_default()
                            })
                        })).await;

                        let router_for_stream_forward = router.clone();
                        mesh_mgr.set_forward_stream_handler(std::sync::Arc::new(
                            move |payload: Vec<u8>, tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>| {
                                let router = router_for_stream_forward.clone();
                                Box::pin(async move {
                                    use tentaflow_protocol::*;
                                    let request: ModelRequest = match rkyv::access::<ArchivedModelRequest, rkyv::rancor::Error>(&payload)
                                        .and_then(|archived| rkyv::deserialize::<ModelRequest, rkyv::rancor::Error>(archived))
                                    {
                                        Ok(r) => r,
                                        Err(e) => {
                                            tracing::error!("Forward stream handler: blad deserializacji ModelRequest: {}", e);
                                            let chunk = ModelStreamChunk {
                                                request_id: String::new(),
                                                chunk: StreamChunkType::Error(ErrorInfo {
                                                    error_type: ErrorType::InternalError,
                                                    message: format!("Forward stream deserialize: {}", e),
                                                    details: None,
                                                }),
                                            };
                                            if let Ok(bytes) = rkyv::to_bytes::<rkyv::rancor::Error>(&chunk) {
                                                let _ = tx.send(bytes.into_vec());
                                            }
                                            return;
                                        }
                                    };
                                    tentaflow_core::mesh::inference_proxy::dispatch_reverse_stream_request(
                                        &router,
                                        request,
                                        tx,
                                    )
                                    .await;
                                })
                            },
                        )).await;

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
        Some(addon_manager.clone()),
        mesh_relay_health_for_server,
        services_port_allocator.clone(),
        mesh_services_registry.clone(),
    )?;

    info!("Wszystkie serwery uruchomione. Nacisnij Ctrl+C aby zakonczyc...");

    // Czekaj na SIGINT (Ctrl+C) lub SIGTERM (docker stop / systemd). Oba sa
    // obslugiwane identycznie — graceful shutdown. Bez SIGTERM docker stop
    // wysyla SIGKILL po 10s a WAL SQLite moze zostac rozjechane.
    wait_for_shutdown_signal().await?;

    info!("Otrzymano sygnal shutdown, zamykanie routera...");
    // Zamknij addon manager: anuluj service tick loops, drop dispatcher
    // sender (rozwalenie cyklu referencyjnego Arc<AddonManager> w
    // spawn_blocking task), drop running instances. Bez tego proces nie
    // konczyl sie po SIGINT.
    addon_manager.shutdown();
    // Zatrzymaj wszystkie supervised services (native python-bundle / native
    // binary / docker) zanim router shutdown zwolni RwLocki. Bez tego vLLM /
    // sglang subprocessy zostawaly zombie po Ctrl+C — trzymaly VRAM (~15 GiB
    // dla 9B modelu) i nastepny deploy konkurowal o pamiec z poprzedniej
    // instancji.
    if let Some(ports) = services_port_allocator.clone() {
        let errors =
            tentaflow_core::services::deploy::stop_all_supervised(&db, ports).await;
        if !errors.is_empty() {
            for (id, msg) in &errors {
                tracing::warn!("shutdown stop service id={}: {}", id, msg);
            }
        }
    }
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
        wgpu_core=error,\
        mainline=error";
    // RUST_LOG MOZE byc ustawione w srodowisku — wtedy uzytkownik dostaje
    // kontrole nad poziomem, ale BASE_FILTER dokladamy ZAWSZE zeby iroh/noq
    // spam nie wrocil tylnymi drzwiami. Directives sa wstawiane PRZED
    // zawartoscia RUST_LOG: pozniejsze dyrektywy dla tych samych celow
    // nadpisaly by nasze, wiec nasze wyciszenia sa append'owane na koncu i
    // wygrywaja przy kolizji z ogolnym RUST_LOG=info.
    let user_level = std::env::var("RUST_LOG").ok().unwrap_or_else(|| {
        if verbose {
            "debug".to_string()
        } else {
            "info".to_string()
        }
    });
    let filter_str = format!("{},{}", user_level, BASE_FILTER);
    let filter = EnvFilter::new(filter_str);

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
    info!("   - Serwisy: snapshot-driven (DB + mesh registry)");
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
