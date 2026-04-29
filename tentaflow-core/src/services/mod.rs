// =============================================================================
// Plik: services/mod.rs
// Opis: Klienci serwisow zewnetrznych — RAG, TTS, embeddingi.
//       Eksportuje klientow QUIC/HTTP do komunikacji z silnikami AI.
// =============================================================================

pub mod embeddings;
pub mod manifest;
pub mod portainer;
pub mod rag;
pub mod teams_bot_bootstrap;
pub mod tts;

// Unified services refactor (Phase 1 — additive, runs alongside legacy code).
pub mod deploy;
pub mod lifecycle;
pub mod ports;
pub mod registry;
pub mod supervisor;
pub mod transport;

pub use embeddings::{EmbeddingsClient, EmbeddingsEngineConfigCompat};
pub use rag::{RAGClient, RAGEngineConfigCompat};
pub use tts::{SynthesizeCallback, TTSBufferingProcessor, TTSClient, TTSConfigCompat};
