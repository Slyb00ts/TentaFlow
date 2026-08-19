// =============================================================================
// Plik: flow_engine/node_adapters/vision_parse.rs
// Opis: VisionParseNodeAdapter — parsowanie obrazu strony dokumentu na markdown
//       przez model VISION-CHAT (VLM na /v1/chat/completions, np. nemotron-parse).
//       Reużywa istniejącą ścieżkę vision-chat (ctx.llm.execute_chat z image part)
//       — zero duplikacji HTTP. Input: image(Image) → output: markdown(Text).
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::blob_store::BlobRef;
use crate::flow_engine::dispatchers::LlmRequest;
use crate::flow_engine::envelope::{ChatMessage, FlowEnvelope, FlowValue, MessagePart, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "vision_parse";
/// Domyślny alias modelu vision-parse (failover aliasów rozwiązuje go do
/// realnego serwisu VLM). Operator pinuje go w `node.config['model']`.
const DEFAULT_MODEL: &str = "rag-parse";
/// Domyślny budżet tokenów — strona dokumentu jako markdown bywa długa.
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// Instrukcje parsowania zależne od trybu `tools`. `markdown_bbox` i `markdown`
/// proszą o czysty markdown (struktura tabel GFM, kolejność czytania); `text`
/// o sam tekst bez znaczników. VLM i tak zwraca tekst — różnica to ile struktury
/// chcemy zachować. Bboxy są rolą detektorów (`page_detect`), nie tego nodu, więc
/// `markdown_bbox` różni się od `markdown` tylko naciskiem na zachowanie layoutu.
const INSTRUCTION_MARKDOWN: &str =
    "Wyodrębnij całą treść tej strony dokumentu jako czysty Markdown. \
     Zachowaj strukturę tabel (GFM), nagłówki, listy i kolejność czytania. \
     Zwróć WYŁĄCZNIE treść dokumentu, bez komentarza.";
const INSTRUCTION_TEXT: &str =
    "Wyodrębnij całą treść tekstową tej strony dokumentu w kolejności czytania. \
     Zwróć czysty tekst bez znaczników Markdown ani komentarza.";

pub struct VisionParseNodeAdapter;

impl VisionParseNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Model-picking wzorem `llm`: `node.config['model']` > `envelope.meta['model']`
    /// > domyślny alias `rag-parse`. Alias zawsze istnieje (failover po stronie
    /// dispatchera), więc — w odróżnieniu od `llm` — brak konfiguracji NIE jest
    /// błędem; ten node jest częścią flow-ingestu RAG z ustalonym aliasem.
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

    pub(crate) fn pick_max_tokens(node: &FlowNode, envelope: &FlowEnvelope) -> u32 {
        node.config
            .get("max_tokens")
            .and_then(|v| v.as_u64())
            .or_else(|| envelope.meta.get("max_tokens").and_then(|v| v.as_u64()))
            .map(|u| u as u32)
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_MAX_TOKENS)
    }

    /// Tryb wyodrębniania: markdown_bbox (domyślny) / markdown → markdown,
    /// text → czysty tekst.
    pub(crate) fn instruction(node: &FlowNode) -> &'static str {
        match node
            .config
            .get("tools")
            .and_then(|v| v.as_str())
            .unwrap_or("markdown_bbox")
        {
            "text" => INSTRUCTION_TEXT,
            _ => INSTRUCTION_MARKDOWN,
        }
    }

    fn resolve_image_source(envelope: &FlowEnvelope) -> Result<BlobRef> {
        match &envelope.payload {
            FlowValue::Image { blob_ref, .. } => Ok(blob_ref.clone()),
            other => Err(anyhow!(
                "vision_parse: payload musi być Image, dostał {}",
                other.kind()
            )),
        }
    }
}

/// Parsuje JEDEN obraz (blob) na markdown przez vision-chat. Wydzielone z
/// `execute`, żeby batch-owy `vision_parse_pages` reużywał DOKŁADNIE tę samą
/// ścieżkę (instrukcja + multimodal message + ctx.llm.execute_chat), bez
/// duplikacji budowania requestu ani kodowania obrazu. `envelope` służy tylko do
/// odczytu meta (flow_id/correlation_id) — payload obrazu przekazujemy osobno
/// w `blob_ref`, bo batch iteruje po wielu stronach jednego envelope.
pub(crate) async fn parse_image_to_markdown(
    ctx: &ExecutionContext,
    node: &FlowNode,
    envelope: &FlowEnvelope,
    blob_ref: BlobRef,
    model: String,
    max_tokens: u32,
    instruction: &'static str,
) -> Result<(String, crate::flow_engine::envelope::TokenUsage)> {
    let messages = vec![ChatMessage::user_multimodal(vec![
        MessagePart::Text {
            text: instruction.to_string(),
        },
        MessagePart::Image {
            blob_ref,
            detail: "high".to_string(),
        },
    ])];

    let req = LlmRequest {
        // §2.5 — the run's stamp, from `ctx`, never from `envelope.meta`.
        provenance: ctx.provenance(),
        audio_out: None,
        reasoning_effort: None,
        model,
        messages,
        temperature: Some(0.0),
        max_tokens: Some(max_tokens),
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: Vec::new(),
        tools: Vec::new(),
        tool_choice: None,
        deadline: ctx.deadline,
        cancel_token: ctx.cancel_token.clone(),
        user_id: ctx.user_id.clone(),
        user_role: ctx.user_role.clone(),
        flow_id: envelope
            .meta
            .get("flow_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string()),
        flow_node_id: Some(node.id.clone()),
        agent_id: None,
        agent_run_id: None,
        correlation_id: envelope
            .meta
            .get("correlation_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string()),
    };

    let response = ctx
        .llm
        .execute_chat(req)
        .await
        .map_err(|e| anyhow!("vision_parse: dispatcher failed: {e}"))?;
    Ok((response.content, response.usage))
}

impl Default for VisionParseNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for VisionParseNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Image)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("markdown", FlowDataType::Text)]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("vision_parse: brak krawędzi wejściowej"))?;
        let envelope = &input.envelope;

        let blob_ref = Self::resolve_image_source(envelope)?;
        let model = Self::pick_model(node, envelope);
        let max_tokens = Self::pick_max_tokens(node, envelope);
        let instruction = Self::instruction(node);

        // Vision-chat: jeden obraz → markdown. Ścieżka (instrukcja + multimodal
        // message + ctx.llm.execute_chat) jest wspólna z batch-owym
        // `vision_parse_pages` przez `parse_image_to_markdown`. temperature=0
        // (deterministyczny parse) jest wewnątrz helpera.
        let (content, usage) = parse_image_to_markdown(
            ctx,
            node,
            envelope,
            blob_ref,
            model,
            max_tokens,
            instruction,
        )
        .await?;

        ctx.usage_sink.record(&node.id, usage);

        // Markdown = treść odpowiedzi. Payload staje się Text (kolejny node —
        // chunk / document_merge — czyta go bez znajomości obrazu).
        let mut out: FlowEnvelope = (**envelope).clone();
        out.payload = FlowValue::Text(content);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use serde_json::json;

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "vp1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    fn image_envelope() -> FlowEnvelope {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Image {
            blob_ref: BlobRef {
                id: "page1".into(),
                size_bytes: 100,
                mime: "image/png".into(),
                sha256: "deadbeef".into(),
            },
            mime: "image/png".into(),
            dims: None,
        };
        env
    }

    #[test]
    fn pick_model_defaults_to_rag_parse() {
        let env = image_envelope();
        assert_eq!(
            VisionParseNodeAdapter::pick_model(&node(json!({})), &env),
            "rag-parse"
        );
        assert_eq!(
            VisionParseNodeAdapter::pick_model(&node(json!({"model": "nemo"})), &env),
            "nemo"
        );
    }

    #[test]
    fn instruction_switches_on_tools_mode() {
        assert_eq!(
            VisionParseNodeAdapter::instruction(&node(json!({"tools": "text"}))),
            INSTRUCTION_TEXT
        );
        assert_eq!(
            VisionParseNodeAdapter::instruction(&node(json!({"tools": "markdown_bbox"}))),
            INSTRUCTION_MARKDOWN
        );
        assert_eq!(
            VisionParseNodeAdapter::instruction(&node(json!({}))),
            INSTRUCTION_MARKDOWN
        );
    }

    #[test]
    fn rejects_non_image_payload() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("nope".into());
        let err = VisionParseNodeAdapter::resolve_image_source(&env).unwrap_err();
        assert!(err.to_string().contains("musi być Image"));
    }

    #[tokio::test]
    async fn errors_when_no_input_edge() {
        let ctx = stub_ctx();
        let err = VisionParseNodeAdapter::new()
            .execute(&node(json!({})), &[], &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("brak krawędzi"));
    }
}
