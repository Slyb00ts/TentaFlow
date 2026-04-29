// =============================================================================
// File: api/dashboard/handlers_translate.rs — Binary handler for Translate user
// app. Wraps the LLM chat pipeline with a fixed system prompt and returns a
// single completion. Policy: UserSession.
// =============================================================================

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    MessageBody, ProtocolError, ProtocolErrorCode, SessionAuth, TranslateResponse,
};

use crate::api::openai::types::{ChatCompletionRequest, Message, MessageContent};
use crate::db::repository;
use crate::dispatch::HandlerContext;

// Upper bound for the source text to protect prompt budget and latency.
const MAX_SOURCE_CHARS: usize = 10_000;

// Supported ISO 639-1 target codes. Kept in sync with the frontend.
const SUPPORTED_LANGS: &[&str] = &[
    "en", "pl", "de", "es", "fr", "it", "nl", "pt", "uk", "ru", "cs", "ja", "zh", "ko",
];

// Display name used in the system prompt. Passing the English endonym to the
// model is enough for instruction-tuned LLMs; localisation of these labels in
// the GUI is independent.
fn lang_display_name(code: &str) -> &'static str {
    match code {
        "en" => "English",
        "pl" => "Polish",
        "de" => "German",
        "es" => "Spanish",
        "fr" => "French",
        "it" => "Italian",
        "nl" => "Dutch",
        "pt" => "Portuguese",
        "uk" => "Ukrainian",
        "ru" => "Russian",
        "cs" => "Czech",
        "ja" => "Japanese",
        "zh" => "Chinese",
        "ko" => "Korean",
        _ => "the target language",
    }
}

fn is_supported_lang(code: &str) -> bool {
    SUPPORTED_LANGS.iter().any(|&c| c == code)
}

fn validate_tone(tone: &str) -> bool {
    matches!(tone, "formal" | "casual" | "neutral")
}

// Extracts plain text from the first choice of a ChatCompletionResponse.
fn first_choice_text(resp: &crate::api::openai::types::ChatCompletionResponse) -> String {
    resp.choices
        .first()
        .and_then(|c| c.message.content.as_ref())
        .map(|content| match content {
            MessageContent::Text(text) => text.clone(),
            MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|p| {
                    if let crate::api::openai::types::ContentPart::Text { text } = p {
                        Some(text.clone())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join(""),
        })
        .unwrap_or_default()
}

// Selects an LLM identifier. Prefers the first unified LLM from mesh registry;
// falls back to the generic "default" that local_inference resolves at runtime.
fn pick_llm_model(ctx: &HandlerContext) -> String {
    let unified =
        crate::api::dashboard::api_models::collect_unified(&ctx.state.mesh_services_registry);
    unified
        .into_iter()
        .find(|m| m.service_type.eq_ignore_ascii_case("llm"))
        .map(|m| m.model_name)
        .unwrap_or_else(|| "default".to_string())
}

fn current_user_id(ctx: &HandlerContext) -> Option<i64> {
    match &ctx.session {
        SessionAuth::UserSession { user_id, .. } => {
            if user_id[0] != 0xFF {
                return None;
            }
            let mut le = [0u8; 8];
            le.copy_from_slice(&user_id[8..]);
            Some(i64::from_le_bytes(le))
        }
        _ => None,
    }
}

fn audit_translate(
    ctx: &HandlerContext,
    source_lang: &str,
    target_lang: &str,
    chars_in: usize,
    chars_out: usize,
) {
    let user_id = current_user_id(ctx);
    let details = serde_json::json!({
        "source_lang": source_lang,
        "target_lang": target_lang,
        "chars_in": chars_in,
        "chars_out": chars_out,
    })
    .to_string();
    let node_id = ctx.state.local_node_id.as_ref();
    if let Err(e) = repository::log_audit_full(
        &ctx.state.db,
        user_id,
        None,
        "translate",
        Some("translate"),
        None,
        Some(&details),
        "info",
        None,
        Some(node_id),
    ) {
        tracing::warn!("audit log failed (translate): {}", e);
    }
}

#[handler(variant = "TranslateRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn translate(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::TranslateBody(tentaflow_protocol::TranslatePayload::Req(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected TranslateRequestBody")),
    };

    let source_text = payload.source_text.trim();
    if source_text.is_empty() {
        return Err(ProtocolError::bad_request("source_text cannot be empty"));
    }
    if source_text.chars().count() > MAX_SOURCE_CHARS {
        return Err(ProtocolError::bad_request(
            "source_text exceeds maximum length",
        ));
    }

    if !is_supported_lang(&payload.target_lang) {
        return Err(ProtocolError::bad_request(
            "target_lang is not in the supported allowlist",
        ));
    }
    let source_is_auto = payload.source_lang == "auto";
    if !source_is_auto && !is_supported_lang(&payload.source_lang) {
        return Err(ProtocolError::bad_request(
            "source_lang must be 'auto' or a supported ISO 639-1 code",
        ));
    }
    if let Some(t) = payload.tone.as_deref() {
        if !validate_tone(t) {
            return Err(ProtocolError::bad_request(
                "tone must be 'formal', 'casual' or 'neutral'",
            ));
        }
    }

    let target_name = lang_display_name(&payload.target_lang);
    let mut system_prompt = format!(
        "You are a professional translator. Translate the user's text into {target}. \
Preserve tone, formatting, punctuation and technical terms. Output ONLY the translated \
text, with no explanations, quotes, preface or meta-commentary.",
        target = target_name
    );
    if !source_is_auto {
        let source_name = lang_display_name(&payload.source_lang);
        system_prompt.push_str(&format!(" Source language is {}.", source_name));
    }
    if let Some(tone) = payload.tone.as_deref() {
        system_prompt.push_str(&format!(" Use a {} register.", tone));
    }

    let model_id = pick_llm_model(ctx);

    let completion_req = ChatCompletionRequest {
        model: model_id.clone(),
        messages: vec![
            Message {
                role: "system".to_string(),
                content: Some(MessageContent::Text(system_prompt)),
                reasoning_content: None,
                name: None,
                tool_calls: None,
                tool_call_id: None,
            },
            Message {
                role: "user".to_string(),
                content: Some(MessageContent::Text(source_text.to_string())),
                reasoning_content: None,
                name: None,
                tool_calls: None,
                tool_call_id: None,
            },
        ],
        temperature: Some(0.2),
        max_tokens: None,
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        stream: false,
        user: None,
        response_format: None,
        tools: None,
        tool_choice: None,
        n: None,
        rag_options: None,
        memory_options: None,
        audio_input: None,
    };

    let route_result = ctx
        .state
        .router
        .route_chat_completion(completion_req)
        .await
        .map_err(|e| {
            ProtocolError::new(
                ProtocolErrorCode::Internal,
                format!("translation failed: {}", e),
            )
        })?;

    let response = route_result.response;
    let translated_text = first_choice_text(&response).trim().to_string();
    if translated_text.is_empty() {
        return Err(ProtocolError::internal("empty translation from LLM"));
    }

    let tokens_used = response
        .usage
        .as_ref()
        .map(|u| u.total_tokens as i32)
        .unwrap_or(0);

    audit_translate(
        ctx,
        &payload.source_lang,
        &payload.target_lang,
        source_text.chars().count(),
        translated_text.chars().count(),
    );

    Ok(MessageBody::TranslateBody(
        tentaflow_protocol::TranslatePayload::Res(TranslateResponse {
            translated_text,
            // Auto-detection of the source language is not surfaced by current
            // LLM backends; leaving None until a detector is wired in.
            detected_source_lang: None,
            model_used: if response.model.is_empty() {
                model_id
            } else {
                response.model
            },
            tokens_used,
        }),
    ))
}
