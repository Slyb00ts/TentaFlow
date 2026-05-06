# Etap 3c — Streaming TTS

**Plan v1.0 (do review codex)**
**Codex session ID:** `019dfca1-fef1-7ca1-b154-b73a796670a8`
**Data:** 2026-05-06
**Bazuje na:** Etap 3b (commit `7b197c4`)

---

## Po co (use case)

OpenAI nie ma `/v1/audio/speech/stream` (TTS endpoint jest blocking — całe
audio zwracane w jednym response body). Ale realne aplikacje voice
chatbotowe potrzebują niskiej latencji:

- **Voice assistants** (Alexa-like) — zaczyna mówić zanim cała odpowiedź
  jest wygenerowana. Latency między user query a pierwszym dźwiękiem ma
  być <500ms.
- **Karaoke / dubbing pipelines** — chunki audio idą do encodera/mixera
  bez bufferowania całości.
- **Real-time podcasts / live narration** — TTS-as-you-go.

Backend support:
- **sherpa-onnx** ma streaming TTS przez WebSocket (chunked PCM).
- **xtts** (coqui) ma streaming generator (yields PCM frames).
- **kokoro / Apple AVSpeech / espeak** — blocking only (whole audio at end).

Etap 3c dostarcza **interface + single-chunk wrapper**:
- klient może już używać streaming endpoint
- backendy które streamują → real-time audio chunks
- backendy blocking → jeden duży chunk + finish_reason
- migracja transparent — klient zawsze konsumuje stream (nawet z 1 chunk)

Real backend streaming integration (sherpa WebSocket, xtts) wraca w
backend-specific follow-up.

---

## Zakres (3c, single axis)

1. **`EnvelopeDelta::Audio`** — nowy variant `EnvelopeDelta` enum dla
   spójności z chat streaming (`EnvelopeDelta::Llm`).
2. **`TtsStreamChunk` DTO** w `flow_engine/dispatchers/tts.rs`.
3. **`TtsDispatcher::stream_synthesize`** trait method — symetryczny do
   `LlmDispatcher::stream_chat`.
4. **`TtsDispatcherImpl::stream_synthesize`** — wrapper:
   - Etap 3c: zawsze fallback do blocking `synthesize` + emit jeden chunk
     z `bytes_delta=cały_blob`, `finish_reason=Some(Stop)`. Backend native
     streaming integration jest poza zakresem 3c.
   - Backend (HTTP/QUIC/embedded) decyduje przez `surface = TtsStream`;
     jeśli backend nie wspiera, dispatcher fallback'uje na blocking.
5. **Nowy endpoint `POST /v1/audio/speech/stream`** — przyjmuje ten sam
   payload co `/v1/audio/speech`, zwraca `Content-Type: text/event-stream`
   z `data: { audio_chunk: "<base64>", mime: "audio/wav", finish_reason: null }`
   per chunk. Ostatni chunk: `finish_reason: "stop"`. Po nim `[DONE]`.
6. **Routing layer:** `Router::synthesize_speech_stream` (already exists w
   różnym kształcie!) refactored do TtsDispatcher path. Sprawdzić aktualny
   stan.

---

## Co NIE robimy w 3c

- Backend-native streaming (sherpa WebSocket, xtts streaming gen) — każdy
  backend = osobna integracja
- TTS-as-flow streaming (`vision_llm` analog dla TTS w flow_engine) —
  wymaga generalizacji `execute_streaming` z LLM-only do multi-node-type;
  duża zmiana, defer do 3e/3f
- Streaming STT (`EnvelopeDelta::Transcript`) — osobny sub-stage 3d
- WebSocket transport (klient ↔ TentaFlow) — SSE wystarcza, WebSocket
  zbędny dla chunked PCM. Defer.
- Dynamic voice/format change mid-stream — nie istnieje w OpenAI API,
  brak use case.

---

## Hard rules

16. **Stream chunk = single TTS request, single voice/format.** Klient
    nie zmienia voice/format mid-stream. Mid-request control jest poza
    zakresem.
17. **Bytes_delta jest zawsze surowymi bytes audio formatu zadeklarowanego
    w pierwszym chunku.** Klient łączy chunki przez konkatenację. Dla WAV
    pierwszy chunk niesie nagłówek RIFF; kolejne chunki to czysty PCM
    payload bez nagłówka. Dla MP3/Opus/inne — backend dostarcza spójny
    container per chunk LUB pierwszy chunk + raw frames. **Etap 3c:**
    pojedynczy chunk z całością (single-chunk wrapper), więc problem WAV
    header split nie istnieje w 3c. Pojawia się gdy backend dorzuci real
    streaming.
18. **Finish_reason w ostatnim chunk'u.** Wszystkie poprzednie chunki mają
    `finish_reason=None`. Klient wykrywa końc stream'u przez `Some(Stop)`
    albo SSE `[DONE]` (oba sygnalizują koniec). Cancel/error → finish
    chunk z `finish_reason=Some(Cancelled)` / `Some(Error)`.

---

## Typy

### `EnvelopeDelta::Audio` (rozszerzenie `flow_engine/envelope.rs`)

```rust
#[derive(Debug, Clone)]
pub enum EnvelopeDelta {
    Llm(LlmStreamChunk),
    /// Etap 3c: streaming TTS audio.
    Audio(AudioStreamChunk),
}

#[derive(Debug, Clone)]
pub struct AudioStreamChunk {
    /// Surowe bytes audio. Format zadeklarowany w `mime`.
    pub bytes_delta: Vec<u8>,
    /// MIME type, np. "audio/wav". Niesione w każdym chunk'u (klient
    /// może kierować się pierwszym).
    pub mime: String,
    /// Sample rate Hz (gdy aplicable, np. PCM). MP3/Opus mogą mieć None.
    pub sample_rate: Option<u32>,
    /// Niesione tylko w terminalnym chunk'u. `None` dla mid-stream.
    pub finish_reason: Option<FinishReason>,
}
```

### `TtsStreamChunk` (`flow_engine/dispatchers/tts.rs`)

Symetryczny do `LlmStreamChunk` w envelope. Faktycznie ten sam typ co
`AudioStreamChunk` powyżej — re-eksportowany dla spójności surface.

```rust
// flow_engine/dispatchers/tts.rs
pub use crate::flow_engine::envelope::AudioStreamChunk as TtsStreamChunk;
```

### `TtsDispatcher` rozszerzenie

```rust
#[async_trait]
pub trait TtsDispatcher: Send + Sync {
    async fn synthesize(&self, req: TtsRequest) -> Result<TtsResponse>;

    /// Etap 3c: streaming TTS. Backendy które wspierają streaming yield
    /// chunki w czasie rzeczywistym; blocking backendy fallback'ują na
    /// `synthesize` + emit jeden chunk z całością.
    async fn stream_synthesize(
        &self,
        req: TtsRequest,
    ) -> Result<BoxStream<'static, Result<TtsStreamChunk>>>;
}
```

---

## TtsDispatcherImpl::stream_synthesize

Etap 3c implementacja: zawsze fallback. Backend native streaming będzie
podpinany per-backend gdy ich integracja dorzuci `stream_synthesize`.

```rust
async fn stream_synthesize(
    &self,
    req: TtsRequest,
) -> Result<BoxStream<'static, Result<TtsStreamChunk>>> {
    // Etap 3c: blocking fallback. Wszystkie backendy poprzez
    // `executor.execute_tts` zwracają cały blob; opakowujemy w jeden
    // chunk + emit. Real streaming integration (sherpa, xtts) wraca
    // w backend-specific follow-up.
    let response = self.synthesize(req).await?;
    let bytes = self.blobs.get(&response.audio).await?;
    let chunk = TtsStreamChunk {
        bytes_delta: bytes,
        mime: response.mime,
        sample_rate: response.sample_rate,
        finish_reason: Some(FinishReason::Stop),
    };
    let stream = futures::stream::once(async move { Ok(chunk) });
    Ok(Box::pin(stream))
}
```

Po 3c, gdy chcemy real streaming dla sherpa-onnx: dodajemy gałąź "if
backend supports streaming → backend.stream_chunks() else fallback".

---

## Endpoint `/v1/audio/speech/stream`

### Routing

`api/openai/server.rs::handle_request`:

```rust
("POST", "/v1/audio/speech/stream") => {
    handle_audio_tts_stream(req, router).await
}
```

### Handler

Mirror `handle_audio_tts` ale z SSE response:

```rust
async fn handle_audio_tts_stream(
    req: Request<Incoming>,
    router: Arc<Router>,
) -> std::result::Result<Response<StreamBody<...>>, hyper::Error> {
    let user_ctx = req.extensions().get::<UserContext>().cloned();
    let body_bytes = req.into_body().collect().await?.to_bytes();
    let tts_request: TTSRequest = match serde_json::from_slice(&body_bytes) {
        Ok(r) => r,
        Err(e) => return error_response(BAD_REQUEST, "invalid_request", ...),
    };

    let dispatcher = router.flow_dispatcher.as_ref().ok_or_else(|| {
        // ...
    })?;
    let blobs = dispatcher.blobs();
    let tts_dispatcher = dispatcher.tts(); // NEW accessor — patrz niżej

    let req_dto = build_tts_request_dto(&tts_request, user_ctx);
    let chunk_stream = match tts_dispatcher.stream_synthesize(req_dto).await {
        Ok(s) => s,
        Err(e) => return error_response(...),
    };

    // Bridge TtsStreamChunk → SSE data lines
    let sse_stream = chunk_stream.flat_map(|res| match res {
        Ok(chunk) => {
            let json = serde_json::json!({
                "audio_chunk": base64::encode(&chunk.bytes_delta),
                "mime": chunk.mime,
                "sample_rate": chunk.sample_rate,
                "finish_reason": chunk.finish_reason.and_then(|f| f.as_openai_str()),
            });
            futures::stream::iter(vec![
                Ok(Frame::data(format!("data: {json}\n\n").into())),
            ])
        }
        Err(e) => futures::stream::iter(vec![
            Ok(Frame::data(format!("data: {{\"error\": \"{e}\"}}\n\n").into())),
        ]),
    });

    // Append [DONE]
    let final_done = futures::stream::once(async {
        Ok(Frame::data("data: [DONE]\n\n".into()))
    });
    let combined = sse_stream.chain(final_done);

    let resp = Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("Connection", "keep-alive")
        .body(StreamBody::new(Box::pin(combined)))
        .unwrap();
    Ok(resp)
}
```

### `FlowDispatcher::tts()` accessor

Dispatcher ma już `blobs()`. Dodajemy `tts()` zwracający `Arc<dyn
TtsDispatcher>` z ContextFactory.

```rust
impl FlowDispatcher {
    pub fn tts(&self) -> Arc<dyn TtsDispatcher> {
        self.ctx_factory.tts.clone()
    }
}
```

---

## CancelOnDropStream w SSE

Klient disconnect → hyper drop response body → `CancelOnDropStream`
puszcza cancel_token (z meta — ale dla TTS-stream meta nie istnieje, bo
to nie jest flow). Etap 3c: nie podpinamy cancel — backend blocking
i tak fini'shuje przed emisją tail. Real cancel wraca razem z real
backend streaming.

---

## Call site refactor map

| Plik | Akcja | LOC |
|------|-------|-----|
| `flow_engine/envelope.rs` | + `EnvelopeDelta::Audio(AudioStreamChunk)` + struct | +35 |
| `flow_engine/dispatchers/tts.rs` | + `stream_synthesize` na traicie + re-export `TtsStreamChunk` | +30 |
| `flow_engine/dispatchers_impl/tts_impl.rs` | + `stream_synthesize` impl (fallback wrapper) | +50 |
| `flow_engine/dispatcher.rs` | + `pub fn tts()` accessor | +10 |
| `api/openai/server.rs` | + `handle_audio_tts_stream` + route | +120 |
| `routing/tts.rs` | (sprawdzić obecny `synthesize_speech_stream`, refactor lub usunąć — to dziś jest bare passthrough z chunkowaniem PCM) | +0 / -50 (cleanup) |
| Tests w `flow_engine/dispatchers_impl/tts_impl.rs` | unit test: stream_synthesize fallback emituje 1 chunk | +30 |
| Tests w `api/openai/server.rs` (lub integration) | E2E SSE response shape | +60 |

**Razem: ~285 LOC.** Małe sub-stage.

---

## Test strategy

### Unit
- `TtsDispatcherImpl::stream_synthesize` z fake `synthesize` zwraca jeden
  `TtsStreamChunk` z `finish_reason=Some(Stop)` i `bytes_delta=całość`.
- `EnvelopeDelta::Audio` round-trip.

### Integration
- Klient POST `/v1/audio/speech/stream` z `model+voice+input` dostaje
  `Content-Type: text/event-stream`, ciało `data: {...}\n\ndata: [DONE]\n\n`.
- Audio chunk JSON ma `audio_chunk` (base64), `mime`, `finish_reason`.

---

## Otwarte ryzyka

1. **`routing/tts.rs::synthesize_speech_stream` istnieje** — trzeba
   sprawdzić jego dzisiejszy stan przed implementacją. Może to być
   już chunkowanie PCM (legacy) które kolinduje z nowym path.
2. **Brak cancel propagation** — backend blocking sync produkuje całość
   przed yield, więc klient disconnect nie zatrzymuje syntezy. Real
   backend streaming musi to obsłużyć w follow-up.
3. **Single-chunk wrapper latency** — klient z 3c nie widzi pierwszego
   audio aż backend skończy całość. Wartość biznesowa 3c = mieć działający
   contract; latency win pojawia się dopiero z real backend streaming.
4. **WAV header w single chunk** — całość nagłówka RIFF leci w pierwszym
   (jedynym) chunk'u, klient musi to zaakceptować. Player'y to zwykle
   robią automatycznie.
5. **Brak per-chunk usage tokens** — TTS nie ma "tokens" w klasycznym
   sensie, niektóre backendy raportują character_count. Etap 3c pomija.
   Etap 3a streaming usage dla LLM nie aplikuje się tu (chat is text
   tokens, TTS is bytes).

---

## Workflow

1. Plan codex review
2. Sprawdzić stan `routing/tts.rs::synthesize_speech_stream` przed impl
3. Implementacja
4. Codex code review
5. Bundle CLAUDE.md update z 3a + 3b + 3c naraz (po 3c)
