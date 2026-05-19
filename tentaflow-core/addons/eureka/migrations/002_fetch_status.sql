-- =============================================================================
-- Plik: addons/eureka/migrations/002_fetch_status.sql
-- Opis: Status pobrania kazdego identyfikatora Eureka dla retry i audytu zrzutu.
-- =============================================================================

CREATE TABLE IF NOT EXISTS eureka_fetch_status (
    id INTEGER PRIMARY KEY,
    status TEXT NOT NULL,
    http_status INTEGER,
    error TEXT NOT NULL DEFAULT '',
    attempts INTEGER NOT NULL DEFAULT 0,
    last_attempt_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_eureka_fetch_status_status ON eureka_fetch_status(status);
CREATE INDEX IF NOT EXISTS idx_eureka_fetch_status_last_attempt ON eureka_fetch_status(last_attempt_at);
