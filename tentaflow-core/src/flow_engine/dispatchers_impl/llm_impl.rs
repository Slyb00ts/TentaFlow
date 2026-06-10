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
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Instant;
use tokio::time::sleep_until;
use tokio_util::sync::CancellationToken;

use super::{build_user_context, ModelRuntimeSlot};
use crate::api::openai::types::{
    ChatCompletionChunk, ChatCompletionRequest, ContentPart, FunctionCall, FunctionDefinition,
    ImageUrl, Message, MessageContent, Tool, ToolCall, ToolChoice,
};
use crate::flow_engine::blob_store::BlobStore;
use crate::flow_engine::dispatchers::{LlmDispatcher, LlmRequest, LlmResponse, LlmToolSpec};
use crate::flow_engine::envelope::{
    ChatMessage, ChatMessageContent, ChatRole, FinishReason, LlmStreamChunk, LlmToolCall,
    MessagePart, TokenUsage, ToolCallDelta,
};
use crate::services::runtime::context::ExecutionContext as RuntimeContext;
use base64::Engine;
use std::sync::Arc;

pub struct LlmDispatcherImpl {
    runtime: ModelRuntimeSlot,
    /// Etap 3b: BlobStore do rozwijania `MessagePart::Image.blob_ref` na
    /// data URL przed wysłaniem do backendu.
    blobs: Arc<dyn BlobStore>,
}

impl LlmDispatcherImpl {
    pub fn new(runtime: ModelRuntimeSlot, blobs: Arc<dyn BlobStore>) -> Self {
        Self { runtime, blobs }
    }

    fn runtime(
        &self,
    ) -> Result<std::sync::Arc<crate::services::runtime::executor::ModelRuntimeExecutor>> {
        self.runtime
            .read()
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow!("LlmDispatcher: ModelRuntimeExecutor not wired"))
    }
}

#[async_trait]
impl LlmDispatcher for LlmDispatcherImpl {
    async fn execute_chat(&self, req: LlmRequest) -> Result<LlmResponse> {
        let cancel = req.cancel_token.clone();
        let deadline = req.deadline;
        let api_req = build_chat_request(&req, false, self.blobs.as_ref()).await?;
        let user = build_user_context(req.user_id, req.user_role.as_deref());
        let mut rctx = RuntimeContext::new(user);
        // Cancel + deadline są egzekwowane na poziomie wrappera bo
        // ModelRuntimeExecutor::execute_chat nie eksponuje tych pól.
        // select! w pierwszej kolejności sprawdza cancel/deadline, więc
        // klient disconnect / timeout abort'uje request natychmiast nawet
        // jeśli backend nie odpowiada.
        let runtime = self.runtime()?;
        let response = run_with_deadline_and_cancel(
            runtime.execute_chat(api_req, &mut rctx),
            deadline,
            cancel,
        )
        .await
        .map_err(|e| anyhow!("LlmDispatcher execute_chat: {e}"))?;

        let choice = response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("LlmDispatcher: backend returned 0 choices"))?;

        let tool_calls: Vec<LlmToolCall> = choice
            .message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(openai_tool_call_to_envelope)
            .collect();

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
            tool_calls,
        })
    }

    async fn stream_chat(
        &self,
        req: LlmRequest,
    ) -> Result<BoxStream<'static, Result<LlmStreamChunk>>> {
        let cancel = req.cancel_token.clone();
        let deadline = req.deadline;
        let api_req = build_chat_request(&req, true, self.blobs.as_ref()).await?;
        let user = build_user_context(req.user_id, req.user_role.as_deref());
        let mut rctx = RuntimeContext::new(user);
        // Pre-handoff: budowa streamu też podlega cancel/deadline. Gdy
        // resolver/strategy się zacina lub backend nie zdąży otworzyć
        // strumienia w czasie, abort'ujemy zanim zwrócimy stream do callera.
        let runtime = self.runtime()?;
        let stream = run_with_deadline_and_cancel(
            runtime.stream_chat(api_req, &mut rctx),
            deadline,
            cancel.clone(),
        )
        .await
        .map_err(|e| anyhow!("LlmDispatcher stream_chat: {e}"))?;

        // Post-handoff: każdy chunk podlega cancel + deadline. Gdy executor
        // (lub klient) anuluje request, stream kończy się przy najbliższym
        // poll'u. Stream EOF i tak zatrzyma backend producer.
        let cancel_for_stream = cancel;
        let mapped = stream.map(|item| match item {
            Ok(chunk) => Ok(chat_chunk_to_llm_chunk(chunk)),
            Err(e) => Err(anyhow!("LlmDispatcher stream chunk: {e}")),
        });
        let bounded = StreamBoundary::new(Box::pin(mapped), deadline, cancel_for_stream);
        Ok(Box::pin(bounded))
    }
}

/// Wykonuje future z deadline (`tokio::time::timeout`) + cancel
/// (`select!` z `cancel.cancelled()`). Te dwa pola żyją w `LlmRequest`
/// ale `ModelRuntimeExecutor` ich nie czyta — wrapper musi sam je honorować.
async fn run_with_deadline_and_cancel<F, T, E>(
    fut: F,
    deadline: Option<Instant>,
    cancel: CancellationToken,
) -> Result<T>
where
    F: std::future::Future<Output = std::result::Result<T, E>>,
    E: std::fmt::Display,
{
    tokio::pin!(fut);
    if let Some(dl) = deadline {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(anyhow!("cancelled")),
            _ = sleep_until(dl.into()) => Err(anyhow!("deadline exceeded")),
            res = &mut fut => res.map_err(|e| anyhow!("{e}")),
        }
    } else {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(anyhow!("cancelled")),
            res = &mut fut => res.map_err(|e| anyhow!("{e}")),
        }
    }
}

/// Stream wrapper który przerywa kolejne `poll_next` gdy cancel albo
/// deadline minęły. Sam adapter_stream może być nieświadomy cancel'a —
/// my zatrzymujemy konsumpcję na granicy chunka, a backend zauważy
/// rozłączenie po EOF.
struct StreamBoundary<S> {
    inner: Pin<Box<S>>,
    deadline: Option<Instant>,
    cancel: CancellationToken,
    finished: bool,
}

impl<S> StreamBoundary<S> {
    fn new(inner: Pin<Box<S>>, deadline: Option<Instant>, cancel: CancellationToken) -> Self {
        Self {
            inner,
            deadline,
            cancel,
            finished: false,
        }
    }
}

impl<S> futures::Stream for StreamBoundary<S>
where
    S: futures::Stream<Item = Result<LlmStreamChunk>> + Send,
{
    type Item = Result<LlmStreamChunk>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.finished {
            return Poll::Ready(None);
        }
        if self.cancel.is_cancelled() {
            self.finished = true;
            return Poll::Ready(Some(Err(anyhow!("cancelled"))));
        }
        if let Some(dl) = self.deadline {
            if Instant::now() >= dl {
                self.finished = true;
                return Poll::Ready(Some(Err(anyhow!("deadline exceeded"))));
            }
        }
        match self.inner.as_mut().poll_next(cx) {
            Poll::Ready(None) => {
                self.finished = true;
                Poll::Ready(None)
            }
            other => other,
        }
    }
}

async fn build_chat_request(
    req: &LlmRequest,
    stream: bool,
    blobs: &dyn BlobStore,
) -> Result<ChatCompletionRequest> {
    let mut messages = Vec::with_capacity(req.messages.len());
    for m in &req.messages {
        messages.push(chat_msg_to_openai(m, blobs).await?);
    }
    Ok(ChatCompletionRequest {
        model: req.model.clone(),
        messages,
        temperature: req.temperature,
        max_tokens: req.max_tokens,
        top_p: req.top_p,
        frequency_penalty: req.frequency_penalty,
        presence_penalty: req.presence_penalty,
        stop: if req.stop.is_empty() {
            None
        } else {
            Some(req.stop.clone())
        },
        stream,
        stream_options: None,
        user: None,
        response_format: None,
        tools: if req.tools.is_empty() {
            None
        } else {
            Some(req.tools.iter().map(tool_spec_to_openai).collect())
        },
        tool_choice: req.tool_choice.clone().map(ToolChoice::String),
        n: None,
        memory_options: None,
        audio_input: None,
    })
}

fn tool_spec_to_openai(spec: &LlmToolSpec) -> Tool {
    Tool {
        tool_type: "function".to_string(),
        function: FunctionDefinition {
            name: spec.name.clone(),
            description: Some(spec.description.clone()),
            parameters: Some(spec.parameters.clone()),
        },
    }
}

fn openai_tool_call_to_envelope(tc: ToolCall) -> LlmToolCall {
    LlmToolCall {
        id: tc.id,
        name: tc.function.name,
        arguments: tc.function.arguments,
    }
}

/// Reverse mapping used when resending conversation history (a tool loop
/// replays the assistant message that requested the calls) and when the
/// converter rebuilds an OpenAI response from a flow outcome.
pub(crate) fn envelope_tool_call_to_openai(tc: &LlmToolCall) -> ToolCall {
    ToolCall {
        id: tc.id.clone(),
        tool_type: "function".to_string(),
        function: FunctionCall {
            name: tc.name.clone(),
            arguments: tc.arguments.clone(),
        },
    }
}

/// Etap 3b: async — rozwija `MessagePart::Image.blob_ref` przez BlobStore
/// na base64 data URL. `Text` content przechodzi bez async work.
async fn chat_msg_to_openai(m: &ChatMessage, blobs: &dyn BlobStore) -> Result<Message> {
    let content = match &m.content {
        ChatMessageContent::Text(t) => Some(MessageContent::Text(t.clone())),
        ChatMessageContent::Parts(parts) => {
            let mut openai_parts = Vec::with_capacity(parts.len());
            for p in parts {
                match p {
                    MessagePart::Text { text } => {
                        openai_parts.push(ContentPart::Text { text: text.clone() });
                    }
                    MessagePart::Image { blob_ref, detail } => {
                        let bytes = blobs.get(blob_ref).await?;
                        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                        let url = format!("data:{};base64,{}", blob_ref.mime, b64);
                        openai_parts.push(ContentPart::ImageUrl {
                            image_url: ImageUrl {
                                url,
                                detail: Some(detail.clone()),
                            },
                        });
                    }
                }
            }
            Some(MessageContent::Parts(openai_parts))
        }
    };
    Ok(Message {
        role: chat_role_to_str(m.role).to_string(),
        content,
        reasoning_content: None,
        name: m.name.clone(),
        tool_calls: m
            .tool_calls
            .as_ref()
            .map(|tcs| tcs.iter().map(envelope_tool_call_to_openai).collect()),
        tool_call_id: m.tool_call_id.clone(),
    })
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

/// Stage 3d Krok 1a: zakładamy **single-choice-per-chunk** invariant —
/// OpenAI streaming protocol emituje osobne SSE events dla każdego choice
/// gdy `n>1` (każdy chunk ma `choices: [{index, delta, ...}]` z jedną
/// pozycją). Bierzemy `chunk.choices.next()` i propagujemy `choice.index`.
/// Wielochunkowe `chunk.choices[..]` (np. backend agreguje multiple
/// choices per emit) **nie są wspierane** w v1 — multi-choice routing
/// fan-out wymagałby zmiany sygnatury z `LlmStreamChunk` na `Vec<...>`,
/// out-of-scope Krok 1.
fn chat_chunk_to_llm_chunk(chunk: ChatCompletionChunk) -> LlmStreamChunk {
    let mut choice_index: u32 = 0;
    let mut text_delta = String::new();
    let mut reasoning_delta: Option<String> = None;
    let mut tool_calls: Vec<ToolCallDelta> = Vec::new();
    let mut finish_reason: Option<FinishReason> = None;

    if let Some(choice) = chunk.choices.into_iter().next() {
        choice_index = choice.index;
        if let Some(c) = choice.delta.content {
            text_delta = c;
        }
        if let Some(r) = choice.delta.reasoning_content {
            reasoning_delta = Some(r);
        }
        if let Some(tcs) = choice.delta.tool_calls {
            // Empty strings mean "no delta for this field in this chunk" → None.
            tool_calls = tcs
                .into_iter()
                .map(|tc| {
                    let (function_name, arguments_delta) = match tc.function {
                        Some(f) => (
                            f.name.filter(|s| !s.is_empty()),
                            f.arguments.filter(|s| !s.is_empty()),
                        ),
                        None => (None, None),
                    };
                    ToolCallDelta {
                        index: tc.index,
                        id: tc.id.filter(|s| !s.is_empty()),
                        function_name,
                        arguments_delta,
                    }
                })
                .collect();
        }
        if let Some(fr) = choice.finish_reason {
            finish_reason = Some(openai_finish_to_envelope(Some(&fr)));
        }
    }

    LlmStreamChunk {
        choice_index,
        text_delta,
        reasoning_delta,
        tool_calls,
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
        assert_eq!(
            openai_finish_to_envelope(Some("length")),
            FinishReason::Length
        );
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

    #[tokio::test]
    async fn chat_msg_round_trips_role_and_content() {
        use crate::flow_engine::blob_store::InMemoryBlobStore;
        let m = ChatMessage::user("hello");
        let blobs = InMemoryBlobStore::new();
        let api = chat_msg_to_openai(&m, &blobs).await.unwrap();
        assert_eq!(api.role, "user");
        match api.content {
            Some(MessageContent::Text(t)) => assert_eq!(t, "hello"),
            _ => panic!("expected text content"),
        }
    }

    #[tokio::test]
    async fn build_chat_request_maps_tools_and_tool_choice() {
        use crate::flow_engine::blob_store::InMemoryBlobStore;
        let blobs = InMemoryBlobStore::new();
        let mut req = LlmRequest::new("m");
        req.messages = vec![ChatMessage::user("hi")];
        req.tools = vec![LlmToolSpec {
            name: "memory.memory_store".into(),
            description: "Store a fact".into(),
            parameters: serde_json::json!({"type":"object"}),
        }];
        req.tool_choice = Some("auto".into());
        let api = build_chat_request(&req, false, &blobs).await.unwrap();
        let tools = api.tools.expect("tools should be set");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].tool_type, "function");
        assert_eq!(tools[0].function.name, "memory.memory_store");
        assert_eq!(
            tools[0].function.description.as_deref(),
            Some("Store a fact")
        );
        assert_eq!(
            tools[0].function.parameters,
            Some(serde_json::json!({"type":"object"}))
        );
        match api.tool_choice {
            Some(ToolChoice::String(ref s)) => assert_eq!(s, "auto"),
            other => panic!("expected string tool_choice, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn build_chat_request_omits_tools_when_empty() {
        use crate::flow_engine::blob_store::InMemoryBlobStore;
        let blobs = InMemoryBlobStore::new();
        let mut req = LlmRequest::new("m");
        req.messages = vec![ChatMessage::user("hi")];
        let api = build_chat_request(&req, false, &blobs).await.unwrap();
        assert!(api.tools.is_none());
        assert!(api.tool_choice.is_none());
    }

    #[tokio::test]
    async fn chat_msg_carries_assistant_tool_calls_to_openai() {
        use crate::flow_engine::blob_store::InMemoryBlobStore;
        let blobs = InMemoryBlobStore::new();
        let mut m = ChatMessage::assistant("");
        m.tool_calls = Some(vec![LlmToolCall {
            id: "call_0_aa".into(),
            name: "t".into(),
            arguments: "{\"a\":1}".into(),
        }]);
        let api = chat_msg_to_openai(&m, &blobs).await.unwrap();
        let tcs = api.tool_calls.expect("tool_calls should round trip");
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].id, "call_0_aa");
        assert_eq!(tcs[0].tool_type, "function");
        assert_eq!(tcs[0].function.name, "t");
        assert_eq!(tcs[0].function.arguments, "{\"a\":1}");
    }

    #[test]
    fn chat_chunk_maps_delta_tool_calls() {
        use crate::api::openai::types::{
            ChunkChoice, Delta, FunctionCallDelta, ToolCallDelta as WireToolCallDelta,
        };
        let chunk = ChatCompletionChunk {
            id: "c".into(),
            object: "chat.completion.chunk".into(),
            created: 0,
            model: "m".into(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: Delta {
                    role: None,
                    content: None,
                    reasoning_content: None,
                    tool_calls: Some(vec![
                        WireToolCallDelta {
                            index: 1,
                            id: Some("call_1".into()),
                            tool_type: Some("function".into()),
                            function: Some(FunctionCallDelta {
                                name: Some("t".into()),
                                arguments: Some("{\"a\"".into()),
                            }),
                        },
                        // Continuation fragment: only index + arguments, the
                        // shape OpenAI sends after the slot-opening fragment.
                        WireToolCallDelta {
                            index: 1,
                            id: None,
                            tool_type: None,
                            function: Some(FunctionCallDelta {
                                name: None,
                                arguments: Some(":1}".into()),
                            }),
                        },
                    ]),
                },
                finish_reason: None,
                logprobs: None,
            }],
            system_fingerprint: None,
            audio: None,
            detected_intent: None,
            detected_tools: None,
            transcribed_text: None,
            speaker_id: None,
            speaker_name: None,
            usage: None,
        };
        let mapped = chat_chunk_to_llm_chunk(chunk);
        assert_eq!(mapped.tool_calls.len(), 2);
        assert_eq!(mapped.tool_calls[0].index, 1);
        assert_eq!(mapped.tool_calls[0].id.as_deref(), Some("call_1"));
        assert_eq!(mapped.tool_calls[0].function_name.as_deref(), Some("t"));
        assert_eq!(
            mapped.tool_calls[0].arguments_delta.as_deref(),
            Some("{\"a\"")
        );
        assert_eq!(mapped.tool_calls[1].index, 1);
        assert!(mapped.tool_calls[1].id.is_none());
        assert!(mapped.tool_calls[1].function_name.is_none());
        assert_eq!(mapped.tool_calls[1].arguments_delta.as_deref(), Some(":1}"));
    }

    /// Etap 3b: multimodal Parts → OpenAI Parts z base64 data URL.
    #[tokio::test]
    async fn chat_msg_multimodal_resolves_blob_to_data_url() {
        use crate::flow_engine::blob_store::InMemoryBlobStore;
        use crate::flow_engine::envelope::MessagePart;
        let blobs = InMemoryBlobStore::new();
        let blob_ref = blobs
            .put(b"fake-jpeg".to_vec(), "image/jpeg")
            .await
            .unwrap();
        let m = ChatMessage::user_multimodal(vec![
            MessagePart::Text {
                text: "what?".into(),
            },
            MessagePart::Image {
                blob_ref: blob_ref.clone(),
                detail: "auto".into(),
            },
        ]);
        let api = chat_msg_to_openai(&m, &blobs).await.unwrap();
        match api.content {
            Some(MessageContent::Parts(parts)) => {
                assert_eq!(parts.len(), 2);
                if let ContentPart::ImageUrl { image_url } = &parts[1] {
                    assert!(image_url.url.starts_with("data:image/jpeg;base64,"));
                    assert_eq!(image_url.detail.as_deref(), Some("auto"));
                } else {
                    panic!("expected ImageUrl in parts[1]");
                }
            }
            _ => panic!("expected Parts content"),
        }
    }
}
