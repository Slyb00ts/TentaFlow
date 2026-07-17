// ===== File: gpu_sampling.rs — on-GPU sampling vs the CPU path on a real model =====
// Loads the qwen3 test GGUF and checks that the GPU sampling path is
// greedy-bit-identical to the CPU logits path and deterministic per seed on
// the stochastic path. Skips cleanly without a CUDA device or the model.

use std::path::PathBuf;
use std::sync::Arc;

use forge_engine::model::{Model, ModelConfig};
use forge_engine::sample::{GpuSampler, Sampler, SamplingParams};
use forge_hal::cuda::{CudaDevice, PoolSizes};
use forge_hal::Device;

const STEPS: usize = 24;

fn model_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-models/gguf/qwen3-0.6b-q8_0.gguf")
}

fn load() -> Option<Model> {
    let path = model_path();
    if !path.is_file() {
        eprintln!("skipping: test model missing at {}", path.display());
        return None;
    }
    let device = match CudaDevice::new(
        0,
        PoolSizes {
            weights: 2 << 30,
            kv_cache: 512 << 20,
            activations: 256 << 20,
            kv_page_size: 256 << 10,
        },
    ) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skipping: no CUDA device: {e}");
            return None;
        }
    };
    let dev: Arc<dyn Device> = device;
    // 64 KV pages (2048 tokens) keep the cache within the test pool.
    let cfg = ModelConfig {
        kv_pages: 64,
        ..ModelConfig::default()
    };
    Some(Model::load_gguf(dev, &path, cfg).unwrap())
}

fn prompt() -> Vec<u32> {
    // Fixed token ids; the model only needs a plausible in-vocab prefix.
    vec![151644, 872, 198, 105043, 100165, 11319, 151645, 198, 151644, 77091, 198]
}

/// Greedy ids via the CPU path: full logits download + host argmax through
/// the CPU sampler (temperature 0).
fn cpu_greedy_ids(model: &mut Model) -> Vec<u32> {
    let params = SamplingParams {
        temperature: 0.0,
        ..SamplingParams::default()
    };
    let mut sampler = Sampler::new(params);
    let mut seq = model.new_seq();
    let mut ids = Vec::new();
    let mut logits = model.prefill_chunk(&mut seq, &prompt()).unwrap();
    let mut next = sampler.sample(&logits, &ids).unwrap();
    ids.push(next);
    while ids.len() < STEPS {
        logits = model.step(&mut seq, next).unwrap();
        next = sampler.sample(&logits, &ids).unwrap();
        ids.push(next);
    }
    model.release_seq(&mut seq);
    ids
}

fn gpu_ids(model: &mut Model, params: SamplingParams) -> Vec<u32> {
    assert!(model.gpu_sampling_supported(&params));
    let mut sampler = GpuSampler::new(params);
    let mut seq = model.new_seq();
    let mut ids: Vec<u32> = Vec::new();
    model.prefill_chunk(&mut seq, &prompt()).unwrap();
    let mut next = model.sample_last_logits(&mut sampler).unwrap();
    ids.push(next);
    while ids.len() < STEPS {
        sampler.note_token(next);
        next = model.step_and_sample(&mut seq, next, &mut sampler).unwrap();
        ids.push(next);
    }
    model.release_seq(&mut seq);
    ids
}

#[test]
fn gpu_paths_match_and_replay() {
    let Some(mut model) = load() else { return };

    // Greedy must be bit-identical to the CPU argmax over downloaded logits.
    let cpu = cpu_greedy_ids(&mut model);
    let gpu = gpu_ids(
        &mut model,
        SamplingParams {
            temperature: 0.0,
            ..SamplingParams::default()
        },
    );
    assert_eq!(cpu, gpu, "greedy GPU sampling diverged from CPU argmax");

    // Greedy with an active repetition penalty must also match the CPU
    // sampler (distinct-token penalty applied on-device).
    let pen = SamplingParams {
        temperature: 0.0,
        repetition_penalty: 1.3,
        ..SamplingParams::default()
    };
    let mut sampler = Sampler::new(pen.clone());
    let mut seq = model.new_seq();
    let mut cpu_pen = Vec::new();
    let mut logits = model.prefill_chunk(&mut seq, &prompt()).unwrap();
    let mut next = sampler.sample(&logits, &cpu_pen).unwrap();
    cpu_pen.push(next);
    while cpu_pen.len() < STEPS {
        logits = model.step(&mut seq, next).unwrap();
        next = sampler.sample(&logits, &cpu_pen).unwrap();
        cpu_pen.push(next);
    }
    model.release_seq(&mut seq);
    let gpu_pen = gpu_ids(&mut model, pen);
    assert_eq!(cpu_pen, gpu_pen, "penalized greedy diverged");

    // The stochastic path replays identically for a fixed seed and varies
    // across seeds.
    let seeded = |seed: u64| SamplingParams {
        temperature: 0.7,
        seed: Some(seed),
        ..SamplingParams::default()
    };
    let a = gpu_ids(&mut model, seeded(42));
    let b = gpu_ids(&mut model, seeded(42));
    assert_eq!(a, b, "fixed-seed GPU sampling must replay identically");
    let c = gpu_ids(&mut model, seeded(43));
    let d = gpu_ids(&mut model, seeded(44));
    assert!(
        a != c || a != d,
        "different seeds should not all produce identical streams"
    );
}
