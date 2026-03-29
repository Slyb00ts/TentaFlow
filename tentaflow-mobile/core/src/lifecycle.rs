// =============================================================================
// Plik: lifecycle.rs
// Opis: Zarzadzanie cyklem zycia aplikacji mobilnej — pause/resume/memory
//       warning. Integracja z iOS UIApplicationDelegate i Android Activity.
// =============================================================================

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::{error, info, warn};

/// Stan cyklu zycia aplikacji mobilnej
pub struct MobileLifecycle {
    /// Czy aplikacja jest na pierwszym planie
    is_foreground: Arc<AtomicBool>,
    /// Czy otrzymano ostrzezenie o pamieci
    memory_warning: Arc<AtomicBool>,
}

impl MobileLifecycle {
    pub fn new() -> Self {
        Self {
            is_foreground: Arc::new(AtomicBool::new(true)),
            memory_warning: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Wywolywane gdy aplikacja przechodzi w tlo
    /// iOS: applicationDidEnterBackground
    /// Android: onPause
    pub fn on_pause(&self) {
        info!("Aplikacja przechodzi w tlo");
        self.is_foreground.store(false, Ordering::SeqCst);
    }

    /// Wywolywane gdy aplikacja wraca na pierwszy plan
    /// iOS: applicationWillEnterForeground
    /// Android: onResume
    pub fn on_resume(&self) {
        info!("Aplikacja wraca na pierwszy plan");
        self.is_foreground.store(true, Ordering::SeqCst);
        self.memory_warning.store(false, Ordering::SeqCst);
    }

    /// Wywolywane przy ostrzezeniu o niskiej pamieci
    /// iOS: didReceiveMemoryWarning
    /// Android: onTrimMemory
    ///
    /// Automatycznie wyladowuje zaladowany model aby zwolnic pamiec.
    pub fn on_memory_warning(&self) {
        warn!("Ostrzezenie o niskiej pamieci — wyladowywanie modelu");
        self.memory_warning.store(true, Ordering::SeqCst);

        // Wyladuj model w tle przez wspoldzielony InferenceManager
        let inference = tentaflow_core::inference::shared_inference_manager();

        // Uzyj tokio runtime handle do wywolania async unload
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let mut mgr = inference.write().await;
                if let Err(e) = mgr.unload_model().await {
                    error!("Blad wyladowywania modelu przy memory warning: {}", e);
                } else {
                    info!("Model wyladowany z powodu ostrzezenia o pamieci");
                }
            });
        } else {
            warn!("Brak tokio runtime — nie mozna wyladowac modelu asynchronicznie");
        }
    }

    pub fn is_foreground(&self) -> bool {
        self.is_foreground.load(Ordering::SeqCst)
    }

    pub fn has_memory_warning(&self) -> bool {
        self.memory_warning.load(Ordering::SeqCst)
    }
}

// FFI eksporty do wywolania z natywnego kodu iOS/Android

static mut LIFECYCLE: Option<MobileLifecycle> = None;

/// Inicjalizuje lifecycle manager (wywolywane raz przy starcie)
pub fn init_lifecycle() -> &'static MobileLifecycle {
    unsafe {
        LIFECYCLE = Some(MobileLifecycle::new());
        LIFECYCLE.as_ref().unwrap()
    }
}

/// Zwraca referencje do lifecycle managera
pub fn get_lifecycle() -> Option<&'static MobileLifecycle> {
    unsafe { LIFECYCLE.as_ref() }
}

#[no_mangle]
pub extern "C" fn tentaflow_on_pause() {
    if let Some(lifecycle) = get_lifecycle() {
        lifecycle.on_pause();
    }
}

#[no_mangle]
pub extern "C" fn tentaflow_on_resume() {
    if let Some(lifecycle) = get_lifecycle() {
        lifecycle.on_resume();
    }
}

#[no_mangle]
pub extern "C" fn tentaflow_on_memory_warning() {
    if let Some(lifecycle) = get_lifecycle() {
        lifecycle.on_memory_warning();
    }
}
