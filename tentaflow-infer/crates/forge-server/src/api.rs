// ===== File: api.rs — OpenAI request/response types + pure request validation =====
// Everything here is transport-only and unit-testable without a GPU: parse
// the wire shapes, validate them, and produce the sampling spec the engine
// consumes. Unknown request fields (frequency_penalty, presence_penalty,
// logit_bias, ...) are accepted and ignored by serde's default behavior.

use forge_engine::sample::SamplingParams;
use forge_tokenize::ChatMessage;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;

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
    /// OpenAI tool definitions, passed verbatim to the chat template.
    #[serde(default)]
    pub tools: Option<serde_json::Value>,
    /// "auto" (default), "none", or an unsupported forcing spec.
    #[serde(default)]
    pub tool_choice: Option<serde_json::Value>,
}

/// How this request wants tool calling handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolMode {
    /// Render tools into the template and parse tool calls from the output.
    Auto,
    /// Do not render tools and do not parse tool calls.
    None,
}

/// `prompt` accepts a string or an array; only a single prompt is served.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum PromptSpec {
    One(String),
    Many(Vec<String>),
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
}

/// Engine-facing generation parameters extracted from a validated request.
#[derive(Debug, Clone)]
pub struct GenerationSpec {
    pub sampling: SamplingParams,
    pub max_tokens: usize,
    pub stop: Vec<String>,
}

// Requests without max_tokens still need a bound: the engine's admission
// control projects prompt+max_tokens against KV pages, so "unlimited" would
// starve the queue.
const DEFAULT_MAX_TOKENS: usize = 1024;

#[allow(clippy::too_many_arguments)]
fn generation_spec(
    n: Option<u32>,
    temperature: Option<f32>,
    top_p: Option<f32>,
    top_k: Option<usize>,
    min_p: Option<f32>,
    repetition_penalty: Option<f32>,
    seed: Option<u64>,
    max_tokens: Option<usize>,
    max_completion_tokens: Option<usize>,
    stop: Option<StopSpec>,
) -> Result<GenerationSpec, ApiError> {
    if let Some(n) = n {
        if n == 0 {
            return Err(ApiError::invalid_request("n must be at least 1"));
        }
        if n > 1 {
            return Err(ApiError::invalid_request(
                "n > 1 is not supported by this server",
            ));
        }
    }
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
    sampling.seed = seed;

    let max_tokens = max_tokens
        .or(max_completion_tokens)
        .unwrap_or(DEFAULT_MAX_TOKENS);
    if max_tokens == 0 {
        return Err(ApiError::invalid_request("max_tokens must be at least 1"));
    }

    Ok(GenerationSpec {
        sampling,
        max_tokens,
        stop: stop.map(StopSpec::into_vec).unwrap_or_default(),
    })
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
        generation_spec(
            self.n,
            self.temperature,
            self.top_p,
            self.top_k,
            self.min_p,
            self.repetition_penalty,
            self.seed,
            self.max_tokens,
            self.max_completion_tokens,
            self.stop.clone(),
        )
    }

    /// Validate `tool_choice`. Only "auto" and "none" are implemented;
    /// "required" and named function forcing need constrained decoding
    /// (SPEC §8.1.1) and are rejected honestly instead of being ignored.
    pub fn tool_mode(&self) -> Result<ToolMode, ApiError> {
        let has_tools = match &self.tools {
            None | Some(serde_json::Value::Null) => false,
            Some(serde_json::Value::Array(a)) => !a.is_empty(),
            Some(_) => {
                return Err(ApiError::invalid_request(
                    "tools must be an array of tool definitions",
                ))
            }
        };
        match &self.tool_choice {
            None => {}
            Some(serde_json::Value::String(s)) => match s.as_str() {
                "auto" => {}
                "none" => return Ok(ToolMode::None),
                "required" => {
                    return Err(ApiError::invalid_request(
                        "tool_choice \"required\" is not implemented by this server",
                    ))
                }
                other => {
                    return Err(ApiError::invalid_request(format!(
                        "tool_choice {other:?} is not one of \"auto\", \"none\""
                    )))
                }
            },
            Some(_) => {
                return Err(ApiError::invalid_request(
                    "named tool_choice forcing is not implemented by this server",
                ))
            }
        }
        Ok(if has_tools { ToolMode::Auto } else { ToolMode::None })
    }
}

impl CompletionRequest {
    pub fn generation_spec(&self) -> Result<GenerationSpec, ApiError> {
        if self.echo == Some(true) {
            return Err(ApiError::invalid_request(
                "echo is not supported by this server",
            ));
        }
        generation_spec(
            self.n,
            self.temperature,
            self.top_p,
            self.top_k,
            self.min_p,
            self.repetition_penalty,
            self.seed,
            self.max_tokens,
            self.max_completion_tokens,
            self.stop.clone(),
        )
    }

    /// The single prompt string this server accepts.
    pub fn single_prompt(&self) -> Result<&str, ApiError> {
        match &self.prompt {
            PromptSpec::One(s) => Ok(s),
            PromptSpec::Many(v) if v.len() == 1 => Ok(&v[0]),
            PromptSpec::Many(_) => Err(ApiError::invalid_request(
                "prompt arrays with more than one entry are not supported",
            )),
        }
    }
}

// ---- Response shapes ----

#[derive(Debug, Serialize)]
pub struct Usage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
}

impl Usage {
    pub fn new(prompt_tokens: usize, completion_tokens: usize) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
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

#[derive(Debug, Serialize)]
pub struct ChatChoice {
    pub index: u32,
    pub message: ChatResponseMessage,
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

#[derive(Debug, Serialize)]
pub struct CompletionChoice {
    pub index: u32,
    pub text: String,
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
        assert!(matches!(r.input.into_items().as_slice(), [EmbItem::Tokens(ids)] if ids == &[1, 2, 3]));
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
        let r = embed_req(
            serde_json::json!({"model": "m", "input": "x", "encoding_format": "base64"}),
        );
        assert_eq!(r.encoding().unwrap(), EncodingFormat::Base64);
        let r = embed_req(
            serde_json::json!({"model": "m", "input": "x", "encoding_format": "yaml"}),
        );
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
    fn n_greater_than_one_is_rejected() {
        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}], "n": 2
        }));
        let err = r.generation_spec().unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
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
    fn ignored_openai_fields_are_accepted() {
        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}],
            "frequency_penalty": 0.5, "presence_penalty": 0.1,
            "logit_bias": {"5": -100}, "user": "abc"
        }));
        r.generation_spec().unwrap();
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

        // Forcing modes are honestly rejected, not silently ignored.
        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}],
            "tools": tools, "tool_choice": "required"
        }));
        assert_eq!(
            r.tool_mode().unwrap_err().status,
            axum::http::StatusCode::BAD_REQUEST
        );
        let r = chat_req(serde_json::json!({
            "model": "m", "messages": [{"role": "user", "content": "hi"}],
            "tools": tools,
            "tool_choice": {"type": "function", "function": {"name": "get_weather"}}
        }));
        assert_eq!(
            r.tool_mode().unwrap_err().status,
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
    fn completions_prompt_and_echo_rules() {
        let r: CompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "m", "prompt": ["only one"]
        }))
        .unwrap();
        assert_eq!(r.single_prompt().unwrap(), "only one");
        r.generation_spec().unwrap();

        let r: CompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "m", "prompt": ["a", "b"]
        }))
        .unwrap();
        assert!(r.single_prompt().is_err());

        let r: CompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "m", "prompt": "p", "echo": true
        }))
        .unwrap();
        assert!(r.generation_spec().is_err());
    }
}
