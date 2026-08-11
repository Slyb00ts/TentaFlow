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
use forge_tokenize::{ChatMessage, ChatTemplateEngine};
use forge_types::ForgeError;
use tokio_stream::wrappers::ReceiverStream;

use std::collections::BTreeMap;

use crate::api::{
    base64_encode, ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatLogprobEntry,
    ChatLogprobs, ChatResponseMessage, CompletionChoice, CompletionLogprobs, CompletionRequest,
    CompletionResponse, EmbItem, EmbeddingData, EmbeddingUsage, EmbeddingVec, EmbeddingsRequest,
    EmbeddingsResponse, EncodingFormat, FunctionCallOut, GenerationSpec, ModelEntry, ModelList,
    ToolCallOut, ToolMode, TopLogprobEntry, Usage,
};
use crate::error::ApiError;
use crate::toolcall::{OutputParser, ParseStep, ToolParserKind};
use crate::ServerState;
use forge_engine::sample::TokenLogprob;
use forge_tokenize::Tokenizer;

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Monotonic completion ids; wall-clock prefix keeps them unique across
/// restarts without a uuid dependency.
pub(crate) fn new_id(prefix: &str) -> String {
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

/// GET /metrics — Prometheus text exposition (SPEC §8.3). Served outside the
/// API-key gate like /healthz. Renders the engine's live counters/gauges/
/// histograms plus the per-route HTTP request counts.
pub async fn metrics_endpoint(State(state): State<Arc<ServerState>>) -> Response {
    let body = crate::metrics::render(state.engine.metrics(), &state.http_metrics, &state.model_id);
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
        .into_response()
}

/// Records the matched-route template + final status of every request into
/// `HttpMetrics` after the handler runs. Uses the route template (not the raw
/// path) so cardinality stays bounded.
pub async fn record_http_metrics(
    State(state): State<Arc<ServerState>>,
    req: Request,
    next: Next,
) -> Response {
    let route = req
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| req.uri().path().to_string());
    let resp = next.run(req).await;
    state.http_metrics.record(&route, resp.status().as_u16());
    resp
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

pub(crate) enum GenInput {
    /// Wiadomości, narzędzia i zmienne szablonu — komplet tego, co czyta jinja.
    Chat(
        Vec<ChatMessage>,
        Option<serde_json::Value>,
        Option<serde_json::Map<String, serde_json::Value>>,
    ),
    Text(String),
    /// Pre-tokenized prompt (completions `prompt` given as token ids).
    Tokens(Vec<u32>),
}

/// Derive a distinct-but-deterministic seed for completion `i` of an `n`-way
/// request: index 0 keeps the caller's seed (so `n == 1` is unchanged), later
/// indices are offset by a fixed odd multiplier so the completions differ yet
/// reproduce across runs. With no caller seed, each completion stays independent
/// (time-derived inside the engine).
fn seed_for(base: Option<u64>, i: usize) -> Option<u64> {
    base.map(|s| s.wrapping_add((i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)))
}

/// The admitted streams + tokenized prompt for ONE input prompt: `spec.n`
/// bridged event streams (one per completion) and the prompt tokens (kept for
/// `echo`).
pub(crate) struct InputGen {
    pub(crate) streams: Vec<tokio::sync::mpsc::Receiver<EngineEvent>>,
    pub(crate) prompt_tokens: Vec<u32>,
}

/// One admitted generation across one or more input prompts.
pub(crate) struct Generation {
    pub(crate) per_input: Vec<InputGen>,
}

/// Admit `inputs.len() × spec.n` completions: take that many admission slots
/// (429 when the queue is full), then render/tokenize each prompt once and
/// submit one engine request per (prompt, completion). ALL requests are
/// submitted before any is awaited, so the scheduler admits them into the same
/// decode batch. Each completion samples from a distinct per-index seed and
/// shares a prompt prefix through the engine's radix prefix cache.
pub(crate) async fn start_generation(
    state: &Arc<ServerState>,
    inputs: Vec<GenInput>,
    spec: GenerationSpec,
    grammar: Option<forge_grammar::GrammarProgram>,
) -> Result<Generation, ApiError> {
    let total = inputs.len().saturating_mul(spec.n);
    // One admission slot per (prompt, completion); release everything on
    // partial failure (the permits drop when this Vec drops on an error path).
    let mut permits = Vec::with_capacity(total);
    for _ in 0..total {
        match state.slots.clone().try_acquire_owned() {
            Ok(p) => permits.push(p),
            Err(_) => {
                return Err(ApiError::overloaded(
                    "too many concurrent requests, retry shortly",
                ))
            }
        }
    }

    let st = state.clone();
    let raw: Vec<(Vec<std::sync::mpsc::Receiver<EngineEvent>>, Vec<u32>)> =
        tokio::task::spawn_blocking(move || {
            let mut per_input = Vec::with_capacity(inputs.len());
            for input in &inputs {
                let prompt_tokens = match input {
                    GenInput::Chat(messages, tools, kwargs) => {
                        let mut vars = st.template_vars.clone();
                        // Żądanie dokłada swoje zmienne i wygrywa z domyślnymi:
                        // `bos_token`/`eos_token` opisują model, a te opisują tę
                        // jedną rozmowę.
                        if let Some(kwargs) = kwargs {
                            vars.extend(kwargs.clone());
                        }
                        let prompt =
                            render_chat_prompt(&st.chat_template, &vars, messages, tools.as_ref())?;
                        st.tokenizer
                            .encode(&prompt, true)
                            .map_err(|e| ApiError::internal(format!("tokenization failed: {e}")))?
                    }
                    GenInput::Text(s) => st
                        .tokenizer
                        .encode(s, true)
                        .map_err(|e| ApiError::internal(format!("tokenization failed: {e}")))?,
                    GenInput::Tokens(ids) => ids.clone(),
                };
                if prompt_tokens.is_empty() {
                    return Err(ApiError::invalid_request("a prompt produced zero tokens"));
                }
                crate::api::check_context(prompt_tokens.len(), spec.max_tokens, st.max_context)?;
                let mut streams = Vec::with_capacity(spec.n);
                for i in 0..spec.n {
                    let mut sampling = spec.sampling.clone();
                    sampling.seed = seed_for(spec.sampling.seed, i);
                    let rx = st
                        .engine
                        .submit(EngineRequest {
                            prompt_tokens: prompt_tokens.clone(),
                            max_tokens: spec.max_tokens,
                            sampling,
                            stop: spec.stop.clone(),
                            eos_ids: st.eos_ids.clone(),
                            grammar: grammar.clone(),
                            logit_bias: spec.logit_bias.clone(),
                            min_tokens: spec.min_tokens,
                            logprobs: spec.logprobs,
                            emit_empty_tokens: false,
                        })
                        .map_err(|e| ApiError::internal(e.to_string()))?;
                    streams.push(rx);
                }
                per_input.push((streams, prompt_tokens));
            }
            Ok::<_, ApiError>(per_input)
        })
        .await
        .map_err(|e| ApiError::internal(format!("request preparation failed: {e}")))??;

    // Bridge every raw std receiver onto tokio, handing each its admission
    // permit (permits are consumed in submission order across all inputs).
    let mut permits = permits.into_iter();
    let per_input = raw
        .into_iter()
        .map(|(streams, prompt_tokens)| InputGen {
            streams: streams
                .into_iter()
                .map(|rx| {
                    let permit = permits.next().expect("one permit per submitted stream");
                    bridge_events(rx, permit)
                })
                .collect(),
            prompt_tokens,
        })
        .collect();
    Ok(Generation { per_input })
}

pub(crate) struct Collected {
    pub(crate) text: String,
    pub(crate) finish: &'static str,
    /// Raw engine reason, so the Anthropic layer can distinguish EOS (end_turn)
    /// from a matched stop sequence (stop_sequence) — the OpenAI `finish` string
    /// collapses both to "stop".
    pub(crate) finish_reason: FinishReason,
    pub(crate) usage: Usage,
    /// Per-token log-probability reports (empty unless `logprobs` was set).
    pub(crate) logprobs: Vec<forge_engine::sample::TokenLogprob>,
}

pub(crate) async fn collect_events(
    mut rx: tokio::sync::mpsc::Receiver<EngineEvent>,
) -> Result<Collected, ApiError> {
    let mut text = String::new();
    let mut logprobs = Vec::new();
    while let Some(ev) = rx.recv().await {
        match ev {
            EngineEvent::Token {
                text: piece,
                logprob,
                ..
            } => {
                text.push_str(&piece);
                if let Some(lp) = logprob {
                    logprobs.push(lp);
                }
            }
            EngineEvent::Done {
                reason,
                tokens,
                prompt_tokens,
                cache_read_tokens,
                ..
            } => {
                return Ok(Collected {
                    text,
                    finish: finish_reason_str(reason),
                    finish_reason: reason,
                    usage: Usage::with_cache(prompt_tokens, tokens, cache_read_tokens),
                    logprobs,
                })
            }
            EngineEvent::Error(msg) => return Err(ApiError::from_engine_error(&msg)),
        }
    }
    Err(ApiError::internal("engine stream ended without completion"))
}

/// Decode a single token id to its display string (byte-level pieces may
/// render as replacement chars for a partial UTF-8 token; the `bytes` field
/// carries the exact bytes so clients can reassemble).
fn decode_token(tok: &Tokenizer, id: u32) -> String {
    tok.decode(&[id], false).unwrap_or_default()
}

/// Build the chat `logprobs.content[]` from the engine's per-token reports.
fn chat_logprobs(tok: &Tokenizer, gen: &[TokenLogprob]) -> ChatLogprobs {
    let content = gen
        .iter()
        .map(|lp| {
            let token = decode_token(tok, lp.token);
            let top_logprobs = lp
                .top
                .iter()
                .map(|&(id, v)| {
                    let t = decode_token(tok, id);
                    TopLogprobEntry {
                        bytes: t.as_bytes().to_vec(),
                        token: t,
                        logprob: v,
                    }
                })
                .collect();
            ChatLogprobEntry {
                bytes: token.as_bytes().to_vec(),
                token,
                logprob: lp.logprob,
                top_logprobs,
            }
        })
        .collect();
    ChatLogprobs { content }
}

/// Build the completions legacy `logprobs` object. `echo_prompt`, when set,
/// prepends the prompt tokens (with `null` conditional log-probabilities — the
/// prefill positions are not scored). `text_offset` accumulates the decoded
/// character length of each token.
fn completion_logprobs(
    tok: &Tokenizer,
    echo_prompt: Option<&[u32]>,
    gen: &[TokenLogprob],
) -> CompletionLogprobs {
    let mut tokens = Vec::new();
    let mut token_logprobs = Vec::new();
    let mut top_logprobs = Vec::new();
    let mut text_offset = Vec::new();
    let mut offset = 0usize;
    if let Some(prompt) = echo_prompt {
        for &id in prompt {
            let s = decode_token(tok, id);
            text_offset.push(offset);
            offset += s.chars().count();
            tokens.push(s);
            token_logprobs.push(None);
            top_logprobs.push(BTreeMap::new());
        }
    }
    for lp in gen {
        let s = decode_token(tok, lp.token);
        text_offset.push(offset);
        offset += s.chars().count();
        tokens.push(s);
        token_logprobs.push(Some(lp.logprob));
        let mut top = BTreeMap::new();
        for &(id, v) in &lp.top {
            top.insert(decode_token(tok, id), v);
        }
        top_logprobs.push(top);
    }
    CompletionLogprobs {
        tokens,
        token_logprobs,
        top_logprobs,
        text_offset,
    }
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
                    cache_read_tokens,
                    ..
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
                            Some(Usage::with_cache(prompt_tokens, tokens, cache_read_tokens)),
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
    let spec = match req.generation_spec_with(&state.default_sampling, &state.default_stop) {
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
    // Constrained decoding (SPEC §8.1.2): response_format / grammar / forced
    // tool_choice compile to a grammar the sampler must obey.
    let grammar = match state.grammar.resolve(&req, parser_kind) {
        Ok(g) => g,
        Err(e) => return e.into_response(),
    };
    let want_logprobs = spec.logprobs.is_some();
    // Streaming a multi-completion response is not supported (the OpenAI stream
    // shape interleaves choices ambiguously); ask for a non-streaming request.
    if req.stream && spec.n > 1 {
        return ApiError::invalid_request("streaming is not supported with n > 1").into_response();
    }
    let gen = match start_generation(
        &state,
        vec![GenInput::Chat(
            req.messages,
            tools,
            req.chat_template_kwargs,
        )],
        spec,
        grammar,
    )
    .await
    {
        Ok(g) => g,
        Err(e) => return e.into_response(),
    };
    let mut streams = gen
        .per_input
        .into_iter()
        .next()
        .expect("single chat input yields one InputGen")
        .streams;

    if req.stream {
        let rx = streams.pop().expect("n >= 1 yields at least one stream");
        let include_usage = req.stream_options.is_some_and(|o| o.include_usage);
        return stream_response(
            state.clone(),
            rx,
            StreamKind::Chat,
            new_id("chatcmpl"),
            include_usage,
            Some(ChatStream::new(parser_kind)),
        )
        .await;
    }

    // Non-streaming: collect every completion, then assemble `choices[0..n]`.
    let mut collected = Vec::with_capacity(streams.len());
    for rx in streams {
        match collect_events(rx).await {
            Ok(c) => collected.push(c),
            Err(e) => return e.into_response(),
        }
    }

    let mut choices = Vec::with_capacity(collected.len());
    let mut prompt_tokens = 0usize;
    let mut cached_tokens = 0usize;
    let mut completion_tokens = 0usize;
    for (index, c) in collected.into_iter().enumerate() {
        prompt_tokens = c.usage.prompt_tokens;
        cached_tokens = c
            .usage
            .prompt_tokens_details
            .as_ref()
            .map(|d| d.cached_tokens)
            .unwrap_or(0);
        completion_tokens += c.usage.completion_tokens;
        let logprobs = want_logprobs.then(|| chat_logprobs(&state.tokenizer, &c.logprobs));
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
        // With tool calls, leftover inter-marker whitespace is noise: trim it
        // and null out an empty content, per the OpenAI shape.
        let content = if tool_calls.is_empty() {
            Some(step.text)
        } else {
            let trimmed = step.text.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        };
        choices.push(ChatChoice {
            index: index as u32,
            message: ChatResponseMessage {
                role: "assistant",
                content,
                reasoning_content: (!step.reasoning.is_empty()).then_some(step.reasoning),
                tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
            },
            logprobs,
            finish_reason: finish,
        });
    }

    Json(ChatCompletionResponse {
        id: new_id("chatcmpl"),
        object: "chat.completion",
        created: now_unix(),
        model: state.model_id.clone(),
        choices,
        usage: Usage::with_cache(prompt_tokens, completion_tokens, cached_tokens),
    })
    .into_response()
}

pub async fn completions(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<CompletionRequest>,
) -> Response {
    if req.model != state.model_id {
        return ApiError::model_not_found(&req.model, &state.model_id).into_response();
    }
    let spec = match req.generation_spec_with(&state.default_sampling, &state.default_stop) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let items = req.prompt.clone().into_items();
    if items.is_empty() {
        return ApiError::invalid_request("`prompt` must not be empty").into_response();
    }
    let echo = spec.echo;
    // Per-input echo text (the prompt itself): a raw string prepends verbatim; a
    // pre-tokenized prompt decodes back through the tokenizer.
    let echo_texts: Vec<String> = if echo {
        items
            .iter()
            .map(|it| match it {
                crate::api::PromptItem::Text(s) => s.clone(),
                crate::api::PromptItem::Tokens(ids) => {
                    state.tokenizer.decode(ids, false).unwrap_or_default()
                }
            })
            .collect()
    } else {
        Vec::new()
    };
    let want_logprobs = spec.logprobs.is_some();
    // Total completions across all prompts; streaming only supports exactly one.
    let total_completions = items.len().saturating_mul(spec.n);
    if req.stream && total_completions > 1 {
        return ApiError::invalid_request(
            "streaming is not supported with multiple prompts or n > 1",
        )
        .into_response();
    }
    // echo/logprobs reshape the response body, so they only apply to the
    // buffered (non-streaming) path; a streaming request keeps the raw deltas.
    if req.stream && (echo || want_logprobs) {
        return ApiError::invalid_request(
            "echo and logprobs are only supported for non-streaming completions",
        )
        .into_response();
    }
    let inputs: Vec<GenInput> = items
        .into_iter()
        .map(|it| match it {
            crate::api::PromptItem::Text(s) => GenInput::Text(s),
            crate::api::PromptItem::Tokens(ids) => GenInput::Tokens(ids),
        })
        .collect();
    let gen = match start_generation(&state, inputs, spec, None).await {
        Ok(g) => g,
        Err(e) => return e.into_response(),
    };

    if req.stream {
        let rx = gen
            .per_input
            .into_iter()
            .next()
            .and_then(|ig| ig.streams.into_iter().next())
            .expect("single completion yields one stream");
        let include_usage = req.stream_options.is_some_and(|o| o.include_usage);
        return stream_response(
            state.clone(),
            rx,
            StreamKind::Text,
            new_id("cmpl"),
            include_usage,
            None,
        )
        .await;
    }

    // Collect every completion, prompt-major then completion-index, so batched
    // `choices[]` carry a stable running index across all prompts.
    let mut choices = Vec::with_capacity(gen.per_input.len() * 2);
    let mut prompt_tokens = 0usize;
    let mut cached_tokens = 0usize;
    let mut completion_tokens = 0usize;
    let mut index = 0u32;
    for (input_idx, ig) in gen.per_input.into_iter().enumerate() {
        let prompt_ids = ig.prompt_tokens;
        for (comp_idx, rx) in ig.streams.into_iter().enumerate() {
            let c = match collect_events(rx).await {
                Ok(c) => c,
                Err(e) => return e.into_response(),
            };
            // Each prompt's tokens count once (its n completions share the same
            // prompt); completion tokens aggregate across everything.
            if comp_idx == 0 {
                prompt_tokens += c.usage.prompt_tokens;
                cached_tokens += c
                    .usage
                    .prompt_tokens_details
                    .as_ref()
                    .map(|d| d.cached_tokens)
                    .unwrap_or(0);
            }
            completion_tokens += c.usage.completion_tokens;
            let logprobs = want_logprobs.then(|| {
                completion_logprobs(
                    &state.tokenizer,
                    echo.then_some(prompt_ids.as_slice()),
                    &c.logprobs,
                )
            });
            let text = if echo {
                format!("{}{}", echo_texts[input_idx], c.text)
            } else {
                c.text
            };
            choices.push(CompletionChoice {
                index,
                text,
                logprobs,
                finish_reason: c.finish,
            });
            index += 1;
        }
    }

    Json(CompletionResponse {
        id: new_id("cmpl"),
        object: "text_completion",
        created: now_unix(),
        model: state.model_id.clone(),
        choices,
        usage: Usage::with_cache(prompt_tokens, completion_tokens, cached_tokens),
    })
    .into_response()
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
        return ApiError::overloaded("too many concurrent transcription requests").into_response();
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

/// POST /v1/embeddings — OpenAI-compatible embeddings. Accepts a single
/// string, a batch of strings, or pre-tokenized id array(s). Runs each input
/// through the embedding model's pooled forward pass, applies optional
/// Matryoshka truncation (`dimensions`) with renormalization, and returns
/// float or base64 vectors. 404 when the server has no embedding model.
pub async fn embeddings(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<EmbeddingsRequest>,
) -> Response {
    let Some(embed) = state.embed.clone() else {
        return ApiError::not_found(
            "no embedding model is configured; start the server with --embed-model",
        )
        .into_response();
    };
    let encoding = match req.encoding() {
        Ok(e) => e,
        Err(e) => return e.into_response(),
    };
    if let Some(0) = req.dimensions {
        return ApiError::invalid_request("`dimensions` must be at least 1").into_response();
    }
    let dimensions = req.dimensions;
    let items = req.input.into_items();
    if items.is_empty() {
        return ApiError::invalid_request("`input` must not be empty").into_response();
    }

    // One embedding batch at a time on this device; admit before the GPU work
    // so excess load is rejected up front rather than queued unbounded.
    let Ok(_permit) = state.embed_slots.try_acquire() else {
        return ApiError::overloaded("too many concurrent embedding requests").into_response();
    };

    let model_id = embed.model_id.clone();
    let joined =
        tokio::task::spawn_blocking(move || -> Result<(Vec<Vec<f32>>, usize), ApiError> {
            // Tokenize (or accept caller ids) and bound each input before any GPU
            // work, so an over-length item fails fast without partial compute.
            let mut token_sets: Vec<Vec<u32>> = Vec::with_capacity(items.len());
            let mut prompt_tokens = 0usize;
            for item in items {
                let ids = match item {
                    EmbItem::Text(s) => embed
                        .tokenizer
                        .encode(&s, true)
                        .map_err(|e| ApiError::internal(format!("tokenization failed: {e}")))?,
                    EmbItem::Tokens(ids) => ids,
                };
                if ids.is_empty() {
                    return Err(ApiError::invalid_request(
                        "an `input` item produced zero tokens",
                    ));
                }
                if ids.len() > embed.max_context {
                    return Err(ApiError::context_length_exceeded(format!(
                        "input has {} tokens, over the embedding model limit of {}",
                        ids.len(),
                        embed.max_context
                    )));
                }
                prompt_tokens += ids.len();
                token_sets.push(ids);
            }

            let mut model = embed.model.blocking_lock();
            let mut vecs = Vec::with_capacity(token_sets.len());
            for ids in &token_sets {
                let mut v = model
                    .embed(ids, embed.pooling, embed.normalize)
                    .map_err(|e| ApiError::internal(format!("embedding failed: {e}")))?;
                // Matryoshka: keep the leading `dimensions` components; the
                // prefixes of these models are trained to stay meaningful, and a
                // normalized model needs a renormalize after truncation.
                if let Some(d) = dimensions {
                    if d < v.len() {
                        v.truncate(d);
                        if embed.normalize {
                            forge_engine::model::l2_normalize(&mut v);
                        }
                    }
                }
                vecs.push(v);
            }
            Ok((vecs, prompt_tokens))
        })
        .await;

    match joined {
        Ok(Ok((vecs, prompt_tokens))) => {
            let data = vecs
                .into_iter()
                .enumerate()
                .map(|(index, v)| {
                    let embedding = match encoding {
                        EncodingFormat::Float => EmbeddingVec::Float(v),
                        EncodingFormat::Base64 => {
                            let mut bytes = Vec::with_capacity(v.len() * 4);
                            for f in &v {
                                bytes.extend_from_slice(&f.to_le_bytes());
                            }
                            EmbeddingVec::Base64(base64_encode(&bytes))
                        }
                    };
                    EmbeddingData {
                        object: "embedding",
                        embedding,
                        index,
                    }
                })
                .collect();
            Json(EmbeddingsResponse {
                object: "list",
                data,
                model: model_id,
                usage: EmbeddingUsage {
                    prompt_tokens,
                    total_tokens: prompt_tokens,
                },
            })
            .into_response()
        }
        Ok(Err(e)) => e.into_response(),
        Err(e) => ApiError::internal(format!("embedding task failed: {e}")).into_response(),
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

        let out =
            render_chat_prompt(template, &serde_json::Map::new(), &messages, Some(&tools)).unwrap();
        assert!(out.contains("<tool_call>"), "assistant call missing: {out}");
        assert!(out.contains("get_weather"));
        assert!(out.contains("<tool_response>\n12°C, słonecznie"));
        assert!(!out.contains("undefined"), "undefined leaked: {out}");
    }
}
