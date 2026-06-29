// =============================================================================
// Plik: flow_engine/dispatchers/documents.rs
// Opis: DocumentsDispatcher — narrow trait dla node-adapterów flow-ingestu RAG
//       (page_detect / table_structure / graphic_elements / ocr — PARTIA 2).
//       Wrapper nad `ModelRuntimeExecutor::execute_document_infer` (typed surface
//       `Documents`, `/v1/infer`) z tym samym failoverem aliasów co rerank.
//       Adapter widzi tylko obraz + task, dostaje listę regionów (`DocRegion`).
// =============================================================================

use async_trait::async_trait;

use tentaflow_protocol::DocumentInferResult;

#[async_trait]
pub trait DocumentsDispatcher: Send + Sync {
    /// Detekcja struktury strony dokumentu. `task` ∈ {"page_elements",
    /// "table_structure", "graphic_elements", "ocr"}. Błąd jako `String`, bo
    /// node-adaptery składają go w `FlowError` po swojej stronie.
    async fn infer(
        &self,
        model: &str,
        image: &[u8],
        mime: &str,
        task: &str,
    ) -> Result<DocumentInferResult, String>;

    /// Parsuje stronę dokumentu (obraz) na markdown ze strukturą (tabele GFM/
    /// HTML, wzory, kolejność czytania) przez powierzchnię document-parse
    /// (`execute_documents`). Backend dobiera resolver wg urządzenia z failoverem
    /// aliasu: embedded (PaddleOCR-VL MLX na Apple), HTTP (docker nemotron-parse/
    /// paddle-ocr), QUIC, mesh-forward. Zwraca markdown.
    async fn parse(&self, model: &str, image: &[u8], mime: &str) -> Result<String, String>;
}
