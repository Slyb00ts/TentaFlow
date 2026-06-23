// =============================================================================
// Plik: api/openai/mod.rs
// Opis: Implementacja protokolu OpenAI API (kompatybilnego z OpenAI, Azure OpenAI,
//       Anthropic, i innymi zgodnymi API). Obsluguje Chat Completions, Vision,
//       Image Generation, Audio TTS/STT, Embeddings.
// =============================================================================

pub mod types;

pub mod anthropic;

pub mod comfyui;

pub mod openapi;

pub mod server;

pub use types::*;

pub use server::OpenAIServer;

pub use server::OpenAIBody;
