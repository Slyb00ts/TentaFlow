// ===== File: kv_tier.rs — KV tiering correctness: tiered runs are bit-identical to VRAM-only =====
// Proves SPEC §5.4B v1 on a real model: (a) a context larger than the VRAM KV
// pool completes via spills + streamed attention with tokens EQUAL to an
// untiered big-pool run (KV bytes are moved, not transformed); (b) the
// watermark spill → full-restore path is equally exact; (c) both the RAM and
// NVMe tiers hold. Skips cleanly without a CUDA device or the qwen3 GGUF.

use std::path::PathBuf;
use std::sync::Arc;

use forge_engine::model::{Model, ModelConfig};
use forge_engine::sample::{GpuSampler, SamplingParams};
use forge_engine::tier::{KvTierConfig, KvTierMode};
use forge_hal::cuda::{CudaDevice, PoolSizes};
use forge_hal::Device;

/// One test at a time: they share the GPU's primary context, and a decode
/// graph capture in one test invalidates a concurrent synchronize in another.
static GPU_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn model_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-models/gguf/qwen3-0.6b-q8_0.gguf")
}

fn load(kv_pages: usize, tier: KvTierConfig) -> Option<Model> {
    let path = model_path();
    if !path.is_file() {
        eprintln!("skipping: test model missing at {}", path.display());
        return None;
    }
    let device = match CudaDevice::new(
        0,
        PoolSizes {
            weights: 3 << 30,
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
        kv_pages,
        kv_tier: tier,
        ..ModelConfig::default()
    };
    Some(Model::load_gguf(dev, &path, cfg).unwrap())
}

fn ram_tier() -> KvTierConfig {
    KvTierConfig {
        mode: KvTierMode::Ram,
        ..KvTierConfig::default()
    }
}

fn nvme_tier() -> KvTierConfig {
    KvTierConfig {
        mode: KvTierMode::Nvme,
        // Tiny RAM budget forces demotion to the spill file, so the cold
        // (file-backed) streaming path is actually exercised.
        ram_budget_bytes: 16 << 20,
        ..KvTierConfig::default()
    }
}

/// A long prompt of valid qwen3 chat tokens (cycled body, chat-shaped tail).
fn long_prompt(len: usize) -> Vec<u32> {
    let body = [105043u32, 100165, 11319, 3838, 374, 220, 17, 10, 17, 30];
    let mut p: Vec<u32> = vec![151644, 872, 198];
    while p.len() < len - 5 {
        p.push(body[p.len() % body.len()]);
    }
    p.extend_from_slice(&[151645, 198, 151644, 77091, 198]);
    p
}

/// Greedy generation: chunked prefill + single-stream decode (the tiered
/// model routes spilled sequences through the streamed path internally).
fn greedy_ids(model: &mut Model, prompt: &[u32], prefill_chunk: usize, steps: usize) -> Vec<u32> {
    let mut sampler = GpuSampler::new(SamplingParams {
        temperature: 0.0,
        ..SamplingParams::default()
    });
    let mut seq = model.new_seq();
    for chunk in prompt.chunks(prefill_chunk) {
        model.prefill_chunk(&mut seq, chunk).unwrap();
    }
    let mut ids = vec![model.sample_last_logits(&mut sampler).unwrap()];
    while ids.len() < steps {
        let last = *ids.last().unwrap();
        sampler.note_token(last);
        ids.push(model.step_and_sample(&mut seq, last, &mut sampler).unwrap());
    }
    model.release_seq(&mut seq);
    ids
}

/// Context (3000 tok) larger than the VRAM KV pool (48 pages = 1536 tok):
/// prefill spills + streams, every decode step streams the spilled prefix
/// through staging. Tokens must equal the untiered big-pool run exactly.
#[test]
fn streamed_context_beyond_vram_is_bit_identical() {
    let _gpu = GPU_LOCK.lock().unwrap();
    let prompt = long_prompt(3000);
    let reference = {
        let Some(mut model) = load(256, KvTierConfig::default()) else {
            return;
        };
        greedy_ids(&mut model, &prompt, 512, 24)
    };
    for (name, tier) in [("ram", ram_tier()), ("nvme", nvme_tier())] {
        let Some(mut model) = load(48, tier) else {
            return;
        };
        let ids = greedy_ids(&mut model, &prompt, 512, 24);
        assert_eq!(
            ids, reference,
            "tier={name}: streamed generation diverged from the untiered run"
        );
    }
}

/// Full restore path: a neighbor sequence squeezes the pool so the long
/// sequence spills during prefill; releasing the neighbor frees headroom and
/// the first decode step restores every spilled chunk (transfer-vs-recompute
/// rule) back into fresh VRAM pages. Tokens must stay exact and the sequence
/// must actually return to full residency.
#[test]
fn watermark_spill_and_restore_is_bit_identical() {
    let _gpu = GPU_LOCK.lock().unwrap();
    let prompt = long_prompt(1400);
    let reference = {
        let Some(mut model) = load(256, KvTierConfig::default()) else {
            return;
        };
        greedy_ids(&mut model, &prompt, 512, 40)
    };
    let Some(mut model) = load(96, ram_tier()) else {
        return;
    };
    let mut neighbor = model.new_seq();
    for chunk in long_prompt(1800).chunks(512) {
        model.prefill_chunk(&mut neighbor, chunk).unwrap();
    }
    let mut sampler = GpuSampler::new(SamplingParams {
        temperature: 0.0,
        ..SamplingParams::default()
    });
    let mut seq = model.new_seq();
    for chunk in prompt.chunks(512) {
        model.prefill_chunk(&mut seq, chunk).unwrap();
    }
    assert!(
        !seq.spilled.is_empty(),
        "scenario must actually spill during prefill"
    );
    model.release_seq(&mut neighbor);
    let mut ids = vec![model.sample_last_logits(&mut sampler).unwrap()];
    while ids.len() < 40 {
        let last = *ids.last().unwrap();
        sampler.note_token(last);
        ids.push(model.step_and_sample(&mut seq, last, &mut sampler).unwrap());
    }
    assert!(
        seq.spilled.is_empty(),
        "headroom after release must trigger a full restore"
    );
    model.release_seq(&mut seq);
    assert_eq!(ids, reference, "restore path diverged from the untiered run");
}

/// Tiering on but never under pressure: the fast graphed path must stay in
/// charge and produce identical tokens (zero-regression hot path).
#[test]
fn tier_on_without_pressure_is_bit_identical() {
    let _gpu = GPU_LOCK.lock().unwrap();
    let prompt = long_prompt(600);
    let reference = {
        let Some(mut model) = load(256, KvTierConfig::default()) else {
            return;
        };
        greedy_ids(&mut model, &prompt, 512, 24)
    };
    let Some(mut model) = load(256, ram_tier()) else {
        return;
    };
    let ids = greedy_ids(&mut model, &prompt, 512, 24);
    assert_eq!(ids, reference, "idle tiering changed the fast path output");
}
