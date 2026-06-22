# RAG Etap 3 — MemGraphRAG (D): projekt + slice'y

Projekt implementacji MemGraphRAG (KDD 2026, `RAG_MEMORY.pdf`) na istniejącym stacku GraphRAG.
Zasada: core daje prymitywy, logika MemGraphRAG w addonie RAG (`tentaflow-core/addons/rag/`),
Rust/WASM, zero Pythona, izolacja per `(org_id, addon_id/instance_id, collection)`.

## 0. Punkt wyjścia (zweryfikowane w kodzie)
Mamy: graf G_fac (kolekcja `kg` nodes/edges z provenance, `services/graph/backend.rs`), PPR
(`ppr.rs`/`collection.rs`/`csr.rs`), provenance faktów (Ψ częściowo), refcount+tombstone
(`graph_artifacts`, soft-delete `alive=false`), retrieval grafowy multi-hop (`rag_graphrag.rs`,
`rag_multihop.rs`), pasaże M_pas (wektory `passages`+chunki SQLite), bezpieczne host-fns grafu
(upsert/neighbors/ppr/delete=tombstone), async Scheduler (`scheduler/mod.rs`), aliasy LLM z failoverem.

BRAK (sedno D): warstwa Ontologii (M_ont/G_ont, schematy z częstotliwością), indeksy Φ/Ψ jawne,
protokół Candidate→Stable (τ), agenci konfliktów A_det/A_res, entity merge (Structural Unification,
pochłania E3.1), multi-layer + structure-aware retrieval (P_init log-degree/info-density).

## 1. Mapowanie paper→stack
- M_pas/G_pas: zostaje (wektory passages + chunki SQLite). Ψ przez provenance.chunk_id + jawny indeks.
- M_fac/G_fac: zostaje w grafie Cozo `kg`. Encje=węzły, fakty=krawędzie.
- M_ont/G_ont: NOWA kolekcja grafowa `ont` (materializacja) + rejestr częstotliwości w addon SQLite
  (źródło prawdy Candidate→Stable, atomowy BEGIN IMMEDIATE jak refcount). Nie raw Datalog (usunięty).
- Φ (fakt↔schemat): pole `schema_id` w props krawędzi + indeks SQLite `fact_schema`.
- Ψ (fakt↔pasaż): jawny indeks SQLite `fact_evidence` budowany w ingeście obok graph_artifacts.
- A_ext: zostaje w ingeście (`extract_chunk_graph`), rozszerzony o schemat+evidence.
- A_det: ASYNC przez Scheduler (interval), tool `conflict_scan`. Symbolic match (SQL) + similarity
  (wektor), 0 LLM. NIE synchronicznie w ingeście (telefon, szybkość).
- A_res: LLM przez rag-llm, batch per schema_id, cache, tylko realne konflikty. Tombstone loserów
  (odwracalny). Eskalacja high-impact do człowieka (panel GUI później).

## 2. Architektura warstwowa + schemat
### 2.1 Graf `ont` (G_ont) — nowa kolekcja, reużywa schemat nodes/edges
ont.nodes{ id=type_name => label="OntType", props{freq,status}, provenance, ts }
ont.edges{ src=head_type, rel=relation, dst=tail_type => props{schema_id,freq,status}, weight=freq }
schema_id = sha1("{head_type}|{relation}|{tail_type}"). Status/freq materializacja; źródło prawdy SQLite.

### 2.2 Graf `kg` (G_fac) — rozszerzenie props, zero migracji rdzenia
kg.edges{ src,rel,dst => props{ schema_id, active:bool, conflict_state }, weight=confidence, provenance }
`active` = fakt ze stabilnego schematu (Thematic Denoising). Filtr retrievalu po active.
DECYZJA D2: active-via-tombstone-reuse (MVP, zero zmian rdzenia) vs jawny filtr props.active w
backend.rs neighbors/export_csr. Preferencja: tombstone-reuse w MVP, nie ruszać rdzenia.

### 2.3 Addon SQLite — migracja 003_memgraph.sql
schema_registry(schema_id PK, head_type, relation, tail_type, freq, status, first_seen, promoted_at)
fact_schema(fact_key, schema_id, document_id, PK(fact_key,document_id))
fact_evidence(fact_key, document_id, chunk_id, span, confidence, PK(fact_key,chunk_id))
conflicts(id PK, conflict_type, schema_id, head_id, fact_keys JSON, status, decision JSON, resolver,
          created_at, resolved_at)
entity_aliases(alias_id PK, canonical_id, similarity, method, created_at)
Indeksy: fact_schema(schema_id), fact_evidence(fact_key), conflicts(status),
conflicts(schema_id,head_id), entity_aliases(canonical_id).

## 3. Protokół ekstrakcji Candidate→Stable (τ)
W `extract_chunk_graph`, atomowo (BEGIN IMMEDIATE): schema_id z typów (label węzłów),
UPSERT schema_registry freq+1; gdy freq>=τ i status='candidate' → 'stable', enqueue aktywacji
faktów schematu (props.active=true). Fakt zapisywany zawsze (active=(status=='stable')); do
retrievalu wchodzi tylko active = Thematic Denoising. τ per-instancja (serwer 2-3, telefon 1=off),
domyślnie 2. Aktywacja faktu = trigger A_det (job wstawia fact_key do kolejki skanu).

## 4. Agenci konfliktów
### 4.1 Async przez Scheduler, tool conflict_scan, kursor wznawialny, idempotentny.
### 4.2 A_det (0 LLM): symbolic match (te same (head,rel) lub schema_id+head, różny tail → kandydaci,
SQL+neighbors). Klasyfikacja typu regułami kardynalności (functional/temporal/hierarchical, mała tabela
config): Mutually Exclusive (functional+różny tail), Temporal (temporal-rel+różny tail), Granularity
(part_of/hierarchia, trawers ont/kg). Similarity (wektor faktu) odsiewa pozorne (podobne head→MERGE
nie konflikt → Structural Unification).
### 4.3 A_res (LLM batch+cache): evidence z fact_evidence→pasaże + schemat ont → rag-llm. Decyzja:
winner (tombstone loserów, odwracalne), temporal_split (oba+adnotacja czasu), merge (granularity→
kanonizacja), escalate (high-impact→człowiek). Batch per schema_id, cache (schema_id,head,fact_keys_hash).
Eskalacja: evidence po obu stronach > próg LUB functional z silnym evidence. Odwracalność: log
fact_keys+decision, tombstone odwracalny, cofnięcie=re-upsert+status='open'.

## 5. Structural Unification (entity merge, pochłania E3.1)
Kandydaci: type-based (ten sam label+podobne nazwy, normalize_entity_name) + similarity-based
(embedding nazwy encji, namespace `entities`, próg cosine). Auto-merge powyżej progu twardego, szara
strefa→A_res/eskalacja. Merge: wybierz canonical_id, zapis entity_aliases, przekieruj krawędzie
(upsert canonical + tombstone alias), update refcount graph_artifacts. Retrieval: rag_graph_seed
aplikuje alias→canonical na seedach (jedna linia). Ryzyko: entity resolution dominuje korektność →
próg konserwatywny, confidence, odwracalność, człowiek w szarej strefie.

## 6. Memory-guided retrieval (rozszerzenie query-flow, zero nowego silnika)
1. Multi-Layer: M_pas (wektor, jest) + M_fac (PPR kg, jest) + M_ont (top-K schematów, NOWE — ważenie
   faktów po stabilności/częstości schematu + filtr active).
2. Structure-Aware P_init: encje (relevance z hitów wektorowych), typy (log-degree penalty
   weight/=log(1+degree)), pasaże (information density). Weighted seedy → PPR (csr.rs przyjmuje weighted).
3. PPR: jest. Heterogeniczny graf (pasaże-węzły) opcjonalny refinement D6b (podbija rozmiar/telefon).
Wpięcie: rozszerzenie identify_query_entities + collect_facts w rag_graphrag.rs. Degradacja zachowana.

## 7. Slice'y (MVP=D1-D5, refinement=D6-D8)
- D1 — Ontologia+schema_registry: migracja 003, kolekcja ont, w ingeście schema_id+freq++ atomowo+
  materializacja ont+Φ/Ψ. Bez promocji. Zależy: E3.0. Ryzyko: atomowość, narzut ingestu (tani SQL).
- D2 — Candidate→Stable+Denoising: próg τ, promocja, props.active, filtr retrievalu po active.
  Decyzja active-via-tombstone vs filtr backend.rs. Zależy: D1. Ryzyko: dotyk rdzenia, zły τ.
- D3 — A_det (0 LLM): tool conflict_scan+scheduler, symbolic+klasyfikacja+similarity, conflicts(open),
  kursor. Zależy: D1,D2. Ryzyko: latencja skanu (batch+kursor), precyzja reguł.
- D4 — A_res (LLM batch+cache): prompt evidence-driven→rag-llm, decyzje, tombstone, log. Zależy: D3,Ψ.
  Ryzyko: koszt LLM, korektność adjudykacji, halucynacja (provenance).
- D5 — Structural Unification (E3.1): kandydaci type+similarity, merge, entity_aliases, redirect
  krawędzi, alias-rewrite seedów. Zależy: D4, wektory. Ryzyko: entity resolution dominuje korektność.
- D6 — Memory-guided retrieval: M_ont w hopie, P_init weighted, alias-rewrite. Zależy: D2,D5.
  Ryzyko: latencja PPR (capy mamy), heterogeniczny graf→odłożyć.
- D7 — Panel konfliktów GUI (poza MVP). D8 — Ewaluacja (RAG vs GraphRAG vs MemGraphRAG, metryki, koszt).
Ścieżka krytyczna: D1→D2→D3→D4→D5→D6. D7/D8 równolegle.

## 8. Ryzyka i testy bez modeli
Koszt LLM: A_det 0 LLM, A_res tylko realne konflikty+batch+cache+eskalacja. Latencja: agenci async,
ingest D1 tylko SQL, query PPR bounded (GraphComputeGuard+capy). Korektność: evidence-driven+provenance+
odwracalne+człowiek w szarej strefie. Izolacja: każda tabela/kolekcja per (org,addon,collection); ont
przez GraphManager; uninstall delete_all_for_addon obejmuje ont. Testy bez modeli: A_det 100%
(symbolic+reguły, czyste funkcje), A_res na stub-LLM (logika decyzji/batch/cache/odwracalność), schema
promotion czyste funkcje, entity merge na stub_graph (feature graph), E2E z modelami #[ignore]+env.

## 9. Punkty integracji
addon lib.rs: extract_chunk_graph (schema/freq/Φ/Ψ/ont), tool conflict_scan, A_det/A_res/merge,
handle_ask seed alias-rewrite. Migracja 003_memgraph.sql. Manifest: [[graph_collection]] ont,
scheduled conflict_scan, [[vector_namespace]] entities. rag_graphrag.rs: identify_query_entities
(weights), alias-rewrite, schema-weight/active. Core TYLKO jeśli D2 wybierze jawny filtr active
(backend.rs) — preferencja NIE ruszać. Scheduler bez zmian. Brak nowych prymitywów grafu w MVP.

## REWIZJA po niezależnym review codexa (NO-GO→warunkowe GO) — NADRZĘDNA nad §2-§7

Codex (statycznie) wykrył fałszywe założenie krytyczne + 4 realne ryzyka. Poprawki przyjęte
w całości. Poniższe DECYZJE są nadrzędne nad wcześniejszymi sekcjami tam gdzie kolidują.

### R1. SQLite addona = JEDYNE źródło prawdy. Graf = odtwarzalna materializacja aktywnego widoku.
Φ/Ψ, statusy schematów, stan faktów, konflikty, aliasy — wyłącznie w SQLite addona. Graf NIE jest
bazą do skanów decyzyjnych (host-fns celowo ograniczone do upsert/neighbors/pagerank/ppr/delete).
A_det/A_res/merge czytają i decydują WYŁĄCZNIE z SQLite; graf tylko karmi PPR.

### R2. D2 — ODRZUCONE active-via-tombstone-reuse. Przyjęte: osobna kolekcja `kg_active`.
Powód NO-GO: `alive=false` to soft-delete globalnie filtrowany w neighbors/PageRank/CSR
(`backend.rs:521,668`), a `upsert_edge` ZAWSZE ożywia (`alive=true`, `backend.rs:439`) — tombstone
jako „candidate inactive" koliduje z prawdziwym soft-delete dokumentu i refcountem graph_artifacts.
DECYZJA: retrieval/PPR działa na **`kg_active`** — kolekcji zawierającej WYŁĄCZNIE fakty
stable + non-conflict + canonical. Pełny ledger faktów (wraz z candidate/przegranymi/aliasami)
żyje w SQLite, nie w osobnym pełnym grafie. Zero zmian rdzenia w MVP (D1-D5). `kg` (dzisiejszy)
zostaje zastąpiony przez `kg_active` materializowany z SQLite — E3.0 przestaje pisać do grafu
wprost, pisze do SQLite+outbox, materializacja wpisuje do `kg_active`.

### R3. Ingest NIE jest atomowy SQLite↔Cozo → wzorzec OUTBOX (idempotentny).
W JEDNEJ transakcji SQLite (BEGIN IMMEDIATE): schema_registry, fact_schema, fact_evidence,
fact_state, graph_outbox. OSOBNY idempotentny krok aplikuje graph_outbox do `kg_active`/`ont`
(upsert/delete host-fn), po sukcesie znacza `applied`. Retry deterministyczny po `fact_key`/`op_id`.
Promocja candidate→stable i aktywacja faktów = wpisy do graph_outbox (materializacja dodaje krawędź
do kg_active). A_det trigger NIE zależy od natychmiastowego upsertu grafu — czyta fact_state z SQLite.

### R4. Scheduler — egzekwowanie org_id + instancji. conflict_scan z monotonicznym fact_seq.
`scheduler/mod.rs:293` woła `start_addon(addon_id,...,None)` — brak jawnego org_id (host-fns grafu
biorą org z AddonState z fallbackiem do default org `graph.rs:17`) = ryzyko izolacji multi-tenant.
conflict_scan MUSI: nieść i egzekwować `org_id` + konkretny `addon_id` instancji; payload
`{collection_id, since_seq, batch_size}`; blokada per-collection (jeden skan naraz); kursor =
monotoniczny `fact_seq` (AUTOINCREMENT w fact_state) lub `graph_outbox.id > cursor` — sam kursor
czasowy NIE wystarcza przy równoległym ingeście. [Scheduler trzeba rozszerzyć o przekazanie org_id
do scheduled job — to drobna zmiana core w scheduler, w granicach „prymitywu".]

### R5. Entity merge (D5) = logiczna transakcja SQLite + redirect artefaktów, w pełni odwracalna.
Redirect krawędzi + tombstone alias-node psuje refcount (graph_artifacts referuje stare n_id/src/dst;
cleanup dokumentu kasowałby nieistniejące klucze) i ukrywa incydentne krawędzie (join z non-tombstone
node, `backend.rs:597`). DECYZJA: merge to tx SQLite: `entity_aliases(alias_id,canonical_id,status)`,
`entity_merge_log` (pełny diff starych↔nowych edge-keys), `artifact_redirects(old_key→canonical_key)`
(graph_artifacts cleanup rozwiązuje redirect), materializacja przez graph_outbox, undo przez
inverse-outbox. Alias-rewrite seedów to TYLKO retrieval-side ułatwienie, nie mechanizm merge.

### R6. D6 wagi P_init — wymaga rozszerzenia primitywu PPR o seedy ważone.
`GraphManager::ppr` bierze `Vec<String>` (`collection.rs:730`), adapter zrzuca same id
(`rag_graphrag.rs:414`) → obecne PPR IGNORUJE wagi seedów (personalizacja uniform po seedach).
P_init (relevance/log-degree/info-density) wymaga `ppr(Vec<(String,f32)>)` — rozszerzenie primitywu
grafu w core (legalne: prymityw). Zaplanowane w D6, nie wcześniej.

### R7. Manifest — deklaracje do dodania w D1/D3.
`[[graph_collection]] name="kg_active"` (i ewentualnie `ont`); tool `conflict_scan`;
`[[vector_namespace]] entities` (similarity-merge). Obecnie manifest deklaruje tylko `kg`.

### R8. Limity kosztu A_res (D4, twarde): batch cap, token cap, cache TTL, max conflicts/run, audit.

### Zrewidowana kolejność/bramki
D1 (SQLite-first: migracja 003 + schema_registry/fact_schema/fact_evidence/fact_state/graph_outbox;
manifest kg_active; ingest pisze SQLite+outbox; materializacja outbox→kg_active; E3.0 przepięte z
bezpośredniego zapisu kg na outbox) → D2 (Candidate→Stable: τ, promocja przez outbox, kg_active =
tylko stable) → D3 (A_det async, fact_seq, lock per-collection, org_id) → D4 (A_res LLM+limity R8) →
D5 (merge wg R5) → D6 (memory-guided + ppr ważone R6). Każdy slice: codex review realnego kodu.

## STATUS: D1 ✅ (c82ab056), D2 ✅ — denoising przez idempotentny reconcile po commicie (promocja z COUNT≥τ, exactly-once aktywacja przez warunkowy enqueue WHERE active=0 + BEGIN IMMEDIATE, migracja 004 dedupe+unique, COUNT(DISTINCT) refcount). Codex GO. Następne: D3 (A_det async).

## D3 ✅ — A_det async detekcja konfliktów (0 LLM): kursor monotoniczny activation_seq, lock atomowy (rows_affected+owner), detekcja symboliczna head+rel (klasyfikacja po relation_cardinality functional/temporal/hierarchical), conflict_members znormalizowane (atomowe INSERT OR IGNORE, cap 64), tozsamosc=grupa (1 open/grupa), scheduler org_id (stempel z auth + asercja instance_org_id, addon_id==instance_id unikalny). Migracje 005/006. Codex GO. Nastepne: D4 (A_res LLM adjudykacja + limity R8). similarity-gate odlozony do D5.

## D4 ✅ — A_res adjudykacja konfliktów LLM: tool conflict_resolve, evidence-driven (per-member balance), akcje odwracalne (keep_winner→tombstone outbox, temporal_split, merge_pending→D5, escalate→D7), claim exactly-once (resolve_owner) + apply/finalize atomowe warunkowane ownership+members_rev (TOCTOU guard members_rev=COUNT atomowy), reconcile wyklucza resolved_loser, indeks open|resolving, limity R8 (batch/evidence/cache/audit). Migracje 007/008. Codex GO (4 rundy). Nastepne: D5 (entity merge / Structural Unification + similarity-gate z D3).

## D5 ✅ — Structural Unification (entity merge): kanonizacja aliasów u ŹRÓDŁA (resolve_canonical w ingeście→fakty od razu kanoniczne), detekcja type+similarity z progiem konserwatywnym (prefiks→szara strefa→konflikt entity_merge, pochłania similarity-gate D3+E3.1), merge przez outbox (redirect krawędzi+wezłów, graph_artifacts przepisane na kanoniczne), odwracalność BIT-W-BIT (pełny snapshot pre-merge w edge_diff: docs+active+schema+evidence per-dokument, undo inverse-outbox), cleanup SQL-in-tx serializowany z merge, members_rev=COUNT po redirect conflict_members, integracja merge_pending D4, alias-rewrite seedów retrievalu. Migracja 009. Codex GO (4 rundy). Nastepne: D6 (memory-guided retrieval: multi-layer M_ont/M_fac/M_pas + P_init log-degree/info-density + ppr wazone R6) — OSTATNI slice D. Potem A (GUI).

## D6 ✅ — memory-guided retrieval: PPR WAŻONE (R6 — personalized_pagerank seedy (idx,waga), GraphManager::ppr seedy (id,waga), wszyscy wolajacy zaktualizowani w miejscu), P_init structure-aware (ppr_with_p_init: jeden snapshot CSR, log-degree penalty w/=1+ln(1+degree), relevance boost x2 z pasazy, cap MAX_GRAPH_SEEDS PO przewazeniu, filtr nieznanych przed capem), M_ont = denoising D2 (filtr active; freq-weighting odlozony). Codex GO (3 rundy).

# ETAP 3 MemGraphRAG (D) UKONCZONY: D1 SQLite+outbox, D2 denoising, D3 A_det, D4 A_res, D5 entity merge, D6 memory-guided retrieval. Wszystkie codex GO. Pozostaje: A (GUI addona RAG).
