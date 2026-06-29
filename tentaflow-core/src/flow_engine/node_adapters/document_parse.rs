// =============================================================================
// Plik: flow_engine/node_adapters/document_parse.rs
// Opis: DocumentParseNodeAdapter — JAWNY blok parsowania strony dokumentu na
//       markdown przez powierzchnię document-parse (`ctx.documents.parse` →
//       `execute_documents`). Model/alias jest WIDOCZNY w `node.config['model']`
//       (np. `paddle-ocr-mlx` na Apple, `nemotron-parse` na NVIDIA). Backend
//       (embedded MLX / docker HTTP / mesh-forward) dobiera resolver z
//       failoverem — ale WYBÓR SILNIKA jest na diagramie (per gałąź switcha
//       platformy), nie ukryty. Input: Image → output: Text(markdown).
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "document_parse";

/// Domyślny alias modelu parse; operator pinuje realny silnik w
/// `node.config['model']` (widoczny na bloku).
const DEFAULT_MODEL: &str = "rag-parse";

pub struct DocumentParseNodeAdapter;

impl DocumentParseNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    fn pick_model(node: &FlowNode) -> String {
        node.config
            .get("model")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_MODEL)
            .to_string()
    }
}

impl Default for DocumentParseNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for DocumentParseNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }

    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Image)]
    }

    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("full", FlowDataType::Text)]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("document_parse: brak krawędzi wejściowej"))?;
        let envelope = &input.envelope;

        let (blob_ref, mime) = match &envelope.payload {
            FlowValue::Image { blob_ref, mime, .. } => (blob_ref.clone(), mime.clone()),
            other => {
                return Err(anyhow!(
                    "document_parse: payload musi być Image, dostał {}",
                    other.kind()
                ))
            }
        };

        let bytes = ctx
            .blobs
            .get(&blob_ref)
            .await
            .map_err(|e| anyhow!("document_parse: pobranie obrazu: {e}"))?;

        let model = Self::pick_model(node);
        let markdown = ctx
            .documents
            .parse(&model, &bytes, &mime)
            .await
            .map_err(|e| anyhow!("document_parse: {e}"))?;

        let mut out = (**envelope).clone();
        out.payload = FlowValue::Text(markdown);
        Ok(out)
    }
}
