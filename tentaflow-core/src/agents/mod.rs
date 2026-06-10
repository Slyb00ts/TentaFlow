// ===== File: agents/mod.rs — Agent harness service: registry access, tool
// catalog resolution (addon tools ∩ allowlist ∩ permissions + core.* builtins)
// and core builtin tool handlers (Harness §3.3, §3.4, §3.5). Thin by design —
// the agent loop is a Flow Builder flow, not Rust code. =====

mod builtins;
mod catalog;
mod principal;
mod service;

use std::sync::Arc;

pub use builtins::{is_core_tool, BuiltinToolError, CoreToolName};
pub use catalog::{tool_in_allowlist, AllowlistEntry, ToolCatalog};
pub use principal::AgentPrincipal;
pub use service::AgentService;

/// Late-bound `AgentService` slot (§3.5.0), mirroring `ModelRuntimeSlot`.
/// `build_registry()` is argument-free and runs before `AddonManager` exists,
/// so the agent_context / tool_exec adapters hold a clone of this empty slot;
/// `main.rs` fills it once via `FlowDispatcher::set_agent_service`. An empty
/// slot at `execute` time is a node error (unreachable after process start —
/// slots are filled before traffic).
pub type AgentServiceSlot = Arc<parking_lot::RwLock<Option<Arc<AgentService>>>>;
