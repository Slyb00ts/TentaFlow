-- =============================================================================
-- Plik: addons/rag/migrations/001_init.sql
-- Opis: Schemat per-instance SQLite addona RAG. Trzyma metadane kolekcji,
--       dokumentow, chunkow i jobow ingestu. Wektory chunkow zyja w przestrzeni
--       wektorowej 'passages' (host vector API); tu trzymamy referencje.
-- =============================================================================

-- Kolekcje dokumentow w obrebie instancji RAG.
CREATE TABLE IF NOT EXISTS collections (
  id          TEXT PRIMARY KEY,
  name        TEXT NOT NULL,
  created_at  INTEGER NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_collections_name
ON collections(name);

-- Dokumenty zrodlowe. doc_id_blob = referencja do per-instance document store
-- (z host-fn document_put), gdzie leza surowe bajty pliku.
CREATE TABLE IF NOT EXISTS documents (
  id             TEXT PRIMARY KEY,
  collection_id  TEXT NOT NULL,
  doc_id_blob    TEXT NOT NULL,
  filename       TEXT NOT NULL,
  mime           TEXT NOT NULL,
  status         TEXT NOT NULL DEFAULT 'pending',
  page_count     INTEGER NOT NULL DEFAULT 0,
  created_at     INTEGER NOT NULL,
  FOREIGN KEY(collection_id) REFERENCES collections(id)
);

CREATE INDEX IF NOT EXISTS ix_documents_collection
ON documents(collection_id);

-- Chunki tekstu. id (INTEGER rowid) sluzy jako ref_id wektora w 'passages'.
-- vector_ref = ten sam id zapisany jawnie, do odwrotnego mapowania trafienia
-- wektorowego na chunk.
CREATE TABLE IF NOT EXISTS chunks (
  id             INTEGER PRIMARY KEY AUTOINCREMENT,
  document_id    TEXT NOT NULL,
  collection_id  TEXT NOT NULL,
  chunk_index    INTEGER NOT NULL,
  text           TEXT NOT NULL,
  vector_ref     INTEGER NOT NULL,
  created_at     INTEGER NOT NULL,
  FOREIGN KEY(document_id) REFERENCES documents(id),
  FOREIGN KEY(collection_id) REFERENCES collections(id)
);

CREATE INDEX IF NOT EXISTS ix_chunks_document
ON chunks(document_id);

CREATE INDEX IF NOT EXISTS ix_chunks_collection
ON chunks(collection_id);

CREATE UNIQUE INDEX IF NOT EXISTS ux_chunks_doc_index
ON chunks(document_id, chunk_index);

-- Joby ingestu — postep i bledy przetwarzania pojedynczego dokumentu.
CREATE TABLE IF NOT EXISTS ingest_jobs (
  id           TEXT PRIMARY KEY,
  document_id  TEXT NOT NULL,
  status       TEXT NOT NULL DEFAULT 'queued',
  progress     INTEGER NOT NULL DEFAULT 0,
  error        TEXT,
  created_at   INTEGER NOT NULL,
  updated_at   INTEGER NOT NULL,
  FOREIGN KEY(document_id) REFERENCES documents(id)
);

CREATE INDEX IF NOT EXISTS ix_ingest_jobs_document
ON ingest_jobs(document_id);

CREATE INDEX IF NOT EXISTS ix_ingest_jobs_status
ON ingest_jobs(status);
