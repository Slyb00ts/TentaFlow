// ===== File: matcher.rs — per-sequence grammar state + token-mask logic =====
// Holds one sequence's live parse state (the nondeterministic stack set plus
// any incomplete trailing UTF-8 bytes) and turns it into a vocab mask: a token
// is allowed iff appending its bytes keeps the grammar satisfiable. Byte
// fragments that leave an incomplete UTF-8 scalar are allowed only when some
// accepted codepoint can still complete them, so the constraint stays sound
// for multi-byte / byte-fallback tokens. Allowed-token sets are cached per
// parse state (the perf-critical bit — decode revisits few distinct states).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::grammar::{Grammar, Stack};
use crate::vocab::GrammarVocab;

/// Canonical key for a parse state (stack set is kept sorted+deduped).
type StateKey = (Vec<Stack>, Vec<u8>);

/// Shared, cheap-to-clone compiled grammar bound to a vocabulary. Several
/// sequences of one request share the automaton, the vocab and the
/// per-state allowed-token cache.
#[derive(Clone)]
pub struct GrammarProgram {
    grammar: Arc<Grammar>,
    vocab: Arc<GrammarVocab>,
    cache: Arc<Mutex<HashMap<StateKey, Arc<Vec<bool>>>>>,
}

impl GrammarProgram {
    pub fn new(grammar: Arc<Grammar>, vocab: Arc<GrammarVocab>) -> Self {
        Self {
            grammar,
            vocab,
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// A fresh matcher positioned at the grammar start.
    pub fn matcher(&self) -> GrammarMatcher {
        GrammarMatcher {
            program: self.clone(),
            stacks: self.grammar.init_stacks(),
            partial: Vec::new(),
        }
    }
}

/// One sequence's mutable grammar state.
pub struct GrammarMatcher {
    program: GrammarProgram,
    stacks: Vec<Stack>,
    partial: Vec<u8>,
}

impl GrammarMatcher {
    /// Whether the grammar is in an accepting state (EOS is permitted) with no
    /// incomplete byte tail.
    pub fn is_complete(&self) -> bool {
        self.partial.is_empty() && Grammar::is_complete(&self.stacks)
    }

    /// Rewrite `logits` in place: every token the grammar forbids is set to
    /// `-inf`, so both greedy and stochastic sampling can only pick a
    /// conforming token. If nothing is allowed (a dead end), EOS tokens are
    /// left intact so generation can terminate rather than sample garbage.
    pub fn apply_mask(&self, logits: &mut [f32]) {
        let mask = self.allowed_mask();
        let mut any = false;
        for (i, l) in logits.iter_mut().enumerate() {
            match mask.get(i) {
                Some(true) => any = true,
                _ => *l = f32::NEG_INFINITY,
            }
        }
        if !any {
            // Dead end: permit termination via any EOS token.
            for &id in self.program.vocab.eos_ids() {
                if let Some(l) = logits.get_mut(id as usize) {
                    *l = 0.0;
                }
            }
        }
    }

    /// Advance the state by an accepted token's bytes (called after the token
    /// is chosen). EOS / special tokens (no grammar bytes) leave the state
    /// unchanged.
    pub fn accept_token(&mut self, id: u32) {
        let vocab = self.program.vocab.clone();
        let Some(bytes) = vocab.token_bytes(id) else {
            return;
        };
        if bytes.is_empty() {
            return;
        }
        if let Some((stacks, partial)) = feed_bytes(
            &self.program.grammar,
            self.stacks.clone(),
            self.partial.clone(),
            bytes,
        ) {
            self.stacks = stacks;
            self.partial = partial;
        }
    }

    /// The per-vocab allowed mask for the current state, cached.
    fn allowed_mask(&self) -> Arc<Vec<bool>> {
        let key: StateKey = (self.stacks.clone(), self.partial.clone());
        if let Some(m) = self.program.cache.lock().expect("cache lock").get(&key) {
            return m.clone();
        }
        let mask = Arc::new(self.compute_mask());
        self.program
            .cache
            .lock()
            .expect("cache lock")
            .insert(key, mask.clone());
        mask
    }

    fn compute_mask(&self) -> Vec<bool> {
        let vocab = &self.program.vocab;
        let n = vocab.len();
        let mut mask = vec![false; n];
        let complete = self.is_complete();
        // First-byte prefilter: a token whose first byte the grammar rejects
        // outright can never be allowed, so only tokens starting with an
        // accepted byte need the full (cloning) feed. For structured grammars
        // this prunes the vast majority of the vocab scan.
        let mut first_ok = [false; 256];
        for (b, ok) in first_ok.iter_mut().enumerate() {
            *ok = feed_bytes(
                &self.program.grammar,
                self.stacks.clone(),
                self.partial.clone(),
                &[b as u8],
            )
            .is_some();
        }
        for (id, slot) in mask.iter_mut().enumerate().take(n) {
            let idu = id as u32;
            if vocab.is_eos(idu) {
                *slot = complete;
                continue;
            }
            let Some(bytes) = vocab.token_bytes(idu) else {
                continue;
            };
            let Some(&first) = bytes.first() else {
                continue;
            };
            if !first_ok[first as usize] {
                continue;
            }
            *slot = feed_bytes(
                &self.program.grammar,
                self.stacks.clone(),
                self.partial.clone(),
                bytes,
            )
            .is_some();
        }
        mask
    }
}

/// Feed a byte string through a cloned state, returning the new
/// `(stacks, partial)` or `None` if any byte is rejected.
fn feed_bytes(
    grammar: &Grammar,
    mut stacks: Vec<Stack>,
    mut partial: Vec<u8>,
    bytes: &[u8],
) -> Option<(Vec<Stack>, Vec<u8>)> {
    for &b in bytes {
        partial.push(b);
        match decode_partial(&partial) {
            PartialState::Complete(cp) => {
                stacks = grammar.accept(&stacks, cp);
                if stacks.is_empty() {
                    return None;
                }
                partial.clear();
            }
            PartialState::Incomplete => {
                // A valid but unfinished scalar: only sound if some accepted
                // codepoint can still complete this prefix.
                if !grammar.any_prefix_accepts(&stacks, &partial) {
                    return None;
                }
            }
            PartialState::Invalid => return None,
        }
    }
    Some((stacks, partial))
}

enum PartialState {
    Complete(u32),
    Incomplete,
    Invalid,
}

/// Interpret `partial` as the start of one UTF-8 scalar.
fn decode_partial(partial: &[u8]) -> PartialState {
    match std::str::from_utf8(partial) {
        Ok(s) => match s.chars().next() {
            Some(c) if c.len_utf8() == partial.len() => PartialState::Complete(c as u32),
            // More than one scalar cannot happen (we clear after each), and a
            // shorter-than-buffer scalar means trailing junk.
            _ => PartialState::Invalid,
        },
        Err(e) => {
            if e.error_len().is_none() && e.valid_up_to() == 0 {
                // The whole buffer is a valid incomplete lead sequence.
                if partial.len() < 4 {
                    PartialState::Incomplete
                } else {
                    PartialState::Invalid
                }
            } else {
                PartialState::Invalid
            }
        }
    }
}
