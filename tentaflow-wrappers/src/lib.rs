// =============================================================================
// Plik: lib.rs
// Opis: Publiczne moduły własnych wrapperów TentaFlow dla natywnych silników.
// Przykład: use tentaflow_wrappers::llama::LlamaLoadConfig;
// =============================================================================

pub mod llama;
pub mod llama_engine;
pub mod native;
pub mod whisper;

#[cfg(feature = "sherpa")]
pub mod sherpa {
    pub use sherpa_rs_sys as sys;
}

pub use native::{NativeError, NativeLayout, NativePlatform};
