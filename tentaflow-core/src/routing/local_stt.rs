// =============================================================================
// Plik: routing/local_stt.rs
// Opis: Adapter konwertujacy OpenAI-compatible TranscriptionRequest na lokalne
//       wywolania SttEngine. Obsluguje transkrypcje audio przez Whisper backend.
// =============================================================================

use crate::api::openai::types::{
    TranscriptionRequest, TranscriptionResponse, TranscriptionSegment,
};
use crate::stt::{SttManager, TranscribeParams};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::debug;

pub struct LocalSttHandler {
    stt_manager: Arc<RwLock<SttManager>>,
}

impl LocalSttHandler {
    pub fn new(manager: Arc<RwLock<SttManager>>) -> Self {
        Self {
            stt_manager: manager,
        }
    }

    /// Czy jest zaladowany jakikolwiek model STT
    pub async fn is_available(&self) -> bool {
        let mgr = self.stt_manager.read().await;
        mgr.active_engine().map(|e| e.is_loaded()).unwrap_or(false)
    }

    /// Synchroniczna wersja is_available — uzywa try_read() na RwLock
    pub fn is_available_sync(&self) -> bool {
        match self.stt_manager.try_read() {
            Ok(mgr) => mgr.active_engine().map(|e| e.is_loaded()).unwrap_or(false),
            Err(_) => false,
        }
    }

    /// Obsluga transkrypcji przez lokalny silnik STT
    pub async fn transcribe(
        &self,
        request: &TranscriptionRequest,
    ) -> anyhow::Result<TranscriptionResponse> {
        // Zbuduj TranscribeParams z TranscriptionRequest.
        // Domyslny jezyk: polski (jesli request.language nie jest ustawiony)
        let params = TranscribeParams {
            audio_data: request.file.clone(),
            language: request.language.clone().or_else(|| Some("pl".to_string())),
            translate: false,
            word_timestamps: request
                .timestamp_granularities
                .as_ref()
                .map(|g| g.iter().any(|s| s == "word"))
                .unwrap_or(false),
            temperature: request.temperature,
            no_speech_threshold: request.no_speech_threshold,
            initial_prompt: request.prompt.clone(),
        };

        // Wywolaj silnik STT
        let result = {
            let mgr = self.stt_manager.read().await;
            let engine = mgr
                .active_engine()
                .ok_or_else(|| anyhow::anyhow!("Brak zaladowanego modelu STT"))?;
            engine.transcribe(params).await?
        };

        debug!(
            "Lokalna transkrypcja STT: {} segmentow, {:.2}s audio",
            result.segments.len(),
            result.duration_seconds
        );

        // Filtruj segmenty wedlug progow z requestu
        let filtered_segments: Vec<_> = result
            .segments
            .iter()
            .filter(|seg| {
                if let Some(thr) = request.no_speech_threshold {
                    if seg.no_speech_prob >= thr {
                        return false;
                    }
                }
                if let Some(thr) = request.avg_logprob_threshold {
                    if seg.avg_logprob < thr {
                        return false;
                    }
                }
                if let Some(thr) = request.compression_ratio_threshold {
                    if seg.compression_ratio > thr {
                        return false;
                    }
                }
                true
            })
            .collect();

        // Konwertuj TranscribeResult -> TranscriptionResponse
        let segments = if request.response_format.as_deref() == Some("verbose_json") {
            Some(
                filtered_segments
                    .iter()
                    .map(|seg| TranscriptionSegment {
                        id: seg.id,
                        seek: 0,
                        start: seg.start as f32,
                        end: seg.end as f32,
                        text: seg.text.clone(),
                        tokens: seg.tokens.iter().map(|&t| t as u32).collect(),
                        temperature: 0.0,
                        avg_logprob: seg.avg_logprob,
                        compression_ratio: seg.compression_ratio,
                        no_speech_prob: seg.no_speech_prob,
                        speaker_label: None,
                        speaker_similarity: None,
                        is_known_speaker: None,
                    })
                    .collect(),
            )
        } else {
            None
        };

        // Tekst z przefiltrowanych segmentow
        let text = if filtered_segments.len() < result.segments.len() {
            filtered_segments
                .iter()
                .map(|seg| seg.text.as_str())
                .collect::<Vec<_>>()
                .join("")
        } else {
            result.text
        };

        Ok(TranscriptionResponse {
            text,
            task: Some("transcribe".to_string()),
            language: Some(result.language),
            duration: Some(result.duration_seconds as f32),
            segments,
        })
    }
}
