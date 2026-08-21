// =============================================================================
// Plik: flow_engine/progress_broker.rs
// Opis: ProgressBroker (§3.11 C) — per-scope tokio broadcast fan-out for
//       ephemeral run progress events. Lives in AppState; the production
//       ProgressSink publishes here and phase-3 wire handlers
//       (AgentsPayload::RunEventsSubscribe) subscribe per scope. Events are
//       NOT persisted — durable record is `run_log`.
// =============================================================================

use std::sync::Arc;
use std::sync::OnceLock;

use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use tokio::sync::broadcast;
use tracing::warn;

use crate::events::store::now_ms;
use crate::flow_engine::dispatcher::{ActorKind, FlowOrigin, FlowRequestMeta};
use crate::flow_engine::dispatchers::{ProgressEvent, ProgressSink};

/// Process-global broker. The dashboard server builds one `AppState` per WS
/// connection but all connections must share a single broker so a run started
/// on one socket is visible to a subscriber on another. Mirrors the
/// `ui_session::global_registry` pattern.
static GLOBAL_BROKER: OnceLock<Arc<ProgressBroker>> = OnceLock::new();

/// Returns the shared broker, initialising it on first call.
pub fn global_broker() -> Arc<ProgressBroker> {
    GLOBAL_BROKER
        .get_or_init(|| Arc::new(ProgressBroker::new()))
        .clone()
}

/// One published progress event together with the instant it was EMITTED.
///
/// The stamp is taken in [`ProgressBroker::publish`] — the single point where an
/// emitted event enters the broadcast, reached synchronously from every emitter
/// through `ProgressSink::emit` or a direct `publish`. It travels with the event
/// because a consumer cannot recover it afterwards: the event log batches, and a
/// subscriber that a tokio wakeup reaches late, or that is still writing the
/// previous batch, would date every row by when it happened to look rather than
/// by when the thing happened. Every duration read back out of the log is a
/// difference of two of these stamps, so the difference has to be one of
/// emission instants (§2.7, invariant 5).
///
/// Stamping here rather than at each emit site is deliberate: the emitters are
/// spread across the executor and the harness, and giving each one a clock would
/// be the per-adapter instrumentation the design forbids.
#[derive(Debug, Clone, PartialEq)]
pub struct StampedProgressEvent {
    /// Epoch milliseconds — the unit `run_events.at_ms` stores.
    pub at_ms: i64,
    pub event: ProgressEvent,
}

/// Capacity of each per-scope broadcast ring. A slow subscriber that lags past
/// this many events gets `RecvError::Lagged` (the UI reconciles from RunDetail
/// on reconnect — §3.11 C), so a dropped tail never blocks the executor.
const SCOPE_CHANNEL_CAPACITY: usize = 256;

/// Server-minted provenance of the run currently executing under a scope.
///
/// A `ProgressEvent` carries none of this and never could: it is emitted from
/// inside the executor, where the only values in reach are the ones nodes can
/// write. The event log needs `origin`, the actor, the tenant and the
/// correlation id for every row it stores (§2.3), so the dispatcher binds them
/// HERE — once per run, from the `FlowRequestMeta` an entry point stamped after
/// authorization — and the log copies the binding instead of deriving anything
/// from the event (invariant 1).
///
/// This is NOT an authorization token and must never be used as one: the ACL
/// that decides who may subscribe stays `session_owners`. A rebinding is still
/// allowed — one session scope carries many runs one after another, each with
/// its own `request_id` — but only WITHIN ONE PRINCIPAL, see
/// [`ProgressBroker::bind_run_provenance`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunProvenance {
    /// `FlowRequestMeta.request_id` — the `run_id` every stored row is keyed by.
    pub run_id: String,
    pub origin: FlowOrigin,
    pub actor_kind: ActorKind,
    pub actor_id: Option<String>,
    pub actor_user_id: Option<String>,
    pub org_id: Option<String>,
    pub correlation_id: Option<String>,
    pub session_id: Option<String>,
    /// WHAT the run executes, the other half of the accountability question
    /// (§2.8: "which actor, from which origin, against which model"). Minted by
    /// the dispatcher at the same instant as this binding — see
    /// [`RunDescriptor`].
    pub descriptor: RunDescriptor,
}

/// Server-minted description of what a run executes: the model routing key it
/// was resolved by and the flow that actually ran.
///
/// It is bound together with [`RunProvenance`] rather than derived later
/// because the dispatcher is the ONLY place that holds these facts as facts.
/// `FlowRequestMeta` does not carry them, a `ProgressEvent` carries nothing but
/// a node id, and `envelope.meta` — the one other place a model name appears
/// near a run — is writable by every node including a WASM addon block, so a
/// value read from there could be chosen by the thing being audited
/// (invariant 1). Every field here is passed BY THE DISPATCHER from the
/// arguments it resolved on, and `flow_id` from the `CompiledFlow` it is about
/// to execute.
///
/// A field is `None` when the run genuinely had no such fact, never as a
/// stand-in: a flow dispatched by id was not resolved from a model routing key,
/// and a direct capability call (`try_dispatch` with no user-defined flow) runs
/// a single node on the executor with no flow at all. An absent field is a gap
/// in the record; a borrowed one would be a false statement in an audit row
/// (invariant 6).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunDescriptor {
    /// Model name the run was dispatched against — `None` for a run reached by
    /// flow id, where no model resolution happened.
    pub model: Option<String>,
    /// The flow that executed — `None` for a direct capability call, which has
    /// no flow.
    pub flow_id: Option<String>,
    /// Capability the model was addressed as (`llm`, `stt`, `tts`, …), the
    /// second component of the resolver key.
    pub service_type: Option<String>,
    /// Third component of the resolver key, derived by the dispatcher from the
    /// shape of the initial payload (`derive_modality`) or fixed by the caller
    /// (`try_dispatch_with_modality`).
    pub modality: Option<String>,
}

impl RunDescriptor {
    /// The descriptor of a run resolved through the `{model}:{service_type}:
    /// {modality}` key. `flow_id` stays empty until [`Self::with_flow`] names
    /// the compiled flow, so a direct capability call keeps it absent.
    pub fn resolved(model: &str, service_type: &str, modality: &str) -> Self {
        Self {
            model: Some(model.to_string()),
            flow_id: None,
            service_type: Some(service_type.to_string()),
            modality: Some(modality.to_string()),
        }
    }

    /// Names the flow that is about to execute. Taken from the `CompiledFlow`
    /// itself rather than from the id a caller asked for, so the record names
    /// what ran.
    pub fn with_flow(mut self, flow_id: &str) -> Self {
        self.flow_id = Some(flow_id.to_string());
        self
    }
}

impl RunProvenance {
    pub fn from_meta(meta: &FlowRequestMeta, descriptor: RunDescriptor) -> Self {
        Self {
            run_id: meta.request_id.clone(),
            origin: meta.origin,
            actor_kind: meta.actor_kind,
            actor_id: meta.actor_id.clone(),
            actor_user_id: meta.actor_user_id.clone(),
            org_id: meta.org_id.clone(),
            correlation_id: meta.correlation_id.clone(),
            session_id: meta.session_id.clone(),
            descriptor,
        }
    }

    /// Whether two bindings name the same authorized PRINCIPAL — the actor and
    /// the tenant an entry point stamped, not the run. A scope's run id changes
    /// with every run under it; who is behind it does not.
    fn same_principal(&self, other: &Self) -> bool {
        self.actor_kind == other.actor_kind
            && self.actor_id == other.actor_id
            && self.actor_user_id == other.actor_user_id
            && self.org_id == other.org_id
    }
}

/// What a scope's provenance slot holds.
///
/// The extra state is not bookkeeping: a broadcast scope is keyed by the
/// session id, which on the foreground path is a value the CLIENT supplies
/// (`stream_handlers`: `meta.session_id = invoke.session_id`). Two principals
/// can therefore end up publishing into one ring, and a `ProgressEvent` carries
/// nothing that says which run it came from — so once that has happened, no
/// stamp the log could apply to the events still in flight is known to be the
/// right one. Storing them under either principal would attribute one user's
/// request to the other, and `request_started` is mirrored into `audit_log`.
/// A hole in a diagnostic log is acceptable; a forged audit row is not
/// (invariant 6 over invariant 1).
#[derive(Debug, Clone)]
enum ScopeProvenance {
    Bound(RunProvenance),
    /// A second principal tried to bind a scope another principal was already
    /// running under. Cleared with the rest of the scope by
    /// [`ProgressBroker::release_run_provenance`] when its subscriber stops.
    Contested,
}

/// Announces a run to the progress layer: binds its server-minted provenance to
/// the broadcast scope and attaches the event-log subscriber to that scope.
///
/// Called from `ContextFactory::make_context`, the one place every dispatch
/// entry passes through holding the authorized `FlowRequestMeta`. The
/// `descriptor` is the same call's answer to WHAT is being run: the dispatcher
/// hands it in because it is the only layer holding the resolved model and the
/// compiled flow, and by the time an event reaches the log neither is
/// recoverable from anything but node-writable state. Two guards
/// keep it to actual runs: a request with no `progress_sink` emits nothing at
/// all (headless, tests), and `flow_depth > 0` is a capability hop re-entering
/// the engine under the SAME scope — rebinding there would move the parent's
/// remaining events onto the inner run's id.
///
/// Attaching before the flow starts is load-bearing: `ProgressBroker::publish`
/// is a no-op for a scope nobody subscribed to, so a subscriber created after
/// the first node started would miss the opening of the run.
pub fn begin_run(meta: &FlowRequestMeta, descriptor: RunDescriptor) {
    if meta.progress_sink.is_none() || meta.flow_depth > 0 {
        return;
    }
    let scope = meta.progress_scope();
    let broker = global_broker();
    broker.bind_run_provenance(&scope, RunProvenance::from_meta(meta, descriptor));
    if !crate::events::progress_log::attach_scope(&scope) {
        // Nothing is watching this scope and nothing will: a headless build
        // that never opened `events.db`, a synchronous test, or a log already
        // shutting down. The subscriber is what releases a binding on its way
        // out, so a binding it never took ownership of has to go back now —
        // otherwise the map grows by one entry per run for the life of the
        // process.
        broker.release_run_provenance(&scope);
    }
}

/// Per-scope broadcast registry. A scope is a session id or a run id. Senders
/// are created lazily on first publish/subscribe and dropped when the last
/// subscriber goes away AND no fresh event arrives — see `prune_if_idle`.
pub struct ProgressBroker {
    scopes: DashMap<String, broadcast::Sender<StampedProgressEvent>>,
    /// §3.3 ACL — server-side binding of a session-scope key to the principal
    /// that started the flow under it. A `Session` event scope is the
    /// client-minted conversation id (low entropy, time-correlated); it MUST
    /// NOT double as an authorization token. The dispatch layer records the
    /// owner here when a foreground flow starts, and `authorize_scope` rejects a
    /// `Session` subscription whose key is unbound or owned by a different
    /// principal (admin bypass). Without this, any authenticated user could
    /// guess/observe another user's session id and read their harness activity
    /// (UserQuestion / PermissionRequest / RouterDecision) — cross-principal
    /// leak (OWASP A01).
    session_owners: DashMap<String, String>,
    /// §2.5 — server-minted provenance of the run executing under each scope,
    /// bound by [`begin_run`] and read by the event-log subscriber. Bounded by
    /// the number of live subscribers: `begin_run` gives the entry back when no
    /// subscriber took it, and the subscriber releases it when it stops.
    run_provenance: DashMap<String, ScopeProvenance>,
}

impl ProgressBroker {
    pub fn new() -> Self {
        Self {
            scopes: DashMap::new(),
            session_owners: DashMap::new(),
            run_provenance: DashMap::new(),
        }
    }

    /// Records the provenance of the run now executing under `scope`.
    ///
    /// A rebinding by the SAME principal is the normal case — a session scope
    /// carries one run after another and each has its own `request_id`. A
    /// rebinding by a DIFFERENT principal is not: the scope key is
    /// client-supplied on the foreground path, so this is how a second user
    /// would re-point a live scope and have the first user's remaining events
    /// filed under their own actor. It neither overwrites nor silently keeps
    /// the old stamp for the newcomer's events — the scope goes
    /// [`ScopeProvenance::Contested`] and stores nothing further.
    pub fn bind_run_provenance(&self, scope: &str, provenance: RunProvenance) {
        if scope.is_empty() {
            return;
        }
        match self.run_provenance.entry(scope.to_string()) {
            Entry::Occupied(mut occupied) => {
                if let ScopeProvenance::Bound(current) = occupied.get() {
                    if current.same_principal(&provenance) {
                        occupied.insert(ScopeProvenance::Bound(provenance));
                        return;
                    }
                }
                warn!(
                    scope = %scope,
                    "a second principal dispatched under a live progress scope; \
                     its events are no longer attributable and are not stored"
                );
                occupied.insert(ScopeProvenance::Contested);
            }
            Entry::Vacant(vacant) => {
                vacant.insert(ScopeProvenance::Bound(provenance));
            }
        }
    }

    /// Provenance of the run currently executing under `scope`, if one is bound
    /// and unambiguous. A contested scope answers `None`, which the event log
    /// already treats as "no bound run; not stored".
    pub fn run_provenance(&self, scope: &str) -> Option<RunProvenance> {
        match self.run_provenance.get(scope)?.value() {
            ScopeProvenance::Bound(provenance) => Some(provenance.clone()),
            ScopeProvenance::Contested => None,
        }
    }

    /// Drops a scope's provenance binding. Called by the event-log subscriber
    /// when it stops watching a scope, so an idle node keeps no run metadata.
    pub fn release_run_provenance(&self, scope: &str) {
        self.run_provenance.remove(scope);
    }

    /// Binds a session-scope key to the principal that started a flow under it.
    /// Called from the foreground dispatch path with the authenticated
    /// `user_id`. Idempotent re-binding to the same owner is a no-op; a session
    /// id is owned by the first principal to claim it, so a second principal
    /// cannot hijack an in-flight session scope by re-dispatching under the same
    /// id (the binding is only cleared by `release_session_owner`).
    pub fn bind_session_owner(&self, session_id: &str, user_id: &str) {
        if session_id.is_empty() || user_id.is_empty() {
            return;
        }
        self.session_owners
            .entry(session_id.to_string())
            .or_insert_with(|| user_id.to_string());
    }

    /// Returns the principal that owns a session-scope key, if bound.
    pub fn session_owner(&self, session_id: &str) -> Option<String> {
        self.session_owners
            .get(session_id)
            .map(|o| o.value().clone())
    }

    /// Subscribe to a scope's progress stream. Creates the channel if absent so
    /// a subscriber that arrives before the first event still receives later
    /// ones. The returned receiver only sees events published AFTER it
    /// subscribed (broadcast semantics) — the dashboard reconciles backlog from
    /// `RunDetail`.
    pub fn subscribe(&self, scope: &str) -> broadcast::Receiver<StampedProgressEvent> {
        if let Some(tx) = self.scopes.get(scope) {
            return tx.subscribe();
        }
        let (tx, rx) = broadcast::channel(SCOPE_CHANNEL_CAPACITY);
        self.scopes.insert(scope.to_string(), tx);
        rx
    }

    /// Publish one event to a scope. No-op when no channel exists yet AND there
    /// is no subscriber — we do NOT create a channel just to publish into the
    /// void, so idle runs leave no entries. When a channel exists but has zero
    /// live receivers, `send` returns `Err` (no receivers); we drop the entry
    /// so the map does not grow unbounded across runs.
    pub fn publish(&self, scope: &str, event: ProgressEvent) {
        let Some(tx) = self.scopes.get(scope) else {
            return;
        };
        // The stamp is taken HERE, still inside the emitter's own call, and
        // never by whoever pulls the event out of the ring — see
        // [`StampedProgressEvent`].
        let stamped = StampedProgressEvent {
            at_ms: now_ms(),
            event,
        };
        if tx.send(stamped).is_err() {
            // No live receivers — drop the sender lock before removing to avoid
            // a deadlock on the DashMap shard.
            drop(tx);
            self.scopes.remove(scope);
        }
    }

    /// Number of live subscribers for a scope (0 when the scope is unknown).
    /// Used by the wire layer to decide whether a run is being watched.
    pub fn subscriber_count(&self, scope: &str) -> usize {
        self.scopes
            .get(scope)
            .map(|tx| tx.receiver_count())
            .unwrap_or(0)
    }
}

impl Default for ProgressBroker {
    fn default() -> Self {
        Self::new()
    }
}

/// Production `ProgressSink` backed by a `ProgressBroker`. The executor holds it
/// as `Arc<dyn ProgressSink>`; `emit` forwards to `broker.publish`. A single
/// sink serves every run — the scope arrives per `emit` call, so the sink is
/// stateless apart from the shared broker handle.
pub struct BrokerProgressSink {
    broker: Arc<ProgressBroker>,
}

impl BrokerProgressSink {
    pub fn new(broker: Arc<ProgressBroker>) -> Self {
        Self { broker }
    }
}

impl ProgressSink for BrokerProgressSink {
    fn emit(&self, scope: &str, event: ProgressEvent) {
        self.broker.publish(scope, event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subscriber_receives_published_events() {
        let broker = Arc::new(ProgressBroker::new());
        let mut rx = broker.subscribe("session-1");
        let sink = BrokerProgressSink::new(broker.clone());
        sink.emit(
            "session-1",
            ProgressEvent::NodeStarted {
                node_id: "n1".into(),
                node_type: "llm".into(),
            },
        );
        let got = rx.recv().await.expect("event delivered");
        assert_eq!(
            got.event,
            ProgressEvent::NodeStarted {
                node_id: "n1".into(),
                node_type: "llm".into(),
            }
        );
    }

    /// The seam of §2.7: the stamp belongs to the emitter's call, not to the
    /// reader's. Both events are drained only AFTER both were published, so a
    /// stamp taken at receipt would put them within a millisecond of each other
    /// and every duration read out of the log would be a difference of receipt
    /// times.
    #[tokio::test]
    async fn the_stamp_is_taken_at_publish_not_at_receive() {
        let broker = Arc::new(ProgressBroker::new());
        let mut rx = broker.subscribe("stamp");
        let sink = BrokerProgressSink::new(broker.clone());
        sink.emit(
            "stamp",
            ProgressEvent::NodeStarted {
                node_id: "n1".into(),
                node_type: "llm".into(),
            },
        );
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        sink.emit(
            "stamp",
            ProgressEvent::FirstToken {
                node_id: "n1".into(),
            },
        );

        let start = rx.recv().await.expect("start delivered");
        let token = rx.recv().await.expect("token delivered");
        let gap = token.at_ms - start.at_ms;
        assert!(
            (249..400).contains(&gap),
            "the two emissions were 250 ms apart, the stamps say {gap} ms"
        );
    }

    /// §2.5 — a scope key is client-supplied on the foreground path, so a
    /// second principal must not be able to re-point a live scope. Neither
    /// principal's stamp survives the collision: the newcomer's events are not
    /// stored under the first user's actor either.
    #[test]
    fn a_foreign_principal_cannot_repoint_a_live_scope() {
        use crate::flow_engine::dispatcher::FlowActor;

        let bind = |broker: &ProgressBroker, run: &str, user: &str| {
            let mut meta = FlowRequestMeta::new(run, FlowOrigin::Chat, FlowActor::user(user));
            meta.org_id = Some(format!("org-of-{user}"));
            meta.session_id = Some("shared-session".into());
            broker.bind_run_provenance(
                "shared-session",
                RunProvenance::from_meta(&meta, RunDescriptor::resolved("m", "llm", "text")),
            );
        };

        let broker = ProgressBroker::new();
        bind(&broker, "run-a1", "user-a");
        let bound = broker
            .run_provenance("shared-session")
            .expect("the first principal owns the scope");
        assert_eq!(bound.run_id, "run-a1");

        // The same principal's next run under the same scope rebinds normally.
        bind(&broker, "run-a2", "user-a");
        assert_eq!(
            broker
                .run_provenance("shared-session")
                .expect("still bound")
                .run_id,
            "run-a2"
        );

        // A different principal guessing the session id contests it.
        bind(&broker, "run-b1", "user-b");
        assert!(
            broker.run_provenance("shared-session").is_none(),
            "a contested scope must stamp nothing at all"
        );

        // And it stays contested until its subscriber releases the scope — the
        // attacker cannot claim it by binding once more.
        bind(&broker, "run-b2", "user-b");
        assert!(broker.run_provenance("shared-session").is_none());
        broker.release_run_provenance("shared-session");
        bind(&broker, "run-a3", "user-a");
        assert_eq!(
            broker
                .run_provenance("shared-session")
                .expect("a released scope binds again")
                .run_id,
            "run-a3"
        );
    }

    #[tokio::test]
    async fn publish_without_subscriber_is_noop() {
        let broker = Arc::new(ProgressBroker::new());
        let sink = BrokerProgressSink::new(broker.clone());
        // No subscriber for this scope — publish must not create an entry.
        sink.emit(
            "orphan",
            ProgressEvent::Compaction {
                node_id: "n".into(),
            },
        );
        assert_eq!(broker.subscriber_count("orphan"), 0);
    }

    #[tokio::test]
    async fn dropped_subscriber_prunes_scope_on_next_publish() {
        let broker = Arc::new(ProgressBroker::new());
        let rx = broker.subscribe("session-2");
        drop(rx);
        // First publish after the last receiver dropped removes the entry.
        broker.publish(
            "session-2",
            ProgressEvent::NodeFinished {
                node_id: "n1".into(),
                status: "ok".into(),
            },
        );
        assert_eq!(broker.subscriber_count("session-2"), 0);
    }

    #[test]
    fn session_owner_binds_first_principal_and_rejects_hijack() {
        let broker = ProgressBroker::new();
        assert_eq!(broker.session_owner("s1"), None);
        broker.bind_session_owner("s1", "user-a");
        assert_eq!(broker.session_owner("s1").as_deref(), Some("user-a"));
        // A second principal cannot steal an in-flight session scope.
        broker.bind_session_owner("s1", "user-b");
        assert_eq!(broker.session_owner("s1").as_deref(), Some("user-a"));
        // Empty inputs never create a binding.
        broker.bind_session_owner("", "user-c");
        broker.bind_session_owner("s2", "");
        assert_eq!(broker.session_owner("s2"), None);
    }

    #[tokio::test]
    async fn isolates_scopes() {
        let broker = Arc::new(ProgressBroker::new());
        let mut rx_a = broker.subscribe("a");
        let mut rx_b = broker.subscribe("b");
        broker.publish(
            "a",
            ProgressEvent::ToolCallStarted {
                call_id: "test-call".into(),
                name: "search".into(),
            },
        );
        let got = rx_a.recv().await.expect("scope a delivered");
        assert_eq!(
            got.event,
            ProgressEvent::ToolCallStarted {
                call_id: "test-call".into(),
                name: "search".into()
            }
        );
        // Scope b saw nothing.
        assert!(rx_b.try_recv().is_err());
    }
}
