-- =============================================================================
-- File: addons/sdk-showcase/migrations/001_init.sql
-- Purpose: test schema for the SQL host function suite — one `items` table
--          with a UNIQUE constraint (SqlConstraint probes) and a name index.
-- =============================================================================

CREATE TABLE items (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    qty INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);

CREATE INDEX idx_items_name ON items(name);
