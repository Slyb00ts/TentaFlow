// =============================================================================
// Plik: routing/mod.rs
// Opis: Logika routingu — rozwiazywanie aliasow, kierowanie zapytan do backendow.
//       Eksportuje wszystkie podmoduly routera.
// =============================================================================

pub mod audio_stream;
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

/// Stage 3d-0b-final: mapowanie typed `DispatchError` → `CoreError`.
/// Plan v1.5: `Denied` → 404 (nie ujawniamy istnienia modelu klientom
/// bez ACL); pozostałe → 500 z czytelnym message.
pub(crate) fn dispatch_error_to_core(
    err: crate::flow_engine::dispatcher::DispatchError,
    model: &str,
) -> crate::error::CoreError {
    use crate::flow_engine::dispatcher::DispatchError;
    match err {
        DispatchError::Denied { .. } => crate::error::CoreError::ModelNotFound {
            model_name: model.to_string(),
        },
        DispatchError::CompileFailed { flow_id, msg } => crate::error::CoreError::InternalError {
            message: format!("flow {flow_id} compile failed: {msg}"),
            source: None,
        },
        DispatchError::Unsupported { service_type, model } => crate::error::CoreError::InternalError {
            message: format!(
                "synthetic dispatch unsupported for service_type='{service_type}', model='{model}'"
            ),
            source: None,
        },
        DispatchError::Internal(msg) => crate::error::CoreError::InternalError {
            message: format!("flow dispatch: {msg}"),
            source: None,
        },
    }
}
pub use router::{
    BackendMetric, DiarizedSpeaker, Router, RouterMetrics, SpeakerIdentifyResult,
    SttWithDiarization, VoiceInfo,
};

use crate::api::openai::types::{
    ChatCompletionRequest, ChatCompletionResponse, ContentPart, Message, MessageContent,
};
use crate::error::Result;
use crate::flow_engine::dispatcher::FlowRequestMeta;
use crate::flow_engine::envelope::{ChatMessage, ChatRole, FlowEnvelope, FlowValue};

/// Buduje seed envelope + per-request meta z `ChatCompletionRequest`. Trigger
/// adapter konsumuje envelope (model + messages + payload), dispatcher
/// wzbogaca meta o user_id/role gdy ACL'em chroniony.
pub(crate) async fn build_initial_envelope_for_user(
    request: &ChatCompletionRequest,
    user: Option<crate::auth::acl::UserContext>,
    blobs: &std::sync::Arc<dyn crate::flow_engine::blob_store::BlobStore>,
) -> Result<(FlowEnvelope, FlowRequestMeta)> {
    let (mut envelope, mut meta) = build_initial_envelope_inner(request, blobs.as_ref()).await?;
    if let Some(u) = user {
        meta.user_id = Some(u.user_id);
        meta.user_role = Some(u.role);
    }
    let _ = &mut envelope;
    Ok((envelope, meta))
}

async fn build_initial_envelope_inner(
    request: &ChatCompletionRequest,
    blobs: &dyn crate::flow_engine::blob_store::BlobStore,
) -> Result<(FlowEnvelope, FlowRequestMeta)> {
    let mut env = FlowEnvelope::empty();
    env.meta
        .insert("model".into(), serde_json::Value::String(request.model.clone()));

    // Etap 2: request seed params trafiają do envelope.meta. LlmNodeAdapter
    // czyta je przez fallback `node.config -> envelope.meta`, więc operator
    // może override'ować temperature etc. w node config flow, a brak override
    // = użyj wartości z requestu.
    if let Some(t) = request.temperature {
        if let Some(num) = serde_json::Number::from_f64(t as f64) {
            env.meta.insert("temperature".into(), serde_json::Value::Number(num));
        }
    }
    if let Some(mt) = request.max_tokens {
        env.meta.insert("max_tokens".into(), serde_json::Value::Number(mt.into()));
    }
    if let Some(tp) = request.top_p {
        if let Some(num) = serde_json::Number::from_f64(tp as f64) {
            env.meta.insert("top_p".into(), serde_json::Value::Number(num));
        }
    }
    if let Some(fp) = request.frequency_penalty {
        if let Some(num) = serde_json::Number::from_f64(fp as f64) {
            env.meta
                .insert("frequency_penalty".into(), serde_json::Value::Number(num));
        }
    }
    if let Some(pp) = request.presence_penalty {
        if let Some(num) = serde_json::Number::from_f64(pp as f64) {
            env.meta
                .insert("presence_penalty".into(), serde_json::Value::Number(num));
        }
    }

    // Etap 3b: detect image w ostatniej user message (Parts z ImageUrl).
    // Decode data URL → BlobStore.put → payload = FlowValue::Image. Bez
    // image fallback do payload Text per pre-3b zachowanie. HTTP/HTTPS
    // image URLs odrzucone z InvalidRequest (3b nie robi fetch).
    let mut found_image: Option<(crate::flow_engine::blob_store::BlobRef, String)> = None;
    if let Some(last_user) = request
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
    {
        if let Some(MessageContent::Parts(parts)) = &last_user.content {
            for p in parts {
                if let ContentPart::ImageUrl { image_url } = p {
                    let (bytes, mime) = decode_data_url(&image_url.url)?;
                    let blob_ref = blobs.put(bytes, &mime).await.map_err(|e| {
                        crate::error::CoreError::InternalError {
                            message: format!("blob put for image: {e}"),
                            source: None,
                        }
                    })?;
                    found_image = Some((blob_ref, mime));
                    break;
                }
            }
        }
    }

    if let Some((blob_ref, mime)) = found_image {
        env.payload = FlowValue::Image {
            blob_ref,
            mime,
            dims: None,
        };
    } else {
        let payload_text = request
            .messages
            .last()
            .and_then(|m| m.content.as_ref())
            .map(message_content_to_text)
            .unwrap_or_default();
        if !payload_text.is_empty() {
            env.payload = FlowValue::Text(payload_text);
        }
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

    Ok((env, meta))
}

/// Etap 3b: parsuje OpenAI `image_url.url` jako `data:<mime>;base64,<...>`.
/// Zwraca `(bytes, mime)` po sukcesie, `Err(InvalidRequest)` dla:
/// - `http://` lub `https://` URLs (3b nie robi fetch — klient encoduje
///   po swojej stronie).
/// - innych formatów (file://, blob:, broken data URL).
fn decode_data_url(url: &str) -> Result<(Vec<u8>, String)> {
    if url.starts_with("http://") || url.starts_with("https://") {
        return Err(crate::error::CoreError::InvalidRequest {
            message: "image_url.url must be a base64 data URL — HTTP/HTTPS \
                      image URLs are not supported in this stage. Encode the \
                      image client-side as data:<mime>;base64,..."
                .to_string(),
            details: None,
        }
        .into());
    }
    if !url.starts_with("data:") {
        return Err(crate::error::CoreError::InvalidRequest {
            message: format!(
                "image_url.url must be a data URL (data:<mime>;base64,...), got: {}",
                if url.len() > 60 { &url[..60] } else { url }
            ),
            details: None,
        }
        .into());
    }
    // data:image/jpeg;base64,<...>
    let body = &url[5..]; // skip "data:"
    let (header, b64) = body.split_once(',').ok_or_else(|| {
        crate::error::CoreError::InvalidRequest {
            message: "data URL missing comma separator".to_string(),
            details: None,
        }
    })?;
    let mime = match header.split(';').next() {
        Some(m) if !m.is_empty() => m.to_string(),
        _ => {
            return Err(crate::error::CoreError::InvalidRequest {
                message: "data URL missing mime type".to_string(),
                details: None,
            }
            .into());
        }
    };
    if !header.contains("base64") {
        return Err(crate::error::CoreError::InvalidRequest {
            message: "only base64-encoded data URLs are supported".to_string(),
            details: None,
        }
        .into());
    }
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| crate::error::CoreError::InvalidRequest {
            message: format!("base64 decode failed: {e}"),
            details: None,
        })?;
    Ok((bytes, mime))
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
    use crate::flow_engine::envelope::{ChatMessageContent, MessagePart};
    let content = match m.content.as_ref() {
        Some(MessageContent::Text(t)) => ChatMessageContent::Text(t.clone()),
        Some(MessageContent::Parts(_parts)) => {
            // Etap 3b: zostawiamy tylko text parts. ImageUrl ekstraktowany
            // do payload w build_initial_envelope_inner (raz, przy pierwszym
            // image w ostatniej user message). Tu nie próbujemy ich
            // zachować, bo wymagałoby to async (BlobStore.put per part).
            // Vision flow widzi obraz przez payload + tekst pytania przez
            // resolve_prompt scanning ostatniej message text part.
            let text: String = _parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.clone()),
                    ContentPart::ImageUrl { .. } => None,
                })
                .collect::<Vec<_>>()
                .join(" ");
            ChatMessageContent::Text(text)
        }
        None => ChatMessageContent::Text(String::new()),
    };
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

#[cfg(test)]
mod data_url_tests {
    use super::*;

    #[test]
    fn decode_data_url_jpeg_base64() {
        let url = "data:image/jpeg;base64,/9j/4AAQ";
        let (bytes, mime) = decode_data_url(url).unwrap();
        assert_eq!(mime, "image/jpeg");
        assert!(!bytes.is_empty());
    }

    #[test]
    fn decode_data_url_rejects_http() {
        let url = "https://example.com/image.jpg";
        let err = decode_data_url(url).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("http"));
    }

    #[test]
    fn decode_data_url_rejects_non_data() {
        let url = "file:///etc/passwd";
        let err = decode_data_url(url).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("data url"));
    }

    #[test]
    fn decode_data_url_rejects_non_base64() {
        let url = "data:image/jpeg,raw_bytes_here";
        let err = decode_data_url(url).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("base64"));
    }
}
