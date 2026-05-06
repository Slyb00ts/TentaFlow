// =============================================================================
// Plik: flow_engine/dispatchers_impl/stt_impl.rs
// Opis: SttDispatcherImpl — wrapper nad `services::stt::SttRuntime::transcribe`.
//       Plan v4.2 D4 omija `executor.rs::execute_stt` żeby nie ciągnąć całego
//       resolvera/strategy. Audio blob jest pobierany z `BlobStore` (rkyv
//       przenosi tylko BlobRef), runtime dostaje `Arc<[u8]>` i miele lokalnie.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use std::sync::Arc;

use crate::api::openai::types::{SttRequestOptions, TranscriptionRequest};
use crate::flow_engine::blob_store::BlobStore;
use crate::flow_engine::dispatchers::{SttDispatcher, SttRequest, SttResponse};
use crate::services::stt::SttRuntime;

pub type SttRuntimeSlot = Arc<parking_lot::RwLock<Option<Arc<SttRuntime>>>>;

pub struct SttDispatcherImpl {
    runtime: SttRuntimeSlot,
    blobs: Arc<dyn BlobStore>,
}

impl SttDispatcherImpl {
    pub fn new(runtime: SttRuntimeSlot, blobs: Arc<dyn BlobStore>) -> Self {
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
            .ok_or_else(|| anyhow!("SttDispatcher: SttRuntime not wired"))?;

        let bytes = self.blobs.get(&req.audio).await?;
        let file: Arc<[u8]> = Arc::from(bytes.to_vec());

        let mime = req.audio.mime.clone();
        let filename = blob_filename(&req.audio.id, &mime);

        let transcription_req = TranscriptionRequest {
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

        let response = runtime
            .transcribe(transcription_req)
            .await
            .map_err(|e| anyhow!("SttDispatcher transcribe failed: {e}"))?;

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
