// =============================================================================
// Plik: flow_engine/node_adapters/office_extract.rs
// Opis: Trzy node-adaptery ekstrakcji tekstu z dokumentów biurowych do markdown
//       GFM: excel_extract (XLSX→calamine), word_extract (DOCX→quick-xml),
//       pptx_extract (PPTX→zip+quick-xml). Każdy: input Other(plik) → output
//       Text(markdown). Bez modelu — czysta ekstrakcja (logika w
//       services/document/extract.rs).
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};
use crate::services::document::extract::{docx_to_markdown, pptx_to_markdown, xlsx_to_markdown};

/// Wspólny szkielet ekstraktora: pobiera bajty payloadu `Other` z blob store,
/// woła `extractor` (czysta funkcja bytes→markdown) i zwraca envelope z
/// payloadem Text. Cała różnica między excel/word/pptx to wskaźnik na funkcję.
async fn run_extract(
    node_type: &str,
    inputs: &[NodeInput],
    ctx: &ExecutionContext,
    extractor: fn(&[u8]) -> Result<String, String>,
) -> Result<FlowEnvelope> {
    let input = inputs
        .first()
        .ok_or_else(|| anyhow!("{node_type}: brak krawędzi wejściowej"))?;
    let envelope = &input.envelope;

    let blob_ref = match &envelope.payload {
        FlowValue::Other { blob_ref, .. } => blob_ref.clone(),
        other => {
            return Err(anyhow!(
                "{node_type}: payload musi być Other(plik), dostał {}",
                other.kind()
            ))
        }
    };

    let bytes = ctx
        .blobs
        .get(&blob_ref)
        .await
        .map_err(|e| anyhow!("{node_type}: pobranie pliku: {e}"))?;
    if bytes.is_empty() {
        return Err(anyhow!("{node_type}: pusty plik wejściowy"));
    }

    // Ekstrakcja jest CPU-bound (parsowanie ZIP/XML/arkusza). Dla dużych plików
    // robimy ją w spawn_blocking, by nie blokować reaktora tokio.
    let markdown = tokio::task::spawn_blocking(move || extractor(&bytes))
        .await
        .map_err(|e| anyhow!("{node_type}: join ekstrakcji: {e}"))?
        .map_err(|e| anyhow!("{node_type}: {e}"))?;

    if markdown.trim().is_empty() {
        return Err(anyhow!("{node_type}: ekstrakcja nie zwróciła tekstu"));
    }

    let mut out = (**envelope).clone();
    out.payload = FlowValue::Text(markdown);
    Ok(out)
}

/// Wspólne porty dla wszystkich trzech ekstraktorów: Other → Text.
fn office_input_ports() -> Vec<PortSpec> {
    vec![PortSpec::new("in", FlowDataType::Other)]
}
fn office_output_ports() -> Vec<PortSpec> {
    vec![PortSpec::new("full", FlowDataType::Text)]
}

// -----------------------------------------------------------------------------
// excel_extract
// -----------------------------------------------------------------------------

pub struct ExcelExtractNodeAdapter;

impl ExcelExtractNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}
impl Default for ExcelExtractNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for ExcelExtractNodeAdapter {
    fn node_type(&self) -> &str {
        "excel_extract"
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        office_input_ports()
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        office_output_ports()
    }
    async fn execute(
        &self,
        _node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        run_extract("excel_extract", inputs, ctx, xlsx_to_markdown).await
    }
}

// -----------------------------------------------------------------------------
// word_extract
// -----------------------------------------------------------------------------

pub struct WordExtractNodeAdapter;

impl WordExtractNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}
impl Default for WordExtractNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for WordExtractNodeAdapter {
    fn node_type(&self) -> &str {
        "word_extract"
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        office_input_ports()
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        office_output_ports()
    }
    async fn execute(
        &self,
        _node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        run_extract("word_extract", inputs, ctx, docx_to_markdown).await
    }
}

// -----------------------------------------------------------------------------
// pptx_extract
// -----------------------------------------------------------------------------

pub struct PptxExtractNodeAdapter;

impl PptxExtractNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}
impl Default for PptxExtractNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for PptxExtractNodeAdapter {
    fn node_type(&self) -> &str {
        "pptx_extract"
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        office_input_ports()
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        office_output_ports()
    }
    async fn execute(
        &self,
        _node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        run_extract("pptx_extract", inputs, ctx, pptx_to_markdown).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::blob_store::BlobStore;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use std::io::Write;
    use std::sync::Arc;

    fn node(node_type: &str) -> FlowNode {
        FlowNode {
            id: "ext-1".into(),
            node_type: node_type.into(),
            config: serde_json::json!({}),
            position: None,
            label: None,
            region: None,
        }
    }

    async fn other_input(ctx: &ExecutionContext, mime: &str, bytes: Vec<u8>) -> NodeInput {
        let blob_ref = ctx.blobs.put(bytes, mime).await.unwrap();
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Other {
            blob_ref,
            mime: mime.to_string(),
            filename: None,
        };
        NodeInput {
            from_node_id: "router".into(),
            from_port: "xlsx".into(),
            envelope: Arc::new(env),
        }
    }

    /// Buduje minimalny DOCX (ZIP z word/document.xml) z jednym akapitem.
    fn minimal_docx(paragraph: &str) -> Vec<u8> {
        let xml = format!(
            r#"<?xml version="1.0"?><w:document xmlns:w="x"><w:body><w:p><w:r><w:t>{paragraph}</w:t></w:r></w:p></w:body></w:document>"#
        );
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            zw.start_file("word/document.xml", opts).unwrap();
            zw.write_all(xml.as_bytes()).unwrap();
            zw.finish().unwrap();
        }
        buf
    }

    /// Buduje minimalny PPTX (ZIP z ppt/slides/slide1.xml) z jednym akapitem.
    fn minimal_pptx(text: &str) -> Vec<u8> {
        let xml = format!(
            r#"<?xml version="1.0"?><p:sld xmlns:p="x" xmlns:a="y"><a:p><a:r><a:t>{text}</a:t></a:r></a:p></p:sld>"#
        );
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            zw.start_file("ppt/slides/slide1.xml", opts).unwrap();
            zw.write_all(xml.as_bytes()).unwrap();
            zw.finish().unwrap();
        }
        buf
    }

    #[tokio::test]
    async fn word_extract_emits_markdown_text() {
        let ctx = stub_ctx();
        let docx = minimal_docx("Treść dokumentu testowego.");
        let input = other_input(
            &ctx,
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            docx,
        )
        .await;
        let out = WordExtractNodeAdapter::new()
            .execute(&node("word_extract"), &[input], &ctx)
            .await
            .unwrap();
        match out.payload {
            FlowValue::Text(t) => assert!(t.contains("Treść dokumentu testowego")),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pptx_extract_emits_slide_markdown() {
        let ctx = stub_ctx();
        let pptx = minimal_pptx("Tytuł slajdu");
        let input = other_input(
            &ctx,
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            pptx,
        )
        .await;
        let out = PptxExtractNodeAdapter::new()
            .execute(&node("pptx_extract"), &[input], &ctx)
            .await
            .unwrap();
        match out.payload {
            FlowValue::Text(t) => {
                assert!(t.contains("Slajd 1"), "markdown: {t}");
                assert!(t.contains("Tytuł slajdu"), "markdown: {t}");
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn excel_extract_rejects_non_other_payload() {
        let ctx = stub_ctx();
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("nope".into());
        let input = NodeInput {
            from_node_id: "x".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        };
        let err = ExcelExtractNodeAdapter::new()
            .execute(&node("excel_extract"), &[input], &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("musi być Other"), "{err}");
    }
}
