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

use crate::api::openai::types::{ChatCompletionRequest, ChatCompletionResponse};
use crate::flow_engine::dispatcher::FlowDispatcher;
use crate::services::catalog::{CatalogProvider, InputModality, OutputModality, ServiceSurface};
use crate::services::handles_cache::BackendHandle;
use crate::services::runtime::context::ExecutionContext;
use crate::services::runtime::middleware::StreamMiddlewareFactory;
use crate::services::runtime::resolver::{AliasResolver, ResolveError, ResolveRequest};
use crate::services::runtime::strategy::{rank, StrategyState};
use crate::services::runtime::target::ResolvedExecutionTarget;

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
    #[error("internal error: {0}")]
    Internal(String),
}

impl ExecutorError {
    /// Should the caller stop iterating fallback candidates after seeing
    /// this error? `true` for config-level failures that the next
    /// candidate cannot fix. Transient transport failures (HTTP 5xx,
    /// QUIC reconnect) keep iterating.
    fn aborts_fallback_chain(&self) -> bool {
        matches!(self, Self::FlowDispatcherUnavailable)
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
    local_inference: Arc<crate::routing::local_inference::LocalInferenceHandler>,
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
        local_inference: Arc<crate::routing::local_inference::LocalInferenceHandler>,
        middleware: Vec<Arc<dyn StreamMiddlewareFactory>>,
    ) -> Self {
        Self {
            catalog,
            resolver,
            flow_dispatcher,
            local_inference,
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

        Err(ExecutorError::AllCandidatesFailed {
            target_kind: last_kind,
            attempts,
            last_error: last_err.unwrap_or_else(|| "no candidates after rank".into()),
        })
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
                BackendHandle::Quic(_) => {
                    Err(ExecutorError::TransportPendingCutover("quic_sidecar"))
                }
            },
            ResolvedExecutionTarget::MeshForward { .. } => {
                // Forwarding to a peer requires verifying the destination
                // is in our trusted-keys set before dialling — adding
                // mesh transport here without that check would let a
                // tampered catalog snapshot redirect requests to any
                // node id. Wire trust verification together with the
                // transport plumbing.
                Err(ExecutorError::TransportPendingCutover("mesh_forward"))
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
                    let flow_ctx = crate::routing::build_flow_context_for_user(
                        &request, false, user,
                    );
                    dispatcher.dispatch_by_flow_id(*flow_id, flow_ctx).await
                };
                ctx.leave_flow();

                let result = dispatch_result
                    .map_err(|e| ExecutorError::Internal(e.to_string()))?
                    .ok_or_else(|| ExecutorError::FlowEmptyResult {
                        model: published_name.clone(),
                    })?;

                Ok(crate::routing::chat::flow_result_to_chat_response(
                    result,
                    &request.model,
                ))
            }
        }
    }

    /// Public accessor for the configured middleware factory list. The
    /// streaming entry points walk this list and call `start_session`
    /// per request to materialise an isolated stack — never share the
    /// returned `Vec` itself across streams.
    pub fn middleware_factories(&self) -> &[Arc<dyn StreamMiddlewareFactory>] {
        &self.middleware
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
        assert!(!ExecutorError::TransportPendingCutover("x").aborts_fallback_chain());
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
}

