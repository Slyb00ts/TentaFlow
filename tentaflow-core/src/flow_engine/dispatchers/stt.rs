// =============================================================================
// Plik: flow_engine/dispatchers/stt.rs
// Opis: SttDispatcher — wrapper nad services/stt/runtime.rs::transcribe.
//       Adapter STT dostaje audio blob, zwraca tekst. Język opcjonalny —
//       silnik może auto-detect (np. Whisper).
// =============================================================================

use anyhow::Result;
use async_trait::async_trait;

use crate::flow_engine::blob_store::BlobRef;

#[derive(Debug, Clone)]
pub struct SttRequest {
    pub model: String,
    pub audio: BlobRef,
    pub language: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SttResponse {
    pub text: String,
    pub detected_language: Option<String>,
}

#[async_trait]
pub trait SttDispatcher: Send + Sync {
    async fn transcribe(&self, req: SttRequest) -> Result<SttResponse>;
}
