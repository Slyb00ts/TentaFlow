// ===== File: batched_bielik.rs — golden-ids gate for the batched decode path =====
// Ignored by default (needs a CUDA GPU + the local Bielik NVFP4 snapshot).
// Proves the continuous-batching forward pass reproduces the canonical Bielik
// greedy stream: (a) a single-sequence batch matches the golden ids exactly and
// (b) four identical prompts batched together all reproduce the golden stream
// (per-seq isolation + correct paged attention). Run:
//   cargo test -p forge-server --release --test batched_bielik -- --ignored --nocapture

use std::path::Path;
use std::sync::Arc;

use forge_engine::model::ModelConfig;
use forge_engine::sample::{GpuSampler, SamplingParams, SeqSampleParams};
use forge_hal::cuda::{CudaDevice, PoolSizes};
use forge_hal::Device;
use forge_server::source::{kv_pool_bytes, load_model, read_descriptor};

const BIELIK_DIR: &str = "/home/critix/repos/rust/TentaFlow/.runtime/models/models--TentaFlow--Bielik-PL-Minitron-7B-NVFP4/snapshots/831550e879fd7d700e3f6d79dffc14373deda3a7";

/// Canonical Bielik-PL-Minitron-7B-NVFP4 greedy continuation of
/// "Stolicą Polski jest" (16 tokens), the reference for the whole engine.
const GOLDEN: [u32; 16] = [
    3718, 31917, 28220, 403, 8068, 15212, 265, 1182, 392, 3468, 3604, 6690, 285, 4061, 31917, 3718,
];

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
        penalty_ids: Vec::new(),
    }
}

#[test]
#[ignore = "requires a CUDA GPU and the local Bielik snapshot"]
fn batched_reproduces_golden() {
    let path = Path::new(BIELIK_DIR);
    if !path.is_dir() {
        eprintln!("skipping: Bielik snapshot missing at {BIELIK_DIR}");
        return;
    }
    let steps = GOLDEN.len();
    let kv_page_size = 32;
    let kv_pages = 256;
    let desc = read_descriptor(path).expect("read descriptor");
    let device = CudaDevice::new(
        0,
        PoolSizes {
            weights: 8 << 30,
            kv_cache: kv_pool_bytes(&desc, kv_page_size, kv_pages, forge_engine::kv::KvQuant::F16).max(1 << 30),
            activations: 2 << 30,
            kv_page_size: PoolSizes::DEFAULT_KV_PAGE,
        },
    )
    .expect("cuda device");
    let dev: Arc<dyn Device> = device;
    let mut loaded = load_model(
        dev,
        path,
        ModelConfig {
            kv_page_size,
            kv_pages,
            max_seq_len: 4096,
            kv_quant: forge_engine::kv::KvQuant::F16,
            kv_tier: Default::default(),
            prefix_cache: false,
        },
    )
    .expect("load model");
    let prompt = loaded
        .bundle
        .tokenizer
        .encode("Stolicą Polski jest", true)
        .expect("encode prompt");
    let model = &mut loaded.model;

    // (a) B=1 batched.
    let n = 4;
    let mut seqs: Vec<_> = (0..n).map(|_| model.new_seq()).collect();
    let mut ids: Vec<Vec<u32>> = vec![Vec::new(); n];
    let mut cur: Vec<u32> = vec![0; n];
    for j in 0..n {
        let mut g = GpuSampler::new(greedy());
        model.prefill_chunk(&mut seqs[j], &prompt).expect("prefill");
        let t = model.sample_last_logits(&mut g).expect("first token");
        ids[j].push(t);
        cur[j] = t;
    }
    while ids[0].len() < steps {
        let params: Vec<SeqSampleParams> = (0..n).map(|_| greedy_seq_params()).collect();
        let mut refs: Vec<&mut _> = seqs.iter_mut().collect();
        let out = model.batched_decode(&mut refs, &cur, &params).expect("batched decode");
        for j in 0..n {
            ids[j].push(out[j]);
            cur[j] = out[j];
        }
    }
    for s in seqs.iter_mut() {
        model.release_seq(s);
    }

    for (j, lane) in ids.iter().enumerate() {
        assert_eq!(*lane, GOLDEN, "batched lane {j} diverged from golden ids");
    }
    for s in seqs.iter_mut() {
        model.release_seq(s);
    }
    eprintln!("batched decode reproduced the Bielik golden stream on {n} lanes");

    // Throughput scaling: aggregate tok/s summed over all lanes.
    let n_steps = 96usize;
    // Single-stream fused reference (the scheduler B=1 path).
    {
        let mut g = GpuSampler::new(greedy());
        let mut seq = model.new_seq();
        model.prefill_chunk(&mut seq, &prompt).unwrap();
        let mut next = model.sample_last_logits(&mut g).unwrap();
        for _ in 0..8 {
            next = model.step_and_sample(&mut seq, next, &mut g).unwrap();
        }
        let t0 = std::time::Instant::now();
        for _ in 0..n_steps {
            next = model.step_and_sample(&mut seq, next, &mut g).unwrap();
        }
        let fused = n_steps as f64 / t0.elapsed().as_secs_f64();
        eprintln!("Bielik single-stream fused decode (scheduler B=1 path): {fused:.1} tok/s");
        model.release_seq(&mut seq);
    }
    eprintln!("Bielik NVFP4 batched decode throughput (aggregate tok/s over all lanes):");
    let mut baseline = 0.0f64;
    for &bsz in &[1usize, 4, 8, 16, 32] {
        let mut bs: Vec<_> = (0..bsz).map(|_| model.new_seq()).collect();
        let mut c: Vec<u32> = vec![0; bsz];
        for j in 0..bsz {
            let mut g = GpuSampler::new(greedy());
            model.prefill_chunk(&mut bs[j], &prompt).unwrap();
            c[j] = model.sample_last_logits(&mut g).unwrap();
        }
        let ps: Vec<SeqSampleParams> = (0..bsz).map(|_| greedy_seq_params()).collect();
        for _ in 0..8 {
            let mut r: Vec<&mut _> = bs.iter_mut().collect();
            let out = model.batched_decode(&mut r, &c, &ps).unwrap();
            c.copy_from_slice(&out);
        }
        let t0 = std::time::Instant::now();
        for _ in 0..n_steps {
            let mut r: Vec<&mut _> = bs.iter_mut().collect();
            let out = model.batched_decode(&mut r, &c, &ps).unwrap();
            c.copy_from_slice(&out);
        }
        let dt = t0.elapsed().as_secs_f64();
        let agg = (bsz * n_steps) as f64 / dt;
        if bsz == 1 {
            baseline = agg;
        }
        eprintln!(
            "  B={bsz:<3} per-seq {:7.1} tok/s | aggregate {agg:8.1} tok/s | {:.2}x baseline",
            n_steps as f64 / dt,
            agg / baseline
        );
        for s in bs.iter_mut() {
            model.release_seq(s);
        }
    }
}
