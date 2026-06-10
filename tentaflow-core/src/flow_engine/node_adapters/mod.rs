// =============================================================================
// Plik: flow_engine/node_adapters/mod.rs
// Opis: Nowe adaptery dla flow_engine clean rewrite (plan v4.2). Każdy
//       implementuje `flow_engine::node_adapter::NodeAdapter` (single execute
//       method). Stage 1b: standalone — koegzystują z legacy `flow_engine::
//       adapters` do czasu executor rewrite w stage 1c.
// =============================================================================

pub mod addon;
pub mod agent_block;
pub mod agent_context;
pub mod agent_router;
pub mod combine;
pub mod compact_context;
pub mod condition;
pub mod conversation_history;
pub mod embeddings;
pub mod llm;
pub mod loop_block;
pub mod map_block;
pub mod memory;
pub mod output;
pub mod pii_filter;
pub mod sentence_buffer;
pub mod session_context;
pub mod speaker_context;
pub mod stt;
pub mod subflow;
pub mod tool_exec;
pub mod trigger;
pub mod tts;
pub mod tts_clean;
pub mod variable_merge;
pub mod vision_llm;

pub use addon::AddonNodeAdapter;
pub use agent_block::AgentNodeAdapter;
pub use agent_context::AgentContextNodeAdapter;
pub use agent_router::AgentRouterNodeAdapter;
pub use combine::CombineNodeAdapter;
pub use compact_context::CompactContextNodeAdapter;
pub use condition::ConditionNodeAdapter;
pub use conversation_history::ConversationHistoryNodeAdapter;
pub use embeddings::EmbeddingsNodeAdapter;
pub use llm::LlmNodeAdapter;
pub use loop_block::LoopNodeAdapter;
pub use map_block::MapNodeAdapter;
pub use memory::MemoryNodeAdapter;
pub use output::OutputNodeAdapter;
pub use pii_filter::PiiFilterNodeAdapter;
pub use sentence_buffer::SentenceBufferNodeAdapter;
pub use session_context::SessionContextNodeAdapter;
pub use speaker_context::SpeakerContextNodeAdapter;
pub use stt::SttNodeAdapter;
pub use subflow::SubflowNodeAdapter;
pub use tool_exec::ToolExecNodeAdapter;
pub use trigger::TriggerNodeAdapter;
pub use tts::TtsNodeAdapter;
pub use tts_clean::TtsCleanNodeAdapter;
pub use vision_llm::VisionNodeAdapter;
