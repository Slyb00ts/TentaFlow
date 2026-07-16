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
    pub seed: Option<u64>,
    #[serde(default)]
    pub n: Option<u32>,
    #[serde(default)]
    pub repetition_penalty: Option<f32>,
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
            if m.text_content().is_none() {
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

#[derive(Debug, Serialize)]
pub struct ChatResponseMessage {
    pub role: &'static str,
    pub content: String,
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
