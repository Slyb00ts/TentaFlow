// =============================================================================
// Plik: flow_engine/node_adapters/vision_llm.rs
// Opis: VisionNodeAdapter — multimodal LLM (text + image → text). Etap 3b.
//       Single-image scope (multi-image batch zostaje na cardinality stage).
//       Image source = inputs[0].envelope.payload (FlowValue::Image). Prompt
//       z node.config['prompt'] albo ostatniej user message w envelope.context.
//       Backend dispatched przez istniejący LlmDispatcher z multimodal Parts.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::blob_store::BlobRef;
use crate::flow_engine::dispatchers::LlmRequest;
use crate::flow_engine::envelope::{
    ChatMessage, ChatMessageContent, ChatRole, FlowEnvelope, FlowValue, MessagePart, NodeInput,
};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "vision_llm";

pub struct VisionNodeAdapter;

impl VisionNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    fn pick_model(node: &FlowNode, envelope: &FlowEnvelope) -> Result<String> {
        if let Some(m) = node
            .config
            .get("model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return Ok(m.to_string());
        }
        if let Some(m) = envelope
            .meta
            .get("model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return Ok(m.to_string());
        }
        Err(anyhow!(
            "vision adapter: no model — node config 'model' nor envelope.meta['model']"
        ))
    }

    fn pick_optional_f32(node: &FlowNode, envelope: &FlowEnvelope, key: &str) -> Option<f32> {
        node.config
            .get(key)
            .and_then(|v| v.as_f64())
            .or_else(|| envelope.meta.get(key).and_then(|v| v.as_f64()))
            .map(|f| f as f32)
    }

    fn pick_optional_u32(node: &FlowNode, envelope: &FlowEnvelope, key: &str) -> Option<u32> {
        node.config
            .get(key)
            .and_then(|v| v.as_u64())
            .or_else(|| envelope.meta.get(key).and_then(|v| v.as_u64()))
            .map(|u| u as u32)
    }

    fn pick_stop(node: &FlowNode) -> Vec<String> {
        node.config
            .get("stop")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn resolve_image_source(envelope: &FlowEnvelope) -> Result<BlobRef> {
        match &envelope.payload {
            FlowValue::Image { blob_ref, .. } => Ok(blob_ref.clone()),
            other => Err(anyhow!(
                "vision adapter: payload must be Image, got {}",
                other.kind()
            )),
        }
    }

    fn resolve_prompt(node: &FlowNode, envelope: &FlowEnvelope) -> Result<String> {
        if let Some(p) = node
            .config
            .get("prompt")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return Ok(p.to_string());
        }
        // Ostatnia user message text content. Skip multimodal Parts (vision
        // request seed wsadzi tekst pytania osobno via text part, ale text-only
        // historia też pasuje).
        if let Some(text) = envelope
            .context
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, ChatRole::User))
            .and_then(|m| match &m.content {
                ChatMessageContent::Text(t) if !t.is_empty() => Some(t.clone()),
                ChatMessageContent::Parts(parts) => parts
                    .iter()
                    .find_map(|p| match p {
                        MessagePart::Text { text } if !text.is_empty() => Some(text.clone()),
                        _ => None,
                    }),
                _ => None,
            })
        {
            return Ok(text);
        }
        Err(anyhow!(
            "vision adapter: no prompt (node.config['prompt'] empty, brak user message text w envelope.context)"
        ))
    }

    fn pick_detail(node: &FlowNode) -> String {
        node.config
            .get("detail")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("auto")
            .to_string()
    }
}

impl Default for VisionNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for VisionNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }
    fn supported_input_ports(&self) -> &[&'static str] {
        &["in"]
    }
    fn supported_output_ports(&self) -> &[&'static str] {
        &["full"]
    }
    fn input_port_type(&self, _port: &str) -> FlowDataType {
        FlowDataType::Image
    }
    fn output_port_type(&self, _port: &str) -> FlowDataType {
        FlowDataType::Text
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("vision adapter: missing input edge"))?;
        let envelope = &input.envelope;

        let blob_ref = Self::resolve_image_source(envelope)?;
        let prompt = Self::resolve_prompt(node, envelope)?;
        let detail = Self::pick_detail(node);
        let model = Self::pick_model(node, envelope)?;

        // Compose messages: system_prompts → System; istniejące context.messages
        // PRZED (history); na końcu user multimodal z prompt + image.
        let mut messages: Vec<ChatMessage> = envelope
            .context
            .system_prompts
            .iter()
            .map(|sp| ChatMessage::system(sp.clone()))
            .collect();
        messages.extend(envelope.context.messages.iter().cloned());
        messages.push(ChatMessage::user_multimodal(vec![
            MessagePart::Text { text: prompt },
            MessagePart::Image { blob_ref, detail },
        ]));

        let req = LlmRequest {
            model,
            messages,
            temperature: Self::pick_optional_f32(node, envelope, "temperature"),
            max_tokens: Self::pick_optional_u32(node, envelope, "max_tokens"),
            top_p: Self::pick_optional_f32(node, envelope, "top_p"),
            frequency_penalty: Self::pick_optional_f32(node, envelope, "frequency_penalty"),
            presence_penalty: Self::pick_optional_f32(node, envelope, "presence_penalty"),
            stop: Self::pick_stop(node),
            deadline: ctx.deadline,
            cancel_token: ctx.cancel_token.clone(),
            user_id: ctx.user_id,
            user_role: ctx.user_role.clone(),
        };

        let response = ctx
            .llm
            .execute_chat(req)
            .await
            .map_err(|e| anyhow!("vision adapter: dispatcher failed: {e}"))?;

        ctx.usage_sink.record(&node.id, response.usage);

        let mut out: FlowEnvelope = (**envelope).clone();
        out.payload = FlowValue::Text(response.content.clone());
        out.context
            .messages
            .push(ChatMessage::assistant(response.content));
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
            id: "v1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
        }
    }

    fn image_envelope() -> FlowEnvelope {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Image {
            blob_ref: BlobRef {
                id: "img1".into(),
                size_bytes: 100,
                mime: "image/jpeg".into(),
                sha256: "deadbeef".into(),
            },
            mime: "image/jpeg".into(),
            dims: None,
        };
        env.meta
            .insert("model".into(), serde_json::Value::String("gpt-4o".into()));
        env
    }

    fn input(envelope: FlowEnvelope) -> NodeInput {
        NodeInput {
            from_node_id: "trigger".into(),
            from_port: "full".into(),
            envelope: Arc::new(envelope),
        }
    }

    #[test]
    fn resolve_image_source_from_payload_image() {
        let env = image_envelope();
        let bf = VisionNodeAdapter::resolve_image_source(&env).unwrap();
        assert_eq!(bf.id, "img1");
    }

    #[test]
    fn resolve_image_source_rejects_text_payload() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("nope".into());
        let err = VisionNodeAdapter::resolve_image_source(&env).unwrap_err();
        assert!(err.to_string().contains("must be Image"));
    }

    #[test]
    fn resolve_prompt_prefers_node_config() {
        let env = image_envelope();
        let n = node(json!({"prompt": "what is this?"}));
        let p = VisionNodeAdapter::resolve_prompt(&n, &env).unwrap();
        assert_eq!(p, "what is this?");
    }

    #[test]
    fn resolve_prompt_falls_back_to_last_user_message() {
        let mut env = image_envelope();
        env.context.messages.push(ChatMessage::user("describe it"));
        let p = VisionNodeAdapter::resolve_prompt(&node(json!({})), &env).unwrap();
        assert_eq!(p, "describe it");
    }

    #[test]
    fn resolve_prompt_errors_when_no_source() {
        let env = image_envelope();
        let err = VisionNodeAdapter::resolve_prompt(&node(json!({})), &env).unwrap_err();
        assert!(err.to_string().contains("no prompt"));
    }
}
