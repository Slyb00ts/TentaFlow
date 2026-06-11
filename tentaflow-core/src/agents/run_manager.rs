// ===== File: agents/run_manager.rs — AgentRunManager: background agent runs and
// the sub-agent control builtins (core.agent_spawn/wait/list/cancel, §3.6).
//
// A background run IS a flow execution: the manager spawns a tokio task that
// runs the agent's harness flow (agents.flow_id, default the seeded "Agent Run")
// through the flow dispatcher — there is NO second loop engine. The manager owns
// only the orchestration around that execution: an `agent_runs` row, a global
// concurrency semaphore, a per-run cancel token + status `watch` channel, and a
// heartbeat.
//
// Anti-livelock (§3.6): a parent that enters core.agent_wait flips to status
// `waiting` and RELEASES its global-cap semaphore permit, re-acquiring it on
// wake — so `cap+1` nested waits cannot deadlock the pool. Depth and per-parent
// fan-out caps come from the DB (`parent_run_id` chain, `max_spawn_depth`,
// `max_subagents`), never from model-supplied data.
//
// Restart semantics: on startup, rows left in running/waiting/waiting_user are
// marked `interrupted` (honest v1; resume is out of scope). Mailbox and
// auto-continuation are phase 7 and deliberately absent. =====

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use dashmap::DashMap;
use serde_json::{json, Value};
use tokio::sync::{watch, Semaphore};
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

use crate::db::models::{AgentRunStatusUpdate, NewAgentRun};
use crate::db::{repository, DbPool};
use crate::flow_engine::dispatchers::{ProgressEvent, ProgressSink};
use crate::flow_engine::envelope::{FlowEnvelope, FlowValue};
use crate::flow_engine::progress_broker::ProgressBroker;

use super::catalog::tool_in_allowlist;
use super::principal::AgentPrincipal;

/// Global concurrency cap setting key and default (§3.6). Bounds how many
/// background runs hold a semaphore permit at once across the whole process.
pub const MAX_CONCURRENT_RUNS_SETTING: &str = "agents.max_concurrent_runs";
const DEFAULT_MAX_CONCURRENT_RUNS: usize = 8;

/// Heartbeat cadence for a live run (§3.6) — the watchdog reads
/// `last_heartbeat_at`, so a long, quiet run still proves liveness.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Default agent_wait budget when the model omits `timeout_secs`.
const DEFAULT_WAIT_TIMEOUT_SECS: u64 = 600;

/// Terminal vs in-flight run status (mirrors the DB CHECK set). Carried on the
/// per-run `watch` channel so a waiter wakes the moment a child settles, no
/// polling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Queued,
    Running,
    Waiting,
    WaitingUser,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Queued => "queued",
            RunStatus::Running => "running",
            RunStatus::Waiting => "waiting",
            RunStatus::WaitingUser => "waiting_user",
            RunStatus::Completed => "completed",
            RunStatus::Failed => "failed",
            RunStatus::Cancelled => "cancelled",
            RunStatus::Interrupted => "interrupted",
        }
    }

    /// A run is terminal once it can no longer change — a waiter stops here.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            RunStatus::Completed
                | RunStatus::Failed
                | RunStatus::Cancelled
                | RunStatus::Interrupted
        )
    }
}

/// Runs the agent harness flow that backs one background run. Abstracted so the
/// manager's orchestration (semaphore, watch, cancel, heartbeat) is unit-testable
/// without a live `FlowDispatcher`. The production impl is `FlowDispatcherRunner`.
#[async_trait]
pub trait BackgroundFlowRunner: Send + Sync {
    /// Runs `flow_id` with `initial` as the trigger input under `principal`,
    /// governed by `deadline` and `cancel`. Returns the final answer text. The
    /// `agent_run_id` already lives in `initial.meta`, so the harness flow's
    /// `agent_context` reuses the manager-created row instead of opening a new
    /// one. `progress` is the run-scoped sink the harness emits node/tool events
    /// to; `scope` is the run id broadcast key.
    async fn run_agent_flow(
        &self,
        flow_id: String,
        initial: FlowEnvelope,
        principal: AgentPrincipal,
        deadline: Option<Instant>,
        cancel: CancellationToken,
        progress: Arc<dyn ProgressSink>,
        scope: String,
    ) -> Result<String>;
}

/// Shared slot owning a run's concurrency permit. `agent_wait` takes the permit
/// out (releasing the global slot) and puts a freshly reacquired one back on
/// wake; `run_task` drains it on completion (dropping the permit). Behind a
/// std Mutex (held only across a take/store, never across an await).
type PermitSlot = Arc<std::sync::Mutex<Option<tokio::sync::OwnedSemaphorePermit>>>;

/// One live run's control handle. The join handle aborts the tokio task on drop
/// (so a dropped registry tears down its runs); the `watch::Sender` publishes
/// status to waiters; the cancel token cooperatively stops the flow between
/// nodes; the permit slot is shared with the task and with agent_wait.
struct RunHandle {
    parent_run_id: Option<String>,
    status: watch::Sender<RunStatus>,
    cancel: CancellationToken,
    permit: PermitSlot,
    /// Held for its drop side effect (aborts the task). Read only via the
    /// status channel; the field exists to own the task's lifetime.
    _join: AbortOnDropHandle<()>,
}

/// One sub-agent task in a spawn request (single form folds into a 1-element
/// batch). `context` is optional extra text prepended to the task.
#[derive(Debug, Clone)]
struct SpawnTask {
    agent_name: String,
    task: String,
    context: Option<String>,
}

/// Background-run registry + concurrency governor (§3.6). Process-global (like
/// the progress broker): a run spawned on one WS connection is visible to a
/// waiter on another. Wired once at startup via `init_global`.
pub struct AgentRunManager {
    db: DbPool,
    runner: Arc<dyn BackgroundFlowRunner>,
    progress: Arc<ProgressBroker>,
    /// Global concurrency permit pool. A running task holds one permit; a parent
    /// parked in agent_wait releases its permit (anti-livelock).
    semaphore: Arc<Semaphore>,
    /// Live-run registry, shared with each spawned task so the task can evict
    /// its own entry on completion (dropping its permit + abort handle).
    runs: Arc<DashMap<String, RunHandle>>,
}

/// Process-global manager. Mirrors `progress_broker::global_broker` — one
/// instance shared by every AppState so background runs survive past the
/// connection that started them.
static GLOBAL: OnceLock<Arc<AgentRunManager>> = OnceLock::new();

/// Installs the process-global manager. Idempotent: a second call returns the
/// already-installed instance (the first wins), so a re-entrant startup never
/// forks the registry. Call once after the FlowDispatcher exists.
pub fn init_global(manager: Arc<AgentRunManager>) -> Arc<AgentRunManager> {
    let _ = GLOBAL.set(manager);
    GLOBAL.get().expect("manager just set").clone()
}

/// The process-global manager, if installed. `None` on headless deploys / tests
/// that never wired one — tool_exec then refuses the sub-agent builtins with a
/// recoverable tool error.
pub fn global() -> Option<Arc<AgentRunManager>> {
    GLOBAL.get().cloned()
}

impl AgentRunManager {
    /// Builds a manager with an explicit concurrency cap (used by `from_setting`
    /// after reading `agents.max_concurrent_runs`, and directly by tests).
    pub fn new(
        db: DbPool,
        runner: Arc<dyn BackgroundFlowRunner>,
        progress: Arc<ProgressBroker>,
        max_concurrent_runs: usize,
    ) -> Self {
        let cap = max_concurrent_runs.max(1);
        Self {
            db,
            runner,
            progress,
            semaphore: Arc::new(Semaphore::new(cap)),
            runs: Arc::new(DashMap::new()),
        }
    }

    fn runs_ref(&self) -> Arc<DashMap<String, RunHandle>> {
        self.runs.clone()
    }

    /// Builds a manager reading the concurrency cap from settings (default 8).
    pub fn from_setting(
        db: DbPool,
        runner: Arc<dyn BackgroundFlowRunner>,
        progress: Arc<ProgressBroker>,
    ) -> Self {
        let cap = repository::get_setting(&db, MAX_CONCURRENT_RUNS_SETTING)
            .ok()
            .flatten()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_MAX_CONCURRENT_RUNS);
        Self::new(db, runner, progress, cap)
    }

    /// Marks orphaned rows (running/waiting/waiting_user) `interrupted` on
    /// startup (§3.6). The previous process owned the tokio tasks that backed
    /// them; this process cannot resume, so they are honestly closed out.
    /// Returns the number of rows reaped.
    pub fn reap_interrupted_on_startup(db: &DbPool) -> Result<usize> {
        // Every non-terminal row (queued/running/waiting/waiting_user) is dead on
        // restart — the previous process owned its task. Close them all out.
        let orphans = repository::list_active_agent_runs(db)?;
        let mut reaped = 0;
        for run in &orphans {
            repository::update_agent_run_status(
                db,
                &run.id,
                &AgentRunStatusUpdate {
                    status: RunStatus::Interrupted.as_str(),
                    exit_reason: Some("interrupted"),
                    set_finished: true,
                    ..Default::default()
                },
            )?;
            reaped += 1;
        }
        Ok(reaped)
    }

    /// Spawn-tree depth of a run, counted as the number of `parent_run_id` hops
    /// from this run up to a top-level run (not the model's word — read from the
    /// DB). A top-level run is depth 0, its children depth 1, grandchildren depth
    /// 2. A would-be child sits at `caller_depth + 1`, which must be
    /// `<= max_spawn_depth` (so `max_spawn_depth = 1` permits one level of
    /// children that may not themselves spawn).
    fn spawn_depth_of(&self, run_id: &str) -> usize {
        let mut depth = 0usize;
        let mut current = run_id.to_string();
        // The guard bounds the walk if a cycle were ever persisted.
        let mut guard = 0;
        while guard < 64 {
            guard += 1;
            match repository::get_agent_run(&self.db, &current) {
                Ok(Some(run)) => match run.parent_run_id {
                    Some(p) if !p.is_empty() => {
                        depth += 1;
                        current = p;
                    }
                    _ => break,
                },
                _ => break,
            }
        }
        depth
    }

    /// Count of this parent's children that are not yet terminal — enforced
    /// against `max_subagents`.
    fn active_child_count(&self, parent_run_id: &str) -> usize {
        repository::list_agent_runs_by_parent(&self.db, parent_run_id)
            .map(|rows| {
                rows.iter()
                    .filter(|r| {
                        matches!(
                            r.status.as_str(),
                            "queued" | "running" | "waiting" | "waiting_user"
                        )
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    /// Spawns one background run. Creates the `agent_runs` row (`queued`),
    /// acquires a global permit, then launches a tokio task that runs the agent's
    /// harness flow. Returns the run id immediately — the task outlives this call.
    ///
    /// `inherited_tools` is the JSON allowlist a child is restricted to
    /// (`tools(child) ∩ tools(parent)`); it is persisted indirectly via the
    /// agent definition, so the parameter records the intersection the spawn was
    /// authorized under (used to reject an over-broad child before launch).
    pub async fn spawn(
        &self,
        agent_id: &str,
        prompt: &str,
        parent_run_id: Option<&str>,
        principal: &AgentPrincipal,
        inherited_tools: &[String],
    ) -> Result<String> {
        let agent = repository::get_agent(&self.db, agent_id)?
            .ok_or_else(|| anyhow!("agent '{agent_id}' not found"))?;
        if !agent.is_enabled {
            return Err(anyhow!("agent '{agent_id}' is disabled"));
        }

        // Child tools must be a subset of the inherited (parent) surface (§3.6).
        if parent_run_id.is_some() {
            self.assert_tools_subset(&agent.tools_json, inherited_tools)?;
        }

        let run_id = uuid::Uuid::new_v4().to_string();
        repository::create_agent_run(
            &self.db,
            &NewAgentRun {
                id: &run_id,
                agent_id: &agent.id,
                parent_run_id,
                flow_execution_id: None,
                user_id: principal.user_id(),
                org_id: principal.org_id.as_deref(),
                prompt,
            },
        )?;

        let flow_id = agent
            .flow_id
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(crate::flow_engine::node_adapters::AGENT_RUN_FLOW_ID)
            .to_string();

        // The permit is acquired INSIDE the task, not here: a saturated pool must
        // not block `spawn` (a parent dispatching a batch returns immediately, the
        // surplus children queue FIFO — §3.6). The slot starts empty; the task
        // fills it once it wins a permit. agent_wait drains/refills the slot.
        let permit_slot: PermitSlot = Arc::new(std::sync::Mutex::new(None));

        let (status_tx, _status_rx) = watch::channel(RunStatus::Queued);
        let cancel = CancellationToken::new();

        // Initial envelope: the prompt is the trigger payload; meta carries the
        // agent id + the pre-created run id so the harness flow's agent_context
        // reuses this row rather than minting a fresh one.
        let mut initial = FlowEnvelope::empty();
        initial.payload = FlowValue::Text(prompt.to_string());
        initial
            .meta
            .insert("agent_id".into(), Value::String(agent.id.clone()));
        initial
            .meta
            .insert("agent_run_id".into(), Value::String(run_id.clone()));

        let deadline = (agent.timeout_secs > 0)
            .then(|| Instant::now() + Duration::from_secs(agent.timeout_secs as u64));

        let ctx = TaskContext {
            db: self.db.clone(),
            runner: self.runner.clone(),
            progress: self.progress.clone(),
            runs: self.runs_ref(),
            semaphore: self.semaphore.clone(),
            run_id: run_id.clone(),
            parent_run_id: parent_run_id.map(|s| s.to_string()),
            flow_id,
            initial,
            principal: principal.clone(),
            deadline,
            cancel: cancel.clone(),
            status: status_tx.clone(),
            permit: permit_slot.clone(),
        };

        let join = tokio::spawn(run_task(ctx));
        self.runs.insert(
            run_id.clone(),
            RunHandle {
                parent_run_id: parent_run_id.map(|s| s.to_string()),
                status: status_tx,
                cancel,
                permit: permit_slot,
                _join: AbortOnDropHandle::new(join),
            },
        );

        // Notify any subscriber on the parent's scope that a child appeared.
        if let Some(parent) = parent_run_id {
            self.progress.publish(
                parent,
                ProgressEvent::ChildSpawned {
                    run_id: run_id.clone(),
                    agent: agent.name.clone(),
                },
            );
        }

        Ok(run_id)
    }

    /// `core.agent_spawn` handler. Parses the single-or-batch argument shape,
    /// enforces depth + per-parent fan-out caps from the DB, intersects the
    /// child tools with the caller's, and returns `{run_ids}`. Caps that would
    /// be exceeded fail the call (the model sees a recoverable tool error), they
    /// do not silently drop tasks.
    pub async fn handle_agent_spawn(
        &self,
        caller: &CallerRun,
        args: &Value,
    ) -> Result<Value> {
        let tasks = parse_spawn_tasks(args)?;
        if tasks.is_empty() {
            return Err(anyhow!("agent_spawn: no tasks provided"));
        }

        let parent = repository::get_agent(&self.db, &caller.agent_id)?
            .ok_or_else(|| anyhow!("agent_spawn: caller agent not found"))?;
        if parent.max_subagents <= 0 {
            return Err(anyhow!("agent_spawn: this agent may not spawn sub-agents"));
        }

        // Depth cap: a child sits one level below this run; reject before the
        // child would exceed the parent agent's configured max_spawn_depth.
        let caller_depth = self.spawn_depth_of(&caller.run_id);
        if caller_depth + 1 > parent.max_spawn_depth as usize {
            return Err(anyhow!(
                "agent_spawn: spawn depth {} would exceed max_spawn_depth {}",
                caller_depth + 1,
                parent.max_spawn_depth
            ));
        }

        // Fan-out cap: existing active children + this batch must fit
        // max_subagents.
        let active = self.active_child_count(&caller.run_id);
        if active + tasks.len() > parent.max_subagents as usize {
            return Err(anyhow!(
                "agent_spawn: {} active + {} requested children exceed max_subagents {}",
                active,
                tasks.len(),
                parent.max_subagents
            ));
        }

        let parent_tools: Vec<String> =
            serde_json::from_str(&parent.tools_json).unwrap_or_default();

        let mut run_ids = Vec::with_capacity(tasks.len());
        for task in tasks {
            let child = repository::get_agent_by_name(&self.db, &task.agent_name)?
                .ok_or_else(|| anyhow!("agent_spawn: agent '{}' not found", task.agent_name))?;
            let prompt = match &task.context {
                Some(c) if !c.is_empty() => format!("{c}\n\n{}", task.task),
                _ => task.task.clone(),
            };
            let run_id = self
                .spawn(
                    &child.id,
                    &prompt,
                    Some(&caller.run_id),
                    &caller.principal,
                    &parent_tools,
                )
                .await?;
            run_ids.push(run_id);
        }

        Ok(json!({ "run_ids": run_ids }))
    }

    /// `core.agent_wait` handler. Waits for each named run to settle on its
    /// `watch` channel (no polling), bounded by `timeout_secs`. ANTI-LIVELOCK:
    /// the caller's run flips to `waiting` and releases its global permit for the
    /// duration, re-acquiring on wake — so `cap+1` nested waits never deadlock.
    /// Only children of the caller may be waited on (a run cannot await an
    /// unrelated run's result).
    pub async fn handle_agent_wait(&self, caller: &CallerRun, args: &Value) -> Result<Value> {
        let run_ids: Vec<String> = args
            .get("run_ids")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();
        if run_ids.is_empty() {
            return Err(anyhow!("agent_wait: run_ids required"));
        }
        let timeout_secs = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_WAIT_TIMEOUT_SECS);

        // Only wait on the caller's own children.
        for id in &run_ids {
            if !self.is_child_of(id, &caller.run_id) {
                return Err(anyhow!(
                    "agent_wait: run '{id}' is not a child of the calling run"
                ));
            }
        }

        // Release the caller's permit and flip to `waiting` so the pool is not
        // starved while we block (§3.6). The permit slot lives in the caller's
        // registry handle; we take the permit out (returning the slot to the
        // pool) and reacquire on wake. A caller with no registered handle (a
        // foreground flow calling agent_wait, not a manager-owned task) holds no
        // permit — there is nothing to release, and the wait simply blocks.
        let held_permit = self
            .runs
            .get(&caller.run_id)
            .and_then(|h| h.permit.lock().ok().and_then(|mut p| p.take()));
        let had_permit = held_permit.is_some();
        drop(held_permit); // releasing the slot for queued siblings
        if had_permit {
            self.set_status(&caller.run_id, RunStatus::Waiting, "waiting");
        }

        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        let mut results = serde_json::Map::new();
        for id in &run_ids {
            let entry = self.wait_one(id, deadline).await;
            results.insert(id.clone(), entry);
        }

        // Reacquire a permit before resuming (the caller's flow keeps running
        // after agent_wait returns). If the pool is saturated this blocks, which
        // is the intended back-pressure; the deadline already bounded the wait.
        //
        // A cancel that fired while the caller was parked here already wrote the
        // terminal status and signalled the cancel token (`cancel` does not drain
        // the permit slot). Reacquiring + flipping to `running` would clobber that
        // terminal row back to a live state and re-take a global slot for a run
        // that is finished — so skip the resume when the caller is already
        // terminal/cancelled and let `run_task` finalize. A handle ALREADY
        // evicted from the registry (the caller's own `run_task` settled and ran
        // `runs.remove`) is terminal too — `None` here means "do not resume".
        let caller_terminal = self
            .runs
            .get(&caller.run_id)
            .map(|h| h.cancel.is_cancelled() || h.status.borrow().is_terminal())
            .unwrap_or(true);
        if had_permit && !caller_terminal {
            let fresh = Arc::clone(&self.semaphore)
                .acquire_owned()
                .await
                .map_err(|_| anyhow!("run manager semaphore closed"))?;
            if let Some(handle) = self.runs.get(&caller.run_id) {
                if let Ok(mut slot) = handle.permit.lock() {
                    *slot = Some(fresh);
                }
            }
            self.set_status(&caller.run_id, RunStatus::Running, "running");
        }

        Ok(Value::Object(results))
    }

    /// Blocks until run `id` settles or `deadline` passes. Reads the live
    /// `watch` channel when the run is in-registry, else falls back to the DB
    /// (a run that finished before this wait subscribed). Returns
    /// `{status, result?}`.
    async fn wait_one(&self, id: &str, deadline: Instant) -> Value {
        // Subscribe first so we never miss a transition between the DB read and
        // the channel borrow.
        let rx = self.runs.get(id).map(|h| h.status.subscribe());
        if let Some(mut rx) = rx {
            loop {
                let current = *rx.borrow();
                if current.is_terminal() {
                    return self.terminal_result(id, current);
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return json!({ "status": current.as_str(), "timed_out": true });
                }
                match tokio::time::timeout(remaining, rx.changed()).await {
                    // Channel changed — re-read the loop head for the new value.
                    Ok(Ok(())) => continue,
                    // Sender dropped (the task finished and evicted its handle):
                    // the terminal row is authoritative.
                    Ok(Err(_)) => return self.db_result(id),
                    // Wait budget elapsed.
                    Err(_) => {
                        let now = *rx.borrow();
                        return json!({ "status": now.as_str(), "timed_out": true });
                    }
                }
            }
        }
        // Not in registry — read the persisted row directly.
        self.db_result(id)
    }

    /// Reads a run's status + result straight from the persisted row — the
    /// authoritative answer once the live task has evicted its handle.
    fn db_result(&self, id: &str) -> Value {
        match repository::get_agent_run(&self.db, id) {
            Ok(Some(run)) => json!({
                "status": run.status,
                "result": run.result,
            }),
            _ => json!({ "status": "unknown" }),
        }
    }

    fn terminal_result(&self, id: &str, status: RunStatus) -> Value {
        let result = repository::get_agent_run(&self.db, id)
            .ok()
            .flatten()
            .and_then(|r| r.result);
        json!({ "status": status.as_str(), "result": result })
    }

    /// Enters `waiting_user` for a run blocked on a human interaction (§3.13):
    /// flips status to `waiting_user` and RELEASES its global concurrency permit
    /// (same anti-livelock rule as agent_wait — a run parked on a person must not
    /// hold a pool slot). Returns whether a permit was actually released, so the
    /// caller knows to reacquire one on resume (a foreground / unmanaged run has
    /// no permit and simply blocks). The heartbeat ticker keeps running, so the
    /// watchdog does not reap a run that is merely waiting on input.
    pub fn enter_waiting_user(&self, run_id: &str) -> bool {
        let held_permit = self
            .runs
            .get(run_id)
            .and_then(|h| h.permit.lock().ok().and_then(|mut p| p.take()));
        let had_permit = held_permit.is_some();
        drop(held_permit); // releasing the slot for queued siblings
        if had_permit {
            self.set_status(run_id, RunStatus::WaitingUser, "waiting_user");
        }
        had_permit
    }

    /// Resumes a run from `waiting_user` after a human reply (or timeout):
    /// reacquires a global permit (this awaits if the pool is saturated — the
    /// intended back-pressure) and flips status back to `running`. A no-op when
    /// the run held no permit (it was never managed) — `had_permit` is the value
    /// `enter_waiting_user` returned.
    pub async fn exit_waiting_user(&self, run_id: &str, had_permit: bool) -> Result<()> {
        if !had_permit {
            return Ok(());
        }
        let fresh = Arc::clone(&self.semaphore)
            .acquire_owned()
            .await
            .map_err(|_| anyhow!("run manager semaphore closed"))?;
        if let Some(handle) = self.runs.get(run_id) {
            if let Ok(mut slot) = handle.permit.lock() {
                *slot = Some(fresh);
            }
        }
        self.set_status(run_id, RunStatus::Running, "running");
        Ok(())
    }

    /// `core.agent_list` handler — active children of the caller.
    pub fn handle_agent_list(&self, caller: &CallerRun) -> Result<Value> {
        let children = repository::list_agent_runs_by_parent(&self.db, &caller.run_id)?;
        let active: Vec<Value> = children
            .iter()
            .filter(|r| {
                matches!(
                    r.status.as_str(),
                    "queued" | "running" | "waiting" | "waiting_user"
                )
            })
            .map(|r| {
                json!({
                    "run_id": r.id,
                    "agent_id": r.agent_id,
                    "status": r.status,
                })
            })
            .collect();
        Ok(json!({ "runs": active }))
    }

    /// `core.agent_cancel` handler — cancels one child run of the caller.
    pub fn handle_agent_cancel(&self, caller: &CallerRun, args: &Value) -> Result<Value> {
        let run_id = args
            .get("run_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("agent_cancel: run_id required"))?;
        if !self.is_child_of(run_id, &caller.run_id) {
            return Err(anyhow!(
                "agent_cancel: run '{run_id}' is not a child of the calling run"
            ));
        }
        let cancelled = self.cancel(run_id);
        Ok(json!({ "run_id": run_id, "cancelled": cancelled }))
    }

    /// Cancels a run by id: signals its cancel token (the flow stops between
    /// nodes) and marks the row `cancelled`. Returns true when a live run was
    /// signalled. The task itself finalizes the row in `run_task`, but we set
    /// the status here too so a caller observing immediately sees `cancelled`.
    pub fn cancel(&self, run_id: &str) -> bool {
        // Cancellation closes any open questions/grants this run raised so the
        // awaiting tool degrades (sentinel / deny) instead of hanging (§3.13 A).
        super::interaction::global().drop_run(run_id);
        let Some(handle) = self.runs.get(run_id) else {
            return false;
        };
        handle.cancel.cancel();
        let _ = handle.status.send(RunStatus::Cancelled);
        let _ = repository::update_agent_run_status(
            &self.db,
            run_id,
            &AgentRunStatusUpdate {
                status: RunStatus::Cancelled.as_str(),
                exit_reason: Some("cancelled"),
                set_finished: true,
                ..Default::default()
            },
        );
        true
    }

    /// Number of currently registered (in-flight) runs — test/diagnostic hook.
    pub fn live_run_count(&self) -> usize {
        self.runs.len()
    }

    fn set_status(&self, run_id: &str, status: RunStatus, db_status: &str) {
        if let Some(handle) = self.runs.get(run_id) {
            let _ = handle.status.send(status);
        }
        let _ = repository::update_agent_run_status(
            &self.db,
            run_id,
            &AgentRunStatusUpdate {
                status: db_status,
                ..Default::default()
            },
        );
    }

    fn is_child_of(&self, run_id: &str, parent_run_id: &str) -> bool {
        if let Some(handle) = self.runs.get(run_id) {
            if let Some(p) = &handle.parent_run_id {
                return p == parent_run_id;
            }
            return false;
        }
        repository::get_agent_run(&self.db, run_id)
            .ok()
            .flatten()
            .and_then(|r| r.parent_run_id)
            .map(|p| p == parent_run_id)
            .unwrap_or(false)
    }

    fn assert_tools_subset(&self, child_tools_json: &str, parent_tools: &[String]) -> Result<()> {
        let child: Vec<String> = serde_json::from_str(child_tools_json).unwrap_or_default();
        let parent_json = serde_json::to_string(parent_tools).unwrap_or_else(|_| "[]".into());
        for tool in &child {
            // A child tool is admissible if the parent allowlist admits it (an
            // addon wildcard on the parent covers the child's exact tool).
            if !tool_in_allowlist(&parent_json, tool) && !parent_tools.contains(tool) {
                return Err(anyhow!(
                    "agent_spawn: child tool '{tool}' is outside the parent's tool surface"
                ));
            }
        }
        Ok(())
    }
}

/// The calling run's identity, threaded into every builtin handler. `run_id` and
/// `agent_id` come from the harness flow's `meta`; `principal` from the run's
/// `ExecutionContext`. The caller's concurrency permit (if any) lives in its
/// registry handle, so agent_wait finds and releases it by `run_id` — the caller
/// does not carry it.
#[derive(Clone)]
pub struct CallerRun {
    pub run_id: String,
    pub agent_id: String,
    pub principal: AgentPrincipal,
}

/// Everything the spawned task needs. Built in `spawn`, consumed by `run_task`.
struct TaskContext {
    db: DbPool,
    runner: Arc<dyn BackgroundFlowRunner>,
    progress: Arc<ProgressBroker>,
    runs: Arc<DashMap<String, RunHandle>>,
    semaphore: Arc<Semaphore>,
    run_id: String,
    parent_run_id: Option<String>,
    flow_id: String,
    initial: FlowEnvelope,
    principal: AgentPrincipal,
    deadline: Option<Instant>,
    cancel: CancellationToken,
    status: watch::Sender<RunStatus>,
    permit: PermitSlot,
}

/// The background task body: acquire a concurrency permit (queuing FIFO while
/// the pool is saturated), drive the agent harness flow to completion,
/// heartbeating and publishing status, then finalize the row. The permit lives
/// in a shared slot (released on completion / temporarily by agent_wait). On
/// completion the task evicts its own registry entry, dropping the permit and
/// the abort handle.
async fn run_task(ctx: TaskContext) {
    let TaskContext {
        db,
        runner,
        progress,
        runs,
        semaphore,
        run_id,
        parent_run_id,
        flow_id,
        initial,
        principal,
        deadline,
        cancel,
        status,
        permit,
    } = ctx;

    // Acquire the global permit before running the flow. While the pool is full
    // the task parks here in `queued`; a cancel before a permit is won still
    // aborts cleanly (the run is finalized cancelled below).
    let acquired = tokio::select! {
        _ = cancel.cancelled() => None,
        p = Arc::clone(&semaphore).acquire_owned() => p.ok(),
    };
    match acquired {
        Some(p) => {
            if let Ok(mut slot) = permit.lock() {
                *slot = Some(p);
            }
        }
        None => {
            // Cancelled (or semaphore closed) before starting — finalize and exit.
            let _ = repository::update_agent_run_status(
                &db,
                &run_id,
                &AgentRunStatusUpdate {
                    status: RunStatus::Cancelled.as_str(),
                    exit_reason: Some("cancelled"),
                    set_finished: true,
                    ..Default::default()
                },
            );
            let _ = status.send(RunStatus::Cancelled);
            publish_child_finished(
                &progress,
                &run_id,
                parent_run_id.as_deref(),
                RunStatus::Cancelled,
            );
            runs.remove(&run_id);
            return;
        }
    }

    let _ = status.send(RunStatus::Running);
    let _ = repository::update_agent_run_status(
        &db,
        &run_id,
        &AgentRunStatusUpdate {
            status: RunStatus::Running.as_str(),
            set_started: true,
            ..Default::default()
        },
    );

    // Heartbeat ticker — proves liveness for the watchdog while the flow runs.
    let hb_db = db.clone();
    let hb_run = run_id.clone();
    let hb_cancel = cancel.clone();
    let heartbeat = tokio::spawn(async move {
        let mut tick = tokio::time::interval(HEARTBEAT_INTERVAL);
        loop {
            tokio::select! {
                _ = hb_cancel.cancelled() => break,
                _ = tick.tick() => {
                    let _ = repository::touch_agent_run_heartbeat(&hb_db, &hb_run);
                }
            }
        }
    });
    let _heartbeat = AbortOnDropHandle::new(heartbeat);

    let sink: Arc<dyn ProgressSink> =
        Arc::new(crate::flow_engine::progress_broker::BrokerProgressSink::new(progress.clone()));

    let outcome = runner
        .run_agent_flow(
            flow_id,
            initial,
            principal,
            deadline,
            cancel.clone(),
            sink,
            run_id.clone(),
        )
        .await;

    let (final_status, exit_reason, result_text) = if cancel.is_cancelled() {
        (RunStatus::Cancelled, "cancelled".to_string(), None)
    } else {
        match outcome {
            Ok(text) => (RunStatus::Completed, "final_response".to_string(), Some(text)),
            Err(e) => (RunStatus::Failed, format!("error:{e}"), None),
        }
    };

    let _ = repository::update_agent_run_status(
        &db,
        &run_id,
        &AgentRunStatusUpdate {
            status: final_status.as_str(),
            result: result_text.as_deref(),
            exit_reason: Some(&exit_reason),
            set_finished: true,
            ..Default::default()
        },
    );
    let _ = status.send(final_status);

    // Drop any open interactions + per-run permission grants this run earned —
    // a settled run leaves no stale waiting questions or cached grants (§3.13).
    let interactions = super::interaction::global();
    interactions.drop_run(&run_id);
    interactions.clear_run_grants(&run_id);

    // Notify both the child's own scope and the parent's scope that this child
    // settled (the parent flow subscribes to its own run id; the dashboard may
    // subscribe to the child directly). The parent's agent_wait wakes off the
    // watch channel, not this event, so ordering vs. evict is safe.
    publish_child_finished(&progress, &run_id, parent_run_id.as_deref(), final_status);

    // Drop the permit (returns the slot to the pool) before evicting so a queued
    // sibling can acquire it. Then evict the registry entry — any in-flight
    // waiter already holds its own watch::Receiver and observed the terminal
    // value above, so dropping the sender here is safe. Evicting drops the
    // AbortOnDropHandle for THIS finished task (a no-op: the future already
    // completed) and the permit slot.
    {
        let _ = permit.lock().map(|mut p| p.take());
    }
    runs.remove(&run_id);
}

/// Publishes `ChildFinished` to the run's own scope and, when the run has a
/// parent, to the parent's scope too (where the matching `ChildSpawned` went).
fn publish_child_finished(
    progress: &ProgressBroker,
    run_id: &str,
    parent_run_id: Option<&str>,
    status: RunStatus,
) {
    let event = ProgressEvent::ChildFinished {
        run_id: run_id.to_string(),
        status: status.as_str().to_string(),
    };
    progress.publish(run_id, event.clone());
    if let Some(parent) = parent_run_id {
        progress.publish(parent, event);
    }
}

/// Parses the spawn argument into a list of tasks (single or batch form).
fn parse_spawn_tasks(args: &Value) -> Result<Vec<SpawnTask>> {
    // Batch form wins when present.
    if let Some(arr) = args.get("tasks").and_then(|v| v.as_array()) {
        let mut out = Vec::with_capacity(arr.len());
        for entry in arr {
            out.push(parse_one_task(entry)?);
        }
        return Ok(out);
    }
    // Single form.
    Ok(vec![parse_one_task(args)?])
}

fn parse_one_task(entry: &Value) -> Result<SpawnTask> {
    let agent_name = entry
        .get("agent_name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("agent_spawn: 'agent_name' required"))?
        .to_string();
    let task = entry
        .get("task")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("agent_spawn: 'task' required"))?
        .to_string();
    let context = entry
        .get("context")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);
    Ok(SpawnTask {
        agent_name,
        task,
        context,
    })
}

/// Production runner: drives the agent harness flow through the FlowDispatcher's
/// background path (no 120 s cap, governed by deadline + cancel). Holds a `Weak`
/// to the dispatcher to avoid an ownership cycle with AppState; a dropped
/// dispatcher (shutdown) surfaces as a run error.
pub struct FlowDispatcherRunner {
    dispatcher: std::sync::Weak<crate::flow_engine::dispatcher::FlowDispatcher>,
}

impl FlowDispatcherRunner {
    pub fn new(dispatcher: &Arc<crate::flow_engine::dispatcher::FlowDispatcher>) -> Self {
        Self {
            dispatcher: Arc::downgrade(dispatcher),
        }
    }
}

#[async_trait]
impl BackgroundFlowRunner for FlowDispatcherRunner {
    async fn run_agent_flow(
        &self,
        flow_id: String,
        initial: FlowEnvelope,
        principal: AgentPrincipal,
        deadline: Option<Instant>,
        cancel: CancellationToken,
        progress: Arc<dyn ProgressSink>,
        scope: String,
    ) -> Result<String> {
        let dispatcher = self
            .dispatcher
            .upgrade()
            .ok_or_else(|| anyhow!("flow dispatcher dropped (shutdown)"))?;

        // Scope = run id so progress events fan out under the run id (§3.6); the
        // session id is unset for a background run.
        let meta = crate::flow_engine::dispatcher::FlowRequestMeta {
            request_id: scope.clone(),
            session_id: Some(scope),
            user_id: principal.user_id().map(String::from),
            user_role: None,
            deadline,
            cancel_token: cancel,
            progress_sink: Some(progress),
        };

        let outcome = dispatcher
            .dispatch_by_flow_id_background(flow_id, initial, meta)
            .await
            .map_err(|e| anyhow!("agent flow dispatch failed: {e}"))?;
        if let Some(err) = outcome.error {
            return Err(anyhow!("agent flow failed: {err}"));
        }
        Ok(outcome
            .final_envelope
            .payload
            .as_text()
            .unwrap_or("")
            .to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations;
    use crate::db::models::AgentParams;
    use std::sync::Mutex;

    fn db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        migrations::run(&conn).expect("migrations");
        Arc::new(Mutex::new(conn))
    }

    #[allow(clippy::too_many_arguments)]
    fn seed_agent(
        pool: &DbPool,
        id: &str,
        name: &str,
        tools: &str,
        max_subagents: i64,
        max_spawn_depth: i64,
    ) {
        repository::upsert_agent(
            pool,
            &AgentParams {
                id,
                name,
                display_name: None,
                description: "test agent",
                system_prompt: None,
                model: None,
                tools_json: tools,
                skills_json: "{}",
                params_json: "{}",
                max_iterations: 5,
                timeout_secs: 600,
                max_subagents,
                max_spawn_depth,
                flow_id: Some("11111111-0000-4000-8000-000000000099"),
                routable: true,
                is_enabled: true,
                actor_user_id: None,
            },
        )
        .expect("seed agent");
    }

    /// Test gate: level-triggered (no lost wakeups) — the runner waits until the
    /// shared flag flips true. A `watch` channel makes the release idempotent and
    /// safe even when the waiter has not yet parked.
    #[derive(Clone)]
    struct Gate {
        rx: watch::Receiver<bool>,
        tx: Arc<watch::Sender<bool>>,
    }

    impl Gate {
        fn new() -> Self {
            let (tx, rx) = watch::channel(false);
            Self {
                rx,
                tx: Arc::new(tx),
            }
        }
        fn open(&self) {
            let _ = self.tx.send(true);
        }
        async fn wait(&self) {
            let mut rx = self.rx.clone();
            // Return as soon as the flag is (or becomes) true.
            while !*rx.borrow() {
                if rx.changed().await.is_err() {
                    break;
                }
            }
        }
    }

    /// Controllable runner: each run blocks on the shared gate until the test
    /// opens it, then returns a fixed result. Lets a test assert spawn returns
    /// immediately and that the run completes only after release.
    struct GatedRunner {
        gate: Gate,
        /// When true, the runner observes the cancel token and returns early
        /// (simulating a cancellable flow); else it waits on the gate.
        honor_cancel: bool,
    }

    #[async_trait]
    impl BackgroundFlowRunner for GatedRunner {
        async fn run_agent_flow(
            &self,
            _flow_id: String,
            _initial: FlowEnvelope,
            _principal: AgentPrincipal,
            _deadline: Option<Instant>,
            cancel: CancellationToken,
            _progress: Arc<dyn ProgressSink>,
            scope: String,
        ) -> Result<String> {
            if self.honor_cancel {
                tokio::select! {
                    _ = cancel.cancelled() => return Err(anyhow!("cancelled")),
                    _ = self.gate.wait() => {}
                }
            } else {
                self.gate.wait().await;
            }
            Ok(format!("result-of-{scope}"))
        }
    }

    /// A runner that completes instantly with a fixed result.
    struct InstantRunner;
    #[async_trait]
    impl BackgroundFlowRunner for InstantRunner {
        async fn run_agent_flow(
            &self,
            _flow_id: String,
            _initial: FlowEnvelope,
            _principal: AgentPrincipal,
            _deadline: Option<Instant>,
            _cancel: CancellationToken,
            _progress: Arc<dyn ProgressSink>,
            scope: String,
        ) -> Result<String> {
            Ok(format!("done-{scope}"))
        }
    }

    fn manager(db: DbPool, runner: Arc<dyn BackgroundFlowRunner>, cap: usize) -> Arc<AgentRunManager> {
        Arc::new(AgentRunManager::new(
            db,
            runner,
            Arc::new(ProgressBroker::new()),
            cap,
        ))
    }

    #[tokio::test]
    async fn spawn_returns_immediately_and_child_completes() {
        let pool = db();
        seed_agent(&pool, "a1", "worker", "[]", 0, 1);
        let gate = Gate::new();
        let mgr = manager(
            pool.clone(),
            Arc::new(GatedRunner {
                gate: gate.clone(),
                honor_cancel: false,
            }),
            8,
        );
        let principal = AgentPrincipal::user("u1");

        let run_id = mgr
            .spawn("a1", "do it", None, &principal, &[])
            .await
            .expect("spawn");

        // The run exists in queued/running while the runner is gated.
        let row = repository::get_agent_run(&pool, &run_id)
            .expect("get")
            .expect("row");
        assert!(matches!(row.status.as_str(), "queued" | "running"));

        // Release the runner; the row settles to completed with the result.
        gate.open();
        wait_until_status(&pool, &run_id, "completed").await;
        let row = repository::get_agent_run(&pool, &run_id)
            .expect("get")
            .expect("row");
        assert_eq!(row.result.as_deref(), Some(&*format!("result-of-{run_id}")));
    }

    #[tokio::test]
    async fn agent_wait_blocks_then_returns_child_result() {
        let pool = db();
        seed_agent(&pool, "parent", "boss", "[]", 4, 2);
        seed_agent(&pool, "child", "worker", "[]", 0, 1);
        let gate = Gate::new();
        let mgr = manager(
            pool.clone(),
            Arc::new(GatedRunner {
                gate: gate.clone(),
                honor_cancel: false,
            }),
            8,
        );
        let principal = AgentPrincipal::user("u1");

        // A parent run row (created directly — the parent's own flow is elsewhere).
        let parent_run = mgr
            .spawn("parent", "lead", None, &principal, &[])
            .await
            .expect("spawn parent");

        let caller = CallerRun {
            run_id: parent_run.clone(),
            agent_id: "parent".into(),
            principal: principal.clone(),
        };
        let spawn_out = mgr
            .handle_agent_spawn(
                &caller,
                &json!({"agent_name": "worker", "task": "subtask"}),
            )
            .await
            .expect("spawn child");
        let child_id = spawn_out["run_ids"][0].as_str().unwrap().to_string();

        // agent_wait must block until the child is released.
        let mgr2 = mgr.clone();
        let caller2 = caller.clone();
        let child_for_wait = child_id.clone();
        let wait = tokio::spawn(async move {
            mgr2.handle_agent_wait(
                &caller2,
                &json!({"run_ids": [child_for_wait], "timeout_secs": 30}),
            )
            .await
        });

        // Give the wait a moment to park, then release the gate for everyone.
        tokio::time::sleep(Duration::from_millis(50)).await;
        gate.open();

        let result = wait.await.expect("join").expect("wait ok");
        assert_eq!(
            result[&child_id]["status"].as_str(),
            Some("completed"),
            "got {result}"
        );
        assert_eq!(
            result[&child_id]["result"].as_str(),
            Some(&*format!("result-of-{child_id}"))
        );
    }

    #[tokio::test]
    async fn waiting_parent_releases_permit_preventing_pool_deadlock() {
        // cap permits, cap+1 parents that each spawn a gated child then agent_wait
        // on it. The running parents hold every permit; their children cannot
        // start until a permit frees. Only the agent_wait permit release lets the
        // children (and the queued (cap+1)th parent) acquire permits and finish —
        // without it the pool would deadlock (parents holding all permits forever,
        // children stuck queued). The test asserts every wait returns.
        let cap = 2usize;
        let pool = db();
        seed_agent(&pool, "parent", "boss", "[]", 4, 2);
        seed_agent(&pool, "child", "worker", "[]", 0, 1);
        let gate = Gate::new();
        let mgr = manager(
            pool.clone(),
            Arc::new(GatedRunner {
                gate: gate.clone(),
                honor_cancel: false,
            }),
            cap,
        );
        let principal = AgentPrincipal::user("u1");

        // 1) Spawn all parents (non-blocking) and let the cap running ones win
        //    permits before anyone waits — the (cap+1)th parent stays queued.
        let mut callers = Vec::new();
        for i in 0..(cap + 1) {
            let parent_run = mgr
                .spawn("parent", &format!("lead-{i}"), None, &principal, &[])
                .await
                .expect("spawn parent");
            callers.push(CallerRun {
                run_id: parent_run,
                agent_id: "parent".into(),
                principal: principal.clone(),
            });
        }
        tokio::time::sleep(Duration::from_millis(150)).await;

        // 2) Each parent spawns a gated child, then waits on it. The running
        //    parents must release their permits for the children to start.
        let mut waits = Vec::new();
        for caller in &callers {
            let spawn_out = mgr
                .handle_agent_spawn(caller, &json!({"agent_name": "worker", "task": "t"}))
                .await
                .expect("spawn child");
            let child_id = spawn_out["run_ids"][0].as_str().unwrap().to_string();
            let mgr2 = mgr.clone();
            let caller2 = caller.clone();
            waits.push(tokio::spawn(async move {
                mgr2.handle_agent_wait(
                    &caller2,
                    &json!({"run_ids": [child_id], "timeout_secs": 10}),
                )
                .await
            }));
        }

        // Let the waits park and release their permits, then open the gate.
        tokio::time::sleep(Duration::from_millis(150)).await;
        gate.open();

        // All waits must complete (none stuck) within a generous bound.
        let all = tokio::time::timeout(Duration::from_secs(5), async {
            for w in waits {
                let r = w.await.expect("join").expect("wait ok");
                let entry = r.as_object().unwrap().values().next().unwrap();
                assert_eq!(entry["status"].as_str(), Some("completed"));
            }
        })
        .await;
        assert!(all.is_ok(), "nested waits deadlocked the pool");
    }

    #[tokio::test]
    async fn agent_cancel_cancels_a_child() {
        let pool = db();
        seed_agent(&pool, "parent", "boss", "[]", 4, 2);
        seed_agent(&pool, "child", "worker", "[]", 0, 1);
        let gate = Gate::new();
        let mgr = manager(
            pool.clone(),
            Arc::new(GatedRunner {
                gate,
                honor_cancel: true,
            }),
            8,
        );
        let principal = AgentPrincipal::user("u1");
        let parent_run = mgr
            .spawn("parent", "lead", None, &principal, &[])
            .await
            .expect("spawn parent");
        let caller = CallerRun {
            run_id: parent_run,
            agent_id: "parent".into(),
            principal,
        };
        let spawn_out = mgr
            .handle_agent_spawn(&caller, &json!({"agent_name": "worker", "task": "t"}))
            .await
            .expect("spawn child");
        let child_id = spawn_out["run_ids"][0].as_str().unwrap().to_string();

        let out = mgr
            .handle_agent_cancel(&caller, &json!({"run_id": child_id}))
            .expect("cancel");
        assert_eq!(out["cancelled"].as_bool(), Some(true));
        wait_until_status(&pool, &child_id, "cancelled").await;
    }

    #[tokio::test]
    async fn depth_and_fanout_caps_are_enforced() {
        let pool = db();
        // Parent allows 1 child, depth 1 — so a child cannot itself spawn.
        seed_agent(&pool, "parent", "boss", "[]", 1, 1);
        seed_agent(&pool, "child", "worker", "[]", 0, 1);
        let mgr = manager(pool.clone(), Arc::new(InstantRunner), 8);
        let principal = AgentPrincipal::user("u1");
        let parent_run = mgr
            .spawn("parent", "lead", None, &principal, &[])
            .await
            .expect("spawn parent");
        let caller = CallerRun {
            run_id: parent_run.clone(),
            agent_id: "parent".into(),
            principal: principal.clone(),
        };

        // First child fits max_subagents=1.
        mgr.handle_agent_spawn(&caller, &json!({"agent_name": "worker", "task": "a"}))
            .await
            .expect("first child");
        // Wait for it so it is no longer counted active, OR a second batch of 2.
        let batch = mgr
            .handle_agent_spawn(
                &caller,
                &json!({"tasks": [
                    {"agent_name": "worker", "task": "b"},
                    {"agent_name": "worker", "task": "c"}
                ]}),
            )
            .await;
        assert!(batch.is_err(), "fan-out cap must reject over-budget batch");
        assert!(batch.unwrap_err().to_string().contains("max_subagents"));

        // Depth: a child trying to spawn a grandchild exceeds max_spawn_depth=1.
        // Simulate by giving the child spawn rights and asking it to spawn.
        seed_agent(&pool, "spawner", "midboss", "[]", 2, 1);
        let child_run = mgr
            .spawn(
                "spawner",
                "mid",
                Some(&parent_run),
                &principal,
                &[],
            )
            .await
            .expect("spawn mid");
        let mid_caller = CallerRun {
            run_id: child_run,
            agent_id: "spawner".into(),
            principal,
        };
        let grand = mgr
            .handle_agent_spawn(&mid_caller, &json!({"agent_name": "worker", "task": "g"}))
            .await;
        assert!(grand.is_err(), "depth cap must reject a grandchild");
        assert!(grand.unwrap_err().to_string().contains("depth"));
    }

    #[tokio::test]
    async fn child_tools_must_be_subset_of_parent() {
        let pool = db();
        seed_agent(&pool, "parent", "boss", r#"["memory.memory_search"]"#, 4, 2);
        // Child wants a tool the parent never had.
        seed_agent(&pool, "child", "worker", r#"["contacts.lookup"]"#, 0, 1);
        let mgr = manager(pool.clone(), Arc::new(InstantRunner), 8);
        let principal = AgentPrincipal::user("u1");
        let parent_run = mgr
            .spawn("parent", "lead", None, &principal, &[])
            .await
            .expect("spawn parent");
        let caller = CallerRun {
            run_id: parent_run,
            agent_id: "parent".into(),
            principal,
        };
        let out = mgr
            .handle_agent_spawn(&caller, &json!({"agent_name": "worker", "task": "x"}))
            .await;
        assert!(out.is_err(), "child tool outside parent surface must reject");
        assert!(out.unwrap_err().to_string().contains("tool surface"));
    }

    #[tokio::test]
    async fn restart_marks_orphans_interrupted() {
        let pool = db();
        seed_agent(&pool, "a1", "worker", "[]", 0, 1);
        // Two active rows from a "previous process".
        repository::create_agent_run(
            &pool,
            &NewAgentRun {
                id: "orphan-1",
                agent_id: "a1",
                parent_run_id: None,
                flow_execution_id: None,
                user_id: Some("u1"),
                org_id: None,
                prompt: "p",
            },
        )
        .expect("create");
        repository::update_agent_run_status(
            &pool,
            "orphan-1",
            &AgentRunStatusUpdate {
                status: "running",
                set_started: true,
                ..Default::default()
            },
        )
        .expect("running");
        repository::create_agent_run(
            &pool,
            &NewAgentRun {
                id: "orphan-2",
                agent_id: "a1",
                parent_run_id: None,
                flow_execution_id: None,
                user_id: Some("u1"),
                org_id: None,
                prompt: "p",
            },
        )
        .expect("create");

        let reaped = AgentRunManager::reap_interrupted_on_startup(&pool).expect("reap");
        assert_eq!(reaped, 2);
        for id in ["orphan-1", "orphan-2"] {
            let row = repository::get_agent_run(&pool, id)
                .expect("get")
                .expect("row");
            assert_eq!(row.status, "interrupted");
            assert!(row.finished_at.is_some());
        }
    }

    /// Polls the DB until the run reaches `target` or a generous bound elapses.
    async fn wait_until_status(pool: &DbPool, run_id: &str, target: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(Some(run)) = repository::get_agent_run(pool, run_id) {
                if run.status == target {
                    return;
                }
            }
            if Instant::now() > deadline {
                panic!("run {run_id} never reached status {target}");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// A run entering waiting_user releases its permit so a queued run starts;
    /// resuming reacquires one. With cap=1, a single running parent that enters
    /// waiting_user must free the slot for a second gated run, then both finish
    /// once released (§3.13 — same anti-livelock rule as agent_wait).
    #[tokio::test]
    async fn waiting_user_releases_permit_and_resumes() {
        let pool = db();
        seed_agent(&pool, "a", "worker", "[]", 0, 1);
        let gate = Gate::new();
        let mgr = manager(
            pool.clone(),
            Arc::new(GatedRunner {
                gate: gate.clone(),
                honor_cancel: false,
            }),
            1,
        );
        let principal = AgentPrincipal::user("u1");

        // First run wins the single permit and parks on the gate.
        let run_a = mgr.spawn("a", "first", None, &principal, &[]).await.expect("spawn a");
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(mgr.semaphore.available_permits(), 0, "run holds the only permit");

        // It enters waiting_user → releases the permit.
        let had = mgr.enter_waiting_user(&run_a);
        assert!(had, "a running managed run holds a permit to release");
        assert_eq!(
            mgr.semaphore.available_permits(),
            1,
            "waiting_user must free the permit"
        );
        let row = repository::get_agent_run(&pool, &run_a).expect("get").expect("row");
        assert_eq!(row.status, "waiting_user");

        // Resume reacquires it.
        mgr.exit_waiting_user(&run_a, had).await.expect("resume");
        assert_eq!(
            mgr.semaphore.available_permits(),
            0,
            "resume must reacquire the permit"
        );
        let row = repository::get_agent_run(&pool, &run_a).expect("get").expect("row");
        assert_eq!(row.status, "running");

        // Release the gate so the run completes and frees its permit.
        gate.open();
        wait_until_status(&pool, &run_a, "completed").await;
    }

    /// Finding 4 — a parent cancelled while parked in agent_wait must NOT be
    /// resurrected when the wait returns. With cap=1, the parent releases its
    /// permit on entry to the wait; cancelling it mid-wait writes `cancelled` and
    /// signals its token. When the child settles and agent_wait returns, the
    /// post-wait resume must observe the terminal caller and skip both the
    /// permit reacquire and the `running` flip, so the row stays `cancelled` and
    /// no global slot is re-taken for a finished run.
    #[tokio::test]
    async fn cancel_during_agent_wait_skips_resume() {
        let pool = db();
        seed_agent(&pool, "parent", "boss", "[]", 4, 2);
        seed_agent(&pool, "child", "worker", "[]", 0, 1);
        let gate = Gate::new();
        let mgr = manager(
            pool.clone(),
            Arc::new(GatedRunner {
                gate: gate.clone(),
                honor_cancel: true,
            }),
            1,
        );
        let principal = AgentPrincipal::user("u1");

        let parent_run = mgr
            .spawn("parent", "lead", None, &principal, &[])
            .await
            .expect("spawn parent");
        let caller = CallerRun {
            run_id: parent_run.clone(),
            agent_id: "parent".into(),
            principal: principal.clone(),
        };
        let spawn_out = mgr
            .handle_agent_spawn(&caller, &json!({"agent_name": "worker", "task": "subtask"}))
            .await
            .expect("spawn child");
        let child_id = spawn_out["run_ids"][0].as_str().unwrap().to_string();

        // Park the parent in agent_wait — it releases its permit and flips to
        // `waiting`, letting the gated child acquire the only permit.
        let mgr2 = mgr.clone();
        let caller2 = caller.clone();
        let child_for_wait = child_id.clone();
        let wait = tokio::spawn(async move {
            mgr2.handle_agent_wait(
                &caller2,
                &json!({"run_ids": [child_for_wait], "timeout_secs": 30}),
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(80)).await;

        // Cancel the parent while it is parked, then release the gate so the
        // child completes and the wait returns.
        assert!(mgr.cancel(&parent_run), "parent run is live and cancellable");
        gate.open();
        let _ = wait.await.expect("join").expect("wait ok");

        // The parent row stays cancelled — the resume path did not clobber it.
        let row = repository::get_agent_run(&pool, &parent_run)
            .expect("get")
            .expect("row");
        assert_eq!(row.status, "cancelled", "resume resurrected a cancelled run");
        // No permit was reacquired for the finished parent (the child already
        // freed its own on completion, leaving the pool full).
        assert_eq!(
            mgr.semaphore.available_permits(),
            1,
            "resume re-took a slot for a cancelled run"
        );
    }
}
