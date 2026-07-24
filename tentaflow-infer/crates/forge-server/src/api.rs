// ===== File: api.rs — OpenAI request/response types + pure request validation =====
// Everything here is transport-only and unit-testable without a GPU: parse
// the wire shapes, validate them, and produce the sampling spec the engine
// consumes. Unknown request fields are accepted and ignored by serde.

use std::collections::{BTreeMap, HashMap};

use forge_engine::sample::SamplingParams;
use forge_tokenize::ChatMessage;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;

/// Largest `n` (parallel completions) one request may ask for.
const MAX_N: u32 = 128;
/// Largest `top_logprobs` / completions `logprobs` count accepted.
const MAX_TOP_LOGPROBS: usize = 20;

/// `stop` accepts a single string or an array of strings.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum StopSpec {
    One(String),
    Many(Vec<String>),
}

impl StopSpec {
    pub fn into_vec(self) -> Vec<String> {
        match self {
            StopSpec::One(s) => vec![s],
            StopSpec::Many(v) => v,
        }
    }
}

/// OpenAI `stream_options` — only `include_usage` is meaningful here.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StreamOptions {
    #[serde(default)]
    pub include_usage: bool,
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub top_k: Option<usize>,
    #[serde(default)]
    pub min_p: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<usize>,
    /// Newer OpenAI alias for `max_tokens`.
    #[serde(default)]
    pub max_completion_tokens: Option<usize>,
    #[serde(default)]
    pub stop: Option<StopSpec>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub stream_options: Option<StreamOptions>,
    #[serde(default)]
    pub seed: Option<u64>,
    #[serde(default)]
    pub n: Option<u32>,
    #[serde(default)]
    pub repetition_penalty: Option<f32>,
    #[serde(default)]
    pub frequency_penalty: Option<f32>,
    #[serde(default)]
    pub presence_penalty: Option<f32>,
    /// Rozszerzenie zgodne z llama.cpp; zero obejmuje cały prompt i odpowiedź.
    #[serde(default)]
    pub repeat_last_n: Option<usize>,
    /// `logit_bias` (SPEC §8.1.2): `{token_id: bias}` with string keys, bias in
    /// [-100, 100]. ±100 ≈ hard force/ban.
    #[serde(default)]
    pub logit_bias: Option<HashMap<String, f32>>,
    /// `min_tokens` (non-standard extension): floor on generated tokens; EOS is
    /// suppressed until reached.
    #[serde(default)]
    pub min_tokens: Option<usize>,
    /// `logprobs` (SPEC §8.1.2): when `true`, each output token carries its
    /// log-probability (and up to `top_logprobs` alternatives).
    #[serde(default)]
    pub logprobs: Option<bool>,
    /// Number of most-likely alternatives to report per token (0..=20). Requires
    /// `logprobs = true`.
    #[serde(default)]
    pub top_logprobs: Option<usize>,
    /// OpenAI tool definitions, passed verbatim to the chat template.
    #[serde(default)]
    pub tools: Option<serde_json::Value>,
    /// "auto" (default), "none", "required", or a named function forcing spec.
    #[serde(default)]
    pub tool_choice: Option<serde_json::Value>,
    /// OpenAI `response_format`: `{"type":"text"}` (default),
    /// `{"type":"json_object"}` (any valid JSON) or
    /// `{"type":"json_schema","json_schema":{"schema":{...}}}`
    /// (schema-constrained). Non-standard extensions:
    /// `{"type":"regex","regex":"..."}` and `{"type":"grammar","grammar":"..."}`.
    #[serde(default)]
    pub response_format: Option<serde_json::Value>,
    /// GBNF/EBNF grammar passthrough (non-standard; constrains the output).
    #[serde(default)]
    pub grammar: Option<String>,
}

/// A tool the model must call (from `tool_choice` "required" / named).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolForcing {
    /// Any one of the request's tools.
    Any,
    /// One named function.
    Named(String),
}

/// How this request wants tool calling handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolMode {
    /// Render tools into the template and parse tool calls from the output.
    Auto,
    /// Do not render tools and do not parse tool calls.
    None,
}

/// `prompt` accepts a single string, a batch of strings, a single pre-tokenized
/// id array, or a batch of id arrays (the OpenAI completions `prompt` shape).
/// `untagged` tries the variants top-down; token arrays never parse as strings,
/// so the ordering is unambiguous (string batch, then id-array batch, then a
/// single id array).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum PromptSpec {
    One(String),
    Many(Vec<String>),
    TokenBatches(Vec<Vec<u32>>),
    Tokens(Vec<u32>),
}

/// One resolved completions prompt: raw text still to tokenize, or ids supplied
/// directly by the caller.
#[derive(Debug, Clone)]
pub enum PromptItem {
    Text(String),
    Tokens(Vec<u32>),
}

impl PromptSpec {
    /// Flatten the request `prompt` into one item per completion prompt.
    pub fn into_items(self) -> Vec<PromptItem> {
        match self {
            PromptSpec::One(s) => vec![PromptItem::Text(s)],
            PromptSpec::Many(v) => v.into_iter().map(PromptItem::Text).collect(),
            PromptSpec::Tokens(ids) => vec![PromptItem::Tokens(ids)],
            PromptSpec::TokenBatches(b) => b.into_iter().map(PromptItem::Tokens).collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CompletionRequest {
    pub model: String,
    pub prompt: PromptSpec,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub top_k: Option<usize>,
    #[serde(default)]
    pub min_p: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<usize>,
    #[serde(default)]
    pub max_completion_tokens: Option<usize>,
    #[serde(default)]
    pub stop: Option<StopSpec>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub stream_options: Option<StreamOptions>,
    #[serde(default)]
    pub seed: Option<u64>,
    #[serde(default)]
    pub n: Option<u32>,
    #[serde(default)]
    pub echo: Option<bool>,
    #[serde(default)]
    pub repetition_penalty: Option<f32>,
    #[serde(default)]
    pub frequency_penalty: Option<f32>,
    #[serde(default)]
    pub presence_penalty: Option<f32>,
    #[serde(default)]
    pub repeat_last_n: Option<usize>,
    /// `logit_bias` (SPEC §8.1.2): `{token_id: bias}`, bias in [-100, 100].
    #[serde(default)]
    pub logit_bias: Option<HashMap<String, f32>>,
    /// `min_tokens` (non-standard extension): EOS suppressed until reached.
    #[serde(default)]
    pub min_tokens: Option<usize>,
    /// `logprobs` (SPEC §8.1.2): number of top alternatives to report per token
    /// (0..=20); `0` reports only each token's own log-probability.
    #[serde(default)]
    pub logprobs: Option<usize>,
}

/// Engine-facing generation parameters extracted from a validated request.
#[derive(Debug, Clone)]
pub struct GenerationSpec {
    pub sampling: SamplingParams,
    pub max_tokens: usize,
    pub stop: Vec<String>,
    /// Number of independent completions to generate (`n`, ≥ 1).
    pub n: usize,
    /// `logit_bias` as `(token_id, bias)` pairs, sorted by id for determinism.
    pub logit_bias: Vec<(u32, f32)>,
    /// EOS floor (`min_tokens`); `0` = disabled.
    pub min_tokens: usize,
    /// `logprobs`: `Some(top_n)` to report per-token log-probabilities with
    /// `top_n` alternatives; `None` to omit.
    pub logprobs: Option<usize>,
    /// `echo` (completions): prepend the prompt to the response.
    pub echo: bool,
}

// Requests without max_tokens still need a bound: the engine's admission
// control projects prompt+max_tokens against KV pages, so "unlimited" would
// starve the queue.
const DEFAULT_MAX_TOKENS: usize = 1024;

/// Common sampling core shared by both endpoints: validate the sampling knobs
/// and resolve `max_tokens`/`stop`. The per-endpoint extras (`n`, `logit_bias`,
/// `min_tokens`, `logprobs`, `echo`) are layered on by each request's builder.
#[allow(clippy::too_many_arguments)]
fn sampling_core(
    temperature: Option<f32>,
    top_p: Option<f32>,
    top_k: Option<usize>,
    min_p: Option<f32>,
    repetition_penalty: Option<f32>,
    frequency_penalty: Option<f32>,
    presence_penalty: Option<f32>,
    repeat_last_n: Option<usize>,
    seed: Option<u64>,
    max_tokens: Option<usize>,
    max_completion_tokens: Option<usize>,
    stop: Option<StopSpec>,
) -> Result<(SamplingParams, usize, Vec<String>), ApiError> {
    let mut sampling = SamplingParams::default();
    if let Some(t) = temperature {
        if !t.is_finite() || t < 0.0 {
            return Err(ApiError::invalid_request(
                "temperature must be a finite number >= 0",
            ));
        }
        sampling.temperature = t;
    }
    if let Some(p) = top_p {
        if !p.is_finite() || p <= 0.0 || p > 1.0 {
            return Err(ApiError::invalid_request("top_p must be in (0, 1]"));
        }
        sampling.top_p = p;
    }
    if let Some(k) = top_k {
        sampling.top_k = k;
    }
    if let Some(p) = min_p {
        if !p.is_finite() || !(0.0..=1.0).contains(&p) {
            return Err(ApiError::invalid_request("min_p must be in [0, 1]"));
        }
        sampling.min_p = p;
    }
    if let Some(rp) = repetition_penalty {
        if !rp.is_finite() || rp <= 0.0 {
            return Err(ApiError::invalid_request(
                "repetition_penalty must be a finite number > 0",
            ));
        }
        sampling.repetition_penalty = rp;
    }
    if let Some(value) = frequency_penalty {
        if !value.is_finite() || !(-2.0..=2.0).contains(&value) {
            return Err(ApiError::invalid_request(
                "frequency_penalty must be in [-2, 2]",
            ));
        }
        sampling.frequency_penalty = value;
    }
    if let Some(value) = presence_penalty {
        if !value.is_finite() || !(-2.0..=2.0).contains(&value) {
            return Err(ApiError::invalid_request(
                "presence_penalty must be in [-2, 2]",
            ));
        }
        sampling.presence_penalty = value;
    }
    sampling.repeat_last_n = repeat_last_n.unwrap_or(0);
    sampling.seed = seed;

    let max_tokens = max_tokens
        .or(max_completion_tokens)
        .unwrap_or(DEFAULT_MAX_TOKENS);
    if max_tokens == 0 {
        return Err(ApiError::invalid_request("max_tokens must be at least 1"));
    }

    Ok((
        sampling,
        max_tokens,
        stop.map(StopSpec::into_vec).unwrap_or_default(),
    ))
}

/// Validate `n` (number of parallel completions): default 1, at least 1, at
/// most `MAX_N`.
fn resolve_n(n: Option<u32>) -> Result<usize, ApiError> {
    match n {
        None => Ok(1),
        Some(0) => Err(ApiError::invalid_request("n must be at least 1")),
        Some(v) if v > MAX_N => Err(ApiError::invalid_request(format!(
            "n must be at most {MAX_N}"
        ))),
        Some(v) => Ok(v as usize),
    }
}

/// Parse an OpenAI `logit_bias` map (string token-id keys → bias) into sorted
/// `(id, bias)` pairs. Biases must be finite and within [-100, 100]. Sorted by
/// id so the engine sees a deterministic order regardless of map iteration.
fn parse_logit_bias(map: &Option<HashMap<String, f32>>) -> Result<Vec<(u32, f32)>, ApiError> {
    let Some(m) = map else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(m.len());
    for (k, &v) in m {
        let id: u32 = k.parse().map_err(|_| {
            ApiError::invalid_request(format!("logit_bias key {k:?} is not a token id"))
        })?;
        if !v.is_finite() || !(-100.0..=100.0).contains(&v) {
            return Err(ApiError::invalid_request(
                "logit_bias values must be finite and in [-100, 100]",
            ));
        }
        out.push((id, v));
    }
    out.sort_unstable_by_key(|&(id, _)| id);
    Ok(out)
}

/// Validate `min_tokens`: `None`/`0` disables it; it must not exceed
/// `max_tokens` (the sequence can never produce more than that).
fn resolve_min_tokens(min_tokens: Option<usize>, max_tokens: usize) -> Result<usize, ApiError> {
    match min_tokens {
        None | Some(0) => Ok(0),
        Some(m) if m > max_tokens => Err(ApiError::invalid_request(format!(
            "min_tokens ({m}) must not exceed max_tokens ({max_tokens})"
        ))),
        Some(m) => Ok(m),
    }
}

/// Reject requests whose token budget can never fit the model context.
pub fn check_context(
    prompt_len: usize,
    max_tokens: usize,
    max_context: usize,
) -> Result<(), ApiError> {
    match prompt_len.checked_add(max_tokens) {
        Some(total) if total <= max_context => Ok(()),
        _ => Err(ApiError::context_length_exceeded(format!(
            "this model supports at most {max_context} context tokens; the request has \
             {prompt_len} prompt tokens and asks for {max_tokens} completion tokens"
        ))),
    }
}

const CHAT_ROLES: &[&str] = &["system", "user", "assistant", "tool"];

impl ChatCompletionRequest {
    pub fn generation_spec(&self) -> Result<GenerationSpec, ApiError> {
        if self.messages.is_empty() {
            return Err(ApiError::invalid_request("messages must not be empty"));
        }
        for (i, m) in self.messages.iter().enumerate() {
            if !CHAT_ROLES.contains(&m.role.as_str()) {
                return Err(ApiError::invalid_request(format!(
                    "messages[{i}].role {:?} is not one of {CHAT_ROLES:?}",
                    m.role
                )));
            }
            // Assistant turns replayed from a previous tool-call response
            // legitimately carry content:null next to tool_calls.
            let has_tool_calls = m
                .tool_calls
                .as_ref()
                .is_some_and(|tc| tc.as_array().is_some_and(|a| !a.is_empty()));
            if m.text_content().is_none() && !(m.role == "assistant" && has_tool_calls) {
                return Err(ApiError::invalid_request(format!(
                    "messages[{i}].content must be a string or an array of text parts"
                )));
            }
        }
        let (sampling, max_tokens, stop) = sampling_core(
            self.temperature,
            self.top_p,
            self.top_k,
            self.min_p,
            self.repetition_penalty,
            self.frequency_penalty,
            self.presence_penalty,
            self.repeat_last_n,
            self.seed,
            self.max_tokens,
            self.max_completion_tokens,
            self.stop.clone(),
        )?;
        // `logprobs` is a bool for chat; `top_logprobs` selects the alternative
        // count and requires `logprobs = true`.
        let logprobs = match self.logprobs {
            Some(true) => {
                let n = self.top_logprobs.unwrap_or(0);
                if n > MAX_TOP_LOGPROBS {
                    return Err(ApiError::invalid_request(format!(
                        "top_logprobs must be in [0, {MAX_TOP_LOGPROBS}]"
                    )));
                }
                Some(n)
            }
            _ => {
                if self.top_logprobs.is_some() {
                    return Err(ApiError::invalid_request(
                        "top_logprobs requires logprobs = true",
                    ));
                }
                None
            }
        };
        Ok(GenerationSpec {
            sampling,
            n: resolve_n(self.n)?,
            logit_bias: parse_logit_bias(&self.logit_bias)?,
            min_tokens: resolve_min_tokens(self.min_tokens, max_tokens)?,
            logprobs,
            echo: false,
            max_tokens,
            stop,
        })
    }

    /// Whether tools are rendered into the template and parsed out of the
    /// output. "required" and named forcing both render+parse (like "auto");
    /// the forcing itself is applied via constrained decoding — see
    /// [`ChatCompletionRequest::tool_forcing`].
    pub fn tool_mode(&self) -> Result<ToolMode, ApiError> {
        let has_tools = self.has_tools()?;
        match &self.tool_choice {
            None => {}
            Some(serde_json::Value::String(s)) => match s.as_str() {
                "auto" | "required" => {}
                "none" => return Ok(ToolMode::None),
                other => {
                    return Err(ApiError::invalid_request(format!(
                        "tool_choice {other:?} is not one of \"auto\", \"none\", \"required\""
                    )))
                }
            },
            // A named-function forcing object renders + parses like "auto".
            Some(serde_json::Value::Object(_)) => {}
            Some(_) => {
                return Err(ApiError::invalid_request(
                    "tool_choice must be a string or a function-forcing object",
                ))
            }
        }
        Ok(if has_tools {
            ToolMode::Auto
        } else {
            ToolMode::None
        })
    }

    fn has_tools(&self) -> Result<bool, ApiError> {
        match &self.tools {
            None | Some(serde_json::Value::Null) => Ok(false),
            Some(serde_json::Value::Array(a)) => Ok(!a.is_empty()),
            Some(_) => Err(ApiError::invalid_request(
                "tools must be an array of tool definitions",
            )),
        }
    }

    /// Resolve `tool_choice` "required" / named forcing (SPEC §8.1.1): the
    /// model is constrained to emit a valid call. `None` = no forcing.
    pub fn tool_forcing(&self) -> Result<Option<ToolForcing>, ApiError> {
        if !self.has_tools()? {
            // "required" with no tools is a client error; "auto"/"none" fine.
            if matches!(&self.tool_choice, Some(serde_json::Value::String(s)) if s == "required")
                || matches!(&self.tool_choice, Some(serde_json::Value::Object(_)))
            {
                return Err(ApiError::invalid_request(
                    "tool_choice forcing requires a non-empty `tools` array",
                ));
            }
            return Ok(None);
        }
        match &self.tool_choice {
            Some(serde_json::Value::String(s)) if s == "required" => Ok(Some(ToolForcing::Any)),
            Some(serde_json::Value::Object(o)) => {
                let name = o
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .ok_or_else(|| {
                        ApiError::invalid_request(
                            "named tool_choice must be {\"type\":\"function\",\"function\":{\"name\":...}}",
                        )
                    })?;
                Ok(Some(ToolForcing::Named(name.to_string())))
            }
            _ => Ok(None),
        }
    }

    /// The `(name, parameters-schema)` pairs of this request's function tools.
    pub fn tool_definitions(&self) -> Vec<(String, serde_json::Value)> {
        let Some(serde_json::Value::Array(tools)) = &self.tools else {
            return Vec::new();
        };
        tools
            .iter()
            .filter_map(|t| {
                let f = t.get("function")?;
                let name = f.get("name")?.as_str()?.to_string();
                let params = f
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({"type": "object"}));
                Some((name, params))
            })
            .collect()
    }
}

impl CompletionRequest {
    pub fn generation_spec(&self) -> Result<GenerationSpec, ApiError> {
        let (sampling, max_tokens, stop) = sampling_core(
            self.temperature,
            self.top_p,
            self.top_k,
            self.min_p,
            self.repetition_penalty,
            self.frequency_penalty,
            self.presence_penalty,
            self.repeat_last_n,
            self.seed,
            self.max_tokens,
            self.max_completion_tokens,
            self.stop.clone(),
        )?;
        let logprobs = match self.logprobs {
            None => None,
            Some(n) if n > MAX_TOP_LOGPROBS => {
                return Err(ApiError::invalid_request(format!(
                    "logprobs must be in [0, {MAX_TOP_LOGPROBS}]"
                )))
            }
            Some(n) => Some(n),
        };
        Ok(GenerationSpec {
            sampling,
            n: resolve_n(self.n)?,
            logit_bias: parse_logit_bias(&self.logit_bias)?,
            min_tokens: resolve_min_tokens(self.min_tokens, max_tokens)?,
            logprobs,
            echo: self.echo.unwrap_or(false),
            max_tokens,
            stop,
        })
    }
}

// ---- Response shapes ----

#[derive(Debug, Serialize)]
pub struct Usage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
    /// OpenAI-compatible cache accounting: `cached_tokens` reports the prompt
    /// prefix served from the radix KV cache (SPEC §5.2). Omitted when zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens_details: Option<PromptTokensDetails>,
}

#[derive(Debug, Serialize)]
pub struct PromptTokensDetails {
    pub cached_tokens: usize,
}

impl Usage {
    pub fn new(prompt_tokens: usize, completion_tokens: usize) -> Self {
        Self::with_cache(prompt_tokens, completion_tokens, 0)
    }

    pub fn with_cache(
        prompt_tokens: usize,
        completion_tokens: usize,
        cached_tokens: usize,
    ) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            prompt_tokens_details: (cached_tokens > 0)
                .then_some(PromptTokensDetails { cached_tokens }),
        }
    }
}

/// OpenAI tool-call object on a response message.
#[derive(Debug, Serialize)]
pub struct ToolCallOut {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: &'static str,
    pub function: FunctionCallOut,
}

#[derive(Debug, Serialize)]
pub struct FunctionCallOut {
    pub name: String,
    /// JSON-encoded arguments string, per the OpenAI wire shape.
    pub arguments: String,
}

#[derive(Debug, Serialize)]
pub struct ChatResponseMessage {
    pub role: &'static str,
    /// `null` (not omitted) when the message is only tool calls.
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallOut>>,
}

/// One alternative in a token's `top_logprobs` list.
#[derive(Debug, Serialize)]
pub struct TopLogprobEntry {
    pub token: String,
    pub logprob: f32,
    pub bytes: Vec<u8>,
}

/// One chat output token's log-probability entry (OpenAI `logprobs.content[]`).
#[derive(Debug, Serialize)]
pub struct ChatLogprobEntry {
    pub token: String,
    pub logprob: f32,
    pub bytes: Vec<u8>,
    pub top_logprobs: Vec<TopLogprobEntry>,
}

/// Chat `logprobs` object: one entry per surfaced output token.
#[derive(Debug, Serialize)]
pub struct ChatLogprobs {
    pub content: Vec<ChatLogprobEntry>,
}

#[derive(Debug, Serialize)]
pub struct ChatChoice {
    pub index: u32,
    pub message: ChatResponseMessage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<ChatLogprobs>,
    pub finish_reason: &'static str,
}

#[derive(Debug, Serialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: &'static str,
    pub created: u64,
    pub model: String,
    pub choices: Vec<ChatChoice>,
    pub usage: Usage,
}

/// Completions `logprobs` object (OpenAI legacy shape): parallel arrays over
/// the emitted tokens. `token_logprobs[i]` is `null` for a position without a
/// conditional log-probability (the first echoed prompt token).
#[derive(Debug, Serialize)]
pub struct CompletionLogprobs {
    pub tokens: Vec<String>,
    pub token_logprobs: Vec<Option<f32>>,
    pub top_logprobs: Vec<BTreeMap<String, f32>>,
    pub text_offset: Vec<usize>,
}

#[derive(Debug, Serialize)]
pub struct CompletionChoice {
    pub index: u32,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<CompletionLogprobs>,
    pub finish_reason: &'static str,
}

#[derive(Debug, Serialize)]
pub struct CompletionResponse {
    pub id: String,
    pub object: &'static str,
    pub created: u64,
    pub model: String,
    pub choices: Vec<CompletionChoice>,
    pub usage: Usage,
}

// ---- Embeddings ----

/// OpenAI `/v1/embeddings` `input`: one string, a batch of strings, a single
/// pre-tokenized id array, or a batch of id arrays. `untagged` tries the
/// variants top-down; token arrays never parse as strings, so the ordering is
/// unambiguous (string batch, then id-array batch, then single id array).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum EmbeddingInput {
    Text(String),
    Texts(Vec<String>),
    TokenBatches(Vec<Vec<u32>>),
    Tokens(Vec<u32>),
}

/// One resolved embedding input: raw text still to tokenize, or ids supplied
/// directly by the caller.
#[derive(Debug, Clone)]
pub enum EmbItem {
    Text(String),
    Tokens(Vec<u32>),
}

impl EmbeddingInput {
    pub fn into_items(self) -> Vec<EmbItem> {
        match self {
            EmbeddingInput::Text(s) => vec![EmbItem::Text(s)],
            EmbeddingInput::Texts(v) => v.into_iter().map(EmbItem::Text).collect(),
            EmbeddingInput::Tokens(ids) => vec![EmbItem::Tokens(ids)],
            EmbeddingInput::TokenBatches(b) => b.into_iter().map(EmbItem::Tokens).collect(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddingsRequest {
    pub model: String,
    pub input: EmbeddingInput,
    /// "float" (default) or "base64" (little-endian f32 bytes, base64 string).
    #[serde(default)]
    pub encoding_format: Option<String>,
    /// Matryoshka truncation: keep the first `dimensions` components and
    /// renormalize. Ignored when ≥ the model's native width.
    #[serde(default)]
    pub dimensions: Option<usize>,
}

/// Per-request wire format for each vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodingFormat {
    Float,
    Base64,
}

impl EmbeddingsRequest {
    pub fn encoding(&self) -> Result<EncodingFormat, ApiError> {
        match self.encoding_format.as_deref() {
            None | Some("float") => Ok(EncodingFormat::Float),
            Some("base64") => Ok(EncodingFormat::Base64),
            Some(other) => Err(ApiError::invalid_request(format!(
                "unsupported encoding_format '{other}' (expected float | base64)"
            ))),
        }
    }
}

/// Either a raw float vector or its base64 encoding, chosen by the request.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum EmbeddingVec {
    Float(Vec<f32>),
    Base64(String),
}

#[derive(Debug, Serialize)]
pub struct EmbeddingData {
    pub object: &'static str,
    pub embedding: EmbeddingVec,
    pub index: usize,
}

/// Embeddings usage: no completion tokens, unlike chat.
#[derive(Debug, Serialize)]
pub struct EmbeddingUsage {
    pub prompt_tokens: usize,
    pub total_tokens: usize,
}

#[derive(Debug, Serialize)]
pub struct EmbeddingsResponse {
    pub object: &'static str,
    pub data: Vec<EmbeddingData>,
    pub model: String,
    pub usage: EmbeddingUsage,
}

/// Standard-alphabet base64 (with padding), used for `encoding_format:
/// "base64"` vector bodies.
pub fn base64_encode(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[(n >> 18) as usize & 0x3f] as char);
        out.push(T[(n >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

#[derive(Debug, Serialize)]
pub struct ModelEntry {
    pub id: String,
    pub object: &'static str,
    pub created: u64,
    pub owned_by: &'static str,
}

#[derive(Debug, Serialize)]
pub struct ModelList {
    pub object: &'static str,
    pub data: Vec<ModelEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat_req(json: serde_json::Value) -> ChatCompletionRequest {
        serde_json::from_value(json).unwrap()
    }

    fn embed_req(json: serde_json::Value) -> EmbeddingsRequest {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn embedding_input_variants_parse_unambiguously() {
        // Single string.
        let r = embed_req(serde_json::json!({"model": "m", "input": "hello"}));
        assert!(matches!(r.input.into_items().as_slice(), [EmbItem::Text(s)] if s == "hello"));
        // String batch.
        let r = embed_req(serde_json::json!({"model": "m", "input": ["a", "b"]}));
        assert_eq!(r.input.into_items().len(), 2);
        // Single pre-tokenized id array.
        let r = embed_req(serde_json::json!({"model": "m", "input": [1, 2, 3]}));
        assert!(
            matches!(r.input.into_items().as_slice(), [EmbItem::Tokens(ids)] if ids == &[1, 2, 3])
        );
        // Batch of id arrays.
        let r = embed_req(serde_json::json!({"model": "m", "input": [[1, 2], [3]]}));
        let items = r.input.into_items();
        assert_eq!(items.len(), 2);
        assert!(matches!(&items[0], EmbItem::Tokens(ids) if ids == &[1, 2]));
    }

    #[test]
    fn encoding_format_defaults_and_rejects() {
        let r = embed_req(serde_json::json!({"model": "m", "input": "x"}));
        assert_eq!(r.encoding().unwrap(), EncodingFormat::Float);
        let r =
            embed_req(serde_json::json!({"model": "m", "input": "x", "encoding_format": "base64"}));
        assert_eq!(r.encoding().unwrap(), EncodingFormat::Base64);
        let r =
            embed_req(serde_json::json!({"model": "m", "input": "x", "encoding_format": "yaml"}));
        assert!(r.encoding().is_err());
    }

    #[test]
    fn base64_matches_reference_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn stop_accepts_string_and_array() {
        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}],
            "stop": "###"
        }));
        assert_eq!(r.generation_spec().unwrap().stop, vec!["###".to_string()]);

        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}],
            "stop": ["a", "b"]
        }));
        assert_eq!(
            r.generation_spec().unwrap().stop,
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn max_completion_tokens_is_an_alias() {
        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}],
            "max_completion_tokens": 7
        }));
        assert_eq!(r.generation_spec().unwrap().max_tokens, 7);

        // max_tokens wins when both are present.
        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 3, "max_completion_tokens": 7
        }));
        assert_eq!(r.generation_spec().unwrap().max_tokens, 3);
    }

    #[test]
    fn n_is_accepted_and_bounded() {
        // n > 1 is now supported.
        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}], "n": 3
        }));
        assert_eq!(r.generation_spec().unwrap().n, 3);

        // Default is a single completion.
        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}]
        }));
        assert_eq!(r.generation_spec().unwrap().n, 1);

        // 0 and over-cap are rejected.
        for bad in [0u32, MAX_N + 1] {
            let r = chat_req(serde_json::json!({
                "model": "m", "messages": [{"role": "user", "content": "hi"}], "n": bad
            }));
            assert_eq!(
                r.generation_spec().unwrap_err().status,
                axum::http::StatusCode::BAD_REQUEST
            );
        }
    }

    #[test]
    fn logit_bias_parses_and_validates() {
        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}],
            "logit_bias": {"5": -100, "2": 12.5}
        }));
        // Sorted by token id for deterministic engine ordering.
        assert_eq!(
            r.generation_spec().unwrap().logit_bias,
            vec![(2u32, 12.5), (5u32, -100.0)]
        );

        // Out-of-range bias is rejected.
        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}],
            "logit_bias": {"5": 250}
        }));
        assert!(r.generation_spec().is_err());

        // Non-integer key is rejected.
        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}],
            "logit_bias": {"cat": 1.0}
        }));
        assert!(r.generation_spec().is_err());
    }

    #[test]
    fn min_tokens_is_bounded_by_max_tokens() {
        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}],
            "min_tokens": 20, "max_tokens": 64
        }));
        assert_eq!(r.generation_spec().unwrap().min_tokens, 20);

        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}],
            "min_tokens": 100, "max_tokens": 64
        }));
        assert!(r.generation_spec().is_err());
    }

    #[test]
    fn chat_logprobs_rules() {
        // logprobs=true, top_logprobs=5 -> Some(5).
        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}],
            "logprobs": true, "top_logprobs": 5
        }));
        assert_eq!(r.generation_spec().unwrap().logprobs, Some(5));

        // logprobs=true without top_logprobs -> Some(0).
        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}],
            "logprobs": true
        }));
        assert_eq!(r.generation_spec().unwrap().logprobs, Some(0));

        // top_logprobs without logprobs=true is an error.
        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}],
            "top_logprobs": 3
        }));
        assert!(r.generation_spec().is_err());

        // Over-cap top_logprobs is an error.
        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}],
            "logprobs": true, "top_logprobs": 999
        }));
        assert!(r.generation_spec().is_err());
    }

    #[test]
    fn multipart_text_content_is_accepted() {
        let r = chat_req(serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "hello "},
                {"type": "text", "text": "world"}
            ]}]
        }));
        r.generation_spec().unwrap();
        assert_eq!(
            r.messages[0].text_content().unwrap(),
            "hello world".to_string()
        );
    }

    #[test]
    fn bad_role_and_bad_sampling_are_rejected() {
        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "wizard", "content": "hi"}]
        }));
        assert!(r.generation_spec().is_err());

        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}],
            "temperature": -0.5
        }));
        assert!(r.generation_spec().is_err());

        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}],
            "top_p": 1.5
        }));
        assert!(r.generation_spec().is_err());
    }

    #[test]
    fn parametry_probkowania_sa_przekazywane_bez_zmian() {
        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}],
            "temperature": 0.4, "top_k": 17, "top_p": 0.8, "min_p": 0.15,
            "seed": 0
        }));
        let sampling = r.generation_spec().unwrap().sampling;

        assert_eq!(sampling.temperature, 0.4);
        assert_eq!(sampling.top_k, 17);
        assert_eq!(sampling.top_p, 0.8);
        assert_eq!(sampling.min_p, 0.15);
        assert_eq!(sampling.seed, Some(0));

        for min_p in [-0.1, 1.1] {
            let invalid = chat_req(serde_json::json!({
                "model": "m", "messages": [{"role": "user", "content": "hi"}],
                "min_p": min_p
            }));
            assert!(invalid.generation_spec().is_err());
        }
    }

    #[test]
    fn openai_penalties_sa_walidowane_i_przekazywane() {
        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}],
            "frequency_penalty": 0.5, "presence_penalty": 0.1,
            "repetition_penalty": 1.2, "repeat_last_n": 64,
            "logit_bias": {"5": -100}, "user": "abc"
        }));
        let sampling = r.generation_spec().unwrap().sampling;
        assert_eq!(sampling.frequency_penalty, 0.5);
        assert_eq!(sampling.presence_penalty, 0.1);
        assert_eq!(sampling.repetition_penalty, 1.2);
        assert_eq!(sampling.repeat_last_n, 64);

        let invalid = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}],
            "frequency_penalty": 2.1
        }));
        assert!(invalid.generation_spec().is_err());

        let completion: CompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "m", "prompt": [1, 2, 3],
            "frequency_penalty": -0.25, "presence_penalty": 0.75,
            "repeat_last_n": 2
        }))
        .unwrap();
        let sampling = completion.generation_spec().unwrap().sampling;
        assert_eq!(sampling.frequency_penalty, -0.25);
        assert_eq!(sampling.presence_penalty, 0.75);
        assert_eq!(sampling.repeat_last_n, 2);
    }

    #[test]
    fn context_budget_is_enforced() {
        check_context(100, 100, 200).unwrap();
        assert!(check_context(100, 101, 200).is_err());
        // Overflow must reject, not wrap.
        assert!(check_context(usize::MAX, usize::MAX, 8192).is_err());
        let err = check_context(9000, 1024, 8192).unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(err.body.code.as_deref(), Some("context_length_exceeded"));
    }

    #[test]
    fn stream_options_include_usage_parses() {
        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}],
            "stream": true, "stream_options": {"include_usage": true}
        }));
        assert!(r.stream_options.unwrap().include_usage);

        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}], "stream": true
        }));
        assert!(r.stream_options.is_none());
    }

    #[test]
    fn tool_choice_rules() {
        let tools = serde_json::json!([{
            "type": "function",
            "function": {"name": "get_weather", "parameters": {"type": "object"}}
        }]);

        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}],
            "tools": tools
        }));
        assert_eq!(r.tool_mode().unwrap(), ToolMode::Auto);

        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}],
            "tools": tools, "tool_choice": "auto"
        }));
        assert_eq!(r.tool_mode().unwrap(), ToolMode::Auto);

        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}],
            "tools": tools, "tool_choice": "none"
        }));
        assert_eq!(r.tool_mode().unwrap(), ToolMode::None);

        // No tools (or an empty list) means nothing to parse.
        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}]
        }));
        assert_eq!(r.tool_mode().unwrap(), ToolMode::None);
        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}], "tools": []
        }));
        assert_eq!(r.tool_mode().unwrap(), ToolMode::None);

        // Forcing modes now render + parse like "auto" (the forcing itself is
        // applied via constrained decoding).
        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}],
            "tools": tools, "tool_choice": "required"
        }));
        assert_eq!(r.tool_mode().unwrap(), ToolMode::Auto);
        assert_eq!(r.tool_forcing().unwrap(), Some(ToolForcing::Any));

        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}],
            "tools": tools,
            "tool_choice": {"type": "function", "function": {"name": "get_weather"}}
        }));
        assert_eq!(r.tool_mode().unwrap(), ToolMode::Auto);
        assert_eq!(
            r.tool_forcing().unwrap(),
            Some(ToolForcing::Named("get_weather".into()))
        );

        // "required" with no tools is a client error.
        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}],
            "tool_choice": "required"
        }));
        assert_eq!(
            r.tool_forcing().unwrap_err().status,
            axum::http::StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn tool_call_round_trip_messages_validate() {
        // The follow-up request of a tool-calling exchange replays our own
        // response: assistant with content:null + tool_calls, then the tool
        // result. That must validate.
        let r = chat_req(serde_json::json!({
            "model": "m",
            "messages": [
                {"role": "user", "content": "Jaka jest pogoda w Krakowie?"},
                {"role": "assistant", "content": null, "tool_calls": [{
                    "id": "call_0badc0de", "type": "function",
                    "function": {"name": "get_weather", "arguments": "{\"city\":\"Kraków\"}"}
                }]},
                {"role": "tool", "tool_call_id": "call_0badc0de", "content": "12°C, słonecznie"}
            ]
        }));
        r.generation_spec().unwrap();

        // Content-less assistant WITHOUT tool calls is still rejected...
        let r = chat_req(serde_json::json!({
            "model": "m",
            "messages": [{"role": "assistant", "tool_calls": []}]
        }));
        assert!(r.generation_spec().is_err());
        // ...and so is a content-less tool message even next to tool_calls.
        let r = chat_req(serde_json::json!({
            "model": "m",
            "messages": [{"role": "tool", "tool_call_id": "call_1", "tool_calls": [{"x": 1}]}]
        }));
        assert!(r.generation_spec().is_err());
    }

    #[test]
    fn tools_must_be_an_array() {
        for bad in [
            serde_json::json!({"type": "function"}),
            serde_json::json!("get_weather"),
            serde_json::json!(42),
        ] {
            let r = chat_req(serde_json::json!({
                "model": "m", "messages": [{"role": "user", "content": "hi"}],
                "tools": bad
            }));
            let err = r.tool_mode().unwrap_err();
            assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
            assert_eq!(err.body.error_type, "invalid_request_error");
        }
        // Explicit null is "no tools", not an error.
        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}],
            "tools": null
        }));
        assert_eq!(r.tool_mode().unwrap(), ToolMode::None);
    }

    #[test]
    fn tool_call_message_serializes_null_content() {
        let msg = ChatResponseMessage {
            role: "assistant",
            content: None,
            reasoning_content: None,
            tool_calls: Some(vec![ToolCallOut {
                id: "call_0badc0de".into(),
                call_type: "function",
                function: FunctionCallOut {
                    name: "get_weather".into(),
                    arguments: "{\"city\":\"Kraków\"}".into(),
                },
            }]),
        };
        let v = serde_json::to_value(&msg).unwrap();
        assert!(v.get("content").unwrap().is_null());
        assert!(v.get("reasoning_content").is_none());
        assert_eq!(v["tool_calls"][0]["type"], "function");
        assert_eq!(v["tool_calls"][0]["function"]["name"], "get_weather");
    }

    #[test]
    fn completions_prompt_variants_parse() {
        // Single string.
        let r: CompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "m", "prompt": "only one"
        }))
        .unwrap();
        assert!(matches!(r.prompt.clone().into_items().as_slice(),
            [PromptItem::Text(s)] if s == "only one"));
        r.generation_spec().unwrap();

        // Batch of strings → one item each.
        let r: CompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "m", "prompt": ["a", "b"]
        }))
        .unwrap();
        assert_eq!(r.prompt.into_items().len(), 2);

        // Single pre-tokenized id array.
        let r: CompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "m", "prompt": [1, 2, 3]
        }))
        .unwrap();
        assert!(matches!(r.prompt.into_items().as_slice(),
            [PromptItem::Tokens(ids)] if ids == &[1, 2, 3]));

        // Batch of id arrays.
        let r: CompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "m", "prompt": [[1, 2], [3]]
        }))
        .unwrap();
        let items = r.prompt.into_items();
        assert_eq!(items.len(), 2);
        assert!(matches!(&items[0], PromptItem::Tokens(ids) if ids == &[1, 2]));

        // echo is supported (handled at the response layer).
        let r: CompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "m", "prompt": "p", "echo": true
        }))
        .unwrap();
        assert!(r.generation_spec().unwrap().echo);

        // Completions logprobs is a count.
        let r: CompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "m", "prompt": "p", "logprobs": 3
        }))
        .unwrap();
        assert_eq!(r.generation_spec().unwrap().logprobs, Some(3));
    }
}
