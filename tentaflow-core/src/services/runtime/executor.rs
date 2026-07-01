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
    EmbeddingInput, EmbeddingRequest, EmbeddingResponse, EmbeddingUsage, RerankRequest,
    RerankResponse, RerankResultEntry, TTSRequest, TranscriptionRequest, TranscriptionResponse,
};
use crate::error::Result as CoreResult;
use crate::flow_engine::dispatcher::FlowDispatcher;
use crate::services::catalog::{CatalogProvider, InputModality, OutputModality, ServiceSurface};
use crate::services::handles_cache::BackendHandle;
use crate::services::runtime::context::ExecutionContext;
use crate::services::runtime::resolver::{AliasResolver, ResolveError, ResolveRequest};
use crate::services::runtime::strategy::{rank, StrategyState};
use crate::services::runtime::target::ResolvedExecutionTarget;
use crate::services::runtime::tool_calling::{self, ToolCallMode};

/// Strumien chunkow zwracany przez `stream_chat`. Boxed `Pin<Box<dyn Stream>>`
/// zeby caller mog go zapakowac w SSE bez wiedzy o konkretnym typie strumienia
/// (kazdy backend transport produkuje inny typ wewnetrznie).
pub type ExecutorChunkStream = Pin<Box<dyn Stream<Item = CoreResult<ChatCompletionChunk>> + Send>>;

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
            Self::FlowDispatcherUnavailable | Self::SttRuntimeUnavailable | Self::SttBackend(_)
        )
    }
}

/// Outcome of the `tool_call_mode` deployment-config lookup. `Failed` stays
/// distinct from `NoOverride` so a transient DB failure never silently flips
/// an explicitly-prompt HTTP service to native tool delivery.
#[derive(Debug, Clone, Copy)]
enum ToolCallModeLookup {
    Mode(ToolCallMode),
    NoOverride,
    Failed,
}

/// RAG E1.2 — żądanie sparsowania obrazu strony dokumentu na markdown+bloki.
/// Nieść surowe bajty obrazu (`image_bytes`) + ich `mime`; `model` to alias
/// (domyślnie `rag-parse`) albo konkretna nazwa serwisu vision-parse. Pola
/// Tożsamość callera trafia do flow-targetu przez `ExecutionContext::user`
/// (nie przez pola request), więc tu trzymamy tylko payload + `flow_depth`,
/// który dziedziczy głębokość zagnieżdżenia z caller-flow (guard rekursji).
#[derive(Debug, Clone)]
pub struct DocumentParseRequest {
    pub model: String,
    pub image_bytes: Vec<u8>,
    pub mime: String,
    pub flow_depth: u8,
}

/// RAG E1.2 — jeden blok layoutu zwrócony przez serwis parsujący. `bbox` to
/// `[x1, y1, x2, y2]` w pikselach oryginału; `confidence` puste gdy backend go
/// nie raportuje. `page` 0-bazowy (zawsze 0 dla pojedynczego obrazu).
#[derive(Debug, Clone, PartialEq)]
pub struct DocBlock {
    pub page: u32,
    pub class: String,
    pub bbox: [f32; 4],
    pub text: String,
    pub confidence: Option<f32>,
}

/// RAG E1.2 — odpowiedź parsera: pełny markdown strony + rozbicie na bloki.
/// `usage` to opcjonalny licznik tokenów raportowany przez backend (vision
/// parse services zwykle go nie zwracają → `None`).
#[derive(Debug, Clone)]
pub struct DocumentParseResponse {
    pub markdown: String,
    pub blocks: Vec<DocBlock>,
    pub usage: Option<crate::api::openai::types::Usage>,
}

/// RAG Partia 3 (prerequisite) — żądanie ingestu JEDNEGO dokumentu jako flow.
/// Niesie surowe bajty pliku (PDF/xlsx/docx/obraz — pobrane przez host fn z
/// per-instance document store), `mime`, nazwę flow `model` (`<model>:ingest`)
/// oraz `options` (collection_id, graph toggle, parametry chunkingu) wstrzykiwane
/// do flow.meta. `flow_depth` dziedziczy głębokość zagnieżdżenia (guard rekursji,
/// jak `DocumentParseRequest`). To JEDYNA ścieżka wywołania flow-ingestu z
/// binarnym dokumentem — Partia 3 podmieni `run_ingest_pipeline` na to wywołanie.
#[derive(Debug, Clone)]
pub struct IngestRequest {
    pub model: String,
    pub document_bytes: Vec<u8>,
    pub mime: String,
    pub options: serde_json::Map<String, serde_json::Value>,
    pub flow_depth: u8,
}

/// RAG Partia 3 — wynik flow-ingestu: markdown rekonstrukcji dokumentu + liczba
/// chunków zapisanych do indeksu wektorowego przez węzeł store flow. `page_count`
/// = liczba stron (1 dla pojedynczego obrazu). Flow zwraca to jako
/// `Json{markdown, chunks, page_count}` w finalnym envelope.
#[derive(Debug, Clone)]
pub struct IngestResponse {
    pub markdown: String,
    pub chunks: u32,
    pub page_count: u32,
}

/// PARTIA 0 — żądanie detekcji struktury dokumentu przez typed surface
/// `Documents` (`/v1/infer`). Niesie surowe bajty obrazu strony + `mime` + `task`
/// (jeden z detektorów: page_elements / table_structure / graphic_elements / ocr).
/// `flow_depth` dziedziczy głębokość zagnieżdżenia z caller-flow (guard rekursji,
/// jak `DocumentParseRequest`). To fundament node-adapterów flow-ingestu RAG.
#[derive(Debug, Clone)]
pub struct DocumentInferRequest {
    pub model: String,
    pub image_bytes: Vec<u8>,
    pub mime: String,
    pub task: String,
    pub flow_depth: u8,
}

/// PARTIA 0 — odpowiedź detektora: lista wykrytych regionów strony (layout boxes,
/// komórki tabel, grafika, OCR spany). Reużywa typów drutu z `tentaflow_protocol`,
/// bo ta sama struktura leci przez mesh (`ModelResult::Documents`).
#[derive(Debug, Clone)]
pub struct DocumentInferResponse {
    pub regions: Vec<tentaflow_protocol::DocRegion>,
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
    mesh_manager: Arc<parking_lot::RwLock<Option<Arc<crate::mesh::iroh_manager::IrohMeshManager>>>>,
    /// Per-alias round-robin state keyed by alias name. `DashMap` so we
    /// can mutate per-key without serialising the whole map.
    strategy_state: Arc<dashmap::DashMap<String, Arc<StrategyState>>>,
    /// Lazy-load + memory guard dla embedded modeli (unpinned). Planted przez
    /// `Router::start` (potrzebuje db+ports). `None` na nodach bez residency —
    /// wtedy `ensure_resident` to no-op (model musi byc juz zaladowany jak dotad).
    model_residency:
        Arc<parking_lot::RwLock<Option<Arc<crate::services::model_residency::ModelResidency>>>>,
    /// DbPool dla czyszczenia tekstu TTS (`clean_cache::clean` — substytucja z
    /// `tts_cleaning_rules` + strip emoji/punktuacji). `None` w testach DB-less.
    db: Option<crate::db::DbPool>,
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
        model_residency: Arc<
            parking_lot::RwLock<Option<Arc<crate::services::model_residency::ModelResidency>>>,
        >,
        db: Option<crate::db::DbPool>,
    ) -> Self {
        Self {
            catalog,
            resolver,
            flow_dispatcher,
            local_inference,
            stt_runtime,
            mesh_manager,
            strategy_state: Arc::new(dashmap::DashMap::new()),
            model_residency,
            db,
        }
    }

    /// Lazy-load embedded model do pamieci przed dispatch (no-op gdy residency
    /// nie podpiete, target nie jest Local/Embedded, albo serwis pinned/nie-embedded).
    async fn ensure_resident(&self, target: &ResolvedExecutionTarget) -> Result<(), ExecutorError> {
        if let ResolvedExecutionTarget::Local {
            model_name, handle, ..
        } = target
        {
            if matches!(handle, BackendHandle::Embedded { .. }) {
                let residency = self.model_residency.read().clone();
                if let Some(res) = residency {
                    res.ensure_loaded(model_name)
                        .await
                        .map_err(|e| ExecutorError::Internal(e.to_string()))?;
                }
            }
        }
        Ok(())
    }

    /// Wspolne wymiary tozsamosci (writer + tenant + user) dla metryk modelu.
    /// `node_id` to ZAWSZE lokalny wezel (single-writer per row — kazdy wezel
    /// zapisuje wlasne wiersze). `org`/`user` z `ctx`, z sentinelami jak reszta
    /// hot-path telemetrii (`org-default` / `__system__`).
    fn metric_identity(&self, ctx: &ExecutionContext) -> (String, String, String) {
        let node_id = self.resolver.local_node_id();
        let org_id = ctx
            .org_id
            .as_deref()
            .unwrap_or(crate::services::org::DEFAULT_ORG_ID)
            .to_string();
        let user_id = ctx
            .user
            .as_ref()
            .map(|u| u.user_id.clone())
            .unwrap_or_else(|| crate::db::repository::TOKEN_USAGE_SYSTEM_USER.to_string());
        (node_id, org_id, user_id)
    }

    /// Metryki jednego OBSLUZONEGO LOKALNIE requestu (sukces). MeshForward i Flow
    /// sa POMIJANE: MeshForward realnie wykonuje zdalny wezel (jego executor
    /// zapisuje metryke pod swoim `node_id` — brak double-count na koordynatorze),
    /// a Flow ma wlasne wpiecie z bogatszym `perf` z `FlowExecutionOutcome`.
    fn record_served_metrics(
        &self,
        target: &ResolvedExecutionTarget,
        ctx: &ExecutionContext,
        modality: &str,
        usage: Option<&crate::api::openai::types::Usage>,
        perf: Option<&crate::api::openai::types::GenPerf>,
        e2e_ms: Option<u32>,
    ) {
        let Some(db) = self.db.as_ref() else {
            return;
        };
        if matches!(
            target,
            ResolvedExecutionTarget::MeshForward { .. } | ResolvedExecutionTarget::Flow { .. }
        ) {
            return;
        }
        let (node_id, org_id, user_id) = self.metric_identity(ctx);
        let backend = target.telemetry_tag();
        let model_id = target.requested_model();
        let service_key = format!("{backend}/{model_id}");
        let is_embedding = modality == "embedding";
        let prompt_tokens = usage.map(|u| u.prompt_tokens as i64).unwrap_or(0);
        let completion_tokens = usage.map(|u| u.completion_tokens as i64).unwrap_or(0);
        let total_tokens = usage.map(|u| u.total_tokens as i64).unwrap_or(0);
        let e2e_latency_ms = e2e_ms
            .map(|v| v as i64)
            .or_else(|| perf.map(|p| p.total_ms as i64))
            .unwrap_or(0);
        bump_model_metric_row(
            db,
            &ModelMetricInput {
                node_id: &node_id,
                org_id: &org_id,
                user_id: &user_id,
                model_id,
                service_key: &service_key,
                backend,
                modality,
                prompt_tokens,
                completion_tokens,
                total_tokens,
                embedding_tokens: if is_embedding { total_tokens } else { 0 },
                e2e_latency_ms,
                ttft_sample: perf.and_then(|p| (p.ttft_ms > 0).then_some(p.ttft_ms as i64)),
                decode_tps_sample: perf
                    .and_then(|p| (p.decode_tps > 0.0).then_some(p.decode_tps as f64)),
                e2e_sample: (e2e_latency_ms > 0).then_some(e2e_latency_ms),
                is_error: false,
            },
        );
    }

    /// Metryki zakonczonego-bledem requestu. Wolane RAZ, gdy `execute_*` zwraca
    /// `Err` po wyczerpaniu wszystkich kandydatow (nie per-proba retry). `backend`
    /// to tag ostatniego probowanego kandydata; brak tokenow/perf.
    fn record_error_metrics(
        &self,
        ctx: &ExecutionContext,
        model_id: &str,
        backend: &str,
        modality: &str,
    ) {
        let Some(db) = self.db.as_ref() else {
            return;
        };
        let (node_id, org_id, user_id) = self.metric_identity(ctx);
        let service_key = format!("{backend}/{model_id}");
        bump_model_metric_row(
            db,
            &ModelMetricInput {
                node_id: &node_id,
                org_id: &org_id,
                user_id: &user_id,
                model_id,
                service_key: &service_key,
                backend,
                modality,
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                embedding_tokens: 0,
                e2e_latency_ms: 0,
                ttft_sample: None,
                decode_tps_sample: None,
                e2e_sample: None,
                is_error: true,
            },
        );
    }

    /// Buduje lekki recorder metryk dla streamu (Local Http/Quic). Wymiary
    /// klonowane do `String`, bo `ExternalPerfStream` drenuje w osobnym tasku bez
    /// dostepu do `target`/`ctx`. `None` gdy brak db albo target nie jest lokalny.
    fn stream_recorder(
        &self,
        target: &ResolvedExecutionTarget,
        ctx: &ExecutionContext,
    ) -> Option<StreamMetricsRecorder> {
        let db = self.db.as_ref()?.clone();
        if matches!(
            target,
            ResolvedExecutionTarget::MeshForward { .. } | ResolvedExecutionTarget::Flow { .. }
        ) {
            return None;
        }
        let (node_id, org_id, user_id) = self.metric_identity(ctx);
        let backend = target.telemetry_tag();
        let model_id = target.requested_model().to_string();
        let service_key = format!("{backend}/{model_id}");
        Some(StreamMetricsRecorder {
            db,
            node_id,
            org_id,
            user_id,
            model_id,
            service_key,
            backend: backend.to_string(),
        })
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
        // Znacznik e2e (dispatch -> odpowiedz) dla metryk modelu.
        let dispatch_at = std::time::Instant::now();
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
            match self
                .dispatch_chat_blocking(&target, request.clone(), ctx)
                .await
            {
                Ok(response) => {
                    ctx.route_metadata.served_by_node = served_by(&target);
                    ctx.route_metadata.served_model = Some(target.requested_model().to_string());
                    ctx.route_metadata.backend_type = Some(target.telemetry_tag().to_string());
                    ctx.route_metadata.fallbacks_tried = (attempts - 1) as u32;
                    note_fallback(
                        &request.model,
                        outcome.requested_is_alias,
                        attempts,
                        target.telemetry_tag(),
                    );
                    let e2e_ms = dispatch_at.elapsed().as_millis() as u32;
                    self.record_served_metrics(
                        &target,
                        ctx,
                        "chat",
                        response.usage.as_ref(),
                        None,
                        Some(e2e_ms),
                    );
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

        // Request zakonczony bledem po wyczerpaniu wszystkich kandydatow — jeden
        // wpis error na koordynatorze (nie per-proba). Dla MeshForward NIE
        // zapisujemy error: jesli zdalny wezel dostal request, sam zapisal swoj
        // wiersz error pod swoim node_id (analogicznie do pomijania success dla
        // MeshForward). Jesli zdalny byl NIEOSIAGALNY (request nigdy nie dotarl),
        // error nie zostanie policzony nigdzie — akceptujemy to jako mniejsze zlo
        // niz inflacja error-rate podwojnym liczeniem.
        if last_kind != "mesh_forward" {
            self.record_error_metrics(ctx, &request.model, last_kind, "chat");
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
            match self
                .dispatch_chat_stream(&target, request.clone(), ctx)
                .await
            {
                Ok(stream) => {
                    // One line per stream-chat answering "where did this
                    // request ACTUALLY go" — requested model vs resolved
                    // target (alias fallbacks can silently swap a remote
                    // model for a local one and this is the only place that
                    // sees the final decision).
                    tracing::info!(
                        requested_model = %request.model,
                        target = ?target,
                        attempts,
                        "chat stream dispatch routed"
                    );
                    ctx.route_metadata.served_by_node = served_by(&target);
                    ctx.route_metadata.served_model = Some(target.requested_model().to_string());
                    ctx.route_metadata.backend_type = Some(target.telemetry_tag().to_string());
                    ctx.route_metadata.fallbacks_tried = (attempts - 1) as u32;
                    note_fallback(
                        &request.model,
                        outcome.requested_is_alias,
                        attempts,
                        target.telemetry_tag(),
                    );
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

        self.ensure_resident(target).await?;
        if let ResolvedExecutionTarget::Local { model_name, .. } = target {
            if request.model != *model_name {
                request.model = model_name.clone();
            }
        }
        request.stream = true;
        // Prompt-mode tool calling is intentionally NOT applied on the
        // streaming path: intercepting `<tool_call>` tags mid-stream ships
        // with the agent-loop phases, and today's tool-sending callers use
        // the blocking path. Native (HTTP) candidates serialize `tools` in
        // the request body and their tool-call deltas pass through untouched.

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
                    // Wymuszamy `include_usage` upstream — bez niego vLLM/sglang
                    // nie emituje finalnego chunku z usage, więc nie znalibyśmy
                    // prompt/completion tokenów do wall-clockowego perf.
                    request.stream_options = Some(crate::api::openai::types::StreamOptions {
                        include_usage: true,
                    });
                    // Zegar dispatchu sprzed wysłania HTTP — TTFT/total liczone od
                    // realnego startu (wysłanie requestu + odbiór nagłówków), nie od
                    // spawnu wrappera, który następuje już po `await`.
                    let dispatch_at = std::time::Instant::now();
                    let recorder = self.stream_recorder(target, ctx);
                    let stream = client
                        .chat_completion_stream(request)
                        .await
                        .map_err(|e| ExecutorError::Internal(e.to_string()))?;
                    Ok(Box::pin(ExternalPerfStream::new(
                        stream,
                        dispatch_at,
                        recorder,
                    )))
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
                    // Zegar dispatchu sprzed wysłania QUIC — TTFT/total od realnego
                    // startu requestu, nie od spawnu wrappera po `await`.
                    let dispatch_at = std::time::Instant::now();
                    let recorder = self.stream_recorder(target, ctx);
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
                                    StreamChunkType::TextDelta(text) => {
                                        Some(Ok(ChatCompletionChunk {
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
                                            perf: None,
                                        }))
                                    }
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
                                            perf: None,
                                        }))
                                    }
                                    StreamChunkType::ToolCallDelta(tc) => {
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
                                                    tool_calls: Some(vec![
                                                        crate::routing::stream_helpers::protocol_tool_call_delta_to_openai(tc),
                                                    ]),
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
                                            perf: None,
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
                                        let usage =
                                            extract_completion_usage(final_metrics.as_ref());
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
                                            perf: None,
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
                    Ok(Box::pin(ExternalPerfStream::new(
                        Box::pin(stream),
                        dispatch_at,
                        recorder,
                    )))
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
                let mesh =
                    self.mesh_manager.read().clone().ok_or_else(|| {
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
                let payload = tentaflow_protocol::cbor::encode(&model_request).map_err(|e| {
                    ExecutorError::Internal(format!("mesh stream serialize ModelRequest: {}", e))
                })?;
                let frame_stream = mesh
                    .forward_stream_request(node_id, &request_id, payload)
                    .await
                    .map_err(|e| {
                        ExecutorError::Internal(format!("mesh forward stream request: {}", e))
                    })?;
                let backend_url = format!("mesh://{}", node_id);
                let protocol_stream = frame_stream.map(move |frame_result| {
                    let frame =
                        frame_result.map_err(|e| crate::error::CoreError::NetworkError {
                            message: format!("mesh stream read: {}", e),
                            source: e,
                        })?;
                    tentaflow_protocol::cbor::decode::<ModelStreamChunk>(&frame).map_err(|e| {
                        crate::error::CoreError::BackendError {
                            backend_url: backend_url.clone(),
                            message: format!("mesh stream deserialize ModelStreamChunk: {}", e),
                            source: None,
                        }
                    })
                });
                Ok(
                    crate::routing::stream_helpers::quic_stream_to_openai_chunks(
                        protocol_stream,
                        target_model,
                    ),
                )
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
            (true, true) => &[
                InputModality::Audio,
                InputModality::Image,
                InputModality::Text,
            ],
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

    /// HARNESS_PLAN §3.1: native/prompt tool-calling decision, per candidate.
    /// An explicit `tool_call_mode` from the service deployment config wins
    /// where the transport supports it; otherwise OpenAI-compatible HTTP
    /// backends default to native and everything else uses prompt mode,
    /// because the embedded engine and the QUIC/mesh `CompletionPayload`
    /// wire cannot carry `tools`. A FAILED config lookup (not "no override")
    /// falls back to prompt mode even on HTTP: prompt works on every chat
    /// backend, while defaulting to native would leak `tools` natively past
    /// a service the operator explicitly pinned to prompt.
    async fn tool_call_mode_for(&self, target: &ResolvedExecutionTarget) -> ToolCallMode {
        let lookup = self.explicit_tool_call_mode(target).await;
        let is_http = matches!(
            target,
            ResolvedExecutionTarget::Local {
                handle: BackendHandle::Http(_),
                ..
            }
        );
        if is_http {
            match lookup {
                ToolCallModeLookup::Mode(mode) => mode,
                ToolCallModeLookup::NoOverride => ToolCallMode::Native,
                ToolCallModeLookup::Failed => {
                    tracing::warn!(
                        target_kind = target.telemetry_tag(),
                        "tool_call_mode config lookup failed; using prompt mode for this request"
                    );
                    ToolCallMode::Prompt
                }
            }
        } else {
            if matches!(lookup, ToolCallModeLookup::Mode(ToolCallMode::Native)) {
                tracing::warn!(
                    target_kind = target.telemetry_tag(),
                    "tool_call_mode=native is unsupported on this transport; using prompt mode"
                );
            }
            ToolCallMode::Prompt
        }
    }

    /// Reads the optional top-level `tool_call_mode` key from the candidate
    /// service's `services.config_json` (same store the deploy wizard writes
    /// — mirror of how `request_time_parameters` is persisted). Only Local
    /// candidates have a local service row; mesh forwards resolve the mode
    /// on the owning node and flows never consult this.
    async fn explicit_tool_call_mode(
        &self,
        target: &ResolvedExecutionTarget,
    ) -> ToolCallModeLookup {
        let Some(service_id) = target.local_service_id() else {
            return ToolCallModeLookup::NoOverride;
        };
        // DB-less router: no config store exists, so no override can exist.
        let Some(db) = self.db.clone() else {
            return ToolCallModeLookup::NoOverride;
        };
        tokio::task::spawn_blocking(move || {
            let Ok(conn) = db.read() else {
                return ToolCallModeLookup::Failed;
            };
            let svc = match crate::services_repo::services::get(&conn, service_id) {
                Ok(Some(svc)) => svc,
                // Service row deleted mid-flight: no config row, no override.
                Ok(None) => return ToolCallModeLookup::NoOverride,
                Err(_) => return ToolCallModeLookup::Failed,
            };
            let Ok(cfg) = serde_json::from_str::<serde_json::Value>(&svc.config_json) else {
                return ToolCallModeLookup::Failed;
            };
            match cfg.get("tool_call_mode").and_then(|v| v.as_str()) {
                Some(s) => ToolCallMode::from_config_str(s)
                    .map_or(ToolCallModeLookup::NoOverride, ToolCallModeLookup::Mode),
                None => ToolCallModeLookup::NoOverride,
            }
        })
        .await
        .unwrap_or(ToolCallModeLookup::Failed)
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
        self.ensure_resident(target).await?;
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
        // HARNESS_PLAN §3.1: prompt-mode candidates get the tool section moved
        // into the system prompt BEFORE transport-specific dispatch (so the
        // section also crosses the QUIC/mesh message wire) and the completion
        // parsed for `<tool_call>` blocks afterwards. Flow targets keep the
        // request untouched — flows compose their own tool wiring (phase 3).
        let prompt_tools = if matches!(target, ResolvedExecutionTarget::Flow { .. }) {
            None
        } else if request.tools.as_ref().is_some_and(|t| !t.is_empty())
            && self.tool_call_mode_for(target).await == ToolCallMode::Prompt
        {
            tool_calling::apply_prompt_mode_request(&mut request)
        } else {
            None
        };
        let mut response = match target {
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
                BackendHandle::Quic(handle) => Self::dispatch_chat_quic(handle, request).await,
            },
            ResolvedExecutionTarget::MeshForward {
                node_id,
                model_name,
                ..
            } => {
                use tentaflow_protocol::*;
                // Vision-chat (np. nemotron-parse): CompletionPayload niesie tylko
                // tekst, więc obraz zgubiłby się na hopie mesh. Gdy request niesie
                // obraz, forwardujemy jako ModelPayload::Vision (peer ma ramię
                // Vision -> route_vision_via_protocol). Inaczej zwykły Completion.
                let payload = if crate::routing::messages_have_image(&request.messages) {
                    ModelPayload::Vision(VisionPayload {
                        model: model_name.clone(),
                        messages: crate::routing::openai_messages_to_vision(&request.messages),
                        max_tokens: request.max_tokens,
                        temperature: request.temperature,
                    })
                } else {
                    ModelPayload::Completion(CompletionPayload {
                        model: model_name.clone(),
                        prompt: None,
                        messages: crate::routing::openai_messages_to_protocol(&request.messages),
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
                    })
                };
                let model_request = ModelRequest {
                    request_id: uuid::Uuid::new_v4().to_string(),
                    payload,
                    stream: false,
                    metadata: None,
                    session_id: None,
                };
                let response = self.forward_via_mesh(node_id, model_request, ctx).await?;
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
                published_name: _,
            } => {
                let dispatcher = self
                    .flow_dispatcher
                    .as_ref()
                    .ok_or(ExecutorError::FlowDispatcherUnavailable)?;
                ctx.enter_flow(flow_id)
                    .map_err(|e| ExecutorError::Internal(format!("flow recursion limit: {}", e)))?;

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
                    let blobs = dispatcher.blobs();
                    let (initial, mut meta) =
                        crate::routing::build_initial_envelope_for_user(&request, user, &blobs)
                            .await
                            .map_err(|e| ExecutorError::Internal(format!("envelope seed: {e}")))?;
                    // RAG E1.0 — przeprowadź tożsamość addona-callera do flow.
                    meta.addon_id = ctx.addon_id.clone();
                    meta.org_id = ctx.org_id.clone();
                    dispatcher
                        .dispatch_by_flow_id(flow_id.clone(), initial, meta)
                        .await
                };
                ctx.leave_flow();

                let outcome =
                    dispatch_result.map_err(|e| ExecutorError::Internal(e.to_string()))?;

                // No flow-level metric row here: the flow's internal LLM node
                // calls back into `ModelRuntimeExecutor::execute_chat`, which
                // records the real per-model row with identical tokens+perf.
                // Recording a `flow_engine` row too would double-count tokens.
                Ok(crate::routing::chat::flow_outcome_to_chat_response(
                    outcome,
                    &request.model,
                ))
            }
        }?;
        if let Some(tools) = &prompt_tools {
            tool_calling::apply_prompt_mode_response(&mut response, tools);
        }
        Ok(response)
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

        ctx.enter_hop()
            .map_err(|e| ExecutorError::Internal(format!("mesh forward hop limit: {}", e)))?;

        let mesh = self
            .mesh_manager
            .read()
            .clone()
            .ok_or_else(|| ExecutorError::TransportPendingCutover("mesh_forward"))?;

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
        let payload = tentaflow_protocol::cbor::encode(&model_request)
            .map_err(|e| ExecutorError::Internal(format!("mesh forward serialize: {}", e)))?;
        let response_bytes = mesh
            .forward_request(target_node_id, &request_id, payload)
            .await
            .map_err(|e| ExecutorError::Internal(format!("mesh forward request: {}", e)))?;
        tentaflow_protocol::cbor::decode::<ModelResponse>(&response_bytes).map_err(|e| {
            ExecutorError::Internal(format!("mesh forward deserialize ModelResponse: {}", e))
        })
    }

    /// QUIC sidecar dispatch (R2a). Wczesniej zwracalo
    /// `TransportPendingCutover`; teraz buduje `ModelRequest::Completion`,
    /// wysyla przez `Arc<QuicClient>` z handle'u, mapuje response na
    /// `ChatCompletionResponse`. Logika lustro `chat.rs::route_to_quic_llm`
    /// — PII cleaning idzie przez flow_engine `pii_filter` node, executor
    /// pozostaje agnostic.
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
        let dispatch_at = std::time::Instant::now();
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
                    ctx.route_metadata.served_model = Some(target.requested_model().to_string());
                    ctx.route_metadata.backend_type = Some(target.telemetry_tag().to_string());
                    ctx.route_metadata.fallbacks_tried = (attempts - 1) as u32;
                    note_fallback(
                        &request.model,
                        outcome.requested_is_alias,
                        attempts,
                        target.telemetry_tag(),
                    );
                    let e2e_ms = dispatch_at.elapsed().as_millis() as u32;
                    let usage = crate::api::openai::types::Usage {
                        prompt_tokens: response.usage.prompt_tokens,
                        completion_tokens: 0,
                        total_tokens: response.usage.total_tokens,
                    };
                    self.record_served_metrics(
                        &target,
                        ctx,
                        "embedding",
                        Some(&usage),
                        None,
                        Some(e2e_ms),
                    );
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

        // MeshForward error jest zapisywany przez zdalny wezel pod jego node_id
        // (jesli request dotarl); koordynator go pomija, zeby nie liczyc dwa razy.
        // Zdalny nieosiagalny → error niepoliczony nigdzie (mniejsze zlo).
        if last_kind != "mesh_forward" {
            self.record_error_metrics(ctx, &request.model, last_kind, "embedding");
        }
        Err(ExecutorError::AllCandidatesFailed {
            target_kind: last_kind,
            attempts,
            last_error: last_err.unwrap_or_else(|| "no candidates after rank".into()),
        })
    }

    /// Rozwiązuje `model` dla zadanej powierzchni/modalności i zwraca pierwszy
    /// żywy kandydat po rankingu. Używane przez reverse-proxy endpointy
    /// (`/v1/infer`, `/v1/rerank`), które nie reserializują odpowiedzi do
    /// typowanej struktury, tylko potrzebują adresu docelowego serwisu. Cała
    /// logika katalogu/aliasów/ACL-modalności jest współdzielona z resztą
    /// dispatchu — nie duplikujemy resolvera. Zwracamy `ResolvedExecutionTarget`,
    /// bo to on niesie `service_id` (Local) albo `node_id` (MeshForward),
    /// których caller potrzebuje do wyciągnięcia `endpoint_url` ze snapshotu
    /// serwisów.
    pub fn resolve_proxy_target(
        &self,
        model: &str,
        surface: ServiceSurface,
        input_modalities: &[InputModality],
        ctx: &mut ExecutionContext,
    ) -> Result<ResolvedExecutionTarget, ExecutorError> {
        let outcome = {
            let snapshot = self.catalog.snapshot();
            let req = ResolveRequest {
                requested_model: model,
                required_surface: surface,
                required_input_modalities: input_modalities,
                required_output_modalities: &[],
            };
            self.resolver.resolve(&req, &snapshot, ctx)?
        };

        let state = self.strategy_state_for(model);
        let ranked = rank(&outcome.candidates, outcome.strategy, &state);

        ranked
            .into_iter()
            .find(|t| t.is_alive())
            .ok_or(ExecutorError::AllCandidatesFailed {
                target_kind: "unknown",
                attempts: 0,
                last_error: "no live candidate after rank".into(),
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

        self.ensure_resident(target).await?;
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
                let response = self.forward_via_mesh(node_id, model_request, ctx).await?;
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
                published_name: _,
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
                ctx.enter_flow(flow_id)
                    .map_err(|e| ExecutorError::Internal(format!("flow recursion limit: {}", e)))?;
                // Codex R3b.1 round 2 H1: propagate user → flow ACL gate.
                // Without this `dispatch_by_flow_id` sees `user_id = None`
                // and skips the per-flow ACL check.
                let (initial, mut meta) =
                    embeddings_request_to_initial_envelope(&request, ctx.user.clone());
                // RAG C2: re-wejście w flow dziedziczy bieżącą głębokość (po
                // `enter_flow`), żeby self-referencyjny embeddings-flow narastał
                // przez `subflow_depth` zamiast resetować się do 0.
                meta.flow_depth = ctx.flow_stack.len() as u8;
                // RAG E1.0 — przeprowadź tożsamość addona-callera do flow.
                meta.addon_id = ctx.addon_id.clone();
                meta.org_id = ctx.org_id.clone();
                let dispatch_result = dispatcher
                    .dispatch_by_flow_id(flow_id.clone(), initial, meta)
                    .await;
                ctx.leave_flow();
                let outcome =
                    dispatch_result.map_err(|e| ExecutorError::Internal(e.to_string()))?;
                let expected_count = match &request.input {
                    EmbeddingInput::Single(_) => 1,
                    EmbeddingInput::Multiple(texts) => texts.len(),
                };
                // No flow-level metric row: the flow's internal embeddings node
                // calls back into `ModelRuntimeExecutor::execute_embeddings`,
                // which records the real per-model row with identical
                // tokens+perf. A `flow_engine` row would double-count.
                flow_outcome_to_embedding_response(outcome, &request, expected_count)
            }
        }
    }

    // =========================================================================
    // RAG C2 — Rerank dispatch (cross-encoder, /v1/rerank)
    // =========================================================================

    /// Rerank dispatch — mirrors `execute_embeddings`. Resolves the requested
    /// model with `ServiceSurface::Rerank` (text in / text out), ranks the
    /// candidate list and tries each until one succeeds. Alias `rag-reranker`
    /// gets the SAME failover (candidates + availability + `alias_fallback_total`
    /// metric + warn) as chat/embeddings — there is no second rerank path.
    ///
    /// **ACL is the caller's responsibility.**
    pub async fn execute_rerank(
        &self,
        request: RerankRequest,
        ctx: &mut ExecutionContext,
    ) -> Result<RerankResponse, ExecutorError> {
        let outcome = {
            let snapshot = self.catalog.snapshot();
            let req = ResolveRequest {
                requested_model: &request.model,
                required_surface: ServiceSurface::Rerank,
                required_input_modalities: &[InputModality::Text],
                required_output_modalities: &[OutputModality::Text],
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
                .dispatch_rerank_blocking(&target, request.clone(), ctx)
                .await
            {
                Ok(response) => {
                    ctx.route_metadata.served_by_node = served_by(&target);
                    ctx.route_metadata.served_model = Some(target.requested_model().to_string());
                    ctx.route_metadata.backend_type = Some(target.telemetry_tag().to_string());
                    ctx.route_metadata.fallbacks_tried = (attempts - 1) as u32;
                    note_fallback(
                        &request.model,
                        outcome.requested_is_alias,
                        attempts,
                        target.telemetry_tag(),
                    );
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
                        "rerank dispatch failed; trying next candidate"
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

    /// Per-target rerank dispatch. Cross-encoders are served by external
    /// runtimes (vLLM `--task score`), so HTTP/QUIC/mesh carry the request;
    /// embedded engines have no reranking surface and surface an error so the
    /// fallback chain moves on. Flow targets execute a rerank-surface flow.
    async fn dispatch_rerank_blocking(
        &self,
        target: &ResolvedExecutionTarget,
        mut request: RerankRequest,
        ctx: &mut ExecutionContext,
    ) -> Result<RerankResponse, ExecutorError> {
        self.ensure_resident(target).await?;
        if let ResolvedExecutionTarget::Local { model_name, .. } = target {
            if request.model != *model_name {
                request.model = model_name.clone();
            }
        }

        match target {
            ResolvedExecutionTarget::Local { handle, .. } => match handle {
                BackendHandle::Embedded { .. } => Err(ExecutorError::Internal(
                    "embedded backend does not support reranking".into(),
                )),
                BackendHandle::Http(client) => client
                    .rerank_request(request)
                    .await
                    .map_err(|e| ExecutorError::Internal(e.to_string())),
                BackendHandle::Quic(handle) => {
                    let quic_client = handle.get_client().await.ok_or_else(|| {
                        ExecutorError::Internal(format!(
                            "QUIC client not connected for service '{}'",
                            handle.config.name
                        ))
                    })?;
                    let model_request = rerank_model_request(&request);
                    let response = quic_client
                        .send_request(model_request)
                        .await
                        .map_err(|e| ExecutorError::Internal(format!("QUIC rerank: {}", e)))?;
                    rerank_result_to_response(response.result)
                }
            },
            ResolvedExecutionTarget::MeshForward {
                node_id,
                model_name,
                ..
            } => {
                let mut forwarded = request.clone();
                forwarded.model = model_name.clone();
                let model_request = rerank_model_request(&forwarded);
                let response = self.forward_via_mesh(node_id, model_request, ctx).await?;
                rerank_result_to_response(response.result)
            }
            ResolvedExecutionTarget::Flow {
                flow_id,
                published_name: _,
            } => {
                let dispatcher = self
                    .flow_dispatcher
                    .as_ref()
                    .ok_or(ExecutorError::FlowDispatcherUnavailable)?;
                ctx.enter_flow(flow_id)
                    .map_err(|e| ExecutorError::Internal(format!("flow recursion limit: {}", e)))?;
                let (initial, mut meta) =
                    rerank_request_to_initial_envelope(&request, ctx.user.clone());
                // RAG C2: re-wejście w flow dziedziczy bieżącą głębokość (po
                // `enter_flow`), żeby self-referencyjny rerank-flow narastał
                // przez `subflow_depth` zamiast resetować się do 0.
                meta.flow_depth = ctx.flow_stack.len() as u8;
                // RAG E1.0 — przeprowadź tożsamość addona-callera do flow.
                meta.addon_id = ctx.addon_id.clone();
                meta.org_id = ctx.org_id.clone();
                let dispatch_result = dispatcher
                    .dispatch_by_flow_id(flow_id.clone(), initial, meta)
                    .await;
                ctx.leave_flow();
                let outcome =
                    dispatch_result.map_err(|e| ExecutorError::Internal(e.to_string()))?;
                flow_outcome_to_rerank_response(outcome)
            }
        }
    }

    // =========================================================================
    // RAG E1.2 — Document parse dispatch (vision → markdown + bloki)
    // =========================================================================

    /// Document-parse dispatch — lustro `execute_rerank`. Resoluje żądany model
    /// przez `ServiceSurface::Documents` (input Image / output Text), rankuje
    /// kandydatów per alias-strategy i próbuje każdego aż któryś zadziała. Alias
    /// `rag-parse` dostaje TEN SAM failover (kandydaci + dostępność +
    /// `alias_fallback_total` + warn) co rerank — to jeden punkt metryki
    /// fallbacku; nie ma drugiej ścieżki parsowania.
    ///
    /// **ACL po stronie callera** — host fn `doc_parse_v1` bramkuje uprawnienie
    /// `document.parse` zanim zbuduje request.
    pub async fn execute_documents(
        &self,
        request: DocumentParseRequest,
        ctx: &mut ExecutionContext,
    ) -> Result<DocumentParseResponse, ExecutorError> {
        // RAG E1.4 — PDF wchodzi jako wielostronicowy dokument: rasteryzujemy go
        // na obrazy stron i każdą stronę parsujemy istniejącą ścieżką vision,
        // po czym scalamy wyniki (markdown + bloki z poprawnymi numerami stron).
        if crate::services::document::is_pdf_mime(&request.mime) {
            return self.execute_documents_pdf(request, ctx).await;
        }
        // Vision-parse to model CHAT (VLM przez /v1/chat/completions, np.
        // nemotron-parse — zgodnie z NVIDIA nv-ingest, które serwuje
        // nemoretriever-parse jako VLM na /v1/chat/completions z obrazem). Budujemy
        // vision-chat request (obraz strony jako image_url base64 + instrukcja) i
        // idziemy przez `execute_chat`: resolve Chat-surface, Local→HTTP
        // /v1/chat/completions, MeshForward→ModelPayload::Vision (obraz ZACHOWANY
        // przez mesh — binarnie). Markdown = tekst odpowiedzi. Detektory
        // YOLOX/table/OCR (`/v1/infer`) to osobna typed ścieżka Documents —
        // patrz docs/RAG_INGEST_FLOW_PLAN.md.
        use crate::api::openai::types::{
            ChatCompletionRequest, ContentPart, ImageUrl, Message, MessageContent,
        };
        use base64::Engine as _;

        let b64 = base64::engine::general_purpose::STANDARD.encode(&request.image_bytes);
        let data_uri = format!("data:{};base64,{}", request.mime, b64);
        let instruction = "Wyodrębnij całą treść tej strony dokumentu jako czysty Markdown. \
             Zachowaj strukturę tabel (GFM), nagłówki, listy i kolejność czytania. \
             Zwróć WYŁĄCZNIE treść dokumentu, bez komentarza.";

        let chat_request = ChatCompletionRequest {
            model: request.model.clone(),
            messages: vec![Message {
                role: "user".to_string(),
                content: Some(MessageContent::Parts(vec![
                    ContentPart::Text {
                        text: instruction.to_string(),
                    },
                    ContentPart::ImageUrl {
                        image_url: ImageUrl {
                            url: data_uri,
                            detail: None,
                        },
                    },
                ])),
                ..Default::default()
            }],
            temperature: Some(0.0),
            max_tokens: Some(8192),
            top_p: None,
            n: None,
            stream: false,
            stream_options: None,
            stop: None,
            presence_penalty: None,
            frequency_penalty: None,
            user: None,
            tools: None,
            tool_choice: None,
            response_format: None,
            memory_options: None,
            audio_input: None,
        };

        let response = self.execute_chat(chat_request, ctx).await?;
        let markdown = crate::routing::extract_response_text(&response);
        Ok(DocumentParseResponse {
            markdown,
            blocks: Vec::new(),
            usage: response.usage,
        })
    }

    /// RAG Partia 3 (prerequisite) — uruchamia flow-ingest JEDNEGO dokumentu z
    /// BINARNYM payloadem. To JEDYNA ścieżka, która potrafi wywołać flow `rag:ingest`
    /// z surowymi bajtami pliku (PDF/xlsx/docx/obraz) — dotąd addon mógł wołać flow
    /// tylko przez `llm_generate`, które buduje wyłącznie tekstową wiadomość.
    ///
    /// Seeduje binarny envelope (`ingest_request_to_initial_envelope`: bajty →
    /// `ctx.blobs.put` → `FlowValue::Image`/`Other`), resolwuje flow po stałej
    /// modality `"document"` (`<model>:ingest:document` — jeden flow niezależnie od
    /// typu pliku) przez `FlowDispatcher::try_dispatch_with_modality`, wykonuje go
    /// blocking i mapuje wynik na `IngestResponse`. Blob dokumentu jest kasowany po
    /// flow (także przy błędzie), żeby nie zostawiać osieroconych plików w trwałym
    /// store. Guard rekursji przez `enter_flow`/`leave_flow` (jak parse/embeddings).
    ///
    /// **ACL po stronie callera** — host fn `ingest_invoke_v1` bramkuje uprawnienie
    /// zanim zbuduje request. Tożsamość addona-callera przeprowadzana do flow.meta.
    pub async fn execute_ingest(
        &self,
        request: IngestRequest,
        ctx: &mut ExecutionContext,
    ) -> Result<IngestResponse, ExecutorError> {
        let dispatcher = self
            .flow_dispatcher
            .as_ref()
            .ok_or(ExecutorError::FlowDispatcherUnavailable)?;

        // Pseudo flow_id dla guardu rekursji: ingest nie resolwuje po flow_id
        // (jak parse/embeddings przez catalog), tylko po nazwie `<model>:ingest`.
        // `enter_flow` potrzebuje stabilnego identyfikatora w stosie — używamy
        // nazwy modelu, by self-referencyjny ingest narastał `subflow_depth`.
        let flow_key = format!("{}:ingest", request.model);
        ctx.enter_flow(&flow_key)
            .map_err(|e| ExecutorError::Internal(format!("flow recursion limit: {}", e)))?;

        let blobs = dispatcher.blobs();
        let (initial, mut meta) =
            match ingest_request_to_initial_envelope(&request, ctx.user.clone(), blobs).await {
                Ok(seed) => seed,
                Err(e) => {
                    ctx.leave_flow();
                    return Err(ExecutorError::Internal(e.to_string()));
                }
            };
        meta.flow_depth = ctx.flow_stack.len() as u8;
        meta.addon_id = ctx.addon_id.clone();
        meta.org_id = ctx.org_id.clone();

        // Blob dokumentu MUSI być skasowany po flow — `put` ląduje w trwałym
        // `CompositeBlobStore`, więc bez `delete` każdy ingest zostawia osierocony
        // plik do grubego GC.
        let doc_blob_ref = match &initial.payload {
            crate::flow_engine::envelope::FlowValue::Image { blob_ref, .. }
            | crate::flow_engine::envelope::FlowValue::Other { blob_ref, .. } => {
                Some(blob_ref.clone())
            }
            _ => None,
        };

        let dispatch_result = dispatcher
            .try_dispatch_with_modality(&request.model, "ingest", "document", initial, meta)
            .await;
        ctx.leave_flow();

        if let Some(blob_ref) = doc_blob_ref {
            if let Err(e) = dispatcher.blobs().delete(&blob_ref).await {
                tracing::warn!(error = %e, "ingest: failed to delete document blob after flow");
            }
        }

        let outcome = dispatch_result.map_err(|e| ExecutorError::Internal(e.to_string()))?;
        flow_outcome_to_ingest_response(outcome)
    }

    /// PARTIA 0 — typed-surface `Documents` detektor (`/v1/infer`). Lustro
    /// `execute_rerank`: resoluje żądany model przez `ServiceSurface::Documents`
    /// (input Image / output Text), rankuje kandydatów per alias-strategy i próbuje
    /// każdego aż któryś zadziała. Local → HTTP `POST /v1/infer`; MeshForward →
    /// `ModelPayload::Documents` (obraz ZACHOWANY przez mesh — binarnie, serde_bytes).
    /// To NIE jest vision-parse (`execute_documents`, surface Chat) — to osobna,
    /// strukturalna ścieżka dla node-adapterów flow-ingestu RAG (PARTIA 2).
    pub async fn execute_document_infer(
        &self,
        request: DocumentInferRequest,
        ctx: &mut ExecutionContext,
    ) -> Result<DocumentInferResponse, ExecutorError> {
        let outcome = {
            let snapshot = self.catalog.snapshot();
            let req = ResolveRequest {
                requested_model: &request.model,
                required_surface: ServiceSurface::Documents,
                required_input_modalities: &[InputModality::Image],
                required_output_modalities: &[OutputModality::Text],
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
                .dispatch_document_infer_blocking(&target, request.clone(), ctx)
                .await
            {
                Ok(response) => {
                    ctx.route_metadata.served_by_node = served_by(&target);
                    ctx.route_metadata.served_model = Some(target.requested_model().to_string());
                    ctx.route_metadata.backend_type = Some(target.telemetry_tag().to_string());
                    ctx.route_metadata.fallbacks_tried = (attempts - 1) as u32;
                    note_fallback(
                        &request.model,
                        outcome.requested_is_alias,
                        attempts,
                        target.telemetry_tag(),
                    );
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
                        "document infer dispatch failed; trying next candidate"
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

    /// Per-target document-infer dispatch — lustro `dispatch_rerank_blocking`.
    /// Detektory struktury są serwowane przez zewnętrzne runtime'y (yolox/OCR przez
    /// `/v1/infer`), więc HTTP/mesh niosą request; embedded engines nie mają tej
    /// powierzchni i surface'ują błąd, żeby fallback szedł dalej.
    async fn dispatch_document_infer_blocking(
        &self,
        target: &ResolvedExecutionTarget,
        mut request: DocumentInferRequest,
        ctx: &mut ExecutionContext,
    ) -> Result<DocumentInferResponse, ExecutorError> {
        self.ensure_resident(target).await?;
        if let ResolvedExecutionTarget::Local { model_name, .. } = target {
            if request.model != *model_name {
                request.model = model_name.clone();
            }
        }

        match target {
            ResolvedExecutionTarget::Local { handle, .. } => match handle {
                BackendHandle::Embedded { .. } => Err(ExecutorError::Internal(
                    "embedded backend does not support document infer".into(),
                )),
                BackendHandle::Http(client) => {
                    let result = client
                        .document_infer(
                            &request.model,
                            &request.image_bytes,
                            &request.task,
                            &request.mime,
                        )
                        .await
                        .map_err(|e| ExecutorError::Internal(e.to_string()))?;
                    Ok(DocumentInferResponse {
                        regions: result.regions,
                    })
                }
                BackendHandle::Quic(handle) => {
                    let quic_client = handle.get_client().await.ok_or_else(|| {
                        ExecutorError::Internal(format!(
                            "QUIC client not connected for service '{}'",
                            handle.config.name
                        ))
                    })?;
                    let model_request = document_infer_model_request(&request);
                    let response = quic_client
                        .send_request(model_request)
                        .await
                        .map_err(|e| ExecutorError::Internal(format!("QUIC document infer: {}", e)))?;
                    document_infer_result_to_response(response.result)
                }
            },
            ResolvedExecutionTarget::MeshForward {
                node_id,
                model_name,
                ..
            } => {
                let mut forwarded = request.clone();
                forwarded.model = model_name.clone();
                let model_request = document_infer_model_request(&forwarded);
                let response = self.forward_via_mesh(node_id, model_request, ctx).await?;
                document_infer_result_to_response(response.result)
            }
            ResolvedExecutionTarget::Flow { .. } => Err(ExecutorError::Internal(
                "document infer has no flow-target surface".into(),
            )),
        }
    }

    /// RAG E1.4 — ścieżka PDF: rasteryzuje dokument na obrazy stron (pdfium,
    /// bezwarunkowo), parsuje każdą stronę przez `execute_documents` (ten sam
    /// resolve→rank→failover co dla pojedynczego obrazu) i scala wyniki
    /// (`merge_page_responses`). Cap stron egzekwowany na poziomie rasteryzera
    /// ([`MAX_PDF_PAGES`]).
    async fn execute_documents_pdf(
        &self,
        request: DocumentParseRequest,
        ctx: &mut ExecutionContext,
    ) -> Result<DocumentParseResponse, ExecutorError> {
        use crate::services::document::{self, DEFAULT_RENDER_DPI, MAX_PDF_PAGES};
        use crate::services::document::rasterize::{rasterize_pdf_streaming, PageRender, SinkClosed};

        // Bug 1 (agregatowy memory-DoS): NIE materializujemy wszystkich stron
        // jako RGB/PNG przed parse. Streaming strona-po-stronie: producent
        // (spawn_blocking) ładuje dokument RAZ pod pdfium-lockiem, renderuje
        // stronę → PNG → `blocking_send` na kanał o pojemności 2; konsument
        // (tu, async) odbiera PNG i parsuje POZA pdfium-lockiem. Backpressure
        // kanału ogranicza szczyt pamięci do ~2 stron, nie O(N).
        use futures::StreamExt as _;

        // Współbieżność parsowania stron. Model parse (885M VLM) NIE wysyca GPU
        // na pojedynczej stronie (util ~25% — bottleneck to narzut Pythona per
        // token, nie compute), więc kilka stron równolegle wypełnia GPU i
        // wall-clock wielostronicowego dokumentu spada wielokrotnie. Kanał ma tę
        // samą pojemność, by producent (rasteryzer) trzymał strony gotowe dla
        // workerów; szczyt pamięci ~PARSE_PAGE_CONCURRENCY stron PNG (bounded).
        const PARSE_PAGE_CONCURRENCY: usize = 6;

        let pdf_bytes = request.image_bytes.clone();
        let (tx, rx) = tokio::sync::mpsc::channel::<PageRender>(PARSE_PAGE_CONCURRENCY);

        let producer = tokio::task::spawn_blocking(move || {
            rasterize_pdf_streaming(&pdf_bytes, DEFAULT_RENDER_DPI, MAX_PDF_PAGES, |page| {
                // `blocking_send` blokuje wątek producenta gdy kanał pełny —
                // to właśnie backpressure (producent nie renderuje stron na
                // zapas). `Err` = konsument zamknął odbiornik → przerwij render.
                tx.blocking_send(page).map_err(|_| SinkClosed)
            })
        });

        // Konsument RÓWNOLEGŁY: parsujemy do PARSE_PAGE_CONCURRENCY stron naraz.
        // Każda strona to NIEZALEŻNY dispatch: własny KLON ctx z resetem hopa
        // (enter_hop inkrementuje hop_count TRWALE; współdzielony ctx tripnąłby
        // MAX_HOP_COUNT po kilku stronach). Wyniki wracają NIEUPORZĄDKOWANE, więc
        // niesiemy `page.index` i sortujemy przed merge — `merge_page_responses`
        // nadaje numery stron po kolejności w Vec.
        let base_hop = ctx.hop_count;
        let base_ctx = ctx.clone();
        let model = request.model.clone();
        let flow_depth = request.flow_depth;
        let page_stream = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|page| (page, rx))
        });
        let mut indexed: Vec<(u32, Result<DocumentParseResponse, ExecutorError>)> = page_stream
            .map(|page| {
                let mut page_ctx = base_ctx.clone();
                page_ctx.hop_count = base_hop;
                let model = model.clone();
                async move {
                    let page_request = DocumentParseRequest {
                        model,
                        image_bytes: page.png,
                        mime: "image/png".to_string(),
                        flow_depth,
                    };
                    // `Box::pin` — rekurencyjna async fn (execute_documents wołane
                    // z execute_documents_pdf); bez tego typ future byłby
                    // nieskończenie zagnieżdżony.
                    let res = Box::pin(self.execute_documents(page_request, &mut page_ctx)).await;
                    (page.index, res)
                }
            })
            .buffer_unordered(PARSE_PAGE_CONCURRENCY)
            .collect()
            .await;

        // Strumień skonsumował `rx` (drop wewnątrz unfold po wyczerpaniu) →
        // producent dostaje Closed i kończy.
        indexed.sort_by_key(|(idx, _)| *idx);
        let mut page_responses: Vec<DocumentParseResponse> = Vec::with_capacity(indexed.len());
        let mut failed_pages = 0usize;
        let mut last_page_err: Option<ExecutorError> = None;
        for (_idx, res) in indexed {
            match res {
                Ok(resp) => page_responses.push(resp),
                Err(e) => {
                    // Tolerancja per-strona: pojedyncza felerna strona (np. bug
                    // postprocessingu modelu parse na konkretnym layoutcie, albo
                    // chwilowy błąd backendu) NIE może ubić całego wielostronicowego
                    // dokumentu. Logujemy i pomijamy; dokument fail-uje TYLKO gdy
                    // ŻADNA strona się nie sparsowała (sprawdzenie niżej).
                    failed_pages += 1;
                    tracing::warn!(
                        error = %e,
                        failed_pages,
                        "document parse: strona nie sparsowana, pomijam (tolerancja per-strona)"
                    );
                    last_page_err = Some(e);
                }
            }
        }

        let rasterize_result = producer
            .await
            .map_err(|e| ExecutorError::Internal(format!("rasterize join: {e}")))?;

        let page_count =
            rasterize_result.map_err(|e| ExecutorError::Internal(format!("PDF rasterize: {e}")))?;

        // Dokument fail-uje TYLKO gdy żadna strona się nie sparsowała. Gdy choć
        // jedna przeszła, scalamy co mamy — felerne strony zostały pominięte
        // (raportujemy ile), zamiast tracić cały dokument przez jedną stronę.
        if page_responses.is_empty() {
            if let Some(e) = last_page_err {
                return Err(e);
            }
            return Err(ExecutorError::Internal("PDF has no renderable pages".into()));
        }
        if failed_pages > 0 {
            tracing::warn!(
                failed_pages,
                ok_pages = page_responses.len(),
                page_count,
                "document parse: dokument ukończony z pominiętymi stronami"
            );
        }

        Ok(document::merge_page_responses(page_responses))
    }

    /// Per-target document-parse dispatch. Serwer obsługuje parsowanie przez
    /// zdalny serwis vision (HTTP `POST /parse`); QUIC sidecar nie ma jeszcze
    /// wpiętego kanału obrazu, a embedded (telefon, Burn) to gniazdo na
    /// przyszłość — oba zwracają błąd, więc failover schodzi na zdalny backend
    /// zamiast blokować architekturę. Flow-target wykonuje documents-surface flow.
    async fn dispatch_documents_blocking(
        &self,
        target: &ResolvedExecutionTarget,
        mut request: DocumentParseRequest,
        ctx: &mut ExecutionContext,
    ) -> Result<DocumentParseResponse, ExecutorError> {
        self.ensure_resident(target).await?;
        if let ResolvedExecutionTarget::Local { model_name, .. } = target {
            if request.model != *model_name {
                request.model = model_name.clone();
            }
        }

        match target {
            ResolvedExecutionTarget::Local { handle, .. } => match handle {
                // Embedded parser dokumentów (PaddleOCR-VL MLX na macOS/iOS,
                // zarejestrowany przez deploy `paddle-ocr-mlx`). Gdy brak
                // zarejestrowanego parsera → Internal (nie abort), żeby pętla
                // failover zeszła na zdalny serwis.
                BackendHandle::Embedded { .. } => match crate::vision::get_document_parser() {
                    Some(parser) => {
                        let img = request.image_bytes.clone();
                        let mime = request.mime.clone();
                        // MLX liczy synchronicznie — poza tokio worker poolem.
                        let markdown = tokio::task::spawn_blocking(move || parser.parse(&img, &mime))
                            .await
                            .map_err(|e| ExecutorError::Internal(format!("paddle parse task: {e}")))?
                            .map_err(|e| ExecutorError::Internal(e.to_string()))?;
                        Ok(DocumentParseResponse {
                            markdown,
                            blocks: Vec::new(),
                            usage: None,
                        })
                    }
                    None => Err(ExecutorError::Internal(
                        "embedded document parse: brak zarejestrowanego DocumentParser".into(),
                    )),
                },
                BackendHandle::Http(client) => client
                    .parse_document(&request.model, &request.image_bytes, &request.mime)
                    .await
                    .map_err(|e| ExecutorError::Internal(e.to_string())),
                // Obraz przez QUIC nie ma jeszcze wpiętego payloadu — odkładamy
                // do follow-up. Internal, żeby failover spróbował HTTP/Flow.
                BackendHandle::Quic(_) => Err(ExecutorError::Internal(
                    "documents over QUIC TBD".into(),
                )),
            },
            // Mesh-forward obrazu (cross-node parse) to osobny slice — na razie
            // pending cutover, jak rerank.
            ResolvedExecutionTarget::MeshForward { .. } => {
                Err(ExecutorError::TransportPendingCutover("mesh_forward"))
            }
            ResolvedExecutionTarget::Flow {
                flow_id,
                published_name: _,
            } => {
                let dispatcher = self
                    .flow_dispatcher
                    .as_ref()
                    .ok_or(ExecutorError::FlowDispatcherUnavailable)?;
                ctx.enter_flow(flow_id)
                    .map_err(|e| ExecutorError::Internal(format!("flow recursion limit: {}", e)))?;
                let blobs = dispatcher.blobs();
                let (initial, mut meta) =
                    match document_request_to_initial_envelope(&request, ctx.user.clone(), blobs)
                        .await
                    {
                        Ok(seed) => seed,
                        Err(e) => {
                            ctx.leave_flow();
                            return Err(ExecutorError::Internal(e.to_string()));
                        }
                    };
                // RAG E1.2: re-wejście w flow dziedziczy bieżącą głębokość (po
                // `enter_flow`), żeby self-referencyjny parse-flow narastał przez
                // `subflow_depth` zamiast resetować się do 0.
                meta.flow_depth = ctx.flow_stack.len() as u8;
                // RAG E1.0 — przeprowadź tożsamość addona-callera do flow.
                meta.addon_id = ctx.addon_id.clone();
                meta.org_id = ctx.org_id.clone();
                // Obraz strony wrzucony do BlobStore MUSI być skasowany po
                // zakończeniu flow (także przy błędzie) — `put` ląduje w trwałym
                // store (`CompositeBlobStore`), więc bez jawnego `delete` częste
                // parse-as-flow zostawia osierocone obrazy do grubego GC.
                let page_blob_ref = match &initial.payload {
                    crate::flow_engine::envelope::FlowValue::Image { blob_ref, .. } => {
                        Some(blob_ref.clone())
                    }
                    _ => None,
                };
                let dispatch_result = dispatcher
                    .dispatch_by_flow_id(flow_id.clone(), initial, meta)
                    .await;
                ctx.leave_flow();
                if let Some(blob_ref) = page_blob_ref {
                    if let Err(e) = dispatcher.blobs().delete(&blob_ref).await {
                        tracing::warn!(error = %e, "document parse: failed to delete page blob after flow");
                    }
                }
                let outcome =
                    dispatch_result.map_err(|e| ExecutorError::Internal(e.to_string()))?;
                flow_outcome_to_document_response(outcome)
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
    // TODO Chunk 2b: TTS/STT metrics need usage/duration plumbing (audio_ms,
    // char count) before they can feed `model_metrics_rollup`.
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
                    ctx.route_metadata.served_model = Some(target.requested_model().to_string());
                    ctx.route_metadata.backend_type = Some(target.telemetry_tag().to_string());
                    ctx.route_metadata.fallbacks_tried = (attempts - 1) as u32;
                    note_fallback(
                        &request.model,
                        outcome.requested_is_alias,
                        attempts,
                        target.telemetry_tag(),
                    );
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

                    // Lazy-load + memory guard: dla unpinned embedded TTS zaladuj
                    // przez residency (evict innych rezydentnych + tracking +
                    // idle-unload). Best-effort — przy bledzie self-heal blok
                    // ponizej i tak zaladuje (zero regresji). Pinned / brak
                    // residency → no-op.
                    // Bind clone do zmiennej PRZED await — inaczej temporary
                    // guard parking_lot (`*mut`, !Send) zylby przez await
                    // (if-let temporary lifetime) i future przestawalby byc Send.
                    let tts_residency = self.model_residency.read().clone();
                    if let Some(res) = tts_residency {
                        if let Err(e) = res.ensure_loaded(&model_name_owned).await {
                            tracing::warn!(
                                "TTS residency ensure_loaded '{}': {}",
                                model_name_owned,
                                e
                            );
                        }
                    }

                    // Self-heal: po restarcie procesu `TtsManager` startuje pusty
                    // mimo `status=running` uslugi (deploy laduje przy prepare,
                    // nie ma boot-reloadu). Leniwie laduje + rejestruje silnik
                    // zanim siegniemy po `synthesize`, zeby nie zwracac "engine
                    // nie zarejestrowany".
                    if !crate::tts::shared_tts_manager()
                        .read()
                        .await
                        .has(&engine_id_owned)
                    {
                        let repo = resolve_embedded_tts_repo(&engine_id_owned, &model_name_owned);
                        crate::tts::ensure_embedded_engine_loaded(
                            &engine_id_owned,
                            &repo,
                            Some(model_name_owned.as_str()),
                        )
                        .await
                        .map_err(|e| {
                            ExecutorError::Internal(format!("embedded TTS load: {e:#}"))
                        })?;
                    }
                    // Czysci tekst przed synteza: substytucja z `tts_cleaning_rules`
                    // (cache) + strip emoji/punktuacji. Robione TU (nie w
                    // node `tts_clean`) zeby kazda synteza TTS we flow czyscila
                    // — niezaleznie czy flow ma osobny node tts_clean. Galaz
                    // tekstu (odpowiedz) idzie inna sciezka i NIE jest czyszczona.
                    let text = {
                        let raw = request.input.clone();
                        match &self.db {
                            Some(db) => {
                                let db = db.clone();
                                let raw_fallback = raw.clone();
                                tokio::task::spawn_blocking(move || {
                                    crate::tts::clean_cache::clean(&raw, &db)
                                })
                                .await
                                .unwrap_or(raw_fallback)
                            }
                            None => raw,
                        }
                    };
                    let speed = request.speed.unwrap_or(1.0);
                    // Multilingual silniki (Supertonic) potrzebuja jezyka z
                    // requestu (per-user trafia do `TTSRequest.language` przez
                    // TtsDispatcher); voice preset (np. `M1`) wybiera glos.
                    let language = request.language.clone();
                    let voice = if request.voice.is_empty() {
                        None
                    } else {
                        Some(request.voice.clone())
                    };
                    let res =
                        tokio::task::spawn_blocking(move || -> anyhow::Result<(Vec<f32>, u32)> {
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
                                    voice,
                                    language,
                                },
                            )?;
                            Ok((out.samples, out.sample_rate))
                        })
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
                    let format = request
                        .response_format
                        .clone()
                        .unwrap_or_else(|| "wav".into());
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
                let format = request
                    .response_format
                    .clone()
                    .unwrap_or_else(|| "wav".into());
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
                let response = self.forward_via_mesh(node_id, model_request, ctx).await?;
                match response.result {
                    ModelResult::Audio(audio_result) => match audio_result.data {
                        AudioResultData::Audio(bytes) => Ok(TtsExecutionResult { bytes, format }),
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
                ctx.enter_flow(flow_id)
                    .map_err(|e| ExecutorError::Internal(format!("flow recursion limit: {e}")))?;
                let (initial, mut meta) = tts_request_to_initial_envelope(&request, ctx.user.clone());
                // RAG E1.0 — przeprowadź tożsamość addona-callera do flow.
                meta.addon_id = ctx.addon_id.clone();
                meta.org_id = ctx.org_id.clone();
                let dispatch_result = dispatcher
                    .dispatch_by_flow_id(flow_id.clone(), initial, meta)
                    .await;
                ctx.leave_flow();
                let outcome =
                    dispatch_result.map_err(|e| ExecutorError::Internal(e.to_string()))?;
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
    ///   Executor (mesh STT forward czeka na refactor mesh inference proxy);
    ///   wracamy `SttBackend("mesh forward not implemented")`
    ///   zeby request padal czytelnie zamiast cicho lokalnym whisperem.
    /// * `Flow` → wracamy clean failure (flow STT idzie przez flow_engine
    ///   adapter, nie executor).
    /// Resolver error (UnknownModel/CapabilityUnsupported/NoLiveInstance) padamy
    /// `SttBackend(error)` zeby user zobaczyl klarowny blad.
    /// Gdy `model` jest pusty / brak kandydatow → fallback do default
    /// local whisper (zachowuje pre-existing UX dla single-engine node'u).
    // TODO Chunk 2b: TTS/STT metrics need usage/duration plumbing (audio_ms,
    // transcript tokens) before they can feed `model_metrics_rollup`.
    pub async fn execute_stt(
        &self,
        request: TranscriptionRequest,
        ctx: &mut ExecutionContext,
    ) -> Result<TranscriptionResponse, ExecutorError> {
        use tentaflow_protocol::*;

        let runtime = self
            .stt_runtime
            .read()
            .clone()
            .ok_or_else(|| ExecutorError::SttRuntimeUnavailable)?;

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
            | Err(crate::services::runtime::resolver::ResolveError::CapabilityUnsupported {
                ..
            })
            | Err(crate::services::runtime::resolver::ResolveError::NoLiveInstance(_)) => {
                // Model nie zmatchował żadnej usługi STT w katalogu → fallback
                // do default local whisper (legacy single-node z "whisper-1").
                // Gdy lokalnego też nie ma — błąd musi nazwać model, żeby user
                // widział że nazwa w node'ie nie pasuje do wdrożonej usługi STT.
                let model = request.model.clone();
                tracing::warn!(
                    requested_model = %model,
                    "STT: model nie rozwiązał się do usługi w katalogu — fallback na lokalny whisper"
                );
                return runtime.transcribe(request).await.map_err(|e| {
                    ExecutorError::SttBackend(format!(
                        "model STT '{model}' nie pasuje do żadnej uruchomionej usługi STT \
                         w katalogu, a lokalny silnik whisper nie jest załadowany ({e})"
                    ))
                });
            }
            Err(e) => return Err(ExecutorError::SttBackend(format!("resolver: {}", e))),
        };

        let state = self.strategy_state_for(&request.model);
        let ranked = rank(&outcome.candidates, outcome.strategy, &state);
        if ranked.is_empty() {
            let model = request.model.clone();
            return runtime.transcribe(request).await.map_err(|e| {
                ExecutorError::SttBackend(format!(
                    "model STT '{model}' rozwiązany ale bez kandydatów do wykonania, \
                     a lokalny silnik whisper nie jest załadowany ({e})"
                ))
            });
        }

        for target in ranked {
            match target {
                ResolvedExecutionTarget::Local {
                    service_id,
                    model_name,
                    ..
                } => {
                    // Lazy-load + memory guard (best-effort): zaladuj embedded STT
                    // przez residency (evict innych rezydentnych). Przy bledzie
                    // transcribe_for_service i tak ma wlasny lazy-load — bez regresji.
                    let stt_residency = self.model_residency.read().clone();
                    if let Some(res) = stt_residency {
                        if let Err(e) = res.ensure_loaded(&model_name).await {
                            tracing::warn!("STT residency ensure_loaded '{}': {}", model_name, e);
                        }
                    }
                    return runtime
                        .transcribe_for_service(service_id, request)
                        .await
                        .map_err(|e| ExecutorError::SttBackend(e.to_string()));
                }
                ResolvedExecutionTarget::MeshForward {
                    node_id,
                    service_id,
                    model_name,
                } => {
                    // Usługa "mesh" wskazująca na NASZ własny node (albo gdy mesh
                    // w ogóle nie jest podłączony = single-node lokalnie) jest de
                    // facto lokalna — odpalamy ją przez service zamiast forwardować
                    // sam do siebie po mesh.
                    let is_local = match self.mesh_manager.read().clone() {
                        Some(m) => m.node_id() == node_id,
                        None => true,
                    };
                    if is_local {
                        return runtime
                            .transcribe_for_service(service_id, request)
                            .await
                            .map_err(|e| ExecutorError::SttBackend(e.to_string()));
                    }
                    let model_request = ModelRequest {
                        request_id: uuid::Uuid::new_v4().to_string(),
                        payload: ModelPayload::Audio(AudioPayload {
                            operation: AudioOperation::STT {
                                model: model_name.clone(),
                                audio_data: request.file.to_vec(),
                                language: request.language.clone(),
                                response_format: request.response_format.clone(),
                                prompt: request.prompt.clone(),
                                temperature: None,
                                timestamp_granularities: None,
                                no_speech_threshold: None,
                                avg_logprob_threshold: None,
                                compression_ratio_threshold: None,
                                extra_params: None,
                            },
                        }),
                        stream: false,
                        metadata: None,
                        session_id: None,
                    };
                    let response = self.forward_via_mesh(&node_id, model_request, ctx).await?;
                    return match response.result {
                        ModelResult::Audio(audio_result) => match audio_result.data {
                            AudioResultData::Text(text) => Ok(TranscriptionResponse {
                                text,
                                task: None,
                                language: request.language.clone(),
                                duration: None,
                                segments: None,
                                speakers: None,
                            }),
                            AudioResultData::Detailed {
                                text,
                                language,
                                duration,
                                ..
                            } => Ok(TranscriptionResponse {
                                text,
                                task: None,
                                language: Some(language),
                                duration: Some(duration),
                                segments: None,
                                speakers: None,
                            }),
                            _ => Err(ExecutorError::SttBackend(
                                "mesh STT returned non-text result".into(),
                            )),
                        },
                        ModelResult::Error(err) => Err(ExecutorError::SttBackend(format!(
                            "mesh STT error: {}",
                            err.message
                        ))),
                        _ => Err(ExecutorError::SttBackend(
                            "mesh STT returned unexpected result type".into(),
                        )),
                    };
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
/// Dla `EmbeddingInput::Multiple` payload zostaje pierwszym tekstem
/// (single-input fallback dla legacy adapterów), a pełna lista trafia do
/// `envelope.meta["embeddings_inputs"]` jako JSON array. EmbeddingsNodeAdapter
/// preferuje meta gdy istnieje (multi-input batch), w przeciwnym wypadku
/// używa payload (single).
pub(crate) fn embeddings_request_to_initial_envelope(
    request: &EmbeddingRequest,
    user: Option<crate::auth::acl::UserContext>,
) -> (
    crate::flow_engine::envelope::FlowEnvelope,
    crate::flow_engine::dispatcher::FlowRequestMeta,
) {
    use crate::flow_engine::envelope::{FlowEnvelope, FlowValue};
    let mut env = FlowEnvelope::empty();
    match &request.input {
        EmbeddingInput::Single(text) => {
            env.payload = FlowValue::Text(text.clone());
        }
        EmbeddingInput::Multiple(texts) => {
            // Pierwszy tekst zostaje na payload jako fallback dla adapterów
            // które zostały na single-input contract; pełna lista w meta.
            env.payload = FlowValue::Text(texts.first().cloned().unwrap_or_default());
            env.meta.insert(
                "embeddings_inputs".into(),
                serde_json::Value::Array(
                    texts
                        .iter()
                        .map(|t| serde_json::Value::String(t.clone()))
                        .collect(),
                ),
            );
        }
    }
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
pub(crate) fn tts_request_to_initial_envelope(
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
    if let Some(spd) = request.speed {
        if let Some(num) = serde_json::Number::from_f64(spd as f64) {
            env.meta
                .insert("speed".into(), serde_json::Value::Number(num));
        }
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
pub(crate) async fn flow_outcome_to_tts_result(
    outcome: crate::flow_engine::envelope::FlowExecutionOutcome,
    blobs: std::sync::Arc<dyn crate::flow_engine::blob_store::BlobStore>,
) -> Result<TtsExecutionResult, ExecutorError> {
    use crate::flow_engine::envelope::FlowValue;
    match outcome.final_envelope.payload {
        FlowValue::Audio { blob_ref, mime, .. } => {
            let bytes = blobs
                .get(&blob_ref)
                .await
                .map_err(|e| ExecutorError::Internal(format!("tts flow blob read: {e}")))?;
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
pub(crate) fn flow_outcome_to_embedding_response(
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

/// Buduje `ModelRequest` (QUIC/mesh) z `RerankRequest`. Strip well-known
/// router-side prefixu z modelu jak embeddings — silnik widzi gołą nazwę.
fn rerank_model_request(request: &RerankRequest) -> tentaflow_protocol::ModelRequest {
    use tentaflow_protocol::*;
    let engine_model_name = request
        .model
        .strip_prefix("tentaflow-rerank-")
        .or_else(|| request.model.strip_prefix("rerank-"))
        .unwrap_or(&request.model)
        .to_string();
    ModelRequest {
        request_id: uuid::Uuid::new_v4().to_string(),
        payload: ModelPayload::Rerank(RerankPayload {
            model: engine_model_name,
            query: request.query.clone(),
            documents: request.documents.clone(),
            top_n: request.top_n.map(|n| n as usize),
            return_documents: false,
        }),
        stream: false,
        metadata: None,
        session_id: None,
    }
}

/// Mapuje `ModelResult` (QUIC/mesh) na `RerankResponse`. Wynik z silnika niesie
/// `index`/`relevance_score` — `document` ignorujemy (adapter ma własne teksty).
fn rerank_result_to_response(
    result: tentaflow_protocol::ModelResult,
) -> Result<RerankResponse, ExecutorError> {
    use tentaflow_protocol::ModelResult;
    match result {
        ModelResult::Rerank(r) => Ok(RerankResponse {
            results: r
                .results
                .into_iter()
                .map(|item| RerankResultEntry {
                    index: item.index,
                    relevance_score: item.relevance_score,
                })
                .collect(),
        }),
        ModelResult::Error(err) => {
            Err(ExecutorError::Internal(format!("rerank error: {}", err.message)))
        }
        _ => Err(ExecutorError::Internal(
            "rerank returned unexpected result type".into(),
        )),
    }
}

/// PARTIA 0 — buduje `ModelRequest` (QUIC/mesh) z `DocumentInferRequest`.
/// Obraz leci binarnie w `image_bytes` (serde_bytes → CBOR byte-string).
fn document_infer_model_request(
    request: &DocumentInferRequest,
) -> tentaflow_protocol::ModelRequest {
    use tentaflow_protocol::*;
    ModelRequest {
        request_id: uuid::Uuid::new_v4().to_string(),
        payload: ModelPayload::Documents(DocumentInferPayload {
            model: request.model.clone(),
            image_bytes: request.image_bytes.clone(),
            mime: request.mime.clone(),
            task: request.task.clone(),
        }),
        stream: false,
        metadata: None,
        session_id: None,
    }
}

/// PARTIA 0 — mapuje `ModelResult` (QUIC/mesh) na `DocumentInferResponse`.
fn document_infer_result_to_response(
    result: tentaflow_protocol::ModelResult,
) -> Result<DocumentInferResponse, ExecutorError> {
    use tentaflow_protocol::ModelResult;
    match result {
        ModelResult::Documents(r) => Ok(DocumentInferResponse {
            regions: r.regions,
        }),
        ModelResult::Error(err) => Err(ExecutorError::Internal(format!(
            "document infer error: {}",
            err.message
        ))),
        _ => Err(ExecutorError::Internal(
            "document infer returned unexpected result type".into(),
        )),
    }
}

/// Buduje seed envelope + meta dla rerank-as-flow path. Query na payload (Text),
/// dokumenty + model + top_n w meta — flow-owy adapter rerankera czyta je stamtąd.
pub(crate) fn rerank_request_to_initial_envelope(
    request: &RerankRequest,
    user: Option<crate::auth::acl::UserContext>,
) -> (
    crate::flow_engine::envelope::FlowEnvelope,
    crate::flow_engine::dispatcher::FlowRequestMeta,
) {
    use crate::flow_engine::envelope::{FlowEnvelope, FlowValue};
    let mut env = FlowEnvelope::empty();
    env.payload = FlowValue::Json(serde_json::json!({
        "query": request.query,
        "candidates": request
            .documents
            .iter()
            .enumerate()
            .map(|(i, text)| serde_json::json!({ "id": i.to_string(), "text": text }))
            .collect::<Vec<_>>(),
    }));
    env.meta.insert(
        "rerank_model".into(),
        serde_json::Value::String(request.model.clone()),
    );
    if let Some(n) = request.top_n {
        env.meta
            .insert("top_n".into(), serde_json::Value::Number(n.into()));
    }
    let mut meta =
        crate::flow_engine::dispatcher::FlowRequestMeta::new(uuid::Uuid::new_v4().to_string());
    if let Some(u) = user {
        meta.user_id = Some(u.user_id);
        meta.user_role = Some(u.role);
    }
    (env, meta)
}

/// Konwertuje FlowExecutionOutcome (rerank-as-flow) na `RerankResponse`. Flow
/// output to `Json{ranked:[{id,score,text}]}` — `id` (string) mapujemy z
/// powrotem na oryginalny `index` przez parsowanie pozycji.
pub(crate) fn flow_outcome_to_rerank_response(
    outcome: crate::flow_engine::envelope::FlowExecutionOutcome,
) -> Result<RerankResponse, ExecutorError> {
    use crate::flow_engine::envelope::FlowValue;
    let ranked = match &outcome.final_envelope.payload {
        FlowValue::Json(v) => v.get("ranked").and_then(|r| r.as_array()).cloned(),
        _ => None,
    }
    .ok_or_else(|| {
        ExecutorError::Internal("rerank flow returned no 'ranked' array in Json payload".into())
    })?;

    let mut results = Vec::with_capacity(ranked.len());
    for entry in ranked {
        let index = entry
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<usize>().ok())
            .ok_or_else(|| {
                ExecutorError::Internal("rerank flow entry missing numeric 'id'".into())
            })?;
        let relevance_score = entry
            .get("score")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| ExecutorError::Internal("rerank flow entry missing 'score'".into()))?
            as f32;
        results.push(RerankResultEntry {
            index,
            relevance_score,
        });
    }
    Ok(RerankResponse { results })
}

/// RAG E1.2 — buduje seed envelope + meta dla document-parse-as-flow path.
/// Obraz (binarny, potencjalnie duży) ląduje w BlobStore jak audio w STT —
/// payload nosi sentinel `FlowValue::Image{blob_ref}`, adapter parsera pobiera
/// bajty z `ctx.blobs.get(&blob_ref)`. Nazwa modelu ląduje w `envelope.meta`.
pub(crate) async fn document_request_to_initial_envelope(
    request: &DocumentParseRequest,
    user: Option<crate::auth::acl::UserContext>,
    blobs: std::sync::Arc<dyn crate::flow_engine::blob_store::BlobStore>,
) -> anyhow::Result<(
    crate::flow_engine::envelope::FlowEnvelope,
    crate::flow_engine::dispatcher::FlowRequestMeta,
)> {
    use crate::flow_engine::envelope::{FlowEnvelope, FlowValue};
    let blob_ref = blobs
        .put(request.image_bytes.clone(), &request.mime)
        .await
        .map_err(|e| anyhow::anyhow!("document parse blob put: {e}"))?;
    let mut env = FlowEnvelope::empty();
    env.payload = FlowValue::Image {
        blob_ref,
        mime: request.mime.clone(),
        dims: None,
    };
    env.meta.insert(
        "parse_model".into(),
        serde_json::Value::String(request.model.clone()),
    );
    let mut meta =
        crate::flow_engine::dispatcher::FlowRequestMeta::new(uuid::Uuid::new_v4().to_string());
    if let Some(u) = user {
        meta.user_id = Some(u.user_id);
        meta.user_role = Some(u.role);
    }
    Ok((env, meta))
}

/// RAG Partia 3 — buduje seed envelope + meta dla ingest-as-flow path. Dokument
/// (binarny, potencjalnie duży) ląduje w BlobStore jak obraz w parse/audio w STT:
/// payload nosi sentinel `FlowValue::Image{blob_ref}` dla obrazów albo
/// `FlowValue::Other{blob_ref}` dla generycznych plików (PDF/xlsx/docx), a węzeł
/// parsera flow pobiera bajty z `ctx.blobs.get(&blob_ref)`. Nazwa flow ląduje w
/// `parse_model`/`ingest_model`; `options` (collection_id, graph toggle, params)
/// wsiąka do `envelope.meta` — to JEDYNE miejsce wstrzyknięcia opcji ingestu do
/// core'owego flow. Wzór: `document_request_to_initial_envelope`.
pub(crate) async fn ingest_request_to_initial_envelope(
    request: &IngestRequest,
    user: Option<crate::auth::acl::UserContext>,
    blobs: std::sync::Arc<dyn crate::flow_engine::blob_store::BlobStore>,
) -> anyhow::Result<(
    crate::flow_engine::envelope::FlowEnvelope,
    crate::flow_engine::dispatcher::FlowRequestMeta,
)> {
    use crate::flow_engine::envelope::{FlowEnvelope, FlowValue};
    let blob_ref = blobs
        .put(request.document_bytes.clone(), &request.mime)
        .await
        .map_err(|e| anyhow::anyhow!("ingest blob put: {e}"))?;
    let mut env = FlowEnvelope::empty();
    // Obraz → Image (modality vision na parse-węźle), inne pliki → Other (PDF
    // rasteryzowany / xlsx/docx czytany przez wyspecjalizowany węzeł parsera).
    // Typ bierzemy z mime (source-of-truth), nie z rozszerzenia.
    env.payload = if request.mime.starts_with("image/") {
        FlowValue::Image {
            blob_ref,
            mime: request.mime.clone(),
            dims: None,
        }
    } else {
        FlowValue::Other {
            blob_ref,
            mime: request.mime.clone(),
            filename: None,
        }
    };
    env.meta.insert(
        "ingest_model".into(),
        serde_json::Value::String(request.model.clone()),
    );
    // Opcje ingestu z addona (collection_id, graph_enabled, chunking) wstrzyknięte
    // do flow.meta pod kluczami źródłowymi — węzły flow czytają je przez fallback
    // `node.config -> envelope.meta`, jak parametry seedów query.
    for (key, value) in &request.options {
        env.meta.insert(key.clone(), value.clone());
    }
    let mut meta =
        crate::flow_engine::dispatcher::FlowRequestMeta::new(uuid::Uuid::new_v4().to_string());
    if let Some(u) = user {
        meta.user_id = Some(u.user_id);
        meta.user_role = Some(u.role);
    }
    Ok((env, meta))
}

/// RAG Partia 3 — konwertuje `FlowExecutionOutcome` (ingest-as-flow) na
/// `IngestResponse`. Flow output to `Json{markdown, chunks, page_count}` — węzeł
/// store ingestu raportuje markdown rekonstrukcji + liczbę zapisanych chunków.
/// Brakujące `chunks`/`page_count` dekoduje do 0/1 (tolerancja kształtu).
pub(crate) fn flow_outcome_to_ingest_response(
    outcome: crate::flow_engine::envelope::FlowExecutionOutcome,
) -> Result<IngestResponse, ExecutorError> {
    use crate::flow_engine::envelope::FlowValue;
    let json = match &outcome.final_envelope.payload {
        FlowValue::Json(v) => v.clone(),
        _ => {
            return Err(ExecutorError::Internal(
                "ingest flow returned no Json payload".into(),
            ))
        }
    };
    let markdown = json
        .get("markdown")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let chunks = json
        .get("chunks")
        .and_then(|v| v.as_u64())
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0);
    let page_count = json
        .get("page_count")
        .and_then(|v| v.as_u64())
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(1);
    // Teksty chunków NIE wracają przez ABI: duży dokument przekroczyłby cap 8 MiB
    // (PayloadTooLarge) i addon oznaczyłby ingest jako failed mimo zapisanych
    // wektorów (niespójność). Addon czyta teksty chunków z przestrzeni wektorowej
    // `passages` po `doc_id` (to samo źródło prawdy co cleanup/delete) — graf nie
    // potrzebuje ich z odpowiedzi.
    Ok(IngestResponse {
        markdown,
        chunks,
        page_count,
    })
}

/// RAG E1.2 — konwertuje `FlowExecutionOutcome` (parse-as-flow) na
/// `DocumentParseResponse`. Flow output to `Json{markdown, blocks:[{...}]}` —
/// czytamy markdown i bloki tak jak z serwisu HTTP. Brakujące `confidence`
/// dekoduje się do `None`, brakujące `page` do 0.
pub(crate) fn flow_outcome_to_document_response(
    outcome: crate::flow_engine::envelope::FlowExecutionOutcome,
) -> Result<DocumentParseResponse, ExecutorError> {
    use crate::flow_engine::envelope::FlowValue;
    let json = match &outcome.final_envelope.payload {
        FlowValue::Json(v) => v.clone(),
        _ => {
            return Err(ExecutorError::Internal(
                "document parse flow returned no Json payload".into(),
            ))
        }
    };
    let markdown = json
        .get("markdown")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let blocks = parse_blocks_json(json.get("blocks"));
    Ok(DocumentParseResponse {
        markdown,
        blocks,
        usage: None,
    })
}

/// Wspólny parser bloków z `serde_json` (używany przez ścieżkę HTTP serwisu i
/// flow-target). Blok bez wymaganych pól (`class`/`text` jako string), ze złym
/// `bbox` (musi być tablicą dokładnie 4 liczb) lub z `page` poza zakresem u32
/// jest POMIJANY — nie wchodzi do wyniku jako pusty/uszkodzony wpis. Tolerancja
/// per-blok: jeden zły blok nie wywala całej odpowiedzi.
pub(crate) fn parse_blocks_json(blocks: Option<&serde_json::Value>) -> Vec<DocBlock> {
    let Some(arr) = blocks.and_then(|b| b.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(arr.len());
    for entry in arr {
        // Pola wymagane: `class` i `text` MUSZĄ być stringami. Brak/zły typ →
        // pomiń cały blok (nie wpychaj pustego — to cicha korupcja wyniku).
        let Some(class) = entry.get("class").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(text) = entry.get("text").and_then(|v| v.as_str()) else {
            continue;
        };
        // `bbox` musi być tablicą DOKŁADNIE 4 liczb — inaczej pomiń blok
        // zamiast zerować/ucinać współrzędne.
        let Some(bbox_arr) = entry.get("bbox").and_then(|v| v.as_array()) else {
            continue;
        };
        if bbox_arr.len() != 4 {
            continue;
        }
        let mut bbox = [0.0f32; 4];
        let mut bbox_ok = true;
        for (slot, raw) in bbox.iter_mut().zip(bbox_arr.iter()) {
            match raw.as_f64() {
                Some(n) => *slot = n as f32,
                None => {
                    bbox_ok = false;
                    break;
                }
            }
        }
        if !bbox_ok {
            continue;
        }
        // `page`: u64 → u32 przez try_from (zła/za duża wartość → pomiń blok,
        // NIE owijaj `as u32`). Brak pola dekoduje do 0 (pojedynczy obraz).
        let page = match entry.get("page") {
            Some(v) => match v.as_u64() {
                Some(p) => match u32::try_from(p) {
                    Ok(p) => p,
                    Err(_) => continue,
                },
                None => continue,
            },
            None => 0,
        };
        let confidence = entry
            .get("confidence")
            .and_then(|v| v.as_f64())
            .map(|c| c as f32);
        out.push(DocBlock {
            page,
            class: class.to_string(),
            bbox,
            text: text.to_string(),
            confidence,
        });
    }
    out
}

/// Stage 3d-0b-4: buduje seed envelope + meta dla STT-as-flow path.
/// Audio bytes lądują w BlobStore (sentinel BlobRef zostaje na payload),
/// adapter STT pobiera bytes z `ctx.blobs.get(&blob_ref)` w execute().
/// Pola `language` / `prompt` / `temperature` lądują w `envelope.meta` —
/// adapter może je czytać z fallback `node.config -> envelope.meta`.
pub(crate) async fn stt_request_to_initial_envelope(
    request: &TranscriptionRequest,
    user: Option<crate::auth::acl::UserContext>,
    blobs: std::sync::Arc<dyn crate::flow_engine::blob_store::BlobStore>,
) -> anyhow::Result<(
    crate::flow_engine::envelope::FlowEnvelope,
    crate::flow_engine::dispatcher::FlowRequestMeta,
)> {
    use crate::flow_engine::envelope::{FlowEnvelope, FlowValue};
    let mime = mime_for_filename(&request.filename);
    let bytes_vec = request.file.to_vec();
    let blob_ref = blobs
        .put(bytes_vec, &mime)
        .await
        .map_err(|e| anyhow::anyhow!("STT blob put: {e}"))?;
    let mut env = FlowEnvelope::empty();
    env.payload = FlowValue::Audio {
        blob_ref,
        mime,
        sample_rate: None,
    };
    env.meta.insert(
        "stt_model".into(),
        serde_json::Value::String(request.model.clone()),
    );
    if let Some(lang) = &request.language {
        env.meta
            .insert("language".into(), serde_json::Value::String(lang.clone()));
    }
    if let Some(prompt) = &request.prompt {
        env.meta
            .insert("prompt".into(), serde_json::Value::String(prompt.clone()));
    }
    if let Some(temp) = request.temperature {
        if let Some(num) = serde_json::Number::from_f64(temp as f64) {
            env.meta
                .insert("temperature".into(), serde_json::Value::Number(num));
        }
    }
    if let Some(fmt) = &request.response_format {
        env.meta.insert(
            "response_format".into(),
            serde_json::Value::String(fmt.clone()),
        );
    }

    let mut meta =
        crate::flow_engine::dispatcher::FlowRequestMeta::new(uuid::Uuid::new_v4().to_string());
    if let Some(u) = user {
        meta.user_id = Some(u.user_id);
        meta.user_role = Some(u.role);
    }
    Ok((env, meta))
}

fn mime_for_filename(filename: &str) -> String {
    let ext = filename
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "wav" => "audio/wav".to_string(),
        "mp3" => "audio/mpeg".to_string(),
        "ogg" => "audio/ogg".to_string(),
        "flac" => "audio/flac".to_string(),
        "webm" => "audio/webm".to_string(),
        "m4a" | "mp4" => "audio/mp4".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

/// Stage 3d-0b-4: konwertuje FlowExecutionOutcome na TranscriptionResponse.
/// STT flow output to FlowValue::Text (transcript) z verbose polami w
/// envelope.meta (segments / duration / speakers / detected_language) —
/// SttNodeAdapter zapisuje je gdy backend zwrócił verbose_json.
pub(crate) fn flow_outcome_to_stt_response(
    outcome: crate::flow_engine::envelope::FlowExecutionOutcome,
) -> Result<TranscriptionResponse, ExecutorError> {
    use crate::flow_engine::envelope::FlowValue;
    let envelope = outcome.final_envelope;
    let text = match envelope.payload {
        FlowValue::Text(t) => t,
        FlowValue::Empty => String::new(),
        other => {
            return Err(ExecutorError::Internal(format!(
                "stt flow returned non-Text payload kind: {}",
                other.kind()
            )));
        }
    };
    let language = envelope
        .meta
        .get("detected_language")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let duration = envelope
        .meta
        .get("duration")
        .and_then(|v| v.as_f64())
        .map(|n| n as f32);
    let segments = envelope
        .meta
        .get("segments")
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    let speakers = envelope
        .meta
        .get("speakers")
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    Ok(TranscriptionResponse {
        text,
        task: None,
        language,
        duration,
        segments,
        speakers,
    })
}

/// Etap 3a: extract token usage z `ModelMetrics.detailed` gdy backend dostarczył
/// `DetailedMetrics::Completion`. Inny wariant (np. Embeddings dla embeddings
/// stream'a) lub brak `final_metrics` zwraca `None` — chunk wtedy bez `usage`,
/// klient z `include_usage=true` widzi brak (warn'em wpisany w
/// `apply_include_usage_split`).
/// Pomiar perf dla strumieni z ZEWNĘTRZNYCH serwisów (Docker vLLM/sglang,
/// natywny python-bundle przez HTTP, QUIC sidecar). Silniki embedded raportują
/// perf zmierzony wewnątrz silnika; serwis zewnętrzny nie ujawnia swojego
/// wewnętrznego czasu prefill, więc liczymy UCZCIWE wartości obserwowane po
/// stronie klienta:
///   - TTFT = zegar ścienny od dispatchu do PIERWSZEGO tokena z treścią. Dla
///     zdalnego serwisu to realna latencja (zawiera sieć + kolejkę serwera) — nie
///     zmyślamy wewnętrznego czasu prefill.
///   - decode tok/s = REALNE `completion_tokens` (z usage) / okno pierwszy→ostatni
///     token treści.
///   - prefill tok/s = REALNE `prompt_tokens` (z usage) / TTFT — uczciwe
///     przybliżenie przepustowości prefill: tokeny promptu przetworzone w oknie do
///     pierwszego tokena.
/// Gdy backend NIE zwraca realnych liczników w usage, NIE fabrykujemy ich z
/// długości tekstu — pomijamy tok/s (0.0 → UI pokazuje brak zamiast bzdury).
/// Perf doklejany jest do FINALNEGO chunku (tego, który niesie usage), żeby
/// przeszedł istniejącą ścieżką (LlmStreamChunk.perf → FlowExecutionOutcome.perf
/// → ChatStreamEnd).
struct ExternalPerfStream {
    rx: tokio::sync::mpsc::Receiver<CoreResult<ChatCompletionChunk>>,
}

impl ExternalPerfStream {
    /// `start` to znacznik DISPATCHU (sprzed wysłania requestu HTTP/QUIC), nie
    /// moment spawnu tego wrappera — inaczej TTFT/total gubiłyby czas wysłania
    /// nagłówków i odbioru odpowiedzi serwera. Klient mierzy realną latencję.
    fn new(
        mut inner: ExecutorChunkStream,
        start: std::time::Instant,
        recorder: Option<StreamMetricsRecorder>,
    ) -> Self {
        // Bounded (wysoki cap): normalne odpowiedzi mieszczą się bez tarcia, a
        // patologiczny zawieszony konsument dostaje backpressure zamiast OOM przy
        // długiej generacji. Eager-drain dalej stempluje czasy przy przybyciu.
        let (tx, rx) = tokio::sync::mpsc::channel(8192);
        // Eager drain: czytamy SSE z serwisu tak szybko jak przychodzi i
        // stemplujemy czasy TU (przybycie z vLLM), a nie przy poll konsumenta.
        // Bez tego leniwy strumień + backpressure TCP od przeglądarki rozciąga
        // okno dekodowania do tempa konsumpcji i decode tok/s spada ~4x poniżej
        // realnego tempa silnika. Timestampy domknięte w tasku drenującym są
        // niezależne od konsumenta, więc decode_tps odzwierciedla tempo generacji.
        tokio::spawn(async move {
            use futures::StreamExt;
            let mut first_token_at: Option<std::time::Instant> = None;
            let mut last_token_at: Option<std::time::Instant> = None;
            // Exact-once: metryka (success ALBO error) zapisywana DOKLADNIE RAZ na
            // strumien, nawet gdy backend wysle >1 chunk z usage.
            let mut recorded = false;
            loop {
                tokio::select! {
                    // Anti-hang: gdy konsument porzuci `rx`, kończymy nawet jeśli
                    // upstream (vLLM) stalluje — zwalniamy strumień HTTP/QUIC, nie
                    // wisimy w nieskończoność na `inner.next()`.
                    _ = tx.closed() => break,
                    item = inner.next() => {
                        match item {
                            Some(Ok(mut chunk)) => {
                                // Reasoning tokens ARE decode work (GPU/energy), so
                                // reasoning models (ds4, deepseek) that stream
                                // `reasoning_content` before any visible `content` must
                                // still mark TTFT / count as output — otherwise
                                // first_token_at never fires and decode_tps degenerates
                                // to 0 for an entire chain-of-thought.
                                let has_content = chunk.choices.iter().any(|c| {
                                    c.delta.content.as_deref().is_some_and(|s| !s.is_empty())
                                        || c.delta
                                            .reasoning_content
                                            .as_deref()
                                            .is_some_and(|s| !s.is_empty())
                                });
                                if has_content {
                                    let now = std::time::Instant::now();
                                    if first_token_at.is_none() {
                                        first_token_at = Some(now);
                                    }
                                    // Okno dekodowania domykamy na OSTATNIM tokenie treści,
                                    // nie na chunku usage (który dla vLLM/sglang przychodzi
                                    // za treścią).
                                    last_token_at = Some(now);
                                }
                                // Finalny chunk to ten, który niesie usage (vLLM/sglang
                                // usage-tail albo QUIC Done z final_metrics) — wtedy
                                // doklejamy perf.
                                if chunk.usage.is_some() && chunk.perf.is_none() {
                                    chunk.perf = Some(build_external_perf(
                                        start,
                                        first_token_at,
                                        last_token_at,
                                        chunk.usage.as_ref(),
                                    ));
                                    // Finalny chunk niesie usage + swiezo doklejony
                                    // perf — jedyny punkt streamu gdzie oba wspolistnieja,
                                    // wiec tu (raz) stemplujemy metryke modelu.
                                    if !recorded {
                                        if let Some(rec) = &recorder {
                                            if let (Some(u), Some(p)) =
                                                (chunk.usage.as_ref(), chunk.perf.as_ref())
                                            {
                                                rec.record(u, p);
                                                recorded = true;
                                            }
                                        }
                                    }
                                }
                                if tx.send(Ok(chunk)).await.is_err() {
                                    // Konsument odpadł → kończymy drenowanie (anti-hang).
                                    break;
                                }
                            }
                            Some(Err(e)) => {
                                let _ = tx.send(Err(e)).await;
                                break;
                            }
                            None => break,
                        }
                    }
                }
            }
            // Strumien skonczyl sie bez finalnego chunku usage (upstream error,
            // EOF bez usage-tail, albo drop konsumenta przez `tx.closed()`) —
            // zapisz wiersz BLEDU raz, zeby error-rate lapal porzucone/blednie
            // zakonczone strumienie. Guard `recorded` gwarantuje exact-once.
            if !recorded {
                if let Some(rec) = &recorder {
                    rec.record_error();
                }
            }
        });
        Self { rx }
    }
}

/// Buduje GenPerf z realnych liczników usage + zegara ściennego klienta.
/// Każda metryka jest albo realna/uczciwie przybliżona, albo pominięta (0.0)
/// gdy brak danych — żadnych fabrykowanych estymat. Timestampy są stemplowane
/// w tasku drenującym (przybycie chunku z serwisu), więc decode tok/s mierzy
/// tempo generacji vLLM, nie tempo konsumpcji przeglądarki.
fn build_external_perf(
    start: std::time::Instant,
    first_token_at: Option<std::time::Instant>,
    last_token_at: Option<std::time::Instant>,
    usage: Option<&crate::api::openai::types::Usage>,
) -> crate::api::openai::types::GenPerf {
    // TTFT: czas od dispatchu do pierwszego tokena treści. Realna,
    // klient-obserwowana latencja (sieć + kolejka + prefill serwera).
    let ttft_ms = first_token_at
        .map(|t| t.duration_since(start).as_millis() as u32)
        .unwrap_or(0);

    // Tylko REALNE liczniki z usage. Brak usage → 0 (pomijamy tok/s), nie
    // zgadujemy z liczby chunków/długości tekstu.
    let prompt_tokens = usage.map(|u| u.prompt_tokens).unwrap_or(0);
    let completion_tokens = usage.map(|u| u.completion_tokens).unwrap_or(0);

    // decode tok/s = (completion_tokens-1) / okno (pierwszy→ostatni token).
    // Dla N tokenów jest N-1 interwałów między pierwszym a ostatnim, zgodnie z
    // kontraktem GenPerf i local.rs (przy 1 tokenie decode_tps=0 — brak okna).
    let decode_tps = match (first_token_at, last_token_at) {
        (Some(first), Some(last)) if completion_tokens > 0 => {
            let secs = last.duration_since(first).as_secs_f32();
            if secs > 0.0 {
                completion_tokens.saturating_sub(1) as f32 / secs
            } else {
                0.0
            }
        }
        _ => 0.0,
    };

    // prefill tok/s = realne prompt_tokens / TTFT (uczciwe przybliżenie).
    let ttft_secs = (ttft_ms as f32) / 1000.0;
    let prefill_tps = if prompt_tokens > 0 && ttft_secs > 0.0 {
        prompt_tokens as f32 / ttft_secs
    } else {
        0.0
    };

    // total_ms: pełny czas od dispatchu do ostatniego tokena treści.
    let total_ms = last_token_at
        .map(|t| t.duration_since(start).as_millis() as u32)
        .unwrap_or(0);

    crate::api::openai::types::GenPerf {
        ttft_ms,
        prefill_tps,
        decode_tps,
        total_ms,
    }
}

impl Stream for ExternalPerfStream {
    type Item = CoreResult<ChatCompletionChunk>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

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

/// Jeden wspolny punkt liczenia fallbacku aliasu. `attempts` to numer proby,
/// na ktorej dispatch sie powiodl (1 = primary). Gdy > 1, primary realnie padl
/// i request zszedl na kandydata o pozycji `attempts - 1` — logujemy `warn!`
/// tu, zeby /v1, flow i addon (wszystkie idace przez ten executor) raportowaly
/// fallback przez ten sam mechanizm (anti-cicha-degradacja).
///
/// `requested_is_alias` (tania flaga z resolvera, bez zapytania DB w petli)
/// bramkuje metryke `alias_fallback_total{alias}`: liczymy ja WYLACZNIE gdy
/// `requested_model` to faktyczny alias. Zwykly model z wieloma instancjami
/// tez failuje miedzy kandydatami, ale jego nazwa nie jest aliasem — liczenie
/// go pod etykieta aliasowa zaszumialoby metryke. Nie-aliasowy fallback nadal
/// dostaje `warn!` (widocznosc degradacji), tylko bez inkrementu metryki.
fn note_fallback(
    requested_model: &str,
    requested_is_alias: bool,
    attempts: usize,
    target_tag: &'static str,
) {
    if attempts > 1 {
        if requested_is_alias {
            crate::services::runtime::alias_metrics::record_alias_fallback(requested_model);
        }
        tracing::warn!(
            model = %requested_model,
            is_alias = requested_is_alias,
            chain_position = attempts - 1,
            target_kind = target_tag,
            "primary niedostepny — zszedlem na fallback"
        );
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

/// Wersja rozkladu kubelkow histogramow (musi zgadzac sie z `bump_histogram_bucket`
/// w repository.rs). Bump z ta sama wersja laczy sie w ten sam wiersz rollupu.
const MODEL_METRICS_HISTOGRAM_VERSION: i64 = 1;

/// Znormalizowane wejscie jednego zapisu metryk modelu. Grupuje wymiary +
/// liczniki w jeden argument, zeby `bump_model_metric_row` nie mial dziesiatek
/// parametrow (clippy `too_many_arguments`). Wszystkie pola sa juz policzone
/// przez callera — ta warstwa tylko mapuje na struktury repozytorium.
struct ModelMetricInput<'a> {
    node_id: &'a str,
    org_id: &'a str,
    user_id: &'a str,
    model_id: &'a str,
    service_key: &'a str,
    backend: &'a str,
    modality: &'a str,
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
    embedding_tokens: i64,
    e2e_latency_ms: i64,
    /// Proba do histogramu TTFT — `Some` tylko dla realnego pomiaru (>0).
    ttft_sample: Option<i64>,
    /// Proba do histogramu decode tok/s — `Some` tylko dla realnego pomiaru (>0).
    decode_tps_sample: Option<f64>,
    /// Proba do histogramu e2e — `Some` tylko dla realnego pomiaru (>0).
    e2e_sample: Option<i64>,
    is_error: bool,
}

/// Jeden punkt zapisu do `model_metrics_rollup`. Metryki nie moga wywrocic
/// requestu — blad zapisu jest tylko logowany (`warn`). `hour_bucket` liczony
/// tutaj (RFC3339 przyciety do godziny), zeby kazdy caller mial identyczny format.
fn bump_model_metric_row(db: &crate::db::DbPool, input: &ModelMetricInput<'_>) {
    let hour_bucket = chrono::Utc::now().format("%Y-%m-%dT%H:00:00Z").to_string();
    let dims = crate::db::models::ModelMetricsDims {
        node_id: input.node_id,
        org_id: input.org_id,
        user_id: input.user_id,
        model_id: input.model_id,
        service_key: input.service_key,
        backend: input.backend,
        modality: input.modality,
        hour_bucket: &hour_bucket,
        histogram_version: MODEL_METRICS_HISTOGRAM_VERSION,
    };
    let counters = crate::db::models::ModelMetricsCounters {
        request_count: 1,
        success_count: if input.is_error { 0 } else { 1 },
        error_count: if input.is_error { 1 } else { 0 },
    };
    let tokens = crate::db::models::ModelMetricsTokens {
        prompt_tokens: input.prompt_tokens,
        completion_tokens: input.completion_tokens,
        total_tokens: input.total_tokens,
        embedding_tokens: input.embedding_tokens,
        audio_ms: 0,
        images: 0,
    };
    let times = crate::db::models::ModelMetricsTimes {
        // prefill/decode sumy czasow swiadomie pominiete: GenPerf niesie tok/s,
        // nie sekundy, a przeliczanie ich na czas byloby zgadywaniem. Histogramy
        // ttft/decode_tps niosa realny obraz wydajnosci. queue_ms brak pomiaru.
        prefill_secs: 0.0,
        decode_secs: 0.0,
        e2e_latency_ms: input.e2e_latency_ms,
        queue_ms: 0,
    };
    let perf = crate::db::models::ModelMetricsPerfSamples {
        ttft_ms: input.ttft_sample,
        decode_tps: input.decode_tps_sample,
        e2e_ms: input.e2e_sample,
    };
    if let Err(e) = crate::db::repository::bump_model_metrics_rollup(
        db, &dims, &counters, &tokens, &times, &perf,
    ) {
        tracing::warn!(
            model_id = input.model_id,
            error = %e,
            "model metrics rollup bump failed (metrics dropped, request unaffected)"
        );
    }
}

/// Lekki uchwyt do zapisu metryk ze strumienia. `ExternalPerfStream` drenuje
/// chunki w osobnym tasku (bez `ctx`/`target`), wiec wymiary klonujemy do prostych
/// `String` przy budowie streamu i stemplujemy metryke na finalnym chunku (tym,
/// ktory niesie `usage` + doklejony `perf`). Modalnosc zawsze `chat` — to jedyna
/// sciezka streamujaca przez ten wrapper.
#[derive(Clone)]
struct StreamMetricsRecorder {
    db: crate::db::DbPool,
    node_id: String,
    org_id: String,
    user_id: String,
    model_id: String,
    service_key: String,
    backend: String,
}

impl StreamMetricsRecorder {
    fn record(
        &self,
        usage: &crate::api::openai::types::Usage,
        perf: &crate::api::openai::types::GenPerf,
    ) {
        let e2e_ms = perf.total_ms as i64;
        bump_model_metric_row(
            &self.db,
            &ModelMetricInput {
                node_id: &self.node_id,
                org_id: &self.org_id,
                user_id: &self.user_id,
                model_id: &self.model_id,
                service_key: &self.service_key,
                backend: &self.backend,
                modality: "chat",
                prompt_tokens: usage.prompt_tokens as i64,
                completion_tokens: usage.completion_tokens as i64,
                total_tokens: usage.total_tokens as i64,
                embedding_tokens: 0,
                e2e_latency_ms: e2e_ms,
                ttft_sample: (perf.ttft_ms > 0).then_some(perf.ttft_ms as i64),
                decode_tps_sample: (perf.decode_tps > 0.0).then_some(perf.decode_tps as f64),
                e2e_sample: (e2e_ms > 0).then_some(e2e_ms),
                is_error: false,
            },
        );
    }

    /// Wiersz BLEDU dla strumienia, ktory zakonczyl sie bez finalnego chunku
    /// usage: upstream error, EOF bez usage-tail, albo drop konsumenta
    /// (`tx.closed()`). Bez tokenow/perf — liczy sie wylacznie do error-rate.
    /// Rozroznienie "drop konsumenta" vs "blad backendu" jest tu niepewne, wiec
    /// oba traktujemy jako error (lepsze niz gubienie porzuconych strumieni).
    fn record_error(&self) {
        bump_model_metric_row(
            &self.db,
            &ModelMetricInput {
                node_id: &self.node_id,
                org_id: &self.org_id,
                user_id: &self.user_id,
                model_id: &self.model_id,
                service_key: &self.service_key,
                backend: &self.backend,
                modality: "chat",
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                embedding_tokens: 0,
                e2e_latency_ms: 0,
                ttft_sample: None,
                decode_tps_sample: None,
                e2e_sample: None,
                is_error: true,
            },
        );
    }
}

/// HF repo dla embedded TTS na podstawie `engine_id` + `model_name` (preset id
/// z katalogu). Mapowanie preset→repo zyje w manifescie silnika; gdy go nie ma,
/// `model_name` traktujemy jako bezposrednie repo (single-voice deploy).
fn resolve_embedded_tts_repo(engine_id: &str, model_name: &str) -> String {
    if let Some(manifest) = crate::services::manifest::registry().by_id(engine_id) {
        if let Some(preset) = manifest
            .model_presets
            .iter()
            .find(|p| p.id == model_name)
            .or_else(|| manifest.model_presets.iter().find(|p| p.recommended))
            .or_else(|| manifest.model_presets.first())
        {
            return preset.repo.clone();
        }
    }
    model_name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Chunk 2: `bump_model_metric_row` mapuje `ModelMetricInput` na wiersz
    /// `model_metrics_rollup` — sprawdzamy wymiary (model/backend/service_key),
    /// liczniki (sukces vs `is_error`), `embedding_tokens` dla modalnosci
    /// embedding oraz bramkowanie histogramow (perf `None` → zero probek).
    #[test]
    fn bump_model_metric_row_maps_dims_counters_and_perf() {
        let pool = crate::db::init(std::path::Path::new(":memory:")).expect("init test db");

        // Sukces chat, brak perf → request+success policzone, histogramy nietkniete.
        bump_model_metric_row(
            &pool,
            &ModelMetricInput {
                node_id: "node-A",
                org_id: crate::services::org::DEFAULT_ORG_ID,
                user_id: "u1",
                model_id: "qwen-chat",
                service_key: "http/qwen-chat",
                backend: "http",
                modality: "chat",
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                embedding_tokens: 0,
                e2e_latency_ms: 120,
                ttft_sample: None,
                decode_tps_sample: None,
                e2e_sample: Some(120),
                is_error: false,
            },
        );

        let rows = crate::db::repository::list_model_metrics_rollup(
            &pool,
            crate::services::org::DEFAULT_ORG_ID,
            &Default::default(),
        )
        .expect("list rollup");
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.node_id, "node-A");
        assert_eq!(row.model_id, "qwen-chat");
        assert_eq!(row.backend, "http");
        assert_eq!(row.service_key, "http/qwen-chat");
        assert_eq!(row.modality, "chat");
        assert_eq!(row.request_count, 1);
        assert_eq!(row.success_count, 1);
        assert_eq!(row.error_count, 0);
        assert_eq!(row.total_tokens, 15);
        assert_eq!(row.embedding_tokens, 0);
        // perf None → brak probki w histogramach, ale e2e_sample Some → jeden e2e.
        assert_eq!(row.ttft_sample_count, 0);
        assert_eq!(row.decode_tps_sample_count, 0);
        assert_eq!(row.e2e_sample_count, 1);

        // Blad w tym samym kubelku wymiarow → error_count rosnie, success nie.
        bump_model_metric_row(
            &pool,
            &ModelMetricInput {
                node_id: "node-A",
                org_id: crate::services::org::DEFAULT_ORG_ID,
                user_id: "u1",
                model_id: "qwen-chat",
                service_key: "http/qwen-chat",
                backend: "http",
                modality: "chat",
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                embedding_tokens: 0,
                e2e_latency_ms: 0,
                ttft_sample: None,
                decode_tps_sample: None,
                e2e_sample: None,
                is_error: true,
            },
        );
        let row = &crate::db::repository::list_model_metrics_rollup(
            &pool,
            crate::services::org::DEFAULT_ORG_ID,
            &Default::default(),
        )
        .expect("list rollup")[0];
        assert_eq!(row.request_count, 2);
        assert_eq!(row.success_count, 1);
        assert_eq!(row.error_count, 1);

        // Embedding: `embedding_tokens` == total_tokens dla modalnosci embedding
        // (osobny kubelek — inny modality/model → nowy wiersz).
        bump_model_metric_row(
            &pool,
            &ModelMetricInput {
                node_id: "node-A",
                org_id: crate::services::org::DEFAULT_ORG_ID,
                user_id: "u1",
                model_id: "bge-embed",
                service_key: "embedded/bge-embed",
                backend: "embedded",
                modality: "embedding",
                prompt_tokens: 8,
                completion_tokens: 0,
                total_tokens: 8,
                embedding_tokens: 8,
                e2e_latency_ms: 5,
                ttft_sample: None,
                decode_tps_sample: None,
                e2e_sample: Some(5),
                is_error: false,
            },
        );
        let rows = crate::db::repository::list_model_metrics_rollup(
            &pool,
            crate::services::org::DEFAULT_ORG_ID,
            &Default::default(),
        )
        .expect("list rollup");
        let emb = rows
            .iter()
            .find(|r| r.model_id == "bge-embed")
            .expect("embedding row present");
        assert_eq!(emb.modality, "embedding");
        assert_eq!(emb.embedding_tokens, 8);
        assert_eq!(emb.total_tokens, 8);
    }

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
        assert!(!ExecutorError::AllCandidatesFailed {
            target_kind: "x",
            attempts: 1,
            last_error: "y".into(),
        }
        .aborts_fallback_chain());
        assert!(!ExecutorError::Internal("z".into()).aborts_fallback_chain());
        assert!(!ExecutorError::FlowEmptyResult { model: "m".into() }.aborts_fallback_chain());
    }

    /// `note_fallback` bramkuje metryke `alias_fallback_total{model}` flaga
    /// `requested_is_alias`: aliasowy fallback liczy sie raz, nie-aliasowy
    /// (zwykly model z wieloma instancjami) NIE inkrementuje metryki aliasowej.
    /// Primary trafiony (attempts=1) nigdy nie liczy fallbacku.
    #[test]
    fn note_fallback_only_counts_real_aliases() {
        use crate::services::runtime::alias_metrics::alias_fallback_count;

        let alias = "note-fallback-alias-test";
        let plain = "note-fallback-plain-model-test";

        // Primary trafiony — zaden fallback, zero inkrementu nawet dla aliasu.
        note_fallback(alias, true, 1, "embedded");
        assert_eq!(alias_fallback_count(alias), 0);

        // Aliasowy fallback (pozycja 1) liczy sie raz.
        note_fallback(alias, true, 2, "mesh_forward");
        assert_eq!(alias_fallback_count(alias), 1);

        // Nie-aliasowy fallback (zwykly model z wieloma kandydatami) NIE
        // inkrementuje metryki aliasowej — etykieta zostaje czysta.
        note_fallback(plain, false, 2, "http");
        note_fallback(plain, false, 3, "embedded");
        assert_eq!(alias_fallback_count(plain), 0);

        // Kolejny aliasowy fallback inkrementuje dalej (do 2).
        note_fallback(alias, true, 3, "http");
        assert_eq!(alias_fallback_count(alias), 2);
    }

    /// Test regresyjny odsprzęgania pomiaru decode tok/s od tempa konsumpcji.
    /// `ExternalPerfStream` stempluje first/last token w momencie PRZYBYCIA chunku
    /// z serwisu (eager-drain task), więc decode_tps odzwierciedla tempo generacji
    /// silnika, NIE tempo z jakim leniwy konsument (przeglądarka renderująca
    /// markdown) odczytuje strumień. Symulujemy serwis emitujący ~200 tok/s i
    /// konsumenta odczytującego ~40 tok/s — decode_tps MUSI być bliskie tempu
    /// emisji. Gdyby okno dekodowania szło w tempie poll konsumenta (stary, zły
    /// kod), decode_tps wyszłoby ~40. Multi-thread runtime jest KLUCZOWY: bez
    /// równoległości eager-drain task nie biegłby naprawdę obok wolnego konsumenta.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn external_perf_decouples_decode_tps_from_slow_consumer() {
        use crate::api::openai::types::{ChunkChoice, Delta, Usage};
        use futures::StreamExt;
        use std::time::{Duration, Instant};

        fn content_chunk(text: &str) -> ChatCompletionChunk {
            ChatCompletionChunk {
                id: "test".into(),
                object: "chat.completion.chunk".into(),
                created: 0,
                model: "test-model".into(),
                choices: vec![ChunkChoice {
                    index: 0,
                    delta: Delta {
                        role: None,
                        content: Some(text.into()),
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
                perf: None,
            }
        }

        // Serwis emituje 100 chunków treści po ~5ms (≈200 tok/s), potem chunk
        // usage (bez treści, completion_tokens=100), na końcu bez perf.
        let inner = async_stream::stream! {
            for _ in 0..100 {
                tokio::time::sleep(Duration::from_millis(5)).await;
                yield Ok(content_chunk("x"));
            }
            let mut tail = content_chunk("");
            tail.choices[0].delta.content = None;
            tail.usage = Some(Usage {
                prompt_tokens: 10,
                completion_tokens: 100,
                total_tokens: 110,
            });
            yield Ok(tail);
        };
        let inner: ExecutorChunkStream = Box::pin(inner);

        let mut stream = ExternalPerfStream::new(inner, Instant::now(), None);

        // Wolny konsument: 25ms na token (≈40 tok/s) — gdyby pomiar szedł w jego
        // tempie, decode_tps spadłoby ~5x poniżej realnego tempa silnika.
        let mut captured_perf = None;
        while let Some(item) = stream.next().await {
            let chunk = item.expect("chunk powinien być Ok");
            if chunk.perf.is_some() {
                captured_perf = chunk.perf;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        let perf = captured_perf.expect("finalny chunk z usage musi nieść perf");
        eprintln!(
            "external_perf: decode_tps={:.1} ttft_ms={} total_ms={}",
            perf.decode_tps, perf.ttft_ms, perf.total_ms
        );

        // decode_tps odzwierciedla tempo emisji (~200/s), nie konsumpcji (~40/s).
        // Próg 120 z zapasem na jitter timerów; gdyby okno szło w tempie
        // konsumenta, wyszłoby ~40 i asercja by padła.
        assert!(
            perf.decode_tps > 120.0,
            "decode_tps={} powinno odzwierciedlac tempo emisji ~200/s, nie konsumpcji ~40/s",
            perf.decode_tps
        );
        // Sanity: pełne okno i TTFT są dodatnie.
        assert!(perf.total_ms > 0, "total_ms powinno być dodatnie");
        assert!(perf.ttft_ms > 0, "ttft_ms powinno być dodatnie");
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
        let local_inference = Arc::new(crate::inference::local::LocalInferenceHandler::new(
            crate::inference::shared_inference_manager(),
        ));
        let stt_slot = Arc::new(parking_lot::RwLock::new(None));
        let mesh_slot = Arc::new(parking_lot::RwLock::new(None));
        ModelRuntimeExecutor::new(
            catalog,
            resolver,
            None,
            local_inference,
            stt_slot,
            mesh_slot,
            Arc::new(parking_lot::RwLock::new(None)),
            None,
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
        let target = ResolvedExecutionTarget::Local {
            service_id: 1,
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
            flow_id: "1".to_string(),
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
            perf: None,
            finish_reason: crate::flow_engine::envelope::FinishReason::Stop,
            total_latency_ms: 0,
            error: None,
        }
    }

    fn batch_request(model: &str, count: usize) -> EmbeddingRequest {
        EmbeddingRequest {
            model: model.to_string(),
            input: EmbeddingInput::Multiple((0..count).map(|i| format!("text-{i}")).collect()),
            encoding_format: None,
            dimensions: None,
            user: None,
        }
    }

    /// Single-vector outcome trafia do `data[0]` z `index=0`.
    #[test]
    fn flow_outcome_extracts_single_embedding_for_single_input() {
        let request = make_request("any");
        let outcome =
            outcome_with_payload(crate::flow_engine::envelope::FlowValue::Embedding(vec![
                0.1, 0.2, 0.3,
            ]));
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

    // RAG C2: per-target `dispatch_rerank_blocking` tests. Embedded has no
    // reranking surface (chain moves on); MeshForward defers to pending
    // cutover; Flow without a dispatcher surfaces the typed error.

    fn make_rerank_request(model: &str) -> RerankRequest {
        RerankRequest {
            model: model.to_string(),
            query: "what is rust".into(),
            documents: vec!["doc a".into(), "doc b".into()],
            top_n: Some(1),
        }
    }

    #[tokio::test]
    async fn rerank_embedded_is_unsupported_so_chain_moves_on() {
        let exec = dummy_executor();
        let target = ResolvedExecutionTarget::Local {
            service_id: 1,
            model_name: "rerank-m".into(),
            handle: BackendHandle::Embedded {
                model_name: "rerank-m".into(),
                node_id: "local".into(),
                engine_id: "test-engine".into(),
            },
        };
        let mut ctx = ExecutionContext::default();
        let err = exec
            .dispatch_rerank_blocking(&target, make_rerank_request("rerank-m"), &mut ctx)
            .await
            .expect_err("embedded reranking is unsupported");
        // Internal (not abort) → the failover loop keeps trying later candidates.
        assert!(matches!(err, ExecutorError::Internal(_)));
        assert!(!err.aborts_fallback_chain());
    }

    #[tokio::test]
    async fn rerank_mesh_forward_returns_pending_cutover() {
        let exec = dummy_executor();
        let target = ResolvedExecutionTarget::MeshForward {
            node_id: "peer".into(),
            service_id: 1,
            model_name: "rerank-m".into(),
        };
        let mut ctx = ExecutionContext::default();
        let err = exec
            .dispatch_rerank_blocking(&target, make_rerank_request("rerank-m"), &mut ctx)
            .await
            .expect_err("mesh_forward branch should be pending cutover");
        assert!(matches!(
            err,
            ExecutorError::TransportPendingCutover("mesh_forward")
        ));
    }

    #[tokio::test]
    async fn rerank_flow_without_dispatcher_returns_typed_error() {
        let exec = dummy_executor();
        let target = ResolvedExecutionTarget::Flow {
            flow_id: "1".to_string(),
            published_name: "rerank-flow".into(),
        };
        let mut ctx = ExecutionContext::default();
        let err = exec
            .dispatch_rerank_blocking(&target, make_rerank_request("any"), &mut ctx)
            .await
            .expect_err("flow without dispatcher should be a typed error");
        assert!(matches!(err, ExecutorError::FlowDispatcherUnavailable));
    }

    /// `execute_rerank` for an unknown model surfaces the resolver error rather
    /// than panicking — the empty catalog of `dummy_executor` has no rerank
    /// service, so resolution fails cleanly.
    #[tokio::test]
    async fn execute_rerank_unknown_model_surfaces_resolve_error() {
        let exec = dummy_executor();
        let mut ctx = ExecutionContext::default();
        let err = exec
            .execute_rerank(make_rerank_request("no-such-reranker"), &mut ctx)
            .await
            .expect_err("unknown rerank model must error, not panic");
        assert!(matches!(err, ExecutorError::Resolve(_)));
    }

    /// Flow rerank outcome → `RerankResponse`: maps string `id` back to the
    /// original index and reads `score`. Proves the rerank-as-flow contract
    /// used by `dispatch_rerank_blocking`'s Flow branch.
    #[test]
    fn flow_outcome_to_rerank_maps_id_and_score() {
        let outcome = outcome_with_payload(crate::flow_engine::envelope::FlowValue::Json(
            serde_json::json!({ "ranked": [
                { "id": "1", "score": 0.9, "text": "b" },
                { "id": "0", "score": 0.4, "text": "a" }
            ]}),
        ));
        let resp = flow_outcome_to_rerank_response(outcome).expect("flow rerank maps");
        assert_eq!(resp.results.len(), 2);
        assert_eq!(resp.results[0].index, 1);
        assert!((resp.results[0].relevance_score - 0.9).abs() < 1e-6);
        assert_eq!(resp.results[1].index, 0);
    }

    // RAG E1.2: per-target `dispatch_documents_blocking` tests. Embedded jest
    // gniazdem (Internal → chain moves on, NIE crash); MeshForward defers do
    // pending cutover; Flow bez dispatchera surface'uje typed error.

    fn make_doc_request(model: &str) -> DocumentParseRequest {
        DocumentParseRequest {
            model: model.to_string(),
            image_bytes: vec![0x89, 0x50, 0x4e, 0x47],
            mime: "image/png".into(),
            flow_depth: 0,
        }
    }

    #[tokio::test]
    async fn documents_embedded_is_socket_so_chain_moves_on() {
        let exec = dummy_executor();
        let target = ResolvedExecutionTarget::Local {
            service_id: 1,
            model_name: "parse-m".into(),
            handle: BackendHandle::Embedded {
                model_name: "parse-m".into(),
                node_id: "local".into(),
                engine_id: "test-engine".into(),
            },
        };
        let mut ctx = ExecutionContext::default();
        let err = exec
            .dispatch_documents_blocking(&target, make_doc_request("parse-m"), &mut ctx)
            .await
            .expect_err("embedded document parse is a socket (TBD)");
        // Internal (nie abort) → failover schodzi na zdalny backend, nie crash.
        assert!(matches!(err, ExecutorError::Internal(_)));
        assert!(!err.aborts_fallback_chain());
    }

    #[tokio::test]
    async fn documents_mesh_forward_returns_pending_cutover() {
        let exec = dummy_executor();
        let target = ResolvedExecutionTarget::MeshForward {
            node_id: "peer".into(),
            service_id: 1,
            model_name: "parse-m".into(),
        };
        let mut ctx = ExecutionContext::default();
        let err = exec
            .dispatch_documents_blocking(&target, make_doc_request("parse-m"), &mut ctx)
            .await
            .expect_err("mesh_forward branch should be pending cutover");
        assert!(matches!(
            err,
            ExecutorError::TransportPendingCutover("mesh_forward")
        ));
    }

    #[tokio::test]
    async fn documents_flow_without_dispatcher_returns_typed_error() {
        let exec = dummy_executor();
        let target = ResolvedExecutionTarget::Flow {
            flow_id: "1".to_string(),
            published_name: "parse-flow".into(),
        };
        let mut ctx = ExecutionContext::default();
        let err = exec
            .dispatch_documents_blocking(&target, make_doc_request("any"), &mut ctx)
            .await
            .expect_err("flow without dispatcher should be a typed error");
        assert!(matches!(err, ExecutorError::FlowDispatcherUnavailable));
    }

    /// RAG E1.4 — buduje `DocumentParseRequest` z realnym, minimalnym PDF
    /// (jedna strona A4) i mime `application/pdf`.
    fn make_pdf_request(model: &str) -> DocumentParseRequest {
        let pdf = crate::services::document::rasterize::minimal_pdf(1);
        DocumentParseRequest {
            model: model.to_string(),
            image_bytes: pdf,
            mime: crate::services::document::PDF_MIME.to_string(),
            flow_depth: 0,
        }
    }

    /// RAG E1.4 — z feature `pdf` mime `application/pdf` wchodzi w ścieżkę
    /// rasteryzacji (`execute_documents_pdf`): PDF jest renderowany do obrazów,
    /// a parse per-strona idzie przez resolver. Na pustym katalogu pierwsza
    /// strona surface'uje `Resolve` (brak serwisu documents) — to dowód, że
    /// rasteryzacja się powiodła (gdyby pdfium padł, dostalibyśmy `Internal`).
    #[tokio::test]
    async fn execute_documents_pdf_rasterizes_then_resolves_per_page() {
        let exec = dummy_executor();
        let mut ctx = ExecutionContext::default();
        let err = exec
            .execute_documents(make_pdf_request("rag-parse"), &mut ctx)
            .await
            .expect_err("pusty katalog → resolve error po rasteryzacji");
        assert!(
            matches!(err, ExecutorError::Resolve(_)),
            "po udanej rasteryzacji per-strona idzie przez resolver: {err:?}"
        );
    }

    /// `execute_documents` dla aliasu `rag-parse` na pustym katalogu surface'uje
    /// błąd resolvera (brak serwisu documents), nie panikuje — to ten sam
    /// failover-aware resolve co rerank `rag-reranker`.
    #[tokio::test]
    async fn execute_documents_unknown_alias_surfaces_resolve_error() {
        let exec = dummy_executor();
        let mut ctx = ExecutionContext::default();
        let err = exec
            .execute_documents(make_doc_request("rag-parse"), &mut ctx)
            .await
            .expect_err("unknown parse alias must error, not panic");
        assert!(matches!(err, ExecutorError::Resolve(_)));
    }

    /// Flow parse outcome → `DocumentParseResponse`: czyta markdown + bloki z
    /// `Json{markdown, blocks}`. Brakujące `confidence`/`page` → None/0.
    #[test]
    fn flow_outcome_to_document_maps_markdown_and_blocks() {
        let outcome = outcome_with_payload(crate::flow_engine::envelope::FlowValue::Json(
            serde_json::json!({
                "markdown": "# Faktura",
                "blocks": [
                    { "class": "Title", "bbox": [1.0, 2.0, 3.0, 4.0], "text": "# Faktura", "confidence": 0.9 },
                    { "class": "Text", "bbox": [1.0, 5.0, 3.0, 8.0], "text": "Kwota" }
                ]
            }),
        ));
        let resp = flow_outcome_to_document_response(outcome).expect("flow parse maps");
        assert_eq!(resp.markdown, "# Faktura");
        assert_eq!(resp.blocks.len(), 2);
        assert_eq!(resp.blocks[0].class, "Title");
        assert_eq!(resp.blocks[0].bbox, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(resp.blocks[0].confidence, Some(0.9));
        assert_eq!(resp.blocks[1].page, 0);
        assert_eq!(resp.blocks[1].confidence, None);
    }

    /// `parse_blocks_json` (współdzielony przez ścieżkę HTTP i flow) tolerancyjnie
    /// dekoduje kształt z serwisu `{markdown, blocks:[{class,bbox,text}]}`.
    #[test]
    fn parse_blocks_json_decodes_service_shape() {
        let v = serde_json::json!([
            { "class": "Table", "bbox": [0.0, 0.0, 10.0, 10.0], "text": "<table></table>" }
        ]);
        let blocks = parse_blocks_json(Some(&v));
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].class, "Table");
        assert_eq!(blocks[0].text, "<table></table>");
        assert_eq!(blocks[0].bbox, [0.0, 0.0, 10.0, 10.0]);
        assert!(parse_blocks_json(None).is_empty());
    }

    /// `parse_blocks_json` POMIJA złe bloki (brak `class`/`text`, `bbox` ≠ 4
    /// liczby, `page` poza u32), a dobre zostawia — walidacja-i-pomiń zamiast
    /// cichej korupcji (puste pola / zerowany bbox / zawinięty page).
    #[test]
    fn parse_blocks_json_skips_malformed_keeps_valid() {
        let v = serde_json::json!([
            // dobry blok
            { "class": "Text", "bbox": [0.0, 0.0, 1.0, 1.0], "text": "ok", "page": 0 },
            // brak `class`
            { "bbox": [0.0, 0.0, 1.0, 1.0], "text": "no class" },
            // brak `text`
            { "class": "Text", "bbox": [0.0, 0.0, 1.0, 1.0] },
            // `bbox` o złej długości (3 elementy)
            { "class": "Text", "bbox": [0.0, 0.0, 1.0], "text": "short bbox" },
            // `bbox` z nie-liczbą
            { "class": "Text", "bbox": [0.0, 0.0, 1.0, "x"], "text": "bad bbox elem" },
            // `page` poza zakresem u32
            { "class": "Text", "bbox": [0.0, 0.0, 1.0, 1.0], "text": "huge page", "page": 4294967296u64 },
            // drugi dobry blok
            { "class": "Title", "bbox": [2.0, 3.0, 4.0, 5.0], "text": "ok2", "page": 7 },
        ]);
        let blocks = parse_blocks_json(Some(&v));
        assert_eq!(blocks.len(), 2, "tylko 2 poprawne bloki wchodzą do wyniku");
        assert_eq!(blocks[0].text, "ok");
        assert_eq!(blocks[0].page, 0);
        assert_eq!(blocks[1].text, "ok2");
        assert_eq!(blocks[1].page, 7);
        assert_eq!(blocks[1].bbox, [2.0, 3.0, 4.0, 5.0]);
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
            flow_id: "1".to_string(),
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
