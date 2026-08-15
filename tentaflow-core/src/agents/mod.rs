// ===== File: agents/mod.rs — Agent harness service: registry access, tool
// catalog resolution (addon tools ∩ allowlist ∩ permissions + core.* builtins)
// and core builtin tool handlers (Harness §3.3, §3.4, §3.5). Thin by design —
// the agent loop is a Flow Builder flow, not Rust code. =====

mod builtins;
mod catalog;
pub(crate) mod interaction;
mod principal;
mod retention_purge;
pub(crate) mod run_manager;
mod service;
mod subagent_reactor;

use std::sync::Arc;

pub use builtins::{is_core_tool, BuiltinToolError, CoreToolName, CORE_ADDON_ID};
pub use catalog::{tool_in_allowlist, AllowlistEntry, ToolCatalog};
pub use interaction::{
    await_reply as await_interaction_reply, global as interaction_registry_global,
    init_global as interaction_registry_init_global, no_response_sentinel,
    now_ms as interaction_now_ms, run_ask_user, run_permission_request, wrap_user_reply,
    InteractionKind, InteractionOutcome, InteractionRegistry, InteractionReply, PendingInteraction,
    PermissionDecision, QuestionReply, DEFAULT_INTERACTION_TIMEOUT_SECS,
};
pub use principal::AgentPrincipal;
pub use retention_purge::{purge_expired_agent_runtime, start_agent_runtime_purge_task};
pub use run_manager::{
    global as agent_run_manager_global, init_global as agent_run_manager_init_global,
    AgentRunManager, BackgroundFlowRunner, CallerRun, ChildFinishedEvent, FlowDispatcherRunner,
    RunStatus, MAX_CONCURRENT_RUNS_SETTING,
};
pub use service::AgentService;
pub use subagent_reactor::{
    init_global as subagent_reactor_init_global, FlowDispatcherReactorDispatch,
    ReactorFlowDispatch, SubagentReactor,
};

/// Late-bound `AgentService` slot (§3.5.0), mirroring `ModelRuntimeSlot`.
/// `build_registry()` is argument-free and runs before `AddonManager` exists,
/// so the agent_context / tool_exec adapters hold a clone of this empty slot;
/// `main.rs` fills it once via `FlowDispatcher::set_agent_service`. An empty
/// slot at `execute` time is a node error (unreachable after process start —
/// slots are filled before traffic).
pub type AgentServiceSlot = Arc<parking_lot::RwLock<Option<Arc<AgentService>>>>;
