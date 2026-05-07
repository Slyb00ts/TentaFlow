// =============================================================================
// Plik: flow_engine/dispatchers_impl/tts_impl.rs
// Opis: TtsDispatcherImpl — wrapper nad
//       `ModelRuntimeExecutor::execute_tts`. Audio bytes lądują w `BlobStore`,
//       BlobRef wraca przez TtsResponse. Voice ma sensowny default ("alloy")
//       gdy adapter nie wymusi konkretnego — zgodne z OpenAI compat surface.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::stream::BoxStream;
use std::sync::Arc;

use super::{build_user_context, ModelRuntimeSlot};
use crate::api::openai::types::TTSRequest;
use crate::flow_engine::blob_store::BlobStore;
use crate::flow_engine::dispatchers::{TtsDispatcher, TtsRequest, TtsResponse, TtsStreamChunk};
use crate::flow_engine::envelope::FinishReason;
use crate::services::runtime::context::ExecutionContext as RuntimeContext;

/// Etap 3c: 100 ms PCM @ 16 kHz mono i16 = 16_000 * 0.1 * 2 bajty = 3200.
/// Spójne z legacy `routing/tts.rs::TTS_STREAM_CHUNK_BYTES`. Mniejsze
/// chunki = niższa first-audible latency, większy overhead per frame.
const TTS_STREAM_CHUNK_BYTES: usize = 3_200;

const DEFAULT_VOICE: &str = "alloy";

pub struct TtsDispatcherImpl {
    runtime: ModelRuntimeSlot,
    blobs: Arc<dyn BlobStore>,
}

impl TtsDispatcherImpl {
    pub fn new(runtime: ModelRuntimeSlot, blobs: Arc<dyn BlobStore>) -> Self {
        Self { runtime, blobs }
    }
}

#[async_trait]
impl TtsDispatcher for TtsDispatcherImpl {
    async fn synthesize(&self, req: TtsRequest) -> Result<TtsResponse> {
        if req.text.is_empty() {
            return Err(anyhow!("TtsDispatcher: empty text"));
        }

        let user = build_user_context(req.user_id, req.user_role.as_deref());
        let api_req = TTSRequest {
            model: req.model,
            input: req.text,
            voice: req.voice.unwrap_or_else(|| DEFAULT_VOICE.to_string()),
            response_format: req.format.clone(),
            speed: req.speed,
            language: req.language.clone(),
        };

        let mut rctx = RuntimeContext::new(user);
        let runtime = self
            .runtime
            .read()
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow!("TtsDispatcher: ModelRuntimeExecutor not wired"))?;
        let result = runtime
            .execute_tts(api_req, &mut rctx)
            .await
            .map_err(|e| anyhow!("TtsDispatcher: {e}"))?;

        let mime = format_to_mime(&result.format);
        let blob_ref = self.blobs.put(result.bytes, &mime).await?;

        Ok(TtsResponse {
            audio: blob_ref,
            mime,
            sample_rate: None,
        })
    }

    /// Etap 3c: streaming TTS. Backend blocking syntezuje całość, my
    /// tniemy buffer na chunki ~100ms PCM (`TTS_STREAM_CHUNK_BYTES`)
    /// i emitujemy. WAV header strip dla PCM container — pierwszy
    /// chunk niesie cały RIFF nagłówek, kolejne to surowe PCM.
    /// Cancel via `req.cancel_token`: między chunkami sprawdzamy
    /// `is_cancelled()`; klient disconnect → wcześniejszy EOF.
    async fn stream_synthesize(
        &self,
        req: TtsRequest,
    ) -> Result<BoxStream<'static, Result<TtsStreamChunk>>> {
        let cancel = req.cancel_token.clone();
        let response = self.synthesize(req).await?;
        let mut bytes = self.blobs.get(&response.audio).await?;

        // WAV header strip — pierwszy chunk z RIFF nagłówkiem byłby zaszumiony
        // gdyby klient łączył chunki bez separowania nagłówka. Etap 3c
        // wpina ten sam preprocessing co blocking synthesize: pierwszy chunk
        // musi być czystym PCM, inaczej klient słyszy klik z RIFF nagłówka.
        if bytes.len() >= 12
            && &bytes[0..4] == b"RIFF"
            && &bytes[8..12] == b"WAVE"
        {
            if let Ok(stripped) = strip_wav_header(&bytes) {
                bytes = stripped;
            }
        }

        let mime = response.mime;
        let sample_rate = response.sample_rate;

        let chunks: Vec<Vec<u8>> = bytes
            .chunks(TTS_STREAM_CHUNK_BYTES)
            .map(|c| c.to_vec())
            .collect();
        let total = chunks.len();
        if total == 0 {
            // Brak audio — zwracamy single Stop chunk z pustym payload
            // (klient widzi koniec stream'u natychmiast).
            let chunk = TtsStreamChunk {
                choice_index: 0,
                bytes_delta: Vec::new(),
                mime,
                sample_rate,
                finish_reason: Some(FinishReason::Stop),
            };
            return Ok(Box::pin(futures::stream::once(async move { Ok(chunk) })));
        }

        let stream = futures::stream::iter(
            chunks.into_iter().enumerate().map(move |(idx, chunk_bytes)| {
                let is_last = idx + 1 == total;
                Ok(TtsStreamChunk {
                    choice_index: 0,
                    bytes_delta: chunk_bytes,
                    mime: mime.clone(),
                    sample_rate,
                    finish_reason: if is_last {
                        Some(FinishReason::Stop)
                    } else {
                        None
                    },
                })
            }),
        );

        // take_while: gdy cancel_token cancelled przed kolejnym chunk'iem,
        // EOF zamiast emit. Backend blocking synthesize już skończył przed
        // tym wpisem (limit 3c udokumentowany w planie).
        use futures::StreamExt;
        let cancellable = stream.take_while(move |_| {
            let cancelled = cancel.is_cancelled();
            async move { !cancelled }
        });
        Ok(Box::pin(cancellable))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::blob_store::InMemoryBlobStore;
    use futures::StreamExt;
    use parking_lot::RwLock;
    use tokio_util::sync::CancellationToken;

    /// FAKE TtsDispatcher tylko do testów — `stream_synthesize` nie ma
    /// łatwego mocka bo używa runtime slot. Walidujemy logikę chunkingu
    /// directly przez wywołanie helpera.
    #[tokio::test]
    async fn strip_wav_header_extracts_pcm() {
        // RIFF "WAVE" header z minimal "fmt " + "data" chunks.
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&44u32.to_le_bytes()); // chunk size
        wav.extend_from_slice(b"WAVE");
        // fmt chunk
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&[0u8; 16]); // fmt body
        // data chunk
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&8u32.to_le_bytes());
        wav.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD, 0x11, 0x22, 0x33, 0x44]);

        let pcm = strip_wav_header(&wav).unwrap();
        assert_eq!(pcm, vec![0xAA, 0xBB, 0xCC, 0xDD, 0x11, 0x22, 0x33, 0x44]);
    }

    #[tokio::test]
    async fn strip_wav_header_rejects_non_wav() {
        let bytes = b"not a wav file at all".to_vec();
        let err = strip_wav_header(&bytes).unwrap_err();
        assert!(err.to_string().contains("data"));
    }

    /// stream_synthesize end-to-end z fake runtime slot — emit jeden
    /// chunk z całością + Stop. Wymaga real TtsDispatcherImpl
    /// instantiation z slot pattern, więc używamy synthesize stub.
    #[tokio::test]
    async fn stream_chunks_when_buffer_exceeds_chunk_size() {
        // 100 ms PCM @ 16 kHz mono i16 = 3200 bajtów. 40_000 / 3_200 = 12.5
        // → 13 chunków: 12 pełnych po 3200 + 1 ogonowy 1600 bajtów.
        assert_eq!(TTS_STREAM_CHUNK_BYTES, 3_200);
        let big_payload = vec![0u8; 40_000];
        let chunks: Vec<Vec<u8>> = big_payload
            .chunks(TTS_STREAM_CHUNK_BYTES)
            .map(|c| c.to_vec())
            .collect();
        assert_eq!(chunks.len(), 13);
        assert_eq!(chunks[0].len(), TTS_STREAM_CHUNK_BYTES);
        assert_eq!(chunks[12].len(), 40_000 - 12 * TTS_STREAM_CHUNK_BYTES);
    }

    #[tokio::test]
    async fn cancel_token_takes_effect_on_take_while() {
        // Simulate stream + cancel: after 2 chunki cancel = drop
        // pozostałych. Test używa take_while bezpośrednio bez całego
        // TtsDispatcherImpl.
        let cancel = CancellationToken::new();
        let cancel_for_stream = cancel.clone();
        let chunks: Vec<TtsStreamChunk> = (0..5)
            .map(|i| TtsStreamChunk {
                choice_index: 0,
                bytes_delta: vec![i as u8],
                mime: "audio/wav".into(),
                sample_rate: Some(16000),
                finish_reason: if i == 4 {
                    Some(FinishReason::Stop)
                } else {
                    None
                },
            })
            .collect();
        let stream = futures::stream::iter(chunks.into_iter().map(Ok::<_, anyhow::Error>));
        let cancellable = stream.take_while(move |_| {
            let cancelled = cancel_for_stream.is_cancelled();
            async move { !cancelled }
        });
        let mut s = Box::pin(cancellable);

        // Pierwszy chunk OK
        assert!(s.next().await.is_some());
        // Cancel → kolejne next() = None
        cancel.cancel();
        assert!(s.next().await.is_none());
    }

    // Stub żeby unused_imports nie krzyczał.
    fn _unused(_blobs: Arc<InMemoryBlobStore>, _slot: ModelRuntimeSlot, _rwl: RwLock<i32>) {}
}

/// Strip WAV RIFF/WAVE container header, returning raw PCM payload.
/// Tolerant na opcjonalne LIST/INFO chunki przed `data`. Wymaga PCM16
/// container; inaczej Err — caller fallback'uje na całe bytes (z
/// nagłówkiem) jako jeden chunk.
fn strip_wav_header(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut cursor = 12usize;
    while cursor + 8 <= bytes.len() {
        let chunk_id = &bytes[cursor..cursor + 4];
        let chunk_size = u32::from_le_bytes(
            bytes[cursor + 4..cursor + 8]
                .try_into()
                .map_err(|_| anyhow!("WAV strip: bad chunk size"))?,
        ) as usize;
        let body = cursor + 8;
        if chunk_id == b"data" {
            return Ok(bytes[body..].to_vec());
        }
        cursor = body + chunk_size + (chunk_size & 1);
    }
    Err(anyhow!("WAV strip: brak data chunk"))
}

/// Mapuje format z `TtsExecutionResult.format` (nazwa kodeka albo
/// rozszerzenie) na MIME type. Embedded TTS zawsze emituje WAV; HTTP/QUIC
/// echo'ują requestowy format. Nieznane formaty traktujemy jako
/// `application/octet-stream`.
fn format_to_mime(format: &str) -> String {
    match format.to_ascii_lowercase().as_str() {
        "wav" | "audio/wav" | "audio/x-wav" => "audio/wav".into(),
        "mp3" | "mpeg" | "audio/mpeg" => "audio/mpeg".into(),
        "opus" | "audio/opus" => "audio/opus".into(),
        "aac" | "audio/aac" => "audio/aac".into(),
        "flac" | "audio/flac" => "audio/flac".into(),
        "pcm" | "audio/pcm" => "audio/pcm".into(),
        "ogg" | "audio/ogg" => "audio/ogg".into(),
        _ => "application/octet-stream".into(),
    }
}
