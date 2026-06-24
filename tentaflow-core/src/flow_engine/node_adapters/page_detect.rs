// =============================================================================
// Plik: flow_engine/node_adapters/page_detect.rs
// Opis: PageDetectNodeAdapter — detekcja struktury layoutu strony dokumentu
//       (text/table/figure/title…) przez typed surface Documents (`/v1/infer`,
//       task=page_elements). Reużywa ctx.documents.infer (PARTIA 0). Input:
//       image(Image) → output: regions(Json {regions:[DocRegion...]}).
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::blob_store::BlobRef;
use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "page_detect";
const DEFAULT_MODEL: &str = "rag-page-elements";
const TASK: &str = "page_elements";

pub struct PageDetectNodeAdapter;

impl PageDetectNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Model-picking wzorem `llm`, ale z domyślnym aliasem (alias zawsze
    /// rozwiązywalny przez failover dispatchera, więc brak konfiguracji nie jest
    /// błędem — node ma ustaloną rolę w flow-ingeście RAG).
    pub(crate) fn pick_model(node: &FlowNode, envelope: &FlowEnvelope, default: &str) -> String {
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
        default.to_string()
    }
}

/// Wspólne dla wszystkich nodów Documents-infer: wyciągnij blob obrazu + mime
/// z payloadu Image. Inne payloady to błąd (node oczekuje konkretnie strony
/// dokumentu jako obrazu — PNG/JPEG z `pdf_rasterize` albo wejściowy obraz).
pub(crate) fn resolve_image(envelope: &FlowEnvelope) -> Result<(BlobRef, String)> {
    match &envelope.payload {
        FlowValue::Image { blob_ref, mime, .. } => Ok((blob_ref.clone(), mime.clone())),
        other => Err(anyhow!(
            "{NODE_TYPE}: payload musi być Image, dostał {}",
            other.kind()
        )),
    }
}

/// Serializacja `Vec<DocRegion>` do FlowValue::Json `{regions:[...]}` — wspólny
/// kształt outputu dla page_detect i graphic_elements (downstream iteruje
/// `regions`).
pub(crate) fn regions_payload(
    regions: &[tentaflow_protocol::DocRegion],
) -> Result<serde_json::Value> {
    Ok(serde_json::json!({
        "regions": serde_json::to_value(regions)
            .map_err(|e| anyhow!("serializacja regionów: {e}"))?,
    }))
}

#[async_trait]
impl NodeAdapter for PageDetectNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Image)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        // Kolekcja regionów jako Json (nie Image) — to lista bboxów, nie obraz.
        vec![PortSpec::new("regions", FlowDataType::Json)]
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
            return Err(anyhow!("{NODE_TYPE}: pusty obraz strony"));
        }
        let model = Self::pick_model(node, envelope, DEFAULT_MODEL);

        let result = ctx
            .documents
            .infer(&model, &image, &mime, TASK)
            .await
            .map_err(|e| anyhow!("{NODE_TYPE}: detektor zawiódł: {e}"))?;

        let mut out: FlowEnvelope = (**envelope).clone();
        out.payload = FlowValue::Json(regions_payload(&result.regions)?);
        Ok(out)
    }
}

impl Default for PageDetectNodeAdapter {
    fn default() -> Self {
        Self::new()
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
            id: "pd1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    async fn image_input(ctx: &ExecutionContext) -> NodeInput {
        let blob = ctx.blobs.put(vec![1u8; 32], "image/png").await.unwrap();
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Image {
            blob_ref: blob,
            mime: "image/png".into(),
            dims: None,
        };
        NodeInput {
            from_node_id: "raster".into(),
            from_port: "images".into(),
            envelope: Arc::new(env),
        }
    }

    #[test]
    fn pick_model_defaults_then_overrides() {
        let env = FlowEnvelope::empty();
        assert_eq!(
            PageDetectNodeAdapter::pick_model(&node(json!({})), &env, DEFAULT_MODEL),
            "rag-page-elements"
        );
        assert_eq!(
            PageDetectNodeAdapter::pick_model(&node(json!({"model": "x"})), &env, DEFAULT_MODEL),
            "x"
        );
    }

    /// StubDocuments zwraca pusty `regions` — node musi wyemitować
    /// Json{regions:[]} bez paniki (kształt kontraktu nawet przy 0 detekcji).
    #[tokio::test]
    async fn emits_regions_json_payload() {
        let ctx = stub_ctx();
        let input = image_input(&ctx).await;
        let out = PageDetectNodeAdapter::new()
            .execute(&node(json!({})), &[input], &ctx)
            .await
            .unwrap();
        match out.payload {
            FlowValue::Json(v) => {
                assert!(v.get("regions").and_then(|r| r.as_array()).is_some());
            }
            other => panic!("expected Json, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_non_image_payload() {
        let ctx = stub_ctx();
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("nope".into());
        let input = NodeInput {
            from_node_id: "x".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        };
        let err = PageDetectNodeAdapter::new()
            .execute(&node(json!({})), &[input], &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("musi być Image"));
    }
}
