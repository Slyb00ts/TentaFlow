-- =============================================================================
-- File: addons/notes/migrations/002_analysis.sql
-- Purpose: auto-graph analysis pipeline state. analysis_queue is the durable
--          work queue (save/delete enqueue, opportunistic UI-drain + the
--          analyze_pending tool consume). merge_suggestions holds gray-band
--          entity-merge candidates awaiting a human decision. note_chunks
--          records how many chunk vectors a note has in the 'notes' namespace,
--          so re-analysis and tombstoning can delete stale ref_ids.
-- =============================================================================

-- One pending row per note. INSERT OR REPLACE on every save resets attempts —
-- fresh content deserves fresh retries. attempts/last_error make failures
-- visible in the panel instead of silently looping.
CREATE TABLE IF NOT EXISTS analysis_queue (
  note_id     TEXT PRIMARY KEY,
  enqueued_at INTEGER NOT NULL,
  attempts    INTEGER NOT NULL DEFAULT 0,
  last_error  TEXT
);

-- Gray-band merge candidates (similarity in [0.80, 0.95) for same-type
-- entities). entity_a/entity_b are stored in canonical order (min, max) so the
-- partial-unique index dedups the pair regardless of detection direction.
CREATE TABLE IF NOT EXISTS merge_suggestions (
  id         TEXT PRIMARY KEY,
  entity_a   TEXT NOT NULL,
  entity_b   TEXT NOT NULL,
  similarity REAL NOT NULL,
  status     TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'accepted', 'rejected')),
  created_at INTEGER NOT NULL,
  decided_at INTEGER,
  decided_by TEXT
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_merge_suggestions_pair_open
  ON merge_suggestions(entity_a, entity_b) WHERE status = 'open';
CREATE INDEX IF NOT EXISTS ix_merge_suggestions_status ON merge_suggestions(status);

-- Chunk-vector registry: ref_ids in the 'notes' namespace are derived from
-- (note_id, chunk_index). chunk_count = currently registered chunks;
-- max_chunk_count = historical high-water mark, bumped BEFORE embedding so a
-- failed analysis can never leave vectors above the recorded range — the
-- tombstone cleanup deletes 0..max_chunk_count and catches such orphans.
CREATE TABLE IF NOT EXISTS note_chunks (
  note_id         TEXT PRIMARY KEY,
  chunk_count     INTEGER NOT NULL,
  max_chunk_count INTEGER NOT NULL,
  updated_at      INTEGER NOT NULL
);
