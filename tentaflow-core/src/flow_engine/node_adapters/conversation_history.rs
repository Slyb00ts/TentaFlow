// =============================================================================
// Plik: flow_engine/node_adapters/conversation_history.rs
// Opis: ConversationHistoryNodeAdapter — pobiera ostatnie N wiadomości z
//       ConversationHistoryStore i wstrzykuje je do envelope.context.messages
//       (przed dotychczasowymi). Aktualną user message (z payload Text)
//       dopisuje do storu po wstrzyknięciu — kolejne calls w tej samej sesji
//       widzą ją jako historię.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{ChatMessage, FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter};
use crate::flow_engine::types::FlowNode;

const NODE_TYPE: &str = "conversation_history";
const DEFAULT_MAX_MESSAGES: u32 = 20;

pub struct ConversationHistoryNodeAdapter;

impl ConversationHistoryNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    fn pick_session(node: &FlowNode, ctx: &ExecutionContext) -> Result<String> {
        if let Some(s) = node
            .config
            .get("session_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return Ok(s.to_string());
        }
        ctx.session_id
            .clone()
            .ok_or_else(|| anyhow!("conversation_history adapter: no session_id (node config nor ctx.session_id)"))
    }
}

impl Default for ConversationHistoryNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for ConversationHistoryNodeAdapter {
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
            .ok_or_else(|| anyhow!("conversation_history adapter: missing input edge"))?;
        let envelope = &input.envelope;

        let session = Self::pick_session(node, ctx)?;
        let max = node
            .config
            .get("max_messages")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32)
            .unwrap_or(DEFAULT_MAX_MESSAGES);

        let history = ctx.history.recent(&session, max).await?;

        let mut out: FlowEnvelope = (**envelope).clone();
        // Plan v4.2 D1: cross-node lookup zabity, więc wstrzykujemy historię
        // PRZED istniejącymi messages — zachowujemy chronologię (najstarsza
        // pierwsza). Inline system prompts z envelope.context.system_prompts
        // dochodzą później w llm adapter.
        let mut new_msgs: Vec<ChatMessage> = history;
        new_msgs.extend(out.context.messages.drain(..));
        out.context.messages = new_msgs;

        // Po wstrzyknięciu — zapis bieżącego user input do historii. Tylko
        // gdy payload to niepusty Text. Tag user'a w meta['user_role']
        // (jeśli jest) idzie do `name`, żeby downstream mógł rozróżnić
        // wielomówców w jednej sesji.
        if let FlowValue::Text(t) = &envelope.payload {
            if !t.is_empty() {
                let mut msg = ChatMessage::user(t.clone());
                msg.name = ctx.user_role.clone();
                ctx.history.append(&session, msg).await?;
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::dispatchers::ConversationHistoryStore;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "h1".into(),
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

    struct FakeHistory {
        messages: Vec<ChatMessage>,
        appended: Mutex<Vec<(String, ChatMessage)>>,
    }

    #[async_trait]
    impl ConversationHistoryStore for FakeHistory {
        async fn recent(&self, _: &str, _: u32) -> Result<Vec<ChatMessage>> {
            Ok(self.messages.clone())
        }
        async fn append(&self, session: &str, m: ChatMessage) -> Result<()> {
            self.appended
                .lock()
                .unwrap()
                .push((session.to_string(), m));
            Ok(())
        }
    }

    #[tokio::test]
    async fn injects_recent_history_before_existing_messages() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("now".into());
        env.context.messages = vec![ChatMessage::user("now")];
        let mut ctx = stub_ctx();
        ctx.session_id = Some("s1".into());
        let fake = Arc::new(FakeHistory {
            messages: vec![
                ChatMessage::user("old1"),
                ChatMessage::assistant("old1-reply"),
            ],
            appended: Mutex::new(Vec::new()),
        });
        ctx.history = fake.clone();

        let out = ConversationHistoryNodeAdapter::new()
            .execute(&node(json!({})), &[input(env)], &ctx)
            .await
            .unwrap();

        assert_eq!(out.context.messages.len(), 3);
        assert_eq!(out.context.messages[0].content, "old1");
        assert_eq!(out.context.messages[1].content, "old1-reply");
        assert_eq!(out.context.messages[2].content, "now");
        let ap = fake.appended.lock().unwrap();
        assert_eq!(ap.len(), 1);
        assert_eq!(ap[0].0, "s1");
        assert_eq!(ap[0].1.content, "now");
    }

    #[tokio::test]
    async fn skips_append_when_payload_is_empty_or_non_text() {
        let env = FlowEnvelope::empty(); // payload = Empty
        let mut ctx = stub_ctx();
        ctx.session_id = Some("s2".into());
        let fake = Arc::new(FakeHistory {
            messages: Vec::new(),
            appended: Mutex::new(Vec::new()),
        });
        ctx.history = fake.clone();

        ConversationHistoryNodeAdapter::new()
            .execute(&node(json!({})), &[input(env)], &ctx)
            .await
            .unwrap();
        assert!(fake.appended.lock().unwrap().is_empty());
    }
}
