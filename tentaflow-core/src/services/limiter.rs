//! In-flight request limit of one AI service backend (`max_concurrent` of
//! `services.config_json`; SUM/tentabus/PLAN-TB03-LIMITY-SLA.md, E3).
//!
//! A limiter is a counting semaphore with a bounded wait. It lives in a
//! node-global map keyed by `(node_id, service_id)`, not inside a client, so a
//! client or handle that is rebuilt (endpoint change, reconnect) picks the same
//! limiter up again and the limit is not reset. Until the supervisor configures
//! a limiter it admits everything: a service this node does not own, or one that
//! has not reached the snapshot yet, behaves as it did before the limit existed.
//!
//! A permit is taken where a request is actually sent (`BackendClient` for HTTP,
//! `IrohServiceClient` for QUIC) and is returned when the call ends, when a
//! stream is read to its end, or when the permit is dropped. It is never held
//! across candidates of a fallback chain, so a chain cannot wait on itself.
//!
//! Lowering the limit while requests are in flight uses the same shrink-debt
//! pattern as `sla::gate::ServiceGate`: slots above the new capacity are
//! retired as the calls holding them finish instead of being handed back.

use std::future::Future;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use parking_lot::Mutex;
use tokio::sync::Semaphore;

use crate::error::CoreError;

/// Shortest `Retry-After` hint, and the granularity it is rounded up to.
const MIN_RETRY_AFTER_MS: u64 = 1_000;

/// A refusal is logged at most this often per service, so a saturated backend
/// does not flood the log with one line per refused request.
const WARN_EVERY: Duration = Duration::from_secs(10);

tokio::task_local! {
    /// Set by a fallback chain for every candidate but the last: such a
    /// candidate is only tried when it has a free slot right now, because
    /// another candidate may be idle.
    static NO_WAIT: bool;
}

/// Runs `fut` with slot acquisition that never waits: a service at its limit
/// refuses at once with `BackendBusy`. Everything awaited inside `fut` that
/// takes a slot is affected, so it belongs around one dispatch, not around a
/// whole request.
pub async fn without_waiting<F: Future>(fut: F) -> F::Output {
    NO_WAIT.scope(true, fut).await
}

fn must_not_wait() -> bool {
    NO_WAIT.try_with(|v| *v).unwrap_or(false)
}

struct Capacity {
    /// The configured number of slots; `Semaphore::MAX_PERMITS` when unlimited.
    limit: usize,
    /// Slots the semaphore still holds above `limit` after a shrink while calls
    /// were in flight. A release pays it down instead of returning the slot.
    /// It shares the mutex with `limit` and the shrink itself: a slot returned
    /// between "forget what is free" and "record the rest" would stay in the
    /// pool and admit a call above the new limit.
    debt: usize,
}

/// A point-in-time reading, for the service status views.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimiterStats {
    /// `None` when the service has no limit.
    pub limit: Option<usize>,
    pub in_flight: usize,
    pub waiting: usize,
    /// Requests refused with `BackendBusy` since the process started,
    /// including the ones a fallback chain moved past.
    pub rejected: u64,
}

pub struct ServiceLimiter {
    label: String,
    semaphore: Semaphore,
    capacity: Mutex<Capacity>,
    /// Longest a request waits for a slot (the service's `timeout_ms`).
    wait: Mutex<Duration>,
    /// Requests waiting for a slot. Capped at the limit (decision P4: a queue
    /// as long as the concurrency limit); one more is refused at once.
    waiting: AtomicUsize,
    rejected: AtomicU64,
    /// Milliseconds since `EPOCH` of the last logged refusal, `0` = never.
    last_warn_ms: AtomicU64,
}

static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

impl std::fmt::Debug for ServiceLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceLimiter")
            .field("label", &self.label)
            .field("limit", &self.limit())
            .finish_non_exhaustive()
    }
}

/// One slot. Dropping it returns the slot (or pays down shrink debt).
#[must_use = "the slot is released when the permit is dropped"]
pub struct ServicePermit {
    limiter: Arc<ServiceLimiter>,
}

impl Drop for ServicePermit {
    fn drop(&mut self) {
        self.limiter.release();
    }
}

impl std::fmt::Debug for ServicePermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServicePermit")
            .field("limiter", &self.limiter.label)
            .finish()
    }
}

struct Waiting<'a>(&'a AtomicUsize);

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl ServiceLimiter {
    /// A limiter that admits everything until [`Self::configure`] sets a limit.
    pub fn unlimited(label: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            label: label.into(),
            semaphore: Semaphore::new(Semaphore::MAX_PERMITS),
            capacity: Mutex::new(Capacity {
                limit: Semaphore::MAX_PERMITS,
                debt: 0,
            }),
            wait: Mutex::new(Duration::ZERO),
            waiting: AtomicUsize::new(0),
            rejected: AtomicU64::new(0),
            last_warn_ms: AtomicU64::new(0),
        })
    }

    /// A limiter that already enforces `limit` slots, waiting up to `wait`.
    pub fn with_limit(label: impl Into<String>, limit: usize, wait: Duration) -> Arc<Self> {
        let limiter = Self::unlimited(label);
        limiter.configure(Some(limit), wait);
        limiter
    }

    /// Applies a new limit in place; `None` removes it. Calls in flight keep
    /// their slots; a smaller limit takes effect as the calls above it finish,
    /// and no call is cut short. A limit of zero is read as "none".
    pub fn configure(&self, limit: Option<usize>, wait: Duration) {
        *self.wait.lock() = wait;
        let target = match limit {
            Some(0) | None => Semaphore::MAX_PERMITS,
            Some(n) => n.min(Semaphore::MAX_PERMITS),
        };
        let mut capacity = self.capacity.lock();
        match target.cmp(&capacity.limit) {
            std::cmp::Ordering::Greater => {
                let delta = target - capacity.limit;
                let paid = capacity.debt.min(delta);
                capacity.debt -= paid;
                if delta > paid {
                    self.semaphore.add_permits(delta - paid);
                }
            }
            std::cmp::Ordering::Less => {
                let delta = capacity.limit - target;
                let forgotten = self.semaphore.forget_permits(delta);
                capacity.debt += delta - forgotten;
            }
            std::cmp::Ordering::Equal => {}
        }
        capacity.limit = target;
    }

    /// The configured limit; `None` when the service is unlimited.
    pub fn limit(&self) -> Option<usize> {
        let limit = self.capacity.lock().limit;
        (limit < Semaphore::MAX_PERMITS).then_some(limit)
    }

    /// Requests that hold a slot right now. The semaphore always holds
    /// `limit + debt` slots in total, free or in flight.
    pub fn in_flight(&self) -> usize {
        let capacity = self.capacity.lock();
        (capacity.limit + capacity.debt).saturating_sub(self.semaphore.available_permits())
    }

    pub fn stats(&self) -> LimiterStats {
        LimiterStats {
            limit: self.limit(),
            in_flight: self.in_flight(),
            waiting: self.waiting.load(Ordering::Acquire),
            rejected: self.rejected.load(Ordering::Relaxed),
        }
    }

    fn release(&self) {
        let mut capacity = self.capacity.lock();
        if capacity.debt > 0 {
            capacity.debt -= 1;
        } else {
            self.semaphore.add_permits(1);
        }
    }

    /// `Retry-After` hint: the service's wait, in whole seconds rounded up and
    /// never under a second. A client told to come back after the time a
    /// request would have waited is not turned away again on arrival.
    fn retry_after_ms(&self) -> u64 {
        let wait_ms = u64::try_from(self.wait.lock().as_millis()).unwrap_or(u64::MAX);
        wait_ms
            .div_ceil(MIN_RETRY_AFTER_MS)
            .max(1)
            .saturating_mul(MIN_RETRY_AFTER_MS)
    }

    fn refuse(&self, reason: &str) -> CoreError {
        self.rejected.fetch_add(1, Ordering::Relaxed);
        if !must_not_wait() {
            self.warn_rate_limited(reason);
        }
        CoreError::BackendBusy {
            backend: self.label.clone(),
            retry_after_ms: self.retry_after_ms(),
        }
    }

    fn warn_rate_limited(&self, reason: &str) {
        let now_ms = u64::try_from(EPOCH.elapsed().as_millis())
            .unwrap_or(u64::MAX)
            .max(1);
        let last = self.last_warn_ms.load(Ordering::Relaxed);
        let due = last == 0 || now_ms.saturating_sub(last) >= WARN_EVERY.as_millis() as u64;
        if due
            && self
                .last_warn_ms
                .compare_exchange(last, now_ms, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            let stats = self.stats();
            tracing::warn!(
                service = %self.label,
                reason,
                limit = ?stats.limit,
                in_flight = stats.in_flight,
                waiting = stats.waiting,
                rejected_total = stats.rejected,
                "model service is at its concurrency limit; request refused"
            );
        }
    }

    /// Takes a slot. Waits at most the configured wait, at most `limit`
    /// requests wait at once, and inside [`without_waiting`] it does not wait at
    /// all. Cancel-safe: dropping the future while it waits leaves nothing
    /// behind.
    pub async fn acquire(self: &Arc<Self>) -> Result<ServicePermit, CoreError> {
        let permit = match self.semaphore.try_acquire() {
            Ok(permit) => permit,
            Err(_) => {
                if must_not_wait() {
                    return Err(self.refuse("no free slot"));
                }
                let queue_cap = self.capacity.lock().limit;
                if self.waiting.fetch_add(1, Ordering::AcqRel) >= queue_cap {
                    self.waiting.fetch_sub(1, Ordering::AcqRel);
                    return Err(self.refuse("waiting queue full"));
                }
                let _waiting = Waiting(&self.waiting);
                let wait = *self.wait.lock();
                match tokio::time::timeout(wait, self.semaphore.acquire()).await {
                    Ok(Ok(permit)) => permit,
                    _ => return Err(self.refuse("wait for a slot timed out")),
                }
            }
        };
        permit.forget();
        Ok(ServicePermit {
            limiter: self.clone(),
        })
    }
}

type Key = (String, i64);

static LIMITERS: LazyLock<DashMap<Key, Arc<ServiceLimiter>>> = LazyLock::new(DashMap::new);

fn label(node_id: &str, service_id: i64) -> String {
    let short: String = node_id.chars().take(8).collect();
    format!("service {service_id} on node {short}")
}

/// The limiter of one service, created unlimited on first use. The same `Arc`
/// is returned while the service exists, so it survives client rebuilds. The
/// label names the service for logs only; it is never sent to a client.
pub fn limiter_for(node_id: &str, service_id: i64) -> Arc<ServiceLimiter> {
    let key = (node_id.to_string(), service_id);
    if let Some(found) = LIMITERS.get(&key) {
        return found.clone();
    }
    LIMITERS
        .entry(key)
        .or_insert_with(|| ServiceLimiter::unlimited(label(node_id, service_id)))
        .clone()
}

/// Sets the limit of one service from its `services.config_json`: `Some(n)`
/// when the key is present, `None` (no limit) when it is absent or removed.
pub fn configure(node_id: &str, service_id: i64, max_concurrent: Option<u32>, wait: Duration) {
    limiter_for(node_id, service_id).configure(max_concurrent.map(|n| n as usize), wait);
}

/// Drops the limiters nothing refers to any more: services that are gone and
/// whose clients and handles were dropped (the map holds the only `Arc`).
/// `keep` names the services of this node that still exist.
pub fn prune(node_id: &str, keep: impl Fn(i64) -> bool) {
    LIMITERS.retain(|(node, service), limiter| {
        Arc::strong_count(limiter) > 1 || (node == node_id && keep(*service))
    });
}

/// Number of limiters in the map. For tests.
#[cfg(test)]
fn map_len(node_id: &str) -> usize {
    LIMITERS.iter().filter(|e| e.key().0 == node_id).count()
}

/// Hands `permit` to a stream so the slot is returned when the stream ends
/// (not only when it is dropped: a consumer may keep a finished stream around).
pub struct HoldUntilEnd<S> {
    inner: S,
    permit: Option<ServicePermit>,
}

impl<S> HoldUntilEnd<S> {
    pub fn new(inner: S, permit: Option<ServicePermit>) -> Self {
        Self { inner, permit }
    }
}

impl<S: futures::Stream + Unpin> futures::Stream for HoldUntilEnd<S> {
    type Item = S::Item;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let polled = std::pin::Pin::new(&mut self.inner).poll_next(cx);
        if matches!(polled, std::task::Poll::Ready(None)) {
            self.permit = None;
        }
        polled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    const WAIT: Duration = Duration::from_secs(5);

    #[tokio::test]
    async fn unconfigured_limiter_admits_everything() {
        let limiter = ServiceLimiter::unlimited("t");
        let mut held = Vec::new();
        for _ in 0..1000 {
            held.push(limiter.acquire().await.expect("unlimited"));
        }
        assert_eq!(limiter.in_flight(), 1000);
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_limiter_waits_then_refuses_with_a_typed_error() {
        let limiter = ServiceLimiter::with_limit("svc", 1, Duration::from_millis(250));
        let first = limiter.acquire().await.unwrap();
        let started = tokio::time::Instant::now();
        let refused = limiter.acquire().await.unwrap_err();
        assert!(started.elapsed() >= Duration::from_millis(250));
        assert!(
            matches!(
                refused,
                CoreError::BackendBusy {
                    retry_after_ms: 1000,
                    ..
                }
            ),
            "got {refused:?}"
        );
        assert_eq!(refused.status_code(), 503);
        assert_eq!(refused.retry_after_secs(), Some(1));
        drop(first);
        limiter.acquire().await.expect("slot is free again");
    }

    #[tokio::test]
    async fn a_waiter_is_admitted_when_a_slot_frees() {
        let limiter = ServiceLimiter::with_limit("svc", 1, WAIT);
        let first = limiter.acquire().await.unwrap();
        let waiter = tokio::spawn({
            let limiter = limiter.clone();
            async move { limiter.acquire().await.map(|_| ()) }
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        drop(first);
        waiter.await.unwrap().expect("admitted after release");
    }

    #[tokio::test]
    async fn growing_wakes_a_waiter() {
        let limiter = ServiceLimiter::with_limit("svc", 1, WAIT);
        let _first = limiter.acquire().await.unwrap();
        let waiter = tokio::spawn({
            let limiter = limiter.clone();
            async move { limiter.acquire().await }
        });
        tokio::task::yield_now().await;
        limiter.configure(Some(2), WAIT);
        let permit = waiter.await.unwrap().expect("admitted by the new slot");
        assert_eq!(limiter.in_flight(), 2);
        drop(permit);
    }

    #[tokio::test]
    async fn shrinking_does_not_over_admit() {
        let limiter = ServiceLimiter::with_limit("svc", 4, Duration::from_millis(20));
        let mut held = Vec::new();
        for _ in 0..4 {
            held.push(limiter.acquire().await.unwrap());
        }
        limiter.configure(Some(2), Duration::from_millis(20));
        assert_eq!(limiter.limit(), Some(2));

        // Four calls are still in flight; none is cut short and none is admitted.
        assert_eq!(limiter.in_flight(), 4);
        assert!(limiter.acquire().await.is_err());

        // Two finish: still at the new limit, so nothing new fits.
        held.pop();
        held.pop();
        assert!(limiter.acquire().await.is_err(), "limit is 2, 2 in flight");

        // One more finishes: exactly one slot is free again.
        held.pop();
        let again = limiter.acquire().await.expect("one slot below the limit");
        assert!(limiter.acquire().await.is_err());
        drop(again);
        drop(held);
        assert_eq!(limiter.in_flight(), 0);
    }

    #[tokio::test]
    async fn growing_after_a_shrink_pays_the_debt_first() {
        let limiter = ServiceLimiter::with_limit("svc", 4, Duration::from_millis(20));
        let held: Vec<_> = futures::future::join_all((0..4).map(|_| limiter.acquire()))
            .await
            .into_iter()
            .map(|p| p.unwrap())
            .collect();
        limiter.configure(Some(2), Duration::from_millis(20));
        limiter.configure(Some(3), Duration::from_millis(20));
        // Limit 3, four in flight: nothing is admitted until two finish.
        assert!(limiter.acquire().await.is_err());
        drop(held);
        let mut again = Vec::new();
        for _ in 0..3 {
            again.push(limiter.acquire().await.unwrap());
        }
        assert!(limiter.acquire().await.is_err(), "exactly 3 slots");
    }

    #[tokio::test]
    async fn the_global_map_returns_the_same_limiter_for_a_rebuilt_client() {
        let node = "limiter-map-test-node";
        configure(node, 71, Some(1), Duration::from_millis(20));
        let held = limiter_for(node, 71).acquire().await.unwrap();

        // A rebuilt client asks the map again and finds the limit in force.
        let again = limiter_for(node, 71);
        assert!(again.acquire().await.is_err());
        drop(held);
        assert!(again.acquire().await.is_ok());

        // A different service or node is independent and starts unlimited.
        assert!(limiter_for(node, 72).acquire().await.is_ok());
        assert!(limiter_for("other-node", 71).acquire().await.is_ok());
    }

    #[tokio::test]
    async fn a_stream_returns_its_slot_when_it_ends() {
        let limiter = ServiceLimiter::with_limit("svc", 1, Duration::from_millis(20));
        let permit = limiter.acquire().await.unwrap();
        let mut stream = HoldUntilEnd::new(futures::stream::iter([1, 2]), Some(permit));
        assert_eq!(stream.next().await, Some(1));
        assert!(limiter.acquire().await.is_err(), "held mid-stream");
        assert_eq!(stream.next().await, Some(2));
        assert!(
            limiter.acquire().await.is_err(),
            "held until the end is seen"
        );
        assert_eq!(stream.next().await, None);
        limiter.acquire().await.expect("released at the end");
    }

    #[tokio::test]
    async fn a_dropped_stream_returns_its_slot() {
        let limiter = ServiceLimiter::with_limit("svc", 1, Duration::from_millis(20));
        let permit = limiter.acquire().await.unwrap();
        let stream = HoldUntilEnd::new(futures::stream::pending::<u8>(), Some(permit));
        assert!(limiter.acquire().await.is_err());
        drop(stream);
        limiter.acquire().await.expect("released on drop");
    }

    #[tokio::test]
    async fn a_removed_limit_makes_the_limiter_unlimited_again() {
        let limiter = ServiceLimiter::with_limit("svc", 1, Duration::from_millis(20));
        let _held = limiter.acquire().await.unwrap();
        assert!(limiter.acquire().await.is_err());
        limiter.configure(None, Duration::from_millis(20));
        assert_eq!(limiter.limit(), None);
        let mut more = Vec::new();
        for _ in 0..50 {
            more.push(limiter.acquire().await.expect("no limit"));
        }
        // Zero reads as "not set".
        limiter.configure(Some(0), Duration::ZERO);
        assert_eq!(limiter.limit(), None);
    }

    #[tokio::test]
    async fn without_waiting_refuses_at_once_and_does_not_leak_into_other_calls() {
        let limiter = ServiceLimiter::with_limit("svc", 1, WAIT);
        let held = limiter.acquire().await.unwrap();

        let started = std::time::Instant::now();
        let refused = without_waiting(limiter.acquire()).await;
        assert!(matches!(refused, Err(CoreError::BackendBusy { .. })));
        assert!(started.elapsed() < Duration::from_secs(1), "must not wait");

        // A free slot is still taken inside the scope.
        drop(held);
        let free = without_waiting(limiter.acquire()).await.expect("free slot");
        drop(free);

        // Outside the scope the call waits again.
        let held = limiter.acquire().await.unwrap();
        let waiter = tokio::spawn({
            let limiter = limiter.clone();
            async move { limiter.acquire().await.map(|_| ()) }
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        drop(held);
        waiter.await.unwrap().expect("waited, then admitted");
    }

    #[tokio::test]
    async fn no_more_requests_wait_than_the_limit_allows() {
        let limiter = ServiceLimiter::with_limit("svc", 1, WAIT);
        let held = limiter.acquire().await.unwrap();
        let first_waiter = tokio::spawn({
            let limiter = limiter.clone();
            async move { limiter.acquire().await.map(|_| ()) }
        });
        while limiter.stats().waiting < 1 {
            tokio::task::yield_now().await;
        }
        // The queue is as long as the limit (1): the next one is refused at once.
        let started = std::time::Instant::now();
        assert!(limiter.acquire().await.is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(limiter.stats().rejected, 1);
        drop(held);
        first_waiter
            .await
            .unwrap()
            .expect("the queued one is served");
        assert_eq!(limiter.stats().waiting, 0);
    }

    #[tokio::test]
    async fn retry_after_follows_the_wait_in_whole_seconds() {
        let limiter = ServiceLimiter::with_limit("svc", 1, Duration::from_millis(2_500));
        assert_eq!(limiter.retry_after_ms(), 3_000);
        limiter.configure(Some(1), Duration::from_millis(10));
        assert_eq!(limiter.retry_after_ms(), 1_000, "never under a second");
        limiter.configure(Some(1), Duration::from_secs(30));
        assert_eq!(limiter.retry_after_ms(), 30_000);
    }

    #[tokio::test]
    async fn stats_report_the_limit_and_what_is_in_flight() {
        let limiter = ServiceLimiter::with_limit("svc", 3, Duration::from_millis(10));
        let _a = limiter.acquire().await.unwrap();
        let _b = limiter.acquire().await.unwrap();
        assert_eq!(
            limiter.stats(),
            LimiterStats {
                limit: Some(3),
                in_flight: 2,
                waiting: 0,
                rejected: 0
            }
        );
    }

    #[tokio::test]
    async fn prune_drops_only_unreferenced_limiters_of_services_that_are_gone() {
        let node = "limiter-prune-node";
        let kept = limiter_for(node, 1); // still referenced by a handle
        let _ = limiter_for(node, 2); // gone and unreferenced
        let _ = limiter_for(node, 3); // exists in the snapshot
        assert_eq!(map_len(node), 3);

        prune(node, |id| id == 3);
        assert_eq!(map_len(node), 2, "service 2 is pruned");

        drop(kept);
        prune(node, |id| id == 3);
        assert_eq!(map_len(node), 1, "service 1 goes once its handle is gone");
    }
}
