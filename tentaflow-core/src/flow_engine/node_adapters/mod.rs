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
pub mod ask_user;
pub mod await_subagents;
pub mod camera_alert;
pub mod camera_verdict;
pub mod chunk;
pub mod combine;
pub mod compact_context;
pub mod condition;
pub mod conversation_history;
pub mod critic_gate;
pub mod delegate_cli;
pub mod document_merge;
pub mod document_parse;
pub mod document_router;
pub mod embed_chunks;
pub mod embeddings;
pub mod exec_command;
pub mod task_gate;
// Registered UNCONDITIONALLY: a seeded flow naming `graph_extract` must still
// validate on a build without cozo (the writes inside are feature-gated).
pub mod graph_extract;
#[cfg(feature = "graph")]
pub mod graph_search;
pub mod graphic_elements;
pub mod interval;
pub mod llm;
pub mod loop_block;
pub mod map_block;
pub mod memory;
pub mod ocr;
pub mod ocr_pages;
pub mod office_extract;
pub mod on_subagent_complete;
pub mod output;
pub mod page_branch;
pub mod page_detect;
pub mod page_detect_pages;
pub mod patch_review;
pub mod pdf_rasterize;
pub mod persist_turn;
pub mod pii_filter;
pub mod platform_switch;
pub mod project_knowledge;
pub mod rag_graphrag;
pub mod rag_multihop;
pub mod reranker;
pub mod sentence_buffer;
pub mod session_context;
pub mod spawn;
pub mod speaker_context;
pub mod store;
pub mod stt;
pub mod subagent_status;
pub mod subflow;
pub mod table_structure;
pub mod text_extract;
pub mod tool_exec;
pub mod trigger;
pub mod tts;
pub mod tts_clean;
pub mod variable_merge;
pub mod vector;
pub mod vision_classify;
pub mod vision_crop;
pub mod vision_llm;
pub mod vision_ocr;
pub mod vision_parse;
pub mod vision_parse_pages;
pub mod workspace_context;

pub use addon::AddonNodeAdapter;
pub use agent_block::{AgentNodeAdapter, AGENT_RUN_FLOW_ID};
pub use agent_context::AgentContextNodeAdapter;
pub use agent_router::AgentRouterNodeAdapter;
pub use ask_user::AskUserNodeAdapter;
pub use await_subagents::AwaitSubagentsNodeAdapter;
pub use camera_alert::CameraAlertNodeAdapter;
pub use camera_verdict::CameraVerdictNodeAdapter;
pub use chunk::ChunkNodeAdapter;
pub use combine::CombineNodeAdapter;
pub use compact_context::CompactContextNodeAdapter;
pub use condition::ConditionNodeAdapter;
pub use conversation_history::ConversationHistoryNodeAdapter;
pub use critic_gate::CriticGateNodeAdapter;
pub use delegate_cli::DelegateCliNodeAdapter;
pub use document_merge::DocumentMergeNodeAdapter;
pub use document_parse::DocumentParseNodeAdapter;
pub use document_router::DocumentRouterNodeAdapter;
pub use embed_chunks::EmbedChunksNodeAdapter;
pub use embeddings::EmbeddingsNodeAdapter;
pub use exec_command::ExecCommandNodeAdapter;
pub use graph_extract::GraphExtractNodeAdapter;
#[cfg(feature = "graph")]
pub use graph_search::GraphSearchNodeAdapter;
pub use graphic_elements::GraphicElementsNodeAdapter;
pub use interval::IntervalNodeAdapter;
pub use llm::LlmNodeAdapter;
pub use loop_block::LoopNodeAdapter;
pub use map_block::MapNodeAdapter;
pub use memory::MemoryNodeAdapter;
pub use ocr::OcrNodeAdapter;
pub use ocr_pages::OcrPagesNodeAdapter;
pub use office_extract::{ExcelExtractNodeAdapter, PptxExtractNodeAdapter, WordExtractNodeAdapter};
pub use on_subagent_complete::{CompletionFilter, OnSubagentCompleteNodeAdapter};
pub use output::OutputNodeAdapter;
pub use page_detect::PageDetectNodeAdapter;
pub use page_detect_pages::PageDetectPagesNodeAdapter;
pub use patch_review::{InteractionGate, PatchReviewNodeAdapter};
pub use pdf_rasterize::PdfRasterizeNodeAdapter;
pub use persist_turn::PersistTurnNodeAdapter;
pub use pii_filter::PiiFilterNodeAdapter;
pub use platform_switch::PlatformSwitchNodeAdapter;
pub use project_knowledge::ProjectKnowledgeNodeAdapter;
pub use rag_graphrag::{RagGraphFactsNodeAdapter, RagGraphSeedNodeAdapter};
pub use rag_multihop::{
    RagAccumulateNodeAdapter, RagFinalizeNodeAdapter, RagJudgeNodeAdapter, RagQuerySeedNodeAdapter,
};
pub use reranker::RerankerNodeAdapter;
pub use sentence_buffer::SentenceBufferNodeAdapter;
pub use session_context::SessionContextNodeAdapter;
pub use spawn::SpawnNodeAdapter;
pub use speaker_context::SpeakerContextNodeAdapter;
pub use store::StoreNodeAdapter;
pub use stt::SttNodeAdapter;
pub use subagent_status::SubagentStatusNodeAdapter;
pub use subflow::SubflowNodeAdapter;
pub use table_structure::TableStructureNodeAdapter;
pub use task_gate::TaskGateNodeAdapter;
pub use text_extract::TextExtractNodeAdapter;
pub use tool_exec::ToolExecNodeAdapter;
pub use trigger::TriggerNodeAdapter;
pub use tts::TtsNodeAdapter;
pub use tts_clean::TtsCleanNodeAdapter;
pub use vector::VectorNodeAdapter;
pub use vision_classify::VisionClassifyNodeAdapter;
pub use vision_llm::VisionNodeAdapter;
pub use vision_ocr::VisionOcrNodeAdapter;
pub use vision_parse::VisionParseNodeAdapter;
pub use vision_parse_pages::VisionParsePagesNodeAdapter;
pub use workspace_context::WorkspaceContextNodeAdapter;
