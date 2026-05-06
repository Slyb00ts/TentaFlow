// =============================================================================
// File: services/runtime/executor.rs
// Unified dispatch front-end. Walks the catalog through `AliasResolver`,
// permutes the candidate list with `strategy::rank`, and tries each
// candidate until one succeeds. The actual transport call is dispatched
// per `ResolvedExecutionTarget` variant.
//
// Scope today is chat (blocking) end-to-end for embedded / HTTP / flow
// targets. QUIC sidecar and mesh forward currently surface
// `TransportPendingCutover` so callers get a clear, typed error rather
// than a silent fallback to a transport that has not been wired up yet.
// =============================================================================

use std::sync::Arc;

use thiserror::Error;

use std::pin::Pin;

use futures::Stream;

use crate::api::openai::types::{
    ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse, EmbeddingData,
    EmbeddingInput, EmbeddingRequest, EmbeddingResponse, EmbeddingUsage, TTSRequest,
    TranscriptionRequest, TranscriptionResponse,
};
use crate::error::Result as CoreResult;
use crate::flow_engine::dispatcher::FlowDispatcher;
use crate::services::catalog::{CatalogProvider, InputModality, OutputModality, ServiceSurface};
use crate::services::handles_cache::BackendHandle;
use crate::services::runtime::context::ExecutionContext;
use crate::services::runtime::middleware::StreamMiddlewareFactory;
use crate::services::runtime::resolver::{AliasResolver, ResolveError, ResolveRequest};
use crate::services::runtime::strategy::{rank, StrategyState};
use crate::services::runtime::target::ResolvedExecutionTarget;

/// Strumien chunkow zwracany przez `stream_chat`. Boxed `Pin<Box<dyn Stream>>`
/// zeby caller mog go zapakowac w SSE bez wiedzy o konkretnym typie strumienia
/// (kazdy backend transport produkuje inny typ wewnetrznie).
pub type ExecutorChunkStream =
    Pin<Box<dyn Stream<Item = CoreResult<ChatCompletionChunk>> + Send>>;

/// Errors visible to the caller. Every variant maps onto a user-facing
/// outcome — `model_capability_unsupported` from the resolver, transport
/// failures with the failing target's tag for diagnostics.
#[derive(Debug, Error)]
pub enum ExecutorError {
    #[error(transparent)]
    Resolve(#[from] ResolveError),
    #[error("dispatch failed for {target_kind} target ({attempts} attempts): {last_error}")]
    AllCandidatesFailed {
        target_kind: &'static str,
        attempts: usize,
        last_error: String,
    },
    /// Resolver picked a transport that the executor cannot dispatch yet
    /// (QUIC sidecar / mesh forward). Distinct from a transient transport
    /// failure so the caller can surface a deploy-blocking message instead
    /// of pretending the next candidate might fix it.
    #[error("transport '{0}' is not routed through the runtime executor in this build")]
    TransportPendingCutover(&'static str),
    /// Resolver picked a Flow target but the dispatcher is not configured
    /// (DB-less router used by some test harnesses). This is a fatal
    /// config issue, not a transient failure — surface it directly so the
    /// fallback chain doesn't bury the real cause.
    #[error("flow dispatcher is not configured (DB-less router?)")]
    FlowDispatcherUnavailable,
    #[error("flow engine returned no result for model='{model}'")]
    FlowEmptyResult { model: String },
    /// `SttRuntime` is not wired yet (DB-less router / `Router::start`
    /// has not run). The caller should fall back to the legacy STT path.
    #[error("STT runtime is not wired yet")]
    SttRuntimeUnavailable,
    /// Real STT dispatch error from the runtime (engine failure, alias
    /// missing, etc.). The caller should NOT re-dispatch — surface this
    /// directly. Mirrors the chat/embeddings/TTS pattern of returning
    /// typed errors so HTTP layer maps them onto the right status code.
    #[error("STT backend error: {0}")]
    SttBackend(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl ExecutorError {
    /// Should the caller stop iterating fallback candidates after seeing
    /// this error? `true` for config-level failures that the next
    /// candidate cannot fix. Transient transport failures (HTTP 5xx,
    /// QUIC reconnect) keep iterating.
    ///
    /// `TransportPendingCutover` is **not** classified as abort — see
    /// `defer_transport_pending_cutover` for the dispatch loop's special
    /// handling. The variant must reach the caller so chat.rs/embeddings.rs
    /// can route through the legacy dispatch path, but only after every
    /// later candidate (HTTP/Local) has been tried.
    fn aborts_fallback_chain(&self) -> bool {
        matches!(
            self,
            Self::FlowDispatcherUnavailable
                | Self::SttRuntimeUnavailable
                | Self::SttBackend(_)
        )
    }
}

/// Top-level orchestrator. Holds Arc references to every collaborator;
/// no state of its own beyond a per-alias `StrategyState` map. The
/// resolver already owns `LiveHandlesCache` for hydrating Local
/// candidates — executor doesn't need a second handle here.
pub struct ModelRuntimeExecutor {
    catalog: Arc<CatalogProvider>,
    resolver: Arc<AliasResolver>,
    flow_dispatcher: Option<Arc<FlowDispatcher>>,
    local_inference: Arc<crate::inference::local::LocalInferenceHandler>,
    /// SttRuntime slot — same `Arc<RwLock<Option<...>>>` as `Router.stt_runtime`
    /// and `FlowDispatcher`'s SttRuntimeSlot. Shared instance so the
    /// `/v1/audio/transcriptions` handler, the flow STT adapter, and
    /// `executor.execute_stt` all dispatch through the same owner (D.3
    /// single STT path). `None` until `Router::start` plants the runtime.
    stt_runtime: Arc<parking_lot::RwLock<Option<Arc<crate::services::stt::SttRuntime>>>>,
    /// Mesh transport slot — same `Arc<RwLock<Option<...>>>` as
    /// `Router.mesh_manager`. Wired by `Router::start` once the iroh
    /// endpoint is up. Used by R3b.7 to dispatch `MeshForward` candidates
    /// directly through the executor instead of returning
    /// `TransportPendingCutover` and falling back to legacy router code.
    /// `None` for DB-less / no-mesh routers; the dispatcher returns
    /// `TransportPendingCutover` so the caller can pick the next
    /// candidate or take the legacy fallback.
    mesh_manager: Arc<
        parking_lot::RwLock<Option<Arc<crate::mesh::iroh_manager::IrohMeshManager>>>,
    >,
    middleware: Vec<Arc<dyn StreamMiddlewareFactory>>,
    /// Per-alias round-robin state keyed by alias name. `DashMap` so we
    /// can mutate per-key without serialising the whole map.
    strategy_state: Arc<dashmap::DashMap<String, Arc<StrategyState>>>,
}

impl ModelRuntimeExecutor {
    pub fn new(
        catalog: Arc<CatalogProvider>,
        resolver: Arc<AliasResolver>,
        flow_dispatcher: Option<Arc<FlowDispatcher>>,
        local_inference: Arc<crate::inference::local::LocalInferenceHandler>,
        stt_runtime: Arc<parking_lot::RwLock<Option<Arc<crate::services::stt::SttRuntime>>>>,
        mesh_manager: Arc<
            parking_lot::RwLock<Option<Arc<crate::mesh::iroh_manager::IrohMeshManager>>>,
        >,
        middleware: Vec<Arc<dyn StreamMiddlewareFactory>>,
    ) -> Self {
        Self {
            catalog,
            resolver,
            flow_dispatcher,
            local_inference,
            stt_runtime,
            mesh_manager,
            middleware,
            strategy_state: Arc::new(dashmap::DashMap::new()),
        }
    }

    /// Non-streaming chat completion. Resolves the requested model into a
    /// candidate list, ranks per alias strategy, and tries candidates in
    /// order. First success wins; aggregate failure surfaces the last
    /// transport error so the caller knows what went wrong on the way.
    ///
    /// **ACL is the caller's responsibility.** This function does not
    /// inspect `ctx.user` against the requested model — handlers must
    /// gate access (model-level + per-flow ACL) before building the
    /// `ChatCompletionRequest`. Bypassing the handler-side check lets a
    /// user reach any model named in the catalog, which is a regression
    /// against the unified-catalog ACL contract.
    pub async fn execute_chat(
        &self,
        request: ChatCompletionRequest,
        ctx: &mut ExecutionContext,
    ) -> Result<ChatCompletionResponse, ExecutorError> {
        let outcome = {
            let snapshot = self.catalog.snapshot();
            let req = self.build_chat_resolve_request(&request);
            self.resolver.resolve(&req, &snapshot, ctx)?
        };

        let state = self.strategy_state_for(&request.model);
        let ranked = rank(&outcome.candidates, outcome.strategy, &state);

        let mut last_err: Option<String> = None;
        let mut attempts = 0usize;
        let mut last_kind: &'static str = "unknown";
        let mut deferred_cutover: Option<&'static str> = None;

        for target in ranked {
            attempts += 1;
            last_kind = target.telemetry_tag();
            match self.dispatch_chat_blocking(&target, request.clone(), ctx).await {
                Ok(response) => {
                    ctx.route_metadata.served_by_node = served_by(&target);
                    ctx.route_metadata.backend_type = Some(target.telemetry_tag().to_string());
                    ctx.route_metadata.fallbacks_tried = (attempts - 1) as u32;
                    return Ok(response);
                }
                Err(e) if e.aborts_fallback_chain() => {
                    // Config-level failure: trying the next candidate
                    // cannot help. Surface the original error directly
                    // so the operator sees the actual cause instead of
                    // an aggregated `AllCandidatesFailed`.
                    return Err(e);
                }
                Err(ExecutorError::TransportPendingCutover(kind)) => {
                    // Codex R3b.1 round 2 M1: don't short-circuit — later
                    // candidates (HTTP/Local) might serve the request.
                    // Remember the cutover so we can surface it iff every
                    // other candidate fails.
                    deferred_cutover.get_or_insert(kind);
                }
                Err(e) => {
                    tracing::warn!(
                        target_kind = target.telemetry_tag(),
                        error = %e,
                        "chat dispatch failed; trying next candidate"
                    );
                    last_err = Some(e.to_string());
                }
            }
        }

        if let Some(kind) = deferred_cutover {
            return Err(ExecutorError::TransportPendingCutover(kind));
        }

        Err(ExecutorError::AllCandidatesFailed {
            target_kind: last_kind,
            attempts,
            last_error: last_err.unwrap_or_else(|| "no candidates after rank".into()),
        })
    }

    /// R3a streaming: streaming chat completion. Lustro `execute_chat` ale
    /// dispatch zwraca `Stream<ChatCompletionChunk>` zamiast jednego
    /// `ChatCompletionResponse`. MeshForward + middleware (PII, TTS) sa
    /// deferred do follow-up.
    ///
    /// **Fallback semantyka** (Codex M3): fallback miedzy kandydatami zachodzi
    /// wylacznie podczas KONSTRUKCJI streamu — `dispatch_chat_stream` zwraca
    /// `Result<ExecutorChunkStream>`. Bledy *pre-handoff* (transport reject,
    /// QUIC client missing, niewspierany backend) inicjuja kolejna proba.
    /// Jezeli stream zostal juz handoff'owany do callera (caller dostal
    /// `Ok(stream)`), kolejne wywolania `Stream::poll_next` ktore zwroca Err
    /// **NIE** powoduja retry — chunki z pierwszego backendu mogly juz
    /// dotrzec do klienta SSE i podmiana streamu w polowie zlamalaby
    /// kontrakt OpenAI API (chunki z dwoch zrodel zmieszane). To zgodne z
    /// planem v7 R1.5g "no fallback after first chunk" interpretowane jako
    /// "no fallback po zwroceniu Stream do SSE pipeline'u".
    pub async fn stream_chat(
        &self,
        request: ChatCompletionRequest,
        ctx: &mut ExecutionContext,
    ) -> Result<ExecutorChunkStream, ExecutorError> {
        let outcome = {
            let snapshot = self.catalog.snapshot();
            let req = self.build_chat_resolve_request(&request);
            self.resolver.resolve(&req, &snapshot, ctx)?
        };

        let state = self.strategy_state_for(&request.model);
        let ranked = rank(&outcome.candidates, outcome.strategy, &state);

        let mut last_err: Option<String> = None;
        let mut attempts = 0usize;
        let mut last_kind: &'static str = "unknown";
        let mut deferred_cutover: Option<&'static str> = None;

        for target in ranked {
            attempts += 1;
            last_kind = target.telemetry_tag();
            match self.dispatch_chat_stream(&target, request.clone(), ctx).await {
                Ok(stream) => {
                    ctx.route_metadata.served_by_node = served_by(&target);
                    ctx.route_metadata.backend_type = Some(target.telemetry_tag().to_string());
                    ctx.route_metadata.fallbacks_tried = (attempts - 1) as u32;
                    return Ok(stream);
                }
                Err(e) if e.aborts_fallback_chain() => return Err(e),
                Err(ExecutorError::TransportPendingCutover(kind)) => {
                    deferred_cutover.get_or_insert(kind);
                }
                Err(e) => {
                    tracing::warn!(
                        target_kind = target.telemetry_tag(),
                        error = %e,
                        "stream dispatch failed; trying next candidate"
                    );
                    last_err = Some(e.to_string());
                }
            }
        }

        if let Some(kind) = deferred_cutover {
            return Err(ExecutorError::TransportPendingCutover(kind));
        }

        Err(ExecutorError::AllCandidatesFailed {
            target_kind: last_kind,
            attempts,
            last_error: last_err.unwrap_or_else(|| "no candidates after rank".into()),
        })
    }

    /// Per-target stream dispatch. MVP: Local Embedded / HTTP / QUIC. Mesh
    /// + Flow streaming wraca `TransportPendingCutover` (Flow-stream
    /// dispatcher istnieje przez `try_dispatch_streaming`, ale zostawiam
    /// na R3a follow-up zeby ten cut byl atomowy).
    async fn dispatch_chat_stream(
        &self,
        target: &ResolvedExecutionTarget,
        mut request: ChatCompletionRequest,
        ctx: &mut ExecutionContext,
    ) -> Result<ExecutorChunkStream, ExecutorError> {
        use crate::api::openai::types::{ChunkChoice, Delta};
        use futures::StreamExt;
        use tentaflow_protocol::*;

        if let ResolvedExecutionTarget::Local { model_name, .. } = target {
            if request.model != *model_name {
                request.model = model_name.clone();
            }
        }
        request.stream = true;

        match target {
            ResolvedExecutionTarget::Local { handle, .. } => match handle {
                BackendHandle::Embedded { .. } => {
                    let rx = self
                        .local_inference
                        .stream_chat_chunks(&request)
                        .await
                        .map_err(|e| ExecutorError::Internal(e.to_string()))?;
                    let stream = futures::stream::unfold(rx, |mut rx| async move {
                        rx.recv().await.map(|chunk| (Ok(chunk), rx))
                    });
                    Ok(Box::pin(stream))
                }
                BackendHandle::Http(client) => {
                    let stream = client
                        .chat_completion_stream(request)
                        .await
                        .map_err(|e| ExecutorError::Internal(e.to_string()))?;
                    Ok(stream)
                }
                BackendHandle::Quic(handle) => {
                    let quic_client = handle.get_client().await.ok_or_else(|| {
                        ExecutorError::Internal(format!(
                            "QUIC client not connected for service '{}'",
                            handle.config.name
                        ))
                    })?;
                    let protocol_messages =
                        crate::routing::openai_messages_to_protocol(&request.messages);
                    let request_id = uuid::Uuid::new_v4().to_string();
                    let model_name_for_chunks = request.model.clone();
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
                    let quic_stream = quic_client
                        .send_request_stream(model_request)
                        .await
                        .map_err(|e| ExecutorError::Internal(format!("QUIC stream: {}", e)))?;

                    // Map raw protocol StreamChunk → ChatCompletionChunk.
                    // Mirror dyzurnej logiki z `routing/streaming.rs::route_to_quic_llm_stream`
                    // ale bez metric markera (executor middleware nie aktywny w MVP).
                    let chat_id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
                    let created = chrono::Utc::now().timestamp() as u64;
                    let stream = quic_stream.filter_map(move |chunk_result| {
                        let chat_id = chat_id.clone();
                        let model = model_name_for_chunks.clone();
                        async move {
                            match chunk_result {
                                Ok(stream_chunk) => match stream_chunk.chunk {
                                    StreamChunkType::TextDelta(text) => Some(Ok(ChatCompletionChunk {
                                        id: chat_id,
                                        object: "chat.completion.chunk".to_string(),
                                        created,
                                        model,
                                        choices: vec![ChunkChoice {
                                            index: 0,
                                            delta: Delta {
                                                role: None,
                                                content: Some(text),
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
                                        usage: None,
                                    })),
                                    StreamChunkType::ReasoningDelta(reasoning) => {
                                        Some(Ok(ChatCompletionChunk {
                                            id: chat_id,
                                            object: "chat.completion.chunk".to_string(),
                                            created,
                                            model,
                                            choices: vec![ChunkChoice {
                                                index: 0,
                                                delta: Delta {
                                                    role: None,
                                                    content: None,
                                                    reasoning_content: Some(reasoning),
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
                                            usage: None,
                                        }))
                                    }
                                    StreamChunkType::Done { final_metrics } => {
                                        // Etap 3a: stempluj `usage` na finish chunk gdy
                                        // backend zaraportował token rollup w
                                        // `DetailedMetrics::Completion`. Routing layer
                                        // (apply_include_usage_split) decyduje czy
                                        // przepuścić to pole na wire (gdy klient
                                        // poprosił `stream_options.include_usage=true`)
                                        // czy stripować je back-compat default.
                                        let usage = extract_completion_usage(final_metrics.as_ref());
                                        Some(Ok(ChatCompletionChunk {
                                            id: chat_id,
                                            object: "chat.completion.chunk".to_string(),
                                            created,
                                            model,
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
                                            usage,
                                        }))
                                    }
                                    _ => None,
                                },
                                Err(e) => {
                                    Some(Err(anyhow::anyhow!("QUIC stream chunk error: {}", e)))
                                }
                            }
                        }
                    });
                    Ok(Box::pin(stream))
                }
            },
            ResolvedExecutionTarget::MeshForward {
                node_id,
                model_name,
                ..
            } => {
                ctx.enter_hop().map_err(|e| {
                    ExecutorError::Internal(format!("mesh forward stream hop limit: {}", e))
                })?;
                let mesh = self.mesh_manager.read().clone().ok_or_else(|| {
                    ExecutorError::TransportPendingCutover("mesh_forward_stream")
                })?;
                let protocol_messages =
                    crate::routing::openai_messages_to_protocol(&request.messages);
                let request_id = uuid::Uuid::new_v4().to_string();
                let target_model = model_name.clone();
                let model_request = ModelRequest {
                    request_id: request_id.clone(),
                    payload: ModelPayload::Completion(CompletionPayload {
                        model: target_model.clone(),
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
                    .map_err(|e| {
                        ExecutorError::Internal(format!(
                            "mesh stream serialize ModelRequest: {}",
                            e
                        ))
                    })?
                    .into_vec();
                let frame_stream = mesh
                    .forward_stream_request(node_id, &request_id, payload)
                    .await
                    .map_err(|e| {
                        ExecutorError::Internal(format!(
                            "mesh forward stream request: {}",
                            e
                        ))
                    })?;
                let backend_url = format!("mesh://{}", node_id);
                let protocol_stream = frame_stream.map(move |frame_result| {
                    let frame = frame_result.map_err(|e| {
                        crate::error::CoreError::NetworkError {
                            message: format!("mesh stream read: {}", e),
                            source: e,
                        }
                    })?;
                    let archived = rkyv::access::<ArchivedModelStreamChunk, rkyv::rancor::Error>(
                        &frame,
                    )
                    .map_err(|e| crate::error::CoreError::BackendError {
                        backend_url: backend_url.clone(),
                        message: format!("mesh stream access ModelStreamChunk: {}", e),
                        source: None,
                    })?;
                    rkyv::deserialize::<ModelStreamChunk, rkyv::rancor::Error>(archived).map_err(
                        |e| crate::error::CoreError::BackendError {
                            backend_url: backend_url.clone(),
                            message: format!(
                                "mesh stream deserialize ModelStreamChunk: {}",
                                e
                            ),
                            source: None,
                        },
                    )
                });
                Ok(crate::routing::stream_helpers::quic_stream_to_openai_chunks(
                    protocol_stream,
                    target_model,
                ))
            }
            ResolvedExecutionTarget::Flow { .. } => {
                Err(ExecutorError::TransportPendingCutover("flow_stream"))
            }
        }
    }

    /// Build a `ResolveRequest` for chat. Surface is always Chat. Input
    /// modalities are inferred from the request shape: presence of
    /// `audio_input` flips Audio, image fragments flip Image. The chat
    /// path never silently transcribes audio for the model, so the
    /// resolver must reject any candidate that cannot decode the
    /// payload rather than fall back to text-only inference.
    fn build_chat_resolve_request<'a>(
        &self,
        request: &'a ChatCompletionRequest,
    ) -> ResolveRequest<'a> {
        // Audio enters chat through the dedicated `audio_input` field —
        // `MessageContent::Parts` only carries text + image fragments
        // today, so we don't probe it for audio.
        let needs_audio = request.audio_input.is_some();
        let needs_image = request.messages.iter().any(|m| {
            matches!(
                m.content.as_ref(),
                Some(crate::api::openai::types::MessageContent::Parts(parts))
                    if parts.iter().any(|p| matches!(
                        p,
                        crate::api::openai::types::ContentPart::ImageUrl { .. }
                    ))
            )
        });

        // Required modality slice — slot-allocate to keep the lifetime
        // bound to the request. Empty when only text in / text out.
        let inputs: &'a [InputModality] = match (needs_audio, needs_image) {
            (true, true) => &[InputModality::Audio, InputModality::Image, InputModality::Text],
            (true, false) => &[InputModality::Audio, InputModality::Text],
            (false, true) => &[InputModality::Image, InputModality::Text],
            (false, false) => &[],
        };

        ResolveRequest {
            requested_model: &request.model,
            required_surface: ServiceSurface::Chat,
            required_input_modalities: inputs,
            required_output_modalities: &[OutputModality::Text],
        }
    }

    /// Per-alias rotation state. New aliases get a fresh counter on first
    /// dispatch — `entry().or_insert` is atomic on DashMap so concurrent
    /// initialisation is safe.
    fn strategy_state_for(&self, alias: &str) -> Arc<StrategyState> {
        self.strategy_state
            .entry(alias.to_string())
            .or_insert_with(|| Arc::new(StrategyState::new()))
            .clone()
    }

    /// Branches per `ResolvedExecutionTarget`. Local handles dispatch
    /// in-process; flow goes through the dispatcher; mesh forward and
    /// QUIC sidecar return `TransportPendingCutover` because their
    /// transport plumbing still lives elsewhere in this build.
    ///
    /// Alias rewrite: when the request arrived under an alias and the
    /// resolver picked a service-backed candidate whose underlying
    /// `model_name` differs from the alias, we substitute that name
    /// onto the request before sending. OpenAI-compatible HTTP backends
    /// validate `request.model` against their loaded models and would
    /// reject the alias; the embedded engine looks up the model by name
    /// in `LocalInferenceManager` and would miss the resolved one
    /// otherwise. Flow targets keep the original name — the flow engine
    /// uses it as request context, not as a dispatch key.
    async fn dispatch_chat_blocking(
        &self,
        target: &ResolvedExecutionTarget,
        mut request: ChatCompletionRequest,
        ctx: &mut ExecutionContext,
    ) -> Result<ChatCompletionResponse, ExecutorError> {
        if let ResolvedExecutionTarget::Local { model_name, .. } = target {
            if request.model != *model_name {
                tracing::debug!(
                    requested = %request.model,
                    resolved = %model_name,
                    "rewriting request.model to resolved target id before dispatch"
                );
                request.model = model_name.clone();
            }
        }
        match target {
            ResolvedExecutionTarget::Local { handle, .. } => match handle {
                BackendHandle::Embedded { .. } => self
                    .local_inference
                    .handle_chat_completion(&request)
                    .await
                    .map_err(|e| ExecutorError::Internal(e.to_string())),
                BackendHandle::Http(client) => client
                    .chat_completion(request)
                    .await
                    .map_err(|e| ExecutorError::Internal(e.to_string())),
                BackendHandle::Quic(handle) => {
                    Self::dispatch_chat_quic(handle, request).await
                }
            },
            ResolvedExecutionTarget::MeshForward {
                node_id,
                model_name,
                ..
            } => {
                use tentaflow_protocol::*;
                let protocol_messages =
                    crate::routing::openai_messages_to_protocol(&request.messages);
                let model_request = ModelRequest {
                    request_id: uuid::Uuid::new_v4().to_string(),
                    payload: ModelPayload::Completion(CompletionPayload {
                        model: model_name.clone(),
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
                    stream: false,
                    metadata: None,
                    session_id: None,
                };
                let response = self
                    .forward_via_mesh(node_id, model_request, ctx)
                    .await?;
                match response.result {
                    ModelResult::Completion(completion) => Ok(ChatCompletionResponse {
                        id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
                        object: "chat.completion".to_string(),
                        created: chrono::Utc::now().timestamp() as u64,
                        model: request.model.clone(),
                        choices: vec![crate::api::openai::types::Choice {
                            index: 0,
                            message: crate::api::openai::types::Message {
                                role: "assistant".to_string(),
                                content: Some(crate::api::openai::types::MessageContent::Text(
                                    completion.text,
                                )),
                                reasoning_content: completion.reasoning_content,
                                ..Default::default()
                            },
                            finish_reason: completion.finish_reason,
                            logprobs: None,
                        }],
                        usage: response.metrics.and_then(|m| {
                            if let Some(DetailedMetrics::Completion {
                                prompt_tokens,
                                completion_tokens,
                                total_tokens,
                            }) = m.detailed
                            {
                                Some(crate::api::openai::types::Usage {
                                    prompt_tokens,
                                    completion_tokens,
                                    total_tokens,
                                })
                            } else {
                                None
                            }
                        }),
                        system_fingerprint: None,
                        transcribed_text: None,
                        speaker_id: None,
                        speaker_name: None,
                        speaker_confidence: None,
                        detected_intent: None,
                        detected_tools: None,
                    }),
                    ModelResult::Error(err) => Err(ExecutorError::Internal(format!(
                        "mesh chat error: {}",
                        err.message
                    ))),
                    _ => Err(ExecutorError::Internal(
                        "mesh chat returned unexpected result type".into(),
                    )),
                }
            }
            ResolvedExecutionTarget::Flow {
                flow_id,
                published_name,
            } => {
                let dispatcher = self
                    .flow_dispatcher
                    .as_ref()
                    .ok_or(ExecutorError::FlowDispatcherUnavailable)?;
                ctx.enter_flow(*flow_id).map_err(|e| {
                    ExecutorError::Internal(format!("flow recursion limit: {}", e))
                })?;

                // Pair `enter_flow` with `leave_flow` on every exit path
                // — a dispatcher failure must not leave the recursion
                // counter incremented, otherwise the next fallback
                // candidate (or a sibling resolve in an inherited ctx)
                // would falsely trip the depth limit.
                //
                // Dispatch by `flow_id` (resolved from the catalog),
                // not by `request.model`. Re-resolving the model name
                // through the dispatcher's name → flow lookup could land
                // on a different flow if the catalog has changed since
                // resolution or if the model name maps to a default flow
                // that is not the one this branch picked.
                let dispatch_result = {
                    let user = ctx.user.clone();
                    let (initial, meta) = crate::routing::build_initial_envelope_for_user(
                        &request, user,
                    );
                    dispatcher
                        .dispatch_by_flow_id(*flow_id, initial, meta)
                        .await
                };
                ctx.leave_flow();

                let outcome = dispatch_result
                    .map_err(|e| ExecutorError::Internal(e.to_string()))?
                    .ok_or_else(|| ExecutorError::FlowEmptyResult {
                        model: published_name.clone(),
                    })?;

                Ok(crate::routing::chat::flow_outcome_to_chat_response(
                    outcome,
                    &request.model,
                ))
            }
        }
    }

    /// R3b.7 — shared mesh forwarding for chat / embeddings / TTS / STT.
    /// Bumps `ctx.hop_count` (rejecting loops at `MAX_HOP_COUNT`),
    /// requires the mesh manager slot to be wired (DB-less router or
    /// `--no-mesh` returns `TransportPendingCutover` so the caller can
    /// pick the next candidate or take the legacy fallback), and trusts
    /// the mesh manager's pre-existing peer authentication (only trusted
    /// peers ever land in `IrohMeshManager.connections`).
    async fn forward_via_mesh(
        &self,
        target_node_id: &str,
        mut model_request: tentaflow_protocol::ModelRequest,
        ctx: &mut ExecutionContext,
    ) -> Result<tentaflow_protocol::ModelResponse, ExecutorError> {
        use tentaflow_protocol::*;

        ctx.enter_hop().map_err(|e| {
            ExecutorError::Internal(format!("mesh forward hop limit: {}", e))
        })?;

        let mesh = self.mesh_manager.read().clone().ok_or_else(|| {
            ExecutorError::TransportPendingCutover("mesh_forward")
        })?;

        // Codex R3b.7 H1 (defense in depth): re-verify trust on the
        // executor side too. Underlying transport already checks but a
        // misconfigured slot or peer registered before trust gating
        // would slip through.
        if !mesh.is_trusted(target_node_id) {
            return Err(ExecutorError::Internal(format!(
                "mesh forward target '{}' is not trusted",
                target_node_id
            )));
        }

        // Codex R3b.7 H2: carry hop count across the mesh boundary so
        // peers can refuse re-forwarding past `MAX_HOP_COUNT`. Without
        // this an A→B→A cycle resets to 0 on every node and loops
        // until the underlying QUIC connection breaks.
        let hop_kv = (
            crate::services::runtime::context::MESH_HOP_HEADER.to_string(),
            ctx.hop_count.to_string(),
        );
        match model_request.metadata.as_mut() {
            Some(meta) => meta.push(hop_kv),
            None => model_request.metadata = Some(vec![hop_kv]),
        }

        let request_id = model_request.request_id.clone();
        let payload = rkyv::to_bytes::<rkyv::rancor::Error>(&model_request)
            .map_err(|e| {
                ExecutorError::Internal(format!("mesh forward serialize: {}", e))
            })?
            .into_vec();
        let response_bytes = mesh
            .forward_request(target_node_id, &request_id, payload)
            .await
            .map_err(|e| ExecutorError::Internal(format!("mesh forward request: {}", e)))?;
        let archived = rkyv::access::<ArchivedModelResponse, rkyv::rancor::Error>(
            &response_bytes,
        )
        .map_err(|e| {
            ExecutorError::Internal(format!("mesh forward access ModelResponse: {}", e))
        })?;
        rkyv::deserialize::<ModelResponse, rkyv::rancor::Error>(archived).map_err(|e| {
            ExecutorError::Internal(format!("mesh forward deserialize ModelResponse: {}", e))
        })
    }

    /// QUIC sidecar dispatch (R2a). Wczesniej zwracalo
    /// `TransportPendingCutover`; teraz buduje `ModelRequest::Completion`,
    /// wysyla przez `Arc<QuicClient>` z handle'u, mapuje response na
    /// `ChatCompletionResponse`. Logika lustro `chat.rs::route_to_quic_llm`
    /// bez aplikacji `response_middleware` — to robi caller (api handler /
    /// chat.rs) zeby executor pozostal middleware-agnostic.
    async fn dispatch_chat_quic(
        handle: &Arc<crate::services::runtime::quic_handle::QuicServiceHandle>,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, ExecutorError> {
        use crate::api::openai::types::{Choice, Message, MessageContent, Usage};
        use tentaflow_protocol::*;

        let quic_client = handle.get_client().await.ok_or_else(|| {
            ExecutorError::Internal(format!(
                "QUIC client not connected for service '{}'",
                handle.config.name
            ))
        })?;

        let protocol_messages =
            crate::routing::openai_messages_to_protocol(&request.messages);
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
            stream: false,
            metadata: None,
            session_id: None,
        };

        let model_response = quic_client
            .send_request(model_request)
            .await
            .map_err(|e| ExecutorError::Internal(format!("QUIC send_request: {}", e)))?;

        match model_response.result {
            ModelResult::Completion(completion) => Ok(ChatCompletionResponse {
                id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
                object: "chat.completion".to_string(),
                created: chrono::Utc::now().timestamp() as u64,
                model: request.model.clone(),
                choices: vec![Choice {
                    index: 0,
                    message: Message {
                        role: "assistant".to_string(),
                        content: Some(MessageContent::Text(completion.text)),
                        reasoning_content: completion.reasoning_content,
                        ..Default::default()
                    },
                    finish_reason: completion.finish_reason,
                    logprobs: None,
                }],
                usage: model_response.metrics.and_then(|m| {
                    if let Some(DetailedMetrics::Completion {
                        prompt_tokens,
                        completion_tokens,
                        total_tokens,
                    }) = m.detailed
                    {
                        Some(Usage {
                            prompt_tokens,
                            completion_tokens,
                            total_tokens,
                        })
                    } else {
                        None
                    }
                }),
                system_fingerprint: None,
                transcribed_text: None,
                speaker_id: None,
                speaker_name: None,
                speaker_confidence: None,
                detected_intent: None,
                detected_tools: None,
            }),
            ModelResult::Error(err) => Err(ExecutorError::Internal(format!(
                "QUIC LLM error: {}",
                err.message
            ))),
            _ => Err(ExecutorError::Internal(
                "QUIC LLM returned unexpected result type".to_string(),
            )),
        }
    }

    /// Public accessor for the configured middleware factory list. The
    /// streaming entry points walk this list and call `start_session`
    /// per request to materialise an isolated stack — never share the
    /// returned `Vec` itself across streams.
    pub fn middleware_factories(&self) -> &[Arc<dyn StreamMiddlewareFactory>] {
        &self.middleware
    }

    // =========================================================================
    // R3b.1 — Embeddings dispatch
    // =========================================================================

    /// Embeddings dispatch — mirrors `execute_chat`. Resolves the requested
    /// model through the catalog with `ServiceSurface::Embeddings`, ranks
    /// candidates per alias strategy, dispatches to the first that succeeds.
    /// `MeshForward` returns `TransportPendingCutover` until R3b.7 wires the
    /// mesh transport into the executor.
    ///
    /// **ACL is the caller's responsibility** — mirror of `execute_chat`.
    pub async fn execute_embeddings(
        &self,
        request: EmbeddingRequest,
        ctx: &mut ExecutionContext,
    ) -> Result<EmbeddingResponse, ExecutorError> {
        let outcome = {
            let snapshot = self.catalog.snapshot();
            // OpenAI `/v1/embeddings` is text-in / vector-out. Constrain the
            // resolver so an image-only embedding service (e.g. CLIP-vision)
            // cannot match a plain text request — keeps the same `Embeddings`
            // surface but filters by modality.
            let req = ResolveRequest {
                requested_model: &request.model,
                required_surface: ServiceSurface::Embeddings,
                required_input_modalities: &[InputModality::Text],
                required_output_modalities: &[OutputModality::Embedding],
            };
            self.resolver.resolve(&req, &snapshot, ctx)?
        };

        let state = self.strategy_state_for(&request.model);
        let ranked = rank(&outcome.candidates, outcome.strategy, &state);

        let mut last_err: Option<String> = None;
        let mut attempts = 0usize;
        let mut last_kind: &'static str = "unknown";
        let mut deferred_cutover: Option<&'static str> = None;

        for target in ranked {
            attempts += 1;
            last_kind = target.telemetry_tag();
            match self
                .dispatch_embeddings_blocking(&target, request.clone(), ctx)
                .await
            {
                Ok(response) => {
                    ctx.route_metadata.served_by_node = served_by(&target);
                    ctx.route_metadata.backend_type = Some(target.telemetry_tag().to_string());
                    ctx.route_metadata.fallbacks_tried = (attempts - 1) as u32;
                    return Ok(response);
                }
                Err(e) if e.aborts_fallback_chain() => return Err(e),
                Err(ExecutorError::TransportPendingCutover(kind)) => {
                    deferred_cutover.get_or_insert(kind);
                }
                Err(e) => {
                    tracing::warn!(
                        target_kind = target.telemetry_tag(),
                        error = %e,
                        "embeddings dispatch failed; trying next candidate"
                    );
                    last_err = Some(e.to_string());
                }
            }
        }

        if let Some(kind) = deferred_cutover {
            return Err(ExecutorError::TransportPendingCutover(kind));
        }

        Err(ExecutorError::AllCandidatesFailed {
            target_kind: last_kind,
            attempts,
            last_error: last_err.unwrap_or_else(|| "no candidates after rank".into()),
        })
    }

    /// Per-target embeddings dispatch. Embedded backends route through
    /// `LocalInferenceHandler::handle_embeddings` — engines that don't
    /// implement embeddings (the trait default is `bail!`) surface their
    /// own error rather than this dispatcher hard-rejecting them.
    async fn dispatch_embeddings_blocking(
        &self,
        target: &ResolvedExecutionTarget,
        mut request: EmbeddingRequest,
        ctx: &mut ExecutionContext,
    ) -> Result<EmbeddingResponse, ExecutorError> {
        use tentaflow_protocol::*;

        if let ResolvedExecutionTarget::Local { model_name, .. } = target {
            if request.model != *model_name {
                request.model = model_name.clone();
            }
        }

        match target {
            ResolvedExecutionTarget::Local { handle, .. } => match handle {
                BackendHandle::Embedded { .. } => self
                    .local_inference
                    .handle_embeddings(&request)
                    .await
                    .map_err(|e| ExecutorError::Internal(e.to_string())),
                BackendHandle::Http(client) => client
                    .embeddings_request(request)
                    .await
                    .map_err(|e| ExecutorError::Internal(e.to_string())),
                BackendHandle::Quic(handle) => {
                    let quic_client = handle.get_client().await.ok_or_else(|| {
                        ExecutorError::Internal(format!(
                            "QUIC client not connected for service '{}'",
                            handle.config.name
                        ))
                    })?;

                    let input_texts = match &request.input {
                        EmbeddingInput::Single(text) => vec![text.clone()],
                        EmbeddingInput::Multiple(texts) => texts.clone(),
                    };
                    let text_count = input_texts.len();
                    // Strip well-known router-side prefixes so the engine
                    // sees the bare model name. Mirror of legacy
                    // `routing/embeddings.rs::route_embeddings_quic`.
                    let engine_model_name = request
                        .model
                        .strip_prefix("tentaflow-embeddings-")
                        .or_else(|| request.model.strip_prefix("embeddings-"))
                        .unwrap_or(&request.model)
                        .to_string();

                    let model_request = ModelRequest {
                        request_id: uuid::Uuid::new_v4().to_string(),
                        payload: ModelPayload::Embeddings(EmbeddingsPayload {
                            model: engine_model_name,
                            input: input_texts,
                            normalize: true,
                        }),
                        stream: false,
                        metadata: None,
                        session_id: None,
                    };

                    let response = quic_client
                        .send_request(model_request)
                        .await
                        .map_err(|e| ExecutorError::Internal(format!("QUIC embeddings: {}", e)))?;

                    match response.result {
                        ModelResult::Embeddings(result) => {
                            let data = result
                                .embeddings
                                .into_iter()
                                .enumerate()
                                .map(|(idx, embedding)| EmbeddingData {
                                    object: "embedding".to_string(),
                                    index: idx as u32,
                                    embedding,
                                })
                                .collect();
                            // Heuristic token count — embeddings backends do not
                            // return usage stats over the wire; mirror of the
                            // legacy routing/embeddings.rs estimate.
                            let estimated = (text_count * 50) as u32;
                            Ok(EmbeddingResponse {
                                object: "list".to_string(),
                                data,
                                model: request.model.clone(),
                                usage: EmbeddingUsage {
                                    prompt_tokens: estimated,
                                    total_tokens: estimated,
                                },
                            })
                        }
                        ModelResult::Error(err) => Err(ExecutorError::Internal(format!(
                            "QUIC embeddings error: {}",
                            err.message
                        ))),
                        _ => Err(ExecutorError::Internal(
                            "QUIC embeddings returned unexpected result type".into(),
                        )),
                    }
                }
            },
            ResolvedExecutionTarget::MeshForward {
                node_id,
                model_name,
                ..
            } => {
                let input_texts = match &request.input {
                    EmbeddingInput::Single(text) => vec![text.clone()],
                    EmbeddingInput::Multiple(texts) => texts.clone(),
                };
                let text_count = input_texts.len();
                let model_request = ModelRequest {
                    request_id: uuid::Uuid::new_v4().to_string(),
                    payload: ModelPayload::Embeddings(EmbeddingsPayload {
                        model: model_name.clone(),
                        input: input_texts,
                        normalize: true,
                    }),
                    stream: false,
                    metadata: None,
                    session_id: None,
                };
                let response = self
                    .forward_via_mesh(node_id, model_request, ctx)
                    .await?;
                match response.result {
                    ModelResult::Embeddings(result) => {
                        // Codex R3b.7 M2: cardinality guard. Peer that
                        // returns fewer/more vectors than the input batch
                        // size is a wire contract violation — surface it
                        // instead of silently mis-aligning vectors with
                        // their input texts.
                        if result.embeddings.len() != text_count {
                            return Err(ExecutorError::Internal(format!(
                                "mesh embeddings returned {} vectors for {} input(s) — cardinality mismatch",
                                result.embeddings.len(),
                                text_count
                            )));
                        }
                        let data = result
                            .embeddings
                            .into_iter()
                            .enumerate()
                            .map(|(idx, embedding)| EmbeddingData {
                                object: "embedding".to_string(),
                                index: idx as u32,
                                embedding,
                            })
                            .collect();
                        let estimated = (text_count * 50) as u32;
                        Ok(EmbeddingResponse {
                            object: "list".to_string(),
                            data,
                            model: request.model.clone(),
                            usage: EmbeddingUsage {
                                prompt_tokens: estimated,
                                total_tokens: estimated,
                            },
                        })
                    }
                    ModelResult::Error(err) => Err(ExecutorError::Internal(format!(
                        "mesh embeddings error: {}",
                        err.message
                    ))),
                    _ => Err(ExecutorError::Internal(
                        "mesh embeddings returned unexpected result type".into(),
                    )),
                }
            }
            ResolvedExecutionTarget::Flow {
                flow_id,
                published_name,
            } => {
                // Catalog can advertise embedding-surface flows
                // (`EmbeddingsNodeAdapter` is registered) so this branch must
                // execute the flow, not refuse it. Caller convention: flow
                // output's `embedding` (single) or `embeddings` (batched)
                // key carries the vector payload. Anything else → reject
                // with Internal so the operator notices a mis-shaped flow
                // instead of getting an empty embedding.
                let dispatcher = self
                    .flow_dispatcher
                    .as_ref()
                    .ok_or(ExecutorError::FlowDispatcherUnavailable)?;
                ctx.enter_flow(*flow_id).map_err(|e| {
                    ExecutorError::Internal(format!("flow recursion limit: {}", e))
                })?;
                // Codex R3b.1 round 2 H1: propagate user → flow ACL gate.
                // Without this `dispatch_by_flow_id` sees `user_id = None`
                // and skips the per-flow ACL check.
                let (initial, meta) = embeddings_request_to_initial_envelope(
                    &request, ctx.user.clone(),
                );
                let dispatch_result = dispatcher
                    .dispatch_by_flow_id(*flow_id, initial, meta)
                    .await;
                ctx.leave_flow();
                let outcome = dispatch_result
                    .map_err(|e| ExecutorError::Internal(e.to_string()))?
                    .ok_or_else(|| ExecutorError::FlowEmptyResult {
                        model: published_name.clone(),
                    })?;
                let expected_count = match &request.input {
                    EmbeddingInput::Single(_) => 1,
                    EmbeddingInput::Multiple(texts) => texts.len(),
                };
                flow_outcome_to_embedding_response(outcome, &request, expected_count)
            }
        }
    }

    // =========================================================================
    // R3b.3 — TTS dispatch
    // =========================================================================

    /// TTS dispatch — mirrors `execute_chat`/`execute_embeddings`. Resolves
    /// the requested model with `ServiceSurface::Tts`, requires text input
    /// and audio output. Returns the audio bytes alongside the actual
    /// container/codec produced by the backend so the HTTP layer can set
    /// `Content-Type` correctly. Embedded engines synthesise PCM samples
    /// and the executor packs them as WAV; HTTP backends honour
    /// `request.response_format`; QUIC backends echo whatever the upstream
    /// returns and reuse the request format as the wire-side hint.
    ///
    /// **ACL is the caller's responsibility.**
    pub async fn execute_tts(
        &self,
        request: TTSRequest,
        ctx: &mut ExecutionContext,
    ) -> Result<TtsExecutionResult, ExecutorError> {
        let outcome = {
            let snapshot = self.catalog.snapshot();
            let req = ResolveRequest {
                requested_model: &request.model,
                required_surface: ServiceSurface::Tts,
                required_input_modalities: &[InputModality::Text],
                required_output_modalities: &[OutputModality::Audio],
            };
            self.resolver.resolve(&req, &snapshot, ctx)?
        };

        let state = self.strategy_state_for(&request.model);
        let ranked = rank(&outcome.candidates, outcome.strategy, &state);

        let mut last_err: Option<String> = None;
        let mut attempts = 0usize;
        let mut last_kind: &'static str = "unknown";
        let mut deferred_cutover: Option<&'static str> = None;

        for target in ranked {
            attempts += 1;
            last_kind = target.telemetry_tag();
            match self
                .dispatch_tts_blocking(&target, request.clone(), ctx)
                .await
            {
                Ok(result) => {
                    ctx.route_metadata.served_by_node = served_by(&target);
                    ctx.route_metadata.backend_type = Some(target.telemetry_tag().to_string());
                    ctx.route_metadata.fallbacks_tried = (attempts - 1) as u32;
                    return Ok(result);
                }
                Err(e) if e.aborts_fallback_chain() => return Err(e),
                Err(ExecutorError::TransportPendingCutover(kind)) => {
                    deferred_cutover.get_or_insert(kind);
                }
                Err(e) => {
                    tracing::warn!(
                        target_kind = target.telemetry_tag(),
                        error = %e,
                        "tts dispatch failed; trying next candidate"
                    );
                    last_err = Some(e.to_string());
                }
            }
        }

        if let Some(kind) = deferred_cutover {
            return Err(ExecutorError::TransportPendingCutover(kind));
        }

        Err(ExecutorError::AllCandidatesFailed {
            target_kind: last_kind,
            attempts,
            last_error: last_err.unwrap_or_else(|| "no candidates after rank".into()),
        })
    }

    /// Per-target TTS dispatch.
    /// - `Local::Embedded` → `crate::tts::shared_tts_manager()` synthesize
    ///   on a blocking task (FFI calls into Apple AVSpeech / Kokoro / sherpa
    ///   are sync). Result wrapped in WAV PCM16.
    /// - `Local::Http(client)` → OpenAI-compatible POST `/v1/audio/speech`.
    /// - `Local::Quic(handle)` → `ModelRequest::Audio(TTS{...})`.
    /// - `MeshForward` → `TransportPendingCutover` (R3b.7).
    /// - `Flow` → `Internal` — no surface for TTS-as-flow yet.
    async fn dispatch_tts_blocking(
        &self,
        target: &ResolvedExecutionTarget,
        mut request: TTSRequest,
        ctx: &mut ExecutionContext,
    ) -> Result<TtsExecutionResult, ExecutorError> {
        use tentaflow_protocol::*;

        if let ResolvedExecutionTarget::Local { model_name, .. } = target {
            if request.model != *model_name {
                request.model = model_name.clone();
            }
        }

        match target {
            ResolvedExecutionTarget::Local { handle, .. } => match handle {
                BackendHandle::Embedded {
                    engine_id,
                    model_name,
                    ..
                } => {
                    // Codex R3b.3 H2: embedded engines always emit WAV
                    // (PCM samples packed locally). Reject mismatched
                    // requested format up-front so the caller learns the
                    // unsupported codec instead of getting WAV bytes
                    // labeled as MP3.
                    if let Some(req_fmt) = &request.response_format {
                        let normalized = req_fmt.to_ascii_lowercase();
                        if !matches!(normalized.as_str(), "wav" | "pcm") {
                            return Err(ExecutorError::Internal(format!(
                                "embedded TTS engine '{}' only emits WAV/PCM; \
                                 requested '{}' is not supported here",
                                engine_id, req_fmt
                            )));
                        }
                    }
                    // Codex R3b.3 H1: lookup by `engine_id` (manifest engine.id,
                    // e.g. "apple-tts") — `model_name` like "zosia-pl" is the
                    // voice preset and would miss the manager registration.
                    let engine_id_owned = engine_id.clone();
                    let model_name_owned = model_name.clone();
                    let text = request.input.clone();
                    let speed = request.speed.unwrap_or(1.0);
                    let res = tokio::task::spawn_blocking(
                        move || -> anyhow::Result<(Vec<f32>, u32)> {
                            let mgr = crate::tts::shared_tts_manager();
                            let guard = mgr.blocking_read();
                            // Some embedded engines (apple-tts) honour
                            // per-voice presets through `speaker_id`; pre-R3b
                            // legacy passes 0 and lets the engine decide. We
                            // mirror that — voice/preset selection by name
                            // happens inside the engine via `engine_id`.
                            let _ = model_name_owned;
                            let out = guard.synthesize(
                                &engine_id_owned,
                                crate::tts::SynthesizeParams {
                                    text,
                                    speaker_id: 0,
                                    speed,
                                },
                            )?;
                            Ok((out.samples, out.sample_rate))
                        },
                    )
                    .await
                    .map_err(|e| ExecutorError::Internal(format!("embedded TTS join: {e}")))?
                    .map_err(|e| ExecutorError::Internal(e.to_string()))?;
                    let (samples, sr) = res;
                    Ok(TtsExecutionResult {
                        bytes: samples_to_wav_pcm16(&samples, sr),
                        format: "wav".to_string(),
                    })
                }
                BackendHandle::Http(client) => {
                    let bytes = client
                        .audio_speech(&request)
                        .await
                        .map_err(|e| ExecutorError::Internal(e.to_string()))?;
                    let format = request
                        .response_format
                        .clone()
                        .unwrap_or_else(|| "wav".to_string());
                    Ok(TtsExecutionResult { bytes, format })
                }
                BackendHandle::Quic(handle) => {
                    let quic_client = handle.get_client().await.ok_or_else(|| {
                        ExecutorError::Internal(format!(
                            "QUIC client not connected for service '{}'",
                            handle.config.name
                        ))
                    })?;
                    let format = request.response_format.clone().unwrap_or_else(|| "wav".into());
                    let speed = request.speed.unwrap_or(1.0);
                    let model_request = ModelRequest {
                        request_id: uuid::Uuid::new_v4().to_string(),
                        payload: ModelPayload::Audio(AudioPayload {
                            operation: AudioOperation::TTS {
                                model: request.model.clone(),
                                input: request.input.clone(),
                                voice: request.voice.clone(),
                                format: Some(format.clone()),
                                speed: Some(speed),
                                language: request.language.clone(),
                            },
                        }),
                        stream: false,
                        metadata: None,
                        session_id: None,
                    };
                    let response = quic_client
                        .send_request(model_request)
                        .await
                        .map_err(|e| ExecutorError::Internal(format!("QUIC TTS: {}", e)))?;
                    match response.result {
                        ModelResult::Audio(audio_result) => match audio_result.data {
                            AudioResultData::Audio(bytes) => {
                                Ok(TtsExecutionResult { bytes, format })
                            }
                            _ => Err(ExecutorError::Internal(
                                "QUIC TTS returned non-audio result".into(),
                            )),
                        },
                        ModelResult::Error(err) => Err(ExecutorError::Internal(format!(
                            "QUIC TTS error: {}",
                            err.message
                        ))),
                        _ => Err(ExecutorError::Internal(
                            "QUIC TTS returned unexpected result type".into(),
                        )),
                    }
                }
            },
            ResolvedExecutionTarget::MeshForward {
                node_id,
                model_name,
                ..
            } => {
                let format = request.response_format.clone().unwrap_or_else(|| "wav".into());
                let model_request = ModelRequest {
                    request_id: uuid::Uuid::new_v4().to_string(),
                    payload: ModelPayload::Audio(AudioPayload {
                        operation: AudioOperation::TTS {
                            model: model_name.clone(),
                            input: request.input.clone(),
                            voice: request.voice.clone(),
                            format: Some(format.clone()),
                            speed: request.speed,
                            language: request.language.clone(),
                        },
                    }),
                    stream: false,
                    metadata: None,
                    session_id: None,
                };
                let response = self
                    .forward_via_mesh(node_id, model_request, ctx)
                    .await?;
                match response.result {
                    ModelResult::Audio(audio_result) => match audio_result.data {
                        AudioResultData::Audio(bytes) => {
                            Ok(TtsExecutionResult { bytes, format })
                        }
                        _ => Err(ExecutorError::Internal(
                            "mesh TTS returned non-audio result".into(),
                        )),
                    },
                    ModelResult::Error(err) => Err(ExecutorError::Internal(format!(
                        "mesh TTS error: {}",
                        err.message
                    ))),
                    _ => Err(ExecutorError::Internal(
                        "mesh TTS returned unexpected result type".into(),
                    )),
                }
            }
            ResolvedExecutionTarget::Flow { flow_id, .. } => {
                let dispatcher = self
                    .flow_dispatcher
                    .as_ref()
                    .ok_or(ExecutorError::FlowDispatcherUnavailable)?;
                ctx.enter_flow(*flow_id).map_err(|e| {
                    ExecutorError::Internal(format!("flow recursion limit: {e}"))
                })?;
                let (initial, meta) =
                    tts_request_to_initial_envelope(&request, ctx.user.clone());
                let dispatch_result =
                    dispatcher.dispatch_by_flow_id(*flow_id, initial, meta).await;
                ctx.leave_flow();
                let outcome = dispatch_result
                    .map_err(|e| ExecutorError::Internal(e.to_string()))?
                    .ok_or_else(|| ExecutorError::FlowEmptyResult {
                        model: request.model.clone(),
                    })?;
                flow_outcome_to_tts_result(outcome, dispatcher.blobs()).await
            }
        }
    }

    // =========================================================================
    // R3b.5 — STT dispatch (thin delegate to SttRuntime)
    // =========================================================================

    /// STT delegate. Resolver wybiera service po modelu (`build_stt_resolve_request`):
    /// * `Local{service_id}` → `transcribe_for_service(service_id)` —
    ///   wybiera per-service backend (Http dla python-bundle wrapperow,
    ///   Local dla embedded whisper).
    /// * `MeshForward{node_id, service_id}` → forward STT request przez
    ///   QUIC/iroh do peera. Aktualnie nie wspierane wprost na poziomie
    ///   Executor (mesh STT forward jest TODO przy bigger refactor mesh
    ///   inference proxy); wracamy `SttBackend("mesh forward not implemented")`
    ///   zeby request padal czytelnie zamiast cicho lokalnym whisperem.
    /// * `Flow` → wracamy clean failure (flow STT idzie przez flow_engine
    ///   adapter, nie executor).
    /// Resolver error (UnknownModel/CapabilityUnsupported) padamy
    /// `SttBackend(error)` zeby user zobaczyl klarowny blad.
    /// Gdy `model` jest pusty / brak kandydatow → fallback do default
    /// local whisper (zachowuje pre-existing UX dla single-engine node'u).
    pub async fn execute_stt(
        &self,
        request: TranscriptionRequest,
        ctx: &mut ExecutionContext,
    ) -> Result<TranscriptionResponse, ExecutorError> {
        let runtime = self.stt_runtime.read().clone().ok_or_else(|| {
            ExecutorError::SttRuntimeUnavailable
        })?;

        // Pusty model = bezposredni fallback do default local whisper
        // (handler `/v1/audio/transcriptions` bez `model` field — legacy
        // zachowanie).
        if request.model.trim().is_empty() {
            return runtime
                .transcribe(request)
                .await
                .map_err(|e| ExecutorError::SttBackend(e.to_string()));
        }

        let snapshot = self.catalog.snapshot();
        let req = self.build_stt_resolve_request(&request);
        let outcome = match self.resolver.resolve(&req, &snapshot, ctx) {
            Ok(o) => o,
            Err(crate::services::runtime::resolver::ResolveError::UnknownModel(_))
            | Err(crate::services::runtime::resolver::ResolveError::CapabilityUnsupported { .. }) => {
                // Legacy single-node bez catalog STT entries: client wysyla
                // `model="whisper-1"` (default), katalog nie ma takiego
                // wpisu — fallback do default local whisper zamiast hard
                // error. Inne resolver errors (np. AclDenied) propagujemy.
                return runtime
                    .transcribe(request)
                    .await
                    .map_err(|e| ExecutorError::SttBackend(e.to_string()));
            }
            Err(e) => return Err(ExecutorError::SttBackend(format!("resolver: {}", e))),
        };

        let state = self.strategy_state_for(&request.model);
        let ranked = rank(&outcome.candidates, outcome.strategy, &state);
        if ranked.is_empty() {
            // Resolver nie zwrocil zadnego kandydata mimo OK — fallback
            // do default local zeby legacy single-node node bez STT services
            // w katalogu nadal dzialal.
            return runtime
                .transcribe(request)
                .await
                .map_err(|e| ExecutorError::SttBackend(e.to_string()));
        }

        for target in ranked {
            match target {
                ResolvedExecutionTarget::Local { service_id, .. } => {
                    return runtime
                        .transcribe_for_service(service_id, request)
                        .await
                        .map_err(|e| ExecutorError::SttBackend(e.to_string()));
                }
                ResolvedExecutionTarget::MeshForward { node_id, .. } => {
                    return Err(ExecutorError::SttBackend(format!(
                        "mesh forward STT (node {}) not implemented yet",
                        node_id
                    )));
                }
                ResolvedExecutionTarget::Flow { .. } => {
                    return Err(ExecutorError::SttBackend(
                        "STT through flow_engine not supported via executor.execute_stt"
                            .to_string(),
                    ));
                }
            }
        }
        unreachable!("ranked has at least one element after empty check")
    }

    fn build_stt_resolve_request<'a>(
        &self,
        request: &'a TranscriptionRequest,
    ) -> ResolveRequest<'a> {
        ResolveRequest {
            requested_model: &request.model,
            required_surface: ServiceSurface::Stt,
            required_input_modalities: &[InputModality::Audio],
            required_output_modalities: &[OutputModality::Text],
        }
    }
}

/// TTS execution outcome with the actual audio container so callers can set
/// the right `Content-Type`. Embedded TTS always emits WAV; HTTP/QUIC
/// backends honour the requested format and reflect it back here.
#[derive(Debug, Clone)]
pub struct TtsExecutionResult {
    pub bytes: Vec<u8>,
    pub format: String,
}

/// Pack `Vec<f32>` PCM samples (range -1.0..=1.0) into a WAV/PCM16 byte
/// buffer with a 44-byte RIFF header. Used by embedded TTS engines whose
/// output is normalised float — callers expecting raw bytes from the
/// `/v1/audio/speech` surface get a self-describing container.
fn samples_to_wav_pcm16(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let pcm16: Vec<i16> = samples
        .iter()
        .map(|f| (f.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
        .collect();
    let data_bytes: Vec<u8> = pcm16.iter().flat_map(|s| s.to_le_bytes()).collect();
    let data_len = data_bytes.len() as u32;
    let chunk_size = 36 + data_len;
    let byte_rate = sample_rate * 2;
    let mut buf = Vec::with_capacity(44 + data_bytes.len());
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&chunk_size.to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    buf.extend_from_slice(&1u16.to_le_bytes()); // mono
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&2u16.to_le_bytes()); // block align
    buf.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_len.to_le_bytes());
    buf.extend_from_slice(&data_bytes);
    buf
}

/// Buduje seed envelope + per-request meta dla embeddings flow path.
fn embeddings_request_to_initial_envelope(
    request: &EmbeddingRequest,
    user: Option<crate::auth::acl::UserContext>,
) -> (
    crate::flow_engine::envelope::FlowEnvelope,
    crate::flow_engine::dispatcher::FlowRequestMeta,
) {
    use crate::flow_engine::envelope::{FlowEnvelope, FlowValue};
    let input_text = match &request.input {
        EmbeddingInput::Single(text) => text.clone(),
        EmbeddingInput::Multiple(texts) => texts.join("\n"),
    };
    let mut env = FlowEnvelope::empty();
    env.payload = FlowValue::Text(input_text);
    env.meta.insert(
        "embeddings_model".into(),
        serde_json::Value::String(request.model.clone()),
    );
    if let Some(d) = request.dimensions {
        env.meta
            .insert("dimensions".into(), serde_json::Value::Number(d.into()));
    }
    if let Some(fmt) = &request.encoding_format {
        env.meta.insert(
            "encoding_format".into(),
            serde_json::Value::String(fmt.clone()),
        );
    }

    let mut meta =
        crate::flow_engine::dispatcher::FlowRequestMeta::new(uuid::Uuid::new_v4().to_string());
    if let Some(u) = user {
        meta.user_id = Some(u.user_id);
        meta.user_role = Some(u.role);
    }
    (env, meta)
}

/// Buduje seed envelope + meta dla TTS-as-flow path. `voice` / `format` /
/// `language` lądują w `envelope.meta`, `TtsNodeAdapter::pick_optional_str`
/// czyta je z fallback `node.config -> envelope.meta`. Operator może
/// override'ować przez node config; brak override = użyj wartości z requestu.
fn tts_request_to_initial_envelope(
    request: &TTSRequest,
    user: Option<crate::auth::acl::UserContext>,
) -> (
    crate::flow_engine::envelope::FlowEnvelope,
    crate::flow_engine::dispatcher::FlowRequestMeta,
) {
    use crate::flow_engine::envelope::{FlowEnvelope, FlowValue};
    let mut env = FlowEnvelope::empty();
    env.payload = FlowValue::Text(request.input.clone());
    env.meta.insert(
        "tts_model".into(),
        serde_json::Value::String(request.model.clone()),
    );
    env.meta.insert(
        "voice".into(),
        serde_json::Value::String(request.voice.clone()),
    );
    if let Some(fmt) = &request.response_format {
        env.meta
            .insert("format".into(), serde_json::Value::String(fmt.clone()));
    }
    if let Some(lang) = &request.language {
        env.meta
            .insert("language".into(), serde_json::Value::String(lang.clone()));
    }

    let mut meta =
        crate::flow_engine::dispatcher::FlowRequestMeta::new(uuid::Uuid::new_v4().to_string());
    if let Some(u) = user {
        meta.user_id = Some(u.user_id);
        meta.user_role = Some(u.role);
    }
    (env, meta)
}

/// Konwertuje FlowExecutionOutcome (z TTS-as-flow) na `TtsExecutionResult`.
/// Output flow musi mieć `payload = FlowValue::Audio { blob_ref, mime, .. }`;
/// w przeciwnym wypadku zwracamy Internal — runtime check ostatniej deski
/// ratunku, bo R8 walidacja sama nie wymusza Audio-on-output (`output` adapter
/// ma `input_port_type = Any`).
async fn flow_outcome_to_tts_result(
    outcome: crate::flow_engine::envelope::FlowExecutionOutcome,
    blobs: std::sync::Arc<dyn crate::flow_engine::blob_store::BlobStore>,
) -> Result<TtsExecutionResult, ExecutorError> {
    use crate::flow_engine::envelope::FlowValue;
    match outcome.final_envelope.payload {
        FlowValue::Audio { blob_ref, mime, .. } => {
            let bytes = blobs.get(&blob_ref).await.map_err(|e| {
                ExecutorError::Internal(format!("tts flow blob read: {e}"))
            })?;
            let format = tts_mime_to_format(&mime)?;
            Ok(TtsExecutionResult { bytes, format })
        }
        other => Err(ExecutorError::Internal(format!(
            "tts flow returned non-Audio payload kind: {}",
            other.kind()
        ))),
    }
}

fn tts_mime_to_format(mime: &str) -> Result<String, ExecutorError> {
    let format = match mime {
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/mpeg" => "mp3",
        "audio/opus" => "opus",
        "audio/aac" => "aac",
        "audio/flac" => "flac",
        "audio/ogg" => "ogg",
        other => {
            return Err(ExecutorError::Internal(format!(
                "tts flow output mime '{other}' nie ma mapowania format — \
                 dodaj entry w tts_mime_to_format albo popraw flow"
            )));
        }
    };
    Ok(format.to_string())
}

/// Konwertuje FlowExecutionOutcome na EmbeddingResponse z walidacją
/// cardinality (batch flow z jednym wektorem dla wielu inputów to misconfig).
fn flow_outcome_to_embedding_response(
    outcome: crate::flow_engine::envelope::FlowExecutionOutcome,
    request: &EmbeddingRequest,
    expected_count: usize,
) -> Result<EmbeddingResponse, ExecutorError> {
    let response =
        crate::flow_engine::converter::flow_outcome_to_embedding_response(&outcome, &request.model)
            .map_err(|e| ExecutorError::Internal(format!("{e}")))?;
    if response.data.len() != expected_count {
        return Err(ExecutorError::Internal(format!(
            "flow returned {} embedding(s) for {} input(s) — cardinality mismatch",
            response.data.len(),
            expected_count
        )));
    }
    Ok(response)
}

/// Etap 3a: extract token usage z `ModelMetrics.detailed` gdy backend dostarczył
/// `DetailedMetrics::Completion`. Inny wariant (np. Embeddings dla embeddings
/// stream'a) lub brak `final_metrics` zwraca `None` — chunk wtedy bez `usage`,
/// klient z `include_usage=true` widzi brak (warn'em wpisany w
/// `apply_include_usage_split`).
fn extract_completion_usage(
    metrics: Option<&tentaflow_protocol::ModelMetrics>,
) -> Option<crate::api::openai::types::Usage> {
    use tentaflow_protocol::DetailedMetrics;
    match metrics?.detailed.as_ref()? {
        DetailedMetrics::Completion {
            prompt_tokens,
            completion_tokens,
            total_tokens,
        } => Some(crate::api::openai::types::Usage {
            prompt_tokens: *prompt_tokens,
            completion_tokens: *completion_tokens,
            total_tokens: *total_tokens,
        }),
        _ => None,
    }
}

fn served_by(target: &ResolvedExecutionTarget) -> Option<String> {
    match target {
        ResolvedExecutionTarget::Local { handle, .. } => match handle {
            BackendHandle::Embedded { node_id, .. } => Some(node_id.clone()),
            _ => None,
        },
        ResolvedExecutionTarget::MeshForward { node_id, .. } => Some(node_id.clone()),
        ResolvedExecutionTarget::Flow { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `aborts_fallback_chain` flags only the variants that no fallback
    /// candidate can fix. Everything else lets the executor try the
    /// next candidate; flipping a variant's classification accidentally
    /// would either bury config errors or let transient failures take
    /// down the whole request.
    #[test]
    fn fallback_chain_abort_classification_is_stable() {
        assert!(ExecutorError::FlowDispatcherUnavailable.aborts_fallback_chain());
        // Codex R3b.1 round 2 M1: TransportPendingCutover does NOT abort —
        // we keep iterating later candidates (HTTP/Local may save the
        // request). The cutover error is preserved separately by the
        // dispatch loop and surfaced only if every other candidate fails.
        assert!(!ExecutorError::TransportPendingCutover("x").aborts_fallback_chain());
        // Codex R3b.5+6 H1: SttBackend errors must NOT trigger legacy
        // fallback — that would re-dispatch the same expensive request.
        assert!(ExecutorError::SttBackend("x".into()).aborts_fallback_chain());
        // Codex R3b.5+6 M2: SttRuntimeUnavailable is the **only** STT
        // failure where the caller may try the legacy path.
        assert!(ExecutorError::SttRuntimeUnavailable.aborts_fallback_chain());
        assert!(
            !ExecutorError::AllCandidatesFailed {
                target_kind: "x",
                attempts: 1,
                last_error: "y".into(),
            }
            .aborts_fallback_chain()
        );
        assert!(
            !ExecutorError::Internal("z".into()).aborts_fallback_chain()
        );
        assert!(
            !ExecutorError::FlowEmptyResult { model: "m".into() }
                .aborts_fallback_chain()
        );
    }

    // R3b.1: `dispatch_embeddings_blocking` per-target tests. Branches without
    // network IO (Embedded / MeshForward / Flow) are testable directly; Http
    // and Quic happy paths land in caller-level integration tests w R3b.2.

    fn make_request(model: &str) -> EmbeddingRequest {
        EmbeddingRequest {
            model: model.to_string(),
            input: EmbeddingInput::Single("hello".into()),
            encoding_format: None,
            dimensions: None,
            user: None,
        }
    }

    fn dummy_executor() -> ModelRuntimeExecutor {
        use crate::services::handles_cache::LiveHandlesCache;
        use crate::services::runtime::resolver::AliasResolver;
        let catalog = Arc::new(crate::services::catalog::CatalogProvider::new());
        let handles = Arc::new(LiveHandlesCache::new());
        let resolver = Arc::new(AliasResolver::new_with_static_id(
            handles,
            "local-node".to_string(),
        ));
        let local_inference = Arc::new(
            crate::inference::local::LocalInferenceHandler::new(
                crate::inference::shared_inference_manager(),
            ),
        );
        let stt_slot = Arc::new(parking_lot::RwLock::new(None));
        let mesh_slot = Arc::new(parking_lot::RwLock::new(None));
        ModelRuntimeExecutor::new(
            catalog,
            resolver,
            None,
            local_inference,
            stt_slot,
            mesh_slot,
            Vec::new(),
        )
    }

    /// Embedded branch routes through `LocalInferenceHandler::handle_embeddings`
    /// (Codex R3b.1 fix #2). Without a loaded model the handler bails with a
    /// "no model loaded" error — surfaced as `ExecutorError::Internal`. We
    /// don't assert the message text (handler comment is Polish, may change),
    /// only the typed variant.
    #[tokio::test]
    async fn embeddings_embedded_routes_through_local_inference() {
        let exec = dummy_executor();
        let target = ResolvedExecutionTarget::Local { service_id: 1,
            model_name: "qwen-emb".into(),
            handle: BackendHandle::Embedded {
                model_name: "qwen-emb".into(),
                node_id: "local".into(),
                engine_id: "test-engine".into(),
            },
        };
        let mut ctx = ExecutionContext::default();
        let err = exec
            .dispatch_embeddings_blocking(&target, make_request("qwen-emb"), &mut ctx)
            .await
            .expect_err("no model loaded → handler bails");
        assert!(matches!(err, ExecutorError::Internal(_)));
    }

    #[tokio::test]
    async fn embeddings_mesh_forward_returns_pending_cutover() {
        let exec = dummy_executor();
        let target = ResolvedExecutionTarget::MeshForward {
            node_id: "peer".into(),
            service_id: 1,
            model_name: "qwen-emb".into(),
        };
        let mut ctx = ExecutionContext::default();
        let err = exec
            .dispatch_embeddings_blocking(&target, make_request("qwen-emb"), &mut ctx)
            .await
            .expect_err("mesh_forward branch should be pending cutover");
        assert!(matches!(
            err,
            ExecutorError::TransportPendingCutover("mesh_forward")
        ));
    }

    /// Flow embeddings without a registered FlowDispatcher must surface the
    /// typed `FlowDispatcherUnavailable` error so the caller knows the
    /// router was constructed DB-less, not that the flow itself failed.
    #[tokio::test]
    async fn embeddings_flow_without_dispatcher_returns_typed_error() {
        let exec = dummy_executor();
        let target = ResolvedExecutionTarget::Flow {
            flow_id: 1,
            published_name: "embed-flow".into(),
        };
        let mut ctx = ExecutionContext::default();
        let err = exec
            .dispatch_embeddings_blocking(&target, make_request("any"), &mut ctx)
            .await
            .expect_err("flow without dispatcher should be a typed error");
        assert!(matches!(err, ExecutorError::FlowDispatcherUnavailable));
    }

    fn outcome_with_payload(
        payload: crate::flow_engine::envelope::FlowValue,
    ) -> crate::flow_engine::envelope::FlowExecutionOutcome {
        let mut env = crate::flow_engine::envelope::FlowEnvelope::empty();
        env.payload = payload;
        crate::flow_engine::envelope::FlowExecutionOutcome {
            final_envelope: env,
            trace: vec![],
            usage: crate::flow_engine::envelope::TokenUsage::default(),
            finish_reason: crate::flow_engine::envelope::FinishReason::Stop,
            total_latency_ms: 0,
            error: None,
        }
    }

    fn batch_request(model: &str, count: usize) -> EmbeddingRequest {
        EmbeddingRequest {
            model: model.to_string(),
            input: EmbeddingInput::Multiple(
                (0..count).map(|i| format!("text-{i}")).collect(),
            ),
            encoding_format: None,
            dimensions: None,
            user: None,
        }
    }

    /// Single-vector outcome trafia do `data[0]` z `index=0`.
    #[test]
    fn flow_outcome_extracts_single_embedding_for_single_input() {
        let request = make_request("any");
        let outcome = outcome_with_payload(crate::flow_engine::envelope::FlowValue::Embedding(
            vec![0.1, 0.2, 0.3],
        ));
        let resp = flow_outcome_to_embedding_response(outcome, &request, 1).expect("single ok");
        assert_eq!(resp.data.len(), 1);
        assert_eq!(resp.data[0].embedding.len(), 3);
    }

    /// Batch JSON `{ "embeddings": [[..],[..]] }` mapuje na `data[]` z
    /// `index` 0..n.
    #[test]
    fn flow_outcome_extracts_batched_embeddings_for_batched_input() {
        let request = batch_request("any", 2);
        let outcome = outcome_with_payload(crate::flow_engine::envelope::FlowValue::Json(
            serde_json::json!({ "embeddings": [[0.1], [0.2]] }),
        ));
        let resp = flow_outcome_to_embedding_response(outcome, &request, 2).expect("batched ok");
        assert_eq!(resp.data.len(), 2);
    }

    /// Cardinality mismatch (1 wektor dla 3 inputów) zwraca Internal — silent
    /// collapse byłby ukrytym misconfigiem flow.
    #[test]
    fn flow_outcome_cardinality_mismatch_returns_internal() {
        let request = batch_request("any", 3);
        let outcome = outcome_with_payload(crate::flow_engine::envelope::FlowValue::Json(
            serde_json::json!({ "embeddings": [[0.1]] }),
        ));
        let err = flow_outcome_to_embedding_response(outcome, &request, 3)
            .expect_err("1 embedding for 3 inputs must reject");
        assert!(matches!(err, ExecutorError::Internal(_)));
    }

    fn make_tts_request(model: &str) -> TTSRequest {
        TTSRequest {
            model: model.to_string(),
            input: "hello world".to_string(),
            voice: "alloy".to_string(),
            response_format: Some("wav".to_string()),
            speed: Some(1.0),
            language: Some("en".to_string()),
        }
    }

    #[tokio::test]
    async fn tts_mesh_forward_returns_pending_cutover() {
        let exec = dummy_executor();
        let target = ResolvedExecutionTarget::MeshForward {
            node_id: "peer".into(),
            service_id: 1,
            model_name: "tts".into(),
        };
        let mut ctx = ExecutionContext::default();
        let err = exec
            .dispatch_tts_blocking(&target, make_tts_request("tts"), &mut ctx)
            .await
            .expect_err("mesh_forward branch should be pending cutover");
        assert!(matches!(
            err,
            ExecutorError::TransportPendingCutover("mesh_forward")
        ));
    }

    /// Etap 2: TTS-as-flow path działa, ale dummy_executor nie ma
    /// FlowDispatcher (Router::new go tworzy). Bez dispatchera dostajemy
    /// `FlowDispatcherUnavailable`, nie `Internal('not supported')`.
    #[tokio::test]
    async fn tts_flow_without_dispatcher_returns_typed_error() {
        let exec = dummy_executor();
        let target = ResolvedExecutionTarget::Flow {
            flow_id: 1,
            published_name: "tts-flow".into(),
        };
        let mut ctx = ExecutionContext::default();
        let err = exec
            .dispatch_tts_blocking(&target, make_tts_request("any"), &mut ctx)
            .await
            .expect_err("flow without dispatcher should be a typed error");
        assert!(matches!(err, ExecutorError::FlowDispatcherUnavailable));
    }

    /// Codex R3b.5+6 L4: direct test for `execute_stt` when no SttRuntime
    /// is wired. The thin delegate must surface the typed
    /// `SttRuntimeUnavailable` variant so the caller's narrow fallback
    /// logic can distinguish it from real backend errors.
    #[tokio::test]
    async fn execute_stt_without_runtime_returns_unavailable() {
        let exec = dummy_executor();
        let request = TranscriptionRequest {
            file: std::sync::Arc::from(vec![0u8, 1, 2, 3].into_boxed_slice()),
            filename: "x.wav".into(),
            model: "whisper-1".into(),
            language: None,
            prompt: None,
            response_format: None,
            temperature: None,
            timestamp_granularities: None,
            no_speech_threshold: None,
            avg_logprob_threshold: None,
            compression_ratio_threshold: None,
            options: crate::api::openai::types::SttRequestOptions::default(),
        };
        let mut ctx = crate::services::runtime::context::ExecutionContext::default();
        let err = exec
            .execute_stt(request, &mut ctx)
            .await
            .expect_err("no STT runtime → typed error");
        assert!(matches!(err, ExecutorError::SttRuntimeUnavailable));
    }

    #[test]
    fn samples_to_wav_pcm16_emits_riff_header() {
        let wav = samples_to_wav_pcm16(&[0.0, 0.5, -0.5], 16_000);
        assert!(wav.len() > 44);
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        // PCM format = 1, mono = 1
        assert_eq!(u16::from_le_bytes([wav[20], wav[21]]), 1);
        assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 1);
        // sample rate
        assert_eq!(
            u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]),
            16_000
        );
    }

    /// NaN / Inf in embedding payload break downstream cosine similarity
    /// silently — reject at parse time so the operator sees a clear error.
    #[test]
    fn flow_outcome_rejects_non_numeric_batch_entries() {
        let request = make_request("any");
        let outcome = outcome_with_payload(crate::flow_engine::envelope::FlowValue::Json(
            serde_json::json!({ "embeddings": [[0.1, "NaN", 0.3]] }),
        ));
        let err = flow_outcome_to_embedding_response(outcome, &request, 1)
            .expect_err("non-numeric entry rejects");
        assert!(matches!(err, ExecutorError::Internal(_)));
    }
}

