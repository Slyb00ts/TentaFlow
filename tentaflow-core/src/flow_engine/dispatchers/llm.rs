// =============================================================================
// Plik: flow_engine/dispatchers/llm.rs
// Opis: LlmDispatcher trait + DTO. Wrapper nad services/runtime/executor.rs::
//       execute_chat / stream_chat. Mapping do/z OpenAI-compat rkyv idzie
//       w impl wrapperu (dochodzi razem z executor rewrite).
// =============================================================================

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::BoxStream;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::flow_engine::envelope::{ChatMessage, LlmStreamChunk, TokenUsage};

#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub stop: Vec<String>,
    pub deadline: Option<Instant>,
    pub cancel_token: CancellationToken,
    /// User context propagated z `ExecutionContext.user_id` / `user_role`.
    /// Wrapper przekazuje to do `RuntimeContext` żeby resolver/strategy nie
    /// widziały `user=None` mimo że request przyszedł od zalogowanego usera.
    pub user_id: Option<i64>,
    pub user_role: Option<String>,
}

impl LlmRequest {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            messages: Vec::new(),
            temperature: None,
            max_tokens: None,
            stop: Vec::new(),
            deadline: None,
            cancel_token: CancellationToken::new(),
            user_id: None,
            user_role: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub content: String,
    pub usage: TokenUsage,
    pub finish_reason: super::super::envelope::FinishReason,
}

#[async_trait]
pub trait LlmDispatcher: Send + Sync {
    async fn execute_chat(&self, req: LlmRequest) -> Result<LlmResponse>;
    async fn stream_chat(
        &self,
        req: LlmRequest,
    ) -> Result<BoxStream<'static, Result<LlmStreamChunk>>>;
}
