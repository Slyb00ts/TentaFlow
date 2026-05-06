// =============================================================================
// Plik: flow_engine/dispatchers/tts.rs
// Opis: TtsDispatcher — wrapper nad executor.rs::execute_tts. Tekst wchodzi,
//       audio blob wychodzi. Voice opcjonalny (engine default fallback).
// =============================================================================

use anyhow::Result;
use async_trait::async_trait;

use crate::flow_engine::blob_store::BlobRef;

#[derive(Debug, Clone)]
pub struct TtsRequest {
    pub model: String,
    pub text: String,
    pub voice: Option<String>,
    pub format: Option<String>, // "wav" | "mp3" | "ogg" — engine-specific
    /// ISO-639-1 (np. "en", "pl") — backend wybiera locale syntezy. Etap 2.
    pub language: Option<String>,
    pub user_id: Option<i64>,
    pub user_role: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TtsResponse {
    pub audio: BlobRef,
    pub mime: String,
    pub sample_rate: Option<u32>,
}

#[async_trait]
pub trait TtsDispatcher: Send + Sync {
    async fn synthesize(&self, req: TtsRequest) -> Result<TtsResponse>;
}
