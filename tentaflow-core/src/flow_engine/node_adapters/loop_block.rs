// ===== File: flow_engine/node_adapters/loop_block.rs — LoopNodeAdapter
// (node_type "loop", category logic). General-purpose iterator: runs a body
// flow repeatedly via the shared SubflowRunner — the output envelope of
// iteration N is the input of iteration N+1 — until the `until` CEL boolean
// turns true OR the iteration budget is exhausted. This is the harness loop
// (§3.4): the body flow ("Agent Iteration") owns one model→tools turn, and
// this block owns only the "repeat until done" mechanics, so the loop body
// stays fully editable in the Flow Builder. Iterations run in light mode (the
// runner skips the per-iteration flow_executions row so 25 iterations never
// spam the table) and the recursion guard (depth + visited set) is inherited
// from ExecutionContext exactly like `subflow`. After the
// loop the block stamps meta.loop_iterations + meta.loop_exit_reason. Harness
// control signals (harness_done / loop_max_iterations / loop_final_pass) stay
// in envelope.meta — engine plumbing exchanged with agent_context, never
// promoted to declared variables. (Harness §3.5 block 1, §3.5.0, §3.10,
// §3.11 C.) =====

use std::time::Instant;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::stream::BoxStream;
use serde_json::Value;

use crate::flow_engine::dispatchers::ProgressEvent;
use crate::flow_engine::envelope::{EnvelopeDelta, FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::expr::{evaluate_bool, ExprScope};
use crate::flow_engine::node_adapter::{
    ExecutionContext, NodeAdapter, PortSpec, StreamProducerAdapter,
};
use crate::flow_engine::subflow_runner::{SubflowRunner, SubflowRunnerSlot, MAX_SUBFLOW_DEPTH};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "loop";

/// Default `until` expression (§3.5 block 1). The harness end-of-task signal
/// `harness_done` lives in `envelope.meta` (engine plumbing, set by tool_exec),
/// so the default reads it from the `meta` scope CEL exposes read-only — not
/// from `vars`. The `has(...)` guard is mandatory: CEL errors on a missing map
/// key, and `harness_done` is absent until tool_exec sets it, so a bare
/// `meta.harness_done == true` would fail every early iteration. A flow that
/// promotes the signal to a declared variable can override `until` to
/// `has(vars.harness_done) && vars.harness_done`.
const DEFAULT_UNTIL: &str = "has(meta.harness_done) && meta.harness_done == true";

/// Default iteration budget when neither node config nor
/// `meta.loop_max_iterations` (set by `agent_context` from the agent
/// definition) supplies one.
const DEFAULT_MAX_ITERATIONS: u32 = 25;

/// Hard cap on iterations regardless of config / meta override — bounds a
/// runaway agent loop. A configured or meta value above this is clamped down.
const MAX_ITERATIONS_CAP: u32 = 100;

pub struct LoopNodeAdapter {
    runner: SubflowRunnerSlot,
}

impl LoopNodeAdapter {
    pub fn new(runner: SubflowRunnerSlot) -> Self {
        Self { runner }
    }

    /// Reads the body flow id from node config. Required — a loop with no body
    /// is a misconfiguration.
    fn body_flow_id(node: &FlowNode) -> Result<String> {
        node.config
            .get("body_flow_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("loop: missing config 'body_flow_id'"))
    }

    /// The `until` CEL boolean. Empty / absent config uses `DEFAULT_UNTIL`.
    fn until_expr(node: &FlowNode) -> String {
        node.config
            .get("until")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_UNTIL)
            .to_string()
    }

    /// Resolves the iteration budget: `meta.loop_max_iterations` (set by
    /// agent_context) overrides node config, which overrides the default;
    /// the result is always clamped to `MAX_ITERATIONS_CAP`.
    fn max_iterations(node: &FlowNode, envelope: &FlowEnvelope) -> u32 {
        let from_meta = envelope
            .meta
            .get("loop_max_iterations")
            .and_then(|v| v.as_i64())
            .filter(|n| *n > 0)
            .map(|n| n as u32);
        let from_config = node
            .config
            .get("max_iterations")
            .and_then(|v| v.as_i64())
            .filter(|n| *n > 0)
            .map(|n| n as u32);
        from_meta
            .or(from_config)
            .unwrap_or(DEFAULT_MAX_ITERATIONS)
            .min(MAX_ITERATIONS_CAP)
    }

    /// Whether a grace-summary final pass runs after the budget is exhausted
    /// (§1.1). Default false.
    fn final_pass_enabled(node: &FlowNode) -> bool {
        node.config
            .get("final_pass")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// Evaluates `until` against the current envelope with `iteration` (the
    /// number of iterations already completed) bound as an extra. The harness
    /// signals it reads live in `meta`, exposed read-only by the CEL scope.
    fn until_true(expr: &str, envelope: &FlowEnvelope, iteration: u32) -> Result<bool> {
        let extras: [(&str, Value); 1] = [("iteration", Value::from(iteration))];
        let scope = ExprScope {
            vars: &envelope.variables,
            payload: &envelope.payload,
            artifacts: &envelope.artifacts,
            meta: &envelope.meta,
            extras: &extras,
        };
        evaluate_bool(expr, &scope, None).map_err(|e| anyhow!("loop until: {e}"))
    }

    /// Runs the guards (depth + cycle) shared by `execute` and `produce_stream`,
    /// resolves the runner, and returns the resolved loop plan + seed envelope.
    fn prepare(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<(std::sync::Arc<SubflowRunner>, LoopPlan, FlowEnvelope)> {
        let body_flow_id = Self::body_flow_id(node)?;

        // Same recursion guards as `subflow` (§3.10).
        if ctx.subflow_depth >= MAX_SUBFLOW_DEPTH {
            return Err(anyhow!(
                "loop: max nesting depth {MAX_SUBFLOW_DEPTH} reached (body flow '{body_flow_id}')"
            ));
        }
        if ctx.subflow_visited.iter().any(|v| v == &body_flow_id) {
            return Err(anyhow!(
                "loop: cycle detected — body flow '{body_flow_id}' already on the call path"
            ));
        }

        let runner = self
            .runner
            .read()
            .clone()
            .ok_or_else(|| anyhow!("loop: SubflowRunner slot not wired"))?;

        // The current envelope seeds iteration 0; each iteration's output feeds
        // the next. Built from the incoming input (falls back to the initial
        // envelope for a triggerless test harness).
        let seed: FlowEnvelope = inputs
            .first()
            .map(|i| (*i.envelope).clone())
            .unwrap_or_else(|| (*ctx.initial_envelope).clone());

        let plan = LoopPlan {
            body_flow_id,
            until: Self::until_expr(node),
            final_pass: Self::final_pass_enabled(node),
            // Budget is resolved against the seed envelope (agent_context has
            // already stamped meta.loop_max_iterations by the time the loop runs).
            max_iterations: Self::max_iterations(node, &seed),
        };
        Ok((runner, plan, seed))
    }

    /// Runs the budgeted blocking iterations: the body executes repeatedly (the
    /// output of iteration N is the input of N+1) until `until` turns true, the
    /// budget is exhausted, or cancel/deadline fires. Each iteration runs in
    /// light mode (no per-iteration flow_executions row) and drives the agent's
    /// tool-calling turn. Returns the final envelope, the iteration count, and
    /// the exit reason. Shared by `execute` (blocking) and `produce_stream`
    /// (streaming): the streaming path runs these intermediate tool-calling
    /// iterations blocking FIRST, then streams only the final iteration.
    async fn run_budgeted_iterations(
        runner: &SubflowRunner,
        plan: &LoopPlan,
        node: &FlowNode,
        ctx: &ExecutionContext,
        mut current: FlowEnvelope,
    ) -> Result<(FlowEnvelope, u32, &'static str)> {
        let mut iterations: u32 = 0;
        let exit_reason: &str = loop {
            // Cancel / deadline checked before each iteration — a long agent loop
            // must honour a client disconnect or the flow deadline without
            // waiting for the body to finish first.
            if ctx.cancel_token.is_cancelled() {
                break "cancelled";
            }
            // Iteration gating must honour the human-wait extension: a turn that
            // parked in `waiting_user` (ask_user / permission grant inside the
            // body's tool_exec) extends the SHARED deadline Arc, so the loop
            // driver reads `effective_deadline`, never the bare `deadline` —
            // otherwise the next iteration boundary would abort a run that the
            // §3.13 extension just kept alive.
            if ctx.effective_deadline().is_some_and(|d| Instant::now() >= d) {
                break "cancelled";
            }
            // Exit-on-until is checked at the top so an already-satisfied
            // condition on entry runs zero body iterations.
            if Self::until_true(&plan.until, &current, iterations)? {
                break "until";
            }
            if iterations >= plan.max_iterations {
                break "max_iterations";
            }

            ctx.progress.emit(
                &ctx.progress_scope,
                ProgressEvent::IterationStarted {
                    node_id: node.id.clone(),
                    n: iterations + 1,
                    max: plan.max_iterations,
                },
            );
            let next = runner
                .run(&plan.body_flow_id, current, ctx, 1, true)
                .await
                .map_err(|e| anyhow!("loop iteration {}: {e}", iterations + 1))?;
            current = next;
            iterations += 1;
            ctx.progress.emit(
                &ctx.progress_scope,
                ProgressEvent::IterationFinished {
                    node_id: node.id.clone(),
                    n: iterations,
                },
            );
        };
        Ok((current, iterations, exit_reason))
    }
}

/// Resolved loop configuration, computed once in `prepare` and reused by the
/// blocking and streaming iteration drivers.
struct LoopPlan {
    body_flow_id: String,
    until: String,
    final_pass: bool,
    max_iterations: u32,
}

#[async_trait]
impl NodeAdapter for LoopNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Any)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        // `full` is the blocking loop result; `stream` (§3.11 B) forwards the
        // final iteration's stream and makes this block the parent's stream
        // producer (R7). This is the harness final-answer path.
        vec![
            PortSpec::new("full", FlowDataType::Any),
            PortSpec::new("stream", FlowDataType::Any),
        ]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        // The recursion guards, runner, resolved plan and seed envelope are
        // shared with `produce_stream` (§3.5 block 1, §3.10). Sequential
        // repetition of the same body is legal — the visited set tracks nesting
        // depth of distinct flows, not repeat count.
        let (runner, plan, seed) = self.prepare(node, inputs, ctx)?;

        let (mut current, mut iterations, exit_reason) =
            Self::run_budgeted_iterations(&runner, &plan, node, ctx, seed).await?;

        // Grace summary (§1.1): one extra body iteration with
        // meta.loop_final_pass=true so the body's llm block drops tools and asks
        // the model to summarise. Only after budget exhaustion (not on `until`
        // / `cancelled`), and not when cancel/deadline already fired.
        if plan.final_pass
            && exit_reason == "max_iterations"
            && !ctx.cancel_token.is_cancelled()
            && !ctx.effective_deadline().is_some_and(|d| Instant::now() >= d)
        {
            current
                .meta
                .insert("loop_final_pass".into(), Value::Bool(true));
            ctx.progress.emit(
                &ctx.progress_scope,
                ProgressEvent::IterationStarted {
                    node_id: node.id.clone(),
                    n: iterations + 1,
                    max: plan.max_iterations,
                },
            );
            current = runner
                .run(&plan.body_flow_id, current, ctx, 1, true)
                .await
                .map_err(|e| anyhow!("loop final pass: {e}"))?;
            iterations += 1;
            // Clear the signal so it does not leak into the parent envelope.
            current.meta.remove("loop_final_pass");
            ctx.progress.emit(
                &ctx.progress_scope,
                ProgressEvent::IterationFinished {
                    node_id: node.id.clone(),
                    n: iterations,
                },
            );
            // A cancel/deadline that fired DURING the grace pass must still
            // surface — the loop chose "max_iterations" before the pass, so
            // re-check here and convert to the cancelled error path below.
            if ctx.cancel_token.is_cancelled()
                || ctx.effective_deadline().is_some_and(|d| Instant::now() >= d)
            {
                return Err(anyhow!(
                    "loop '{}': cancelled during final pass after {iterations} iteration(s)",
                    node.id
                ));
            }
        }

        current
            .meta
            .insert("loop_iterations".into(), Value::from(iterations));
        current.meta.insert(
            "loop_exit_reason".into(),
            Value::String(exit_reason.to_string()),
        );
        // A cancelled / deadline-hit loop is a hard stop: surface it as a node
        // error so the executor aborts the flow rather than continuing with a
        // half-finished agent turn.
        if exit_reason == "cancelled" {
            // Keep payload non-Empty so downstream output has something to show.
            if matches!(current.payload, FlowValue::Empty) {
                current.payload = FlowValue::Text(String::new());
            }
            return Err(anyhow!(
                "loop '{}': cancelled after {iterations} iteration(s)",
                node.id
            ));
        }

        Ok(current)
    }
}

/// Wraps an already-computed envelope's text payload as a terminal one-shot
/// stream: one text delta carrying the final answer, then a finish delta. Used
/// by the streaming path when the loop already holds the final answer (the body
/// finished via `until`, or the budget ran out with no grace pass) so it does
/// NOT issue a redundant extra LLM call — that would double-bill and could
/// diverge from the answer the blocking iterations already produced.
fn terminal_stream_from(envelope: &FlowEnvelope) -> BoxStream<'static, Result<EnvelopeDelta>> {
    use crate::flow_engine::envelope::{FinishReason, LlmStreamChunk};
    use futures::StreamExt;
    let text = envelope.payload.as_text().unwrap_or_default().to_string();
    let deltas = vec![
        Ok(EnvelopeDelta::Llm(LlmStreamChunk {
            text_delta: text,
            ..Default::default()
        })),
        Ok(EnvelopeDelta::Llm(LlmStreamChunk {
            finish_reason: Some(FinishReason::Stop),
            ..Default::default()
        })),
    ];
    futures::stream::iter(deltas).boxed()
}

/// §3.11 B — the harness stream path. When a flow wires this block's `stream`
/// output port, the loop is the parent's stream producer.
///
/// Control flow (SCOPE item 5 — `max_iterations` + cancel/deadline are still
/// honoured; this mirrors `execute` exactly so the streamed answer never differs
/// from the blocking one):
///   1. The intermediate tool-calling iterations run BLOCKING via
///      `run_budgeted_iterations` — exactly as `execute` would — until `until`
///      turns true, the budget is exhausted, or cancel/deadline fires. These
///      drive the agent's tool calls and MUST complete before any streaming.
///   2a. `until`: the body's `tool_exec` already produced the final no-tool
///      answer (it is in `current.payload`). It is forwarded as a terminal
///      stream — NO extra LLM call. Issuing a fresh streaming pass here would
///      re-answer a turn that already finished, doubling the model cost and
///      risking a streamed summary that diverges from the computed answer.
///   2b. `max_iterations` WITH `final_pass`: one FINAL body iteration runs
///      STREAMING with `meta.loop_final_pass=true`, so the body's `llm` block
///      drops tools (deterministic — `pick_tools` returns empty on the final
///      pass) and produces the streaming grace summary (§1.1). This is the only
///      case that streams a fresh pass, matching `execute`'s grace-summary gate.
///   2c. `max_iterations` WITHOUT `final_pass`: the budget ran out; the last
///      computed envelope is the answer, forwarded as a terminal stream.
/// A loop that exited blocking for `cancelled` does NOT run a streaming final
/// pass — the cancel surfaces as a producer error so the executor aborts.
#[async_trait]
impl StreamProducerAdapter for LoopNodeAdapter {
    async fn produce_stream(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<BoxStream<'static, Result<EnvelopeDelta>>> {
        let (runner, plan, seed) = self.prepare(node, inputs, ctx)?;

        // Step 1: run intermediate tool-calling iterations blocking.
        let (mut current, iterations, exit_reason) =
            Self::run_budgeted_iterations(&runner, &plan, node, ctx, seed).await?;

        if exit_reason == "cancelled" {
            return Err(anyhow!(
                "loop '{}': cancelled after {iterations} iteration(s)",
                node.id
            ));
        }

        // The grace-summary streaming pass runs ONLY when the budget was
        // exhausted AND final_pass is configured — identical gate to `execute`.
        // Every other terminal state already holds the final answer.
        if exit_reason != "max_iterations" || !plan.final_pass {
            return Ok(terminal_stream_from(&current));
        }

        // Grace summary: drop tools via loop_final_pass so the model produces a
        // clean streaming answer instead of another tool call, then forward the
        // body's stream.
        current
            .meta
            .insert("loop_final_pass".into(), Value::Bool(true));
        ctx.progress.emit(
            &ctx.progress_scope,
            ProgressEvent::IterationStarted {
                node_id: node.id.clone(),
                n: iterations + 1,
                max: plan.max_iterations,
            },
        );
        let exec = runner
            .run_streaming(&plan.body_flow_id, current, ctx, 1, true)
            .await
            .map_err(|e| anyhow!("loop final stream pass: {e}"))?;
        Ok(exec.stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{migrations, DbPool};
    use crate::flow_engine::dispatcher::build_registry_for_test;
    use crate::flow_engine::node_adapter::test_support::{stub_ctx, CapturingProgress};
    use crate::flow_engine::node_adapter::AdapterRegistry;
    use crate::flow_engine::subflow_runner::SubflowRunner;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        migrations::run(&conn).expect("migrations");
        Arc::new(Mutex::new(conn))
    }

    fn insert_flow(pool: &DbPool, id: &str, name: &str, flow_json: &str, status: &str) {
        let conn = pool.lock().unwrap();
        conn.execute(
            "INSERT INTO flows (id, name, service_type, flow_json, status, is_default) \
             VALUES (?1, ?2, NULL, ?3, ?4, 0)",
            rusqlite::params![id, name, flow_json, status],
        )
        .expect("insert flow");
    }

    /// Counter body flow: a `loop_test_body` node increments meta.iter and sets
    /// meta.harness_done=true once meta.iter reaches `stop_at`. Registered into
    /// a custom registry so the SubflowRunner can compile a flow referencing it.
    fn counter_registry_and_runner(
        pool: DbPool,
        stop_at: i64,
    ) -> (Arc<AdapterRegistry>, SubflowRunnerSlot) {
        let mut registry = build_registry_for_test();
        registry.register(Arc::new(CounterBodyAdapter { stop_at }));
        let registry = Arc::new(registry);
        let slot: SubflowRunnerSlot = Arc::new(parking_lot::RwLock::new(None));
        let runner = Arc::new(SubflowRunner::new(pool, Arc::downgrade(&registry)));
        *slot.write() = Some(runner);
        (registry, slot)
    }

    struct CounterBodyAdapter {
        stop_at: i64,
    }

    #[async_trait]
    impl NodeAdapter for CounterBodyAdapter {
        fn node_type(&self) -> &str {
            "loop_test_body"
        }
        fn input_ports(&self) -> Vec<PortSpec> {
            vec![PortSpec::new("text", FlowDataType::Text)]
        }
        fn output_ports(&self) -> Vec<PortSpec> {
            vec![PortSpec::new("text", FlowDataType::Text)]
        }
        async fn execute(
            &self,
            _node: &FlowNode,
            inputs: &[NodeInput],
            ctx: &ExecutionContext,
        ) -> Result<FlowEnvelope> {
            let mut env = inputs
                .first()
                .map(|i| (*i.envelope).clone())
                .unwrap_or_else(|| (*ctx.initial_envelope).clone());
            let n = env.meta.get("iter").and_then(|v| v.as_i64()).unwrap_or(0) + 1;
            env.meta.insert("iter".into(), Value::from(n));
            if n >= self.stop_at {
                env.meta.insert("harness_done".into(), Value::Bool(true));
            }
            // Text payload so the `text` output port stays type-valid.
            env.payload = FlowValue::Text(format!("iter {n}"));
            Ok(env)
        }
    }

    /// Body flow JSON: trigger → loop_test_body → output. The counter node sees
    /// the trigger envelope (carrying meta from the parent loop), bumps it, and
    /// output echoes it back as the iteration result.
    fn counter_body_json() -> String {
        json!({
            "nodes": [
                {"id": "t", "type": "trigger", "config": {}},
                {"id": "b", "type": "loop_test_body", "config": {}},
                {"id": "o", "type": "output", "config": {"format": "text"}}
            ],
            "edges": [
                {"from": "t", "from_port": "text", "to": "b", "to_port": "text"},
                {"from": "b", "from_port": "text", "to": "o", "to_port": "text"}
            ]
        })
        .to_string()
    }

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "loop1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
        }
    }

    fn input(env: FlowEnvelope) -> NodeInput {
        NodeInput {
            from_node_id: "trigger".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }
    }

    #[tokio::test]
    async fn loop_runs_body_until_harness_done() {
        let pool = db();
        let body_id = "11111111-loop-0000-0000-000000000001";
        insert_flow(
            &pool,
            body_id,
            "counter-body",
            &counter_body_json(),
            "active",
        );
        // Body flips harness_done at iter 3 → loop exits with reason `until`.
        let (_registry, slot) = counter_registry_and_runner(pool.clone(), 3);

        let out = LoopNodeAdapter::new(slot)
            .execute(
                &node(json!({"body_flow_id": body_id})),
                &[input(FlowEnvelope::empty())],
                &stub_ctx(),
            )
            .await
            .expect("execute");

        assert_eq!(
            out.meta.get("loop_exit_reason").and_then(|v| v.as_str()),
            Some("until")
        );
        assert_eq!(
            out.meta.get("loop_iterations").and_then(|v| v.as_i64()),
            Some(3)
        );
        // harness_done set by the body on the 3rd iteration.
        assert_eq!(
            out.meta.get("harness_done").and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[tokio::test]
    async fn iterations_create_no_flow_executions_rows() {
        let pool = db();
        let body_id = "99999999-loop-0000-0000-000000000001";
        insert_flow(&pool, body_id, "counter-body", &counter_body_json(), "active");
        // Body never stops on its own → the loop runs the full budget.
        let (_registry, slot) = counter_registry_and_runner(pool.clone(), 1000);

        LoopNodeAdapter::new(slot)
            .execute(
                &node(json!({"body_flow_id": body_id, "max_iterations": 8})),
                &[input(FlowEnvelope::empty())],
                &stub_ctx(),
            )
            .await
            .expect("execute");

        // Light-mode body runs must NOT insert a flow_executions row per
        // iteration — 8 iterations leave the table empty (§3.5 block 1).
        let rows: i64 = {
            let conn = pool.lock().unwrap();
            conn.query_row("SELECT COUNT(*) FROM flow_executions", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(rows, 0, "light-mode loop spammed flow_executions");
    }

    #[tokio::test]
    async fn loop_respects_max_iterations_cap() {
        let pool = db();
        let body_id = "22222222-loop-0000-0000-000000000001";
        insert_flow(
            &pool,
            body_id,
            "counter-body",
            &counter_body_json(),
            "active",
        );
        // Body never sets harness_done (stop_at far above the budget), so the
        // loop must stop at max_iterations.
        let (_registry, slot) = counter_registry_and_runner(pool.clone(), 1000);

        let out = LoopNodeAdapter::new(slot)
            .execute(
                &node(json!({"body_flow_id": body_id, "max_iterations": 5})),
                &[input(FlowEnvelope::empty())],
                &stub_ctx(),
            )
            .await
            .expect("execute");

        assert_eq!(
            out.meta.get("loop_exit_reason").and_then(|v| v.as_str()),
            Some("max_iterations")
        );
        assert_eq!(
            out.meta.get("loop_iterations").and_then(|v| v.as_i64()),
            Some(5)
        );
    }

    #[tokio::test]
    async fn meta_loop_max_iterations_overrides_config_and_caps() {
        let pool = db();
        let body_id = "33333333-loop-0000-0000-000000000001";
        insert_flow(
            &pool,
            body_id,
            "counter-body",
            &counter_body_json(),
            "active",
        );
        let (_registry, slot) = counter_registry_and_runner(pool.clone(), 1000);

        // meta override (3) beats config (50); both are under the cap.
        let mut env = FlowEnvelope::empty();
        env.meta
            .insert("loop_max_iterations".into(), Value::from(3));
        let out = LoopNodeAdapter::new(slot)
            .execute(
                &node(json!({"body_flow_id": body_id, "max_iterations": 50})),
                &[input(env)],
                &stub_ctx(),
            )
            .await
            .expect("execute");
        assert_eq!(
            out.meta.get("loop_iterations").and_then(|v| v.as_i64()),
            Some(3)
        );
    }

    #[tokio::test]
    async fn final_pass_runs_one_extra_iteration_after_budget() {
        let pool = db();
        let body_id = "44444444-loop-0000-0000-000000000001";
        insert_flow(
            &pool,
            body_id,
            "counter-body",
            &counter_body_json(),
            "active",
        );
        let (_registry, slot) = counter_registry_and_runner(pool.clone(), 1000);

        let out = LoopNodeAdapter::new(slot)
            .execute(
                &node(json!({"body_flow_id": body_id, "max_iterations": 2, "final_pass": true})),
                &[input(FlowEnvelope::empty())],
                &stub_ctx(),
            )
            .await
            .expect("execute");

        // 2 budgeted + 1 grace = 3 total body runs.
        assert_eq!(
            out.meta.get("loop_iterations").and_then(|v| v.as_i64()),
            Some(3)
        );
        assert_eq!(
            out.meta.get("loop_exit_reason").and_then(|v| v.as_str()),
            Some("max_iterations")
        );
        // The final-pass signal is cleared before returning to the parent.
        assert!(out.meta.get("loop_final_pass").is_none());
        // The body still saw it: the counter ran a 3rd time.
        assert_eq!(out.meta.get("iter").and_then(|v| v.as_i64()), Some(3));
    }

    #[tokio::test]
    async fn cancelled_loop_is_node_error() {
        let pool = db();
        let body_id = "55555555-loop-0000-0000-000000000001";
        insert_flow(
            &pool,
            body_id,
            "counter-body",
            &counter_body_json(),
            "active",
        );
        let (_registry, slot) = counter_registry_and_runner(pool.clone(), 1000);

        let ctx = stub_ctx();
        ctx.cancel_token.cancel();
        let err = LoopNodeAdapter::new(slot)
            .execute(
                &node(json!({"body_flow_id": body_id, "max_iterations": 10})),
                &[input(FlowEnvelope::empty())],
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cancelled"), "{err}");
    }

    #[tokio::test]
    async fn emits_iteration_progress_events() {
        let pool = db();
        let body_id = "66666666-loop-0000-0000-000000000001";
        insert_flow(
            &pool,
            body_id,
            "counter-body",
            &counter_body_json(),
            "active",
        );
        let (_registry, slot) = counter_registry_and_runner(pool.clone(), 2);

        let progress = Arc::new(CapturingProgress::new());
        let mut ctx = stub_ctx();
        ctx.progress = progress.clone();
        ctx.progress_scope = "scope-1".into();

        LoopNodeAdapter::new(slot)
            .execute(
                &node(json!({"body_flow_id": body_id, "max_iterations": 10})),
                &[input(FlowEnvelope::empty())],
                &ctx,
            )
            .await
            .expect("execute");

        let events = progress.events();
        let started = events
            .iter()
            .filter(|(_, e)| matches!(e, ProgressEvent::IterationStarted { .. }))
            .count();
        let finished = events
            .iter()
            .filter(|(_, e)| matches!(e, ProgressEvent::IterationFinished { .. }))
            .count();
        assert_eq!(started, 2);
        assert_eq!(finished, 2);
    }

    #[tokio::test]
    async fn depth_guard_fires_at_cap() {
        let pool = db();
        let body_id = "77777777-loop-0000-0000-000000000001";
        insert_flow(&pool, body_id, "body", &counter_body_json(), "active");
        let (_registry, slot) = counter_registry_and_runner(pool.clone(), 1);

        let mut ctx = stub_ctx();
        ctx.subflow_depth = MAX_SUBFLOW_DEPTH;
        let err = LoopNodeAdapter::new(slot)
            .execute(
                &node(json!({"body_flow_id": body_id})),
                &[input(FlowEnvelope::empty())],
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("max nesting depth"), "{err}");
    }

    #[tokio::test]
    async fn cycle_guard_fires_on_self_reference() {
        let pool = db();
        let body_id = "88888888-loop-0000-0000-000000000001";
        insert_flow(&pool, body_id, "body", &counter_body_json(), "active");
        let (_registry, slot) = counter_registry_and_runner(pool.clone(), 1);

        let mut ctx = stub_ctx();
        ctx.subflow_visited = Arc::new(vec![body_id.to_string()]);
        let err = LoopNodeAdapter::new(slot)
            .execute(
                &node(json!({"body_flow_id": body_id})),
                &[input(FlowEnvelope::empty())],
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cycle detected"), "{err}");
    }

    #[tokio::test]
    async fn unwired_slot_is_error() {
        let slot: SubflowRunnerSlot = Arc::new(parking_lot::RwLock::new(None));
        let err = LoopNodeAdapter::new(slot)
            .execute(
                &node(json!({"body_flow_id": "x"})),
                &[input(FlowEnvelope::empty())],
                &stub_ctx(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("slot not wired"), "{err}");
    }

    /// Streaming-capable counter body (§3.11 B test double). Its `execute`
    /// (blocking path, run for intermediate tool-calling iterations) bumps a
    /// counter and sets harness_done at `stop_at`; its `produce_stream` (final
    /// pass) emits a two-chunk delta stream tagged with the iteration count so a
    /// test can assert which iteration streamed.
    struct StreamingCounterBodyAdapter {
        stop_at: i64,
    }

    #[async_trait]
    impl NodeAdapter for StreamingCounterBodyAdapter {
        fn node_type(&self) -> &str {
            "loop_test_stream_body"
        }
        fn input_ports(&self) -> Vec<PortSpec> {
            vec![PortSpec::new("in", FlowDataType::Text)]
        }
        fn output_ports(&self) -> Vec<PortSpec> {
            vec![
                PortSpec::new("stream", FlowDataType::Text),
                PortSpec::new("full", FlowDataType::Text),
            ]
        }
        async fn execute(
            &self,
            _node: &FlowNode,
            inputs: &[NodeInput],
            ctx: &ExecutionContext,
        ) -> Result<FlowEnvelope> {
            let mut env = inputs
                .first()
                .map(|i| (*i.envelope).clone())
                .unwrap_or_else(|| (*ctx.initial_envelope).clone());
            let n = env.meta.get("iter").and_then(|v| v.as_i64()).unwrap_or(0) + 1;
            env.meta.insert("iter".into(), Value::from(n));
            if n >= self.stop_at {
                env.meta.insert("harness_done".into(), Value::Bool(true));
            }
            env.payload = FlowValue::Text(format!("iter {n}"));
            Ok(env)
        }
    }

    #[async_trait]
    impl StreamProducerAdapter for StreamingCounterBodyAdapter {
        async fn produce_stream(
            &self,
            _node: &FlowNode,
            inputs: &[NodeInput],
            ctx: &ExecutionContext,
        ) -> Result<BoxStream<'static, Result<EnvelopeDelta>>> {
            use crate::flow_engine::envelope::{FinishReason, LlmStreamChunk};
            use futures::StreamExt;
            let env = inputs
                .first()
                .map(|i| (*i.envelope).clone())
                .unwrap_or_else(|| (*ctx.initial_envelope).clone());
            let iter = env.meta.get("iter").and_then(|v| v.as_i64()).unwrap_or(0);
            // Final pass means tools are dropped — assert the harness signalled it.
            let final_pass = env
                .meta
                .get("loop_final_pass")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let first = LlmStreamChunk {
                text_delta: format!("final-answer iter={iter} final_pass={final_pass}"),
                ..Default::default()
            };
            let last = LlmStreamChunk {
                finish_reason: Some(FinishReason::Stop),
                ..Default::default()
            };
            Ok(futures::stream::iter(vec![
                Ok(EnvelopeDelta::Llm(first)),
                Ok(EnvelopeDelta::Llm(last)),
            ])
            .boxed())
        }
    }

    /// Streaming body flow: trigger → loop_test_stream_body → output(stream).
    /// Blocking iterations drive the counter; the final pass streams.
    fn stream_body_json() -> String {
        json!({
            "nodes": [
                {"id": "t", "type": "trigger", "config": {}},
                {"id": "b", "type": "loop_test_stream_body", "config": {}},
                {"id": "o", "type": "output", "config": {"mode": "stream"}}
            ],
            "edges": [
                {"from": "t", "from_port": "text", "to": "b", "to_port": "in"},
                {"from": "b", "from_port": "stream", "to": "o", "to_port": "text"}
            ]
        })
        .to_string()
    }

    fn stream_registry_and_runner(
        pool: DbPool,
        stop_at: i64,
    ) -> (Arc<AdapterRegistry>, SubflowRunnerSlot) {
        let mut registry = build_registry_for_test();
        registry.register_stream_producer(Arc::new(StreamingCounterBodyAdapter { stop_at }));
        let registry = Arc::new(registry);
        let slot: SubflowRunnerSlot = Arc::new(parking_lot::RwLock::new(None));
        let runner = Arc::new(SubflowRunner::new(pool, Arc::downgrade(&registry)));
        *slot.write() = Some(runner);
        (registry, slot)
    }

    async fn collect_stream(
        stream: BoxStream<'static, Result<EnvelopeDelta>>,
    ) -> (String, bool) {
        use crate::flow_engine::envelope::FinishReason;
        use futures::StreamExt;
        let mut text = String::new();
        let mut saw_finish = false;
        let mut s = stream;
        while let Some(item) = s.next().await {
            if let EnvelopeDelta::Llm(c) = item.expect("delta ok") {
                text.push_str(&c.text_delta);
                if c.finish_reason == Some(FinishReason::Stop) {
                    saw_finish = true;
                }
            }
        }
        (text, saw_finish)
    }

    /// §3.11 B — a loop that finished via `until` already holds the final
    /// answer; `produce_stream` forwards THAT payload as a terminal stream and
    /// MUST NOT issue a fresh streaming pass (that would re-answer a finished
    /// turn, doubling cost and risking divergence). The body sets harness_done
    /// at iter 1, so the loop runs one blocking iteration (payload "iter 1"),
    /// exits `until`, and the stream is the computed answer — not the body's
    /// streaming-pass marker.
    #[tokio::test]
    async fn produce_stream_forwards_computed_answer_on_until() {
        let pool = db();
        let body_id = "aaaa0000-loop-strm-0000-000000000001";
        insert_flow(&pool, body_id, "stream-body", &stream_body_json(), "active");
        // Body flips harness_done at iter 1 → loop exits `until` after one
        // blocking iteration; no streaming grace pass.
        let (_registry, slot) = stream_registry_and_runner(pool.clone(), 1);

        let stream = LoopNodeAdapter::new(slot)
            .produce_stream(
                &node(json!({"body_flow_id": body_id, "max_iterations": 10})),
                &[input(FlowEnvelope::empty())],
                &stub_ctx(),
            )
            .await
            .expect("produce_stream");

        let (text, saw_finish) = collect_stream(stream).await;
        // The forwarded answer is the blocking iteration's payload, NOT the
        // streaming body's "final-answer ..." marker (which only appears when a
        // fresh streaming pass runs).
        assert_eq!(text, "iter 1", "expected forwarded computed answer: {text:?}");
        assert!(
            !text.contains("final-answer"),
            "until exit must not run a streaming pass: {text:?}"
        );
        assert!(saw_finish, "client never got finish_reason=Stop");
    }

    /// Finding 3 — a loop that finished via `until` after multiple tool-calling
    /// turns still forwards the computed answer, never a fresh streaming pass.
    /// The body sets harness_done at iter 3 → 3 blocking iterations, payload
    /// "iter 3", terminal stream.
    #[tokio::test]
    async fn multi_iteration_until_forwards_computed_answer() {
        let pool = db();
        let body_id = "bbbb0000-loop-strm-0000-000000000001";
        insert_flow(&pool, body_id, "stream-body", &stream_body_json(), "active");
        let (_registry, slot) = stream_registry_and_runner(pool.clone(), 3);

        let stream = LoopNodeAdapter::new(slot)
            .produce_stream(
                &node(json!({"body_flow_id": body_id, "max_iterations": 10})),
                &[input(FlowEnvelope::empty())],
                &stub_ctx(),
            )
            .await
            .expect("produce_stream");

        let (text, saw_finish) = collect_stream(stream).await;
        assert_eq!(text, "iter 3", "expected forwarded computed answer: {text:?}");
        assert!(
            !text.contains("final-answer"),
            "until exit must not run a streaming pass: {text:?}"
        );
        assert!(saw_finish, "client never got finish_reason=Stop");
    }

    /// Finding 3 — the streaming grace pass runs ONLY on `max_iterations` with
    /// `final_pass=true`, matching `execute`. The body never sets harness_done,
    /// so the loop exhausts its 2-iteration budget, then streams one final pass
    /// (loop_final_pass=true, tools dropped) via the streaming body.
    #[tokio::test]
    async fn produce_stream_runs_grace_pass_on_budget_with_final_pass() {
        let pool = db();
        let body_id = "dddd0000-loop-strm-0000-000000000001";
        insert_flow(&pool, body_id, "stream-body", &stream_body_json(), "active");
        // Body never stops on its own → budget exhausted at max_iterations.
        let (_registry, slot) = stream_registry_and_runner(pool.clone(), 1000);

        let stream = LoopNodeAdapter::new(slot)
            .produce_stream(
                &node(json!({"body_flow_id": body_id, "max_iterations": 2, "final_pass": true})),
                &[input(FlowEnvelope::empty())],
                &stub_ctx(),
            )
            .await
            .expect("produce_stream");

        let (text, saw_finish) = collect_stream(stream).await;
        // 2 blocking iterations ran; the streaming grace pass then carries
        // iter=2 (the last blocking iteration's count) and loop_final_pass=true.
        assert!(text.contains("final-answer"), "grace pass must stream: {text:?}");
        assert!(text.contains("iter=2"), "expected grace pass on iter 2: {text:?}");
        assert!(text.contains("final_pass=true"), "stream text: {text:?}");
        assert!(saw_finish, "client never got finish_reason=Stop");
    }

    /// Finding 3 — budget exhausted WITHOUT final_pass forwards the last
    /// computed envelope as a terminal stream, no fresh LLM call (mirrors
    /// `execute`, which does not run a grace pass when final_pass is off).
    #[tokio::test]
    async fn produce_stream_forwards_answer_on_budget_without_final_pass() {
        let pool = db();
        let body_id = "eeee0000-loop-strm-0000-000000000001";
        insert_flow(&pool, body_id, "stream-body", &stream_body_json(), "active");
        let (_registry, slot) = stream_registry_and_runner(pool.clone(), 1000);

        let stream = LoopNodeAdapter::new(slot)
            .produce_stream(
                &node(json!({"body_flow_id": body_id, "max_iterations": 2})),
                &[input(FlowEnvelope::empty())],
                &stub_ctx(),
            )
            .await
            .expect("produce_stream");

        let (text, saw_finish) = collect_stream(stream).await;
        assert_eq!(text, "iter 2", "expected forwarded computed answer: {text:?}");
        assert!(
            !text.contains("final-answer"),
            "no final_pass must not run a streaming pass: {text:?}"
        );
        assert!(saw_finish, "client never got finish_reason=Stop");
    }

    /// SCOPE item 5 — a cancelled loop never streams a final pass; the cancel
    /// surfaces as a producer error so the executor aborts the flow.
    #[tokio::test]
    async fn cancelled_loop_produce_stream_is_error() {
        let pool = db();
        let body_id = "cccc0000-loop-strm-0000-000000000001";
        insert_flow(&pool, body_id, "stream-body", &stream_body_json(), "active");
        let (_registry, slot) = stream_registry_and_runner(pool.clone(), 1000);

        let ctx = stub_ctx();
        ctx.cancel_token.cancel();
        let result = LoopNodeAdapter::new(slot)
            .produce_stream(
                &node(json!({"body_flow_id": body_id, "max_iterations": 10})),
                &[input(FlowEnvelope::empty())],
                &ctx,
            )
            .await;
        let err = match result {
            Ok(_) => panic!("cancelled loop must not produce a stream"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("cancelled"), "{err}");
    }
}
