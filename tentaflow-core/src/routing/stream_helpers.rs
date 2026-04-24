// =============================================================================
// Plik: routing/stream_helpers.rs
// Opis: Wspoldzielone helpery konwertujace backendowe stream chunki (QUIC
//       ModelStreamChunk, HTTP SSE) na ChatCompletionChunk. Uzywane przez
//       routing/streaming.rs oraz flow_engine::adapters::llm.
// =============================================================================

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures::stream::StreamExt;
use futures::Stream;

use crate::api::openai::types::{ChatCompletionChunk, ChunkChoice, Delta};
use crate::error::Result;
use tentaflow_protocol::{ModelStreamChunk, StreamChunkType};

/// Konwertuje strumien QUIC ModelStreamChunk na strumien ChatCompletionChunk
/// zgodny z OpenAI SSE. Pierwszy chunk dostaje role=assistant; dalsze delta-only.
/// `Done` mapuje sie na chunk z `finish_reason="stop"`. `Metadata` jest pomijana.
pub fn quic_stream_to_openai_chunks<S>(
    quic_stream: S,
    model_name: String,
) -> Pin<Box<dyn Stream<Item = Result<ChatCompletionChunk>> + Send>>
where
    S: Stream<Item = std::result::Result<ModelStreamChunk, crate::error::CoreError>>
        + Send
        + 'static,
{
    let chat_id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
    let created = chrono::Utc::now().timestamp() as u64;
    let is_first = Arc::new(AtomicBool::new(true));

    let converted = quic_stream.filter_map(move |chunk_result| {
        let chat_id = chat_id.clone();
        let model_name = model_name.clone();
        let is_first = is_first.clone();

        async move {
            match chunk_result {
                Ok(stream_chunk) => match stream_chunk.chunk {
                    StreamChunkType::TextDelta(text) => {
                        let first = is_first.swap(false, Ordering::SeqCst);
                        Some(Ok(make_chunk(
                            chat_id,
                            created,
                            model_name,
                            first,
                            Some(text),
                            None,
                            None,
                        )))
                    }
                    StreamChunkType::ReasoningDelta(reasoning) => {
                        let first = is_first.swap(false, Ordering::SeqCst);
                        Some(Ok(make_chunk(
                            chat_id,
                            created,
                            model_name,
                            first,
                            None,
                            Some(reasoning),
                            None,
                        )))
                    }
                    StreamChunkType::Done { final_metrics: _ } => Some(Ok(make_chunk(
                        chat_id,
                        created,
                        model_name,
                        false,
                        None,
                        None,
                        Some("stop".to_string()),
                    ))),
                    _ => None,
                },
                Err(e) => Some(Err(anyhow::Error::from(e))),
            }
        }
    });

    Box::pin(converted)
}

fn make_chunk(
    id: String,
    created: u64,
    model: String,
    first: bool,
    content: Option<String>,
    reasoning: Option<String>,
    finish_reason: Option<String>,
) -> ChatCompletionChunk {
    ChatCompletionChunk {
        id,
        object: "chat.completion.chunk".to_string(),
        created,
        model,
        choices: vec![ChunkChoice {
            index: 0,
            delta: Delta {
                role: if first {
                    Some("assistant".to_string())
                } else {
                    None
                },
                content,
                reasoning_content: reasoning,
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use tentaflow_protocol::ModelStreamChunk;

    fn text_chunk(text: &str) -> ModelStreamChunk {
        ModelStreamChunk {
            request_id: "r".to_string(),
            chunk: StreamChunkType::TextDelta(text.to_string()),
        }
    }

    fn done_chunk() -> ModelStreamChunk {
        ModelStreamChunk {
            request_id: "r".to_string(),
            chunk: StreamChunkType::Done {
                final_metrics: None,
            },
        }
    }

    #[tokio::test]
    async fn quic_stream_converts_text_deltas_and_done() {
        let input: Vec<std::result::Result<ModelStreamChunk, crate::error::CoreError>> = vec![
            Ok(text_chunk("Hel")),
            Ok(text_chunk("lo")),
            Ok(done_chunk()),
        ];
        let src = stream::iter(input);

        let converted = quic_stream_to_openai_chunks(src, "model-x".to_string());
        let collected: Vec<_> = converted.collect().await;

        assert_eq!(collected.len(), 3);
        let first_chunk = collected[0].as_ref().unwrap();
        assert_eq!(first_chunk.model, "model-x");
        assert_eq!(first_chunk.choices[0].delta.role.as_deref(), Some("assistant"));
        assert_eq!(first_chunk.choices[0].delta.content.as_deref(), Some("Hel"));

        let second = collected[1].as_ref().unwrap();
        assert!(second.choices[0].delta.role.is_none());
        assert_eq!(second.choices[0].delta.content.as_deref(), Some("lo"));

        let done = collected[2].as_ref().unwrap();
        assert_eq!(done.choices[0].finish_reason.as_deref(), Some("stop"));
    }
}
