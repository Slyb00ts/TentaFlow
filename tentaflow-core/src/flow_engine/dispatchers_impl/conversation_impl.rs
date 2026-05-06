// =============================================================================
// Plik: flow_engine/dispatchers_impl/conversation_impl.rs
// Opis: ConversationHistoryImpl — wrapper nad in-memory `ConversationCache`
//       (services::runtime::quic_handle::ConversationCache). Adapter
//       conversation_history widzi tylko `recent(session, limit)` i
//       `append(session, msg)`. DB persist jest poza zakresem (cache jest
//       efemeryczny per-process; persist do `conversation_log` zostaje na
//       późniejszy etap razem z user-defined memory backendami).
// =============================================================================

use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

use crate::flow_engine::dispatchers::ConversationHistoryStore;
use crate::flow_engine::envelope::{ChatMessage, ChatRole};
use crate::services::runtime::quic_handle::ConversationCache;

pub struct ConversationHistoryImpl {
    cache: Arc<ConversationCache>,
}

impl ConversationHistoryImpl {
    pub fn new(cache: Arc<ConversationCache>) -> Self {
        Self { cache }
    }
}

fn role_from_str(s: &str) -> ChatRole {
    // Cache trzyma role jako String — mapujemy na typed wariant. Nieznane
    // wartości traktujemy jako User (legacy logi zapisywały surowe stringi
    // bez walidacji).
    match s.to_ascii_lowercase().as_str() {
        "system" => ChatRole::System,
        "assistant" => ChatRole::Assistant,
        "tool" => ChatRole::Tool,
        _ => ChatRole::User,
    }
}

fn role_to_str(r: ChatRole) -> &'static str {
    match r {
        ChatRole::System => "system",
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
        ChatRole::Tool => "tool",
    }
}

#[async_trait]
impl ConversationHistoryStore for ConversationHistoryImpl {
    async fn recent(&self, session_id: &str, limit: u32) -> Result<Vec<ChatMessage>> {
        let raw = self.cache.get_history(session_id).await;
        let take = limit as usize;
        let start = raw.len().saturating_sub(take);
        Ok(raw
            .into_iter()
            .skip(start)
            .map(|m| ChatMessage {
                role: role_from_str(&m.role),
                // Etap 3b: cache trzyma plain String — zawsze Text content.
                // Multimodal historia (image w cache) wraca razem z storage
                // rewrite (Etap 4+).
                content: crate::flow_engine::envelope::ChatMessageContent::Text(m.content),
                name: None,
                tool_call_id: None,
            })
            .collect())
    }

    async fn append(&self, session_id: &str, message: ChatMessage) -> Result<()> {
        // Cache string-only — text_or_default skleja text parts gdy
        // multimodal, image parts są dropowane (storage limit, wraca z
        // multimodal cache w late Etap 3+).
        let content = message.text_or_default();
        self.cache
            .add_message(session_id, role_to_str(message.role), &content)
            .await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn append_then_recent_roundtrip() {
        let cache = Arc::new(ConversationCache::new());
        let store = ConversationHistoryImpl::new(cache);
        store
            .append("s1", ChatMessage::user("hello"))
            .await
            .unwrap();
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
    async fn recent_respects_limit_returns_tail() {
        let cache = Arc::new(ConversationCache::new());
        let store = ConversationHistoryImpl::new(cache);
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
}
