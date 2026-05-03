// =============================================================================
// Plik: routing/embeddings.rs
// Opis: Obsluga zapytan o embeddingi — `route_embeddings_for_user`
//       deleguje przez `ModelRuntimeExecutor.execute_embeddings`; legacy
//       mesh fallback trzymany do R3b.7 (mesh transport w executor).
//       `route_embeddings_via_quic` to protocol-native API uzywane przez
//       mesh reverse handler (`mesh/inference_proxy.rs`).
// =============================================================================

use crate::api::openai::types::{
    EmbeddingData, EmbeddingInput, EmbeddingRequest, EmbeddingResponse, EmbeddingUsage,
};
use crate::error::{CoreError, Result};
use crate::routing::router::Router;

use tentaflow_protocol::*;
use tracing::debug;

impl Router {
    /// Routuje embeddings request do odpowiedniego backendu.
    ///
    /// Wariant z user context — sprawdza ACL ('model', request.model) zanim
    /// uderzymy w backend. Maskujemy denied jako AllBackendsUnavailable
    /// zeby nie ujawniac istnienia modelu.
    pub async fn route_embeddings_for_user(
        &self,
        request: EmbeddingRequest,
        user: Option<crate::auth::acl::UserContext>,
    ) -> Result<crate::routing::RouteResult<EmbeddingResponse>> {
        if let Some(ref u) = user {
            if let Some(ref db) = self.db {
                if !crate::auth::acl::check_access_safe(
                    db,
                    "model",
                    &request.model,
                    u.user_id,
                    &u.role,
                ) {
                    tracing::warn!(user_id = u.user_id, model = %request.model, "ACL denied embedding model");
                    return Err(crate::error::CoreError::AllBackendsUnavailable {
                        model_name: request.model.clone(),
                    }
                    .into());
                }
            }
        }
        self.route_embeddings_inner(request, user).await
    }

    /// Obsluguje zarowno Single jak i Multiple input. Sciezka glowna idzie
    /// przez `ModelRuntimeExecutor::execute_embeddings` (single source of
    /// truth dla alias resolution + strategy + per-instance modality
    /// filter). MeshForward executor zwraca `TransportPendingCutover` —
    /// wracamy wtedy do legacy `dispatch_with_fallback` na pojedynczy
    /// MeshForward branch. Po R3b.7 (mesh w executor) ten fallback zniknie
    /// razem z `routing::middleware::BackendHandle`.
    ///
    /// `user` is propagated into `ExecutionContext.user` so the flow ACL
    /// gate in `dispatch_by_flow_id` sees the user_id/role. Without this
    /// embedding-surface flows would skip the per-flow ACL check.
    async fn route_embeddings_inner(
        &self,
        request: EmbeddingRequest,
        user: Option<crate::auth::acl::UserContext>,
    ) -> Result<crate::routing::RouteResult<EmbeddingResponse>> {
        debug!("Routing embeddings dla modelu: {}", request.model);

        let t = std::time::Instant::now();
        let executor_snapshot = self.executor.read().clone();
        if let Some(executor) = executor_snapshot {
            use crate::services::runtime::context::ExecutionContext;
            use crate::services::runtime::executor::ExecutorError;

            let mut exec_ctx = ExecutionContext {
                user: user.clone(),
                ..ExecutionContext::default()
            };
            match executor
                .execute_embeddings(request.clone(), &mut exec_ctx)
                .await
            {
                Ok(response) => {
                    let metadata = crate::routing::RouteMetadata {
                        served_by_node: exec_ctx
                            .route_metadata
                            .served_by_node
                            .unwrap_or_else(|| {
                                hostname::get()
                                    .map(|h| h.to_string_lossy().to_string())
                                    .unwrap_or_else(|_| "unknown".to_string())
                            }),
                        backend_type: exec_ctx
                            .route_metadata
                            .backend_type
                            .unwrap_or_else(|| "executor".to_string()),
                        strategy_used: "executor".to_string(),
                        fallbacks_tried: exec_ctx.route_metadata.fallbacks_tried,
                        hop_count: 0,
                        latency_ms: Some(t.elapsed().as_secs_f64() * 1000.0),
                    };
                    return Ok(crate::routing::RouteResult { response, metadata });
                }
                Err(ExecutorError::TransportPendingCutover(_)) => {
                    debug!(
                        "executor returned TransportPendingCutover for embeddings — falling back to legacy mesh dispatch"
                    );
                    // fall through to legacy dispatch below
                }
                Err(e) => return Err(executor_err_to_core(e, &request.model).into()),
            }
        }

        // Legacy mesh-only fallback. Po R3b.7 mesh transport bedzie w
        // executor; ten branch wyparuje razem z `BackendHandle::MeshForward`.
        self.legacy_embeddings_mesh_dispatch(request).await
    }

    /// Mesh-only legacy fallback dla `route_embeddings`. Iteruje po
    /// MeshForward kandydatach z `dispatch_with_fallback` i wysyla
    /// `EmbeddingsPayload` przez `forward_model_request_to_mesh`. Pozostale
    /// branche (HTTP/QuicEmbedding) sa obslugiwane przez executor — tutaj
    /// zostaje tylko mesh, zeby uniknac duplikacji logiki dispatch.
    async fn legacy_embeddings_mesh_dispatch(
        &self,
        request: EmbeddingRequest,
    ) -> Result<crate::routing::RouteResult<EmbeddingResponse>> {
        use crate::routing::middleware::BackendHandle;
        let this = self.clone();
        let req = request.clone();
        // Enforce text-input modality so the mesh fallback does not forward
        // `/v1/embeddings` traffic to a remote service that declares only
        // image inputs (CLIP-vision style). Mirrors the `Text` constraint
        // in `executor.execute_embeddings`.
        let required_input = Some(crate::services::catalog::InputModality::Text);
        self.dispatch_with_fallback(&request.model, 0, required_input, |handle| {
            let this = this.clone();
            let req = req.clone();
            let handle = handle.clone();
            async move {
                let BackendHandle::MeshForward(node_id, svc) = &handle else {
                    return Err(anyhow::anyhow!(
                        "embeddings legacy fallback: only MeshForward is handled here \
                         (executor owns Local/HTTP/QUIC)"
                    ));
                };
                debug!(
                    target_node = %node_id,
                    service = %svc,
                    "MeshForward embeddings do zdalnej uslugi"
                );
                let input_texts = match &req.input {
                    EmbeddingInput::Single(text) => vec![text.clone()],
                    EmbeddingInput::Multiple(texts) => texts.clone(),
                };
                let text_count = input_texts.len();
                let request = ModelRequest {
                    request_id: uuid::Uuid::new_v4().to_string(),
                    payload: ModelPayload::Embeddings(EmbeddingsPayload {
                        model: svc.clone(),
                        input: input_texts,
                        normalize: true,
                    }),
                    stream: false,
                    metadata: None,
                    session_id: None,
                };
                let response = this
                    .forward_model_request_to_mesh(node_id, request)
                    .await
                    .map_err(|e| anyhow::anyhow!("Mesh embeddings request failed: {}", e))?;
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
                        let estimated = (text_count * 50) as u32;
                        Ok(EmbeddingResponse {
                            object: "list".to_string(),
                            data,
                            model: svc.clone(),
                            usage: EmbeddingUsage {
                                prompt_tokens: estimated,
                                total_tokens: estimated,
                            },
                        })
                    }
                    ModelResult::Error(err) => Err(anyhow::anyhow!(
                        "Mesh embeddings error {:?}: {}",
                        err.error_type,
                        err.message
                    )),
                    _ => Err(anyhow::anyhow!(
                        "Mesh embeddings returned unexpected response type"
                    )),
                }
            }
        })
        .await
    }

    /// Routuje embeddings request przez QUIC - wersja dla protocol types.
    /// Protocol-native embeddings API used by `mesh/inference_proxy.rs`
    /// when a peer sends `EmbeddingsPayload` over the reverse stream.
    /// Delegates through the same executor as `/v1/embeddings` so the
    /// resolver / modality filter applies; a **mesh-forward guard**
    /// (`hop_count = MAX_HOP_COUNT`) blocks re-forwarding to a third
    /// node. `TransportPendingCutover` from the executor is mapped to
    /// `AllBackendsUnavailable` so the peer can retry through another
    /// route instead of bouncing.
    pub async fn route_embeddings_via_quic(
        &self,
        model: &str,
        texts: Vec<String>,
    ) -> Result<tentaflow_protocol::ModelResponse> {
        use crate::api::openai::types::EmbeddingInput;
        use crate::services::runtime::context::ExecutionContext;

        debug!("route_embeddings_via_quic: START model={}", model);

        if texts.is_empty() {
            return Err(CoreError::InvalidRequest {
                message: "embeddings request has zero inputs".to_string(),
                details: Some("at least one text is required".to_string()),
            }
            .into());
        }

        let executor = self
            .executor
            .read()
            .clone()
            .ok_or_else(|| CoreError::AllBackendsUnavailable {
                model_name: model.to_string(),
            })?;

        let request = EmbeddingRequest {
            model: model.to_string(),
            input: if texts.len() == 1 {
                EmbeddingInput::Single(texts[0].clone())
            } else {
                EmbeddingInput::Multiple(texts.clone())
            },
            encoding_format: None,
            dimensions: None,
            user: None,
        };

        // Mesh re-forward guard: max out the hop counter so any further
        // `enter_hop` call inside the executor's mesh path will reject.
        // Anti-loop on the protocol-native reverse path — a peer's
        // EmbeddingsPayload must land on a local instance, never bounce.
        let mut exec_ctx = ExecutionContext {
            hop_count: crate::services::runtime::context::MAX_HOP_COUNT,
            ..ExecutionContext::default()
        };

        let response = match executor.execute_embeddings(request, &mut exec_ctx).await {
            Ok(r) => r,
            Err(e) => return Err(executor_err_to_core(e, model).into()),
        };

        // Convert `EmbeddingResponse` → protocol-native `ModelResponse`
        // (the reverse handler expects the rkyv-encoded protocol shape).
        let request_id = uuid::Uuid::new_v4().to_string();
        let embeddings: Vec<Vec<f32>> =
            response.data.into_iter().map(|d| d.embedding).collect();
        let dimensions = embeddings.first().map(|v| v.len()).unwrap_or(0);
        let proto_response = ModelResponse {
            request_id,
            result: ModelResult::Embeddings(EmbeddingsResult {
                embeddings,
                dimensions,
                model: response.model,
            }),
            metrics: None,
        };

        Ok(proto_response)
    }
}

/// Map executor errors onto typed `CoreError` variants so the OpenAI HTTP
/// layer can serve a precise status code (404 / 400 / 503) instead of a
/// catch-all 500.
fn executor_err_to_core(
    err: crate::services::runtime::executor::ExecutorError,
    model: &str,
) -> CoreError {
    use crate::services::runtime::executor::ExecutorError;
    use crate::services::runtime::resolver::ResolveError;
    match err {
        ExecutorError::Resolve(ResolveError::UnknownModel(m)) => CoreError::ModelNotFound {
            model_name: m,
        },
        ExecutorError::Resolve(ResolveError::CapabilityUnsupported { requested, .. }) => {
            CoreError::InvalidRequest {
                message: format!(
                    "model '{}' has no candidate matching requested capabilities",
                    requested
                ),
                details: None,
            }
        }
        ExecutorError::Resolve(other) => CoreError::InternalError {
            message: format!("alias resolution: {}", other),
            source: None,
        },
        ExecutorError::AllCandidatesFailed { .. } => CoreError::AllBackendsUnavailable {
            model_name: model.to_string(),
        },
        ExecutorError::FlowDispatcherUnavailable
        | ExecutorError::FlowEmptyResult { .. }
        | ExecutorError::Internal(_)
        | ExecutorError::SttRuntimeUnavailable
        | ExecutorError::SttBackend(_) => CoreError::InternalError {
            message: format!("executor: {}", err),
            source: None,
        },
        ExecutorError::TransportPendingCutover(_) => CoreError::AllBackendsUnavailable {
            model_name: model.to_string(),
        },
    }
}
