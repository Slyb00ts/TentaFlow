// ===== File: arch.rs — declarative architecture registry: tensor roles + hyperparams =====
//
// Each supported architecture is described by an embedded RON file mapping
// tensor-name templates (`{layer}` placeholder) to semantic weight roles for
// both GGUF (`blk.N.attn_q.weight`) and HF (`model.layers.N.self_attn.
// q_proj.weight`) naming. `ModelDescriptor` resolves the templates into a
// concrete role → tensor-name map plus unified hyperparams.

use std::collections::HashMap;
use std::sync::OnceLock;

use forge_types::{ForgeError, QuantKind, Result};
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
    /// Dzielniki częstotliwości rope (`rope_freqs`) używane przez warstwy
    /// globalne Gemmy 4 — rope proporcjonalne. Tensor f32 o długości head_dim/2.
    RopeFreqs,
    /// Norma PO bloku uwagi, przed rezydualem (rodzina Gemma — „sandwich norm”).
    PostAttnNorm,
    /// Norma PO bloku FFN, przed rezydualem (rodzina Gemma).
    PostFfwNorm,
    /// Skalar mnożący wyjście warstwy (`layer_output_scale`, Gemma 4).
    LayerOutputScale,
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

    // --- DeepSeek V4: uwaga latentna z projekcjami LoRA ---
    /// Zejście LoRA dla Q: [hidden, q_lora_rank].
    AttnQA,
    /// Wyjście LoRA dla Q: [q_lora_rank, n_heads*head_dim].
    AttnQB,
    /// Wspólna projekcja KV jednej głowicy: [hidden, head_dim].
    AttnKV,
    /// Norma RMS na skompresowanym KV.
    AttnKvNorm,
    /// Zejście LoRA wyjścia uwagi, grupowane: [n_heads*head_dim/o_groups,
    /// o_groups*o_lora_rank].
    AttnOA,
    /// Wyjście LoRA uwagi: [o_groups*o_lora_rank, hidden].
    AttnOB,
    /// Logit „kotwicy" uwagi, jeden na głowicę (attention sink).
    AttnSink,

    // --- DeepSeek V4: kompresor strumienia KV ---
    /// Kodowanie pozycji wewnątrz okna kompresji.
    CompressorApe,
    CompressorNorm,
    /// Projekcja bramki wyznaczającej wagi poolingu.
    CompressorWGate,
    /// Projekcja kompresowanego KV.
    CompressorWkv,

    // --- DeepSeek V4: indekser rzadkiej uwagi (własny kompresor) ---
    IndexerCompressorApe,
    IndexerCompressorNorm,
    IndexerCompressorWGate,
    IndexerCompressorWkv,
    /// Waga na głowicę przy sumowaniu wyników indeksera.
    IndexerWeightsProj,
    /// Wyjście LoRA dla zapytań indeksera.
    IndexerWqB,

    // --- DeepSeek V4: routing ---
    /// Bias dodawany do logitów routera przed wyborem top-k.
    FfnGateBias,
    /// Tablica token -> ekspert dla warstw z routingiem haszowanym.
    FfnGateTid2Eid,

    // --- DeepSeek V4: modulacja warunkowana haszem ---
    HcAttnBase,
    HcAttnFn,
    HcAttnScale,
    HcFfnBase,
    HcFfnFn,
    HcFfnScale,
    HcHeadBase,
    HcHeadFn,
    HcHeadScale,
}

impl Hyperparams {
    /// `head_dim` warstwy `layer`. Architektury z naprzemienną geometrią
    /// (Gemma 4) mają inny wymiar głowicy w warstwach z oknem niż w globalnych;
    /// pola skalarne opisują warstwy globalne, więc to jedyny poprawny sposób
    /// pytania o wymiar konkretnej warstwy.
    pub fn head_dim_at(&self, layer: usize) -> usize {
        match &self.alt_attn {
            Some(alt) if alt.sliding.get(layer).copied().unwrap_or(false) => alt.head_dim_swa,
            _ => self.head_dim,
        }
    }

    /// Liczba głowic KV warstwy `layer` — jak wyżej.
    pub fn n_kv_heads_at(&self, layer: usize) -> usize {
        match &self.alt_attn {
            Some(alt) if alt.sliding.get(layer).copied().unwrap_or(false) => alt.n_kv_heads_swa,
            _ => self.n_kv_heads,
        }
    }

    /// Skala logitów uwagi dla warstwy `layer`.
    pub fn attn_scale_at(&self, layer: usize) -> f32 {
        match self.attn_logit_scale {
            Some(scale) => scale,
            None => 1.0 / (self.head_dim_at(layer) as f32).sqrt(),
        }
    }

    /// Największy `head_dim` w modelu — do wymiarowania buforów aktywacji,
    /// które muszą pomieścić każdą warstwę.
    pub fn max_head_dim(&self) -> usize {
        match &self.alt_attn {
            Some(alt) => self.head_dim.max(alt.head_dim_swa),
            None => self.head_dim,
        }
    }

    /// Największa szerokość projekcji Q w modelu.
    pub fn max_q_dim(&self) -> usize {
        self.n_heads * self.max_head_dim()
    }

    /// Największa szerokość projekcji K (lub V) w modelu. Liczona per warstwa,
    /// bo największa liczba głowic i największy `head_dim` mogą być w RÓŻNYCH
    /// warstwach — iloczyn maksimów zawyżyłby bufor.
    pub fn max_kv_dim(&self) -> usize {
        let global = self.n_kv_heads * self.head_dim;
        match &self.alt_attn {
            Some(alt) => global.max(alt.n_kv_heads_swa * alt.head_dim_swa),
            None => global,
        }
    }

    /// Geometria, na którą rozmiarowany jest cache KV: musi pomieścić
    /// NAJSZERSZĄ warstwę modelu. Przy naprzemiennej uwadze (Gemma 4) warstwy
    /// różnią się liczbą głowic KV i szerokością głowicy, a każda adresuje swój
    /// slab własnymi wymiarami — mieści się wtedy w tym zakresie. Dla modeli
    /// jednorodnych zwraca dokładnie `n_kv_heads` i `head_dim`.
    pub fn kv_cache_head_dim(&self) -> usize {
        self.max_head_dim()
    }

    pub fn kv_cache_heads(&self) -> usize {
        self.max_kv_dim().div_ceil(self.max_head_dim())
    }

    /// Podstawa rope warstwy `layer`. Gemma 4 ma dwie: 1e4 dla warstw okiennych
    /// i 1e6 dla globalnych.
    pub fn rope_theta_at(&self, layer: usize) -> f32 {
        match &self.alt_attn {
            Some(alt) if alt.sliding.get(layer).copied().unwrap_or(false) => alt.rope_theta_swa,
            _ => self.rope_theta,
        }
    }

    /// Czy warstwa `layer` używa rope proporcjonalnego (tensor `rope_freqs`).
    /// Dotyczy wyłącznie warstw globalnych architektur z naprzemienną uwagą.
    pub fn rope_proportional_at(&self, layer: usize) -> bool {
        match &self.alt_attn {
            Some(alt) => !alt.sliding.get(layer).copied().unwrap_or(false),
            None => false,
        }
    }

    /// Czy warstwa `layer` w ogóle ma projekcję V. Warstwy globalne Gemmy 4 jej
    /// nie mają i używają wtedy K jako V (potwierdzone w llama.cpp).
    pub fn has_v_proj(&self, layer: usize) -> bool {
        match &self.alt_attn {
            Some(alt) => alt.sliding.get(layer).copied().unwrap_or(true),
            None => true,
        }
    }
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
    /// `None` dla ról, które istnieją wyłącznie w checkpointach HF.
    #[serde(default)]
    pub gguf: Option<String>,
    /// HF safetensors tensor-name template.
    pub hf: String,
    pub per_layer: bool,
    /// Rola rozwijana na każdego eksperta: nazwa zachowuje `{expert}`, a
    /// konkretny indeks podstawia loader. Bez tego 43 warstwy po 256 ekspertów
    /// oznaczałyby 132 tysiące nazw w opisie modelu.
    #[serde(default)]
    pub per_expert: bool,
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
    include_str!("../arch/qwen35.ron"),
    include_str!("../arch/gemma4.ron"),
    include_str!("../arch/deepseek_v4.ron"),
];

/// Embedded specs are compile-time assets of this crate, so a parse failure
/// is a build defect, not untrusted input; a unit test guards every file.
pub fn registry() -> &'static [ArchSpec] {
    static REGISTRY: OnceLock<Vec<ArchSpec>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        ARCH_SOURCES
            .iter()
            .map(|src| {
                // IMPLICIT_SOME pozwala pisać `gguf: "nazwa"` zamiast
                // `gguf: Some("nazwa")` w każdej z kilkudziesięciu ról.
                ron::Options::default()
                    .with_default_extension(ron::extensions::Extensions::IMPLICIT_SOME)
                    .from_str(src)
                    .expect("embedded arch spec must parse")
            })
            .collect()
    })
}

/// Nieliniowość bramki FFN. SwiGLU (`silu`) jest domyślne; rodzina Gemma używa
/// GeGLU z przybliżeniem tanh, a różnica jest widoczna w logitach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FfnActivation {
    #[default]
    SiLU,
    GeLUTanh,
}

/// Naprzemienna geometria uwagi: część warstw ma okno przesuwne i WŁASNY
/// `head_dim` oraz liczbę głowic KV, część jest globalna. Gemma 4 powtarza
/// wzorzec [lokalna x5, globalna x1], przy czym warstwy lokalne mają
/// head_dim 256 i 8 głowic KV, a globalne head_dim 512 i jedną głowicę.
///
/// Wartości spoza wzorca (`head_dim`, `n_kv_heads` w `Hyperparams`) opisują
/// warstwy GLOBALNE, żeby modele bez tego pola czytały się bez zmian.
#[derive(Debug, Clone, PartialEq)]
pub struct AltAttnParams {
    /// `true` = warstwa z oknem przesuwnym; długość równa liczbie warstw.
    pub sliding: Vec<bool>,
    /// Rozmiar okna w tokenach.
    pub window: usize,
    pub head_dim_swa: usize,
    pub n_kv_heads_swa: usize,
    pub rope_theta_swa: f32,
}

/// Czyta naprzemienną geometrię uwagi z metadanych GGUF; `None`, gdy model jej
/// nie deklaruje (wtedy wszystkie warstwy mają jedną geometrię).
///
/// Wzorce w GGUF są KRÓTKIE i powtarzalne: Gemma 4 ma 48 warstw, ale
/// `sliding_window_pattern` liczy 6 pozycji, a `head_count_kv` też 6 — trzeba je
/// rozwinąć modulo długość, inaczej wychodzi zła geometria od siódmej warstwy.
fn parse_alt_attn(
    gguf: &Gguf,
    arch: &str,
    block_count: usize,
) -> Option<AltAttnParams> {
    let key = |suffix: &str| format!("{arch}.{suffix}");
    let pattern = gguf.get_array(&key("attention.sliding_window_pattern"))?;
    if pattern.is_empty() {
        return None;
    }
    let flags: Vec<bool> = pattern.iter().filter_map(|v| v.as_bool()).collect();
    if flags.len() != pattern.len() {
        return None;
    }
    let sliding: Vec<bool> = (0..block_count)
        .map(|layer| flags[layer % flags.len()])
        .collect();
    let window = gguf.get_u64(&key("attention.sliding_window"))? as usize;
    let head_dim_swa = gguf.get_u64(&key("attention.key_length_swa"))? as usize;
    // Liczba głowic KV jest tablicą o tej samej długości co wzorzec: pozycje
    // lokalne i globalne mają różne wartości (u Gemmy 8 wobec 1).
    let kv = gguf.get_array(&key("attention.head_count_kv"))?;
    let kv: Vec<usize> = kv.iter().filter_map(|v| v.as_u64()).map(|v| v as usize).collect();
    if kv.len() != flags.len() {
        return None;
    }
    let n_kv_heads_swa = flags
        .iter()
        .zip(&kv)
        .find(|(local, _)| **local)
        .map(|(_, heads)| *heads)?;
    let rope_theta_swa = gguf
        .get_f32(&key("rope.freq_base_swa"))
        .unwrap_or_else(|| gguf.get_f32(&key("rope.freq_base")).unwrap_or(10000.0));
    Some(AltAttnParams {
        sliding,
        window,
        head_dim_swa,
        n_kv_heads_swa,
        rope_theta_swa,
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

/// Parametry swoiste dla DeepSeeka V4: uwaga latentna z projekcjami LoRA,
/// dwustrumieniowy KV z kompresorem, indekser rzadkiej uwagi i modulacja
/// warunkowana haszem. Wszystkie są bez odpowiednika w pozostałych
/// architekturach, więc siedzą w osobnym bloku zamiast rozlewać się po
/// `Hyperparams`.
#[derive(Debug, Clone, PartialEq)]
pub struct DeepseekV4Params {
    /// Ranga zejścia LoRA dla Q.
    pub q_lora_rank: usize,
    /// Ranga LoRA wyjścia uwagi (na grupę).
    pub o_lora_rank: usize,
    /// Na ile grup dzielone jest wyjście uwagi przed projekcją.
    pub o_groups: usize,
    /// Ile wymiarów głowicy obejmuje rope; reszta zostaje bez pozycji.
    pub rope_head_dim: usize,
    /// Szerokość okna przesuwnego uwagi.
    pub window_size: usize,
    /// Stopień kompresji KV per warstwa; 0 = warstwa bez kompresora.
    pub compress_ratios: Vec<usize>,
    /// Baza rope skompresowanego strumienia KV.
    pub compress_rope_theta: f32,
    pub index_n_heads: usize,
    pub index_head_dim: usize,
    pub index_topk: usize,
    /// Ile warstw routuje przez tablicę token->ekspert zamiast wyuczonej bramki.
    pub n_hash_layers: usize,
    /// Funkcja punktująca bramki routera (`sqrtsoftplus` w tym checkpoincie).
    pub scoring_func: String,
    /// Mnożnik wag routingu po wyborze top-k.
    pub routed_scaling_factor: f32,
    /// Górne ograniczenie bramki SwiGLU (0 = brak).
    pub swiglu_limit: f32,
}

impl DeepseekV4Params {
    /// Czy warstwa ma kompresor strumienia KV.
    pub fn has_compressor(&self, layer: usize) -> bool {
        self.compress_ratios.get(layer).is_some_and(|r| *r != 0)
    }

    /// Indekser istnieje tylko przy najgęstszej kompresji (ratio 4); warstwy o
    /// ratio 128 czytają skompresowany strumień w całości.
    pub fn has_indexer(&self, layer: usize) -> bool {
        self.compress_ratios.get(layer) == Some(&4)
    }
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
    /// Tokeny, których logity model MUSI maskować do -inf. Checkpointy Gemmy 4
    /// przypisują wysokie logity tokenom `<image|>`/`<audio|>`, więc bez maski
    /// greedy wybiera je i generacja się rozjeżdża (referencja llama.cpp wstawia
    /// tę maskę jako wejście grafu).
    pub suppress_tokens: Vec<u32>,
    /// V przechodzi CZYSTĄ normalizację RMS (bez wagi) przed zapisem do cache —
    /// rodzina Gemma. Realizowane wektorem jedynek, żeby nie mnożyć kerneli.
    pub v_rms_norm: bool,
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
    /// Nieliniowość bramki FFN (SwiGLU domyślnie, GeGLU w rodzinie Gemma).
    pub ffn_activation: FfnActivation,
    /// Naprzemienna geometria uwagi (okno przesuwne + własny head_dim/KV);
    /// `None`, gdy wszystkie warstwy mają tę samą geometrię.
    pub alt_attn: Option<AltAttnParams>,
    /// Ograniczenie logitów `softcap * tanh(x / softcap)` przed samplingiem
    /// (Gemma). 0 = wyłączone.
    pub final_logit_softcap: f32,
    /// Jawna skala logitów uwagi. `None` = domyślne `1/sqrt(head_dim)`.
    /// Gemma 4 używa 1,0 (w referencji `f_attention_scale = 1.0f`), więc bez
    /// tego pola model liczyłby uwagę na skali mniejszej ~22x.
    pub attn_logit_scale: Option<f32>,
    /// `Some` tylko dla DeepSeeka V4.
    pub deepseek_v4: Option<DeepseekV4Params>,
    /// Mnożnik embeddingu wejściowego. `None` = brak. Rodzina Gemma mnoży przez
    /// `sqrt(hidden_size)` (tylko dla wejścia tokenowego).
    pub embd_scale: Option<f32>,
}

/// Rola tensora należącego do jednej warstwy NextN.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MtpWeightRole {
    Embedding,
    SharedHead,
    AttnK,
    AttnKNorm,
    AttnNorm,
    AttnO,
    AttnQ,
    AttnQNorm,
    AttnV,
    FfnDown,
    FfnGate,
    FfnUp,
    FfnNorm,
    EhProj,
    ENorm,
    HNorm,
    SharedHeadNorm,
    FfnGateInp,
    FfnGateExps,
    FfnUpExps,
    FfnDownExps,
    FfnGateShExp,
    FfnUpShExp,
    FfnDownShExp,
    FfnGateInpShExp,
}

/// Zakres bloków NextN zapisanych po trunku modelu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MtpDescriptor {
    pub first_block: usize,
    pub block_count: usize,
    pub layers: Vec<HashMap<MtpWeightRole, String>>,
    pub share_target_embedding: bool,
    pub share_target_output: bool,
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
    /// Bloki MTP są opisane osobno i nie wchodzą do podstawowego forwardu.
    pub mtp: Option<MtpDescriptor>,
}

fn expand(template: &str, layer: usize) -> String {
    template.replace("{layer}", &layer.to_string())
}

fn checked_block_counts(gguf: &Gguf, key: &str) -> Result<(usize, usize)> {
    let block_count_all = gguf
        .get_u64(&format!("{key}.block_count"))
        .ok_or_else(|| fmt_err(format!("gguf: missing metadata key {key}.block_count")))?;
    let block_count_all = usize::try_from(block_count_all)
        .map_err(|_| fmt_err("gguf: block_count does not fit usize"))?;
    if block_count_all > gguf.tensors().len() {
        return Err(fmt_err(format!(
            "gguf: block_count {block_count_all} exceeds tensor count {}",
            gguf.tensors().len()
        )));
    }
    let nextn = gguf
        .get_u64(&format!("{key}.nextn_predict_layers"))
        .unwrap_or(0);
    let nextn = usize::try_from(nextn)
        .map_err(|_| fmt_err("gguf: nextn_predict_layers does not fit usize"))?;
    let block_count = block_count_all.checked_sub(nextn).ok_or_else(|| {
        fmt_err(format!(
            "gguf: nextn_predict_layers {nextn} exceeds block_count {block_count_all}"
        ))
    })?;
    if nextn > 0 && block_count == 0 {
        return Err(fmt_err(
            "gguf: nextn_predict_layers nie może obejmować całego trunku",
        ));
    }
    Ok((block_count, nextn))
}

fn required_mtp_tensor(
    gguf: &Gguf,
    weights: &mut HashMap<MtpWeightRole, String>,
    role: MtpWeightRole,
    name: String,
) -> Result<()> {
    if gguf.tensor(&name).is_none() {
        return Err(fmt_err(format!("gguf: MTP requires tensor '{name}'")));
    }
    weights.insert(role, name);
    Ok(())
}

fn optional_mtp_tensor(
    gguf: &Gguf,
    weights: &mut HashMap<MtpWeightRole, String>,
    role: MtpWeightRole,
    name: String,
) {
    if gguf.tensor(&name).is_some() {
        weights.insert(role, name);
    }
}

fn build_dense_mtp(
    gguf: &Gguf,
    spec: &ArchSpec,
    first_block: usize,
    block_count: usize,
) -> Result<Option<MtpDescriptor>> {
    if block_count == 0 {
        return Ok(None);
    }
    let role_map = [
        (WeightRole::AttnK, MtpWeightRole::AttnK),
        (WeightRole::AttnKNorm, MtpWeightRole::AttnKNorm),
        (WeightRole::AttnNorm, MtpWeightRole::AttnNorm),
        (WeightRole::AttnO, MtpWeightRole::AttnO),
        (WeightRole::AttnQ, MtpWeightRole::AttnQ),
        (WeightRole::AttnQNorm, MtpWeightRole::AttnQNorm),
        (WeightRole::AttnV, MtpWeightRole::AttnV),
        (WeightRole::FfnDown, MtpWeightRole::FfnDown),
        (WeightRole::FfnGate, MtpWeightRole::FfnGate),
        (WeightRole::FfnUp, MtpWeightRole::FfnUp),
        (WeightRole::FfnNorm, MtpWeightRole::FfnNorm),
    ];
    let mut layers = Vec::with_capacity(block_count);
    for block in first_block..first_block + block_count {
        let mut weights = HashMap::with_capacity(15);
        for (weight_role, mtp_role) in role_map {
            let template = spec
                .roles
                .iter()
                .find(|entry| entry.role == weight_role && entry.per_layer)
                .ok_or_else(|| {
                    fmt_err(format!(
                        "{} spec missing MTP role {weight_role:?}",
                        spec.name
                    ))
                })?;
            let gguf_template = template.gguf.as_deref().ok_or_else(|| {
                fmt_err(format!(
                    "{} spec role {weight_role:?} has no GGUF name",
                    spec.name
                ))
            })?;
            required_mtp_tensor(gguf, &mut weights, mtp_role, expand(gguf_template, block))?;
        }
        for (role, suffix) in [
            (MtpWeightRole::EhProj, "eh_proj.weight"),
            (MtpWeightRole::ENorm, "enorm.weight"),
            (MtpWeightRole::HNorm, "hnorm.weight"),
            (MtpWeightRole::SharedHeadNorm, "shared_head_norm.weight"),
        ] {
            required_mtp_tensor(
                gguf,
                &mut weights,
                role,
                format!("blk.{block}.nextn.{suffix}"),
            )?;
        }
        optional_mtp_tensor(
            gguf,
            &mut weights,
            MtpWeightRole::Embedding,
            format!("blk.{block}.nextn.embed_tokens.weight"),
        );
        optional_mtp_tensor(
            gguf,
            &mut weights,
            MtpWeightRole::SharedHead,
            format!("blk.{block}.nextn.shared_head_head.weight"),
        );
        layers.push(weights);
    }
    Ok(Some(MtpDescriptor {
        first_block,
        block_count,
        layers,
        share_target_embedding: true,
        share_target_output: true,
    }))
}

fn build_moe_mtp(
    gguf: &Gguf,
    first_block: usize,
    block_count: usize,
) -> Result<Option<MtpDescriptor>> {
    if block_count == 0 {
        return Ok(None);
    }
    let mut layers = Vec::with_capacity(block_count);
    for block in first_block..first_block + block_count {
        let mut weights = HashMap::with_capacity(20);
        for (role, suffix) in [
            (MtpWeightRole::AttnK, "attn_k.weight"),
            (MtpWeightRole::AttnKNorm, "attn_k_norm.weight"),
            (MtpWeightRole::AttnNorm, "attn_norm.weight"),
            (MtpWeightRole::AttnO, "attn_output.weight"),
            (MtpWeightRole::AttnQ, "attn_q.weight"),
            (MtpWeightRole::AttnQNorm, "attn_q_norm.weight"),
            (MtpWeightRole::AttnV, "attn_v.weight"),
            (MtpWeightRole::FfnGateInp, "ffn_gate_inp.weight"),
            (MtpWeightRole::FfnGateExps, "ffn_gate_exps.weight"),
            (MtpWeightRole::FfnUpExps, "ffn_up_exps.weight"),
            (MtpWeightRole::FfnDownExps, "ffn_down_exps.weight"),
            (MtpWeightRole::FfnGateShExp, "ffn_gate_shexp.weight"),
            (MtpWeightRole::FfnUpShExp, "ffn_up_shexp.weight"),
            (MtpWeightRole::FfnDownShExp, "ffn_down_shexp.weight"),
            (MtpWeightRole::FfnGateInpShExp, "ffn_gate_inp_shexp.weight"),
            (MtpWeightRole::FfnNorm, "post_attention_norm.weight"),
        ] {
            required_mtp_tensor(gguf, &mut weights, role, format!("blk.{block}.{suffix}"))?;
        }
        for (role, suffix) in [
            (MtpWeightRole::EhProj, "eh_proj.weight"),
            (MtpWeightRole::ENorm, "enorm.weight"),
            (MtpWeightRole::HNorm, "hnorm.weight"),
            (MtpWeightRole::SharedHeadNorm, "shared_head_norm.weight"),
        ] {
            required_mtp_tensor(
                gguf,
                &mut weights,
                role,
                format!("blk.{block}.nextn.{suffix}"),
            )?;
        }
        optional_mtp_tensor(
            gguf,
            &mut weights,
            MtpWeightRole::Embedding,
            format!("blk.{block}.nextn.embed_tokens.weight"),
        );
        optional_mtp_tensor(
            gguf,
            &mut weights,
            MtpWeightRole::SharedHead,
            format!("blk.{block}.nextn.shared_head_head.weight"),
        );
        layers.push(weights);
    }
    Ok(Some(MtpDescriptor {
        first_block,
        block_count,
        layers,
        share_target_embedding: true,
        share_target_output: true,
    }))
}

/// Sprawdza, czy zakres etapu pipeline'u mieści się w modelu. Wydzielone z
/// `restrict_layers`, żeby dało się to przetestować bez budowania całego
/// deskryptora — sama arytmetyka granic jest tu jedyną rzeczą, która może się
/// pomylić.
pub fn check_pipeline_range(total: usize, first: usize, count: usize) -> Result<()> {
    if count == 0 {
        return Err(fmt_err("etap pipeline'u bez warstw"));
    }
    if first + count > total {
        return Err(fmt_err(format!(
            "zakres warstw {first}..{} wychodzi poza model o {total} warstwach",
            first + count
        )));
    }
    Ok(())
}

impl ModelDescriptor {
    /// Ogranicza model do zakresu warstw `[first, first + count)`.
    ///
    /// Podstawa pipeline parallel: każdy etap ładuje WYŁĄCZNIE swoje warstwy,
    /// więc model większy od jednej karty mieści się na kilku. Etap, który nie
    /// zawiera warstwy zerowej, nie potrzebuje embeddingu, a etap bez ostatniej
    /// — normy końcowej i głowy; o tym decyduje już wołający, bo tylko on wie,
    /// czy jest pierwszy albo ostatni.
    pub fn restrict_layers(&mut self, first: usize, count: usize) -> Result<()> {
        check_pipeline_range(self.layers.len(), first, count)?;
        self.layers = self.layers.drain(first..first + count).collect();
        if !self.layer_kinds.is_empty() {
            self.layer_kinds = self.layer_kinds.drain(first..first + count).collect();
        }
        self.params.block_count = count;
        Ok(())
    }


    /// Czy model używa RoPE PRZEPLATANEGO (pary `(2i, 2i+1)`) zamiast NeoX
    /// (pary `(i, i + d/2)`).
    ///
    /// Rodzina Llama zapisuje w GGUF wagi w układzie, dla którego llama.cpp
    /// stosuje rotację przeplataną; Qwen, Gemma i DeepSeek używają NeoX.
    /// Pomylenie tych dwóch definicji nie psuje pozycji zerowej (kąt = 0), więc
    /// model odpowiada poprawnie na prompt jednotokenowy i rozjeżdża się dopiero
    /// od drugiego tokenu — dokładnie tak objawiał się błąd znaleziony
    /// 2026-07-28 przez porównanie z `llama-eval-callback`.
    pub fn rope_interleaved(&self) -> bool {
        matches!(self.arch.as_str(), "llama" | "mistral")
    }


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

        if matches!(arch, "qwen35" | "qwen35moe") {
            return build_qwen35_hybrid(gguf, spec);
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
        let (block_count, nextn) = checked_block_counts(gguf, arch)?;
        let mtp = build_dense_mtp(gguf, spec, block_count, nextn)?;
        let hidden_size = req_u("embedding_length")?;
        let n_heads = req_u("attention.head_count")?;
        let alt_attn = parse_alt_attn(gguf, &spec.gguf_arch, block_count);
        // Przy naprzemiennej geometrii `head_count_kv` jest TABLICĄ; skalary
        // `Hyperparams` opisują wtedy warstwy globalne (patrz `AltAttnParams`).
        let n_kv_heads = match (&alt_attn, gguf.get_array(&key("attention.head_count_kv"))) {
            (Some(alt), Some(values)) => {
                let heads: Vec<usize> = values
                    .iter()
                    .filter_map(|v| v.as_u64())
                    .map(|v| v as usize)
                    .collect();
                alt.sliding
                    .iter()
                    .zip(heads.iter().cycle())
                    .find(|(local, _)| !**local)
                    .map(|(_, h)| *h)
                    .unwrap_or(n_heads)
            }
            _ => gguf
                .get_u64(&key("attention.head_count_kv"))
                .map(|v| v as usize)
                .unwrap_or(n_heads),
        };
        let head_dim = gguf
            .get_u64(&key("attention.key_length"))
            .map(|v| v as usize)
            .unwrap_or(hidden_size / n_heads.max(1));
        let final_logit_softcap = gguf
            .get_f32(&key("final_logit_softcapping"))
            .unwrap_or(0.0);
        // GGUF nie nosi typu aktywacji; rodzina Gemma ma GeGLU z tanh, reszta
        // obsługiwanych architektur SwiGLU.
        let ffn_activation = if spec.gguf_arch.starts_with("gemma") {
            FfnActivation::GeLUTanh
        } else {
            FfnActivation::SiLU
        };
        // Rodzina Gemma: uwaga bez dzielenia przez sqrt(head_dim) i embedding
        // przemnożony przez sqrt(hidden). Oba potwierdzone w implementacji
        // wzorcowej i oba są niewidoczne w metadanych GGUF.
        let (attn_logit_scale, embd_scale) = if spec.gguf_arch.starts_with("gemma") {
            (Some(1.0f32), Some((hidden_size as f32).sqrt()))
        } else {
            (None, None)
        };
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
            // Rola bez nazwy GGUF istnieje wyłącznie w checkpointach HF.
            let Some(template) = role.gguf.as_deref() else {
                continue;
            };
            if role.per_layer {
                for (layer, map) in layers.iter_mut().enumerate() {
                    let name = expand(template, layer);
                    if gguf.tensor(&name).is_some() {
                        map.insert(role.role, name);
                    } else if role.required {
                        return Err(fmt_err(format!(
                            "gguf: arch '{}' requires tensor '{name}' which is missing",
                            spec.name
                        )));
                    }
                }
            } else if gguf.tensor(template).is_some() {
                globals.insert(role.role, template.to_string());
            } else if role.required {
                return Err(fmt_err(format!(
                    "gguf: arch '{}' requires tensor '{template}' which is missing",
                    spec.name
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
                .ok_or_else(|| {
                    fmt_err(format!(
                        "gguf: MoE model missing {}",
                        key("expert_used_count")
                    ))
                })?;
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
        // Porównanie musi używać head_dim WARSTWY 0 — przy naprzemiennej
        // geometrii (Gemma 4) warstwy okienne mają węższe głowice niż globalne,
        // a pomyłka tutaj normalizuje całą projekcję wagą długości jednej
        // głowicy i czyta poza bufor.
        let head_dim_layer0 = match &alt_attn {
            Some(alt) if alt.sliding.first().copied().unwrap_or(false) => alt.head_dim_swa,
            _ => head_dim,
        };
        let qk_norm_over_hidden = layers
            .first()
            .and_then(|m| m.get(&WeightRole::AttnQNorm))
            .and_then(|n| gguf.tensor(n))
            .map(|t| {
                let numel: usize = t.dims.iter().map(|&d| d as usize).product();
                numel != head_dim_layer0
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
                v_rms_norm: spec.gguf_arch.starts_with("gemma"),
                suppress_tokens: gguf
                    .get_array("tokenizer.ggml.suppress_tokens")
                    .map(|a| a.iter().filter_map(|v| v.as_u64()).map(|v| v as u32).collect())
                    .unwrap_or_default(),
                ssm: None,
                rope_sections: None,
                full_attention_interval: 0,
                attn_gated: false,
            ffn_activation,
            alt_attn,
            final_logit_softcap,
            attn_logit_scale,
            embd_scale,
            deepseek_v4: None,
            },
            globals,
            layers,
            layer_kinds: vec![LayerKind::Attention; block_count],
            mtp,
        })
    }

    /// Build a descriptor from an HF config.json (safetensors-side naming).
    /// Usuwa role opcjonalne, których tensorów nie ma w checkpoincie.
    ///
    /// Ścieżka GGUF sprawdza obecność przy budowie opisu, bo ma tabelę
    /// tensorów pod ręką; ścieżka HF widzi tylko `config.json`, więc deklaruje
    /// wszystkie role każdej warstwy. Dla architektur, w których część warstw
    /// nie ma kompresora, indeksera czy tablicy routingu, opis obiecywałby
    /// wtedy tensory, po które loader sięgnąłby na darmo.
    pub fn prune_absent_optional(&mut self, exists: impl Fn(&str) -> bool) {
        let Some(spec) = registry().iter().find(|s| s.name == self.arch) else {
            return;
        };
        let optional: HashMap<WeightRole, bool> = spec
            .roles
            .iter()
            .map(|role| (role.role, role.per_expert))
            .filter(|(role, _)| {
                spec.roles
                    .iter()
                    .any(|entry| entry.role == *role && !entry.required)
            })
            .collect();
        for layer in &mut self.layers {
            layer.retain(|role, template| match optional.get(role) {
                // Rola per ekspert jest szablonem — obecność sprawdzamy na
                // eksperice zero, bo albo warstwa ma komplet, albo żadnego.
                Some(true) => exists(&template.replace("{expert}", "0")),
                Some(false) => exists(template),
                None => true,
            });
        }
        self.globals.retain(|role, name| match optional.get(role) {
            Some(_) => exists(name),
            None => true,
        });
    }

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
                    // Rola per ekspert zostaje szablonem z `{expert}`: loader
                    // podstawia indeks sam, bo inaczej opis modelu trzymałby
                    // sto tysięcy nazw.
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
                deepseek_v4: (spec.name == "deepseek_v4").then(|| DeepseekV4Params {
                    q_lora_rank: config.q_lora_rank.unwrap_or(0),
                    o_lora_rank: config.o_lora_rank.unwrap_or(0),
                    o_groups: config.o_groups.unwrap_or(1),
                    rope_head_dim: config.qk_rope_head_dim.unwrap_or(0),
                    window_size: config.sliding_window.unwrap_or(0),
                    compress_ratios: config.compress_ratios.clone(),
                    compress_rope_theta: config.compress_rope_theta.unwrap_or(config.rope_theta),
                    index_n_heads: config.index_n_heads.unwrap_or(0),
                    index_head_dim: config.index_head_dim.unwrap_or(0),
                    index_topk: config.index_topk.unwrap_or(0),
                    n_hash_layers: config.num_hash_layers,
                    scoring_func: config
                        .scoring_func
                        .clone()
                        .unwrap_or_else(|| "softmax".to_string()),
                    routed_scaling_factor: config.routed_scaling_factor.unwrap_or(1.0),
                    swiglu_limit: config.swiglu_limit.unwrap_or(0.0),
                }),
                moe: config.n_routed_experts.map(|n_experts| MoeParams {
                    n_experts,
                    n_experts_used: config.num_experts_per_tok.unwrap_or(1),
                    moe_intermediate_size: config.moe_intermediate_size.unwrap_or(0),
                    norm_topk_prob: config.norm_topk_prob,
                    shared_intermediate_size: config
                        .n_shared_experts
                        .map(|shared| shared * config.moe_intermediate_size.unwrap_or(0))
                        .unwrap_or(0),
                }),
                qk_norm_over_hidden: false,
                v_rms_norm: false,
                suppress_tokens: Vec::new(),
                ssm: None,
                rope_sections: None,
                full_attention_interval: 0,
                attn_gated: false,
            ffn_activation: FfnActivation::SiLU,
            alt_attn: None,
            final_logit_softcap: 0.0,
            attn_logit_scale: None,
            embd_scale: None,
            },
            globals,
            layers,
            layer_kinds: vec![LayerKind::Attention; block_count],
            mtp: None,
        })
    }
}

/// Build the descriptor for the Qwen3.5/3.6 hybrid MoE (`qwen35moe`): an
/// interleaved stack of full-attention and Gated-DeltaNet layers with routed
/// experts + a gated shared expert. Every `full_attention_interval`-th layer
/// (`(idx+1) % interval == 0`) is attention; the rest are DeltaNet. The final
/// `nextn_predict_layers` blocks are MTP/NextN speculation heads and are
/// dropped from the autoregressive stack (basic decode never runs them).
fn build_qwen35_hybrid(gguf: &Gguf, spec: &ArchSpec) -> Result<ModelDescriptor> {
    let key = |suffix: &str| format!("{}.{suffix}", spec.gguf_arch);
    let req_u = |suffix: &str| {
        gguf.get_u64(&key(suffix))
            .map(|v| v as usize)
            .ok_or_else(|| fmt_err(format!("gguf: missing metadata key {}", key(suffix))))
    };

    let (block_count, nextn) = checked_block_counts(gguf, &spec.gguf_arch)?;
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
            let v: Vec<u32> = a
                .iter()
                .filter_map(|e| e.as_u64().map(|x| x as u32))
                .collect();
            if v.len() >= 4 {
                Some([v[0], v[1], v[2], v[3]])
            } else {
                None
            }
        });

    let n_experts = gguf.get_u64(&key("expert_count")).unwrap_or(0) as usize;
    let moe = if n_experts > 0 {
        let n_experts_used = req_u("expert_used_count")?;
        let moe_intermediate_size = gguf
            .get_u64(&key("expert_feed_forward_length"))
            .map(|v| v as usize)
            .unwrap_or(feed_forward_length);
        let shared_intermediate_size = gguf
            .get_u64(&key("expert_shared_feed_forward_length"))
            .map(|v| v as usize)
            .unwrap_or(0);
        Some(MoeParams {
            n_experts,
            n_experts_used,
            moe_intermediate_size,
            norm_topk_prob: true,
            shared_intermediate_size,
        })
    } else {
        None
    };
    if moe.is_some() && nextn > 0 {
        return Err(fmt_err(
            "qwen35moe: natywny runtime MTP nie obsługuje jeszcze bloku MoE",
        ));
    }
    let mtp = if moe.is_some() {
        build_moe_mtp(gguf, block_count, nextn)?
    } else {
        build_dense_mtp(gguf, spec, block_count, nextn)?
    };

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
            .and_then(|r| r.gguf.clone())
            .ok_or_else(|| fmt_err(format!("qwen35moe spec missing global role {role:?}")))?;
        if gguf.tensor(&name).is_none() {
            return Err(fmt_err(format!(
                "{}: missing global tensor '{name}'",
                spec.name
            )));
        }
        globals.insert(role, name);
    }
    // Untied LM head: present as output.weight, else tie to the embedding.
    let tie_word_embeddings = gguf.tensor("output.weight").is_none();
    if !tie_word_embeddings {
        globals.insert(WeightRole::LmHead, "output.weight".to_string());
    }

    // Common per-layer roles shared by both attention and DeltaNet layers.
    let insert =
        |m: &mut HashMap<WeightRole, String>, role: WeightRole, name: String| -> Result<()> {
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
        insert(
            &mut m,
            WeightRole::AttnNorm,
            format!("blk.{il}.attn_norm.weight"),
        )?;
        // Post-attention norm feeds the MoE FFN (GGUF: post_attention_norm).
        insert(
            &mut m,
            WeightRole::FfnNorm,
            format!("blk.{il}.post_attention_norm.weight"),
        )?;

        match kind {
            LayerKind::Attention => {
                // Q projection is gated: width = head_dim * n_heads * 2.
                insert(&mut m, WeightRole::AttnQ, format!("blk.{il}.attn_q.weight"))?;
                insert(&mut m, WeightRole::AttnK, format!("blk.{il}.attn_k.weight"))?;
                insert(&mut m, WeightRole::AttnV, format!("blk.{il}.attn_v.weight"))?;
                insert(
                    &mut m,
                    WeightRole::AttnO,
                    format!("blk.{il}.attn_output.weight"),
                )?;
                insert(
                    &mut m,
                    WeightRole::AttnQNorm,
                    format!("blk.{il}.attn_q_norm.weight"),
                )?;
                insert(
                    &mut m,
                    WeightRole::AttnKNorm,
                    format!("blk.{il}.attn_k_norm.weight"),
                )?;
            }
            LayerKind::DeltaNet => {
                insert(
                    &mut m,
                    WeightRole::SsmInProj,
                    format!("blk.{il}.attn_qkv.weight"),
                )?;
                insert(
                    &mut m,
                    WeightRole::SsmGate,
                    format!("blk.{il}.attn_gate.weight"),
                )?;
                insert(
                    &mut m,
                    WeightRole::SsmConv1d,
                    format!("blk.{il}.ssm_conv1d.weight"),
                )?;
                insert(&mut m, WeightRole::SsmDt, format!("blk.{il}.ssm_dt.bias"))?;
                insert(&mut m, WeightRole::SsmA, format!("blk.{il}.ssm_a"))?;
                insert(
                    &mut m,
                    WeightRole::SsmBeta,
                    format!("blk.{il}.ssm_beta.weight"),
                )?;
                insert(
                    &mut m,
                    WeightRole::SsmAlpha,
                    format!("blk.{il}.ssm_alpha.weight"),
                )?;
                insert(
                    &mut m,
                    WeightRole::SsmNorm,
                    format!("blk.{il}.ssm_norm.weight"),
                )?;
                insert(
                    &mut m,
                    WeightRole::SsmOut,
                    format!("blk.{il}.ssm_out.weight"),
                )?;
            }
        }

        if moe.is_some() {
            insert(
                &mut m,
                WeightRole::FfnGateInp,
                format!("blk.{il}.ffn_gate_inp.weight"),
            )?;
            insert(
                &mut m,
                WeightRole::FfnGateExps,
                format!("blk.{il}.ffn_gate_exps.weight"),
            )?;
            insert(
                &mut m,
                WeightRole::FfnUpExps,
                format!("blk.{il}.ffn_up_exps.weight"),
            )?;
            insert(
                &mut m,
                WeightRole::FfnDownExps,
                format!("blk.{il}.ffn_down_exps.weight"),
            )?;
            insert(
                &mut m,
                WeightRole::FfnGateShExp,
                format!("blk.{il}.ffn_gate_shexp.weight"),
            )?;
            insert(
                &mut m,
                WeightRole::FfnUpShExp,
                format!("blk.{il}.ffn_up_shexp.weight"),
            )?;
            insert(
                &mut m,
                WeightRole::FfnDownShExp,
                format!("blk.{il}.ffn_down_shexp.weight"),
            )?;
            insert(
                &mut m,
                WeightRole::FfnGateInpShExp,
                format!("blk.{il}.ffn_gate_inp_shexp.weight"),
            )?;
        } else {
            insert(
                &mut m,
                WeightRole::FfnGate,
                format!("blk.{il}.ffn_gate.weight"),
            )?;
            insert(&mut m, WeightRole::FfnUp, format!("blk.{il}.ffn_up.weight"))?;
            insert(
                &mut m,
                WeightRole::FfnDown,
                format!("blk.{il}.ffn_down.weight"),
            )?;
        }

        layers.push(m);
        layer_kinds.push(kind);
    }

    // The MoE expert scratch is sized from intermediate_size; fold the expert
    // and shared-expert FFN widths into it.
    let intermediate_size = moe
        .as_ref()
        .map(|m| m.moe_intermediate_size.max(m.shared_intermediate_size))
        .unwrap_or(feed_forward_length);

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
            v_rms_norm: false,
            suppress_tokens: gguf
                .get_array("tokenizer.ggml.suppress_tokens")
                .map(|a| a.iter().filter_map(|v| v.as_u64()).map(|v| v as u32).collect())
                .unwrap_or_default(),
            ssm: Some(ssm),
            rope_sections,
            full_attention_interval,
            attn_gated: true,
            ffn_activation: FfnActivation::SiLU,
            alt_attn: None,
            final_logit_softcap: 0.0,
            attn_logit_scale: None,
            deepseek_v4: None,
            embd_scale: None,
        },
        globals,
        layers,
        layer_kinds,
        mtp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct SyntheticTensor {
        name: String,
        dims: Vec<u64>,
        ggml_type: u32,
        data: Vec<u8>,
    }

    fn write_string(output: &mut Vec<u8>, value: &str) {
        output.extend_from_slice(&(value.len() as u64).to_le_bytes());
        output.extend_from_slice(value.as_bytes());
    }

    fn metadata_u32(key: &str, value: u32) -> Vec<u8> {
        let mut output = Vec::new();
        write_string(&mut output, key);
        output.extend_from_slice(&4u32.to_le_bytes());
        output.extend_from_slice(&value.to_le_bytes());
        output
    }

    fn metadata_f32(key: &str, value: f32) -> Vec<u8> {
        let mut output = Vec::new();
        write_string(&mut output, key);
        output.extend_from_slice(&6u32.to_le_bytes());
        output.extend_from_slice(&value.to_le_bytes());
        output
    }

    fn metadata_string(key: &str, value: &str) -> Vec<u8> {
        let mut output = Vec::new();
        write_string(&mut output, key);
        output.extend_from_slice(&8u32.to_le_bytes());
        write_string(&mut output, value);
        output
    }

    fn metadata_u32_array(key: &str, values: &[u32]) -> Vec<u8> {
        let mut output = Vec::new();
        write_string(&mut output, key);
        output.extend_from_slice(&9u32.to_le_bytes());
        output.extend_from_slice(&4u32.to_le_bytes());
        output.extend_from_slice(&(values.len() as u64).to_le_bytes());
        for value in values {
            output.extend_from_slice(&value.to_le_bytes());
        }
        output
    }

    fn tensor(name: impl Into<String>, dims: Vec<u64>) -> SyntheticTensor {
        let element_count = dims.iter().product::<u64>() as usize;
        SyntheticTensor {
            name: name.into(),
            dims,
            ggml_type: 0,
            data: vec![0; element_count * 4],
        }
    }

    fn write_synthetic_gguf(metadata: Vec<Vec<u8>>, tensors: Vec<SyntheticTensor>) -> Gguf {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"GGUF");
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&(tensors.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&(metadata.len() as u64).to_le_bytes());
        for entry in metadata {
            bytes.extend_from_slice(&entry);
        }

        let mut offset = 0u64;
        for entry in &tensors {
            write_string(&mut bytes, &entry.name);
            bytes.extend_from_slice(&(entry.dims.len() as u32).to_le_bytes());
            for dim in &entry.dims {
                bytes.extend_from_slice(&dim.to_le_bytes());
            }
            bytes.extend_from_slice(&entry.ggml_type.to_le_bytes());
            bytes.extend_from_slice(&offset.to_le_bytes());
            offset += entry.data.len() as u64;
        }
        let aligned = (bytes.len() + 31) & !31;
        bytes.resize(aligned, 0);
        for entry in tensors {
            bytes.extend_from_slice(&entry.data);
        }

        let mut file = tempfile::NamedTempFile::new().expect("utwórz plik GGUF");
        file.write_all(&bytes).expect("zapisz plik GGUF");
        Gguf::open(file.path()).expect("otwórz syntetyczny GGUF")
    }

    fn synthetic_qwen35(
        block_count: u32,
        nextn: u32,
        include_mtp: bool,
        dedicated_mtp_io: bool,
    ) -> Gguf {
        let metadata = vec![
            metadata_string("general.architecture", "qwen35"),
            metadata_u32("qwen35.block_count", block_count),
            metadata_u32("qwen35.nextn_predict_layers", nextn),
            metadata_u32("qwen35.embedding_length", 64),
            metadata_u32("qwen35.feed_forward_length", 128),
            metadata_u32("qwen35.attention.head_count", 1),
            metadata_u32("qwen35.attention.head_count_kv", 1),
            metadata_u32("qwen35.attention.key_length", 64),
            metadata_u32("qwen35.context_length", 1024),
            metadata_f32("qwen35.rope.freq_base", 10_000_000.0),
            metadata_f32("qwen35.attention.layer_norm_rms_epsilon", 1e-6),
            metadata_u32("qwen35.full_attention_interval", 2),
            metadata_u32("qwen35.ssm.conv_kernel", 4),
            metadata_u32("qwen35.ssm.inner_size", 128),
            metadata_u32("qwen35.ssm.state_size", 64),
            metadata_u32("qwen35.ssm.time_step_rank", 2),
            metadata_u32("qwen35.ssm.group_count", 1),
            metadata_u32_array("qwen35.rope.dimension_sections", &[8, 8, 8, 0]),
        ];

        let mut tensors = vec![
            tensor("token_embd.weight", vec![64, 32]),
            tensor("output_norm.weight", vec![64]),
        ];
        for name in [
            "blk.0.attn_norm.weight",
            "blk.0.post_attention_norm.weight",
            "blk.0.attn_qkv.weight",
            "blk.0.attn_gate.weight",
            "blk.0.ssm_conv1d.weight",
            "blk.0.ssm_dt.bias",
            "blk.0.ssm_a",
            "blk.0.ssm_beta.weight",
            "blk.0.ssm_alpha.weight",
            "blk.0.ssm_norm.weight",
            "blk.0.ssm_out.weight",
            "blk.0.ffn_gate.weight",
            "blk.0.ffn_up.weight",
            "blk.0.ffn_down.weight",
            "blk.1.attn_norm.weight",
            "blk.1.post_attention_norm.weight",
            "blk.1.attn_q.weight",
            "blk.1.attn_k.weight",
            "blk.1.attn_v.weight",
            "blk.1.attn_output.weight",
            "blk.1.attn_q_norm.weight",
            "blk.1.attn_k_norm.weight",
            "blk.1.ffn_gate.weight",
            "blk.1.ffn_up.weight",
            "blk.1.ffn_down.weight",
        ] {
            tensors.push(tensor(name, vec![64]));
        }
        if include_mtp {
            for name in [
                "blk.2.attn_k.weight",
                "blk.2.attn_k_norm.weight",
                "blk.2.attn_norm.weight",
                "blk.2.attn_output.weight",
                "blk.2.attn_q.weight",
                "blk.2.attn_q_norm.weight",
                "blk.2.attn_v.weight",
                "blk.2.ffn_down.weight",
                "blk.2.ffn_gate.weight",
                "blk.2.ffn_up.weight",
                "blk.2.post_attention_norm.weight",
                "blk.2.nextn.eh_proj.weight",
                "blk.2.nextn.enorm.weight",
                "blk.2.nextn.hnorm.weight",
                "blk.2.nextn.shared_head_norm.weight",
            ] {
                tensors.push(tensor(name, vec![64]));
            }
            if dedicated_mtp_io {
                tensors.push(tensor("blk.2.nextn.embed_tokens.weight", vec![64, 32]));
                tensors.push(tensor("blk.2.nextn.shared_head_head.weight", vec![64, 32]));
            }
        }
        write_synthetic_gguf(metadata, tensors)
    }

    #[test]
    fn all_embedded_specs_parse() {
        let specs = registry();
        assert_eq!(specs.len(), 9);
        assert_eq!(specs[0].name, "qwen3");
        assert_eq!(specs[1].name, "llama");
        assert_eq!(specs[2].name, "mistral");
        assert_eq!(specs[3].name, "olmoe");
        assert_eq!(specs[4].name, "qwen3moe");
        assert_eq!(specs[5].name, "qwen35moe");
        assert_eq!(specs[6].name, "qwen35");
        assert_eq!(specs[7].name, "gemma4");
        assert_eq!(specs[8].name, "deepseek_v4");
        // The MoE specs carry the router + stacked-expert roles.
        assert!(specs[3]
            .roles
            .iter()
            .any(|r| r.role == WeightRole::FfnGateInp));
        assert!(specs[3]
            .roles
            .iter()
            .any(|r| r.role == WeightRole::FfnGateExps));
        assert!(specs[3]
            .roles
            .iter()
            .any(|r| r.role == WeightRole::FfnDownExps));
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
    fn dense_qwen35_separates_mtp_from_trunk() {
        let gguf = synthetic_qwen35(3, 1, true, false);
        let descriptor = ModelDescriptor::detect(&gguf).expect("wykryj qwen35");

        assert_eq!(descriptor.arch, "qwen35");
        assert_eq!(descriptor.params.block_count, 2);
        assert_eq!(descriptor.layers.len(), 2);
        assert_eq!(
            descriptor.layer_kinds,
            [LayerKind::DeltaNet, LayerKind::Attention]
        );
        let mtp = descriptor.mtp.as_ref().expect("wydziel MTP");
        assert_eq!(mtp.first_block, 2);
        assert_eq!(mtp.block_count, 1);
        assert_eq!(mtp.layers.len(), 1);
        assert_eq!(mtp.layers[0].len(), 15);
        assert_eq!(
            mtp.layers[0][&MtpWeightRole::EhProj],
            "blk.2.nextn.eh_proj.weight"
        );
        assert!(descriptor.params.moe.is_none());
        assert_eq!(descriptor.params.intermediate_size, 128);
        assert!(descriptor.layers[0].contains_key(&WeightRole::SsmInProj));
        assert!(descriptor.layers[0].contains_key(&WeightRole::FfnGate));
        assert!(descriptor.layers[1].contains_key(&WeightRole::AttnQ));
        assert!(descriptor.layers[1].contains_key(&WeightRole::FfnDown));
        assert!(descriptor
            .layers
            .iter()
            .flat_map(HashMap::values)
            .all(|name| !name.starts_with("blk.2.")));
        assert!(gguf.tensor("blk.2.nextn.eh_proj.weight").is_some());
    }

    #[test]
    fn dense_qwen35_prefers_dedicated_mtp_embedding_and_head() {
        let gguf = synthetic_qwen35(3, 1, true, true);
        let descriptor = ModelDescriptor::detect(&gguf).expect("wykryj dedykowane MTP IO");
        let mtp = descriptor.mtp.as_ref().expect("wydziel MTP");
        assert_eq!(
            mtp.layers[0][&MtpWeightRole::Embedding],
            "blk.2.nextn.embed_tokens.weight"
        );
        assert_eq!(
            mtp.layers[0][&MtpWeightRole::SharedHead],
            "blk.2.nextn.shared_head_head.weight"
        );
    }

    #[test]
    fn dense_qwen35_rejects_mtp_bez_trunku() {
        let gguf = synthetic_qwen35(1, 1, false, false);
        let error = ModelDescriptor::detect(&gguf).expect_err("odrzuć pusty trunk");
        assert!(error.to_string().contains("całego trunku"));
    }

    #[test]
    fn dense_qwen35_rejects_mtp_count_larger_than_stack() {
        let gguf = synthetic_qwen35(1, 2, false, false);
        let error = ModelDescriptor::detect(&gguf).expect_err("odrzuć błędną liczbę bloków");
        assert!(error.to_string().contains("exceeds block_count"));
    }

    #[test]
    fn dense_qwen35_rejects_missing_mtp_tensor() {
        let gguf = synthetic_qwen35(3, 1, false, false);
        let error = ModelDescriptor::detect(&gguf).expect_err("odrzuć niepełny blok MTP");
        assert!(error.to_string().contains("blk.2.attn_k.weight"));
    }

    #[test]
    fn dense_qwen35_rejects_block_count_larger_than_tensor_table() {
        let gguf = synthetic_qwen35(1_000, 0, false, false);
        let error = ModelDescriptor::detect(&gguf).expect_err("odrzuć niebezpieczną liczbę bloków");
        assert!(error.to_string().contains("exceeds tensor count"));
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
        assert_eq!(
            moe.shared_intermediate_size, 0,
            "OLMoE has no shared expert"
        );
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

    /// Odrzuca realny Qwen3.6 MoE z MTP, dopóki runtime nie wykonuje bloku MoE.
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
        let error = ModelDescriptor::detect(&gguf).expect_err("odrzuć MTP MoE");
        assert!(error.to_string().contains("runtime MTP"));
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

#[cfg(test)]
mod pipeline_tests {
    use super::check_pipeline_range;

    #[test]
    fn zakres_ze_srodka_jest_poprawny() {
        assert!(check_pipeline_range(40, 20, 20).is_ok());
        assert!(check_pipeline_range(65, 0, 33).is_ok());
    }

    #[test]
    fn zakres_poza_model_jest_bledem() {
        assert!(check_pipeline_range(40, 30, 20).is_err());
        assert!(check_pipeline_range(40, 40, 1).is_err());
    }

    #[test]
    fn etap_bez_warstw_jest_bledem() {
        assert!(check_pipeline_range(40, 0, 0).is_err());
    }

    #[test]
    fn caly_model_jako_jeden_etap_przechodzi() {
        assert!(check_pipeline_range(40, 0, 40).is_ok());
    }
}

// ===== Kontrakt podziału tensor-parallel =====
//
// Ranga to osobny model na własnej karcie, zbudowany z deskryptora o
// PODZIELONYCH liczbach głowic. Dzięki temu prefill, dekodowanie, weryfikacja
// MTP i batch przechodzą przez ten sam kod na mniejszym kształcie, zamiast
// wymagać osobnego wpięcia każda z osobna.
//
// Ten moduł nie dzieli wag — opisuje WYŁĄCZNIE, co komu przypada, i odmawia,
// gdy kształt się nie dzieli. Wszystko, co dzieli wagi, ma się o niego pytać.

/// Przydział rangi w podziale tensor-parallel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TpShard {
    pub rank: usize,
    pub world: usize,
}

impl TpShard {
    pub fn new(rank: usize, world: usize) -> Result<Self> {
        if world == 0 || rank >= world {
            return Err(fmt_err(format!(
                "tensor parallel: ranga {rank} poza światem {world}"
            )));
        }
        Ok(Self { rank, world })
    }

    /// Podział pojedynczego wymiaru; `align` to najmniejsza niepodzielna
    /// jednostka (blok kwantyzacji albo `head_dim`).
    fn split(&self, what: &str, total: usize, align: usize) -> Result<usize> {
        if align == 0 || !total.is_multiple_of(align) {
            return Err(fmt_err(format!(
                "tensor parallel: {what} = {total} nie jest wielokrotnością {align}"
            )));
        }
        let units = total / align;
        if !units.is_multiple_of(self.world) {
            return Err(fmt_err(format!(
                "tensor parallel: {what} = {total} daje {units} jednostek po {align}, \
                 czego nie da się podzielić na {} rang",
                self.world
            )));
        }
        Ok(total / self.world)
    }
}

impl Hyperparams {
    /// Hiperparametry JEDNEJ rangi. Odmawia zamiast dobierać po cichu — podział,
    /// który nie wychodzi, ma się zatrzymać przy starcie, a nie policzyć co
    /// innego.
    pub fn shard(&self, shard: TpShard) -> Result<Self> {
        if shard.world == 1 {
            return Ok(self.clone());
        }
        if self.moe.is_some() {
            return Err(fmt_err(
                "tensor parallel: MoE dzieli się po ekspertach, nie po głowicach",
            ));
        }
        if self.alt_attn.is_some() {
            return Err(fmt_err(
                "tensor parallel: naprzemienna geometria uwagi nie ma opisanego podziału",
            ));
        }
        let mut out = self.clone();
        // GQA jest tu NAJCIAŚNIEJSZE: głowic KV jest zwykle kilka, więc to one
        // wyznaczają dopuszczalną liczbę rang, a nie liczba głowic Q.
        out.n_kv_heads = shard.split("n_kv_heads", self.n_kv_heads, 1)?;
        out.n_heads = shard.split("n_heads", self.n_heads, 1)?;
        if !out.n_heads.is_multiple_of(out.n_kv_heads) {
            return Err(fmt_err(format!(
                "tensor parallel: po podziale zostaje {} głowic Q na {} KV — grupa GQA \
                 musi zostać cała na jednej randze",
                out.n_heads, out.n_kv_heads
            )));
        }
        // Wymiar pośredni dzieli się po WIERSZACH `gate`/`up` i po KOLUMNACH
        // `down`, więc musi zostać wielokrotnością największego bloku kwantyzacji
        // K-kwantów; inaczej kolumnowy wycinek `down` trafiałby w środek bloku.
        out.intermediate_size = shard.split("intermediate_size", self.intermediate_size, 256)?;
        if let Some(ssm) = &self.ssm {
            out.ssm = Some(ssm.shard(shard)?);
        }
        Ok(out)
    }
}

impl SsmParams {
    /// Parametry DeltaNet jednej rangi.
    ///
    /// PUŁAPKA, na której rozbiła się poprzednia próba: GGUF przypisuje głowicę
    /// V do głowicy K przez MODULO (`deltanet_repeat_qk_f16` liczy
    /// `source = index % key_dim`), więc CIĄGŁY zakres głowic V nie pokrywa się z
    /// żadnym zakresem głowic K. Ranga, która dostała V 24-47 i K 8-15, sięga
    /// głowicą V 39 po głowicę K 7 — której nie ma. Nie ma z tego błędu ani
    /// asercji, tylko tekst-śmieć.
    ///
    /// Dlatego podział idzie po KLASACH RESZTY: ranga bierze głowice K
    /// `[r*n_k/W, (r+1)*n_k/W)` oraz te głowice V, których reszta z dzielenia
    /// przez `n_k` wpada w ten zakres. Odpowiada to `n_v / n_k` ciągłym
    /// przebiegom — patrz `TpShard::v_head_runs`.
    pub fn shard(&self, shard: TpShard) -> Result<Self> {
        if shard.world == 1 {
            return Ok(self.clone());
        }
        if !self.dt_rank.is_multiple_of(self.n_group) {
            return Err(fmt_err(format!(
                "tensor parallel: {} głowic V nie jest wielokrotnością {} głowic K, \
                 więc mapowanie GQA DeltaNet nie ma podziału",
                self.dt_rank, self.n_group
            )));
        }
        let mut out = self.clone();
        out.n_group = shard.split("ssm.group_count", self.n_group, 1)?;
        out.dt_rank = shard.split("ssm.time_step_rank", self.dt_rank, 1)?;
        out.d_inner = out.dt_rank * self.d_state;
        Ok(out)
    }
}

impl TpShard {
    /// Globalne indeksy głowic K tej rangi (ciągły zakres).
    pub fn k_head_range(&self, n_k_heads: usize) -> Result<(usize, usize)> {
        let per = self.split("ssm.group_count", n_k_heads, 1)?;
        Ok((self.rank * per, per))
    }

    /// Globalne indeksy głowic V tej rangi, W KOLEJNOŚCI LOKALNEJ.
    ///
    /// Kolejność jest istotna, a nie kosmetyczna: scalony wstęp kroku DeltaNet
    /// numeruje głowice V jako `h_local + run * n_k_local`, więc loader musi
    /// ułożyć wiersze dokładnie w tej kolejności. Inaczej ranga liczyłaby swoje
    /// głowice poprawnie, ale przypisała im cudze wagi.
    pub fn v_head_order(&self, n_k_heads: usize, n_v_heads: usize) -> Result<Vec<usize>> {
        if n_k_heads == 0 || !n_v_heads.is_multiple_of(n_k_heads) {
            return Err(fmt_err(format!(
                "tensor parallel: {n_v_heads} głowic V nie jest wielokrotnością {n_k_heads} głowic K"
            )));
        }
        let (first_k, per_k) = self.k_head_range(n_k_heads)?;
        let runs = n_v_heads / n_k_heads;
        let mut order = Vec::with_capacity(runs * per_k);
        for run in 0..runs {
            for h in 0..per_k {
                order.push(run * n_k_heads + first_k + h);
            }
        }
        Ok(order)
    }
}

#[cfg(test)]
mod tp_shard_tests {
    use super::*;

    fn qwen35_ssm() -> SsmParams {
        SsmParams {
            d_conv: 4,
            d_inner: 6144,
            d_state: 128,
            dt_rank: 48,
            n_group: 16,
        }
    }

    #[test]
    fn dwie_rangi_dziela_glowice_qwen35() {
        let shard = TpShard::new(1, 2).expect("ranga w zasięgu");
        let ssm = qwen35_ssm().shard(shard).expect("qwen35 dzieli się na dwie");
        assert_eq!(ssm.n_group, 8);
        assert_eq!(ssm.dt_rank, 24);
        assert_eq!(ssm.d_inner, 24 * 128);
    }

    #[test]
    fn glowice_v_ida_klasami_reszty_a_nie_ciagiem() {
        let rank0 = TpShard::new(0, 2).expect("ranga 0");
        let rank1 = TpShard::new(1, 2).expect("ranga 1");
        let v0 = rank0.v_head_order(16, 48).expect("podział V");
        let v1 = rank1.v_head_order(16, 48).expect("podział V");
        // Ciągły podział dałby 0-23 i 24-47, i głowica V 39 sięgałaby po
        // głowicę K 7, której ranga 1 nie ma.
        assert_eq!(&v0[..8], &[0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(&v0[8..16], &[16, 17, 18, 19, 20, 21, 22, 23]);
        assert_eq!(&v0[16..], &[32, 33, 34, 35, 36, 37, 38, 39]);
        assert_eq!(&v1[..8], &[8, 9, 10, 11, 12, 13, 14, 15]);
        assert_eq!(v0.len() + v1.len(), 48);
        // Każda głowica V trafia dokładnie raz i na tę rangę, która ma jej K.
        let mut all: Vec<usize> = v0.iter().chain(v1.iter()).copied().collect();
        all.sort_unstable();
        assert_eq!(all, (0..48).collect::<Vec<_>>());
        for (rank, heads) in [(0usize, &v0), (1usize, &v1)] {
            let (first_k, per_k) = TpShard::new(rank, 2)
                .expect("ranga")
                .k_head_range(16)
                .expect("zakres K");
            for &v in heads {
                let k = v % 16;
                assert!(
                    k >= first_k && k < first_k + per_k,
                    "głowica V {v} sięga po K {k} spoza rangi {rank}"
                );
            }
        }
    }

    #[test]
    fn niepodzielne_ksztalty_sa_odrzucane() {
        let shard = TpShard::new(0, 3).expect("ranga 0 z trzech");
        // 16 głowic K nie dzieli się na trzy rangi.
        assert!(qwen35_ssm().shard(shard).is_err());
        let mut ssm = qwen35_ssm();
        // 48 głowic V przy 5 głowicach K nie ma mapowania GQA.
        ssm.n_group = 5;
        assert!(ssm.shard(TpShard::new(0, 2).expect("ranga")).is_err());
    }

    #[test]
    fn ranga_poza_swiatem_jest_bledem() {
        assert!(TpShard::new(2, 2).is_err());
        assert!(TpShard::new(0, 0).is_err());
    }
}

/// Geometria bloku kwantyzacji: ile wartości i ile bajtów zajmuje jeden blok.
///
/// Podział tensor-parallel tnie macierze bez dekwantyzacji, więc musi znać
/// dokładnie tę jedną rzecz. Lista jest świadomie WĄSKA — to formaty, w których
/// leżą checkpointy objęte podziałem. Każdy inny ma tu paść głośno, a nie zostać
/// pocięty regułą, której nikt nie sprawdził na danych.
pub fn block_geometry(quant: QuantKind) -> Result<(usize, usize)> {
    Ok(match quant {
        QuantKind::None => (1, 2),
        QuantKind::Q8_0 => (32, 34),
        QuantKind::Q4K => (256, 144),
        QuantKind::Q6K => (256, 210),
        QuantKind::NVFP4Gguf => (64, 36),
        other => {
            return Err(fmt_err(format!(
                "podział tensor-parallel nie zna geometrii bloku {other:?}"
            )));
        }
    })
}

/// Macierz `[rows, cols]` w formacie blokowym, krojona na rangi bez
/// dekwantyzacji.
///
/// Dwa kierunki cięcia wynikają wprost z tego, który wymiar jest dzielony:
///
/// - macierz **kolumnowo równoległa** (dzielone WYJŚCIE) tnie WIERSZE, a wynik
///   zostaje lokalny. Wiersze leżą ciągle w każdym formacie blokowym, więc
///   wycinek to sklejenie zakresów bajtów;
/// - macierz **wierszowo równoległa** (dzielone WEJŚCIE) tnie KOLUMNY, czyli
///   wycinek WEWNĄTRZ każdego wiersza. Dlatego granica musi paść na blok:
///   inaczej wycinek zaczynałby się w środku bloku, a jego skala opisywałaby
///   wagi, których na tej randze nie ma.
pub struct BlockMatrix<'a> {
    pub data: &'a [u8],
    pub rows: usize,
    pub cols: usize,
    pub quant: QuantKind,
}

impl BlockMatrix<'_> {
    fn geometry(&self) -> Result<(usize, usize, usize)> {
        let (values, block_bytes) = block_geometry(self.quant)?;
        if !self.cols.is_multiple_of(values) {
            return Err(fmt_err(format!(
                "podział: {} kolumn nie jest wielokrotnością bloku {values}",
                self.cols
            )));
        }
        let row_bytes = (self.cols / values) * block_bytes;
        let expected = self.rows * row_bytes;
        if self.data.len() != expected {
            return Err(fmt_err(format!(
                "podział: [{}, {}] w {:?} ma zajmować {expected} B, a tensor ma {} B",
                self.rows,
                self.cols,
                self.quant,
                self.data.len()
            )));
        }
        Ok((values, block_bytes, row_bytes))
    }

    /// Wycinek WIERSZY: skleja podane zakresy W PODANEJ KOLEJNOŚCI.
    ///
    /// Kolejność jest częścią kontraktu, a nie kosmetyką. Głowice V DeltaNet idą
    /// klasami reszty (`TpShard::v_head_order`), więc ranga dostaje kilka
    /// rozłącznych przebiegów, które muszą trafić do wag dokładnie w tej
    /// kolejności, w jakiej numeruje je kernel. Odwrotna kolejność nie daje
    /// błędu — daje cudze wagi pod właściwą głowicą.
    pub fn take_rows(&self, ranges: &[(usize, usize)]) -> Result<Vec<u8>> {
        let (_, _, row_bytes) = self.geometry()?;
        let mut out =
            Vec::with_capacity(ranges.iter().map(|(_, count)| count * row_bytes).sum::<usize>());
        for &(first, count) in ranges {
            let end = first
                .checked_add(count)
                .ok_or_else(|| fmt_err("podział wierszy: przepełnienie zakresu"))?;
            if end > self.rows {
                return Err(fmt_err(format!(
                    "podział wierszy: zakres [{first}, {end}) wykracza poza {} wierszy",
                    self.rows
                )));
            }
            out.extend_from_slice(&self.data[first * row_bytes..end * row_bytes]);
        }
        Ok(out)
    }

    /// Wycinek KOLUMN, ten sam w każdym wierszu. Granice muszą paść na blok.
    pub fn take_cols(&self, first: usize, count: usize) -> Result<Vec<u8>> {
        let (values, block_bytes, row_bytes) = self.geometry()?;
        if !first.is_multiple_of(values) || !count.is_multiple_of(values) {
            return Err(fmt_err(format!(
                "podział kolumn: granica [{first}, {}) nie pada na blok {values} wartości",
                first + count
            )));
        }
        let end = first + count;
        if end > self.cols {
            return Err(fmt_err(format!(
                "podział kolumn: zakres [{first}, {end}) wykracza poza {} kolumn",
                self.cols
            )));
        }
        let take_from = (first / values) * block_bytes;
        let take_bytes = (count / values) * block_bytes;
        let mut out = Vec::with_capacity(self.rows * take_bytes);
        for row in 0..self.rows {
            let base = row * row_bytes + take_from;
            out.extend_from_slice(&self.data[base..base + take_bytes]);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod block_matrix_tests {
    use super::*;

    /// Q4_K: 144 B na 256 wartości. Bajt `i` wiersza `r` niesie numer wiersza,
    /// więc każdy wycinek da się sprawdzić po zawartości, a nie po długości.
    fn matrix(rows: usize, cols: usize) -> Vec<u8> {
        let row_bytes = (cols / 256) * 144;
        let mut data = vec![0u8; rows * row_bytes];
        for r in 0..rows {
            for b in 0..row_bytes {
                data[r * row_bytes + b] = (r % 251) as u8;
            }
        }
        data
    }

    #[test]
    fn wycinek_wierszy_zachowuje_kolejnosc_zakresow() {
        let data = matrix(48, 256);
        let m = BlockMatrix {
            data: &data,
            rows: 48,
            cols: 256,
            quant: QuantKind::Q4K,
        };
        // Kolejność głowic V rangi 1 przy n_k = 16, n_v = 48.
        let order = TpShard::new(1, 2)
            .expect("ranga")
            .v_head_order(16, 48)
            .expect("podział V");
        let ranges: Vec<(usize, usize)> = order.iter().map(|&h| (h, 1)).collect();
        let out = m.take_rows(&ranges).expect("wycinek");
        assert_eq!(out.len() / 144, order.len());
        for (slot, &head) in order.iter().enumerate() {
            assert_eq!(out[slot * 144], (head % 251) as u8, "slot {slot}");
        }
    }

    #[test]
    fn wycinek_kolumn_bierze_ten_sam_zakres_w_kazdym_wierszu() {
        let data = matrix(4, 1024);
        let m = BlockMatrix {
            data: &data,
            rows: 4,
            cols: 1024,
            quant: QuantKind::Q4K,
        };
        let out = m.take_cols(512, 512).expect("wycinek");
        assert_eq!(out.len(), 4 * 2 * 144);
        for r in 0..4 {
            assert_eq!(out[r * 2 * 144], (r % 251) as u8);
        }
    }

    #[test]
    fn granica_w_srodku_bloku_jest_bledem() {
        let data = matrix(2, 512);
        let m = BlockMatrix {
            data: &data,
            rows: 2,
            cols: 512,
            quant: QuantKind::Q4K,
        };
        assert!(m.take_cols(128, 256).is_err());
        assert!(m.take_cols(0, 300).is_err());
        assert!(m.take_rows(&[(1, 2)]).is_err());
    }

    #[test]
    fn niezgodna_dlugosc_tensora_jest_bledem() {
        let data = vec![0u8; 100];
        let m = BlockMatrix {
            data: &data,
            rows: 2,
            cols: 512,
            quant: QuantKind::Q4K,
        };
        assert!(m.take_rows(&[(0, 1)]).is_err());
    }
}
