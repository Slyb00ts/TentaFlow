// ===== File: vocab.rs — vocabulary byte table for token-level masking =====
// The grammar engine is pure (no tokenizer dependency); the host builds this
// table once from the tokenizer and shares it across requests. `token_bytes`
// gives the raw bytes a token contributes to the output stream, or `None` for
// special / control tokens that never carry grammar bytes.

use std::collections::HashSet;

pub struct GrammarVocab {
    /// Per-token output bytes; `None` = special/unusable token.
    token_bytes: Vec<Option<Vec<u8>>>,
    /// Tokens that terminate generation (allowed only in an accepting state).
    eos: HashSet<u32>,
    eos_list: Vec<u32>,
}

impl GrammarVocab {
    pub fn new(token_bytes: Vec<Option<Vec<u8>>>, eos_ids: &[u32]) -> Self {
        Self {
            token_bytes,
            eos: eos_ids.iter().copied().collect(),
            eos_list: eos_ids.to_vec(),
        }
    }

    pub fn len(&self) -> usize {
        self.token_bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.token_bytes.is_empty()
    }

    /// Bytes a token contributes, or `None` when it carries none (special
    /// token, or out of range).
    pub fn token_bytes(&self, id: u32) -> Option<&[u8]> {
        // EOS/special tokens never contribute grammar bytes.
        if self.eos.contains(&id) {
            return None;
        }
        self.token_bytes
            .get(id as usize)
            .and_then(|o| o.as_deref())
    }

    pub fn is_eos(&self, id: u32) -> bool {
        self.eos.contains(&id)
    }

    pub fn eos_ids(&self) -> &[u32] {
        &self.eos_list
    }
}
