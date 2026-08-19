// =============================================================================
// Plik: lib.rs
// Opis: TentaFlow Core — wspolna biblioteka dla Router, Desktop i Mobile.
// =============================================================================

// Self-alias umozliwia proc-macrom z `tentaflow-macros` uzywac bezwzglednej
// sciezki `::tentaflow_core::dispatch::*` zarowno w zewnetrznych crate'ach jak
// i w kodzie samego tentaflow-core (handlerom).
extern crate self as tentaflow_core;

pub mod audio_models;
pub mod benchmark;
pub mod code_studio;
pub mod compliance;
pub mod config;
pub mod crypto;
pub mod db;
pub mod error;
pub mod events;
pub mod flow_engine;
pub mod flow_runtime;
pub mod hub;
pub mod inference;
pub mod memory;
pub mod mesh;
pub mod metrics;
pub mod ml_studio;
pub mod net;
pub mod project_studio;
pub mod prompt_registry;
pub mod routing;
pub mod scheduler;
pub mod services;
pub mod services_repo;
pub mod skills;
pub mod skills_hub;
pub mod stt;
pub mod sync;
pub mod tts;
pub mod vision;
pub mod vision_models;
#[cfg(all(unix, feature = "camera", feature = "inference-vision-gpu"))]
pub mod vision_worker;
pub mod web_research;

// macos_ffi: dlopen helpery dla libMLXBridge.dylib. Zawsze aktywne na
// macOS/iOS (apple-tts), oraz pod feature flags mlx-whisper/mlx-kokoro
// gdziekolwiek indziej (te dwa featury wlaczaja go na desktop macOS,
// gdzie target gate i tak by go aktywował, ale zachowujemy explicit dla
// czytelnosci). Na Linux/Windows: niedostepne.
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    feature = "inference-mlx-whisper",
    feature = "inference-mlx-kokoro"
))]
pub mod macos_ffi;

#[cfg(feature = "inference-diarization")]
pub mod diarization;

pub mod api;
pub mod audit;
pub mod auth;
pub mod dispatch;
pub mod util;

pub mod addon;
pub mod agents;
pub mod deploy;
pub mod license;
pub mod lifecycle_signal;
pub mod meeting;
pub mod paths;
pub mod profiling;
pub mod system_check;

#[cfg(target_os = "windows")]
pub mod firewall_check;
