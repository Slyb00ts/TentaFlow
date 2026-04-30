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
}

impl ModelSpec {
    /// Liczba bajtow per parametr na podstawie dtype/quantization.
    /// Quantization wartosci uwzgledniaja overhead skali/zero-pointow:
    /// - 4-bit (awq/gptq/nvfp4/fp4/mxfp4/bnb_4bit/...): 0.5 + ~0.0625 = 0.5625
    /// - 8-bit (int8/fp8/bnb_8bit): 1.0 + ~0.0625 = 1.0625
    pub fn bytes_per_param(&self) -> f64 {
        if let Some(q) = &self.quantization {
            return quant_label_to_bytes(q).unwrap_or_else(|| self.bytes_per_dtype());
        }
        self.bytes_per_dtype()
    }

    fn bytes_per_dtype(&self) -> f64 {
        match self.dtype.as_str() {
            "bfloat16" | "float16" | "f16" | "bf16" => 2.0,
            "float32" | "f32" => 4.0,
            "int8" | "fp8" => 1.0,
            "int4" => 0.5,
            _ => 2.0, // bf16 default dla nowoczesnych LLM
        }
    }

    /// Bajty per element KV cache. fp8 ekstra opcja - dwukrotna oszczednosc.
    pub fn bytes_per_kv_element(kv_cache_dtype: &str) -> f64 {
        match kv_cache_dtype {
            "fp8" | "fp8_e5m2" | "fp8_e4m3" => 1.0,
            "auto" | "fp16" | "float16" | "bfloat16" | "bf16" => 2.0,
            _ => 2.0,
        }
    }

    /// Wzor liczenia parametrow gdy num_parameters = 0:
    ///   embed: vocab × hidden
    ///   per_layer: 4 × hidden² (qkv+o) + 3 × hidden × intermediate (gate+up+down) + 2×hidden (norms)
    ///   total: embed + layers × per_layer + lm_head(vocab × hidden)
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
        let embed = v * h;
        let per_layer = 4.0 * h * h + 3.0 * h * i + 2.0 * h;
        let lm_head = v * h;
        (embed + l * per_layer + lm_head) as u64
    }

    /// Liczba aktywnych parametrow (MoE: tylko top-K expertow). Default = wszystkie.
    pub fn active_params(&self) -> u64 {
        if self.num_active_parameters > 0 {
            self.num_active_parameters
        } else {
            self.estimated_params()
        }
    }
}

/// Przeklada etykiete quantization (dowolny case, '-' lub '_') na bytes/param.
/// Zwraca None gdy etykieta nieznana - caller fallbackuje do dtype.
/// Wartosci dla 4/8-bit zawieraja overhead group-scales (~6.25%).
pub fn quant_label_to_bytes(label: &str) -> Option<f64> {
    let q = label.to_lowercase().replace('-', "_");
    match q.as_str() {
        // 4-bit: AWQ, GPTQ, AutoRound INT4, bnb-4bit, NVFP4, FP4, MXFP4, w4a16
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
        | "mxfp4"
        | "w4a16"
        | "compressed_tensors_4bit" => Some(0.5625),
        // 8-bit: int8, fp8, bnb-8bit, w8a8, w8a16
        "int8" | "fp8" | "fp8_e4m3" | "fp8_e5m2" | "bnb_8bit" | "bitsandbytes_8bit"
        | "load_in_8bit" | "w8a8" | "w8a16" | "modelopt_fp8" => Some(1.0625),
        // 2-bit (rzadkie ale istnieje)
        "int2" | "w2a16" => Some(0.3125),
        // 16-bit warianty
        "fp16" | "float16" | "bf16" | "bfloat16" | "f16" => Some(2.0),
        "fp32" | "float32" | "f32" => Some(4.0),
        _ => None,
    }
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

/// Konfiguracja runtime do estymacji.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VramEstimateInput {
    pub gpu_count: u32,
    pub gpu_memory_gb_each: f64,
    pub tensor_parallel: u32,
    pub pipeline_parallel: u32,
    pub max_model_len: u64,
    pub max_num_seqs: u64,
    /// `auto` (=fp16), `fp16`, `bfloat16`, `fp8`
    pub kv_cache_dtype: String,
    /// vLLM `--gpu-memory-utilization` (0.0–1.0). Default 0.9.
    pub gpu_memory_utilization: f64,
    /// Activation memory overhead jako % weights+kv. Empirycznie 8-15%.
    pub activation_overhead_pct: f64,
}

impl Default for VramEstimateInput {
    fn default() -> Self {
        Self {
            gpu_count: 1,
            gpu_memory_gb_each: 24.0,
            tensor_parallel: 1,
            pipeline_parallel: 1,
            max_model_len: 8192,
            max_num_seqs: 256,
            kv_cache_dtype: "auto".to_string(),
            gpu_memory_utilization: 0.9,
            activation_overhead_pct: 10.0,
        }
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
    pub warnings: Vec<String>,
}

/// Glowna funkcja kalkulacji.
///
/// Modeluje cluster-wide totals (weights + kv_cache + activations + overhead) oraz
/// realistyczne wartosci per-GPU po podziale przez TP*PP. KV cache uwzglednia GQA
/// (`num_key_value_heads` zamiast pelnej liczby attention heads). Activations
/// modelowane jako workspace ~5 GB na GPU + drobny percent weights/GPU (real vLLM
/// behavior - workspace doesn't shard symmetrically with weights).
pub fn estimate_vllm_vram(model: &ModelSpec, input: &VramEstimateInput) -> VramEstimate {
    let mut warnings: Vec<String> = Vec::new();

    // Weights: pelne parametry (nie active - MoE w vllm ladowane sa wszystkie experty)
    let total_params = model.estimated_params();
    let bytes_per_param = model.bytes_per_param();
    let model_weights_bytes = total_params as f64 * bytes_per_param;
    let model_weights_gb = bytes_to_gib(model_weights_bytes);

    // KV cache GQA: `num_key_value_heads` (NOT num_attention_heads) decyduje o
    // KV memory; `head_dim = hidden / num_attention_heads` chyba ze HF zadeklarowal
    // jawnie. `seq_len` = `max_model_len` z requesta (nie `max_position_embeddings` -
    // to byl stary bug, zawyzal KV ~8x dla modeli z 256k context window).
    // Formula: 2 (K+V) × layers × kv_heads × head_dim × max_model_len × max_num_seqs × bytes_kv
    let head_dim = if model.head_dim > 0 {
        model.head_dim
    } else if model.num_attention_heads > 0 {
        model.hidden_size / model.num_attention_heads
    } else {
        128
    };
    let kv_heads = if model.num_key_value_heads > 0 {
        model.num_key_value_heads
    } else {
        model.num_attention_heads.max(1)
    };
    let bytes_kv = ModelSpec::bytes_per_kv_element(&input.kv_cache_dtype);
    let kv_per_seq_per_token =
        2.0 * model.num_hidden_layers as f64 * kv_heads as f64 * head_dim as f64 * bytes_kv;
    let kv_cache_bytes =
        kv_per_seq_per_token * input.max_model_len as f64 * input.max_num_seqs as f64;
    let kv_cache_gb = bytes_to_gib(kv_cache_bytes);

    // Activations modelowane PER-GPU: real vLLM bierze stale ~5 GB workspace na
    // kazdy worker (CUDA graphs, allocator pools, intermediate buffers) + ok 10%
    // weights/GPU jako transient activations w forwardzie. Ten model jest blizszy
    // realnemu zachowaniu niz jednolite skalowanie sumy.
    let tp = input.tensor_parallel.max(1) as f64;
    let pp = input.pipeline_parallel.max(1) as f64;
    let parallel = tp * pp;

    let weights_per_gpu = model_weights_gb / parallel;
    // KV cache shardsuje sie z TP (per-head split); PP shardsuje warstwy ale KV
    // dla aktywnej warstwy zyje pelny - aproksymujemy podzialem przez tp*pp jak
    // wczesniej (dominujacy efekt: TP).
    let kv_per_gpu = kv_cache_gb / parallel;
    let activation_pct = (input.activation_overhead_pct / 100.0).max(0.0);
    let activations_per_gpu = 5.0 + weights_per_gpu * activation_pct;
    let activations_gb = activations_per_gpu * parallel; // cluster-wide (informational)
    let overhead_gb = 0.5; // CUDA runtime, allocator metadata - per cluster

    let total_gb = model_weights_gb + kv_cache_gb + activations_gb + overhead_gb;
    let per_gpu_gb = weights_per_gpu + kv_per_gpu + activations_per_gpu;

    // Walidacja TP/PP vs model heads/layers
    if model.num_attention_heads > 0
        && model.num_attention_heads % input.tensor_parallel as u64 != 0
    {
        warnings.push(format!(
            "tensor_parallel={} nie dzieli num_attention_heads={} - vLLM odrzuci konfiguracje",
            input.tensor_parallel, model.num_attention_heads
        ));
    }
    if model.num_key_value_heads > 0
        && model.num_key_value_heads % input.tensor_parallel as u64 != 0
    {
        warnings.push(format!(
            "tensor_parallel={} nie dzieli num_key_value_heads={} - vLLM odrzuci konfiguracje",
            input.tensor_parallel, model.num_key_value_heads
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
pub fn recommend_parallelism_vram_aware(
    model: &ModelSpec,
    gpu_count: u32,
    gpu_memory_gb_each: f64,
    gpu_memory_utilization: f64,
) -> (u32, u32) {
    if gpu_count <= 1 {
        return (1, 1);
    }
    let heads = model.num_attention_heads.max(1);
    let kv_heads = model.num_key_value_heads.max(1);
    let layers = model.num_hidden_layers.max(1);

    // Kandydaci czysci (TP*PP=gpu_count) + dzielniki heads/layers. Sortuj po TP
    // rosnaco - preferuj minimalne TP (mniejszy comm overhead) ktore mimo to fits.
    let mut candidates: Vec<(u32, u32)> = (1..=gpu_count)
        .filter(|tp| gpu_count % tp == 0)
        .map(|tp| (tp, gpu_count / tp))
        .filter(|(tp, pp)| {
            heads % (*tp as u64) == 0 && kv_heads % (*tp as u64) == 0 && layers % (*pp as u64) == 0
        })
        .collect();
    candidates.sort_by(|a, b| a.0.cmp(&b.0));

    for (tp, pp) in &candidates {
        let probe = VramEstimateInput {
            gpu_count,
            gpu_memory_gb_each,
            tensor_parallel: *tp,
            pipeline_parallel: *pp,
            max_model_len: 1024,
            max_num_seqs: 1,
            kv_cache_dtype: "auto".into(),
            gpu_memory_utilization,
            activation_overhead_pct: 10.0,
        };
        let est = estimate_vllm_vram(model, &probe);
        if est.fits_per_gpu {
            return (*tp, *pp);
        }
    }

    // Brak konfiguracji ktora miesci weights - zwracamy szeroka partycje;
    // recommend handler zglosi warning OOM uzytkownikowi.
    if let Some(largest_tp) = candidates.last() {
        return *largest_tp;
    }
    recommend_parallelism(model, gpu_count)
}

/// Wejscie auto-fit. `requested_*` to surowe wartosci od usera; `lock_*` mowi
/// czy backend ma je zachowac (true = nie obnizaj, traktuj jako sztywne) czy
/// moze auto-cap'owac do dopasowania VRAM.
#[derive(Debug, Clone)]
pub struct AutoFitRequest {
    pub gpu_count: u32,
    pub gpu_memory_gb_each: f64,
    pub kv_cache_dtype: String,
    pub gpu_memory_utilization: f64,
    pub requested_max_model_len: Option<u64>,
    pub requested_max_num_seqs: Option<u64>,
    pub requested_tensor_parallel: Option<u32>,
    pub requested_pipeline_parallel: Option<u32>,
    pub lock_max_model_len: bool,
    pub lock_max_num_seqs: bool,
    pub lock_tensor_parallel: bool,
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

/// Auto-fit: dopasuj konfiguracje vLLM tak zeby na pewno miescila sie w VRAM.
///
/// Algorytm:
/// 1. TP/PP: gdy locked - bierzemy wartosc usera. Inaczej probujemy
///    `recommend_parallelism_vram_aware` zaczynajac od najmniejszego TP.
/// 2. KV budget per GPU = `capacity * util - weights/parallel - activations/GPU`.
/// 3. Iterujemy lock_*: gdy `max_model_len` locked a `max_num_seqs` not -
///    obliczamy max_num_seqs = budget / (kv_per_seq_token * ctx). I vice versa.
/// 4. Gdy oba locked i nie miesci sie - zwracamy `error`.
/// 5. Gdy nic nie locked - heurystyka defaults: ctx = min(8k, max_position),
///    seqs = 16, oba auto-skalowane do KV budget.
pub fn auto_fit_config(model: &ModelSpec, req: &AutoFitRequest) -> AutoFitOutcome {
    // 1. Wybor TP/PP.
    let (rec_tp, rec_pp) = recommend_parallelism_vram_aware(
        model,
        req.gpu_count,
        req.gpu_memory_gb_each,
        req.gpu_memory_utilization,
    );
    let chosen_tp = if req.lock_tensor_parallel {
        req.requested_tensor_parallel.unwrap_or(rec_tp)
    } else {
        req.requested_tensor_parallel.unwrap_or(rec_tp)
    };
    let chosen_pp = req.requested_pipeline_parallel.unwrap_or(rec_pp);
    let parallel = (chosen_tp.max(1) * chosen_pp.max(1)) as f64;

    // 2. KV budget per GPU.
    let weights_gb = bytes_to_gib(model.estimated_params() as f64 * model.bytes_per_param());
    let weights_per_gpu = weights_gb / parallel;
    let activations_per_gpu = 5.0 + weights_per_gpu * 0.10;
    let usable_per_gpu = req.gpu_memory_gb_each * req.gpu_memory_utilization;
    let kv_budget_gb = (usable_per_gpu - weights_per_gpu - activations_per_gpu).max(0.0);
    let kv_budget_bytes = kv_budget_gb * 1024.0 * 1024.0 * 1024.0;
    let kv_per_seq_token = kv_bytes_per_seq_per_token(model, &req.kv_cache_dtype).max(1.0);

    if kv_budget_gb <= 0.0 {
        return AutoFitOutcome {
            applied: VramEstimateInput {
                gpu_count: req.gpu_count,
                gpu_memory_gb_each: req.gpu_memory_gb_each,
                tensor_parallel: chosen_tp,
                pipeline_parallel: chosen_pp,
                max_model_len: req.requested_max_model_len.unwrap_or(2048),
                max_num_seqs: req.requested_max_num_seqs.unwrap_or(1),
                kv_cache_dtype: req.kv_cache_dtype.clone(),
                gpu_memory_utilization: req.gpu_memory_utilization,
                activation_overhead_pct: 10.0,
            },
            auto_adjusted: Vec::new(),
            at_limit: true,
            error: Some(format!(
                "Wagi modelu ({:.1} GB / GPU) + activations ({:.1} GB) przekraczaja \
                 dostepne {:.1} GB - zwieksz liczbe GPU lub uzyj quantization",
                weights_per_gpu, activations_per_gpu, usable_per_gpu
            )),
        };
    }

    // 3. Heurystyka domyslnych wartosci.
    // Default policy gdy user nic nie lockuje: prefer maksymalny kontekst dla
    // single-user dev setup (long system prompts, RAG, code analysis). Throughput
    // (batchowanie wielu requestow) traktujemy jako wybor manualny - zeby zwiekszyc
    // num_seqs user musi go ustawic explicit albo zlockowac.
    let absolute_ctx_ceiling: u64 = 1_048_576;
    let model_ctx_ceiling = if model.max_position_embeddings > 0 {
        model.max_position_embeddings.min(absolute_ctx_ceiling)
    } else {
        absolute_ctx_ceiling
    };
    let default_seqs: u64 = 1;
    let default_ctx = model_ctx_ceiling.max(2048);

    let req_ctx = req.requested_max_model_len.unwrap_or(default_ctx).max(512);
    let req_seqs = req.requested_max_num_seqs.unwrap_or(default_seqs).max(1);

    // 4. Auto-cap pozostalych params zgodnie z lockami.
    let mut auto_adjusted: Vec<String> = Vec::new();
    let (final_ctx, final_seqs) = match (req.lock_max_model_len, req.lock_max_num_seqs) {
        (true, true) => {
            // Oba locked - sprawdz czy fits.
            let needed = kv_per_seq_token * req_ctx as f64 * req_seqs as f64;
            if needed > kv_budget_bytes {
                return AutoFitOutcome {
                    applied: VramEstimateInput {
                        gpu_count: req.gpu_count,
                        gpu_memory_gb_each: req.gpu_memory_gb_each,
                        tensor_parallel: chosen_tp,
                        pipeline_parallel: chosen_pp,
                        max_model_len: req_ctx,
                        max_num_seqs: req_seqs,
                        kv_cache_dtype: req.kv_cache_dtype.clone(),
                        gpu_memory_utilization: req.gpu_memory_utilization,
                        activation_overhead_pct: 10.0,
                    },
                    auto_adjusted: Vec::new(),
                    at_limit: true,
                    error: Some(format!(
                        "Locked max_model_len={} × max_num_seqs={} wymaga {:.1} GB \
                         KV cache ale budget per GPU to {:.1} GB. Odblokuj jeden \
                         z parametrow albo zwieksz GPU/uzyj fp8 KV.",
                        req_ctx,
                        req_seqs,
                        needed / (1024.0 * 1024.0 * 1024.0),
                        kv_budget_gb
                    )),
                };
            }
            (req_ctx, req_seqs)
        }
        (true, false) => {
            // ctx locked - skaluj seqs.
            let max_seqs = (kv_budget_bytes / (kv_per_seq_token * req_ctx as f64)).floor() as u64;
            let capped = max_seqs.max(1).min(req_seqs);
            if capped < req_seqs {
                auto_adjusted.push("max_num_seqs".into());
            }
            (req_ctx, capped)
        }
        (false, true) => {
            // seqs locked - skaluj ctx.
            let max_ctx = (kv_budget_bytes / (kv_per_seq_token * req_seqs as f64)).floor() as u64;
            let capped = max_ctx.max(512).min(req_ctx);
            if capped < req_ctx {
                auto_adjusted.push("max_model_len".into());
            }
            (capped, req_seqs)
        }
        (false, false) => {
            // Brak lockow. Polityka: trzymaj num_seqs jak najnizej (default 1)
            // a max_model_len pcham do gornego limitu VRAM, capped przez
            // model.max_position_embeddings i absolutny ceiling 1M.
            //
            // Gdy user explicit podal max_num_seqs (req.requested_max_num_seqs)
            // bez locka - traktujemy to jako preferencje throughputu i probujemy
            // zachowac, ale ctx i tak rozszerzamy do max ktory fits.
            let target_seqs = req_seqs.max(1);
            let mut new_seqs = target_seqs;
            let kv_per_seq_full = kv_per_seq_token * req_ctx as f64;
            // Jesli przy zadanym ctx + seqs nie fits - obnizamy seqs (nie ctx).
            // Min 1 seq; jak nie fits przy 1 seq to dopiero kapujemy ctx.
            if kv_per_seq_full * new_seqs as f64 > kv_budget_bytes {
                let max_seqs_at_req_ctx = (kv_budget_bytes / kv_per_seq_full).floor() as u64;
                new_seqs = max_seqs_at_req_ctx.max(1).min(target_seqs);
                if new_seqs < target_seqs {
                    auto_adjusted.push("max_num_seqs".into());
                }
            }
            // Teraz wyznacz max ctx ktory fits przy ustalonym new_seqs. Bierzemy
            // wieksze z dwojga: req_ctx (jak fits) lub max mozliwy z VRAM.
            let max_ctx_from_vram =
                (kv_budget_bytes / (kv_per_seq_token * new_seqs as f64)).floor() as u64;
            let max_ctx_capped = max_ctx_from_vram.min(model_ctx_ceiling);
            let final_ctx_unlocked = req_ctx.max(max_ctx_capped).min(max_ctx_from_vram).max(512);
            // Round down do wielokrotnosci 1024 zeby konfiguracja wygladala czysto.
            let final_ctx_unlocked = (final_ctx_unlocked / 1024).max(1) * 1024;
            if final_ctx_unlocked < req_ctx {
                auto_adjusted.push("max_model_len".into());
            }
            (final_ctx_unlocked, new_seqs)
        }
    };

    // TP auto-adjust: gdy nie-locked i recommend_vram_aware wybral inny niz request.
    if !req.lock_tensor_parallel {
        if let Some(rt) = req.requested_tensor_parallel {
            if rt != chosen_tp {
                // user prosil ale zostal nadpisany - oznaczamy jako adjusted.
                // (Aktualnie chosen_tp == requested gdy podany; ten branch zostawiamy
                // na przyszlosc gdyby logika selekcji zmienila TP automatycznie.)
            }
        } else if rec_tp != 1 {
            // TP wybrany przez heurystyke (nie z request) - to nie jest auto-adjust
            // wzgledem requesta, wiec nie dodajemy do listy.
        }
    }

    // 5. at_limit: cokolwiek dopasowane albo headroom < 5%.
    let used_kv_bytes = kv_per_seq_token * final_ctx as f64 * final_seqs as f64;
    let headroom = (kv_budget_bytes - used_kv_bytes) / kv_budget_bytes.max(1.0);
    let at_limit = !auto_adjusted.is_empty() || headroom < 0.05;

    AutoFitOutcome {
        applied: VramEstimateInput {
            gpu_count: req.gpu_count,
            gpu_memory_gb_each: req.gpu_memory_gb_each,
            tensor_parallel: chosen_tp,
            pipeline_parallel: chosen_pp,
            max_model_len: final_ctx,
            max_num_seqs: final_seqs,
            kv_cache_dtype: req.kv_cache_dtype.clone(),
            gpu_memory_utilization: req.gpu_memory_utilization,
            activation_overhead_pct: 10.0,
        },
        auto_adjusted,
        at_limit,
        error: None,
    }
}

/// KV cache rozmiar (GB) dla 1 sekwencji × 1 tokena dla danej konfiguracji.
/// Wykorzystywane przez auto-fit do obliczenia ile sekwencji × tokenow zmiesci
/// sie w wolnym budzecie KV.
pub fn kv_bytes_per_seq_per_token(model: &ModelSpec, kv_cache_dtype: &str) -> f64 {
    let head_dim = if model.head_dim > 0 {
        model.head_dim
    } else if model.num_attention_heads > 0 {
        model.hidden_size / model.num_attention_heads
    } else {
        128
    };
    let kv_heads = if model.num_key_value_heads > 0 {
        model.num_key_value_heads
    } else {
        model.num_attention_heads.max(1)
    };
    let bytes_kv = ModelSpec::bytes_per_kv_element(kv_cache_dtype);
    2.0 * model.num_hidden_layers as f64 * kv_heads as f64 * head_dim as f64 * bytes_kv
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
        let est = estimate_vllm_vram(model, &try_input);
        if est.fits_per_gpu {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// Maksymalna `max_num_seqs` (rownoleglych zapytan) przy zadanym ctx_len.
pub fn max_concurrent_seqs_for_budget(model: &ModelSpec, input: &VramEstimateInput) -> u64 {
    let mut lo: u64 = 1;
    let mut hi: u64 = 1024;
    while lo + 4 < hi {
        let mid = (lo + hi) / 2;
        let mut try_input = input.clone();
        try_input.max_num_seqs = mid;
        let est = estimate_vllm_vram(model, &try_input);
        if est.fits_per_gpu {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
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
    let quantization = detect_quantization(model_name, config_json, quantization_override);

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

    Ok(ModelSpec {
        model_type: pick_str(cfg, "model_type"),
        architectures,
        dtype: if dtype.is_empty() {
            "bfloat16".into()
        } else {
            dtype
        },
        hidden_size,
        num_attention_heads,
        num_key_value_heads: kv_heads_final,
        num_hidden_layers: pick_u64_either("num_hidden_layers"),
        vocab_size: pick_u64_either("vocab_size"),
        head_dim,
        intermediate_size: pick_u64_either("intermediate_size"),
        max_position_embeddings: pick_u64_either("max_position_embeddings"),
        has_vision,
        has_audio,
        num_parameters: 0,
        num_active_parameters: 0,
        quantization,
    })
}

/// Buduje string `--key val --key val ...` do wpisania w VLLM_ARGS env.
/// Zalacza tylko parametry rozne od vllm defaults zeby nie zasmiecac.
/// Wspoldzielone miedzy api_deploy_recommend (endpoint dla GUI) i runner.rs
/// (auto-defaults dla bundle native gdy user nie ustawil Advanced).
pub fn build_vllm_args_string(spec: &ModelSpec, input: &VramEstimateInput) -> String {
    let mut parts: Vec<String> = Vec::new();

    parts.push("--dtype".into());
    parts.push("auto".into());
    parts.push("--gpu-memory-utilization".into());
    parts.push(format!("{:.2}", input.gpu_memory_utilization));
    parts.push("--max-model-len".into());
    parts.push(input.max_model_len.to_string());
    parts.push("--max-num-seqs".into());
    parts.push(input.max_num_seqs.to_string());
    parts.push("--max-num-batched-tokens".into());
    parts.push(input.max_model_len.max(8192).to_string());

    // chunked prefill TYLKO dla nie-multimodal: vllm dla VL modeli (Gemma 4,
    // Qwen 2.5 VL itp.) Forcuje --disable_chunked_mm_input wewnetrznie i
    // chunked-prefill staje sie no-op. Brak flagi nie szkodzi text-only.
    if !spec.has_vision && !spec.has_audio {
        parts.push("--enable-chunked-prefill".into());
    }

    if input.tensor_parallel > 1 {
        parts.push("--tensor-parallel-size".into());
        parts.push(input.tensor_parallel.to_string());
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
            // NVIDIA Modelopt NVFP4/FP4/MXFP4 - vllm rozpoznaje "modelopt_fp4".
            "nvfp4" | "fp4" | "mxfp4" => {
                parts.push("--quantization".into());
                parts.push("modelopt_fp4".into());
            }
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
            max_position_embeddings: 131072,
            has_vision: true,
            num_parameters: 31_000_000_000,
            ..Default::default()
        }
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
        // ~1 GB weights + KV + 5 GB workspace + 10% activations + 0.5 GB overhead = ~7 GB.
        // Margines do 12 GB chroni przed drobnymi zmianami formuly.
        assert!(
            est.total_gb < 12.0,
            "Qwen 0.5B nie powinien zjesc >12GB: {}",
            est.total_gb
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
    fn max_context_decreases_when_kv_cache_dtype_fp16() {
        // Wieksze KV (Llama-7B-class) zeby fp16 vs fp8 miala znaczenie.
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
            max_position_embeddings: 32768,
            ..Default::default()
        };
        // 80GB GPU (A100/H100) + duzy batch zeby KV byl dominujacy i mial
        // 'oddech' do wzrostu po zmianie z fp16 na fp8.
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
            gpu_count: 4,
            gpu_memory_gb_each: 24.0,
            kv_cache_dtype: "auto".into(),
            gpu_memory_utilization: 0.9,
            requested_max_model_len: Some(32768),
            requested_max_num_seqs: Some(8),
            requested_tensor_parallel: None,
            requested_pipeline_parallel: None,
            lock_max_model_len: false,
            lock_max_num_seqs: false,
            lock_tensor_parallel: false,
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
    fn auto_fit_caps_max_num_seqs_when_ctx_locked() {
        // Gemma 27B z lockedmax_model_len = 131072 - powinno auto-cap max_num_seqs.
        let m = gemma2_27b_like();
        let fit = auto_fit_config(
            &m,
            &AutoFitRequest {
                gpu_count: 4,
                gpu_memory_gb_each: 24.0,
                kv_cache_dtype: "auto".into(),
                gpu_memory_utilization: 0.9,
                requested_max_model_len: Some(131072),
                requested_max_num_seqs: Some(256), // request duzo
                requested_tensor_parallel: None,
                requested_pipeline_parallel: None,
                lock_max_model_len: true,
                lock_max_num_seqs: false,
                lock_tensor_parallel: false,
            },
        );
        assert!(fit.error.is_none(), "Powinno znalezc fit: {:?}", fit.error);
        assert_eq!(fit.applied.max_model_len, 131072, "ctx zachowane (locked)");
        assert!(fit.applied.max_num_seqs < 256, "seqs powinno byc obniżone");
        assert!(
            fit.auto_adjusted.iter().any(|s| s == "max_num_seqs"),
            "auto_adjusted powinno zawierac max_num_seqs: {:?}",
            fit.auto_adjusted
        );
    }

    #[test]
    fn auto_fit_errors_when_both_locked_overflow() {
        let m = gemma2_27b_like();
        let fit = auto_fit_config(
            &m,
            &AutoFitRequest {
                gpu_count: 4,
                gpu_memory_gb_each: 24.0,
                kv_cache_dtype: "auto".into(),
                gpu_memory_utilization: 0.9,
                requested_max_model_len: Some(1_000_000),
                requested_max_num_seqs: Some(256),
                requested_tensor_parallel: None,
                requested_pipeline_parallel: None,
                lock_max_model_len: true,
                lock_max_num_seqs: true,
                lock_tensor_parallel: false,
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
                gpu_count: 2,
                gpu_memory_gb_each: 24.0,
                kv_cache_dtype: "auto".into(),
                gpu_memory_utilization: 0.9,
                requested_max_model_len: Some(32768),
                requested_max_num_seqs: Some(64),
                requested_tensor_parallel: None,
                requested_pipeline_parallel: None,
                lock_max_model_len: false,
                lock_max_num_seqs: false,
                lock_tensor_parallel: false,
            },
        );
        if fit.error.is_none() {
            let est = estimate_vllm_vram(&m, &fit.applied);
            assert!(est.fits_per_gpu, "Po auto-fit musi fits: {est:?}");
        }
    }

    #[test]
    fn auto_default_prefers_max_ctx_with_single_seq() {
        // Gemma2 27B-like, 4x 24GB, brak request + brak lockow -> default policy.
        // Oczekiwanie: max_num_seqs = 1, max_model_len = max mozliwy z VRAM
        // (capped przez model.max_position_embeddings = 32768).
        let m = gemma2_27b_like();
        let fit = auto_fit_config(
            &m,
            &AutoFitRequest {
                gpu_count: 4,
                gpu_memory_gb_each: 24.0,
                kv_cache_dtype: "auto".into(),
                gpu_memory_utilization: 0.9,
                requested_max_model_len: None,
                requested_max_num_seqs: None,
                requested_tensor_parallel: None,
                requested_pipeline_parallel: None,
                lock_max_model_len: false,
                lock_max_num_seqs: false,
                lock_tensor_parallel: false,
            },
        );
        assert!(fit.error.is_none(), "Powinno znalezc fit: {:?}", fit.error);
        assert_eq!(
            fit.applied.max_num_seqs, 1,
            "default num_seqs powinien byc 1, got {}",
            fit.applied.max_num_seqs
        );
        // 27B BF16 na 4×24GB: ~13.5 GB weights/GPU + ~6.5 act = ~20 GB, KV budget
        // ~1.7 GB/GPU dla 1 seq -> ~4-7k ctx. Test sprawdza ze ctx wynosi co najmniej
        // 4k (default policy realnie wyciaga budget) i nie przekracza model maxa.
        assert!(
            fit.applied.max_model_len >= 4096,
            "max_model_len powinien wykorzystac VRAM (>= 4k), got {}",
            fit.applied.max_model_len
        );
        assert!(
            fit.applied.max_model_len <= m.max_position_embeddings,
            "max_model_len {} > model.max_position_embeddings {}",
            fit.applied.max_model_len,
            m.max_position_embeddings
        );
        let est = estimate_vllm_vram(&m, &fit.applied);
        assert!(est.fits_per_gpu, "Per GPU musi fits: {est:?}");
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
                gpu_count: 1,
                gpu_memory_gb_each: 24.0,
                kv_cache_dtype: "auto".into(),
                gpu_memory_utilization: 0.9,
                requested_max_model_len: None,
                requested_max_num_seqs: None,
                requested_tensor_parallel: None,
                requested_pipeline_parallel: None,
                lock_max_model_len: false,
                lock_max_num_seqs: false,
                lock_tensor_parallel: false,
            },
        );
        assert!(fit.error.is_none(), "Powinno fits: {:?}", fit.error);
        assert_eq!(fit.applied.max_num_seqs, 1);
        assert_eq!(
            fit.applied.max_model_len, m.max_position_embeddings,
            "Maly model: ctx == model.max_position_embeddings ({}), got {}",
            m.max_position_embeddings, fit.applied.max_model_len
        );
    }

    #[test]
    fn quant_label_to_bytes_mapping() {
        // 4-bit warianty -> 0.5625 (z overhead skali)
        for q in &[
            "nvfp4",
            "fp4",
            "mxfp4",
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
        // 8-bit -> 1.0625
        for q in &[
            "int8",
            "fp8",
            "fp8-e4m3",
            "bnb_8bit",
            "w8a8",
            "load_in_8bit",
        ] {
            assert_eq!(
                quant_label_to_bytes(q),
                Some(1.0625),
                "8-bit '{}' powinno dac 1.0625",
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
            gpu_count: 4,
            gpu_memory_gb_each: 24.0,
            tensor_parallel: 2,
            pipeline_parallel: 2,
            max_model_len: 32768,
            max_num_seqs: 1,
            kv_cache_dtype: "auto".into(),
            gpu_memory_utilization: 0.9,
            activation_overhead_pct: 10.0,
        };
        let est = estimate_vllm_vram(&m, &input);
        // Wagi powinny byc ~16-18 GB (vs 56.9 GB w bf16).
        assert!(
            est.model_weights_gb >= 14.0 && est.model_weights_gb <= 20.0,
            "NVFP4 30.6B weights ~16 GB, got {}",
            est.model_weights_gb
        );
        assert!(est.fits_per_gpu, "NVFP4 30.6B na 4×24GB musi fits: {est:?}");
        // Per GPU << 24 GB - duzo zapasu.
        assert!(
            est.per_gpu_gb < 18.0,
            "Per GPU powinien byc << 24 GB (komfortowo): got {}",
            est.per_gpu_gb
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
                gpu_count: 4,
                gpu_memory_gb_each: 24.0,
                kv_cache_dtype: "auto".into(),
                gpu_memory_utilization: 0.9,
                requested_max_model_len: None,
                requested_max_num_seqs: None,
                requested_tensor_parallel: None,
                requested_pipeline_parallel: None,
                lock_max_model_len: false,
                lock_max_num_seqs: false,
                lock_tensor_parallel: false,
            },
        );
        assert!(fit.error.is_none(), "Powinno znalezc fit: {:?}", fit.error);
        assert_eq!(fit.applied.max_num_seqs, 1, "default policy: 1 seq");
        // Gemma4 31B ma 60 layers × 16 kv_heads × 256 head_dim → KV ~960 KB/token.
        // Z budgetu ~12 GB/GPU dla 1 seq dostajemy ~12k tokenow ctx. NVFP4 oszczednosc
        // dotyczy WAGS (~17 GB vs 56 GB) ale KV cache zostaje na bf16 i to on tu
        // dominuje. Test wymaga zeby ctx byl realnie wyciagniety (>= 8k - vs PRZED
        // poprawka, kiedy weights byly liczone jak bf16, model w ogole nie fit'owal).
        assert!(
            fit.applied.max_model_len >= 8192,
            "Z malymi wagami (NVFP4) powinno dac sensowny ctx (>= 8k), got {}",
            fit.applied.max_model_len
        );
        assert!(fit.applied.max_model_len <= m.max_position_embeddings);
    }
}
