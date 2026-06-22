// =============================================================================
// File: services/backend/codex.rs — ChatGPT subscription backend (Codex)
//
// Drives a ChatGPT Plus/Pro plan via OpenAI's Codex backend instead of a
// pay-per-token API key. Mirrors how OpenAI's own `codex` CLI authenticates and
// talks to `https://chatgpt.com/backend-api/codex/responses` using the Responses
// API. Credentials are the contents of `~/.codex/auth.json` (access_token +
// refresh_token + account_id); the access token is refreshed on expiry / 401
// through `https://auth.openai.com/oauth/token` with the public Codex client id.
//
// Endpoint, headers, request body, SSE event names and refresh flow were taken
// verbatim from the openai/codex source (codex-rs: model-provider-info,
// login/src/auth/manager.rs, codex-api/src/sse/responses.rs), not guessed.
// =============================================================================

use std::pin::Pin;

use base64::Engine;
use futures::stream::{Stream, StreamExt};
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tracing::warn;

use crate::api::openai::types::{
    ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse, Choice, ChunkChoice, Delta,
    Message, MessageContent,
};
use crate::api::openai::types::ContentPart;
use crate::error::{CoreError, Result};

/// Full Responses endpoint for the ChatGPT (Codex) backend.
pub const CHATGPT_CODEX_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
const OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
/// Public OAuth client id used by the Codex CLI for the token refresh flow.
const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// Parsed ChatGPT subscription credentials.
#[derive(Clone, Debug, Default)]
pub struct CodexCreds {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub account_id: Option<String>,
}

/// Lazily-initialised, refreshable credential cache held by a `BackendClient`.
pub type CodexCredsCache = RwLock<Option<CodexCreds>>;

/// Parse the credential blob the admin pasted. Accepts either the full
/// `~/.codex/auth.json` document (`{ "tokens": { access_token, refresh_token,
/// account_id, id_token } }`) or a bare access-token JWT (account id is then
/// decoded from the token's `https://api.openai.com/auth` claim).
pub fn parse_creds(blob: &str) -> CodexCreds {
    let trimmed = blob.trim();
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        let tokens = v.get("tokens").unwrap_or(&v);
        let access = tokens
            .get("access_token")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string();
        if !access.is_empty() {
            let refresh = tokens
                .get("refresh_token")
                .and_then(|x| x.as_str())
                .map(String::from);
            let mut account = tokens
                .get("account_id")
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from);
            if account.is_none() {
                if let Some(id) = tokens.get("id_token").and_then(|x| x.as_str()) {
                    account = account_id_from_jwt(id);
                }
            }
            if account.is_none() {
                account = account_id_from_jwt(&access);
            }
            return CodexCreds {
                access_token: access,
                refresh_token: refresh,
                account_id: account,
            };
        }
    }
    CodexCreds {
        account_id: account_id_from_jwt(trimmed),
        access_token: trimmed.to_string(),
        refresh_token: None,
    }
}

fn jwt_payload(jwt: &str) -> Option<Value> {
    let mut parts = jwt.split('.');
    let (_h, payload, _s) = (parts.next()?, parts.next()?, parts.next()?);
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub(crate) fn account_id_from_jwt(jwt: &str) -> Option<String> {
    jwt_payload(jwt)?
        .get("https://api.openai.com/auth")?
        .get("chatgpt_account_id")?
        .as_str()
        .map(String::from)
}

/// Best-effort human label (email) decoded from an OpenAI id token, for showing
/// which account just logged in.
pub(crate) fn email_from_jwt(jwt: &str) -> Option<String> {
    let payload = jwt_payload(jwt)?;
    payload
        .get("email")
        .and_then(Value::as_str)
        .or_else(|| {
            payload
                .get("https://api.openai.com/profile")
                .and_then(|p| p.get("email"))
                .and_then(Value::as_str)
        })
        .map(String::from)
}

/// True when the access token's `exp` is within 60s (or already past). When the
/// token has no decodable `exp` we assume it is still valid and let a 401 drive
/// the refresh instead.
fn token_needs_refresh(access_token: &str) -> bool {
    match jwt_payload(access_token).and_then(|p| p.get("exp").and_then(Value::as_i64)) {
        Some(exp) => exp - now_secs() as i64 <= 60,
        None => false,
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

async fn refresh(client: &reqwest::Client, creds: &mut CodexCreds) -> Result<()> {
    let refresh_token = creds.refresh_token.clone().ok_or_else(|| CoreError::BackendError {
        backend_url: OAUTH_TOKEN_URL.to_string(),
        message: "subscription access token expired and no refresh_token is present — re-run `codex login` and paste the full ~/.codex/auth.json".to_string(),
        source: None,
    })?;
    let resp = client
        .post(OAUTH_TOKEN_URL)
        .header("Content-Type", "application/json")
        .json(&json!({
            "client_id": CODEX_CLIENT_ID,
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
        }))
        .send()
        .await
        .map_err(|e| CoreError::NetworkError {
            message: format!("Codex token refresh: {e}"),
            source: e.into(),
        })?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(CoreError::BackendError {
            backend_url: OAUTH_TOKEN_URL.to_string(),
            message: format!("Codex token refresh failed ({status}): {body}"),
            source: None,
        }
        .into());
    }
    let body: Value = resp.json().await.map_err(|e| CoreError::BackendError {
        backend_url: OAUTH_TOKEN_URL.to_string(),
        message: format!("Codex token refresh: bad response: {e}"),
        source: Some(e.into()),
    })?;
    if let Some(at) = body.get("access_token").and_then(Value::as_str) {
        creds.access_token = at.to_string();
    }
    if let Some(rt) = body.get("refresh_token").and_then(Value::as_str) {
        creds.refresh_token = Some(rt.to_string());
    }
    if creds.account_id.is_none() {
        if let Some(id) = body.get("id_token").and_then(Value::as_str) {
            creds.account_id = account_id_from_jwt(id);
        }
    }
    Ok(())
}

/// Return valid (refreshed if needed) credentials, initialising the cache from
/// `blob` on first use.
async fn ensure_fresh(
    client: &reqwest::Client,
    cache: &CodexCredsCache,
    blob: &str,
) -> Result<CodexCreds> {
    {
        let guard = cache.read().await;
        if let Some(c) = guard.as_ref() {
            if !c.access_token.is_empty() && !token_needs_refresh(&c.access_token) {
                return Ok(c.clone());
            }
        }
    }
    let mut guard = cache.write().await;
    let creds = guard.get_or_insert_with(|| parse_creds(blob));
    if creds.access_token.is_empty() {
        *creds = parse_creds(blob);
    }
    if token_needs_refresh(&creds.access_token) {
        refresh(client, creds).await?;
    }
    Ok(creds.clone())
}

/// Force a refresh after a 401 and return the new credentials.
async fn refresh_after_unauthorized(
    client: &reqwest::Client,
    cache: &CodexCredsCache,
    blob: &str,
) -> Result<CodexCreds> {
    let mut guard = cache.write().await;
    let creds = guard.get_or_insert_with(|| parse_creds(blob));
    refresh(client, creds).await?;
    Ok(creds.clone())
}

fn message_text(m: &Message) -> String {
    match &m.content {
        Some(MessageContent::Text(t)) => t.clone(),
        Some(MessageContent::Parts(parts)) => parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.clone()),
                ContentPart::ImageUrl { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        None => String::new(),
    }
}

/// Convert an OpenAI chat request into the Responses API body the Codex backend
/// expects. System/developer turns become top-level `instructions` (the backend
/// rejects requests without them); the rest become `input` message items.
fn build_request_body(request: &ChatCompletionRequest) -> Value {
    let mut instructions = String::new();
    let mut input: Vec<Value> = Vec::new();
    for m in &request.messages {
        let text = message_text(m);
        match m.role.as_str() {
            "system" | "developer" => {
                if !instructions.is_empty() {
                    instructions.push('\n');
                }
                instructions.push_str(&text);
            }
            "assistant" => input.push(json!({
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": text }],
            })),
            _ => input.push(json!({
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": text }],
            })),
        }
    }
    if instructions.trim().is_empty() {
        instructions = "You are a helpful assistant.".to_string();
    }
    json!({
        "model": request.model,
        "instructions": instructions,
        "input": input,
        "tools": [],
        "tool_choice": "auto",
        "parallel_tool_calls": false,
        "store": false,
        "stream": true,
        "include": [],
    })
}

fn apply_headers(req: reqwest::RequestBuilder, creds: &CodexCreds) -> reqwest::RequestBuilder {
    let mut req = req
        .header("Authorization", format!("Bearer {}", creds.access_token))
        .header("OpenAI-Beta", "responses=experimental")
        .header("originator", "codex_cli_rs")
        .header("session_id", uuid::Uuid::new_v4().to_string())
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream");
    if let Some(acc) = creds.account_id.as_ref().filter(|s| !s.is_empty()) {
        req = req.header("ChatGPT-Account-ID", acc.clone());
    }
    req
}

/// Extract the assistant text from a fully-buffered Responses SSE body.
fn collect_output_text(sse_body: &str) -> String {
    let mut out = String::new();
    for line in sse_body.lines() {
        let Some(data) = line.trim().strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        if let Ok(ev) = serde_json::from_str::<Value>(data) {
            if ev.get("type").and_then(Value::as_str) == Some("response.output_text.delta") {
                if let Some(d) = ev.get("delta").and_then(Value::as_str) {
                    out.push_str(d);
                }
            }
        }
    }
    out
}

fn build_chat_response(model: &str, text: &str) -> ChatCompletionResponse {
    ChatCompletionResponse {
        id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
        object: "chat.completion".to_string(),
        created: now_secs(),
        model: model.to_string(),
        choices: vec![Choice {
            index: 0,
            message: Message {
                role: "assistant".to_string(),
                content: Some(MessageContent::Text(text.to_string())),
                ..Default::default()
            },
            finish_reason: Some("stop".to_string()),
            logprobs: None,
        }],
        usage: None,
        system_fingerprint: None,
        transcribed_text: None,
        speaker_id: None,
        speaker_name: None,
        speaker_confidence: None,
        detected_intent: None,
        detected_tools: None,
    }
}

const CHATGPT_CODEX_MODELS_URL: &str = "https://chatgpt.com/backend-api/codex/models";

/// List the models available to this ChatGPT plan via the Codex backend (the
/// subscription token is rejected by the standard `/v1/models`). Returns the
/// normalized `ProviderModel` shape the model picker consumes.
pub async fn list_models(blob: &str) -> Result<Vec<crate::services::providers::ProviderModel>> {
    let client = reqwest::Client::new();
    let mut creds = parse_creds(blob);
    if creds.access_token.is_empty() {
        return Err(CoreError::BackendError {
            backend_url: CHATGPT_CODEX_MODELS_URL.to_string(),
            message: "no subscription access token — sign in again".to_string(),
            source: None,
        }
        .into());
    }
    if token_needs_refresh(&creds.access_token) {
        refresh(&client, &mut creds).await?;
    }

    let send = |creds: &CodexCreds| {
        let mut req = client
            .get(CHATGPT_CODEX_MODELS_URL)
            .header("Authorization", format!("Bearer {}", creds.access_token))
            .header("OpenAI-Beta", "responses=experimental")
            .header("originator", "codex_cli_rs")
            .header("Accept", "application/json");
        if let Some(acc) = creds.account_id.as_ref().filter(|s| !s.is_empty()) {
            req = req.header("ChatGPT-Account-ID", acc.clone());
        }
        req.send()
    };

    let mut resp = send(&creds).await.map_err(|e| CoreError::NetworkError {
        message: format!("Codex models request: {e}"),
        source: e.into(),
    })?;
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        refresh(&client, &mut creds).await?;
        resp = send(&creds).await.map_err(|e| CoreError::NetworkError {
            message: format!("Codex models request (retry): {e}"),
            source: e.into(),
        })?;
    }
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(CoreError::BackendError {
            backend_url: CHATGPT_CODEX_MODELS_URL.to_string(),
            message: format!("Codex models returned {status}: {body}"),
            source: None,
        }
        .into());
    }
    let v: Value = resp.json().await.map_err(|e| CoreError::BackendError {
        backend_url: CHATGPT_CODEX_MODELS_URL.to_string(),
        message: format!("Codex models bad response: {e}"),
        source: Some(e.into()),
    })?;
    let arr = v
        .get("models")
        .or_else(|| v.get("data"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let models = arr
        .iter()
        .filter_map(|m| m.get("id").and_then(Value::as_str))
        .map(|id| crate::services::providers::ProviderModel {
            id: id.to_string(),
            display_name: None,
            modality: "chat".to_string(),
            context_length: None,
        })
        .collect();
    Ok(models)
}

/// Blocking (non-streaming) chat completion against the Codex backend. The
/// backend always streams, so we buffer the SSE body and fold the text deltas.
pub async fn chat_completion(
    client: &reqwest::Client,
    endpoint: &str,
    cache: &CodexCredsCache,
    blob: &str,
    request: &ChatCompletionRequest,
) -> Result<ChatCompletionResponse> {
    let body = build_request_body(request);
    let mut creds = ensure_fresh(client, cache, blob).await?;

    let mut resp = apply_headers(client.post(endpoint), &creds)
        .json(&body)
        .send()
        .await
        .map_err(|e| CoreError::NetworkError {
            message: format!("Codex responses request: {e}"),
            source: e.into(),
        })?;

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        creds = refresh_after_unauthorized(client, cache, blob).await?;
        resp = apply_headers(client.post(endpoint), &creds)
            .json(&body)
            .send()
            .await
            .map_err(|e| CoreError::NetworkError {
                message: format!("Codex responses request (retry): {e}"),
                source: e.into(),
            })?;
    }

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(CoreError::BackendError {
            backend_url: endpoint.to_string(),
            message: format!("Codex responses returned {status}: {text}"),
            source: None,
        }
        .into());
    }

    let raw = resp.text().await.map_err(|e| CoreError::BackendError {
        backend_url: endpoint.to_string(),
        message: format!("Codex responses: read body: {e}"),
        source: Some(e.into()),
    })?;
    Ok(build_chat_response(&request.model, &collect_output_text(&raw)))
}

fn role_chunk(id: &str, model: &str, created: u64) -> ChatCompletionChunk {
    chunk(id, model, created, Some("assistant".to_string()), None, None)
}

fn content_chunk(id: &str, model: &str, created: u64, delta: &str) -> ChatCompletionChunk {
    chunk(id, model, created, None, Some(delta.to_string()), None)
}

fn finish_chunk(id: &str, model: &str, created: u64) -> ChatCompletionChunk {
    chunk(id, model, created, None, None, Some("stop".to_string()))
}

fn chunk(
    id: &str,
    model: &str,
    created: u64,
    role: Option<String>,
    content: Option<String>,
    finish_reason: Option<String>,
) -> ChatCompletionChunk {
    ChatCompletionChunk {
        id: id.to_string(),
        object: "chat.completion.chunk".to_string(),
        created,
        model: model.to_string(),
        choices: vec![ChunkChoice {
            index: 0,
            delta: Delta {
                role,
                content,
                reasoning_content: None,
                tool_calls: None,
            },
            finish_reason,
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
    }
}

/// Streaming chat completion against the Codex backend. Maps Responses SSE
/// events to OpenAI chat-completion chunks (role → text deltas → stop).
pub async fn chat_completion_stream(
    client: &reqwest::Client,
    endpoint: &str,
    cache: &CodexCredsCache,
    blob: &str,
    request: &ChatCompletionRequest,
) -> Result<Pin<Box<dyn Stream<Item = Result<ChatCompletionChunk>> + Send>>> {
    let body = build_request_body(request);
    let creds = ensure_fresh(client, cache, blob).await?;

    let resp = apply_headers(client.post(endpoint), &creds)
        .json(&body)
        .send()
        .await
        .map_err(|e| CoreError::NetworkError {
            message: format!("Codex responses stream: {e}"),
            source: e.into(),
        })?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(CoreError::BackendError {
            backend_url: endpoint.to_string(),
            message: format!("Codex responses stream returned {status}: {text}"),
            source: None,
        }
        .into());
    }

    let model = request.model.clone();
    let id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
    let created = now_secs();
    let byte_stream = resp.bytes_stream();

    let stream = async_stream::stream! {
        yield Ok(role_chunk(&id, &model, created));
        let mut buffer = String::new();
        futures::pin_mut!(byte_stream);
        while let Some(chunk_result) = byte_stream.next().await {
            let bytes = match chunk_result {
                Ok(b) => b,
                Err(e) => {
                    yield Err(CoreError::NetworkError {
                        message: format!("Codex stream read: {e}"),
                        source: e.into(),
                    }
                    .into());
                    return;
                }
            };
            buffer.push_str(&String::from_utf8_lossy(&bytes));
            while let Some(nl) = buffer.find('\n') {
                let line: String = buffer[..nl].trim().to_string();
                buffer.drain(..=nl);
                let Some(data) = line.strip_prefix("data:") else { continue };
                let data = data.trim();
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }
                let Ok(ev) = serde_json::from_str::<Value>(data) else { continue };
                match ev.get("type").and_then(Value::as_str) {
                    Some("response.output_text.delta") => {
                        if let Some(d) = ev.get("delta").and_then(Value::as_str) {
                            yield Ok(content_chunk(&id, &model, created, d));
                        }
                    }
                    Some("response.completed") => {
                        yield Ok(finish_chunk(&id, &model, created));
                        return;
                    }
                    Some("response.failed") => {
                        warn!("Codex response.failed: {ev}");
                        yield Err(CoreError::BackendError {
                            backend_url: CHATGPT_CODEX_RESPONSES_URL.to_string(),
                            message: format!("Codex response.failed: {ev}"),
                            source: None,
                        }
                        .into());
                        return;
                    }
                    _ => {}
                }
            }
        }
        // Stream ended without an explicit response.completed — emit a stop.
        yield Ok(finish_chunk(&id, &model, created));
    };

    Ok(Box::pin(stream))
}
