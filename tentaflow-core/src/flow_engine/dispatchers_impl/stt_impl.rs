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

        let user = build_user_context(req.user_id, req.user_role.as_deref());
        let api_req = TranscriptionRequest {
            file,
            filename,
            model: req.model,
            language: req.language,
            prompt: req.prompt,
            response_format: req.response_format,
            temperature: req.temperature,
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

        // Verbose pola serializujemy do JSON żeby przeszły przez SttResponse
        // (envelope artifacts) bez rozszerzania publicznego API
        // dispatcher'a o pełny TranscriptionSegment shape.
        let segments_json = response
            .segments
            .as_ref()
            .and_then(|segs| serde_json::to_string(segs).ok());
        let speakers_json = response
            .speakers
            .as_ref()
            .and_then(|sp| serde_json::to_string(sp).ok());

        Ok(SttResponse {
            text: response.text,
            detected_language: response.language,
            duration: response.duration,
            segments_json,
            speakers_json,
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
