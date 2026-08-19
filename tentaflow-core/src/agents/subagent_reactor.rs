// ===== File: agents/subagent_reactor.rs — SubagentReactor: turns sub-agent
// completion events into REACTIVE flow runs (Harness §3.6 phase 4b).
//
// A long-lived tokio task subscribes once to the AgentRunManager's process-
// global child-completion broadcast (`ChildFinishedEvent`). For each settled run
// it finds every active flow whose entry node is `on_subagent_complete` and
// whose filter (`agent_id` / `match_status`) matches the finished child, then
// dispatches that flow with the child's result as the seed payload.
//
// Why a dedicated broadcast and not the per-scope ProgressBroker: the broker's
// `publish` is a no-op without a live per-scope subscriber, and scopes are run
// ids the reactor cannot enumerate ahead of time. The always-on broadcast ring
// lets the reactor subscribe exactly once.
//
// Why a cached subscription registry: scanning + JSON-parsing every active flow
// on each event is wasteful when the flow set is stable. The registry is rebuilt
// lazily only when the flow set's signature (id + version per flow) changes, so
// a burst of completions reuses one parse.
//
// Idempotency: the manager fires exactly one terminal event per run, and the
// reactor de-dups by run id over a bounded recent-window so a `RecvError::Lagged`
// induced re-derivation or any double-emit cannot double-dispatch the same run.
// The result is read from the durable `agent_runs.result` (not the event), so a
// dispatch always carries the persisted answer. =====

use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

use crate::db::{repository, DbPool};
use crate::flow_engine::envelope::{FlowEnvelope, FlowValue};
use crate::flow_engine::node_adapters::CompletionFilter;
use crate::flow_engine::types::FlowDefinition;

use super::principal::AgentPrincipal;
use super::run_manager::ChildFinishedEvent;

/// How many recently-dispatched run ids the reactor remembers for de-dup. Bounds
/// memory while covering any realistic re-emit / lag window.
const DEDUP_WINDOW: usize = 4096;

/// Dispatches one reactive flow run. Abstracted so the reactor is unit-testable
/// with a spy, without a live `FlowDispatcher`. The production impl
/// (`FlowDispatcherReactorDispatch`) drives the flow through the dispatcher's
/// background path.
#[async_trait]
pub trait ReactorFlowDispatch: Send + Sync {
    /// Runs `flow_id` with `initial` as the seeded entry envelope, under the
    /// finished child's principal so the reactive run is attributed to the same
    /// caller. Best-effort: a dispatch error is logged by the reactor, never
    /// propagated to the finishing run.
    async fn dispatch(
        &self,
        flow_id: String,
        initial: FlowEnvelope,
        principal: AgentPrincipal,
    ) -> anyhow::Result<()>;
}

/// Production dispatch: drives an event-driven flow through the FlowDispatcher's
/// background path (no foreground deadline; governed by its own flow). Holds a
/// `Weak` to the dispatcher to avoid an ownership cycle — a dropped dispatcher
/// (shutdown) surfaces as a dispatch error the reactor logs.
pub struct FlowDispatcherReactorDispatch {
    dispatcher: std::sync::Weak<crate::flow_engine::dispatcher::FlowDispatcher>,
}

impl FlowDispatcherReactorDispatch {
    pub fn new(dispatcher: &Arc<crate::flow_engine::dispatcher::FlowDispatcher>) -> Self {
        Self {
            dispatcher: Arc::downgrade(dispatcher),
        }
    }
}

#[async_trait]
impl ReactorFlowDispatch for FlowDispatcherReactorDispatch {
    async fn dispatch(
        &self,
        flow_id: String,
        initial: FlowEnvelope,
        principal: AgentPrincipal,
    ) -> anyhow::Result<()> {
        let dispatcher = self
            .dispatcher
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("flow dispatcher dropped (shutdown)"))?;
        let request_id = uuid::Uuid::new_v4().to_string();
        // §2.5 — a reactive run is not a fresh anonymous request: it continues
        // the finished child's work, so it carries that run's provenance.
        let mut meta = crate::flow_engine::dispatcher::FlowRequestMeta::new(
            request_id,
            principal.origin,
            principal.actor.clone(),
        );
        meta.user_id = principal.user_id.clone();
        meta.org_id = principal.org_id.clone();
        let outcome = dispatcher
            .dispatch_by_flow_id_background(flow_id, initial, meta)
            .await
            .map_err(|e| anyhow::anyhow!("reactive flow dispatch failed: {e}"))?;
        if let Some(err) = outcome.error {
            return Err(anyhow::anyhow!("reactive flow failed: {err}"));
        }
        Ok(())
    }
}

/// One flow subscribed to sub-agent completions: its id + parsed filter.
#[derive(Debug, Clone)]
struct Subscription {
    flow_id: String,
    filter: CompletionFilter,
}

/// Cached set of subscriptions, rebuilt only when the flow set changes. The
/// signature is the concatenation of (id, version) over active flows — any
/// add/remove/edit (version bump on save) invalidates it.
struct SubscriptionRegistry {
    db: DbPool,
    signature: Option<String>,
    subs: Vec<Subscription>,
}

impl SubscriptionRegistry {
    fn new(db: DbPool) -> Self {
        Self {
            db,
            signature: None,
            subs: Vec::new(),
        }
    }

    /// Returns the current subscriptions, rebuilding from the DB only when the
    /// active-flow signature changed since the last call.
    fn current(&mut self) -> &[Subscription] {
        match self.compute_signature_and_flows() {
            Ok((sig, flows)) => {
                if self.signature.as_deref() != Some(sig.as_str()) {
                    self.subs = build_subscriptions(&flows);
                    self.signature = Some(sig);
                }
            }
            Err(e) => {
                tracing::warn!("subagent reactor: flow scan failed: {e}");
                // Keep the previous cache rather than dropping subscriptions on a
                // transient DB hiccup.
            }
        }
        &self.subs
    }

    /// Lists active flows once, returning their signature and (id, flow_json)
    /// pairs. Only `active` flows can be dispatched, so inactive ones are
    /// excluded from both the signature and the parse set.
    fn compute_signature_and_flows(&self) -> anyhow::Result<(String, Vec<(String, String)>)> {
        // The flow set is small (dozens), so a single full list is cheap; paging
        // would only add round-trips. A high cap guards against pathological
        // growth without truncating realistic deployments.
        let flows = repository::list_flows(&self.db, 0, 10_000)?;
        let mut sig = String::new();
        let mut out = Vec::new();
        for f in flows {
            if f.status != "active" {
                continue;
            }
            sig.push_str(&f.id);
            sig.push(':');
            sig.push_str(&f.version.to_string());
            sig.push(';');
            out.push((f.id, f.flow_json));
        }
        Ok((sig, out))
    }
}

/// Parses each active flow's JSON and keeps those whose sole entry node is
/// `on_subagent_complete` with a well-formed filter. A malformed filter (no
/// agent_id and no match_status) or unparseable flow is skipped with a warning —
/// it cannot have passed save validation, so this only guards hand-edited rows.
fn build_subscriptions(flows: &[(String, String)]) -> Vec<Subscription> {
    let mut subs = Vec::new();
    for (flow_id, flow_json) in flows {
        let def: FlowDefinition = match serde_json::from_str(flow_json) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("subagent reactor: flow '{flow_id}' does not parse: {e}");
                continue;
            }
        };
        let Some(entry) = def.nodes.iter().find(|n| {
            n.node_type == crate::flow_engine::node_adapters::on_subagent_complete::NODE_TYPE
        }) else {
            continue;
        };
        match CompletionFilter::from_config(&entry.config) {
            Ok(filter) => subs.push(Subscription {
                flow_id: flow_id.clone(),
                filter,
            }),
            Err(e) => {
                tracing::warn!(
                    "subagent reactor: flow '{flow_id}' has a malformed on_subagent_complete \
                     filter, skipping: {e}"
                );
            }
        }
    }
    subs
}

/// Bounded FIFO de-dup of recently dispatched run ids.
struct DedupWindow {
    seen: std::collections::HashSet<String>,
    order: VecDeque<String>,
}

impl DedupWindow {
    fn new() -> Self {
        Self {
            seen: std::collections::HashSet::new(),
            order: VecDeque::new(),
        }
    }

    /// Records `run_id`; returns false when it was already seen (a duplicate).
    fn insert(&mut self, run_id: &str) -> bool {
        if self.seen.contains(run_id) {
            return false;
        }
        self.seen.insert(run_id.to_string());
        self.order.push_back(run_id.to_string());
        if self.order.len() > DEDUP_WINDOW {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
        true
    }
}

/// Long-lived reactor: owns the broadcast receiver, the cached subscription
/// registry and the de-dup window. Started by `start`, which returns an
/// `AbortOnDropHandle` so the owner controls its lifetime.
pub struct SubagentReactor {
    db: DbPool,
    dispatch: Arc<dyn ReactorFlowDispatch>,
    registry: SubscriptionRegistry,
    dedup: DedupWindow,
}

impl SubagentReactor {
    fn new(db: DbPool, dispatch: Arc<dyn ReactorFlowDispatch>) -> Self {
        Self {
            registry: SubscriptionRegistry::new(db.clone()),
            db,
            dispatch,
            dedup: DedupWindow::new(),
        }
    }

    /// Handles one completion event: matches active subscriptions, reads the
    /// child's persisted result, seeds the envelope and dispatches each matching
    /// flow. De-dups by run id first so a re-emit never double-dispatches.
    async fn handle(&mut self, event: ChildFinishedEvent) {
        // Match BEFORE recording the run id: a run that matches no subscription
        // must not consume a de-dup slot (so a later flow add could still react
        // to a re-derived event for the same run — though the manager fires once,
        // this keeps the window meaningful for matched runs only).
        let matching: Vec<String> = self
            .registry
            .current()
            .iter()
            .filter(|s| s.filter.matches(&event.agent_id, &event.status))
            .map(|s| s.flow_id.clone())
            .collect();
        if matching.is_empty() {
            return;
        }
        if !self.dedup.insert(&event.run_id) {
            return;
        }

        // The result is the durable persisted answer, not the event (the event
        // carries only ids/status). A run with no result (failure/cancel matched
        // by an explicit match_status) seeds an empty payload.
        let finished = repository::get_agent_run(&self.db, &event.run_id)
            .ok()
            .flatten();
        let result = finished.as_ref().and_then(|r| r.result.clone());
        // §2.5 — inherit the finished run's caller. The run row persists who and
        // which org, not the original entry point, so the reactive run reports
        // `agent` as its origin — which is what it is.
        let principal = AgentPrincipal::new(
            finished.as_ref().and_then(|r| r.user_id.clone()),
            finished.as_ref().and_then(|r| r.org_id.clone()),
        );

        for flow_id in matching {
            let initial = seed_envelope(&event, result.as_deref());
            if let Err(e) = self
                .dispatch
                .dispatch(flow_id.clone(), initial, principal.clone())
                .await
            {
                tracing::warn!(
                    "subagent reactor: dispatch of flow '{flow_id}' for run '{}' failed: {e}",
                    event.run_id
                );
            }
        }
    }
}

/// Builds the seed envelope a reactive flow's entry emits: payload = the child's
/// result text (empty when none), meta = child run id / status / agent id so the
/// flow can branch on them.
fn seed_envelope(event: &ChildFinishedEvent, result: Option<&str>) -> FlowEnvelope {
    let mut env = FlowEnvelope::empty();
    env.payload = FlowValue::Text(result.unwrap_or_default().to_string());
    env.meta
        .insert("child_run_id".into(), Value::String(event.run_id.clone()));
    env.meta
        .insert("child_status".into(), Value::String(event.status.clone()));
    env.meta
        .insert("agent_id".into(), Value::String(event.agent_id.clone()));
    env
}

/// Starts the reactor task. Subscribes to the manager's child-completion stream,
/// then loops handling events until `cancel` fires or the sender is dropped.
/// Returns an abort-on-drop handle so the owner ties the task to its lifetime.
pub fn start(
    db: DbPool,
    dispatch: Arc<dyn ReactorFlowDispatch>,
    mut events: broadcast::Receiver<ChildFinishedEvent>,
    cancel: CancellationToken,
) -> AbortOnDropHandle<()> {
    let mut reactor = SubagentReactor::new(db, dispatch);
    let handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                recv = events.recv() => match recv {
                    Ok(event) => reactor.handle(event).await,
                    // A slow reactor overran the ring — events were dropped.
                    // Reactive flows are best-effort, so log and keep going; the
                    // durable mailbox / agent_runs record is unaffected.
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("subagent reactor: lagged, dropped {n} completion event(s)");
                    }
                    // Sender gone (manager dropped) — nothing more will arrive.
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    });
    AbortOnDropHandle::new(handle)
}

/// Process-global handle keeping the reactor task alive for the process lifetime.
/// `init_global` installs it; dropping the handle (only on a second init attempt,
/// which is ignored) would abort the task.
static GLOBAL: std::sync::OnceLock<AbortOnDropHandle<()>> = std::sync::OnceLock::new();

/// Installs the process-global reactor backed by the live FlowDispatcher,
/// subscribed to the given manager's completion stream. Idempotent: a second
/// call is ignored (the first reactor keeps running). Call once at startup after
/// the AgentRunManager and FlowDispatcher exist.
pub fn init_global(
    db: DbPool,
    dispatcher: &Arc<crate::flow_engine::dispatcher::FlowDispatcher>,
    events: broadcast::Receiver<ChildFinishedEvent>,
) {
    if GLOBAL.get().is_some() {
        tracing::warn!("subagent reactor: init_global called twice — ignoring second call");
        return;
    }
    let dispatch: Arc<dyn ReactorFlowDispatch> =
        Arc::new(FlowDispatcherReactorDispatch::new(dispatcher));
    let handle = start(db, dispatch, events, CancellationToken::new());
    let _ = GLOBAL.set(handle);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations;
    use crate::db::models::{AgentParams, AgentRunStatusUpdate, FlowParams, NewAgentRun};
    use serde_json::json;
    use std::sync::Mutex;
    use std::time::Duration;
    use tokio::sync::mpsc;

    fn db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        migrations::run(&conn).expect("migrations");
        Arc::new(crate::db::Db::from_connection(conn))
    }

    /// Spy dispatch: records (flow_id, seed payload text) and signals each
    /// dispatch on an mpsc so the test can await it without sleeping.
    struct SpyDispatch {
        calls: Arc<Mutex<Vec<(String, String)>>>,
        tx: mpsc::UnboundedSender<()>,
    }

    #[async_trait]
    impl ReactorFlowDispatch for SpyDispatch {
        async fn dispatch(
            &self,
            flow_id: String,
            initial: FlowEnvelope,
            _principal: AgentPrincipal,
        ) -> anyhow::Result<()> {
            let payload = initial.payload.as_text().unwrap_or_default().to_string();
            self.calls.lock().unwrap().push((flow_id, payload));
            let _ = self.tx.send(());
            Ok(())
        }
    }

    fn seed_agent(pool: &DbPool, id: &str, name: &str) {
        repository::upsert_agent(
            pool,
            &AgentParams {
                id,
                name,
                display_name: None,
                description: "t",
                system_prompt: None,
                model: None,
                tools_json: "[]",
                skills_json: "{}",
                params_json: "{}",
                max_iterations: 5,
                timeout_secs: 600,
                max_subagents: 0,
                max_spawn_depth: 1,
                flow_id: None,
                routable: true,
                is_enabled: true,
                on_child_complete: "notify",
                allowed_agents_json: None,
                actor_user_id: None,
            },
        )
        .expect("seed agent");
    }

    /// Inserts an active flow whose entry is `on_subagent_complete` with the
    /// given filter config and returns its id.
    fn seed_event_flow(pool: &DbPool, name: &str, filter_cfg: Value) -> String {
        let flow_json = json!({
            "nodes": [
                {"id": "e", "type": "on_subagent_complete", "config": filter_cfg},
                {"id": "o", "type": "output", "config": {}}
            ],
            "edges": [
                {"from": "e", "to": "o", "from_port": "text", "to_port": "text"}
            ]
        })
        .to_string();
        repository::create_flow(
            pool,
            &FlowParams {
                name,
                description: None,
                is_default: false,
                service_type: Some("chat"),
                flow_json: &flow_json,
                status: "active",
                published_model_name: None,
                actor_user_id: None,
            },
        )
        .expect("create flow")
    }

    /// Creates a completed agent_runs row with a result and returns the event the
    /// manager would broadcast for it.
    fn finished_run(pool: &DbPool, agent_id: &str, result: &str) -> ChildFinishedEvent {
        let run_id = uuid::Uuid::new_v4().to_string();
        repository::create_agent_run(
            pool,
            &NewAgentRun {
                id: &run_id,
                agent_id,
                parent_run_id: None,
                flow_execution_id: None,
                user_id: Some("u1"),
                org_id: None,
                prompt: "p",
            },
        )
        .expect("create run");
        repository::update_agent_run_status(
            pool,
            &run_id,
            &AgentRunStatusUpdate {
                status: "completed",
                result: Some(result),
                exit_reason: Some("final_response"),
                set_finished: true,
                ..Default::default()
            },
        )
        .expect("complete run");
        ChildFinishedEvent {
            run_id,
            agent_id: agent_id.to_string(),
            status: "completed".to_string(),
        }
    }

    fn reactor(pool: &DbPool, spy: Arc<SpyDispatch>) -> SubagentReactor {
        SubagentReactor::new(pool.clone(), spy)
    }

    #[tokio::test]
    async fn dispatches_matching_flow_with_child_result() {
        let pool = db();
        seed_agent(&pool, "a1", "worker");
        let flow_id = seed_event_flow(&pool, "react", json!({"agent_id": "a1"}));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (tx, _rx) = mpsc::unbounded_channel();
        let spy = Arc::new(SpyDispatch {
            calls: calls.clone(),
            tx,
        });
        let mut r = reactor(&pool, spy);

        let event = finished_run(&pool, "a1", "the answer");
        r.handle(event).await;

        let got = calls.lock().unwrap().clone();
        assert_eq!(got.len(), 1, "exactly one dispatch");
        assert_eq!(got[0].0, flow_id);
        assert_eq!(got[0].1, "the answer", "seed payload is the child result");
    }

    #[tokio::test]
    async fn does_not_dispatch_on_non_matching_agent() {
        let pool = db();
        seed_agent(&pool, "a1", "worker");
        seed_agent(&pool, "a2", "other");
        seed_event_flow(&pool, "react", json!({"agent_id": "a1"}));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (tx, _rx) = mpsc::unbounded_channel();
        let spy = Arc::new(SpyDispatch {
            calls: calls.clone(),
            tx,
        });
        let mut r = reactor(&pool, spy);

        // A completion of a DIFFERENT agent must not fire the a1-filtered flow.
        let event = finished_run(&pool, "a2", "nope");
        r.handle(event).await;
        assert!(
            calls.lock().unwrap().is_empty(),
            "non-matching agent dispatched"
        );
    }

    #[tokio::test]
    async fn does_not_dispatch_on_non_matching_status() {
        let pool = db();
        seed_agent(&pool, "a1", "worker");
        // Flow only reacts to failures.
        seed_event_flow(
            &pool,
            "react",
            json!({"agent_id": "a1", "match_status": "failed"}),
        );
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (tx, _rx) = mpsc::unbounded_channel();
        let spy = Arc::new(SpyDispatch {
            calls: calls.clone(),
            tx,
        });
        let mut r = reactor(&pool, spy);

        // A COMPLETED run does not match a failed-only filter.
        let event = finished_run(&pool, "a1", "ok");
        r.handle(event).await;
        assert!(
            calls.lock().unwrap().is_empty(),
            "status mismatch dispatched"
        );
    }

    #[tokio::test]
    async fn idempotent_same_event_dispatches_once() {
        let pool = db();
        seed_agent(&pool, "a1", "worker");
        seed_event_flow(&pool, "react", json!({"agent_id": "a1"}));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (tx, _rx) = mpsc::unbounded_channel();
        let spy = Arc::new(SpyDispatch {
            calls: calls.clone(),
            tx,
        });
        let mut r = reactor(&pool, spy);

        let event = finished_run(&pool, "a1", "once");
        r.handle(event.clone()).await;
        r.handle(event).await; // same run id again
        assert_eq!(
            calls.lock().unwrap().len(),
            1,
            "duplicate event re-dispatched"
        );
    }

    /// End-to-end through the broadcast channel + the spawned task: a
    /// ChildFinishedEvent sent on the ring drives one dispatch.
    #[tokio::test]
    async fn task_dispatches_event_from_broadcast() {
        let pool = db();
        seed_agent(&pool, "a1", "worker");
        let flow_id = seed_event_flow(&pool, "react", json!({"agent_id": "a1"}));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let spy = Arc::new(SpyDispatch {
            calls: calls.clone(),
            tx,
        });
        let (ev_tx, ev_rx) = broadcast::channel(16);
        let cancel = CancellationToken::new();
        let _handle = start(pool.clone(), spy, ev_rx, cancel.clone());

        let event = finished_run(&pool, "a1", "streamed");
        ev_tx.send(event).expect("send event");

        // Await the spy's signal rather than sleeping.
        tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("dispatch within bound")
            .expect("spy signalled");
        let got = calls.lock().unwrap().clone();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, flow_id);
        assert_eq!(got[0].1, "streamed");
        cancel.cancel();
    }
}
