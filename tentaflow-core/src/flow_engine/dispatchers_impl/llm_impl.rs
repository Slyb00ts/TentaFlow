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
    ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse, Choice, ContentPart,
    FunctionCall, FunctionDefinition, ImageUrl, Message, MessageContent, Tool, ToolCall,
    ToolChoice, Usage,
};
use crate::auth::acl::UserContext;
use crate::compliance::ai_gateway::{AiGateway, AiGatewayContext};
use crate::db::DbPool;
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
    /// Harness §3.4: DbPool backing the AiGateway so every blocking
    /// `execute_chat` opens/finishes one `compliance_ai_events` row. `None` in
    /// tests / DB-less bootstraps — audit is then skipped, generation unchanged.
    db: Option<DbPool>,
    /// Node id stamped on AI audit events (the local node, like the routing
    /// layer's gateway).
    node_id: String,
}

impl LlmDispatcherImpl {
    pub fn new(
        runtime: ModelRuntimeSlot,
        blobs: Arc<dyn BlobStore>,
        db: Option<DbPool>,
        node_id: impl Into<String>,
    ) -> Self {
        Self {
            runtime,
            blobs,
            db,
            node_id: node_id.into(),
        }
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

    /// Opens a `compliance_ai_events` row for one blocking chat call. Returns
    /// `None` when there is no DB (tests) — audit is then a no-op. The gateway
    /// mints a fresh per-call `request_id` (distinct so it never collides with
    /// the routing-layer session event on `UNIQUE(org_id, request_id)`), while
    /// `correlation_id` carries the turn's shared key from `envelope.meta` so
    /// the session row and every per-call row of one user turn link (§3.4). A
    /// failed audit-start must NOT block generation, so the error is logged and
    /// treated as "no event".
    fn start_audit_event(
        &self,
        req: &LlmRequest,
        api_req: &ChatCompletionRequest,
    ) -> Option<crate::compliance::ai_gateway::AiEventHandle> {
        let db = self.db.as_ref()?;
        let gateway = AiGateway::new(
            db.clone(),
            self.node_id.clone(),
            crate::compliance::ai_gateway::token_quota_enabled(),
        );
        let user = req.user_id.as_ref().map(|uid| UserContext {
            user_id: uid.clone(),
            role: req.user_role.clone().unwrap_or_else(|| "user".to_string()),
        });
        let context = AiGatewayContext {
            org_id: None,
            addon_id: None,
            instance_id: None,
            flow_id: req.flow_id.clone(),
            flow_node_id: req.flow_node_id.clone(),
            agent_id: req.agent_id.clone(),
            agent_run_id: req.agent_run_id.clone(),
            correlation_id: req.correlation_id.clone(),
            flow_meta: std::collections::BTreeMap::new(),
        };
        let mut per_call = api_req.clone();
        per_call.user = None;
        match gateway.start_chat_event(&per_call, user.as_ref(), Some(&context)) {
            Ok(handle) => Some(handle),
            Err(e) => {
                tracing::warn!("gateway-aware LLM audit start failed (skipping event): {e}");
                None
            }
        }
    }
}

/// Builds a minimal `ChatCompletionResponse` from the flow-engine `LlmResponse`
/// so the AiGateway records the real response text, usage and tool calls per
/// call. The gateway only reads content/usage/tool_calls, so the synthetic
/// wrapper is faithful for audit purposes.
fn llm_response_to_chat_response(response: &LlmResponse, model: &str) -> ChatCompletionResponse {
    let tool_calls = if response.tool_calls.is_empty() {
        None
    } else {
        Some(
            response
                .tool_calls
                .iter()
                .map(envelope_tool_call_to_openai)
                .collect(),
        )
    };
    ChatCompletionResponse {
        id: String::new(),
        object: "chat.completion".to_string(),
        created: 0,
        model: model.to_string(),
        choices: vec![Choice {
            index: 0,
            message: Message {
                audio: None,
                role: "assistant".to_string(),
                content: Some(MessageContent::Text(response.content.clone())),
                reasoning_content: response.reasoning_content.clone(),
                name: None,
                tool_calls,
                tool_call_id: None,
            },
            finish_reason: response
                .finish_reason
                .as_openai_str()
                .map(|s| s.to_string()),
            logprobs: None,
        }],
        usage: Some(Usage {
            prompt_tokens: response.usage.prompt_tokens as u32,
            completion_tokens: response.usage.completion_tokens as u32,
            total_tokens: response.usage.total_tokens as u32,
        }),
        system_fingerprint: None,
        transcribed_text: None,
        speaker_id: None,
        speaker_name: None,
        speaker_confidence: None,
        detected_intent: None,
        detected_tools: None,
    }
}

#[async_trait]
impl LlmDispatcher for LlmDispatcherImpl {
    async fn execute_chat(&self, req: LlmRequest) -> Result<LlmResponse> {
        let cancel = req.cancel_token.clone();
        let deadline = req.deadline;
        let api_req = build_chat_request(&req, false, self.blobs.as_ref()).await?;
        // Harness §3.4: open one compliance_ai_events row per call BEFORE
        // dispatch so the prompt is recorded even if the backend hangs / the
        // request is cancelled mid-flight (the event then finishes failed).
        let audit_event = self.start_audit_event(&req, &api_req);
        let model = api_req.model.clone();
        let provenance = req.provenance.clone();
        let user = build_user_context(req.user_id, req.user_role.as_deref());
        // §2.5 — the calling flow's stamp travels with the request; a fresh
        // runtime context here would report the nested dispatch as `system`.
        let mut rctx = RuntimeContext::new(user, provenance.origin, provenance.actor);
        // Cancel + deadline są egzekwowane na poziomie wrappera bo
        // ModelRuntimeExecutor::execute_chat nie eksponuje tych pól.
        // select! w pierwszej kolejności sprawdza cancel/deadline, więc
        // klient disconnect / timeout abort'uje request natychmiast nawet
        // jeśli backend nie odpowiada.
        let runtime = self.runtime()?;
        let response = match run_with_deadline_and_cancel(
            runtime.execute_chat(api_req, &mut rctx),
            deadline,
            cancel,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                let msg = format!("LlmDispatcher execute_chat: {e}");
                if let Some(event) = audit_event.as_ref() {
                    let _ = event.finish_failed(&msg);
                }
                return Err(anyhow!(msg));
            }
        };

        let choice = match response.choices.into_iter().next() {
            Some(c) => c,
            None => {
                let msg = "LlmDispatcher: backend returned 0 choices".to_string();
                if let Some(event) = audit_event.as_ref() {
                    let _ = event.finish_failed(&msg);
                }
                return Err(anyhow!(msg));
            }
        };

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
        let reasoning_content = choice.message.reasoning_content.clone();

        let usage = response
            .usage
            .map(|u| TokenUsage {
                prompt_tokens: u.prompt_tokens as u64,
                completion_tokens: u.completion_tokens as u64,
                total_tokens: u.total_tokens as u64,
            })
            .unwrap_or_default();

        let finish_reason = openai_finish_to_envelope(choice.finish_reason.as_deref());

        // Native speech from an omni model. Base64 is decoded HERE so nothing
        // downstream has to know the wire format; a payload we cannot decode is
        // dropped with a warning rather than passed on as garbage bytes.
        let audio = choice.message.audio.as_ref().and_then(|a| {
            match base64::engine::general_purpose::STANDARD.decode(&a.data) {
                Ok(bytes) if !bytes.is_empty() => Some(crate::flow_engine::dispatchers::LlmAudioOut {
                    mime: format!(
                        "audio/{}",
                        req.audio_out.as_ref().map(|o| o.format.as_str()).unwrap_or("wav")
                    ),
                    bytes,
                    transcript: a.transcript.clone(),
                }),
                Ok(_) => None,
                Err(e) => {
                    tracing::warn!("model returned audio that is not valid base64: {e}");
                    None
                }
            }
        });
        let llm_response = LlmResponse {
            audio,
            content,
            reasoning_content,
            usage,
            finish_reason,
            tool_calls,
        };
        // Finish the audit event with the real response (text, usage, the tool
        // calls the model REQUESTED — their execution outcome lands later via
        // record_tool_execution in tool_exec). Audit-write failure is logged,
        // never propagated: a generated response must still reach the caller.
        if let Some(event) = audit_event.as_ref() {
            let response = llm_response_to_chat_response(&llm_response, &model);
            if let Err(e) = event.finish_success(&response) {
                tracing::warn!("gateway-aware LLM audit finish failed: {e}");
            }
        }
        Ok(llm_response)
    }

    async fn stream_chat(
        &self,
        req: LlmRequest,
    ) -> Result<BoxStream<'static, Result<LlmStreamChunk>>> {
        let cancel = req.cancel_token.clone();
        let deadline = req.deadline;
        let api_req = build_chat_request(&req, true, self.blobs.as_ref()).await?;
        // Per-call audit event with the RESOLVED model (api_req.model from the
        // node's config) — the streaming twin of the blocking `execute` path.
        // Without it the only token bump came from the session-level chat event,
        // whose model is empty for flow chats → usage mis-attributed to "".
        // AuditFinishStream finishes it with the streamed usage (incl reasoning).
        let audit_event = self.start_audit_event(&req, &api_req);
        let provenance = req.provenance.clone();
        let user = build_user_context(req.user_id, req.user_role.as_deref());
        // §2.5 — the calling flow's stamp travels with the request; a fresh
        // runtime context here would report the nested dispatch as `system`.
        let mut rctx = RuntimeContext::new(user, provenance.origin, provenance.actor);
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
        Ok(Box::pin(AuditFinishStream::new(
            Box::pin(bounded),
            audit_event,
        )))
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

/// Wraps the bounded LLM stream to finish the per-call AiGateway audit event
/// once the stream settles, accumulating the assistant text and the real token
/// usage (the usage-tail carries reasoning tokens too). This is what bumps
/// `token_usage_daily` under the node's RESOLVED model for streaming flow chats
/// — the streaming twin of the blocking `execute` path. Mirrors
/// `ComplianceAuditStream` but for the flow engine's `LlmStreamChunk` shape.
struct AuditFinishStream<S> {
    inner: Pin<Box<S>>,
    event: Option<crate::compliance::ai_gateway::AiEventHandle>,
    text: String,
    usage: Option<TokenUsage>,
    finished: bool,
}

impl<S> AuditFinishStream<S> {
    fn new(
        inner: Pin<Box<S>>,
        event: Option<crate::compliance::ai_gateway::AiEventHandle>,
    ) -> Self {
        Self {
            inner,
            event,
            text: String::new(),
            usage: None,
            finished: false,
        }
    }

    fn finish(&mut self, error: Option<&str>) {
        if self.finished {
            return;
        }
        self.finished = true;
        let Some(event) = self.event.take() else {
            return;
        };
        if let Some(msg) = error {
            let _ = event.finish_failed(msg);
            return;
        }
        let usage = self.usage.map(|u| crate::api::openai::types::Usage {
            prompt_tokens: u.prompt_tokens as u32,
            completion_tokens: u.completion_tokens as u32,
            total_tokens: u.total_tokens as u32,
        });
        // tool_calls stay empty here: per-call tool execution is audited
        // separately; this finish exists for response text + token accounting.
        if let Err(e) = event.finish_stream_success(&self.text, usage.as_ref(), &[]) {
            tracing::warn!("gateway-aware LLM stream audit finish failed: {e}");
        }
    }
}

impl<S> futures::Stream for AuditFinishStream<S>
where
    S: futures::Stream<Item = Result<LlmStreamChunk>> + Send,
{
    type Item = Result<LlmStreamChunk>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                self.text.push_str(&chunk.text_delta);
                if let Some(u) = chunk.usage {
                    self.usage = Some(u);
                }
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(Some(Err(e))) => {
                let msg = e.to_string();
                self.finish(Some(&msg));
                Poll::Ready(Some(Err(e)))
            }
            Poll::Ready(None) => {
                self.finish(None);
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S> Drop for AuditFinishStream<S> {
    fn drop(&mut self) {
        if !self.finished {
            self.finish(Some("stream dropped before completion"));
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
    // Audio out is opt-in and needs BOTH fields: `modalities` says the turn may
    // speak, `audio` says with what voice and container. Sending either to a
    // text-only backend is rejected, so both stay absent unless asked for.
    let (modalities, audio) = match &req.audio_out {
        Some(a) => (
            Some(vec!["text".to_string(), "audio".to_string()]),
            Some(crate::api::openai::types::AudioConfig {
                voice: a.voice.clone(),
                format: a.format.clone(),
            }),
        ),
        None => (None, None),
    };
    Ok(ChatCompletionRequest {
        reasoning_effort: req.reasoning_effort.clone(),
        modalities,
        audio,
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
                    MessagePart::Audio { blob_ref, format } => {
                        // OpenAI audio parts carry RAW base64 — no `data:` URL,
                        // unlike images — and a container code rather than the
                        // MIME type.
                        let bytes = blobs.get(blob_ref).await?;
                        openai_parts.push(ContentPart::InputAudio {
                            input_audio: crate::api::openai::types::InputAudio {
                                data: base64::engine::general_purpose::STANDARD.encode(&bytes),
                                format: format.clone(),
                            },
                        });
                    }
                }
            }
            Some(MessageContent::Parts(openai_parts))
        }
    };
    Ok(Message {
        audio: None,
        role: chat_role_to_str(m.role).to_string(),
        content,
        reasoning_content: m.reasoning_content.clone(),
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
        // Przewlekamy realne liczniki z finalnego chunku silnika do flow-engine
        // (executor agreguje to do FlowExecutionOutcome.usage → bump tokenów).
        usage: chunk.usage.map(|u| TokenUsage {
            prompt_tokens: u.prompt_tokens as u64,
            completion_tokens: u.completion_tokens as u64,
            total_tokens: u.total_tokens as u64,
        }),
        perf: chunk.perf.map(|p| crate::flow_engine::envelope::GenPerf {
            ttft_ms: p.ttft_ms,
            prefill_tps: p.prefill_tps,
            decode_tps: p.decode_tps,
            total_ms: p.total_ms,
        }),
        finish_reason,
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations;
    use crate::flow_engine::blob_store::InMemoryBlobStore;
    use crate::flow_engine::dispatcher::CallProvenance;
    use crate::flow_engine::dispatchers::LlmResponse;
    use crate::flow_engine::envelope::{ChatMessage, FinishReason, TokenUsage};
    use rusqlite::Connection;

    fn audit_db() -> DbPool {
        let conn = Connection::open_in_memory().expect("memory db");
        migrations::run(&conn).expect("migrations");
        Arc::new(crate::db::Db::from_connection(conn))
    }

    fn dispatcher_with_db(db: DbPool) -> LlmDispatcherImpl {
        let runtime: ModelRuntimeSlot = Arc::new(parking_lot::RwLock::new(None));
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        LlmDispatcherImpl::new(runtime, blobs, Some(db), "node-test")
    }

    /// Harness §3.4: the gateway-aware dispatcher opens one compliance_ai_events
    /// row per call carrying the agent context, and finishing it on success
    /// stamps status + the response payload. Exercises start/finish directly
    /// (the backend dispatch in execute_chat needs a full runtime, out of scope
    /// for a unit test).
    #[tokio::test]
    async fn gateway_aware_audit_writes_event_with_agent_context() {
        let db = audit_db();
        // The run principal is a real account (compliance_ai_events.user_id FKs
        // user_accounts) — seed it, like production where the flow's user_id is
        // a logged-in user.
        db.write()
            .unwrap()
            .execute(
                "INSERT INTO user_accounts (id, username, password_hash) \
                 VALUES ('u-agent', 'agent-user', 'x')",
                [],
            )
            .expect("seed user");
        let dispatcher = dispatcher_with_db(db.clone());

        let mut req = LlmRequest::new("bielik", CallProvenance::system());
        req.messages = vec![ChatMessage::user("remember my favorite color")];
        req.user_id = Some("u-agent".into());
        req.flow_id = None;
        req.flow_node_id = Some("llm-1".into());
        req.agent_id = Some("agent-research".into());
        req.agent_run_id = Some("run-42".into());
        req.correlation_id = Some("turn-corr-1".into());

        let api_req = build_chat_request(&req, false, dispatcher.blobs.as_ref())
            .await
            .expect("build chat request");
        let event = dispatcher
            .start_audit_event(&req, &api_req)
            .expect("audit event opened");

        let response = LlmResponse {
            audio: None,
            content: "Noted — blue.".into(),
            reasoning_content: None,
            usage: TokenUsage {
                prompt_tokens: 6,
                completion_tokens: 3,
                total_tokens: 9,
            },
            finish_reason: FinishReason::Stop,
            tool_calls: Vec::new(),
        };
        let chat_response = llm_response_to_chat_response(&response, &api_req.model);
        event.finish_success(&chat_response).expect("finish event");

        let conn = db.read().expect("db lock");
        let (status, agent_id, agent_run_id, flow_node_id, model, correlation_id): (
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT status, agent_id, agent_run_id, flow_node_id, model_id, correlation_id \
                 FROM compliance_ai_events WHERE event_id = ?1",
                rusqlite::params![event.event_id()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .expect("event row");
        assert_eq!(status, "success");
        assert_eq!(agent_id.as_deref(), Some("agent-research"));
        assert_eq!(agent_run_id.as_deref(), Some("run-42"));
        assert_eq!(flow_node_id.as_deref(), Some("llm-1"));
        assert_eq!(model, "bielik");
        // The per-call event copies the turn's correlation key (§3.4), so it
        // links to the routing session event of the same user turn.
        assert_eq!(correlation_id.as_deref(), Some("turn-corr-1"));

        // Prompt + response payloads recorded for this one call.
        let payload_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM compliance_ai_payloads WHERE event_id = ?1",
                rusqlite::params![event.event_id()],
                |row| row.get(0),
            )
            .expect("payload count");
        assert_eq!(payload_count, 2);
    }

    /// Without a DB the dispatcher must still work — audit is simply skipped
    /// (start_audit_event returns None), generation is unaffected.
    #[tokio::test]
    async fn no_db_skips_audit() {
        let runtime: ModelRuntimeSlot = Arc::new(parking_lot::RwLock::new(None));
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let dispatcher = LlmDispatcherImpl::new(runtime, blobs, None, "node-test");
        let mut req = LlmRequest::new("m", CallProvenance::system());
        req.messages = vec![ChatMessage::user("hi")];
        let api_req = build_chat_request(&req, false, dispatcher.blobs.as_ref())
            .await
            .expect("build");
        assert!(dispatcher.start_audit_event(&req, &api_req).is_none());
    }

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
        let mut req = LlmRequest::new("m", CallProvenance::system());
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
        let mut req = LlmRequest::new("m", CallProvenance::system());
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
        m.reasoning_content = Some("reasoning".into());
        m.tool_calls = Some(vec![LlmToolCall {
            id: "call_0_aa".into(),
            name: "t".into(),
            arguments: "{\"a\":1}".into(),
        }]);
        let api = chat_msg_to_openai(&m, &blobs).await.unwrap();
        assert_eq!(api.reasoning_content.as_deref(), Some("reasoning"));
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
            perf: None,
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
