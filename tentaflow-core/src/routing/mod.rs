// =============================================================================
// Plik: routing/mod.rs
// Opis: Logika routingu — rozwiazywanie aliasow, kierowanie zapytan do backendow.
//       Eksportuje wszystkie podmoduly routera.
// =============================================================================

pub mod chat;
pub mod chat_template;
pub mod embeddings;
pub mod live_metrics;
pub mod middleware;
pub mod router;
pub mod stream_helpers;
pub mod streaming;
pub mod stt;
pub mod transcript_store;
pub mod tts;
pub mod video_pipeline;

// Re-eksporty publicznych typow
pub use middleware::{ResolvedRoute, RouteMetadata, RouteResult};
pub use router::{
    BackendMetric, DiarizedSpeaker, RequestMetrics, Router, RouterMetrics, SpeakerIdentifyResult,
    SttWithDiarization, VoiceInfo,
};

use crate::api::openai::types::{
    ChatCompletionRequest, ChatCompletionResponse, ContentPart, Message, MessageContent,
};
use crate::flow_engine::dispatcher::FlowRequestMeta;
use crate::flow_engine::envelope::{ChatMessage, ChatRole, FlowEnvelope, FlowValue};

/// Buduje seed envelope + per-request meta z `ChatCompletionRequest`. Trigger
/// adapter konsumuje envelope (model + messages + payload), dispatcher
/// wzbogaca meta o user_id/role gdy ACL'em chroniony.
pub(crate) fn build_initial_envelope_for_user(
    request: &ChatCompletionRequest,
    user: Option<crate::auth::acl::UserContext>,
) -> (FlowEnvelope, FlowRequestMeta) {
    let (mut envelope, mut meta) = build_initial_envelope_inner(request);
    if let Some(u) = user {
        meta.user_id = Some(u.user_id);
        meta.user_role = Some(u.role);
    }
    let _ = &mut envelope; // referencja by IDE nie traktował jako unused
    (envelope, meta)
}

fn build_initial_envelope_inner(request: &ChatCompletionRequest) -> (FlowEnvelope, FlowRequestMeta) {
    let mut env = FlowEnvelope::empty();
    env.meta
        .insert("model".into(), serde_json::Value::String(request.model.clone()));

    let payload_text = request
        .messages
        .last()
        .and_then(|m| m.content.as_ref())
        .map(message_content_to_text)
        .unwrap_or_default();
    if !payload_text.is_empty() {
        env.payload = FlowValue::Text(payload_text);
    }

    env.context.messages = request
        .messages
        .iter()
        .filter_map(message_to_chat_message)
        .collect();

    if request.audio_input.is_some() {
        // R4.B: audio chat path. Stage 1d zapisuje sygnał w meta — pełny
        // multimodal trigger (Audio payload via BlobStore) wraca w stage 2.
        env.meta.insert("has_audio_input".into(), serde_json::Value::Bool(true));
    }

    let mut meta = FlowRequestMeta::new(uuid::Uuid::new_v4().to_string());
    if let Some(opts) = request.memory_options.as_ref() {
        meta.session_id = opts.session_id.clone();
        if let Some(person_id) = &opts.person_id {
            env.meta.insert(
                "person_id".into(),
                serde_json::Value::String(person_id.clone()),
            );
        }
        if let Some(name) = &opts.speaker_name {
            env.meta.insert(
                "speaker_name".into(),
                serde_json::Value::String(name.clone()),
            );
        }
        if let Some(conf) = opts.speaker_confidence {
            if let Some(num) = serde_json::Number::from_f64(conf as f64) {
                env.meta
                    .insert("speaker_confidence".into(), serde_json::Value::Number(num));
            }
        }
    }

    (env, meta)
}

fn message_content_to_text(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(t) => t.clone(),
        MessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|p| {
                if let ContentPart::Text { text } = p {
                    Some(text.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

fn message_to_chat_message(m: &Message) -> Option<ChatMessage> {
    let role = match m.role.as_str() {
        "system" => ChatRole::System,
        "user" => ChatRole::User,
        "assistant" => ChatRole::Assistant,
        "tool" => ChatRole::Tool,
        _ => return None,
    };
    let content = m.content.as_ref().map(message_content_to_text).unwrap_or_default();
    Some(ChatMessage {
        role,
        content,
        name: m.name.clone(),
        tool_call_id: m.tool_call_id.clone(),
    })
}

/// Konwertuje OpenAI messages na protocol messages (rola + tekst).
pub(crate) fn openai_messages_to_protocol(
    messages: &[crate::api::openai::types::Message],
) -> Vec<tentaflow_protocol::Message> {
    messages
        .iter()
        .map(|m| {
            let content = match &m.content {
                Some(MessageContent::Text(text)) => text.clone(),
                Some(MessageContent::Parts(parts)) => parts
                    .iter()
                    .filter_map(|part| {
                        if let ContentPart::Text { text } = part {
                            Some(text.clone())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(""),
                None => String::new(),
            };
            tentaflow_protocol::Message {
                role: m.role.clone(),
                content,
            }
        })
        .collect()
}

/// Wyciaga tekst z pierwszego choice w ChatCompletionResponse.
pub(crate) fn extract_response_text(response: &ChatCompletionResponse) -> String {
    response
        .choices
        .first()
        .and_then(|c| c.message.content.as_ref())
        .map(|content| match content {
            MessageContent::Text(text) => text.clone(),
            MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|p| {
                    if let ContentPart::Text { text } = p {
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
