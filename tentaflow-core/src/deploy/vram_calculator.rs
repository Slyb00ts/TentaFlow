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
#[derive(Debug, Clone, Default)]
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
    pub fn bytes_per_param(&self) -> f64 {
        if let Some(q) = &self.quantization {
            return match q.as_str() {
                "int4" | "awq" | "gptq" | "int4_autoround" | "auto_round" => 0.5,
                "int8" | "fp8" => 1.0,
                _ => self.bytes_per_dtype(),
            };
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
pub fn estimate_vllm_vram(model: &ModelSpec, input: &VramEstimateInput) -> VramEstimate {
    let mut warnings: Vec<String> = Vec::new();

    // Weights: pelne parametry (nie active - MoE w vllm ladowane sa wszystkie experty)
    let total_params = model.estimated_params();
    let bytes_per_param = model.bytes_per_param();
    let model_weights_bytes = total_params as f64 * bytes_per_param;
    let model_weights_gb = bytes_to_gib(model_weights_bytes);

    // KV cache: 2 (K + V) × layers × kv_heads × head_dim × max_model_len × max_num_seqs × bytes_kv
    let head_dim = if model.head_dim > 0 {
        model.head_dim
    } else if model.num_attention_heads > 0 {
        model.hidden_size / model.num_attention_heads
    } else {
        128
    };
    let bytes_kv = ModelSpec::bytes_per_kv_element(&input.kv_cache_dtype);
    let kv_per_seq_per_token = 2.0
        * model.num_hidden_layers as f64
        * model.num_key_value_heads.max(1) as f64
        * head_dim as f64
        * bytes_kv;
    let kv_cache_bytes =
        kv_per_seq_per_token * input.max_model_len as f64 * input.max_num_seqs as f64;
    let kv_cache_gb = bytes_to_gib(kv_cache_bytes);

    let raw_total_gb = model_weights_gb + kv_cache_gb;
    let activations_gb = raw_total_gb * (input.activation_overhead_pct / 100.0);
    let overhead_gb = 0.5; // workspace, cuda runtime, allocator overhead

    let total_gb = model_weights_gb + kv_cache_gb + activations_gb + overhead_gb;

    // Per-GPU: TP dzieli weights+KV; PP dzieli layers (KV i weights per stage).
    let tp = input.tensor_parallel.max(1) as f64;
    let pp = input.pipeline_parallel.max(1) as f64;
    let parallel = tp * pp;
    let per_gpu_gb = total_gb / parallel;

    // Walidacja TP/PP vs model heads/layers
    if model.num_attention_heads > 0 && model.num_attention_heads % input.tensor_parallel as u64 != 0
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
    if model.num_hidden_layers > 0
        && model.num_hidden_layers % input.pipeline_parallel as u64 != 0
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
        if heads % (*tp as u64) == 0
            && kv_heads % (*tp as u64) == 0
            && layers % (*pp as u64) == 0
        {
            return (*tp, *pp);
        }
    }
    // Fallback: TP=1, PP=gpu_count - PP dziala dla niemal kazdej liczby
    // layers (jesli nie podzielne, vllm i tak rzuci blad ale jest najmniej
    // restrictive niz TP).
    (1, gpu_count)
}

/// Maksymalny `max_model_len` ktory zmiesci sie przy danej konfiguracji + batch.
/// Iteracyjnie redukuje ctx_len az kv_cache + weights + overhead miesci sie w VRAM.
pub fn max_context_for_budget(
    model: &ModelSpec,
    input: &VramEstimateInput,
) -> u64 {
    let mut lo: u64 = 512;
    let mut hi: u64 = model.max_position_embeddings.max(input.max_model_len).max(8192);
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
pub fn max_concurrent_seqs_for_budget(
    model: &ModelSpec,
    input: &VramEstimateInput,
) -> u64 {
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
pub fn parse_hf_config(
    config_json: &serde_json::Value,
    model_name: &str,
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
        obj.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
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

    // Quantization detection: config field or model name heuristic.
    let quantization = cfg
        .get("quantization_config")
        .and_then(|q| q.as_object())
        .and_then(|q| q.get("quant_method").and_then(|v| v.as_str()).map(String::from))
        .or_else(|| {
            let lower = model_name.to_lowercase();
            if lower.contains("int4") || lower.contains("awq") || lower.contains("autoround") {
                Some("int4".into())
            } else if lower.contains("int8") {
                Some("int8".into())
            } else if lower.contains("fp8") {
                Some("fp8".into())
            } else if lower.contains("gptq") {
                Some("gptq".into())
            } else {
                None
            }
        });

    let has_vision = cfg.contains_key("vision_config")
        || architectures
            .iter()
            .any(|a| a.contains("ConditionalGeneration") || a.contains("Vision"));
    let has_audio = cfg.contains_key("audio_config")
        || cfg.get("audio_token_id").map(|v| !v.is_null()).unwrap_or(false);

    let kv_heads = pick_u64_either("num_key_value_heads");
    let kv_heads_final = if kv_heads > 0 { kv_heads } else { num_attention_heads };

    Ok(ModelSpec {
        model_type: pick_str(cfg, "model_type"),
        architectures,
        dtype: if dtype.is_empty() { "bfloat16".into() } else { dtype },
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
        assert!(est.total_gb < 5.0, "Qwen 0.5B nie powinien zjesc >5GB: {}", est.total_gb);
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
        assert!(!est.fits_per_gpu, "Gemma 31B nie moze sie miescic na 1x 24GB");
        assert!(est.model_weights_gb > 50.0, "31B w bf16 to ~62GB: {}", est.model_weights_gb);
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
        assert!(est.fits_per_gpu, "31B na 6x 3090 z TP*PP=6 musi sie miescic: {est:?}");
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
        let json: serde_json::Value = serde_json::from_str(r#"{
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
        }"#).unwrap();
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
        assert_eq!(spec.quantization.as_deref(), Some("int4"));
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
        assert!(ctx_fp8 > ctx_fp16, "fp8 KV powinno dac wiecej ctx: fp8={ctx_fp8} fp16={ctx_fp16}");
        assert!(ctx_fp8 >= ctx_fp16 * 2 - 512, "fp8 powinno dac ~2x wiecej (lub blisko): fp8={ctx_fp8} fp16={ctx_fp16}");
    }
}
