// =============================================================================
// Plik: flow_engine/dispatchers_impl/mod.rs
// Opis: Konkretne implementacje capability dispatcherów (plan v4.2 D4).
//       Każdy wrapper bierze NAJWĘŻSZY runtime/store potrzebny do logiki —
//       żaden impl nie trzyma `Arc<ServiceManager>`. Bootstrap (Router::new)
//       buduje każdy `Arc<dyn ...>` raz i wstrzykuje do `ExecutionContext`.
// =============================================================================

pub mod audit_impl;
pub mod conversation_impl;
pub mod embeddings_impl;
pub mod llm_impl;
pub mod memory_impl;
pub mod pii_rules_impl;
pub mod prompts_impl;
pub mod quic_finder;
pub mod stt_impl;
pub mod tts_cleaning_impl;
pub mod tts_impl;

use std::sync::Arc;

/// Slot na `ModelRuntimeExecutor` — Router::new tworzy slot pusty, później
/// (po skonstruowaniu executora) wpina przez `slot.write() = Some(...)`. LLM,
/// embeddings i TTS dispatcher impls czytają slot leniwie przy każdym calls.
pub type ModelRuntimeSlot = Arc<
    parking_lot::RwLock<
        Option<Arc<crate::services::runtime::executor::ModelRuntimeExecutor>>,
    >,
>;

pub use audit_impl::AuditSinkImpl;
pub use conversation_impl::ConversationHistoryImpl;
pub use embeddings_impl::EmbeddingsDispatcherImpl;
pub use llm_impl::LlmDispatcherImpl;
pub use memory_impl::MemoryStoreImpl;
pub use pii_rules_impl::PiiRulesStoreImpl;
pub use prompts_impl::PromptsImpl;
pub use quic_finder::{QuicClientFinder, ServiceManagerQuicFinder};
pub use stt_impl::{SttDispatcherImpl, SttRuntimeSlot};
pub use tts_cleaning_impl::TtsCleaningStoreImpl;
pub use tts_impl::TtsDispatcherImpl;
