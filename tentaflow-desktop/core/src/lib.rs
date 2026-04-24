// =============================================================================
// Plik: lib.rs
// Opis: Biblioteka TentaFlow Desktop — eksportuje publiczna funkcje run()
//       umozliwiajaca uruchomienie aplikacji z platform-specific wrapperow.
// =============================================================================

pub mod tray;
pub mod runtime;

use anyhow::Result;
use clap::Parser;
use tentaflow_core::config::NodeConfig;
use tentaflow_ui::app::TentaFlowApp;
use tentaflow_ui::state::{new_shared_state, SharedAppState};
use tray_icon::menu::MenuEvent;
use tracing::{info, warn, error};
use tracing_subscriber::EnvFilter;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// TentaFlow Desktop — natywna aplikacja z tray icon i GUI
#[derive(Parser, Debug)]
#[command(name = "tentaflow-desktop")]
#[command(version, about = "TentaFlow Desktop — lokalne AI z mesh networking")]
struct Args {
    /// Uruchom bez GUI (tylko serwisy w tle + tray icon)
    #[arg(long, default_value_t = false)]
    headless: bool,

    /// Uruchom bez tray icon
    #[arg(long, default_value_t = false)]
    no_tray: bool,

    /// Port HTTP API (nadpisuje config)
    #[arg(short, long)]
    port: Option<u16>,

    /// Sciezka do pliku konfiguracji
    #[arg(short, long, default_value = "config.toml")]
    config: String,
}

/// Laduje konfiguracje z pliku lub tworzy domyslna dla trybu desktop
fn load_or_default_config(args: &Args) -> NodeConfig {
    match NodeConfig::from_file(&args.config) {
        Ok(mut config) => {
            // Nadpisz port jesli podany z CLI
            if let Some(port) = args.port {
                config.protocols.openai_api.bind = format!("127.0.0.1:{}", port);
            }
            config
        }
        Err(e) => {
            warn!(
                path = %args.config,
                error = %e,
                "Nie znaleziono pliku konfiguracji — uzywam domyslnej"
            );
            create_default_config(args.port)
        }
    }
}

/// Tworzy domyslna konfiguracje dla trybu desktop
fn create_default_config(port: Option<u16>) -> NodeConfig {
    use tentaflow_core::config::*;

    let bind_port = port.unwrap_or(8090);

    NodeConfig {
        server: ServerConfig {
            max_total_connections: 100,
            max_concurrent_requests: 50,
            max_queued_requests: 100,
            worker_threads: 0,
            cpu_affinity: false,
            log_level: "info".to_string(),
            log_format: "pretty".to_string(),
        },
        protocols: ProtocolsConfig {
            openai_api: ProtocolConfig {
                enabled: true,
                bind: format!("0.0.0.0:{}", bind_port),
                tls_cert: None,
                tls_key: None,
                max_connections: 100,
                request_timeout_ms: 120_000,
                body_limit_bytes: 10_485_760,
                mtls_client_ca: None,
            },
            grpc: None,
            quic: None,
        },
        middleware: MiddlewareConfig::default(),
        rate_limiting: RateLimitingConfig::default(),
        load_balancing: LoadBalancingConfig {
            health_check_interval_ms: 10_000,
            health_check_timeout_ms: 5_000,
            unhealthy_threshold: 3,
            healthy_threshold: 2,
            queue_timeout_ms: 30_000,
            circuit_breaker_enabled: true,
            circuit_breaker_threshold: 5,
            circuit_breaker_timeout_ms: 60_000,
        },
        services: vec![],
        service_aliases: vec![],
        monitoring: MonitoringConfig::default(),
        memory: None,
        security: None,
        node_role: NodeRole::Desktop,
        mesh: Some(MeshConfig {
            enabled: true,
            port: 8090,
            static_peers: vec![],
            mdns_enabled: true,
            heartbeat_interval_ms: 500,
            peer_timeout_ms: 3000,
            cluster_name: "tentaflow".to_string(),
        }),
        inference: Some(InferenceConfig {
            enabled: true,
            models_dir: dirs::data_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join("tentaflow-ai")
                .join("models")
                .to_string_lossy()
                .to_string(),
            autoload_models: vec![],
            gpu_layers: None,
            backend: "llamacpp".to_string(),
        }),
    }
}

/// Uruchamia GUI (eframe/egui) — blokuje do zamkniecia okna.
/// Opcjonalnie obsluguje zdarzenia tray icon w petli zdarzen winit.
fn run_gui(
    state: SharedAppState,
    should_quit: Arc<AtomicBool>,
    app_tray: Option<tray::AppTray>,
    api_port: u16,
) -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("TentaFlow Desktop")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([800.0, 500.0]),
        ..Default::default()
    };

    // Tray event handling — przenosimy do TentaFlowApp
    let tray_state = state.clone();
    let tray_quit = should_quit.clone();

    eframe::run_native(
        "TentaFlow Desktop",
        options,
        Box::new(move |_cc| {
            Ok(Box::new(TentaFlowAppWithTray {
                inner: TentaFlowApp::new(tray_state),
                tray: app_tray,
                should_quit: tray_quit,
                api_port,
            }))
        }),
    )
    .map_err(|e| anyhow::anyhow!("Blad uruchomienia eframe: {}", e))?;

    // Po zamknieciu okna sygnalizuj zakonczenie
    should_quit.store(true, Ordering::SeqCst);

    Ok(())
}

/// Wrapper na TentaFlowApp ktory obsluguje rowniez zdarzenia tray icon
struct TentaFlowAppWithTray {
    inner: TentaFlowApp,
    tray: Option<tray::AppTray>,
    should_quit: Arc<AtomicBool>,
    api_port: u16,
}

impl eframe::App for TentaFlowAppWithTray {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Obsluga zdarzen tray w kazdej klatce
        if let Some(ref app_tray) = self.tray {
            let menu_rx = MenuEvent::receiver();
            while let Ok(event) = menu_rx.try_recv() {
                match tray::handle_menu_event(&event, &app_tray.menu_ids) {
                    tray::TrayAction::OpenGui => {
                        // Okno jest juz otwarte
                    }
                    tray::TrayAction::OpenDashboard => {
                        let url = format!("http://127.0.0.1:{}", self.api_port);
                        if let Err(e) = open::that(&url) {
                            error!("Nie udalo sie otworzyc przegladarki: {}", e);
                        }
                    }
                    tray::TrayAction::OpenSettings => {}
                    tray::TrayAction::Quit => {
                        self.should_quit.store(true, Ordering::SeqCst);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        return;
                    }
                    tray::TrayAction::None => {}
                }
            }
            // Aktualizuj statusy w tray
            tray::update_menu_status(app_tray, &self.inner.state());
        }

        // Sprawdz czy powinno sie zamknac (Ctrl+C, tray quit)
        if self.should_quit.load(Ordering::SeqCst) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        // Render glownej aplikacji
        self.inner.update(ctx, frame);

        // Wymuszaj odswiezanie co 100ms (dla tray events i should_quit)
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

/// Petla obslugi tray icon — nasluchuje zdarzen menu
fn run_tray_loop(
    app_tray: tray::AppTray,
    state: SharedAppState,
    should_quit: Arc<AtomicBool>,
    api_port: u16,
) {
    let menu_rx = MenuEvent::receiver();

    loop {
        if should_quit.load(Ordering::SeqCst) {
            break;
        }

        // Sprawdz zdarzenia menu (non-blocking)
        if let Ok(event) = menu_rx.try_recv() {
            match tray::handle_menu_event(&event, &app_tray.menu_ids) {
                tray::TrayAction::OpenGui => {
                    info!("Otwieranie GUI...");
                    // GUI jest juz uruchomione w osobnym watku — tutaj mozna
                    // wyslac sygnal do przywrocenia okna
                }
                tray::TrayAction::OpenDashboard => {
                    let url = format!("http://127.0.0.1:{}", api_port);
                    info!(url = %url, "Otwieranie dashboard w przegladarce");
                    if let Err(e) = open::that(&url) {
                        error!("Nie udalo sie otworzyc przegladarki: {}", e);
                    }
                }
                tray::TrayAction::OpenSettings => {
                    info!("Otwieranie ustawien...");
                }
                tray::TrayAction::Quit => {
                    info!("Zakonczenie z menu tray");
                    should_quit.store(true, Ordering::SeqCst);
                    break;
                }
                tray::TrayAction::None => {}
            }
        }

        // Aktualizuj statusy w menu tray
        tray::update_menu_status(&app_tray, &state);

        // Krotka pauza zeby nie obciazac CPU
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Rejestruje handler Ctrl+C
fn ctrlc_handler(should_quit: Arc<AtomicBool>) {
    let _ = ctrlc::set_handler(move || {
        info!("Otrzymano Ctrl+C");
        should_quit.store(true, Ordering::SeqCst);
    });
}

/// Glowna funkcja uruchomieniowa — wywoływana z platform-specific wrapperow
pub fn run() -> Result<()> {
    let args = Args::parse();

    // Inicjalizacja tracing
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,iroh::net_report=error,iroh_relay=error,noq_proto=error,mdns_sd=off"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .init();

    // Instalacja rustls crypto provider (wymagane przed QUIC mesh)
    let _ = rustls::crypto::ring::default_provider().install_default();

    info!("TentaFlow Desktop v{}", env!("CARGO_PKG_VERSION"));
    info!(headless = args.headless, no_tray = args.no_tray, "Tryb pracy");

    // Ladowanie konfiguracji
    let config = load_or_default_config(&args);
    let api_port = config
        .protocols
        .openai_api
        .bind
        .rsplit(':')
        .next()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(3000);

    info!(port = api_port, "Konfiguracja zaladowana");

    // Wspoldzielony stan miedzy Core, UI i tray
    let state = new_shared_state();
    let should_quit = Arc::new(AtomicBool::new(false));

    // Uruchom tokio runtime dla serwisow Core
    let runtime = tokio::runtime::Runtime::new()?;

    let service_handles = runtime.block_on(async {
        runtime::start_services(config, state.clone()).await
    })?;

    // Rejestruj handler Ctrl+C
    {
        let sq = should_quit.clone();
        ctrlc_handler(sq);
    }

    // Tray icon — tworzony na glownym watku (wymagane przez macOS)
    let app_tray = if !args.no_tray {
        match tray::create_tray(state.clone()) {
            Ok(t) => {
                info!("Tray icon utworzony");
                Some(t)
            }
            Err(e) => {
                warn!("Nie udalo sie utworzyc tray icon: {} — kontynuuje bez tray", e);
                None
            }
        }
    } else {
        None
    };

    // GUI na glownym watku (eframe/winit wymaga glownego watku na macOS)
    // Tray events sa obslugiwane w petli zdarzen eframe
    if !args.headless {
        info!("Uruchamianie GUI...");
        if let Err(e) = run_gui(state.clone(), should_quit.clone(), app_tray, api_port) {
            error!("Blad GUI: {}", e);
        }
    } else {
        info!("Tryb headless — nacisnij Ctrl+C aby zakonczyc");
        // W trybie headless obsluguj tray na glownym watku
        if let Some(tray) = app_tray {
            run_tray_loop(tray, state.clone(), should_quit.clone(), api_port);
        } else {
            while !should_quit.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        }
    }

    // Sygnalizuj zakonczenie
    should_quit.store(true, Ordering::SeqCst);

    // Graceful shutdown — z timeoutem zeby nie wisiec w nieskonczonosc
    info!("Zamykanie aplikacji...");
    runtime.block_on(async {
        let shutdown_future = runtime::shutdown(service_handles);
        match tokio::time::timeout(std::time::Duration::from_secs(5), shutdown_future).await {
            Ok(_) => info!("Graceful shutdown zakonczony"),
            Err(_) => warn!("Timeout graceful shutdown (5s) — wymuszam zamkniecie"),
        }
    });

    info!("TentaFlow Desktop zakonczony");
    Ok(())
}
