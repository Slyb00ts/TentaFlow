// =============================================================================
// Plik: flow_engine/dispatchers_impl/conversation_impl.rs
// Opis: ConversationHistoryImpl — durable conversation history backed by
//       SQLite (`conversation_messages`). This is the source of truth: it keeps
//       the full ChatMessage structure (tool_calls, tool_call_id, name,
//       multimodal parts) that the old in-memory text cache used to drop, and
//       survives restarts. DB work runs on a blocking pool thread because the
//       rusqlite connection behind `DbPool` uses blocking locks.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::db::{repository, DbPool};
use crate::flow_engine::dispatchers::conversation::{
    row_to_message, ConversationHistoryStore, EncodedMessage,
};
use crate::flow_engine::envelope::ChatMessage;

pub struct ConversationHistoryImpl {
    db: DbPool,
}

impl ConversationHistoryImpl {
    pub fn new(db: DbPool) -> Self {
        Self { db }
    }
}

#[async_trait]
impl ConversationHistoryStore for ConversationHistoryImpl {
    async fn recent(&self, session_id: &str, limit: u32) -> Result<Vec<ChatMessage>> {
        let db = self.db.clone();
        let session = session_id.to_string();
        let rows = tokio::task::spawn_blocking(move || {
            repository::recent_conversation_messages(&db, &session, limit)
        })
        .await
        .map_err(|e| anyhow!("conversation recent join: {e}"))??;
        rows.iter().map(row_to_message).collect()
    }

    async fn append(&self, session_id: &str, message: ChatMessage) -> Result<()> {
        self.append_batch(session_id, std::slice::from_ref(&message))
            .await
    }

    async fn append_batch(&self, session_id: &str, messages: &[ChatMessage]) -> Result<()> {
        if messages.is_empty() {
            return Ok(());
        }
        let encoded = messages
            .iter()
            .map(EncodedMessage::from_message)
            .collect::<Result<Vec<_>>>()?;
        let db = self.db.clone();
        let session = session_id.to_string();
        tokio::task::spawn_blocking(move || {
            let rows: Vec<_> = encoded.iter().map(EncodedMessage::as_new_row).collect();
            repository::insert_conversation_messages(&db, &session, &rows)
        })
        .await
        .map_err(|e| anyhow!("conversation append join: {e}"))??;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::envelope::{ChatMessageContent, ChatRole, LlmToolCall};
    use std::path::Path;

    fn test_db() -> DbPool {
        // `db::init` runs migrations (including conversation_messages, v70).
        crate::db::init(Path::new(":memory:")).expect("init db")
    }

    #[tokio::test]
    async fn append_then_recent_roundtrip() {
        let store = ConversationHistoryImpl::new(test_db());
        store.append("s1", ChatMessage::user("hello")).await.unwrap();
        store
            .append("s1", ChatMessage::assistant("hi back"))
            .await
            .unwrap();
        let h = store.recent("s1", 10).await.unwrap();
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].role, ChatRole::User);
        assert_eq!(h[1].role, ChatRole::Assistant);
        assert_eq!(h[0].text(), Some("hello"));
    }

    #[tokio::test]
    async fn recent_respects_limit_returns_tail_in_order() {
        let store = ConversationHistoryImpl::new(test_db());
        for i in 0..5 {
            store
                .append("s", ChatMessage::user(format!("m{i}")))
                .await
                .unwrap();
        }
        let h = store.recent("s", 2).await.unwrap();
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].text(), Some("m3"));
        assert_eq!(h[1].text(), Some("m4"));
    }

    #[tokio::test]
    async fn tool_calls_and_tool_result_round_trip() {
        let store = ConversationHistoryImpl::new(test_db());
        let assistant = ChatMessage {
            role: ChatRole::Assistant,
            content: ChatMessageContent::Text(String::new()),
            name: None,
            tool_call_id: None,
            tool_calls: Some(vec![LlmToolCall {
                id: "call_1".into(),
                name: "get_weather".into(),
                arguments: "{\"city\":\"Warsaw\"}".into(),
            }]),
        };
        let tool_result = ChatMessage {
            role: ChatRole::Tool,
            content: ChatMessageContent::Text("18C".into()),
            name: Some("get_weather".into()),
            tool_call_id: Some("call_1".into()),
            tool_calls: None,
        };
        store
            .append_batch("s", &[assistant.clone(), tool_result.clone()])
            .await
            .unwrap();
        let h = store.recent("s", 10).await.unwrap();
        assert_eq!(h.len(), 2);
        assert_eq!(h[0], assistant);
        assert_eq!(h[1], tool_result);
    }

    #[tokio::test]
    async fn colliding_seq_is_ignored_not_duplicated() {
        // UNIQUE(session_id, seq) makes a retried write at an existing seq a
        // no-op — the idempotency guarantee `persist_turn` relies on.
        let store = ConversationHistoryImpl::new(test_db());
        store
            .append_batch("s", &[ChatMessage::user("a"), ChatMessage::assistant("b")])
            .await
            .unwrap();
        {
            let conn = store.db.write().unwrap();
            let affected = conn
                .execute(
                    "INSERT OR IGNORE INTO conversation_messages
                        (session_id, seq, role, content) VALUES ('s', 0, 'user', 'a')",
                    [],
                )
                .unwrap();
            assert_eq!(affected, 0, "colliding seq must be ignored");
        }
        let h = store.recent("s", 10).await.unwrap();
        assert_eq!(h.len(), 2);
    }

    #[tokio::test]
    async fn multimodal_message_populates_payload_ref_column() {
        use crate::flow_engine::blob_store::BlobRef;
        use crate::flow_engine::envelope::MessagePart;

        let pool = test_db();
        let store = ConversationHistoryImpl::new(pool.clone());
        let blob = BlobRef {
            id: "blob-123".into(),
            size_bytes: 9,
            mime: "image/png".into(),
            sha256: "deadbeef".into(),
        };
        let msg = ChatMessage::user_multimodal(vec![
            MessagePart::Text { text: "see".into() },
            MessagePart::Image {
                blob_ref: blob,
                detail: "auto".into(),
            },
        ]);
        store.append("s", msg.clone()).await.unwrap();

        // The queryable pointer columns are filled and the full Parts content
        // still round-trips through `recent`.
        let (payload_ref, payload_kind): (Option<String>, Option<String>) = {
            let conn = pool.read().unwrap();
            conn.query_row(
                "SELECT payload_ref, payload_kind FROM conversation_messages WHERE session_id='s'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
        };
        assert_eq!(payload_ref.as_deref(), Some("blob-123"));
        assert_eq!(payload_kind.as_deref(), Some("image"));
        let h = store.recent("s", 10).await.unwrap();
        assert_eq!(h[0], msg);
    }

    #[tokio::test]
    async fn survives_restart_new_store_same_pool() {
        let pool = test_db();
        {
            let store = ConversationHistoryImpl::new(pool.clone());
            store
                .append("s", ChatMessage::user("persisted"))
                .await
                .unwrap();
        }
        // "Restart": a fresh store over the same pool. SQLite is the truth, so
        // the message is still there.
        let store2 = ConversationHistoryImpl::new(pool);
        let h = store2.recent("s", 10).await.unwrap();
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].text(), Some("persisted"));
    }
}
