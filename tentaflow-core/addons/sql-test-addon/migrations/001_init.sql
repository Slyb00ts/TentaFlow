-- =============================================================================
-- Plik: addons/sql-test-addon/migrations/001_init.sql
-- Opis: Schemat testowy F1a M1.W4 — jedna tabela `items` z UNIQUE constraint
--       (do testow SqlConstraint) i indeksem na name.
-- =============================================================================

CREATE TABLE items (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    qty INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);

CREATE INDEX idx_items_name ON items(name);
