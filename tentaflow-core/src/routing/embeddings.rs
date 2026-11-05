// =============================================================================
// Plik: routing/embeddings.rs
// Opis: Obsluga zapytan o embeddingi przez FlowDispatcher. Jawny flow gdy
//       skonfigurowany; gdy brak — model embeddingów wykonywany bezpośrednio na
//       executorze (EmbeddingsDispatcherImpl → executor.execute_embeddings).
//       `route_embeddings_via_quic` to mesh inbound EXEMPT — direct executor
//       żeby zachować ultra-low latency LAN budżet.
// =============================================================================

use crate::api::openai::types::{EmbeddingRequest, EmbeddingResponse};
use crate::error::{CoreError, Result};
use crate::routing::router::Router;

use tentaflow_protocol::*;
use tracing::debug;

impl Router {
    /// Routuje embeddings request przez flow_engine.
    ///
    /// Wariant z user context — sprawdza ACL ('model', request.model) zanim
    /// uderzymy w backend. ACL deny mapuje na ModelNotFound (404) — nie
    /// ujawniamy istnienia modelu klientom bez dostępu (parytet z 4
    /// pozostałymi default HTTP paths).
    pub async fn route_embeddings_for_user(
        &self,
        request: EmbeddingRequest,
        user: Option<crate::auth::acl::UserContext>,
        origin: crate::flow_engine::dispatcher::FlowOrigin,
        actor: crate::flow_engine::dispatcher::FlowActor,
    ) -> Result<crate::routing::RouteResult<EmbeddingResponse>> {
        if let Some(ref u) = user {
            if let Some(ref db) = self.db {
                if !crate::auth::acl::check_access_safe(
                    db,
                    "model",
                    &request.model,
                    &u.user_id,
                    &u.role,
                ) {
                    tracing::warn!(user_id = %u.user_id, model = %request.model, "ACL denied embedding model");
                    return Err(crate::error::CoreError::ModelNotFound {
                        model_name: request.model.clone(),
                    }
                    .into());
                }
            }
        }
        // Usage is booked by the embeddings dispatcher through the AI gateway,
        // once per backend call; booking it here as well would count it twice.
        self.route_embeddings_inner(request, user, origin, actor)
            .await
    }

    /// Obsluguje zarowno Single jak i Multiple input przez flow_engine.
    /// Multi-input batch propagowany przez `envelope.meta["embeddings_inputs"]`
    /// (JSON array); EmbeddingsNodeAdapter rozpakowuje do EmbeddingsRequest.
    /// inputs. `user` propagowany do `FlowRequestMeta` żeby ACL gate flow
    /// (jeśli admin skonfigurował user-defined flow) widział user_id/role.
    async fn route_embeddings_inner(
        &self,
        request: EmbeddingRequest,
        user: Option<crate::auth::acl::UserContext>,
        origin: crate::flow_engine::dispatcher::FlowOrigin,
        actor: crate::flow_engine::dispatcher::FlowActor,
    ) -> Result<crate::routing::RouteResult<EmbeddingResponse>> {
        debug!("Routing embeddings dla modelu: {}", request.model);

        let t = std::time::Instant::now();

        // Embeddings path zawsze przez FlowDispatcher: jawny flow gdy
        // skonfigurowany, inaczej model embeddingów wykonywany bezpośrednio na
        // executorze (NotFound → direct execution wewnątrz dispatchera).
        if let Some(ref dispatcher) = self.flow_dispatcher {
            let (initial, meta) =
                crate::services::runtime::executor::embeddings_request_to_initial_envelope(
                    &request,
                    user.clone(),
                    origin,
                    actor.clone(),
                );
            match dispatcher
                .try_dispatch(&request.model, "embeddings", initial, meta)
                .await
            {
                Ok(outcome) => {
                    let expected_count = match &request.input {
                        crate::api::openai::types::EmbeddingInput::Single(_) => 1,
                        crate::api::openai::types::EmbeddingInput::Multiple(texts) => texts.len(),
                    };
                    let response =
                        crate::services::runtime::executor::flow_outcome_to_embedding_response(
                            outcome,
                            &request,
                            expected_count,
                        )
                        .map_err(|e| {
                            crate::error::CoreError::InternalError {
                                message: format!("embeddings flow result: {e}"),
                                source: None,
                            }
                        })?;
                    let metadata = crate::routing::RouteMetadata {
                        served_by_node: crate::mesh::node_info_collector::local_hostname(),
                        backend_type: "flow_engine".to_string(),
                        strategy_used: "flow_dispatch".to_string(),
                        fallbacks_tried: 0,
                        hop_count: 0,
                        latency_ms: Some(t.elapsed().as_secs_f64() * 1000.0),
                        usage: None,
                        finish_reason: None,
                    };
                    return Ok(crate::routing::RouteResult { response, metadata });
                }
                Err(e) => {
                    return Err(crate::routing::dispatch_error_to_core(e, &request.model).into());
                }
            }
        }

        // Stage 3d-0b-final: brak flow_dispatcher (DB-less router) → 500.
        // Direct executor.execute_embeddings fallback wycięty.
        let _ = t;
        Err(crate::error::CoreError::InternalError {
            message: format!(
                "flow_dispatcher not wired for embeddings model '{}' — DB-less router \
                 nie wspiera Universal Flow Gateway",
                request.model
            ),
            source: None,
        }
        .into())
    }

    /// Protocol-native embeddings API uzywane przez `mesh/inference_proxy.rs`
    /// gdy peer wysyla `EmbeddingsPayload` przez reverse stream. Deleguje
    /// przez ten sam executor co `/v1/embeddings`, z mesh-forward guardem
    /// (`hop_count = MAX_HOP_COUNT`) zeby peer nie mogl wybic re-forward
    /// loop'u.
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

        let executor =
            self.executor
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
            extra: serde_json::Map::new(),
        };

        // Mesh re-forward guard: max out the hop counter so any further
        // `enter_hop` call inside the executor's mesh path will reject.
        // Anti-loop on the protocol-native reverse path — a peer's
        // EmbeddingsPayload must land on a local instance, never bounce.
        // §2.5 — a peer node forwarded this call; the originating user stays on
        // the initiator's node, so the acting identity here is the mesh peer.
        let mut exec_ctx = ExecutionContext::new(
            None,
            crate::flow_engine::dispatcher::FlowOrigin::Mesh,
            crate::flow_engine::dispatcher::FlowActor::system_component("mesh_peer"),
        );
        exec_ctx.hop_count = crate::services::runtime::context::MAX_HOP_COUNT;

        // EXEMPT-MESH-INBOUND (stage 3d v1.5): protocol-native embeddings
        // mesh reverse path — peer forwarduje CBOR ModelRequest, my
        // wykonujemy direct executor żeby zachować ultra-low latency
        // budget (LAN 1-5ms). Plan v1.5 dokumentuje to jako jedyny
        // dozwolony wyjątek od "wszystko przez flow_engine".
        let response = match executor.execute_embeddings(request, &mut exec_ctx).await {
            Ok(r) => r,
            Err(e) => return Err(executor_err_to_core(e, model).into()),
        };

        // Convert `EmbeddingResponse` → protocol-native `ModelResponse`
        // (the reverse handler expects the CBOR-encoded protocol shape).
        let request_id = uuid::Uuid::new_v4().to_string();
        let embeddings: Vec<Vec<f32>> = response.data.into_iter().map(|d| d.embedding).collect();
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

    /// Protocol-native rerank API uzywane przez `mesh/inference_proxy.rs` gdy
    /// peer wysyla `RerankPayload` przez reverse stream. Lustro
    /// `route_embeddings_via_quic`: ten sam executor co `/v1/rerank`, mesh-forward
    /// guard (`hop_count = MAX_HOP_COUNT`) przeciw re-forward loopowi. Bez tego
    /// rerank na zdalnym wezle pada (odbiorca nie obslugiwal Discriminant Rerank)
    /// i retrieval degradowal do czystego vector order.
    pub async fn route_rerank_via_quic(&self, payload: &RerankPayload) -> Result<ModelResponse> {
        use crate::api::openai::types::RerankRequest;
        use crate::services::runtime::context::ExecutionContext;

        let executor =
            self.executor
                .read()
                .clone()
                .ok_or_else(|| CoreError::AllBackendsUnavailable {
                    model_name: payload.model.clone(),
                })?;

        let request = RerankRequest {
            model: payload.model.clone(),
            query: payload.query.clone(),
            documents: payload.documents.clone(),
            top_n: payload.top_n.map(|n| n as u32),
        };

        // §2.5 — a peer node forwarded this call; the originating user stays on
        // the initiator's node, so the acting identity here is the mesh peer.
        let mut exec_ctx = ExecutionContext::new(
            None,
            crate::flow_engine::dispatcher::FlowOrigin::Mesh,
            crate::flow_engine::dispatcher::FlowActor::system_component("mesh_peer"),
        );
        exec_ctx.hop_count = crate::services::runtime::context::MAX_HOP_COUNT;

        let response = match executor.execute_rerank(request, &mut exec_ctx).await {
            Ok(r) => r,
            Err(e) => return Err(executor_err_to_core(e, &payload.model).into()),
        };

        let results = response
            .results
            .into_iter()
            .map(|e| RerankResultItem {
                index: e.index,
                relevance_score: e.relevance_score,
                document: None,
            })
            .collect();

        Ok(ModelResponse {
            request_id: uuid::Uuid::new_v4().to_string(),
            result: ModelResult::Rerank(RerankResult {
                results,
                model: payload.model.clone(),
            }),
            metrics: None,
        })
    }
}

/// Map executor errors onto typed `CoreError` variants so the OpenAI HTTP
/// layer can serve a precise status code (404 / 400 / 503) instead of a
/// catch-all 500.
pub(crate) fn executor_err_to_core(
    err: crate::services::runtime::executor::ExecutorError,
    model: &str,
) -> CoreError {
    use crate::services::runtime::executor::ExecutorError;
    use crate::services::runtime::resolver::ResolveError;
    match err {
        ExecutorError::Resolve(ResolveError::UnknownModel(m)) => {
            CoreError::ModelNotFound { model_name: m }
        }
        ExecutorError::Resolve(ResolveError::CapabilityUnsupported { requested, .. }) => {
            CoreError::InvalidRequest {
                message: format!(
                    "model '{}' has no candidate matching requested capabilities",
                    requested
                ),
                details: None,
            }
        }
        ExecutorError::Resolve(ResolveError::NoLiveInstance(m)) => {
            CoreError::AllBackendsUnavailable { model_name: m }
        }
        ExecutorError::Resolve(ResolveError::CrossEnvironmentDenied(m)) => {
            CoreError::AllBackendsUnavailable { model_name: m }
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
        ExecutorError::SttServiceUnavailable => CoreError::SttServiceUnavailable,
        ExecutorError::TransportPendingCutover(_) => CoreError::AllBackendsUnavailable {
            model_name: model.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::api::openai::types::EmbeddingInput;
    use crate::flow_engine::dispatcher::{FlowActor, FlowOrigin};

    fn request(model: &str) -> EmbeddingRequest {
        EmbeddingRequest {
            model: model.to_string(),
            input: EmbeddingInput::Single("hello".to_string()),
            encoding_format: None,
            dimensions: None,
            user: None,
            extra: serde_json::Map::new(),
        }
    }

    fn router() -> (tempfile::TempDir, crate::db::DbPool, Router) {
        let directory = tempfile::tempdir().expect("test directory");
        let db = crate::db::init(&directory.path().join("embeddings.db")).expect("migrate");
        let router =
            Router::new(crate::config::RouterConfig::default(), Some(db.clone())).expect("router");
        (directory, db, router)
    }

    /// A spent quota inside the embeddings flow reaches the edge as the typed
    /// 429, not as a generic internal error wrapped around its text. A caller
    /// without a user is accounted to the default organization.
    #[tokio::test]
    async fn a_spent_quota_reaches_the_edge_as_rate_limit_exceeded() {
        let (_directory, db, router) = router();
        crate::db::repository::create_token_quota(
            &db,
            &crate::db::models::NewTokenQuota {
                org_id: crate::services::org::DEFAULT_ORG_ID,
                scope_type: "org",
                subject_id: None,
                model_id: None,
                period: "daily",
                max_total_tokens: 0,
                is_active: true,
            },
        )
        .unwrap();
        let error = router
            .route_embeddings_for_user(
                request("quota-embed"),
                None,
                FlowOrigin::Dashboard,
                FlowActor::system(),
            )
            .await
            .err()
            .expect("refused");
        match error.downcast_ref::<CoreError>() {
            Some(CoreError::RateLimitExceeded { message }) => {
                assert!(message.contains("token quota exceeded"), "{message}")
            }
            other => panic!("expected RateLimitExceeded, got {other:?} / {error:#}"),
        }
    }

    /// The dispatchers built inside `Router::new` stamp the mesh node id that is
    /// registered AFTER they were built, and a call that failed books no usage.
    #[tokio::test]
    async fn the_real_wiring_stamps_the_registered_mesh_node_id() {
        let (_directory, db, router) = router();
        let registry = Arc::new(crate::services::mesh_registry::MeshServicesRegistry::new());
        registry.replace_local("mesh-wired-node".to_string(), Vec::new());
        router
            .service_manager()
            .set_mesh_services_registry(registry);

        router
            .route_embeddings_for_user(
                request("wired-embed"),
                None,
                FlowOrigin::Dashboard,
                FlowActor::system(),
            )
            .await
            .err()
            .expect("no backend serves this model");

        let node_ids: Vec<(String, String)> = {
            let conn = db.read().unwrap();
            let mut statement = conn
                .prepare("SELECT node_id, backend FROM compliance_ai_events")
                .unwrap();
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap()
        };
        assert_eq!(
            node_ids,
            vec![("mesh-wired-node".to_string(), "embeddings".to_string())]
        );
        assert!(crate::services::runtime::token_usage_cache::global()
            .pending_deltas_snapshot()
            .iter()
            .all(|(key, _)| key.node_id != "mesh-wired-node"));
    }

    /// One successful call through the whole stack is booked exactly once, as
    /// embedding tokens: no chat tokens (which quotas count toward
    /// `total_tokens`) and no second booking from the routing edge.
    #[tokio::test]
    async fn a_served_call_books_embedding_tokens_exactly_once() {
        use crate::project_studio::ingest::access_test_support::{
            advertise_model, DelayedModel, ModelReply,
        };
        const MODEL: &str = "booking-once-embed";
        let state = crate::dispatch::state::AppState::for_test();
        let mut backend = DelayedModel::new(MODEL, ModelReply::Embedding);
        advertise_model(&state, "embeddings", MODEL, &backend.endpoint);

        let router = state.router.clone();
        let call = tokio::spawn(async move {
            router
                .route_embeddings_for_user(
                    request(MODEL),
                    None,
                    FlowOrigin::Dashboard,
                    FlowActor::system(),
                )
                .await
        });
        backend.wait_request().await;
        backend.release();
        call.await.expect("call task").expect("embeddings served");
        backend.finish();

        let rows: Vec<_> = crate::services::runtime::token_usage_cache::global()
            .pending_deltas_snapshot()
            .into_iter()
            .filter(|(key, _)| key.model_id == MODEL)
            .collect();
        assert_eq!(rows.len(), 1, "{rows:?}");
        let totals = &rows[0].1;
        assert_eq!(
            (
                totals.embedding_tokens,
                totals.prompt_tokens,
                totals.completion_tokens,
                totals.request_count
            ),
            (1, 0, 0, 1),
            "{totals:?}"
        );
    }
}
