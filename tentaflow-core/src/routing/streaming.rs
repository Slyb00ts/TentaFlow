// =============================================================================
// Plik: routing/streaming.rs
// Opis: Streaming SSE — route_chat_completion_stream, route_to_rag_stream,
//       route_to_quic_llm_stream. Obsluga PII filtering w strumieniu,
//       TTS buffering, memory store po zakonczeniu streamu.
// =============================================================================

use crate::error::{Result, CoreError};
use crate::api::openai::types::{
    ChatCompletionRequest, ChatCompletionChunk,
    ChunkChoice, Delta, Message, MessageContent, TTSRequest,
};
use crate::routing::router::{Router, RequestMetrics};
use crate::routing::chat::flow_result_to_chat_response;
use crate::services::tts::{SynthesizeCallback, TTSBufferingProcessor};
use crate::intent_analyzer::{Intent, ToolExecutor};

use tentaflow_protocol::*;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Mutex as TokioMutex;
use tracing::{debug, error, info, warn};

use futures::Stream;
use futures::stream::StreamExt;

impl Router {
    /// Routuje chat completion request (STREAMING MODE).
    ///
    /// Analogiczna do route_chat_completion() ale zwraca Stream zamiast Response.
    /// Obsluguje voice conversation (STT + speaker identification), intent analysis,
    /// memory integration, PII filtering w strumieniu, memory store po zakonczeniu.
    pub async fn route_chat_completion_stream(
        &self,
        mut request: ChatCompletionRequest,
    ) -> Result<crate::routing::RouteResult<Pin<Box<dyn Stream<Item = Result<ChatCompletionChunk>> + Send>>>> {
        let stream_start = std::time::Instant::now();
        let stream_node_name = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        // === FLOW ENGINE: proba wykonania przez konfigurowalny flow ===
        if let Some(ref dispatcher) = self.flow_dispatcher {
            let ctx = crate::routing::build_flow_context(&request, true);

            match dispatcher.try_dispatch(&request.model, "chat", ctx).await {
                Ok(Some(result)) => {
                    let response = flow_result_to_chat_response(result, &request.model);
                    let text = response.choices.first()
                        .and_then(|c| c.message.content.as_ref())
                        .map(|c| match c {
                            MessageContent::Text(t) => t.clone(),
                            MessageContent::Parts(_) => String::new(),
                        })
                        .unwrap_or_default();

                    let chunk = ChatCompletionChunk {
                        id: response.id,
                        object: "chat.completion.chunk".to_string(),
                        created: response.created,
                        model: response.model,
                        choices: vec![ChunkChoice {
                            index: 0,
                            delta: Delta {
                                role: Some("assistant".to_string()),
                                content: Some(text),
                                tool_calls: None,
                                reasoning_content: None,
                            },
                            finish_reason: Some("stop".to_string()),
                            logprobs: None,
                        }],
                        system_fingerprint: None,
                        audio: None,
                        detected_intent: None,
                        detected_tools: None,
                        transcribed_text: None,
                        speaker_id: None,
                        speaker_name: None,
                    };

                    let stream = futures::stream::once(async move { Ok(chunk) });
                    let metadata = crate::routing::RouteMetadata {
                        served_by_node: stream_node_name.clone(),
                        backend_type: "flow_engine".to_string(),
                        strategy_used: "direct".to_string(),
                        fallbacks_tried: 0,
                        hop_count: 0,
                        latency_ms: Some(stream_start.elapsed().as_secs_f64() * 1000.0),
                    };
                    return Ok(crate::routing::RouteResult { response: Box::pin(stream), metadata });
                }
                Ok(None) => {}
                Err(e) => {
                    warn!("Flow Engine error (stream), fallback na stary pipeline: {}", e);
                }
            }
        }

        let mut metrics = RequestMetrics::new();
        let route = self.resolve_route(&request.model);
        let model_name = route.targets.first().cloned().unwrap_or_else(|| request.model.clone());
        metrics.model_name = Some(model_name.clone());

        debug!("Routing streaming request dla modelu: {}", model_name);

        // === VOICE CONVERSATION: Przetworz audio przez STT przed LLM ===
        if let Some(ref audio_data) = request.audio_input {
            if !audio_data.is_empty() {
                let t_stt = std::time::Instant::now();

                let audio_for_stt = audio_data.clone();
                let audio_for_speaker = audio_data.clone();

                let (stt_result, speaker_result) = tokio::join!(
                    self.process_stt_for_voice(&audio_for_stt),
                    self.process_speaker_identify(&audio_for_speaker)
                );

                metrics.stt_ms = Some(t_stt.elapsed().as_millis() as u64);

                let (transcribed_text, diarized_speakers) = match stt_result {
                    Ok(stt_data) => {
                        debug!("STT transkrypcja: '{}'", stt_data.text);
                        if stt_data.speakers.len() > 1 {
                            info!("Wykryto {} mowcow w audio", stt_data.speakers.len());
                        }
                        (stt_data.text, stt_data.speakers)
                    }
                    Err(e) => {
                        error!("STT error: {}", e);
                        return Err(e);
                    }
                };

                let speaker_id = speaker_result.speaker_id.clone();
                let speaker_name = speaker_result.speaker_name.clone();
                let speaker_confidence = speaker_result.similarity;

                info!(
                    "Speaker Identification: confidence={}, similarity={:.3}, speaker_id={:?}, speaker_name={:?}",
                    speaker_result.confidence_level,
                    speaker_confidence.unwrap_or(0.0),
                    speaker_id,
                    speaker_name
                );

                match speaker_result.confidence_level.as_str() {
                    "HIGH" => {
                        info!("Speaker HIGH confidence: {} (id: {:?}, similarity: {:?})",
                              speaker_name.as_deref().unwrap_or("?"), speaker_id, speaker_confidence);
                    }
                    "MEDIUM" => {
                        info!("Speaker MEDIUM confidence: {} (id: {:?}, similarity: {:?}) - needs_confirmation={}",
                              speaker_name.as_deref().unwrap_or("?"), speaker_id, speaker_confidence,
                              speaker_result.needs_confirmation);
                    }
                    _ => {
                        debug!("Speaker LOW/UNKNOWN confidence (similarity: {:?}) - new speaker?", speaker_confidence);
                    }
                }

                // Ustaw speaker info w memory_options
                {
                    let memory_opts = request.memory_options.get_or_insert_with(Default::default);
                    if memory_opts.session_id.is_none() {
                        memory_opts.session_id = Some(uuid::Uuid::new_v4().to_string());
                    }

                    if speaker_result.is_high_confidence() {
                        if let Some(ref sid) = speaker_id {
                            memory_opts.person_id = Some(sid.clone());
                            memory_opts.speaker_confidence = speaker_confidence;
                            memory_opts.speaker_name = speaker_name.clone();
                        }
                    } else if speaker_result.is_medium_confidence() {
                        memory_opts.speaker_confidence = speaker_confidence;
                        if let Some(ref name) = speaker_name {
                            let confirmation_hint = speaker_result.confirmation_message.clone()
                                .unwrap_or_else(|| format!("Czy to ty, {}?", name));
                            memory_opts.session_context = Some(format!(
                                "SPEAKER_CANDIDATE: id={}, name={}, confidence={:.2}, ask: {}",
                                speaker_id.as_deref().unwrap_or("?"),
                                name,
                                speaker_confidence.unwrap_or(0.0),
                                confirmation_hint
                            ));
                        }
                    }
                }

                // === INTENT ANALYZER ===
                let session_id = request.memory_options
                    .as_ref()
                    .and_then(|m| m.session_id.clone())
                    .unwrap_or_else(|| "default".to_string());

                let cache = self.memory_integration.conversation_cache();
                let history = cache.get_history(&session_id).await;

                let conversation_context = self.build_context_from_conversation_cache(&history, 4);

                let session_context = conversation_context
                    .or_else(|| request.memory_options
                        .as_ref()
                        .and_then(|m| m.session_context.as_deref())
                        .map(|s| s.to_string()));

                let t_intent = std::time::Instant::now();
                let intent_result = self.intent_analyzer.analyze(
                    &transcribed_text,
                    speaker_id.as_deref(),
                    speaker_name.as_deref(),
                    speaker_confidence,
                    Some(&diarized_speakers),
                    session_context.as_deref(),
                ).await;

                match intent_result {
                    Ok(analysis) => {
                        info!(
                            "Intent Analyzer: intent={:?}, took={:?}ms, tools={}, memory_query={}, reasoning='{}'",
                            analysis.primary_intent,
                            t_intent.elapsed().as_millis(),
                            analysis.tool_calls.len(),
                            analysis.needs_memory_query,
                            analysis.reasoning.chars().take(100).collect::<String>()
                        );

                        // === HANDLE INTRODUCTION ===
                        if let Some(Intent::Introduction { ref name, confidence }) = analysis.primary_intent {
                            info!(
                                "Wykryto przedstawienie: name='{}', confidence={:.2}, speaker_is_unknown={}",
                                name, confidence, speaker_result.is_unknown()
                            );
                            if confidence >= 0.7 && speaker_result.is_unknown() {
                                info!("Intent: ENROLLING new speaker as '{}' (confidence: {:.2})", name, confidence);

                                let new_speaker_id = format!("voice_{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("unknown"));

                                let stt_service_name = self.service_manager.get_first_stt_service_name().map(|s| s.to_string());
                                if let Some(service_name) = stt_service_name {
                                    if let Some(stt_client) = self.service_manager.get_quic_stt_client(&service_name).await {
                                        let memory_client = {
                                            let mut client = None;
                                            let memory_handles: Vec<_> = self.service_manager.quic_memory_services.read().values().cloned().collect();
                                            for handle in memory_handles {
                                                let client_guard = handle.client.read().await;
                                                if let Some(c) = client_guard.as_ref() {
                                                    client = Some(c.clone());
                                                    break;
                                                }
                                            }
                                            client
                                        };

                                        let audio_for_enroll = audio_for_speaker.clone();
                                        let name_for_enroll = name.clone();
                                        let speaker_id_for_enroll = new_speaker_id.clone();
                                        let pending_samples = self.pending_voice_samples.clone();

                                        tokio::spawn(async move {
                                            debug!("Enrolling new speaker: id={}, name={}", speaker_id_for_enroll, name_for_enroll);

                                            let model_request = tentaflow_protocol::ModelRequest {
                                                request_id: uuid::Uuid::new_v4().to_string(),
                                                payload: tentaflow_protocol::ModelPayload::Audio(tentaflow_protocol::AudioPayload {
                                                    operation: tentaflow_protocol::AudioOperation::SpeakerEnroll {
                                                        speaker_id: speaker_id_for_enroll.clone(),
                                                        speaker_name: name_for_enroll.clone(),
                                                        audio_samples: vec![audio_for_enroll],
                                                        metadata: vec![
                                                            ("source".to_string(), "intent_analyzer_introduction".to_string()),
                                                        ],
                                                    },
                                                }),
                                                stream: false,
                                                metadata: None,
                                                session_id: None,
                                            };

                                            match stt_client.send_request(model_request).await {
                                                Ok(response) => {
                                                    info!(
                                                        "SUCCESS: Registered new voice: {} ({}) - response: {:?}",
                                                        name_for_enroll, speaker_id_for_enroll, response.result
                                                    );

                                                    {
                                                        let mut pending = pending_samples.write().await;
                                                        pending.insert(speaker_id_for_enroll.clone(), 3);
                                                    }

                                                    if let Some(mem_client) = memory_client {
                                                        let memory_request = tentaflow_protocol::ModelRequest {
                                                            request_id: uuid::Uuid::new_v4().to_string(),
                                                            payload: tentaflow_protocol::ModelPayload::Memory(tentaflow_protocol::MemoryPayload {
                                                                operation: tentaflow_protocol::MemoryOperation::UpdatePersonName {
                                                                    session_id: "enrollment".to_string(),
                                                                    voice_id: Some(speaker_id_for_enroll.clone()),
                                                                    node_id: None,
                                                                    new_name: name_for_enroll.clone(),
                                                                    preserve_history: false,
                                                                },
                                                            }),
                                                            stream: false,
                                                            metadata: None,
                                                            session_id: None,
                                                        };

                                                        match mem_client.send_request(memory_request).await {
                                                            Ok(_) => {
                                                                info!("SUCCESS: Registered person in Memory: {} ({})",
                                                                    name_for_enroll, speaker_id_for_enroll);
                                                            }
                                                            Err(e) => {
                                                                error!("FAILED to register person in Memory for {}: {}", name_for_enroll, e);
                                                            }
                                                        }
                                                    }
                                                }
                                                Err(e) => {
                                                    error!("FAILED to register voice for {}: {}", name_for_enroll, e);
                                                }
                                            }
                                        });
                                    }
                                }
                            }
                        }

                        // === HANDLE TOOL CALLS ===
                        if !analysis.tool_calls.is_empty() {
                            info!("Executing {} tool call(s)", analysis.tool_calls.len());
                            let execution_results = ToolExecutor::execute_all(&analysis.tool_calls).await;

                            let mut tool_context_parts = Vec::new();
                            for exec_result in &execution_results {
                                if exec_result.success {
                                    tool_context_parts.push(format!("[TOOL RESULT] {}", exec_result.message));
                                } else {
                                    tool_context_parts.push(format!("[TOOL INCOMPLETE] {}", exec_result.message));
                                }
                            }

                            if !tool_context_parts.is_empty() {
                                let memory_opts = request.memory_options.get_or_insert_with(Default::default);
                                let tool_context = tool_context_parts.join("\n");
                                if let Some(ref existing) = memory_opts.session_context {
                                    memory_opts.session_context = Some(format!("{}\n\n{}", existing, tool_context));
                                } else {
                                    memory_opts.session_context = Some(tool_context);
                                }
                            }
                        }

                        // === INJECT CONTEXT FOR LLM ===
                        if let Some(ref ctx) = analysis.context_for_llm {
                            let memory_opts = request.memory_options.get_or_insert_with(Default::default);
                            if let Some(ref existing) = memory_opts.session_context {
                                memory_opts.session_context = Some(format!("{}\n\n{}", existing, ctx));
                            } else {
                                memory_opts.session_context = Some(ctx.clone());
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Intent Analyzer error: {} - continuing without intent analysis", e);
                    }
                }

                // Dodaj transkrypcje jako user message
                if !transcribed_text.trim().is_empty() {
                    // === MULTI-SPEAKER HANDLING ===
                    let multi_speaker_context = if diarized_speakers.len() > 1 {
                        let known_speakers: Vec<_> = diarized_speakers.iter().filter(|s| s.is_known).collect();
                        let unknown_speakers: Vec<_> = diarized_speakers.iter().filter(|s| !s.is_known).collect();

                        let mut context_parts = Vec::new();
                        for speaker in &known_speakers {
                            context_parts.push(format!("[{}] mowi: \"{}\"", speaker.label, speaker.text.trim()));
                        }
                        for (i, speaker) in unknown_speakers.iter().enumerate() {
                            context_parts.push(format!("[Nieznany mowca {}] mowi: \"{}\"", i + 1, speaker.text.trim()));
                        }

                        let instruction = if known_speakers.len() > 0 && unknown_speakers.len() > 0 {
                            let known_names: Vec<_> = known_speakers.iter().map(|s| s.label.as_str()).collect();
                            if unknown_speakers.len() == 1 {
                                format!(
                                    "[INFO MULTI-SPEAKER] W nagraniu slyszę {} osob. Rozpoznaję: {}. Jest tez jedna osoba, ktorej nie znam - zapytaj o imie.",
                                    diarized_speakers.len(), known_names.join(", ")
                                )
                            } else {
                                format!(
                                    "[INFO MULTI-SPEAKER] W nagraniu slyszę {} osob. Rozpoznaję: {}. Jest tez {} osob, ktorych nie znam - popros o przedstawienie sie.",
                                    diarized_speakers.len(), known_names.join(", "), unknown_speakers.len()
                                )
                            }
                        } else if unknown_speakers.len() > 1 {
                            format!(
                                "[INFO MULTI-SPEAKER] W nagraniu slyszę {} nieznanych osob. Popros wszystkich o przedstawienie sie.",
                                unknown_speakers.len()
                            )
                        } else {
                            String::new()
                        };

                        if !instruction.is_empty() {
                            Some(format!("{}\n\nTranskrypcja per mowca:\n{}", instruction, context_parts.join("\n")))
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    if let Some(ref ms_context) = multi_speaker_context {
                        let memory_opts = request.memory_options.get_or_insert_with(Default::default);
                        if let Some(ref existing) = memory_opts.session_context {
                            memory_opts.session_context = Some(format!("{}\n\n{}", existing, ms_context));
                        } else {
                            memory_opts.session_context = Some(ms_context.clone());
                        }
                    }

                    let should_replace = request.messages.last()
                        .map(|m| {
                            m.role == "user" &&
                            m.content.as_ref().map(|c| match c {
                                crate::api::openai::types::MessageContent::Text(t) => t.trim().is_empty(),
                                _ => false,
                            }).unwrap_or(true)
                        })
                        .unwrap_or(false);

                    if should_replace && !request.messages.is_empty() {
                        let last_idx = request.messages.len() - 1;
                        request.messages[last_idx].content = Some(
                            crate::api::openai::types::MessageContent::Text(transcribed_text.clone())
                        );
                    } else {
                        request.messages.push(crate::api::openai::types::Message {
                            role: "user".to_string(),
                            content: Some(crate::api::openai::types::MessageContent::Text(transcribed_text.clone())),
                            reasoning_content: None,
                            name: speaker_name,
                            tool_call_id: None,
                            tool_calls: None,
                        });
                    }
                } else {
                    let metadata = crate::routing::RouteMetadata {
                        served_by_node: stream_node_name.clone(),
                        backend_type: "local_stt".to_string(),
                        strategy_used: "direct".to_string(),
                        fallbacks_tried: 0,
                        hop_count: 0,
                        latency_ms: Some(stream_start.elapsed().as_secs_f64() * 1000.0),
                    };
                    return Ok(crate::routing::RouteResult {
                        response: Box::pin(futures::stream::empty()),
                        metadata,
                    });
                }

                request.audio_input = None;
            }
        }

        // === MEMORY INTEGRATION: Query przed LLM ===
        let (request, _query_decision, mem_timings) = match self.memory_integration.process_request(request.clone()).await {
            Ok((req, decision, timings)) => (req, decision, timings),
            Err(e) => {
                warn!("Memory integration error: {} - kontynuuje bez pamieci", e);
                (request, None, crate::routing::memory_integration::MemoryTimings::default())
            }
        };
        metrics.query_analysis_ms = mem_timings.query_analysis_ms;
        metrics.memory_query_ms = mem_timings.memory_query_ms;

        // === DISPATCH: iteruj backendy wg strategii, zwroc stream ===
        {
            use crate::routing::middleware::BackendHandle;
            let backends = self.get_backends(&model_name);
            let ordered = self.apply_strategy(&backends, &route.strategy);

            for handle in &ordered {
                match handle {
                    BackendHandle::LocalLlm => {
                        let sse_rx = match self.local_inference.handle_chat_completion_stream(&request).await {
                            Ok(rx) => rx,
                            Err(e) => {
                                debug!("Lokalna inferencja stream error: {}", e);
                                continue;
                            }
                        };
                        let stream = futures::stream::unfold(sse_rx, |mut rx| async move {
                            loop {
                                let sse_line = rx.recv().await?;
                                let trimmed = sse_line.trim().to_string();
                                if trimmed == "data: [DONE]" || trimmed.is_empty() {
                                    continue;
                                }
                                if let Some(json_str) = trimmed.strip_prefix("data: ") {
                                    match serde_json::from_str::<ChatCompletionChunk>(json_str) {
                                        Ok(chunk) => return Some((Ok(chunk), rx)),
                                        Err(_) => continue,
                                    }
                                }
                            }
                        });
                        let metadata = crate::routing::RouteMetadata {
                            served_by_node: stream_node_name.clone(),
                            backend_type: "local_llm".to_string(),
                            strategy_used: route.strategy.to_string(),
                            fallbacks_tried: 0,
                            hop_count: 0,
                            latency_ms: Some(stream_start.elapsed().as_secs_f64() * 1000.0),
                        };
                        return Ok(crate::routing::RouteResult { response: Box::pin(stream), metadata });
                    }
                    BackendHandle::Rag(_name) => {
                        match self.route_to_rag_stream(request.clone()).await {
                            Ok(stream) => {
                                let metadata = crate::routing::RouteMetadata {
                                    served_by_node: stream_node_name.clone(),
                                    backend_type: "rag".to_string(),
                                    strategy_used: route.strategy.to_string(),
                                    fallbacks_tried: 0,
                                    hop_count: 0,
                                    latency_ms: Some(stream_start.elapsed().as_secs_f64() * 1000.0),
                                };
                                return Ok(crate::routing::RouteResult { response: stream, metadata });
                            }
                            Err(e) => {
                                debug!("RAG stream error: {}", e);
                                continue;
                            }
                        }
                    }
                    BackendHandle::QuicLlm(name) => {
                        match self.route_to_quic_llm_stream(name.clone(), request.clone(), metrics.clone()).await {
                            Ok(stream) => {
                                let metadata = crate::routing::RouteMetadata {
                                    served_by_node: stream_node_name.clone(),
                                    backend_type: "quic_llm".to_string(),
                                    strategy_used: route.strategy.to_string(),
                                    fallbacks_tried: 0,
                                    hop_count: 0,
                                    latency_ms: Some(stream_start.elapsed().as_secs_f64() * 1000.0),
                                };
                                return Ok(crate::routing::RouteResult { response: stream, metadata });
                            }
                            Err(e) => {
                                debug!("QUIC LLM stream error: {}", e);
                                continue;
                            }
                        }
                    }
                    BackendHandle::Http(_) => {
                        break;
                    }
                    _ => continue,
                }
            }
        }

        // HTTP backend streaming z PII filtering i memory store
        let backend = self.select_http_backend(&model_name)
            .ok_or_else(|| CoreError::ModelNotFound {
                model_name: model_name.clone(),
            })?;

        debug!(
            "Wybrany backend streaming: {}",
            backend.url()
        );

        use std::sync::{Arc as StdArc, Mutex};
        let collected_response: StdArc<Mutex<String>> = StdArc::new(Mutex::new(String::new()));
        let collected_response_clone = collected_response.clone();
        let request_for_memory = request.clone();
        let memory_integration = self.memory_integration.clone();

        let t_llm = std::time::Instant::now();
        let backend_stream = backend.chat_completion_stream(request).await?;

        use crate::middleware::response::StreamingProcessor;

        let response_middleware = self.response_middleware.clone();

        let stream = backend_stream.scan(
            HashMap::<usize, (StreamingProcessor, StreamingProcessor)>::new(),
            move |processors, chunk_result| {
                let mut chunk = match chunk_result {
                    Ok(c) => c,
                    Err(e) => return futures::future::ready(Some(Err(e))),
                };

                for (idx, choice) in chunk.choices.iter_mut().enumerate() {
                    let (content_processor, reasoning_processor) = processors
                        .entry(idx)
                        .or_insert_with(|| {
                            (
                                response_middleware.streaming_processor(),
                                response_middleware.streaming_processor(),
                            )
                        });

                    if let Some(ref content_text) = choice.delta.content {
                        if !content_text.is_empty() {
                            match content_processor.process_token(content_text) {
                                Ok(Some(cleaned_chunks)) => {
                                    let cleaned = cleaned_chunks.join("");
                                    choice.delta.content = Some(cleaned);
                                }
                                Ok(None) => {
                                    choice.delta.content = Some(String::new());
                                }
                                Err(e) => return futures::future::ready(Some(Err(e))),
                            }
                        }
                    }

                    if let Some(ref reasoning_text) = choice.delta.reasoning_content {
                        if !reasoning_text.is_empty() {
                            match reasoning_processor.process_token(reasoning_text) {
                                Ok(Some(cleaned_chunks)) => {
                                    let cleaned = cleaned_chunks.join("");
                                    choice.delta.reasoning_content = Some(cleaned);
                                }
                                Ok(None) => {
                                    choice.delta.reasoning_content = Some(String::new());
                                }
                                Err(e) => return futures::future::ready(Some(Err(e))),
                            }
                        }
                    }
                }

                futures::future::ready(Some(Ok(chunk)))
            },
        );

        let stream = stream.scan(
            (collected_response_clone, false),
            move |(collector, finished), chunk_result| {
                let chunk = match chunk_result {
                    Ok(c) => c,
                    Err(e) => return futures::future::ready(Some(Err(e))),
                };

                for choice in &chunk.choices {
                    if let Some(ref content) = choice.delta.content {
                        if !content.is_empty() {
                            if let Ok(mut response) = collector.lock() {
                                response.push_str(content);
                            }
                        }
                    }

                    if choice.finish_reason.is_some() {
                        *finished = true;
                    }
                }

                futures::future::ready(Some(Ok(chunk)))
            },
        );

        let stream = stream.chain(futures::stream::once(async move {
            let response_text = collected_response.lock()
                .map(|r| r.clone())
                .unwrap_or_default();

            let mut final_metrics = metrics;
            final_metrics.llm_inference_ms = Some(t_llm.elapsed().as_millis() as u64);
            info!("\n{}", final_metrics.format_table());

            if !response_text.is_empty() {
                memory_integration.process_response_async(&request_for_memory, &response_text, None);
            }

            Err::<ChatCompletionChunk, anyhow::Error>(anyhow::anyhow!("__memory_marker__"))
        }).filter_map(|r| async move {
            match r {
                Ok(chunk) => Some(Ok(chunk)),
                Err(e) if e.to_string() == "__memory_marker__" => None,
                Err(e) => Some(Err(e)),
            }
        }));

        let metadata = crate::routing::RouteMetadata {
            served_by_node: stream_node_name,
            backend_type: "http".to_string(),
            strategy_used: "single".to_string(),
            fallbacks_tried: 0,
            hop_count: 0,
            latency_ms: Some(stream_start.elapsed().as_secs_f64() * 1000.0),
        };
        Ok(crate::routing::RouteResult { response: Box::pin(stream), metadata })
    }

    /// Routuje request do RAG engine (STREAMING MODE).
    pub async fn route_to_rag_stream(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatCompletionChunk>> + Send>>> {
        use futures::stream::{self, StreamExt};

        let route = self.resolve_route(&request.model);
        let model_name = route.targets.first().cloned().unwrap_or_else(|| request.model.clone());

        let rag_handle = { self.service_manager.rag_services.read().get(&model_name).cloned() }
            .ok_or_else(|| CoreError::ModelNotFound {
                model_name: model_name.clone(),
            })?;

        let rag_client = rag_handle.get_client().await
            .ok_or_else(|| CoreError::AllBackendsUnavailable {
                model_name: model_name.clone(),
            })?;

        let query = request
            .messages
            .last()
            .and_then(|m| match &m.content {
                Some(MessageContent::Text(text)) => Some(text.clone()),
                _ => None,
            })
            .ok_or_else(|| CoreError::InvalidRequest {
                message: "Brak user message w request".to_string(),
                details: Some("messages[] nie zawiera ostatniej wiadomosci z contentem tekstowym".to_string()),
            })?;

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

        let (rag_payload, _requires_llm, requires_audio) = crate::routing::build_rag_payload(&request, query, context);

        let tts_service_name = if requires_audio {
            request.rag_options.as_ref()
                .and_then(|opts| opts.tts_model.as_ref())
                .map(|s| s.to_string())
                .or_else(|| self.service_manager.get_first_tts_service_name())
        } else {
            None
        };

        let rag_result = rag_client.send_request(rag_payload).await?;

        let response_middleware = self.response_middleware.clone();
        let chat_id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
        let created = chrono::Utc::now().timestamp() as u64;

        if rag_result.requires_llm_processing {
            let llm_model_name = rag_result
                .llm_model
                .clone()
                .ok_or_else(|| anyhow::anyhow!("RAG result requires_llm_processing=true ale llm_model=None"))?;

            let llm_backend = self.select_http_backend(&llm_model_name)
                .ok_or_else(|| CoreError::ModelNotFound {
                    model_name: llm_model_name.clone(),
                })?;

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
                stream: true,
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

            let llm_stream = llm_backend.chat_completion_stream(llm_request).await?;

            let processor = Arc::new(StdMutex::new(response_middleware.streaming_processor()));
            let processor_for_stream = processor.clone();
            let processor_for_flush = processor.clone();
            let llm_model_name_for_flush = llm_model_name.clone();

            let tts_processor = if requires_audio {
                if let Some(tts_name) = tts_service_name {
                    let tts_voice = request.rag_options.as_ref()
                        .and_then(|opts| opts.tts_voice.clone())
                        .unwrap_or_else(|| "default".to_string());
                    let tts_model = tts_name.to_string();

                    let self_clone = self.clone();
                    let synthesize_fn: SynthesizeCallback = Box::new(move |model, input, voice, speed| {
                        let router = self_clone.clone();
                        Box::pin(async move {
                            let request = TTSRequest {
                                model,
                                input,
                                voice,
                                response_format: Some("wav".to_string()),
                                speed: Some(speed),
                            };
                            router.synthesize_speech(&request).await.map(|r| r.response)
                        })
                    });

                    Some(Arc::new(TokioMutex::new(TTSBufferingProcessor::new(
                        synthesize_fn,
                        tts_model,
                        tts_voice,
                        "wav".to_string(),
                        1.0,
                    ))))
                } else {
                    None
                }
            } else {
                None
            };
            let tts_processor_for_stream = tts_processor.clone();
            let tts_processor_for_flush = tts_processor.clone();

            let stream = llm_stream.flat_map(move |chunk_result| {
                let mut processor = processor_for_stream.lock().unwrap();
                let chunks: Vec<Result<ChatCompletionChunk>> = match chunk_result {
                    Ok(chunk) => {
                        let content = chunk.choices.first()
                            .and_then(|choice| choice.delta.content.clone())
                            .unwrap_or_default();

                        if !content.is_empty() {
                            match processor.process_token(&content) {
                                Ok(Some(cleaned_chunks)) => {
                                    cleaned_chunks
                                        .into_iter()
                                        .map(|cleaned_content| {
                                            Ok(ChatCompletionChunk {
                                                id: chunk.id.clone(),
                                                object: "chat.completion.chunk".to_string(),
                                                created: chunk.created,
                                                model: chunk.model.clone(),
                                                choices: vec![ChunkChoice {
                                                    index: 0,
                                                    delta: Delta {
                                                        role: None,
                                                        content: Some(cleaned_content),
                                                        reasoning_content: None,
                                                        tool_calls: None,
                                                    },
                                                    finish_reason: None,
                                                    logprobs: None,
                                                }],
                                                system_fingerprint: None,
                                                audio: None,
                                                detected_intent: None,
                                                detected_tools: None,
                                                transcribed_text: None,
                                                speaker_id: None,
                                                speaker_name: None,
                                            })
                                        })
                                        .collect()
                                }
                                Ok(None) => vec![],
                                Err(e) => vec![Err(e)],
                            }
                        } else {
                            vec![Ok(chunk)]
                        }
                    }
                    Err(e) => vec![Err(e)],
                };

                stream::iter(chunks)
            });

            let stream_with_tts = stream.then(move |chunk_result| {
                let tts_proc = tts_processor_for_stream.clone();
                async move {
                    match chunk_result {
                        Ok(mut chunk) => {
                            if let Some(ref tts_processor) = tts_proc {
                                if let Some(choice) = chunk.choices.first() {
                                    if let Some(ref content) = choice.delta.content {
                                        if !content.is_empty() {
                                            let content_clone = content.clone();
                                            let mut tts = tts_processor.lock().await;
                                            let audio_result = tts.process_token(&content_clone).await;

                                            match audio_result {
                                                Ok(Some(audio_bytes)) => {
                                                    let audio_base64 = base64::Engine::encode(
                                                        &base64::engine::general_purpose::STANDARD,
                                                        &audio_bytes
                                                    );
                                                    chunk.audio = Some(audio_base64);
                                                }
                                                Ok(None) => {}
                                                Err(e) => {
                                                    error!("Blad TTS synthesis: {}", e);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            Ok(chunk)
                        }
                        Err(e) => Err(e),
                    }
                }
            });

            let flush_stream = stream::once(async move {
                let mut chunks = Vec::new();

                let text_flush_result = {
                    let mut processor = processor_for_flush.lock().unwrap();
                    processor.flush()
                };

                match text_flush_result {
                    Ok(flushed_chunks) => {
                        for cleaned_content in flushed_chunks {
                            chunks.push(Ok(ChatCompletionChunk {
                                id: "flush-chunk".to_string(),
                                object: "chat.completion.chunk".to_string(),
                                created: chrono::Utc::now().timestamp() as u64,
                                model: llm_model_name_for_flush.clone(),
                                choices: vec![ChunkChoice {
                                    index: 0,
                                    delta: Delta {
                                        role: None,
                                        content: Some(cleaned_content),
                                        reasoning_content: None,
                                        tool_calls: None,
                                    },
                                    finish_reason: None,
                                    logprobs: None,
                                }],
                                system_fingerprint: None,
                                audio: None,
                                detected_intent: None,
                                detected_tools: None,
                                transcribed_text: None,
                                speaker_id: None,
                                speaker_name: None,
                            }));
                        }
                    }
                    Err(e) => {
                        error!("Blad flush text: {}", e);
                        chunks.push(Err(e));
                        return chunks;
                    }
                }

                if let Some(ref tts_processor) = tts_processor_for_flush {
                    let mut tts = tts_processor.lock().await;
                    match tts.flush().await {
                        Ok(Some(audio_bytes)) => {
                            let audio_base64 = base64::Engine::encode(
                                &base64::engine::general_purpose::STANDARD,
                                &audio_bytes
                            );
                            chunks.push(Ok(ChatCompletionChunk {
                                id: "flush-audio-chunk".to_string(),
                                object: "chat.completion.chunk".to_string(),
                                created: chrono::Utc::now().timestamp() as u64,
                                model: llm_model_name_for_flush.clone(),
                                choices: vec![ChunkChoice {
                                    index: 0,
                                    delta: Delta {
                                        role: None,
                                        content: None,
                                        reasoning_content: None,
                                        tool_calls: None,
                                    },
                                    finish_reason: None,
                                    logprobs: None,
                                }],
                                system_fingerprint: None,
                                audio: Some(audio_base64),
                                detected_intent: None,
                                detected_tools: None,
                                transcribed_text: None,
                                speaker_id: None,
                                speaker_name: None,
                            }));
                        }
                        Ok(None) => {}
                        Err(e) => {
                            error!("Blad flush TTS: {}", e);
                            chunks.push(Err(e));
                        }
                    }
                }

                chunks
            })
            .flat_map(stream::iter);

            let stream_with_flush = stream_with_tts.chain(flush_stream);

            Ok(Box::pin(stream_with_flush))
        } else {
            let cleaned_content = response_middleware.clean_text(&rag_result.context_text)?;

            let chunk = ChatCompletionChunk {
                id: chat_id,
                object: "chat.completion.chunk".to_string(),
                created,
                model: model_name,
                choices: vec![ChunkChoice {
                    index: 0,
                    delta: Delta {
                        role: Some("assistant".to_string()),
                        content: Some(cleaned_content),
                        reasoning_content: None,
                        tool_calls: None,
                    },
                    finish_reason: Some("stop".to_string()),
                    logprobs: None,
                }],
                system_fingerprint: None,
                audio: None,
                detected_intent: None,
                detected_tools: None,
                transcribed_text: None,
                speaker_id: None,
                speaker_name: None,
            };

            Ok(Box::pin(stream::once(async { Ok(chunk) })))
        }
    }

    /// Routuje request do QUIC LLM engine (STREAMING MODE).
    pub(crate) async fn route_to_quic_llm_stream(
        &self,
        llm_name: String,
        request: ChatCompletionRequest,
        metrics: RequestMetrics,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatCompletionChunk>> + Send>>> {
        use tentaflow_protocol::*;

        debug!("Routing streaming to QUIC LLM: {}", llm_name);

        let t_llm = std::time::Instant::now();

        let request_for_memory = request.clone();
        let memory_integration = self.memory_integration.clone();

        let collected_response: std::sync::Arc<std::sync::Mutex<String>> =
            std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let collected_response_clone = collected_response.clone();

        let quic_client = self.service_manager.get_quic_llm_client(&llm_name).await
            .ok_or_else(|| CoreError::AllBackendsUnavailable {
                model_name: llm_name.clone(),
            })?;

        let protocol_messages = crate::routing::openai_messages_to_protocol(&request.messages);

        let request_id = uuid::Uuid::new_v4().to_string();
        let model_request = ModelRequest {
            request_id: request_id.clone(),
            payload: ModelPayload::Completion(CompletionPayload {
                model: request.model.clone(),
                prompt: None,
                messages: protocol_messages,
                temperature: request.temperature,
                max_tokens: request.max_tokens,
                top_p: request.top_p,
                stop: request.stop.clone(),
                presence_penalty: request.presence_penalty,
                frequency_penalty: request.frequency_penalty,
                tts_options: None,
                memory_options: None,
                audio_input: None,
                prefix_cache_id: None,
                prefix_text: None,
            }),
            stream: true,
            metadata: None,
            session_id: None,
        };

        let quic_stream = quic_client.send_request_stream(model_request).await?;

        let chat_id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
        let created = chrono::Utc::now().timestamp() as u64;
        let response_middleware = self.response_middleware.clone();

        let is_first = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));

        let stream = quic_stream.filter_map(move |chunk_result| {
            let chat_id = chat_id.clone();
            let llm_name = llm_name.clone();
            let response_middleware = response_middleware.clone();
            let is_first = is_first.clone();
            let collected_response = collected_response_clone.clone();

            async move {
                match chunk_result {
                    Ok(stream_chunk) => {
                        match stream_chunk.chunk {
                            StreamChunkType::TextDelta(text) => {
                                let cleaned_text = response_middleware.clean_text(&text)
                                    .unwrap_or_else(|_| text.clone());

                                if let Ok(mut response) = collected_response.lock() {
                                    response.push_str(&cleaned_text);
                                }

                                let first = is_first.swap(false, std::sync::atomic::Ordering::SeqCst);

                                Some(Ok(ChatCompletionChunk {
                                    id: chat_id,
                                    object: "chat.completion.chunk".to_string(),
                                    created,
                                    model: llm_name,
                                    choices: vec![ChunkChoice {
                                        index: 0,
                                        delta: Delta {
                                            role: if first { Some("assistant".to_string()) } else { None },
                                            content: Some(cleaned_text),
                                            reasoning_content: None,
                                            tool_calls: None,
                                        },
                                        finish_reason: None,
                                        logprobs: None,
                                    }],
                                    system_fingerprint: None,
                                    audio: None,
                                    detected_intent: None,
                                    detected_tools: None,
                                    transcribed_text: None,
                                    speaker_id: None,
                                    speaker_name: None,
                                }))
                            }
                            StreamChunkType::ReasoningDelta(reasoning) => {
                                let cleaned_reasoning = response_middleware.clean_text(&reasoning)
                                    .unwrap_or_else(|_| reasoning.clone());
                                let first = is_first.swap(false, std::sync::atomic::Ordering::SeqCst);

                                Some(Ok(ChatCompletionChunk {
                                    id: chat_id,
                                    object: "chat.completion.chunk".to_string(),
                                    created,
                                    model: llm_name,
                                    choices: vec![ChunkChoice {
                                        index: 0,
                                        delta: Delta {
                                            role: if first { Some("assistant".to_string()) } else { None },
                                            content: None,
                                            reasoning_content: Some(cleaned_reasoning),
                                            tool_calls: None,
                                        },
                                        finish_reason: None,
                                        logprobs: None,
                                    }],
                                    system_fingerprint: None,
                                    audio: None,
                                    detected_intent: None,
                                    detected_tools: None,
                                    transcribed_text: None,
                                    speaker_id: None,
                                    speaker_name: None,
                                }))
                            }
                            StreamChunkType::Done { final_metrics: _ } => {
                                Some(Ok(ChatCompletionChunk {
                                    id: chat_id,
                                    object: "chat.completion.chunk".to_string(),
                                    created,
                                    model: llm_name,
                                    choices: vec![ChunkChoice {
                                        index: 0,
                                        delta: Delta {
                                            role: None,
                                            content: None,
                                            reasoning_content: None,
                                            tool_calls: None,
                                        },
                                        finish_reason: Some("stop".to_string()),
                                        logprobs: None,
                                    }],
                                    system_fingerprint: None,
                                    audio: None,
                                    detected_intent: None,
                                    detected_tools: None,
                                    transcribed_text: None,
                                    speaker_id: None,
                                    speaker_name: None,
                                }))
                            }
                            StreamChunkType::Metadata(_) => None,
                            _ => None,
                        }
                    }
                    Err(e) => Some(Err(anyhow::Error::from(e))),
                }
            }
        });

        let stream = stream.chain(futures::stream::once(async move {
            let response_text = collected_response.lock()
                .map(|r| r.clone())
                .unwrap_or_default();

            let mut final_metrics = metrics;
            final_metrics.llm_inference_ms = Some(t_llm.elapsed().as_millis() as u64);
            info!("\n{}", final_metrics.format_table());

            if !response_text.is_empty() {
                memory_integration.process_response_async(&request_for_memory, &response_text, None);
            }

            Err::<ChatCompletionChunk, CoreError>(CoreError::InternalError {
                message: "__memory_marker__".to_string(),
                source: None,
            })
        }).filter_map(|r| async move {
            match r {
                Ok(chunk) => Some(Ok(chunk)),
                Err(ref e) if e.to_string().contains("__memory_marker__") => None,
                Err(e) => Some(Err(anyhow::Error::from(e))),
            }
        }));

        Ok(Box::pin(stream))
    }
}
