// =============================================================================
// Plik: net/quic/handler_impls.rs
// Opis: Implementacja traitu RouterHandler dla typow z Core.
//       Laczy QuicServer (generyczny) z konkretnym typem Router.
// =============================================================================

use anyhow::Result;
use std::pin::Pin;
use std::future::Future;

use crate::routing::Router;
use super::server::RouterHandler;
use tentaflow_protocol::*;
use tracing::{debug, warn};

// =============================================================================
// Makro do obslugi bledow routingu
// =============================================================================

macro_rules! route_or_error {
    ($expr:expr, $label:expr, $request_id:expr) => {
        match $expr.await {
            Ok(response) => response,
            Err(e) => {
                tracing::error!("Blad {}: {}", $label, e);
                ModelResponse {
                    request_id: $request_id.to_string(),
                    result: ModelResult::Error(ErrorInfo {
                        error_type: ErrorType::InternalError,
                        message: e.to_string(),
                        details: None,
                    }),
                    metrics: None,
                }
            }
        }
    };
}

// =============================================================================
// RouterHandler dla Router
// =============================================================================

impl RouterHandler for Router {
    fn route_model_request(
        &self,
        request_bytes: &[u8],
        is_forwarded: bool,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + Send + '_>> {
        let bytes = request_bytes.to_vec();
        Box::pin(async move {
            debug!("route_model_request: start, {} bajtow", bytes.len());

            // Deserializuj przez rkyv (zero-copy access do archived data)
            let archived = rkyv::access::<ArchivedModelRequest, rkyv::rancor::Error>(&bytes)
                .map_err(|e| anyhow::anyhow!("Nie udalo sie zdeserializowac ModelRequest: {}", e))?;

            let request_id = archived.request_id.to_string();
            debug!("ModelRequest: id={}", request_id);

            // Routing na podstawie typu payload (konwersja z Archived na owned inline)
            let response = match &archived.payload {
                ArchivedModelPayload::Completion(comp) => {
                    debug!("route_model_request: payload = Completion");
                    let model = comp.model.to_string();
                    let messages: Vec<Message> = comp.messages.iter().map(|m| Message {
                        role: m.role.to_string(),
                        content: m.content.to_string(),
                    }).collect();
                    let temperature: Option<f32> = comp.temperature.as_ref().map(|t| (*t).into());
                    let max_tokens: Option<u32> = comp.max_tokens.as_ref().map(|t| (*t).into());
                    let prompt: Option<String> = comp.prompt.as_ref().map(|p| p.to_string());
                    let stop: Option<Vec<String>> = comp.stop.as_ref().map(|s| s.iter().map(|t| t.to_string()).collect());

                    route_or_error!(
                        self.route_completion_via_protocol(&model, messages, temperature, max_tokens, prompt, stop),
                        "completion", &request_id
                    )
                }
                ArchivedModelPayload::Embeddings(emb) => {
                    debug!("route_model_request: payload = Embeddings");
                    let model = emb.model.to_string();
                    let texts: Vec<String> = emb.input.iter().map(|s| s.to_string()).collect();
                    route_or_error!(
                        self.route_embeddings_via_quic(&model, texts),
                        "embeddings", &request_id
                    )
                }
                ArchivedModelPayload::RAG(rag) => {
                    debug!("route_model_request: payload = RAG");
                    let search_modes: Vec<SearchMode> = rag.search_modes.iter().map(|mode| {
                        match mode {
                            rkyv::Archived::<SearchMode>::FullTextSearch => SearchMode::FullTextSearch,
                            rkyv::Archived::<SearchMode>::VectorSearch => SearchMode::VectorSearch,
                            rkyv::Archived::<SearchMode>::HiRAG => SearchMode::HiRAG,
                            rkyv::Archived::<SearchMode>::GSW => SearchMode::GSW,
                        }
                    }).collect();
                    let rag_payload = RAGPayload {
                        query: rag.query.to_string(),
                        context: None,
                        params: RAGParams {
                            top_k: rag.params.top_k.into(),
                            min_similarity: rag.params.min_similarity.into(),
                            use_reranking: rag.params.use_reranking.as_ref().map(|v| (*v).into()),
                        },
                        requires_llm_processing: rag.requires_llm_processing.into(),
                        requires_audio_output: rag.requires_audio_output.into(),
                        search_modes,
                    };
                    route_or_error!(self.route_rag_payload(rag_payload), "RAG", &request_id)
                }
                ArchivedModelPayload::Audio(audio) => {
                    debug!("route_model_request: payload = Audio");
                    let operation = Self::convert_audio_operation(&audio.operation);
                    route_or_error!(self.route_audio_via_protocol(&operation), "audio", &request_id)
                }
                ArchivedModelPayload::Image(image) => {
                    debug!("route_model_request: payload = Image");
                    let operation = Self::convert_image_operation(&image.operation);
                    route_or_error!(self.route_image_via_protocol(&operation), "image", &request_id)
                }
                ArchivedModelPayload::Vision(vision) => {
                    debug!("route_model_request: payload = Vision");
                    let payload = VisionPayload {
                        model: vision.model.to_string(),
                        messages: vision.messages.iter().map(|m| {
                            VisionMessage {
                                role: m.role.to_string(),
                                content: m.content.iter().map(|part| {
                                    match part {
                                        ArchivedVisionContentPart::Text { text } => {
                                            VisionContentPart::Text { text: text.to_string() }
                                        }
                                        ArchivedVisionContentPart::ImageUrl { url, detail } => {
                                            VisionContentPart::ImageUrl {
                                                url: url.to_string(),
                                                detail: detail.as_ref().map(|d| d.to_string()),
                                            }
                                        }
                                    }
                                }).collect(),
                            }
                        }).collect(),
                        max_tokens: vision.max_tokens.as_ref().map(|t| (*t).into()),
                        temperature: vision.temperature.as_ref().map(|t| (*t).into()),
                    };
                    route_or_error!(self.route_vision_via_protocol(&payload), "vision", &request_id)
                }
                ArchivedModelPayload::Rerank(rerank) => {
                    debug!("route_model_request: payload = Rerank");
                    let payload = RerankPayload {
                        model: rerank.model.to_string(),
                        query: rerank.query.to_string(),
                        documents: rerank.documents.iter().map(|d| d.to_string()).collect(),
                        top_n: rerank.top_n.as_ref().map(|n| {
                            rkyv::deserialize::<usize, rkyv::rancor::Error>(n).unwrap_or(0)
                        }),
                        return_documents: rerank.return_documents.into(),
                    };
                    route_or_error!(self.route_rerank_via_quic(&payload), "rerank", &request_id)
                }
                ArchivedModelPayload::Memory(memory) => {
                    debug!("route_model_request: payload = Memory");
                    let payload = Self::convert_memory_payload(memory);
                    route_or_error!(self.route_memory_via_quic(&payload), "memory", &request_id)
                }
                ArchivedModelPayload::PrefixCacheInit(_) => {
                    warn!("PrefixCacheInit na Router QUIC server — powinno byc wyslane do LLM");
                    ModelResponse {
                        request_id: request_id.clone(),
                        result: ModelResult::Error(ErrorInfo {
                            error_type: ErrorType::InvalidRequest,
                            message: "PrefixCacheInit is not handled by Router QUIC server".to_string(),
                            details: Some("PrefixCacheInit should be sent to LLM server, not Router".to_string()),
                        }),
                        metrics: None,
                    }
                }
            };

            // Mesh fallback — jesli model nie znaleziony lokalnie, szukaj na zdalnym nodzie
            // Forwardowane requesty nie probuja mesh fallback (ochrona przed petla)
            let response = if is_forwarded {
                response
            } else {
                self.try_mesh_fallback(response, &archived.payload, &bytes).await
            };

            // Serializuj odpowiedz przez rkyv
            let response_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&response)
                .map_err(|e| anyhow::anyhow!("Nie udalo sie zserializowac ModelResponse: {}", e))?;

            Ok(response_bytes.into_vec())
        })
    }
}

// =============================================================================
// Mesh fallback — forwarding do zdalnego noda
// =============================================================================

impl Router {
    /// Sprawdza czy odpowiedz to blad "model not found" i probuje przekierowac przez mesh.
    /// Jesli model znaleziony na zdalnym nodzie — forwarduje surowe bajty requestu.
    async fn try_mesh_fallback(
        &self,
        response: ModelResponse,
        payload: &ArchivedModelPayload,
        raw_request_bytes: &[u8],
    ) -> ModelResponse {
        // Sprawdz czy odpowiedz to blad wskazujacy na brak serwisu lokalnie
        let is_not_found = match &response.result {
            ModelResult::Error(err) => err.error_type == ErrorType::ModelNotFound,
            _ => false,
        };

        if !is_not_found {
            return response;
        }

        // Wyciagnij typ serwisu i nazwe modelu z payloadu
        let (service_type, model_name) = Self::extract_service_info(payload);

        if service_type.is_empty() || model_name.is_empty() {
            return response;
        }

        debug!(
            "Mesh fallback: szukam serwisu typ='{}' model='{}' na zdalnych nodach",
            service_type, model_name
        );

        // Szukaj w mesh registry
        let location = self.service_manager.find_service(&service_type, &model_name);

        match location {
            Some(crate::routing::service_manager::ServiceLocation::MeshNode { node_id }) => {
                debug!(
                    "Mesh fallback: znaleziono model '{}' na nodzie '{}' — forwardowanie",
                    model_name, node_id
                );

                match self.route_through_mesh(&node_id, raw_request_bytes).await {
                    Ok(response_bytes) => {
                        match rkyv::access::<ArchivedModelResponse, rkyv::rancor::Error>(&response_bytes) {
                            Ok(archived_resp) => {
                                match rkyv::deserialize::<ModelResponse, rkyv::rancor::Error>(archived_resp) {
                                    Ok(mesh_response) => {
                                        debug!("Mesh fallback: odpowiedz z noda '{}' odebrana", node_id);
                                        return mesh_response;
                                    }
                                    Err(e) => {
                                        warn!("Mesh fallback: blad deserializacji odpowiedzi z noda '{}': {}", node_id, e);
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("Mesh fallback: blad dostepu do archived odpowiedzi z noda '{}': {}", node_id, e);
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Mesh fallback: blad forwardowania do noda '{}': {}", node_id, e);
                    }
                }
            }
            _ => {
                debug!("Mesh fallback: model '{}' nie znaleziony na zadnym nodzie", model_name);
            }
        }

        response
    }

    /// Wyciaga typ serwisu i nazwe modelu z ArchivedModelPayload
    fn extract_service_info(payload: &ArchivedModelPayload) -> (String, String) {
        match payload {
            ArchivedModelPayload::Completion(comp) => {
                ("llm".to_string(), comp.model.to_string())
            }
            ArchivedModelPayload::Embeddings(emb) => {
                ("embedding".to_string(), emb.model.to_string())
            }
            ArchivedModelPayload::RAG(_) => {
                ("rag".to_string(), String::new())
            }
            ArchivedModelPayload::Audio(audio) => {
                match &audio.operation {
                    ArchivedAudioOperation::TTS { model, .. } => ("tts".to_string(), model.to_string()),
                    ArchivedAudioOperation::STT { model, .. } => ("stt".to_string(), model.to_string()),
                    _ => ("stt".to_string(), String::new()),
                }
            }
            ArchivedModelPayload::Image(image) => {
                let model_name = match &image.operation {
                    ArchivedImageOperation::Generate { model, .. } => model.to_string(),
                    ArchivedImageOperation::Edit { model, .. } => model.to_string(),
                    ArchivedImageOperation::Variation { model, .. } => model.to_string(),
                };
                ("llm".to_string(), model_name)
            }
            ArchivedModelPayload::Vision(vision) => {
                ("llm".to_string(), vision.model.to_string())
            }
            ArchivedModelPayload::Rerank(rerank) => {
                ("embedding".to_string(), rerank.model.to_string())
            }
            ArchivedModelPayload::Memory(_) => {
                ("memory".to_string(), String::new())
            }
            ArchivedModelPayload::PrefixCacheInit(_) => {
                (String::new(), String::new())
            }
        }
    }
}

// =============================================================================
// Helpery konwersji Archived -> Owned
// =============================================================================

impl Router {
    /// Konwertuje ArchivedAudioOperation na owned AudioOperation
    fn convert_audio_operation(archived: &ArchivedAudioOperation) -> AudioOperation {
        match archived {
            ArchivedAudioOperation::TTS { model, input, voice, format, speed } => {
                AudioOperation::TTS {
                    model: model.to_string(),
                    input: input.to_string(),
                    voice: voice.to_string(),
                    format: format.as_ref().map(|f| f.to_string()),
                    speed: speed.as_ref().map(|s| (*s).into()),
                }
            }
            ArchivedAudioOperation::STT {
                model, audio_data, language, response_format, prompt,
                temperature, timestamp_granularities, no_speech_threshold,
                avg_logprob_threshold, compression_ratio_threshold,
            } => {
                AudioOperation::STT {
                    model: model.to_string(),
                    audio_data: audio_data.to_vec(),
                    language: language.as_ref().map(|l| l.to_string()),
                    response_format: response_format.as_ref().map(|r| r.to_string()),
                    prompt: prompt.as_ref().map(|p| p.to_string()),
                    temperature: temperature.as_ref().map(|t| (*t).into()),
                    timestamp_granularities: timestamp_granularities.as_ref().map(|v| {
                        v.iter().map(|s| s.to_string()).collect()
                    }),
                    no_speech_threshold: no_speech_threshold.as_ref().map(|t| (*t).into()),
                    avg_logprob_threshold: avg_logprob_threshold.as_ref().map(|t| (*t).into()),
                    compression_ratio_threshold: compression_ratio_threshold.as_ref().map(|t| (*t).into()),
                }
            }
            ArchivedAudioOperation::SpeakerEnroll { speaker_id, speaker_name, audio_samples, metadata } => {
                AudioOperation::SpeakerEnroll {
                    speaker_id: speaker_id.to_string(),
                    speaker_name: speaker_name.to_string(),
                    audio_samples: audio_samples.iter().map(|s| s.to_vec()).collect(),
                    metadata: metadata.iter().map(|tuple| (tuple.0.to_string(), tuple.1.to_string())).collect(),
                }
            }
            ArchivedAudioOperation::SpeakerAddSamples { speaker_id, audio_samples } => {
                AudioOperation::SpeakerAddSamples {
                    speaker_id: speaker_id.to_string(),
                    audio_samples: audio_samples.iter().map(|s| s.to_vec()).collect(),
                }
            }
            ArchivedAudioOperation::SpeakerRemove { speaker_id } => {
                AudioOperation::SpeakerRemove { speaker_id: speaker_id.to_string() }
            }
            ArchivedAudioOperation::SpeakerUpdateName { speaker_id, new_name } => {
                AudioOperation::SpeakerUpdateName {
                    speaker_id: speaker_id.to_string(),
                    new_name: new_name.to_string(),
                }
            }
            ArchivedAudioOperation::SpeakerList => AudioOperation::SpeakerList,
            ArchivedAudioOperation::SpeakerInfo => AudioOperation::SpeakerInfo,
            ArchivedAudioOperation::SpeakerIdentify { audio_data, threshold } => {
                AudioOperation::SpeakerIdentify {
                    audio_data: audio_data.to_vec(),
                    threshold: threshold.as_ref().map(|t| (*t).into()),
                }
            }
            ArchivedAudioOperation::SpeakerVerify { speaker_id, audio_data, threshold } => {
                AudioOperation::SpeakerVerify {
                    speaker_id: speaker_id.to_string(),
                    audio_data: audio_data.to_vec(),
                    threshold: threshold.as_ref().map(|t| (*t).into()),
                }
            }
            ArchivedAudioOperation::SpeakerIdentifyWithConfidence {
                audio_data, high_threshold, medium_threshold, audio_metadata,
            } => {
                AudioOperation::SpeakerIdentifyWithConfidence {
                    audio_data: audio_data.to_vec(),
                    high_threshold: high_threshold.as_ref().map(|t| (*t).into()),
                    medium_threshold: medium_threshold.as_ref().map(|t| (*t).into()),
                    audio_metadata: audio_metadata.as_ref().map(|m| {
                        m.iter().map(|tuple| (tuple.0.to_string(), tuple.1.to_string())).collect()
                    }),
                }
            }
            ArchivedAudioOperation::SpeakerConfirmIdentity {
                speaker_id, audio_data, add_sample, sample_metadata,
            } => {
                AudioOperation::SpeakerConfirmIdentity {
                    speaker_id: speaker_id.to_string(),
                    audio_data: audio_data.as_ref().map(|d| d.to_vec()),
                    add_sample: (*add_sample).into(),
                    sample_metadata: sample_metadata.as_ref().map(|m| {
                        m.iter().map(|tuple| (tuple.0.to_string(), tuple.1.to_string())).collect()
                    }),
                }
            }
            ArchivedAudioOperation::SpeakerLinkToMemory { speaker_id, memory_node_id, voice_id } => {
                AudioOperation::SpeakerLinkToMemory {
                    speaker_id: speaker_id.to_string(),
                    memory_node_id: (*memory_node_id).into(),
                    voice_id: voice_id.to_string(),
                }
            }
            ArchivedAudioOperation::WakeWordDetect { audio_data, wake_words, sensitivity, return_audio_after } => {
                AudioOperation::WakeWordDetect {
                    audio_data: audio_data.to_vec(),
                    wake_words: wake_words.as_ref().map(|ww| ww.iter().map(|s| s.to_string()).collect()),
                    sensitivity: sensitivity.as_ref().map(|s| (*s).into()),
                    return_audio_after: (*return_audio_after).into(),
                }
            }
            ArchivedAudioOperation::WakeWordConfigure {
                wake_words, sensitivity, min_detection_interval_ms, vad_enabled, vad_threshold,
            } => {
                AudioOperation::WakeWordConfigure {
                    wake_words: wake_words.iter().map(|s| s.to_string()).collect(),
                    sensitivity: (*sensitivity).into(),
                    min_detection_interval_ms: min_detection_interval_ms.as_ref().map(|v| (*v).into()),
                    vad_enabled: vad_enabled.as_ref().map(|v| (*v).into()),
                    vad_threshold: vad_threshold.as_ref().map(|v| (*v).into()),
                }
            }
            ArchivedAudioOperation::WakeWordStreamStart { wake_words, sensitivity, vad_enabled } => {
                AudioOperation::WakeWordStreamStart {
                    wake_words: wake_words.as_ref().map(|ww| ww.iter().map(|s| s.to_string()).collect()),
                    sensitivity: sensitivity.as_ref().map(|s| (*s).into()),
                    vad_enabled: vad_enabled.as_ref().map(|v| (*v).into()),
                }
            }
            ArchivedAudioOperation::WakeWordStreamChunk { audio_data, timestamp_ms } => {
                AudioOperation::WakeWordStreamChunk {
                    audio_data: audio_data.to_vec(),
                    timestamp_ms: (*timestamp_ms).into(),
                }
            }
            ArchivedAudioOperation::WakeWordStreamStop => AudioOperation::WakeWordStreamStop,
            ArchivedAudioOperation::ConversationStart { config } => {
                let mode = match &config.mode {
                    ArchivedSessionMode::AlwaysOn => SessionMode::AlwaysOn,
                    ArchivedSessionMode::WakeWordTimeout { silence_timeout_ms } => {
                        SessionMode::WakeWordTimeout { silence_timeout_ms: (*silence_timeout_ms).into() }
                    }
                    ArchivedSessionMode::WakeWordExplicitStop => SessionMode::WakeWordExplicitStop,
                };
                AudioOperation::ConversationStart {
                    config: ConversationSessionConfig {
                        mode,
                        wake_words: config.wake_words.iter().map(|s| s.to_string()).collect(),
                        stop_phrases: config.stop_phrases.iter().map(|s| s.to_string()).collect(),
                        wake_word_sensitivity: config.wake_word_sensitivity.into(),
                        vad_enabled: config.vad_enabled.into(),
                        vad_threshold: config.vad_threshold.into(),
                        play_activation_sound: config.play_activation_sound.into(),
                        play_deactivation_sound: config.play_deactivation_sound.into(),
                    },
                }
            }
            ArchivedAudioOperation::ConversationAudio { session_id, audio_data, timestamp_ms } => {
                AudioOperation::ConversationAudio {
                    session_id: session_id.to_string(),
                    audio_data: audio_data.to_vec(),
                    timestamp_ms: (*timestamp_ms).into(),
                }
            }
            ArchivedAudioOperation::ConversationEnd { session_id, reason } => {
                AudioOperation::ConversationEnd {
                    session_id: session_id.to_string(),
                    reason: reason.as_ref().map(|r| r.to_string()),
                }
            }
            ArchivedAudioOperation::ConversationStatus { session_id } => {
                AudioOperation::ConversationStatus {
                    session_id: session_id.to_string(),
                }
            }
        }
    }

    /// Konwertuje ArchivedImageOperation na owned ImageOperation
    fn convert_image_operation(archived: &ArchivedImageOperation) -> ImageOperation {
        match archived {
            ArchivedImageOperation::Generate { model, prompt, size, quality, n } => {
                ImageOperation::Generate {
                    model: model.to_string(),
                    prompt: prompt.to_string(),
                    size: size.as_ref().map(|s| s.to_string()),
                    quality: quality.as_ref().map(|q| q.to_string()),
                    n: n.as_ref().map(|v| (*v).into()),
                }
            }
            ArchivedImageOperation::Edit { model, image, mask, prompt, size, n } => {
                ImageOperation::Edit {
                    model: model.to_string(),
                    image: image.to_vec(),
                    mask: mask.as_ref().map(|m| m.to_vec()),
                    prompt: prompt.to_string(),
                    size: size.as_ref().map(|s| s.to_string()),
                    n: n.as_ref().map(|v| (*v).into()),
                }
            }
            ArchivedImageOperation::Variation { model, image, n, size } => {
                ImageOperation::Variation {
                    model: model.to_string(),
                    image: image.to_vec(),
                    n: n.as_ref().map(|v| (*v).into()),
                    size: size.as_ref().map(|s| s.to_string()),
                }
            }
        }
    }

    /// Konwertuje ArchivedMemoryPayload na owned MemoryPayload
    fn convert_memory_payload(archived: &ArchivedMemoryPayload) -> MemoryPayload {
        let operation = match &archived.operation {
            ArchivedMemoryOperation::Store { session_id, facts, context_embedding } => {
                MemoryOperation::Store {
                    session_id: session_id.to_string(),
                    facts: facts.iter().map(|f| MemoryFact {
                        subject: f.subject.to_string(),
                        relation: f.relation.to_string(),
                        object: f.object.to_string(),
                        confidence: f.confidence.into(),
                        source: f.source.as_ref().map(|s| s.to_string()),
                        metadata: f.metadata.as_ref().map(|m| m.iter().map(|pair| (pair.0.to_string(), pair.1.to_string())).collect()),
                    }).collect(),
                    context_embedding: context_embedding.as_ref().map(|e| e.iter().map(|v| (*v).into()).collect()),
                }
            }
            ArchivedMemoryOperation::Query { session_id, query, query_embedding, query_type, max_depth, top_k, include_reasoning } => {
                MemoryOperation::Query {
                    session_id: session_id.to_string(),
                    query: query.to_string(),
                    query_embedding: query_embedding.as_ref().map(|e| e.iter().map(|v| (*v).into()).collect()),
                    query_type: match query_type {
                        ArchivedMemoryQueryType::What => MemoryQueryType::What,
                        ArchivedMemoryQueryType::WhatCanDo => MemoryQueryType::WhatCanDo,
                        ArchivedMemoryQueryType::WhatFor => MemoryQueryType::WhatFor,
                        ArchivedMemoryQueryType::Where => MemoryQueryType::Where,
                        ArchivedMemoryQueryType::HowTo => MemoryQueryType::HowTo,
                        ArchivedMemoryQueryType::Why => MemoryQueryType::Why,
                        ArchivedMemoryQueryType::Similar => MemoryQueryType::Similar,
                        ArchivedMemoryQueryType::Pattern => MemoryQueryType::Pattern,
                    },
                    max_depth: max_depth.as_ref().map(|d| (*d).into()),
                    top_k: top_k.as_ref().map(|k| (*k).into()),
                    include_reasoning: include_reasoning.as_ref().map(|r| *r),
                }
            }
            ArchivedMemoryOperation::Consolidate { session_id, consolidate_all } => {
                MemoryOperation::Consolidate {
                    session_id: session_id.to_string(),
                    consolidate_all: *consolidate_all,
                }
            }
            ArchivedMemoryOperation::Stats { session_id } => {
                MemoryOperation::Stats {
                    session_id: session_id.as_ref().map(|s| s.to_string()),
                }
            }
            ArchivedMemoryOperation::Clear { session_id, preserve_long_term } => {
                MemoryOperation::Clear {
                    session_id: session_id.to_string(),
                    preserve_long_term: *preserve_long_term,
                }
            }
            ArchivedMemoryOperation::Feedback { session_id, node_id, feedback_type, value } => {
                MemoryOperation::Feedback {
                    session_id: session_id.to_string(),
                    node_id: (*node_id).into(),
                    feedback_type: match feedback_type {
                        ArchivedMemoryFeedbackType::Positive => MemoryFeedbackType::Positive,
                        ArchivedMemoryFeedbackType::Negative => MemoryFeedbackType::Negative,
                        ArchivedMemoryFeedbackType::Important => MemoryFeedbackType::Important,
                        ArchivedMemoryFeedbackType::Irrelevant => MemoryFeedbackType::Irrelevant,
                    },
                    value: (*value).into(),
                }
            }
            ArchivedMemoryOperation::LinkVoice { session_id, node_id, voice_id } => {
                MemoryOperation::LinkVoice {
                    session_id: session_id.to_string(),
                    node_id: (*node_id).into(),
                    voice_id: voice_id.to_string(),
                }
            }
            ArchivedMemoryOperation::FindByVoice { session_id, voice_id } => {
                MemoryOperation::FindByVoice {
                    session_id: session_id.to_string(),
                    voice_id: voice_id.to_string(),
                }
            }
            ArchivedMemoryOperation::UpdatePersonName { session_id, voice_id, node_id, new_name, preserve_history } => {
                MemoryOperation::UpdatePersonName {
                    session_id: session_id.to_string(),
                    voice_id: voice_id.as_ref().map(|v| v.to_string()),
                    node_id: node_id.as_ref().map(|n| (*n).into()),
                    new_name: new_name.to_string(),
                    preserve_history: *preserve_history,
                }
            }
        };

        MemoryPayload { operation }
    }
}


