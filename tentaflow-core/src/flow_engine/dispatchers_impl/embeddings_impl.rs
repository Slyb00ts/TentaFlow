// =============================================================================
// Plik: flow_engine/dispatchers_impl/embeddings_impl.rs
// Opis: EmbeddingsDispatcherImpl — wrapper nad
//       `services::runtime::executor::ModelRuntimeExecutor::execute_embeddings`.
//       Adapter widzi tylko narrow trait. Mutable runtime `ExecutionContext`
//       (resolver/strategy/route_metadata) tworzymy świeży per call,
//       ZASIANY głębokością flow z węzła (`req.flow_depth`), żeby
//       self-referencyjny embeddings-flow trafił w guard rekurencji zamiast
//       resetować głębokość do 0 (RAG C2). Flow-engine ma własny
//       `ExecutionContext` w `node_adapter.rs`; runtime-level state nie
//       wycieka między requestami.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use super::{build_user_context, ModelRuntimeSlot};
use crate::api::openai::types::{EmbeddingInput, EmbeddingRequest};
use crate::flow_engine::dispatchers::{
    EmbeddingsDispatcher, EmbeddingsRequest, EmbeddingsResponse,
};
use crate::auth::acl::UserContext;
use crate::compliance::ai_gateway::{AiGateway, AiGatewayContext};
use crate::db::DbPool;
use crate::flow_engine::dispatcher::FlowOrigin;
use crate::flow_engine::envelope::TokenUsage;
use crate::services::runtime::context::ExecutionContext as RuntimeContext;

pub struct EmbeddingsDispatcherImpl {
    runtime: ModelRuntimeSlot,
    /// Backs the AI gateway for BPMN process calls (quota and audit). `None` in
    /// tests and DB-less bootstraps, where there is nothing to record.
    db: Option<DbPool>,
    node_id: String,
}

impl EmbeddingsDispatcherImpl {
    pub fn new(runtime: ModelRuntimeSlot, db: Option<DbPool>, node_id: impl Into<String>) -> Self {
        Self {
            runtime,
            db,
            node_id: node_id.into(),
        }
    }

    /// Opens the audit event of a process step's embeddings call, applying the
    /// token quota first. Any other origin is not audited here (as before).
    fn start_process_event(
        &self,
        req: &EmbeddingsRequest,
    ) -> Result<Option<crate::compliance::ai_gateway::AiEventHandle>> {
        let Some(db) = self.db.as_ref() else {
            return Ok(None);
        };
        if req.provenance.origin != FlowOrigin::Process {
            return Ok(None);
        }
        let gateway = AiGateway::new(
            db.clone(),
            self.node_id.clone(),
            crate::compliance::ai_gateway::token_quota_enabled(),
        )
        .with_strict_quota_reads();
        let user = req.user_id.as_ref().map(|uid| UserContext {
            user_id: uid.clone(),
            role: req.user_role.clone().unwrap_or_else(|| "user".to_string()),
        });
        let context = AiGatewayContext {
            org_id: req.provenance.org_id.clone(),
            ..Default::default()
        };
        gateway
            .start_embeddings_event(&req.model, &req.inputs, user.as_ref(), Some(&context))
            .map(Some)
    }
}

#[async_trait]
impl EmbeddingsDispatcher for EmbeddingsDispatcherImpl {
    async fn embed(&self, req: EmbeddingsRequest) -> Result<EmbeddingsResponse> {
        if req.inputs.is_empty() {
            return Err(anyhow!("EmbeddingsDispatcher: empty inputs"));
        }

        let input = if req.inputs.len() == 1 {
            EmbeddingInput::Single(req.inputs[0].clone())
        } else {
            EmbeddingInput::Multiple(req.inputs.clone())
        };

        let audit_event = self.start_process_event(&req)?;
        let flow_depth = req.flow_depth;
        let provenance = req.provenance.clone();
        let user = build_user_context(req.user_id, req.user_role.as_deref());
        let api_req = EmbeddingRequest {
            model: req.model,
            input,
            encoding_format: req.encoding_format.clone(),
            dimensions: req.dimensions,
            user: req.user,
            extra: req.extra,
        };

        // §2.5 — the calling node's stamp travels with the request; a fresh
        // runtime context here would report the inner dispatch as `system`.
        let mut rctx = RuntimeContext::new_with_flow_depth(
            user,
            flow_depth,
            provenance.origin,
            provenance.actor,
        );
        let runtime = match self.runtime.read().as_ref().cloned() {
            Some(runtime) => runtime,
            None => {
                let message = "EmbeddingsDispatcher: ModelRuntimeExecutor not wired";
                if let Some(event) = audit_event.as_ref() {
                    let _ = event.finish_failed(message);
                }
                return Err(anyhow!(message));
            }
        };
        let response = match runtime.execute_embeddings(api_req, &mut rctx).await {
            Ok(response) => response,
            Err(e) => {
                let message = format!("EmbeddingsDispatcher: {e}");
                if let Some(event) = audit_event.as_ref() {
                    let _ = event.finish_failed(&message);
                }
                return Err(anyhow!(message));
            }
        };
        if let Some(event) = audit_event.as_ref() {
            let usage = crate::api::openai::types::Usage {
                prompt_tokens: response.usage.prompt_tokens,
                completion_tokens: 0,
                total_tokens: response.usage.total_tokens,
            };
            let summary = format!("{} embedding vectors", response.data.len());
            let _ = event.finish_stream_success(&summary, Some(&usage), &[]);
        }

        // Cardinality 1:1 z input — sortujemy po `index` żeby kolejność była
        // deterministyczna nawet jeśli backend zwrócił out-of-order.
        let mut data = response.data;
        data.sort_by_key(|d| d.index);
        let vectors = data.into_iter().map(|d| d.embedding).collect();

        Ok(EmbeddingsResponse {
            vectors,
            usage: TokenUsage {
                prompt_tokens: response.usage.prompt_tokens as u64,
                completion_tokens: 0,
                total_tokens: response.usage.total_tokens as u64,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::flow_engine::dispatcher::{CallProvenance, FlowActor};

    fn db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        crate::db::migrations::run(&conn).expect("migrations");
        Arc::new(crate::db::Db::from_connection(conn))
    }

    fn request(provenance: CallProvenance) -> EmbeddingsRequest {
        EmbeddingsRequest {
            model: "embed-model".into(),
            inputs: vec!["first".into(), "second".into()],
            dimensions: None,
            encoding_format: None,
            user_id: Some("u-proc".into()),
            user_role: Some("user".into()),
            user: None,
            extra: Default::default(),
            flow_depth: 0,
            provenance,
        }
    }

    fn events(db: &DbPool) -> Vec<(String, String, String)> {
        let conn = db.read().unwrap();
        let mut statement = conn
            .prepare("SELECT org_id, backend, status FROM compliance_ai_events")
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn setup() -> (DbPool, String, EmbeddingsDispatcherImpl) {
        let db = db();
        db.write()
            .unwrap()
            .execute(
                "INSERT INTO user_accounts (id, username, password_hash) VALUES ('u-proc', 'proc-user', 'x')",
                [],
            )
            .unwrap();
        let org = crate::services::org::repo::create_organization(
            &db, "Process org", "process-org", None, None, None, None,
        )
        .unwrap()
        .org_id;
        let runtime: ModelRuntimeSlot = Arc::new(parking_lot::RwLock::new(None));
        let dispatcher = EmbeddingsDispatcherImpl::new(runtime, Some(db.clone()), "node-test");
        (db, org, dispatcher)
    }

    fn process(org: &str) -> CallProvenance {
        CallProvenance::new(FlowOrigin::Process, FlowActor::user("u-proc"))
            .with_org(Some(org.to_owned()))
    }

    #[tokio::test]
    async fn a_process_embeddings_call_is_audited_under_the_process_organization() {
        let (db, org, dispatcher) = setup();
        // The runtime is not wired, so the call fails after its event was opened:
        // the audit row is there, and it says the call failed.
        let error = dispatcher.embed(request(process(&org))).await.unwrap_err();
        assert!(error.to_string().contains("not wired"));
        assert_eq!(events(&db), vec![(org, "embeddings".to_owned(), "failed".to_owned())]);
    }

    #[tokio::test]
    async fn other_origins_are_not_audited_here_as_before() {
        let (db, _org, dispatcher) = setup();
        let ordinary = CallProvenance::new(FlowOrigin::Dashboard, FlowActor::user("u-proc"));
        dispatcher.embed(request(ordinary)).await.unwrap_err();
        assert!(events(&db).is_empty());
    }

    #[tokio::test]
    async fn a_spent_quota_refuses_a_process_embeddings_call_before_the_backend() {
        let (db, org, dispatcher) = setup();
        crate::db::repository::create_token_quota(
            &db,
            &crate::db::models::NewTokenQuota {
                org_id: &org,
                scope_type: "org",
                subject_id: None,
                model_id: None,
                period: "daily",
                max_total_tokens: 0,
                is_active: true,
            },
        )
        .unwrap();
        let error = dispatcher.embed(request(process(&org))).await.unwrap_err();
        assert!(format!("{error:#}").contains("token quota exceeded"), "{error:#}");
        assert!(events(&db).is_empty(), "a refused call opens no event");
    }
}
