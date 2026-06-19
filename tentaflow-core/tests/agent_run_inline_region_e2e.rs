// =============================================================================
// File: tests/agent_run_inline_region_e2e.rs — single-graph "Agent Run" harness
// (inline loop region) end-to-end.
//
// Two complementary layers:
//   1. The seeded `agent_run_flow_json()` graph compiles with the production
//      adapter set and passes R1–R11, with the `agent_turn` region resolved to
//      entry=compact_context / exit=tool_exec and the parsed budget/final_pass.
//   2. A realistic execution of the same region shape over ONE envelope:
//      real `llm` adapter (a tool-calling stub LlmDispatcher: tool calls on the
//      first turn, a plain final answer on the second), real `tool_exec`
//      (real AgentService + agent allowlist + a real `core.skill_view` tool),
//      real `conversation_history`/`persist_turn` over the durable SQLite
//      `ConversationHistoryImpl`. Asserts the region ran exactly 2 iterations,
//      the structural stop fired (last assistant has no tool calls), the turn
//      delta was persisted to `conversation_messages`, and the whole flow wrote
//      exactly one `flow_executions` row (no per-iteration trace rows — the
//      region runs inline).
//
// The execution flow deliberately omits the `agent_context` node: it would
// create an agent_runs row and resolve a model/budget against a live
// AgentService, which is exercised by its own unit tests. Here it is replaced by
// seeding the same envelope meta (`agent_id`, `harness_tools`, `model`) that
// agent_context would emit, so the loop body (compact_context → llm → tool_exec)
// and persistence are tested for real without that heavier dependency. Layer 1
// keeps the FULL seeded graph (agent_context included) honest against R1–R11.
// =============================================================================

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use serde_json::json;

use tentaflow_core::agents::{AgentService, AgentServiceSlot};
use tentaflow_core::db::{init as db_init, models::AgentParams, models::SkillParams, repository, DbPool};
use tentaflow_core::flow_engine::cache::CompiledFlow;
use tentaflow_core::flow_engine::dispatchers_impl::ConversationHistoryImpl;
use tentaflow_core::flow_engine::envelope::{
    ChatRole, FinishReason, FlowEnvelope, FlowValue, LlmStreamChunk, LlmToolCall, TokenUsage,
};
use tentaflow_core::flow_engine::envelope::{EnvelopeDelta, ToolCallDelta};
use tentaflow_core::flow_engine::executor::{execute_blocking, execute_streaming};
use tentaflow_core::flow_engine::node_adapter::test_support::stub_ctx;
use tentaflow_core::flow_engine::node_adapter::AdapterRegistry;
use tentaflow_core::flow_engine::node_adapters::{
    CompactContextNodeAdapter, ConversationHistoryNodeAdapter, LlmNodeAdapter, OutputNodeAdapter,
    PersistTurnNodeAdapter, ToolExecNodeAdapter, TriggerNodeAdapter,
};
use tentaflow_core::flow_engine::dispatchers::{LlmDispatcher, LlmRequest, LlmResponse};
use tentaflow_core::flow_engine::validation::validate;
use tentaflow_core::db::seed::agent_run_flow_json;

// -----------------------------------------------------------------------------
// Layer 1 — the seeded single graph compiles and validates.
// -----------------------------------------------------------------------------

/// Production adapter set needed to validate + compile the full "Agent Run"
/// graph (agent_context/tool_exec/agent_router need a service slot, which can be
/// empty: validation/compile never call into the slot — only `execute` does).
fn full_registry(slot: AgentServiceSlot) -> AdapterRegistry {
    let mut r = AdapterRegistry::new();
    r.register(Arc::new(TriggerNodeAdapter::new()));
    r.register(Arc::new(OutputNodeAdapter::new()));
    r.register(Arc::new(ConversationHistoryNodeAdapter::new()));
    r.register(Arc::new(PersistTurnNodeAdapter::new()));
    r.register(Arc::new(CompactContextNodeAdapter::new()));
    r.register(Arc::new(
        tentaflow_core::flow_engine::node_adapters::AgentContextNodeAdapter::new(slot.clone()),
    ));
    r.register(Arc::new(ToolExecNodeAdapter::new(slot.clone())));
    r.register(Arc::new(
        tentaflow_core::flow_engine::node_adapters::AgentRouterNodeAdapter::new(slot),
    ));
    r.register_llm(Arc::new(LlmNodeAdapter::new()));
    r
}

fn empty_slot() -> AgentServiceSlot {
    Arc::new(parking_lot::RwLock::new(None))
}

#[test]
fn seeded_agent_run_graph_validates_and_compiles_with_region() {
    let reg = full_registry(empty_slot());
    let json = agent_run_flow_json();

    // R1–R11 must pass on the exact seeded graph.
    let def = serde_json::from_str(&json).expect("agent_run_flow_json parses");
    validate(&def, &reg).expect("seeded Agent Run must pass R1–R11");

    // Compile resolves the single inline loop region.
    let compiled = CompiledFlow::from_json(
        "00000000-0000-4000-8000-000000000012",
        &json,
        &reg,
    )
    .expect("seeded Agent Run must compile");

    assert_eq!(compiled.regions.len(), 1, "exactly one inline loop region");
    let region = &compiled.regions[0];
    assert_eq!(region.id, "agent_turn");

    // Entry is compact_context (region config holder), exit is tool_exec
    // (loop_back source + edge to persist_turn).
    let entry_def = compiled.execution_order[region.entry_pos];
    let exit_def = compiled.execution_order[region.exit_pos];
    assert_eq!(compiled.definition.nodes[entry_def].node_type, "compact_context");
    assert_eq!(compiled.definition.nodes[exit_def].node_type, "tool_exec");

    // The region body is exactly the three marked nodes.
    assert_eq!(region.member_pos.len(), 3);

    // Budget + final_pass parsed from the entry node config.
    assert_eq!(region.max_iterations, 25);
    assert!(region.final_pass);
}

// -----------------------------------------------------------------------------
// Layer 2 — realistic execution of the region body over one envelope.
// -----------------------------------------------------------------------------

/// Tool-calling LlmDispatcher double. Call 1 returns an assistant turn that
/// invokes `core.skill_view` (the loop must continue and run the tool). Call 2
/// returns a plain final answer with no tool calls (the region's structural
/// stop). `execute_chat` is the only path the blocking llm adapter uses.
struct ScriptedLlm {
    calls: AtomicUsize,
}

impl ScriptedLlm {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl LlmDispatcher for ScriptedLlm {
    async fn execute_chat(&self, _req: LlmRequest) -> Result<LlmResponse> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            Ok(LlmResponse {
                content: String::new(),
                usage: TokenUsage::default(),
                finish_reason: FinishReason::ToolCalls,
                tool_calls: vec![LlmToolCall {
                    id: "call-1".into(),
                    name: "core.skill_view".into(),
                    arguments: r#"{"name":"do-thing"}"#.into(),
                }],
            })
        } else {
            Ok(LlmResponse {
                content: "final answer".into(),
                usage: TokenUsage::default(),
                finish_reason: FinishReason::Stop,
                tool_calls: Vec::new(),
            })
        }
    }

    async fn stream_chat(
        &self,
        _req: LlmRequest,
    ) -> Result<BoxStream<'static, Result<LlmStreamChunk>>> {
        unreachable!("blocking flow never streams");
    }
}

fn execution_registry(slot: AgentServiceSlot, llm: Arc<ScriptedLlm>) -> Arc<AdapterRegistry> {
    let mut r = AdapterRegistry::new();
    r.register(Arc::new(TriggerNodeAdapter::new()));
    r.register(Arc::new(OutputNodeAdapter::new()));
    r.register(Arc::new(ConversationHistoryNodeAdapter::new()));
    r.register(Arc::new(PersistTurnNodeAdapter::new()));
    r.register(Arc::new(CompactContextNodeAdapter::new()));
    r.register(Arc::new(ToolExecNodeAdapter::new(slot)));
    r.register_llm(Arc::new(LlmNodeAdapter::new()));
    let _ = llm; // the dispatcher is wired via ctx.llm, not the registry.
    Arc::new(r)
}

/// Execution flow = the region body plus history read/persist, with the
/// `agent_context` node replaced by pre-seeded meta. Entry compact_context,
/// exit tool_exec, back edge tool_exec→compact_context, then persist_turn.
/// compact_context's threshold is set high so it never summarises here (the
/// turn count is small) — the loop mechanics, tool execution and persistence
/// are what this test exercises.
fn execution_flow_json() -> serde_json::Value {
    json!({
        "nodes": [
            {"id": "t1", "type": "trigger", "config": {}},
            {"id": "h1", "type": "conversation_history", "config": {"max_messages": 20}},
            {"id": "k1", "type": "compact_context", "region": "agent_turn",
             "config": {"threshold_percent": 99, "protect_last_messages": 4, "summary_model": "",
                        "loop_max_iterations": 25, "loop_final_pass": true}},
            {"id": "m1", "type": "llm", "region": "agent_turn",
             "config": {"model": "", "temperature": 0.0, "max_tokens": 256, "stream": false}},
            {"id": "x1", "type": "tool_exec", "region": "agent_turn",
             "config": {"max_result_chars": 16000, "max_tool_calls_per_iteration": 16}},
            {"id": "p1", "type": "persist_turn", "config": {}},
            {"id": "o1", "type": "output", "config": {"format": "text"}}
        ],
        "edges": [
            {"from_node": "t1", "to_node": "h1", "from_port": "text", "data_type": "text"},
            {"from_node": "h1", "to_node": "k1"},
            {"from_node": "k1", "to_node": "m1", "to_port": "in"},
            {"from_node": "m1", "to_node": "x1", "from_port": "full"},
            {"from_node": "x1", "to_node": "k1", "kind": "loop_back"},
            {"from_node": "x1", "to_node": "p1", "from_port": "full"},
            {"from_node": "p1", "to_node": "o1", "to_port": "text"}
        ]
    })
}

fn test_db() -> DbPool {
    db_init(Path::new(":memory:")).expect("init db")
}

fn service_slot(pool: DbPool) -> AgentServiceSlot {
    let cipher = Arc::new(tentaflow_core::crypto::SettingsCipher::new(&[0u8; 32]));
    let addon_manager =
        Arc::new(tentaflow_core::addon::AddonManager::new(pool.clone(), cipher).expect("addon mgr"));
    let svc = Arc::new(AgentService::new(pool, addon_manager));
    Arc::new(parking_lot::RwLock::new(Some(svc)))
}

fn seed_agent(pool: &DbPool, id: &str) {
    repository::upsert_agent(
        pool,
        &AgentParams {
            id,
            name: "a",
            display_name: None,
            description: "d",
            system_prompt: None,
            model: None,
            tools_json: r#"["core.skill_view"]"#,
            skills_json: "{}",
            params_json: "{}",
            max_iterations: 25,
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

fn count_conversation_rows(pool: &DbPool, session: &str) -> i64 {
    let conn = pool.read().unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM conversation_messages WHERE session_id = ?1",
        rusqlite::params![session],
        |r| r.get(0),
    )
    .unwrap()
}

fn count_flow_executions(pool: &DbPool) -> i64 {
    let conn = pool.read().unwrap();
    conn.query_row("SELECT COUNT(*) FROM flow_executions", [], |r| r.get(0))
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn region_runs_two_iterations_persists_turn_and_stops_structurally() {
    let pool = test_db();
    seed_skill(&pool, "11111111-0000-0000-0000-0000000000aa", "do-thing");
    seed_agent(&pool, "agent-1");
    let slot = service_slot(pool.clone());

    let llm = Arc::new(ScriptedLlm::new());
    let reg = execution_registry(slot, llm.clone());
    let compiled = Arc::new(
        CompiledFlow::from_json(
            "00000000-0000-4000-8000-000000000012",
            &execution_flow_json().to_string(),
            &reg,
        )
        .expect("execution flow compiles"),
    );

    // Initial envelope: the user turn + the meta agent_context would have set
    // (agent_id for the tool allowlist, harness_tools the model may call, model
    // so the llm adapter builds a request).
    let session = "sess-region-e2e";
    let mut initial = FlowEnvelope::empty();
    initial.payload = FlowValue::Text("do the thing".into());
    initial
        .context
        .messages
        .push(tentaflow_core::flow_engine::envelope::ChatMessage::user("do the thing"));
    initial.meta.insert("agent_id".into(), json!("agent-1"));
    initial.meta.insert("model".into(), json!("scripted-test-model"));
    initial.meta.insert(
        "harness_tools".into(),
        json!([{
            "name": "core.skill_view",
            "description": "view a skill",
            "parameters": {"type": "object"}
        }]),
    );

    let mut ctx = stub_ctx();
    ctx.session_id = Some(session.to_string());
    ctx.history = Arc::new(ConversationHistoryImpl::new(pool.clone()));
    ctx.llm = llm;

    let outcome = execute_blocking(pool.clone(), compiled, initial, ctx, reg)
        .await
        .expect("execute_blocking");
    assert!(outcome.error.is_none(), "flow error: {:?}", outcome.error);

    // The region ran exactly two iterations: turn 1 (tool call) + turn 2 (final).
    assert_eq!(
        outcome
            .final_envelope
            .meta
            .get("loop_iterations")
            .and_then(|v| v.as_i64()),
        Some(2),
        "region must run exactly 2 iterations"
    );
    // Structural stop, not budget/cancel.
    assert_eq!(
        outcome
            .final_envelope
            .meta
            .get("loop_exit_reason")
            .and_then(|v| v.as_str()),
        Some("no_tool_calls")
    );

    // The single envelope accumulated the full turn in order:
    // user → assistant+tool_calls → tool result → assistant(final).
    let roles: Vec<ChatRole> = outcome
        .final_envelope
        .context
        .messages
        .iter()
        .map(|m| m.role)
        .collect();
    assert_eq!(
        roles,
        vec![
            ChatRole::User,
            ChatRole::Assistant,
            ChatRole::Tool,
            ChatRole::Assistant,
        ],
        "messages: {:?}",
        outcome.final_envelope.context.messages
    );
    // First assistant requested the tool; last assistant is the final answer.
    let assistants: Vec<&_> = outcome
        .final_envelope
        .context
        .messages
        .iter()
        .filter(|m| m.role == ChatRole::Assistant)
        .collect();
    assert!(assistants[0].tool_calls.as_ref().is_some_and(|c| !c.is_empty()));
    assert!(assistants[1].tool_calls.as_ref().map(|c| c.is_empty()).unwrap_or(true));
    assert_eq!(assistants[1].text(), Some("final answer"));

    // The tool message carries the real core.skill_view output (proves a real
    // tool ran, not a stub).
    let tool_msg = outcome
        .final_envelope
        .context
        .messages
        .iter()
        .find(|m| m.role == ChatRole::Tool)
        .expect("a tool result message");
    assert_eq!(tool_msg.tool_call_id.as_deref(), Some("call-1"));
    assert!(tool_msg
        .text()
        .is_some_and(|t| t.contains("full instructions")));

    // persist_turn wrote the whole live turn delta durably: history was empty,
    // so base=0 and all four messages are persisted to conversation_messages.
    assert_eq!(
        count_conversation_rows(&pool, session),
        4,
        "persist_turn must write the 4-message turn delta"
    );

    // Exactly one flow_executions row: the region iterations run inline, not as
    // separate child executions (light region — no per-iteration trace rows).
    assert_eq!(
        count_flow_executions(&pool),
        1,
        "the whole agent run is one flow_executions row"
    );
}

// -----------------------------------------------------------------------------
// Layer 3 — codex-style live token streaming through the inline region.
// -----------------------------------------------------------------------------

/// Streaming tool-calling LlmDispatcher double. Call 1 streams a narration text
/// delta followed by a tool-call delta for `core.skill_view` (the loop must run
/// the tool and continue). Call 2 streams the final answer text in two deltas
/// with no tool calls (the structural stop). `stream_chat` is the only path the
/// region's streaming llm member uses.
struct ScriptedStreamingLlm {
    calls: AtomicUsize,
}

impl ScriptedStreamingLlm {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl LlmDispatcher for ScriptedStreamingLlm {
    async fn execute_chat(&self, _req: LlmRequest) -> Result<LlmResponse> {
        unreachable!("streaming flow never calls execute_chat");
    }

    async fn stream_chat(
        &self,
        _req: LlmRequest,
    ) -> Result<BoxStream<'static, Result<LlmStreamChunk>>> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks: Vec<LlmStreamChunk> = if n == 0 {
            // Turn 1: visible narration, then a tool call (no finish text yet).
            vec![
                LlmStreamChunk {
                    text_delta: "Let me check the skill. ".into(),
                    ..Default::default()
                },
                LlmStreamChunk {
                    tool_calls: vec![
                        ToolCallDelta {
                            index: 0,
                            id: Some("call-1".into()),
                            function_name: Some("core.skill_view".into()),
                            arguments_delta: Some(r#"{"name":"do-thing"}"#.into()),
                        },
                    ],
                    finish_reason: Some(FinishReason::ToolCalls),
                    ..Default::default()
                },
            ]
        } else {
            // Turn 2: the final answer, streamed in two deltas, no tool calls.
            vec![
                LlmStreamChunk {
                    text_delta: "The answer is ".into(),
                    ..Default::default()
                },
                LlmStreamChunk {
                    text_delta: "forty-two.".into(),
                    finish_reason: Some(FinishReason::Stop),
                    ..Default::default()
                },
            ]
        };
        Ok(futures::stream::iter(chunks.into_iter().map(Ok)).boxed())
    }
}

/// The seeded streaming shape, with `agent_context` replaced by pre-seeded meta
/// (same simplification as the blocking layer): entry compact_context, exit
/// tool_exec, back edge tool_exec→compact_context. The region exit streams to
/// output(mode=stream); its `full` port feeds persist_turn on the blocking
/// finalizer path.
fn streaming_flow_json() -> serde_json::Value {
    json!({
        "nodes": [
            {"id": "t1", "type": "trigger", "config": {}},
            {"id": "h1", "type": "conversation_history", "config": {"max_messages": 20}},
            {"id": "k1", "type": "compact_context", "region": "agent_turn",
             "config": {"threshold_percent": 99, "protect_last_messages": 4, "summary_model": "",
                        "loop_max_iterations": 25, "loop_final_pass": true}},
            {"id": "m1", "type": "llm", "region": "agent_turn",
             "config": {"model": "", "temperature": 0.0, "max_tokens": 256, "stream": true}},
            {"id": "x1", "type": "tool_exec", "region": "agent_turn",
             "config": {"max_result_chars": 16000, "max_tool_calls_per_iteration": 16}},
            {"id": "p1", "type": "persist_turn", "config": {}},
            {"id": "o1", "type": "output", "config": {"mode": "stream"}}
        ],
        "edges": [
            {"from_node": "t1", "to_node": "h1", "from_port": "text", "data_type": "text"},
            {"from_node": "h1", "to_node": "k1"},
            {"from_node": "k1", "to_node": "m1", "to_port": "in"},
            {"from_node": "m1", "to_node": "x1", "from_port": "full"},
            {"from_node": "x1", "to_node": "k1", "kind": "loop_back"},
            {"from_node": "x1", "to_node": "p1", "from_port": "full"},
            {"from_node": "x1", "to_node": "o1", "from_port": "stream", "to_port": "text"},
            {"from_node": "p1", "to_node": "o1", "to_port": "text"}
        ]
    })
}

fn streaming_registry(slot: AgentServiceSlot) -> Arc<AdapterRegistry> {
    let mut r = AdapterRegistry::new();
    r.register(Arc::new(TriggerNodeAdapter::new()));
    r.register(Arc::new(OutputNodeAdapter::new()));
    r.register(Arc::new(ConversationHistoryNodeAdapter::new()));
    r.register(Arc::new(PersistTurnNodeAdapter::new()));
    r.register(Arc::new(CompactContextNodeAdapter::new()));
    r.register(Arc::new(ToolExecNodeAdapter::new(slot)));
    r.register_llm(Arc::new(LlmNodeAdapter::new()));
    Arc::new(r)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn region_streams_tokens_live_persists_turn_and_stops_structurally() {
    let pool = test_db();
    seed_skill(&pool, "11111111-0000-0000-0000-0000000000aa", "do-thing");
    seed_agent(&pool, "agent-1");
    let slot = service_slot(pool.clone());

    let llm = Arc::new(ScriptedStreamingLlm::new());
    let reg = streaming_registry(slot);
    let compiled = Arc::new(
        CompiledFlow::from_json(
            "00000000-0000-4000-8000-000000000012",
            &streaming_flow_json().to_string(),
            &reg,
        )
        .expect("streaming flow compiles"),
    );
    assert!(compiled.is_streaming, "flow must be detected as streaming");

    let session = "sess-region-stream-e2e";
    let mut initial = FlowEnvelope::empty();
    initial.payload = FlowValue::Text("do the thing".into());
    initial
        .context
        .messages
        .push(tentaflow_core::flow_engine::envelope::ChatMessage::user("do the thing"));
    initial.meta.insert("agent_id".into(), json!("agent-1"));
    initial.meta.insert("model".into(), json!("scripted-test-model"));
    initial.meta.insert(
        "harness_tools".into(),
        json!([{
            "name": "core.skill_view",
            "description": "view a skill",
            "parameters": {"type": "object"}
        }]),
    );

    let mut ctx = stub_ctx();
    ctx.session_id = Some(session.to_string());
    ctx.history = Arc::new(ConversationHistoryImpl::new(pool.clone()));
    ctx.llm = llm.clone();

    let exec = execute_streaming(pool.clone(), compiled, initial, ctx, reg)
        .await
        .expect("execute_streaming");

    // Collect the live token deltas in order. Only the narration (turn 1) and the
    // final answer (turn 2) text deltas are forwarded; tool-call deltas stay
    // internal. A terminal trailer carries finish_reason=Stop.
    let mut text_deltas: Vec<String> = Vec::new();
    let mut concat = String::new();
    let mut final_finish: Option<FinishReason> = None;
    let mut stream = exec.stream;
    while let Some(item) = stream.next().await {
        let delta = item.expect("delta ok");
        match delta {
            EnvelopeDelta::Llm(c) => {
                if !c.text_delta.is_empty() {
                    text_deltas.push(c.text_delta.clone());
                    concat.push_str(&c.text_delta);
                }
                if let Some(fr) = c.finish_reason {
                    final_finish = Some(fr);
                }
            }
            EnvelopeDelta::Audio(_) => panic!("region text stream emitted an audio delta"),
        }
    }

    // Live ordering: turn-1 narration arrives before the turn-2 final answer.
    let narration_idx = text_deltas
        .iter()
        .position(|t| t.contains("check the skill"))
        .expect("narration delta from iteration 1");
    let final_idx = text_deltas
        .iter()
        .position(|t| t.contains("forty-two"))
        .expect("final answer delta from iteration 2");
    assert!(
        narration_idx < final_idx,
        "iteration-1 narration must stream before the iteration-2 final answer: {text_deltas:?}"
    );
    assert!(
        concat.contains("Let me check the skill.") && concat.contains("The answer is forty-two."),
        "client did not receive both turns token-by-token: {concat:?}"
    );
    assert_eq!(
        final_finish,
        Some(FinishReason::Stop),
        "client must see a terminal finish_reason=Stop"
    );

    // The scripted llm streamed exactly twice (turn 1 tool call + turn 2 final).
    assert_eq!(llm.calls.load(Ordering::SeqCst), 2, "region must run 2 iterations");

    // The outcome carries the fully accumulated turn:
    // user → assistant+tool_calls → tool → assistant(final).
    let outcome = exec.outcome.await.expect("outcome");
    assert!(outcome.error.is_none(), "flow error: {:?}", outcome.error);
    assert_eq!(outcome.finish_reason, FinishReason::Stop);
    assert_eq!(
        outcome
            .final_envelope
            .meta
            .get("loop_iterations")
            .and_then(|v| v.as_i64()),
        Some(2),
        "region must run exactly 2 iterations"
    );
    assert_eq!(
        outcome
            .final_envelope
            .meta
            .get("loop_exit_reason")
            .and_then(|v| v.as_str()),
        Some("no_tool_calls")
    );
    let roles: Vec<ChatRole> = outcome
        .final_envelope
        .context
        .messages
        .iter()
        .map(|m| m.role)
        .collect();
    assert_eq!(
        roles,
        vec![
            ChatRole::User,
            ChatRole::Assistant,
            ChatRole::Tool,
            ChatRole::Assistant,
        ],
        "messages: {:?}",
        outcome.final_envelope.context.messages
    );
    let assistants: Vec<&_> = outcome
        .final_envelope
        .context
        .messages
        .iter()
        .filter(|m| m.role == ChatRole::Assistant)
        .collect();
    // Turn-1 assistant carries the reassembled tool call (from streamed deltas).
    let first_calls = assistants[0]
        .tool_calls
        .as_ref()
        .expect("turn-1 assistant has tool calls");
    assert_eq!(first_calls.len(), 1);
    assert_eq!(first_calls[0].id, "call-1");
    assert_eq!(first_calls[0].name, "core.skill_view");
    assert_eq!(first_calls[0].arguments, r#"{"name":"do-thing"}"#);
    // Turn-2 assistant is the final streamed answer, no tool calls.
    assert!(assistants[1]
        .tool_calls
        .as_ref()
        .map(|c| c.is_empty())
        .unwrap_or(true));
    assert_eq!(assistants[1].text(), Some("The answer is forty-two."));

    // The real tool ran (proves tool_exec executed on the streaming path).
    let tool_msg = outcome
        .final_envelope
        .context
        .messages
        .iter()
        .find(|m| m.role == ChatRole::Tool)
        .expect("a tool result message");
    assert_eq!(tool_msg.tool_call_id.as_deref(), Some("call-1"));
    assert!(tool_msg
        .text()
        .is_some_and(|t| t.contains("full instructions")));

    // persist_turn ran on the blocking finalizer path over the accumulated turn:
    // the full 4-message delta is durable.
    assert_eq!(
        count_conversation_rows(&pool, session),
        4,
        "persist_turn must write the 4-message turn delta on the streaming path"
    );

    // One flow_executions row for the whole streaming run.
    assert_eq!(count_flow_executions(&pool), 1);
}
