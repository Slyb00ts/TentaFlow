// ===== File: speculation/cascade.rs — additive cascade of proposers (SPEC 6.2) =====
//! Builds ONE draft from an ordered proposer list: the first proposer drafts
//! the core, each later proposer sees history + draft-so-far as its context
//! and can only APPEND — additive by construction, never redundant. Segment
//! attribution records which proposer produced which run so acceptance
//! feedback lands on the right stats.

use super::stats::{ProposerStats, ProposerStatsSnapshot};
use super::{Proposer, SeqContext};

/// One contiguous run of draft tokens attributed to a single proposer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftSegment {
    pub proposer_idx: usize,
    pub len: usize,
}

struct Slot {
    proposer: Box<dyn Proposer>,
    stats: ProposerStats,
}

pub struct CascadeComposer {
    slots: Vec<Slot>,
}

impl CascadeComposer {
    pub fn new(proposers: Vec<Box<dyn Proposer>>) -> Self {
        Self {
            slots: proposers
                .into_iter()
                .map(|proposer| Slot {
                    proposer,
                    stats: ProposerStats::new(),
                })
                .collect(),
        }
    }

    /// Compose one draft over `ctx_tokens` under a total token budget.
    /// Sleeping proposers are skipped but keep receiving `observe`, so their
    /// index is current when they wake.
    pub fn compose(&mut self, ctx_tokens: &[u32], budget: usize) -> (Vec<u32>, Vec<DraftSegment>) {
        let mut extended = ctx_tokens.to_vec();
        let base = extended.len();
        let mut segments = Vec::new();
        for (idx, slot) in self.slots.iter_mut().enumerate() {
            let used = extended.len() - base;
            if used >= budget {
                break;
            }
            if slot.stats.is_sleeping() {
                continue;
            }
            let remaining = budget - used;
            let mut tokens = slot
                .proposer
                .propose(&SeqContext::new(&extended), remaining);
            tokens.truncate(remaining);
            if tokens.is_empty() {
                continue;
            }
            segments.push(DraftSegment {
                proposer_idx: idx,
                len: tokens.len(),
            });
            extended.extend_from_slice(&tokens);
        }
        (extended.split_off(base), segments)
    }

    /// Attribute the accepted prefix across segments in draft order: a
    /// segment is charged with what the verifier accepted from ITS run, and
    /// once acceptance stops, every later segment scores zero.
    pub fn commit_feedback(&mut self, segments: &[DraftSegment], n_accepted: usize) {
        let mut remaining = n_accepted;
        for seg in segments {
            let accepted = remaining.min(seg.len);
            remaining -= accepted;
            let slot = &mut self.slots[seg.proposer_idx];
            slot.proposer.accept_feedback(seg.len, accepted);
            slot.stats.record(seg.len, accepted);
        }
    }

    /// Feed one committed token to every proposer (sleeping ones included)
    /// and advance sleep countdowns.
    pub fn observe(&mut self, token: u32) {
        for slot in &mut self.slots {
            slot.proposer.observe(token);
            slot.stats.note_tokens(1);
        }
    }

    pub fn stats(&self) -> Vec<ProposerStatsSnapshot> {
        self.slots
            .iter()
            .map(|slot| slot.stats.snapshot(slot.proposer.name()))
            .collect()
    }
}
