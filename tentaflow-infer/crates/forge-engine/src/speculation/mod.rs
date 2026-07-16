// ===== File: speculation/mod.rs — composable speculative-decoding framework (SPEC §6, PLAN chunk 8) =====
//! CPU-side speculation: the `Proposer` trait, an n-gram self-drafting
//! proposer, cascade composition (SPEC 6.2) with per-proposer acceptance
//! stats and adaptive disable, and the linear (greedy) verification helper.
//!
//! Drafts are flat token runs: a linear draft is the degenerate single-branch
//! tree, so when GPU tree verification (tree-attention mask, spec-sampling)
//! lands, the same `Proposer` implementations feed the tree builder and only
//! the verification side changes.

pub mod cascade;
pub mod ngram;
pub mod state;
pub mod stats;

pub use cascade::{CascadeComposer, DraftSegment};
pub use ngram::NgramProposer;
pub use state::SpeculativeState;
pub use stats::{ProposerStats, ProposerStatsSnapshot};

/// Read-only view of one sequence's full token history (prompt + generated).
/// During cascade composition the view also covers draft tokens appended by
/// earlier proposers, so later proposers extend rather than duplicate.
pub struct SeqContext<'a> {
    tokens: &'a [u32],
}

impl<'a> SeqContext<'a> {
    pub fn new(tokens: &'a [u32]) -> Self {
        Self { tokens }
    }

    pub fn tokens(&self) -> &[u32] {
        self.tokens
    }
}

/// A draft-token source. Implementations: `NgramProposer` (this chunk);
/// draft-model / MTP / EAGLE proposers plug into the same slot later.
pub trait Proposer: Send {
    /// Propose up to `budget` continuation tokens for the sequence in `ctx`.
    /// Must be cheap (microseconds) — it runs on the decode hot path.
    fn propose(&mut self, ctx: &SeqContext<'_>, budget: usize) -> Vec<u32>;

    /// Acceptance feedback for one earlier `propose` call (how many of the
    /// proposed tokens the verifier accepted). Lets learned proposers adapt.
    fn accept_feedback(&mut self, proposed: usize, accepted: usize);

    /// Feed one committed sequence token (prompt or verified output).
    /// Default no-op: proposers without an incremental index ignore it.
    fn observe(&mut self, _token: u32) {}

    fn name(&self) -> &str;
}

/// Linear-path verification: the engine steps the target model over the draft
/// and greedy-samples one token per position; position `i`'s sample must equal
/// `draft[i]` for the draft token to count. Returns the accepted prefix length.
pub fn verify_greedy(draft: &[u32], sampled: &[u32]) -> usize {
    draft
        .iter()
        .zip(sampled)
        .take_while(|(d, s)| d == s)
        .count()
}

#[cfg(test)]
mod tests;
