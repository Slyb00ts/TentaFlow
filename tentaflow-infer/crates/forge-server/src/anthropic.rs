// ===== File: anthropic.rs — Anthropic-compatible Messages API (SPEC §8.1) =====
// POST /v1/messages. A thin translation layer over the SAME internal generate
// path as /v1/chat/completions: the Anthropic request (system + messages with
// content blocks, max_tokens, stop_sequences, sampling) is mapped to the
// server's ChatMessage list and GenerationSpec, run through `start_generation`,
// and the engine stream is rendered back into the Anthropic response shape
// (non-streaming JSON message, or the message_start → content_block_* →
// message_delta → message_stop SSE event sequence). No parallel generate path.

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use forge_engine::generate::FinishReason;
use forge_engine::sample::SamplingParams;
use forge_engine::server::EngineEvent;
use forge_tokenize::ChatMessage;
use serde::Deserialize;
use tokio_stream::wrappers::ReceiverStream;

use crate::api::GenerationSpec;
use crate::error::ApiError;
use crate::routes::{collect_events, new_id, start_generation, GenInput};
use crate::toolcall::OutputParser;
use crate::ServerState;

/// Anthropic `system`: a plain string or an array of text content blocks.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum SystemPrompt {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

impl SystemPrompt {
    fn into_text(self) -> String {
        match self {
            SystemPrompt::Text(s) => s,
            SystemPrompt::Blocks(b) => join_text_blocks(&b),
        }
    }
}

/// Anthropic message `content`: a plain string or an array of content blocks.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

impl MessageContent {
    fn into_text(self) -> String {
        match self {
            MessageContent::Text(s) => s,
            MessageContent::Blocks(b) => join_text_blocks(&b),
        }
    }
}

/// One Anthropic content block. Only `text` blocks carry generation-relevant
/// content here; other block types (image, tool_use, tool_result) are accepted
/// and their text, if any, is surfaced — non-text blocks contribute nothing.
#[derive(Debug, Clone, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(default)]
    text: Option<String>,
}

/// Concatenate the `text` of every `text` block (Anthropic multi-block content).
fn join_text_blocks(blocks: &[ContentBlock]) -> String {
    let mut out = String::new();
    for b in blocks {
        if b.block_type == "text" {
            if let Some(t) = &b.text {
                out.push_str(t);
            }
        }
    }
    out
}

#[derive(Debug, Clone, Deserialize)]
struct AnthropicMessage {
    role: String,
    content: MessageContent,
}

/// Anthropic `POST /v1/messages` request body.
#[derive(Debug, Deserialize)]
pub struct MessagesRequest {
    pub model: String,
    messages: Vec<AnthropicMessage>,
    #[serde(default)]
    system: Option<SystemPrompt>,
    /// Required by Anthropic (the completion budget).
    #[serde(default)]
    max_tokens: Option<usize>,
    #[serde(default)]
    stop_sequences: Option<Vec<String>>,
    #[serde(default)]
    temperature: Option<f32>,
    #[serde(default)]
    top_p: Option<f32>,
    #[serde(default)]
    top_k: Option<usize>,
    #[serde(default)]
    stream: bool,
}

const ANTHROPIC_ROLES: &[&str] = &["user", "assistant"];

impl MessagesRequest {
    /// Translate the Anthropic request into the server's ChatMessage list
    /// (system prompt prepended) plus the engine GenerationSpec.
    fn resolve(self) -> Result<(Vec<ChatMessage>, GenerationSpec, bool), ApiError> {
        if self.messages.is_empty() {
            return Err(ApiError::invalid_request("messages must not be empty"));
        }
        let Some(max_tokens) = self.max_tokens else {
            return Err(ApiError::invalid_request("max_tokens is required"));
        };
        if max_tokens == 0 {
            return Err(ApiError::invalid_request("max_tokens must be at least 1"));
        }

        let mut messages: Vec<ChatMessage> = Vec::with_capacity(self.messages.len() + 1);
        if let Some(system) = self.system {
            let text = system.into_text();
            if !text.is_empty() {
                messages.push(ChatMessage::text("system", text));
            }
        }
        for (i, m) in self.messages.into_iter().enumerate() {
            if !ANTHROPIC_ROLES.contains(&m.role.as_str()) {
                return Err(ApiError::invalid_request(format!(
                    "messages[{i}].role {:?} is not one of {ANTHROPIC_ROLES:?}",
                    m.role
                )));
            }
            messages.push(ChatMessage::text(m.role, m.content.into_text()));
        }

        let mut sampling = SamplingParams::default();
        if let Some(t) = self.temperature {
            if !t.is_finite() || t < 0.0 {
                return Err(ApiError::invalid_request(
                    "temperature must be a finite number >= 0",
                ));
            }
            sampling.temperature = t;
        }
        if let Some(p) = self.top_p {
            if !p.is_finite() || p <= 0.0 || p > 1.0 {
                return Err(ApiError::invalid_request("top_p must be in (0, 1]"));
            }
            sampling.top_p = p;
        }
        if let Some(k) = self.top_k {
            sampling.top_k = k;
        }

        let spec = GenerationSpec {
            sampling,
            max_tokens,
            stop: self.stop_sequences.unwrap_or_default(),
            n: 1,
            logit_bias: Vec::new(),
            min_tokens: 0,
            logprobs: None,
            echo: false,
        };
        Ok((messages, spec, self.stream))
    }
}

/// Map the engine finish reason to the Anthropic `stop_reason` vocabulary:
/// EOS → end_turn, length cap → max_tokens, matched stop sequence →
/// stop_sequence.
fn stop_reason(reason: FinishReason) -> &'static str {
    match reason {
        FinishReason::Eos => "end_turn",
        FinishReason::Length => "max_tokens",
        FinishReason::Stop => "stop_sequence",
    }
}

/// POST /v1/messages — Anthropic-compatible chat.
pub async fn messages(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<MessagesRequest>,
) -> Response {
    if req.model != state.model_id {
        return ApiError::model_not_found(&req.model, &state.model_id).into_response();
    }
    let stream = req.stream;
    let (messages, spec, _) = match req.resolve() {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };

    let gen = match start_generation(&state, vec![GenInput::Chat(messages, None)], spec, None).await
    {
        Ok(g) => g,
        Err(e) => return e.into_response(),
    };
    let input = gen
        .per_input
        .into_iter()
        .next()
        .expect("single input yields one InputGen");
    let prompt_len = input.prompt_tokens.len();
    let rx = input
        .streams
        .into_iter()
        .next()
        .expect("n == 1 yields one stream");

    if stream {
        stream_message(state, rx, prompt_len).await
    } else {
        non_stream_message(state, rx, prompt_len).await
    }
}

/// Buffer the whole generation and render one Anthropic `message` object.
async fn non_stream_message(
    state: Arc<ServerState>,
    rx: tokio::sync::mpsc::Receiver<EngineEvent>,
    prompt_len: usize,
) -> Response {
    let collected = match collect_events(rx).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    // Strip <think> reasoning; surface the content channel as the text block.
    let step = OutputParser::new(state.tool_parser).parse_all(&collected.text);
    let output_tokens = collected.usage.completion_tokens;
    let body = serde_json::json!({
        "id": new_id("msg"),
        "type": "message",
        "role": "assistant",
        "model": state.model_id,
        "content": [{ "type": "text", "text": step.text }],
        "stop_reason": stop_reason(collected.finish_reason),
        "stop_sequence": serde_json::Value::Null,
        "usage": { "input_tokens": prompt_len, "output_tokens": output_tokens },
    });
    Json(body).into_response()
}

/// Render the generation as the Anthropic SSE event sequence.
async fn stream_message(
    state: Arc<ServerState>,
    mut rx: tokio::sync::mpsc::Receiver<EngineEvent>,
    prompt_len: usize,
) -> Response {
    // Commit to a 200 only after the first engine event, so a queue rejection
    // still maps to the right HTTP status (mirrors the OpenAI stream path).
    let first = match rx.recv().await {
        Some(EngineEvent::Error(msg)) => return ApiError::from_engine_error(&msg).into_response(),
        Some(ev) => ev,
        None => {
            return ApiError::internal("engine stream ended without completion").into_response()
        }
    };

    let id = new_id("msg");
    let model = state.model_id.clone();
    let parser_kind = state.tool_parser;
    let (tx, out) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(256);

    tokio::spawn(async move {
        let mut parser = OutputParser::new(parser_kind);
        let mut output_tokens = 0usize;

        // message_start: input tokens known up front (the prompt is tokenized).
        let msg_start = serde_json::json!({
            "type": "message_start",
            "message": {
                "id": id,
                "type": "message",
                "role": "assistant",
                "model": model,
                "content": [],
                "stop_reason": serde_json::Value::Null,
                "stop_sequence": serde_json::Value::Null,
                "usage": { "input_tokens": prompt_len, "output_tokens": 0 },
            }
        });
        if send_event(&tx, "message_start", &msg_start).await.is_err() {
            return;
        }
        let block_start = serde_json::json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": { "type": "text", "text": "" },
        });
        if send_event(&tx, "content_block_start", &block_start)
            .await
            .is_err()
        {
            return;
        }

        let mut stop = FinishReason::Eos;
        let mut pending = Some(first);
        loop {
            let ev = match pending.take() {
                Some(ev) => ev,
                None => match rx.recv().await {
                    Some(ev) => ev,
                    None => break,
                },
            };
            match ev {
                EngineEvent::Token { text, .. } => {
                    output_tokens += 1;
                    let step = parser.push(&text);
                    if !step.text.is_empty()
                        && emit_text_delta(&tx, &step.text).await.is_err()
                    {
                        return;
                    }
                }
                EngineEvent::Done { reason, tokens, .. } => {
                    output_tokens = tokens;
                    stop = reason;
                    // Flush any parser-held tail before closing the block.
                    let step = parser.finish();
                    if !step.text.is_empty()
                        && emit_text_delta(&tx, &step.text).await.is_err()
                    {
                        return;
                    }
                    break;
                }
                EngineEvent::Error(msg) => {
                    // Status already committed; surface in-band as an error event.
                    let body = serde_json::json!({
                        "type": "error",
                        "error": { "type": "api_error", "message": msg },
                    });
                    let _ = send_event(&tx, "error", &body).await;
                    return;
                }
            }
        }

        let block_stop = serde_json::json!({ "type": "content_block_stop", "index": 0 });
        if send_event(&tx, "content_block_stop", &block_stop)
            .await
            .is_err()
        {
            return;
        }
        let msg_delta = serde_json::json!({
            "type": "message_delta",
            "delta": { "stop_reason": stop_reason(stop), "stop_sequence": serde_json::Value::Null },
            "usage": { "output_tokens": output_tokens },
        });
        if send_event(&tx, "message_delta", &msg_delta).await.is_err() {
            return;
        }
        let msg_stop = serde_json::json!({ "type": "message_stop" });
        let _ = send_event(&tx, "message_stop", &msg_stop).await;
    });

    Sse::new(ReceiverStream::new(out))
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Send one named Anthropic SSE event; `Err` means the client hung up.
async fn send_event(
    tx: &tokio::sync::mpsc::Sender<Result<Event, Infallible>>,
    name: &str,
    body: &serde_json::Value,
) -> Result<(), ()> {
    tx.send(Ok(Event::default().event(name).data(body.to_string())))
        .await
        .map_err(|_| ())
}

async fn emit_text_delta(
    tx: &tokio::sync::mpsc::Sender<Result<Event, Infallible>>,
    text: &str,
) -> Result<(), ()> {
    let delta = serde_json::json!({
        "type": "content_block_delta",
        "index": 0,
        "delta": { "type": "text_delta", "text": text },
    });
    send_event(tx, "content_block_delta", &delta).await
}
