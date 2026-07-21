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
pub mod config;
pub mod ngram;
pub mod state;
pub mod stats;

pub use cascade::{CascadeComposer, DraftSegment};
pub use config::{ProposerKind, SpeculationCoordinator, SpeculativeConfig};
pub use ngram::NgramProposer;
pub use state::SpeculativeState;
pub use stats::{ProposerStats, ProposerStatsSnapshot};

#[derive(Debug, Clone, PartialEq)]
pub struct DraftNode {
    pub token_id: u32,
    pub parent: Option<usize>,
    pub depth: usize,
    pub source: ProposerKind,
    pub proposal_logprob: Option<f32>,
    pub conditional_confidence: Option<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DraftTree {
    nodes: Vec<DraftNode>,
}

impl DraftTree {
    pub fn new(nodes: Vec<DraftNode>) -> forge_types::Result<Self> {
        for (index, node) in nodes.iter().enumerate() {
            if let Some(parent) = node.parent {
                if parent >= index || node.depth != nodes[parent].depth + 1 {
                    return Err(forge_types::ForgeError::Scheduler(
                        "speculative draft nodes are not in valid topological order".into(),
                    ));
                }
            } else if node.depth != 0 {
                return Err(forge_types::ForgeError::Scheduler(
                    "speculative draft root must have depth zero".into(),
                ));
            }
            if let Some(logprob) = node.proposal_logprob {
                if !logprob.is_finite() || logprob > 0.0 {
                    return Err(forge_types::ForgeError::Scheduler(
                        "speculative proposal logprob must be finite and non-positive".into(),
                    ));
                }
            }
            if let Some(confidence) = node.conditional_confidence {
                if !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
                    return Err(forge_types::ForgeError::Scheduler(
                        "speculative conditional confidence must be finite and in [0, 1]".into(),
                    ));
                }
            }
        }
        Ok(Self { nodes })
    }

    pub fn linear(source: ProposerKind, tokens: Vec<u32>) -> Self {
        let nodes = tokens
            .into_iter()
            .enumerate()
            .map(|(depth, token_id)| DraftNode {
                token_id,
                parent: depth.checked_sub(1),
                depth,
                source,
                proposal_logprob: None,
                conditional_confidence: None,
            })
            .collect();
        Self { nodes }
    }

    pub fn nodes(&self) -> &[DraftNode] {
        &self.nodes
    }

    pub fn linear_tokens(&self) -> forge_types::Result<Vec<u32>> {
        for (index, node) in self.nodes.iter().enumerate() {
            if node.parent != index.checked_sub(1) || node.depth != index {
                return Err(forge_types::ForgeError::Unsupported(
                    "linear speculative verifier does not support branching drafts".into(),
                ));
            }
        }
        Ok(self.nodes.iter().map(|node| node.token_id).collect())
    }
}

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
    fn propose(&mut self, ctx: &SeqContext<'_>, budget: usize) -> forge_types::Result<DraftTree>;

    fn kind(&self) -> ProposerKind;

    /// Acceptance feedback for one earlier `propose` call (how many of the
    /// proposed tokens the verifier accepted). Lets learned proposers adapt.
    fn accept_feedback(&mut self, proposed: usize, accepted: usize);

    /// Feed one committed sequence token (prompt or verified output).
    /// Default no-op: proposers without an incremental index ignore it.
    fn observe(&mut self, _token: u32) {}
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
