// =============================================================================
// Plik: flow_engine/node_adapters/text_extract.rs
// Opis: TextExtractNodeAdapter — dekoduje plik tekstowy (text/plain, markdown,
//       application/json) z payloadu Other na czysty tekst (FlowValue::Text).
//       Stoi na gałęziach `router.text` ORAZ `router.unknown` flow-ingestu: dla
//       rozpoznanego tekstu zwraca jego treść (UTF-8 lossy), a dla
//       nieobsługiwanego typu binarnego — TWARDY błąd. Dzięki temu nieznany typ
//       kończy się jawnym błędem ingestu zamiast cichego placeholdera
//       "<file: mime>" wpisanego do indeksu (combine.flow_value_to_text(Other)).
//       Bez modelu — czysta dekoda.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};
use crate::services::document::extract::{classify_source, SourceKind};

const NODE_TYPE: &str = "text_extract";

pub struct TextExtractNodeAdapter;

impl TextExtractNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TextExtractNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for TextExtractNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }

    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Other)]
    }

    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("full", FlowDataType::Text)]
    }

    async fn execute(
        &self,
        _node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("{NODE_TYPE}: brak krawędzi wejściowej"))?;
        let envelope = &input.envelope;

        let (blob_ref, mime) = match &envelope.payload {
            FlowValue::Other { blob_ref, mime, .. } => (blob_ref.clone(), mime.clone()),
            other => {
                return Err(anyhow!(
                    "{NODE_TYPE}: payload musi być Other(plik), dostał {}",
                    other.kind()
                ))
            }
        };

        let bytes = ctx
            .blobs
            .get(&blob_ref)
            .await
            .map_err(|e| anyhow!("{NODE_TYPE}: pobranie pliku: {e}"))?;
        if bytes.is_empty() {
            return Err(anyhow!("{NODE_TYPE}: pusty plik wejściowy"));
        }

        // Re-klasyfikacja TĄ SAMĄ funkcją co document_router (mime + magic-bytes),
        // by gałąź `router.unknown` (nieznany typ binarny) skończyła się TWARDYM
        // błędem ingestu, a nie cichym placeholderem. Tylko rozpoznany tekst
        // (text/plain, markdown, application/json) dekodujemy na treść.
        match classify_source(&mime, &bytes) {
            SourceKind::Text => {}
            other => {
                return Err(anyhow!(
                    "{NODE_TYPE}: nieobsługiwany typ dokumentu (mime '{mime}', \
                     klasyfikacja {other:?}) — ingest nieobsługiwanego pliku odrzucony"
                ))
            }
        }

        // text/* jest z definicji tekstem — dekodujemy lossy (zamieniamy nie-UTF-8
        // sekwencje na U+FFFD), zamiast wywalać ingest pliku z pojedynczym złym
        // bajtem. Dla NIEznanego binarnego nigdy tu nie dochodzimy (błąd wyżej).
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if text.trim().is_empty() {
            return Err(anyhow!("{NODE_TYPE}: plik tekstowy nie zawiera treści"));
        }

        let mut out = (**envelope).clone();
        out.payload = FlowValue::Text(text);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use std::sync::Arc;

    fn node() -> FlowNode {
        FlowNode {
            id: "te-1".into(),
            node_type: NODE_TYPE.into(),
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
            from_port: "text".into(),
            envelope: Arc::new(env),
        }
    }

    #[tokio::test]
    async fn decodes_plain_text() {
        let ctx = stub_ctx();
        let input = other_input(&ctx, "text/plain", b"Ala ma kota".to_vec()).await;
        let out = TextExtractNodeAdapter::new()
            .execute(&node(), &[input], &ctx)
            .await
            .unwrap();
        assert_eq!(out.payload.as_text(), Some("Ala ma kota"));
    }

    #[tokio::test]
    async fn decodes_json_as_text() {
        let ctx = stub_ctx();
        let input = other_input(&ctx, "application/json", br#"{"a":1}"#.to_vec()).await;
        let out = TextExtractNodeAdapter::new()
            .execute(&node(), &[input], &ctx)
            .await
            .unwrap();
        assert_eq!(out.payload.as_text(), Some(r#"{"a":1}"#));
    }

    #[tokio::test]
    async fn lossy_utf8_does_not_fail() {
        let ctx = stub_ctx();
        // Tekstowy mime z pojedynczym złym bajtem — dekoda lossy, nie błąd.
        let input = other_input(&ctx, "text/plain", vec![b'h', b'i', 0xff]).await;
        let out = TextExtractNodeAdapter::new()
            .execute(&node(), &[input], &ctx)
            .await
            .unwrap();
        let text = out.payload.as_text().unwrap();
        assert!(text.starts_with("hi"), "{text}");
    }

    #[tokio::test]
    async fn unknown_binary_is_hard_error() {
        let ctx = stub_ctx();
        // Gałąź router.unknown: nieznany typ binarny → TWARDY błąd, nie placeholder.
        let input = other_input(&ctx, "application/x-tar", b"\x00\x01\x02random".to_vec()).await;
        let err = TextExtractNodeAdapter::new()
            .execute(&node(), &[input], &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("nieobsługiwany typ"), "{err}");
    }

    #[tokio::test]
    async fn empty_file_is_error() {
        let ctx = stub_ctx();
        let input = other_input(&ctx, "text/plain", Vec::new()).await;
        let err = TextExtractNodeAdapter::new()
            .execute(&node(), &[input], &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("pusty plik"), "{err}");
    }

    #[test]
    fn advertises_other_in_text_out() {
        let a = TextExtractNodeAdapter::new();
        assert_eq!(a.node_type(), "text_extract");
        assert_eq!(a.input_port_type("in"), FlowDataType::Other);
        assert_eq!(a.output_port_type("full"), FlowDataType::Text);
    }
}
