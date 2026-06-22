-- =============================================================================
-- Plik: addons/rag/migrations/007_conflict_resolution.sql
-- Opis: MemGraphRAG slice D4 — agent adjudykacji konfliktow A_res (LLM, evidence-driven,
--       z twardymi limitami kosztu R8). conflict_resolve (osobny tool od conflict_scan)
--       bierze OPEN konflikty z 'conflicts' (D3), zbiera evidence (conflict_members ->
--       fact_state -> fact_evidence -> chunks) z capem tokenow, woła rag-llm i ZASTOSOWUJE
--       decyzje ODWRACALNIE (tombstone loserow przez graph_outbox / temporal_split /
--       merge_pending / escalate). Ta migracja rozszerza 'conflicts' o pola sterujace
--       claimem exactly-once, recovery po crashu i cache (eliminacja re-adjudykacji).
--       Zrodlem prawdy pozostaje SQLite addona (R1) — A_res czyta i decyduje WYLACZNIE
--       z tych tabel; graf jest tylko karmiony tombstone'ami przez outbox (R3).
-- =============================================================================

-- resolved_members_hash: kanoniczny hash POSORTOWANYCH fact_keys czlonkow grupy w chwili
-- ostatniej adjudykacji (member_set_hash). Cache R8: jesli przy kolejnym przebiegu zbior
-- czlonkow grupy ma IDENTYCZNY hash i decision != NULL, A_res NIE woła ponownie LLM (zbior
-- sie nie zmienil => decyzja nadal obowiazuje) — utrzymuje/reaplikuje istniejaca decyzje.
-- To eliminuje re-adjudykacje przy re-ingescie / re-skanie tej samej grupy. NULL = jeszcze
-- nie adjudykowano (D3 wstawia open bez decyzji).
ALTER TABLE conflicts ADD COLUMN resolved_members_hash TEXT;

-- updated_at: znacznik ostatniej zmiany statusu konfliktu (claim open->resolving, zapis
-- decyzji). Sluzy RECOVERY: konflikt zaklinowany w 'resolving' (crash A_res w trakcie
-- adjudykacji, przed zapisem decyzji) starszy niz CONFLICT_RESOLVE_RESOLVING_TTL jest w
-- kolejnym przebiegu traktowany jak open (re-claim). Bez tego pola crash zostawialby
-- konflikt w 'resolving' na zawsze (nigdy nie adjudykowany, nigdy nie odblokowany).
-- D3 wstawial open BEZ updated_at; backfill ponizej ustawia je na created_at dla istniejacych
-- wierszy, by recovery mialo deterministyczny punkt odniesienia (a nie NULL).
ALTER TABLE conflicts ADD COLUMN updated_at INTEGER;

-- Backfill updated_at dla konfliktow sprzed tej migracji: dla open bez znacznika ustaw na
-- created_at (a gdy i ten NULL — 0, czyli "bardzo stary", co przy recovery jest bezpieczne:
-- open i tak jest re-claimowalny, a ewentualny stary 'resolving' z NULL trafi do recovery).
UPDATE conflicts SET updated_at = COALESCE(created_at, 0) WHERE updated_at IS NULL;

-- Indeks pod CLAIM/RECOVERY A_res: przebieg pobiera kandydatow
-- WHERE status='open' OR (status='resolving' AND updated_at < :now-TTL) ORDER BY id.
-- ix_conflicts_status (z 005) pokrywa filtr po samym status; (status, updated_at) zaweza
-- dodatkowo galaz recovery (resolving + prog czasowy) bez skanu po updated_at.
CREATE INDEX IF NOT EXISTS ix_conflicts_status_updated ON conflicts(status, updated_at);

-- resolve_owner: UNIKALNY token przebiegu, ktory przejal konflikt (claim open->resolving).
-- Sluzy OWNERSHIP exactly-once przy zastosowaniu decyzji: caly apply+finalize jest jedna
-- transakcja, a KAZDY write jest warunkowany na (status='resolving' AND resolve_owner=:token).
-- Recovery po TTL pozwala DRUGIEMU przebiegowi przejac konflikt (gdy LLM pierwszego trwa
-- dluzej niz TTL) — gdyby pierwszy przebieg wrocil z odpowiedzia LLM PO przejeciu przez
-- drugiego, jego warunki ownershipu sa juz falszywe => wszystkie writy to no-op (brak
-- podwojnego apply, brak rozjazdu loser-active=0-bez-tombstone). NULL = konflikt nie jest
-- aktualnie przejety (open lub juz domkniety). claim_conflict ustawia token, finalize go
-- zostawia (slad audytowy ostatniego wlasciciela), recovery nadpisze przy re-claimie.
ALTER TABLE conflicts ADD COLUMN resolve_owner TEXT;

-- members_rev: TWARDY straznik TOCTOU zbioru czlonkow podczas adjudykacji. A_res czyta
-- czlonkow i decyduje DLUGO (LLM), w tym czasie D3 (upsert_group_conflict) moze dopisac
-- NOWEGO czlonka do konfliktu 'resolving'. Bez wersjonowania A_res zamknalby konflikt na
-- STARYM (niepelnym) zbiorze, a kursor conflict_scan (activation_seq) juz minal nowy fakt
-- => nowy konflikt nigdy nie wymusilby re-adjudykacji => decyzja na nieaktualnym zbiorze.
-- Mechanizm: members_rev to AUTORYTATYWNA liczba czlonkow grupy = COUNT(*) conflict_members
-- (NIE inkrement), ustawiana PODZAPYTANIEM `SELECT COUNT(*)` w TEJ SAMEJ transakcji co inserty
-- czlonkow (jeden commit). To czyni atomowym inwariant "czlonek widoczny ⟺ members_rev to
-- odzwierciedla" i eliminuje wyscig dwoch rownoleglych inkrementow (wzor z D2 freq=COUNT).
-- A_res czyta rev0 przy claimie PRZED snapshotem czlonkow i KAZDY write apply/finalize warunkuje
-- dodatkowo na members_rev=:rev0. Jesli zbior urosl podczas LLM (members_rev > rev0) -> wszystkie
-- writy no-op, decyzja odrzucona, a status wraca do 'open' (re-claim re-czyta SWIEZY pelny zbior).
-- Idempotentny INSERT istniejacego czlonka nie zmienia COUNT (brak falszywych odrzucen przy
-- re-ingescie tego samego).
ALTER TABLE conflicts ADD COLUMN members_rev INTEGER NOT NULL DEFAULT 0;
