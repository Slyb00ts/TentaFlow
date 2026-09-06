// =============================================================================
// Plik: routing/mod.rs
// Opis: Logika routingu — rozwiazywanie aliasow, kierowanie zapytan do backendow.
//       Eksportuje wszystkie podmoduly routera.
// =============================================================================

pub mod audio_stream;
pub mod chat;
pub mod chat_template;
pub mod documents;
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
        DispatchError::Unsupported {
            service_type,
            model,
        } => crate::error::CoreError::InternalError {
            message: format!(
                "direct dispatch unsupported for service_type='{service_type}', model='{model}'"
            ),
            source: None,
        },
        DispatchError::Internal(msg) => crate::error::CoreError::InternalError {
            message: format!("flow dispatch: {msg}"),
            source: None,
        },
        DispatchError::SttServiceUnavailable => crate::error::CoreError::SttServiceUnavailable,
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
use crate::flow_engine::dispatcher::{FlowActor, FlowOrigin, FlowRequestMeta};
use crate::flow_engine::envelope::{ChatMessage, ChatRole, FlowEnvelope, FlowValue};
use sha2::{Digest, Sha256};

/// Buduje seed envelope + per-request meta z `ChatCompletionRequest`. Trigger
/// adapter konsumuje envelope (model + messages + payload), dispatcher
/// wzbogaca meta o user_id/role gdy ACL'em chroniony.
pub(crate) async fn build_initial_envelope_for_user(
    request: &ChatCompletionRequest,
    user: Option<crate::auth::acl::UserContext>,
    origin: FlowOrigin,
    actor: FlowActor,
    blobs: &std::sync::Arc<dyn crate::flow_engine::blob_store::BlobStore>,
) -> Result<(FlowEnvelope, FlowRequestMeta)> {
    let (envelope, mut meta) =
        build_initial_envelope_inner(request, origin, actor, blobs.as_ref()).await?;
    if let Some(u) = user {
        meta.user_id = Some(u.user_id);
        meta.user_role = Some(u.role);
    }
    if meta.origin == FlowOrigin::Api {
        let caller_session = meta.session_id.take();
        meta.session_id = caller_session
            .as_deref()
            .and_then(|caller| mint_api_session_id(&meta, caller));
    }
    Ok((envelope, meta))
}

/// `conversation_messages` is keyed by `session_id` and by nothing else — the
/// table carries no owner column, so on `/v1` the request body itself names
/// which conversation gets replayed into the prompt. A caller who guesses (or
/// is told) another tenant's session id would read that tenant's history. The
/// server therefore stops trusting the wire value and mints the effective key
/// from the authenticated principal plus the caller's name: the caller still
/// chooses its own session names, but a foreign namespace has no address it
/// can type. `None` means "this request has no conversation memory", which is
/// the safe reading of "no identity" — an unowned shared bucket is the same
/// defect one level down.
fn mint_api_session_id(meta: &FlowRequestMeta, caller_session: &str) -> Option<String> {
    // Strongest identity available, in order: the authenticated user, the user
    // an API key resolved to, then the key itself — a service key has no human
    // behind it but is still a distinct principal, so it keeps its memory
    // instead of losing it. All three come from the server-minted
    // `FlowRequestMeta`, never from the request body or a header.
    let identity = meta
        .user_id
        .as_deref()
        .or(meta.actor_user_id.as_deref())
        .or(meta.actor_id.as_deref())?;

    // Length-prefixed framing, because a plain `identity:caller` join is not
    // injective: ("a", "b:c") and ("a:b", "c") would mint one key and hand two
    // principals the same conversation — the very collision this exists to
    // prevent.
    let mut hasher = Sha256::new();
    hasher.update(SESSION_KEY_DOMAIN);
    hasher.update((identity.len() as u64).to_le_bytes());
    hasher.update(identity.as_bytes());
    hasher.update((caller_session.len() as u64).to_le_bytes());
    hasher.update(caller_session.as_bytes());
    Some(format!("v1s-{}", hex::encode(hasher.finalize())))
}

/// Domain separator: keeps these digests from ever colliding with another
/// SHA-256 of the same bytes used elsewhere for a different purpose.
const SESSION_KEY_DOMAIN: &[u8] = b"tentaflow/v1-session/1";

async fn build_initial_envelope_inner(
    request: &ChatCompletionRequest,
    origin: FlowOrigin,
    actor: FlowActor,
    blobs: &dyn crate::flow_engine::blob_store::BlobStore,
) -> Result<(FlowEnvelope, FlowRequestMeta)> {
    let mut env = FlowEnvelope::empty();
    // Empty request.model stays OUT of meta: the dashboard sends no model for
    // an explicitly selected flow, and an empty seed here would let an llm
    // node without a pinned model silently dispatch to "" (or, historically,
    // to whatever model the client defaulted to). Missing model must surface
    // as the adapter's hard "llm adapter: no model" error instead.
    if !request.model.is_empty() {
        env.meta.insert(
            "model".into(),
            serde_json::Value::String(request.model.clone()),
        );
    }

    // Etap 2: request seed params trafiają do envelope.meta. LlmNodeAdapter
    // czyta je przez fallback `node.config -> envelope.meta`, więc operator
    // może override'ować temperature etc. w node config flow, a brak override
    // = użyj wartości z requestu.
    if let Some(t) = request.temperature {
        if let Some(num) = serde_json::Number::from_f64(t as f64) {
            env.meta
                .insert("temperature".into(), serde_json::Value::Number(num));
        }
    }
    if let Some(mt) = request.max_tokens {
        env.meta
            .insert("max_tokens".into(), serde_json::Value::Number(mt.into()));
    }
    if let Some(tp) = request.top_p {
        if let Some(num) = serde_json::Number::from_f64(tp as f64) {
            env.meta
                .insert("top_p".into(), serde_json::Value::Number(num));
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
    if let Some(last_user) = request.messages.iter().rev().find(|m| m.role == "user") {
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
        env.meta
            .insert("has_audio_input".into(), serde_json::Value::Bool(true));
    }

    let mut meta = FlowRequestMeta::new(uuid::Uuid::new_v4().to_string(), origin, actor);
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
pub fn decode_data_url(url: &str) -> Result<(Vec<u8>, String)> {
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
    let (header, b64) =
        body.split_once(',')
            .ok_or_else(|| crate::error::CoreError::InvalidRequest {
                message: "data URL missing comma separator".to_string(),
                details: None,
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
    use crate::flow_engine::envelope::ChatMessageContent;
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
                    ContentPart::ImageUrl { .. } | ContentPart::InputAudio { .. } => None,
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
        reasoning_content: m.reasoning_content.clone(),
        name: m.name.clone(),
        tool_call_id: m.tool_call_id.clone(),
        // Inbound history replay: an external tool loop resends the assistant
        // message that requested the calls — dropping them here would break
        // the call/result pairing the backend validates.
        tool_calls: m.tool_calls.as_ref().map(|tcs| {
            tcs.iter()
                .map(|tc| crate::flow_engine::envelope::LlmToolCall {
                    id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    arguments: tc.function.arguments.clone(),
                })
                .collect()
        }),
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
                reasoning_content: m.reasoning_content.clone(),
            }
        })
        .collect()
}

/// Czy którakolwiek wiadomość niesie obraz (request vision/multimodal). Decyduje
/// czy MeshForward ma iść jako `ModelPayload::Vision` (niesie obrazy) zamiast
/// `Completion` (tekst-only) — bez tego obraz gubi się na hopie mesh.
pub(crate) fn messages_have_image(messages: &[crate::api::openai::types::Message]) -> bool {
    messages.iter().any(|m| {
        matches!(
            &m.content,
            Some(MessageContent::Parts(parts))
                if parts.iter().any(|p| matches!(p, ContentPart::ImageUrl { .. }))
        )
    })
}

/// Konwertuje OpenAI messages na protocol `VisionMessage` (tekst + obrazy
/// ZACHOWANE). Lustro `openai_messages_to_protocol`, ale dla ścieżki vision —
/// CompletionPayload niesie tylko tekst, więc multimodal idzie przez VisionPayload.
pub(crate) fn openai_messages_to_vision(
    messages: &[crate::api::openai::types::Message],
) -> Vec<tentaflow_protocol::VisionMessage> {
    messages
        .iter()
        .map(|m| {
            let content: Vec<tentaflow_protocol::VisionContentPart> = match &m.content {
                Some(MessageContent::Text(text)) => {
                    vec![tentaflow_protocol::VisionContentPart::Text { text: text.clone() }]
                }
                Some(MessageContent::Parts(parts)) => parts
                    .iter()
                    .filter_map(|part| match part {
                        ContentPart::Text { text } => {
                            Some(tentaflow_protocol::VisionContentPart::Text { text: text.clone() })
                        }
                        ContentPart::ImageUrl { image_url } => {
                            Some(tentaflow_protocol::VisionContentPart::ImageUrl {
                                url: image_url.url.clone(),
                                detail: image_url.detail.clone(),
                            })
                        }
                        // The vision wire has no audio part and inventing one
                        // would be a protocol change; a vision route simply
                        // carries no sound.
                        ContentPart::InputAudio { .. } => None,
                    })
                    .collect(),
                None => Vec::new(),
            };
            tentaflow_protocol::VisionMessage {
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

#[cfg(test)]
mod reasoning_tests {
    use super::*;

    #[test]
    fn openai_reasoning_content_survives_mesh_message_conversion() {
        let messages = vec![crate::api::openai::types::Message {
            role: "assistant".to_string(),
            content: Some(MessageContent::Text("answer".to_string())),
            reasoning_content: Some("reasoning".to_string()),
            ..Default::default()
        }];

        let protocol_messages = openai_messages_to_protocol(&messages);

        assert_eq!(protocol_messages.len(), 1);
        assert_eq!(protocol_messages[0].content, "answer");
        assert_eq!(
            protocol_messages[0].reasoning_content.as_deref(),
            Some("reasoning")
        );
    }
}

#[cfg(test)]
mod provenance_tests {
    use super::*;
    use crate::flow_engine::blob_store::InMemoryBlobStore;

    pub(super) fn chat_request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "m".into(),
            messages: vec![Message {
                audio: None,
                role: "user".into(),
                content: Some(MessageContent::Text("hi".into())),
                reasoning_content: None,
                name: None,
                tool_calls: None,
                tool_call_id: None,
            }],
            temperature: None,
            reasoning_effort: None,
            modalities: None,
            audio: None,
            max_tokens: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            stream: false,
            stream_options: None,
            user: None,
            response_format: None,
            tools: None,
            tool_choice: None,
            n: None,
            memory_options: None,
            audio_input: None,
            extra: Default::default(),
        }
    }

    /// §2.5 — the `/v1` entry point. `actor_user_id` carries the user an API key
    /// resolves to, which the unified server resolved while verifying the key.
    #[tokio::test]
    async fn v1_entry_stamps_api_origin_and_user_bound_key() {
        let blobs: std::sync::Arc<dyn crate::flow_engine::blob_store::BlobStore> =
            std::sync::Arc::new(InMemoryBlobStore::new());
        let (_env, meta) = build_initial_envelope_for_user(
            &chat_request(),
            Some(crate::auth::acl::UserContext::new("user-7", "user")),
            FlowOrigin::Api,
            FlowActor::api_key("key-42", Some("user-7".to_string())),
            &blobs,
        )
        .await
        .expect("seed envelope");
        assert_eq!(meta.origin, FlowOrigin::Api);
        assert_eq!(
            meta.actor_kind,
            crate::flow_engine::dispatcher::ActorKind::ApiKey
        );
        assert_eq!(meta.actor_id.as_deref(), Some("key-42"));
        assert_eq!(meta.actor_user_id.as_deref(), Some("user-7"));
    }

    /// A service key ("group" / "general") has no user behind it — `NULL`, not
    /// an empty string, so the UI can say so explicitly.
    #[tokio::test]
    async fn v1_entry_marks_service_key_without_bound_user() {
        let blobs: std::sync::Arc<dyn crate::flow_engine::blob_store::BlobStore> =
            std::sync::Arc::new(InMemoryBlobStore::new());
        let (_env, meta) = build_initial_envelope_for_user(
            &chat_request(),
            None,
            FlowOrigin::Api,
            FlowActor::api_key("key-svc", None),
            &blobs,
        )
        .await
        .expect("seed envelope");
        assert_eq!(meta.origin, FlowOrigin::Api);
        assert_eq!(meta.actor_id.as_deref(), Some("key-svc"));
        assert_eq!(meta.actor_user_id, None);
    }
}

/// The `/v1` session key is a tenancy boundary: `conversation_messages` has no
/// owner column, so the key IS the access control. These tests pin the four
/// properties that make a foreign conversation unaddressable.
#[cfg(test)]
mod v1_session_key_tests {
    use super::*;
    use crate::flow_engine::blob_store::InMemoryBlobStore;

    fn request_with_session(session: &str) -> ChatCompletionRequest {
        let mut request = super::provenance_tests::chat_request();
        request.memory_options = Some(crate::api::openai::types::MemoryOptions {
            session_id: Some(session.to_string()),
            ..Default::default()
        });
        request
    }

    async fn session_key(
        user: Option<&str>,
        actor: FlowActor,
        origin: FlowOrigin,
        caller_session: &str,
    ) -> Option<String> {
        let blobs: std::sync::Arc<dyn crate::flow_engine::blob_store::BlobStore> =
            std::sync::Arc::new(InMemoryBlobStore::new());
        let (_env, meta) = build_initial_envelope_for_user(
            &request_with_session(caller_session),
            user.map(|u| crate::auth::acl::UserContext::new(u, "user")),
            origin,
            actor,
            &blobs,
        )
        .await
        .expect("seed envelope");
        meta.session_id
    }

    /// The attack itself: caller B replays caller A's `session_id` verbatim and
    /// must land somewhere else.
    #[tokio::test]
    async fn same_caller_session_isolates_two_identities() {
        let victim = session_key(
            Some("user-victim"),
            FlowActor::api_key("key-victim", Some("user-victim".to_string())),
            FlowOrigin::Api,
            "shared-name",
        )
        .await
        .expect("victim key");
        let attacker = session_key(
            Some("user-attacker"),
            FlowActor::api_key("key-attacker", Some("user-attacker".to_string())),
            FlowOrigin::Api,
            "shared-name",
        )
        .await
        .expect("attacker key");

        assert_ne!(victim, attacker);
        assert_ne!(victim, "shared-name");
        assert_ne!(attacker, "shared-name");
    }

    /// Two distinct service keys are two distinct principals: each keeps memory,
    /// neither reaches the other's.
    #[tokio::test]
    async fn two_service_keys_do_not_share_a_session() {
        let first = session_key(
            None,
            FlowActor::api_key("key-a", None),
            FlowOrigin::Api,
            "shared-name",
        )
        .await
        .expect("first service key");
        let second = session_key(
            None,
            FlowActor::api_key("key-b", None),
            FlowOrigin::Api,
            "shared-name",
        )
        .await
        .expect("second service key");

        assert_ne!(first, second);
    }

    /// Isolation is worthless if it also breaks memory: the legitimate owner must
    /// reach the same bucket on every request, and still keep separate
    /// conversations apart.
    #[tokio::test]
    async fn same_identity_keeps_a_stable_key_per_conversation() {
        let actor = || FlowActor::api_key("key-1", Some("user-1".to_string()));
        let first = session_key(Some("user-1"), actor(), FlowOrigin::Api, "chat-a")
            .await
            .expect("first request");
        let second = session_key(Some("user-1"), actor(), FlowOrigin::Api, "chat-a")
            .await
            .expect("second request");
        let other_conversation = session_key(Some("user-1"), actor(), FlowOrigin::Api, "chat-b")
            .await
            .expect("other conversation");

        assert_eq!(first, second);
        assert_ne!(first, other_conversation);
    }

    /// No identity means no memory. Sharing one unowned namespace would rebuild
    /// the very defect this key exists to close.
    #[tokio::test]
    async fn request_without_identity_gets_no_session_memory() {
        let key = session_key(None, FlowActor::system(), FlowOrigin::Api, "chat-a").await;
        assert_eq!(key, None);
    }

    /// Injectivity: a naive `identity:caller` join would hand ("u", "a:b") and
    /// ("u:a", "b") the same conversation.
    #[tokio::test]
    async fn separator_shifts_between_identity_and_session_do_not_collide() {
        let left = session_key(
            Some("u"),
            FlowActor::api_key("key-1", Some("u".to_string())),
            FlowOrigin::Api,
            "a:b",
        )
        .await
        .expect("left key");
        let right = session_key(
            Some("u:a"),
            FlowActor::api_key("key-1", Some("u:a".to_string())),
            FlowOrigin::Api,
            "b",
        )
        .await
        .expect("right key");

        assert_ne!(left, right);
    }

    /// Only `/v1` is re-keyed. On surfaces where the server already picked the id
    /// (dashboard chat, project chat) rewriting it would orphan every existing
    /// conversation.
    #[tokio::test]
    async fn non_api_origins_keep_their_server_chosen_session_id() {
        for origin in [FlowOrigin::Chat, FlowOrigin::Project, FlowOrigin::Addon] {
            let key = session_key(
                Some("user-1"),
                FlowActor::user("user-1"),
                origin,
                "conversation-42",
            )
            .await;
            assert_eq!(key.as_deref(), Some("conversation-42"));
        }
    }
}
