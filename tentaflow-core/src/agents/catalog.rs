// ===== File: agents/catalog.rs — Tool catalog resolution: given an agent's
// tools_json allowlist + a run principal, produce the LlmToolSpec list the
// model sees. Result = (addon tools ∩ allowlist ∩ "llm" permission for the
// principal) ∪ (core.* builtins in the allowlist). (Harness §3.1, §3.3). =====

use crate::addon::{tool_dispatch::tool_definition_to_spec, ToolDefinition};
use crate::flow_engine::dispatchers::LlmToolSpec;

use super::builtins::CoreToolName;
use super::principal::AgentPrincipal;

/// One parsed allowlist entry. The agent's `tools_json` is a JSON array of
/// public names: `"addon_id.tool"`, an `"addon_id.*"` wildcard, or a
/// `"core.<tool>"` builtin. Anything else is rejected on parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllowlistEntry {
    /// Exact `addon_id.tool_name`.
    Tool { addon_id: String, tool_name: String },
    /// `addon_id.*` — every tool of one addon.
    AddonWildcard { addon_id: String },
    /// `core.<tool>` builtin.
    Core(CoreToolName),
}

impl AllowlistEntry {
    /// Parses a single public name. `core.<tool>` resolves against the known
    /// builtin set (an unknown `core.*` is rejected, not silently ignored, so a
    /// typo in an agent definition surfaces). `addon.*` is the addon wildcard.
    fn parse(raw: &str) -> Option<Self> {
        let (head, tail) = raw.split_once('.')?;
        if head.is_empty() || tail.is_empty() {
            return None;
        }
        if head == super::builtins::CORE_ADDON_ID {
            return CoreToolName::from_public_name(raw).map(AllowlistEntry::Core);
        }
        if tail == "*" {
            return Some(AllowlistEntry::AddonWildcard {
                addon_id: head.to_string(),
            });
        }
        Some(AllowlistEntry::Tool {
            addon_id: head.to_string(),
            tool_name: tail.to_string(),
        })
    }

    /// True when this entry admits a given addon tool.
    fn matches_addon_tool(&self, addon_id: &str, tool_name: &str) -> bool {
        match self {
            AllowlistEntry::Tool {
                addon_id: a,
                tool_name: t,
            } => a == addon_id && t == tool_name,
            AllowlistEntry::AddonWildcard { addon_id: a } => a == addon_id,
            AllowlistEntry::Core(_) => false,
        }
    }
}

/// Parses the agent's `tools_json` allowlist. Unparseable entries are dropped
/// (the handler validates well-formed JSON; a stray malformed name simply does
/// not admit any tool). Returns the parsed entry list.
pub fn parse_allowlist(tools_json: &str) -> Vec<AllowlistEntry> {
    let names: Vec<String> = serde_json::from_str(tools_json).unwrap_or_default();
    names
        .iter()
        .filter_map(|n| AllowlistEntry::parse(n))
        .collect()
}

/// Convenience predicate: is `name` admitted by `tools_json` for an addon tool
/// or a core builtin? Used by tool_exec to reject a model-issued call to a tool
/// outside the agent's surface before dispatching it.
pub fn tool_in_allowlist(tools_json: &str, name: &str) -> bool {
    let entries = parse_allowlist(tools_json);
    if let Some(core) = CoreToolName::from_public_name(name) {
        return entries.iter().any(|e| e == &AllowlistEntry::Core(core));
    }
    match name.split_once('.') {
        Some((addon_id, tool_name)) if !addon_id.is_empty() && !tool_name.is_empty() => entries
            .iter()
            .any(|e| e.matches_addon_tool(addon_id, tool_name)),
        _ => false,
    }
}

/// Resolves the full LlmToolSpec list the model sees for one agent + principal.
/// Pure over its inputs (`addon_tools` is the live `AddonManager::list_tools()`,
/// `is_permitted` the per-addon "llm" permission check for the principal) so the
/// intersection logic is unit-testable without a WASM runtime.
pub struct ToolCatalog;

impl ToolCatalog {
    /// Build the catalog: addon tools admitted by the allowlist AND granted to
    /// the principal, followed by core builtins in the allowlist. Order is
    /// stable (addon tools in `list_tools` order, then core builtins in
    /// `CoreToolName::all` order) so a flow's tool list is deterministic.
    ///
    /// `is_permitted(addon_id)` must return the "llm" permission decision for
    /// the run principal. An unattended run (`principal.user_id == None`) admits
    /// NO addon tools — only core builtins — because there is no user to check.
    ///
    /// `can_spawn` gates the sub-agent control builtins (agent_spawn/wait/list/
    /// cancel): they appear only when the running agent may spawn children
    /// (`max_subagents > 0`, §3.6), even if its allowlist names them. An agent
    /// that cannot spawn never sees the delegation surface.
    pub fn resolve<F>(
        tools_json: &str,
        principal: &AgentPrincipal,
        addon_tools: &[ToolDefinition],
        can_spawn: bool,
        mut is_permitted: F,
    ) -> Vec<LlmToolSpec>
    where
        F: FnMut(&str) -> bool,
    {
        let entries = parse_allowlist(tools_json);
        let mut out = Vec::new();

        if principal.user_id().is_some() {
            for tool in addon_tools {
                let admitted = entries
                    .iter()
                    .any(|e| e.matches_addon_tool(&tool.addon_id, &tool.tool_name));
                if admitted && is_permitted(&tool.addon_id) {
                    out.push(tool_definition_to_spec(tool));
                }
            }
        }

        for core in CoreToolName::all() {
            if core.is_subagent_control() && !can_spawn {
                continue;
            }
            if entries.iter().any(|e| e == &AllowlistEntry::Core(*core)) {
                out.push(core.spec());
            }
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(addon_id: &str, tool_name: &str) -> ToolDefinition {
        ToolDefinition {
            addon_id: addon_id.to_string(),
            tool_name: tool_name.to_string(),
            description: format!("{tool_name} desc"),
            parameters_schema: serde_json::json!({"type": "object"}),
            return_schema: None,
            keywords: Vec::new(),
        }
    }

    #[test]
    fn parses_exact_wildcard_and_core_entries() {
        assert_eq!(
            AllowlistEntry::parse("memory.memory_store"),
            Some(AllowlistEntry::Tool {
                addon_id: "memory".into(),
                tool_name: "memory_store".into()
            })
        );
        assert_eq!(
            AllowlistEntry::parse("memory.*"),
            Some(AllowlistEntry::AddonWildcard {
                addon_id: "memory".into()
            })
        );
        assert_eq!(
            AllowlistEntry::parse("core.skill_view"),
            Some(AllowlistEntry::Core(CoreToolName::SkillView))
        );
        assert_eq!(
            AllowlistEntry::parse("core.agent_spawn"),
            Some(AllowlistEntry::Core(CoreToolName::AgentSpawn))
        );
        // An UNKNOWN core builtin is rejected, not treated as an addon tool.
        assert!(AllowlistEntry::parse("core.bogus").is_none());
        assert!(AllowlistEntry::parse("nodot").is_none());
        assert!(AllowlistEntry::parse(".tool").is_none());
    }

    #[test]
    fn resolve_intersects_allowlist_and_permission() {
        let tools = vec![
            tool("memory", "memory_store"),
            tool("memory", "memory_recall"),
            tool("contacts", "lookup"),
        ];
        let principal = AgentPrincipal::user("u1");
        // Allowlist admits all memory tools (wildcard) + contacts.lookup +
        // core.skill_view. Permission denies the `contacts` addon.
        let json = r#"["memory.*","contacts.lookup","core.skill_view"]"#;
        let specs =
            ToolCatalog::resolve(json, &principal, &tools, false, |addon| addon == "memory");
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "memory.memory_store",
                "memory.memory_recall",
                "core.skill_view"
            ]
        );
    }

    #[test]
    fn resolve_exact_entry_excludes_sibling_tool() {
        let tools = vec![
            tool("memory", "memory_store"),
            tool("memory", "memory_recall"),
        ];
        let principal = AgentPrincipal::user("u1");
        let json = r#"["memory.memory_store"]"#;
        let specs = ToolCatalog::resolve(json, &principal, &tools, false, |_| true);
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["memory.memory_store"]);
    }

    #[test]
    fn subagent_control_builtins_gated_by_can_spawn() {
        let principal = AgentPrincipal::user("u1");
        let json = r#"["core.skill_view","core.agent_spawn","core.agent_wait"]"#;
        // can_spawn=false hides the delegation surface even when allowlisted.
        let no_spawn = ToolCatalog::resolve(json, &principal, &[], false, |_| true);
        let names: Vec<&str> = no_spawn.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["core.skill_view"]);
        // can_spawn=true surfaces the allowlisted control builtins.
        let with_spawn = ToolCatalog::resolve(json, &principal, &[], true, |_| true);
        let names: Vec<&str> = with_spawn.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["core.skill_view", "core.agent_spawn", "core.agent_wait"]
        );
    }

    #[test]
    fn unattended_principal_gets_only_core_builtins() {
        let tools = vec![tool("memory", "memory_store")];
        let principal = AgentPrincipal::new(
            None,
            None,
            crate::flow_engine::dispatcher::FlowOrigin::Api,
            crate::flow_engine::dispatcher::FlowActor::api_key("key-svc", None),
        );
        let json = r#"["memory.*","core.skill_view"]"#;
        // Even with a permissive checker, no user_id means no addon tools.
        let specs = ToolCatalog::resolve(json, &principal, &tools, false, |_| true);
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["core.skill_view"]);
    }

    #[test]
    fn code_studio_names_parse_and_a_typo_admits_nothing() {
        // The allowlist is the first sieve of §10: every Code Studio verb must
        // resolve, and a name that is not a known builtin must admit NOTHING —
        // not the tool, and not a silent passthrough to the addon dispatcher.
        for tool in CoreToolName::all().iter().filter(|t| t.is_code_studio()) {
            assert_eq!(
                AllowlistEntry::parse(tool.public_name()),
                Some(AllowlistEntry::Core(*tool)),
                "{}",
                tool.public_name()
            );
        }
        assert_eq!(
            AllowlistEntry::parse("core.code_search"),
            Some(AllowlistEntry::Core(CoreToolName::CodeSearch))
        );
        assert!(AllowlistEntry::parse("core.fs_chmod").is_none());
        // A typo does not fall through to "an addon called core".
        let json = r#"["core.fs_read","core.fs_chmod"]"#;
        assert!(tool_in_allowlist(json, "core.fs_read"));
        assert!(!tool_in_allowlist(json, "core.fs_chmod"));
        assert_eq!(parse_allowlist(json).len(), 1);
    }

    #[test]
    fn a_read_only_agent_never_surfaces_a_write_verb() {
        // §15: the separation of duties is the allowlist, and no permission
        // check can widen it — `is_permitted` is consulted for ADDON tools only.
        let reviewer = r#"["core.fs_read","core.fs_grep","core.git_read"]"#;
        let specs =
            ToolCatalog::resolve(reviewer, &AgentPrincipal::user("u1"), &[], true, |_| true);
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["core.fs_read", "core.fs_grep", "core.git_read"]);
        assert!(!names.contains(&"core.fs_write"));
        assert!(!names.contains(&"core.git_push"));
    }

    #[test]
    fn tool_in_allowlist_matches_addon_and_core() {
        let json = r#"["memory.*","core.skill_view"]"#;
        assert!(tool_in_allowlist(json, "memory.memory_store"));
        assert!(tool_in_allowlist(json, "core.skill_view"));
        assert!(!tool_in_allowlist(json, "contacts.lookup"));
        assert!(!tool_in_allowlist(json, "core.agent_spawn"));
    }
}
