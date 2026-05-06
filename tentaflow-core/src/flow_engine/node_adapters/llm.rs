// =============================================================================
// Plik: flow_engine/node_adapters/llm.rs
// Opis: LlmNodeAdapter — adapter LLM nowego stacku (plan v4.2). Implementuje
//       NodeAdapter (blocking execute) i LlmAdapter (typed accessor
//       prepare_llm_request używany przez streaming executor). Czyta state
//       wyłącznie z `inputs[0].envelope` zgodnie z hard rule 1 (1-input edge).
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use std::time::Instant;

use crate::flow_engine::dispatchers::LlmRequest;
use crate::flow_engine::envelope::{
    ArtifactProvenance, ChatMessage, ChatRole, FlowEnvelope, FlowValue, NodeInput,
};
use crate::flow_engine::node_adapter::{ExecutionContext, LlmAdapter, NodeAdapter};
use crate::flow_engine::types::FlowNode;

const NODE_TYPE: &str = "llm";

pub struct LlmNodeAdapter;

impl LlmNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    fn pick_model(node: &FlowNode, envelope: &FlowEnvelope) -> Result<String> {
        // 1. Override z node config — najwyższy priorytet (operator pin'uje
        //    konkretny backend dla tej ścieżki flow).
        if let Some(m) = node
            .config
            .get("model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return Ok(m.to_string());
        }
        // 2. Model z envelope.meta — trigger seed'uje go z requestu.
        if let Some(m) = envelope
            .meta
            .get("model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return Ok(m.to_string());
        }
        Err(anyhow!(
            "llm adapter: no model — node config 'model' nor envelope.meta['model']"
        ))
    }

    fn pick_optional_f32(node: &FlowNode, key: &str) -> Option<f32> {
        node.config.get(key).and_then(|v| v.as_f64()).map(|f| f as f32)
    }

    fn pick_optional_u32(node: &FlowNode, key: &str) -> Option<u32> {
        node.config.get(key).and_then(|v| v.as_u64()).map(|u| u as u32)
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

    fn inline_system_prompt(node: &FlowNode) -> Option<String> {
        node.config
            .get("system_prompt")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    }

    /// Zbieranie messages z envelope.context. Plan v4.2:
    /// 1. system_prompts → osobne System messages (nie sklejać).
    /// 2. inline `system_prompt` z node config → osobny System message
    ///    dopisany ZA system_prompts (envelope-driven idą pierwsze).
    /// 3. context.messages w kolejności.
    /// 4. Jeśli payload jest Text i ostatnia message ma inny content,
    ///    doklejamy User(payload.text). Empty payload nie produkuje żadnego
    ///    dodatkowego user'a.
    fn build_messages(node: &FlowNode, envelope: &FlowEnvelope) -> Vec<ChatMessage> {
        let mut out: Vec<ChatMessage> = Vec::new();

        for sp in &envelope.context.system_prompts {
            out.push(ChatMessage::system(sp.clone()));
        }
        if let Some(inline) = Self::inline_system_prompt(node) {
            out.push(ChatMessage::system(inline));
        }
        out.extend(envelope.context.messages.iter().cloned());

        if let FlowValue::Text(t) = &envelope.payload {
            if !t.is_empty() {
                let last_matches = out
                    .last()
                    .map(|m| m.role == ChatRole::User && m.content == *t)
                    .unwrap_or(false);
                if !last_matches {
                    out.push(ChatMessage::user(t.clone()));
                }
            }
        }
        out
    }

    fn build_llm_request(
        node: &FlowNode,
        envelope: &FlowEnvelope,
        ctx: &ExecutionContext,
    ) -> Result<LlmRequest> {
        let model = Self::pick_model(node, envelope)?;
        let messages = Self::build_messages(node, envelope);
        Ok(LlmRequest {
            model,
            messages,
            temperature: Self::pick_optional_f32(node, "temperature"),
            max_tokens: Self::pick_optional_u32(node, "max_tokens"),
            stop: Self::pick_stop(node),
            deadline: ctx.deadline,
            cancel_token: ctx.cancel_token.clone(),
        })
    }
}

impl Default for LlmNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for LlmNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }
    fn supported_input_ports(&self) -> &[&'static str] {
        &["in"]
    }
    fn supported_output_ports(&self) -> &[&'static str] {
        // "stream" jest egzekwowany przez executor (nie adapter); blocking
        // path produkuje "full". Validation sprawdza streaming end-shape.
        &["stream", "full"]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("llm adapter: missing input edge"))?;
        let envelope = &input.envelope;

        let request = Self::build_llm_request(node, envelope, ctx)?;
        let response = ctx
            .llm
            .execute_chat(request)
            .await
            .map_err(|e| anyhow!("llm adapter: dispatcher failed: {e}"))?;

        ctx.usage_sink.record(&node.id, response.usage);

        // Output envelope: klon input + nadpisany payload + dopisana
        // assistant message + provenance dla payload artifact.
        let mut out: FlowEnvelope = (**envelope).clone();
        out.payload = FlowValue::Text(response.content.clone());
        out.context
            .messages
            .push(ChatMessage::assistant(response.content));
        out.provenance.insert(
            "payload".into(),
            ArtifactProvenance {
                producer_node_id: node.id.clone(),
                producer_node_type: NODE_TYPE.to_string(),
                timestamp_ms: now_ms(ctx.clock.as_ref()),
            },
        );
        Ok(out)
    }
}

impl LlmAdapter for LlmNodeAdapter {
    fn prepare_llm_request(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> LlmRequest {
        // prepare_llm_request jest sync — używany przez streaming branch
        // executora po wykonaniu wszystkich pre-LLM nodów. Brakujący input
        // albo brak modelu zwracają minimalny fallback z pustym modelem;
        // executor i tak złapie błąd w stream_chat (LlmDispatcher zwróci
        // 'no candidates' / 'model not found').
        let envelope_owned: FlowEnvelope;
        let envelope: &FlowEnvelope = match inputs.first() {
            Some(i) => &i.envelope,
            None => {
                envelope_owned = FlowEnvelope::empty();
                &envelope_owned
            }
        };
        Self::build_llm_request(node, envelope, ctx).unwrap_or_else(|_| LlmRequest {
            model: String::new(),
            messages: Self::build_messages(node, envelope),
            temperature: Self::pick_optional_f32(node, "temperature"),
            max_tokens: Self::pick_optional_u32(node, "max_tokens"),
            stop: Self::pick_stop(node),
            deadline: ctx.deadline,
            cancel_token: ctx.cancel_token.clone(),
        })
    }
}

fn now_ms(clock: &dyn crate::flow_engine::dispatchers::Clock) -> u64 {
    let _ = clock; // Clock trait używamy później (jeśli dodamy now_ms metodę);
    let _ = Instant::now();
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::envelope::ConversationContext;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use serde_json::json;
    use std::sync::Arc;

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "llm1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
        }
    }

    fn input(envelope: FlowEnvelope) -> NodeInput {
        NodeInput {
            from_node_id: "trigger".into(),
            from_port: "full".into(),
            envelope: Arc::new(envelope),
        }
    }

    #[test]
    fn build_messages_stitches_system_prompts_then_messages() {
        let mut env = FlowEnvelope::empty();
        env.context = ConversationContext {
            messages: vec![ChatMessage::user("ping")],
            system_prompts: vec!["sp1".into(), "sp2".into()],
        };
        let n = node(json!({"system_prompt": "inline"}));
        let msgs = LlmNodeAdapter::build_messages(&n, &env);
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0].role, ChatRole::System);
        assert_eq!(msgs[0].content, "sp1");
        assert_eq!(msgs[1].role, ChatRole::System);
        assert_eq!(msgs[1].content, "sp2");
        assert_eq!(msgs[2].role, ChatRole::System);
        assert_eq!(msgs[2].content, "inline");
        assert_eq!(msgs[3].role, ChatRole::User);
    }

    #[test]
    fn payload_text_appended_when_last_message_differs() {
        let mut env = FlowEnvelope::empty();
        env.context.messages = vec![ChatMessage::user("old")];
        env.payload = FlowValue::Text("new question".into());
        let msgs = LlmNodeAdapter::build_messages(&node(json!({})), &env);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[1].content, "new question");
    }

    #[test]
    fn payload_text_skipped_when_last_user_message_matches() {
        let mut env = FlowEnvelope::empty();
        env.context.messages = vec![ChatMessage::user("same")];
        env.payload = FlowValue::Text("same".into());
        let msgs = LlmNodeAdapter::build_messages(&node(json!({})), &env);
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn pick_model_prefers_node_config_then_meta() {
        let mut env = FlowEnvelope::empty();
        env.meta
            .insert("model".into(), json!("envelope-model"));
        let n = node(json!({"model": "node-model"}));
        assert_eq!(
            LlmNodeAdapter::pick_model(&n, &env).unwrap(),
            "node-model"
        );
        let n = node(json!({}));
        assert_eq!(
            LlmNodeAdapter::pick_model(&n, &env).unwrap(),
            "envelope-model"
        );
    }

    #[test]
    fn pick_model_errors_when_neither_source_has_value() {
        let env = FlowEnvelope::empty();
        let n = node(json!({}));
        assert!(LlmNodeAdapter::pick_model(&n, &env).is_err());
    }

    #[test]
    fn prepare_llm_request_passes_temp_and_stop() {
        let mut env = FlowEnvelope::empty();
        env.meta.insert("model".into(), json!("m"));
        let n = node(json!({"temperature": 0.7, "max_tokens": 128, "stop": ["\n"]}));
        let inputs = vec![input(env)];
        let ctx = stub_ctx();
        let adapter = LlmNodeAdapter::new();
        let req = adapter.prepare_llm_request(&n, &inputs, &ctx);
        assert_eq!(req.model, "m");
        assert_eq!(req.temperature, Some(0.7));
        assert_eq!(req.max_tokens, Some(128));
        assert_eq!(req.stop, vec!["\n".to_string()]);
    }
}
