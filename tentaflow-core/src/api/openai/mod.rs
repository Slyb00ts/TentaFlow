// =============================================================================
// Plik: api/openai/mod.rs
// Opis: Implementacja protokolu OpenAI API (kompatybilnego z OpenAI, Azure OpenAI,
//       Anthropic, i innymi zgodnymi API). Obsluguje Chat Completions, Vision,
//       Image Generation, Audio TTS/STT, Embeddings.
// =============================================================================

pub mod types;

#[cfg(feature = "dashboard-api")]
pub mod server;

pub use types::*;

#[cfg(feature = "dashboard-api")]
pub use server::OpenAIServer;

#[cfg(feature = "dashboard-api")]
pub use server::OpenAIBody;
