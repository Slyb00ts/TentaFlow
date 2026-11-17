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
use tentaflow_core::paths;

#[cfg(target_os = "macos")]
mod mlx_swift_init;
mod receipt;
mod service;
mod update;
#[cfg(windows)]
mod windows_host;

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

    /// Run under the Windows Service Control Manager. install.ps1 puts it in
    /// the registered service command line; it is not for interactive use.
    #[arg(long = "windows-service", hide = true)]
    windows_service: bool,
}

#[derive(clap::Subcommand, Debug)]
enum Subcommand {
    /// Uruchamia usluge TentaFlow (systemd / launchd / usluga Windows)
    Start,
    /// Zatrzymuje usluge TentaFlow
    Stop,
    /// Restartuje usluge TentaFlow
    Restart,
    /// Stan uslugi: autostart, PID, config, dashboard, health
    Status,
    /// Zapisuje domyslna konfiguracje do wskazanego pliku i konczy. Uzywane
    /// przez instalator: schemat configu zyje w NodeConfig, a nie w skrypcie
    /// bash, ktory i tak nie nadazylby za zmianami sekcji.
    InitConfig {
        /// Docelowy plik (nadpisanie wymaga --force).
        #[arg(long = "output", short = 'o')]
        output: PathBuf,
        /// Adres nasluchu HTTP/QUIC. Domyslnie zostaje to, co w NodeConfig.
        #[arg(long = "bind")]
        bind: Option<String>,
        /// Wylacza mesh (enabled/mdns/dht) w zapisanym pliku.
        #[arg(long = "no-mesh")]
        no_mesh: bool,
        /// Nadpisz istniejacy plik.
        #[arg(long = "force")]
        force: bool,
    },
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

// =============================================================================
// Punkt wejscia
// =============================================================================

// Sync entry point — zeby `tentaflow update` mogl spokojnie uruchomic axoupdater
// (axoupdater::run_sync / is_update_needed_sync sa BLOCKING i panikuja w
// srodku tokio runtime). Dla normalnego startu serwera tworzymy tokio runtime
// recznie pod `run_server`.
fn main() -> Result<()> {
    // GPU probe child: enumerate and print, then exit. Must run before any thread
    // exists — the Vulkan ICD filter it applies sets environment variables.
    if let Some(code) = tentaflow_core::mesh::node_info_collector::maybe_run_gpu_probe_entrypoint()
    {
        std::process::exit(code);
    }
    // Register our own binary so the GPU probe can re-exec it. Skipped by test
    // binaries, which keep the in-process enumeration.
    if let Ok(exe) = std::env::current_exe() {
        tentaflow_core::mesh::node_info_collector::set_gpu_probe_executable(exe);
    }
    if let Some(code) = tentaflow_core::code_studio::process_sandbox::maybe_run_sandbox_entrypoint()
    {
        std::process::exit(code);
    }
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

    if args.windows_service {
        #[cfg(windows)]
        return windows_host::run(args);
        #[cfg(not(windows))]
        anyhow::bail!("--windows-service exists only on Windows");
    }
    let result = serve(args);
    flush_logs();
    result
}

/// Builds the async runtime and runs the server until a shutdown signal. Shared
/// by the console and the Windows service, which differ only in who asks for
/// the shutdown.
fn serve(args: Args) -> Result<()> {
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
    let result = runtime.block_on(run_server(args));
    // Dropping a runtime waits for every blocking task without limit; one stuck
    // in a syscall (disk, DNS) would leave a stopped node alive for good.
    runtime.shutdown_timeout(std::time::Duration::from_secs(5));
    result
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
    // Inicjalizacja loggingu. A Windows service has no console and no journal,
    // so its log goes to daily files under <home>\logs.
    let log_dir = args
        .windows_service
        .then(|| paths::tentaflow_home().join("logs"));
    setup_logging(args.verbose, log_dir.as_deref())?;

    // Windows Firewall self-check — przy braku regul Allow Inbound dla
    // 8090 TCP+UDP odpala UAC z PowerShell New-NetFirewallRule. Blad nie
    // przerywa startu — server moze dzialac lokalnie nawet bez regul.
    // Osobny watek: monit UAC czeka na czlowieka, a listenery nie moga czekac
    // na jego odpowiedz (headless start, usluga, test bez pulpitu).
    // A service has no desktop to show that prompt on; install.ps1 creates the
    // rules itself.
    #[cfg(target_os = "windows")]
    if !args.windows_service {
        std::thread::spawn(tentaflow_core::firewall_check::ensure_firewall_rules);
    }

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

    // Process-wide settings, the core database and its write-behind workers.
    let db = tentaflow_core::bootstrap::open_database(&config, &db_path)?;
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

    let identity = tentaflow_core::bootstrap::init_core_state(&db, None, paths::data_dir())?;
    log_config_summary(&config, &db_path);

    let mut node = tentaflow_core::bootstrap::build_router(&config, &db, identity).await?;

    // Deployment klastra zyje w kontenerach, ktore przezywaja restart procesu,
    // ale jego fazy prowadzi zadanie ginace razem z procesem. Bez tego rekord
    // zostawal `deploying` na zawsze (i blokowal kolejny deploy), a `running`
    // nie mial kto zweryfikowac — supervisor serwisow celowo pomija czlonkow
    // distributed, bo headless worker zawsze wypadlby u niego jako awaria.
    tentaflow_core::services::deploy::cluster_health::reconcile_on_startup(&db);
    tentaflow_core::services::deploy::cluster_health::spawn_health_loop(
        db.clone(),
        std::sync::Arc::from(node.local_node_id.as_str()),
        node.router.clone(),
    );

    // Best-effort discovery of user-managed external daemons (Ollama). Runs in
    // the background so a slow probe does not block the rest of startup; any
    // failure is logged and ignored — auto-detect is a convenience, not a
    // requirement.
    if let Some(port_allocator) = node.port_allocator.clone() {
        let db_for_detect = db.clone();
        let settings_cipher_for_detect = node.settings_cipher.clone();
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

    let addon_manager = node.start_router_and_addons()?;
    let mesh_handles = node.start_mesh(&addon_manager, "router", None).await;
    let metrics = node.start_background_services(mesh_handles.as_ref()).await;

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

    if let Some(port_allocator) = node.port_allocator.clone() {
        resume_interrupted_deployments(
            db.clone(),
            port_allocator,
            node.local_node_id.clone(),
            node.mesh_services_registry.clone(),
            mesh_handles.as_ref().and_then(|h| h.quic_mesh.clone()),
            node.settings_cipher.clone(),
        )
        .await;
    }

    node.start_server(&metrics, &addon_manager, mesh_handles.as_ref())?;

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
    node.stop_process_runtime().await?;
    // Stop the vision worker fleet first: each worker gets a link Shutdown
    // (drain + clean exit), then a bounded group kill — GPU memory must be
    // released before anything else races the teardown.
    #[cfg(all(unix, feature = "camera", feature = "vision"))]
    if let Some(sup) = &vision_workers {
        sup.stop().await;
    }
    node.shutdown(&addon_manager, mesh_handles, None).await;

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
    #[cfg(windows)]
    {
        // A service has no console, and a Ctrl+C handler it cannot install must
        // not read as a shutdown request: it then waits for the SCM alone.
        let ctrl_c = async {
            match tokio::signal::ctrl_c().await {
                Ok(()) => info!("Ctrl+C odebrany"),
                Err(err) => {
                    warn!("no console Ctrl+C handler ({err}); waiting for a service stop");
                    std::future::pending::<()>().await;
                }
            }
        };
        tokio::select! {
            _ = ctrl_c => {}
            _ = windows_host::stop_requested() => info!("Service Control Manager: stop requested"),
        }
        Ok(())
    }
}

// =============================================================================
// Setup loggingu
// =============================================================================

/// The guard of the background log writer. A static is never dropped, so the
/// lines still buffered when the process ends would be lost without
/// flush_logs() — the whole shutdown of a service, which ends right after it.
static LOG_WORKER: std::sync::Mutex<Option<tracing_appender::non_blocking::WorkerGuard>> =
    std::sync::Mutex::new(None);

/// Drains the log buffer. Called once the server has stopped; anything logged
/// later is dropped.
fn flush_logs() {
    drop(LOG_WORKER.lock().unwrap_or_else(|e| e.into_inner()).take());
}

fn setup_logging(verbose: bool, log_dir: Option<&std::path::Path>) -> Result<()> {
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
    // sie na wolnym terminalu. `WorkerGuard` lives in LOG_WORKER until
    // flush_logs() drops it, which drains the buffer.
    let (non_blocking, worker) = match log_dir {
        // Two weeks of daily files: enough to look back at an incident, bounded
        // so a long-running service does not fill the disk.
        Some(dir) => tracing_appender::non_blocking(
            tracing_appender::rolling::Builder::new()
                .rotation(tracing_appender::rolling::Rotation::DAILY)
                .filename_prefix("tentaflow")
                .filename_suffix("log")
                .max_log_files(14)
                .build(dir)?,
        ),
        None => tracing_appender::non_blocking(std::io::stdout()),
    };
    *LOG_WORKER.lock().unwrap_or_else(|e| e.into_inner()) = Some(worker);

    fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_thread_ids(false)
        .with_file(true)
        .with_line_number(true)
        .with_ansi(log_dir.is_none())
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
    setup_logging(verbose, None)?;
    match cmd {
        Subcommand::Start => service::start(),
        Subcommand::Stop => service::stop(),
        Subcommand::Restart => service::restart(),
        Subcommand::Status => service::status(),
        Subcommand::InitConfig {
            output,
            bind,
            no_mesh,
            force,
        } => run_init_config(output, bind.as_deref(), *no_mesh, *force),
        Subcommand::SystemCheck => {
            let caps = tentaflow_core::system_check::collect();
            println!("{}", serde_json::to_string_pretty(&caps)?);
            Ok(())
        }
        Subcommand::Update { check, force } => update::run(*check, *force),
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

/// Writes the default configuration to `output`, optionally pinning the listen
/// address and disabling mesh. The installer calls this instead of composing
/// TOML itself: `NodeConfig` has sections without `serde(default)`, so a
/// hand-written partial file fails to parse, and a hand-written full one would
/// have to track every schema change made in Rust.
fn run_init_config(output: &PathBuf, bind: Option<&str>, no_mesh: bool, force: bool) -> Result<()> {
    if output.exists() && !force {
        return Err(anyhow::anyhow!(
            "{} juz istnieje — uzyj --force aby nadpisac",
            output.display()
        ));
    }

    let mut config = tentaflow_core::config::NodeConfig::default();
    if let Some(addr) = bind {
        config.protocols.openai_api.bind = addr.to_string();
        if let Some(quic) = config.protocols.quic.as_mut() {
            quic.bind = addr.to_string();
        }
        // The health endpoint has its own listener; leaving it on 0.0.0.0 while
        // the API is bound to loopback would expose the one thing that answers
        // without auth.
        config.monitoring.health_check_bind = addr.to_string();
    }
    if no_mesh {
        if let Some(mesh) = config.mesh.as_mut() {
            mesh.enabled = false;
            mesh.mdns_enabled = false;
            mesh.dht_enabled = false;
        }
    }

    let toml_str = config
        .to_toml_string()
        .map_err(|e| anyhow::anyhow!("serializacja configu: {}", e))?;
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output, toml_str)?;
    println!("Wrote configuration: {}", output.display());
    Ok(())
}
