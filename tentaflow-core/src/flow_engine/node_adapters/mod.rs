// =============================================================================
// Plik: flow_engine/node_adapters/mod.rs
// Opis: Nowe adaptery dla flow_engine clean rewrite (plan v4.2). Każdy
//       implementuje `flow_engine::node_adapter::NodeAdapter` (single execute
//       method). Stage 1b: standalone — koegzystują z legacy `flow_engine::
//       adapters` do czasu executor rewrite w stage 1c.
// =============================================================================

pub mod condition;
pub mod conversation_history;
pub mod embeddings;
pub mod llm;
pub mod memory;
pub mod output;
pub mod pii_filter;
pub mod session_context;
pub mod speaker_context;
pub mod stt;
pub mod trigger;
pub mod tts;
pub mod tts_clean;
pub mod vision_llm;

pub use condition::ConditionNodeAdapter;
pub use conversation_history::ConversationHistoryNodeAdapter;
pub use embeddings::EmbeddingsNodeAdapter;
pub use llm::LlmNodeAdapter;
pub use memory::MemoryNodeAdapter;
pub use output::OutputNodeAdapter;
pub use pii_filter::PiiFilterNodeAdapter;
pub use session_context::SessionContextNodeAdapter;
pub use speaker_context::SpeakerContextNodeAdapter;
pub use stt::SttNodeAdapter;
pub use trigger::TriggerNodeAdapter;
pub use tts::TtsNodeAdapter;
pub use tts_clean::TtsCleanNodeAdapter;
pub use vision_llm::VisionNodeAdapter;
