// =============================================================================
// Plik: platform.rs
// Opis: Abstrakcja platformowa — sciezki danych, logging i informacje o
//       urzadzeniu dla iOS i Android. Fallback na standardowe dirs.
// =============================================================================

use std::path::PathBuf;
#[cfg(target_os = "android")]
use tracing::info;

/// Informacje o urzadzeniu mobilnym
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub model: String,
    pub os_version: String,
    pub ram_mb: u64,
}

// =============================================================================
// Katalog danych aplikacji
// =============================================================================

/// Zwraca sciezke do katalogu danych aplikacji
///
/// iOS: Documents/ w sandbox aplikacji
/// Android: wewnetrzna pamiec /data/data/<pkg>/files
/// Fallback: standardowy katalog danych uzytkownika
pub fn data_dir() -> PathBuf {
    #[cfg(target_os = "ios")]
    {
        ios_data_dir()
    }

    #[cfg(target_os = "android")]
    {
        android_data_dir()
    }

    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("tentaflow-mobile")
    }
}

#[cfg(target_os = "ios")]
fn ios_data_dir() -> PathBuf {
    // NSDocumentDirectory — sandbox aplikacji iOS
    // W produkcji uzywa sie objc runtime do pobrania sciezki
    dirs::document_dir()
        .unwrap_or_else(|| PathBuf::from("/var/mobile/Documents"))
        .join("tentaflow-ai")
}

#[cfg(target_os = "android")]
fn android_data_dir() -> PathBuf {
    // Wewnetrzna pamiec aplikacji Android
    // ndk-glue udostepnia context z getFilesDir()
    let base = std::env::var("ANDROID_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/data/data/ai.tentaflow.mobile/files"));
    base.join("tentaflow-ai")
}

// =============================================================================
// Inicjalizacja loggingu
// =============================================================================

/// Inicjalizuje logging specyficzny dla platformy
pub fn init_logging() {
    #[cfg(target_os = "android")]
    {
        android_logger::init_once(
            android_logger::Config::default()
                .with_max_level(log::LevelFilter::Debug)
                .with_tag("TentaFlowAI"),
        );
        info!("Logging zainicjalizowany (Android logcat)");
    }

    #[cfg(target_os = "ios")]
    {
        // iOS — tracing-subscriber z formatem compact (OSLog przez stdout)
        use tracing_subscriber::EnvFilter;

        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("info,iroh::net_report=error,iroh_relay=error,noq_proto=error,mdns_sd=off"));

        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .compact()
            .init();
    }

    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        // Fallback — standardowy tracing (dev/test)
        use tracing_subscriber::EnvFilter;

        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("info,tentaflow_core=debug,tentaflow_mobile=debug,iroh::net_report=error,iroh_relay=error,noq_proto=error,mdns_sd=off"));

        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(true)
            .init();
    }
}

// =============================================================================
// Informacje o urzadzeniu
// =============================================================================

/// Zwraca informacje o urzadzeniu mobilnym
pub fn device_info() -> DeviceInfo {
    #[cfg(target_os = "ios")]
    {
        ios_device_info()
    }

    #[cfg(target_os = "android")]
    {
        android_device_info()
    }

    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        DeviceInfo {
            model: "dev-host".to_string(),
            os_version: std::env::consts::OS.to_string(),
            ram_mb: 0,
        }
    }
}

#[cfg(target_os = "ios")]
fn ios_device_info() -> DeviceInfo {
    // Pobierz nazwe urzadzenia z Swift przez FFI
    let device_name = unsafe {
        let ptr = tentaflow_get_device_name();
        if ptr.is_null() {
            "iPhone".to_string()
        } else {
            let name = std::ffi::CStr::from_ptr(ptr).to_string_lossy().to_string();
            extern "C" { fn free(ptr: *mut std::ffi::c_void); }
            free(ptr as *mut std::ffi::c_void);
            name
        }
    };

    let ram_mb = unsafe { tentaflow_get_ram_mb() };

    DeviceInfo {
        model: device_name,
        os_version: "iOS".to_string(),
        ram_mb: ram_mb as u64,
    }
}

#[cfg(target_os = "ios")]
extern "C" {
    /// Zwraca nazwe urzadzenia (UIDevice.current.name) — caller musi zwolnic przez free()
    fn tentaflow_get_device_name() -> *mut std::ffi::c_char;
    /// Zwraca ilosc RAM w MB
    fn tentaflow_get_ram_mb() -> u64;
}

#[cfg(target_os = "android")]
fn android_device_info() -> DeviceInfo {
    // Odczyt z /proc/meminfo i android.os.Build
    let ram_mb = read_proc_meminfo().unwrap_or(0);

    DeviceInfo {
        model: std::env::var("ANDROID_MODEL").unwrap_or_else(|_| "Android".to_string()),
        os_version: std::env::var("ANDROID_VERSION").unwrap_or_else(|_| "unknown".to_string()),
        ram_mb,
    }
}

/// Odczytuje calkowita pamiec RAM z /proc/meminfo (Android/Linux)
#[cfg(target_os = "android")]
fn read_proc_meminfo() -> Option<u64> {
    let content = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in content.lines() {
        if line.starts_with("MemTotal:") {
            let kb: u64 = line
                .split_whitespace()
                .nth(1)?
                .parse()
                .ok()?;
            return Some(kb / 1024);
        }
    }
    None
}
