// =============================================================================
// Plik: flow_engine/node_adapters/conversation_history.rs
// Opis: ConversationHistoryNodeAdapter — pobiera ostatnie N wiadomości z
//       ConversationHistoryStore i wstrzykuje je do envelope.context.messages
//       (przed dotychczasowymi). This node is read-only: it records
//       `history_base_len` in meta (count of replayed rows) so a downstream
//       `persist_turn` node can compute the turn delta and write the WHOLE turn
//       (user input, assistant reply, tool results, multimodal) durably. The
//       old "append the user text here" side effect is gone — it only ever
//       captured a Text payload and dropped the assistant reply and tool
//       results, so durable persistence now lives entirely in `persist_turn`.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{FlowEnvelope, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "conversation_history";
const DEFAULT_MAX_MESSAGES: u32 = 20;

/// Meta key carrying the number of messages replayed from the durable store.
/// `persist_turn` reads it as the delta boundary: only `messages[base..]` is a
/// new turn that must be written. Engine plumbing — not user-facing.
pub const HISTORY_BASE_LEN_META: &str = "history_base_len";

pub struct ConversationHistoryNodeAdapter;

impl ConversationHistoryNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Resolves the conversation to replay. `Ok(None)` means "this run has no
    /// conversation": a node with `require_session: false` sits in a shell
    /// shared between a conversational caller and a one-shot one (the RAG
    /// `query` shell answers both the project chat and the addon's `ask`), and
    /// a one-shot run has nothing to replay. Without the opt-in a missing
    /// session stays a hard error, so a chat flow that forgot to pass one still
    /// fails loudly instead of silently losing its memory.
    fn pick_session(node: &FlowNode, ctx: &ExecutionContext) -> Result<Option<String>> {
        if let Some(s) = node
            .config
            .get("session_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return Ok(Some(s.to_string()));
        }
        if let Some(s) = ctx.session_id.clone() {
            return Ok(Some(s));
        }
        let required = node
            .config
            .get("require_session")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        if required {
            return Err(anyhow!(
                "conversation_history adapter: no session_id (node config nor ctx.session_id)"
            ));
        }
        Ok(None)
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
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Any)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("full", FlowDataType::Any)]
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

        let history = match session {
            Some(ref s) => ctx.history.recent(s, max).await?,
            None => Vec::new(),
        };
        let base_len = history.len();

        let mut out: FlowEnvelope = (**envelope).clone();
        // Plan v4.2 D1: cross-node lookup zabity, więc wstrzykujemy historię
        // PRZED istniejącymi messages — zachowujemy chronologię (najstarsza
        // pierwsza). Inline system prompts z envelope.context.system_prompts
        // dochodzą później w llm adapter.
        let mut new_msgs = history;
        new_msgs.extend(out.context.messages.drain(..));
        out.context.messages = new_msgs;

        // Delta boundary for `persist_turn`: everything from `base_len` onward
        // is the live turn (user input + assistant reply + tool results) that
        // must be written durably; the prefix is already-persisted history.
        out.meta.insert(
            HISTORY_BASE_LEN_META.to_string(),
            serde_json::Value::from(base_len),
        );
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::dispatchers::ConversationHistoryStore;
    use crate::flow_engine::envelope::{ChatMessage, FlowValue};
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
            region: None,
        }
    }

    fn input(env: FlowEnvelope) -> NodeInput {
        NodeInput {
            from_node_id: "trigger".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }
    }

    /// Read-only fake: this node must NOT write, so any append is a test
    /// failure (durable persistence moved to `persist_turn`).
    struct FakeHistory {
        messages: Vec<ChatMessage>,
        appended: Mutex<usize>,
    }

    #[async_trait]
    impl ConversationHistoryStore for FakeHistory {
        async fn recent(&self, _: &str, _: u32) -> Result<Vec<ChatMessage>> {
            Ok(self.messages.clone())
        }
        async fn append(&self, _: &str, _: ChatMessage) -> Result<()> {
            *self.appended.lock().unwrap() += 1;
            Ok(())
        }
        async fn append_batch(&self, _: &str, m: &[ChatMessage]) -> Result<()> {
            *self.appended.lock().unwrap() += m.len();
            Ok(())
        }
    }

    #[tokio::test]
    async fn injects_recent_history_before_existing_and_sets_base_len() {
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
            appended: Mutex::new(0),
        });
        ctx.history = fake.clone();

        let out = ConversationHistoryNodeAdapter::new()
            .execute(&node(json!({})), &[input(env)], &ctx)
            .await
            .unwrap();

        assert_eq!(out.context.messages.len(), 3);
        assert_eq!(out.context.messages[0].text(), Some("old1"));
        assert_eq!(out.context.messages[1].text(), Some("old1-reply"));
        assert_eq!(out.context.messages[2].text(), Some("now"));
        // base_len = 2 replayed rows; the live turn is messages[2..].
        assert_eq!(
            out.meta.get(HISTORY_BASE_LEN_META),
            Some(&serde_json::Value::from(2usize))
        );
        // Read node never writes.
        assert_eq!(*fake.appended.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn empty_history_yields_base_len_zero_and_no_write() {
        let env = FlowEnvelope::empty();
        let mut ctx = stub_ctx();
        ctx.session_id = Some("s2".into());
        let fake = Arc::new(FakeHistory {
            messages: Vec::new(),
            appended: Mutex::new(0),
        });
        ctx.history = fake.clone();

        let out = ConversationHistoryNodeAdapter::new()
            .execute(&node(json!({})), &[input(env)], &ctx)
            .await
            .unwrap();
        assert_eq!(
            out.meta.get(HISTORY_BASE_LEN_META),
            Some(&serde_json::Value::from(0usize))
        );
        assert_eq!(*fake.appended.lock().unwrap(), 0);
    }

    /// A run without a conversation is an error by default — a chat flow that
    /// forgot its session must fail loudly, not silently lose its memory.
    #[tokio::test]
    async fn missing_session_is_an_error_by_default() {
        let env = FlowEnvelope::empty();
        let mut ctx = stub_ctx();
        ctx.session_id = None;
        let err = ConversationHistoryNodeAdapter::new()
            .execute(&node(json!({})), &[input(env)], &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no session_id"));
    }

    /// The shared RAG shell opts out: the addon's one-shot `ask` has nothing to
    /// replay, so the node passes through with an empty history.
    #[tokio::test]
    async fn missing_session_replays_nothing_when_not_required() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("question".into());
        let mut ctx = stub_ctx();
        ctx.session_id = None;
        let fake = Arc::new(FakeHistory {
            messages: vec![ChatMessage::user("other-session")],
            appended: Mutex::new(0),
        });
        ctx.history = fake.clone();

        let out = ConversationHistoryNodeAdapter::new()
            .execute(&node(json!({"require_session": false})), &[input(env)], &ctx)
            .await
            .expect("shared shell answers a caller without a conversation");
        assert!(out.context.messages.is_empty(), "nothing to replay");
        assert_eq!(
            out.meta.get(HISTORY_BASE_LEN_META),
            Some(&serde_json::Value::from(0usize))
        );
        assert_eq!(out.payload.as_text(), Some("question"));
    }
}
