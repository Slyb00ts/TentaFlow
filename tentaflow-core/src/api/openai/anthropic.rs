// =============================================================================
// Plik: api/openai/anthropic.rs
// Opis: Zewnetrzne, zgodne z Anthropic Messages API endpointy (`POST /v1/messages`
//       + `/v1/messages/count_tokens`). Wspolistnieja z OpenAI `/v1/chat/completions`
//       na tym samym porcie: inna sciezka, inny naglowek auth (`x-api-key` +
//       `anthropic-version`). To czysta warstwa tlumaczaca Anthropic -> OpenAI ->
//       Anthropic ponad istniejaca sciezka chatu (router + ACL bez zmian).
// =============================================================================

use crate::api::openai::types::{
    ChatCompletionRequest, ChatCompletionResponse, ContentPart, FunctionCall, FunctionDefinition,
    ImageUrl, Message, MessageContent, Tool, ToolCall, ToolChoice, ToolChoiceFunction,
    ToolChoiceObject,
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

    /// Deklaracje narzedzi (Anthropic `input_schema` == OpenAI
    /// `function.parameters`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<AnthropicTool>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<AnthropicToolChoice>,

    #[serde(default)]
    pub stream: bool,

    /// Anthropic extended thinking config (`{"type":"enabled","budget_tokens":N}`).
    /// Parsujemy dla diagnostyki; OpenAI-compat backendy nie mają odpowiednika.
    #[serde(default, rename = "thinking")]
    pub thinking_config: Option<serde_json::Value>,
}

/// Deklaracja narzedzia. Anthropic dopuszcza tez narzedzia serwerowe
/// (`type: "web_search_20250305"` itd.) — te nie maja odpowiednika w OpenAI
/// function calling i sa pomijane przy tlumaczeniu (brak `input_schema`).
#[derive(Debug, Clone, Deserialize)]
pub struct AnthropicTool {
    pub name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
}

/// `tool_choice` Anthropic: `{type:"auto"|"any"|"tool"|"none", name?}`.
#[derive(Debug, Clone, Deserialize)]
pub struct AnthropicToolChoice {
    #[serde(rename = "type")]
    pub choice_type: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Request `POST /v1/messages/count_tokens` — `max_tokens` nie jest tu wymagane.
#[derive(Debug, Clone, Deserialize)]
pub struct AnthropicCountTokensRequest {
    #[allow(dead_code)]
    pub model: String,

    pub messages: Vec<AnthropicMessage>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<AnthropicSystem>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<AnthropicTool>>,
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

/// Blok zawartosci wejsciowej. Poza `text`/`image` obslugujemy bloki
/// narzedziowe (`tool_use` w turze asystenta, `tool_result` w turze
/// uzytkownika) oraz bloki rozumowania, ktore klienci Anthropic odsylaja w
/// historii. Nieznane typy nie sa bledem requestu — sesja agenta trwa wiele
/// tur i twardy 400 zerwalby ja calkowicie, wiec sa logowane i pomijane.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicContentBlock {
    Text {
        text: String,
    },
    Image {
        source: AnthropicImageSource,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        #[serde(default)]
        content: Option<AnthropicToolResultContent>,
    },
    /// Rozumowanie modelu odsylane w historii. OpenAI-compatible reasoning
    /// backends expect it as `reasoning_content` on the assistant message.
    Thinking {
        #[serde(default)]
        thinking: String,
    },
    RedactedThinking {
        #[serde(default)]
        data: String,
    },
    #[serde(other)]
    Unsupported,
}

/// `tool_result.content` — string albo lista blokow (text/image).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum AnthropicToolResultContent {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
}

impl AnthropicToolResultContent {
    /// Splaszcza wynik narzedzia do tekstu. OpenAI `role:"tool"` przyjmuje
    /// wylacznie string, wiec bloki obrazow nie maja tu reprezentacji.
    fn flatten(&self) -> String {
        match self {
            AnthropicToolResultContent::Text(s) => s.clone(),
            AnthropicToolResultContent::Blocks(blocks) => {
                let mut parts: Vec<&str> = Vec::with_capacity(blocks.len());
                for block in blocks {
                    match block {
                        AnthropicContentBlock::Text { text } => parts.push(text.as_str()),
                        AnthropicContentBlock::Image { .. } => {
                            warn!("tool_result: blok image pominiety (OpenAI role=tool przyjmuje tylko tekst)");
                        }
                        _ => {}
                    }
                }
                parts.join("\n")
            }
        }
    }
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
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        /// The proxy-generated block has no provider signature. This marker
        /// keeps Anthropic clients from dropping the block during replay.
        signature: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
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
        anthropic_message_to_openai(msg, &mut messages)?;
    }

    let tools = to_openai_tools(req.tools.as_deref());
    // `tool_choice` bez `tools` nie ma sensu i czesc backendow OpenAI odrzuca
    // taki request bledem — wysylamy je tylko razem.
    let tool_choice = tools
        .as_ref()
        .and_then(|_| to_openai_tool_choice(req.tool_choice.as_ref()));

    Ok(ChatCompletionRequest {
        reasoning_effort: None,
        modalities: None,
        audio: None,
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
        tools,
        tool_choice,
        n: None,
        memory_options: None,
        audio_input: None,
    })
}

/// Tlumaczy deklaracje narzedzi Anthropic na OpenAI. `input_schema` to ten sam
/// JSON Schema co OpenAI `function.parameters`. Narzedzia serwerowe Anthropic
/// (bez `input_schema`) sa pomijane — nie da sie ich wyrazic jako funkcji.
fn to_openai_tools(tools: Option<&[AnthropicTool]>) -> Option<Vec<Tool>> {
    let tools = tools?;
    let mapped: Vec<Tool> = tools
        .iter()
        .filter_map(|t| {
            let Some(schema) = t.input_schema.as_ref() else {
                warn!(
                    "narzedzie '{}' bez input_schema pominiete (brak odpowiednika w OpenAI function calling)",
                    t.name
                );
                return None;
            };
            Some(Tool {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: Some(schema.clone()),
                },
            })
        })
        .collect();

    (!mapped.is_empty()).then_some(mapped)
}

/// Mapuje Anthropic `tool_choice` na OpenAI. `any` (wymus dowolne narzedzie)
/// odpowiada OpenAI `required`.
fn to_openai_tool_choice(choice: Option<&AnthropicToolChoice>) -> Option<ToolChoice> {
    let choice = choice?;
    match choice.choice_type.as_str() {
        "auto" => Some(ToolChoice::String("auto".to_string())),
        "any" => Some(ToolChoice::String("required".to_string())),
        "none" => Some(ToolChoice::String("none".to_string())),
        "tool" => choice.name.as_ref().map(|name| {
            ToolChoice::Object(ToolChoiceObject {
                tool_type: "function".to_string(),
                function: ToolChoiceFunction { name: name.clone() },
            })
        }),
        other => {
            warn!("tool_choice.type '{}' nieznany — pomijane", other);
            None
        }
    }
}

/// Tlumaczy wiadomosc Anthropic i dopisuje wynik do `out`. Jedna wiadomosc
/// Anthropic moze rozwinac sie w kilka wiadomosci OpenAI: bloki `tool_result`
/// wymagaja osobnych wiadomosci `role:"tool"` z `tool_call_id`.
fn anthropic_message_to_openai(
    msg: AnthropicMessage,
    out: &mut Vec<Message>,
) -> std::result::Result<(), String> {
    let blocks = match msg.content {
        AnthropicContent::Text(s) => {
            out.push(Message {
                role: msg.role,
                content: Some(MessageContent::Text(s)),
                ..Default::default()
            });
            return Ok(());
        }
        AnthropicContent::Blocks(blocks) => blocks,
    };

    let mut parts: Vec<ContentPart> = Vec::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut reasoning_content: Option<String> = None;
    // Wyniki narzedzi musza poprzedzic wiadomosc, w ktorej przyszly: OpenAI
    // wymaga aby `role:"tool"` bezposrednio nastepowalo po assistant-cie z
    // odpowiadajacym `tool_calls`, a nie po tekscie tej samej tury.
    let mut tool_results: Vec<Message> = Vec::new();

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
                let url = format!("data:{};base64,{}", source.media_type, source.data);
                parts.push(ContentPart::ImageUrl {
                    image_url: ImageUrl { url, detail: None },
                });
            }
            AnthropicContentBlock::ToolUse { id, name, input } => {
                tool_calls.push(ToolCall {
                    id,
                    tool_type: "function".to_string(),
                    function: FunctionCall {
                        name,
                        // OpenAI niesie argumenty jako JSON string, Anthropic
                        // jako obiekt.
                        arguments: serde_json::to_string(&input).unwrap_or_else(|_| "{}".into()),
                    },
                });
            }
            AnthropicContentBlock::ToolResult {
                tool_use_id,
                content,
            } => {
                let text = content.as_ref().map(|c| c.flatten()).unwrap_or_default();
                tool_results.push(Message {
                    role: "tool".to_string(),
                    content: Some(MessageContent::Text(text)),
                    tool_call_id: Some(tool_use_id),
                    ..Default::default()
                });
            }
            AnthropicContentBlock::Thinking { thinking } => {
                match reasoning_content.as_mut() {
                    Some(existing) => existing.push_str(&thinking),
                    None => reasoning_content = Some(thinking),
                }
            }
            AnthropicContentBlock::RedactedThinking { data } => {
                match reasoning_content.as_mut() {
                    Some(existing) => existing.push_str(&data),
                    None => reasoning_content = Some(data),
                }
            }
            AnthropicContentBlock::Unsupported => {
                warn!("nieznany typ bloku zawartosci pominiety");
            }
        }
    }

    out.append(&mut tool_results);

    let has_text = parts.iter().any(|p| match p {
        ContentPart::Text { text } => !text.is_empty(),
        // Media count as content: a turn carrying only a picture or a
        // recording is not an empty turn.
        ContentPart::ImageUrl { .. } | ContentPart::InputAudio { .. } => true,
    });
    if has_text || !tool_calls.is_empty() || reasoning_content.is_some() {
        out.push(Message {
            role: msg.role,
            content: has_text.then(|| flatten_parts(parts)),
            reasoning_content,
            tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
            ..Default::default()
        });
    }

    Ok(())
}

/// Splaszcza czesci do `Text` gdy nie ma obrazow — czysto tekstowy `content`
/// jest akceptowany przez wszystkie backendy, tablica `parts` nie.
fn flatten_parts(parts: Vec<ContentPart>) -> MessageContent {
    let all_text = parts.iter().all(|p| matches!(p, ContentPart::Text { .. }));
    if all_text {
        let joined = parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                ContentPart::ImageUrl { .. } | ContentPart::InputAudio { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        MessageContent::Text(joined)
    } else {
        MessageContent::Parts(parts)
    }
}

// =============================================================================
// TLUMACZENIE ODPOWIEDZI: OpenAI -> Anthropic (blocking)
// =============================================================================

/// Mapuje OpenAI `finish_reason` na Anthropic `stop_reason`. `stop` przy
/// dopasowanej sekwencji stop -> `stop_sequence`; inaczej `stop` -> `end_turn`.
fn map_stop_reason(finish_reason: Option<&str>, matched_stop: bool) -> &'static str {
    match finish_reason {
        Some("length") => "max_tokens",
        Some("tool_calls") => "tool_use",
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
                ContentPart::ImageUrl { .. } | ContentPart::InputAudio { .. } => None,
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
    let (text, reasoning, tool_calls, finish_reason) = match choice {
        Some(c) => (
            extract_text(c.message.content.as_ref()),
            c.message.reasoning_content.unwrap_or_default(),
            c.message.tool_calls.unwrap_or_default(),
            c.finish_reason,
        ),
        None => (String::new(), String::new(), Vec::new(), None),
    };

    let matched_stop = finish_reason.as_deref() == Some("stop")
        && stop_sequences.map(|s| !s.is_empty()).unwrap_or(false);
    // Backend moze zwrocic tool_calls z finish_reason "stop" (albo bez niego);
    // dla klienta Anthropic rozstrzygajaca jest obecnosc blokow tool_use, bo
    // bez `stop_reason: tool_use` nie wykona narzedzia i utknie.
    let stop_reason = if tool_calls.is_empty() {
        map_stop_reason(finish_reason.as_deref(), matched_stop)
    } else {
        "tool_use"
    };

    let mut content: Vec<AnthropicResponseBlock> =
        Vec::with_capacity(tool_calls.len() + usize::from(!reasoning.is_empty()) + 1);
    if !reasoning.is_empty() {
        content.push(AnthropicResponseBlock::Thinking {
            thinking: reasoning,
            signature: "tentaflow-generated".to_string(),
        });
    }
    if !text.is_empty() {
        content.push(AnthropicResponseBlock::Text { text });
    }
    for call in tool_calls {
        content.push(AnthropicResponseBlock::ToolUse {
            id: call.id,
            name: call.function.name,
            input: parse_tool_arguments(&call.function.arguments),
        });
    }
    // Anthropic zawsze zwraca co najmniej jeden blok.
    if content.is_empty() {
        content.push(AnthropicResponseBlock::Text {
            text: String::new(),
        });
    }

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
        content,
        model: resp.model,
        stop_reason: Some(stop_reason.to_string()),
        stop_sequence: None,
        usage,
    }
}

/// Parsuje OpenAI `function.arguments` (JSON string) na obiekt wymagany przez
/// Anthropic `tool_use.input`. Puste albo niepoprawne argumenty daja `{}` —
/// klient odrzucilby blok o zlym ksztalcie i przerwal petle agenta.
fn parse_tool_arguments(arguments: &str) -> serde_json::Value {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return serde_json::json!({});
    }
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(value @ serde_json::Value::Object(_)) => value,
        Ok(other) => {
            warn!(
                "tool_use.input: argumenty nie sa obiektem JSON ({}), zwracam pusty obiekt",
                other
            );
            serde_json::json!({})
        }
        Err(e) => {
            warn!(
                "tool_use.input: niepoprawny JSON argumentow ({}) — pusty obiekt",
                e
            );
            serde_json::json!({})
        }
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

        match router
            .route_chat_completion(openai_req, user_ctx, None)
            .await
        {
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

/// Hard cap na liczbe slotow tool-call odtwarzanych z jednego streamu.
/// `delta.index` pochodzi z backendu (potencjalnie zdalnego), wiec bez limitu
/// jeden sfalszowany chunk z ogromnym indeksem alokowalby pamiec bez granic.
const MAX_TOOL_BLOCKS: usize = 256;

/// Ktory blok Anthropic jest aktualnie otwarty. Anthropic dopuszcza dokladnie
/// jeden otwarty blok w danym momencie — przed otwarciem kolejnego trzeba
/// wyslac `content_block_stop`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenBlock {
    Text(u32),
    Thinking(u32),
    Tool { index: u32, slot: u32 },
}

impl OpenBlock {
    fn index(&self) -> u32 {
        match self {
            OpenBlock::Text(i) | OpenBlock::Thinking(i) => *i,
            OpenBlock::Tool { index, .. } => *index,
        }
    }
}

/// Cykl zycia bloku `tool_use`. Anthropic nie pozwala wznowic zamknietego
/// bloku, wiec fragmenty przychodzace po `content_block_stop` musza zostac
/// odrzucone (i zalogowane) zamiast psuc strumien.
#[derive(Debug, PartialEq, Eq)]
enum ToolBlockState {
    Pending,
    Open,
    Closed,
}

/// Slot tool-call z OpenAI odwzorowany na blok Anthropic. `id` i `name`
/// przychodza na fragmencie otwierajacym slot, argumenty akumuluja sie na
/// kolejnych — dopoki nie znamy `id` + `name`, nie da sie wyemitowac
/// `content_block_start`, wiec argumenty buforujemy.
#[derive(Debug)]
struct ToolBlock {
    block_index: u32,
    id: String,
    name: String,
    buffered_args: String,
    state: ToolBlockState,
}

/// Stan tlumaczenia OpenAI chunk-streamu na sekwencje zdarzen Anthropic.
#[derive(Default)]
struct AnthropicStreamState {
    open_block: Option<OpenBlock>,
    next_index: u32,
    /// Klucz to `ToolCallDelta.index` (slot OpenAI), nie indeks bloku.
    tool_blocks: std::collections::HashMap<u32, ToolBlock>,
    finish_reason: Option<String>,
    emitted_tool_use: bool,
    input_tokens: u32,
    output_tokens: u32,
}

type SseFrame = std::result::Result<Frame<Bytes>, std::io::Error>;

impl AnthropicStreamState {
    fn take_index(&mut self) -> u32 {
        let index = self.next_index;
        self.next_index += 1;
        index
    }

    fn close_open_block(&mut self, out: &mut Vec<SseFrame>) {
        let Some(open) = self.open_block.take() else {
            return;
        };
        if matches!(open, OpenBlock::Thinking(_)) {
            out.push(Ok(sse_frame(
                "content_block_delta",
                &serde_json::json!({
                    "type": "content_block_delta",
                    "index": open.index(),
                    "delta": {
                        "type": "signature_delta",
                        "signature": "tentaflow-generated"
                    }
                }),
            )));
        }
        if let OpenBlock::Tool { slot, .. } = open {
            if let Some(block) = self.tool_blocks.get_mut(&slot) {
                block.state = ToolBlockState::Closed;
            }
        }
        out.push(Ok(sse_frame(
            "content_block_stop",
            &serde_json::json!({ "type": "content_block_stop", "index": open.index() }),
        )));
    }

    /// Dopisuje fragment tekstu. Blok tekstowy otwierany jest leniwie — gdy
    /// odpowiedz zawiera wylacznie wywolania narzedzi, pusty blok tekstowy
    /// nigdy nie powstaje.
    fn push_text_delta(&mut self, text: &str, out: &mut Vec<SseFrame>) {
        if !matches!(self.open_block, Some(OpenBlock::Text(_))) {
            self.close_open_block(out);
            let index = self.take_index();
            out.push(Ok(sse_frame(
                "content_block_start",
                &serde_json::json!({
                    "type": "content_block_start",
                    "index": index,
                    "content_block": { "type": "text", "text": "" }
                }),
            )));
            self.open_block = Some(OpenBlock::Text(index));
        }
        let index = self.open_block.map(|b| b.index()).unwrap_or(0);
        out.push(Ok(sse_frame(
            "content_block_delta",
            &serde_json::json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "text_delta", "text": text }
            }),
        )));
    }

    fn push_reasoning_delta(&mut self, reasoning: &str, out: &mut Vec<SseFrame>) {
        if !matches!(self.open_block, Some(OpenBlock::Thinking(_))) {
            self.close_open_block(out);
            let index = self.take_index();
            out.push(Ok(sse_frame(
                "content_block_start",
                &serde_json::json!({
                    "type": "content_block_start",
                    "index": index,
                    "content_block": { "type": "thinking", "thinking": "" }
                }),
            )));
            self.open_block = Some(OpenBlock::Thinking(index));
        }
        let index = self.open_block.map(|b| b.index()).unwrap_or(0);
        out.push(Ok(sse_frame(
            "content_block_delta",
            &serde_json::json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "thinking_delta", "thinking": reasoning }
            }),
        )));
    }

    fn push_tool_delta(
        &mut self,
        delta: &crate::api::openai::types::ToolCallDelta,
        out: &mut Vec<SseFrame>,
    ) {
        let slot = delta.index;
        if !self.tool_blocks.contains_key(&slot) && self.tool_blocks.len() >= MAX_TOOL_BLOCKS {
            warn!(
                slot,
                "liczba slotow tool-call przekroczyla limit — fragment odrzucony"
            );
            return;
        }

        let entry = self.tool_blocks.entry(slot).or_insert_with(|| ToolBlock {
            block_index: 0,
            id: String::new(),
            name: String::new(),
            buffered_args: String::new(),
            state: ToolBlockState::Pending,
        });

        if let Some(id) = delta.id.as_ref() {
            entry.id = id.clone();
        }
        let mut args_fragment: Option<String> = None;
        if let Some(function) = delta.function.as_ref() {
            if let Some(name) = function.name.as_ref() {
                entry.name.push_str(name);
            }
            if let Some(arguments) = function.arguments.as_ref() {
                args_fragment = Some(arguments.clone());
            }
        }

        match entry.state {
            ToolBlockState::Closed => {
                warn!(
                    slot,
                    "fragment tool-call po zamknieciu bloku — odrzucony (Anthropic nie pozwala wznowic bloku)"
                );
            }
            ToolBlockState::Pending => {
                if let Some(fragment) = args_fragment {
                    entry.buffered_args.push_str(&fragment);
                }
                // `content_block_start` wymaga `id` i `name`; bez nich klient
                // nie ma czego wywolac, wiec czekamy na fragment otwierajacy.
                if entry.id.is_empty() || entry.name.is_empty() {
                    return;
                }
                let id = entry.id.clone();
                let name = entry.name.clone();
                let buffered = std::mem::take(&mut entry.buffered_args);

                self.close_open_block(out);
                let index = self.take_index();
                out.push(Ok(sse_frame(
                    "content_block_start",
                    &serde_json::json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {
                            "type": "tool_use",
                            "id": id,
                            "name": name,
                            "input": {}
                        }
                    }),
                )));
                if !buffered.is_empty() {
                    out.push(Ok(input_json_delta(index, &buffered)));
                }

                let entry = self
                    .tool_blocks
                    .get_mut(&slot)
                    .expect("slot wstawiony powyzej");
                entry.block_index = index;
                entry.state = ToolBlockState::Open;
                self.open_block = Some(OpenBlock::Tool { index, slot });
                self.emitted_tool_use = true;
            }
            ToolBlockState::Open => {
                let index = entry.block_index;
                if let Some(fragment) = args_fragment {
                    if !fragment.is_empty() {
                        out.push(Ok(input_json_delta(index, &fragment)));
                    }
                }
            }
        }
    }

    /// Domyka strumien: zamyka otwarty blok, gwarantuje co najmniej jeden blok
    /// zawartosci (Anthropic zawsze go zwraca) i emituje `message_delta` +
    /// `message_stop`.
    fn finalize(&mut self) -> Vec<SseFrame> {
        let mut out = Vec::new();
        self.close_open_block(&mut out);

        if self.next_index == 0 {
            let index = self.take_index();
            out.push(Ok(sse_frame(
                "content_block_start",
                &serde_json::json!({
                    "type": "content_block_start",
                    "index": index,
                    "content_block": { "type": "text", "text": "" }
                }),
            )));
            out.push(Ok(sse_frame(
                "content_block_stop",
                &serde_json::json!({ "type": "content_block_stop", "index": index }),
            )));
        }

        // Backend moze zamknac strumien tool-calli z finish_reason "stop";
        // dla klienta Anthropic rozstrzyga fakt emisji blokow tool_use, bo bez
        // `stop_reason: tool_use` nie wykona narzedzia i utknie.
        let stop_reason = if self.emitted_tool_use {
            "tool_use"
        } else {
            // OpenAI nie raportuje dopasowanej sekwencji stop w streamie.
            map_stop_reason(self.finish_reason.as_deref(), false)
        };

        out.push(Ok(sse_frame(
            "message_delta",
            &serde_json::json!({
                "type": "message_delta",
                "delta": { "stop_reason": stop_reason, "stop_sequence": null },
                "usage": {
                    "input_tokens": self.input_tokens,
                    "output_tokens": self.output_tokens
                }
            }),
        )));
        out.push(Ok(sse_frame(
            "message_stop",
            &serde_json::json!({ "type": "message_stop" }),
        )));
        out
    }
}

fn input_json_delta(index: u32, partial_json: &str) -> Frame<Bytes> {
    sse_frame(
        "content_block_delta",
        &serde_json::json!({
            "type": "content_block_delta",
            "index": index,
            "delta": { "type": "input_json_delta", "partial_json": partial_json }
        }),
    )
}

/// Buduje strumien SSE w formacie Anthropic z OpenAI chunk-stream'a. Sekwencja:
/// `message_start` -> (`content_block_start` -> N* `content_block_delta` ->
/// `content_block_stop`)* -> `message_delta` -> `message_stop`. Bloki tekstowe
/// niosa `text_delta`, bloki `tool_use` — `input_json_delta`.
fn anthropic_sse_response(
    chunk_stream: Pin<
        Box<
            dyn Stream<Item = crate::error::Result<crate::api::openai::types::ChatCompletionChunk>>
                + Send,
        >,
    >,
    model: String,
) -> Response<OpenAIBody> {
    let msg_id = format!("msg_{}", uuid::Uuid::new_v4().simple());

    // `input_tokens` nie jest znane przed pierwszym chunkiem z usage; realna
    // wartosc trafia do `message_delta` na koncu strumienia.
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

    let prefix = futures::stream::iter(vec![Ok(sse_frame("message_start", &message_start))]);

    let state = Arc::new(parking_lot::Mutex::new(AnthropicStreamState::default()));
    let state_for_body = state.clone();

    let body_stream = chunk_stream.flat_map(move |chunk_result| {
        let frames: Vec<SseFrame> = match chunk_result {
            Ok(chunk) => {
                let mut guard = state_for_body.lock();
                let mut out = Vec::new();
                if let Some(usage) = chunk.usage.as_ref() {
                    guard.input_tokens = usage.prompt_tokens;
                    guard.output_tokens = usage.completion_tokens;
                }
                for choice in &chunk.choices {
                    if let Some(fr) = choice.finish_reason.as_ref() {
                        guard.finish_reason = Some(fr.clone());
                    }
                    if let Some(reasoning) = choice.delta.reasoning_content.as_deref() {
                        if !reasoning.is_empty() {
                            guard.push_reasoning_delta(reasoning, &mut out);
                        }
                    }
                    if let Some(text) = choice.delta.content.as_deref() {
                        if !text.is_empty() {
                            guard.push_text_delta(text, &mut out);
                        }
                    }
                    if let Some(tool_calls) = choice.delta.tool_calls.as_ref() {
                        for delta in tool_calls {
                            guard.push_tool_delta(delta, &mut out);
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

    let suffix =
        futures::stream::once(async move { futures::stream::iter(state.lock().finalize()) });

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
    // Deklaracje narzedzi ida do modelu razem z promptem, wiec licza sie do
    // input_tokens — pominiecie ich zanizalo by estymate przy duzych schematach.
    for tool in request.tools.iter().flatten() {
        total += crate::metrics::token_counter::estimate_tokens(&tool.name);
        if let Some(description) = tool.description.as_deref() {
            total += crate::metrics::token_counter::estimate_tokens(description);
        }
        if let Some(schema) = tool.input_schema.as_ref() {
            total += crate::metrics::token_counter::estimate_tokens(&schema.to_string());
        }
    }
    for msg in &request.messages {
        total += match &msg.content {
            AnthropicContent::Text(s) => crate::metrics::token_counter::estimate_tokens(s),
            AnthropicContent::Blocks(blocks) => blocks.iter().map(count_block_tokens).sum(),
        };
    }

    let body = serde_json::json!({ "input_tokens": total });
    let bytes = serde_json::to_vec(&body).unwrap_or_default();
    Ok(json_response(StatusCode::OK, bytes))
}

/// Estymuje tokeny pojedynczego bloku wejsciowego. Bloki `image` pomijane
/// (brak tokenizera wizyjnego w tej sciezce).
fn count_block_tokens(block: &AnthropicContentBlock) -> u64 {
    match block {
        AnthropicContentBlock::Text { text } => {
            crate::metrics::token_counter::estimate_tokens(text)
        }
        AnthropicContentBlock::Image { .. } => 0,
        AnthropicContentBlock::ToolUse { name, input, .. } => {
            crate::metrics::token_counter::estimate_tokens(name)
                + crate::metrics::token_counter::estimate_tokens(&input.to_string())
        }
        AnthropicContentBlock::ToolResult { content, .. } => content
            .as_ref()
            .map(|c| crate::metrics::token_counter::estimate_tokens(&c.flatten()))
            .unwrap_or(0),
        AnthropicContentBlock::Thinking { thinking } => {
            crate::metrics::token_counter::estimate_tokens(thinking)
        }
        AnthropicContentBlock::RedactedThinking { .. } | AnthropicContentBlock::Unsupported => 0,
    }
}

// =============================================================================
// TESTY
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::openai::types::{Choice, FunctionCallDelta, ToolCallDelta, Usage};

    fn parse_request(json: serde_json::Value) -> AnthropicMessagesRequest {
        serde_json::from_value(json).expect("request parsuje sie")
    }

    /// Wyciaga pola `data:` z ramek SSE jako wartosci JSON.
    fn frames_to_events(frames: Vec<SseFrame>) -> Vec<serde_json::Value> {
        frames
            .into_iter()
            .map(|f| {
                let bytes = f.expect("ramka ok").into_data().expect("ramka danych");
                let text = String::from_utf8(bytes.to_vec()).expect("utf8");
                let data = text
                    .lines()
                    .find_map(|l| l.strip_prefix("data: "))
                    .expect("linia data");
                serde_json::from_str(data).expect("data to JSON")
            })
            .collect()
    }

    fn tool_delta(
        index: u32,
        id: Option<&str>,
        name: Option<&str>,
        args: Option<&str>,
    ) -> ToolCallDelta {
        ToolCallDelta {
            index,
            id: id.map(str::to_string),
            tool_type: id.map(|_| "function".to_string()),
            function: (name.is_some() || args.is_some()).then(|| FunctionCallDelta {
                name: name.map(str::to_string),
                arguments: args.map(str::to_string),
            }),
        }
    }

    // ---- request: tools -> OpenAI ----

    #[test]
    fn tools_map_input_schema_to_function_parameters() {
        let req = parse_request(serde_json::json!({
            "model": "m",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{
                "name": "get_weather",
                "description": "Pobiera pogode",
                "input_schema": {
                    "type": "object",
                    "properties": {"city": {"type": "string"}},
                    "required": ["city"]
                }
            }]
        }));

        let openai = to_openai_request(req, false).expect("tlumaczenie ok");
        let tools = openai.tools.expect("tools przekazane");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].tool_type, "function");
        assert_eq!(tools[0].function.name, "get_weather");
        assert_eq!(
            tools[0].function.description.as_deref(),
            Some("Pobiera pogode")
        );
        assert_eq!(
            tools[0].function.parameters.as_ref().expect("schema")["required"][0],
            "city"
        );
    }

    #[test]
    fn server_tool_without_input_schema_is_skipped() {
        let req = parse_request(serde_json::json!({
            "model": "m",
            "max_tokens": 8,
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{"name": "web_search"}]
        }));
        let openai = to_openai_request(req, false).expect("ok");
        assert!(
            openai.tools.is_none(),
            "narzedzie bez schematu nie leci dalej"
        );
        assert!(
            openai.tool_choice.is_none(),
            "tool_choice bez tools nie ma sensu"
        );
    }

    #[test]
    fn tool_choice_variants_map_to_openai() {
        let cases = [
            (serde_json::json!({"type": "auto"}), "auto"),
            (serde_json::json!({"type": "any"}), "required"),
            (serde_json::json!({"type": "none"}), "none"),
        ];
        for (input, expected) in cases {
            let choice: AnthropicToolChoice = serde_json::from_value(input).expect("parsuje");
            match to_openai_tool_choice(Some(&choice)).expect("zmapowane") {
                ToolChoice::String(s) => assert_eq!(s, expected),
                other => panic!("oczekiwano stringa, jest {:?}", other),
            }
        }

        let named: AnthropicToolChoice =
            serde_json::from_value(serde_json::json!({"type": "tool", "name": "get_weather"}))
                .expect("parsuje");
        match to_openai_tool_choice(Some(&named)).expect("zmapowane") {
            ToolChoice::Object(o) => {
                assert_eq!(o.tool_type, "function");
                assert_eq!(o.function.name, "get_weather");
            }
            other => panic!("oczekiwano obiektu, jest {:?}", other),
        }
    }

    // ---- request: tool_use / tool_result w historii ----

    #[test]
    fn tool_use_block_becomes_openai_tool_call() {
        let req = parse_request(serde_json::json!({
            "model": "m",
            "max_tokens": 8,
            "messages": [
                {"role": "user", "content": "pogoda?"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "sprawdzam"},
                    {"type": "tool_use", "id": "toolu_1", "name": "get_weather",
                     "input": {"city": "Warszawa"}}
                ]}
            ]
        }));

        let openai = to_openai_request(req, false).expect("ok");
        let assistant = openai.messages.last().expect("wiadomosc assistant");
        assert_eq!(assistant.role, "assistant");
        let calls = assistant.tool_calls.as_ref().expect("tool_calls");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "toolu_1");
        assert_eq!(calls[0].function.name, "get_weather");
        let args: serde_json::Value =
            serde_json::from_str(&calls[0].function.arguments).expect("argumenty to JSON");
        assert_eq!(args["city"], "Warszawa");
    }

    #[test]
    fn tool_result_becomes_tool_role_message_before_text() {
        let req = parse_request(serde_json::json!({
            "model": "m",
            "max_tokens": 8,
            "messages": [{"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_1", "content": "18C"},
                {"type": "text", "text": "i co dalej?"}
            ]}]
        }));

        let openai = to_openai_request(req, false).expect("ok");
        assert_eq!(openai.messages.len(), 2, "tool_result to osobna wiadomosc");
        assert_eq!(openai.messages[0].role, "tool");
        assert_eq!(openai.messages[0].tool_call_id.as_deref(), Some("toolu_1"));
        match openai.messages[0].content.as_ref().expect("content") {
            MessageContent::Text(t) => assert_eq!(t, "18C"),
            other => panic!("oczekiwano tekstu, jest {:?}", other),
        }
        assert_eq!(openai.messages[1].role, "user");
    }

    #[test]
    fn tool_result_with_block_content_is_flattened() {
        let req = parse_request(serde_json::json!({
            "model": "m",
            "max_tokens": 8,
            "messages": [{"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": [
                    {"type": "text", "text": "linia1"},
                    {"type": "text", "text": "linia2"}
                ]}
            ]}]
        }));
        let openai = to_openai_request(req, false).expect("ok");
        match openai.messages[0].content.as_ref().expect("content") {
            MessageContent::Text(t) => assert_eq!(t, "linia1\nlinia2"),
            other => panic!("oczekiwano tekstu, jest {:?}", other),
        }
    }

    #[test]
    fn thinking_is_preserved_as_reasoning_content() {
        let req = parse_request(serde_json::json!({
            "model": "m",
            "max_tokens": 8,
            "messages": [{"role": "assistant", "content": [
                {"type": "thinking", "thinking": "hmm", "signature": "sig"},
                {"type": "some_future_block", "whatever": 1},
                {"type": "text", "text": "gotowe"}
            ]}]
        }));
        let openai = to_openai_request(req, false).expect("nieznane bloki nie sa bledem");
        assert_eq!(openai.messages.len(), 1);
        assert_eq!(
            openai.messages[0].reasoning_content.as_deref(),
            Some("hmm")
        );
        match openai.messages[0].content.as_ref().expect("content") {
            MessageContent::Text(t) => assert_eq!(t, "gotowe"),
            other => panic!("oczekiwano tekstu, jest {:?}", other),
        }
    }

    #[test]
    fn redacted_thinking_is_preserved_for_reasoning_backend_replay() {
        let req = parse_request(serde_json::json!({
            "model": "m",
            "max_tokens": 8,
            "messages": [{"role": "assistant", "content": [
                {"type": "redacted_thinking", "data": "opaque-plan-state"},
                {"type": "text", "text": "plan"}
            ]}]
        }));

        let openai = to_openai_request(req, false).expect("ok");
        assert_eq!(
            openai.messages[0].reasoning_content.as_deref(),
            Some("opaque-plan-state")
        );
        let body = serde_json::to_value(&openai).expect("serializacja requestu");
        assert_eq!(
            body["messages"][0]["reasoning_content"],
            "opaque-plan-state"
        );
    }

    #[test]
    fn tool_only_assistant_turn_has_no_content_field() {
        let req = parse_request(serde_json::json!({
            "model": "m",
            "max_tokens": 8,
            "messages": [{"role": "assistant", "content": [
                {"type": "tool_use", "id": "t1", "name": "f", "input": {}}
            ]}]
        }));
        let openai = to_openai_request(req, false).expect("ok");
        assert!(
            openai.messages[0].content.is_none(),
            "brak pustego contentu"
        );
        assert!(openai.messages[0].tool_calls.is_some());
    }

    // ---- response: OpenAI -> Anthropic ----

    fn response_with(message: Message, finish_reason: &str) -> ChatCompletionResponse {
        ChatCompletionResponse {
            id: "id".into(),
            object: "chat.completion".into(),
            created: 0,
            model: "m".into(),
            choices: vec![Choice {
                index: 0,
                message,
                finish_reason: Some(finish_reason.to_string()),
                logprobs: None,
            }],
            usage: Some(Usage {
                prompt_tokens: 11,
                completion_tokens: 7,
                total_tokens: 18,
            }),
            system_fingerprint: None,
            detected_intent: None,
            detected_tools: None,
            transcribed_text: None,
            speaker_id: None,
            speaker_name: None,
            speaker_confidence: None,
        }
    }

    #[test]
    fn tool_calls_become_tool_use_blocks_with_stop_reason() {
        let resp = response_with(
            Message {
                role: "assistant".into(),
                content: Some(MessageContent::Text("sprawdzam".into())),
                tool_calls: Some(vec![ToolCall {
                    id: "call_1".into(),
                    tool_type: "function".into(),
                    function: FunctionCall {
                        name: "get_weather".into(),
                        arguments: r#"{"city":"Krakow"}"#.into(),
                    },
                }]),
                ..Default::default()
            },
            "tool_calls",
        );

        let out = to_anthropic_response(resp, None);
        assert_eq!(out.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(out.content.len(), 2, "blok tekstu + blok tool_use");
        match &out.content[0] {
            AnthropicResponseBlock::Text { text } => assert_eq!(text, "sprawdzam"),
            other => panic!("oczekiwano tekstu, jest {:?}", other),
        }
        match &out.content[1] {
            AnthropicResponseBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "call_1");
                assert_eq!(name, "get_weather");
                assert_eq!(input["city"], "Krakow");
            }
            other => panic!("oczekiwano tool_use, jest {:?}", other),
        }
        assert_eq!(out.usage.input_tokens, 11);
        assert_eq!(out.usage.output_tokens, 7);
    }

    #[test]
    fn tool_calls_force_tool_use_even_when_backend_says_stop() {
        let resp = response_with(
            Message {
                role: "assistant".into(),
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: "c".into(),
                    tool_type: "function".into(),
                    function: FunctionCall {
                        name: "f".into(),
                        arguments: "{}".into(),
                    },
                }]),
                ..Default::default()
            },
            "stop",
        );
        let out = to_anthropic_response(resp, None);
        assert_eq!(out.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(out.content.len(), 1, "brak pustego bloku tekstowego");
    }

    #[test]
    fn text_only_response_keeps_end_turn_and_one_block() {
        let resp = response_with(
            Message {
                role: "assistant".into(),
                content: Some(MessageContent::Text("hej".into())),
                ..Default::default()
            },
            "stop",
        );
        let out = to_anthropic_response(resp, None);
        assert_eq!(out.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(out.content.len(), 1);
    }

    #[test]
    fn reasoning_response_becomes_an_anthropic_thinking_block() {
        let resp = response_with(
            Message {
                role: "assistant".into(),
                content: Some(MessageContent::Text("answer".into())),
                reasoning_content: Some("private reasoning".into()),
                ..Default::default()
            },
            "stop",
        );

        let out = to_anthropic_response(resp, None);
        assert_eq!(out.content.len(), 2);
        match &out.content[0] {
            AnthropicResponseBlock::Thinking { thinking, .. } => {
                assert_eq!(thinking, "private reasoning")
            }
            other => panic!("oczekiwano thinking, jest {:?}", other),
        }
        match &out.content[1] {
            AnthropicResponseBlock::Text { text } => assert_eq!(text, "answer"),
            other => panic!("oczekiwano tekstu, jest {:?}", other),
        }
    }

    #[test]
    fn empty_response_still_carries_one_content_block() {
        let resp = response_with(
            Message {
                role: "assistant".into(),
                content: None,
                ..Default::default()
            },
            "stop",
        );
        let out = to_anthropic_response(resp, None);
        assert_eq!(out.content.len(), 1);
    }

    #[test]
    fn map_stop_reason_covers_tool_calls() {
        assert_eq!(map_stop_reason(Some("tool_calls"), false), "tool_use");
        assert_eq!(map_stop_reason(Some("length"), false), "max_tokens");
        assert_eq!(map_stop_reason(Some("stop"), true), "stop_sequence");
        assert_eq!(map_stop_reason(Some("stop"), false), "end_turn");
        assert_eq!(map_stop_reason(None, false), "end_turn");
    }

    #[test]
    fn malformed_tool_arguments_degrade_to_empty_object() {
        assert_eq!(parse_tool_arguments(""), serde_json::json!({}));
        assert_eq!(parse_tool_arguments("{not json"), serde_json::json!({}));
        assert_eq!(parse_tool_arguments("[1,2]"), serde_json::json!({}));
        assert_eq!(
            parse_tool_arguments(r#"{"a":1}"#),
            serde_json::json!({"a": 1})
        );
    }

    // ---- streaming ----

    #[test]
    fn streamed_tool_call_emits_input_json_delta_sequence() {
        let mut state = AnthropicStreamState::default();
        let mut out = Vec::new();

        state.push_text_delta("sprawdzam", &mut out);
        state.push_tool_delta(
            &tool_delta(0, Some("call_1"), Some("get_weather"), None),
            &mut out,
        );
        state.push_tool_delta(&tool_delta(0, None, None, Some(r#"{"city":"#)), &mut out);
        state.push_tool_delta(&tool_delta(0, None, None, Some(r#""Gdansk"}"#)), &mut out);
        state.finish_reason = Some("tool_calls".into());
        out.extend(state.finalize());

        let events = frames_to_events(out);
        let types: Vec<&str> = events
            .iter()
            .map(|e| e["type"].as_str().expect("type"))
            .collect();
        assert_eq!(
            types,
            vec![
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );

        assert_eq!(events[0]["content_block"]["type"], "text");
        assert_eq!(events[0]["index"], 0);
        assert_eq!(events[1]["delta"]["type"], "text_delta");
        assert_eq!(events[2]["index"], 0);

        assert_eq!(events[3]["content_block"]["type"], "tool_use");
        assert_eq!(events[3]["content_block"]["id"], "call_1");
        assert_eq!(events[3]["content_block"]["name"], "get_weather");
        assert_eq!(events[3]["index"], 1);
        assert_eq!(events[4]["delta"]["type"], "input_json_delta");
        assert_eq!(events[4]["delta"]["partial_json"], r#"{"city":"#);
        assert_eq!(events[5]["delta"]["partial_json"], r#""Gdansk"}"#);

        assert_eq!(events[7]["delta"]["stop_reason"], "tool_use");
    }

    #[test]
    fn arguments_before_name_are_buffered_until_block_opens() {
        let mut state = AnthropicStreamState::default();
        let mut out = Vec::new();

        // Fragment z argumentami przed poznaniem id/name — bez buforowania
        // pierwsza czesc JSON-a argumentow zniknelaby.
        state.push_tool_delta(&tool_delta(0, None, None, Some(r#"{"a":"#)), &mut out);
        assert!(out.is_empty(), "blok nie moze sie otworzyc bez id i name");
        state.push_tool_delta(&tool_delta(0, Some("c1"), Some("f"), Some("1}")), &mut out);

        let events = frames_to_events(out);
        assert_eq!(events[0]["type"], "content_block_start");
        assert_eq!(events[1]["delta"]["partial_json"], r#"{"a":1}"#);
    }

    #[test]
    fn parallel_tool_calls_get_separate_blocks() {
        let mut state = AnthropicStreamState::default();
        let mut out = Vec::new();

        state.push_tool_delta(&tool_delta(0, Some("c1"), Some("f1"), Some("{}")), &mut out);
        state.push_tool_delta(&tool_delta(1, Some("c2"), Some("f2"), Some("{}")), &mut out);
        out.extend(state.finalize());

        let events = frames_to_events(out);
        let starts: Vec<&serde_json::Value> = events
            .iter()
            .filter(|e| e["type"] == "content_block_start")
            .collect();
        assert_eq!(starts.len(), 2);
        assert_eq!(starts[0]["index"], 0);
        assert_eq!(starts[0]["content_block"]["name"], "f1");
        assert_eq!(starts[1]["index"], 1);
        assert_eq!(starts[1]["content_block"]["name"], "f2");

        // Kazdy otwarty blok musi zostac zamkniety.
        let stops = events
            .iter()
            .filter(|e| e["type"] == "content_block_stop")
            .count();
        assert_eq!(stops, 2);
    }

    #[test]
    fn fragment_after_block_close_is_dropped() {
        let mut state = AnthropicStreamState::default();
        let mut out = Vec::new();

        state.push_tool_delta(&tool_delta(0, Some("c1"), Some("f1"), Some("{}")), &mut out);
        state.push_tool_delta(&tool_delta(1, Some("c2"), Some("f2"), Some("{}")), &mut out);
        let before = out.len();
        // Slot 0 jest juz zamkniety przez otwarcie slotu 1.
        state.push_tool_delta(&tool_delta(0, None, None, Some("spozniony")), &mut out);
        assert_eq!(out.len(), before, "spozniony fragment nie generuje zdarzen");
    }

    #[test]
    fn text_only_stream_keeps_end_turn_and_single_block() {
        let mut state = AnthropicStreamState::default();
        let mut out = Vec::new();
        state.push_text_delta("hej", &mut out);
        state.finish_reason = Some("stop".into());
        out.extend(state.finalize());

        let events = frames_to_events(out);
        let types: Vec<&str> = events.iter().map(|e| e["type"].as_str().unwrap()).collect();
        assert_eq!(
            types,
            vec![
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
        assert_eq!(events[3]["delta"]["stop_reason"], "end_turn");
    }

    #[test]
    fn reasoning_stream_emits_a_thinking_block_separate_from_text() {
        let mut state = AnthropicStreamState::default();
        let mut out = Vec::new();
        state.push_reasoning_delta("private reasoning", &mut out);
        state.push_text_delta("answer", &mut out);
        state.finish_reason = Some("stop".into());
        out.extend(state.finalize());

        let events = frames_to_events(out);
        assert_eq!(events[0]["content_block"]["type"], "thinking");
        assert_eq!(events[1]["delta"]["type"], "thinking_delta");
        assert_eq!(events[1]["delta"]["thinking"], "private reasoning");
        assert_eq!(events[2]["delta"]["type"], "signature_delta");
        assert_eq!(events[3]["type"], "content_block_stop");
        assert_eq!(events[4]["content_block"]["type"], "text");
        assert_eq!(events[5]["delta"]["type"], "text_delta");
        assert_eq!(events[5]["delta"]["text"], "answer");
    }

    #[test]
    fn empty_stream_still_emits_one_content_block() {
        let mut state = AnthropicStreamState::default();
        let events = frames_to_events(state.finalize());
        let types: Vec<&str> = events.iter().map(|e| e["type"].as_str().unwrap()).collect();
        assert_eq!(
            types,
            vec![
                "content_block_start",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
    }

    #[test]
    fn finalize_reports_usage_from_stream() {
        let mut state = AnthropicStreamState::default();
        state.input_tokens = 42;
        state.output_tokens = 9;
        let events = frames_to_events(state.finalize());
        let delta = events
            .iter()
            .find(|e| e["type"] == "message_delta")
            .expect("message_delta");
        assert_eq!(delta["usage"]["input_tokens"], 42);
        assert_eq!(delta["usage"]["output_tokens"], 9);
    }

    #[test]
    fn tool_slot_cap_is_enforced() {
        let mut state = AnthropicStreamState::default();
        let mut out = Vec::new();
        for slot in 0..(MAX_TOOL_BLOCKS as u32 + 10) {
            state.push_tool_delta(
                &tool_delta(slot, Some("c"), Some("f"), Some("{}")),
                &mut out,
            );
        }
        assert_eq!(state.tool_blocks.len(), MAX_TOOL_BLOCKS);
    }

    // ---- count_tokens ----

    #[test]
    fn count_block_tokens_covers_tool_blocks() {
        let tool_use = AnthropicContentBlock::ToolUse {
            id: "t".into(),
            name: "get_weather".into(),
            input: serde_json::json!({"city": "Warszawa"}),
        };
        assert!(count_block_tokens(&tool_use) > 0);

        let tool_result = AnthropicContentBlock::ToolResult {
            tool_use_id: "t".into(),
            content: Some(AnthropicToolResultContent::Text("18 stopni".into())),
        };
        assert!(count_block_tokens(&tool_result) > 0);

        assert_eq!(count_block_tokens(&AnthropicContentBlock::Unsupported), 0);
    }
}
