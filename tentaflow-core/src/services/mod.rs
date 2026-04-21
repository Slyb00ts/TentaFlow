// =============================================================================
// Plik: services/mod.rs
// Opis: Klienci serwisow zewnetrznych — RAG, TTS, embeddingi.
//       Eksportuje klientow QUIC/HTTP do komunikacji z silnikami AI.
// =============================================================================

pub mod embeddings;
pub mod manifest;
pub mod portainer;
pub mod rag;
pub mod tts;

pub use embeddings::{EmbeddingsClient, EmbeddingsEngineConfigCompat};
pub use rag::{RAGClient, RAGEngineConfigCompat};
pub use tts::{SynthesizeCallback, TTSBufferingProcessor, TTSClient, TTSConfigCompat};
