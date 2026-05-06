// =============================================================================
// Plik: flow_engine/dispatchers_impl/mod.rs
// Opis: Konkretne implementacje capability dispatcherów (plan v4.2 D4).
//       Każdy wrapper bierze NAJWĘŻSZY runtime/store potrzebny do logiki —
//       żaden impl nie trzyma `Arc<ServiceManager>`. Bootstrap (Router::new)
//       buduje każdy `Arc<dyn ...>` raz i wstrzykuje do `ExecutionContext`.
// =============================================================================

pub mod audit_impl;
pub mod conversation_impl;
pub mod pii_rules_impl;
pub mod prompts_impl;
pub mod tts_cleaning_impl;

pub use audit_impl::AuditSinkImpl;
pub use conversation_impl::ConversationHistoryImpl;
pub use pii_rules_impl::PiiRulesStoreImpl;
pub use prompts_impl::PromptsImpl;
pub use tts_cleaning_impl::TtsCleaningStoreImpl;
