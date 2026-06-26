-- =============================================================================
-- 0002_vector_refs.sql
-- Maps a vector namespace ref_id (u64) back to the alarm string id it was built
-- from. The "events" vector namespace keys each embedding by a numeric hash of
-- the alarm id; a search hit only carries that ref_id, so this table resolves it
-- to the real alarm row (db::get_alarm). One row per indexed alarm.
-- =============================================================================

CREATE TABLE IF NOT EXISTS vector_refs (
    ref_id    INTEGER PRIMARY KEY,
    alarm_id  TEXT NOT NULL,
    ts        INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_vector_refs_alarm ON vector_refs(alarm_id);
