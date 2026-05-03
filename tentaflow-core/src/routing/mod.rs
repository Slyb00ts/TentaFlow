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

use crate::api::openai::types::{ChatCompletionRequest, ChatCompletionResponse, MessageContent};
use crate::flow_engine::types::FlowContext;

/// Builds a FlowContext from a ChatCompletionRequest. When a user is attached
/// the dispatcher gates per-flow ACL on user_id/role; internal callers
/// (addons, mesh, translate) pass `user = None`.
pub(crate) fn build_flow_context_for_user(
    request: &ChatCompletionRequest,
    stream: bool,
    user: Option<crate::auth::acl::UserContext>,
) -> FlowContext {
    let mut ctx = build_flow_context_inner(request, stream);
    if let Some(u) = user {
        ctx.user_id = Some(u.user_id);
        ctx.user_role = Some(u.role);
    }
    ctx
}

fn build_flow_context_inner(request: &ChatCompletionRequest, stream: bool) -> FlowContext {
    FlowContext {
        request_id: uuid::Uuid::new_v4().to_string(),
        model: request.model.clone(),
        input: request
            .messages
            .last()
            .and_then(|m| m.content.as_ref())
            .map(|c| match c {
                MessageContent::Text(t) => t.clone(),
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
                    .join(" "),
            })
            .unwrap_or_default(),
        messages: request
            .messages
            .iter()
            .filter_map(|m| serde_json::to_value(m).ok())
            .collect(),
        stream,
        service_type: "chat".to_string(),
        original_request: serde_json::to_value(request).ok(),
        session_id: request
            .memory_options
            .as_ref()
            .and_then(|o| o.session_id.clone()),
        person_id: request
            .memory_options
            .as_ref()
            .and_then(|o| o.person_id.clone()),
        speaker_confidence: request
            .memory_options
            .as_ref()
            .and_then(|o| o.speaker_confidence)
            .unwrap_or(0.0),
        speaker_name: request
            .memory_options
            .as_ref()
            .and_then(|o| o.speaker_name.clone()),
        // R4.B: when the chat audio policy guard admits a request to a
        // flow (audio chat → flow with STT node), the audio bytes must
        // reach the flow context — otherwise the STT adapter looks at
        // an empty `ctx.audio_input`, returns an empty transcript, and
        // the flow happily proceeds with a blank input.
        audio_input: request.audio_input.clone(),
        ..Default::default()
    }
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
                        if let crate::api::openai::types::ContentPart::Text { text } = part {
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

#[cfg(test)]
mod build_flow_context_tests {
    use super::*;
    use crate::api::openai::types::Message;

    fn make_request(audio: Option<Vec<u8>>, content: Option<MessageContent>) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "test-model".into(),
            messages: vec![Message {
                role: "user".into(),
                content,
                reasoning_content: None,
                name: None,
                tool_call_id: None,
                tool_calls: None,
            }],
            temperature: None,
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
            memory_options: None,
            audio_input: audio,
        }
    }

    /// R4.B: chat audio policy admits audio chat → flow with STT, so the
    /// audio bytes must travel from `ChatCompletionRequest.audio_input`
    /// into `FlowContext.audio_input`. Pre-fix the field was always
    /// `None`, the STT adapter saw empty data and emitted an empty
    /// transcript without raising an error.
    #[test]
    fn chat_audio_input_propagates_to_flow_context() {
        let request = make_request(Some(vec![1, 2, 3, 4]), None);
        let ctx = build_flow_context_inner(&request, false);
        assert_eq!(ctx.audio_input.as_deref(), Some(&[1, 2, 3, 4][..]));
    }

    /// Negative complement: text chat (no audio_input) yields a
    /// FlowContext with `audio_input = None`. STT adapter on such a
    /// flow legitimately produces an empty transcript.
    #[test]
    fn text_chat_yields_no_flow_audio() {
        let request = make_request(None, Some(MessageContent::Text("hello".into())));
        let ctx = build_flow_context_inner(&request, false);
        assert!(ctx.audio_input.is_none());
    }
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
