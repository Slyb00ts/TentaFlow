# Plan architektury RAG (TentaFlow)

Cel: własna, niezawodna, bardzo szybka i dająca bezbłędne odpowiedzi aplikacja RAG —
GUI i logika nasze, pod spodem te same modele co NVIDIA RAG Blueprint, plus baza grafowa
(GraphRAG) i pamięciowy multi-agent (MemGraphRAG). Działa od dużych serwerów po telefony.

## 1. Zasada architektoniczna (ustalona z właścicielem)

- **Core (Rust) dostarcza PRYMITYWY jako narzędzia**: baza wektorowa (jest), SQLite (jest),
  KV (jest) oraz NOWA baza grafowa. Wszystko izolowane per `(org, addon_instance, collection)`.
- **Logika RAG żyje w ADDONIE** (WASM): orkiestracja, strategia chunkowania, ekstrakcja
  encji/triple'ów, polityka rozwiązywania konfliktów, fuzja wyników, GUI, konfiguracja instancji.
- **Pipeline modeli to FLOW w FlowBuilder**: addon składa flow z węzłów core (embeddings,
  reranker, parse, llm, vector, graph) i własnych bloków `addon.rag.*`, uruchamia przez
  `flow_invoke_v1`.
- **Modele przez ALIASY z fallbackami**: addon deklaruje `rag-embeddings`, `rag-llm`,
  `rag-reranker`, `rag-parse`; my mapujemy aliasy na realne modele. Na serwerze duże modele,
  na telefonie mniejsze — ta sama logika addona, inne mapowanie aliasu.
- **Wiele niezależnych instancji**: `install_instance` przepisuje `[addon].id` na syntetyczny
  `instance_id` → izolacja wektorów/SQL/KV/grafu staje się per-instancja (gotowe w core).

## 2. Dlaczego NVIDIA Blueprint nie działa na B300 (i nasz escape)

Blueprint = aplikacja LangChain/LangGraph spięta z **NIM microservices**. NIM to kontenery z
TensorRT-LLM (buildy silników pod konkretną compute capability), CUDA/cuDNN/NCCL/Triton oraz
cuVS (GPU vector index). Brak profilu `sm_100` (Blackwell B200/B300) → build silnika lub
dispatch kerneli pada, mimo że model HF rusza na świeżym vLLM/PyTorch/CUDA. To **kwestia
pakowania, nie tożsamości modeli**. Nasz escape już zrobiony: re-serwowanie tych samych
modeli przez vLLM/native.

## 3. Co już mamy (potwierdzone w kodzie)

| Komponent | Status | Dowód |
|-----------|--------|-------|
| LLM (nemotron-3-super-120b) | ✅ | `tentaflow-containers/llm/_services/vllm.toml` |
| Embeddings (llama-nemotron-embed-1b-v2) | ✅ | `embeddings/_services/nemotron-embed.toml` |
| Reranker (llama-nemotron-rerank-1b-v2) | ✅ | `reranker/_services/nemotron-rerank.toml` |
| nv-ingest (ocr/parse/page/table/graphic) | ✅ | `vision/_services/nemotron-*.toml`, `paddle-ocr.toml` |
| Vector DB (zvec embedded + Milvus) | ✅ | `services/vector/{namespace,milvus_backend}.rs` |
| Vector host ABI (upsert/search/hybrid/delete) | ✅ | `addon/host_functions/vector.rs` |
| Izolacja `(org, addon, namespace)` + limity 10 ns / 1M wek. | ✅ | `services/vector/namespace.rs` |
| Wiele instancji addona | ✅ | `addon/lifecycle.rs:install_instance` |
| Flow engine + node adaptery (embeddings, llm, memory, addon.*) | ✅ | `flow_engine/node_adapters/` |
| `flow_invoke_v1/status/cancel` (addon→flow) | ✅ | `addon/host_functions/flow.rs` |
| Aliasy modeli (`[[alias]]`, `[[uses_alias]]`, resolve) | ✅ (częściowo) | `addon/host_functions/aliases.rs`, `db/repository.rs:resolve_model_alias_for_addon` |
| Addon GUI (CBOR PanelShell), per-addon SQLite/KV | ✅ | `addons/contacts/`, `host_functions/storage.rs` |

`embeddings-chunker` = cienki helper (naiwny chunking + `llm_generate`), nie trwały indeks.
`memory` addon = deklaracja architektoniczna w docs, nie produkcyjny GraphRAG.

## 4. Czego BRAKUJE (luki do zbudowania)

1. **Baza grafowa w core** — zero (brak neo4j/kuzu/cozo/petgraph jako warstwy). → CozoDB.
2. **Graph host functions + flow node** — `graph_upsert_node/edge`, `graph_query`,
   `graph_pagerank`, `graph_ppr`, `graph_neighbors`, izolacja per instancja.
3. **Fallback chain aliasów NIE działa w runtime** — struktura `fallback_targets` jest w DB,
   ale executor nie robi failoveru A→B→C. Krytyczne dla „serwer→telefon". → dobudować w
   `resolve_model_alias_for_addon` + ścieżce wykonania (LLM/embeddings/reranker dispatch).
4. **Brak węzła flow rerankera** — reranker jest serwisem, ale nie ma node adaptera ani
   wpięcia w retrieval.
5. **Brak węzła flow parsowania dokumentów** — vision services (ocr/parse/page/table/graphic)
   nie są opakowane w node adapter / pipeline ingestu.
6. **Brak document/blob store dla surowych plików** — addon ma tylko KV (max 1MB/wartość) i
   recording (PNG/MP4). `BlobStore` trait istnieje w `flow_engine/blob_store.rs`, ale addon
   nie ma host function. → host function `document_put/get/delete` (duże pliki PDF).
7. **Brak modelu jobów ingestu** — `flow_invoke` daje running/status/cancel, ale masowy
   ingest wielu dokumentów potrzebuje kolejki z postępem/retry (addon SQLite + scheduler).
8. **Quoty wektorowe sztywne** (10 ns / 1M wek. per addon) — za mało na serwer, za dużo na
   telefon. → quoty per-instancja/profil.
9. **PPR (Personalized PageRank)** — Cozo ma zwykły PageRank, ale nie wektor personalizacji.
   → implementacja PPR w Rust nad snapshotem CSR z Cozo (bounded, seed subgraph).
10. **Entity resolution, walidacja triple'ów, provenance, model konfliktów** (MemGraphRAG) — nowe.
11. **Harness ewaluacji** — recall@k, MRR, faithfulness, latency, throughput ingestu.

## 5. Wybór bazy grafowej: CozoDB

Rust, osadzona jak SQLite, działa wszędzie (serwer / telefon embedded / przeglądarka WASM).
Relacyjno-grafowo-wektorowa w jednym: Datalog, wbudowane algorytmy grafowe (PageRank, shortest
path, community detection), własny HNSW w Rust. Wydajność PageRank: ~50ms (10K/120K), ~1s
(100K/1.7M), ~30s (1.6M/32M). Pamięciowo oszczędna (RAII) — dobra na telefon. Backend storage:
mem / SQLite / RocksDB (mobile: sqlite). Licencja MPL-2.0.

- **Personalized PageRank**: Cozo PageRank nie przyjmuje wektora personalizacji. Rozwiązanie:
  Cozo trzyma graf + robi traversal/seed; **PPR liczymy w Rust** nad CSR wyciągniętym dla
  seed-subgrafu (bounded power-iteration, warm cache, top-N pruning). To zgodne z MemGraphRAG
  §4.3.3 (PPR po inicjalizacji `P_init`).
- Odrzucone: Neo4j/Memgraph (łamią embedded/mobile — tylko adapter serwerowy P2), Kuzu (ryzyko
  buildu mobile — ewentualnie później), własna baza grafowa (marnotrawstwo wobec Cozo),
  petgraph sam (struktura w pamięci, nie baza).
- **Spike GO (potwierdzone uruchomieniem)**: PageRank + HNSW + Datalog działają na backendach
  `mem`/`sqlite`; wasm32 skompilowany; iOS/Android bez blokerów Cozo (tylko toolchain środowiska);
  PPR-w-Rust nad CSR zwalidowany; jedyna natywna zależność = bundled SQLite przez `cc`. Do produkcji
  rozważyć fork `cozo-ce`/`mnestic` (utrzymanie, to samo API). Szczegóły: `RAG_ETAP0_DESIGN.md`.

## 6. Architektura docelowa (warstwy)

```
┌────────────────────────── ADDON RAG (WASM) — LOGIKA + GUI ──────────────────────────┐
│ • PanelShell GUI: kolekcje, upload, status ingestu, chat z cytatami, eksplorator grafu│
│ • Per-instance SQLite: collections, documents, chunks, ingest_jobs, citations, conflicts│
│ • Orkiestracja: składanie i uruchamianie flow (flow_invoke_v1), kolejka jobów          │
│ • Logika MemGraphRAG: prompty ekstrakcji, polityka adjudykacji, fuzja retrievalu        │
│ • Deklaruje aliasy: rag-embeddings / rag-llm / rag-reranker / rag-parse (z fallbackami) │
└───────────────┬──────────────────────────────────────────────────────────────────────┘
                │ host functions + flow_invoke
┌───────────────▼────────── CORE (Rust) — PRYMITYWY/NARZĘDZIA ──────────────────────────┐
│ Flow nodes:  embeddings✅  llm✅  reranker(NOWY)  doc_parse(NOWY)  vector(NOWY)  graph(NOWY)│
│ Host fns:    vector_*✅  sql_*✅  storage_*✅  graph_*(NOWE)  document_*(NOWE)  alias_*✅    │
│ Silniki:     zvec/Milvus✅   CozoDB graf+HNSW(NOWY)   PPR w Rust(NOWY)                    │
│ Aliasy:      resolve + FALLBACK CHAIN(dobudowa)  → mapowanie na realne modele/profil HW │
└───────────────┬──────────────────────────────────────────────────────────────────────┘
                │ service routing (mesh / local)
┌───────────────▼────────── MODELE (vLLM/native, nie NIM) ──────────────────────────────┐
│ LLM nemotron-3 │ embed-1b-v2 │ rerank-1b-v2 │ ocr/parse/page/table/graphic │ paddle-ocr │
└───────────────────────────────────────────────────────────────────────────────────────┘
```

## 6a. Architektura RAG zrewidowana (decyzja właściciela po odkryciu 2 systemów flow)

Repo ma DWA rozłączne systemy flow: `flow_engine` (wizualny FlowBuilder; bogate węzły llm/embeddings/
reranker + `loop_block`/`map_block`/`subflow`/`agent_block`; odpalany JAKO MODEL przez `FlowDispatcher.
try_dispatch`; `ExecutionContext` bez tożsamości addona) oraz `flow_runtime` (operatory source/predict/
branch/threshold/aggregate/sink; MA tożsamość addona w `OperatorContext`; BEZ pętli; używany przez
`flow_invoke_v1`).

Podział RAG (wg wizji właściciela):
- **Ingest (zapis) = ADDON bezpośrednio**: addon woła host-fn narzędzia (doc_parse, embeddings, vector/
  graph upsert) i sam steruje zapisem chunków. Logika ingestu w addonie. (Codex rec: „logika w addonie,
  core daje narzędzia".)
- **Query→odpowiedź = FLOW w `flow_engine`** (FlowBuilder, bo TAM są pętle i węzły): `trigger → [loop:
  embed-query → vector/graph-search → rerank → warunek] → llm → output`. Pętla = wielokrotne dociąganie
  kontekstu (multi-hop RAG). Addon wyzwala swój przypisany query-flow JAKO MODEL (przez istniejący
  `service_request`→`ModelRuntimeExecutor`→`FlowDispatcher`), z aliasami modeli (rag-llm/rag-reranker).
- **Enabler (fundament)**: propagacja tożsamości — `addon_id`/`org_id` callera (CallerContext) przez
  `service_call.rs` → executor → `FlowRequestMeta` (dla `ResolvedExecutionTarget::Flow`) → `ExecutionContext`
  (flow_engine). Bez tego węzły retrievalu w query-flow nie wiedzą, w którą przestrzeń instancji uderzać
  (codex: dziś tożsamość ginie w `ExecutionContext::new(None)`).

Rewizja C-fazy: węzły flow są dla QUERY (flow_engine), NIE dla ingestu. Ingest = host fns. Potrzebne:
enabler tożsamości; węzły `vector_search`/`graph_search` (scoped do instancji); reranker node (C2 ✓);
host fns `doc_parse` + `document store` (ingest). C4 (vector node) wraca PO enablerze (wtedy żywy).

## 7. Plan etapowy

### Etap 0 — Fundamenty w core (odblokowują wszystko)
Cel: prymitywy i wpięcia, bez których addon nie ruszy. Agenci: `programista-rust`.

- **0.1 CozoDB w core**: nowy crate/moduł `services/graph/` z silnikiem Cozo per
  `(org, addon_instance, collection)`, backend sqlite (mobile) / rocksdb (serwer). Cykl życia
  spięty z `addon/lifecycle.rs` (tworzenie/kasowanie przy install/uninstall instancji).
- **0.2 Graph host functions** (`addon/host_functions/graph.rs`): `graph_upsert_node_v1`,
  `graph_upsert_edge_v1`, `graph_query_v1` (Datalog), `graph_neighbors_v1`,
  `graph_pagerank_v1`, `graph_ppr_v1` (seed→wektor personalizacji, liczone w Rust),
  `graph_delete_v1`. Permission `graph.read/graph.write`, audit, quoty per instancja.
- **0.3 Graph flow node** (`flow_engine/node_adapters/graph.rs`) — opcjonalnie; podstawą jest
  host function dla addona.
- **0.4 Fallback chain aliasów**: w `resolve_model_alias_for_addon` + dispatch (LLM/embeddings/
  reranker) — przy niedostępności `target_model` przejdź po `fallback_targets` wg `strategy`
  (`first_available`). Health-check dostępności serwisu/modelu (mesh registry + local).
- **0.5 Reranker flow node** (`node_adapters/reranker.rs`) + klient serwisu rerankera w
  `ctx` (`/v1/rerank`). Wejście: query + kandydaci, wyjście: posortowane score.
- **0.6 Doc-parse flow node** (`node_adapters/doc_parse.rs`) — routuje PDF/obraz przez
  vision services (ocr/parse/page-elements/table-structure/graphic) i zwraca strukturę
  (markdown + bloki tabel/wykresów + provenance: strona/bbox).
- **0.7 Vector flow node** (`node_adapters/vector.rs`) — upsert/search/hybrid jako węzeł flow
  (dziś tylko host function).
- **0.8 Document/blob store host function** (`document_put/get/delete_v1`) nad `BlobStore`,
  per instancja, dla surowych plików (PDF) ponad limitem KV.
- **0.9 Quoty per-instancja/profil** — parametryzacja limitów wektor/graf wg profilu HW.

### Etap 1 — Solidny klasyczny RAG (działa samodzielnie)
Cel: niezawodny, szybki, z cytatami. Agenci: `planer`→`programista-rust`(core gaps z Et.0)→
`programista-frontend`+`programista-rust`(addon)→`code-reviewer`→`tester-*`.

- **1.1 Addon `rag`**: manifest z `[application]` (GUI), per-instance SQLite (migrations:
  `collections`, `documents`, `chunks`, `ingest_jobs`, `citations`), `[[alias]]` rag-embeddings/
  rag-llm/rag-reranker/rag-parse, `[[uses_alias]]`, `[[flow_template]]` ingest/query,
  `[[vector_namespace]]` `passages` (dim = wymiar embeddera).
- **1.2 Flow ingestu**: trigger → doc_parse → chunking (addon block: semantyczny, z overlap,
  provenance) → embeddings(`rag-embeddings`) → vector upsert(`passages`) + zapis chunków do SQL.
- **1.3 Flow zapytania (hybrydowy)**: trigger → embeddings(query) → vector hybrid search
  (dense+sparse) → reranker(`rag-reranker`) → pakowanie kontekstu+cytatów → llm(`rag-llm`)
  streaming → output. Reflection/guardrails opcjonalnie (re-use AiGateway).
- **1.4 GUI**: lista kolekcji, upload (tf-file-input → document_put), pasek postępu ingestu
  (events), chat z cytatami (klik → źródłowy fragment/strona), ustawienia instancji + wybór profilu.
- **1.5 Kolejka jobów ingestu** w addonie (SQLite + retry/cancel, postęp przez events/StatePatch).
- **1.6 Harness ewaluacji** (Etap 1 baseline): recall@k, MRR, faithfulness, latency p50/p95,
  throughput ingestu — zestaw pytań/dokumentów + skrypt.

### Etap 2 — GraphRAG
Cel: wielohopowe rozumowanie, lepsza precyzja. Bazuje na CozoDB + PPR z Etapu 0.

- **2.1 Ekstrakcja triple'ów**: flow/blok ekstraktora (llm `rag-llm`) → encje + relacje +
  provenance (chunk_id, span). Walidacja schematu (typy, dozwolone relacje).
- **2.2 Entity resolution**: kanonizacja aliasów/akronimów/wersji (embedding similarity +
  reguły), merge encji, tabela aliasów + confidence.
- **2.3 Budowa grafu w Cozo**: węzły (entity/type/passage), krawędzie (fakty), provenance,
  tombstones; inkrementalny re-index (content hash, stabilne chunk_id, invalidacja krawędzi).
- **2.4 Retrieval grafowy**: seed z hybrid search → inicjalizacja `P_init` (encje z faktów,
  typy z log-degree penalty, pasaże z information density) → **PPR w Rust** → top-K pasaży +
  top-M encji → reranker → llm. Fuzja z czystym wektorowym retrievalem.
- **2.5 Eksplorator grafu w GUI** (tf-* + wizualizacja).

### Etap 3 — MemGraphRAG (multi-agent, pamięciowy)
Cel: globalna spójność grafu, rozwiązywanie konfliktów (wg PDF, KDD 2026).

- **3.1 Trójwarstwowa pamięć globalna** (`M_ont`/`M_fac`/`M_pas`): schematy z częstotliwością
  (Ontology), fakty (Fact), pasaże (Passage); dense indexing (schema-instance alignment,
  fact-evidence grounding) w Cozo.
- **3.2 Probabilistyczny protokół ekstrakcji**: schematy „Candidate"→„Stable" po progu
  częstotliwości `τ` (Unified Schema Filtering — denoising tematyczny).
- **3.3 Agent detekcji konfliktów** (`A_det`): asynchroniczny skan przy aktywacji nowego
  faktu — similarity + symbolic match → zbiór konfliktów (mutually exclusive / temporal /
  granularity).
- **3.4 Agent rozwiązywania** (`A_res`): evidence-driven adjudykacja z `M_pas` + ontologią
  (Global Adjudication); batchowanie + cache; eskalacja high-impact do człowieka (GUI:
  panel konfliktów do zatwierdzenia).
- **3.5 Memory-guided retrieval**: 3-warstwowe filtrowanie pamięci → structure-aware
  init → PPR (re-use z Etapu 2) → generacja.
- **3.6 Ewaluacja**: porównanie z baseline RAG/GraphRAG na zestawie wielohopowym.

## 8. Najtwardsze ryzyka (korektność + wydajność)

- **Entity resolution dominuje korektność** — błędna kanonizacja truje graf. Confidence,
  reguły + embedding, ręczna korekta w GUI.
- **Halucynowane triple'e są zabójcze** — każdy fakt z provenance, confidence, wersją
  ekstraktora, source span, odwracalnym kasowaniem (tombstone).
- **Koszt adjudykacji konfliktów** — agenci LLM na każdym ingeście wolni/drodzy: batch, cache,
  człowiek tylko do high-impact.
- **Latencja PPR** — bounded seed subgraph, precompute adjacency, top-N pruning, warm cache,
  inkrementalna inwalidacja.
- **Inkrementalny indeks trudniejszy niż pełny** — content hashe, stabilne chunk_id, tombstones,
  invalidacja krawędzi grafu, polityka re-embed.
- **Izolacja multi-tenant nie może być „po nazwie"** — każdy klucz tabeli/indeksu/cache zawiera
  `org/addon_instance/collection`. Uninstall = jedna operacja kasująca wektory+graf+SQL+blob.
- **Koszt rerankera** — cross-encoder na 200 kandydatach potrafi być wąskim gardłem przed
  generacją: limit kandydatów, batch.
- **Fallback aliasów** — bez health-checku failover może maskować ciche degradacje jakości
  (log + metryka, które realne modele/profil użyto).

## 9. Podział na agentów (zgodnie z workflow PM)

- `planer` — architektura każdego etapu, kontrakty host functions/flow.
- `programista-rust` — core (CozoDB, graph host fns, PPR, fallback aliasów, węzły flow,
  document store).
- `programista-frontend` + `programista-rust` (WASM) — addon (GUI CBOR + logika).
- `programista-bazy-danych` — schematy SQLite addona + Datalog/Cozo.
- `optymalizator-rust` — PPR, reranker batch, latencja retrievalu.
- `code-reviewer` — OWASP + izolacja multi-tenant.
- `tester-jednostkowy` + `tester-e2e` — izolacja 2 instancji, ingest→retrieve→answer, harness.
- `dokumentator` — CLAUDE.md + docs per etap.

## 10. Rekomendowana kolejność startu

Etap 0 (0.1 CozoDB + 0.2 graph host fns + 0.4 fallback aliasów + 0.5 reranker node +
0.6 doc_parse node) jest na ścieżce krytycznej — bez tego addon nie ma narzędzi. Start od
`planer` dla Etapu 0, równolegle `programista-rust` na CozoDB i fallback aliasów.
