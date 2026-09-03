// =============================================================================
// Plik: lib.rs
// Opis: Publiczne moduły własnych wrapperów TentaFlow dla natywnych silników.
// Przykład: use tentaflow_wrappers::llama::LlamaLoadConfig;
// =============================================================================

pub mod llama;
pub mod llama_engine;
pub mod native;
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub mod whisper;

#[cfg(feature = "sherpa")]
pub mod sherpa {
    pub use sherpa_rs_sys as sys;
}

pub use native::{NativeError, NativeLayout, NativePlatform};
