// =============================================================================
// Plik: flow_engine/node_adapters/mod.rs
// Opis: Nowe adaptery dla flow_engine clean rewrite (plan v4.2). Każdy
//       implementuje `flow_engine::node_adapter::NodeAdapter` (single execute
//       method). Stage 1b: standalone — koegzystują z legacy `flow_engine::
//       adapters` do czasu executor rewrite w stage 1c.
// =============================================================================

pub mod condition;
pub mod output;
pub mod pii_filter;
pub mod trigger;
pub mod tts_clean;

pub use condition::ConditionNodeAdapter;
pub use output::OutputNodeAdapter;
pub use pii_filter::PiiFilterNodeAdapter;
pub use trigger::TriggerNodeAdapter;
pub use tts_clean::TtsCleanNodeAdapter;
