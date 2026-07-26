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
        weight_host_budget: 0,
weight_spill_dir: None,
        kv_pages: 64,
        ..ModelConfig::default()
    };
    Some(Model::load_gguf(dev, &path, cfg).unwrap())
}

fn prompt() -> Vec<u32> {
    // Fixed token ids; the model only needs a plausible in-vocab prefix.
    vec![
        151644, 872, 198, 105043, 100165, 11319, 151645, 198, 151644, 77091, 198,
    ]
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
    model
        .prefill_chunk_device_logits(&mut seq, &prompt())
        .unwrap();
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
fn device_prefill_logits_match_host_prefill() {
    let Some(mut model) = load() else { return };
    let params = SamplingParams {
        temperature: 0.0,
        ..SamplingParams::default()
    };
    let mut cpu_sampler = Sampler::new(params.clone());
    let mut gpu_sampler = GpuSampler::new(params);
    let mut host_seq = model.new_seq();
    let mut device_seq = model.new_seq();

    let host_logits = model.prefill_chunk(&mut host_seq, &prompt()).unwrap();
    let expected = cpu_sampler.sample(&host_logits, &[]).unwrap();
    model
        .prefill_chunk_device_logits(&mut device_seq, &prompt())
        .unwrap();
    let actual = model.sample_last_logits(&mut gpu_sampler).unwrap();

    model.release_seq(&mut host_seq);
    model.release_seq(&mut device_seq);
    assert_eq!(actual, expected);
}

#[test]
fn device_prefill_profile_is_ready_after_sampling() {
    let Some(mut model) = load() else { return };
    let prompt = prompt();
    let mut sampler = GpuSampler::new(SamplingParams {
        temperature: 0.0,
        ..SamplingParams::default()
    });
    model.prepare_prefill_profiles(prompt.len(), 1).unwrap();
    let mut seq = model.new_seq();

    model
        .prefill_chunk_device_logits(&mut seq, &prompt)
        .unwrap();
    model.sample_last_logits(&mut sampler).unwrap();
    let profile = model
        .take_prefill_profile()
        .unwrap()
        .expect("profil prefill powinien istnieć");

    model.release_seq(&mut seq);
    assert!(profile.target_gpu_ms.is_some_and(|ms| ms > 0.0));
}

#[test]
fn device_prefill_multichunk_matches_host_with_interleaved_sequences() {
    let Some(mut model) = load() else { return };
    let tokens = prompt();
    let chunks = [&tokens[..5], &tokens[5..]];
    let params = SamplingParams {
        temperature: 0.0,
        ..SamplingParams::default()
    };
    let mut expected = [0u32; 2];
    for lane in &mut expected {
        let mut seq = model.new_seq();
        model.prefill_chunk(&mut seq, chunks[0]).unwrap();
        let logits = model.prefill_chunk(&mut seq, chunks[1]).unwrap();
        *lane = Sampler::new(params.clone()).sample(&logits, &[]).unwrap();
        model.release_seq(&mut seq);
    }

    let mut seqs = [model.new_seq(), model.new_seq()];
    model
        .prefill_chunk_device_sync(&mut seqs[0], chunks[0])
        .unwrap();
    model
        .prefill_chunk_device_sync(&mut seqs[1], chunks[0])
        .unwrap();
    let mut actual = [0u32; 2];
    for lane in 0..2 {
        let mut sampler = GpuSampler::new(params.clone());
        model
            .prefill_chunk_device_logits(&mut seqs[lane], chunks[1])
            .unwrap();
        actual[lane] = model.sample_last_logits(&mut sampler).unwrap();
    }

    for seq in &mut seqs {
        model.release_seq(seq);
    }
    assert_eq!(actual, expected);
}

#[test]
fn dense_equal_prefill_b4_b8_b16_matches_serial() {
    let Some(mut model) = load() else { return };
    let mut tokens = Vec::with_capacity(64);
    while tokens.len() < 64 {
        tokens.extend(prompt());
    }
    tokens.truncate(64);
    let params = SamplingParams {
        temperature: 0.0,
        ..SamplingParams::default()
    };
    let mut reference_seq = model.new_seq();
    let logits = model.prefill_chunk(&mut reference_seq, &tokens).unwrap();
    let expected = Sampler::new(params.clone()).sample(&logits, &[]).unwrap();
    model.release_seq(&mut reference_seq);

    for batch in [4usize, 8, 16] {
        assert!(model.dense_prefill_batch_capable(batch, tokens.len()));
        let mut seqs = (0..batch).map(|_| model.new_seq()).collect::<Vec<_>>();
        let mut seq_refs = seqs.iter_mut().collect::<Vec<_>>();
        let token_lanes = (0..batch).map(|_| tokens.as_slice()).collect::<Vec<_>>();
        model
            .prefill_batch_device_logits(&mut seq_refs, &token_lanes)
            .unwrap();
        let mut samplers = (0..batch)
            .map(|_| GpuSampler::new(params.clone()))
            .collect::<Vec<_>>();
        let mut sampler_refs = samplers.iter_mut().collect::<Vec<_>>();
        let actual = model
            .sample_prefill_batch_logits(&mut sampler_refs)
            .unwrap();
        assert_eq!(actual, vec![expected; batch], "B={batch}");
        for seq in &mut seqs {
            model.release_seq(seq);
        }
    }
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
