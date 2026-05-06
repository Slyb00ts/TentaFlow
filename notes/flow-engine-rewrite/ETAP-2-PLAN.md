# Etap 2 — Typed porty + ArtifactKey + TTS-as-flow + trailers + FileBlobStore

**Plan v1.1 (po round 1 codex)**
**Codex session ID:** `019dfca1-fef1-7ca1-b154-b73a796670a8` (kontynuacja po Etapie 1)
**Data:** 2026-05-06
**Bazuje na:** Etap 1 v4.2 (zamknięty, commits do `400baa0`)

## Zmiany v1.0 → v1.1 (codex round 1)

1. **FileBlobStore zakres ścięty** — istniejący `flow_engine/blob_store.rs::FileBlobStore`
   ma już atomic write (temp+rename), dedup verify-on-disk, sha256 integrity check w
   `get`. Jedyny brakujący kawałek to `gc(retention)` (dziś stub). `BlobStore` trait
   świadomie nie ma `delete()` — dedup-by-sha sprawia że dwa `BlobRef` mogą wskazywać
   na ten sam plik i naiwny per-ref delete rozsadza drugi. Etap 2 = wire + gc impl,
   nie redesign.
2. **TTS-as-flow dropowało voice/format/language** — `TtsNodeAdapter::pick_optional_str`
   czyta tylko `node.config`. Plan v1.0 wsadzał voice/format/language do `envelope.meta`
   ale adapter nie czytał. Fix: rozszerzamy `TtsNodeAdapter` (i `LlmNodeAdapter`/
   `EmbeddingsNodeAdapter` analogicznie) o fallback `node.config -> envelope.meta`.
3. **`FlowDataType::from_value(Empty)`** zwracało `Any` co miesza "no payload" z
   "wildcard". Fix: `from_value(&FlowValue) -> Option<FlowDataType>`, gdzie `Empty`
   → `None`. Validation/runtime check gracefully handle'uje None (treat as
   "missing", caller decyduje czy to error).
4. **R8 nie jest bridgem** — `edge.data_type` to deklaracja, nie konwerter.
   Producent `Text` + konsument `Audio` zostają niekompatybilni niezależnie od
   edge.data_type. Plan v1.1 doprecyzowuje że R8 sprawdza 3 rzeczy: producent vs
   konsument (must be compatible), edge.data_type vs producent (must compat),
   edge.data_type vs konsument (must compat). Brak "bridge".
5. **Seedowane flows pass R8 trywialnie** — wszystkie edges domyślnie `Any`, więc
   walidacja przepuszcza. R8 zaczyna realnie chronić dopiero gdy GUI/save zacznie
   produkować konkretne `data_type`. Plan v1.1 stwierdza to wprost zamiast
   sugerować że R8 łapie TTS-flow output mismatch (łapie tylko `flow_outcome_to_tts_result`
   runtime check).

---

## Kontekst

Etap 1 dał: typed FlowEnvelope, narrow capability dispatchers, 13 node adapterów,
1-input-edge hard rule, streaming end-shape, cancel/deadline propagation. Wszystkie
edges są dziś typeless (default `from_port=full`/`to_port=in`, zero info o typie
danych płynących). Adaptery deklarują tylko nazwy portów. TTS-as-flow zwraca błąd
(`executor.rs:1540`). BlobStore default = InMemory (audio/video w RAM). Brak headerów
trailerowych po blocking response.

Etap 2 dodaje typed porty + ArtifactKey + TTS-as-flow + non-streaming trailers +
FileBlobStore. Streaming TTS / multimodal trigger / HTTP/2 trailers / cardinality > 1
zostają na Etap 3.

---

## Zakres (5 osi)

1. **Typed porty** — `FlowEdge.data_type`, `FlowDataType` enum, walidacja R8
2. **ArtifactKey registry** — deklaracje adapterów (produced/consumed) — bez nowej
   reguły walidacji, tylko dokumentacja + GUI hint surface
3. **TTS-as-flow** — `executor.rs::dispatch_tts_blocking::Flow` arm wywołuje
   `FlowDispatcher::dispatch_by_flow_id`, output flow musi mieć `payload =
   FlowValue::Audio`
4. **Non-streaming trailers** — `X-Want-Trailers: true` header w request → response
   dostaje `X-Tentaflow-{Latency-Ms,*Tokens,Finish-Reason}` headery
5. **FileBlobStore** — pełny `<tentaflow_home>/blobs/<sha[0:2]>/<sha[2:4]>/<sha>.bin`,
   atomic write, gc(retention)

---

## Co NIE robimy w Etapie 2

- streaming TTS (`EnvelopeDelta::Audio`) — Etap 3
- streaming STT (`EnvelopeDelta::Transcript`) — Etap 3
- HTTP/2 trailers (post-EOF SSE trailers) — Etap 3
- multimodal LLM (Vision/Omni) — Etap 3
- ForEach loop / Many cardinality / Merge node — Etap 3
- CEL conditions — Etap 3
- compiled flow persistence (replay z DB) — Etap 3
- ArtifactKey validation rule (R9) — Etap 3 (Etap 2 ma tylko deklaracje)
- usuwanie wariantu `FlowDataType::Any` — `Any` zostaje przez Etap 2 jako fallback
  dla legacy flow_json (default value gdy `data_type` brak w edge JSON)

---

## Hard rules (nowe)

8. **Typed edge compatibility** — `FlowEdge.data_type` musi się zgadzać z
   `producer.output_port_type(from_port)` i `consumer.input_port_type(to_port)`.
   `Any` na którejkolwiek stronie = wildcard (compatible z każdym konkretnym typem).
   Ta reguła wchodzi do `validation::validate` jako R8.
9. **Add-only artifact registry** — adapter deklaruje listę produced/consumed
   artifact keys + typów. Walidacja MOŻE w przyszłości sprawdzać że konsument widzi
   producenta upstream (R9, Etap 3). Etap 2: tylko deklaracje, bez nowej reguły
   sprawdzania.

---

## Typy

### `FlowDataType` (NEW w `flow_engine/types.rs`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FlowDataType {
    /// Niesprecyzowany typ — kompatybilny z każdym konkretnym (transitional
    /// fallback dla edges bez deklaracji albo dla adapterów które nie wiedzą
    /// którego typu data popłynie). Etap 3 wymusza konkretny typ; `Any`
    /// znika z public surface.
    #[default]
    Any,
    Text,
    Audio,
    Image,
    Video,
    Embedding,
    Json,
}

impl FlowDataType {
    /// Compatibility: `Any` na której kolwiek stronie = match. W przeciwnym
    /// wypadku: dokładny match.
    pub fn compatible_with(self, other: FlowDataType) -> bool {
        matches!(self, FlowDataType::Any)
            || matches!(other, FlowDataType::Any)
            || self == other
    }

    /// Mapowanie z `FlowValue` na typ — używane przy runtime sanity checks
    /// (Etap 3) i w testach. `Empty` → `None` (brak payloadu ≠ wildcard;
    /// caller decyduje czy to legalne — np. trigger może wystartować flow
    /// bez payloadu). `Any` jako wariant `FlowDataType` istnieje tylko jako
    /// transitional default na edges, nie produkujemy go z FlowValue.
    pub fn from_value(v: &crate::flow_engine::envelope::FlowValue) -> Option<Self> {
        use crate::flow_engine::envelope::FlowValue;
        match v {
            FlowValue::Empty => None,
            FlowValue::Text(_) => Some(FlowDataType::Text),
            FlowValue::Json(_) => Some(FlowDataType::Json),
            FlowValue::Audio { .. } => Some(FlowDataType::Audio),
            FlowValue::Image { .. } => Some(FlowDataType::Image),
            FlowValue::Video { .. } => Some(FlowDataType::Video),
            FlowValue::Embedding(_) => Some(FlowDataType::Embedding),
        }
    }
}
```

### `FlowEdge.data_type` (extend)

```rust
pub struct FlowEdge {
    // ... existing fields ...

    /// Deklarowany typ danych płynących edge'em. Default `Any` dla legacy
    /// flow_json. Walidacja R8 sprawdza match z producent.output_port_type
    /// i konsument.input_port_type. Skip serialize gdy default żeby legacy
    /// flow_json round-trippowały byte-identycznie.
    #[serde(
        default,
        skip_serializing_if = "is_default_data_type"
    )]
    pub data_type: FlowDataType,
}

fn is_default_data_type(t: &FlowDataType) -> bool {
    matches!(t, FlowDataType::Any)
}
```

### `NodeAdapter` extension

```rust
pub trait NodeAdapter: Send + Sync {
    // existing
    fn node_type(&self) -> &str;
    fn supported_input_ports(&self) -> &[&'static str];
    fn supported_output_ports(&self) -> &[&'static str];
    async fn execute(...) -> Result<FlowEnvelope>;

    // NEW: per-port type. Default `Any` żeby legacy adaptery działały bez
    // override (jeśli kiedyś wprowadzimy nowy adapter który zapomni
    // override'ować, nadal będzie compatible z każdym edge — degraded ale
    // not broken).
    fn input_port_type(&self, _port: &str) -> FlowDataType {
        FlowDataType::Any
    }
    fn output_port_type(&self, _port: &str) -> FlowDataType {
        FlowDataType::Any
    }

    // NEW: ArtifactKey deklaracje. Default puste.
    fn produced_artifacts(&self) -> &[(&'static str, FlowDataType)] {
        &[]
    }
    fn consumed_artifact_types(&self) -> &[(&'static str, FlowDataType)] {
        &[]
    }
}
```

---

## Per-adapter port types (Etap 2 deklaracje)

Tabela egzekwowana przez R8 walidację:

| Adapter | Input port | Input type | Output port | Output type | Produced artifacts |
|---------|-----------|------------|-------------|-------------|---------------------|
| `trigger` | (brak) | — | `full` | `Any` | — |
| `output` | `in` | `Any` | `full` | `Any` | — |
| `condition` | `in` | `Any` | `true`/`false` | `Any` | — |
| `pii_filter` | `in` | `Text` | `full` | `Text` | — |
| `tts_clean` | `in` | `Text` | `full` | `Text` | — |
| `llm` | `in` | `Text` | `stream`/`full` | `Text` | — |
| `stt` | `in` | `Audio` | `full` | `Text` | `source_audio: Audio` |
| `tts` | `in` | `Text` | `full` | `Audio` | `source_text: Text` |
| `embeddings` | `in` | `Text` | `full` | `Embedding` | — |
| `memory` | `in` | `Text` | `full` | `Text` | — |
| `conversation_history` | `in` | `Any` | `full` | `Any` | — |
| `session_context` | `in` | `Any` | `full` | `Any` | — |
| `speaker_context` | `in` | `Any` | `full` | `Any` | — |

`trigger` zostaje `Any` na output bo trigger może wyprodukować Text (chat),
Audio (audio chat — Etap 3 multimodal trigger jeszcze nie ma) lub Empty (no-payload
kick). Trigger w Etapie 2 = passthrough type-wise.

`condition` zostaje `Any` na in/out bo condition tylko routes — jakikolwiek typ
wchodzi, ten sam wychodzi.

`memory`/`conversation_history`/`session_context`/`speaker_context` zostają `Any`
na in/out bo mutują tylko `envelope.context.system_prompts` / `messages`, payload
pass-through.

---

## Validation R8 (rozszerzenie `validation.rs`)

`edge.data_type` to deklaracja, NIE konwerter — Etap 2 nie ma rzutowania typów.
Walidacja sprawdza 3 niezależne pary kompatybilności:

```rust
// W pętli edges, po istniejących sprawdzeniach R3 (port membership):
let from_type = from_adapter.output_port_type(&edge.from_port);
let to_type = to_adapter.input_port_type(&edge.to_port);

// (a) producent vs konsument — bez tego edge.data_type byłby tylko grzecznym
//     opisem przy realnym mismatchu. `Any` na każdej stronie = wildcard.
if !from_type.compatible_with(to_type) {
    return Err(FlowValidationError::EdgePortTypesMismatch {
        from_node: edge.from.clone(),
        from_port: edge.from_port.clone(),
        from_type,
        to_node: edge.to.clone(),
        to_port: edge.to_port.clone(),
        to_type,
    });
}

// (b) edge.data_type vs producent.output_port_type
if !edge.data_type.compatible_with(from_type) {
    return Err(FlowValidationError::EdgeTypeMismatch {
        edge_id: edge.id.clone().unwrap_or_else(|| format!("{}->{}", edge.from, edge.to)),
        side: "from",
        edge_type: edge.data_type,
        port_type: from_type,
    });
}

// (c) edge.data_type vs konsument.input_port_type
if !edge.data_type.compatible_with(to_type) {
    return Err(FlowValidationError::EdgeTypeMismatch {
        edge_id: edge.id.clone().unwrap_or_else(|| format!("{}->{}", edge.from, edge.to)),
        side: "to",
        edge_type: edge.data_type,
        port_type: to_type,
    });
}
```

`FlowValidationError` rozszerza się o 2 warianty: `EdgeTypeMismatch` (edge declared
nie pasuje do portu) i `EdgePortTypesMismatch` (producent vs konsument różne
konkretne typy).

**R8 nie chroni TTS-flow output:** `output` adapter ma `input_port_type("in") = Any`
(passthrough). Producent (np. `tts`) ma `output_port_type("full") = Audio`. Edge
między nimi przechodzi R8 niezależnie od `edge.data_type` (`Any` ↔ `Audio` =
compatible). Walidacja "TTS flow musi kończyć Audio na payloadzie" siedzi w runtime
checku `flow_outcome_to_tts_result` (zwraca Internal gdy outcome.payload nie jest
Audio), nie w R8. To świadomy choice: R8 sprawdza port-level compatibility, nie
end-of-flow payload type.

**R8 transitional weakness:** wszystkie obecne seedowane flowy (`Standardowy
pipeline LLM`, `teams-flow`) mają `edge.data_type = Any` (default), więc R8
przepuszcza je trywialnie. Realna ochrona zaczyna działać dopiero gdy GUI zacznie
zapisywać konkretne `data_type` na edges (Etap 2 dorzuca to do save handler) i gdy
trigger dostaje typed output (Etap 3 multimodal trigger). Etap 2 daje fundament,
nie pełną twardą walidację.

---

## TTS-as-flow

### Aktualnie (Etap 1)

`services/runtime/executor.rs:1539`:
```rust
ResolvedExecutionTarget::Flow { .. } => Err(ExecutorError::Internal(
    "TTS via flow not supported yet".into(),
)),
```

### Po Etapie 2

```rust
ResolvedExecutionTarget::Flow { flow_id, .. } => {
    let dispatcher = self
        .flow_dispatcher
        .as_ref()
        .ok_or(ExecutorError::FlowDispatcherUnavailable)?;
    ctx.enter_flow(*flow_id)?;
    let (initial, meta) = tts_request_to_initial_envelope(&request, ctx.user.clone());
    let dispatch_result = dispatcher
        .dispatch_by_flow_id(*flow_id, initial, meta)
        .await;
    ctx.leave_flow();
    let outcome = dispatch_result
        .map_err(|e| ExecutorError::Internal(e.to_string()))?
        .ok_or_else(|| ExecutorError::FlowEmptyResult { model: request.model.clone() })?;
    flow_outcome_to_tts_result(outcome, dispatcher.blobs()).await
}
```

### Adapter envelope-meta fallback

Plan v1.0 był niekompletny: `tts_request_to_initial_envelope` wsadzał voice/format/
language do `envelope.meta`, ale `TtsNodeAdapter::pick_optional_str` czytał tylko
`node.config`. Plan v1.1 dodaje fallback `node.config -> envelope.meta` w 3
adapterach (tts, llm, embeddings) — symetrycznie do istniejącego `pick_model`:

```rust
// flow_engine/node_adapters/tts.rs
fn pick_optional_str(node: &FlowNode, envelope: &FlowEnvelope, key: &str) -> Option<String> {
    // 1. Override z node config — najwyższy priorytet (operator pin'uje
    //    konkretne ustawienie dla tej ścieżki flow).
    if let Some(s) = node
        .config
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        return Some(s.to_string());
    }
    // 2. Fallback z envelope.meta — request seed (np. TTS-as-flow z user
    //    request voice/format/language).
    envelope
        .meta
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}
```

Klucze które adapter czyta (z fallback) — kontrakt request → envelope → adapter:

| Adapter | Key | Source w trigger seed | Default gdy brak |
|---------|-----|------------------------|------------------|
| `tts` | `voice` | `TTSRequest.voice` | (brak — `TtsDispatcherImpl` ma `DEFAULT_VOICE = "alloy"`) |
| `tts` | `format` | `TTSRequest.response_format` | (brak — backend wybiera) |
| `tts` | `language` | `TTSRequest.language` | (brak) — Etap 2 dodaje pole `language` do `TtsRequest` DTO |
| `llm` | `temperature` | `ChatCompletionRequest.temperature` | (brak — backend default) |
| `llm` | `max_tokens` | `ChatCompletionRequest.max_tokens` | (brak) |
| `embeddings` | `dimensions` | `EmbeddingRequest.dimensions` | (brak) — Etap 2 dodaje |
| `embeddings` | `encoding_format` | `EmbeddingRequest.encoding_format` | (brak) |

Plan v1.1 rozszerza `TtsRequest` DTO (`flow_engine/dispatchers/tts.rs`) o pole
`language: Option<String>` żeby dispatcher impl mógł je propagować do `TTSRequest`
runtime.

### TTS seed envelope helper

```rust
// services/runtime/executor.rs
fn tts_request_to_initial_envelope(
    request: &TTSRequest,
    user: Option<crate::auth::acl::UserContext>,
) -> (FlowEnvelope, FlowRequestMeta) {
    let mut env = FlowEnvelope::empty();
    env.payload = FlowValue::Text(request.input.clone());
    env.meta.insert("tts_model".into(), Value::String(request.model.clone()));
    env.meta.insert("voice".into(), Value::String(request.voice.clone()));
    if let Some(fmt) = &request.response_format {
        env.meta.insert("format".into(), Value::String(fmt.clone()));
    }
    if let Some(lang) = &request.language {
        env.meta.insert("language".into(), Value::String(lang.clone()));
    }
    let mut meta = FlowRequestMeta::new(uuid::Uuid::new_v4().to_string());
    if let Some(u) = user {
        meta.user_id = Some(u.user_id);
        meta.user_role = Some(u.role);
    }
    (env, meta)
}

async fn flow_outcome_to_tts_result(
    outcome: FlowExecutionOutcome,
    blobs: Arc<dyn BlobStore>,
) -> Result<TtsExecutionResult, ExecutorError> {
    match outcome.final_envelope.payload {
        FlowValue::Audio { blob_ref, mime, .. } => {
            let bytes = blobs.get(&blob_ref).await
                .map_err(|e| ExecutorError::Internal(format!("blob read: {e}")))?;
            let format = mime_to_format(&mime);
            Ok(TtsExecutionResult { bytes, format })
        }
        other => Err(ExecutorError::Internal(format!(
            "tts flow returned non-Audio payload kind: {}",
            other.kind()
        ))),
    }
}

fn mime_to_format(mime: &str) -> String {
    match mime {
        "audio/wav" | "audio/x-wav" => "wav".into(),
        "audio/mpeg" => "mp3".into(),
        "audio/opus" => "opus".into(),
        "audio/aac" => "aac".into(),
        "audio/flac" => "flac".into(),
        "audio/ogg" => "ogg".into(),
        _ => "wav".into(), // konserwatywny fallback
    }
}
```

### `FlowDispatcher::blobs()` accessor (NEW)

```rust
impl FlowDispatcher {
    pub fn blobs(&self) -> Arc<dyn BlobStore> {
        self.ctx_factory.blobs.clone()
    }
}
```

### Flow shape egzekwowany przez R8

Flow TTS musi mieć:
- ostatni node przed `output` produkuje `Audio` (np. `tts` adapter)
- edge ten ma `data_type: Audio` (deklarowane w GUI; default `Any` przepuszcza, ale
  GUI po Etapie 2 sugeruje `Audio` na podstawie `output_port_type` producenta)
- `output` adapter has `input_port_type("in") = Any` więc akceptuje
- runtime check w `flow_outcome_to_tts_result` bouncuje gdy payload nie jest Audio

---

## Non-streaming trailers

### Header contract

- **Request:** `X-Want-Trailers: true` (case-insensitive). Brak = bez trailerów.
- **Response (gdy want-trailers obecne i request blocking, NIE SSE):**
  - `X-Tentaflow-Latency-Ms: 1234`
  - `X-Tentaflow-Prompt-Tokens: 100`
  - `X-Tentaflow-Completion-Tokens: 50`
  - `X-Tentaflow-Total-Tokens: 150`
  - `X-Tentaflow-Finish-Reason: stop` (lub `length`/`tool_calls`/`content_filter`/
    `null` dla cancelled/error)

Streaming SSE: header check ignored w Etapie 2 (HTTP/1 trailers wymagają chunked
encoding work; HTTP/2 trailers w Etapie 3).

### Plumbing

`RouteMetadata` rozszerza się:

```rust
pub struct RouteMetadata {
    // existing
    pub served_by_node: String,
    pub backend_type: String,
    pub strategy_used: String,
    pub fallbacks_tried: u32,
    pub hop_count: u32,
    pub latency_ms: Option<f64>,

    // NEW: trailer-friendly fields
    pub usage: Option<TokenUsageMetadata>,
    pub finish_reason: Option<String>,
}

pub struct TokenUsageMetadata {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}
```

Populate w 3 miejscach:
1. `routing/chat.rs::route_chat_completion` (flow path) — z `outcome.usage` + `outcome.finish_reason`
2. `routing/chat.rs::route_chat_completion` (executor path) — z `response.usage` + `response.choices[0].finish_reason`
3. `services/runtime/executor.rs` embeddings/TTS Flow path — z outcome

### `api/openai/server.rs` emit

```rust
fn want_trailers(req: &Request<Body>) -> bool {
    req.headers()
        .get("x-want-trailers")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

// Po `route_chat_completion(...)` Ok arm:
if want_trailers {
    if let Some(usage) = &route_result.metadata.usage {
        resp.headers_mut().insert("x-tentaflow-prompt-tokens", usage.prompt_tokens.into());
        resp.headers_mut().insert("x-tentaflow-completion-tokens", usage.completion_tokens.into());
        resp.headers_mut().insert("x-tentaflow-total-tokens", usage.total_tokens.into());
    }
    if let Some(latency) = route_result.metadata.latency_ms {
        resp.headers_mut().insert("x-tentaflow-latency-ms", (latency as u64).into());
    }
    if let Some(fr) = &route_result.metadata.finish_reason {
        resp.headers_mut().insert("x-tentaflow-finish-reason", fr.parse().unwrap_or_else(|_| "stop".parse().unwrap()));
    }
}
```

---

## FileBlobStore

### Stan obecny (po Etapie 1)

`flow_engine/blob_store.rs::FileBlobStore` jest już zaimplementowany:
- Sharded layout `<root>/<sha[0:2]>/<sha[2:4]>/<full_sha>.bin`
- Atomic write: `temp + fsync + rename` (rename same-fs)
- Dedup with verify-on-disk (corrupted half-written blob z poprzedniej sesji
  zostaje wykryty i nadpisany)
- `get` robi sha256 integrity check po read (corrupted content rzuca błąd zamiast
  cicho propagować)
- Race przy concurrent put tego samego content: target verify after rename failure,
  redundant temp cleanup
- `delete()` świadomie nie ma w trait — dedup-by-sha sprawia że dwa BlobRef
  wskazujące na ten sam plik nie mogą bezpiecznie usuwać per ref

`gc(retention)` jest dziś stub (zwraca 0).

### Co Etap 2 dodaje

1. **`gc(retention)` impl** — walk po `<root>/*/*/*.bin`, stat mtime, usuń pliki
   starsze niż `retention`. Bez refcount/orphan registry: gc pasuje gdy żadne
   request nie używa obecnie blobs starszych niż retention. Default retention 24h
   (config'owalny). Race z concurrent put tego samego content: get-after-delete
   może dostać NotFound, caller potraktuje jako transient błąd. To jest akceptowalny
   trade-off dla pierwszej iteracji GC.

```rust
async fn gc(&self, retention: Duration) -> Result<u64> {
    let root = self.root.clone();
    let cutoff = std::time::SystemTime::now() - retention;
    let removed = tokio::task::spawn_blocking(move || -> std::io::Result<u64> {
        let mut count = 0u64;
        for shard in std::fs::read_dir(&root)?.flatten() {
            if !shard.file_type()?.is_dir() { continue; }
            for sub in std::fs::read_dir(shard.path())?.flatten() {
                if !sub.file_type()?.is_dir() { continue; }
                for blob in std::fs::read_dir(sub.path())?.flatten() {
                    let meta = blob.metadata()?;
                    let modified = meta.modified().unwrap_or_else(|_| std::time::SystemTime::now());
                    if modified < cutoff {
                        if std::fs::remove_file(blob.path()).is_ok() {
                            count += 1;
                        }
                    }
                }
            }
        }
        Ok(count)
    })
    .await
    .map_err(|e| anyhow::anyhow!("blob gc join: {e}"))??;
    Ok(removed)
}
```

2. **GC scheduler (opcjonalny)** — w `Router::start` (lub osobny tick task)
   spawnujemy task który co N minut woła `blob_store.gc(retention)`. Defaults:
   interval = 1h, retention = 24h. W Etapie 2 zostawiamy to OPCJONALNE — operator
   może wywołać gc przez ręczny endpoint admina, scheduler zostaje za feature flag
   `blob_gc_scheduler_enabled` w config (default false). Jeśli refcount/orphan
   registry kiedyś powstanie, scheduler będzie używał go zamiast czystego mtime.

3. **Wire FileBlobStore zamiast InMemoryBlobStore w `Router::new`** — dziś
   `FlowDispatcher::new` tworzy `Arc::new(InMemoryBlobStore::new())` wewnątrz.
   Etap 2: `FlowDispatcher::new` przyjmuje `blobs: Arc<dyn BlobStore>` jako
   parametr. `Router::new` decyduje:
   - Default: `FileBlobStore` w `<TENTAFLOW_HOME>/blobs`
   - Override: `TENTAFLOW_BLOB_STORE=memory` → `InMemoryBlobStore` (testy/local dev)

### Bootstrap

`Router::new` (`routing/router.rs`):
```rust
let blobs: Arc<dyn BlobStore> = match std::env::var("TENTAFLOW_BLOB_STORE").as_deref() {
    Ok("memory") => Arc::new(InMemoryBlobStore::new()),
    _ => {
        let root = std::env::var("TENTAFLOW_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home_dir().join(".tentaflow"))
            .join("blobs");
        Arc::new(FileBlobStore::new(root).expect("init FileBlobStore"))
    }
};
let flow_dispatcher = db.map(|pool| {
    Arc::new(FlowDispatcher::new(
        pool,
        service_manager.clone(),
        runtime_slot.clone(),
        stt_runtime_slot.clone(),
        blobs,
    ))
});
```

`FlowDispatcher::new` przyjmuje `blobs: Arc<dyn BlobStore>` jako 5-ty parametr
(zamiast tworzyć InMemoryBlobStore wewnątrz).

---

## Dependencies

`sha2` jest już używane przez `blob_store.rs`; nie trzeba dodawać. Inne nowe deps:
brak — Etap 2 używa standard library + istniejących crate'ów.

---

## Call site refactor map

| Plik | Akcja | LOC |
|------|-------|-----|
| `flow_engine/types.rs` | + `FlowDataType` enum, `FlowEdge.data_type` field | +80 |
| `flow_engine/node_adapter.rs` | + 4 default trait methods | +30 |
| `flow_engine/node_adapters/*.rs` | per-adapter override (12 adapterów × ~10 linii) | +120 |
| `flow_engine/validation.rs` | R8 + 2 nowe warianty błędów + testy | +180 |
| `flow_engine/blob_store.rs` | impl `gc(retention)` + 2-3 testy GC (FileBlobStore istnieje) | +80 |
| `flow_engine/dispatcher.rs` | przyjmuje `blobs` parameter, `pub fn blobs()` | +20 |
| `flow_engine/converter.rs` | + `flow_outcome_to_tts_result` | +50 |
| `services/runtime/executor.rs` | TTS Flow arm, `tts_request_to_initial_envelope` | +80 |
| `routing/middleware.rs` | + `usage`/`finish_reason` w `RouteMetadata` | +30 |
| `routing/chat.rs` | populate metadata.usage/finish_reason | +30 |
| `api/openai/server.rs` | want-trailers parsing + header emit | +50 |
| `routing/router.rs` | wire `FileBlobStore` → `FlowDispatcher::new` | +30 |
| `Cargo.toml` | `sha2` / `hex` deps (jeśli brak) | +2 |

**Razem: ~780 LOC** (po ścięciu FileBlobStore z plan v1.0). Mieści się w jednej
sesji bez problemu.

---

## Test strategy

### Unit testy

- `FlowDataType::compatible_with` — Any wildcard, exact match, mismatch
- `FlowDataType::from_value` — wszystkie warianty FlowValue
- Adapter `input_port_type`/`output_port_type` per node type — egzekwują tabelę
- `validation::validate` R8 — pozytywne/negatywne (Text→Audio mismatch, Any
  bridge, dwie konkretne na portach z Any edge)
- `FileBlobStore::{put,get,delete}` — round-trip
- `FileBlobStore::put` deduplikacja sha (drugi put tego samego content nie psuje)
- `FileBlobStore::gc` — usuwa stare pliki, zostawia świeże
- `flow_outcome_to_tts_result` — Audio payload OK, Text payload Err

### Integration

- TTS-as-flow end-to-end z fake `TtsDispatcher` produkującym Audio payload —
  `dispatcher.dispatch_by_flow_id` → outcome z FlowValue::Audio →
  `flow_outcome_to_tts_result` → bytes
- Trailers: `route_chat_completion_stream` (blocking path z X-Want-Trailers)
  produkuje response z X-Tentaflow-* headerami
- `seeded_flows_pass_adapter_validation` musi przejść z R8 (oznacza że seed flows
  mają poprawne data_type albo używają default Any — flowy seedowane wszystkie
  mają default Any, więc przechodzą)

---

## Otwarte ryzyka

1. **Legacy flow_json bez `data_type`** — round-trip przez `Any` default. Nowy
   flow zapisany w GUI dostaje konkretny `data_type` z metadata adaptera. Mix
   legacy/new w bazie OK.
2. **R8 vs streaming end-shape (R7)** — niezależne. R7 sprawdza shape (jeden
   producent stream → output), R8 sprawdza typ (LLM stream → Text → output Text).
   Oba muszą przejść.
3. **FileBlobStore concurrent put różnych content** — sha różne, paths różne, brak
   race. Concurrent put tego samego content: rename atomic, ostatni wygrywa,
   content identyczny więc OK.
4. **FileBlobStore na Windows** — `std::fs::rename` przez różne dyski może fail'ować.
   Mitygacja: `temp` w samym dir docelowym (już zrobione), nie w `/tmp`.
5. **Trailers ignorowane przez axum proxy / nginx?** — zwykłe response headery, nie
   prawdziwe trailers HTTP, więc proxy je przepuszcza. Klient po prostu czyta
   `response.headers.get("x-tentaflow-latency-ms")`.
6. **TTS-as-flow: flow musi produkować Audio** — runtime check w
   `flow_outcome_to_tts_result` zwraca Internal gdy nie. R8 walidacja LIKELY też to
   wyłapie przy save (output node z `input_port_type=Any`, ale producer tts adapter
   ma `output_port_type=Audio` więc edge data_type sugerowany Audio, GUI to wymusi).

---

## Workflow (jak Etap 1)

1. Plan szczegółowy → codex review (rundy iteratywne)
2. Iteracja planu aż codex passuje
3. **Implementacja** (jeden duży commit albo 2-3 mniejsze, decyzja przy implementacji)
4. Codex review codu (post-impl, fresh session via `/codex resume <id>`)
5. Iteracja codu aż codex passuje
6. Update CLAUDE.md o Etap 2 changes
7. Etap 3 zaczynamy od nowego planu

---

## Po zakończeniu Etapu 2

CLAUDE.md w sekcji "Flow engine":
- Tabela hard rules: + R8 (typed edge compatibility), + R9 (artifact registry — tylko deklaracje, validation Etap 3)
- Tabela node adapters: kolumna "Input/Output type" wypełniona
- Sekcja "Bypass paths": doprecyzowanie że TTS-as-flow działa (executor.rs:1539 nie
  zwraca już błędu)
- Sekcja "Co NIE robimy": lista przesunięta do Etap 3 (multi-modality, cardinality,
  CEL, frame-aware streaming, compiled flow persistence)
- Sekcja "Trailers": opis X-Want-Trailers + X-Tentaflow-* headerów dla
  non-streaming
- Sekcja "BlobStore": layout filesystem + GC schedule (jeśli wpięty)
