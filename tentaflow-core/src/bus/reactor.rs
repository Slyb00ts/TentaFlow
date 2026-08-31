// ===== File: bus/reactor.rs — BusReactor: turns fetched TentaBus records into
// REACTIVE flow runs (PLAN.md §6.3, M3a). Structurally mirrors `agents::
// subagent_reactor` (cached subscription registry, `ReactorFlowDispatch`
// reuse, `AbortOnDropHandle` lifetime, `init_global`), but the EVENT SOURCE is
// different by necessity: `subagent_reactor` subscribes once to a push-based
// `broadcast::Receiver`, while `ConsumerHandle::fetch` is a blocking, pull-
// based long-poll with no equivalent notification hook (see its own doc).
// So instead of one shared subscriber, this reactor runs one dedicated
// background poll loop PER (flow_id, subscription) — a supervisor task
// (`reconcile`) periodically rescans the active flow set for `bus_consume`
// entries and starts/stops per-subscription tasks to match.
//
// No dedup window (unlike `subagent_reactor`): a broadcast ring can replay or
// lag-skip the SAME event, but a poll cycle only ever fetches records AFTER
// its handle's cursor, so there is no duplicate-event source to guard against
// here — redelivery on failure is the intended at-least-once behavior, not a
// bug this reactor works around.
//
// Fresh `ConsumerHandle` every poll cycle, never reused across cycles: `fetch`
// advances its handle's in-memory `next_offset` independently of `commit()`
// (`commit` only ever clamps `next_offset` upward), so a long-lived handle
// that skips `commit()` after a failed batch would never re-fetch those
// records from the SAME handle again. `open_consumer` seeds `next_offset` from
// the durable `committed_offset` at open time, so reopening every cycle is
// what makes "do not commit on failure" actually redeliver next time.
//
// `commit_mode` interaction (`bus::groups::CommitMode`): `AutoAfterSuccess`
// (the default) is this reactor committing after a successful dispatch, or
// per `on_error` on a failed one — exactly what its doc names this call site
// for. `AtMostOnce` already commits INSIDE `fetch()` itself before dispatch
// even runs, so this reactor never commits again for it (doing so risks
// "offset behind what's already committed", which `commit()` rejects
// outright) — data is gone on failure regardless of `on_error`; only the DLQ
// side-effect of `Dlq` still runs (harmlessly degrading to
// `DlqOutcome::SentToDlqOffsetMismatch`, per `note_delivery_failure`'s own
// mismatch guard) for operator visibility. `Explicit` has no commit path in
// M3a at all — no flow node exposes one yet (that lands with M3b's host
// functions) — so an `Explicit` subscription is redelivered every cycle
// regardless of dispatch outcome until that mechanism exists; a flow author
// choosing `Explicit` today is opting into that.
//
// `batch_size` vs `fetch`'s byte bound: `ConsumerHandle::fetch` has no
// message-count parameter, only `max_bytes` — round-robining every subscribed
// partition up to that bound. This reactor requests a generous fixed
// `FETCH_MAX_BYTES` and truncates the result to `config.batch_size` itself.
// Records beyond the truncation point are simply never committed — the
// SAME "fresh handle every cycle" property above means they are naturally
// re-fetched (redundantly, not incorrectly) on the next cycle.
//
// Attribution: a bus-triggered run has no natural principal (unlike
// `on_subagent_complete`, which inherits a finished run's own stamp) — there
// is no human/service identity behind "a message landed on a topic". Per the
// "never fabricate a principal" rule, this reactor states one explicitly:
// `AgentPrincipal::new(None, Some(config.org_id), FlowOrigin::Bus,
// FlowActor::system())`. `org_id` comes from `bus_consume`'s own config field
// (not PLAN §6.3's literal list — `DbFlow` carries no org scope, so a
// subscription cannot otherwise address an org-scoped topic; see
// `bus_consume.rs::ConsumeConfig::from_config`'s doc). =====

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

use crate::agents::{AgentPrincipal, FlowDispatcherReactorDispatch, ReactorFlowDispatch};
use crate::bus::dlq::DlqReason;
use crate::bus::groups::CommitMode;
use crate::bus::{
    BusCallContext, BusService, BusServiceError, ConsumerConfig, ConsumerHandle, FetchedRecordMeta,
    TopicPartition,
};
use crate::db::{repository, DbPool};
use crate::flow_engine::dispatcher::{FlowActor, FlowOrigin};
use crate::flow_engine::envelope::{FlowEnvelope, FlowValue};
use crate::flow_engine::node_adapters::bus_consume::{self, ConsumeConfig, OnError};
use crate::flow_engine::types::FlowDefinition;

/// Byte bound passed to `ConsumerHandle::fetch` on every poll cycle. Not
/// `config.batch_size` (a message count `fetch` has no parameter for) — the
/// module doc above explains the truncate-after-fetch strategy this enables.
const FETCH_MAX_BYTES: usize = 8 * 1024 * 1024;

/// How often the supervisor rescans the active flow set for `bus_consume`
/// subscriptions. Flow saves are rare, so this trades a little detection
/// latency for a cheap, simple poll instead of a push-based flow-change
/// notification this codebase has no hook for.
const RECONCILE_INTERVAL: Duration = Duration::from_secs(5);

/// Backoff before a subscription loop retries after `open_consumer`/`fetch`
/// itself errors (bus not initialized yet, transient auth/DB hiccup) — avoids
/// a tight error loop hammering the same failure.
const ERROR_BACKOFF: Duration = Duration::from_secs(2);

/// One `bus_consume` node's subscription: its flow id and parsed config.
#[derive(Debug, Clone)]
struct Subscription {
    flow_id: String,
    config: ConsumeConfig,
}

/// Cached set of subscriptions, rebuilt only when the active-flow signature
/// changes — same shape and reason as `subagent_reactor::SubscriptionRegistry`.
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

    fn current(&mut self) -> &[Subscription] {
        match self.compute_signature_and_flows() {
            Ok((sig, flows)) => {
                if self.signature.as_deref() != Some(sig.as_str()) {
                    self.subs = build_subscriptions(&flows);
                    self.signature = Some(sig);
                }
            }
            Err(e) => {
                tracing::warn!("bus reactor: flow scan failed: {e}");
            }
        }
        &self.subs
    }

    fn compute_signature_and_flows(&self) -> anyhow::Result<(String, Vec<(String, String)>)> {
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

/// Parses each active flow's JSON and keeps those whose entry node is
/// `bus_consume` with a well-formed config. A malformed config (missing
/// `topic`/`group`, unknown `commit_mode`/`on_error`) or unparseable flow is
/// skipped with a warning — it cannot have passed save validation, so this
/// only guards hand-edited rows.
fn build_subscriptions(flows: &[(String, String)]) -> Vec<Subscription> {
    let mut subs = Vec::new();
    for (flow_id, flow_json) in flows {
        let def: FlowDefinition = match serde_json::from_str(flow_json) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("bus reactor: flow '{flow_id}' does not parse: {e}");
                continue;
            }
        };
        let Some(entry) = def
            .nodes
            .iter()
            .find(|n| n.node_type == bus_consume::NODE_TYPE)
        else {
            continue;
        };
        match ConsumeConfig::from_config(&entry.config) {
            Ok(config) => subs.push(Subscription {
                flow_id: flow_id.clone(),
                config,
            }),
            Err(e) => {
                tracing::warn!(
                    "bus reactor: flow '{flow_id}' has a malformed bus_consume config, \
                     skipping: {e}"
                );
            }
        }
    }
    subs
}

/// A running per-subscription poll task, keyed by flow id. `config` is kept
/// alongside the handle so `reconcile` can detect an edit (a save that
/// changes topic/group/batch_size/…) and restart rather than leave a stale
/// task running against the old subscription.
struct RunningSubscription {
    config: ConsumeConfig,
    _handle: AbortOnDropHandle<()>,
}

/// Owns the cached registry and the set of currently running per-subscription
/// tasks. `reconcile` is the only thing that mutates `running` — called by
/// `start`'s supervisor loop on a fixed interval.
struct BusReactor {
    registry: SubscriptionRegistry,
    dispatch: Arc<dyn ReactorFlowDispatch>,
    running: HashMap<String, RunningSubscription>,
}

impl BusReactor {
    fn new(db: DbPool, dispatch: Arc<dyn ReactorFlowDispatch>) -> Self {
        Self {
            registry: SubscriptionRegistry::new(db),
            dispatch,
            running: HashMap::new(),
        }
    }

    /// Starts newly-added or config-changed subscriptions, stops removed or
    /// changed ones (dropping a `RunningSubscription` aborts its task via
    /// `AbortOnDropHandle`) — a changed config gets a fresh task rather than
    /// an in-place update since any in-flight retry/backoff state is tied to
    /// the old subscription anyway.
    fn reconcile(&mut self) {
        let wanted: HashMap<String, ConsumeConfig> = self
            .registry
            .current()
            .iter()
            .map(|s| (s.flow_id.clone(), s.config.clone()))
            .collect();
        self.running
            .retain(|flow_id, running| wanted.get(flow_id) == Some(&running.config));
        for (flow_id, config) in wanted {
            if self.running.contains_key(&flow_id) {
                continue;
            }
            let handle = spawn_subscription(flow_id.clone(), config.clone(), self.dispatch.clone());
            self.running.insert(
                flow_id,
                RunningSubscription {
                    config,
                    _handle: handle,
                },
            );
        }
    }
}

fn spawn_subscription(
    flow_id: String,
    config: ConsumeConfig,
    dispatch: Arc<dyn ReactorFlowDispatch>,
) -> AbortOnDropHandle<()> {
    let handle = tokio::spawn(subscription_loop(flow_id, config, dispatch));
    AbortOnDropHandle::new(handle)
}

/// Groups records by `(topic, partition)`, keeping the highest offset seen for
/// each — the argument `ConsumerHandle::commit` wants is the NEXT offset to
/// read from, hence `+ 1`.
fn end_offsets(records: &[FetchedRecordMeta]) -> Vec<(TopicPartition, u64)> {
    let mut max: HashMap<(String, u32), u64> = HashMap::new();
    for r in records {
        let key = (r.topic.clone(), r.partition);
        max.entry(key)
            .and_modify(|o| *o = (*o).max(r.offset))
            .or_insert(r.offset);
    }
    max.into_iter()
        .map(|((topic, partition), offset)| (TopicPartition { topic, partition }, offset + 1))
        .collect()
}

/// Builds the seed envelope a `bus_consume` entry emits (PLAN §6.3): a single
/// decoded record's JSON for `batch_size == 1` (matching `bus_consume`'s own
/// `active_output_ports`, which activates "message" for a non-array Json
/// payload), a JSON array for a larger batch (activating "batch"). `meta`
/// carries delivery metadata directly as scalar keys — ports in this flow
/// engine only gate which OUTGOING EDGES are followed, they do not select
/// sub-values of the envelope, so downstream nodes read `meta.bus_*` off the
/// same envelope regardless of which port routed them there.
fn build_envelope(
    config: &ConsumeConfig,
    records: &[FetchedRecordMeta],
    values: &[serde_json::Value],
) -> FlowEnvelope {
    let mut env = FlowEnvelope::empty();
    env.meta
        .insert("bus_topic".into(), serde_json::json!(config.topic));
    env.meta
        .insert("bus_group".into(), serde_json::json!(config.group));
    if let ([record], [value]) = (records, values) {
        env.payload = FlowValue::Json(value.clone());
        env.meta
            .insert("bus_partition".into(), serde_json::json!(record.partition));
        env.meta
            .insert("bus_offset".into(), serde_json::json!(record.offset));
        env.meta.insert(
            "bus_timestamp_ms".into(),
            serde_json::json!(record.timestamp_ms),
        );
        if let Some(key) = &record.key {
            env.meta.insert(
                "bus_key".into(),
                serde_json::json!(String::from_utf8_lossy(key)),
            );
        }
    } else {
        env.payload = FlowValue::Json(serde_json::Value::Array(values.to_vec()));
        env.meta
            .insert("bus_batch_count".into(), serde_json::json!(records.len()));
    }
    env
}

/// Runs `handle.commit` off the async runtime (BLOCKING, see `ConsumerHandle::
/// fetch`'s own doc). Consumes `handle` — the caller's poll cycle is over
/// either way, so the next cycle opens a fresh one.
async fn commit_offsets(handle: ConsumerHandle, offsets: Vec<(TopicPartition, u64)>) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || handle.commit(&offsets))
        .await
        .map_err(|e| anyhow::anyhow!("commit task panicked: {e}"))?
        .map_err(|e| anyhow::anyhow!("commit failed: {e}"))
}

/// Runs `BusService::note_delivery_failure` off the async runtime for one
/// record.
async fn note_failure(
    svc: Arc<BusService>,
    bctx: BusCallContext,
    group: String,
    record: FetchedRecordMeta,
    reason: DlqReason,
    message: String,
) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || {
        svc.note_delivery_failure(
            &bctx,
            &group,
            &record.topic,
            record.partition,
            record.offset,
            &record,
            reason,
            &message,
        )
    })
    .await
    .map_err(|e| anyhow::anyhow!("note_delivery_failure task panicked: {e}"))?
    .map_err(|e| anyhow::anyhow!("note_delivery_failure failed: {e}"))?;
    Ok(())
}

/// Whether a poll cycle should keep going after `on_error` handling.
enum AfterFailure {
    Continue,
    Halt,
}

/// Routes a failed batch (JSON decode failure OR a failed flow dispatch)
/// through `config.on_error` (`bus_consume::OnError`'s own doc). Consumes
/// `handle` — every branch either commits it, hands it to a blocking call, or
/// simply drops it; the next cycle opens a fresh one regardless.
async fn handle_batch_failure(
    svc: &Arc<BusService>,
    bctx: &BusCallContext,
    handle: ConsumerHandle,
    flow_id: &str,
    config: &ConsumeConfig,
    records: &[FetchedRecordMeta],
    reason: DlqReason,
    message: &str,
) -> AfterFailure {
    match config.on_error {
        OnError::Halt => {
            tracing::warn!(
                "bus reactor: flow '{flow_id}' halted ({reason:?}): {message} — an operator \
                 must fix and republish the flow to resume"
            );
            drop(handle);
            AfterFailure::Halt
        }
        OnError::Skip => {
            tracing::warn!(
                "bus reactor: flow '{flow_id}' skipping {} record(s) ({reason:?}): {message}",
                records.len()
            );
            if config.commit_mode == CommitMode::AutoAfterSuccess {
                let offsets = end_offsets(records);
                if let Err(e) = commit_offsets(handle, offsets).await {
                    tracing::warn!("bus reactor: flow '{flow_id}' skip-commit failed: {e}");
                }
            } else {
                drop(handle);
            }
            AfterFailure::Continue
        }
        OnError::Dlq => {
            drop(handle);
            for record in records {
                let outcome = note_failure(
                    svc.clone(),
                    bctx.clone(),
                    config.group.clone(),
                    record.clone(),
                    reason,
                    message.to_string(),
                )
                .await;
                if let Err(e) = outcome {
                    tracing::warn!(
                        "bus reactor: flow '{flow_id}' note_delivery_failure failed for \
                         offset {}: {e}",
                        record.offset
                    );
                }
            }
            AfterFailure::Continue
        }
    }
}

/// One full poll cycle for a subscription: opens a fresh `ConsumerHandle`,
/// fetches, and either dispatches the decoded batch reactively or routes a
/// failure through `on_error` — see this module's doc for the full design.
/// Takes `svc` as a parameter (rather than reaching for `bus::global()`
/// itself) so it is directly testable against a locally-constructed
/// `BusService`, without touching the process-wide singleton every other test
/// in this crate's binary also shares.
async fn run_cycle(
    svc: &Arc<BusService>,
    flow_id: &str,
    config: &ConsumeConfig,
    dispatch: &Arc<dyn ReactorFlowDispatch>,
) -> AfterFailure {
    let bctx = BusCallContext {
        org_id: config.org_id.clone(),
        actor: None,
        correlation_id: None,
        origin: FlowOrigin::Bus.as_str().to_string(),
    };

    let (handle, mut records) = {
        let svc = svc.clone();
        let open_bctx = bctx.clone();
        let group = config.group.clone();
        let topic = config.topic.clone();
        let commit_mode = config.commit_mode;
        let max_wait_ms = config.max_wait_ms;
        let joined = tokio::task::spawn_blocking(move || {
            let handle = svc.open_consumer(&open_bctx, &group, &[topic], ConsumerConfig { commit_mode })?;
            let batch = handle.fetch(FETCH_MAX_BYTES, max_wait_ms)?;
            Ok::<_, BusServiceError>((handle, batch))
        })
        .await;
        match joined {
            Ok(Ok((handle, batch))) => (handle, batch.records),
            Ok(Err(e)) => {
                tracing::warn!("bus reactor: flow '{flow_id}' open_consumer/fetch failed: {e}");
                tokio::time::sleep(ERROR_BACKOFF).await;
                return AfterFailure::Continue;
            }
            Err(e) => {
                tracing::error!("bus reactor: flow '{flow_id}' open/fetch task panicked: {e}");
                tokio::time::sleep(ERROR_BACKOFF).await;
                return AfterFailure::Continue;
            }
        }
    };

    if records.is_empty() {
        // `fetch` already long-polled up to `max_wait_ms` internally.
        return AfterFailure::Continue;
    }
    records.truncate(config.batch_size as usize);

    let decoded: Result<Vec<serde_json::Value>, serde_json::Error> = records
        .iter()
        .map(|r| serde_json::from_slice::<serde_json::Value>(&r.payload))
        .collect();
    let values = match decoded {
        Ok(values) => values,
        Err(e) => {
            let msg = format!("payload is not valid JSON: {e}");
            return handle_batch_failure(
                svc,
                &bctx,
                handle,
                flow_id,
                config,
                &records,
                DlqReason::SchemaViolation,
                &msg,
            )
            .await;
        }
    };

    let envelope = build_envelope(config, &records, &values);
    let principal = AgentPrincipal::new(
        None,
        Some(config.org_id.clone()),
        FlowOrigin::Bus,
        FlowActor::system(),
    );

    match dispatch.dispatch(flow_id.to_string(), envelope, principal).await {
        Ok(()) => {
            if config.commit_mode == CommitMode::AutoAfterSuccess {
                let offsets = end_offsets(&records);
                if let Err(e) = commit_offsets(handle, offsets).await {
                    tracing::warn!(
                        "bus reactor: flow '{flow_id}' commit after successful dispatch failed: {e}"
                    );
                }
            }
            // `AtMostOnce` already committed inside `fetch()`; `Explicit`
            // has no commit path in M3a — see this module's doc.
            AfterFailure::Continue
        }
        Err(e) => {
            let msg = e.to_string();
            handle_batch_failure(
                svc,
                &bctx,
                handle,
                flow_id,
                config,
                &records,
                DlqReason::ConsumerError,
                &msg,
            )
            .await
        }
    }
}

/// One subscription's background poll loop: waits for the bus service to be
/// live, then runs `run_cycle` until it signals a halt.
async fn subscription_loop(flow_id: String, config: ConsumeConfig, dispatch: Arc<dyn ReactorFlowDispatch>) {
    loop {
        let Some(svc) = crate::bus::global() else {
            tracing::warn!("bus reactor: flow '{flow_id}' bus service not initialized yet");
            tokio::time::sleep(ERROR_BACKOFF).await;
            continue;
        };
        if let AfterFailure::Halt = run_cycle(&svc, &flow_id, &config, &dispatch).await {
            break;
        }
    }
}

/// Starts the reactor's supervisor task: reconciles per-subscription tasks
/// against the active flow set every `RECONCILE_INTERVAL` until `cancel`
/// fires. Dropping the returned handle (or cancelling) aborts the supervisor
/// AND, via each `RunningSubscription`'s own `AbortOnDropHandle`, every
/// per-subscription task it started.
pub fn start(db: DbPool, dispatch: Arc<dyn ReactorFlowDispatch>, cancel: CancellationToken) -> AbortOnDropHandle<()> {
    let mut reactor = BusReactor::new(db, dispatch);
    let handle = tokio::spawn(async move {
        loop {
            reactor.reconcile();
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(RECONCILE_INTERVAL) => {}
            }
        }
    });
    AbortOnDropHandle::new(handle)
}

/// Process-global handle keeping the reactor alive for the process lifetime —
/// same shape as `subagent_reactor::init_global`.
static GLOBAL: std::sync::OnceLock<AbortOnDropHandle<()>> = std::sync::OnceLock::new();

/// Installs the process-global bus reactor backed by the live FlowDispatcher.
/// Idempotent: a second call is ignored. Call once at startup after the
/// FlowDispatcher exists (the bus service itself may still initialize later —
/// each subscription loop tolerates `bus::global()` being `None` yet, per
/// `subscription_loop`'s first check).
pub fn init_global(db: DbPool, dispatcher: &Arc<crate::flow_engine::dispatcher::FlowDispatcher>) {
    if GLOBAL.get().is_some() {
        tracing::warn!("bus reactor: init_global called twice — ignoring second call");
        return;
    }
    let dispatch: Arc<dyn ReactorFlowDispatch> = Arc::new(FlowDispatcherReactorDispatch::new(dispatcher));
    let handle = start(db, dispatch, CancellationToken::new());
    let _ = GLOBAL.set(handle);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::topics::TopicOptions;
    use crate::bus::{BusAction, BusAuthorizer, BusInitConfig, PublishBatch, PublishRecord};
    use crate::db::migrations;
    use crate::db::models::FlowParams;
    use async_trait::async_trait;
    use bytes::Bytes;
    use serde_json::json;
    use std::sync::Mutex;
    use tokio::sync::mpsc;

    /// Permissive `BusAuthorizer` for tests — mirrors `bus::mod`'s own
    /// private test-only `AllowAllAuthorizer`, duplicated here since that one
    /// is private to `bus::mod`'s test module.
    struct AllowAllAuthorizer;
    impl BusAuthorizer for AllowAllAuthorizer {
        fn authorize(&self, _ctx: &BusCallContext, _action: BusAction, _topic: &str) -> Result<(), BusServiceError> {
            Ok(())
        }
        fn authorize_group(
            &self,
            _ctx: &BusCallContext,
            _action: BusAction,
            _topic: &str,
            _group: &str,
        ) -> Result<(), BusServiceError> {
            Ok(())
        }
        fn generation(&self) -> u64 {
            0
        }
    }

    fn db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        migrations::run(&conn).expect("migrations");
        Arc::new(crate::db::Db::from_connection(conn))
    }

    /// Inserts an active flow whose entry is `bus_consume` with the given
    /// config and returns its id.
    fn seed_consume_flow(pool: &DbPool, name: &str, config: serde_json::Value) -> String {
        let flow_json = json!({
            "nodes": [
                {"id": "c", "type": bus_consume::NODE_TYPE, "config": config},
                {"id": "o", "type": "output", "config": {}}
            ],
            "edges": [
                {"from": "c", "to": "o", "from_port": "message", "to_port": "text"}
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

    fn record(topic: &str, partition: u32, offset: u64, payload: &str) -> FetchedRecordMeta {
        FetchedRecordMeta {
            topic: topic.to_string(),
            partition,
            offset,
            timestamp_ms: 0,
            key: None,
            headers: vec![],
            payload: Bytes::from(payload.to_string()),
            schema_id: 0,
        }
    }

    #[test]
    fn build_subscriptions_parses_bus_consume_entry() {
        let pool = db();
        let flow_id = seed_consume_flow(
            &pool,
            "react",
            json!({"topic": "orders.raw", "group": "g1"}),
        );
        let flows = vec![(
            flow_id.clone(),
            repository::list_flows(&pool, 0, 10)
                .unwrap()
                .into_iter()
                .find(|f| f.id == flow_id)
                .unwrap()
                .flow_json,
        )];
        let subs = build_subscriptions(&flows);
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].flow_id, flow_id);
        assert_eq!(subs[0].config.topic, "orders.raw");
        assert_eq!(subs[0].config.group, "g1");
    }

    #[test]
    fn build_subscriptions_skips_flow_without_bus_consume_entry() {
        let flow_json = json!({
            "nodes": [{"id": "t", "type": "trigger", "config": {}}],
            "edges": []
        })
        .to_string();
        let subs = build_subscriptions(&[("f1".to_string(), flow_json)]);
        assert!(subs.is_empty());
    }

    #[test]
    fn build_subscriptions_skips_malformed_config() {
        let flow_json = json!({
            "nodes": [{"id": "c", "type": bus_consume::NODE_TYPE, "config": {}}],
            "edges": []
        })
        .to_string();
        let subs = build_subscriptions(&[("f1".to_string(), flow_json)]);
        assert!(subs.is_empty(), "missing topic/group must be skipped, not panic");
    }

    #[test]
    fn end_offsets_takes_max_plus_one_per_partition() {
        let records = vec![
            record("t", 0, 5, "a"),
            record("t", 0, 7, "b"),
            record("t", 1, 2, "c"),
        ];
        let mut offsets = end_offsets(&records);
        offsets.sort_by_key(|(tp, _)| tp.partition);
        assert_eq!(
            offsets,
            vec![
                (TopicPartition { topic: "t".into(), partition: 0 }, 8),
                (TopicPartition { topic: "t".into(), partition: 1 }, 3),
            ]
        );
    }

    #[test]
    fn build_envelope_single_record_uses_message_shape() {
        let config = ConsumeConfig::from_config(&json!({"topic": "t", "group": "g"})).unwrap();
        let records = vec![record("t", 0, 3, "x")];
        let values = vec![json!({"id": 1})];
        let env = build_envelope(&config, &records, &values);
        match &env.payload {
            FlowValue::Json(v) => assert_eq!(v, &json!({"id": 1})),
            other => panic!("expected Json payload, got {other:?}"),
        }
        assert_eq!(env.meta.get("bus_topic"), Some(&json!("t")));
        assert_eq!(env.meta.get("bus_partition"), Some(&json!(0)));
        assert_eq!(env.meta.get("bus_offset"), Some(&json!(3)));
        assert!(env.meta.get("bus_batch_count").is_none());
    }

    #[test]
    fn build_envelope_multi_record_uses_batch_shape() {
        let config = ConsumeConfig::from_config(&json!({"topic": "t", "group": "g"})).unwrap();
        let records = vec![record("t", 0, 1, "x"), record("t", 0, 2, "y")];
        let values = vec![json!({"a": 1}), json!({"a": 2})];
        let env = build_envelope(&config, &records, &values);
        match &env.payload {
            FlowValue::Json(v) => assert_eq!(v, &json!([{"a": 1}, {"a": 2}])),
            other => panic!("expected Json payload, got {other:?}"),
        }
        assert_eq!(env.meta.get("bus_batch_count"), Some(&json!(2)));
        assert!(env.meta.get("bus_offset").is_none());
    }

    #[tokio::test]
    async fn reconcile_starts_and_stops_tasks_as_flows_change() {
        let pool = db();
        struct NoopDispatch;
        #[async_trait]
        impl ReactorFlowDispatch for NoopDispatch {
            async fn dispatch(
                &self,
                _flow_id: String,
                _initial: FlowEnvelope,
                _principal: AgentPrincipal,
            ) -> anyhow::Result<()> {
                Ok(())
            }
        }
        let mut reactor = BusReactor::new(pool.clone(), Arc::new(NoopDispatch));

        reactor.reconcile();
        assert!(reactor.running.is_empty(), "no subscriptions yet");

        let flow_id = seed_consume_flow(&pool, "react", json!({"topic": "t", "group": "g"}));
        reactor.reconcile();
        assert_eq!(reactor.running.len(), 1);
        assert!(reactor.running.contains_key(&flow_id));

        // Re-reconciling with nothing changed must not restart the task
        // (same config), so we only assert the set is stable, not identity.
        reactor.reconcile();
        assert_eq!(reactor.running.len(), 1);

        let existing = repository::get_flow(&pool, &flow_id)
            .expect("read flow")
            .expect("flow exists");
        repository::update_flow(
            &pool,
            &flow_id,
            existing.version,
            &FlowParams {
                name: "react",
                description: None,
                is_default: false,
                service_type: Some("chat"),
                flow_json: &existing.flow_json,
                status: "draft",
                published_model_name: None,
                actor_user_id: None,
            },
        )
        .expect("deactivate");
        reactor.reconcile();
        assert!(
            reactor.running.is_empty(),
            "deactivated flow's subscription must be stopped"
        );
    }

    /// Spy `ReactorFlowDispatch` shared by the `run_cycle` integration tests
    /// below: records `(flow_id, payload_json)` and lets a test hand back a
    /// scripted result per call (a `Mutex<VecDeque<...>>` of results, success
    /// by default once exhausted).
    struct ScriptedDispatch {
        calls: Mutex<Vec<(String, serde_json::Value)>>,
        fail_next: std::sync::atomic::AtomicBool,
    }

    impl ScriptedDispatch {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                fail_next: std::sync::atomic::AtomicBool::new(false),
            }
        }
    }

    #[async_trait]
    impl ReactorFlowDispatch for ScriptedDispatch {
        async fn dispatch(
            &self,
            flow_id: String,
            initial: FlowEnvelope,
            _principal: AgentPrincipal,
        ) -> anyhow::Result<()> {
            let payload = match &initial.payload {
                FlowValue::Json(v) => v.clone(),
                other => serde_json::json!(format!("{other:?}")),
            };
            self.calls.lock().unwrap().push((flow_id, payload));
            if self.fail_next.swap(false, std::sync::atomic::Ordering::SeqCst) {
                anyhow::bail!("scripted dispatch failure");
            }
            Ok(())
        }
    }

    /// A fresh `BusService` in its own temp dir, with a permissive
    /// authorizer — never touches the process-wide `bus::global()` singleton
    /// every other test in this crate's binary also shares (see
    /// `dispatch::bus::tests::bus_fixture`'s own doc for why that singleton
    /// is a shared-state hazard across test modules).
    fn test_bus_service() -> (tempfile::TempDir, Arc<BusService>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
        crate::db::repository::bus_test_support::create_bus_tables(&db).expect("bus tables");
        let svc = Arc::new(
            BusService::new(BusInitConfig {
                bus_dir: dir.path().join("bus"),
                db,
                authorizer: Arc::new(AllowAllAuthorizer),
                retention_interval: None,
                dedup_expected_rate_per_sec: 10_000,
                partition_handle_lru: None,
                publish_ack_timeout: crate::bus::DEFAULT_PUBLISH_ACK_TIMEOUT,
            })
            .expect("bus service"),
        );
        (dir, svc)
    }

    fn test_ctx(org: &str) -> BusCallContext {
        BusCallContext {
            org_id: org.to_string(),
            actor: Some("tester".to_string()),
            correlation_id: None,
            origin: "test".to_string(),
        }
    }

    /// Runs a blocking `BusService`/`ConsumerHandle` call off the async
    /// runtime — every direct call in this test file must go through this
    /// (see this module's own BLOCKING doc, and `bus::mod`'s: `publish`
    /// panics if called directly from a Tokio worker thread via its internal
    /// `blocking_recv`).
    async fn blocking<T, F>(f: F) -> T
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        tokio::task::spawn_blocking(f).await.expect("blocking task panicked")
    }

    async fn create_topic(svc: &Arc<BusService>, ctx: &BusCallContext, topic: &str, opts: TopicOptions) {
        let svc = svc.clone();
        let ctx = ctx.clone();
        let topic = topic.to_string();
        blocking(move || svc.create_topic(&ctx, &topic, opts).map(|_| ()))
            .await
            .expect("create topic");
    }

    async fn publish_one(svc: &Arc<BusService>, ctx: &BusCallContext, topic: &str, payload: &str) {
        let svc = svc.clone();
        let ctx = ctx.clone();
        let topic = topic.to_string();
        let payload = payload.to_string();
        blocking(move || {
            svc.publish(
                &ctx,
                &topic,
                PublishBatch {
                    partition: Some(0),
                    producer: None,
                    records: vec![PublishRecord {
                        key: None,
                        headers: vec![],
                        payload: Bytes::from(payload),
                        timestamp_ms: 0,
                        schema_id: 0,
                    }],
                },
            )
        })
        .await
        .expect("publish");
    }

    /// Opens a fresh consumer and fetches once, both off the async runtime.
    async fn fetch_once(svc: &Arc<BusService>, ctx: &BusCallContext, group: &str, topic: &str) -> Vec<FetchedRecordMeta> {
        let svc = svc.clone();
        let ctx = ctx.clone();
        let group = group.to_string();
        let topic = topic.to_string();
        blocking(move || {
            let handle = svc
                .open_consumer(
                    &ctx,
                    &group,
                    &[topic],
                    ConsumerConfig {
                        commit_mode: CommitMode::AutoAfterSuccess,
                    },
                )
                .expect("open consumer");
            handle.fetch(1024, 10).expect("fetch").records
        })
        .await
    }

    /// A successful dispatch commits the offset, so a second cycle sees
    /// nothing new even though the record is technically still on the topic.
    #[tokio::test]
    async fn run_cycle_dispatches_and_commits_on_success() {
        let (_dir, svc) = test_bus_service();
        let ctx = test_ctx("org-default");
        create_topic(&svc, &ctx, "orders.raw", TopicOptions::default()).await;
        publish_one(&svc, &ctx, "orders.raw", r#"{"id":1}"#).await;

        let scripted = Arc::new(ScriptedDispatch::new());
        let dispatch: Arc<dyn ReactorFlowDispatch> = scripted.clone();
        let config = ConsumeConfig::from_config(&json!({
            "topic": "orders.raw", "group": "g1", "max_wait_ms": 20
        }))
        .unwrap();

        let after = run_cycle(&svc, "flow-1", &config, &dispatch).await;
        assert!(matches!(after, AfterFailure::Continue));
        {
            let calls = scripted.calls.lock().unwrap();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0], ("flow-1".to_string(), json!({"id": 1})));
        }

        // Second cycle: nothing new (short max_wait_ms keeps this fast).
        let after2 = run_cycle(&svc, "flow-1", &config, &dispatch).await;
        assert!(matches!(after2, AfterFailure::Continue));
        assert_eq!(scripted.calls.lock().unwrap().len(), 1, "no redelivery after commit");
    }

    /// `on_error: halt` stops the cycle immediately after a failed dispatch,
    /// without committing — the same record is still there for a next cycle
    /// (which the caller, per `subscription_loop`, never issues once halted).
    #[tokio::test]
    async fn run_cycle_halts_without_committing_on_error_halt() {
        let (_dir, svc) = test_bus_service();
        let ctx = test_ctx("org-default");
        create_topic(&svc, &ctx, "orders.raw", TopicOptions::default()).await;
        publish_one(&svc, &ctx, "orders.raw", r#"{"id":1}"#).await;

        let scripted = Arc::new(ScriptedDispatch::new());
        scripted
            .fail_next
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let dispatch: Arc<dyn ReactorFlowDispatch> = scripted;
        let config = ConsumeConfig::from_config(&json!({
            "topic": "orders.raw", "group": "g1", "max_wait_ms": 20, "on_error": "halt"
        }))
        .unwrap();

        let after = run_cycle(&svc, "flow-1", &config, &dispatch).await;
        assert!(matches!(after, AfterFailure::Halt));

        // Uncommitted: a fresh handle still sees the record.
        let records = fetch_once(&svc, &ctx, "g1", "orders.raw").await;
        assert_eq!(records.len(), 1, "halted cycle must not commit");
    }

    /// `on_error: dlq` (the default) sends the failed record to the topic's
    /// DLQ and, since nothing else has committed past it yet, advances the
    /// source offset too (`note_delivery_failure`'s own `committed == offset`
    /// auto-advance) — so the source is not redelivered either. The topic is
    /// created with `max_delivery_attempts: 1` so a single failure escalates
    /// straight to the DLQ instead of retrying.
    #[tokio::test]
    async fn run_cycle_dlq_on_error_sends_to_dlq_and_advances_offset() {
        let (_dir, svc) = test_bus_service();
        let ctx = test_ctx("org-default");
        create_topic(
            &svc,
            &ctx,
            "orders.raw",
            TopicOptions {
                max_delivery_attempts: Some(1),
                ..Default::default()
            },
        )
        .await;
        publish_one(&svc, &ctx, "orders.raw", r#"{"id":1}"#).await;

        let scripted = Arc::new(ScriptedDispatch::new());
        scripted
            .fail_next
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let dispatch: Arc<dyn ReactorFlowDispatch> = scripted;
        let config = ConsumeConfig::from_config(&json!({
            "topic": "orders.raw", "group": "g1", "max_wait_ms": 20, "on_error": "dlq"
        }))
        .unwrap();

        let after = run_cycle(&svc, "flow-1", &config, &dispatch).await;
        assert!(matches!(after, AfterFailure::Continue));

        let records = fetch_once(&svc, &ctx, "g1", "orders.raw").await;
        assert!(
            records.is_empty(),
            "DLQ escalation must advance the source offset past the poison record"
        );

        let dlq_topic = crate::bus::dlq::dlq_topic_name("orders.raw");
        let dlq_records = fetch_once(&svc, &ctx, "dlq-reader", &dlq_topic).await;
        assert_eq!(dlq_records.len(), 1, "one record landed in the DLQ");
    }
}
