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
    /// Etap 2: opcjonalna dimension hint dla embedding-3* modeli.
    pub dimensions: Option<u32>,
    /// Etap 2: "float" lub "base64". Backend embedded ignoruje.
    pub encoding_format: Option<String>,
    pub user_id: Option<String>,
    pub user_role: Option<String>,
    /// OpenAI `user` field of the external request, forwarded verbatim to an
    /// HTTP backend. Unrelated to `user_id` (the authenticated principal).
    pub user: Option<String>,
    /// Unknown vendor fields of the external request, forwarded verbatim to an
    /// HTTP backend (`truncate`, `input_type`, ...).
    pub extra: serde_json::Map<String, serde_json::Value>,
    /// RAG C2 (recursion guard) — głębokość zagnieżdżenia flow, z której
    /// pochodzi to wywołanie (`ExecutionContext.subflow_depth` węzła). Dispatcher
    /// seeduje nim runtime'owy `flow_stack`, żeby re-wejście embeddings w
    /// flow-surface DZIEDZICZYŁO głębokość zamiast resetować do 0.
    pub flow_depth: u8,
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
