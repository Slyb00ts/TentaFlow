// =============================================================================
// Plik: flow_engine/node_adapters/tts_stream_bridge.rs
// Opis: TtsStreamBridgeNodeAdapter — most LLM stream → audio chunks.
//       Konsumuje upstream EnvelopeDelta::Llm (text deltami), buforuje per
//       choice, syntezuje TTS na sentence boundary, emituje EnvelopeDelta::
//       Audio. Stage 3d Krok 2b — alternatywny sink dla streaming chain'a
//       (LLM → tts_stream_bridge → output(audio)).
//
//       Reasoning_delta NIE idzie do TTS (tylko text_delta). Cancel
//       checked przed każdym blocking await (cleaning, synthesize).
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::debug;

use crate::flow_engine::dispatchers::TtsRequest;
use crate::flow_engine::envelope::{
    ArtifactProvenance, AudioStreamChunk, EnvelopeDelta, EnvelopeDeltaKind, FinishReason,
    FlowEnvelope, FlowValue, NodeInput,
};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec, StreamingNodeAdapter};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "tts_stream_bridge";

/// Sentence terminators dla per-zdanie batching: `.!?…;` + `\n`
/// (spójne z PII filter).
const SENTENCE_TERMINATORS: &[char] = &['.', '!', '?', '…', ';', '\n'];

/// Maks bytes bufora przed forced flush. Default 1000 — kompromis: za małe
/// = pierwsze audio chunki krótkie (rwany glos); za duże = klient czeka.
/// Konfiguralnie przez `node.config['max_buffer_chars']`.
const DEFAULT_MAX_BUFFER_CHARS: usize = 1000;

pub struct TtsStreamBridgeNodeAdapter;

impl TtsStreamBridgeNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    fn pick_model(node: &FlowNode, envelope: &FlowEnvelope) -> Result<String> {
        if let Some(m) = node
            .config
            .get("model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return Ok(m.to_string());
        }
        if let Some(m) = envelope
            .meta
            .get("tts_model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return Ok(m.to_string());
        }
        Err(anyhow!(
            "tts_stream_bridge: no model — node config 'model' nor envelope.meta['tts_model']"
        ))
    }

    fn pick_optional_str(node: &FlowNode, envelope: &FlowEnvelope, key: &str) -> Option<String> {
        if let Some(s) = node
            .config
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return Some(s.to_string());
        }
        envelope
            .meta
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    }

    fn pick_optional_f32(node: &FlowNode, envelope: &FlowEnvelope, key: &str) -> Option<f32> {
        if let Some(n) = node.config.get(key).and_then(|v| v.as_f64()) {
            return Some(n as f32);
        }
        envelope
            .meta
            .get(key)
            .and_then(|v| v.as_f64())
            .map(|n| n as f32)
    }
}

impl Default for TtsStreamBridgeNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for TtsStreamBridgeNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Text)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        vec![
            PortSpec::new("full", FlowDataType::Audio),
            PortSpec::new("stream", FlowDataType::Audio),
        ]
    }

    fn produced_artifacts(&self) -> &[(&'static str, FlowDataType)] {
        &[("source_text", FlowDataType::Text)]
    }

    /// Blocking fallback — gdy node użyty poza stream chain'em (np. ktoś
    /// podpiął tts_stream_bridge między blocking nody). Konsumuje pełny
    /// payload Text, syntezuje całość przez ctx.tts.synthesize, emit Audio.
    /// Identyczne do `TtsNodeAdapter::execute` — ale ten node deklaruje
    /// streaming-aware contract więc walidator wymaga `StreamingNodeAdapter`
    /// impl.
    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("tts_stream_bridge: missing input edge"))?;
        let envelope = &input.envelope;

        let text = match &envelope.payload {
            FlowValue::Text(t) if !t.is_empty() => t.clone(),
            FlowValue::Text(_) | FlowValue::Empty => {
                return Err(anyhow!("tts_stream_bridge: empty input text"));
            }
            other => {
                return Err(anyhow!(
                    "tts_stream_bridge: payload must be Text, got {}",
                    other.kind()
                ));
            }
        };

        // Cleaning przez ctx.tts_cleaning (single source of truth — codex
        // round 6 P1#8: nie duplikujemy clean_text_for_tts w nowym node).
        let cleaned = ctx
            .tts_cleaning
            .clean(&text)
            .await
            .map_err(|e| anyhow!("tts_stream_bridge cleaning: {e}"))?;

        let req = TtsRequest {
            model: Self::pick_model(node, envelope)?,
            text: cleaned.clone(),
            voice: Self::pick_optional_str(node, envelope, "voice"),
            format: Self::pick_optional_str(node, envelope, "format"),
            language: Self::pick_optional_str(node, envelope, "language"),
            speed: Self::pick_optional_f32(node, envelope, "speed"),
            user_id: ctx.user_id,
            user_role: ctx.user_role.clone(),
            cancel_token: ctx.cancel_token.clone(),
        };

        let response = ctx
            .tts
            .synthesize(req)
            .await
            .map_err(|e| anyhow!("tts_stream_bridge synthesize: {e}"))?;

        let mut out: FlowEnvelope = (**envelope).clone();
        out.payload = FlowValue::Audio {
            blob_ref: response.audio,
            mime: response.mime,
            sample_rate: response.sample_rate,
        };
        out.put_artifact(
            "source_text",
            FlowValue::Text(text),
            ArtifactProvenance {
                producer_node_id: node.id.clone(),
                producer_node_type: NODE_TYPE.to_string(),
                timestamp_ms: ctx.clock.now_ms(),
            },
        )
        .map_err(|e| anyhow!("tts_stream_bridge: {e}"))?;
        Ok(out)
    }
}

/// Stage 3d Krok 2b: streaming bridge LLM → audio. Konsumuje
/// `EnvelopeDelta::Llm` text deltami, batch'uje per zdanie, syntezuje TTS
/// blocking per zdanie, emituje `EnvelopeDelta::Audio` z BlobRef + mime
/// + sample_rate. Pierwsza ramka audio dociera do klienta po pierwszym
/// kompletnym zdaniu (low first-audible latency).
///
/// Cancel-on-drop: explicit `ctx.cancel_token.is_cancelled()` check przed
/// każdym blocking await (cleaning + synthesize). Drop SSE → cancel
/// propagacja → bridge przerywa po obecnym zdaniu (worst case: jedno
/// zdanie całe się dosynethyzuje; brak audio chunk'a in-flight).
#[async_trait]
impl StreamingNodeAdapter for TtsStreamBridgeNodeAdapter {
    fn stream_input_kind(&self) -> EnvelopeDeltaKind {
        EnvelopeDeltaKind::Llm
    }
    fn stream_output_kind(&self) -> EnvelopeDeltaKind {
        EnvelopeDeltaKind::Audio
    }

    async fn process_stream(
        &self,
        node: &FlowNode,
        upstream: BoxStream<'static, Result<EnvelopeDelta>>,
        seed_envelope: Arc<FlowEnvelope>,
        ctx: &ExecutionContext,
    ) -> Result<BoxStream<'static, Result<EnvelopeDelta>>> {
        let max_buffer_chars = node
            .config
            .get("max_buffer_chars")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_MAX_BUFFER_CHARS);

        // Snapshot TTS parametrów raz na start streamu (admin nie zmienia
        // mid-stream voice/model/format).
        let model = Self::pick_model(node, &seed_envelope)?;
        let voice = Self::pick_optional_str(node, &seed_envelope, "voice");
        let format = Self::pick_optional_str(node, &seed_envelope, "format");
        let language = Self::pick_optional_str(node, &seed_envelope, "language");
        let speed = Self::pick_optional_f32(node, &seed_envelope, "speed");
        let user_id = ctx.user_id;
        let user_role = ctx.user_role.clone();
        let cancel = ctx.cancel_token.clone();
        let tts = ctx.tts.clone();
        let cleaning = ctx.tts_cleaning.clone();
        let blobs = ctx.blobs.clone();

        let stream = futures::stream::unfold(
            (
                upstream,
                HashMap::<u32, String>::new(),
                max_buffer_chars,
                false, // eof
                false, // emitted_final_chunk
            ),
            move |(mut upstream, mut buffers, max_chars, mut eof, mut emitted_final)| {
                let model = model.clone();
                let voice = voice.clone();
                let format = format.clone();
                let language = language.clone();
                let user_role = user_role.clone();
                let cancel = cancel.clone();
                let tts = tts.clone();
                let cleaning = cleaning.clone();
                let blobs = blobs.clone();
                async move {
                    loop {
                        if cancel.is_cancelled() {
                            // Klient disconnect → EOF natychmiast, bez
                            // syntezy pozostałych zdań.
                            return None;
                        }
                        if eof {
                            // EOF — drain remaining buffers (po jednym choice
                            // na iterację) + emit final chunk z finish_reason.
                            if let Some(idx) = buffers.keys().next().copied() {
                                let text = buffers.remove(&idx).unwrap();
                                if !text.is_empty() {
                                    match synthesize_chunk(
                                        &text,
                                        &model,
                                        voice.clone(),
                                        format.clone(),
                                        language.clone(),
                                        speed,
                                        user_id,
                                        user_role.clone(),
                                        cancel.clone(),
                                        &tts,
                                        &cleaning,
                                        &blobs,
                                        false, // not yet final — drainujemy
                                        idx,
                                    )
                                    .await
                                    {
                                        Ok(Some(audio)) => {
                                            return Some((
                                                Ok(EnvelopeDelta::Audio(audio)),
                                                (upstream, buffers, max_chars, eof, emitted_final),
                                            ));
                                        }
                                        Ok(None) => continue,
                                        Err(e) => {
                                            return Some((
                                                Err(e),
                                                (upstream, buffers, max_chars, eof, emitted_final),
                                            ));
                                        }
                                    }
                                }
                                continue;
                            }
                            // Wszystkie bufory drained — emit final empty
                            // chunk z finish_reason=Stop żeby klient widział
                            // koniec stream'u. emitted_final flag chroni
                            // przed duplikatem (gdyby finish_reason mid-stream
                            // już oznaczył audio chunk jako Stop, skipujemy).
                            if !emitted_final {
                                emitted_final = true;
                                let final_chunk = AudioStreamChunk {
                                    choice_index: 0,
                                    bytes_delta: Vec::new(),
                                    mime: format.clone().unwrap_or_else(|| "audio/wav".into()),
                                    sample_rate: None,
                                    finish_reason: Some(FinishReason::Stop),
                                };
                                return Some((
                                    Ok(EnvelopeDelta::Audio(final_chunk)),
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
                                    if drained.is_empty() {
                                        continue;
                                    }
                                    match synthesize_chunk(
                                        &drained,
                                        &model,
                                        voice.clone(),
                                        format.clone(),
                                        language.clone(),
                                        speed,
                                        user_id,
                                        user_role.clone(),
                                        cancel.clone(),
                                        &tts,
                                        &cleaning,
                                        &blobs,
                                        has_finish,
                                        idx,
                                    )
                                    .await
                                    {
                                        Ok(Some(audio)) => {
                                            // P1 fix: gdy LLM emituje finish_reason
                                            // mid-stream, audio chunk już jest
                                            // terminalny (is_final=true). Skip
                                            // EOF empty Stop żeby nie wysyłać
                                            // duplikatu.
                                            if has_finish {
                                                emitted_final = true;
                                            }
                                            return Some((
                                                Ok(EnvelopeDelta::Audio(audio)),
                                                (upstream, buffers, max_chars, eof, emitted_final),
                                            ));
                                        }
                                        Ok(None) => continue,
                                        Err(e) => {
                                            return Some((
                                                Err(e),
                                                (upstream, buffers, max_chars, eof, emitted_final),
                                            ));
                                        }
                                    }
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

/// Helper — clean text + synthesize + zwróć AudioStreamChunk z bytes.
/// `is_final` = true emitted gdy upstream zaraportował finish_reason w
/// tym chunku (forced flush przez stream end).
#[allow(clippy::too_many_arguments)]
async fn synthesize_chunk(
    text: &str,
    model: &str,
    voice: Option<String>,
    format: Option<String>,
    language: Option<String>,
    speed: Option<f32>,
    user_id: Option<i64>,
    user_role: Option<String>,
    cancel: tokio_util::sync::CancellationToken,
    tts: &Arc<dyn crate::flow_engine::dispatchers::TtsDispatcher>,
    cleaning: &Arc<dyn crate::flow_engine::dispatchers::TtsCleaningStore>,
    blobs: &Arc<dyn crate::flow_engine::blob_store::BlobStore>,
    is_final: bool,
    choice_index: u32,
) -> Result<Option<AudioStreamChunk>> {
    if cancel.is_cancelled() {
        return Ok(None);
    }
    let cleaned = cleaning
        .clean(text)
        .await
        .map_err(|e| anyhow!("tts_stream_bridge cleaning: {e}"))?;
    if cleaned.trim().is_empty() {
        debug!("tts_stream_bridge: skip empty cleaned chunk");
        return Ok(None);
    }
    if cancel.is_cancelled() {
        return Ok(None);
    }
    let req = TtsRequest {
        model: model.to_string(),
        text: cleaned,
        voice,
        format: format.clone(),
        language,
        speed,
        user_id,
        user_role,
        cancel_token: cancel.clone(),
    };
    let response = tts
        .synthesize(req)
        .await
        .map_err(|e| anyhow!("tts_stream_bridge synthesize: {e}"))?;
    let bytes = blobs
        .get(&response.audio)
        .await
        .map_err(|e| anyhow!("tts_stream_bridge blob fetch: {e}"))?;
    Ok(Some(AudioStreamChunk {
        choice_index,
        bytes_delta: bytes,
        mime: response.mime,
        sample_rate: response.sample_rate,
        finish_reason: if is_final {
            Some(FinishReason::Stop)
        } else {
            None
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::blob_store::BlobRef;
    use crate::flow_engine::dispatchers::{TtsDispatcher, TtsResponse};
    use crate::flow_engine::envelope::LlmStreamChunk;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use serde_json::json;
    use std::sync::Mutex;

    struct FakeTts {
        synthesized: Mutex<Vec<String>>,
        bytes: Vec<u8>,
    }

    #[async_trait]
    impl TtsDispatcher for FakeTts {
        async fn synthesize(&self, req: TtsRequest) -> Result<TtsResponse> {
            self.synthesized.lock().unwrap().push(req.text.clone());
            Ok(TtsResponse {
                audio: BlobRef {
                    id: format!("blob-{}", self.synthesized.lock().unwrap().len()),
                    size_bytes: self.bytes.len() as u64,
                    mime: "audio/wav".into(),
                    sha256: "x".into(),
                },
                mime: "audio/wav".into(),
                sample_rate: Some(22_050),
            })
        }
        async fn stream_synthesize(
            &self,
            _req: TtsRequest,
        ) -> Result<BoxStream<'static, Result<crate::flow_engine::dispatchers::TtsStreamChunk>>>
        {
            Err(anyhow!("FakeTts: stream_synthesize not used in bridge tests"))
        }
    }

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "ttsb-1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
        }
    }

    #[tokio::test]
    async fn bridge_synthesizes_per_sentence() {
        // 3 zdania w 4 chunkach: "Hello", " world.", " Second sentence.",
        // " Third!" — flush po każdej kropce. 3 audio chunki (jedno per
        // zdanie) + final empty chunk z finish_reason=Stop.
        let mut ctx = stub_ctx();
        let fake = Arc::new(FakeTts {
            synthesized: Mutex::new(Vec::new()),
            bytes: vec![0xAA, 0xBB, 0xCC],
        });
        ctx.tts = fake.clone();
        // Mock blob store — FakeTts zwraca BlobRef z id "blob-N"; BlobStore
        // domyślny stub_ctx zwraca pusty vec — tweak żeby zwracał FakeTts.bytes.
        let blob_bytes = vec![0xAA, 0xBB, 0xCC];
        ctx.blobs = Arc::new(StaticBytesBlob(blob_bytes.clone()));

        let upstream = futures::stream::iter(vec![
            Ok(EnvelopeDelta::Llm(LlmStreamChunk {
                choice_index: 0,
                text_delta: "Hello".into(),
                ..Default::default()
            })),
            Ok(EnvelopeDelta::Llm(LlmStreamChunk {
                choice_index: 0,
                text_delta: " world.".into(), // sentence 1 flush
                ..Default::default()
            })),
            Ok(EnvelopeDelta::Llm(LlmStreamChunk {
                choice_index: 0,
                text_delta: " Second sentence.".into(), // sentence 2 flush
                ..Default::default()
            })),
            Ok(EnvelopeDelta::Llm(LlmStreamChunk {
                choice_index: 0,
                text_delta: " Third!".into(), // sentence 3 flush
                ..Default::default()
            })),
        ])
        .boxed();

        let seed = Arc::new(FlowEnvelope::empty());
        let mut out = TtsStreamBridgeNodeAdapter
            .process_stream(&node(json!({"model": "voxcpm"})), upstream, seed, &ctx)
            .await
            .unwrap();

        let mut audio_chunks = Vec::new();
        while let Some(item) = out.next().await {
            audio_chunks.push(item.unwrap());
        }

        // 3 audio chunki per zdanie + 1 final empty z finish_reason=Stop = 4
        assert_eq!(audio_chunks.len(), 4);
        // Ostatni chunk = final empty stop.
        let EnvelopeDelta::Audio(last) = &audio_chunks[3] else {
            panic!("expected Audio");
        };
        assert!(last.bytes_delta.is_empty());
        assert_eq!(last.finish_reason, Some(FinishReason::Stop));
        // Synthesized = 3 zdania.
        let synthesized = fake.synthesized.lock().unwrap().clone();
        assert_eq!(synthesized.len(), 3);
    }

    /// Codex review Krok 2b P1#1: gdy LLM emituje finish_reason
    /// mid-stream, audio chunk powinien być terminal (is_final=true)
    /// I emitted_final flag musi być ustawione żeby EOF NIE wysłał
    /// drugiego pustego Stop chunka. Klient nigdy nie widzi 2x Stop.
    #[tokio::test]
    async fn bridge_finish_reason_mid_stream_no_double_stop() {
        let mut ctx = stub_ctx();
        let fake = Arc::new(FakeTts {
            synthesized: Mutex::new(Vec::new()),
            bytes: vec![0xAA],
        });
        ctx.tts = fake.clone();
        ctx.blobs = Arc::new(StaticBytesBlob(vec![0xAA]));

        let upstream = futures::stream::iter(vec![Ok(EnvelopeDelta::Llm(LlmStreamChunk {
            choice_index: 0,
            text_delta: "Hello world.".into(),
            finish_reason: Some(FinishReason::Stop),
            ..Default::default()
        }))])
        .boxed();

        let seed = Arc::new(FlowEnvelope::empty());
        let mut out = TtsStreamBridgeNodeAdapter
            .process_stream(&node(json!({"model": "voxcpm"})), upstream, seed, &ctx)
            .await
            .unwrap();

        let mut chunks = Vec::new();
        while let Some(item) = out.next().await {
            chunks.push(item.unwrap());
        }

        // Tylko 1 chunk — audio z bytes + finish_reason=Stop. Brak
        // duplikatu empty final.
        assert_eq!(chunks.len(), 1);
        let EnvelopeDelta::Audio(c) = &chunks[0] else {
            panic!("expected Audio");
        };
        assert!(!c.bytes_delta.is_empty(), "audio chunk should carry bytes");
        assert_eq!(c.finish_reason, Some(FinishReason::Stop));
    }

    /// Codex review Krok 2b P1#2: AudioStreamChunk ma teraz choice_index
    /// (parytet z LlmStreamChunk Krok 1a). Multi-choice n>1: 2 zdania
    /// per choice → audio chunki niosą poprawny choice_index.
    #[tokio::test]
    async fn bridge_propagates_choice_index_multi_choice() {
        let mut ctx = stub_ctx();
        let fake = Arc::new(FakeTts {
            synthesized: Mutex::new(Vec::new()),
            bytes: vec![0xAA],
        });
        ctx.tts = fake.clone();
        ctx.blobs = Arc::new(StaticBytesBlob(vec![0xAA]));

        let upstream = futures::stream::iter(vec![
            Ok(EnvelopeDelta::Llm(LlmStreamChunk {
                choice_index: 0,
                text_delta: "Choice zero.".into(), // sentence flush, idx=0
                ..Default::default()
            })),
            Ok(EnvelopeDelta::Llm(LlmStreamChunk {
                choice_index: 1,
                text_delta: "Choice one.".into(), // sentence flush, idx=1
                ..Default::default()
            })),
        ])
        .boxed();

        let seed = Arc::new(FlowEnvelope::empty());
        let mut out = TtsStreamBridgeNodeAdapter
            .process_stream(&node(json!({"model": "voxcpm"})), upstream, seed, &ctx)
            .await
            .unwrap();

        let mut audios: Vec<AudioStreamChunk> = Vec::new();
        while let Some(item) = out.next().await {
            if let EnvelopeDelta::Audio(a) = item.unwrap() {
                audios.push(a);
            }
        }

        // 2 audio (po 1 per choice) + 1 final empty.
        assert_eq!(audios.len(), 3);
        assert_eq!(audios[0].choice_index, 0);
        assert_eq!(audios[1].choice_index, 1);
        // Final empty chunk → choice_index 0 (synthetic terminal).
        assert!(audios[2].bytes_delta.is_empty());
    }

    #[tokio::test]
    async fn bridge_cancel_token_aborts_before_synthesize() {
        let cancel = tokio_util::sync::CancellationToken::new();
        let mut ctx = stub_ctx();
        ctx.cancel_token = cancel.clone();
        let fake = Arc::new(FakeTts {
            synthesized: Mutex::new(Vec::new()),
            bytes: vec![],
        });
        ctx.tts = fake.clone();

        // Cancel od razu — bridge nie powinien syntetyzować ani jednego chunku.
        cancel.cancel();

        let upstream = futures::stream::iter(vec![Ok(EnvelopeDelta::Llm(LlmStreamChunk {
            choice_index: 0,
            text_delta: "Hello world.".into(),
            ..Default::default()
        }))])
        .boxed();

        let seed = Arc::new(FlowEnvelope::empty());
        let mut out = TtsStreamBridgeNodeAdapter
            .process_stream(&node(json!({"model": "voxcpm"})), upstream, seed, &ctx)
            .await
            .unwrap();

        // Pierwsze .next() po cancel → None natychmiast.
        assert!(out.next().await.is_none());
        // Brak syntezy.
        assert_eq!(fake.synthesized.lock().unwrap().len(), 0);
    }

    /// Krok 8 item 32: bridge MUSI wywołać `ctx.tts_cleaning.clean()`
    /// przed każdym synthesize, żeby tekst zdania był sprzątnięty
    /// (markdown, hashtagi, emoji-jak-tekst itd.) zanim trafi do
    /// silnika TTS. Counting stub zlicza wywołania i porównuje z
    /// liczbą wysyntetyzowanych zdań.
    #[tokio::test]
    async fn bridge_uses_tts_cleaning_store_per_sentence() {
        use crate::flow_engine::dispatchers::TtsCleaningStore;

        struct CountingClean {
            called_with: Mutex<Vec<String>>,
        }
        #[async_trait]
        impl TtsCleaningStore for CountingClean {
            async fn clean(&self, text: &str) -> Result<String> {
                self.called_with.lock().unwrap().push(text.to_string());
                Ok(format!("[clean] {}", text))
            }
        }

        let mut ctx = stub_ctx();
        let fake = Arc::new(FakeTts {
            synthesized: Mutex::new(Vec::new()),
            bytes: vec![0xAA],
        });
        let counting = Arc::new(CountingClean {
            called_with: Mutex::new(Vec::new()),
        });
        ctx.tts = fake.clone();
        ctx.tts_cleaning = counting.clone();
        ctx.blobs = Arc::new(StaticBytesBlob(vec![0xAA]));

        let upstream = futures::stream::iter(vec![
            Ok(EnvelopeDelta::Llm(LlmStreamChunk {
                choice_index: 0,
                text_delta: "First sentence.".into(),
                ..Default::default()
            })),
            Ok(EnvelopeDelta::Llm(LlmStreamChunk {
                choice_index: 0,
                text_delta: " Second!".into(),
                ..Default::default()
            })),
        ])
        .boxed();

        let seed = Arc::new(FlowEnvelope::empty());
        let mut out = TtsStreamBridgeNodeAdapter
            .process_stream(&node(json!({"model": "voxcpm"})), upstream, seed, &ctx)
            .await
            .unwrap();
        while out.next().await.is_some() {}

        let cleans = counting.called_with.lock().unwrap().clone();
        assert_eq!(
            cleans.len(),
            2,
            "tts_cleaning.clean() musi być wywołane raz per zdanie, got {cleans:?}"
        );
        // Synthesize widzi wynik clean'a, nie raw input.
        let synthesized = fake.synthesized.lock().unwrap().clone();
        assert!(
            synthesized.iter().all(|s| s.starts_with("[clean] ")),
            "synthesize widziało tekst nie-cleaned: {synthesized:?}"
        );
    }

    /// Pomocniczy BlobStore dla testów — zawsze zwraca te same bajty.
    struct StaticBytesBlob(Vec<u8>);
    #[async_trait]
    impl crate::flow_engine::blob_store::BlobStore for StaticBytesBlob {
        async fn put(&self, _bytes: Vec<u8>, mime: &str) -> Result<BlobRef> {
            Ok(BlobRef {
                id: "x".into(),
                size_bytes: self.0.len() as u64,
                mime: mime.to_string(),
                sha256: "x".into(),
            })
        }
        async fn get(&self, _r: &BlobRef) -> Result<Vec<u8>> {
            Ok(self.0.clone())
        }
        async fn gc(&self, _retention: std::time::Duration) -> Result<u64> {
            Ok(0)
        }
    }
}
