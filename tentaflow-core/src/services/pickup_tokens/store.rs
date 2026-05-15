// =============================================================================
// File: services/pickup_tokens/store.rs — inflight DashMap with TTL cleanup
// =============================================================================
//
// Holds every issued PickupToken so we can enforce one-shot semantics
// (`AtomicBool::compare_exchange` on the consumed bit) and reject tokens that
// vanished from the table (server restart / cleanup purge). Cleanup runs every
// 60 s and removes anything older than `2 × TTL` so the table never grows
// without bound even if a service never picks up.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;

use super::token::TokenPayload;

/// In-store record. `consumed` is the one-shot bit; `issued_at` is for the
/// background cleanup sweep.
pub struct IssuedToken {
    pub payload: TokenPayload,
    pub issued_at: Instant,
    pub consumed: AtomicBool,
}

impl IssuedToken {
    pub fn new(payload: TokenPayload) -> Self {
        Self {
            payload,
            issued_at: Instant::now(),
            consumed: AtomicBool::new(false),
        }
    }

    /// Atomic one-shot consume — first caller wins, subsequent callers see
    /// `false` from `compare_exchange` and the verifier maps that to
    /// `AlreadyConsumed`.
    pub fn try_consume(&self) -> bool {
        self.consumed
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
}

/// Container indexed by the wire string. Wrapped in `Arc` so cleanup task
/// + main thread share state cheaply.
pub type InflightMap = Arc<DashMap<String, IssuedToken>>;

/// Sweep entries older than `retain_for`. Called from a background tokio task.
pub fn sweep(inflight: &InflightMap, retain_for: Duration) {
    let now = Instant::now();
    inflight.retain(|_k, t| now.duration_since(t.issued_at) < retain_for);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_payload() -> TokenPayload {
        TokenPayload {
            raw_ref: "frame_x".into(),
            service_id: "svc".into(),
            request_id: "req".into(),
            expiry_unix_ms: 0,
            one_shot: true,
        }
    }

    #[test]
    fn try_consume_is_one_shot() {
        let t = IssuedToken::new(mk_payload());
        assert!(t.try_consume());
        assert!(!t.try_consume(), "second consume must fail");
    }

    #[test]
    fn sweep_removes_old_entries() {
        let map: InflightMap = Arc::new(DashMap::new());
        let mut t = IssuedToken::new(mk_payload());
        // Force `issued_at` into the past so the sweep classifies the entry
        // as expired without sleeping.
        t.issued_at = Instant::now()
            .checked_sub(Duration::from_secs(120))
            .expect("clock past genesis");
        map.insert("k".into(), t);
        assert_eq!(map.len(), 1);
        sweep(&map, Duration::from_secs(60));
        assert_eq!(map.len(), 0, "old entry must be purged");
    }

    #[test]
    fn sweep_keeps_fresh_entries() {
        let map: InflightMap = Arc::new(DashMap::new());
        map.insert("k".into(), IssuedToken::new(mk_payload()));
        sweep(&map, Duration::from_secs(60));
        assert_eq!(map.len(), 1);
    }
}
