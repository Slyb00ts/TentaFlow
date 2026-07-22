// ===== File: batched_decode.rs — continuous-batching decode correctness on a real model =====
// Proves the batched forward path: (a) a single-sequence batch matches the
// legacy fused single-seq greedy stream; (b) N identical prompts decoded
// together produce identical streams; (c) N distinct prompts each match their
// own single-sequence greedy generation. Skips cleanly without a CUDA device or
// the qwen3 test GGUF.

use std::path::PathBuf;
use std::sync::Arc;

use forge_engine::model::{Model, ModelConfig};
use forge_engine::sample::{GpuSampler, SamplingParams, SeqSampleParams};
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
            kv_cache: 1 << 30,
            activations: 2 << 30,
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
    let cfg = ModelConfig {
        kv_pages: 256,
        ..ModelConfig::default()
    };
    Some(Model::load_gguf(dev, &path, cfg).unwrap())
}

fn greedy() -> SamplingParams {
    SamplingParams {
        temperature: 0.0,
        ..SamplingParams::default()
    }
}

fn greedy_seq_params() -> SeqSampleParams {
    SeqSampleParams {
        greedy: true,
        k: 1,
        inv_t: 1.0,
        top_p: 1.0,
        min_p: 0.0,
        seed: 0,
        step: 0,
        penalty: 1.0,
        frequency_penalty: 0.0,
        presence_penalty: 0.0,
        penalty_ids: Vec::new(),
        penalty_counts: Vec::new(),
    }
}

/// Legacy single-sequence greedy stream (prefill + fused graphed decode).
fn single_seq_greedy(model: &mut Model, prompt: &[u32], steps: usize) -> Vec<u32> {
    let mut sampler = GpuSampler::new(greedy());
    let mut seq = model.new_seq();
    let mut ids: Vec<u32> = Vec::new();
    model.prefill_chunk(&mut seq, prompt).unwrap();
    let mut next = model.sample_last_logits(&mut sampler).unwrap();
    ids.push(next);
    while ids.len() < steps {
        sampler.note_token(next);
        next = model.step_and_sample(&mut seq, next, &mut sampler).unwrap();
        ids.push(next);
    }
    model.release_seq(&mut seq);
    ids
}

/// Batched greedy decode over `prompts`: prefill each sequence and sample its
/// first token individually, then advance them all through `batched_decode`.
fn batched_greedy(model: &mut Model, prompts: &[Vec<u32>], steps: usize) -> Vec<Vec<u32>> {
    let n = prompts.len();
    let mut seqs: Vec<_> = (0..n).map(|_| model.new_seq()).collect();
    let mut ids: Vec<Vec<u32>> = vec![Vec::new(); n];
    let mut cur: Vec<u32> = vec![0; n];
    for j in 0..n {
        let mut g = GpuSampler::new(greedy());
        model.prefill_chunk(&mut seqs[j], &prompts[j]).unwrap();
        let t = model.sample_last_logits(&mut g).unwrap();
        ids[j].push(t);
        cur[j] = t;
    }
    while ids[0].len() < steps {
        let params: Vec<SeqSampleParams> = (0..n).map(|_| greedy_seq_params()).collect();
        let mut refs: Vec<&mut _> = seqs.iter_mut().collect();
        let out = model.batched_decode(&mut refs, &cur, &params).unwrap();
        for j in 0..n {
            ids[j].push(out[j]);
            cur[j] = out[j];
        }
    }
    for s in seqs.iter_mut() {
        model.release_seq(s);
    }
    ids
}

/// Aggregate decode throughput (sum over all lanes) at a range of batch sizes.
/// Prints the scaling curve; run with `--ignored --nocapture`.
#[test]
#[ignore = "throughput benchmark; needs a CUDA GPU + the qwen3 test GGUF"]
fn throughput_scaling() {
    let Some(mut model) = load() else { return };
    let n_steps = 128usize;

    // Single-stream reference: the tuned fused decode path the scheduler uses
    // at B=1 (batched B=1 below is only a scaling-curve anchor, not the path a
    // lone request takes).
    {
        let mut sampler = GpuSampler::new(greedy());
        let mut seq = model.new_seq();
        model.prefill_chunk(&mut seq, &prompt_a()).unwrap();
        let mut next = model.sample_last_logits(&mut sampler).unwrap();
        for _ in 0..16 {
            next = model.step_and_sample(&mut seq, next, &mut sampler).unwrap();
        }
        let t0 = std::time::Instant::now();
        for _ in 0..n_steps {
            next = model.step_and_sample(&mut seq, next, &mut sampler).unwrap();
        }
        let fused = n_steps as f64 / t0.elapsed().as_secs_f64();
        eprintln!("single-stream fused decode (scheduler B=1 path): {fused:.1} tok/s");
        model.release_seq(&mut seq);
    }

    eprintln!("qwen3-0.6b-q8_0 batched decode throughput (aggregate tok/s over all lanes):");
    let mut baseline = 0.0f64;
    for &b in &[1usize, 4, 8, 16, 32] {
        let prompts: Vec<Vec<u32>> = (0..b).map(|_| prompt_a()).collect();
        let mut seqs: Vec<_> = (0..b).map(|_| model.new_seq()).collect();
        let mut cur: Vec<u32> = vec![0; b];
        for j in 0..b {
            let mut g = GpuSampler::new(greedy());
            model.prefill_chunk(&mut seqs[j], &prompts[j]).unwrap();
            cur[j] = model.sample_last_logits(&mut g).unwrap();
        }
        let params: Vec<SeqSampleParams> = (0..b).map(|_| greedy_seq_params()).collect();
        // Warm up: capture the bucket graph and spin the clocks up.
        for _ in 0..16 {
            let mut refs: Vec<&mut _> = seqs.iter_mut().collect();
            let out = model.batched_decode(&mut refs, &cur, &params).unwrap();
            cur.copy_from_slice(&out);
        }
        let t0 = std::time::Instant::now();
        for _ in 0..n_steps {
            let mut refs: Vec<&mut _> = seqs.iter_mut().collect();
            let out = model.batched_decode(&mut refs, &cur, &params).unwrap();
            cur.copy_from_slice(&out);
        }
        let dt = t0.elapsed().as_secs_f64();
        let per_seq = n_steps as f64 / dt;
        let agg = (b * n_steps) as f64 / dt;
        if b == 1 {
            baseline = agg;
        }
        eprintln!(
            "  B={b:<3} per-seq {per_seq:7.1} tok/s | aggregate {agg:8.1} tok/s | {:.2}x baseline",
            agg / baseline
        );
        for s in seqs.iter_mut() {
            model.release_seq(s);
        }
    }
}

fn prompt_a() -> Vec<u32> {
    vec![151644, 872, 198, 105043, 100165, 11319, 151645, 198, 151644, 77091, 198]
}

fn prompt_b() -> Vec<u32> {
    vec![151644, 872, 198, 3838, 374, 220, 17, 10, 17, 30, 151645, 198, 151644, 77091, 198]
}

#[test]
fn batched_matches_single_seq() {
    let Some(mut model) = load() else { return };

    // (a) B=1 batched must match the legacy fused single-seq greedy stream.
    let single_a = single_seq_greedy(&mut model, &prompt_a(), STEPS);
    let b1 = batched_greedy(&mut model, &[prompt_a()], STEPS);
    assert_eq!(single_a, b1[0], "B=1 batched diverged from single-seq greedy");

    // (b) N identical prompts must all produce identical streams, equal to the
    // single-sequence result (per-seq isolation + correct paged attention).
    let same = batched_greedy(&mut model, &[prompt_a(), prompt_a(), prompt_a(), prompt_a()], STEPS);
    for (j, s) in same.iter().enumerate() {
        assert_eq!(*s, single_a, "identical-prompt lane {j} diverged");
    }

    // (c) Distinct prompts in one batch must each match their own single-seq
    // greedy generation.
    let single_b = single_seq_greedy(&mut model, &prompt_b(), STEPS);
    let mixed = batched_greedy(&mut model, &[prompt_a(), prompt_b(), prompt_a(), prompt_b()], STEPS);
    assert_eq!(mixed[0], single_a, "mixed lane 0 (prompt A) diverged");
    assert_eq!(mixed[1], single_b, "mixed lane 1 (prompt B) diverged");
    assert_eq!(mixed[2], single_a, "mixed lane 2 (prompt A) diverged");
    assert_eq!(mixed[3], single_b, "mixed lane 3 (prompt B) diverged");
}
