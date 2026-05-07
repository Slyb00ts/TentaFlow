# Etap 3d — All-middleware → flow nodes (plan v1.5)

## Diff vs v1.4 (codex round 7)

- **[ACL deny semantics]** ACL `allow=false` na user-defined flow →
  `Err(DispatchError::Denied)` → 404 model_not_found. **Nie**
  synthetic fallback — synthetic aktywuje się TYLKO gdy resolver
  zwrócił `None` (= no flow attached for model). ACL deny ≠ no flow.
- **[Brak flow → synthetic]** stara sekcja "R-SAFETY fail-closed"
  zaktualizowana — `Ok(None)` znika z `try_dispatch*`. Resolver `None`
  → synthetic (zawsze `Ok(Some)`). Validation error → `Err(Compile)` → 500.
- **[Wrapper outcome]** wrapper sync→stream w `try_dispatch_streaming`
  zasila `StreamingExecution.outcome` przez resolved oneshot channel.

## Diff vs v1.3 (codex round 6)

- **[P1#1]** Konkretne callsite cuts (8 lokalizacji), w tym
  protocol-native branch w stt/embeddings.
- **[P1#3]** `validate(def, source: ValidationSource)` **mandatoryjny**
  param, no default. Wszystkie callery uaktualnione w tym samym kroku.
- **[P1#4]** `try_dispatch_streaming` zwraca zawsze `Ok(Some)` —
  blocking-only user flow → wrapper sync→stream wewnątrz dispatchera.
  No user flow → synthetic streaming.
- **[P1#5]** D4 invariant relaxed: `SttDispatcherImpl` woła
  `executor.execute_stt` (parity z LLM/TTS/Embeddings). Single source
  of truth.
- **Mesh inbound bypass udokumentowany jako wyjątek**: peer mesh forward
  to remote backend call analogiczny do HTTP/QUIC service, flow żyje
  po stronie inicjatora. **Trzy** mesh inbound paths zostają direct
  executor — z explicit komentarzami `// EXEMPT-MESH-INBOUND`:
  `mesh/inference_proxy.rs:190` (chat), `routing/stt.rs:301`
  (protocol-native STT), `routing/embeddings.rs:222` (route_embeddings_via_quic).
  Wszystkie 3 są wywoływane **tylko** z mesh reverse path
  (`route_audio_via_protocol`, `route_embeddings_via_quic`,
  `inference_proxy::handle_inference_request`), nigdy z HTTP entrypoint.
- **[P2#6]** `FlowCache::synthetic` z LRU eviction (`max_synthetic_entries`,
  default 256). `set()` reapuje LRU gdy cap.

## Mesh exception (udokumentowane)

Mesh to peer-to-peer remote backend call, **nie** distributed flow.
Architektura:

- Node A trzyma user-defined flow. Flow ma `llm` node z `config.target =
  "mesh:peer-B"`.
- Capability dispatcher (`LlmDispatcherImpl::execute_chat`) wewnątrz
  Node A widzi target=mesh, woła QUIC client, serializuje request,
  wysyła do peer B.
- Peer B odbiera w `mesh/inference_proxy.rs::handle_inference_request`
  → wykonuje **direct** `executor.execute_chat` (bez flow_engine na
  swoim node'cie). Zwraca rkyv response do Node A.
- Node A kontynuuje swoje flow (kolejne nody).

**Rationale:** flow żyje po stronie inicjatora. Peer to backend, nie
re-entry punkt do flow_engine. Wymóg ultra-low latency dla mesh ops
(local LAN, 1-5ms baseline) — full flow_engine overhead na peer'ze
psuje budżet latencji.

Trzy mesh inbound callsites zachowane z komentarzami `EXEMPT-MESH-INBOUND`:

| Plik:linia | Wywoływane z | Service |
|---|---|---|
| `mesh/inference_proxy.rs:190` | `inference_proxy::handle_inference_request` | chat |
| `routing/stt.rs:301` | `Router::route_audio_via_protocol` | STT |
| `routing/embeddings.rs:222` | `Router::route_embeddings_via_quic` | embeddings |

```rust
// EXEMPT-MESH-INBOUND: direct executor call jest celowy.
// Flow żyje na inicjatorze; peer = remote backend (jak HTTP service).
// Dla "all-through-flow" exception: mesh peer-to-peer ultra-low-latency.
```

To są **jedyne** udokumentowane wyjątki w repo. Wspólny mianownik:
wszystkie 3 są wywoływane **tylko** z mesh reverse path (peer forwarduje
rkyv request przez QUIC), **nigdy** z HTTP entrypoint /v1/*. Każdy inny
direct executor call kasujemy.

## Diff vs v1.2 (user directive: WSZYSTKO przez flow)

User: "obojetnie czy chat, teams, SDK, OpenAI API — ZAWSZE przez flow,
nawet jak będzie miało tylko input → llm → output. Jedno źródło prawdy."

### Universal Flow Gateway

Dziś `routing/{chat,streaming,tts,stt,embeddings}.rs` ma **dual path**:
1. Najpierw `dispatcher.try_dispatch` (jeśli admin skonfigurował flow
   dla modelu w DB).
2. Fallback do `executor.execute_*` direct gdy resolver zwróci `None`.

Plan v1.3 likwiduje ten dual path. **Każdy** request `/v1/*` (lokalny i
przez OpenAI compat) przechodzi przez `FlowDispatcher` z gwarancją
że istnieje skompilowany flow do dispatchu — albo user-defined (DB),
albo **synthetic ad-hoc** (in-memory, single-block per kind requestu).

### Synthetic ad-hoc flow

Gdy `resolver::resolve_flow(model, kind)` zwróci `None`, runtime
buduje minimalny `FlowDefinition` per request kind:

| Kind | Synthetic flow definition |
|---|---|
| chat | `trigger → llm(model=<from_request>) → output(mode=blocking\|stream, final_kind=llm)` |
| tts | `trigger → tts(model=<from_request>) → output(mode=blocking, final_kind=audio)` |
| stt | `trigger → stt(model=<from_request>) → output(mode=blocking, final_kind=text)` |
| embeddings | `trigger → embeddings(model=<from_request>) → output(mode=blocking)` |

Synthetic flow **nie idzie do DB** — żyje wyłącznie w runtime.
Compiled raz, cache'owany w `FlowCache` pod kluczem
`__synthetic__:<kind>:<model_hash>`. Następne requesty z tym samym
modelem trafiają w cache.

### R-SAFETY i synthetic flows

R-SAFETY (mandatoryjny `pii_filter` na chain) dotyczy **wyłącznie
user-defined flows** (DB-persistent). Synthetic flows są zwolnione —
admin który nie skonfigurował flow akceptuje raw output. Decyzja:
brak flow = brak ochrony (jednoznaczne, log warn na load-time).

Jeśli admin chce PII na danym modelu — definiuje flow w DB z
`pii_filter`. Synthetic to fallback minimalny.

### Kasujemy bypass paths (8 callsites)

| # | Plik:linia | Co wycinamy |
|---|---|---|
| 1 | `routing/chat.rs:187` | DEL `else { executor.execute_chat }` branch. Cała chat path leci wyłącznie przez `dispatcher.try_dispatch`. |
| 2 | `routing/streaming.rs:739–740` | DEL fallback do blocking `try_dispatch` po stream None. `try_dispatch_streaming` ma wewnętrzny wrapper sync→stream (sekcja niżej). |
| 3 | `routing/tts.rs:102` | DEL `executor.execute_tts` direct. `synthesize_speech` woła `dispatcher.try_dispatch(model, "tts", ...)`. |
| 4 | `routing/stt.rs:56` | DEL pierwszy `executor.execute_stt` direct (default path). |
| 5 | `routing/stt.rs:249` | DEL drugi `executor.execute_stt` direct (protocol-native AudioOperation::STT branch). |
| 6 | `routing/embeddings.rs:71` | DEL `executor.execute_embeddings` direct (default path). |
| 7 | `routing/embeddings.rs:161` | DEL drugi `executor.execute_embeddings` direct (protocol-native branch). |
| 8 | `flow_engine/dispatchers_impl/stt_impl.rs:30` | REFACTOR: `SttDispatcherImpl::transcribe` woła `executor.execute_stt` zamiast `SttRuntime::transcribe` direct. D4 invariant relaxed (rationale w sekcji niżej). |

**Mesh inbound paths NIE wycinamy** (3 callsites: `mesh/inference_proxy.rs:190`,
`routing/stt.rs:301`, `routing/embeddings.rs:222`) — zostają z komentarzami
`EXEMPT-MESH-INBOUND` (sekcja Mesh exception powyżej).

Po cięciach: `routing/*` ma **jedyne** wywołanie do `flow_engine` —
brak direct executor calls. `ModelRuntimeExecutor::execute_*` wołane
wyłącznie z capability dispatcher impls + mesh inbound exception.

### D4 invariant relaxed

Stary D4 (plan v4.2): "capability dispatcher impl nie ciąga
ServiceManager → dispatcher omija executor.rs::execute_stt".

Nowy D4 (3d v1.4): "capability dispatcher impl woła
`executor.execute_*` jak każdy inny consumer. Executor jest cienkim
routerem do backendu (SttRuntime/ModelManager/etc), dispatcher jest
cienkim wrapperem nad executorem."

Powód: single source of truth wymaga że każdy chat/tts/stt/embeddings
backend call lecaj **tym samym** kanałem — `executor.execute_*`. Inaczej
ścieżki dispatcherów rozjeżdżają się (LLM/TTS/Embeddings woła executor,
STT bypassuje). Po cięciach każdy capability dispatcher impl ma
identyczną architekturę:

```
NodeAdapter::execute → ctx.{llm|tts|stt|embeddings}.{execute_*|stream_*}
    → CapabilityDispatcherImpl
    → executor.execute_*
    → backend (Embedded/HTTP/QUIC/Mesh)
```

### try_dispatch_streaming — wrapper logic + ACL semantics

`FlowDispatcher::try_dispatch_streaming(model, kind, ...)` zwraca:
- `Ok(StreamingExecution { stream, outcome })` — zawsze gdy flow ma
  być wykonany (user-defined lub synthetic).
- `Err(DispatchError::Denied)` — ACL `allow=false` dla user-defined.
- `Err(DispatchError::Compile(e))` — validation failure.
- `Err(DispatchError::Internal(e))` — runtime błąd.

**Decision tree:**

```
1. resolver::resolve_flow(model, kind) →
   - Some(flow) → check ACL:
     - allow=true → kompiluj user-defined, dispatch, return Ok.
     - allow=false → Err(Denied) [→ caller 404 model_not_found].
   - None (no flow attached) → kompiluj synthetic, dispatch, return Ok.
2. compile failure → Err(Compile) [→ caller 500].
3. runtime failure → Err(Internal) [→ caller 500].
```

**Synthetic NIE aktywuje się dla ACL deny.** ACL deny = "ten user nie
ma dostępu do tego modelu" — szanujemy decyzję admina, zwracamy 404.
Synthetic to fallback **tylko** gdy admin nie skonfigurował flow.

**Wrapper sync→stream dla blocking-only user flow:**

User-defined flow bez streaming end-shape (R7 chain). Dispatcher
wykonuje blocking, wrapuje outcome w stream:

```rust
async fn try_dispatch_streaming(...) -> Result<StreamingExecution> {
    if user_flow.is_blocking_only() {
        let outcome: FlowExecutionOutcome = dispatch_blocking(...).await?;
        let payload_text = outcome.payload.as_text().unwrap_or_default();
        let usage = outcome.usage.clone();
        let finish = outcome.finish_reason.clone();
        let chunk = LlmStreamChunk {
            choice_index: 0,
            text_delta: payload_text,
            usage: Some(usage),
            finish_reason: Some(finish),
            ..Default::default()
        };
        let stream = futures::stream::once(async move {
            Ok(EnvelopeDelta::Llm(chunk))
        }).boxed();
        let (outcome_tx, outcome_rx) = oneshot::channel();
        outcome_tx.send(outcome).ok();  // resolved natychmiast
        Ok(StreamingExecution { stream, outcome: outcome_rx })
    }
}
```

Caller (`routing/streaming.rs`) konsumuje `StreamingExecution` jednolicie
— nie wie czy stream jest natywny czy wrapped. Outcome rx rozwiązuje
się natychmiast dla wrapped, po EOF dla native streaming.

`try_dispatch` (blocking) analogicznie zwraca `Ok(outcome)` lub `Err`.
Brak `Ok(None)` w obu API.

### Nowe komponenty (Krok 0)

1. `flow_engine/synthetic.rs` (nowy plik):
   ```rust
   pub fn synthetic_chat(model: String) -> FlowDefinition { ... }
   pub fn synthetic_tts(model: String) -> FlowDefinition { ... }
   pub fn synthetic_stt(model: String) -> FlowDefinition { ... }
   pub fn synthetic_embeddings(model: String) -> FlowDefinition { ... }
   ```
   Każda funkcja konstruuje `FlowDefinition` z 3 nodes (trigger →
   capability → output) + 2 edges. Model wstawiany w `node.config["model"]`.

2. `flow_engine/cache.rs::FlowCache` — drugi slot z LRU eviction:
   ```rust
   pub struct FlowCache {
       compiled: HashMap<String, Arc<CompiledFlow>>,          // user-defined (flow_id key)
       synthetic: SyntheticSlot,                              // ad-hoc, LRU-bounded
   }

   pub struct SyntheticSlot {
       entries: HashMap<String, Arc<CompiledFlow>>,           // "kind:model_hash" key
       lru_order: VecDeque<String>,                           // access order, oldest first
       max_size: usize,                                       // default 256
   }

   impl SyntheticSlot {
       pub fn set(&mut self, key: String, flow: Arc<CompiledFlow>) {
           if self.entries.len() >= self.max_size {
               if let Some(oldest) = self.lru_order.pop_front() {
                   self.entries.remove(&oldest);
               }
           }
           self.entries.insert(key.clone(), flow);
           self.lru_order.push_back(key);
       }
       pub fn get(&mut self, key: &str) -> Option<Arc<CompiledFlow>> {
           // Hit → przesuń na koniec lru_order
       }
   }
   ```
   `max_size` default 256 (config przez `[flow_engine] synthetic_cache_size`).
   OOM mitigation: każdy unikalny model w produkcji to nowy entry,
   bez capu pamięć rośnie liniowo z liczbą deployowanych modeli.

3. `flow_engine/dispatcher.rs::FlowDispatcher`:
   - `try_dispatch(model, kind, ...)` zmiana: gdy `resolve_flow` →
     `None`, zbuduj synthetic, compile (przez R-SAFETY exempt path),
     cache, dispatch. **Nigdy nie zwraca `Ok(None)`.**
   - `Ok(None)` znika z surface'u — sygnalizacja "no flow" jest
     niemożliwa, bo każdy model ma synthetic fallback.

### Update R-SAFETY validation

`flow_engine/validation.rs::ValidationSource` enum (mandatoryjny param,
no default Option):

```rust
pub enum ValidationSource {
    UserDefined,  // flow z DB (save path, seed, dispatch resolver hit)
    Synthetic,    // ad-hoc compile (FlowDispatcher synthetic fallback)
}
```

`validate(def, source)` zmiana z `validate(def)` — wszystkie callery
**muszą** podać source. Brak defaultu = compile error gdy ktoś zapomni.

Callsite update (3 callery):
- `flow_engine/cache.rs::compile(def, registry)` → przyjmuje
  `source: ValidationSource` jako trzeci param. Forward do `validate`.
- `dispatch/handlers.rs:118` (admin save flow path) →
  `compile(def, registry, ValidationSource::UserDefined)`.
- `db/seed.rs:851` (seed install path) →
  `compile(def, registry, ValidationSource::UserDefined)`.
- `flow_engine/dispatcher.rs::FlowDispatcher::compile_synthetic` (nowy
  helper) → `compile(def, registry, ValidationSource::Synthetic)`.

R-SAFETY behavior:
- `Synthetic` → skip rule, accept. Synthetic ma jeden kind node + output,
  brak chain, R-SAFETY nieaplikowalny.
- `UserDefined` → enforce. Flow z `llm` source MUSI mieć `pii_filter`
  na chain (blocking lub streaming branch). Brak = compile error.

### Tasks update

Dodać między Krok 0 a Krok 1:

**Krok 0a: Universal flow gateway (synthetic)**

- 0a.1 `flow_engine/synthetic.rs` — 4 builder functions.
- 0a.2 `flow_engine/cache.rs::FlowCache` — drugi slot synthetic, accessor.
- 0a.3 `flow_engine/dispatcher.rs::FlowDispatcher::try_dispatch` —
  resolver None → synthetic fallback. Same dla `try_dispatch_streaming`.
- 0a.4 `flow_engine/validation.rs::ValidationSource` enum +
  `validate(def, source)` przyjmuje. R-SAFETY skip dla Synthetic.

**Krok 0b: Demolish bypass paths w routing**

- 0b.1 `routing/chat.rs` — DEL `else { executor.execute_chat }` branch.
  Cała chat path leci wyłącznie przez `dispatcher.try_dispatch`.
- 0b.2 `routing/streaming.rs` — DEL fallback po `try_dispatch_streaming`
  zwracający None. Streaming dispatcher też synthetic-aware.
- 0b.3 `routing/tts.rs::synthesize_speech` — DEL `executor.execute_tts`
  direct. Replace z `dispatcher.try_dispatch(model, "tts", ...)`.
- 0b.4 `routing/stt.rs::transcribe_audio` — DEL executor direct.
  Replace z dispatcher.
- 0b.5 `routing/embeddings.rs` — DEL executor direct. Replace z
  dispatcher.

Tests:
- `synthetic::tests::chat_synthetic_compiles` — model X bez DB flow →
  synthetic flow dispatchuje OK.
- `synthetic::tests::tts_synthetic_uses_request_model` — synthetic_tts
  ma `node.config["model"]` z requestu.
- `synthetic::tests::cache_hit_on_repeat_model` — drugi request z tym
  samym modelem nie kompiluje synthetic ponownie.
- `validation::tests::r_safety_skipped_for_synthetic` — synthetic chat
  bez `pii_filter` przechodzi compile.
- `routing::chat::tests::no_executor_direct_call` — usunięcie fallbacku.

## Diff vs v1.1 (codex round 2)

- **[P1#1]** Konkretny diff `LlmStreamChunk`: dodajemy tylko pole
  `choice_index: u32` (default 0). Pozostałe pola (`text_delta`,
  `reasoning_delta`, `tool_calls`, `usage`, `finish_reason`, `error`)
  zachowane. Dispatcher impl + bridge propagują wartość zamiast 0.
- **[P1#2]** Generic `register_streaming<T>` z bound `T: NodeAdapter +
  StreamingNodeAdapter + 'static` — `Arc<T>` koersuje do
  `Arc<dyn NodeAdapter>` i `Arc<dyn StreamingNodeAdapter>` osobno
  podczas insert do typed map. Brak upcasting issue.
- **[P1#3]** Port semantics konkretne: streaming nodes mają
  `outputs=["full","stream"]`. `output` zostaje passthrough z
  `inputs=["in"]`. Edge `from_port="stream"` consumera celuje w
  `to_port="in"`. `output.config.final_kind` to hint dla HTTP bridge
  (Llm/Audio), nie zmiana portów.
- **[NEW R-SAFETY fail-closed]** Validation error w `CompiledFlow::compile`
  (source=UserDefined) zwraca `Err(DispatchError::Compile)` propagowane do
  dispatcher → caller 500. Resolver `None` (no flow attached) → synthetic
  fallback (v1.3+, source=Synthetic, R-SAFETY skip). ACL deny →
  `Err(DispatchError::Denied)` → 404. Pełny decision tree w sekcji
  "try_dispatch_streaming — wrapper logic + ACL semantics".
- **[NEW Llm→Audio chain shape]** Stream typ między nodami w chain to
  **envelope-level** `BoxStream<Result<EnvelopeDelta>>`. `LlmDispatcher`
  zostaje LLM-specific (zwraca `BoxStream<Result<LlmStreamChunk>>` jak
  teraz); **executor** mapuje `LlmStreamChunk` → `EnvelopeDelta::Llm`
  na wejściu do fold chain'a (single point of conversion). Fold chain'a
  operuje na envelope-level.
  Executor finalizer rozdwojony: `final_kind=Llm` (text+reasoning
  aggregate, usage), `final_kind=Audio` (last `FlowValue::Audio(BlobRef)`).
  Bridge HTTP `routing/streaming.rs` i nowy `routing/audio_stream.rs`
  konsumują `BoxStream<EnvelopeDelta>` per kind.



Naprawa parallel install. **Cała** funkcjonalność PII + TTS sentence
batching żyje wyłącznie w flow_engine jako node'y. Cztery równoległe
stosy poza flow_engine kasujemy w jednym etapie.

User directive: "WSZYSTKO przez flow, tylko bloczki obsługują wszystko".

## Diff vs v1.0 (codex round 1, archiwalne)

- **[P1#1]** `LlmStreamChunk.choice_index: u32` (default 0) wpięte w
  schemat delta + propagacja w dispatcher impl + bridge.
- **[P1#2]** `StreamingNodeAdapter` żyje w **osobnym slocie rejestru**
  (HashMap), nie zastępuje `NodeAdapter`. Adapter rejestruje się dwukrotnie.
- **[P1#3]** Konkretne zmiany w `CompiledFlow`, `output` portach,
  validator. Stary `is_streaming: bool` → `streaming: Option<StreamingChain>`.
- **[P1#4]** Nowy endpoint `/v1/audio/speech/flow-stream` — audio sink
  dla `tts_stream_bridge` w final_kind=Audio. Chat-stream-bridge dalej
  zabrania `EnvelopeDelta::Audio` (text-only contract).
- **[P1#5]** Built-in detektory PII w kodzie adaptera (zachowanie
  parity z `sanitize_pii`), DB rules są opcjonalne user-extension.
  Brak nowej kolumny `built_in`.
- **[P1#6]** Hard validation rule **R-SAFETY**: flow z `llm` source musi
  mieć `pii_filter` w chain. + wszystkie seedy update.
- **[P1#7]** Pełna inventory cięć: dodano `streaming.rs:758,1006`,
  testy modułów, `MiddlewareConfig::response_filtering_enabled`.
- **[P1#8]** `tts_stream_bridge` używa `ctx.tts_cleaning` (istniejący
  store), nie nowy private helper. `clean_text_for_tts` przepisana do
  cleaning store impl gdy migrujemy reguły.
- **[P2#1]** Cancel-on-drop: explicit `cancel.is_cancelled()` check
  przed każdym blocking `await synthesize` / `await put`.
- **[P2#2]** `MiddlewareConfig::response_filtering_enabled` skasowany
  z `config/mod.rs`. config.toml dokumentacja update.

## Inwentaryzacja parallel install (do wycięcia w 3d)

### Pliki / moduły

| Ścieżka | Linie | Status |
|---|---|---|
| `tentaflow-core/src/middleware/pii.rs` | 305 | DEL całość |
| `tentaflow-core/src/middleware/response.rs` | 339 | DEL całość |
| `tentaflow-core/src/middleware/mod.rs` | ~62 | DEL całość |
| `tentaflow-core/src/services/runtime/middleware.rs` | ~700 | DEL całość |
| `tentaflow-core/src/services/tts/processor.rs::TTSBufferingProcessor` | ~150 (linie 207–356 fragmentu) | DEL fragment |

### Callsites do zmiany

| Plik | Co |
|---|---|
| `tentaflow-core/src/lib.rs` | DEL `pub mod middleware` |
| `tentaflow-core/src/routing/router.rs:12,35,291,355` | DEL import + field + init + getter |
| `tentaflow-core/src/routing/chat.rs:142–144,190–192,261–296,402` | DEL `apply_response_middleware` + 3 callsites |
| `tentaflow-core/src/routing/streaming.rs:22,377–522,387,396,401,694,757–759,759,862–864,1006–1024` | DEL `wrap_with_pii_streaming`, `response_middleware` field access, test `pii_filter_bypasses_tail_chunk`, blocking-flow-as-stream `clean_text` callsite (758), test imports (1009) |
| `tentaflow-core/src/services/runtime/executor.rs:32,131,147,996` | DEL `StreamMiddlewareFactory` field + new() param + getter, `apply_stack`/`open_session_stack` użycia |
| `tentaflow-core/src/services/runtime/mod.rs:22` | DEL re-export |
| `tentaflow-core/src/services/tts/mod.rs:11` | DEL re-export `TTSBufferingProcessor`, `SynthesizeCallback` |
| `tentaflow-core/src/services/mod.rs:31` | DEL re-export |
| `tentaflow-core/src/config/mod.rs:344–352` | DEL `MiddlewareConfig::response_filtering_enabled` (jeśli `request_validation_enabled` też deadem zostaje — sprawdzić; jeśli tak, DEL całe `MiddlewareConfig`) |

Po cięciach: `cargo build` + `cargo test --lib`. Zero referencji do
usuniętych typów.

## Flow engine — co dodać

### Streaming delta protocol

Obecny `flow_engine/envelope.rs:364`:
```rust
pub struct LlmStreamChunk {
    pub text_delta: String,
    pub reasoning_delta: Option<String>,
    pub tool_calls: Vec<ToolCallDelta>,
    pub usage: Option<TokenUsage>,
    pub finish_reason: Option<FinishReason>,
    pub error: Option<String>,
}
```

**Dodajemy jedno pole** (default 0):
```rust
pub struct LlmStreamChunk {
    pub choice_index: u32,                // NEW
    pub text_delta: String,
    pub reasoning_delta: Option<String>,
    pub tool_calls: Vec<ToolCallDelta>,
    pub usage: Option<TokenUsage>,
    pub finish_reason: Option<FinishReason>,
    pub error: Option<String>,
}
```

Propagacja:
- `LlmDispatcherImpl::stream_chat` (`flow_engine/dispatchers_impl/llm_impl.rs:321`)
  ustawia `choice_index = chunk.choices[i].index` z OpenAI response.
- `envelope_stream_to_chunk_stream` (`routing/streaming.rs:181`) używa
  `chunk.choice_index` zamiast hardcoded `0`.
- `executor.rs:303` finalizer agreguje `usage` po `finish_reason.is_some()` —
  niezmieniony, ale jeśli n>1, sumujemy per choice (out-of-scope w v1).

W v1 backend dispatcher zawsze setuje `choice_index=0` (n=1). Schemat
gotowy na multi-choice w przyszłości.

### Stream typ od źródła — envelope-level

**Decyzja architektoniczna**: stream typ pomiędzy nodami w chain to
`BoxStream<Result<EnvelopeDelta>>`, a nie `BoxStream<Result<LlmStreamChunk>>`.

Powód: `tts_stream_bridge` produkuje `EnvelopeDelta::Audio`, więc po
fold chain stream może być mieszany typ (Llm + Audio). Ujednolicamy
do `EnvelopeDelta` od razu na źródle.

**Zmiany w `executor.rs::execute_streaming`** (linia 227):
- LLM dispatcher zwraca `BoxStream<Result<LlmStreamChunk>>` jak teraz.
- Executor mapuje `LlmStreamChunk` → `EnvelopeDelta::Llm(chunk)` przed
  pierwszym fold step'em chain'a.
- Każdy `StreamingNodeAdapter::process_stream(upstream, ...)` operuje
  na `BoxStream<Result<EnvelopeDelta>>` — dostaje envelope deltami,
  zwraca envelope deltami (potencjalnie innym kindem).
- Final stream = ostatni `process_stream` output, też envelope-level.

**Finalizer `finalize_streaming_flow`** (linia 290, 351) — przepisany:
```rust
match streaming.final_kind {
    EnvelopeDeltaKind::Llm => finalize_llm_outcome(...),    // text aggregate
    EnvelopeDeltaKind::Audio => finalize_audio_outcome(...), // BlobRef collect
}
```

`finalize_llm_outcome`: agreguje `text_delta` + `reasoning_delta` per
choice, sumuje `usage`, mapuje `finish_reason` ostatniego chunk'a do
`FlowExecutionOutcome { payload: FlowValue::Text(...), usage, ... }`.

`finalize_audio_outcome`: zbiera kolejne `EnvelopeDelta::Audio(chunk)` —
ostatni chunk z `finish_reason=Some(Stop)` daje końcowy `payload =
FlowValue::Audio { blob_ref: <last>, ... }`. Pośrednie `BlobRef`-y
trzymane w `artifacts["audio_chunks"]` jako lista (do wyboru klienta
gdy chce surowe ramki, nie tylko końcowe audio).

### Bridge HTTP — dwa odbiorcy envelope stream

`routing/streaming.rs::envelope_stream_to_chunk_stream` (linia 177):
- Konsumuje `BoxStream<Result<EnvelopeDelta>>`.
- Filtruje **tylko** `EnvelopeDelta::Llm` → mapuje na
  `ChatCompletionChunk` z `choice_index` propagowanym.
- `EnvelopeDelta::Audio` → emit `Err(InternalError("audio in chat stream — flow misconfigured"))`
  (dziś już jest na linii 83/129, zachowujemy).

`routing/audio_stream.rs::envelope_stream_to_audio_chunks` (nowy plik):
- Konsumuje `BoxStream<Result<EnvelopeDelta>>`.
- Filtruje **tylko** `EnvelopeDelta::Audio` → SSE base64 frame jak 3c.
- `EnvelopeDelta::Llm` → ignore (text deltas nie są emitted, audio-only
  endpoint) lub Err — decyzja: ignore z trace log (nie psuje stream'u
  jeśli flow został źle zwalidowany).

### EnvelopeDeltaKind enum

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeDeltaKind { Llm, Audio }

impl EnvelopeDelta {
    pub fn kind(&self) -> EnvelopeDeltaKind {
        match self {
            EnvelopeDelta::Llm(_) => EnvelopeDeltaKind::Llm,
            EnvelopeDelta::Audio(_) => EnvelopeDeltaKind::Audio,
        }
    }
}
```

`Transcript` variant nie istnieje (3d-streaming-STT wycięty wcześniej).

### StreamingNodeAdapter trait — osobny slot rejestru

`flow_engine/node_adapter.rs`:

```rust
#[async_trait]
pub trait StreamingNodeAdapter: NodeAdapter {
    async fn process_stream(
        &self,
        node: &FlowNode,
        upstream: BoxStream<'static, Result<EnvelopeDelta>>,
        seed_envelope: Arc<FlowEnvelope>,
        ctx: &ExecutionContext,
    ) -> Result<BoxStream<'static, Result<EnvelopeDelta>>>;

    fn stream_input_kind(&self) -> EnvelopeDeltaKind;
    fn stream_output_kind(&self) -> EnvelopeDeltaKind;
}

pub struct AdapterRegistry {
    adapters: HashMap<&'static str, Arc<dyn NodeAdapter>>,
    streaming_adapters: HashMap<&'static str, Arc<dyn StreamingNodeAdapter>>,  // NEW
    llm_adapter: Option<Arc<dyn LlmAdapter>>,  // existing
}

impl AdapterRegistry {
    /// Generic register dla adaptera implementującego oba traity.
    /// `Arc<T>` koersuje do `Arc<dyn NodeAdapter>` i `Arc<dyn StreamingNodeAdapter>`
    /// **niezależnie** podczas insert do typed map. Brak runtime upcasting issue.
    pub fn register_streaming<T>(&mut self, adapter: Arc<T>)
    where
        T: NodeAdapter + StreamingNodeAdapter + 'static,
    {
        let key = adapter.node_type();
        let blocking: Arc<dyn NodeAdapter> = adapter.clone();
        let streaming: Arc<dyn StreamingNodeAdapter> = adapter;
        self.adapters.insert(key, blocking);
        self.streaming_adapters.insert(key, streaming);
    }
    pub fn streaming(&self, node_type: &str) -> Option<&Arc<dyn StreamingNodeAdapter>> {
        self.streaming_adapters.get(node_type)
    }
}
```

`pii_filter` i `tts_stream_bridge` rejestrują się przez `register_streaming`
(jeden Arc, dwa sloty). Executor `execute_streaming` woła
`registry.streaming(node_type)` żeby zbudować chain.

### CompiledFlow streaming chain

`flow_engine/cache.rs::CompiledFlow`:

```rust
pub struct StreamingChain {
    pub producer_run_idx: usize,           // index w execution_order, node_type=llm
    pub chain_run_idxs: Vec<usize>,        // intermediate streaming nodes (mogą być puste)
    pub sink_run_idx: usize,               // output node
    pub final_kind: EnvelopeDeltaKind,
}

pub struct CompiledFlow {
    pub flow_id: String,
    pub definition: FlowDefinition,
    pub execution_order: Vec<usize>,
    pub adjacency: HashMap<...>,
    pub streaming: Option<StreamingChain>,  // ZAMIAST is_streaming: bool
}
```

`compile()` wykrywa stream chain: znajduje LLM producer,
walks `from_port="stream"` edges aż do `output` z `mode=stream`,
sprawdza wszystkie intermediate node'y mają `StreamingNodeAdapter`,
sprawdza chain compatibility (producer.output_kind == next.input_kind, ...).

### Output node — audio variant

`flow_engine/node_adapters/output.rs::OutputNodeAdapter`:

Porty zostają **bez zmian**: `inputs=["in"]`, `outputs=["full"]`. Output
node jest passthrough sink, nie dodajemy nowego portu.

Config rozszerzamy o `final_kind` field (opcjonalny, default `llm`):
```jsonc
{ "type": "output", "config": { "mode": "stream", "final_kind": "llm" } }
{ "type": "output", "config": { "mode": "stream", "final_kind": "audio" } }
```

**Streaming nodes** (`pii_filter`, `tts_stream_bridge`) mają natomiast
`outputs=["full","stream"]`:
- `"full"` — blocking aggregated payload (wynik ich `execute()`).
- `"stream"` — streaming delty (wynik `process_stream()`).

Edge `from_port="stream"` consumera celuje w **standardowy port**
`to_port="in"` na konsumenicie streaming albo na `output(mode=stream)`.
Validator wie z konfiguracji czy konsument operuje na stream stream
(jeśli to `StreamingNodeAdapter` — process_stream'em; jeśli to
`output` — bridge HTTP).

**Walidacja `final_kind`**:
- `output.config.mode="stream"` → `final_kind` MUSI być `llm` lub `audio`.
- `final_kind` MUSI równać się `chain.last_node.stream_output_kind`.
  (np. chain `llm.stream → pii_filter.stream → output(stream, audio)`
  jest invalid: `pii_filter.stream_output_kind=Llm`, ale `output.final_kind=audio`.)
- Bridge HTTP wybiera handler na podstawie `final_kind`:
  - `llm` → `routing/streaming.rs::envelope_stream_to_chunk_stream` (text SSE)
  - `audio` → `routing/audio_stream.rs::envelope_stream_to_audio_chunks` (audio SSE)

### R-SAFETY fail-closed semantics

Po Universal Flow Gateway (v1.3+): `Ok(None)` **znika** z surface'u
`try_dispatch*`. Trzy ścieżki:

1. **Resolver `Some(flow)` + ACL allow** → kompiluj user-defined
   (source=UserDefined) → dispatch.
   - Compile failure (`MissingPiiFilter`, `R7 violation`, ...) →
     `Err(DispatchError::Compile(e))` → caller 500
     "flow validation failed: <e>". **Brak silent bypass** — nawet
     jeśli compile fails, synthetic NIE aktywuje (admin chciał
     konkretny flow, fix go).

2. **Resolver `Some(flow)` + ACL deny** → `Err(DispatchError::Denied)`
   → caller 404 model_not_found.

3. **Resolver `None`** → kompiluj synthetic
   (source=Synthetic, R-SAFETY skip) → dispatch. **Synthetic NIE
   ma `pii_filter`** — admin nie chciał flow, akceptuje raw output.

**R-SAFETY validator** (`flow_engine/validation.rs::validate(def, source)`):
- `source = UserDefined` → iteruj nodes, jeśli `node_type=="llm"`
  istnieje, DFS po adjacency od LLM do każdego `output`. Każda ścieżka
  MUSI przechodzić przez `pii_filter`. Brak = `Err(ValidationError::MissingPiiFilter)`.
- `source = Synthetic` → skip rule (synthetic ma trivial topology
  trigger→llm→output, brak chain'a, R-SAFETY nieaplikowalny).
- Compile-time (= flow_engine compile, nie Rust compile-time).

**Executor `FlowEmptyResult`** (`services/runtime/executor.rs:95,810`):
po cięciu bypass paths, executor jest wołany wyłącznie przez capability
dispatcher impls. `FlowEmptyResult` w tym kontekście to bug (synthetic
zawsze produkuje wynik) → `Err(ExecutorError::Internal)`, nie silent skip.

### R7/R8 update

`flow_engine/validation.rs`:

**R7** (przepisana): streaming end-shape =
- ≤1 edge `from_port="stream"` na producer LLM (już jest)
- target tego edge to `StreamingNodeAdapter` LUB `output` z `mode=stream`
- jeśli target to streaming node, walk dalej po `from_port="stream"` aż do `output(stream)`
- każdy intermediate node w chain musi być w `streaming_adapters` registry
- chain compatibility: `producer.stream_output_kind == consumer.stream_input_kind` na każdym edge'u
- sink `output(stream)` config `final_kind` musi pasować do `last_node.stream_output_kind`

**R8** niezmieniona — `data_type` na edge dalej Text/Audio/Empty wg portów.

**R-SAFETY** (nowa): jeśli flow ma `llm` jako producer, walidator
**wymaga** `pii_filter` na chain (blocking branch — `from_port="full"`
chain — albo streaming branch — `from_port="stream"` chain). Brak
`pii_filter` = compile error. Built-in flowy (seedy w DB) muszą
przejść walidację.

Hard rule. Brak compile = brak deploy.

### Adapter: pii_filter (rozszerzony)

`flow_engine/node_adapters/pii_filter.rs`:

**Blocking path (istnieje):**
- `inputs=["in"]`, `outputs=["full"]` (jak teraz).
- `execute()` aplikuje (1) built-in detectors (full_name, polish_surname_suffix, address heuristic, NIP/PESEL/email/phone z regex set) (2) DB rules przez `ctx.pii_rules.list()`.

**Streaming path (nowy):**
- Też `outputs=["stream"]`.
- `impl StreamingNodeAdapter`:
  - `stream_input_kind = Llm`, `stream_output_kind = Llm`.
  - `process_stream`:
    1. `let rules = ctx.pii_rules.list().await?` (raz, na początku).
    2. Per-choice buffer: `HashMap<u32, ChoiceState { text_buffer: String, reasoning_buffer: String, pending_finish: Option<FinishReason> }>`. (W v1 prawie zawsze `choice_index=0`.)
    3. Konsumuj `EnvelopeDelta::Llm(chunk)`:
       - Push `chunk.text_delta` do `text_buffer`, push `chunk.reasoning_delta` do `reasoning_buffer`.
       - Flush warunki dla content: ostatni char ∈ `.!?…;\n` LUB `len >= max_buffer_chars` (default 1000, configurable z `node.config["max_buffer_chars"]`).
       - Flush: `apply_built_in_detectors(buf) + apply_db_rules(buf, &rules)` → emit `EnvelopeDelta::Llm(LlmStreamChunk { choice_index, text_delta: cleaned, ..default })`. Bufor reset.
       - Reasoning content: niezależny flush (bo chain-of-thought też leci do klienta).
       - `chunk.finish_reason.is_some()` z bufor non-empty → `pending_finish = chunk.finish_reason; cancel_token`-aware drain bufora przed emit finish chunk (analogicznie do `services/runtime/middleware.rs::TtsBufferSession::pending_finish`).
    4. EOF od upstream → flush wszystkie bufory + emit pending_finish chunki.
- **Built-in detectors**: prywatne funkcje w pii_filter.rs:
  - `detect_full_names(text) -> Vec<(start, end)>` (algorytm z `middleware/pii.rs:197`, przeniesiony)
  - `apply_regex_set(text) -> String` z lazy_static regexami: NIP, PESEL, EMAIL, PHONE, ADDRESS, SURNAME_PATTERN (przeniesione z `middleware/pii.rs:41–85`)
  - Polish names list (`COMMON_POLISH_NAMES`, `COMMON_POLISH_SURNAMES`) jako `&[&str]` const w pliku.
  - Sufix detector `has_polish_surname_suffix` przeniesiony.
- **DB rules**: dodatek user-extensible regex+replace. `ctx.pii_rules.list()` zwraca `Vec<PiiRule { pattern, replacement }>`. Pętla po nich, regex compile cache (per call).

Zachowuje pełen parity z `sanitize_pii`. Jedna implementacja w jednym miejscu.

### Adapter: tts_stream_bridge (nowy)

`flow_engine/node_adapters/tts_stream_bridge.rs` (nowy plik):

- `node_type = "tts_stream_bridge"`.
- `inputs=["in"]`, `outputs=["full"]`.
- `input_port_type("in") = Text`, `output_port_type("full") = Audio`.
- `produced_artifacts = &[]`.
- Blocking `execute`: konsumuje payload Text, syntezuje całość przez
  `ctx.tts.synthesize`, emituje payload Audio. (Fallback gdy ktoś
  użyje node poza stream chainem.)
- `impl StreamingNodeAdapter`:
  - `stream_input_kind = Llm`, `stream_output_kind = Audio`.
  - `process_stream`:
    1. Per-choice text buffer (HashMap<u32, String>).
    2. Konsumuj `EnvelopeDelta::Llm(chunk)`:
       - Push `chunk.text_delta` do per-choice buffer.
       - Sentence boundary flush: ostatni char ∈ `.!?…;\n` LUB `len >= 1000`.
       - **Cancel check przed flush**: `if ctx.cancel_token.is_cancelled() { drop bufor; return EOF }`.
       - Flush: `let cleaned = ctx.tts_cleaning.clean(&buf).await?`. Przed wywołaniem cleaning store check cancel.
       - `let req = TtsRequest { text: cleaned, model, voice, format, language, cancel_token: ctx.cancel_token.clone() }` z `node.config`.
       - **Cancel check przed synthesize**: `if ctx.cancel_token.is_cancelled() { return EOF }`.
       - `let resp = ctx.tts.synthesize(req).await?` (blocking per zdanie).
       - Emit `EnvelopeDelta::Audio(AudioStreamChunk { bytes_delta: bytes_z_blob_resp, mime, sample_rate, finish_reason: None })`.
    3. EOF: flush remaining bufory + emit ostatni `EnvelopeDelta::Audio` z `finish_reason: Some(Stop)`.
- Reasoning content **nie** idzie do TTS (tylko `text_delta`). `reasoning_delta` przepuszczone bez TTS, ale audio emitowany tylko z `text_delta` zdań.
- Cancel-on-drop: explicit `cancel.is_cancelled()` check przed każdym `await` na blocking call (3 miejsca: cleaning, synthesize, blob fetch).

### Audio sink endpoint /v1/audio/speech/flow-stream

`tentaflow-core/src/api/openai/server.rs`:

- `POST /v1/audio/speech/flow-stream` — TentaFlow-specific.
- Body: JSON `{ "model": "...", "input": "...", "voice": "...", ... }` jak `/v1/audio/speech` blocking + opcjonalnie `flow_id` override.
- ACL gate na model.
- Resolver szuka flow z `final_kind=audio` zwracający stream Llm→Audio. Jeśli flow nie pasuje → 404.
- Dispatch przez flow_engine `execute_streaming` z `final_kind=Audio`.
- SSE emit: per `EnvelopeDelta::Audio(chunk)` → `data: {"audio_chunk": "<base64>", "mime": "...", "sample_rate": ..., "finish_reason": "..."}\n\n`.
- `CancelOnDropStream` opakowuje stream — disconnect → cancel propagacja przez `ExecutionContext::cancel_token`.

Routing chat path (`/v1/chat/completions` z `stream=true`) **dalej**
zabrania `final_kind=audio` — chat-stream-bridge ma text-only contract.
Jeśli admin przypisał audio flow do chat model — resolver odrzuca z 400
"flow audio output incompatible with chat completions endpoint".

### Migration: built-in PII rules + seed updates

**Brak nowej kolumny `built_in`** w `pii_rules`. Built-in detectors
żyją w kodzie adaptera; DB rules są user-extensible.

**Seed updates** (`db/seed.rs`):
- Każdy seed flow z `llm` source musi mieć `pii_filter` na chain.
  Lista flow:
  - `Standardowy pipeline LLM` (linia 542) — ma. OK.
  - `teams-flow` (linia 570) — ma. OK.
  - Pozostałe seedowane flowy — przegląd, dodanie `pii_filter` gdzie brak.
- Streaming flow musi mieć `pii_filter` z `from_port="stream"` chain
  (jeśli flow obsługuje streaming).
- Test seed_validation: każdy seed przechodzi nową R-SAFETY rule.

**User flows w produkcji**: po wdrożeniu R-SAFETY niektóre user flows
mogą nie kompilować się. Fallback: `compile()` dla niewalidnego flow
loguje error + flow nie jest dostępny do użycia. Admin musi dodać
`pii_filter`. To celowa breakage — bezpieczeństwo > kompatybilność.

## Tasks (kolejność implementacji)

### Krok 1: Schemat delty + audio sink

1. `flow_engine/envelope.rs` — `LlmStreamChunk.choice_index: u32` (default 0). `EnvelopeDeltaKind` enum + `EnvelopeDelta::kind()`.
2. `flow_engine/dispatchers_impl/llm_impl.rs:321` — propagate `choice.index` z OpenAI response.
3. `routing/streaming.rs:177` — `envelope_stream_to_chunk_stream` używa `chunk.choice_index` zamiast hardcoded 0.

### Krok 2: Streaming chain foundation

4. `flow_engine/node_adapter.rs` — `StreamingNodeAdapter` trait + `AdapterRegistry::streaming_adapters` slot + `register_streaming` helper.
5. `flow_engine/cache.rs` — `StreamingChain` struct, `compile()` wykrywa chain.
6. `flow_engine/validation.rs` — R7 nowa, R-SAFETY nowa.
7. `flow_engine/executor.rs::execute_streaming` — fold chain przez `streaming_adapters`. Finalizer rozróżnia `final_kind=Llm` (text outcome) vs `Audio` (audio outcome).
8. `flow_engine/node_adapters/output.rs` — `final_kind` config field, walidacja `mode=stream` ⇒ `final_kind` musi być.

### Krok 3: pii_filter streaming variant

9. `flow_engine/node_adapters/pii_filter.rs`:
   - Built-in detectors: przenieś z `middleware/pii.rs` (`detect_full_names`, regex set, Polish names lists, sufiksy).
   - `outputs=["full", "stream"]`.
   - `impl StreamingNodeAdapter` z per-choice buffer + sentence flush + reasoning_content flush + pending_finish hold.
10. `flow_engine/dispatcher.rs::FlowDispatcher::bootstrap` — register `pii_filter` przez `register_streaming`.

### Krok 4: tts_stream_bridge

11. `flow_engine/node_adapters/tts_stream_bridge.rs` — nowy plik. Blocking + streaming. `ctx.tts_cleaning.clean()` przed synthesize. Cancel checks.
12. `flow_engine/dispatcher.rs::FlowDispatcher::bootstrap` — register.

### Krok 5: Audio sink HTTP

13. `routing/audio_stream.rs` — nowy plik. `envelope_stream_to_audio_chunks`. Bridge `EnvelopeDelta::Audio` → SSE base64 audio chunks.
14. `api/openai/server.rs` — `handle_audio_speech_flow_stream` + route `POST /v1/audio/speech/flow-stream`. ACL, resolver z `final_kind=audio` constraint, dispatch flow stream, audio SSE emit, CancelOnDropStream.

### Krok 6: Demolish parallel installs

15. `tentaflow-core/src/middleware/` — DEL całość + `lib.rs::pub mod middleware`.
16. `routing/router.rs` — DEL `response_middleware` field/init/getter.
17. `routing/chat.rs` — DEL `apply_response_middleware` + 3 callsites.
18. `routing/streaming.rs` — DEL `wrap_with_pii_streaming` (377–522), 4 callsites (694, 758, 864, 1009-1024 test), import.
19. `services/runtime/middleware.rs` — DEL całość. `services/runtime/mod.rs:22` re-export DEL. `services/runtime/executor.rs` — DEL `middleware: Vec<...>` field, `new()` param, getter, callsites.
20. `services/tts/processor.rs::TTSBufferingProcessor` — DEL + re-exporty (`services/tts/mod.rs:11`, `services/mod.rs:31`). `services/tts/processor.rs::clean_text_for_tts` przenieś do `tts_cleaning_store_impl` jeśli nie ma już parity, albo DEL całość pliku jeśli `processor.rs` ma tylko `TTSBufferingProcessor` + `clean_text_for_tts`.
21. `config/mod.rs` — DEL `MiddlewareConfig::response_filtering_enabled`. Sprawdzić czy `request_validation_enabled` też dead, jeśli tak DEL całe `MiddlewareConfig`.
22. `config.toml` — DEL keys.

### Krok 7: Migration + seeds

23. `db/seed.rs` — przegląd wszystkich seed flowów, dodanie `pii_filter` na chain (full + stream), update test `seeded_flows_pass_adapter_validation` o R-SAFETY.
24. Migracja `pii_rules` table — bez zmian struktury (built-in żyje w kodzie). Opcjonalnie: dodać przykładowe user rules do seed jako sample.

### Krok 8: Tests

25. `pii_filter::tests::streaming_buffers_until_sentence_boundary` — token "Jan" + " " + "Kowalski." → flush, cleaned.
26. `pii_filter::tests::streaming_max_buffer_flush` — 1000 char bez boundary → flush.
27. `pii_filter::tests::streaming_per_choice_separation` — choice 0 + choice 1 → niezależne bufory.
28. `pii_filter::tests::streaming_pending_finish_held_until_buffer_drained` — finish_reason chunk z bufor non-empty → finish emitowany po cleaned content.
29. `pii_filter::tests::streaming_reasoning_independent_buffer` — content + reasoning_content niezależne.
30. `tts_stream_bridge::tests::streaming_synthesizes_per_sentence` — fake TtsDispatcher, 3 zdania → 3 audio chunki.
31. `tts_stream_bridge::tests::cancel_token_aborts_before_next_synthesize` — cancel mid-stream → no further synthesize calls.
32. `tts_stream_bridge::tests::uses_tts_cleaning_store` — verify `ctx.tts_cleaning.clean()` called per sentence.
33. `executor::tests::streaming_chain_llm_pii_output` — pełna integracja.
34. `executor::tests::streaming_chain_llm_pii_tts_audio_output` — pełna integracja audio.
35. `validation::tests::r_safety_rejects_flow_without_pii_filter` — flow z LLM bez pii_filter → compile error.
36. `validation::tests::r_safety_accepts_flow_with_pii_filter_blocking` — pii_filter na full chain.
37. `validation::tests::r_safety_accepts_flow_with_pii_filter_streaming` — pii_filter na stream chain.
38. `seed::tests::all_seeded_flows_pass_r_safety` — regression.
39. Removal-safety: `cargo test --lib --features dashboard-api,docker` po Kroku 6 — zero referencji `ResponseMiddleware`/`StreamingProcessor`/`TTSBufferingProcessor`/`StreamMiddlewareFactory`/`PiiFilterFactory`/`TtsBufferFactory`/`response_middleware`/`apply_response_middleware`/`wrap_with_pii_streaming` (rg confirms).

## Hard rules

- **R7 update** — chain stream edges, `StreamingNodeAdapter` na każdym intermediate node, chain `EnvelopeDeltaKind` compatibility, sink `output(stream)` z `final_kind` matching.
- **R8 unchanged** — typed edge data_type compatibility na portach.
- **R-SAFETY new** — flow z `llm` producerem MUSI mieć `pii_filter` w chain. Hard validation, blokuje compile.
- **No parallel install** — po stage 3d zero callsites/types z `middleware/`, `services/runtime/middleware`, `TTSBufferingProcessor`, `Router::response_middleware`.

## Bezpieczeństwo / OWASP

- **A01 Broken Access Control** — R-SAFETY enforce'uje pii_filter na każdym LLM flow. Brak raw output by default.
- **A04 Insecure Design** — built-in PII detectors w kodzie (parity z legacy `sanitize_pii`); user rules z DB tylko jako extension. Single source of truth.
- **A05 Security Misconfiguration** — `MiddlewareConfig::response_filtering_enabled` dead knob skasowany (config nie obiecuje feature który nie istnieje).
- **Cancel-on-drop** — `tts_stream_bridge` explicit cancel check przed każdym blocking await (cleaning, synthesize, blob put). `pii_filter` jest pure compute, brak cancel issues.
- **Cross-stream leak** — per-choice buffer (HashMap<u32, ...>) w obu streaming nodach.

## Migracja / breaking

- **Wycięcie `crate::middleware`** — wszystkie `use crate::middleware::*` w repo failuje cargo check. Updateujemy jednym etapem.
- **`Router::new` API** — bez zmiany sygnatury (response_middleware był wewnętrzny).
- **`ModelRuntimeExecutor::new` API** — usunięcie param `middleware: Vec<...>`. Wszystkie callsites updateujemy.
- **`MiddlewareConfig`** — pole `response_filtering_enabled` znika z config. Migracja config.toml: pole ignorowane (TOML deserializer pomija unknown keys).
- **User flows w produkcji** — flow bez `pii_filter` fail compile po R-SAFETY. Admin musi dodać node. Decyzja: bezpieczeństwo > kompat.
- **Seedowane flowy** — wszystkie aktualizujemy w tym etapie.

## Nie-cele

- **Multi-language sentence segmentation** — sentence boundary chars dalej `.!?…;\n` (parity z legacy).
- **Real-time TTS** (token-level neural) — `tts_stream_bridge` synthesize per zdanie blocking, pierwsza ramka po pierwszym zdaniu (parity z stary `TTSBufferingProcessor`).
- **Word-level granularity** w PII — mid-word flush nie istnieje, czekamy na boundary (parity z legacy).
- **Mandatory PII enforcement at runtime** — R-SAFETY jest compile-time. Runtime nie sprawdza czy admin obszedł go np. setupując flow z `condition` node który pomija `pii_filter`. Out-of-scope.
- **Multi-choice (n>1) full support** — schemat gotowy (`choice_index`), backend dispatcher i tak n=1, runtime test n=1.
