// ===== File: tokenizer.rs — Tokenizer wrapper over HF `tokenizers` with a stable FORGE-facing API =====

use std::path::Path;

use forge_types::{ForgeError, Result};

/// Thin wrapper over `tokenizers::Tokenizer` that carries the special-token
/// ids the engine layer needs (bos/eos/pad) — tokenizer.json alone does not
/// declare them, so they are set separately from tokenizer_config / GGUF
/// metadata.
pub struct Tokenizer {
    inner: tokenizers::Tokenizer,
    bos_id: Option<u32>,
    eos_id: Option<u32>,
    pad_id: Option<u32>,
}

impl std::fmt::Debug for Tokenizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokenizer")
            .field("vocab_size", &self.vocab_size())
            .field("bos_id", &self.bos_id)
            .field("eos_id", &self.eos_id)
            .field("pad_id", &self.pad_id)
            .finish()
    }
}

impl Tokenizer {
    /// Load a HF `tokenizer.json`. Special-token ids start unset; the caller
    /// wires them from tokenizer_config.json / GGUF metadata via
    /// [`Tokenizer::set_special_ids`].
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let inner = tokenizers::Tokenizer::from_file(path.as_ref())
            .map_err(|e| ForgeError::Tokenizer(format!("failed to load tokenizer.json: {e}")))?;
        Ok(Self {
            inner,
            bos_id: None,
            eos_id: None,
            pad_id: None,
        })
    }

    pub(crate) fn from_inner(
        inner: tokenizers::Tokenizer,
        bos_id: Option<u32>,
        eos_id: Option<u32>,
        pad_id: Option<u32>,
    ) -> Self {
        Self {
            inner,
            bos_id,
            eos_id,
            pad_id,
        }
    }

    pub fn set_special_ids(
        &mut self,
        bos_id: Option<u32>,
        eos_id: Option<u32>,
        pad_id: Option<u32>,
    ) {
        self.bos_id = bos_id;
        self.eos_id = eos_id;
        self.pad_id = pad_id;
    }

    pub fn encode(&self, text: &str, add_special_tokens: bool) -> Result<Vec<u32>> {
        let encoding = self
            .inner
            .encode_fast(text, add_special_tokens)
            .map_err(|e| ForgeError::Tokenizer(format!("encode failed: {e}")))?;
        Ok(encoding.get_ids().to_vec())
    }

    pub fn decode(&self, ids: &[u32], skip_special_tokens: bool) -> Result<String> {
        self.inner
            .decode(ids, skip_special_tokens)
            .map_err(|e| ForgeError::Tokenizer(format!("decode failed: {e}")))
    }

    /// Vocabulary size including added tokens.
    pub fn vocab_size(&self) -> usize {
        self.inner.get_vocab_size(true)
    }

    /// Raw vocabulary piece for a token id (undecoded — byte-level pieces keep
    /// their `Ġ`/`Ċ` alphabet, SPM pieces keep `▁`). Useful for logit-bias
    /// mapping and debugging.
    pub fn token_to_piece(&self, id: u32) -> Option<String> {
        self.inner.id_to_token(id)
    }

    pub fn token_to_id(&self, piece: &str) -> Option<u32> {
        self.inner.token_to_id(piece)
    }

    pub fn bos_id(&self) -> Option<u32> {
        self.bos_id
    }

    pub fn eos_id(&self) -> Option<u32> {
        self.eos_id
    }

    pub fn pad_id(&self) -> Option<u32> {
        self.pad_id
    }

    /// Escape hatch to the underlying HF tokenizer for engine-layer features
    /// not covered by this wrapper (offsets, batch encode).
    pub fn inner(&self) -> &tokenizers::Tokenizer {
        &self.inner
    }
}
