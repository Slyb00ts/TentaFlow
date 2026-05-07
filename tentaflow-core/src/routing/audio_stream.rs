// =============================================================================
// Plik: routing/audio_stream.rs
// Opis: Bridge `StreamingExecution` (rkyv `EnvelopeDelta::Audio`) na strumień
//       SSE z base64-zakodowanymi audio chunkami. Używany przez
//       `/v1/audio/speech/flow-stream` (Krok 5) — flow_engine streaming
//       kanał audio dla flowów z `tts_stream_bridge` lub blocking TTS-as-flow.
// =============================================================================

use base64::Engine;
use futures::Stream;
use hyper::body::{Bytes, Frame};
use std::pin::Pin;

use crate::flow_engine::envelope::EnvelopeDelta;
use crate::flow_engine::executor::StreamingExecution;

/// Przepuszcza `StreamingExecution.stream` przez SSE encoder. Każdy
/// `EnvelopeDelta::Audio` lattice jako jedna ramka:
///
/// ```text
/// data: { "audio_chunk": "<base64>", "mime": "...", "sample_rate": ..., "finish_reason": ... }
/// ```
///
/// `EnvelopeDelta::Llm` na audio sink to misconfig (np. blocking flow który
/// zwrócił Text payload zamiast Audio) — emitujemy frame z `error` żeby klient
/// mógł zaraportować, ale stream zamykamy normalnie. Outcome receiver jest
/// spawnowany w background — handler audio sink nie czeka na finalizera, tail
/// usage chunk nie ma sensu dla audio (klient i tak dostał `finish_reason` w
/// ostatniej ramce audio).
pub fn envelope_stream_to_audio_chunks(
    stream_exec: StreamingExecution,
) -> Pin<Box<dyn Stream<Item = std::result::Result<Frame<Bytes>, std::io::Error>> + Send>> {
    use futures::StreamExt;

    let StreamingExecution { stream, outcome } = stream_exec;

    tokio::spawn(async move {
        match outcome.await {
            Ok(o) => tracing::info!(
                latency_ms = o.total_latency_ms,
                error = ?o.error,
                "flow audio streaming completed"
            ),
            Err(_) => tracing::warn!("flow audio finalizer dropped without outcome"),
        }
    });

    let body_chunks = stream.flat_map(|res| {
        let frames: Vec<std::result::Result<Frame<Bytes>, std::io::Error>> = match res {
            Ok(EnvelopeDelta::Audio(chunk)) => {
                let b64 = base64::engine::general_purpose::STANDARD.encode(&chunk.bytes_delta);
                let json = serde_json::json!({
                    "audio_chunk": b64,
                    "mime": chunk.mime,
                    "sample_rate": chunk.sample_rate,
                    "finish_reason": chunk
                        .finish_reason
                        .and_then(|f| f.as_openai_str().map(|s| s.to_string())),
                });
                vec![Ok(Frame::data(Bytes::from(format!("data: {}\n\n", json))))]
            }
            // Audio sink dostał LLM delta — flow misconfig. Klient nie dostanie
            // bytes; emit error frame, niech operator widzi w logach po stronie
            // serwera (`tracing::warn!`) i w SSE error event po stronie klienta.
            Ok(EnvelopeDelta::Llm(_)) => {
                tracing::warn!(
                    "audio sink received Llm delta — flow musi mieć tts_stream_bridge \
                     albo blocking output FlowValue::Audio"
                );
                let json = serde_json::json!({
                    "error": "audio sink received Llm delta — flow misconfig"
                });
                vec![Ok(Frame::data(Bytes::from(format!("data: {}\n\n", json))))]
            }
            Err(e) => {
                let json = serde_json::json!({ "error": format!("{e}") });
                vec![Ok(Frame::data(Bytes::from(format!("data: {}\n\n", json))))]
            }
        };
        futures::stream::iter(frames)
    });

    let done = futures::stream::once(async {
        Ok::<_, std::io::Error>(Frame::data(Bytes::from("data: [DONE]\n\n")))
    });

    Box::pin(body_chunks.chain(done))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::envelope::{
        AudioStreamChunk, EnvelopeDelta, FinishReason, FlowEnvelope, FlowExecutionOutcome,
        TokenUsage,
    };
    use crate::flow_engine::executor::StreamingExecution;
    use futures::StreamExt;

    fn make_exec(deltas: Vec<EnvelopeDelta>) -> StreamingExecution {
        let stream = futures::stream::iter(
            deltas.into_iter().map(Ok::<_, anyhow::Error>),
        )
        .boxed();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = tx.send(FlowExecutionOutcome {
            final_envelope: FlowEnvelope::empty(),
            trace: Vec::new(),
            usage: TokenUsage::default(),
            finish_reason: FinishReason::Stop,
            total_latency_ms: 0,
            error: None,
        });
        StreamingExecution {
            stream,
            outcome: rx,
        }
    }

    async fn collect_frames(
        mut stream: Pin<Box<dyn Stream<Item = std::result::Result<Frame<Bytes>, std::io::Error>> + Send>>,
    ) -> Vec<String> {
        let mut out = Vec::new();
        while let Some(frame) = stream.next().await {
            let frame = frame.expect("frame");
            if let Ok(data) = frame.into_data() {
                out.push(String::from_utf8(data.to_vec()).expect("utf8"));
            }
        }
        out
    }

    /// Audio delta → SSE base64 frame z mime + sample_rate + finish_reason.
    /// Stream zamykany [DONE].
    #[tokio::test]
    async fn audio_delta_emits_base64_sse_then_done() {
        let exec = make_exec(vec![EnvelopeDelta::Audio(AudioStreamChunk {
            choice_index: 0,
            bytes_delta: vec![0x01, 0x02, 0x03],
            mime: "audio/wav".into(),
            sample_rate: Some(16_000),
            finish_reason: Some(FinishReason::Stop),
        })]);
        let frames = collect_frames(envelope_stream_to_audio_chunks(exec)).await;
        assert_eq!(frames.len(), 2, "audio + DONE");
        let audio = &frames[0];
        assert!(audio.starts_with("data: "));
        assert!(audio.contains("AQID"), "base64 of [01 02 03] = AQID, got {audio}");
        assert!(audio.contains("audio/wav"));
        assert!(audio.contains("16000"));
        assert!(audio.contains("\"stop\""));
        assert_eq!(frames[1], "data: [DONE]\n\n");
    }

    /// LLM delta na audio sink to misconfig — emit error frame, ale stream
    /// zamykany normalnie żeby klient HTTP dostał spójny shutdown.
    #[tokio::test]
    async fn llm_delta_emits_error_frame() {
        use crate::flow_engine::envelope::LlmStreamChunk;
        let exec = make_exec(vec![EnvelopeDelta::Llm(LlmStreamChunk {
            choice_index: 0,
            text_delta: "hello".into(),
            reasoning_delta: None,
            tool_calls: Vec::new(),
            usage: None,
            finish_reason: None,
            error: None,
        })]);
        let frames = collect_frames(envelope_stream_to_audio_chunks(exec)).await;
        assert_eq!(frames.len(), 2);
        assert!(frames[0].contains("error"));
        assert!(frames[0].contains("misconfig"));
        assert_eq!(frames[1], "data: [DONE]\n\n");
    }
}
