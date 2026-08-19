// =============================================================================
// Plik: flow_engine/dispatchers_impl/rerank_impl.rs
// Opis: RerankDispatcherImpl — wrapper nad
//       `services::runtime::executor::ModelRuntimeExecutor::execute_rerank`.
//       Adapter widzi tylko narrow trait; failover aliasu `rag-reranker` jest
//       TEN SAM co embeddings/chat (A1). Świeży runtime `ExecutionContext`
//       per call, ZASIANY głębokością flow z węzła (`req.flow_depth`), żeby
//       self-referencyjny rerank-flow trafił w guard rekurencji zamiast
//       resetować głębokość do 0 (RAG C2).
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use super::{build_user_context, ModelRuntimeSlot};
use crate::api::openai::types::RerankRequest as ApiRerankRequest;
use crate::flow_engine::dispatchers::{
    RerankDispatcher, RerankRequest, RerankResponse, RerankResult,
};
use crate::flow_engine::envelope::TokenUsage;
use crate::services::runtime::context::ExecutionContext as RuntimeContext;

pub struct RerankDispatcherImpl {
    runtime: ModelRuntimeSlot,
}

impl RerankDispatcherImpl {
    pub fn new(runtime: ModelRuntimeSlot) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl RerankDispatcher for RerankDispatcherImpl {
    async fn rerank(&self, req: RerankRequest) -> Result<RerankResponse> {
        if req.documents.is_empty() {
            return Err(anyhow!("RerankDispatcher: empty documents"));
        }

        let doc_count = req.documents.len();
        let flow_depth = req.flow_depth;
        let provenance = req.provenance.clone();
        let user = build_user_context(req.user_id, req.user_role.as_deref());
        let api_req = ApiRerankRequest {
            model: req.model,
            query: req.query,
            documents: req.documents,
            top_n: req.top_n,
        };

        // §2.5 — the calling node's stamp travels with the request; a fresh
        // runtime context here would report the inner dispatch as `system`.
        let mut rctx = RuntimeContext::new_with_flow_depth(
            user,
            flow_depth,
            provenance.origin,
            provenance.actor,
        );
        let runtime = self
            .runtime
            .read()
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow!("RerankDispatcher: ModelRuntimeExecutor not wired"))?;
        let response = runtime
            .execute_rerank(api_req, &mut rctx)
            .await
            .map_err(|e| anyhow!("RerankDispatcher: {e}"))?;

        // Guard: backend nie może zwrócić indeksu spoza zakresu dokumentów —
        // inaczej adapter zmapowałby score na zły kandydat.
        let mut results: Vec<RerankResult> = Vec::with_capacity(response.results.len());
        for entry in response.results {
            if entry.index >= doc_count {
                return Err(anyhow!(
                    "RerankDispatcher: backend returned out-of-range index {} for {} document(s)",
                    entry.index,
                    doc_count
                ));
            }
            results.push(RerankResult {
                index: entry.index,
                score: entry.relevance_score,
            });
        }
        // Dispatcher gwarantuje kolejność malejącą po score — backend bywa
        // out-of-order, a adapter polega na tym kontrakcie.
        results.sort_by(|a, b| b.score.total_cmp(&a.score));

        Ok(RerankResponse {
            results,
            usage: TokenUsage::default(),
        })
    }
}
