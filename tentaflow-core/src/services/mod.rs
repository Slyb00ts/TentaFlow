// =============================================================================
// Plik: services/mod.rs
// Opis: Klienci serwisow zewnetrznych — RAG, TTS, embeddingi.
//       Eksportuje klientow QUIC/HTTP do komunikacji z silnikami AI.
// =============================================================================

pub mod manifest;
pub mod model_download;
pub mod models;
pub mod nim;
pub mod portainer;
pub mod rag;
pub mod teams_bot_bootstrap;
pub mod tts;

// Unified services refactor (Phase 1 — additive, runs alongside legacy code).
pub mod auto_detect;
pub mod deploy;
pub mod handles_cache;
pub mod lifecycle;
pub mod mesh_registry;
pub mod ports;
pub mod registry;
pub mod snapshot_builder;
pub mod supervisor;
pub mod transport;

pub use rag::{RAGClient, RAGEngineConfigCompat};
pub use tts::{SynthesizeCallback, TTSBufferingProcessor, TTSClient, TTSConfigCompat};
