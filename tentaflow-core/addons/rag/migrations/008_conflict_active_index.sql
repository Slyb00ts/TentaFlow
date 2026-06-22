-- =============================================================================
-- Plik: addons/rag/migrations/008_conflict_active_index.sql
-- Opis: MemGraphRAG slice D4 (A_res) — domkniecie wyscigu open<->resolving. Partial-unique
--       ux_conflicts_open (z 005) chronil JEDEN konflikt per grupa TYLKO dla status='open'.
--       Gdy A_res przestawia open->resolving (claim), klucz dedup_key sie zwalnia, wiec A_det
--       (D3) upsert_group_conflict moglby wstawic DRUGI 'open' dla tej samej grupy podczas gdy
--       pierwszy jest jeszcze adjudykowany ('resolving') — dwa nakladajace sie konflikty
--       lifecycle tej samej grupy (head,rel). Tu rozszerzamy unikalnosc na CALY aktywny cykl
--       zycia: open ORAZ resolving. Dzieki temu istnieje najwyzej JEDEN aktywny konflikt na
--       grupe niezaleznie od fazy adjudykacji; A_det dokłada nowych czlonkow do istniejacego
--       (INSERT OR IGNORE conflict_members), a A_res re-adjudykuje gdy member_set_hash sie
--       zmieni (cache invaliduje). Po zamknieciu (resolved_*/escalated) klucz sie zwalnia —
--       gdyby grupa znow weszla w konflikt, A_det otwiera nowy.
-- =============================================================================

DROP INDEX IF EXISTS ux_conflicts_open;

CREATE UNIQUE INDEX IF NOT EXISTS ux_conflicts_active
  ON conflicts(dedup_key) WHERE status IN ('open', 'resolving');
