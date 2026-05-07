// =============================================================================
// Plik: flow_engine/dispatchers_impl/stt_impl.rs
// Opis: SttDispatcherImpl — wrapper nad
//       `services::runtime::executor::ModelRuntimeExecutor::execute_stt`.
//       Stage 3d-0b: D4 invariant relaxed. Capability dispatcher impl
//       woła executor (parity z LLM/TTS/Embeddings) — single source of
//       truth backend dispatch. Audio bytes pobieramy z BlobStore i
//       budujemy `TranscriptionRequest` przekazywany do executor'a.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use std::sync::Arc;

use super::{build_user_context, ModelRuntimeSlot};
use crate::api::openai::types::{SttRequestOptions, TranscriptionRequest};
use crate::flow_engine::blob_store::BlobStore;
use crate::flow_engine::dispatchers::{SttDispatcher, SttRequest, SttResponse};
use crate::services::runtime::context::ExecutionContext as RuntimeContext;

pub struct SttDispatcherImpl {
    runtime: ModelRuntimeSlot,
    blobs: Arc<dyn BlobStore>,
}

impl SttDispatcherImpl {
    pub fn new(runtime: ModelRuntimeSlot, blobs: Arc<dyn BlobStore>) -> Self {
        Self { runtime, blobs }
    }
}

#[async_trait]
impl SttDispatcher for SttDispatcherImpl {
    async fn transcribe(&self, req: SttRequest) -> Result<SttResponse> {
        let runtime = self
            .runtime
            .read()
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow!("SttDispatcher: ModelRuntimeExecutor not wired"))?;

        let bytes = self.blobs.get(&req.audio).await?;
        let file: Arc<[u8]> = bytes.into();

        let mime = req.audio.mime.clone();
        let filename = blob_filename(&req.audio.id, &mime);

        // SttRequest nie ma jeszcze user_id/user_role (rozszerzenie planu w
        // późniejszym kroku 3d). Build user context jako None — executor
        // wewnętrznie waliduje ACL przez catalog provider, nie wymaga user
        // context dla blocking transcribe path.
        let user = build_user_context(None, None);
        let api_req = TranscriptionRequest {
            file,
            filename,
            model: req.model,
            language: req.language,
            prompt: None,
            response_format: None,
            temperature: None,
            timestamp_granularities: None,
            no_speech_threshold: None,
            avg_logprob_threshold: None,
            compression_ratio_threshold: None,
            options: SttRequestOptions::default(),
        };

        let mut rctx = RuntimeContext::new(user);
        let response = runtime
            .execute_stt(api_req, &mut rctx)
            .await
            .map_err(|e| anyhow!("SttDispatcher execute_stt: {e}"))?;

        Ok(SttResponse {
            text: response.text,
            detected_language: response.language,
        })
    }
}

/// Buduje pseudo-nazwę pliku dla SttRuntime — engine używa tylko rozszerzenia
/// w logach/format detection. BlobRef.id (uuid) wystarcza za stable handle.
fn blob_filename(id: &str, mime: &str) -> String {
    let ext = match mime {
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/mpeg" => "mp3",
        "audio/ogg" => "ogg",
        "audio/flac" => "flac",
        "audio/webm" => "webm",
        _ => "bin",
    };
    format!("{id}.{ext}")
}
