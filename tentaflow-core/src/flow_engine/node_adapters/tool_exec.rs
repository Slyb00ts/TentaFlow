// ===== File: flow_engine/node_adapters/tool_exec.rs — ToolExecNodeAdapter
// (node_type "tool_exec", category service). Executes the tool_calls of the
// last assistant message: core.* in Core, addon tools through the
// ToolDispatcher (wasmtime, run on a blocking thread). Results become role=tool
// messages (middle-out truncated); each execution is audited and appended to
// the run log. No tool_calls present → the run is done (harness_done signal,
// end detection à la Codex/Hermes). The loop that re-runs this block is a Flow
// Builder flow (phase 5). (Harness §3.4, §3.5.) =====

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::Utc;

use crate::addon::tool_dispatch::{format_results_as_messages, ToolCallResult};
use crate::agents::{is_core_tool, AgentPrincipal, AgentService, AgentServiceSlot};
use crate::db::repository;
use crate::flow_engine::envelope::{ChatRole, FlowEnvelope, LlmToolCall, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "tool_exec";
const DEFAULT_MAX_RESULT_CHARS: usize = 16_000;
const DEFAULT_MAX_TOOL_CALLS: usize = 16;
const TRUNCATION_MARKER: &str = "\n…[truncated]…\n";

pub struct ToolExecNodeAdapter {
    service: AgentServiceSlot,
}

impl ToolExecNodeAdapter {
    pub fn new(service: AgentServiceSlot) -> Self {
        Self { service }
    }

    fn config_usize(node: &FlowNode, key: &str, default: usize) -> usize {
        node.config
            .get(key)
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .filter(|n| *n > 0)
            .unwrap_or(default)
    }

    /// Pulls the tool_calls off the last assistant message (the turn the model
    /// just produced). Returns an empty slice when the last message has none —
    /// that is the run's "final response, no more tools" signal.
    fn last_assistant_tool_calls(envelope: &FlowEnvelope) -> Vec<LlmToolCall> {
        envelope
            .context
            .messages
            .iter()
            .rev()
            .find(|m| m.role == ChatRole::Assistant)
            .and_then(|m| m.tool_calls.clone())
            .unwrap_or_default()
    }

    /// Middle-out truncation (Codex/Hermes): keep the head and tail of an
    /// oversized tool result, drop the middle. Smaller than the limit → as-is.
    /// The result never exceeds `max_chars`: when the budget is too small to
    /// fit even the marker, the marker is dropped and the content is hard-cut
    /// to the budget (a pathological config like `max_result_chars: 5`, not the
    /// 16k default — but the invariant must still hold).
    fn truncate_middle_out(content: String, max_chars: usize) -> String {
        let total = content.chars().count();
        if total <= max_chars {
            return content;
        }
        let marker_len = TRUNCATION_MARKER.chars().count();
        if max_chars <= marker_len {
            return content.chars().take(max_chars).collect();
        }
        let keep = max_chars - marker_len;
        let head_len = keep / 2;
        let tail_len = keep - head_len;
        let chars: Vec<char> = content.chars().collect();
        let head: String = chars[..head_len].iter().collect();
        let tail: String = chars[chars.len() - tail_len..].iter().collect();
        format!("{head}{TRUNCATION_MARKER}{tail}")
    }

    /// Records every executed call against the run's AI event (§3.10). Core
    /// tools have no owning addon; addon tools carry their addon id. Best-effort
    /// — failures are swallowed inside the service.
    fn audit_results(
        service: &AgentService,
        run_id: &str,
        calls: &[LlmToolCall],
        results: &[ToolCallResult],
        started_at: chrono::DateTime<chrono::Utc>,
    ) {
        for (call, result) in calls.iter().zip(results.iter()) {
            let addon_id = if is_core_tool(&result.name) {
                None
            } else {
                result.name.split_once('.').map(|(a, _)| a)
            };
            let error_message = if result.success {
                None
            } else {
                Some(result.content.as_str())
            };
            service.record_tool_execution(
                run_id,
                &crate::compliance::ai_gateway::ToolExecution {
                    tool_call_id: &result.tool_call_id,
                    addon_id,
                    tool_name: &result.name,
                    arguments: &call.arguments,
                    output: &result.content,
                    success: result.success,
                    error_message,
                    started_at,
                },
            );
        }
    }
}

#[async_trait]
impl NodeAdapter for ToolExecNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Any)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("full", FlowDataType::Any)]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("tool_exec: missing input edge"))?;
        let envelope = &input.envelope;

        let service = self
            .service
            .read()
            .clone()
            .ok_or_else(|| anyhow!("tool_exec: AgentService slot not wired"))?;

        let max_result_chars = Self::config_usize(node, "max_result_chars", DEFAULT_MAX_RESULT_CHARS);
        let max_tool_calls = Self::config_usize(node, "max_tool_calls_per_iteration", DEFAULT_MAX_TOOL_CALLS);

        let mut out: FlowEnvelope = (**envelope).clone();
        let mut calls = Self::last_assistant_tool_calls(envelope);

        // End detection: an assistant turn without tool calls is the final
        // response — signal the loop to stop (§1.1, §3.4).
        if calls.is_empty() {
            out.meta.insert("harness_done".into(), serde_json::json!(true));
            out.meta.insert(
                "harness_exit_reason".into(),
                serde_json::json!("final_response"),
            );
            return Ok(out);
        }

        // Cap calls per iteration: a runaway model cannot fan out unbounded tool
        // work in one turn. Excess calls are dropped before dispatch.
        if calls.len() > max_tool_calls {
            calls.truncate(max_tool_calls);
        }

        // The effective tool surface is the agent's allowlist (§3.3); reload it
        // from the agent the harness pinned in meta. No agent id = no allowlist
        // (every call is rejected as out-of-surface — a misconfigured flow).
        let agent_id = envelope.meta.get("agent_id").and_then(|v| v.as_str());
        let tools_json = match agent_id {
            Some(id) => service
                .get_agent(id)?
                .map(|a| a.tools_json)
                .unwrap_or_else(|| "[]".to_string()),
            None => "[]".to_string(),
        };

        let principal = AgentPrincipal::new(ctx.user_id.clone(), None);
        let started_at = Utc::now();

        // Tool dispatch (incl. wasmtime addon calls) is synchronous — run it on
        // a blocking thread so the async executor is not stalled (§2.12).
        let service_for_blocking = service.clone();
        let calls_for_blocking = calls.clone();
        let results = tokio::task::spawn_blocking(move || {
            service_for_blocking.process_tool_calls(
                &tools_json,
                &calls_for_blocking,
                &principal,
            )
        })
        .await
        .map_err(|e| anyhow!("tool_exec: dispatch task join failed: {e}"))?;

        // Audit + run log against the run's AI event before truncation (the
        // audit keeps the full output; only the model-facing message is cut).
        if let Some(run_id) = envelope
            .meta
            .get("agent_run_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            Self::audit_results(&service, run_id, &calls, &results, started_at);
            let step = serde_json::json!({
                "kind": "tool_exec",
                "calls": results
                    .iter()
                    .map(|r| serde_json::json!({
                        "name": r.name,
                        "success": r.success,
                    }))
                    .collect::<Vec<_>>(),
                "at": Utc::now().to_rfc3339(),
            });
            let _ = repository::append_agent_run_log(service.db(), run_id, &step.to_string());
        }

        // Truncate each result middle-out, then append as role=tool messages
        // after the assistant turn that requested them.
        let truncated: Vec<ToolCallResult> = results
            .into_iter()
            .map(|mut r| {
                r.content = Self::truncate_middle_out(r.content, max_result_chars);
                r
            })
            .collect();
        out.context
            .messages
            .extend(format_results_as_messages(&truncated));

        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentService;
    use crate::db::migrations;
    use crate::db::models::{AgentParams, SkillParams};
    use crate::db::DbPool;
    use crate::flow_engine::envelope::{ChatMessage, ChatMessageContent};
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        migrations::run(&conn).expect("migrations");
        Arc::new(Mutex::new(conn))
    }

    fn service(pool: DbPool) -> AgentServiceSlot {
        let cipher = Arc::new(crate::crypto::SettingsCipher::new(&[0u8; 32]));
        let addon_manager = Arc::new(
            crate::addon::AddonManager::new(pool.clone(), cipher).expect("addon manager"),
        );
        let svc = Arc::new(AgentService::new(pool, addon_manager));
        Arc::new(parking_lot::RwLock::new(Some(svc)))
    }

    fn seed_skill(pool: &DbPool, id: &str, name: &str) {
        repository::upsert_skill(
            pool,
            &SkillParams {
                id,
                name,
                display_name: None,
                description: "desc",
                content: "# Skill\nthe full instructions",
                tags_json: "[]",
                category: None,
                source: "user",
                source_ref: None,
                status: "active",
                created_by: None,
                actor_user_id: None,
            },
        )
        .expect("seed skill");
    }

    fn seed_agent(pool: &DbPool, id: &str, tools: &str) {
        repository::upsert_agent(
            pool,
            &AgentParams {
                id,
                name: "a",
                display_name: None,
                description: "d",
                system_prompt: None,
                model: None,
                tools_json: tools,
                skills_json: "{}",
                params_json: "{}",
                max_iterations: 5,
                timeout_secs: 600,
                max_subagents: 0,
                max_spawn_depth: 1,
                flow_id: None,
                routable: true,
                is_enabled: true,
                actor_user_id: None,
            },
        )
        .expect("seed agent");
    }

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "te1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
        }
    }

    fn input(env: FlowEnvelope) -> NodeInput {
        NodeInput {
            from_node_id: "llm".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }
    }

    fn assistant_with_calls(calls: Vec<LlmToolCall>) -> ChatMessage {
        let mut m = ChatMessage::assistant("");
        m.tool_calls = Some(calls);
        m
    }

    #[tokio::test]
    async fn no_tool_calls_sets_harness_done() {
        let slot = service(db());
        let mut env = FlowEnvelope::empty();
        env.context.messages.push(ChatMessage::assistant("final answer"));
        let ctx = stub_ctx();

        let out = ToolExecNodeAdapter::new(slot)
            .execute(&node(json!({})), &[input(env)], &ctx)
            .await
            .expect("execute");

        assert_eq!(out.meta.get("harness_done").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(
            out.meta.get("harness_exit_reason").and_then(|v| v.as_str()),
            Some("final_response")
        );
    }

    #[tokio::test]
    async fn executes_core_skill_view_call() {
        let pool = db();
        seed_skill(&pool, "11111111-0000-0000-0000-0000000000aa", "do-thing");
        seed_agent(&pool, "agent-1", r#"["core.skill_view"]"#);
        let slot = service(pool);

        let mut env = FlowEnvelope::empty();
        env.meta.insert("agent_id".into(), json!("agent-1"));
        env.context.messages.push(assistant_with_calls(vec![LlmToolCall {
            id: "call-1".into(),
            name: "core.skill_view".into(),
            arguments: r#"{"name":"do-thing"}"#.into(),
        }]));
        let ctx = stub_ctx();

        let out = ToolExecNodeAdapter::new(slot)
            .execute(&node(json!({})), &[input(env)], &ctx)
            .await
            .expect("execute");

        let tool_msg = out
            .context
            .messages
            .iter()
            .find(|m| m.role == ChatRole::Tool)
            .expect("tool message appended");
        assert_eq!(tool_msg.tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(tool_msg.name.as_deref(), Some("core.skill_view"));
        if let ChatMessageContent::Text(t) = &tool_msg.content {
            assert!(t.contains("full instructions"));
        } else {
            panic!("tool content must be text");
        }
        assert!(out.meta.get("harness_done").is_none());
    }

    #[tokio::test]
    async fn rejects_call_outside_allowlist() {
        let pool = db();
        seed_agent(&pool, "agent-2", r#"["core.skill_view"]"#);
        let slot = service(pool);

        let mut env = FlowEnvelope::empty();
        env.meta.insert("agent_id".into(), json!("agent-2"));
        env.context.messages.push(assistant_with_calls(vec![LlmToolCall {
            id: "call-9".into(),
            name: "memory.memory_store".into(),
            arguments: "{}".into(),
        }]));
        let mut ctx = stub_ctx();
        ctx.user_id = Some("u1".into());

        let out = ToolExecNodeAdapter::new(slot)
            .execute(&node(json!({})), &[input(env)], &ctx)
            .await
            .expect("execute");

        let tool_msg = out
            .context
            .messages
            .iter()
            .find(|m| m.role == ChatRole::Tool)
            .expect("tool message appended");
        if let ChatMessageContent::Text(t) = &tool_msg.content {
            assert!(t.contains("not in agent allowlist"), "got: {t}");
        } else {
            panic!("tool content must be text");
        }
    }

    #[tokio::test]
    async fn truncates_oversized_result_middle_out() {
        let pool = db();
        let big = "x".repeat(50_000);
        repository::upsert_skill(
            &pool,
            &SkillParams {
                id: "22222222-0000-0000-0000-0000000000bb",
                name: "big",
                display_name: None,
                description: "d",
                content: &big,
                tags_json: "[]",
                category: None,
                source: "user",
                source_ref: None,
                status: "active",
                created_by: None,
                actor_user_id: None,
            },
        )
        .expect("seed big skill");
        seed_agent(&pool, "agent-3", r#"["core.skill_view"]"#);
        let slot = service(pool);

        let mut env = FlowEnvelope::empty();
        env.meta.insert("agent_id".into(), json!("agent-3"));
        env.context.messages.push(assistant_with_calls(vec![LlmToolCall {
            id: "c".into(),
            name: "core.skill_view".into(),
            arguments: r#"{"name":"big"}"#.into(),
        }]));
        let ctx = stub_ctx();

        let out = ToolExecNodeAdapter::new(slot)
            .execute(&node(json!({"max_result_chars": 2000})), &[input(env)], &ctx)
            .await
            .expect("execute");

        let tool_msg = out
            .context
            .messages
            .iter()
            .find(|m| m.role == ChatRole::Tool)
            .expect("tool message appended");
        if let ChatMessageContent::Text(t) = &tool_msg.content {
            assert!(t.chars().count() <= 2000, "len was {}", t.chars().count());
            assert!(t.contains("truncated"));
        } else {
            panic!("tool content must be text");
        }
    }

    #[test]
    fn truncate_middle_out_keeps_head_and_tail() {
        let s = "A".repeat(100) + &"B".repeat(100);
        let out = ToolExecNodeAdapter::truncate_middle_out(s, 40);
        assert!(out.chars().count() <= 40);
        assert!(out.starts_with('A'));
        assert!(out.ends_with('B'));
        assert!(out.contains("truncated"));
    }

    #[test]
    fn truncate_middle_out_never_exceeds_a_tiny_budget() {
        // A budget below the marker length must still produce <= max_chars: the
        // marker is dropped and the content hard-cut, never the bare 15-char
        // marker that would overshoot a max_chars of 5.
        let s = "X".repeat(200);
        for budget in [1usize, 5, 14, 15] {
            let out = ToolExecNodeAdapter::truncate_middle_out(s.clone(), budget);
            assert!(
                out.chars().count() <= budget,
                "budget {budget} produced {} chars",
                out.chars().count()
            );
        }
        // Content already within budget is returned verbatim.
        assert_eq!(ToolExecNodeAdapter::truncate_middle_out("hi".into(), 3), "hi");
    }
}
