// =============================================================================
// Plik: flow_engine/node_adapters/session_context.rs
// Opis: SessionContextNodeAdapter — klasyfikuje stan sesji (first / continue
//       / unclear) i dopisuje odpowiedni system prompt z PromptStore do
//       envelope.context.system_prompts. Heurystyka first-message: pusta
//       envelope.context.messages (plan v4.2 D1, bez cross-node lookup).
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter};
use crate::flow_engine::types::FlowNode;

const NODE_TYPE: &str = "session_context";

pub struct SessionContextNodeAdapter;

impl SessionContextNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    fn pick_prompt_id<'a>(node: &'a FlowNode, key: &str) -> Option<&'a str> {
        node.config
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    }

    /// Heurystyka noise: payload Text < 3 znaki / same cyfry / sama
    /// interpunkcja. Empty payload też traktujemy jako noise (nie ma
    /// nic do sklasyfikowania). Reszta wariantów (Audio/Image/Json)
    /// → false (multimodal nie jest noise'em z natury).
    fn is_noise(envelope: &FlowEnvelope) -> bool {
        match &envelope.payload {
            FlowValue::Text(t) => {
                let trimmed = t.trim();
                if trimmed.len() < 3 {
                    return true;
                }
                if trimmed
                    .chars()
                    .all(|c| c.is_ascii_digit() || c.is_whitespace())
                {
                    return true;
                }
                if trimmed
                    .chars()
                    .all(|c| c.is_ascii_punctuation() || c.is_whitespace())
                {
                    return true;
                }
                false
            }
            FlowValue::Empty => true,
            _ => false,
        }
    }
}

impl Default for SessionContextNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for SessionContextNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }
    fn supported_input_ports(&self) -> &[&'static str] {
        &["in"]
    }
    fn supported_output_ports(&self) -> &[&'static str] {
        &["full"]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("session_context adapter: missing input edge"))?;
        let envelope = &input.envelope;

        let is_first = envelope.context.messages.is_empty();
        let is_noise = Self::is_noise(envelope);

        let prompt_id = if is_noise && !is_first {
            Self::pick_prompt_id(node, "unclear_prompt_id")
        } else if is_first {
            Self::pick_prompt_id(node, "first_prompt_id")
        } else {
            Self::pick_prompt_id(node, "continue_prompt_id")
        };

        let mut out: FlowEnvelope = (**envelope).clone();
        if let Some(pid) = prompt_id {
            if let Some(content) = ctx.prompts.get_prompt(pid, None).await? {
                if !content.is_empty() {
                    out.context.system_prompts.push(content);
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::dispatchers::PromptStore;
    use crate::flow_engine::envelope::ChatMessage;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use async_trait::async_trait;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "sc1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
        }
    }

    fn input(env: FlowEnvelope) -> NodeInput {
        NodeInput {
            from_node_id: "trigger".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }
    }

    struct FakePrompts(HashMap<String, String>);
    #[async_trait]
    impl PromptStore for FakePrompts {
        async fn get_prompt(&self, key: &str, _: Option<&str>) -> Result<Option<String>> {
            Ok(self.0.get(key).cloned())
        }
    }

    #[tokio::test]
    async fn first_message_appends_first_prompt() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("hello".into());
        let mut ctx = stub_ctx();
        ctx.prompts = Arc::new(FakePrompts(HashMap::from([(
            "first".to_string(),
            "WELCOME".to_string(),
        )])));
        let out = SessionContextNodeAdapter::new()
            .execute(
                &node(json!({"first_prompt_id": "first"})),
                &[input(env)],
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out.context.system_prompts, vec!["WELCOME".to_string()]);
    }

    #[tokio::test]
    async fn continue_branch_used_when_history_present() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("kolejne".into());
        env.context.messages = vec![ChatMessage::user("poprzednie")];
        let mut ctx = stub_ctx();
        ctx.prompts = Arc::new(FakePrompts(HashMap::from([(
            "cont".to_string(),
            "RES".to_string(),
        )])));
        let out = SessionContextNodeAdapter::new()
            .execute(
                &node(json!({"continue_prompt_id": "cont"})),
                &[input(env)],
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out.context.system_prompts, vec!["RES".to_string()]);
    }

    #[tokio::test]
    async fn noise_continue_message_uses_unclear_prompt() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("..".into());
        env.context.messages = vec![ChatMessage::user("prev")];
        let mut ctx = stub_ctx();
        ctx.prompts = Arc::new(FakePrompts(HashMap::from([(
            "u".to_string(),
            "ASK_AGAIN".to_string(),
        )])));
        let out = SessionContextNodeAdapter::new()
            .execute(
                &node(json!({"unclear_prompt_id": "u"})),
                &[input(env)],
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out.context.system_prompts, vec!["ASK_AGAIN".to_string()]);
    }

    #[tokio::test]
    async fn missing_prompt_id_is_passthrough() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("x".into());
        let ctx = stub_ctx();
        let out = SessionContextNodeAdapter::new()
            .execute(&node(json!({})), &[input(env)], &ctx)
            .await
            .unwrap();
        assert!(out.context.system_prompts.is_empty());
    }
}
