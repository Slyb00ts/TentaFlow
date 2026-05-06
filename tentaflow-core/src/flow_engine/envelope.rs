// =============================================================================
// Plik: flow_engine/envelope.rs
// Opis: typed FlowEnvelope + FlowValue payload, conversation context, trace,
//       outcome, streaming delta. Plan v4.1 — replacement for FlowContext.
//       Stage 1: standalone module, coexists with legacy FlowContext until
//       executor rewrite migrates all call sites.
// =============================================================================

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use super::blob_store::BlobRef;

/// Typed payload niesiony przez envelope. Brak FlowFrame/Many — cardinality 1:1
/// w Etapie 1 (plan hard rule 5).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum FlowValue {
    Empty,
    Text(String),
    Json(serde_json::Value),
    Audio {
        blob_ref: BlobRef,
        mime: String,
        sample_rate: Option<u32>,
    },
    Image {
        blob_ref: BlobRef,
        mime: String,
        dims: Option<(u32, u32)>,
    },
    Video {
        blob_ref: BlobRef,
        mime: String,
        duration_ms: Option<u64>,
    },
    Embedding(Vec<f32>),
}

impl FlowValue {
    pub fn is_empty(&self) -> bool {
        matches!(self, FlowValue::Empty)
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            FlowValue::Text(t) => Some(t.as_str()),
            _ => None,
        }
    }

    /// Krótki tag wariantu używany w komunikatach błędu adapterów —
    /// "Audio", "Text", "Empty" itd. Pozwala adapterowi powiedzieć
    /// dokładnie co dostał gdy spodziewał się innego wariantu.
    pub fn kind(&self) -> &'static str {
        match self {
            FlowValue::Empty => "Empty",
            FlowValue::Text(_) => "Text",
            FlowValue::Json(_) => "Json",
            FlowValue::Audio { .. } => "Audio",
            FlowValue::Image { .. } => "Image",
            FlowValue::Video { .. } => "Video",
            FlowValue::Embedding(_) => "Embedding",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ArtifactProvenance {
    pub producer_node_id: String,
    pub producer_node_type: String,
    pub timestamp_ms: u64,
}

/// Mutable conversation state. System prompts trzymane jako lista — flatten do
/// jednego/wielu System messages dopiero w `LlmAdapter::prepare_request`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ConversationContext {
    #[serde(default)]
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub system_prompts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: ChatMessageContent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// Etap 3b: content multimodal. Pre-3b każdy adapter trzymał `String` —
/// `Text(s)` jest back-compat path. `Parts(...)` używane przez vision
/// adapter / OpenAI request z image_url.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ChatMessageContent {
    Text(String),
    Parts(Vec<MessagePart>),
}

impl Default for ChatMessageContent {
    fn default() -> Self {
        ChatMessageContent::Text(String::new())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessagePart {
    Text {
        text: String,
    },
    /// Image przez `BlobRef` — `LlmDispatcherImpl::chat_msg_to_openai`
    /// rozwiązuje BlobRef → bytes → base64 data URL przed wysłaniem do
    /// backendu. `detail` zgodne z OpenAI vision spec ("auto"/"low"/"high").
    Image {
        blob_ref: crate::flow_engine::blob_store::BlobRef,
        #[serde(default = "default_image_detail")]
        detail: String,
    },
}

fn default_image_detail() -> String {
    "auto".to_string()
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::System,
            content: ChatMessageContent::Text(content.into()),
            name: None,
            tool_call_id: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: ChatMessageContent::Text(content.into()),
            name: None,
            tool_call_id: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: ChatMessageContent::Text(content.into()),
            name: None,
            tool_call_id: None,
        }
    }

    /// Etap 3b: vision-aware konstruktor dla user message z multimodal Parts.
    pub fn user_multimodal(parts: Vec<MessagePart>) -> Self {
        Self {
            role: ChatRole::User,
            content: ChatMessageContent::Parts(parts),
            name: None,
            tool_call_id: None,
        }
    }

    /// Helper Etap 3b: zwraca `Some(&str)` gdy content to czysty Text,
    /// inaczej `None` (Parts). Adaptery które operują tylko na tekście
    /// skipują obrazy używając tego helpera.
    pub fn text(&self) -> Option<&str> {
        match &self.content {
            ChatMessageContent::Text(t) => Some(t.as_str()),
            ChatMessageContent::Parts(_) => None,
        }
    }

    /// Helper Etap 3b: zwraca text — dla Text(s) bezpośrednio, dla
    /// Parts'ów konkatenuje wszystkie text parts (image parts pomijane).
    /// Używane przez adaptery legacy które potrzebują String reprezentacji.
    pub fn text_or_default(&self) -> String {
        match &self.content {
            ChatMessageContent::Text(t) => t.clone(),
            ChatMessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    MessagePart::Text { text } => Some(text.as_str()),
                    MessagePart::Image { .. } => None,
                })
                .collect::<Vec<_>>()
                .join(" "),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Default, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

impl TokenUsage {
    pub fn add(&mut self, other: &TokenUsage) {
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        self.total_tokens += other.total_tokens;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TraceStatus {
    Ok,
    Skipped,
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TraceStep {
    pub node_id: String,
    pub node_type: String,
    pub started_at_ms: u64,
    pub duration_ms: u64,
    pub status: TraceStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Cancelled,
    Error,
}

impl FinishReason {
    /// String w stylu OpenAI dla `chat.completion[.chunk].choices[].finish_reason`.
    /// `Cancelled`/`Error` → null po stronie wire (caller zwraca Value::Null).
    pub fn as_openai_str(&self) -> Option<&'static str> {
        match self {
            FinishReason::Stop => Some("stop"),
            FinishReason::Length => Some("length"),
            FinishReason::ToolCalls => Some("tool_calls"),
            FinishReason::ContentFilter => Some("content_filter"),
            FinishReason::Cancelled | FinishReason::Error => None,
        }
    }
}

impl std::fmt::Display for FinishReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FinishReason::Stop => f.write_str("stop"),
            FinishReason::Length => f.write_str("length"),
            FinishReason::ToolCalls => f.write_str("tool_calls"),
            FinishReason::ContentFilter => f.write_str("content_filter"),
            FinishReason::Cancelled => f.write_str("cancelled"),
            FinishReason::Error => f.write_str("error"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowEnvelope {
    pub schema_version: u16,
    pub payload: FlowValue,
    #[serde(default)]
    pub artifacts: HashMap<String, FlowValue>,
    #[serde(default)]
    pub provenance: HashMap<String, ArtifactProvenance>,
    #[serde(default)]
    pub context: ConversationContext,
    #[serde(default)]
    pub meta: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub trace: Vec<TraceStep>,
}

impl FlowEnvelope {
    pub const SCHEMA_VERSION: u16 = 1;

    pub fn empty() -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            payload: FlowValue::Empty,
            artifacts: HashMap::new(),
            provenance: HashMap::new(),
            context: ConversationContext::default(),
            meta: BTreeMap::new(),
            trace: Vec::new(),
        }
    }

    pub fn with_payload(payload: FlowValue) -> Self {
        let mut env = Self::empty();
        env.payload = payload;
        env
    }

    /// Add-only invariant: duplicate key is an error. Mutation w stylu "node
    /// nadpisuje cudzy artefakt" jest świadomie zakazana — gdyby było
    /// potrzebne, należy użyć osobnej, jawnej operacji `replace_artifact`
    /// (nie ma dziś bo żaden adapter Etapu 1 nie ma takiej semantyki).
    pub fn put_artifact(
        &mut self,
        key: impl Into<String>,
        value: FlowValue,
        provenance: ArtifactProvenance,
    ) -> Result<(), DuplicateArtifactKey> {
        let key = key.into();
        if self.artifacts.contains_key(&key) {
            return Err(DuplicateArtifactKey(key));
        }
        self.artifacts.insert(key.clone(), value);
        self.provenance.insert(key, provenance);
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("duplicate artifact key: {0}")]
pub struct DuplicateArtifactKey(pub String);

/// Wejście do node'a — plan hard rule 1: max 1 input edge w Etapie 1.
/// `Arc<FlowEnvelope>` żeby fan-out w przyszłości nie kopiował.
#[derive(Debug, Clone)]
pub struct NodeInput {
    pub from_node_id: String,
    pub from_port: String,
    pub envelope: Arc<FlowEnvelope>,
}

#[derive(Debug, Clone)]
pub struct FlowExecutionOutcome {
    pub final_envelope: FlowEnvelope,
    pub trace: Vec<TraceStep>,
    pub usage: TokenUsage,
    pub finish_reason: FinishReason,
    pub total_latency_ms: i64,
    pub error: Option<String>,
}

/// Pojedynczy chunk LLM streaming. Mapowany z dispatchera (`StreamChunkType`
/// w `services/runtime/executor.rs:398`). `tool_calls` i `error` pokrywają
/// szerszy protokół `ToolCallDelta`/`Error` z `tentaflow-protocol/src/types.rs`
/// — pole jest puste/None dla większości chunków.
#[derive(Debug, Clone, Default)]
pub struct LlmStreamChunk {
    pub text_delta: String,
    pub reasoning_delta: Option<String>,
    pub tool_calls: Vec<ToolCallDelta>,
    pub usage: Option<TokenUsage>,
    pub finish_reason: Option<FinishReason>,
    /// Engine-level error embedded w streamie (np. backend rate limit). Adapter
    /// może zdecydować — przerwać flow albo skonsumować i kontynuować.
    pub error: Option<String>,
}

/// Streamingowy delta wywołań narzędzi. Indeks identyfikuje slot toola w
/// odpowiedzi, name/arguments_delta nakładają się chunk po chunku w stylu
/// OpenAI tool-calls streaming.
#[derive(Debug, Clone, Default)]
pub struct ToolCallDelta {
    pub index: u32,
    pub id: Option<String>,
    pub function_name: Option<String>,
    pub arguments_delta: Option<String>,
}

/// Streamingowy delta — single-variant dziś, otwarte na przyszłą rozbudowę.
/// Terminal state nigdy nie idzie tym kanałem — wraca przez `outcome_receiver`.
#[derive(Debug, Clone)]
pub enum EnvelopeDelta {
    Llm(LlmStreamChunk),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_usage_add_accumulates() {
        let mut a = TokenUsage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        };
        a.add(&TokenUsage {
            prompt_tokens: 3,
            completion_tokens: 7,
            total_tokens: 10,
        });
        assert_eq!(a.prompt_tokens, 13);
        assert_eq!(a.completion_tokens, 12);
        assert_eq!(a.total_tokens, 25);
    }

    #[test]
    fn finish_reason_openai_mapping() {
        assert_eq!(FinishReason::Stop.as_openai_str(), Some("stop"));
        assert_eq!(FinishReason::ToolCalls.as_openai_str(), Some("tool_calls"));
        assert!(FinishReason::Cancelled.as_openai_str().is_none());
        assert!(FinishReason::Error.as_openai_str().is_none());
    }

    #[test]
    fn envelope_with_payload_seeds_schema_version() {
        let env = FlowEnvelope::with_payload(FlowValue::Text("hi".into()));
        assert_eq!(env.schema_version, FlowEnvelope::SCHEMA_VERSION);
        assert_eq!(env.payload.as_text(), Some("hi"));
    }

    #[test]
    fn put_artifact_writes_provenance_pair() {
        let mut env = FlowEnvelope::empty();
        let prov = ArtifactProvenance {
            producer_node_id: "n1".into(),
            producer_node_type: "stt".into(),
            timestamp_ms: 1000,
        };
        env.put_artifact("transcript", FlowValue::Text("hello".into()), prov.clone())
            .unwrap();
        assert!(env.artifacts.contains_key("transcript"));
        assert_eq!(env.provenance.get("transcript"), Some(&prov));
    }

    #[test]
    fn put_artifact_rejects_duplicate_key() {
        let mut env = FlowEnvelope::empty();
        let prov = ArtifactProvenance {
            producer_node_id: "n1".into(),
            producer_node_type: "stt".into(),
            timestamp_ms: 1000,
        };
        env.put_artifact("k", FlowValue::Text("a".into()), prov.clone())
            .unwrap();
        let err = env
            .put_artifact("k", FlowValue::Text("b".into()), prov)
            .unwrap_err();
        assert_eq!(err.0, "k");
        // Original value preserved.
        assert_eq!(env.artifacts.get("k").and_then(|v| v.as_text()), Some("a"));
    }

    #[test]
    fn flow_value_text_round_trip_json() {
        let v = FlowValue::Text("ok".into());
        let s = serde_json::to_string(&v).unwrap();
        assert!(s.contains("\"kind\":\"text\""), "got: {s}");
        let back: FlowValue = serde_json::from_str(&s).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn flow_value_embedding_round_trip_json() {
        let v = FlowValue::Embedding(vec![0.1, 0.2, 0.3]);
        let s = serde_json::to_string(&v).unwrap();
        let back: FlowValue = serde_json::from_str(&s).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn chat_message_helpers_set_role() {
        assert_eq!(ChatMessage::system("x").role, ChatRole::System);
        assert_eq!(ChatMessage::user("x").role, ChatRole::User);
        assert_eq!(ChatMessage::assistant("x").role, ChatRole::Assistant);
    }
}
