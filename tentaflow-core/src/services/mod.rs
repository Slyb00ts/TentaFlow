// =============================================================================
// Plik: services/mod.rs
// Opis: Klienci serwisow zewnetrznych — RAG, TTS, embeddingi.
//       Eksportuje klientow QUIC/HTTP do komunikacji z silnikami AI.
// =============================================================================

pub mod rag;
pub mod tts;
pub mod embeddings;
pub mod portainer;

pub use rag::{RAGClient, RAGEngineConfigCompat};
pub use tts::{TTSClient, TTSConfigCompat, SynthesizeCallback, TTSBufferingProcessor};
pub use embeddings::{EmbeddingsClient, EmbeddingsEngineConfigCompat};
