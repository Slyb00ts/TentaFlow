// =============================================================================
// Plik: flow_engine/dispatchers/progress.rs
// Opis: ProgressSink — engine-agnostic execution progress events (§3.11 C).
//       Mirrors MetricsSink: a narrow trait the executor (and, later, the
//       loop/map/router/child emitters owned by phases 5/6) calls to publish
//       ephemeral run progress. The event enum is intentionally rich and
//       future-proof so those later phases emit without extending it.
// =============================================================================

/// One ephemeral progress signal from a running flow. Events are NOT persisted
/// (durable record is `run_log`); a production sink fans them out over a
/// broadcast channel keyed by scope (session / run_id) so the dashboard can
/// drill into a live run.
///
/// The full variant set is declared up front (phases 3/5/6 emit the
/// iteration/map/tool/child/router variants from their own — not-yet-existing —
/// blocks). Phase 4 only emits `NodeStarted` / `NodeFinished` from the executor.
#[derive(Debug, Clone, PartialEq)]
pub enum ProgressEvent {
    /// A node began executing (engine-level, one per node incl. the streaming
    /// producer). `node_type` lets the UI pick an icon without a flow lookup.
    NodeStarted { node_id: String, node_type: String },
    /// A node settled. `status` is the trace status label (`ok` / `error` /
    /// `skipped`) so the UI mirrors the trace without carrying the full step.
    NodeFinished { node_id: String, status: String },
    /// The streaming producer delivered its first visible token for the current
    /// step. TTFT is `request_started -> first_token` and decoding is
    /// `first_token -> assistant_message`, so this event exists to keep both as
    /// DIFFERENCES BETWEEN EVENTS — no adapter measures its own latency.
    /// Emitted once per streaming step (per harness iteration), never per run.
    FirstToken { node_id: String },
    /// A `loop` body iteration began (phase 5 emits). `max` is the configured
    /// iteration budget (0 = unbounded / until-only).
    IterationStarted { node_id: String, n: u32, max: u32 },
    /// A `loop` body iteration settled (phase 5 emits).
    IterationFinished { node_id: String, n: u32 },
    /// A `map` element began (phase 5 emits). `total` is the element count.
    MapElement {
        node_id: String,
        index: u32,
        total: u32,
        status: String,
    },
    /// A tool call began (phase 3/5 harness emits). `name` is the tool name.
    /// `call_id` pochodzi wprost z wywolania modelu. Bez niego dwa rownolegle
    /// wywolania tego samego narzedzia sa nierozroznialne — a odkad wywolania jada
    /// obok siebie, parowanie po nazwie potrafi zlaczyc start jednego z koncem
    /// drugiego i wyliczyc z tego bzdurny czas.
    ToolCallStarted { call_id: String, name: String },
    /// A tool call settled (phase 3/5 harness emits).
    ToolCallFinished {
        call_id: String,
        name: String,
        status: String,
    },
    /// Context compaction ran for a node (phase 5 harness emits).
    Compaction { node_id: String },
    /// A background child run was spawned (phase 6 emits). `agent` names the
    /// agent definition that produced it.
    ChildSpawned { run_id: String, agent: String },
    /// A background child run settled (phase 6 emits).
    ChildFinished { run_id: String, status: String },
    /// A router node picked a branch (phase 5 emits). `selected` is the chosen
    /// branch label, `reason` a short human-readable justification.
    RouterDecision {
        node_id: String,
        selected: String,
        reason: String,
    },
    /// A run is asking the operator a question and entered `waiting_user`
    /// (§3.13 A — `core.ask_user` / the `ask_user` block). The dashboard renders
    /// the question card from `interaction_id` + `question` + `choices`.
    UserQuestion {
        run_id: String,
        interaction_id: String,
        question: String,
        choices: Vec<String>,
    },
    /// A run needs a permission grant to run a denied tool and entered
    /// `waiting_user` (§3.13 B). The dashboard renders the grant card naming the
    /// addon/tool/permission; the reply carries the operator's decision.
    PermissionRequest {
        run_id: String,
        interaction_id: String,
        addon_id: String,
        tool_name: String,
        permission: String,
    },
    /// A pending interaction was resolved (answered, decided, or timed out), so
    /// the dashboard can dismiss its card. `outcome` is a short label
    /// (`replied` / `timed_out`).
    InteractionResolved {
        run_id: String,
        interaction_id: String,
        outcome: String,
    },
}

/// Narrow sink the executor publishes progress to. Mirrors `MetricsSink`:
/// `Send + Sync`, fire-and-forget, never blocks the execution path.
///
/// `scope` is the broadcast key (session id or run id) the production sink
/// already knows for this run — passing it per-call keeps the sink itself
/// stateless and lets one sink serve runs across scopes.
pub trait ProgressSink: Send + Sync {
    fn emit(&self, scope: &str, event: ProgressEvent);
}

/// Default no-op sink used by tests/test_support and whenever no broker is
/// wired (headless deploys without the dashboard).
pub struct NoopProgress;

impl ProgressSink for NoopProgress {
    fn emit(&self, _scope: &str, _event: ProgressEvent) {}
}
