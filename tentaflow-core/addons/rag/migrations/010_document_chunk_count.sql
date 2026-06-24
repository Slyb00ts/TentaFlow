-- =============================================================================
-- Plik: addons/rag/migrations/010_document_chunk_count.sql
-- Opis: Liczba chunkow per dokument utrwalona na wierszu `documents`. Wektory
--       pisze teraz wezel `store` flow-ingestu WPROST do przestrzeni `passages`
--       (Core), wiec addonowa tabela `chunks` nie jest juz zapelniana i COUNT(*)
--       z niej zwracalby 0. Zapisujemy liczbe chunkow zwrocona przez flow-ingest
--       (IngestInvokeOutput.chunks), zeby lista dokumentow pokazywala realny
--       chunk_count bez odpytywania indeksu wektorowego per wiersz.
-- =============================================================================

ALTER TABLE documents ADD COLUMN chunk_count INTEGER NOT NULL DEFAULT 0;
