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
use tokio::sync::{broadcast, watch, Semaphore};
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

use crate::db::models::{AgentRunStatusUpdate, DbAgentRun, NewAgentRun};
use crate::db::{repository, DbPool};
use crate::flow_engine::dispatchers::{ProgressEvent, ProgressSink};
use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, TokenUsage};
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

/// Capacity of the process-global child-completion notification ring the
/// subagent reactor subscribes to. Sized for a burst of concurrent children
/// settling at once; a lagging reactor that overruns it drops the oldest events
/// (`RecvError::Lagged`) — the reactor logs and continues, never blocking a
/// finishing run's task. Reactive flows are an at-least-once-best-effort signal,
/// not a durable queue (the durable record stays the mailbox / `agent_runs`).
const CHILD_FINISHED_CHANNEL_CAPACITY: usize = 1024;

/// A settled sub-agent run, broadcast process-wide so the reactor
/// (`agents::subagent_reactor`) can dispatch event-driven flows keyed on
/// `on_subagent_complete`. Carries the child's agent id + terminal status so the
/// reactor matches a flow's filter without a DB read on the hot path.
#[derive(Debug, Clone)]
pub struct ChildFinishedEvent {
    pub run_id: String,
    pub agent_id: String,
    pub status: String,
}

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

/// Terminal check on a persisted status string — the DB mirror of
/// `RunStatus::is_terminal` for rows read back from `agent_runs`.
fn db_status_is_terminal(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled" | "interrupted")
}

/// Outcome of watching one run's live status channel until it settles — the
/// shared core of `wait_one` (agent_wait JSON path) and `await_run` (Rust
/// caller path).
enum WatchOutcome {
    /// The run reached a terminal status while subscribed.
    Terminal(RunStatus),
    /// The sender dropped mid-wait (the task finished and evicted its handle);
    /// the persisted row is authoritative.
    Evicted,
    /// The deadline passed with the run still live in the carried state.
    TimedOut(RunStatus),
    /// No registry handle — the run either settled already or never ran in
    /// this process; the caller decides what the DB row means.
    NotInRegistry,
}

/// What one harness flow produced: the answer, and what it cost.
///
/// The accounting travels with the text because the run row is settled in one
/// place — a caller that has to ask a second layer "and how many tokens was
/// that?" ends up writing zeros, which is precisely what happened while this
/// carried a bare `String`.
pub struct AgentFlowOutcome {
    pub text: String,
    pub usage: TokenUsage,
    /// Model the flow's last LLM call resolved to, `None` for a flow that
    /// called none.
    pub model: Option<String>,
}

/// Runs the agent harness flow that backs one background run. Abstracted so the
/// manager's orchestration (semaphore, watch, cancel, heartbeat) is unit-testable
/// without a live `FlowDispatcher`. The production impl is `FlowDispatcherRunner`.
#[async_trait]
pub trait BackgroundFlowRunner: Send + Sync {
    /// Runs `flow_id` with `initial` as the trigger input under `principal`,
    /// governed by `deadline` and `cancel`. Returns the final answer and what it
    /// cost. The `agent_run_id` already lives in `initial.meta`, so the harness flow's
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
    ) -> Result<AgentFlowOutcome>;
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
    /// Process-global child-completion notifications. Every finishing run's task
    /// broadcasts a `ChildFinishedEvent` here; the subagent reactor subscribes
    /// once at startup and dispatches event-driven flows. Always-on (unlike the
    /// per-scope `ProgressBroker`, whose `publish` is a no-op without a live
    /// subscriber) so the reactor never has to pre-subscribe to unknown run ids.
    child_finished_tx: broadcast::Sender<ChildFinishedEvent>,
    /// A `Weak` to the `Arc`-wrapped manager, so a finishing child's task can
    /// start an auto-continuation parent run (§3.6 level 3) on the SAME manager
    /// (counting toward its caps), without forcing the process-global instance —
    /// tests wire their own. Set once via `attach_self` right after the `Arc` is
    /// built (`init_global`, the test `manager()` helper). Empty = no
    /// auto-continuation (a manager not yet attached, e.g. mid-construction).
    weak_self: OnceLock<std::sync::Weak<AgentRunManager>>,
}

/// Process-global manager. Mirrors `progress_broker::global_broker` — one
/// instance shared by every AppState so background runs survive past the
/// connection that started them.
static GLOBAL: OnceLock<Arc<AgentRunManager>> = OnceLock::new();

/// Installs the process-global manager. Idempotent: a second call returns the
/// already-installed instance (the first wins), so a re-entrant startup never
/// forks the registry. Call once after the FlowDispatcher exists.
pub fn init_global(manager: Arc<AgentRunManager>) -> Arc<AgentRunManager> {
    manager.attach_self();
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
        let (child_finished_tx, _) = broadcast::channel(CHILD_FINISHED_CHANNEL_CAPACITY);
        Self {
            db,
            runner,
            progress,
            semaphore: Arc::new(Semaphore::new(cap)),
            runs: Arc::new(DashMap::new()),
            child_finished_tx,
            weak_self: OnceLock::new(),
        }
    }

    /// Subscribes to the process-global child-completion stream. The subagent
    /// reactor holds the sole subscriber; the sender lives in the manager, so a
    /// subscriber dropping never stops finishing tasks from broadcasting (they
    /// just see no receivers, which `try_send`-style `send` reports as `Err`).
    pub fn child_finished_subscribe(&self) -> broadcast::Receiver<ChildFinishedEvent> {
        self.child_finished_tx.subscribe()
    }

    /// Stores a `Weak` self-reference so a finishing child can start an
    /// auto-continuation parent run on this same manager. Idempotent (first
    /// wins). Call right after wrapping the manager in an `Arc`.
    pub fn attach_self(self: &Arc<Self>) {
        let _ = self.weak_self.set(Arc::downgrade(self));
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
    ///
    /// `extra_meta` entries are merged into the initial envelope meta BEFORE
    /// the task starts — the atomic path for server-minted bindings (e.g.
    /// Project Studio `ps_generation`): no post-spawn write, no race with the
    /// first tool call.
    ///
    /// `flow_override` replaces the harness graph this run executes. It exists
    /// for Code Studio, where the graph is a property of the SESSION (§16) and
    /// not of the agent definition.
    #[allow(clippy::too_many_arguments)]
    pub async fn spawn(
        &self,
        agent_id: &str,
        prompt: &str,
        parent_run_id: Option<&str>,
        principal: &AgentPrincipal,
        inherited_tools: &[String],
        extra_meta: &[(&str, Value)],
        target_session_id: Option<&str>,
        flow_override: Option<&str>,
    ) -> Result<String> {
        self.spawn_with_run_id(
            &uuid::Uuid::new_v4().to_string(),
            agent_id,
            prompt,
            parent_run_id,
            principal,
            inherited_tools,
            extra_meta,
            target_session_id,
            flow_override,
        )
        .await
    }

    /// Same spawn, under a run id the caller minted.
    ///
    /// The sub-agent path needs the id BEFORE the run exists: a Code Studio
    /// session claims its run budget by inserting the run's own row, and a
    /// claim that could not name the run it claims for would have to count
    /// first and insert later — the exact race the budget is there to close.
    #[allow(clippy::too_many_arguments)]
    async fn spawn_with_run_id(
        &self,
        run_id: &str,
        agent_id: &str,
        prompt: &str,
        parent_run_id: Option<&str>,
        principal: &AgentPrincipal,
        inherited_tools: &[String],
        extra_meta: &[(&str, Value)],
        target_session_id: Option<&str>,
        flow_override: Option<&str>,
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

        let run_id = run_id.to_string();
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

        // A Code Studio session pins the harness graph its runs execute (§16),
        // and that pin belongs to the SESSION, not to the agent definition — the
        // same agent serves every workspace. The caller therefore names the flow
        // when it has one; everyone else keeps the agent's own harness.
        let flow_id = flow_override
            .filter(|s| !s.is_empty())
            .or(agent.flow_id.as_deref().filter(|s| !s.is_empty()))
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
        for (key, value) in extra_meta {
            initial.meta.insert((*key).to_string(), value.clone());
        }

        let deadline = (agent.timeout_secs > 0)
            .then(|| Instant::now() + Duration::from_secs(agent.timeout_secs as u64));

        let ctx = TaskContext {
            db: self.db.clone(),
            runner: self.runner.clone(),
            progress: self.progress.clone(),
            manager: self.weak_self.get().cloned(),
            runs: self.runs_ref(),
            semaphore: self.semaphore.clone(),
            child_finished_tx: self.child_finished_tx.clone(),
            run_id: run_id.clone(),
            agent_id: agent.id.clone(),
            parent_run_id: parent_run_id.map(|s| s.to_string()),
            target_session_id: target_session_id.map(|s| s.to_string()),
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
    pub async fn handle_agent_spawn(&self, caller: &CallerRun, args: &Value) -> Result<Value> {
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

        // Every child is resolved before any of them starts, because the
        // session budget below is claimed for the whole batch at once and a
        // claim for a task that turns out to name no agent would burn budget on
        // a run that can never exist.
        // The caller's roster, if it declared one. `None` = unrestricted, which is
        // what every agent did before the column existed; an empty list means the
        // agent may delegate to nobody. Parsed once for the whole batch.
        let roster: Option<Vec<String>> = repository::get_agent(&self.db, &caller.agent_id)?
            .and_then(|a| a.allowed_agents_json)
            .map(|raw| serde_json::from_str::<Vec<String>>(&raw))
            .transpose()
            .map_err(|e| anyhow!("agent_spawn: caller's allowed_agents is not a list: {e}"))?;

        let mut planned = Vec::with_capacity(tasks.len());
        for task in tasks {
            // Refuse BEFORE resolving: naming an agent outside the roster must
            // read the same whether that agent exists or not, otherwise the
            // error message turns the roster into a directory of every agent
            // on the node.
            if let Some(allowed) = &roster {
                if !allowed.iter().any(|n| n == &task.agent_name) {
                    return Err(anyhow!(
                        "agent_spawn: '{}' is not in this agent's delegation roster ({})",
                        task.agent_name,
                        if allowed.is_empty() {
                            "it may not delegate at all".to_string()
                        } else {
                            allowed.join(", ")
                        }
                    ));
                }
            }
            let child = repository::get_agent_by_name(&self.db, &task.agent_name)?
                .ok_or_else(|| anyhow!("agent_spawn: agent '{}' not found", task.agent_name))?;
            let prompt = match &task.context {
                Some(c) if !c.is_empty() => format!("{c}\n\n{}", task.task),
                _ => task.task.clone(),
            };
            planned.push(PlannedChild {
                run_id: uuid::Uuid::new_v4().to_string(),
                agent_id: child.id,
                prompt,
            });
        }

        // Session budget (§15): depth bounds one branch and max_subagents
        // bounds one parent — neither bounds the tree, so a Code Studio session
        // also carries an absolute count over ALL of its runs, nested ones
        // included. The refusal stops this SPAWN and nothing else: the runs
        // already working keep working and the caller sees a recoverable tool
        // error naming the budget.
        let session = self.claim_session_runs(caller, &planned)?;

        // The Code Studio binding travels with the delegation: a child that
        // reviews or tests must land in the parent's worktree, and passing it
        // here (rather than letting the child ask for one) is what makes that
        // impossible to redirect.
        let extra_meta: Vec<(&str, Value)> = match &caller.code_session {
            Some(binding) => vec![(
                crate::code_studio::tools::SESSION_META_KEY,
                binding.clone(),
            )],
            None => Vec::new(),
        };

        let mut run_ids = Vec::with_capacity(planned.len());
        for child in &planned {
            let spawned = self
                .spawn_with_run_id(
                    &child.run_id,
                    &child.agent_id,
                    &child.prompt,
                    Some(&caller.run_id),
                    &caller.principal,
                    &parent_tools,
                    &extra_meta,
                    caller.session_id.as_deref(),
                    // A sub-agent runs ITS OWN harness, not the caller's: the
                    // session pin (§16.6) binds the root run's graph, and a
                    // reviewer or tester is a different agent with a different
                    // flow. Overriding here would run every specialist through
                    // the orchestrator's graph.
                    None,
                )
                .await;
            match spawned {
                Ok(run_id) => run_ids.push(run_id),
                Err(e) => {
                    // The slot was claimed before the launch, so a child that
                    // never started is closed as failed rather than left
                    // "running" on the session's timeline for ever.
                    if let Some((pool, session_id)) = &session {
                        let end = crate::code_studio::session::SubagentRunEnd {
                            status: "failed",
                            error: Some("the run could not be started"),
                            ..Default::default()
                        };
                        for pending in &planned[run_ids.len()..] {
                            if let Err(error) = crate::code_studio::session::close_subagent_run(
                                pool,
                                session_id,
                                &pending.run_id,
                                end,
                            ) {
                                tracing::warn!(
                                    run_id = %pending.run_id,
                                    "cannot close an unstarted sub-agent run: {error:#}"
                                );
                            }
                        }
                    }
                    return Err(e);
                }
            }
        }

        Ok(json!({ "run_ids": run_ids }))
    }

    /// Claims one session run slot per planned child when the caller runs
    /// inside a Code Studio session, returning the session's runtime pool so a
    /// later failure can close the rows it just wrote. `None` for a caller with
    /// no session binding — an ordinary background agent has no session to
    /// budget.
    fn claim_session_runs(
        &self,
        caller: &CallerRun,
        planned: &[PlannedChild],
    ) -> Result<Option<(DbPool, String)>> {
        let Some(value) = caller.code_session.as_ref() else {
            return Ok(None);
        };
        // The binding is server-minted; a malformed one means this run's meta
        // was corrupted, and guessing "then there is no budget" is exactly the
        // wrong way to resolve that.
        let binding = crate::code_studio::tools::binding_from_value(value)
            .ok_or_else(|| anyhow!("agent_spawn: the session binding of this run is malformed"))?;
        let pool = crate::code_studio::workspace_db::open(&binding.workspace_id)?;
        let budget = crate::code_studio::session::max_session_runs(&self.db);
        let runs: Vec<crate::code_studio::session::SubagentRun<'_>> = planned
            .iter()
            .map(|child| crate::code_studio::session::SubagentRun {
                run_id: &child.run_id,
                parent_run_id: &caller.run_id,
                agent_id: &child.agent_id,
            })
            .collect();
        crate::code_studio::session::claim_subagent_runs(
            &pool,
            &binding.session_id,
            &runs,
            budget,
        )?;
        Ok(Some((pool, binding.session_id)))
    }

    /// `core.agent_wait` handler. Waits for the named runs to settle on their
    /// `watch` channels (no polling), bounded by `timeout_secs`. ANTI-LIVELOCK:
    /// the caller's run flips to `waiting` and releases its global permit for the
    /// duration, re-acquiring on wake — so `cap+1` nested waits never deadlock.
    /// Only children of the caller may be waited on (a run cannot await an
    /// unrelated run's result). `mode` (`all` default, or `any`) decides whether
    /// the call returns once every run is terminal or as soon as the first one
    /// settles; in `any` mode the result map only carries the runs already
    /// terminal at return time.
    pub async fn handle_agent_wait(&self, caller: &CallerRun, args: &Value) -> Result<Value> {
        let run_ids: Vec<String> = args
            .get("run_ids")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if run_ids.is_empty() {
            return Err(anyhow!("agent_wait: run_ids required"));
        }
        let wait_any = matches!(args.get("mode").and_then(|v| v.as_str()), Some("any"));
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
        let results = if wait_any {
            self.wait_any(&run_ids, deadline).await
        } else {
            let mut map = serde_json::Map::new();
            for id in &run_ids {
                let entry = self.wait_one(id, deadline).await;
                map.insert(id.clone(), entry);
            }
            map
        };

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

    /// Watches run `id`'s live status channel until it turns terminal, the
    /// handle is evicted, or `deadline` passes. Subscribes BEFORE reading the
    /// current value so no transition is missed between the registry lookup
    /// and the first borrow.
    async fn watch_until_terminal(&self, id: &str, deadline: Instant) -> WatchOutcome {
        let Some(mut rx) = self.runs.get(id).map(|h| h.status.subscribe()) else {
            return WatchOutcome::NotInRegistry;
        };
        loop {
            let current = *rx.borrow();
            if current.is_terminal() {
                return WatchOutcome::Terminal(current);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return WatchOutcome::TimedOut(current);
            }
            match tokio::time::timeout(remaining, rx.changed()).await {
                // Channel changed — re-read the loop head for the new value.
                Ok(Ok(())) => continue,
                Ok(Err(_)) => return WatchOutcome::Evicted,
                // Wait budget elapsed.
                Err(_) => return WatchOutcome::TimedOut(*rx.borrow()),
            }
        }
    }

    /// Blocks until run `id` settles or `deadline` passes. Reads the live
    /// `watch` channel when the run is in-registry, else falls back to the DB
    /// (a run that finished before this wait subscribed). Returns
    /// `{status, result?}`.
    async fn wait_one(&self, id: &str, deadline: Instant) -> Value {
        match self.watch_until_terminal(id, deadline).await {
            WatchOutcome::Terminal(status) => self.terminal_result(id, status),
            // Sender dropped (the task finished and evicted its handle) or the
            // run was never in the registry: the persisted row is authoritative.
            WatchOutcome::Evicted | WatchOutcome::NotInRegistry => self.db_result(id),
            WatchOutcome::TimedOut(status) => {
                json!({ "status": status.as_str(), "timed_out": true })
            }
        }
    }

    /// Blocks until run `run_id` reaches a terminal state and returns its
    /// persisted row — the server-side counterpart of `core.agent_wait` for
    /// Rust callers (e.g. a module that spawns a generator agent and needs the
    /// result without a frontend), so it is NOT gated to children of a calling
    /// run and holds no concurrency permit to release. On timeout the run
    /// KEEPS executing — this method never cancels it.
    pub async fn await_run(&self, run_id: &str, timeout: Duration) -> Result<DbAgentRun> {
        let deadline = Instant::now() + timeout;
        match self.watch_until_terminal(run_id, deadline).await {
            WatchOutcome::Terminal(_) | WatchOutcome::Evicted => {
                self.read_terminal_row(run_id).await
            }
            WatchOutcome::TimedOut(status) => Err(anyhow!(
                "await_run: run '{run_id}' still '{}' after {timeout:?} (run keeps executing)",
                status.as_str()
            )),
            WatchOutcome::NotInRegistry => {
                let run = repository::get_agent_run(&self.db, run_id)?
                    .ok_or_else(|| anyhow!("await_run: run '{run_id}' not found"))?;
                if db_status_is_terminal(&run.status) {
                    return Ok(run);
                }
                // A non-terminal row with no live handle is an orphan (its
                // owning process died mid-run). This read path must not write
                // the `interrupted` transition itself — that stays owned by
                // `reap_interrupted_on_startup` — so surface the inconsistency
                // instead of parking on a run nothing is executing.
                Err(anyhow!(
                    "await_run: run '{run_id}' is '{}' but has no live task in this process \
                     (orphaned row; a restart reaps it to 'interrupted')",
                    run.status
                ))
            }
        }
    }

    /// Reads the settled row after a terminal watch signal. `run_task` commits
    /// the terminal row BEFORE sending on the watch channel, but `cancel()`
    /// sends first and writes after — so a waiter can wake a beat before the
    /// terminal columns land. A short bounded re-read closes that gap instead
    /// of handing back a stale in-flight row.
    async fn read_terminal_row(&self, run_id: &str) -> Result<DbAgentRun> {
        let mut last = None;
        for attempt in 0..20u32 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            match repository::get_agent_run(&self.db, run_id)? {
                Some(run) if db_status_is_terminal(&run.status) => return Ok(run),
                other => last = other,
            }
        }
        // The terminal row update is a synchronous write issued around the
        // watch send; not seeing it inside the retry budget means that write
        // failed (it is fire-and-forget on the cancel path). Hand back the
        // freshest row rather than nothing — its status tells the caller.
        last.ok_or_else(|| anyhow!("await_run: run '{run_id}' row missing after completion"))
    }

    /// `mode=any` wait: returns as soon as the FIRST of `run_ids` reaches a
    /// terminal state (or the deadline passes). The returned map carries an entry
    /// only for the run that finished first — a downstream block in `any` mode acts
    /// on that finisher and leaves the rest running. Each id is awaited through the
    /// same `wait_one` (its own `watch` subscription + initial DB read for a run
    /// that already settled); `select_all` races them without spawning, so every
    /// future keeps borrowing `&self`.
    async fn wait_any(
        &self,
        run_ids: &[String],
        deadline: Instant,
    ) -> serde_json::Map<String, Value> {
        let mut pending: Vec<_> = run_ids
            .iter()
            .map(|id| {
                let id = id.clone();
                Box::pin(async move {
                    let entry = self.wait_one(&id, deadline).await;
                    (id, entry)
                })
            })
            .collect();

        let mut map = serde_json::Map::new();
        while !pending.is_empty() {
            let ((id, entry), _idx, rest) = futures::future::select_all(pending).await;
            pending = rest;
            let terminal = entry
                .get("status")
                .and_then(|s| s.as_str())
                .map(|s| matches!(s, "completed" | "failed" | "cancelled" | "interrupted"))
                .unwrap_or(false);
            if terminal {
                // First finisher wins; the remaining waiters are dropped (the runs
                // they watch keep running).
                map.insert(id, entry);
                break;
            }
            // A non-terminal settle means this id hit the deadline (timed_out);
            // record it and keep waiting on the others until one finishes or all
            // time out.
            map.insert(id, entry);
        }
        map
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
    /// Chat session the spawning context belongs to (§3.6 level 2). A child
    /// spawned here records it as `target_session_id` so its result reaches the
    /// session's next interaction via the mailbox. `None` for a background
    /// parent with no originating session.
    pub session_id: Option<String>,
    /// Server-minted Code Studio session binding of the calling run, carried so
    /// a delegated `code-reviewer` / `code-tester` / `code-committer` works in
    /// the SAME worktree as its parent. It is copied, never chosen: the child
    /// cannot name a workspace, and a caller outside Code Studio has `None`, so
    /// spawning never grants access that the parent did not already hold.
    pub code_session: Option<Value>,
}

/// One child of a delegation, resolved and given its run id before anything is
/// launched. The id exists this early so a Code Studio session can claim the
/// run's budget slot by inserting that run's own row.
struct PlannedChild {
    run_id: String,
    agent_id: String,
    prompt: String,
}

impl CallerRun {
    /// Builds the calling run's identity from a harness envelope's `meta`
    /// (`agent_run_id` + `agent_id`, set by `agent_context`) plus the run's
    /// principal and session. Shared by `tool_exec` (model-driven control calls)
    /// and the deterministic `spawn`/`await_subagents`/`subagent_status` blocks so
    /// both paths derive the caller the same way. An absent id is an empty string
    /// (the handlers reject an empty `run_id` as "not a managed run context").
    pub fn from_envelope(
        envelope: &FlowEnvelope,
        principal: AgentPrincipal,
        session_id: Option<String>,
    ) -> Self {
        let run_id = envelope
            .meta
            .get("agent_run_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let agent_id = envelope
            .meta
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let code_session = envelope
            .meta
            .get(crate::code_studio::tools::SESSION_META_KEY)
            .cloned();
        Self {
            run_id,
            agent_id,
            principal,
            session_id,
            code_session,
        }
    }
}

/// Everything the spawned task needs. Built in `spawn`, consumed by `run_task`.
struct TaskContext {
    db: DbPool,
    runner: Arc<dyn BackgroundFlowRunner>,
    progress: Arc<ProgressBroker>,
    /// `Weak` to the owning manager, so the task can enqueue the mailbox + run an
    /// auto-continuation parent run on completion (§3.6 levels 2 & 3). `None`
    /// when the manager was not `attach_self`'d (no auto-continuation possible).
    manager: Option<std::sync::Weak<AgentRunManager>>,
    runs: Arc<DashMap<String, RunHandle>>,
    semaphore: Arc<Semaphore>,
    /// Clone of the manager's child-completion broadcast sender — the task emits
    /// the terminal `ChildFinishedEvent` here for the subagent reactor.
    child_finished_tx: broadcast::Sender<ChildFinishedEvent>,
    run_id: String,
    /// The agent definition id this run executes — carried on the completion
    /// event so the reactor can match a flow's `agent_id` filter directly.
    agent_id: String,
    parent_run_id: Option<String>,
    /// Chat session the spawning context belonged to — the mailbox
    /// `target_session_id` for this child's result.
    target_session_id: Option<String>,
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
        manager,
        runs,
        semaphore,
        child_finished_tx,
        run_id,
        agent_id,
        parent_run_id,
        target_session_id,
        flow_id,
        initial,
        principal,
        deadline,
        cancel,
        status,
        permit,
    } = ctx;

    // Read once, here: the envelope moves into the runner below, and a
    // sub-agent registered on a Code Studio session has to close its session
    // row wherever this task ends.
    let code_session = crate::code_studio::tools::binding_from_meta(&initial.meta);

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
            close_session_run(
                code_session.as_ref(),
                &run_id,
                crate::code_studio::session::SubagentRunEnd {
                    status: RunStatus::Cancelled.as_str(),
                    ..Default::default()
                },
            );
            publish_child_finished(
                &progress,
                &run_id,
                parent_run_id.as_deref(),
                RunStatus::Cancelled,
            );
            broadcast_child_finished(&child_finished_tx, &run_id, &agent_id, RunStatus::Cancelled);
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

    // The accounting is settled from whatever the flow reported, INDEPENDENT of
    // the status: a cancelled run still burned the tokens it burned, and a row
    // that reports the work but not its cost cannot be billed.
    let (usage, model) = match &outcome {
        Ok(flow) => (Some(flow.usage), flow.model.clone()),
        Err(_) => (None, None),
    };
    let (final_status, exit_reason, result_text) = if cancel.is_cancelled() {
        (RunStatus::Cancelled, "cancelled".to_string(), None)
    } else {
        match outcome {
            Ok(flow) => (
                RunStatus::Completed,
                "final_response".to_string(),
                Some(flow.text),
            ),
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
            total_tokens: usage.map(|u| u.total_tokens as i64),
            prompt_tokens: usage.map(|u| u.prompt_tokens as i64),
            completion_tokens: usage.map(|u| u.completion_tokens as i64),
            model: model.as_deref(),
            set_finished: true,
            ..Default::default()
        },
    );
    let _ = status.send(final_status);
    close_session_run(
        code_session.as_ref(),
        &run_id,
        crate::code_studio::session::SubagentRunEnd {
            status: final_status.as_str(),
            prompt_tokens: usage.map(|u| u.prompt_tokens as i64).unwrap_or(0),
            completion_tokens: usage.map(|u| u.completion_tokens as i64).unwrap_or(0),
            model: model.as_deref(),
            error: Some(exit_reason.as_str()).filter(|r| r.starts_with("error:")),
        },
    );

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

    // Process-global completion signal for the subagent reactor (phase 4b). A
    // top-level run (no parent) fires too: an `on_subagent_complete` flow keyed
    // on an `agent_id` reacts to ANY run of that agent settling, not only ones
    // spawned as someone's child. The event is best-effort (no live reactor =
    // dropped); the durable record stays the mailbox / `agent_runs` row.
    broadcast_child_finished(&child_finished_tx, &run_id, &agent_id, final_status);

    // Mailbox + auto-continuation (§3.6 levels 2 & 3). A run that spawned a
    // child (parent_run_id set) and completed with a result delivers that result
    // back to the spawning context: always enqueue a mailbox entry addressed to
    // the originating session and/or parent agent, and — if the parent agent
    // opted into `on_child_complete='continue'` — start a fresh parent run with
    // the child result as input. Failures/cancellations carry no result and skip
    // delivery (nothing useful to hand back). The continuation goes through the
    // same manager (`spawn`), so it counts toward depth + concurrency caps; a
    // mutual-continuation loop dies on those limits like any other run.
    if final_status == RunStatus::Completed {
        if let (Some(parent), Some(result)) = (parent_run_id.as_deref(), result_text.as_deref()) {
            deliver_child_result(
                &db,
                manager.clone(),
                &run_id,
                parent,
                target_session_id.as_deref(),
                result,
            );
        }
    }

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

/// Closes the Code Studio session row of a settled run, when the run has one.
///
/// Best effort by design: the authoritative record of a run is its `agent_runs`
/// row, and a workspace whose runtime database cannot be opened (moved storage,
/// a workspace deleted under a live run) must not turn a finished run into a
/// failing task. `close_subagent_run` ignores the ROOT turn's row, which the
/// session coordinator's own watcher owns.
fn close_session_run(
    binding: Option<&crate::code_studio::tools::SessionBinding>,
    run_id: &str,
    end: crate::code_studio::session::SubagentRunEnd<'_>,
) {
    let Some(binding) = binding else {
        return;
    };
    let closed = crate::code_studio::workspace_db::open(&binding.workspace_id).and_then(|pool| {
        crate::code_studio::session::close_subagent_run(&pool, &binding.session_id, run_id, end)
    });
    if let Err(error) = closed {
        tracing::warn!(run_id, "cannot close the session row of a run: {error:#}");
    }
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

/// Broadcasts a settled run on the process-global child-completion ring. A
/// `send` error means no subscriber (no reactor wired / headless) — dropped
/// silently, the durable record stays elsewhere.
fn broadcast_child_finished(
    tx: &broadcast::Sender<ChildFinishedEvent>,
    run_id: &str,
    agent_id: &str,
    status: RunStatus,
) {
    let _ = tx.send(ChildFinishedEvent {
        run_id: run_id.to_string(),
        agent_id: agent_id.to_string(),
        status: status.as_str().to_string(),
    });
}

/// Delivers a finished child's result back to the context that spawned it
/// (§3.6 levels 2 & 3). Always enqueues a mailbox entry addressed to the
/// originating session and/or the parent run's agent (whichever is known), then,
/// if the parent agent opted into `on_child_complete='continue'`, starts a fresh
/// parent run with the child result as input. The continuation runs the parent
/// AGENT (its own harness flow), inheriting the parent run's principal, and is a
/// top-level run (no parent_run_id) so it does not re-enter the spawn-depth chain
/// — it is a sibling of the original parent, bounded by the global concurrency
/// cap. Best-effort: a failure to enqueue or spawn is logged, never fatal to the
/// child's finalization.
fn deliver_child_result(
    db: &DbPool,
    manager: Option<std::sync::Weak<AgentRunManager>>,
    child_run_id: &str,
    parent_run_id: &str,
    target_session_id: Option<&str>,
    result: &str,
) {
    let parent_run = match repository::get_agent_run(db, parent_run_id) {
        Ok(Some(run)) => run,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!("mailbox: parent run '{parent_run_id}' lookup failed: {e}");
            return;
        }
    };

    // A child must address at least one target or the mailbox row is
    // unreachable. The parent run's agent is always a target; the originating
    // session is an extra target when known.
    let entry_id = uuid::Uuid::new_v4().to_string();
    let entry = crate::db::models::NewAgentMailboxEntry {
        id: &entry_id,
        run_id: child_run_id,
        target_session_id,
        target_agent_id: Some(&parent_run.agent_id),
        payload: result,
    };
    if let Err(e) = repository::enqueue_mailbox(db, &entry) {
        tracing::warn!("mailbox: enqueue for child '{child_run_id}' failed: {e}");
    }

    // Auto-continuation (level 3) is opt-in per parent agent and autonomous, so
    // it is gated strictly on the parent agent's `on_child_complete='continue'`.
    let parent_agent = match repository::get_agent(db, &parent_run.agent_id) {
        Ok(Some(agent)) => agent,
        _ => return,
    };
    if parent_agent.on_child_complete != "continue" {
        return;
    }
    let Some(manager) = manager.and_then(|w| w.upgrade()) else {
        return;
    };

    // The continuation prompt is the child's result (Ralph-style: the parent
    // resumes with what its delegate produced). Principal is inherited from the
    // parent run; tools come from the parent agent's own allowlist (a top-level
    // run, so no intersection narrowing applies). The continuation is launched on
    // a fresh task — `spawn` returns a run id immediately, and detaching it here
    // keeps `run_task`'s own future non-recursive (and `Send`): a child's
    // finalization does not block on starting the parent's next run.
    let principal = AgentPrincipal::new(parent_run.user_id.clone(), parent_run.org_id.clone());
    let parent_tools: Vec<String> =
        serde_json::from_str(&parent_agent.tools_json).unwrap_or_default();
    let continuation_prompt = format!(
        "A delegated task you spawned finished with this result:\n\n{result}\n\n\
Continue your work using it.",
    );
    let child_run_id = child_run_id.to_string();
    let target_session_id = target_session_id.map(|s| s.to_string());
    tokio::spawn(async move {
        match manager
            .spawn(
                &parent_agent.id,
                &continuation_prompt,
                None,
                &principal,
                &parent_tools,
                &[],
                target_session_id.as_deref(),
                // The continuation reuses the agent's own flow. `agent_runs`
                // records no flow id, so the run's pinned graph cannot be
                // recovered here; this is exactly the behaviour that existed
                // before `flow_override`, not a new gap opened by it.
                None,
            )
            .await
        {
            Ok(new_run) => tracing::info!(
                "auto-continuation: child '{child_run_id}' started parent run '{new_run}' \
for agent '{}'",
                parent_agent.id
            ),
            Err(e) => tracing::warn!(
                "auto-continuation for agent '{}' failed: {e}",
                parent_agent.id
            ),
        }
    });
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
    ) -> Result<AgentFlowOutcome> {
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
            // Ścieżka agenta (nie-addon) — bez tożsamości instancji addona.
            addon_id: None,
            org_id: None,
            vector_home: None,
            deadline,
            cancel_token: cancel,
            progress_sink: Some(progress),
            flow_depth: 0,
        };

        let outcome = dispatcher
            .dispatch_by_flow_id_background(flow_id, initial, meta)
            .await
            .map_err(|e| anyhow!("agent flow dispatch failed: {e}"))?;
        if let Some(err) = outcome.error {
            return Err(anyhow!("agent flow failed: {err}"));
        }
        Ok(AgentFlowOutcome {
            text: outcome
                .final_envelope
                .payload
                .as_text()
                .unwrap_or("")
                .to_string(),
            usage: outcome.usage,
            model: outcome.model,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations;
    use crate::db::models::{AgentParams, AgentRunListFilter};

    fn db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        migrations::run(&conn).expect("migrations");
        Arc::new(crate::db::Db::from_connection(conn))
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
                on_child_complete: "notify",
                allowed_agents_json: None,
                actor_user_id: None,
            },
        )
        .expect("seed agent");
    }

    /// Seeds an agent with an explicit `on_child_complete` (auto-continuation
    /// tests). Same defaults as `seed_agent` otherwise.
    #[allow(clippy::too_many_arguments)]
    fn seed_agent_with_continue(
        pool: &DbPool,
        id: &str,
        name: &str,
        max_subagents: i64,
        on_child_complete: &str,
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
                tools_json: "[]",
                skills_json: "{}",
                params_json: "{}",
                max_iterations: 5,
                timeout_secs: 600,
                max_subagents,
                max_spawn_depth: 2,
                flow_id: Some("11111111-0000-4000-8000-000000000099"),
                routable: true,
                is_enabled: true,
                on_child_complete,
                allowed_agents_json: None,
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
        ) -> Result<AgentFlowOutcome> {
            if self.honor_cancel {
                tokio::select! {
                    _ = cancel.cancelled() => return Err(anyhow!("cancelled")),
                    _ = self.gate.wait() => {}
                }
            } else {
                self.gate.wait().await;
            }
            Ok(AgentFlowOutcome {
                text: format!("result-of-{scope}"),
                usage: TokenUsage::default(),
                model: None,
            })
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
        ) -> Result<AgentFlowOutcome> {
            Ok(AgentFlowOutcome {
                text: format!("done-{scope}"),
                usage: TokenUsage::default(),
                model: None,
            })
        }
    }

    fn manager(
        db: DbPool,
        runner: Arc<dyn BackgroundFlowRunner>,
        cap: usize,
    ) -> Arc<AgentRunManager> {
        let mgr = Arc::new(AgentRunManager::new(
            db,
            runner,
            Arc::new(ProgressBroker::new()),
            cap,
        ));
        // Attach the self-reference so auto-continuation (§3.6 level 3) can start
        // a parent run on this same test-local manager (not the process-global).
        mgr.attach_self();
        mgr
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
            .spawn("a1", "do it", None, &principal, &[], &[], None, None)
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
            .spawn("parent", "lead", None, &principal, &[], &[], None, None)
            .await
            .expect("spawn parent");

        let caller = CallerRun {
            run_id: parent_run.clone(),
            agent_id: "parent".into(),
            principal: principal.clone(),
            session_id: None,
            code_session: None,
        };
        let spawn_out = mgr
            .handle_agent_spawn(&caller, &json!({"agent_name": "worker", "task": "subtask"}))
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

    /// `await_run` — the Rust-caller wait: a timeout errors WITHOUT cancelling
    /// the run; a live run resolves off its watch channel with the fresh
    /// terminal row; a run already gone from the registry resolves straight
    /// from the persisted row.
    #[tokio::test]
    async fn await_run_times_out_then_returns_terminal_row() {
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
            .spawn("a1", "do it", None, &principal, &[], &[], None, None)
            .await
            .expect("spawn");

        // Timeout path: the gate is closed, so the run cannot settle.
        let err = mgr
            .await_run(&run_id, Duration::from_millis(50))
            .await
            .expect_err("closed gate must time out");
        assert!(err.to_string().contains(&run_id), "got {err}");
        // The timed-out wait must NOT have cancelled the run.
        let row = repository::get_agent_run(&pool, &run_id)
            .expect("get")
            .expect("row");
        assert!(matches!(row.status.as_str(), "queued" | "running"));

        // Blocking path: park a waiter on the live run, then release the gate.
        let mgr2 = mgr.clone();
        let id2 = run_id.clone();
        let waiter =
            tokio::spawn(async move { mgr2.await_run(&id2, Duration::from_secs(5)).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        gate.open();
        let run = waiter.await.expect("join").expect("await_run");
        assert_eq!(run.status, "completed");
        assert_eq!(run.result.as_deref(), Some(&*format!("result-of-{run_id}")));

        // Already-settled path: the terminal row alone resolves the wait.
        wait_until_status(&pool, &run_id, "completed").await;
        let again = mgr
            .await_run(&run_id, Duration::from_secs(1))
            .await
            .expect("settled run resolves from the row");
        assert_eq!(again.status, "completed");
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
                .spawn("parent", &format!("lead-{i}"), None, &principal, &[], &[], None, None)
                .await
                .expect("spawn parent");
            callers.push(CallerRun {
                run_id: parent_run,
                agent_id: "parent".into(),
                principal: principal.clone(),
                session_id: None,
                code_session: None,
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
            .spawn("parent", "lead", None, &principal, &[], &[], None, None)
            .await
            .expect("spawn parent");
        let caller = CallerRun {
            run_id: parent_run,
            agent_id: "parent".into(),
            principal,
            session_id: None,
            code_session: None,
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
            .spawn("parent", "lead", None, &principal, &[], &[], None, None)
            .await
            .expect("spawn parent");
        let caller = CallerRun {
            run_id: parent_run.clone(),
            agent_id: "parent".into(),
            principal: principal.clone(),
            session_id: None,
            code_session: None,
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
            .spawn("spawner", "mid", Some(&parent_run), &principal, &[], &[], None, None)
            .await
            .expect("spawn mid");
        let mid_caller = CallerRun {
            run_id: child_run,
            agent_id: "spawner".into(),
            principal,
            session_id: None,
            code_session: None,
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
            .spawn("parent", "lead", None, &principal, &[], &[], None, None)
            .await
            .expect("spawn parent");
        let caller = CallerRun {
            run_id: parent_run,
            agent_id: "parent".into(),
            principal,
            session_id: None,
            code_session: None,
        };
        let out = mgr
            .handle_agent_spawn(&caller, &json!({"agent_name": "worker", "task": "x"}))
            .await;
        assert!(
            out.is_err(),
            "child tool outside parent surface must reject"
        );
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
        let run_a = mgr
            .spawn("a", "first", None, &principal, &[], &[], None, None)
            .await
            .expect("spawn a");
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(
            mgr.semaphore.available_permits(),
            0,
            "run holds the only permit"
        );

        // It enters waiting_user → releases the permit.
        let had = mgr.enter_waiting_user(&run_a);
        assert!(had, "a running managed run holds a permit to release");
        assert_eq!(
            mgr.semaphore.available_permits(),
            1,
            "waiting_user must free the permit"
        );
        let row = repository::get_agent_run(&pool, &run_a)
            .expect("get")
            .expect("row");
        assert_eq!(row.status, "waiting_user");

        // Resume reacquires it.
        mgr.exit_waiting_user(&run_a, had).await.expect("resume");
        assert_eq!(
            mgr.semaphore.available_permits(),
            0,
            "resume must reacquire the permit"
        );
        let row = repository::get_agent_run(&pool, &run_a)
            .expect("get")
            .expect("row");
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
            .spawn("parent", "lead", None, &principal, &[], &[], None, None)
            .await
            .expect("spawn parent");
        let caller = CallerRun {
            run_id: parent_run.clone(),
            agent_id: "parent".into(),
            principal: principal.clone(),
            session_id: None,
            code_session: None,
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
        assert!(
            mgr.cancel(&parent_run),
            "parent run is live and cancellable"
        );
        gate.open();
        let _ = wait.await.expect("join").expect("wait ok");

        // The parent row stays cancelled — the resume path did not clobber it.
        let row = repository::get_agent_run(&pool, &parent_run)
            .expect("get")
            .expect("row");
        assert_eq!(
            row.status, "cancelled",
            "resume resurrected a cancelled run"
        );
        // No permit was reacquired for the finished parent (the child already
        // freed its own on completion, leaving the pool full).
        assert_eq!(
            mgr.semaphore.available_permits(),
            1,
            "resume re-took a slot for a cancelled run"
        );
    }

    /// §3.6 level 2: when a child run (parent_run_id set) settles with a result,
    /// the manager enqueues a mailbox entry addressed to the spawning session AND
    /// the parent run's agent, so the result can be picked up later.
    #[tokio::test]
    async fn finished_child_enqueues_mailbox_for_its_target() {
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

        let parent_run = mgr
            .spawn("parent", "lead", None, &principal, &[], &[], None, None)
            .await
            .expect("spawn parent");
        // Caller carries the originating session — the mailbox target.
        let caller = CallerRun {
            run_id: parent_run.clone(),
            agent_id: "parent".into(),
            principal: principal.clone(),
            session_id: Some("sess-7".into()),
            code_session: None,
        };
        let spawn_out = mgr
            .handle_agent_spawn(&caller, &json!({"agent_name": "worker", "task": "subtask"}))
            .await
            .expect("spawn child");
        let child_id = spawn_out["run_ids"][0].as_str().unwrap().to_string();

        // Release the child so it completes; its run_task enqueues the mailbox.
        gate.open();
        wait_until_status(&pool, &child_id, "completed").await;
        // The task enqueues after writing the terminal row; poll briefly.
        let deadline = Instant::now() + Duration::from_secs(5);
        let entry = loop {
            let by_session =
                repository::list_undelivered_mailbox_for_session(&pool, "sess-7").unwrap();
            if let Some(e) = by_session.into_iter().find(|e| e.run_id == child_id) {
                break e;
            }
            if Instant::now() > deadline {
                panic!("mailbox entry for child {child_id} never appeared");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert_eq!(entry.payload, format!("result-of-{child_id}"));
        assert_eq!(entry.target_agent_id.as_deref(), Some("parent"));
        assert!(entry.delivered_at.is_none());

        // Addressed to the parent agent too (the same entry is reachable by agent).
        let by_agent = repository::list_undelivered_mailbox_for_agent(&pool, "parent").unwrap();
        assert!(by_agent.iter().any(|e| e.id == entry.id));
    }

    /// §3.6 level 3: a parent agent with on_child_complete='continue' gets a new
    /// run started when its child finishes (the child result is the new prompt).
    #[tokio::test]
    async fn continue_starts_a_new_parent_run_bounded_by_caps() {
        let pool = db();
        // Parent opts into auto-continuation; child does not spawn.
        seed_agent_with_continue(&pool, "parent", "boss", 4, "continue");
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

        let parent_run = mgr
            .spawn("parent", "lead", None, &principal, &[], &[], None, None)
            .await
            .expect("spawn parent");
        let caller = CallerRun {
            run_id: parent_run.clone(),
            agent_id: "parent".into(),
            principal: principal.clone(),
            session_id: None,
            code_session: None,
        };
        let spawn_out = mgr
            .handle_agent_spawn(&caller, &json!({"agent_name": "worker", "task": "subtask"}))
            .await
            .expect("spawn child");
        let child_id = spawn_out["run_ids"][0].as_str().unwrap().to_string();

        gate.open();
        wait_until_status(&pool, &child_id, "completed").await;

        // A NEW top-level run of the parent agent (no parent_run_id) appears,
        // distinct from the original parent run, with the child result as prompt.
        let deadline = Instant::now() + Duration::from_secs(5);
        let continuation = loop {
            let runs = repository::list_agent_runs(
                &pool,
                &AgentRunListFilter {
                    agent_id: Some("parent"),
                    ..Default::default()
                },
            )
            .unwrap();
            if let Some(r) = runs
                .into_iter()
                .find(|r| r.id != parent_run && r.parent_run_id.is_none())
            {
                break r;
            }
            if Instant::now() > deadline {
                panic!("auto-continuation run never started");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert!(continuation
            .prompt
            .contains(&format!("result-of-{child_id}")));
        assert_eq!(continuation.agent_id, "parent");
    }

    /// §3.6 level 3 default: on_child_complete='notify' (the default) does NOT
    /// start a new parent run — only the mailbox + event path runs.
    #[tokio::test]
    async fn notify_does_not_start_a_new_parent_run() {
        let pool = db();
        seed_agent_with_continue(&pool, "parent", "boss", 4, "notify");
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

        let parent_run = mgr
            .spawn("parent", "lead", None, &principal, &[], &[], None, None)
            .await
            .expect("spawn parent");
        let caller = CallerRun {
            run_id: parent_run.clone(),
            agent_id: "parent".into(),
            principal: principal.clone(),
            session_id: None,
            code_session: None,
        };
        let spawn_out = mgr
            .handle_agent_spawn(&caller, &json!({"agent_name": "worker", "task": "subtask"}))
            .await
            .expect("spawn child");
        let child_id = spawn_out["run_ids"][0].as_str().unwrap().to_string();

        gate.open();
        wait_until_status(&pool, &child_id, "completed").await;
        // The mailbox still gets the result (notify path), confirming the child
        // settled fully — but no extra parent run is created.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let mb = repository::list_undelivered_mailbox_for_agent(&pool, "parent").unwrap();
            if mb.iter().any(|e| e.run_id == child_id) {
                break;
            }
            if Instant::now() > deadline {
                panic!("notify path never enqueued the mailbox");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Give any (erroneous) continuation a moment to appear, then assert none.
        tokio::time::sleep(Duration::from_millis(150)).await;
        let parent_runs = repository::list_agent_runs(
            &pool,
            &AgentRunListFilter {
                agent_id: Some("parent"),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            parent_runs.len(),
            1,
            "notify must not start a continuation run; got {parent_runs:?}"
        );
        assert_eq!(parent_runs[0].id, parent_run);
    }
}
