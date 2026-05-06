# Etap 2 — Typed porty + ArtifactKey + TTS-as-flow + trailers + FileBlobStore

**Plan v1.0 (do review codex)**
**Codex session ID:** `019dfca1-fef1-7ca1-b154-b73a796670a8` (kontynuacja po Etapie 1)
**Data:** 2026-05-06
**Bazuje na:** Etap 1 v4.2 (zamknięty, commits do `400baa0`)

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
    /// (Etap 3) i w testach. Empty → Any (puste pole pasuje do każdego typu
    /// jako "no value yet").
    pub fn from_value(v: &crate::flow_engine::envelope::FlowValue) -> Self {
        use crate::flow_engine::envelope::FlowValue;
        match v {
            FlowValue::Empty => FlowDataType::Any,
            FlowValue::Text(_) => FlowDataType::Text,
            FlowValue::Json(_) => FlowDataType::Json,
            FlowValue::Audio { .. } => FlowDataType::Audio,
            FlowValue::Image { .. } => FlowDataType::Image,
            FlowValue::Video { .. } => FlowDataType::Video,
            FlowValue::Embedding(_) => FlowDataType::Embedding,
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

```rust
// W pętli edges, po istniejących sprawdzeniach R3 (port membership):
let from_type = from_adapter.output_port_type(&edge.from_port);
let to_type = to_adapter.input_port_type(&edge.to_port);

// Edge.data_type compatibility:
// - edge.Any (legacy domyślne) → akceptujemy każdy from_type/to_type
// - edge konkretne → musi być compatible z producent i konsument
// - producent/konsument Any → wildcard (przepuszczamy)
if !edge.data_type.compatible_with(from_type) {
    return Err(FlowValidationError::EdgeTypeMismatch {
        edge_id: edge.id.clone().unwrap_or_else(|| format!("{}->{}", edge.from, edge.to)),
        side: "from",
        edge_type: edge.data_type,
        port_type: from_type,
    });
}
if !edge.data_type.compatible_with(to_type) {
    return Err(FlowValidationError::EdgeTypeMismatch {
        edge_id: edge.id.clone().unwrap_or_else(|| format!("{}->{}", edge.from, edge.to)),
        side: "to",
        edge_type: edge.data_type,
        port_type: to_type,
    });
}

// Dodatkowo: producent vs konsument muszą być spójne ze sobą gdy oba są
// konkretne (oba Any to OK, bo edge.data_type pełni rolę pomostu).
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
```

`FlowValidationError` rozszerza się o 2 warianty: `EdgeTypeMismatch` (edge declared
nie pasuje do portu) i `EdgePortTypesMismatch` (producent vs konsument różne
konkretne typy bez edge.data_type bridge).

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

### Helpers

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

### Layout

```
<TENTAFLOW_HOME>/blobs/<sha2[0:2]>/<sha2[2:4]>/<full_sha2>.bin
```

`<TENTAFLOW_HOME>` = env `TENTAFLOW_HOME` lub `~/.tentaflow` jako default. Bootstrap
przekazuje `Arc<dyn BlobStore>` do `FlowDispatcher::new`.

### Operacje

```rust
pub struct FileBlobStore {
    root: PathBuf,
}

impl FileBlobStore {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    fn path_for(&self, sha: &str) -> PathBuf {
        self.root
            .join(&sha[0..2])
            .join(&sha[2..4])
            .join(format!("{sha}.bin"))
    }
}

#[async_trait]
impl BlobStore for FileBlobStore {
    async fn put(&self, bytes: Vec<u8>, mime: &str) -> Result<BlobRef> {
        let id = uuid::Uuid::new_v4().to_string();
        let sha = sha256_hex(&bytes);
        let path = self.path_for(&sha);
        let dir = path.parent().expect("path has parent");
        let root = self.root.clone();
        let bytes_clone = bytes.clone();
        let path_clone = path.clone();
        let dir_clone = dir.to_path_buf();
        // Atomic write: temp w samym dir + rename. Brak race nawet przy
        // współbieżnym put tego samego sha (rename nadpisuje, content
        // identyczny dla tego samego sha).
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            std::fs::create_dir_all(&dir_clone)?;
            let temp = dir_clone.join(format!("{id}.tmp", id = id_alpha()));
            std::fs::write(&temp, &bytes_clone)?;
            std::fs::rename(&temp, &path_clone)?;
            Ok(())
        })
        .await??;
        Ok(BlobRef {
            id,
            size_bytes: bytes.len() as u64,
            mime: mime.to_string(),
            sha256: sha,
        })
    }

    async fn get(&self, blob_ref: &BlobRef) -> Result<Vec<u8>> {
        let path = self.path_for(&blob_ref.sha256);
        let bytes = tokio::task::spawn_blocking(move || std::fs::read(&path)).await??;
        Ok(bytes)
    }

    async fn delete(&self, blob_ref: &BlobRef) -> Result<()> {
        let path = self.path_for(&blob_ref.sha256);
        tokio::task::spawn_blocking(move || {
            if path.exists() {
                std::fs::remove_file(&path)?;
            }
            Ok::<_, std::io::Error>(())
        })
        .await??;
        Ok(())
    }

    async fn gc(&self, retention: Duration) -> Result<u64> {
        let root = self.root.clone();
        let cutoff = std::time::SystemTime::now() - retention;
        tokio::task::spawn_blocking(move || -> std::io::Result<u64> {
            let mut removed = 0u64;
            for shard in std::fs::read_dir(&root)?.flatten() {
                if !shard.file_type()?.is_dir() { continue; }
                for sub in std::fs::read_dir(shard.path())?.flatten() {
                    if !sub.file_type()?.is_dir() { continue; }
                    for blob in std::fs::read_dir(sub.path())?.flatten() {
                        let meta = blob.metadata()?;
                        if let Ok(modified) = meta.modified() {
                            if modified < cutoff {
                                std::fs::remove_file(blob.path())?;
                                removed += 1;
                            }
                        }
                    }
                }
            }
            Ok(removed)
        })
        .await
        .map_err(|e| anyhow::anyhow!("blob gc join: {e}"))??;
        Ok(0)
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}
```

`gc` zostaje proste (no concurrent put detection, no in-progress delete) — jeden
proces TentaFlow trzyma BlobStore, GC odpalany przez scheduler raz na N minut z
retention = 24h. Jeśli race z concurrent put zdarzy się, get-after-delete dostanie
NotFound i caller (TTS adapter) potraktuje jako transient error.

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

`Cargo.toml`: `sha2 = "0.10"` i `hex = "0.4"` jeśli nie ma. Sprawdzić — `sha2` jest
prawdopodobnie już used elsewhere, `hex` często też.

---

## Call site refactor map

| Plik | Akcja | LOC |
|------|-------|-----|
| `flow_engine/types.rs` | + `FlowDataType` enum, `FlowEdge.data_type` field | +80 |
| `flow_engine/node_adapter.rs` | + 4 default trait methods | +30 |
| `flow_engine/node_adapters/*.rs` | per-adapter override (12 adapterów × ~10 linii) | +120 |
| `flow_engine/validation.rs` | R8 + 2 nowe warianty błędów + testy | +180 |
| `flow_engine/blob_store.rs` | + `FileBlobStore` impl + 4 testy | +280 |
| `flow_engine/dispatcher.rs` | przyjmuje `blobs` parameter, `pub fn blobs()` | +20 |
| `flow_engine/converter.rs` | + `flow_outcome_to_tts_result` | +50 |
| `services/runtime/executor.rs` | TTS Flow arm, `tts_request_to_initial_envelope` | +80 |
| `routing/middleware.rs` | + `usage`/`finish_reason` w `RouteMetadata` | +30 |
| `routing/chat.rs` | populate metadata.usage/finish_reason | +30 |
| `api/openai/server.rs` | want-trailers parsing + header emit | +50 |
| `routing/router.rs` | wire `FileBlobStore` → `FlowDispatcher::new` | +30 |
| `Cargo.toml` | `sha2` / `hex` deps (jeśli brak) | +2 |

**Razem: ~980 LOC.** Mieści się w jednej sesji.

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
