# Etap 3a — Streaming usage w SSE (`stream_options.include_usage`)

**Plan v1.0 (do review codex)**
**Codex session ID:** `019dfca1-fef1-7ca1-b154-b73a796670a8`
**Data:** 2026-05-06
**Bazuje na:** Etap 2 (zamknięty, commit `b63c096`)

---

## Po co (use case)

Streaming chat completion przesyła tokeny chunk-po-chunku, kończy `[DONE]`. Klient
**nie wie ile tokenów zużył** — total tokens znane są dopiero po zakończeniu
generacji, a SSE jest już zamknięte. Bez tej informacji:

- **Billing** — nie da się policzyć kosztu request'a. Streaming = dziura w
  fakturze albo policzenie po stronie klienta (zawodne, każdy tokenizer inny).
- **Quota** — "user X ma 1M tokenów dziennie" niemożliwe do egzekwowania na
  streamingu.
- **UI** — ChatGPT-like "X tokens, Y seconds" pod odpowiedzią po skończeniu —
  bez tego pole jest puste.
- **Observability** — brak monitoringu zużycia tokenów per request.
- **Caching** — `finish_reason="length"` (cut-off) nie powinien być
  cache'owany; bez `finish_reason` brak sygnału.

OpenAI od marca 2024 ma `stream_options.include_usage: true` — gdy klient go
wyśle, przed `[DONE]` przychodzi dodatkowy chunk z `usage` i `finish_reason`,
poprzednie chunki mają `usage: null`. Format:

```json
data: {"id":"...","choices":[{"delta":{"content":"hello"}}], "usage": null}
data: {"id":"...","choices":[{"delta":{"content":" world"}}], "usage": null}
data: {"id":"...","choices":[{"delta":{},"finish_reason":"stop"}], "usage": null}
data: {"id":"...","choices":[], "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}}
data: [DONE]
```

Etap 3a dostarcza dokładnie ten kontrakt.

---

## Zakres (3a, single-axis)

1. `ChatCompletionRequest.stream_options: Option<StreamOptions>` z polem
   `include_usage: bool`. Default brak / false = nie zmieniamy zachowania (back
   compat).
2. Routing parses flag, propaguje do streaming pipeline.
3. **Flow streaming path** — po EOF z `StreamingExecution.stream`, jeśli
   include_usage=true, awaiting `outcome` receiver, emit dodatkowy
   `ChatCompletionChunk` z `choices: []` i `usage`. Zamiast detached log
   task'a, `outcome` jest realnie używany.
4. **Bare passthrough (non-flow)** — `ModelRuntimeExecutor::stream_chat`
   produkuje `ChatCompletionChunk`-i. Backend może lub nie wstawiać `usage` w
   ostatni chunk. Implementacja:
   - jeśli ostatni chunk z source ma `usage` (Some) — przepuszczamy bez zmian
   - jeśli nie ma — agregujemy `text_delta` po stronie routera (count token z
     tiktoken/podobnego — NIE) ALBO opieramy się o backend-specific
     `final_metrics` które dziś `executor.rs:398` ma jako `Done.final_metrics:_`
     (świadomie ignorowane). Etap 3a wpina to: `Done { final_metrics: Some(m) }`
     produkuje finalny chunk z `usage`.
5. Tests: `stream_options.include_usage=true` wstawia tail chunk z usage; brak
   pola = pre-Etap-3a zachowanie (no tail chunk).

---

## Co NIE robimy w Etap 3a

- streaming TTS / STT (`EnvelopeDelta::Audio` / `::Transcript`) — Etap 3c
- HTTP/2 trailers — alternatywa odrzucona (last-chunk usage = OpenAI compat)
- `stream_options.continuous_usage_stats` (per-chunk usage, eksperymentalne w
  OpenAI) — gdy ktoś poprosi, dodajemy. Etap 3a tylko `include_usage` boolean.
- multi-choice (`n>1`) handling — Etap 3a obsługuje tylko `choices[0]` (zgodne
  z całą obecną implementacją chat path).
- usage w bare passthrough gdy backend nie raportuje `final_metrics` — w
  Etapie 3a logujemy warn i nie dorzucamy tail chunk'a (klient widzi brak).
  Tiktoken-based estymacja routera-side wraca w Etap 3d razem z observability.

---

## Hard rules

10. **Tail chunk po EOF** — nigdy w środku stream'u. Per OpenAI: `usage` chunk
    przychodzi PO ostatnim regularnym chunk'u (z `finish_reason: "stop"` lub
    similar), PRZED `[DONE]`. Klient czyta sekwencyjnie.
11. **Tail chunk ma `choices: []`** — nie wpychamy delta tekstu w niego, żeby
    nie pomylić klienta. To tylko nośnik usage + finish_reason rollup.
12. **Bez `include_usage=true` zachowanie nie zmienia się** — back compat
    z klientami pre-stream-options.

---

## Typy

### `StreamOptions` (NEW w `flow_engine/openai/types.rs`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StreamOptions {
    /// When true, server emits an extra chunk with `usage` field populated
    /// before `[DONE]`. All regular chunks have `usage: None`.
    #[serde(default)]
    pub include_usage: bool,
}
```

### `ChatCompletionRequest` extension

```rust
pub struct ChatCompletionRequest {
    // ... existing fields ...

    /// Streaming behavior options (OpenAI extension marca 2024).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
}
```

### `ChatCompletionChunk.usage` field

Dziś chunk nie ma pola `usage`. Dodajemy:

```rust
pub struct ChatCompletionChunk {
    // ... existing fields ...

    /// Per-OpenAI stream_options.include_usage: tail chunk niesie tu rollup
    /// total/prompt/completion tokens. Regular chunki mają None.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}
```

---

## Streaming path (flow_engine)

### Bridge `envelope_stream_to_chunk_stream` rozszerzenie

`routing/streaming.rs::envelope_stream_to_chunk_stream` dziś detached spawnuje
`outcome.await` do log task'a. Po Etapie 3a parametryzujemy przez `include_usage`:

```rust
fn envelope_stream_to_chunk_stream(
    stream_exec: StreamingExecution,
    model: String,
    include_usage: bool,
) -> Pin<Box<dyn Stream<Item = Result<ChatCompletionChunk>> + Send>> {
    let StreamingExecution { stream, outcome } = stream_exec;
    let id = format!("flow-{}", uuid::Uuid::new_v4());
    let created = unix_timestamp();

    if !include_usage {
        // Pre-Etap-3a path — detached log, no tail chunk.
        tokio::spawn(async move { /* log outcome */ });
        let mapped = stream.map(/* delta -> chunk */);
        return Box::pin(mapped);
    }

    // include_usage=true: po stream EOF awaiting outcome, emit tail chunk.
    // Stream + tail = unfold state machine.
    let model_clone = model.clone();
    let composite = futures::stream::unfold(
        StreamState::Producing { stream, outcome, id: id.clone(), created, model: model_clone },
        |state| async move {
            match state {
                StreamState::Producing { mut stream, outcome, id, created, model } => {
                    match stream.next().await {
                        Some(Ok(delta)) => {
                            let chunk = delta_to_chunk(delta, &id, created, &model);
                            Some((Ok(chunk), StreamState::Producing { stream, outcome, id, created, model }))
                        }
                        Some(Err(e)) => Some((Err(e), StreamState::Done)),
                        None => {
                            // Source stream EOF — await outcome, emit tail.
                            match outcome.await {
                                Ok(o) => {
                                    let tail = build_tail_chunk(&o, &id, created, &model);
                                    Some((Ok(tail), StreamState::Done))
                                }
                                Err(_) => {
                                    tracing::warn!("flow finalizer dropped without outcome — no usage tail");
                                    None
                                }
                            }
                        }
                    }
                }
                StreamState::Done => None,
            }
        }
    );
    Box::pin(composite)
}

enum StreamState {
    Producing {
        stream: BoxStream<'static, Result<EnvelopeDelta>>,
        outcome: oneshot::Receiver<FlowExecutionOutcome>,
        id: String,
        created: u64,
        model: String,
    },
    Done,
}

fn build_tail_chunk(
    outcome: &FlowExecutionOutcome,
    id: &str,
    created: u64,
    model: &str,
) -> ChatCompletionChunk {
    ChatCompletionChunk {
        id: id.to_string(),
        object: "chat.completion.chunk".to_string(),
        created,
        model: model.to_string(),
        choices: vec![],
        usage: Some(Usage {
            prompt_tokens: outcome.usage.prompt_tokens as u32,
            completion_tokens: outcome.usage.completion_tokens as u32,
            total_tokens: outcome.usage.total_tokens as u32,
        }),
        // Zero pozostałych Etap-2 trailer-friendly fields. finish_reason siedzi
        // w ostatnim regularnym chunk'u, nie w usage tail.
        system_fingerprint: None,
        audio: None,
        detected_intent: None,
        detected_tools: None,
        transcribed_text: None,
        speaker_id: None,
        speaker_name: None,
    }
}
```

`outcome.await` jest blocking; klient widzi tail dopiero gdy executor finalizer
flush'nie outcome. To jest OK — finalizer emituje outcome zaraz po EOF source
stream'a (max kilka ms), więc latency ostatniego chunk = ~latency finalizer
persist.

---

## Bare passthrough (non-flow streaming)

`routing/streaming.rs` ma drugą ścieżkę przez `executor.stream_chat` →
`ExecutorChunkStream` (`Pin<Box<dyn Stream<Item=Result<ChatCompletionChunk>>>>`).
Backend (QUIC/HTTP/Local) produkuje `ChatCompletionChunk`-i. Etap 3a:

1. Jeśli klient nie poprosił `include_usage=true` → passthrough bez zmian.
2. Jeśli poprosił:
   - sprawdzamy ostatni chunk source — jeśli ma `usage: Some(_)`, dispatcher już
     dorzucił rollup (np. niektóre OpenAI-compat backendy to robią) →
     passthrough.
   - jeśli nie — `ExecutorChunkStream` przy `StreamChunkType::Done { final_metrics }`
     trzeba mapować na `ChatCompletionChunk { choices: [], usage: Some(metrics) }`.
     `services/runtime/executor.rs:398` dziś ignoruje `final_metrics` —
     Etap 3a to rozszerza:

```rust
// services/runtime/executor.rs ~ line 448
StreamChunkType::Done { final_metrics } => {
    // Etap 3a: jeśli backend dostarczył usage, emit jako tail.
    if let Some(metrics) = final_metrics {
        Some(Ok(ChatCompletionChunk {
            id: chat_id.clone(),
            object: "chat.completion.chunk".to_string(),
            created: created_ts,
            model: model_name_for_chunks.clone(),
            choices: vec![],
            usage: Some(Usage {
                prompt_tokens: metrics.prompt_tokens.unwrap_or(0) as u32,
                completion_tokens: metrics.completion_tokens.unwrap_or(0) as u32,
                total_tokens: metrics.total_tokens.unwrap_or(0) as u32,
            }),
            system_fingerprint: None,
            audio: None,
            detected_intent: None,
            detected_tools: None,
            transcribed_text: None,
            speaker_id: None,
            speaker_name: None,
        }))
    } else {
        None
    }
}
```

3. Filter middleware (`wrap_with_pii_streaming`) musi przepuszczać tail chunk
   (`choices: []`) bez modyfikacji — tail nie ma tekstu do filtrowania.

`StreamingProcessor::process_token` skip'uje gdy `choice.delta.content.is_none()`
i `choices.is_empty()` → już safe (sprawdzić w testach).

### Flag propagation

`route_chat_completion_stream` wyciąga `include_usage` z `request.stream_options`,
przekazuje do:
- `envelope_stream_to_chunk_stream` (flow path)
- konstrukcji executor chunk stream → musi zostać przekazane jako parameter,
  ale `executor.stream_chat` już zwraca chunk bez tego — ok, executor sam
  decyduje kiedy emitować tail chunk (w arm `Done`). Wystarczy że klient
  request ma `stream_options` przekazane do backendu (QUIC/HTTP) — niektóre
  backendy honorują flag, niektóre nie.

Decyzja: **router ZAWSZE emituje tail chunk gdy `include_usage=true`**, even
if backend już to zrobił. W praktyce backend tego dziś nie robi (ignoruje
flag), więc duplikat nie wystąpi. Gdyby się okazało że jakiś backend dorzuca,
addresujemy fixem (idempotent dedup po `usage.is_some()` na poprzednim
chunk'u).

---

## Aggregate usage gdy backend nic nie raportuje (defer Etap 3d)

Przypadek: bare passthrough → backend embedded (Apple MLX, llama.cpp local) →
chunki bez `final_metrics`. Tail chunk nie jest emitowany, klient widzi brak
mimo `include_usage=true`.

W Etapie 3a logujemy `tracing::warn!` i NIE emitujemy tail (klient z
`include_usage=true` ale brak usage = wiadome ograniczenie backendu, nie błąd).
Etap 3d może dodać tiktoken-based estymację router-side jako fallback.

---

## Call site refactor map

| Plik | Akcja | LOC |
|------|-------|-----|
| `api/openai/types.rs` | + `StreamOptions`, `ChatCompletionRequest.stream_options`, `ChatCompletionChunk.usage` | +60 |
| `routing/streaming.rs` | `envelope_stream_to_chunk_stream` parametryzacja + `StreamState` machine + `build_tail_chunk` | +120 |
| `routing/streaming.rs` | `route_chat_completion_stream` reads `request.stream_options.include_usage`, forwards | +10 |
| `services/runtime/executor.rs` | `Done { final_metrics }` arm produkuje tail chunk gdy include_usage | +60 |
| Tests w `routing/streaming.rs` / `services/runtime/executor.rs` | tail emission, flow + bare paths | +80 |

**Razem: ~330 LOC.** Małe sub-stage.

---

## Test strategy

### Unit testy

- `StreamOptions` deserialization (with / without `include_usage`)
- `build_tail_chunk` z `FlowExecutionOutcome` produkuje `choices: []` + `usage: Some`
- `envelope_stream_to_chunk_stream` z `include_usage=false` → no tail (back compat)
- `envelope_stream_to_chunk_stream` z `include_usage=true` → tail chunk emit po
  EOF source

### Integration

- E2E flow streaming: client request `stream_options.include_usage=true` →
  ostatni chunk niesie usage przed `[DONE]`. Mock backend w testach.
- Bare passthrough: backend embedded zwraca chunki bez final_metrics, log warn,
  no tail.
- `wrap_with_pii_streaming` przepuszcza tail chunk niezmieniony.

---

## Otwarte ryzyka

1. **`outcome.await` blokuje tail emission** — finalizer flush'uje outcome zaraz
   po EOF, więc latency dodatkowa = persist time. Jeśli persist DB jest powolny
   (>100ms), klient widzi tail z opóźnieniem. Mitygacja: persist jest spawn'owany
   w background task, outcome leci do oneshot natychmiast po build.
2. **Backend już dorzuca `usage` w chunk'u** — duplikat. Dziś żaden backend
   tego nie robi (sprawdzone), ale jakby ktoś włączył, klient dostałby 2 tail
   chunki. Mitygacja: w Etap 3d dodajemy idempotent dedup gdy widzimy
   `usage.is_some()` na pre-tail chunk'u.
3. **HTTP/2 trailers nie istnieją** — świadomie wybrane na rzecz last-chunk
   usage (OpenAI compat). Klient bez `stream_options.include_usage=true` nie ma
   sposobu na poznanie usage post-stream. To jest contract — klient musi opt-in.
4. **n>1 (multi-choice)** — Etap 3a obsługuje tylko `choices[0]`. n>1 wraca
   razem z multi-choice support w Etap 3b/c.
5. **Brak final_metrics w embedded backendach** — Apple MLX / llama.cpp local
   produkują chunki bez usage. Etap 3a emituje warn + brak tail. Etap 3d może
   dodać tiktoken estymację.

---

## Workflow

1. Plan codex review
2. Implementacja
3. Codex code review
4. Update CLAUDE.md o Etap 3a changes
5. Etap 3b zaczynamy od nowego planu
