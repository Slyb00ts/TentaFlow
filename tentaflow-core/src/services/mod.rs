// =============================================================================
// Plik: services/mod.rs
// Opis: Klienci serwisow zewnetrznych — TTS, embeddingi.
//       Eksportuje klientow QUIC/HTTP do komunikacji z silnikami AI.
// =============================================================================

pub mod manifest;
pub mod model_download;
pub mod models;
pub mod nim;
pub mod portainer;
pub mod stt;
pub mod teams_bot_bootstrap;
pub mod tts;

// Unified services refactor (Phase 1 — additive, runs alongside legacy code).
pub mod auto_detect;
pub mod backend;
pub mod catalog;
pub mod deploy;
pub mod handles_cache;
pub mod lifecycle;
pub mod mesh_registry;
pub mod ports;
pub mod registry;
pub mod runtime;
pub mod snapshot_builder;
pub mod supervisor;
pub mod transport;

pub use tts::{TTSClient, TTSConfigCompat};
