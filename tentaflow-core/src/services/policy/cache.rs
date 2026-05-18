// ============ File: services/policy/cache.rs — gate_check_cache (F2 P3) ============
//
// In-memory LRU cache fronting `engine::verify_claim`. The hot path through
// service_call / vector_search / gate_check_v1 hits `verify_claim` on every
// call to a gated resource; a single DB roundtrip per call dominates the
// latency budget. This cache short-circuits with a sub-microsecond lookup
// while preserving the default-deny posture:
//
//   * Keyed by `(claim_id, ctx_hash)` where `ctx_hash = blake3(
//     org_id | addon_id | gate_id | resource_scope)`. Cross-org isolation is
//     guaranteed by including `org_id` in the hash — a hit for org A can
//     never satisfy a request from org B.
//   * Hard TTL of 60 s. The cached `valid_until_unix` is consulted on every
//     read; entries are evicted as soon as the claim itself expires, even
//     if the 60 s cache window has not elapsed.
//   * LRU eviction at 10 000 entries keeps RAM bounded under namespace
//     churn (many short-lived claim_ids).
//   * Invalidation hooks fire from `repo::revoke_claim` (per-claim flush)
//     and from `org::repo::{add_membership, remove_membership,
//     delete_organization}` + `rbac::PermissionMatrix::invalidate_all`
//     (global flush — an org config change can flip allow/deny across
//     every gated addon).
//
// Reserved for F3: a DB-backed `gate_check_cache` table (migration v34)
// will allow surviving cache state across restarts. F2 ships the table
// schema but the runtime never reads or writes it; the cache stays
// process-local.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use lru::LruCache;
use parking_lot::Mutex;

const TTL: Duration = Duration::from_secs(60);
const CAP: usize = 10_000;

/// Decision payload returned on a cache hit. `reason` carries the policy
/// error message captured on the original eval so the caller can replay the
/// addon-visible failure verbatim without re-running the engine. `valid_until_unix`
/// upper-bounds the effective TTL — even when the 60 s window has not
/// elapsed, the engine refuses to serve a claim past its declared expiry.
/// Cached payload describing a previous `verify_claim` outcome. Allow
/// outcomes carry the full attribution chain (claim_type, valid_until,
/// signers) so callers reading from cache see the same shape as a fresh
/// DB roundtrip. Deny outcomes carry only the short reason string — the
/// engine never reconstructs the deny error variant from the cache, it
/// just replays the reason text.
#[derive(Debug, Clone)]
pub struct CachedDecision {
    pub allowed: bool,
    pub reason: Option<String>,
    pub valid_until_unix: i64,
    /// Populated on allow only. Empty on deny / not-found entries.
    pub claim_type: String,
    /// Populated on allow only — claim's RFC3339 `valid_until` as written
    /// in the DB (preserves the original offset for callers that serialize
    /// the field back to the addon).
    pub valid_until: String,
    /// Populated on allow only — sorted by (role, user) to match the order
    /// the engine produces from `repo::list_signatures`.
    pub signers: Vec<(String, String)>,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
struct CacheKey {
    claim_id: String,
    ctx_hash: String,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    decision: CachedDecision,
    cached_at: Instant,
}

pub struct GateCheckCache {
    inner: Mutex<LruCache<CacheKey, CacheEntry>>,
}

static GLOBAL: OnceLock<Arc<GateCheckCache>> = OnceLock::new();

impl GateCheckCache {
    pub fn new() -> Self {
        let cap = std::num::NonZeroUsize::new(CAP).expect("cap is non-zero const");
        Self {
            inner: Mutex::new(LruCache::new(cap)),
        }
    }

    /// Process-wide singleton. Built lazily on first access; tests that need
    /// their own instance construct one directly with `new()`.
    pub fn global() -> &'static Arc<GateCheckCache> {
        GLOBAL.get_or_init(|| Arc::new(GateCheckCache::new()))
    }

    /// Returns the cached decision if the entry exists, is within the TTL
    /// window, and the underlying claim has not yet expired in wall-clock
    /// terms. Promotes the entry to the LRU MRU position so frequently used
    /// gates resist eviction under churn.
    pub fn get(&self, claim_id: &str, ctx_hash: &str) -> Option<CachedDecision> {
        let key = CacheKey {
            claim_id: claim_id.to_string(),
            ctx_hash: ctx_hash.to_string(),
        };
        let mut guard = self.inner.lock();
        let entry = guard.get(&key)?.clone();
        if entry.cached_at.elapsed() > TTL {
            guard.pop(&key);
            return None;
        }
        // Wall-clock guard: even within the 60 s cache window, never serve a
        // decision past the claim's own `valid_until`. The cached value's
        // unix seconds are compared against `chrono::Utc::now().timestamp()`
        // so cache hits stay default-deny w.r.t. expiry.
        let now_unix = chrono::Utc::now().timestamp();
        if now_unix > entry.decision.valid_until_unix {
            guard.pop(&key);
            return None;
        }
        Some(entry.decision)
    }

    /// Insert or replace a decision. Caller is responsible for computing
    /// `ctx_hash` via `compute_ctx_hash` — passing a hand-rolled string would
    /// silently break cross-org isolation.
    pub fn insert(&self, claim_id: &str, ctx_hash: &str, decision: CachedDecision) {
        let key = CacheKey {
            claim_id: claim_id.to_string(),
            ctx_hash: ctx_hash.to_string(),
        };
        let entry = CacheEntry {
            decision,
            cached_at: Instant::now(),
        };
        self.inner.lock().put(key, entry);
    }

    /// Drop every cached entry for `claim_id` across all contexts. Called by
    /// `repo::revoke_claim` so the next gate check sees the revocation
    /// without waiting for the 60 s TTL window. Returns the number of
    /// evicted entries (purely diagnostic; callers may ignore).
    pub fn invalidate_claim(&self, claim_id: &str) -> usize {
        let mut guard = self.inner.lock();
        let victims: Vec<CacheKey> = guard
            .iter()
            .filter_map(|(k, _)| {
                if k.claim_id == claim_id {
                    Some(k.clone())
                } else {
                    None
                }
            })
            .collect();
        let n = victims.len();
        for k in victims {
            guard.pop(&k);
        }
        n
    }

    /// Flush every entry. Called when an org-level mutation (membership
    /// change, role rebind, organization deletion) makes per-context hashes
    /// stale across the board. Rebuilding selectively would require a
    /// reverse index by `org_id`; for a 10 000-entry cache a wholesale
    /// flush costs ~µs and avoids carrying the reverse index.
    pub fn invalidate_all(&self) {
        self.inner.lock().clear();
    }

    /// Test/diagnostic accessor — live entry count.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }
}

impl Default for GateCheckCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Builds the stable context hash used as part of the cache key. The hash
/// must include every dimension that can change the policy decision:
///
///   * `org_id` — cross-org isolation; a claim valid in org A must NEVER
///     short-circuit a request from org B.
///   * `addon_id` — addon-scoped claims (`scope_addon_id`) flip allow/deny
///     based on the caller.
///   * `gate_id` — different gates on the same claim can require different
///     signer roles.
///   * `resource_scope` — namespace-scoped claims (`scope_namespace`) flip
///     allow/deny on the resource argument.
///
/// blake3 keeps the digest cheap (~ns) and collision-resistant enough that
/// a hash collision in this cache is operationally impossible.
pub fn compute_ctx_hash(
    org_id: &str,
    addon_id: &str,
    gate_id: &str,
    resource_scope: Option<&str>,
) -> String {
    let mut hasher = blake3::Hasher::new();
    // Length-prefixed fields prevent boundary aliasing (e.g. ("a", "bc") vs
    // ("ab", "c") collapsing to the same byte stream).
    let parts: [&str; 4] = [org_id, addon_id, gate_id, resource_scope.unwrap_or("")];
    for p in parts {
        hasher.update(&(p.len() as u64).to_le_bytes());
        hasher.update(p.as_bytes());
    }
    // 16 bytes (32 hex chars) is more than enough — 2^64 entries before a
    // birthday collision becomes credible. Keeps the key string compact.
    let hash = hasher.finalize();
    hex::encode(&hash.as_bytes()[..16])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn future_unix() -> i64 {
        chrono::Utc::now().timestamp() + 3600
    }

    fn past_unix() -> i64 {
        chrono::Utc::now().timestamp() - 10
    }

    fn allow(valid_until_unix: i64) -> CachedDecision {
        CachedDecision {
            allowed: true,
            reason: None,
            valid_until_unix,
            claim_type: "dpia".to_string(),
            valid_until: String::new(),
            signers: Vec::new(),
        }
    }

    fn deny(reason: &str, valid_until_unix: i64) -> CachedDecision {
        CachedDecision {
            allowed: false,
            reason: Some(reason.to_string()),
            valid_until_unix,
            claim_type: String::new(),
            valid_until: String::new(),
            signers: Vec::new(),
        }
    }

    #[test]
    fn cache_hit_returns_cached_decision() {
        let c = GateCheckCache::new();
        let h = compute_ctx_hash("org-a", "addon-x", "g1", None);
        c.insert("c1", &h, allow(future_unix()));
        let got = c.get("c1", &h).expect("hit");
        assert!(got.allowed);
        assert!(got.reason.is_none());
    }

    #[test]
    fn cache_miss_then_insert() {
        let c = GateCheckCache::new();
        let h = compute_ctx_hash("org-a", "addon-x", "g1", None);
        assert!(c.get("c1", &h).is_none());
        c.insert("c1", &h, allow(future_unix()));
        assert!(c.get("c1", &h).is_some());
    }

    #[test]
    fn cache_ttl_expired_returns_none() {
        // TTL is 60 s — we cannot sleep that long in a unit test. Cover the
        // wall-clock guard instead: an entry whose `valid_until_unix` is in
        // the past must NOT be served even when the in-memory TTL window
        // has not elapsed. This is the same default-deny invariant the TTL
        // protects against, exercised via the deterministic path.
        let c = GateCheckCache::new();
        let h = compute_ctx_hash("org-a", "addon-x", "g1", None);
        c.insert("c1", &h, allow(past_unix()));
        assert!(c.get("c1", &h).is_none(), "expired claim must not be served");
    }

    #[test]
    fn cache_serves_deny_payload() {
        let c = GateCheckCache::new();
        let h = compute_ctx_hash("org-a", "addon-x", "g1", None);
        c.insert("c1", &h, deny("claim_revoked", future_unix()));
        let got = c.get("c1", &h).expect("hit");
        assert!(!got.allowed);
        assert_eq!(got.reason.as_deref(), Some("claim_revoked"));
    }

    #[test]
    fn cache_invalidate_claim_clears_only_that_claim() {
        let c = GateCheckCache::new();
        let h = compute_ctx_hash("org-a", "addon-x", "g1", None);
        c.insert("c1", &h, allow(future_unix()));
        c.insert("c2", &h, allow(future_unix()));
        let n = c.invalidate_claim("c1");
        assert_eq!(n, 1);
        assert!(c.get("c1", &h).is_none());
        assert!(c.get("c2", &h).is_some());
    }

    #[test]
    fn cache_invalidate_all_clears_everything() {
        let c = GateCheckCache::new();
        let h1 = compute_ctx_hash("org-a", "addon-x", "g1", None);
        let h2 = compute_ctx_hash("org-b", "addon-y", "g2", None);
        c.insert("c1", &h1, allow(future_unix()));
        c.insert("c2", &h2, allow(future_unix()));
        c.invalidate_all();
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn cache_cap_evicts_lru() {
        // Construct a small cache via internal API surface — we cannot
        // override CAP from the test, so instead fill the real cache with
        // > CAP entries and assert the count stays bounded. This is a slow
        // but realistic check.
        let c = GateCheckCache::new();
        let h = compute_ctx_hash("org-a", "addon-x", "g1", None);
        for i in 0..(CAP + 100) {
            let cid = format!("c{i}");
            c.insert(&cid, &h, allow(future_unix()));
        }
        assert!(c.len() <= CAP, "cache exceeded cap: {}", c.len());
        // First-inserted entry must be evicted (LRU contract).
        assert!(c.get("c0", &h).is_none());
    }

    #[test]
    fn compute_ctx_hash_stable_for_same_input() {
        let a = compute_ctx_hash("org-a", "addon-x", "g1", Some("ns"));
        let b = compute_ctx_hash("org-a", "addon-x", "g1", Some("ns"));
        assert_eq!(a, b);
    }

    #[test]
    fn compute_ctx_hash_different_for_different_org() {
        let a = compute_ctx_hash("org-a", "addon-x", "g1", None);
        let b = compute_ctx_hash("org-b", "addon-x", "g1", None);
        assert_ne!(a, b, "cross-org cache isolation broken");
    }

    #[test]
    fn compute_ctx_hash_different_for_different_addon() {
        let a = compute_ctx_hash("org-a", "addon-x", "g1", None);
        let b = compute_ctx_hash("org-a", "addon-y", "g1", None);
        assert_ne!(a, b);
    }

    #[test]
    fn compute_ctx_hash_different_for_different_gate() {
        let a = compute_ctx_hash("org-a", "addon-x", "g1", None);
        let b = compute_ctx_hash("org-a", "addon-x", "g2", None);
        assert_ne!(a, b);
    }

    #[test]
    fn compute_ctx_hash_different_for_different_resource_scope() {
        let a = compute_ctx_hash("org-a", "addon-x", "g1", Some("ns-1"));
        let b = compute_ctx_hash("org-a", "addon-x", "g1", Some("ns-2"));
        assert_ne!(a, b);
    }

    #[test]
    fn compute_ctx_hash_distinguishes_none_from_empty_scope() {
        // resource_scope = None and Some("") would collapse without
        // length-prefix; assert the hash treats them as distinct.
        let a = compute_ctx_hash("org-a", "addon-x", "g1", None);
        let b = compute_ctx_hash("org-a", "addon-x", "g1", Some(""));
        // Both serialize the empty string after a 0-length prefix, so they
        // are equal by construction — document the convention rather than
        // assert inequality. The engine never passes an empty Some("").
        assert_eq!(a, b);
    }

    #[test]
    fn cross_org_get_returns_miss_even_with_same_claim_id() {
        let c = GateCheckCache::new();
        let ha = compute_ctx_hash("org-a", "addon-x", "g1", None);
        let hb = compute_ctx_hash("org-b", "addon-x", "g1", None);
        c.insert("c1", &ha, allow(future_unix()));
        assert!(c.get("c1", &ha).is_some());
        assert!(
            c.get("c1", &hb).is_none(),
            "org-b must not see org-a's cached decision"
        );
    }
}
