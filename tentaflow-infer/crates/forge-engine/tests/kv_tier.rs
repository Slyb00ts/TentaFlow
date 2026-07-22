// ===== File: kv_tier.rs — KV tiering correctness: tiered runs are bit-identical to VRAM-only =====
// Proves SPEC §5.4B v1 on a real model: (a) a context larger than the VRAM KV
// pool completes via spills + streamed attention with tokens EQUAL to an
// untiered big-pool run (KV bytes are moved, not transformed); (b) the
// watermark spill → full-restore path is equally exact; (c) both the RAM and
// NVMe tiers hold. Skips cleanly without a CUDA device or the qwen3 GGUF.

use std::path::PathBuf;
use std::sync::Arc;

use forge_engine::kv::KvQuant;
use forge_engine::model::{Model, ModelConfig};
use forge_engine::sample::{GpuSampler, SamplingParams, SeqSampleParams};
use forge_engine::tier::{KvTierConfig, KvTierMode};
use forge_hal::cuda::{CudaDevice, PoolSizes};
use forge_hal::Device;

/// One test at a time: they share the GPU's primary context, and a decode
/// graph capture in one test invalidates a concurrent synchronize in another.
static GPU_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn model_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-models/gguf/qwen3-0.6b-q8_0.gguf")
}

fn load(kv_pages: usize, tier: KvTierConfig, quant: KvQuant) -> Option<Model> {
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
        kv_quant: quant,
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
        let Some(mut model) = load(256, KvTierConfig::default(), KvQuant::F16) else {
            return;
        };
        greedy_ids(&mut model, &prompt, 512, 24)
    };
    for (name, tier) in [("ram", ram_tier()), ("nvme", nvme_tier())] {
        let Some(mut model) = load(48, tier, KvQuant::F16) else {
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
        let Some(mut model) = load(256, KvTierConfig::default(), KvQuant::F16) else {
            return;
        };
        greedy_ids(&mut model, &prompt, 512, 40)
    };
    let Some(mut model) = load(96, ram_tier(), KvQuant::F16) else {
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
        let Some(mut model) = load(256, KvTierConfig::default(), KvQuant::F16) else {
            return;
        };
        greedy_ids(&mut model, &prompt, 512, 24)
    };
    let Some(mut model) = load(256, ram_tier(), KvQuant::F16) else {
        return;
    };
    let ids = greedy_ids(&mut model, &prompt, 512, 24);
    assert_eq!(ids, reference, "idle tiering changed the fast path output");
}

/// Batched greedy decode over `prompts` (chunked prefill; every lane advances
/// through `batched_decode`, streamed lanes included).
fn batched_greedy(model: &mut Model, prompts: &[Vec<u32>], steps: usize) -> Vec<Vec<u32>> {
    let n = prompts.len();
    let mut seqs: Vec<_> = (0..n).map(|_| model.new_seq()).collect();
    let mut ids: Vec<Vec<u32>> = vec![Vec::new(); n];
    let mut cur: Vec<u32> = vec![0; n];
    for j in 0..n {
        let mut g = GpuSampler::new(SamplingParams {
            temperature: 0.0,
            ..SamplingParams::default()
        });
        for chunk in prompts[j].chunks(512) {
            // Mirror the scheduler's per-iteration balance: already-prefilled
            // neighbors donate their cold prefixes so this chunk fits
            // (cross-sequence eviction).
            let (done, rest) = seqs.split_at_mut(j);
            let mut donors: Vec<&mut _> = done.iter_mut().collect();
            model
                .tier_balance(&mut donors, chunk.len().div_ceil(32) + 1)
                .unwrap();
            model.prefill_chunk(&mut rest[0], chunk).unwrap();
        }
        let t = model.sample_last_logits(&mut g).unwrap();
        ids[j].push(t);
        cur[j] = t;
    }
    let params: Vec<SeqSampleParams> = (0..n)
        .map(|_| SeqSampleParams {
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
        })
        .collect();
    while ids[0].len() < steps {
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

/// Rot4 KV + tiering, byte fidelity: the packed codes + scales of every
/// spilled page must come back bit-exact after a spill → restore round trip
/// (bytes are moved, not transformed; the residual ring never leaves VRAM).
/// Token-level bit-identity is not asserted for rot — the rot decode kernels
/// are not run-to-run reproducible even without tiering.
#[test]
fn rot_tier_spill_restore_roundtrip_is_bit_exact() {
    let _gpu = GPU_LOCK.lock().unwrap();
    let prompt = long_prompt(3000);
    let rot = KvQuant::Rot {
        bits: 4,
        residual_window: 128,
        activate_at: 4096,
    };
    let Some(mut model) = load(256, ram_tier(), rot) else {
        return;
    };
    let mut seq = model.new_seq();
    for chunk in prompt.chunks(512) {
        model.prefill_chunk(&mut seq, chunk).unwrap();
    }
    // Snapshot layer 0's packed + scale bytes per logical page.
    let region_bytes = model.kv.cfg.tier_region_bytes().unwrap();
    let n_pages = seq.pages.len();
    let mut snap: Vec<Vec<Vec<u8>>> = Vec::new();
    {
        let regions = model.kv.tier_layer_regions(0);
        for (r, buf) in regions.iter().enumerate() {
            let rb = region_bytes[r];
            let mut per_page = Vec::new();
            for lp in 0..n_pages {
                let phys = seq.pages[lp] as usize;
                let mut bytes = vec![0u8; rb];
                model.device.read(buf, phys * rb, &mut bytes).unwrap();
                per_page.push(bytes);
            }
            snap.push(per_page);
        }
    }
    // Force a spill of the cold prefix, then restore it via a decode step
    // (the pool has headroom, so the step's transfer path brings every chunk
    // back into fresh pages).
    model.tier_balance(&mut [&mut seq], 200).unwrap();
    assert!(!seq.spilled.is_empty(), "balance must spill the cold prefix");
    let mut sampler = GpuSampler::new(SamplingParams {
        temperature: 0.0,
        ..SamplingParams::default()
    });
    let next = *prompt.last().unwrap();
    sampler.note_token(next);
    let _ = model.step_and_sample(&mut seq, next, &mut sampler).unwrap();
    assert!(seq.spilled.is_empty(), "step must restore the spilled chunks");
    let regions = model.kv.tier_layer_regions(0);
    for (r, buf) in regions.iter().enumerate() {
        let rb = region_bytes[r];
        // The tail page took the decode append; every other page must be
        // byte-identical across the round trip.
        for lp in 0..n_pages - 1 {
            let phys = seq.pages[lp] as usize;
            let mut bytes = vec![0u8; rb];
            model.device.read(buf, phys * rb, &mut bytes).unwrap();
            assert_eq!(
                bytes, snap[r][lp],
                "region {r} logical page {lp} changed across spill+restore"
            );
        }
    }
    model.release_seq(&mut seq);
}

/// Rot4 + tiering under real pressure: a context ~2x the VRAM pool prefills
/// (spilling packed pages) and decodes through the streamed rot attention on
/// both the RAM and NVMe tiers, staying spilled the whole time.
#[test]
fn rot_tiered_context_beyond_vram_generates() {
    let _gpu = GPU_LOCK.lock().unwrap();
    let prompt = long_prompt(3000);
    let rot = KvQuant::Rot {
        bits: 4,
        residual_window: 128,
        activate_at: 4096,
    };
    for (name, tier) in [("ram", ram_tier()), ("nvme", nvme_tier())] {
        let Some(mut model) = load(48, tier, rot) else {
            return;
        };
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
            "tier={name}: the scenario must spill during prefill"
        );
        let mut ids = vec![model.sample_last_logits(&mut sampler).unwrap()];
        while ids.len() < 24 {
            let last = *ids.last().unwrap();
            sampler.note_token(last);
            ids.push(model.step_and_sample(&mut seq, last, &mut sampler).unwrap());
        }
        assert!(
            !seq.spilled.is_empty(),
            "tier={name}: the pool cannot hold the full context, decode must stay streamed"
        );
        assert_eq!(ids.len(), 24, "tier={name}: generation must complete");
        model.release_seq(&mut seq);
    }
}

/// Cross-sequence eviction: `tier_balance` spills a neighbor's cold prefix so
/// a new long prompt prefills into an otherwise-full pool, and the new
/// sequence's tokens stay identical to an untiered run.
#[test]
fn cross_sequence_eviction_frees_neighbor_pages() {
    let _gpu = GPU_LOCK.lock().unwrap();
    let prompt_a = long_prompt(1800);
    let prompt_b = long_prompt(1400);
    let reference_b = {
        let Some(mut model) = load(256, KvTierConfig::default(), KvQuant::F16) else {
            return;
        };
        greedy_ids(&mut model, &prompt_b, 512, 24)
    };
    let Some(mut model) = load(96, ram_tier(), KvQuant::F16) else {
        return;
    };
    let mut a = model.new_seq();
    for chunk in prompt_a.chunks(512) {
        model.prefill_chunk(&mut a, chunk).unwrap();
    }
    // A holds most of the pool; balancing for B's demand must evict A's cold
    // prefix (A itself is not growing).
    let need_pages = prompt_b.len().div_ceil(32) + 1;
    model.tier_balance(&mut [&mut a], need_pages).unwrap();
    assert!(
        !a.spilled.is_empty(),
        "balance must spill the neighbor's cold prefix"
    );
    let mut sampler = GpuSampler::new(SamplingParams {
        temperature: 0.0,
        ..SamplingParams::default()
    });
    let mut b = model.new_seq();
    for chunk in prompt_b.chunks(512) {
        model.prefill_chunk(&mut b, chunk).unwrap();
    }
    let mut ids = vec![model.sample_last_logits(&mut sampler).unwrap()];
    while ids.len() < 24 {
        let last = *ids.last().unwrap();
        sampler.note_token(last);
        ids.push(model.step_and_sample(&mut b, last, &mut sampler).unwrap());
    }
    model.release_seq(&mut a);
    model.release_seq(&mut b);
    assert_eq!(
        ids, reference_b,
        "sequence B diverged after cross-sequence eviction"
    );
}

/// Streamed sequences join batched decode: a spilled long-context lane and a
/// resident lane advance through one `batched_decode` call per step, each
/// matching the untiered batched run token for token.
#[test]
fn mixed_residency_batched_decode_is_bit_identical() {
    let _gpu = GPU_LOCK.lock().unwrap();
    let prompt_long = long_prompt(3000);
    let prompt_short = long_prompt(600);
    let reference = {
        let Some(mut model) = load(256, KvTierConfig::default(), KvQuant::F16) else {
            return;
        };
        batched_greedy(&mut model, &[prompt_long.clone(), prompt_short.clone()], 24)
    };
    let Some(mut model) = load(48, ram_tier(), KvQuant::F16) else {
        return;
    };
    let out = batched_greedy(&mut model, &[prompt_long, prompt_short], 24);
    assert_eq!(out[1], reference[1], "resident lane diverged in mixed batch");
    assert_eq!(out[0], reference[0], "streamed lane diverged in mixed batch");
}
