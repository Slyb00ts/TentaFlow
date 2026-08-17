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
    /// `tokenizer.ggml.add_eos_token`.
    pub add_eos: bool,
}

// Pre-tokenization split regexes, ported byte-exactly from llama.cpp
// `llm_tokenizer_bpe` (src/llama-vocab.cpp). Multi-entry lists are applied
// sequentially: each regex further splits the pieces the previous one
// produced, mirroring llama.cpp's `unicode_regex_split`. Where llama.cpp
// carries the original tokenizer.json regex in a comment (it adapts `(?i:)`
// for its own engine), the original is used here because fancy-regex
// supports it directly.
const PRE_GPT2: &[&str] =
    &[r"'s|'t|'re|'ve|'m|'ll|'d| ?\p{L}+| ?\p{N}+| ?[^\s\p{L}\p{N}]+|\s+(?!\S)|\s+"];
const PRE_QWEN2: &[&str] = &[
    r"(?i:'s|'t|'re|'ve|'m|'ll|'d)|[^\r\n\p{L}\p{N}]?\p{L}+|\p{N}| ?[^\s\p{L}\p{N}]+[\r\n]*|\s*[\r\n]+|\s+(?!\S)|\s+",
];
// Qwen3.5/3.6 (qwen35moe): like PRE_QWEN2 but the letter classes include
// combining marks (\p{M}), so scripts with separate marks split identically to
// upstream.
const PRE_QWEN35: &[&str] = &[
    r"(?i:'s|'t|'re|'ve|'m|'ll|'d)|[^\r\n\p{L}\p{N}]?[\p{L}\p{M}]+|\p{N}| ?[^\s\p{L}\p{M}\p{N}]+[\r\n]*|\s*[\r\n]+|\s+(?!\S)|\s+",
];
const PRE_LLAMA3: &[&str] = &[
    r"(?i:'s|'t|'re|'ve|'m|'ll|'d)|[^\r\n\p{L}\p{N}]?\p{L}+|\p{N}{1,3}| ?[^\s\p{L}\p{N}]+[\r\n]*|\s*[\r\n]+|\s+(?!\S)|\s+",
];
const PRE_DEFAULT: &[&str] = &[
    "[\\p{P}\\$\\+<=>\\^~\\|]+",
    "'s|'t|'re|'ve|'m|'ll|'d| ?\\p{L}+| ?\\p{N}+| ?[^\\s\\p{L}\\p{N}]+|\\s+(?!\\S)",
    "\\p{N}+",
    "[0-9][0-9][0-9]",
];
const PRE_FALCON: &[&str] = &[
    "[\\p{P}\\$\\+<=>\\^~\\|`]+",
    "'s|'t|'re|'ve|'m|'ll|'d| ?\\p{L}+| ?\\p{N}+| ?[^\\s\\p{L}\\p{N}]+|\\s+(?!\\S)",
    "[0-9][0-9][0-9]",
];
const PRE_STARCODER: &[&str] = &[
    "\\p{N}",
    "'s|'t|'re|'ve|'m|'ll|'d| ?\\p{L}+| ?\\p{N}+| ?[^\\s\\p{L}\\p{N}]+|\\s+(?!\\S)",
];
const PRE_DEEPSEEK_LLM: &[&str] = &[
    "[\r\n]",
    // Cased-letters class from the deepseek tokenizer. Three Greek Extended
    // range endpoints are written escaped: the llama.cpp source carries them
    // NFC-normalized to lookalike singletons (U+1F7D→U+03CE, U+1FD3→U+0390,
    // U+1FDB→U+038A), which makes the ranges descending — llama.cpp's engine
    // tolerates that, fancy-regex correctly rejects it.
    "\\s?[A-Za-zµÀ-ÖØ-öø-ƺƼ-ƿǄ-ʓʕ-ʯͰ-ͳͶͷͻ-ͽͿΆΈ-ΊΌΎ-ΡΣ-ϵϷ-ҁҊ-ԯԱ-ՖႠ-ჅᎠ-Ᏽᏸ-ᏽᲐ-ᲺᲽ-Ჿᴀ-ᴫᵫ-ᵷᵹ-ᶚḀ-ἕἘ-Ἕἠ-ὅὈ-Ὅὐ-ὗὙὛὝὟ-\u{1F7D}ᾀ-ᾴᾶ-ᾼιῂ-ῄῆ-ῌῐ-\u{1FD3}ῖ-\u{1FDB}ῠ-Ῥῲ-ῴῶ-ῼℂℇℊ-ℓℕℙ-ℝℤΩℨK-ℭℯ-ℴℹℼ-ℿⅅ-ⅉⅎↃↄⰀ-ⱻⱾ-ⳤⳫ-ⳮⳲⳳꙀ-ꙭꚀ-ꚛꜢ-ꝯꝱ-ꞇꞋ-ꞎꭰ-ꮿﬀ-ﬆﬓ-ﬗＡ-Ｚａ-ｚ𐐀-𐑏𐒰-𐓓𐓘-𐓻𐲀-𐲲𐳀-𐳲𑢠-𑣟𞤀-𞥃]+",
    "\\s?[!-/:-~！-／：-～‘-‟　-。]+",
    "\\s+$",
    "[一-龥ࠀ-一가-퟿]+",
    "\\p{N}+",
];
const PRE_DEEPSEEK_CODER: &[&str] = &[
    "[\r\n]",
    "\\s?\\p{L}+",
    "\\s?\\p{P}+",
    "[一-龥ࠀ-一가-퟿]+",
    "\\p{N}",
];
const PRE_DEEPSEEK3: &[&str] = &[
    "\\p{N}{1,3}",
    "[一-龥぀-ゟ゠-ヿ]+",
    "[!\"#$%&'()*+,\\-./:;<=>?@\\[\\\\\\]^_`{|}~][A-Za-z]+|[^\r\n\\p{L}\\p{P}\\p{S}]?[\\p{L}\\p{M}]+| ?[\\p{P}\\p{S}]+[\r\n]*|\\s*[\r\n]+|\\s+(?!\\S)|\\s+",
];
const PRE_TEKKEN: &[&str] = &[
    "[^\\r\\n\\p{L}\\p{N}]?((?=[\\p{L}])([^a-z]))*((?=[\\p{L}])([^A-Z]))+|[^\\r\\n\\p{L}\\p{N}]?((?=[\\p{L}])([^a-z]))+((?=[\\p{L}])([^A-Z]))*|\\p{N}| ?[^\\s\\p{L}\\p{N}]+[\\r\\n/]*|\\s*[\\r\\n]+|\\s+(?!\\S)|\\s+",
];
const PRE_GPT4O: &[&str] = &[
    "[^\\r\\n\\p{L}\\p{N}]?[\\p{Lu}\\p{Lt}\\p{Lm}\\p{Lo}\\p{M}]*[\\p{Ll}\\p{Lm}\\p{Lo}\\p{M}]+(?i:'s|'t|'re|'ve|'m|'ll|'d)?|[^\\r\\n\\p{L}\\p{N}]?[\\p{Lu}\\p{Lt}\\p{Lm}\\p{Lo}\\p{M}]+[\\p{Ll}\\p{Lm}\\p{Lo}\\p{M}]*(?i:'s|'t|'re|'ve|'m|'ll|'d)?|\\p{N}{1,3}| ?[^\\s\\p{L}\\p{N}]+[\\r\\n/]*|\\s*[\\r\\n]+|\\s+(?!\\S)|\\s+",
];

/// Per-`tokenizer.ggml.pre` split regexes + BPE `ignore_merges` flag, mirroring
/// llama.cpp's pre-tokenizer table (llama_vocab::impl::load, src/llama-vocab.cpp).
/// Unknown pre schemes are an error: silently substituting another regex would
/// produce wrong token ids.
fn pre_spec(pre: &str) -> Result<(&'static [&'static str], bool)> {
    match pre {
        "" | "default" => Ok((PRE_DEFAULT, false)),
        "gpt-2" | "phi-2" | "jina-es" | "jina-de" | "gigachat" | "jina-v2-es" | "jina-v2-de"
        | "a.x-4.0" | "mellum" | "modern-bert" | "mpt" | "olmo" | "jais" | "trillion"
        | "granite-docling" | "exaone4" => Ok((PRE_GPT2, false)),
        "qwen2" | "deepseek-r1-qwen" | "kormo" | "f2llmv2" | "megrez" | "stablelm2" | "hunyuan" => {
            Ok((PRE_QWEN2, false))
        }
        "qwen35" => Ok((PRE_QWEN35, false)),
        "llama3" | "llama-v3" | "llama-bpe" | "falcon3" | "falcon-h1" | "pixtral" | "midm-2.0"
        | "lfm2" | "jina-v5-nano" => Ok((PRE_LLAMA3, true)),
        "dbrx" | "smaug-bpe" | "glm4" | "chatglm-bpe" => Ok((PRE_LLAMA3, false)),
        "falcon" => Ok((PRE_FALCON, false)),
        "starcoder" | "refact" | "command-r" | "smollm" | "codeshell" | "exaone" | "minerva-7b" => {
            Ok((PRE_STARCODER, false))
        }
        "deepseek-llm" => Ok((PRE_DEEPSEEK_LLM, false)),
        "deepseek-coder" => Ok((PRE_DEEPSEEK_CODER, false)),
        "deepseek-v3" => Ok((PRE_DEEPSEEK3, false)),
        "tekken" => Ok((PRE_TEKKEN, true)),
        "gpt-4o" | "llama4" | "kanana2" | "talkie" => Ok((PRE_GPT4O, false)),
        other => Err(ForgeError::Tokenizer(format!(
            "unimplemented GGUF pre-tokenizer scheme {other:?}: refusing to substitute \
             another split regex (would produce wrong token ids)"
        ))),
    }
}

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
            "gemma4" => build_gemma4(vocab)?,
            other => {
                return Err(ForgeError::Tokenizer(format!(
                    "unsupported GGUF tokenizer model {other:?} \
                     (expected \"gpt2\", \"llama\" or \"gemma4\")"
                )))
            }
        };
        add_special_tokens(&mut inner, vocab)?;
        if vocab.add_bos || vocab.add_eos {
            attach_bos_eos_processor(&mut inner, vocab)?;
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

    // "ignore_merges" (a word already present in the vocab is emitted directly
    // without running merges) is a per-pre property: llama-3-style vocabs and
    // tekken enable it, qwen2 and the rest use the explicit merge table only.
    let (split_regexes, ignore_merges) = pre_spec(vocab.pre.as_str())?;
    let model = BPE::builder()
        .vocab_and_merges(vocab_map, merges)
        .ignore_merges(ignore_merges)
        .byte_fallback(false)
        .fuse_unk(false)
        .build()
        .map_err(|e| ForgeError::Tokenizer(format!("failed to build BPE model: {e}")))?;

    let mut tokenizer = tokenizers::Tokenizer::new(model);

    // The GPT-2 ByteLevel pre-tokenizer embeds its own split regex; each pre
    // scheme has its own regex list, so explicit Splits run first and
    // ByteLevel only does the byte→unicode alphabet mapping.
    tokenizer.with_pre_tokenizer(Some(splits_then_byte_level(split_regexes)?));
    tokenizer.with_decoder(Some(
        ByteLevel::default()
            .add_prefix_space(false)
            .trim_offsets(false)
            .use_regex(false),
    ));
    Ok(tokenizer)
}

fn splits_then_byte_level(patterns: &[&str]) -> Result<PreTokenizerWrapper> {
    let mut steps: Vec<PreTokenizerWrapper> = Vec::with_capacity(patterns.len() + 1);
    for pattern in patterns {
        let split = Split::new(
            SplitPattern::Regex((*pattern).to_string()),
            SplitDelimiterBehavior::Isolated,
            false,
        )
        .map_err(|e| ForgeError::Tokenizer(format!("invalid pre-tokenizer regex: {e}")))?;
        steps.push(split.into());
    }
    steps.push(
        ByteLevel::default()
            .add_prefix_space(false)
            .trim_offsets(false)
            .use_regex(false)
            .into(),
    );
    Ok(tokenizers::pre_tokenizers::sequence::Sequence::new(steps).into())
}

/// llama family: GGUF stores SPM pieces + scores but no merges. Rebuild BPE
/// merges the same way transformers' GGUFLlamaConverter does: for every piece,
/// every two-way split whose halves are both vocab entries becomes a merge
/// candidate ranked by the piece score (higher score = earlier merge).
/// Tokenizer rodziny Gemma 4: BPE z JAWNĄ tablicą merge'ów, ale w kształcie SPM.
///
/// Trzy rzeczy różnią go od `gpt2` i każda zmienia wynik tokenizacji:
///  * spacje zamienia normalizator na `▁` (merge'e w pliku są zapisane właśnie
///    w tej postaci), a NIE kodowanie bajtowe GPT-2 — dlatego bez `ByteLevel`;
///  * pre-tokenizacja dzieli wyłącznie po nowych liniach (`[^\n]+|[\n]+`), bo
///    merge'e biegną po całym tekście, a nie po słowach;
///  * `add_space_prefix` jest w tym modelu wyłączone, więc nie doklejamy `▁`
///    na początku (inaczej niż w ścieżce SPM).
fn build_gemma4(vocab: &GgufVocab) -> Result<tokenizers::Tokenizer> {
    if vocab.merges.is_empty() {
        return Err(ForgeError::Tokenizer(
            "GGUF gemma4 vocab wymaga tablicy merges".into(),
        ));
    }
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

    let model = BPE::builder()
        .vocab_and_merges(vocab_map, merges)
        .ignore_merges(false)
        .byte_fallback(false)
        .fuse_unk(false)
        .build()
        .map_err(|e| ForgeError::Tokenizer(format!("failed to build gemma4 BPE model: {e}")))?;

    let mut tokenizer = tokenizers::Tokenizer::new(model);
    let replace: NormalizerWrapper = Replace::new(" ", "▁")
        .map_err(|e| ForgeError::Tokenizer(format!("failed to build normalizer: {e}")))?
        .into();
    tokenizer
        .with_normalizer(Some(replace))
        .map_err(|e| ForgeError::Tokenizer(format!("failed to set normalizer: {e}")))?;

    let split = Split::new(
        SplitPattern::Regex("[^\n]+|[\n]+".to_string()),
        SplitDelimiterBehavior::Isolated,
        false,
    )
    .map_err(|e| ForgeError::Tokenizer(format!("failed to build pre-tokenizer: {e}")))?;
    tokenizer.with_pre_tokenizer(Some(PreTokenizerWrapper::from(split)));

    tokenizer.with_decoder(Some(tokenizers::decoders::DecoderWrapper::from(
        Replace::new("▁", " ")
            .map_err(|e| ForgeError::Tokenizer(format!("failed to build decoder: {e}")))?,
    )));
    Ok(tokenizer)
}

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

fn special_token(vocab: &GgufVocab, id: Option<u32>, which: &str) -> Result<(String, u32)> {
    let id = id.ok_or_else(|| {
        ForgeError::Tokenizer(format!(
            "GGUF vocab has add_{which}=true but no {which}_token_id"
        ))
    })?;
    let token = vocab.tokens.get(id as usize).cloned().ok_or_else(|| {
        ForgeError::Tokenizer(format!("GGUF {which}_token_id {id} out of vocab range"))
    })?;
    Ok((token, id))
}

fn attach_bos_eos_processor(
    tokenizer: &mut tokenizers::Tokenizer,
    vocab: &GgufVocab,
) -> Result<()> {
    let bos = if vocab.add_bos {
        Some(special_token(vocab, vocab.bos_id, "bos")?)
    } else {
        None
    };
    let eos = if vocab.add_eos {
        Some(special_token(vocab, vocab.eos_id, "eos")?)
    } else {
        None
    };
    let turn = |seq: &str| {
        let mut parts = Vec::new();
        if let Some((tok, _)) = &bos {
            parts.push(tok.clone());
        }
        parts.push(seq.to_string());
        if let Some((tok, _)) = &eos {
            parts.push(tok.clone());
        }
        parts.join(" ")
    };
    let mut specials: Vec<(String, u32)> = bos.iter().chain(eos.iter()).cloned().collect();
    specials.dedup();
    let processor = TemplateProcessing::builder()
        .try_single(turn("$A"))
        .map_err(|e| ForgeError::Tokenizer(format!("failed to build bos/eos template: {e}")))?
        .try_pair(format!("{} {}", turn("$A"), turn("$B")))
        .map_err(|e| ForgeError::Tokenizer(format!("failed to build bos/eos pair template: {e}")))?
        .special_tokens(specials)
        .build()
        .map_err(|e| ForgeError::Tokenizer(format!("failed to build bos/eos processor: {e}")))?;
    tokenizer.with_post_processor(Some(processor));
    Ok(())
}
