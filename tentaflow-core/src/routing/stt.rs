// =============================================================================
// Plik: routing/stt.rs
// Opis: Obsluga zapytan STT — route_audio_transcription (OpenAI API),
//       route_audio_via_protocol (protocol-native z filtrami segmentow),
//       route_speaker_operation, route_speaker_link_to_memory.
// =============================================================================

use crate::api::openai::types::{TranscriptionRequest, TranscriptionResponse};
use crate::error::Result;
use crate::routing::router::Router;

use tracing::{debug, error};

impl Router {
    /// Routuje audio transcription request do odpowiedniego backendu.
    ///
    /// Probuje QUIC STT (preferowany), potem fallback na HTTP backend.
    /// Obsluguje zarowno prosty tekst jak i verbose_json z segmentami.
    /// Wariant z user context — ACL gate przed wywolaniem backendu.
    pub async fn route_audio_transcription_for_user(
        &self,
        request: TranscriptionRequest,
        user: Option<crate::auth::acl::UserContext>,
    ) -> Result<crate::routing::RouteResult<TranscriptionResponse>> {
        if let Some(ref u) = user {
            if let Some(ref db) = self.db {
                if !crate::auth::acl::check_access_safe(
                    db,
                    "model",
                    &request.model,
                    u.user_id,
                    &u.role,
                ) {
                    tracing::warn!(user_id = u.user_id, model = %request.model, "ACL denied STT model");
                    return Err(crate::error::CoreError::ModelNotFound {
                        model_name: request.model.clone(),
                    }
                    .into());
                }
            }
        }
        // Delegate through the executor — single dispatch surface for all
        // four routes (chat / embeddings / tts / stt). `execute_stt` is a
        // thin wrapper over `SttRuntime` (D.3 single owner) so this path
        // ends up in the same place as the legacy `Router.stt_runtime()`
        // delegation; routing through the executor keeps the
        // `routing/*` -> `services/runtime` -> backend layering uniform.
        let executor_snapshot = self.executor.read().clone();
        if let Some(executor) = executor_snapshot {
            use crate::services::runtime::context::ExecutionContext;
            use crate::services::runtime::executor::ExecutorError;
            let mut exec_ctx = ExecutionContext {
                user: user.clone(),
                ..ExecutionContext::default()
            };
            match executor.execute_stt(request.clone(), &mut exec_ctx).await {
                Ok(response) => {
                    return Ok(crate::routing::RouteResult {
                        response,
                        metadata: crate::routing::RouteMetadata {
                            served_by_node: hostname::get()
                                .map(|h| h.to_string_lossy().to_string())
                                .unwrap_or_else(|_| "unknown".to_string()),
                            backend_type: "executor".to_string(),
                            strategy_used: "executor".to_string(),
                            fallbacks_tried: 0,
                            hop_count: 0,
                            latency_ms: None,
                        usage: None,
                        finish_reason: None,
                        },
                    });
                }
                Err(ExecutorError::SttRuntimeUnavailable) => {
                    // Codex R3b.5+6 H1: fall back ONLY for executor-not-ready;
                    // real STT errors must surface so we don't re-dispatch
                    // the same expensive transcription.
                    tracing::debug!(
                        "STT runtime not wired in executor, falling back to legacy route_audio_transcription"
                    );
                }
                Err(ExecutorError::SttBackend(msg)) => {
                    return Err(crate::error::CoreError::InternalError {
                        message: format!("STT backend error: {}", msg),
                        source: None,
                    }
                    .into());
                }
                Err(other) => {
                    return Err(crate::error::CoreError::InternalError {
                        message: format!("executor.execute_stt: {}", other),
                        source: None,
                    }
                    .into());
                }
            }
        }
        self.route_audio_transcription(request).await
    }

    pub async fn route_audio_transcription(
        &self,
        request: TranscriptionRequest,
    ) -> Result<crate::routing::RouteResult<TranscriptionResponse>> {
        // R6.P3: empty file is a client bug — surface immediately rather
        // than dispatch to a backend that will fail or return empty
        // transcription. Mirrors the chat audio guard.
        if request.file.is_empty() {
            return Err(crate::error::CoreError::InvalidRequest {
                message: "transcription file is empty (0 bytes)".to_string(),
                details: Some(
                    "Send a non-empty audio file in the multipart `file` field.".to_string(),
                ),
            }
            .into());
        }

        // After R3b.8 the legacy `BackendHandle` STT dispatch is gone.
        // Surface a typed error when the executor (and therefore SttRuntime)
        // is not wired — DB-less router or `Router::start` not run.
        let model_name = request.model.clone();
        let _ = request;
        Err(crate::error::CoreError::AllBackendsUnavailable { model_name }.into())
    }


    /// Routuje audio request przez protocol-native interface.
    ///
    /// Obsluguje wszystkie AudioOperation: TTS, STT (z filtrowaniem segmentow),
    /// Speaker operations, Wake Word, Conversation sessions.
    pub async fn route_audio_via_protocol(
        &self,
        operation: &tentaflow_protocol::AudioOperation,
    ) -> Result<tentaflow_protocol::ModelResponse> {
        use tentaflow_protocol::*;

        let request_id = uuid::Uuid::new_v4().to_string();

        match operation {
            AudioOperation::TTS {
                model,
                input,
                voice,
                format,
                speed,
                language,
            } => {
                debug!("Audio TTS: model={}, dlugosc_tekstu={}", model, input.len());

                // Uzyj synthesize_speech() ktora obsluguje QUIC TTS (preferowany) i HTTP fallback
                let tts_request = crate::api::openai::types::TTSRequest {
                    model: model.clone(),
                    input: input.clone(),
                    voice: voice.clone(),
                    response_format: format.clone(),
                    speed: *speed,
                    language: language.clone(),
                };

                match self.synthesize_speech(&tts_request).await {
                    Ok(tts_result) => {
                        let audio_bytes = tts_result.response.bytes;
                        let response = ModelResponse {
                            request_id,
                            result: ModelResult::Audio(AudioResult {
                                data: AudioResultData::Audio(audio_bytes),
                                model: model.clone(),
                            }),
                            metrics: None,
                        };
                        Ok(response)
                    }
                    Err(e) => {
                        error!("Blad TTS: {}", e);
                        Ok(ModelResponse {
                            request_id,
                            result: ModelResult::Error(ErrorInfo {
                                error_type: ErrorType::InternalError,
                                message: format!("Blad syntezy mowy: {}", e),
                                details: None,
                            }),
                            metrics: None,
                        })
                    }
                }
            }
            AudioOperation::STT {
                model,
                audio_data,
                language,
                response_format,
                prompt,
                temperature,
                timestamp_granularities,
                no_speech_threshold,
                avg_logprob_threshold,
                compression_ratio_threshold,
                extra_params: _,
            } => {
                debug!(
                    "Audio STT: model={}, rozmiar_audio={} bajtow, response_format={:?}",
                    model,
                    audio_data.len(),
                    response_format
                );

                // Jesli uzytkownik chce filtrowania, wymuszamy verbose_json aby dostac segmenty
                let needs_segments = no_speech_threshold.is_some()
                    || avg_logprob_threshold.is_some()
                    || compression_ratio_threshold.is_some();

                let effective_format =
                    if needs_segments && response_format.as_deref() != Some("verbose_json") {
                        debug!("Wymuszam verbose_json bo filtrowanie jest wlaczone");
                        Some("verbose_json".to_string())
                    } else {
                        response_format.clone()
                    };

                // Utworz request transkrypcji i przekaz do route_audio_transcription
                let request = crate::api::openai::types::TranscriptionRequest {
                    file: std::sync::Arc::from(audio_data.clone().into_boxed_slice()),
                    filename: "audio.wav".to_string(),
                    model: model.clone(),
                    language: language.clone(),
                    prompt: prompt.clone(),
                    response_format: effective_format.clone(),
                    temperature: *temperature,
                    timestamp_granularities: timestamp_granularities.clone(),
                    no_speech_threshold: *no_speech_threshold,
                    avg_logprob_threshold: *avg_logprob_threshold,
                    compression_ratio_threshold: *compression_ratio_threshold,
                    options: crate::api::openai::types::SttRequestOptions::default(),
                };

                // Codex R3b.5+6 M3: protocol-native STT goes through the
                // same executor entry point as `/v1/audio/transcriptions`
                // so the resolver / SttRuntime contract is uniform across
                // mesh reverse and HTTP. `route_audio_transcription` is
                // still hit as fallback for DB-less / executor-not-ready.
                let executor_snapshot = self.executor.read().clone();
                let stt_dispatch = match executor_snapshot {
                    Some(executor) => {
                        use crate::services::runtime::context::ExecutionContext;
                        use crate::services::runtime::executor::ExecutorError;
                        let mut exec_ctx = ExecutionContext::default();
                        match executor.execute_stt(request.clone(), &mut exec_ctx).await {
                            Ok(response) => Ok(crate::routing::RouteResult {
                                response,
                                metadata: crate::routing::RouteMetadata {
                                    served_by_node: hostname::get()
                                        .map(|h| h.to_string_lossy().to_string())
                                        .unwrap_or_else(|_| "unknown".to_string()),
                                    backend_type: "executor".to_string(),
                                    strategy_used: "executor".to_string(),
                                    fallbacks_tried: 0,
                                    hop_count: 0,
                                    latency_ms: None,
                        usage: None,
                        finish_reason: None,
                                },
                            }),
                            Err(ExecutorError::SttRuntimeUnavailable) => {
                                self.route_audio_transcription(request).await
                            }
                            Err(e) => Err(crate::error::CoreError::InternalError {
                                message: format!("executor.execute_stt: {}", e),
                                source: None,
                            }
                            .into()),
                        }
                    }
                    None => self.route_audio_transcription(request).await,
                };
                match stt_dispatch {
                    Ok(route_result) => {
                        let transcription = route_result.response;
                        // Sprawdz czy mamy segmenty i czy trzeba filtrowac
                        let is_verbose = effective_format.as_deref() == Some("verbose_json");

                        if is_verbose {
                            if let Some(segments) = transcription.segments {
                                // Zapamietaj oryginalna liczbe segmentow
                                let original_count = segments.len();

                                // Filtruj segmenty jesli sa progi
                                let filtered_segments: Vec<_> = segments.into_iter()
                                    .filter(|seg| {
                                        // Sprawdz no_speech_prob threshold
                                        if let Some(threshold) = no_speech_threshold {
                                            if seg.no_speech_prob >= *threshold {
                                                debug!("Odrzucono segment id={}: no_speech_prob={} >= {}",
                                                    seg.id, seg.no_speech_prob, threshold);
                                                return false;
                                            }
                                        }
                                        // Sprawdz avg_logprob threshold
                                        if let Some(threshold) = avg_logprob_threshold {
                                            if seg.avg_logprob < *threshold {
                                                debug!("Odrzucono segment id={}: avg_logprob={} < {}",
                                                    seg.id, seg.avg_logprob, threshold);
                                                return false;
                                            }
                                        }
                                        // Sprawdz compression_ratio threshold
                                        if let Some(threshold) = compression_ratio_threshold {
                                            if seg.compression_ratio > *threshold {
                                                debug!("Odrzucono segment id={}: compression_ratio={} > {}",
                                                    seg.id, seg.compression_ratio, threshold);
                                                return false;
                                            }
                                        }
                                        true
                                    })
                                    .collect();

                                // Zrekonstruuj tekst z przefiltrowanych segmentow
                                let filtered_text = filtered_segments
                                    .iter()
                                    .map(|seg| seg.text.as_str())
                                    .collect::<Vec<_>>()
                                    .join("");

                                // Oblicz liczbe odfiltrowanych segmentow
                                let filtered_count = original_count - filtered_segments.len();

                                debug!(
                                    "STT verbose: {} segmentow po filtracji (z {}), odrzucono {}",
                                    filtered_segments.len(),
                                    original_count,
                                    filtered_count
                                );

                                // Jesli user prosil o verbose_json, zwroc Detailed
                                if response_format.as_deref() == Some("verbose_json") {
                                    // Konwertuj segmenty do Protocol format
                                    let protocol_segments: Vec<TranscriptionSegment> =
                                        filtered_segments
                                            .iter()
                                            .map(|seg| TranscriptionSegment {
                                                id: seg.id,
                                                seek: seg.seek,
                                                start: seg.start,
                                                end: seg.end,
                                                text: seg.text.clone(),
                                                tokens: Some(seg.tokens.clone()),
                                                temperature: seg.temperature,
                                                avg_logprob: seg.avg_logprob,
                                                compression_ratio: seg.compression_ratio,
                                                no_speech_prob: seg.no_speech_prob,
                                                speaker_label: seg.speaker_label.clone(),
                                                speaker_similarity: seg.speaker_similarity,
                                                is_known_speaker: seg.is_known_speaker,
                                            })
                                            .collect();

                                    return Ok(ModelResponse {
                                        request_id,
                                        result: ModelResult::Audio(AudioResult {
                                            data: AudioResultData::Detailed {
                                                text: filtered_text,
                                                segments: protocol_segments,
                                                language: transcription
                                                    .language
                                                    .unwrap_or_default(),
                                                duration: transcription.duration.unwrap_or(0.0),
                                                filtered_segments_count: Some(
                                                    filtered_count as u32,
                                                ),
                                            },
                                            model: model.clone(),
                                        }),
                                        metrics: None,
                                    });
                                }

                                // User prosil tylko o filtrowanie, nie o verbose - zwroc Text
                                return Ok(ModelResponse {
                                    request_id,
                                    result: ModelResult::Audio(AudioResult {
                                        data: AudioResultData::Text(filtered_text),
                                        model: model.clone(),
                                    }),
                                    metrics: None,
                                });
                            }
                        }

                        // Brak segmentow lub nie verbose - zwroc surowy tekst
                        Ok(ModelResponse {
                            request_id,
                            result: ModelResult::Audio(AudioResult {
                                data: AudioResultData::Text(transcription.text),
                                model: model.clone(),
                            }),
                            metrics: None,
                        })
                    }
                    Err(e) => {
                        error!("Blad STT: {}", e);
                        Ok(ModelResponse {
                            request_id,
                            result: ModelResult::Error(ErrorInfo {
                                error_type: ErrorType::InternalError,
                                message: format!("Blad transkrypcji audio: {}", e),
                                details: None,
                            }),
                            metrics: None,
                        })
                    }
                }
            }

            // === SPEAKER OPERATIONS ===
            // Forward to QUIC STT service which handles speaker enrollment/identification
            AudioOperation::SpeakerEnroll {
                speaker_id,
                speaker_name,
                audio_samples,
                metadata: _metadata,
            } => {
                debug!(
                    "Speaker Enroll: id={}, name={}, samples={}",
                    speaker_id,
                    speaker_name,
                    audio_samples.len()
                );
                self.route_speaker_operation(operation, &request_id).await
            }

            AudioOperation::SpeakerAddSamples {
                speaker_id,
                audio_samples,
            } => {
                debug!(
                    "Speaker AddSamples: id={}, samples={}",
                    speaker_id,
                    audio_samples.len()
                );
                self.route_speaker_operation(operation, &request_id).await
            }

            AudioOperation::SpeakerRemove { speaker_id } => {
                debug!("Speaker Remove: id={}", speaker_id);
                self.route_speaker_operation(operation, &request_id).await
            }

            AudioOperation::SpeakerList => {
                debug!("Speaker List");
                self.route_speaker_operation(operation, &request_id).await
            }

            AudioOperation::SpeakerInfo => {
                debug!("Speaker Info");
                self.route_speaker_operation(operation, &request_id).await
            }

            AudioOperation::SpeakerIdentify {
                audio_data,
                threshold,
            } => {
                debug!(
                    "Speaker Identify: audio_size={}, threshold={:?}",
                    audio_data.len(),
                    threshold
                );
                self.route_speaker_operation(operation, &request_id).await
            }

            AudioOperation::SpeakerVerify {
                speaker_id,
                audio_data,
                threshold,
            } => {
                debug!(
                    "Speaker Verify: id={}, audio_size={}, threshold={:?}",
                    speaker_id,
                    audio_data.len(),
                    threshold
                );
                self.route_speaker_operation(operation, &request_id).await
            }

            // === VOICE RECOGNITION FLOW OPERATIONS ===
            AudioOperation::SpeakerIdentifyWithConfidence {
                audio_data,
                high_threshold,
                medium_threshold,
                audio_metadata: _audio_metadata,
            } => {
                debug!("Speaker IdentifyWithConfidence: audio_size={}, high_threshold={:?}, medium_threshold={:?}",
                    audio_data.len(), high_threshold, medium_threshold);
                self.route_speaker_operation(operation, &request_id).await
            }

            AudioOperation::SpeakerConfirmIdentity {
                speaker_id,
                audio_data,
                add_sample,
                sample_metadata: _sample_metadata,
            } => {
                debug!(
                    "Speaker ConfirmIdentity: id={}, add_sample={}, has_audio={}",
                    speaker_id,
                    add_sample,
                    audio_data.is_some()
                );
                self.route_speaker_operation(operation, &request_id).await
            }

            AudioOperation::SpeakerLinkToMemory {
                speaker_id,
                memory_node_id,
                voice_id,
            } => {
                debug!(
                    "Speaker LinkToMemory: speaker_id={}, memory_node_id={}, voice_id={}",
                    speaker_id, memory_node_id, voice_id
                );
                // To wymaga polaczenia z Memory - uzywamy route_speaker_operation
                // ktore przekaze do STT, a STT moze wywolac Memory (lub Router robi to bezposrednio)
                self.route_speaker_link_to_memory(
                    speaker_id,
                    *memory_node_id,
                    voice_id,
                    &request_id,
                )
                .await
            }

            // =========================================================================
            // WAKE WORD DETECTION - routuje do STT service
            // =========================================================================
            AudioOperation::WakeWordDetect {
                audio_data,
                wake_words,
                sensitivity,
                return_audio_after: _return_audio_after,
            } => {
                debug!(
                    "WakeWord Detect: audio_size={}, wake_words={:?}, sensitivity={:?}",
                    audio_data.len(),
                    wake_words.as_ref().map(|w| w.len()),
                    sensitivity
                );
                self.route_speaker_operation(operation, &request_id).await
            }

            AudioOperation::WakeWordConfigure {
                wake_words,
                sensitivity,
                min_detection_interval_ms: _min_detection_interval_ms,
                vad_enabled,
                vad_threshold: _vad_threshold,
            } => {
                debug!(
                    "WakeWord Configure: words={:?}, sensitivity={}, vad={}",
                    wake_words,
                    sensitivity,
                    vad_enabled.unwrap_or(true)
                );
                self.route_speaker_operation(operation, &request_id).await
            }

            AudioOperation::WakeWordStreamStart {
                wake_words,
                sensitivity,
                vad_enabled,
            } => {
                debug!(
                    "WakeWord StreamStart: words={:?}, sensitivity={:?}, vad={:?}",
                    wake_words, sensitivity, vad_enabled
                );
                self.route_speaker_operation(operation, &request_id).await
            }

            AudioOperation::WakeWordStreamChunk {
                audio_data,
                timestamp_ms,
            } => {
                debug!(
                    "WakeWord StreamChunk: audio_size={}, timestamp={}ms",
                    audio_data.len(),
                    timestamp_ms
                );
                self.route_speaker_operation(operation, &request_id).await
            }

            AudioOperation::WakeWordStreamStop => {
                debug!("WakeWord StreamStop");
                self.route_speaker_operation(operation, &request_id).await
            }

            // Conversation Session Operations - routed to STT service
            AudioOperation::ConversationStart { config } => {
                debug!(
                    "Conversation Start: mode={:?}, wake_words={:?}",
                    config.mode, config.wake_words
                );
                self.route_speaker_operation(operation, &request_id).await
            }

            AudioOperation::ConversationAudio {
                session_id,
                audio_data,
                timestamp_ms,
            } => {
                debug!(
                    "Conversation Audio: session={}, audio_size={}, timestamp={}ms",
                    session_id,
                    audio_data.len(),
                    timestamp_ms
                );
                self.route_speaker_operation(operation, &request_id).await
            }

            AudioOperation::ConversationEnd { session_id, reason } => {
                debug!(
                    "Conversation End: session={}, reason={:?}",
                    session_id, reason
                );
                self.route_speaker_operation(operation, &request_id).await
            }

            AudioOperation::ConversationStatus { session_id } => {
                debug!("Conversation Status: session={}", session_id);
                self.route_speaker_operation(operation, &request_id).await
            }

            AudioOperation::SpeakerUpdateName {
                speaker_id,
                new_name,
            } => {
                debug!(
                    "Speaker Update Name: speaker_id={}, new_name={}",
                    speaker_id, new_name
                );
                self.route_speaker_operation(operation, &request_id).await
            }
        }
    }

    /// Routuje operacje speaker do QUIC STT service.
    ///
    /// Speaker operations (enrollment, identification, etc.) sa obslugiwane przez
    /// serwis STT ktory ma zaladowany model embeddingów i baze mowcow.
    async fn route_speaker_operation(
        &self,
        operation: &tentaflow_protocol::AudioOperation,
        request_id: &str,
    ) -> Result<tentaflow_protocol::ModelResponse> {
        use tentaflow_protocol::*;

        // Znajdz QUIC STT service
        let stt_service_name = self.service_manager.get_first_stt_service_name();

        let quic_client = if let Some(ref service_name) = stt_service_name {
            self.service_manager.get_quic_stt_client(service_name).await
        } else {
            None
        };

        let quic_client = match quic_client {
            Some(client) => client,
            None => {
                error!("Brak QUIC STT service dla operacji speaker");
                return Ok(ModelResponse {
                    request_id: request_id.to_string(),
                    result: ModelResult::Error(ErrorInfo {
                        error_type: ErrorType::InternalError,
                        message: "Serwis STT niedostepny - speaker operations wymagaja QUIC STT"
                            .to_string(),
                        details: None,
                    }),
                    metrics: None,
                });
            }
        };

        // Zbuduj ModelRequest
        let model_request = ModelRequest {
            request_id: request_id.to_string(),
            payload: ModelPayload::Audio(AudioPayload {
                operation: operation.clone(),
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        // Wyslij przez QUIC
        match quic_client.send_request(model_request).await {
            Ok(response) => Ok(response),
            Err(e) => {
                error!("Blad QUIC STT dla speaker operation: {}", e);
                Ok(ModelResponse {
                    request_id: request_id.to_string(),
                    result: ModelResult::Error(ErrorInfo {
                        error_type: ErrorType::InternalError,
                        message: format!("Blad komunikacji z STT service: {}", e),
                        details: None,
                    }),
                    metrics: None,
                })
            }
        }
    }

    /// Linkuje glos (speaker_id) do osoby w Memory (memory_node_id).
    ///
    /// Flow:
    /// 1. Wywoluje LinkVoice w Memory zeby zapisac voice_id w node
    /// 2. Zwraca wynik operacji
    ///
    /// Ta operacja laczy dwa systemy:
    /// - STT: speaker_id z bazy glosow
    /// - Memory: node_id osoby w grafie wiedzy
    async fn route_speaker_link_to_memory(
        &self,
        speaker_id: &str,
        memory_node_id: u64,
        voice_id: &str,
        request_id: &str,
    ) -> Result<tentaflow_protocol::ModelResponse> {
        use tentaflow_protocol::*;

        debug!(
            "LinkToMemory: linking speaker_id={} -> memory_node_id={} with voice_id={}",
            speaker_id, memory_node_id, voice_id
        );

        let quic_client = self
            .service_manager
            .find_quic_client_for_model("memory")
            .await;

        let Some(memory_client) = quic_client else {
            return Ok(ModelResponse {
                request_id: request_id.to_string(),
                result: ModelResult::Error(ErrorInfo {
                    error_type: ErrorType::InternalError,
                    message: "Memory service unavailable for LinkVoice".to_string(),
                    details: None,
                }),
                metrics: None,
            });
        };

        // Utworz request do Memory
        let memory_request = ModelRequest {
            request_id: request_id.to_string(),
            payload: ModelPayload::Memory(MemoryPayload {
                operation: MemoryOperation::LinkVoice {
                    session_id: "voice_link".to_string(),
                    node_id: memory_node_id,
                    voice_id: voice_id.to_string(),
                },
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        // Wyslij do Memory
        match memory_client.send_request(memory_request).await {
            Ok(response) => {
                // Przekonwertuj MemoryResult na SpeakerLinkToMemoryResult
                match response.result {
                    ModelResult::Memory(memory_result) => match memory_result.result_type {
                        MemoryResultType::LinkVoice(link_result) => Ok(ModelResponse {
                            request_id: request_id.to_string(),
                            result: ModelResult::Audio(AudioResult {
                                data: AudioResultData::SpeakerLinkToMemoryResult {
                                    speaker_id: speaker_id.to_string(),
                                    memory_node_id,
                                    voice_id: voice_id.to_string(),
                                    success: link_result.success,
                                },
                                model: "memory".to_string(),
                            }),
                            metrics: None,
                        }),
                        _ => Ok(ModelResponse {
                            request_id: request_id.to_string(),
                            result: ModelResult::Error(ErrorInfo {
                                error_type: ErrorType::InternalError,
                                message: "Unexpected Memory result type".to_string(),
                                details: None,
                            }),
                            metrics: None,
                        }),
                    },
                    ModelResult::Error(err) => Ok(ModelResponse {
                        request_id: request_id.to_string(),
                        result: ModelResult::Error(err),
                        metrics: None,
                    }),
                    _ => Ok(ModelResponse {
                        request_id: request_id.to_string(),
                        result: ModelResult::Error(ErrorInfo {
                            error_type: ErrorType::InternalError,
                            message: "Unexpected response type from Memory".to_string(),
                            details: None,
                        }),
                        metrics: None,
                    }),
                }
            }
            Err(e) => {
                error!("Memory LinkVoice error: {}", e);
                Ok(ModelResponse {
                    request_id: request_id.to_string(),
                    result: ModelResult::Error(ErrorInfo {
                        error_type: ErrorType::InternalError,
                        message: format!("Memory LinkVoice failed: {}", e),
                        details: None,
                    }),
                    metrics: None,
                })
            }
        }
    }
}
