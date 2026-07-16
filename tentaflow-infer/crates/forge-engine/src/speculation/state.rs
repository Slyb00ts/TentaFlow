// ===== File: speculation/state.rs — per-sequence speculation state =====
//! One `SpeculativeState` per running sequence: it owns the token history,
//! the cascade composer with per-proposer stats, and the draft/commit cycle
//! the engine drives on the linear verification path.

use super::cascade::{CascadeComposer, DraftSegment};
use super::stats::ProposerStatsSnapshot;

pub struct SpeculativeState {
    history: Vec<u32>,
    composer: CascadeComposer,
    /// Draft returned by the last `draft` call plus its segment attribution;
    /// consumed by the matching `commit`.
    pending: Option<(Vec<u32>, Vec<DraftSegment>)>,
}

impl SpeculativeState {
    pub fn new(composer: CascadeComposer) -> Self {
        Self {
            history: Vec::new(),
            composer,
            pending: None,
        }
    }

    /// Feed one committed token (prompt token, engine-sampled token, or the
    /// bonus/correction token after a partial draft acceptance).
    pub fn observe(&mut self, token: u32) {
        self.history.push(token);
        self.composer.observe(token);
    }

    pub fn observe_all(&mut self, tokens: &[u32]) {
        for &t in tokens {
            self.observe(t);
        }
    }

    /// Full committed history (prompt + generated).
    pub fn history(&self) -> &[u32] {
        &self.history
    }

    /// Build one draft of at most `budget` tokens. Each draft must be
    /// resolved by `commit` before the next `draft` call.
    pub fn draft(&mut self, budget: usize) -> Vec<u32> {
        let (draft, segments) = self.composer.compose(&self.history, budget);
        self.pending = Some((draft.clone(), segments));
        draft
    }

    /// Resolve the last draft: `drafted` must be the tokens `draft` returned
    /// and `n_accepted` the verified prefix length (e.g. from
    /// `verify_greedy`). Updates per-proposer stats and feeds the accepted
    /// tokens back into the history/index.
    pub fn commit(&mut self, drafted: &[u32], n_accepted: usize) {
        debug_assert!(n_accepted <= drafted.len());
        if let Some((pending_draft, segments)) = self.pending.take() {
            debug_assert_eq!(pending_draft, drafted);
            self.composer.commit_feedback(&segments, n_accepted);
        }
        self.observe_all(&drafted[..n_accepted.min(drafted.len())]);
    }

    pub fn stats(&self) -> Vec<ProposerStatsSnapshot> {
        self.composer.stats()
    }
}
