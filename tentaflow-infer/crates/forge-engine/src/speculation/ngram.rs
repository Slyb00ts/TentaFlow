// ===== File: speculation/ngram.rs — self-drafting n-gram proposer (hash-map index) =====
//! llama.cpp-style n-gram lookup over the sequence's OWN history: maps of
//! 4/3/2/1-gram → recent follower positions, longest gram wins, candidate
//! occurrences are ranked by how far the match extends backwards. Observe is
//! O(1) amortized (4 bounded map inserts), propose is a constant number of
//! map probes plus bounded comparisons — microseconds either way.

use std::collections::HashMap;

use super::{Proposer, SeqContext};

const MAX_GRAM: usize = 4;
/// Bounds both memory and per-propose candidate scanning; newest occurrences
/// are kept because recent text predicts the continuation best.
const MAX_POSITIONS_PER_GRAM: usize = 8;
/// Cap on the backwards tie-break comparison so propose stays O(1).
const MAX_BACKWARD_EXTEND: usize = 32;

/// Fixed-width key: first `n` slots hold the gram, the rest stay zero. Grams
/// of different lengths live in different maps, so zero-padding cannot alias.
type GramKey = [u32; MAX_GRAM];

fn gram_key(gram: &[u32]) -> GramKey {
    let mut key = [0u32; MAX_GRAM];
    key[..gram.len()].copy_from_slice(gram);
    key
}

pub struct NgramProposer {
    history: Vec<u32>,
    /// maps[n-1]: n-gram → positions of the token that FOLLOWED that gram.
    /// Only grams with a known follower are indexed, so every hit yields at
    /// least one draft token.
    maps: [HashMap<GramKey, Vec<u32>>; MAX_GRAM],
}

impl NgramProposer {
    pub fn new() -> Self {
        Self {
            history: Vec::new(),
            maps: Default::default(),
        }
    }

    /// Tokens fed through `observe` so far (prompt + committed output).
    pub fn observed(&self) -> &[u32] {
        &self.history
    }

    /// Length of the backwards context match preceding an indexed occurrence,
    /// used to pick the candidate whose surrounding context best matches ours.
    fn backward_match(&self, gram_start: usize, ctx: &[u32], ctx_gram_start: usize) -> usize {
        let max = gram_start.min(ctx_gram_start).min(MAX_BACKWARD_EXTEND);
        let mut k = 0;
        while k < max && self.history[gram_start - 1 - k] == ctx[ctx_gram_start - 1 - k] {
            k += 1;
        }
        k
    }
}

impl Default for NgramProposer {
    fn default() -> Self {
        Self::new()
    }
}

impl Proposer for NgramProposer {
    fn propose(&mut self, ctx: &SeqContext<'_>, budget: usize) -> Vec<u32> {
        let ctx_tokens = ctx.tokens();
        if budget == 0 || ctx_tokens.is_empty() {
            return Vec::new();
        }
        for n in (1..=MAX_GRAM).rev() {
            if ctx_tokens.len() < n {
                continue;
            }
            let key = gram_key(&ctx_tokens[ctx_tokens.len() - n..]);
            let Some(positions) = self.maps[n - 1].get(&key) else {
                continue;
            };
            // Positions are stored oldest→newest; `>=` keeps the newest among
            // equally good backwards matches (recency bias).
            let mut best: Option<(usize, usize)> = None;
            for &p in positions {
                let p = p as usize;
                let m = self.backward_match(p - n, ctx_tokens, ctx_tokens.len() - n);
                if best.is_none_or(|(bm, _)| m >= bm) {
                    best = Some((m, p));
                }
            }
            let Some((_, p)) = best else { continue };
            let end = (p + budget).min(self.history.len());
            return self.history[p..end].to_vec();
        }
        Vec::new()
    }

    fn accept_feedback(&mut self, _proposed: usize, _accepted: usize) {
        // Lookup is purely positional; acceptance carries no signal for it.
        // Adaptive disable lives in the composer's per-proposer stats.
    }

    fn observe(&mut self, token: u32) {
        let pos = self.history.len();
        self.history.push(token);
        for n in 1..=MAX_GRAM {
            if pos < n {
                break;
            }
            let key = gram_key(&self.history[pos - n..pos]);
            let list = self.maps[n - 1].entry(key).or_default();
            if list.len() == MAX_POSITIONS_PER_GRAM {
                list.remove(0);
            }
            list.push(pos as u32);
        }
    }

    fn name(&self) -> &str {
        "ngram"
    }
}
