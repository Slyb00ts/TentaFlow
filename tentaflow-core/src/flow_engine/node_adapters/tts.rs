// =============================================================================
// Plik: flow_engine/node_adapters/tts.rs
// Opis: TtsNodeAdapter — JEDEN node TTS obsługujący oba tryby:
//   * blocking (NodeAdapter): cały tekst → audio za jednym razem.
//   * streaming (StreamingNodeAdapter): konsumuje stream LLM, buforuje tokeny w
//     całe zdania, czyści (ctx.tts_cleaning) i syntetyzuje audio per zdanie —
//     pierwsze audio dociera po pierwszym zdaniu (niska latencja). W tym czasie
//     już zbiera kolejne zdanie. `forward_text=true` przepuszcza też oryginalny
//     tekst (do bąbla) obok audio. Oba tryby robią cleaning, więc osobny node
//     `tts_clean` nie jest wymagany przy używaniu samego TTS.
// Tekst źródłowy trafia do artifacts['source_text'] (blocking) dla downstream.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::debug;

use crate::flow_engine::dispatchers::TtsRequest;
use crate::flow_engine::envelope::{
    ArtifactProvenance, AudioStreamChunk, EnvelopeDelta, EnvelopeDeltaKind, FinishReason,
    FlowEnvelope, FlowValue, NodeInput,
};
use crate::flow_engine::node_adapter::{
    ExecutionContext, NodeAdapter, PortSpec, StreamingNodeAdapter,
};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "tts";

/// Sentence terminators dla per-zdanie batching w trybie streaming.
const SENTENCE_TERMINATORS: &[char] = &['.', '!', '?', '…', ';', '\n'];

/// Maks znaków bufora przed forced flush gdy zdanie nie ma terminatora.
const DEFAULT_MAX_BUFFER_CHARS: usize = 1000;

pub struct TtsNodeAdapter;

impl TtsNodeAdapter {
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
            "tts adapter: no model — node config 'model' nor envelope.meta['tts_model']"
        ))
    }

    /// Priorytet `node.config` > `envelope.meta`.
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

impl Default for TtsNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for TtsNodeAdapter {
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

    /// Blocking: cały tekst → cleaning → synteza → Audio za jednym razem.
    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("tts adapter: missing input edge"))?;
        let envelope = &input.envelope;

        let text = match &envelope.payload {
            FlowValue::Text(t) if !t.is_empty() => t.clone(),
            FlowValue::Text(_) | FlowValue::Empty => {
                return Err(anyhow!("tts adapter: empty input text"));
            }
            other => {
                return Err(anyhow!(
                    "tts adapter: payload must be Text, got {}",
                    other.kind()
                ));
            }
        };

        let cleaned = ctx
            .tts_cleaning
            .clean(&text)
            .await
            .map_err(|e| anyhow!("tts adapter cleaning: {e}"))?;

        let req = TtsRequest {
            model: Self::pick_model(node, envelope)?,
            text: cleaned,
            voice: Self::pick_optional_str(node, envelope, "voice"),
            format: Self::pick_optional_str(node, envelope, "format"),
            language: Self::pick_optional_str(node, envelope, "language"),
            speed: Self::pick_optional_f32(node, envelope, "speed"),
            user_id: ctx.user_id.clone(),
            user_role: ctx.user_role.clone(),
            cancel_token: ctx.cancel_token.clone(),
            // §2.5 — the run's stamp, from `ctx`, never from `envelope.meta`.
            provenance: ctx.provenance(),
        };

        let response = ctx
            .tts
            .synthesize(req)
            .await
            .map_err(|e| anyhow!("tts adapter: dispatcher failed: {e}"))?;

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
        .map_err(|e| anyhow!("tts adapter: {e}"))?;
        Ok(out)
    }
}

/// Streaming: LLM stream → audio per zdanie. Buforuje text_delta do granicy
/// zdania, czyści i syntetyzuje, emituje `EnvelopeDelta::Audio`. `forward_text`
/// dodatkowo przepuszcza oryginalne delty tekstu (do bąbla). Cancel sprawdzany
/// przed każdym blocking await.
#[async_trait]
impl StreamingNodeAdapter for TtsNodeAdapter {
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
        let forward_text = node
            .config
            .get("forward_text")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let model = Self::pick_model(node, &seed_envelope)?;
        let voice = Self::pick_optional_str(node, &seed_envelope, "voice");
        let format = Self::pick_optional_str(node, &seed_envelope, "format");
        let language = Self::pick_optional_str(node, &seed_envelope, "language");
        let speed = Self::pick_optional_f32(node, &seed_envelope, "speed");
        let user_id = ctx.user_id.clone();
        let user_role = ctx.user_role.clone();
        // §2.5 — captured once for the whole stream: the node's own stamp.
        let provenance = ctx.provenance();
        let cancel = ctx.cancel_token.clone();
        let tts = ctx.tts.clone();
        let cleaning = ctx.tts_cleaning.clone();
        let blobs = ctx.blobs.clone();

        // forward_text: tee upstream na text passthrough + audio source.
        let (upstream, text_stream): (
            BoxStream<'static, Result<EnvelopeDelta>>,
            Option<BoxStream<'static, Result<EnvelopeDelta>>>,
        ) = if forward_text {
            let (text_tx, text_rx) = mpsc::channel::<Result<EnvelopeDelta>>(64);
            let (audio_tx, audio_rx) = mpsc::channel::<Result<EnvelopeDelta>>(64);
            let cancel_tee = cancel.clone();
            tokio::spawn(async move {
                let mut up = upstream;
                while let Some(item) = up.next().await {
                    if cancel_tee.is_cancelled() {
                        break;
                    }
                    match item {
                        Ok(EnvelopeDelta::Llm(chunk)) => {
                            if text_tx
                                .send(Ok(EnvelopeDelta::Llm(chunk.clone())))
                                .await
                                .is_err()
                            {
                                break;
                            }
                            if audio_tx.send(Ok(EnvelopeDelta::Llm(chunk))).await.is_err() {
                                break;
                            }
                        }
                        Ok(other) => {
                            if audio_tx.send(Ok(other)).await.is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            let _ = audio_tx.send(Err(anyhow!("{e}"))).await;
                            break;
                        }
                    }
                }
            });
            let audio_src = futures::stream::unfold(audio_rx, |mut rx| async move {
                rx.recv().await.map(|item| (item, rx))
            })
            .boxed();
            let text_src = futures::stream::unfold(text_rx, |mut rx| async move {
                rx.recv().await.map(|item| (item, rx))
            })
            .boxed();
            (audio_src, Some(text_src))
        } else {
            (upstream, None)
        };

        let stream = futures::stream::unfold(
            (
                upstream,
                HashMap::<u32, String>::new(),
                max_buffer_chars,
                false, // eof
                false, // emitted_final
            ),
            move |(mut upstream, mut buffers, max_chars, mut eof, mut emitted_final)| {
                let model = model.clone();
                let voice = voice.clone();
                let format = format.clone();
                let language = language.clone();
                let user_id = user_id.clone();
                let user_role = user_role.clone();
                let provenance = provenance.clone();
                let cancel = cancel.clone();
                let tts = tts.clone();
                let cleaning = cleaning.clone();
                let blobs = blobs.clone();
                async move {
                    loop {
                        if cancel.is_cancelled() {
                            return None;
                        }
                        if eof {
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
                                        user_id.clone(),
                                        user_role.clone(),
                                        provenance.clone(),
                                        cancel.clone(),
                                        &tts,
                                        &cleaning,
                                        &blobs,
                                        false,
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
                                        user_id.clone(),
                                        user_role.clone(),
                                        provenance.clone(),
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

        match text_stream {
            Some(text) => Ok(futures::stream::select(text, stream.boxed()).boxed()),
            None => Ok(stream.boxed()),
        }
    }
}

/// Helper — clean + synthesize + zwróć AudioStreamChunk z bytes.
#[allow(clippy::too_many_arguments)]
async fn synthesize_chunk(
    text: &str,
    model: &str,
    voice: Option<String>,
    format: Option<String>,
    language: Option<String>,
    speed: Option<f32>,
    user_id: Option<String>,
    user_role: Option<String>,
    // §2.5 — the streaming node's own stamp, cloned per chunk.
    provenance: crate::flow_engine::dispatcher::CallProvenance,
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
        .map_err(|e| anyhow!("tts streaming cleaning: {e}"))?;
    if cleaned.trim().is_empty() {
        debug!("tts streaming: skip empty cleaned chunk");
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
        provenance,
    };
    let response = tts
        .synthesize(req)
        .await
        .map_err(|e| anyhow!("tts streaming synthesize: {e}"))?;
    let bytes = blobs
        .get(&response.audio)
        .await
        .map_err(|e| anyhow!("tts streaming blob fetch: {e}"))?;
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
    use crate::flow_engine::blob_store::{BlobRef, BlobStore};
    use crate::flow_engine::dispatchers::{TtsDispatcher, TtsResponse};
    use crate::flow_engine::envelope::LlmStreamChunk;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "t1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    fn input(envelope: FlowEnvelope) -> NodeInput {
        NodeInput {
            from_node_id: "trigger".into(),
            from_port: "full".into(),
            envelope: Arc::new(envelope),
        }
    }

    struct FakeTts {
        last: Mutex<Option<TtsRequest>>,
        synthesized: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl TtsDispatcher for FakeTts {
        async fn synthesize(&self, req: TtsRequest) -> Result<TtsResponse> {
            self.synthesized.lock().unwrap().push(req.text.clone());
            *self.last.lock().unwrap() = Some(req);
            Ok(TtsResponse {
                audio: BlobRef {
                    id: "out-blob".into(),
                    size_bytes: 100,
                    mime: "audio/wav".into(),
                    sha256: "y".into(),
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
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    struct StaticBytesBlob(Vec<u8>);
    #[async_trait]
    impl BlobStore for StaticBytesBlob {
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

    fn fake() -> Arc<FakeTts> {
        Arc::new(FakeTts {
            last: Mutex::new(None),
            synthesized: Mutex::new(Vec::new()),
        })
    }

    #[tokio::test]
    async fn blocking_synthesizes_text_into_audio_with_cleaning() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("hello".into());
        let mut ctx = stub_ctx();
        let f = fake();
        ctx.tts = f.clone();

        let out = TtsNodeAdapter::new()
            .execute(
                &node(json!({"model": "m", "voice": "alloy"})),
                &[input(env)],
                &ctx,
            )
            .await
            .unwrap();

        match out.payload {
            FlowValue::Audio { blob_ref, .. } => assert_eq!(blob_ref.id, "out-blob"),
            other => panic!("expected Audio, got {other:?}"),
        }
        assert_eq!(
            f.last.lock().unwrap().as_ref().unwrap().voice.as_deref(),
            Some("alloy")
        );
        match out.artifacts.get("source_text") {
            Some(FlowValue::Text(s)) => assert_eq!(s, "hello"),
            other => panic!("expected source_text Text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn streaming_synthesizes_per_sentence() {
        let mut ctx = stub_ctx();
        let f = fake();
        ctx.tts = f.clone();
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

        let mut out = TtsNodeAdapter::new()
            .process_stream(
                &node(json!({"model": "m"})),
                upstream,
                Arc::new(FlowEnvelope::empty()),
                &ctx,
            )
            .await
            .unwrap();

        let mut audio = 0;
        while let Some(item) = out.next().await {
            if let EnvelopeDelta::Audio(_) = item.unwrap() {
                audio += 1;
            }
        }
        // 2 zdania + final empty stop.
        assert_eq!(audio, 3);
        assert_eq!(f.synthesized.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn streaming_forward_text_emits_text_and_audio() {
        let mut ctx = stub_ctx();
        let f = fake();
        ctx.tts = f.clone();
        ctx.blobs = Arc::new(StaticBytesBlob(vec![0xAA]));

        let upstream = futures::stream::iter(vec![Ok(EnvelopeDelta::Llm(LlmStreamChunk {
            choice_index: 0,
            text_delta: "Tekst zdania.".into(),
            ..Default::default()
        }))])
        .boxed();

        let mut out = TtsNodeAdapter::new()
            .process_stream(
                &node(json!({"model": "m", "forward_text": true})),
                upstream,
                Arc::new(FlowEnvelope::empty()),
                &ctx,
            )
            .await
            .unwrap();

        let mut text = 0;
        let mut audio = 0;
        while let Some(item) = out.next().await {
            match item.unwrap() {
                EnvelopeDelta::Llm(c) if !c.text_delta.is_empty() => text += 1,
                EnvelopeDelta::Audio(_) => audio += 1,
                _ => {}
            }
        }
        assert!(text >= 1, "spodziewano się przepuszczonego tekstu");
        assert!(audio >= 1, "spodziewano się audio");
    }
}
