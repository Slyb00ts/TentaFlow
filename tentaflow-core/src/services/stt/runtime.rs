// =============================================================================
// File: services/stt/runtime.rs — SttRuntime
//
// Single owner STT path (D.3). Handler `/v1/audio/transcriptions`,
// flow STT adapter i `executor.execute_stt` wszystkie deleguja przez
// ten module. R5f (Codex blocker fix): owned dispatch — pre-R5f
// SttRuntime delegowal do `Router.route_audio_transcription` ktore
// po R3b.6 cutover stalo sie stub'em zwracajacym `AllBackendsUnavailable`,
// wiec Whisper STT byl wylaczony. Logika z dawnego `LocalSttHandler`
// przeniesiona tutaj jako primary backend.
//
// Dispatch order:
// 1. Local Whisper (`SttManager`) — jezeli engine zaladowany.
// 2. (Future) QUIC sidecar — gdy `service_type=stt` i quic backend.
// 3. (Future) Mesh forward przez executor / mesh manager.
// =============================================================================

use std::sync::Arc;

use tokio::sync::RwLock;

use crate::api::openai::types::{
    TranscriptionRequest, TranscriptionResponse, TranscriptionSegment,
};
use crate::error::{CoreError, Result};
use crate::stt::{SttManager, TranscribeParams};

use tracing::debug;

/// Single owner STT dispatch.
pub struct SttRuntime {
    stt_manager: Arc<RwLock<SttManager>>,
}

impl SttRuntime {
    pub fn new() -> Self {
        Self {
            stt_manager: crate::stt::shared_stt_manager(),
        }
    }

    /// Czy jest zaladowany jakikolwiek model STT (sync probe — uzywa
    /// try_read na RwLock, fallback do `false` gdy lock zajety).
    pub fn is_available_sync(&self) -> bool {
        match self.stt_manager.try_read() {
            Ok(mgr) => mgr.active_engine().map(|e| e.is_loaded()).unwrap_or(false),
            Err(_) => false,
        }
    }

    /// Transkrypcja pliku audio. Pusty `request.file` jest tu twardo
    /// odrzucany — to lustro guard'u w `routing/stt.rs` (R6.P3) zeby
    /// blad sygnalizowal sie jak najwczesniej.
    pub async fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> Result<TranscriptionResponse> {
        if request.file.is_empty() {
            return Err(CoreError::InvalidRequest {
                message: "transcription file is empty (0 bytes)".to_string(),
                details: Some(
                    "Send a non-empty audio file in the multipart `file` field.".to_string(),
                ),
            }
            .into());
        }

        // Brak language => Whisper auto-detection. Resolver w
        // `api/openai/server.rs` probuje user preference przed wpadnieciem
        // tutaj — None na tym poziomie oznacza ze user faktycznie nie ma
        // zadnej preferencji i chcemy auto-detect zamiast hardkodowanego "pl"
        // (powodowal cross-lang hallucination dla krotkich nagran).
        let params = TranscribeParams {
            audio_data: Arc::clone(&request.file),
            language: request.language.clone(),
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

        let result = {
            let mgr = self.stt_manager.read().await;
            let engine = mgr.active_engine().ok_or_else(|| CoreError::InternalError {
                message: "no STT engine loaded".to_string(),
                source: None,
            })?;
            engine
                .transcribe(params)
                .await
                .map_err(|e| CoreError::InternalError {
                    message: format!("STT engine: {}", e),
                    source: None,
                })?
        };

        debug!(
            "STT runtime: {} segmentow, {:.2}s audio",
            result.segments.len(),
            result.duration_seconds
        );

        // Filtruj segmenty wedlug progow z requestu (no_speech / avg_logprob /
        // compression_ratio). Pre-R5f LocalSttHandler robil to inline; po
        // migracji zostawiamy spojne zachowanie zeby `verbose_json` z progami
        // wciaz dzialal.
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

        // verbose_json zwraca segments z timestampami. Inne formaty pomijaja.
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

        // Tekst z przefiltrowanych segmentow (jezeli filtrowanie cos uciecho).
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
            speakers: None,
        })
    }
}

impl Default for SttRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::openai::types::SttRequestOptions;

    fn empty_request() -> TranscriptionRequest {
        TranscriptionRequest {
            file: std::sync::Arc::from(Vec::<u8>::new().into_boxed_slice()),
            filename: "audio.wav".into(),
            model: "whisper-1".into(),
            language: None,
            prompt: None,
            response_format: None,
            temperature: None,
            timestamp_granularities: None,
            no_speech_threshold: None,
            avg_logprob_threshold: None,
            compression_ratio_threshold: None,
            options: SttRequestOptions::default(),
        }
    }

    /// Empty file → typed `InvalidRequest` BEFORE reaching the engine
    /// (mirror of `routing/stt.rs` R6.P3 guard).
    #[tokio::test]
    async fn transcribe_rejects_empty_file_before_dispatch() {
        let runtime = SttRuntime::new();
        let err = runtime
            .transcribe(empty_request())
            .await
            .expect_err("empty file must reject");
        let core: CoreError = err.downcast().expect("CoreError downcast");
        assert!(matches!(core, CoreError::InvalidRequest { .. }));
    }

    /// Codex R5f Med2: non-empty audio reaches the engine. Without a
    /// loaded model the runtime returns typed `InternalError` ("no STT
    /// engine loaded") instead of silently producing empty output —
    /// proves the post-R5f path is wired through `SttManager` rather
    /// than hitting the Router stub.
    #[tokio::test]
    async fn transcribe_non_empty_reaches_engine_no_model_loaded() {
        let runtime = SttRuntime::new();
        let mut req = empty_request();
        req.file = std::sync::Arc::from(vec![0u8, 1, 2, 3].into_boxed_slice());
        let err = runtime
            .transcribe(req)
            .await
            .expect_err("no model loaded should error");
        let core: CoreError = err.downcast().expect("CoreError downcast");
        match core {
            CoreError::InternalError { message, .. } => {
                assert!(
                    message.contains("no STT engine loaded"),
                    "got message: {message}"
                );
            }
            other => panic!("expected InternalError, got {other:?}"),
        }
    }

    /// R2d (D.3): SttRequestOptions ma byc Default = wszystko false.
    #[test]
    fn stt_request_options_default_is_opt_in() {
        let opts = SttRequestOptions::default();
        assert!(!opts.speaker_identification);
        assert!(!opts.diarization);
        assert!(opts.timestamps.is_none());
        assert!(opts.response_format.is_none());
    }

    /// R2d (D.3): TranscriptionResponse ma nowe pole `speakers` typu
    /// `Option<Vec<SpeakerSegment>>`. Sprawdzamy ze pusta odpowiedz
    /// `text=""` z `speakers=None` serializuje sie i round-tripuje.
    #[test]
    fn transcription_response_speakers_field_round_trips() {
        use crate::api::openai::types::SpeakerSegment;
        let resp = TranscriptionResponse {
            text: "ahoj".into(),
            task: Some("transcribe".into()),
            language: Some("pl".into()),
            duration: Some(1.5),
            segments: None,
            speakers: Some(vec![SpeakerSegment {
                start: 0.0,
                end: 1.5,
                text: "ahoj".into(),
                speaker_label: "SPEAKER_00".into(),
                speaker_id: None,
                similarity: None,
            }]),
        };
        let json = serde_json::to_string(&resp).expect("serialize");
        let parsed: TranscriptionResponse =
            serde_json::from_str(&json).expect("round-trip");
        assert_eq!(parsed.text, "ahoj");
        assert_eq!(parsed.speakers.as_ref().map(|s| s.len()), Some(1));
        assert_eq!(
            parsed.speakers.as_ref().unwrap()[0].speaker_label,
            "SPEAKER_00"
        );
    }
}
