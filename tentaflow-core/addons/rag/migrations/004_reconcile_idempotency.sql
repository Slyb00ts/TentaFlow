-- =============================================================================
-- Plik: addons/rag/migrations/004_reconcile_idempotency.sql
-- Opis: MemGraphRAG slice D2 — idempotencja artefaktow grafu pod RECONCILE.
--       Promocja/aktywacja schematow przeniesiona z predykcji w ingest-tx do
--       osobnego, samonaprawialnego kroku reconcile po commicie ledgera. Reconcile
--       (i rownolegly ingest) moga probowac wstawic ten sam edge-artifact wielokrotnie
--       (krawedz wniesiona przez kilka dokumentow, ponowna aktywacja), wiec rejestr
--       graph_artifacts musi miec UNIKALNY klucz per (dokument, obiekt), a inserty
--       musza byc INSERT OR IGNORE. Bez tego hurtowa aktywacja w petli batchowej
--       duplikowalaby wiersze rejestru i psula refcount cleanupu.
-- =============================================================================

-- DEDUPE PRZED unique indeksami: CREATE UNIQUE INDEX IF NOT EXISTS sprawdza tylko nazwe
-- indeksu, NIE usuwa istniejacych konfliktow. Na bazach sprzed tej migracji graph_artifacts
-- moze juz miec duplikaty (dawny bezwarunkowy INSERT przy aktywacji w petli), wiec samo
-- CREATE UNIQUE INDEX padloby z "UNIQUE constraint failed". Czyscimy zostawiajac MIN(id)
-- per klucz naturalny (najstarszy wiersz = stabilny wybor; refcount liczy DISTINCT document_id,
-- wiec usuniecie dubli w obrebie tego samego klucza nie zmienia refcountu).
DELETE FROM graph_artifacts WHERE kind = 'node' AND id NOT IN (
  SELECT MIN(id) FROM graph_artifacts WHERE kind = 'node' GROUP BY document_id, n_id
);
DELETE FROM graph_artifacts WHERE kind = 'edge' AND id NOT IN (
  SELECT MIN(id) FROM graph_artifacts WHERE kind = 'edge' GROUP BY document_id, src, rel, dst
);

-- Idempotencja rejestru wezlow: jeden wiersz per (dokument, n_id). Inserty wezlow
-- (ingest + aktywacja) ida przez INSERT OR IGNORE, wiec re-ingest/re-aktywacja nie
-- mnozy wierszy. Nazwa UNIKALNA inna niz nie-unique ix_graph_artifacts_node z 002
-- (oba moga wspolistniec; unique pelni role klucza idempotencji, nie-unique zostaje
-- jako indeks refcount-query). Partial index WHERE kind='node' jest poprawny w SQLite.
CREATE UNIQUE INDEX IF NOT EXISTS ux_graph_artifacts_node
ON graph_artifacts(document_id, n_id) WHERE kind = 'node';

-- Idempotencja rejestru krawedzi: jeden wiersz per (dokument, src, rel, dst). Pozwala
-- INSERT OR IGNORE przy hurtowej aktywacji (krawedz aktywowana raz, ale evidence moze
-- pochodzic z wielu chunkow tego dokumentu) bez duplikatu rejestru. Refcount krawedzi
-- liczymy odtad jako COUNT(DISTINCT document_id) — wiersz per dokument jest unikalny,
-- wiec refcount = liczba dokumentow trzymajacych krawedz.
CREATE UNIQUE INDEX IF NOT EXISTS ux_graph_artifacts_edge
ON graph_artifacts(document_id, src, rel, dst) WHERE kind = 'edge';
