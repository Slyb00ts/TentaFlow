// ===== File: addon/tool_dispatch.rs — ToolDispatcher: bridges LLM tool calls to
// addon WASM tools. Resolves public "addon_id.tool_name" names against the
// registered tool list, executes calls through AddonManager and formats results
// as role:"tool" conversation messages (HARNESS_PLAN §3.1). =====

use std::sync::Arc;

use anyhow::{anyhow, bail, Result};
use tracing::{info, warn};

use crate::flow_engine::dispatchers::LlmToolSpec;
use crate::flow_engine::envelope::{ChatMessage, ChatMessageContent, ChatRole, LlmToolCall};

use super::{AddonManager, ToolDefinition};

/// Outcome of one executed tool call — carries everything the tool loop
/// needs to feed the result back to the model and to the compliance audit.
#[derive(Debug, Clone)]
pub struct ToolCallResult {
    /// Id of the `LlmToolCall` this result answers.
    pub tool_call_id: String,
    /// Public tool name (`"addon_id.tool_name"`).
    pub name: String,
    /// JSON-encoded tool output, or `{"error": ...}` on failure.
    pub content: String,
    pub success: bool,
}

/// Splits a public tool name into `(addon_id, tool_name)`. Public names use
/// the `"{addon_id}.{tool_name}"` convention; the split happens at the FIRST
/// dot because addon ids never contain one. Both parts must be non-empty.
pub fn parse_tool_name(full_name: &str) -> Result<(&str, &str)> {
    match full_name.split_once('.') {
        Some((addon_id, tool_name)) if !addon_id.is_empty() && !tool_name.is_empty() => {
            Ok((addon_id, tool_name))
        }
        _ => bail!(
            "invalid tool name '{}': expected 'addon_id.tool_name'",
            full_name
        ),
    }
}

/// Pure pre-dispatch resolution: parses the public name and matches it
/// against the registered tool list. Separate from `ToolDispatcher` so the
/// name/registry logic is unit-testable without a WASM runtime.
pub fn resolve_tool<'a>(
    tools: &'a [ToolDefinition],
    full_name: &str,
) -> Result<&'a ToolDefinition> {
    let (addon_id, tool_name) = parse_tool_name(full_name)?;
    tools
        .iter()
        .find(|t| t.addon_id == addon_id && t.tool_name == tool_name)
        .ok_or_else(|| anyhow!("tool '{}' is not registered by any addon", full_name))
}

/// Canonical `ToolDefinition` → wire-spec mapping — the single source of the
/// public name and parameter-schema shape advertised to models
/// (`LlmRequest.tools`, native body or prompt-mode section alike).
pub fn tool_definition_to_spec(tool: &ToolDefinition) -> LlmToolSpec {
    LlmToolSpec {
        name: format!("{}.{}", tool.addon_id, tool.tool_name),
        description: tool.description.clone(),
        parameters: tool.parameters_schema.clone(),
    }
}

/// Formats executed results as role:"tool" conversation messages — one per
/// call id, in execution order, ready to append after the assistant turn
/// that requested the calls.
pub fn format_results_as_messages(results: &[ToolCallResult]) -> Vec<ChatMessage> {
    results
        .iter()
        .map(|result| ChatMessage {
            role: ChatRole::Tool,
            content: ChatMessageContent::Text(result.content.clone()),
            reasoning_content: None,
            name: Some(result.name.clone()),
            tool_call_id: Some(result.tool_call_id.clone()),
            tool_calls: None,
        })
        .collect()
}

/// Tool-calling bridge between the model loop and addon WASM tools.
/// Validates names against the live tool registry, delegates execution to
/// `AddonManager` and shapes results for the conversation.
pub struct ToolDispatcher {
    addon_manager: Arc<AddonManager>,
}

impl ToolDispatcher {
    pub fn new(addon_manager: Arc<AddonManager>) -> Self {
        Self { addon_manager }
    }

    /// Executes a single tool call. Permission enforcement (the per-addon
    /// "llm" permission) happens inside `AddonManager::call_tool`, so a
    /// denied user fails there — no separate pre-check needed here.
    pub fn dispatch_tool_call(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
        user_id: &str,
    ) -> Result<serde_json::Value> {
        let tools = self.addon_manager.list_tools();
        let tool = resolve_tool(&tools, tool_name)?;
        info!(
            "dispatching tool call '{}' for user_id={}",
            tool_name, user_id
        );
        self.addon_manager
            .call_tool(&tool.addon_id, &tool.tool_name, arguments, user_id)
    }

    /// Like `dispatch_tool_call` but skips the addon permission check — the
    /// caller (harness tool_exec permission path) already adjudicated the grant
    /// (§3.13 B). Used only for AllowOnce / AllowForRun retries, which do not
    /// persist a grant the checker would otherwise see.
    pub fn dispatch_tool_call_preauthorized(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
        user_id: &str,
    ) -> Result<serde_json::Value> {
        let tools = self.addon_manager.list_tools();
        let tool = resolve_tool(&tools, tool_name)?;
        self.addon_manager.call_tool_preauthorized(
            &tool.addon_id,
            &tool.tool_name,
            arguments,
            user_id,
        )
    }

    /// Executes every call requested by one assistant turn, in order
    /// (sequential by design — simpler to audit, addon instances pool
    /// anyway). A failed call becomes an error result, never an aborted
    /// batch: the model expects a role:"tool" reply for every call id it
    /// emitted. Invalid argument JSON fails the call instead of invoking
    /// the tool with empty arguments.
    pub fn process_tool_calls(
        &self,
        tool_calls: &[LlmToolCall],
        user_id: &str,
    ) -> Vec<ToolCallResult> {
        let mut results = Vec::with_capacity(tool_calls.len());
        for call in tool_calls {
            let arguments = match serde_json::from_str::<serde_json::Value>(&call.arguments) {
                Ok(value) => value,
                Err(e) => {
                    warn!("invalid JSON arguments in tool call '{}': {}", call.name, e);
                    results.push(failed_result(call, format!("invalid arguments JSON: {e}")));
                    continue;
                }
            };
            match self.dispatch_tool_call(&call.name, arguments, user_id) {
                Ok(result) => results.push(ToolCallResult {
                    tool_call_id: call.id.clone(),
                    name: call.name.clone(),
                    content: serde_json::to_string(&result).unwrap_or_default(),
                    success: true,
                }),
                Err(e) => {
                    warn!("tool call '{}' failed: {}", call.name, e);
                    results.push(failed_result(call, e.to_string()));
                }
            }
        }
        results
    }

    /// Tools visible to `user_id`, in the wire shape advertised to models.
    /// Filtered by the per-addon "llm" permission so the model never sees a
    /// tool the user could not execute.
    pub fn get_tools_for_llm(&self, user_id: &str) -> Vec<LlmToolSpec> {
        self.addon_manager
            .list_tools()
            .iter()
            .filter(|tool| {
                self.addon_manager
                    .permission_checker()
                    .check(&tool.addon_id, user_id, "llm", None)
                    .is_granted()
            })
            .map(tool_definition_to_spec)
            .collect()
    }
}

fn failed_result(call: &LlmToolCall, error: String) -> ToolCallResult {
    ToolCallResult {
        tool_call_id: call.id.clone(),
        name: call.name.clone(),
        content: serde_json::json!({ "error": error }).to_string(),
        success: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(addon_id: &str, tool_name: &str) -> ToolDefinition {
        ToolDefinition {
            addon_id: addon_id.to_string(),
            package_id: addon_id.to_string(),
            tool_name: tool_name.to_string(),
            description: format!("{tool_name} description"),
            parameters_schema: serde_json::json!({
                "type": "object",
                "properties": { "fact": { "type": "string" } },
                "required": ["fact"]
            }),
            return_schema: None,
            keywords: vec!["remember".to_string()],
        }
    }

    #[test]
    fn parse_tool_name_splits_at_first_dot() {
        assert_eq!(
            parse_tool_name("memory.memory_store").unwrap(),
            ("memory", "memory_store")
        );
        // Only the first dot separates — the remainder belongs to the tool id.
        assert_eq!(parse_tool_name("a.b.c").unwrap(), ("a", "b.c"));
    }

    #[test]
    fn parse_tool_name_rejects_malformed() {
        assert!(parse_tool_name("nodot").is_err());
        assert!(parse_tool_name(".tool").is_err());
        assert!(parse_tool_name("addon.").is_err());
        assert!(parse_tool_name("").is_err());
        assert!(parse_tool_name(".").is_err());
    }

    #[test]
    fn resolve_tool_finds_registered_tool() {
        let tools = vec![tool("memory", "memory_store"), tool("contacts", "lookup")];
        let resolved = resolve_tool(&tools, "contacts.lookup").unwrap();
        assert_eq!(resolved.addon_id, "contacts");
        assert_eq!(resolved.tool_name, "lookup");
    }

    #[test]
    fn resolve_tool_rejects_unknown_and_malformed() {
        let tools = vec![tool("memory", "memory_store")];
        let unknown = resolve_tool(&tools, "memory.memory_recall").unwrap_err();
        assert!(unknown.to_string().contains("memory.memory_recall"));
        let wrong_addon = resolve_tool(&tools, "contacts.memory_store").unwrap_err();
        assert!(wrong_addon.to_string().contains("not registered"));
        assert!(resolve_tool(&tools, "memory_store").is_err());
    }

    #[test]
    fn tool_definition_to_spec_matches_wire_shape() {
        let def = tool("memory", "memory_store");
        let spec = tool_definition_to_spec(&def);
        assert_eq!(spec.name, "memory.memory_store");
        assert_eq!(spec.description, "memory_store description");
        assert_eq!(spec.parameters, def.parameters_schema);
    }

    #[test]
    fn format_results_as_messages_builds_tool_role_messages() {
        let results = vec![
            ToolCallResult {
                tool_call_id: "call_0_aabbccdd".to_string(),
                name: "memory.memory_store".to_string(),
                content: r#"{"stored":true}"#.to_string(),
                success: true,
            },
            ToolCallResult {
                tool_call_id: "call_1_11223344".to_string(),
                name: "memory.memory_recall".to_string(),
                content:
                    r#"{"error":"tool 'memory.memory_recall' is not registered by any addon"}"#
                        .to_string(),
                success: false,
            },
        ];
        let messages = format_results_as_messages(&results);
        assert_eq!(messages.len(), 2);
        for (msg, result) in messages.iter().zip(&results) {
            assert_eq!(msg.role, ChatRole::Tool);
            assert_eq!(
                msg.tool_call_id.as_deref(),
                Some(result.tool_call_id.as_str())
            );
            assert_eq!(msg.name.as_deref(), Some(result.name.as_str()));
            assert_eq!(msg.text(), Some(result.content.as_str()));
            assert!(msg.tool_calls.is_none());
        }
        // The wire role must serialize as "tool" — backends pair it with the
        // assistant tool_calls by this exact string.
        let wire = serde_json::to_value(&messages[0]).unwrap();
        assert_eq!(wire["role"], "tool");
        assert_eq!(wire["tool_call_id"], "call_0_aabbccdd");
    }
}
