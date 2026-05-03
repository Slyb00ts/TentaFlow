// =============================================================================
// Plik: routing/streaming.rs
// Opis: Streaming SSE — route_chat_completion_stream, route_to_quic_llm_stream.
//       Audio input (STT + speaker ID), PII filtering w strumieniu, TTS
//       buffering.
// =============================================================================

use crate::api::openai::types::{
    ChatCompletionChunk, ChatCompletionRequest, ChunkChoice, Delta, MessageContent,
};
use crate::error::Result;
use crate::routing::chat::flow_result_to_chat_response;
use crate::routing::router::{RequestMetrics, Router};

use std::pin::Pin;
use tracing::{debug, warn};

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
                Err(e) => {
                    return Err(crate::routing::chat::executor_err_to_core(e, &request.model).into());
                }
            }
        }
        Err(crate::error::CoreError::InternalError {
            message: "router executor not wired (Router::new precondition)".to_string(),
            source: None,
        }
        .into())
    }
}
