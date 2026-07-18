// ===== File: arch.rs — declarative architecture registry: tensor roles + hyperparams =====
//
// Each supported architecture is described by an embedded RON file mapping
// tensor-name templates (`{layer}` placeholder) to semantic weight roles for
// both GGUF (`blk.N.attn_q.weight`) and HF (`model.layers.N.self_attn.
// q_proj.weight`) naming. `ModelDescriptor` resolves the templates into a
// concrete role → tensor-name map plus unified hyperparams.

use std::collections::HashMap;
use std::sync::OnceLock;

use forge_types::{ForgeError, Result};
use serde::Deserialize;

use crate::gguf::Gguf;
use crate::hf_config::HfConfig;

fn fmt_err(msg: impl Into<String>) -> ForgeError {
    ForgeError::Format(msg.into())
}

/// Sequence-pooling strategy for embedding models. Mirrors llama.cpp's
/// `<arch>.pooling_type` enum (0 = none, 1 = mean, 2 = cls, 3 = last); `None`
/// marks a plain generative model with no pooling declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolingType {
    None,
    Mean,
    Cls,
    Last,
}

impl PoolingType {
    /// Map the GGUF `<arch>.pooling_type` integer to a variant. Unknown values
    /// (e.g. 4 = rank, not an embedding pooler) degrade to `None`.
    fn from_gguf_u32(v: u32) -> Self {
        match v {
            1 => PoolingType::Mean,
            2 => PoolingType::Cls,
            3 => PoolingType::Last,
            _ => PoolingType::None,
        }
    }
}

/// Semantic role of a weight tensor in the transformer graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
pub enum WeightRole {
    TokenEmbd,
    AttnQ,
    AttnK,
    AttnV,
    AttnO,
    AttnQNorm,
    AttnKNorm,
    AttnNorm,
    FfnGate,
    FfnUp,
    FfnDown,
    FfnNorm,
    OutputNorm,
    LmHead,
    /// MoE router (`ffn_gate_inp`): logits over experts, [n_expert, hidden].
    FfnGateInp,
    /// MoE stacked expert projections ([n_expert, inter, hidden] resp.
    /// [n_expert, hidden, inter], quantized) — indexed per selected expert.
    FfnGateExps,
    FfnUpExps,
    FfnDownExps,
    /// Optional always-on shared expert (Qwen-MoE / DeepSeek), a dense FFN
    /// added to every token on top of the routed experts.
    FfnGateShExp,
    FfnUpShExp,
    FfnDownShExp,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RoleSpec {
    pub role: WeightRole,
    /// GGUF tensor-name template; `{layer}` expands to the layer index.
    pub gguf: String,
    /// HF safetensors tensor-name template.
    pub hf: String,
    pub per_layer: bool,
    /// Required roles must resolve to existing tensors on GGUF detect;
    /// optional ones (lm_head with tied embeddings, arch-specific norms) may
    /// be absent.
    pub required: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArchSpec {
    pub name: String,
    /// Value of GGUF `general.architecture` this spec matches.
    pub gguf_arch: String,
    /// HF `architectures` entries this spec matches.
    pub hf_architectures: Vec<String>,
    pub hf_model_types: Vec<String>,
    pub roles: Vec<RoleSpec>,
}

const ARCH_SOURCES: &[&str] = &[
    include_str!("../arch/qwen3.ron"),
    include_str!("../arch/llama.ron"),
    include_str!("../arch/mistral.ron"),
    include_str!("../arch/olmoe.ron"),
    include_str!("../arch/qwen3moe.ron"),
];

/// Embedded specs are compile-time assets of this crate, so a parse failure
/// is a build defect, not untrusted input; a unit test guards every file.
pub fn registry() -> &'static [ArchSpec] {
    static REGISTRY: OnceLock<Vec<ArchSpec>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        ARCH_SOURCES
            .iter()
            .map(|src| ron::from_str(src).expect("embedded arch spec must parse"))
            .collect()
    })
}

/// Mixture-of-Experts routing parameters (present only for MoE architectures).
#[derive(Debug, Clone, PartialEq)]
pub struct MoeParams {
    /// Total experts per MoE layer (`<arch>.expert_count`).
    pub n_experts: usize,
    /// Experts activated per token / top-k (`<arch>.expert_used_count`).
    pub n_experts_used: usize,
    /// Per-expert FFN hidden size (`<arch>.expert_feed_forward_length`).
    pub moe_intermediate_size: usize,
    /// Renormalize the top-k routing weights to sum 1 after selection
    /// (`<arch>.expert_weights_norm`). OLMoE = false, Qwen-MoE = true.
    pub norm_topk_prob: bool,
    /// Shared always-on expert FFN hidden size (0 = no shared expert).
    pub shared_intermediate_size: usize,
}

/// Unified model hyperparameters sourced from GGUF metadata or HF config.
#[derive(Debug, Clone, PartialEq)]
pub struct Hyperparams {
    pub block_count: usize,
    pub hidden_size: usize,
    pub n_heads: usize,
    pub n_kv_heads: usize,
    pub head_dim: usize,
    pub intermediate_size: usize,
    pub vocab_size: usize,
    pub rope_theta: f32,
    pub rms_norm_eps: f32,
    pub max_position_embeddings: usize,
    pub tie_word_embeddings: bool,
    /// Sequence pooling declared by the model (embedding models only); `None`
    /// for generative models.
    pub pooling_type: PoolingType,
    /// MoE routing parameters when the architecture is Mixture-of-Experts;
    /// `None` for a dense FFN model.
    pub moe: Option<MoeParams>,
    /// QK-norm is applied over the whole projection (`n_heads * head_dim`) once
    /// per token rather than per-head over `head_dim`. OLMoE normalizes the full
    /// query/key vector; Qwen3 normalizes each head. `false` when no QK-norm.
    pub qk_norm_over_hidden: bool,
}

/// Architecture + hyperparams + fully resolved weight-name map.
#[derive(Debug, Clone)]
pub struct ModelDescriptor {
    pub arch: String,
    pub params: Hyperparams,
    /// Non-per-layer weights (token_embd, output_norm, lm_head if untied).
    pub globals: HashMap<WeightRole, String>,
    /// Per-layer role → tensor name, index = layer.
    pub layers: Vec<HashMap<WeightRole, String>>,
}

fn expand(template: &str, layer: usize) -> String {
    template.replace("{layer}", &layer.to_string())
}

impl ModelDescriptor {
    /// Detect the architecture of a parsed GGUF file and resolve its weight map.
    pub fn detect(gguf: &Gguf) -> Result<Self> {
        let arch = gguf
            .get_str("general.architecture")
            .ok_or_else(|| fmt_err("gguf: missing general.architecture"))?;
        let spec = registry()
            .iter()
            .find(|s| s.gguf_arch == arch)
            .ok_or_else(|| {
                ForgeError::Unsupported(format!("no architecture spec for gguf arch '{arch}'"))
            })?;

        let key = |suffix: &str| format!("{arch}.{suffix}");
        let req_u = |suffix: &str| {
            gguf.get_u64(&key(suffix))
                .map(|v| v as usize)
                .ok_or_else(|| fmt_err(format!("gguf: missing metadata key {}", key(suffix))))
        };
        // Multi-token-prediction (MTP / NextN) speculation heads are the final
        // `nextn_predict_layers` blocks; they are not part of the autoregressive
        // main forward, so drop them from the transformer stack (basic decode
        // never runs them and they carry non-standard tensors).
        let nextn = gguf
            .get_u64(&key("nextn_predict_layers"))
            .unwrap_or(0) as usize;
        let block_count = req_u("block_count")?.saturating_sub(nextn);
        let hidden_size = req_u("embedding_length")?;
        let n_heads = req_u("attention.head_count")?;
        let n_kv_heads = gguf
            .get_u64(&key("attention.head_count_kv"))
            .map(|v| v as usize)
            .unwrap_or(n_heads);
        let head_dim = gguf
            .get_u64(&key("attention.key_length"))
            .map(|v| v as usize)
            .unwrap_or(hidden_size / n_heads.max(1));
        let intermediate_size = req_u("feed_forward_length")?;
        let max_position_embeddings = req_u("context_length")?;
        let rope_theta = gguf.get_f32(&key("rope.freq_base")).unwrap_or(10_000.0);
        let rms_norm_eps = gguf
            .get_f32(&key("attention.layer_norm_rms_epsilon"))
            .unwrap_or(1e-5);
        let vocab_size = gguf
            .get_u64(&key("vocab_size"))
            .map(|v| v as usize)
            .or_else(|| {
                // Derive from the embedding matrix: dims[0] is the hidden dim
                // (innermost), dims[1] the vocab rows.
                gguf.tensor("token_embd.weight")
                    .and_then(|t| t.dims.get(1))
                    .map(|&v| v as usize)
            })
            .or_else(|| gguf.get_array("tokenizer.ggml.tokens").map(|a| a.len()))
            .ok_or_else(|| fmt_err("gguf: cannot determine vocab size"))?;

        let mut globals = HashMap::new();
        let mut layers: Vec<HashMap<WeightRole, String>> = vec![HashMap::new(); block_count];
        for role in &spec.roles {
            if role.per_layer {
                for (layer, map) in layers.iter_mut().enumerate() {
                    let name = expand(&role.gguf, layer);
                    if gguf.tensor(&name).is_some() {
                        map.insert(role.role, name);
                    } else if role.required {
                        return Err(fmt_err(format!(
                            "gguf: arch '{}' requires tensor '{name}' which is missing",
                            spec.name
                        )));
                    }
                }
            } else if gguf.tensor(&role.gguf).is_some() {
                globals.insert(role.role, role.gguf.clone());
            } else if role.required {
                return Err(fmt_err(format!(
                    "gguf: arch '{}' requires tensor '{}' which is missing",
                    spec.name, role.gguf
                )));
            }
        }
        // Missing output.weight means the lm_head shares the embedding matrix.
        let tie_word_embeddings = !globals.contains_key(&WeightRole::LmHead);
        let pooling_type = gguf
            .get_u32(&key("pooling_type"))
            .map(PoolingType::from_gguf_u32)
            .unwrap_or(PoolingType::None);

        // Mixture-of-Experts: the presence of a positive expert_count promotes
        // the FFN block to routed experts (the `ffn_*_exps` stacked tensors were
        // resolved above as the FfnGateExps/UpExps/DownExps roles).
        let n_experts = gguf.get_u64(&key("expert_count")).unwrap_or(0) as usize;
        let moe = if n_experts > 0 {
            let n_experts_used = gguf
                .get_u64(&key("expert_used_count"))
                .map(|v| v as usize)
                .ok_or_else(|| fmt_err(format!("gguf: MoE model missing {}", key("expert_used_count"))))?;
            let moe_intermediate_size = gguf
                .get_u64(&key("expert_feed_forward_length"))
                .map(|v| v as usize)
                .unwrap_or(intermediate_size);
            let norm_topk_prob = gguf.get_bool(&key("expert_weights_norm")).unwrap_or(false);
            let shared_intermediate_size = gguf
                .get_u64(&key("expert_shared_feed_forward_length"))
                .map(|v| v as usize)
                .unwrap_or(0);
            Some(MoeParams {
                n_experts,
                n_experts_used,
                moe_intermediate_size,
                norm_topk_prob,
                shared_intermediate_size,
            })
        } else {
            None
        };

        // QK-norm granularity: OLMoE normalizes the whole q/k projection, so its
        // attn_q_norm vector spans n_heads*head_dim; Qwen3 normalizes per head,
        // so its vector spans head_dim. Read the resolved tensor's element count.
        let qk_norm_over_hidden = layers
            .first()
            .and_then(|m| m.get(&WeightRole::AttnQNorm))
            .and_then(|n| gguf.tensor(n))
            .map(|t| {
                let numel: usize = t.dims.iter().map(|&d| d as usize).product();
                numel != head_dim
            })
            .unwrap_or(false);

        // The MoE expert scratch is sized from intermediate_size, so fold the
        // expert (and any shared-expert) FFN width into it for a MoE model.
        let intermediate_size = match &moe {
            Some(m) => m.moe_intermediate_size.max(m.shared_intermediate_size),
            None => intermediate_size,
        };

        Ok(ModelDescriptor {
            arch: spec.name.clone(),
            params: Hyperparams {
                block_count,
                hidden_size,
                n_heads,
                n_kv_heads,
                head_dim,
                intermediate_size,
                vocab_size,
                rope_theta,
                rms_norm_eps,
                max_position_embeddings,
                tie_word_embeddings,
                pooling_type,
                moe,
                qk_norm_over_hidden,
            },
            globals,
            layers,
        })
    }

    /// Build a descriptor from an HF config.json (safetensors-side naming).
    pub fn from_hf(config: &HfConfig) -> Result<Self> {
        let spec = registry()
            .iter()
            .find(|s| {
                config
                    .architectures
                    .iter()
                    .any(|a| s.hf_architectures.iter().any(|h| h == a))
                    || config
                        .model_type
                        .as_deref()
                        .is_some_and(|mt| s.hf_model_types.iter().any(|h| h == mt))
            })
            .ok_or_else(|| {
                ForgeError::Unsupported(format!(
                    "no architecture spec for HF architectures {:?} / model_type {:?}",
                    config.architectures, config.model_type
                ))
            })?;

        let block_count = config.num_hidden_layers;
        let mut globals = HashMap::new();
        let mut layers: Vec<HashMap<WeightRole, String>> = vec![HashMap::new(); block_count];
        for role in &spec.roles {
            if role.per_layer {
                for (layer, map) in layers.iter_mut().enumerate() {
                    map.insert(role.role, expand(&role.hf, layer));
                }
            } else if role.role == WeightRole::LmHead && config.tie_word_embeddings {
                // Tied models have no separate lm_head tensor on disk.
            } else {
                globals.insert(role.role, role.hf.clone());
            }
        }

        Ok(ModelDescriptor {
            arch: spec.name.clone(),
            params: Hyperparams {
                block_count,
                hidden_size: config.hidden_size,
                n_heads: config.num_attention_heads,
                n_kv_heads: config.num_key_value_heads(),
                head_dim: config.head_dim(),
                intermediate_size: config.intermediate_size,
                vocab_size: config.vocab_size,
                rope_theta: config.rope_theta,
                rms_norm_eps: config.rms_norm_eps,
                max_position_embeddings: config.max_position_embeddings,
                tie_word_embeddings: config.tie_word_embeddings,
                // HF config.json carries no pooling; sentence-transformers keeps
                // it in a `1_Pooling/config.json` sidecar the loader overrides.
                pooling_type: PoolingType::None,
                // Safetensors MoE loading is not wired yet; the GGUF path is the
                // supported entry for Mixture-of-Experts models.
                moe: None,
                qk_norm_over_hidden: false,
            },
            globals,
            layers,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_embedded_specs_parse() {
        let specs = registry();
        assert_eq!(specs.len(), 5);
        assert_eq!(specs[0].name, "qwen3");
        assert_eq!(specs[1].name, "llama");
        assert_eq!(specs[2].name, "mistral");
        assert_eq!(specs[3].name, "olmoe");
        assert_eq!(specs[4].name, "qwen3moe");
        // The MoE specs carry the router + stacked-expert roles.
        assert!(specs[3].roles.iter().any(|r| r.role == WeightRole::FfnGateInp));
        assert!(specs[3].roles.iter().any(|r| r.role == WeightRole::FfnGateExps));
        assert!(specs[3].roles.iter().any(|r| r.role == WeightRole::FfnDownExps));
        // MoE FFN replaces the dense gate/up/down entirely.
        assert!(!specs[3].roles.iter().any(|r| r.role == WeightRole::FfnGate));
        // qwen3 has QK-norm roles, llama does not.
        assert!(specs[0]
            .roles
            .iter()
            .any(|r| r.role == WeightRole::AttnQNorm));
        assert!(!specs[1]
            .roles
            .iter()
            .any(|r| r.role == WeightRole::AttnQNorm));
        // Detect must resolve gguf arch "llama" to the llama spec (declared
        // before mistral, which shares the gguf arch name).
        let first_llama = specs.iter().find(|s| s.gguf_arch == "llama").unwrap();
        assert_eq!(first_llama.name, "llama");
    }

    #[test]
    fn from_hf_resolves_llama_names() {
        let cfg: HfConfig = serde_json::from_str(
            r#"{
                "architectures": ["LlamaForCausalLM"],
                "model_type": "llama",
                "hidden_size": 1536,
                "num_hidden_layers": 2,
                "num_attention_heads": 12,
                "num_key_value_heads": 2,
                "head_dim": 128,
                "intermediate_size": 8960,
                "vocab_size": 32000,
                "max_position_embeddings": 8192
            }"#,
        )
        .unwrap();
        let desc = ModelDescriptor::from_hf(&cfg).unwrap();
        assert_eq!(desc.arch, "llama");
        assert_eq!(desc.layers.len(), 2);
        assert_eq!(
            desc.layers[1][&WeightRole::AttnQ],
            "model.layers.1.self_attn.q_proj.weight"
        );
        assert_eq!(desc.globals[&WeightRole::LmHead], "lm_head.weight");
        assert_eq!(desc.params.n_kv_heads, 2);
        assert_eq!(desc.params.head_dim, 128);
    }

    #[test]
    fn from_hf_tied_embeddings_drop_lm_head() {
        let cfg: HfConfig = serde_json::from_str(
            r#"{
                "architectures": ["Qwen3ForCausalLM"],
                "hidden_size": 1024,
                "num_hidden_layers": 1,
                "num_attention_heads": 16,
                "num_key_value_heads": 8,
                "intermediate_size": 3072,
                "vocab_size": 151936,
                "max_position_embeddings": 40960,
                "tie_word_embeddings": true
            }"#,
        )
        .unwrap();
        let desc = ModelDescriptor::from_hf(&cfg).unwrap();
        assert_eq!(desc.arch, "qwen3");
        assert!(!desc.globals.contains_key(&WeightRole::LmHead));
        assert!(desc.layers[0].contains_key(&WeightRole::AttnKNorm));
    }

    #[test]
    fn pooling_type_maps_gguf_enum() {
        assert_eq!(PoolingType::from_gguf_u32(0), PoolingType::None);
        assert_eq!(PoolingType::from_gguf_u32(1), PoolingType::Mean);
        assert_eq!(PoolingType::from_gguf_u32(2), PoolingType::Cls);
        assert_eq!(PoolingType::from_gguf_u32(3), PoolingType::Last);
        // 4 = rank (reranker head), not an embedding pooler.
        assert_eq!(PoolingType::from_gguf_u32(4), PoolingType::None);
    }

    #[test]
    fn from_hf_has_no_pooling() {
        let cfg: HfConfig = serde_json::from_str(
            r#"{"architectures":["Qwen3ForCausalLM"],"hidden_size":1024,
                "num_hidden_layers":1,"num_attention_heads":16,
                "num_key_value_heads":8,"intermediate_size":3072,
                "vocab_size":151936,"max_position_embeddings":40960}"#,
        )
        .unwrap();
        let desc = ModelDescriptor::from_hf(&cfg).unwrap();
        assert_eq!(desc.params.pooling_type, PoolingType::None);
    }

    /// Detect OLMoE from the real GGUF and assert its MoE metadata. Skipped
    /// cleanly when the test model has not been downloaded.
    #[test]
    fn detect_olmoe_moe_metadata() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../test-models/gguf/olmoe-1b-7b.gguf"
        );
        if !std::path::Path::new(path).exists() {
            eprintln!("skipping: {path} not present");
            return;
        }
        let gguf = Gguf::open(path).expect("open olmoe gguf");
        let desc = ModelDescriptor::detect(&gguf).expect("detect olmoe");
        assert_eq!(desc.arch, "olmoe");
        let moe = desc.params.moe.as_ref().expect("olmoe is MoE");
        assert_eq!(moe.n_experts, 64, "OLMoE has 64 experts");
        assert_eq!(moe.n_experts_used, 8, "OLMoE routes top-8");
        assert_eq!(moe.shared_intermediate_size, 0, "OLMoE has no shared expert");
        // OLMoE normalizes the full query/key vector, not per head.
        assert!(desc.params.qk_norm_over_hidden);
        // Every layer resolved the router + three stacked expert tensors.
        for layer in &desc.layers {
            assert!(layer.contains_key(&WeightRole::FfnGateInp));
            assert!(layer.contains_key(&WeightRole::FfnGateExps));
            assert!(layer.contains_key(&WeightRole::FfnUpExps));
            assert!(layer.contains_key(&WeightRole::FfnDownExps));
            assert!(!layer.contains_key(&WeightRole::FfnGate));
        }
    }

    #[test]
    fn from_hf_unknown_arch_is_unsupported() {
        let cfg: HfConfig = serde_json::from_str(
            r#"{
                "architectures": ["GptOssForCausalLM"],
                "hidden_size": 1024,
                "num_hidden_layers": 1,
                "num_attention_heads": 16,
                "intermediate_size": 3072,
                "vocab_size": 1000,
                "max_position_embeddings": 2048
            }"#,
        )
        .unwrap();
        assert!(matches!(
            ModelDescriptor::from_hf(&cfg),
            Err(ForgeError::Unsupported(_))
        ));
    }
}
