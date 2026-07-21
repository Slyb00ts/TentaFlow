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
    /// Per-token sigmoid gate on the shared-expert output (qwen35moe): a
    /// [hidden] vector producing one logit per token (`ffn_gate_inp_shexp`).
    FfnGateInpShExp,
    /// Gated-DeltaNet (linear-attention) in-projection producing the mixed
    /// q|k|v stream (`attn_qkv`, [hidden, key_dim*2+value_dim]).
    SsmInProj,
    /// Gated-DeltaNet output gate `z` projection (`attn_gate`, [hidden, value_dim]).
    SsmGate,
    /// Depthwise causal conv over the mixed q|k|v stream (`ssm_conv1d`,
    /// [d_conv, conv_dim]).
    SsmConv1d,
    /// DeltaNet time-step bias added before softplus (`ssm_dt.bias`, [dt_rank]).
    SsmDt,
    /// DeltaNet log-decay scale `-exp(A_log)` (`ssm_a`, [dt_rank]).
    SsmA,
    /// DeltaNet per-head beta projection (`ssm_beta`, [hidden, n_v_heads]).
    SsmBeta,
    /// DeltaNet per-head alpha (decay) projection (`ssm_alpha`, [hidden, n_v_heads]).
    SsmAlpha,
    /// DeltaNet output gated-RMSNorm weight over head_v_dim (`ssm_norm`, [head_v_dim]).
    SsmNorm,
    /// DeltaNet output projection (`ssm_out`, [value_dim, hidden]).
    SsmOut,
}

/// Per-layer computation kind in a hybrid (attention + linear-attention) stack.
/// Non-hybrid architectures are all-`Attention`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerKind {
    /// Standard softmax self-attention (paged KV), optionally output-gated.
    Attention,
    /// Gated-DeltaNet linear attention: causal conv + recurrent state scan.
    DeltaNet,
}

/// Gated-DeltaNet / SSM hyperparameters (hybrid architectures only). Head
/// counts are derived: `n_k_heads = n_group`, `n_v_heads = dt_rank`, and both
/// key and value head dimensions equal `d_state`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SsmParams {
    /// Causal depthwise conv kernel width (`ssm.conv_kernel`, e.g. 4).
    pub d_conv: usize,
    /// Total value width across all v-heads (`ssm.inner_size`, e.g. 4096).
    pub d_inner: usize,
    /// Per-head state dimension = key/value head dim (`ssm.state_size`, e.g. 128).
    pub d_state: usize,
    /// DeltaNet value-head count / time-step rank (`ssm.time_step_rank`, e.g. 32).
    pub dt_rank: usize,
    /// DeltaNet key-head count (`ssm.group_count`, e.g. 16).
    pub n_group: usize,
}

impl SsmParams {
    /// Key-head count (== `n_group`).
    pub fn n_k_heads(&self) -> usize {
        self.n_group
    }
    /// Value-head count (== `dt_rank`).
    pub fn n_v_heads(&self) -> usize {
        self.dt_rank
    }
    /// Per-head key/value dimension (== `d_state`).
    pub fn head_dim(&self) -> usize {
        self.d_state
    }
    /// Total key width across key-heads (`d_state * n_group`).
    pub fn key_dim(&self) -> usize {
        self.d_state * self.n_group
    }
    /// Total value width across value-heads (`d_state * dt_rank == d_inner`).
    pub fn value_dim(&self) -> usize {
        self.d_state * self.dt_rank
    }
    /// Channel count of the mixed q|k|v conv stream (`key_dim*2 + value_dim`).
    pub fn conv_dim(&self) -> usize {
        self.key_dim() * 2 + self.value_dim()
    }
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
    include_str!("../arch/qwen35moe.ron"),
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
    /// Gated-DeltaNet / SSM parameters for hybrid architectures (`qwen35moe`);
    /// `None` for pure-attention models.
    pub ssm: Option<SsmParams>,
    /// M-RoPE dimension sections (`rope.dimension_sections`, hybrid Qwen);
    /// `None` for standard RoPE. For text-only positions M-RoPE reduces to
    /// NEOX partial rotary over the first `sum(sections)*2` dims.
    pub rope_sections: Option<[u32; 4]>,
    /// Every `full_attention_interval`-th layer is full attention, the rest
    /// Gated-DeltaNet (hybrid only; 0 when not hybrid). The concrete per-layer
    /// split lives in `ModelDescriptor::layer_kinds`.
    pub full_attention_interval: usize,
    /// The attention Q projection also emits a per-head sigmoid output gate
    /// (qwen35moe): `wq` has width `head_dim * n_heads * 2` and the second half
    /// gates the attention output. `false` for ungated attention.
    pub attn_gated: bool,
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
    /// Per-layer computation kind. All `Attention` for non-hybrid models;
    /// interleaved `Attention`/`DeltaNet` for `qwen35moe`. Index = layer.
    pub layer_kinds: Vec<LayerKind>,
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

        if arch == "qwen35moe" {
            return build_qwen35moe(gguf, spec);
        }

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
                ssm: None,
                rope_sections: None,
                full_attention_interval: 0,
                attn_gated: false,
            },
            globals,
            layers,
            layer_kinds: vec![LayerKind::Attention; block_count],
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
                ssm: None,
                rope_sections: None,
                full_attention_interval: 0,
                attn_gated: false,
            },
            globals,
            layers,
            layer_kinds: vec![LayerKind::Attention; block_count],
        })
    }
}

/// Build the descriptor for the Qwen3.5/3.6 hybrid MoE (`qwen35moe`): an
/// interleaved stack of full-attention and Gated-DeltaNet layers with routed
/// experts + a gated shared expert. Every `full_attention_interval`-th layer
/// (`(idx+1) % interval == 0`) is attention; the rest are DeltaNet. The final
/// `nextn_predict_layers` blocks are MTP/NextN speculation heads and are
/// dropped from the autoregressive stack (basic decode never runs them).
fn build_qwen35moe(gguf: &Gguf, spec: &ArchSpec) -> Result<ModelDescriptor> {
    let key = |suffix: &str| format!("qwen35moe.{suffix}");
    let req_u = |suffix: &str| {
        gguf.get_u64(&key(suffix))
            .map(|v| v as usize)
            .ok_or_else(|| fmt_err(format!("gguf: missing metadata key {}", key(suffix))))
    };

    let block_count_all = req_u("block_count")?;
    let nextn = gguf.get_u64(&key("nextn_predict_layers")).unwrap_or(0) as usize;
    let block_count = block_count_all.saturating_sub(nextn);
    let hidden_size = req_u("embedding_length")?;
    let n_heads = req_u("attention.head_count")?;
    let n_kv_heads = gguf
        .get_u64(&key("attention.head_count_kv"))
        .map(|v| v as usize)
        .unwrap_or(n_heads);
    // Attention head dim is the explicit key length (256 here), independent of
    // hidden/n_heads (q width = n_heads * head_dim can exceed hidden_size).
    let head_dim = req_u("attention.key_length")?;
    // Pure-MoE model: only per-expert / shared FFN widths are declared, no
    // dense feed_forward_length. Fall back to the expert width.
    let feed_forward_length = gguf
        .get_u64(&key("feed_forward_length"))
        .map(|v| v as usize)
        .unwrap_or(0);
    let max_position_embeddings = req_u("context_length")?;
    let rope_theta = gguf.get_f32(&key("rope.freq_base")).unwrap_or(10_000.0);
    let rms_norm_eps = gguf
        .get_f32(&key("attention.layer_norm_rms_epsilon"))
        .unwrap_or(1e-5);

    let full_attention_interval = gguf
        .get_u64(&key("full_attention_interval"))
        .map(|v| v as usize)
        .unwrap_or(4)
        .max(1);

    let ssm = SsmParams {
        d_conv: req_u("ssm.conv_kernel")?,
        d_inner: req_u("ssm.inner_size")?,
        d_state: req_u("ssm.state_size")?,
        dt_rank: req_u("ssm.time_step_rank")?,
        n_group: req_u("ssm.group_count")?,
    };

    let rope_sections = gguf
        .get_array(&key("rope.dimension_sections"))
        .and_then(|a| {
            let v: Vec<u32> = a.iter().filter_map(|e| e.as_u64().map(|x| x as u32)).collect();
            if v.len() >= 4 {
                Some([v[0], v[1], v[2], v[3]])
            } else {
                None
            }
        });

    // Routed experts + always-on gated shared expert.
    let n_experts = req_u("expert_count")?;
    let n_experts_used = req_u("expert_used_count")?;
    let moe_intermediate_size = gguf
        .get_u64(&key("expert_feed_forward_length"))
        .map(|v| v as usize)
        .unwrap_or(feed_forward_length);
    let shared_intermediate_size = gguf
        .get_u64(&key("expert_shared_feed_forward_length"))
        .map(|v| v as usize)
        .unwrap_or(0);
    let moe = Some(MoeParams {
        n_experts,
        n_experts_used,
        moe_intermediate_size,
        // qwen35moe renormalizes the top-k softmax weights (build_moe_ffn
        // norm_w = true in the reference graph).
        norm_topk_prob: true,
        shared_intermediate_size,
    });

    let vocab_size = gguf
        .tensor("token_embd.weight")
        .and_then(|t| t.dims.get(1))
        .map(|&v| v as usize)
        .or_else(|| gguf.get_array("tokenizer.ggml.tokens").map(|a| a.len()))
        .ok_or_else(|| fmt_err("gguf: cannot determine vocab size"))?;

    let mut globals = HashMap::new();
    for role in [WeightRole::TokenEmbd, WeightRole::OutputNorm] {
        let name = spec
            .roles
            .iter()
            .find(|r| r.role == role)
            .map(|r| r.gguf.clone())
            .ok_or_else(|| fmt_err(format!("qwen35moe spec missing global role {role:?}")))?;
        if gguf.tensor(&name).is_none() {
            return Err(fmt_err(format!("qwen35moe: missing global tensor '{name}'")));
        }
        globals.insert(role, name);
    }
    // Untied LM head: present as output.weight, else tie to the embedding.
    let tie_word_embeddings = gguf.tensor("output.weight").is_none();
    if !tie_word_embeddings {
        globals.insert(WeightRole::LmHead, "output.weight".into());
    }

    // Common per-layer roles shared by both attention and DeltaNet layers.
    let insert = |m: &mut HashMap<WeightRole, String>, role: WeightRole, name: String| -> Result<()> {
        if gguf.tensor(&name).is_none() {
            return Err(fmt_err(format!("qwen35moe: missing tensor '{name}'")));
        }
        m.insert(role, name);
        Ok(())
    };

    let mut layers: Vec<HashMap<WeightRole, String>> = Vec::with_capacity(block_count);
    let mut layer_kinds = Vec::with_capacity(block_count);
    for il in 0..block_count {
        let kind = if (il + 1) % full_attention_interval == 0 {
            LayerKind::Attention
        } else {
            LayerKind::DeltaNet
        };
        let mut m = HashMap::new();
        insert(&mut m, WeightRole::AttnNorm, format!("blk.{il}.attn_norm.weight"))?;
        // Post-attention norm feeds the MoE FFN (GGUF: post_attention_norm).
        insert(&mut m, WeightRole::FfnNorm, format!("blk.{il}.post_attention_norm.weight"))?;

        match kind {
            LayerKind::Attention => {
                // Q projection is gated: width = head_dim * n_heads * 2.
                insert(&mut m, WeightRole::AttnQ, format!("blk.{il}.attn_q.weight"))?;
                insert(&mut m, WeightRole::AttnK, format!("blk.{il}.attn_k.weight"))?;
                insert(&mut m, WeightRole::AttnV, format!("blk.{il}.attn_v.weight"))?;
                insert(&mut m, WeightRole::AttnO, format!("blk.{il}.attn_output.weight"))?;
                insert(&mut m, WeightRole::AttnQNorm, format!("blk.{il}.attn_q_norm.weight"))?;
                insert(&mut m, WeightRole::AttnKNorm, format!("blk.{il}.attn_k_norm.weight"))?;
            }
            LayerKind::DeltaNet => {
                insert(&mut m, WeightRole::SsmInProj, format!("blk.{il}.attn_qkv.weight"))?;
                insert(&mut m, WeightRole::SsmGate, format!("blk.{il}.attn_gate.weight"))?;
                insert(&mut m, WeightRole::SsmConv1d, format!("blk.{il}.ssm_conv1d.weight"))?;
                insert(&mut m, WeightRole::SsmDt, format!("blk.{il}.ssm_dt.bias"))?;
                insert(&mut m, WeightRole::SsmA, format!("blk.{il}.ssm_a"))?;
                insert(&mut m, WeightRole::SsmBeta, format!("blk.{il}.ssm_beta.weight"))?;
                insert(&mut m, WeightRole::SsmAlpha, format!("blk.{il}.ssm_alpha.weight"))?;
                insert(&mut m, WeightRole::SsmNorm, format!("blk.{il}.ssm_norm.weight"))?;
                insert(&mut m, WeightRole::SsmOut, format!("blk.{il}.ssm_out.weight"))?;
            }
        }

        // Routed experts + gated shared expert (present on every trunk layer).
        insert(&mut m, WeightRole::FfnGateInp, format!("blk.{il}.ffn_gate_inp.weight"))?;
        insert(&mut m, WeightRole::FfnGateExps, format!("blk.{il}.ffn_gate_exps.weight"))?;
        insert(&mut m, WeightRole::FfnUpExps, format!("blk.{il}.ffn_up_exps.weight"))?;
        insert(&mut m, WeightRole::FfnDownExps, format!("blk.{il}.ffn_down_exps.weight"))?;
        insert(&mut m, WeightRole::FfnGateShExp, format!("blk.{il}.ffn_gate_shexp.weight"))?;
        insert(&mut m, WeightRole::FfnUpShExp, format!("blk.{il}.ffn_up_shexp.weight"))?;
        insert(&mut m, WeightRole::FfnDownShExp, format!("blk.{il}.ffn_down_shexp.weight"))?;
        insert(&mut m, WeightRole::FfnGateInpShExp, format!("blk.{il}.ffn_gate_inp_shexp.weight"))?;

        layers.push(m);
        layer_kinds.push(kind);
    }

    // The MoE expert scratch is sized from intermediate_size; fold the expert
    // and shared-expert FFN widths into it.
    let intermediate_size = moe_intermediate_size.max(shared_intermediate_size);

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
            pooling_type: PoolingType::None,
            moe,
            // qwen35moe attention normalizes each head over head_dim.
            qk_norm_over_hidden: false,
            ssm: Some(ssm),
            rope_sections,
            full_attention_interval,
            attn_gated: true,
        },
        globals,
        layers,
        layer_kinds,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_embedded_specs_parse() {
        let specs = registry();
        assert_eq!(specs.len(), 6);
        assert_eq!(specs[0].name, "qwen3");
        assert_eq!(specs[1].name, "llama");
        assert_eq!(specs[2].name, "mistral");
        assert_eq!(specs[3].name, "olmoe");
        assert_eq!(specs[4].name, "qwen3moe");
        assert_eq!(specs[5].name, "qwen35moe");
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

    /// Detect the Qwen3.6-35B-A3B hybrid MoE from the real GGUF and assert the
    /// interleaved attention/DeltaNet split, SSM params, M-RoPE sections, shared
    /// expert and MTP drop. Skipped cleanly when the model is not present.
    #[test]
    fn detect_qwen35moe_hybrid_metadata() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../test-models/gguf/qwen36-moe.gguf"
        );
        if !std::path::Path::new(path).exists() {
            eprintln!("skipping: {path} not present");
            return;
        }
        let gguf = Gguf::open(path).expect("open qwen36 gguf");
        let desc = ModelDescriptor::detect(&gguf).expect("detect qwen35moe");
        assert_eq!(desc.arch, "qwen35moe");
        // 41 blocks total, 1 MTP head dropped → 40 trunk layers.
        assert_eq!(desc.params.block_count, 40);
        assert_eq!(desc.layer_kinds.len(), 40);
        assert_eq!(desc.params.hidden_size, 2048);
        assert_eq!(desc.params.n_heads, 16);
        assert_eq!(desc.params.n_kv_heads, 2);
        assert_eq!(desc.params.head_dim, 256);
        assert_eq!(desc.params.full_attention_interval, 4);
        assert!((desc.params.rope_theta - 1.0e7).abs() < 1.0);
        assert_eq!(desc.params.rope_sections, Some([11, 11, 10, 0]));
        assert!(desc.params.attn_gated);

        // Hybrid rule: (idx+1) % 4 == 0 → attention, else DeltaNet.
        for (il, &kind) in desc.layer_kinds.iter().enumerate() {
            let want = if (il + 1) % 4 == 0 {
                LayerKind::Attention
            } else {
                LayerKind::DeltaNet
            };
            assert_eq!(kind, want, "layer {il} kind");
        }
        // Layers 0,1,2 are DeltaNet; layer 3 is attention.
        assert_eq!(desc.layer_kinds[0], LayerKind::DeltaNet);
        assert_eq!(desc.layer_kinds[3], LayerKind::Attention);

        let ssm = desc.params.ssm.as_ref().expect("qwen35moe has SSM params");
        assert_eq!(ssm.d_conv, 4);
        assert_eq!(ssm.d_inner, 4096);
        assert_eq!(ssm.d_state, 128);
        assert_eq!(ssm.dt_rank, 32);
        assert_eq!(ssm.n_group, 16);
        assert_eq!(ssm.n_k_heads(), 16);
        assert_eq!(ssm.n_v_heads(), 32);
        assert_eq!(ssm.key_dim(), 2048);
        assert_eq!(ssm.value_dim(), 4096);
        assert_eq!(ssm.conv_dim(), 8192);

        let moe = desc.params.moe.as_ref().expect("qwen35moe is MoE");
        assert_eq!(moe.n_experts, 256);
        assert_eq!(moe.n_experts_used, 8);
        assert_eq!(moe.moe_intermediate_size, 512);
        assert_eq!(moe.shared_intermediate_size, 512);

        // DeltaNet layer 0 resolved its SSM tensors, not attention Q/K/V.
        let l0 = &desc.layers[0];
        assert!(l0.contains_key(&WeightRole::SsmInProj));
        assert!(l0.contains_key(&WeightRole::SsmConv1d));
        assert!(l0.contains_key(&WeightRole::SsmOut));
        assert!(l0.contains_key(&WeightRole::FfnGateInpShExp));
        assert!(!l0.contains_key(&WeightRole::AttnQ));
        // Attention layer 3 resolved Q/K/V + QK-norm, not SSM tensors.
        let l3 = &desc.layers[3];
        assert!(l3.contains_key(&WeightRole::AttnQ));
        assert!(l3.contains_key(&WeightRole::AttnQNorm));
        assert!(!l3.contains_key(&WeightRole::SsmInProj));
        // Every trunk layer carries routed + shared expert weights.
        for m in &desc.layers {
            assert!(m.contains_key(&WeightRole::FfnGateExps));
            assert!(m.contains_key(&WeightRole::FfnDownShExp));
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
