// =============================================================================
// Plik: flow_engine/dispatchers/conversation.rs
// Opis: ConversationHistoryStore — durable conversation history contract.
//       SQLite is the source of truth; the in-memory cache is only a
//       read-through buffer. `recent` replays the last N messages for a
//       session in chronological order with full structure (tool_calls,
//       tool_call_id, name, multimodal parts). `append`/`append_batch` persist
//       turns durably. Mapping helpers between the DB row shape and ChatMessage
//       live here so both the store impl and the `persist_turn` node share one
//       lossless round-trip.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::db::models::{DbConversationMessage, NewConversationMessage};
use crate::flow_engine::envelope::{ChatMessage, ChatMessageContent, ChatRole, MessagePart};

#[async_trait]
pub trait ConversationHistoryStore: Send + Sync {
    /// Pobierz ostatnie `limit` wiadomości dla sesji. Pusta lista gdy brak.
    async fn recent(&self, session_id: &str, limit: u32) -> Result<Vec<ChatMessage>>;

    /// Dopisz pojedynczą wiadomość do historii (durable).
    async fn append(&self, session_id: &str, message: ChatMessage) -> Result<()>;

    /// Dopisz całą deltę tury jednym transakcyjnym batchem (durable,
    /// idempotentny na retry przez UNIQUE(session_id, seq)).
    async fn append_batch(&self, session_id: &str, messages: &[ChatMessage]) -> Result<()>;
}

/// Stable lowercase wire tag for a role — matches the table CHECK constraint.
pub fn role_to_str(r: ChatRole) -> &'static str {
    match r {
        ChatRole::System => "system",
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
        ChatRole::Tool => "tool",
    }
}

/// Parse a stored role tag. Unknown values fall back to User — legacy logs
/// could carry raw strings, and a malformed row must not abort a replay.
pub fn role_from_str(s: &str) -> ChatRole {
    match s.to_ascii_lowercase().as_str() {
        "system" => ChatRole::System,
        "assistant" => ChatRole::Assistant,
        "tool" => ChatRole::Tool,
        _ => ChatRole::User,
    }
}

/// Owned serialized columns for one outgoing row. Borrowed into a
/// `NewConversationMessage` at the insert call site (`as_new_row`) so the
/// repository keeps its `&str` insert shape while the serialized JSON lives long
/// enough for the batch transaction.
pub struct EncodedMessage {
    role: &'static str,
    content: Option<String>,
    tool_calls: Option<String>,
    tool_call_id: Option<String>,
    name: Option<String>,
    payload_ref: Option<String>,
    payload_kind: Option<&'static str>,
    node_id: Option<String>,
}

impl EncodedMessage {
    /// Encode a `ChatMessage` for storage. Text content goes to `content`;
    /// multimodal Parts content is serialized to `content` (lossless round-trip)
    /// AND, when it carries an image, the first image's BlobRef id is mirrored
    /// into the queryable `payload_ref` column with `payload_kind = "image"`.
    /// The blob bytes already live in the BlobStore — this is only a pointer.
    pub fn from_message(msg: &ChatMessage) -> Result<Self> {
        let (content, payload_ref, payload_kind) = match &msg.content {
            ChatMessageContent::Text(t) => (Some(t.clone()), None, None),
            ChatMessageContent::Parts(parts) => {
                let raw = serde_json::to_string(&msg.content)
                    .map_err(|e| anyhow!("serialize multimodal content: {e}"))?;
                let blob_id = parts.iter().find_map(|p| match p {
                    MessagePart::Image { blob_ref, .. } => Some(blob_ref.id.clone()),
                    MessagePart::Text { .. } => None,
                });
                match blob_id {
                    Some(id) => (Some(raw), Some(id), Some("image")),
                    None => (Some(raw), None, Some("parts")),
                }
            }
        };
        let tool_calls = match &msg.tool_calls {
            Some(calls) => Some(
                serde_json::to_string(calls).map_err(|e| anyhow!("serialize tool_calls: {e}"))?,
            ),
            None => None,
        };
        Ok(Self {
            role: role_to_str(msg.role),
            content,
            tool_calls,
            tool_call_id: msg.tool_call_id.clone(),
            name: msg.name.clone(),
            payload_ref,
            payload_kind,
            node_id: None,
        })
    }

    pub fn as_new_row(&self) -> NewConversationMessage<'_> {
        NewConversationMessage {
            role: self.role,
            content: self.content.as_deref(),
            tool_calls: self.tool_calls.clone(),
            tool_call_id: self.tool_call_id.as_deref(),
            name: self.name.as_deref(),
            payload_ref: self.payload_ref.as_deref(),
            payload_kind: self.payload_kind,
            node_id: self.node_id.as_deref(),
        }
    }
}

/// Reconstruct a `ChatMessage` from a stored row with full structure. Multimodal
/// Parts content (`payload_kind = "parts"`) is deserialized from `content`;
/// `tool_calls` JSON is parsed back to the typed vec. A malformed JSON column is
/// an error so a corrupt row surfaces rather than silently dropping tool calls.
pub fn row_to_message(row: &DbConversationMessage) -> Result<ChatMessage> {
    let content = match (row.payload_kind.as_deref(), &row.content) {
        // "parts" (text-only multimodal) and "image" (parts with a blob) both
        // store the serialized ChatMessageContent::Parts in `content`.
        (Some("parts"), Some(raw)) | (Some("image"), Some(raw)) => serde_json::from_str(raw)
            .map_err(|e| anyhow!("deserialize multimodal content (seq {}): {e}", row.seq))?,
        (_, Some(text)) => ChatMessageContent::Text(text.clone()),
        (_, None) => ChatMessageContent::Text(String::new()),
    };
    let tool_calls = match &row.tool_calls {
        Some(raw) => Some(
            serde_json::from_str(raw)
                .map_err(|e| anyhow!("deserialize tool_calls (seq {}): {e}", row.seq))?,
        ),
        None => None,
    };
    Ok(ChatMessage {
        role: role_from_str(&row.role),
        content,
        name: row.name.clone(),
        tool_call_id: row.tool_call_id.clone(),
        tool_calls,
    })
}
