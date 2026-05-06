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
use crate::routing::chat::flow_outcome_to_chat_response;
use crate::routing::router::{RequestMetrics, Router};

use std::pin::Pin;
use tracing::{debug, warn};

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
/// Bridge `StreamingExecution.stream` (rkyv `EnvelopeDelta::Llm`) na strumień
/// `ChatCompletionChunk` zgodny z OpenAI SSE. Outcome receiver z executor'a
/// jest spawnowany do background task'a (per plan: routing nie czeka na
/// outcome). Disconnect klienta propaguje się przez `CancelOnDropStream`
/// wstawiony przez SSE wrapper.
fn envelope_stream_to_chunk_stream(
    stream_exec: crate::flow_engine::executor::StreamingExecution,
    model: String,
    include_usage: bool,
) -> std::pin::Pin<
    Box<
        dyn futures::Stream<Item = crate::error::Result<crate::api::openai::types::ChatCompletionChunk>>
            + Send,
    >,
> {
    use crate::api::openai::types::{ChatCompletionChunk, ChunkChoice, Delta, Usage};
    use crate::flow_engine::envelope::{EnvelopeDelta, FlowExecutionOutcome};
    use futures::StreamExt;

    let crate::flow_engine::executor::StreamingExecution { stream, outcome } = stream_exec;
    let id = format!("flow-{}", uuid::Uuid::new_v4());
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    if !include_usage {
        // Pre-Etap-3a path — detached log, brak tail chunk.
        tokio::spawn(async move {
            match outcome.await {
                Ok(o) => tracing::info!(
                    latency_ms = o.total_latency_ms,
                    prompt_tokens = o.usage.prompt_tokens,
                    completion_tokens = o.usage.completion_tokens,
                    error = ?o.error,
                    "flow streaming completed"
                ),
                Err(_) => tracing::warn!("flow finalizer dropped without outcome"),
            }
        });
        let id_for_map = id;
        let model_for_map = model;
        let mapped = stream.map(move |item| match item {
            Ok(EnvelopeDelta::Llm(c)) => Ok(make_chunk(&id_for_map, created, &model_for_map, c)),
            // Etap 3c: chat streaming bridge nigdy nie powinno dostać
            // Audio delta (audio leci przez /v1/audio/speech/stream
            // endpoint, NIE chat stream). Defensywnie mapujemy na error.
            Ok(EnvelopeDelta::Audio(_)) => Err(crate::error::CoreError::InternalError {
                message: "chat stream received Audio delta — flow misconfig".into(),
                source: None,
            }
            .into()),
            Err(e) => Err(crate::error::CoreError::InternalError {
                message: format!("flow stream error: {e}"),
                source: None,
            }
            .into()),
        });
        return Box::pin(mapped);
    }

    // include_usage=true: po stream EOF awaiting outcome, emit tail chunk z usage
    // przed `[DONE]`. State machine pilnuje że tail leci dopiero raz, po EOF.
    let composite = futures::stream::unfold(
        SplitState::Producing {
            stream,
            outcome,
            id,
            created,
            model,
        },
        move |state| async move {
            match state {
                SplitState::Producing {
                    mut stream,
                    outcome,
                    id,
                    created,
                    model,
                } => match stream.next().await {
                    Some(Ok(EnvelopeDelta::Llm(c))) => {
                        let chunk = make_chunk(&id, created, &model, c);
                        Some((
                            Ok(chunk),
                            SplitState::Producing {
                                stream,
                                outcome,
                                id,
                                created,
                                model,
                            },
                        ))
                    }
                    Some(Ok(EnvelopeDelta::Audio(_))) => Some((
                        Err(crate::error::CoreError::InternalError {
                            message: "chat stream received Audio delta — flow misconfig".into(),
                            source: None,
                        }
                        .into()),
                        SplitState::Done,
                    )),
                    Some(Err(e)) => Some((
                        Err(crate::error::CoreError::InternalError {
                            message: format!("flow stream error: {e}"),
                            source: None,
                        }
                        .into()),
                        SplitState::Done,
                    )),
                    None => match outcome.await {
                        Ok(o) => {
                            let tail = build_flow_tail_chunk(&o, &id, created, &model);
                            Some((Ok(tail), SplitState::Done))
                        }
                        Err(_) => {
                            tracing::warn!("flow finalizer dropped without outcome — no usage tail");
                            None
                        }
                    },
                },
                SplitState::Done => None,
            }
        },
    );
    Box::pin(composite)
}

enum SplitState {
    Producing {
        stream: futures::stream::BoxStream<
            'static,
            crate::error::Result<crate::flow_engine::envelope::EnvelopeDelta>,
        >,
        outcome: tokio::sync::oneshot::Receiver<crate::flow_engine::envelope::FlowExecutionOutcome>,
        id: String,
        created: u64,
        model: String,
    },
    Done,
}

fn make_chunk(
    id: &str,
    created: u64,
    model: &str,
    c: crate::flow_engine::envelope::LlmStreamChunk,
) -> crate::api::openai::types::ChatCompletionChunk {
    use crate::api::openai::types::{ChatCompletionChunk, ChunkChoice, Delta};
    ChatCompletionChunk {
        id: id.to_string(),
        object: "chat.completion.chunk".to_string(),
        created,
        model: model.to_string(),
        choices: vec![ChunkChoice {
            index: 0,
            delta: Delta {
                role: None,
                content: if c.text_delta.is_empty() {
                    None
                } else {
                    Some(c.text_delta)
                },
                reasoning_content: c.reasoning_delta,
                tool_calls: None,
            },
            finish_reason: c
                .finish_reason
                .and_then(|f| f.as_openai_str().map(|s| s.to_string())),
            logprobs: None,
        }],
        system_fingerprint: None,
        audio: None,
        detected_intent: None,
        detected_tools: None,
        transcribed_text: None,
        speaker_id: None,
        speaker_name: None,
        usage: None,
    }
}

fn build_flow_tail_chunk(
    outcome: &crate::flow_engine::envelope::FlowExecutionOutcome,
    id: &str,
    created: u64,
    model: &str,
) -> crate::api::openai::types::ChatCompletionChunk {
    use crate::api::openai::types::{ChatCompletionChunk, Usage};
    ChatCompletionChunk {
        id: id.to_string(),
        object: "chat.completion.chunk".to_string(),
        created,
        model: model.to_string(),
        choices: vec![],
        system_fingerprint: None,
        audio: None,
        detected_intent: None,
        detected_tools: None,
        transcribed_text: None,
        speaker_id: None,
        speaker_name: None,
        usage: Some(Usage {
            prompt_tokens: outcome.usage.prompt_tokens as u32,
            completion_tokens: outcome.usage.completion_tokens as u32,
            total_tokens: outcome.usage.total_tokens as u32,
        }),
    }
}

/// Etap 3a: state machine która patrzy na `chunk.usage` (stemplowane przez
/// executor.rs::Done arm gdy backend dostarczył DetailedMetrics::Completion).
/// Decyduje per `include_usage` jak wykorzystać:
/// - `false` (default, back-compat): strip `usage` z chunk'u przed wireem.
///   Klient nie prosił, pole nigdy się nie pokazuje.
/// - `true`: emit chunk z `usage: None` (regular finish chunk per OpenAI
///   contract), POTEM emit dodatkowy tail chunk z `choices: []` + `usage`.
///   Dwa chunki z jednego źródłowego (OpenAI requirement).
///
/// Wszystkie chunki bez `usage` (regularne content delta) przepuszczane bez
/// modyfikacji.
fn apply_include_usage_split<S>(
    inner: S,
    include_usage: bool,
) -> std::pin::Pin<
    Box<
        dyn futures::Stream<
                Item = crate::error::Result<crate::api::openai::types::ChatCompletionChunk>,
            > + Send,
    >,
>
where
    S: futures::Stream<Item = crate::error::Result<crate::api::openai::types::ChatCompletionChunk>>
        + Send
        + 'static,
{
    use crate::api::openai::types::ChatCompletionChunk;
    use futures::StreamExt;

    let inner = Box::pin(inner)
        as std::pin::Pin<
            Box<
                dyn futures::Stream<
                        Item = crate::error::Result<ChatCompletionChunk>,
                    > + Send,
            >,
        >;

    let composite = futures::stream::unfold(
        UsageSplitState::Active { inner, include_usage },
        |state| async move {
            match state {
                UsageSplitState::Active {
                    mut inner,
                    include_usage,
                } => {
                    let next = match inner.next().await {
                        Some(Ok(c)) => c,
                        Some(Err(e)) => return Some((Err(e), UsageSplitState::Done)),
                        None => return None,
                    };
                    if next.usage.is_none() {
                        // Regular chunk — przepuszczamy bez zmian.
                        return Some((
                            Ok(next),
                            UsageSplitState::Active { inner, include_usage },
                        ));
                    }
                    // Chunk niesie usage. Decyzja per flag.
                    if !include_usage {
                        // Klient nie prosił — strip usage, emit chunk.
                        let mut stripped = next;
                        stripped.usage = None;
                        return Some((
                            Ok(stripped),
                            UsageSplitState::Active { inner, include_usage },
                        ));
                    }
                    // include_usage=true: split na finish chunk + tail.
                    let metrics = next.usage.clone();
                    let mut finish_chunk = next;
                    finish_chunk.usage = None;
                    let tail = ChatCompletionChunk {
                        id: finish_chunk.id.clone(),
                        object: "chat.completion.chunk".to_string(),
                        created: finish_chunk.created,
                        model: finish_chunk.model.clone(),
                        choices: vec![],
                        system_fingerprint: None,
                        audio: None,
                        detected_intent: None,
                        detected_tools: None,
                        transcribed_text: None,
                        speaker_id: None,
                        speaker_name: None,
                        usage: metrics,
                    };
                    Some((
                        Ok(finish_chunk),
                        UsageSplitState::EmitTail { tail, inner, include_usage },
                    ))
                }
                UsageSplitState::EmitTail {
                    tail,
                    inner,
                    include_usage,
                } => Some((
                    Ok(tail),
                    UsageSplitState::Active { inner, include_usage },
                )),
                UsageSplitState::Done => None,
            }
        },
    );
    Box::pin(composite)
}

enum UsageSplitState {
    Active {
        inner: std::pin::Pin<
            Box<
                dyn futures::Stream<
                        Item = crate::error::Result<crate::api::openai::types::ChatCompletionChunk>,
                    > + Send,
            >,
        >,
        include_usage: bool,
    },
    EmitTail {
        tail: crate::api::openai::types::ChatCompletionChunk,
        inner: std::pin::Pin<
            Box<
                dyn futures::Stream<
                        Item = crate::error::Result<crate::api::openai::types::ChatCompletionChunk>,
                    > + Send,
            >,
        >,
        include_usage: bool,
    },
    Done,
}

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
                // Etap 3a: tail chunk z usage (choices.is_empty() && usage.is_some())
                // przepuszczamy untouched — brak tekstu do filtrowania, OpenAI
                // contract wymaga że tail leci PRZED [DONE] w czystej formie.
                if chunk.choices.is_empty() && chunk.usage.is_some() {
                    return futures::stream::iter(vec![Ok(chunk)]);
                }
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
            let blobs = dispatcher.blobs();
            // Najpierw streamowa sciezka — tylko gdy flow ma edge from_port="stream".
            let (initial_stream, meta_stream) =
                crate::routing::build_initial_envelope_for_user(
                    &request,
                    user.clone(),
                    &blobs,
                )
                .await?;
            // Disconnect bridge: ten sam cancel_token co w meta dostaje
            // CancelOnDropStream poniżej, więc gdy hyper droppuje SSE body
            // (klient się rozłączył), token zostaje cancelled i finalizer
            // executor'a zauważa to przez biased select! (R7 plan).
            let stream_cancel = meta_stream.cancel_token.clone();
            match dispatcher
                .try_dispatch_streaming(&request.model, "chat", initial_stream, meta_stream)
                .await
            {
                Ok(Some(stream_exec)) => {
                    let model_for_stream = request.model.clone();
                    let include_usage = request
                        .stream_options
                        .as_ref()
                        .map(|so| so.include_usage)
                        .unwrap_or(false);
                    let chunk_stream = envelope_stream_to_chunk_stream(
                        stream_exec,
                        model_for_stream,
                        include_usage,
                    );
                    let filtered =
                        wrap_with_pii_streaming(chunk_stream, self.response_middleware.clone());
                    let cancel_wrapped: std::pin::Pin<
                        Box<
                            dyn futures::Stream<
                                    Item = crate::error::Result<
                                        crate::api::openai::types::ChatCompletionChunk,
                                    >,
                                > + Send,
                        >,
                    > = Box::pin(crate::flow_engine::cancel_on_drop::CancelOnDropStream::new(
                        filtered,
                        stream_cancel,
                    ));
                    let filtered = cancel_wrapped;
                    let metadata = crate::routing::RouteMetadata {
                        served_by_node: stream_node_name.clone(),
                        backend_type: "flow_engine_stream".to_string(),
                        strategy_used: "direct".to_string(),
                        fallbacks_tried: 0,
                        hop_count: 0,
                        latency_ms: Some(stream_start.elapsed().as_secs_f64() * 1000.0),
                    usage: None,
                    finish_reason: None,
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

            let (initial, meta) =
                crate::routing::build_initial_envelope_for_user(
                    &request,
                    user.clone(),
                    &blobs,
                )
                .await?;
            match dispatcher
                .try_dispatch(&request.model, "chat", initial, meta)
                .await
            {
                Ok(Some(outcome)) => {
                    let response = flow_outcome_to_chat_response(outcome, &request.model);
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
                    usage: None,
                    };

                    let stream = futures::stream::once(async move { Ok(chunk) });
                    let metadata = crate::routing::RouteMetadata {
                        served_by_node: stream_node_name.clone(),
                        backend_type: "flow_engine".to_string(),
                        strategy_used: "direct".to_string(),
                        fallbacks_tried: 0,
                        hop_count: 0,
                        latency_ms: Some(stream_start.elapsed().as_secs_f64() * 1000.0),
                    usage: None,
                    finish_reason: None,
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
                    // Etap 3a: split usage off finish chunk per
                    // `stream_options.include_usage`. Executor stempluje
                    // `chunk.usage = Some(metrics)` na finish chunk gdy
                    // backend zaraportował tokeny; routing layer decyduje
                    // jak to wykorzystać (strip back-compat lub split na tail).
                    let include_usage = request
                        .stream_options
                        .as_ref()
                        .map(|so| so.include_usage)
                        .unwrap_or(false);
                    let usage_aware = apply_include_usage_split(stream, include_usage);
                    // Codex H2 + H3 round 2: PII filter + EOF flush — wspolny
                    // helper dla executor + flow streaming paths.
                    let filtered = wrap_with_pii_streaming(
                        usage_aware,
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
                    usage: None,
                    finish_reason: None,
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

#[cfg(test)]
mod include_usage_tests {
    use super::*;
    use crate::api::openai::types::{
        ChatCompletionChunk, ChunkChoice, Delta, Usage,
    };
    use futures::StreamExt;

    fn chunk_with_usage(text: &str, finish: bool, usage: Option<Usage>) -> ChatCompletionChunk {
        ChatCompletionChunk {
            id: "id1".into(),
            object: "chat.completion.chunk".into(),
            created: 0,
            model: "m".into(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: Delta {
                    role: None,
                    content: Some(text.into()),
                    reasoning_content: None,
                    tool_calls: None,
                },
                finish_reason: if finish {
                    Some("stop".into())
                } else {
                    None
                },
                logprobs: None,
            }],
            system_fingerprint: None,
            audio: None,
            detected_intent: None,
            detected_tools: None,
            transcribed_text: None,
            speaker_id: None,
            speaker_name: None,
            usage,
        }
    }

    /// include_usage=false strips usage z finish chunk'u (back compat).
    #[tokio::test]
    async fn split_false_strips_usage_from_finish_chunk() {
        let usage = Some(Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        });
        let chunks = vec![
            Ok(chunk_with_usage("hello", false, None)),
            Ok(chunk_with_usage("", true, usage)),
        ];
        let inner = futures::stream::iter(chunks);
        let mut out = apply_include_usage_split(inner, false);
        let c1 = out.next().await.unwrap().unwrap();
        assert_eq!(c1.choices[0].delta.content.as_deref(), Some("hello"));
        assert!(c1.usage.is_none());
        let c2 = out.next().await.unwrap().unwrap();
        assert_eq!(c2.choices[0].finish_reason.as_deref(), Some("stop"));
        assert!(c2.usage.is_none(), "usage stripped when include_usage=false");
        assert!(out.next().await.is_none());
    }

    /// include_usage=true splits finish chunk na regular finish + dodatkowy tail.
    #[tokio::test]
    async fn split_true_emits_tail_chunk_with_usage() {
        let usage = Some(Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        });
        let chunks = vec![
            Ok(chunk_with_usage("hi", false, None)),
            Ok(chunk_with_usage("", true, usage.clone())),
        ];
        let inner = futures::stream::iter(chunks);
        let mut out = apply_include_usage_split(inner, true);
        // 1: regular content
        let c1 = out.next().await.unwrap().unwrap();
        assert_eq!(c1.choices[0].delta.content.as_deref(), Some("hi"));
        assert!(c1.usage.is_none());
        // 2: finish chunk z usage=None (split)
        let c2 = out.next().await.unwrap().unwrap();
        assert_eq!(c2.choices[0].finish_reason.as_deref(), Some("stop"));
        assert!(c2.usage.is_none());
        // 3: tail chunk z choices:[] + usage:Some
        let c3 = out.next().await.unwrap().unwrap();
        assert!(c3.choices.is_empty());
        assert_eq!(c3.usage.as_ref().unwrap().total_tokens, 15);
        assert!(out.next().await.is_none());
    }

    /// Brak usage na chunkach = wszystkie przepuszczone bez zmian.
    #[tokio::test]
    async fn split_passthrough_when_no_usage_stamped() {
        let chunks = vec![
            Ok(chunk_with_usage("a", false, None)),
            Ok(chunk_with_usage("", true, None)),
        ];
        let inner = futures::stream::iter(chunks);
        let collected: Vec<_> = apply_include_usage_split(inner, true)
            .collect::<Vec<_>>()
            .await;
        assert_eq!(collected.len(), 2);
    }

    /// PII filter passes chunks z choices.is_empty()+usage.is_some() untouched.
    #[tokio::test]
    async fn pii_filter_bypasses_tail_chunk() {
        use crate::middleware::response::ResponseMiddleware;
        // PII middleware z disabled flag — wystarcza dla testu bypass'u tail.
        let rm = std::sync::Arc::new(ResponseMiddleware::new(false));
        let usage = Some(Usage {
            prompt_tokens: 1,
            completion_tokens: 2,
            total_tokens: 3,
        });
        let mut tail = chunk_with_usage("", true, usage);
        tail.choices = vec![]; // proper tail shape
        let chunks = vec![Ok(tail.clone())];
        let inner = futures::stream::iter(chunks);
        let mut out = wrap_with_pii_streaming(Box::pin(inner), rm);
        let c = out.next().await.unwrap().unwrap();
        assert!(c.choices.is_empty());
        assert!(c.usage.is_some());
    }
}
