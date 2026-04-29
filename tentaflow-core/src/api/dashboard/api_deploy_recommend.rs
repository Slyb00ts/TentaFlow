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
    analyze_gpu_compatibility, auto_fit_config, build_vllm_args_string, estimate_vllm_vram,
    fetch_hf_config, max_concurrent_seqs_for_budget, max_context_for_budget,
    parse_hf_config_with_override, AutoFitOutcome, AutoFitRequest, GpuCompatibilityReport,
    VramEstimate, VramEstimateInput,
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

    /// Manualny override etykiety kwantyzacji (np. "nvfp4", "awq", "fp16").
    /// Pomija auto-detekcje z `quantization_config` HF i nazwy repo. Przydatne
    /// gdy backend zle wykrywa lub user wie ze model zostal przekonwertowany
    /// po treningu. "none"/"auto" wylacza override.
    pub quantization_override: Option<String>,

    // Lock flags - gdy true, backend traktuje odpowiadajacy parametr jako fixed
    // (uzytkownik wybral go swiadomie) i auto-zmniejsza POZOSTALE parametry zeby
    // calosc miescila sie w VRAM. Gdy false albo pominiete - parametr moze byc
    // auto-cap'owany przez auto_fit.
    pub lock_max_model_len: Option<bool>,
    pub lock_max_num_seqs: Option<bool>,
    pub lock_tensor_parallel: Option<bool>,
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
    /// Analiza zgodnosci liczby GPU z modelem - lepsze wartosci do wyboru
    /// gdy aktualny setup jest nieoptymalny (np. 5 GPU dla Gemma -> rekomendacja 4 lub 6).
    pub gpu_compatibility: GpuCompatibilityReport,
    /// Konfiguracja faktycznie zastosowana po auto-fit. Moze sie roznic od
    /// `recommended` gdy backend musial obciac parametry zeby fits w VRAM.
    pub applied: AppliedConfig,
    /// Lista nazw parametrow ktore auto-fit zmniejszyl wzgledem tego co user
    /// przyslal (np. `["max_num_seqs", "max_model_len"]`). Pusta gdy zadne
    /// auto-cap nie wystapil.
    pub auto_adjusted: Vec<String>,
    /// True gdy headroom < 5% albo cokolwiek bylo auto-cap'owane. GUI uzywa
    /// tego do pokazania ostrzezenia "konfiguracja na granicy VRAM".
    pub at_limit: bool,
}

/// Wartosci uzyte do faktycznej estymacji (po auto-fit). Jak `RecommendedConfig`
/// ale moze byc po obcieciu.
#[derive(Debug, Serialize)]
pub struct AppliedConfig {
    pub tensor_parallel: u32,
    pub pipeline_parallel: u32,
    pub max_model_len: u64,
    pub max_num_seqs: u64,
    pub kv_cache_dtype: String,
    pub gpu_memory_utilization: f64,
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

    let spec = parse_hf_config_with_override(
        &config_json,
        &req.model,
        req.quantization_override.as_deref(),
    )
    .map_err(|e| anyhow::anyhow!("Parse HF config: {e}"))?;

    let gpu_count = req.gpus.len() as u32;
    let gpu_memory_gb = req.gpus.iter().map(|g| g.memory_gb).fold(f64::INFINITY, f64::min);

    let kv_dtype = req
        .kv_cache_dtype
        .clone()
        .unwrap_or_else(|| "auto".to_string());
    let gpu_mem_util = req.gpu_memory_utilization.unwrap_or(0.9);

    // Wartosci ktore user JAWNIE wyslal w body sa traktowane jako fixed gdy
    // odpowiadajacy lock_* = true. Gdy user lockuje param ktorego nie wyslal,
    // bierzemy wartosc z heurystyki dla auto-fit i tak nie pozwalamy obniżać.
    let lock_ctx = req.lock_max_model_len.unwrap_or(false);
    let lock_seqs = req.lock_max_num_seqs.unwrap_or(false);
    let lock_tp = req.lock_tensor_parallel.unwrap_or(false);

    let fit = auto_fit_config(
        &spec,
        &AutoFitRequest {
            gpu_count,
            gpu_memory_gb_each: gpu_memory_gb,
            kv_cache_dtype: kv_dtype.clone(),
            gpu_memory_utilization: gpu_mem_util,
            requested_max_model_len: req.max_model_len,
            requested_max_num_seqs: req.max_num_seqs,
            requested_tensor_parallel: req.tensor_parallel,
            requested_pipeline_parallel: req.pipeline_parallel,
            lock_max_model_len: lock_ctx,
            lock_max_num_seqs: lock_seqs,
            lock_tensor_parallel: lock_tp,
        },
    );

    let AutoFitOutcome {
        applied: applied_input,
        auto_adjusted,
        at_limit,
        error: fit_error,
    } = fit;

    if let Some(err) = fit_error {
        return Ok((409, serde_json::json!({"error": err}).to_string()));
    }

    let estimate = estimate_vllm_vram(&spec, &applied_input);

    // Max limits dla GUI suwakow - obliczone niezaleznie zeby user wiedzial
    // do jakiej wartosci moze podkrecic. Liczone wzgledem applied (po fit).
    let max_supported_model_len = max_context_for_budget(&spec, &applied_input);
    let max_supported_num_seqs = max_concurrent_seqs_for_budget(&spec, &applied_input);

    let recommended_vllm_args = build_vllm_args_string(&spec, &applied_input);

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

    let mut warnings = estimate.warnings.clone();
    let gpu_compat = analyze_gpu_compatibility(&spec, gpu_count);
    if let Some(w) = &gpu_compat.warning {
        warnings.push(w.clone());
    }

    let response = RecommendResponse {
        model_spec: summary,
        vram_estimate: estimate,
        recommended: RecommendedConfig {
            tensor_parallel: applied_input.tensor_parallel,
            pipeline_parallel: applied_input.pipeline_parallel,
            max_model_len: applied_input.max_model_len,
            max_num_seqs: applied_input.max_num_seqs,
            kv_cache_dtype: applied_input.kv_cache_dtype.clone(),
            gpu_memory_utilization: applied_input.gpu_memory_utilization,
        },
        max_supported_model_len,
        max_supported_num_seqs,
        recommended_vllm_args,
        warnings,
        gpu_compatibility: gpu_compat,
        applied: AppliedConfig {
            tensor_parallel: applied_input.tensor_parallel,
            pipeline_parallel: applied_input.pipeline_parallel,
            max_model_len: applied_input.max_model_len,
            max_num_seqs: applied_input.max_num_seqs,
            kv_cache_dtype: applied_input.kv_cache_dtype,
            gpu_memory_utilization: applied_input.gpu_memory_utilization,
        },
        auto_adjusted,
        at_limit,
    };

    Ok((200, serde_json::to_string(&response)?))
}

#[derive(Debug, Serialize)]
pub struct LimitsResponse {
    pub max_model_len: u64,
    pub max_num_seqs: u64,
    pub max_tensor_parallel: u32,
    pub available_kv_budget_gb: f64,
    /// TP wybrane jako baseline (smart-pick lub user lock) - GUI uzywa do
    /// wyswietlenia "przy TP=4 mozesz miec ctx do X".
    pub tensor_parallel: u32,
    pub pipeline_parallel: u32,
}

/// Handler GET `/api/deploy/vllm/limits`. Query params (URL-encoded):
///   `model` - HF repo id (required)
///   `gpus` - csv `<idx>:<mem_gb>,...` lub jen csv `<mem_gb>` (np. `24,24,24,24`)
///   `lock_max_model_len`, `max_model_len` - opcjonalny lock + wartosc
///   `lock_max_num_seqs`, `max_num_seqs`
///   `lock_tensor_parallel`, `tensor_parallel`
///   `kv_cache_dtype`, `gpu_memory_utilization`, `hf_token`
pub async fn handle_limits(query: &str) -> Result<(u16, String)> {
    let params = parse_query(query);

    let model = params
        .get("model")
        .cloned()
        .unwrap_or_default();
    if model.trim().is_empty() {
        return Ok((400, r#"{"error":"model wymagany"}"#.to_string()));
    }

    let gpus_csv = params.get("gpus").cloned().unwrap_or_default();
    let gpus: Vec<f64> = gpus_csv
        .split(',')
        .filter(|s| !s.is_empty())
        .filter_map(|s| {
            // akceptujemy zarowno "24" jak i "0:24"
            let last = s.rsplit(':').next().unwrap_or(s);
            last.parse::<f64>().ok()
        })
        .collect();
    if gpus.is_empty() {
        return Ok((400, r#"{"error":"gpus wymagane (csv mem_gb)"}"#.to_string()));
    }

    let hf_token = params.get("hf_token").cloned();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| anyhow::anyhow!("reqwest client: {e}"))?;
    let config_json = match fetch_hf_config(&client, &model, hf_token.as_deref()).await {
        Ok(c) => c,
        Err(e) => {
            return Ok((
                404,
                serde_json::json!({"error": format!("HF config fetch: {e}")}).to_string(),
            ));
        }
    };
    let spec = parse_hf_config_with_override(&config_json, &model, None)
        .map_err(|e| anyhow::anyhow!("parse HF: {e}"))?;

    let gpu_count = gpus.len() as u32;
    let gpu_memory_gb = gpus.iter().copied().fold(f64::INFINITY, f64::min);
    let kv_dtype = params
        .get("kv_cache_dtype")
        .cloned()
        .unwrap_or_else(|| "auto".to_string());
    let gpu_mem_util: f64 = params
        .get("gpu_memory_utilization")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.9);
    let lock_ctx = parse_bool(params.get("lock_max_model_len"));
    let lock_seqs = parse_bool(params.get("lock_max_num_seqs"));
    let lock_tp = parse_bool(params.get("lock_tensor_parallel"));
    let req_ctx = params.get("max_model_len").and_then(|s| s.parse().ok());
    let req_seqs = params.get("max_num_seqs").and_then(|s| s.parse().ok());
    let req_tp = params
        .get("tensor_parallel")
        .and_then(|s| s.parse().ok());

    let fit = auto_fit_config(
        &spec,
        &AutoFitRequest {
            gpu_count,
            gpu_memory_gb_each: gpu_memory_gb,
            kv_cache_dtype: kv_dtype,
            gpu_memory_utilization: gpu_mem_util,
            requested_max_model_len: req_ctx,
            requested_max_num_seqs: req_seqs,
            requested_tensor_parallel: req_tp,
            requested_pipeline_parallel: None,
            lock_max_model_len: lock_ctx,
            lock_max_num_seqs: lock_seqs,
            lock_tensor_parallel: lock_tp,
        },
    );

    if let Some(err) = fit.error {
        return Ok((409, serde_json::json!({"error": err}).to_string()));
    }

    let applied = fit.applied;
    let max_model_len = max_context_for_budget(&spec, &applied);
    let max_num_seqs = max_concurrent_seqs_for_budget(&spec, &applied);

    // KV budget: capacity*util - weights/parallel - activations
    let parallel = (applied.tensor_parallel * applied.pipeline_parallel).max(1) as f64;
    let weights_gb = (spec.estimated_params() as f64 * spec.bytes_per_param())
        / (1024.0 * 1024.0 * 1024.0);
    let weights_per_gpu = weights_gb / parallel;
    let activations_per_gpu = 5.0 + weights_per_gpu * 0.10;
    let usable_per_gpu = applied.gpu_memory_gb_each * applied.gpu_memory_utilization;
    let kv_budget = (usable_per_gpu - weights_per_gpu - activations_per_gpu).max(0.0);

    // max_tensor_parallel: najwieksze TP dla ktorego model+min KV miesci sie
    // na pojedynczym GPU (przy danej PP=1) i dzieli heads. Iteruj malejaco.
    let heads = spec.num_attention_heads.max(1);
    let kv_heads = spec.num_key_value_heads.max(1);
    let mut max_tp = 1u32;
    for tp in 1..=gpu_count {
        if gpu_count % tp != 0 {
            continue;
        }
        if heads % (tp as u64) != 0 || kv_heads % (tp as u64) != 0 {
            continue;
        }
        let probe = VramEstimateInput {
            gpu_count,
            gpu_memory_gb_each: gpu_memory_gb,
            tensor_parallel: tp,
            pipeline_parallel: 1,
            max_model_len: 1024,
            max_num_seqs: 1,
            kv_cache_dtype: applied.kv_cache_dtype.clone(),
            gpu_memory_utilization: gpu_mem_util,
            activation_overhead_pct: 10.0,
        };
        if estimate_vllm_vram(&spec, &probe).fits_per_gpu {
            max_tp = tp;
        }
    }

    let resp = LimitsResponse {
        max_model_len,
        max_num_seqs,
        max_tensor_parallel: max_tp,
        available_kv_budget_gb: kv_budget,
        tensor_parallel: applied.tensor_parallel,
        pipeline_parallel: applied.pipeline_parallel,
    };
    Ok((200, serde_json::to_string(&resp)?))
}

/// Prosty parser query stringu (`a=1&b=hello+world`). URL-decode tylko `+` -> ` `
/// i `%XX`. Zwraca map klucz -> wartosc; ostatnie wystapienie wygrywa.
fn parse_query(q: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for pair in q.split('&').filter(|p| !p.is_empty()) {
        let mut it = pair.splitn(2, '=');
        let k = it.next().unwrap_or("");
        let v = it.next().unwrap_or("");
        out.insert(url_decode(k), url_decode(v));
    }
    out
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    out.push(byte);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

fn parse_bool(v: Option<&String>) -> bool {
    matches!(
        v.map(|s| s.as_str()),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

// build_vllm_args_string przeniesione do crate::deploy::vram_calculator
// zeby moglo byc reuse'owane przez runner.rs (auto-defaults dla bundle).
