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
