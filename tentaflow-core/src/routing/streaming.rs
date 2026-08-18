// =============================================================================
// Plik: routing/streaming.rs
// Opis: Streaming SSE — route_chat_completion_stream, route_to_quic_llm_stream.
//       Audio input (STT + speaker ID), PII filtering w strumieniu, TTS
//       buffering.
// =============================================================================

use crate::api::openai::types::{
    ChatCompletionChunk, ChatCompletionRequest, FunctionCall, ToolCall,
};
use crate::compliance::ai_gateway::{AiEventHandle, AiGateway, AiGatewayContext};
use crate::error::Result;
use crate::routing::router::Router;

use std::pin::Pin;
use std::task::{Context, Poll};

use futures::Stream;

/// Wybór flow dla streamowanego chatu (`route_chat_completion_stream`).
#[derive(Debug, Clone)]
pub enum ChatFlowSelector {
    /// Standardowa rezolucja: czysty model / alias→model → bezpośrednie
    /// wykonanie na backendzie; alias→flow / flow published as model → flow
    /// engine (jawny flow albo — gdy brak — direct execution).
    Auto,
    /// Konkretny flow użytkownika po ID (wybrany w selektorze czatu).
    FlowId(String),
}

/// Bridge `StreamingExecution.stream` (CBOR `EnvelopeDelta::Llm`) na strumień
/// `ChatCompletionChunk` zgodny z OpenAI SSE. Outcome receiver z executor'a
/// jest spawnowany do background task'a (per plan: routing nie czeka na
/// outcome). Disconnect klienta propaguje się przez `CancelOnDropStream`
/// wstawiony przez SSE wrapper. PII cleaning siedzi w `pii_filter`
/// StreamingNodeAdapter wewnątrz flow_engine (Krok 6) — wire layer już
/// nie filtruje.
fn envelope_stream_to_chunk_stream(
    stream_exec: crate::flow_engine::executor::StreamingExecution,
    model: String,
    include_usage: bool,
) -> std::pin::Pin<
    Box<
        dyn futures::Stream<
                Item = crate::error::Result<crate::api::openai::types::ChatCompletionChunk>,
            > + Send,
    >,
> {
    use crate::flow_engine::envelope::EnvelopeDelta;
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
        let mapped = stream.filter_map(move |item| {
            futures::future::ready(match item {
                Ok(EnvelopeDelta::Llm(c)) => {
                    Some(Ok(make_chunk(&id_for_map, created, &model_for_map, c)))
                }
                // Voice flow invoked on the text-chat surface: its TTS audio
                // deltas are meaningless here — drop them and keep the text
                // stream alive instead of aborting mid-reply. Audio surfaces
                // (FlowInvoke / /v1/audio) consume the same flow's audio.
                Ok(EnvelopeDelta::Audio(_)) => None,
                Err(e) => Some(Err(crate::error::CoreError::InternalError {
                    message: format!("flow stream error: {e}"),
                    source: None,
                }
                .into())),
            })
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
                } => loop {
                    match stream.next().await {
                        Some(Ok(EnvelopeDelta::Llm(c))) => {
                            let chunk = make_chunk(&id, created, &model, c);
                            break Some((
                                Ok(chunk),
                                SplitState::Producing {
                                    stream,
                                    outcome,
                                    id,
                                    created,
                                    model,
                                },
                            ));
                        }
                        // Voice flow on the text-chat surface — TTS audio
                        // deltas are dropped, the text stream continues.
                        Some(Ok(EnvelopeDelta::Audio(_))) => continue,
                        Some(Err(e)) => {
                            break Some((
                                Err(crate::error::CoreError::InternalError {
                                    message: format!("flow stream error: {e}"),
                                    source: None,
                                }
                                .into()),
                                SplitState::Done,
                            ));
                        }
                        None => {
                            break match outcome.await {
                                Ok(o) => {
                                    let tail = build_flow_tail_chunk(&o, &id, created, &model);
                                    Some((Ok(tail), SplitState::Done))
                                }
                                Err(_) => {
                                    tracing::warn!(
                                        "flow finalizer dropped without outcome — no usage tail"
                                    );
                                    None
                                }
                            };
                        }
                    }
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

struct ComplianceAuditStream {
    inner: Pin<Box<dyn Stream<Item = Result<ChatCompletionChunk>> + Send>>,
    event: Option<AiEventHandle>,
    response_text: String,
    usage: Option<crate::api::openai::types::Usage>,
    tool_calls: Vec<ToolCall>,
    finished: bool,
    // Gdy compliance jest aktywne, strumień jest budowany z usage-tail NIEZALEŻNIE
    // od tego czy klient prosił o include_usage — token accounting (AiGateway
    // bump) musi widzieć realne usage także dla zwykłego streamingu z dashboardu.
    // Jeśli klient NIE prosił o usage, ten tail jest wewnętrzny: zbieramy z niego
    // usage, ale nie przepuszczamy go do klienta.
    emit_usage_to_client: bool,
}

impl ComplianceAuditStream {
    fn new(
        inner: Pin<Box<dyn Stream<Item = Result<ChatCompletionChunk>> + Send>>,
        event: AiEventHandle,
        emit_usage_to_client: bool,
    ) -> Self {
        Self {
            inner,
            event: Some(event),
            response_text: String::new(),
            usage: None,
            tool_calls: Vec::new(),
            finished: false,
            emit_usage_to_client,
        }
    }

    fn finish_success(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        if let Some(event) = self.event.take() {
            if let Err(error) = event.finish_stream_success(
                &self.response_text,
                self.usage.as_ref(),
                &self.tool_calls,
            ) {
                tracing::warn!(error = %error, "compliance AI audit stream finish failed");
            }
        }
    }

    fn finish_failed(&mut self, error_message: &str) {
        if self.finished {
            return;
        }
        self.finished = true;
        if let Some(event) = self.event.take() {
            if let Err(error) = event.finish_failed(error_message) {
                tracing::warn!(error = %error, "compliance AI audit stream failure capture failed");
            }
        }
    }
}

impl Stream for ComplianceAuditStream {
    type Item = Result<ChatCompletionChunk>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            match self.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(chunk))) => {
                    for choice in &chunk.choices {
                        if let Some(content) = choice.delta.content.as_deref() {
                            self.response_text.push_str(content);
                        }
                        if let Some(tool_calls) = choice.delta.tool_calls.as_ref() {
                            for delta in tool_calls {
                                absorb_tool_call_delta(&mut self.tool_calls, delta);
                            }
                        }
                    }
                    if let Some(usage) = chunk.usage.as_ref() {
                        self.usage = Some(usage.clone());
                        // Usage-tail wymuszony wewnętrznie dla token accountingu:
                        // gdy klient nie prosił o include_usage, nie przepuszczamy
                        // tego tail-chunku (nie niesie treści) dalej.
                        let content_free = chunk
                            .choices
                            .iter()
                            .all(|c| c.delta.content.is_none() && c.delta.tool_calls.is_none());
                        if !self.emit_usage_to_client && content_free {
                            continue;
                        }
                    }
                    return Poll::Ready(Some(Ok(chunk)));
                }
                Poll::Ready(Some(Err(error))) => {
                    let error_message = error.to_string();
                    self.finish_failed(&error_message);
                    return Poll::Ready(Some(Err(error)));
                }
                Poll::Ready(None) => {
                    self.finish_success();
                    return Poll::Ready(None);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl Drop for ComplianceAuditStream {
    fn drop(&mut self) {
        if !self.finished {
            self.finish_failed("stream dropped before completion");
        }
    }
}

/// Hard cap on tool-call slots reassembled from one stream. `delta.index`
/// comes from an untrusted backend (or a remote mesh node) — without a cap
/// a single forged chunk with a huge index would allocate slots until OOM.
const MAX_TOOL_CALL_SLOTS: usize = 256;

/// Reassembles streamed tool-call fragments into full `ToolCall`s for the
/// AI audit record: id/name arrive on the fragment that opens a slot,
/// argument text accumulates across fragments of the same `index`.
fn absorb_tool_call_delta(
    acc: &mut Vec<ToolCall>,
    delta: &crate::api::openai::types::ToolCallDelta,
) {
    let idx = delta.index as usize;
    if idx >= MAX_TOOL_CALL_SLOTS {
        tracing::warn!(
            index = idx,
            "tool-call delta index exceeds slot cap; fragment dropped from audit record"
        );
        return;
    }
    while acc.len() <= idx {
        acc.push(ToolCall {
            id: String::new(),
            tool_type: "function".to_string(),
            function: FunctionCall {
                name: String::new(),
                arguments: String::new(),
            },
        });
    }
    let slot = &mut acc[idx];
    if let Some(id) = &delta.id {
        slot.id = id.clone();
    }
    if let Some(function) = &delta.function {
        if let Some(name) = &function.name {
            slot.function.name.push_str(name);
        }
        if let Some(arguments) = &function.arguments {
            slot.function.arguments.push_str(arguments);
        }
    }
}

fn make_chunk(
    id: &str,
    created: u64,
    model: &str,
    c: crate::flow_engine::envelope::LlmStreamChunk,
) -> crate::api::openai::types::ChatCompletionChunk {
    use crate::api::openai::types::{
        ChatCompletionChunk, ChunkChoice, Delta, FunctionCallDelta, ToolCallDelta,
    };
    ChatCompletionChunk {
        id: id.to_string(),
        object: "chat.completion.chunk".to_string(),
        created,
        model: model.to_string(),
        choices: vec![ChunkChoice {
            // Stage 3d Krok 1: propagate choice_index z LlmStreamChunk
            // (zamiast hardcoded 0). Default 0 dla synthetic + większości
            // backendów; multi-choice n>1 dostaje per-choice value.
            index: c.choice_index,
            delta: Delta {
                role: None,
                content: if c.text_delta.is_empty() {
                    None
                } else {
                    Some(c.text_delta)
                },
                reasoning_content: c.reasoning_delta,
                tool_calls: if c.tool_calls.is_empty() {
                    None
                } else {
                    Some(
                        c.tool_calls
                            .into_iter()
                            .map(|tc| ToolCallDelta {
                                index: tc.index,
                                // "function" is stamped on the slot-opening
                                // fragment (the one carrying the id),
                                // mirroring OpenAI wire behaviour.
                                tool_type: tc.id.is_some().then(|| "function".to_string()),
                                id: tc.id,
                                function: if tc.function_name.is_none()
                                    && tc.arguments_delta.is_none()
                                {
                                    None
                                } else {
                                    Some(FunctionCallDelta {
                                        name: tc.function_name,
                                        arguments: tc.arguments_delta,
                                    })
                                },
                            })
                            .collect(),
                    )
                },
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
        // Metryki wydajnosci jada na finalnym chunku (trailer LlmStreamChunk
        // niesie perf razem z usage); regular delty maja None.
        perf: c.perf.map(|p| crate::api::openai::types::GenPerf {
            ttft_ms: p.ttft_ms,
            prefill_tps: p.prefill_tps,
            decode_tps: p.decode_tps,
            total_ms: p.total_ms,
        }),
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
        perf: outcome.perf.map(|p| crate::api::openai::types::GenPerf {
            ttft_ms: p.ttft_ms,
            prefill_tps: p.prefill_tps,
            decode_tps: p.decode_tps,
            total_ms: p.total_ms,
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
///
/// Po Universal Flow Gateway (stage 3d) production path nie używa już tego
/// helpera — usage tail jest budowany w `envelope_stream_to_chunk_stream`
/// bezpośrednio z `FlowExecutionOutcome.usage`. Helper żyje wyłącznie pod
/// `cfg(test)` jako oracle dla testów include_usage semantyki — kasowanie
/// całości razem z testami zostawiłoby bez pokrycia.
#[cfg(test)]
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
            Box<dyn futures::Stream<Item = crate::error::Result<ChatCompletionChunk>> + Send>,
        >;

    let composite = futures::stream::unfold(
        UsageSplitState::Active {
            inner,
            include_usage,
        },
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
                            UsageSplitState::Active {
                                inner,
                                include_usage,
                            },
                        ));
                    }
                    // Chunk niesie usage. Decyzja per flag.
                    if !include_usage {
                        // Klient nie prosił — strip usage, emit chunk.
                        let mut stripped = next;
                        stripped.usage = None;
                        return Some((
                            Ok(stripped),
                            UsageSplitState::Active {
                                inner,
                                include_usage,
                            },
                        ));
                    }
                    // include_usage=true: split na finish chunk + tail. Perf
                    // jedzie razem z usage na tailu (oba sa metrykami finalnymi),
                    // wiec przenosimy je z finish chunka tak samo jak usage.
                    let metrics = next.usage.clone();
                    let perf = next.perf.clone();
                    let mut finish_chunk = next;
                    finish_chunk.usage = None;
                    finish_chunk.perf = None;
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
                        perf,
                    };
                    Some((
                        Ok(finish_chunk),
                        UsageSplitState::EmitTail {
                            tail,
                            inner,
                            include_usage,
                        },
                    ))
                }
                UsageSplitState::EmitTail {
                    tail,
                    inner,
                    include_usage,
                } => Some((
                    Ok(tail),
                    UsageSplitState::Active {
                        inner,
                        include_usage,
                    },
                )),
                UsageSplitState::Done => None,
            }
        },
    );
    Box::pin(composite)
}

#[cfg(test)]
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

impl Router {
    /// Routuje chat completion request (STREAMING MODE). Czysty model /
    /// alias→model streamuje wprost z backendu (bez flow, bez PII buforowania).
    /// Flow published as a model / alias→flow idzie przez flow_engine: jawny
    /// user-defined flow, a gdy go brak — model wykonywany BEZPOŚREDNIO na
    /// executorze (bez pii_filter). User-defined blocking-only flow jest
    /// opakowywany w single-chunk stream (wrapper sync→stream w
    /// FlowDispatcher::try_dispatch_streaming). PII cleaning istnieje wyłącznie
    /// jako opcjonalny `pii_filter` node w jawnym flow (PII jest opt-in).
    pub async fn route_chat_completion_stream(
        &self,
        request: ChatCompletionRequest,
        user: Option<crate::auth::acl::UserContext>,
        compliance_context: Option<AiGatewayContext>,
        flow_selector: ChatFlowSelector,
    ) -> Result<
        crate::routing::RouteResult<
            Pin<Box<dyn Stream<Item = Result<ChatCompletionChunk>> + Send>>,
        >,
    > {
        let stream_start = std::time::Instant::now();
        let stream_node_name = crate::mesh::node_info_collector::local_hostname();

        if let Some(ref u) = user {
            if let Some(ref db) = self.db {
                if !crate::auth::acl::check_access_safe(
                    db,
                    "model",
                    &request.model,
                    &u.user_id,
                    &u.role,
                ) {
                    tracing::warn!(
                        user_id = %u.user_id,
                        model = %request.model,
                        "ACL denied chat-stream model"
                    );
                    return Err(crate::error::CoreError::ModelNotFound {
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

        let compliance_event = if let Some(db) = self.db.as_ref() {
            let gateway = AiGateway::new(
                db.clone(),
                self.local_node_id(),
                crate::compliance::ai_gateway::token_quota_enabled(),
            );
            Some(
                gateway
                    .start_chat_event(&request, user.as_ref(), compliance_context.as_ref())
                    // Zachowaj realny błąd domenowy (np. RateLimitExceeded z limitu
                    // tokenów) — inaczej klient widzi mylące „błąd wewnętrzny".
                    .map_err(|e| match e.downcast::<crate::error::CoreError>() {
                        Ok(core) => core,
                        Err(e) => crate::error::CoreError::InternalError {
                            message: "compliance AI audit stream start failed".to_string(),
                            source: Some(e),
                        },
                    })?,
            )
        } else {
            None
        };

        // === DIRECT MODEL: raw model → backend stream bez flow ===
        // A plain model name (or an alias resolving to one) streams straight from
        // the backend (no flow, no PII sentence-buffering → token-level TTFT).
        // Only a Flow published as a model (or an alias resolving to one) takes
        // the flow-engine path below. An explicit FlowId selector (a picked flow)
        // always goes through the flow engine.
        if matches!(flow_selector, ChatFlowSelector::Auto)
            && !crate::routing::chat::model_resolves_to_flow(
                &self.catalog_snapshot(),
                &request.model,
            )
        {
            let executor = match self.executor() {
                Some(e) => e,
                None => {
                    if let Some(event) = compliance_event.as_ref() {
                        let _ = event.finish_failed("runtime_executor_not_wired");
                    }
                    return Err(crate::error::CoreError::InternalError {
                        message: "runtime executor not wired — direct model stream unavailable"
                            .to_string(),
                        source: None,
                    }
                    .into());
                }
            };
            let include_usage = request
                .stream_options
                .as_ref()
                .map(|so| so.include_usage)
                .unwrap_or(false);
            // Force a usage tail whenever token accounting must be audited even
            // if the client did not ask; ComplianceAuditStream strips that
            // content-free tail from client output when include_usage=false.
            let need_usage = include_usage || compliance_event.is_some();
            let mut direct_req = request.clone();
            if need_usage {
                direct_req.stream_options = Some(crate::api::openai::types::StreamOptions {
                    include_usage: true,
                });
            }
            let mut exec_ctx =
                crate::services::runtime::context::ExecutionContext::new(user.clone());
            match executor.stream_chat(direct_req, &mut exec_ctx).await {
                Ok(stream) => {
                    let filtered: Pin<Box<dyn Stream<Item = Result<ChatCompletionChunk>> + Send>> =
                        if let Some(event) = compliance_event {
                            Box::pin(ComplianceAuditStream::new(stream, event, include_usage))
                        } else {
                            stream
                        };
                    let metadata = crate::routing::RouteMetadata {
                        served_by_node: exec_ctx
                            .route_metadata
                            .served_by_node
                            .clone()
                            .unwrap_or_else(|| stream_node_name.clone()),
                        backend_type: "direct_stream".to_string(),
                        strategy_used: "direct".to_string(),
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
                    if let Some(event) = compliance_event.as_ref() {
                        let _ = event.finish_failed(&e.to_string());
                    }
                    return Err(
                        crate::routing::chat::executor_error_to_core(e, &request.model).into(),
                    );
                }
            }
        }

        // === FLOW ENGINE: proba wykonania przez konfigurowalny flow ===
        if let Some(ref dispatcher) = self.flow_dispatcher {
            let blobs = dispatcher.blobs();
            // Najpierw streamowa sciezka — tylko gdy flow ma edge from_port="stream".
            let (mut initial_stream, meta_stream) =
                crate::routing::build_initial_envelope_for_user(&request, user.clone(), &blobs)
                    .await?;
            // §3.4: seed the turn's correlation key with the session event's
            // request_id so per-call `llm` events in the flow link to this row.
            if let Some(handle) = compliance_event.as_ref() {
                initial_stream.meta.insert(
                    "correlation_id".into(),
                    serde_json::Value::String(handle.request_id().to_string()),
                );
            }
            // Disconnect bridge: ten sam cancel_token co w meta dostaje
            // CancelOnDropStream poniżej, więc gdy hyper droppuje SSE body
            // (klient się rozłączył), token zostaje cancelled i finalizer
            // executor'a zauważa to przez biased select! (R7 plan).
            let stream_cancel = meta_stream.cancel_token.clone();
            let dispatch_result = match &flow_selector {
                ChatFlowSelector::Auto => {
                    dispatcher
                        .try_dispatch_streaming(&request.model, "chat", initial_stream, meta_stream)
                        .await
                }
                ChatFlowSelector::FlowId(flow_id) => {
                    dispatcher
                        .dispatch_by_flow_id_streaming(flow_id.clone(), initial_stream, meta_stream)
                        .await
                }
            };
            match dispatch_result {
                Ok(stream_exec) => {
                    let model_for_stream = request.model.clone();
                    let include_usage = request
                        .stream_options
                        .as_ref()
                        .map(|so| so.include_usage)
                        .unwrap_or(false);
                    // Token accounting (AiGateway bump) potrzebuje realnego usage
                    // także gdy klient nie prosił o include_usage. Gdy compliance
                    // jest aktywne, wymuszamy wewnętrzny usage-tail; ComplianceAuditStream
                    // go zbierze i — jeśli klient nie prosił — nie przepuszcza dalej.
                    let need_usage = include_usage || compliance_event.is_some();
                    let chunk_stream =
                        envelope_stream_to_chunk_stream(stream_exec, model_for_stream, need_usage);
                    // PII cleaning istnieje wyłącznie jako opcjonalny
                    // `pii_filter` node w jawnym user-defined flow (PII jest
                    // opt-in); direct execution i wire layer nie filtrują.
                    let filtered = chunk_stream;
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
                    let filtered: std::pin::Pin<
                        Box<
                            dyn futures::Stream<
                                    Item = crate::error::Result<
                                        crate::api::openai::types::ChatCompletionChunk,
                                    >,
                                > + Send,
                        >,
                    > = if let Some(event) = compliance_event {
                        Box::pin(ComplianceAuditStream::new(filtered, event, include_usage))
                    } else {
                        filtered
                    };
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
                Err(e) => {
                    if let Some(event) = compliance_event.as_ref() {
                        if let Err(audit_error) = event.finish_failed(&e.to_string()) {
                            tracing::warn!(
                                error = %audit_error,
                                "compliance AI audit stream failure capture failed"
                            );
                        }
                    }
                    return Err(crate::routing::dispatch_error_to_core(e, &request.model).into());
                }
            }
        }

        // Brak flow_dispatcher (DB-less router) i model klasyfikowany jako flow
        // → 500 (plain models obsłużone wyżej ścieżką direct stream).
        let _ = target_accepts_audio;
        let _ = stream_start;
        let _ = stream_node_name;
        if let Some(event) = compliance_event.as_ref() {
            if let Err(audit_error) = event.finish_failed("flow_dispatcher_not_wired") {
                tracing::warn!(
                    error = %audit_error,
                    "compliance AI audit stream failure capture failed"
                );
            }
        }
        Err(crate::error::CoreError::InternalError {
            message: "flow_dispatcher not wired (DB-less router) — chat streaming \
                      path requires Universal Flow Gateway"
                .to_string(),
            source: None,
        }
        .into())
    }
}

#[cfg(test)]
mod include_usage_tests {
    use super::*;
    use crate::api::openai::types::{ChatCompletionChunk, ChunkChoice, Delta, Usage};
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
                finish_reason: if finish { Some("stop".into()) } else { None },
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
            perf: None,
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
        assert!(
            c2.usage.is_none(),
            "usage stripped when include_usage=false"
        );
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
}

#[cfg(test)]
mod compliance_stream_tests {
    use super::*;
    use crate::api::openai::types::{
        ChatCompletionChunk, ChunkChoice, Delta, Message, MessageContent, Usage,
    };
    use crate::compliance::ai_gateway::AiGateway;
    use crate::db::migrations;
    use futures::StreamExt;
    use rusqlite::{params, Connection};
    use std::sync::Arc;

    fn db() -> crate::db::DbPool {
        let conn = Connection::open_in_memory().expect("baza testowa");
        migrations::run(&conn).expect("migracje");
        Arc::new(crate::db::Db::from_connection(conn))
    }

    fn chunk(text: &str, usage: Option<Usage>) -> ChatCompletionChunk {
        ChatCompletionChunk {
            id: "stream-1".to_string(),
            object: "chat.completion.chunk".to_string(),
            created: 1,
            model: "bielik".to_string(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: Delta {
                    role: None,
                    content: if text.is_empty() {
                        None
                    } else {
                        Some(text.to_string())
                    },
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
            usage,
            perf: None,
        }
    }

    #[tokio::test]
    async fn compliance_stream_zapisuje_scalona_odpowiedz() {
        let db = db();
        let gateway = AiGateway::new(db.clone(), "node-test", true);
        let request = ChatCompletionRequest {
            modalities: None,
            audio: None,
            model: "bielik".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: Some(MessageContent::Text("Daj odpowiedź".to_string())),
                ..Default::default()
            }],
            temperature: None,
            max_tokens: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            stream: true,
            stream_options: None,
            user: None,
            response_format: None,
            tools: None,
            tool_choice: None,
            n: None,
            memory_options: None,
            audio_input: None,
        };
        let handle = gateway
            .start_chat_event(&request, None, None)
            .expect("start event");
        let event_id = handle.event_id().to_string();
        let usage = Usage {
            prompt_tokens: 4,
            completion_tokens: 6,
            total_tokens: 10,
        };
        let inner = futures::stream::iter(vec![
            Ok(chunk("pierwsza ", None)),
            Ok(chunk("druga", Some(usage))),
        ]);
        let mut stream = ComplianceAuditStream::new(Box::pin(inner), handle, true);
        while stream
            .next()
            .await
            .transpose()
            .expect("stream chunk")
            .is_some()
        {}

        let conn = db.read().expect("db lock");
        let response_payload: String = conn
            .query_row(
                "SELECT content_text FROM compliance_ai_payloads WHERE event_id = ?1 AND payload_kind = 'response'",
                params![&event_id],
                |row| row.get(0),
            )
            .expect("response payload");
        let status: String = conn
            .query_row(
                "SELECT status FROM compliance_ai_events WHERE event_id = ?1",
                params![&event_id],
                |row| row.get(0),
            )
            .expect("event status");

        assert_eq!(response_payload, "pierwsza druga");
        assert_eq!(status, "success");
    }
}
