// ===== File: generate.rs — e2e smoke: load a model on the GPU and generate text =====
// Usage:
//   cargo run -p forge-engine --release --example generate -- <model.gguf|hf-dir> "prompt" [max_tokens]

use std::path::PathBuf;
use std::sync::Arc;

use forge_engine::generate::{generate, GenerateRequest, StreamEvent};
use forge_engine::model::{Model, ModelConfig};
use forge_engine::sample::SamplingParams;
use forge_hal::{PoolSizes, gpu};
use forge_hal::Device;
use forge_tokenize::Tokenizer;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(args.next().expect("model path"));
    let prompt = args
        .next()
        .unwrap_or_else(|| "The capital of France is".into());
    let max_tokens: usize = args.next().map(|s| s.parse().unwrap()).unwrap_or(48);

    let device = gpu::open(
        0,
        PoolSizes {
            weights: 14 << 30,
            kv_cache: 2 << 30,
            activations: 1 << 30,
            kv_page_size: 256 << 10,
        },
    )?;
    let dev: Arc<dyn Device> = device;

    let t0 = std::time::Instant::now();
    let (mut model, tokenizer) = if path.is_dir() {
        let model = Model::load_safetensors_dir(dev, &path, ModelConfig::default())?;
        let tokenizer = Tokenizer::from_file(path.join("tokenizer.json"))?;
        (model, tokenizer)
    } else {
        let gguf = forge_formats::Gguf::open(&path)?;
        let vocab = forge_engine::gguf_vocab::gguf_vocab(&gguf)?;
        drop(gguf);
        let tokenizer = Tokenizer::from_gguf_vocab(&vocab)?;
        let model = Model::load_gguf(dev, &path, ModelConfig::default())?;
        (model, tokenizer)
    };
    eprintln!("model loaded in {:.1}s", t0.elapsed().as_secs_f32());

    let p = &model.weights.descriptor.params;
    eprintln!(
        "arch={} layers={} hidden={} heads={}/{} head_dim={} vocab={}",
        model.weights.descriptor.arch,
        p.block_count,
        p.hidden_size,
        p.n_heads,
        p.n_kv_heads,
        p.head_dim,
        p.vocab_size
    );
    eprintln!(
        "fused layers: qkv {}/{} gate_up {}/{}",
        model.weights.fused_qkv_layers,
        p.block_count,
        model.weights.fused_gate_up_layers,
        p.block_count
    );

    let prompt_tokens = tokenizer.encode(&prompt, true)?;
    eprintln!("prompt tokens: {prompt_tokens:?}");

    let eos_ids = tokenizer.eos_id().into_iter().collect();
    let req = GenerateRequest {
        prompt_tokens,
        max_tokens,
        sampling: SamplingParams {
            temperature: 0.0,
            ..Default::default()
        },
        stop: vec![],
        eos_ids,
        grammar: None,
        ..Default::default()
    };

    let t1 = std::time::Instant::now();
    let out = generate(&mut model, &tokenizer, &req, |ev| {
        if let StreamEvent::Token { text, .. } = ev {
            print!("{text}");
            use std::io::Write;
            std::io::stdout().flush().ok();
        }
    })?;
    println!();
    eprintln!("generated ids: {:?}", out.tokens);
    let dt = t1.elapsed().as_secs_f32();
    eprintln!(
        "\n{} prompt + {} generated in {:.2}s ({:.1} tok/s), finish={:?}",
        out.prompt_tokens,
        out.tokens.len(),
        dt,
        (out.prompt_tokens + out.tokens.len()) as f32 / dt,
        out.finish
    );
    Ok(())
}
