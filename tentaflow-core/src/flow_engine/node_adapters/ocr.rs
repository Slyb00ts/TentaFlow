// =============================================================================
// Plik: flow_engine/node_adapters/ocr.rs
// Opis: OcrNodeAdapter — rozpoznawanie tekstu z obrazu (region/strona) przez
//       typed surface Documents (`/v1/infer`, task=ocr). Z DocRegion.ocr_spans
//       składa tekst w reading-order (po bbox: góra→dół, lewa→prawa). Input:
//       image(Image) → output: text(Text).
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use tentaflow_protocol::{DocRegion, OcrSpan};

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::node_adapters::page_detect::resolve_image;
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "ocr";
const DEFAULT_MODEL: &str = "rag-ocr";
const TASK: &str = "ocr";
/// Tolerancja (px) grupowania spanów w jedną linię tekstu: spany, których
/// środek y różni się o mniej niż próg, należą do tej samej linii i są
/// sortowane po x (lewa→prawa). Bez tego dwa słowa w tej samej linii o lekko
/// różnym y trafiłyby do osobnych linii.
const LINE_TOLERANCE: f32 = 10.0;

pub struct OcrNodeAdapter;

impl OcrNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    pub(crate) fn pick_model(node: &FlowNode, envelope: &FlowEnvelope) -> String {
        if let Some(m) = node
            .config
            .get("model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return m.to_string();
        }
        if let Some(m) = envelope
            .meta
            .get("model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return m.to_string();
        }
        DEFAULT_MODEL.to_string()
    }

    /// Składa tekst ze wszystkich spanów wszystkich regionów w reading-order.
    /// Spany grupujemy w linie po środku y (z tolerancją), w linii sortujemy po
    /// x; linie sortujemy po y. Pusto → pusty string.
    pub(crate) fn spans_to_text(regions: &[DocRegion]) -> String {
        let mut spans: Vec<&OcrSpan> = regions
            .iter()
            .filter_map(|r| r.ocr_spans.as_ref())
            .flatten()
            .filter(|s| !s.text.trim().is_empty())
            .collect();
        if spans.is_empty() {
            return String::new();
        }
        // Najpierw po y (top→bottom), żeby grupowanie linii szło z góry na dół.
        spans.sort_by(|a, b| Self::cy(a.bbox).total_cmp(&Self::cy(b.bbox)));

        let mut lines: Vec<Vec<&OcrSpan>> = Vec::new();
        for span in spans {
            match lines.last_mut() {
                Some(line)
                    if (Self::cy(span.bbox) - Self::cy(line[0].bbox)).abs() < LINE_TOLERANCE =>
                {
                    line.push(span);
                }
                _ => lines.push(vec![span]),
            }
        }

        let mut out = String::new();
        for (i, mut line) in lines.into_iter().enumerate() {
            line.sort_by(|a, b| a.bbox[0].total_cmp(&b.bbox[0]));
            if i > 0 {
                out.push('\n');
            }
            let joined = line
                .iter()
                .map(|s| s.text.trim())
                .collect::<Vec<_>>()
                .join(" ");
            out.push_str(&joined);
        }
        out
    }

    fn cy(b: [f32; 4]) -> f32 {
        (b[1] + b[3]) / 2.0
    }
}

impl Default for OcrNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for OcrNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Image)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("text", FlowDataType::Text)]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("{NODE_TYPE}: brak krawędzi wejściowej"))?;
        let envelope = &input.envelope;

        let (blob_ref, mime) = resolve_image(envelope)?;
        let image = ctx
            .blobs
            .get(&blob_ref)
            .await
            .map_err(|e| anyhow!("{NODE_TYPE}: pobranie obrazu: {e}"))?;
        if image.is_empty() {
            return Err(anyhow!("{NODE_TYPE}: pusty obraz"));
        }
        let model = Self::pick_model(node, envelope);

        let result = ctx
            .documents
            .infer(&model, &image, &mime, TASK)
            .await
            .map_err(|e| anyhow!("{NODE_TYPE}: OCR zawiódł: {e}"))?;

        let text = Self::spans_to_text(&result.regions);

        let mut out: FlowEnvelope = (**envelope).clone();
        out.payload = FlowValue::Text(text);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use serde_json::json;
    use std::sync::Arc;

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "ocr1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    fn span(x1: f32, y1: f32, x2: f32, y2: f32, text: &str) -> OcrSpan {
        OcrSpan {
            bbox: [x1, y1, x2, y2],
            text: text.into(),
            score: 0.9,
        }
    }

    /// Dwie linie po dwa słowa, podane w przypadkowej kolejności — reading-order
    /// musi je ułożyć góra→dół, lewa→prawa.
    #[test]
    fn spans_assemble_in_reading_order() {
        let region = DocRegion {
            class: "text".into(),
            bbox: [0.0, 0.0, 100.0, 100.0],
            score: 0.9,
            cells: None,
            ocr_spans: Some(vec![
                span(50.0, 0.0, 90.0, 12.0, "świat"),
                span(0.0, 0.0, 40.0, 12.0, "Witaj"),
                span(0.0, 40.0, 40.0, 52.0, "druga"),
                span(50.0, 40.0, 90.0, 52.0, "linia"),
            ]),
        };
        let text = OcrNodeAdapter::spans_to_text(&[region]);
        assert_eq!(text, "Witaj świat\ndruga linia");
    }

    #[test]
    fn empty_spans_yield_empty_text() {
        let region = DocRegion {
            class: "text".into(),
            bbox: [0.0; 4],
            score: 0.5,
            cells: None,
            ocr_spans: None,
        };
        assert_eq!(OcrNodeAdapter::spans_to_text(&[region]), "");
    }

    #[tokio::test]
    async fn stub_documents_yields_empty_text() {
        let ctx = stub_ctx();
        let blob = ctx.blobs.put(vec![1u8; 16], "image/png").await.unwrap();
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Image {
            blob_ref: blob,
            mime: "image/png".into(),
            dims: None,
        };
        let input = NodeInput {
            from_node_id: "x".into(),
            from_port: "images".into(),
            envelope: Arc::new(env),
        };
        let out = OcrNodeAdapter::new()
            .execute(&node(json!({})), &[input], &ctx)
            .await
            .unwrap();
        assert!(matches!(out.payload, FlowValue::Text(t) if t.is_empty()));
    }
}
