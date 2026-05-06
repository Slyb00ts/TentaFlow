// =============================================================================
// Plik: flow_engine/dispatchers_impl/embeddings_impl.rs
// Opis: EmbeddingsDispatcherImpl — wrapper nad
//       `services::runtime::executor::ModelRuntimeExecutor::execute_embeddings`.
//       Adapter widzi tylko narrow trait. Mutable runtime `ExecutionContext`
//       (resolver/strategy/route_metadata) tworzymy świeży per call —
//       flow-engine ma własny `ExecutionContext` w `node_adapter.rs`,
//       runtime-level state nie wycieka między requestami.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use std::sync::Arc;

use crate::api::openai::types::{EmbeddingInput, EmbeddingRequest};
use crate::flow_engine::dispatchers::{EmbeddingsDispatcher, EmbeddingsRequest, EmbeddingsResponse};
use crate::flow_engine::envelope::TokenUsage;
use crate::services::runtime::context::ExecutionContext as RuntimeContext;
use crate::services::runtime::executor::ModelRuntimeExecutor;

pub struct EmbeddingsDispatcherImpl {
    runtime: Arc<ModelRuntimeExecutor>,
}

impl EmbeddingsDispatcherImpl {
    pub fn new(runtime: Arc<ModelRuntimeExecutor>) -> Self {
        Self { runtime }
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

        let api_req = EmbeddingRequest {
            model: req.model,
            input,
            encoding_format: None,
            dimensions: None,
            user: None,
        };

        let mut rctx = RuntimeContext::new(None);
        let response = self
            .runtime
            .execute_embeddings(api_req, &mut rctx)
            .await
            .map_err(|e| anyhow!("EmbeddingsDispatcher: {e}"))?;

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
