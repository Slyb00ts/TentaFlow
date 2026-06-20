// =============================================================================
// File: services/runtime/alias_metrics.rs
// Licznik fallbackow aliasow dla JEDNEGO wspolnego punktu liczenia. Executor
// (`ModelRuntimeExecutor`) inkrementuje `alias_fallback_total{alias}` ilekroc
// realnie wybral kandydata o pozycji > 0 w lancuchu — niezaleznie od tego, czy
// request przyszedl z `/v1`, flow czy addona (wszystkie trzy failuja przez ten
// sam executor). Repo nie ma frameworka metryk z etykietami (router trzyma
// tylko stale pola atomowe), wiec utrzymujemy dedykowana mape lock-free.
// =============================================================================

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use dashmap::DashMap;

static ALIAS_FALLBACK_TOTAL: OnceLock<DashMap<String, AtomicU64>> = OnceLock::new();

fn alias_fallback_total() -> &'static DashMap<String, AtomicU64> {
    ALIAS_FALLBACK_TOTAL.get_or_init(DashMap::new)
}

/// Inkrementuje `alias_fallback_total{alias}` o 1. Wolane z pętli failoveru
/// executora dokładnie wtedy, gdy zwycięski kandydat ma pozycję > 0.
pub fn record_alias_fallback(alias: &str) {
    alias_fallback_total()
        .entry(alias.to_string())
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

/// Snapshot licznika fallbackow per alias (dla dashboardu/diagnostyki).
pub fn alias_fallback_snapshot() -> Vec<(String, u64)> {
    alias_fallback_total()
        .iter()
        .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
        .collect()
}

#[cfg(test)]
pub fn alias_fallback_count(alias: &str) -> u64 {
    alias_fallback_total()
        .get(alias)
        .map(|c| c.load(Ordering::Relaxed))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_metric_counts() {
        record_alias_fallback("alias-metrics-unit-test");
        record_alias_fallback("alias-metrics-unit-test");
        assert!(alias_fallback_count("alias-metrics-unit-test") >= 2);
        let snap = alias_fallback_snapshot();
        assert!(snap.iter().any(|(a, _)| a == "alias-metrics-unit-test"));
    }
}
