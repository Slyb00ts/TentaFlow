// =============================================================================
// Plik: flow_engine/dispatchers/rerank.rs
// Opis: RerankDispatcher — narrow trait dla węzła flow `reranker`. Wrapper nad
//       `ModelRuntimeExecutor::execute_rerank` (cross-encoder, /v1/rerank),
//       z tym samym failoverem aliasów (A1) co embeddings/chat. Adapter widzi
//       tylko query + dokumenty, dostaje wyniki (index → score).
// =============================================================================

use anyhow::Result;
use async_trait::async_trait;

use crate::flow_engine::envelope::TokenUsage;
use crate::flow_engine::dispatcher::CallProvenance;

#[derive(Debug, Clone)]
pub struct RerankRequest {
    pub model: String,
    pub query: String,
    pub documents: Vec<String>,
    /// Ile najlepszych zwrócić (None = wszystkie). Cross-encoder to wąskie
    /// gardło — adapter capuje to po stronie hosta przed dispatchem.
    pub top_n: Option<u32>,
    pub user_id: Option<String>,
    pub user_role: Option<String>,
    /// RAG C2 (recursion guard) — głębokość zagnieżdżenia flow, z której
    /// pochodzi to wywołanie (`ExecutionContext.subflow_depth` węzła `reranker`).
    /// Dispatcher seeduje nim runtime'owy `flow_stack`, żeby re-wejście
    /// rerankera w flow-surface DZIEDZICZYŁO głębokość zamiast resetować do 0.
    pub flow_depth: u8,
    /// §2.5 — server-minted provenance of the flow this call belongs to, copied
    /// from the node's `ExecutionContext`. Threaded for the same reason as
    /// `flow_depth`: the runtime context the dispatcher builds re-enters the
    /// executor, and an alias resolving onto a flow surface starts THAT flow
    /// with this stamp. Not derived from request content.
    pub provenance: CallProvenance,
}

/// Pojedynczy wynik rerankingu — odwzorowanie pozycji dokumentu na score.
#[derive(Debug, Clone)]
pub struct RerankResult {
    /// Index w oryginalnej liście `documents` (0-indexed).
    pub index: usize,
    pub score: f32,
}

#[derive(Debug, Clone)]
pub struct RerankResponse {
    /// Wyniki posortowane malejąco po score (gwarantuje dispatcher impl).
    pub results: Vec<RerankResult>,
    pub usage: TokenUsage,
}

#[async_trait]
pub trait RerankDispatcher: Send + Sync {
    async fn rerank(&self, req: RerankRequest) -> Result<RerankResponse>;
}
