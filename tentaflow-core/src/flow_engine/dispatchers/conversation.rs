// =============================================================================
// Plik: flow_engine/dispatchers/conversation.rs
// Opis: ConversationHistoryStore — narrow trait nad conversation_cache
//       z adapters/conversation_history.rs:43. Adapter prosi o ostatnie N
//       wiadomości dla session_id, store zwraca w kolejności chronologicznej.
// =============================================================================

use anyhow::Result;
use async_trait::async_trait;

use crate::flow_engine::envelope::ChatMessage;

#[async_trait]
pub trait ConversationHistoryStore: Send + Sync {
    /// Pobierz ostatnie `limit` wiadomości dla sesji. Pusta lista gdy brak.
    async fn recent(&self, session_id: &str, limit: u32) -> Result<Vec<ChatMessage>>;

    /// Dopisz wiadomość do historii. Adapter wywołuje po zakończeniu turn'a.
    async fn append(&self, session_id: &str, message: ChatMessage) -> Result<()>;
}
