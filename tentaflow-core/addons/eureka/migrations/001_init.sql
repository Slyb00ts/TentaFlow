-- =============================================================================
-- Plik: addons/eureka/migrations/001_init.sql
-- Opis: Lokalny indeks wpisow Eureka MF oraz checkpointy wznawialnego crawlera.
-- =============================================================================

CREATE TABLE eureka_entries (
    id INTEGER PRIMARY KEY,
    url TEXT NOT NULL,
    title TEXT NOT NULL,
    template_name TEXT NOT NULL,
    signature TEXT NOT NULL,
    thesis TEXT NOT NULL,
    publication_date TEXT NOT NULL,
    issue_date TEXT NOT NULL,
    content_text TEXT NOT NULL,
    content_html TEXT NOT NULL,
    metadata_json TEXT NOT NULL,
    source_hash INTEGER NOT NULL,
    fetched_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE INDEX idx_eureka_entries_publication_date ON eureka_entries(publication_date);
CREATE INDEX idx_eureka_entries_signature ON eureka_entries(signature);
CREATE INDEX idx_eureka_entries_title ON eureka_entries(title);

CREATE TABLE eureka_sync_state (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE eureka_fetch_status (
    id INTEGER PRIMARY KEY,
    status TEXT NOT NULL,
    http_status INTEGER,
    error TEXT NOT NULL DEFAULT '',
    attempts INTEGER NOT NULL DEFAULT 0,
    last_attempt_at INTEGER NOT NULL
);

CREATE INDEX idx_eureka_fetch_status_status ON eureka_fetch_status(status);
CREATE INDEX idx_eureka_fetch_status_last_attempt ON eureka_fetch_status(last_attempt_at);
