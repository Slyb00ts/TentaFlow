-- =============================================================================
-- Plik: addons/rag/migrations/005_conflicts.sql
-- Opis: MemGraphRAG slice D3 — agent detekcji konfliktow A_det (0 LLM, 0 embeddingow).
--       A_det skanuje ASYNCHRONICZNIE nowo-aktywne fakty (fact_state.active=1) i wykrywa
--       SYMBOLICZNIE kandydatow konfliktu (ten sam head_id+rel, rozny tail_id) klasyfikujac
--       typ wg reguly kardynalnosci (relation_cardinality). Otwarte konflikty laduja w
--       'conflicts' (status='open'); rozwiazywanie (A_res, LLM) to D4. Zrodlem prawdy
--       pozostaje SQLite addona (R1) — A_det czyta i decyduje WYLACZNIE z tych tabel.
-- =============================================================================

-- Rejestr wykrytych konfliktow = TOZSAMOSC GRUPY konfliktowej (conflict_type + head_id + rel
-- via dedup_key). Czlonkostwo faktow NIE jest tu trzymane jako JSON: zostalo ZNORMALIZOWANE
-- do osobnej tabeli conflict_members (patrz nizej). status steruje cyklem zycia: 'open'
-- (wykryty, czeka na A_res) -> 'resolved' (D4 zapisze decision/resolver). decision/resolver/
-- resolved_at sa NULL dopoki D4 ich nie wypelni (D3 ich nie rusza).
CREATE TABLE IF NOT EXISTS conflicts (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  conflict_type TEXT NOT NULL,
  schema_id     TEXT NOT NULL,
  head_id       TEXT NOT NULL,
  rel           TEXT NOT NULL,
  dedup_key     TEXT NOT NULL,
  status        TEXT NOT NULL DEFAULT 'open',
  decision      TEXT,
  resolver      TEXT,
  created_at    INTEGER,
  resolved_at   INTEGER
);

-- Idempotencja detekcji = TOZSAMOSC GRUPOWA: dedup_key kanonizuje GRUPE konfliktowa
-- (conflict_type + head_id + rel). Powod: A_res (D4) oczekuje JEDNEGO konfliktu na grupe
-- (head,rel). Gdyby tozsamoscia byl pelny zbior faktow, sekwencja: open[A,B] + nowy fakt C
-- -> nowy open BEZ zamkniecia [A,B] => dwa nakladajace sie otwarte konflikty tej samej grupy.
-- Z kluczem grupowym istnieje najwyzej JEDEN otwarty konflikt per (head,rel): dojscie nowego
-- faktu do grupy DOPISUJE wiersz do conflict_members istniejacego open (INSERT OR IGNORE),
-- nie tworzy drugiego open. Partial-unique WHERE status='open' (wzor jak ux_graph_outbox_pending
-- z 003): po rozwiazaniu (status<>'open') klucz sie zwalnia, wiec gdyby grupa znow weszla w
-- konflikt, A_det otwiera nowy (nie blokujemy globalnie historii rozwiazan).
CREATE UNIQUE INDEX IF NOT EXISTS ux_conflicts_open ON conflicts(dedup_key) WHERE status = 'open';
CREATE INDEX IF NOT EXISTS ix_conflicts_status      ON conflicts(status);
CREATE INDEX IF NOT EXISTS ix_conflicts_schema_head ON conflicts(schema_id, head_id);

-- Czlonkostwo faktow w grupie konfliktowej — ZNORMALIZOWANE (jeden wiersz = jeden fakt w
-- grupie). Zastepuje dawny JSON conflicts.fact_keys + read-modify-write union (blocker 2 /
-- wazne 4): tamten wzorzec rosl bez capa i GUBIL wiekszy union przy wyscigu (po TTL dwa
-- rownolegle skany robily read-before-insert na tym samym open). Tu dopisanie czlonka to
-- ATOMOWY, IDEMPOTENTNY `INSERT OR IGNORE` na PRIMARY KEY (conflict_dedup_key, fact_key) —
-- bez odczytu-przed-zapisem => brak wyscigu union, a ponowny skan tej samej grupy nic nie
-- zmienia. conflict_dedup_key wiaze czlonka z grupa (a NIE z conflicts.id), bo open zamyka
-- sie i otwiera ponownie pod tym samym dedup_key; A_res (D4) odczyta czlonkow po dedup_key
-- aktywnego open. added_at = czas dopisania czlonka (audyt kolejnosci wlaczania do grupy).
CREATE TABLE IF NOT EXISTS conflict_members (
  conflict_dedup_key TEXT NOT NULL,
  fact_key           TEXT NOT NULL,
  added_at           INTEGER,
  PRIMARY KEY (conflict_dedup_key, fact_key)
);

-- Reguly kardynalnosci relacji — mala tabela konfiguracyjna sterujaca KLASYFIKACJA typu
-- konfliktu w A_det. kind:
--   'functional'   — relacja jednowartosciowa (jedno prawidlowe tail): rozny tail => twardy
--                    konflikt 'mutual_exclusive' (np. born_in, birth_date, capital_of).
--   'temporal'     — relacja zmienna w czasie (rozne tail dopuszczalne w roznych okresach):
--                    rozny tail => miekki 'temporal' (np. president_of, ceo_of).
--   'hierarchical' — relacja zagniezdzenia (tail-e moga byc w relacji zawierania, np.
--                    Shanghai part_of China): rozny tail => 'granularity'-kandydat.
-- Relacje SPOZA tej tabeli A_det traktuje jako nie-functional: brak reguly = brak pewnosci,
-- wiec NIE tworzymy konfliktu (unikamy zalewu false-positive). Patrz A_det w lib.rs.
CREATE TABLE IF NOT EXISTS relation_cardinality (
  relation TEXT PRIMARY KEY,
  kind     TEXT NOT NULL
);

INSERT OR IGNORE INTO relation_cardinality (relation, kind) VALUES
  ('born_in',      'functional'),
  ('birth_date',   'functional'),
  ('birth_place',  'functional'),
  ('died_in',      'functional'),
  ('death_date',   'functional'),
  ('capital_of',   'functional'),
  ('founded_in',   'functional'),
  ('headquartered_in', 'functional'),
  ('nationality',  'functional'),
  ('president_of', 'temporal'),
  ('ceo_of',       'temporal'),
  ('employed_by',  'temporal'),
  ('member_of',    'temporal'),
  ('located_in',   'hierarchical'),
  ('part_of',      'hierarchical'),
  ('subsidiary_of','hierarchical'),
  ('contained_in', 'hierarchical');

-- Kursor skanu A_det per kolekcja. Kursor MONOTONICZNY po activation_seq (migracja 006):
-- conflict_scan pobiera fakty WHERE active=1 AND activation_seq>last_activation_seq
-- ORDER BY activation_seq LIMIT batch. activation_seq jest przydzielany WYLACZNIE przy
-- flipie active=0->1 w reconcile (MAX(activation_seq)+1 w tej samej tx; BEGIN IMMEDIATE
-- serializuje), wiec odzwierciedla kolejnosc AKTYWACJI, nie ingestu. To lapie KAZDA
-- aktywacje: fakt wstawiony wczesnie (niski fact_seq), aktywowany pozno, dostaje WYSOKI
-- activation_seq => nie zostanie pominiety. Dawny kursor czasowy (updated_at) gubil fakty
-- aktywowane w tej samej sekundzie po advance kursora (sekundowa rozdzielczosc).
--
-- scan_lock_until + scan_lock_owner: blokada per-instancja (jeden skan naraz). acquire robi
-- WARUNKOWY UPDATE i sprawdza rows_affected==1 (atomowo, bez read-after) — dwa skany w tej
-- samej sekundzie nie moga oba "wziac" locka. scan_lock_owner to UNIKALNY token wlasciciela:
-- release zwalnia TYLKO wlasny lock (WHERE owner=?), wiec stary skan po TTL nie wyzeruje
-- locka nowego, ktory tymczasem przejal blokade.
CREATE TABLE IF NOT EXISTS conflict_scan_cursor (
  collection_id       TEXT PRIMARY KEY,
  last_activation_seq INTEGER NOT NULL DEFAULT 0,
  scan_lock_until     INTEGER NOT NULL DEFAULT 0,
  scan_lock_owner     TEXT
);
