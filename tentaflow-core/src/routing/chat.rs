// =============================================================================
// Plik: routing/chat.rs
// Opis: Obsluga zapytan chat completion — non-streaming route, flow engine,
//       audio input processing, speaker identification, intent analysis,
//       RAG routing (non-streaming), QUIC LLM routing, callback handler,
//       protocol-native completion, memory store.
// =============================================================================

use crate::config::RouterConfig;
use crate::error::{Result, CoreError};
use crate::flow_engine::converter;
use crate::flow_engine::types::FlowExecutionResult;
use crate::api::openai::types::{
    ChatCompletionRequest, ChatCompletionResponse,
    Choice, Message, MessageContent, Usage,
    TranscriptionRequest,
};
use crate::routing::router::{
    Router, RequestMetrics, SpeakerIdentifyResult, DiarizedSpeaker,
    VoiceInfo, SttWithDiarization,
};
use crate::routing::service_manager::ServiceManager;

use tentaflow_protocol::*;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

impl Router {
    /// Routuje chat completion request do odpowiedniego backendu lub RAG engine.
    ///
    /// Algorytm:
    /// 1. Rozwiaz alias modelu (jesli uzywany)
    /// 2. Sprawdz czy to RAG model -> route do RAG engine
    /// 3. Jesli nie RAG, znajdz standardowy model pool
    /// 4. Wybierz backend z pool (load balancing)
    /// 5. Wyslij request do backendu/RAG
    /// 6. Zwroc response
    pub async fn route_chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse> {
        let mut metrics = RequestMetrics::new();

        // === FLOW ENGINE: proba wykonania przez konfigurowalny flow ===
        if let Some(ref dispatcher) = self.flow_dispatcher {
            let ctx = crate::routing::build_flow_context(&request, false);

            match dispatcher.try_dispatch(&request.model, "chat", ctx).await {
                Ok(Some(result)) => {
                    let response = flow_result_to_chat_response(result, &request.model);
                    self.process_memory_store_async(&request, &response, None);
                    return Ok(response);
                }
                Ok(None) => {}
                Err(e) => {
                    warn!("Flow Engine error, fallback na stary pipeline: {}", e);
                }
            }
        }

        // === AUDIO INPUT PROCESSING ===
        let t0 = std::time::Instant::now();
        let (request, voice_info) = self.process_audio_input(request).await?;
        if voice_info.is_some() {
            metrics.stt_ms = Some(t0.elapsed().as_millis() as u64);
        }

        // === MEMORY INTEGRATION (QUERY) ===
        let (request, query_decision, mem_timings) = self.memory_integration.process_request(request).await?;
        metrics.query_analysis_ms = mem_timings.query_analysis_ms;
        metrics.memory_query_ms = mem_timings.memory_query_ms;

        // === ALIAS RETENTAFLOWN ===
        let model_name = self.resolve_to_service_name(&request.model);
        metrics.model_name = Some(model_name.clone());

        // === ROUTE TO APPROPRIATE BACKEND ===
        let t2 = std::time::Instant::now();
        let mut response = if self.is_local_inference_model(&model_name) {
            // Lokalna inferencja in-process (MLX, llama.cpp) — bez sieci
            debug!("Routing '{}' do lokalnej inferencji in-process", model_name);
            self.local_inference.handle_chat_completion(&request).await?
        } else if self.is_rag_model(&model_name) {
            self.route_to_rag(model_name.clone(), request.clone()).await?
        } else if self.is_quic_llm_model(&model_name) {
            self.route_to_quic_llm(model_name.clone(), request.clone(), None, None).await?
        } else {
            let backends = self.get_service_backends(&model_name)
                .ok_or_else(|| CoreError::ModelNotFound { model_name: model_name.clone() })?;
            if backends.is_empty() {
                return Err(CoreError::AllBackendsUnavailable { model_name: model_name.clone() }.into());
            }
            let strategy = self.get_strategy(&model_name)
                .ok_or_else(|| CoreError::ModelNotFound { model_name: model_name.clone() })?;
            let backend_idx = strategy.select_backend(backends)?;
            backends[backend_idx].chat_completion(request.clone()).await?
        };
        metrics.llm_inference_ms = Some(t2.elapsed().as_millis() as u64);

        // === MEMORY STORE (async) ===
        self.process_memory_store_async(&request, &response, query_decision);

        // === ADD VOICE INFO TO RESPONSE ===
        if let Some(info) = voice_info {
            response.transcribed_text = Some(info.transcribed_text);
            response.speaker_id = info.speaker_id;
            response.speaker_name = info.speaker_name;
            response.speaker_confidence = info.speaker_confidence;
        }

        // === LOG TIMING TABLE ===
        info!("\n{}", metrics.format_table());

        Ok(response)
    }

    /// Helper do asynchronicznego zapisu do Memory po odpowiedzi modelu
    pub(crate) fn process_memory_store_async(
        &self,
        request: &ChatCompletionRequest,
        response: &ChatCompletionResponse,
        query_decision: Option<crate::memory_analyzer::QueryDecision>,
    ) {
        let response_text = crate::routing::extract_response_text(response);

        if response_text.is_empty() {
            return;
        }

        self.memory_integration.process_response_async(request, &response_text, query_decision);
    }

    /// Przetwarza audio_input: STT + speaker identification z confidence levels.
    pub(crate) async fn process_audio_input(
        &self,
        mut request: ChatCompletionRequest,
    ) -> Result<(ChatCompletionRequest, Option<VoiceInfo>)> {
        let audio_data = match request.audio_input.take() {
            Some(data) if !data.is_empty() => data,
            _ => return Ok((request, None)),
        };

        debug!("Processing audio_input: {} bytes", audio_data.len());

        // === KROK 1: STT - transkrypcja audio z diarization ===
        let stt_result = self.process_stt_for_voice(&audio_data).await?;
        let transcribed_text = stt_result.text;
        let diarized_speakers = stt_result.speakers;

        if transcribed_text.is_empty() {
            debug!("STT zwrocilo pusty tekst - pomijam audio input");
            return Ok((request, None));
        }

        debug!("STT transkrypcja: {}", transcribed_text);
        if diarized_speakers.len() > 1 {
            info!("Wykryto {} mowcow w audio (process_audio_input)", diarized_speakers.len());
        }

        // === KROK 2: Speaker Identification z confidence levels ===
        let speaker_result = self.process_speaker_identify(&audio_data).await;

        debug!(
            "Speaker identify: id={:?}, name={:?}, confidence={:?}, level={}",
            speaker_result.speaker_id, speaker_result.speaker_name,
            speaker_result.similarity, speaker_result.confidence_level
        );

        // === KROK 2.5: Zbieraj dodatkowe probki glosu dla nowo-zarejestrowanych mowcow ===
        if speaker_result.is_high_confidence() {
            if let Some(ref speaker_id) = speaker_result.speaker_id {
                let should_collect = {
                    let pending = self.pending_voice_samples.read().await;
                    pending.get(speaker_id).copied()
                };

                if let Some(remaining) = should_collect {
                    info!(
                        "Collecting additional voice sample for {}: {} remaining",
                        speaker_id, remaining
                    );

                    let speaker_id_for_sample = speaker_id.clone();
                    let audio_for_sample = audio_data.clone();
                    let pending_samples = self.pending_voice_samples.clone();
                    let service_manager = self.service_manager.clone();

                    tokio::spawn(async move {
                        let stt_client = match service_manager.get_first_quic_stt_client().await {
                            Some(client) => client,
                            None => {
                                warn!("No STT service for voice sample collection");
                                return;
                            }
                        };

                        use tentaflow_protocol::*;
                        let add_samples_payload = AudioPayload {
                            operation: AudioOperation::SpeakerAddSamples {
                                speaker_id: speaker_id_for_sample.clone(),
                                audio_samples: vec![audio_for_sample],
                            },
                        };

                        let request_id = uuid::Uuid::new_v4().to_string();
                        let add_request = ModelRequest {
                            request_id: request_id.clone(),
                            payload: ModelPayload::Audio(add_samples_payload),
                            metadata: None,
                            session_id: None,
                            stream: false,
                        };

                        match stt_client.send_request(add_request).await {
                            Ok(_) => {
                                info!(
                                    "Voice sample added for {}, decrementing counter",
                                    speaker_id_for_sample
                                );

                                let mut pending = pending_samples.write().await;
                                if let Some(count) = pending.get_mut(&speaker_id_for_sample) {
                                    if *count <= 1 {
                                        pending.remove(&speaker_id_for_sample);
                                        info!(
                                            "Voice sample collection complete for {} (3 samples collected)",
                                            speaker_id_for_sample
                                        );
                                    } else {
                                        *count -= 1;
                                        info!(
                                            "Voice samples remaining for {}: {}",
                                            speaker_id_for_sample, *count
                                        );
                                    }
                                }
                            }
                            Err(e) => {
                                warn!(
                                    "Failed to add voice sample for {}: {}",
                                    speaker_id_for_sample, e
                                );
                            }
                        }
                    });
                }
            }
        }

        // === KROK 3: Dodaj transkrypcje jako user message ===
        request.messages.push(Message {
            role: "user".to_string(),
            content: Some(MessageContent::Text(transcribed_text.clone())),
            reasoning_content: None,
            name: if speaker_result.is_high_confidence() {
                speaker_result.speaker_name.clone()
            } else {
                None
            },
            tool_calls: None,
            tool_call_id: None,
        });

        // === KROK 4: Ustaw person_id w memory_options wg confidence ===
        {
            let memory_opts = request.memory_options.get_or_insert_with(Default::default);

            if speaker_result.is_high_confidence() {
                if let Some(ref id) = speaker_result.speaker_id {
                    memory_opts.person_id = Some(id.clone());
                    memory_opts.speaker_confidence = speaker_result.similarity;
                    debug!("HIGH confidence - setting person_id={}", id);
                }
            } else if speaker_result.is_medium_confidence() {
                memory_opts.speaker_confidence = speaker_result.similarity;
                if let Some(ref name) = speaker_result.speaker_name {
                    let confirmation_hint = speaker_result.confirmation_message.clone()
                        .unwrap_or_else(|| format!("Czy to ty, {}?", name));
                    memory_opts.session_context = Some(format!(
                        "SPEAKER_CANDIDATE: id={}, name={}, confidence={:.2}, ask: {}",
                        speaker_result.speaker_id.as_deref().unwrap_or("?"),
                        name,
                        speaker_result.similarity.unwrap_or(0.0),
                        confirmation_hint
                    ));
                    debug!("MEDIUM confidence - candidate={}, needs confirmation", name);
                }
            } else {
                debug!("LOW confidence - treating as new speaker");
            }
        }

        let voice_info = VoiceInfo {
            transcribed_text,
            speaker_id: if speaker_result.is_high_confidence() {
                speaker_result.speaker_id.clone()
            } else {
                None
            },
            speaker_name: if speaker_result.is_high_confidence() {
                speaker_result.speaker_name.clone()
            } else {
                None
            },
            speaker_confidence: speaker_result.similarity,
            confidence_level: speaker_result.confidence_level,
            needs_confirmation: speaker_result.needs_confirmation,
            confirmation_message: speaker_result.confirmation_message,
            diarized_speakers,
        };

        Ok((request, Some(voice_info)))
    }

    /// Helper: STT dla voice conversation z diarization.
    pub(crate) async fn process_stt_for_voice(&self, audio_data: &[u8]) -> Result<SttWithDiarization> {
        let stt_client = self.service_manager.get_first_quic_stt_client().await
            .ok_or_else(|| CoreError::ModelNotFound {
                model_name: "stt-service".to_string(),
            })?;

        let stt_payload = AudioPayload {
            operation: AudioOperation::STT {
                model: "whisper".to_string(),
                audio_data: audio_data.to_vec(),
                language: None,
                prompt: None,
                temperature: None,
                response_format: Some("verbose_json".to_string()),
                timestamp_granularities: None,
                no_speech_threshold: None,
                avg_logprob_threshold: None,
                compression_ratio_threshold: None,
            },
        };

        let request_id = uuid::Uuid::new_v4().to_string();
        let stt_request = ModelRequest {
            request_id: request_id.clone(),
            payload: ModelPayload::Audio(stt_payload),
            metadata: None,
            session_id: None,
            stream: false,
        };

        let response = stt_client.send_request(stt_request).await
            .map_err(|e| CoreError::NetworkError {
                message: format!("STT request failed: {}", e),
                source: anyhow::anyhow!("{}", e),
            })?;

        match response.result {
            ModelResult::Audio(result) => {
                match result.data {
                    AudioResultData::Text(text) => Ok(SttWithDiarization {
                        text,
                        speakers: vec![],
                    }),
                    AudioResultData::Detailed { text, segments, .. } => {
                        let mut speaker_texts: std::collections::HashMap<String, (bool, Option<f32>, Vec<String>)> = std::collections::HashMap::new();

                        for seg in segments {
                            let label = seg.speaker_label.clone().unwrap_or_else(|| "SPEAKER_00".to_string());
                            let is_known = seg.is_known_speaker.unwrap_or(false);
                            let similarity = seg.speaker_similarity;

                            let entry = speaker_texts.entry(label.clone()).or_insert((is_known, similarity, vec![]));
                            entry.2.push(seg.text.clone());
                        }

                        let speakers: Vec<DiarizedSpeaker> = speaker_texts
                            .into_iter()
                            .map(|(label, (is_known, similarity, texts))| DiarizedSpeaker {
                                label,
                                is_known,
                                similarity,
                                text: texts.join(" "),
                            })
                            .collect();

                        if speakers.len() > 1 {
                            debug!("Diarization wykryla {} mowcow", speakers.len());
                            for s in &speakers {
                                debug!("  - {}: {} (known={})", s.label, s.text, s.is_known);
                            }
                        }

                        Ok(SttWithDiarization { text, speakers })
                    }
                    _ => Err(CoreError::InternalError {
                        message: "Unexpected audio result type (expected Text)".to_string(),
                        source: None,
                    }.into()),
                }
            }
            ModelResult::Error(e) => {
                Err(CoreError::InternalError {
                    message: format!("STT error: {}", e.message),
                    source: None,
                }.into())
            }
            _ => Err(CoreError::InternalError {
                message: "Unexpected STT response type".to_string(),
                source: None,
            }.into()),
        }
    }

    /// Helper: Speaker identification.
    /// Zwraca informacje o rozpoznaniu mowcy z poziomem pewnosci.
    pub(crate) async fn process_speaker_identify(&self, audio_data: &[u8]) -> SpeakerIdentifyResult {
        let stt_client = match self.service_manager.get_first_quic_stt_client().await {
            Some(client) => client,
            None => {
                warn!("No STT service available for speaker identification");
                return SpeakerIdentifyResult::unknown();
            }
        };

        // TODO: pobierac progi z ustawien DB (speaker_confidence_high, speaker_confidence_medium)
        let speaker_operation = AudioOperation::SpeakerIdentifyWithConfidence {
            audio_data: audio_data.to_vec(),
            high_threshold: Some(0.78),
            medium_threshold: Some(0.55),
            audio_metadata: None,
        };

        let request_id = uuid::Uuid::new_v4().to_string();
        let speaker_request = ModelRequest {
            request_id: request_id.clone(),
            payload: ModelPayload::Audio(AudioPayload {
                operation: speaker_operation,
            }),
            metadata: None,
            session_id: None,
            stream: false,
        };

        let response = match stt_client.send_request(speaker_request).await {
            Ok(resp) => resp,
            Err(e) => {
                warn!("Speaker identify request failed: {}", e);
                return SpeakerIdentifyResult::unknown();
            }
        };

        match response.result {
            ModelResult::Audio(audio_result) => {
                match audio_result.data {
                    AudioResultData::SpeakerIdentifyWithConfidenceResult {
                        is_match,
                        speaker_id,
                        speaker_name,
                        similarity,
                        confidence_level,
                        needs_confirmation,
                        confirmation_message,
                        ..
                    } => {
                        if is_match {
                            let valid_id = speaker_id.filter(|s| !s.is_empty());
                            let valid_name = speaker_name.filter(|s| !s.is_empty());

                            debug!("Speaker identified: id={:?}, name={:?}, confidence={}, level={}",
                                  valid_id, valid_name, similarity, confidence_level);

                            SpeakerIdentifyResult {
                                speaker_id: valid_id,
                                speaker_name: valid_name,
                                similarity: Some(similarity),
                                confidence_level,
                                needs_confirmation,
                                confirmation_message,
                            }
                        } else {
                            debug!("Speaker not recognized (similarity={}, level={})", similarity, confidence_level);
                            SpeakerIdentifyResult {
                                speaker_id: None,
                                speaker_name: None,
                                similarity: Some(similarity),
                                confidence_level: "LOW".to_string(),
                                needs_confirmation: false,
                                confirmation_message: None,
                            }
                        }
                    }
                    AudioResultData::SpeakerIdentifyResult { is_match, speaker_id, speaker_name, similarity, .. } => {
                        if is_match {
                            let valid_id = speaker_id.filter(|s| !s.is_empty());
                            let valid_name = speaker_name.filter(|s| !s.is_empty());
                            SpeakerIdentifyResult {
                                speaker_id: valid_id,
                                speaker_name: valid_name,
                                similarity: Some(similarity),
                                confidence_level: if similarity >= 0.78 { "HIGH" } else { "MEDIUM" }.to_string(),
                                needs_confirmation: similarity < 0.78,
                                confirmation_message: None,
                            }
                        } else {
                            SpeakerIdentifyResult::unknown()
                        }
                    }
                    _ => {
                        warn!("Unexpected audio result type for speaker identify");
                        SpeakerIdentifyResult::unknown()
                    }
                }
            }
            ModelResult::Error(e) => {
                warn!("Speaker identify error: {}", e.message);
                SpeakerIdentifyResult::unknown()
            }
            _ => {
                warn!("Unexpected speaker identify response type");
                SpeakerIdentifyResult::unknown()
            }
        }
    }

    /// Routuje request do RAG engine przez QUIC (non-streaming).
    pub(crate) async fn route_to_rag(
        &self,
        rag_engine_name: String,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse> {
        debug!("Routing to RAG engine: {}", rag_engine_name);

        let rag_handle = { self.service_manager.rag_services.read().get(&rag_engine_name).cloned() }
            .ok_or_else(|| CoreError::ModelNotFound {
                model_name: rag_engine_name.clone(),
            })?;

        let rag_client = rag_handle.get_client().await
            .ok_or_else(|| CoreError::AllBackendsUnavailable {
                model_name: rag_engine_name.clone(),
            })?;

        let query = request
            .messages
            .last()
            .and_then(|m| match &m.content {
                Some(MessageContent::Text(text)) => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();

        let context = if request.messages.len() > 1 {
            Some(RAGContext {
                messages: request
                    .messages
                    .iter()
                    .map(|m| {
                        let content = match &m.content {
                            Some(MessageContent::Text(text)) => text.clone(),
                            _ => String::new(),
                        };
                        tentaflow_protocol::Message {
                            role: m.role.clone(),
                            content,
                        }
                    })
                    .collect(),
                metadata: vec![],
            })
        } else {
            None
        };

        let (rag_payload, requires_llm, requires_audio) = crate::routing::build_rag_payload(&request, query, context);

        debug!(
            "Sending RAGPayload (llm: {}, audio: {}, modes: {:?})",
            requires_llm, requires_audio, rag_payload.search_modes
        );

        let rag_result = rag_client.send_request(rag_payload).await?;

        debug!(
            "Received RAGResult: {} chunks, llm: {}, audio: {}",
            rag_result.metadata.len(),
            rag_result.requires_llm_processing,
            rag_result.requires_audio_output
        );

        let content = if rag_result.requires_llm_processing {
            debug!("RAG wymaga przetworzenia przez LLM - routing do LLM backend");

            let llm_model_name = rag_result
                .llm_model
                .clone()
                .ok_or_else(|| anyhow::anyhow!("RAG result requires_llm_processing=true ale llm_model=None"))?;

            let llm_backends = self.get_service_backends(&llm_model_name)
                .ok_or_else(|| CoreError::ModelNotFound {
                    model_name: llm_model_name.clone(),
                })?;

            if llm_backends.is_empty() {
                return Err(CoreError::AllBackendsUnavailable {
                    model_name: llm_model_name.clone(),
                }.into());
            }

            let strategy = self.get_strategy(&llm_model_name)
                .ok_or_else(|| CoreError::ModelNotFound {
                    model_name: llm_model_name.clone(),
                })?;

            let backend_idx = strategy.select_backend(llm_backends)?;
            let llm_backend = &llm_backends[backend_idx];

            debug!(
                "Wybrany LLM backend ({}): {} [{}]",
                strategy.name(), llm_backend.url(), backend_idx
            );

            let llm_request = ChatCompletionRequest {
                model: llm_model_name.clone(),
                messages: vec![Message {
                    role: "user".to_string(),
                    content: Some(MessageContent::Text(rag_result.context_text.clone())),
                    ..Default::default()
                }],
                max_tokens: request.max_tokens,
                temperature: request.temperature,
                top_p: request.top_p,
                n: Some(1),
                stream: false,
                stop: request.stop.clone(),
                presence_penalty: request.presence_penalty,
                frequency_penalty: request.frequency_penalty,
                user: request.user.clone(),
                response_format: None,
                tools: None,
                tool_choice: None,
                rag_options: None,
                memory_options: None,
                audio_input: None,
            };

            let llm_response = llm_backend.chat_completion(llm_request).await?;

            let llm_text = llm_response
                .choices
                .first()
                .and_then(|choice| match &choice.message.content {
                    Some(MessageContent::Text(text)) => Some(text.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| {
                    warn!("LLM nie zwrocil tekstu w pierwszym choice - uzywam pustego");
                    String::new()
                });

            debug!("Otrzymano odpowiedz z LLM: {} znakow", llm_text.len());

            if rag_result.requires_audio_output {
                debug!("requires_audio_output=true ale uzywamy non-streaming mode - pomijam TTS");
            }

            llm_text
        } else {
            debug!("RAG zwrocil gotowa odpowiedz - zwracam bezposrednio");
            rag_result.context_text
        };

        let cleaned_content = self.response_middleware.clean_text(&content)?;

        let chat_response = ChatCompletionResponse {
            id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
            object: "chat.completion".to_string(),
            created: chrono::Utc::now().timestamp() as u64,
            model: rag_engine_name,
            choices: vec![Choice {
                index: 0,
                message: Message {
                    role: "assistant".to_string(),
                    content: Some(MessageContent::Text(cleaned_content)),
                    ..Default::default()
                },
                finish_reason: Some("stop".to_string()),
                logprobs: None,
            }],
            usage: Some(Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
            }),
            system_fingerprint: None,
            transcribed_text: None,
            speaker_id: None,
            speaker_name: None,
            speaker_confidence: None,
            detected_intent: None,
            detected_tools: None,
        };

        Ok(chat_response)
    }

    /// Routuje request do QUIC LLM engine (non-streaming).
    pub(crate) async fn route_to_quic_llm(
        &self,
        llm_name: String,
        request: ChatCompletionRequest,
        prompt_override: Option<String>,
        stop_override: Option<Vec<String>>,
    ) -> Result<ChatCompletionResponse> {
        use tentaflow_protocol::*;

        debug!("Routing to QUIC LLM: {}, prompt_override={:?}", llm_name, prompt_override.as_ref().map(|p| p.len()));

        let quic_client = self.service_manager.get_quic_llm_client(&llm_name).await
            .ok_or_else(|| CoreError::AllBackendsUnavailable {
                model_name: llm_name.clone(),
            })?;

        let protocol_messages = crate::routing::openai_messages_to_protocol(&request.messages);

        let stop_tokens = stop_override.or(request.stop.clone());

        let request_id = uuid::Uuid::new_v4().to_string();
        let model_request = ModelRequest {
            request_id: request_id.clone(),
            payload: ModelPayload::Completion(CompletionPayload {
                model: request.model.clone(),
                prompt: prompt_override,
                messages: protocol_messages,
                temperature: request.temperature,
                max_tokens: request.max_tokens,
                top_p: request.top_p,
                stop: stop_tokens,
                presence_penalty: request.presence_penalty,
                frequency_penalty: request.frequency_penalty,
                tts_options: None,
                memory_options: None,
                audio_input: None,
                prefix_cache_id: None,
                prefix_text: None,
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        debug!("Wysylam request do QUIC LLM: {}", llm_name);

        let model_response = quic_client.send_request(model_request).await?;

        match model_response.result {
            ModelResult::Completion(completion_result) => {
                let cleaned_text = self.response_middleware.clean_text(&completion_result.text)?;
                let cleaned_reasoning = if let Some(ref rc) = completion_result.reasoning_content {
                    Some(self.response_middleware.clean_text(rc)?)
                } else {
                    None
                };

                let chat_response = ChatCompletionResponse {
                    id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
                    object: "chat.completion".to_string(),
                    created: chrono::Utc::now().timestamp() as u64,
                    model: llm_name,
                    choices: vec![Choice {
                        index: 0,
                        message: crate::api::openai::types::Message {
                            role: "assistant".to_string(),
                            content: Some(crate::api::openai::types::MessageContent::Text(cleaned_text)),
                            reasoning_content: cleaned_reasoning,
                            ..Default::default()
                        },
                        finish_reason: completion_result.finish_reason,
                        logprobs: None,
                    }],
                    usage: model_response.metrics.map(|m| {
                        if let Some(DetailedMetrics::Completion { prompt_tokens, completion_tokens, total_tokens }) = m.detailed {
                            Usage { prompt_tokens, completion_tokens, total_tokens }
                        } else {
                            Usage { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 }
                        }
                    }),
                    system_fingerprint: None,
                    transcribed_text: None,
                    speaker_id: None,
                    speaker_name: None,
                    speaker_confidence: None,
                    detected_intent: None,
                    detected_tools: None,
                };

                debug!("QUIC LLM response received: {} chars", chat_response.choices.first().map(|c| {
                    match &c.message.content {
                        Some(crate::api::openai::types::MessageContent::Text(t)) => t.len(),
                        _ => 0,
                    }
                }).unwrap_or(0));

                Ok(chat_response)
            }
            ModelResult::Error(error_info) => {
                Err(CoreError::InternalError {
                    message: format!("QUIC LLM error: {}", error_info.message),
                    source: None,
                }.into())
            }
            _ => Err(CoreError::InternalError {
                message: "Unexpected response type from QUIC LLM".to_string(),
                source: None,
            }.into()),
        }
    }

    /// Startuje task obslugujacy callback requests od RAG engines.
    pub(crate) fn spawn_callback_handler(&self) {
        let callback_rx = self.get_callback_rx();
        let service_manager = self.service_manager.clone();
        let config = self.config.clone();

        tokio::spawn(async move {
            debug!("Callback handler started");

            loop {
                let (callback_req, resp_tx) = {
                    let mut rx = callback_rx.lock().await;
                    match rx.recv().await {
                        Some(req) => req,
                        None => {
                            warn!("Callback channel closed");
                            break;
                        }
                    }
                };

                debug!("Processing callback: {}", callback_req.request_id);

                let sm = service_manager.clone();
                let cfg = config.clone();
                tokio::spawn(async move {
                    let response = Self::handle_callback(callback_req, sm, cfg).await;

                    if resp_tx.send(response).await.is_err() {
                        warn!("Failed to send callback response");
                    }
                });
            }
        });
    }

    /// Obsluguje pojedynczy callback request od RAG.
    pub(crate) async fn handle_callback(
        request: ModelRequest,
        service_manager: Arc<ServiceManager>,
        _config: Arc<RouterConfig>,
    ) -> ModelResponse {
        let request_id = request.request_id.clone();

        match request.payload {
            ModelPayload::Embeddings(embeddings_payload) => {
                let model = embeddings_payload.model.clone();
                let input = embeddings_payload.input.clone();
                debug!("Embedding callback: model={}, {} tekstow", model, input.len());

                let emb_handle = { service_manager.quic_embedding_services.read().get(&model).cloned() };
                if let Some(quic_handle) = emb_handle {
                    if let Some(quic_client) = quic_handle.get_client().await {
                        debug!("Uzywam QUIC client dla embeddingow: {}", model);

                        let quic_request = ModelRequest {
                            request_id: request_id.clone(),
                            payload: ModelPayload::Embeddings(embeddings_payload),
                            stream: false,
                            metadata: None,
                            session_id: None,
                        };

                        match quic_client.send_request(quic_request).await {
                            Ok(response) => {
                                debug!("QUIC embedding callback sukces");
                                return response;
                            }
                            Err(e) => {
                                error!("QUIC embedding callback error: {}", e);
                                return ModelResponse {
                                    request_id,
                                    result: ModelResult::Error(ErrorInfo {
                                        error_type: ErrorType::InternalError,
                                        message: format!("QUIC embedding error: {}", e),
                                        details: Some(e.to_string()),
                                    }),
                                    metrics: None,
                                };
                            }
                        }
                    } else {
                        warn!("QUIC embedding client '{}' nie jest polaczony, fallback do HTTP", model);
                    }
                }

                let backends = match service_manager.service_backends.get(&model) {
                    Some(b) => b,
                    None => {
                        return ModelResponse {
                            request_id,
                            result: ModelResult::Error(ErrorInfo {
                                error_type: ErrorType::ModelNotFound,
                                message: format!("Model embedding '{}' nie znaleziony w konfiguracji (ani QUIC ani HTTP)", model),
                                details: None,
                            }),
                            metrics: None,
                        }
                    }
                };

                if backends.is_empty() {
                    return ModelResponse {
                        request_id,
                        result: ModelResult::Error(ErrorInfo {
                            error_type: ErrorType::ModelNotFound,
                            message: format!("Brak dostepnych backendow dla modelu '{}'", model),
                            details: None,
                        }),
                        metrics: None,
                    };
                }

                let strategy = match service_manager.load_balancing_strategies.get(&model) {
                    Some(s) => s,
                    None => {
                        return ModelResponse {
                            request_id,
                            result: ModelResult::Error(ErrorInfo {
                                error_type: ErrorType::InternalError,
                                message: format!("Brak strategii load balancing dla modelu '{}'", model),
                                details: None,
                            }),
                            metrics: None,
                        }
                    }
                };

                let backend_idx = match strategy.select_backend(backends) {
                    Ok(idx) => idx,
                    Err(e) => {
                        return ModelResponse {
                            request_id,
                            result: ModelResult::Error(ErrorInfo {
                                error_type: ErrorType::InternalError,
                                message: format!("Blad wyboru backendu: {}", e),
                                details: Some(e.to_string()),
                            }),
                            metrics: None,
                        }
                    }
                };

                let backend = &backends[backend_idx];

                debug!(
                    "Wybrany backend ({}): {} [{}]",
                    strategy.name(), backend.url(), backend_idx
                );

                match backend.embedding(input).await {
                    Ok(embeddings) => {
                        debug!("Embedding callback sukces: {} wektorow", embeddings.len());
                        ModelResponse {
                            request_id,
                            result: ModelResult::Embeddings(EmbeddingsResult {
                                embeddings,
                                dimensions: 0,
                                model,
                            }),
                            metrics: None,
                        }
                    }
                    Err(e) => {
                        error!("Embedding callback error: {}", e);
                        ModelResponse {
                            request_id,
                            result: ModelResult::Error(ErrorInfo {
                                error_type: ErrorType::InternalError,
                                message: format!("Backend embedding error: {}", e),
                                details: Some(e.to_string()),
                            }),
                            metrics: None,
                        }
                    }
                }
            }

            ModelPayload::Completion(completion_payload) => {
                let model = completion_payload.model.clone();
                let messages = completion_payload.messages;
                let temperature = completion_payload.temperature;
                let max_tokens = completion_payload.max_tokens;
                let top_p = completion_payload.top_p;
                let prompt = completion_payload.prompt;
                let stop = completion_payload.stop;

                debug!(
                    "Completion callback: model={}, {} wiadomosci, prompt_len={:?}",
                    model, messages.len(), prompt.as_ref().map(|p| p.len())
                );

                let llm_handle = { service_manager.quic_llm_services.read().get(&model).cloned() };
                if let Some(quic_handle) = llm_handle {
                    if let Some(quic_client) = quic_handle.get_client().await {
                        debug!("Uzywam QUIC client dla LLM: {}", model);

                        let quic_request = ModelRequest {
                            request_id: request_id.clone(),
                            payload: ModelPayload::Completion(CompletionPayload {
                                model: model.clone(),
                                prompt,
                                messages,
                                temperature,
                                max_tokens,
                                top_p,
                                stop,
                                presence_penalty: None,
                                frequency_penalty: None,
                                tts_options: None,
                                memory_options: None,
                                audio_input: None,
                                prefix_cache_id: None,
                                prefix_text: None,
                            }),
                            stream: false,
                            metadata: None,
                            session_id: None,
                        };

                        match quic_client.send_request(quic_request).await {
                            Ok(response) => {
                                debug!("QUIC LLM callback sukces");
                                return response;
                            }
                            Err(e) => {
                                error!("QUIC LLM callback error: {}", e);
                                return ModelResponse {
                                    request_id,
                                    result: ModelResult::Error(ErrorInfo {
                                        error_type: ErrorType::InternalError,
                                        message: format!("QUIC LLM error: {}", e),
                                        details: Some(e.to_string()),
                                    }),
                                    metrics: None,
                                };
                            }
                        }
                    } else {
                        warn!("QUIC LLM client '{}' nie jest polaczony, fallback do HTTP", model);
                    }
                }

                let backends = match service_manager.service_backends.get(&model) {
                    Some(b) => b,
                    None => {
                        return ModelResponse {
                            request_id,
                            result: ModelResult::Error(ErrorInfo {
                                error_type: ErrorType::ModelNotFound,
                                message: format!("Model LLM '{}' nie znaleziony w konfiguracji (ani QUIC ani HTTP)", model),
                                details: None,
                            }),
                            metrics: None,
                        }
                    }
                };

                if backends.is_empty() {
                    return ModelResponse {
                        request_id,
                        result: ModelResult::Error(ErrorInfo {
                            error_type: ErrorType::ModelNotFound,
                            message: format!("No backends available for model: {}", model),
                            details: None,
                        }),
                        metrics: None,
                    };
                }

                let strategy = match service_manager.load_balancing_strategies.get(&model) {
                    Some(s) => s,
                    None => {
                        return ModelResponse {
                            request_id,
                            result: ModelResult::Error(ErrorInfo {
                                error_type: ErrorType::InternalError,
                                message: format!("Brak strategii load balancing dla modelu '{}'", model),
                                details: None,
                            }),
                            metrics: None,
                        }
                    }
                };

                let backend_idx = match strategy.select_backend(backends) {
                    Ok(idx) => idx,
                    Err(e) => {
                        return ModelResponse {
                            request_id,
                            result: ModelResult::Error(ErrorInfo {
                                error_type: ErrorType::InternalError,
                                message: format!("Blad wyboru backendu: {}", e),
                                details: Some(e.to_string()),
                            }),
                            metrics: None,
                        }
                    }
                };

                let backend = &backends[backend_idx];

                debug!(
                    "Callback ChatCompletion - wybrany backend ({}): {} [{}]",
                    strategy.name(), backend.url(), backend_idx
                );

                let openai_messages: Vec<Message> = messages
                    .iter()
                    .map(|m| Message {
                        role: m.role.clone(),
                        content: Some(MessageContent::Text(m.content.clone())),
                        ..Default::default()
                    })
                    .collect();

                let chat_request = ChatCompletionRequest {
                    model: model.clone(),
                    messages: openai_messages,
                    max_tokens,
                    temperature,
                    top_p,
                    n: Some(1),
                    stream: false,
                    stop: None,
                    presence_penalty: None,
                    frequency_penalty: None,
                    user: None,
                    response_format: None,
                    tools: None,
                    tool_choice: None,
                    rag_options: None,
                    memory_options: None,
                    audio_input: None,
                };

                match backend.chat_completion(chat_request).await {
                    Ok(response) => {
                        let text = response
                            .choices
                            .first()
                            .and_then(|c| match &c.message.content {
                                Some(MessageContent::Text(text)) => Some(text.clone()),
                                _ => None,
                            })
                            .unwrap_or_default();

                        ModelResponse {
                            request_id,
                            result: ModelResult::Completion(CompletionResult {
                                text,
                                reasoning_content: None,
                                model,
                                finish_reason: Some("stop".to_string()),
                                tool_calls: None,
                                detected_intent: None,
                                detected_tools: None,
                                transcribed_text: None,
                                speaker_id: None,
                                speaker_name: None,
                            }),
                            metrics: None,
                        }
                    }
                    Err(e) => {
                        ModelResponse {
                            request_id,
                            result: ModelResult::Error(ErrorInfo {
                                error_type: ErrorType::InternalError,
                                message: format!("Backend error: {}", e),
                                details: Some(e.to_string()),
                            }),
                            metrics: None,
                        }
                    }
                }
            }

            ModelPayload::Image(_image_payload) => {
                warn!("ImageGeneration callback not yet implemented");
                ModelResponse {
                    request_id,
                    result: ModelResult::Error(ErrorInfo {
                        error_type: ErrorType::InternalError,
                        message: "ImageGeneration callbacks not yet implemented".to_string(),
                        details: None,
                    }),
                    metrics: None,
                }
            }

            ModelPayload::Audio(audio_payload) => {
                // TODO: Przenies obsluge audio callbacks (STT, Speaker operations) z oryginalnego kodu
                // Pelna implementacja w stt.rs
                match audio_payload.operation {
                    AudioOperation::TTS { .. } => {
                        warn!("AudioTTS callback not yet implemented");
                        ModelResponse {
                            request_id,
                            result: ModelResult::Error(ErrorInfo {
                                error_type: ErrorType::InternalError,
                                message: "AudioTTS callbacks not yet implemented".to_string(),
                                details: None,
                            }),
                            metrics: None,
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
                    } => {
                        debug!("Processing AudioSTT callback: model={}, audio_size={} bytes", model, audio_data.len());

                        let backends = match service_manager.service_backends.get(&model) {
                            Some(b) => b,
                            None => {
                                return ModelResponse {
                                    request_id,
                                    result: ModelResult::Error(ErrorInfo {
                                        error_type: ErrorType::ModelNotFound,
                                        message: format!("Model not found: {}", model),
                                        details: None,
                                    }),
                                    metrics: None,
                                }
                            }
                        };

                        if backends.is_empty() {
                            return ModelResponse {
                                request_id,
                                result: ModelResult::Error(ErrorInfo {
                                    error_type: ErrorType::ModelNotFound,
                                    message: format!("No backends available for model: {}", model),
                                    details: None,
                                }),
                                metrics: None,
                            };
                        }

                        let strategy = match service_manager.load_balancing_strategies.get(&model) {
                            Some(s) => s,
                            None => {
                                return ModelResponse {
                                    request_id,
                                    result: ModelResult::Error(ErrorInfo {
                                        error_type: ErrorType::InternalError,
                                        message: format!("Brak strategii load balancing dla modelu '{}'", model),
                                        details: None,
                                    }),
                                    metrics: None,
                                }
                            }
                        };

                        let backend_idx = match strategy.select_backend(backends) {
                            Ok(idx) => idx,
                            Err(e) => {
                                return ModelResponse {
                                    request_id,
                                    result: ModelResult::Error(ErrorInfo {
                                        error_type: ErrorType::InternalError,
                                        message: format!("Blad wyboru backendu: {}", e),
                                        details: Some(e.to_string()),
                                    }),
                                    metrics: None,
                                }
                            }
                        };

                        let backend = &backends[backend_idx];

                        let filename = format!("audio_{}.mp3", uuid::Uuid::new_v4());

                        let needs_segments = no_speech_threshold.is_some()
                            || avg_logprob_threshold.is_some()
                            || compression_ratio_threshold.is_some();

                        let effective_format = if needs_segments && response_format.as_deref() != Some("verbose_json") {
                            Some("verbose_json".to_string())
                        } else {
                            response_format.clone()
                        };

                        let transcription_request = TranscriptionRequest {
                            file: audio_data,
                            filename,
                            model: model.clone(),
                            language,
                            prompt,
                            response_format: effective_format.clone(),
                            temperature,
                            timestamp_granularities,
                            no_speech_threshold,
                            avg_logprob_threshold,
                            compression_ratio_threshold,
                        };

                        match backend.audio_transcription(transcription_request).await {
                            Ok(transcription) => {
                                let is_verbose = effective_format.as_deref() == Some("verbose_json");

                                if is_verbose {
                                    if let Some(segments) = transcription.segments {
                                        let filtered_segments: Vec<_> = segments.into_iter()
                                            .filter(|seg| {
                                                if let Some(threshold) = no_speech_threshold {
                                                    if seg.no_speech_prob >= threshold {
                                                        return false;
                                                    }
                                                }
                                                if let Some(threshold) = avg_logprob_threshold {
                                                    if seg.avg_logprob < threshold {
                                                        return false;
                                                    }
                                                }
                                                if let Some(threshold) = compression_ratio_threshold {
                                                    if seg.compression_ratio > threshold {
                                                        return false;
                                                    }
                                                }
                                                true
                                            })
                                            .collect();

                                        let filtered_text = filtered_segments.iter()
                                            .map(|seg| seg.text.as_str())
                                            .collect::<Vec<_>>()
                                            .join("");

                                        return ModelResponse {
                                            request_id,
                                            result: ModelResult::Audio(AudioResult {
                                                data: AudioResultData::Text(filtered_text),
                                                model,
                                            }),
                                            metrics: None,
                                        };
                                    }
                                }

                                ModelResponse {
                                    request_id,
                                    result: ModelResult::Audio(AudioResult {
                                        data: AudioResultData::Text(transcription.text),
                                        model,
                                    }),
                                    metrics: None,
                                }
                            }
                            Err(e) => {
                                warn!("AudioSTT callback error: {}", e);
                                ModelResponse {
                                    request_id,
                                    result: ModelResult::Error(ErrorInfo {
                                        error_type: ErrorType::InternalError,
                                        message: format!("Audio transcription failed: {}", e),
                                        details: Some(e.to_string()),
                                    }),
                                    metrics: None,
                                }
                            }
                        }
                    }

                    AudioOperation::SpeakerEnroll { .. }
                    | AudioOperation::SpeakerAddSamples { .. }
                    | AudioOperation::SpeakerRemove { .. }
                    | AudioOperation::SpeakerList
                    | AudioOperation::SpeakerInfo
                    | AudioOperation::SpeakerIdentify { .. }
                    | AudioOperation::SpeakerVerify { .. }
                    | AudioOperation::SpeakerIdentifyWithConfidence { .. }
                    | AudioOperation::SpeakerConfirmIdentity { .. }
                    | AudioOperation::SpeakerLinkToMemory { .. }
                    | AudioOperation::WakeWordDetect { .. }
                    | AudioOperation::WakeWordConfigure { .. }
                    | AudioOperation::WakeWordStreamStart { .. }
                    | AudioOperation::WakeWordStreamChunk { .. }
                    | AudioOperation::WakeWordStreamStop
                    | AudioOperation::ConversationStart { .. }
                    | AudioOperation::ConversationAudio { .. }
                    | AudioOperation::ConversationEnd { .. }
                    | AudioOperation::ConversationStatus { .. }
                    | AudioOperation::SpeakerUpdateName { .. } => {
                        warn!("Speaker/WakeWord/Conversation operations not supported in RAG callbacks");
                        ModelResponse {
                            request_id,
                            result: ModelResult::Error(ErrorInfo {
                                error_type: ErrorType::InvalidRequest,
                                message: "Speaker operations are not supported in RAG callbacks".to_string(),
                                details: None,
                            }),
                            metrics: None,
                        }
                    }
                }
            }

            ModelPayload::Vision(vision_payload) => {
                let model = vision_payload.model;
                let messages = vision_payload.messages;
                let max_tokens = vision_payload.max_tokens;
                debug!("Processing Vision callback: model={}, messages={}", model, messages.len());

                let backends = match service_manager.service_backends.get(&model) {
                    Some(b) => b,
                    None => {
                        return ModelResponse {
                            request_id,
                            result: ModelResult::Error(ErrorInfo {
                                error_type: ErrorType::ModelNotFound,
                                message: format!("Model not found: {}", model),
                                details: None,
                            }),
                            metrics: None,
                        }
                    }
                };

                if backends.is_empty() {
                    return ModelResponse {
                        request_id,
                        result: ModelResult::Error(ErrorInfo {
                            error_type: ErrorType::ModelNotFound,
                            message: format!("No backends available for model: {}", model),
                            details: None,
                        }),
                        metrics: None,
                    };
                }

                let strategy = match service_manager.load_balancing_strategies.get(&model) {
                    Some(s) => s,
                    None => {
                        return ModelResponse {
                            request_id,
                            result: ModelResult::Error(ErrorInfo {
                                error_type: ErrorType::InternalError,
                                message: format!("Brak strategii load balancing dla modelu '{}'", model),
                                details: None,
                            }),
                            metrics: None,
                        }
                    }
                };

                let backend_idx = match strategy.select_backend(backends) {
                    Ok(idx) => idx,
                    Err(e) => {
                        return ModelResponse {
                            request_id,
                            result: ModelResult::Error(ErrorInfo {
                                error_type: ErrorType::InternalError,
                                message: format!("Blad wyboru backendu: {}", e),
                                details: Some(e.to_string()),
                            }),
                            metrics: None,
                        }
                    }
                };

                let backend = &backends[backend_idx];

                match backend.vision(model.clone(), messages.clone(), max_tokens).await {
                    Ok(text) => {
                        debug!("Vision callback success: {} znaki tekstu", text.len());
                        ModelResponse {
                            request_id,
                            result: ModelResult::Vision(VisionResult {
                                text,
                                model,
                            }),
                            metrics: None,
                        }
                    }
                    Err(e) => {
                        warn!("Vision callback error: {}", e);
                        ModelResponse {
                            request_id,
                            result: ModelResult::Error(ErrorInfo {
                                error_type: ErrorType::InternalError,
                                message: format!("Vision request failed: {}", e),
                                details: Some(e.to_string()),
                            }),
                            metrics: None,
                        }
                    }
                }
            }

            ModelPayload::RAG(_) => {
                warn!("Unexpected RAG payload in callback");
                ModelResponse {
                    request_id,
                    result: ModelResult::Error(ErrorInfo {
                        error_type: ErrorType::InternalError,
                        message: "RAG callbacks are not supported".to_string(),
                        details: None,
                    }),
                    metrics: None,
                }
            }

            ModelPayload::Rerank(rerank_payload) => {
                let model = rerank_payload.model.clone();
                debug!("Rerank callback: model={}, {} dokumentow", model, rerank_payload.documents.len());

                let rerank_handle = { service_manager.quic_embedding_services.read().get(&model).cloned() };
                if let Some(quic_handle) = rerank_handle {
                    if let Some(quic_client) = quic_handle.get_client().await {
                        debug!("Uzywam QUIC client dla rerankingu: {}", model);

                        let quic_request = ModelRequest {
                            request_id: request_id.clone(),
                            payload: ModelPayload::Rerank(rerank_payload),
                            stream: false,
                            metadata: None,
                            session_id: None,
                        };

                        match quic_client.send_request(quic_request).await {
                            Ok(response) => {
                                debug!("QUIC rerank callback sukces");
                                return response;
                            }
                            Err(e) => {
                                error!("QUIC rerank callback error: {}", e);
                                return ModelResponse {
                                    request_id,
                                    result: ModelResult::Error(ErrorInfo {
                                        error_type: ErrorType::InternalError,
                                        message: format!("QUIC rerank error: {}", e),
                                        details: Some(e.to_string()),
                                    }),
                                    metrics: None,
                                };
                            }
                        }
                    }
                }

                warn!("Serwis rerankera '{}' nie znaleziony", model);
                ModelResponse {
                    request_id,
                    result: ModelResult::Error(ErrorInfo {
                        error_type: ErrorType::ModelNotFound,
                        message: format!("Serwis rerankera '{}' nie znaleziony", model),
                        details: None,
                    }),
                    metrics: None,
                }
            }

            ModelPayload::Memory(_) => {
                warn!("Unexpected Memory payload in callback - Memory should use Embeddings for callbacks");
                ModelResponse {
                    request_id,
                    result: ModelResult::Error(ErrorInfo {
                        error_type: ErrorType::InvalidRequest,
                        message: "Memory callbacks should use Embeddings payload, not Memory payload".to_string(),
                        details: Some("Memory Engine uses Router for embeddings callbacks only".to_string()),
                    }),
                    metrics: None,
                }
            }

            ModelPayload::PrefixCacheInit(_) => {
                warn!("Unexpected PrefixCacheInit payload in callback");
                ModelResponse {
                    request_id,
                    result: ModelResult::Error(ErrorInfo {
                        error_type: ErrorType::InvalidRequest,
                        message: "PrefixCacheInit is not valid in callbacks".to_string(),
                        details: Some("PrefixCacheInit is sent from Router to LLM, not in callbacks".to_string()),
                    }),
                    metrics: None,
                }
            }
        }
    }

    /// Routuje zadanie ingestion dokumentu do RAG engine.
    pub async fn route_document_ingestion(
        &self,
        request: tentaflow_protocol::IngestRequest,
    ) -> Result<tentaflow_protocol::IngestResponse> {
        let rag_handle = { self.service_manager.rag_services.read().values().next().cloned() }
            .ok_or_else(|| CoreError::ModelNotFound {
                model_name: "rag".to_string(),
            })?;

        let rag_client = rag_handle.get_client().await
            .ok_or_else(|| CoreError::AllBackendsUnavailable {
                model_name: "rag".to_string(),
            })?;

        debug!("Wysylanie IngestRequest do RAG: doc_id={}", request.document_id);

        let response = rag_client.send_ingest_request(request).await?;

        debug!(
            "Otrzymano IngestResponse: status={:?}, chunks={}",
            response.status, response.chunk_count
        );

        Ok(response)
    }

    // ========================================================================
    // PROTOCOL-NATIVE METHODS (dla QUIC Server)
    // ========================================================================

    /// Routuje RAG query - wersja dla protocol types (pelna kontrola parametrow).
    pub async fn route_rag_payload(
        &self,
        rag_payload: tentaflow_protocol::RAGPayload,
    ) -> Result<tentaflow_protocol::ModelResponse> {
        use tentaflow_protocol::*;

        debug!("route_rag_payload: query={}, search_modes={:?}",
            rag_payload.query.chars().take(50).collect::<String>(),
            rag_payload.search_modes);

        let (rag_name, rag_handle) = { self.service_manager.rag_services.read().iter().next().map(|(n, h)| (n.clone(), h.clone())) }
            .ok_or_else(|| CoreError::InternalError {
                message: "Brak skonfigurowanego RAG engine".to_string(),
                source: None,
            })?;

        let rag_client = rag_handle.get_client().await
            .ok_or_else(|| CoreError::AllBackendsUnavailable {
                model_name: rag_name.clone(),
            })?;

        debug!("route_rag_payload: uzywam RAG engine: {}", rag_name);

        let rag_result = rag_client.send_request(rag_payload).await?;

        let cleaned_context = self.response_middleware.clean_text(&rag_result.context_text)?;

        let request_id = uuid::Uuid::new_v4().to_string();

        let response = ModelResponse {
            request_id,
            result: ModelResult::RAG(RAGResult {
                context_text: cleaned_context,
                metadata: rag_result.metadata,
                requires_llm_processing: rag_result.requires_llm_processing,
                requires_audio_output: rag_result.requires_audio_output,
                llm_model: rag_result.llm_model,
            }),
            metrics: None,
        };

        Ok(response)
    }

    /// Routuje RAG query - uproszczona wersja (kompatybilnosc wsteczna).
    pub async fn route_rag_query(
        &self,
        query: &str,
        top_k: u32,
        min_similarity: f32,
    ) -> Result<tentaflow_protocol::ModelResponse> {
        use tentaflow_protocol::*;

        let rag_payload = RAGPayload {
            query: query.to_string(),
            context: None,
            params: RAGParams {
                top_k,
                min_similarity,
                use_reranking: None,
            },
            requires_llm_processing: false,
            requires_audio_output: false,
            search_modes: vec![SearchMode::VectorSearch],
        };

        self.route_rag_payload(rag_payload).await
    }

    /// Routuje chat completion - wersja dla protocol types.
    pub async fn route_completion_via_protocol(
        &self,
        model: &str,
        messages: Vec<tentaflow_protocol::Message>,
        temperature: Option<f32>,
        max_tokens: Option<u32>,
        prompt: Option<String>,
        stop: Option<Vec<String>>,
    ) -> Result<tentaflow_protocol::ModelResponse> {
        use tentaflow_protocol::*;

        let model_name = self.resolve_to_service_name(model);

        debug!("route_completion_via_protocol: model={}, messages={}, prompt_len={:?}",
               model_name, messages.len(), prompt.as_ref().map(|p| p.len()));

        let start_time = std::time::Instant::now();

        let openai_messages: Vec<crate::api::openai::types::Message> = messages
            .iter()
            .map(|m| crate::api::openai::types::Message {
                role: m.role.clone(),
                content: Some(MessageContent::Text(m.content.clone())),
                ..Default::default()
            })
            .collect();

        let request = ChatCompletionRequest {
            model: model_name.clone(),
            messages: openai_messages,
            temperature,
            max_tokens,
            top_p: None,
            n: None,
            stream: false,
            stop: stop.clone(),
            presence_penalty: None,
            frequency_penalty: None,
            user: None,
            tools: None,
            tool_choice: None,
            response_format: None,
            rag_options: None,
            memory_options: None,
            audio_input: None,
        };

        let response = if self.is_quic_llm_model(&model_name) {
            self.route_to_quic_llm(model_name.clone(), request, prompt, stop).await?
        } else {
            self.route_chat_completion(request).await?
        };

        let content = crate::routing::extract_response_text(&response);

        let reasoning_content = response.choices.first()
            .and_then(|c| c.message.reasoning_content.clone());

        let tool_calls = response.choices.first()
            .and_then(|c| c.message.tool_calls.as_ref())
            .map(|tcs| {
                tcs.iter().map(|tc| {
                    ToolCallResult {
                        id: tc.id.clone(),
                        tool_type: tc.tool_type.clone(),
                        function_name: tc.function.name.clone(),
                        arguments: tc.function.arguments.clone(),
                    }
                }).collect::<Vec<_>>()
            });

        let cleaned_content = self.response_middleware.clean_text(&content)?;

        let cleaned_reasoning = if let Some(ref rc) = reasoning_content {
            Some(self.response_middleware.clean_text(rc)?)
        } else {
            None
        };

        let finish_reason = response.choices.first()
            .and_then(|c| c.finish_reason.clone());

        let request_id = uuid::Uuid::new_v4().to_string();

        let latency_ms = start_time.elapsed().as_millis() as u64;
        let metrics = response.usage.map(|usage| {
            let tokens_per_sec = if latency_ms > 0 && usage.completion_tokens > 0 {
                Some((usage.completion_tokens as f32 / latency_ms as f32) * 1000.0)
            } else {
                None
            };
            ModelMetrics {
                model_name: response.model.clone(),
                latency_ms,
                time_to_first_token_ms: None,
                tokens_processed: Some(usage.total_tokens as usize),
                throughput_tokens_per_sec: tokens_per_sec,
                detailed: Some(DetailedMetrics::Completion {
                    prompt_tokens: usage.prompt_tokens,
                    completion_tokens: usage.completion_tokens,
                    total_tokens: usage.total_tokens,
                }),
            }
        });

        let model_response = ModelResponse {
            request_id,
            result: ModelResult::Completion(CompletionResult {
                text: cleaned_content,
                reasoning_content: cleaned_reasoning,
                model: model_name,
                finish_reason,
                tool_calls,
                detected_intent: None,
                detected_tools: None,
                transcribed_text: None,
                speaker_id: None,
                speaker_name: None,
            }),
            metrics,
        };

        Ok(model_response)
    }

    /// Routuje request Memory przez QUIC do Memory Engine.
    pub async fn route_memory_via_quic(
        &self,
        payload: &tentaflow_protocol::MemoryPayload,
    ) -> Result<tentaflow_protocol::ModelResponse> {
        use tentaflow_protocol::*;

        debug!("route_memory_via_quic: START operation={:?}", std::mem::discriminant(&payload.operation));

        let quic_client = {
            let mut client = None;
            let memory_handles: Vec<_> = self.service_manager.quic_memory_services.read().values().cloned().collect();
            for handle in memory_handles {
                if let Some(c) = handle.get_client().await {
                    client = Some(c);
                    break;
                }
            }
            client.ok_or_else(|| CoreError::AllBackendsUnavailable {
                model_name: "memory".to_string(),
            })?
        };

        let request_id = uuid::Uuid::new_v4().to_string();

        let model_request = ModelRequest {
            request_id: request_id.clone(),
            payload: ModelPayload::Memory(MemoryPayload {
                operation: payload.operation.clone(),
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let response = quic_client.send_request(model_request).await?;

        Ok(response)
    }

    /// Routuje request Vision przez LLM z multimodal.
    pub async fn route_vision_via_protocol(
        &self,
        payload: &tentaflow_protocol::VisionPayload,
    ) -> Result<tentaflow_protocol::ModelResponse> {
        use tentaflow_protocol::*;

        let request_id = uuid::Uuid::new_v4().to_string();
        let model_name = self.resolve_to_service_name(&payload.model);

        debug!("Vision: model={}, liczba_wiadomosci={}", model_name, payload.messages.len());

        let openai_messages: Vec<crate::api::openai::types::Message> = payload.messages
            .iter()
            .map(|vm| {
                let parts: Vec<crate::api::openai::types::ContentPart> = vm.content
                    .iter()
                    .map(|part| match part {
                        VisionContentPart::Text { text } => {
                            crate::api::openai::types::ContentPart::Text {
                                text: text.clone(),
                            }
                        }
                        VisionContentPart::ImageUrl { url, detail } => {
                            crate::api::openai::types::ContentPart::ImageUrl {
                                image_url: crate::api::openai::types::ImageUrl {
                                    url: url.clone(),
                                    detail: detail.clone(),
                                },
                            }
                        }
                    })
                    .collect();

                crate::api::openai::types::Message {
                    role: vm.role.clone(),
                    content: Some(crate::api::openai::types::MessageContent::Parts(parts)),
                    ..Default::default()
                }
            })
            .collect();

        let request = crate::api::openai::types::ChatCompletionRequest {
            model: model_name.clone(),
            messages: openai_messages,
            temperature: payload.temperature,
            max_tokens: payload.max_tokens,
            top_p: None,
            n: None,
            stream: false,
            stop: None,
            presence_penalty: None,
            frequency_penalty: None,
            user: None,
            tools: None,
            tool_choice: None,
            response_format: None,
            rag_options: None,
            memory_options: None,
            audio_input: None,
        };

        match self.route_chat_completion(request).await {
            Ok(response) => {
                let content = crate::routing::extract_response_text(&response);

                let cleaned_content = self.response_middleware.clean_text(&content)?;

                let finish_reason = response.choices.first()
                    .and_then(|c| c.finish_reason.clone());

                let metrics = response.usage.map(|usage| ModelMetrics {
                    model_name: response.model.clone(),
                    latency_ms: 0,
                    time_to_first_token_ms: None,
                    tokens_processed: Some(usage.total_tokens as usize),
                    throughput_tokens_per_sec: None,
                    detailed: Some(DetailedMetrics::Completion {
                        prompt_tokens: usage.prompt_tokens,
                        completion_tokens: usage.completion_tokens,
                        total_tokens: usage.total_tokens,
                    }),
                });

                Ok(ModelResponse {
                    request_id,
                    result: ModelResult::Completion(CompletionResult {
                        text: cleaned_content,
                        reasoning_content: None,
                        model: model_name,
                        finish_reason,
                        tool_calls: None,
                        detected_intent: None,
                        detected_tools: None,
                        transcribed_text: None,
                        speaker_id: None,
                        speaker_name: None,
                    }),
                    metrics,
                })
            }
            Err(e) => {
                error!("Blad Vision: {}", e);
                Ok(ModelResponse {
                    request_id,
                    result: ModelResult::Error(ErrorInfo {
                        error_type: ErrorType::InternalError,
                        message: format!("Blad rozumienia obrazu: {}", e),
                        details: None,
                    }),
                    metrics: None,
                })
            }
        }
    }

    /// Routuje request Image (generacja, edycja, wariacje) - niezaimplementowane.
    pub async fn route_image_via_protocol(
        &self,
        operation: &tentaflow_protocol::ImageOperation,
    ) -> Result<tentaflow_protocol::ModelResponse> {
        use tentaflow_protocol::*;

        let request_id = uuid::Uuid::new_v4().to_string();

        let (model, op_name) = match operation {
            ImageOperation::Generate { model, .. } => (model.clone(), "Generacja"),
            ImageOperation::Edit { model, .. } => (model.clone(), "Edycja"),
            ImageOperation::Variation { model, .. } => (model.clone(), "Wariacja"),
        };

        warn!("Operacja {} na obrazie niezaimplementowana dla modelu: {}", op_name, model);

        Ok(ModelResponse {
            request_id,
            result: ModelResult::Error(ErrorInfo {
                error_type: ErrorType::InternalError,
                message: format!("Operacja {} na obrazie niezaimplementowana - wymaga ImageClient", op_name),
                details: None,
            }),
            metrics: None,
        })
    }
}

/// Konwertuje wynik flow engine na standardowy ChatCompletionResponse.
pub(crate) fn flow_result_to_chat_response(result: FlowExecutionResult, model: &str) -> ChatCompletionResponse {
    let json_value = converter::flow_result_to_chat_response(&result, model);
    serde_json::from_value(json_value).unwrap_or_else(|_| {
        let text = result
            .output
            .get("text")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| result.output.to_string());

        ChatCompletionResponse {
            id: format!("flow-{}", uuid::Uuid::new_v4()),
            object: "chat.completion".to_string(),
            created: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            model: model.to_string(),
            choices: vec![Choice {
                index: 0,
                message: Message {
                    role: "assistant".to_string(),
                    content: Some(MessageContent::Text(text)),
                    reasoning_content: None,
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
                finish_reason: Some("stop".to_string()),
                logprobs: None,
            }],
            usage: Some(Usage {
                prompt_tokens: result.prompt_tokens as u32,
                completion_tokens: result.completion_tokens as u32,
                total_tokens: result.total_tokens as u32,
            }),
            system_fingerprint: Some("flow-engine".to_string()),
            transcribed_text: None,
            speaker_id: None,
            speaker_name: None,
            speaker_confidence: None,
            detected_intent: None,
            detected_tools: None,
        }
    })
}
