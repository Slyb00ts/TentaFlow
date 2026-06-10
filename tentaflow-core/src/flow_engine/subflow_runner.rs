// ===== File: flow_engine/subflow_runner.rs — SubflowRunner: the foundation the
// subflow / loop / map / agent blocks reuse to run one flow as the body of
// another (Harness §3.5.0, §3.5 block 8). Loads a flow by id, compiles it per
// call, and runs `execute_blocking` on a CLONE of the parent ExecutionContext
// with a fresh execution_id (recorded with parent_execution_id), a fresh
// UsageSink (so the nested run does not steal the parent's token attribution),
// an incremented recursion depth and an extended visited set. =====

use std::sync::{Arc, Weak};

use anyhow::{anyhow, Result};

use crate::db::{repository, DbPool};
use crate::flow_engine::cache::CompiledFlow;
use crate::flow_engine::envelope::{FlowEnvelope, FlowExecutionOutcome};
use crate::flow_engine::executor::execute_blocking;
use crate::flow_engine::node_adapter::{AdapterRegistry, ExecutionContext, UsageSink};

/// Hard cap on sub-flow nesting depth (§3.5 block 8). A flow may nest sub-flows
/// up to this many levels; the depth guard fires when a child would exceed it.
/// Kept here (engine plumbing) and enforced by the `subflow` adapter against
/// `ctx.subflow_depth` before invoking the runner.
pub const MAX_SUBFLOW_DEPTH: u8 = 4;

/// Late-bound `SubflowRunner` slot (§3.5.0), mirroring `AgentServiceSlot` /
/// `ModelRuntimeSlot`. Filled in `FlowDispatcher::new` (the dispatcher already
/// holds the DbPool and the registry Arc), so the slot is populated before any
/// traffic. The `subflow` / `loop` / `map` / `agent` adapters hold a clone of
/// this slot; an empty slot at `execute` time is a node error (unreachable in
/// practice).
pub type SubflowRunnerSlot = Arc<parking_lot::RwLock<Option<Arc<SubflowRunner>>>>;

/// Runs a flow as the body of another flow. Holds the DbPool plus a `Weak`
/// reference to the AdapterRegistry — `Weak` breaks the ownership cycle
/// `FlowDispatcher → AdapterRegistry → (adapter holding the slot) → SubflowRunner
/// → AdapterRegistry`. The registry outlives every run (owned by the dispatcher),
/// so upgrading the `Weak` only fails during shutdown, where a node error is the
/// correct outcome.
pub struct SubflowRunner {
    db: DbPool,
    registry: Weak<AdapterRegistry>,
}

impl SubflowRunner {
    pub fn new(db: DbPool, registry: Weak<AdapterRegistry>) -> Self {
        Self { db, registry }
    }

    /// Runs `flow_id` with `initial_envelope` as its trigger input on a clone of
    /// `parent_ctx`. `extra_depth` is added to the parent depth (1 for a single
    /// nested run such as `subflow`/`agent`; `loop`/`map` bodies pass 1 too —
    /// sequential repetition of the same body is legal, deeper nesting of the
    /// same flow is not, which the visited-set guard catches).
    ///
    /// `light` requests a light-mode child run (§3.5 blocks 1/2): no
    /// per-iteration / per-element `flow_executions` row. `subflow`/`agent` pass
    /// `false` (they get a real audit row linked to the parent); `loop`/`map`
    /// pass `true`.
    ///
    /// The cycle/depth guards live in the calling adapter (it owns the node-error
    /// message and the self-reference check); this method assumes they passed and
    /// records the child flow into the visited set for the next level down.
    pub async fn run(
        &self,
        flow_id: &str,
        initial_envelope: FlowEnvelope,
        parent_ctx: &ExecutionContext,
        extra_depth: u8,
        light: bool,
    ) -> Result<FlowEnvelope> {
        let registry = self
            .registry
            .upgrade()
            .ok_or_else(|| anyhow!("subflow_runner: AdapterRegistry dropped (shutdown)"))?;

        // Load the flow row. `dispatch_by_flow_id` already recompiles per call;
        // FlowCache is keyed by `{model}:{service_type}:{modality}` and not
        // reusable here, so we recompile per call too. A flow_id-keyed cache is
        // a later optimization.
        let pool = self.db.clone();
        let lookup_id = flow_id.to_string();
        let flow = tokio::task::spawn_blocking(move || repository::get_flow(&pool, &lookup_id))
            .await
            .map_err(|e| anyhow!("subflow_runner: join: {e}"))?
            .map_err(|e| anyhow!("subflow_runner: load flow '{flow_id}': {e}"))?
            .ok_or_else(|| anyhow!("subflow_runner: flow '{flow_id}' not found"))?;

        if flow.status != "active" {
            return Err(anyhow!(
                "subflow_runner: flow '{flow_id}' status='{}' (not active)",
                flow.status
            ));
        }

        let compiled = CompiledFlow::from_json(&flow.id, &flow.flow_json, &registry)
            .map_err(|e| anyhow!("subflow_runner: compile flow '{flow_id}': {e}"))?;

        // Clone the parent context, then rewrite exactly the fields that must be
        // distinct for the child:
        // - `parent_execution_id` is the parent's OWN execution id (when it has
        //   one — light parents run with id 0), so a real child row links back to
        //   the parent; `execute_blocking` then assigns the child its own
        //   `execution_id`;
        // - `light` is propagated from the caller (loop iterations / map
        //   elements skip the audit row);
        // - a fresh `UsageSink` — a shared sink would be drained by the nested
        //   run and steal the parent's per-node token attribution;
        // - `subflow_depth` + `extra_depth`;
        // - `subflow_visited` extended with the child flow id.
        let mut child_ctx = parent_ctx.clone();
        child_ctx.parent_execution_id = (parent_ctx.execution_id > 0).then_some(parent_ctx.execution_id);
        child_ctx.light = light;
        child_ctx.usage_sink = Arc::new(UsageSink::new());
        child_ctx.subflow_depth = parent_ctx.subflow_depth.saturating_add(extra_depth);
        let mut visited = (*parent_ctx.subflow_visited).clone();
        visited.push(flow.id.clone());
        child_ctx.subflow_visited = Arc::new(visited);

        let outcome: FlowExecutionOutcome = execute_blocking(
            self.db.clone(),
            Arc::new(compiled),
            initial_envelope,
            child_ctx,
            registry,
        )
        .await?;

        if let Some(err) = outcome.error {
            return Err(anyhow!("subflow_runner: flow '{flow_id}' failed: {err}"));
        }
        Ok(outcome.final_envelope)
    }
}
