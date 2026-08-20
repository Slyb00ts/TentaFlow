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
use crate::flow_engine::envelope::{
    AudioStreamChunk, EnvelopeDelta, FlowEnvelope, FlowExecutionOutcome, FlowValue, LlmStreamChunk,
};
use crate::flow_engine::executor::{execute_blocking, execute_streaming, StreamingExecution};
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

    /// Resolves a published model name (`{addon_id}:{engine_flow_id}`) to the
    /// flow row id. Addon engine-flows get a random UUID minted at install
    /// (`register_engine_flow_atomic`), so an outer flow cannot hardcode the
    /// body flow id in JSON; instead it names the body by its install-stable
    /// published name and resolves it here at runtime. Used by `loop`/`subflow`
    /// blocks whose body is a sibling engine-flow of the same addon instance.
    pub async fn resolve_published_flow_id(&self, published_name: &str) -> Result<String> {
        let pool = self.db.clone();
        let name = published_name.to_string();
        let id = tokio::task::spawn_blocking(move || {
            repository::get_flow_id_by_published_model_name(&pool, &name)
        })
        .await
        .map_err(|e| anyhow!("subflow_runner: join: {e}"))?
        .map_err(|e| anyhow!("subflow_runner: resolve '{published_name}': {e}"))?;
        id.ok_or_else(|| {
            anyhow!("subflow_runner: no flow published as '{published_name}' (not installed?)")
        })
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
        let Prepared {
            registry,
            compiled,
            child_ctx,
        } = self
            .prepare(flow_id, parent_ctx, extra_depth, light)
            .await?;

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

    /// Streaming variant of `run` (§3.11 B): runs the child flow in streaming
    /// mode and returns its `EnvelopeDelta` stream + outcome receiver, so a
    /// `subflow` / `agent` / `loop` block that is the parent flow's stream
    /// producer can forward the inner final unit's tokens straight out without
    /// buffering the whole answer. The child ctx is built exactly as in `run`
    /// (fresh usage sink, parent_execution_id link, depth/visited guards).
    ///
    /// A child flow WITHOUT a streaming end-shape (no `from_port="stream"` edge)
    /// cannot be driven by `execute_streaming`; rather than fail, it runs
    /// blocking and the single final payload is wrapped as one terminal delta —
    /// callers always get a stream regardless of how the child was authored.
    /// The child's own finalizer (or the blocking persist) writes its
    /// `flow_executions` row; the returned outcome receiver lets the caller
    /// observe the settled result if it wants to (the executor's producer
    /// finalizer builds the PARENT outcome from the forwarded deltas, so most
    /// callers drop it).
    pub async fn run_streaming(
        &self,
        flow_id: &str,
        initial_envelope: FlowEnvelope,
        parent_ctx: &ExecutionContext,
        extra_depth: u8,
        light: bool,
    ) -> Result<StreamingExecution> {
        let Prepared {
            registry,
            compiled,
            child_ctx,
        } = self
            .prepare(flow_id, parent_ctx, extra_depth, light)
            .await?;

        if compiled.is_streaming {
            return execute_streaming(
                self.db.clone(),
                Arc::new(compiled),
                initial_envelope,
                child_ctx,
                registry,
            )
            .await;
        }

        // Non-streaming child: run blocking, then surface its final payload as a
        // single terminal delta so the forwarding parent still produces a stream.
        let blobs = child_ctx.blobs.clone();
        let outcome = execute_blocking(
            self.db.clone(),
            Arc::new(compiled),
            initial_envelope,
            child_ctx,
            registry,
        )
        .await?;
        if let Some(err) = &outcome.error {
            return Err(anyhow!("subflow_runner: flow '{flow_id}' failed: {err}"));
        }
        Ok(wrap_outcome_as_stream(outcome, blobs))
    }

    /// Shared setup for `run` / `run_streaming`: upgrades the registry Weak,
    /// loads + compiles the flow, and builds the child execution context with
    /// the same field rewrites both paths need.
    pub(crate) async fn prepare(
        &self,
        flow_id: &str,
        parent_ctx: &ExecutionContext,
        extra_depth: u8,
        light: bool,
    ) -> Result<Prepared> {
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
        child_ctx.parent_execution_id =
            (parent_ctx.execution_id > 0).then_some(parent_ctx.execution_id);
        child_ctx.light = light;
        child_ctx.usage_sink = Arc::new(UsageSink::new());
        child_ctx.subflow_depth = parent_ctx.subflow_depth.saturating_add(extra_depth);
        let mut visited = (*parent_ctx.subflow_visited).clone();
        visited.push(flow.id.clone());
        child_ctx.subflow_visited = Arc::new(visited);

        Ok(Prepared {
            registry,
            compiled,
            child_ctx,
        })
    }
}

/// Output of `SubflowRunner::prepare` — the upgraded registry, the compiled
/// child flow, and the rewritten child execution context.
pub(crate) struct Prepared {
    pub(crate) registry: Arc<AdapterRegistry>,
    pub(crate) compiled: CompiledFlow,
    pub(crate) child_ctx: ExecutionContext,
}

/// Wraps a blocking child `FlowExecutionOutcome` as a single-delta stream so a
/// streaming-forwarding parent (subflow / agent / a loop whose final iteration
/// body is not itself streaming) still emits a `StreamingExecution`. Mirrors
/// the dispatcher's `wrap_blocking_as_stream`: an audio payload is fetched from
/// the blob store and emitted as one `EnvelopeDelta::Audio`, everything else as
/// one terminal `EnvelopeDelta::Llm` carrying the whole text.
fn wrap_outcome_as_stream(
    outcome: FlowExecutionOutcome,
    blobs: Arc<dyn crate::flow_engine::blob_store::BlobStore>,
) -> StreamingExecution {
    use futures::stream::StreamExt;
    let payload = outcome.final_envelope.payload.clone();
    let usage = outcome.usage.clone();
    let perf = outcome.perf;
    let finish = outcome.finish_reason.clone();
    let err = outcome.error.clone();
    let stream = futures::stream::once(async move {
        match payload {
            FlowValue::Audio {
                blob_ref,
                mime,
                sample_rate,
            } => {
                let bytes = blobs
                    .get(&blob_ref)
                    .await
                    .map_err(|e| anyhow!("subflow_runner: audio blob fetch: {e}"))?;
                Ok(EnvelopeDelta::Audio(AudioStreamChunk {
                    choice_index: 0,
                    bytes_delta: bytes,
                    mime,
                    sample_rate,
                    finish_reason: Some(finish),
                }))
            }
            other => {
                let text_delta = match &other {
                    FlowValue::Text(t) => t.clone(),
                    FlowValue::Empty => String::new(),
                    v => serde_json::to_string(&crate::flow_engine::converter::payload_to_json(v))
                        .unwrap_or_default(),
                };
                Ok(EnvelopeDelta::Llm(LlmStreamChunk {
                    choice_index: 0,
                    text_delta,
                    reasoning_delta: None,
                    tool_calls: Vec::new(),
                    usage: Some(usage),
                    perf,
                    finish_reason: Some(finish),
                    error: err,
                }))
            }
        }
    })
    .boxed();
    // No producer ran here — the final envelope is the closest equivalent, it
    // carries the meta every node wrote on the way.
    let producer_input = Arc::new(outcome.final_envelope.clone());
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = tx.send(outcome);
    StreamingExecution {
        stream,
        outcome: rx,
        producer_input,
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::dispatcher::{
        build_registry_for_test, ActorKind, FlowActor, FlowOrigin, FlowRequestMeta,
    };
    use std::path::Path;

    fn db_with_flow(flow_id: &str, flow_json: &str) -> DbPool {
        let pool = crate::db::init(Path::new(":memory:")).expect("in-memory db");
        {
            let conn = pool.write().expect("db lock");
            conn.execute(
                "INSERT INTO flows (id, name, flow_json, status) VALUES (?1, 'body', ?2, 'active')",
                rusqlite::params![flow_id, flow_json],
            )
            .expect("seed flow");
        }
        pool
    }

    /// §2.5 — a sub-flow (and every `loop` / `map` / `agent` body, which share
    /// this runner) inherits the parent's provenance UNCHANGED, while the fields
    /// that must differ per child are rewritten.
    ///
    /// This drives the real `SubflowRunner::prepare`: it loads and compiles the
    /// body flow from the DB and builds the child context the executor will run
    /// under. Cloning a context inside the test would only prove that `Clone`
    /// copies fields, not that the runner leaves provenance alone.
    #[tokio::test]
    async fn subflow_child_context_inherits_parent_provenance() {
        let flow_json = r#"{
            "nodes":[
                {"id":"t1","type":"trigger","config":{}},
                {"id":"o1","type":"output","config":{}}
            ],
            "edges":[{"from":"t1","to":"o1","from_port":"text","to_port":"text"}]
        }"#;
        let db = db_with_flow("body-1", flow_json);
        let registry = Arc::new(build_registry_for_test());
        let runner = SubflowRunner::new(db, Arc::downgrade(&registry));

        let mut meta = FlowRequestMeta::new(
            "req-parent",
            FlowOrigin::CodeStudio,
            FlowActor::api_key("key-77", Some("u-3".to_string())),
        );
        meta.correlation_id = Some("corr-parent".into());
        let parent_ctx = crate::flow_engine::dispatcher::make_test_context(&meta);

        let prepared = runner
            .prepare("body-1", &parent_ctx, 1, false)
            .await
            .expect("prepare");
        let child = &prepared.child_ctx;

        assert_eq!(child.origin, FlowOrigin::CodeStudio);
        assert_eq!(child.actor_kind, ActorKind::ApiKey);
        assert_eq!(child.actor_id.as_deref(), Some("key-77"));
        assert_eq!(child.actor_user_id.as_deref(), Some("u-3"));
        assert_eq!(child.correlation_id.as_deref(), Some("corr-parent"));
        // The fields that MUST differ per child still do, so this is not simply
        // "prepare copies everything".
        assert_eq!(child.subflow_depth, parent_ctx.subflow_depth + 1);
        assert!(child.subflow_visited.contains(&"body-1".to_string()));
    }
}
