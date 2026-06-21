// =============================================================================
// Plik: lib.rs
// Opis: Glowny modul biblioteki TentaFlow Mobile — punkty wejscia dla iOS
//       (extern "C") i Android (JNI). Uruchamia serwisy Core w tle
//       oraz lokalny server HTTPS dla dashboardu WebView na porcie 8090.
// =============================================================================

pub mod lifecycle;
mod platform;
mod runtime;
pub mod ffi_discovery;
mod diagnostics;

use anyhow::Result;
#[cfg(target_os = "android")]
use jni::{objects::{JClass, JString}, JNIEnv};
#[cfg(target_os = "android")]
use std::path::PathBuf;
use tentaflow_core::config::NodeConfig;
use tracing::{info, error};

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

    if let Err(e) = start_app(None) {
        error!("Blad uruchomienia aplikacji: {:#}", e);
    }
}

// =============================================================================
// Punkt wejscia Android — wywolywany z JNI
// =============================================================================

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_ai_tentaflow_mobile_NativeLib_start(
    mut env: JNIEnv,
    _class: JClass,
    data_dir: JString,
) {
    let data_dir = match env.get_string(&data_dir) {
        Ok(value) => Some(PathBuf::from(value.to_string_lossy().into_owned())),
        Err(e) => {
            platform::init_logging();
            error!("Blad odczytu katalogu danych z JNI: {}", e);
            None
        }
    };

    if let Err(e) = start_app(data_dir) {
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
fn start_app(data_dir_override: Option<std::path::PathBuf>) -> Result<()> {
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
    let data_dir = data_dir_override.unwrap_or_else(platform::data_dir);
    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        error!("Blad tworzenia katalogu danych: {}", e);
        return Err(e.into());
    }
    info!("data_dir={}", data_dir.display());

    // tentaflow_core::paths::tentaflow_home() domyslnie spada na current_exe().parent(),
    // ktore w sandboxie iOS pokazuje na read-only Bundle. Wymuszamy writable Documents
    // PRZED pierwszym uzyciem paths::* (OnceLock cache'uje pierwszy dostep). Bez tego
    // vision_models::extract_blob i kazdy inny zapis do models_root() failuje EROFS.
    std::env::set_var("TENTAFLOW_HOME", &data_dir);

    // Konfiguracja dla trybu mobilnego
    let config = create_mobile_config(&data_dir);

    // Uruchom serwisy w osobnym watku — NIE blokuj main thread iOS
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
            match runtime::start_services(config).await {
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
            mtls: None,
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
        services_runtime: ServicesRuntimeConfig::default(),
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
            iroh_relay_url: "https://use.iroh.network/".to_string(),
            dht_enabled: false,
            trust_expiry_days: 30,
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
