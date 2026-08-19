// ===== File: services/cancel_registry.rs — cooperative cancellation for in-process runs =====
//
// One registry type for "a long-running job this process owns can be asked to
// stop". Three copies of the same map had grown independently — Project Studio
// ingest (whose comment admitted it was "a module-local mirror of
// dispatch/benchmark.rs"), Project Studio auto-runs, and the benchmark handler.
//
// The contract is deliberately narrow. A flag is cooperative: the worker decides
// where it is safe to stop and no work is interrupted mid-write. Registration is
// per PROCESS, so `signal` returning false means "this node does not own that
// run", never "that run does not exist" — the caller decides whether to forward
// the request to another node.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use parking_lot::RwLock;

/// Registry of cancellation flags keyed by a caller-chosen run id.
///
/// The lazy `OnceLock` lives INSIDE the type so `new()` stays const: a call site
/// declares a plain `static` and no longer repeats the init dance every copy
/// used to carry.
pub struct CancelRegistry {
    inner: OnceLock<RwLock<HashMap<String, Arc<AtomicBool>>>>,
}

impl CancelRegistry {
    pub const fn new() -> Self {
        Self {
            inner: OnceLock::new(),
        }
    }

    fn map(&self) -> &RwLock<HashMap<String, Arc<AtomicBool>>> {
        self.inner.get_or_init(Default::default)
    }

    /// Registers `id` and returns its flag. Re-registering the same id replaces
    /// the entry, so a restarted run never inherits an already-tripped flag.
    pub fn register(&self, id: &str) -> Arc<AtomicBool> {
        let token = Arc::new(AtomicBool::new(false));
        self.map().write().insert(id.to_string(), token.clone());
        token
    }

    /// Drops the entry. The worker's `Arc` keeps the flag alive until it exits,
    /// so unregistering while it runs is safe — it only stops NEW cancel
    /// requests from reaching a run that is already finishing.
    pub fn unregister(&self, id: &str) {
        self.map().write().remove(id);
    }

    /// Trips the flag. `false` = this process does not (or no longer) owns `id`.
    pub fn signal(&self, id: &str) -> bool {
        match self.map().read().get(id) {
            Some(token) => {
                token.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    /// Whether this process currently owns a run under `id`.
    pub fn is_registered(&self, id: &str) -> bool {
        self.map().read().contains_key(id)
    }
}

impl Default for CancelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_reaches_only_registered_ids() {
        let reg = CancelRegistry::new();
        let token = reg.register("run-1");
        assert!(!token.load(Ordering::Relaxed));
        assert!(!reg.signal("run-2"), "obcy id nie moze zwrocic true");
        assert!(reg.signal("run-1"));
        assert!(token.load(Ordering::Relaxed));
    }

    #[test]
    fn reregistering_clears_a_tripped_flag() {
        let reg = CancelRegistry::new();
        let first = reg.register("run-1");
        reg.signal("run-1");
        assert!(first.load(Ordering::Relaxed));
        // Ponowny start pod tym samym id nie moze wystartowac juz anulowany.
        let second = reg.register("run-1");
        assert!(!second.load(Ordering::Relaxed));
    }

    #[test]
    fn unregister_stops_further_signals() {
        let reg = CancelRegistry::new();
        let token = reg.register("run-1");
        reg.unregister("run-1");
        assert!(!reg.is_registered("run-1"));
        assert!(!reg.signal("run-1"));
        assert!(!token.load(Ordering::Relaxed));
    }
}
