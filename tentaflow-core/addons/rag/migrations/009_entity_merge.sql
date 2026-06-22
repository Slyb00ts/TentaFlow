-- =============================================================================
-- Plik: addons/rag/migrations/009_entity_merge.sql
-- Opis: MemGraphRAG slice D5 — Structural Unification (entity merge), W PELNI ODWRACALNY
--       (R5). Encje = wezly grafu (node_id = normalize_entity_name). Scalanie aliasu w
--       kanoniczny wezel to LOGICZNA transakcja SQLite + redirect artefaktow + przekierowanie
--       krawedzi PRZEZ outbox (R1/R3, nigdy bezposrednio do grafu). Zrodlem prawdy pozostaje
--       SQLite addona — graf 'kg_active' jest tylko odtwarzalna materializacja.
--
--       Dwie tabele (+ kursor skanu):
--         * entity_aliases       — mapa alias_id -> canonical_id (status active/reverted),
--                                  uzywana przy retrievalu (alias-rewrite seedow) i undo.
--         * entity_merge_log     — pelny diff KAZDEGO merge'u (przekierowane edge-keys + snapshot
--                                  kanonicznego/wezlow), dane do bezstratnego undo (inverse-outbox).
--       Merge przepisuje graph_artifacts (krawedzie i wezly) PROSTO na kanoniczne klucze w swojej
--       tx, wiec cleanup dokumentu czyta juz kanoniczne src/rel/dst/n_id W TEJ SAMEJ serializowanej
--       tx (BEGIN IMMEDIATE) — osobna tabela redirectow nie jest potrzebna.
-- =============================================================================

-- Mapa aliasow: alias_id (znormalizowana nazwa encji-aliasu) -> canonical_id (kanoniczny
-- wezel docelowy). status:
--   'active'   — merge obowiazuje; alias-rewrite seedow retrievalu przekierowuje alias->canonical,
--                cleanup rozwiazuje redirecty, undo jest mozliwy.
--   'reverted' — merge cofniety (undo); rekord ZOSTAJE jako slad audytowy, ale NIE jest juz
--                stosowany przy retrievalu ani cleanupie (alias-rewrite/redirect filtruja active).
-- similarity/method to provenance decyzji (type-based prefix/edit-distance vs similarity-based
-- embedding vs merge_pending z A_res). created_at = czas scalenia.
CREATE TABLE IF NOT EXISTS entity_aliases (
  alias_id     TEXT PRIMARY KEY,
  canonical_id TEXT NOT NULL,
  similarity   REAL,
  method       TEXT,
  status       TEXT NOT NULL DEFAULT 'active',
  created_at   INTEGER
);

-- Log merge'ow = pelny diff do UNDO (R5). Kazdy wiersz to jeden zastosowany merge aliasu w
-- kanoniczny wezel. edge_diff to JSON z lista przekierowanych krawedzi:
--   { "edges": [ { "old": {src,rel,dst}, "new": {src,rel,dst}, "fact_key_old", "fact_key_new" }, ... ],
--     "canonical_id", "alias_id" }
-- Undo odtwarza inverse-outbox z tego diffa: przywraca krawedzie aliasu (upsert old) i kasuje
-- dodane kanoniczne (delete new) — o ile new nie jest wspoldzielone przez inny, niezalezny fakt.
-- reverted_at NULL => merge aktywny; ustawiany przy undo (rekord nie jest kasowany — audyt).
CREATE TABLE IF NOT EXISTS entity_merge_log (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  alias_id     TEXT NOT NULL,
  canonical_id TEXT NOT NULL,
  edge_diff    TEXT NOT NULL,
  method       TEXT,
  similarity   REAL,
  created_at   INTEGER,
  reverted_at  INTEGER
);

-- Kursor + lock skanu kandydatow merge (A_uni), wzorem conflict_scan_cursor z 005: monotoniczny
-- po fact_seq fact_state (nowe encje pojawiaja sie wraz z nowymi faktami), blokada per-instancja
-- (jeden skan naraz, atomowy warunkowy UPDATE + rows_affected, owner-scoped release, TTL anty-crash).
CREATE TABLE IF NOT EXISTS entity_merge_scan_cursor (
  collection_id   TEXT PRIMARY KEY,
  last_fact_seq   INTEGER NOT NULL DEFAULT 0,
  scan_lock_until INTEGER NOT NULL DEFAULT 0,
  scan_lock_owner TEXT
);

-- Indeksy hot-path: alias-rewrite/undo czytaja po canonical_id i status; cleanup rozwiazuje
-- redirecty po old_key (PK juz pokrywa) oraz odwrotnie po canonical_key przy undo.
CREATE INDEX IF NOT EXISTS ix_entity_aliases_canonical ON entity_aliases(canonical_id);
CREATE INDEX IF NOT EXISTS ix_entity_aliases_status    ON entity_aliases(status);
CREATE INDEX IF NOT EXISTS ix_entity_merge_log_alias   ON entity_merge_log(alias_id);
