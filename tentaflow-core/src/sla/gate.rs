//! Enforcement of one service's SLA limits: a concurrency semaphore with a
//! bounded waiting queue, a clock-aligned fixed call window, outcome counters
//! and a fixed-bucket latency histogram.
//!
//! Everything here is in memory and per node. A `SlaPermit` is the RAII handle
//! of one admitted call: dropping it releases the slot and records the outcome,
//! so a cancelled future or a panicking task cannot leak a slot.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};
use tokio::sync::Semaphore;

use super::policy::SlaPolicySpec;

/// How long a call waits for a free slot when the policy does not say.
pub const DEFAULT_QUEUE_TIMEOUT: Duration = Duration::from_secs(10);

/// Hint returned with a concurrency refusal; a slot frees as soon as any call
/// in flight finishes, so a short pause is the honest answer.
const CONCURRENCY_RETRY_AFTER_MS: u64 = 1_000;

/// Milliseconds since the Unix epoch. Injected so the window boundary can be
/// tested without sleeping.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

pub fn system_clock() -> Clock {
    Arc::new(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    })
}

/// Where a call came from. A request edge answers a refusal with a 429, the bus
/// reactor sleeps `retry_after_ms` and tries again, so the refusal carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmitOrigin {
    Request,
    Bus,
    Background,
}

/// The limits one gate enforces, with the defaults of the plan already applied
/// (decision P4: a queue as long as the concurrency limit, waiting up to 10 s).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateLimits {
    pub max_concurrency: Option<u32>,
    pub max_queue: u32,
    pub queue_timeout: Duration,
    /// `(calls, window in seconds)`.
    pub call_limit: Option<(u32, u32)>,
    /// Execution timeout the dispatcher applies to an admitted call.
    pub timeout: Option<Duration>,
}

impl GateLimits {
    pub fn from_spec(spec: &SlaPolicySpec) -> Self {
        Self {
            max_concurrency: spec.max_concurrency,
            max_queue: spec.max_queue.unwrap_or(spec.max_concurrency.unwrap_or(0)),
            queue_timeout: spec.queue_timeout_ms.map_or(DEFAULT_QUEUE_TIMEOUT, |ms| {
                Duration::from_millis(u64::from(ms))
            }),
            call_limit: spec.call_limit.zip(spec.call_window_secs),
            timeout: spec
                .timeout_ms
                .map(|ms| Duration::from_millis(u64::from(ms))),
        }
        .normalized()
    }

    /// A zero-second window would divide by zero when the window epoch is
    /// computed. The schema and `validate` forbid it; a gate still never trusts
    /// that, and reads it as the shortest real window.
    pub fn normalized(mut self) -> Self {
        if let Some((_, window)) = &mut self.call_limit {
            *window = (*window).max(1);
        }
        self
    }

    fn capacity(&self) -> usize {
        self.max_concurrency
            .map_or(Semaphore::MAX_PERMITS, |n| n as usize)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateConfig {
    pub policy_id: String,
    pub org_id: String,
    pub limits: GateLimits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalReason {
    /// The call window is used up.
    CallLimit,
    /// All slots are taken and the waiting queue is full.
    QueueFull,
    /// The call waited `queue_timeout` and no slot freed.
    QueueTimeout,
}

impl RefusalReason {
    pub fn code(self) -> &'static str {
        match self {
            Self::CallLimit => "calls_limit",
            Self::QueueFull => "concurrency_queue_full",
            Self::QueueTimeout => "concurrency_queue_timeout",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("service {target_id} refused the call: {}", .reason.code())]
pub struct SlaRefusal {
    pub target_id: String,
    pub reason: RefusalReason,
    pub retry_after_ms: u64,
    pub origin: AdmitOrigin,
}

/// How an admitted call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Error,
    /// The policy's execution timeout elapsed.
    Timeout,
    /// The permit was dropped without an outcome: the caller went away.
    Cancelled,
    /// The permit was dropped while its thread was unwinding from a panic.
    Panicked,
}

impl Outcome {
    /// The outcome of a task that ended without a value: a panic is
    /// `Panicked`, anything else (an abort) is `Cancelled`. For a caller that
    /// keeps the permit outside the task and awaits the `JoinHandle`.
    pub fn of_join_error(error: &tokio::task::JoinError) -> Self {
        if error.is_panic() {
            Self::Panicked
        } else {
            Self::Cancelled
        }
    }
}

/// Upper bounds in milliseconds; one more bucket counts everything above.
pub const LATENCY_BUCKETS_MS: [u64; 24] = [
    1, 2, 5, 10, 20, 50, 100, 200, 300, 500, 750, 1_000, 1_500, 2_000, 3_000, 5_000, 7_500, 10_000,
    15_000, 30_000, 60_000, 120_000, 300_000, 3_600_000,
];
const BUCKET_SLOTS: usize = LATENCY_BUCKETS_MS.len() + 1;

#[derive(Debug)]
pub struct LatencyHistogram {
    counts: [AtomicU64; BUCKET_SLOTS],
}

impl Default for LatencyHistogram {
    fn default() -> Self {
        Self {
            counts: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

impl LatencyHistogram {
    pub fn record(&self, elapsed: Duration) {
        let ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        let slot = LATENCY_BUCKETS_MS.partition_point(|bound| ms > *bound);
        self.counts[slot].fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> HistogramSnapshot {
        HistogramSnapshot {
            counts: std::array::from_fn(|i| self.counts[i].load(Ordering::Relaxed)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistogramSnapshot {
    counts: [u64; BUCKET_SLOTS],
}

impl HistogramSnapshot {
    pub fn total(&self) -> u64 {
        self.counts.iter().sum()
    }

    /// What was recorded after `earlier` was taken, so an evaluator can look at
    /// a window instead of the whole life of the process.
    pub fn since(&self, earlier: &Self) -> Self {
        Self {
            counts: std::array::from_fn(|i| self.counts[i].saturating_sub(earlier.counts[i])),
        }
    }

    /// Upper bound of the bucket that holds the `quantile` (0..=1) call; `None`
    /// when nothing was recorded. Calls above the last bound report that bound,
    /// so the answer is a floor there.
    pub fn percentile_ms(&self, quantile: f64) -> Option<u64> {
        let total = self.total();
        if total == 0 {
            return None;
        }
        let target = ((quantile.clamp(0.0, 1.0) * total as f64).ceil() as u64).max(1);
        let mut seen = 0u64;
        for (slot, count) in self.counts.iter().enumerate() {
            seen += count;
            if seen >= target {
                return Some(LATENCY_BUCKETS_MS[slot.min(LATENCY_BUCKETS_MS.len() - 1)]);
            }
        }
        LATENCY_BUCKETS_MS.last().copied()
    }

    pub fn p95_ms(&self) -> Option<u64> {
        self.percentile_ms(0.95)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateStats {
    pub in_flight: u64,
    pub waiting: u64,
    pub accepted: u64,
    pub rejected_concurrency: u64,
    pub rejected_calls: u64,
    pub ok: u64,
    pub errors: u64,
    pub timeouts: u64,
    pub cancelled: u64,
    /// Calls counted in the current window, `0` without a call limit.
    pub window_calls: u32,
    /// When the current window ends; `None` without a call limit.
    pub window_resets_at_ms: Option<u64>,
    pub latency: HistogramSnapshot,
}

#[derive(Debug, Default)]
struct Counters {
    in_flight: AtomicU64,
    accepted: AtomicU64,
    rejected_concurrency: AtomicU64,
    rejected_calls: AtomicU64,
    ok: AtomicU64,
    errors: AtomicU64,
    timeouts: AtomicU64,
    cancelled: AtomicU64,
}

fn pack(epoch: u32, count: u32) -> u64 {
    (u64::from(epoch) << 32) | u64::from(count)
}

fn unpack(state: u64) -> (u32, u32) {
    ((state >> 32) as u32, state as u32)
}

pub struct ServiceGate {
    target_id: String,
    config: RwLock<Arc<GateConfig>>,
    clock: Clock,
    semaphore: Semaphore,
    /// The configured capacity; held while reconfiguring, so two
    /// reconfigurations never interleave.
    capacity: Mutex<usize>,
    /// Slots the semaphore holds beyond the configured capacity after a
    /// shrink while calls were in flight. A release pays it down instead of
    /// handing the slot back. It is a mutex, not an atomic, because the shrink
    /// (forget what is free, record the rest) and a release (pay the debt or
    /// return the slot) must not interleave: a slot returned between the two
    /// steps would stay in the pool and admit a call above the new limit.
    shrink_debt: Mutex<usize>,
    waiting: AtomicUsize,
    /// `epoch << 32 | count` of the fixed call window. One atomic, so the epoch
    /// roll-over and the count cannot be observed half-updated.
    window: AtomicU64,
    counters: Counters,
    latency: LatencyHistogram,
}

impl std::fmt::Debug for ServiceGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceGate")
            .field("target_id", &self.target_id)
            .field("config", &*self.config.read())
            .finish_non_exhaustive()
    }
}

/// Returns the call it reserved to the window unless the call was admitted.
struct CallReservation<'a> {
    gate: &'a ServiceGate,
    epoch: Option<u32>,
}

impl CallReservation<'_> {
    fn commit(mut self) {
        self.epoch = None;
    }
}

impl Drop for CallReservation<'_> {
    fn drop(&mut self) {
        if let Some(epoch) = self.epoch {
            self.gate.refund_call(epoch);
        }
    }
}

struct WaitingSlot<'a>(&'a AtomicUsize);

impl Drop for WaitingSlot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl ServiceGate {
    pub fn new(target_id: impl Into<String>, mut config: GateConfig, clock: Clock) -> Arc<Self> {
        config.limits = config.limits.normalized();
        let capacity = config.limits.capacity();
        Arc::new(Self {
            target_id: target_id.into(),
            config: RwLock::new(Arc::new(config)),
            clock,
            semaphore: Semaphore::new(capacity),
            capacity: Mutex::new(capacity),
            shrink_debt: Mutex::new(0),
            waiting: AtomicUsize::new(0),
            window: AtomicU64::new(0),
            counters: Counters::default(),
            latency: LatencyHistogram::default(),
        })
    }

    pub fn target_id(&self) -> &str {
        &self.target_id
    }

    pub fn config(&self) -> Arc<GateConfig> {
        self.config.read().clone()
    }

    /// Applies new limits in place. Calls in flight keep their slots and the
    /// counters are kept: only the ceiling moves. A smaller ceiling takes effect
    /// as the calls above it finish; no call is cut short.
    pub fn reconfigure(&self, mut new: GateConfig) {
        new.limits = new.limits.normalized();
        let mut capacity = self.capacity.lock();
        let old = {
            let mut guard = self.config.write();
            if **guard == new {
                return;
            }
            std::mem::replace(&mut *guard, Arc::new(new.clone()))
        };
        let target = new.limits.capacity();
        match target.cmp(&*capacity) {
            std::cmp::Ordering::Greater => self.grow(target - *capacity),
            std::cmp::Ordering::Less => self.shrink(*capacity - target),
            std::cmp::Ordering::Equal => {}
        }
        *capacity = target;
        if old.limits.call_limit.map(|(_, w)| w) != new.limits.call_limit.map(|(_, w)| w) {
            self.window.store(0, Ordering::Release);
        }
    }

    fn grow(&self, delta: usize) {
        let mut debt = self.shrink_debt.lock();
        let paid = (*debt).min(delta);
        *debt -= paid;
        if delta > paid {
            self.semaphore.add_permits(delta - paid);
        }
    }

    fn shrink(&self, delta: usize) {
        let mut debt = self.shrink_debt.lock();
        let forgotten = self.semaphore.forget_permits(delta);
        *debt += delta - forgotten;
    }

    fn release_slot(&self) {
        let mut debt = self.shrink_debt.lock();
        if *debt > 0 {
            *debt -= 1;
        } else {
            self.semaphore.add_permits(1);
        }
    }

    /// Whether any call is in flight or waiting on this gate.
    pub fn is_busy(&self) -> bool {
        self.counters.in_flight.load(Ordering::Acquire) > 0
            || self.waiting.load(Ordering::Acquire) > 0
    }

    pub fn stats(&self) -> GateStats {
        let config = self.config();
        let now = (self.clock)();
        let (window_calls, window_resets_at_ms) = match config.limits.call_limit {
            Some((_, secs)) => {
                let window_ms = u64::from(secs.max(1)) * 1000;
                let epoch = now / window_ms;
                let (stored, count) = unpack(self.window.load(Ordering::Acquire));
                let calls = if stored == epoch as u32 { count } else { 0 };
                (calls, Some((epoch + 1) * window_ms))
            }
            None => (0, None),
        };
        let c = &self.counters;
        GateStats {
            in_flight: c.in_flight.load(Ordering::Acquire),
            waiting: self.waiting.load(Ordering::Acquire) as u64,
            accepted: c.accepted.load(Ordering::Relaxed),
            rejected_concurrency: c.rejected_concurrency.load(Ordering::Relaxed),
            rejected_calls: c.rejected_calls.load(Ordering::Relaxed),
            ok: c.ok.load(Ordering::Relaxed),
            errors: c.errors.load(Ordering::Relaxed),
            timeouts: c.timeouts.load(Ordering::Relaxed),
            cancelled: c.cancelled.load(Ordering::Relaxed),
            window_calls,
            window_resets_at_ms,
            latency: self.latency.snapshot(),
        }
    }

    /// Admits one call or says why not. Cancel-safe: dropping the future while
    /// it waits gives the queue place and the reserved call back.
    pub async fn admit(self: &Arc<Self>, origin: AdmitOrigin) -> Result<SlaPermit, SlaRefusal> {
        self.admit_with(origin, true).await
    }

    /// [`Self::admit`] that never waits: a call that finds no free slot is
    /// refused at once (`QueueFull`) instead of joining the waiting queue. For a
    /// caller that has somewhere better to put a call off than a queue held open
    /// by its own lease, such as a BPMN job that can be deferred.
    pub async fn try_admit(self: &Arc<Self>, origin: AdmitOrigin) -> Result<SlaPermit, SlaRefusal> {
        self.admit_with(origin, false).await
    }

    async fn admit_with(
        self: &Arc<Self>,
        origin: AdmitOrigin,
        may_wait: bool,
    ) -> Result<SlaPermit, SlaRefusal> {
        let config = self.config();
        let limits = config.limits;
        let refuse = |reason: RefusalReason, retry_after_ms: u64| SlaRefusal {
            target_id: self.target_id.clone(),
            reason,
            retry_after_ms,
            origin,
        };

        let reservation = match self.reserve_call(limits.call_limit) {
            Ok(reservation) => reservation,
            Err(retry_after_ms) => {
                self.counters.rejected_calls.fetch_add(1, Ordering::Relaxed);
                return Err(refuse(RefusalReason::CallLimit, retry_after_ms));
            }
        };
        let acquired = if may_wait {
            self.acquire_slot(&limits).await
        } else {
            self.try_acquire_slot()
        };
        if let Err(reason) = acquired {
            self.counters
                .rejected_concurrency
                .fetch_add(1, Ordering::Relaxed);
            return Err(refuse(reason, CONCURRENCY_RETRY_AFTER_MS));
        }
        let window_epoch = reservation.epoch;
        reservation.commit();
        self.counters.accepted.fetch_add(1, Ordering::Relaxed);
        self.counters.in_flight.fetch_add(1, Ordering::AcqRel);
        Ok(SlaPermit {
            gate: Some(self.clone()),
            started: Instant::now(),
            outcome: None,
            timeout: limits.timeout,
            window_epoch,
        })
    }

    fn try_acquire_slot(&self) -> Result<(), RefusalReason> {
        match self.semaphore.try_acquire() {
            Ok(permit) => {
                permit.forget();
                Ok(())
            }
            Err(_) => Err(RefusalReason::QueueFull),
        }
    }

    /// `Err` carries the milliseconds until the window rolls over.
    fn reserve_call(&self, limit: Option<(u32, u32)>) -> Result<CallReservation<'_>, u64> {
        let Some((calls, window_secs)) = limit else {
            return Ok(CallReservation {
                gate: self,
                epoch: None,
            });
        };
        let now = (self.clock)();
        let window_ms = u64::from(window_secs.max(1)) * 1000;
        let epoch = now / window_ms;
        let epoch32 = epoch as u32;
        let mut state = self.window.load(Ordering::Acquire);
        loop {
            let (stored, count) = unpack(state);
            let next = if stored != epoch32 {
                pack(epoch32, 1)
            } else if count >= calls {
                return Err(((epoch + 1) * window_ms - now).max(1));
            } else {
                pack(stored, count + 1)
            };
            match self.window.compare_exchange_weak(
                state,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Ok(CallReservation {
                        gate: self,
                        epoch: Some(epoch32),
                    })
                }
                Err(current) => state = current,
            }
        }
    }

    fn refund_call(&self, epoch: u32) {
        let mut state = self.window.load(Ordering::Acquire);
        loop {
            let (stored, count) = unpack(state);
            if stored != epoch || count == 0 {
                return;
            }
            match self.window.compare_exchange_weak(
                state,
                pack(stored, count - 1),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(current) => state = current,
            }
        }
    }

    async fn acquire_slot(&self, limits: &GateLimits) -> Result<(), RefusalReason> {
        if let Ok(permit) = self.semaphore.try_acquire() {
            permit.forget();
            return Ok(());
        }
        let _place = self
            .enter_queue(limits.max_queue as usize)
            .ok_or(RefusalReason::QueueFull)?;
        match tokio::time::timeout(limits.queue_timeout, self.semaphore.acquire()).await {
            Ok(Ok(permit)) => {
                permit.forget();
                Ok(())
            }
            Ok(Err(_closed)) => Err(RefusalReason::QueueFull),
            Err(_elapsed) => Err(RefusalReason::QueueTimeout),
        }
    }

    fn enter_queue(&self, max: usize) -> Option<WaitingSlot<'_>> {
        let mut waiting = self.waiting.load(Ordering::Acquire);
        loop {
            if waiting >= max {
                return None;
            }
            match self.waiting.compare_exchange_weak(
                waiting,
                waiting + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(WaitingSlot(&self.waiting)),
                Err(current) => waiting = current,
            }
        }
    }

    /// Takes back an admission whose call never started: the slot is freed, the
    /// call returns to the window and the call is not counted as accepted, so a
    /// refusal that happens after admission (a denied ACL, a flow that does not
    /// compile) costs the service nothing.
    fn abandon(&self, window_epoch: Option<u32>) {
        if let Some(epoch) = window_epoch {
            self.refund_call(epoch);
        }
        self.counters.accepted.fetch_sub(1, Ordering::Relaxed);
        self.counters.in_flight.fetch_sub(1, Ordering::AcqRel);
        self.release_slot();
    }

    fn finish(&self, outcome: Outcome, elapsed: Duration) {
        let c = &self.counters;
        let counter = match outcome {
            Outcome::Ok => &c.ok,
            Outcome::Error | Outcome::Panicked => &c.errors,
            Outcome::Timeout => &c.timeouts,
            Outcome::Cancelled => &c.cancelled,
        };
        counter.fetch_add(1, Ordering::Relaxed);
        // A call that was cancelled or panicked did not run to a result, so its
        // duration says nothing about how long the service takes.
        if matches!(outcome, Outcome::Ok | Outcome::Error | Outcome::Timeout) {
            self.latency.record(elapsed);
        }
        c.in_flight.fetch_sub(1, Ordering::AcqRel);
        self.release_slot();
    }
}

/// One admitted call. Dropping it is how a call ends: the slot is released and
/// the duration and outcome are recorded, whatever way the owner got there.
#[derive(Debug)]
#[must_use = "dropping the permit ends the call"]
pub struct SlaPermit {
    gate: Option<Arc<ServiceGate>>,
    started: Instant,
    outcome: Option<Outcome>,
    timeout: Option<Duration>,
    /// The call-window epoch the call was counted in, so `forgo` can return it.
    window_epoch: Option<u32>,
}

impl SlaPermit {
    /// The permit of a call the policy does not govern (no policy, or a nested
    /// flow that must not take a second slot): holds nothing, records nothing.
    pub fn untracked() -> Self {
        Self {
            gate: None,
            started: Instant::now(),
            outcome: None,
            timeout: None,
            window_epoch: None,
        }
    }

    pub fn is_tracked(&self) -> bool {
        self.gate.is_some()
    }

    /// The execution timeout of the policy the call was admitted under.
    pub fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    pub fn set_outcome(&mut self, outcome: Outcome) {
        self.outcome = Some(outcome);
    }

    pub fn finish(mut self, outcome: Outcome) {
        self.outcome = Some(outcome);
    }

    /// Gives the admission back because the call never started: records
    /// nothing, frees the slot and returns the call to its window.
    pub fn forgo(mut self) {
        if let Some(gate) = self.gate.take() {
            gate.abandon(self.window_epoch);
        }
    }
}

impl Drop for SlaPermit {
    fn drop(&mut self) {
        let Some(gate) = self.gate.take() else {
            return;
        };
        let outcome = self.outcome.unwrap_or(if std::thread::panicking() {
            Outcome::Panicked
        } else {
            Outcome::Cancelled
        });
        gate.finish(outcome, self.started.elapsed());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(max_concurrency: Option<u32>, max_queue: u32, queue_timeout_ms: u64) -> GateLimits {
        GateLimits {
            max_concurrency,
            max_queue,
            queue_timeout: Duration::from_millis(queue_timeout_ms),
            call_limit: None,
            timeout: None,
        }
    }

    fn config(limits: GateLimits) -> GateConfig {
        GateConfig {
            policy_id: "p1".into(),
            org_id: "o1".into(),
            limits,
        }
    }

    fn gate(limits: GateLimits) -> Arc<ServiceGate> {
        ServiceGate::new("flow-1", config(limits), system_clock())
    }

    fn manual_clock(start_ms: u64) -> (Clock, Arc<AtomicU64>) {
        let now = Arc::new(AtomicU64::new(start_ms));
        let handle = now.clone();
        let clock: Clock = Arc::new(move || handle.load(Ordering::SeqCst));
        (clock, now)
    }

    #[test]
    fn defaults_follow_the_plan() {
        let spec = SlaPolicySpec {
            max_concurrency: Some(4),
            ..Default::default()
        };
        let limits = GateLimits::from_spec(&spec);
        assert_eq!(limits.max_queue, 4, "the queue is as long as the limit");
        assert_eq!(limits.queue_timeout, Duration::from_secs(10));
        let unlimited = GateLimits::from_spec(&SlaPolicySpec::default());
        assert_eq!(unlimited.max_concurrency, None);
        assert_eq!(unlimited.call_limit, None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_hundred_tasks_never_exceed_the_limit() {
        let gate = gate(limits(Some(5), 100, 30_000));
        let current = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for _ in 0..100 {
            let (gate, current, peak) = (gate.clone(), current.clone(), peak.clone());
            tasks.push(tokio::spawn(async move {
                let permit = gate
                    .admit(AdmitOrigin::Request)
                    .await
                    .expect("queued, not refused");
                let now = current.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(2)).await;
                current.fetch_sub(1, Ordering::SeqCst);
                permit.finish(Outcome::Ok);
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        let observed = peak.load(Ordering::SeqCst);
        assert!(observed <= 5, "peak {observed} exceeded the limit");
        assert!(
            observed >= 2,
            "the tasks never overlapped, the test proves nothing"
        );
        let stats = gate.stats();
        assert_eq!(
            (stats.accepted, stats.ok, stats.in_flight, stats.waiting),
            (100, 100, 0, 0)
        );
    }

    #[tokio::test]
    async fn a_call_that_waits_longer_than_queue_timeout_is_refused() {
        let gate = gate(limits(Some(1), 1, 40));
        let held = gate.admit(AdmitOrigin::Request).await.unwrap();
        let started = Instant::now();
        let refusal = gate.admit(AdmitOrigin::Request).await.unwrap_err();
        assert_eq!(refusal.reason, RefusalReason::QueueTimeout);
        assert!(started.elapsed() >= Duration::from_millis(35));
        assert_eq!(refusal.retry_after_ms, CONCURRENCY_RETRY_AFTER_MS);
        assert_eq!(gate.stats().waiting, 0, "the waiting place is given back");
        assert_eq!(gate.stats().rejected_concurrency, 1);
        drop(held);
    }

    #[tokio::test]
    async fn a_call_is_refused_at_once_when_the_queue_is_full() {
        let gate = gate(limits(Some(1), 1, 5_000));
        let held = gate.admit(AdmitOrigin::Request).await.unwrap();
        let waiter = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.admit(AdmitOrigin::Request).await })
        };
        while gate.stats().waiting < 1 {
            tokio::task::yield_now().await;
        }
        let started = Instant::now();
        let refusal = gate.admit(AdmitOrigin::Bus).await.unwrap_err();
        assert_eq!(refusal.reason, RefusalReason::QueueFull);
        assert_eq!(refusal.origin, AdmitOrigin::Bus);
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "a full queue does not wait"
        );

        drop(held);
        let promoted = waiter
            .await
            .unwrap()
            .expect("the queued call gets the freed slot");
        promoted.finish(Outcome::Ok);
    }

    #[tokio::test]
    async fn without_a_queue_the_second_call_is_refused_at_once() {
        let gate = gate(limits(Some(1), 0, 5_000));
        let _held = gate.admit(AdmitOrigin::Request).await.unwrap();
        let refusal = gate.admit(AdmitOrigin::Request).await.unwrap_err();
        assert_eq!(refusal.reason, RefusalReason::QueueFull);
    }

    #[tokio::test]
    async fn the_fixed_window_is_clock_aligned_and_resets_on_the_boundary() {
        let window_start = 10 * 60_000;
        let (clock, now) = manual_clock(window_start + 59_000);
        let mut cfg = limits(None, 0, 10);
        cfg.call_limit = Some((3, 60));
        let gate = ServiceGate::new("flow-1", config(cfg), clock);

        for _ in 0..3 {
            gate.admit(AdmitOrigin::Request)
                .await
                .unwrap()
                .finish(Outcome::Ok);
        }
        let refusal = gate.admit(AdmitOrigin::Request).await.unwrap_err();
        assert_eq!(refusal.reason, RefusalReason::CallLimit);
        assert_eq!(refusal.retry_after_ms, 1_000, "1 s to the next minute");
        let stats = gate.stats();
        assert_eq!((stats.window_calls, stats.rejected_calls), (3, 1));
        assert_eq!(stats.window_resets_at_ms, Some(window_start + 60_000));

        now.store(window_start + 59_999, Ordering::SeqCst);
        assert!(
            gate.admit(AdmitOrigin::Request).await.is_err(),
            "still the old window"
        );

        now.store(window_start + 60_000, Ordering::SeqCst);
        for _ in 0..3 {
            gate.admit(AdmitOrigin::Request)
                .await
                .unwrap()
                .finish(Outcome::Ok);
        }
        assert!(gate.admit(AdmitOrigin::Request).await.is_err());
        assert_eq!(gate.stats().window_calls, 3);
    }

    #[tokio::test]
    async fn a_refused_call_does_not_use_up_the_window() {
        let (clock, _now) = manual_clock(5 * 60_000);
        let mut cfg = limits(Some(1), 0, 10);
        cfg.call_limit = Some((2, 60));
        let gate = ServiceGate::new("flow-1", config(cfg), clock);
        let held = gate.admit(AdmitOrigin::Request).await.unwrap();
        for _ in 0..5 {
            let refusal = gate.admit(AdmitOrigin::Request).await.unwrap_err();
            assert_eq!(refusal.reason, RefusalReason::QueueFull);
        }
        assert_eq!(
            gate.stats().window_calls,
            1,
            "only the admitted call is counted"
        );
        held.finish(Outcome::Ok);
        drop(
            gate.admit(AdmitOrigin::Request)
                .await
                .expect("the second call of the window"),
        );
    }

    #[tokio::test]
    async fn a_waiter_cancelled_in_the_queue_gives_back_its_place_and_its_call() {
        let (clock, _now) = manual_clock(5 * 60_000);
        let mut cfg = limits(Some(1), 1, 60_000);
        cfg.call_limit = Some((10, 60));
        let gate = ServiceGate::new("flow-1", config(cfg), clock);
        let held = gate.admit(AdmitOrigin::Request).await.unwrap();
        let waiter = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.admit(AdmitOrigin::Request).await })
        };
        while gate.stats().waiting < 1 {
            tokio::task::yield_now().await;
        }
        assert_eq!(gate.stats().window_calls, 2);
        waiter.abort();
        let _ = waiter.await;
        let stats = gate.stats();
        assert_eq!((stats.waiting, stats.window_calls), (0, 1));
        drop(held);
        assert_eq!(gate.stats().in_flight, 0);
        drop(
            gate.admit(AdmitOrigin::Request)
                .await
                .expect("the slot is free"),
        );
    }

    #[tokio::test]
    async fn a_permit_dropped_on_cancel_frees_its_slot_and_records_it() {
        let gate = gate(limits(Some(1), 0, 10));
        let task = {
            let gate = gate.clone();
            tokio::spawn(async move {
                let _permit = gate.admit(AdmitOrigin::Request).await.unwrap();
                tokio::time::sleep(Duration::from_secs(3600)).await;
            })
        };
        while gate.stats().in_flight < 1 {
            tokio::task::yield_now().await;
        }
        assert!(gate.admit(AdmitOrigin::Request).await.is_err());
        task.abort();
        let _ = task.await;
        let stats = gate.stats();
        assert_eq!((stats.in_flight, stats.cancelled, stats.errors), (0, 1, 0));
        assert_eq!(
            stats.latency.total(),
            0,
            "a cancelled call says nothing about how long the service takes"
        );
        drop(
            gate.admit(AdmitOrigin::Request)
                .await
                .expect("the slot came back"),
        );
    }

    #[tokio::test]
    async fn a_permit_dropped_by_a_panic_frees_its_slot_and_counts_an_error() {
        let gate = gate(limits(Some(1), 0, 10));
        let task = {
            let gate = gate.clone();
            tokio::spawn(async move {
                let _permit = gate.admit(AdmitOrigin::Request).await.unwrap();
                panic!("flow blew up");
            })
        };
        assert!(task.await.unwrap_err().is_panic());
        let stats = gate.stats();
        assert_eq!((stats.in_flight, stats.errors, stats.cancelled), (0, 1, 0));
        drop(
            gate.admit(AdmitOrigin::Request)
                .await
                .expect("the slot came back"),
        );
    }

    #[tokio::test]
    async fn outcomes_are_counted_by_kind() {
        let gate = gate(limits(Some(4), 0, 10));
        for outcome in [Outcome::Ok, Outcome::Ok, Outcome::Error, Outcome::Timeout] {
            gate.admit(AdmitOrigin::Request)
                .await
                .unwrap()
                .finish(outcome);
        }
        let stats = gate.stats();
        assert_eq!(
            (stats.ok, stats.errors, stats.timeouts, stats.cancelled),
            (2, 1, 1, 0)
        );
        assert_eq!(stats.in_flight, 0);
    }

    #[tokio::test]
    async fn reconfigure_keeps_the_calls_in_flight_and_never_over_admits() {
        let gate = gate(limits(Some(3), 0, 10));
        let a = gate.admit(AdmitOrigin::Request).await.unwrap();
        let b = gate.admit(AdmitOrigin::Request).await.unwrap();
        let c = gate.admit(AdmitOrigin::Request).await.unwrap();

        gate.reconfigure(config(limits(Some(1), 0, 10)));
        assert_eq!(gate.stats().in_flight, 3, "nothing in flight is cut short");
        assert!(gate.admit(AdmitOrigin::Request).await.is_err());

        drop(a);
        drop(b);
        assert!(
            gate.admit(AdmitOrigin::Request).await.is_err(),
            "one call is still above the new ceiling of 1"
        );
        drop(c);
        let only = gate
            .admit(AdmitOrigin::Request)
            .await
            .expect("the one slot is free");
        assert!(
            gate.admit(AdmitOrigin::Request).await.is_err(),
            "the ceiling is 1, not 3"
        );
        drop(only);

        gate.reconfigure(config(limits(Some(2), 0, 10)));
        let first = gate.admit(AdmitOrigin::Request).await.unwrap();
        let second = gate.admit(AdmitOrigin::Request).await.unwrap();
        assert!(gate.admit(AdmitOrigin::Request).await.is_err());
        drop((first, second));
    }

    #[tokio::test]
    async fn growing_while_calls_are_in_flight_admits_the_difference_only() {
        let gate = gate(limits(Some(2), 0, 10));
        let a = gate.admit(AdmitOrigin::Request).await.unwrap();
        let b = gate.admit(AdmitOrigin::Request).await.unwrap();
        gate.reconfigure(config(limits(Some(3), 0, 10)));
        let c = gate.admit(AdmitOrigin::Request).await.unwrap();
        assert!(gate.admit(AdmitOrigin::Request).await.is_err());
        drop((a, b, c));
        assert_eq!(gate.stats().in_flight, 0);
    }

    #[tokio::test]
    async fn lifting_the_concurrency_limit_and_putting_it_back_works() {
        let gate = gate(limits(Some(1), 0, 10));
        let held = gate.admit(AdmitOrigin::Request).await.unwrap();
        gate.reconfigure(config(limits(None, 0, 10)));
        let extra: Vec<_> = {
            let mut v = Vec::new();
            for _ in 0..50 {
                v.push(gate.admit(AdmitOrigin::Request).await.unwrap());
            }
            v
        };
        drop(extra);
        gate.reconfigure(config(limits(Some(1), 0, 10)));
        assert!(
            gate.admit(AdmitOrigin::Request).await.is_err(),
            "the held call fills the one slot"
        );
        drop(held);
        drop(gate.admit(AdmitOrigin::Request).await.unwrap());
    }

    #[test]
    fn the_histogram_reports_the_bucket_of_the_95th_percentile() {
        let histogram = LatencyHistogram::default();
        assert_eq!(histogram.snapshot().p95_ms(), None);
        for _ in 0..94 {
            histogram.record(Duration::from_millis(40));
        }
        for _ in 0..6 {
            histogram.record(Duration::from_millis(900));
        }
        let snapshot = histogram.snapshot();
        assert_eq!(snapshot.total(), 100);
        assert_eq!(snapshot.percentile_ms(0.5), Some(50));
        assert_eq!(
            snapshot.p95_ms(),
            Some(1_000),
            "the slowest 6 % are above 95 %"
        );
        assert_eq!(snapshot.percentile_ms(0.9), Some(50));

        histogram.record(Duration::from_secs(7_200));
        let later = histogram.snapshot();
        assert_eq!(later.percentile_ms(1.0), Some(3_600_000));
        let window = later.since(&snapshot);
        assert_eq!(window.total(), 1);
        assert_eq!(window.p95_ms(), Some(3_600_000));
    }

    #[tokio::test]
    async fn a_task_that_panics_after_an_await_with_the_permit_held_is_panicked() {
        let gate = gate(limits(Some(1), 0, 10));
        let task = {
            let gate = gate.clone();
            tokio::spawn(async move {
                let _permit = gate.admit(AdmitOrigin::Request).await.unwrap();
                tokio::time::sleep(Duration::from_millis(5)).await;
                tokio::task::yield_now().await;
                panic!("flow blew up after an await");
            })
        };
        assert!(task.await.unwrap_err().is_panic());
        let stats = gate.stats();
        assert_eq!((stats.in_flight, stats.errors, stats.cancelled), (0, 1, 0));
        assert_eq!(
            stats.latency.total(),
            0,
            "a panicked call is not a latency sample"
        );
        drop(gate.admit(AdmitOrigin::Request).await.unwrap());
    }

    #[tokio::test]
    async fn a_permit_kept_outside_the_task_takes_its_outcome_from_the_join_error() {
        let gate = gate(limits(Some(2), 0, 10));

        let permit = gate.admit(AdmitOrigin::Request).await.unwrap();
        let panicking = tokio::spawn(async { panic!("boom") });
        let error = panicking.await.unwrap_err();
        assert_eq!(Outcome::of_join_error(&error), Outcome::Panicked);
        permit.finish(Outcome::of_join_error(&error));

        let permit = gate.admit(AdmitOrigin::Request).await.unwrap();
        let sleeping = tokio::spawn(tokio::time::sleep(Duration::from_secs(3600)));
        sleeping.abort();
        let error = sleeping.await.unwrap_err();
        assert_eq!(Outcome::of_join_error(&error), Outcome::Cancelled);
        permit.finish(Outcome::of_join_error(&error));

        let stats = gate.stats();
        assert_eq!((stats.errors, stats.cancelled, stats.in_flight), (1, 1, 0));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn shrinking_while_calls_finish_never_admits_above_the_new_limit() {
        for round in 0..200 {
            let gate = gate(limits(Some(8), 0, 10));
            let mut permits = Vec::new();
            for _ in 0..8 {
                permits.push(gate.admit(AdmitOrigin::Request).await.unwrap());
            }
            let finishing = tokio::task::spawn_blocking(move || drop(permits));
            gate.reconfigure(config(limits(Some(2), 0, 10)));
            finishing.await.unwrap();

            let mut admitted = Vec::new();
            while let Ok(permit) = gate.admit(AdmitOrigin::Request).await {
                admitted.push(permit);
                assert!(
                    admitted.len() <= 2,
                    "round {round}: admitted above the new limit of 2"
                );
            }
            assert_eq!(admitted.len(), 2, "round {round}: lost a slot");
        }
    }

    #[tokio::test]
    async fn a_zero_second_window_is_read_as_one_second() {
        let (clock, _now) = manual_clock(5_000);
        let mut cfg = limits(None, 0, 10);
        cfg.call_limit = Some((1, 0));
        let gate = ServiceGate::new("flow-1", config(cfg), clock);
        drop(gate.admit(AdmitOrigin::Request).await.unwrap());
        let refusal = gate.admit(AdmitOrigin::Request).await.unwrap_err();
        assert_eq!(refusal.reason, RefusalReason::CallLimit);
        assert_eq!(refusal.retry_after_ms, 1_000);
        assert_eq!(gate.stats().window_resets_at_ms, Some(6_000));

        let mut changed = limits(None, 0, 10);
        changed.call_limit = Some((1, 0));
        gate.reconfigure(config(changed));
        let spec = SlaPolicySpec {
            call_limit: Some(3),
            call_window_secs: Some(0),
            ..Default::default()
        };
        assert_eq!(GateLimits::from_spec(&spec).call_limit, Some((3, 1)));
    }

    #[tokio::test]
    async fn a_forgone_admission_returns_its_slot_and_its_call() {
        let (clock, _now) = manual_clock(5_000);
        let mut cfg = limits(Some(1), 0, 10);
        cfg.call_limit = Some((1, 60));
        let gate = ServiceGate::new("flow-1", config(cfg), clock);

        let permit = gate.admit(AdmitOrigin::Request).await.unwrap();
        assert_eq!(gate.stats().in_flight, 1);
        permit.forgo();

        let stats = gate.stats();
        assert_eq!(
            (stats.in_flight, stats.accepted, stats.window_calls),
            (0, 0, 0),
            "nothing is left of a call that never started"
        );
        assert_eq!((stats.ok, stats.errors, stats.cancelled), (0, 0, 0));
        // Both limits are free again: one slot, one call per window.
        gate.admit(AdmitOrigin::Request)
            .await
            .expect("the slot and the window call were returned");
    }

    #[tokio::test]
    async fn try_admit_refuses_at_once_instead_of_queueing() {
        let gate = gate(limits(Some(1), 5, 30_000));
        let held = gate.try_admit(AdmitOrigin::Request).await.unwrap();

        let started = Instant::now();
        let refusal = gate.try_admit(AdmitOrigin::Request).await.unwrap_err();
        assert_eq!(refusal.reason, RefusalReason::QueueFull);
        assert!(refusal.retry_after_ms > 0);
        assert!(started.elapsed() < Duration::from_secs(1), "no queueing");
        let stats = gate.stats();
        assert_eq!((stats.waiting, stats.rejected_concurrency), (0, 1));
        drop(held);
        gate.try_admit(AdmitOrigin::Request)
            .await
            .expect("free again once the holder is done");
    }
}
