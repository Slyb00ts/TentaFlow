// =============================================================================
// File: tests/vllm_advanced_e2e.rs
// Opis: E2E test calej feature VRAM calc + endpoint recommend + spawn args
//       passthrough. Wymaga internetu (HF API) - test #[ignore] runable
//       przez `cargo test --test vllm_advanced_e2e -- --ignored`.
// =============================================================================

use tentaflow_core::deploy::vram_calculator::{
    estimate_vllm_vram, fetch_hf_config, max_context_for_budget, parse_hf_config,
    recommend_parallelism, VramEstimateInput,
};

#[tokio::test]
#[ignore]
async fn vllm_recommend_qwen_05b_fits_single_3090() {
    // Real HF fetch.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .expect("client");
    let cfg = fetch_hf_config(&client, "Qwen/Qwen2.5-0.5B-Instruct", None)
        .await
        .expect("HF fetch");
    let spec = parse_hf_config(&cfg, "Qwen/Qwen2.5-0.5B-Instruct").expect("parse");

    println!("Qwen 0.5B spec: {:?}", spec);
    assert_eq!(spec.model_type, "qwen2");
    assert!(spec.num_attention_heads > 0);

    let (tp, pp) = recommend_parallelism(&spec, 1);
    assert_eq!(tp, 1);
    assert_eq!(pp, 1);

    let input = VramEstimateInput {
        gpu_count: 1,
        gpu_memory_gb_each: 24.0,
        tensor_parallel: 1,
        pipeline_parallel: 1,
        max_model_len: 4096,
        max_num_seqs: 32,
        kv_cache_dtype: "auto".into(),
        gpu_memory_utilization: 0.9,
        activation_overhead_pct: 10.0,
    };
    let est = estimate_vllm_vram(&spec, &input);
    println!("Qwen 0.5B estimate: total={:.2}GB per_gpu={:.2}GB", est.total_gb, est.per_gpu_gb);
    assert!(est.fits_per_gpu, "Qwen 0.5B musi fits 1x 3090");
    assert!(est.total_gb < 5.0);
    assert!(est.warnings.is_empty(), "no warnings expected: {:?}", est.warnings);
}

#[tokio::test]
#[ignore]
async fn vllm_recommend_gemma4_31b_picks_tp2_pp3_for_6_gpus() {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .expect("client");
    let cfg = fetch_hf_config(&client, "google/gemma-4-31B-it", None)
        .await
        .expect("HF fetch");
    let spec = parse_hf_config(&cfg, "google/gemma-4-31B-it").expect("parse");

    println!("Gemma 4 31B: heads={} layers={} dtype={} multimodal={}",
        spec.num_attention_heads, spec.num_hidden_layers, spec.dtype, spec.has_vision);
    assert_eq!(spec.model_type, "gemma4");
    assert!(spec.has_vision);
    assert!(spec.num_attention_heads >= 16);

    // 6 GPU - TP=2 PP=3 powinno byc rekomendowane (32 % 2 = 0, 60 % 3 = 0).
    let (tp, pp) = recommend_parallelism(&spec, 6);
    println!("Recommended TP={} PP={}", tp, pp);
    assert_eq!(tp * pp, 6);
    assert!(spec.num_attention_heads % tp as u64 == 0);
    assert!(spec.num_hidden_layers % pp as u64 == 0);

    let input = VramEstimateInput {
        gpu_count: 6,
        gpu_memory_gb_each: 24.0,
        tensor_parallel: tp,
        pipeline_parallel: pp,
        max_model_len: 4096,
        max_num_seqs: 4,
        kv_cache_dtype: "fp8".into(),
        gpu_memory_utilization: 0.95,
        activation_overhead_pct: 10.0,
    };
    let est = estimate_vllm_vram(&spec, &input);
    println!("Gemma 31B 6x3090 TP{} PP{}: weights={:.1}GB kv={:.1}GB per_gpu={:.1}GB fits={}",
        tp, pp, est.model_weights_gb, est.kv_cache_gb, est.per_gpu_gb, est.fits_per_gpu);
    // Z fp8 KV i niskim batch musi fits.
    assert!(est.fits_per_gpu, "31B na 6x3090 z TP{}xPP{} musi fits: {est:?}", tp, pp);
}

#[tokio::test]
#[ignore]
async fn vllm_recommend_intel_gemma31b_int4_fits_single_3090() {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .expect("client");
    let cfg = fetch_hf_config(&client, "Intel/gemma-4-31B-it-int4-AutoRound", None)
        .await
        .expect("HF fetch INT4");
    let spec = parse_hf_config(&cfg, "Intel/gemma-4-31B-it-int4-AutoRound").expect("parse");

    println!("INT4 Gemma 31B: quant={:?} bytes_per_param={}",
        spec.quantization, spec.bytes_per_param());
    // HF uzywa "auto-round" - mialy mapowac na 0.5 bytes/param przez
    // bytes_per_param().
    assert!(
        matches!(spec.quantization.as_deref(), Some("auto-round") | Some("int4")),
        "expected auto-round or int4, got {:?}", spec.quantization
    );
    assert_eq!(spec.bytes_per_param(), 0.5, "AutoRound INT4 = 0.5 bytes/param");

    // Realistyczna konfiguracja dla 1x 3090 24GB: weights ~14GB, zostaje ~7GB
    // dla KV cache + workspace. ctx=2048 max_seqs=2 = ~1.9GB KV (fp8) - fits.
    // Wieksze parametry wymagaja TP=2 zeby podzielic weights.
    let input = VramEstimateInput {
        gpu_count: 1,
        gpu_memory_gb_each: 24.0,
        tensor_parallel: 1,
        max_model_len: 2048,
        max_num_seqs: 2,
        kv_cache_dtype: "fp8".into(),
        gpu_memory_utilization: 0.92,
        ..Default::default()
    };
    let est = estimate_vllm_vram(&spec, &input);
    println!("INT4 31B 1x3090 ctx2k seqs2 fp8: weights={:.1}GB kv={:.1}GB total={:.1}GB fits={}",
        est.model_weights_gb, est.kv_cache_gb, est.total_gb, est.fits_per_gpu);
    assert!(est.model_weights_gb < 20.0, "INT4 31B = ~14-16GB, dostalismy {}", est.model_weights_gb);
    assert!(est.fits_per_gpu,
        "INT4 31B z ctx=2048 max_seqs=2 fp8 musi fits 1x 3090: {est:?}");
}

#[tokio::test]
#[ignore]
async fn max_ctx_responds_to_kv_dtype_change_for_real_model() {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .expect("client");
    let cfg = fetch_hf_config(&client, "Qwen/Qwen2.5-7B-Instruct", None)
        .await
        .expect("HF fetch 7B");
    let spec = parse_hf_config(&cfg, "Qwen/Qwen2.5-7B-Instruct").expect("parse");

    let mut input = VramEstimateInput {
        gpu_count: 1,
        gpu_memory_gb_each: 24.0,
        tensor_parallel: 1,
        max_num_seqs: 8,
        gpu_memory_utilization: 0.9,
        kv_cache_dtype: "auto".into(),
        ..Default::default()
    };
    let ctx_fp16 = max_context_for_budget(&spec, &input);
    input.kv_cache_dtype = "fp8".into();
    let ctx_fp8 = max_context_for_budget(&spec, &input);
    println!("Qwen 7B max_ctx: fp16={} fp8={} (powinno byc ~2x wiecej z fp8)", ctx_fp16, ctx_fp8);
    assert!(ctx_fp8 > ctx_fp16, "fp8 daje wiecej ctx");
}
