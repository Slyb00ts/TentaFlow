// =============================================================================
// Plik: flow_engine/dispatchers_impl/llm_impl.rs
// Opis: LlmDispatcherImpl — wrapper nad
//       `ModelRuntimeExecutor::execute_chat` / `stream_chat`. Mapuje DTO
//       flow-engine (`LlmRequest` / `LlmResponse` / `LlmStreamChunk`)
//       w obie strony z OpenAI-compatible typami runtime.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use std::sync::Arc;

use crate::api::openai::types::{
    ChatCompletionChunk, ChatCompletionRequest, Message, MessageContent,
};
use crate::flow_engine::dispatchers::{LlmDispatcher, LlmRequest, LlmResponse};
use crate::flow_engine::envelope::{ChatMessage, ChatRole, FinishReason, LlmStreamChunk, TokenUsage};
use crate::services::runtime::context::ExecutionContext as RuntimeContext;
use crate::services::runtime::executor::ModelRuntimeExecutor;

pub struct LlmDispatcherImpl {
    runtime: Arc<ModelRuntimeExecutor>,
}

impl LlmDispatcherImpl {
    pub fn new(runtime: Arc<ModelRuntimeExecutor>) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl LlmDispatcher for LlmDispatcherImpl {
    async fn execute_chat(&self, req: LlmRequest) -> Result<LlmResponse> {
        let api_req = build_chat_request(&req, false);
        let mut rctx = RuntimeContext::new(None);
        let response = self
            .runtime
            .execute_chat(api_req, &mut rctx)
            .await
            .map_err(|e| anyhow!("LlmDispatcher execute_chat: {e}"))?;

        let choice = response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("LlmDispatcher: backend returned 0 choices"))?;

        let content = match choice.message.content {
            Some(MessageContent::Text(t)) => t,
            Some(MessageContent::Parts(parts)) => parts
                .into_iter()
                .filter_map(|p| match p {
                    crate::api::openai::types::ContentPart::Text { text } => Some(text),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(""),
            None => String::new(),
        };

        let usage = response
            .usage
            .map(|u| TokenUsage {
                prompt_tokens: u.prompt_tokens as u64,
                completion_tokens: u.completion_tokens as u64,
                total_tokens: u.total_tokens as u64,
            })
            .unwrap_or_default();

        let finish_reason = openai_finish_to_envelope(choice.finish_reason.as_deref());

        Ok(LlmResponse {
            content,
            usage,
            finish_reason,
        })
    }

    async fn stream_chat(
        &self,
        req: LlmRequest,
    ) -> Result<BoxStream<'static, Result<LlmStreamChunk>>> {
        let api_req = build_chat_request(&req, true);
        let mut rctx = RuntimeContext::new(None);
        let stream = self
            .runtime
            .stream_chat(api_req, &mut rctx)
            .await
            .map_err(|e| anyhow!("LlmDispatcher stream_chat: {e}"))?;

        // ExecutorChunkStream → BoxStream<Result<LlmStreamChunk>>.
        // Każdy ChatCompletionChunk producent zwraca albo text delta, albo
        // reasoning delta, albo terminal chunk z finish_reason. Mapujemy
        // 1:1 — backpressure i finalizacja zostają u callera (executor).
        let mapped = stream.map(|item| match item {
            Ok(chunk) => Ok(chat_chunk_to_llm_chunk(chunk)),
            Err(e) => Err(anyhow!("LlmDispatcher stream chunk: {e}")),
        });
        Ok(Box::pin(mapped))
    }
}

fn build_chat_request(req: &LlmRequest, stream: bool) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: req.model.clone(),
        messages: req.messages.iter().map(chat_msg_to_openai).collect(),
        temperature: req.temperature,
        max_tokens: req.max_tokens,
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: if req.stop.is_empty() {
            None
        } else {
            Some(req.stop.clone())
        },
        stream,
        user: None,
        response_format: None,
        tools: None,
        tool_choice: None,
        n: None,
        memory_options: None,
        audio_input: None,
    }
}

fn chat_msg_to_openai(m: &ChatMessage) -> Message {
    Message {
        role: chat_role_to_str(m.role).to_string(),
        content: Some(MessageContent::Text(m.content.clone())),
        reasoning_content: None,
        name: m.name.clone(),
        tool_calls: None,
        tool_call_id: m.tool_call_id.clone(),
    }
}

fn chat_role_to_str(r: ChatRole) -> &'static str {
    match r {
        ChatRole::System => "system",
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
        ChatRole::Tool => "tool",
    }
}

fn openai_finish_to_envelope(s: Option<&str>) -> FinishReason {
    match s {
        Some("stop") => FinishReason::Stop,
        Some("length") => FinishReason::Length,
        Some("tool_calls") => FinishReason::ToolCalls,
        Some("content_filter") => FinishReason::ContentFilter,
        // Brak finish_reason w response oznacza że backend nie zaraportował —
        // traktujemy jak Stop (najbliższe legacy zachowanie). Cancelled/Error
        // są emitowane wyłącznie w finalizerze executora po cancel/Err.
        _ => FinishReason::Stop,
    }
}

fn chat_chunk_to_llm_chunk(chunk: ChatCompletionChunk) -> LlmStreamChunk {
    let mut text_delta = String::new();
    let mut reasoning_delta: Option<String> = None;
    let mut finish_reason: Option<FinishReason> = None;

    if let Some(choice) = chunk.choices.into_iter().next() {
        if let Some(c) = choice.delta.content {
            text_delta = c;
        }
        if let Some(r) = choice.delta.reasoning_content {
            reasoning_delta = Some(r);
        }
        if let Some(fr) = choice.finish_reason {
            finish_reason = Some(openai_finish_to_envelope(Some(&fr)));
        }
    }

    LlmStreamChunk {
        text_delta,
        reasoning_delta,
        tool_calls: Vec::new(),
        usage: None,
        finish_reason,
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_reason_mapping_covers_canonical_values() {
        assert_eq!(openai_finish_to_envelope(Some("stop")), FinishReason::Stop);
        assert_eq!(openai_finish_to_envelope(Some("length")), FinishReason::Length);
        assert_eq!(
            openai_finish_to_envelope(Some("tool_calls")),
            FinishReason::ToolCalls
        );
        assert_eq!(
            openai_finish_to_envelope(Some("content_filter")),
            FinishReason::ContentFilter
        );
        // Unknown / None default to Stop, never to Cancelled/Error.
        assert_eq!(openai_finish_to_envelope(None), FinishReason::Stop);
        assert_eq!(openai_finish_to_envelope(Some("xxx")), FinishReason::Stop);
    }

    #[test]
    fn chat_msg_round_trips_role_and_content() {
        let m = ChatMessage::user("hello");
        let api = chat_msg_to_openai(&m);
        assert_eq!(api.role, "user");
        match api.content {
            Some(MessageContent::Text(t)) => assert_eq!(t, "hello"),
            _ => panic!("expected text content"),
        }
    }
}
