// ===== File: prefix_cache.rs — radix prefix-cache correctness on a real model =====
// Proves SPEC §5.2 end to end on qwen3-0.6b-q8_0: (a) a second request sharing
// a ~500-token prefix skips re-prefilling it (cache_read ≈ 480, measured prefill
// drop) and produces byte-identical greedy tokens to the first request and to a
// prefix-cache-OFF run; (b) a multi-turn conversation reuses turn 1's KV for
// turn 2, correct and faster. Ignored by default (needs a CUDA GPU + the GGUF).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use forge_engine::model::{Model, ModelConfig, MAX_PREFILL_CHUNK};
use forge_hal::{PoolSizes, gpu};
use forge_hal::Device;

fn model_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-models/gguf/qwen3-0.6b-q8_0.gguf")
}

/// Load qwen3-0.6b with the prefix cache on/off. `None` = no GPU / no model
/// (the test then skips cleanly).
fn load(prefix_cache: bool) -> Option<Model> {
    let path = model_path();
    if !path.is_file() {
        eprintln!("skipping: test model missing at {}", path.display());
        return None;
    }
    let device = match gpu::open(
        0,
        PoolSizes {
            weights: 3 << 30,
            kv_cache: 4 << 20,
            activations: 1 << 30,
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
        weight_host_budget: 0,
weight_spill_dir: None,
        kv_pages: 512,
        prefix_cache,
        ..ModelConfig::default()
    };
    Some(Model::load_gguf(dev, &path, cfg).unwrap())
}

fn argmax(v: &[f32]) -> u32 {
    v.iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).expect("finite logits"))
        .expect("non-empty logits")
        .0 as u32
}

/// Deterministic prompt of `n` valid token ids (cycled seed); `salt` picks a
/// disjoint token set so different prompts never share a prefix by accident.
fn make_prompt(n: usize, salt: u32) -> Vec<u32> {
    let seed = [
        785u32, 6722, 315, 9625, 374, 264, 2244, 3283, 13, 1084, 702, 3840, 9080,
    ];
    (0..n).map(|i| seed[i % seed.len()] + salt * 100).collect()
}

/// Greedy generation through the Model directly, borrowing the cached prefix
/// first. Returns (generated ids, cache_read tokens, prefill wall-clock).
fn greedy_run(model: &mut Model, prompt: &[u32], n_gen: usize) -> (Vec<u32>, usize, f64) {
    let mut seq = model.new_seq();
    let cache_read = model.acquire_prefix(&mut seq, prompt);
    let t0 = Instant::now();
    let mut logits = None;
    for chunk in prompt[cache_read..].chunks(MAX_PREFILL_CHUNK) {
        logits = Some(model.prefill_chunk(&mut seq, chunk).unwrap());
    }
    let prefill_s = t0.elapsed().as_secs_f64();
    let mut ids = Vec::with_capacity(n_gen);
    let mut next = argmax(&logits.expect("at least one prefill chunk"));
    ids.push(next);
    while ids.len() < n_gen {
        let l = model.step(&mut seq, next).unwrap();
        next = argmax(&l);
        ids.push(next);
    }
    model.release_seq(&mut seq);
    (ids, cache_read, prefill_s)
}

// Big "system prompt" so the cold prefill is substantial and dwarfs launch /
// clock noise; the hit then prefills only the ~1-page divergent tail.
const PREFIX_TOKENS: usize = 2048;
const GEN: usize = 24;

// 2048 tokens, page 32, capped at 2047 → floor(2047/32)=63 whole pages = 2016.
const EXPECT_SHARED: usize = 2016;

#[test]
#[ignore = "requires a CUDA GPU and the qwen3-0.6b test GGUF"]
fn shared_prefix_is_bit_identical_and_skips_prefill() {
    let Some(mut model) = load(true) else { return };
    // Spin the (idle-downclocked) GPU up and capture the prefill/decode kernels
    // on disjoint prompts so the cold-vs-hit comparison reflects skipped work,
    // not first-call capture or a cold clock.
    for salt in 5..9 {
        let _ = greedy_run(&mut model, &make_prompt(PREFIX_TOKENS, salt), 4);
    }

    let prompt = make_prompt(PREFIX_TOKENS, 0);
    // Cold: the cache is empty for this prompt, so nothing is shared.
    let (ids_cold, cr_cold, t_cold) = greedy_run(&mut model, &prompt, GEN);
    assert_eq!(cr_cold, 0, "cold run should not hit the cache");

    // Hit: the cold run donated its prefill pages on completion; the second
    // identical request borrows them and prefills only the partial tail. Take
    // the best of a few hits (all served from cache) to shed timing jitter.
    let mut ids_hit = Vec::new();
    let mut cr_hit = 0;
    let mut t_hit = f64::INFINITY;
    for _ in 0..3 {
        let (ids, cr, t) = greedy_run(&mut model, &prompt, GEN);
        ids_hit = ids;
        cr_hit = cr;
        t_hit = t_hit.min(t);
    }
    eprintln!(
        "prefix hit: cache_read={cr_hit} tok, prefill cold {:.2} ms ({:.0} tok/s) → hit {:.2} ms ({:.1}x faster)",
        t_cold * 1e3,
        PREFIX_TOKENS as f64 / t_cold,
        t_hit * 1e3,
        t_cold / t_hit.max(1e-9)
    );
    assert_eq!(cr_hit, EXPECT_SHARED, "expected the whole shared pages");
    assert_eq!(ids_hit, ids_cold, "prefix-cache hit diverged from cold run");
    assert!(
        t_hit < t_cold,
        "prefill hit ({t_hit:.4}s) not faster than cold ({t_cold:.4}s)"
    );

    drop(model);

    // OFF must reproduce the ON-cold greedy stream byte-for-byte.
    let Some(mut off) = load(false) else { return };
    let _ = greedy_run(&mut off, &make_prompt(PREFIX_TOKENS, 9), 2);
    let (ids_off, cr_off, _) = greedy_run(&mut off, &prompt, GEN);
    assert_eq!(cr_off, 0, "prefix cache OFF must never report a hit");
    assert_eq!(ids_off, ids_cold, "prefix-cache OFF diverged from ON");
}

#[test]
#[ignore = "requires a CUDA GPU and the qwen3-0.6b test GGUF"]
fn multi_turn_reuses_prior_turn_kv() {
    // Reference: turn-2 prompt generated from scratch with the cache OFF.
    let turn1 = make_prompt(128, 1);
    let mut turn2 = turn1.clone();
    turn2.extend(make_prompt(96, 2)); // 224 tokens, first 128 = turn 1
    let Some(mut off) = load(false) else { return };
    let (ids_ref, _, _) = greedy_run(&mut off, &turn2, GEN);
    drop(off);

    let Some(mut model) = load(true) else { return };
    // Turn 1 prefills + decodes; its prefill pages enter the radix tree.
    let (_ids1, cr1, _) = greedy_run(&mut model, &turn1, 8);
    assert_eq!(cr1, 0, "turn 1 is the first request, no hit");
    // Turn 2 shares turn 1's whole prefill pages (128 tokens = 4 pages).
    let (ids2, cr2, t2) = greedy_run(&mut model, &turn2, GEN);
    eprintln!(
        "multi-turn: turn-2 cache_read={cr2} tok, prefill {:.2} ms",
        t2 * 1e3
    );
    assert_eq!(cr2, 128, "turn 2 should reuse turn 1's 4 whole KV pages");
    assert_eq!(ids2, ids_ref, "multi-turn reuse changed the greedy output");
}
