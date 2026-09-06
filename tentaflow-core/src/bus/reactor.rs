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
// `bus_consume.rs::ConsumeConfig::from_config`'s doc).
//
// Multi-instance (plan-app-platform §3.5): `ConsumeConfig::instance_id`
// (REQUIRED, no fallback) names which running `BusService` a subscription
// reads from. `subscription_loop` resolves it via `bus::instance(&config.
// instance_id)`, never `bus::global()` — the single-instance shim derives its
// answer from "exactly one instance is running", so with instance A the only
// one up, a subscription configured for a DIFFERENT (disabled/not-yet-
// started) instance B would silently start consuming A's records through
// `bus::global()`. `bus::instance()` looks up the SPECIFIC id instead, so a
// subscription for an instance that is not running gets `None` and backs off
// — it can never resolve to a different instance's engine. `reconcile` also
// stops (and refuses to start) a subscription whose instance is not enabled,
// via `dispatch::app_gate::instance_enabled`, so disabling an instance stops
// its flow consumers within one `RECONCILE_INTERVAL` even when no flow
// config changed. =====

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

use crate::agents::{AgentPrincipal, FlowDispatcherReactorDispatch, ReactorFlowDispatch};
use crate::bus::dlq::DlqReason;
use crate::bus::groups::CommitMode;
use crate::bus::instance::BusInstanceId;
use crate::bus::{
    BusCallContext, BusService, BusServiceError, ConsumerConfig, ConsumerHandle, FetchedRecordMeta,
    TopicPartition,
};
use crate::db::{repository, DbPool};
use crate::dispatch::app_gate;
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
/// `bus_consume` with a well-formed config. A malformed config (missing or
/// malformed `instance_id`, missing `topic`/`group`, unknown `commit_mode`/
/// `on_error`) or unparseable flow is skipped with a warning — it cannot have
/// passed save validation, so this only guards hand-edited rows. No special
/// case needed for `instance_id` specifically: `ConsumeConfig::from_config`
/// already returns `Err` for it exactly like any other required field, and
/// the `match` below already skips-and-warns on any `Err`.
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
    /// the old subscription anyway. Also stops (and refuses to start) a
    /// subscription whose `instance_id` is no longer enabled — an instance
    /// being disabled/uninstalled is not a flow-config change, so it would
    /// never otherwise be caught by the `wanted != running` comparison above;
    /// this makes disabling an instance stop its flow consumers within one
    /// `RECONCILE_INTERVAL` (plan-app-platform §3.5).
    fn reconcile(&mut self) {
        let db = self.registry.db.clone();
        let wanted: HashMap<String, ConsumeConfig> = self
            .registry
            .current()
            .iter()
            .map(|s| (s.flow_id.clone(), s.config.clone()))
            .collect();

        // Resolve each DISTINCT instance's enabled state ONCE per reconcile
        // call, not once per subscription: most subscriptions in a real
        // deployment share a handful of instance ids, so per-subscription
        // lookups ask the same `get_instance_of_package` question over and
        // over every `RECONCILE_INTERVAL`. Built fresh every call (no
        // cross-cycle cache) — a cached answer surviving between cycles
        // would delay noticing a disable past the one-`RECONCILE_INTERVAL`
        // guarantee §3.5 asks for.
        let mut enabled: HashMap<BusInstanceId, bool> = HashMap::new();
        for config in wanted.values() {
            enabled
                .entry(config.instance_id.clone())
                .or_insert_with(|| instance_enabled(&db, &config.instance_id));
        }
        for running in self.running.values() {
            enabled
                .entry(running.config.instance_id.clone())
                .or_insert_with(|| instance_enabled(&db, &running.config.instance_id));
        }

        self.running.retain(|flow_id, running| {
            if wanted.get(flow_id) != Some(&running.config) {
                return false;
            }
            if !enabled[&running.config.instance_id] {
                tracing::info!(
                    "bus reactor: flow '{flow_id}' instance '{}' is no longer enabled, \
                     stopping its subscription",
                    running.config.instance_id
                );
                return false;
            }
            true
        });
        for (flow_id, config) in wanted {
            if self.running.contains_key(&flow_id) {
                continue;
            }
            if !enabled[&config.instance_id] {
                tracing::debug!(
                    "bus reactor: flow '{flow_id}' instance '{}' is not enabled, not starting \
                     its subscription",
                    config.instance_id
                );
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

/// `dispatch::app_gate::instance_enabled` scoped to the TentaBus package —
/// the small wrapper keeps `reconcile` from repeating `BusInstanceId::
/// PACKAGE_ID` at both call sites.
fn instance_enabled(db: &DbPool, id: &BusInstanceId) -> bool {
    app_gate::instance_enabled(db, BusInstanceId::PACKAGE_ID, id.as_str())
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
async fn commit_offsets(
    handle: ConsumerHandle,
    offsets: Vec<(TopicPartition, u64)>,
) -> anyhow::Result<()> {
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
/// Takes `svc` as a parameter (rather than resolving `config.instance_id`
/// itself, which `subscription_loop` does before calling this) so it is
/// directly testable against a locally-constructed `BusService`, without
/// touching the process-wide instance registry every other test in this
/// crate's binary also shares.
async fn run_cycle(
    svc: &Arc<BusService>,
    flow_id: &str,
    config: &ConsumeConfig,
    dispatch: &Arc<dyn ReactorFlowDispatch>,
) -> AfterFailure {
    let bctx = BusCallContext {
        instance_id: svc.typed_instance_id(),
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
            let handle =
                svc.open_consumer(&open_bctx, &group, &[topic], ConsumerConfig { commit_mode })?;
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

    match dispatch
        .dispatch(flow_id.to_string(), envelope, principal)
        .await
    {
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

/// One subscription's background poll loop: waits for `config.instance_id`'s
/// engine to be running, then runs `run_cycle` until it signals a halt.
///
/// Resolves through `bus::instance(&config.instance_id)` — the SPECIFIC
/// instance this subscription was configured for — never `bus::global()`.
/// `bus::global()` is a single-instance compatibility shim that answers from
/// "exactly one instance is running" regardless of which one; with instance A
/// the only engine up, it would silently hand a subscription configured for
/// a different instance B (disabled, uninstalled, or not started yet) A's
/// `BusService`, cross-wiring B's flow onto A's data. `bus::instance()` can
/// only ever return the named instance's own engine, so an unavailable
/// target instance backs off instead of leaking another instance's records.
async fn subscription_loop(
    flow_id: String,
    config: ConsumeConfig,
    dispatch: Arc<dyn ReactorFlowDispatch>,
) {
    loop {
        let Some(svc) = crate::bus::instance(&config.instance_id) else {
            tracing::warn!(
                "bus reactor: flow '{flow_id}' bus instance '{}' is not running (disabled, \
                 uninstalled, or not started yet) — backing off",
                config.instance_id
            );
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
pub fn start(
    db: DbPool,
    dispatch: Arc<dyn ReactorFlowDispatch>,
    cancel: CancellationToken,
) -> AbortOnDropHandle<()> {
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

/// plan-app-platform §9 (risk R8): every ACTIVE flow's `bus_consume`/
/// `bus_publish`/`bus_transform` node whose config has no `instance_id` at
/// all (empty/missing — a pre-migration flow, since `instance_id` is now
/// REQUIRED with no fallback). Pure and DB-free like `build_subscriptions`,
/// which it deliberately does not replace: `build_subscriptions` already
/// skips a malformed `bus_consume` with its OWN per-reconcile-cycle warning,
/// so re-scanning for it here would be duplicate noise on every
/// `RECONCILE_INTERVAL` — this instead runs ONCE (from `init_global`) to
/// name every affected flow up front, including `bus_publish`/`bus_transform`
/// nodes `build_subscriptions` never looks at (it only parses `bus_consume`
/// entries).
fn flows_missing_instance_id(flows: &[(String, String)]) -> Vec<(String, String, String)> {
    let mut missing = Vec::new();
    for (flow_id, flow_json) in flows {
        let Ok(def) = serde_json::from_str::<FlowDefinition>(flow_json) else {
            continue;
        };
        for node in &def.nodes {
            if !matches!(
                node.node_type.as_str(),
                "bus_consume" | "bus_publish" | "bus_transform"
            ) {
                continue;
            }
            let has_instance_id = node
                .config
                .get("instance_id")
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.trim().is_empty());
            if !has_instance_id {
                missing.push((flow_id.clone(), node.id.clone(), node.node_type.clone()));
            }
        }
    }
    missing
}

/// Fetches every active flow and logs `flows_missing_instance_id`'s findings,
/// one warning line per (flow, node) — the migration aid that makes
/// `instance_id` going from absent to required survivable: never auto-fills
/// a value (an instance has no sensible default, see `ConsumeConfig::
/// instance_id`'s doc), only names what an operator must fix.
fn log_flows_missing_instance_id(db: &DbPool) {
    let flows = match repository::list_flows(db, 0, 10_000) {
        Ok(flows) => flows,
        Err(e) => {
            tracing::warn!("bus reactor: startup instance_id scan: flow list failed: {e}");
            return;
        }
    };
    let active: Vec<(String, String)> = flows
        .into_iter()
        .filter(|f| f.status == "active")
        .map(|f| (f.id, f.flow_json))
        .collect();
    for (flow_id, node_id, node_type) in flows_missing_instance_id(&active) {
        tracing::warn!(
            "bus reactor: flow '{flow_id}' node '{node_id}' (type '{node_type}') has no \
             'instance_id' — this field is REQUIRED (plan-app-platform §3.3); the flow will \
             fail at save/consume/publish time until it is set to an installed TentaBus \
             instance"
        );
    }
}

/// Process-global handle keeping the reactor alive for the process lifetime —
/// same shape as `subagent_reactor::init_global`.
static GLOBAL: std::sync::OnceLock<AbortOnDropHandle<()>> = std::sync::OnceLock::new();

/// Installs the process-global bus reactor backed by the live FlowDispatcher.
/// Idempotent: a second call is ignored. Call once at startup after the
/// FlowDispatcher exists (an individual instance's engine may still
/// initialize later — each subscription loop tolerates `bus::instance(&config
/// .instance_id)` being `None` yet, per `subscription_loop`'s first check).
/// The reactor itself stays a single process-global — it is a *flow*
/// reactor, not a bus component, and multiplexes across instances by config
/// (plan-app-platform §3.5). Also runs the ONE-TIME `instance_id` migration
/// scan (`log_flows_missing_instance_id`) — `init_global`'s own
/// twice-is-a-no-op guard makes that scan run exactly once per process too.
pub fn init_global(db: DbPool, dispatcher: &Arc<crate::flow_engine::dispatcher::FlowDispatcher>) {
    if GLOBAL.get().is_some() {
        tracing::warn!("bus reactor: init_global called twice — ignoring second call");
        return;
    }
    log_flows_missing_instance_id(&db);
    let dispatch: Arc<dyn ReactorFlowDispatch> =
        Arc::new(FlowDispatcherReactorDispatch::new(dispatcher));
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
        fn authorize(
            &self,
            _ctx: &BusCallContext,
            _action: BusAction,
            _topic: &str,
        ) -> Result<(), BusServiceError> {
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

    /// Inserts a minimal `addons` row for a TentaBus instance, the shape
    /// `dispatch::app_gate::instance_enabled` reads (`addon_id`/`package_id`/
    /// `is_enabled`). Lighter than `app_gate::test_support::install_app_
    /// instance` (which needs a full `Arc<AppState>` + permission checker) —
    /// `reconcile`'s instance-enabled check only ever queries this table.
    fn install_bus_instance(pool: &DbPool, addon_id: &str, enabled: bool) {
        let conn = pool.write().expect("db lock");
        conn.execute(
            "INSERT INTO addons (addon_id, name, version, package_id, is_enabled) \
             VALUES (?1, ?1, '1.0.0', 'tentabus', ?2)",
            rusqlite::params![addon_id, enabled as i64],
        )
        .expect("seed bus instance row");
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
            json!({"instance_id": "tentabus-8b000001", "topic": "orders.raw", "group": "g1"}),
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
        assert_eq!(subs[0].config.instance_id.as_str(), "tentabus-8b000001");
        assert_eq!(subs[0].config.topic, "orders.raw");
        assert_eq!(subs[0].config.group, "g1");
    }

    /// plan-app-platform §3.3/§3.5: a `bus_consume` node with no `instance_id`
    /// at all (a pre-migration hand-edited row, since save-time validation
    /// now rejects this) is a malformed config like a missing `topic`/
    /// `group` — `build_subscriptions` skips it with a warning, it does not
    /// fall back to any default instance.
    #[test]
    fn build_subscriptions_skips_missing_instance_id() {
        let flow_json = json!({
            "nodes": [{
                "id": "c", "type": bus_consume::NODE_TYPE,
                "config": {"topic": "orders.raw", "group": "g1"}
            }],
            "edges": []
        })
        .to_string();
        let subs = build_subscriptions(&[("f1".to_string(), flow_json)]);
        assert!(
            subs.is_empty(),
            "missing instance_id must be skipped, not panic"
        );
    }

    /// plan-app-platform §9 (R8): the startup migration scan finds a
    /// `bus_consume` node with an empty `instance_id`, and — unlike
    /// `build_subscriptions` — also finds `bus_publish`/`bus_transform`
    /// nodes it never parses.
    #[test]
    fn flows_missing_instance_id_finds_every_bus_node_type() {
        let flow_json = json!({
            "nodes": [
                {"id": "c", "type": "bus_consume", "config": {"instance_id": "", "topic": "t", "group": "g"}},
                {"id": "p", "type": "bus_publish", "config": {"topic": "t"}},
                {"id": "x", "type": "bus_transform", "config": {"expression": "payload"}}
            ],
            "edges": []
        })
        .to_string();
        let missing = flows_missing_instance_id(&[("f1".to_string(), flow_json)]);
        let mut node_ids: Vec<&str> = missing
            .iter()
            .map(|(_, node_id, _)| node_id.as_str())
            .collect();
        node_ids.sort();
        assert_eq!(node_ids, vec!["c", "p", "x"]);
        assert!(missing.iter().all(|(flow_id, ..)| flow_id == "f1"));
    }

    #[test]
    fn flows_missing_instance_id_ignores_a_flow_with_valid_ids() {
        let flow_json = json!({
            "nodes": [
                {"id": "c", "type": "bus_consume", "config": {
                    "instance_id": "tentabus-aaaaaaaa", "topic": "t", "group": "g"
                }},
                {"id": "o", "type": "output", "config": {}}
            ],
            "edges": [{"from": "c", "to": "o", "from_port": "message", "to_port": "text"}]
        })
        .to_string();
        let missing = flows_missing_instance_id(&[("f1".to_string(), flow_json)]);
        assert!(missing.is_empty());
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
        assert!(
            subs.is_empty(),
            "missing topic/group must be skipped, not panic"
        );
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
                (
                    TopicPartition {
                        topic: "t".into(),
                        partition: 0
                    },
                    8
                ),
                (
                    TopicPartition {
                        topic: "t".into(),
                        partition: 1
                    },
                    3
                ),
            ]
        );
    }

    #[test]
    fn build_envelope_single_record_uses_message_shape() {
        let config = ConsumeConfig::from_config(
            &json!({"instance_id": "tentabus-8b000002", "topic": "t", "group": "g"}),
        )
        .unwrap();
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
        let config = ConsumeConfig::from_config(
            &json!({"instance_id": "tentabus-8b000002", "topic": "t", "group": "g"}),
        )
        .unwrap();
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

    /// No-op `ReactorFlowDispatch` for `reconcile` tests — they only care
    /// about which tasks are running, never about a dispatch outcome.
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

    #[tokio::test]
    async fn reconcile_starts_and_stops_tasks_as_flows_change() {
        let pool = db();
        install_bus_instance(&pool, "tentabus-8b000003", true);
        let mut reactor = BusReactor::new(pool.clone(), Arc::new(NoopDispatch));

        reactor.reconcile();
        assert!(reactor.running.is_empty(), "no subscriptions yet");

        let flow_id = seed_consume_flow(
            &pool,
            "react",
            json!({"instance_id": "tentabus-8b000003", "topic": "t", "group": "g"}),
        );
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

    /// plan-app-platform §3.5, finding F4's fix: a `bus_consume` subscription
    /// whose target instance is DISABLED (installed row present, `is_enabled
    /// = 0`) must never be started by `reconcile`, even though the flow
    /// config itself is perfectly well-formed.
    #[tokio::test]
    async fn reconcile_does_not_start_a_subscription_for_a_disabled_instance() {
        let pool = db();
        install_bus_instance(&pool, "tentabus-8b000004", false);
        let mut reactor = BusReactor::new(pool.clone(), Arc::new(NoopDispatch));

        seed_consume_flow(
            &pool,
            "react",
            json!({"instance_id": "tentabus-8b000004", "topic": "t", "group": "g"}),
        );
        reactor.reconcile();
        assert!(
            reactor.running.is_empty(),
            "a disabled instance's subscription must not start"
        );
    }

    /// plan-app-platform §3.5, finding F4's fix: disabling an instance while
    /// its subscription is already running must stop that subscription
    /// within one `reconcile` call, even though nothing about the FLOW
    /// changed (no version bump, no config edit).
    #[tokio::test]
    async fn reconcile_stops_a_running_subscription_when_its_instance_is_disabled() {
        let pool = db();
        install_bus_instance(&pool, "tentabus-8b000005", true);
        let mut reactor = BusReactor::new(pool.clone(), Arc::new(NoopDispatch));

        seed_consume_flow(
            &pool,
            "react",
            json!({"instance_id": "tentabus-8b000005", "topic": "t", "group": "g"}),
        );
        reactor.reconcile();
        assert_eq!(
            reactor.running.len(),
            1,
            "instance is enabled, subscription starts"
        );

        repository::set_addon_enabled(&pool, "tentabus-8b000005", false).expect("disable");
        reactor.reconcile();
        assert!(
            reactor.running.is_empty(),
            "disabling the instance must stop its subscription within one reconcile, \
             even though the flow itself did not change"
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
            if self
                .fail_next
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
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
        let local_conn = rusqlite::Connection::open_in_memory().expect("open local db");
        crate::bus::db::migrate(&local_conn).expect("migrate local db");
        let local_db: crate::db::DbPool = Arc::new(crate::db::Db::from_connection(local_conn));
        let svc = Arc::new(
            BusService::new(BusInitConfig {
                instance_id: crate::bus::instance::BusInstanceId::parse("tentabus-00000001")
                    .expect("valid instance id"),
                local_db,
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
            instance_id: crate::bus::instance::BusInstanceId::parse("tentabus-00000001")
                .expect("valid instance id"),
            org_id: org.to_string(),
            actor: Some("tester".to_string()),
            correlation_id: None,
            origin: "test".to_string(),
        }
    }

    fn test_ctx_for(instance_id: &BusInstanceId, org: &str) -> BusCallContext {
        BusCallContext {
            instance_id: instance_id.clone(),
            org_id: org.to_string(),
            actor: Some("tester".to_string()),
            correlation_id: None,
            origin: "test".to_string(),
        }
    }

    /// A `BusService` registered in the SHARED process-wide instance registry
    /// (`bus::init_instance`), unlike `test_bus_service` which builds one that
    /// only `run_cycle`'s tests ever see directly. Needed to test
    /// `subscription_loop`'s ACTUAL resolution path (`bus::instance(&config.
    /// instance_id)`), which `run_cycle`'s own tests bypass by taking `svc` as
    /// a parameter. Caller must `bus::stop_instance(&id)` when done — see
    /// `bus::mod`'s own registry tests for the same convention and the reason
    /// (a real process-global `static`, shared with every other `#[cfg(test)]`
    /// module in this crate's `--lib` binary).
    fn registry_bus_service(id: BusInstanceId) -> (tempfile::TempDir, Arc<BusService>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
        crate::db::repository::bus_test_support::create_bus_tables(&db).expect("bus tables");
        let local_conn = rusqlite::Connection::open_in_memory().expect("open local db");
        crate::bus::db::migrate(&local_conn).expect("migrate local db");
        let local_db: crate::db::DbPool = Arc::new(crate::db::Db::from_connection(local_conn));
        let svc = crate::bus::init_instance(BusInitConfig {
            instance_id: id,
            local_db,
            bus_dir: dir.path().join("bus"),
            db,
            authorizer: Arc::new(AllowAllAuthorizer),
            retention_interval: None,
            dedup_expected_rate_per_sec: 10_000,
            partition_handle_lru: None,
            publish_ack_timeout: crate::bus::DEFAULT_PUBLISH_ACK_TIMEOUT,
        })
        .expect("init registry instance");
        (dir, svc)
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
        tokio::task::spawn_blocking(f)
            .await
            .expect("blocking task panicked")
    }

    async fn create_topic(
        svc: &Arc<BusService>,
        ctx: &BusCallContext,
        topic: &str,
        opts: TopicOptions,
    ) {
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
    async fn fetch_once(
        svc: &Arc<BusService>,
        ctx: &BusCallContext,
        group: &str,
        topic: &str,
    ) -> Vec<FetchedRecordMeta> {
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
            "instance_id": "tentabus-00000001", "topic": "orders.raw", "group": "g1", "max_wait_ms": 20
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
        assert_eq!(
            scripted.calls.lock().unwrap().len(),
            1,
            "no redelivery after commit"
        );
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
            "instance_id": "tentabus-00000001", "topic": "orders.raw", "group": "g1", "max_wait_ms": 20, "on_error": "halt"
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
            "instance_id": "tentabus-00000001", "topic": "orders.raw", "group": "g1", "max_wait_ms": 20, "on_error": "dlq"
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

    /// THE regression test for finding F4 (plan-app-platform §3.5): a
    /// subscription configured for instance B must NEVER consume instance
    /// A's records, even when A is the ONLY instance currently running.
    ///
    /// This reproduces the actual bug exactly: before the fix,
    /// `subscription_loop` resolved `crate::bus::global()`, a single-instance
    /// compatibility shim that answers "the" bus service whenever exactly one
    /// instance is running — regardless of which instance the subscription
    /// was configured for. With A registered and B never started (disabled/
    /// uninstalled/not-yet-started), the OLD code would silently hand this
    /// B-targeted subscription A's `BusService` and dispatch A's data through
    /// a flow an operator scoped to B. The fix resolves `bus::instance(&config
    /// .instance_id)` — the SPECIFIC id — so an unavailable target instance
    /// backs off instead of ever touching a different instance's engine.
    #[tokio::test]
    async fn subscription_never_resolves_a_different_instance_than_configured() {
        let id_a = BusInstanceId::parse("tentabus-8b000006").expect("valid instance id");
        let id_b =
            BusInstanceId::parse("tentabus-8b000007").expect("valid instance id — never started");

        let (_dir_a, svc_a) = registry_bus_service(id_a.clone());
        let ctx_a = test_ctx_for(&id_a, "org-default");
        create_topic(&svc_a, &ctx_a, "orders.raw", TopicOptions::default()).await;
        publish_one(&svc_a, &ctx_a, "orders.raw", r#"{"source":"instance-a"}"#).await;

        let scripted = Arc::new(ScriptedDispatch::new());
        let dispatch: Arc<dyn ReactorFlowDispatch> = scripted.clone();
        // instance B is intentionally NEVER registered — A is the only
        // engine running on this node, exactly the condition that made the
        // old `bus::global()` shim answer for it.
        let config = ConsumeConfig::from_config(&json!({
            "instance_id": id_b.as_str(), "topic": "orders.raw", "group": "g1", "max_wait_ms": 20
        }))
        .unwrap();

        let _handle = spawn_subscription("flow-cross".to_string(), config, dispatch);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            scripted.calls.lock().unwrap().is_empty(),
            "a subscription configured for instance B must never consume instance A's \
             records, even when A is the only instance running"
        );

        crate::bus::stop_instance(&id_a);
    }
}
