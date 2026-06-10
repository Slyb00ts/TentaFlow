// =============================================================================
// Plik: flow_engine/dispatchers/llm.rs
// Opis: LlmDispatcher trait + DTO. Wrapper nad services/runtime/executor.rs::
//       execute_chat / stream_chat. Mapping do/z OpenAI-compat CBOR idzie
//       w impl wrapperu (dochodzi razem z executor rewrite).
// =============================================================================

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::BoxStream;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::flow_engine::envelope::{ChatMessage, LlmStreamChunk, LlmToolCall, TokenUsage};

/// Tool advertised to the model — backend-agnostic shape mapped per
/// candidate to either native OpenAI `tools` or the prompt-mode section
/// (`services/runtime/tool_calling.rs`). `parameters` is a JSON Schema
/// object describing the arguments.
#[derive(Debug, Clone)]
pub struct LlmToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    /// Etap 2: pozostałe sampling params z `ChatCompletionRequest` —
    /// adapter LLM czyta je z fallback `node.config -> envelope.meta`.
    pub top_p: Option<f32>,
    pub frequency_penalty: Option<f32>,
    pub presence_penalty: Option<f32>,
    pub stop: Vec<String>,
    /// Tools offered for this request. Empty = no tool calling — the
    /// outgoing request carries no `tools` field at all.
    pub tools: Vec<LlmToolSpec>,
    /// OpenAI-style tool_choice ("auto" / "none" / "required"). `None`
    /// leaves the backend default.
    pub tool_choice: Option<String>,
    pub deadline: Option<Instant>,
    pub cancel_token: CancellationToken,
    /// User context propagated z `ExecutionContext.user_id` / `user_role`.
    /// Wrapper przekazuje to do `RuntimeContext` żeby resolver/strategy nie
    /// widziały `user=None` mimo że request przyszedł od zalogowanego usera.
    pub user_id: Option<String>,
    pub user_role: Option<String>,
}

impl LlmRequest {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            messages: Vec::new(),
            temperature: None,
            max_tokens: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: Vec::new(),
            tools: Vec::new(),
            tool_choice: None,
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
    /// Tool invocations requested by the model. Empty when the backend
    /// answered with plain content only.
    pub tool_calls: Vec<LlmToolCall>,
}

#[async_trait]
pub trait LlmDispatcher: Send + Sync {
    async fn execute_chat(&self, req: LlmRequest) -> Result<LlmResponse>;
    async fn stream_chat(
        &self,
        req: LlmRequest,
    ) -> Result<BoxStream<'static, Result<LlmStreamChunk>>>;
}
