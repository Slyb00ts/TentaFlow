# Etap 0 RAG — projekt fundamentów w core (kontrakty)

Dokument projektowy gotowy dla `programista-rust`. Kontrakty dopasowane do realnych wzorców w
repo (cytowane ścieżki). Zasada: core daje prymitywy, izolacja zawsze po `(org_id, addon_id, ...)`,
CBOR I/O, audit na każdej ścieżce wyjścia, zero stubów. Nadrzędny plan: `RAG_ARCHITECTURE_PLAN.md`.

## STATUS REALIZACJI

- **Slice A2 = 0.1 CozoDB w core: ZROBIONE, codex GO** (po 5 rundach napraw + 6 review). 32 testy
  zielone (w tym współbieżne 3×), domyślny build (bez `graph`) nietknięty, clippy czysty w
  `services/graph`. Backend `sled` (sqlite kolidował z rusqlite), wasm→`mem` (target-gated), serwer
  opcjonalnie rocksdb. Wzorce: per-collection `RwLock` + manager-owned lifetime (bez wyciekania
  `Arc`), atomowy ledger rezerwacji quoty (`BEGIN IMMEDIATE`), `BackendSlot::Removed` + bounded
  re-fetch (64 + `force_open`), ścieżka deterministyczna z klucza, intern-first serializacja
  cold-create vs delete, MAX_OPEN_GRAPHS (4 mobile / 16 serwer) z realnym zamykaniem przy eviction.
  Nie commitowane (czeka na decyzję PM). Drobny nit codex: `remove_if` cleanup bierze shard-then-slot
  — brak przeciwnej żywej ścieżki, do pilnowania przy przyszłych zmianach.
- **Slice A1 = 0.4 fallback aliasów: ZROBIONE, codex GO** (po 1 NO-GO + GO-WITH-CHANGES). Kluczowa
  korekta: NIE budować równoległego resolvera — addonowy `service_request` przepuszczony przez ISTNIEJĄCY
  `ModelRuntimeExecutor`/`AliasResolver` (failover po kandydatach, embedded-accepted, mesh-forward,
  dispatch po tożsamości). Metryka `alias_fallback_total{alias}` + warn w jednym punkcie pętli failoveru
  (liczy /v1+flow+addon, tylko aliasy). `log_alias_call` z realnym `target_used`/`fallback_used`/pozycją
  z `route_metadata.served_model`. Usunięty błędny `alias_resolver.rs`. 40 testów w dotkniętych modułach.
- **Dług pre-existing (NIE A1, do osobnej naprawy):** testy `db::repository::alias_resolve_tests` (i pokrewne)
  failują `no such table: model_alias_{owners,changes,aliases}` — testowy harness DB nie aplikuje migracji
  tych tabel (migracje istnieją, np. `model_alias_changes` migrations.rs:3998). A1 nie dotyka `db/`/migracji.
- **Slice B1+B2 = 0.2 graph host functions + uninstall cleanup: ZROBIONE, codex GO** (po 2 NO-GO +
  decyzja właściciela). KLUCZOWA decyzja: **raw `graph_query` (Datalog od addona) USUNIĘTY** — niebezpieczny
  (compute DoS, obejście gramatyki, tombstone/alive leak). Addon dostaje TYLKO bezpieczne prymitywy:
  upsert_node/edge, neighbors, pagerank (cap iter 100), ppr (cap iter + 64 seedy), delete=tombstone.
  Parametry clampowane host-side, cap współbieżności (globalny 8 + per-addon 2, RAII, fail-closed),
  tombstone/alive filtrowane joinem wszędzie, uninstall files-before-row (jedna ścieżka przez
  `delete_all_for_addon`). Workaround: Cozo 0.7.6+sled nie honoruje `:rm` → delete = tombstone (`alive=false`),
  fizyczny purge = późniejsza kompakcja. 18 host-fn + 638 sdk-spec + 32 A2 zielone.
- **Dług pre-existing (NIE nasze):** testy `:memory:` masowo czerwone na branchu (`flows`/alias tabele
  „no such table" na świeżym `db::init(":memory:")`) — harness `:memory:`+pool czyta osobne puste połączenie;
  migration runner ma transakcję PER-migrację (nie all-or-nothing), a `flows` to wczesna tabela → nasze
  v85/v86 NIE mogą tego powodować (pliki-bazowane testy grafu przechodzą pełny łańcuch). Do naprawy osobno.
- **Slice C2 = 0.5 reranker flow node: ZROBIONE, codex GO** (po GO-WITH-CHANGES). Węzeł `reranker`
  (`{query,candidates}`→`{ranked}`, cap 200/top_n), `RerankDispatcher` + `execute_rerank` reuse pętli
  failoveru A1 (alias `rag-reranker`, metryka w jednym punkcie). Naprawiony recursion-hole: głębokość
  flow propagowana przez `new_with_flow_depth` (guard `MAX_FLOW_DEPTH=3` narasta przy re-wejściu) — TEN
  SAM fix dla embeddings; resolver pomija Embedded dla surface Rerank.
- Następne: C4 (vector node + addon_id w ExecutionContext), C3 (doc_parse node nad vision services),
  C1 (document/blob store per-instancja), D2 (quoty per-profil HW).
- ⚠️ Buildy: TYLKO `/mnt/d` (CARGO_TARGET_DIR/TMPDIR/HOME), NIE /tmp ani /mnt/e (sieciowy). Wąskie testy.

## STATUS QUERY-FLOW (architektura zrewidowana — patrz RAG_ARCHITECTURE_PLAN §6a)

- **C2 reranker flow node** ✅ (codex GO) — dla query-flow + FlowBuilder.
- **E1.0 enabler tożsamości + vector node** ✅ (codex GO-WITH-CHANGES→fix). `addon_id`/`org_id` callera
  propagowane z `service_call.rs` → executor (4 gałęzie Flow) → `FlowRequestMeta` → `flow_engine::
  ExecutionContext`; węzeł `vector` (upsert/search/hybrid) scoped do instancji, walidacja-przed-zapisem.
  Addon wyzwala query-flow JAKO MODEL (`service_request`), tożsamość dociera do węzłów retrievalu.
- Następne: `graph_search` node (neighbors/ppr scoped do instancji) → host-fn `doc_parse` + `document store`
  (ingest) → Etap 1 (addon RAG + flow templates + GUI).
- **Hardening (nie blocker):** PK `addon_vector_namespaces` to `(addon_id, namespace)` bez `org_id` —
  bezpieczne bo `instance_id` globalnie unikalny + runtime kluczuje po org wszędzie; do migracji na
  `(org_id, addon_id, namespace)` przy okazji (graf już ma poprawny PK).

## Spike CozoDB — wynik: GO (potwierdzone realnym uruchomieniem)

- `cozo` 0.7.6, MPL-2.0. Do produkcji rozważyć utrzymywany fork `cozo-ce` 0.7.13 lub `mnestic`
  0.8.6 (to samo API/Datalog). Backendy: `mem` (zawsze), `storage-sqlite(-src)` (mobile, bundled
  SQLite przez `cc`), `storage-rocksdb` opcjonalnie na serwer (C++ — tylko nie-mobile).
- Zadziałały realnie: Datalog graph query, wbudowany **PageRank**, **HNSW** vector search.
- **PPR NIE jest wbudowany** (potwierdzone w źródle `fixed_rule/algos/pagerank.rs` — brak wektora
  personalizacji) → liczymy w Rust nad CSR z Cozo (zwalidowane w spike).
- Cross-platform: serwer GO; wasm32 skompilowany (mem backend, wymaga spójnej konfiguracji
  `getrandom` js/wasm_js — tej samej co dashboard); iOS/Android GO warunkowo (padło tylko na braku
  iOS SDK / Android NDK w środowisku, nie na ograniczeniach Cozo — zero C++/cmake/RocksDB na mobile).
- Build lekki: 27s clean, jedyna natywna zależność to bundled SQLite przez `cc` (jak nasza obecna ścieżka).
- Izolacja: osobny `DbInstance` na plik per `(org, addon_instance, collection)`; uninstall = skasowanie pliku.
- Spike: `/mnt/e/repos/rust/_scratch/cozo-spike/` (Cargo.toml + src/main.rs + spike.db).

## Konwencje wspólne (z `host_functions/vector.rs`)

- ABI host fn: `(input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32`.
- Kolejność entry-point: `get_memory` → `check_permission` (przed dekodowaniem) → `read_input_cbor`
  → walidacja → dispatch → `audit` → `write_cbor_capped`.
- Struktury I/O w `tentaflow-sdk-spec` (minicbor `Decode/Encode`), reuse `FieldValue`/`Filter`.
- `org_id` z `caller.data().org_id` (fallback `DEFAULT_ORG_ID`); `addon_id == instance_id`.
- Błędy przez `map_*_error(e) -> (AbiError, &'static str)`.

## 0.1 CozoDB w core — `services/graph/`

Nowy moduł `tentaflow-core/src/services/graph/{mod.rs,error.rs,collection.rs,backend.rs,ppr.rs}`
analogicznie do `services/vector/`. `graph_manager(db)` singleton (lustro `vector_namespace_manager`).

- Klucz: `GraphKey { org_id, addon_id, collection }`. Walidacja nazw reuse
  `services::vector::namespace::{validate_org_id,validate_addon_id,validate_namespace_name}`.
- Ścieżka: `<HOME>/.tentaflow/orgs/<org>/addons/<addon>/graph/<collection>.cozo`. `with_root` dla testów.
- Backend: serwer `rocksdb` (feature), mobile `sqlite`; brak feature rocksdb ⇒ fallback `sqlite` (NIE błąd).
- Migracja `addon_graph_collections` (lustro `addon_vector_namespaces`): kolumny addon_id, collection,
  org_id, file_path, engine, node_count, edge_count, created_at, updated_at; PK (addon_id, collection).
- Wewn. schemat Cozo per kolekcja (`:create`): `nodes{id => label, props, provenance, ts}`,
  `edges{src, rel, dst => props, provenance, weight, ts}`, HNSW na nodes (2.x).
- Cykl życia: tworzenie leniwe przy pierwszym `graph_*` (jak vektory). Kasowanie w
  `uninstall_instance` (`lifecycle.rs:198`) + `delete_all_for_addon`; dopisać `addon_graph_collections`
  do listy `tables` w `uninstall` (`lifecycle.rs:1394`); `invalidate_addon` w `materialize_addon_derived_state`.
- Quoty: `MAX_COLLECTIONS_PER_ADDON`, `MAX_NODES_PER_ADDON`, `MAX_EDGES_PER_ADDON`; przesłaniane per
  profil HW przez `addon_resource_limits` (kolumny `graph_nodes_max`, `graph_edges_max`).

## 0.2 Graph host functions — `addon/host_functions/graph.rs`

Wzór 1:1 z `vector.rs`. Permissions `graph.read`/`graph.write`, audit `RiskClass::B`. Manifest
`[[graph_collection]]` (`GraphCollectionSpec { name, data_class, gate }`) obok `VectorNamespaceSpec`;
addon MUSI zadeklarować kolekcję (blokuje ad-hoc).

Struktury CBOR (sdk-spec): `GraphNode{id,label,props:Map<String,FieldValue>,provenance}`,
`Provenance{chunk_id,doc_id,page,span,confidence,extractor_version}`, oraz I/O dla 7 funkcji:
`graph_upsert_node_v1`(W), `graph_upsert_edge_v1`(W), `graph_query_v1`(R, Datalog), `graph_neighbors_v1`(R),
`graph_pagerank_v1`(R), `graph_ppr_v1`(R, seeds=wektor personalizacji, liczone w `ppr.rs`),
`graph_delete_v1`(W, z wariantem Tombstone=soft delete).

Bezpieczeństwo `graph_query_v1` (krytyczne): Datalog od addona niezaufany — tryb read-only Cozo
(`ScriptMutability::Immutable`), odrzucenie tx-keywordów (`:put/:rm/:create/:replace/:ensure`),
jeden plik=jedna kolekcja=izolacja fizyczna, twardy `limit` niezależny od skryptu.
`PayloadKind::GraphItem` w `abi_helpers.rs` (ceiling ~256 KiB; SDK retry na `OutputBufferTooSmall`).
Rejestracja w `host_functions/mod.rs` (blok jak vector); SDK wrappery `graph_*`.

## 0.4 Fallback chain aliasów

Stan: `resolve_model_alias_for_addon` (`repository.rs:1358`) zwraca `target_model` + `fallback_targets`
(JSON) + `strategy`, ale NIE failuje. `alias_calls` ma już kolumny `fallback_used`,
`fallback_chain_position` (`migrations.rs:3933`) — nieużywane.

Nowy `services/alias_resolver.rs`: `resolve_available_target(db, alias_row, method, health) ->
ResolvedTarget{model, service_id, node_id, chain_position, is_fallback}`. Algorytm `first_available`:
kandydaci `[target_model] + fallback_targets` (reuse `collect_chain_candidates` `repository.rs:2746`),
pierwszy dostępny wg `ModelHealth::is_model_available` (local: `services` Running/Degraded+endpoint;
mesh: widoczny serwis). Cache `DashMap<(model,method),(bool,Instant)>` TTL ~5s + invalidacja przy
zmianie statusu serwisu.

Wpięcie: `host_functions/llm.rs`, `service_call.rs` (addon ABI), oraz flow dispatchers
(Embeddings/Llm/nowy Rerank) — gdy model to alias, rozwiąż przez `resolve_available_target`.
`log_alias_call` (`service_call.rs:711`) wypełnia realne `fallback_used`/`fallback_chain_position`/
`target_used`/`service_id`. OBOWIĄZKOWO: `warn!` + metryka `alias_fallback_total{alias}` (anty-ciche-degradacje).

## 0.5/0.6/0.7 Węzły flow (trait `NodeAdapter`)

`ExecutionContext` (`node_adapter.rs:82`) NIE niesie `addon_id` — DODAĆ (C0) zanim ruszą vector/graph
node, plus `+reranker`, `+vectors`; aktualizować `test_support::stub_ctx` (`:662`).

- **0.5 reranker** (`node_adapters/reranker.rs`): in `Json{query,candidates}` → out `Json{ranked}`.
  Nowy `RerankDispatcher` w `ExecutionContext` (`/v1/rerank`, alias-aware ⇒ 0.4). config: model
  (`rag-reranker`), top_n, batch_size (kontrola wąskiego gardła cross-encodera).
- **0.6 doc_parse** (`node_adapters/doc_parse.rs`): in `Any` (Image lub BlobRef PDF) → out
  `Json{markdown, blocks:[{type,content,page,bbox,confidence}], provenance}`. Reuse/rozszerz
  `VisionDispatcher` (`node_adapter.rs:139`) o tryby ocr/parse/page/table/graphic. config: mode, model
  (`rag-parse`), max_pages. Provenance (strona/bbox) obowiązkowe (cytaty Etap 1).
- **0.7 vector** (`node_adapters/vector.rs`): opakowuje `NamespaceManager`; `op=upsert/search/hybrid`.
  Dodać `pub vectors: Arc<NamespaceManager>` + `addon_id` do `ExecutionContext`. Quoty przez `upsert_with_quota`.

## 0.8 Document/blob store — `addon/host_functions/document.rs`

Nad `flow_engine/blob_store.rs` BlobStore, dla surowych PDF > limit KV (1MB). Permissions
`document.read/write`, audit `RiskClass::B`. Chunked put przez `read_guest_bytes` (NIE base64 całości):
`document_put_v1`(chunk_index/total), `document_get_v1`, `document_delete_v1`. Izolacja: per-instance
`FileBlobStore` root `orgs/<org>/addons/<addon>/documents/` + rejestr w per-addon SQLite (`documents`);
kasuje się z `addon_data_dir` w `uninstall_instance`. Limit per instancja: kolumna
`addon_resource_limits.document_storage_mb`. `doc_parse` (0.6) czyta PDF przez `ctx.blobs`.

## Kolejność i zależności

```
Faza A (równolegle): A1=0.4 fallback aliasów | A2=0.1 CozoDB
Faza B (po A2):      B1=0.2 graph host fns   | B2=uninstall cleanup grafu
Faza C (po C0):      C0=ExecutionContext(+addon_id,+reranker,+vectors) | C1=0.8 document | C2=0.5 reranker | C3=0.6 doc_parse | C4=0.7 vector
Faza D:              D1=0.3 graph flow node (opc.) | D2=0.9 quoty per-profil
```
Ścieżka krytyczna: A1+A2 → B1 → C2/C3. C0 to wspólny bloker węzłów — najpierw.

## Pliki

Utworzyć: `services/graph/{mod,error,collection,backend,ppr}.rs`, `services/alias_resolver.rs`,
`addon/host_functions/{graph,document}.rs`, `flow_engine/node_adapters/{reranker,doc_parse,vector}.rs`.
Zmienić: `Cargo.toml` (cozo+feature graph), `services/mod.rs`, `db/migrations.rs`, `db/repository.rs`,
`services/service_call.rs`, `addon/host_functions/mod.rs`, `addon/host_functions/{llm,abi_helpers}.rs`,
`addon/manifest.rs`, `addon/lifecycle.rs`, `flow_engine/node_adapter.rs`, `flow_engine/dispatchers.rs`,
bootstrap rejestru adapterów, `tentaflow-sdk-spec`, `addon-sdk`.

## POPRAWKI PO REVIEW CODEX (OBOWIĄZKOWE — werdykt GO-WITH-CHANGES)

Codex zweryfikował projekt wobec realnego kodu. Przed implementacją zastosować:

1. **Sandbox `graph_query_v1` — `ScriptMutability::Immutable` NIE wystarcza.** Cozo nadal parsuje
   sys-ops (`::running`, `::kill`, `::relations`, `::columns`); `::kill` truje zapytania nawet w
   read-only (`cozo-0.7.6/src/runtime/db.rs:1368`, `parse/sys.rs:112`). Filtrowanie keywordów stringiem
   za słabe. Wymagane: parser/whitelist dozwolonych form zapytań, **odrzucenie WSZYSTKICH `::` sys-ops**,
   odrzucenie skryptów imperatywnych, ograniczenie referowanych relacji do schematu kolekcji.
2. **`limit` NIE chroni przed DoS CPU/RAM.** Cozo stosuje timeout tylko gdy skrypt o niego prosi, a na
   `wasm32` timeout nie działa (`db.rs:1490`, `:1920`). `limit` nie ogranicza joinów/rekurencji/PageRank/
   HNSW/fixed-rules. Dodać twarde budżety hosta: max bajtów skryptu, max params, max skanowanych/zwracanych
   wierszy, max runtime, max równoległych graph-calls per addon.
3. **Schemat: PK MUSI zawierać `org_id`.** Projekt mówi klucz `(org, addon, collection)`, a migracja
   miała PK tylko `(addon_id, collection)` — błąd dla multi-org. Vector używa org w runtime
   (`namespace.rs:290`). Poprawić PK/indeksy `addon_graph_collections` na `(org_id, addon_id, collection)`.
4. **`validate_org_id` jest PRYWATNA** (`namespace.rs:142`) — „reuse" niemożliwe wprost: upublicznić
   albo świadomie zduplikować regułę. `validate_namespace_name`/`validate_addon_id` są publiczne.
5. **Uninstall NIE jest kompletny jak zakładał projekt.** Generyczna lista `tables` w `uninstall`
   (`lifecycle.rs:1394`) dziś NIE zawiera nawet `addon_vector_namespaces`; nie istnieje żadne
   `delete_all_for_addon`. Dla grafu dodać JAWNE: cleanup DB + invalidacja cache (DashMap) + kasowanie
   pliku. NIE polegać tylko na `remove_dir_all(addon_data_dir)`. (Przy okazji rozważyć dopisanie
   `addon_vector_namespaces` do cleanup — istniejąca luka.)
6. **Fallback aliasów: cache 5s wymaga invalidacji zdarzeniowej + jitter/histereza.** Samo polling 5s
   będzie „flapować" przy deploy/restart i trzymać nieświeży stan „available". Invalidacja przy zmianie
   statusu serwisu obowiązkowa.
7. **`doc_parse`: obecny `VisionDispatcher` robi TYLKO OCR/classify nad RGB** — nie PDF/parse/table/
   page/graphic. To NOWA powierzchnia dispatchu (nowy trait/serwis), nie mały adapter. Przeszacowanie
   pracy w 0.6 — potraktować jak osobny dispatcher.
8. **Document store: NIE da się zrobić samym opakowaniem `BlobStore`.** `FileBlobStore` jest
   content-addressed po sha pod jednym globalnym rootem (`<TENTAFLOW_HOME>/blobs`,
   `blob_store.rs:76`); trwały per-ref delete jawnie nierozwiązany (`blob_store.rs:31`). Per-instancja
   wymaga: rejestru dokumentów + refcount LUB osobnego per-instance store, ORAZ autoryzacji na
   `BlobRef.id/sha` (inaczej addon z refem może deref/utrzymać cudzy blob). Kasowanie współdzielonych
   plików sha nie może korumpować innych właścicieli.
9. **Kolumny limitów NIE istnieją.** `addon_resource_limits` ma `storage_limit_mb`, `fuel_limit`, ale
   NIE `document_storage_mb`/`graph_nodes_max`/`graph_edges_max` (`migrations.rs:4510`). Najpierw migracja
   dodająca kolumny, dopiero potem egzekwowanie (albo reuse istniejących).
10. **Cozo GO za beztroskie.** Trzymać się `sqlite` backend na start; RocksDB tylko serwer/później;
    pilnować utrzymania (0.7.6 porzucony → fork `cozo-ce`/`mnestic`), braku timeoutu na wasm i wielu
    wersji `getrandom` w lockfile spike'a.

Kolejność uwzględnia poprawki: do Fazy A/B dołożyć migracje kolumn (pkt 9) i poprawny PK (pkt 3)
PRZED host fns; sandbox Datalog (pkt 1-2) jest częścią 0.2, nie dodatkiem.

## DECYZJA: raw graph_query USUNIĘTY z Etapu 0 (2× NO-GO codex — raw Datalog niebezpieczny)

Surowy Datalog od addona przecieka za każdym razem (compute DoS, obejście gramatyki przez reguły-
pomocnicze, tombstone filtr błędny, alive-leak). Decyzja właściciela: **usunąć `graph_query_v1` (raw
Datalog) z Etapu 0**; addon dostaje TYLKO bezpieczne, host-kontrolowane prymitywy. Raw/strukturalne
zapytania odłożone na później (za adminem / osobny slice z kompilatorem zapytań nad żywym widokiem).

Rework B1+B2 (finalny):
1. **USUŃ `graph_query_v1`** całkowicie: host fn, rejestracja, SDK wrapper, CBOR `GraphQuery*`,
   `validate_query` (gramatyka/blacklist), watchdog/`run_immutable_budgeted` jeśli służył tylko query,
   host-side tombstone filter zapytania, manager `query_params`/backend `run_query_params` jeśli używane
   tylko przez raw query. Usuń martwy kod i testy raw-query. (Wewnętrzne, host-budowane zapytania
   neighbors/pagerank/ppr/export ZOSTAJĄ — są bezpieczne, nie przechodzą przez addona.)
2. **Bezpieczne prymitywy ZOSTAJĄ**: upsert_node/edge, neighbors (bounded depth 1..=3), pagerank
   (CAP iteracji host-side), ppr (CAP iteracji + CAP liczby seedów), delete=tombstone. Wszystkie filtrują
   alive/tombstone (już zrobione joinem) i capują parametry — addon nie kontroluje kształtu zapytania.
3. **Cap współbieżności na funkcjach liczących** (pagerank/ppr/neighbors): globalny + per-addon licznik
   in-flight (np. AtomicUsize/semafor), acquire PRZED pracą, release po; saturacja → fail-closed błąd
   rate-limit/quota. Bez tego addon odpala N ciężkich pagerank równolegle → CPU exhaustion.
4. **Uninstall atomowy(-ie spójny)**: kolejność close-handle → delete pliki → delete wiersz rejestru
   (wiersz znika DOPIERO gdy pliki skasowane, więc fail zostawia wiersz → retry działa, brak orphan-files
   bez wiersza). `delete_all_for_addon` zbiera błędy i propaguje; uninstall przerywa przed remove_dir.

Cel GO: zero ścieżki raw-Datalog od addona; ciężkie funkcje capowane parametrami + współbieżnością;
tombstone/alive niewidoczne nigdzie; uninstall spójny.

## KOREKTA B1+B2 (NO-GO codex — sandbox Datalog + delete + cleanup) [HISTORYCZNE — zastąpione decyzją powyżej]

Ustalenie upraszczające: **addony wykonują się w NATYWNYM core (wasmtime host), nie w przeglądarce**
— browser-wasm to tylko dashboard. Więc graph host fns zawsze idą przez natywne Cozo, gdzie host MOŻE
wymusić timeout. To czyni host-injected timeout poprawnym budżetem na wszystkich celach (serwer+telefon).

Rework (wymagane przed akceptacją):

1. **Budżet obliczeniowy graph_query (CRIT #1/#2):**
   - `validate_query` ODRZUCA fixed-rules (`<~`) i user-defined reguły rekursywne (te dają
     `PageRank(iterations:1e9)` / transitive-closure). Dozwolone tylko NIEREKURENCYJNE koniunkcyjne
     odczyty nad `*nodes`/`*edges` (filtry, projekcje, join nodes↔edges). Blacklist `::`/tx zostaje jako
     defense-in-depth, ale to NIE jedyna bariera.
   - **Host WSTRZYKUJE twardy `:timeout N` do KAŻDEGO wykonywanego zapytania** (np. 2s). ZWERYFIKUJ
     realnym testem, że Cozo faktycznie ABORTUJE zapytanie typu iloczyn-kartezjański w ~timeout (nie
     wisi) — jeśli Cozo nie przerywa wewnątrz wielkiego joina, dołóż watchdog/ograniczenie arności joinu.
   - Zostają: cap bajtów skryptu, cap wierszy wyniku, cap params. Udokumentuj budżety i dozwoloną gramatykę.

2. **Delete = tombstone, filtrowany JEDNOLICIE (CRIT #3/#4):**
   - `graph_delete_v1` = **tombstone (O(1) `:put` etykiety)**; USUŃ z host-path hard-delete przez
     `:replace` całej relacji (O(N+E), nieatomowy, + bug `:rm` na sled). Fizyczny purge odłożony do
     późniejszej operacji kompakcji (batch, jeden `:replace`).
   - **WSZYSTKIE ścieżki retrievalu wykluczają tombstone**: graph_query (auto-filtr `label != tombstone`
     wstrzykiwany przez host), neighbors, pagerank, ppr, export_csr — węzły tombstone ORAZ ich incydentne
     krawędzie nie wchodzą do wyniku/obliczeń. Test: po tombstone węzeł znika z query/neighbors/pagerank/ppr.

3. **Uninstall cleanup (MED #5):** błąd `delete_all_for_addon` NIE może być połknięty — propaguj/surface
   (fail uninstall albo twardy retry) przed usunięciem katalogu/wierszy; upewnij się że uchwyty zamknięte.

### Rozstrzygnięcie po implementacji (B1+B2 rework)

**Budżet czasu — POTWIERDZONE empirycznie, że samo `:timeout` NIE wystarcza.** Cozo 0.7.6
sprawdza poison-pill (`:timeout`) TYLKO między etapami rule-eval (`query/eval.rs`: `poison.check()`
po pełnym przebiegu `rule.relation.iter`), a NIE wewnątrz materializacji pojedynczego joina —
ciężki spięty join (np. 3-hop ścieżka na klice) finiszuje DALEKO za budżetem zanim poison
zapali. Dlatego budżet to TRZY warstwy:
1. **Gramatyka sandboxa** (`validate_query`): odrzuca fixed-rules (`<~`), reguły rekurencyjne
   (cykl w grafie zależności reguł), join kartezjański (atomy relacji niespięte wspólną zmienną)
   oraz **cap arności joinu** `MAX_RELATION_ATOMS_PER_RULE = 3` (≥4 atomy relacji w regule → reject).
2. **`:timeout` wstrzykiwany przez host** (`QUERY_TIMEOUT_SECS = 2s`) — tnie rekurencję/wieloetapowe
   zapytania (defense-in-depth).
3. **Watchdog wall-clock** (`backend.rs`): zapytanie biegnie na ODDZIELNYM wątku, host czeka co
   najwyżej `QUERY_TIMEOUT_SECS + QUERY_WATCHDOG_GRACE_SECS` (≈3s) i zwraca `GraphError::QueryTimeout`.
   To TWARDA gwarancja, że host nie wisi nawet, gdy pojedynczy join materializuje się jednym
   przebiegiem (cap arności trzyma wątek-uciekiniera skończonym i krótkim). Na wasm32 (brak wątków/
   `:timeout`) jedyną barierą jest gramatyka. Test `e2e_query_timeout_aborts_heavy_query` dowodzi,
   że host wraca ~przy budżecie (nie rośnie z rozmiarem joina).

**Dozwolona gramatyka graph_query:** nierekursywne koniunkcyjne ODCZYTY nad `*nodes`/`*edges`
(filtry, projekcje, join nodes↔edges po wspólnych zmiennych), opcje `:limit`/`:order`/`:offset`,
≤3 atomy relacji na regułę. Pagerank/PPR/neighbors/głębsze trawersy idą przez dedykowane host-fn.

**Delete = soft-delete (O(1)).** `delete_node` → tombstone (label = `__tombstone__`, `:put`),
`delete_edge` → `alive=false` (`:put` na tym samym kluczu). Hard-delete przez `:replace` całej
relacji USUNIĘTY. Wszystkie ścieżki retrievalu wykluczają tombstone: `neighbors`/`pagerank`/
`export_csr` joinem z nie-tombstone węzłami + `alive==true`; `graph_query` host-side filtrem po
zbiorze tombstone'owanych id (`tombstoned_ids`). Fizyczny purge = późniejsza kompakcja.

**Uninstall cleanup** propagowany: `uninstall_instance` PRZERYWA z błędem, gdy
`delete_all_for_addon` zawiedzie (przed `remove_dir_all`); uchwyty sled zamknięte w
`seal_key_for_delete` (slot → `Removed`).

## KOREKTA A1 (NO-GO codex — reuse istniejącego resolvera, NIE budować równoległego)

Repo MA JUŻ availability-aware failover aliasów: `tentaflow-core/src/services/runtime/resolver.rs`
(`AliasResolver`, zwraca dispatchowalny `ResolvedExecutionTarget` z tożsamością embedded/local/remote-node
+ capability + status) używany przez `runtime/executor.rs` (`execute_chat`/`execute_embeddings` z pętlą
po kandydatach `fallback_targets`). `/v1` (`routing/chat.rs`,`routing/embeddings.rs`) i flow
(`flow_engine/dispatchers_impl/{llm,embeddings}_impl.rs`) failują przez TĘ ścieżkę. Realna luka: addonowy
`service_call.rs::service_request` rozwiązywał alias tylko jako bramkę i dispatchował SUROWĄ NAZWĘ.

BŁĄD pierwszej implementacji A1: dodano RÓWNOLEGŁY `services/alias_resolver.rs` — który (a) odrzuca
serwisy embedded (wymaga `endpoint_url` — model in-process na telefonie go nie ma → łamie cel serwer→telefon),
(b) gubi `node_id`/`service_id` przy dispatchu (fallback trafia do innego właściciela modelu),
(c) dubluje i rozjeżdża się z istniejącym resolverem (status/capability/identity), (d) split-probe
race (is_available z histerezy true, locate current false). Narusza „sprawdź istniejące przed pisaniem
nowych".

POPRAWNY KIERUNEK (rework A1):
1. **USUNĄĆ `services/alias_resolver.rs`** (cały równoległy resolver + cache histereza + invalidacja —
   zbędne: istniejąca ścieżka failuje na kandydatach w momencie dispatchu, brak nieświeżego cache).
2. **Przepuścić `service_call.rs::service_request` przez istniejący `AliasResolver`/`ModelRuntimeExecutor`**:
   dla aliasu llm/embeddings rozwiąż na `ResolvedExecutionTarget` (embedded/local/remote) i dispatchuj
   PO TEJ TOŻSAMOŚCI (mesh-forward/embedded), nie po nazwie modelu. To daje addonom ten sam failover co
   /v1+flow, z obsługą embedded i właściwego węzła.
3. **`log_alias_call`** wypełnia realne `fallback_used`/`fallback_chain_position`/`target_used`/`service_id`/
   `target_node_id` z `ResolvedExecutionTarget` istniejącej ścieżki (nie z równoległego resolvera).
4. Metryka `alias_fallback_total{alias}` + `warn!` przy fallbacku — dopiąć do ISTNIEJĄCEJ ścieżki failoveru
   (jeden punkt liczenia dla /v1+flow+addon), nie osobno.
5. Jeśli `service_request` obejmuje metody spoza executor (tts/stt/inne) — dla nich reuse tej samej
   `AliasResolver` do rozwiązania targetu i dispatch po tożsamości; nie wprowadzać drugiej definicji „available".

Cel akceptacji: addonowy alias z fallbackiem do modelu EMBEDDED (telefon) realnie failuje na embedded,
dispatch trafia we właściwy backend/węzeł, log pokazuje realny użyty target. Zero drugiego resolvera.

## MACIERZ BACKENDÓW COZO (rozstrzygnięte po implementacji A2)

Cozo `storage-sqlite` linkuje `sqlite3` (`links="sqlite3"`) i KOLIDUJE z naszym `rusqlite`
(`libsqlite3-sys`, też `links="sqlite3"`) — potwierdzone też dla forka `mnestic`. Dlatego:

- **Serwer/desktop (Linux/Win/macOS natywnie)**: domyślnie `sled` (czysto-Rust, bez konfliktu
  linkera); opcjonalny feature `graph-rocksdb` dla dużych grafów.
- **Mobile (Android/iOS natywnie)**: `sled`, z DOSTROJONĄ konfiguracją (mały `cache_capacity` per
  baza, NIE domyślny 1 GiB; rozsądny `flush_every_ms`).
- **wasm32 (dashboard w przeglądarce)**: TYLKO `mem` (sled NIE kompiluje się na wasm — fs2/libc/mmap).
  Persystencja w przeglądarce odłożona (snapshot/export). sled MUSI być `#[cfg(not(target_arch="wasm32"))]`.

NIE przekłamywać w komentarzach/docach że „sled działa na wasm". sled w Cozo jest *Experimental* —
zaakceptowane świadomie z dostrojeniem + idle-close uchwytów.

## POPRAWKI PO REVIEW CODEX SLICE A2 (NO-GO → wymagane przed akceptacją)

1. **Backend gating + tuning**: sled `#[cfg(not(wasm32))]`, wasm→`mem`; sled config z małym
   `cache_capacity` (np. 32 MiB) i sensownym `flush_every_ms`; lazy-open + LRU/idle-close uchwytów
   `GraphDb` w `DashMap` (setki kolekcji nie mogą pinować GiB-ów). Poprawić przekłamane komentarze
   w `backend.rs`/`Cargo.toml`.
2. **Quota kolekcji atomowa**: check-count + insert w JEDNEJ transakcji `BEGIN IMMEDIATE` (dziś
   load→check→insert osobno, wyścig).
3. **Liczniki węzłów/krawędzi**: Cozo jest ŹRÓDŁEM PRAWDY — quota egzekwowana przez zapytanie count
   z Cozo w ścieżce zapisu (param, nie format!), rejestr SQLite tylko dla UI/listingu z rekonsyliacją
   przy otwarciu. Koniec z nieatomowym dwu-store „commitem".
4. **Datalog tylko przez parametry Cozo** także w quota/count-checkach (`$id`/`$src`/...), usunąć
   `escape_single()`+`format!()` (injection surface).
5. **`delete_all_for_addon` kluczowane `(org_id, addon_id)`**, nie samym `addon_id`; poprawić test
   (dziś betonuje cross-tenant delete — łamie izolację).
6. **`delete_collection` z protokołem quiesce**: usuń z DashMap → flush+drop uchwytu sled → dopiero
   kasuj pliki; toleruj Windows (retry/deferred). Brak kasowania pod żywym uchwytem.
7. **PPR**: uwzględnić `weight` krawędzi przy eksporcie CSR; deduplikować indeksy seedów (inaczej
   zawyżenie). Jeśli świadomie unweighted — udokumentować, ale projekt zakłada wagi (faktów).

## RUNDA 2 POPRAWEK A2 (drugi NO-GO codex — współbieżność + wasm)

Korzeń problemów #3/#4/#5: `GraphManager` wydaje `Arc<CozoBackend>` na zewnątrz → traci kontrolę
nad cyklem życia (nie umie ograniczyć otwartych baz, kasować, ani serializować). Redesign:

A. **Manager-owned lifetime, BEZ wyciekania `Arc` do callerów.** Operacje (upsert_node/upsert_edge/
   query/neighbors/pagerank/ppr/count) wykonywane WEWNĄTRZ `GraphManager`, trzymając lock kolekcji;
   caller nie dostaje uchwytu do przetrzymania. Każdy wpis cache = backend za `RwLock` (lub `Mutex`).
   To naraz: (a) czyni quota check+mutate ATOMOWYM (write-lock obejmuje count i mutację Cozo),
   (b) czyni delete bezpiecznym (write-lock → close backend → remove z mapy → kasuj pliki; brak
   operacji w locie), (c) pozwala LRU REALNIE zamknąć backend przy eviction (brak zewn. `Arc`).
B. **Wasm dependency**: `cozo/storage-sled` warunkowo — `[target.'cfg(not(target_arch="wasm32"))'.
   dependencies]` ciągnie cozo ze `storage-sled`; wasm dostaje cozo bez sled (mem-only). Feature
   `graph` nie może wymuszać `storage-sled` na wasm.
C. **Migracja**: dopuścić `engine='mem'` w CHECK (wasm wstawia 'mem').
D. **MAX_OPEN_GRAPHS**: na mobile niższy cap (cfg) — ale realnym bndem jest zamykanie backendu przy
   eviction (z A), nie sama liczność mapy.
E. `get_or_create` tej samej nowej kolekcji równolegle: drugi wątek na UNIQUE-error ma ZAŁADOWAĆ
   istniejący wiersz, nie zwracać surowego błędu DB.

## INWARIANT IZOLACJI PER-INSTANCJA (twardy — testowany)

Każda zainstalowana instancja addona (`instance_id`) trzyma WYŁĄCZNIE swoje dane w
`~/.tentaflow/orgs/<org>/addons/<instance_id>/`: `addon.db` (SQLite), KV (`addon_storage`),
`vectors/`, `graph/`, `documents/`. Uninstall kasuje katalog + wiersze DB. Żaden zasób nie może
wyciec między instancjami. Wyjątek świadomy: `state_*` (A3, w pamięci) dzielone między instancjami
TEGO SAMEGO pakietu (telemetria/mirrory) — nie trwałe dane. UWAGA: obecny `FileBlobStore` jest
GLOBALNY (sha-dedup, jeden root) — slice 0.8 MUSI zrobić document store per-instancja z
ownership/refcount, inaczej łamie ten inwariant.

## RUNDA 3 POPRAWEK A2 (trzeci NO-GO codex — quota cross-collection + lifetime)

Runda 2 naprawiła współbieżność W OBRĘBIE jednej kolekcji. Zostały bugi MIĘDZY kolekcjami / cyklu życia:

F. **Globalna quota per-addon nieatomowa między kolekcjami** (`collection.rs:525/568`): `others`
   sumowane z SQLite POZA write-lockiem kolekcji → dwóch piszących do RÓŻNYCH kolekcji tego samego
   addona oba przejdą. Fix: **rejestr SQLite = atomowy ledger rezerwacji**. W JEDNEJ `BEGIN IMMEDIATE`:
   `SELECT SUM(node_count) WHERE org,addon` → jeśli +delta > limit reject → `UPDATE node_count+=delta
   WHERE collection` → COMMIT (rezerwacja). Potem mutacja Cozo; jeśli Cozo padnie → kompensata
   `node_count-=delta`. `reconcile_counts` z Cozo przy otwarciu koryguje dryf. To czyni globalną
   quotę atomową bez per-collection-only locka. (Zmienia wcześniejsze „Cozo=źródło prawdy dla quoty"
   → Cozo=źródło prawdy dla GRAFU, SQLite=atomowy ledger quoty z rekonsyliacją.)
G. **Eviction/delete/open serializowane per-klucz** (`collection.rs:187/340/404/813/830`): wprowadź
   per-key lock (np. `DashMap<GraphKey, Arc<Mutex<KeyState>>>` lub flaga `removed` w `GraphEntry`
   sprawdzana pod slot-write-lockiem PRZED open). WSZYSTKIE operacje {ensure_open, with_read/write,
   evict, delete, get_or_create} przechodzą przez ten punkt. Przeterminowany `Arc<GraphEntry>` po
   eviction/delete MUSI widzieć `removed=true` i re-fetchować kanoniczny wpis z mapy zamiast otwierać
   usuniętą bazę (eliminuje przekroczenie MAX_OPEN_GRAPHS i double-open WouldBlock).
H. **Delete finalny i serializowany nawet przy cache-miss** (`collection.rs:813`): delete bierze
   per-key lock, ustawia tombstone wiersza DB, kasuje pliki, usuwa wpis — wszystko pod tym lockiem.
   Przy cache-miss też przechodzi przez per-key lock (nie kasuje plików wprost). Kolejność: tombstone/
   delete wiersza DB i kasowanie plików w tym samym protokole wykluczania, żeby równoległy
   `get_or_create` nie wskrzesił kolekcji.

## Kryteria akceptacji

- Dwie instancje addona: fizycznie rozdzielony graf/wektory/blob; uninstall jednej nie rusza drugiej.
- `graph_query_v1` odrzuca sys-ops `::*`, skrypty imperatywne i relacje spoza kolekcji; host wymusza
  budżet czasu/wierszy (test: zapytanie-bomba ubite przez hosta, nie przez `limit`).
- PK `addon_graph_collections` zawiera `org_id`; dwie organizacje nie widzą swoich grafów.
- Uninstall grafu: jawny cleanup DB+cache+plik (nie tylko remove_dir).
- Document store: addon nie zderefuje cudzego `BlobRef`; refcount nie kasuje współdzielonych sha.
- `graph_*`: permission+audit na każdej ścieżce; ad-hoc kolekcja odrzucona; `graph_query_v1` odrzuca
  mutacje i cross-collection.
- Alias z fallbackiem: padnięty primary realnie failuje, `alias_calls.fallback_used=1` + pozycja poprawna.
- `reranker`/`doc_parse`/`vector` rejestrują się i przechodzą `execute` na stub dispatcherach.
- `doc_parse` zwraca markdown+bloki+provenance (strona/bbox). `document_put/get/delete` > 1MB, izolowany.
- Działa na mobile (cozo sqlite) i serwer (cozo rocksdb).

## STATUS INGEST (Etap 1 — doc_parse)

- **E1.2 doc_parse foundation** ✅ (codex GO-WITH-CHANGES→fix): `execute_documents()` (lustro
  execute_rerank, alias `rag-parse`, failover), `BackendClient::parse_document` (multipart→vision service),
  host fn `doc_parse_v1` (permission `document.parse`, audyt/CBOR, tożsamość usera+addona). Wejście OBRAZ.
  Gniazdo embedded-Burn (telefon, błąd→fallback). Walidacja kształtu odpowiedzi (failover na błąd),
  skip malformed blocks, blob cleanup. PDF→obraz i multi-detektor = kolejne slice'y.
- **REPO PRZENIESIONE na `/mnt/d` (lokalny nvme)** — `/mnt/e` (sieciowy) padł 2× (I/O storm + fabric drop);
  praca na /mnt/d, /mnt/e backup. Buildy: CARGO_TARGET_DIR/TMPDIR/HOME na /mnt/d.
- Następne: document/blob store host fn (upload pliku per instancja) → PDF→obraz (spike rasteryzera Rust)
  → multi-detektor nv-ingest → addon RAG (manifest/flows/GUI/logika).

## STATUS INGEST cd.
- **E1.3 document/blob store host fns** ✅ (codex GO, po 2 NO-GO + redesign): per-instancja store
  (`addon_data_dir/documents/`), rejestr `documents.db`, **streaming chunków do pliku** (zero OOM),
  serializacja per-instancja (mutex: put-finalize/get/delete/GC), **publikacja blob-PRZED-wierszem**
  (czytelnik zawsze widzi spójny stan), quota transakcyjna (`BEGIN IMMEDIATE`), `document_storage_mb`
  wpięte, limity pending (512MiB/8/2GiB), GC partial+finalizing, lock-map evict przy uninstall.
  Migracje v87/v88. document_put/get/delete/list_v1.
- Następne: PDF→obraz (rasteryzer Rust, cross-platform) → doc_parse obsługuje PDF (per strona) →
  multi-detektor nv-ingest → **addon RAG** (manifest/flows/GUI/logika — Etap 1 właściwy).

## STATUS — Etap 0/ingest ZAKOŃCZONE, start Etapu 1 (addon RAG)
- **E1.4 PDF→obraz** ✅ (codex GO-WITH-CHANGES→fix): pdfium-render (feature `pdf`), build-pdfium.sh
  (prebuilt+SHA256+hardened tar), rasteryzacja **streaming** (bounded channel, O(1 strona)), wpięte w
  execute_documents (multi-page parse+merge), anti-DoS capy, izolacja symboli FPDF_*.
- **Warstwa prymitywów RAG kompletna**: graf(CozoDB)+host fns, wektory+host fns/węzeł, aliasy+fallback,
  reranker/vector/graph_search węzły, tożsamość addon→flow_engine, doc_parse(obraz+PDF), document store.
- **Etap 1 (addon RAG)** — start: manifest (aliasy rag-*, namespaces, flow_template, [application], perms),
  per-instance SQLite (collections/documents/chunks/ingest_jobs), logika ingestu w WASM
  (document→doc_parse→chunk→embed→vector+graf), logika query (trigger flow→retrieval→answer), GUI.
