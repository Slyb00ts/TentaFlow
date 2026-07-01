-- Dedup dokumentow po tresci: content_hash = sha256 pliku (host store'uje bloby
-- content-addressed po sha256). Wgranie TEGO SAMEGO pliku do kolekcji nie tworzy
-- juz duplikatu dokumentu/chunkow/wektorow — ingest wykrywa istniejacy content_hash
-- w kolekcji i pomija. Kolumna nullable: stare dokumenty (bez hasha) zostaja.
ALTER TABLE documents ADD COLUMN content_hash TEXT;

-- Lookup dedup per kolekcja. NIE-unique: nieudane (failed) proby moga zostawic
-- ten sam hash, a dedup w kodzie filtruje po status != 'failed'.
CREATE INDEX IF NOT EXISTS ix_documents_coll_hash
  ON documents(collection_id, content_hash);
