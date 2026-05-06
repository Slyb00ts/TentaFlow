# Etap 3c — Streaming TTS

**Plan v1.1 (po round 1 codex)**

## Zmiany v1.0 → v1.1 (codex round 1)

1. **CRITICAL — single-chunk wrapper to nie streaming.** Plan v1.0
   proponował `stream_synthesize` jako `synthesize → 1 chunk → Stop`,
   co dodaje SSE+base64 overhead bez korzyści (klient mógłby tak samo
   użyć blocking `/v1/audio/speech`). Plan v1.1 oparty o **istniejący
   chunking** w `routing/tts.rs::synthesize_speech_stream` (już dziś
   tnie buffer na ~100ms PCM chunki, strip WAV header). Real streaming
   = real chunking, end-to-end.
2. **CRITICAL — konflikt z istniejącym `synthesize_speech_stream`.**
   Legacy używa callback API (`chunk_sink: F`), nowy endpoint potrzebuje
   `Stream<Item=TtsStreamChunk>`. Plan v1.1: refactor istniejącego API
   na `Stream`-based + dodanie `cancel_token` parametru. Stara
   sygnatura znika; jedyny caller (gdyby jakiś istniał — sprawdzić)
   migrowany na nową.
3. **IMPORTANT — cancel propagation.** Plan v1.1 dodaje route-level
   `CancellationToken` przekazywany do `synthesize_speech_stream`.
   Klient disconnect → `CancelOnDropStream` puszcza cancel → TTS abort
   na granicy chunka. Backend native cancel (np. abort sherpa stream
   socket) dochodzi z backend integration; Etap 3c pilnuje że nie
   dodajemy KOLEJNYCH chunków do bufora po cancel.
4. **IMPORTANT — clarify HTTP schema is private.** SSE shape `{audio_chunk:
   "<base64>", mime, finish_reason}` to **TentaFlow-specific** endpoint
   (`POST /v1/audio/speech/stream`), NIE OpenAI-compatible. OpenAI nie
   ma streaming TTS contract'a (Realtime API to inny protokół, WebSocket
   audio frames). Plan v1.1 nazywa endpoint `/v1/audio/speech/stream` ale
   document'uje wprost: prywatny TentaFlow contract.
5. **NIT akceptowany** — `AudioStreamChunk` shape OK dla 3c. Nie
   próbujemy obsłużyć Realtime/WebSocket bez translation w przyszłości.

## Plan v1.0 (do review codex)
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

## TtsDispatcherImpl::stream_synthesize (v1.1)

**Real chunking** — bazujemy na istniejącym mechanizmie z
`routing/tts.rs::synthesize_speech_stream` (PCM chunked at
~100ms / `TTS_STREAM_CHUNK_BYTES`). Refactor istniejącego API na
`Stream`-based + cancel:

```rust
// routing/tts.rs (REFACTOR)
const TTS_STREAM_CHUNK_BYTES: usize = 16_000; // ~100ms PCM @ 16kHz

impl Router {
    /// Etap 3c v1.1: zwraca Stream zamiast callback. Backend produkuje
    /// całość blocking, my tniemy buffer + emitujemy chunki PCM.
    /// `cancel` z `CancelOnDropStream` na endpoint side abortuje na
    /// granicy chunka — kolejne chunki nie są emitowane po cancel.
    pub async fn synthesize_speech_stream(
        &self,
        request: &TTSRequest,
        user: Option<UserContext>,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<TtsStreamChunk>>> {
        // Step 1: blocking synthesize (cała próbka).
        let route_result = self.synthesize_speech(request, user).await?;
        let mut bytes = route_result.response.bytes;
        let mime = route_result.response.format.clone();
        let sample_rate = ...;  // z backendu, gdy WAV — odkrywamy z header

        // Step 2: jeśli WAV, strip header — surowe PCM chunki bez RIFF.
        if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
            bytes = strip_wav_header(&bytes)?;
        }

        // Step 3: chunkuj. Stream owinięty w state machine z cancel check.
        let chunk_iter = bytes.chunks(TTS_STREAM_CHUNK_BYTES)
            .map(|c| c.to_vec())
            .collect::<Vec<_>>();
        let total = chunk_iter.len();

        let mime_clone = mime.clone();
        let stream = futures::stream::iter(chunk_iter.into_iter().enumerate())
            .map(move |(idx, chunk_bytes)| {
                let is_last = idx + 1 == total;
                Ok(TtsStreamChunk {
                    bytes_delta: chunk_bytes,
                    mime: mime_clone.clone(),
                    sample_rate,
                    finish_reason: if is_last {
                        Some(FinishReason::Stop)
                    } else {
                        None
                    },
                })
            })
            // CancelOnDrop bridge: gdy klient disconnect, cancel.cancel()
            // wywołane w drop, kolejne `next()` zwraca None.
            .take_while(move |_| {
                let cancelled = cancel.is_cancelled();
                async move { !cancelled }
            });
        Ok(Box::pin(stream))
    }
}
```

`TtsDispatcher::stream_synthesize` w `flow_engine/dispatchers/tts.rs`
nadal zostaje na traicie (dla flow-engine spójności), ale **endpoint
3c nie idzie przez dispatcher** — idzie direct przez
`Router::synthesize_speech_stream` (już ma backend dispatch logic).
TtsDispatcher::stream_synthesize w 3c implementacji robi to samo (woła
Router'a) — symetryczne dla future flow integration.

**Nie usuwamy** `TtsDispatcher::stream_synthesize` żeby trait surface
nie był asymetryczny vs `LlmDispatcher::stream_chat`. Etap 3e (TTS-as-flow
streaming) podepnie to do executor'a.

### Cancel mechanism

`CancelOnDropStream` (już istnieje w `flow_engine/cancel_on_drop.rs`)
opakuje finalny SSE response. Drop → `cancel_token.cancel()` →
`take_while` w stream machinerii widzi `is_cancelled() == true` → Stream
EOF.

**Limitation:** backend blocking synthesize nie jest abort'owany. Jeśli
backend bierze 5s na pełną syntezę i klient disconnect po 1s, backend
i tak skończy syntezę zanim my stream'ujemy. Real abort wymaga
backend-side cancel (sherpa: WebSocket close; xtts: drop generator
async).

Etap 3c **dokumentuje to ograniczenie** — pełny resource leak fix wraca
z backend native streaming + native cancel (osobny per-backend follow-up).
3c daje:
- działający chunked stream interface
- klient już może konsumować chunked PCM
- gdy backend support'uje real streaming, `synthesize_speech_stream`
  refactor'owany do real-time emit (zamiast post-blocking chunking)

---

## Endpoint `/v1/audio/speech/stream`

**TentaFlow-specific contract** (NIE OpenAI-compatible). OpenAI nie ma
streaming TTS endpointa (Realtime API to inny protokół, WebSocket audio
frames). Nasz endpoint = SSE z chunked PCM payload.

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

## CancelOnDropStream w SSE (v1.1)

Endpoint tworzy `CancellationToken`, przekazuje do
`synthesize_speech_stream`. Response body owinięty
`CancelOnDropStream(stream, cancel_token)`. Klient disconnect → hyper
drop body → `Drop` impl `cancel_token.cancel()` → stream `take_while`
widzi `is_cancelled()` true → EOF.

**Co robi:** zatrzymuje emisję KOLEJNYCH chunków po cancel. Bufor
wewnętrzny się nie wypełnia, kolejne `chunk_iter.next()` nie powodują
więcej allocs.

**Czego NIE robi (limitation Etap 3c):** nie abortuje backend blocking
synthesize. Jeśli backend bierze 5s, my czekamy 5s na pierwszy
`synthesize_speech_stream(...)` await — cancel sygnalizuje się dopiero
PO blocking syntezie. To jest świadomy ograniczenie 3c; pełny abort
wymaga backend-native cancel (sherpa WebSocket close, xtts generator
drop) i wraca z per-backend streaming follow-up.

---

## Call site refactor map

| Plik | Akcja | LOC |
|------|-------|-----|
| `flow_engine/envelope.rs` | + `EnvelopeDelta::Audio(AudioStreamChunk)` + struct | +35 |
| `flow_engine/dispatchers/tts.rs` | + `stream_synthesize` na traicie + re-export `TtsStreamChunk` | +30 |
| `flow_engine/dispatchers_impl/tts_impl.rs` | + `stream_synthesize` impl (fallback wrapper) | +50 |
| `flow_engine/dispatcher.rs` | + `pub fn tts()` accessor | +10 |
| `api/openai/server.rs` | + `handle_audio_tts_stream` + route | +120 |
| `routing/tts.rs` | refactor `synthesize_speech_stream`: callback `chunk_sink: F` → `Stream<Item=TtsStreamChunk>`. + `cancel_token` parametr. take_while bridge. + `strip_wav_header` reused. | +60 |
| Tests w `flow_engine/dispatchers_impl/tts_impl.rs` | unit test: stream_synthesize fallback emituje 1 chunk | +30 |
| Tests w `api/openai/server.rs` (lub integration) | E2E SSE response shape | +60 |

**Razem v1.1: ~330 LOC** (refactor `synthesize_speech_stream` zamiast
pseudo-streaming wrapper).

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
