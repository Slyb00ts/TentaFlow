// =============================================================================
// Plik: flow_engine/node_adapters/sentence_buffer.rs
// Opis: SentenceBufferNodeAdapter — re-chunkuje streaming LLM tekst z granularności
//       per-token (raw z backendu) na granularność per-zdanie. Konsumuje
//       EnvelopeDelta::Llm, buforuje text_delta per choice, emituje pełne zdanie
//       jako jeden EnvelopeDelta::Llm gdy wykryje terminator (.!?…;\n) albo
//       przekroczy limit bufora. Pozwala wstawić blok między LLM a tts_clean/TTS
//       w Flow Builderze, żeby downstream dostawał całe zdania (lepsze cleaning +
//       TTS niż cięcie w połowie słowa).
//
//       Ten node jest na ścieżce tekst-do-mowy — reasoning_delta i tool_calls
//       NIE są forwardowane (spójne z tts_stream_bridge, który też bierze tylko
//       text_delta). Gałąź tekst-do-bąbla idzie LLM→output bezpośrednio, z
//       pominięciem tego bloku.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use std::collections::HashMap;

use crate::flow_engine::envelope::{
    EnvelopeDelta, EnvelopeDeltaKind, FinishReason, FlowEnvelope, LlmStreamChunk, NodeInput,
};
use crate::flow_engine::node_adapter::{
    ExecutionContext, NodeAdapter, PortSpec, StreamingNodeAdapter,
};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "sentence_buffer";

/// Sentence terminators — spójne z tts_stream_bridge / pii_filter.
const SENTENCE_TERMINATORS: &[char] = &['.', '!', '?', '…', ';', '\n'];

/// Maks znaków bufora przed forced flush, gdy zdanie nie ma terminatora
/// (np. długa lista bez kropek). Konfiguralnie przez `node.config['max_buffer_chars']`.
const DEFAULT_MAX_BUFFER_CHARS: usize = 1000;

pub struct SentenceBufferNodeAdapter;

impl SentenceBufferNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SentenceBufferNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for SentenceBufferNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }

    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Text)]
    }

    fn output_ports(&self) -> Vec<PortSpec> {
        vec![
            PortSpec::new("full", FlowDataType::Text),
            PortSpec::new("stream", FlowDataType::Text),
        ]
    }

    /// Blocking fallback — gdy node użyty poza stream chain'em. Pełny tekst jest
    /// już kompletny, więc buforowanie zdań to no-op; passthrough payloadu.
    async fn execute(
        &self,
        _node: &FlowNode,
        inputs: &[NodeInput],
        _ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("sentence_buffer: missing input edge"))?;
        Ok((*input.envelope).clone())
    }
}

#[async_trait]
impl StreamingNodeAdapter for SentenceBufferNodeAdapter {
    fn stream_input_kind(&self) -> EnvelopeDeltaKind {
        EnvelopeDeltaKind::Llm
    }
    fn stream_output_kind(&self) -> EnvelopeDeltaKind {
        EnvelopeDeltaKind::Llm
    }

    async fn process_stream(
        &self,
        node: &FlowNode,
        upstream: BoxStream<'static, Result<EnvelopeDelta>>,
        _seed_envelope: std::sync::Arc<FlowEnvelope>,
        ctx: &ExecutionContext,
    ) -> Result<BoxStream<'static, Result<EnvelopeDelta>>> {
        let max_buffer_chars = node
            .config
            .get("max_buffer_chars")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_MAX_BUFFER_CHARS);
        let cancel = ctx.cancel_token.clone();

        let stream = futures::stream::unfold(
            (
                upstream,
                HashMap::<u32, String>::new(),
                max_buffer_chars,
                false, // eof
                false, // emitted_final
            ),
            move |(mut upstream, mut buffers, max_chars, mut eof, mut emitted_final)| {
                let cancel = cancel.clone();
                async move {
                    loop {
                        if cancel.is_cancelled() {
                            return None;
                        }
                        if eof {
                            // Drain pozostałych buforów (po jednym choice na
                            // iterację), bez finish_reason — final marker idzie
                            // osobnym pustym chunkiem na końcu.
                            if let Some(idx) = buffers.keys().next().copied() {
                                let text = buffers.remove(&idx).unwrap();
                                if !text.is_empty() {
                                    return Some((
                                        Ok(EnvelopeDelta::Llm(LlmStreamChunk {
                                            choice_index: idx,
                                            text_delta: text,
                                            ..Default::default()
                                        })),
                                        (upstream, buffers, max_chars, eof, emitted_final),
                                    ));
                                }
                                continue;
                            }
                            if !emitted_final {
                                emitted_final = true;
                                return Some((
                                    Ok(EnvelopeDelta::Llm(LlmStreamChunk {
                                        choice_index: 0,
                                        text_delta: String::new(),
                                        finish_reason: Some(FinishReason::Stop),
                                        ..Default::default()
                                    })),
                                    (upstream, buffers, max_chars, eof, emitted_final),
                                ));
                            }
                            return None;
                        }
                        match upstream.next().await {
                            Some(Ok(EnvelopeDelta::Llm(chunk))) => {
                                let idx = chunk.choice_index;
                                let buffer = buffers.entry(idx).or_default();
                                buffer.push_str(&chunk.text_delta);
                                let has_terminator = chunk
                                    .text_delta
                                    .chars()
                                    .any(|c| SENTENCE_TERMINATORS.contains(&c));
                                let over_cap = buffer.len() >= max_chars;
                                let has_finish = chunk.finish_reason.is_some();
                                if has_terminator || over_cap || has_finish {
                                    let drained = std::mem::take(buffer);
                                    if drained.is_empty() && !has_finish {
                                        continue;
                                    }
                                    if has_finish {
                                        emitted_final = true;
                                    }
                                    return Some((
                                        Ok(EnvelopeDelta::Llm(LlmStreamChunk {
                                            choice_index: idx,
                                            text_delta: drained,
                                            finish_reason: chunk.finish_reason,
                                            ..Default::default()
                                        })),
                                        (upstream, buffers, max_chars, eof, emitted_final),
                                    ));
                                }
                                continue;
                            }
                            Some(Ok(other)) => {
                                // Nie-Llm delta — passthrough defensywny.
                                return Some((
                                    Ok(other),
                                    (upstream, buffers, max_chars, eof, emitted_final),
                                ));
                            }
                            Some(Err(e)) => {
                                return Some((
                                    Err(e),
                                    (upstream, buffers, max_chars, eof, emitted_final),
                                ));
                            }
                            None => {
                                eof = true;
                                continue;
                            }
                        }
                    }
                }
            },
        );

        Ok(stream.boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use serde_json::json;
    use std::sync::Arc;

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "sb-1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
        }
    }

    fn llm(text: &str) -> Result<EnvelopeDelta> {
        Ok(EnvelopeDelta::Llm(LlmStreamChunk {
            choice_index: 0,
            text_delta: text.into(),
            ..Default::default()
        }))
    }

    async fn collect_text(
        adapter: &SentenceBufferNodeAdapter,
        node: &FlowNode,
        chunks: Vec<Result<EnvelopeDelta>>,
    ) -> Vec<(String, Option<FinishReason>)> {
        let upstream = futures::stream::iter(chunks).boxed();
        let seed = Arc::new(FlowEnvelope::empty());
        let mut out = adapter
            .process_stream(node, upstream, seed, &stub_ctx())
            .await
            .unwrap();
        let mut got = Vec::new();
        while let Some(item) = out.next().await {
            if let EnvelopeDelta::Llm(c) = item.unwrap() {
                got.push((c.text_delta, c.finish_reason));
            }
        }
        got
    }

    #[tokio::test]
    async fn buffers_tokens_into_sentences() {
        // 5 tokenów → 2 zdania ("Hello world." + " How are you?") + final stop.
        let got = collect_text(
            &SentenceBufferNodeAdapter,
            &node(json!({})),
            vec![llm("Hello"), llm(" world."), llm(" How"), llm(" are you?")],
        )
        .await;
        let sentences: Vec<&str> = got.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(sentences, vec!["Hello world.", " How are you?", ""]);
        // Ostatni chunk to terminalny stop.
        assert_eq!(got.last().unwrap().1, Some(FinishReason::Stop));
    }

    #[tokio::test]
    async fn flushes_remainder_without_terminator_on_eof() {
        // Brak terminatora — reszta wypływa na EOF jako jedno zdanie.
        let got = collect_text(
            &SentenceBufferNodeAdapter,
            &node(json!({})),
            vec![llm("bez kropki "), llm("na koncu")],
        )
        .await;
        assert_eq!(got[0].0, "bez kropki na koncu");
        assert_eq!(got.last().unwrap().1, Some(FinishReason::Stop));
    }

    #[tokio::test]
    async fn forced_flush_over_max_buffer_chars() {
        // max_buffer_chars=8: "abcdefgh" (8) bez terminatora → forced flush.
        let got = collect_text(
            &SentenceBufferNodeAdapter,
            &node(json!({ "max_buffer_chars": 8 })),
            vec![llm("abcd"), llm("efgh"), llm("ij")],
        )
        .await;
        assert_eq!(got[0].0, "abcdefgh");
        // Reszta "ij" wypływa na EOF.
        assert_eq!(got[1].0, "ij");
    }

    #[tokio::test]
    async fn finish_reason_mid_stream_no_double_stop() {
        // Pojedynczy chunk z finish_reason → 1 zdanie z Stop, bez drugiego
        // pustego final chunka.
        let got = collect_text(
            &SentenceBufferNodeAdapter,
            &node(json!({})),
            vec![Ok(EnvelopeDelta::Llm(LlmStreamChunk {
                choice_index: 0,
                text_delta: "Tylko jedno zdanie.".into(),
                finish_reason: Some(FinishReason::Stop),
                ..Default::default()
            }))],
        )
        .await;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "Tylko jedno zdanie.");
        assert_eq!(got[0].1, Some(FinishReason::Stop));
    }

    #[tokio::test]
    async fn cancel_aborts_immediately() {
        let cancel = tokio_util::sync::CancellationToken::new();
        let mut ctx = stub_ctx();
        ctx.cancel_token = cancel.clone();
        cancel.cancel();
        let upstream = futures::stream::iter(vec![llm("Hello world.")]).boxed();
        let seed = Arc::new(FlowEnvelope::empty());
        let mut out = SentenceBufferNodeAdapter
            .process_stream(&node(json!({})), upstream, seed, &ctx)
            .await
            .unwrap();
        assert!(out.next().await.is_none());
    }

    #[test]
    fn advertises_text_ports() {
        let a = SentenceBufferNodeAdapter;
        let in_names: Vec<String> = a.input_ports().iter().map(|p| p.name.clone()).collect();
        let out_names: Vec<String> = a.output_ports().iter().map(|p| p.name.clone()).collect();
        assert_eq!(in_names, vec!["in"]);
        assert_eq!(out_names, vec!["full", "stream"]);
    }
}
