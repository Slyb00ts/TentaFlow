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
    /// Why a node is about to settle as `error`, in words that are safe to
    /// store: the executor emits it only for a failure the adapter raised as a
    /// [`SafeFailure`], right before the node's `NodeFinished(error)`.
    /// Never carries record content or the raw error text of a node.
    NodeFailed { node_id: String, reason: String },
    /// The streaming producer delivered its first visible token for the current
    /// step. TTFT is `request_started -> first_token` and decoding is
    /// `first_token -> assistant_message`, so this event exists to keep both as
    /// DIFFERENCES BETWEEN EVENTS — no adapter measures its own latency.
    /// Emitted once per streaming step (per harness iteration), never per run.
    FirstToken { node_id: String },
    /// What a node received and what it returned, as JSON text, so the event
    /// log can keep the content behind a step. Either side may be absent: the
    /// engine reports what it saw, and a later event for the same node
    /// replaces an earlier one (the `llm` node reports its exact prompt, which
    /// is more than the generic envelope snapshot). Carries no timing.
    NodeIo {
        node_id: String,
        input: Option<String>,
        output: Option<String>,
    },
    /// Which model a `llm` step used and what it spent. `requested` is the name
    /// the flow asked for (often an alias); `served` is what the backend says
    /// it ran. Fields arrive in separate events (the request is built before
    /// the answer exists) and merge per node; an absent field never erases one
    /// already reported.
    NodeModel {
        node_id: String,
        requested: Option<String>,
        served: Option<String>,
        prompt_tokens: Option<u64>,
        completion_tokens: Option<u64>,
    },
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
    /// The content of one tool call, for a sink that keeps content: the
    /// arguments the model sent (reported just before the call starts) and the
    /// text the tool returned (just before the call settles). Never reaches a
    /// live view.
    ToolIo {
        call_id: String,
        arguments: Option<String>,
        result: Option<String>,
    },
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
    /// A top-level flow run ended, whichever way: `completed`, `failed` or
    /// `cancelled`. Emitted once per run by the executor (nested subflow, loop
    /// and map runs share the scope and do not emit it), so the event log can
    /// close a run that made no LLM call. Carries no content. `run_id` is
    /// stamped at publish time: a session scope carries one run after another,
    /// and by the time the log reads the event the scope may be bound to the
    /// next run, which must not inherit this run's end.
    RunFinished { run_id: String, status: String },
}

/// A node error that also carries a reason safe to store. Its `Display` is the
/// node's full error message, so every consumer of the error (execution log,
/// client error, step text) sees exactly what it saw before; `reason` is what
/// the event log may keep. The raw message of a node may quote the data it was
/// handling, so the log never stores it; only a reason carried by this type
/// reaches the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeFailure {
    message: String,
    reason: String,
}

impl SafeFailure {
    pub fn new(message: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            reason: reason.into(),
        }
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }
}

impl std::fmt::Display for SafeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SafeFailure {}

/// Narrow sink the executor publishes progress to. Mirrors `MetricsSink`:
/// `Send + Sync`, fire-and-forget, never blocks the execution path.
///
/// `scope` is the broadcast key (session id or run id) the production sink
/// already knows for this run — passing it per-call keeps the sink itself
/// stateless and lets one sink serve runs across scopes.
pub trait ProgressSink: Send + Sync {
    fn emit(&self, scope: &str, event: ProgressEvent);

    /// Whether anyone keeps the content of a step. Building a `NodeIo` means
    /// serialising every node's payload, so the engine only does it for a sink
    /// that stores it.
    fn wants_io(&self) -> bool {
        false
    }
}

/// The run a model call is made on behalf of, when the call is not a top-level
/// run itself (an `addon.*` block inside a flow calling `llm_generate`).
///
/// `origin` is the parent's, so a bus run stays a bus call. `sink` is the
/// parent's progress sink with its scope: the call's content is recorded there
/// as a child step of the parent run. It is `None` when nothing may be recorded
/// for the call: a Bus or Mesh parent, or a sink that keeps no content.
#[derive(Clone)]
pub struct ParentRun {
    pub origin: crate::flow_engine::dispatcher::FlowOrigin,
    pub sink: Option<std::sync::Arc<dyn ProgressSink>>,
    pub scope: String,
}

impl ParentRun {
    /// A parent run. The sink is kept only for an origin whose content is
    /// recorded (never Bus or Mesh) and only when the sink stores content.
    pub fn new(
        origin: crate::flow_engine::dispatcher::FlowOrigin,
        sink: std::sync::Arc<dyn ProgressSink>,
        scope: String,
    ) -> Self {
        let records = !matches!(
            origin,
            crate::flow_engine::dispatcher::FlowOrigin::Bus
                | crate::flow_engine::dispatcher::FlowOrigin::Mesh
        ) && sink.wants_io();
        Self {
            origin,
            sink: records.then_some(sink),
            scope,
        }
    }

    /// A parent whose calls are never recorded (a mesh forward, a bus run
    /// seen without its sink).
    pub fn unrecorded(origin: crate::flow_engine::dispatcher::FlowOrigin) -> Self {
        Self {
            origin,
            sink: None,
            scope: String::new(),
        }
    }
}

impl std::fmt::Debug for ParentRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParentRun")
            .field("origin", &self.origin)
            .field("recorded", &self.sink.is_some())
            .field("scope", &self.scope)
            .finish()
    }
}

impl PartialEq for ParentRun {
    fn eq(&self, other: &Self) -> bool {
        self.origin == other.origin
            && self.scope == other.scope
            && match (&self.sink, &other.sink) {
                (Some(a), Some(b)) => std::sync::Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
    }
}

impl Eq for ParentRun {}

/// Default no-op sink used by tests/test_support and whenever no broker is
/// wired (headless deploys without the dashboard).
pub struct NoopProgress;

impl ProgressSink for NoopProgress {
    fn emit(&self, _scope: &str, _event: ProgressEvent) {}
}
