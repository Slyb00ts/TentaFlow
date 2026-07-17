// ===== File: golden_ids.rs — greedy token-id dump for bit-exactness gating =====
// Usage: golden_ids <model> <prompt> <max_tokens> [prefill_chunk]
// Drives Model::prefill_chunk + Model::step directly (no stream decoder, so
// no ids are hidden by empty text pieces), then replays the same greedy
// stream through the on-GPU sampler and fails on any divergence. Prints the
// (identical) greedy ids.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Result};
use forge_engine::model::ModelConfig;
use forge_engine::sample::{GpuSampler, SamplingParams};
use forge_hal::cuda::CudaDevice;
use forge_hal::Device;
use forge_server::source::load_model;

fn argmax(logits: &[f32]) -> u32 {
    let mut best = 0usize;
    for (i, &v) in logits.iter().enumerate() {
        if v > logits[best] {
            best = i;
        }
    }
    best as u32
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        bail!("usage: golden_ids <model> <prompt> <max_tokens> [prefill_chunk]");
    }
    let model_path = PathBuf::from(&args[1]);
    let prompt = &args[2];
    let max_tokens: usize = args[3].parse()?;
    let prefill_chunk: usize = args.get(4).map_or(Ok(256), |s| s.parse())?;

    let device = CudaDevice::with_default_pools(0)?;
    let dev: Arc<dyn Device> = device;
    let mut loaded = load_model(dev, &model_path, ModelConfig::default())?;
    let tokenizer = Arc::new(loaded.bundle.tokenizer);
    let prompt_tokens = tokenizer
        .encode(prompt, true)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    eprintln!("prompt tokens: {}", prompt_tokens.len());

    let model = &mut loaded.model;
    let mut seq = model.new_seq();
    let mut logits = Vec::new();
    for chunk in prompt_tokens.chunks(prefill_chunk) {
        logits = model.prefill_chunk(&mut seq, chunk)?;
    }
    let mut ids = Vec::new();
    let mut next = argmax(&logits);
    ids.push(next);
    while ids.len() < max_tokens {
        logits = model.step(&mut seq, next)?;
        next = argmax(&logits);
        ids.push(next);
    }
    model.release_seq(&mut seq);

    // The GPU greedy sampler must reproduce the CPU argmax ids bit-exactly.
    let params = SamplingParams {
        temperature: 0.0,
        ..SamplingParams::default()
    };
    let mut sampler = GpuSampler::new(params);
    let mut seq = model.new_seq();
    for chunk in prompt_tokens.chunks(prefill_chunk) {
        model.prefill_chunk(&mut seq, chunk)?;
    }
    let mut gpu_ids = Vec::new();
    let mut next = model.sample_last_logits(&mut sampler)?;
    gpu_ids.push(next);
    while gpu_ids.len() < max_tokens {
        next = model.step_and_sample(&mut seq, next, &mut sampler)?;
        gpu_ids.push(next);
    }
    model.release_seq(&mut seq);
    if gpu_ids != ids {
        bail!("GPU greedy sampling diverged from CPU argmax:\n  cpu: {ids:?}\n  gpu: {gpu_ids:?}");
    }
    eprintln!("gpu greedy path matches cpu argmax");

    let strs: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
    println!("[{}]", strs.join(", "));
    Ok(())
}
