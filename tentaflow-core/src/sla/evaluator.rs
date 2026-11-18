//! The per-node SLA evaluator: every 30 s it reads the stats of each governed
//! service, judges them against the policy over a 5-minute window, keeps the
//! breaches in `sla_breaches` and writes a sample per minute to `sla_samples`
//! (plan TB-03 section 3.5, decisions Q8, Q9 and Q13).
//!
//! # Windowing
//!
//! A gate's counters and its latency histogram are cumulative, so the evaluator
//! keeps its own short history: one snapshot per tick (counters, histogram and
//! the call-window peak) for the last [`EVAL_WINDOW`] plus the one snapshot
//! that serves as the baseline. The window's counts and its p95 are the
//! difference between the newest snapshot and that baseline. The gate's
//! admission path is untouched; a call that starts costs one CAS and, only when
//! it sets a new window peak, one `fetch_max`.
//!
//! A gate that was BORN during the evaluator's life is baselined at zero: all
//! it counted is news. A gate that was already running when the evaluator first
//! saw it (a draining gate that came back, an evaluator that started late) is
//! baselined at its current stats, so its lifetime totals do not land in the
//! first window, the rejection audit or the first sample. The state of a
//! service is kept for as long as its gate is alive, also while the service is
//! unassigned and the gate drains, so a reassignment carries on instead of
//! starting over; it is reset when the gate is replaced or the organization of
//! the assignment changes.
//!
//! # Clocks
//!
//! Durations and ordering (the window, saturation, the audit window, the
//! housekeeping interval) use a monotonic clock, so a step of the wall clock
//! cannot stretch or collapse them. The wall clock only stamps what is stored
//! (`raised_at_ms`, `resolved_at_ms`, the sample's minute) and decides where a
//! minute sample falls; a wall clock that steps back only re-synchronizes the
//! sampler, it writes nothing twice.
//!
//! # What is judged
//!
//! * `latency`: the estimated p95 of the window is above `latency_p95_ms`. The
//!   estimate places the quantile inside its histogram bucket
//!   ([`HistogramSnapshot::estimate_percentile_ms`]), so 210 ms calls do not
//!   read as 300 ms against a 250 ms threshold. It stays an estimate, and a
//!   threshold above the histogram's last bound (one hour) fires only when the
//!   slow calls average above it.
//! * `errors`: errors are at least `error_rate_pct` of the calls that finished
//!   in the window.
//! * `timeouts`: at least one call timed out in the window.
//! * `calls_warn` / `calls_limit`: the highest number of calls that STARTED in
//!   one call window during the evaluation window reached `warn_at_pct` of the
//!   limit / the limit itself. This is read from the window's own count, never
//!   from refusals, because the bus waits for a free call instead of being
//!   refused (a throttled bus flow must still raise them), and a permit a bus
//!   reactor only reserved and gave back is not a call. The peak, not the live
//!   count, is judged: a 1-second window has rolled over many times since the
//!   last tick. A peak counts only against the limit it was measured under: when
//!   the limit changes, the older peaks are ignored.
//! * `concurrency_saturated`: running calls equal `max_concurrency` at every
//!   tick for at least [`SATURATION_AFTER`]. Reserved bus slots do not count.
//!   This is sampled, not continuous: a slot that frees and is retaken between
//!   two ticks is not seen. The first tick that sees a free slot forgets the run.
//!
//! The thresholds come from the policies as loaded at the start of the round,
//! the limits from the gate's own configuration. After a policy edit the two can
//! disagree for one tick, until the registry has reloaded.
//!
//! # Resolution rule
//!
//! A breach is resolved on the first evaluation on which its condition no longer
//! holds. There is no separate hysteresis: for the window-based kinds the
//! 5-minute window is the hysteresis, because the slow calls, the errors and
//! the usage peak stay inside it for five minutes after they happened, so a
//! condition clears only after five clean minutes. Saturation clears on the
//! first tick that sees a free slot. A breach also resolves when its policy or
//! its assignment is gone, or the policy no longer defines the threshold.
//!
//! # Minute samples
//!
//! A sample is written on the first tick of a new wall-clock minute and is
//! stamped with the minute that just ended. It holds what the gate counted since
//! the previous sample, so with 30 s ticks its coverage is that minute give or
//! take one tick. The first tick of a service writes nothing: its sample comes a
//! minute later.
//!
//! # Audit
//!
//! `sla.calls.rejected`, `sla.concurrency.rejected`, `sla.breach.raised` and
//! `sla.breach.resolved` go through a window like the bus's `AuditWindows`: the
//! first occurrence per (org, action, subject) in a 60 s window is written at
//! once, the rest are counted and written as one `count=N` row when the window
//! ends. The subject is the service for rejections and the breach (its dedupe
//! key) for raised and resolved, so a folded row still says what it is about.
//! Rejections are counted from the gate's counters, so the refusal path itself
//! does no I/O. Every row carries the organization of the policy.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use parking_lot::Mutex;
use tracing::{debug, warn};

use super::breach::{self, Breach, BreachKind, Candidate, RaiseOutcome, Sample};
use super::gate::ServiceGate;
use super::gate::{system_clock, Clock, GateStats, HistogramSnapshot, LatencyHistogram};
use super::policy::SLA_TARGET_FLOW;
use super::registry::SlaRegistry;
use super::repository::{self, ActiveAssignment};
use crate::db::DbPool;

/// How often the evaluator runs.
pub const TICK: Duration = Duration::from_secs(30);

/// The window the thresholds are judged over (decision Q8).
pub const EVAL_WINDOW: Duration = Duration::from_secs(5 * 60);

/// How long every slot must stay taken before `concurrency_saturated` opens.
pub const SATURATION_AFTER: Duration = Duration::from_secs(60);

/// Minute samples are kept this long (decision Q13).
pub const SAMPLE_RETENTION: Duration = Duration::from_secs(7 * 24 * 3600);

/// Resolved breaches are kept this long (decision Q13).
pub const BREACH_RETENTION: Duration = Duration::from_secs(90 * 24 * 3600);

/// Window of the audit rows, as for `bus.quota.exceeded`.
pub const AUDIT_WINDOW: Duration = Duration::from_secs(60);

/// How long the shutdown path waits for the evaluator's final flush.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

const MINUTE_MS: u64 = 60_000;

const ACTION_CALLS_REJECTED: &str = "sla.calls.rejected";
const ACTION_CONCURRENCY_REJECTED: &str = "sla.concurrency.rejected";
const ACTION_BREACH_RAISED: &str = "sla.breach.raised";
const ACTION_BREACH_RESOLVED: &str = "sla.breach.resolved";

fn ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Milliseconds on a clock that never steps: from the first time it is read.
pub fn monotonic_clock() -> Clock {
    let origin = Instant::now();
    Arc::new(move || ms(origin.elapsed()))
}

/// What the evaluator saw of a service over its last window; the protocol layer
/// shows it next to the live gate stats.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowSummary {
    /// Wall-clock time the window was evaluated.
    pub at_ms: u64,
    /// How much time the window really covers: shorter than [`EVAL_WINDOW`]
    /// while the service has been watched for less.
    pub span_ms: u64,
    /// Calls that ran to a result (ok, error or timeout).
    pub finished: u64,
    pub errors: u64,
    pub timeouts: u64,
    pub rejected_calls: u64,
    pub rejected_concurrency: u64,
    /// Estimated p95 of the window (see the module docs); `None` without
    /// finished calls.
    pub p95_ms: Option<f64>,
    /// `None` without finished calls.
    pub error_rate_pct: Option<f64>,
}

/// The latest [`WindowSummary`] of every governed service of this node.
#[derive(Debug, Default)]
pub struct WindowViews {
    views: DashMap<String, WindowSummary>,
}

impl WindowViews {
    /// The views the node's evaluator publishes and the protocol layer reads.
    pub fn global() -> Arc<WindowViews> {
        static VIEWS: OnceLock<Arc<WindowViews>> = OnceLock::new();
        VIEWS.get_or_init(Arc::default).clone()
    }

    pub fn get(&self, target_id: &str) -> Option<WindowSummary> {
        self.views.get(target_id).map(|entry| *entry.value())
    }

    fn set(&self, target_id: &str, summary: WindowSummary) {
        self.views.insert(target_id.to_string(), summary);
    }

    fn retain(&self, keep: &HashSet<String>) {
        self.views.retain(|target, _| keep.contains(target));
    }
}

/// The cumulative part of [`GateStats`] the windows are cut from.
#[derive(Clone, Copy)]
struct Snapshot {
    ok: u64,
    errors: u64,
    timeouts: u64,
    accepted: u64,
    rejected_calls: u64,
    rejected_concurrency: u64,
    latency: HistogramSnapshot,
}

impl Snapshot {
    fn zero() -> Self {
        Self {
            ok: 0,
            errors: 0,
            timeouts: 0,
            accepted: 0,
            rejected_calls: 0,
            rejected_concurrency: 0,
            latency: LatencyHistogram::default().snapshot(),
        }
    }

    fn of(stats: &GateStats) -> Self {
        Self {
            ok: stats.ok,
            errors: stats.errors,
            timeouts: stats.timeouts,
            accepted: stats.accepted,
            rejected_calls: stats.rejected_calls,
            rejected_concurrency: stats.rejected_concurrency,
            latency: stats.latency,
        }
    }
}

/// What happened between two snapshots. A counter that went down (a call that
/// was admitted and then given back) reads as no change.
struct Delta {
    finished: u64,
    errors: u64,
    timeouts: u64,
    accepted: u64,
    rejected_calls: u64,
    rejected_concurrency: u64,
    p95_ms: Option<f64>,
}

impl Delta {
    fn between(newer: &Snapshot, older: &Snapshot) -> Self {
        let up = |n: u64, o: u64| n.saturating_sub(o);
        let errors = up(newer.errors, older.errors);
        let timeouts = up(newer.timeouts, older.timeouts);
        Self {
            finished: up(newer.ok, older.ok) + errors + timeouts,
            errors,
            timeouts,
            accepted: up(newer.accepted, older.accepted),
            rejected_calls: up(newer.rejected_calls, older.rejected_calls),
            rejected_concurrency: up(newer.rejected_concurrency, older.rejected_concurrency),
            p95_ms: newer.latency.since(&older.latency).estimate_p95_ms(),
        }
    }

    fn error_rate_pct(&self) -> Option<f64> {
        (self.finished > 0).then(|| self.errors as f64 * 100.0 / self.finished as f64)
    }
}

struct Entry {
    /// Monotonic.
    at_ms: u64,
    snapshot: Snapshot,
    /// Highest number of calls that started in one call window between the
    /// previous entry and this one.
    window_peak: u32,
    /// The call limit `window_peak` was measured under.
    call_limit: Option<(u32, u32)>,
}

struct TargetState {
    gate: Weak<ServiceGate>,
    org_id: String,
    history: VecDeque<Entry>,
    /// Monotonic.
    saturated_since_ms: Option<u64>,
    /// The snapshot the next minute sample is the difference to.
    sample_base: Snapshot,
    /// Wall-clock minute of the last sample (or of the first sighting).
    last_sample_minute: Option<u64>,
    /// Most calls running that any tick saw since the last sample.
    in_flight_peak: u64,
}

impl TargetState {
    fn new(gate: &Arc<ServiceGate>, org_id: &str, now: Now, baseline: Snapshot) -> Self {
        Self {
            gate: Arc::downgrade(gate),
            org_id: org_id.to_string(),
            history: VecDeque::from([Entry {
                at_ms: now.mono,
                snapshot: baseline,
                window_peak: 0,
                call_limit: None,
            }]),
            saturated_since_ms: None,
            sample_base: baseline,
            last_sample_minute: Some(now.wall / MINUTE_MS),
            in_flight_peak: 0,
        }
    }
}

/// One reading of both clocks.
#[derive(Clone, Copy)]
struct Now {
    wall: u64,
    mono: u64,
}

/// Counts occurrences of an audit action within a window: the first one in a
/// window is to be written at once, the rest are folded into the next row. Same
/// behavior as the bus's `AuditWindows`, with an injected monotonic clock.
struct WindowedAudit {
    window_ms: u64,
    buckets: HashMap<(String, &'static str, String), AuditBucket>,
}

struct AuditBucket {
    window_start_ms: u64,
    suppressed: u32,
    resource: String,
}

struct AuditFlush {
    org_id: String,
    action: &'static str,
    subject: String,
    resource: String,
    count: u32,
}

impl WindowedAudit {
    fn new(window: Duration) -> Self {
        Self {
            window_ms: ms(window),
            buckets: HashMap::new(),
        }
    }

    /// `Some(count)` when this occurrence must be written now; `count` is how
    /// many occurrences the row stands for.
    fn record(
        &mut self,
        now_ms: u64,
        org_id: &str,
        action: &'static str,
        subject: &str,
        resource: &str,
        occurrences: u32,
    ) -> Option<u32> {
        let key = (org_id.to_string(), action, subject.to_string());
        let Some(bucket) = self.buckets.get_mut(&key) else {
            self.buckets.insert(
                key,
                AuditBucket {
                    window_start_ms: now_ms,
                    suppressed: 0,
                    resource: resource.to_string(),
                },
            );
            return Some(occurrences);
        };
        if now_ms.saturating_sub(bucket.window_start_ms) >= self.window_ms {
            let carried = bucket.suppressed.saturating_add(occurrences);
            bucket.window_start_ms = now_ms;
            bucket.suppressed = 0;
            Some(carried)
        } else {
            bucket.suppressed = bucket.suppressed.saturating_add(occurrences);
            None
        }
    }

    /// Takes the buckets whose window has ended (all of them with `force`);
    /// those that suppressed something become one row each.
    fn drain(&mut self, now_ms: u64, force: bool) -> Vec<AuditFlush> {
        let window_ms = self.window_ms;
        let mut flushed = Vec::new();
        self.buckets.retain(|(org_id, action, subject), bucket| {
            if !force && now_ms.saturating_sub(bucket.window_start_ms) < window_ms {
                return true;
            }
            if bucket.suppressed > 0 {
                flushed.push(AuditFlush {
                    org_id: org_id.clone(),
                    action: *action,
                    subject: subject.clone(),
                    resource: std::mem::take(&mut bucket.resource),
                    count: bucket.suppressed,
                });
            }
            false
        });
        flushed
    }
}

pub struct Evaluator {
    node_id: String,
    /// Wall clock, milliseconds since the Unix epoch: stamps and minute boundaries.
    wall: Clock,
    /// Monotonic milliseconds: every duration.
    mono: Clock,
    views: Arc<WindowViews>,
    targets: HashMap<String, TargetState>,
    audit: WindowedAudit,
    last_housekeeping_ms: Option<u64>,
    /// A gate built after this was born during the evaluator's life.
    started: Instant,
}

impl Evaluator {
    pub fn new(
        node_id: impl Into<String>,
        wall: Clock,
        mono: Clock,
        views: Arc<WindowViews>,
    ) -> Self {
        Self {
            node_id: node_id.into(),
            wall,
            mono,
            views,
            targets: HashMap::new(),
            audit: WindowedAudit::new(AUDIT_WINDOW),
            last_housekeeping_ms: None,
            started: Instant::now(),
        }
    }

    fn now(&self) -> Now {
        Now {
            wall: (self.wall)(),
            mono: (self.mono)(),
        }
    }

    /// One evaluation round. Never fails: a storage error is logged and the
    /// round carries on with what it can still do. In particular, when the
    /// policies cannot be read nothing is resolved, because "could not look" is
    /// not "the condition cleared".
    pub fn tick(&mut self, pool: &DbPool, registry: &SlaRegistry) {
        let now = self.now();
        // The registry reloads lazily on admission; a policy assigned to a flow
        // nobody calls yet must still get a gate, so the evaluator asks too.
        let _ = registry.refresh_if_stale(pool);
        let active: Vec<ActiveAssignment> = match repository::load_active(pool) {
            Ok(active) => active
                .into_iter()
                .filter(|a| a.target_kind == SLA_TARGET_FLOW)
                .collect(),
            Err(e) => {
                warn!(error = %e, "SLA evaluator: policies could not be read, skipping the round");
                return;
            }
        };
        self.flush_audit(pool, now.mono, false);

        let mut evaluated: HashSet<String> = HashSet::new();
        let mut holding: HashSet<String> = HashSet::new();
        let assigned: HashSet<String> = active.iter().map(|a| a.target_id.clone()).collect();

        for assignment in &active {
            let Some(gate) = registry.gate(&assignment.target_id) else {
                continue;
            };
            evaluated.insert(assignment.target_id.clone());
            let candidates = self.observe(pool, assignment, &gate, now);
            for candidate in candidates {
                holding.insert(breach::dedupe_key(
                    &candidate.org_id,
                    &candidate.target_id,
                    candidate.kind,
                ));
                self.raise(pool, &candidate, now);
            }
        }

        self.resolve_cleared(pool, &assigned, &evaluated, &holding, now);
        // A service that lost its assignment keeps its state while its gate
        // still drains: if it comes back it is the same gate and the same
        // counters.
        self.targets
            .retain(|target, state| assigned.contains(target) || state.gate.strong_count() > 0);
        self.views.retain(&assigned);
        self.housekeeping(pool, now);
    }

    /// Writes the audit rows still pending, for a clean shutdown.
    pub fn flush_pending_audit(&mut self, pool: &DbPool) {
        let now = self.now();
        self.flush_audit(pool, now.mono, true);
    }

    /// Updates the history of one service and returns the conditions that hold.
    fn observe(
        &mut self,
        pool: &DbPool,
        assignment: &ActiveAssignment,
        gate: &Arc<ServiceGate>,
        now: Now,
    ) -> Vec<Candidate> {
        let stats = gate.stats();
        let limits = gate.config().limits;
        let snapshot = Snapshot::of(&stats);

        let same_gate = self
            .targets
            .get(&assignment.target_id)
            .is_some_and(|state| Weak::ptr_eq(&state.gate, &Arc::downgrade(gate)));
        let same_org = self
            .targets
            .get(&assignment.target_id)
            .is_some_and(|state| state.org_id == assignment.org_id);
        if !(same_gate && same_org) {
            // All a gate counted is news only if it is a new gate: one that
            // was built during this evaluator's life and that no state of ours
            // describes yet. Otherwise (an older gate, or the same gate under a
            // new organization) the history starts at what it has counted.
            let baseline = if !same_gate && gate.created() >= self.started {
                Snapshot::zero()
            } else {
                snapshot
            };
            self.targets.insert(
                assignment.target_id.clone(),
                TargetState::new(gate, &assignment.org_id, now, baseline),
            );
        }
        let state = self
            .targets
            .get_mut(&assignment.target_id)
            .expect("the state was just inserted");

        // A state that was not ticked for a while (its service was unassigned
        // and came back on a draining gate, or a round stalled) must not carry
        // a saturation run or lifetime totals across the gap: it starts over
        // at what the gate counts now, keeping the same gate.
        let gap = ms(TICK) * 3 / 2;
        if state
            .history
            .back()
            .is_some_and(|last| now.mono.saturating_sub(last.at_ms) > gap)
        {
            state.saturated_since_ms = None;
            state.history = VecDeque::from([Entry {
                at_ms: now.mono,
                snapshot,
                window_peak: 0,
                call_limit: None,
            }]);
            state.sample_base = snapshot;
            state.in_flight_peak = 0;
        }

        let since_previous = state
            .history
            .back()
            .map(|previous| Delta::between(&snapshot, &previous.snapshot))
            .expect("history is never empty");

        state.history.push_back(Entry {
            at_ms: now.mono,
            snapshot,
            window_peak: gate.take_window_peak(),
            call_limit: limits.call_limit,
        });
        let window_ms = ms(EVAL_WINDOW);
        while state.history.len() > 1
            && state.history[1].at_ms <= now.mono.saturating_sub(window_ms)
        {
            state.history.pop_front();
        }
        state.in_flight_peak = state.in_flight_peak.max(stats.running);

        state.saturated_since_ms = match limits.max_concurrency {
            Some(max) if stats.running >= u64::from(max) => {
                Some(state.saturated_since_ms.unwrap_or(now.mono))
            }
            _ => None,
        };

        let baseline = state.history.front().expect("history is never empty");
        let window = Delta::between(&snapshot, &baseline.snapshot);
        let call_peak = state
            .history
            .iter()
            .skip(1)
            .filter(|entry| entry.call_limit == limits.call_limit)
            .map(|entry| entry.window_peak)
            .max()
            .unwrap_or(0);
        self.views.set(
            &assignment.target_id,
            WindowSummary {
                at_ms: now.wall,
                span_ms: now.mono.saturating_sub(baseline.at_ms),
                finished: window.finished,
                errors: window.errors,
                timeouts: window.timeouts,
                rejected_calls: window.rejected_calls,
                rejected_concurrency: window.rejected_concurrency,
                p95_ms: window.p95_ms,
                error_rate_pct: window.error_rate_pct(),
            },
        );

        let mut candidates = Vec::new();
        let candidate = |kind: BreachKind, threshold: f64, value: f64| Candidate {
            org_id: assignment.org_id.clone(),
            target_id: assignment.target_id.clone(),
            policy_id: assignment.policy_id.clone(),
            kind,
            threshold,
            value,
        };
        let spec = &assignment.spec;
        if let (Some(threshold), Some(p95)) = (spec.latency_p95_ms, window.p95_ms) {
            if p95 > f64::from(threshold) {
                candidates.push(candidate(BreachKind::Latency, f64::from(threshold), p95));
            }
        }
        if let (Some(threshold), Some(rate)) = (spec.error_rate_pct, window.error_rate_pct()) {
            if rate >= f64::from(threshold) {
                candidates.push(candidate(BreachKind::Errors, f64::from(threshold), rate));
            }
        }
        if window.timeouts > 0 {
            candidates.push(candidate(BreachKind::Timeouts, 0.0, window.timeouts as f64));
        }
        if let Some((limit, _)) = limits.call_limit {
            let used_pct = f64::from(call_peak) * 100.0 / f64::from(limit);
            if let Some(warn_at) = spec.warn_at_pct {
                if used_pct >= f64::from(warn_at) {
                    candidates.push(candidate(
                        BreachKind::CallsWarn,
                        f64::from(warn_at),
                        used_pct,
                    ));
                }
            }
            if call_peak >= limit {
                candidates.push(candidate(
                    BreachKind::CallsLimit,
                    f64::from(limit),
                    f64::from(call_peak),
                ));
            }
        }
        if let (Some(max), Some(since)) = (limits.max_concurrency, state.saturated_since_ms) {
            if now.mono.saturating_sub(since) >= ms(SATURATION_AFTER) {
                candidates.push(candidate(
                    BreachKind::ConcurrencySaturated,
                    f64::from(max),
                    stats.running as f64,
                ));
            }
        }

        self.audit_rejections(pool, assignment, &since_previous, now);
        self.write_minute_sample(pool, assignment, &snapshot, now);
        candidates
    }

    fn audit_rejections(
        &mut self,
        pool: &DbPool,
        assignment: &ActiveAssignment,
        delta: &Delta,
        now: Now,
    ) {
        for (action, count) in [
            (ACTION_CALLS_REJECTED, delta.rejected_calls),
            (ACTION_CONCURRENCY_REJECTED, delta.rejected_concurrency),
        ] {
            if count == 0 {
                continue;
            }
            let occurrences = u32::try_from(count).unwrap_or(u32::MAX);
            self.audit_event(
                pool,
                now,
                &assignment.org_id,
                action,
                &assignment.target_id,
                &assignment.target_id,
                occurrences,
                "warning",
                &format!("policy_id={}", assignment.policy_id),
            );
        }
    }

    /// On the first tick of a new wall-clock minute: the sample of the minute
    /// that just ended (see the module docs).
    fn write_minute_sample(
        &mut self,
        pool: &DbPool,
        assignment: &ActiveAssignment,
        snapshot: &Snapshot,
        now: Now,
    ) {
        let minute = now.wall / MINUTE_MS;
        let Some(state) = self.targets.get_mut(&assignment.target_id) else {
            return;
        };
        match state.last_sample_minute {
            Some(last) if minute == last => return,
            Some(last) if minute < last => {
                // The wall clock stepped back: wait for it to catch up.
                state.last_sample_minute = Some(minute);
                return;
            }
            _ => {}
        }
        let delta = Delta::between(snapshot, &state.sample_base);
        let sample = Sample {
            org_id: assignment.org_id.clone(),
            target_id: assignment.target_id.clone(),
            sampled_at_ms: i64::try_from((minute - 1) * MINUTE_MS).unwrap_or(i64::MAX),
            accepted: delta.accepted,
            rejected_concurrency: delta.rejected_concurrency,
            rejected_calls: delta.rejected_calls,
            timeouts: delta.timeouts,
            errors: delta.errors,
            in_flight_peak: state.in_flight_peak,
            p95_ms: delta.p95_ms.map(|p95| p95.round() as u64),
        };
        match breach::write_sample(pool, &sample) {
            Ok(()) => {
                state.sample_base = *snapshot;
                state.last_sample_minute = Some(minute);
                state.in_flight_peak = 0;
            }
            // The base stays, so the next tick's sample covers this minute too.
            Err(e) => {
                warn!(error = %e, target_id = %assignment.target_id, "SLA sample not written")
            }
        }
    }

    fn raise(&mut self, pool: &DbPool, candidate: &Candidate, now: Now) {
        let at = i64::try_from(now.wall).unwrap_or(i64::MAX);
        match breach::raise_or_refresh(pool, candidate, &self.node_id, at) {
            Ok(RaiseOutcome::Raised(breach)) => {
                self.audit_event(
                    pool,
                    now,
                    &breach.org_id,
                    ACTION_BREACH_RAISED,
                    &breach.dedupe_key(),
                    &breach.target_id,
                    1,
                    "warning",
                    &breach_details(&breach),
                );
            }
            Ok(RaiseOutcome::Refreshed | RaiseOutcome::Unchanged) => {}
            Err(e) => warn!(
                error = %e,
                target_id = %candidate.target_id,
                kind = candidate.kind.as_str(),
                "SLA breach not recorded"
            ),
        }
    }

    /// Resolves every open breach whose condition did not hold this round: its
    /// service was evaluated (or is no longer assigned at all) and its key is
    /// not among those holding. A service that is assigned but had no gate to
    /// read keeps its breaches, nothing was observed about it.
    fn resolve_cleared(
        &mut self,
        pool: &DbPool,
        assigned: &HashSet<String>,
        evaluated: &HashSet<String>,
        holding: &HashSet<String>,
        now: Now,
    ) {
        let open = match breach::open_breaches(pool) {
            Ok(open) => open,
            Err(e) => {
                warn!(error = %e, "SLA evaluator: open breaches could not be read");
                return;
            }
        };
        let at = i64::try_from(now.wall).unwrap_or(i64::MAX);
        for breach in open {
            let judged =
                evaluated.contains(&breach.target_id) || !assigned.contains(&breach.target_id);
            if !judged || holding.contains(&breach.dedupe_key()) {
                continue;
            }
            match breach::resolve(pool, &breach.id, at) {
                Ok(true) => self.audit_event(
                    pool,
                    now,
                    &breach.org_id,
                    ACTION_BREACH_RESOLVED,
                    &breach.dedupe_key(),
                    &breach.target_id,
                    1,
                    "info",
                    &breach_details(&breach),
                ),
                Ok(false) => {}
                Err(e) => warn!(error = %e, breach_id = %breach.id, "SLA breach not resolved"),
            }
        }
    }

    /// Once a minute: drops what is past its retention.
    fn housekeeping(&mut self, pool: &DbPool, now: Now) {
        if self
            .last_housekeeping_ms
            .is_some_and(|last| now.mono.saturating_sub(last) < MINUTE_MS)
        {
            return;
        }
        self.last_housekeeping_ms = Some(now.mono);
        let before = |retention: Duration| {
            i64::try_from(now.wall.saturating_sub(ms(retention))).unwrap_or(i64::MAX)
        };
        match breach::prune(pool, before(SAMPLE_RETENTION), before(BREACH_RETENTION)) {
            Ok(pruned) if pruned != breach::Pruned::default() => {
                debug!(
                    samples = pruned.samples,
                    breaches = pruned.breaches,
                    "SLA history pruned"
                );
            }
            Ok(_) => {}
            Err(e) => warn!(error = %e, "SLA history could not be pruned"),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn audit_event(
        &mut self,
        pool: &DbPool,
        now: Now,
        org_id: &str,
        action: &'static str,
        subject: &str,
        target_id: &str,
        occurrences: u32,
        severity: &str,
        details: &str,
    ) {
        if let Some(count) =
            self.audit
                .record(now.mono, org_id, action, subject, target_id, occurrences)
        {
            self.write_audit(pool, org_id, action, target_id, severity, details, count);
        }
    }

    fn flush_audit(&mut self, pool: &DbPool, now_mono: u64, force: bool) {
        for flush in self.audit.drain(now_mono, force) {
            let severity = if flush.action == ACTION_BREACH_RESOLVED {
                "info"
            } else {
                "warning"
            };
            self.write_audit(
                pool,
                &flush.org_id,
                flush.action,
                &flush.resource,
                severity,
                &format!("subject={}", flush.subject),
                flush.count,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn write_audit(
        &self,
        pool: &DbPool,
        org_id: &str,
        action: &str,
        target_id: &str,
        severity: &str,
        details: &str,
        count: u32,
    ) {
        let details = if details.is_empty() {
            format!("org_id={org_id} count={count}")
        } else {
            format!("org_id={org_id} {details} count={count}")
        };
        if let Err(e) = crate::db::repository::log_audit_full(
            pool,
            None,
            None,
            action,
            Some(SLA_TARGET_FLOW),
            Some(target_id),
            Some(&details),
            severity,
            "A",
            None,
            Some(org_id),
            None,
            Some(&self.node_id),
        ) {
            warn!(error = %e, action, "SLA audit row not written");
        }
    }
}

fn breach_details(breach: &Breach) -> String {
    format!(
        "kind={} policy_id={} threshold={} peak={}",
        breach.kind.as_str(),
        breach.policy_id,
        breach.threshold,
        breach.peak_value
    )
}

/// Starts the node's evaluator. It runs every [`TICK`] until `shutdown` turns
/// `true` (or its sender is dropped), then writes the audit rows still pending.
/// The caller keeps the handle and awaits it (bounded by [`SHUTDOWN_GRACE`])
/// before the database is closed, so that final flush is not lost. The rounds
/// run on the blocking pool: they read and write SQLite.
pub fn spawn(
    pool: DbPool,
    node_id: String,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let evaluator = Evaluator::new(
        node_id,
        system_clock(),
        monotonic_clock(),
        WindowViews::global(),
    );
    run(evaluator, pool, shutdown)
}

fn run(
    evaluator: Evaluator,
    pool: DbPool,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let evaluator = Arc::new(Mutex::new(evaluator));
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                _ = interval.tick() => {
                    let (evaluator, pool) = (evaluator.clone(), pool.clone());
                    let round = tokio::task::spawn_blocking(move || {
                        evaluator.lock().tick(&pool, super::global());
                    });
                    if let Err(e) = round.await {
                        warn!(error = %e, "SLA evaluator round panicked");
                    }
                }
            }
        }
        let _ =
            tokio::task::spawn_blocking(move || evaluator.lock().flush_pending_audit(&pool)).await;
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sla::breach::{BreachQuery, BreachUnit};
    use crate::sla::gate::{AdmitOrigin, Outcome};
    use crate::sla::policy::SlaPolicySpec;
    use crate::sla::repository::{assign, create_policy, test_pool, unassign, update_policy};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Minute-aligned, so a sample's stamp is the clock itself.
    const T0: u64 = 1_700_000_040_000;
    const SECOND: u64 = 1_000;

    struct Fixture {
        pool: DbPool,
        registry: SlaRegistry,
        clock: Arc<AtomicU64>,
        mono: Arc<AtomicU64>,
        evaluator: Evaluator,
        views: Arc<WindowViews>,
    }

    impl Fixture {
        fn new() -> Self {
            let clock = Arc::new(AtomicU64::new(T0));
            let reading = clock.clone();
            let shared: Clock = Arc::new(move || reading.load(Ordering::SeqCst));
            // The monotonic clock starts elsewhere on purpose: nothing may
            // depend on it being the wall clock.
            let mono = Arc::new(AtomicU64::new(5_000));
            let mono_reading = mono.clone();
            let mono_shared: Clock = Arc::new(move || mono_reading.load(Ordering::SeqCst));
            let views = Arc::new(WindowViews::default());
            Self {
                pool: test_pool(),
                registry: SlaRegistry::with_clock(shared.clone()),
                clock,
                mono,
                evaluator: Evaluator::new("node-1", shared, mono_shared, views.clone()),
                views,
            }
        }

        fn advance(&self, millis: u64) {
            self.clock.fetch_add(millis, Ordering::SeqCst);
            self.mono.fetch_add(millis, Ordering::SeqCst);
        }

        fn now(&self) -> u64 {
            self.clock.load(Ordering::SeqCst)
        }

        /// A policy of `org` assigned to `flow`.
        fn govern(&self, org: &str, flow: &str, spec: SlaPolicySpec) -> String {
            let policy = create_policy(&self.pool, org, "admin", &spec).unwrap();
            assign(&self.pool, org, "admin", "flow", flow, &policy.id).unwrap();
            policy.id
        }

        fn tick(&mut self) {
            self.evaluator.tick(&self.pool, &self.registry);
        }

        /// Ticks every 30 s for `duration`.
        fn run(&mut self, duration: Duration) {
            for _ in 0..(ms(duration) / ms(TICK)) {
                self.advance(ms(TICK));
                self.tick();
            }
        }

        fn gate(&self, flow: &str) -> Arc<ServiceGate> {
            self.registry.refresh_if_stale(&self.pool).unwrap();
            self.registry.gate(flow).expect("the flow is governed")
        }

        fn breaches(&self, org: &str) -> Vec<Breach> {
            breach::list_breaches(&self.pool, org, None, BreachQuery::default())
                .unwrap()
                .items
        }

        /// `(org_id, details)` of the audit rows of `action`, oldest first.
        fn audit(&self, action: &str) -> Vec<(Option<String>, String)> {
            let conn = self.pool.read().unwrap();
            let mut stmt = conn
                .prepare("SELECT org_id, details FROM audit_log WHERE action = ?1 ORDER BY id")
                .unwrap();
            stmt.query_map([action], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        }
    }

    fn spec(name: &str) -> SlaPolicySpec {
        SlaPolicySpec {
            name: name.to_string(),
            ..SlaPolicySpec::default()
        }
    }

    async fn call(gate: &Arc<ServiceGate>, outcome: Outcome) {
        gate.admit(AdmitOrigin::Request)
            .await
            .expect("admitted")
            .finish(outcome);
    }

    #[tokio::test]
    async fn a_breach_is_opened_once_refreshed_while_it_lasts_and_resolved_when_it_clears() {
        let mut f = Fixture::new();
        f.govern(
            "org-a",
            "flow-1",
            SlaPolicySpec {
                error_rate_pct: Some(50),
                ..spec("errors")
            },
        );
        let gate = f.gate("flow-1");

        for _ in 0..3 {
            call(&gate, Outcome::Error).await;
        }
        call(&gate, Outcome::Ok).await;
        f.tick();
        let raised = f.breaches("org-a");
        assert_eq!(raised.len(), 1);
        assert_eq!(raised[0].kind, BreachKind::Errors);
        assert_eq!(raised[0].kind.unit(), BreachUnit::Percent);
        assert_eq!(raised[0].threshold, 50.0);
        assert_eq!(raised[0].peak_value, 75.0, "3 of 4 calls failed");
        assert_eq!(raised[0].node_id, "node-1");
        assert_eq!(raised[0].raised_at_ms, T0 as i64);
        assert!(raised[0].is_open());

        // Still failing on the next evaluations: the same row, a higher peak.
        for _ in 0..3 {
            call(&gate, Outcome::Error).await;
        }
        f.run(Duration::from_secs(60));
        let refreshed = f.breaches("org-a");
        assert_eq!(
            refreshed.len(),
            1,
            "one breach row however often it is seen"
        );
        assert_eq!(refreshed[0].id, raised[0].id);
        assert!((refreshed[0].peak_value - 6.0 * 100.0 / 7.0).abs() < 1e-9);
        assert_eq!(refreshed[0].raised_at_ms, raised[0].raised_at_ms);
        assert_eq!(
            f.audit(ACTION_BREACH_RAISED).len(),
            1,
            "raised once, audited once"
        );

        // The failures age out of the 5-minute window: the first evaluation
        // without them resolves the breach.
        f.run(Duration::from_secs(4 * 60));
        assert!(f.breaches("org-a")[0].is_open(), "still inside the window");
        f.run(Duration::from_secs(90));
        let resolved = f.breaches("org-a");
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].id, raised[0].id);
        assert!(resolved[0].resolved_at_ms.is_some());
        assert_eq!(f.audit(ACTION_BREACH_RESOLVED).len(), 1);

        // A new episode is a new breach: the key was freed by the resolution.
        for _ in 0..2 {
            call(&gate, Outcome::Error).await;
        }
        f.advance(ms(TICK));
        f.tick();
        let all = f.breaches("org-a");
        assert_eq!(all.len(), 2);
        assert_eq!(all.iter().filter(|b| b.is_open()).count(), 1);
    }

    #[tokio::test]
    async fn the_latency_threshold_is_judged_on_the_window_p95() {
        let mut f = Fixture::new();
        f.govern(
            "org-a",
            "flow-1",
            SlaPolicySpec {
                latency_p95_ms: Some(10),
                ..spec("latency")
            },
        );
        let gate = f.gate("flow-1");
        for _ in 0..10 {
            call(&gate, Outcome::Ok).await;
        }
        f.tick();
        assert!(
            f.breaches("org-a").is_empty(),
            "instant calls are fast enough"
        );
        assert!(
            f.views.get("flow-1").unwrap().p95_ms.unwrap() <= 1.0,
            "instant calls sit in the first bucket"
        );

        let mut slow = gate.admit(AdmitOrigin::Request).await.unwrap();
        std::thread::sleep(Duration::from_millis(30));
        slow.set_outcome(Outcome::Ok);
        drop(slow);
        for _ in 0..3 {
            let mut slow = gate.admit(AdmitOrigin::Request).await.unwrap();
            std::thread::sleep(Duration::from_millis(30));
            slow.set_outcome(Outcome::Ok);
        }
        f.advance(ms(TICK));
        f.tick();
        let breaches = f.breaches("org-a");
        assert_eq!(breaches.len(), 1);
        assert_eq!(breaches[0].kind, BreachKind::Latency);
        assert_eq!(breaches[0].kind.unit(), BreachUnit::Milliseconds);
        // The slow calls took at least 30 ms (a sleep may overshoot, never
        // undershoot); how far above is up to the scheduler.
        assert!(
            breaches[0].peak_value >= 30.0 && breaches[0].peak_value > 10.0,
            "estimated p95 {}",
            breaches[0].peak_value
        );
    }

    #[tokio::test]
    async fn a_timed_out_call_raises_a_timeouts_breach() {
        let mut f = Fixture::new();
        f.govern("org-a", "flow-1", spec("timeouts"));
        let gate = f.gate("flow-1");
        call(&gate, Outcome::Ok).await;
        f.tick();
        assert!(f.breaches("org-a").is_empty());
        call(&gate, Outcome::Timeout).await;
        f.advance(ms(TICK));
        f.tick();
        let breaches = f.breaches("org-a");
        assert_eq!(breaches.len(), 1);
        assert_eq!(breaches[0].kind, BreachKind::Timeouts);
        assert_eq!(breaches[0].peak_value, 1.0);
    }

    #[tokio::test]
    async fn concurrency_saturation_needs_a_full_minute_of_saturated_ticks() {
        let mut f = Fixture::new();
        f.govern(
            "org-a",
            "flow-1",
            SlaPolicySpec {
                max_concurrency: Some(1),
                ..spec("threads")
            },
        );
        let gate = f.gate("flow-1");
        let held = gate.admit(AdmitOrigin::Request).await.unwrap();

        f.tick();
        f.advance(30 * SECOND);
        f.tick();
        assert!(
            f.breaches("org-a").is_empty(),
            "saturated for 30 s is not enough"
        );
        f.advance(30 * SECOND);
        f.tick();
        let breaches = f.breaches("org-a");
        assert_eq!(breaches.len(), 1, "60 s of saturation opens the breach");
        assert_eq!(breaches[0].kind, BreachKind::ConcurrencySaturated);
        assert_eq!(breaches[0].threshold, 1.0);

        drop(held);
        f.advance(30 * SECOND);
        f.tick();
        assert!(
            !f.breaches("org-a")[0].is_open(),
            "the first tick that sees a free slot resolves it"
        );
    }

    #[tokio::test]
    async fn a_slot_seen_free_restarts_the_saturation_run() {
        let mut f = Fixture::new();
        f.govern(
            "org-a",
            "flow-1",
            SlaPolicySpec {
                max_concurrency: Some(1),
                ..spec("threads")
            },
        );
        let gate = f.gate("flow-1");
        let held = gate.admit(AdmitOrigin::Request).await.unwrap();
        f.tick();
        f.advance(30 * SECOND);
        f.tick();
        drop(held);
        f.advance(30 * SECOND);
        f.tick();
        let held = gate.admit(AdmitOrigin::Request).await.unwrap();
        f.advance(30 * SECOND);
        f.tick();
        f.advance(30 * SECOND);
        f.tick();
        assert!(
            f.breaches("org-a").is_empty(),
            "only 30 s of the new run have passed, the earlier 60 s do not count"
        );
        drop(held);
    }

    #[tokio::test]
    async fn the_call_limit_is_read_from_the_window_usage_not_from_refusals() {
        let mut f = Fixture::new();
        f.govern(
            "org-a",
            "flow-1",
            SlaPolicySpec {
                call_limit: Some(10),
                call_window_secs: Some(3600),
                warn_at_pct: Some(80),
                ..spec("calls")
            },
        );
        let gate = f.gate("flow-1");
        for _ in 0..8 {
            call(&gate, Outcome::Ok).await;
        }
        f.tick();
        let kinds = |f: &Fixture| {
            let mut kinds: Vec<_> = f
                .breaches("org-a")
                .into_iter()
                .filter(|b| b.is_open())
                .map(|b| b.kind)
                .collect();
            kinds.sort();
            kinds
        };
        assert_eq!(kinds(&f), vec![BreachKind::CallsWarn]);
        let warn = &f.breaches("org-a")[0];
        assert_eq!((warn.threshold, warn.peak_value), (80.0, 80.0));

        // Filling the window exactly: nobody was refused, yet the limit is reached.
        for _ in 0..2 {
            call(&gate, Outcome::Ok).await;
        }
        f.advance(ms(TICK));
        f.tick();
        assert_eq!(
            kinds(&f),
            vec![BreachKind::CallsWarn, BreachKind::CallsLimit]
        );
        assert_eq!(gate.stats().rejected_calls, 0);
        let limit = f
            .breaches("org-a")
            .into_iter()
            .find(|b| b.kind == BreachKind::CallsLimit)
            .unwrap();
        assert_eq!((limit.threshold, limit.peak_value), (10.0, 10.0));
        assert_eq!(limit.kind.unit(), BreachUnit::Calls);
    }

    #[tokio::test]
    async fn a_short_window_that_rolled_over_before_the_tick_is_still_seen_through_its_peak() {
        let mut f = Fixture::new();
        f.govern(
            "org-a",
            "flow-1",
            SlaPolicySpec {
                call_limit: Some(3),
                call_window_secs: Some(1),
                ..spec("burst")
            },
        );
        let gate = f.gate("flow-1");
        for _ in 0..3 {
            call(&gate, Outcome::Ok).await;
        }
        f.advance(5 * SECOND);
        assert_eq!(
            gate.stats().window_calls,
            0,
            "the 1 s window is empty again"
        );
        f.tick();
        let breaches = f.breaches("org-a");
        assert_eq!(breaches.len(), 1);
        assert_eq!(breaches[0].kind, BreachKind::CallsLimit);
        assert_eq!(breaches[0].peak_value, 3.0);

        // Nothing since: the peak leaves the evaluation window and it resolves.
        f.run(Duration::from_secs(6 * 60));
        assert!(!f.breaches("org-a")[0].is_open());
    }

    #[tokio::test]
    async fn removing_the_assignment_or_the_threshold_resolves_the_breach() {
        let mut f = Fixture::new();
        f.govern(
            "org-a",
            "flow-1",
            SlaPolicySpec {
                error_rate_pct: Some(10),
                ..spec("errors")
            },
        );
        f.govern(
            "org-a",
            "flow-2",
            SlaPolicySpec {
                error_rate_pct: Some(10),
                ..spec("errors 2")
            },
        );
        for flow in ["flow-1", "flow-2"] {
            call(&f.gate(flow), Outcome::Error).await;
        }
        f.tick();
        assert_eq!(
            f.breaches("org-a").iter().filter(|b| b.is_open()).count(),
            2
        );

        // flow-1 loses its policy.
        assert!(unassign(&f.pool, "org-a", "admin", "flow", "flow-1").unwrap().is_some());
        f.advance(ms(TICK));
        f.tick();
        let open: Vec<_> = f
            .breaches("org-a")
            .into_iter()
            .filter(|b| b.is_open())
            .collect();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].target_id, "flow-2");
        assert_eq!(f.audit(ACTION_BREACH_RESOLVED).len(), 1);
        assert!(
            f.views.get("flow-1").is_none(),
            "the view of a removed service is dropped"
        );

        // flow-2's policy stops defining the threshold.
        let flow_2_policy =
            crate::sla::repository::get_assignment(&f.pool, "org-a", "flow", "flow-2")
                .unwrap()
                .unwrap()
                .policy_id;
        update_policy(&f.pool, "org-a", "admin", &flow_2_policy, &spec("errors 2")).unwrap();
        f.advance(ms(TICK));
        f.tick();
        assert!(f.breaches("org-a").iter().all(|b| !b.is_open()));
    }

    #[tokio::test]
    async fn breaches_and_their_audit_rows_carry_the_organization_of_the_policy() {
        let mut f = Fixture::new();
        for (org, flow) in [("org-a", "flow-1"), ("org-b", "flow-2")] {
            f.govern(
                org,
                flow,
                SlaPolicySpec {
                    error_rate_pct: Some(10),
                    call_limit: Some(1),
                    call_window_secs: Some(3600),
                    ..spec("p")
                },
            );
            let gate = f.gate(flow);
            call(&gate, Outcome::Error).await;
            assert!(gate.admit(AdmitOrigin::Request).await.is_err());
        }
        f.tick();

        for (org, flow) in [("org-a", "flow-1"), ("org-b", "flow-2")] {
            let breaches = f.breaches(org);
            assert!(!breaches.is_empty());
            assert!(
                breaches
                    .iter()
                    .all(|b| b.org_id == org && b.target_id == flow),
                "{org} sees only its own breaches"
            );
        }
        assert!(
            breach::list_breaches(&f.pool, "org-c", None, BreachQuery::default())
                .unwrap()
                .items
                .is_empty()
        );
        for action in [ACTION_BREACH_RAISED, ACTION_CALLS_REJECTED] {
            let rows = f.audit(action);
            let orgs: std::collections::BTreeSet<_> =
                rows.iter().map(|(org, _)| org.clone().unwrap()).collect();
            assert_eq!(
                orgs.into_iter().collect::<Vec<_>>(),
                ["org-a", "org-b"],
                "{action}: rows of both orgs, stamped"
            );
            for (org, details) in rows {
                assert!(
                    details.contains(&format!("org_id={}", org.unwrap())),
                    "details: {details}"
                );
            }
        }
    }

    #[tokio::test]
    async fn rejections_are_audited_through_a_window_without_flooding_the_log() {
        let mut f = Fixture::new();
        f.govern(
            "org-a",
            "flow-1",
            SlaPolicySpec {
                call_limit: Some(1),
                call_window_secs: Some(3600),
                max_concurrency: Some(1),
                max_queue: Some(0),
                ..spec("limits")
            },
        );
        let gate = f.gate("flow-1");
        let held = gate.admit(AdmitOrigin::Request).await.unwrap();
        for _ in 0..3 {
            assert!(gate.admit(AdmitOrigin::Request).await.is_err());
        }
        f.tick();
        let rows = f.audit(ACTION_CALLS_REJECTED);
        assert_eq!(
            rows.len(),
            1,
            "the first rejections of a window are written at once"
        );
        assert!(rows[0].1.ends_with("count=3"), "{}", rows[0].1);
        assert_eq!(rows[0].0.as_deref(), Some("org-a"));

        // Two more inside the same 60 s window are only counted ...
        for _ in 0..2 {
            assert!(gate.admit(AdmitOrigin::Request).await.is_err());
        }
        f.advance(30 * SECOND);
        f.tick();
        assert_eq!(f.audit(ACTION_CALLS_REJECTED).len(), 1);
        // ... and folded into one row when the window has ended.
        f.advance(30 * SECOND);
        f.tick();
        let rows = f.audit(ACTION_CALLS_REJECTED);
        assert_eq!(rows.len(), 2);
        assert!(rows[1].1.ends_with("count=2"), "{}", rows[1].1);
        drop(held);
    }

    #[tokio::test]
    async fn a_full_concurrency_queue_is_audited_as_a_concurrency_rejection() {
        let mut f = Fixture::new();
        f.govern(
            "org-a",
            "flow-1",
            SlaPolicySpec {
                max_concurrency: Some(1),
                max_queue: Some(0),
                ..spec("threads")
            },
        );
        let gate = f.gate("flow-1");
        let held = gate.admit(AdmitOrigin::Request).await.unwrap();
        assert!(gate.admit(AdmitOrigin::Request).await.is_err());
        f.tick();
        assert_eq!(f.audit(ACTION_CONCURRENCY_REJECTED).len(), 1);
        assert!(f.audit(ACTION_CALLS_REJECTED).is_empty());
        let window = f.views.get("flow-1").unwrap();
        assert_eq!((window.rejected_concurrency, window.rejected_calls), (1, 0));
        drop(held);
    }

    #[tokio::test]
    async fn a_minute_sample_is_written_on_the_first_tick_of_the_next_minute_and_old_ones_pruned() {
        let mut f = Fixture::new();
        f.govern("org-a", "flow-1", spec("samples"));
        let gate = f.gate("flow-1");
        for _ in 0..3 {
            call(&gate, Outcome::Ok).await;
        }
        call(&gate, Outcome::Error).await;
        f.tick();
        let samples = |f: &Fixture| breach::list_samples(&f.pool, "org-a", "flow-1", 0).unwrap();
        assert!(samples(&f).is_empty(), "the first sighting writes nothing");

        f.advance(30 * SECOND);
        f.tick();
        assert!(samples(&f).is_empty(), "still the same minute");

        call(&gate, Outcome::Ok).await;
        f.advance(30 * SECOND);
        f.tick();
        let first = samples(&f);
        assert_eq!(first.len(), 1);
        assert_eq!(
            first[0].sampled_at_ms, T0 as i64,
            "stamped with the minute that just ended"
        );
        assert_eq!(
            (first[0].accepted, first[0].errors, first[0].timeouts),
            (5, 1, 0)
        );
        assert!(first[0].p95_ms.unwrap() <= 1);
        assert_eq!(first[0].org_id, "org-a");

        f.advance(30 * SECOND);
        f.tick();
        assert_eq!(samples(&f).len(), 1, "the same minute is not sampled twice");

        call(&gate, Outcome::Ok).await;
        f.advance(30 * SECOND);
        f.tick();
        let two = samples(&f);
        assert_eq!(two.len(), 2);
        assert_eq!(two[1].sampled_at_ms, (T0 + 60 * SECOND) as i64);
        assert_eq!(
            two[1].accepted, 1,
            "a sample counts what happened since the last one"
        );

        // Seven days on, the old ones are past retention. Only the sample just
        // taken remains.
        f.advance(7 * 24 * 3600 * SECOND);
        f.tick();
        let kept = samples(&f);
        assert_eq!(kept.len(), 1);
        assert_eq!(
            kept[0].sampled_at_ms as u64 / MINUTE_MS,
            f.now() / MINUTE_MS - 1
        );
    }

    #[tokio::test]
    async fn resolved_breaches_are_kept_for_90_days_and_open_ones_for_ever() {
        let mut f = Fixture::new();
        f.govern(
            "org-a",
            "flow-1",
            SlaPolicySpec {
                error_rate_pct: Some(10),
                ..spec("errors")
            },
        );
        let gate = f.gate("flow-1");
        call(&gate, Outcome::Error).await;
        f.tick();
        assert_eq!(f.breaches("org-a").len(), 1);
        // Clear it: five clean minutes.
        f.run(Duration::from_secs(6 * 60));
        assert!(!f.breaches("org-a")[0].is_open());

        f.advance(89 * 24 * 3600 * SECOND);
        f.tick();
        assert_eq!(f.breaches("org-a").len(), 1, "89 days after: kept");
        f.advance(2 * 24 * 3600 * SECOND);
        f.tick();
        assert!(f.breaches("org-a").is_empty(), "91 days after: pruned");

        // An open breach is not pruned however old it is.
        call(&gate, Outcome::Error).await;
        f.advance(ms(TICK));
        f.tick();
        assert_eq!(f.breaches("org-a").len(), 1);
        let pruned = breach::prune(&f.pool, i64::MAX, i64::MAX).unwrap();
        assert_eq!(pruned.breaches, 0);
        assert_eq!(breach::count_open(&f.pool).unwrap(), 1);
    }

    #[tokio::test]
    async fn status_reports_the_gate_the_window_and_the_open_breaches_of_one_organization() {
        let mut f = Fixture::new();
        f.govern(
            "org-a",
            "flow-1",
            SlaPolicySpec {
                max_concurrency: Some(2),
                call_limit: Some(10),
                call_window_secs: Some(60),
                error_rate_pct: Some(10),
                ..spec("status")
            },
        );
        let gate = f.gate("flow-1");
        call(&gate, Outcome::Error).await;
        let held = gate.admit(AdmitOrigin::Request).await.unwrap();
        f.tick();

        let status =
            crate::sla::status::service_status(&f.pool, &f.registry, &f.views, "org-a", "flow-1")
                .unwrap();
        assert_eq!((status.in_flight, status.max_concurrency), (1, Some(2)));
        let window = status.call_window.unwrap();
        assert_eq!((window.used, window.limit, window.window_secs), (2, 10, 60));
        assert_eq!(window.resets_at_ms, T0 + 60 * SECOND);
        assert_eq!(status.window.unwrap().error_rate_pct, Some(100.0));
        assert_eq!(status.open_breaches.len(), 1);
        assert_eq!(status.open_breaches[0].kind, BreachKind::Errors);

        let other =
            crate::sla::status::service_status(&f.pool, &f.registry, &f.views, "org-b", "flow-1");
        assert_eq!(other.unwrap_err().code(), "not_found");
        assert!(
            crate::sla::status::list_service_statuses(&f.pool, &f.registry, &f.views, "org-b")
                .unwrap()
                .is_empty()
        );
        let all =
            crate::sla::status::list_service_statuses(&f.pool, &f.registry, &f.views, "org-a")
                .unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].open_breaches.len(), 1);
        drop(held);
    }

    #[test]
    fn breaches_are_listed_newest_first_in_pages_pinned_to_the_organization() {
        let pool = test_pool();
        for (i, kind) in [
            BreachKind::Latency,
            BreachKind::Errors,
            BreachKind::Timeouts,
        ]
        .into_iter()
        .enumerate()
        {
            let candidate = |org: &str| Candidate {
                org_id: org.into(),
                target_id: "flow-1".into(),
                policy_id: "p".into(),
                kind,
                threshold: 1.0,
                value: 2.0,
            };
            breach::raise_or_refresh(&pool, &candidate("org-a"), "n", 100 + i as i64).unwrap();
            breach::raise_or_refresh(&pool, &candidate("org-b"), "n", 100 + i as i64).unwrap();
        }
        let first = breach::list_breaches(
            &pool,
            "org-a",
            None,
            BreachQuery {
                open_only: false,
                offset: 0,
                limit: 2,
            },
        )
        .unwrap();
        assert_eq!(first.total, 3);
        assert_eq!(
            first.items.iter().map(|b| b.kind).collect::<Vec<_>>(),
            [BreachKind::Timeouts, BreachKind::Errors]
        );
        let second = breach::list_breaches(
            &pool,
            "org-a",
            None,
            BreachQuery {
                open_only: false,
                offset: 2,
                limit: 2,
            },
        )
        .unwrap();
        assert_eq!(second.items.len(), 1);
        assert!(second.items.iter().all(|b| b.org_id == "org-a"));

        assert!(breach::resolve(&pool, &first.items[0].id, 500).unwrap());
        assert!(
            !breach::resolve(&pool, &first.items[0].id, 600).unwrap(),
            "resolved once"
        );
        let open = breach::list_breaches(
            &pool,
            "org-a",
            Some("flow-1"),
            BreachQuery {
                open_only: true,
                ..BreachQuery::default()
            },
        )
        .unwrap();
        assert_eq!(open.total, 2);
        let none =
            breach::list_breaches(&pool, "org-a", Some("flow-2"), BreachQuery::default()).unwrap();
        assert_eq!(none.total, 0);
    }

    #[test]
    fn the_audit_window_writes_the_first_occurrence_and_folds_the_rest_per_subject() {
        let mut audit = WindowedAudit::new(Duration::from_secs(60));
        assert_eq!(audit.record(0, "o", "a", "s1", "r1", 1), Some(1));
        assert_eq!(audit.record(10_000, "o", "a", "s1", "r1", 4), None);
        assert_eq!(
            audit.record(15_000, "o", "a", "s2", "r2", 1),
            Some(1),
            "another subject has its own window"
        );
        assert_eq!(
            audit.record(20_000, "p", "a", "s1", "r3", 1),
            Some(1),
            "per organization"
        );
        assert!(
            audit.drain(30_000, false).is_empty(),
            "windows are still open"
        );
        let flushed = audit.drain(61_000, false);
        assert_eq!(
            flushed.len(),
            1,
            "only the elapsed window that suppressed something"
        );
        assert_eq!(
            (
                flushed[0].count,
                flushed[0].subject.as_str(),
                flushed[0].resource.as_str()
            ),
            (4, "s1", "r1")
        );
        assert_eq!(
            audit.record(70_000, "o", "a", "s1", "r1", 1),
            Some(1),
            "a new window starts"
        );
    }

    #[test]
    fn the_estimated_p95_stays_inside_its_bucket_around_the_observed_mean() {
        let histogram = LatencyHistogram::default();
        let zero = histogram.snapshot();
        for _ in 0..40 {
            histogram.record(Duration::from_millis(210));
        }
        let p95 = histogram.snapshot().since(&zero).estimate_p95_ms().unwrap();
        assert!(
            (210.0..250.0).contains(&p95),
            "210 ms calls must not read as the 300 ms bucket bound: {p95}"
        );
        assert_eq!(
            histogram.snapshot().p95_ms(),
            Some(300),
            "the coarse reading is the bound"
        );

        // Spread over the whole bucket the estimate reaches the top of it.
        let spread = LatencyHistogram::default();
        for millis in 201..=300 {
            spread.record(Duration::from_millis(millis));
        }
        let p95 = spread.snapshot().estimate_p95_ms().unwrap();
        assert!((290.0..=300.0).contains(&p95), "{p95}");

        // Calls above the last bound: the answer is that bucket's mean.
        let overflow = LatencyHistogram::default();
        overflow.record(Duration::from_secs(2 * 3600));
        assert_eq!(
            overflow.snapshot().estimate_p95_ms().unwrap().round(),
            7_200_000.0
        );
        assert_eq!(
            LatencyHistogram::default().snapshot().estimate_p95_ms(),
            None
        );
    }

    #[tokio::test]
    async fn a_reservation_given_back_is_not_a_call_nor_a_running_slot() {
        // The bus reactor in at-most-once mode reserves `parallelism` permits
        // before every fetch and gives them back after an empty poll.
        let mut f = Fixture::new();
        f.govern(
            "org-a",
            "flow-1",
            SlaPolicySpec {
                max_concurrency: Some(3),
                call_limit: Some(3),
                call_window_secs: Some(60),
                warn_at_pct: Some(50),
                ..spec("bus")
            },
        );
        let gate = f.gate("flow-1");
        for _ in 0..5 {
            let mut held = Vec::new();
            for _ in 0..3 {
                held.push(gate.admit(AdmitOrigin::Bus).await.unwrap());
            }
            assert_eq!(gate.stats().in_flight, 3, "the slots are held");
            // A tick lands while the cycle is waiting for records.
            f.advance(30 * SECOND);
            f.tick();
            assert_eq!(gate.stats().running, 0);
            for permit in held {
                permit.forgo();
            }
        }
        assert!(
            f.breaches("org-a").is_empty(),
            "an idle consumer is neither at its call limit nor saturated: {:?}",
            f.breaches("org-a")
        );
        let status =
            crate::sla::status::service_status(&f.pool, &f.registry, &f.views, "org-a", "flow-1")
                .unwrap();
        assert_eq!(status.in_flight, 0);
        assert_eq!(status.call_window.unwrap().used, 0);
        assert_eq!(gate.take_window_peak(), 0);
        let samples = breach::list_samples(&f.pool, "org-a", "flow-1", 0).unwrap();
        assert!(!samples.is_empty() && samples.iter().all(|s| s.in_flight_peak == 0));
        assert_eq!(
            crate::sla::status::list_service_statuses(&f.pool, &f.registry, &f.views, "org-a")
                .unwrap()[0]
                .in_flight,
            0
        );
    }

    #[tokio::test]
    async fn bus_permits_that_began_count_as_calls_and_as_running() {
        let mut f = Fixture::new();
        f.govern(
            "org-a",
            "flow-1",
            SlaPolicySpec {
                max_concurrency: Some(2),
                call_limit: Some(2),
                call_window_secs: Some(3600),
                ..spec("bus")
            },
        );
        let gate = f.gate("flow-1");
        let mut first = gate.admit(AdmitOrigin::Bus).await.unwrap();
        let mut second = gate.admit(AdmitOrigin::Bus).await.unwrap();
        first.begin();
        first.begin();
        second.begin();
        assert_eq!(gate.stats().running, 2, "begin counts a permit once");
        f.tick();
        f.advance(30 * SECOND);
        f.tick();
        f.advance(30 * SECOND);
        f.tick();
        let mut kinds: Vec<_> = f.breaches("org-a").into_iter().map(|b| b.kind).collect();
        kinds.sort();
        assert_eq!(
            kinds,
            [BreachKind::CallsLimit, BreachKind::ConcurrencySaturated]
        );
        drop((first, second));
        assert_eq!(gate.stats().running, 0);
    }

    #[tokio::test]
    async fn a_draining_gate_that_comes_back_does_not_replay_its_lifetime() {
        let mut f = Fixture::new();
        let policy_id = f.govern(
            "org-a",
            "flow-1",
            SlaPolicySpec {
                error_rate_pct: Some(10),
                ..spec("errors")
            },
        );
        let gate = f.gate("flow-1");
        let held = gate.admit(AdmitOrigin::Request).await.unwrap();
        call(&gate, Outcome::Error).await;
        f.tick();
        assert_eq!(f.audit(ACTION_BREACH_RAISED).len(), 1);

        // Unassigned while a call is still in flight: the gate drains.
        assert!(unassign(&f.pool, "org-a", "admin", "flow", "flow-1").unwrap().is_some());
        f.advance(30 * SECOND);
        f.tick();
        assert!(f.breaches("org-a").iter().all(|b| !b.is_open()));
        f.advance(20 * 60 * SECOND);
        assign(&f.pool, "org-a", "admin", "flow", "flow-1", &policy_id).unwrap();
        f.tick();
        assert!(
            Arc::ptr_eq(&gate, &f.gate("flow-1")),
            "the draining gate came back"
        );

        f.advance(30 * SECOND);
        f.tick();
        assert_eq!(
            f.breaches("org-a").len(),
            1,
            "the old errors are not news again"
        );
        assert_eq!(f.audit(ACTION_BREACH_RAISED).len(), 1);
        drop(held);
    }

    #[tokio::test]
    async fn a_saturation_run_does_not_survive_a_gap_in_the_evaluation() {
        let mut f = Fixture::new();
        let policy_id = f.govern(
            "org-a",
            "flow-1",
            SlaPolicySpec {
                max_concurrency: Some(1),
                ..spec("threads")
            },
        );
        let gate = f.gate("flow-1");
        let held = gate.admit(AdmitOrigin::Request).await.unwrap();
        f.tick();
        f.advance(30 * SECOND);
        f.tick();
        assert!(f.breaches("org-a").is_empty());

        assert!(unassign(&f.pool, "org-a", "admin", "flow", "flow-1").unwrap().is_some());
        f.advance(30 * SECOND);
        f.tick();
        f.advance(20 * 60 * SECOND);
        assign(&f.pool, "org-a", "admin", "flow", "flow-1", &policy_id).unwrap();
        f.tick();
        assert!(
            Arc::ptr_eq(&gate, &f.gate("flow-1")),
            "the same gate came back"
        );
        assert!(
            f.breaches("org-a").is_empty(),
            "the minute of saturation before the gap is not carried over it"
        );
        f.advance(30 * SECOND);
        f.tick();
        f.advance(30 * SECOND);
        f.tick();
        assert_eq!(f.breaches("org-a").len(), 1, "a fresh minute of it is");
        drop(held);
    }

    #[test]
    fn a_stored_minute_is_not_overwritten_by_a_later_write_for_the_same_minute() {
        let pool = test_pool();
        let sample = |accepted| Sample {
            org_id: "org-a".into(),
            target_id: "flow-1".into(),
            sampled_at_ms: T0 as i64,
            accepted,
            rejected_concurrency: 0,
            rejected_calls: 0,
            timeouts: 0,
            errors: 0,
            in_flight_peak: 0,
            p95_ms: None,
        };
        breach::write_sample(&pool, &sample(5)).unwrap();
        breach::write_sample(&pool, &sample(9)).unwrap();
        let stored = breach::list_samples(&pool, "org-a", "flow-1", 0).unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].accepted, 5, "the first write of a minute stands");
    }

    #[tokio::test]
    async fn a_gate_that_was_already_running_is_baselined_at_its_current_stats() {
        let mut f = Fixture::new();
        f.govern(
            "org-a",
            "flow-1",
            SlaPolicySpec {
                error_rate_pct: Some(10),
                ..spec("errors")
            },
        );
        let gate = f.gate("flow-1");
        for _ in 0..5 {
            call(&gate, Outcome::Error).await;
        }
        // An evaluator that starts after the gate was built.
        let mut late = Evaluator::new(
            "node-1",
            f.evaluator.wall.clone(),
            f.evaluator.mono.clone(),
            Arc::new(WindowViews::default()),
        );
        late.started = Instant::now() + Duration::from_secs(1);
        late.tick(&f.pool, &f.registry);
        assert!(
            f.breaches("org-a").is_empty(),
            "history before the first sighting is not judged"
        );
        call(&gate, Outcome::Error).await;
        f.advance(30 * SECOND);
        late.tick(&f.pool, &f.registry);
        assert_eq!(f.breaches("org-a").len(), 1, "what happens afterwards is");
    }

    #[tokio::test]
    async fn a_peak_measured_under_an_older_limit_is_not_judged_against_the_new_one() {
        let mut f = Fixture::new();
        let policy_id = f.govern(
            "org-a",
            "flow-1",
            SlaPolicySpec {
                call_limit: Some(100),
                call_window_secs: Some(1),
                ..spec("burst")
            },
        );
        let gate = f.gate("flow-1");
        for _ in 0..50 {
            call(&gate, Outcome::Ok).await;
        }
        f.tick();
        assert!(f.breaches("org-a").is_empty(), "50 of 100");

        f.advance(5 * SECOND);
        update_policy(
            &f.pool,
            "org-a",
            "admin",
            &policy_id,
            &SlaPolicySpec {
                call_limit: Some(10),
                call_window_secs: Some(1),
                ..spec("burst")
            },
        )
        .unwrap();
        f.tick();
        assert!(
            f.breaches("org-a").is_empty(),
            "the 50 calls were within the old limit: {:?}",
            f.breaches("org-a")
        );
        for _ in 0..10 {
            call(&gate, Outcome::Ok).await;
        }
        f.advance(ms(TICK));
        f.tick();
        assert_eq!(
            f.breaches("org-a").len(),
            1,
            "the new limit is judged on its own peaks"
        );
    }

    #[tokio::test]
    async fn a_changed_organization_of_the_assignment_starts_the_service_over() {
        let mut f = Fixture::new();
        f.govern(
            "org-a",
            "flow-1",
            SlaPolicySpec {
                error_rate_pct: Some(10),
                ..spec("errors")
            },
        );
        let gate = f.gate("flow-1");
        call(&gate, Outcome::Error).await;
        f.tick();
        assert!(unassign(&f.pool, "org-a", "admin", "flow", "flow-1").unwrap().is_some());
        let other = create_policy(
            &f.pool,
            "org-b",
            "admin",
            &SlaPolicySpec {
                error_rate_pct: Some(10),
                ..spec("errors")
            },
        )
        .unwrap();
        assign(&f.pool, "org-b", "admin", "flow", "flow-1", &other.id).unwrap();
        f.advance(30 * SECOND);
        f.tick();
        assert!(
            f.breaches("org-a").iter().all(|b| !b.is_open()),
            "org-a's breach is closed"
        );
        assert!(
            f.breaches("org-b").is_empty(),
            "org-b did not make those calls; its history starts now"
        );
    }

    #[tokio::test]
    async fn breach_audit_rows_of_different_breaches_do_not_share_a_window() {
        let mut f = Fixture::new();
        for flow in ["flow-1", "flow-2"] {
            f.govern(
                "org-a",
                flow,
                SlaPolicySpec {
                    error_rate_pct: Some(10),
                    ..spec(flow)
                },
            );
            call(&f.gate(flow), Outcome::Error).await;
        }
        f.tick();
        let rows = f.audit(ACTION_BREACH_RAISED);
        assert_eq!(
            rows.len(),
            2,
            "two breaches in one window, two rows: {rows:?}"
        );
    }

    #[tokio::test]
    async fn the_task_flushes_pending_audit_rows_and_ends_on_shutdown() {
        let f = Fixture::new();
        let mut evaluator = Evaluator::new(
            "node-1",
            system_clock(),
            monotonic_clock(),
            Arc::new(WindowViews::default()),
        );
        // A burst whose tail is still only counted when the node stops.
        assert_eq!(
            evaluator
                .audit
                .record(0, "org-a", ACTION_CALLS_REJECTED, "flow-1", "flow-1", 1),
            Some(1)
        );
        assert_eq!(
            evaluator
                .audit
                .record(1, "org-a", ACTION_CALLS_REJECTED, "flow-1", "flow-1", 6),
            None
        );

        let (stop, signal) = tokio::sync::watch::channel(false);
        let handle = run(evaluator, f.pool.clone(), signal);
        stop.send(true).unwrap();
        tokio::time::timeout(SHUTDOWN_GRACE, handle)
            .await
            .expect("the task ends in time")
            .unwrap();
        let rows = f.audit(ACTION_CALLS_REJECTED);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].1.ends_with("count=6"), "{}", rows[0].1);
        assert_eq!(rows[0].0.as_deref(), Some("org-a"));
    }
}
