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
// mismatch guard) for operator visibility. `Explicit` never reaches this
// reactor at all: no flow node commits offsets, so an `Explicit` subscription
// could only redeliver the same batch every cycle forever —
// `ConsumeConfig::from_config` rejects the value outright and
// `build_subscriptions` skips the flow with a warning rather than starting a
// silent redelivery loop. The flow-save path runs that same parse
// (`FlowDispatcher::validate_flow`), so a flow choosing it is refused before
// it can be stored — the skip here only catches rows that bypassed that
// check.
//
// `batch_size` vs `fetch`'s byte bound: `ConsumerHandle::fetch` has no
// message-count parameter, only `max_bytes` — round-robining every subscribed
// partition up to that bound. This reactor requests a generous fixed
// `FETCH_MAX_BYTES` and truncates the result to `config.batch_size` itself.
// Records beyond the truncation point are simply never committed — the
// SAME "fresh handle every cycle" property above means they are naturally
// re-fetched (redundantly, not incorrectly) on the next cycle.
//
// Attribution: a bus-triggered run has no natural human principal (unlike
// `on_subagent_complete`, which inherits a finished run's own stamp). It runs
// as the SERVICE ACCOUNT its `bus_consume` node names: the uid of a general API
// key (`ConsumeConfig::service_account`). Both the consumer's `BusCallContext`
// and the run's principal carry that key (`ActorKind::ApiKey`, no bound
// user), so the topic ACL and the data-hiding rules judge the flow exactly as
// they judge the key, and every audit row names `api_key:<uid>`. The
// reactor never holds the key's token, only its uid, and there is no fallback
// identity: a node without a service account, or whose key is missing,
// disabled, user-bound or lacks the read right, is REFUSED (see
// `ConsumerStatus`) and consumes nothing. The key consumes under its own
// `k:<uid>.<group>` namespace (`ConsumeConfig::effective_group`) — the only
// one the authorizer lets a key use. `org_id` comes from `bus_consume`'s own
// config field (`DbFlow` carries no org scope, so a subscription cannot
// otherwise address an org-scoped topic; see `bus_consume.rs::ConsumeConfig::
// from_config`'s doc).
//
// Status: every flow's consumer state (`Running`/`Waiting`/`Refused`/`Halted`,
// with a reason) is kept in memory per node (`StatusStore`) and exposed via
// `consumer_status`, so the UI can say why a flow reads nothing. A refusal is
// logged once when it starts and once when it recovers, never on every
// retry. The supervisor repeats its cheap pre-start check on every reconcile,
// so a fixed cause (a right granted, a key re-bound) resumes consumption
// within one `RECONCILE_INTERVAL`.
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

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;
use tracing::Instrument;

use crate::agents::{AgentPrincipal, FlowDispatcherReactorDispatch, ReactorFlowDispatch};
use crate::auth::actor::ActorKind;
use crate::bus::correlation;
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

/// Why a flow's consumer is not reading right now. Stable strings: the UI
/// maps them to localized text.
pub mod reason {
    pub const SERVICE_ACCOUNT_MISSING: &str = "service_account_missing";
    pub const SERVICE_ACCOUNT_NOT_FOUND: &str = "service_account_not_found";
    pub const SERVICE_ACCOUNT_DISABLED: &str = "service_account_disabled";
    pub const SERVICE_ACCOUNT_NOT_SERVICE_KEY: &str = "service_account_not_service_key";
    pub const SERVICE_ACCOUNT_LOOKUP_FAILED: &str = "service_account_lookup_failed";
    pub const READ_DENIED: &str = "read_denied";
    pub const NEEDS_TOPIC_WIDE_RULE: &str = "needs_topic_wide_rule";
    pub const MALFORMED_CONFIG: &str = "malformed_config";
    pub const INSTANCE_NOT_RUNNING: &str = "instance_not_running";
    pub const NOT_LEADER: &str = "not_leader";
    pub const TOPIC_NOT_FOUND: &str = "topic_not_found";
    pub const BUS_ERROR: &str = "bus_error";
}

/// Where a flow's consumer stands on this node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumerState {
    /// Fetching.
    Running,
    /// Not reading yet, and expected to on its own (instance not up, this
    /// node is not the partition leader, a transient bus error).
    Waiting,
    /// Will not read until something is fixed (config, key, rights); the
    /// supervisor re-checks every reconcile.
    Refused,
    /// `on_error = halt` stopped it; only a save of the flow resumes it.
    Halted,
}

impl ConsumerState {
    pub fn as_str(self) -> &'static str {
        match self {
            ConsumerState::Running => "running",
            ConsumerState::Waiting => "waiting",
            ConsumerState::Refused => "refused",
            ConsumerState::Halted => "halted",
        }
    }

    fn is_blocked(self) -> bool {
        !matches!(self, ConsumerState::Running)
    }
}

/// One flow's consumer status on this node. Per node on purpose: refusals
/// come from replicated data and read the same everywhere, only "not leader"
/// differs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsumerStatus {
    pub state: ConsumerState,
    pub reason: Option<&'static str>,
    pub detail: Option<String>,
    /// Unix seconds when `state`/`reason` last changed.
    pub since_epoch: u64,
}

/// What `StatusStore::set` found.
struct Transition {
    changed: bool,
    was_blocked: bool,
}

/// In-memory flow id -> `ConsumerStatus`. The process-wide one backs
/// `consumer_status`; tests use a private one so parallel tests never see each
/// other's flows.
#[derive(Default)]
pub struct StatusStore {
    inner: Mutex<HashMap<String, ConsumerStatus>>,
}

impl StatusStore {
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, ConsumerStatus>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn get(&self, flow_id: &str) -> Option<ConsumerStatus> {
        self.map().get(flow_id).cloned()
    }

    /// Stores the status; `since_epoch` moves only when state or reason
    /// changed, so repeating the same refusal is free.
    fn set(
        &self,
        flow_id: &str,
        state: ConsumerState,
        reason: Option<&'static str>,
        detail: Option<String>,
    ) -> Transition {
        let mut map = self.map();
        let previous = map.get(flow_id);
        let changed = previous.map_or(true, |p| p.state != state || p.reason != reason);
        let was_blocked = previous.is_some_and(|p| p.state.is_blocked());
        let since_epoch = match previous {
            Some(p) if !changed => p.since_epoch,
            _ => now_epoch_secs(),
        };
        map.insert(
            flow_id.to_string(),
            ConsumerStatus {
                state,
                reason,
                detail,
                since_epoch,
            },
        );
        Transition {
            changed,
            was_blocked,
        }
    }

    /// Drops the status of every flow not in `live`.
    fn retain_flows(&self, live: &HashSet<&str>) {
        self.map()
            .retain(|flow_id, _| live.contains(flow_id.as_str()));
    }

    /// Records a status and logs the transition: a refusal or wait when it
    /// starts, a recovery when a blocked flow reads again. A repeat of the
    /// same status logs nothing.
    fn record(
        &self,
        flow_id: &str,
        state: ConsumerState,
        reason: Option<&'static str>,
        detail: Option<String>,
    ) {
        let log_detail = detail.clone().unwrap_or_default();
        let t = self.set(flow_id, state, reason, detail);
        let reason_str = reason.unwrap_or("-");
        match state {
            ConsumerState::Refused if t.changed => tracing::warn!(
                "bus reactor: flow '{flow_id}' consumption refused ({reason_str}) {log_detail}"
            ),
            ConsumerState::Waiting if t.changed => tracing::warn!(
                "bus reactor: flow '{flow_id}' is waiting ({reason_str}) {log_detail}"
            ),
            ConsumerState::Running if t.was_blocked => {
                tracing::info!("bus reactor: flow '{flow_id}' consumes again")
            }
            _ => {}
        }
    }
}

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

static GLOBAL_STATUS: std::sync::OnceLock<Arc<StatusStore>> = std::sync::OnceLock::new();

fn global_status() -> Arc<StatusStore> {
    GLOBAL_STATUS.get_or_init(Arc::default).clone()
}

/// The consumer status of `flow_id` on this node, `None` when the reactor
/// knows nothing about it (not a `bus_consume` flow, not active, not scanned
/// yet).
pub fn consumer_status(flow_id: &str) -> Option<ConsumerStatus> {
    global_status().get(flow_id)
}

/// Why `service_account` cannot act as a flow's identity, from its key row,
/// or `None` when it can: it must exist, be a general key (no user or group
/// binding, which would bring that subject's rights back in) and be active.
fn service_account_refusal(db: &DbPool, service_account: &str) -> Option<&'static str> {
    use crate::services::bus_authorizer::{service_account_problem, ServiceAccountProblem};
    service_account_problem(db, service_account).map(|problem| match problem {
        ServiceAccountProblem::NotFound => reason::SERVICE_ACCOUNT_NOT_FOUND,
        ServiceAccountProblem::NotServiceKey => reason::SERVICE_ACCOUNT_NOT_SERVICE_KEY,
        ServiceAccountProblem::Disabled => reason::SERVICE_ACCOUNT_DISABLED,
        ServiceAccountProblem::LookupFailed => reason::SERVICE_ACCOUNT_LOOKUP_FAILED,
    })
}

/// A refusal and its optional detail.
type Refusal = (&'static str, Option<String>);

/// The bus call context of a flow consuming as its service account.
fn consumer_context(
    instance_id: BusInstanceId,
    config: &ConsumeConfig,
    service_account: &str,
) -> BusCallContext {
    BusCallContext {
        instance_id,
        org_id: config.org_id.clone(),
        actor: Some(service_account.to_string()),
        actor_kind: ActorKind::ApiKey,
        correlation_id: None,
        origin: FlowOrigin::Bus.as_str().to_string(),
    }
}

/// The cheap pre-start check `reconcile` repeats every cycle: DB reads only,
/// no consumer opened, no audit row. `svc` is the running engine of the
/// subscription's instance, when there is one; without it only the key itself
/// can be judged.
fn precheck(
    db: &DbPool,
    config: &ConsumeConfig,
    svc: Option<&Arc<BusService>>,
) -> Result<(), Refusal> {
    let Some(service_account) = config.service_account.as_deref() else {
        return Err((reason::SERVICE_ACCOUNT_MISSING, None));
    };
    if let Some(refusal) = service_account_refusal(db, service_account) {
        return Err((refusal, None));
    }
    let (Some(svc), Some(group)) = (svc, config.effective_group()) else {
        return Ok(());
    };
    let bctx = consumer_context(config.instance_id.clone(), config, service_account);
    match svc.check_consume_access(&bctx, &config.topic, &group) {
        Ok(()) => Ok(()),
        Err(BusServiceError::PermissionDenied { .. }) => Err((reason::READ_DENIED, None)),
        Err(BusServiceError::KeyNeedsTopicWideRule { .. }) => {
            Err((reason::NEEDS_TOPIC_WIDE_RULE, None))
        }
        Err(
            e @ (BusServiceError::InvalidArgument(_) | BusServiceError::InvalidTopicName { .. }),
        ) => Err((reason::MALFORMED_CONFIG, Some(e.to_string()))),
        // Anything else (instance still starting, not the leader) is the
        // loop's to classify once it runs.
        Err(_) => Ok(()),
    }
}

/// One `bus_consume` node's subscription: its flow id and parsed config.
#[derive(Debug, Clone)]
struct Subscription {
    flow_id: String,
    /// `flows.version` the config was read at: a save bumps it, which is how
    /// a halted subscription learns it may run again even when the config
    /// itself did not change.
    version: i64,
    config: ConsumeConfig,
}

/// An active flow as the registry reads it: id, `flow_json`, `flows.version`.
type ActiveFlow = (String, String, i64);

/// A flow whose `bus_consume` entry cannot become a subscription: its id and
/// why.
type MalformedFlow = (String, String);

/// What `build_subscriptions` found in the active flow set.
#[derive(Debug, Default)]
struct SubscriptionScan {
    subs: Vec<Subscription>,
    malformed: Vec<MalformedFlow>,
}

/// Cached set of subscriptions, rebuilt only when the active-flow signature
/// changes — same shape and reason as `subagent_reactor::SubscriptionRegistry`.
struct SubscriptionRegistry {
    db: DbPool,
    signature: Option<String>,
    scan: SubscriptionScan,
}

impl SubscriptionRegistry {
    fn new(db: DbPool) -> Self {
        Self {
            db,
            signature: None,
            scan: SubscriptionScan::default(),
        }
    }

    fn current(&mut self) -> &SubscriptionScan {
        match self.compute_signature_and_flows() {
            Ok((sig, flows)) => {
                if self.signature.as_deref() != Some(sig.as_str()) {
                    self.scan = build_subscriptions(&flows);
                    self.signature = Some(sig);
                }
            }
            Err(e) => {
                tracing::warn!("bus reactor: flow scan failed: {e}");
            }
        }
        &self.scan
    }

    fn compute_signature_and_flows(&self) -> anyhow::Result<(String, Vec<ActiveFlow>)> {
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
            out.push((f.id, f.flow_json, f.version));
        }
        Ok((sig, out))
    }
}

/// Parses each active flow's JSON and keeps those whose entry node is
/// `bus_consume` with a well-formed config. A malformed config (missing or
/// malformed `instance_id`, missing `topic`/`group`, a `group` outside the
/// broker's charset, a `service_account` that is no key uid, unknown
/// `commit_mode`/`on_error`) or unparseable flow is skipped and reported in
/// `SubscriptionScan::malformed`, which `reconcile` turns into a `Refused`
/// status. A node with no `service_account` at all is NOT malformed: it
/// parses, and `reconcile` refuses it with its own reason.
///
/// The four flow-save handlers run this SAME parse before storing anything
/// (`dispatch::handlers::validate_flow_json_str` ->
/// `FlowDispatcher::validate_flow` -> `validate_bus_consume_configs`), so a
/// flow that still reaches here malformed is one that bypassed that check —
/// e.g. a row written straight through `repository::create_flow` (integration
/// fixtures do exactly that), or a row stored before the check existed. For
/// those the status is the operator's signal: the flow stays
/// `status='active'` and simply never consumes. That is why the save path
/// rejects instead of relying on it.
///
/// No special case needed for `instance_id` specifically:
/// `ConsumeConfig::from_config` already returns `Err` for it exactly like any
/// other required field, and the `match` below already skips-and-warns on any
/// `Err`.
fn build_subscriptions(flows: &[ActiveFlow]) -> SubscriptionScan {
    let mut scan = SubscriptionScan::default();
    for (flow_id, flow_json, version) in flows {
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
            Ok(config) => scan.subs.push(Subscription {
                flow_id: flow_id.clone(),
                version: *version,
                config,
            }),
            Err(e) => scan.malformed.push((flow_id.clone(), e.to_string())),
        }
    }
    scan
}

/// A running per-subscription poll task, keyed by flow id. `config` and
/// `version` are kept alongside the handle so `reconcile` can detect an edit
/// or a re-save and restart rather than leave a stale task running against
/// the old subscription.
struct RunningSubscription {
    config: ConsumeConfig,
    version: i64,
    handle: AbortOnDropHandle<()>,
}

/// Owns the cached registry and the set of currently running per-subscription
/// tasks. `reconcile` is the only thing that mutates `running` — called by
/// `start`'s supervisor loop on a fixed interval.
struct BusReactor {
    registry: SubscriptionRegistry,
    dispatch: Arc<dyn ReactorFlowDispatch>,
    status: Arc<StatusStore>,
    running: HashMap<String, RunningSubscription>,
}

impl BusReactor {
    fn new(db: DbPool, dispatch: Arc<dyn ReactorFlowDispatch>) -> Self {
        Self::with_status(db, dispatch, global_status())
    }

    fn with_status(
        db: DbPool,
        dispatch: Arc<dyn ReactorFlowDispatch>,
        status: Arc<StatusStore>,
    ) -> Self {
        Self {
            registry: SubscriptionRegistry::new(db),
            dispatch,
            status,
            running: HashMap::new(),
        }
    }

    fn reconcile(&mut self) {
        self.reconcile_with(&|id| crate::bus::instance(id));
    }

    /// Starts newly-added or changed subscriptions, stops removed or changed
    /// ones (dropping a `RunningSubscription` aborts its task via
    /// `AbortOnDropHandle`) — a changed config gets a fresh task rather than
    /// an in-place update since any in-flight retry/backoff state is tied to
    /// the old subscription anyway. A re-save (version bump) restarts too,
    /// which is what lets a halted subscription run again. Also stops (and
    /// refuses to start) a subscription whose `instance_id` is no longer
    /// enabled — an instance being disabled/uninstalled is not a flow-config
    /// change, so it would never otherwise be caught by the `wanted !=
    /// running` comparison; this makes disabling an instance stop its flow
    /// consumers within one `RECONCILE_INTERVAL` (plan-app-platform §3.5).
    ///
    /// Before starting a subscription the service account is checked (`precheck`),
    /// and a refused one is not started. The check repeats on every
    /// reconcile, so a task that ended `Refused` comes back by itself once its
    /// cause is fixed. A task that ended `Halted` stays stopped.
    ///
    /// `resolve` finds the running engine of an instance (`bus::instance` in
    /// production).
    fn reconcile_with(&mut self, resolve: &dyn Fn(&BusInstanceId) -> Option<Arc<BusService>>) {
        let db = self.registry.db.clone();
        let status = self.status.clone();
        let scan = self.registry.current();
        let wanted: HashMap<String, (ConsumeConfig, i64)> = scan
            .subs
            .iter()
            .map(|s| (s.flow_id.clone(), (s.config.clone(), s.version)))
            .collect();
        let malformed: Vec<MalformedFlow> = scan.malformed.clone();

        for (flow_id, detail) in &malformed {
            status.record(
                flow_id,
                ConsumerState::Refused,
                Some(reason::MALFORMED_CONFIG),
                Some(detail.clone()),
            );
        }

        // Resolve each DISTINCT instance's enabled state ONCE per reconcile
        // call, not once per subscription: most subscriptions in a real
        // deployment share a handful of instance ids, so per-subscription
        // lookups ask the same `get_instance_of_package` question over and
        // over every `RECONCILE_INTERVAL`. Built fresh every call (no
        // cross-cycle cache) — a cached answer surviving between cycles
        // would delay noticing a disable past the one-`RECONCILE_INTERVAL`
        // guarantee §3.5 asks for.
        let mut enabled: HashMap<BusInstanceId, bool> = HashMap::new();
        for (config, _) in wanted.values() {
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
            let unchanged = wanted.get(flow_id).is_some_and(|(config, version)| {
                *config == running.config && *version == running.version
            });
            if !unchanged {
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
            if running.handle.is_finished() {
                // A halted task stays put (same config and version) until the
                // flow is saved again; any other ended task (refused, or a
                // panic) is dropped so the checks below decide on a restart.
                return status
                    .get(flow_id)
                    .is_some_and(|s| s.state == ConsumerState::Halted);
            }
            true
        });
        for (flow_id, (config, version)) in wanted.iter() {
            if self.running.contains_key(flow_id) {
                continue;
            }
            if !enabled[&config.instance_id] {
                tracing::debug!(
                    "bus reactor: flow '{flow_id}' instance '{}' is not enabled, not starting \
                     its subscription",
                    config.instance_id
                );
                status.record(
                    flow_id,
                    ConsumerState::Waiting,
                    Some(reason::INSTANCE_NOT_RUNNING),
                    None,
                );
                continue;
            }
            let svc = resolve(&config.instance_id);
            if let Err((why, detail)) = precheck(&db, config, svc.as_ref()) {
                status.record(flow_id, ConsumerState::Refused, Some(why), detail);
                continue;
            }
            let handle = spawn_subscription(
                flow_id.clone(),
                config.clone(),
                self.dispatch.clone(),
                self.status.clone(),
            );
            self.running.insert(
                flow_id.clone(),
                RunningSubscription {
                    config: config.clone(),
                    version: *version,
                    handle,
                },
            );
        }

        let live: HashSet<&str> = wanted
            .keys()
            .map(String::as_str)
            .chain(malformed.iter().map(|(id, _)| id.as_str()))
            .collect();
        status.retain_flows(&live);
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
    status: Arc<StatusStore>,
) -> AbortOnDropHandle<()> {
    let handle = tokio::spawn(subscription_loop(flow_id, config, dispatch, status));
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

/// The correlation id a reactive run carries, and the member ids when the
/// batch mixed several chains.
#[derive(Debug, PartialEq, Eq)]
struct BatchCorrelation {
    /// The run's correlation id: the records' shared id, or a minted one.
    id: String,
    /// Distinct member ids of a batch whose records carry different ids
    /// (`id` is then a minted batch id). `None` otherwise.
    member_ids: Option<Vec<String>>,
}

/// TB-02 D6. One record, or every record with the same id: that id. Mixed ids:
/// a minted batch id plus the distinct member ids. No record id at all (a
/// legacy producer): one minted id per dispatch. Delivery semantics do not
/// change: the batch is still one run.
fn resolve_batch_correlation(records: &[FetchedRecordMeta]) -> BatchCorrelation {
    let mut distinct: Vec<String> = Vec::new();
    let mut any_missing = false;
    for r in records {
        match correlation::from_fetched_headers(&r.headers) {
            Some(id) if !distinct.contains(&id) => distinct.push(id),
            Some(_) => {}
            None => any_missing = true,
        }
    }
    match (distinct.len(), any_missing) {
        (1, false) => BatchCorrelation {
            id: distinct.remove(0),
            member_ids: None,
        },
        (0, _) => BatchCorrelation {
            id: correlation::mint(),
            member_ids: None,
        },
        _ => BatchCorrelation {
            id: correlation::mint(),
            member_ids: Some(distinct),
        },
    }
}

fn apply_correlation_meta(env: &mut FlowEnvelope, corr: &BatchCorrelation) {
    env.meta
        .insert("bus_correlation_id".into(), serde_json::json!(corr.id));
    if let Some(ids) = &corr.member_ids {
        env.meta
            .insert("bus_correlation_ids".into(), serde_json::json!(ids));
    }
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
        .insert("bus_instance".into(), serde_json::json!(config.instance_id));
    env.meta
        .insert("bus_topic".into(), serde_json::json!(config.topic));
    env.meta.insert(
        "bus_group".into(),
        serde_json::json!(config
            .effective_group()
            .unwrap_or_else(|| config.group.clone())),
    );
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
    /// `on_error = halt`: the loop ends and only a save of the flow resumes it.
    Halt,
    /// The consumer may not read (service account or rights): the loop ends
    /// and the supervisor's pre-start check decides when to try again.
    Refused,
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
    group: &str,
    records: &[FetchedRecordMeta],
    reason: DlqReason,
    message: &str,
    run_correlation_id: Option<&str>,
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
                // The record's own chain; a legacy record without one joins the
                // run it failed in (when a run was resolved).
                let mut record_ctx = bctx.clone();
                record_ctx.correlation_id = correlation::from_fetched_headers(&record.headers)
                    .or_else(|| run_correlation_id.map(str::to_string));
                let outcome = note_failure(
                    svc.clone(),
                    record_ctx,
                    group.to_string(),
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
    status: &StatusStore,
) -> AfterFailure {
    let span = tracing::info_span!(
        "bus_reactor_cycle",
        flow_id = %flow_id,
        topic = %config.topic,
        correlation_id = tracing::field::Empty,
    );
    let after = run_cycle_in(svc, flow_id, config, dispatch, status, &span)
        .instrument(span.clone())
        .await;
    if matches!(after, AfterFailure::Halt) {
        status.record(flow_id, ConsumerState::Halted, None, None);
    }
    after
}

/// How a failed `open_consumer`/`fetch` shows up in the consumer status.
fn classify_open_error(
    db: &DbPool,
    service_account: &str,
    error: &BusServiceError,
) -> (ConsumerState, &'static str, Option<String>) {
    match error {
        // The authorizer's answer does not say why; the key row does.
        BusServiceError::PermissionDenied { .. } => (
            ConsumerState::Refused,
            service_account_refusal(db, service_account).unwrap_or(reason::READ_DENIED),
            None,
        ),
        BusServiceError::KeyNeedsTopicWideRule { .. } => {
            (ConsumerState::Refused, reason::NEEDS_TOPIC_WIDE_RULE, None)
        }
        BusServiceError::InvalidArgument(_) | BusServiceError::InvalidTopicName { .. } => (
            ConsumerState::Refused,
            reason::MALFORMED_CONFIG,
            Some(error.to_string()),
        ),
        BusServiceError::NotLeader { .. } => (ConsumerState::Waiting, reason::NOT_LEADER, None),
        BusServiceError::NotInitialized => {
            (ConsumerState::Waiting, reason::INSTANCE_NOT_RUNNING, None)
        }
        BusServiceError::TopicNotFound { .. } => {
            (ConsumerState::Waiting, reason::TOPIC_NOT_FOUND, None)
        }
        other => (
            ConsumerState::Waiting,
            reason::BUS_ERROR,
            Some(other.to_string()),
        ),
    }
}

async fn run_cycle_in(
    svc: &Arc<BusService>,
    flow_id: &str,
    config: &ConsumeConfig,
    dispatch: &Arc<dyn ReactorFlowDispatch>,
    status: &StatusStore,
    span: &tracing::Span,
) -> AfterFailure {
    let (Some(service_account), Some(group)) =
        (config.service_account.as_deref(), config.effective_group())
    else {
        status.record(
            flow_id,
            ConsumerState::Refused,
            Some(reason::SERVICE_ACCOUNT_MISSING),
            None,
        );
        return AfterFailure::Refused;
    };
    let bctx = consumer_context(svc.typed_instance_id(), config, service_account);

    let (handle, mut records) = {
        let task_svc = svc.clone();
        let open_bctx = bctx.clone();
        let group = group.clone();
        let topic = config.topic.clone();
        let commit_mode = config.commit_mode;
        let max_wait_ms = config.max_wait_ms;
        let joined = tokio::task::spawn_blocking(move || {
            let handle = task_svc.open_consumer(
                &open_bctx,
                &group,
                &[topic],
                ConsumerConfig { commit_mode },
            )?;
            let batch = handle.fetch(FETCH_MAX_BYTES, max_wait_ms)?;
            Ok::<_, BusServiceError>((handle, batch))
        })
        .await;
        match joined {
            Ok(Ok((handle, batch))) => {
                status.record(flow_id, ConsumerState::Running, None, None);
                (handle, batch.records)
            }
            Ok(Err(e)) => {
                let (state, why, detail) = classify_open_error(svc.db(), service_account, &e);
                status.record(flow_id, state, Some(why), detail);
                if state == ConsumerState::Refused {
                    return AfterFailure::Refused;
                }
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
                &group,
                &records,
                DlqReason::SchemaViolation,
                &msg,
                None,
            )
            .await;
        }
    };

    let batch_corr = resolve_batch_correlation(&records);
    span.record("correlation_id", batch_corr.id.as_str());
    let mut envelope = build_envelope(config, &records, &values);
    apply_correlation_meta(&mut envelope, &batch_corr);
    let principal = AgentPrincipal::new(
        None,
        Some(config.org_id.clone()),
        FlowOrigin::Bus,
        FlowActor::api_key(service_account, None),
    )
    .with_correlation_id(Some(batch_corr.id.clone()));

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
            // cannot reach here — `ConsumeConfig::from_config` rejects it, so
            // `build_subscriptions` never builds such a subscription. See
            // this module's doc.
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
                &group,
                &records,
                DlqReason::ConsumerError,
                &msg,
                Some(batch_corr.id.as_str()),
            )
            .await
        }
    }
}

/// One subscription's background poll loop: waits for `config.instance_id`'s
/// engine to be running, then runs `run_cycle` until it signals a halt or a
/// refusal (the supervisor restarts a refused one once its pre-start check
/// passes again).
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
    status: Arc<StatusStore>,
) {
    loop {
        let Some(svc) = crate::bus::instance(&config.instance_id) else {
            // Disabled, uninstalled, or not started yet: back off.
            status.record(
                &flow_id,
                ConsumerState::Waiting,
                Some(reason::INSTANCE_NOT_RUNNING),
                None,
            );
            tokio::time::sleep(ERROR_BACKOFF).await;
            continue;
        };
        match run_cycle(&svc, &flow_id, &config, &dispatch, &status).await {
            AfterFailure::Continue => {}
            AfterFailure::Halt | AfterFailure::Refused => break,
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

/// Ids of the active flows whose `bus_consume` entry has no `service_account`:
/// they have never read anything (the reactor used to consume with no identity
/// at all) and stay refused until an operator binds a key. Pure and DB-free,
/// like `flows_missing_instance_id`.
fn flows_missing_service_account(flows: &[(String, String)]) -> Vec<String> {
    flows
        .iter()
        .filter(|(_, flow_json)| {
            serde_json::from_str::<FlowDefinition>(flow_json)
                .ok()
                .and_then(|def| {
                    def.nodes
                        .into_iter()
                        .find(|n| n.node_type == bus_consume::NODE_TYPE)
                })
                .is_some_and(|node| {
                    node.config
                        .get("service_account")
                        .and_then(|v| v.as_str())
                        .map_or(true, |s| s.trim().is_empty())
                })
        })
        .map(|(flow_id, _)| flow_id.clone())
        .collect()
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
    for flow_id in flows_missing_service_account(&active) {
        tracing::warn!(
            "bus reactor: flow '{flow_id}' has a bus_consume entry with no 'service_account' — \
             it reads nothing until an operator picks a general API key for it and saves the flow"
        );
    }
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

    /// A well-formed key uid for tests whose authorizer allows everything and
    /// that never read the key row.
    const TEST_KEY_UID: &str = "0b9e6a6e-7f0c-4a53-9d1a-3a1f2f6c8b11";

    /// A general API key row in `pool`; returns its uid.
    fn seed_general_key(pool: &DbPool) -> String {
        seed_key(pool, "general", None)
    }

    fn seed_key(pool: &DbPool, key_type: &str, subject: Option<&str>) -> String {
        let (_, uid) = repository::create_api_key(
            pool,
            &format!("verifier-{}", uuid::Uuid::new_v4()),
            "sk-...abcdef",
            "Flow service account",
            key_type,
            subject,
            60,
        )
        .expect("create key");
        uid
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
            json!({
                "instance_id": "tentabus-8b000001", "service_account": TEST_KEY_UID,
                "topic": "orders.raw", "group": "g1"
            }),
        );
        let flow = repository::get_flow(&pool, &flow_id).unwrap().unwrap();
        let scan = build_subscriptions(&[(flow_id.clone(), flow.flow_json, flow.version)]);
        assert_eq!(scan.subs.len(), 1);
        assert!(scan.malformed.is_empty());
        let sub = &scan.subs[0];
        assert_eq!(sub.flow_id, flow_id);
        assert_eq!(sub.version, flow.version);
        assert_eq!(sub.config.instance_id.as_str(), "tentabus-8b000001");
        assert_eq!(sub.config.topic, "orders.raw");
        assert_eq!(sub.config.group, "g1");
        assert_eq!(
            sub.config.effective_group().unwrap(),
            format!("k:{TEST_KEY_UID}.g1")
        );
    }

    /// A legacy node without a service account still parses: the supervisor
    /// refuses it with its own reason instead of the scan dropping it silently.
    #[test]
    fn build_subscriptions_keeps_a_node_without_a_service_account() {
        let flow_json = json!({
            "nodes": [{
                "id": "c", "type": bus_consume::NODE_TYPE,
                "config": {"instance_id": "tentabus-8b000001", "topic": "orders.raw", "group": "g1"}
            }],
            "edges": []
        })
        .to_string();
        let scan = build_subscriptions(&[("f1".to_string(), flow_json, 1)]);
        assert_eq!(scan.subs.len(), 1);
        assert_eq!(scan.subs[0].config.service_account, None);
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
        let scan = build_subscriptions(&[("f1".to_string(), flow_json, 1)]);
        assert!(
            scan.subs.is_empty(),
            "missing instance_id must be skipped, not panic"
        );
        assert_eq!(scan.malformed.len(), 1);
        assert_eq!(scan.malformed[0].0, "f1");
    }

    /// plan-app-platform §9 (R8): the startup migration scan finds a
    /// `bus_consume` node with an empty `instance_id`, and — unlike
    /// `build_subscriptions` — also finds `bus_publish`/`bus_transform`
    /// nodes it never parses.
    #[test]
    fn flows_missing_instance_id_finds_every_bus_node_type() {
        let flow_json = json!({
            "nodes": [
                {"id": "c", "type": "bus_consume", "config": {"instance_id": "", "topic": "t", "group": "gg"}},
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
                    "instance_id": "tentabus-aaaaaaaa", "topic": "t", "group": "gg"
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
        let scan = build_subscriptions(&[("f1".to_string(), flow_json, 1)]);
        assert!(scan.subs.is_empty());
        assert!(scan.malformed.is_empty());
    }

    #[test]
    fn build_subscriptions_skips_malformed_config() {
        let flow_json = json!({
            "nodes": [{"id": "c", "type": bus_consume::NODE_TYPE, "config": {}}],
            "edges": []
        })
        .to_string();
        let scan = build_subscriptions(&[("f1".to_string(), flow_json, 1)]);
        assert!(
            scan.subs.is_empty(),
            "missing topic/group must be skipped, not panic"
        );
        assert_eq!(scan.malformed.len(), 1);
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
            &json!({"instance_id": "tentabus-8b000002", "topic": "t", "group": "gg"}),
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
            &json!({"instance_id": "tentabus-8b000002", "topic": "t", "group": "gg"}),
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
        let key = seed_general_key(&pool);
        let mut reactor = BusReactor::new(pool.clone(), Arc::new(NoopDispatch));

        reactor.reconcile();
        assert!(reactor.running.is_empty(), "no subscriptions yet");

        let flow_id = seed_consume_flow(
            &pool,
            "react",
            json!({
                "instance_id": "tentabus-8b000003", "service_account": key,
                "topic": "t", "group": "gg"
            }),
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
            json!({
                "instance_id": "tentabus-8b000004", "service_account": TEST_KEY_UID,
                "topic": "t", "group": "gg"
            }),
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
        let key = seed_general_key(&pool);
        let mut reactor = BusReactor::new(pool.clone(), Arc::new(NoopDispatch));

        seed_consume_flow(
            &pool,
            "react",
            json!({
                "instance_id": "tentabus-8b000005", "service_account": key,
                "topic": "t", "group": "gg"
            }),
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
                settings_cipher: Some(std::sync::Arc::new(crate::crypto::SettingsCipher::new(
                    &[7u8; 32],
                ))),
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
            actor_kind: crate::auth::actor::ActorKind::User,
            correlation_id: None,
            origin: "test".to_string(),
        }
    }

    fn test_ctx_for(instance_id: &BusInstanceId, org: &str) -> BusCallContext {
        BusCallContext {
            instance_id: instance_id.clone(),
            org_id: org.to_string(),
            actor: Some("tester".to_string()),
            actor_kind: crate::auth::actor::ActorKind::User,
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
            settings_cipher: Some(std::sync::Arc::new(crate::crypto::SettingsCipher::new(
                &[7u8; 32],
            ))),
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
            "instance_id": "tentabus-00000001", "service_account": TEST_KEY_UID, "topic": "orders.raw", "group": "g1", "max_wait_ms": 20
        }))
        .unwrap();

        let after = run_cycle(&svc, "flow-1", &config, &dispatch, &StatusStore::default()).await;
        assert!(matches!(after, AfterFailure::Continue));
        {
            let calls = scripted.calls.lock().unwrap();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0], ("flow-1".to_string(), json!({"id": 1})));
        }

        // Second cycle: nothing new (short max_wait_ms keeps this fast).
        let after2 = run_cycle(&svc, "flow-1", &config, &dispatch, &StatusStore::default()).await;
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
            "instance_id": "tentabus-00000001", "service_account": TEST_KEY_UID, "topic": "orders.raw", "group": "g1", "max_wait_ms": 20, "on_error": "halt"
        }))
        .unwrap();

        let after = run_cycle(&svc, "flow-1", &config, &dispatch, &StatusStore::default()).await;
        assert!(matches!(after, AfterFailure::Halt));

        // Uncommitted: a fresh handle in the flow's own group still sees the
        // record.
        let group = config.effective_group().unwrap();
        let records = fetch_once(&svc, &ctx, &group, "orders.raw").await;
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
            "instance_id": "tentabus-00000001", "service_account": TEST_KEY_UID, "topic": "orders.raw", "group": "g1", "max_wait_ms": 20, "on_error": "dlq"
        }))
        .unwrap();

        let after = run_cycle(&svc, "flow-1", &config, &dispatch, &StatusStore::default()).await;
        assert!(matches!(after, AfterFailure::Continue));

        let group = config.effective_group().unwrap();
        let records = fetch_once(&svc, &ctx, &group, "orders.raw").await;
        assert!(
            records.is_empty(),
            "DLQ escalation must advance the source offset past the poison record"
        );

        let dlq_topic = crate::bus::dlq::dlq_topic_name("orders.raw");
        let dlq_records = fetch_once(&svc, &ctx, "dlq-reader", &dlq_topic).await;
        assert_eq!(dlq_records.len(), 1, "one record landed in the DLQ");
    }

    fn record_with_correlation(offset: u64, id: Option<&str>) -> FetchedRecordMeta {
        let mut r = record("t", 0, offset, "{}");
        if let Some(id) = id {
            r.headers.push((
                Bytes::from_static(b"tf.correlation_id"),
                Bytes::from(id.to_string()),
            ));
        }
        r
    }

    #[test]
    fn batch_correlation_uses_the_shared_id_of_uniform_records() {
        let one = resolve_batch_correlation(&[record_with_correlation(0, Some("c-1"))]);
        assert_eq!(one.id, "c-1");
        assert!(one.member_ids.is_none());
        let same = resolve_batch_correlation(&[
            record_with_correlation(0, Some("c-1")),
            record_with_correlation(1, Some("c-1")),
        ]);
        assert_eq!(same.id, "c-1");
        assert!(same.member_ids.is_none());
    }

    #[test]
    fn batch_correlation_mints_a_batch_id_for_mixed_records() {
        let mixed = resolve_batch_correlation(&[
            record_with_correlation(0, Some("c-1")),
            record_with_correlation(1, Some("c-2")),
            record_with_correlation(2, Some("c-1")),
        ]);
        assert!(correlation::validate(&mixed.id));
        assert!(mixed.id != "c-1" && mixed.id != "c-2");
        assert_eq!(
            mixed.member_ids,
            Some(vec!["c-1".to_string(), "c-2".to_string()])
        );
        let partial = resolve_batch_correlation(&[
            record_with_correlation(0, Some("c-1")),
            record_with_correlation(1, None),
        ]);
        assert_ne!(partial.id, "c-1");
        assert_eq!(partial.member_ids, Some(vec!["c-1".to_string()]));
    }

    #[test]
    fn batch_correlation_mints_one_id_for_legacy_records() {
        let legacy = resolve_batch_correlation(&[record_with_correlation(0, None)]);
        assert!(correlation::validate(&legacy.id));
        assert!(legacy.member_ids.is_none());
    }

    /// Records the principal and envelope meta of every dispatch.
    struct CapturingDispatch {
        seen: Mutex<Vec<(Option<String>, serde_json::Map<String, serde_json::Value>)>>,
        fail: bool,
    }

    #[async_trait]
    impl ReactorFlowDispatch for CapturingDispatch {
        async fn dispatch(
            &self,
            _flow_id: String,
            initial: FlowEnvelope,
            principal: AgentPrincipal,
        ) -> anyhow::Result<()> {
            let meta = initial
                .meta
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            self.seen
                .lock()
                .unwrap()
                .push((principal.correlation_id, meta));
            if self.fail {
                anyhow::bail!("scripted dispatch failure");
            }
            Ok(())
        }
    }

    async fn publish_with_correlation(
        svc: &Arc<BusService>,
        ctx: &BusCallContext,
        topic: &str,
        records: Vec<(&str, Option<&str>)>,
    ) {
        let svc = svc.clone();
        let ctx = ctx.clone();
        let topic = topic.to_string();
        let records: Vec<PublishRecord> = records
            .into_iter()
            .map(|(payload, id)| PublishRecord {
                key: None,
                headers: id
                    .map(|id| vec![("correlation-id".to_string(), Bytes::from(id.to_string()))])
                    .unwrap_or_default(),
                payload: Bytes::from(payload.to_string()),
                timestamp_ms: 0,
                schema_id: 0,
            })
            .collect();
        blocking(move || {
            svc.publish(
                &ctx,
                &topic,
                PublishBatch {
                    partition: Some(0),
                    producer: None,
                    records,
                },
            )
        })
        .await
        .expect("publish");
    }

    #[tokio::test]
    async fn run_cycle_carries_the_record_correlation_id_into_the_run() {
        let (_dir, svc) = test_bus_service();
        let ctx = test_ctx("org-default");
        create_topic(&svc, &ctx, "orders.raw", TopicOptions::default()).await;
        publish_with_correlation(
            &svc,
            &ctx,
            "orders.raw",
            vec![(r#"{"id":1}"#, Some("chain-7"))],
        )
        .await;

        let spy = Arc::new(CapturingDispatch {
            seen: Mutex::new(Vec::new()),
            fail: false,
        });
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let config = ConsumeConfig::from_config(&json!({
            "instance_id": "tentabus-00000001", "service_account": TEST_KEY_UID, "topic": "orders.raw", "group": "g1", "max_wait_ms": 20
        }))
        .unwrap();
        run_cycle(&svc, "flow-1", &config, &dispatch, &StatusStore::default()).await;

        let seen = spy.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0.as_deref(), Some("chain-7"));
        assert_eq!(seen[0].1.get("bus_correlation_id"), Some(&json!("chain-7")));
        assert!(seen[0].1.get("bus_correlation_ids").is_none());
    }

    #[tokio::test]
    async fn run_cycle_mints_a_batch_id_and_lists_members_for_a_mixed_batch() {
        let (_dir, svc) = test_bus_service();
        let ctx = test_ctx("org-default");
        create_topic(&svc, &ctx, "orders.raw", TopicOptions::default()).await;
        publish_with_correlation(
            &svc,
            &ctx,
            "orders.raw",
            vec![("{}", Some("chain-a")), ("{}", Some("chain-b"))],
        )
        .await;

        let spy = Arc::new(CapturingDispatch {
            seen: Mutex::new(Vec::new()),
            fail: false,
        });
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let config = ConsumeConfig::from_config(&json!({
            "instance_id": "tentabus-00000001", "service_account": TEST_KEY_UID, "topic": "orders.raw", "group": "g1",
            "max_wait_ms": 20, "batch_size": 10
        }))
        .unwrap();
        run_cycle(&svc, "flow-1", &config, &dispatch, &StatusStore::default()).await;

        let seen = spy.seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "a mixed batch is still one run");
        let batch_id = seen[0].0.clone().expect("a batch id is minted");
        assert!(batch_id != "chain-a" && batch_id != "chain-b");
        assert_eq!(seen[0].1.get("bus_correlation_id"), Some(&json!(batch_id)));
        assert_eq!(
            seen[0].1.get("bus_correlation_ids"),
            Some(&json!(["chain-a", "chain-b"]))
        );
    }

    #[tokio::test]
    async fn run_cycle_mints_an_id_for_a_legacy_record_without_one() {
        let (_dir, svc) = test_bus_service();
        let ctx = test_ctx("org-default");
        create_topic(&svc, &ctx, "orders.raw", TopicOptions::default()).await;
        publish_one(&svc, &ctx, "orders.raw", r#"{"id":1}"#).await;

        let spy = Arc::new(CapturingDispatch {
            seen: Mutex::new(Vec::new()),
            fail: false,
        });
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let config = ConsumeConfig::from_config(&json!({
            "instance_id": "tentabus-00000001", "service_account": TEST_KEY_UID, "topic": "orders.raw", "group": "g1", "max_wait_ms": 20
        }))
        .unwrap();
        run_cycle(&svc, "flow-1", &config, &dispatch, &StatusStore::default()).await;

        let seen = spy.seen.lock().unwrap();
        let minted = seen[0].0.as_deref().expect("an id is minted");
        assert!(correlation::validate(minted));
    }

    #[tokio::test]
    async fn run_cycle_dlq_keeps_the_record_correlation_id() {
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
        publish_with_correlation(
            &svc,
            &ctx,
            "orders.raw",
            vec![(r#"{"id":1}"#, Some("chain-dlq"))],
        )
        .await;

        let spy = Arc::new(CapturingDispatch {
            seen: Mutex::new(Vec::new()),
            fail: true,
        });
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy;
        let config = ConsumeConfig::from_config(&json!({
            "instance_id": "tentabus-00000001", "service_account": TEST_KEY_UID, "topic": "orders.raw", "group": "g1",
            "max_wait_ms": 20, "on_error": "dlq"
        }))
        .unwrap();
        run_cycle(&svc, "flow-1", &config, &dispatch, &StatusStore::default()).await;

        let dlq_topic = crate::bus::dlq::dlq_topic_name("orders.raw");
        let dlq_records = fetch_once(&svc, &ctx, "dlq-reader", &dlq_topic).await;
        assert_eq!(dlq_records.len(), 1);
        assert_eq!(
            correlation::from_fetched_headers(&dlq_records[0].headers).as_deref(),
            Some("chain-dlq")
        );
    }

    /// F4 B7 review R1 P2-2: the flow reads through the reader's field-policy
    /// rule (masked and hashed), but the DLQ keeps the record AS PUBLISHED —
    /// otherwise "retry" would republish `*************111` and a hash as the
    /// card and the name.
    #[tokio::test]
    async fn run_cycle_dlq_keeps_the_published_record_not_the_masked_and_hashed_one() {
        use crate::bus::field_policies as fp;
        let (_dir, svc) = test_bus_service();
        let ctx = test_ctx("org-default");
        create_topic(
            &svc,
            &ctx,
            "cards.raw",
            TopicOptions {
                max_delivery_attempts: Some(1),
                content_type: Some("application/json".to_string()),
                ..Default::default()
            },
        )
        .await;
        let raw = r#"{"id":"7","card":"4111111111111111","name":"Jan"}"#;
        publish_one(&svc, &ctx, "cards.raw", raw).await;
        let set = |masked: &[&str], hashed: &[&str]| {
            let names = |v: &[&str]| {
                v.iter()
                    .map(|s| s.to_string())
                    .collect::<std::collections::BTreeSet<_>>()
            };
            fp::set_policy(
                svc.db(),
                svc.instance_id(),
                "org-default",
                "cards.raw",
                "any",
                fp::SUBJECT_ANY,
                fp::Direction::Read,
                &names(&["id", "card", "name"]),
                &names(&[]),
                &names(masked),
                &names(hashed),
                crate::db::repository::BusFieldPolicyExpect::Any,
            )
            .expect("rule");
        };
        set(&["card"], &["name"]);
        let hash = svc
            .field_hash_keys()
            .get_or_create("org-default")
            .unwrap()
            .hash_hex(b"Jan");

        let scripted = Arc::new(ScriptedDispatch::new());
        scripted
            .fail_next
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let dispatch: Arc<dyn ReactorFlowDispatch> = scripted.clone();
        let config = ConsumeConfig::from_config(&json!({
            "instance_id": "tentabus-00000001", "service_account": TEST_KEY_UID, "topic": "cards.raw", "group": "g1", "max_wait_ms": 20, "on_error": "dlq"
        }))
        .unwrap();
        let after = run_cycle(&svc, "flow-1", &config, &dispatch, &StatusStore::default()).await;
        assert!(matches!(after, AfterFailure::Continue));
        assert_eq!(
            scripted.calls.lock().unwrap()[0].1,
            json!({"id": "7", "card": "*************111", "name": hash}),
            "the flow itself reads through the rule"
        );

        // What the DLQ holds is read with no rule in the way.
        {
            let svc = svc.clone();
            blocking(move || {
                fp::delete_policy(
                    svc.db(),
                    svc.instance_id(),
                    "org-default",
                    "cards.raw",
                    "any",
                    fp::SUBJECT_ANY,
                    fp::Direction::Read,
                    None,
                )
            })
            .await
            .expect("delete rule");
        }
        let dlq_topic = crate::bus::dlq::dlq_topic_name("cards.raw");
        let dlq_records = fetch_once(&svc, &ctx, "dlq-reader", &dlq_topic).await;
        assert_eq!(dlq_records.len(), 1);
        assert_eq!(
            String::from_utf8(dlq_records[0].payload.to_vec()).unwrap(),
            raw,
            "the DLQ entry is the published record"
        );
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

        let _handle = spawn_subscription(
            "flow-cross".to_string(),
            config,
            dispatch,
            Arc::new(StatusStore::default()),
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            scripted.calls.lock().unwrap().is_empty(),
            "a subscription configured for instance B must never consume instance A's \
             records, even when A is the only instance running"
        );

        crate::bus::stop_instance(&id_a);
    }

    // ---- Service-account consumption behind the REAL authorizer ------------
    //
    // Every test above uses an allow-all authorizer, which is why a reactor
    // that consumed with no identity at all went unnoticed. These run the
    // production `InstanceBusAuthorizer` on the same database the reactor reads.

    const REAL_INSTANCE: &str = "tentabus-00000001";

    struct RealAuth {
        _dir: tempfile::TempDir,
        db: DbPool,
        svc: Arc<BusService>,
    }

    /// A `BusService` behind `InstanceBusAuthorizer`, with the user `tester`
    /// holding every matrix permission (it creates and fills topics).
    fn real_auth_bus_service() -> RealAuth {
        use crate::addon::permissions::PermissionChecker;
        use crate::services::bus_authorizer::InstanceBusAuthorizer;
        let dir = tempfile::tempdir().expect("tempdir");
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
        crate::db::repository::bus_test_support::create_bus_tables(&db).expect("bus tables");
        let local_conn = rusqlite::Connection::open_in_memory().expect("open local db");
        crate::bus::db::migrate(&local_conn).expect("migrate local db");
        let local_db: DbPool = Arc::new(crate::db::Db::from_connection(local_conn));
        let id = BusInstanceId::parse(REAL_INSTANCE).expect("valid instance id");
        let checker = Arc::new(PermissionChecker::new(db.clone()));
        for perm in ["bus.read", "bus.write", "bus.admin"] {
            repository::upsert_permission(
                &db,
                REAL_INSTANCE,
                "user",
                "tester",
                perm,
                "allow",
                None,
            )
            .expect("grant");
        }
        checker.refresh_addon(REAL_INSTANCE);
        let svc = Arc::new(
            BusService::new(BusInitConfig {
                instance_id: id.clone(),
                local_db,
                bus_dir: dir.path().join("bus"),
                db: db.clone(),
                authorizer: Arc::new(InstanceBusAuthorizer::new(db.clone(), id, checker)),
                retention_interval: None,
                dedup_expected_rate_per_sec: 10_000,
                partition_handle_lru: None,
                publish_ack_timeout: crate::bus::DEFAULT_PUBLISH_ACK_TIMEOUT,
                settings_cipher: Some(Arc::new(crate::crypto::SettingsCipher::new(&[7u8; 32]))),
            })
            .expect("bus service"),
        );
        install_bus_instance(&db, REAL_INSTANCE, true);
        RealAuth { _dir: dir, db, svc }
    }

    fn topic_right(db: &DbPool, key_uid: &str, topic: &str, action: &str) {
        repository::resource_permissions::set_with_action(
            db,
            "topic",
            &crate::services::bus_authorizer::topic_acl_resource_id(
                REAL_INSTANCE,
                "org-default",
                topic,
            ),
            "api_key",
            key_uid,
            action,
            "allow",
        )
        .expect("topic right");
    }

    fn key_ctx(key_uid: &str) -> BusCallContext {
        BusCallContext {
            actor: Some(key_uid.to_string()),
            actor_kind: ActorKind::ApiKey,
            ..test_ctx("org-default")
        }
    }

    fn consume_config(key_uid: &str, extra: serde_json::Value) -> ConsumeConfig {
        let mut config = json!({
            "instance_id": REAL_INSTANCE, "service_account": key_uid,
            "topic": "orders.raw", "group": "g1", "max_wait_ms": 20
        });
        config
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        ConsumeConfig::from_config(&config).expect("config")
    }

    /// Records the actor of every run principal.
    struct ActorSpy {
        actors: Mutex<Vec<FlowActor>>,
        fail: bool,
    }

    impl ActorSpy {
        fn new(fail: bool) -> Arc<Self> {
            Arc::new(Self {
                actors: Mutex::new(Vec::new()),
                fail,
            })
        }
    }

    #[async_trait]
    impl ReactorFlowDispatch for ActorSpy {
        async fn dispatch(
            &self,
            _flow_id: String,
            _initial: FlowEnvelope,
            principal: AgentPrincipal,
        ) -> anyhow::Result<()> {
            self.actors.lock().unwrap().push(principal.actor);
            if self.fail {
                anyhow::bail!("scripted dispatch failure");
            }
            Ok(())
        }
    }

    /// A topic `orders.raw` holding one record, made by the user `tester`.
    async fn real_topic_with_record(fx: &RealAuth, opts: TopicOptions) {
        let ctx = test_ctx("org-default");
        create_topic(&fx.svc, &ctx, "orders.raw", opts).await;
        publish_one(&fx.svc, &ctx, "orders.raw", r#"{"id":1}"#).await;
    }

    fn status_of(status: &StatusStore, flow: &str) -> (ConsumerState, Option<&'static str>) {
        let s = status.get(flow).expect("a status was recorded");
        (s.state, s.reason)
    }

    /// THE regression guard: the reactor used to consume with no actor, which
    /// the production authorizer refuses, so no `bus_consume` flow ever read.
    /// Now the flow reads as its service account: the run carries the key, the
    /// offset is committed under the key's own group, and nothing redelivers.
    #[tokio::test]
    async fn real_authorizer_a_service_account_with_read_rights_consumes() {
        let fx = real_auth_bus_service();
        real_topic_with_record(&fx, TopicOptions::default()).await;
        let key = seed_general_key(&fx.db);
        topic_right(&fx.db, &key, "orders.raw", "read");

        let spy = ActorSpy::new(false);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let status = StatusStore::default();
        let config = consume_config(&key, json!({}));

        let after = run_cycle(&fx.svc, "flow-real-ok", &config, &dispatch, &status).await;
        assert!(matches!(after, AfterFailure::Continue));
        assert_eq!(
            *spy.actors.lock().unwrap(),
            vec![FlowActor::api_key(key.clone(), None)]
        );
        assert_eq!(
            status_of(&status, "flow-real-ok"),
            (ConsumerState::Running, None)
        );

        // Committed under the key's group: a fresh consumer there sees nothing,
        // and so does the next cycle.
        let group = format!("k:{key}.g1");
        assert!(fetch_once(&fx.svc, &key_ctx(&key), &group, "orders.raw")
            .await
            .is_empty());
        run_cycle(&fx.svc, "flow-real-ok", &config, &dispatch, &status).await;
        assert_eq!(spy.actors.lock().unwrap().len(), 1, "no redelivery");
    }

    #[tokio::test]
    async fn real_authorizer_a_key_without_read_rights_is_refused_and_reads_nothing() {
        let fx = real_auth_bus_service();
        real_topic_with_record(&fx, TopicOptions::default()).await;
        let key = seed_general_key(&fx.db);

        let spy = ActorSpy::new(false);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let status = StatusStore::default();
        let after = run_cycle(
            &fx.svc,
            "flow-real-denied",
            &consume_config(&key, json!({})),
            &dispatch,
            &status,
        )
        .await;
        assert!(matches!(after, AfterFailure::Refused));
        assert_eq!(
            status_of(&status, "flow-real-denied"),
            (ConsumerState::Refused, Some(reason::READ_DENIED))
        );
        assert!(spy.actors.lock().unwrap().is_empty());
    }

    /// A write right is not a read right.
    #[tokio::test]
    async fn real_authorizer_a_write_only_key_cannot_consume() {
        let fx = real_auth_bus_service();
        real_topic_with_record(&fx, TopicOptions::default()).await;
        let key = seed_general_key(&fx.db);
        topic_right(&fx.db, &key, "orders.raw", "write");

        let dispatch: Arc<dyn ReactorFlowDispatch> = ActorSpy::new(false);
        let status = StatusStore::default();
        let after = run_cycle(
            &fx.svc,
            "flow-real-write-only",
            &consume_config(&key, json!({})),
            &dispatch,
            &status,
        )
        .await;
        assert!(matches!(after, AfterFailure::Refused));
    }

    /// A revoked key stops the next cycle, and the loop ends instead of
    /// retrying every backoff.
    #[tokio::test]
    async fn real_authorizer_a_revoked_key_ends_the_consumer() {
        let fx = real_auth_bus_service();
        real_topic_with_record(&fx, TopicOptions::default()).await;
        let key = seed_general_key(&fx.db);
        topic_right(&fx.db, &key, "orders.raw", "read");

        let spy = ActorSpy::new(false);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let status = StatusStore::default();
        let config = consume_config(&key, json!({}));
        let first = run_cycle(&fx.svc, "flow-real-revoked", &config, &dispatch, &status).await;
        assert!(matches!(first, AfterFailure::Continue));
        assert_eq!(spy.actors.lock().unwrap().len(), 1);

        repository::delete_api_key_by_uid(&fx.db, &key).expect("revoke");
        publish_one(
            &fx.svc,
            &test_ctx("org-default"),
            "orders.raw",
            r#"{"id":2}"#,
        )
        .await;
        let second = run_cycle(&fx.svc, "flow-real-revoked", &config, &dispatch, &status).await;
        assert!(matches!(second, AfterFailure::Refused));
        assert_eq!(
            status_of(&status, "flow-real-revoked"),
            (
                ConsumerState::Refused,
                Some(reason::SERVICE_ACCOUNT_NOT_FOUND)
            )
        );
        assert_eq!(
            spy.actors.lock().unwrap().len(),
            1,
            "nothing read after revoke"
        );
    }

    #[tokio::test]
    async fn real_authorizer_a_disabled_key_is_refused() {
        let fx = real_auth_bus_service();
        real_topic_with_record(&fx, TopicOptions::default()).await;
        let key = seed_general_key(&fx.db);
        topic_right(&fx.db, &key, "orders.raw", "read");
        repository::set_api_key_active(&fx.db, &key, false).expect("disable");

        let dispatch: Arc<dyn ReactorFlowDispatch> = ActorSpy::new(false);
        let status = StatusStore::default();
        let after = run_cycle(
            &fx.svc,
            "flow-real-disabled",
            &consume_config(&key, json!({})),
            &dispatch,
            &status,
        )
        .await;
        assert!(matches!(after, AfterFailure::Refused));
        assert_eq!(
            status_of(&status, "flow-real-disabled"),
            (
                ConsumerState::Refused,
                Some(reason::SERVICE_ACCOUNT_DISABLED)
            )
        );
    }

    /// A key bound to a user acts as that user elsewhere, so it can never be a
    /// flow's identity, whatever rights rows it carries.
    #[tokio::test]
    async fn real_authorizer_a_user_bound_key_is_refused() {
        let fx = real_auth_bus_service();
        real_topic_with_record(&fx, TopicOptions::default()).await;
        let key = seed_key(&fx.db, "user", Some("u-owner"));
        topic_right(&fx.db, &key, "orders.raw", "read");

        let dispatch: Arc<dyn ReactorFlowDispatch> = ActorSpy::new(false);
        let status = StatusStore::default();
        let after = run_cycle(
            &fx.svc,
            "flow-real-user-key",
            &consume_config(&key, json!({})),
            &dispatch,
            &status,
        )
        .await;
        assert!(matches!(after, AfterFailure::Refused));
        assert_eq!(
            status_of(&status, "flow-real-user-key"),
            (
                ConsumerState::Refused,
                Some(reason::SERVICE_ACCOUNT_NOT_SERVICE_KEY)
            )
        );
    }

    /// A topic that hides data from chosen subjects only is closed to a key
    /// until the topic-wide rule exists: reading it would be the one access
    /// those rules were never written for.
    #[tokio::test]
    async fn real_authorizer_a_key_needs_the_topic_wide_rule_when_rules_exist() {
        use crate::bus::field_policies as fp;
        let fx = real_auth_bus_service();
        real_topic_with_record(
            &fx,
            TopicOptions {
                content_type: Some("application/json".to_string()),
                ..Default::default()
            },
        )
        .await;
        let key = seed_general_key(&fx.db);
        topic_right(&fx.db, &key, "orders.raw", "read");
        let names = |v: &[&str]| {
            v.iter()
                .map(|s| s.to_string())
                .collect::<std::collections::BTreeSet<_>>()
        };
        fp::set_policy(
            &fx.db,
            REAL_INSTANCE,
            "org-default",
            "orders.raw",
            "user",
            "someone-else",
            fp::Direction::Read,
            &names(&["id"]),
            &names(&[]),
            &names(&[]),
            &names(&[]),
            crate::db::repository::BusFieldPolicyExpect::Any,
        )
        .expect("rule for another subject");

        let config = consume_config(&key, json!({}));
        let dispatch: Arc<dyn ReactorFlowDispatch> = ActorSpy::new(false);
        let status = StatusStore::default();
        let after = run_cycle(&fx.svc, "flow-real-rules", &config, &dispatch, &status).await;
        assert!(matches!(after, AfterFailure::Refused));
        assert_eq!(
            status_of(&status, "flow-real-rules"),
            (ConsumerState::Refused, Some(reason::NEEDS_TOPIC_WIDE_RULE))
        );
        // The pre-start check sees the same without opening a consumer.
        assert!(matches!(
            precheck(&fx.db, &config, Some(&fx.svc)),
            Err((reason::NEEDS_TOPIC_WIDE_RULE, _))
        ));
    }

    /// `on_error = dlq` must work for a key too: it holds no right on the
    /// reserved `__dlq.*` topic, so the broker writes the dead letter itself,
    /// once the key's read right on the source was checked. This failed before
    /// the broker did that.
    #[tokio::test]
    async fn real_authorizer_dlq_on_error_lands_the_record_in_the_dlq() {
        let fx = real_auth_bus_service();
        real_topic_with_record(
            &fx,
            TopicOptions {
                max_delivery_attempts: Some(1),
                ..Default::default()
            },
        )
        .await;
        let key = seed_general_key(&fx.db);
        topic_right(&fx.db, &key, "orders.raw", "read");

        let spy = ActorSpy::new(true);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let status = StatusStore::default();
        let config = consume_config(&key, json!({"on_error": "dlq"}));
        let after = run_cycle(&fx.svc, "flow-real-dlq", &config, &dispatch, &status).await;
        assert!(matches!(after, AfterFailure::Continue));

        let dlq_topic = crate::bus::dlq::dlq_topic_name("orders.raw");
        let dlq_records =
            fetch_once(&fx.svc, &test_ctx("org-default"), "dlq-reader", &dlq_topic).await;
        assert_eq!(dlq_records.len(), 1, "the dead letter landed");
        let group = format!("k:{key}.g1");
        assert!(
            fetch_once(&fx.svc, &key_ctx(&key), &group, "orders.raw")
                .await
                .is_empty(),
            "the source offset moved past the poison record"
        );
    }

    /// The broker's dead-letter write is no way for a key to write `__` topics
    /// itself: asked directly, the key is still refused there.
    #[tokio::test]
    async fn real_authorizer_a_key_still_cannot_write_the_dlq_itself() {
        let fx = real_auth_bus_service();
        real_topic_with_record(&fx, TopicOptions::default()).await;
        let key = seed_general_key(&fx.db);
        topic_right(&fx.db, &key, "orders.raw", "read");
        topic_right(&fx.db, &key, "orders.raw", "write");

        let dlq_topic = crate::bus::dlq::dlq_topic_name("orders.raw");
        let svc = fx.svc.clone();
        let ctx = key_ctx(&key);
        let result = blocking(move || {
            svc.publish(
                &ctx,
                &dlq_topic,
                PublishBatch {
                    partition: None,
                    producer: None,
                    records: vec![PublishRecord {
                        key: None,
                        headers: vec![],
                        payload: Bytes::from_static(b"{}"),
                        timestamp_ms: 0,
                        schema_id: 0,
                    }],
                },
            )
        })
        .await;
        assert!(matches!(
            result,
            Err(BusServiceError::PermissionDenied { .. })
        ));
    }

    // ---- reconcile ---------------------------------------------------------

    /// A legacy node (no service account) starts nothing and says why.
    #[tokio::test]
    async fn reconcile_refuses_a_flow_without_a_service_account() {
        let fx = real_auth_bus_service();
        let flow_id = seed_consume_flow(
            &fx.db,
            "legacy",
            json!({"instance_id": REAL_INSTANCE, "topic": "orders.raw", "group": "g1"}),
        );
        let status = Arc::new(StatusStore::default());
        let mut reactor =
            BusReactor::with_status(fx.db.clone(), Arc::new(NoopDispatch), status.clone());
        let svc = fx.svc.clone();
        reactor.reconcile_with(&move |_| Some(svc.clone()));
        assert!(reactor.running.is_empty());
        assert_eq!(
            status_of(&status, &flow_id),
            (
                ConsumerState::Refused,
                Some(reason::SERVICE_ACCOUNT_MISSING)
            )
        );
    }

    /// A refused flow starts by itself once the right is granted, with no save.
    #[tokio::test]
    async fn reconcile_starts_a_refused_flow_once_the_right_is_granted() {
        let fx = real_auth_bus_service();
        real_topic_with_record(&fx, TopicOptions::default()).await;
        let key = seed_general_key(&fx.db);
        let flow_id = seed_consume_flow(
            &fx.db,
            "sa-flow",
            json!({
                "instance_id": REAL_INSTANCE, "service_account": key,
                "topic": "orders.raw", "group": "g1"
            }),
        );
        let status = Arc::new(StatusStore::default());
        let mut reactor =
            BusReactor::with_status(fx.db.clone(), Arc::new(NoopDispatch), status.clone());
        let svc = fx.svc.clone();
        let resolve = move |_: &BusInstanceId| Some(svc.clone());

        reactor.reconcile_with(&resolve);
        assert!(reactor.running.is_empty());
        assert_eq!(
            status_of(&status, &flow_id),
            (ConsumerState::Refused, Some(reason::READ_DENIED))
        );

        topic_right(&fx.db, &key, "orders.raw", "read");
        reactor.reconcile_with(&resolve);
        assert!(reactor.running.contains_key(&flow_id));
    }

    /// A save bumps the flow version; a halted subscription stays stopped
    /// until then, and a re-save with the very same config restarts it.
    #[tokio::test]
    async fn reconcile_restarts_a_halted_subscription_only_after_a_resave() {
        let pool = db();
        install_bus_instance(&pool, "tentabus-8b000008", true);
        let key = seed_general_key(&pool);
        let config = json!({
            "instance_id": "tentabus-8b000008", "service_account": key,
            "topic": "t", "group": "gg"
        });
        let flow_id = seed_consume_flow(&pool, "halting", config.clone());
        let status = Arc::new(StatusStore::default());
        let mut reactor =
            BusReactor::with_status(pool.clone(), Arc::new(NoopDispatch), status.clone());

        // Stand in for a task that ended on `on_error = halt`.
        reactor.reconcile();
        let flow = repository::get_flow(&pool, &flow_id).unwrap().unwrap();
        let ended = tokio::spawn(async {});
        while !ended.is_finished() {
            tokio::task::yield_now().await;
        }
        reactor.running.get_mut(&flow_id).unwrap().handle = AbortOnDropHandle::new(ended);
        status.record(&flow_id, ConsumerState::Halted, None, None);

        reactor.reconcile();
        assert!(
            reactor.running[&flow_id].handle.is_finished(),
            "a halted subscription stays stopped while the flow is unchanged"
        );

        repository::update_flow(
            &pool,
            &flow_id,
            flow.version,
            &FlowParams {
                name: "halting",
                description: None,
                is_default: false,
                service_type: Some("chat"),
                flow_json: &flow.flow_json,
                status: "active",
                published_model_name: None,
                actor_user_id: None,
            },
        )
        .expect("re-save with the same config");
        reactor.reconcile();
        assert!(
            !reactor.running[&flow_id].handle.is_finished(),
            "a re-save restarts the subscription"
        );
    }

    #[test]
    fn flows_missing_service_account_names_only_bus_consume_flows_without_one() {
        let consume = |config: serde_json::Value| {
            json!({"nodes": [{"id": "c", "type": "bus_consume", "config": config}], "edges": []})
                .to_string()
        };
        let flows = vec![
            (
                "none".to_string(),
                consume(json!({"topic": "t", "group": "gg"})),
            ),
            (
                "blank".to_string(),
                consume(json!({"service_account": "  "})),
            ),
            (
                "bound".to_string(),
                consume(json!({"service_account": TEST_KEY_UID})),
            ),
            (
                "other".to_string(),
                json!({"nodes": [{"id": "t", "type": "trigger", "config": {}}], "edges": []})
                    .to_string(),
            ),
        ];
        assert_eq!(flows_missing_service_account(&flows), vec!["none", "blank"]);
    }

    #[test]
    fn a_repeated_status_logs_nothing_and_keeps_its_time() {
        let store = StatusStore::default();
        let first = store.set("f", ConsumerState::Refused, Some(reason::READ_DENIED), None);
        assert!(first.changed);
        let again = store.set("f", ConsumerState::Refused, Some(reason::READ_DENIED), None);
        assert!(!again.changed);
        let recovered = store.set("f", ConsumerState::Running, None, None);
        assert!(recovered.changed && recovered.was_blocked);
    }
}
