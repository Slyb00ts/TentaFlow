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
// A `ConsumerHandle` is kept across cycles (`ConsumerSlot`) only while the
// cycle ends clean: `fetch` advances its handle's in-memory `next_offset`
// independently of `commit()` (`commit` only ever clamps `next_offset`
// upward), so a handle that skips `commit()` after a failed batch would never
// re-fetch those records from the SAME handle again. `open_consumer` seeds
// `next_offset` from the durable `committed_offset` at open time, so every
// cycle that did not commit (dispatch failure, DLQ, halt, failed commit, any
// fetch error) drops the handle and the next cycle reopens it: "do not commit
// on failure" still redelivers. A cycle that committed (or `AtMostOnce`, which
// committed inside `fetch`) hands the handle back, saving the per-cycle open.
//
// A kept handle must not outlive its engine or its topic: `open_consumer`
// fixes the partition list, so a reuse first requires the engine to be running
// and each topic to have the incarnation and partition count the handle was
// opened with (`BusService::consumer_is_current`); the subscription loop also
// drops the handle whenever its instance is not running, so a stopped engine's
// partition writers and directory locks are released.
//
// A kept handle must not outlive the service account's authority: every cycle
// that reuses one first repeats the checks the pre-start check makes (the key
// row is active and general, `check_consume_access`: the read right and the
// field-policy lookup). When either fails the handle is dropped and a fresh
// `open_consumer` runs, so the refusal, its reason and the
// `bus.consume.denied` audit row are exactly those of the open-every-cycle
// design. Fetching itself re-authorizes on a permission-generation change as
// before. A handle is also dropped when the subscription resolves a different
// `BusService` (instance restarted).
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
// `batch_size` vs `fetch`'s bounds: the reactor calls
// `ConsumerHandle::fetch_up_to(FETCH_MAX_BYTES, batch_size, ..)`. The record
// bound stops the engine read at about `batch_size` records and moves the
// cursor only past the records returned, so nothing is read twice. The byte
// bound is only a ceiling for batches of very large records.
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
// SLA policy (TB-03, `sla::ServiceGate`): a flow with a policy is consumed under
// its gate. Without one, or with a `max_concurrency` of 1 or none, a cycle is
// what it always was: one fetch, one unit of everything fetched, one dispatch,
// one commit. With `max_concurrency = N` a cycle fetches up to N units of
// `batch_size` records (N fetches, so a backlog spreads over partitions), cuts
// the records into units of one partition each and runs them with at most N in
// flight. `ordering = partition` runs the units of one partition one after
// another and different partitions side by side; `ordering = none` runs every
// unit side by side. Each unit takes a gate permit (`AdmitOrigin::Bus`) around
// its dispatch; the dispatcher is told (`slot_held_by_caller`) not to take a
// second one, so the call is counted once, and still bounds the run by the
// policy's timeout, which the reactor records as the permit's outcome. A gate
// refusal (call window used up, no slot in time) is never a failure of the
// records: the unit sleeps the gate's `retry_after_ms` and asks again, nothing
// is dead-lettered or lost, and the lag grows.
//
// Where the permit is taken depends on the commit mode, because a record that
// is fetched and not run must not be lost. `AutoAfterSuccess`: fetch first,
// then every unit joins the gate's queue like any other caller (fair, so
// /v1 or chat traffic on the same flow cannot starve the bus); the records are
// uncommitted, so a consumer that is held back or lost only has them delivered
// again, and after any wait the unit checks the consumer is still usable
// (engine, rights, leadership) before it dispatches. `AtMostOnce`: the fetch
// commits, so the cycle reserves first (the first permit queued, up to N - 1
// more only if free right now), fetches no more than the permits cover, merges
// the records into at most that many units, and gives the unused permits back
// (`forgo`: no call, no acceptance, no cancellation), so an idle poll leaves
// no trace in the gate. The reserved slots are held through the fetch's long
// poll, which is capped at half the policy's queue timeout (or `max_wait_ms`,
// if shorter) so an idle consumer does not make other callers of the flow time
// out in the queue. A refused reservation ends the cycle after one pause
// (the next cycle asks again, so one throttle episode is counted once), with the cached consumer dropped first, so the subscription loop checks its
// instance again instead of a stopped engine's consumer waiting out the pause.
//
// The commit follows from at-least-once: per partition only the contiguous
// prefix of handled units is committed (`commit_prefix`). A unit is handled when
// the flow succeeded on it, `on_error = skip` gave it up, or every record of it
// is in the dead-letter topic. After a unit that is not (a failure, a dead
// letter still under its delivery attempts, a halt) the rest of its partition
// is delivered again, even units that succeeded in `ordering = none`; in
// `ordering = partition` they are not even started. A halt starts no further
// unit, lets those in flight finish, and commits what precedes the halted one.
// The kept handle follows the same rule as before: it stays only when everything
// it fetched is committed.
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;
use tracing::Instrument;

use crate::agents::{
    AgentPrincipal, FlowDispatcherReactorDispatch, ReactiveDispatchError, ReactorFlowDispatch,
};
use crate::auth::actor::ActorKind;
use crate::bus::correlation;
use crate::bus::dlq::{DlqOutcome, DlqReason};
use crate::bus::groups::CommitMode;
use crate::bus::instance::BusInstanceId;
use crate::bus::{
    BusCallContext, BusService, BusServiceError, ConsumerConfig, ConsumerHandle, FetchedBatch,
    FetchedRecordMeta, TopicPartition,
};
use crate::db::{repository, DbPool};
use crate::dispatch::app_gate;
use crate::flow_engine::dispatcher::{DispatchError, FlowActor, FlowOrigin};
use crate::flow_engine::envelope::{FlowEnvelope, FlowValue};
use crate::flow_engine::node_adapters::bus_consume::{self, ConsumeConfig, OnError};
use crate::flow_engine::types::FlowDefinition;
use crate::sla::{
    AdmitOrigin, BusOrdering, Outcome, ServiceGate, SlaPermit, SlaRefusal, SlaRegistry,
};

/// Byte ceiling passed to `ConsumerHandle::fetch_up_to` on every poll cycle;
/// `config.batch_size` is the record bound that normally ends the read first.
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

/// The longest a unit refused by its SLA gate sleeps before it asks again. The
/// gate's own hint can be as long as a whole call window (a day); asking again
/// sooner lets a raised limit or a changed policy take effect without waiting
/// that out.
const MAX_THROTTLE_PAUSE: Duration = Duration::from_secs(30);

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
    pub const DLQ_UNAVAILABLE: &str = "dlq_unavailable";
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
    /// Units of records the consumer has in flight right now. Counted only for
    /// a flow under an SLA policy; `0` otherwise.
    pub in_flight: u32,
    /// The `max_concurrency` of the flow's SLA policy, as of the last cycle;
    /// `None` without a policy or without a concurrency limit.
    pub max_concurrency: Option<u32>,
    /// Unix milliseconds until which the policy holds the consumer back (its
    /// call limit is used up, or no slot is free); `None` when it is not.
    pub throttled_until: Option<u64>,
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
    /// Flows whose last `AtMostOnce` reservation was refused: the next one asks
    /// again for the same work, so the gate counts one refusal per episode.
    reservation_refused: Mutex<HashSet<String>>,
}

impl StatusStore {
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, ConsumerStatus>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn get(&self, flow_id: &str) -> Option<ConsumerStatus> {
        let mut status = self.map().get(flow_id).cloned()?;
        let now = now_epoch_ms();
        status.throttled_until = status.throttled_until.filter(|until| *until > now);
        Some(status)
    }

    fn reservation_refused(&self, flow_id: &str) -> bool {
        let refused = self
            .reservation_refused
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        refused.contains(flow_id)
    }

    fn set_reservation_refused(&self, flow_id: &str, refused: bool) {
        let mut set = self
            .reservation_refused
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if refused {
            set.insert(flow_id.to_string());
        } else {
            set.remove(flow_id);
        }
    }

    /// Records the concurrency limit the flow's consumer runs under.
    fn set_limit(&self, flow_id: &str, max_concurrency: Option<u32>) {
        if let Some(status) = self.map().get_mut(flow_id) {
            status.max_concurrency = max_concurrency;
        }
    }

    fn add_in_flight(&self, flow_id: &str, delta: i32) {
        if let Some(status) = self.map().get_mut(flow_id) {
            status.in_flight = status.in_flight.saturating_add_signed(delta);
        }
    }

    /// Notes that the policy holds the consumer back until `until_ms`. Read
    /// back only while it lies in the future, so nothing has to clear it. A
    /// consumer held back on its very first cycle has no status yet: it is
    /// created as `Running`, which is what the consumer is once it is let in.
    fn throttle_until(&self, flow_id: &str, until_ms: u64) {
        let mut map = self.map();
        let status = map
            .entry(flow_id.to_string())
            .or_insert_with(|| ConsumerStatus {
                state: ConsumerState::Running,
                reason: None,
                detail: None,
                since_epoch: now_epoch_secs(),
                in_flight: 0,
                max_concurrency: None,
                throttled_until: None,
            });
        status.throttled_until = Some(status.throttled_until.map_or(until_ms, |t| t.max(until_ms)));
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
        // What the cycle reports about its units outlives a change of state.
        let (in_flight, max_concurrency, throttled_until) = previous.map_or((0, None, None), |p| {
            (p.in_flight, p.max_concurrency, p.throttled_until)
        });
        map.insert(
            flow_id.to_string(),
            ConsumerStatus {
                state,
                reason,
                detail,
                since_epoch,
                in_flight,
                max_concurrency,
                throttled_until,
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

fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
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

/// Holds a unit or a cycle back for `retry_after_ms`, the gate's own estimate,
/// and says so in the consumer status. Only the sleep is capped: the status
/// shows the real time, and a raised limit is noticed within the cap.
async fn pause_throttled(status: &StatusStore, flow_id: &str, retry_after_ms: u64) {
    let ms = retry_after_ms.max(1);
    status.throttle_until(flow_id, now_epoch_ms() + ms);
    tokio::time::sleep(Duration::from_millis(ms).min(MAX_THROTTLE_PAUSE)).await;
}

/// Runs `handle.commit` off the async runtime (BLOCKING, see `ConsumerHandle::
/// fetch`'s own doc). The caller decides whether the handle survives into the
/// next cycle (see the module doc).
async fn commit_offsets(
    handle: Arc<ConsumerHandle>,
    offsets: Vec<(TopicPartition, u64)>,
) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || {
        // A dead letter advances the store on its own, possibly past what a
        // prefix says, and `commit` refuses the whole call when one entry lies
        // behind the store. Such an entry is raised to the store: it is
        // covered already, and the handle's cursor follows the store.
        let mut pending = Vec::with_capacity(offsets.len());
        for (tp, offset) in offsets {
            let stored = handle.committed_offset(&tp.topic, tp.partition)?;
            pending.push((tp, offset.max(stored)));
        }
        handle.commit(&pending)
    })
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
) -> anyhow::Result<DlqOutcome> {
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
    .map_err(|e| anyhow::anyhow!("note_delivery_failure failed: {e}"))
}

/// Whether a poll cycle should keep going after `on_error` handling.
enum AfterFailure {
    Continue,
    /// `on_error = halt`: the loop ends and only a save of the flow resumes it.
    /// Carries the failure, which the status shows.
    Halt(String),
    /// The consumer may not read (service account or rights): the loop ends
    /// and the supervisor's pre-start check decides when to try again.
    Refused,
    /// The dead letter could not be written (the `__dlq.*` topic is created on
    /// first use and has no leader for a moment). The record stays uncommitted,
    /// so running the flow on it again at once would repeat its side effects
    /// every cycle: the loop waits `ERROR_BACKOFF` first.
    Backoff,
}

/// The consumer a subscription keeps between poll cycles, with the engine it
/// was opened on.
struct CachedConsumer {
    svc: Arc<BusService>,
    handle: Arc<ConsumerHandle>,
}

type ConsumerSlot = Option<CachedConsumer>;

/// Whether a kept consumer may still be used: the engine and the topic are
/// the ones it was opened on (`BusService::consumer_is_current`: same
/// incarnation, same partition count), and the checks of the pre-start check
/// still pass, repeated per cycle because a kept handle skips
/// `open_consumer`'s authorization. DB reads only, no audit row.
fn still_usable(
    svc: &BusService,
    handle: &ConsumerHandle,
    bctx: &BusCallContext,
    topic: &str,
    group: &str,
    service_account: &str,
) -> bool {
    svc.consumer_is_current(handle)
        && service_account_refusal(svc.db(), service_account).is_none()
        && svc.check_consume_access(bctx, topic, group).is_ok()
}

/// What became of one unit of records.
#[derive(Debug)]
enum UnitFate {
    /// The flow ran on it and succeeded.
    Done,
    /// It failed and `on_error = skip` gave it up.
    Skipped,
    /// It failed and every record of it is in the dead-letter topic.
    DeadLettered,
    /// It failed and stays uncommitted, so it is delivered again: a dead letter
    /// still under its delivery attempts, or one that could not be written
    /// (`dlq_unavailable`, which makes the loop pause).
    Failed { dlq_unavailable: bool },
    /// It failed under `on_error = halt`.
    Halt(String),
    /// It never ran: an earlier unit of its partition did not succeed, or the
    /// cycle was halted first. Delivered again.
    NotRun,
}

impl UnitFate {
    /// Whether the unit's offsets may be committed once everything before it in
    /// its partition is: it succeeded or `on_error` gave it up.
    fn is_handled(&self) -> bool {
        matches!(
            self,
            UnitFate::Done | UnitFate::Skipped | UnitFate::DeadLettered
        )
    }
}

/// Records handed to one flow run. Without a policy (or with a concurrency of
/// one) a cycle has a single unit holding everything the fetch returned, as
/// before; with a concurrency of N it has up to N, each of one partition and at
/// most `batch_size` records.
struct Unit {
    records: Vec<FetchedRecordMeta>,
}

/// The SLA gate a cycle runs under and what the reactor reads off its policy.
struct Governed {
    gate: Arc<ServiceGate>,
    max_concurrency: Option<u32>,
    /// Units in flight at once; `1` when the policy sets no concurrency limit,
    /// which keeps the consumer serial exactly as without a policy.
    parallelism: usize,
    ordering: BusOrdering,
}

impl Governed {
    fn new(gate: Arc<ServiceGate>) -> Self {
        let limits = gate.config().limits;
        Self {
            max_concurrency: limits.max_concurrency,
            parallelism: limits.max_concurrency.map_or(1, |n| (n as usize).max(1)),
            ordering: limits.ordering,
            gate,
        }
    }
}

/// Splits what a cycle fetched into lanes of units. A lane runs its units one
/// after another; lanes run side by side.
///
/// Serial (`parallel` is `None`): one lane with one unit of everything fetched.
/// Otherwise records are grouped by partition in fetch order and cut into units
/// of `batch_size`. With `BusOrdering::Partition` a partition is a lane, so its
/// units keep their order; with `BusOrdering::None` every unit is a lane. Units
/// are listed partition by partition, each partition in offset order, which is
/// the order the commit prefix reads them in.
fn plan_lanes(
    records: Vec<FetchedRecordMeta>,
    parallel: Option<(usize, BusOrdering)>,
) -> Vec<Vec<Unit>> {
    let Some((batch_size, ordering)) = parallel else {
        return vec![vec![Unit { records }]];
    };
    let mut partitions: Vec<Vec<FetchedRecordMeta>> = Vec::new();
    let mut index: HashMap<(String, u32), usize> = HashMap::new();
    for record in records {
        let slot = *index
            .entry((record.topic.clone(), record.partition))
            .or_insert_with(|| {
                partitions.push(Vec::new());
                partitions.len() - 1
            });
        partitions[slot].push(record);
    }
    let mut lanes = Vec::new();
    for records in partitions {
        let mut units = Vec::new();
        let mut remaining = records.into_iter();
        loop {
            let chunk: Vec<_> = remaining.by_ref().take(batch_size.max(1)).collect();
            if chunk.is_empty() {
                break;
            }
            units.push(Unit { records: chunk });
        }
        match ordering {
            BusOrdering::Partition => lanes.push(units),
            BusOrdering::None => lanes.extend(units.into_iter().map(|unit| vec![unit])),
        }
    }
    lanes
}

/// Like [`plan_lanes`], for a cycle that holds `permits` permits and so cannot
/// run more units than that. When the plan has more units (a fetch that spans
/// partitions cuts them short), the records are merged into units of
/// `batch_size` again, which is at most `permits` of them: a cycle reads no
/// more than `permits * batch_size` records. Under `BusOrdering::Partition`
/// the merged units are one lane, so no partition is ever read out of order.
fn plan_fitting(
    records: Vec<FetchedRecordMeta>,
    parallel: Option<(usize, BusOrdering)>,
    permits: usize,
) -> Vec<Vec<Unit>> {
    let lanes = plan_lanes(records, parallel);
    let Some((batch_size, ordering)) = parallel else {
        return lanes;
    };
    if lanes.iter().map(Vec::len).sum::<usize>() <= permits {
        return lanes;
    }
    let mut records = lanes.into_iter().flatten().flat_map(|unit| unit.records);
    let mut units = Vec::new();
    loop {
        let chunk: Vec<_> = records.by_ref().take(batch_size.max(1)).collect();
        if chunk.is_empty() {
            break;
        }
        units.push(Unit { records: chunk });
    }
    match ordering {
        BusOrdering::Partition => vec![units],
        BusOrdering::None => units.into_iter().map(|unit| vec![unit]).collect(),
    }
}

/// The gate permits an `AtMostOnce` cycle took before it fetched. What is left
/// when it is dropped is given back with `forgo`, so a permit that never ran a
/// unit is neither a call, nor an acceptance, nor a cancellation.
struct Reservation {
    permits: Vec<SlaPermit>,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        for permit in self.permits.drain(..) {
            permit.forgo();
        }
    }
}

/// Takes up to `wanted` permits of `gate`: the first one the way every caller
/// does, queued, and the others only if they are free right now. A refusal of
/// the first one is the gate's answer to the whole cycle. `again`: the previous
/// cycle was refused, so a refusal now is not counted a second time.
async fn reserve(
    gate: &Arc<ServiceGate>,
    wanted: usize,
    again: bool,
) -> Result<Reservation, SlaRefusal> {
    let first = if again {
        gate.admit_again(AdmitOrigin::Bus).await?
    } else {
        gate.admit(AdmitOrigin::Bus).await?
    };
    let mut held = Reservation {
        permits: vec![first],
    };
    while held.permits.len() < wanted {
        match gate.try_admit_more(AdmitOrigin::Bus).await {
            Ok(permit) => held.permits.push(permit),
            Err(_) => break,
        }
    }
    Ok(held)
}

/// The offsets a cycle may commit: per partition, the contiguous prefix of
/// handled units. `units` lists each partition's units in offset order. A unit
/// that was not handled blocks every partition it touches, so a later success
/// of that partition is not committed and is delivered again (at-least-once).
fn commit_prefix(units: &[(&[FetchedRecordMeta], bool)]) -> Vec<(TopicPartition, u64)> {
    let mut blocked: HashSet<(String, u32)> = HashSet::new();
    let mut next: HashMap<(String, u32), u64> = HashMap::new();
    for (records, handled) in units {
        let ends = end_offsets(records);
        let key = |tp: &TopicPartition| (tp.topic.clone(), tp.partition);
        if *handled && ends.iter().all(|(tp, _)| !blocked.contains(&key(tp))) {
            for (tp, end) in &ends {
                next.insert(key(tp), *end);
            }
        } else {
            blocked.extend(ends.iter().map(|(tp, _)| key(tp)));
        }
    }
    next.into_iter()
        .map(|((topic, partition), offset)| (TopicPartition { topic, partition }, offset))
        .collect()
}

/// Counts a unit as in flight in the consumer status while it runs.
struct InFlight<'a> {
    status: &'a StatusStore,
    flow_id: &'a str,
}

impl<'a> InFlight<'a> {
    fn enter(status: &'a StatusStore, flow_id: &'a str) -> Self {
        status.add_in_flight(flow_id, 1);
        Self { status, flow_id }
    }
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.status.add_in_flight(self.flow_id, -1);
    }
}

/// Everything the units of one cycle share.
///
/// Lanes are polled together by one `join_all` on the cycle's task: they are
/// concurrent, not parallel. That suits flows that wait on I/O (model calls,
/// HTTP); a dispatch that computes or blocks its thread holds the other lanes
/// back while it does.
struct CycleCtx<'a> {
    svc: &'a Arc<BusService>,
    handle: &'a Arc<ConsumerHandle>,
    bctx: &'a BusCallContext,
    flow_id: &'a str,
    config: &'a ConsumeConfig,
    group: &'a str,
    service_account: &'a str,
    dispatch: &'a Arc<dyn ReactorFlowDispatch>,
    status: &'a StatusStore,
    span: &'a tracing::Span,
    /// Where the flow's gate is looked up again whenever a unit asks for a
    /// permit, so a policy that changed meanwhile is followed.
    sla: Option<&'static SlaRegistry>,
    /// The gate the cycle started under (`None`: no policy).
    governed: Option<&'a Governed>,
    /// Bounds the units in flight at once when there are several lanes: the
    /// policy's concurrency limit.
    slots: Option<Semaphore>,
    /// The permits an `AtMostOnce` cycle took before it fetched, one per unit.
    reserved: Option<Mutex<Reservation>>,
    /// Set by a halt: no unit starts after it. Never set under `AtMostOnce`,
    /// whose records are committed already: every fetched unit runs, and the
    /// halt takes effect once they all have.
    stop: AtomicBool,
}

/// How a unit's request for a permit ended.
enum Admission {
    Permit(SlaPermit),
    /// The flow has no gate any more: the unit runs without a permit.
    Untracked,
    /// The cycle was halted or the consumer is no longer usable: the unit does
    /// not run and is delivered again.
    Stopped,
}

impl CycleCtx<'_> {
    fn at_most_once(&self) -> bool {
        self.config.commit_mode == CommitMode::AtMostOnce
    }

    /// Runs the units of one lane in order; a unit that is not handled ends the
    /// lane (the rest of its partition would only be delivered again). Under
    /// `AtMostOnce` nothing is delivered again, so a failure ends nothing.
    /// Returns one fate per unit.
    async fn run_lane(&self, units: &[Unit]) -> Vec<UnitFate> {
        let mut fates = Vec::with_capacity(units.len());
        for unit in units {
            let fate = if self.stop.load(Ordering::Acquire) {
                UnitFate::NotRun
            } else {
                self.run_unit(unit).await
            };
            let go_on = fate.is_handled() || self.at_most_once();
            fates.push(fate);
            if !go_on {
                break;
            }
        }
        fates.resize_with(units.len(), || UnitFate::NotRun);
        fates
    }

    async fn run_unit(&self, unit: &Unit) -> UnitFate {
        let records = &unit.records;
        let decoded: Result<Vec<serde_json::Value>, serde_json::Error> = records
            .iter()
            .map(|r| serde_json::from_slice::<serde_json::Value>(&r.payload))
            .collect();
        let values = match decoded {
            Ok(values) => values,
            Err(e) => {
                let msg = format!("payload is not valid JSON: {e}");
                return self
                    .fail(records, DlqReason::SchemaViolation, &msg, None)
                    .await;
            }
        };

        let batch_corr = resolve_batch_correlation(records);
        // Units that run side by side must not overwrite each other's id on the
        // cycle's span, so each gets a span of its own.
        let span = if self.slots.is_some() {
            tracing::info_span!(parent: self.span, "bus_reactor_unit", correlation_id = %batch_corr.id)
        } else {
            self.span.record("correlation_id", batch_corr.id.as_str());
            self.span.clone()
        };
        self.run_decoded(records, &values, &batch_corr)
            .instrument(span)
            .await
    }

    async fn run_decoded(
        &self,
        records: &[FetchedRecordMeta],
        values: &[serde_json::Value],
        batch_corr: &BatchCorrelation,
    ) -> UnitFate {
        let _slot = match &self.slots {
            Some(slots) => slots.acquire().await.ok(),
            None => None,
        };
        if self.stop.load(Ordering::Acquire) {
            return UnitFate::NotRun;
        }
        let mut envelope = build_envelope(self.config, records, values);
        apply_correlation_meta(&mut envelope, batch_corr);
        let principal = AgentPrincipal::new(
            None,
            Some(self.config.org_id.clone()),
            FlowOrigin::Bus,
            FlowActor::api_key(self.service_account, None),
        )
        .with_correlation_id(Some(batch_corr.id.clone()));

        let permit = if self.governed.is_some() {
            match self.admit().await {
                Admission::Permit(permit) => Some(permit),
                Admission::Untracked => None,
                Admission::Stopped => return UnitFate::NotRun,
            }
        } else {
            None
        };
        let in_flight = self
            .governed
            .map(|_| InFlight::enter(self.status, self.flow_id));
        let result = self
            .dispatch
            .dispatch(self.flow_id.to_string(), envelope, principal)
            .await;
        drop(in_flight);
        let error = match result {
            Ok(()) => {
                if let Some(permit) = permit {
                    permit.finish(Outcome::Ok);
                }
                return UnitFate::Done;
            }
            Err(error) => error,
        };
        match error.downcast_ref::<ReactiveDispatchError>().map(|e| &e.0) {
            Some(DispatchError::TimedOut { .. }) => {
                if let Some(permit) = permit {
                    permit.finish(Outcome::Timeout);
                }
            }
            // The run never started: it costs the policy nothing.
            Some(DispatchError::CompileFailed { .. }) => {
                if let Some(permit) = permit {
                    permit.forgo();
                }
            }
            _ => {
                if let Some(permit) = permit {
                    permit.finish(Outcome::Error);
                }
            }
        }
        self.fail(
            records,
            DlqReason::ConsumerError,
            &error.to_string(),
            Some(batch_corr.id.as_str()),
        )
        .await
    }

    /// The flow's gate as it is now.
    fn current_gate(&self) -> Option<Arc<ServiceGate>> {
        self.sla.and_then(|registry| {
            let _ = registry.refresh_if_stale(self.svc.db());
            registry.gate(self.flow_id)
        })
    }

    /// The permit for one unit. An `AtMostOnce` cycle took its permits before it
    /// fetched, so a unit just takes one of them. Otherwise the unit joins the
    /// gate's queue like any caller. A refusal (the call window is used up, or
    /// no slot freed in time) is never a failure of the records: the unit waits
    /// out the gate's `retry_after_ms` and asks again, so nothing is
    /// dead-lettered or lost and the lag simply grows. The gate is looked up
    /// again after every pause, and the refusal is counted once. After any wait
    /// the consumer may have lost its engine, its rights or its leadership; if
    /// so the permit goes back unused and the unit is delivered again.
    async fn admit(&self) -> Admission {
        if let Some(reserved) = &self.reserved {
            let permit = reserved
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .permits
                .pop();
            return permit.map_or(Admission::Untracked, |mut permit| {
                // Held since before the fetch: the unit's run is what is timed.
                permit.begin();
                Admission::Permit(permit)
            });
        }
        let mut refused = false;
        let mut waited = false;
        loop {
            if self.stop.load(Ordering::Acquire) {
                return Admission::Stopped;
            }
            let Some(gate) = self.current_gate() else {
                return Admission::Untracked;
            };
            let admitted = if refused {
                gate.admit_again(AdmitOrigin::Bus).await
            } else {
                // A free slot is taken at once, with nothing to recheck.
                match gate.try_admit_more(AdmitOrigin::Bus).await {
                    Ok(permit) => Ok(permit),
                    Err(_) => {
                        waited = true;
                        gate.admit(AdmitOrigin::Bus).await
                    }
                }
            };
            match admitted {
                Ok(permit) => {
                    if !waited || self.still_usable_now().await {
                        return Admission::Permit(permit);
                    }
                    permit.forgo();
                    return Admission::Stopped;
                }
                Err(refusal) => {
                    tracing::debug!(
                        "bus reactor: flow '{}' is held back by its SLA policy ({}), \
                         retrying in {} ms",
                        self.flow_id,
                        refusal.reason.code(),
                        refusal.retry_after_ms
                    );
                    refused = true;
                    waited = true;
                    pause_throttled(self.status, self.flow_id, refusal.retry_after_ms).await;
                    if !self.still_usable_now().await {
                        return Admission::Stopped;
                    }
                }
            }
        }
    }

    /// Whether the cycle's consumer may still read: its engine, its rights and
    /// its leadership. DB reads, off the async runtime.
    async fn still_usable_now(&self) -> bool {
        let svc = self.svc.clone();
        let handle = self.handle.clone();
        let bctx = self.bctx.clone();
        let topic = self.config.topic.clone();
        let group = self.group.to_string();
        let account = self.service_account.to_string();
        tokio::task::spawn_blocking(move || {
            still_usable(&svc, &handle, &bctx, &topic, &group, &account)
                && handle.check_leadership().is_ok()
        })
        .await
        .unwrap_or(false)
    }

    /// Routes a failed unit (a JSON decode failure OR a failed flow dispatch)
    /// through `config.on_error` (`bus_consume::OnError`'s own doc).
    async fn fail(
        &self,
        records: &[FetchedRecordMeta],
        reason: DlqReason,
        message: &str,
        run_correlation_id: Option<&str>,
    ) -> UnitFate {
        let flow_id = self.flow_id;
        match self.config.on_error {
            OnError::Halt => {
                tracing::warn!(
                    "bus reactor: flow '{flow_id}' halted ({reason:?}): {message} — an operator \
                     must fix and republish the flow to resume"
                );
                if !self.at_most_once() {
                    self.stop.store(true, Ordering::Release);
                }
                UnitFate::Halt(message.to_string())
            }
            OnError::Skip => {
                tracing::warn!(
                    "bus reactor: flow '{flow_id}' skipping {} record(s) ({reason:?}): {message}",
                    records.len()
                );
                UnitFate::Skipped
            }
            OnError::Dlq => {
                let mut all_dead_lettered = true;
                let mut dlq_failed = false;
                for record in records {
                    // The record's own chain; a legacy record without one joins
                    // the run it failed in (when a run was resolved).
                    let mut record_ctx = self.bctx.clone();
                    record_ctx.correlation_id = correlation::from_fetched_headers(&record.headers)
                        .or_else(|| run_correlation_id.map(str::to_string));
                    let outcome = note_failure(
                        self.svc.clone(),
                        record_ctx,
                        self.group.to_string(),
                        record.clone(),
                        reason,
                        message.to_string(),
                    )
                    .await;
                    match outcome {
                        Ok(DlqOutcome::Retry { .. }) => all_dead_lettered = false,
                        Ok(_) => {}
                        Err(e) => {
                            dlq_failed = true;
                            tracing::warn!(
                                "bus reactor: flow '{flow_id}' note_delivery_failure failed for \
                                 offset {}: {e}",
                                record.offset
                            );
                        }
                    }
                }
                if dlq_failed {
                    UnitFate::Failed {
                        dlq_unavailable: true,
                    }
                } else if all_dead_lettered {
                    UnitFate::DeadLettered
                } else {
                    UnitFate::Failed {
                        dlq_unavailable: false,
                    }
                }
            }
        }
    }
}

/// One full poll cycle for a subscription: reuses the kept `ConsumerHandle`
/// (or opens one), fetches, and either dispatches the decoded records reactively or routes a
/// failure through `on_error` — see this module's doc for the full design.
/// Takes `svc` as a parameter (rather than resolving `config.instance_id`
/// itself, which `subscription_loop` does before calling this) so it is
/// directly testable against a locally-constructed `BusService`, without
/// touching the process-wide instance registry every other test in this
/// crate's binary also shares.
///
/// `sla` is the registry the flow's gate is looked up in (`None`: no policy
/// applies, whatever the registry says).
async fn run_governed_cycle(
    svc: &Arc<BusService>,
    flow_id: &str,
    config: &ConsumeConfig,
    dispatch: &Arc<dyn ReactorFlowDispatch>,
    status: &StatusStore,
    slot: &mut ConsumerSlot,
    sla: Option<&'static SlaRegistry>,
) -> AfterFailure {
    let span = tracing::info_span!(
        "bus_reactor_cycle",
        flow_id = %flow_id,
        topic = %config.topic,
        correlation_id = tracing::field::Empty,
    );
    let after = run_cycle_in(svc, flow_id, config, dispatch, status, slot, sla, &span)
        .instrument(span.clone())
        .await;
    match after {
        AfterFailure::Halt(ref failure) => status.record(
            flow_id,
            ConsumerState::Halted,
            None,
            Some(failure.chars().take(300).collect()),
        ),
        AfterFailure::Backoff => status.record(
            flow_id,
            ConsumerState::Waiting,
            Some(reason::DLQ_UNAVAILABLE),
            None,
        ),
        _ => {}
    }
    after
}

/// [`run_governed_cycle`] for a flow no policy governs.
#[cfg(test)]
async fn run_cycle(
    svc: &Arc<BusService>,
    flow_id: &str,
    config: &ConsumeConfig,
    dispatch: &Arc<dyn ReactorFlowDispatch>,
    status: &StatusStore,
    slot: &mut ConsumerSlot,
) -> AfterFailure {
    run_governed_cycle(svc, flow_id, config, dispatch, status, slot, None).await
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

/// What one cycle's fetches returned.
struct CycleFetch {
    records: Vec<FetchedRecordMeta>,
    /// Every fetch the cycle wanted succeeded. When one failed after others had
    /// advanced the handle's cursor, the handle is not kept: a reopen starts
    /// from the committed offset.
    clean: bool,
}

/// Fetches what one cycle works on: one fetch of up to `batch_size` records,
/// and, for a consumer that runs up to `fetches` units at once, up to
/// `fetches - 1` more while each returns a full unit. Every call returns the
/// records of the partitions it starts at, so the extra calls are what spread
/// a backlog across partitions; they never wait. `fetches == 1` is the single
/// fetch of a consumer without a policy.
fn fetch_cycle(
    handle: &ConsumerHandle,
    batch_size: usize,
    fetches: usize,
    max_wait_ms: u32,
) -> Result<CycleFetch, BusServiceError> {
    fetch_cycle_with(
        &mut |max_bytes, max_wait_ms| handle.fetch_up_to(max_bytes, batch_size, max_wait_ms),
        batch_size,
        fetches,
        max_wait_ms,
    )
}

/// [`fetch_cycle`] over any fetch function `(max_bytes, max_wait_ms)`.
fn fetch_cycle_with(
    fetch: &mut dyn FnMut(usize, u32) -> Result<FetchedBatch, BusServiceError>,
    batch_size: usize,
    fetches: usize,
    max_wait_ms: u32,
) -> Result<CycleFetch, BusServiceError> {
    let mut records = fetch(FETCH_MAX_BYTES, max_wait_ms)?.records;
    let mut bytes: usize = records.iter().map(|r| r.payload.len()).sum();
    let mut last = records.len();
    for _ in 1..fetches {
        if last < batch_size || bytes >= FETCH_MAX_BYTES {
            break;
        }
        match fetch(FETCH_MAX_BYTES - bytes, 0) {
            Ok(batch) => {
                last = batch.records.len();
                bytes += batch.records.iter().map(|r| r.payload.len()).sum::<usize>();
                records.extend(batch.records);
            }
            Err(e) => {
                tracing::warn!("bus reactor: a top-up fetch failed, working on what was read: {e}");
                return Ok(CycleFetch {
                    records,
                    clean: false,
                });
            }
        }
    }
    Ok(CycleFetch {
        records,
        clean: true,
    })
}

#[allow(clippy::too_many_arguments)]
async fn run_cycle_in(
    svc: &Arc<BusService>,
    flow_id: &str,
    config: &ConsumeConfig,
    dispatch: &Arc<dyn ReactorFlowDispatch>,
    status: &StatusStore,
    slot: &mut ConsumerSlot,
    sla: Option<&'static SlaRegistry>,
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

    // The gate is looked up afresh every cycle; a failed policy load is logged
    // and rate limited by the registry, which keeps serving the last good gates.
    let governed = sla
        .and_then(|registry| {
            let _ = registry.refresh_if_stale(svc.db());
            registry.gate(flow_id)
        })
        .map(Governed::new);
    let kept = slot
        .take()
        .filter(|c| Arc::ptr_eq(&c.svc, svc))
        .map(|c| c.handle);

    // `AtMostOnce` commits inside the fetch, so a record fetched and not run is
    // lost: the cycle takes its permits first and fetches no more than they
    // cover. The slots are held through the fetch's long poll; the permits the
    // fetch leaves unused are given back, and an idle poll leaves no trace in
    // the gate. `AutoAfterSuccess` fetches first: its records stay uncommitted,
    // so a consumer that is held back or lost only has them delivered again,
    // and every unit joins the gate's queue for its own permit.
    let mut reservation = None;
    let mut fetches = governed.as_ref().map_or(1, |g| g.parallelism);
    let mut max_wait_ms = config.max_wait_ms;
    if let Some(governed) = governed
        .as_ref()
        .filter(|_| config.commit_mode == CommitMode::AtMostOnce)
    {
        let again = status.reservation_refused(flow_id);
        match reserve(&governed.gate, governed.parallelism, again).await {
            Ok(held) => {
                status.set_reservation_refused(flow_id, false);
                fetches = held.permits.len();
                reservation = Some(held);
                // The held slots keep other callers of the flow queued for as
                // long as the fetch polls, so it polls for at most half of what
                // a queued caller waits before it is refused.
                let queue_timeout = governed.gate.config().limits.queue_timeout;
                if !queue_timeout.is_zero() {
                    let half = u32::try_from(queue_timeout.as_millis() / 2).unwrap_or(u32::MAX);
                    max_wait_ms = max_wait_ms.min(half.max(1));
                }
            }
            Err(refusal) => {
                status.set_reservation_refused(flow_id, true);
                // The cached consumer goes before the pause, and the cycle ends
                // after it: the subscription loop checks its instance again
                // instead of this cycle sitting on a stopped engine's consumer.
                drop(kept);
                tracing::debug!(
                    "bus reactor: flow '{flow_id}' is held back by its SLA policy ({}), \
                     retrying in {} ms",
                    refusal.reason.code(),
                    refusal.retry_after_ms
                );
                pause_throttled(status, flow_id, refusal.retry_after_ms).await;
                return AfterFailure::Continue;
            }
        }
    }

    let (handle, fetched) = {
        let task_svc = svc.clone();
        let open_bctx = bctx.clone();
        let group = group.clone();
        let topic = config.topic.clone();
        let commit_mode = config.commit_mode;
        let batch_size = config.batch_size as usize;
        let task_account = service_account.to_string();
        let joined = tokio::task::spawn_blocking(move || {
            let kept = kept.filter(|handle| {
                still_usable(&task_svc, handle, &open_bctx, &topic, &group, &task_account)
            });
            let handle = match kept {
                Some(handle) => handle,
                None => Arc::new(task_svc.open_consumer(
                    &open_bctx,
                    &group,
                    &[topic],
                    ConsumerConfig { commit_mode },
                )?),
            };
            let fetched = fetch_cycle(&handle, batch_size, fetches, max_wait_ms)?;
            Ok::<_, BusServiceError>((handle, fetched))
        })
        .await;
        match joined {
            Ok(Ok((handle, fetched))) => {
                status.record(flow_id, ConsumerState::Running, None, None);
                (handle, fetched)
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
    let CycleFetch { records, clean } = fetched;
    status.set_limit(flow_id, governed.as_ref().and_then(|g| g.max_concurrency));

    if records.is_empty() {
        // `fetch` already long-polled up to `max_wait_ms` internally.
        restore_slot(slot, svc, Some(handle));
        return AfterFailure::Continue;
    }

    let parallel = governed
        .as_ref()
        .filter(|g| g.parallelism > 1)
        .map(|g| (config.batch_size as usize, g.ordering));
    let lanes = match &mut reservation {
        Some(held) => {
            let lanes = plan_fitting(records, parallel, held.permits.len());
            let needed: usize = lanes.iter().map(Vec::len).sum();
            for spare in held.permits.split_off(needed.min(held.permits.len())) {
                spare.forgo();
            }
            lanes
        }
        None => plan_lanes(records, parallel),
    };
    let ctx = CycleCtx {
        svc,
        handle: &handle,
        bctx: &bctx,
        flow_id,
        config,
        group: &group,
        service_account,
        dispatch,
        status,
        span,
        sla,
        governed: governed.as_ref(),
        slots: parallel.map(|_| Semaphore::new(fetches)),
        reserved: reservation.map(Mutex::new),
        stop: AtomicBool::new(false),
    };
    let fates: Vec<Vec<UnitFate>> =
        futures::future::join_all(lanes.iter().map(|units| ctx.run_lane(units))).await;
    let units: Vec<(&Unit, &UnitFate)> =
        lanes.iter().flatten().zip(fates.iter().flatten()).collect();

    let auto_commit = config.commit_mode == CommitMode::AutoAfterSuccess;
    let mut committed = true;
    if auto_commit {
        let handled: Vec<(&[FetchedRecordMeta], bool)> = units
            .iter()
            .map(|(unit, fate)| (unit.records.as_slice(), fate.is_handled()))
            .collect();
        let offsets = commit_prefix(&handled);
        if !offsets.is_empty() {
            if let Err(e) = commit_offsets(handle.clone(), offsets).await {
                tracing::warn!("bus reactor: flow '{flow_id}' commit failed: {e}");
                committed = false;
            }
        }
    }
    // `AtMostOnce` already committed inside `fetch()`; `Explicit` cannot reach
    // here — `ConsumeConfig::from_config` rejects it, so `build_subscriptions`
    // never builds such a subscription. See this module's doc.
    //
    // The handle's cursor is past everything fetched. It may stay only when
    // all of that is also committed (or, for `AtMostOnce`, was never to be).
    let advanced = units.iter().all(|(_, fate)| {
        matches!(fate, UnitFate::Done) || (auto_commit && matches!(fate, UnitFate::Skipped))
    });
    restore_slot(
        slot,
        svc,
        (clean && committed && advanced).then_some(handle),
    );

    if let Some(failure) = units.iter().find_map(|(_, fate)| match fate {
        UnitFate::Halt(message) => Some(message.clone()),
        _ => None,
    }) {
        return AfterFailure::Halt(failure);
    }
    let dlq_unavailable = units.iter().any(|(_, fate)| {
        matches!(
            fate,
            UnitFate::Failed {
                dlq_unavailable: true
            }
        )
    });
    if dlq_unavailable {
        AfterFailure::Backoff
    } else {
        AfterFailure::Continue
    }
}

/// Keeps `handle` for the next cycle, when the cycle left one worth keeping.
fn restore_slot(
    slot: &mut ConsumerSlot,
    svc: &Arc<BusService>,
    handle: Option<Arc<ConsumerHandle>>,
) {
    *slot = handle.map(|handle| CachedConsumer {
        svc: svc.clone(),
        handle,
    });
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
    let mut slot = ConsumerSlot::None;
    loop {
        let Some(svc) = crate::bus::instance(&config.instance_id) else {
            // Disabled, uninstalled, or not started yet: back off. The kept
            // consumer goes first: it holds the stopped engine's partition
            // writers and directory locks, and a re-init of the instance on
            // the same directory would fail while it lives.
            slot = ConsumerSlot::None;
            status.record(
                &flow_id,
                ConsumerState::Waiting,
                Some(reason::INSTANCE_NOT_RUNNING),
                None,
            );
            tokio::time::sleep(ERROR_BACKOFF).await;
            continue;
        };
        let after = run_governed_cycle(
            &svc,
            &flow_id,
            &config,
            &dispatch,
            &status,
            &mut slot,
            Some(crate::sla::global()),
        )
        .await;
        if !continue_after(after).await {
            break;
        }
    }
}

/// What the subscription loop does with a finished cycle: `false` ends it, and
/// a `Backoff` waits `ERROR_BACKOFF` before the next cycle runs the flow on the
/// same record again.
async fn continue_after(after: AfterFailure) -> bool {
    match after {
        AfterFailure::Continue => true,
        AfterFailure::Backoff => {
            tokio::time::sleep(ERROR_BACKOFF).await;
            true
        }
        AfterFailure::Halt(_) | AfterFailure::Refused => false,
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
        always_fail: std::sync::atomic::AtomicBool,
    }

    impl ScriptedDispatch {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                fail_next: std::sync::atomic::AtomicBool::new(false),
                always_fail: std::sync::atomic::AtomicBool::new(false),
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
                || self.always_fail.load(std::sync::atomic::Ordering::SeqCst)
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
        test_bus_service_with(Arc::new(AllowAllAuthorizer))
    }

    fn test_bus_service_with(
        authorizer: Arc<dyn BusAuthorizer>,
    ) -> (tempfile::TempDir, Arc<BusService>) {
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
                authorizer,
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

    /// Allows everything except producing to a dead-letter topic: stands in for
    /// a `__dlq.*` topic that is not writable yet (created on first use, no
    /// leader for a moment).
    struct DenyDeadLetterWrites;
    impl BusAuthorizer for DenyDeadLetterWrites {
        fn authorize(
            &self,
            _ctx: &BusCallContext,
            action: BusAction,
            topic: &str,
        ) -> Result<(), BusServiceError> {
            if action == BusAction::Produce && topic.starts_with("__dlq.") {
                return Err(BusServiceError::PermissionDenied {
                    action: "produce",
                    topic: topic.to_string(),
                });
            }
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

    /// A dead letter that cannot be written yet must not turn into a hot loop:
    /// the record stays uncommitted, so without a pause every cycle ran the flow
    /// (and its side effects) on it again, hundreds of times a second. The
    /// cycle reports `Backoff`, the status says why, and the loop waits.
    #[tokio::test]
    async fn an_unwritable_dead_letter_backs_off_instead_of_rerunning_the_flow() {
        let (_dir, svc) = test_bus_service_with(Arc::new(DenyDeadLetterWrites));
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
            .always_fail
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let dispatch: Arc<dyn ReactorFlowDispatch> = scripted.clone();
        let config = ConsumeConfig::from_config(&json!({
            "instance_id": "tentabus-00000001", "service_account": TEST_KEY_UID, "topic": "orders.raw", "group": "g1", "max_wait_ms": 20, "on_error": "dlq"
        }))
        .unwrap();
        let status = StatusStore::default();

        let started = std::time::Instant::now();
        let mut cycles = 0;
        while started.elapsed() < Duration::from_millis(ERROR_BACKOFF.as_millis() as u64 + 500) {
            let after = run_cycle(
                &svc,
                "flow-dlq",
                &config,
                &dispatch,
                &status,
                &mut ConsumerSlot::None,
            )
            .await;
            assert!(matches!(after, AfterFailure::Backoff), "cycle {cycles}");
            assert_eq!(
                status_of(&status, "flow-dlq"),
                (ConsumerState::Waiting, Some(reason::DLQ_UNAVAILABLE))
            );
            cycles += 1;
            if !continue_after(after).await {
                break;
            }
        }
        let runs = scripted.calls.lock().unwrap().len();
        assert!(
            (1..=3).contains(&runs) && runs == cycles,
            "the flow ran {runs} times in {cycles} cycles over {:?}",
            started.elapsed()
        );
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
        let svc =
            registry_bus_service_at(id, dir.path().join("bus")).expect("init registry instance");
        (dir, svc)
    }

    /// `registry_bus_service` on a given bus directory, with the init result
    /// visible (a second init on a directory still locked fails).
    fn registry_bus_service_at(
        id: BusInstanceId,
        bus_dir: std::path::PathBuf,
    ) -> Result<Arc<BusService>, BusServiceError> {
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
        crate::db::repository::bus_test_support::create_bus_tables(&db).expect("bus tables");
        let local_conn = rusqlite::Connection::open_in_memory().expect("open local db");
        crate::bus::db::migrate(&local_conn).expect("migrate local db");
        let local_db: crate::db::DbPool = Arc::new(crate::db::Db::from_connection(local_conn));
        crate::bus::init_instance(BusInitConfig {
            instance_id: id,
            local_db,
            bus_dir,
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

        let after = run_cycle(
            &svc,
            "flow-1",
            &config,
            &dispatch,
            &StatusStore::default(),
            &mut ConsumerSlot::None,
        )
        .await;
        assert!(matches!(after, AfterFailure::Continue));
        {
            let calls = scripted.calls.lock().unwrap();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0], ("flow-1".to_string(), json!({"id": 1})));
        }

        // Second cycle: nothing new (short max_wait_ms keeps this fast).
        let after2 = run_cycle(
            &svc,
            "flow-1",
            &config,
            &dispatch,
            &StatusStore::default(),
            &mut ConsumerSlot::None,
        )
        .await;
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

        let after = run_cycle(
            &svc,
            "flow-1",
            &config,
            &dispatch,
            &StatusStore::default(),
            &mut ConsumerSlot::None,
        )
        .await;
        assert!(matches!(after, AfterFailure::Halt(_)));

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

        let after = run_cycle(
            &svc,
            "flow-1",
            &config,
            &dispatch,
            &StatusStore::default(),
            &mut ConsumerSlot::None,
        )
        .await;
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
        run_cycle(
            &svc,
            "flow-1",
            &config,
            &dispatch,
            &StatusStore::default(),
            &mut ConsumerSlot::None,
        )
        .await;

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
        run_cycle(
            &svc,
            "flow-1",
            &config,
            &dispatch,
            &StatusStore::default(),
            &mut ConsumerSlot::None,
        )
        .await;

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
        run_cycle(
            &svc,
            "flow-1",
            &config,
            &dispatch,
            &StatusStore::default(),
            &mut ConsumerSlot::None,
        )
        .await;

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
        run_cycle(
            &svc,
            "flow-1",
            &config,
            &dispatch,
            &StatusStore::default(),
            &mut ConsumerSlot::None,
        )
        .await;

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
        let after = run_cycle(
            &svc,
            "flow-1",
            &config,
            &dispatch,
            &StatusStore::default(),
            &mut ConsumerSlot::None,
        )
        .await;
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

        let after = run_cycle(
            &fx.svc,
            "flow-real-ok",
            &config,
            &dispatch,
            &status,
            &mut ConsumerSlot::None,
        )
        .await;
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
        run_cycle(
            &fx.svc,
            "flow-real-ok",
            &config,
            &dispatch,
            &status,
            &mut ConsumerSlot::None,
        )
        .await;
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
            &mut ConsumerSlot::None,
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
            &mut ConsumerSlot::None,
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
        let first = run_cycle(
            &fx.svc,
            "flow-real-revoked",
            &config,
            &dispatch,
            &status,
            &mut ConsumerSlot::None,
        )
        .await;
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
        let second = run_cycle(
            &fx.svc,
            "flow-real-revoked",
            &config,
            &dispatch,
            &status,
            &mut ConsumerSlot::None,
        )
        .await;
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

    /// Records the `id` of every record in every run, in order.
    struct IdSpy {
        runs: Mutex<Vec<Vec<i64>>>,
        fail: bool,
    }

    impl IdSpy {
        fn new(fail: bool) -> Arc<Self> {
            Arc::new(Self {
                runs: Mutex::new(Vec::new()),
                fail,
            })
        }
    }

    #[async_trait]
    impl ReactorFlowDispatch for IdSpy {
        async fn dispatch(
            &self,
            _flow_id: String,
            initial: FlowEnvelope,
            _principal: AgentPrincipal,
        ) -> anyhow::Result<()> {
            let FlowValue::Json(value) = initial.payload else {
                anyhow::bail!("expected a JSON payload");
            };
            let ids = match value {
                serde_json::Value::Array(items) => items.iter().map(|v| v["id"].as_i64()).collect(),
                single => vec![single["id"].as_i64()],
            };
            self.runs
                .lock()
                .unwrap()
                .push(ids.into_iter().map(|id| id.unwrap_or(-1)).collect());
            if self.fail {
                anyhow::bail!("scripted dispatch failure");
            }
            Ok(())
        }
    }

    async fn publish_ids(fx: &RealAuth, ids: std::ops::RangeInclusive<i64>) {
        for id in ids {
            publish_one(
                &fx.svc,
                &test_ctx("org-default"),
                "orders.raw",
                &format!(r#"{{"id":{id}}}"#),
            )
            .await;
        }
    }

    /// The kept consumer reads each record once: `batch_size` bounds the
    /// fetch and the cursor stops after the last record returned.
    #[tokio::test]
    async fn real_authorizer_a_kept_consumer_reads_each_record_once_in_batches() {
        let fx = real_auth_bus_service();
        real_topic_with_record(&fx, TopicOptions::default()).await;
        publish_ids(&fx, 2..=5).await;
        let key = seed_general_key(&fx.db);
        topic_right(&fx.db, &key, "orders.raw", "read");

        let spy = IdSpy::new(false);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let status = StatusStore::default();
        let config = consume_config(&key, json!({"batch_size": 2}));
        let mut slot = ConsumerSlot::None;
        for _ in 0..4 {
            let after =
                run_cycle(&fx.svc, "flow-kept", &config, &dispatch, &status, &mut slot).await;
            assert!(matches!(after, AfterFailure::Continue));
            assert!(slot.is_some(), "a clean cycle keeps the consumer");
        }
        assert_eq!(
            *spy.runs.lock().unwrap(),
            vec![vec![1, 2], vec![3, 4], vec![5]]
        );
    }

    /// A cycle that did not commit drops the kept consumer, so the batch is
    /// delivered again.
    #[tokio::test]
    async fn real_authorizer_a_failed_batch_drops_the_kept_consumer_and_redelivers() {
        let fx = real_auth_bus_service();
        real_topic_with_record(&fx, TopicOptions::default()).await;
        publish_ids(&fx, 2..=3).await;
        let key = seed_general_key(&fx.db);
        topic_right(&fx.db, &key, "orders.raw", "read");

        let config = consume_config(&key, json!({"batch_size": 2, "on_error": "halt"}));
        let status = StatusStore::default();
        let mut slot = ConsumerSlot::None;
        let ok = IdSpy::new(false);
        let ok_dispatch: Arc<dyn ReactorFlowDispatch> = ok.clone();
        run_cycle(
            &fx.svc,
            "flow-kept",
            &config,
            &ok_dispatch,
            &status,
            &mut slot,
        )
        .await;
        assert!(slot.is_some());

        let failing = IdSpy::new(true);
        let failing_dispatch: Arc<dyn ReactorFlowDispatch> = failing.clone();
        let after = run_cycle(
            &fx.svc,
            "flow-kept",
            &config,
            &failing_dispatch,
            &status,
            &mut slot,
        )
        .await;
        assert!(matches!(after, AfterFailure::Halt(_)));
        assert!(
            slot.is_none(),
            "an uncommitted cycle must not keep the consumer"
        );

        run_cycle(
            &fx.svc,
            "flow-kept",
            &config,
            &ok_dispatch,
            &status,
            &mut slot,
        )
        .await;
        assert_eq!(*failing.runs.lock().unwrap(), vec![vec![3]]);
        assert_eq!(*ok.runs.lock().unwrap(), vec![vec![1, 2], vec![3]]);
    }

    /// `bus.consume.denied` audit rows written for `orders.raw`.
    fn denied_rows(db: &DbPool) -> Vec<crate::db::models::AuditLogEntry> {
        repository::list_audit_logs(
            db,
            &crate::db::models::AuditLogFilters {
                action: Some("bus.consume.denied".to_string()),
                ..Default::default()
            },
            0,
            50,
        )
        .expect("audit rows")
        .into_iter()
        .filter(|row| row.resource.as_deref() == Some("orders.raw"))
        .collect()
    }

    /// What ends a kept consumer's authority between two cycles: the next
    /// cycle is refused with the same reason a fresh open would give, writes
    /// `expected_denied_rows` denial audit rows (a fresh open writes one for a
    /// denied right, none for a field-policy refusal), and reads nothing.
    async fn assert_kept_consumer_is_refused_after(
        change: impl FnOnce(&RealAuth, &str),
        expected_reason: &'static str,
        expected_denied_rows: usize,
    ) {
        let fx = real_auth_bus_service();
        real_topic_with_record(&fx, TopicOptions::default()).await;
        let key = seed_general_key(&fx.db);
        topic_right(&fx.db, &key, "orders.raw", "read");

        let spy = IdSpy::new(false);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let status = StatusStore::default();
        let config = consume_config(&key, json!({}));
        let mut slot = ConsumerSlot::None;
        let first = run_cycle(&fx.svc, "flow-kept", &config, &dispatch, &status, &mut slot).await;
        assert!(matches!(first, AfterFailure::Continue));
        assert!(slot.is_some());
        assert!(
            denied_rows(&fx.db).is_empty(),
            "no denial before the change"
        );

        change(&fx, &key);
        publish_ids(&fx, 2..=2).await;
        let second = run_cycle(&fx.svc, "flow-kept", &config, &dispatch, &status, &mut slot).await;
        assert!(matches!(second, AfterFailure::Refused));
        assert_eq!(
            status_of(&status, "flow-kept"),
            (ConsumerState::Refused, Some(expected_reason))
        );
        assert!(slot.is_none());
        assert_eq!(
            *spy.runs.lock().unwrap(),
            vec![vec![1]],
            "nothing read after the change"
        );
        let denied = denied_rows(&fx.db);
        assert_eq!(denied.len(), expected_denied_rows, "{denied:?}");
    }

    #[tokio::test]
    async fn real_authorizer_a_kept_consumer_is_refused_after_the_key_is_deleted() {
        assert_kept_consumer_is_refused_after(
            |fx, key| {
                repository::delete_api_key_by_uid(&fx.db, key).expect("delete");
            },
            reason::SERVICE_ACCOUNT_NOT_FOUND,
            1,
        )
        .await;
    }

    #[tokio::test]
    async fn real_authorizer_a_kept_consumer_is_refused_after_the_key_is_disabled() {
        assert_kept_consumer_is_refused_after(
            |fx, key| {
                repository::set_api_key_active(&fx.db, key, false).expect("disable");
            },
            reason::SERVICE_ACCOUNT_DISABLED,
            1,
        )
        .await;
    }

    #[tokio::test]
    async fn real_authorizer_a_kept_consumer_is_refused_after_the_read_right_is_removed() {
        assert_kept_consumer_is_refused_after(
            |fx, key| {
                repository::resource_permissions::clear(
                    &fx.db,
                    "topic",
                    &crate::services::bus_authorizer::topic_acl_resource_id(
                        REAL_INSTANCE,
                        "org-default",
                        "orders.raw",
                    ),
                    "api_key",
                    key,
                )
                .expect("clear right")
            },
            reason::READ_DENIED,
            1,
        )
        .await;
    }

    /// A field policy for another subject appears while the consumer is kept:
    /// the key now needs a topic-wide rule.
    #[tokio::test]
    async fn real_authorizer_a_kept_consumer_is_refused_after_a_policy_for_another_subject_appears()
    {
        use crate::bus::field_policies as fp;
        assert_kept_consumer_is_refused_after(
            |fx, _key| {
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
            },
            reason::NEEDS_TOPIC_WIDE_RULE,
            0,
        )
        .await;
    }

    /// `open_consumer` fixes the partition list, so a topic that grows
    /// partitions under a kept consumer must make the next cycle reopen it.
    #[tokio::test]
    async fn real_authorizer_a_kept_consumer_follows_a_topic_that_grows_partitions() {
        let fx = real_auth_bus_service();
        let ctx = test_ctx("org-default");
        create_topic(
            &fx.svc,
            &ctx,
            "orders.raw",
            TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .await;
        publish_ids(&fx, 1..=1).await;
        let key = seed_general_key(&fx.db);
        topic_right(&fx.db, &key, "orders.raw", "read");

        let spy = IdSpy::new(false);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let status = StatusStore::default();
        let config = consume_config(&key, json!({}));
        let mut slot = ConsumerSlot::None;
        run_cycle(&fx.svc, "flow-grow", &config, &dispatch, &status, &mut slot).await;
        assert!(slot.is_some());

        let svc = fx.svc.clone();
        let grow_ctx = ctx.clone();
        blocking(move || {
            svc.update_topic(
                &grow_ctx,
                "orders.raw",
                TopicOptions {
                    partitions: Some(2),
                    ..Default::default()
                },
            )
            .map(|_| ())
        })
        .await
        .expect("grow partitions");
        let svc = fx.svc.clone();
        blocking(move || {
            svc.publish(
                &ctx,
                "orders.raw",
                PublishBatch {
                    partition: Some(1),
                    producer: None,
                    records: vec![PublishRecord {
                        key: None,
                        headers: vec![],
                        payload: Bytes::from(r#"{"id":2}"#),
                        timestamp_ms: 0,
                        schema_id: 0,
                    }],
                },
            )
            .map(|_| ())
        })
        .await
        .expect("publish to the new partition");

        run_cycle(&fx.svc, "flow-grow", &config, &dispatch, &status, &mut slot).await;
        assert_eq!(*spy.runs.lock().unwrap(), vec![vec![1], vec![2]]);
    }

    /// A kept consumer must not outlive a stopped engine: it holds the
    /// partition writers and directory locks, so the instance could not start
    /// again on the same directory.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_stopped_instance_releases_the_kept_consumer_so_it_can_start_again() {
        let id = BusInstanceId::parse("tentabus-8b000008").expect("valid instance id");
        let (dir, svc) = registry_bus_service(id.clone());
        let ctx = BusCallContext {
            instance_id: id.clone(),
            ..test_ctx("org-default")
        };
        create_topic(&svc, &ctx, "orders.raw", TopicOptions::default()).await;
        publish_one(&svc, &ctx, "orders.raw", r#"{"id":1}"#).await;
        let key = seed_general_key(svc.db());
        let config = ConsumeConfig::from_config(&json!({
            "instance_id": id.as_str(), "service_account": key,
            "topic": "orders.raw", "group": "g1", "max_wait_ms": 20
        }))
        .expect("config");

        let spy = IdSpy::new(false);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let task = spawn_subscription(
            "flow-stop".to_string(),
            config,
            dispatch,
            Arc::new(StatusStore::default()),
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while spy.runs.lock().unwrap().is_empty() {
            assert!(std::time::Instant::now() < deadline, "no delivery");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        drop(svc);
        crate::bus::stop_instance(&id);
        let bus_dir = dir.path().join("bus");
        let mut restarted = None;
        while std::time::Instant::now() < deadline {
            match registry_bus_service_at(id.clone(), bus_dir.clone()) {
                Ok(svc) => {
                    restarted = Some(svc);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
        drop(task);
        crate::bus::stop_instance(&id);
        assert!(
            restarted.is_some(),
            "the instance could not start again on its directory"
        );
    }

    /// `AtMostOnce` committed inside `fetch`, so its handle is kept too.
    #[tokio::test]
    async fn real_authorizer_an_at_most_once_consumer_is_kept_and_reads_each_record_once() {
        let fx = real_auth_bus_service();
        real_topic_with_record(&fx, TopicOptions::default()).await;
        publish_ids(&fx, 2..=3).await;
        let key = seed_general_key(&fx.db);
        topic_right(&fx.db, &key, "orders.raw", "read");

        let spy = IdSpy::new(false);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let status = StatusStore::default();
        let config = consume_config(
            &key,
            json!({"batch_size": 2, "commit_mode": "at_most_once"}),
        );
        let mut slot = ConsumerSlot::None;
        for _ in 0..3 {
            let after =
                run_cycle(&fx.svc, "flow-amo", &config, &dispatch, &status, &mut slot).await;
            assert!(matches!(after, AfterFailure::Continue));
            assert!(slot.is_some());
        }
        assert_eq!(*spy.runs.lock().unwrap(), vec![vec![1, 2], vec![3]]);
    }

    /// A skipped failure commits, so the handle is kept and the next cycle
    /// continues after the skipped records.
    #[tokio::test]
    async fn real_authorizer_a_kept_consumer_continues_after_a_skipped_batch() {
        let fx = real_auth_bus_service();
        real_topic_with_record(&fx, TopicOptions::default()).await;
        publish_ids(&fx, 2..=4).await;
        let key = seed_general_key(&fx.db);
        topic_right(&fx.db, &key, "orders.raw", "read");

        let config = consume_config(&key, json!({"batch_size": 2, "on_error": "skip"}));
        let status = StatusStore::default();
        let mut slot = ConsumerSlot::None;
        let failing = IdSpy::new(true);
        let failing_dispatch: Arc<dyn ReactorFlowDispatch> = failing.clone();
        let after = run_cycle(
            &fx.svc,
            "flow-skip",
            &config,
            &failing_dispatch,
            &status,
            &mut slot,
        )
        .await;
        assert!(matches!(after, AfterFailure::Continue));
        assert!(slot.is_some(), "a committed skip keeps the consumer");

        let ok = IdSpy::new(false);
        let ok_dispatch: Arc<dyn ReactorFlowDispatch> = ok.clone();
        run_cycle(
            &fx.svc,
            "flow-skip",
            &config,
            &ok_dispatch,
            &status,
            &mut slot,
        )
        .await;
        assert_eq!(*failing.runs.lock().unwrap(), vec![vec![1, 2]]);
        assert_eq!(*ok.runs.lock().unwrap(), vec![vec![3, 4]]);
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
            &mut ConsumerSlot::None,
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
            &mut ConsumerSlot::None,
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
        let after = run_cycle(
            &fx.svc,
            "flow-real-rules",
            &config,
            &dispatch,
            &status,
            &mut ConsumerSlot::None,
        )
        .await;
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
        let after = run_cycle(
            &fx.svc,
            "flow-real-dlq",
            &config,
            &dispatch,
            &status,
            &mut ConsumerSlot::None,
        )
        .await;
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

    // ---- TB-03 S4: consumption under an SLA policy ----

    use crate::sla::SlaPolicySpec;

    fn unit_of(records: &[(u32, u64)]) -> Vec<FetchedRecordMeta> {
        records
            .iter()
            .map(|(partition, offset)| record("t", *partition, *offset, "{}"))
            .collect()
    }

    #[test]
    fn commit_prefix_stops_a_partition_at_its_first_unhandled_unit() {
        let (a, b, c) = (
            unit_of(&[(0, 0), (0, 1)]),
            unit_of(&[(0, 2)]),
            unit_of(&[(0, 3)]),
        );
        let mut committed = commit_prefix(&[(&a, true), (&b, false), (&c, true)]);
        committed.sort_by_key(|(tp, _)| tp.partition);
        assert_eq!(
            committed,
            vec![(
                TopicPartition {
                    topic: "t".into(),
                    partition: 0
                },
                2
            )],
            "only the units before the first unhandled one are committed"
        );
    }

    #[test]
    fn commit_prefix_keeps_partitions_independent() {
        let (a, b, c) = (unit_of(&[(0, 0)]), unit_of(&[(1, 0)]), unit_of(&[(1, 1)]));
        let mut committed = commit_prefix(&[(&a, false), (&b, true), (&c, true)]);
        committed.sort_by_key(|(tp, _)| tp.partition);
        assert_eq!(
            committed,
            vec![(
                TopicPartition {
                    topic: "t".into(),
                    partition: 1
                },
                2
            )]
        );
    }

    #[test]
    fn commit_prefix_of_a_serial_unit_spanning_partitions_is_all_or_nothing() {
        let both = unit_of(&[(0, 4), (1, 7)]);
        let mut committed = commit_prefix(&[(&both, true)]);
        committed.sort_by_key(|(tp, _)| tp.partition);
        assert_eq!(
            committed.iter().map(|(_, o)| *o).collect::<Vec<_>>(),
            vec![5, 8]
        );
        assert!(commit_prefix(&[(&both, false)]).is_empty());
    }

    #[test]
    fn plan_lanes_without_parallelism_is_one_unit_of_everything_fetched() {
        let records = unit_of(&[(0, 0), (1, 0), (0, 1)]);
        let lanes = plan_lanes(records, None);
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].len(), 1);
        assert_eq!(lanes[0][0].records.len(), 3);
    }

    #[test]
    fn plan_lanes_cuts_partitions_into_units_in_offset_order() {
        let records = unit_of(&[(0, 0), (1, 0), (0, 1), (0, 2), (1, 1)]);
        let lanes = plan_lanes(records.clone(), Some((2, BusOrdering::Partition)));
        let shape: Vec<Vec<Vec<(u32, u64)>>> = lanes
            .iter()
            .map(|lane| {
                lane.iter()
                    .map(|unit| {
                        unit.records
                            .iter()
                            .map(|r| (r.partition, r.offset))
                            .collect()
                    })
                    .collect()
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                vec![vec![(0, 0), (0, 1)], vec![(0, 2)]],
                vec![vec![(1, 0), (1, 1)]],
            ]
        );

        let unordered = plan_lanes(records, Some((2, BusOrdering::None)));
        assert_eq!(unordered.len(), 3, "every unit is a lane of its own");
        assert!(unordered.iter().all(|lane| lane.len() == 1));
    }

    #[test]
    fn a_throttle_shows_only_while_it_lies_ahead_and_status_changes_keep_the_live_fields() {
        let store = StatusStore::default();
        store.record("f", ConsumerState::Running, None, None);
        store.set_limit("f", Some(4));
        store.add_in_flight("f", 2);
        store.throttle_until("f", now_epoch_ms() + 60_000);
        store.record("f", ConsumerState::Waiting, Some(reason::BUS_ERROR), None);
        let status = store.get("f").unwrap();
        assert_eq!(
            (status.in_flight, status.max_concurrency),
            (2, Some(4)),
            "a state change does not reset what the cycle reports"
        );
        assert!(status.throttled_until.is_some());

        store.throttle_until("f", 1);
        assert!(
            store.get("f").unwrap().throttled_until.is_some(),
            "the later time stands"
        );
        let past = StatusStore::default();
        past.record("g", ConsumerState::Running, None, None);
        past.throttle_until("g", 1);
        assert_eq!(past.get("g").unwrap().throttled_until, None);
        past.add_in_flight("g", -1);
        assert_eq!(past.get("g").unwrap().in_flight, 0, "never below zero");
    }

    /// Dispatch spy for parallel cycles. Payloads are `{"id": n, "p": partition}`
    /// (one object, or an array of them for a batch). Every dispatch waits at a
    /// barrier of `peers` dispatches on the way in and on the way out, so a
    /// test that expects N units in flight at once gets exactly that (and can
    /// look at them all in flight) or fails on the timeout, with no sleeping.
    struct ParallelSpy {
        state: Mutex<SpyState>,
        arrive: tokio::sync::Barrier,
        depart: tokio::sync::Barrier,
        failing: Mutex<HashSet<i64>>,
        timing_out: Mutex<HashSet<i64>>,
        /// Ids whose run never starts (`CompileFailed`).
        never_starts: Mutex<HashSet<i64>>,
        /// How long a dispatch stays in flight (between the two barriers).
        hold: Duration,
        status: Option<(Arc<StatusStore>, String)>,
    }

    #[derive(Default)]
    struct SpyState {
        current: usize,
        peak: usize,
        active: HashMap<i64, usize>,
        peak_per_partition: HashMap<i64, usize>,
        started: Vec<(i64, Vec<i64>)>,
        slot_held: Vec<bool>,
        seen_in_flight: Vec<(u32, Option<u32>)>,
    }

    impl ParallelSpy {
        fn new(peers: usize) -> Arc<Self> {
            Arc::new(Self {
                state: Mutex::default(),
                arrive: tokio::sync::Barrier::new(peers),
                depart: tokio::sync::Barrier::new(peers),
                failing: Mutex::default(),
                timing_out: Mutex::default(),
                never_starts: Mutex::default(),
                hold: Duration::ZERO,
                status: None,
            })
        }

        /// A spy whose dispatches stay in flight for `hold`, long enough for
        /// two that overlap to be seen overlapping.
        fn holding(peers: usize, hold: Duration) -> Arc<Self> {
            Arc::new(Self {
                state: Mutex::default(),
                arrive: tokio::sync::Barrier::new(peers),
                depart: tokio::sync::Barrier::new(peers),
                failing: Mutex::default(),
                timing_out: Mutex::default(),
                never_starts: Mutex::default(),
                hold,
                status: None,
            })
        }

        fn observing(peers: usize, status: Arc<StatusStore>, flow: &str) -> Arc<Self> {
            Arc::new(Self {
                state: Mutex::default(),
                arrive: tokio::sync::Barrier::new(peers),
                depart: tokio::sync::Barrier::new(peers),
                failing: Mutex::default(),
                timing_out: Mutex::default(),
                never_starts: Mutex::default(),
                hold: Duration::ZERO,
                status: Some((status, flow.to_string())),
            })
        }

        fn fail(&self, ids: &[i64]) {
            *self.failing.lock().unwrap() = ids.iter().copied().collect();
        }

        fn started(&self) -> Vec<(i64, Vec<i64>)> {
            self.state.lock().unwrap().started.clone()
        }

        fn started_ids(&self, partition: i64) -> Vec<i64> {
            self.started()
                .into_iter()
                .filter(|(p, _)| *p == partition)
                .flat_map(|(_, ids)| ids)
                .collect()
        }
    }

    #[async_trait]
    impl ReactorFlowDispatch for ParallelSpy {
        async fn dispatch(
            &self,
            flow_id: String,
            initial: FlowEnvelope,
            principal: AgentPrincipal,
        ) -> anyhow::Result<()> {
            let FlowValue::Json(value) = initial.payload else {
                anyhow::bail!("expected a JSON payload");
            };
            let items: Vec<(i64, i64)> = match value {
                serde_json::Value::Array(items) => items.iter().map(id_and_partition).collect(),
                single => vec![id_and_partition(&single)],
            };
            let partition = items[0].1;
            let ids: Vec<i64> = items.iter().map(|(id, _)| *id).collect();
            {
                let mut state = self.state.lock().unwrap();
                state.current += 1;
                state.peak = state.peak.max(state.current);
                let active = state.active.entry(partition).or_default();
                *active += 1;
                let active = *active;
                let peak = state.peak_per_partition.entry(partition).or_default();
                *peak = (*peak).max(active);
                state.started.push((partition, ids.clone()));
                state.slot_held.push(
                    crate::agents::reactive_run_meta("x".into(), &principal).slot_held_by_caller,
                );
            }
            tokio::time::timeout(Duration::from_secs(10), self.arrive.wait())
                .await
                .expect("the other units never ran beside this one");
            if let Some((status, flow)) = &self.status {
                let seen = status.get(flow).expect("status");
                self.state
                    .lock()
                    .unwrap()
                    .seen_in_flight
                    .push((seen.in_flight, seen.max_concurrency));
            }
            tokio::time::sleep(self.hold).await;
            tokio::time::timeout(Duration::from_secs(10), self.depart.wait())
                .await
                .expect("the other units never left");
            {
                let mut state = self.state.lock().unwrap();
                state.current -= 1;
                *state.active.get_mut(&partition).unwrap() -= 1;
            }
            if ids
                .iter()
                .any(|id| self.timing_out.lock().unwrap().contains(id))
            {
                return Err(anyhow::Error::new(ReactiveDispatchError(
                    crate::flow_engine::dispatcher::DispatchError::TimedOut {
                        flow_id,
                        timeout_ms: 100,
                    },
                )));
            }
            if ids
                .iter()
                .any(|id| self.never_starts.lock().unwrap().contains(id))
            {
                return Err(anyhow::Error::new(ReactiveDispatchError(
                    crate::flow_engine::dispatcher::DispatchError::CompileFailed {
                        flow_id,
                        msg: "scripted".into(),
                    },
                )));
            }
            if ids
                .iter()
                .any(|id| self.failing.lock().unwrap().contains(id))
            {
                anyhow::bail!("scripted dispatch failure");
            }
            Ok(())
        }
    }

    fn id_and_partition(value: &serde_json::Value) -> (i64, i64) {
        (
            value["id"].as_i64().expect("id"),
            value["p"].as_i64().expect("partition"),
        )
    }

    const SLA_FIRST_CLOCK_MS: u64 = 10_000_000;

    /// A registry of the test's own on a clock the test moves, with `flow_id`
    /// governed by `spec` in `svc`'s database.
    struct SlaRig {
        registry: &'static SlaRegistry,
        clock: Arc<std::sync::atomic::AtomicU64>,
        gate: Arc<ServiceGate>,
    }

    fn govern(svc: &Arc<BusService>, flow_id: &str, spec: SlaPolicySpec, clock_ms: u64) -> SlaRig {
        let clock = Arc::new(std::sync::atomic::AtomicU64::new(clock_ms));
        let handle = clock.clone();
        let registry: &'static SlaRegistry =
            Box::leak(Box::new(SlaRegistry::with_clock(Arc::new(move || {
                handle.load(std::sync::atomic::Ordering::SeqCst)
            }))));
        {
            let conn = svc.db().write().expect("db lock");
            conn.execute(
                "INSERT INTO flows (id, name, flow_json, status) VALUES (?1, ?1, '{}', 'active')",
                [flow_id],
            )
            .expect("insert flow");
            // The change journal references the organization and the actor.
            conn.execute(
                "INSERT OR IGNORE INTO organizations (org_id, name, slug, created_at) \
                 VALUES ('org-a', 'org-a', 'org-a', datetime('now'))",
                [],
            )
            .expect("insert organization");
            conn.execute(
                "INSERT OR IGNORE INTO user_accounts (id, username, password_hash) \
                 VALUES ('admin', 'sla-test-admin', 'x')",
                [],
            )
            .expect("insert actor");
        }
        let policy = crate::sla::repository::create_policy(
            svc.db(),
            "org-a",
            "admin",
            &SlaPolicySpec {
                name: format!("policy-{flow_id}"),
                ..spec
            },
        )
        .expect("create policy");
        crate::sla::repository::assign(svc.db(), "org-a", "admin", "flow", flow_id, &policy.id)
            .expect("assign policy");
        registry.refresh_if_stale(svc.db()).expect("load policies");
        let gate = registry.gate(flow_id).expect("gate exists");
        SlaRig {
            registry,
            clock,
            gate,
        }
    }

    fn concurrent(max_concurrency: u32) -> SlaPolicySpec {
        SlaPolicySpec {
            max_concurrency: Some(max_concurrency),
            ..Default::default()
        }
    }

    /// The flow's consumer config under a general key that exists in `svc`'s
    /// database, so that the per-cycle checks of the consumer pass and a kept
    /// consumer is really reused.
    fn keyed_config(svc: &Arc<BusService>, extra: serde_json::Value) -> ConsumeConfig {
        let key = seed_general_key(svc.db());
        let mut config = json!({
            "instance_id": "tentabus-00000001", "service_account": key,
            "topic": "orders.raw", "group": "g1", "max_wait_ms": 20
        });
        config
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        ConsumeConfig::from_config(&config).unwrap()
    }

    async fn publish_to(svc: &Arc<BusService>, ctx: &BusCallContext, partition: u32, id: i64) {
        let svc = svc.clone();
        let ctx = ctx.clone();
        blocking(move || {
            svc.publish(
                &ctx,
                "orders.raw",
                PublishBatch {
                    partition: Some(partition),
                    producer: None,
                    records: vec![PublishRecord {
                        key: None,
                        headers: vec![],
                        payload: Bytes::from(json!({"id": id, "p": partition}).to_string()),
                        timestamp_ms: 0,
                        schema_id: 0,
                    }],
                },
            )
            .map(|_| ())
        })
        .await
        .expect("publish");
    }

    /// A topic with `partitions` partitions and `ids_per_partition` records on
    /// each, ids `p * 100 + k` in publish order.
    async fn filled_topic(
        partitions: u32,
        ids_per_partition: i64,
        opts: TopicOptions,
    ) -> (tempfile::TempDir, Arc<BusService>, BusCallContext) {
        let (dir, svc) = test_bus_service();
        let ctx = test_ctx("org-default");
        create_topic(
            &svc,
            &ctx,
            "orders.raw",
            TopicOptions {
                partitions: Some(partitions),
                ..opts
            },
        )
        .await;
        for p in 0..partitions {
            for k in 1..=ids_per_partition {
                publish_to(&svc, &ctx, p, i64::from(p) * 100 + k).await;
            }
        }
        (dir, svc, ctx)
    }

    /// The ids the flow's group would still be delivered, from a fresh consumer.
    async fn remaining(
        svc: &Arc<BusService>,
        ctx: &BusCallContext,
        config: &ConsumeConfig,
    ) -> Vec<i64> {
        let group = config.effective_group().unwrap();
        let mut ids: Vec<i64> = fetch_once(svc, ctx, &group, "orders.raw")
            .await
            .iter()
            .map(|r| id_and_partition(&serde_json::from_slice(&r.payload).unwrap()).0)
            .collect();
        ids.sort_unstable();
        ids
    }

    async fn cycle(
        svc: &Arc<BusService>,
        config: &ConsumeConfig,
        dispatch: &Arc<dyn ReactorFlowDispatch>,
        status: &StatusStore,
        sla: &SlaRig,
    ) -> AfterFailure {
        cycle_in(svc, config, dispatch, status, sla, &mut ConsumerSlot::None).await
    }

    /// [`cycle`] that keeps its consumer in `slot` between calls, as the
    /// subscription loop does.
    async fn cycle_in(
        svc: &Arc<BusService>,
        config: &ConsumeConfig,
        dispatch: &Arc<dyn ReactorFlowDispatch>,
        status: &StatusStore,
        sla: &SlaRig,
        slot: &mut ConsumerSlot,
    ) -> AfterFailure {
        tokio::time::timeout(
            Duration::from_secs(20),
            run_governed_cycle(
                svc,
                "flow-sla",
                config,
                dispatch,
                status,
                slot,
                Some(sla.registry),
            ),
        )
        .await
        .expect("the cycle did not finish")
    }

    #[tokio::test]
    async fn units_beyond_the_concurrency_wait_and_never_run_more_than_n_at_once() {
        let (_dir, svc, ctx) = filled_topic(4, 1, TopicOptions::default()).await;
        let sla = govern(&svc, "flow-sla", concurrent(2), SLA_FIRST_CLOCK_MS);
        let status = Arc::new(StatusStore::default());
        let spy = ParallelSpy::observing(2, status.clone(), "flow-sla");
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        // One fetch returns all four records, one per partition: four units,
        // two slots.
        let config = keyed_config(&svc, json!({"batch_size": 4}));

        let after = cycle(&svc, &config, &dispatch, &status, &sla).await;

        assert!(matches!(after, AfterFailure::Continue));
        {
            let state = spy.state.lock().unwrap();
            assert_eq!(state.started.len(), 4);
            assert_eq!(state.peak, 2, "two ran together, never three");
            assert!(state.slot_held.iter().all(|held| *held));
            assert!(
                state
                    .seen_in_flight
                    .iter()
                    .all(|seen| *seen == (2, Some(2))),
                "the status reports what is in flight and the limit: {:?}",
                state.seen_in_flight
            );
        }
        assert_eq!(remaining(&svc, &ctx, &config).await, Vec::<i64>::new());
        let stats = sla.gate.stats();
        assert_eq!((stats.accepted, stats.ok, stats.in_flight), (4, 4, 0));
        assert_eq!(status.get("flow-sla").unwrap().in_flight, 0);
    }

    #[tokio::test]
    async fn partitions_run_side_by_side_and_each_keeps_its_order() {
        let (_dir, svc, ctx) = filled_topic(3, 4, TopicOptions::default()).await;
        let sla = govern(&svc, "flow-sla", concurrent(3), SLA_FIRST_CLOCK_MS);
        let status = StatusStore::default();
        let spy = ParallelSpy::new(3);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let config = keyed_config(&svc, json!({}));

        // Each cycle fetches three units, one record from each partition.
        for _ in 0..4 {
            let after = cycle(&svc, &config, &dispatch, &status, &sla).await;
            assert!(matches!(after, AfterFailure::Continue));
        }

        {
            let state = spy.state.lock().unwrap();
            assert_eq!(state.peak, 3, "the three partitions ran together");
            assert!(
                state.peak_per_partition.values().all(|peak| *peak == 1),
                "a partition never runs two units at once"
            );
        }
        for p in 0..3i64 {
            assert_eq!(
                spy.started_ids(p),
                (1..=4).map(|k| p * 100 + k).collect::<Vec<_>>(),
                "partition {p} in order"
            );
        }
        assert_eq!(remaining(&svc, &ctx, &config).await, Vec::<i64>::new());
    }

    #[tokio::test]
    async fn a_failure_in_the_middle_commits_only_the_prefix_and_redelivers_the_rest() {
        let (_dir, svc, ctx) = filled_topic(1, 3, TopicOptions::default()).await;
        let sla = govern(&svc, "flow-sla", concurrent(3), SLA_FIRST_CLOCK_MS);
        let status = StatusStore::default();
        let spy = ParallelSpy::new(1);
        spy.fail(&[2]);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let config = keyed_config(&svc, json!({}));
        let mut slot = ConsumerSlot::None;

        let after = cycle_in(&svc, &config, &dispatch, &status, &sla, &mut slot).await;

        assert!(matches!(after, AfterFailure::Continue));
        assert_eq!(
            spy.started_ids(0),
            vec![1, 2],
            "the unit after the failed one of its partition does not start"
        );
        assert_eq!(remaining(&svc, &ctx, &config).await, vec![2, 3]);
        assert!(
            slot.is_none(),
            "the cursor is past what was committed: the consumer is not kept"
        );

        spy.fail(&[]);
        let after = cycle_in(&svc, &config, &dispatch, &status, &sla, &mut slot).await;
        assert!(matches!(after, AfterFailure::Continue));
        assert_eq!(
            spy.started_ids(0),
            vec![1, 2, 2, 3],
            "redelivered, in order"
        );
        assert_eq!(remaining(&svc, &ctx, &config).await, Vec::<i64>::new());
        assert!(slot.is_some(), "everything committed: the consumer is kept");
    }

    #[tokio::test]
    async fn without_ordering_a_later_success_after_a_failure_is_redelivered() {
        let (_dir, svc, ctx) = filled_topic(1, 3, TopicOptions::default()).await;
        let sla = govern(
            &svc,
            "flow-sla",
            SlaPolicySpec {
                ordering: BusOrdering::None,
                ..concurrent(3)
            },
            SLA_FIRST_CLOCK_MS,
        );
        let status = StatusStore::default();
        let spy = ParallelSpy::new(3);
        spy.fail(&[2]);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let config = keyed_config(&svc, json!({}));

        let after = cycle(&svc, &config, &dispatch, &status, &sla).await;

        assert!(matches!(after, AfterFailure::Continue));
        assert_eq!(
            spy.state.lock().unwrap().peak,
            3,
            "units of one partition ran together"
        );
        assert_eq!(
            remaining(&svc, &ctx, &config).await,
            vec![2, 3],
            "3 succeeded but sits behind the failed 2"
        );
    }

    #[tokio::test]
    async fn halt_in_a_partition_commits_what_precedes_it_and_starts_nothing_after() {
        let (_dir, svc, ctx) = filled_topic(1, 3, TopicOptions::default()).await;
        let sla = govern(&svc, "flow-sla", concurrent(3), SLA_FIRST_CLOCK_MS);
        let status = StatusStore::default();
        let spy = ParallelSpy::new(1);
        spy.fail(&[2]);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let config = keyed_config(&svc, json!({"on_error": "halt"}));
        let mut slot = ConsumerSlot::None;

        let after = cycle_in(&svc, &config, &dispatch, &status, &sla, &mut slot).await;

        assert!(matches!(after, AfterFailure::Halt(_)));
        assert_eq!(
            status_of(&status, "flow-sla"),
            (ConsumerState::Halted, None)
        );
        assert_eq!(spy.started_ids(0), vec![1, 2]);
        assert_eq!(remaining(&svc, &ctx, &config).await, vec![2, 3]);
        assert!(slot.is_none(), "a halted cycle does not keep its consumer");
        // Fixed and resumed (the supervisor restarts a halted loop on a save):
        // the redelivery starts at the halted record.
        spy.fail(&[]);
        let after = cycle_in(&svc, &config, &dispatch, &status, &sla, &mut slot).await;
        assert!(matches!(after, AfterFailure::Continue));
        assert_eq!(spy.started_ids(0), vec![1, 2, 2, 3]);
    }

    #[tokio::test]
    async fn halt_lets_the_other_partitions_in_flight_finish_and_commits_them() {
        let (_dir, svc, ctx) = filled_topic(2, 1, TopicOptions::default()).await;
        let sla = govern(&svc, "flow-sla", concurrent(2), SLA_FIRST_CLOCK_MS);
        let status = StatusStore::default();
        let spy = ParallelSpy::new(2);
        spy.fail(&[101]);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let config = keyed_config(&svc, json!({"on_error": "halt", "batch_size": 2}));

        let after = cycle(&svc, &config, &dispatch, &status, &sla).await;

        assert!(matches!(after, AfterFailure::Halt(_)));
        assert_eq!(spy.started().len(), 2, "both partitions were in flight");
        assert_eq!(
            remaining(&svc, &ctx, &config).await,
            vec![101],
            "partition 0 finished and is committed, the halted unit is not"
        );
    }

    #[tokio::test]
    async fn dead_letters_are_written_per_record_and_the_partition_goes_on() {
        let (_dir, svc, ctx) = filled_topic(
            1,
            4,
            TopicOptions {
                max_delivery_attempts: Some(1),
                ..Default::default()
            },
        )
        .await;
        let sla = govern(&svc, "flow-sla", concurrent(2), SLA_FIRST_CLOCK_MS);
        let status = StatusStore::default();
        let spy = ParallelSpy::new(1);
        spy.fail(&[1, 2]);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        // Two units of two records each, one partition: they run in order.
        let config = keyed_config(&svc, json!({"batch_size": 2}));

        let after = cycle(&svc, &config, &dispatch, &status, &sla).await;

        assert!(matches!(after, AfterFailure::Continue));
        assert_eq!(spy.started(), vec![(0, vec![1, 2]), (0, vec![3, 4])]);
        assert_eq!(
            remaining(&svc, &ctx, &config).await,
            Vec::<i64>::new(),
            "the dead-lettered unit counts as handled and the one after it committed"
        );
        let dead = fetch_once(
            &svc,
            &ctx,
            "dlq-reader",
            &crate::bus::dlq::dlq_topic_name("orders.raw"),
        )
        .await;
        assert_eq!(
            dead.len(),
            2,
            "one dead letter per record of the failed unit"
        );
    }

    #[tokio::test]
    async fn at_most_once_commits_in_fetch_and_the_reactor_commits_nothing_more() {
        let (_dir, svc, ctx) = filled_topic(2, 1, TopicOptions::default()).await;
        let sla = govern(&svc, "flow-sla", concurrent(2), SLA_FIRST_CLOCK_MS);
        let status = StatusStore::default();
        let spy = ParallelSpy::new(2);
        spy.fail(&[101]);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let config = keyed_config(
            &svc,
            json!({
                "commit_mode": "at_most_once", "on_error": "skip", "batch_size": 2
            }),
        );

        let after = cycle(&svc, &config, &dispatch, &status, &sla).await;

        assert!(matches!(after, AfterFailure::Continue));
        assert_eq!(spy.started().len(), 2);
        assert_eq!(
            remaining(&svc, &ctx, &config).await,
            Vec::<i64>::new(),
            "both were committed by the fetch, the failed one included"
        );
        cycle(&svc, &config, &dispatch, &status, &sla).await;
        assert_eq!(spy.started().len(), 2, "nothing is delivered twice");
    }

    #[tokio::test]
    async fn a_call_limit_delays_the_unit_without_a_dead_letter_and_without_loss() {
        let (_dir, svc, ctx) = filled_topic(
            1,
            3,
            TopicOptions {
                max_delivery_attempts: Some(1),
                ..Default::default()
            },
        )
        .await;
        // 50 ms before the next one-second window.
        let sla = govern(
            &svc,
            "flow-sla",
            SlaPolicySpec {
                call_limit: Some(2),
                call_window_secs: Some(1),
                ..concurrent(1)
            },
            SLA_FIRST_CLOCK_MS + 950,
        );
        let status = StatusStore::default();
        let spy = ParallelSpy::new(1);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let config = keyed_config(&svc, json!({}));

        for _ in 0..2 {
            cycle(&svc, &config, &dispatch, &status, &sla).await;
        }
        assert_eq!(spy.started_ids(0), vec![1, 2]);

        let third = cycle(&svc, &config, &dispatch, &status, &sla);
        let observe = async {
            tokio::time::timeout(Duration::from_secs(10), async {
                while status
                    .get("flow-sla")
                    .and_then(|s| s.throttled_until)
                    .is_none()
                {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            })
            .await
            .expect("the consumer was never held back");
            assert_eq!(
                spy.started_ids(0),
                vec![1, 2],
                "the held back unit did not run"
            );
            assert_eq!(
                remaining(&svc, &ctx, &config).await,
                vec![3],
                "the record is still there, not lost"
            );
            assert_eq!(
                fetch_dead_letters(&svc, &ctx).await,
                0,
                "a refusal is not a failure of the record"
            );
            sla.clock.store(
                SLA_FIRST_CLOCK_MS + 1_100,
                std::sync::atomic::Ordering::SeqCst,
            );
        };
        let (after, ()) = tokio::join!(third, observe);

        assert!(matches!(after, AfterFailure::Continue));
        assert_eq!(
            spy.started_ids(0),
            vec![1, 2, 3],
            "delivered in the next window"
        );
        assert_eq!(remaining(&svc, &ctx, &config).await, Vec::<i64>::new());
        let stats = sla.gate.stats();
        assert_eq!((stats.accepted, stats.ok, stats.in_flight), (3, 3, 0));
        assert_eq!(
            stats.rejected_calls, 1,
            "the refusal is counted once for the unit, not for every time it asked"
        );
        assert_eq!(fetch_dead_letters(&svc, &ctx).await, 0);
    }

    async fn fetch_dead_letters(svc: &Arc<BusService>, ctx: &BusCallContext) -> usize {
        let svc = svc.clone();
        let ctx = ctx.clone();
        let topic = crate::bus::dlq::dlq_topic_name("orders.raw");
        blocking(move || {
            svc.open_consumer(
                &ctx,
                "dlq-reader",
                &[topic],
                ConsumerConfig {
                    commit_mode: CommitMode::AutoAfterSuccess,
                },
            )
            .ok()
            .map_or(0, |handle| {
                handle.fetch(1024, 10).expect("fetch").records.len()
            })
        })
        .await
    }

    #[tokio::test]
    async fn a_concurrency_of_one_is_the_serial_consumer_with_its_calls_counted() {
        let (_dir, svc, ctx) = filled_topic(1, 3, TopicOptions::default()).await;
        let sla = govern(&svc, "flow-sla", concurrent(1), SLA_FIRST_CLOCK_MS);
        let status = StatusStore::default();
        let spy = ParallelSpy::new(1);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let config = keyed_config(&svc, json!({"batch_size": 3}));

        let after = cycle(&svc, &config, &dispatch, &status, &sla).await;

        assert!(matches!(after, AfterFailure::Continue));
        assert_eq!(
            spy.started(),
            vec![(0, vec![1, 2, 3])],
            "one fetch, one run of the whole batch"
        );
        assert_eq!(spy.state.lock().unwrap().slot_held, vec![true]);
        assert_eq!(remaining(&svc, &ctx, &config).await, Vec::<i64>::new());
        let stats = sla.gate.stats();
        assert_eq!((stats.accepted, stats.ok), (1, 1));
        assert_eq!(status.get("flow-sla").unwrap().max_concurrency, Some(1));
    }

    #[tokio::test]
    async fn a_policy_without_a_concurrency_limit_keeps_the_consumer_serial() {
        let (_dir, svc, _ctx) = filled_topic(2, 1, TopicOptions::default()).await;
        let sla = govern(
            &svc,
            "flow-sla",
            SlaPolicySpec {
                call_limit: Some(100),
                call_window_secs: Some(60),
                ..Default::default()
            },
            SLA_FIRST_CLOCK_MS,
        );
        let status = StatusStore::default();
        let spy = ParallelSpy::new(1);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let config = keyed_config(&svc, json!({"batch_size": 5}));

        cycle(&svc, &config, &dispatch, &status, &sla).await;

        let started = spy.started();
        assert_eq!(started.len(), 1, "everything fetched is one unit");
        let mut ids = started[0].1.clone();
        ids.sort_unstable();
        assert_eq!(ids, vec![1, 101]);
        assert_eq!(sla.gate.stats().accepted, 1);
        assert_eq!(status.get("flow-sla").unwrap().max_concurrency, None);
    }

    #[tokio::test]
    async fn a_policy_timeout_is_recorded_on_the_permit_not_as_a_plain_error() {
        let (_dir, svc, ctx) = filled_topic(1, 1, TopicOptions::default()).await;
        let sla = govern(&svc, "flow-sla", concurrent(1), SLA_FIRST_CLOCK_MS);
        let status = StatusStore::default();
        let spy = ParallelSpy::new(1);
        spy.timing_out.lock().unwrap().insert(1);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let config = keyed_config(&svc, json!({"on_error": "skip"}));

        let after = cycle(&svc, &config, &dispatch, &status, &sla).await;

        assert!(matches!(after, AfterFailure::Continue));
        let stats = sla.gate.stats();
        assert_eq!(
            (
                stats.accepted,
                stats.timeouts,
                stats.errors,
                stats.in_flight
            ),
            (1, 1, 0, 0)
        );
        assert_eq!(remaining(&svc, &ctx, &config).await, Vec::<i64>::new());
    }

    // ---- S4 review: capacity before fetch, commit edge cases, refusals ----

    #[tokio::test]
    async fn at_most_once_runs_every_fetched_unit_even_after_a_failure() {
        let (_dir, svc, ctx) = filled_topic(1, 3, TopicOptions::default()).await;
        let sla = govern(&svc, "flow-sla", concurrent(3), SLA_FIRST_CLOCK_MS);
        let status = StatusStore::default();
        let spy = ParallelSpy::new(1);
        spy.fail(&[1]);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let config = keyed_config(&svc, json!({"commit_mode": "at_most_once"}));

        let after = cycle(&svc, &config, &dispatch, &status, &sla).await;

        assert!(matches!(after, AfterFailure::Continue));
        assert_eq!(
            spy.started_ids(0),
            vec![1, 2, 3],
            "the fetch committed all three: none may be dropped unrun"
        );
        assert_eq!(remaining(&svc, &ctx, &config).await, Vec::<i64>::new());
    }

    #[tokio::test]
    async fn at_most_once_halts_only_after_every_fetched_unit_ran() {
        let (_dir, svc, ctx) = filled_topic(1, 3, TopicOptions::default()).await;
        let sla = govern(&svc, "flow-sla", concurrent(3), SLA_FIRST_CLOCK_MS);
        let status = StatusStore::default();
        let spy = ParallelSpy::new(1);
        spy.fail(&[2]);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let config = keyed_config(
            &svc,
            json!({"commit_mode": "at_most_once", "on_error": "halt"}),
        );

        let after = cycle(&svc, &config, &dispatch, &status, &sla).await;

        assert!(matches!(after, AfterFailure::Halt(_)));
        assert_eq!(spy.started_ids(0), vec![1, 2, 3]);
        assert_eq!(remaining(&svc, &ctx, &config).await, Vec::<i64>::new());
    }

    #[tokio::test]
    async fn idle_polls_spend_no_call_budget() {
        let (_dir, svc, ctx) = filled_topic(1, 0, TopicOptions::default()).await;
        let sla = govern(
            &svc,
            "flow-sla",
            SlaPolicySpec {
                call_limit: Some(2),
                call_window_secs: Some(60),
                ..concurrent(1)
            },
            SLA_FIRST_CLOCK_MS + 500,
        );
        let status = StatusStore::default();
        let spy = ParallelSpy::new(1);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let config = keyed_config(&svc, json!({}));

        for _ in 0..5 {
            cycle(&svc, &config, &dispatch, &status, &sla).await;
        }
        let stats = sla.gate.stats();
        assert_eq!((stats.accepted, stats.window_calls), (0, 0));

        publish_to(&svc, &ctx, 0, 1).await;
        publish_to(&svc, &ctx, 0, 2).await;
        for _ in 0..2 {
            cycle(&svc, &config, &dispatch, &status, &sla).await;
        }
        assert_eq!(spy.started_ids(0), vec![1, 2], "the whole budget was there");
        assert_eq!(sla.gate.stats().window_calls, 2);
    }

    #[tokio::test]
    async fn a_throttled_at_most_once_consumer_fetches_nothing_until_it_has_room() {
        let (_dir, svc, ctx) = filled_topic(1, 2, TopicOptions::default()).await;
        let sla = govern(
            &svc,
            "flow-sla",
            SlaPolicySpec {
                call_limit: Some(1),
                call_window_secs: Some(60),
                ..concurrent(1)
            },
            SLA_FIRST_CLOCK_MS + 500,
        );
        let status = StatusStore::default();
        let spy = ParallelSpy::new(1);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let config = keyed_config(&svc, json!({"commit_mode": "at_most_once"}));
        cycle(&svc, &config, &dispatch, &status, &sla).await;
        assert_eq!(spy.started_ids(0), vec![1]);

        // The window is used up: the next cycle waits and, being dropped here,
        // never ran anything.
        let held = tokio::time::timeout(
            Duration::from_millis(300),
            run_governed_cycle(
                &svc,
                "flow-sla",
                &config,
                &dispatch,
                &status,
                &mut ConsumerSlot::None,
                Some(sla.registry),
            ),
        )
        .await;

        assert!(held.is_err(), "the cycle is waiting for room");
        assert!(status.get("flow-sla").unwrap().throttled_until.is_some());
        assert_eq!(
            remaining(&svc, &ctx, &config).await,
            vec![2],
            "record 2 was not fetched, so the fetch did not commit it away"
        );
    }

    #[tokio::test]
    async fn a_dead_letter_after_a_done_unit_is_committed_by_the_prefix() {
        let (_dir, svc, ctx) = filled_topic(
            1,
            4,
            TopicOptions {
                max_delivery_attempts: Some(1),
                ..Default::default()
            },
        )
        .await;
        let sla = govern(&svc, "flow-sla", concurrent(2), SLA_FIRST_CLOCK_MS);
        let status = StatusStore::default();
        let spy = ParallelSpy::new(1);
        spy.fail(&[3, 4]);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let config = keyed_config(&svc, json!({"batch_size": 2}));

        let after = cycle(&svc, &config, &dispatch, &status, &sla).await;

        assert!(matches!(after, AfterFailure::Continue));
        // The dead letters could not advance the store (the first unit was not
        // committed yet), so the prefix commit is what moves it past them.
        assert_eq!(remaining(&svc, &ctx, &config).await, Vec::<i64>::new());
        assert_eq!(fetch_dead_letters(&svc, &ctx).await, 2);
    }

    #[tokio::test]
    async fn a_dead_letter_ahead_of_the_prefix_does_not_hold_back_another_partitions_commit() {
        let (_dir, svc, ctx) = filled_topic(
            2,
            0,
            TopicOptions {
                max_delivery_attempts: Some(2),
                ..Default::default()
            },
        )
        .await;
        for id in 1..=4 {
            publish_to(&svc, &ctx, 0, id).await;
        }
        publish_to(&svc, &ctx, 1, 101).await;
        let sla = govern(&svc, "flow-sla", concurrent(3), SLA_FIRST_CLOCK_MS);
        let status = StatusStore::default();
        let config = keyed_config(&svc, json!({"batch_size": 2}));
        // Offsets 0 to 2 of p0 have failed once already: their next failure is
        // dead-lettered, and each dead letter advances the store by itself.
        for offset in 0..3 {
            note_failure(
                svc.clone(),
                consumer_context(svc.typed_instance_id(), &config, TEST_KEY_UID),
                config.effective_group().unwrap(),
                record("orders.raw", 0, offset, "{}"),
                DlqReason::ConsumerError,
                "earlier".to_string(),
            )
            .await
            .expect("noted");
        }
        // Three fetches of two records: [1, 2] of p0, [101, 3] and [4]. The units
        // of p0 are [1, 2], which is dead-lettered whole (the store moves to
        // 2), and [3, 4], with one dead letter (the store moves to 3) and one
        // retry: the prefix ends at 2 while the store is at 3.
        let spy = ParallelSpy::new(1);
        spy.fail(&[1, 2, 3, 4]);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();

        let after = cycle(&svc, &config, &dispatch, &status, &sla).await;

        assert!(matches!(after, AfterFailure::Continue));
        assert_eq!(
            remaining(&svc, &ctx, &config).await,
            vec![4],
            "p1's success is committed, p0 resumes after its dead letters"
        );
        assert_eq!(fetch_dead_letters(&svc, &ctx).await, 3);
    }

    /// Two callers of the gate that are never idle (what /v1 or chat traffic on
    /// the same flow looks like): one runs while the other is queued, so the
    /// gate always has a waiter. `stop` ends them.
    fn crowd_the_gate(
        gate: &Arc<ServiceGate>,
    ) -> (Arc<AtomicBool>, Vec<tokio::task::JoinHandle<()>>) {
        let stop = Arc::new(AtomicBool::new(false));
        let tasks = (0..2)
            .map(|_| {
                let gate = gate.clone();
                let stop = stop.clone();
                tokio::spawn(async move {
                    while !stop.load(Ordering::Acquire) {
                        if let Ok(permit) = gate.admit(AdmitOrigin::Request).await {
                            tokio::time::sleep(Duration::from_millis(15)).await;
                            permit.finish(Outcome::Ok);
                        }
                    }
                })
            })
            .collect();
        (stop, tasks)
    }

    #[tokio::test]
    async fn a_consumer_makes_progress_while_other_callers_keep_the_gate_queued() {
        for commit_mode in ["auto_after_success", "at_most_once"] {
            let (_dir, svc, ctx) = filled_topic(1, 3, TopicOptions::default()).await;
            let sla = govern(
                &svc,
                "flow-sla",
                SlaPolicySpec {
                    max_queue: Some(10),
                    queue_timeout_ms: Some(10_000),
                    ..concurrent(1)
                },
                SLA_FIRST_CLOCK_MS,
            );
            let status = StatusStore::default();
            let spy = ParallelSpy::new(1);
            let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
            let config = keyed_config(&svc, json!({"commit_mode": commit_mode, "batch_size": 1}));
            let (stop, crowd) = crowd_the_gate(&sla.gate);

            for _ in 0..3 {
                let after = cycle(&svc, &config, &dispatch, &status, &sla).await;
                assert!(matches!(after, AfterFailure::Continue), "{commit_mode}");
            }
            stop.store(true, Ordering::Release);
            for task in crowd {
                task.await.expect("the crowd ends");
            }

            assert_eq!(spy.started_ids(0), vec![1, 2, 3], "{commit_mode}");
            assert_eq!(
                remaining(&svc, &ctx, &config).await,
                Vec::<i64>::new(),
                "{commit_mode}"
            );
        }
    }

    #[tokio::test]
    async fn at_most_once_idle_polls_leave_no_trace_in_the_gate() {
        let (_dir, svc, _ctx) = filled_topic(2, 0, TopicOptions::default()).await;
        let sla = govern(
            &svc,
            "flow-sla",
            SlaPolicySpec {
                call_limit: Some(2),
                call_window_secs: Some(60),
                ..concurrent(2)
            },
            SLA_FIRST_CLOCK_MS + 500,
        );
        let status = StatusStore::default();
        let spy = ParallelSpy::new(1);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let config = keyed_config(&svc, json!({"commit_mode": "at_most_once"}));

        for _ in 0..5 {
            cycle(&svc, &config, &dispatch, &status, &sla).await;
        }

        let stats = sla.gate.stats();
        assert_eq!(
            (
                stats.accepted,
                stats.window_calls,
                stats.in_flight,
                stats.cancelled,
                stats.rejected_calls,
                stats.rejected_concurrency
            ),
            (0, 0, 0, 0, 0, 0),
            "the permits taken for the fetch were given back"
        );
        assert!(spy.started().is_empty());
    }

    #[tokio::test]
    async fn at_most_once_never_fetches_more_units_than_it_holds_permits() {
        let (_dir, svc, ctx) = filled_topic(4, 1, TopicOptions::default()).await;
        let sla = govern(&svc, "flow-sla", concurrent(3), SLA_FIRST_CLOCK_MS);
        let status = StatusStore::default();
        let spy = ParallelSpy::new(1);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let config = keyed_config(
            &svc,
            json!({"commit_mode": "at_most_once", "batch_size": 4}),
        );
        // Another caller holds one of the three slots: the cycle can reserve
        // two, and one fetch returns all four records, one per partition.
        let outsider = sla.gate.try_admit(AdmitOrigin::Request).await.unwrap();

        let after = cycle(&svc, &config, &dispatch, &status, &sla).await;

        assert!(matches!(after, AfterFailure::Continue));
        let mut delivered: Vec<i64> = spy.started().into_iter().flat_map(|(_, ids)| ids).collect();
        delivered.sort_unstable();
        assert_eq!(
            delivered,
            vec![1, 101, 201, 301],
            "every record the fetch committed was handed to a unit that ran"
        );
        assert!(spy.started().len() <= 2, "no more units than permits");
        assert_eq!(remaining(&svc, &ctx, &config).await, Vec::<i64>::new());
        let stats = sla.gate.stats();
        assert_eq!(
            (stats.in_flight, stats.cancelled),
            (1, 0),
            "only the outsider is left, the spare permit was given back"
        );
        drop(outsider);
    }

    #[tokio::test]
    async fn a_throttled_first_cycle_already_shows_in_the_consumer_status() {
        let (_dir, svc, _ctx) = filled_topic(1, 2, TopicOptions::default()).await;
        let sla = govern(
            &svc,
            "flow-sla",
            SlaPolicySpec {
                call_limit: Some(1),
                call_window_secs: Some(60),
                ..concurrent(1)
            },
            SLA_FIRST_CLOCK_MS + 500,
        );
        drop(sla.gate.try_admit(AdmitOrigin::Request).await.unwrap());
        let status = StatusStore::default();
        let spy = ParallelSpy::new(1);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let config = keyed_config(&svc, json!({"commit_mode": "at_most_once"}));

        let held = tokio::time::timeout(
            Duration::from_millis(300),
            run_governed_cycle(
                &svc,
                "flow-sla",
                &config,
                &dispatch,
                &status,
                &mut ConsumerSlot::None,
                Some(sla.registry),
            ),
        )
        .await;

        assert!(held.is_err(), "the cycle is waiting for room");
        let shown = status.get("flow-sla").expect("a status exists");
        assert_eq!(shown.state, ConsumerState::Running);
        assert!(shown.throttled_until.is_some());
    }

    #[tokio::test]
    async fn a_throttle_episode_is_counted_once_however_many_cycles_it_spans() {
        let (_dir, svc, _ctx) = filled_topic(1, 2, TopicOptions::default()).await;
        let sla = govern(
            &svc,
            "flow-sla",
            SlaPolicySpec {
                call_limit: Some(1),
                call_window_secs: Some(60),
                ..concurrent(1)
            },
            SLA_FIRST_CLOCK_MS + 500,
        );
        sla.gate
            .try_admit(AdmitOrigin::Request)
            .await
            .unwrap()
            .finish(Outcome::Ok);
        let status = StatusStore::default();
        let spy = ParallelSpy::new(1);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let config = keyed_config(&svc, json!({"commit_mode": "at_most_once"}));

        for _ in 0..3 {
            let held = tokio::time::timeout(
                Duration::from_millis(200),
                run_governed_cycle(
                    &svc,
                    "flow-sla",
                    &config,
                    &dispatch,
                    &status,
                    &mut ConsumerSlot::None,
                    Some(sla.registry),
                ),
            )
            .await;
            assert!(held.is_err(), "the cycle is paused");
        }

        assert_eq!(sla.gate.stats().rejected_calls, 1);
    }

    #[tokio::test]
    async fn an_idle_at_most_once_poll_is_capped_by_the_queue_timeout() {
        let (_dir, svc, _ctx) = filled_topic(1, 0, TopicOptions::default()).await;
        let sla = govern(
            &svc,
            "flow-sla",
            SlaPolicySpec {
                queue_timeout_ms: Some(400),
                max_queue: Some(5),
                ..concurrent(1)
            },
            SLA_FIRST_CLOCK_MS,
        );
        let status = StatusStore::default();
        let spy = ParallelSpy::new(1);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let config = keyed_config(
            &svc,
            json!({"commit_mode": "at_most_once", "max_wait_ms": 5000}),
        );

        let started = std::time::Instant::now();
        cycle(&svc, &config, &dispatch, &status, &sla).await;

        assert!(
            started.elapsed() < Duration::from_millis(2500),
            "the long poll held the slot for {:?}, not for half the queue timeout",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn a_throttled_consumer_does_not_keep_its_cached_consumer_through_the_pause() {
        let (_dir, svc, _ctx) = filled_topic(1, 2, TopicOptions::default()).await;
        let sla = govern(
            &svc,
            "flow-sla",
            SlaPolicySpec {
                call_limit: Some(2),
                call_window_secs: Some(60),
                ..concurrent(1)
            },
            SLA_FIRST_CLOCK_MS + 500,
        );
        let status = StatusStore::default();
        let spy = ParallelSpy::new(1);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let config = keyed_config(&svc, json!({"commit_mode": "at_most_once"}));
        let mut slot = ConsumerSlot::None;
        cycle_in(&svc, &config, &dispatch, &status, &sla, &mut slot).await;
        let kept = Arc::downgrade(&slot.as_ref().expect("the consumer is kept").handle);
        // One call of the window is left; take it so the next cycle is refused.
        drop(sla.gate.try_admit(AdmitOrigin::Request).await.unwrap());

        let mut paused = Box::pin(run_governed_cycle(
            &svc,
            "flow-sla",
            &config,
            &dispatch,
            &status,
            &mut slot,
            Some(sla.registry),
        ));
        let observe = async {
            tokio::time::timeout(Duration::from_secs(10), async {
                while status
                    .get("flow-sla")
                    .and_then(|s| s.throttled_until)
                    .is_none()
                {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            })
            .await
            .expect("the consumer was never held back");
            assert!(
                kept.upgrade().is_none(),
                "the cycle sits in its pause without the consumer"
            );
        };
        tokio::select! {
            _ = &mut paused => panic!("the cycle should be paused"),
            () = observe => {}
        }
    }

    #[tokio::test]
    async fn a_policy_removed_while_a_unit_waits_lets_it_run_without_a_permit() {
        let (_dir, svc, ctx) = filled_topic(1, 1, TopicOptions::default()).await;
        let sla = govern(
            &svc,
            "flow-sla",
            SlaPolicySpec {
                max_queue: Some(0),
                ..concurrent(1)
            },
            SLA_FIRST_CLOCK_MS,
        );
        let held = sla.gate.try_admit(AdmitOrigin::Request).await.unwrap();
        let status = StatusStore::default();
        let spy = ParallelSpy::new(1);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let config = keyed_config(&svc, json!({}));

        let run = cycle(&svc, &config, &dispatch, &status, &sla);
        let lift = async {
            tokio::time::timeout(Duration::from_secs(10), async {
                while status
                    .get("flow-sla")
                    .and_then(|s| s.throttled_until)
                    .is_none()
                {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            })
            .await
            .expect("the unit was never refused");
            assert!(spy.started().is_empty());
            crate::sla::repository::unassign(svc.db(), "org-a", "admin", "flow", "flow-sla")
                .expect("unassign");
        };
        let (after, ()) = tokio::join!(run, lift);

        assert!(matches!(after, AfterFailure::Continue));
        assert_eq!(
            spy.started_ids(0),
            vec![1],
            "it ran once the policy was gone"
        );
        assert_eq!(remaining(&svc, &ctx, &config).await, Vec::<i64>::new());
        assert_eq!(
            sla.gate.stats().accepted,
            1,
            "only the outsider was ever admitted"
        );
        drop(held);
    }

    #[tokio::test]
    async fn a_consumer_that_lost_its_rights_during_a_pause_dispatches_nothing() {
        let (_dir, svc, ctx) = filled_topic(1, 1, TopicOptions::default()).await;
        let sla = govern(
            &svc,
            "flow-sla",
            SlaPolicySpec {
                call_limit: Some(1),
                call_window_secs: Some(1),
                ..concurrent(1)
            },
            SLA_FIRST_CLOCK_MS + 950,
        );
        sla.gate
            .try_admit(AdmitOrigin::Request)
            .await
            .unwrap()
            .finish(Outcome::Ok);
        let status = StatusStore::default();
        let spy = ParallelSpy::new(1);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let config = keyed_config(&svc, json!({}));
        let key = config.service_account.clone().expect("a key");

        let run = cycle(&svc, &config, &dispatch, &status, &sla);
        let revoke = async {
            tokio::time::timeout(Duration::from_secs(10), async {
                while status
                    .get("flow-sla")
                    .and_then(|s| s.throttled_until)
                    .is_none()
                {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            })
            .await
            .expect("the unit was never refused");
            crate::db::repository::set_api_key_active(svc.db(), &key, false).expect("disable");
            sla.clock.store(
                SLA_FIRST_CLOCK_MS + 1_100,
                std::sync::atomic::Ordering::SeqCst,
            );
        };
        let (after, ()) = tokio::join!(run, revoke);

        assert!(matches!(after, AfterFailure::Continue));
        assert!(spy.started().is_empty(), "nothing was dispatched");
        crate::db::repository::set_api_key_active(svc.db(), &key, true).expect("enable");
        assert_eq!(
            remaining(&svc, &ctx, &config).await,
            vec![1],
            "the record is delivered again"
        );
        let stats = sla.gate.stats();
        assert_eq!(
            (stats.accepted, stats.in_flight, stats.cancelled),
            (1, 0, 0),
            "the permit taken after the pause went back unused (only the outsider counts)"
        );
    }

    #[tokio::test]
    async fn a_commit_raises_an_entry_behind_the_store_to_the_store() {
        let (_dir, svc, ctx) = filled_topic(2, 1, TopicOptions::default()).await;
        let config = keyed_config(&svc, json!({}));
        let group = config.effective_group().unwrap();
        let handle = {
            let svc = svc.clone();
            let ctx = ctx.clone();
            blocking(move || {
                svc.open_consumer(
                    &ctx,
                    &group,
                    &["orders.raw".to_string()],
                    ConsumerConfig {
                        commit_mode: CommitMode::AutoAfterSuccess,
                    },
                )
            })
            .await
            .map(Arc::new)
            .expect("open")
        };
        let tp = |partition| TopicPartition {
            topic: "orders.raw".into(),
            partition,
        };
        commit_offsets(handle.clone(), vec![(tp(0), 1)])
            .await
            .expect("commit");
        let behind_the_store = vec![(tp(0), 0), (tp(1), 1)];
        assert!(
            handle.commit(&behind_the_store).is_err(),
            "the raw commit refuses the whole call"
        );

        commit_offsets(handle.clone(), behind_the_store)
            .await
            .expect("the covered entry is raised to the store, the other one is written");

        assert_eq!(handle.committed_offset("orders.raw", 0).unwrap(), 1);
        assert_eq!(handle.committed_offset("orders.raw", 1).unwrap(), 1);
    }

    #[tokio::test]
    async fn a_run_that_never_started_costs_the_policy_nothing() {
        let (_dir, svc, ctx) = filled_topic(1, 1, TopicOptions::default()).await;
        let sla = govern(&svc, "flow-sla", concurrent(1), SLA_FIRST_CLOCK_MS);
        let status = StatusStore::default();
        let spy = ParallelSpy::new(1);
        spy.never_starts.lock().unwrap().insert(1);
        let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
        let config = keyed_config(&svc, json!({"on_error": "skip"}));

        let after = cycle(&svc, &config, &dispatch, &status, &sla).await;

        assert!(matches!(after, AfterFailure::Continue));
        let stats = sla.gate.stats();
        assert_eq!(
            (stats.accepted, stats.errors, stats.in_flight),
            (0, 0, 0),
            "not counted as a call nor as an error"
        );
        assert_eq!(remaining(&svc, &ctx, &config).await, Vec::<i64>::new());
    }

    #[tokio::test]
    async fn two_units_of_one_partition_never_overlap_but_do_without_ordering() {
        for (ordering, expected_peak) in [(BusOrdering::Partition, 1), (BusOrdering::None, 2)] {
            let (_dir, svc, _ctx) = filled_topic(1, 2, TopicOptions::default()).await;
            let sla = govern(
                &svc,
                "flow-sla",
                SlaPolicySpec {
                    ordering,
                    ..concurrent(2)
                },
                SLA_FIRST_CLOCK_MS,
            );
            let status = StatusStore::default();
            // Units that overlap stay in flight together for 60 ms.
            let spy = ParallelSpy::holding(1, Duration::from_millis(60));
            let dispatch: Arc<dyn ReactorFlowDispatch> = spy.clone();
            let config = keyed_config(&svc, json!({}));

            cycle(&svc, &config, &dispatch, &status, &sla).await;

            let state = spy.state.lock().unwrap();
            assert_eq!(state.started.len(), 2);
            assert_eq!(state.peak, expected_peak, "{ordering:?}");
            assert_eq!(state.peak_per_partition[&0], expected_peak, "{ordering:?}");
        }
    }

    #[test]
    fn a_failed_top_up_fetch_returns_what_was_read_and_is_not_clean() {
        let one = |offset| FetchedBatch {
            records: vec![record("t", 0, offset, "{}")],
        };
        let mut calls = 0;
        let mut fetch = |_: usize, _: u32| {
            calls += 1;
            match calls {
                1 => Ok(one(0)),
                2 => Ok(one(1)),
                _ => Err(BusServiceError::NotInitialized),
            }
        };
        let fetched = fetch_cycle_with(&mut fetch, 1, 4, 10).expect("fetched");
        assert_eq!(fetched.records.len(), 2);
        assert!(
            !fetched.clean,
            "the handle's cursor may be ahead: do not keep it"
        );
        assert_eq!(calls, 3, "no fetch after the failed one");

        let mut calls = 0;
        let mut ends_short = |_: usize, _: u32| {
            calls += 1;
            Ok(if calls == 1 {
                one(0)
            } else {
                FetchedBatch::default()
            })
        };
        let fetched = fetch_cycle_with(&mut ends_short, 1, 4, 10).expect("fetched");
        assert!(fetched.clean);
        assert_eq!((fetched.records.len(), calls), (1, 2));
    }
}
