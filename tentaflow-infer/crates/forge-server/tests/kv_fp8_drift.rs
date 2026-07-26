// ===== File: kv_fp8_drift.rs — GPU integration gate: FP8 KV cache vs f16 (greedy ids + logit drift) =====
// Ignored by default: needs a CUDA GPU and local model files. Run serially —
// each pass provisions its own multi-GiB VRAM pools:
//   cargo test -p forge-server --release --test kv_fp8_drift -- --ignored --nocapture --test-threads=1
// For each available model the test runs prefill + 16 greedy decode steps
// with a f16 KV cache and again with fp8-e4m3, asserts the greedy token ids
// are identical, and reports the max-abs logit drift per step (honest
// number: fp8 quantizes K/V, so logits are NOT bit-equal).

use std::path::Path;
use std::sync::Arc;

use forge_engine::model::ModelConfig;
use forge_hal::cuda::{CudaDevice, PoolSizes};
use forge_hal::Device;
use forge_server::source::{kv_pool_bytes, load_model, read_descriptor};
use forge_types::DType;

const BIELIK_DIR: &str = "/home/critix/repos/rust/TentaFlow/.runtime/models/models--TentaFlow--Bielik-PL-Minitron-7B-NVFP4/snapshots/831550e879fd7d700e3f6d79dffc14373deda3a7";
const STEPS: usize = 16;

fn greedy(logits: &[f32]) -> u32 {
    let mut best = 0usize;
    for (i, &v) in logits.iter().enumerate() {
        if v > logits[best] {
            best = i;
        }
    }
    best as u32
}

fn e4m3_decode(b: u8) -> f32 {
    let sign = if b & 0x80 != 0 { -1.0f32 } else { 1.0 };
    let e = ((b >> 3) & 0x0F) as i32;
    let man = (b & 0x07) as f32;
    if e == 0 {
        sign * man * (1.0 / 512.0)
    } else {
        sign * (1.0 + man / 8.0) * 2.0f32.powi(e - 7)
    }
}

/// Range evidence for the scale-free e4m3 assumption, gathered from the live
/// cache: max |K|/|V| magnitude and saturated-code count over used slots.
#[derive(Default)]
struct Fp8CacheStats {
    max_abs: f32,
    saturated: usize,
    nan_codes: usize,
    values: usize,
}

fn fp8_cache_stats(
    model: &forge_engine::model::Model,
    seq: &forge_engine::kv::SeqKv,
) -> Fp8CacheStats {
    let mut stats = Fp8CacheStats::default();
    let cfg = &model.kv.cfg;
    let page_elems = cfg.n_kv_heads * cfg.page_size * cfg.head_dim;
    let mut page_bytes = vec![0u8; page_elems];
    for l in 0..cfg.n_layers {
        for slab in [&model.kv.k[l], &model.kv.v[l]] {
            for (pi, &page) in seq.pages.iter().enumerate() {
                model
                    .device
                    .read(slab, page as usize * page_elems, &mut page_bytes)
                    .expect("read kv page");
                let filled = (seq.len - pi * cfg.page_size).min(cfg.page_size);
                for h in 0..cfg.n_kv_heads {
                    for slot in 0..filled {
                        let base = (h * cfg.page_size + slot) * cfg.head_dim;
                        for &b in &page_bytes[base..base + cfg.head_dim] {
                            stats.values += 1;
                            match b & 0x7F {
                                0x7E => stats.saturated += 1,
                                0x7F => stats.nan_codes += 1,
                                _ => stats.max_abs = stats.max_abs.max(e4m3_decode(b).abs()),
                            }
                        }
                    }
                }
            }
        }
    }
    if stats.saturated > 0 {
        stats.max_abs = 448.0;
    }
    stats
}

/// Prefill `prompt_ids` (chunked) and decode STEPS greedy tokens; returns
/// (ids, per-step logits, fp8 cache stats when kv_dtype is fp8).
fn run_pass_ids(
    path: &Path,
    prompt_ids: &[u32],
    kv_dtype: DType,
    kv_pages: usize,
) -> (Vec<u32>, Vec<Vec<f32>>, Fp8CacheStats) {
    let kv_page_size = 32;
    let kv_quant = if kv_dtype == DType::F8E4M3 {
        forge_engine::kv::KvQuant::Fp8
    } else {
        forge_engine::kv::KvQuant::F16
    };
    let desc = read_descriptor(path).expect("read model descriptor");
    let device = CudaDevice::new(
        0,
        PoolSizes {
            weights: 8 << 30,
            kv_cache: kv_pool_bytes(&desc, kv_page_size, kv_pages, kv_quant, false)
                .unwrap()
                .max(1 << 30),
            activations: 1 << 30,
            kv_page_size: PoolSizes::DEFAULT_KV_PAGE,
        },
    )
    .expect("cuda device");
    let dev: Arc<dyn Device> = device;
    let mut loaded = load_model(
        dev,
        path,
        ModelConfig {
            weight_spill_dir: None,
            weight_host_budget: 0,
            kv_page_size,
            kv_pages,
            max_seq_len: 4096,
            kv_quant,
            kv_tier: Default::default(),
            prefix_cache: false,
            native_mtp: false,
            nvfp4_gguf_layout: forge_engine::model::Nvfp4GgufLayout::RowMajor36,
            nvfp4_ct_layout: forge_engine::weights::NvFp4CtLayoutPolicy::Auto,
        },
    )
    .expect("load model");
    let model = &mut loaded.model;
    let mut seq = model.new_seq();
    let mut logits = Vec::new();
    for chunk in prompt_ids.chunks(forge_engine::model::MAX_PREFILL_CHUNK) {
        logits = model.prefill_chunk(&mut seq, chunk).expect("prefill");
    }
    let mut ids = Vec::new();
    let mut history = Vec::new();
    for _ in 0..STEPS {
        let next = greedy(&logits);
        history.push(logits);
        ids.push(next);
        logits = model.step(&mut seq, next).expect("decode step");
    }

    let stats = if kv_dtype == DType::F8E4M3 {
        fp8_cache_stats(model, &seq)
    } else {
        Fp8CacheStats::default()
    };
    model.release_seq(&mut seq);
    (ids, history, stats)
}

fn encode_prompt(path: &Path, prompt: &str) -> Vec<u32> {
    let bundle = if path.is_dir() {
        forge_server::source::load_tokenizer_dir(path).expect("load tokenizer")
    } else {
        let gguf = forge_formats::Gguf::open(path).expect("open gguf");
        forge_server::source::load_tokenizer_gguf(&gguf).expect("load tokenizer")
    };
    bundle
        .tokenizer
        .encode(prompt, true)
        .expect("encode prompt")
}

/// `run_pass_ids` over an encoded text prompt.
fn run_pass(
    path: &Path,
    prompt: &str,
    kv_dtype: DType,
) -> (Vec<u32>, Vec<Vec<f32>>, Fp8CacheStats) {
    let prompt_ids = encode_prompt(path, prompt);
    run_pass_ids(path, &prompt_ids, kv_dtype, 256)
}

#[test]
#[ignore = "requires a CUDA GPU and local model files"]
fn fp8_kv_matches_f16_greedy() {
    let models: [(&str, &str); 3] = [
        (BIELIK_DIR, "Stolicą Polski jest"),
        (
            "../../test-models/gguf/qwen3-0.6b-q8_0.gguf",
            "The capital of France is",
        ),
        (
            "../../test-models/gguf/mistral-7b-q4_k_m.gguf",
            "The capital of France is",
        ),
    ];
    let mut ran = 0;
    for (path, prompt) in models {
        let path = Path::new(path);
        if !path.exists() {
            eprintln!("skipping missing model {}", path.display());
            continue;
        }
        let (ids16, hist16, _) = run_pass(path, prompt, DType::F16);
        let (ids8, hist8, stats) = run_pass(path, prompt, DType::F8E4M3);
        let mut max_drift = 0.0f32;
        for (a, b) in hist16.iter().zip(&hist8) {
            for (x, y) in a.iter().zip(b) {
                max_drift = max_drift.max((x - y).abs());
            }
        }
        println!(
            "{}: greedy ids f16 {:?} | fp8 {:?} | max logit drift over {STEPS} steps: {max_drift}",
            path.display(),
            ids16,
            ids8
        );
        println!(
            "  fp8 cache range: max |k/v| = {} over {} values, saturated codes = {}, NaN codes = {}",
            stats.max_abs, stats.values, stats.saturated, stats.nan_codes
        );
        assert_eq!(
            ids16,
            ids8,
            "fp8 KV changed the greedy path for {}",
            path.display()
        );
        // Scale-free e4m3 assumption: post-norm K/V must not saturate.
        assert_eq!(
            stats.saturated,
            0,
            "e4m3 saturation in the KV cache of {}",
            path.display()
        );
        assert_eq!(
            stats.nan_codes,
            0,
            "NaN codes in the KV cache of {}",
            path.display()
        );
        ran += 1;
    }
    assert!(ran > 0, "no test models available");
}

#[test]
#[ignore = "requires a CUDA GPU and local model files"]
fn fp8_kv_range_at_long_context() {
    // The scale-free e4m3 store rides on post-norm K/V staying under ±448.
    // Short prompts already show qwen3 (QK-norm) peaking at 416 — this gate
    // fills 4096 tokens of context and asserts the cache still has zero
    // saturated codes, on both a QK-norm model and a plain one.
    let models: [(&str, &str); 2] = [
        (
            "../../test-models/gguf/qwen3-0.6b-q8_0.gguf",
            "One of the most important cities in the history of Poland is Krakow. ",
        ),
        (
            BIELIK_DIR,
            "Jednym z najważniejszych miast w historii Polski jest Kraków. ",
        ),
    ];
    let mut ran = 0;
    for (path, seed) in models {
        let path = Path::new(path);
        if !path.exists() {
            eprintln!("skipping missing model {}", path.display());
            continue;
        }
        let seed_ids = encode_prompt(path, seed);
        let prompt_ids: Vec<u32> = seed_ids
            .iter()
            .cycle()
            .take(4096 - STEPS)
            .copied()
            .collect();
        let (_, _, stats) = run_pass_ids(path, &prompt_ids, DType::F8E4M3, 4096 / 32);
        println!(
            "{}: fp8 cache range at ~4096 ctx: max |k/v| = {} over {} values, saturated = {}, NaN = {}",
            path.display(),
            stats.max_abs,
            stats.values,
            stats.saturated,
            stats.nan_codes
        );
        assert_eq!(
            stats.saturated,
            0,
            "e4m3 saturation at long context for {}",
            path.display()
        );
        assert_eq!(
            stats.nan_codes,
            0,
            "NaN codes at long context for {}",
            path.display()
        );
        ran += 1;
    }
    assert!(ran > 0, "no test models available");
}
