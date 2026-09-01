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
mod service;

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

    /// Sciezka do bazy SQLite (domyslnie <tentaflow_home>/data/tentaflow.db)
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
    /// Uruchamia usluge TentaFlow (systemd / launchd)
    Start,
    /// Zatrzymuje usluge TentaFlow
    Stop,
    /// Restartuje usluge TentaFlow
    Restart,
    /// Stan uslugi: autostart, PID, config, dashboard, health
    Status,
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
    /// Slim GPU vision worker process — spawned and supervised by the core
    /// process per `[vision].workers_per_gpu` (not intended for manual use)
    VisionWorker {
        /// Stable worker index assigned by the supervisor
        #[arg(long = "worker-id")]
        worker_id: u32,
        /// CUDA device id this worker is pinned to
        #[arg(long = "gpu")]
        gpu: i32,
        /// Unix socket path of the core's worker link
        #[arg(long = "link")]
        link: PathBuf,
        /// Hex auth token for the link Hello (one per incarnation)
        #[arg(long = "token")]
        token: String,
        /// Core SQLite database path (the worker opens it READ-ONLY)
        #[arg(long = "db")]
        db: Option<PathBuf>,
        /// The core's `[vision]` config section serialized as JSON — the worker
        /// freezes these process-wide vision settings at boot (absent = defaults)
        #[arg(long = "vision-config")]
        vision_config: Option<String>,
    },
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

    // Build the runtime honoring `[server].worker_threads` from the config.
    // The full config is loaded inside run_server (async), but the worker-thread
    // count must be known BEFORE the runtime exists, so peek the file here.
    // 0 / missing / unreadable → tokio default (= num_cpus), so a fresh node
    // still uses every core.
    let worker_threads = peek_worker_threads(&args.config);
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    builder.enable_all();
    // Worker/blocking threads default to a 2 MiB stack, which the camera ingest path
    // overflows: GStreamer/CUDA pipeline build runs SYNCHRONOUSLY on a worker (via
    // `block_in_place`) and its deep C/glue call chain needs more — fatal in debug
    // (larger frames) and uncomfortably tight in release. 16 MiB is reserved address
    // space (committed lazily), so this is cheap and removes the RUST_MIN_STACK
    // workaround for every thread the runtime spawns.
    builder.thread_stack_size(16 * 1024 * 1024);
    if worker_threads > 0 {
        builder.worker_threads(worker_threads);
    }
    let runtime = builder.build()?;
    runtime.block_on(run_server(args))
}

/// Best-effort read of `[server].worker_threads` from the config file before the
/// async runtime is built. Any error (missing file, parse/validation failure)
/// returns 0 → tokio default; run_server re-loads the config and surfaces real
/// errors there.
fn peek_worker_threads(config_path: &std::path::Path) -> usize {
    if !config_path.exists() {
        return 0;
    }
    NodeConfig::from_file(config_path)
        .map(|c| c.server.worker_threads)
        .unwrap_or(0)
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

    // Zaplanowane przeniesienia katalogow Data/Sync (Ustawienia → Magazyn
    // danych) wykonuja sie TERAZ — przed otwarciem bazy i ledgera, gdy zadne
    // uchwyty nie sa jeszcze otwarte. Zaraz po nich laduja sie override'y
    // plikowe (storage-paths.conf), zeby `database_path()` wskazywala juz
    // przeniesiony katalog; klucze live z bazy dociagane sa po `db::init`.
    paths::apply_pending_boot_migrations();
    paths::load_path_overrides(|_| None);

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

    let db_path: PathBuf = args.db_path.clone().unwrap_or_else(paths::database_path);

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

    // Freeze the process-wide vision settings from `[vision]` BEFORE anything
    // vision-related (camera ingest, detector pools, worker supervisor) can
    // read them — the config TOML is the only operator mechanism (no env vars).
    if let Err(e) = tentaflow_core::vision::settings::init(config.vision.clone()) {
        error!("Vision settings init: {}", e);
        return Err(anyhow::anyhow!("vision settings init: {}", e));
    }

    tentaflow_core::compliance::ai_gateway::set_token_quota_enabled(
        config.token_metrics.enabled,
    );

    // AI audit persistence: async by default (writes off the request hot path,
    // ~2 ms/request saved + no SQLite-writer-mutex serialisation under load).
    // Opt into synchronous (compliance-strict: prompt persisted BEFORE dispatch,
    // survives a crash) with `TENTAFLOW_AI_AUDIT_SYNC=1`.
    let audit_sync = std::env::var("TENTAFLOW_AI_AUDIT_SYNC")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    tentaflow_core::compliance::audit_worker::set_audit_async(!audit_sync);
    tentaflow_core::compliance::audit_worker::init_audit_worker();
    info!(
        "AI audit mode: {}",
        if audit_sync { "sync" } else { "async" }
    );

    // Inicjalizacja bazy danych
    info!("Inicjalizacja bazy danych: {:?}", db_path);
    let db = db::init(&db_path).map_err(|e| {
        error!("Blad inicjalizacji bazy danych: {}", e);
        e
    })?;
    // Zapisy metryk (model_metrics_rollup) i licznikow zuzycia
    // (token_usage_daily) schodza z hot-path: worker tla akumuluje inkrementy i
    // flushuje je batchowo jedna transakcja co ~200 ms / 512 jobow, a enforcement
    // czyta niesflushowana czesc z pamieci (patrz services/runtime/
    // metrics_worker.rs oraz token_usage_cache.rs). Inicjalizacja PO db::init —
    // worker potrzebuje puli do batchowego flusha. Cache inicjalizujemy PRZED
    // workerem (init jest odporny na kazda kolejnosc, ale ta jest naturalna):
    // petla workera lapie globalna instancje cache'a przy pierwszym flushu.
    tentaflow_core::services::runtime::token_usage_cache::init_token_usage_cache(db.clone());
    tentaflow_core::services::runtime::metrics_worker::init_metrics_worker(db.clone());
    // Wczytaj konfigurowalne lokalizacje katalogow danych (Ustawienia → Magazyn
    // danych) i zastosuj jako runtime override w `paths`. Wolane PO `db::init`
    // (pool dostepny), wiec ponawiamy `ensure_app_dirs()` — jest idempotentne —
    // zeby katalogi powstaly w nowej lokalizacji. Data/Sync dociagane z
    // storage-paths.conf wewnatrz `load_path_overrides`. Klucze `*_dir` sa
    // node-local (nie syncowane).
    {
        paths::load_path_overrides(|key| db::repository::get_setting(&db, key).ok().flatten());
        if let Err(e) = paths::ensure_app_dirs() {
            error!("ensure_app_dirs po wczytaniu override nieudany: {}", e);
            return Err(anyhow::anyhow!("ensure_app_dirs (override): {}", e));
        }
    }

    // ML Studio uses its OWN dedicated SQLite file (`data/ml_studio.db`) with a
    // separate pool and migration runner; open it right after the core DB.
    if let Err(e) = tentaflow_core::ml_studio::init(paths::tentaflow_home()) {
        error!("Blad inicjalizacji bazy ML Studio: {}", e);
        return Err(e);
    }
    // Project Studio ("Projekty") mirrors the same pattern: central registry in
    // `data/projects.db` + per-project pools with an idle sweeper.
    if let Err(e) = tentaflow_core::project_studio::init() {
        error!("Blad inicjalizacji bazy Project Studio: {}", e);
        return Err(e);
    }
    // Durable ingest queue — `data/jobs.db`, its own writer connection (the main
    // database has ONE writer and a queue claim/heartbeat/finish would serialise
    // behind settings, flows and audit writes), and a file SEPARATE from
    // `events.db` because the event log rotates under retention while queued
    // work must not. Reconciliation runs right after the open, BEFORE any worker
    // can claim: a job left `running` by a process run that no longer exists is
    // closed here, and a job still `queued` is kept and drained below.
    if let Err(e) =
        tentaflow_core::services::ingest_jobs::init(&paths::data_dir().join("jobs.db"))
    {
        error!("Ingest queue initialisation failed: {}", e);
        return Err(e);
    }
    tentaflow_core::project_studio::ingest::reconcile_orphans();

    // Event log — `data/events.db`, its own writer connection so a
    // high-frequency timeline never queues behind settings, flows, agents and
    // audit writes on the main database's single writer. Also STARTS the audit
    // outbox delivery loop and the retention sweep; a store without them is a
    // file that grows forever and never delivers its audit copies.
    if let Err(e) = tentaflow_core::events::init(&db) {
        error!("Event log initialisation failed: {}", e);
        return Err(e);
    }
    match tentaflow_core::db::repository::ensure_default_core_sync_policies(&db) {
        Ok(n) if n > 0 => info!("Sync Ledger zasiał {} domyślnych polityk core", n),
        Err(e) => error!("Sync Ledger nie zasiał domyślnych polityk core: {}", e),
        _ => {}
    }
    match tentaflow_core::db::repository::ensure_trusted_nodes_in_sync_identity(&db) {
        Ok(n) if n > 0 => info!("Sync Ledger zarejestrował {} zaufanych nodów mesh", n),
        Err(e) => error!("Sync Ledger nie zarejestrował zaufanych nodów mesh: {}", e),
        _ => {}
    }
    // Auto-harmonogram drainu ingestu dla addonów z toolem `ingest_drain` (np. RAG):
    // bez tego kolejka ingestu stoi bez ręcznego joba, a reset TentaFlow nie wznawia
    // przetwarzania. Idempotentne — odtwarza harmonogram po każdym restarcie.
    match tentaflow_core::scheduler::ensure_addon_ingest_drain_schedules(&db) {
        Ok(n) if n > 0 => info!("Scheduler zapewnił {} auto-jobów drainu ingestu", n),
        Err(e) => error!("Scheduler nie zapewnił auto-jobów drainu ingestu: {}", e),
        _ => {}
    }

    // Restore persisted robot geo anchors into the SLAM scene manager so robots keep
    // their real-world georeference across restarts.
    tentaflow_core::dispatch::robots::load_geo_anchors(&db);

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
    match tentaflow_core::sync::runtime::init(
        db.clone(),
        mesh_security.clone(),
        settings_cipher.clone(),
    ) {
        Ok(_) => {
            info!("Sync Ledger runtime initialized");
            match tentaflow_core::db::repository::enqueue_existing_shared_secret_settings(
                &db,
                &settings_cipher,
            ) {
                Ok(enqueued) if enqueued > 0 => {
                    info!("Sync Ledger enqueued {} shared secret settings", enqueued)
                }
                Err(e) => error!("Sync Ledger shared secret enqueue failed: {}", e),
                _ => {}
            }
            match tentaflow_core::sync::runtime::run_pending_baseline_cutover() {
                Ok(Some(reseeded)) => info!(
                    "Sync Ledger core baseline reset after v53 cutover: re-seeded {} core ops under new epoch",
                    reseeded
                ),
                Ok(None) => {}
                Err(e) => error!("Sync Ledger baseline cutover failed: {}", e),
            }
            match tentaflow_core::addon::storage_sql_exec::drain_installed_sql_captures(&db, 1000) {
                Ok(drained) => info!("Sync Ledger drained {} pending SQL captures", drained),
                Err(e) => error!("Sync Ledger SQL capture drain failed: {}", e),
            }
            match tentaflow_core::sync::core_capture::drain_pending_core_captures(&db, 1000) {
                Ok(drained) => info!("Sync Ledger drained {} pending core captures", drained),
                Err(e) => error!("Sync Ledger core capture drain failed: {}", e),
            }
            match tentaflow_core::sync::kv_capture::drain_pending_kv_captures(&db, 1000) {
                Ok(drained) => info!("Sync Ledger drained {} pending KV captures", drained),
                Err(e) => error!("Sync Ledger KV capture drain failed: {}", e),
            }
            match tentaflow_core::sync::blob_capture::drain_pending_blob_captures(&db, 1000) {
                Ok(drained) => info!("Sync Ledger drained {} pending blob captures", drained),
                Err(e) => error!("Sync Ledger blob capture drain failed: {}", e),
            }
            match tentaflow_core::sync::runtime::apply_unapplied_inbox(1000) {
                Ok(Some(applied)) => info!("Sync Ledger applied {} inbox operations", applied),
                Ok(None) => {}
                Err(e) => error!("Sync Ledger inbox apply failed: {}", e),
            }
        }
        Err(e) => error!("Sync Ledger runtime init failed: {}", e),
    }

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

    for (node_id, public_key_hex, _approved_at) in mesh_security.get_all_trusted_keys() {
        if node_id != local_node_id_str {
            mesh_peer_store.ensure_trusted_peer(&node_id, &public_key_hex, "");
        }
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
        match db.read() {
            Ok(conn) => match tentaflow_core::services_repo::services::list_all(&conn) {
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
            },
            Err(e) => tracing::warn!("boot port reserve: db lock poisoned: {}", e),
        }
    }

    // Inicjalizacja routera (non-blocking)
    info!("Inicjalizacja routera...");
    let router: Arc<Router> = Arc::new(Router::new(config.clone(), Some(db.clone()))?);

    // Ingest queue workers. They need the router (the ingest flow runs through
    // it) and the core pool, which a job recovered after a restart has no
    // request context to get them from, so the handles are published here.
    tentaflow_core::project_studio::ingest::start_workers(db.clone(), router.clone());

    // === Phase 4 (cont.): wire the supervisor against the router's
    // `LiveHandlesCache` so reconcile() updates the same cache the routing
    // call sites read. Order matters: router first, supervisor second. ===
    // Health-loop shutdown flag, set on graceful shutdown before stopping
    // services so the loop does not respawn engines being torn down.
    let mut services_supervisor_shutdown: Option<Arc<std::sync::atomic::AtomicBool>> = None;
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
            settings_cipher.clone(),
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

        // Capture the shutdown flag BEFORE spawn() consumes the supervisor, so
        // the graceful-shutdown path can stop the health loop before tearing
        // down engines (otherwise the loop respawns them → orphans).
        services_supervisor_shutdown = Some(supervisor.shutdown_flag());
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

    // Deployment klastra zyje w kontenerach, ktore przezywaja restart procesu,
    // ale jego fazy prowadzi zadanie ginace razem z procesem. Bez tego rekord
    // zostawal `deploying` na zawsze (i blokowal kolejny deploy), a `running`
    // nie mial kto zweryfikowac — supervisor serwisow celowo pomija czlonkow
    // distributed, bo headless worker zawsze wypadlby u niego jako awaria.
    tentaflow_core::services::deploy::cluster_health::reconcile_on_startup(&db);
    tentaflow_core::services::deploy::cluster_health::spawn_health_loop(
        db.clone(),
        std::sync::Arc::from(local_node_id_str.as_str()),
    );

    // Best-effort discovery of user-managed external daemons (Ollama). Runs in
    // the background so a slow probe does not block the rest of startup; any
    // failure is logged and ignored — auto-detect is a convenience, not a
    // requirement.
    if let Some(port_allocator) = services_port_allocator.clone() {
        let db_for_detect = db.clone();
        let settings_cipher_for_detect = settings_cipher.clone();
        tokio::spawn(async move {
            if let Err(e) = tentaflow_core::services::auto_detect::auto_register_ollama(
                &db_for_detect,
                port_allocator,
                &settings_cipher_for_detect,
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

    // Store pakietow addonow obok bazy (jeden korzen danych dla tej binarki).
    tentaflow_core::addon::bundled::set_packages_base(paths::data_dir());

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
    // Narzedzia i custom flow bloki sa rejestrowane w pamieci procesu — po
    // restarcie bez tego przejscia katalog narzedzi agenta i dispatch narzedzi
    // LLM tracilyby wszystkie grupy addonow az do reinstallu / sync-reconcile.
    addon_manager.register_installed_runtimes();
    // Wpiecie reconcilera mesh-sync: gdy zreplikowana instancja addona wyladuje,
    // sync runtime kaze AddonManagerowi zaladowac/odladowac runtime wg stanu DB.
    tentaflow_core::sync::runtime::set_global_addon_reconciler(addon_manager.clone());
    router
        .service_manager()
        .set_event_bus(addon_manager.event_bus().clone());

    // Late-bind flow_runtime's ServiceManager + EventBus handles so operators
    // (Predict, Sink event_publish) reach the same QUIC routing surface and
    // bus the WASM host functions use.
    {
        let sched = tentaflow_core::flow_runtime::scheduler::FlowScheduler::global();
        sched.set_service_manager(router.service_manager().clone());
        sched.set_event_bus(addon_manager.event_bus().clone());
        if let Some(executor) = router.executor() {
            sched.set_executor(executor);
        }
    }
    tentaflow_core::addon::event_publish::init_global(addon_manager.event_bus().clone());

    addon_manager.clone().start_event_dispatcher();

    // Wpiecie addon block resolverem do flow_engine — od tego momentu flow
    // z node_type "addon.{id}.{block}" dostaje AddonNodeAdapter z resolvera
    // zamiast bledu "no adapter for node".
    if let Some(dispatcher) = router.flow_dispatcher() {
        dispatcher.set_addon_resolver(addon_manager.clone());
        tracing::info!("FlowDispatcher: addon block resolver wpiety");

        // Harness §3.5.0: build AgentService (registry + tool catalog + core.*
        // builtins) with its own deps and pin it into the AgentServiceSlot so
        // the phase-3 blocks read it. AddonManager backs the ToolDispatcher and
        // the per-principal tool permission checks.
        let agent_service = Arc::new(tentaflow_core::agents::AgentService::new(
            db.clone(),
            addon_manager.clone(),
        ));
        dispatcher.set_agent_service(agent_service);
        tracing::info!("FlowDispatcher: AgentService wpiety do slotu");

        // Harness §3.6: background agent runs. Mark orphaned runs (running/
        // waiting from a previous process) interrupted, then install the
        // process-global AgentRunManager backed by this dispatcher (no second
        // loop engine — a background run is a flow execution).
        match tentaflow_core::agents::AgentRunManager::reap_interrupted_on_startup(&db) {
            Ok(n) if n > 0 => {
                tracing::info!("AgentRunManager: {n} orphaned run(s) marked interrupted")
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("AgentRunManager: orphan reap failed: {e}"),
        }
        let run_manager = std::sync::Arc::new(tentaflow_core::agents::AgentRunManager::from_setting(
            db.clone(),
            std::sync::Arc::new(tentaflow_core::agents::FlowDispatcherRunner::new(dispatcher)),
            tentaflow_core::flow_engine::progress_broker::global_broker(),
        ));
        let run_manager = tentaflow_core::agents::agent_run_manager_init_global(run_manager);

        // Harness §3.6 phase 4b: the subagent reactor turns child-completion
        // events into reactive flow runs (flows whose entry is
        // `on_subagent_complete`). It subscribes to the manager's completion
        // stream and dispatches matching flows through this same dispatcher.
        tentaflow_core::agents::subagent_reactor_init_global(
            db.clone(),
            dispatcher,
            run_manager.child_finished_subscribe(),
        );
        tracing::info!("SubagentReactor: zainstalowany (reaktywne flow on_subagent_complete)");
        // Harness §3.13: install the process-global pending-interaction registry
        // (ask_user questions + permission grants raised during a run).
        tentaflow_core::agents::interaction_registry_init_global(std::sync::Arc::new(
            tentaflow_core::agents::InteractionRegistry::new(),
        ));
        tracing::info!("AgentRunManager: global registry installed");

        // Harness §3.6: periodic retention purge for agent runtime state —
        // redacts expired agent_runs PII columns + deletes their mailbox entries
        // per the org's agent_runs retention term (default 30 days). Runs once at
        // startup, then daily.
        tentaflow_core::agents::start_agent_runtime_purge_task(db.clone());
    }

    // Harness §3.2: optional periodic skills curator REPORT pass. Spawns only when
    // `curator_interval_hours` is set to a positive value; the task persists each
    // proposal as an open snapshot for the dashboard and never auto-applies.
    tentaflow_core::skills::start_curator_schedule_task(db.clone(), router.clone());

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
                token_metrics: config.token_metrics.clone(),
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
                                let request: ModelRequest = match tentaflow_protocol::cbor::decode(&payload) {
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
                                        return tentaflow_protocol::cbor::encode(&error_response)
                                            .unwrap_or_default();
                                    }
                                };

                                let response = tentaflow_core::mesh::inference_proxy::dispatch_reverse_request(
                                    &router,
                                    request,
                                    None,
                                ).await;

                                tentaflow_protocol::cbor::encode(&response)
                                    .unwrap_or_default()
                            })
                        })).await;

                        let router_for_stream_forward = router.clone();
                        mesh_mgr.set_forward_stream_handler(std::sync::Arc::new(
                            move |payload: Vec<u8>, tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>| {
                                let router = router_for_stream_forward.clone();
                                Box::pin(async move {
                                    use tentaflow_protocol::*;
                                    let request: ModelRequest = match tentaflow_protocol::cbor::decode(&payload) {
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
                                            if let Ok(bytes) = tentaflow_protocol::cbor::encode(&chunk) {
                                                let _ = tx.send(bytes);
                                            }
                                            return;
                                        }
                                    };
                                    tentaflow_core::mesh::inference_proxy::dispatch_reverse_stream_request(
                                        &router,
                                        request,
                                        tx,
                                        None,
                                    )
                                    .await;
                                })
                            },
                        )).await;

                        // Owner-side live camera relay: a trusted observer node
                        // opens a bi-stream for `camera:<id>`; we subscribe to the
                        // local StreamHub and pump fMP4 frames back. The closure
                        // captures this node's mesh id for the owner-side org gate.
                        #[cfg(feature = "camera")]
                        {
                        let camera_relay_node_id = mesh_mgr.node_id();
                        mesh_mgr.set_camera_stream_handler(std::sync::Arc::new(
                            move |payload: Vec<u8>, tx: tokio::sync::mpsc::Sender<Vec<u8>>| {
                                let local_node_id = camera_relay_node_id.clone();
                                Box::pin(async move {
                                    tentaflow_core::services::camera_relay::server::handle(
                                        payload,
                                        tx,
                                        local_node_id,
                                    )
                                    .await;
                                })
                            },
                        )).await;
                        }

                        // Owner-side live LiDAR relay: a trusted observer node opens
                        // a bi-stream for `lidar:<robot_id>`; we subscribe to the
                        // local StreamHub and pump canonical frames back. Registered
                        // UNCONDITIONALLY (unlike the camera relay) because the LiDAR
                        // pipeline (`services::lidar_push`/`lidar_hub`) is not behind
                        // the `camera` feature — robots may carry LiDAR without a
                        // camera. The closure captures this node's mesh id for the
                        // owner-side org gate.
                        let lidar_relay_node_id = mesh_mgr.node_id();
                        mesh_mgr.set_lidar_stream_handler(std::sync::Arc::new(
                            move |payload: Vec<u8>, tx: tokio::sync::mpsc::Sender<Vec<u8>>| {
                                let local_node_id = lidar_relay_node_id.clone();
                                Box::pin(async move {
                                    tentaflow_core::services::lidar_relay::server::handle(
                                        payload,
                                        tx,
                                        local_node_id,
                                    )
                                    .await;
                                })
                            },
                        )).await;

                        if let Some(port_allocator) = services_port_allocator.clone() {
                            if let Some(executor) = mesh_mgr.command_executor().await {
                                executor
                                    .set_service_action_context(
                                        tentaflow_core::mesh::command_executor::ServiceActionContext {
                                            db: db.clone(),
                                            port_allocator,
                                            iroh: mesh_mgr.clone(),
                                            router: router.clone(),
                                            addon_manager: addon_manager.clone(),
                                        },
                                    )
                                    .await;
                            }
                        }

                        // Sender-side robot-control context for the
                        // `robot_dispatch_v1` host function (routes a controller
                        // action to the node that physically owns the robot).
                        tentaflow_core::mesh::robot_dispatch::set_dispatch_context(
                            tentaflow_core::mesh::robot_dispatch::RobotDispatchContext {
                                iroh: mesh_mgr.clone(),
                                addon_manager: addon_manager.clone(),
                                local_node_id: mesh_mgr.node_id(),
                            },
                        );

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

    // Reset stale deploymentów po unclean shutdown. Runner tokio-task który je
    // produkował nie żyje po restarcie, więc oznaczamy je jako przerwane i
    // odtwarzamy z trwałego wiersza deploymentu.
    match tentaflow_core::db::repository::deployments::reset_stale(&db) {
        Ok(n) if n > 0 => info!(
            "Deployments cleanup: {} stale rows marked as interrupted",
            n
        ),
        Ok(_) => {}
        Err(e) => warn!("Deployments cleanup: {}", e),
    }
    if let Some(port_allocator) = services_port_allocator.clone() {
        resume_interrupted_deployments(
            db.clone(),
            port_allocator,
            local_node_id_str.clone(),
            mesh_services_registry.clone(),
            quic_mesh_for_server.clone(),
            settings_cipher.clone(),
        )
        .await;
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

    // Multi-process vision workers (docs/VISION_WORKER_SHARDING.md).
    // Configured EXCLUSIVELY via the `[vision]` config TOML section;
    // the default 0 spawns nothing and binds no link socket, so production
    // behavior without the section is unchanged. MUST start before the camera
    // hydrate below: the hydrate consults the worker fleet to decide which
    // cameras stay in-process — a late fleet install would double-ingest
    // worker cameras locally.
    #[cfg(all(unix, feature = "camera", feature = "vision"))]
    let vision_workers =
        tentaflow_core::services::vision_worker::supervisor::VisionWorkerSupervisor::start(
            &config.vision,
            db_path.clone(),
        );

    // Boot-time camera ingest hydrate. Without this, `CameraIngestSupervisor`
    // stays empty until SOMEONE opens TentaVision UI in a browser — kamera nie
    // produkuje klatek, analiza Flow nie ma na czym pracować, status zostaje
    // "starting" forever. Cameras are a core resource (not an addon resource),
    // so they must come up at boot just like the dashboard server itself.
    #[cfg(feature = "camera")]
    tokio::spawn(async {
        if let Err(e) =
            tentaflow_core::addon::host_functions::camera::ensure_supervisor_started().await
        {
            tracing::warn!("boot: camera supervisor hydrate failed: {e}");
        }
    });

    info!("Wszystkie serwery uruchomione. Nacisnij Ctrl+C aby zakonczyc...");

    // Czekaj na SIGINT (Ctrl+C) lub SIGTERM (docker stop / systemd). Oba sa
    // obslugiwane identycznie — graceful shutdown. Bez SIGTERM docker stop
    // wysyla SIGKILL po 10s a WAL SQLite moze zostac rozjechane.
    wait_for_shutdown_signal().await?;

    info!("Otrzymano sygnal shutdown, zamykanie routera...");
    // Stop the vision worker fleet first: each worker gets a link Shutdown
    // (drain + clean exit), then a bounded group kill — GPU memory must be
    // released before anything else races the teardown.
    #[cfg(all(unix, feature = "camera", feature = "vision"))]
    if let Some(sup) = &vision_workers {
        sup.stop().await;
    }
    // Zamknij addon manager: anuluj service tick loops, drop dispatcher
    // sender (rozwalenie cyklu referencyjnego Arc<AddonManager> w
    // spawn_blocking task), drop running instances. Bez tego proces nie
    // konczyl sie po SIGINT.
    addon_manager.shutdown();
    // Await the write-behind state flusher's final drain so pending durable
    // addon state is persisted before the process exits (bounded — a stuck DB
    // never hangs shutdown).
    if let Err(e) = addon_manager
        .await_state_flusher_drain(std::time::Duration::from_secs(10))
        .await
    {
        tracing::warn!("addon state flusher drain on shutdown: {}", e);
    }
    // Zatrzymaj wszystkie supervised services (native python-bundle / native
    // binary / docker) zanim router shutdown zwolni RwLocki. Bez tego vLLM /
    // sglang subprocessy zostawaly zombie po Ctrl+C — trzymaly VRAM (~15 GiB
    // dla 9B modelu) i nastepny deploy konkurowal o pamiec z poprzedniej
    // instancji.
    // Stop the health loop FIRST so it cannot respawn the engines we are about
    // to kill (the loop is otherwise leaked for the process lifetime).
    if let Some(flag) = &services_supervisor_shutdown {
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    if let Some(ports) = services_port_allocator.clone() {
        let errors = tentaflow_core::services::deploy::stop_all_supervised(&db, ports).await;
        if !errors.is_empty() {
            for (id, msg) in &errors {
                tracing::warn!("shutdown stop service id={}: {}", id, msg);
            }
        }
    }
    router.shutdown();

    // Stop every active camera session (drains the F1a CameraIngestSupervisor
    // singleton). GStreamer pipelines must terminate before the runtime
    // shuts down, otherwise EOS messages race the tokio worker teardown.
    #[cfg(feature = "camera")]
    tentaflow_core::addon::host_functions::camera::shutdown_camera_supervisor_global().await;

    // Graceful shutdown mesh — zamyka QUIC endpoint (zwalnia port UDP) i wyrejestruje mDNS
    if let Some(mesh) = _mesh_handles {
        mesh.shutdown().await;
    }

    // Wymusz WAL checkpoint — bez tego baza moze zostac z niesfl ushowanym WAL
    // (zwlaszcza po SIGKILL w docker stop)
    if let Err(e) = tentaflow_core::db::checkpoint_wal(&db) {
        tracing::warn!("Checkpoint WAL nieudany: {}", e);
    }
    if let Err(e) = tentaflow_core::ml_studio::db::checkpoint_wal() {
        tracing::warn!("Checkpoint WAL ML Studio nieudany: {}", e);
    }
    if let Err(e) = tentaflow_core::services::ingest_jobs::checkpoint_wal() {
        error!("Ingest queue WAL checkpoint failed: {}", e);
    }
    if let Err(e) = tentaflow_core::project_studio::db::checkpoint_wal() {
        tracing::warn!("Checkpoint WAL Project Studio nieudany: {}", e);
    }
    tentaflow_core::project_studio::project_db::checkpoint_all();
    // Stop the event-log subscribers before the checkpoint: a scope task still
    // draining the progress broadcast would append behind the truncation.
    tentaflow_core::events::progress_log::stop();
    if let Err(e) = tentaflow_core::events::db::checkpoint_wal() {
        tracing::warn!("Event log WAL checkpoint failed: {}", e);
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

    // Non-blocking writer: logi ida przez bufor + watek tla zamiast
    // synchronicznego zapisu na stdout (pty). Watek requestu nigdy nie blokuje
    // sie na wolnym terminalu. `WorkerGuard` musi zyca, dopoki proces loguje —
    // jego drop zamknilby kanal i cisnal reszte bufora, wiec trzymany jest w
    // `static` na cale zycie procesu.
    static LOG_WORKER: std::sync::OnceLock<tracing_appender::non_blocking::WorkerGuard> =
        std::sync::OnceLock::new();
    let (non_blocking, worker) = tracing_appender::non_blocking(std::io::stdout());
    let _ = LOG_WORKER.set(worker);

    fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_thread_ids(false)
        .with_file(true)
        .with_line_number(true)
        .with_writer(non_blocking)
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

async fn resume_interrupted_deployments(
    db: tentaflow_core::db::DbPool,
    port_allocator: Arc<tentaflow_core::services::ports::PortAllocator>,
    local_node_id: String,
    mesh_services_registry: Arc<tentaflow_core::services::mesh_registry::MeshServicesRegistry>,
    quic_mesh: Option<Arc<tentaflow_core::mesh::iroh_manager::IrohMeshManager>>,
    settings_cipher: Arc<tentaflow_core::crypto::SettingsCipher>,
) {
    let rows = match db.read() {
        Ok(conn) => match tentaflow_core::services_repo::deployments::list_resumable(&conn) {
            Ok(rows) => rows,
            Err(e) => {
                warn!("deployment resume: list failed: {}", e);
                return;
            }
        },
        Err(e) => {
            warn!("deployment resume: db lock poisoned: {}", e);
            return;
        }
    };

    for row in rows {
        let Some(service_id) = row.target_service_id else {
            continue;
        };
        let Some(deploy_id) = row.slug.clone() else {
            continue;
        };
        let service = match db.read() {
            Ok(conn) => match tentaflow_core::services_repo::services::get(&conn, service_id) {
                Ok(Some(service)) => service,
                Ok(None) => {
                    warn!(
                        deployment_id = row.id,
                        service_id, "deployment resume: service missing"
                    );
                    continue;
                }
                Err(e) => {
                    warn!(
                        deployment_id = row.id,
                        service_id, "deployment resume: service lookup failed: {}", e
                    );
                    continue;
                }
            },
            Err(e) => {
                warn!("deployment resume: db lock poisoned: {}", e);
                continue;
            }
        };

        let manifest = match tentaflow_core::services::manifest::registry()
            .by_id(&service.engine_id)
            .cloned()
        {
            Some(manifest) => manifest,
            None => {
                mark_resume_failed(
                    &db,
                    service_id,
                    row.id,
                    &deploy_id,
                    Some(&format!("manifest '{}' not found", service.engine_id)),
                );
                continue;
            }
        };
        let deploy_method = match tentaflow_core::services_repo::services::parse_deploy_method(
            &row.deploy_method,
        ) {
            Ok(method) => method,
            Err(e) => {
                mark_resume_failed(&db, service_id, row.id, &deploy_id, Some(&e.to_string()));
                continue;
            }
        };
        let user_config = match serde_json::from_str::<serde_json::Value>(&service.config_json) {
            Ok(value) => value,
            Err(e) => {
                mark_resume_failed(&db, service_id, row.id, &deploy_id, Some(&e.to_string()));
                continue;
            }
        };

        if let Ok(conn) = db.write() {
            let _ = tentaflow_core::services_repo::services::update_status(
                &conn,
                service_id,
                tentaflow_core::services_repo::services::ServiceStatus::Deploying,
            );
            let _ = tentaflow_core::services_repo::services::update_deploy_progress(
                &conn,
                service_id,
                0,
                Some("resuming deployment"),
            );
            let _ = tentaflow_core::services_repo::deployments::set_progress(
                &conn,
                &deploy_id,
                tentaflow_core::services_repo::deployments::DeploymentStatus::Deploying,
                "resuming",
                0,
            );
        }

        publish_resumed_service(
            &db,
            service_id,
            &local_node_id,
            &mesh_services_registry,
            quic_mesh.as_ref(),
        )
        .await;

        let db_task = db.clone();
        let ports_task = port_allocator.clone();
        let local_node_task = local_node_id.clone();
        let registry_task = mesh_services_registry.clone();
        let quic_task = quic_mesh.clone();
        let manifest_task = manifest.clone();
        let config_task = user_config.clone();
        let cipher_task = settings_cipher.clone();
        let deploy_id_task = deploy_id.clone();
        let job = tentaflow_core::services::deploy::DeployJob {
            deploy_id,
            deployment_id: row.id,
            service_id,
            is_redeploy: false,
        };
        let sender_task = tentaflow_core::deploy::log_bus::sender_for(&deploy_id_task);
        {
            let mut progress_rx = sender_task.subscribe();
            let db_progress = db.clone();
            let local_node_progress = local_node_id.clone();
            let registry_progress = mesh_services_registry.clone();
            let quic_progress = quic_mesh.clone();
            tokio::spawn(async move {
                loop {
                    match progress_rx.recv().await {
                        Ok(tentaflow_core::deploy::log_bus::BusMessage::Line(line))
                            if line.kind == "phase" || line.kind == "progress" =>
                        {
                            publish_resumed_service(
                                &db_progress,
                                service_id,
                                &local_node_progress,
                                &registry_progress,
                                quic_progress.as_ref(),
                            )
                            .await;
                        }
                        Ok(tentaflow_core::deploy::log_bus::BusMessage::Line(_)) => {}
                        Ok(tentaflow_core::deploy::log_bus::BusMessage::End { .. }) => return,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    }
                }
            });
        }

        tokio::spawn(async move {
            let start_ms = tentaflow_core::deploy::log_bus::now_ms();
            let result = tentaflow_core::services::deploy::deploy(
                job,
                deploy_method,
                &manifest_task,
                &config_task,
                &ports_task,
                &db_task,
                &cipher_task,
                Some(sender_task.clone()),
            )
            .await;
            match result {
                Ok(_) => {
                    let _ = sender_task.send(tentaflow_core::deploy::log_bus::BusMessage::End {
                        deploy_id: deploy_id_task.clone(),
                        final_status: "success".to_string(),
                        image_tag: String::new(),
                        container_name: String::new(),
                        error_message: String::new(),
                        duration_ms: tentaflow_core::deploy::log_bus::now_ms() - start_ms,
                    });
                }
                Err(err) => {
                    let _ = sender_task.send(tentaflow_core::deploy::log_bus::BusMessage::End {
                        deploy_id: deploy_id_task.clone(),
                        final_status: "failed".to_string(),
                        image_tag: String::new(),
                        container_name: String::new(),
                        error_message: err.to_string(),
                        duration_ms: tentaflow_core::deploy::log_bus::now_ms() - start_ms,
                    });
                }
            }
            publish_resumed_service(
                &db_task,
                service_id,
                &local_node_task,
                &registry_task,
                quic_task.as_ref(),
            )
            .await;
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            tentaflow_core::deploy::log_bus::close(&deploy_id_task);
        });
        info!(
            service_id,
            deployment_id = row.id,
            "deployment resume scheduled"
        );
    }
}

fn mark_resume_failed(
    db: &tentaflow_core::db::DbPool,
    service_id: i64,
    deployment_id: i64,
    deploy_id: &str,
    message: Option<&str>,
) {
    if let Ok(conn) = db.write() {
        let _ = tentaflow_core::services_repo::deployments::mark_finished(
            &conn,
            deployment_id,
            tentaflow_core::services_repo::deployments::DeploymentStatus::Failed,
            message,
        );
        let _ = tentaflow_core::services_repo::services::mark_deploy_failed(
            &conn,
            service_id,
            deploy_id,
            tentaflow_core::services_repo::services::ServiceStatus::Failed,
            message,
        );
    }
}

async fn publish_resumed_service(
    db: &tentaflow_core::db::DbPool,
    service_id: i64,
    local_node_id: &str,
    mesh_services_registry: &tentaflow_core::services::mesh_registry::MeshServicesRegistry,
    quic_mesh: Option<&Arc<tentaflow_core::mesh::iroh_manager::IrohMeshManager>>,
) {
    let Ok(Some(info)) =
        tentaflow_core::services::snapshot_builder::build_one(db, service_id, local_node_id)
    else {
        return;
    };
    mesh_services_registry.apply_local_change(
        local_node_id,
        tentaflow_protocol::ServiceChange::Updated(info.clone()),
    );
    if let Some(qm) = quic_mesh {
        let payload = tentaflow_protocol::mesh::MeshServicesUpdatePayload {
            from_node_id: local_node_id.to_string(),
            change: tentaflow_protocol::ServiceChange::Updated(info),
        };
        if let Ok(bytes) = tentaflow_core::mesh::cbor::encode(&payload) {
            let _ = qm
                .broadcast_ufp2_to_trusted(
                    tentaflow_protocol::mesh::MESH_MSG_SERVICES_UPDATE,
                    &bytes,
                    None,
                )
                .await;
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
        Subcommand::Start => service::start(),
        Subcommand::Stop => service::stop(),
        Subcommand::Restart => service::restart(),
        Subcommand::Status => service::status(),
        Subcommand::SystemCheck => {
            let caps = tentaflow_core::system_check::collect();
            println!("{}", serde_json::to_string_pretty(&caps)?);
            Ok(())
        }
        Subcommand::Update { check, force } => run_update(*check, *force),
        Subcommand::VisionWorker {
            worker_id,
            gpu,
            link,
            token,
            db,
            vision_config,
        } => run_vision_worker_mode(
            *worker_id,
            *gpu,
            link.clone(),
            token.clone(),
            db.clone(),
            vision_config.clone(),
        ),
    }
}

/// Boots the slim vision-worker runtime (docs/VISION_WORKER_SHARDING.md Stage
/// A). Every parameter arrives via CLI args from the spawning supervisor —
/// there is no environment contract for this mode. The `[vision]` settings
/// travel as `--vision-config` JSON and are frozen BEFORE anything vision runs.
fn run_vision_worker_mode(
    worker_id: u32,
    gpu: i32,
    link: PathBuf,
    token: String,
    db: Option<PathBuf>,
    vision_config: Option<String>,
) -> Result<()> {
    #[cfg(all(unix, feature = "camera", feature = "vision"))]
    {
        let vision: tentaflow_core::config::VisionConfig = match vision_config.as_deref() {
            Some(json) => serde_json::from_str(json)
                .map_err(|e| anyhow::anyhow!("parse --vision-config JSON: {e}"))?,
            None => tentaflow_core::config::VisionConfig::default(),
        };
        tentaflow_core::vision::settings::init(vision)
            .map_err(|e| anyhow::anyhow!("freeze vision settings: {e}"))?;
        let db_path = db.unwrap_or_else(tentaflow_core::paths::database_path);
        let mut builder = tokio::runtime::Builder::new_multi_thread();
        builder.enable_all();
        // Same reasoning as the server runtime: GStreamer/CUDA pipeline builds
        // overflow the default 2 MiB worker stacks.
        builder.thread_stack_size(16 * 1024 * 1024);
        let runtime = builder.build()?;
        runtime.block_on(tentaflow_core::vision_worker::run_vision_worker(
            tentaflow_core::vision_worker::VisionWorkerConfig {
                worker_id,
                gpu,
                link_path: link,
                token,
                db_path,
            },
        ))
    }
    #[cfg(not(all(unix, feature = "camera", feature = "vision")))]
    {
        let _ = (worker_id, gpu, link, token, db, vision_config);
        #[cfg(not(unix))]
        anyhow::bail!("vision-worker mode requires Unix domain sockets (Linux/macOS only)");
        #[cfg(unix)]
        anyhow::bail!("this build has no vision pipeline (slim edition)")
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
