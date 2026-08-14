// ===== File: gguf_vocab.rs — the tokenizer a GGUF carries inside itself =====
//
// A GGUF file embeds its own vocabulary, merges and template. Reading them is a
// format question, and the answer is the same on every machine — so this lives
// beside the tokenizer it feeds rather than inside one backend's engine, where
// it spent long enough that the Metal path could not reach it.

use crate::GgufVocab;
use forge_formats::Gguf;
use forge_types::{ForgeError, Result};

/// Extract the embedded tokenizer definition from GGUF metadata.
pub fn gguf_vocab(gguf: &Gguf) -> Result<GgufVocab> {
    let model = gguf
        .get_str("tokenizer.ggml.model")
        .ok_or_else(|| ForgeError::Tokenizer("gguf: missing tokenizer.ggml.model".into()))?
        .to_string();
    let pre = gguf
        .get_str("tokenizer.ggml.pre")
        .unwrap_or("default")
        .to_string();

    let tokens: Vec<String> = gguf
        .get_array("tokenizer.ggml.tokens")
        .ok_or_else(|| ForgeError::Tokenizer("gguf: missing tokenizer.ggml.tokens".into()))?
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect();

    let token_types: Vec<i32> = gguf
        .get_array("tokenizer.ggml.token_type")
        .map(|a| {
            a.iter()
                .map(|v| v.as_u64().map(|u| u as i32).unwrap_or(1))
                .collect()
        })
        .unwrap_or_else(|| vec![1; tokens.len()]);

    let scores: Vec<f32> = gguf
        .get_array("tokenizer.ggml.scores")
        .map(|a| a.iter().map(|v| v.as_f32().unwrap_or(0.0)).collect())
        .unwrap_or_default();

    let merges: Vec<String> = gguf
        .get_array("tokenizer.ggml.merges")
        .map(|a| {
            a.iter()
                .map(|v| v.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();

    Ok(GgufVocab {
        model,
        pre,
        tokens,
        token_types,
        scores,
        merges,
        bos_id: gguf.get_u32("tokenizer.ggml.bos_token_id"),
        eos_id: gguf.get_u32("tokenizer.ggml.eos_token_id"),
        pad_id: gguf.get_u32("tokenizer.ggml.padding_token_id"),
        unk_id: gguf.get_u32("tokenizer.ggml.unknown_token_id"),
        add_bos: gguf
            .get_bool("tokenizer.ggml.add_bos_token")
            .unwrap_or(false),
        add_eos: gguf
            .get_bool("tokenizer.ggml.add_eos_token")
            .unwrap_or(false),
    })
}
