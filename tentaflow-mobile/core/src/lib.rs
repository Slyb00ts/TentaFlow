// =============================================================================
// Plik: lib.rs
// Opis: Glowny modul biblioteki TentaFlow Mobile — punkty wejscia dla iOS
//       (extern "C") i Android (JNI). Uruchamia serwisy Core w tle
//       i GUI egui/wgpu (identycznie jak Desktop) + HTTPS server na porcie 8090.
// =============================================================================

pub mod lifecycle;
mod platform;
mod runtime;
pub mod ffi_discovery;
mod diagnostics;

use anyhow::Result;
use tentaflow_core::config::NodeConfig;
use tentaflow_ui::app::TentaFlowApp;
use tentaflow_ui::state::{new_shared_state, SharedAppState};
use tracing::{info, error};

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

// =============================================================================
// Punkt wejscia iOS — wywolywany z Obj-C/Swift bridge
// =============================================================================

#[cfg(target_os = "ios")]
#[no_mangle]
pub extern "C" fn tentaflow_mobile_start() {
    // Panic hook — loguj komunikat paniku do NSLog (widoczne w Xcode console)
    std::panic::set_hook(Box::new(|info| {
        let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown panic".to_string()
        };
        let location = info.location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown".to_string());
        eprintln!("RUST PANIC: {} at {}", msg, location);
    }));

    if let Err(e) = start_app() {
        error!("Blad uruchomienia aplikacji: {:#}", e);
    }
}

// =============================================================================
// Punkt wejscia Android — wywolywany z JNI
// =============================================================================

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_ai_tentaflow_mobile_NativeLib_start(
    _env: *mut std::ffi::c_void,
    _class: *mut std::ffi::c_void,
) {
    if let Err(e) = start_app() {
        error!("Blad uruchomienia aplikacji: {}", e);
    }
}

// =============================================================================
// Wrappery JNI dla cyklu zycia Android
// =============================================================================

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_ai_tentaflow_mobile_NativeLib_onPause(
    _env: *mut std::ffi::c_void,
    _class: *mut std::ffi::c_void,
) {
    lifecycle::tentaflow_on_pause();
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_ai_tentaflow_mobile_NativeLib_onResume(
    _env: *mut std::ffi::c_void,
    _class: *mut std::ffi::c_void,
) {
    lifecycle::tentaflow_on_resume();
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_ai_tentaflow_mobile_NativeLib_onMemoryWarning(
    _env: *mut std::ffi::c_void,
    _class: *mut std::ffi::c_void,
) {
    lifecycle::tentaflow_on_memory_warning();
}

// =============================================================================
// Wspolna logika uruchamiania
// =============================================================================

/// Uruchamia aplikacje mobilna — serwisy Core w tle + HTTPS server.
/// Na iOS wywolywane z didFinishLaunchingWithOptions (main thread).
/// Serwisy startuja w osobnym watku, funkcja NIE blokuje.
fn start_app() -> Result<()> {
    // Logging specyficzny dla platformy
    platform::init_logging();

    info!("start_app() — init_logging OK");

    // Instalacja rustls crypto provider (wymagane przed QUIC mesh)
    let _ = rustls::crypto::ring::default_provider().install_default();

    info!("rustls crypto provider OK");

    // Inicjalizacja lifecycle managera
    lifecycle::init_lifecycle();

    let device = platform::device_info();
    info!("device={}, os={}, ram={}MB", device.model, device.os_version, device.ram_mb);

    // Katalog danych aplikacji
    let data_dir = platform::data_dir();
    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        error!("Blad tworzenia katalogu danych: {}", e);
        return Err(e.into());
    }
    info!("data_dir={}", data_dir.display());

    // Konfiguracja dla trybu mobilnego
    let config = create_mobile_config(&data_dir);

    // Wspoldzielony stan miedzy Core i UI
    let state = new_shared_state();

    // Uruchom serwisy w osobnym watku — NIE blokuj main thread iOS
    let state_for_runtime = state.clone();
    std::thread::spawn(move || {
        info!("Tworzenie tokio runtime...");
        let runtime = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                error!("Blad tworzenia tokio runtime: {}", e);
                return;
            }
        };

        runtime.block_on(async {
            info!("Uruchamianie serwisow Core...");
            match runtime::start_services(config, state_for_runtime).await {
                Ok(_handles) => {
                    info!("Serwisy Core uruchomione — HTTPS na porcie 8090");
                    // Nie upuszczaj handles — serwisy dzialaja w tle
                    std::mem::forget(_handles);
                }
                Err(e) => {
                    error!("Blad uruchamiania serwisow: {:#}", e);
                }
            }
        });

        // Runtime zyje wiecznie — serwisy dzialaja w tle
        info!("Runtime dziala w tle");
        std::mem::forget(runtime);
    });

    info!("start_app() zakonczony — serwisy startuja w tle");
    Ok(())
}

/// Uruchamia GUI egui/wgpu — blokuje do zamkniecia
fn run_gui(state: SharedAppState, should_quit: Arc<AtomicBool>) -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("TentaFlow")
            .with_inner_size([1024.0, 768.0])
            .with_min_inner_size([320.0, 480.0]),
        ..Default::default()
    };

    eframe::run_native(
        "TentaFlow",
        options,
        Box::new(move |_cc| {
            Ok(Box::new(MobileApp {
                inner: TentaFlowApp::new(state),
                should_quit,
            }))
        }),
    )
    .map_err(|e| anyhow::anyhow!("Blad uruchomienia eframe: {}", e))?;

    Ok(())
}

/// Wrapper na TentaFlowApp z obsluga should_quit
struct MobileApp {
    inner: TentaFlowApp,
    should_quit: Arc<AtomicBool>,
}

impl eframe::App for MobileApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Sprawdz czy powinno sie zamknac
        if self.should_quit.load(Ordering::SeqCst) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        // Render glownej aplikacji (identyczne UI jak Desktop)
        self.inner.update(ctx, frame);

        // Odswiezaj co 100ms
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

/// Tworzy konfiguracje dostosowana do urzadzenia mobilnego
fn create_mobile_config(data_dir: &std::path::Path) -> NodeConfig {
    use tentaflow_core::config::*;

    // Backend inferencji — llamacpp na obu platformach
    // (na iOS dostepny tez mlx-swift bridge — rejestrowany z poziomu Swift)
    let inference_backend = "llamacpp";

    NodeConfig {
        server: ServerConfig {
            max_total_connections: 20,
            max_concurrent_requests: 10,
            max_queued_requests: 20,
            worker_threads: 0,
            cpu_affinity: false,
            log_level: "info".to_string(),
            log_format: "compact".to_string(),
        },
        protocols: ProtocolsConfig {
            openai_api: ProtocolConfig {
                enabled: true,
                bind: "0.0.0.0:8090".to_string(),
                tls_cert: None,
                tls_key: None,
                max_connections: 20,
                request_timeout_ms: 60_000,
                body_limit_bytes: 5_242_880,
                mtls_client_ca: None,
            },
            grpc: None,
            quic: None,
        },
        middleware: MiddlewareConfig::default(),
        rate_limiting: RateLimitingConfig::default(),
        load_balancing: LoadBalancingConfig {
            health_check_interval_ms: 30_000,
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
        node_role: NodeRole::Mobile,
        mesh: Some(MeshConfig {
            enabled: true,
            port: 8090,
            static_peers: vec![],
            // iOS blokuje raw multicast bez Apple entitlementa — swarm-discovery
            // dostaje EHOSTUNREACH. LAN discovery robi NativeDiscovery.swift
            // przez systemowy mDNSResponder (NWBrowser/NetService) i karmi
            // iroh przez FFI tentaflow_mobile_add_discovered_peer.
            mdns_enabled: cfg!(not(target_os = "ios")),
            heartbeat_interval_ms: 500,
            peer_timeout_ms: 3000,
            cluster_name: "tentaflow".to_string(),
            iroh_relay_url: "https://use.iroh.network./".to_string(),
        }),
        inference: Some(InferenceConfig {
            enabled: true,
            models_dir: data_dir
                .join("models")
                .to_string_lossy()
                .to_string(),
            autoload_models: vec![],
            gpu_layers: None,
            backend: inference_backend.to_string(),
        }),
    }
}
