// ===== File: golden_ids.rs — greedy token-id dump for bit-exactness gating =====
// Usage: golden_ids <model> <prompt> <max_tokens> [prefill_chunk]
// Drives Model::prefill_chunk + Model::step directly (no stream decoder, so
// no ids are hidden by empty text pieces) and prints the greedy ids.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Result};
use forge_engine::model::ModelConfig;
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
    let strs: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
    println!("[{}]", strs.join(", "));
    Ok(())
}
