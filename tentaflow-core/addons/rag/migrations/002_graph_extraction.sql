-- =============================================================================
-- Plik: addons/rag/migrations/002_graph_extraction.sql
-- Opis: Schemat pod ekstrakcje encji/relacji (GraphRAG, Etap 2). Dodaje liczniki
--       wyekstrahowanych encji/relacji do dokumentow oraz tabele sledzaca
--       artefakty grafu (node-id i klucze krawedzi) per dokument, by cleanup-on-
--       failure i re-ingest mogly ODWRACALNIE skasowac graf tego dokumentu.
-- =============================================================================

-- Liczniki ekstrakcji per dokument (graf best-effort: moga byc 0 mimo udanego
-- ingestu wektorowego). graph_partial = 1 gdy ekstrakcja sie nie powiodla lub
-- zostala obcieta, a wektory i tak sa kompletne.
ALTER TABLE documents ADD COLUMN entity_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE documents ADD COLUMN relation_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE documents ADD COLUMN graph_partial INTEGER NOT NULL DEFAULT 0;

-- Rejestr artefaktow grafu utworzonych dla dokumentu. Graf (kolekcja 'kg') zyje
-- w osobnym silniku (host graph API), wiec tu trzymamy referencje do skasowania:
-- kind='node' -> n_id to id wezla; kind='edge' -> (src, rel, dst) to klucz krawedzi.
--
-- Encje maja id = znormalizowana nazwa, wiec ten sam node_id / klucz krawedzi jest
-- WSPOLDZIELONY miedzy dokumentami (istota GraphRAG: wiele dokumentow mowi o tej
-- samej "einstein"). Rejestr trzyma OSOBNY wiersz per (document_id, obiekt), wiec
-- pelni role REFCOUNTU: wezel/krawedz zyje w grafie dopoki referuje go jakikolwiek
-- dokument, a cleanup kasuje go z grafu dopiero gdy refcount spadnie do 0 (zaden
-- inny dokument juz nie ma wiersza dla tego node_id / klucza krawedzi).
CREATE TABLE IF NOT EXISTS graph_artifacts (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  document_id  TEXT NOT NULL,
  kind         TEXT NOT NULL,
  n_id         TEXT,
  src          TEXT,
  rel          TEXT,
  dst          TEXT,
  created_at   INTEGER NOT NULL,
  FOREIGN KEY(document_id) REFERENCES documents(id)
);

CREATE INDEX IF NOT EXISTS ix_graph_artifacts_document
ON graph_artifacts(document_id);

-- Indeksy pod refcount-query cleanupu: po skasowaniu wierszy dokumentu A pytamy
-- czy ISTNIEJE jeszcze JAKIKOLWIEK wiersz referujacy dany node_id / klucz krawedzi
-- (innego dokumentu). Bez indeksu byloby to skanowanie calej tabeli per obiekt.
CREATE INDEX IF NOT EXISTS ix_graph_artifacts_node
ON graph_artifacts(n_id) WHERE kind = 'node';

CREATE INDEX IF NOT EXISTS ix_graph_artifacts_edge
ON graph_artifacts(src, rel, dst) WHERE kind = 'edge';
