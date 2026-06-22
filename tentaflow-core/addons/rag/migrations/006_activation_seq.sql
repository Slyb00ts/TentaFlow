-- =============================================================================
-- Plik: addons/rag/migrations/006_activation_seq.sql
-- Opis: MemGraphRAG slice D3 — MONOTONICZNY kursor aktywacji pod A_det.
--       fact_state.fact_seq (z 003) rosnie przy PIERWSZYM pojawieniu faktu, a
--       updated_at (sekundowy now_unix) bumpuje sie przy AKTYWACJI. Oba sa zlym
--       kursorem skanu konfliktow: dwa fakty aktywowane w TEJ SAMEJ sekundzie z
--       roznym fact_seq, gdzie skan zaawansowal kursor czasowy miedzy nimi, gubia
--       sie (sekundowa rozdzielczosc nie rozroznia kolejnosci aktywacji, a fact_seq
--       odzwierciedla kolejnosc INGESTU, nie AKTYWACJI). Rozwiazanie: osobny licznik
--       activation_seq przydzielany WYLACZNIE przy flipie active=0->1 w reconcile
--       (BEGIN IMMEDIATE serializuje => monotonicznosc per-instancja). Kursor A_det
--       skanuje po activation_seq, wiec lapie KAZDA aktywacje niezaleznie od tego, jak
--       wczesnie fakt zostal wstawiony (niski fact_seq) i jak pozno aktywowany.
--       fact_state pochodzi z zacommitowanej 003 — ALTER musi byc w nowej migracji.
-- =============================================================================

-- activation_seq: numer porzadkowy aktywacji faktu (active 0->1). NULL/0 = fakt jeszcze
-- nieaktywny (D1/D2 wstawia z active=0; reconcile przydziela seq dopiero przy aktywacji).
-- Brak DEFAULT 1/AUTOINCREMENT: numeracje nadaje reconcile recznie z MAX(activation_seq)+1
-- w tej samej tx co flip, zeby seq odzwierciedlal kolejnosc AKTYWACJI, nie wstawienia.
--
-- IDEMPOTENCJA ALTER (wazne 3): SQLite nie wspiera `ADD COLUMN IF NOT EXISTS`, ale runner
-- migracji addonow (addon/migrations.rs) gwarantuje wykonanie KAZDEGO pliku DOKLADNIE RAZ:
-- po sukcesie zapisuje (addon_id, migration_name, hash, status='success') do core DB
-- addon_migrations_applied; przy ponownym install hash-match => skip. Caly plik jest
-- aplikowany ATOMOWO (execute_batch w jednej transakcji), wiec jesli ktorykolwiek statement
-- zawiedzie, ALTER tez sie wycofuje (rollback) i nie zostaje 'pol-zaaplikowany' — ponowny
-- bieg (status<>'success') startuje od tabeli BEZ kolumny. Dlatego goly ALTER jest tu
-- bezpieczny: nie ma sciezki, w ktorej uruchamiamy go na tabeli juz majacej activation_seq.
ALTER TABLE fact_state ADD COLUMN activation_seq INTEGER NOT NULL DEFAULT 0;

-- Indeks kursora A_det: skan czyta WHERE active=1 AND activation_seq > :cur
-- ORDER BY activation_seq LIMIT :batch. Bez tego indeksu kazdy batch robilby
-- full-scan fact_state po activation_seq (hot-path skanu konfliktow).
CREATE INDEX IF NOT EXISTS ix_fact_state_activation ON fact_state(active, activation_seq);

-- Indeks detekcji wspol-faktow: A_det dla kazdego aktywnego faktu pyta
-- WHERE active=1 AND head_id=? AND rel=? AND tail_id<>?. Z samym ix_fact_state_active
-- (003) to skan wszystkich aktywnych faktow per fakt => O(n^2) na popularnym head_id+rel.
-- Zlozony indeks (active, head_id, rel) zaweza do grupy konfliktowej; tail_id w indeksie
-- pozwala odsiac rowny tail bez czytania wiersza.
CREATE INDEX IF NOT EXISTS ix_fact_state_peer ON fact_state(active, head_id, rel, tail_id);
