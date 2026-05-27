// =============================================================================
// Plik: flow_engine/dispatchers/mod.rs
// Opis: Capability dispatcher traits — narrow surface area dla NodeAdapter,
//       zastępuje god-object ServiceManager w nowym executor pipeline (plan v4.1).
//       Stage 1: tylko traity + DTO, implementacje wrapperów dochodzą razem
//       z adapter rewrite (gdy ExecutionContext zacznie istnieć).
// =============================================================================

pub mod audit;
pub mod clock;
pub mod conversation;
pub mod embeddings;
pub mod llm;
pub mod memory;
pub mod metrics;
pub mod pii_rules;
pub mod prompts;
pub mod stt;
pub mod tts;
pub mod tts_cleaning;

pub use audit::{AuditEvent, AuditSink};
pub use clock::Clock;
pub use conversation::ConversationHistoryStore;
pub use embeddings::{EmbeddingsDispatcher, EmbeddingsRequest, EmbeddingsResponse};
pub use llm::{LlmDispatcher, LlmRequest, LlmResponse};
pub use memory::{
    MemoryHit, MemoryQuery, MemoryRecall, MemoryRecord, MemoryStore, MemoryStoreReceipt,
};
pub use metrics::{MetricsSink, NoopMetrics};
pub use pii_rules::{PiiRule, PiiRulesStore};
pub use prompts::PromptStore;
pub use stt::{SttDispatcher, SttRequest, SttResponse};
pub use tts::{TtsDispatcher, TtsRequest, TtsResponse, TtsStreamChunk};
pub use tts_cleaning::TtsCleaningStore;
