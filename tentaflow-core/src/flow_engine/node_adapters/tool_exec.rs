// ===== File: flow_engine/node_adapters/tool_exec.rs — ToolExecNodeAdapter
// (node_type "tool_exec", category service). Executes the tool_calls of the
// last assistant message: core.* in Core, addon tools through the
// ToolDispatcher (wasmtime, run on a blocking thread). Results become role=tool
// messages (middle-out truncated); each execution is audited and appended to
// the run log. No tool_calls present → the run is done (harness_done signal,
// end detection à la Codex/Hermes). The loop that re-runs this block is a Flow
// Builder flow (phase 5). (Harness §3.4, §3.5.) =====

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;

use crate::addon::tool_dispatch::{format_results_as_messages, ToolCallResult};
use crate::agents::{
    is_core_tool, AgentPrincipal, AgentRunManager, AgentService, AgentServiceSlot, CallerRun,
    CoreToolName, PermissionDecision, DEFAULT_INTERACTION_TIMEOUT_SECS,
};
use crate::db::repository;
use crate::flow_engine::dispatchers::ProgressEvent;
use crate::flow_engine::envelope::{ChatRole, FlowEnvelope, LlmToolCall, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};
use crate::flow_engine::dispatchers::EmbeddingsRequest;
use crate::project_studio::{
    generation as project_generation, ingest as project_ingest, knowledge as project_knowledge,
};
use crate::services::org::DEFAULT_ORG_ID;

const NODE_TYPE: &str = "tool_exec";
const DEFAULT_MAX_RESULT_CHARS: usize = 16_000;
const DEFAULT_MAX_TOOL_CALLS: usize = 16;
const TRUNCATION_MARKER: &str = "\n…[truncated]…\n";

/// Budget for a `core.project_search` result JSON — headroom under the default
/// 16k middle-out truncation, so the model always receives intact JSON (a
/// middle-out cut through JSON would be unparseable).
const PROJECT_SEARCH_RESULT_BUDGET: usize = 15_000;

/// Human-wait budget for a permission grant card (§3.13 B). A grant has no
/// model-supplied timeout (unlike ask_user), so it uses the shared default.
const DEFAULT_PERMISSION_TIMEOUT: Duration = Duration::from_secs(DEFAULT_INTERACTION_TIMEOUT_SECS);

/// Shapes a failed tool call into a recoverable `[TOOL_ERROR]`-style result the
/// model can adapt to (mirrors the service's private helper).
fn error_result(call: &LlmToolCall, error: String) -> ToolCallResult {
    ToolCallResult {
        tool_call_id: call.id.clone(),
        name: call.name.clone(),
        content: serde_json::json!({ "error": error }).to_string(),
        success: false,
    }
}

/// Resolves an ask_user `timeout_secs` argument (clamped to a sane ceiling),
/// defaulting to the shared 600 s budget when absent (§3.13 A).
fn resolve_timeout(args: &Value) -> Duration {
    let secs = args
        .get("timeout_secs")
        .and_then(|v| v.as_u64())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_INTERACTION_TIMEOUT_SECS)
        // Cap at one hour so a model-chosen timeout cannot park a run for days.
        .min(3600);
    Duration::from_secs(secs)
}

/// Runs one synchronous core builtin (skill_view) and shapes its result. Async
/// core builtins (ask_user, agent_*) never reach here — they route on the async
/// path in `execute`.
fn run_core_sync(service: &AgentService, call: &LlmToolCall) -> ToolCallResult {
    let arguments = match serde_json::from_str::<Value>(&call.arguments) {
        Ok(v) => v,
        Err(e) => return error_result(call, format!("invalid arguments JSON: {e}")),
    };
    match service.execute_core_tool(&call.name, &arguments) {
        Ok(output) => ToolCallResult {
            tool_call_id: call.id.clone(),
            name: call.name.clone(),
            content: serde_json::to_string(&output).unwrap_or_default(),
            success: true,
        },
        Err(e) => error_result(call, e.to_string()),
    }
}

/// Audits one permission decision to the `audit_log` chain (§3.13 B — every
/// decision is recorded). Best-effort: an audit write failure must not abort the
/// tool loop.
fn record_permission_decision(
    service: &AgentService,
    user_id: &str,
    tool_name: &str,
    decision: PermissionDecision,
) {
    let addon_id = tool_name.split_once('.').map(|(a, _)| a);
    let details = serde_json::json!({
        "tool": tool_name,
        "decision": decision.as_str(),
    })
    .to_string();
    let _ = repository::log_audit(
        service.db(),
        Some(user_id),
        addon_id,
        "agent.permission_decision",
        Some(tool_name),
        Some(&details),
        None,
        None,
    );
}

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

    /// True for the async sub-agent control builtins, dispatched through the
    /// AgentRunManager rather than the synchronous core path.
    fn is_subagent_control(name: &str) -> bool {
        CoreToolName::from_public_name(name)
            .map(|c| c.is_subagent_control())
            .unwrap_or(false)
    }

    /// True for the Project Studio knowledge builtins — async (the query
    /// embedding goes through `ctx.embeddings`), so they must never fall
    /// through to the synchronous core path.
    fn is_project_knowledge(name: &str) -> bool {
        CoreToolName::from_public_name(name)
            .map(|c| c.is_project_knowledge())
            .unwrap_or(false)
    }

    /// True for `core.project_case_save` — the generation sink with its own
    /// arm (it reads the envelope's server-minted binding).
    fn is_case_save(name: &str) -> bool {
        CoreToolName::from_public_name(name)
            .map(|c| c.is_case_save())
            .unwrap_or(false)
    }

    /// Runs one `core.project_case_save` call. The binding comes ONLY from
    /// `envelope.meta["ps_generation"]` (minted by GenerationStart at spawn) —
    /// a run without it (any non-generation agent) gets a tool error, so the
    /// model cannot target an arbitrary project. Validation failures are
    /// per-case `[TOOL_ERROR]`s the model repairs and retries.
    async fn run_case_save_call(
        ctx: &ExecutionContext,
        principal: &AgentPrincipal,
        envelope: &FlowEnvelope,
        call: &LlmToolCall,
    ) -> ToolCallResult {
        let args: Value = match serde_json::from_str(&call.arguments) {
            Ok(v) => v,
            Err(e) => return error_result(call, format!("invalid arguments JSON: {e}")),
        };
        let Some(user_id) = principal.user_id().map(|s| s.to_string()) else {
            return error_result(
                call,
                format!("tool '{}' requires a user identity", call.name),
            );
        };
        let Some(binding) = project_generation::binding_from_meta(&envelope.meta) else {
            return error_result(
                call,
                "this run is not bound to a test-case generation \
                 (core.project_case_save works only inside GenerationStart runs)"
                    .to_string(),
            );
        };
        let org = ctx
            .org_id
            .clone()
            .unwrap_or_else(|| DEFAULT_ORG_ID.to_string());
        let agent_id = envelope
            .meta
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let agent_run_id = envelope
            .meta
            .get("agent_run_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        // Blocking SQLite work off the async worker.
        let outcome = tokio::task::spawn_blocking(move || {
            project_generation::save_generated_case(
                &org,
                &user_id,
                &binding,
                &agent_id,
                &agent_run_id,
                &args,
            )
        })
        .await;
        match outcome {
            Ok(Ok(output)) => ToolCallResult {
                tool_call_id: call.id.clone(),
                name: call.name.clone(),
                content: serde_json::to_string(&output).unwrap_or_default(),
                success: true,
            },
            Ok(Err(message)) => error_result(call, message),
            Err(e) => error_result(call, format!("case save join failed: {e}")),
        }
    }

    /// Runs one `core.project_search` / `core.project_list_sources` call.
    /// The principal's user must be a member of the project (the shared
    /// membership gate answers non-members and missing projects identically,
    /// so an agent cannot probe project existence). Search shares the exact
    /// node-adapter pipeline: `rag-embeddings` query embedding + the project's
    /// `passages` namespace, with the result JSON bounded to fit the tool
    /// budget intact.
    async fn run_project_knowledge_call(
        ctx: &ExecutionContext,
        principal: &AgentPrincipal,
        call: &LlmToolCall,
    ) -> ToolCallResult {
        let args: Value = match serde_json::from_str(&call.arguments) {
            Ok(v) => v,
            Err(e) => return error_result(call, format!("invalid arguments JSON: {e}")),
        };
        let Some(user_id) = principal.user_id() else {
            return error_result(
                call,
                format!("tool '{}' requires a user identity", call.name),
            );
        };
        let org = ctx
            .org_id
            .clone()
            .unwrap_or_else(|| DEFAULT_ORG_ID.to_string());
        let Some(project_id) = args
            .get("project_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        else {
            return error_result(call, "missing required argument 'project_id'".to_string());
        };
        if let Err(e) = project_knowledge::require_member(&org, project_id, user_id) {
            return error_result(call, e.to_string());
        }

        let outcome = match CoreToolName::from_public_name(&call.name) {
            Some(CoreToolName::ProjectListSources) => {
                project_knowledge::list_sources_json(project_id)
            }
            Some(CoreToolName::ProjectSearch) => {
                let Some(query) = args
                    .get("query")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                else {
                    return error_result(call, "missing required argument 'query'".to_string());
                };
                let top_k =
                    project_knowledge::clamp_top_k(args.get("top_k").and_then(|v| v.as_u64()));
                let source_ids: Vec<String> = args
                    .get("source_ids")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|x| x.as_str())
                            .filter(|s| !s.is_empty())
                            .map(|s| s.to_string())
                            .collect()
                    })
                    .unwrap_or_default();

                let embed = ctx
                    .embeddings
                    .embed(EmbeddingsRequest {
                        model: project_ingest::EMBEDDINGS_ALIAS.to_string(),
                        inputs: vec![query.to_string()],
                        dimensions: None,
                        encoding_format: None,
                        user_id: Some(user_id.to_string()),
                        user_role: ctx.user_role.clone(),
                        flow_depth: ctx.subflow_depth,
                    })
                    .await;
                match embed {
                    Ok(response) => match response
                        .vectors
                        .into_iter()
                        .next()
                        .filter(|v| !v.is_empty())
                    {
                        Some(query_vec) => project_knowledge::search(
                            &ctx.vectors,
                            &org,
                            project_id,
                            &query_vec,
                            &source_ids,
                            top_k,
                        )
                        .map(|hits| {
                            project_knowledge::hits_to_json_bounded(
                                &hits,
                                PROJECT_SEARCH_RESULT_BUDGET,
                            )
                        }),
                        None => Err(anyhow!("query embedding empty")),
                    },
                    Err(e) => Err(anyhow!("query embedding: {e}")),
                }
            }
            _ => Err(anyhow!(
                "tool '{}' is not a project knowledge call",
                call.name
            )),
        };
        match outcome {
            Ok(output) => ToolCallResult {
                tool_call_id: call.id.clone(),
                name: call.name.clone(),
                content: serde_json::to_string(&output).unwrap_or_default(),
                success: true,
            },
            Err(e) => error_result(call, e.to_string()),
        }
    }

    /// Runs one sub-agent control call (agent_spawn/wait/list/cancel) through the
    /// manager and shapes the outcome into a ToolCallResult. A missing manager
    /// (headless / not wired) or any handler error becomes a recoverable tool
    /// error — never an aborted iteration.
    async fn run_manager_call(
        manager: &AgentRunManager,
        caller: &CallerRun,
        call: &LlmToolCall,
    ) -> ToolCallResult {
        let args: Value = match serde_json::from_str(&call.arguments) {
            Ok(v) => v,
            Err(e) => {
                return error_result(call, format!("invalid arguments JSON: {e}"));
            }
        };
        let outcome = match CoreToolName::from_public_name(&call.name) {
            Some(CoreToolName::AgentSpawn) => manager.handle_agent_spawn(caller, &args).await,
            Some(CoreToolName::AgentWait) => manager.handle_agent_wait(caller, &args).await,
            Some(CoreToolName::AgentList) => manager.handle_agent_list(caller),
            Some(CoreToolName::AgentCancel) => manager.handle_agent_cancel(caller, &args),
            _ => Err(anyhow!(
                "tool '{}' is not a sub-agent control call",
                call.name
            )),
        };
        match outcome {
            Ok(output) => ToolCallResult {
                tool_call_id: call.id.clone(),
                name: call.name.clone(),
                content: serde_json::to_string(&output).unwrap_or_default(),
                success: true,
            },
            Err(e) => error_result(call, e.to_string()),
        }
    }

    /// Runs one `core.ask_user` call (§3.13 A): raises a question interaction,
    /// parks the run in `waiting_user` (releasing its permit + pausing its
    /// deadline) and awaits the operator's reply with the configured timeout.
    /// The reply enters the model result wrapped in a trusted-user-channel
    /// marker; a timeout yields the no-response sentinel so the model adapts.
    async fn run_ask_user_call(
        manager: Option<&AgentRunManager>,
        ctx: &ExecutionContext,
        run_id: &str,
        parent_run_id: Option<&str>,
        call: &LlmToolCall,
    ) -> ToolCallResult {
        let args: Value = match serde_json::from_str(&call.arguments) {
            Ok(v) => v,
            Err(e) => return error_result(call, format!("invalid arguments JSON: {e}")),
        };
        let question = args
            .get("question")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let Some(question) = question else {
            return error_result(call, "ask_user: 'question' is required".to_string());
        };
        // At most 4 choices (§3.13 A); the dashboard appends its own "other".
        let choices: Vec<String> = args
            .get("choices")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .take(4)
                    .collect()
            })
            .unwrap_or_default();
        let timeout = resolve_timeout(&args);

        let (answer, waited) = crate::agents::run_ask_user(
            &crate::agents::interaction_registry_global(),
            manager,
            ctx.progress.as_ref(),
            &ctx.progress_scope,
            run_id,
            parent_run_id,
            question,
            &choices,
            timeout,
        )
        .await;
        // Human think-time must not consume the run's deadline (§3.13).
        ctx.extend_deadline(waited);

        ToolCallResult {
            tool_call_id: call.id.clone(),
            name: call.name.clone(),
            content: serde_json::json!({
                "question": question,
                "choices_offered": choices,
                "user_response": answer,
            })
            .to_string(),
            success: true,
        }
    }

    /// Runs one non-control tool call: `core.skill_view` (sync DB read) or an
    /// addon tool. Addon tools are permission-gated (§3.13 B): a NotConfigured
    /// deny raises a grant card and waits for the operator's decision; AllowOnce
    /// /AllowForRun dispatch pre-authorized, Always persists the grant, Deny /
    /// timeout becomes a `[TOOL_ERROR] permission denied` result. An explicit
    /// (configured) deny is final — it never prompts.
    async fn run_tool_call(
        service: &Arc<AgentService>,
        manager: Option<&AgentRunManager>,
        ctx: &ExecutionContext,
        principal: &AgentPrincipal,
        run_id: &str,
        parent_run_id: Option<&str>,
        call: &LlmToolCall,
    ) -> ToolCallResult {
        // Core builtins (skill_view) run synchronously on a blocking thread.
        if is_core_tool(&call.name) {
            let service = service.clone();
            let call_for_blocking = call.clone();
            return tokio::task::spawn_blocking(move || {
                run_core_sync(&service, &call_for_blocking)
            })
            .await
            .unwrap_or_else(|e| {
                error_result(call, format!("core tool dispatch join failed: {e}"))
            });
        }

        // Addon tools need a user principal.
        let Some(user_id) = principal.user_id().map(|s| s.to_string()) else {
            return error_result(
                call,
                format!("tool '{}' requires a user principal", call.name),
            );
        };
        let arguments: Value = match serde_json::from_str(&call.arguments) {
            Ok(v) => v,
            Err(e) => return error_result(call, format!("invalid arguments JSON: {e}")),
        };

        let registry = crate::agents::interaction_registry_global();
        // A grant earned earlier in this run skips the prompt (§3.13 B).
        let run_granted = !run_id.is_empty() && registry.run_grant_holds(run_id, &call.name);

        let mut preauthorized = run_granted;
        if !run_granted {
            use crate::addon::permissions::PermissionResult;
            match service.permission_for_tool(&call.name, &user_id) {
                // Already granted (explicit grant / default / admin bypass):
                // dispatch through the normal checked path.
                PermissionResult::Granted => {}
                // Explicitly denied — final, never prompts.
                PermissionResult::Denied => {
                    return error_result(
                        call,
                        format!("[TOOL_ERROR] permission denied for '{}'", call.name),
                    );
                }
                // NotConfigured → raise a grant card and wait for a decision.
                PermissionResult::NotConfigured => {
                    let addon_id = call
                        .name
                        .split_once('.')
                        .map(|(a, _)| a)
                        .unwrap_or(&call.name);
                    let (decision, waited) = crate::agents::run_permission_request(
                        &registry,
                        manager,
                        ctx.progress.as_ref(),
                        &ctx.progress_scope,
                        run_id,
                        parent_run_id,
                        addon_id,
                        &call.name,
                        "llm",
                        DEFAULT_PERMISSION_TIMEOUT,
                    )
                    .await;
                    ctx.extend_deadline(waited);
                    record_permission_decision(service, &user_id, &call.name, decision);

                    use crate::agents::PermissionDecision;
                    match decision {
                        PermissionDecision::Deny => {
                            return error_result(
                                call,
                                format!("[TOOL_ERROR] permission denied for '{}'", call.name),
                            );
                        }
                        PermissionDecision::AllowOnce => preauthorized = true,
                        PermissionDecision::AllowForRun => {
                            if !run_id.is_empty() {
                                registry.grant_for_run(run_id, &call.name);
                            }
                            preauthorized = true;
                        }
                        PermissionDecision::Always => {
                            // Persist a principal-scoped grant; the refreshed
                            // checker lets the normal path through (not pre-auth).
                            if let Err(e) = service.persist_tool_grant(
                                &call.name,
                                &user_id,
                                false,
                                Some(&user_id),
                            ) {
                                return error_result(call, format!("failed to persist grant: {e}"));
                            }
                        }
                    }
                }
            }
        }

        // Dispatch on a blocking thread (wasmtime, §2.12). Pre-authorized retries
        // (AllowOnce / AllowForRun) skip the in-line permission re-check.
        let service = service.clone();
        let name = call.name.clone();
        let call_for_err = call.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            if preauthorized {
                service.dispatch_addon_tool_preauthorized(&name, arguments, &user_id)
            } else {
                service.dispatch_addon_tool(&name, arguments, &user_id)
            }
        })
        .await;
        match outcome {
            Ok(Ok(output)) => ToolCallResult {
                tool_call_id: call.id.clone(),
                name: call.name.clone(),
                content: serde_json::to_string(&output).unwrap_or_default(),
                success: true,
            },
            Ok(Err(e)) => error_result(call, e.to_string()),
            Err(e) => error_result(&call_for_err, format!("tool dispatch join failed: {e}")),
        }
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
        // `full` is the blocking output (one envelope per iteration / final
        // turn). `stream` marks this node as an inline loop region's exit-side
        // stream producer: when a region's exit wires its `stream` port, the
        // executor drives the region's streaming runner (the real tokens come
        // from the region's `llm` member), forwarding live deltas through here.
        // tool_exec itself never produces tokens — the port exists so a region
        // exit can be wired as the producer (R3/R7) without a synthetic node.
        vec![
            PortSpec::new("full", FlowDataType::Any),
            PortSpec::new("stream", FlowDataType::Text),
        ]
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

        let max_result_chars =
            Self::config_usize(node, "max_result_chars", DEFAULT_MAX_RESULT_CHARS);
        let max_tool_calls =
            Self::config_usize(node, "max_tool_calls_per_iteration", DEFAULT_MAX_TOOL_CALLS);

        let mut out: FlowEnvelope = (**envelope).clone();
        let mut calls = Self::last_assistant_tool_calls(envelope);

        // End detection: an assistant turn without tool calls is the final
        // response — signal the loop to stop (§1.1, §3.4).
        if calls.is_empty() {
            out.meta
                .insert("harness_done".into(), serde_json::json!(true));
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

        // The calling run's identity (sub-agent control calls act under it).
        let run_id = envelope
            .meta
            .get("agent_run_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        // Dispatch every call in order. Each goes to exactly one path:
        //   - core.ask_user → async question interaction (§3.13 A),
        //   - sub-agent control → async AgentRunManager (§3.6),
        //   - addon tool → permission-gated dispatch, which may raise an async
        //     grant card on a NotConfigured deny (§3.13 B),
        //   - other core.* (skill_view) → synchronous DB read.
        // Order is preserved so results line up with the assistant turn for the
        // audit. wasmtime dispatch is offloaded per call to a blocking thread.
        let manager = crate::agents::agent_run_manager_global();
        let mut results: Vec<ToolCallResult> = Vec::with_capacity(calls.len());

        let caller = CallerRun::from_envelope(envelope, principal.clone(), ctx.session_id.clone());
        // Parent chain for bubbling a child's question to the same principal
        // (§3.13 A): the dashboard sees the parent_run_id so the ask is visibly
        // attributed up the spawn tree.
        let parent_run_id = if run_id.is_empty() {
            None
        } else {
            repository::get_agent_run(service.db(), &run_id)
                .ok()
                .flatten()
                .and_then(|r| r.parent_run_id)
        };

        for call in &calls {
            ctx.progress.emit(
                &ctx.progress_scope,
                ProgressEvent::ToolCallStarted {
                    name: call.name.clone(),
                },
            );

            let result = if !service.tool_allowed(&tools_json, &call.name) {
                error_result(call, format!("tool '{}' not in agent allowlist", call.name))
            } else if CoreToolName::from_public_name(&call.name)
                .map(|c| c.is_ask_user())
                .unwrap_or(false)
            {
                Self::run_ask_user_call(
                    manager.as_deref(),
                    ctx,
                    &run_id,
                    parent_run_id.as_deref(),
                    call,
                )
                .await
            } else if Self::is_subagent_control(&call.name) {
                match (&manager, run_id.is_empty()) {
                    (Some(mgr), false) => Self::run_manager_call(mgr, &caller, call).await,
                    (Some(_), true) => error_result(
                        call,
                        "sub-agent control requires a managed run context".to_string(),
                    ),
                    (None, _) => error_result(
                        call,
                        "sub-agent control is not available on this node".to_string(),
                    ),
                }
            } else if Self::is_project_knowledge(&call.name) {
                Self::run_project_knowledge_call(ctx, &principal, call).await
            } else if Self::is_case_save(&call.name) {
                Self::run_case_save_call(ctx, &principal, envelope, call).await
            } else {
                Self::run_tool_call(
                    &service,
                    manager.as_deref(),
                    ctx,
                    &principal,
                    &run_id,
                    parent_run_id.as_deref(),
                    call,
                )
                .await
            };

            ctx.progress.emit(
                &ctx.progress_scope,
                ProgressEvent::ToolCallFinished {
                    name: call.name.clone(),
                    status: if result.success { "ok" } else { "error" }.to_string(),
                },
            );
            results.push(result);
        }

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
    use std::sync::Arc;

    fn db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        migrations::run(&conn).expect("migrations");
        Arc::new(crate::db::Db::from_connection(conn))
    }

    fn service(pool: DbPool) -> AgentServiceSlot {
        let cipher = Arc::new(crate::crypto::SettingsCipher::new(&[0u8; 32]));
        let addon_manager =
            Arc::new(crate::addon::AddonManager::new(pool.clone(), cipher).expect("addon manager"));
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
                on_child_complete: "notify",
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
            region: None,
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
        env.context
            .messages
            .push(ChatMessage::assistant("final answer"));
        let ctx = stub_ctx();

        let out = ToolExecNodeAdapter::new(slot)
            .execute(&node(json!({})), &[input(env)], &ctx)
            .await
            .expect("execute");

        assert_eq!(
            out.meta.get("harness_done").and_then(|v| v.as_bool()),
            Some(true)
        );
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
        env.context
            .messages
            .push(assistant_with_calls(vec![LlmToolCall {
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
        env.context
            .messages
            .push(assistant_with_calls(vec![LlmToolCall {
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
        env.context
            .messages
            .push(assistant_with_calls(vec![LlmToolCall {
                id: "c".into(),
                name: "core.skill_view".into(),
                arguments: r#"{"name":"big"}"#.into(),
            }]));
        let ctx = stub_ctx();

        let out = ToolExecNodeAdapter::new(slot)
            .execute(
                &node(json!({"max_result_chars": 2000})),
                &[input(env)],
                &ctx,
            )
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
        let s = "A".repeat(100) + "B".repeat(100).as_str();
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
        assert_eq!(
            ToolExecNodeAdapter::truncate_middle_out("hi".into(), 3),
            "hi"
        );
    }

    #[tokio::test]
    async fn ask_user_call_delivers_wrapped_reply() {
        // An allowlisted core.ask_user call raises a question; the operator's
        // reply lands wrapped in the tool result (§3.13 A).
        let pool = db();
        seed_agent(&pool, "agent-ask", r#"["core.ask_user"]"#);
        let slot = service(pool);

        let mut env = FlowEnvelope::empty();
        env.meta.insert("agent_id".into(), json!("agent-ask"));
        env.context
            .messages
            .push(assistant_with_calls(vec![LlmToolCall {
                id: "ask-1".into(),
                name: "core.ask_user".into(),
                arguments: r#"{"question":"proceed?","choices":["yes","no"]}"#.into(),
            }]));
        let ctx = stub_ctx();

        let adapter = ToolExecNodeAdapter::new(slot);
        let exec =
            tokio::spawn(
                async move { adapter.execute(&node(json!({})), &[input(env)], &ctx).await },
            );

        // Resolve the single pending question.
        let reg = crate::agents::interaction_registry_global();
        let id = loop {
            if let Some(p) = reg
                .list_for(true, &[])
                .iter()
                .find(|p| p.prompt == "proceed?")
            {
                break p.id.clone();
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        };
        assert!(reg.reply(
            &id,
            crate::agents::InteractionReply::Question(crate::agents::QuestionReply {
                answer: "yes".into()
            })
        ));

        let out = exec.await.expect("join").expect("execute");
        let tool_msg = out
            .context
            .messages
            .iter()
            .find(|m| m.role == ChatRole::Tool && m.tool_call_id.as_deref() == Some("ask-1"))
            .expect("ask_user tool message");
        if let ChatMessageContent::Text(t) = &tool_msg.content {
            assert!(t.contains("trusted user channel"), "got: {t}");
            assert!(t.contains("yes"));
            assert!(t.contains("choices_offered"));
        } else {
            panic!("tool content must be text");
        }
    }

    #[tokio::test]
    async fn ask_user_call_outside_allowlist_is_rejected() {
        // ask_user is NOT in subagent allowlists by default — an agent without
        // core.ask_user gets a rejection, not a question (§3.13 A).
        let pool = db();
        seed_agent(&pool, "agent-noask", r#"["core.skill_view"]"#);
        let slot = service(pool);

        let mut env = FlowEnvelope::empty();
        env.meta.insert("agent_id".into(), json!("agent-noask"));
        env.context
            .messages
            .push(assistant_with_calls(vec![LlmToolCall {
                id: "ask-2".into(),
                name: "core.ask_user".into(),
                arguments: r#"{"question":"hi"}"#.into(),
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
            .expect("tool message");
        if let ChatMessageContent::Text(t) = &tool_msg.content {
            assert!(t.contains("not in agent allowlist"), "got: {t}");
        } else {
            panic!("tool content must be text");
        }
    }
}
