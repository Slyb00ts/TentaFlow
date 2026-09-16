// =============================================================================
// File: bus/quota.rs — TentaBus M1: per-org token-bucket quotas (PLAN §7.1)
// =============================================================================
//
// "Kwoty per org... w praktyce nielimitujące" by default — they exist so
// one team cannot starve a node, not as a routine throttle. Checked on the
// publish path only (PLAN §5.3.7's backpressure is the engine's own
// mechanism for a full write channel; this is the org-level ceiling in
// front of it).

use std::time::Instant;

use dashmap::DashMap;
use parking_lot::Mutex;

use crate::db::repository::{self, DbBusQuota};
use crate::db::DbPool;

use super::BusServiceError;

#[derive(Debug, Clone, Copy)]
pub struct QuotaConfig {
    pub max_topics: u32,
    pub max_partitions: u32,
    pub max_bytes_total: u64,
    pub produce_msgs_per_sec: u32,
    pub produce_bytes_per_sec: u64,
    /// Ceiling on the number of DISTINCT `(group, topic)` rows this org may
    /// have in `bus_groups` at once — see `DEFAULT_MAX_GROUPS`'s doc.
    /// Promoted from a separate `QuotaManager`-only override into this
    /// struct (follow-up toru P, task 6) now that `dispatch/bus.rs`'s
    /// `quota_set_v1` is no longer under concurrent edit and can construct
    /// this field directly: `QuotaConfig` is meant to be "the whole org
    /// quota" wire-for-wire, and a caller reading `QuotaGetResponse` had no
    /// way to see this figure at all while it lived outside the struct.
    pub max_groups: u32,
}

impl Default for QuotaConfig {
    fn default() -> Self {
        Self {
            max_topics: 100,
            max_partitions: 1024,
            max_bytes_total: 1024 * 1024 * 1024 * 1024, // 1 TiB
            produce_msgs_per_sec: 200_000,
            produce_bytes_per_sec: 2 * 1024 * 1024 * 1024, // 2 GiB/s
            max_groups: DEFAULT_MAX_GROUPS,
        }
    }
}

/// Default ceiling on the number of DISTINCT `(group, topic)` rows an org
/// may have in `bus_groups` at once (PLAN §7.1-style resource quota, same
/// spirit as `QuotaConfig::max_topics`/`max_partitions`) — enforced by
/// `BusService::open_consumer` only when it is about to create a NEW row
/// (an existing group reconnecting never counts against this again).
pub const DEFAULT_MAX_GROUPS: u32 = 1000;

/// A continuously-refilling token bucket: capacity == rate, so it represents
/// "up to `rate` units per rolling second" rather than a burst allowance
/// that outgrows the configured rate.
struct TokenBucket {
    rate_per_sec: f64,
    state: Mutex<(f64, Instant)>,
}

impl TokenBucket {
    fn new(rate_per_sec: f64) -> Self {
        Self {
            rate_per_sec,
            state: Mutex::new((rate_per_sec, Instant::now())),
        }
    }

    /// A request whose `amount` alone exceeds this bucket's capacity
    /// (capacity == the configured rate, see the struct doc) can never
    /// succeed no matter how long the caller waits — the bucket never holds
    /// more than `rate_per_sec` tokens even at full refill. Reporting that
    /// as `QuotaExceeded { retry_after_ms }` would be a lie: the caller
    /// would retry forever, and each retry would generate another audit
    /// entry (the exact flood this quota layer exists to prevent). A rate
    /// of 0 ("unlimited", see `try_consume`) can never be exceeded by
    /// definition.
    fn oversized(&self, unit: &'static str, amount: f64) -> Option<super::BusServiceError> {
        if self.rate_per_sec > 0.0 && amount > self.rate_per_sec {
            Some(super::BusServiceError::QuotaRequestTooLarge {
                unit,
                amount: amount as u64,
                capacity: self.rate_per_sec as u64,
            })
        } else {
            None
        }
    }

    /// `Ok(())` and debits `amount` tokens, or `Err(retry_after_ms)` without
    /// debiting anything.
    fn try_consume(&self, amount: f64) -> Result<(), u32> {
        if self.rate_per_sec <= 0.0 {
            // A configured rate of 0 means "unlimited" (PLAN §7.1 defaults
            // are "w praktyce nielimitujące" — treating 0 as a hard block
            // would make an unset quota indistinguishable from a deny-all).
            return Ok(());
        }
        let mut guard = self.state.lock();
        let (tokens, last) = &mut *guard;
        let now = Instant::now();
        let elapsed = now.duration_since(*last).as_secs_f64();
        *tokens = (*tokens + elapsed * self.rate_per_sec).min(self.rate_per_sec);
        *last = now;
        if *tokens >= amount {
            *tokens -= amount;
            Ok(())
        } else {
            let deficit = amount - *tokens;
            let retry_after_ms = ((deficit / self.rate_per_sec) * 1000.0).ceil() as u32;
            Err(retry_after_ms.max(1))
        }
    }
}

struct OrgBuckets {
    config: QuotaConfig,
    msgs: TokenBucket,
    bytes: TokenBucket,
}

pub struct QuotaManager {
    orgs: DashMap<String, OrgBuckets>,
}

impl Default for QuotaManager {
    fn default() -> Self {
        Self::new()
    }
}

impl QuotaManager {
    pub fn new() -> Self {
        Self {
            orgs: DashMap::new(),
        }
    }

    /// Drops every bucket and limit override for `org_id` so a purged
    /// organization leaves no per-org state in memory; a later call for the
    /// same id starts from the defaults again.
    pub fn remove_org(&self, org_id: &str) {
        self.orgs.remove(org_id);
    }

    /// Sets/replaces an org's quota, resetting its token buckets to full.
    /// Called by admin operations (org settings) — not on the publish path.
    pub fn set_org_quota(&self, org_id: &str, cfg: QuotaConfig) {
        self.orgs.insert(
            org_id.to_string(),
            OrgBuckets {
                config: cfg,
                msgs: TokenBucket::new(cfg.produce_msgs_per_sec as f64),
                bytes: TokenBucket::new(cfg.produce_bytes_per_sec as f64),
            },
        );
    }

    fn buckets_or_default(
        &self,
        org_id: &str,
    ) -> dashmap::mapref::one::RefMut<'_, String, OrgBuckets> {
        self.orgs.entry(org_id.to_string()).or_insert_with(|| {
            let cfg = QuotaConfig::default();
            OrgBuckets {
                config: cfg,
                msgs: TokenBucket::new(cfg.produce_msgs_per_sec as f64),
                bytes: TokenBucket::new(cfg.produce_bytes_per_sec as f64),
            }
        })
    }

    /// Checks and debits both the message-rate and byte-rate buckets for a
    /// publish of `msgs` records totalling `bytes` bytes. Debits nothing if
    /// EITHER bucket is short, so a caller's retry does not silently drain
    /// the other bucket for a batch it never actually admitted.
    pub fn try_consume(&self, org_id: &str, msgs: u32, bytes: u64) -> Result<(), BusServiceError> {
        let entry = self.buckets_or_default(org_id);
        // Checked BEFORE any debit, and as a hard error rather than
        // `QuotaExceeded`: a request larger than the bucket's own capacity
        // can never be satisfied by waiting, so it must never carry a
        // `retry_after_ms` that promises otherwise (see `TokenBucket::
        // oversized`'s doc).
        if let Some(err) = entry.msgs.oversized("messages", msgs as f64) {
            return Err(err);
        }
        if let Some(err) = entry.bytes.oversized("bytes", bytes as f64) {
            return Err(err);
        }
        // Debit the msgs bucket first; if the bytes bucket then turns out to
        // be short, refund the msgs debit so a rejected batch never leaves a
        // partial charge behind. Two short-lived lock acquisitions are fine
        // at this call rate (per-batch, not per-record).
        let msg_check = entry.msgs.try_consume(msgs as f64);
        match msg_check {
            Ok(()) => {}
            Err(retry_after_ms) => {
                return Err(BusServiceError::QuotaExceeded { retry_after_ms });
            }
        }
        if let Err(retry_after_ms) = entry.bytes.try_consume(bytes as f64) {
            // Msgs bucket was already debited above; refund it since this
            // publish is being rejected as a whole.
            entry.msgs.refund(msgs as f64);
            return Err(BusServiceError::QuotaExceeded { retry_after_ms });
        }
        Ok(())
    }

    /// Configured per-org stored-bytes ceiling (PLAN §7.1 `max_bytes`),
    /// enforced by `BusService::publish` against
    /// `BusService::org_stored_bytes` — a running per-org total of LOG
    /// SEGMENT bytes, seeded once at startup from the engine's own
    /// per-partition segment accounting and then moved by publish and the
    /// retention sweep in that same unit. `publish` reads it as a map
    /// lookup; it never walks the filesystem. See that field's doc for what
    /// is counted (segments only — not the indexes, not the preallocated
    /// `dedup.bin`) and for its one blind spot.
    pub fn max_bytes_total(&self, org_id: &str) -> u64 {
        self.buckets_or_default(org_id).config.max_bytes_total
    }

    pub fn max_topics(&self, org_id: &str) -> u32 {
        self.buckets_or_default(org_id).config.max_topics
    }

    pub fn max_partitions(&self, org_id: &str) -> u32 {
        self.buckets_or_default(org_id).config.max_partitions
    }

    /// Configured `bus_groups` ceiling for `org_id` (`DEFAULT_MAX_GROUPS` if
    /// never overridden) — checked by `BusService::open_consumer` before it
    /// inserts a brand-new `bus_groups` row.
    pub fn max_groups(&self, org_id: &str) -> u32 {
        self.buckets_or_default(org_id).config.max_groups
    }

    /// Overrides ONLY `org_id`'s `max_groups` field, leaving every other
    /// quota figure (topics/partitions/bytes/rates) and the token buckets'
    /// current fill level untouched — unlike `set_org_quota`/
    /// `configure_org`, which replace the WHOLE config (`max_groups`
    /// included, now that it lives in `QuotaConfig`) and reset the buckets
    /// to full. Kept as a scoped setter for callers (admin tooling, tests)
    /// that only want to move this one ceiling.
    pub fn set_max_groups(&self, org_id: &str, max_groups: u32) {
        self.buckets_or_default(org_id).config.max_groups = max_groups;
    }

    /// Configured publish message-rate ceiling (`QuotaConfig::
    /// produce_msgs_per_sec`) — a plain getter alongside `max_topics`/
    /// `max_partitions`/`max_bytes_total` above, added so `QuotaGet`
    /// (dispatch layer) can report the SAME value `try_consume` actually
    /// enforces instead of an unconditional `None`.
    pub fn produce_msgs_per_sec(&self, org_id: &str) -> u32 {
        self.buckets_or_default(org_id).config.produce_msgs_per_sec
    }

    /// Configured publish byte-rate ceiling (`QuotaConfig::
    /// produce_bytes_per_sec`) — see `produce_msgs_per_sec`'s doc.
    pub fn produce_bytes_per_sec(&self, org_id: &str) -> u64 {
        self.buckets_or_default(org_id).config.produce_bytes_per_sec
    }

    /// Full-replace an org's quota configuration (topics/partitions/bytes
    /// ceilings, publish rate buckets), resetting its token buckets to full
    /// — the same operation `set_org_quota` already performs; kept as an
    /// alias so a caller reaching for "configure this org's quota" finds a
    /// method under either name. Does NOT touch `max_groups` (see
    /// `DEFAULT_MAX_GROUPS`'s doc for why that is a separate override) —
    /// call `set_max_groups` for that. `QuotaSet` (dispatch layer) calls
    /// this/`set_org_quota` directly; `QuotaGet` uses the getters above.
    pub fn configure_org(&self, org_id: &str, cfg: QuotaConfig) {
        self.set_org_quota(org_id, cfg);
    }
}

impl TokenBucket {
    fn refund(&self, amount: f64) {
        let mut guard = self.state.lock();
        guard.0 = (guard.0 + amount).min(self.rate_per_sec);
    }
}

// ---- Durability (`bus_org_quotas`, migration v154) ----------------------
//
// `QuotaManager` above is pure in-memory state and stays that way — it holds
// no `DbPool` and never reads one. Persistence is two free functions the
// layers that DO own a pool call: `dispatch/bus.rs`'s `quota_set_v1` writes
// (through `BusService::db`, the engine's own pool), and `BusService::new`
// reads back from that same pool at startup. Keeping the pool out of the
// manager leaves the publish hot path — `try_consume`, the getters — exactly
// as cheap as it was, and leaves the manager usable in tests with no
// database at all.

/// `QuotaConfig`'s byte fields are `u64`; SQLite's `INTEGER` is signed
/// 64-bit. Values above `i64::MAX` are CLAMPED rather than wrapped: a quota
/// of `i64::MAX` bytes (8 EiB) is already unlimited for any real
/// deployment, while a wrapped negative row would read back through
/// `quota_bytes_from_i64` as `0` — a ceiling no publish could ever satisfy.
fn quota_bytes_to_i64(bytes: u64) -> i64 {
    bytes.min(i64::MAX as u64) as i64
}

/// Inverse of `quota_bytes_to_i64`. A negative row (only reachable by hand-
/// editing the database) reads as `0`, not as a huge `u64` — the
/// conservative direction for a ceiling.
fn quota_bytes_from_i64(bytes: i64) -> u64 {
    bytes.max(0) as u64
}

fn config_from_row(row: &DbBusQuota) -> QuotaConfig {
    QuotaConfig {
        max_topics: row.max_topics,
        max_partitions: row.max_partitions,
        max_bytes_total: quota_bytes_from_i64(row.max_bytes_total),
        produce_msgs_per_sec: row.produce_msgs_per_sec,
        produce_bytes_per_sec: quota_bytes_from_i64(row.produce_bytes_per_sec),
        max_groups: row.max_groups,
    }
}

/// Writes `cfg` as `(instance_id, org_id)`'s durable quota. Called by the
/// admin path (`dispatch/bus.rs`'s `quota_set_v1`) BEFORE it applies `cfg`
/// in memory, and fatal to that request if it fails: a quota live on this
/// engine but missing from `bus_org_quotas` would silently revert to
/// `QuotaConfig::default()` at the next restart. Writing first means a
/// failed write leaves BOTH the table and the manager on the previous
/// value rather than only the table.
pub fn persist_org_quota(
    db: &DbPool,
    instance_id: &str,
    org_id: &str,
    cfg: QuotaConfig,
) -> Result<(), BusServiceError> {
    repository::bus_quota_set(
        db,
        &DbBusQuota {
            instance_id: instance_id.to_string(),
            org_id: org_id.to_string(),
            max_topics: cfg.max_topics,
            max_partitions: cfg.max_partitions,
            max_bytes_total: quota_bytes_to_i64(cfg.max_bytes_total),
            produce_msgs_per_sec: cfg.produce_msgs_per_sec,
            produce_bytes_per_sec: quota_bytes_to_i64(cfg.produce_bytes_per_sec),
            max_groups: cfg.max_groups,
            updated_at_ms: super::now_ms(),
        },
    )?;
    Ok(())
}

/// Replays every persisted quota for `instance_id` into `mgr`, returning how
/// many orgs were restored. Each row goes through `set_org_quota`, so a
/// restored org starts with full token buckets — the same state a freshly
/// applied `QuotaSet` leaves behind, and the only honest one available
/// after a restart (rate buckets are per-second windows, not durable state).
/// An org with no row keeps falling back to `QuotaConfig::default()`,
/// exactly as before this table existed.
pub fn load_persisted_quotas(
    db: &DbPool,
    instance_id: &str,
    mgr: &QuotaManager,
) -> Result<usize, BusServiceError> {
    let rows = repository::bus_quota_list_for_instance(db, instance_id)?;
    for row in &rows {
        mgr.set_org_quota(&row.org_id, config_from_row(row));
    }
    Ok(rows.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unconfigured_org_gets_the_practically_unlimited_default() {
        let mgr = QuotaManager::new();
        assert!(mgr.try_consume("org-1", 100, 1024).is_ok());
    }

    #[test]
    fn tight_msg_quota_is_enforced_and_reports_retry_after() {
        let mgr = QuotaManager::new();
        mgr.set_org_quota(
            "org-1",
            QuotaConfig {
                produce_msgs_per_sec: 10,
                produce_bytes_per_sec: 1024 * 1024,
                ..QuotaConfig::default()
            },
        );
        assert!(mgr.try_consume("org-1", 10, 100).is_ok());
        let err = mgr.try_consume("org-1", 1, 100).unwrap_err();
        match err {
            BusServiceError::QuotaExceeded { retry_after_ms } => assert!(retry_after_ms > 0),
            other => panic!("expected QuotaExceeded, got {other:?}"),
        }
    }

    #[test]
    fn tight_byte_quota_is_enforced_independently_of_msg_quota() {
        let mgr = QuotaManager::new();
        mgr.set_org_quota(
            "org-1",
            QuotaConfig {
                produce_msgs_per_sec: 1_000_000,
                produce_bytes_per_sec: 1024,
                ..QuotaConfig::default()
            },
        );
        assert!(mgr.try_consume("org-1", 1, 1024).is_ok());
        let err = mgr.try_consume("org-1", 1, 1).unwrap_err();
        assert!(matches!(err, BusServiceError::QuotaExceeded { .. }));
    }

    #[test]
    fn buckets_refill_over_time() {
        let mgr = QuotaManager::new();
        mgr.set_org_quota(
            "org-1",
            QuotaConfig {
                produce_msgs_per_sec: 1000,
                produce_bytes_per_sec: 1024 * 1024,
                ..QuotaConfig::default()
            },
        );
        // Drain + must-fail is two statements, not one atomic step: at
        // 1000 tokens/s a scheduler stall of ~1 ms between them (routine
        // under a loaded test host) refills a token and flips the second
        // call to Ok. Retry the pair so the assertion tests "an empty
        // bucket stays empty", not scheduler timing.
        let drain_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            assert!(mgr.try_consume("org-1", 1000, 10).is_ok());
            if mgr.try_consume("org-1", 1, 10).is_err() {
                break;
            }
            assert!(
                std::time::Instant::now() < drain_deadline,
                "drained bucket kept admitting requests for 5s"
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        // ~50 tokens refilled at 1000/s over 50ms; small requests succeed.
        assert!(mgr.try_consume("org-1", 10, 10).is_ok());
    }

    #[test]
    fn different_orgs_do_not_share_a_bucket() {
        let mgr = QuotaManager::new();
        mgr.set_org_quota(
            "org-1",
            QuotaConfig {
                produce_msgs_per_sec: 1,
                produce_bytes_per_sec: 1,
                ..QuotaConfig::default()
            },
        );
        assert!(mgr.try_consume("org-1", 1, 1).is_ok());
        assert!(mgr.try_consume("org-1", 1, 1).is_err());
        // org-2 has its own (default, practically unlimited) bucket.
        assert!(mgr.try_consume("org-2", 1, 1).is_ok());
    }

    // ---- amount > bucket capacity is a hard config error -----

    #[test]
    fn msg_amount_larger_than_rate_is_a_hard_error_not_quota_exceeded() {
        let mgr = QuotaManager::new();
        mgr.set_org_quota(
            "org-1",
            QuotaConfig {
                produce_msgs_per_sec: 10,
                produce_bytes_per_sec: 1024 * 1024,
                ..QuotaConfig::default()
            },
        );
        let err = mgr.try_consume("org-1", 11, 10).unwrap_err();
        match err {
            BusServiceError::QuotaRequestTooLarge {
                unit,
                amount,
                capacity,
            } => {
                assert_eq!(unit, "messages");
                assert_eq!(amount, 11);
                assert_eq!(capacity, 10);
            }
            other => panic!("expected QuotaRequestTooLarge, got {other:?}"),
        }
        // The bucket must be untouched by the rejected oversized request —
        // a full-capacity request right after it must still succeed.
        assert!(mgr.try_consume("org-1", 10, 10).is_ok());
    }

    #[test]
    fn byte_amount_larger_than_rate_is_a_hard_error_not_quota_exceeded() {
        let mgr = QuotaManager::new();
        mgr.set_org_quota(
            "org-1",
            QuotaConfig {
                produce_msgs_per_sec: 1_000_000,
                produce_bytes_per_sec: 1024,
                ..QuotaConfig::default()
            },
        );
        let err = mgr.try_consume("org-1", 1, 2048).unwrap_err();
        assert!(matches!(
            err,
            BusServiceError::QuotaRequestTooLarge {
                unit: "bytes",
                amount: 2048,
                capacity: 1024,
            }
        ));
    }

    #[test]
    fn an_unlimited_rate_never_reports_a_request_as_oversized() {
        // rate = 0 means "unlimited" (see `TokenBucket::try_consume`'s
        // doc) — no request should ever be rejected as oversized against it.
        let mgr = QuotaManager::new();
        mgr.set_org_quota(
            "org-1",
            QuotaConfig {
                produce_msgs_per_sec: 0,
                produce_bytes_per_sec: 0,
                ..QuotaConfig::default()
            },
        );
        assert!(mgr.try_consume("org-1", u32::MAX, u64::MAX).is_ok());
    }

    // ---- resource-limit getters (wired into `BusService::create_topic`) --

    #[test]
    fn resource_limit_getters_report_the_configured_quota() {
        let mgr = QuotaManager::new();
        mgr.set_org_quota(
            "org-1",
            QuotaConfig {
                max_topics: 3,
                max_partitions: 16,
                max_bytes_total: 42,
                ..QuotaConfig::default()
            },
        );
        assert_eq!(mgr.max_topics("org-1"), 3);
        assert_eq!(mgr.max_partitions("org-1"), 16);
        assert_eq!(mgr.max_bytes_total("org-1"), 42);
        // An org with no explicit quota gets PLAN §7.1's defaults.
        assert_eq!(mgr.max_topics("org-2"), QuotaConfig::default().max_topics);
    }

    #[test]
    fn rate_getters_report_the_configured_quota() {
        let mgr = QuotaManager::new();
        mgr.configure_org(
            "org-1",
            QuotaConfig {
                produce_msgs_per_sec: 55,
                produce_bytes_per_sec: 4096,
                ..QuotaConfig::default()
            },
        );
        assert_eq!(mgr.produce_msgs_per_sec("org-1"), 55);
        assert_eq!(mgr.produce_bytes_per_sec("org-1"), 4096);
    }

    #[test]
    fn max_groups_defaults_and_set_max_groups_leaves_the_rest_of_the_quota_untouched() {
        let mgr = QuotaManager::new();
        assert_eq!(mgr.max_groups("org-1"), DEFAULT_MAX_GROUPS);

        mgr.set_max_groups("org-1", 7);
        assert_eq!(mgr.max_groups("org-1"), 7);
        // `set_max_groups` is scoped: unrelated to the rate/topic/partition
        // quota.
        assert_eq!(mgr.max_topics("org-1"), QuotaConfig::default().max_topics);

        // A different org is unaffected.
        assert_eq!(mgr.max_groups("org-2"), DEFAULT_MAX_GROUPS);

        mgr.remove_org("org-1");
        assert_eq!(
            mgr.max_groups("org-1"),
            DEFAULT_MAX_GROUPS,
            "remove_org must also drop the max_groups override"
        );
    }

    /// `max_groups` is now a first-class `QuotaConfig` field (follow-up toru
    /// P, task 6): a full-replace `configure_org`/`set_org_quota` call DOES
    /// carry it and overwrites whatever `set_max_groups` had set before —
    /// this is the wire-visible behavior `QuotaSet` (dispatch layer) relies
    /// on to let an admin move every quota figure, including `max_groups`,
    /// in one request.
    #[test]
    fn configure_org_replaces_max_groups_too_since_it_is_now_part_of_quota_config() {
        let mgr = QuotaManager::new();
        mgr.set_max_groups("org-1", 7);
        assert_eq!(mgr.max_groups("org-1"), 7);

        mgr.configure_org(
            "org-1",
            QuotaConfig {
                max_groups: 42,
                ..QuotaConfig::default()
            },
        );
        assert_eq!(mgr.max_groups("org-1"), 42);

        // A full replace that does not explicitly set `max_groups` resets it
        // to the default, exactly like any other `QuotaConfig` field would.
        mgr.configure_org("org-1", QuotaConfig::default());
        assert_eq!(mgr.max_groups("org-1"), DEFAULT_MAX_GROUPS);
    }

    // ---- durability (`bus_org_quotas`) ---------------------------------

    const TEST_INSTANCE: &str = "tentabus-00000001";
    const OTHER_INSTANCE: &str = "tentabus-00000002";

    fn test_db() -> crate::db::DbPool {
        crate::db::init(std::path::Path::new(":memory:")).expect("test db")
    }

    /// The C4 gap: before `bus_org_quotas` existed, a `QuotaSet` lived only
    /// in `QuotaManager`'s `DashMap` and every configured ceiling silently
    /// reverted to `QuotaConfig::default()` on the next start. A restart is
    /// simulated the way it really happens — a brand-new `QuotaManager`,
    /// exactly what `BusService::new` constructs — reading back the same
    /// database.
    #[test]
    fn a_persisted_quota_survives_a_simulated_restart() {
        let db = test_db();
        let cfg = QuotaConfig {
            max_topics: 7,
            max_partitions: 9,
            max_bytes_total: 4096,
            produce_msgs_per_sec: 11,
            produce_bytes_per_sec: 2048,
            max_groups: 3,
        };
        let mgr = QuotaManager::new();
        mgr.set_org_quota("org-1", cfg);
        persist_org_quota(&db, TEST_INSTANCE, "org-1", cfg).unwrap();

        let restarted = QuotaManager::new();
        assert_eq!(
            restarted.max_topics("org-1"),
            QuotaConfig::default().max_topics,
            "a fresh manager must start from the defaults"
        );
        assert_eq!(
            load_persisted_quotas(&db, TEST_INSTANCE, &restarted).unwrap(),
            1
        );
        assert_eq!(restarted.max_topics("org-1"), 7);
        assert_eq!(restarted.max_partitions("org-1"), 9);
        assert_eq!(restarted.max_bytes_total("org-1"), 4096);
        assert_eq!(restarted.produce_msgs_per_sec("org-1"), 11);
        assert_eq!(restarted.produce_bytes_per_sec("org-1"), 2048);
        assert_eq!(restarted.max_groups("org-1"), 3);
        // The restored rate is really installed in the token bucket, not
        // just reported by the getter: a batch wider than the restored
        // capacity is rejected as oversized. Asserted through the
        // capacity check rather than by draining the bucket, so the test
        // depends on no timing at all (see `buckets_refill_over_time`'s
        // own note on how little a drained bucket can be trusted between
        // two statements).
        assert!(matches!(
            restarted.try_consume("org-1", 12, 10).unwrap_err(),
            BusServiceError::QuotaRequestTooLarge {
                unit: "messages",
                capacity: 11,
                ..
            }
        ));

        // plan-app-platform §1.4/W3: rows are instance-scoped — another
        // instance's engine must not inherit this one's ceilings.
        let other_instance = QuotaManager::new();
        assert_eq!(
            load_persisted_quotas(&db, OTHER_INSTANCE, &other_instance).unwrap(),
            0
        );
        assert_eq!(
            other_instance.max_topics("org-1"),
            QuotaConfig::default().max_topics
        );
    }

    /// A later `QuotaSet` for the same org replaces the row rather than
    /// accumulating a second one, and an org that was never configured is
    /// simply absent from the table.
    #[test]
    fn persisting_the_same_org_twice_replaces_the_stored_quota() {
        let db = test_db();
        persist_org_quota(
            &db,
            TEST_INSTANCE,
            "org-1",
            QuotaConfig {
                max_topics: 7,
                ..QuotaConfig::default()
            },
        )
        .unwrap();
        persist_org_quota(
            &db,
            TEST_INSTANCE,
            "org-1",
            QuotaConfig {
                max_topics: 12,
                ..QuotaConfig::default()
            },
        )
        .unwrap();

        let mgr = QuotaManager::new();
        assert_eq!(load_persisted_quotas(&db, TEST_INSTANCE, &mgr).unwrap(), 1);
        assert_eq!(mgr.max_topics("org-1"), 12);
        assert_eq!(
            mgr.max_topics("org-never-configured"),
            QuotaConfig::default().max_topics
        );
    }

    /// SQLite has no unsigned 64-bit integer: a byte ceiling above
    /// `i64::MAX` must clamp, never wrap into a negative row that would read
    /// back as a limit nothing can satisfy.
    #[test]
    fn a_byte_ceiling_above_i64_max_clamps_instead_of_wrapping() {
        let db = test_db();
        persist_org_quota(
            &db,
            TEST_INSTANCE,
            "org-1",
            QuotaConfig {
                max_bytes_total: u64::MAX,
                produce_bytes_per_sec: u64::MAX,
                ..QuotaConfig::default()
            },
        )
        .unwrap();

        let mgr = QuotaManager::new();
        load_persisted_quotas(&db, TEST_INSTANCE, &mgr).unwrap();
        assert_eq!(mgr.max_bytes_total("org-1"), i64::MAX as u64);
        assert_eq!(mgr.produce_bytes_per_sec("org-1"), i64::MAX as u64);
    }
}
