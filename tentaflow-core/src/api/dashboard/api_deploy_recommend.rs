// =============================================================================
// File: api/dashboard/api_deploy_recommend.rs
// Opis: Endpoint POST /api/deploy/vllm/recommend - inteligentny kalkulator
//       konfiguracji vLLM (TP/PP/ctx_len/max_seqs/kv_dtype) na podstawie
//       wybranego modelu HF i listy GPU. Czyta config.json z HF, oblicza
//       VRAM, zwraca rekomendacje + warnings + max limits dla suwakow GUI.
// =============================================================================

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::deploy::vram_calculator::{
    build_vllm_args_string, estimate_vllm_vram, fetch_hf_config, max_concurrent_seqs_for_budget,
    max_context_for_budget, parse_hf_config, recommend_parallelism, ModelSpec, VramEstimate,
    VramEstimateInput,
};

#[derive(Debug, Deserialize)]
pub struct RecommendRequest {
    /// HF repo id, np. "Qwen/Qwen2.5-0.5B-Instruct" lub "google/gemma-4-31B-it"
    pub model: String,
    /// Lista wybranych GPU - kazdy z `memory_gb`
    pub gpus: Vec<GpuInfo>,
    /// Opcjonalnie: HF token dla gated models
    pub hf_token: Option<String>,

    // Optional overrides - jesli ustawione, nadpisujemy default smart-picks
    pub tensor_parallel: Option<u32>,
    pub pipeline_parallel: Option<u32>,
    pub max_model_len: Option<u64>,
    pub max_num_seqs: Option<u64>,
    pub kv_cache_dtype: Option<String>,
    pub gpu_memory_utilization: Option<f64>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct GpuInfo {
    pub index: u32,
    pub name: String,
    pub memory_gb: f64,
}

#[derive(Debug, Serialize)]
pub struct RecommendResponse {
    /// Wyciagniete z HF config.json (zarchiwizowane fields)
    pub model_spec: ModelSpecSummary,
    /// Aktualna estymacja VRAM dla wybranej konfiguracji
    pub vram_estimate: VramEstimate,
    /// Rekomendacja smart-pick (TP/PP zgodne z heads/layers + dziela GPU)
    pub recommended: RecommendedConfig,
    /// Maksymalny ctx ktory zmiesci sie z aktualnym batch_size + KV dtype
    pub max_supported_model_len: u64,
    /// Maksymalna concurrent seqs przy aktualnym ctx
    pub max_supported_num_seqs: u64,
    /// Gotowy string z VLLM_ARGS do wpisania w deploy
    pub recommended_vllm_args: String,
    /// Warnings (TP nie dzieli heads, model multimodal, OOM etc.)
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ModelSpecSummary {
    pub model_type: String,
    pub architectures: Vec<String>,
    pub dtype: String,
    pub quantization: Option<String>,
    pub hidden_size: u64,
    pub num_attention_heads: u64,
    pub num_key_value_heads: u64,
    pub num_hidden_layers: u64,
    pub max_position_embeddings: u64,
    pub has_vision: bool,
    pub has_audio: bool,
    pub estimated_params_billions: f64,
    pub bytes_per_param: f64,
}

#[derive(Debug, Serialize)]
pub struct RecommendedConfig {
    pub tensor_parallel: u32,
    pub pipeline_parallel: u32,
    pub max_model_len: u64,
    pub max_num_seqs: u64,
    pub kv_cache_dtype: String,
    pub gpu_memory_utilization: f64,
}

/// Handler endpointa. Body: JSON `RecommendRequest`. Response: `RecommendResponse`.
pub async fn handle_recommend(body: &[u8]) -> Result<(u16, String)> {
    let req: RecommendRequest = serde_json::from_slice(body)
        .map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {e}"))?;

    if req.model.trim().is_empty() {
        return Ok((400, r#"{"error":"model wymagany"}"#.to_string()));
    }
    if req.gpus.is_empty() {
        return Ok((400, r#"{"error":"co najmniej jeden GPU wymagany"}"#.to_string()));
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| anyhow::anyhow!("reqwest client: {e}"))?;

    let config_json = match fetch_hf_config(&client, &req.model, req.hf_token.as_deref()).await {
        Ok(c) => c,
        Err(e) => {
            return Ok((
                404,
                serde_json::json!({
                    "error": format!("Nie udalo sie pobrac config.json z HF: {e}. Sprawdz nazwe modelu i ewentualnie HF token (gated repo)."),
                })
                .to_string(),
            ));
        }
    };

    let spec = parse_hf_config(&config_json, &req.model)
        .map_err(|e| anyhow::anyhow!("Parse HF config: {e}"))?;

    let gpu_count = req.gpus.len() as u32;
    let gpu_memory_gb = req.gpus.iter().map(|g| g.memory_gb).fold(f64::INFINITY, f64::min);

    // Smart-pick TP/PP gdy user nie wymusil - wybiera kombinacje zgodna
    // z liczba attention heads i hidden layers modelu.
    let (rec_tp, rec_pp) = recommend_parallelism(&spec, gpu_count);
    let tp = req.tensor_parallel.unwrap_or(rec_tp);
    let pp = req.pipeline_parallel.unwrap_or(rec_pp);

    // Defaulty dla pozostalych pol (uzywane gdy user nie nadpisuje).
    let kv_dtype = req
        .kv_cache_dtype
        .clone()
        .unwrap_or_else(|| "auto".to_string());
    let gpu_mem_util = req.gpu_memory_utilization.unwrap_or(0.9);
    let max_ctx_default = req
        .max_model_len
        .unwrap_or_else(|| spec.max_position_embeddings.min(8192).max(2048));
    let max_seqs_default = req.max_num_seqs.unwrap_or(16);

    let input = VramEstimateInput {
        gpu_count,
        gpu_memory_gb_each: gpu_memory_gb,
        tensor_parallel: tp,
        pipeline_parallel: pp,
        max_model_len: max_ctx_default,
        max_num_seqs: max_seqs_default,
        kv_cache_dtype: kv_dtype.clone(),
        gpu_memory_utilization: gpu_mem_util,
        activation_overhead_pct: 10.0,
    };

    let estimate = estimate_vllm_vram(&spec, &input);

    // Max limits dla GUI suwakow - obliczone niezaleznie zeby user wiedzial
    // do jakiej wartosci moze podkrecic.
    let max_supported_model_len = max_context_for_budget(&spec, &input);
    let max_supported_num_seqs = max_concurrent_seqs_for_budget(&spec, &input);

    let recommended_vllm_args = build_vllm_args_string(&spec, &input);

    let estimated_params = spec.estimated_params() as f64 / 1_000_000_000.0;
    let bytes_per_param = spec.bytes_per_param();

    let summary = ModelSpecSummary {
        model_type: spec.model_type.clone(),
        architectures: spec.architectures.clone(),
        dtype: spec.dtype.clone(),
        quantization: spec.quantization.clone(),
        hidden_size: spec.hidden_size,
        num_attention_heads: spec.num_attention_heads,
        num_key_value_heads: spec.num_key_value_heads,
        num_hidden_layers: spec.num_hidden_layers,
        max_position_embeddings: spec.max_position_embeddings,
        has_vision: spec.has_vision,
        has_audio: spec.has_audio,
        estimated_params_billions: estimated_params,
        bytes_per_param,
    };

    let warnings = estimate.warnings.clone();

    let response = RecommendResponse {
        model_spec: summary,
        vram_estimate: estimate,
        recommended: RecommendedConfig {
            tensor_parallel: tp,
            pipeline_parallel: pp,
            max_model_len: max_ctx_default,
            max_num_seqs: max_seqs_default,
            kv_cache_dtype: kv_dtype,
            gpu_memory_utilization: gpu_mem_util,
        },
        max_supported_model_len,
        max_supported_num_seqs,
        recommended_vllm_args,
        warnings,
    };

    Ok((200, serde_json::to_string(&response)?))
}

// build_vllm_args_string przeniesione do crate::deploy::vram_calculator
// zeby moglo byc reuse'owane przez runner.rs (auto-defaults dla bundle).
