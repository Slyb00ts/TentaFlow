// ===== File: routes.rs — axum handlers: chat/completions, models, health, SSE streaming =====

use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Multipart, Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use forge_engine::generate::FinishReason;
use forge_engine::server::{EngineEvent, EngineRequest};
use forge_types::ForgeError;
use forge_tokenize::{ChatMessage, ChatTemplateEngine};
use tokio_stream::wrappers::ReceiverStream;

use crate::api::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatResponseMessage,
    CompletionChoice, CompletionRequest, CompletionResponse, FunctionCallOut, GenerationSpec,
    ModelEntry, ModelList, ToolCallOut, ToolMode, Usage,
};
use crate::error::ApiError;
use crate::toolcall::{OutputParser, ParseStep, ToolParserKind};
use crate::ServerState;

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Monotonic completion ids; wall-clock prefix keeps them unique across
/// restarts without a uuid dependency.
fn new_id(prefix: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{prefix}-{t:x}{n:x}")
}

/// OpenAI-style tool-call ids: "call_" + 8 hex chars, unique per process.
fn new_call_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    // Mix the counter into the high bits so consecutive ids differ even
    // within one clock tick.
    format!("call_{:08x}", (t ^ n.rotate_left(32)) as u32)
}

fn finish_reason_str(r: FinishReason) -> &'static str {
    match r {
        FinishReason::Stop | FinishReason::Eos => "stop",
        FinishReason::Length => "length",
    }
}

pub async fn healthz() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

pub async fn list_models(State(state): State<Arc<ServerState>>) -> Json<ModelList> {
    Json(ModelList {
        object: "list",
        data: vec![ModelEntry {
            id: state.model_id.clone(),
            object: "model",
            created: state.created,
            owned_by: "forge",
        }],
    })
}

/// Bearer-key gate for /v1/*. Constant-time comparison so response timing
/// leaks nothing about key prefixes.
pub async fn require_api_key(
    State(state): State<Arc<ServerState>>,
    req: Request,
    next: Next,
) -> Response {
    let Some(expected) = &state.api_key else {
        return next.run(req).await;
    };
    let provided = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let ok = provided.is_some_and(|p| {
        let (a, b) = (p.as_bytes(), expected.as_bytes());
        a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    });
    if ok {
        next.run(req).await
    } else {
        ApiError::unauthorized().into_response()
    }
}

/// Forward the engine's blocking receiver onto a tokio channel. The loop
/// polls with a timeout and watches the tokio sender: when the HTTP client
/// disconnects while the request is still queued or prefilling (no events
/// flowing yet), the std receiver is dropped promptly and the engine's
/// send-failure path cancels the sequence. The semaphore permit rides along
/// so a bridged request occupies exactly one admission slot until it ends.
fn bridge_events(
    rx: std::sync::mpsc::Receiver<EngineEvent>,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> tokio::sync::mpsc::Receiver<EngineEvent> {
    let (tx, out) = tokio::sync::mpsc::channel(256);
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        loop {
            if tx.is_closed() {
                return; // client hung up; dropping rx cancels the engine seq
            }
            match rx.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(ev) => {
                    let terminal = matches!(ev, EngineEvent::Done { .. } | EngineEvent::Error(_));
                    if tx.blocking_send(ev).is_err() || terminal {
                        return;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    });
    out
}

/// Rendered chat prompt for a request. Multipart text parts are flattened to
/// plain string content so any HF template's `message['content'] + ...`
/// works; all other message fields (name, tool_calls, ...) pass through.
fn render_chat_prompt(
    chat_template: &str,
    template_vars: &serde_json::Map<String, serde_json::Value>,
    messages: &[ChatMessage],
    tools: Option<&serde_json::Value>,
) -> Result<String, ApiError> {
    let flattened: Vec<ChatMessage> = messages
        .iter()
        .map(|m| {
            let mut m = m.clone();
            // Content-less assistant tool-call turns flatten to "" so
            // templates that concatenate content never see undefined.
            m.content = Some(serde_json::Value::String(
                m.text_content().unwrap_or_default(),
            ));
            m
        })
        .collect();
    ChatTemplateEngine::new()
        .render(chat_template, &flattened, tools, true, false, template_vars)
        .map_err(|e| ApiError::invalid_request(format!("chat template render failed: {e}")))
}

enum GenInput {
    Chat(Vec<ChatMessage>, Option<serde_json::Value>),
    Text(String),
}

/// Admit one generation: take an admission slot (429 when the queue is
/// full), then render/tokenize/submit on the blocking pool — template
/// rendering and tokenization of large prompts must not stall the async
/// workers.
async fn start_generation(
    state: &Arc<ServerState>,
    input: GenInput,
    spec: GenerationSpec,
) -> Result<tokio::sync::mpsc::Receiver<EngineEvent>, ApiError> {
    let permit = state
        .slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::overloaded("too many concurrent requests, retry shortly"))?;

    let st = state.clone();
    let rx = tokio::task::spawn_blocking(move || {
        let prompt = match &input {
            GenInput::Chat(messages, tools) => {
                render_chat_prompt(&st.chat_template, &st.template_vars, messages, tools.as_ref())?
            }
            GenInput::Text(s) => s.clone(),
        };
        let prompt_tokens = st
            .tokenizer
            .encode(&prompt, true)
            .map_err(|e| ApiError::internal(format!("tokenization failed: {e}")))?;
        crate::api::check_context(prompt_tokens.len(), spec.max_tokens, st.max_context)?;
        st.engine
            .submit(EngineRequest {
                prompt_tokens,
                max_tokens: spec.max_tokens,
                sampling: spec.sampling.clone(),
                stop: spec.stop.clone(),
                eos_ids: st.eos_ids.clone(),
            })
            .map_err(|e| ApiError::internal(e.to_string()))
    })
    .await
    .map_err(|e| ApiError::internal(format!("request preparation failed: {e}")))??;

    Ok(bridge_events(rx, permit))
}

struct Collected {
    text: String,
    finish: &'static str,
    usage: Usage,
}

async fn collect_events(
    mut rx: tokio::sync::mpsc::Receiver<EngineEvent>,
) -> Result<Collected, ApiError> {
    let mut text = String::new();
    while let Some(ev) = rx.recv().await {
        match ev {
            EngineEvent::Token { text: piece, .. } => text.push_str(&piece),
            EngineEvent::Done {
                reason,
                tokens,
                prompt_tokens,
            } => {
                return Ok(Collected {
                    text,
                    finish: finish_reason_str(reason),
                    usage: Usage::new(prompt_tokens, tokens),
                })
            }
            EngineEvent::Error(msg) => return Err(ApiError::from_engine_error(&msg)),
        }
    }
    Err(ApiError::internal("engine stream ended without completion"))
}

/// Streaming payload shape differences between the two endpoints.
#[derive(Clone, Copy)]
enum StreamKind {
    Chat,
    Text,
}

impl StreamKind {
    fn object(self) -> &'static str {
        match self {
            StreamKind::Chat => "chat.completion.chunk",
            StreamKind::Text => "text_completion",
        }
    }

    fn text_choice(self, text: Option<&str>, finish: Option<&str>) -> serde_json::Value {
        serde_json::json!({
            "index": 0, "text": text.unwrap_or(""), "finish_reason": finish
        })
    }
}

/// Per-response state of a streaming chat: the output parser plus the
/// OpenAI delta conventions (role on the first delta only, tool-call
/// indices, "tool_calls" finish reason once any call was emitted).
struct ChatStream {
    parser: OutputParser,
    first: bool,
    tool_index: u32,
    any_calls: bool,
}

impl ChatStream {
    fn new(kind: ToolParserKind) -> Self {
        Self {
            parser: OutputParser::new(kind),
            first: true,
            tool_index: 0,
            any_calls: false,
        }
    }

    fn delta_choice(
        &mut self,
        mut delta: serde_json::Map<String, serde_json::Value>,
        finish: Option<&str>,
    ) -> serde_json::Value {
        if self.first {
            delta.insert("role".into(), "assistant".into());
            self.first = false;
        }
        serde_json::json!({ "index": 0, "delta": delta, "finish_reason": finish })
    }

    /// One delta choice per surfaced item: reasoning, content, then each
    /// completed tool call as a single full-function delta at its index.
    fn choices_for(&mut self, step: ParseStep) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        if !step.reasoning.is_empty() {
            let mut d = serde_json::Map::new();
            d.insert("reasoning_content".into(), step.reasoning.into());
            out.push(self.delta_choice(d, None));
        }
        if !step.text.is_empty() {
            let mut d = serde_json::Map::new();
            d.insert("content".into(), step.text.into());
            out.push(self.delta_choice(d, None));
        }
        for call in step.calls {
            let tc = serde_json::json!([{
                "index": self.tool_index,
                "id": new_call_id(),
                "type": "function",
                "function": { "name": call.name, "arguments": call.arguments },
            }]);
            self.tool_index += 1;
            self.any_calls = true;
            let mut d = serde_json::Map::new();
            d.insert("tool_calls".into(), tc);
            out.push(self.delta_choice(d, None));
        }
        out
    }

    fn finish_choice(&mut self, engine_reason: &'static str) -> serde_json::Value {
        // "length" must stay visible even when calls were parsed: a truncated
        // sequence may have produced incomplete or missing calls.
        let reason = if self.any_calls && engine_reason == "stop" {
            "tool_calls"
        } else {
            engine_reason
        };
        self.delta_choice(serde_json::Map::new(), Some(reason))
    }
}

fn sse_chunk(
    kind: StreamKind,
    id: &str,
    created: u64,
    model: &str,
    choices: Vec<serde_json::Value>,
    usage: Option<Usage>,
) -> Event {
    let mut body = serde_json::json!({
        "id": id,
        "object": kind.object(),
        "created": created,
        "model": model,
        "choices": choices,
    });
    if let Some(u) = usage {
        body["usage"] = serde_json::to_value(u).expect("usage serializes");
    }
    Event::default().data(body.to_string())
}

/// Run one generation as an SSE response. The first engine event is awaited
/// before committing to a 200, so queue rejections still map to proper HTTP
/// status codes (429 for KV-page pressure). With `include_usage`
/// (stream_options), usage arrives as a separate final chunk with an empty
/// `choices` array, matching OpenAI's shape.
async fn stream_response(
    state: Arc<ServerState>,
    mut rx: tokio::sync::mpsc::Receiver<EngineEvent>,
    kind: StreamKind,
    id: String,
    include_usage: bool,
    chat: Option<ChatStream>,
) -> Response {
    let first = match rx.recv().await {
        Some(EngineEvent::Error(msg)) => return ApiError::from_engine_error(&msg).into_response(),
        Some(ev) => ev,
        None => {
            return ApiError::internal("engine stream ended without completion").into_response()
        }
    };

    let created = now_unix();
    let model = state.model_id.clone();
    let (tx, out) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(256);
    tokio::spawn(async move {
        let mut pending = Some(first);
        let mut chat = chat;
        loop {
            let ev = match pending.take() {
                Some(ev) => ev,
                None => match rx.recv().await {
                    Some(ev) => ev,
                    None => break,
                },
            };
            let done = match ev {
                EngineEvent::Token { text, .. } => {
                    let choices = match chat.as_mut() {
                        Some(cs) => {
                            let step = cs.parser.push(&text);
                            cs.choices_for(step)
                        }
                        None => vec![kind.text_choice(Some(&text), None)],
                    };
                    // One delta per chunk, matching OpenAI's stream shape.
                    for choice in choices {
                        let chunk = sse_chunk(kind, &id, created, &model, vec![choice], None);
                        if tx.send(Ok(chunk)).await.is_err() {
                            return; // client hung up; dropping rx cancels the engine seq
                        }
                    }
                    false
                }
                EngineEvent::Done {
                    reason,
                    tokens,
                    prompt_tokens,
                } => {
                    let engine_reason = finish_reason_str(reason);
                    let mut choices = Vec::new();
                    match chat.as_mut() {
                        Some(cs) => {
                            // Flush held-back parser state before finishing.
                            let step = cs.parser.finish();
                            choices.extend(cs.choices_for(step));
                            choices.push(cs.finish_choice(engine_reason));
                        }
                        None => choices.push(kind.text_choice(None, Some(engine_reason))),
                    }
                    for choice in choices {
                        let chunk = sse_chunk(kind, &id, created, &model, vec![choice], None);
                        if tx.send(Ok(chunk)).await.is_err() {
                            return;
                        }
                    }
                    if include_usage {
                        let usage_chunk = sse_chunk(
                            kind,
                            &id,
                            created,
                            &model,
                            vec![],
                            Some(Usage::new(prompt_tokens, tokens)),
                        );
                        if tx.send(Ok(usage_chunk)).await.is_err() {
                            return;
                        }
                    }
                    true
                }
                EngineEvent::Error(msg) => {
                    // The HTTP status is already committed; surface the error
                    // in-band the way OpenAI's API does.
                    let body = serde_json::json!({
                        "error": { "message": msg, "type": "server_error", "code": null }
                    });
                    let _ = tx.send(Ok(Event::default().data(body.to_string()))).await;
                    true
                }
            };
            if done {
                let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
                return;
            }
        }
    });

    Sse::new(ReceiverStream::new(out))
        .keep_alive(KeepAlive::default())
        .into_response()
}

pub async fn chat_completions(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<ChatCompletionRequest>,
) -> Response {
    if req.model != state.model_id {
        return ApiError::model_not_found(&req.model, &state.model_id).into_response();
    }
    let spec = match req.generation_spec() {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let tool_mode = match req.tool_mode() {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };
    // tool_choice "none" (or no tools at all): tools are neither rendered
    // into the template nor parsed out of the output.
    let (parser_kind, tools) = match tool_mode {
        ToolMode::Auto => (state.tool_parser, req.tools.clone()),
        ToolMode::None => (ToolParserKind::None, None),
    };
    let rx = match start_generation(&state, GenInput::Chat(req.messages, tools), spec).await {
        Ok(rx) => rx,
        Err(e) => return e.into_response(),
    };

    if req.stream {
        let include_usage = req.stream_options.is_some_and(|o| o.include_usage);
        return stream_response(
            state,
            rx,
            StreamKind::Chat,
            new_id("chatcmpl"),
            include_usage,
            Some(ChatStream::new(parser_kind)),
        )
        .await;
    }

    match collect_events(rx).await {
        Ok(c) => {
            let step = OutputParser::new(parser_kind).parse_all(&c.text);
            let tool_calls: Vec<ToolCallOut> = step
                .calls
                .into_iter()
                .map(|call| ToolCallOut {
                    id: new_call_id(),
                    call_type: "function",
                    function: FunctionCallOut {
                        name: call.name,
                        arguments: call.arguments,
                    },
                })
                .collect();
            // "length" stays visible even with parsed calls (truncation).
            let finish = if !tool_calls.is_empty() && c.finish == "stop" {
                "tool_calls"
            } else {
                c.finish
            };
            // With tool calls, leftover inter-marker whitespace is noise:
            // trim it and null out an empty content, per the OpenAI shape.
            let content = if tool_calls.is_empty() {
                Some(step.text)
            } else {
                let trimmed = step.text.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_string())
            };
            Json(ChatCompletionResponse {
                id: new_id("chatcmpl"),
                object: "chat.completion",
                created: now_unix(),
                model: state.model_id.clone(),
                choices: vec![ChatChoice {
                    index: 0,
                    message: ChatResponseMessage {
                        role: "assistant",
                        content,
                        reasoning_content: (!step.reasoning.is_empty()).then_some(step.reasoning),
                        tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
                    },
                    finish_reason: finish,
                }],
                usage: c.usage,
            })
            .into_response()
        }
        Err(e) => e.into_response(),
    }
}

pub async fn completions(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<CompletionRequest>,
) -> Response {
    if req.model != state.model_id {
        return ApiError::model_not_found(&req.model, &state.model_id).into_response();
    }
    let spec = match req.generation_spec() {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let prompt = match req.single_prompt() {
        Ok(p) => p.to_string(),
        Err(e) => return e.into_response(),
    };
    let rx = match start_generation(&state, GenInput::Text(prompt), spec).await {
        Ok(rx) => rx,
        Err(e) => return e.into_response(),
    };

    if req.stream {
        let include_usage = req.stream_options.is_some_and(|o| o.include_usage);
        return stream_response(
            state,
            rx,
            StreamKind::Text,
            new_id("cmpl"),
            include_usage,
            None,
        )
        .await;
    }

    match collect_events(rx).await {
        Ok(c) => Json(CompletionResponse {
            id: new_id("cmpl"),
            object: "text_completion",
            created: now_unix(),
            model: state.model_id.clone(),
            choices: vec![CompletionChoice {
                index: 0,
                text: c.text,
                finish_reason: c.finish,
            }],
            usage: c.usage,
        })
        .into_response(),
        Err(e) => e.into_response(),
    }
}

/// POST /v1/audio/transcriptions — OpenAI-compatible speech-to-text.
/// Multipart form: `file` (WAV bytes, required), `language` (optional ISO
/// code), `model` (accepted for API compatibility; this server hosts exactly
/// one Whisper model). Returns `{"text": "..."}`. 404 when the server was
/// started without `--whisper-model`.
pub async fn audio_transcriptions(
    State(state): State<Arc<ServerState>>,
    mut multipart: Multipart,
) -> Response {
    let Some(whisper) = state.whisper.clone() else {
        return ApiError::not_found(
            "no speech-to-text model is configured; start the server with --whisper-model",
        )
        .into_response();
    };
    // Admission before the body is read: every admitted request may hold a
    // ≤64 MiB upload while transcriptions run one at a time, so excess load
    // is rejected up front instead of buffered.
    let Ok(_permit) = state.stt_slots.try_acquire() else {
        return ApiError::overloaded("too many concurrent transcription requests")
            .into_response();
    };

    let mut file: Option<Vec<u8>> = None;
    let mut language: Option<String> = None;
    loop {
        match multipart.next_field().await {
            Ok(Some(field)) => match field.name().unwrap_or("") {
                "file" => match field.bytes().await {
                    Ok(b) => file = Some(b.to_vec()),
                    Err(e) => {
                        return ApiError::invalid_request(format!("reading `file` part: {e}"))
                            .into_response()
                    }
                },
                "language" => match field.text().await {
                    Ok(t) if !t.trim().is_empty() => language = Some(t.trim().to_string()),
                    Ok(_) => {}
                    Err(e) => {
                        return ApiError::invalid_request(format!("reading `language` part: {e}"))
                            .into_response()
                    }
                },
                // `model`, `response_format`, `temperature`, … accepted and
                // ignored: one model per server, JSON response only.
                _ => {}
            },
            Ok(None) => break,
            Err(e) => {
                return ApiError::invalid_request(format!("malformed multipart body: {e}"))
                    .into_response()
            }
        }
    }
    let Some(bytes) = file else {
        return ApiError::invalid_request("missing `file` part").into_response();
    };

    // One transcription at a time: the owned guard rides into spawn_blocking
    // so the GPU work never blocks a tokio worker.
    let guard = whisper.lock_owned().await;
    let joined = tokio::task::spawn_blocking(move || {
        let mut model = guard;
        let samples = forge_whisper::audio::decode_wav_bytes(&bytes)?;
        model.transcribe(&samples, language.as_deref())
    })
    .await;
    match joined {
        Ok(Ok(text)) => Json(serde_json::json!({ "text": text })).into_response(),
        Ok(Err(e @ (ForgeError::Format(_) | ForgeError::Unsupported(_)))) => {
            ApiError::invalid_request(e.to_string()).into_response()
        }
        Ok(Err(e)) => ApiError::internal(format!("transcription failed: {e}")).into_response(),
        Err(e) => ApiError::internal(format!("transcription task failed: {e}")).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_call_round_trip_renders() {
        // A replayed tool-calling exchange (assistant content:null +
        // tool_calls, then the tool result) must render through a
        // tool-aware template without errors or undefined leaks.
        let template = forge_tokenize::builtin_chat_template("qwen").unwrap();
        let messages: Vec<ChatMessage> = serde_json::from_value(serde_json::json!([
            {"role": "user", "content": "Jaka jest pogoda w Krakowie?"},
            {"role": "assistant", "content": null, "tool_calls": [{
                "id": "call_0badc0de", "type": "function",
                "function": {"name": "get_weather", "arguments": "{\"city\":\"Kraków\"}"}
            }]},
            {"role": "tool", "tool_call_id": "call_0badc0de", "content": "12°C, słonecznie"}
        ]))
        .unwrap();
        let tools = serde_json::json!([{
            "type": "function",
            "function": {"name": "get_weather", "parameters": {"type": "object"}}
        }]);

        let out = render_chat_prompt(
            template,
            &serde_json::Map::new(),
            &messages,
            Some(&tools),
        )
        .unwrap();
        assert!(out.contains("<tool_call>"), "assistant call missing: {out}");
        assert!(out.contains("get_weather"));
        assert!(out.contains("<tool_response>\n12°C, słonecznie"));
        assert!(!out.contains("undefined"), "undefined leaked: {out}");
    }
}
