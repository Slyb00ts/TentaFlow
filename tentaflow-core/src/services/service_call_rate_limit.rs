// =============================================================================
// File: services/service_call_rate_limit.rs — per-addon limiter for service_call_v1
// =============================================================================
//
// Per-addon token bucket guarding `service_request` (a.k.a. `service_call_v1`)
// host calls. Default budget: burst 100, sustain 1000 req/min (= 16.67 req/s).
// An addon spamming 10 000 req/s would otherwise drain shared backend services
// (yolo, whisper, ...) — this limiter is the first line of defence before the
// alias resolver / QUIC dispatch.
//
// One bucket per `addon_id` keyed via DashMap. Bounded by an LRU eviction at
// `MAX_ADDON_ENTRIES = 10_000` to keep memory finite under runaway addon-id
// churn (e.g. test harnesses installing thousands of throwaway addons). Idle
// eviction sweeps entries untouched for `IDLE_EVICT_AFTER`. Pattern mirrors
// `api::rate_limit::RateLimiter` — same TokenBucket primitive (extracted to
// `util::token_bucket`).
//
// The limiter is a process-wide singleton (`service_call_rate_limiter()`),
// initialised on first call. Caller integration is at the top of
// `addon::host_functions::service::service_request` — denial returns
// `AbiError::QuotaExceeded` (code 11, reused from M1.W7 streaming subs).

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use dashmap::DashMap;

use crate::util::token_bucket::TokenBucket;

/// Tunable bucket parameters. `per_addon_capacity` is burst depth (immediate
/// allowance), `per_addon_refill_per_sec` is the sustained refill rate.
/// Defaults sized for the F1b handoff target: 1000 req/min sustain, 100
/// burst — generous enough for legitimate vision-loop addons that fan a
/// frame out to multiple backends, restrictive enough that a self-DoS bug
/// or an attacker can't blast a shared yolo service.
#[derive(Debug, Clone, Copy)]
pub struct ServiceCallRateLimitConfig {
    pub per_addon_capacity: u32,
    pub per_addon_refill_per_sec: f64,
}

impl Default for ServiceCallRateLimitConfig {
    fn default() -> Self {
        Self {
            per_addon_capacity: 100,
            per_addon_refill_per_sec: 16.67,
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum RateLimitResult {
    Allow,
    AddonLimit { addon_id: String, retry_after_secs: f64 },
}

#[derive(Debug)]
struct AddonEntry {
    bucket: TokenBucket,
    last_seen: Instant,
}

/// Idle entries older than this are evicted during sweeps. 10 minutes is long
/// enough that a quiet addon does not lose its bucket between calls, short
/// enough that a churn of throwaway addon-ids does not pin RAM.
const IDLE_EVICT_AFTER: Duration = Duration::from_secs(600);
/// Hard ceiling on the per-addon map. Once reached the next `check` triggers
/// an LRU pass evicting the oldest 25 % regardless of `last_seen`.
const MAX_ADDON_ENTRIES: usize = 10_000;

pub struct ServiceCallRateLimiter {
    per_addon: DashMap<String, AddonEntry>,
    config: ServiceCallRateLimitConfig,
}

impl ServiceCallRateLimiter {
    pub fn new(config: ServiceCallRateLimitConfig) -> Self {
        Self {
            per_addon: DashMap::new(),
            config,
        }
    }

    /// Acquire one token for `addon_id`. Allocates a fresh bucket on first
    /// sighting. On denial returns the wait time until ONE token refills —
    /// callers round up to seconds when serialising into audit details.
    pub fn check(&self, addon_id: &str) -> RateLimitResult {
        let now = Instant::now();
        self.sweep_if_needed(now);

        let mut entry = self
            .per_addon
            .entry(addon_id.to_string())
            .or_insert_with(|| AddonEntry {
                bucket: TokenBucket::new(self.config.per_addon_capacity),
                last_seen: now,
            });
        entry.last_seen = now;
        match entry.bucket.refill_and_peek(
            self.config.per_addon_capacity,
            self.config.per_addon_refill_per_sec,
            now,
        ) {
            Ok(()) => {
                entry.bucket.commit_one();
                RateLimitResult::Allow
            }
            Err(retry) => RateLimitResult::AddonLimit {
                addon_id: addon_id.to_string(),
                retry_after_secs: retry,
            },
        }
    }

    /// Cheap idle eviction. Walks up to 64 random entries per call. If after
    /// the idle pass the map is still at or above the hard cap, evicts the
    /// oldest 25 % by `last_seen` (approximate LRU).
    fn sweep_if_needed(&self, now: Instant) {
        let over_cap = self.per_addon.len() >= MAX_ADDON_ENTRIES;
        let limit = if over_cap { usize::MAX } else { 64 };
        let mut scanned = 0;
        self.per_addon.retain(|_k, v| {
            scanned += 1;
            if scanned > limit {
                return true;
            }
            now.saturating_duration_since(v.last_seen) < IDLE_EVICT_AFTER
        });

        if self.per_addon.len() >= MAX_ADDON_ENTRIES {
            let target = MAX_ADDON_ENTRIES * 3 / 4;
            let mut snapshot: Vec<(String, Instant)> = self
                .per_addon
                .iter()
                .map(|e| (e.key().clone(), e.value().last_seen))
                .collect();
            snapshot.sort_by_key(|(_, ts)| *ts);
            let drop_count = snapshot.len().saturating_sub(target);
            for (key, _) in snapshot.into_iter().take(drop_count) {
                self.per_addon.remove(&key);
            }
        }
    }

    /// Test/diagnostic helper — number of live per-addon buckets currently
    /// tracked. Used by integration tests to assert eviction behaviour.
    pub fn addon_entry_count(&self) -> usize {
        self.per_addon.len()
    }
}

static SERVICE_CALL_RATE_LIMITER: OnceLock<Arc<ServiceCallRateLimiter>> = OnceLock::new();

pub fn service_call_rate_limiter() -> &'static Arc<ServiceCallRateLimiter> {
    SERVICE_CALL_RATE_LIMITER.get_or_init(|| {
        Arc::new(ServiceCallRateLimiter::new(
            ServiceCallRateLimitConfig::default(),
        ))
    })
}

// -----------------------------------------------------------------------------
// Collapsed audit map for rate-limit denials
// -----------------------------------------------------------------------------
//
// Mirrors `api::dashboard::server::RATE_LIMIT_AUDIT`. Under a DoS the denied
// requests can each be 1000s/s — emitting one `audit_log` row per denial
// would itself become the DoS. Coalesce: at most one row per
// `AUDIT_DENY_WINDOW` per `addon_id`, carrying the in-window count.

static SERVICE_CALL_AUDIT: OnceLock<DashMap<String, (Instant, u32)>> = OnceLock::new();
pub const AUDIT_DENY_WINDOW: Duration = Duration::from_secs(60);
const AUDIT_IDLE_EVICT_AFTER: Duration = Duration::from_secs(120);
const MAX_AUDIT_ENTRIES: usize = 10_000;

fn service_call_audit_map() -> &'static DashMap<String, (Instant, u32)> {
    SERVICE_CALL_AUDIT.get_or_init(DashMap::new)
}

fn sweep_audit_map(now: Instant) {
    let map = service_call_audit_map();
    if map.len() < 1_000 {
        return;
    }
    map.retain(|_, (last_seen, _)| now.saturating_duration_since(*last_seen) < AUDIT_IDLE_EVICT_AFTER);
    if map.len() >= MAX_AUDIT_ENTRIES {
        let target = MAX_AUDIT_ENTRIES * 3 / 4;
        let mut snapshot: Vec<(String, Instant)> =
            map.iter().map(|e| (e.key().clone(), e.value().0)).collect();
        snapshot.sort_by_key(|(_, ts)| *ts);
        let drop_count = snapshot.len().saturating_sub(target);
        for (key, _) in snapshot.into_iter().take(drop_count) {
            map.remove(&key);
        }
    }
}

/// Outcome of `note_denial_for_audit`: when `Emit` is returned the caller
/// should write a single collapsed `audit_log` row carrying `denied_count`;
/// `Skip` means the previous row inside the window already covers this denial.
pub enum AuditEmitDecision {
    Emit { denied_count: u32 },
    Skip,
}

/// Records a denial for `addon_id` and returns whether the caller should emit
/// a collapsed audit row. Cleans up stale entries on every call.
pub fn note_denial_for_audit(addon_id: &str) -> AuditEmitDecision {
    let now = Instant::now();
    sweep_audit_map(now);
    let map = service_call_audit_map();
    // Two cases:
    //   * fresh addon_id — insert anchor (now, 1) and emit denied_count=1.
    //   * existing entry — bump count; emit (with count) only when the
    //     window has fully elapsed, otherwise skip.
    let mut decision = AuditEmitDecision::Skip;
    map.entry(addon_id.to_string())
        .and_modify(|(anchor, count)| {
            *count = count.saturating_add(1);
            if now.saturating_duration_since(*anchor) >= AUDIT_DENY_WINDOW {
                decision = AuditEmitDecision::Emit { denied_count: *count };
                *anchor = now;
                *count = 0;
            }
        })
        .or_insert_with(|| {
            decision = AuditEmitDecision::Emit { denied_count: 1 };
            (now, 0)
        });
    decision
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ServiceCallRateLimitConfig {
        ServiceCallRateLimitConfig {
            per_addon_capacity: 3,
            per_addon_refill_per_sec: 1.0,
        }
    }

    #[test]
    fn per_addon_burst_allowed() {
        let rl = ServiceCallRateLimiter::new(ServiceCallRateLimitConfig {
            per_addon_capacity: 100,
            per_addon_refill_per_sec: 0.01,
        });
        for _ in 0..100 {
            assert_eq!(rl.check("addon-a"), RateLimitResult::Allow);
        }
    }

    #[test]
    fn per_addon_burst_exceeded_denied() {
        let rl = ServiceCallRateLimiter::new(ServiceCallRateLimitConfig {
            per_addon_capacity: 100,
            per_addon_refill_per_sec: 0.001,
        });
        for _ in 0..100 {
            assert_eq!(rl.check("addon-a"), RateLimitResult::Allow);
        }
        match rl.check("addon-a") {
            RateLimitResult::AddonLimit { addon_id, retry_after_secs } => {
                assert_eq!(addon_id, "addon-a");
                assert!(retry_after_secs > 0.0);
            }
            other => panic!("expected AddonLimit, got {:?}", other),
        }
    }

    #[test]
    fn different_addons_independent() {
        let rl = ServiceCallRateLimiter::new(cfg());
        for _ in 0..3 {
            assert_eq!(rl.check("addon-a"), RateLimitResult::Allow);
        }
        assert!(matches!(rl.check("addon-a"), RateLimitResult::AddonLimit { .. }));
        // addon-b still has a fresh bucket.
        for _ in 0..3 {
            assert_eq!(rl.check("addon-b"), RateLimitResult::Allow);
        }
    }

    #[test]
    fn eviction_at_hard_cap() {
        let rl = ServiceCallRateLimiter::new(ServiceCallRateLimitConfig {
            per_addon_capacity: 1,
            per_addon_refill_per_sec: 1.0,
        });
        for n in 0..11_000 {
            let id = format!("addon-{n}");
            let _ = rl.check(&id);
        }
        assert!(
            rl.addon_entry_count() <= MAX_ADDON_ENTRIES,
            "map size {} exceeded hard cap {}",
            rl.addon_entry_count(),
            MAX_ADDON_ENTRIES
        );
    }

    #[test]
    fn refill_resumes_after_quota() {
        let rl = ServiceCallRateLimiter::new(cfg());
        for _ in 0..3 {
            assert_eq!(rl.check("addon-a"), RateLimitResult::Allow);
        }
        assert!(matches!(rl.check("addon-a"), RateLimitResult::AddonLimit { .. }));
        std::thread::sleep(Duration::from_millis(1_100));
        assert_eq!(rl.check("addon-a"), RateLimitResult::Allow);
    }

    #[test]
    fn audit_collapse_first_emits_subsequent_skip() {
        let id = format!("collapse-test-{}", uuid::Uuid::new_v4());
        match note_denial_for_audit(&id) {
            AuditEmitDecision::Emit { denied_count } => assert_eq!(denied_count, 1),
            AuditEmitDecision::Skip => panic!("first denial must emit"),
        }
        for _ in 0..10 {
            assert!(matches!(note_denial_for_audit(&id), AuditEmitDecision::Skip));
        }
    }
}
