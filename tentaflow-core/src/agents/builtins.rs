// ===== File: agents/builtins.rs — core.* builtin tools executed in Core (not
// WASM). The `core` addon id is reserved (addon install rejects it), so the
// tool_exec block dispatches `core.*` here BEFORE the ToolDispatcher.
// `skill_view` is synchronous (DB read) and runs through `execute_core_tool`.
// The background-run builtins (`agent_spawn`/`agent_wait`/`agent_list`/
// `agent_cancel`) are ASYNC and need the AgentRunManager (run registry +
// semaphore + flow dispatcher) — they own only their LLM-facing specs here and
// execute through `AgentRunManager` (Harness §3.6), never `execute_core_tool`.
// =====

use anyhow::Result;

use crate::db::{repository, DbPool};
use crate::flow_engine::dispatchers::LlmToolSpec;

/// Reserved addon id for Core builtins. Addon installation rejects this id so a
/// malicious addon can never shadow a `core.*` tool.
pub const CORE_ADDON_ID: &str = "core";

/// The core builtin tools recognised by the dispatch table. Kept as an enum so
/// the tool_exec block and the catalog agree on the exact set — an unknown
/// `core.*` name is an error, never a silent passthrough to the ToolDispatcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreToolName {
    /// `core.skill_view(name, file_path?)` — loads a skill's SKILL.md (or a
    /// reference file under it) into the turn and bumps the skill's use_count.
    SkillView,
    /// `core.agent_spawn` — delegate one task (or a batch) to a sub-agent. Runs
    /// in the background; returns the run ids immediately (§3.6).
    AgentSpawn,
    /// `core.agent_wait` — block until named child runs settle, returning each
    /// child's status + result. Releases the parent's concurrency permit while
    /// waiting (anti-livelock, §3.6).
    AgentWait,
    /// `core.agent_list` — list the active child runs of the current run.
    AgentList,
    /// `core.agent_cancel` — cancel one child run.
    AgentCancel,
    /// `core.ask_user(question, choices?, timeout_secs?)` — ask the operator a
    /// clarification and block (in `waiting_user`) until they answer or the
    /// timeout elapses (§3.13). Async: it awaits a human reply, so it routes
    /// through the interaction registry, never the synchronous core path. Not in
    /// sub-agent allowlists by default — only top-level agents ask directly.
    AskUser,
    /// `core.project_search(project_id, query, top_k?, source_ids?)` — semantic
    /// search over a Project Studio knowledge base. Async (query embedding goes
    /// through the platform executor); membership of the run's user principal
    /// is enforced per call.
    ProjectSearch,
    /// `core.project_list_sources(project_id)` — the knowledge-source catalog
    /// of a project. Async path (same membership gate as project_search).
    ProjectListSources,
    /// `core.project_case_save(...)` — saves ONE generated manual test case
    /// into a Project Studio generation. The target project/generation is NOT
    /// a parameter: it binds server-side via `envelope.meta["ps_generation"]`
    /// minted at spawn, so the model can never redirect output to another
    /// project. Async path (per-call membership + editor re-check).
    CaseSave,
}

impl CoreToolName {
    /// Public tool name as advertised to the model (`core.<tool>`).
    pub fn public_name(self) -> &'static str {
        match self {
            CoreToolName::SkillView => "core.skill_view",
            CoreToolName::AgentSpawn => "core.agent_spawn",
            CoreToolName::AgentWait => "core.agent_wait",
            CoreToolName::AgentList => "core.agent_list",
            CoreToolName::AgentCancel => "core.agent_cancel",
            CoreToolName::AskUser => "core.ask_user",
            CoreToolName::ProjectSearch => "core.project_search",
            CoreToolName::ProjectListSources => "core.project_list_sources",
            CoreToolName::CaseSave => "core.project_case_save",
        }
    }

    /// Bare tool name (the part after the `core.` prefix).
    pub fn bare_name(self) -> &'static str {
        match self {
            CoreToolName::SkillView => "skill_view",
            CoreToolName::AgentSpawn => "agent_spawn",
            CoreToolName::AgentWait => "agent_wait",
            CoreToolName::AgentList => "agent_list",
            CoreToolName::AgentCancel => "agent_cancel",
            CoreToolName::AskUser => "ask_user",
            CoreToolName::ProjectSearch => "project_search",
            CoreToolName::ProjectListSources => "project_list_sources",
            CoreToolName::CaseSave => "project_case_save",
        }
    }

    /// Resolves a public `core.<tool>` name to the enum, or `None` when the
    /// name is not a known core builtin.
    pub fn from_public_name(name: &str) -> Option<Self> {
        match name.strip_prefix("core.")? {
            "skill_view" => Some(CoreToolName::SkillView),
            "agent_spawn" => Some(CoreToolName::AgentSpawn),
            "agent_wait" => Some(CoreToolName::AgentWait),
            "agent_list" => Some(CoreToolName::AgentList),
            "agent_cancel" => Some(CoreToolName::AgentCancel),
            "ask_user" => Some(CoreToolName::AskUser),
            "project_search" => Some(CoreToolName::ProjectSearch),
            "project_list_sources" => Some(CoreToolName::ProjectListSources),
            "project_case_save" => Some(CoreToolName::CaseSave),
            _ => None,
        }
    }

    /// True for builtins handled synchronously by `execute_core_tool` (DB-only).
    /// The agent_* builtins and ask_user are async and return false.
    pub fn is_synchronous(self) -> bool {
        matches!(self, CoreToolName::SkillView)
    }

    /// True for `core.ask_user` — the async builtin that blocks the run on a
    /// human reply via the interaction registry (§3.13). Routed on the async
    /// path in tool_exec, like the sub-agent control builtins, but distinct: it
    /// is offered to top-level agents by default, not gated on `max_subagents`.
    pub fn is_ask_user(self) -> bool {
        matches!(self, CoreToolName::AskUser)
    }

    /// True for the Project Studio knowledge builtins — async in tool_exec
    /// (query embedding via the flow context's dispatcher), never routed
    /// through `execute_core_tool`.
    pub fn is_project_knowledge(self) -> bool {
        matches!(
            self,
            CoreToolName::ProjectSearch | CoreToolName::ProjectListSources
        )
    }

    /// True for `core.project_case_save` — the generation sink routed through
    /// its own async arm in tool_exec (needs the envelope's server-minted
    /// `ps_generation` binding, which the other paths never read).
    pub fn is_case_save(self) -> bool {
        matches!(self, CoreToolName::CaseSave)
    }

    /// True for the sub-agent control builtins, which are admitted to the tool
    /// catalog only when the running agent may spawn (`max_subagents > 0`, §3.6).
    pub fn is_subagent_control(self) -> bool {
        matches!(
            self,
            CoreToolName::AgentSpawn
                | CoreToolName::AgentWait
                | CoreToolName::AgentList
                | CoreToolName::AgentCancel
        )
    }

    /// All core builtins, in catalog order.
    pub fn all() -> &'static [CoreToolName] {
        &[
            CoreToolName::SkillView,
            CoreToolName::AgentSpawn,
            CoreToolName::AgentWait,
            CoreToolName::AgentList,
            CoreToolName::AgentCancel,
            CoreToolName::AskUser,
            CoreToolName::ProjectSearch,
            CoreToolName::ProjectListSources,
            CoreToolName::CaseSave,
        ]
    }

    /// LLM-facing spec (name, description, JSON Schema params) for this builtin.
    pub fn spec(self) -> LlmToolSpec {
        match self {
            CoreToolName::SkillView => LlmToolSpec {
                name: self.public_name().to_string(),
                description: "Load a skill's full instructions into the conversation. \
                              Call with the skill name to read its SKILL.md; pass an \
                              optional file_path (e.g. references/api.md) to read one of \
                              its reference files."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Skill name (kebab-case) to load."
                        },
                        "file_path": {
                            "type": "string",
                            "description": "Optional reference file under the skill \
                                            (references/ or templates/) to read instead \
                                            of the main SKILL.md."
                        }
                    },
                    "required": ["name"]
                }),
            },
            CoreToolName::AgentSpawn => LlmToolSpec {
                name: self.public_name().to_string(),
                description: "Delegate a task to a sub-agent that runs in the background. \
                              Pass a single (agent_name, task) or a batch of tasks. Returns \
                              the run ids immediately; collect results later with \
                              core.agent_wait."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "agent_name": {
                            "type": "string",
                            "description": "Name of the sub-agent to run a single task."
                        },
                        "task": {
                            "type": "string",
                            "description": "The task prompt for a single sub-agent."
                        },
                        "context": {
                            "type": "string",
                            "description": "Optional extra context prepended to the task."
                        },
                        "tasks": {
                            "type": "array",
                            "description": "Batch form: one entry per sub-agent task.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "agent_name": {"type": "string"},
                                    "task": {"type": "string"},
                                    "context": {"type": "string"}
                                },
                                "required": ["agent_name", "task"]
                            }
                        }
                    }
                }),
            },
            CoreToolName::AgentWait => LlmToolSpec {
                name: self.public_name().to_string(),
                description: "Wait for one or more sub-agent runs to finish and return each \
                              run's status and result. Blocks until every named run settles \
                              or the timeout elapses."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "run_ids": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "Run ids returned by core.agent_spawn."
                        },
                        "timeout_secs": {
                            "type": "integer",
                            "description": "Max seconds to wait before returning the current \
                                            (possibly still-running) statuses."
                        }
                    },
                    "required": ["run_ids"]
                }),
            },
            CoreToolName::AgentList => LlmToolSpec {
                name: self.public_name().to_string(),
                description: "List the active sub-agent runs spawned by the current run \
                              (run id, agent, status)."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {}
                }),
            },
            CoreToolName::AgentCancel => LlmToolSpec {
                name: self.public_name().to_string(),
                description: "Cancel one sub-agent run by its run id.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "run_id": {
                            "type": "string",
                            "description": "Run id of the child to cancel."
                        }
                    },
                    "required": ["run_id"]
                }),
            },
            CoreToolName::AskUser => LlmToolSpec {
                name: self.public_name().to_string(),
                description: "Ask the operator a clarifying question and wait for their reply. \
                              Use it for genuine ambiguity or missing information — NOT to confirm \
                              dangerous actions (those go through the permission flow). Offer up \
                              to 4 choices, or ask an open question. If the user does not respond \
                              within the timeout you receive a sentinel and should adapt rather \
                              than re-ask."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "question": {
                            "type": "string",
                            "description": "The question to put to the operator."
                        },
                        "choices": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "Up to 4 options to offer; omit for an open question."
                        },
                        "timeout_secs": {
                            "type": "integer",
                            "description": "Max seconds to wait for an answer (default 600)."
                        }
                    },
                    "required": ["question"]
                }),
            },
            CoreToolName::ProjectSearch => LlmToolSpec {
                name: self.public_name().to_string(),
                description: "Search the knowledge base of a Project Studio project the \
                              current user is a member of. Returns the best-matching \
                              passages with source name, file path, chunk index, score \
                              and a text snippet."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "project_id": {
                            "type": "string",
                            "description": "Id of the project whose knowledge base to search."
                        },
                        "query": {
                            "type": "string",
                            "description": "The natural-language search query."
                        },
                        "top_k": {
                            "type": "integer",
                            "description": "Max passages to return (1-50, default 8)."
                        },
                        "source_ids": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "Optional: restrict the search to these source ids."
                        }
                    },
                    "required": ["project_id", "query"]
                }),
            },
            CoreToolName::ProjectListSources => LlmToolSpec {
                name: self.public_name().to_string(),
                description: "List the knowledge sources of a Project Studio project the \
                              current user is a member of (id, name, kind, status, file \
                              and chunk counts)."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "project_id": {
                            "type": "string",
                            "description": "Id of the project whose sources to list."
                        }
                    },
                    "required": ["project_id"]
                }),
            },
            CoreToolName::CaseSave => LlmToolSpec {
                name: self.public_name().to_string(),
                description: "Save ONE generated manual test case into the current \
                              generation. Call it IMMEDIATELY after designing each case — \
                              only saved cases count. The target project and generation \
                              are bound server-side; do not pass any ids. A [TOOL_ERROR] \
                              rejects only THIS case: fix it per the message and retry."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "title": {
                            "type": "string",
                            "description": "Concise case title (1..200 characters)."
                        },
                        "priority": {
                            "type": "string",
                            "enum": ["low", "medium", "high", "critical"],
                            "description": "Case priority."
                        },
                        "preconditions": {
                            "type": "string",
                            "description": "State required before executing the steps."
                        },
                        "steps": {
                            "type": "array",
                            "description": "1..50 ordered steps.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "action": {"type": "string"},
                                    "expected": {"type": "string"}
                                },
                                "required": ["action", "expected"]
                            }
                        },
                        "test_data": {
                            "type": "string",
                            "description": "Input data the tester needs."
                        },
                        "tags": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "Up to 10 tags (created lazily)."
                        },
                        "source_refs": {
                            "type": "array",
                            "description": "Passages the case is grounded in (source ids \
                                            from this generation + short verbatim quotes).",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "source_id": {"type": "string"},
                                    "quote": {"type": "string"}
                                },
                                "required": ["source_id"]
                            }
                        }
                    },
                    "required": ["title", "priority", "steps"]
                }),
            },
        }
    }
}

/// Returns true for a public name owned by the core builtin dispatch table.
pub fn is_core_tool(name: &str) -> bool {
    CoreToolName::from_public_name(name).is_some()
}

/// Errors a core builtin can return. Surfaced to the model as a `[TOOL_ERROR]`
/// result by the tool_exec block — never an aborted run.
#[derive(Debug, thiserror::Error)]
pub enum BuiltinToolError {
    #[error("unknown core tool '{0}'")]
    Unknown(String),
    #[error("missing required argument '{0}'")]
    MissingArg(&'static str),
    #[error("skill '{0}' not found")]
    SkillNotFound(String),
    #[error("skill '{skill}' has no reference file '{path}'")]
    SkillFileNotFound { skill: String, path: String },
    /// An async sub-agent builtin was routed to the synchronous path. Indicates
    /// a tool_exec wiring bug, not model input — the model never sees it.
    #[error("core tool '{0}' is async; dispatch it through AgentRunManager")]
    NotSynchronous(&'static str),
}

/// Executes one `core.*` builtin call. Synchronous (DB read + bump) — the
/// caller (tool_exec block) runs it on a blocking thread alongside addon
/// dispatch. Returns the JSON value handed back to the model as the tool result.
pub fn execute_core_tool(
    db: &DbPool,
    name: &str,
    arguments: &serde_json::Value,
) -> Result<serde_json::Value> {
    let tool = CoreToolName::from_public_name(name)
        .ok_or_else(|| BuiltinToolError::Unknown(name.to_string()))?;
    match tool {
        CoreToolName::SkillView => skill_view(db, arguments),
        // Sub-agent control is async (AgentRunManager). Reaching here means
        // tool_exec failed to route it to the manager — a wiring bug.
        other => Err(BuiltinToolError::NotSynchronous(other.public_name()).into()),
    }
}

/// `core.skill_view` — loads a skill (or one of its reference files) and bumps
/// `use_count`. The bump is the curator's usage signal (§3.2); it is a
/// node-local stat, never synced. Loading a missing skill is a tool error the
/// model can recover from, not a run failure.
fn skill_view(db: &DbPool, arguments: &serde_json::Value) -> Result<serde_json::Value> {
    let skill_name = arguments
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or(BuiltinToolError::MissingArg("name"))?;

    let skill = repository::get_skill_by_name(db, skill_name)?
        .ok_or_else(|| BuiltinToolError::SkillNotFound(skill_name.to_string()))?;

    // The bump is non-fatal: a successful load must still return content even
    // if the stat write loses a race with a concurrent skill delete.
    let _ = repository::bump_skill_use(db, &skill.id);

    let file_path = arguments
        .get("file_path")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());

    if let Some(path) = file_path {
        let files = repository::list_skill_files(db, &skill.id)?;
        let file = files.into_iter().find(|f| f.path == path).ok_or_else(|| {
            BuiltinToolError::SkillFileNotFound {
                skill: skill_name.to_string(),
                path: path.to_string(),
            }
        })?;
        return Ok(serde_json::json!({
            "skill": skill.name,
            "file_path": file.path,
            "content": file.content,
        }));
    }

    Ok(serde_json::json!({
        "skill": skill.name,
        "display_name": skill.display_name,
        "description": skill.description,
        "content": skill.content,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_tool_name_round_trips() {
        assert_eq!(
            CoreToolName::from_public_name("core.skill_view"),
            Some(CoreToolName::SkillView)
        );
        assert_eq!(CoreToolName::SkillView.public_name(), "core.skill_view");
        assert_eq!(CoreToolName::SkillView.bare_name(), "skill_view");
        assert_eq!(
            CoreToolName::from_public_name("core.agent_spawn"),
            Some(CoreToolName::AgentSpawn)
        );
        assert_eq!(
            CoreToolName::from_public_name("core.agent_wait"),
            Some(CoreToolName::AgentWait)
        );
        assert!(CoreToolName::from_public_name("core.unknown").is_none());
        assert!(CoreToolName::from_public_name("memory.memory_store").is_none());
    }

    #[test]
    fn is_core_tool_only_for_known_builtins() {
        assert!(is_core_tool("core.skill_view"));
        assert!(is_core_tool("core.agent_spawn"));
        assert!(is_core_tool("core.agent_cancel"));
        assert!(!is_core_tool("core.unknown"));
        assert!(!is_core_tool("memory.memory_store"));
    }

    #[test]
    fn subagent_control_builtins_are_async() {
        assert!(CoreToolName::SkillView.is_synchronous());
        for tool in [
            CoreToolName::AgentSpawn,
            CoreToolName::AgentWait,
            CoreToolName::AgentList,
            CoreToolName::AgentCancel,
        ] {
            assert!(!tool.is_synchronous());
            assert!(tool.is_subagent_control());
        }
        assert!(!CoreToolName::SkillView.is_subagent_control());
        // The async builtins must refuse the synchronous path.
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        crate::db::migrations::run(&conn).expect("migrations");
        let pool: DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));
        let err = execute_core_tool(&pool, "core.agent_spawn", &serde_json::json!({})).unwrap_err();
        assert!(err.to_string().contains("async"), "{err}");
    }

    #[test]
    fn skill_view_spec_is_valid_schema() {
        let spec = CoreToolName::SkillView.spec();
        assert_eq!(spec.name, "core.skill_view");
        assert_eq!(spec.parameters["type"], "object");
        assert_eq!(spec.parameters["required"][0], "name");
    }
}
