# RAG Ingest jako jawny Flow (NVIDIA nv-ingest faithful, bez gRPC)

Cel: przeniesienie ingestu dokumentów z zahardkodowanego `run_ingest_pipeline`
(addon RAG, Rust) do **jawnego flow** edytowalnego w Flow Builderze. Nic ukrytego —
każdy krok to node, każdy model wybierany z dropdowna. Replikuje pipeline NVIDIA
RAG / nv-ingest, ale wewnątrz mesh ZAWSZE binary, do kontenerów/native REST
(OpenAI/NVIDIA-compatible), na zewnątrz nasze REST. Zero gRPC (NVIDIA daje gRPC+HTTP
per NIM — bierzemy HTTP).

## Stan obecny (do usunięcia)

`addons/rag/src/lib.rs::run_ingest_pipeline` — twardy `match classify_mime`:
`Parse`(obraz/PDF)→`doc_parse`, `Text`, `Xlsx`→calamine, `Docx`→quick-xml →
`split_into_chunks` → per chunk embed+graf. Routing typu pliku i dobór parsera
ukryte w Rust. Flow istnieje TYLKO dla query (`rag:query`, `retrieval-round`).

## Mapowanie modeli NVIDIA → nasze ServiceSurface → API

| Funkcja | Model NVIDIA | Surface (nasz) | API do kontenera (REST) |
|---|---|---|---|
| Generacja | nemotron-super | `Chat` | `/v1/chat/completions` |
| Embedding | llama-nemotron-embed-vl | `Embeddings` | `/v1/embeddings` |
| Rerank | llama-nemotron-rerank-vl | `Rerank` | `/v1/rerank` (≈ NVIDIA `/v1/ranking`) |
| **Vision parse (VLM)** | nemotron-parse | **`Chat`** (vision) | **`/v1/chat/completions`** (image_url+tools) |
| Detekcja page-elements | nemotron-page-elements (YOLOX) | `Documents` | `/v1/infer` |
| Struktura tabel | nemotron-table-structure | `Documents` | `/v1/infer` |
| Grafiki/wykresy | nemotron-graphic-elements | `Documents` | `/v1/infer` |
| OCR | nemotron-ocr / paddleocr | `Documents` | `/v1/infer` |

Transport node↔node: binary (UFP/2) dla KAŻDego surface. `Chat`/`Embeddings`/`Rerank`
już działają przez mesh; `Documents` (detekcja) wymaga dorobienia mesh-forward
(sender + receiver arm, wzorem rerank).

## Docelowy flow ingestu (engine-flow `rag:ingest`)

```
[trigger]
   ├─ image ────────────────────────────────────► [vision_parse] ─┐
   └─ other ─► [document_router]                                   │
                  ├─ pdf  ─► [pdf_rasterize] ─img─► [vision_parse] ─┤
                  │                          └─img─► [page_detect] ─► [table_structure] ─► [ocr] ─►(merge)─┤
                  ├─ xlsx ─► [excel_extract] ──────────────md───────────────────────────────────────────┤
                  ├─ docx ─► [word_extract] ───────────────md───────────────────────────────────────────┤
                  └─ pptx ─► [pptx_extract] ───────────────md───────────────────────────────────────────┤
                                                                                                         ▼
                                                                       [chunk] ─► [embed] ─► [store] ─► [graph_extract]
```

## Nowe node-adaptery (każdy z wyborem modelu gdzie dotyczy)

- **document_router** — wejście `other` (bajty+mime+magic-bytes), wyjścia per-typ:
  `pdf`/`xlsx`/`docx`/`pptx`/`image`/`text`/`unknown`. Bez modelu (czysta detekcja typu).
- **pdf_rasterize** — PDF→strony PNG (pdfium, bezwarunkowe). Config: DPI, max stron.
- **vision_parse** — obraz strony → markdown przez model `Chat`-vision. **Config: `model`
  (dropdown, dynamic_enum source=models category=chat/vision; domyślnie alias `rag-parse`),
  `tools` (markdown_bbox|markdown|text), max_tokens.** Silnik = vision-chat (`/v1/chat/completions`).
- **page_detect** — detekcja regionów (tekst/tabela/wykres) modelem `Documents`. Config: `model`
  (dropdown, domyślnie alias `rag-page-elements`).
- **table_structure** — struktura tabeli → markdown table. Config: `model` (`rag-table-structure`).
- **graphic_elements** — wykresy/grafiki → opis/dane. Config: `model` (`rag-graphic-elements`).
- **ocr** — OCR regionu/strony. Config: `model` (`rag-ocr`, np. paddleocr|nemotron-ocr).
- **excel_extract** / **word_extract** / **pptx_extract** — czysty Rust (calamine / quick-xml /
  pptx), GFM tabele. Opcjonalnie wyjście `images`→vision_parse dla osadzonych obrazów/wykresów.
- **chunk** — markdown→chunki (size/overlap config). **embed** — istnieje (`embeddings`).
- **store** — INSERT chunk + vector_upsert. **graph_extract** — istniejąca ekstrakcja grafu (bramka).

## Wybór modeli per-blok (wymóg użytkownika)

Każdy node parsujący ma pole `model` w schemacie konfiguracji (jak `llm` node):
`dynamic_enum { source: "models", category: <surface> }`. Domyślnie alias RAG
(`rag-parse`/`rag-page-elements`/…), ale operator zmienia na konkretny model/alias
w Flow Builderze. Aliasy RAG auto-tworzone przy install addona (jak `rag-llm`).
Dzięki temu „dla tabel/wykresów dobieramy odpowiednie modele" jest JAWNE w flow.

## Rejestracja + trigger

Addon RAG rejestruje `[[engine_flow]] ingest` (jak `query`). `run_ingest_pipeline`
znika — handler `ingest-uploaded` woła flow `rag:ingest` jako model (envelope:
FlowValue::Other{bytes,mime} dla nie-obrazu, FlowValue::Image dla obrazu). Flow
zwraca markdown+chunki, addon zapisuje. Per-kolekcja override flow (własny ingest
w Flow Builderze) możliwy przez `FlowDispatcher`.

## KOREKTY z review codex (planning-stage) — OBOWIĄZUJĄCE

1. **Binarny chat NIE niesie obrazów (krytyczne).** `CompletionPayload.messages: Vec<Message>`,
   `Message.content: String` (tylko tekst). Konwersja mesh wyrzuca części nie-tekstowe
   (`routing/mod.rs:343`), reverse odbudowuje jako tekst (`inference_proxy.rs:351`). Vision-chat
   przez mesh NIE działa dopóki nie rozszerzymy binarnego payloadu. **Wybór: rozszerzyć
   `CompletionPayload`/`Message` o `VisionContentPart` (image_url base64) end-to-end przez mesh,
   ALBO użyć `ModelPayload::Vision`.** To jest PIERWSZY krok, nie założenie.
2. **`vision_parse` woła `execute_chat`/vision-chat WPROST**, nie `execute_documents`
   (executor.rs:1743 twardo resolvuje `Documents` — gdy parse→chat przestanie go znajdować).
   `execute_documents`/`DocumentParseResponse` zostaje TYLKO dla detektorów (`/v1/infer`).
3. **Trzy odrębne kontrakty backendu** (nie mutować `parse_document`): (a) chat-vision
   `/v1/chat/completions` dla `vision_parse`; (b) typed `/v1/infer` (`DocumentInferRequest/Response`
   z bbox/klasa/score/komórki/OCR-spans) dla `page_detect`/`table_structure`/`graphic_elements`/`ocr`;
   (c) `parse_document` (`/parse` multipart, client.rs:841) — legacy, zostaje lub usuwany.
4. **Nowy node `document_merge`** (brakował): scala VLM markdown + detekcje + OCR spans + struktury
   tabel + grafiki + numery stron + reading order. Bez niego flow builder nie wyrazi nv-ingest.
5. **`graphic_elements`** wpiąć: konsumuje regiony grafik z `page_detect` → `document_merge`.
6. **Zachować transakcyjny cleanup** z `run_ingest_pipeline` (lib.rs:642 cleanup-then-reingest,
   lib.rs:720 cleanup wektorów/chunków przy failu) — `store` lub orkiestracja ingestu musi to
   odtworzyć (kompensacja), inaczej fail node'a zostawia orphany.
7. **Nowe node-typy → modality inference** (provider.rs:411) + drift-testy, inaczej katalog
   źle anonsuje capabilities flow.
8. Ryzyka: base64 overhead per strona; fan-out N chat-calls per dokument → rate-limit/concurrency;
   retry/idempotency dla `store`; progress-reporting zamiast obecnych job updates; rekursja/głębokość
   flow gdy ingest-flow woła parse-flow; ACL jawne (`execute_chat` — caller owns checks, executor.rs:237).
   pptx to NOWY scope (dziś tylko Text/Parse/Xlsx/Docx, lib.rs:6888).

## Sekwencja implementacji (POPRAWIONA wg codex)

1. **Binarny payload vision przez mesh** — rozszerzyć `CompletionPayload`/`Message` o vision content
   (image_url base64) end-to-end: sender (`routing/mod.rs`), receiver (`inference_proxy.rs`),
   CBOR wire. ALBO standaryzacja na `ModelPayload::Vision`. Bez tego reszta nie ma fundamentu.
2. **Vision-chat parse jako ścieżka Chat** — `vision_parse` → `execute_chat` z obrazem strony →
   markdown. `nemotron-parse.toml` = `["chat"]` (zgodne z NVIDIA `/v1/chat/completions`). Odblokowuje PDF.
3. **Typed Documents `/v1/infer` + mesh-forward** — `DocumentInferRequest/Response`, sender
   (`dispatch_documents_blocking` MeshForward) + receiver arm (wzorem rerank) dla detektorów.
4. **Model artefaktów flow** — blob/page artifacts (obrazy stron, crop regiony), FlowValue rozszerzenia.
5. **Node-adaptery + `document_merge`** — document_router, pdf_rasterize, vision_parse, page_detect,
   table_structure, graphic_elements, ocr, excel/word/pptx_extract, chunk, store, graph_extract, document_merge.
6. **Engine-flow `rag:ingest`** + rejestracja w addonie + trigger zamiast `run_ingest_pipeline`
   (z zachowaniem transakcyjnego cleanupu).
7. **Flow Builder** — szablony nodów (`flow_node_templates`) z polami `model` (dynamic_enum per surface).
8. **Serwisy** — page-elements/table-structure/graphic-elements/ocr (`/v1/infer`), aliasy RAG,
   deploy docker/native (rig ma nemotron-page-elements, nemotron-ocr).
