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

    // --- Code Studio (§10). The complete verb set of a coding agent: every
    // one of these binds its session server-side through
    // `envelope.meta["code_session"]` (never a model argument), passes
    // `code_studio::pep::authorize` and journals a `session_operations` row.
    /// `core.fs_read` — read one file of the session worktree.
    FsRead,
    /// `core.fs_list` — list one directory of the session worktree.
    FsList,
    /// `core.fs_glob` — match worktree paths against a glob pattern.
    FsGlob,
    /// `core.fs_grep` — regex search over worktree file contents.
    FsGrep,
    /// `core.fs_write` — write a whole file (CAS precondition).
    FsWrite,
    /// `core.fs_edit` — replace one unambiguous literal occurrence in a file.
    FsEdit,
    /// `core.fs_move` — rename/move a worktree path (CAS precondition).
    FsMove,
    /// `core.fs_delete` — delete a worktree path (CAS precondition).
    FsDelete,
    /// `core.fs_mkdir` — create a directory inside the worktree.
    FsMkdir,
    /// `core.exec` — run a command in the session sandbox.
    Exec,
    /// `core.git_read` — read-only git through the broker.
    GitRead,
    /// `core.git_branch` — branch inspection/creation within the session branch.
    GitBranch,
    /// `core.git_sync` — `fetch` / `pull` through the broker.
    GitSync,
    /// `core.git_stage` — stage worktree paths through the broker.
    GitStage,
    /// `core.git_commit` — commit from ACCEPTED blobs (PEP gate 5a).
    GitCommit,
    /// `core.git_push` — push the session branch (`mandatory_interactive`).
    GitPush,
    /// `core.git_merge` — merge into a detached integration worktree.
    GitMerge,
    /// `core.git_merge_finalize` — commit the merge result and move the ref.
    GitMergeFinalize,
    /// `core.code_search` — semantic search over the workspace index (§14).
    /// A shortcut, never the source of truth: grep stays authoritative, and a
    /// `degraded` answer means the index does not describe the current head.
    CodeSearch,
    /// `core.workspace_info` — the session's workspace facts, no host paths.
    WorkspaceInfo,
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
            CoreToolName::FsRead => "core.fs_read",
            CoreToolName::FsList => "core.fs_list",
            CoreToolName::FsGlob => "core.fs_glob",
            CoreToolName::FsGrep => "core.fs_grep",
            CoreToolName::FsWrite => "core.fs_write",
            CoreToolName::FsEdit => "core.fs_edit",
            CoreToolName::FsMove => "core.fs_move",
            CoreToolName::FsDelete => "core.fs_delete",
            CoreToolName::FsMkdir => "core.fs_mkdir",
            CoreToolName::Exec => "core.exec",
            CoreToolName::GitRead => "core.git_read",
            CoreToolName::GitBranch => "core.git_branch",
            CoreToolName::GitSync => "core.git_sync",
            CoreToolName::GitStage => "core.git_stage",
            CoreToolName::GitCommit => "core.git_commit",
            CoreToolName::GitPush => "core.git_push",
            CoreToolName::GitMerge => "core.git_merge",
            CoreToolName::GitMergeFinalize => "core.git_merge_finalize",
            CoreToolName::CodeSearch => "core.code_search",
            CoreToolName::WorkspaceInfo => "core.workspace_info",
        }
    }

    /// Bare tool name (the part after the `core.` prefix). Derived from
    /// `public_name` so the two can never drift apart.
    pub fn bare_name(self) -> &'static str {
        self.public_name()
            .strip_prefix("core.")
            .expect("every public_name carries the core. prefix")
    }

    /// Resolves a public `core.<tool>` name to the enum, or `None` when the
    /// name is not a known core builtin. Resolved against `all()`, so a variant
    /// missing from the catalog list is unreachable by name rather than
    /// silently dispatchable.
    pub fn from_public_name(name: &str) -> Option<Self> {
        if !name.starts_with("core.") {
            return None;
        }
        Self::all()
            .iter()
            .copied()
            .find(|c| c.public_name() == name)
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

    /// True for the Code Studio tool set (§10) — the coding agent's complete
    /// verb list. Async in tool_exec: each call binds its session from
    /// `envelope.meta["code_session"]`, runs the policy enforcement point and
    /// journals an operation, none of which the synchronous core path can do.
    pub fn is_code_studio(self) -> bool {
        matches!(
            self,
            CoreToolName::FsRead
                | CoreToolName::FsList
                | CoreToolName::FsGlob
                | CoreToolName::FsGrep
                | CoreToolName::FsWrite
                | CoreToolName::FsEdit
                | CoreToolName::FsMove
                | CoreToolName::FsDelete
                | CoreToolName::FsMkdir
                | CoreToolName::Exec
                | CoreToolName::GitRead
                | CoreToolName::GitBranch
                | CoreToolName::GitSync
                | CoreToolName::GitStage
                | CoreToolName::GitCommit
                | CoreToolName::GitPush
                | CoreToolName::GitMerge
                | CoreToolName::GitMergeFinalize
                | CoreToolName::CodeSearch
                | CoreToolName::WorkspaceInfo
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
            CoreToolName::FsRead,
            CoreToolName::FsList,
            CoreToolName::FsGlob,
            CoreToolName::FsGrep,
            CoreToolName::FsWrite,
            CoreToolName::FsEdit,
            CoreToolName::FsMove,
            CoreToolName::FsDelete,
            CoreToolName::FsMkdir,
            CoreToolName::Exec,
            CoreToolName::GitRead,
            CoreToolName::GitBranch,
            CoreToolName::GitSync,
            CoreToolName::GitStage,
            CoreToolName::GitCommit,
            CoreToolName::GitPush,
            CoreToolName::GitMerge,
            CoreToolName::GitMergeFinalize,
            CoreToolName::CodeSearch,
            CoreToolName::WorkspaceInfo,
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
                description: "Save ONE generated test case into the current generation. Call \
                              it IMMEDIATELY after designing each case — only saved cases \
                              count. The target project, generation and case KIND are bound \
                              server-side; do not pass any ids or a kind. Manual generations \
                              expect `steps`; code generations (ui/api/unit/perf/security) \
                              expect `script` plus the extras of their kind — the other \
                              fields are ignored. A [TOOL_ERROR] rejects only THIS case: fix \
                              it per the message and retry."
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
                            "description": "Manual kind: state required before the steps."
                        },
                        "steps": {
                            "type": "array",
                            "description": "Manual kind: 1..50 ordered steps.",
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
                            "description": "Manual kind: input data the tester needs."
                        },
                        "script": {
                            "type": "string",
                            "description": "Code kinds: the runnable script (max 64000 \
                                            characters) honouring the execution contract of \
                                            the kind stated in your task."
                        },
                        "language": {
                            "type": "string",
                            "description": "Code kinds: programming language of the script \
                                            (default and currently the only supported value: \
                                            python)."
                        },
                        "config": {
                            "type": "object",
                            "description": "Kind ui: {viewport:{width,height}, timeout_ms, \
                                            headed}. Kind api: {timeout_ms}.",
                            "properties": {
                                "timeout_ms": {"type": "integer"},
                                "headed": {"type": "boolean"},
                                "viewport": {
                                    "type": "object",
                                    "properties": {
                                        "width": {"type": "integer"},
                                        "height": {"type": "integer"}
                                    }
                                }
                            }
                        },
                        "profile": {
                            "type": "object",
                            "description": "Kind perf: load profile of the Locust run.",
                            "properties": {
                                "users": {"type": "integer"},
                                "spawn_rate": {"type": "number"},
                                "duration_secs": {"type": "integer"}
                            }
                        },
                        "checklist": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "Kind security: up to 50 short statements of what \
                                            the script verifies."
                        },
                        "build_profile_ref": {
                            "type": "string",
                            "description": "Kind unit: id of the code source whose build \
                                            profile this case runs against."
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
                    "required": ["title", "priority"]
                }),
            },

            // --- Code Studio (§10) ---
            // Every description below is written FOR THE MODEL: what the verb
            // does, what it refuses, and what it costs. None of them takes a
            // workspace or session argument — the session is bound server-side.
            CoreToolName::FsRead => LlmToolSpec {
                name: self.public_name().to_string(),
                description: "Read a text file from the repository you are working in. \
                              Paths are relative to the repository root (e.g. src/main.rs); \
                              absolute paths, `..` and symlinks leaving the tree are refused. \
                              Use offset/limit to page through a large file instead of \
                              re-reading it whole."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Repository-relative path of the file to read."
                        },
                        "offset": {
                            "type": "integer",
                            "description": "First line to return (1-based). Omit to start at \
                                            the beginning."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of lines to return."
                        }
                    },
                    "required": ["path"]
                }),
            },
            CoreToolName::FsList => LlmToolSpec {
                name: self.public_name().to_string(),
                description: "List the entries of one directory in the repository. Returns \
                              name, kind (file/dir/symlink) and size. It does NOT recurse — \
                              use core.fs_glob to match paths across the tree."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Repository-relative directory. Omit or pass \".\" \
                                            for the repository root."
                        }
                    }
                }),
            },
            CoreToolName::FsGlob => LlmToolSpec {
                name: self.public_name().to_string(),
                description: "Find files by path pattern (e.g. `src/**/*.rs`, `**/Cargo.toml`). \
                              Returns matching repository-relative paths, newest first. Use it \
                              to locate files by name; use core.fs_grep to search their contents."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "pattern": {
                            "type": "string",
                            "description": "Glob pattern matched against repository-relative \
                                            paths."
                        },
                        "path": {
                            "type": "string",
                            "description": "Optional subdirectory to search under."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of paths to return."
                        }
                    },
                    "required": ["pattern"]
                }),
            },
            CoreToolName::FsGrep => LlmToolSpec {
                name: self.public_name().to_string(),
                description: "Search file CONTENTS with a regular expression. This is the \
                              AUTHORITATIVE way to find code in this repository: it reads the \
                              files as they are right now, which core.code_search does not. \
                              Narrow the search with `path` and `glob` before raising `limit`; \
                              a pattern that takes too long is aborted rather than truncated \
                              silently."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "pattern": {
                            "type": "string",
                            "description": "Regular expression to match against file contents."
                        },
                        "path": {
                            "type": "string",
                            "description": "Optional subdirectory to restrict the search to."
                        },
                        "glob": {
                            "type": "string",
                            "description": "Optional path glob filter, e.g. `*.rs`."
                        },
                        "case_insensitive": {
                            "type": "boolean",
                            "description": "Match without regard to case (default false)."
                        },
                        "context_lines": {
                            "type": "integer",
                            "description": "Lines of context to show around each match (0-10)."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of matches to return."
                        }
                    },
                    "required": ["pattern"]
                }),
            },
            CoreToolName::FsWrite => LlmToolSpec {
                name: self.public_name().to_string(),
                description: "Write a file in the repository, creating it or replacing its \
                              whole content. To replace an EXISTING file you must first read it \
                              and pass its `expected_sha256` — a mismatch means someone else \
                              changed the file and the write is refused instead of overwriting \
                              their work. For a small change prefer core.fs_edit."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Repository-relative path of the file to write."
                        },
                        "content": {
                            "type": "string",
                            "description": "The complete new content of the file."
                        },
                        "expected_sha256": {
                            "type": "string",
                            "description": "SHA-256 of the content you read, for an existing \
                                            file. Pass \"\" (empty) to assert the file does not \
                                            exist yet."
                        }
                    },
                    "required": ["path", "content"]
                }),
            },
            CoreToolName::FsEdit => LlmToolSpec {
                name: self.public_name().to_string(),
                description: "Replace one exact literal occurrence in a file. `old_string` must \
                              appear EXACTLY ONCE — if it matches zero or several times the \
                              edit is refused, so include enough surrounding lines to make it \
                              unique. Pass `expected_sha256` from your read of the file so a \
                              concurrent change cannot be clobbered."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Repository-relative path of the file to edit."
                        },
                        "old_string": {
                            "type": "string",
                            "description": "Exact text to replace, unique within the file."
                        },
                        "new_string": {
                            "type": "string",
                            "description": "Replacement text."
                        },
                        "expected_sha256": {
                            "type": "string",
                            "description": "SHA-256 of the file content you read."
                        }
                    },
                    "required": ["path", "old_string", "new_string"]
                }),
            },
            CoreToolName::FsMove => LlmToolSpec {
                name: self.public_name().to_string(),
                description: "Rename or move a file inside the repository. Both paths stay \
                              inside the tree. Pass `expected_sha256` of the source so the move \
                              is refused if the file changed since you read it."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "from": {
                            "type": "string",
                            "description": "Current repository-relative path."
                        },
                        "to": {
                            "type": "string",
                            "description": "New repository-relative path."
                        },
                        "expected_sha256": {
                            "type": "string",
                            "description": "SHA-256 of the source file content you read."
                        }
                    },
                    "required": ["from", "to"]
                }),
            },
            CoreToolName::FsDelete => LlmToolSpec {
                name: self.public_name().to_string(),
                description: "Delete a file from the repository. Pass `expected_sha256` of the \
                              content you read — deleting a file that changed meanwhile is \
                              refused. Deleting a directory requires `recursive: true`."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Repository-relative path to delete."
                        },
                        "expected_sha256": {
                            "type": "string",
                            "description": "SHA-256 of the file content you read."
                        },
                        "recursive": {
                            "type": "boolean",
                            "description": "Required to delete a non-empty directory."
                        }
                    },
                    "required": ["path"]
                }),
            },
            CoreToolName::FsMkdir => LlmToolSpec {
                name: self.public_name().to_string(),
                description: "Create a directory (and missing parents) inside the repository. \
                              Creating a directory that already exists succeeds without change."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Repository-relative directory to create."
                        }
                    },
                    "required": ["path"]
                }),
            },
            CoreToolName::Exec => LlmToolSpec {
                name: self.public_name().to_string(),
                description: "Run a command in the session sandbox — this is how you build, \
                              run tests and use project tooling. Pass the command as an argv \
                              ARRAY (e.g. [\"cargo\",\"test\",\"--lib\"]); there is no shell, so \
                              pipes, redirections and `&&` do not work. Returns exit code, \
                              stdout and stderr (truncated). Network access and write access \
                              are decided by policy, not by this call."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "argv": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "Program and arguments; the first entry is the \
                                            executable."
                        },
                        "cwd": {
                            "type": "string",
                            "description": "Repository-relative working directory (default: \
                                            repository root)."
                        },
                        "timeout_secs": {
                            "type": "integer",
                            "description": "Kill the command after this many seconds."
                        },
                        "purpose": {
                            "type": "string",
                            "description": "One short sentence on why you are running this — \
                                            shown to the operator when the command needs \
                                            approval."
                        }
                    },
                    "required": ["argv"]
                }),
            },
            CoreToolName::GitRead => LlmToolSpec {
                name: self.public_name().to_string(),
                description: "Read git state through the repository broker: `status`, `diff`, \
                              `log`, `show`, `ls_files`. Read-only — it never changes the \
                              repository. Use `diff` to see what you have changed so far."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "operation": {
                            "type": "string",
                            "enum": ["status", "diff", "log", "show", "ls_files"],
                            "description": "Which read to perform."
                        },
                        "path": {
                            "type": "string",
                            "description": "Optional repository-relative path to restrict the \
                                            read to."
                        },
                        "rev": {
                            "type": "string",
                            "description": "Revision for `show`/`log`, or the base of a `diff`."
                        },
                        "staged": {
                            "type": "boolean",
                            "description": "`diff`: compare the staged content instead of the \
                                            working tree."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "`log`/`ls_files`: maximum entries to return."
                        }
                    },
                    "required": ["operation"]
                }),
            },
            CoreToolName::GitBranch => LlmToolSpec {
                name: self.public_name().to_string(),
                description: "List the repository's branches with their upstream and how far \
                              ahead or behind each one is, and say which branch this session \
                              owns. You cannot leave that branch, and creating a branch is not \
                              offered — a session gets its branch when it opens."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {}
                }),
            },
            CoreToolName::GitSync => LlmToolSpec {
                name: self.public_name().to_string(),
                description: "Bring remote history in: `fetch` (download only) or `pull` \
                              (fetch and integrate into the session branch). Runs in the \
                              broker with the workspace's own credentials; you never see them."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "operation": {
                            "type": "string",
                            "enum": ["fetch", "pull"],
                            "description": "Which sync operation to perform."
                        },
                        "remote": {
                            "type": "string",
                            "description": "Remote name (default `origin`)."
                        }
                    },
                    "required": ["operation"]
                }),
            },
            CoreToolName::GitStage => LlmToolSpec {
                name: self.public_name().to_string(),
                description: "Prepare the change set for review and commit: it closes what you \
                              have changed so far into one reviewable set and reports which of \
                              the paths you asked about it captured. Call it with no paths to \
                              see everything that changed. Removing a changed file from the set \
                              is the reviewer's decision, not yours."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "paths": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "Optional repository-relative paths to report on; \
                                            omit for every changed path."
                        }
                    }
                }),
            },
            CoreToolName::GitCommit => LlmToolSpec {
                name: self.public_name().to_string(),
                description: "Commit the reviewed changes. If the operator has not accepted a \
                              patch set yet, this call does NOT fail — it opens the change \
                              review and waits for their decision, then commits. The commit is \
                              always built from the ACCEPTED content, not from whatever is on \
                              disk at that moment, so editing files after the review does not \
                              change what gets committed."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "message": {
                            "type": "string",
                            "description": "Commit message: a concise subject line, and a body \
                                            explaining WHY when the change is not obvious."
                        }
                    },
                    "required": ["message"]
                }),
            },
            CoreToolName::GitPush => LlmToolSpec {
                name: self.public_name().to_string(),
                description: "Push this session's branch to the remote. The operator is asked \
                              EVERY time — there is no way to pre-approve it — so call it only \
                              when publishing the work is what the user asked for."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "remote": {
                            "type": "string",
                            "description": "Remote name (default `origin`)."
                        },
                        "force_with_lease": {
                            "type": "boolean",
                            "description": "Allow a non-fast-forward push that still refuses to \
                                            overwrite unseen remote commits."
                        }
                    }
                }),
            },
            CoreToolName::GitMerge => LlmToolSpec {
                name: self.public_name().to_string(),
                description: "Merge this session's branch into a target branch inside a \
                              DETACHED integration worktree. It never moves the target branch — \
                              it produces the merge result (or the conflicts) for you to test \
                              and review. The operator is asked every time. Finish with \
                              core.git_merge_finalize once the result is verified."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "target_branch": {
                            "type": "string",
                            "description": "Branch to merge into (e.g. main)."
                        }
                    },
                    "required": ["target_branch"]
                }),
            },
            CoreToolName::GitMergeFinalize => LlmToolSpec {
                name: self.public_name().to_string(),
                description: "Complete a merge started with core.git_merge: commit the accepted \
                              merge result and move the target branch to it. Requires an \
                              accepted review of the merge result and asks the operator every \
                              time; it aborts rather than overwrite if the target branch moved \
                              in the meantime."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "message": {
                            "type": "string",
                            "description": "Merge commit message."
                        }
                    }
                }),
            },
            CoreToolName::CodeSearch => LlmToolSpec {
                name: self.public_name().to_string(),
                description: "Find code by MEANING instead of by exact text: describe what you \
                              are looking for (\"where the retry budget is applied\") and get \
                              back the closest code chunks with their path and line range. It \
                              is a shortcut for locating unfamiliar code, NOT the source of \
                              truth — core.fs_grep is authoritative here, because it reads the \
                              files while this searches an index built earlier. When the answer \
                              comes back with `degraded: true` the index does not describe the \
                              repository's current state, so treat its hits as leads and verify \
                              them; a degraded answer with NO hits says nothing at all about \
                              the code — repeat the search with core.fs_grep before concluding \
                              that something does not exist."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "What you are looking for, in natural language or as \
                                            the code idea itself."
                        },
                        "prefix": {
                            "type": "string",
                            "description": "Optional repository-relative path prefix to restrict \
                                            results to, e.g. `src/`. Omit to search the whole \
                                            repository."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of code chunks to return."
                        }
                    },
                    "required": ["query"]
                }),
            },
            CoreToolName::WorkspaceInfo => LlmToolSpec {
                name: self.public_name().to_string(),
                description: "Describe the repository you are working in: name, current branch, \
                              base commit, whether the tree is dirty, the detected toolchain and \
                              the limits in force (autonomy mode, network access). Call it once \
                              at the start when you need your bearings; it never returns \
                              credentials or host paths."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {}
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
    fn code_studio_set_is_exactly_the_verbs_of_section_ten() {
        // The tool table of §10 is the WHOLE verb set of a coding agent. Both
        // directions matter: a missing verb leaves the agent unable to do its
        // job, and an extra one is a capability nobody reviewed.
        let mut actual: Vec<&str> = CoreToolName::all()
            .iter()
            .filter(|t| t.is_code_studio())
            .map(|t| t.public_name())
            .collect();
        actual.sort_unstable();
        let mut expected = vec![
            "core.code_search",
            "core.exec",
            "core.fs_delete",
            "core.fs_edit",
            "core.fs_glob",
            "core.fs_grep",
            "core.fs_list",
            "core.fs_mkdir",
            "core.fs_move",
            "core.fs_read",
            "core.fs_write",
            "core.git_branch",
            "core.git_commit",
            "core.git_merge",
            "core.git_merge_finalize",
            "core.git_push",
            "core.git_read",
            "core.git_stage",
            "core.git_sync",
            "core.workspace_info",
        ];
        expected.sort_unstable();
        assert_eq!(actual, expected);
    }

    #[test]
    fn code_search_offers_the_index_without_unseating_grep() {
        let tool = CoreToolName::from_public_name("core.code_search").expect("known builtin");
        assert_eq!(tool, CoreToolName::CodeSearch);
        assert!(tool.is_code_studio());
        let spec = tool.spec();
        assert_eq!(spec.name, "core.code_search");
        assert_eq!(spec.parameters["required"][0], "query");
        assert!(spec.parameters["properties"]["prefix"].is_object());
        assert!(spec.parameters["properties"]["limit"].is_object());
        // §14 is the whole point of the description: the model must know which
        // tool is authoritative and what a degraded answer obliges it to do.
        assert!(spec.description.contains("core.fs_grep"));
        assert!(spec.description.contains("authoritative"));
        assert!(spec.description.contains("degraded"));
        // And grep must no longer claim the index does not exist.
        assert!(!CoreToolName::FsGrep
            .spec()
            .description
            .contains("no semantic index"));
    }

    #[test]
    fn code_studio_tools_never_take_a_session_or_workspace_argument() {
        // The binding is server-minted (`envelope.meta["code_session"]`). A
        // parameter with either name would be exactly the redirection the
        // binding exists to prevent, so it must not appear in any schema.
        for tool in CoreToolName::all().iter().filter(|t| t.is_code_studio()) {
            let spec = tool.spec();
            let rendered = spec.parameters.to_string();
            for forbidden in ["workspace_id", "session_id", "worktree"] {
                assert!(
                    !rendered.contains(forbidden),
                    "{} exposes '{forbidden}'",
                    tool.public_name()
                );
            }
            assert_eq!(spec.parameters["type"], "object");
            assert!(!spec.description.is_empty());
        }
    }

    #[test]
    fn code_studio_tools_are_async_and_not_confused_with_the_other_families() {
        for tool in CoreToolName::all().iter().filter(|t| t.is_code_studio()) {
            assert!(!tool.is_synchronous(), "{}", tool.public_name());
            assert!(!tool.is_subagent_control(), "{}", tool.public_name());
            assert!(!tool.is_project_knowledge(), "{}", tool.public_name());
            assert!(!tool.is_case_save(), "{}", tool.public_name());
            assert!(!tool.is_ask_user(), "{}", tool.public_name());
        }
        // And the pre-existing families are not Code Studio.
        for other in [
            CoreToolName::SkillView,
            CoreToolName::AgentSpawn,
            CoreToolName::AskUser,
            CoreToolName::ProjectSearch,
            CoreToolName::CaseSave,
        ] {
            assert!(!other.is_code_studio(), "{}", other.public_name());
        }
    }

    #[test]
    fn every_catalog_entry_round_trips_through_its_public_name() {
        for tool in CoreToolName::all() {
            assert_eq!(
                CoreToolName::from_public_name(tool.public_name()),
                Some(*tool)
            );
            assert_eq!(
                tool.public_name(),
                format!("core.{}", tool.bare_name()),
                "bare name must be the public name without the prefix"
            );
        }
    }

    #[test]
    fn skill_view_spec_is_valid_schema() {
        let spec = CoreToolName::SkillView.spec();
        assert_eq!(spec.name, "core.skill_view");
        assert_eq!(spec.parameters["type"], "object");
        assert_eq!(spec.parameters["required"][0], "name");
    }
}
