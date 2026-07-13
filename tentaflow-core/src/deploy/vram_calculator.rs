// =============================================================================
// File: deploy/vram_calculator.rs
// Opis: Estymator VRAM dla deploymentu vLLM. Czyta HF config.json, oblicza
//       weights + kv_cache + activations dla danej konfiguracji TP/PP/context/
//       kv_dtype. Generuje rekomendacje TP/PP zgodne z liczba GPU i atrybutami
//       modelu (num_attention_heads musi byc podzielne przez TP, num_hidden_layers
//       przez PP).
// =============================================================================

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

/// Konfiguracja modelu pobrana z HF config.json. Pola opcjonalne bo
/// config moze byc zagniezdzony (text_config dla multimodal) albo uzywac
/// alternatywnych nazw.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelSpec {
    pub model_type: String,
    pub architectures: Vec<String>,
    /// `bfloat16` / `float16` / `float32` / `int4` / `int8` / quantization name
    pub dtype: String,
    pub hidden_size: u64,
    pub num_attention_heads: u64,
    pub num_key_value_heads: u64,
    pub num_hidden_layers: u64,
    pub vocab_size: u64,
    pub head_dim: u64,
    pub intermediate_size: u64,
    pub max_position_embeddings: u64,
    /// Liczba ekspertow MoE per warstwa. 0 = model gesty (dense).
    pub num_experts: u64,
    /// Liczba ekspertow aktywowanych na token (top-K routingu). 0 dla dense.
    pub num_experts_per_tok: u64,
    /// Wymiar posredni pojedynczego eksperta MoE. Czesto inny (mniejszy) niz
    /// `intermediate_size`; gdy 0 a model jest MoE, fallback do intermediate_size.
    pub moe_intermediate_size: u64,
    /// Wymiar posredni wspoldzielonego eksperta (Qwen-MoE/DeepSeek). 0 = brak.
    pub shared_expert_intermediate_size: u64,
    /// Wagi lm_head wspoldzielone z embeddingiem wejsciowym. Gdy true, lm_head
    /// nie dodaje osobnych parametrow.
    pub tie_word_embeddings: bool,
    /// Jest multimodal (vision/audio)
    pub has_vision: bool,
    pub has_audio: bool,
    /// Jawna liczba parametrow (z safetensors index lub HF API). Gdy 0 -
    /// kalkulujemy z hidden/layers/vocab.
    pub num_parameters: u64,
    /// Aktywne parametry MoE. 0 = nie MoE.
    pub num_active_parameters: u64,
    /// Quantization wykryta z nazwy modelu / config (auto/awq/gptq/int4/int8/fp8).
    pub quantization: Option<String>,
    /// Jawny bytes/param (MLX top-level `quantization` z group_size: szerokosc
    /// zalezy od group_size, np. 4-bit g64 = 0.5625, g32 = 0.625). Gdy `Some`,
    /// ma pierwszenstwo przed `quantization`/`dtype` w `bytes_per_param`.
    pub bytes_per_param_override: Option<f64>,
    /// Sliding-window attention size in tokens. 0 = model has no SWA layers.
    #[serde(default)]
    pub sliding_window: u64,
    /// Per-token K cache elements summed over GLOBAL (full-attention) layers.
    #[serde(default)]
    pub kv_k_elems_global: u64,
    /// Per-token V cache elements summed over GLOBAL (full-attention) layers.
    #[serde(default)]
    pub kv_v_elems_global: u64,
    /// Per-token K cache elements summed over SWA layers (window-capped cache).
    #[serde(default)]
    pub kv_k_elems_swa: u64,
    /// Per-token V cache elements summed over SWA layers (window-capped cache).
    #[serde(default)]
    pub kv_v_elems_swa: u64,
}

impl ModelSpec {
    /// Liczba bajtow per parametr na podstawie dtype/quantization. Jawny
    /// `bytes_per_param_override` (MLX group-size) wygrywa nad etykieta i dtype.
    /// Quantization wartosci uwzgledniaja overhead skali/zero-pointow:
    /// - 4-bit (awq/gptq/nvfp4/fp4/mxfp4/bnb_4bit/...): 0.5 + ~0.0625 = 0.5625
    /// - 8-bit (int8/fp8/bnb_8bit): 1.0 + ~0.0625 = 1.0625
    pub fn bytes_per_param(&self) -> f64 {
        if let Some(b) = self.bytes_per_param_override {
            return b;
        }
        if let Some(q) = &self.quantization {
            return quant_label_to_bytes(q).unwrap_or_else(|| self.bytes_per_dtype());
        }
        self.bytes_per_dtype()
    }

    fn bytes_per_dtype(&self) -> f64 {
        match self.dtype.as_str() {
            "bfloat16" | "float16" | "f16" | "bf16" => 2.0,
            "float32" | "f32" => 4.0,
            // dtype 8-bitowy w config zapisuje wagi po 1 bajcie na parametr
            // (fp8 per-tensor/block, uint8 dla pakowanych int8) - bez tego
            // wpadaja w default 2.0 i zawyzaja raw fp8 2x.
            "int8" | "fp8" | "float8_e4m3fn" | "float8_e5m2" | "float8" | "uint8" => 1.0,
            "int4" => 0.5,
            _ => 2.0, // bf16 default dla nowoczesnych LLM
        }
    }

    /// Liczba glow KV (z fallbackiem do glow uwagi gdy config nie podaje GQA).
    fn kv_heads_effective(&self) -> u64 {
        if self.num_key_value_heads > 0 {
            self.num_key_value_heads
        } else {
            self.num_attention_heads
        }
    }

    /// Wymiar pojedynczej glowy (jawny `head_dim` albo `hidden/heads`).
    fn head_dim_effective(&self) -> u64 {
        if self.head_dim > 0 {
            self.head_dim
        } else if self.num_attention_heads > 0 {
            self.hidden_size / self.num_attention_heads
        } else {
            0
        }
    }

    /// Effective per-token K/V element sums split into global vs SWA layer
    /// buckets. When the parser did not provide the SWA-aware aggregates (all
    /// four are 0), derives the global sums from the legacy uniform fields
    /// (layers × kv_heads × head_dim for both K and V) with zero SWA sums.
    fn kv_layer_elem_sums(&self) -> (u64, u64, u64, u64) {
        let total = self.kv_k_elems_global
            + self.kv_v_elems_global
            + self.kv_k_elems_swa
            + self.kv_v_elems_swa;
        if total > 0 {
            return (
                self.kv_k_elems_global,
                self.kv_v_elems_global,
                self.kv_k_elems_swa,
                self.kv_v_elems_swa,
            );
        }
        let head_dim = if self.head_dim > 0 {
            self.head_dim
        } else if self.num_attention_heads > 0 {
            self.hidden_size / self.num_attention_heads
        } else {
            128
        };
        let kv_heads = if self.num_key_value_heads > 0 {
            self.num_key_value_heads
        } else {
            self.num_attention_heads.max(1)
        };
        let per = self.num_hidden_layers * kv_heads * head_dim;
        (per, per, 0, 0)
    }

    /// KV cache bytes for ONE sequence of `ctx_tokens`, with separate K/V cache
    /// dtypes. Global layers grow linearly with the context; SWA layers are
    /// capped at the sliding window plus ~one ubatch (512) of llama.cpp padding.
    /// Invalid dtype labels fall back to 2.0 B/elem (fp16).
    pub fn kv_bytes_for_ctx(
        &self,
        engine: DeployEngine,
        k_label: &str,
        v_label: &str,
        ctx_tokens: u64,
    ) -> f64 {
        let bytes_k = kv_bytes_per_element(engine, k_label).unwrap_or(2.0);
        let bytes_v = kv_bytes_per_element(engine, v_label).unwrap_or(2.0);
        let (k_g, v_g, k_swa, v_swa) = self.kv_layer_elem_sums();
        let swa_tokens = if self.sliding_window > 0 {
            ctx_tokens.min(self.sliding_window + 512)
        } else {
            ctx_tokens
        };
        (k_g as f64 * bytes_k + v_g as f64 * bytes_v) * ctx_tokens as f64
            + (k_swa as f64 * bytes_k + v_swa as f64 * bytes_v) * swa_tokens as f64
    }

    /// Wymiar posredni eksperta MoE. Czesc configow trzyma rozmiar per-ekspert
    /// w `intermediate_size`, wiec gdy `moe_intermediate_size==0` ale model jest
    /// MoE, traktujemy `intermediate_size` jako wartosc per-ekspert.
    fn moe_intermediate_effective(&self) -> u64 {
        if self.moe_intermediate_size > 0 {
            self.moe_intermediate_size
        } else {
            self.intermediate_size
        }
    }

    /// Wzor liczenia parametrow gdy num_parameters = 0:
    ///   embed: vocab × hidden
    ///   attn per warstwa: q(h²) + o(h²) + k+v z GQA (2·h·kv_heads·head_dim)
    ///   ffn per warstwa: dense = 3·h·intermediate; MoE = sumy wszystkich ekspertow
    ///     (vLLM/MLX ladują WSZYSTKIE wagi ekspertow) + ewentualny shared expert + router
    ///   norms per warstwa: 2·h
    ///   lm_head: vocab × hidden, tylko gdy embeddingi NIE sa tied
    pub fn estimated_params(&self) -> u64 {
        if self.num_parameters > 0 {
            return self.num_parameters;
        }
        let h = self.hidden_size as f64;
        let v = self.vocab_size as f64;
        let i = if self.intermediate_size > 0 {
            self.intermediate_size as f64
        } else {
            h * 4.0
        };
        let l = self.num_hidden_layers as f64;
        let kv_heads = self.kv_heads_effective() as f64;
        let head_dim = self.head_dim_effective() as f64;

        let embed = v * h;
        // q i o sa pelne (h×h); k i v sa zwezone przez GQA do kv_heads głow.
        let attn_per_layer = 2.0 * h * h + 2.0 * h * kv_heads * head_dim;
        let ffn_per_layer = if self.num_experts > 0 {
            let moe_i = self.moe_intermediate_effective() as f64;
            let experts = self.num_experts as f64;
            let shared = if self.shared_expert_intermediate_size > 0 {
                3.0 * h * self.shared_expert_intermediate_size as f64
            } else {
                0.0
            };
            // 3 macierze (gate+up+down) na eksperta + router (h × num_experts).
            experts * 3.0 * h * moe_i + shared + h * experts
        } else {
            3.0 * h * i
        };
        let norms_per_layer = 2.0 * h;
        let lm_head = if self.tie_word_embeddings { 0.0 } else { v * h };
        (embed + l * (attn_per_layer + ffn_per_layer + norms_per_layer) + lm_head) as u64
    }

    /// Liczba aktywnych parametrow (MoE: tylko top-K expertow). Default = wszystkie.
    /// Dla MoE liczy te same czlony co `estimated_params`, ale FFN obejmuje jedynie
    /// `num_experts_per_tok` ekspertow zamiast wszystkich.
    pub fn active_params(&self) -> u64 {
        if self.num_parameters > 0 && self.num_active_parameters > 0 {
            return self.num_active_parameters;
        }
        if self.num_experts == 0 {
            return self.estimated_params();
        }
        let h = self.hidden_size as f64;
        let v = self.vocab_size as f64;
        let l = self.num_hidden_layers as f64;
        let kv_heads = self.kv_heads_effective() as f64;
        let head_dim = self.head_dim_effective() as f64;
        let moe_i = self.moe_intermediate_effective() as f64;
        let active_experts = self.num_experts_per_tok.max(1) as f64;

        let embed = v * h;
        let attn_per_layer = 2.0 * h * h + 2.0 * h * kv_heads * head_dim;
        let shared = if self.shared_expert_intermediate_size > 0 {
            3.0 * h * self.shared_expert_intermediate_size as f64
        } else {
            0.0
        };
        let ffn_per_layer = active_experts * 3.0 * h * moe_i + shared + h * self.num_experts as f64;
        let norms_per_layer = 2.0 * h;
        let lm_head = if self.tie_word_embeddings { 0.0 } else { v * h };
        (embed + l * (attn_per_layer + ffn_per_layer + norms_per_layer) + lm_head) as u64
    }
}

/// Przeklada etykiete quantization (dowolny case, '-' lub '_') na bytes/param.
/// Zwraca None gdy etykieta nieznana - caller fallbackuje do dtype.
/// Wartosci dla 4/8-bit zawieraja overhead group-scales (~6.25%).
pub fn quant_label_to_bytes(label: &str) -> Option<f64> {
    let q = label.to_lowercase().replace('-', "_");
    match q.as_str() {
        // mxfp4: 4 bity + wspoldzielony skalar e8m0 per 32 elementy = 4.25 bit/param
        // (4.25/8 = 0.5312), wezej niz group-scale 4-bit ponizej.
        "mxfp4" => Some(0.5312),
        // 4-bit: AWQ, GPTQ, AutoRound INT4, bnb-4bit, NVFP4, FP4, w4a16
        "int4"
        | "awq"
        | "gptq"
        | "int4_autoround"
        | "auto_round"
        | "bnb_4bit"
        | "bitsandbytes_4bit"
        | "load_in_4bit"
        | "nvfp4"
        | "fp4"
        | "w4a16"
        | "compressed_tensors_4bit" => Some(0.5625),
        // fp8 per-tensor/block: znikomy overhead skali, 1 bajt/param.
        "fp8" | "fp8_e4m3" | "fp8_e5m2" | "modelopt_fp8" => Some(1.0),
        // int8 group-scale: 1 bajt + ~6.25% skali na grupe = 1.0625.
        "int8" | "w8a8" | "w8a16" | "bnb_8bit" | "bitsandbytes_8bit" | "load_in_8bit" => {
            Some(1.0625)
        }
        // 2-bit (rzadkie ale istnieje)
        "int2" | "w2a16" => Some(0.3125),
        // 16-bit warianty
        "fp16" | "float16" | "bf16" | "bfloat16" | "f16" => Some(2.0),
        "fp32" | "float32" | "f32" => Some(4.0),
        _ => None,
    }
}

/// Bajty na parametr dla kwantyzacji MLX z jawnym `group_size`. MLX trzyma jeden
/// 16-bitowy skalar i jeden bias per grupa, czyli `32 bity / group_size` overheadu
/// ponad surowe `bits/8` bajtow. Przyklady: 4-bit g64 = 0.5 + 32/64/8 = 0.5625;
/// 4-bit g32 = 0.5 + 32/32/8 = 0.625; 8-bit g64 = 1.0 + 0.0625 = 1.0625.
pub fn mlx_weight_bytes(bits: u64, group_size: u64) -> f64 {
    let g = group_size.max(1) as f64;
    bits as f64 / 8.0 + 32.0 / g / 8.0
}

/// Heurystyka: wykrywa kwantyzacje na podstawie nazwy repo HF
/// (`User/Foo-NVFP4-turbo`, `Intel/x-int4-AutoRound`, `*-AWQ`, `*-GGUF-Q4_K_M` itd.).
/// Zwraca etykiete nadajaca sie do `quant_label_to_bytes` lub None.
pub fn detect_quant_from_name(repo: &str) -> Option<String> {
    let lower = repo.to_lowercase();
    // Kolejnosc wazna: bardziej specyficzne wzorce najpierw.
    let patterns: &[(&[&str], &str)] = &[
        (&["nvfp4"], "nvfp4"),
        (&["mxfp4"], "mxfp4"),
        (&["fp4"], "fp4"),
        (&["awq"], "awq"),
        (&["gptq"], "gptq"),
        (&["autoround"], "auto_round"),
        (&["w4a16"], "w4a16"),
        (
            &[
                "int4", "4bit", "4_bit", "q4_k", "q4_0", "q4_1", "gguf_q4", "gguf-q4",
            ],
            "int4",
        ),
        (&["w8a8"], "w8a8"),
        (&["w8a16"], "w8a16"),
        (&["fp8"], "fp8"),
        (
            &["int8", "8bit", "8_bit", "q8_0", "gguf_q8", "gguf-q8"],
            "int8",
        ),
    ];
    for (needles, label) in patterns {
        if needles.iter().any(|n| lower.contains(n)) {
            return Some((*label).to_string());
        }
    }
    None
}

/// Wyciaga etykiete quantization z pola `quantization_config` w HF config.json.
/// Obsluguje:
/// - `quant_method` (awq/gptq/bitsandbytes/fp8/compressed-tensors/modelopt/...)
/// - `bits` (2/4/8) - decyduje o szerokosci dla bitsandbytes/compressed-tensors
/// - `load_in_4bit` / `load_in_8bit` (bitsandbytes legacy fields)
pub fn quant_label_from_config(qc: &serde_json::Value) -> Option<String> {
    let obj = qc.as_object()?;
    let method = obj
        .get("quant_method")
        .and_then(|v| v.as_str())
        .map(|s| s.to_lowercase().replace('-', "_"))
        .unwrap_or_default();
    let bits = obj.get("bits").and_then(|v| v.as_u64()).unwrap_or(0);

    // bnb legacy: `load_in_4bit` / `load_in_8bit` bool flags.
    if obj
        .get("load_in_4bit")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return Some("bnb_4bit".into());
    }
    if obj
        .get("load_in_8bit")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return Some("bnb_8bit".into());
    }

    match method.as_str() {
        "awq" => Some("awq".into()),
        "gptq" => Some("gptq".into()),
        "fp8" => Some("fp8".into()),
        "bitsandbytes" => match bits {
            8 => Some("bnb_8bit".into()),
            _ => Some("bnb_4bit".into()),
        },
        "compressed_tensors" => match bits {
            8 => Some("w8a16".into()),
            _ => Some("compressed_tensors_4bit".into()),
        },
        "modelopt" => match bits {
            8 => Some("modelopt_fp8".into()),
            _ => Some("nvfp4".into()),
        },
        "nvfp4" | "fp4" | "mxfp4" => Some(method),
        "" => None,
        // Nieznany method - zwroc surowo, caller moze sparsowac przez bits.
        other => match bits {
            4 => Some("int4".into()),
            8 => Some("int8".into()),
            _ => Some(other.into()),
        },
    }
}

/// Konsolidowana detekcja: override (manual z UI) -> hf config -> nazwa repo.
/// Zwraca etykiete kwantyzacji lub None gdy model jest pelnoprecyzyjny.
pub fn detect_quantization(
    repo: &str,
    hf_config: &serde_json::Value,
    override_label: Option<&str>,
) -> Option<String> {
    if let Some(o) = override_label {
        let trimmed = o.trim();
        if !trimmed.is_empty() {
            // Specjalny token "none"/"auto" wylacza override.
            let lower = trimmed.to_lowercase();
            if lower != "none" && lower != "auto" && lower != "off" {
                return Some(trimmed.to_string());
            }
        }
    }
    if let Some(qc) = hf_config.get("quantization_config") {
        if let Some(label) = quant_label_from_config(qc) {
            return Some(label);
        }
    }
    detect_quant_from_name(repo)
}

/// Silnik inferencji dla ktorego liczymy VRAM. Fizyka pamieci rozni sie
/// fundamentalnie: vLLM trzyma staly ~5 GB workspace per worker, llama.cpp to
/// jeden proces ze split-mode (compute buffer rzedu setek MB, KV liczony dla
/// calego `-c`, nie `-c × max_num_seqs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeployEngine {
    Vllm,
    LlamaCpp,
    /// Apple MLX (mlx-lm / mlx-swift). Unified memory: JEDNO urzadzenie, brak
    /// TP/PP/split-mode i brak ~5 GB workspace vLLM. Budzet pamieci pochodzi z
    /// wired-limit, nie z liczby kart.
    Mlx,
}

impl Default for DeployEngine {
    fn default() -> Self {
        DeployEngine::Vllm
    }
}

/// Bajty per element KV cache dla danego silnika i etykiety typu cache. To
/// JEDYNE zrodlo prawdy dla szerokosci KV — uzywane przez estymacje, auto_fit
/// i builder argow, zeby estymacja i wdrozona komenda nigdy sie nie rozjechaly.
///
/// `None` = etykieta nieprawidlowa dla tego silnika (caller decyduje co zrobic;
/// estymacja fallbackuje do 2.0 jak fp16). Etykieta normalizowana: lowercase,
/// '-' -> '_'; `auto` mapuje per silnik na natywny domyslny typ.
///
/// Wartosci llama.cpp to `block_bytes / 32` z ggml-common.h (rozmiar bloku 32
/// elementow): q8_0 = 34/32 = 1.0625, q5_1 = 24/32 = 0.75, q5_0 = 22/32 = 0.6875,
/// q4_1 = 20/32 = 0.625, q4_0 = 18/32 = 0.5625, iq4_nl = 18/32 = 0.5625.
pub fn kv_bytes_per_element(engine: DeployEngine, label: &str) -> Option<f64> {
    let l = label.to_lowercase().replace('-', "_");
    match engine {
        DeployEngine::Vllm => match l.as_str() {
            "auto" | "f16" | "fp16" | "bf16" | "bfloat16" | "float16" => Some(2.0),
            "fp8" | "fp8_e4m3" | "fp8_e5m2" => Some(1.0),
            _ => None,
        },
        DeployEngine::LlamaCpp => match l.as_str() {
            "auto" | "f16" | "fp16" | "bf16" | "bfloat16" => Some(2.0),
            "q8_0" => Some(1.0625),
            "q5_1" => Some(0.75),
            "q5_0" => Some(0.6875),
            "q4_1" => Some(0.625),
            "q4_0" => Some(0.5625),
            "iq4_nl" => Some(0.5625),
            _ => None,
        },
        // MLX QuantizedKVCache: kv8 ~ q8_0 (1.0625), kv4 ~ q4_0 (0.5625).
        DeployEngine::Mlx => match l.as_str() {
            "none" | "auto" | "f16" | "fp16" | "bf16" | "bfloat16" => Some(2.0),
            "kv8" | "q8" => Some(1.0625),
            "kv4" | "q4" => Some(0.5625),
            _ => None,
        },
    }
}

/// Konfiguracja runtime do estymacji.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VramEstimateInput {
    /// Silnik docelowy — dispatch fizyki VRAM (`estimate_vram`).
    #[serde(default)]
    pub engine: DeployEngine,
    pub gpu_count: u32,
    pub gpu_memory_gb_each: f64,
    pub tensor_parallel: u32,
    pub pipeline_parallel: u32,
    pub max_model_len: u64,
    pub max_num_seqs: u64,
    /// vLLM `--max-num-batched-tokens`: liczba tokenow w jednym kroku schedulera.
    /// Driver szczytu aktywacji (residual + bufory MLP skaluja sie z nim, NIE
    /// z liczba parametrow modelu).
    pub max_num_batched_tokens: u64,
    /// `auto` (=fp16), `fp16`, `bfloat16`, `fp8`
    pub kv_cache_dtype: String,
    /// Osobny typ V cache dla llama.cpp (K=q8_0, V=q4_0). `None` = uzyj
    /// `kv_cache_dtype` dla obu (vLLM/MLX nie maja osobnego V).
    pub kv_cache_dtype_v: Option<String>,
    /// vLLM `--gpu-memory-utilization` (0.0–1.0). Default 0.9.
    pub gpu_memory_utilization: f64,
    /// Activation memory overhead jako % weights+kv. Empirycznie 8-15%.
    pub activation_overhead_pct: f64,
    /// Dokladny rozmiar wag w bajtach. Dla GGUF plik .gguf JEST dokladnym
    /// skwantyzowanym footprintem wag, wiec nie liczymy ich z params×bytes_per_param.
    /// `None` = licz z ModelSpec (klasyczne safetensors). `Some` = uzyj wprost.
    pub weights_bytes_override: Option<u64>,
}

impl Default for VramEstimateInput {
    fn default() -> Self {
        Self {
            engine: DeployEngine::default(),
            gpu_count: 1,
            gpu_memory_gb_each: 24.0,
            tensor_parallel: 1,
            pipeline_parallel: 1,
            max_model_len: 8192,
            max_num_seqs: 256,
            max_num_batched_tokens: 8192,
            kv_cache_dtype: "auto".to_string(),
            kv_cache_dtype_v: None,
            gpu_memory_utilization: 0.9,
            activation_overhead_pct: 10.0,
            weights_bytes_override: None,
        }
    }
}

impl VramEstimateInput {
    /// Etykieta typu K cache (zawsze `kv_cache_dtype`).
    fn k_label(&self) -> &str {
        &self.kv_cache_dtype
    }

    /// Etykieta typu V cache: osobny `kv_cache_dtype_v` gdy podany, inaczej K.
    fn v_label(&self) -> &str {
        self.kv_cache_dtype_v
            .as_deref()
            .unwrap_or(&self.kv_cache_dtype)
    }
}

/// Wynik estymacji VRAM per GPU + warnings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VramEstimate {
    pub model_weights_gb: f64,
    pub kv_cache_gb: f64,
    pub activations_gb: f64,
    pub overhead_gb: f64,
    pub total_gb: f64,
    /// VRAM per pojedynczy GPU (po podzialu przez TP*PP).
    pub per_gpu_gb: f64,
    pub fits_per_gpu: bool,
    pub fits_total: bool,
    /// Pula KV (cluster-wide) = `util*VRAM - weights - activations` zsumowana po
    /// wszystkich GPU. Dla vLLM to resztkowa pamiec dostepna dla PagedAttention,
    /// NIE iloczyn ctx*seqs. Dla llama.cpp = realne KV calego `-c`.
    pub kv_pool_gb: f64,
    /// Ile tokenow KV miesci pula na JEDNEJ GPU (vLLM). Limit twardy: musi byc
    /// >= max_model_len, inaczej vLLM odrzuci start (max seq len > KV cache).
    pub pool_tokens: u64,
    /// Ile pelnych sekwencji `max_model_len` miesci pula (`pool_tokens / ctx`).
    /// Wartosc informacyjna o osiagalnej wspolbieznosci, nie limit pamieci.
    pub concurrent_full_len_seqs: f64,
    pub warnings: Vec<String>,
}

/// Dispatch estymacji VRAM po silniku - fizyka pamieci rozni sie fundamentalnie
/// (patrz `DeployEngine`). Binary-search budzetu (ctx/seqs/parallel) MUSI isc
/// przez te funkcje, zeby fizyka odpowiadala faktycznemu silnikowi.
pub fn estimate_vram(model: &ModelSpec, input: &VramEstimateInput) -> VramEstimate {
    match input.engine {
        DeployEngine::Vllm => estimate_vllm_vram(model, input),
        DeployEngine::LlamaCpp => estimate_llamacpp_vram(model, input),
        DeployEngine::Mlx => estimate_mlx_vram(model, input),
    }
}

/// Estymacja VRAM dla vLLM wedlug modelu PULI KV.
///
/// vLLM nie prealokuje siatki `max_num_seqs × max_model_len`. Po zaladowaniu wag
/// i sprofilowaniu aktywacji tworzy JEDNA pule KV (PagedAttention):
/// `KV_pool = util*VRAM - weights - activations`. `max_num_seqs` to limit admisji
/// schedulera, NIE rozmiar pamieci — dlatego nie wchodzi do zadnej formuly pamieci.
/// Fit zalezy od dwoch warunkow: staly footprint miesci sie w `util*VRAM` ORAZ pula
/// pomiesci co najmniej jedna pelna sekwencje (`pool_tokens >= max_model_len`).
pub fn estimate_vllm_vram(model: &ModelSpec, input: &VramEstimateInput) -> VramEstimate {
    let mut warnings: Vec<String> = Vec::new();

    // Weights: gdy override podany (GGUF - dokladny rozmiar pliku to footprint wag)
    // uzywamy go wprost; inaczej pelne parametry × bytes_per_param (safetensors).
    // MoE: w vllm ladowane sa wszystkie experty, wiec liczymy pelne params.
    let model_weights_bytes = match input.weights_bytes_override {
        Some(bytes) => bytes as f64,
        None => model.estimated_params() as f64 * model.bytes_per_param(),
    };
    let model_weights_gb = bytes_to_gib(model_weights_bytes);

    let tp = input.tensor_parallel.max(1) as f64;
    let pp = input.pipeline_parallel.max(1) as f64;
    let parallel = tp * pp;

    let weights_per_gpu = model_weights_gb / parallel;

    // Szczyt aktywacji skaluje sie z liczba tokenow w kroku schedulera, nie z liczba
    // parametrow: residual stream (2*hidden) + jeden bufor MLP intermediate, razy
    // szerokosc aktywacji (bf16/fp16 = 2 bajty). Do tego staly koszt CUDA graphs
    // i bufory NCCL przy TP>1.
    let act_dtype_bytes = 2.0;
    let activation_peak = bytes_to_gib(
        input.max_num_batched_tokens as f64
            * (2.0 * model.hidden_size as f64 + model.intermediate_size.max(1) as f64)
            * act_dtype_bytes,
    );
    let cuda_graph_const = 1.5;
    let nccl = if tp > 1.0 { 0.3 * tp } else { 0.0 };
    let activations_per_gpu = activation_peak + cuda_graph_const + nccl;
    let activations_gb = activations_per_gpu * parallel; // cluster-wide (informational)
    let overhead_gb = 0.5; // CUDA runtime, allocator metadata - per cluster

    let required_fixed_per_gpu = weights_per_gpu + activations_per_gpu;

    // KV shardsuje sie tylko do `min(tp, kv_heads)` — powyzej vLLM REPLIKUJE glowy
    // KV (kazdy rank trzyma >=1 cala glowe), wiec per-GPU KV przestaje malec. PP
    // dzieli warstwy osobno, wiec szerokosc tokena dzieli sie dodatkowo przez pp.
    let kv_heads = if model.num_key_value_heads > 0 {
        model.num_key_value_heads
    } else {
        model.num_attention_heads.max(1)
    };
    let kv_tp_shards = (input.tensor_parallel as u64).min(kv_heads).max(1) as f64;
    // Effective per-token KV width: SWA layers stop growing past the window, so
    // the width depends on the context being budgeted (one full sequence).
    let kv_one_seq_bytes = model.kv_bytes_for_ctx(
        input.engine,
        &input.kv_cache_dtype,
        &input.kv_cache_dtype,
        input.max_model_len,
    );
    let kv_per_token_total = if input.max_model_len > 0 {
        kv_one_seq_bytes / input.max_model_len as f64
    } else {
        0.0
    };
    let kv_per_token_per_gpu = kv_per_token_total / (kv_tp_shards * pp);

    let usable_per_gpu = input.gpu_memory_gb_each * input.gpu_memory_utilization;
    let kv_pool_per_gpu = (usable_per_gpu - required_fixed_per_gpu).max(0.0);
    let kv_pool_per_gpu_bytes = kv_pool_per_gpu * 1024.0 * 1024.0 * 1024.0;
    let pool_tokens = if kv_per_token_per_gpu > 0.0 {
        (kv_pool_per_gpu_bytes / kv_per_token_per_gpu).floor() as u64
    } else {
        0
    };
    let concurrent_full_len_seqs = if input.max_model_len > 0 {
        pool_tokens as f64 / input.max_model_len as f64
    } else {
        0.0
    };

    // Pula KV cluster-wide do paska UI — kazda GPU trzyma osobna pule tej samej
    // wielkosci, wiec roll-up to per-GPU × liczba shardow.
    let kv_cache_gb = kv_pool_per_gpu * parallel;
    let kv_pool_gb = kv_cache_gb;

    let total_gb = model_weights_gb + activations_gb + kv_pool_gb + overhead_gb;
    let per_gpu_gb = required_fixed_per_gpu + kv_pool_per_gpu;

    // Walidacja TP/PP vs model heads/layers. `num_attention_heads % tp` i
    // `layers % pp` sa twarde (vLLM odrzuci). `kv_heads % tp` NIE jest bledem:
    // przy TP > kv_heads vLLM replikuje glowy KV (legalne, lecz bez dalszej
    // oszczednosci pamieci per-GPU).
    if model.num_attention_heads > 0
        && model.num_attention_heads % input.tensor_parallel as u64 != 0
    {
        warnings.push(format!(
            "tensor_parallel={} nie dzieli num_attention_heads={} - vLLM odrzuci konfiguracje",
            input.tensor_parallel, model.num_attention_heads
        ));
    }
    if model.num_key_value_heads > 0 && (input.tensor_parallel as u64) > model.num_key_value_heads {
        warnings.push(format!(
            "tensor_parallel={} > num_key_value_heads={} - KV replikowane miedzy ranki, \
             brak dalszej oszczednosci KV per-GPU powyzej TP={}",
            input.tensor_parallel, model.num_key_value_heads, model.num_key_value_heads
        ));
    }
    if model.num_hidden_layers > 0 && model.num_hidden_layers % input.pipeline_parallel as u64 != 0
    {
        warnings.push(format!(
            "pipeline_parallel={} nie dzieli num_hidden_layers={} - vLLM odrzuci konfiguracje",
            input.pipeline_parallel, model.num_hidden_layers
        ));
    }
    if parallel as u32 > input.gpu_count {
        warnings.push(format!(
            "TP*PP = {} > liczba GPU {} - brak GPU dla wszystkich shardow",
            parallel as u32, input.gpu_count
        ));
    }

    let fits_per_gpu =
        required_fixed_per_gpu <= usable_per_gpu && pool_tokens >= input.max_model_len;
    // DRUG-1: budzet calkowity respektuje util (vLLM nie ma dostepu do calego VRAM)
    // i nigdy nie jest spelniony gdy ktorakolwiek GPU nie miesci stalego footprintu.
    let fits_total = fits_per_gpu
        && total_gb
            <= input.gpu_memory_gb_each * input.gpu_count as f64 * input.gpu_memory_utilization;

    if required_fixed_per_gpu > usable_per_gpu {
        warnings.push(format!(
            "Staly footprint per GPU {:.1} GB (wagi + aktywacje) > dostepne {:.1} GB \
             ({}% z {:.1} GB) - OOM przy starcie",
            required_fixed_per_gpu,
            usable_per_gpu,
            (input.gpu_memory_utilization * 100.0) as u32,
            input.gpu_memory_gb_each
        ));
    } else if pool_tokens < input.max_model_len {
        warnings.push(format!(
            "pula KV miesci {} tokenow < max_model_len {} - vLLM odrzuci (max seq len > KV cache); \
             zwieksz liczbe GPU, uzyj fp8 KV albo zmniejsz max_model_len",
            pool_tokens, input.max_model_len
        ));
    }

    if model.has_vision || model.has_audio {
        warnings.push(
            "Model multimodalny (vision/audio) - dodaj --max-num-batched-tokens 8192 \
             --enable-chunked-prefill, encoder cache nie jest tu policzony"
                .to_string(),
        );
    }

    VramEstimate {
        model_weights_gb,
        kv_cache_gb,
        activations_gb,
        overhead_gb,
        total_gb,
        per_gpu_gb,
        fits_per_gpu,
        fits_total,
        kv_pool_gb,
        pool_tokens,
        concurrent_full_len_seqs,
        warnings,
    }
}

/// Fizyczny ubatch llama.cpp (`-ub`, default 512). Compute buffer i logits skaluja
/// sie z nim, nie z `max_num_seqs`.
const LLAMACPP_UBATCH: f64 = 512.0;
/// Realny primary CUDA context per device (GB) — alokowany na kazdej karcie
/// trzymajacej fragment modelu.
const LLAMACPP_CUDA_CTX_PER_GPU: f64 = 0.40;

/// Compute buffer llama.cpp (GB): logits dla aktywnych sekwencji + scratch
/// aktywacji grafu. Siedzi glownie na karcie main, dlatego liczony bez podzialu
/// przez GPU. Server liczy logits TYLKO dla ostatniego tokena kazdej aktywnej
/// sekwencji (`vocab * n_active_seq * 4`), nie dla calego ubatch — `vocab*ubatch`
/// zawyzalo decode ~512x.
fn llamacpp_compute_buffer_gb(model: &ModelSpec, max_num_seqs: u64) -> f64 {
    let logits = model.vocab_size as f64 * max_num_seqs.max(1) as f64 * 4.0;
    // ~6 zywych tensorow aktywacji w grafie forward (residual, attn, mlp...).
    let scratch = LLAMACPP_UBATCH * model.hidden_size as f64 * 4.0 * 6.0;
    bytes_to_gib(logits + scratch)
}

/// Estymacja VRAM dla llama.cpp: jeden proces, multi-GPU przez split-mode.
/// W przeciwienstwie do vLLM nie ma per-worker workspace ~5 GB — pamiec to wagi
/// + KV (caly `-c`, NIE razy max_num_seqs) + jeden compute buffer (setki MB) +
/// primary CUDA context per karta. TP/PP mapuja na liczbe kart w splicie.
pub fn estimate_llamacpp_vram(model: &ModelSpec, input: &VramEstimateInput) -> VramEstimate {
    let mut warnings: Vec<String> = Vec::new();

    // Wagi: GGUF override to dokladny footprint pliku; inaczej params×bytes_per_param.
    let model_weights_bytes = match input.weights_bytes_override {
        Some(bytes) => bytes as f64,
        None => model.estimated_params() as f64 * model.bytes_per_param(),
    };
    let weights_gb = bytes_to_gib(model_weights_bytes);

    // `max_model_len` to kontekst PER-REQUEST; llama-server dzieli `-c` na `-np`
    // slotow (`n_ctx_seq = n_ctx / n_seq_max`). Zeby kazdy slot dostal pelne
    // `max_model_len`, calkowity `-c` = max_model_len × max_num_seqs, wiec total
    // KV rosnie liniowo z liczba slotow (inaczej niz w vLLM page-cache).
    let seqs = input.max_num_seqs.max(1);
    let n_ctx = input.max_model_len * seqs;
    // Per-slot KV (SWA-aware: window-capped layers don't grow with ctx) × slots.
    let kv_one_seq_bytes = model.kv_bytes_for_ctx(
        input.engine,
        input.k_label(),
        input.v_label(),
        input.max_model_len,
    );
    let kv_cache_gb = bytes_to_gib(kv_one_seq_bytes * seqs as f64);

    let compute_buffer_gb = llamacpp_compute_buffer_gb(model, seqs);

    // llama.cpp uzywa JEDNEGO split-mode; TP i PP mapuja na liczbe kart splicie.
    let tp = input.tensor_parallel.max(1);
    let pp = input.pipeline_parallel.max(1);
    let gpus_used = ((tp * pp).min(input.gpu_count.max(1))).max(1) as f64;

    let weights_per_gpu = weights_gb / gpus_used;
    let overhead_gb = 0.3; // allocator / metadata

    // Cluster-wide activations (do paska UI): compute buffer + CUDA context na kazdej
    // karcie. To realny zywy footprint, nie sztuczne 5 GB × N jak w modelu vLLM.
    let activations_gb = compute_buffer_gb + LLAMACPP_CUDA_CTX_PER_GPU * gpus_used;
    let total_gb = weights_gb + kv_cache_gb + activations_gb + overhead_gb;

    // Per-GPU zalezy od trybu split. Row-split (tp>1): KV + compute (attention)
    // siedza na MAIN GPU i NIE dziela sie rowno — main = weights/gpus + PELNE KV
    // + compute + cuda; secondary = weights/gpus + cuda. Layer-split (pp>1, tp==1):
    // warstwa KV zyje na karcie tej warstwy, wiec KV dzieli sie rowno (kv/gpus).
    let per_gpu_gb = if tp > 1 {
        let main_gpu =
            weights_per_gpu + kv_cache_gb + compute_buffer_gb + LLAMACPP_CUDA_CTX_PER_GPU;
        let secondary = weights_per_gpu + LLAMACPP_CUDA_CTX_PER_GPU;
        main_gpu.max(secondary)
    } else {
        let kv_per_gpu = kv_cache_gb / gpus_used;
        weights_per_gpu + kv_per_gpu + compute_buffer_gb + LLAMACPP_CUDA_CTX_PER_GPU
    };

    if tp > 1 && pp > 1 {
        warnings.push(format!(
            "llama.cpp nie laczy split-mode row+layer jednoczesnie; uzyto row na wszystkich {} kartach",
            gpus_used as u32
        ));
    }

    // Domyslnie liczymy layer-split (KV shardsuje sie rowno). Gdy user wymusil
    // row (TP>1), KV nie dzieli sie liniowo - main GPU dostaje wiekszy fragment,
    // wiec model VRAM (kv/gpus_used) zanizają realne zuzycie na main GPU.
    if tp > 1 {
        warnings.push(
            "split-mode row: KV cache nie dzieli sie rowno miedzy karty (main GPU ciezsza) \
             - przy dlugim kontekscie mozliwy OOM na main GPU; rozwaz split-mode layer \
             (pipeline_parallel)"
                .to_string(),
        );
    }

    // Zadany split przekracza liczbe kart - estymata uzyla gpus_used (capped),
    // realny deploy nie ma tylu GPU dla shardow.
    if tp.max(1) * pp.max(1) > input.gpu_count {
        warnings.push(format!(
            "zadany split (TP×PP={}) przekracza liczbe GPU {} - uzyto {} kart",
            tp.max(1) * pp.max(1),
            input.gpu_count,
            gpus_used as u32
        ));
    }

    if gpus_used < input.gpu_count as f64 {
        warnings.push(format!(
            "pozostale {} GPU nieuzywane",
            input.gpu_count - gpus_used as u32
        ));
    }

    // Compute buffer wymaga vocab_size + hidden_size z metadanych GGUF. Gdy ich
    // brak liczymy 0 - user musi wiedziec ze ten skladnik jest pominiety.
    if model.vocab_size == 0 || model.hidden_size == 0 {
        warnings
            .push("compute buffer niepoliczony - brak vocab/hidden w metadanych GGUF".to_string());
    }

    let usable_per_gpu = input.gpu_memory_gb_each * input.gpu_memory_utilization;
    let fits_per_gpu = per_gpu_gb <= usable_per_gpu;
    let fits_total = total_gb <= input.gpu_memory_gb_each * input.gpu_count as f64;

    if !fits_per_gpu {
        warnings.push(format!(
            "VRAM per GPU {:.1} GB > dostepne {:.1} GB ({}% z {:.1} GB) - OOM przy starcie",
            per_gpu_gb,
            usable_per_gpu,
            (input.gpu_memory_utilization * 100.0) as u32,
            input.gpu_memory_gb_each
        ));
    }

    if model.has_vision || model.has_audio {
        warnings.push(
            "Model multimodalny (vision/audio) - projektor mmproj nie jest tu policzony"
                .to_string(),
        );
    }

    // KV llama.cpp to realne KV calego `-c` (= max_model_len × slotow): pool_tokens
    // = n_ctx, kv_pool_gb = KV, a wspolbieznosc to liczba slotow `-np`.
    VramEstimate {
        model_weights_gb: weights_gb,
        kv_cache_gb,
        activations_gb,
        overhead_gb,
        total_gb,
        per_gpu_gb,
        fits_per_gpu,
        fits_total,
        kv_pool_gb: kv_cache_gb,
        pool_tokens: n_ctx,
        concurrent_full_len_seqs: seqs as f64,
        warnings,
    }
}

/// Estymacja VRAM dla MLX (Apple unified memory): JEDNO urzadzenie, brak TP/PP
/// i split-mode, brak ~5 GB workspace vLLM. `gpu_memory_gb_each` niesie budzet
/// urzadzenia (wizard wysyla `mlx_max_memory_mb`), a `gpu_memory_utilization`
/// pelni role rezerwy dla OS (np. 0.9). Pamiec = wagi + scratch grafu + pula KV.
pub fn estimate_mlx_vram(model: &ModelSpec, input: &VramEstimateInput) -> VramEstimate {
    let mut warnings: Vec<String> = Vec::new();

    let model_weights_bytes = match input.weights_bytes_override {
        Some(bytes) => bytes as f64,
        None => model.estimated_params() as f64 * model.bytes_per_param(),
    };
    let weights_gb = bytes_to_gib(model_weights_bytes);

    // KV per-request × liczba sekwencji (mlx-lm batchuje). Single device, wiec
    // bez shardingu — pelna szerokosc tokena (SWA-aware per sequence).
    let seqs = input.max_num_seqs.max(1);
    let kv_one_seq_bytes = model.kv_bytes_for_ctx(
        input.engine,
        input.k_label(),
        input.v_label(),
        input.max_model_len,
    );
    let kv_cache_gb = bytes_to_gib(kv_one_seq_bytes * seqs as f64);

    // Graf MLX: residual + bufor MLP na tokeny batcha; brak osobnego CUDA-graph
    // const, ale 0.5 GB na bufory frameworka/Metal heap.
    let batch_tokens = input.max_num_batched_tokens.max(512) as f64;
    let scratch_gb = 0.5 + bytes_to_gib(batch_tokens * model.hidden_size as f64 * 2.0 * 4.0);
    let overhead_gb = 0.0; // brak osobnego allocatora poza scratch

    let budget_gb = input.gpu_memory_gb_each * input.gpu_memory_utilization;
    let required_gb = weights_gb + scratch_gb;

    let kv_pool_gb = (budget_gb - required_gb).max(0.0);
    // Pula tokenow liczona per-request KV (single device, brak shardingu).
    let kv_per_token_per_request = if input.max_model_len > 0 {
        (kv_one_seq_bytes / input.max_model_len as f64).max(1.0)
    } else {
        1.0
    };
    let pool_tokens = if kv_pool_gb > 0.0 {
        ((kv_pool_gb * 1024.0 * 1024.0 * 1024.0) / kv_per_token_per_request).floor() as u64
    } else {
        0
    };
    let concurrent_full_len_seqs = if input.max_model_len > 0 {
        pool_tokens as f64 / input.max_model_len as f64
    } else {
        0.0
    };

    let total_gb = required_gb + kv_cache_gb;
    let per_gpu_gb = total_gb;
    let fits = total_gb <= budget_gb;

    if input.tensor_parallel > 1 || input.pipeline_parallel > 1 {
        warnings.push(
            "MLX to pojedyncze urzadzenie (unified memory) - TP/PP nie maja \
             zastosowania, zignorowano"
                .to_string(),
        );
    }
    if weights_gb > budget_gb {
        warnings.push(format!(
            "Same wagi {:.1} GB przekraczaja budzet pamieci {:.1} GB ({}% z {:.1} GB) \
             - uzyj mocniejszej kwantyzacji albo zwieksz budzet",
            weights_gb,
            budget_gb,
            (input.gpu_memory_utilization * 100.0) as u32,
            input.gpu_memory_gb_each
        ));
    } else if !fits {
        warnings.push(format!(
            "wagi + KV + scratch {:.1} GB > budzet {:.1} GB - zmniejsz max_model_len, \
             max_num_seqs albo uzyj kv4",
            total_gb, budget_gb
        ));
    }
    if model.has_vision || model.has_audio {
        warnings.push(
            "Model multimodalny (vision/audio) - encoder/projektor nie jest tu policzony"
                .to_string(),
        );
    }

    VramEstimate {
        model_weights_gb: weights_gb,
        kv_cache_gb,
        activations_gb: scratch_gb,
        overhead_gb,
        total_gb,
        per_gpu_gb,
        fits_per_gpu: fits,
        fits_total: fits,
        kv_pool_gb,
        pool_tokens,
        concurrent_full_len_seqs,
        warnings,
    }
}

/// Wynik analizy zgodnosci liczby GPU z architektura modelu. GUI wykorzystuje
/// to do pokazania warning chip-a "5 GPU nie dzieli sie dobrze - rekomendowane
/// 4 lub 8" oraz listy sugerowanych counts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuCompatibilityReport {
    /// Faktyczne TP*PP wybrane przez recommend_parallelism. Moze byc < gpu_count
    /// gdy zaden podzial nie pasuje (fallback (1, gpu_count) - PP zwykle dziala).
    pub used_tp: u32,
    pub used_pp: u32,
    /// True gdy TP*PP == gpu_count (zadne GPU nieuzywane).
    pub uses_all_gpus: bool,
    /// True gdy partycja jest "czysta" - heads i layers podzielne idealnie
    /// (vllm nie odrzuca konfiguracji).
    pub clean_partition: bool,
    /// Lista liczb GPU dla ktorych model dzieli sie idealnie (TP*PP=N, heads
    /// i layers podzielne). Sortowana rosnaco. Pomaga user'owi wybrac
    /// "lepszy zestaw kart" (np. zamiast 5 wybrac 4 albo 6).
    pub better_gpu_counts: Vec<u32>,
    /// Komunikat warning gdy current setup nieoptymalny - do pokazania w GUI.
    pub warning: Option<String>,
}

/// Analizuje czy liczba GPU pasuje do architektury modelu i sugeruje lepsze
/// alternatywy. Zwraca raport ktorego user-facing warnings i listy mozna
/// pokazac w GUI Advanced step.
pub fn analyze_gpu_compatibility(spec: &ModelSpec, gpu_count: u32) -> GpuCompatibilityReport {
    let (tp, pp) = recommend_parallelism(spec, gpu_count);
    let uses_all = tp * pp == gpu_count;
    let heads = spec.num_attention_heads.max(1);
    let kv_heads = spec.num_key_value_heads.max(1);
    let layers = spec.num_hidden_layers.max(1);
    let clean =
        heads % (tp as u64) == 0 && kv_heads % (tp as u64) == 0 && layers % (pp as u64) == 0;

    // Lista "lepszych" gpu_counts dla tego modelu: szukamy w zakresie [1..16]
    // wszystkich N takich ze istnieje partycja TP*PP=N gdzie heads%TP=0,
    // kv_heads%TP=0, layers%PP=0.
    let mut better: Vec<u32> = Vec::new();
    for n in 1..=16u32 {
        for cand_tp in 1..=n {
            if n % cand_tp != 0 {
                continue;
            }
            let cand_pp = n / cand_tp;
            if heads % (cand_tp as u64) == 0
                && kv_heads % (cand_tp as u64) == 0
                && layers % (cand_pp as u64) == 0
            {
                better.push(n);
                break;
            }
        }
    }

    let warning =
        if !clean {
            Some(format!(
            "{} GPU nie dzieli sie idealnie dla tego modelu (heads={}, kv_heads={}, layers={}). \
             Wybrano TP={} PP={} jako fallback - czesc GPU moze byc nieoptymalnie wykorzystana \
             albo deploy moze sie nie udac. Lepsze liczby GPU: {}",
            gpu_count, heads, kv_heads, layers, tp, pp,
            better.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(", ")
        ))
        } else if !uses_all {
            Some(format!(
                "{} GPU - {} bedzie nieuzywane (TP={} PP={} = {}). \
             Lepsze liczby GPU: {}",
                gpu_count,
                gpu_count - tp * pp,
                tp,
                pp,
                tp * pp,
                better
                    .iter()
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        } else {
            None
        };

    GpuCompatibilityReport {
        used_tp: tp,
        used_pp: pp,
        uses_all_gpus: uses_all,
        clean_partition: clean,
        better_gpu_counts: better,
        warning,
    }
}

/// Wariant `analyze_gpu_compatibility` dla llama.cpp. llama.cpp NIE wymaga
/// podzielnosci num_attention_heads przez TP ani num_hidden_layers przez PP -
/// to ograniczenie vLLM. `--split-mode row` dzieli wiersze tensorow dowolnie,
/// `--split-mode layer` rozklada warstwy po N kartach. Domyslnie wybieramy
/// layer-split (PP=gpu_count, TP=1) - to natywny default llama.cpp i jedyny
/// tryb shardujacy KV rowno miedzy karty (warstwa KV zyje na karcie tej warstwy),
/// wlasciwy na PCIe bez NVLink. Dla N rownych kart uzywamy wszystkich, wiec
/// partycja jest zawsze "czysta" i nie ma warningow o heads/layers.
pub fn analyze_gpu_compatibility_llamacpp(
    _spec: &ModelSpec,
    gpu_count: u32,
) -> GpuCompatibilityReport {
    let gpus = gpu_count.max(1);
    GpuCompatibilityReport {
        used_tp: 1,
        used_pp: gpus,
        uses_all_gpus: true,
        clean_partition: true,
        better_gpu_counts: (1..=gpus).collect(),
        warning: None,
    }
}

/// Smart pick TP/PP dla danej liczby GPU + atrybutow modelu. Strategia:
/// 1. Jesli gpu_count = 1: TP=1, PP=1.
/// 2. Sprobuj TP=gpu_count (najprostsze, najnizszy comm overhead).
/// 3. Jesli TP nie dzieli heads/kv_heads, sprobuj rozkladow TP*PP=gpu_count
///    z TP < gpu_count (TP=2, PP=N/2; TP=4, PP=N/4; itd.).
/// 4. Wynik: pierwsza kombinacja ktora dzieli heads i layers.
pub fn recommend_parallelism(model: &ModelSpec, gpu_count: u32) -> (u32, u32) {
    if gpu_count <= 1 {
        return (1, 1);
    }
    let heads = model.num_attention_heads.max(1);
    let kv_heads = model.num_key_value_heads.max(1);
    let layers = model.num_hidden_layers.max(1);

    // Posortuj kandydatow TP od najwiekszego do 1 (preferuj TP nad PP - mniej latency).
    let mut candidates: Vec<(u32, u32)> = (1..=gpu_count)
        .filter(|tp| gpu_count % tp == 0)
        .map(|tp| (tp, gpu_count / tp))
        .collect();
    candidates.sort_by(|a, b| b.0.cmp(&a.0));

    for (tp, pp) in &candidates {
        if heads % (*tp as u64) == 0 && kv_heads % (*tp as u64) == 0 && layers % (*pp as u64) == 0 {
            return (*tp, *pp);
        }
    }
    // Fallback: TP=1, PP=gpu_count - PP dziala dla niemal kazdej liczby
    // layers (jesli nie podzielne, vllm i tak rzuci blad ale jest najmniej
    // restrictive niz TP).
    (1, gpu_count)
}

/// VRAM-aware parallelism picker. Iteruje dzielniki `gpu_count` i wybiera
/// najmniejsze TP*PP ktore (a) dzieli heads/layers czysto, (b) miesci weights +
/// minimalne KV (1024 ctx × 1 seq) + activations w `gpu_capacity × util`.
/// Gdy zaden nie pasuje - fallback `recommend_parallelism` (najszerszy podzial
/// dostepny architektonicznie). Zwraca (TP, PP).
///
/// `weights_bytes_override` MUSI byc tym samym dokladnym rozmiarem wag co finalny
/// estimate (GGUF: rozmiar pliku z `fetch_gguf_spec`). Bez niego probe liczyl wagi
/// heurystyka params×bytes - niedoszacowanie dla MoE/mixed-quant powodowalo dobor
/// zbyt waskiego TP/PP, ktory potem realnie raportowal OOM na shardzie. Sciezka
/// safetensors przekazuje `None` (heurystyka jest tam jedyna dostepna).
pub fn recommend_parallelism_vram_aware(
    model: &ModelSpec,
    engine: DeployEngine,
    gpu_count: u32,
    gpu_memory_gb_each: f64,
    gpu_memory_utilization: f64,
    weights_bytes_override: Option<u64>,
) -> (u32, u32) {
    if gpu_count <= 1 {
        return (1, 1);
    }
    let heads = model.num_attention_heads.max(1);
    let kv_heads = model.num_key_value_heads.max(1);
    let layers = model.num_hidden_layers.max(1);

    // Kandydaci czysci (TP*PP=gpu_count) + dzielniki heads/layers. Sortuj po TP
    // malejaco - preferuj maksymalne TP (idealnie TP=gpu_count, PP=1): na
    // datacenter GPU z NVLink (B300/H100) TP all-reduce jest tani, a wysokie TP
    // shardu je wagi I KV najrowniej. PP wchodzi tylko gdy najwyzsze TP nie
    // dzieli glowic albo nie miesci sie w VRAM (nizsze TP w kolejnych iteracjach).
    let mut candidates: Vec<(u32, u32)> = (1..=gpu_count)
        .filter(|tp| gpu_count % tp == 0)
        .map(|tp| (tp, gpu_count / tp))
        .filter(|(tp, pp)| {
            heads % (*tp as u64) == 0 && kv_heads % (*tp as u64) == 0 && layers % (*pp as u64) == 0
        })
        .collect();
    candidates.sort_by(|a, b| b.0.cmp(&a.0));

    for (tp, pp) in &candidates {
        let probe = VramEstimateInput {
            engine,
            gpu_count,
            gpu_memory_gb_each,
            tensor_parallel: *tp,
            pipeline_parallel: *pp,
            max_model_len: 1024,
            max_num_seqs: 1,
            max_num_batched_tokens: 8192,
            kv_cache_dtype: "auto".into(),
            kv_cache_dtype_v: None,
            gpu_memory_utilization,
            activation_overhead_pct: 10.0,
            weights_bytes_override,
        };
        let est = estimate_vram(model, &probe);
        if est.fits_per_gpu {
            return (*tp, *pp);
        }
    }

    // Brak konfiguracji ktora miesci weights - zwracamy najszersza partycje
    // (max TP = najmniej VRAM/GPU); recommend handler zglosi warning OOM.
    // Po sortowaniu malejacym to pierwszy kandydat.
    if let Some(widest_tp) = candidates.first() {
        return *widest_tp;
    }
    recommend_parallelism(model, gpu_count)
}

/// Wejscie auto-fit. `requested_*` to surowe wartosci od usera; `lock_*` mowi
/// czy backend ma je zachowac (true = nie obnizaj, traktuj jako sztywne) czy
/// moze auto-cap'owac do dopasowania VRAM.
#[derive(Debug, Clone)]
pub struct AutoFitRequest {
    pub engine: DeployEngine,
    pub gpu_count: u32,
    pub gpu_memory_gb_each: f64,
    pub kv_cache_dtype: String,
    /// Osobny typ V cache (llama.cpp). `None` = uzyj `kv_cache_dtype` dla obu.
    pub kv_cache_dtype_v: Option<String>,
    pub gpu_memory_utilization: f64,
    pub requested_max_model_len: Option<u64>,
    pub requested_max_num_seqs: Option<u64>,
    pub requested_tensor_parallel: Option<u32>,
    pub requested_pipeline_parallel: Option<u32>,
    pub lock_max_model_len: bool,
    pub lock_max_num_seqs: bool,
    pub lock_tensor_parallel: bool,
    /// Dokladny rozmiar wag w bajtach (GGUF). Propagowany do `applied`
    /// VramEstimateInput i uzywany do liczenia budzetu KV (GPU − wagi − activations).
    pub weights_bytes_override: Option<u64>,
}

/// Wynik auto-fit. `applied` zawiera realnie uzywane parametry. `auto_adjusted`
/// lista nazw pol obnizonych vs request. `at_limit` true gdy headroom < 5% albo
/// cokolwiek auto-cap'owane. `error` ustawione gdy jednoczesnie zalockowano
/// kombinacje przekraczajaca VRAM (locked params nie moga byc obnizone).
#[derive(Debug, Clone)]
pub struct AutoFitOutcome {
    pub applied: VramEstimateInput,
    pub auto_adjusted: Vec<String>,
    pub at_limit: bool,
    pub error: Option<String>,
}

/// Auto-fit: pick a configuration guaranteed to fit in VRAM.
///
/// Policy:
/// 1. TP/PP: locked/explicit user values win, otherwise engine-specific
///    recommendation (vLLM: VRAM-aware divisors; llama.cpp: layer-split on all
///    cards; MLX: single device).
/// 2. vLLM (KV pool model): `max_num_seqs` is a scheduler cap, not memory — the
///    fit checks ONE full sequence in the pool. The recommended seqs derives
///    from achievable pool concurrency (`floor(pool_tokens / ctx)`, clamped to
///    1..=256) instead of a blind 256, unless the user supplied a value.
/// 3. llama.cpp / MLX (KV scales linearly with slots): FULL CONTEXT FIRST, then
///    concurrency. Default seqs = 1; ctx is binary-searched to the largest
///    fitting value, and only when the model's full window fits at the current
///    slot count (and the user did not request/lock seqs) concurrency is scaled
///    up (2, 4, ..., 64).
/// 4. Locked params are never lowered; an impossible locked combination
///    returns `error`.
pub fn auto_fit_config(model: &ModelSpec, req: &AutoFitRequest) -> AutoFitOutcome {
    // 1. Wybor TP/PP. llama.cpp nie ma ograniczenia podzielnosci heads/layers
    // (split-mode row/layer dzieli dowolnie), wiec domyslnie uzywamy wszystkich
    // kart w jednym splicie. Default = layer-split (PP=gpu_count, TP=1): to
    // natywny tryb llama.cpp i jedyny shardujacy KV rowno (bezpieczny na PCIe
    // bez NVLink). Jawne TP od usera nadal wygrywa przez requested_*.unwrap_or.
    let (rec_tp, rec_pp) = match req.engine {
        DeployEngine::LlamaCpp => (1, req.gpu_count.max(1)),
        // MLX to pojedyncze urzadzenie (unified memory) - brak TP/PP.
        DeployEngine::Mlx => (1, 1),
        DeployEngine::Vllm => recommend_parallelism_vram_aware(
            model,
            req.engine,
            req.gpu_count,
            req.gpu_memory_gb_each,
            req.gpu_memory_utilization,
            req.weights_bytes_override,
        ),
    };
    // MLX ignoruje TP/PP nawet jesli user je poda; jedno urzadzenie => parallel=1.
    let (chosen_tp, chosen_pp) = match req.engine {
        DeployEngine::Mlx => (1, 1),
        _ => (
            req.requested_tensor_parallel.unwrap_or(rec_tp),
            req.requested_pipeline_parallel.unwrap_or(rec_pp),
        ),
    };
    let parallel = (chosen_tp.max(1) * chosen_pp.max(1)) as f64;
    // Etykieta V cache: osobna gdy podana, inaczej K (kv_cache_dtype).
    let v_dtype = req
        .kv_cache_dtype_v
        .as_deref()
        .unwrap_or(&req.kv_cache_dtype);

    // 2. Weights from override (GGUF exact file size) or params × bytes_per_param.
    let weights_bytes = match req.weights_bytes_override {
        Some(bytes) => bytes as f64,
        None => model.estimated_params() as f64 * model.bytes_per_param(),
    };
    let weights_gb = bytes_to_gib(weights_bytes);
    let weights_per_gpu = weights_gb / parallel;
    let usable_per_gpu = req.gpu_memory_gb_each * req.gpu_memory_utilization;
    let engine = req.engine;

    // Default concurrency policy per engine: vLLM seqs is a scheduler cap (pool
    // model, no memory), llama.cpp/MLX KV scales with slots so default is 1 and
    // the full context wins over concurrency.
    let default_seqs: u64 = match engine {
        DeployEngine::Vllm => 256,
        DeployEngine::LlamaCpp | DeployEngine::Mlx => 1,
    };
    let seqs_requested = req.requested_max_num_seqs.is_some();
    let req_seqs = req.requested_max_num_seqs.unwrap_or(default_seqs).max(1);

    // Engine-specific per-GPU activations. vLLM: scheduler-step activation peak
    // (8192 tokens, same default as VramEstimateInput) + CUDA-graph const + NCCL.
    // llama.cpp: one compute buffer (logits scale with the slot count) + primary
    // CUDA context. MLX: graph scratch + Metal heap.
    let activations_per_gpu_for = |seqs: u64| -> f64 {
        match engine {
            DeployEngine::Vllm => {
                let act_dtype_bytes = 2.0;
                let activation_peak = bytes_to_gib(
                    8192.0
                        * (2.0 * model.hidden_size as f64 + model.intermediate_size.max(1) as f64)
                        * act_dtype_bytes,
                );
                let nccl = if chosen_tp.max(1) > 1 {
                    0.3 * chosen_tp.max(1) as f64
                } else {
                    0.0
                };
                activation_peak + 1.5 + nccl
            }
            DeployEngine::LlamaCpp => {
                // Missing GGUF vocab/hidden would make the compute buffer ~0 and
                // over-credit the KV budget; floor it to protect against an OOM
                // on the real compute buffer.
                let compute = if model.vocab_size == 0 || model.hidden_size == 0 {
                    0.5
                } else {
                    llamacpp_compute_buffer_gb(model, seqs)
                };
                compute + LLAMACPP_CUDA_CTX_PER_GPU
            }
            DeployEngine::Mlx => 0.5 + bytes_to_gib(8192.0 * model.hidden_size as f64 * 2.0 * 4.0),
        }
    };

    // KV budget in bytes. vLLM: per-GPU pool. llama.cpp layer-split: KV spreads
    // evenly over all cards (budget × parallel); row-split keeps KV on the main
    // GPU (single-card budget). MLX: single device.
    let kv_budget_bytes_for = |seqs: u64| -> f64 {
        let per_gpu = (usable_per_gpu - weights_per_gpu - activations_per_gpu_for(seqs)).max(0.0);
        let gb = match engine {
            DeployEngine::Vllm | DeployEngine::Mlx => per_gpu,
            DeployEngine::LlamaCpp => {
                if chosen_tp.max(1) > 1 {
                    per_gpu
                } else {
                    per_gpu * parallel
                }
            }
        };
        gb * 1024.0 * 1024.0 * 1024.0
    };

    // Demand side: vLLM needs ONE full sequence in the pool, sharded across
    // min(tp, kv_heads) × pp (above kv_heads vLLM replicates heads); llama.cpp
    // and MLX allocate the full window per slot (× seqs).
    let kv_heads = if model.num_key_value_heads > 0 {
        model.num_key_value_heads
    } else {
        model.num_attention_heads.max(1)
    };
    let kv_tp_shards = (chosen_tp.max(1) as u64).min(kv_heads).max(1) as f64;
    let chosen_pp_f = chosen_pp.max(1) as f64;
    let kv_one_seq_bytes = |ctx: u64| -> f64 {
        match engine {
            // vLLM has a single --kv-cache-dtype (no separate V type).
            DeployEngine::Vllm => {
                model.kv_bytes_for_ctx(engine, &req.kv_cache_dtype, &req.kv_cache_dtype, ctx)
            }
            DeployEngine::LlamaCpp | DeployEngine::Mlx => {
                model.kv_bytes_for_ctx(engine, &req.kv_cache_dtype, v_dtype, ctx)
            }
        }
    };
    let kv_demand_bytes = |ctx: u64, seqs: u64| -> f64 {
        match engine {
            DeployEngine::Vllm => kv_one_seq_bytes(ctx) / (kv_tp_shards * chosen_pp_f),
            DeployEngine::LlamaCpp | DeployEngine::Mlx => {
                kv_one_seq_bytes(ctx) * seqs.max(1) as f64
            }
        }
    };
    let fits =
        |ctx: u64, seqs: u64| -> bool { kv_demand_bytes(ctx, seqs) <= kv_budget_bytes_for(seqs) };

    let make_applied = |ctx: u64, seqs: u64| -> VramEstimateInput {
        VramEstimateInput {
            engine: req.engine,
            gpu_count: req.gpu_count,
            gpu_memory_gb_each: req.gpu_memory_gb_each,
            tensor_parallel: chosen_tp,
            pipeline_parallel: chosen_pp,
            max_model_len: ctx,
            max_num_seqs: seqs,
            max_num_batched_tokens: 8192,
            kv_cache_dtype: req.kv_cache_dtype.clone(),
            kv_cache_dtype_v: req.kv_cache_dtype_v.clone(),
            gpu_memory_utilization: req.gpu_memory_utilization,
            activation_overhead_pct: 10.0,
            weights_bytes_override: req.weights_bytes_override,
        }
    };

    if kv_budget_bytes_for(req_seqs) <= 0.0 {
        return AutoFitOutcome {
            applied: make_applied(req.requested_max_model_len.unwrap_or(2048), req_seqs),
            auto_adjusted: Vec::new(),
            at_limit: true,
            error: Some(format!(
                "Wagi modelu ({:.1} GB / GPU) + activations ({:.1} GB) przekraczaja \
                 dostepne {:.1} GB - zwieksz liczbe GPU lub uzyj quantization",
                weights_per_gpu,
                activations_per_gpu_for(req_seqs),
                usable_per_gpu
            )),
        };
    }

    let absolute_ctx_ceiling: u64 = 1_048_576;
    let model_ctx_ceiling = if model.max_position_embeddings > 0 {
        model.max_position_embeddings.min(absolute_ctx_ceiling)
    } else {
        absolute_ctx_ceiling
    };
    let req_ctx = req
        .requested_max_model_len
        .unwrap_or(model_ctx_ceiling.max(2048))
        .max(512);

    // Largest fitting ctx in [512, ceiling] for a fixed slot count. Valid binary
    // search: kv_bytes_for_ctx is monotonic piecewise-linear in ctx.
    let largest_fitting_ctx = |seqs: u64, ceiling: u64| -> u64 {
        let ceiling = ceiling.max(512);
        if fits(ceiling, seqs) {
            return ceiling;
        }
        let mut lo: u64 = 512;
        let mut hi = ceiling;
        while lo + 256 < hi {
            let mid = (lo + hi) / 2;
            if fits(mid, seqs) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    };
    // Round DOWN to a multiple of 1024, but never below 512 and never above the
    // fitted value (rounding up past the VRAM limit would break the fit).
    let round_ctx = |fit: u64| -> u64 { ((fit / 1024) * 1024).max(512).min(fit.max(512)) };

    // Achievable vLLM pool concurrency at the chosen ctx — the recommendation
    // shown/applied when the user did not pick a seqs value themselves.
    let vllm_pool_seqs = |ctx: u64| -> u64 {
        if ctx == 0 {
            return 1;
        }
        let per_token = kv_one_seq_bytes(ctx) / ctx as f64 / (kv_tp_shards * chosen_pp_f);
        if per_token <= 0.0 {
            return 1;
        }
        let pool_tokens = (kv_budget_bytes_for(1) / per_token).floor() as u64;
        (pool_tokens / ctx).clamp(1, 256)
    };
    // llama.cpp/MLX concurrency scale-up: largest slot count keeping ctx fitting.
    let scale_up_seqs = |ctx: u64, start: u64| -> u64 {
        let mut best = start;
        for cand in [2u64, 4, 8, 16, 32, 64] {
            if cand <= start {
                continue;
            }
            if fits(ctx, cand) {
                best = cand;
            } else {
                break;
            }
        }
        best
    };

    let mut auto_adjusted: Vec<String> = Vec::new();
    let (final_ctx, final_seqs) = match (req.lock_max_model_len, req.lock_max_num_seqs) {
        (true, true) => {
            // Both locked: overflow means the locked combination cannot fit.
            if !fits(req_ctx, req_seqs) {
                return AutoFitOutcome {
                    applied: make_applied(req_ctx, req_seqs),
                    auto_adjusted: Vec::new(),
                    at_limit: true,
                    error: Some(format!(
                        "Locked max_model_len={} wymaga {:.1} GB puli KV \
                         ale budget per GPU to {:.1} GB. Zmniejsz max_model_len, zwieksz \
                         liczbe GPU albo uzyj fp8 KV.",
                        req_ctx,
                        bytes_to_gib(kv_demand_bytes(req_ctx, req_seqs)),
                        bytes_to_gib(kv_budget_bytes_for(req_seqs))
                    )),
                };
            }
            (req_ctx, req_seqs)
        }
        (true, false) => {
            // ctx locked, seqs free. If the locked ctx alone (one slot) does not
            // fit, seqs cannot help — return error.
            if !fits(req_ctx, 1) {
                return AutoFitOutcome {
                    applied: make_applied(req_ctx, req_seqs),
                    auto_adjusted: Vec::new(),
                    at_limit: true,
                    error: Some(format!(
                        "Locked max_model_len={} wymaga {:.1} GB puli KV na jedna sekwencje \
                         ale budget per GPU to {:.1} GB. Odblokuj max_model_len albo zwieksz \
                         liczbe GPU/uzyj fp8 KV.",
                        req_ctx,
                        bytes_to_gib(kv_demand_bytes(req_ctx, 1)),
                        bytes_to_gib(kv_budget_bytes_for(1))
                    )),
                };
            }
            match engine {
                // vLLM: seqs is a scheduler cap — keep the user value, otherwise
                // recommend the achievable pool concurrency.
                DeployEngine::Vllm => {
                    let seqs = if seqs_requested {
                        req_seqs
                    } else {
                        vllm_pool_seqs(req_ctx)
                    };
                    (req_ctx, seqs)
                }
                DeployEngine::LlamaCpp | DeployEngine::Mlx => {
                    let mut seqs = req_seqs;
                    while seqs > 1 && !fits(req_ctx, seqs) {
                        seqs /= 2;
                    }
                    if seqs < req_seqs {
                        auto_adjusted.push("max_num_seqs".into());
                    }
                    if !seqs_requested {
                        seqs = scale_up_seqs(req_ctx, seqs);
                    }
                    (req_ctx, seqs)
                }
            }
        }
        (false, true) => {
            // seqs locked — scale ctx down to fit (never above the request).
            let fit = largest_fitting_ctx(req_seqs, req_ctx);
            let capped = round_ctx(fit).min(req_ctx);
            if capped < req_ctx {
                auto_adjusted.push("max_model_len".into());
            }
            (capped, req_seqs)
        }
        (false, false) => {
            // No locks: full context first. Shrink slots only when even ctx=512
            // does not fit at the requested slot count.
            let mut seqs = req_seqs;
            if !matches!(engine, DeployEngine::Vllm) {
                while seqs > 1 && !fits(512, seqs) {
                    seqs /= 2;
                }
                if seqs < req_seqs && seqs_requested {
                    auto_adjusted.push("max_num_seqs".into());
                }
            }
            let fit = largest_fitting_ctx(seqs, model_ctx_ceiling);
            let ctx = round_ctx(fit);
            if ctx < req_ctx {
                auto_adjusted.push("max_model_len".into());
            }
            match engine {
                DeployEngine::Vllm => {
                    let s = if seqs_requested {
                        req_seqs
                    } else {
                        vllm_pool_seqs(ctx)
                    };
                    (ctx, s)
                }
                DeployEngine::LlamaCpp | DeployEngine::Mlx => {
                    // Scale concurrency only once the model's FULL window fits.
                    let s = if !seqs_requested && fit >= model_ctx_ceiling {
                        scale_up_seqs(ctx, seqs)
                    } else {
                        seqs
                    };
                    (ctx, s)
                }
            }
        }
    };

    // at_limit: anything auto-adjusted or KV headroom below 5%.
    let used_kv_bytes = kv_demand_bytes(final_ctx, final_seqs);
    let kv_budget_bytes = kv_budget_bytes_for(final_seqs);
    let headroom = (kv_budget_bytes - used_kv_bytes) / kv_budget_bytes.max(1.0);
    let at_limit = !auto_adjusted.is_empty() || headroom < 0.05;

    AutoFitOutcome {
        applied: make_applied(final_ctx, final_seqs),
        auto_adjusted,
        at_limit,
        error: None,
    }
}

/// Maksymalny `max_model_len` ktory zmiesci sie przy danej konfiguracji + batch.
/// Iteracyjnie redukuje ctx_len az kv_cache + weights + overhead miesci sie w VRAM.
pub fn max_context_for_budget(model: &ModelSpec, input: &VramEstimateInput) -> u64 {
    let mut lo: u64 = 512;
    let mut hi: u64 = model
        .max_position_embeddings
        .max(input.max_model_len)
        .max(8192);
    // Binary search do najwiekszego ctx_len ktory fits.
    while lo + 256 < hi {
        let mid = (lo + hi) / 2;
        let mut try_input = input.clone();
        try_input.max_model_len = mid;
        let est = estimate_vram(model, &try_input);
        if est.fits_per_gpu {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// Maksymalna osiagalna wspolbieznosc (pelnych sekwencji `max_model_len`) przy
/// zadanym ctx_len. W modelu puli vLLM fit NIE zalezy od `max_num_seqs` (to cap
/// schedulera, nie pamiec), wiec binary search po seqs zwracalby bez sensu `hi`.
/// Zamiast tego liczymy jedna estymate i czytamy `concurrent_full_len_seqs`
/// (ile pelnych sekwencji miesci pula KV). llama.cpp i MLX skaluja KV z liczba
/// slotow (n_ctx = max_model_len × seqs), wiec tam binary search po `-np`/seqs.
pub fn max_concurrent_seqs_for_budget(model: &ModelSpec, input: &VramEstimateInput) -> u64 {
    match input.engine {
        DeployEngine::Vllm => {
            let est = estimate_vram(model, input);
            est.concurrent_full_len_seqs.floor().max(1.0) as u64
        }
        DeployEngine::LlamaCpp | DeployEngine::Mlx => {
            let mut lo: u64 = 1;
            let mut hi: u64 = 1024;
            while lo + 4 < hi {
                let mid = (lo + hi) / 2;
                let mut try_input = input.clone();
                try_input.max_num_seqs = mid;
                let est = estimate_vram(model, &try_input);
                if est.fits_per_gpu {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            lo
        }
    }
}

/// Parsuj HF config.json (przekazany jako serde_json::Value). Obsluguje
/// `text_config` zagnieżdżony (multimodal). Wykrywa quantization z
/// `quantization_config` lub nazwy modelu.
pub fn parse_hf_config(config_json: &serde_json::Value, model_name: &str) -> Result<ModelSpec> {
    parse_hf_config_with_override(config_json, model_name, None)
}

/// Wariant `parse_hf_config` z manualnym override quantization (z UI/API).
/// Override ma najwyzszy priorytet; potem `quantization_config` w HF; potem
/// heurystyka z nazwy repo.
pub fn parse_hf_config_with_override(
    config_json: &serde_json::Value,
    model_name: &str,
    quantization_override: Option<&str>,
) -> Result<ModelSpec> {
    let cfg = config_json
        .as_object()
        .ok_or_else(|| anyhow!("config.json nie jest obiektem JSON"))?;

    let text_cfg = cfg
        .get("text_config")
        .and_then(|v| v.as_object())
        .unwrap_or(cfg);

    let pick_u64 = |obj: &serde_json::Map<String, serde_json::Value>, key: &str| -> u64 {
        obj.get(key).and_then(|v| v.as_u64()).unwrap_or(0)
    };

    let pick_u64_either = |key: &str| -> u64 {
        let v = pick_u64(text_cfg, key);
        if v > 0 {
            v
        } else {
            pick_u64(cfg, key)
        }
    };

    // Pierwszy klucz z listy aliasow ktory niesie dodatnia wartosc (configy MoE
    // uzywaja roznych nazw: num_experts / num_local_experts / n_routed_experts).
    let pick_u64_aliases = |keys: &[&str]| -> u64 {
        for key in keys {
            let v = pick_u64_either(key);
            if v > 0 {
                return v;
            }
        }
        0
    };

    let pick_bool_either = |key: &str| -> bool {
        text_cfg
            .get(key)
            .and_then(|v| v.as_bool())
            .or_else(|| cfg.get(key).and_then(|v| v.as_bool()))
            .unwrap_or(false)
    };

    let pick_str = |obj: &serde_json::Map<String, serde_json::Value>, key: &str| -> String {
        obj.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };

    let dtype = {
        let d = pick_str(cfg, "torch_dtype");
        if d.is_empty() {
            pick_str(cfg, "dtype")
        } else {
            d
        }
    };

    let architectures: Vec<String> = cfg
        .get("architectures")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let num_attention_heads = pick_u64_either("num_attention_heads");
    let hidden_size = pick_u64_either("hidden_size");
    let head_dim_explicit = pick_u64_either("head_dim");
    let head_dim = if head_dim_explicit > 0 {
        head_dim_explicit
    } else if num_attention_heads > 0 {
        hidden_size / num_attention_heads
    } else {
        0
    };

    // Quantization detection: override -> HF quantization_config -> name heuristic.
    let mut quantization = detect_quantization(model_name, config_json, quantization_override);

    // MLX-community trzyma kwantyzacje jako TOP-LEVEL obiekt `quantization:
    // {bits, group_size}` (NIE `quantization_config`). Gdy obecny, szerokosc wag
    // zalezy od group_size (g64 4-bit = 0.5625, g32 = 0.625), wiec liczymy jawny
    // bytes/param i ustawiamy etykiete. vLLM/GGUF nie uzywaja tego pola, wiec
    // override ich nie dotyczy.
    let mlx_bytes_override = cfg
        .get("quantization")
        .and_then(|v| v.as_object())
        .and_then(|q| {
            let bits = q.get("bits").and_then(|b| b.as_u64())?;
            let group_size = q.get("group_size").and_then(|g| g.as_u64()).unwrap_or(64);
            Some((bits, group_size, mlx_weight_bytes(bits, group_size)))
        });
    let bytes_per_param_override = mlx_bytes_override.map(|(_, _, b)| b);
    if let Some((bits, group_size, _)) = mlx_bytes_override {
        if quantization.is_none() {
            quantization = Some(format!("mlx_{bits}bit_g{group_size}"));
        }
    }

    let has_vision = cfg.contains_key("vision_config")
        || architectures
            .iter()
            .any(|a| a.contains("ConditionalGeneration") || a.contains("Vision"));
    let has_audio = cfg.contains_key("audio_config")
        || cfg
            .get("audio_token_id")
            .map(|v| !v.is_null())
            .unwrap_or(false);

    let kv_heads = pick_u64_either("num_key_value_heads");
    let kv_heads_final = if kv_heads > 0 {
        kv_heads
    } else {
        num_attention_heads
    };

    let num_hidden_layers = pick_u64_either("num_hidden_layers");

    // Sliding-window attention layout. `use_sliding_window: false` (Qwen2-style)
    // disables a declared window. Layer layout comes from `layer_types`
    // ("sliding_attention"/"full_attention") or an integer
    // `sliding_window_pattern` (gemma3: 6 -> every 6th layer is global). A window
    // with no layer info is resolved per architecture below.
    let use_sliding_window = text_cfg
        .get("use_sliding_window")
        .and_then(|v| v.as_bool())
        .or_else(|| cfg.get("use_sliding_window").and_then(|v| v.as_bool()))
        .unwrap_or(true);
    let sliding_window = if use_sliding_window {
        pick_u64_either("sliding_window")
    } else {
        0
    };
    let layer_types: Option<Vec<bool>> = text_cfg
        .get("layer_types")
        .or_else(|| cfg.get("layer_types"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|v| v.as_str() == Some("sliding_attention"))
                .collect()
        });
    let swa_pattern = pick_u64_either("sliding_window_pattern");
    let model_type = pick_str(cfg, "model_type");
    let mut kv_k_elems_global: u64 = 0;
    let mut kv_v_elems_global: u64 = 0;
    let mut kv_k_elems_swa: u64 = 0;
    let mut kv_v_elems_swa: u64 = 0;
    if sliding_window > 0 && num_hidden_layers > 0 {
        // HF configs have uniform per-layer kv heads and head dim.
        let per_layer = kv_heads_final * head_dim;
        for i in 0..num_hidden_layers {
            let is_swa = match (&layer_types, swa_pattern) {
                (Some(t), _) => t.get(i as usize).copied().unwrap_or(true),
                (None, n) if n > 0 => (i + 1) % n != 0,
                // No layer layout declared: gemma2 configs omit it but the
                // architecture alternates SWA/global (even layers sliding);
                // mistral/mixtral are genuinely uniform SWA. For unknown
                // architectures treat every layer as global — overcounting KV
                // only loses headroom, undercounting recommends OOM deploys.
                _ => match model_type.as_str() {
                    "gemma2" => i % 2 == 0,
                    "mistral" | "mixtral" => true,
                    _ => false,
                },
            };
            if is_swa {
                kv_k_elems_swa += per_layer;
                kv_v_elems_swa += per_layer;
            } else {
                kv_k_elems_global += per_layer;
                kv_v_elems_global += per_layer;
            }
        }
    }

    let num_experts = pick_u64_aliases(&["num_experts", "num_local_experts", "n_routed_experts"]);
    let num_experts_per_tok = pick_u64_aliases(&[
        "num_experts_per_tok",
        "num_experts_per_token",
        "n_experts_per_tok",
    ]);
    let moe_intermediate_size = pick_u64_either("moe_intermediate_size");
    let shared_expert_intermediate_size = pick_u64_either("shared_expert_intermediate_size");
    let tie_word_embeddings = pick_bool_either("tie_word_embeddings");

    let mut spec = ModelSpec {
        model_type,
        architectures,
        dtype: if dtype.is_empty() {
            "bfloat16".into()
        } else {
            dtype
        },
        hidden_size,
        num_attention_heads,
        num_key_value_heads: kv_heads_final,
        num_hidden_layers,
        vocab_size: pick_u64_either("vocab_size"),
        head_dim,
        intermediate_size: pick_u64_either("intermediate_size"),
        max_position_embeddings: pick_u64_either("max_position_embeddings"),
        num_experts,
        num_experts_per_tok,
        moe_intermediate_size,
        shared_expert_intermediate_size,
        tie_word_embeddings,
        has_vision,
        has_audio,
        // Dokladny rozmiar wag dostarcza safetensors index (handler ustawia
        // weights_bytes_override); tu zostawiamy 0, by estimated_params() byl
        // fallbackiem. Dla MoE publikujemy aktywne parametry, zeby UI pokazalo
        // realny rozmiar aktywnej sciezki zamiast pelnej sumy ekspertow.
        num_parameters: 0,
        num_active_parameters: 0,
        quantization,
        bytes_per_param_override,
        sliding_window,
        kv_k_elems_global,
        kv_v_elems_global,
        kv_k_elems_swa,
        kv_v_elems_swa,
    };
    if spec.num_experts > 0 {
        spec.num_active_parameters = spec.active_params();
    }
    Ok(spec)
}

/// Buduje string `--key val --key val ...` do wpisania w VLLM_ARGS env.
/// Zalacza tylko parametry rozne od vllm defaults zeby nie zasmiecac.
/// Wspoldzielone miedzy api_deploy_recommend (endpoint dla GUI) i runner.rs
/// (auto-defaults dla bundle native gdy user nie ustawil Advanced).
pub fn build_vllm_args_string(spec: &ModelSpec, input: &VramEstimateInput) -> String {
    let mut parts: Vec<String> = Vec::new();

    // Wiele nowoczesnych repo (Gemma 4, DeepSeek V4, modele z custom kodem
    // modelujacym) wymaga `--trust-remote-code`, inaczej vLLM nie zaladuje
    // architektury. Wlaczone domyslnie; GUI ma toggle ktory je zdejmuje dla
    // nieufnego repo (default ON).
    parts.push("--trust-remote-code".into());

    parts.push("--dtype".into());
    parts.push("auto".into());
    parts.push("--gpu-memory-utilization".into());
    parts.push(format!("{:.2}", input.gpu_memory_utilization));
    parts.push("--max-model-len".into());
    parts.push(input.max_model_len.to_string());
    parts.push("--max-num-seqs".into());
    parts.push(input.max_num_seqs.to_string());
    parts.push("--max-num-batched-tokens".into());
    // CAP, nie max: rownanie batched-tokens = max_model_len (np. 262144 dla
    // Qwen3.5) sprawia, ze profiling vLLM robi forward na pelnym kontekscie ->
    // ~22GB aktywacji -> CUDA OOM nawet dla malego modelu. Chunked prefill
    // (wlaczony nizej) obsluguje dlugi kontekst w kawalkach <= tej wartosci.
    parts.push(input.max_model_len.min(8192).to_string());

    // chunked prefill TYLKO dla nie-multimodal: vllm dla VL modeli (Gemma 4,
    // Qwen 2.5 VL itp.) Forcuje --disable_chunked_mm_input wewnetrznie i
    // chunked-prefill staje sie no-op. Brak flagi nie szkodzi text-only.
    if !spec.has_vision && !spec.has_audio {
        parts.push("--enable-chunked-prefill".into());
    }

    // prefix-caching: dramatyczne wygrana dla powtarzalnych promptow
    // (system prompts, RAG context). Bezpieczne dla wszystkich kategorii
    // modeli — vllm sam wylacza w przypadkach gdzie nie ma sensu.
    parts.push("--enable-prefix-caching".into());

    // FlashInfer autotune — wybor najlepszych CUDA kerneli per (shape,
    // dtype, arch) przy pierwszym starcie + cache w VLLM_CACHE_ROOT.
    // Aktywny tylko gdy backend == FlashInfer (vllm sam wybiera lub
    // VLLM_ATTENTION_BACKEND=FLASHINFER); na innych backendach no-op.
    parts.push("--enable-flashinfer-autotune".into());

    if input.tensor_parallel > 1 {
        parts.push("--tensor-parallel-size".into());
        parts.push(input.tensor_parallel.to_string());
        // MoE na wielu kartach: expert-parallel kladzie CALE eksperty na roznych
        // GPU (token routing all-to-all) zamiast TP-tnac kazdego eksperta na
        // wszystkie karty (redundantny all-reduce co warstwe). vLLM dzieli
        // eksperty przez EP W OBREBIE grupy TP — nie zmienia rachunku GPU
        // (calkowite = TP), wiec bezpieczne domyslnie dla MoE. Pelny DP/EP
        // restructuring (--data-parallel-size, TP=1) jest model/cluster-specyficzny
        // i zostaje w recepturach recipes.vllm.ai / jawnej konfiguracji usera.
        if spec.num_experts > 0 {
            parts.push("--enable-expert-parallel".into());
        }
    }
    if input.pipeline_parallel > 1 {
        parts.push("--pipeline-parallel-size".into());
        parts.push(input.pipeline_parallel.to_string());
    }
    if input.kv_cache_dtype != "auto" {
        parts.push("--kv-cache-dtype".into());
        parts.push(input.kv_cache_dtype.clone());
    }

    if let Some(q) = &spec.quantization {
        let q_norm = q.to_lowercase().replace('-', "_");
        match q_norm.as_str() {
            "awq" => {
                parts.push("--quantization".into());
                parts.push("awq".into());
            }
            "gptq" => {
                parts.push("--quantization".into());
                parts.push("gptq".into());
            }
            "fp8" | "modelopt_fp8" => {
                parts.push("--quantization".into());
                parts.push("fp8".into());
            }
            "int4" | "int4_autoround" | "auto_round" => {
                parts.push("--quantization".into());
                parts.push("auto_round".into());
            }
            // Label nvfp4/fp4/mxfp4 jest dwuznaczny: repo moze byc spakowane jako
            // NVIDIA ModelOpt (quant_method=modelopt) ALBO jako compressed-tensors
            // (llm-compressor, quant_method=compressed-tensors). Wymuszenie
            // --quantization modelopt_fp4 odrzuca repo compressed-tensors (vLLM
            // czyta prawdziwa metode z config.json i porownuje). Nie emitujemy
            // flagi — vLLM auto-wykrywa kwantyzacje z quantization_config i obsluguje
            // oba warianty FP4 poprawnie.
            "nvfp4" | "fp4" | "mxfp4" => {}
            "compressed_tensors_4bit" | "w4a16" | "w8a8" | "w8a16" => {
                parts.push("--quantization".into());
                parts.push("compressed-tensors".into());
            }
            "bnb_4bit" | "bnb_8bit" | "bitsandbytes_4bit" | "bitsandbytes_8bit" => {
                parts.push("--quantization".into());
                parts.push("bitsandbytes".into());
            }
            _ => {}
        }
    }

    parts.join(" ")
}

/// Buduje argumenty CLI llama.cpp (`llama-server`) z dopasowanej konfiguracji.
/// Split-mode mapuje TP/PP na fizyczny rozklad jednego procesu na karty.
/// Karty zakladamy rowne, wiec `--tensor-split` pomijamy.
pub fn build_llamacpp_args_string(_spec: &ModelSpec, input: &VramEstimateInput) -> String {
    let mut parts: Vec<String> = Vec::new();

    // `max_model_len` to kontekst per-request; `-c` to CALY kontekst dzielony na
    // `-np` slotow, wiec emitujemy max_model_len × max_num_seqs.
    let seqs = input.max_num_seqs.max(1);
    parts.push("-c".into());
    parts.push((input.max_model_len * seqs).to_string());
    parts.push("-ngl".into());
    parts.push("999".into());
    // Fizyczny ubatch (`-ub`, default 512) steruje compute bufferem; logiczny `-b`
    // niepotrzebnie obniza przepustowosc prefilla.
    parts.push("-ub".into());
    parts.push("512".into());

    if seqs > 1 {
        parts.push("-np".into());
        parts.push(seqs.to_string());
    }

    let tp = input.tensor_parallel.max(1);
    let pp = input.pipeline_parallel.max(1);
    let split_mode = match (tp > 1, pp > 1) {
        // llama.cpp nie laczy row+layer; przy obu>1 padamy na row (warning w estimate).
        (true, true) => "row",
        (true, false) => "row",
        (false, true) => "layer",
        (false, false) => "none",
    };
    parts.push("--split-mode".into());
    parts.push(split_mode.into());

    // KV cache: osobne typy K i V. Etykiete mapujemy na token CLI llama.cpp
    // (fp16->f16, bfloat16->bf16, fp8->q8_0, q*_0 doslownie). Domyslne f16/bf16/auto
    // (2.0 B/elem) NIE emituja flagi (server uzywa f16 z defaultu). Kwantyzowane V
    // wymaga flash-attention, wiec `-fa` dodajemy gdy K lub V jest kwantyzowane.
    let k_cli = llamacpp_cache_type_cli(input.k_label());
    let v_cli = llamacpp_cache_type_cli(input.v_label());
    if let Some(k) = &k_cli {
        parts.push("--cache-type-k".into());
        parts.push(k.clone());
    }
    if let Some(v) = &v_cli {
        parts.push("--cache-type-v".into());
        parts.push(v.clone());
    }
    if k_cli.is_some() || v_cli.is_some() {
        parts.push("-fa".into());
    }

    parts.join(" ")
}

/// Mapuje etykiete KV cache (UI/config) na token CLI `--cache-type-k/v` llama.cpp.
/// Zwraca `None` dla typow domyslnych (f16/bf16/auto = 2.0 B), ktorych flaga nie
/// trzeba emitowac (server uzywa f16). Kwantyzowane typy zwracaja swoj token; dla
/// fp8 (vLLM) mapujemy na q8_0 (najblizszy 8-bitowy KV llama.cpp).
fn llamacpp_cache_type_cli(label: &str) -> Option<String> {
    let l = label.to_lowercase().replace('-', "_");
    match l.as_str() {
        "auto" | "f16" | "fp16" | "bf16" | "bfloat16" => None,
        "fp8" | "fp8_e5m2" | "fp8_e4m3" => Some("q8_0".into()),
        "q8_0" | "q5_1" | "q5_0" | "q4_1" | "q4_0" | "iq4_nl" => Some(l),
        _ => None,
    }
}

/// Pobierz HF config.json przez HTTP. Wymaga internet + ewentualnie HF token
/// dla gated repo (przekazany jako Bearer).
pub async fn fetch_hf_config(
    client: &reqwest::Client,
    model_name: &str,
    hf_token: Option<&str>,
) -> Result<serde_json::Value> {
    let url = format!(
        "https://huggingface.co/{}/resolve/main/config.json",
        model_name
    );
    let mut req = client.get(&url);
    if let Some(t) = hf_token {
        if !t.is_empty() {
            req = req.bearer_auth(t);
        }
    }
    let resp = req
        .send()
        .await
        .with_context(|| format!("HF GET {}", url))?;
    if !resp.status().is_success() {
        return Err(anyhow!(
            "HF config fetch failed status={} dla {}",
            resp.status(),
            model_name
        ));
    }
    let json: serde_json::Value = resp.json().await.context("HF config JSON parse")?;
    Ok(json)
}

/// Wyciaga `metadata.total_size` z `model.safetensors.index.json`. To suma
/// bajtow WSZYSTKICH tensorow modelu (= realny footprint wag dla zapisanego
/// dtype/quant), wiec uzyta jako `weights_bytes_override` daje dokladny rozmiar
/// wag bez heurystyki param-count. Zwraca None gdy pola brak lub zero.
pub fn parse_safetensors_total_size(index_json: &serde_json::Value) -> Option<u64> {
    let n = index_json
        .get("metadata")?
        .get("total_size")?
        .as_u64()
        .filter(|&n| n > 0)?;
    Some(n)
}

/// Dokladny rozmiar wag (w bajtach) dla repo safetensors. Najpierw probuje
/// `model.safetensors.index.json` (modele wieloplikowe -> `metadata.total_size`);
/// gdy index nie istnieje (model jednoplikowy), robi HEAD na `model.safetensors`
/// i czyta Content-Length. Zwraca None przy dowolnym bledzie - caller fallbackuje
/// do heurystyki `estimated_params`. Bearer token dla gated repo.
pub async fn fetch_safetensors_total_size(
    client: &reqwest::Client,
    model_name: &str,
    hf_token: Option<&str>,
) -> Option<u64> {
    let index_url = format!(
        "https://huggingface.co/{}/resolve/main/model.safetensors.index.json",
        model_name
    );
    let mut req = client.get(&index_url);
    if let Some(t) = hf_token {
        if !t.is_empty() {
            req = req.bearer_auth(t);
        }
    }
    if let Ok(resp) = req.send().await {
        if resp.status().is_success() {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if let Some(total) = parse_safetensors_total_size(&json) {
                    return Some(total);
                }
            }
        }
    }

    // Model jednoplikowy: brak index.json -> rozmiar pliku model.safetensors.
    let single_url = format!(
        "https://huggingface.co/{}/resolve/main/model.safetensors",
        model_name
    );
    match fetch_gguf_size(client, &single_url, hf_token).await {
        Ok(n) if n > 0 => Some(n),
        _ => None,
    }
}

// =============================================================================
// Parser naglowka GGUF (v2/v3, little-endian).
//
// Repozytoria GGUF nie maja config.json - parametry architektury (block_count,
// embedding_length, head_count, ...) siedza w naglowku metadanych pliku .gguf.
// Czytamy tylko poczatek pliku (range request) i parsujemy bloki metadanych KV.
// Sam plik .gguf JEST dokladnym skwantyzowanym footprintem wag, wiec jego rozmiar
// (Content-Length) zwracamy osobno jako weights_bytes.
// =============================================================================

/// Wartosc metadanych GGUF. Trzymamy tylko typy ktorych faktycznie uzywamy
/// (liczby + string + count tablicy); reszta jest poprawnie pomijana przez kursor.
#[derive(Debug, Clone)]
enum GgufValue {
    U64(u64),
    String(String),
    /// Tablica - przechowujemy tylko liczbe elementow (potrzebna dla vocab_size
    /// liczonego z dlugosci `tokenizer.ggml.tokens`).
    ArrayLen(u64),
    /// Small numeric array (int/bool, len <= 4096) materialized in full — needed
    /// for per-layer metadata like `attention.head_count_kv` and the SWA layer
    /// pattern. Tokenizer-sized arrays keep the ArrayLen/ArrayTruncated path.
    U64Array(Vec<u64>),
    /// Tablica, ktorej elementy urwaly sie na granicy bufora. Count jest znany
    /// (stoi przed elementami), wiec vocab_size dalej da sie odczytac; sygnalizuje
    /// callerowi ze parsowanie kolejnych KV nie ma sensu (early-stop).
    ArrayTruncated(u64),
    /// Wartosc istnieje ale nie mapujemy jej na nic uzytecznego.
    Other,
}

/// Sekwencyjny czytnik bajtow naglowka GGUF. Trzyma kursor i sygnalizuje
/// brak bajtow bledem (caller moze wtedy dociagnac wiekszy zakres).
struct GgufReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> GgufReader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| anyhow!("GGUF: przepelnienie kursora"))?;
        if end > self.buf.len() {
            return Err(anyhow!(
                "GGUF: za malo bajtow (potrzeba {n} przy offset {}, mam {})",
                self.pos,
                self.buf.len()
            ));
        }
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// String GGUF: u64 length + tyle bajtow UTF-8 (lossy gdy niepoprawne).
    fn string(&mut self) -> Result<String> {
        let len = self.u64()? as usize;
        let bytes = self.take(len)?;
        Ok(String::from_utf8_lossy(bytes).into_owned())
    }

    /// Pomija pojedyncza wartosc skalarną danego typu (bez stringa i tablicy).
    /// Zwraca rozmiar w bajtach dla typow stalej dlugosci.
    fn scalar_size(value_type: u32) -> Result<usize> {
        Ok(match value_type {
            0 | 1 | 7 => 1,    // u8 / i8 / bool
            2 | 3 => 2,        // u16 / i16
            4 | 5 | 6 => 4,    // u32 / i32 / f32
            10 | 11 | 12 => 8, // u64 / i64 / f64
            other => {
                return Err(anyhow!(
                    "GGUF: nieobslugiwany typ skalarny {other} w tablicy"
                ))
            }
        })
    }

    /// Reads one integer/bool scalar of the given GGUF type widened to u64
    /// (signed values go through sign-extension, bools become 0/1).
    fn scalar_u64(&mut self, value_type: u32) -> Result<u64> {
        Ok(match value_type {
            0 | 7 => self.u8()? as u64,
            1 => self.u8()? as i8 as i64 as u64,
            2 => self.u16()? as u64,
            3 => self.u16()? as i16 as i64 as u64,
            4 => self.u32()? as u64,
            5 => self.u32()? as i32 as i64 as u64,
            10 => self.u64()?,
            11 => self.u64()? as i64 as u64,
            other => return Err(anyhow!("GGUF: typ {other} nie jest skalarem calkowitym")),
        })
    }

    /// Czyta wartosc dla podanego value_type. MUSI poprawnie przejsc przez KAZDY
    /// typ (takze tablice i typy ktorych nie uzywamy) inaczej kursor sie rozjedzie.
    fn read_value(&mut self, value_type: u32) -> Result<GgufValue> {
        match value_type {
            0 => Ok(GgufValue::U64(self.u8()? as u64)),
            1 => Ok(GgufValue::U64(self.u8()? as i8 as i64 as u64)),
            2 => Ok(GgufValue::U64(self.u16()? as u64)),
            3 => Ok(GgufValue::U64(self.u16()? as i16 as i64 as u64)),
            4 => Ok(GgufValue::U64(self.u32()? as u64)),
            5 => Ok(GgufValue::U64(self.u32()? as i32 as i64 as u64)),
            6 => {
                let _ = self.u32()?; // f32 - nie uzywamy
                Ok(GgufValue::Other)
            }
            7 => Ok(GgufValue::U64(self.u8()? as u64)), // bool
            8 => Ok(GgufValue::String(self.string()?)),
            9 => {
                // Tablica: elem_type (u32) + count (u64) + count elementow.
                // Count odczytujemy ZAWSZE (stoi przed elementami) - dzieki temu
                // vocab_size z `tokenizer.ggml.tokens` znamy nie pobierajac stringow.
                // Skip elementow moze sie urwac na granicy bufora; zwracamy wtedy
                // ArrayTruncated z juz znanym count, a caller decyduje czy
                // wszystkie wymagane pola juz zebral (early-stop).
                let elem_type = self.u32()?;
                let count = self.u64()?;
                if elem_type == 9 {
                    return Err(anyhow!("GGUF: zagniezdzona tablica nieobslugiwana"));
                }
                // Small integer/bool arrays are materialized — per-layer kv-head
                // counts and SWA patterns live here. Anything bigger (tokenizer
                // tables) is skipped as before.
                let is_int = matches!(elem_type, 0 | 1 | 2 | 3 | 4 | 5 | 7 | 10 | 11);
                if is_int && count <= 4096 {
                    let mut vals: Vec<u64> = Vec::with_capacity(count as usize);
                    for _ in 0..count {
                        match self.scalar_u64(elem_type) {
                            Ok(v) => vals.push(v),
                            Err(_) => return Ok(GgufValue::ArrayTruncated(count)),
                        }
                    }
                    return Ok(GgufValue::U64Array(vals));
                }
                for _ in 0..count {
                    if elem_type == 8 {
                        // String o zmiennej dlugosci - musimy przeczytac kazdy.
                        if self.string().is_err() {
                            return Ok(GgufValue::ArrayTruncated(count));
                        }
                    } else {
                        let sz = Self::scalar_size(elem_type)?;
                        if self.take(sz).is_err() {
                            return Ok(GgufValue::ArrayTruncated(count));
                        }
                    }
                }
                Ok(GgufValue::ArrayLen(count))
            }
            10 => Ok(GgufValue::U64(self.u64()?)),
            11 => Ok(GgufValue::U64(self.u64()? as i64 as u64)),
            12 => {
                let _ = self.u64()?; // f64 - nie uzywamy
                Ok(GgufValue::Other)
            }
            other => Err(anyhow!("GGUF: nieznany value_type {other}")),
        }
    }
}

/// Parsuje naglowek GGUF z bufora i buduje `ModelSpec`. Quantization wyprowadzana
/// jednoznacznie z nazwy pliku (Q4_K_M itd.) - file_type w naglowku bywa
/// niejednoznaczny przy mixed-quant. Zwraca blad gdy magic/wersja niepoprawne
/// albo bufor jest za krotki (caller dociaga wiekszy zakres).
pub fn parse_gguf_header(buf: &[u8], gguf_file: &str) -> Result<ModelSpec> {
    let mut r = GgufReader::new(buf);

    let magic = r.take(4)?;
    if magic != b"GGUF" {
        return Err(anyhow!(
            "GGUF: zly magic {:02x?} (oczekiwano 'GGUF')",
            magic
        ));
    }
    let version = r.u32()?;
    if version < 2 {
        return Err(anyhow!(
            "GGUF: wersja {version} nieobslugiwana (wymagane v2/v3)"
        ));
    }
    // GGUFv2/v3 uzywaja u64 dla obu countow.
    let _tensor_count = r.u64()?;
    let kv_count = r.u64()?;

    let get_u64 = |kv: &std::collections::HashMap<String, GgufValue>, key: &str| -> Option<u64> {
        match kv.get(key) {
            Some(GgufValue::U64(v)) => Some(*v),
            _ => None,
        }
    };
    let get_str =
        |kv: &std::collections::HashMap<String, GgufValue>, key: &str| -> Option<String> {
            match kv.get(key) {
                Some(GgufValue::String(s)) => Some(s.clone()),
                _ => None,
            }
        };
    let get_arr_len =
        |kv: &std::collections::HashMap<String, GgufValue>, key: &str| -> Option<u64> {
            match kv.get(key) {
                Some(GgufValue::ArrayLen(n)) | Some(GgufValue::ArrayTruncated(n)) => Some(*n),
                Some(GgufValue::U64Array(a)) => Some(a.len() as u64),
                _ => None,
            }
        };
    let get_u64_array =
        |kv: &std::collections::HashMap<String, GgufValue>, key: &str| -> Option<Vec<u64>> {
            match kv.get(key) {
                Some(GgufValue::U64Array(a)) => Some(a.clone()),
                _ => None,
            }
        };

    // Komplet wymaganych pol: architektura + wszystkie wymiary `{arch}.*` + vocab
    // (jawny klucz albo dlugosc tablicy tokenizera). Gdy zebrane, wolno przerwac
    // parsowanie - tablice tokenizera 128k/150k vocab sa za duze by je dociagac.
    let has_all_required = |kv: &std::collections::HashMap<String, GgufValue>| -> bool {
        let Some(arch) = get_str(kv, "general.architecture") else {
            return false;
        };
        let key = |suffix: &str| format!("{arch}.{suffix}");
        get_u64(kv, &key("block_count")).is_some()
            && get_u64(kv, &key("embedding_length")).is_some()
            && get_u64(kv, &key("attention.head_count")).is_some()
            && get_u64(kv, &key("feed_forward_length")).is_some()
            && get_u64(kv, &key("context_length")).is_some()
            && (get_u64(kv, &key("vocab_size")).is_some()
                || get_arr_len(kv, "tokenizer.ggml.tokens").is_some())
    };

    let mut kv: std::collections::HashMap<String, GgufValue> = std::collections::HashMap::new();
    for _ in 0..kv_count {
        // Gdy bufor sie urywa na granicy kolejnego KV, ale komplet wymaganych pol
        // juz mamy - to early-stop (sukces), nie blad. W przeciwnym razie blad
        // propaguje sie i caller dociaga wiekszy zakres.
        let key = match r.string() {
            Ok(k) => k,
            Err(e) if has_all_required(&kv) => {
                let _ = e;
                break;
            }
            Err(e) => return Err(e),
        };
        let value_type = match r.u32() {
            Ok(t) => t,
            Err(e) if has_all_required(&kv) => {
                let _ = e;
                break;
            }
            Err(e) => return Err(e),
        };
        let value = match r.read_value(value_type) {
            Ok(v) => v,
            Err(e) if has_all_required(&kv) => {
                let _ = e;
                break;
            }
            Err(e) => return Err(e),
        };
        let truncated = matches!(value, GgufValue::ArrayTruncated(_));
        kv.insert(key, value);
        // Urwana tablica oznacza koniec uzytecznego bufora; jesli komplet pol mamy,
        // konczymy sukcesem, inaczej zglaszamy brak bajtow do dociagniecia.
        if truncated {
            if has_all_required(&kv) {
                break;
            }
            return Err(anyhow!(
                "GGUF: tablica metadanych urwana przed zebraniem pol architektury"
            ));
        }
    }

    let arch = get_str(&kv, "general.architecture")
        .ok_or_else(|| anyhow!("GGUF: brak general.architecture"))?;

    let key = |suffix: &str| format!("{arch}.{suffix}");

    let num_hidden_layers = get_u64(&kv, &key("block_count")).unwrap_or(0);
    let hidden_size = get_u64(&kv, &key("embedding_length")).unwrap_or(0);
    let num_attention_heads = get_u64(&kv, &key("attention.head_count")).unwrap_or(0);
    // head_count_kv is a scalar OR a per-layer array (e.g. gemma3/gemma4 mix SWA
    // and global layers with different kv-head counts). The scalar spec field
    // carries the max (display / TP heuristics); the per-layer values feed the
    // SWA-aware KV aggregates below.
    let kv_heads_arr = get_u64_array(&kv, &key("attention.head_count_kv"));
    let num_key_value_heads = match (&kv_heads_arr, get_u64(&kv, &key("attention.head_count_kv"))) {
        (Some(a), _) => a.iter().copied().max().unwrap_or(num_attention_heads),
        (None, Some(v)) => v,
        (None, None) => num_attention_heads,
    };
    let intermediate_size = get_u64(&kv, &key("feed_forward_length")).unwrap_or(0);
    let max_position_embeddings = get_u64(&kv, &key("context_length")).unwrap_or(0);

    // Per-layer K/V head widths. Fallbacks: _swa -> non-swa, value -> key,
    // key -> hidden/heads.
    let k_len = get_u64(&kv, &key("attention.key_length")).unwrap_or_else(|| {
        if num_attention_heads > 0 {
            hidden_size / num_attention_heads
        } else {
            0
        }
    });
    let v_len = get_u64(&kv, &key("attention.value_length")).unwrap_or(k_len);
    let k_len_swa = get_u64(&kv, &key("attention.key_length_swa")).unwrap_or(k_len);
    let v_len_swa = get_u64(&kv, &key("attention.value_length_swa")).unwrap_or(v_len);
    let head_dim = k_len;

    let sliding_window = get_u64(&kv, &key("attention.sliding_window")).unwrap_or(0);
    // SWA layer layout: per-layer 0/1 array (1 = SWA), or a scalar N meaning
    // "every Nth layer is global, the rest SWA" (gemma3 convention). A declared
    // window without any pattern means uniform SWA (mistral-style), except
    // gemma2: its GGUF files carry no pattern and llama.cpp hardcodes the
    // even-SWA/odd-global alternation.
    let swa_pattern_arr = get_u64_array(&kv, &key("attention.sliding_window_pattern"));
    let swa_pattern_scalar = get_u64(&kv, &key("attention.sliding_window_pattern")).unwrap_or(0);
    let mut kv_k_elems_global: u64 = 0;
    let mut kv_v_elems_global: u64 = 0;
    let mut kv_k_elems_swa: u64 = 0;
    let mut kv_v_elems_swa: u64 = 0;
    for i in 0..num_hidden_layers {
        let kvh = kv_heads_arr
            .as_ref()
            .and_then(|a| a.get(i as usize).copied())
            .unwrap_or(num_key_value_heads);
        let is_swa = if sliding_window == 0 {
            false
        } else if let Some(p) = &swa_pattern_arr {
            p.get(i as usize).copied().unwrap_or(1) != 0
        } else if swa_pattern_scalar > 0 {
            (i + 1) % swa_pattern_scalar != 0
        } else if arch == "gemma2" {
            i % 2 == 0
        } else {
            true
        };
        if is_swa {
            kv_k_elems_swa += kvh * k_len_swa;
            kv_v_elems_swa += kvh * v_len_swa;
        } else {
            kv_k_elems_global += kvh * k_len;
            kv_v_elems_global += kvh * v_len;
        }
    }

    // vocab_size: jawny klucz, inaczej dlugosc tablicy tokenizer.ggml.tokens.
    let vocab_size = get_u64(&kv, &key("vocab_size"))
        .or_else(|| get_arr_len(&kv, "tokenizer.ggml.tokens"))
        .unwrap_or(0);

    // general.size_label ("31B", "780M", "3.8B") carries the official parameter
    // count — far more accurate than the dimensional heuristic.
    let num_parameters = get_str(&kv, "general.size_label")
        .and_then(|s| parse_size_label(&s))
        .unwrap_or(0);

    // Quantization z nazwy pliku (jednoznaczna). Fallback: bf16 gdy nazwa milczy.
    let quant_label = detect_quant_from_name(gguf_file);
    let dtype = quant_label
        .clone()
        .unwrap_or_else(|| "bfloat16".to_string());

    Ok(ModelSpec {
        model_type: arch,
        architectures: Vec::new(),
        dtype,
        hidden_size,
        num_attention_heads,
        num_key_value_heads,
        num_hidden_layers,
        vocab_size,
        head_dim,
        intermediate_size,
        max_position_embeddings,
        // GGUF deployuje sie wylacznie na llama.cpp, gdzie rozmiar pliku .gguf
        // jest dokladnym footprintem wag (weights_bytes_override), wiec MoE-aware
        // param-count nie jest tu uzywany — zostawiamy pola w stanie dense.
        num_experts: 0,
        num_experts_per_tok: 0,
        moe_intermediate_size: 0,
        shared_expert_intermediate_size: 0,
        tie_word_embeddings: false,
        has_vision: false,
        has_audio: false,
        num_parameters,
        num_active_parameters: 0,
        quantization: quant_label,
        bytes_per_param_override: None,
        sliding_window,
        kv_k_elems_global,
        kv_v_elems_global,
        kv_k_elems_swa,
        kv_v_elems_swa,
    })
}

/// Parses a `general.size_label` value ("31B", "780M", "3.8B") into a parameter
/// count. MoE labels like "8x7B" are left to the caller (returns None).
fn parse_size_label(label: &str) -> Option<u64> {
    let t = label.trim().to_uppercase();
    let (num, mult) = if let Some(p) = t.strip_suffix('B') {
        (p, 1e9)
    } else if let Some(p) = t.strip_suffix('M') {
        (p, 1e6)
    } else {
        return None;
    };
    let n: f64 = num.trim().parse().ok()?;
    if n <= 0.0 {
        return None;
    }
    Some((n * mult) as u64)
}

/// Pobiera rozmiar pliku (Content-Length) przez HEAD. HF zwraca 302 do CDN,
/// reqwest podaza za redirectem i Content-Length przychodzi z finalnej odpowiedzi.
/// Gdy HEAD nie da dlugosci, robi GET z Range: bytes=0-0 i czyta Content-Range.
async fn fetch_gguf_size(
    client: &reqwest::Client,
    url: &str,
    hf_token: Option<&str>,
) -> Result<u64> {
    let mut head = client.head(url);
    if let Some(t) = hf_token {
        if !t.is_empty() {
            head = head.bearer_auth(t);
        }
    }
    let resp = head.send().await.with_context(|| format!("HEAD {url}"))?;
    if resp.status().is_success() {
        if let Some(len) = resp.content_length() {
            if len > 0 {
                return Ok(len);
            }
        }
    }

    // Fallback: Range bytes=0-0 -> naglowek Content-Range: "bytes 0-0/TOTAL".
    let mut probe = client.get(url).header("Range", "bytes=0-0");
    if let Some(t) = hf_token {
        if !t.is_empty() {
            probe = probe.bearer_auth(t);
        }
    }
    let resp = probe
        .send()
        .await
        .with_context(|| format!("GET range {url}"))?;
    if let Some(cr) = resp.headers().get("content-range") {
        let cr = cr.to_str().unwrap_or("");
        if let Some((_, total)) = cr.rsplit_once('/') {
            if let Ok(n) = total.trim().parse::<u64>() {
                if n > 0 {
                    return Ok(n);
                }
            }
        }
    }
    Err(anyhow!(
        "Nie udalo sie ustalic rozmiaru pliku GGUF (brak Content-Length / Content-Range): {url}"
    ))
}

/// Pobiera zakres bajtow z poczatku pliku (Range request) dla naglowka GGUF.
async fn fetch_gguf_range(
    client: &reqwest::Client,
    url: &str,
    end_inclusive: u64,
    hf_token: Option<&str>,
) -> Result<Vec<u8>> {
    let mut req = client
        .get(url)
        .header("Range", format!("bytes=0-{end_inclusive}"));
    if let Some(t) = hf_token {
        if !t.is_empty() {
            req = req.bearer_auth(t);
        }
    }
    let resp = req
        .send()
        .await
        .with_context(|| format!("GET range header {url}"))?;
    if !resp.status().is_success() && resp.status().as_u16() != 206 {
        return Err(anyhow!(
            "GGUF range fetch status={} dla {url}",
            resp.status()
        ));
    }
    let bytes = resp.bytes().await.context("GGUF range body read")?;
    Ok(bytes.to_vec())
}

/// Czyta metadane modelu GGUF z repo HF. Zwraca `ModelSpec` (architektura z
/// naglowka) oraz rozmiar pliku w bajtach (dokladny footprint wag dla VRAM).
///
/// 1. HEAD/Range -> rozmiar pliku.
/// 2. Range-fetch poczatku pliku (1 MiB; gdy pola architektury leza dalej niz
///    1 MiB - dociaga do 8 MiB) i parsuje naglowek.
///
/// Parser ma early-stop: gdy zbierze komplet pol `{arch}.*` + vocab, konczy
/// sukcesem nawet jesli bufor urywa sie w srodku duzej tablicy tokenizera
/// (modele 128k/150k vocab - Qwen, Llama3). Dlatego nie potrzebujemy dociagac
/// calej sekcji tokenizera (czesto >8 MiB) - 8 MiB wystarcza na same wymiary.
pub async fn fetch_gguf_spec(
    client: &reqwest::Client,
    repo: &str,
    gguf_file: &str,
    hf_token: Option<&str>,
) -> Result<(ModelSpec, u64)> {
    let url = format!("https://huggingface.co/{repo}/resolve/main/{gguf_file}");

    let file_size = fetch_gguf_size(client, &url, hf_token).await?;

    // Probuj 1 MiB; przy braku bajtow na tablice tokenizera dociagnij 8 MiB.
    const CHUNK_1MIB: u64 = 1024 * 1024 - 1;
    const CHUNK_8MIB: u64 = 8 * 1024 * 1024 - 1;
    let first = fetch_gguf_range(
        client,
        &url,
        CHUNK_1MIB.min(file_size.saturating_sub(1)),
        hf_token,
    )
    .await?;
    let spec = match parse_gguf_header(&first, gguf_file) {
        Ok(spec) => spec,
        Err(_) if file_size > CHUNK_1MIB + 1 => {
            let bigger = fetch_gguf_range(
                client,
                &url,
                CHUNK_8MIB.min(file_size.saturating_sub(1)),
                hf_token,
            )
            .await?;
            parse_gguf_header(&bigger, gguf_file)?
        }
        Err(e) => return Err(e),
    };

    Ok((spec, file_size))
}

#[inline]
fn bytes_to_gib(bytes: f64) -> f64 {
    bytes / (1024.0 * 1024.0 * 1024.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn qwen_05b() -> ModelSpec {
        ModelSpec {
            model_type: "qwen2".into(),
            architectures: vec!["Qwen2ForCausalLM".into()],
            dtype: "bfloat16".into(),
            hidden_size: 896,
            num_attention_heads: 14,
            num_key_value_heads: 2,
            num_hidden_layers: 24,
            vocab_size: 151936,
            head_dim: 64,
            intermediate_size: 4864,
            max_position_embeddings: 32768,
            ..Default::default()
        }
    }

    /// Real google/gemma-4-31b GGUF header: 60 layers, 50 SWA (16 kv heads,
    /// 256-dim K/V) + 10 global (4 kv heads, 512-dim K/V), window 1024.
    fn gemma4_31b() -> ModelSpec {
        ModelSpec {
            model_type: "gemma4".into(),
            architectures: vec!["Gemma4ForConditionalGeneration".into()],
            dtype: "bfloat16".into(),
            hidden_size: 5376,
            num_attention_heads: 32,
            num_key_value_heads: 16,
            num_hidden_layers: 60,
            vocab_size: 262144,
            head_dim: 256,
            intermediate_size: 21504,
            max_position_embeddings: 262144,
            has_vision: true,
            num_parameters: 31_000_000_000,
            sliding_window: 1024,
            // 10 global layers × 4 kv heads × 512 = 20480 elems per token.
            kv_k_elems_global: 20480,
            kv_v_elems_global: 20480,
            // 50 SWA layers × 16 kv heads × 256 = 204800 elems per token.
            kv_k_elems_swa: 204800,
            kv_v_elems_swa: 204800,
            ..Default::default()
        }
    }

    fn qwen36_27b_q4() -> ModelSpec {
        ModelSpec {
            model_type: "qwen3moe".into(),
            architectures: vec!["Qwen3MoeForCausalLM".into()],
            dtype: "bfloat16".into(),
            hidden_size: 5120,
            num_attention_heads: 40,
            num_key_value_heads: 8,
            num_hidden_layers: 64,
            vocab_size: 151936,
            head_dim: 128,
            intermediate_size: 25600,
            max_position_embeddings: 262144,
            quantization: Some("int4".into()),
            ..Default::default()
        }
    }

    #[test]
    fn llamacpp_qwen36_27b_q4_total_is_realistic_not_vllm_inflated() {
        // Regresja: vLLM model raportowal 41.6 GB "aktywacji" (5 GB × 8 GPU) i ~57 GB
        // total dla deployu llama.cpp. Fizyka llama.cpp (jeden proces, compute buffer
        // setki MB, KV dla calego -c) musi dac total ~18-21 GB i activations ~3-4 GB.
        let m = qwen36_27b_q4();
        let input = VramEstimateInput {
            engine: DeployEngine::LlamaCpp,
            gpu_count: 8,
            gpu_memory_gb_each: 8.0,
            tensor_parallel: 8,
            pipeline_parallel: 1,
            max_model_len: 1024,
            max_num_seqs: 1,
            max_num_batched_tokens: 8192,
            kv_cache_dtype: "auto".into(),
            kv_cache_dtype_v: None,
            gpu_memory_utilization: 0.9,
            activation_overhead_pct: 10.0,
            // 15.9 GB Q4 footprint pliku .gguf.
            weights_bytes_override: Some((15.9 * 1024.0 * 1024.0 * 1024.0) as u64),
        };
        let est = estimate_llamacpp_vram(&m, &input);
        assert!(
            (est.model_weights_gb - 15.9).abs() < 0.1,
            "wagi GGUF override: {}",
            est.model_weights_gb
        );
        assert!(
            est.total_gb > 18.0 && est.total_gb < 21.0,
            "total ma byc ~20 GB (nie 57 GB vLLM): {}",
            est.total_gb
        );
        assert!(
            est.activations_gb > 3.0 && est.activations_gb < 4.5,
            "activations (compute+cuda) ~3-4 GB (nie 41.6 GB): {}",
            est.activations_gb
        );
    }

    #[test]
    fn estimate_vram_dispatches_per_engine() {
        let m = qwen36_27b_q4();
        let base = VramEstimateInput {
            gpu_count: 8,
            gpu_memory_gb_each: 24.0,
            tensor_parallel: 8,
            max_model_len: 1024,
            max_num_seqs: 1,
            weights_bytes_override: Some((15.9 * 1024.0 * 1024.0 * 1024.0) as u64),
            ..Default::default()
        };
        let vllm = estimate_vram(
            &m,
            &VramEstimateInput {
                engine: DeployEngine::Vllm,
                ..base.clone()
            },
        );
        let llama = estimate_vram(
            &m,
            &VramEstimateInput {
                engine: DeployEngine::LlamaCpp,
                ..base.clone()
            },
        );
        // MLX to pojedyncze urzadzenie: budzet z gpu_memory_gb_each, scratch bez
        // 5 GB workspace, brak TP/PP (parallel ignorowany).
        let mlx = estimate_vram(
            &m,
            &VramEstimateInput {
                engine: DeployEngine::Mlx,
                tensor_parallel: 1,
                pipeline_parallel: 1,
                gpu_memory_gb_each: 64.0,
                ..base
            },
        );
        // vLLM dziedziczy ~5 GB workspace × 8 GPU -> activations wielokrotnie wyzsze.
        assert!(
            vllm.activations_gb > llama.activations_gb * 5.0,
            "vLLM activations {} vs llama.cpp {}",
            vllm.activations_gb,
            llama.activations_gb
        );
        // MLX nie ma 5 GB workspace per worker: scratch znacznie mniejszy niz vLLM.
        assert!(
            mlx.activations_gb < vllm.activations_gb,
            "MLX scratch {} < vLLM activations {}",
            mlx.activations_gb,
            vllm.activations_gb
        );
        // 15.9 GB Q4 na 64 GB budzecie MLX musi sie zmiescic z pula KV.
        assert!(mlx.fits_per_gpu, "MLX 15.9 GB na 64 GB: {mlx:?}");
        assert!(mlx.pool_tokens > 0, "MLX pula tokenow: {}", mlx.pool_tokens);
    }

    #[test]
    fn nvfp4_does_not_force_quantization_flag() {
        // Label nvfp4 jest dwuznaczny (modelopt vs compressed-tensors); vLLM
        // sam wykrywa metode z config.json, wiec NIE wolno wymuszac --quantization.
        let mut m = qwen_05b();
        m.quantization = Some("nvfp4".into());
        let input = VramEstimateInput {
            gpu_count: 1,
            gpu_memory_gb_each: 24.0,
            ..Default::default()
        };
        let out = build_vllm_args_string(&m, &input);
        assert!(
            !out.contains("--quantization"),
            "nvfp4 nie powinno emitowac --quantization: {out}"
        );
        assert!(
            !out.contains("modelopt_fp4"),
            "nvfp4 nie powinno emitowac modelopt_fp4: {out}"
        );
    }

    #[test]
    fn qwen_05b_fits_on_single_3090() {
        let m = qwen_05b();
        let input = VramEstimateInput {
            gpu_count: 1,
            gpu_memory_gb_each: 24.0,
            tensor_parallel: 1,
            pipeline_parallel: 1,
            max_model_len: 4096,
            max_num_seqs: 32,
            ..Default::default()
        };
        let est = estimate_vllm_vram(&m, &input);
        assert!(est.fits_per_gpu, "Qwen 0.5B powinien sie miescic: {est:?}");
        // Model puli: ~1.2 GB wag + ~1.6 GB aktywacji to caly staly footprint, reszta
        // 24 GB GPU (~18.7 GB po util) idzie do puli KV — ogrom miejsca dla setek
        // pelnych sekwencji 4k.
        let fixed = est.model_weights_gb + est.activations_gb;
        assert!(
            fixed < 5.0,
            "Staly footprint Qwen 0.5B (wagi+akt) powinien byc < 5 GB: {}",
            fixed
        );
        assert!(
            est.pool_tokens > 1_000_000,
            "Pula 0.5B na 24 GB powinna miescic >1M tokenow: {}",
            est.pool_tokens
        );
        assert!(
            est.concurrent_full_len_seqs > 50.0,
            "Pula powinna dac >50 pelnych sekwencji 4k: {}",
            est.concurrent_full_len_seqs
        );
    }

    #[test]
    fn gemma4_31b_does_not_fit_single_3090() {
        let m = gemma4_31b();
        let input = VramEstimateInput {
            gpu_count: 1,
            gpu_memory_gb_each: 24.0,
            tensor_parallel: 1,
            ..Default::default()
        };
        let est = estimate_vllm_vram(&m, &input);
        assert!(
            !est.fits_per_gpu,
            "Gemma 31B nie moze sie miescic na 1x 24GB"
        );
        assert!(
            est.model_weights_gb > 50.0,
            "31B w bf16 to ~62GB: {}",
            est.model_weights_gb
        );
    }

    #[test]
    fn gemma4_31b_fits_on_6x3090_with_tp2_pp3() {
        let m = gemma4_31b();
        let (tp, pp) = recommend_parallelism(&m, 6);
        assert!(tp * pp == 6, "TP*PP musi rownac 6: {tp}*{pp}");
        assert!(32 % tp as u64 == 0, "TP={tp} musi dzielic 32 heads");
        assert!(60 % pp as u64 == 0, "PP={pp} musi dzielic 60 layers");

        // Realistyczny initial deploy 31B: ctx 4k, max 4 concurrent (KV cache
        // budget ~4 GB). gpu_memory_utilization 0.95 zostawia 1.2 GB na CUDA
        // runtime/allocator co dla H100/A100/3090 jest standardem.
        let input = VramEstimateInput {
            gpu_count: 6,
            gpu_memory_gb_each: 24.0,
            tensor_parallel: tp,
            pipeline_parallel: pp,
            max_model_len: 4096,
            max_num_seqs: 4,
            kv_cache_dtype: "fp8".into(),
            kv_cache_dtype_v: None,
            gpu_memory_utilization: 0.95,
            ..Default::default()
        };
        let est = estimate_vllm_vram(&m, &input);
        assert!(
            est.fits_per_gpu,
            "31B na 6x 3090 z TP*PP=6 musi sie miescic: {est:?}"
        );
    }

    #[test]
    fn analyze_gpu_compat_warns_on_5_gpu_for_gemma() {
        let m = gemma4_31b(); // 32 heads, 16 kv, 60 layers
        let r = analyze_gpu_compatibility(&m, 5);
        // 5 GPU: probujemy 1*5, 5*1, ale 32%5!=0, 60%5=0 OK dla PP=5.
        // Faktycznie (1,5) jest valid bo layers%5=0. Sprawdzmy.
        if r.clean_partition {
            // Akceptowalne - 60 dzieli sie przez 5
            assert_eq!(r.used_pp, 5);
            assert!(r.warning.is_none(), "5 GPU OK gdy layers%5=0: {:?}", r);
        } else {
            assert!(r.warning.is_some());
        }
        // Lista better powinna zawierac 1, 2, 4, 6, 8 (32 dzieli przez 1,2,4,8;
        // 60 dzieli przez 1,2,3,4,5,6,10,12,15,20,30,60)
        assert!(r.better_gpu_counts.contains(&1));
        assert!(r.better_gpu_counts.contains(&4));
        assert!(r.better_gpu_counts.contains(&6));
        println!(
            "Gemma 31B compat dla 5 GPU: tp={} pp={} better={:?} warning={:?}",
            r.used_tp, r.used_pp, r.better_gpu_counts, r.warning
        );
    }

    #[test]
    fn analyze_gpu_compat_warns_on_3_gpu_for_llama8b() {
        let m = ModelSpec {
            num_attention_heads: 32,
            num_key_value_heads: 8,
            num_hidden_layers: 32,
            ..Default::default()
        };
        let r = analyze_gpu_compatibility(&m, 3);
        // 3 GPU dla Llama: 32%3!=0 (TP nope), 32%3!=0 (PP=3 nope) - warning
        assert!(!r.clean_partition);
        assert!(r.warning.is_some());
        // Better counts dla Llama 8B: 1, 2, 4, 8 (dzielniki 32 i 8)
        assert!(r.better_gpu_counts.contains(&1));
        assert!(r.better_gpu_counts.contains(&2));
        assert!(r.better_gpu_counts.contains(&4));
        assert!(r.better_gpu_counts.contains(&8));
        // 3 nie powinno byc na liscie better
        assert!(!r.better_gpu_counts.contains(&3));
    }

    #[test]
    fn analyze_gpu_compat_llamacpp_no_heads_layers_warning() {
        // Architektura ktora vLLM odrzuca dla 8 GPU (24 % 8 != 0 i 65 % 8 != 0):
        // recommend_parallelism dalby fallback TP=1 PP=8. Dla llama.cpp split-mode
        // layer/row dzieli dowolnie, wiec uzywamy wszystkich kart bez warningu.
        // Default = layer-split (TP=1, PP=gpu_count).
        let m = ModelSpec {
            num_attention_heads: 24,
            num_key_value_heads: 4,
            num_hidden_layers: 65,
            ..Default::default()
        };
        let r = analyze_gpu_compatibility_llamacpp(&m, 8);
        assert_eq!(r.used_tp, 1);
        assert_eq!(r.used_pp, 8);
        assert!(r.uses_all_gpus);
        assert!(r.clean_partition);
        assert!(
            r.warning.is_none(),
            "llama.cpp na 8 kartach nie ma ograniczen podzielnosci: {r:?}"
        );
        // Kazda liczba kart 1..=8 dziala dla llama.cpp.
        assert_eq!(r.better_gpu_counts, (1..=8).collect::<Vec<u32>>());
    }

    #[test]
    fn auto_fit_llamacpp_uses_all_gpus_without_head_divisibility() {
        let m = ModelSpec {
            num_attention_heads: 24,
            num_key_value_heads: 4,
            num_hidden_layers: 65,
            hidden_size: 4096,
            max_position_embeddings: 32768,
            ..Default::default()
        };
        let req = AutoFitRequest {
            engine: DeployEngine::LlamaCpp,
            gpu_count: 8,
            gpu_memory_gb_each: 8.0,
            kv_cache_dtype: "auto".into(),
            kv_cache_dtype_v: None,
            gpu_memory_utilization: 0.9,
            requested_max_model_len: None,
            requested_max_num_seqs: None,
            requested_tensor_parallel: None,
            requested_pipeline_parallel: None,
            lock_max_model_len: false,
            lock_max_num_seqs: false,
            lock_tensor_parallel: false,
            weights_bytes_override: Some(16 * 1024 * 1024 * 1024),
        };
        let out = auto_fit_config(&m, &req);
        assert_eq!(
            out.applied.pipeline_parallel, 8,
            "llama.cpp domyslnie uzywa wszystkich kart w layer-split (PP=gpu_count): {out:?}"
        );
        assert_eq!(
            out.applied.tensor_parallel, 1,
            "llama.cpp default to layer-split, nie row (TP=1): {out:?}"
        );
    }

    #[test]
    fn analyze_gpu_compat_no_warning_for_perfect_match() {
        let m = ModelSpec {
            num_attention_heads: 32,
            num_key_value_heads: 16,
            num_hidden_layers: 60,
            ..Default::default()
        };
        let r = analyze_gpu_compatibility(&m, 6); // TP=2 PP=3 idealnie
        assert!(r.clean_partition);
        assert!(r.uses_all_gpus);
        assert!(r.warning.is_none(), "6 GPU dla Gemma 31B perfect: {:?}", r);
    }

    #[test]
    fn recommend_parallelism_avoids_indivisible_heads() {
        let m = gemma4_31b(); // 32 heads
                              // 3 GPU: 32 % 3 != 0, wiec wybiera (1, 3) bo PP dziala lepiej
        let (tp, pp) = recommend_parallelism(&m, 3);
        assert_eq!(tp * pp, 3);
        assert_eq!(32 % tp as u64, 0);
    }

    #[test]
    fn quantization_int4_halves_weights() {
        let mut m = gemma4_31b();
        m.quantization = Some("int4".into());
        let input = VramEstimateInput {
            gpu_count: 1,
            gpu_memory_gb_each: 24.0,
            ..Default::default()
        };
        let est = estimate_vllm_vram(&m, &input);
        // 31B int4 = ~16GB - fits jeden 3090
        assert!(
            est.model_weights_gb < 20.0 && est.model_weights_gb > 12.0,
            "INT4 31B = ~16GB, dostalismy {}",
            est.model_weights_gb
        );
    }

    #[test]
    fn parse_hf_config_extracts_text_config_for_multimodal() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{
            "model_type": "gemma4",
            "architectures": ["Gemma4ForConditionalGeneration"],
            "dtype": "bfloat16",
            "vision_config": {"hidden_size": 1024},
            "text_config": {
                "hidden_size": 5376,
                "num_attention_heads": 32,
                "num_key_value_heads": 16,
                "num_hidden_layers": 60,
                "vocab_size": 262144,
                "head_dim": 256,
                "intermediate_size": 21504,
                "max_position_embeddings": 131072
            }
        }"#,
        )
        .unwrap();
        let spec = parse_hf_config(&json, "google/gemma-4-31B-it").unwrap();
        assert_eq!(spec.hidden_size, 5376);
        assert_eq!(spec.num_attention_heads, 32);
        assert!(spec.has_vision);
        assert_eq!(spec.dtype, "bfloat16");
    }

    #[test]
    fn parse_hf_config_detects_int4_from_name() {
        let json: serde_json::Value = serde_json::from_str(r#"{"hidden_size": 5376}"#).unwrap();
        let spec = parse_hf_config(&json, "Intel/gemma-4-31B-it-int4-AutoRound").unwrap();
        // Wzorzec "AutoRound" wykrywany jako auto_round (canonical etykieta dla
        // Intel AutoRound INT4); bytes_per_param i tak konczy na 0.5625.
        assert_eq!(spec.quantization.as_deref(), Some("auto_round"));
        assert!((spec.bytes_per_param() - 0.5625).abs() < 1e-9);
    }

    #[test]
    fn estimated_params_mixtral_8x7b_counts_all_experts() {
        // Mixtral-8x7B: 8 ekspertow ladowanych w calosci (~46.7B), NIE 8.05B
        // ktore dawal stary wzor traktujacy MoE jak jeden ekspert dense.
        let m = ModelSpec {
            hidden_size: 4096,
            num_attention_heads: 32,
            num_key_value_heads: 8,
            num_hidden_layers: 32,
            vocab_size: 32000,
            head_dim: 128,
            intermediate_size: 14336,
            num_experts: 8,
            num_experts_per_tok: 2,
            moe_intermediate_size: 14336,
            ..Default::default()
        };
        let p = m.estimated_params() as f64;
        let expected = 46.7e9;
        assert!(
            (p - expected).abs() / expected < 0.02,
            "Mixtral-8x7B params ~46.7B (±2%), dostalismy {:.3}B",
            p / 1e9
        );
        assert!(
            p > 40e9,
            "MoE nie moze byc zaniżony do dense ~8B: {:.3}B",
            p / 1e9
        );
    }

    #[test]
    fn estimated_params_qwen25_7b_respects_gqa() {
        // Qwen2.5-7B: GQA (28 glow uwagi, 4 glowy KV) — stary wzor 4h² dawal
        // ~8.23B; GQA-poprawny attn daje ~7.6B.
        let m = ModelSpec {
            hidden_size: 3584,
            num_attention_heads: 28,
            num_key_value_heads: 4,
            num_hidden_layers: 28,
            vocab_size: 152064,
            head_dim: 128,
            intermediate_size: 18944,
            ..Default::default()
        };
        let p = m.estimated_params() as f64;
        let expected = 7.6e9;
        assert!(
            (p - expected).abs() / expected < 0.03,
            "Qwen2.5-7B params ~7.6B (±3%), dostalismy {:.4}B",
            p / 1e9
        );
    }

    #[test]
    fn estimated_params_tied_embeddings_no_double_lm_head() {
        // Qwen2.5-0.5B-like z tie_word_embeddings: lm_head wspoldzieli wagi
        // z embeddingiem, wiec nie liczy sie podwojnie. ~0.49B, NIE ~0.66B.
        let m = ModelSpec {
            hidden_size: 896,
            num_attention_heads: 14,
            num_key_value_heads: 2,
            num_hidden_layers: 24,
            vocab_size: 151936,
            head_dim: 64,
            intermediate_size: 4864,
            tie_word_embeddings: true,
            ..Default::default()
        };
        let p = m.estimated_params() as f64;
        let expected = 0.49e9;
        assert!(
            (p - expected).abs() / expected < 0.05,
            "tied 0.5B params ~0.49B (±5%), dostalismy {:.4}B",
            p / 1e9
        );
        // Bez tied lm_head dodaje vocab×hidden — wynik musi byc wyraznie wiekszy.
        let mut untied = m.clone();
        untied.tie_word_embeddings = false;
        assert!(
            untied.estimated_params() > m.estimated_params(),
            "untied musi byc wieksze (osobny lm_head)"
        );
    }

    #[test]
    fn active_params_moe_below_full_params() {
        // Mixtral aktywuje top-2 z 8 ekspertow: aktywne (~12.9B) << pelne (~46.7B).
        let m = ModelSpec {
            hidden_size: 4096,
            num_attention_heads: 32,
            num_key_value_heads: 8,
            num_hidden_layers: 32,
            vocab_size: 32000,
            head_dim: 128,
            intermediate_size: 14336,
            num_experts: 8,
            num_experts_per_tok: 2,
            moe_intermediate_size: 14336,
            ..Default::default()
        };
        let active = m.active_params() as f64;
        let full = m.estimated_params() as f64;
        assert!(
            active < full,
            "aktywne MoE musza byc mniejsze niz pelne: active={:.3}B full={:.3}B",
            active / 1e9,
            full / 1e9
        );
        let expected_active = 12.9e9;
        assert!(
            (active - expected_active).abs() / expected_active < 0.05,
            "Mixtral aktywne ~12.9B (±5%), dostalismy {:.3}B",
            active / 1e9
        );
    }

    #[test]
    fn active_params_dense_equals_estimated() {
        let m = ModelSpec {
            hidden_size: 3584,
            num_attention_heads: 28,
            num_key_value_heads: 4,
            num_hidden_layers: 28,
            vocab_size: 152064,
            head_dim: 128,
            intermediate_size: 18944,
            ..Default::default()
        };
        assert_eq!(m.active_params(), m.estimated_params());
    }

    #[test]
    fn parse_hf_config_reads_moe_and_tie_fields() {
        // Config w stylu Mixtral: num_local_experts + moe_intermediate_size + tie.
        let json: serde_json::Value = serde_json::from_str(
            r#"{
            "model_type": "mixtral",
            "hidden_size": 4096,
            "num_attention_heads": 32,
            "num_key_value_heads": 8,
            "num_hidden_layers": 32,
            "vocab_size": 32000,
            "head_dim": 128,
            "intermediate_size": 14336,
            "num_local_experts": 8,
            "num_experts_per_tok": 2,
            "moe_intermediate_size": 14336,
            "shared_expert_intermediate_size": 0,
            "tie_word_embeddings": true
        }"#,
        )
        .unwrap();
        let spec = parse_hf_config(&json, "mistralai/Mixtral-8x7B-v0.1").unwrap();
        assert_eq!(spec.num_experts, 8);
        assert_eq!(spec.num_experts_per_tok, 2);
        assert_eq!(spec.moe_intermediate_size, 14336);
        assert_eq!(spec.shared_expert_intermediate_size, 0);
        assert!(spec.tie_word_embeddings);
        // Dla MoE parser publikuje aktywne parametry dla wyswietlania.
        assert!(spec.num_active_parameters > 0);
        assert!((spec.num_active_parameters as f64) < spec.estimated_params() as f64);
    }

    #[test]
    fn parse_safetensors_total_size_reads_metadata() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{
            "metadata": {"total_size": 93405585408},
            "weight_map": {"model.embed_tokens.weight": "model-00001-of-00019.safetensors"}
        }"#,
        )
        .unwrap();
        assert_eq!(parse_safetensors_total_size(&json), Some(93405585408));

        let no_meta: serde_json::Value = serde_json::from_str(r#"{"weight_map": {}}"#).unwrap();
        assert_eq!(parse_safetensors_total_size(&no_meta), None);

        let zero: serde_json::Value =
            serde_json::from_str(r#"{"metadata": {"total_size": 0}}"#).unwrap();
        assert_eq!(parse_safetensors_total_size(&zero), None);
    }

    #[test]
    fn max_context_decreases_when_kv_cache_dtype_fp16() {
        // Wieksze KV (Llama-7B-class, brak GQA) zeby fp16 vs fp8 miala znaczenie.
        // max_position_embeddings ustawione bardzo wysoko, zeby to PULA KV (a nie
        // limit pozycji) ograniczala max_context — wtedy fp8 (polowa szerokosci KV)
        // realnie podwaja liczbe mieszczonych tokenow.
        let m = ModelSpec {
            model_type: "llama".into(),
            architectures: vec!["LlamaForCausalLM".into()],
            dtype: "bfloat16".into(),
            hidden_size: 4096,
            num_attention_heads: 32,
            num_key_value_heads: 32,
            num_hidden_layers: 32,
            vocab_size: 32000,
            head_dim: 128,
            intermediate_size: 11008,
            max_position_embeddings: 524288,
            ..Default::default()
        };
        // 80GB GPU (A100/H100): staly footprint (~14 GB) maly wobec puli ~58 GB,
        // wiec o max ctx decyduje szerokosc KV.
        let mut input = VramEstimateInput {
            gpu_count: 1,
            gpu_memory_gb_each: 80.0,
            kv_cache_dtype: "auto".into(),
            max_num_seqs: 16,
            ..Default::default()
        };
        let ctx_fp16 = max_context_for_budget(&m, &input);
        input.kv_cache_dtype = "fp8".into();
        let ctx_fp8 = max_context_for_budget(&m, &input);
        assert!(
            ctx_fp8 > ctx_fp16,
            "fp8 KV powinno dac wiecej ctx: fp8={ctx_fp8} fp16={ctx_fp16}"
        );
        assert!(
            ctx_fp8 >= ctx_fp16 * 2 - 512,
            "fp8 powinno dac ~2x wiecej (lub blisko): fp8={ctx_fp8} fp16={ctx_fp16}"
        );
    }

    /// Zbudowany jak gemma-2-27b: 46 layers, GQA 32/16, hidden 4608, vocab 256k.
    /// Cel: 4× 24 GB powinno dac TP=4, kv_cache_gb < 30, per_gpu_gb < 24,
    /// max_supported_num_seqs >= 64 dla ctx 32k.
    fn gemma2_27b_like() -> ModelSpec {
        ModelSpec {
            model_type: "gemma2".into(),
            architectures: vec!["Gemma2ForCausalLM".into()],
            dtype: "bfloat16".into(),
            hidden_size: 4608,
            num_attention_heads: 32,
            num_key_value_heads: 16,
            num_hidden_layers: 46,
            vocab_size: 256000,
            head_dim: 128,
            intermediate_size: 36864,
            max_position_embeddings: 32768,
            num_parameters: 27_000_000_000,
            ..Default::default()
        }
    }

    #[test]
    fn gemma2_27b_fits_on_4x24gb_at_32k_ctx() {
        let m = gemma2_27b_like();
        let req = AutoFitRequest {
            engine: DeployEngine::Vllm,
            gpu_count: 4,
            gpu_memory_gb_each: 24.0,
            kv_cache_dtype: "auto".into(),
            kv_cache_dtype_v: None,
            gpu_memory_utilization: 0.9,
            requested_max_model_len: Some(32768),
            requested_max_num_seqs: Some(8),
            requested_tensor_parallel: None,
            requested_pipeline_parallel: None,
            lock_max_model_len: false,
            lock_max_num_seqs: false,
            lock_tensor_parallel: false,
            weights_bytes_override: None,
        };
        let fit = auto_fit_config(&m, &req);
        assert!(fit.error.is_none(), "Powinno fits: {:?}", fit.error);
        // VRAM-aware picker preferuje najmniejsze TP ktore fits - dla 4x24GB to
        // TP=2 PP=2 (13.5 GB weights + 5 GB act = ~18 GB per GPU). TP=4 PP=1 tez OK
        // ale wybierany rzadziej. Akceptujemy oba prawidlowe podzialy.
        let parallel = fit.applied.tensor_parallel * fit.applied.pipeline_parallel;
        assert_eq!(
            parallel, 4,
            "TP*PP musi=4 dla 4 GPU: TP={} PP={}",
            fit.applied.tensor_parallel, fit.applied.pipeline_parallel
        );
        let est = estimate_vllm_vram(&m, &fit.applied);
        assert!(est.fits_per_gpu, "Per GPU musi fits: {est:?}");
        assert!(
            est.kv_cache_gb < 30.0,
            "kv_cache_gb < 30: got {}",
            est.kv_cache_gb
        );
        assert!(
            est.per_gpu_gb < 24.0,
            "per_gpu_gb < 24: got {}",
            est.per_gpu_gb
        );
        // Sprawdz max ctx (powinien byc znaczacy - co najmniej 4k).
        let max_ctx = max_context_for_budget(&m, &fit.applied);
        assert!(
            max_ctx >= 4096,
            "max_supported_model_len >= 4k: got {}",
            max_ctx
        );
    }

    #[test]
    fn auto_fit_keeps_seqs_as_cap_when_ctx_locked() {
        // Model puli: gdy ctx zalockowane na wartosc mieszczaca sie w puli, suwak
        // max_num_seqs to czysty cap schedulera — NIE jest obnizany pod pamiec.
        // (Przed fixem KRYT-2 ctx=131072 × 256 "wymagal" 512 GiB i seqs spadalo do 4.)
        let m = gemma2_27b_like();
        let fit = auto_fit_config(
            &m,
            &AutoFitRequest {
                engine: DeployEngine::Vllm,
                gpu_count: 4,
                gpu_memory_gb_each: 24.0,
                kv_cache_dtype: "auto".into(),
                kv_cache_dtype_v: None,
                gpu_memory_utilization: 0.9,
                requested_max_model_len: Some(16384),
                requested_max_num_seqs: Some(256),
                requested_tensor_parallel: None,
                requested_pipeline_parallel: None,
                lock_max_model_len: true,
                lock_max_num_seqs: false,
                lock_tensor_parallel: false,
                weights_bytes_override: None,
            },
        );
        assert!(fit.error.is_none(), "Powinno znalezc fit: {:?}", fit.error);
        assert_eq!(fit.applied.max_model_len, 16384, "ctx zachowane (locked)");
        assert_eq!(
            fit.applied.max_num_seqs, 256,
            "seqs to cap schedulera, nie pamiec — musi zostac 256, got {}",
            fit.applied.max_num_seqs
        );
        assert!(
            !fit.auto_adjusted.iter().any(|s| s == "max_num_seqs"),
            "max_num_seqs NIE moze byc auto-cap'owane pod pamiec: {:?}",
            fit.auto_adjusted
        );
        let est = estimate_vllm_vram(&m, &fit.applied);
        assert!(est.fits_per_gpu, "Po auto-fit musi fits: {est:?}");
    }

    #[test]
    fn auto_fit_errors_when_both_locked_overflow() {
        let m = gemma2_27b_like();
        let fit = auto_fit_config(
            &m,
            &AutoFitRequest {
                engine: DeployEngine::Vllm,
                gpu_count: 4,
                gpu_memory_gb_each: 24.0,
                kv_cache_dtype: "auto".into(),
                kv_cache_dtype_v: None,
                gpu_memory_utilization: 0.9,
                requested_max_model_len: Some(1_000_000),
                requested_max_num_seqs: Some(256),
                requested_tensor_parallel: None,
                requested_pipeline_parallel: None,
                lock_max_model_len: true,
                lock_max_num_seqs: true,
                lock_tensor_parallel: false,
                weights_bytes_override: None,
            },
        );
        assert!(fit.error.is_some(), "Oba locked + overflow musi dac error");
        let err = fit.error.unwrap();
        assert!(
            err.contains("KV cache") || err.contains("budget") || err.contains("Locked"),
            "Error message powinien wymieniac KV/budget: {err}"
        );
    }

    #[test]
    fn auto_fit_no_locks_caps_seqs_to_fit() {
        // Gemma 27B na 2x 24GB (ciasno) bez lockow. Polityka: num_seqs default 1,
        // ctx pcham do max z VRAM. Tu i tak moze byc error (model za duzy na 2 GPU).
        let m = gemma2_27b_like();
        let fit = auto_fit_config(
            &m,
            &AutoFitRequest {
                engine: DeployEngine::Vllm,
                gpu_count: 2,
                gpu_memory_gb_each: 24.0,
                kv_cache_dtype: "auto".into(),
                kv_cache_dtype_v: None,
                gpu_memory_utilization: 0.9,
                requested_max_model_len: Some(32768),
                requested_max_num_seqs: Some(64),
                requested_tensor_parallel: None,
                requested_pipeline_parallel: None,
                lock_max_model_len: false,
                lock_max_num_seqs: false,
                lock_tensor_parallel: false,
                weights_bytes_override: None,
            },
        );
        if fit.error.is_none() {
            let est = estimate_vllm_vram(&m, &fit.applied);
            assert!(est.fits_per_gpu, "Po auto-fit musi fits: {est:?}");
        }
    }

    #[test]
    fn auto_default_serves_with_full_ctx_and_server_seqs() {
        // Gemma2 27B-like, 4x 24GB, brak request + brak lockow. max_model_len
        // wyciagniety do max mieszczacego JEDNA sekwencje w puli, capped przez
        // model.max_position_embeddings = 32768. max_num_seqs to rekomendacja z
        // osiagalnej wspolbieznosci puli (floor(pool_tokens/ctx)), nie slepe 256.
        let m = gemma2_27b_like();
        let fit = auto_fit_config(
            &m,
            &AutoFitRequest {
                engine: DeployEngine::Vllm,
                gpu_count: 4,
                gpu_memory_gb_each: 24.0,
                kv_cache_dtype: "auto".into(),
                kv_cache_dtype_v: None,
                gpu_memory_utilization: 0.9,
                requested_max_model_len: None,
                requested_max_num_seqs: None,
                requested_tensor_parallel: None,
                requested_pipeline_parallel: None,
                lock_max_model_len: false,
                lock_max_num_seqs: false,
                lock_tensor_parallel: false,
                weights_bytes_override: None,
            },
        );
        assert!(fit.error.is_none(), "Powinno znalezc fit: {:?}", fit.error);
        // Pula KV mocno przewyzsza 32k tokenow dla 1 sekwencji, wiec ctx siega
        // pelnego model.max_position_embeddings (32768) bez cappingu KV.
        assert_eq!(
            fit.applied.max_model_len, m.max_position_embeddings,
            "ctx powinien siegnac model.max_position_embeddings, got {}",
            fit.applied.max_model_len
        );
        let est = estimate_vllm_vram(&m, &fit.applied);
        assert!(est.fits_per_gpu, "Per GPU musi fits: {est:?}");
        assert!(
            est.concurrent_full_len_seqs >= 1.0,
            "Pula powinna pomiescic >=1 pelna sekwencje 32k: {}",
            est.concurrent_full_len_seqs
        );
        // Rekomendacja seqs = osiagalna wspolbieznosc puli, clamp 1..=256.
        let pool_seqs = (est.pool_tokens / fit.applied.max_model_len).clamp(1, 256);
        assert_eq!(
            fit.applied.max_num_seqs, pool_seqs,
            "seqs ma odzwierciedlac pule (floor(pool/ctx)), got {} pool_seqs {}",
            fit.applied.max_num_seqs, pool_seqs
        );
    }

    #[test]
    fn auto_default_caps_ctx_at_model_max_position() {
        // Maly model (Qwen 0.5B, max_position 32768) na 1x24GB. KV budget olbrzymi
        // wzgledem modelu - ctx ma byc capped przez model.max_position_embeddings,
        // a nie absolutnym ceiling 1M.
        let m = qwen_05b();
        let fit = auto_fit_config(
            &m,
            &AutoFitRequest {
                engine: DeployEngine::Vllm,
                gpu_count: 1,
                gpu_memory_gb_each: 24.0,
                kv_cache_dtype: "auto".into(),
                kv_cache_dtype_v: None,
                gpu_memory_utilization: 0.9,
                requested_max_model_len: None,
                requested_max_num_seqs: None,
                requested_tensor_parallel: None,
                requested_pipeline_parallel: None,
                lock_max_model_len: false,
                lock_max_num_seqs: false,
                lock_tensor_parallel: false,
                weights_bytes_override: None,
            },
        );
        assert!(fit.error.is_none(), "Powinno fits: {:?}", fit.error);
        assert_eq!(
            fit.applied.max_model_len, m.max_position_embeddings,
            "Maly model: ctx == model.max_position_embeddings ({}), got {}",
            m.max_position_embeddings, fit.applied.max_model_len
        );
        // Olbrzymia pula 0.5B na 24 GB: rekomendacja seqs > 1 ale zgodna z pula.
        let est = estimate_vllm_vram(&m, &fit.applied);
        let pool_seqs = (est.pool_tokens / fit.applied.max_model_len).clamp(1, 256);
        assert_eq!(fit.applied.max_num_seqs, pool_seqs);
        assert!(
            fit.applied.max_num_seqs > 1,
            "0.5B na 24 GB ma dac wspolbieznosc > 1: {}",
            fit.applied.max_num_seqs
        );
    }

    #[test]
    fn quant_label_to_bytes_mapping() {
        // 4-bit warianty -> 0.5625 (z overhead group-scales)
        for q in &[
            "nvfp4",
            "fp4",
            "awq",
            "gptq",
            "int4",
            "auto-round",
            "bnb_4bit",
            "load_in_4bit",
            "w4a16",
            "compressed-tensors-4bit",
        ] {
            assert_eq!(
                quant_label_to_bytes(q),
                Some(0.5625),
                "4-bit '{}' powinno dac 0.5625",
                q
            );
        }
        // mxfp4: 4.25 bit (skalar e8m0 per 32) -> 0.5312, wezej niz group-scale 4-bit
        assert_eq!(quant_label_to_bytes("mxfp4"), Some(0.5312));
        // fp8 per-tensor/block -> 1.0 (znikomy overhead skali)
        for q in &["fp8", "fp8-e4m3", "fp8_e5m2", "modelopt_fp8"] {
            assert_eq!(
                quant_label_to_bytes(q),
                Some(1.0),
                "fp8 '{}' powinno dac 1.0",
                q
            );
        }
        // int8 group-scale -> 1.0625
        for q in &["int8", "bnb_8bit", "w8a8", "load_in_8bit"] {
            assert_eq!(
                quant_label_to_bytes(q),
                Some(1.0625),
                "int8 '{}' powinno dac 1.0625",
                q
            );
        }
        // Pelne dtypes
        assert_eq!(quant_label_to_bytes("fp16"), Some(2.0));
        assert_eq!(quant_label_to_bytes("bf16"), Some(2.0));
        assert_eq!(quant_label_to_bytes("fp32"), Some(4.0));
        // Nieznane -> None (fallback do dtype)
        assert_eq!(quant_label_to_bytes("definitely-not-a-quant"), None);
    }

    #[test]
    fn kv_bytes_per_element_engine_aware() {
        // llama.cpp: warianty q*_0 maja realne (mniejsze) szerokosci z ggml-common.h.
        assert_eq!(
            kv_bytes_per_element(DeployEngine::LlamaCpp, "q4_0"),
            Some(0.5625)
        );
        assert_eq!(
            kv_bytes_per_element(DeployEngine::LlamaCpp, "q8_0"),
            Some(1.0625)
        );
        assert_eq!(
            kv_bytes_per_element(DeployEngine::LlamaCpp, "q5_1"),
            Some(0.75)
        );
        assert_eq!(
            kv_bytes_per_element(DeployEngine::LlamaCpp, "q5_0"),
            Some(0.6875)
        );
        assert_eq!(
            kv_bytes_per_element(DeployEngine::LlamaCpp, "q4_1"),
            Some(0.625)
        );
        assert_eq!(
            kv_bytes_per_element(DeployEngine::LlamaCpp, "iq4_nl"),
            Some(0.5625)
        );
        assert_eq!(
            kv_bytes_per_element(DeployEngine::LlamaCpp, "auto"),
            Some(2.0)
        );
        // Normalizacja '-' -> '_'.
        assert_eq!(
            kv_bytes_per_element(DeployEngine::LlamaCpp, "Q4-0"),
            Some(0.5625)
        );
        // Token vLLM (fp8) nieprawidlowy dla llama.cpp.
        assert_eq!(kv_bytes_per_element(DeployEngine::LlamaCpp, "bogus"), None);
        assert_eq!(kv_bytes_per_element(DeployEngine::LlamaCpp, "fp8"), None);

        // vLLM: tylko auto/f16-rodzina (2.0) i fp8-rodzina (1.0).
        assert_eq!(kv_bytes_per_element(DeployEngine::Vllm, "fp8"), Some(1.0));
        assert_eq!(
            kv_bytes_per_element(DeployEngine::Vllm, "fp8_e4m3"),
            Some(1.0)
        );
        assert_eq!(kv_bytes_per_element(DeployEngine::Vllm, "auto"), Some(2.0));
        assert_eq!(kv_bytes_per_element(DeployEngine::Vllm, "bf16"), Some(2.0));
        // Etykiety llama.cpp (q*_0) nieprawidlowe dla vLLM.
        assert_eq!(kv_bytes_per_element(DeployEngine::Vllm, "q4_0"), None);
        assert_eq!(kv_bytes_per_element(DeployEngine::Vllm, "q8_0"), None);
    }

    #[test]
    fn kv_bytes_for_ctx_separate_k_and_v() {
        // GQA-style spec: 32 layers, 8 kv_heads, head_dim 128, no SWA — the
        // legacy uniform fallback path of kv_layer_elem_sums.
        let m = ModelSpec {
            num_hidden_layers: 32,
            num_key_value_heads: 8,
            num_attention_heads: 32,
            head_dim: 128,
            hidden_size: 4096,
            ..Default::default()
        };
        let factor = 32.0 * 8.0 * 128.0; // layers * kv_heads * head_dim

        // Rozne K/V: K=q8_0 (1.0625), V=q4_0 (0.5625) -> suma osobnych szerokosci.
        let mixed = m.kv_bytes_for_ctx(DeployEngine::LlamaCpp, "q8_0", "q4_0", 1);
        assert!((mixed - factor * (1.0625 + 0.5625)).abs() < 1e-9);

        // k == v: 2 × szerokosc pojedynczego cache.
        let symmetric = m.kv_bytes_for_ctx(DeployEngine::LlamaCpp, "q4_0", "q4_0", 1);
        assert!((symmetric - factor * 2.0 * 0.5625).abs() < 1e-9);

        // Bez SWA wynik skaluje sie liniowo z ctx.
        let ctx_4k = m.kv_bytes_for_ctx(DeployEngine::LlamaCpp, "q4_0", "q4_0", 4096);
        assert!((ctx_4k - symmetric * 4096.0).abs() < 1e-3);

        // Nieprawidlowa etykieta fallbackuje do 2.0 (fp16) per element.
        let fallback = m.kv_bytes_for_ctx(DeployEngine::LlamaCpp, "bogus", "bogus", 1);
        assert!((fallback - factor * 2.0 * 2.0).abs() < 1e-9);
    }

    #[test]
    fn kv_bytes_for_ctx_gemma4_swa_matches_real_header() {
        // Real gemma4-31b @ ctx 262144, f16: global layers 80 KiB/token × 262144
        // = 20.0 GiB; SWA layers 800 KiB/token capped at window 1024 (+512
        // ubatch padding) ≈ 1.17 GiB. Total ≈ 21.17 GiB vs the ideal 20.78 GiB
        // (padding adds ~0.39 GiB). The old uniform formula gave ~960 GB.
        let m = gemma4_31b();
        let kv = m.kv_bytes_for_ctx(DeployEngine::LlamaCpp, "f16", "f16", 262144);
        let kv_gib = kv / (1024.0 * 1024.0 * 1024.0);
        assert!(
            (kv_gib - 20.78).abs() < 0.45,
            "gemma4 KV @262k f16 ma byc ~20.78-21.2 GiB, got {kv_gib}"
        );
        // q8_0 K+V: 1.0625/2.0 ratio vs f16.
        let kv_q8 = m.kv_bytes_for_ctx(DeployEngine::LlamaCpp, "q8_0", "q8_0", 262144);
        assert!(((kv_q8 / kv) - 1.0625 / 2.0).abs() < 1e-9);
    }

    #[test]
    fn quantization_detected_from_repo_name() {
        assert_eq!(
            detect_quant_from_name("LilaRest/gemma-4-31B-it-NVFP4-turbo").as_deref(),
            Some("nvfp4")
        );
        // AutoRound pattern wygrywa nad surowym "int4" - i tak konczy na 4-bit.
        assert_eq!(
            detect_quant_from_name("Intel/foo-int4-AutoRound").as_deref(),
            Some("auto_round")
        );
        assert_eq!(
            detect_quant_from_name("user/Llama-3-8B-AWQ").as_deref(),
            Some("awq")
        );
        assert_eq!(
            detect_quant_from_name("user/Mixtral-8x7B-GPTQ").as_deref(),
            Some("gptq")
        );
        assert_eq!(
            detect_quant_from_name("nvidia/foo-FP8").as_deref(),
            Some("fp8")
        );
        assert_eq!(
            detect_quant_from_name("user/Foo-MXFP4-Instruct").as_deref(),
            Some("mxfp4")
        );
        assert_eq!(
            detect_quant_from_name("Qwen/Qwen2.5-7B-Instruct-GGUF-Q4_K_M").as_deref(),
            Some("int4")
        );
        // Brak hinta -> None
        assert!(detect_quant_from_name("meta-llama/Llama-3-70B-Instruct").is_none());
    }

    #[test]
    fn quantization_detected_from_hf_config() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{
            "quantization_config": {"quant_method": "awq", "bits": 4, "group_size": 128}
        }"#,
        )
        .unwrap();
        let q = detect_quantization("user/foo", &json, None);
        assert_eq!(q.as_deref(), Some("awq"));

        // bitsandbytes 4-bit przez load_in_4bit flag
        let json2: serde_json::Value = serde_json::from_str(
            r#"{
            "quantization_config": {"quant_method": "bitsandbytes", "load_in_4bit": true}
        }"#,
        )
        .unwrap();
        assert_eq!(
            detect_quantization("user/foo", &json2, None).as_deref(),
            Some("bnb_4bit")
        );

        // Modelopt NVFP4
        let json3: serde_json::Value = serde_json::from_str(
            r#"{
            "quantization_config": {"quant_method": "modelopt", "bits": 4}
        }"#,
        )
        .unwrap();
        assert_eq!(
            detect_quantization("user/foo", &json3, None).as_deref(),
            Some("nvfp4")
        );

        // compressed-tensors 8-bit
        let json4: serde_json::Value = serde_json::from_str(
            r#"{
            "quantization_config": {"quant_method": "compressed-tensors", "bits": 8}
        }"#,
        )
        .unwrap();
        assert_eq!(
            detect_quantization("user/foo", &json4, None).as_deref(),
            Some("w8a16")
        );
    }

    #[test]
    fn quantization_override_wins_over_config() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{
            "quantization_config": {"quant_method": "awq", "bits": 4}
        }"#,
        )
        .unwrap();
        // User wymusza fp16 mimo ze config mowi awq.
        assert_eq!(
            detect_quantization("user/foo", &json, Some("fp16")).as_deref(),
            Some("fp16")
        );
        // "none" / "auto" wylacza override -> wraca do config.
        assert_eq!(
            detect_quantization("user/foo", &json, Some("none")).as_deref(),
            Some("awq")
        );
        assert_eq!(
            detect_quantization("user/foo", &json, Some("auto")).as_deref(),
            Some("awq")
        );
    }

    #[test]
    fn user_case_gemma_30b_nvfp4_fits_4x24gb() {
        // LilaRest/gemma-4-31B-it-NVFP4-turbo: 30.6B params, NVFP4.
        // Wagi: 30.6B × 0.5625 = 17.2 GB. Per GPU (TP=2/PP=2): ~4.3 GB.
        // Cala konfiguracja z 32k ctx musi sie zmiescic luxurowo na 4×24GB.
        let mut m = gemma4_31b();
        m.num_parameters = 30_600_000_000;
        m.quantization = Some("nvfp4".into());

        let input = VramEstimateInput {
            engine: DeployEngine::default(),
            gpu_count: 4,
            gpu_memory_gb_each: 24.0,
            tensor_parallel: 2,
            pipeline_parallel: 2,
            max_model_len: 32768,
            max_num_seqs: 1,
            max_num_batched_tokens: 8192,
            kv_cache_dtype: "auto".into(),
            kv_cache_dtype_v: None,
            gpu_memory_utilization: 0.9,
            activation_overhead_pct: 10.0,
            weights_bytes_override: None,
        };
        let est = estimate_vllm_vram(&m, &input);
        // Wagi powinny byc ~16-18 GB (vs 56.9 GB w bf16).
        assert!(
            est.model_weights_gb >= 14.0 && est.model_weights_gb <= 20.0,
            "NVFP4 30.6B weights ~16 GB, got {}",
            est.model_weights_gb
        );
        assert!(est.fits_per_gpu, "NVFP4 30.6B na 4×24GB musi fits: {est:?}");
        // Model puli: per_gpu_gb wypelnia sie pula KV (~usable), wiec "komfort"
        // mierzymy stalym footprintem (wagi/GPU + aktywacje) — musi byc maly,
        // zostawiajac wielka pule na KV.
        let fixed = est.model_weights_gb / 4.0 + est.activations_gb / 4.0;
        assert!(
            fixed < 10.0,
            "Staly footprint per GPU (wagi/GPU + akt) powinien byc maly: got {}",
            fixed
        );
        assert!(
            est.kv_pool_gb > 30.0,
            "Pula KV cluster-wide powinna byc duza (>30 GB): got {}",
            est.kv_pool_gb
        );
    }

    #[test]
    fn user_case_gemma_30b_nvfp4_auto_fit_max_ctx() {
        let mut m = gemma4_31b();
        m.num_parameters = 30_600_000_000;
        m.quantization = Some("nvfp4".into());
        let fit = auto_fit_config(
            &m,
            &AutoFitRequest {
                engine: DeployEngine::Vllm,
                gpu_count: 4,
                gpu_memory_gb_each: 24.0,
                kv_cache_dtype: "auto".into(),
                kv_cache_dtype_v: None,
                gpu_memory_utilization: 0.9,
                requested_max_model_len: None,
                requested_max_num_seqs: None,
                requested_tensor_parallel: None,
                requested_pipeline_parallel: None,
                lock_max_model_len: false,
                lock_max_num_seqs: false,
                lock_tensor_parallel: false,
                weights_bytes_override: None,
            },
        );
        assert!(fit.error.is_none(), "Powinno znalezc fit: {:?}", fit.error);
        // SWA-aware KV: 50 warstw okienkowych nie rosnie z ctx, wiec male wagi
        // NVFP4 (~17 GB) pozwalaja na bardzo duzy kontekst na 4×24 GB.
        assert!(
            fit.applied.max_model_len >= 32768,
            "Z malymi wagami (NVFP4) pula KV powinna dac duzy ctx (>= 32k), got {}",
            fit.applied.max_model_len
        );
        assert!(fit.applied.max_model_len <= m.max_position_embeddings);
        // Rekomendacja seqs z osiagalnej wspolbieznosci puli, nie slepe 256.
        let est = estimate_vllm_vram(&m, &fit.applied);
        assert!(est.fits_per_gpu, "Po auto-fit musi fits: {est:?}");
        let pool_seqs = (est.pool_tokens / fit.applied.max_model_len).clamp(1, 256);
        assert_eq!(fit.applied.max_num_seqs, pool_seqs);
    }

    // --- Pomocniki budujace syntetyczny naglowek GGUF (v3, little-endian) ---

    fn gguf_push_string(buf: &mut Vec<u8>, s: &str) {
        buf.extend_from_slice(&(s.len() as u64).to_le_bytes());
        buf.extend_from_slice(s.as_bytes());
    }

    fn gguf_kv_u32(buf: &mut Vec<u8>, key: &str, val: u32) {
        gguf_push_string(buf, key);
        buf.extend_from_slice(&4u32.to_le_bytes()); // value_type 4 = u32
        buf.extend_from_slice(&val.to_le_bytes());
    }

    fn gguf_kv_string(buf: &mut Vec<u8>, key: &str, val: &str) {
        gguf_push_string(buf, key);
        buf.extend_from_slice(&8u32.to_le_bytes()); // value_type 8 = string
        gguf_push_string(buf, val);
    }

    /// Tablica u32 (value_type 9, elem_type 4) - per-layer metadane (kv heads,
    /// SWA pattern).
    fn gguf_kv_u32_array(buf: &mut Vec<u8>, key: &str, items: &[u32]) {
        gguf_push_string(buf, key);
        buf.extend_from_slice(&9u32.to_le_bytes()); // value_type 9 = array
        buf.extend_from_slice(&4u32.to_le_bytes()); // elem_type 4 = u32
        buf.extend_from_slice(&(items.len() as u64).to_le_bytes());
        for it in items {
            buf.extend_from_slice(&it.to_le_bytes());
        }
    }

    /// Tablica stringow (value_type 9, elem_type 8) - test poprawnego przejscia.
    fn gguf_kv_string_array(buf: &mut Vec<u8>, key: &str, items: &[&str]) {
        gguf_push_string(buf, key);
        buf.extend_from_slice(&9u32.to_le_bytes()); // value_type 9 = array
        buf.extend_from_slice(&8u32.to_le_bytes()); // elem_type 8 = string
        buf.extend_from_slice(&(items.len() as u64).to_le_bytes());
        for it in items {
            gguf_push_string(buf, it);
        }
    }

    #[test]
    fn parse_gguf_header_maps_qwen2_spec() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"GGUF");
        buf.extend_from_slice(&3u32.to_le_bytes()); // version 3
        buf.extend_from_slice(&0u64.to_le_bytes()); // tensor_count
                                                    // metadata_kv_count = 8
        buf.extend_from_slice(&8u64.to_le_bytes());

        gguf_kv_string(&mut buf, "general.architecture", "qwen2");
        gguf_kv_u32(&mut buf, "qwen2.block_count", 24);
        gguf_kv_u32(&mut buf, "qwen2.embedding_length", 896);
        gguf_kv_u32(&mut buf, "qwen2.attention.head_count", 14);
        gguf_kv_u32(&mut buf, "qwen2.attention.head_count_kv", 2);
        gguf_kv_u32(&mut buf, "qwen2.context_length", 32768);
        gguf_kv_u32(&mut buf, "qwen2.feed_forward_length", 4864);
        // Tablica tokenizera (3 stringi) - vocab_size z dlugosci + test array skip.
        gguf_kv_string_array(
            &mut buf,
            "tokenizer.ggml.tokens",
            &["<pad>", "hello", "world"],
        );

        let spec = parse_gguf_header(&buf, "qwen2.5-0.5b-instruct-q4_k_m.gguf")
            .expect("parser GGUF powinien przejsc");

        assert_eq!(spec.model_type, "qwen2");
        assert_eq!(spec.num_hidden_layers, 24);
        assert_eq!(spec.hidden_size, 896);
        assert_eq!(spec.num_attention_heads, 14);
        assert_eq!(spec.num_key_value_heads, 2);
        assert_eq!(spec.max_position_embeddings, 32768);
        assert_eq!(spec.intermediate_size, 4864);
        // head_dim wyliczone: 896 / 14 = 64.
        assert_eq!(spec.head_dim, 64);
        // vocab_size z dlugosci tablicy tokenow = 3 (poprawne przejscie array).
        assert_eq!(spec.vocab_size, 3);
        // Quantization z nazwy pliku Q4_K_M -> int4.
        assert_eq!(spec.quantization.as_deref(), Some("int4"));
    }

    #[test]
    fn parse_gguf_header_rejects_bad_magic() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"XXXX");
        buf.extend_from_slice(&3u32.to_le_bytes());
        assert!(parse_gguf_header(&buf, "x.gguf").is_err());
    }

    #[test]
    fn parse_gguf_header_rejects_v1() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"GGUF");
        buf.extend_from_slice(&1u32.to_le_bytes()); // v1 martwy - odrzuc
        buf.extend_from_slice(&0u64.to_le_bytes());
        buf.extend_from_slice(&0u64.to_le_bytes());
        assert!(parse_gguf_header(&buf, "x.gguf").is_err());
    }

    #[test]
    fn parse_gguf_header_early_stops_on_truncated_tokenizer_array() {
        // Symuluje model 128k+ vocab: tablica tokenizera urywa sie w polowie
        // (bufor 1/8 MiB konczy sie przed jej koncem), ale wszystkie pola
        // architektury + count tablicy juz przeczytane. Early-stop musi zwrocic
        // poprawny ModelSpec z vocab_size = zadeklarowany count, NIE blad.
        let mut buf = Vec::new();
        buf.extend_from_slice(b"GGUF");
        buf.extend_from_slice(&3u32.to_le_bytes()); // version 3
        buf.extend_from_slice(&0u64.to_le_bytes()); // tensor_count
        buf.extend_from_slice(&8u64.to_le_bytes()); // metadata_kv_count = 8

        gguf_kv_string(&mut buf, "general.architecture", "qwen2");
        gguf_kv_u32(&mut buf, "qwen2.block_count", 24);
        gguf_kv_u32(&mut buf, "qwen2.embedding_length", 896);
        gguf_kv_u32(&mut buf, "qwen2.attention.head_count", 14);
        gguf_kv_u32(&mut buf, "qwen2.attention.head_count_kv", 2);
        gguf_kv_u32(&mut buf, "qwen2.context_length", 32768);
        gguf_kv_u32(&mut buf, "qwen2.feed_forward_length", 4864);

        // Tablica tokenizera: deklaruje 150000 elementow, ale dostarczamy tylko 2
        // stringi a potem bufor sie urywa - tak jak przy capie 8 MiB na realnym modelu.
        gguf_push_string(&mut buf, "tokenizer.ggml.tokens");
        buf.extend_from_slice(&9u32.to_le_bytes()); // value_type 9 = array
        buf.extend_from_slice(&8u32.to_le_bytes()); // elem_type 8 = string
        buf.extend_from_slice(&150_000u64.to_le_bytes()); // zadeklarowany vocab
        gguf_push_string(&mut buf, "<pad>");
        gguf_push_string(&mut buf, "hello");
        // ... reszta tablicy "ucieta" - bufor konczy sie tutaj.

        let spec = parse_gguf_header(&buf, "qwen2.5-0.5b-instruct-q4_k_m.gguf")
            .expect("early-stop powinien zwrocic ModelSpec mimo urwanej tablicy");

        assert_eq!(spec.model_type, "qwen2");
        assert_eq!(spec.num_hidden_layers, 24);
        assert_eq!(spec.hidden_size, 896);
        assert_eq!(spec.num_attention_heads, 14);
        assert_eq!(spec.num_key_value_heads, 2);
        assert_eq!(spec.intermediate_size, 4864);
        assert_eq!(spec.max_position_embeddings, 32768);
        // vocab_size z zadeklarowanego count tablicy (odczytany PRZED elementami).
        assert_eq!(spec.vocab_size, 150_000);
    }

    #[test]
    fn parse_gguf_header_truncated_array_before_fields_is_error() {
        // Tablica urywa sie ZANIM zebralismy komplet pol architektury - to musi
        // byc blad (caller dociaga wiekszy zakres), a NIE cichy ModelSpec z zerami.
        let mut buf = Vec::new();
        buf.extend_from_slice(b"GGUF");
        buf.extend_from_slice(&3u32.to_le_bytes());
        buf.extend_from_slice(&0u64.to_le_bytes());
        buf.extend_from_slice(&5u64.to_le_bytes()); // metadata_kv_count = 5

        gguf_kv_string(&mut buf, "general.architecture", "qwen2");
        gguf_kv_u32(&mut buf, "qwen2.block_count", 24);
        // Brak embedding_length/head_count/feed_forward_length/context_length.

        // Duza tablica urywa sie - wymagane pola NIE sa kompletne.
        gguf_push_string(&mut buf, "tokenizer.ggml.tokens");
        buf.extend_from_slice(&9u32.to_le_bytes());
        buf.extend_from_slice(&8u32.to_le_bytes());
        buf.extend_from_slice(&150_000u64.to_le_bytes());
        gguf_push_string(&mut buf, "<pad>");
        // bufor urwany.

        let err = parse_gguf_header(&buf, "x-q4_k_m.gguf")
            .expect_err("urwana tablica przed kompletem pol musi byc bledem");
        let msg = err.to_string();
        assert!(
            msg.contains("urwana") || msg.contains("za malo"),
            "blad powinien jasno wskazywac na brak bajtow: {msg}"
        );
    }

    #[test]
    fn gguf_quant_from_filename_is_4bit() {
        // Q4_K_M w nazwie pliku -> etykieta int4 -> ~4-bit footprint per param.
        let label = detect_quant_from_name("qwen2.5-0.5b-instruct-q4_k_m.gguf")
            .expect("Q4_K_M powinno dac etykiete");
        assert_eq!(label, "int4");
        let bytes = quant_label_to_bytes(&label).expect("int4 ma znana szerokosc");
        assert!(
            (0.5..=0.6).contains(&bytes),
            "int4 powinno byc ~4-bit (0.5-0.5625 B/param), got {bytes}"
        );
    }

    #[test]
    fn weights_override_drives_estimate_weights() {
        // Override wag (GGUF) musi nadpisac model_weights_gb niezaleznie od params.
        let m = qwen_05b();
        let exact_bytes: u64 = 491_000_000; // ~0.49 GB skwantyzowanych wag
        let input = VramEstimateInput {
            gpu_count: 1,
            gpu_memory_gb_each: 24.0,
            weights_bytes_override: Some(exact_bytes),
            ..Default::default()
        };
        let est = estimate_vllm_vram(&m, &input);
        let expected_gb = exact_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
        assert!(
            (est.model_weights_gb - expected_gb).abs() < 0.001,
            "model_weights_gb {} powinno = override {}",
            est.model_weights_gb,
            expected_gb
        );
    }

    /// Qwen2.5-32B (dense): hidden 5120, 40 heads, 8 kv_heads, 64 layers, head_dim
    /// 128, vocab 152064, intermediate 27648, bf16, ~32.5B params.
    fn qwen25_32b() -> ModelSpec {
        ModelSpec {
            model_type: "qwen2".into(),
            architectures: vec!["Qwen2ForCausalLM".into()],
            dtype: "bfloat16".into(),
            hidden_size: 5120,
            num_attention_heads: 40,
            num_key_value_heads: 8,
            num_hidden_layers: 64,
            vocab_size: 152064,
            head_dim: 128,
            intermediate_size: 27648,
            max_position_embeddings: 131072,
            num_parameters: 32_500_000_000,
            ..Default::default()
        }
    }

    #[test]
    fn qwen25_32b_tp2_pool_model_fits_not_inflated() {
        // Rdzeniowy fix KRYT-1: model puli zamiast kv = ctx*seqs. Przed fixem ten
        // sam config raportowal per_gpu_gb ~1062 GB (false OOM). Teraz: staly
        // footprint + pula KV mieszczaca setki tysiecy tokenow.
        let m = qwen25_32b();
        let input = VramEstimateInput {
            engine: DeployEngine::Vllm,
            gpu_count: 2,
            gpu_memory_gb_each: 80.0,
            tensor_parallel: 2,
            pipeline_parallel: 1,
            max_model_len: 32768,
            max_num_seqs: 256,
            max_num_batched_tokens: 8192,
            kv_cache_dtype: "auto".into(),
            kv_cache_dtype_v: None,
            gpu_memory_utilization: 0.9,
            activation_overhead_pct: 10.0,
            weights_bytes_override: None,
        };
        let est = estimate_vllm_vram(&m, &input);
        assert!(
            est.fits_per_gpu,
            "Qwen2.5-32B TP=2 na 2×80GB musi fits: {est:?}"
        );
        assert!(
            est.per_gpu_gb < 80.0,
            "per_gpu_gb musi byc < 80 GB (nie 1062): got {}",
            est.per_gpu_gb
        );
        // Pula ~39 GB/GPU → ~320k tokenow przy 128 KiB/token TP-shardowanym.
        assert!(
            (250_000..=340_000).contains(&est.pool_tokens),
            "pool_tokens rzedu ~250k-340k: got {}",
            est.pool_tokens
        );
        assert!(
            (7.0..=11.0).contains(&est.concurrent_full_len_seqs),
            "wspolbieznosc ~7-11 pelnych sekwencji 32k: got {}",
            est.concurrent_full_len_seqs
        );
        // Pula cluster-wide (2 GPU) to ~78 GB — to liczba ktora widzi renderVramCard
        // zamiast dawnych 1062 GB.
        assert!(
            est.kv_pool_gb > 60.0 && est.kv_pool_gb < 90.0,
            "kv_pool_gb cluster-wide ~78 GB: got {}",
            est.kv_pool_gb
        );
    }

    #[test]
    fn kv_tp_shards_clamped_at_kv_heads() {
        // KRYT-4: KV shardsuje sie tylko do min(tp, kv_heads). Llama-70B-like
        // (kv_heads=8). Powyzej TP=8 vLLM replikuje glowy → szerokosc KV per GPU
        // przestaje malec. Porownujemy TP=8 vs TP=16 na duzym klastrze.
        let m = ModelSpec {
            model_type: "llama".into(),
            architectures: vec!["LlamaForCausalLM".into()],
            dtype: "bfloat16".into(),
            hidden_size: 8192,
            num_attention_heads: 64,
            num_key_value_heads: 8,
            num_hidden_layers: 80,
            vocab_size: 128256,
            head_dim: 128,
            intermediate_size: 28672,
            max_position_embeddings: 131072,
            num_parameters: 70_000_000_000,
            ..Default::default()
        };
        let base = VramEstimateInput {
            engine: DeployEngine::Vllm,
            gpu_count: 16,
            gpu_memory_gb_each: 80.0,
            pipeline_parallel: 1,
            max_model_len: 8192,
            max_num_seqs: 256,
            max_num_batched_tokens: 8192,
            kv_cache_dtype: "auto".into(),
            kv_cache_dtype_v: None,
            gpu_memory_utilization: 0.9,
            activation_overhead_pct: 10.0,
            weights_bytes_override: None,
            tensor_parallel: 8,
        };
        let est_tp8 = estimate_vllm_vram(
            &m,
            &VramEstimateInput {
                tensor_parallel: 8,
                ..base.clone()
            },
        );
        let est_tp16 = estimate_vllm_vram(
            &m,
            &VramEstimateInput {
                tensor_parallel: 16,
                ..base
            },
        );
        // Szerokosc KV per token per GPU = kv_per_token_total / (kv_tp_shards*pp).
        // kv_tp_shards clamp na 8 dla TP=8 ORAZ TP=16 → identyczna szerokosc.
        let kv_total = m.kv_bytes_for_ctx(DeployEngine::Vllm, "auto", "auto", 1);
        let width_tp8 = kv_total / 8.0; // min(8,8)
        let width_tp16 = kv_total / 8.0; // min(16,8) clamp
        assert!((width_tp8 - width_tp16).abs() < 1e-9, "clamp na kv_heads=8");
        // pool_tokens przy TP=16 NIE moze byc mniejszy z powodu KV (szerokosc plaska);
        // wagi/GPU sa mniejsze przy TP=16, wiec pula moze byc nawet wieksza.
        assert!(
            est_tp16.pool_tokens >= est_tp8.pool_tokens,
            "TP=16 nie daje wezszej szerokosci KV niz TP=8: tp8={} tp16={}",
            est_tp8.pool_tokens,
            est_tp16.pool_tokens
        );
        // Ostrzezenie o replikacji KV przy TP>kv_heads (informacyjne, nie blad).
        assert!(
            est_tp16
                .warnings
                .iter()
                .any(|w| w.contains("KV replikowane")),
            "TP>kv_heads powinno dac informacyjne ostrzezenie o replikacji: {:?}",
            est_tp16.warnings
        );
    }

    #[test]
    fn max_num_seqs_does_not_change_vllm_memory() {
        // CORE fix: w modelu puli max_num_seqs to cap schedulera, NIE pamiec.
        // Ta sama konfiguracja z seqs=1 vs seqs=256 musi dac identyczny footprint.
        let m = qwen25_32b();
        let base = VramEstimateInput {
            engine: DeployEngine::Vllm,
            gpu_count: 2,
            gpu_memory_gb_each: 80.0,
            tensor_parallel: 2,
            pipeline_parallel: 1,
            max_model_len: 32768,
            max_num_seqs: 1,
            max_num_batched_tokens: 8192,
            kv_cache_dtype: "auto".into(),
            kv_cache_dtype_v: None,
            gpu_memory_utilization: 0.9,
            activation_overhead_pct: 10.0,
            weights_bytes_override: None,
        };
        let est_1 = estimate_vllm_vram(&m, &base);
        let est_256 = estimate_vllm_vram(
            &m,
            &VramEstimateInput {
                max_num_seqs: 256,
                ..base
            },
        );
        assert!(
            (est_1.per_gpu_gb - est_256.per_gpu_gb).abs() < 1e-9,
            "per_gpu_gb nie moze zalezec od max_num_seqs: 1seq={} 256seq={}",
            est_1.per_gpu_gb,
            est_256.per_gpu_gb
        );
        assert!(
            (est_1.kv_pool_gb - est_256.kv_pool_gb).abs() < 1e-9,
            "kv_pool_gb nie moze zalezec od max_num_seqs: 1seq={} 256seq={}",
            est_1.kv_pool_gb,
            est_256.kv_pool_gb
        );
        assert_eq!(
            est_1.fits_per_gpu, est_256.fits_per_gpu,
            "fits_per_gpu nie moze zalezec od max_num_seqs"
        );
        assert_eq!(
            est_1.pool_tokens, est_256.pool_tokens,
            "pool_tokens nie moze zalezec od max_num_seqs"
        );
    }

    #[test]
    fn fits_total_respects_util() {
        // DRUG-1: fits_total musi mnozyc budzet przez util. Config tuz powyzej
        // count*each*util ma fits_total=false, mimo ze count*each (bez util) by go
        // pomiescil. Maly model na 2×24GB: total ~weights+akt+pula+overhead ≈
        // 2*each*util gdy pula wypelnia budzet, wiec total > 2*each*0.9 jest false.
        let m = qwen_05b();
        let input = VramEstimateInput {
            engine: DeployEngine::Vllm,
            gpu_count: 2,
            gpu_memory_gb_each: 24.0,
            tensor_parallel: 1,
            pipeline_parallel: 2,
            max_model_len: 4096,
            max_num_seqs: 256,
            max_num_batched_tokens: 8192,
            kv_cache_dtype: "auto".into(),
            kv_cache_dtype_v: None,
            gpu_memory_utilization: 0.9,
            activation_overhead_pct: 10.0,
            weights_bytes_override: None,
        };
        let est = estimate_vllm_vram(&m, &input);
        // total wypelnia pule do util*VRAM, wiec lezy tuz powyzej count*each*util
        // (przez overhead 0.5) → fits_total musi byc false (z util), choc total
        // < count*each (bez util).
        let raw_total = input.gpu_memory_gb_each * input.gpu_count as f64;
        let util_total = raw_total * input.gpu_memory_utilization;
        assert!(
            est.total_gb > util_total,
            "total {} powinno przekroczyc budzet z util {}",
            est.total_gb,
            util_total
        );
        assert!(
            est.total_gb <= raw_total,
            "total {} powinno miescic sie w surowym VRAM {}",
            est.total_gb,
            raw_total
        );
        assert!(
            !est.fits_total,
            "fits_total z util musi byc false gdy total > count*each*util: {est:?}"
        );
    }

    #[test]
    fn auto_fit_vllm_default_uses_server_concurrency() {
        // Bez lockow vLLM powinien zwrocic seqs serwerowe (>= 2) i sensowny ctx.
        let m = qwen25_32b();
        let fit = auto_fit_config(
            &m,
            &AutoFitRequest {
                engine: DeployEngine::Vllm,
                gpu_count: 2,
                gpu_memory_gb_each: 80.0,
                kv_cache_dtype: "auto".into(),
                kv_cache_dtype_v: None,
                gpu_memory_utilization: 0.9,
                requested_max_model_len: None,
                requested_max_num_seqs: None,
                requested_tensor_parallel: None,
                requested_pipeline_parallel: None,
                lock_max_model_len: false,
                lock_max_num_seqs: false,
                lock_tensor_parallel: false,
                weights_bytes_override: None,
            },
        );
        assert!(fit.error.is_none(), "Powinno znalezc fit: {:?}", fit.error);
        assert!(
            fit.applied.max_num_seqs >= 2,
            "default seqs serwerowe (>=2, nie 1): got {}",
            fit.applied.max_num_seqs
        );
        assert!(
            fit.applied.max_model_len >= 8192,
            "ctx sensowny (>= 8k): got {}",
            fit.applied.max_model_len
        );
        let est = estimate_vllm_vram(&m, &fit.applied);
        assert!(est.fits_per_gpu, "Po auto-fit musi fits: {est:?}");
        // Rekomendacja nie przekracza osiagalnej wspolbieznosci puli.
        assert!(
            fit.applied.max_num_seqs <= (est.pool_tokens / fit.applied.max_model_len).max(1),
            "seqs {} > floor(pool {}/ctx {})",
            fit.applied.max_num_seqs,
            est.pool_tokens,
            fit.applied.max_model_len
        );
    }

    #[test]
    fn max_concurrent_seqs_reports_pool_concurrency() {
        // Po fixie KRYT-2: dla vLLM max_concurrent_seqs_for_budget czyta osiagalna
        // wspolbieznosc z puli (concurrent_full_len_seqs), nie binary search po seqs
        // (ktory zwracalby bez sensu hi, bo fit nie zalezy od seqs).
        let m = qwen25_32b();
        let input = VramEstimateInput {
            engine: DeployEngine::Vllm,
            gpu_count: 2,
            gpu_memory_gb_each: 80.0,
            tensor_parallel: 2,
            pipeline_parallel: 1,
            max_model_len: 32768,
            max_num_seqs: 256,
            max_num_batched_tokens: 8192,
            kv_cache_dtype: "auto".into(),
            kv_cache_dtype_v: None,
            gpu_memory_utilization: 0.9,
            activation_overhead_pct: 10.0,
            weights_bytes_override: None,
        };
        let est = estimate_vllm_vram(&m, &input);
        let conc = max_concurrent_seqs_for_budget(&m, &input);
        assert_eq!(
            conc,
            est.concurrent_full_len_seqs.floor().max(1.0) as u64,
            "max_concurrent = floor(concurrent_full_len_seqs)"
        );
        assert!(
            conc >= 7,
            "Qwen2.5-32B TP=2 puli starcza na >=7 sekwencji 32k: {conc}"
        );
    }

    #[test]
    fn llamacpp_args_emit_total_ctx_and_np_and_ubatch() {
        // KRYT-7: `-c` to CALY kontekst (max_model_len × max_num_seqs), `-np` to
        // liczba slotow. LCPP-UBATCH-B-FLAG: `-ub 512` zamiast `-b 512`.
        let m = qwen25_32b();
        let input = VramEstimateInput {
            engine: DeployEngine::LlamaCpp,
            max_model_len: 8192,
            max_num_seqs: 8,
            ..Default::default()
        };
        let args = build_llamacpp_args_string(&m, &input);
        assert!(args.contains("-c 65536"), "caly ctx = 8192*8: {args}");
        assert!(args.contains("-np 8"), "8 slotow: {args}");
        assert!(args.contains("-ub 512"), "fizyczny ubatch: {args}");
        assert!(
            !args.contains("-b 512"),
            "logiczny -b NIE emitowany: {args}"
        );
    }

    #[test]
    fn llamacpp_separate_kv_emits_both_flags_and_fa() {
        // Osobne K/V (K=q8_0, V=q4_0) -> obie flagi + -fa (kwantyzowane V wymaga FA).
        let m = qwen25_32b();
        let input = VramEstimateInput {
            engine: DeployEngine::LlamaCpp,
            max_model_len: 4096,
            max_num_seqs: 1,
            kv_cache_dtype: "q8_0".into(),
            kv_cache_dtype_v: Some("q4_0".into()),
            ..Default::default()
        };
        let args = build_llamacpp_args_string(&m, &input);
        assert!(args.contains("--cache-type-k q8_0"), "K=q8_0: {args}");
        assert!(args.contains("--cache-type-v q4_0"), "V=q4_0: {args}");
        assert!(
            args.contains("-fa"),
            "kwantyzowane V wymaga flash-attn: {args}"
        );

        // f16/auto NIE emituja flagi cache-type ani -fa (domyslne 2.0 B).
        let input_default = VramEstimateInput {
            engine: DeployEngine::LlamaCpp,
            kv_cache_dtype: "auto".into(),
            ..Default::default()
        };
        let args_default = build_llamacpp_args_string(&m, &input_default);
        assert!(
            !args_default.contains("--cache-type") && !args_default.contains("-fa"),
            "domyslne f16 nie emituje cache-type/-fa: {args_default}"
        );

        // fp8 (token vLLM) mapuje na q8_0 dla llama.cpp.
        let input_fp8 = VramEstimateInput {
            engine: DeployEngine::LlamaCpp,
            kv_cache_dtype: "fp8".into(),
            ..Default::default()
        };
        let args_fp8 = build_llamacpp_args_string(&m, &input_fp8);
        assert!(
            args_fp8.contains("--cache-type-k q8_0"),
            "fp8->q8_0: {args_fp8}"
        );
    }

    #[test]
    fn llamacpp_q4_0_kv_is_smaller_than_f16() {
        // KRYT-6/Faza 3: q4_0 KV = 0.5625× f16 przy tym samym n_ctx.
        let m = qwen25_32b();
        let base = VramEstimateInput {
            engine: DeployEngine::LlamaCpp,
            gpu_count: 1,
            gpu_memory_gb_each: 80.0,
            max_model_len: 8192,
            max_num_seqs: 1,
            ..Default::default()
        };
        let f16 = estimate_llamacpp_vram(
            &m,
            &VramEstimateInput {
                kv_cache_dtype: "f16".into(),
                ..base.clone()
            },
        );
        let q4 = estimate_llamacpp_vram(
            &m,
            &VramEstimateInput {
                kv_cache_dtype: "q4_0".into(),
                ..base
            },
        );
        // q4_0 K+V = 2×0.5625 vs f16 2×2.0, wiec ratio = 0.5625/2.0 = 0.28125.
        let ratio = q4.kv_cache_gb / f16.kv_cache_gb;
        assert!(
            (ratio - 0.28125).abs() < 1e-6,
            "q4_0 KV / f16 KV = {ratio} (oczekiwane 0.28125)"
        );
        // Osobne K/V K=q8_0 V=q4_0: srednia (1.0625+0.5625)/2 vs 2.0 dla f16.
        let mixed = estimate_llamacpp_vram(
            &m,
            &VramEstimateInput {
                engine: DeployEngine::LlamaCpp,
                gpu_count: 1,
                gpu_memory_gb_each: 80.0,
                max_model_len: 8192,
                max_num_seqs: 1,
                kv_cache_dtype: "q8_0".into(),
                kv_cache_dtype_v: Some("q4_0".into()),
                ..Default::default()
            },
        );
        let expected_ratio = (1.0625 + 0.5625) / (2.0 + 2.0);
        let mixed_ratio = mixed.kv_cache_gb / f16.kv_cache_gb;
        assert!(
            (mixed_ratio - expected_ratio).abs() < 1e-6,
            "mixed KV / f16 = {mixed_ratio} (oczekiwane {expected_ratio})"
        );
    }

    #[test]
    fn llamacpp_n_ctx_scales_with_seqs() {
        // KRYT-7: n_ctx = max_model_len × seqs, wiec KV rosnie z liczba slotow.
        let m = qwen25_32b();
        let base = VramEstimateInput {
            engine: DeployEngine::LlamaCpp,
            gpu_count: 1,
            gpu_memory_gb_each: 80.0,
            max_model_len: 8192,
            kv_cache_dtype: "f16".into(),
            ..Default::default()
        };
        let one = estimate_llamacpp_vram(
            &m,
            &VramEstimateInput {
                max_num_seqs: 1,
                ..base.clone()
            },
        );
        let eight = estimate_llamacpp_vram(
            &m,
            &VramEstimateInput {
                max_num_seqs: 8,
                ..base
            },
        );
        assert_eq!(one.pool_tokens, 8192, "1 slot: n_ctx = ctx");
        assert_eq!(eight.pool_tokens, 65536, "8 slotow: n_ctx = ctx*8");
        assert!(
            (eight.kv_cache_gb / one.kv_cache_gb - 8.0).abs() < 1e-6,
            "8 slotow daje 8x KV: {} vs {}",
            eight.kv_cache_gb,
            one.kv_cache_gb
        );
    }

    #[test]
    fn llamacpp_row_split_puts_full_kv_on_main_gpu() {
        // KRYT-8: tp=2 (row) -> per_gpu liczone z PELNYM KV na main GPU, nie kv/2.
        // fits = max(main, secondary).
        let m = qwen25_32b();
        let input = VramEstimateInput {
            engine: DeployEngine::LlamaCpp,
            gpu_count: 2,
            gpu_memory_gb_each: 24.0,
            tensor_parallel: 2,
            pipeline_parallel: 1,
            max_model_len: 32768,
            max_num_seqs: 1,
            kv_cache_dtype: "f16".into(),
            weights_bytes_override: Some((20.0 * 1024.0 * 1024.0 * 1024.0) as u64),
            ..Default::default()
        };
        let est = estimate_llamacpp_vram(&m, &input);
        let weights_per_gpu = 20.0 / 2.0;
        let main_gpu = weights_per_gpu
            + est.kv_cache_gb
            + llamacpp_compute_buffer_gb(&m, 1)
            + LLAMACPP_CUDA_CTX_PER_GPU;
        assert!(
            (est.per_gpu_gb - main_gpu).abs() < 0.01,
            "row-split per_gpu = main z pelnym KV: {} vs {}",
            est.per_gpu_gb,
            main_gpu
        );
        // per_gpu z pelnym KV musi byc znacznie wyzsze niz naiwne kv/2.
        let naive_even = weights_per_gpu
            + est.kv_cache_gb / 2.0
            + llamacpp_compute_buffer_gb(&m, 1)
            + LLAMACPP_CUDA_CTX_PER_GPU;
        assert!(
            est.per_gpu_gb > naive_even,
            "row-split nie dzieli KV rowno: {} > {}",
            est.per_gpu_gb,
            naive_even
        );
    }

    #[test]
    fn auto_fit_llamacpp_kv_scales_with_locked_seqs() {
        // KRYT-7 w auto: seqs wplywa na budzet KV (n_ctx = ctx*seqs). Lock ctx +
        // seqs=8 liczy KV dla ctx*8.
        let m = qwen25_32b();
        let req = AutoFitRequest {
            engine: DeployEngine::LlamaCpp,
            gpu_count: 1,
            gpu_memory_gb_each: 80.0,
            kv_cache_dtype: "f16".into(),
            kv_cache_dtype_v: None,
            gpu_memory_utilization: 0.9,
            requested_max_model_len: Some(8192),
            requested_max_num_seqs: Some(8),
            requested_tensor_parallel: None,
            requested_pipeline_parallel: None,
            lock_max_model_len: true,
            lock_max_num_seqs: true,
            lock_tensor_parallel: false,
            weights_bytes_override: Some((16.0 * 1024.0 * 1024.0 * 1024.0) as u64),
        };
        let out = auto_fit_config(&m, &req);
        assert!(
            out.error.is_none(),
            "ctx 8192 × 8 slotow miesci sie w 80 GB: {:?}",
            out.error
        );
        let est = estimate_llamacpp_vram(&m, &out.applied);
        assert_eq!(est.pool_tokens, 8192 * 8, "n_ctx liczone dla ctx*seqs");
    }

    fn mlx_4bit_20gb() -> ModelSpec {
        // ~20 GB w 4-bit (g64 -> 0.5625 B/param) => ~35.5B parametrow.
        ModelSpec {
            model_type: "qwen2".into(),
            architectures: vec!["Qwen2ForCausalLM".into()],
            dtype: "bfloat16".into(),
            hidden_size: 5120,
            num_attention_heads: 40,
            num_key_value_heads: 8,
            num_hidden_layers: 64,
            vocab_size: 152064,
            head_dim: 128,
            intermediate_size: 27648,
            max_position_embeddings: 131072,
            quantization: Some("mlx_4bit_g64".into()),
            num_parameters: 35_500_000_000,
            bytes_per_param_override: Some(0.5625),
            ..Default::default()
        }
    }

    #[test]
    fn mlx_fits_on_64gb_unified_with_kv_pool() {
        // Faza 5: model 4bit ~20 GB na budzecie 64 GB -> fits, pool_tokens > 0.
        let m = mlx_4bit_20gb();
        let input = VramEstimateInput {
            engine: DeployEngine::Mlx,
            gpu_count: 1,
            gpu_memory_gb_each: 64.0,
            tensor_parallel: 1,
            pipeline_parallel: 1,
            max_model_len: 8192,
            max_num_seqs: 1,
            kv_cache_dtype: "none".into(),
            gpu_memory_utilization: 0.9,
            ..Default::default()
        };
        let est = estimate_mlx_vram(&m, &input);
        // 35.5B × 0.5625 B = ~19.97e9 B = ~18.6 GiB.
        assert!(
            est.model_weights_gb > 17.0 && est.model_weights_gb < 20.0,
            "wagi ~18.6 GB (35.5B × 0.5625): {}",
            est.model_weights_gb
        );
        assert!(est.fits_per_gpu, "20 GB na 64 GB unified: {est:?}");
        assert!(est.pool_tokens > 0, "pula KV > 0: {}", est.pool_tokens);
        assert!(
            est.per_gpu_gb == est.total_gb,
            "single device: per_gpu == total"
        );

        // kv4 daje wiekszy pool_tokens niz none (mniej bajtow na token).
        let kv4 = estimate_mlx_vram(
            &m,
            &VramEstimateInput {
                kv_cache_dtype: "kv4".into(),
                ..input.clone()
            },
        );
        assert!(
            kv4.pool_tokens > est.pool_tokens,
            "kv4 ({}) > none ({}) pool_tokens",
            kv4.pool_tokens,
            est.pool_tokens
        );
    }

    #[test]
    fn mlx_ignores_tp_pp_and_warns_when_weights_exceed_budget() {
        let m = mlx_4bit_20gb();
        // TP/PP > 1 ignorowane (single device); ostrzezenie obecne.
        let input = VramEstimateInput {
            engine: DeployEngine::Mlx,
            gpu_count: 1,
            gpu_memory_gb_each: 8.0,
            tensor_parallel: 4,
            pipeline_parallel: 2,
            max_model_len: 4096,
            max_num_seqs: 1,
            kv_cache_dtype: "none".into(),
            gpu_memory_utilization: 0.9,
            ..Default::default()
        };
        let est = estimate_mlx_vram(&m, &input);
        assert!(!est.fits_per_gpu, "20 GB nie miesci sie w 8 GB budzecie");
        assert!(
            est.warnings.iter().any(|w| w.contains("Same wagi")),
            "ostrzezenie o wagach > budzet: {:?}",
            est.warnings
        );
        assert!(
            est.warnings.iter().any(|w| w.contains("unified memory")),
            "ostrzezenie o ignorowaniu TP/PP: {:?}",
            est.warnings
        );
    }

    #[test]
    fn mlx_kv_bytes_per_element_table() {
        assert_eq!(kv_bytes_per_element(DeployEngine::Mlx, "none"), Some(2.0));
        assert_eq!(kv_bytes_per_element(DeployEngine::Mlx, "f16"), Some(2.0));
        assert_eq!(kv_bytes_per_element(DeployEngine::Mlx, "kv8"), Some(1.0625));
        assert_eq!(kv_bytes_per_element(DeployEngine::Mlx, "kv4"), Some(0.5625));
        assert_eq!(kv_bytes_per_element(DeployEngine::Mlx, "bogus"), None);
    }

    #[test]
    fn mlx_weight_bytes_group_size_aware() {
        // 4-bit g64 = 0.5625, g32 = 0.625, 8-bit g64 = 1.0625.
        assert!((mlx_weight_bytes(4, 64) - 0.5625).abs() < 1e-9);
        assert!((mlx_weight_bytes(4, 32) - 0.625).abs() < 1e-9);
        assert!((mlx_weight_bytes(8, 64) - 1.0625).abs() < 1e-9);
    }

    #[test]
    fn parse_mlx_top_level_quantization_sets_group_size_bytes() {
        // MLX-community: top-level `quantization: {bits, group_size}` (NIE
        // quantization_config). g32 4-bit -> 0.625 B/param, nie 0.5625.
        let json: serde_json::Value = serde_json::from_str(
            r#"{
            "model_type": "qwen2",
            "hidden_size": 5120,
            "num_attention_heads": 40,
            "num_key_value_heads": 8,
            "num_hidden_layers": 64,
            "vocab_size": 152064,
            "head_dim": 128,
            "intermediate_size": 27648,
            "quantization": {"group_size": 32, "bits": 4}
        }"#,
        )
        .unwrap();
        let spec = parse_hf_config(&json, "mlx-community/Qwen2.5-32B-4bit").unwrap();
        assert!(
            (spec.bytes_per_param() - 0.625).abs() < 1e-9,
            "g32 4-bit = 0.625 (nie 0.5625): {}",
            spec.bytes_per_param()
        );

        // g64 4-bit = 0.5625.
        let json64: serde_json::Value = serde_json::from_str(
            r#"{"hidden_size": 5120, "quantization": {"group_size": 64, "bits": 4}}"#,
        )
        .unwrap();
        let spec64 = parse_hf_config(&json64, "mlx-community/foo-4bit").unwrap();
        assert!((spec64.bytes_per_param() - 0.5625).abs() < 1e-9);
    }

    #[test]
    fn parse_gguf_header_gemma4_swa_aggregates_and_size_label() {
        // Synthetic gemma4 header mirroring the real
        // google/gemma-4-31b-it-qat-q4_0-gguf metadata: per-layer kv heads
        // [16,16,16,16,16,4]×10, SWA pattern [1,1,1,1,1,0]×10 (every 6th layer
        // global), separate _swa K/V widths and size_label "31B".
        let mut kv_heads: Vec<u32> = Vec::new();
        let mut swa_pattern: Vec<u32> = Vec::new();
        for _ in 0..10 {
            kv_heads.extend_from_slice(&[16, 16, 16, 16, 16, 4]);
            swa_pattern.extend_from_slice(&[1, 1, 1, 1, 1, 0]);
        }

        let mut buf = Vec::new();
        buf.extend_from_slice(b"GGUF");
        buf.extend_from_slice(&3u32.to_le_bytes()); // version 3
        buf.extend_from_slice(&0u64.to_le_bytes()); // tensor_count
        buf.extend_from_slice(&15u64.to_le_bytes()); // metadata_kv_count

        gguf_kv_string(&mut buf, "general.architecture", "gemma4");
        gguf_kv_string(&mut buf, "general.size_label", "31B");
        gguf_kv_u32(&mut buf, "gemma4.block_count", 60);
        gguf_kv_u32(&mut buf, "gemma4.embedding_length", 5376);
        gguf_kv_u32(&mut buf, "gemma4.attention.head_count", 32);
        gguf_kv_u32_array(&mut buf, "gemma4.attention.head_count_kv", &kv_heads);
        gguf_kv_u32(&mut buf, "gemma4.context_length", 262144);
        gguf_kv_u32(&mut buf, "gemma4.feed_forward_length", 21504);
        gguf_kv_u32(&mut buf, "gemma4.attention.key_length", 512);
        gguf_kv_u32(&mut buf, "gemma4.attention.value_length", 512);
        gguf_kv_u32(&mut buf, "gemma4.attention.key_length_swa", 256);
        gguf_kv_u32(&mut buf, "gemma4.attention.value_length_swa", 256);
        gguf_kv_u32(&mut buf, "gemma4.attention.sliding_window", 1024);
        gguf_kv_u32_array(
            &mut buf,
            "gemma4.attention.sliding_window_pattern",
            &swa_pattern,
        );
        gguf_kv_string_array(&mut buf, "tokenizer.ggml.tokens", &["<pad>", "a", "b"]);

        let spec = parse_gguf_header(&buf, "gemma-4-31b-it-qat-q4_0.gguf")
            .expect("parser GGUF powinien przejsc");

        assert_eq!(spec.model_type, "gemma4");
        assert_eq!(spec.num_hidden_layers, 60);
        assert_eq!(spec.hidden_size, 5376);
        assert_eq!(spec.num_attention_heads, 32);
        // Scalar field carries the max of the per-layer array.
        assert_eq!(spec.num_key_value_heads, 16);
        assert_eq!(spec.sliding_window, 1024);
        // Global: 10 layers × 4 kv heads × 512; SWA: 50 layers × 16 × 256.
        assert_eq!(spec.kv_k_elems_global, 20480);
        assert_eq!(spec.kv_v_elems_global, 20480);
        assert_eq!(spec.kv_k_elems_swa, 204800);
        assert_eq!(spec.kv_v_elems_swa, 204800);
        // size_label "31B" -> 31e9 params (the dimensional heuristic gave 37.7B).
        assert_eq!(spec.num_parameters, 31_000_000_000);
        assert_eq!(spec.quantization.as_deref(), Some("int4"));
    }

    #[test]
    fn parse_gguf_header_scalar_swa_pattern_every_nth_global() {
        // gemma3-style: scalar sliding_window_pattern=6 means every 6th layer is
        // global, the rest SWA; uniform kv heads.
        let mut buf = Vec::new();
        buf.extend_from_slice(b"GGUF");
        buf.extend_from_slice(&3u32.to_le_bytes());
        buf.extend_from_slice(&0u64.to_le_bytes());
        buf.extend_from_slice(&11u64.to_le_bytes());

        gguf_kv_string(&mut buf, "general.architecture", "gemma3");
        gguf_kv_u32(&mut buf, "gemma3.block_count", 12);
        gguf_kv_u32(&mut buf, "gemma3.embedding_length", 2048);
        gguf_kv_u32(&mut buf, "gemma3.attention.head_count", 8);
        gguf_kv_u32(&mut buf, "gemma3.attention.head_count_kv", 4);
        gguf_kv_u32(&mut buf, "gemma3.context_length", 32768);
        gguf_kv_u32(&mut buf, "gemma3.feed_forward_length", 8192);
        gguf_kv_u32(&mut buf, "gemma3.attention.key_length", 256);
        gguf_kv_u32(&mut buf, "gemma3.attention.sliding_window", 512);
        gguf_kv_u32(&mut buf, "gemma3.attention.sliding_window_pattern", 6);
        gguf_kv_u32(&mut buf, "gemma3.vocab_size", 1000);

        let spec = parse_gguf_header(&buf, "gemma3-x-q4_0.gguf").unwrap();
        assert_eq!(spec.sliding_window, 512);
        // 12 layers, global at i+1 ∈ {6, 12} -> 2 global, 10 SWA. value_length
        // fallback -> key_length; _swa fallback -> non-swa.
        assert_eq!(spec.kv_k_elems_global, 2 * 4 * 256);
        assert_eq!(spec.kv_v_elems_global, 2 * 4 * 256);
        assert_eq!(spec.kv_k_elems_swa, 10 * 4 * 256);
        assert_eq!(spec.kv_v_elems_swa, 10 * 4 * 256);
    }

    #[test]
    fn parse_hf_config_reads_sliding_window_layout() {
        // layer_types array decides the global/SWA split.
        let json: serde_json::Value = serde_json::from_str(
            r#"{
            "model_type": "gemma3_text",
            "hidden_size": 2048,
            "num_attention_heads": 8,
            "num_key_value_heads": 4,
            "num_hidden_layers": 4,
            "head_dim": 256,
            "vocab_size": 262144,
            "intermediate_size": 8192,
            "sliding_window": 512,
            "layer_types": ["sliding_attention", "sliding_attention", "full_attention", "sliding_attention"]
        }"#,
        )
        .unwrap();
        let spec = parse_hf_config(&json, "google/gemma-3-x").unwrap();
        assert_eq!(spec.sliding_window, 512);
        assert_eq!(spec.kv_k_elems_global, 4 * 256);
        assert_eq!(spec.kv_k_elems_swa, 3 * 4 * 256);
        assert_eq!(spec.kv_v_elems_swa, 3 * 4 * 256);

        // sliding_window_pattern=2: every 2nd layer global.
        let json_pattern: serde_json::Value = serde_json::from_str(
            r#"{
            "hidden_size": 2048,
            "num_attention_heads": 8,
            "num_key_value_heads": 4,
            "num_hidden_layers": 4,
            "head_dim": 256,
            "sliding_window": 512,
            "sliding_window_pattern": 2
        }"#,
        )
        .unwrap();
        let spec_p = parse_hf_config(&json_pattern, "google/gemma-3-y").unwrap();
        assert_eq!(spec_p.kv_k_elems_global, 2 * 4 * 256);
        assert_eq!(spec_p.kv_k_elems_swa, 2 * 4 * 256);

        // use_sliding_window=false neutralizes a declared window (Qwen2-style).
        let json_off: serde_json::Value = serde_json::from_str(
            r#"{
            "hidden_size": 2048,
            "num_attention_heads": 8,
            "num_key_value_heads": 4,
            "num_hidden_layers": 4,
            "head_dim": 256,
            "sliding_window": 4096,
            "use_sliding_window": false
        }"#,
        )
        .unwrap();
        let spec_off = parse_hf_config(&json_off, "qwen/qwen2-x").unwrap();
        assert_eq!(spec_off.sliding_window, 0);
        assert_eq!(spec_off.kv_k_elems_swa, 0);
    }

    #[test]
    fn parse_hf_config_gemma2_alternates_swa_without_layout() {
        // Real gemma2 configs declare sliding_window but no layer_types and no
        // sliding_window_pattern; the architecture alternates SWA (even layers)
        // and global (odd layers).
        let json: serde_json::Value = serde_json::from_str(
            r#"{
            "model_type": "gemma2",
            "hidden_size": 4608,
            "num_attention_heads": 32,
            "num_key_value_heads": 16,
            "num_hidden_layers": 46,
            "head_dim": 128,
            "sliding_window": 4096
        }"#,
        )
        .unwrap();
        let spec = parse_hf_config(&json, "google/gemma-2-27b-it").unwrap();
        assert_eq!(spec.sliding_window, 4096);
        // 46 layers alternate: 23 SWA (even indices) + 23 global (odd).
        assert_eq!(spec.kv_k_elems_swa, 23 * 16 * 128);
        assert_eq!(spec.kv_v_elems_swa, 23 * 16 * 128);
        assert_eq!(spec.kv_k_elems_global, 23 * 16 * 128);
        assert_eq!(spec.kv_v_elems_global, 23 * 16 * 128);
    }

    #[test]
    fn parse_hf_config_unknown_arch_window_without_layout_is_all_global() {
        // Unknown architecture with a declared window but no layer layout:
        // assume all layers global (overcount is safe, undercount OOMs).
        let json: serde_json::Value = serde_json::from_str(
            r#"{
            "model_type": "somefuturearch",
            "hidden_size": 2048,
            "num_attention_heads": 8,
            "num_key_value_heads": 4,
            "num_hidden_layers": 4,
            "head_dim": 256,
            "sliding_window": 2048
        }"#,
        )
        .unwrap();
        let spec = parse_hf_config(&json, "acme/future-7b").unwrap();
        assert_eq!(spec.sliding_window, 2048);
        assert_eq!(spec.kv_k_elems_swa, 0);
        assert_eq!(spec.kv_v_elems_swa, 0);
        assert_eq!(spec.kv_k_elems_global, 4 * 4 * 256);
        assert_eq!(spec.kv_v_elems_global, 4 * 4 * 256);
    }

    #[test]
    fn parse_gguf_header_gemma2_alternates_swa_without_pattern() {
        // gemma2 GGUF files declare attention.sliding_window but no pattern;
        // llama.cpp hardcodes even=SWA / odd=global alternation.
        let mut buf = Vec::new();
        buf.extend_from_slice(b"GGUF");
        buf.extend_from_slice(&3u32.to_le_bytes());
        buf.extend_from_slice(&0u64.to_le_bytes());
        buf.extend_from_slice(&10u64.to_le_bytes());

        gguf_kv_string(&mut buf, "general.architecture", "gemma2");
        gguf_kv_u32(&mut buf, "gemma2.block_count", 26);
        gguf_kv_u32(&mut buf, "gemma2.embedding_length", 2304);
        gguf_kv_u32(&mut buf, "gemma2.attention.head_count", 8);
        gguf_kv_u32(&mut buf, "gemma2.attention.head_count_kv", 4);
        gguf_kv_u32(&mut buf, "gemma2.context_length", 8192);
        gguf_kv_u32(&mut buf, "gemma2.feed_forward_length", 9216);
        gguf_kv_u32(&mut buf, "gemma2.attention.key_length", 256);
        gguf_kv_u32(&mut buf, "gemma2.attention.sliding_window", 4096);
        gguf_kv_u32(&mut buf, "gemma2.vocab_size", 256000);

        let spec = parse_gguf_header(&buf, "gemma-2-2b-it-q4_k_m.gguf").unwrap();
        assert_eq!(spec.model_type, "gemma2");
        assert_eq!(spec.sliding_window, 4096);
        // 26 layers alternate: 13 SWA (even indices) + 13 global (odd).
        assert_eq!(spec.kv_k_elems_swa, 13 * 4 * 256);
        assert_eq!(spec.kv_v_elems_swa, 13 * 4 * 256);
        assert_eq!(spec.kv_k_elems_global, 13 * 4 * 256);
        assert_eq!(spec.kv_v_elems_global, 13 * 4 * 256);
    }

    #[test]
    fn auto_fit_llamacpp_gemma4_24gb_full_ctx_first() {
        // User scenario: gemma4-31b Q4 GGUF (16.4 GiB file) on a single 24 GB
        // GPU, f16 KV, no locks. Policy: full context first — seqs stays 1 and
        // ctx gets a LARGE value (SWA keeps 50/60 layers window-capped).
        let m = gemma4_31b();
        let req = AutoFitRequest {
            engine: DeployEngine::LlamaCpp,
            gpu_count: 1,
            gpu_memory_gb_each: 24.0,
            kv_cache_dtype: "f16".into(),
            kv_cache_dtype_v: None,
            gpu_memory_utilization: 0.9,
            requested_max_model_len: None,
            requested_max_num_seqs: None,
            requested_tensor_parallel: None,
            requested_pipeline_parallel: None,
            lock_max_model_len: false,
            lock_max_num_seqs: false,
            lock_tensor_parallel: false,
            weights_bytes_override: Some(17_610_000_000),
        };
        let out = auto_fit_config(&m, &req);
        assert!(out.error.is_none(), "Powinno fits: {:?}", out.error);
        assert_eq!(
            out.applied.max_num_seqs, 1,
            "pelny kontekst przed wspolbieznoscia: seqs=1, got {}",
            out.applied.max_num_seqs
        );
        assert!(
            out.applied.max_model_len >= 16384,
            "SWA-aware KV ma dac duzy ctx (>= 16k) na 24 GB, got {}",
            out.applied.max_model_len
        );
        let est = estimate_llamacpp_vram(&m, &out.applied);
        assert!(est.fits_per_gpu, "Po auto-fit musi fits: {est:?}");
        println!(
            "gemma4-31b/24GB f16: ctx={} seqs={} kv={:.2} GiB per_gpu={:.2} GiB",
            out.applied.max_model_len, out.applied.max_num_seqs, est.kv_cache_gb, est.per_gpu_gb
        );

        // q8_0 KV: narrower cache -> at least as much context.
        let mut req_q8 = req.clone();
        req_q8.kv_cache_dtype = "q8_0".into();
        let out_q8 = auto_fit_config(&m, &req_q8);
        assert!(out_q8.error.is_none());
        assert!(
            out_q8.applied.max_model_len >= out.applied.max_model_len,
            "q8_0 KV nie moze dac mniejszego ctx niz f16: {} vs {}",
            out_q8.applied.max_model_len,
            out.applied.max_model_len
        );
        let est_q8 = estimate_llamacpp_vram(&m, &out_q8.applied);
        assert!(
            est_q8.fits_per_gpu,
            "q8_0 po auto-fit musi fits: {est_q8:?}"
        );
        println!(
            "gemma4-31b/24GB q8_0: ctx={} seqs={} kv={:.2} GiB per_gpu={:.2} GiB",
            out_q8.applied.max_model_len,
            out_q8.applied.max_num_seqs,
            est_q8.kv_cache_gb,
            est_q8.per_gpu_gb
        );
    }

    #[test]
    fn auto_fit_llamacpp_small_model_scales_concurrency_after_full_ctx() {
        // Small model on a big GPU: the full model window fits at seqs=1, so the
        // auto-fit scales concurrency up (2,4,...,64) keeping the full context.
        let m = qwen_05b();
        let out = auto_fit_config(
            &m,
            &AutoFitRequest {
                engine: DeployEngine::LlamaCpp,
                gpu_count: 1,
                gpu_memory_gb_each: 24.0,
                kv_cache_dtype: "auto".into(),
                kv_cache_dtype_v: None,
                gpu_memory_utilization: 0.9,
                requested_max_model_len: None,
                requested_max_num_seqs: None,
                requested_tensor_parallel: None,
                requested_pipeline_parallel: None,
                lock_max_model_len: false,
                lock_max_num_seqs: false,
                lock_tensor_parallel: false,
                weights_bytes_override: None,
            },
        );
        assert!(out.error.is_none(), "Powinno fits: {:?}", out.error);
        assert_eq!(
            out.applied.max_model_len, m.max_position_embeddings,
            "pelne okno modelu (32768), got {}",
            out.applied.max_model_len
        );
        assert!(
            out.applied.max_num_seqs > 1,
            "wspolbieznosc skalowana w gore po pelnym ctx: {}",
            out.applied.max_num_seqs
        );
        assert!(
            out.auto_adjusted.is_empty(),
            "nic nie bylo obnizone vs request: {:?}",
            out.auto_adjusted
        );
        let est = estimate_llamacpp_vram(&m, &out.applied);
        assert!(est.fits_per_gpu, "Po auto-fit musi fits: {est:?}");
    }
}
