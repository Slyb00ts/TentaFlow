// =============================================================================
// Plik: api/openai/anthropic.rs
// Opis: Zewnetrzne, zgodne z Anthropic Messages API endpointy (`POST /v1/messages`
//       + `/v1/messages/count_tokens`). Wspolistnieja z OpenAI `/v1/chat/completions`
//       na tym samym porcie: inna sciezka, inny naglowek auth (`x-api-key` +
//       `anthropic-version`). To czysta warstwa tlumaczaca Anthropic -> OpenAI ->
//       Anthropic ponad istniejaca sciezka chatu (router + ACL bez zmian).
// =============================================================================

use crate::api::openai::types::{
    ChatCompletionRequest, ChatCompletionResponse, ContentPart, ImageUrl, Message, MessageContent,
};
use crate::routing::router::Router;
use crate::routing::streaming::ChatFlowSelector;

use futures::{Stream, StreamExt};
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Bytes, Frame, Incoming};
use hyper::{Request, Response, StatusCode};
use serde::{Deserialize, Serialize};
use tracing::{error, warn};

use std::pin::Pin;
use std::sync::Arc;

use super::server::OpenAIBody;

// =============================================================================
// REQUEST (parsowanie Anthropic Messages API)
// =============================================================================

/// Request Anthropic `POST /v1/messages`. Pola nieobslugiwane przez OpenAI
/// (np. `top_k`) sa swiadomie pomijane przy tlumaczeniu.
#[derive(Debug, Clone, Deserialize)]
pub struct AnthropicMessagesRequest {
    pub model: String,

    /// Wymagane w Anthropic API — limit tokenow odpowiedzi.
    pub max_tokens: u32,

    pub messages: Vec<AnthropicMessage>,

    /// Systemowy prompt na poziomie requestu (string albo lista blokow text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<AnthropicSystem>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,

    /// Brak odpowiednika w OpenAI ChatCompletionRequest — pomijane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,

    #[serde(default)]
    pub stream: bool,
}

/// Request `POST /v1/messages/count_tokens` — `max_tokens` nie jest tu wymagane.
#[derive(Debug, Clone, Deserialize)]
pub struct AnthropicCountTokensRequest {
    #[allow(dead_code)]
    pub model: String,

    pub messages: Vec<AnthropicMessage>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<AnthropicSystem>,
}

/// Pole `system` — string albo lista blokow `{type:"text",text}`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum AnthropicSystem {
    Text(String),
    Blocks(Vec<AnthropicSystemBlock>),
}

#[derive(Debug, Clone, Deserialize)]
pub struct AnthropicSystemBlock {
    #[serde(default)]
    pub text: String,
}

impl AnthropicSystem {
    /// Splaszcza system prompt do pojedynczego stringa (bloki laczone "\n").
    fn flatten(&self) -> String {
        match self {
            AnthropicSystem::Text(s) => s.clone(),
            AnthropicSystem::Blocks(blocks) => blocks
                .iter()
                .map(|b| b.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

/// Wiadomosc Anthropic — `content` to string albo lista blokow (text/image).
#[derive(Debug, Clone, Deserialize)]
pub struct AnthropicMessage {
    pub role: String,
    pub content: AnthropicContent,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum AnthropicContent {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
}

/// Blok zawartosci — `text` albo `image` (base64 source). Inne typy odrzucamy
/// przy tlumaczeniu (fail-loud), bo nie maja odpowiednika w sciezce chatu.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicContentBlock {
    Text {
        text: String,
    },
    Image {
        source: AnthropicImageSource,
    },
}

#[derive(Debug, Clone, Deserialize)]
pub struct AnthropicImageSource {
    #[serde(rename = "type")]
    pub source_type: String,
    pub media_type: String,
    pub data: String,
}

// =============================================================================
// RESPONSE (Anthropic Messages — blocking)
// =============================================================================

#[derive(Debug, Clone, Serialize)]
pub struct AnthropicMessagesResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub message_type: String,
    pub role: String,
    pub content: Vec<AnthropicResponseBlock>,
    pub model: String,
    pub stop_reason: Option<String>,
    pub stop_sequence: Option<String>,
    pub usage: AnthropicUsage,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicResponseBlock {
    Text { text: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct AnthropicUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

// =============================================================================
// TLUMACZENIE REQUESTU: Anthropic -> OpenAI ChatCompletionRequest
// =============================================================================

/// Buduje OpenAI `ChatCompletionRequest` z requestu Anthropic. `system`
/// (top-level) trafia jako pierwsza wiadomosc roli "system"; bloki obrazow
/// mapowane sa na OpenAI `image_url` z data-URI, zeby vision dzialalo ta sama
/// sciezka. `top_k` nie ma odpowiednika i jest pomijane.
fn to_openai_request(
    req: AnthropicMessagesRequest,
    stream: bool,
) -> std::result::Result<ChatCompletionRequest, String> {
    let mut messages: Vec<Message> = Vec::with_capacity(req.messages.len() + 1);

    if let Some(system) = req.system.as_ref() {
        let text = system.flatten();
        if !text.is_empty() {
            messages.push(Message {
                role: "system".to_string(),
                content: Some(MessageContent::Text(text)),
                ..Default::default()
            });
        }
    }

    for msg in req.messages {
        messages.push(anthropic_message_to_openai(msg)?);
    }

    Ok(ChatCompletionRequest {
        model: req.model,
        messages,
        temperature: req.temperature,
        max_tokens: Some(req.max_tokens),
        top_p: req.top_p,
        frequency_penalty: None,
        presence_penalty: None,
        stop: req.stop_sequences,
        stream,
        stream_options: None,
        user: None,
        response_format: None,
        tools: None,
        tool_choice: None,
        n: None,
        memory_options: None,
        audio_input: None,
    })
}

/// Tlumaczy pojedyncza wiadomosc Anthropic na OpenAI. String -> `Text`,
/// lista blokow -> `Parts` (text + image_url data-URI).
fn anthropic_message_to_openai(msg: AnthropicMessage) -> std::result::Result<Message, String> {
    let content = match msg.content {
        AnthropicContent::Text(s) => MessageContent::Text(s),
        AnthropicContent::Blocks(blocks) => {
            let mut parts: Vec<ContentPart> = Vec::with_capacity(blocks.len());
            for block in blocks {
                match block {
                    AnthropicContentBlock::Text { text } => {
                        parts.push(ContentPart::Text { text });
                    }
                    AnthropicContentBlock::Image { source } => {
                        if source.source_type != "base64" {
                            return Err(format!(
                                "image source.type '{}' nieobslugiwany (tylko 'base64')",
                                source.source_type
                            ));
                        }
                        let url =
                            format!("data:{};base64,{}", source.media_type, source.data);
                        parts.push(ContentPart::ImageUrl {
                            image_url: ImageUrl { url, detail: None },
                        });
                    }
                }
            }
            MessageContent::Parts(parts)
        }
    };

    Ok(Message {
        role: msg.role,
        content: Some(content),
        ..Default::default()
    })
}

// =============================================================================
// TLUMACZENIE ODPOWIEDZI: OpenAI -> Anthropic (blocking)
// =============================================================================

/// Mapuje OpenAI `finish_reason` na Anthropic `stop_reason`. `stop` przy
/// dopasowanej sekwencji stop -> `stop_sequence`; inaczej `stop` -> `end_turn`.
fn map_stop_reason(finish_reason: Option<&str>, matched_stop: bool) -> &'static str {
    match finish_reason {
        Some("length") => "max_tokens",
        Some("stop") if matched_stop => "stop_sequence",
        _ => "end_turn",
    }
}

/// Wyciaga plaski tekst z OpenAI `MessageContent` (string albo bloki text).
fn extract_text(content: Option<&MessageContent>) -> String {
    match content {
        Some(MessageContent::Text(s)) => s.clone(),
        Some(MessageContent::Parts(parts)) => parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                ContentPart::ImageUrl { .. } => None,
            })
            .collect::<Vec<_>>()
            .join(""),
        None => String::new(),
    }
}

/// Buduje odpowiedz Anthropic z odpowiedzi OpenAI. `matched_stop` rozroznia
/// `stop_sequence` od `end_turn`, ale OpenAI nie raportuje konkretnej sekwencji
/// — `stop_sequence` w odpowiedzi pozostaje `null` (jak w Anthropic gdy brak
/// dopasowania na poziomie API).
fn to_anthropic_response(
    resp: ChatCompletionResponse,
    stop_sequences: Option<&[String]>,
) -> AnthropicMessagesResponse {
    let choice = resp.choices.into_iter().next();
    let (text, finish_reason) = match choice {
        Some(c) => (extract_text(c.message.content.as_ref()), c.finish_reason),
        None => (String::new(), None),
    };

    let matched_stop = finish_reason.as_deref() == Some("stop")
        && stop_sequences.map(|s| !s.is_empty()).unwrap_or(false);
    let stop_reason = map_stop_reason(finish_reason.as_deref(), matched_stop);

    let usage = resp
        .usage
        .map(|u| AnthropicUsage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
        })
        .unwrap_or(AnthropicUsage {
            input_tokens: 0,
            output_tokens: 0,
        });

    AnthropicMessagesResponse {
        id: format!("msg_{}", uuid::Uuid::new_v4().simple()),
        message_type: "message".to_string(),
        role: "assistant".to_string(),
        content: vec![AnthropicResponseBlock::Text { text }],
        model: resp.model,
        stop_reason: Some(stop_reason.to_string()),
        stop_sequence: None,
        usage,
    }
}

// =============================================================================
// HANDLERY HTTP
// =============================================================================

fn error_response(status: StatusCode, error_type: &str, message: String) -> Response<OpenAIBody> {
    // Anthropic-ksztaltowy blad: { "type":"error", "error":{ "type", "message" } }.
    let body = serde_json::json!({
        "type": "error",
        "error": { "type": error_type, "message": message }
    });
    let bytes = serde_json::to_vec(&body).unwrap_or_default();
    json_response(status, bytes)
}

fn json_response(status: StatusCode, body: Vec<u8>) -> Response<OpenAIBody> {
    let stream = futures::stream::once(async move { Ok(Frame::data(Bytes::from(body))) });
    let boxed: Pin<
        Box<dyn Stream<Item = std::result::Result<Frame<Bytes>, std::io::Error>> + Send>,
    > = Box::pin(stream);
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(StreamBody::new(boxed))
        .unwrap()
}

/// Handler `POST /v1/messages` — blocking + streaming. Reuzywa
/// `Router::route_chat_completion(_stream)` (ta sama sciezka co OpenAI), tylko
/// tlumaczy ksztalt request/response/SSE.
pub async fn handle_messages(
    req: Request<Incoming>,
    router: Arc<Router>,
) -> std::result::Result<Response<OpenAIBody>, hyper::Error> {
    let user_ctx = req
        .extensions()
        .get::<crate::auth::acl::UserContext>()
        .cloned();
    let principal = req
        .extensions()
        .get::<crate::auth::acl::Principal>()
        .cloned();

    let body_bytes = req.collect().await?.to_bytes();

    let request: AnthropicMessagesRequest = match serde_json::from_slice(&body_bytes) {
        Ok(r) => r,
        Err(e) => {
            warn!("Anthropic /v1/messages: niepoprawny JSON: {}", e);
            return Ok(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                format!("Niepoprawny JSON: {}", e),
            ));
        }
    };

    // Brama /v1: ACL per-Principal (404 model_not_found jak na sciezce OpenAI).
    if let Err(resp) =
        super::server::v1_authorize_public(&router, principal.as_ref(), &request.model)
    {
        return Ok(resp);
    }

    let model = request.model.clone();
    let stop_sequences = request.stop_sequences.clone();
    let is_streaming = request.stream;

    if is_streaming {
        let openai_req = match to_openai_request(request, true) {
            Ok(r) => r,
            Err(msg) => {
                return Ok(error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_request_error",
                    msg,
                ));
            }
        };

        match router
            .route_chat_completion_stream(openai_req, user_ctx, None, ChatFlowSelector::Auto)
            .await
        {
            Ok(route_result) => Ok(anthropic_sse_response(route_result.response, model)),
            Err(e) => {
                error!("Anthropic /v1/messages stream: {}", e);
                Ok(error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "api_error",
                    e.to_string(),
                ))
            }
        }
    } else {
        let openai_req = match to_openai_request(request, false) {
            Ok(r) => r,
            Err(msg) => {
                return Ok(error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_request_error",
                    msg,
                ));
            }
        };

        match router.route_chat_completion(openai_req, user_ctx, None).await {
            Ok(route_result) => {
                let anthropic =
                    to_anthropic_response(route_result.response, stop_sequences.as_deref());
                let body = serde_json::to_vec(&anthropic).unwrap_or_default();
                Ok(json_response(StatusCode::OK, body))
            }
            Err(e) => {
                error!("Anthropic /v1/messages: {}", e);
                Ok(error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "api_error",
                    e.to_string(),
                ))
            }
        }
    }
}

/// Buduje strumien SSE w formacie Anthropic z OpenAI chunk-stream'a. Sekwencja:
/// `message_start` -> `content_block_start` -> N* `content_block_delta` ->
/// `content_block_stop` -> `message_delta` -> `message_stop`.
fn anthropic_sse_response(
    chunk_stream: Pin<
        Box<
            dyn Stream<
                    Item = crate::error::Result<crate::api::openai::types::ChatCompletionChunk>,
                > + Send,
        >,
    >,
    model: String,
) -> Response<OpenAIBody> {
    let msg_id = format!("msg_{}", uuid::Uuid::new_v4().simple());

    let message_start = serde_json::json!({
        "type": "message_start",
        "message": {
            "id": msg_id,
            "type": "message",
            "role": "assistant",
            "content": [],
            "model": model,
            "stop_reason": null,
            "stop_sequence": null,
            "usage": { "input_tokens": 0, "output_tokens": 0 }
        }
    });
    let content_block_start = serde_json::json!({
        "type": "content_block_start",
        "index": 0,
        "content_block": { "type": "text", "text": "" }
    });

    let prefix = futures::stream::iter(vec![
        Ok(sse_frame("message_start", &message_start)),
        Ok(sse_frame("content_block_start", &content_block_start)),
    ]);

    // Stan finalu dzielony miedzy body-stream a suffix: po wyczerpaniu chunkow
    // suffix czyta zaakumulowany OpenAI finish_reason + output_tokens i emituje
    // `message_delta`. OpenAI normalizuje reasoning -> content w streamie tej
    // samej sciezki, wiec dla delty czytamy `delta.content`.
    let final_state = Arc::new(parking_lot::Mutex::new((None::<String>, 0u32)));
    let state_for_body = final_state.clone();

    let body_stream = chunk_stream.flat_map(move |chunk_result| {
        let frames: Vec<std::result::Result<Frame<Bytes>, std::io::Error>> = match chunk_result {
            Ok(chunk) => {
                if let Some(usage) = chunk.usage.as_ref() {
                    state_for_body.lock().1 = usage.completion_tokens;
                }
                let mut out = Vec::new();
                for choice in &chunk.choices {
                    if let Some(fr) = choice.finish_reason.as_ref() {
                        state_for_body.lock().0 = Some(fr.clone());
                    }
                    let text = choice
                        .delta
                        .content
                        .as_ref()
                        .or(choice.delta.reasoning_content.as_ref());
                    if let Some(text) = text {
                        if !text.is_empty() {
                            let ev = serde_json::json!({
                                "type": "content_block_delta",
                                "index": 0,
                                "delta": { "type": "text_delta", "text": text }
                            });
                            out.push(Ok(sse_frame("content_block_delta", &ev)));
                        }
                    }
                }
                out
            }
            Err(e) => {
                error!("Anthropic SSE chunk: {}", e);
                let ev = serde_json::json!({
                    "type": "error",
                    "error": { "type": "api_error", "message": e.to_string() }
                });
                vec![Ok(sse_frame("error", &ev))]
            }
        };
        futures::stream::iter(frames)
    });

    let suffix = futures::stream::once(async move {
        let (finish_reason, output_tokens) = {
            let guard = final_state.lock();
            (guard.0.clone(), guard.1)
        };
        // Brak stop_sequence po stronie OpenAI -> "stop" mapujemy na end_turn.
        let stop_reason = map_stop_reason(finish_reason.as_deref(), false);
        futures::stream::iter(finalize_frames(stop_reason, output_tokens))
    });

    let combined = prefix.chain(body_stream).chain(suffix.flatten());

    let boxed: Pin<
        Box<dyn Stream<Item = std::result::Result<Frame<Bytes>, std::io::Error>> + Send>,
    > = Box::pin(combined);

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("Connection", "keep-alive")
        .body(StreamBody::new(boxed))
        .unwrap()
}

/// Buduje finalowe ramki SSE (`content_block_stop` -> `message_delta` ->
/// `message_stop`).
fn finalize_frames(
    stop_reason: &str,
    output_tokens: u32,
) -> Vec<std::result::Result<Frame<Bytes>, std::io::Error>> {
    let content_block_stop = serde_json::json!({
        "type": "content_block_stop",
        "index": 0
    });
    let message_delta = serde_json::json!({
        "type": "message_delta",
        "delta": { "stop_reason": stop_reason, "stop_sequence": null },
        "usage": { "output_tokens": output_tokens }
    });
    let message_stop = serde_json::json!({ "type": "message_stop" });
    vec![
        Ok(sse_frame("content_block_stop", &content_block_stop)),
        Ok(sse_frame("message_delta", &message_delta)),
        Ok(sse_frame("message_stop", &message_stop)),
    ]
}

/// Formatuje pojedyncze zdarzenie SSE: `event: <type>\ndata: <json>\n\n`.
fn sse_frame(event: &str, data: &serde_json::Value) -> Frame<Bytes> {
    let json = serde_json::to_string(data).unwrap_or_default();
    let line = format!("event: {}\ndata: {}\n\n", event, json);
    Frame::data(Bytes::from(line))
}

/// Handler `POST /v1/messages/count_tokens` — zwraca `{"input_tokens": N}`.
/// Estymacja przez ten sam licznik co metryki (chars/4); bloki text liczone,
/// bloki image pomijane (brak tokenizera wizyjnego w tej sciezce).
pub async fn handle_count_tokens(
    req: Request<Incoming>,
    router: Arc<Router>,
) -> std::result::Result<Response<OpenAIBody>, hyper::Error> {
    let principal = req
        .extensions()
        .get::<crate::auth::acl::Principal>()
        .cloned();

    let body_bytes = req.collect().await?.to_bytes();

    let request: AnthropicCountTokensRequest = match serde_json::from_slice(&body_bytes) {
        Ok(r) => r,
        Err(e) => {
            return Ok(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                format!("Niepoprawny JSON: {}", e),
            ));
        }
    };

    if let Err(resp) =
        super::server::v1_authorize_public(&router, principal.as_ref(), &request.model)
    {
        return Ok(resp);
    }

    let mut total = 0u64;
    if let Some(system) = request.system.as_ref() {
        total += crate::metrics::token_counter::estimate_tokens(&system.flatten());
    }
    for msg in &request.messages {
        total += match &msg.content {
            AnthropicContent::Text(s) => {
                crate::metrics::token_counter::estimate_tokens(s)
            }
            AnthropicContent::Blocks(blocks) => blocks
                .iter()
                .map(|b| match b {
                    AnthropicContentBlock::Text { text } => {
                        crate::metrics::token_counter::estimate_tokens(text)
                    }
                    AnthropicContentBlock::Image { .. } => 0,
                })
                .sum(),
        };
    }

    let body = serde_json::json!({ "input_tokens": total });
    let bytes = serde_json::to_vec(&body).unwrap_or_default();
    Ok(json_response(StatusCode::OK, bytes))
}
