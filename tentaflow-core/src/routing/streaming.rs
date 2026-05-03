// =============================================================================
// Plik: routing/streaming.rs
// Opis: Streaming SSE — route_chat_completion_stream, route_to_quic_llm_stream.
//       Audio input (STT + speaker ID), PII filtering w strumieniu, TTS
//       buffering.
// =============================================================================

use crate::api::openai::types::{
    ChatCompletionChunk, ChatCompletionRequest, ChunkChoice, Delta, MessageContent,
};
use crate::error::{CoreError, Result};
use crate::routing::chat::flow_result_to_chat_response;
use crate::routing::router::{RequestMetrics, Router};

use std::pin::Pin;
use tentaflow_protocol::*;
use tracing::{debug, info, warn};

use futures::stream::StreamExt;
use futures::Stream;

/// Single PII streaming wrapper — uzywany przez wszystkie streaming
/// route paths (executor, flow streaming, LocalLlm, QuicLlm, MeshForward,
/// legacy HTTP). Per-chunk `StreamingProcessor.process_token` + EOF flush
/// dla buforowanych tail tokenow.
///
/// Niezmienniki:
/// - Procesory keyed per `choice.index` (u32) — nie per pozycja w wektorze
///   `chunk.choices`. Klient OpenAI moze wysylac chunki z tylko choice
///   `index=1` (n>1, parallel sampling) i `enumerate()` mieszaloby
///   procesory miedzy choices.
/// - Chunki z `finish_reason = Some(_)` sa wstrzymywane i emitowane PO
///   `flush_tail`. Inaczej buforowany tail token wpadalby po `[DONE]`/stop
///   i klient ktory zatrzymuje sie na finish bylby pozbawiony konca tekstu.
///   Jezeli oryginalny finish chunk niesie tez `delta.content` /
///   `delta.reasoning_content`, splitujemy go na content-only (emit od razu,
///   przepuszczone przez procesor) + finish-only (hold do konca).
fn wrap_with_pii_streaming(
    upstream: std::pin::Pin<
        Box<
            dyn futures::Stream<
                    Item = crate::error::Result<
                        crate::api::openai::types::ChatCompletionChunk,
                    >,
                > + Send,
        >,
    >,
    response_middleware: std::sync::Arc<crate::middleware::response::ResponseMiddleware>,
) -> std::pin::Pin<
    Box<
        dyn futures::Stream<
                Item = crate::error::Result<crate::api::openai::types::ChatCompletionChunk>,
            > + Send,
    >,
> {
    use crate::api::openai::types::{ChatCompletionChunk, ChunkChoice, Delta};
    use crate::middleware::response::StreamingProcessor;
    use futures::StreamExt;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    type ProcMap = HashMap<u32, (StreamingProcessor, StreamingProcessor)>;
    let processors: Arc<Mutex<ProcMap>> = Arc::new(Mutex::new(HashMap::new()));
    let template: Arc<Mutex<Option<ChatCompletionChunk>>> = Arc::new(Mutex::new(None));
    let pending_finishes: Arc<Mutex<Vec<ChatCompletionChunk>>> =
        Arc::new(Mutex::new(Vec::new()));

    let p_filter = processors.clone();
    let t_filter = template.clone();
    let rm_filter = response_middleware.clone();
    let pf_filter = pending_finishes.clone();

    let filtered = upstream.flat_map(move |chunk_result| {
        let outputs: Vec<crate::error::Result<ChatCompletionChunk>> = match chunk_result {
            Err(e) => vec![Err(e)],
            Ok(chunk) => {
                {
                    let mut tpl = t_filter.lock().unwrap();
                    if tpl.is_none() {
                        let mut empty = chunk.clone();
                        empty.choices.clear();
                        *tpl = Some(empty);
                    }
                }
                let mut procs = p_filter.lock().unwrap();
                let mut content_choices: Vec<ChunkChoice> = Vec::new();
                let mut finish_choices: Vec<ChunkChoice> = Vec::new();
                let mut error: Option<anyhow::Error> = None;

                for choice in chunk.choices.iter() {
                    let entry = procs.entry(choice.index).or_insert_with(|| {
                        (
                            rm_filter.streaming_processor(),
                            rm_filter.streaming_processor(),
                        )
                    });

                    let mut processed = choice.clone();
                    if let Some(text) = &choice.delta.content {
                        if !text.is_empty() {
                            match entry.0.process_token(text) {
                                Ok(Some(cleaned)) => {
                                    processed.delta.content = Some(cleaned.join(""))
                                }
                                Ok(None) => processed.delta.content = Some(String::new()),
                                Err(e) => {
                                    error = Some(e);
                                    break;
                                }
                            }
                        }
                    }
                    if let Some(text) = &choice.delta.reasoning_content {
                        if !text.is_empty() {
                            match entry.1.process_token(text) {
                                Ok(Some(cleaned)) => {
                                    processed.delta.reasoning_content = Some(cleaned.join(""))
                                }
                                Ok(None) => {
                                    processed.delta.reasoning_content = Some(String::new())
                                }
                                Err(e) => {
                                    error = Some(e);
                                    break;
                                }
                            }
                        }
                    }

                    if processed.finish_reason.is_some() {
                        let has_content = processed
                            .delta
                            .content
                            .as_deref()
                            .map(|s| !s.is_empty())
                            .unwrap_or(false)
                            || processed
                                .delta
                                .reasoning_content
                                .as_deref()
                                .map(|s| !s.is_empty())
                                .unwrap_or(false)
                            || processed.delta.tool_calls.is_some();
                        if has_content {
                            let mut content_only = processed.clone();
                            content_only.finish_reason = None;
                            content_choices.push(content_only);
                        }
                        let mut finish_only = processed;
                        finish_only.delta.content = None;
                        finish_only.delta.reasoning_content = None;
                        finish_only.delta.tool_calls = None;
                        finish_choices.push(finish_only);
                    } else {
                        content_choices.push(processed);
                    }
                }
                drop(procs);

                if let Some(e) = error {
                    vec![Err(e)]
                } else {
                    if !finish_choices.is_empty() {
                        let mut hold = chunk.clone();
                        hold.choices = finish_choices;
                        pf_filter.lock().unwrap().push(hold);
                    }
                    if content_choices.is_empty() {
                        Vec::new()
                    } else {
                        let mut out = chunk;
                        out.choices = content_choices;
                        vec![Ok(out)]
                    }
                }
            }
        };
        futures::stream::iter(outputs)
    });

    let p_flush = processors.clone();
    let t_flush = template.clone();
    let pf_tail = pending_finishes.clone();
    let tail = futures::stream::once(async move {
        let mut out: Vec<crate::error::Result<ChatCompletionChunk>> = Vec::new();
        let template = t_flush.lock().unwrap().clone();
        if let Some(template) = template {
            let mut procs = p_flush.lock().unwrap();
            for (idx, (content_proc, reasoning_proc)) in procs.iter_mut() {
                let content_tail = content_proc.flush().unwrap_or_default().join("");
                let reasoning_tail = reasoning_proc.flush().unwrap_or_default().join("");
                if content_tail.is_empty() && reasoning_tail.is_empty() {
                    continue;
                }
                let mut chunk = template.clone();
                chunk.choices = vec![ChunkChoice {
                    index: *idx,
                    delta: Delta {
                        role: None,
                        content: if content_tail.is_empty() {
                            None
                        } else {
                            Some(content_tail)
                        },
                        reasoning_content: if reasoning_tail.is_empty() {
                            None
                        } else {
                            Some(reasoning_tail)
                        },
                        tool_calls: None,
                    },
                    finish_reason: None,
                    logprobs: None,
                }];
                out.push(Ok(chunk));
            }
        }
        for finish in pf_tail.lock().unwrap().drain(..) {
            out.push(Ok(finish));
        }
        out
    })
    .flat_map(futures::stream::iter);

    Box::pin(filtered.chain(tail))
}

impl Router {
    /// Routuje chat completion request (STREAMING MODE).
    ///
    /// Analogiczna do route_chat_completion() ale zwraca Stream zamiast Response.
    /// Obsluguje voice conversation (STT + speaker identification), intent analysis,
    /// memory integration, PII filtering w strumieniu, memory store po zakonczeniu.
    pub async fn route_chat_completion_stream(
        &self,
        request: ChatCompletionRequest,
        user: Option<crate::auth::acl::UserContext>,
    ) -> Result<
        crate::routing::RouteResult<
            Pin<Box<dyn Stream<Item = Result<ChatCompletionChunk>> + Send>>,
        >,
    > {
        let stream_start = std::time::Instant::now();
        let stream_node_name = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        if let Some(ref u) = user {
            if let Some(ref db) = self.db {
                if !crate::auth::acl::check_access_safe(
                    db,
                    "model",
                    &request.model,
                    u.user_id,
                    &u.role,
                ) {
                    tracing::warn!(
                        user_id = u.user_id,
                        model = %request.model,
                        "ACL denied model access (stream)"
                    );
                    return Err(crate::error::CoreError::AllBackendsUnavailable {
                        model_name: request.model.clone(),
                    }
                    .into());
                }
            }
        }

        // Audio capability guard — mirror of the non-streaming path. Without
        // it a client rejected on `POST /v1/chat/completions` could flip
        // `stream:true` and reach the legacy hidden-STT flow below, which
        // the unified catalog explicitly forbids. Alias surfaces follow
        // their primary target's modalities; an alias whose primary is
        // text-only rejects audio even when an audio-capable fallback
        // exists — fallbacks are filtered per-request inside the resolver,
        // not at the handler boundary.
        // R6.P3: empty `Some(vec![])` is a client bug — reject before
        // capability guard so the operator sees the empty payload, not
        // a confusing capability error downstream.
        if let Some(ref bytes) = request.audio_input {
            if bytes.is_empty() {
                return Err(crate::error::CoreError::InvalidRequest {
                    message: "audio_input is present but empty (0 bytes)".to_string(),
                    details: Some(
                        "Send a non-empty audio payload or omit audio_input entirely.".to_string(),
                    ),
                }
                .into());
            }
        }
        let target_accepts_audio = if request.audio_input.is_some() {
            let snap = self.catalog_snapshot();
            if !crate::routing::chat::catalog_target_accepts_audio(&snap, &request.model) {
                tracing::warn!(
                    model = %request.model,
                    "audio_input_unsupported (streaming): target does not declare Audio in input_modalities"
                );
                return Err(crate::error::CoreError::InvalidRequest {
                    message: format!(
                        "audio_input_unsupported: model '{}' does not accept audio input",
                        request.model
                    ),
                    details: Some(
                        "Use /v1/audio/transcriptions for STT, or pick a model with audio_input capability"
                            .to_string(),
                    ),
                }
                .into());
            }
            true
        } else {
            false
        };

        // === FLOW ENGINE: proba wykonania przez konfigurowalny flow ===
        if let Some(ref dispatcher) = self.flow_dispatcher {
            // Najpierw streamowa sciezka — tylko gdy flow ma edge from_port="stream".
            let ctx_stream = crate::routing::build_flow_context_for_user(&request, true, user.clone());
            match dispatcher
                .try_dispatch_streaming(&request.model, "chat", ctx_stream)
                .await
            {
                Ok(Some(stream)) => {
                    // Codex H1 round 2: flow streaming path tez musi mieć
                    // PII filter — wczesniej tylko executor sciezka miala
                    // scan z StreamingProcessor. Reuse tego samego helpera
                    // co executor success path (wymaga lift do osobnej fn).
                    let filtered = wrap_with_pii_streaming(stream, self.response_middleware.clone());
                    let metadata = crate::routing::RouteMetadata {
                        served_by_node: stream_node_name.clone(),
                        backend_type: "flow_engine_stream".to_string(),
                        strategy_used: "direct".to_string(),
                        fallbacks_tried: 0,
                        hop_count: 0,
                        latency_ms: Some(stream_start.elapsed().as_secs_f64() * 1000.0),
                    };
                    return Ok(crate::routing::RouteResult {
                        response: filtered,
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

            let ctx = crate::routing::build_flow_context_for_user(&request, true, user.clone());
            match dispatcher.try_dispatch(&request.model, "chat", ctx).await {
                Ok(Some(result)) => {
                    let response = flow_result_to_chat_response(result, &request.model);
                    let raw_text = response
                        .choices
                        .first()
                        .and_then(|c| c.message.content.as_ref())
                        .map(|c| match c {
                            MessageContent::Text(t) => t.clone(),
                            MessageContent::Parts(_) => String::new(),
                        })
                        .unwrap_or_default();
                    // Codex H1 round 2: PII filter na single-chunk
                    // (blocking flow → wrapped in stream::once). Aplikujemy
                    // pelne `clean_text` (non-streaming) bo mamy caly text
                    // od razu — StreamingProcessor jest dla token-by-token.
                    let text = self
                        .response_middleware
                        .clean_text(&raw_text)
                        .unwrap_or(raw_text);

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

        // R2d (D.7): chat streaming NIE robi ukrytego STT. Po
        // `target_accepts_audio` guard wyzej, audio_input dociera albo do
        // audio-capable backendu w surowej formie albo request zostaje
        // odrzucony (`audio_input_unsupported`). Speaker info i transkrypcja
        // odbywaja sie jawnie przez /v1/audio/transcriptions albo flow z STT
        // node — nie chowamy ich w strumieniu chat completion.
        let _ = target_accepts_audio;

        // R3a stream: spróbuj jednolity dispatch przez ModelRuntimeExecutor.
        // MeshForward + Flow streaming sa deferred do follow-up — wracamy
        // tam do legacy per-target loop ponizej. HTTP streaming z PII
        // middleware pozostaje na ostatecznej sciezce ponizej (executor
        // MVP nie aplikuje response middleware na chunkach).
        let executor_snapshot = self.executor.read().clone();
        if let Some(executor) = executor_snapshot {
            use crate::services::runtime::context::ExecutionContext;
            use crate::services::runtime::executor::ExecutorError;
            let mut exec_ctx = ExecutionContext {
                user: user.clone(),
                ..ExecutionContext::default()
            };
            match executor.stream_chat(request.clone(), &mut exec_ctx).await {
                Ok(stream) => {
                    // Codex H2 + H3 round 2: PII filter + EOF flush — wspolny
                    // helper dla executor + flow streaming paths.
                    let filtered = wrap_with_pii_streaming(
                        stream,
                        self.response_middleware.clone(),
                    );
                    let metadata = crate::routing::RouteMetadata {
                        served_by_node: exec_ctx
                            .route_metadata
                            .served_by_node
                            .unwrap_or_else(|| stream_node_name.clone()),
                        backend_type: exec_ctx
                            .route_metadata
                            .backend_type
                            .unwrap_or_else(|| "executor_stream".to_string()),
                        strategy_used: "executor".to_string(),
                        fallbacks_tried: exec_ctx.route_metadata.fallbacks_tried,
                        hop_count: 0,
                        latency_ms: Some(stream_start.elapsed().as_secs_f64() * 1000.0),
                    };
                    return Ok(crate::routing::RouteResult {
                        response: filtered,
                        metadata,
                    });
                }
                Err(ExecutorError::TransportPendingCutover(tag)) => {
                    debug!(
                        target = tag,
                        "executor.stream_chat → TransportPendingCutover, fallback na legacy stream dispatch"
                    );
                }
                Err(e) => {
                    debug!(
                        "executor.stream_chat error: {} — fallback na legacy stream dispatch",
                        e
                    );
                }
            }
        }

        // === DISPATCH: iteruj backendy wg strategii, zwroc stream ===
        // R7.P1: pre-fix uzywal tylko `route.targets.first()`, wiec alias
        // `text-primary -> audio-fallback` nigdy nie probowal fallbacka,
        // mimo ze guard P1b admituje audio dla aliasow z audio fallbackiem.
        // Iterujemy targety w kolejnosci jak `dispatch_with_fallback` w chat.
        {
            use crate::routing::middleware::BackendHandle;
            // Mirror the chat path: when the target accepted audio bypass
            // we filter dispatch to instances that actually advertise audio
            // input, so a sibling text-only instance of the same model
            // name cannot receive raw audio.
            let required_input = if target_accepts_audio {
                Some(crate::services::catalog::InputModality::Audio)
            } else {
                None
            };
            for current_target in &route.targets {
            let backends = self.get_backends(current_target, required_input);
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
                        let wrapped = wrap_with_pii_streaming(
                            Box::pin(stream),
                            self.response_middleware.clone(),
                        );
                        let metadata = crate::routing::RouteMetadata {
                            served_by_node: stream_node_name.clone(),
                            backend_type: "local_llm".to_string(),
                            strategy_used: route.strategy.to_string(),
                            fallbacks_tried: 0,
                            hop_count: 0,
                            latency_ms: Some(stream_start.elapsed().as_secs_f64() * 1000.0),
                        };
                        return Ok(crate::routing::RouteResult {
                            response: wrapped,
                            metadata,
                        });
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
                                let wrapped = wrap_with_pii_streaming(
                                    stream,
                                    self.response_middleware.clone(),
                                );
                                let metadata = crate::routing::RouteMetadata {
                                    served_by_node: stream_node_name.clone(),
                                    backend_type: "quic_llm".to_string(),
                                    strategy_used: route.strategy.to_string(),
                                    fallbacks_tried: 0,
                                    hop_count: 0,
                                    latency_ms: Some(stream_start.elapsed().as_secs_f64() * 1000.0),
                                };
                                return Ok(crate::routing::RouteResult {
                                    response: wrapped,
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
                                let wrapped = wrap_with_pii_streaming(
                                    stream,
                                    self.response_middleware.clone(),
                                );
                                let metadata = crate::routing::RouteMetadata {
                                    served_by_node: node_id.clone(),
                                    backend_type: "mesh_forward_stream".to_string(),
                                    strategy_used: route.strategy.to_string(),
                                    fallbacks_tried: 0,
                                    hop_count: 1,
                                    latency_ms: Some(stream_start.elapsed().as_secs_f64() * 1000.0),
                                };
                                return Ok(crate::routing::RouteResult {
                                    response: wrapped,
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
            } // end for current_target
        }

        // HTTP backend streaming z PII filtering i memory store. Mirror
        // the dispatch loop above — try each target in order, take the
        // first one with a registered HTTP backend. Pre-R7.P1 this only
        // looked at `model_name` (= primary), losing the fallback path.
        let backend = route
            .targets
            .iter()
            .find_map(|t| self.select_http_backend(t))
            .ok_or_else(|| CoreError::ModelNotFound {
                model_name: model_name.clone(),
            })?;

        debug!("Wybrany backend streaming: {}", backend.url());

        let t_llm = std::time::Instant::now();
        let backend_stream = backend.chat_completion_stream(request).await?;

        // PII filter + EOF flush + finish-after-tail invariant — single
        // helper used by every streaming exit path.
        let filtered =
            wrap_with_pii_streaming(backend_stream, self.response_middleware.clone());

        // Metrics emit po EOF (przed Box::pin terminacja). Bez collected
        // response — telemetry tylko mierzy czas LLM.
        let metrics_tail = futures::stream::once(async move {
            let mut final_metrics = metrics;
            final_metrics.llm_inference_ms = Some(t_llm.elapsed().as_millis() as u64);
            info!("\n{}", final_metrics.format_table());
            Vec::<Result<ChatCompletionChunk>>::new()
        })
        .flat_map(futures::stream::iter);

        let stream = filtered.chain(metrics_tail);

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
