// =============================================================================
// File: tests/code_harness_flow_e2e.rs — Code Studio "Code Harness" (§16.2,
// §16.5, §24 "Flow").
//
// §16.5 makes graph validation the FIRST task of the phase: the real `flow_json`
// goes through `FlowDefinition` → R1–R11 → `CompiledFlow` before any adapter is
// written. This file is that gate, plus the behavioural claims the graph makes:
//
//   * the harness validates and compiles with the production adapter set;
//   * every back edge closes its own region, so nothing outside the loops forms
//     a cycle (a compile that finds one is a hard error, so compiling IS the
//     proof);
//   * the `code_turn` region ends on a turn WITHOUT tool calls — the structural
//     stop — rather than exhausting `max_iterations` (the thesis of §16.1);
//   * a turn that called tools always walks the planner, implementer, tester
//     and critics, because the pipeline is topology and not a model decision,
//     while a turn that called none skips it;
//   * the roster's separation of duties lives in `tools_json`, not the prompt.
// =============================================================================

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::BoxStream;
use serde_json::json;

use tentaflow_core::agents::{
    tool_in_allowlist, AgentPrincipal, AgentService, AgentServiceSlot, CoreToolName, ToolCatalog,
};
use tentaflow_core::db::seed::{code_harness_flow_json, CODE_HARNESS_FLOW_ID};
use tentaflow_core::db::{init as db_init, repository, DbPool};
use tentaflow_core::flow_engine::cache::CompiledFlow;
use tentaflow_core::flow_engine::dispatchers::{LlmDispatcher, LlmRequest, LlmResponse};
use tentaflow_core::flow_engine::envelope::{
    ChatMessage, FinishReason, FlowEnvelope, FlowValue, LlmStreamChunk, LlmToolCall, NodeInput,
    TokenUsage,
};
use tentaflow_core::flow_engine::executor::execute_blocking;
use tentaflow_core::flow_engine::node_adapter::test_support::stub_ctx;
use tentaflow_core::flow_engine::node_adapter::{AdapterRegistry, PortSpec};
use tentaflow_core::flow_engine::types::{FlowDataType, FlowNode};
use tentaflow_core::flow_engine::validation::validate;

// -----------------------------------------------------------------------------
// Shared fixtures
// -----------------------------------------------------------------------------

fn test_db() -> DbPool {
    db_init(Path::new(":memory:")).expect("init db")
}

fn service_slot(pool: DbPool) -> AgentServiceSlot {
    let cipher = Arc::new(tentaflow_core::crypto::SettingsCipher::new(&[0u8; 32]));
    let addon_manager = Arc::new(
        tentaflow_core::addon::AddonManager::new(pool.clone(), cipher).expect("addon mgr"),
    );
    Arc::new(parking_lot::RwLock::new(Some(Arc::new(AgentService::new(
        pool,
        addon_manager,
    )))))
}

/// Every adapter the harness graph names. Validation and compilation never
/// enter an adapter's `execute`, so the empty service slot is enough — what is
/// being checked here is the port contract each block declares.
fn harness_registry() -> AdapterRegistry {
    use tentaflow_core::flow_engine::node_adapters::*;
    let slot: AgentServiceSlot = Arc::new(parking_lot::RwLock::new(None));
    let mut r = AdapterRegistry::new();
    r.register(Arc::new(TriggerNodeAdapter::new()));
    r.register(Arc::new(OutputNodeAdapter::new()));
    r.register(Arc::new(ConversationHistoryNodeAdapter::new()));
    r.register(Arc::new(PersistTurnNodeAdapter::new()));
    r.register(Arc::new(CompactContextNodeAdapter::new()));
    r.register(Arc::new(ConditionNodeAdapter::new()));
    r.register(Arc::new(CriticGateNodeAdapter::new()));
    r.register(Arc::new(AwaitSubagentsNodeAdapter::new()));
    r.register(Arc::new(PatchReviewNodeAdapter::new(slot.clone())));
    r.register(Arc::new(ExecCommandNodeAdapter::new(slot.clone())));
    r.register(Arc::new(DelegateCliNodeAdapter::new(slot.clone())));
    r.register(Arc::new(AgentContextNodeAdapter::new(slot.clone())));
    r.register(Arc::new(ToolExecNodeAdapter::new(slot.clone())));
    r.register(Arc::new(SpawnNodeAdapter::new(slot.clone())));
    r.register(Arc::new(TaskGateNodeAdapter::new(slot.clone())));
    r.register(Arc::new(WorkspaceContextNodeAdapter::new(slot)));
    r.register_llm(Arc::new(LlmNodeAdapter::new()));
    r
}

// -----------------------------------------------------------------------------
// §16.5 — the graphs, before anything else
// -----------------------------------------------------------------------------

/// Resolves a seeded spawn target back to its roster name. The harness pins
/// every spawn by `agent_id`, because the block schema declares `agent_id` and
/// the Flow Builder validates against the schema — a name would render our own
/// nodes as "missing required: Agent". The assertions stay in names, which is
/// what the separation of duties is stated in.
fn spawned_agent_name(pool: &DbPool, node: &FlowNode) -> String {
    let id = node.config["agent_id"]
        .as_str()
        .unwrap_or_else(|| panic!("spawn '{}' names no agent_id", node.id));
    repository::get_agent(pool, id)
        .expect("query agent")
        .unwrap_or_else(|| panic!("spawn '{}' points at an unseeded agent {id}", node.id))
        .name
}

#[test]
fn the_harness_validates_and_compiles_with_its_three_loops() {
    let pool = test_db();
    let reg = harness_registry();
    let json = code_harness_flow_json();

    let def = serde_json::from_str(&json).expect("the harness parses");
    validate(&def, &reg).expect("the harness must pass R1-R11");

    let compiled =
        CompiledFlow::from_json(CODE_HARNESS_FLOW_ID, &json, &reg).expect("the harness compiles");

    let mut regions: Vec<&str> = compiled.regions.iter().map(|r| r.id.as_str()).collect();
    regions.sort_unstable();
    assert_eq!(regions, vec!["build_review", "code_turn", "plan_review"]);

    let turn = compiled
        .regions
        .iter()
        .find(|r| r.id == "code_turn")
        .expect("code_turn region");
    let entry = &compiled.definition.nodes[compiled.execution_order[turn.entry_pos]];
    let exit = &compiled.definition.nodes[compiled.execution_order[turn.exit_pos]];
    assert_eq!(entry.node_type, "compact_context");
    assert_eq!(exit.node_type, "tool_exec");
    assert_eq!(turn.member_pos.len(), 3);

    // Every delegation is followed by its OWN wait. `spawn` is detached by
    // construction, so without the waits the pipeline would only guarantee
    // that its agents STARTED.
    let spawns: Vec<String> = compiled
        .definition
        .nodes
        .iter()
        .filter(|n| n.node_type == "spawn")
        .map(|n| spawned_agent_name(&pool, n))
        .collect();
    assert_eq!(
        spawns,
        vec![
            "code-planner",
            "code-critic",
            "code-implementer",
            "code-tester",
            "code-critic",
        ]
    );
    let spawn_vars: Vec<&str> = compiled
        .definition
        .nodes
        .iter()
        .filter(|n| n.node_type == "spawn")
        .map(|n| n.config["output_variable"].as_str().unwrap())
        .collect();
    let wait_vars: Vec<&str> = compiled
        .definition
        .nodes
        .iter()
        .filter(|n| n.node_type == "await_subagents")
        .map(|n| n.config["run_ids_var"].as_str().unwrap())
        .collect();
    assert_eq!(wait_vars, spawn_vars);
}

#[test]
fn nothing_outside_the_regions_forms_a_cycle() {
    // The compiler's Kahn sort rejects a cycle, and a region's back edge is
    // excluded from the in-degree — so a successful compile of a graph whose
    // every `loop_back` edge stays inside one region is exactly the property
    // §16.1 demands. Asserting the edge inventory makes the claim explicit
    // rather than implied by the compile above.
    let def: tentaflow_core::flow_engine::types::FlowDefinition =
        serde_json::from_str(&code_harness_flow_json()).expect("parses");
    let region_of = |id: &str| {
        def.nodes
            .iter()
            .find(|n| n.id == id)
            .and_then(|n| n.region.clone())
    };
    let mut closed: Vec<String> = def
        .edges
        .iter()
        .filter(|e| e.is_loop_back())
        .map(|e| {
            let region = region_of(&e.from).expect("back edge leaves a region");
            assert_eq!(region_of(&e.to).as_deref(), Some(region.as_str()));
            region
        })
        .collect();
    closed.sort_unstable();
    assert_eq!(closed, vec!["build_review", "code_turn", "plan_review"]);
}

#[test]
fn the_regions_carry_no_stop_expression() {
    // §16.1 takes `stop_expr` off the list: the structural stop and the critic
    // gates are the only exits, and a second stop mechanism in the config would
    // be a promise the executor does not keep.
    let def: tentaflow_core::flow_engine::types::FlowDefinition =
        serde_json::from_str(&code_harness_flow_json()).expect("parses");
    for node in def.nodes.iter().filter(|n| n.region.is_some()) {
        assert!(
            node.config.get("stop_expr").is_none(),
            "region node '{}' must not carry a stop expression",
            node.id
        );
    }
}

// -----------------------------------------------------------------------------
// §16.1 — the region stops on a turn without tool calls
// -----------------------------------------------------------------------------

/// Answers with a tool call `tool_turns` times, then in prose. With a budget of
/// 25 and two tool turns, an exit reason of `max_iterations` would mean the
/// structural stop never fired.
struct ScriptedLlm {
    calls: AtomicUsize,
    tool_turns: usize,
}

#[async_trait]
impl LlmDispatcher for ScriptedLlm {
    async fn execute_chat(&self, _req: LlmRequest) -> Result<LlmResponse> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n < self.tool_turns {
            Ok(LlmResponse {
                content: String::new(),
                reasoning_content: None,
                usage: TokenUsage::default(),
                finish_reason: FinishReason::ToolCalls,
                audio: None,
                tool_calls: vec![LlmToolCall {
                    id: format!("call-{n}"),
                    name: "core.skill_view".into(),
                    arguments: r#"{"name":"do-thing"}"#.into(),
                }],
            })
        } else {
            Ok(LlmResponse {
                content: "nothing needed changing".into(),
                reasoning_content: None,
                usage: TokenUsage::default(),
                finish_reason: FinishReason::Stop,
                audio: None,
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

/// The `code_turn` region body, with `agent_context` and `workspace_context`
/// replaced by the meta they publish. Both need a live workspace on disk, which
/// their own unit tests cover; what this graph exercises is the LOOP.
fn region_only_flow_json() -> serde_json::Value {
    json!({
        "nodes": [
            {"id": "t1", "type": "trigger", "config": {}},
            {"id": "k1", "type": "compact_context", "region": "code_turn",
             "config": {"threshold_percent": 99, "protect_last_messages": 4,
                        "loop_max_iterations": 25, "loop_final_pass": true}},
            {"id": "m1", "type": "llm", "region": "code_turn",
             "config": {"model": "", "temperature": 0.0, "max_tokens": 256, "stream": false}},
            {"id": "x1", "type": "tool_exec", "region": "code_turn",
             "config": {"max_result_chars": 16000, "max_tool_calls_per_iteration": 16}},
            {"id": "o1", "type": "output", "config": {"format": "text"}}
        ],
        "edges": [
            {"from_node": "t1", "to_node": "k1", "from_port": "text", "data_type": "text"},
            {"from_node": "k1", "to_node": "m1", "to_port": "in"},
            {"from_node": "m1", "to_node": "x1", "from_port": "full"},
            {"from_node": "x1", "to_node": "k1", "kind": "loop_back"},
            {"from_node": "x1", "to_node": "o1", "to_port": "text"}
        ]
    })
}

fn seed_skill(pool: &DbPool, id: &str, name: &str) {
    repository::upsert_skill(
        pool,
        &tentaflow_core::db::models::SkillParams {
            id,
            name,
            display_name: None,
            description: "desc",
            content: "# Skill\ninstructions",
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

fn seed_agent(pool: &DbPool, id: &str, name: &str, tools_json: &str, max_subagents: i64) {
    repository::upsert_agent(
        pool,
        &tentaflow_core::db::models::AgentParams {
            runtime_json: tentaflow_core::agents::LLM_RUNTIME_JSON,
            id,
            name,
            display_name: None,
            description: "d",
            system_prompt: None,
            model: None,
            tools_json,
            skills_json: "{}",
            params_json: "{}",
            max_iterations: 25,
            timeout_secs: 600,
            max_subagents,
            max_spawn_depth: 2,
            flow_id: None,
            routable: false,
            is_enabled: true,
            on_child_complete: "notify",
            // No delegation roster: `None` is unrestricted, and these fixtures
            // do not exercise the roster.
            allowed_agents_json: None,
            actor_user_id: None,
        },
    )
    .expect("seed agent");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn region_ends_on_a_turn_without_tool_calls_not_on_the_budget() {
    let pool = test_db();
    seed_skill(&pool, "11111111-0000-0000-0000-0000000000aa", "do-thing");
    seed_agent(
        &pool,
        "agent-code",
        "code-orchestrator-test",
        r#"["core.skill_view"]"#,
        0,
    );

    let mut reg = AdapterRegistry::new();
    let slot = service_slot(pool.clone());
    reg.register(Arc::new(
        tentaflow_core::flow_engine::node_adapters::TriggerNodeAdapter::new(),
    ));
    reg.register(Arc::new(
        tentaflow_core::flow_engine::node_adapters::OutputNodeAdapter::new(),
    ));
    reg.register(Arc::new(
        tentaflow_core::flow_engine::node_adapters::CompactContextNodeAdapter::new(),
    ));
    reg.register(Arc::new(
        tentaflow_core::flow_engine::node_adapters::ToolExecNodeAdapter::new(slot),
    ));
    reg.register_llm(Arc::new(
        tentaflow_core::flow_engine::node_adapters::LlmNodeAdapter::new(),
    ));
    let reg = Arc::new(reg);

    let compiled = Arc::new(
        CompiledFlow::from_json(
            CODE_HARNESS_FLOW_ID,
            &region_only_flow_json().to_string(),
            &reg,
        )
        .expect("region flow compiles"),
    );

    let mut initial = FlowEnvelope::empty();
    initial.payload = FlowValue::Text("look at the code".into());
    initial
        .context
        .messages
        .push(ChatMessage::user("look at the code"));
    initial.meta.insert("agent_id".into(), json!("agent-code"));
    initial.meta.insert("model".into(), json!("scripted"));
    initial.meta.insert(
        "harness_tools".into(),
        json!([{"name": "core.skill_view", "description": "view", "parameters": {"type":"object"}}]),
    );

    let mut ctx = stub_ctx();
    ctx.llm = Arc::new(ScriptedLlm {
        calls: AtomicUsize::new(0),
        tool_turns: 2,
    });

    let outcome = execute_blocking(pool, compiled, initial, ctx, reg)
        .await
        .expect("execute_blocking");
    assert!(outcome.error.is_none(), "{:?}", outcome.error);

    assert_eq!(
        outcome
            .final_envelope
            .meta
            .get("loop_exit_reason")
            .and_then(|v| v.as_str()),
        Some("no_tool_calls"),
        "the region must stop structurally, not on the iteration budget"
    );
    // Two tool turns + the prose turn.
    assert_eq!(
        outcome
            .final_envelope
            .meta
            .get("loop_iterations")
            .and_then(|v| v.as_i64()),
        Some(3)
    );
}

// -----------------------------------------------------------------------------
// §16.5 — the SEEDED graphs, executed on stub adapters
//
// The plan's acceptance shape is `FlowDefinition -> R1-R11 -> CompiledFlow ->
// execution on stub adapters`. Validating and compiling proves the graph is
// well-formed; only running it proves the harness works. The blocks that need a
// live workspace on disk (`workspace_context`, `agent_context`,
// `conversation_history`, `persist_turn`, `task_gate`, `spawn`/`await`) are
// stubs that record their visit and pass the envelope through; the LOOP, the
// region and the wiring are the real ones, straight out of `seed.rs`.
// -----------------------------------------------------------------------------

/// Node visits in execution order, plus whatever a stub chose to record about
/// the envelope it saw.
#[derive(Default)]
struct Recorder {
    visits: std::sync::Mutex<Vec<String>>,
}

impl Recorder {
    fn record(&self, entry: String) {
        self.visits.lock().expect("recorder lock").push(entry);
    }
    fn visits(&self) -> Vec<String> {
        self.visits.lock().expect("recorder lock").clone()
    }
    fn count(&self, node_id: &str) -> usize {
        self.visits()
            .iter()
            .filter(|v| v.split(':').next() == Some(node_id))
            .count()
    }
}

type StubBody = Box<dyn Fn(&FlowNode, &mut FlowEnvelope, &Recorder) + Send + Sync>;

/// One stand-in block. Ports are deliberately permissive (`in`/`text` in,
/// `full`/`text` out, all `Any`) so the seeded edges attach exactly as written
/// without the stub having to mirror each real adapter's port list.
struct StubAdapter {
    node_type: &'static str,
    recorder: Arc<Recorder>,
    body: StubBody,
}

impl StubAdapter {
    fn passthrough(node_type: &'static str, recorder: Arc<Recorder>) -> Self {
        Self {
            node_type,
            recorder,
            body: Box::new(|_, _, _| {}),
        }
    }
    fn with(node_type: &'static str, recorder: Arc<Recorder>, body: StubBody) -> Self {
        Self {
            node_type,
            recorder,
            body,
        }
    }
}

#[async_trait]
impl tentaflow_core::flow_engine::node_adapter::NodeAdapter for StubAdapter {
    fn node_type(&self) -> &str {
        self.node_type
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![
            PortSpec::new("in", FlowDataType::Any),
            PortSpec::new("text", FlowDataType::Any),
        ]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        vec![
            PortSpec::new("full", FlowDataType::Any),
            PortSpec::new("text", FlowDataType::Any),
        ]
    }
    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        _ctx: &tentaflow_core::flow_engine::node_adapter::ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let mut out: FlowEnvelope = inputs
            .first()
            .map(|i| (*i.envelope).clone())
            .unwrap_or_else(FlowEnvelope::empty);
        (self.body)(node, &mut out, &self.recorder);
        self.recorder.record(node.id.clone());
        Ok(out)
    }
}

/// Stubs for everything that would touch a workspace, the conversation store or
/// the run registry.
fn stub_registry(pool: DbPool, recorder: Arc<Recorder>, agent_id: &str) -> Arc<AdapterRegistry> {
    use tentaflow_core::flow_engine::node_adapters::*;

    let slot = service_slot(pool.clone());
    let mut r = AdapterRegistry::new();
    r.register(Arc::new(TriggerNodeAdapter::new()));
    r.register(Arc::new(OutputNodeAdapter::new()));
    // The loop and the tool step are REAL: they are what is under test.
    r.register(Arc::new(CompactContextNodeAdapter::new()));
    r.register(Arc::new(ToolExecNodeAdapter::new(slot)));
    r.register_llm(Arc::new(LlmNodeAdapter::new()));
    // So are the blocks that decide whether the pipeline runs and when each
    // review loop ends.
    r.register(Arc::new(ConditionNodeAdapter::new()));
    r.register(Arc::new(CriticGateNodeAdapter::new()));

    r.register(Arc::new(StubAdapter::passthrough(
        "conversation_history",
        recorder.clone(),
    )));
    // `workspace_context` publishes the binding facts and the tool surface.
    r.register(Arc::new(StubAdapter::with(
        "workspace_context",
        recorder.clone(),
        Box::new(|_node, env, _rec| {
            env.context
                .system_prompts
                .push("## Workspace\nRepository: stub\n".to_string());
            env.meta.insert(
                "code_workspace".into(),
                json!({"workspace_id": "ws-stub", "session_id": "sess-stub"}),
            );
        }),
    )));
    // `agent_context` pins the agent whose allowlist `tool_exec` reloads, and
    // hands the model its tool specs — exactly the two facts the loop needs.
    let pinned = agent_id.to_string();
    r.register(Arc::new(StubAdapter::with(
        "agent_context",
        recorder.clone(),
        Box::new(move |_node, env, _rec| {
            env.meta.insert("agent_id".into(), json!(pinned));
            env.meta.insert("model".into(), json!("scripted"));
            env.meta.insert(
                "harness_tools".into(),
                json!([{"name": "core.skill_view", "description": "view",
                        "parameters": {"type": "object"}}]),
            );
        }),
    )));
    // `patch_review` reports what a run with no worktree behind it truthfully
    // has to report: an EMPTY patch set. That is not a stand-in for a decision
    // — the real block short-circuits on `set.files.is_empty()` and returns
    // status "empty" WITHOUT ever raising the operator gate, which is the same
    // path a turn that changed nothing takes in production. A stub that
    // answered "accepted" would be inventing an operator who never looked, and
    // the two execution tests below would then be asserting something other
    // than what they claim.
    //
    // The graph does not branch on the outcome: the review has one outgoing
    // edge. So the status this stub reports cannot decide a test — what the
    // tests get from the block is that it sits at the end of the pipeline, runs
    // once, and lets the turn through.
    r.register(Arc::new(StubAdapter::with(
        "patch_review",
        recorder.clone(),
        Box::new(|node, env, _rec| {
            let var = node.config["output_variable"]
                .as_str()
                .unwrap_or("patch_review")
                .to_string();
            env.variables.insert(
                var,
                FlowValue::Json(json!({
                    "patch_set_id": "stub-empty",
                    "status": "empty",
                    "accepted": [],
                    "rejected": [],
                    "conflicted": [],
                    "timed_out": false,
                })),
            );
            env.meta
                .insert("patch_review_status".into(), json!("empty"));
            env.payload =
                FlowValue::Text("review empty: 0 accepted, 0 rejected, 0 conflicted".into());
        }),
    )));
    r.register(Arc::new(StubAdapter::passthrough(
        "persist_turn",
        recorder.clone(),
    )));
    // `task_gate` reads the session's plan from a workspace database. With no
    // open task it leaves the critic's decision untouched, which is what the
    // stub does by passing the envelope through.
    r.register(Arc::new(StubAdapter::passthrough(
        "task_gate",
        recorder.clone(),
    )));
    // The delegation pair. `spawn` writes ITS OWN run-id variable,
    // `await_subagents` reads the variable ITS config names — so the recording
    // proves the pairing comes from the graph, not from the test. Every wait
    // answers with the approval marker, so each critic gate ends its loop
    // after one round.
    let spawn_pool = pool;
    r.register(Arc::new(StubAdapter::with(
        "spawn",
        recorder.clone(),
        Box::new(move |node, env, rec| {
            let var = node.config["output_variable"].as_str().expect("var");
            let agent = spawned_agent_name(&spawn_pool, node);
            let run_id = format!("run-of-{agent}");
            env.variables
                .insert(var.to_string(), FlowValue::Json(json!([run_id])));
            rec.record(format!("{}:spawn:{agent}", node.id));
        }),
    )));
    r.register(Arc::new(StubAdapter::with(
        "await_subagents",
        recorder,
        Box::new(|node, env, rec| {
            let var = node.config["run_ids_var"].as_str().expect("var");
            let seen = env
                .variables
                .get(var)
                .and_then(|v| match v {
                    FlowValue::Json(json) => json.as_array().cloned(),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{}: nothing wrote '{var}'", node.id));
            rec.record(format!(
                "{}:await:{}",
                node.id,
                seen.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            ));
            let out = node.config["output_variable"].as_str().expect("out var");
            env.variables
                .insert(out.to_string(), FlowValue::Text("BEZ UWAG".into()));
        }),
    )));
    Arc::new(r)
}

fn harness_envelope() -> FlowEnvelope {
    let mut initial = FlowEnvelope::empty();
    initial.payload = FlowValue::Text("look at the code".into());
    initial
        .context
        .messages
        .push(ChatMessage::user("look at the code"));
    initial
}

/// §16.5 — the graph `seed.rs` ships is EXECUTED, not merely compiled: the
/// loop, the regions, the gates and the wiring are the real ones, and a turn
/// that called tools walks the whole pipeline in order.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_seeded_harness_runs_the_pipeline_after_a_working_turn() {
    let pool = test_db();
    seed_skill(&pool, "22222222-0000-0000-0000-0000000000aa", "do-thing");
    seed_agent(
        &pool,
        "agent-seeded",
        "code-harness-exec-test",
        r#"["core.skill_view"]"#,
        5,
    );
    let recorder = Arc::new(Recorder::default());
    let reg = stub_registry(pool.clone(), recorder.clone(), "agent-seeded");

    let compiled = Arc::new(
        CompiledFlow::from_json(CODE_HARNESS_FLOW_ID, &code_harness_flow_json(), &reg)
            .expect("the seeded graph must compile"),
    );

    let mut ctx = stub_ctx();
    ctx.llm = Arc::new(ScriptedLlm {
        calls: AtomicUsize::new(0),
        tool_turns: 2,
    });

    let outcome = execute_blocking(pool, compiled, harness_envelope(), ctx, reg)
        .await
        .expect("the seeded graph must execute");
    assert!(outcome.error.is_none(), "{:?}", outcome.error);

    let visits = recorder.visits();
    assert_eq!(
        visits,
        vec![
            "h1",
            "w1",
            "c0",
            // The turn is persisted before the pipeline, so the operator reads
            // the answer while the agents work behind it.
            "p1",
            "pls:spawn:code-planner",
            "pls",
            "pla:await:run-of-code-planner",
            "pla",
            "pcs:spawn:code-critic",
            "pcs",
            "pca:await:run-of-code-critic",
            "pca",
            "ims:spawn:code-implementer",
            "ims",
            "ima:await:run-of-code-implementer",
            "ima",
            "tes:spawn:code-tester",
            "tes",
            "tea:await:run-of-code-tester",
            "tea",
            "bcs:spawn:code-critic",
            "bcs",
            "bca:await:run-of-code-critic",
            "bca",
            "build_reviewt",
            "r1",
        ],
        "the pipeline must be the graph's doing: {visits:?}"
    );
    assert_eq!(
        recorder.count("p1"),
        1,
        "the turn is persisted exactly once"
    );

    // The loop stopped structurally after the prose turn, not on the budget.
    assert_eq!(
        outcome
            .final_envelope
            .meta
            .get("loop_exit_reason")
            .and_then(|v| v.as_str()),
        Some("no_tool_calls")
    );
    // And `workspace_context` really did reach the model's system context.
    assert!(outcome
        .final_envelope
        .context
        .system_prompts
        .iter()
        .any(|p| p.contains("## Workspace")));
}

/// A turn in which the agent only answered called no tools, and five sub-runs
/// over it would review nothing: the condition block routes it past the whole
/// pipeline, review included.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_without_tool_calls_skips_the_pipeline() {
    let pool = test_db();
    seed_agent(
        &pool,
        "agent-seeded-idle",
        "code-harness-idle-test",
        r#"["core.skill_view"]"#,
        5,
    );
    let recorder = Arc::new(Recorder::default());
    let reg = stub_registry(pool.clone(), recorder.clone(), "agent-seeded-idle");

    let compiled = Arc::new(
        CompiledFlow::from_json(CODE_HARNESS_FLOW_ID, &code_harness_flow_json(), &reg)
            .expect("the seeded graph must compile"),
    );

    let mut ctx = stub_ctx();
    ctx.llm = Arc::new(ScriptedLlm {
        calls: AtomicUsize::new(0),
        tool_turns: 0,
    });

    let outcome = execute_blocking(pool, compiled, harness_envelope(), ctx, reg)
        .await
        .expect("the seeded graph must execute");
    assert!(outcome.error.is_none(), "{:?}", outcome.error);

    let visits = recorder.visits();
    assert_eq!(
        visits,
        vec!["h1", "w1", "c0", "p1"],
        "an idle turn must not start a single sub-run: {visits:?}"
    );
}

// -----------------------------------------------------------------------------
// §15 — separation of duties is the allowlist, not the prompt
// -----------------------------------------------------------------------------

fn roster_tools(pool: &DbPool, name: &str) -> String {
    repository::get_agent_by_name(pool, name)
        .expect("query agent")
        .unwrap_or_else(|| panic!("seeded agent '{name}' is missing"))
        .tools_json
}

#[test]
fn seeded_roster_allowlists_enforce_the_separation_of_duties() {
    let pool = test_db();

    let orchestrator = roster_tools(&pool, "code-orchestrator");
    let planner = roster_tools(&pool, "code-planner");
    let implementer = roster_tools(&pool, "code-implementer");
    let searcher = roster_tools(&pool, "code-searcher");
    let reviewer = roster_tools(&pool, "code-reviewer");
    let tester = roster_tools(&pool, "code-tester");
    let committer = roster_tools(&pool, "code-committer");

    // The orchestrator holds the whole §10 set plus delegation and ask_user.
    for tool in CoreToolName::all().iter().filter(|t| t.is_code_studio()) {
        assert!(
            tool_in_allowlist(&orchestrator, tool.public_name(), None),
            "orchestrator must hold {}",
            tool.public_name()
        );
    }
    for extra in [
        "core.agent_spawn",
        "core.agent_wait",
        "core.agent_list",
        "core.agent_cancel",
        "core.ask_user",
    ] {
        assert!(tool_in_allowlist(&orchestrator, extra, None), "missing {extra}");
    }

    // The implementer writes code but cannot publish it.
    assert!(tool_in_allowlist(&implementer, "core.fs_write", None));
    assert!(tool_in_allowlist(&implementer, "core.exec", None));
    assert!(!tool_in_allowlist(&implementer, "core.git_push", None));
    assert!(!tool_in_allowlist(&implementer, "core.git_commit", None));

    // The committer works git but never touches the disk: the commit comes from
    // accepted blobs, so it cannot quietly "fix" code between review and commit.
    assert!(tool_in_allowlist(&committer, "core.git_commit", None));
    assert!(tool_in_allowlist(&committer, "core.git_push", None));
    for write in [
        "core.fs_write",
        "core.fs_edit",
        "core.fs_move",
        "core.fs_delete",
        "core.fs_mkdir",
        "core.exec",
    ] {
        assert!(
            !tool_in_allowlist(&committer, write, None),
            "committer must not hold {write}"
        );
    }

    // Reviewer and tester hold neither write nor push.
    for (name, tools) in [("reviewer", &reviewer), ("tester", &tester)] {
        assert!(
            !tool_in_allowlist(tools, "core.fs_write", None),
            "{name} must not write"
        );
        assert!(
            !tool_in_allowlist(tools, "core.git_push", None),
            "{name} must not push"
        );
    }
    assert!(tool_in_allowlist(&reviewer, "core.git_read", None));
    assert!(!tool_in_allowlist(&reviewer, "core.exec", None));
    assert!(tool_in_allowlist(&tester, "core.exec", None));
    assert!(!tool_in_allowlist(&tester, "core.git_read", None));

    // Planner and searcher are read-only.
    for (name, tools) in [("planner", &planner), ("searcher", &searcher)] {
        assert!(tool_in_allowlist(tools, "core.fs_read", None), "{name} reads");
        assert!(tool_in_allowlist(tools, "core.fs_grep", None), "{name} greps");
        for effect in ["core.fs_write", "core.exec", "core.git_commit"] {
            assert!(
                !tool_in_allowlist(tools, effect, None),
                "{name} must not hold {effect}"
            );
        }
    }
}

/// The allowlist is the FIRST sieve and it is not a permission: no grant can
/// widen it, and it gates DISPATCH, not just the catalog the model is shown.
///
/// The previous version of this test passed `|_| true` and `|_| false` to
/// `ToolCatalog::resolve` — but that closure is consulted for ADDON tools only,
/// so both arms produced the identical answer and the assertion proved nothing
/// about permissions. Here the permission checker is exercised where it really
/// applies (an addon tool), and the core verbs are checked at the point that
/// actually stops a call.
#[tokio::test]
async fn no_permission_grant_can_add_a_tool_the_allowlist_omits() {
    use tentaflow_core::addon::ToolDefinition;

    let pool = test_db();
    let committer = roster_tools(&pool, "code-committer");
    let principal = AgentPrincipal::user("u1");

    // 1. The permission checker moves an ADDON tool in and out of the catalog,
    //    which is what makes the next assertion meaningful.
    let addon_tools = vec![ToolDefinition {
        addon_id: "memory".into(),
        package_id: "memory".into(),
        tool_name: "memory_store".into(),
        description: "store".into(),
        parameters_schema: json!({"type": "object"}),
        return_schema: None,
        keywords: Vec::new(),
        read_only: false,
    }];
    let with_grant = ToolCatalog::resolve(
        r#"["memory.*","core.git_commit"]"#,
        &principal,
        &addon_tools,
        true,
        |_| true,
    );
    let without_grant = ToolCatalog::resolve(
        r#"["memory.*","core.git_commit"]"#,
        &principal,
        &addon_tools,
        true,
        |_| false,
    );
    let names = |specs: &[tentaflow_core::flow_engine::dispatchers::LlmToolSpec]| -> Vec<String> {
        specs.iter().map(|s| s.name.clone()).collect()
    };
    assert_eq!(
        names(&with_grant),
        vec!["memory.memory_store", "core.git_commit"]
    );
    assert_eq!(
        names(&without_grant),
        vec!["core.git_commit"],
        "the checker must really gate addon tools, or this test proves nothing"
    );

    // 2. And it still cannot add a core verb the committer's allowlist omits —
    //    with the SAME maximally permissive checker.
    let specs = ToolCatalog::resolve(&committer, &principal, &addon_tools, true, |_| true);
    assert!(!names(&specs).contains(&"core.fs_write".to_string()));
    assert!(names(&specs).contains(&"core.git_commit".to_string()));

    // 3. The sieve is not decoration on the catalog: dispatching a call outside
    //    the allowlist is refused, whatever the model asked for.
    let cipher = Arc::new(tentaflow_core::crypto::SettingsCipher::new(&[0u8; 32]));
    let addon_manager = Arc::new(
        tentaflow_core::addon::AddonManager::new(pool.clone(), cipher).expect("addon mgr"),
    );
    let service = AgentService::new(pool, addon_manager);
    let results = service.process_tool_calls(
        &committer,
        &[
            LlmToolCall {
                id: "c1".into(),
                name: "core.fs_write".into(),
                arguments: r#"{"path":"src/main.rs","content":"x"}"#.into(),
            },
            LlmToolCall {
                id: "c2".into(),
                name: "memory.memory_store".into(),
                arguments: "{}".into(),
            },
        ],
        &principal,
    );
    assert_eq!(results.len(), 2);
    for result in &results {
        assert!(!result.success, "{result:?}");
        assert!(
            result.content.contains("not in agent allowlist"),
            "the allowlist must be the reason: {result:?}"
        );
    }
}

#[test]
fn an_unknown_core_tool_is_rejected_by_the_catalog() {
    // A typo in an agent definition must surface, not silently pass through to
    // the addon dispatcher (which would look for an addon called `core`).
    assert!(CoreToolName::from_public_name("core.fs_chmod").is_none());
    let json = r#"["core.fs_read","core.fs_chmod"]"#;
    assert!(tool_in_allowlist(json, "core.fs_read", None));
    assert!(!tool_in_allowlist(json, "core.fs_chmod", None));
    let specs = ToolCatalog::resolve(json, &AgentPrincipal::user("u1"), &[], false, |_| true);
    assert_eq!(
        specs.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        vec!["core.fs_read"],
        "an unknown core.* name admits nothing"
    );
}

// -----------------------------------------------------------------------------
// §24 — prompt injection from the repository
// -----------------------------------------------------------------------------

#[test]
fn repository_instructions_cannot_raise_the_autonomy_mode() {
    use tentaflow_core::code_studio::models::AutonomyMode;
    use tentaflow_core::code_studio::models::WorkspaceRole;
    use tentaflow_core::code_studio::pep::{authorize, Capability, Decision, SessionCtx, Target};

    // A session running in `normal` asks to write a file. The mode says "ask",
    // and an AGENTS.md demanding autonomy is not an input to this decision at
    // all: the mode comes from the session row, the file only ever reaches the
    // model's context as fenced data.
    let ctx = SessionCtx {
        role: WorkspaceRole::Editor,
        autonomy: AutonomyMode::Normal,
        is_coordinator: false,
        has_accepted_patch_set: false,
        allowlisted: false,
        session_granted: false,
        run_granted: false,
    };
    let decision = authorize(
        &ctx,
        Capability::FsWrite,
        &Target::Path {
            inside_worktree: true,
        },
    );
    assert!(
        matches!(decision, Decision::AskUser { .. }),
        "normal mode must still ask: {decision:?}"
    );

    // And the fence the context block wraps the file in says so in words, so a
    // model reading the file is told what the server already enforces.
    use tentaflow_core::flow_engine::node_adapters::workspace_context::INSTRUCTIONS_NOTE;
    assert!(INSTRUCTIONS_NOTE.contains("change your autonomy mode"));
    assert!(INSTRUCTIONS_NOTE.contains("not an instruction from your operator"));
}

#[test]
fn code_search_is_a_tool_and_never_a_flow_node() {
    // §14: the semantic index is reachable as an agent tool, so the catalog must
    // know the name — an allowlist entry the catalog cannot resolve is dropped
    // and the tool would be unreachable.
    assert!(CoreToolName::all()
        .iter()
        .any(|t| t.public_name() == "core.code_search"));
    assert!(CoreToolName::from_public_name("core.code_search").is_some());
    // It stays a tool the agent calls inside the harness loop: the harness flow
    // has no search node, so the model decides when to search and grep remains
    // the authoritative fallback.
    let def: tentaflow_core::flow_engine::types::FlowDefinition =
        serde_json::from_str(&code_harness_flow_json()).expect("parses");
    assert!(def.nodes.iter().all(|n| n.node_type != "code_search"));
}
