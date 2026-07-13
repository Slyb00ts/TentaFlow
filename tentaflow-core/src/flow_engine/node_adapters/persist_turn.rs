// =============================================================================
// Plik: flow_engine/node_adapters/persist_turn.rs
// Opis: PersistTurnNodeAdapter — durably writes the delta of the current
//       conversation turn (`context.messages[base..]`) to the conversation
//       history store. `base` is `meta["history_base_len"]` set by the
//       `conversation_history` read node (0 when absent), so only the live turn
//       (user input + assistant reply + tool results, multimodal included) is
//       persisted — never the replayed prefix. Idempotent: the store's
//       UNIQUE(session_id, seq) makes a retried run a no-op. Sink-ish: the
//       envelope passes through unchanged so downstream nodes are unaffected.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{ChatMessageContent, FlowEnvelope, MessagePart, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::node_adapters::conversation_history::HISTORY_BASE_LEN_META;
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "persist_turn";

pub struct PersistTurnNodeAdapter;

impl PersistTurnNodeAdapter {
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
        ctx.session_id.clone().ok_or_else(|| {
            anyhow!("persist_turn adapter: no session_id (node config nor ctx.session_id)")
        })
    }
}

impl Default for PersistTurnNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for PersistTurnNodeAdapter {
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
            .ok_or_else(|| anyhow!("persist_turn adapter: missing input edge"))?;
        let envelope = &input.envelope;

        let session = Self::pick_session(node, ctx)?;
        let base = envelope
            .meta
            .get(HISTORY_BASE_LEN_META)
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;

        let messages = &envelope.context.messages;
        if base < messages.len() {
            let delta = &messages[base..];
            // Multimodal image blobs already live in the BlobStore (the vision
            // path put them there before they reached a message). Reading them
            // back via ctx.blobs and re-putting confirms the bytes are
            // retrievable from the durable store before we persist a row that
            // points at them — a dangling payload_ref would be worse than a
            // failed turn write. The serialized Parts content (written by the
            // store) keeps the full round-trip; payload_ref/payload_kind are the
            // queryable pointer columns.
            for msg in delta {
                if let ChatMessageContent::Parts(parts) = &msg.content {
                    for part in parts {
                        if let MessagePart::Image { blob_ref, .. } = part {
                            ctx.blobs.get(blob_ref).await.map_err(|e| {
                                anyhow!("persist_turn: image blob {} unreadable: {e}", blob_ref.id)
                            })?;
                        }
                    }
                }
            }
            ctx.history.append_batch(&session, delta).await?;
        }

        Ok((**envelope).clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::dispatchers::ConversationHistoryStore;
    use crate::flow_engine::envelope::{ChatMessage, MessagePart};
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use std::sync::{Arc, Mutex};

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "p1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    fn input(env: FlowEnvelope) -> NodeInput {
        NodeInput {
            from_node_id: "llm".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }
    }

    #[derive(Default)]
    struct RecordingHistory {
        batches: Mutex<Vec<(String, Vec<ChatMessage>)>>,
    }

    #[async_trait]
    impl ConversationHistoryStore for RecordingHistory {
        async fn recent(&self, _: &str, _: u32) -> Result<Vec<ChatMessage>> {
            Ok(Vec::new())
        }
        async fn append(&self, s: &str, m: ChatMessage) -> Result<()> {
            self.append_batch(s, std::slice::from_ref(&m)).await
        }
        async fn append_batch(&self, s: &str, m: &[ChatMessage]) -> Result<()> {
            self.batches
                .lock()
                .unwrap()
                .push((s.to_string(), m.to_vec()));
            Ok(())
        }
    }

    #[tokio::test]
    async fn persists_only_delta_after_base() {
        let mut env = FlowEnvelope::empty();
        // Replayed prefix (2) + live turn (2): only [2..] must be written.
        env.context.messages = vec![
            ChatMessage::user("old-q"),
            ChatMessage::assistant("old-a"),
            ChatMessage::user("new-q"),
            ChatMessage::assistant("new-a"),
        ];
        env.meta.insert(
            HISTORY_BASE_LEN_META.to_string(),
            serde_json::Value::from(2usize),
        );
        let mut ctx = stub_ctx();
        ctx.session_id = Some("s1".into());
        let hist = Arc::new(RecordingHistory::default());
        ctx.history = hist.clone();

        let out = PersistTurnNodeAdapter::new()
            .execute(&node(serde_json::json!({})), &[input(env.clone())], &ctx)
            .await
            .unwrap();

        let b = hist.batches.lock().unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].0, "s1");
        assert_eq!(b[0].1.len(), 2);
        assert_eq!(b[0].1[0].text(), Some("new-q"));
        assert_eq!(b[0].1[1].text(), Some("new-a"));
        // Passthrough: envelope unchanged.
        assert_eq!(out.context.messages.len(), 4);
    }

    #[tokio::test]
    async fn no_base_meta_persists_whole_context() {
        let mut env = FlowEnvelope::empty();
        env.context.messages = vec![ChatMessage::user("q"), ChatMessage::assistant("a")];
        let mut ctx = stub_ctx();
        ctx.session_id = Some("s".into());
        let hist = Arc::new(RecordingHistory::default());
        ctx.history = hist.clone();

        PersistTurnNodeAdapter::new()
            .execute(&node(serde_json::json!({})), &[input(env)], &ctx)
            .await
            .unwrap();
        let b = hist.batches.lock().unwrap();
        assert_eq!(b[0].1.len(), 2);
    }

    #[tokio::test]
    async fn empty_delta_writes_nothing() {
        let mut env = FlowEnvelope::empty();
        env.context.messages = vec![ChatMessage::user("only")];
        env.meta.insert(
            HISTORY_BASE_LEN_META.to_string(),
            serde_json::Value::from(1usize),
        );
        let mut ctx = stub_ctx();
        ctx.session_id = Some("s".into());
        let hist = Arc::new(RecordingHistory::default());
        ctx.history = hist.clone();

        PersistTurnNodeAdapter::new()
            .execute(&node(serde_json::json!({})), &[input(env)], &ctx)
            .await
            .unwrap();
        assert!(hist.batches.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn multimodal_message_requires_readable_blob() {
        let mut ctx = stub_ctx();
        ctx.session_id = Some("s".into());
        let hist = Arc::new(RecordingHistory::default());
        ctx.history = hist.clone();

        // Put a real image blob so the readability check passes.
        let blob = ctx
            .blobs
            .put(b"png-bytes".to_vec(), "image/png")
            .await
            .unwrap();
        let mut env = FlowEnvelope::empty();
        env.context.messages = vec![ChatMessage::user_multimodal(vec![
            MessagePart::Text {
                text: "look".into(),
            },
            MessagePart::Image {
                blob_ref: blob,
                detail: "auto".into(),
            },
        ])];
        // base absent → whole context.

        PersistTurnNodeAdapter::new()
            .execute(&node(serde_json::json!({})), &[input(env)], &ctx)
            .await
            .unwrap();
        let b = hist.batches.lock().unwrap();
        assert_eq!(b[0].1.len(), 1);
        // The message kept its multimodal Parts content.
        assert!(matches!(b[0].1[0].content, ChatMessageContent::Parts(_)));
    }
}
