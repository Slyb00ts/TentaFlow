// =============================================================================
// Plik: routing/streaming.rs
// Opis: Streaming SSE — route_chat_completion_stream, route_to_rag_stream,
//       route_to_quic_llm_stream. Audio input (STT + speaker ID), PII
//       filtering w strumieniu, TTS buffering dla RAG audio output.
// =============================================================================

use crate::api::openai::types::{
    ChatCompletionChunk, ChatCompletionRequest, ChunkChoice, Delta, Message, MessageContent,
    TTSRequest,
};
use crate::error::{CoreError, Result};
use crate::routing::chat::flow_result_to_chat_response;
use crate::routing::router::{RequestMetrics, Router};
use crate::services::tts::{SynthesizeCallback, TTSBufferingProcessor};

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex};
use tentaflow_protocol::*;
use tokio::sync::Mutex as TokioMutex;
use tracing::{debug, error, info, warn};

use futures::stream::StreamExt;
use futures::Stream;

impl Router {
    /// Routuje chat completion request (STREAMING MODE).
    ///
    /// Analogiczna do route_chat_completion() ale zwraca Stream zamiast Response.
    /// Obsluguje voice conversation (STT + speaker identification), intent analysis,
    /// memory integration, PII filtering w strumieniu, memory store po zakonczeniu.
    pub async fn route_chat_completion_stream(
        &self,
        mut request: ChatCompletionRequest,
    ) -> Result<
        crate::routing::RouteResult<
            Pin<Box<dyn Stream<Item = Result<ChatCompletionChunk>> + Send>>,
        >,
    > {
        let stream_start = std::time::Instant::now();
        let stream_node_name = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        // === FLOW ENGINE: proba wykonania przez konfigurowalny flow ===
        if let Some(ref dispatcher) = self.flow_dispatcher {
            // Najpierw streamowa sciezka — tylko gdy flow ma edge from_port="stream".
            let ctx_stream = crate::routing::build_flow_context(&request, true);
            match dispatcher
                .try_dispatch_streaming(&request.model, "chat", ctx_stream)
                .await
            {
                Ok(Some(stream)) => {
                    let metadata = crate::routing::RouteMetadata {
                        served_by_node: stream_node_name.clone(),
                        backend_type: "flow_engine_stream".to_string(),
                        strategy_used: "direct".to_string(),
                        fallbacks_tried: 0,
                        hop_count: 0,
                        latency_ms: Some(stream_start.elapsed().as_secs_f64() * 1000.0),
                    };
                    return Ok(crate::routing::RouteResult {
                        response: stream,
                        metadata,
                    });
                }
                Ok(None) => {}
                Err(e) => {
                    warn!(
                        "Flow Engine streaming error, fallback na blocking/stary pipeline: {}",
                        e
                    );
                }
            }

            let ctx = crate::routing::build_flow_context(&request, true);
            match dispatcher.try_dispatch(&request.model, "chat", ctx).await {
                Ok(Some(result)) => {
                    let response = flow_result_to_chat_response(result, &request.model);
                    let text = response
                        .choices
                        .first()
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
                    return Ok(crate::routing::RouteResult {
                        response: Box::pin(stream),
                        metadata,
                    });
                }
                Ok(None) => {}
                Err(e) => {
                    warn!(
                        "Flow Engine error (stream), fallback na stary pipeline: {}",
                        e
                    );
                }
            }
        }

        let mut metrics = RequestMetrics::new();
        let route = self.resolve_route(&request.model);
        let model_name = route
            .targets
            .first()
            .cloned()
            .unwrap_or_else(|| request.model.clone());
        metrics.model_name = Some(model_name.clone());

        debug!("Routing streaming request dla modelu: {}", model_name);

        // Audio input -> STT + speaker ID. Fizyczna transkrypcja audio na tekst;
        // bez tego klient wysylajacy audio dostaje blad. Bez pre-processingu
        // request-a (intent/memory/context injection) — to robia user-defined flows.
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

                let transcribed_text = match stt_result {
                    Ok(stt_data) => {
                        debug!("STT transkrypcja: '{}'", stt_data.text);
                        stt_data.text
                    }
                    Err(e) => {
                        error!("STT error: {}", e);
                        return Err(e);
                    }
                };

                let speaker_name = speaker_result.speaker_name.clone();

                info!(
                    "Speaker Identification: confidence={}, similarity={:.3}, speaker_id={:?}, speaker_name={:?}",
                    speaker_result.confidence_level,
                    speaker_result.similarity.unwrap_or(0.0),
                    speaker_result.speaker_id,
                    speaker_name
                );

                if transcribed_text.trim().is_empty() {
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

                let should_replace = request
                    .messages
                    .last()
                    .map(|m| {
                        m.role == "user"
                            && m.content
                                .as_ref()
                                .map(|c| match c {
                                    MessageContent::Text(t) => t.trim().is_empty(),
                                    _ => false,
                                })
                                .unwrap_or(true)
                    })
                    .unwrap_or(false);

                if should_replace && !request.messages.is_empty() {
                    let last_idx = request.messages.len() - 1;
                    request.messages[last_idx].content =
                        Some(MessageContent::Text(transcribed_text.clone()));
                } else {
                    request.messages.push(Message {
                        role: "user".to_string(),
                        content: Some(MessageContent::Text(transcribed_text.clone())),
                        reasoning_content: None,
                        name: speaker_name,
                        tool_call_id: None,
                        tool_calls: None,
                    });
                }

                request.audio_input = None;
            }
        }

        // === DISPATCH: iteruj backendy wg strategii, zwroc stream ===
        {
            use crate::routing::middleware::BackendHandle;
            let backends = self.get_backends(&model_name);
            let ordered = self.apply_strategy(&backends, &route.strategy);

            for handle in &ordered {
                match handle {
                    BackendHandle::LocalLlm => {
                        // Hot path: zero JSON hop. Stream<ChatCompletionChunk>
                        // bezposrednio z LocalInferenceHandler — wczesniej
                        // robilismy serde_json::to_string per token (w
                        // local_inference) → unfold serde_json::from_str (tu),
                        // co dla 14 tok/s jest waste 100-400µs/chunk.
                        let chunk_rx = match self.local_inference.stream_chat_chunks(&request).await
                        {
                            Ok(rx) => rx,
                            Err(e) => {
                                debug!("Lokalna inferencja stream error: {}", e);
                                continue;
                            }
                        };
                        let stream = futures::stream::unfold(chunk_rx, |mut rx| async move {
                            rx.recv().await.map(|chunk| (Ok(chunk), rx))
                        });
                        let metadata = crate::routing::RouteMetadata {
                            served_by_node: stream_node_name.clone(),
                            backend_type: "local_llm".to_string(),
                            strategy_used: route.strategy.to_string(),
                            fallbacks_tried: 0,
                            hop_count: 0,
                            latency_ms: Some(stream_start.elapsed().as_secs_f64() * 1000.0),
                        };
                        return Ok(crate::routing::RouteResult {
                            response: Box::pin(stream),
                            metadata,
                        });
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
                                return Ok(crate::routing::RouteResult {
                                    response: stream,
                                    metadata,
                                });
                            }
                            Err(e) => {
                                debug!("RAG stream error: {}", e);
                                continue;
                            }
                        }
                    }
                    BackendHandle::QuicLlm(name) => {
                        match self
                            .route_to_quic_llm_stream(
                                name.clone(),
                                request.clone(),
                                metrics.clone(),
                            )
                            .await
                        {
                            Ok(stream) => {
                                let metadata = crate::routing::RouteMetadata {
                                    served_by_node: stream_node_name.clone(),
                                    backend_type: "quic_llm".to_string(),
                                    strategy_used: route.strategy.to_string(),
                                    fallbacks_tried: 0,
                                    hop_count: 0,
                                    latency_ms: Some(stream_start.elapsed().as_secs_f64() * 1000.0),
                                };
                                return Ok(crate::routing::RouteResult {
                                    response: stream,
                                    metadata,
                                });
                            }
                            Err(e) => {
                                debug!("QUIC LLM stream error: {}", e);
                                continue;
                            }
                        }
                    }
                    BackendHandle::MeshForward(node_id, target_model_name) => {
                        match self
                            .route_to_mesh_llm_stream(
                                node_id.clone(),
                                target_model_name.clone(),
                                request.clone(),
                            )
                            .await
                        {
                            Ok(stream) => {
                                let metadata = crate::routing::RouteMetadata {
                                    served_by_node: node_id.clone(),
                                    backend_type: "mesh_forward_stream".to_string(),
                                    strategy_used: route.strategy.to_string(),
                                    fallbacks_tried: 0,
                                    hop_count: 1,
                                    latency_ms: Some(stream_start.elapsed().as_secs_f64() * 1000.0),
                                };
                                return Ok(crate::routing::RouteResult {
                                    response: stream,
                                    metadata,
                                });
                            }
                            Err(e) => {
                                debug!("MeshForward LLM stream error: {}", e);
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
        let backend =
            self.select_http_backend(&model_name)
                .ok_or_else(|| CoreError::ModelNotFound {
                    model_name: model_name.clone(),
                })?;

        debug!("Wybrany backend streaming: {}", backend.url());

        use std::sync::{Arc as StdArc, Mutex};
        let collected_response: StdArc<Mutex<String>> = StdArc::new(Mutex::new(String::new()));
        let collected_response_clone = collected_response.clone();

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
                    let (content_processor, reasoning_processor) =
                        processors.entry(idx).or_insert_with(|| {
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

        let stream = stream.chain(
            futures::stream::once(async move {
                let _ = collected_response
                    .lock()
                    .map(|r| r.clone())
                    .unwrap_or_default();

                let mut final_metrics = metrics;
                final_metrics.llm_inference_ms = Some(t_llm.elapsed().as_millis() as u64);
                info!("\n{}", final_metrics.format_table());

                Err::<ChatCompletionChunk, anyhow::Error>(anyhow::anyhow!("__metrics_marker__"))
            })
            .filter_map(|r| async move {
                match r {
                    Ok(chunk) => Some(Ok(chunk)),
                    Err(e) if e.to_string() == "__metrics_marker__" => None,
                    Err(e) => Some(Err(e)),
                }
            }),
        );

        let metadata = crate::routing::RouteMetadata {
            served_by_node: stream_node_name,
            backend_type: "http".to_string(),
            strategy_used: "single".to_string(),
            fallbacks_tried: 0,
            hop_count: 0,
            latency_ms: Some(stream_start.elapsed().as_secs_f64() * 1000.0),
        };
        Ok(crate::routing::RouteResult {
            response: Box::pin(stream),
            metadata,
        })
    }

    pub(crate) async fn route_to_mesh_llm_stream(
        &self,
        target_node_id: String,
        target_model_name: String,
        request: ChatCompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatCompletionChunk>> + Send>>> {
        let protocol_messages = crate::routing::openai_messages_to_protocol(&request.messages);
        let request_id = uuid::Uuid::new_v4().to_string();
        let model_request = ModelRequest {
            request_id: request_id.clone(),
            payload: ModelPayload::Completion(CompletionPayload {
                model: target_model_name.clone(),
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
        let payload = rkyv::to_bytes::<rkyv::rancor::Error>(&model_request)
            .map_err(|e| anyhow::anyhow!("mesh stream serialize ModelRequest: {}", e))?
            .into_vec();
        let mesh = self
            .mesh_manager
            .read()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("mesh transport not available"))?;
        let frame_stream = mesh
            .forward_stream_request(&target_node_id, &request_id, payload)
            .await
            .map_err(|e| anyhow::anyhow!("mesh forward stream request: {}", e))?;
        let backend_url = format!("mesh://{}", target_node_id);
        let protocol_stream = frame_stream.map(move |frame_result| {
            let frame = frame_result.map_err(|e| CoreError::NetworkError {
                message: format!("mesh stream read: {}", e),
                source: e,
            })?;
            let archived =
                rkyv::access::<ArchivedModelStreamChunk, rkyv::rancor::Error>(&frame).map_err(
                    |e| CoreError::BackendError {
                        backend_url: backend_url.clone(),
                        message: format!("mesh stream access ModelStreamChunk: {}", e),
                        source: None,
                    },
                )?;
            rkyv::deserialize::<ModelStreamChunk, rkyv::rancor::Error>(archived).map_err(|e| {
                CoreError::BackendError {
                    backend_url: backend_url.clone(),
                    message: format!("mesh stream deserialize ModelStreamChunk: {}", e),
                    source: None,
                }
            })
        });
        Ok(crate::routing::stream_helpers::quic_stream_to_openai_chunks(
            protocol_stream,
            target_model_name,
        ))
    }

    /// Routuje request do RAG engine (STREAMING MODE).
    pub async fn route_to_rag_stream(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatCompletionChunk>> + Send>>> {
        use futures::stream::{self, StreamExt};

        let route = self.resolve_route(&request.model);
        let model_name = route
            .targets
            .first()
            .cloned()
            .unwrap_or_else(|| request.model.clone());

        let rag_handle = self
            .service_manager
            .rag_services
            .get(&model_name)
            .map(|r| r.value().clone())
            .ok_or_else(|| CoreError::ModelNotFound {
                model_name: model_name.clone(),
            })?;

        let rag_client =
            rag_handle
                .get_client()
                .await
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
                details: Some(
                    "messages[] nie zawiera ostatniej wiadomosci z contentem tekstowym".to_string(),
                ),
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

        let (rag_payload, _requires_llm, requires_audio) =
            crate::routing::build_rag_payload(&request, query, context);

        let tts_service_name = if requires_audio {
            request
                .rag_options
                .as_ref()
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
            let llm_model_name = rag_result.llm_model.clone().ok_or_else(|| {
                anyhow::anyhow!("RAG result requires_llm_processing=true ale llm_model=None")
            })?;

            let llm_backend = self.select_http_backend(&llm_model_name).ok_or_else(|| {
                CoreError::ModelNotFound {
                    model_name: llm_model_name.clone(),
                }
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
                    let tts_voice = request
                        .rag_options
                        .as_ref()
                        .and_then(|opts| opts.tts_voice.clone())
                        .unwrap_or_else(|| "default".to_string());
                    let tts_model = tts_name.to_string();

                    let self_clone = self.clone();
                    let synthesize_fn: SynthesizeCallback =
                        Box::new(move |model, input, voice, speed| {
                            let router = self_clone.clone();
                            Box::pin(async move {
                                let request = TTSRequest {
                                    model,
                                    input,
                                    voice,
                                    response_format: Some("wav".to_string()),
                                    speed: Some(speed),
                                    language: None,
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
                        let content = chunk
                            .choices
                            .first()
                            .and_then(|choice| choice.delta.content.clone())
                            .unwrap_or_default();

                        if !content.is_empty() {
                            match processor.process_token(&content) {
                                Ok(Some(cleaned_chunks)) => cleaned_chunks
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
                                    .collect(),
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
                                            let audio_result =
                                                tts.process_token(&content_clone).await;

                                            match audio_result {
                                                Ok(Some(audio_bytes)) => {
                                                    let audio_base64 = base64::Engine::encode(
                                                        &base64::engine::general_purpose::STANDARD,
                                                        &audio_bytes,
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
                                &audio_bytes,
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

        let collected_response: std::sync::Arc<std::sync::Mutex<String>> =
            std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let collected_response_clone = collected_response.clone();

        let quic_client = self
            .service_manager
            .get_quic_llm_client(&llm_name)
            .await
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
                    Ok(stream_chunk) => match stream_chunk.chunk {
                        StreamChunkType::TextDelta(text) => {
                            let cleaned_text = response_middleware
                                .clean_text(&text)
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
                                        role: if first {
                                            Some("assistant".to_string())
                                        } else {
                                            None
                                        },
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
                            let cleaned_reasoning = response_middleware
                                .clean_text(&reasoning)
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
                                        role: if first {
                                            Some("assistant".to_string())
                                        } else {
                                            None
                                        },
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
                    },
                    Err(e) => Some(Err(anyhow::Error::from(e))),
                }
            }
        });

        let stream = stream.chain(
            futures::stream::once(async move {
                let _ = collected_response
                    .lock()
                    .map(|r| r.clone())
                    .unwrap_or_default();

                let mut final_metrics = metrics;
                final_metrics.llm_inference_ms = Some(t_llm.elapsed().as_millis() as u64);
                info!("\n{}", final_metrics.format_table());

                Err::<ChatCompletionChunk, CoreError>(CoreError::InternalError {
                    message: "__metrics_marker__".to_string(),
                    source: None,
                })
            })
            .filter_map(|r| async move {
                match r {
                    Ok(chunk) => Some(Ok(chunk)),
                    Err(ref e) if e.to_string().contains("__metrics_marker__") => None,
                    Err(e) => Some(Err(anyhow::Error::from(e))),
                }
            }),
        );

        Ok(Box::pin(stream))
    }
}
