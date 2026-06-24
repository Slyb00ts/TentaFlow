// =============================================================================
// Plik: flow_engine/node_adapters/pdf_rasterize.rs
// Opis: PdfRasterizeNodeAdapter — rasteryzuje PDF (payload Other) na obrazy stron
//       (PNG) przez współdzielony `rasterize_pdf_streaming`. Strony lądują w
//       blob store (ctx.blobs); wyjście to FlowValue::Json z listą blob-refów
//       (fan-out stron w modelu 1:1 cardinality — patrz uwaga niżej). Bez modelu.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};
use crate::services::document::rasterize::{rasterize_pdf_streaming, PageRender, SinkClosed};
use crate::services::document::{DEFAULT_RENDER_DPI, MAX_PDF_PAGES};

const NODE_TYPE: &str = "pdf_rasterize";

const PAGE_PNG_MIME: &str = "image/png";

pub struct PdfRasterizeNodeAdapter;

impl PdfRasterizeNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    /// DPI z node.config (domyślnie DEFAULT_RENDER_DPI). Wartość <= 0 / nie-liczba
    /// → domyślne (rasteryzer i tak klampuje, ale walidujemy tu czytelnie).
    fn pick_dpi(node: &FlowNode) -> f32 {
        node.config
            .get("dpi")
            .and_then(|v| v.as_f64())
            .filter(|d| d.is_finite() && *d > 0.0)
            .map(|d| d as f32)
            .unwrap_or(DEFAULT_RENDER_DPI)
    }

    /// Cap stron z node.config (domyślnie MAX_PDF_PAGES). Nadrzędny cap
    /// MAX_PDF_PAGES jest i tak egzekwowany przez rasteryzer — node tylko może go
    /// dodatkowo zacieśnić.
    fn pick_max_pages(node: &FlowNode) -> u32 {
        node.config
            .get("max_pages")
            .and_then(|v| v.as_u64())
            .map(|n| (n as u32).min(MAX_PDF_PAGES))
            .unwrap_or(MAX_PDF_PAGES)
            .max(1)
    }
}

impl Default for PdfRasterizeNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for PdfRasterizeNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }

    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Other)]
    }

    fn output_ports(&self) -> Vec<PortSpec> {
        // Lista stron-obrazów jako Json (blob-refy). Engine ma cardinality 1:1
        // (envelope.rs hard rule 5), więc zamiast N envelope na porcie emitujemy
        // JEDEN envelope z listą — downstream (document_merge / vision per-page)
        // iteruje po `pages`. Json a nie Image, bo to KOLEKCJA obrazów.
        vec![PortSpec::new("images", FlowDataType::Json)]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("pdf_rasterize: brak krawędzi wejściowej"))?;
        let envelope = &input.envelope;

        let (blob_ref, mime) = match &envelope.payload {
            FlowValue::Other { blob_ref, mime, .. } => (blob_ref.clone(), mime.clone()),
            other => {
                return Err(anyhow!(
                    "pdf_rasterize: payload musi być Other(pdf), dostał {}",
                    other.kind()
                ))
            }
        };

        let pdf_bytes = ctx
            .blobs
            .get(&blob_ref)
            .await
            .map_err(|e| anyhow!("pdf_rasterize: pobranie PDF: {e}"))?;
        if pdf_bytes.is_empty() {
            return Err(anyhow!("pdf_rasterize: pusty plik PDF (mime={mime})"));
        }

        let dpi = Self::pick_dpi(node);
        let max_pages = Self::pick_max_pages(node);

        // Rasteryzacja jest blokująca (FFI pdfium + kodowanie PNG) — odpalamy ją w
        // spawn_blocking, zbierając wyrenderowane strony przez sink. Reużywamy
        // `rasterize_pdf_streaming` (executor.rs::execute_documents_pdf): O(1)
        // pamięci na render strony, cap-y anti-DoS po stronie rasteryzera. Tu
        // zbieramy PNG-i eager (potrzebujemy ich wszystkich do listy blob-refów),
        // ale render→PNG nadal odbywa się strona-po-stronie.
        let pages: Vec<PageRender> = tokio::task::spawn_blocking(move || {
            let mut collected: Vec<PageRender> = Vec::new();
            rasterize_pdf_streaming(&pdf_bytes, dpi, max_pages, |page| {
                collected.push(page);
                Ok::<(), SinkClosed>(())
            })?;
            Ok::<Vec<PageRender>, crate::services::document::rasterize::RasterizeError>(collected)
        })
        .await
        .map_err(|e| anyhow!("pdf_rasterize: join rasteryzacji: {e}"))?
        .map_err(|e| anyhow!("pdf_rasterize: {e}"))?;

        if pages.is_empty() {
            return Err(anyhow!("pdf_rasterize: PDF nie ma renderowalnych stron"));
        }

        // Każda strona → blob PNG w ctx.blobs; lista blob-refów + metadane w Json.
        let mut page_entries: Vec<serde_json::Value> = Vec::with_capacity(pages.len());
        for page in pages {
            let page_ref = ctx
                .blobs
                .put(page.png, PAGE_PNG_MIME)
                .await
                .map_err(|e| anyhow!("pdf_rasterize: zapis strony {}: {e}", page.index))?;
            page_entries.push(serde_json::json!({
                "index": page.index,
                "blob_id": page_ref.id,
                "sha256": page_ref.sha256,
                "size_bytes": page_ref.size_bytes,
                "mime": PAGE_PNG_MIME,
            }));
        }

        let page_count = page_entries.len();
        let mut out = (**envelope).clone();
        out.payload = FlowValue::Json(serde_json::json!({
            "kind": "pdf_pages",
            "page_count": page_count,
            "pages": page_entries,
        }));
        out.meta.insert(
            "pdf_page_count".to_string(),
            serde_json::json!(page_count),
        );
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::blob_store::BlobStore;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use std::sync::Arc;

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "raster-1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    /// Minimalny 2-stronicowy PDF z rasteryzera (test-only generator), wrzucony do
    /// blob store ctx i opakowany w envelope Other.
    async fn pdf_input(ctx: &ExecutionContext, pages: usize) -> NodeInput {
        let pdf = crate::services::document::rasterize::minimal_pdf(pages);
        let blob_ref = ctx.blobs.put(pdf, "application/pdf").await.unwrap();
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Other {
            blob_ref,
            mime: "application/pdf".into(),
            filename: Some("doc.pdf".into()),
        };
        NodeInput {
            from_node_id: "router".into(),
            from_port: "pdf".into(),
            envelope: Arc::new(env),
        }
    }

    #[tokio::test]
    async fn rasterizes_each_page_to_blob_ref() {
        let ctx = stub_ctx();
        let input = pdf_input(&ctx, 2).await;
        let out = PdfRasterizeNodeAdapter::new()
            .execute(&node(serde_json::json!({"dpi": 100})), &[input], &ctx)
            .await
            .unwrap();

        let pages = match &out.payload {
            FlowValue::Json(v) => v.get("pages").and_then(|p| p.as_array()).cloned().unwrap(),
            other => panic!("expected Json, got {other:?}"),
        };
        assert_eq!(pages.len(), 2, "dwie strony PDF → dwa blob-refy");
        assert_eq!(pages[0]["index"].as_u64(), Some(0));
        assert_eq!(pages[1]["index"].as_u64(), Some(1));
        assert_eq!(pages[0]["mime"].as_str(), Some("image/png"));
        assert_eq!(out.meta.get("pdf_page_count").and_then(|v| v.as_u64()), Some(2));

        // Blob-refy są realnie odczytywalne i to PNG-i.
        for page in &pages {
            let blob_ref = crate::flow_engine::blob_store::BlobRef {
                id: page["blob_id"].as_str().unwrap().to_string(),
                size_bytes: page["size_bytes"].as_u64().unwrap(),
                mime: "image/png".into(),
                sha256: page["sha256"].as_str().unwrap().to_string(),
            };
            let bytes = ctx.blobs.get(&blob_ref).await.unwrap();
            assert_eq!(&bytes[..8], &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']);
        }
    }

    #[tokio::test]
    async fn max_pages_caps_output() {
        let ctx = stub_ctx();
        let input = pdf_input(&ctx, 5).await;
        let out = PdfRasterizeNodeAdapter::new()
            .execute(&node(serde_json::json!({"dpi": 80, "max_pages": 2})), &[input], &ctx)
            .await
            .unwrap();
        let count = match &out.payload {
            FlowValue::Json(v) => v.get("page_count").and_then(|n| n.as_u64()).unwrap(),
            _ => panic!("expected Json"),
        };
        assert_eq!(count, 2, "max_pages zacieśnia liczbę renderowanych stron");
    }

    #[tokio::test]
    async fn rejects_non_pdf_payload() {
        let ctx = stub_ctx();
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("not a pdf".into());
        let input = NodeInput {
            from_node_id: "x".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        };
        let err = PdfRasterizeNodeAdapter::new()
            .execute(&node(serde_json::json!({})), &[input], &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("musi być Other"), "{err}");
    }
}
