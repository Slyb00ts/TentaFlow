// ===== File: batched_bielik.rs — golden-ids gate for the batched decode path =====
// Ignored by default (needs a CUDA GPU + the local Bielik NVFP4 snapshot).
// Proves the continuous-batching forward pass reproduces the canonical Bielik
// greedy stream: (a) a single-sequence batch matches the golden ids exactly and
// (b) four identical prompts batched together all reproduce the golden stream
// (per-seq isolation + correct paged attention). Run:
//   cargo test -p forge-server --release --test batched_bielik -- --ignored --nocapture

use std::path::Path;
use std::sync::Arc;

use forge_engine::kv::SeqKv;
use forge_engine::model::Model;
use forge_engine::model::ModelConfig;
use forge_engine::sample::{GpuSampler, SamplingParams, SeqSampleParams};
use forge_engine::server::{spawn_engine_batched, EngineEvent, EngineRequest, SpeculativeConfig};
use forge_hal::cuda::{CudaDevice, PoolSizes};
use forge_hal::Device;
use forge_server::source::{kv_pool_bytes, load_model, read_descriptor};
use sha2::{Digest, Sha256};

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
        frequency_penalty: 0.0,
        presence_penalty: 0.0,
        penalty_ids: Vec::new(),
        penalty_counts: Vec::new(),
    }
}

fn nvfp4_ct_layout() -> forge_engine::weights::NvFp4CtLayoutPolicy {
    match std::env::var("FORGE_NVFP4_CT_LAYOUT").as_deref() {
        Ok("s0") => forge_engine::weights::NvFp4CtLayoutPolicy::S0N64K128,
        _ => forge_engine::weights::NvFp4CtLayoutPolicy::RowMajorE4M3,
    }
}

fn ids_sha256(ids: &[u32]) -> String {
    let mut hasher = Sha256::new();
    for id in ids {
        hasher.update(id.to_le_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn kv_layer_byte_diff(
    model: &Model,
    reference: &SeqKv,
    actual: &SeqKv,
    layer: usize,
) -> (usize, usize) {
    let page_bytes = model.kv.cfg.n_kv_heads * model.kv.cfg.page_size * model.kv.cfg.head_dim * 2;
    let compare = |buffer: &forge_hal::DevBuffer| {
        let mut differences = 0usize;
        for (&expected_page, &actual_page) in reference.pages.iter().zip(&actual.pages) {
            let mut expected = vec![0u8; page_bytes];
            let mut observed = vec![0u8; page_bytes];
            model
                .device
                .read(buffer, expected_page as usize * page_bytes, &mut expected)
                .unwrap();
            model
                .device
                .read(buffer, actual_page as usize * page_bytes, &mut observed)
                .unwrap();
            differences += expected
                .iter()
                .zip(&observed)
                .filter(|(left, right)| left != right)
                .count();
        }
        differences
    };
    (compare(&model.kv.k[layer]), compare(&model.kv.v[layer]))
}

/// Oba testy ładują pełnego Bielika z pulą wag 12 GB, więc równolegle nie
/// mieszczą się w VRAM (`.expect("cuda device")` pada przy tworzeniu drugiego
/// urządzenia). Mutex serializuje je w obrębie binarki testowej.
static BIELIK_GPU: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[test]
#[ignore = "requires a CUDA GPU and the local Bielik snapshot"]
fn batched_reproduces_golden() {
    let _serialized = BIELIK_GPU.lock().unwrap_or_else(|e| e.into_inner());
    let path = Path::new(BIELIK_DIR);
    if !path.is_dir() {
        eprintln!("skipping: Bielik snapshot missing at {BIELIK_DIR}");
        return;
    }
    let steps = GOLDEN.len();
    let kv_page_size = 32;
    let kv_pages = 640;
    let desc = read_descriptor(path).expect("read descriptor");
    let device = CudaDevice::new(
        0,
        PoolSizes {
            weights: 12 << 30,
            kv_cache: kv_pool_bytes(
                &desc,
                kv_page_size,
                kv_pages,
                forge_engine::kv::KvQuant::F16,
                false,
            )
            .unwrap()
            .max(1 << 30),
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
            native_mtp: false,
            nvfp4_gguf_layout: forge_engine::model::Nvfp4GgufLayout::RowMajor36,
            nvfp4_ct_layout: nvfp4_ct_layout(),
        },
    )
    .expect("load model");
    if std::env::var("FORGE_GEMM").ok().as_deref() == Some("fp8mod-ffn") {
        assert!(
            loaded.model.build_fp8_ffn().expect("budowa paczek FP8").built(),
            "Bielik test wymaga aktywnych paczek FP8"
        );
        eprintln!("Bielik test: resident FP8 Q/O/FFN/lm_head aktywne");
    }
    let prompt = loaded
        .bundle
        .tokenizer
        .encode("Stolicą Polski jest", true)
        .expect("encode prompt");
    let model = &mut loaded.model;

    let mut prompt_1024 = Vec::with_capacity(1024);
    while prompt_1024.len() < 1024 {
        prompt_1024.extend_from_slice(&prompt);
    }
    prompt_1024.truncate(1024);
    let mut reference_seq = model.new_seq();
    let mut reference_sampler = GpuSampler::new(greedy());
    let started = std::time::Instant::now();
    for (chunk_index, chunk) in prompt_1024.chunks(128).enumerate() {
        if chunk_index == 7 {
            model
                .prefill_chunk_device_logits(&mut reference_seq, chunk)
                .expect("B1 P1024 final prefill");
        } else {
            model
                .prefill_chunk_device_sync(&mut reference_seq, chunk)
                .expect("B1 P1024 chunk");
        }
    }
    let expected = model
        .sample_last_logits(&mut reference_sampler)
        .expect("B1 P1024 sample");
    eprintln!(
        "Bielik P1024 B1: {:.3} ms",
        started.elapsed().as_secs_f64() * 1e3
    );
    for batch in [4usize, 8, 16] {
        let chunk = 128.min(1024 / batch);
        assert!(model.dense_prefill_batch_capable(batch, chunk));
        let mut seqs = (0..batch).map(|_| model.new_seq()).collect::<Vec<_>>();
        let started = std::time::Instant::now();
        let chunk_count = 1024 / chunk;
        for chunk_index in 0..chunk_count {
            let range = chunk_index * chunk..(chunk_index + 1) * chunk;
            let token_lanes = (0..batch)
                .map(|_| &prompt_1024[range.clone()])
                .collect::<Vec<_>>();
            let mut refs = seqs.iter_mut().collect::<Vec<_>>();
            if chunk_index + 1 == chunk_count {
                model
                    .prefill_batch_device_logits(&mut refs, &token_lanes)
                    .expect("batch P1024 prefill");
            } else {
                model
                    .prefill_batch_device_sync(&mut refs, &token_lanes)
                    .expect("batch P1024 chunk");
            }
        }
        let mut samplers = (0..batch)
            .map(|_| GpuSampler::new(greedy()))
            .collect::<Vec<_>>();
        let mut sampler_refs = samplers.iter_mut().collect::<Vec<_>>();
        let actual = model
            .sample_prefill_batch_logits(&mut sampler_refs)
            .expect("batch P1024 sample");
        assert_eq!(actual, vec![expected; batch], "Bielik P1024 B={batch}");
        if batch == 4 {
            for layer in 0..model.kv.k.len() {
                let lane0 = kv_layer_byte_diff(model, &reference_seq, &seqs[0], layer);
                let lane3 = kv_layer_byte_diff(model, &reference_seq, &seqs[3], layer);
                eprintln!(
                    "KV byte diff layer={layer} lane0 K={} V={} lane3 K={} V={}",
                    lane0.0, lane0.1, lane3.0, lane3.1
                );
            }
        }
        eprintln!(
            "Bielik P1024 B{batch} T{chunk}: {:.3} ms, {:.1} tok/s",
            started.elapsed().as_secs_f64() * 1e3,
            (batch * 1024) as f64 / started.elapsed().as_secs_f64()
        );
        for seq in &mut seqs {
            model.release_seq(seq);
        }
    }
    model.release_seq(&mut reference_seq);

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
        let out = model
            .batched_decode(&mut refs, &cur, &params)
            .expect("batched decode");
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

#[test]
#[ignore = "requires a CUDA GPU and the local Bielik snapshot"]
fn scheduler_prefill_p1024_o256_b1_b4_b8_b16() {
    let _serialized = BIELIK_GPU.lock().unwrap_or_else(|e| e.into_inner());
    let path = Path::new(BIELIK_DIR);
    if !path.is_dir() {
        eprintln!("skipping: Bielik snapshot missing at {BIELIK_DIR}");
        return;
    }
    let kv_page_size = 32;
    let kv_pages = 640;
    let desc = read_descriptor(path).expect("read descriptor");
    let device = CudaDevice::new(
        0,
        PoolSizes {
            weights: 12 << 30,
            kv_cache: kv_pool_bytes(
                &desc,
                kv_page_size,
                kv_pages,
                forge_engine::kv::KvQuant::F16,
                false,
            )
            .unwrap()
            .max(1 << 30),
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
            native_mtp: false,
            nvfp4_gguf_layout: forge_engine::model::Nvfp4GgufLayout::RowMajor36,
            nvfp4_ct_layout: nvfp4_ct_layout(),
        },
    )
    .expect("load model");
    if std::env::var("FORGE_GEMM").ok().as_deref() == Some("fp8mod-ffn") {
        assert!(loaded.model.build_fp8_ffn().expect("budowa paczek FP8").built());
    }
    let seed = loaded
        .bundle
        .tokenizer
        .encode("Stolicą Polski jest", true)
        .expect("encode prompt");
    let mut prompt = Vec::with_capacity(1024);
    while prompt.len() < 1024 {
        prompt.extend_from_slice(&seed);
    }
    prompt.truncate(1024);
    let tokenizer = Arc::new(loaded.bundle.tokenizer);
    let engine = spawn_engine_batched(
        loaded.model,
        tokenizer,
        16,
        128,
        2,
        SpeculativeConfig::off(),
    )
    .expect("spawn engine");
    let output_tokens = std::env::var("FORGE_BIELIK_TEST_TOKENS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(256);
    let mut reference = Vec::new();
    for batch in [1usize, 4, 8, 16] {
        let started = std::time::Instant::now();
        let receivers = (0..batch)
            .map(|_| {
                engine
                    .submit(EngineRequest {
                        prompt_tokens: prompt.clone(),
                        max_tokens: output_tokens,
                        sampling: greedy(),
                        emit_empty_tokens: true,
                        ..EngineRequest::default()
                    })
                    .expect("submit")
            })
            .collect::<Vec<_>>();
        let mut outputs = Vec::with_capacity(batch);
        for receiver in receivers {
            let mut ids = Vec::new();
            loop {
                match receiver.recv().expect("event") {
                    EngineEvent::Token { id, .. } => ids.push(id),
                    EngineEvent::Done { .. } => break,
                    EngineEvent::Error(error) => panic!("scheduler error: {error}"),
                }
            }
            outputs.push(ids);
        }
        if batch == 1 {
            reference = outputs[0].clone();
            assert_eq!(reference.len(), output_tokens);
            eprintln!("scheduler Bielik reference_ids={reference:?}");
        } else {
            for (lane, output) in outputs.iter().enumerate() {
                assert_eq!(output, &reference, "B={batch} lane={lane}");
            }
        }
        for (lane, output) in outputs.iter().enumerate() {
            eprintln!(
                "scheduler Bielik O{output_tokens} B{batch} lane={lane} sha256={}",
                ids_sha256(output)
            );
        }
        eprintln!(
            "scheduler Bielik P1024/O256 B{batch}: {:.3}s",
            started.elapsed().as_secs_f64()
        );
    }
}
