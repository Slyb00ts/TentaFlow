// =============================================================================
// Plik: flow_engine/dispatchers/stt.rs
// Opis: SttDispatcher — wrapper nad services/stt/runtime.rs::transcribe.
//       Adapter STT dostaje audio blob, zwraca tekst. Język opcjonalny —
//       silnik może auto-detect (np. Whisper).
// =============================================================================

use anyhow::Result;
use async_trait::async_trait;

use crate::flow_engine::blob_store::BlobRef;

#[derive(Debug, Clone, Default)]
pub struct SttRequest {
    pub model: String,
    pub audio: BlobRef,
    pub language: Option<String>,
    /// Stage 3d-0b-4-fix: prompt/temperature/response_format propagacja z
    /// API request przez flow envelope do backendu (whisper.cpp).
    pub prompt: Option<String>,
    pub temperature: Option<f32>,
    /// "json" | "text" | "verbose_json" | "srt" | "vtt" — backend gateway.
    /// Dla verbose_json adapter wypełnia language/duration/segments na
    /// envelope artifacts.
    pub response_format: Option<String>,
    pub user_id: Option<i64>,
    pub user_role: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SttResponse {
    pub text: String,
    pub detected_language: Option<String>,
    /// Stage 3d-0b-4-fix: verbose pola propagowane gdy backend je zwrócił.
    /// Adapter STT zapisuje do envelope artifacts żeby
    /// `flow_outcome_to_stt_response` mógł odbudować TranscriptionResponse.
    pub duration: Option<f32>,
    pub segments_json: Option<String>,
    pub speakers_json: Option<String>,
}

#[async_trait]
pub trait SttDispatcher: Send + Sync {
    async fn transcribe(&self, req: SttRequest) -> Result<SttResponse>;
}
