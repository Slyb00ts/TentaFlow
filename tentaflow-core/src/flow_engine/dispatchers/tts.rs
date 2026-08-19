// =============================================================================
// Plik: flow_engine/dispatchers/tts.rs
// Opis: TtsDispatcher — wrapper nad executor.rs::execute_tts. Tekst wchodzi,
//       audio blob wychodzi. Voice opcjonalny (engine default fallback).
// =============================================================================

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::BoxStream;
use tokio_util::sync::CancellationToken;

use crate::flow_engine::blob_store::BlobRef;
use crate::flow_engine::dispatcher::CallProvenance;
pub use crate::flow_engine::envelope::AudioStreamChunk as TtsStreamChunk;

#[derive(Debug, Clone)]
pub struct TtsRequest {
    pub model: String,
    pub text: String,
    pub voice: Option<String>,
    pub format: Option<String>, // "wav" | "mp3" | "ogg" — engine-specific
    /// ISO-639-1 (np. "en", "pl") — backend wybiera locale syntezy. Etap 2.
    pub language: Option<String>,
    /// Stage 3d-0b-2-fix: speed multiplier (1.0 = normalna prędkość mowy).
    /// Backend embedded ignoruje gdy nie wspiera (whisper-cpp skips); HTTP
    /// backendy przepisują w request body.
    pub speed: Option<f32>,
    pub user_id: Option<String>,
    pub user_role: Option<String>,
    /// Etap 3c: cancel signal dla stream_synthesize (klient disconnect).
    /// Blocking `synthesize` ignoruje (nie ma chunked emit do przerwania).
    pub cancel_token: CancellationToken,
    /// §2.5 — server-minted provenance of the flow this call belongs to, copied
    /// from the node's `ExecutionContext`. Threaded for the same reason as
    /// `flow_depth`: the runtime context the dispatcher builds re-enters the
    /// executor, and an alias resolving onto a flow surface starts THAT flow
    /// with this stamp. Not derived from request content.
    pub provenance: CallProvenance,
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

    /// Etap 3c: streaming TTS. Backendy native streaming yield chunki w
    /// czasie syntezy; backendy blocking — chunkowane post-blocking
    /// (chunkowane PCM @ 100 ms = 3200 B per frame przy 16 kHz mono i16).
    async fn stream_synthesize(
        &self,
        req: TtsRequest,
    ) -> Result<BoxStream<'static, Result<TtsStreamChunk>>>;
}
