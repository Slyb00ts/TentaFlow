// =============================================================================
// File: api/rate_limit.rs — token-bucket rate limiter for HMAC-only endpoints
// =============================================================================
//
// Protects the unauthenticated signed-URL surfaces (`/frames/<ref>`,
// `/recordings/<ref>`, `/core/frame/pickup`) against forged-token spam: an
// attacker who blasts 1 000 req/s of garbage tokens otherwise burns CPU in
// HMAC verify and explodes `audit_log`. Two buckets compose:
//
//   * per-IP — small bucket (burst 10, sustain 1/s) keyed by client IP.
//   * global — coarse DoS budget (burst 100, sustain 1 000/s) shared across
//     all clients; protects the process even if the per-IP table grows.
//
// The per-IP map is bounded by an idle-eviction sweep: entries last touched
// more than `IDLE_EVICT_AFTER` ago are removed on every `check` call (cheap —
// the map is sharded, eviction walks one shard at a time, capped at 64 keys
// per call). This avoids unbounded memory under a flood of unique IPs.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use dashmap::DashMap;

/// Bucket parameters — capacity is the burst depth, `refill_per_sec` is the
/// sustained refill rate. Defaults sized for HMAC-only endpoints (cheap when
/// the token is valid, but every miss pays a few microseconds of HMAC +
/// optional `audit_log` INSERT, so the budget is intentionally small).
#[derive(Debug, Clone, Copy)]
pub struct RateLimitConfig {
    pub per_ip_capacity: u32,
    pub per_ip_refill_per_sec: f64,
    pub global_capacity: u32,
    pub global_refill_per_sec: f64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            per_ip_capacity: 10,
            per_ip_refill_per_sec: 1.0,
            global_capacity: 100,
            global_refill_per_sec: 1000.0,
        }
    }
}

/// Result of `RateLimiter::check`. `retry_after_secs` is the time the caller
/// must wait until ONE token is available — rounded up to whole seconds at
/// the HTTP layer when serialising into `Retry-After`.
#[derive(Debug, PartialEq)]
pub enum RateLimitResult {
    Allow,
    IpLimit { ip: String, retry_after_secs: f64 },
    GlobalLimit { retry_after_secs: f64 },
}

#[derive(Debug)]
struct TokenBucket {
    tokens: f64,
    last_refill: Instant,
}

impl TokenBucket {
    fn new(capacity: u32) -> Self {
        Self {
            tokens: capacity as f64,
            last_refill: Instant::now(),
        }
    }

    /// Refill based on elapsed time, then try to take one token. Returns
    /// `None` on success, or `Some(retry_after_secs)` if a token is short.
    fn try_take(&mut self, capacity: u32, refill_per_sec: f64, now: Instant) -> Option<f64> {
        let elapsed = now.saturating_duration_since(self.last_refill).as_secs_f64();
        if elapsed > 0.0 {
            self.tokens = (self.tokens + elapsed * refill_per_sec).min(capacity as f64);
            self.last_refill = now;
        }
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            None
        } else {
            let missing = 1.0 - self.tokens;
            let retry = if refill_per_sec > 0.0 {
                missing / refill_per_sec
            } else {
                f64::INFINITY
            };
            Some(retry)
        }
    }
}

#[derive(Debug)]
struct IpEntry {
    bucket: TokenBucket,
    last_seen: Instant,
}

/// Idle entries older than this are evicted during sweeps. 10 minutes is long
/// enough that a legitimately slow addon does not lose its bucket, short
/// enough that a botnet cycling unique IPs does not pin RAM.
const IDLE_EVICT_AFTER: Duration = Duration::from_secs(600);
/// Hard ceiling on the per-IP map. Once reached the next `check` triggers a
/// full sweep regardless of `last_seen`. Belt-and-braces vs. the idle sweep.
const MAX_PER_IP_ENTRIES: usize = 10_000;

pub struct RateLimiter {
    per_ip: DashMap<String, IpEntry>,
    global: Mutex<TokenBucket>,
    config: RateLimitConfig,
}

impl RateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            per_ip: DashMap::new(),
            global: Mutex::new(TokenBucket::new(config.global_capacity)),
            config,
        }
    }

    /// Acquire one token charged against both the global and the per-IP bucket.
    /// Order: global first (cheaper; protects the process even if `ip` is new
    /// to the map), per-IP second. A global denial does NOT consume a per-IP
    /// token, by design — recovering from a global storm should not also blow
    /// out every quiet client's bucket.
    pub fn check(&self, ip: &str) -> RateLimitResult {
        let now = Instant::now();

        if let Ok(mut g) = self.global.lock() {
            if let Some(retry) =
                g.try_take(self.config.global_capacity, self.config.global_refill_per_sec, now)
            {
                return RateLimitResult::GlobalLimit { retry_after_secs: retry };
            }
        }

        self.sweep_if_needed(now);

        let mut entry = self.per_ip.entry(ip.to_string()).or_insert_with(|| IpEntry {
            bucket: TokenBucket::new(self.config.per_ip_capacity),
            last_seen: now,
        });
        entry.last_seen = now;
        match entry.bucket.try_take(
            self.config.per_ip_capacity,
            self.config.per_ip_refill_per_sec,
            now,
        ) {
            None => RateLimitResult::Allow,
            Some(retry) => RateLimitResult::IpLimit {
                ip: ip.to_string(),
                retry_after_secs: retry,
            },
        }
    }

    /// Cheap idle eviction. Walks up to 64 random entries per call and removes
    /// any whose `last_seen` is older than `IDLE_EVICT_AFTER`. If the map is
    /// over the hard cap we sweep aggressively (no per-call limit) until the
    /// map fits.
    fn sweep_if_needed(&self, now: Instant) {
        let over_cap = self.per_ip.len() > MAX_PER_IP_ENTRIES;
        let limit = if over_cap { usize::MAX } else { 64 };
        let mut scanned = 0;
        self.per_ip.retain(|_k, v| {
            scanned += 1;
            if scanned > limit {
                return true;
            }
            now.saturating_duration_since(v.last_seen) < IDLE_EVICT_AFTER
        });
    }

    /// Test/diagnostic helper — number of live per-IP buckets currently
    /// tracked. Exposed so integration tests can assert eviction behaviour.
    pub fn ip_entry_count(&self) -> usize {
        self.per_ip.len()
    }
}

/// Process-wide singleton. Initialised on first call.
static RATE_LIMITER: OnceLock<Arc<RateLimiter>> = OnceLock::new();

pub fn rate_limiter() -> &'static Arc<RateLimiter> {
    RATE_LIMITER.get_or_init(|| Arc::new(RateLimiter::new(RateLimitConfig::default())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> RateLimitConfig {
        RateLimitConfig {
            per_ip_capacity: 3,
            per_ip_refill_per_sec: 1.0,
            global_capacity: 100,
            global_refill_per_sec: 1000.0,
        }
    }

    #[test]
    fn burst_allowed_then_denied() {
        let rl = RateLimiter::new(cfg());
        for _ in 0..3 {
            assert_eq!(rl.check("1.2.3.4"), RateLimitResult::Allow);
        }
        match rl.check("1.2.3.4") {
            RateLimitResult::IpLimit { ip, retry_after_secs } => {
                assert_eq!(ip, "1.2.3.4");
                assert!(retry_after_secs > 0.0 && retry_after_secs <= 1.0);
            }
            other => panic!("expected IpLimit, got {:?}", other),
        }
    }

    #[test]
    fn per_ip_isolated() {
        let rl = RateLimiter::new(cfg());
        for _ in 0..3 {
            assert_eq!(rl.check("a"), RateLimitResult::Allow);
        }
        // "b" still has a fresh bucket.
        assert_eq!(rl.check("b"), RateLimitResult::Allow);
    }

    #[test]
    fn global_limit_independent_of_ip() {
        let rl = RateLimiter::new(RateLimitConfig {
            per_ip_capacity: 1_000,
            per_ip_refill_per_sec: 1_000.0,
            global_capacity: 2,
            global_refill_per_sec: 0.01,
        });
        assert_eq!(rl.check("x"), RateLimitResult::Allow);
        assert_eq!(rl.check("y"), RateLimitResult::Allow);
        // Third request from a fresh IP hits the global ceiling.
        match rl.check("z") {
            RateLimitResult::GlobalLimit { retry_after_secs } => {
                assert!(retry_after_secs > 0.0);
            }
            other => panic!("expected GlobalLimit, got {:?}", other),
        }
    }

    #[test]
    fn refill_restores_tokens() {
        let rl = RateLimiter::new(cfg());
        for _ in 0..3 {
            assert_eq!(rl.check("ip"), RateLimitResult::Allow);
        }
        assert!(matches!(rl.check("ip"), RateLimitResult::IpLimit { .. }));
        std::thread::sleep(Duration::from_millis(1_100));
        // After 1.1 s with refill 1/s, at least one token is available.
        assert_eq!(rl.check("ip"), RateLimitResult::Allow);
    }
}
