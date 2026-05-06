// =============================================================================
// Plik: flow_engine/dispatchers/embeddings.rs
// Opis: EmbeddingsDispatcher — wrapper nad executor.rs::execute_embeddings.
//       Adapter dostaje listę tekstów, zwraca listę wektorów (cardinality 1:1
//       z input).
// =============================================================================

use anyhow::Result;
use async_trait::async_trait;

use crate::flow_engine::envelope::TokenUsage;

#[derive(Debug, Clone)]
pub struct EmbeddingsRequest {
    pub model: String,
    pub inputs: Vec<String>,
    pub user_id: Option<i64>,
    pub user_role: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EmbeddingsResponse {
    pub vectors: Vec<Vec<f32>>,
    pub usage: TokenUsage,
}

#[async_trait]
pub trait EmbeddingsDispatcher: Send + Sync {
    async fn embed(&self, req: EmbeddingsRequest) -> Result<EmbeddingsResponse>;
}
