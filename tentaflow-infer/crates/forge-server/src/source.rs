// =============================================================================
// Plik: source.rs
// Opis: Rozwiązuje model, tokenizer, tokeny specjalne i szablon rozmowy.
// Przykład: load_model(device, path, config)
// =============================================================================

use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use forge_engine::model::{Model, ModelConfig};
use forge_formats::{Gguf, HfConfig, ModelDescriptor, PoolingType};
use forge_hal::Device;
use forge_tokenize::{resolve_chat_template, Tokenizer};

/// Tokenizer + everything the API layer needs around it.
pub struct TokenizerBundle {
    pub tokenizer: Tokenizer,
    /// Template source from tokenizer_config / chat_template.jinja / GGUF
    /// metadata; `None` falls back to the builtin family registry.
    pub chat_template: Option<String>,
    /// Literal special-token strings some templates reference as variables.
    pub bos_token: Option<String>,
    pub eos_token: Option<String>,
    /// All ids that terminate generation (config may declare several).
    pub eos_ids: Vec<u32>,
}

impl TokenizerBundle {
    /// Final template source, using the builtin registry keyed by the model
    /// architecture when the model ships none.
    pub fn resolve_template(&self, arch: &str) -> Result<String> {
        let family = builtin_family_for_arch(arch);
        resolve_chat_template(None, self.chat_template.as_deref(), None, Some(family))
            .map(str::to_string)
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Context variables templates reference besides `messages`
    /// (`{{ bos_token }}` etc.).
    pub fn template_vars(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut vars = serde_json::Map::new();
        if let Some(bos) = &self.bos_token {
            vars.insert("bos_token".into(), serde_json::Value::String(bos.clone()));
        }
        if let Some(eos) = &self.eos_token {
            vars.insert("eos_token".into(), serde_json::Value::String(eos.clone()));
        }
        vars
    }
}

/// Map an engine architecture name to a builtin chat-template family.
pub fn builtin_family_for_arch(arch: &str) -> &'static str {
    if arch.starts_with("qwen") {
        "qwen"
    } else if arch.starts_with("llama") {
        "llama3"
    } else if arch.starts_with("mistral") {
        "mistral"
    } else if arch.starts_with("gemma") {
        "gemma"
    } else {
        "chatml"
    }
}

/// Special-token fields in tokenizer_config.json are either a bare string or
/// an added-token object with a `content` field.
fn token_str(v: Option<&serde_json::Value>) -> Option<String> {
    match v? {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Object(o) => o
            .get("content")
            .and_then(|c| c.as_str())
            .map(str::to_string),
        _ => None,
    }
}

/// Sidecar metadata of an HF snapshot dir, parsed without the tokenizer
/// itself so it stays unit-testable.
#[derive(Debug, Default)]
pub struct DirMeta {
    pub chat_template: Option<String>,
    pub bos_token: Option<String>,
    pub eos_token: Option<String>,
    pub pad_token: Option<String>,
    /// `generation_config.json` / `config.json` `eos_token_id` entries.
    pub eos_token_ids: Vec<u32>,
}

pub fn read_dir_meta(dir: &Path) -> Result<DirMeta> {
    let mut meta = DirMeta::default();

    let tc_path = dir.join("tokenizer_config.json");
    if tc_path.is_file() {
        let tc: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&tc_path).with_context(|| format!("read {}", tc_path.display()))?,
        )
        .with_context(|| format!("parse {}", tc_path.display()))?;
        meta.chat_template = tc
            .get("chat_template")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        meta.bos_token = token_str(tc.get("bos_token"));
        meta.eos_token = token_str(tc.get("eos_token"));
        meta.pad_token = token_str(tc.get("pad_token"));
    }

    // A standalone chat_template.jinja outranks nothing but fills the gap
    // when tokenizer_config carries no template (HF convention).
    if meta.chat_template.is_none() {
        let jinja = dir.join("chat_template.jinja");
        if jinja.is_file() {
            meta.chat_template = Some(
                std::fs::read_to_string(&jinja)
                    .with_context(|| format!("read {}", jinja.display()))?,
            );
        }
    }

    for name in ["generation_config.json", "config.json"] {
        let p = dir.join(name);
        if !p.is_file() {
            continue;
        }
        let cfg: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&p).with_context(|| format!("read {}", p.display()))?,
        )
        .with_context(|| format!("parse {}", p.display()))?;
        match cfg.get("eos_token_id") {
            Some(serde_json::Value::Number(n)) => {
                if let Some(id) = n.as_u64() {
                    meta.eos_token_ids.push(id as u32);
                }
            }
            Some(serde_json::Value::Array(a)) => {
                meta.eos_token_ids
                    .extend(a.iter().filter_map(|v| v.as_u64()).map(|v| v as u32));
            }
            _ => {}
        }
        if !meta.eos_token_ids.is_empty() {
            break;
        }
    }

    Ok(meta)
}

/// Build the tokenizer bundle for an HF snapshot directory.
pub fn load_tokenizer_dir(dir: &Path) -> Result<TokenizerBundle> {
    let meta = read_dir_meta(dir)?;
    let mut tokenizer =
        Tokenizer::from_file(dir.join("tokenizer.json")).map_err(|e| anyhow::anyhow!("{e}"))?;

    let to_id = |tok: &Option<String>| tok.as_deref().and_then(|t| tokenizer.token_to_id(t));
    let bos_id = to_id(&meta.bos_token);
    let eos_id = to_id(&meta.eos_token);
    let pad_id = to_id(&meta.pad_token);
    tokenizer.set_special_ids(bos_id, eos_id, pad_id);

    let mut eos_ids = meta.eos_token_ids.clone();
    if let Some(id) = eos_id {
        if !eos_ids.contains(&id) {
            eos_ids.push(id);
        }
    }
    if eos_ids.is_empty() {
        bail!(
            "no EOS token id found in {} (tokenizer_config.json / generation_config.json / config.json)",
            dir.display()
        );
    }

    Ok(TokenizerBundle {
        tokenizer,
        chat_template: meta.chat_template,
        bos_token: meta.bos_token,
        eos_token: meta.eos_token,
        eos_ids,
    })
}

/// Build the tokenizer bundle from GGUF-embedded metadata.
pub fn load_tokenizer_gguf(gguf: &Gguf) -> Result<TokenizerBundle> {
    let vocab = forge_tokenize::gguf_vocab(gguf).map_err(|e| anyhow::anyhow!("{e}"))?;
    let tokenizer = Tokenizer::from_gguf_vocab(&vocab).map_err(|e| anyhow::anyhow!("{e}"))?;
    let piece = |id: Option<u32>| id.and_then(|i| tokenizer.token_to_piece(i));
    let bos_token = piece(vocab.bos_id);
    let eos_token = piece(vocab.eos_id);
    let eos_ids: Vec<u32> = vocab.eos_id.into_iter().collect();
    if eos_ids.is_empty() {
        bail!("GGUF declares no tokenizer.ggml.eos_token_id");
    }
    Ok(TokenizerBundle {
        tokenizer,
        chat_template: gguf.get_str("tokenizer.chat_template").map(str::to_string),
        bos_token,
        eos_token,
        eos_ids,
    })
}

/// Read only the architecture descriptor (dims, arch name) of a model path,
/// without loading weights — used to size GPU pools before device creation.
pub fn read_descriptor(path: &Path) -> Result<ModelDescriptor> {
    if path.is_dir() {
        let cfg = HfConfig::load(path.join("config.json")).map_err(|e| anyhow::anyhow!("{e}"))?;
        ModelDescriptor::from_hf(&cfg).map_err(|e| anyhow::anyhow!("{e}"))
    } else {
        let gguf = Gguf::open(path)?;
        ModelDescriptor::detect(&gguf).map_err(|e| anyhow::anyhow!("{e}"))
    }
}

/// Oblicza rozmiar puli KV z uwzględnieniem wyrównania każdego bufora.
/// Tryb Rot rezerwuje historię niskobitową, skale F16 oraz residual ring.
/// Model z włączonym natywnym MTP otrzymuje dodatkowo jedną parę pełnych slabów F16.
pub fn kv_pool_bytes(
    desc: &ModelDescriptor,
    kv_page_size: usize,
    kv_pages: usize,
    quant: forge_engine::kv::KvQuant,
    native_mtp: bool,
) -> Result<usize> {
    let p = &desc.params;
    let overflow = || anyhow::anyhow!("rozmiar puli KV przekracza zakres usize");
    let slots = p
        .kv_cache_heads()
        .checked_mul(kv_page_size)
        .and_then(|value| value.checked_mul(kv_pages))
        .ok_or_else(overflow)?;
    let granularity = forge_hal::cuda::PoolSizes::DEFAULT_KV_PAGE;
    let round = |b: usize| -> Result<usize> {
        let pages = b / granularity + usize::from(!b.is_multiple_of(granularity));
        pages.checked_mul(granularity).ok_or_else(overflow)
    };
    let per_layer_pair = if let Some(pb) = quant.packed_bytes(p.kv_cache_head_dim())? {
        // Rot przechowuje residual ring F16, kody niskobitowe i skale F16.
        let ring_slots = quant.ring_slots().unwrap_or(1);
        let ring = ring_slots
            .checked_mul(p.kv_cache_heads())
            .and_then(|value| value.checked_mul(p.kv_cache_head_dim()))
            .and_then(|value| value.checked_mul(quant.slab_dtype().size()))
            .ok_or_else(overflow)?;
        let packed = slots.checked_mul(pb).ok_or_else(overflow)?;
        let scales = slots.checked_mul(2).ok_or_else(overflow)?;
        let ring_pair = 2usize.checked_mul(round(ring)?).ok_or_else(overflow)?;
        let packed_pair = 2usize.checked_mul(round(packed)?).ok_or_else(overflow)?;
        let scale_pair = 2usize.checked_mul(round(scales)?).ok_or_else(overflow)?;
        ring_pair
            .checked_add(packed_pair)
            .and_then(|value| value.checked_add(scale_pair))
            .ok_or_else(overflow)?
    } else {
        let slab = slots
            .checked_mul(p.kv_cache_head_dim())
            .and_then(|value| value.checked_mul(quant.slab_dtype().size()))
            .ok_or_else(overflow)?;
        2usize.checked_mul(round(slab)?).ok_or_else(overflow)?
    };
    let mtp_pair = if native_mtp && desc.mtp.is_some() {
        let slab = slots
            .checked_mul(p.head_dim)
            .and_then(|value| value.checked_mul(forge_types::DType::F16.size()))
            .ok_or_else(overflow)?;
        2usize.checked_mul(round(slab)?).ok_or_else(overflow)?
    } else {
        0
    };
    let target_layers = desc
        .layer_kinds
        .iter()
        .filter(|kind| matches!(kind, forge_formats::LayerKind::Attention))
        .count();
    target_layers
        .checked_mul(per_layer_pair)
        .and_then(|value| value.checked_add(mtp_pair))
        .and_then(|value| value.checked_add(64 * (1 << 20)))
        .ok_or_else(overflow)
}

/// Resolve the sequence pooling for an embedding model path. GGUF carries
/// `<arch>.pooling_type` in the descriptor; a sentence-transformers HF
/// snapshot keeps it in `1_Pooling/config.json`. Falls back to the
/// descriptor's value (which is `None` for HF dirs) when the sidecar is
/// absent; the engine then defaults `None` to mean pooling.
pub fn resolve_pooling(path: &Path, desc: &ModelDescriptor) -> PoolingType {
    if path.is_dir() {
        read_pooling_dir(path).unwrap_or(desc.params.pooling_type)
    } else {
        desc.params.pooling_type
    }
}

fn read_pooling_dir(dir: &Path) -> Option<PoolingType> {
    let cfg: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("1_Pooling").join("config.json")).ok()?)
            .ok()?;
    let flag = |k: &str| cfg.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    if flag("pooling_mode_lasttoken") {
        Some(PoolingType::Last)
    } else if flag("pooling_mode_cls_token") {
        Some(PoolingType::Cls)
    } else if flag("pooling_mode_mean_tokens") {
        Some(PoolingType::Mean)
    } else {
        None
    }
}

/// Whether the model's pooled vector should be L2-normalized. HF
/// sentence-transformers snapshots declare this with a `Normalize` module in
/// `modules.json`; GGUF embedding models (and dirs without the sidecar)
/// default to normalized, the convention for retrieval embeddings.
pub fn resolve_normalize(path: &Path) -> bool {
    if !path.is_dir() {
        return true;
    }
    match std::fs::read(path.join("modules.json")) {
        Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(serde_json::Value::Array(modules)) => modules.iter().any(|m| {
                m.get("type")
                    .and_then(|v| v.as_str())
                    .is_some_and(|t| t.contains("Normalize"))
            }),
            _ => true,
        },
        Err(_) => true,
    }
}

/// Everything the server needs from a model path, ready to hand to
/// `spawn_engine`.
pub struct LoadedModel {
    pub model: Model,
    pub bundle: TokenizerBundle,
    pub chat_template: String,
}

/// Load weights + tokenizer + template for a GGUF file or HF snapshot dir.
pub fn load_model(device: Arc<dyn Device>, path: &Path, cfg: ModelConfig) -> Result<LoadedModel> {
    let (model, bundle) = if path.is_dir() {
        let bundle = load_tokenizer_dir(path)?;
        let model =
            Model::load_safetensors_dir(device, path, cfg).map_err(|e| anyhow::anyhow!("{e}"))?;
        (model, bundle)
    } else {
        let gguf = Gguf::open(path)?;
        let bundle = load_tokenizer_gguf(&gguf)?;
        drop(gguf);
        let model = Model::load_gguf(device, path, cfg).map_err(|e| anyhow::anyhow!("{e}"))?;
        (model, bundle)
    };
    let chat_template = bundle.resolve_template(&model.weights.descriptor.arch)?;
    Ok(LoadedModel {
        model,
        bundle,
        chat_template,
    })
}

/// Jak `load_model`, ale model jest rozłożony na `devices` jako podział
/// tensor-parallel. Tokenizer i szablon są jedne — dotyczą modelu, nie rangi.
pub fn load_model_tp(
    devices: &[Arc<dyn Device>],
    path: &Path,
    cfg: ModelConfig,
) -> Result<LoadedModel> {
    if path.is_dir() {
        anyhow::bail!("podział na rangi czyta na razie wyłącznie pojedynczy plik GGUF");
    }
    let gguf = Gguf::open(path)?;
    let bundle = load_tokenizer_gguf(&gguf)?;
    drop(gguf);
    let model = Model::load_tp(devices, path, cfg).map_err(|e| anyhow::anyhow!("{e}"))?;
    let chat_template = bundle.resolve_template(&model.weights.descriptor.arch)?;
    Ok(LoadedModel {
        model,
        bundle,
        chat_template,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_formats::{LayerKind, MtpDescriptor};
    use forge_tokenize::{ChatMessage, ChatTemplateEngine};

    fn qwen_hybrid_descriptor(native_mtp: bool) -> ModelDescriptor {
        let config: HfConfig = HfConfig::from_json_str(
            r#"{
                "architectures": ["LlamaForCausalLM"],
                "model_type": "llama",
                "hidden_size": 4096,
                "num_hidden_layers": 64,
                "num_attention_heads": 16,
                "num_key_value_heads": 4,
                "head_dim": 256,
                "intermediate_size": 12288,
                "vocab_size": 248320,
                "max_position_embeddings": 262144
            }"#,
        )
        .unwrap();
        let mut descriptor = ModelDescriptor::from_hf(&config).unwrap();
        descriptor.layer_kinds = [
            vec![LayerKind::DeltaNet; 48],
            vec![LayerKind::Attention; 16],
        ]
        .concat();
        if native_mtp {
            descriptor.mtp = Some(MtpDescriptor {
                first_block: descriptor.params.block_count,
                block_count: 1,
                layers: vec![Default::default()],
                share_target_embedding: true,
                share_target_output: true,
            });
        }
        descriptor
    }

    #[test]
    fn pula_kv_qwen_4096_bez_mtp_ma_320_mib() {
        let descriptor = qwen_hybrid_descriptor(false);

        let bytes =
            kv_pool_bytes(&descriptor, 32, 128, forge_engine::kv::KvQuant::F16, false).unwrap();

        assert_eq!(bytes, 320 << 20);
    }

    #[test]
    fn pula_kv_qwen_4096_z_mtp_ma_336_mib() {
        let descriptor = qwen_hybrid_descriptor(true);

        let bytes =
            kv_pool_bytes(&descriptor, 32, 128, forge_engine::kv::KvQuant::F16, true).unwrap();

        assert_eq!(bytes, 336 << 20);
    }

    #[test]
    fn pula_kv_qwen_dla_czterech_kontekstow_ma_1088_mib() {
        let descriptor = qwen_hybrid_descriptor(false);

        let bytes =
            kv_pool_bytes(&descriptor, 32, 512, forge_engine::kv::KvQuant::F16, false).unwrap();

        assert_eq!(bytes, 1088 << 20);
    }

    #[test]
    fn pula_kv_qwen_dla_64_goracych_stron_ma_192_mib() {
        let descriptor = qwen_hybrid_descriptor(false);

        let bytes =
            kv_pool_bytes(&descriptor, 32, 64, forge_engine::kv::KvQuant::F16, false).unwrap();

        assert_eq!(bytes, 192 << 20);
    }

    #[test]
    fn pula_kv_odrzuca_przepelnienie_liczby_stron() {
        let descriptor = qwen_hybrid_descriptor(true);

        let result = kv_pool_bytes(
            &descriptor,
            usize::MAX,
            usize::MAX,
            forge_engine::kv::KvQuant::F16,
            true,
        );

        assert!(result.is_err());
    }

    #[test]
    fn pula_kv_rezerwuje_slab_mtp_tylko_gdy_runtime_jest_wlaczony() {
        let config: HfConfig = HfConfig::from_json_str(
            r#"{
                "architectures": ["LlamaForCausalLM"],
                "model_type": "llama",
                "hidden_size": 128,
                "num_hidden_layers": 2,
                "num_attention_heads": 2,
                "num_key_value_heads": 2,
                "head_dim": 64,
                "intermediate_size": 256,
                "vocab_size": 1024,
                "max_position_embeddings": 1024
            }"#,
        )
        .unwrap();
        let mut descriptor = ModelDescriptor::from_hf(&config).unwrap();
        descriptor.mtp = Some(MtpDescriptor {
            first_block: descriptor.params.block_count,
            block_count: 1,
            layers: vec![Default::default()],
            share_target_embedding: true,
            share_target_output: true,
        });
        let page_size = 32;
        let pages = 8;
        let without_mtp = kv_pool_bytes(
            &descriptor,
            page_size,
            pages,
            forge_engine::kv::KvQuant::F16,
            false,
        )
        .unwrap();
        let with_mtp = kv_pool_bytes(
            &descriptor,
            page_size,
            pages,
            forge_engine::kv::KvQuant::F16,
            true,
        )
        .unwrap();
        let slots = descriptor.params.n_kv_heads * page_size * pages;
        let slab = slots * descriptor.params.head_dim * forge_types::DType::F16.size();
        let granularity = forge_hal::cuda::PoolSizes::DEFAULT_KV_PAGE;
        let expected_mtp_pair = 2 * slab.div_ceil(granularity) * granularity;

        assert_eq!(with_mtp - without_mtp, expected_mtp_pair);
    }

    #[test]
    fn hybrydowa_pula_kv_obejmuje_tylko_warstwy_attention() {
        let config: HfConfig = HfConfig::from_json_str(
            r#"{
                "architectures": ["LlamaForCausalLM"],
                "model_type": "llama",
                "hidden_size": 1024,
                "num_hidden_layers": 64,
                "num_attention_heads": 4,
                "num_key_value_heads": 4,
                "head_dim": 256,
                "intermediate_size": 2048,
                "vocab_size": 1024,
                "max_position_embeddings": 1024
            }"#,
        )
        .unwrap();
        let mut descriptor = ModelDescriptor::from_hf(&config).unwrap();
        descriptor.layer_kinds = [
            vec![LayerKind::DeltaNet; 48],
            vec![LayerKind::Attention; 16],
        ]
        .concat();
        let page_size = 32;
        let pages = 4;

        let bytes = kv_pool_bytes(
            &descriptor,
            page_size,
            pages,
            forge_engine::kv::KvQuant::F16,
            false,
        )
        .unwrap()
            - 64 * (1 << 20);

        assert_eq!(bytes / (page_size * pages), 64 * 1024);
    }

    #[test]
    fn family_mapping() {
        assert_eq!(builtin_family_for_arch("qwen3"), "qwen");
        assert_eq!(builtin_family_for_arch("llama"), "llama3");
        assert_eq!(builtin_family_for_arch("mistral"), "mistral");
        assert_eq!(builtin_family_for_arch("weird-arch"), "chatml");
    }

    #[test]
    fn tokenizer_config_template_beats_jinja_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tokenizer_config.json"),
            serde_json::json!({
                "chat_template": "FROM_CONFIG",
                "bos_token": "<s>",
                "eos_token": {"content": "<|im_end|>"}
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(dir.path().join("chat_template.jinja"), "FROM_JINJA").unwrap();

        let meta = read_dir_meta(dir.path()).unwrap();
        assert_eq!(meta.chat_template.as_deref(), Some("FROM_CONFIG"));
        assert_eq!(meta.bos_token.as_deref(), Some("<s>"));
        assert_eq!(meta.eos_token.as_deref(), Some("<|im_end|>"));
    }

    #[test]
    fn jinja_file_fills_missing_config_template_and_eos_ids_parse() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tokenizer_config.json"),
            serde_json::json!({ "chat_template": null, "eos_token": "</s>" }).to_string(),
        )
        .unwrap();
        std::fs::write(dir.path().join("chat_template.jinja"), "FROM_JINJA").unwrap();
        std::fs::write(
            dir.path().join("generation_config.json"),
            serde_json::json!({ "eos_token_id": [4, 2] }).to_string(),
        )
        .unwrap();

        let meta = read_dir_meta(dir.path()).unwrap();
        assert_eq!(meta.chat_template.as_deref(), Some("FROM_JINJA"));
        assert_eq!(meta.eos_token_ids, vec![4, 2]);
    }

    #[test]
    fn scalar_eos_token_id_parses() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("generation_config.json"),
            serde_json::json!({ "eos_token_id": 7 }).to_string(),
        )
        .unwrap();
        let meta = read_dir_meta(dir.path()).unwrap();
        assert_eq!(meta.eos_token_ids, vec![7]);
    }

    #[test]
    fn bielik_style_template_renders_with_bos_var() {
        // Same shape as the Bielik chat_template.jinja: ChatML body preceded
        // by a {{bos_token}} variable reference.
        let template = "{{bos_token}}{% for message in messages %}{{'<|im_start|>' + message['role'] + '\n' + message['content'] + '<|im_end|>' + '\n'}}{% endfor %}{% if add_generation_prompt %}{{ '<|im_start|>assistant\n' }}{% endif %}";
        let mut vars = serde_json::Map::new();
        vars.insert("bos_token".into(), serde_json::Value::String("<s>".into()));
        let out = ChatTemplateEngine::new()
            .render(
                template,
                &[
                    ChatMessage::text("system", "Jesteś pomocny."),
                    ChatMessage::text("user", "Cześć!"),
                ],
                None,
                true,
                false,
                &vars,
            )
            .unwrap();
        assert_eq!(
            out,
            "<s><|im_start|>system\nJesteś pomocny.<|im_end|>\n<|im_start|>user\nCześć!<|im_end|>\n<|im_start|>assistant\n"
        );
    }
}
