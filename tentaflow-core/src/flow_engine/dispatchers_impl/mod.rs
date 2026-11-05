// =============================================================================
// Plik: flow_engine/dispatchers_impl/mod.rs
// Opis: Konkretne implementacje capability dispatcherów (plan v4.2 D4).
//       Każdy wrapper bierze NAJWĘŻSZY runtime/store potrzebny do logiki —
//       żaden impl nie trzyma `Arc<ServiceManager>`. Bootstrap (Router::new)
//       buduje każdy `Arc<dyn ...>` raz i wstrzykuje do `ExecutionContext`.
// =============================================================================

pub mod audit_impl;
pub mod conversation_impl;
pub mod documents_impl;
pub mod embeddings_impl;
pub mod llm_impl;
pub mod memory_impl;
pub mod pii_rules_impl;
pub mod prompts_impl;
pub mod quic_finder;
pub mod rerank_impl;
pub mod stt_impl;
pub mod tts_cleaning_impl;
pub mod tts_impl;
pub mod vision_impl;

use std::sync::Arc;

/// Slot na `ModelRuntimeExecutor` — Router::new tworzy slot pusty, później
/// (po skonstruowaniu executora) wpina przez `slot.write() = Some(...)`. LLM,
/// embeddings i TTS dispatcher impls czytają slot leniwie przy każdym calls.
pub type ModelRuntimeSlot =
    Arc<parking_lot::RwLock<Option<Arc<crate::services::runtime::executor::ModelRuntimeExecutor>>>>;

/// Buduje `UserContext` z opcjonalnych pól request DTO. Brak `user_id` =
/// `None` (wewnętrzny / addon / mesh-reverse caller — ACL fail-open).
/// Brak `role` przy obecnym `user_id` defaultuje na "user" (najściślejsza
/// rola, ACL i tak gateuje wcześniej w FlowDispatcher::acl_allow).
pub(crate) fn build_user_context(
    user_id: Option<String>,
    user_role: Option<&str>,
) -> Option<crate::auth::acl::UserContext> {
    user_id.map(|uid| crate::auth::acl::UserContext {
        user_id: uid,
        role: user_role.unwrap_or("user").to_string(),
    })
}

/// Node id the AI gateway stamps on events and uses to find token leases. It is
/// the mesh node id (leases are keyed by it), resolved per call because the mesh
/// registry is still empty when the dispatchers are built. Before the registry
/// is wired the id is `local`, like the routing layer's gateway.
pub(crate) fn gateway_node_id(
    provider: &crate::services::runtime::resolver::LocalNodeIdProvider,
) -> String {
    let id = provider();
    if id.is_empty() {
        "local".to_string()
    } else {
        id
    }
}

/// Whether a gateway refusal is a spent token quota (as opposed to an
/// infrastructure failure while opening the audit event).
pub(crate) fn is_quota_refusal(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<crate::error::CoreError>(),
        Some(crate::error::CoreError::RateLimitExceeded { .. })
    )
}

/// Wraps a dispatcher failure with the adapter's prefix. A quota refusal keeps
/// its type (so `DispatchError::from` can still downcast it and the edge answers
/// 429); every other failure becomes plain text, as before.
pub(crate) fn dispatcher_failure(prefix: &str, error: anyhow::Error) -> anyhow::Error {
    if is_quota_refusal(&error) {
        error.context(prefix.to_string())
    } else {
        anyhow::anyhow!("{prefix}: {error}")
    }
}

pub use audit_impl::AuditSinkImpl;
pub use conversation_impl::ConversationHistoryImpl;
pub use documents_impl::DocumentsDispatcherImpl;
pub use embeddings_impl::EmbeddingsDispatcherImpl;
pub use llm_impl::LlmDispatcherImpl;
pub use memory_impl::MemoryStoreImpl;
pub use pii_rules_impl::PiiRulesStoreImpl;
pub use prompts_impl::PromptsImpl;
pub use quic_finder::{QuicClientFinder, ServiceManagerQuicFinder};
pub use rerank_impl::RerankDispatcherImpl;
pub use stt_impl::SttDispatcherImpl;
pub use tts_cleaning_impl::TtsCleaningStoreImpl;
pub use tts_impl::TtsDispatcherImpl;
pub use vision_impl::VisionDispatcherImpl;
