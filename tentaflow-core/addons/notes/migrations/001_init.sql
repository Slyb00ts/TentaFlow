-- =============================================================================
-- File: addons/notes/migrations/001_init.sql
-- Purpose: full Notes schema for the whole plan (notes CRUD + ACL shares now;
--          entities, note links, graph outbox and entity merge log are laid
--          down up front so the auto-graph/search/dictation stages need no
--          schema migration). SQLite is the source of truth; the graph
--          collection 'notes_kg' is a rebuildable materialization of
--          graph_outbox (same pattern as rag/003_memgraph.sql).
-- =============================================================================

-- Notes. Soft delete via deleted_at (auto-graph must be able to tombstone
-- vectors/edges of a deleted note asynchronously before the row is purged).
CREATE TABLE IF NOT EXISTS notes (
  id             TEXT PRIMARY KEY,
  org_id         TEXT NOT NULL,
  owner_user_id  TEXT NOT NULL,
  title          TEXT NOT NULL DEFAULT '',
  content        TEXT NOT NULL DEFAULT '',
  content_format TEXT NOT NULL DEFAULT 'markdown',
  origin         TEXT NOT NULL DEFAULT 'typed' CHECK (origin IN ('typed', 'dictated')),
  created_at     INTEGER NOT NULL,
  updated_at     INTEGER NOT NULL,
  deleted_at     INTEGER
);

-- ACL shares. subject_id: user id / group id / '' for subject_type='org'
-- (org rows apply to the whole organization of the note).
CREATE TABLE IF NOT EXISTS note_shares (
  note_id      TEXT NOT NULL,
  subject_type TEXT NOT NULL CHECK (subject_type IN ('user', 'group', 'org')),
  subject_id   TEXT NOT NULL DEFAULT '',
  access       TEXT NOT NULL DEFAULT 'read' CHECK (access IN ('read', 'write')),
  created_by   TEXT NOT NULL,
  created_at   INTEGER NOT NULL,
  PRIMARY KEY (note_id, subject_type, subject_id)
);

CREATE TABLE IF NOT EXISTS note_tags (
  note_id TEXT NOT NULL,
  tag     TEXT NOT NULL,
  PRIMARY KEY (note_id, tag)
);

-- Detected entities (person / company / project / topic). canonical_id points
-- to the surviving entity after a merge (NULL = the entity is canonical).
CREATE TABLE IF NOT EXISTS entities (
  id           TEXT PRIMARY KEY,
  org_scope    TEXT NOT NULL,
  name         TEXT NOT NULL,
  entity_type  TEXT NOT NULL,
  canonical_id TEXT
);

-- Entity occurrences per note. first_span = "start:end" byte offsets of the
-- first occurrence in content, count = number of occurrences.
CREATE TABLE IF NOT EXISTS note_entities (
  note_id    TEXT NOT NULL,
  entity_id  TEXT NOT NULL,
  first_span TEXT,
  count      INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY (note_id, entity_id)
);

-- Note-to-note links produced by the auto-graph (semantic similarity, shared
-- entity) or added manually. weight in [0,1]; reason is a human-readable label.
CREATE TABLE IF NOT EXISTS note_links (
  src_note_id TEXT NOT NULL,
  dst_note_id TEXT NOT NULL,
  kind        TEXT NOT NULL CHECK (kind IN ('similar', 'entity', 'manual')),
  weight      REAL NOT NULL DEFAULT 0,
  reason      TEXT NOT NULL DEFAULT '',
  created_at  INTEGER NOT NULL,
  PRIMARY KEY (src_note_id, dst_note_id, kind)
);

-- Graph materialization outbox: writers record a durable intent, a separate
-- idempotent drain applies rows WHERE applied=0 to 'notes_kg' via graph_* host
-- fns and marks applied=1. dedup_key is a canonical encoding of
-- (op, collection, key); dedup covers ONLY pending rows (partial unique) so a
-- re-insert after cleanup re-materializes the graph — see rag/003 rationale.
CREATE TABLE IF NOT EXISTS graph_outbox (
  seq          INTEGER PRIMARY KEY AUTOINCREMENT,
  dedup_key    TEXT NOT NULL,
  op           TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  applied      INTEGER NOT NULL DEFAULT 0,
  created_at   INTEGER NOT NULL
);

-- Reversibility of entity merges: enough to rebuild the inverse outbox and
-- restore the pre-merge state (undone_at set when reverted).
CREATE TABLE IF NOT EXISTS entity_merge_log (
  id             TEXT PRIMARY KEY,
  from_entity_id TEXT NOT NULL,
  into_entity_id TEXT NOT NULL,
  merged_at      INTEGER NOT NULL,
  undone_at      INTEGER
);

-- Hot paths: listing (owner, updated_at DESC, alive only), ACL share lookup,
-- entity/link lookups per note, outbox drain.
CREATE INDEX IF NOT EXISTS ix_notes_owner_updated ON notes(owner_user_id, updated_at DESC);
CREATE INDEX IF NOT EXISTS ix_notes_org_updated   ON notes(org_id, updated_at DESC);
CREATE INDEX IF NOT EXISTS ix_notes_deleted       ON notes(deleted_at);
CREATE INDEX IF NOT EXISTS ix_note_shares_subject ON note_shares(subject_type, subject_id);
CREATE INDEX IF NOT EXISTS ix_note_tags_tag       ON note_tags(tag);
CREATE INDEX IF NOT EXISTS ix_entities_canonical  ON entities(canonical_id);
CREATE INDEX IF NOT EXISTS ix_entities_type_name  ON entities(entity_type, name);
CREATE INDEX IF NOT EXISTS ix_note_entities_ent   ON note_entities(entity_id);
CREATE INDEX IF NOT EXISTS ix_note_links_dst      ON note_links(dst_note_id);
CREATE INDEX IF NOT EXISTS ix_graph_outbox_appl   ON graph_outbox(applied);
CREATE UNIQUE INDEX IF NOT EXISTS ux_graph_outbox_pending ON graph_outbox(dedup_key) WHERE applied = 0;
