// ===== File: gguf.rs — rebuild a HF tokenizer from GGUF-embedded vocab metadata =====
//
// forge-tokenize deliberately does not depend on forge-formats: the engine
// layer extracts `tokenizer.ggml.*` metadata from a GGUF file into the plain
// `GgufVocab` struct below and hands it over here. This keeps the tokenizer
// crate reusable for tokenizer.json models and unit-testable without files.

use forge_types::{ForgeError, Result};
use tokenizers::decoders::byte_fallback::ByteFallback;
use tokenizers::decoders::fuse::Fuse;
use tokenizers::decoders::strip::Strip;
use tokenizers::models::bpe::BPE;
use tokenizers::normalizers::{Prepend, Replace};
use tokenizers::pre_tokenizers::byte_level::ByteLevel;
use tokenizers::pre_tokenizers::split::{Split, SplitPattern};
use tokenizers::processors::template::TemplateProcessing;
use tokenizers::{AddedToken, NormalizerWrapper, PreTokenizerWrapper, SplitDelimiterBehavior};

use crate::tokenizer::Tokenizer;

// GGUF `tokenizer.ggml.token_type` values (llama.cpp `llama_token_type`).
const TOKEN_TYPE_UNKNOWN: i32 = 2;
const TOKEN_TYPE_CONTROL: i32 = 3;
const TOKEN_TYPE_USER_DEFINED: i32 = 4;

/// Plain-data tokenizer description extracted from GGUF metadata
/// (`tokenizer.ggml.*` keys). All vectors are index-aligned: index == token id.
#[derive(Debug, Clone, Default)]
pub struct GgufVocab {
    /// `tokenizer.ggml.model`: "gpt2" (byte-level BPE) or "llama" (SPM).
    pub model: String,
    /// `tokenizer.ggml.pre`: pre-tokenizer flavor ("qwen2", "llama-bpe",
    /// "gpt-2", "default", ...). Only meaningful for the gpt2 family.
    pub pre: String,
    /// `tokenizer.ggml.tokens`.
    pub tokens: Vec<String>,
    /// `tokenizer.ggml.token_type` (1=normal, 2=unknown, 3=control,
    /// 4=user-defined, 5=unused, 6=byte).
    pub token_types: Vec<i32>,
    /// `tokenizer.ggml.scores` — required for the llama/SPM family (used to
    /// reconstruct BPE merge priorities), empty for gpt2.
    pub scores: Vec<f32>,
    /// `tokenizer.ggml.merges` as "left right" pairs — gpt2 family only.
    pub merges: Vec<String>,
    /// `tokenizer.ggml.bos_token_id` / `eos_token_id` / `padding_token_id` /
    /// `unknown_token_id`.
    pub bos_id: Option<u32>,
    pub eos_id: Option<u32>,
    pub pad_id: Option<u32>,
    pub unk_id: Option<u32>,
    /// `tokenizer.ggml.add_bos_token`.
    pub add_bos: bool,
}

// GPT-2 pre-tokenization regex (used when `pre` is unrecognized or "gpt-2").
const GPT2_PRE_RE: &str =
    r"'s|'t|'re|'ve|'m|'ll|'d| ?\p{L}+| ?\p{N}+| ?[^\s\p{L}\p{N}]+|\s+(?!\S)|\s+";
// Qwen2/Qwen3 pre-tokenization regex (matches the upstream tokenizer.json Split).
const QWEN2_PRE_RE: &str = r"(?i:'s|'t|'re|'ve|'m|'ll|'d)|[^\r\n\p{L}\p{N}]?\p{L}+|\p{N}| ?[^\s\p{L}\p{N}]+[\r\n]*|\s*[\r\n]+|\s+(?!\S)|\s+";
// Llama-3 pre-tokenization regex (digits grouped up to 3).
const LLAMA3_PRE_RE: &str = r"(?i:'s|'t|'re|'ve|'m|'ll|'d)|[^\r\n\p{L}\p{N}]?\p{L}+|\p{N}{1,3}| ?[^\s\p{L}\p{N}]+[\r\n]*|\s*[\r\n]+|\s+(?!\S)|\s+";

impl Tokenizer {
    /// Build an in-memory HF tokenizer equivalent to the GGUF-embedded one.
    pub fn from_gguf_vocab(vocab: &GgufVocab) -> Result<Self> {
        if vocab.tokens.is_empty() {
            return Err(ForgeError::Tokenizer("GGUF vocab has no tokens".into()));
        }
        if vocab.token_types.len() != vocab.tokens.len() {
            return Err(ForgeError::Tokenizer(format!(
                "GGUF vocab token_types length {} != tokens length {}",
                vocab.token_types.len(),
                vocab.tokens.len()
            )));
        }
        let mut inner = match vocab.model.as_str() {
            "gpt2" => build_gpt2(vocab)?,
            "llama" => build_spm(vocab)?,
            other => {
                return Err(ForgeError::Tokenizer(format!(
                    "unsupported GGUF tokenizer model {other:?} (expected \"gpt2\" or \"llama\")"
                )))
            }
        };
        add_special_tokens(&mut inner, vocab)?;
        if vocab.add_bos {
            attach_bos_processor(&mut inner, vocab)?;
        }
        Ok(Tokenizer::from_inner(
            inner,
            vocab.bos_id,
            vocab.eos_id,
            vocab.pad_id,
        ))
    }
}

fn id_to_vocab_map(vocab: &GgufVocab) -> tokenizers::models::bpe::Vocab {
    vocab
        .tokens
        .iter()
        .enumerate()
        .map(|(i, t)| (t.clone(), i as u32))
        .collect()
}

/// gpt2 family: byte-level BPE with explicit merges from GGUF metadata.
fn build_gpt2(vocab: &GgufVocab) -> Result<tokenizers::Tokenizer> {
    let vocab_map = id_to_vocab_map(vocab);
    let merges = vocab
        .merges
        .iter()
        .map(|m| {
            m.split_once(' ')
                .map(|(l, r)| (l.to_string(), r.to_string()))
                .ok_or_else(|| ForgeError::Tokenizer(format!("malformed GGUF merge entry {m:?}")))
        })
        .collect::<Result<Vec<_>>>()?;

    // Newer BPE vocabs (qwen2, llama3) rely on "ignore_merges": a word that is
    // already a full vocab entry is emitted directly without running merges.
    let ignore_merges = matches!(vocab.pre.as_str(), "qwen2" | "llama-bpe" | "llama-v3");
    let model = BPE::builder()
        .vocab_and_merges(vocab_map, merges)
        .ignore_merges(ignore_merges)
        .byte_fallback(false)
        .fuse_unk(false)
        .build()
        .map_err(|e| ForgeError::Tokenizer(format!("failed to build BPE model: {e}")))?;

    let mut tokenizer = tokenizers::Tokenizer::new(model);

    // The GPT-2 ByteLevel pre-tokenizer embeds its own split regex; qwen2 and
    // llama3 use a different regex, so those run an explicit Split first and
    // ByteLevel only does the byte→unicode alphabet mapping.
    let pre: PreTokenizerWrapper = match vocab.pre.as_str() {
        "qwen2" => split_then_byte_level(QWEN2_PRE_RE)?,
        "llama-bpe" | "llama-v3" => split_then_byte_level(LLAMA3_PRE_RE)?,
        "gpt-2" | "gpt2" | "default" | "" => split_then_byte_level(GPT2_PRE_RE)?,
        // Unknown pre flavors degrade to the GPT-2 regex rather than failing:
        // llama.cpp does the same and the byte-level alphabet keeps decoding
        // lossless even if word boundaries differ slightly.
        _ => split_then_byte_level(GPT2_PRE_RE)?,
    };
    tokenizer.with_pre_tokenizer(Some(pre));
    tokenizer.with_decoder(Some(
        ByteLevel::default()
            .add_prefix_space(false)
            .trim_offsets(false)
            .use_regex(false),
    ));
    Ok(tokenizer)
}

fn split_then_byte_level(pattern: &str) -> Result<PreTokenizerWrapper> {
    let split = Split::new(
        SplitPattern::Regex(pattern.to_string()),
        SplitDelimiterBehavior::Isolated,
        false,
    )
    .map_err(|e| ForgeError::Tokenizer(format!("invalid pre-tokenizer regex: {e}")))?;
    let byte_level = ByteLevel::default()
        .add_prefix_space(false)
        .trim_offsets(false)
        .use_regex(false);
    Ok(
        tokenizers::pre_tokenizers::sequence::Sequence::new(vec![split.into(), byte_level.into()])
            .into(),
    )
}

/// llama family: GGUF stores SPM pieces + scores but no merges. Rebuild BPE
/// merges the same way transformers' GGUFLlamaConverter does: for every piece,
/// every two-way split whose halves are both vocab entries becomes a merge
/// candidate ranked by the piece score (higher score = earlier merge).
fn build_spm(vocab: &GgufVocab) -> Result<tokenizers::Tokenizer> {
    if vocab.scores.len() != vocab.tokens.len() {
        return Err(ForgeError::Tokenizer(format!(
            "GGUF llama vocab requires scores (got {} scores for {} tokens)",
            vocab.scores.len(),
            vocab.tokens.len()
        )));
    }
    let vocab_map = id_to_vocab_map(vocab);

    let mut scored_merges: Vec<(String, String, f32)> = Vec::new();
    for (token, &score) in vocab.tokens.iter().zip(&vocab.scores) {
        let mut local: Vec<(&str, &str)> = Vec::new();
        for (split_at, _) in token.char_indices().skip(1) {
            let (left, right) = token.split_at(split_at);
            if vocab_map.contains_key(left) && vocab_map.contains_key(right) {
                local.push((left, right));
            }
        }
        local.sort_by_key(|(l, r)| (vocab_map[*l], vocab_map[*r]));
        scored_merges.extend(
            local
                .into_iter()
                .map(|(l, r)| (l.to_string(), r.to_string(), score)),
        );
    }
    // Stable sort keeps the vocab-order tie-break from the loop above.
    scored_merges.sort_by(|a, b| b.2.total_cmp(&a.2));
    let merges: Vec<(String, String)> = scored_merges.into_iter().map(|(l, r, _)| (l, r)).collect();

    let unk_token = vocab
        .unk_id
        .and_then(|id| vocab.tokens.get(id as usize).cloned());
    let mut builder = BPE::builder()
        .vocab_and_merges(vocab_map, merges)
        .fuse_unk(true)
        .byte_fallback(true);
    if let Some(unk) = unk_token {
        builder = builder.unk_token(unk);
    }
    let model = builder
        .build()
        .map_err(|e| ForgeError::Tokenizer(format!("failed to build SPM-BPE model: {e}")))?;

    let mut tokenizer = tokenizers::Tokenizer::new(model);
    let prepend: NormalizerWrapper = Prepend::new("▁".to_string()).into();
    let replace: NormalizerWrapper = Replace::new(" ", "▁")
        .map_err(|e| ForgeError::Tokenizer(format!("failed to build normalizer: {e}")))?
        .into();
    tokenizer
        .with_normalizer(Some(tokenizers::normalizers::Sequence::new(vec![
            prepend, replace,
        ])))
        .map_err(|e| ForgeError::Tokenizer(format!("failed to set normalizer: {e}")))?;
    let decoder_replace = tokenizers::decoders::DecoderWrapper::from(
        Replace::new("▁", " ")
            .map_err(|e| ForgeError::Tokenizer(format!("failed to build decoder: {e}")))?,
    );
    tokenizer.with_decoder(Some(tokenizers::decoders::sequence::Sequence::new(vec![
        decoder_replace,
        ByteFallback::new().into(),
        Fuse::new().into(),
        Strip::new(' ', 1, 0).into(),
    ])));
    Ok(tokenizer)
}

fn add_special_tokens(tokenizer: &mut tokenizers::Tokenizer, vocab: &GgufVocab) -> Result<()> {
    let mut special = Vec::new();
    let mut user_defined = Vec::new();
    for (token, &ty) in vocab.tokens.iter().zip(&vocab.token_types) {
        match ty {
            TOKEN_TYPE_CONTROL | TOKEN_TYPE_UNKNOWN => {
                special.push(AddedToken::from(token.clone(), true).normalized(false));
            }
            TOKEN_TYPE_USER_DEFINED => {
                user_defined.push(AddedToken::from(token.clone(), false).normalized(false));
            }
            _ => {}
        }
    }
    // Tokens already present in the model vocab keep their original ids.
    if !special.is_empty() {
        tokenizer
            .add_special_tokens(special)
            .map_err(|e| ForgeError::Tokenizer(format!("failed to add special tokens: {e}")))?;
    }
    if !user_defined.is_empty() {
        tokenizer
            .add_tokens(user_defined)
            .map_err(|e| ForgeError::Tokenizer(format!("failed to add user tokens: {e}")))?;
    }
    Ok(())
}

fn attach_bos_processor(tokenizer: &mut tokenizers::Tokenizer, vocab: &GgufVocab) -> Result<()> {
    let bos_id = vocab.bos_id.ok_or_else(|| {
        ForgeError::Tokenizer("GGUF vocab has add_bos=true but no bos_token_id".into())
    })?;
    let bos_token = vocab.tokens.get(bos_id as usize).cloned().ok_or_else(|| {
        ForgeError::Tokenizer(format!("GGUF bos_token_id {bos_id} out of vocab range"))
    })?;
    let processor = TemplateProcessing::builder()
        .try_single(format!("{bos_token} $A"))
        .map_err(|e| ForgeError::Tokenizer(format!("failed to build bos template: {e}")))?
        .try_pair(format!("{bos_token} $A {bos_token} $B"))
        .map_err(|e| ForgeError::Tokenizer(format!("failed to build bos pair template: {e}")))?
        .special_tokens(vec![(bos_token, bos_id)])
        .build()
        .map_err(|e| ForgeError::Tokenizer(format!("failed to build bos processor: {e}")))?;
    tokenizer.with_post_processor(Some(processor));
    Ok(())
}
