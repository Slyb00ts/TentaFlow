// ===== File: longctx.rs — long-context sanity + decode-speed-vs-depth probe =====
// Verifies the engine sustains generation to a deep position (default 32k):
// KV capacity, RoPE correctness at large positions, and how decode throughput
// degrades as attention reads a growing cache. Reports tok/s per depth band
// and prints text samples at depth so garbling is visible.
//
// Usage: cargo run -p forge-engine --release --example longctx -- <model> [target_pos]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use forge_engine::model::{Model, ModelConfig};
use forge_engine::sample::{Sampler, SamplingParams};
use forge_hal::{PoolSizes, gpu};
use forge_hal::Device;
use forge_tokenize::{StreamDecoder, Tokenizer};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(args.next().expect("model path"));
    let target: usize = args.next().map(|s| s.parse().unwrap()).unwrap_or(32768);
    let kv_fp8 = args.next().as_deref() == Some("fp8");

    // Size KV for the full target so capacity is never the limiter.
    let page_size = 32usize;
    let kv_pages = (target / page_size) + 8;
    let cfg = ModelConfig {
        weight_host_budget: 0,
weight_spill_dir: None,
        kv_page_size: page_size,
        kv_pages,
        max_seq_len: target + 64,
        kv_quant: if kv_fp8 {
            forge_engine::kv::KvQuant::Fp8
        } else {
            forge_engine::kv::KvQuant::F16
        },
        kv_tier: Default::default(),
        prefix_cache: false,
        layer_range: None,
        native_mtp: false,
        nvfp4_gguf_layout: forge_engine::model::Nvfp4GgufLayout::RowMajor36,
        nvfp4_ct_layout: forge_engine::weights::NvFp4CtLayoutPolicy::RowMajorE4M3,
    };

    let device = gpu::open(
        0,
        PoolSizes {
            weights: 12 << 30,
            kv_cache: 8 << 30,
            activations: 1 << 30,
            kv_page_size: 256 << 10,
        },
    )?;
    let dev: Arc<dyn Device> = device;

    let (mut model, tokenizer) = if path.is_dir() {
        let m = Model::load_safetensors_dir(dev, &path, cfg)?;
        let t = Tokenizer::from_file(path.join("tokenizer.json"))?;
        (m, t)
    } else {
        let gguf = forge_formats::Gguf::open(&path)?;
        let vocab = forge_engine::gguf_vocab::gguf_vocab(&gguf)?;
        drop(gguf);
        let t = Tokenizer::from_gguf_vocab(&vocab)?;
        let m = Model::load_gguf(dev, &path, cfg)?;
        (m, t)
    };

    let p = &model.weights.descriptor.params;
    eprintln!(
        "arch={} layers={} max_pos={} kv_pages={} target={}",
        model.weights.descriptor.arch, p.block_count, p.max_position_embeddings, kv_pages, target
    );
    let eos: Vec<u32> = tokenizer.eos_id().into_iter().collect();

    // A story prompt that invites long continuation.
    let prompt = "Napisz długie, szczegółowe opowiadanie o wyprawie w Tatry. Rozdział 1.";
    let prompt_ids = tokenizer.encode(prompt, true)?;
    let mut seq = model.new_seq();

    let mut sampler = Sampler::new(SamplingParams {
        temperature: 0.8,
        top_k: 40,
        top_p: 0.95,
        seed: Some(42),
        ..Default::default()
    });
    let mut decoder = StreamDecoder::new(&tokenizer, true);
    let mut recent: Vec<u32> = Vec::new();

    // Prefill.
    let t_prefill = Instant::now();
    let mut logits = Vec::new();
    let mut i = 0;
    while i < prompt_ids.len() {
        let end = (i + 256).min(prompt_ids.len());
        logits = model.prefill_chunk(&mut seq, &prompt_ids[i..end])?;
        i = end;
    }
    eprintln!(
        "prefill {} tok in {:.2}s",
        prompt_ids.len(),
        t_prefill.elapsed().as_secs_f32()
    );

    // Decode to target depth, timing each 2k band.
    let band = 2048usize;
    let mut band_start = Instant::now();
    let mut band_tokens = 0usize;
    let mut sample_txt = String::new();
    let mut last_pos = seq.len;

    while seq.len < target {
        let next = sampler.sample(&logits, &recent)?;
        recent.push(next);
        if recent.len() > 256 {
            recent.remove(0);
        }
        if eos.contains(&next) {
            eprintln!("EOS at pos {}", seq.len);
            break;
        }
        let piece = decoder.push(next)?;
        // Capture a readable sample around each band boundary.
        if seq.len >= last_pos && sample_txt.len() < 160 {
            sample_txt.push_str(&piece);
        }
        band_tokens += 1;

        if seq.len / band != (seq.len - 1) / band {
            let dt = band_start.elapsed().as_secs_f32();
            eprintln!(
                "depth {:>6}: {:.1} tok/s   sample@depth: {:?}",
                seq.len,
                band_tokens as f32 / dt,
                sample_txt.trim()
            );
            band_start = Instant::now();
            band_tokens = 0;
            sample_txt.clear();
            last_pos = seq.len;
        }

        logits = model.step(&mut seq, next)?;
    }

    eprintln!(
        "reached depth {} (kv_fp8={kv_fp8}) (coherent if samples above are real words)",
        seq.len
    );
    model.release_seq(&mut seq);
    Ok(())
}
