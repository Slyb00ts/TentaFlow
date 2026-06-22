-- =============================================================================
-- Plik: addons/rag/migrations/003_memgraph.sql
-- Opis: Fundament MemGraphRAG (Etap 3, slice D1). SQLite addona staje sie JEDYNYM
--       zrodlem prawdy o faktach/schematach (R1), a graf 'kg_active' jest tylko
--       odtwarzalna materializacja aktywnego widoku. Ingest pisze stan do tych
--       tabel + kolejki 'graph_outbox' (R3); osobny idempotentny krok aplikuje
--       outbox do grafu host-fnami upsert/delete. D1 trzyma WSZYSTKIE fakty jako
--       aktywne (active=1) — gate progu tau, A_det i merge wchodza w D2+.
-- =============================================================================

-- Rejestr schematow (head_type, relation, tail_type) z czestotliwoscia. Status
-- 'candidate'/'stable' steruje promocja w D2; w D1 tylko zliczamy freq i znaczymy
-- pierwsze wystapienie. schema_id = stabilny hash z trojki typow (deterministyczny).
CREATE TABLE IF NOT EXISTS schema_registry (
  schema_id   TEXT PRIMARY KEY,
  head_type   TEXT NOT NULL,
  relation    TEXT NOT NULL,
  tail_type   TEXT NOT NULL,
  freq        INTEGER NOT NULL DEFAULT 0,
  status      TEXT NOT NULL DEFAULT 'candidate',
  first_seen  INTEGER,
  promoted_at INTEGER
);

-- Indeks Phi (fakt -> schemat) per dokument. Klucz (fact_key, document_id), bo ten
-- sam fakt moze pochodzic z wielu dokumentow (refcount po stronie dokumentu).
CREATE TABLE IF NOT EXISTS fact_schema (
  fact_key    TEXT NOT NULL,
  schema_id   TEXT NOT NULL,
  document_id TEXT NOT NULL,
  PRIMARY KEY (fact_key, document_id)
);

-- Indeks Psi (fakt -> pasaz/chunk) — evidence pod adjudykacje konfliktow (D4).
-- Klucz (fact_key, document_id, chunk_id): jeden dowod per fakt per chunk KONKRETNEGO
-- dokumentu. document_id MUSI byc w PK, bo chunk_id = chunk_index (lokalny dla
-- dokumentu) — bez niego docA/chunk0 i docB/chunk0 nadpisywalyby sobie evidence
-- (rozne dokumenty, ten sam chunk_index). Ponowny ingest tego samego chunku tego
-- samego dokumentu nadal nadpisuje (UPSERT), nie duplikuje.
CREATE TABLE IF NOT EXISTS fact_evidence (
  fact_key    TEXT NOT NULL,
  document_id TEXT NOT NULL,
  chunk_id    TEXT NOT NULL,
  span        TEXT,
  confidence  REAL,
  PRIMARY KEY (fact_key, document_id, chunk_id)
);

-- Stan faktu = zrodlo prawdy o krawedzi grafu. fact_seq to MONOTONICZNY kursor pod
-- A_det (R4): conflict_scan wznawia od ostatniego seq, wiec rownolegly ingest nie
-- gubi nowych faktow. active=1 w D1 dla kazdego faktu (gate tau dopiero w D2).
-- UWAGA dla D3: samo fact_seq>cursor lapie tylko PIERWSZE pojawienie faktu — nie
-- zobaczy AKTUALIZACJI istniejacego faktu (upsert nie rusza fact_seq). A_det w D3
-- bedzie wiec uzywal kursora (updated_at, fact_seq) LACZNIE; upsert fact_state MUSI
-- aktualizowac updated_at, by aktualizacja byla widoczna ponad kursorem.
CREATE TABLE IF NOT EXISTS fact_state (
  fact_seq       INTEGER PRIMARY KEY AUTOINCREMENT,
  fact_key       TEXT NOT NULL UNIQUE,
  schema_id      TEXT NOT NULL,
  head_id        TEXT NOT NULL,
  rel            TEXT NOT NULL,
  tail_id        TEXT NOT NULL,
  active         INTEGER NOT NULL DEFAULT 1,
  conflict_state TEXT,
  created_at     INTEGER,
  updated_at     INTEGER
);

-- Outbox materializacji grafu (R3). Ingest zapisuje TRWALA INTENCJE upsertu/delete,
-- osobny drain czyta WHERE applied=0 i aplikuje ja do 'kg_active' host-fnami, po
-- sukcesie znacza applied=1 (re-drain po crashu domyka applied=0). dedup_key jest
-- KANONICZNYM, length-prefixed kodowaniem (op, collection, klucz) — bezkolizyjnym i
-- jednoznacznym (granice pol nie sa dwuznaczne, brak hasha).
--
-- dedup_key NIE jest globalnie UNIQUE — dedup obejmuje TYLKO operacje OCZEKUJACE
-- (partial-unique WHERE applied=0). Gdyby UNIQUE byl globalny, sekwencja:
--   ingest faktu -> wiersz applied=1 (zmaterializowany w kg_active)
--   usuniecie dokumentu -> cleanup kasuje wezel/krawedz Z grafu
--   re-ingest tego samego faktu -> INSERT OR IGNORE trafia na istniejacy applied=1
--   => zaden NOWY wiersz applied=0 nie powstaje => drain NIE odtwarza kg_active
-- lamala R1/R3 (graf przestawal byc materializacja outboxu). Partial-unique dedupuje
-- tylko pending: po zaaplikowaniu (applied=1) ponowny ingest/delete tego samego klucza
-- tworzy NOWY wiersz applied=0, a drain go materializuje (re-upsert / re-delete).
CREATE TABLE IF NOT EXISTS graph_outbox (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  dedup_key  TEXT NOT NULL,
  op         TEXT NOT NULL,
  collection TEXT NOT NULL,
  payload    TEXT NOT NULL,
  applied    INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER,
  applied_at INTEGER
);

-- Indeksy pod hot-path: drain pyta WHERE applied=0 ORDER BY id; conflict_scan (D3)
-- filtruje schematy po status; Phi/Psi czytane po schema_id / fact_key.
CREATE INDEX IF NOT EXISTS ix_fact_schema_schema   ON fact_schema(schema_id);
CREATE INDEX IF NOT EXISTS ix_fact_evidence_fact   ON fact_evidence(fact_key);
CREATE INDEX IF NOT EXISTS ix_fact_state_active    ON fact_state(active);
CREATE INDEX IF NOT EXISTS ix_graph_outbox_applied ON graph_outbox(applied);
-- Partial-unique: INSERT OR IGNORE dedupuje WYLACZNIE wiersze oczekujace (applied=0).
-- Po zaaplikowaniu klucz zwalnia sie, wiec ponowny ingest/delete tworzy nowy pending
-- (re-materializacja po cleanupie). Patrz komentarz przy CREATE TABLE graph_outbox.
CREATE UNIQUE INDEX IF NOT EXISTS ux_graph_outbox_pending ON graph_outbox(dedup_key) WHERE applied = 0;
CREATE INDEX IF NOT EXISTS ix_schema_registry_stat ON schema_registry(status);
