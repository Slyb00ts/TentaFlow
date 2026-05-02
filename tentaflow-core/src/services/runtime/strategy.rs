// =============================================================================
// File: services/runtime/strategy.rs
// Ranking helpers used after the alias resolver narrows targets to the
// compatible set. Strategy itself comes from `services::catalog::Strategy`
// (`FirstAvailable` and `RoundRobin`); this file provides the state needed
// to drive `RoundRobin` across requests and the rank fn the executor
// calls. Adding a new strategy means extending `Strategy` first; the
// `rank()` match is exhaustive so the compiler will flag the gap.
// =============================================================================

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::services::catalog::Strategy;
use crate::services::runtime::target::ResolvedExecutionTarget;

/// Per-alias counter used by `RoundRobin`. A single shared instance per
/// alias name keeps sibling requests rotating; `Default` starts the
/// rotation at zero so the first request after process start hits the
/// primary target.
#[derive(Debug, Default)]
pub struct StrategyState {
    counter: AtomicUsize,
}

impl StrategyState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bump the counter and return the previous value. Wrap-around is
    /// handled at rank time by modulo against the candidate count, so the
    /// counter itself is allowed to grow unbounded across the process
    /// lifetime — overflow on `usize` is millennia away in practice.
    fn fetch_and_advance(&self) -> usize {
        self.counter.fetch_add(1, Ordering::Relaxed)
    }
}

/// Reorder the resolver's candidate list according to `strategy`. The
/// resolver always emits candidates in declaration order (primary first,
/// fallbacks in `model_aliases.fallback_targets` order); this function
/// is the only place that ever permutes them.
///
/// The returned vec is a fresh allocation — callers walk it in order and
/// stop on the first successful dispatch (the executor handles fallback
/// failures separately).
pub fn rank(
    candidates: &[ResolvedExecutionTarget],
    strategy: Strategy,
    state: &StrategyState,
) -> Vec<ResolvedExecutionTarget> {
    if candidates.is_empty() {
        return Vec::new();
    }
    match strategy {
        Strategy::FirstAvailable => candidates.to_vec(),
        Strategy::RoundRobin => {
            let pivot = state.fetch_and_advance() % candidates.len();
            // Rotate so the pivot becomes the first candidate; the rest
            // keep their relative order so fallback intent (declared
            // before vs after primary) is respected.
            let mut out = Vec::with_capacity(candidates.len());
            out.extend_from_slice(&candidates[pivot..]);
            out.extend_from_slice(&candidates[..pivot]);
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::handles_cache::BackendHandle;

    fn target(name: &str) -> ResolvedExecutionTarget {
        ResolvedExecutionTarget::Local {
            model_name: name.to_string(),
            handle: BackendHandle::Embedded {
                model_name: name.to_string(),
                node_id: "n".into(),
            },
        }
    }

    #[test]
    fn first_available_preserves_order() {
        let cands = vec![target("a"), target("b"), target("c")];
        let state = StrategyState::new();
        let ranked = rank(&cands, Strategy::FirstAvailable, &state);
        assert_eq!(ranked.iter().map(|t| t.requested_model()).collect::<Vec<_>>(), vec!["a", "b", "c"]);
    }

    #[test]
    fn round_robin_rotates_starting_from_primary() {
        let cands = vec![target("a"), target("b"), target("c")];
        let state = StrategyState::new();
        let collect = |strat_state: &StrategyState| -> Vec<String> {
            rank(&cands, Strategy::RoundRobin, strat_state)
                .into_iter()
                .map(|t| t.requested_model().to_string())
                .collect()
        };
        assert_eq!(collect(&state), vec!["a", "b", "c"]); // pivot 0
        assert_eq!(collect(&state), vec!["b", "c", "a"]); // pivot 1
        assert_eq!(collect(&state), vec!["c", "a", "b"]); // pivot 2
        assert_eq!(collect(&state), vec!["a", "b", "c"]); // wraps to pivot 0
    }

    #[test]
    fn rank_on_empty_candidates_returns_empty() {
        let state = StrategyState::new();
        let ranked = rank(&[], Strategy::FirstAvailable, &state);
        assert!(ranked.is_empty());
    }
}
