// =============================================================================
// Plik: flow_engine/converter.rs
// Opis: Konwertery FlowExecutionOutcome → response w formatach OpenAI-compat
//       (chat completions, embeddings). Streaming chunk converter siedzi w
//       routing/streaming.rs (mapuje EnvelopeDelta::Llm → ChatCompletionChunk).
// =============================================================================

use std::borrow::Cow;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::openai::types::{
    ChatCompletionResponse, Choice, EmbeddingData, EmbeddingResponse, EmbeddingUsage, Message,
    MessageContent, Usage,
};
use crate::error::{CoreError, Result as CoreResult};
use crate::flow_engine::envelope::{FlowExecutionOutcome, FlowValue};

pub fn flow_outcome_to_chat_response(
    outcome: &FlowExecutionOutcome,
    model: &str,
) -> ChatCompletionResponse {
    let content: Cow<str> = match &outcome.final_envelope.payload {
        FlowValue::Text(t) => Cow::Borrowed(t.as_str()),
        FlowValue::Empty => Cow::Borrowed(""),
        other => Cow::Owned(serde_json::to_string(&payload_to_json(other)).unwrap_or_default()),
    };
    let finish_reason = outcome
        .finish_reason
        .as_openai_str()
        .map(|s| s.to_string());

    ChatCompletionResponse {
        id: generate_response_id(),
        object: "chat.completion".to_string(),
        created: unix_timestamp(),
        model: model.to_string(),
        choices: vec![Choice {
            index: 0,
            message: Message {
                role: "assistant".to_string(),
                content: Some(MessageContent::Text(content.into_owned())),
                reasoning_content: None,
                name: None,
                tool_calls: None,
                tool_call_id: None,
            },
            finish_reason,
            logprobs: None,
        }],
        usage: Some(Usage {
            prompt_tokens: outcome.usage.prompt_tokens as u32,
            completion_tokens: outcome.usage.completion_tokens as u32,
            total_tokens: outcome.usage.total_tokens as u32,
        }),
        system_fingerprint: None,
        transcribed_text: None,
        speaker_id: None,
        speaker_name: None,
        speaker_confidence: None,
        detected_intent: None,
        detected_tools: None,
    }
}

pub fn flow_outcome_to_embedding_response(
    outcome: &FlowExecutionOutcome,
    model: &str,
) -> CoreResult<EmbeddingResponse> {
    let data = match &outcome.final_envelope.payload {
        FlowValue::Embedding(v) => vec![EmbeddingData {
            object: "embedding".to_string(),
            index: 0,
            embedding: v.clone(),
        }],
        FlowValue::Json(v) => parse_embedding_batch(v)?,
        FlowValue::Empty => Vec::new(),
        other => {
            return Err(CoreError::InternalError {
                message: format!(
                    "embedding flow returned unexpected payload kind: {}",
                    other.kind()
                ),
                source: None,
            }
            .into());
        }
    };
    Ok(EmbeddingResponse {
        object: "list".to_string(),
        data,
        model: model.to_string(),
        usage: EmbeddingUsage {
            prompt_tokens: outcome.usage.prompt_tokens as u32,
            total_tokens: outcome.usage.total_tokens as u32,
        },
    })
}

fn parse_embedding_batch(v: &serde_json::Value) -> CoreResult<Vec<EmbeddingData>> {
    let arr = v
        .get("embeddings")
        .and_then(|e| e.as_array())
        .ok_or_else(|| {
            anyhow::Error::from(CoreError::InternalError {
                message: "embedding batch missing 'embeddings' field".to_string(),
                source: None,
            })
        })?;
    arr.iter()
        .enumerate()
        .map(|(i, vec_val)| {
            let embedding = vec_val
                .as_array()
                .ok_or_else(|| {
                    anyhow::Error::from(CoreError::InternalError {
                        message: format!("embedding[{i}] not array"),
                        source: None,
                    })
                })?
                .iter()
                .map(|x| x.as_f64().map(|f| f as f32))
                .collect::<Option<Vec<f32>>>()
                .ok_or_else(|| {
                    anyhow::Error::from(CoreError::InternalError {
                        message: format!("embedding[{i}] non-numeric"),
                        source: None,
                    })
                })?;
            Ok(EmbeddingData {
                object: "embedding".to_string(),
                index: i as u32,
                embedding,
            })
        })
        .collect()
}

fn payload_to_json(v: &FlowValue) -> serde_json::Value {
    match v {
        FlowValue::Empty => serde_json::Value::Null,
        FlowValue::Text(t) => serde_json::Value::String(t.clone()),
        FlowValue::Json(v) => v.clone(),
        FlowValue::Audio { blob_ref, mime, .. } => serde_json::json!({
            "type": "audio",
            "blob_id": blob_ref.id,
            "mime": mime,
        }),
        FlowValue::Image { blob_ref, mime, .. } => serde_json::json!({
            "type": "image",
            "blob_id": blob_ref.id,
            "mime": mime,
        }),
        FlowValue::Video { blob_ref, mime, .. } => serde_json::json!({
            "type": "video",
            "blob_id": blob_ref.id,
            "mime": mime,
        }),
        FlowValue::Embedding(e) => serde_json::json!({"type":"embedding","values":e}),
    }
}

fn generate_response_id() -> String {
    format!("flow-{}", uuid::Uuid::new_v4())
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::envelope::{FinishReason, FlowEnvelope, TokenUsage};

    fn outcome(payload: FlowValue, finish: FinishReason) -> FlowExecutionOutcome {
        let mut env = FlowEnvelope::empty();
        env.payload = payload;
        FlowExecutionOutcome {
            final_envelope: env,
            trace: vec![],
            usage: TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            },
            finish_reason: finish,
            total_latency_ms: 42,
            error: None,
        }
    }

    #[test]
    fn chat_response_text_payload() {
        let r = flow_outcome_to_chat_response(
            &outcome(FlowValue::Text("hi".into()), FinishReason::Stop),
            "m",
        );
        match r.choices[0].message.content {
            Some(MessageContent::Text(ref s)) => assert_eq!(s, "hi"),
            _ => panic!("expected text content"),
        }
        assert_eq!(r.choices[0].finish_reason.as_deref(), Some("stop"));
        assert_eq!(r.usage.as_ref().unwrap().total_tokens, 15);
    }

    #[test]
    fn chat_response_cancelled_finish_is_null() {
        let r = flow_outcome_to_chat_response(
            &outcome(FlowValue::Text("x".into()), FinishReason::Cancelled),
            "m",
        );
        assert!(r.choices[0].finish_reason.is_none());
    }

    #[test]
    fn embedding_response_single_vector() {
        let r = flow_outcome_to_embedding_response(
            &outcome(FlowValue::Embedding(vec![0.1, 0.2]), FinishReason::Stop),
            "m",
        )
        .unwrap();
        assert_eq!(r.data.len(), 1);
        assert_eq!(r.data[0].embedding, vec![0.1, 0.2]);
    }
}
