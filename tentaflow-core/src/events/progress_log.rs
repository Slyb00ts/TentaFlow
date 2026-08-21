// ===== File: events/progress_log.rs — the flow engine's progress stream, written to the log =====
//
// §2.6. The engine already announces everything the timeline needs; nothing in
// this file measures anything. It subscribes to the `ProgressBroker` and turns
// the events a run publishes into `run_events` rows, so every duration the UI
// shows stays a DIFFERENCE BETWEEN EVENTS (invariant 5). No counter, no timer
// and no new emission point is added anywhere for it.
//
// **Provenance comes from the run, not from the event.** A `ProgressEvent`
// carries a node id and a status and nothing else — it is minted deep inside the
// executor where the only values in reach are ones a node can write. The
// dispatcher therefore binds the run's server-stamped `FlowRequestMeta` to the
// broadcast scope (`progress_broker::begin_run`) and this subscriber COPIES that
// binding onto every row. `envelope.meta` is never read here, so a model that
// writes `origin` into its own envelope changes nothing about what is stored
// (invariant 1).
//
// **The tail may be lost; nothing may be blocked.** The broadcast ring is
// bounded (`SCOPE_CHANNEL_CAPACITY`) and `publish` never waits, so a writer that
// falls behind makes the executor drop timeline events, not stall. A `Lagged`
// receiver logs the gap and continues: missing rows are a hole in a diagnostic
// tool, and inventing them would be worse than not having them (invariant 6).
// The audit copy does not travel this path at all — it is written inside the
// same transaction as its event by `store::append` and delivered by
// `audit_outbox`, which is at-least-once (§2.8).
//
// **`at_ms` is an EMISSION time and never a receipt time.** The stamp arrives
// with the event: `ProgressBroker::publish` takes it inside the emitter's own
// call and the ring carries it as `StampedProgressEvent`. Nothing on this side
// reads a clock, which is what makes the batching below free — a batch may be
// written a wakeup, or a whole blocking write, after the events in it happened,
// and the rows still say when they happened. Stamping at the broker rather than
// at each emit site keeps the emitters (executor, harness) clock-free, which is
// the same rule as invariant 5 one level up.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use dashmap::DashMap;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tokio::sync::broadcast::Receiver;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::db::DbPool;
use crate::events::store::{self, BodyOmission, EventPayload, ResponseBody, RunEvent};
use crate::flow_engine::dispatchers::ProgressEvent;
use crate::flow_engine::progress_broker::{
    global_broker, ProgressBroker, RunProvenance, StampedProgressEvent,
};

/// How long a scope may go without a single event before its subscriber gives
/// up. A run parked on a slow tool or on `waiting_user` stays silent for
/// minutes, so this is deliberately far above any node's own budget; it exists
/// only so a scope nobody will ever publish to again cannot keep a task and a
/// broadcast channel alive for the life of the process.
const IDLE_TIMEOUT: Duration = Duration::from_secs(900);

/// Events written per transaction. After one event arrives, whatever else is
/// already queued behind it is drained and written together — a burst of node
/// starts costs one transaction instead of one per node, which is what keeps
/// the reader from lagging in the first place.
const BATCH_MAX: usize = 128;

/// Process-wide log, published by [`start`] from `events::init`.
static LOG: OnceLock<RunEventLog> = OnceLock::new();

/// The subscriber side of the event log: one task per active scope, all sharing
/// the events pool and one cancellation token.
#[derive(Clone)]
pub struct RunEventLog {
    inner: Arc<Inner>,
}

struct Inner {
    pool: DbPool,
    /// The MAIN database. Carried only so the writer can resolve the
    /// per-organisation response-body setting (`store::write_event`); nothing
    /// on this path reads or writes it for any other reason.
    core_db: DbPool,
    broker: Arc<ProgressBroker>,
    /// Scopes with a live subscriber. An entry is claimed before the task is
    /// spawned and removed by the task on its way out, so `attach` is
    /// idempotent for the whole life of a run.
    scopes: DashMap<String, ()>,
    cancel: CancellationToken,
}

impl RunEventLog {
    pub fn new(pool: DbPool, core_db: DbPool, broker: Arc<ProgressBroker>) -> Self {
        Self {
            inner: Arc::new(Inner {
                pool,
                core_db,
                broker,
                scopes: DashMap::new(),
                cancel: CancellationToken::new(),
            }),
        }
    }

    /// Starts watching `scope` unless it is already watched, and reports
    /// whether a subscriber is watching it once this call returns.
    ///
    /// The answer is what `progress_broker::begin_run` needs: a subscriber owns
    /// the scope's provenance binding and releases it on its way out, so a
    /// `false` here means nobody will ever release it and the caller has to.
    ///
    /// The subscription is taken SYNCHRONOUSLY, before the task exists: the
    /// broker's `publish` is a no-op for a scope with no channel, so a receiver
    /// created inside the spawned task could miss the run's first events. The
    /// task then only owns the draining.
    pub fn attach(&self, scope: &str) -> bool {
        if scope.is_empty() || self.inner.cancel.is_cancelled() {
            return false;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            // No runtime: a synchronous test context, not a real dispatch.
            return false;
        };
        if self.inner.scopes.insert(scope.to_string(), ()).is_some() {
            return true;
        }
        let rx = self.inner.broker.subscribe(scope);
        let log = self.clone();
        let scope = scope.to_string();
        handle.spawn(async move { log.watch(scope, rx).await });
        true
    }

    /// Stops every scope subscriber. The tasks settle on their next await, drop
    /// their receivers and release the provenance they were holding.
    pub fn stop(&self) {
        self.inner.cancel.cancel();
    }

    async fn watch(self, scope: String, mut rx: Receiver<StampedProgressEvent>) {
        let mut state = ScopeState::default();
        loop {
            let received = tokio::select! {
                _ = self.inner.cancel.cancelled() => break,
                r = tokio::time::timeout(IDLE_TIMEOUT, rx.recv()) => r,
            };
            let first = match received {
                Err(_elapsed) => break,
                Ok(Err(RecvError::Closed)) => break,
                Ok(Err(RecvError::Lagged(skipped))) => {
                    warn!(
                        scope = %scope,
                        skipped,
                        "event log fell behind the progress stream; timeline rows dropped"
                    );
                    continue;
                }
                Ok(Ok(event)) => event,
            };

            let mut batch = vec![first];
            while batch.len() < BATCH_MAX {
                match rx.try_recv() {
                    Ok(event) => batch.push(event),
                    Err(TryRecvError::Lagged(skipped)) => {
                        warn!(
                            scope = %scope,
                            skipped,
                            "event log fell behind the progress stream; timeline rows dropped"
                        );
                    }
                    Err(TryRecvError::Empty) | Err(TryRecvError::Closed) => break,
                }
            }

            // The provenance is read once per batch and not once per event: a
            // rebinding mid-batch would mean a new run started while these
            // events were queued, and the events in hand belong to the run that
            // was current when they were emitted.
            let Some(provenance) = self.inner.broker.run_provenance(&scope) else {
                debug!(scope = %scope, "progress events with no bound run; not stored");
                continue;
            };
            state.retarget(&provenance);

            let mut rows: Vec<RunEvent> = Vec::with_capacity(batch.len());
            for stamped in batch {
                state.translate(&provenance, stamped.at_ms, stamped.event, &mut rows);
            }
            if rows.is_empty() {
                continue;
            }

            let pool = self.inner.pool.clone();
            let core_db = self.inner.core_db.clone();
            let write =
                tokio::task::spawn_blocking(move || write_batch(&pool, &core_db, rows)).await;
            match write {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    warn!(scope = %scope, %error, "event log write failed; timeline rows lost")
                }
                Err(error) => {
                    warn!(scope = %scope, %error, "event log writer panicked")
                }
            }
        }

        self.inner.scopes.remove(&scope);
        self.inner.broker.release_run_provenance(&scope);
    }
}

/// Writes a batch in ONE immediate transaction. `store::append_in_tx` still
/// allocates each `seq` inside it, so the batch does not weaken invariant 2 —
/// it only saves the per-event commit.
fn write_batch(pool: &DbPool, core_db: &DbPool, rows: Vec<RunEvent>) -> anyhow::Result<()> {
    let mut conn = pool
        .write()
        .map_err(|e| anyhow::anyhow!("events db write: {e}"))?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    for row in rows {
        store::append_in_tx(&tx, core_db, row)?;
    }
    tx.commit()?;
    Ok(())
}

/// What one scope's subscriber remembers between events.
#[derive(Default)]
struct ScopeState {
    run_id: Option<String>,
    /// Whether this run's `request_started` row has been written. The engine
    /// announces no "run began" event, so the opening of a run is read off the
    /// first node it starts — see [`ScopeState::translate`].
    request_opened: bool,
    /// `node_id -> node_type`, learnt from `NodeStarted`. `NodeFinished` does
    /// not repeat the type and the log will not guess one: a node whose start
    /// was never seen closes with an EMPTY step label, which reads as the gap it
    /// is (invariant 6).
    node_types: HashMap<String, String>,
    /// Nodes that have produced a first token in this run. A node in here is
    /// generating an assistant message, so its `NodeFinished` closes that
    /// message; a node that never streamed anything closes only a step.
    streaming_nodes: HashSet<String>,
}

/// Builds one timeline row from the run's provenance. The provenance is COPIED
/// wholesale — nothing here derives an origin, an actor or a tenant from the
/// event, which is what keeps a model that writes `origin` into its own
/// envelope unable to move a row (invariant 1).
fn row(
    provenance: &RunProvenance,
    at_ms: i64,
    payload: EventPayload,
    node_id: Option<String>,
    call_id: Option<String>,
) -> RunEvent {
    RunEvent {
        run_id: provenance.run_id.clone(),
        at_ms,
        origin: provenance.origin,
        actor_kind: provenance.actor_kind,
        actor_id: provenance.actor_id.clone(),
        actor_user_id: provenance.actor_user_id.clone(),
        correlation_id: provenance.correlation_id.clone(),
        session_id: provenance.session_id.clone(),
        node_id,
        call_id,
        // A progress event has no natural key: two `first_token`s of two
        // steps are two facts, not a retry of one. Deduplication is for
        // callers that can replay a write, and this one cannot.
        idempotency_key: None,
        org_id: provenance.org_id.clone(),
        payload,
    }
}

impl ScopeState {
    /// Clears per-run memory when the scope moves on to a different run. A
    /// session scope carries one run after another, and a node id from the
    /// previous one says nothing about this one.
    fn retarget(&mut self, provenance: &RunProvenance) {
        if self.run_id.as_deref() != Some(provenance.run_id.as_str()) {
            self.run_id = Some(provenance.run_id.clone());
            self.request_opened = false;
            self.node_types.clear();
            self.streaming_nodes.clear();
        }
    }

    /// Maps one engine event onto timeline rows, appending them to `out`. Most
    /// events produce one row; two produce a second, and several produce none.
    ///
    /// **`request_started` is the first node of the run starting.** The engine
    /// announces no opening of a run, but it does not have to:
    /// `progress_broker::begin_run` binds the provenance and attaches this
    /// subscriber BEFORE the engine touches the first node, so the first
    /// `NodeStarted` under a fresh binding is the earliest instant of that run
    /// this side can observe, and nothing of the run precedes it. Its four
    /// descriptors stay `None` — `FlowRequestMeta` carries no model, flow id,
    /// service type or modality, and this file will not guess any of them
    /// (invariant 6). What the row does carry is the accountability half the
    /// audit mirror exists for: origin, actor, tenant and correlation id.
    ///
    /// **`assistant_message` is a streaming node finishing.** A node that
    /// produced a `FirstToken` was generating the answer, so its `NodeFinished`
    /// is the instant that answer completed — which is exactly the far end of
    /// §2.7's decode time. The BODY is not in the stream and is stored as
    /// `BodyOmission::NotCarried` rather than as an empty string; a node that
    /// failed closes as `error` and gets no message row at all, because a
    /// message that never completed has no completion instant.
    ///
    /// The variants deliberately dropped are `MapElement`, `Compaction`,
    /// `ChildSpawned`, `ChildFinished`, `RouterDecision`, `UserQuestion`,
    /// `PermissionRequest` and `InteractionResolved` — recording them would mean
    /// inventing a kind, and folding them into `step_*` would make step counts
    /// and step durations mean two different things at once.
    fn translate(
        &mut self,
        provenance: &RunProvenance,
        at_ms: i64,
        event: ProgressEvent,
        out: &mut Vec<RunEvent>,
    ) {
        match event {
            ProgressEvent::NodeStarted { node_id, node_type } => {
                if !self.request_opened {
                    self.request_opened = true;
                    out.push(row(
                        provenance,
                        at_ms,
                        EventPayload::RequestStarted {
                            model: None,
                            flow_id: None,
                            service_type: None,
                            modality: None,
                        },
                        None,
                        None,
                    ));
                }
                self.node_types.insert(node_id.clone(), node_type.clone());
                out.push(row(
                    provenance,
                    at_ms,
                    EventPayload::StepStart { step: node_type },
                    Some(node_id),
                    None,
                ));
            }
            ProgressEvent::NodeFinished { node_id, status } => {
                let step = self.node_types.get(&node_id).cloned().unwrap_or_default();
                let streamed = self.streaming_nodes.remove(&node_id);
                // A failed node closes as `error`, not as `step_end`: §2.3 has a
                // kind for a failure and nothing else in the engine emits it. The
                // consequence is deliberate — the step stays UNCLOSED, so a
                // duration that was never completed produces no number at all
                // rather than one measured to a failure.
                if status == "error" {
                    out.push(row(
                        provenance,
                        at_ms,
                        EventPayload::Error {
                            stage: step,
                            message: status,
                        },
                        Some(node_id),
                        None,
                    ));
                    return;
                }
                if streamed {
                    out.push(row(
                        provenance,
                        at_ms,
                        EventPayload::AssistantMessage {
                            body: ResponseBody::Omitted(BodyOmission::NotCarried),
                            // The engine reports no token count on this path;
                            // a zero would be a fabricated one.
                            tokens: None,
                        },
                        Some(node_id.clone()),
                        None,
                    ));
                }
                out.push(row(
                    provenance,
                    at_ms,
                    EventPayload::StepEnd { step, status },
                    Some(node_id),
                    None,
                ));
            }
            ProgressEvent::FirstToken { node_id } => {
                self.streaming_nodes.insert(node_id.clone());
                out.push(row(
                    provenance,
                    at_ms,
                    EventPayload::FirstToken {},
                    Some(node_id),
                    None,
                ));
            }
            ProgressEvent::IterationStarted { node_id, n, .. } => {
                // `max` is the configured iteration budget, not a fact about
                // this turn, and the schema has nowhere to put it.
                out.push(row(
                    provenance,
                    at_ms,
                    EventPayload::TurnStart { turn: n },
                    Some(node_id),
                    None,
                ));
            }
            ProgressEvent::IterationFinished { node_id, n } => {
                out.push(row(
                    provenance,
                    at_ms,
                    // The engine reports no outcome for a finished iteration, so
                    // the status is empty rather than a cheerful `ok`.
                    EventPayload::TurnEnd {
                        turn: n,
                        status: String::new(),
                    },
                    Some(node_id),
                    None,
                ));
            }
            ProgressEvent::ToolCallStarted { call_id, name } => {
                out.push(row(
                    provenance,
                    at_ms,
                    // The progress event carries no arguments; an empty map is
                    // the absence of them, not a call that took none.
                    EventPayload::ToolCall {
                        name,
                        arguments: Default::default(),
                    },
                    None,
                    Some(call_id),
                ));
            }
            ProgressEvent::ToolCallFinished {
                call_id, status, ..
            } => {
                out.push(row(
                    provenance,
                    at_ms,
                    EventPayload::ToolResult {
                        ok: status == "ok",
                        summary: status,
                    },
                    None,
                    Some(call_id),
                ));
            }
            ProgressEvent::MapElement { .. }
            | ProgressEvent::Compaction { .. }
            | ProgressEvent::ChildSpawned { .. }
            | ProgressEvent::ChildFinished { .. }
            | ProgressEvent::RouterDecision { .. }
            | ProgressEvent::UserQuestion { .. }
            | ProgressEvent::PermissionRequest { .. }
            | ProgressEvent::InteractionResolved { .. } => {}
        }
    }
}

/// Publishes the process-wide log. Called by `events::init`, alongside the
/// audit-outbox loop and the retention sweep, so the store and the thing that
/// fills it start from the same place.
pub fn start(pool: DbPool, core_db: DbPool) {
    let _ = LOG.set(RunEventLog::new(pool, core_db, global_broker()));
}

/// Starts watching a broadcast scope, reporting whether anything is watching it
/// afterwards. Called by `progress_broker::begin_run`, which releases the
/// scope's provenance binding on `false`. Answers `false` before `start`, which
/// is what a headless build or a unit test that never opened `events.db` gets.
pub fn attach_scope(scope: &str) -> bool {
    match LOG.get() {
        Some(log) => log.attach(scope),
        None => false,
    }
}

/// Stops every scope subscriber of the process-wide log. The counterpart of
/// [`start`] for a clean shutdown, called from `tentaflow`'s shutdown path
/// immediately before `events::db::checkpoint_wal` so no subscriber is still
/// appending while the WAL is being truncated.
pub fn stop() {
    if let Some(log) = LOG.get() {
        log.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::store::{read_run, StoredEvent};
    use crate::flow_engine::dispatcher::{FlowActor, FlowOrigin, FlowRequestMeta};
    use crate::flow_engine::dispatchers::ProgressSink;
    use crate::flow_engine::progress_broker::BrokerProgressSink;

    const SCOPE: &str = "scope-under-test";

    fn provenance(run_id: &str) -> RunProvenance {
        let mut meta = FlowRequestMeta::new(
            run_id,
            FlowOrigin::Api,
            FlowActor::api_key("key-42", Some("user-7".into())),
        );
        meta.org_id = Some("org-3".into());
        meta.correlation_id = Some("corr-1".into());
        meta.session_id = Some(SCOPE.into());
        RunProvenance::from_meta(&meta)
    }

    /// Wires a broker, a sink and a log over a fresh events database and starts
    /// watching one scope — the same order `begin_run` uses in production.
    struct Harness {
        _dir: tempfile::TempDir,
        pool: DbPool,
        broker: Arc<ProgressBroker>,
        sink: BrokerProgressSink,
        log: RunEventLog,
    }

    impl Harness {
        fn start(run_id: &str) -> Self {
            let (dir, pool) = crate::events::test_support::events_db();
            let broker = Arc::new(ProgressBroker::new());
            broker.bind_run_provenance(SCOPE, provenance(run_id));
            let log = RunEventLog::new(
                pool.clone(),
                crate::events::test_support::main_db(),
                broker.clone(),
            );
            log.attach(SCOPE);
            Self {
                _dir: dir,
                pool,
                sink: BrokerProgressSink::new(broker.clone()),
                broker,
                log,
            }
        }

        fn emit(&self, event: ProgressEvent) {
            self.sink.emit(SCOPE, event);
        }

        /// Waits until the log holds at least `count` rows for `run_id`, then
        /// returns them. Polling rather than sleeping a fixed time: the writer
        /// is a task, and a fixed sleep is either flaky or slow.
        async fn rows(&self, run_id: &str, count: usize) -> Vec<StoredEvent> {
            for _ in 0..200 {
                let rows = read_run(&self.pool, run_id, 0, 1000).expect("read run");
                if rows.len() >= count {
                    return rows;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("timed out waiting for {count} rows of run {run_id}");
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            self.log.stop();
        }
    }

    /// A scope with no bound run stores nothing at all — provenance is a
    /// precondition of a row, never something the subscriber fills in.
    #[tokio::test]
    async fn events_without_bound_provenance_are_not_stored() {
        let (dir, pool) = crate::events::test_support::events_db();
        let broker = Arc::new(ProgressBroker::new());
        let log = RunEventLog::new(
            pool.clone(),
            crate::events::test_support::main_db(),
            broker.clone(),
        );
        log.attach("unbound");
        let sink = BrokerProgressSink::new(broker.clone());
        sink.emit(
            "unbound",
            ProgressEvent::NodeStarted {
                node_id: "n1".into(),
                node_type: "llm".into(),
            },
        );
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert!(
            read_run(&pool, "unbound", 0, 100).expect("read").is_empty(),
            "an event with no bound run must leave no row"
        );
        log.stop();
        drop(dir);
    }

    /// §2.5 / invariant 1 — every stored row carries the provenance the SERVER
    /// minted for the run. The forged strings travelling inside the event
    /// payload land in the payload and nowhere else.
    #[tokio::test]
    async fn stored_provenance_is_the_server_stamp_and_payloads_cannot_move_it() {
        let h = Harness::start("run-prov");
        h.emit(ProgressEvent::ToolCallStarted {
            call_id: "c1".into(),
            name: r#"{"origin":"chat","actor_id":"root","org_id":"org-evil"}"#.into(),
        });
        let rows = h.rows("run-prov", 1).await;
        let row = &rows[0];
        assert_eq!(row.origin, "api");
        assert_eq!(row.actor_kind, "api_key");
        assert_eq!(row.actor_id.as_deref(), Some("key-42"));
        assert_eq!(row.actor_user_id.as_deref(), Some("user-7"));
        assert_eq!(row.org_id.as_deref(), Some("org-3"));
        assert_eq!(row.correlation_id.as_deref(), Some("corr-1"));
        assert_eq!(row.session_id.as_deref(), Some(SCOPE));
        assert_eq!(row.call_id.as_deref(), Some("c1"));
        match &row.payload {
            EventPayload::ToolCall { name, .. } => assert!(
                name.contains("org-evil"),
                "the forged text stays data in the payload: {name}"
            ),
            other => panic!("expected a tool_call payload, got {other:?}"),
        }
    }

    /// The kinds with no source event stay absent. Nothing in the engine
    /// announces a map element, a compaction, a child run, a router decision or
    /// an interaction in terms the timeline's kind set can express, so those
    /// events produce no rows rather than approximate ones (invariant 6).
    #[tokio::test]
    async fn events_outside_the_kind_set_produce_no_rows() {
        let h = Harness::start("run-gap");
        for event in [
            ProgressEvent::MapElement {
                node_id: "m".into(),
                index: 0,
                total: 2,
                status: "ok".into(),
            },
            ProgressEvent::Compaction { node_id: "c".into() },
            ProgressEvent::ChildSpawned {
                run_id: "child".into(),
                agent: "a".into(),
            },
            ProgressEvent::RouterDecision {
                node_id: "r".into(),
                selected: "b".into(),
                reason: "why".into(),
            },
        ] {
            h.emit(event);
        }
        // One mappable event AFTER them: when it lands, the unmappable ones have
        // provably already been through the translator.
        h.emit(ProgressEvent::NodeStarted {
            node_id: "n".into(),
            node_type: "llm".into(),
        });
        let rows = h.rows("run-gap", 2).await;
        assert_eq!(
            rows.iter().map(|r| r.kind).collect::<Vec<_>>(),
            vec![
                crate::events::EventKind::RequestStarted,
                crate::events::EventKind::StepStart,
            ],
            "only the node start is representable, and it opens the run: {rows:?}"
        );
    }

    /// A failed node closes the step as `error`, which leaves the step without
    /// a `step_end` on purpose — see the note in `translate`.
    #[tokio::test]
    async fn a_failed_node_is_stored_as_an_error_not_as_a_step_end() {
        let h = Harness::start("run-err");
        h.emit(ProgressEvent::NodeStarted {
            node_id: "n".into(),
            node_type: "llm".into(),
        });
        h.emit(ProgressEvent::NodeFinished {
            node_id: "n".into(),
            status: "error".into(),
        });
        let rows = h.rows("run-err", 3).await;
        assert_eq!(rows[0].kind, crate::events::EventKind::RequestStarted);
        assert_eq!(rows[1].kind, crate::events::EventKind::StepStart);
        assert_eq!(rows[2].kind, crate::events::EventKind::Error);
        match &rows[2].payload {
            EventPayload::Error { stage, .. } => {
                assert_eq!(stage, "llm", "the stage is the node type learnt at start")
            }
            other => panic!("expected an error payload, got {other:?}"),
        }
    }

    /// §2.3 — the two kinds no engine event names outright. A run OPENS with
    /// `request_started` (its first node starting: `begin_run` binds and
    /// attaches before the engine touches the flow, so nothing precedes it) and
    /// a node that streamed tokens CLOSES with `assistant_message` before its
    /// `step_end`. Without these two rows §2.7's TTFT and decode time have no
    /// endpoints and the audit outbox has nothing to deliver.
    #[tokio::test]
    async fn a_run_opens_with_request_started_and_a_streamed_node_closes_the_message() {
        let h = Harness::start("run-shape");
        h.emit(ProgressEvent::NodeStarted {
            node_id: "llm-1".into(),
            node_type: "llm".into(),
        });
        h.emit(ProgressEvent::FirstToken {
            node_id: "llm-1".into(),
        });
        h.emit(ProgressEvent::NodeFinished {
            node_id: "llm-1".into(),
            status: "ok".into(),
        });
        let rows = h.rows("run-shape", 5).await;
        assert_eq!(
            rows.iter().map(|r| r.kind).collect::<Vec<_>>(),
            vec![
                crate::events::EventKind::RequestStarted,
                crate::events::EventKind::StepStart,
                crate::events::EventKind::FirstToken,
                crate::events::EventKind::AssistantMessage,
                crate::events::EventKind::StepEnd,
            ],
            "the timeline shape §2.7 measures over: {rows:?}"
        );
        // The opening row carries the accountability stamp and nothing invented:
        // no engine event names the model or the flow, so those stay absent.
        match &rows[0].payload {
            EventPayload::RequestStarted {
                model,
                flow_id,
                service_type,
                modality,
            } => {
                assert_eq!(rows[0].actor_id.as_deref(), Some("key-42"));
                assert!(
                    model.is_none()
                        && flow_id.is_none()
                        && service_type.is_none()
                        && modality.is_none(),
                    "descriptors the stream does not carry must stay empty"
                );
            }
            other => panic!("expected a request_started payload, got {other:?}"),
        }
        // The message says WHEN it completed; its text was never in the stream,
        // and the row says which of the two that is.
        match &rows[3].payload {
            EventPayload::AssistantMessage { body, tokens } => {
                assert_eq!(
                    body,
                    &crate::events::ResponseBody::Omitted(crate::events::BodyOmission::NotCarried)
                );
                assert!(tokens.is_none(), "no engine event counts the tokens");
            }
            other => panic!("expected an assistant_message payload, got {other:?}"),
        }
        assert_eq!(rows[3].at_ms, rows[4].at_ms, "one event, two facts");

        // §2.8 — `request_started` is the security-relevant kind, so the mirror
        // the delivery loop drains is no longer permanently empty.
        let queued: i64 = h
            .pool
            .read()
            .expect("read")
            .query_row(
                "SELECT COUNT(*) FROM audit_outbox WHERE run_id = ?1",
                rusqlite::params!["run-shape"],
                |row| row.get(0),
            )
            .expect("count outbox");
        assert_eq!(queued, 1, "the opening of a run is mirrored for audit");
    }

    /// Defect of the first cut, pinned: `at_ms` is the instant of EMISSION.
    ///
    /// The runtime is single-threaded and the wait is a BLOCKING sleep, so the
    /// watcher task cannot run between the two emissions — both events sit in
    /// the ring and are drained into ONE batch. A stamp taken when the batch is
    /// read would put the rows within a millisecond of each other and every
    /// duration in the log would be a difference of receipt times.
    #[tokio::test]
    async fn a_batch_written_late_still_carries_the_emission_instants() {
        let h = Harness::start("run-batch");
        h.emit(ProgressEvent::NodeStarted {
            node_id: "llm-1".into(),
            node_type: "llm".into(),
        });
        std::thread::sleep(Duration::from_millis(250));
        h.emit(ProgressEvent::FirstToken {
            node_id: "llm-1".into(),
        });

        let rows = h.rows("run-batch", 3).await;
        let start = rows
            .iter()
            .find(|r| r.kind == crate::events::EventKind::StepStart)
            .expect("the step start");
        let token = rows
            .iter()
            .find(|r| r.kind == crate::events::EventKind::FirstToken)
            .expect("the first token");
        let gap = token.at_ms - start.at_ms;
        assert!(
            (249..400).contains(&gap),
            "the emissions were 250 ms apart; the stored rows say {gap} ms"
        );
    }

    /// A second run under the SAME session scope is stored under its own run id,
    /// and does not inherit the first run's remembered node types. This is the
    /// LEGITIMATE rebinding — one principal's session carrying one run after
    /// another. A rebinding by a different principal is refused by the broker
    /// (`a_foreign_principal_cannot_repoint_a_live_scope`).
    #[tokio::test]
    async fn rebinding_a_scope_moves_later_events_onto_the_new_run() {
        let h = Harness::start("run-first");
        h.emit(ProgressEvent::NodeStarted {
            node_id: "n".into(),
            node_type: "llm".into(),
        });
        h.rows("run-first", 2).await;

        h.broker.bind_run_provenance(SCOPE, provenance("run-second"));
        h.emit(ProgressEvent::NodeFinished {
            node_id: "n".into(),
            status: "ok".into(),
        });
        let rows = h.rows("run-second", 1).await;
        assert_eq!(rows.len(), 1);
        match &rows[0].payload {
            EventPayload::StepEnd { step, .. } => assert!(
                step.is_empty(),
                "a node type learnt in another run must not be reused: {step:?}"
            ),
            other => panic!("expected a step_end payload, got {other:?}"),
        }
    }
}
