# Etap 3b — Vision LLM (multimodal text + image)

**Plan v1.1 (po round 1 codex)**

## Zmiany v1.0 → v1.1 (codex round 1)

1. **CRITICAL — modality-aware flow selection.** `resolver::resolve_flow`
   wybiera flow po `(model, service_type)`; nie wie o multimodalności
   requestu. Konsekwencja: `vision_llm` flow może zostać uruchomiony dla
   pure-text request (i runtime'owo paść), a `llm` flow dla request z image
   (i obraz zostanie zignorowany). Plan v1.1 dodaje:
   - `flow_model_bindings.modality` kolumna w DB (`text` | `image` | `any`,
     default `any`).
   - `resolver::resolve_flow(pool, model, service_type, request_modality)`
     filtruje binding'i: pasuje wpis z `modality` ∈ {`any`, request_modality}.
     Tie-break: konkretny `modality` wygrywa nad `any` przy tym samym
     `model` (np. binding z `image` priorytet nad bindingiem z `any` gdy
     request niesie image).
   - `request_modality` derived w `routing/mod.rs::build_initial_envelope_for_user`
     z parsing OpenAI request: jeśli któraś message ma `Parts(...)` z
     `image_url`, modality = `image`; inaczej `text`.
   - `FlowDispatcher::try_dispatch / try_dispatch_streaming` dostają
     `request_modality` parameter, propagują do resolvera.
   - GUI flow editor: nowy dropdown "Trigger modality" przy save flow,
     mapowany na `flow_model_bindings.modality` przy bind'owaniu modelu.
2. **IMPORTANT — VisionNodeAdapter contract jednoznaczny.** Plan v1.0 mieszał
   "input edge wymagany" z "artifact-only mode". Plan v1.1 wybiera **input
   edge wymagany**:
   - `input_port_type = Image` (R8 wymusza że incoming edge produkuje Image).
   - Image source tylko z `inputs[0].envelope.payload` (musi być
     `FlowValue::Image`).
   - `read_artifact` override w node config WYCIĘTY z 3b. Może wrócić w
     późniejszym etapie razem z artifact graph routing.
   - Trigger node, jeśli flow obsługuje vision, produkuje `FlowValue::Image`
     na payload (routing seedu robi BlobStore.put + payload assignment per
     v1.0 plan — to nie zmienia się).
3. **IMPORTANT — ChatMessage migration blast radius.** Zmiana `content:
   String` → `content: ChatMessageContent` jest wszechobecna. Plan v1.1
   dorzuca jawną listę call sites + helpery:
   - **Helper `ChatMessage::text(&self) -> Option<&str>`** — zwraca `Some`
     gdy content to `Text(s)`, `None` gdy `Parts`. Większość adapterów
     używa tylko text content; helper czyni migrację zerową dla nich.
   - **Helper `ChatMessage::text_or_default(&self) -> String`** — zwraca
     text albo zlepione tekstowe Parts.
   - Migracja konkretnych call sites:
     - `flow_engine/node_adapters/conversation_history.rs` — używa
       `m.content` jako String w `recent` mapping; zmiana na `m.text_or_default()`.
     - `flow_engine/node_adapters/pii_filter.rs` — porównuje `content`
       jako String → migracja do `text_or_default()` (PII filter nie
       działa na image messages, więc dla Parts skipuje).
     - `flow_engine/node_adapters/llm.rs::build_messages` — append
       `ChatMessage::user(payload_text)` używa konstruktora który nadal
       produkuje Text wariant (back compat).
     - `flow_engine/dispatchers_impl/llm_impl.rs::chat_msg_to_openai` —
       core mapping, pełna obsługa Parts.
     - `routing/mod.rs::message_to_chat_message` — konwertuje OpenAI
       Message na ChatMessage; gdy `MessageContent::Parts(...)`, mapuje
       na `ChatMessageContent::Parts`.
   - W każdym z tych miejsc komentarz dlaczego decyzja — text-only vs
     multimodal-aware.
4. **IMPORTANT — explicit error surface dla HTTP image URLs.** Plan v1.1:
   `routing/mod.rs::decode_data_url` zwraca `Err(InvalidRequest)` z
   message `"image_url.url musi być data URL (data:<mime>;base64,...) —
   HTTP/HTTPS URL fetch nie jest wspierane w stage 3b. Pobierz obraz po
   stronie klienta i zakoduj jako data URL."`. Routing layer
   route_chat_completion_stream / route_chat_completion catch'uje ten
   error i mapuje na HTTP 400 + OpenAI-compat error body. Klient widzi
   sensowny komunikat, nie internal/runtime fail.
5. **NIT — data URL re-encoding cost.** Każdy request z N-image historią
   robi N blob.get + N base64 encode w `chat_msg_to_openai`. Plan v1.1
   dokumentuje tradeoff: dla single-image scope (3b) brak optymalizacji.
   Cardinality / persistent message-encoded-cache zostaje na późniejszy
   etap razem z multi-image batch.

## Plan v1.0 (do review codex)
**Codex session ID:** `019dfca1-fef1-7ca1-b154-b73a796670a8`
**Data:** 2026-05-06
**Bazuje na:** Etap 3a (commit `606759d`)

---

## Po co (use case)

OpenAI vision API: klient wysyła obraz + pytanie tekstowe, model opisuje co
widzi. Format: `messages: [{role: user, content: [{type: text, text: "co tu
jest?"}, {type: image_url, image_url: {url: "data:image/jpeg;base64,..."}}]}]`.

Use cases:
- **Document Q&A** — zdjęcie faktury, "jaka jest suma?"
- **Visual debugging** — screenshot UI, "dlaczego ten przycisk jest
  niedopasowany?"
- **Image description** dla accessibility tools
- **Frame analysis** w pipeline'ach video (single frame extract)
- **Receipt/invoice OCR** + structured extraction

Bez 3b TentaFlow obsługuje tylko text-in/text-out chat. Vision-capable
backendy (GPT-4o, Claude Sonnet, llava, qwen-vl, intern-vl, idefics) są
niewykorzystane.

---

## Zakres (3b)

1. **Single-image vision** — jeden obraz + jeden tekst pytania → odpowiedź
   tekstowa. Bez cardinality (multi-image batch zostaje na cardinality stage).
2. **Nowy node type `vision_llm`** — separated od `llm` żeby:
   - GUI mógł go pokazać z innym kolorem / ikoną
   - Validation R3 sprawdzała że flow z vision wymaga input portu Image
   - Adapter przyjmował konkretny shape (Text payload + Image artifact LUB
     Image payload + Text node config prompt)
3. **`ChatMessage` extension** — content: enum z wariantem `Parts(Vec<MessagePart>)`
   gdzie `MessagePart` ma `Text(String)` i `Image { blob_ref, mime, detail }`.
   Backward compat: `ChatMessage::user(s)` produkuje wariant `Text(s)` jak dziś.
4. **`LlmRequest` przepuszcza Parts** — gdy `messages[i].content` to Parts,
   `LlmDispatcherImpl::build_chat_request` mapuje na OpenAI
   `MessageContent::Parts`.
5. **`VisionNodeAdapter`** — input `["in"]` z input_port_type = Image (lub
   Text — see "Shape options" niżej), output `["full"]` Text. Buduje
   `ChatMessage` z Parts (text question + image as data URL).
6. **Image → data URL conversion** — adapter pulluje bytes z `BlobStore` po
   `BlobRef`, base64-encoduje, składa `data:<mime>;base64,...` URL. Backend
   otrzymuje gotowy URL.

---

## Co NIE robimy w Etap 3b

- Multi-image (cardinality) — Etap 3d
- Audio input w chat (Omni LLM) — Etap 3c streaming + multimodal extension
- Image output (image generation flow) — osobny etap
- Vision streaming — vision response jest text, więc dziedziczy chat
  streaming z 3a (działa już)
- HTTP image URL fetch (download → base64) — wszystkie obrazy muszą być w
  BlobStore. URL fetching jako dedicated download node w przyszłości.
- `detail: "low"|"high"|"auto"` per OpenAI spec — Etap 3b ustawia `detail:
  "auto"` zawsze; per-call override z node config dochodzi w follow-up.

---

## Hard rules

13. **Vision flow shape** — `vision_llm` node MUSI mieć incoming edge z
    `from_port` produkującym Image albo musi mieć dostęp do Image przez
    `read_artifact` (ArtifactKey, Etap 3 follow-up). W Etap 3b: input port
    type = Image (validation R8 wymusza).
14. **Image source priority** — adapter szuka image w kolejności:
    `node.config["read_artifact"]` (klucz → envelope.artifacts[key]) →
    `envelope.payload` (gdy jest Image) → Err. Tekst pytania:
    `node.config["prompt"]` → ostatnia user message w `envelope.context.messages`
    → Err.
15. **Brak Vision-specific dispatcher trait** — używamy istniejącego
    `LlmDispatcher`. Vision = LLM z multimodal messages. Backend decyduje czy
    przyjmuje obraz (catalog modality flag); failure surface jak normalny
    "model nie wspiera image_url".

---

## Typy

### `ChatMessage` extension (`flow_engine/envelope.rs`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatMessage {
    pub role: ChatRole,
    /// Etap 3b: rozszerzono na multimodal. Pre-3b kod tworzył
    /// `ChatMessageContent::Text(s)` przez konstruktory `ChatMessage::user`/
    /// `system`/`assistant`. Vision adapter tworzy `Parts(...)`.
    pub content: ChatMessageContent,
    pub name: Option<String>,
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ChatMessageContent {
    Text(String),
    Parts(Vec<MessagePart>),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessagePart {
    /// Fragment tekstowy.
    Text { text: String },
    /// Image przez BlobRef. Etap 3b zawsze rozwiązuje do data URL przed
    /// wysłaniem do backendu (`LlmDispatcherImpl::build_chat_request`).
    /// `detail` controls vision token budget per OpenAI: "auto" / "low" /
    /// "high"; default "auto".
    Image {
        blob_ref: crate::flow_engine::blob_store::BlobRef,
        #[serde(default = "default_image_detail")]
        detail: String,
    },
}

fn default_image_detail() -> String {
    "auto".to_string()
}
```

Konstruktory `ChatMessage::user(text)` / `system` / `assistant` dalej
produkują `Text(s)` — back compat dla wszystkich istniejących adapterów.
Nowy konstruktor `ChatMessage::user_multimodal(parts)` dla vision.

### `MessagePart::Image.blob_ref` vs `MessagePart::Image.data_url`

Trzymamy `BlobRef` w `ChatMessage`, NIE rozwinięty data URL. Powody:
- ChatMessage przechodzi przez `envelope.context.messages` po wielu adapterach;
  data URL może mieć MB → niepotrzebne kopiowanie.
- `LlmDispatcherImpl::build_chat_request` ma dostęp do BlobStore (przez slot)
  — rozwiązuje BlobRef w jednym miejscu, async.
- Pre-image flow (no vision) nigdy nie trzyma image w messages, więc
  ChatMessage stays small.

Konsekwencja: `LlmDispatcherImpl` MUSI dostać BlobStore (dziś nie ma).
Bootstrap: `LlmDispatcherImpl::new(runtime, blobs)` zamiast tylko `runtime`.

---

## VisionNodeAdapter

```rust
// flow_engine/node_adapters/vision_llm.rs

const NODE_TYPE: &str = "vision_llm";

pub struct VisionNodeAdapter;

impl NodeAdapter for VisionNodeAdapter {
    fn node_type(&self) -> &str { NODE_TYPE }
    fn supported_input_ports(&self) -> &[&'static str] { &["in"] }
    fn supported_output_ports(&self) -> &[&'static str] { &["full"] }
    fn input_port_type(&self, _port: &str) -> FlowDataType { FlowDataType::Image }
    fn output_port_type(&self, _port: &str) -> FlowDataType { FlowDataType::Text }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let envelope = inputs.first()
            .ok_or_else(|| anyhow!("vision adapter: missing input"))?
            .envelope.clone();

        // 1. Image source: node.config[read_artifact] -> artifacts[key] albo
        //    payload bezpośrednio gdy Image.
        let (blob_ref, image_mime) = resolve_image_source(node, &envelope)?;
        let detail = node.config.get("detail")
            .and_then(|v| v.as_str())
            .unwrap_or("auto")
            .to_string();

        // 2. Tekst pytania: node.config[prompt] -> ostatnia user message ->
        //    Err.
        let prompt = resolve_prompt(node, &envelope)?;

        // 3. Buduj ChatMessage z Parts.
        let user_message = ChatMessage::user_multimodal(vec![
            MessagePart::Text { text: prompt },
            MessagePart::Image {
                blob_ref: blob_ref.clone(),
                detail,
            },
        ]);

        // 4. Sklej z system_prompts (envelope.context.system_prompts) +
        //    historia (envelope.context.messages PRZED naszym user message).
        let mut messages: Vec<ChatMessage> = envelope
            .context
            .system_prompts
            .iter()
            .map(|sp| ChatMessage::system(sp.clone()))
            .collect();
        messages.extend(envelope.context.messages.iter().cloned());
        messages.push(user_message);

        // 5. Standard LlmRequest, ale messages mają Parts w ostatniej.
        let model = LlmNodeAdapter::pick_model(node, &envelope)?;
        let req = LlmRequest {
            model,
            messages,
            temperature: pick_optional_f32(node, &envelope, "temperature"),
            max_tokens: pick_optional_u32(node, &envelope, "max_tokens"),
            top_p: pick_optional_f32(node, &envelope, "top_p"),
            frequency_penalty: pick_optional_f32(node, &envelope, "frequency_penalty"),
            presence_penalty: pick_optional_f32(node, &envelope, "presence_penalty"),
            stop: pick_stop(node),
            deadline: ctx.deadline,
            cancel_token: ctx.cancel_token.clone(),
            user_id: ctx.user_id,
            user_role: ctx.user_role.clone(),
        };

        let response = ctx.llm.execute_chat(req).await?;

        // 6. Output envelope: payload Text(response.content), usage
        //    rejestrowany przez UsageSink.
        let mut out: FlowEnvelope = (*envelope).clone();
        out.payload = FlowValue::Text(response.content.clone());
        out.context.messages.push(ChatMessage::assistant(response.content));
        ctx.usage_sink.record(&node.id, response.usage);
        Ok(out)
    }
}

fn resolve_image_source(
    node: &FlowNode,
    envelope: &FlowEnvelope,
) -> Result<(BlobRef, String)> {
    // (a) read_artifact override
    if let Some(key) = node.config.get("read_artifact")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        if let Some(FlowValue::Image { blob_ref, mime, .. }) = envelope.artifacts.get(key) {
            return Ok((blob_ref.clone(), mime.clone()));
        }
        return Err(anyhow!("vision adapter: artifact '{key}' missing or not Image"));
    }
    // (b) payload bezpośrednio
    if let FlowValue::Image { blob_ref, mime, .. } = &envelope.payload {
        return Ok((blob_ref.clone(), mime.clone()));
    }
    Err(anyhow!("vision adapter: no image (payload not Image, brak read_artifact w node config)"))
}

fn resolve_prompt(node: &FlowNode, envelope: &FlowEnvelope) -> Result<String> {
    // (a) node.config["prompt"]
    if let Some(p) = node.config.get("prompt")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        return Ok(p.to_string());
    }
    // (b) ostatnia user message z envelope.context.messages
    if let Some(last_user) = envelope.context.messages.iter().rev()
        .find(|m| matches!(m.role, ChatRole::User))
    {
        if let ChatMessageContent::Text(t) = &last_user.content {
            if !t.is_empty() {
                return Ok(t.clone());
            }
        }
    }
    Err(anyhow!("vision adapter: no prompt (node.config['prompt'] empty, brak user message w envelope.context)"))
}
```

---

## LlmDispatcherImpl rozszerzenie

`LlmDispatcherImpl` musi:
1. Przyjmować `BlobStore` żeby rozwinąć `MessagePart::Image.blob_ref` na data
   URL.
2. Rozszerzyć `chat_msg_to_openai`: gdy `content` to `Parts(...)`, mapować na
   OpenAI `MessageContent::Parts(Vec<ContentPart>)`. `MessagePart::Image`
   pulluje bytes z BlobStore, base64-encoduje, składa `data:<mime>;base64,...`.

```rust
pub struct LlmDispatcherImpl {
    runtime: ModelRuntimeSlot,
    blobs: Arc<dyn BlobStore>,  // NEW
}

impl LlmDispatcherImpl {
    pub fn new(runtime: ModelRuntimeSlot, blobs: Arc<dyn BlobStore>) -> Self {
        Self { runtime, blobs }
    }

    async fn chat_msg_to_openai(&self, m: &ChatMessage) -> Result<openai::Message> {
        let content = match &m.content {
            ChatMessageContent::Text(t) => Some(openai::MessageContent::Text(t.clone())),
            ChatMessageContent::Parts(parts) => {
                let mut openai_parts = Vec::with_capacity(parts.len());
                for p in parts {
                    match p {
                        MessagePart::Text { text } => {
                            openai_parts.push(openai::ContentPart::Text { text: text.clone() });
                        }
                        MessagePart::Image { blob_ref, detail } => {
                            let bytes = self.blobs.get(blob_ref).await?;
                            let url = format!(
                                "data:{};base64,{}",
                                blob_ref.mime,
                                base64::engine::general_purpose::STANDARD.encode(&bytes)
                            );
                            openai_parts.push(openai::ContentPart::ImageUrl {
                                image_url: openai::ImageUrl {
                                    url,
                                    detail: Some(detail.clone()),
                                },
                            });
                        }
                    }
                }
                Some(openai::MessageContent::Parts(openai_parts))
            }
        };
        Ok(openai::Message {
            role: chat_role_to_str(m.role).to_string(),
            content,
            reasoning_content: None,
            name: m.name.clone(),
            tool_calls: None,
            tool_call_id: m.tool_call_id.clone(),
        })
    }
}
```

`build_chat_request` staje się async (bo `chat_msg_to_openai` async). Tracking
issue: `execute_chat` i `stream_chat` w `LlmDispatcher` traicie są już async,
więc OK.

`base64` crate prawdopodobnie już jest deps; inaczej dodajemy.

---

## Validation R8 dla vision

Vision adapter ma `input_port_type = Image`. Edge wchodzący do `vision_llm`
musi mieć `from_type = Image` lub `Any`. Producent Image dziś = `stt` (wait —
stt produces Text, ale jego artifact source_audio jest Audio). Single
producent Image w Etap 3b: trigger seed gdy multimodal request. Trigger ma
`output_port_type = Any`, więc R8 przepuszcza.

Praktyczny scenariusz: GUI buduje flow `trigger → vision_llm → output`,
trigger ma `data_type=image` na edge'u (deklaracja). Adapter trigger nie
zmienia typu (Any), edge.data_type konkretny — R8 sprawdza compatible: edge
Image vs trigger Any (compat) i edge Image vs vision_llm Image (compat). ✓.

---

## Routing — image źródło z requestu

OpenAI request `messages: [{role, content: [text + image_url]}]` musi trafić
do flow. Dziś `routing/mod.rs::build_initial_envelope_for_user` buduje:
- `payload = Text(last_message_text)`
- `context.messages = wszystkie messages konwertowane`

Etap 3b rozszerza:
- jeśli ostatnia user message zawiera Parts z image_url, parsujemy data URL
  → bytes → put do BlobStore → BlobRef → `payload = FlowValue::Image { ... }`.
- pozostałe parts text → `payload = Text(combined_text)` przed Image? Albo
  zostawiamy text w `meta["prompt"]` żeby vision adapter przeczytał.

**Decyzja:** `payload = Image` (główny artifact request'a), `text` z message
parts trafia do `envelope.context.messages` jako ChatMessage z Parts. Adapter
vision czyta payload (Image) + ostatni user message (Parts z text).

Jeżeli request ma TYLKO text (no image), zachowanie pre-3b: `payload = Text`.
Vision adapter nie odpali (validation R8 albo runtime check).

```rust
fn build_initial_envelope_inner(request: &ChatCompletionRequest, blobs: &dyn BlobStore)
    -> (FlowEnvelope, FlowRequestMeta)
{
    let mut env = FlowEnvelope::empty();
    // ... existing meta seeding ...

    // Etap 3b: detect image w ostatniej user message i wyciągnij do payload.
    let mut found_image: Option<(BlobRef, String)> = None;
    if let Some(last_user) = request.messages.iter().rev()
        .find(|m| m.role == "user")
    {
        if let Some(MessageContent::Parts(parts)) = &last_user.content {
            for p in parts {
                if let ContentPart::ImageUrl { image_url } = p {
                    if let Some((bytes, mime)) = decode_data_url(&image_url.url) {
                        // Put do blobstore — async w sync builder. Compromise:
                        // build_initial_envelope staje się async.
                        let blob = blobs.put(bytes, &mime).await.ok()?;
                        found_image = Some((blob, mime));
                        break;
                    }
                }
            }
        }
    }
    if let Some((blob_ref, mime)) = found_image {
        env.payload = FlowValue::Image { blob_ref, mime, dims: None };
    } else {
        // Fallback: text payload jak dziś.
        let payload_text = ...;
        if !payload_text.is_empty() {
            env.payload = FlowValue::Text(payload_text);
        }
    }

    // context.messages: konwertujemy każdą Message — Parts → ChatMessageContent::Parts
    env.context.messages = request.messages.iter()
        .filter_map(|m| message_to_chat_message_with_blobs(m, blobs).await)
        .collect();
    ...
}
```

`build_initial_envelope_for_user` staje się async + przyjmuje `&dyn BlobStore`
parametr. Routing/chat.rs i routing/streaming.rs przekazują `dispatcher.blobs()`.

---

## Call site refactor map

| Plik | Akcja | LOC |
|------|-------|-----|
| `flow_engine/envelope.rs` | `ChatMessage.content: ChatMessageContent` (was String) + `MessagePart` enum + image variant | +60 |
| `flow_engine/node_adapters/vision_llm.rs` | NEW VisionNodeAdapter + helpers | +180 |
| `flow_engine/node_adapters/mod.rs` | + `pub mod vision_llm; pub use ...;` | +4 |
| `flow_engine/dispatcher.rs` | `build_registry` rejestruje VisionNodeAdapter | +5 |
| `flow_engine/dispatchers_impl/llm_impl.rs` | LlmDispatcherImpl::new(+blobs), chat_msg_to_openai async resolve image, build_chat_request async | +80 |
| `flow_engine/dispatcher.rs` | `LlmDispatcherImpl::new(slot, blobs)` w bootstrap | +5 |
| `routing/mod.rs` | `build_initial_envelope_for_user` async + image extraction, decode_data_url helper | +80 |
| `routing/chat.rs` / `streaming.rs` | wywołania await na build_initial_envelope_for_user | +10 |
| `services/runtime/executor.rs` | brak zmian (jeśli embeddings/tts seed nie zmienia) | 0 |
| Tests | VisionNodeAdapter resolve_image / resolve_prompt; ChatMessage Parts roundtrip; data URL parse; integration | +120 |
| `Cargo.toml` | `base64 = "0.22"` jeśli brak (sprawdzić) | +1 |

**Razem: ~545 LOC** w plan v1.0; **v1.1 dorzuca:**

| Plik | Akcja v1.1 | LOC |
|------|------------|-----|
| `db/migrations.rs` | + migracja kolumny `flow_model_bindings.modality TEXT DEFAULT 'any'` | +20 |
| `flow_engine/resolver.rs` | `resolve_flow` przyjmuje `request_modality: &str`, filtruje + tie-break | +40 |
| `flow_engine/dispatcher.rs` | `try_dispatch / try_dispatch_streaming` przyjmują `request_modality`, propagują | +20 |
| `routing/mod.rs` | `derive_request_modality` helper z OpenAI request → "text"/"image" | +30 |
| `flow_engine/envelope.rs` | `ChatMessage::text()`, `text_or_default()` helpery | +25 |
| `routing/mod.rs::decode_data_url` | explicit `InvalidRequest` error gdy non-data URL | +10 |
| Migracja call sites (conversation_history, pii_filter, llm) | `text_or_default()` calls | +15 |
| Tests modality routing + helpery | | +50 |

**Razem v1.1: ~755 LOC.** Większy ale wciąż w jednym sub-stage.

---

## Test strategy

### Unit
- `ChatMessage::user_multimodal(parts)` round-trips serializacja JSON
- `MessagePart::Image` JSON round-trip (z `detail`)
- `VisionNodeAdapter::resolve_image_source`: payload Image / artifact / Err
- `VisionNodeAdapter::resolve_prompt`: node config / last user / Err
- `decode_data_url`: prawidłowe parse `data:image/jpeg;base64,...`, mismatch
  rejected
- `LlmDispatcherImpl::chat_msg_to_openai` gdy Parts → mapuje na OpenAI Parts
  z data URL

### Integration
- Flow `trigger → vision_llm → output`: fake LLM dispatcher zwraca Text z
  zawartością "I see a cat", check że envelope.payload na końcu = Text("I see a cat")
- Klient OpenAI vision request (text + image_url) → routing buduje
  initial envelope z payload Image → flow vision execute → response Text
- R8 validation rejects flow gdzie `vision_llm` ma input port_type=Text

---

## Otwarte ryzyka

1. **`build_initial_envelope_for_user` async cascade** — wszystkie callsites
   (chat.rs, streaming.rs, services/runtime/executor.rs) muszą await.
   Compile error explosion mitygowany przez sed/regex bulk patch jak w 3a.
2. **Data URL parse — non-base64 URLs** — obecnie OpenAI klient może wysłać
   `https://example.com/image.jpg` jako image_url.url. Etap 3b NIE robi
   download; rzucamy Err "vision adapter: only base64 data URLs supported in
   stage 3b". HTTP fetch to dedicated download node w przyszłości.
3. **Image MIME detection** — gdy data URL ma `data:application/octet-stream;...`
   zamiast `data:image/jpeg;...`, backend może odrzucić. Adapter loguje warn.
4. **Token usage rollup dla vision** — backend zwraca `prompt_tokens` które
   uwzględnia image tokens (per OpenAI: high-detail = ~765 tokens). Etap 3a
   tail chunk już to przepuszcza, więc 3b nic nie zmienia.
5. **`detail: "low"|"high"` per request** — Etap 3b honoruje tylko
   node.config["detail"] (operator decyduje). Per-request override z
   ChatCompletionRequest pozostawiamy na follow-up; OpenAI klient zwykle
   trzyma się "auto".
6. **base64 dep** — sprawdzić czy istnieje w Cargo.toml, dodać jeśli brak.
7. **Streaming vision** — vision response jest text, więc dziedziczy chat
   streaming (3a działa). Vision input nigdy nie jest streamowane (jeden
   image, blocking).

---

## Workflow

1. Plan codex review
2. Implementacja
3. Codex code review
4. (Opcjonalnie) update CLAUDE.md po 3b albo bundle z 3c.
