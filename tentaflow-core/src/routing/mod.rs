// =============================================================================
// Plik: routing/mod.rs
// Opis: Logika routingu — rozwiazywanie aliasow, kierowanie zapytan do backendow.
//       Eksportuje wszystkie podmoduly routera.
// =============================================================================

pub mod acl;
pub mod backend;
pub mod chat;
pub mod chat_template;
pub mod embeddings;
pub mod live_metrics;
pub mod loadbalancer;
pub mod local_inference;
pub mod local_stt;
pub mod meeting_transcript;
pub mod middleware;
pub mod reverse_request;
pub mod router;
pub mod service_manager;
pub mod stream_helpers;
pub mod streaming;
pub mod stt;
pub mod transcript_store;
pub mod tts;

// Re-eksporty publicznych typow
pub use middleware::{BackendHandle, ResolvedRoute, RouteMetadata, RouteResult};
pub use router::{
    BackendMetric, DiarizedSpeaker, RequestMetrics, Router, RouterMetrics, SpeakerIdentifyResult,
    SttWithDiarization, VoiceInfo,
};

use crate::api::openai::types::{ChatCompletionRequest, ChatCompletionResponse, MessageContent};
use crate::flow_engine::types::FlowContext;
use tentaflow_protocol::{RAGParams, RAGPayload, SearchMode};
use tracing::warn;

/// Buduje FlowContext z ChatCompletionRequest — wspolna logika dla streaming i non-streaming.
pub(crate) fn build_flow_context(request: &ChatCompletionRequest, stream: bool) -> FlowContext {
    build_flow_context_for_user(request, stream, None)
}

/// Wariant z user context — propaguje user_id+role do FlowContext zeby
/// dispatcher mogl gateowac flow ACL.
pub(crate) fn build_flow_context_for_user(
    request: &ChatCompletionRequest,
    stream: bool,
    user: Option<crate::routing::acl::UserContext>,
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
        ..Default::default()
    }
}

/// Buduje RAGPayload z parametrow RAG w ChatCompletionRequest.
/// Zwraca (RAGPayload, requires_llm, requires_audio) lub None jesli brak rag_options.
pub(crate) fn build_rag_payload(
    request: &ChatCompletionRequest,
    query: String,
    context: Option<tentaflow_protocol::RAGContext>,
) -> (RAGPayload, bool, bool) {
    let rag_opts = request.rag_options.as_ref();

    let top_k = rag_opts.and_then(|opts| opts.top_k).unwrap_or(5);
    let min_similarity = rag_opts.and_then(|opts| opts.min_similarity).unwrap_or(0.7);
    let use_reranking = rag_opts.and_then(|opts| opts.use_reranking);
    let requires_llm = rag_opts.and_then(|opts| opts.requires_llm).unwrap_or(true);
    let requires_audio = if !requires_llm {
        false
    } else {
        rag_opts
            .and_then(|opts| opts.requires_audio)
            .unwrap_or(false)
    };

    let search_modes = if let Some(modes_str) = rag_opts.and_then(|opts| opts.search_modes.as_ref())
    {
        let modes: Vec<_> = modes_str
            .iter()
            .filter_map(|s| match s.as_str() {
                "FullTextSearch" => Some(SearchMode::FullTextSearch),
                "VectorSearch" => Some(SearchMode::VectorSearch),
                "HiRAG" => Some(SearchMode::HiRAG),
                "GSW" => Some(SearchMode::GSW),
                _ => {
                    warn!("Nieznany search mode: '{}' - pomijam", s);
                    None
                }
            })
            .collect();
        if modes.is_empty() {
            warn!("search_modes jest puste - uzywam domyslnej kombinacji");
            vec![SearchMode::VectorSearch, SearchMode::FullTextSearch]
        } else {
            modes
        }
    } else {
        vec![SearchMode::VectorSearch, SearchMode::FullTextSearch]
    };

    let rag_payload = RAGPayload {
        query,
        context,
        params: RAGParams {
            top_k,
            min_similarity,
            use_reranking,
        },
        requires_llm_processing: requires_llm,
        requires_audio_output: requires_audio,
        search_modes,
    };

    (rag_payload, requires_llm, requires_audio)
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
