// ===== File: agents/builtins.rs — core.* builtin tools executed in Core (not
// WASM). The `core` addon id is reserved (addon install rejects it), so the
// tool_exec block dispatches `core.*` here BEFORE the ToolDispatcher. Phase 3
// ships `core.skill_view` only; agent_spawn/wait/list/cancel land in phase 6
// and are deliberately absent (no stubs). (Harness §3.4, §3.5). =====

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
}

impl CoreToolName {
    /// Public tool name as advertised to the model (`core.<tool>`).
    pub fn public_name(self) -> &'static str {
        match self {
            CoreToolName::SkillView => "core.skill_view",
        }
    }

    /// Bare tool name (the part after the `core.` prefix).
    pub fn bare_name(self) -> &'static str {
        match self {
            CoreToolName::SkillView => "skill_view",
        }
    }

    /// Resolves a public `core.<tool>` name to the enum, or `None` when the
    /// name is not a known core builtin.
    pub fn from_public_name(name: &str) -> Option<Self> {
        match name.strip_prefix("core.")? {
            "skill_view" => Some(CoreToolName::SkillView),
            _ => None,
        }
    }

    /// All core builtins, in catalog order. Phase 3: just `skill_view`.
    pub fn all() -> &'static [CoreToolName] {
        &[CoreToolName::SkillView]
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
        let file = files
            .into_iter()
            .find(|f| f.path == path)
            .ok_or_else(|| BuiltinToolError::SkillFileNotFound {
                skill: skill_name.to_string(),
                path: path.to_string(),
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
        assert!(CoreToolName::from_public_name("core.agent_spawn").is_none());
        assert!(CoreToolName::from_public_name("memory.memory_store").is_none());
    }

    #[test]
    fn is_core_tool_only_for_known_builtins() {
        assert!(is_core_tool("core.skill_view"));
        assert!(!is_core_tool("core.unknown"));
        assert!(!is_core_tool("memory.memory_store"));
    }

    #[test]
    fn skill_view_spec_is_valid_schema() {
        let spec = CoreToolName::SkillView.spec();
        assert_eq!(spec.name, "core.skill_view");
        assert_eq!(spec.parameters["type"], "object");
        assert_eq!(spec.parameters["required"][0], "name");
    }
}
