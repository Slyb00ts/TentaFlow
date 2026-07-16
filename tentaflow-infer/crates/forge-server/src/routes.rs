// ===== File: routes.rs — axum handlers: chat/completions, models, health, SSE streaming =====

use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use forge_engine::generate::FinishReason;
use forge_engine::server::{EngineEvent, EngineRequest};
use forge_tokenize::{ChatMessage, ChatTemplateEngine};
use tokio_stream::wrappers::ReceiverStream;

use crate::api::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatResponseMessage,
    CompletionChoice, CompletionRequest, CompletionResponse, GenerationSpec, ModelEntry, ModelList,
    Usage,
};
use crate::error::ApiError;
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
fn render_chat_prompt(state: &ServerState, messages: &[ChatMessage]) -> Result<String, ApiError> {
    let flattened: Vec<ChatMessage> = messages
        .iter()
        .map(|m| {
            let mut m = m.clone();
            m.content = Some(serde_json::Value::String(
                m.text_content().unwrap_or_default(),
            ));
            m
        })
        .collect();
    ChatTemplateEngine::new()
        .render(
            &state.chat_template,
            &flattened,
            None,
            true,
            false,
            &state.template_vars,
        )
        .map_err(|e| ApiError::invalid_request(format!("chat template render failed: {e}")))
}

enum GenInput {
    Chat(Vec<ChatMessage>),
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
            GenInput::Chat(messages) => render_chat_prompt(&st, messages)?,
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

    fn choice(self, text: Option<&str>, first: bool, finish: Option<&str>) -> serde_json::Value {
        match self {
            StreamKind::Chat => {
                let mut delta = serde_json::Map::new();
                // OpenAI emits the assistant role on the first delta only.
                if first {
                    delta.insert("role".into(), "assistant".into());
                }
                if let Some(t) = text {
                    delta.insert("content".into(), t.into());
                }
                serde_json::json!({ "index": 0, "delta": delta, "finish_reason": finish })
            }
            StreamKind::Text => serde_json::json!({
                "index": 0, "text": text.unwrap_or(""), "finish_reason": finish
            }),
        }
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
        let mut first_chunk = true;
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
                    let choice = kind.choice(Some(&text), first_chunk, None);
                    first_chunk = false;
                    let chunk = sse_chunk(kind, &id, created, &model, vec![choice], None);
                    if tx.send(Ok(chunk)).await.is_err() {
                        return; // client hung up; dropping rx cancels the engine seq
                    }
                    false
                }
                EngineEvent::Done {
                    reason,
                    tokens,
                    prompt_tokens,
                } => {
                    let choice = kind.choice(None, first_chunk, Some(finish_reason_str(reason)));
                    let chunk = sse_chunk(kind, &id, created, &model, vec![choice], None);
                    if tx.send(Ok(chunk)).await.is_err() {
                        return;
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
    let rx = match start_generation(&state, GenInput::Chat(req.messages), spec).await {
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
        )
        .await;
    }

    match collect_events(rx).await {
        Ok(c) => Json(ChatCompletionResponse {
            id: new_id("chatcmpl"),
            object: "chat.completion",
            created: now_unix(),
            model: state.model_id.clone(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatResponseMessage {
                    role: "assistant",
                    content: c.text,
                },
                finish_reason: c.finish,
            }],
            usage: c.usage,
        })
        .into_response(),
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
        return stream_response(state, rx, StreamKind::Text, new_id("cmpl"), include_usage).await;
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
