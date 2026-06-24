// =============================================================================
// Plik: flow_engine/node_adapters/document_router.rs
// Opis: DocumentRouterNodeAdapter — klasyfikuje plik wejściowy (mime +
//       magic-bytes) i routuje envelope na DOKŁADNIE jeden port wyjściowy
//       (pdf/xlsx/docx/pptx/image/text/unknown). Wzór routingu jak condition.rs
//       (override active_output_ports). Bez modelu — czysta klasyfikacja.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use std::collections::HashSet;

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};
use crate::services::document::extract::{classify_source, SourceKind};

const NODE_TYPE: &str = "document_router";

/// Klucz meta, pod którym router zapisuje wybrany port — czytany przez
/// `active_output_ports` (jedno źródło prawdy, brak ponownej klasyfikacji).
const ROUTE_META_KEY: &str = "document_route";

pub struct DocumentRouterNodeAdapter;

impl DocumentRouterNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Wyciąga (mime, bajty) z payloadu. Router wymaga blob-payloadu (`Other`/
    /// `Image`); inne warianty są błędem konfiguracji flow (router stoi na
    /// początku ingestu, przed jakąkolwiek transformacją). Bajty są potrzebne do
    /// magic-bytes fallbacku, gdy mime jest generyczny.
    async fn fetch_payload(
        envelope: &FlowEnvelope,
        ctx: &ExecutionContext,
    ) -> Result<(String, Vec<u8>)> {
        match &envelope.payload {
            FlowValue::Other { blob_ref, mime, .. } => {
                let bytes = ctx
                    .blobs
                    .get(blob_ref)
                    .await
                    .map_err(|e| anyhow!("document_router: pobranie bloba: {e}"))?;
                Ok((mime.clone(), bytes))
            }
            FlowValue::Image { blob_ref, mime, .. } => {
                let bytes = ctx
                    .blobs
                    .get(blob_ref)
                    .await
                    .map_err(|e| anyhow!("document_router: pobranie obrazu: {e}"))?;
                Ok((mime.clone(), bytes))
            }
            other => Err(anyhow!(
                "document_router: payload musi być Other(plik) albo Image, dostał {}",
                other.kind()
            )),
        }
    }
}

impl Default for DocumentRouterNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for DocumentRouterNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }

    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Any)]
    }

    fn output_ports(&self) -> Vec<PortSpec> {
        vec![
            PortSpec::new("pdf", FlowDataType::Other),
            PortSpec::new("xlsx", FlowDataType::Other),
            PortSpec::new("docx", FlowDataType::Other),
            PortSpec::new("pptx", FlowDataType::Other),
            PortSpec::new("image", FlowDataType::Image),
            PortSpec::new("text", FlowDataType::Other),
            PortSpec::new("unknown", FlowDataType::Any),
        ]
    }

    async fn execute(
        &self,
        _node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("document_router: brak krawędzi wejściowej"))?;
        let envelope = &input.envelope;

        let (mime, bytes) = Self::fetch_payload(envelope, ctx).await?;
        let kind = classify_source(&mime, &bytes);
        let port = kind.router_port();

        // Passthrough envelope — router NIE zmienia payloadu, tylko wybiera port.
        // Wybór trafia w meta, skąd `active_output_ports` go odczyta (lustro
        // condition.rs::condition_result). Dopisujemy też rozpoznany mime, by
        // węzły downstream nie musiały zgadywać.
        let mut out = (**envelope).clone();
        out.meta.insert(
            ROUTE_META_KEY.to_string(),
            serde_json::json!({ "port": port, "mime": mime, "kind": format!("{kind:?}") }),
        );
        Ok(out)
    }

    /// Bramkowanie: aktywuje DOKŁADNIE jeden port — ten zapisany w
    /// `meta.document_route.port` przez `execute`. Brak wpisu (envelope
    /// przepuszczony inaczej) → `unknown`, by gałęzie typowane nie odpaliły się
    /// błędnie.
    fn active_output_ports(
        &self,
        _node: &FlowNode,
        result: &FlowEnvelope,
    ) -> Option<HashSet<String>> {
        let port = result
            .meta
            .get(ROUTE_META_KEY)
            .and_then(|v| v.get("port"))
            .and_then(|v| v.as_str())
            .unwrap_or(SourceKind::Unknown.router_port())
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

    fn node() -> FlowNode {
        FlowNode {
            id: "router-1".into(),
            node_type: NODE_TYPE.into(),
            config: serde_json::json!({}),
            position: None,
            label: None,
            region: None,
        }
    }

    /// Wkłada bajty do blob store ctx i buduje envelope z payloadem `Other`.
    async fn other_input(ctx: &ExecutionContext, mime: &str, bytes: Vec<u8>) -> NodeInput {
        let blob_ref = ctx.blobs.put(bytes, mime).await.unwrap();
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Other {
            blob_ref,
            mime: mime.to_string(),
            filename: None,
        };
        NodeInput {
            from_node_id: "trigger".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }
    }

    async fn route_port(ctx: &ExecutionContext, mime: &str, bytes: Vec<u8>) -> String {
        let input = other_input(ctx, mime, bytes).await;
        let out = DocumentRouterNodeAdapter::new()
            .execute(&node(), &[input], ctx)
            .await
            .unwrap();
        let active = DocumentRouterNodeAdapter::new()
            .active_output_ports(&node(), &out)
            .unwrap();
        assert_eq!(active.len(), 1, "router aktywuje dokładnie jeden port");
        active.into_iter().next().unwrap()
    }

    #[tokio::test]
    async fn routes_pdf_by_mime() {
        let ctx = stub_ctx();
        assert_eq!(
            route_port(&ctx, "application/pdf", b"%PDF-1.4\n".to_vec()).await,
            "pdf"
        );
    }

    #[tokio::test]
    async fn routes_pdf_by_magic_when_mime_generic() {
        let ctx = stub_ctx();
        assert_eq!(
            route_port(&ctx, "application/octet-stream", b"%PDF-1.7 rest".to_vec()).await,
            "pdf"
        );
    }

    #[tokio::test]
    async fn routes_text_and_unknown() {
        let ctx = stub_ctx();
        assert_eq!(route_port(&ctx, "text/plain", b"hello".to_vec()).await, "text");
        assert_eq!(
            route_port(&ctx, "application/x-tar", b"random".to_vec()).await,
            "unknown"
        );
    }

    #[tokio::test]
    async fn passthrough_preserves_payload() {
        let ctx = stub_ctx();
        let input = other_input(&ctx, "text/plain", b"abc".to_vec()).await;
        let original = (*input.envelope).payload.clone();
        let out = DocumentRouterNodeAdapter::new()
            .execute(&node(), &[input], &ctx)
            .await
            .unwrap();
        assert_eq!(out.payload, original, "router nie modyfikuje payloadu");
    }

    #[test]
    fn advertises_seven_output_ports() {
        let names: Vec<String> = DocumentRouterNodeAdapter::new()
            .output_ports()
            .iter()
            .map(|p| p.name.clone())
            .collect();
        assert_eq!(
            names,
            vec!["pdf", "xlsx", "docx", "pptx", "image", "text", "unknown"]
        );
    }
}
