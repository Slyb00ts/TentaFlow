// ============ File: oauth_refresh_guard.rs - per-account mutex for OAuth refresh ============
//
// Deduplicates concurrent refresh_token calls on the same user_oauth_accounts row.
// Provider servers typically invalidate the old refresh_token as soon as a refresh
// succeeds; if two callers race, the second call lands on an already-invalidated
// refresh_token and we lose the account. The guard serializes refreshes by
// account_id so only one HTTP call is in flight per row.
//
// Keyed by account_id (i64) - independent across accounts, so no global bottleneck.
// Lock held only during the HTTP round-trip; second caller then re-reads the
// freshly updated DB row and typically returns without calling the provider.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex as SyncMutex;

/// Refresh guard: one mutex per account_id, created lazily and reused.
///
/// Entries are never removed - the map grows to the set of accounts that have
/// refreshed at least once, bounded in practice by live user count. Removing
/// entries would race with a caller that holds an Arc to a stale mutex.
///
/// Uses a sync mutex because WASM host functions run synchronously; the HTTP
/// refresh call is also blocking (reqwest::blocking). Async callers can still
/// use this by `spawn_blocking` if they need to.
pub struct OAuthRefreshGuard {
    entries: SyncMutex<HashMap<i64, Arc<SyncMutex<()>>>>,
}

impl OAuthRefreshGuard {
    pub fn new() -> Self {
        Self {
            entries: SyncMutex::new(HashMap::new()),
        }
    }

    /// Returns the mutex for this account, creating it on first use.
    pub fn mutex_for(&self, account_id: i64) -> Arc<SyncMutex<()>> {
        let mut map = self.entries.lock();
        map.entry(account_id)
            .or_insert_with(|| Arc::new(SyncMutex::new(())))
            .clone()
    }

    /// Current entry count (for tests / metrics).
    pub fn len(&self) -> usize {
        self.entries.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.lock().is_empty()
    }
}

impl Default for OAuthRefreshGuard {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn mutex_for_same_account_returns_same_arc() {
        let g = OAuthRefreshGuard::new();
        let a = g.mutex_for(42);
        let b = g.mutex_for(42);
        assert!(
            Arc::ptr_eq(&a, &b),
            "same account must yield the same mutex"
        );
        assert_eq!(g.len(), 1);
    }

    #[test]
    fn mutex_for_different_accounts_returns_independent_arcs() {
        let g = OAuthRefreshGuard::new();
        let a = g.mutex_for(1);
        let b = g.mutex_for(2);
        assert!(!Arc::ptr_eq(&a, &b));
        assert_eq!(g.len(), 2);
    }

    #[test]
    fn concurrent_refresh_serializes_by_account() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let g = Arc::new(OAuthRefreshGuard::new());
        let counter = Arc::new(AtomicUsize::new(0));
        let max_observed = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let g = g.clone();
            let counter = counter.clone();
            let max_observed = max_observed.clone();
            handles.push(std::thread::spawn(move || {
                let m = g.mutex_for(7);
                let _guard = m.lock();
                let c = counter.fetch_add(1, Ordering::SeqCst) + 1;
                let mut prev = max_observed.load(Ordering::SeqCst);
                while c > prev {
                    match max_observed.compare_exchange(prev, c, Ordering::SeqCst, Ordering::SeqCst)
                    {
                        Ok(_) => break,
                        Err(v) => prev = v,
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
                counter.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(
            max_observed.load(Ordering::SeqCst),
            1,
            "at most one holder at a time for a given account"
        );
    }
}
