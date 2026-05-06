// =============================================================================
// Plik: flow_engine/dispatchers_impl/tts_impl.rs
// Opis: TtsDispatcherImpl — wrapper nad
//       `ModelRuntimeExecutor::execute_tts`. Audio bytes lądują w `BlobStore`,
//       BlobRef wraca przez TtsResponse. Voice ma sensowny default ("alloy")
//       gdy adapter nie wymusi konkretnego — zgodne z OpenAI compat surface.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use std::sync::Arc;

use crate::api::openai::types::TTSRequest;
use crate::flow_engine::blob_store::BlobStore;
use crate::flow_engine::dispatchers::{TtsDispatcher, TtsRequest, TtsResponse};
use crate::services::runtime::context::ExecutionContext as RuntimeContext;
use crate::services::runtime::executor::ModelRuntimeExecutor;

const DEFAULT_VOICE: &str = "alloy";

pub struct TtsDispatcherImpl {
    runtime: Arc<ModelRuntimeExecutor>,
    blobs: Arc<dyn BlobStore>,
}

impl TtsDispatcherImpl {
    pub fn new(runtime: Arc<ModelRuntimeExecutor>, blobs: Arc<dyn BlobStore>) -> Self {
        Self { runtime, blobs }
    }
}

#[async_trait]
impl TtsDispatcher for TtsDispatcherImpl {
    async fn synthesize(&self, req: TtsRequest) -> Result<TtsResponse> {
        if req.text.is_empty() {
            return Err(anyhow!("TtsDispatcher: empty text"));
        }

        let api_req = TTSRequest {
            model: req.model,
            input: req.text,
            voice: req.voice.unwrap_or_else(|| DEFAULT_VOICE.to_string()),
            response_format: req.format.clone(),
            speed: None,
            language: None,
        };

        let mut rctx = RuntimeContext::new(None);
        let result = self
            .runtime
            .execute_tts(api_req, &mut rctx)
            .await
            .map_err(|e| anyhow!("TtsDispatcher: {e}"))?;

        let mime = format_to_mime(&result.format);
        let blob_ref = self.blobs.put(result.bytes, &mime).await?;

        Ok(TtsResponse {
            audio: blob_ref,
            mime,
            sample_rate: None,
        })
    }
}

/// Mapuje format z `TtsExecutionResult.format` (nazwa kodeka albo
/// rozszerzenie) na MIME type. Embedded TTS zawsze emituje WAV; HTTP/QUIC
/// echo'ują requestowy format. Nieznane formaty traktujemy jako
/// `application/octet-stream`.
fn format_to_mime(format: &str) -> String {
    match format.to_ascii_lowercase().as_str() {
        "wav" | "audio/wav" | "audio/x-wav" => "audio/wav".into(),
        "mp3" | "mpeg" | "audio/mpeg" => "audio/mpeg".into(),
        "opus" | "audio/opus" => "audio/opus".into(),
        "aac" | "audio/aac" => "audio/aac".into(),
        "flac" | "audio/flac" => "audio/flac".into(),
        "pcm" | "audio/pcm" => "audio/pcm".into(),
        "ogg" | "audio/ogg" => "audio/ogg".into(),
        _ => "application/octet-stream".into(),
    }
}
