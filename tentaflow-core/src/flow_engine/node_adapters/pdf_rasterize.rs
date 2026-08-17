// =============================================================================
// Plik: flow_engine/node_adapters/pdf_rasterize.rs
// Opis: PdfRasterizeNodeAdapter — przyjmuje PDF (payload Other) i wybiera ścieżkę
//       ingestu: PDF z GOTOWĄ warstwą tekstową → szybka ekstrakcja tekstu
//       (FPDFText) na port `text` (pomija vision = sekundy zamiast minut); skan/
//       obraz → rasteryzacja stron na obrazy (PNG, port `images`) dla
//       vision-parse. Strony lądują w blob store (ctx.blobs). Bez modelu.
// =============================================================================

use std::collections::HashSet;

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};
use crate::services::document::rasterize::{
    extract_pdf_text, rasterize_pdf_streaming, PageRender, PdfTextResult, SinkClosed,
};
use crate::services::document::{DEFAULT_RENDER_DPI, MAX_PDF_PAGES, MIN_TEXT_LAYER_CHARS_PER_PAGE};

const NODE_TYPE: &str = "pdf_rasterize";

const PAGE_PNG_MIME: &str = "image/png";

/// Klucz meta, pod którym `execute` zapisuje wybraną ścieżkę (`text` albo
/// `images`). Czytany przez `active_output_ports` — jedno źródło prawdy, brak
/// ponownej ekstrakcji (lustro `document_router` / `condition`).
const ROUTE_META_KEY: &str = "pdf_route";

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

    /// Decyzja text-vs-vision: PDF ma UŻYTECZNĄ warstwę tekstową gdy średnia
    /// liczba znaków na stronę osiąga próg (`MIN_TEXT_LAYER_CHARS_PER_PAGE`).
    /// Skan/obraz daje ~0 znaków/stronę → `false` → ścieżka rasteryzacja+vision.
    /// PDF z osadzonym tekstem → `true` → ekstrakcja tekstu (sekundy zamiast
    /// minut, bo pomijamy model vision strona-po-stronie).
    fn has_text_layer(text: &PdfTextResult) -> bool {
        if text.page_count == 0 {
            return false;
        }
        text.total_chars / text.page_count >= MIN_TEXT_LAYER_CHARS_PER_PAGE
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
        // Dwa wzajemnie wykluczające się porty (aktywny dokładnie jeden, patrz
        // `active_output_ports`):
        //   `images` — lista stron-obrazów jako Json (blob-refy) dla vision-parse.
        //     Engine ma cardinality 1:1 (envelope.rs hard rule 5), więc zamiast N
        //     envelope emitujemy JEDEN z listą; downstream iteruje po `pages`.
        //   `text` — gotowa warstwa tekstowa PDF (Text) wpięta wprost w combine,
        //     z pominięciem vision (szybka ścieżka dla PDF z osadzonym tekstem).
        vec![
            PortSpec::new("images", FlowDataType::Json),
            PortSpec::new("text", FlowDataType::Text),
        ]
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

        // Szybka ścieżka: najpierw próba ekstrakcji warstwy tekstowej (FPDFText).
        // PDF z osadzonym tekstem (oficjalne publikacje) ingestuje się w sekundy
        // zamiast minut, bo pomijamy rasteryzację + model vision strona-po-stronie.
        // Blokujące FFI pdfium → spawn_blocking. Skan/obraz da ~0 znaków → niżej
        // schodzimy na ścieżkę rasteryzacja+vision (gałąź NIENARUSZONA).
        let text_pdf_bytes = pdf_bytes.clone();
        let text_max_pages = max_pages as usize;
        let text_result: PdfTextResult =
            tokio::task::spawn_blocking(move || extract_pdf_text(&text_pdf_bytes, text_max_pages))
                .await
                .map_err(|e| anyhow!("pdf_rasterize: join ekstrakcji tekstu: {e}"))?
                .map_err(|e| anyhow!("pdf_rasterize: ekstrakcja tekstu: {e}"))?;

        if Self::has_text_layer(&text_result) {
            let mut out = (**envelope).clone();
            out.payload = FlowValue::Text(text_result.markdown);
            out.meta
                .insert(ROUTE_META_KEY.to_string(), serde_json::json!("text"));
            out.meta.insert(
                "pdf_page_count".to_string(),
                serde_json::json!(text_result.page_count),
            );
            out.meta.insert(
                "pdf_text_chars".to_string(),
                serde_json::json!(text_result.total_chars),
            );
            return Ok(out);
        }

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
        out.meta
            .insert("pdf_page_count".to_string(), serde_json::json!(page_count));
        out.meta
            .insert(ROUTE_META_KEY.to_string(), serde_json::json!("images"));
        Ok(out)
    }

    /// Bramkowanie: aktywuje DOKŁADNIE jeden port — ten zapisany w
    /// `meta.pdf_route` przez `execute` (`text` dla PDF z warstwą tekstową,
    /// `images` dla skanu/obrazu). Następnik osiągalny tylko nieaktywnym portem
    /// (cała gałąź vision albo krawędź `text→combine`) staje się Skipped, więc do
    /// `combine` (fan-in) dociera dokładnie jedna żywa ścieżka PDF. Brak wpisu →
    /// `images` (zachowawczo: pełen render+vision, nie cichy pusty ingest).
    fn active_output_ports(
        &self,
        _node: &FlowNode,
        result: &FlowEnvelope,
    ) -> Option<HashSet<String>> {
        let port = result
            .meta
            .get(ROUTE_META_KEY)
            .and_then(|v| v.as_str())
            .unwrap_or("images")
            .to_string();
        Some(HashSet::from([port]))
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
        // `native-libs/` is built locally and never committed, so a fresh
        // checkout has no pdfium. Skip with a reason instead of failing on a
        // missing optional prerequisite.
        if !crate::services::document::rasterize::pdfium_available() {
            eprintln!("pomijam: libpdfium niedostepny (zbuduj scripts/native-libs/build-all.sh)");
            return;
        }
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
        assert_eq!(
            out.meta.get("pdf_page_count").and_then(|v| v.as_u64()),
            Some(2)
        );

        // Blob-refy są realnie odczytywalne i to PNG-i.
        for page in &pages {
            let blob_ref = crate::flow_engine::blob_store::BlobRef {
                id: page["blob_id"].as_str().unwrap().to_string(),
                size_bytes: page["size_bytes"].as_u64().unwrap(),
                mime: "image/png".into(),
                sha256: page["sha256"].as_str().unwrap().to_string(),
            };
            let bytes = ctx.blobs.get(&blob_ref).await.unwrap();
            assert_eq!(
                &bytes[..8],
                &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']
            );
        }
    }

    #[tokio::test]
    async fn max_pages_caps_output() {
        // `native-libs/` is built locally and never committed, so a fresh
        // checkout has no pdfium. Skip with a reason instead of failing on a
        // missing optional prerequisite.
        if !crate::services::document::rasterize::pdfium_available() {
            eprintln!("pomijam: libpdfium niedostepny (zbuduj scripts/native-libs/build-all.sh)");
            return;
        }
        let ctx = stub_ctx();
        let input = pdf_input(&ctx, 5).await;
        let out = PdfRasterizeNodeAdapter::new()
            .execute(
                &node(serde_json::json!({"dpi": 80, "max_pages": 2})),
                &[input],
                &ctx,
            )
            .await
            .unwrap();
        let count = match &out.payload {
            FlowValue::Json(v) => v.get("page_count").and_then(|n| n.as_u64()).unwrap(),
            _ => panic!("expected Json"),
        };
        assert_eq!(count, 2, "max_pages zacieśnia liczbę renderowanych stron");
    }

    /// PDF z BOGATĄ warstwą tekstową idzie szybką ścieżką: payload Text na
    /// porcie `text`, port `text` aktywny (vision Skipped). Dowód, że PDF z
    /// gotowym tekstem pomija render+vision (sekundy zamiast minut).
    async fn text_pdf_input(ctx: &ExecutionContext, pages: usize) -> NodeInput {
        let pdf = crate::services::document::rasterize::text_layer_pdf(pages);
        let blob_ref = ctx.blobs.put(pdf, "application/pdf").await.unwrap();
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Other {
            blob_ref,
            mime: "application/pdf".into(),
            filename: Some("txt.pdf".into()),
        };
        NodeInput {
            from_node_id: "router".into(),
            from_port: "pdf".into(),
            envelope: Arc::new(env),
        }
    }

    #[tokio::test]
    async fn text_layer_pdf_takes_text_fast_path() {
        // `native-libs/` is built locally and never committed, so a fresh
        // checkout has no pdfium. Skip with a reason instead of failing on a
        // missing optional prerequisite.
        if !crate::services::document::rasterize::pdfium_available() {
            eprintln!("pomijam: libpdfium niedostepny (zbuduj scripts/native-libs/build-all.sh)");
            return;
        }
        let ctx = stub_ctx();
        let input = text_pdf_input(&ctx, 2).await;
        let adapter = PdfRasterizeNodeAdapter::new();
        let out = adapter
            .execute(&node(serde_json::json!({})), &[input], &ctx)
            .await
            .unwrap();

        let text = match &out.payload {
            FlowValue::Text(t) => t.clone(),
            other => panic!("expected Text fast-path payload, got {other:?}"),
        };
        assert!(text.contains("Tresc"), "warstwa tekstowa w payloadzie Text");

        let active = adapter
            .active_output_ports(&node(serde_json::json!({})), &out)
            .unwrap();
        assert_eq!(
            active,
            HashSet::from(["text".to_string()]),
            "port `text` aktywny, gałąź vision Skipped"
        );
    }

    #[tokio::test]
    async fn scan_like_pdf_takes_images_path() {
        // `native-libs/` is built locally and never committed, so a fresh
        // checkout has no pdfium. Skip with a reason instead of failing on a
        // missing optional prerequisite.
        if !crate::services::document::rasterize::pdfium_available() {
            eprintln!("pomijam: libpdfium niedostepny (zbuduj scripts/native-libs/build-all.sh)");
            return;
        }
        let ctx = stub_ctx();
        // `minimal_pdf` ma ~8 znaków/stronę < próg → ścieżka rasteryzacja+vision.
        let input = pdf_input(&ctx, 2).await;
        let adapter = PdfRasterizeNodeAdapter::new();
        let out = adapter
            .execute(&node(serde_json::json!({"dpi": 80})), &[input], &ctx)
            .await
            .unwrap();
        assert!(
            matches!(out.payload, FlowValue::Json(_)),
            "skanopodobny PDF emituje listę obrazów (Json)"
        );
        let active = adapter
            .active_output_ports(&node(serde_json::json!({})), &out)
            .unwrap();
        assert_eq!(
            active,
            HashSet::from(["images".to_string()]),
            "port `images` aktywny, gałąź text Skipped"
        );
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
