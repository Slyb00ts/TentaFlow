// =============================================================================
// Plik: flow_engine/node_adapters/mod.rs
// Opis: Nowe adaptery dla flow_engine clean rewrite (plan v4.2). Każdy
//       implementuje `flow_engine::node_adapter::NodeAdapter` (single execute
//       method). Stage 1b: standalone — koegzystują z legacy `flow_engine::
//       adapters` do czasu executor rewrite w stage 1c.
// =============================================================================

pub mod condition;
pub mod embeddings;
pub mod llm;
pub mod output;
pub mod pii_filter;
pub mod stt;
pub mod trigger;
pub mod tts;
pub mod tts_clean;

pub use condition::ConditionNodeAdapter;
pub use embeddings::EmbeddingsNodeAdapter;
pub use llm::LlmNodeAdapter;
pub use output::OutputNodeAdapter;
pub use pii_filter::PiiFilterNodeAdapter;
pub use stt::SttNodeAdapter;
pub use trigger::TriggerNodeAdapter;
pub use tts::TtsNodeAdapter;
pub use tts_clean::TtsCleanNodeAdapter;
