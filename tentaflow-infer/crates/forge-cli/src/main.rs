// ===== File: main.rs — `forge` CLI: serve an OpenAI API, run one-shot generation, benchmark =====

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use forge_engine::model::ModelConfig;
use forge_engine::sample::SamplingParams;
use forge_engine::server::{spawn_engine, EngineEvent, EngineHandle, EngineRequest};
use forge_hal::cuda::{CudaDevice, PoolSizes};
use forge_hal::Device;
use forge_server::source::{kv_pool_bytes, load_model, read_descriptor, LoadedModel};
use forge_server::{ServerConfig, ServerState};
use forge_tokenize::{ChatMessage, ChatTemplateEngine};

#[derive(Parser)]
#[command(name = "forge", about = "FORGE inference engine CLI", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve an OpenAI-compatible HTTP API for one model.
    Serve {
        /// GGUF file or HF snapshot directory.
        model_path: PathBuf,
        #[arg(long, default_value = "0.0.0.0:8080")]
        bind: SocketAddr,
        /// Served model id; defaults to the file/directory name.
        #[arg(long)]
        model_id: Option<String>,
        /// Require `Authorization: Bearer <key>` on /v1/*.
        #[arg(long)]
        api_key: Option<String>,
        /// Max concurrently decoding sequences.
        #[arg(long, default_value_t = 8, value_parser = clap::value_parser!(u16).range(1..))]
        max_active: u16,
        /// Prompt tokens one sequence may prefill per scheduler iteration.
        #[arg(long, default_value_t = 16)]
        prefill_chunk: usize,
        /// KV cache pages (32 tokens each) shared by all sequences.
        #[arg(long, default_value_t = 512)]
        kv_pages: usize,
        /// Weights pool size in GiB.
        #[arg(long, default_value_t = 16.0)]
        weights_pool_gb: f64,
    },
    /// One-shot generation streamed to stdout.
    Run {
        /// GGUF file or HF snapshot directory.
        model_path: PathBuf,
        prompt: String,
        /// Max tokens to generate.
        #[arg(short = 'n', long = "max-tokens", default_value_t = 256)]
        max_tokens: usize,
        #[arg(long = "temp", default_value_t = 0.7)]
        temperature: f32,
        /// Wrap the prompt in the model's chat template as a user message.
        #[arg(long)]
        chat: bool,
        /// VRAM weights-pool size in GiB (0 = automatic split of free VRAM).
        #[arg(long = "weights-pool-gb", default_value_t = 0.0)]
        weights_pool_gb: f64,
    },
    /// Measure prefill and decode throughput.
    Bench {
        /// GGUF file or HF snapshot directory.
        model_path: PathBuf,
        /// Tokens to decode.
        #[arg(long, default_value_t = 128)]
        tokens: usize,
        /// Prompt length in tokens.
        #[arg(long, default_value_t = 512)]
        prompt_tokens: usize,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    match Cli::parse().command {
        Command::Serve {
            model_path,
            bind,
            model_id,
            api_key,
            max_active,
            prefill_chunk,
            kv_pages,
            weights_pool_gb,
        } => cmd_serve(
            &model_path,
            bind,
            model_id,
            api_key,
            max_active,
            prefill_chunk,
            kv_pages,
            weights_pool_gb,
        ),
        Command::Run {
            model_path,
            prompt,
            max_tokens,
            temperature,
            chat,
            weights_pool_gb,
        } => cmd_run(&model_path, &prompt, max_tokens, temperature, chat, weights_pool_gb),
        Command::Bench {
            model_path,
            tokens,
            prompt_tokens,
        } => cmd_bench(&model_path, tokens, prompt_tokens),
    }
}

fn default_model_id(path: &Path) -> String {
    path.file_stem()
        .or_else(|| path.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "model".into())
}

/// Load a model for the fixed-pool `serve` layout: weights sized by flag,
/// KV pool sized for exactly `kv_pages` pages of this model (floored at
/// 1 GiB), 1 GiB activations.
fn load_for_serve(
    path: &Path,
    kv_pages: usize,
    weights_pool_gb: f64,
) -> Result<(LoadedModel, usize)> {
    let kv_page_size = ModelConfig::default().kv_page_size;
    let desc = read_descriptor(path)?;
    let kv_pool = kv_pool_bytes(&desc, kv_page_size, kv_pages).max(1 << 30);
    let weights = (weights_pool_gb * (1u64 << 30) as f64) as usize;
    let device = CudaDevice::new(
        0,
        PoolSizes {
            weights,
            kv_cache: kv_pool,
            activations: 1 << 30,
            kv_page_size: PoolSizes::DEFAULT_KV_PAGE,
        },
    )
    .context("create CUDA device")?;
    let dev: Arc<dyn Device> = device;
    let cfg = ModelConfig {
        kv_page_size,
        kv_pages,
        ..ModelConfig::default()
    };
    let loaded = load_model(dev, path, cfg)?;
    Ok((loaded, kv_pages))
}

/// Load a model for one-shot commands, sizing pools from free VRAM.
fn load_auto(path: &Path, weights_pool_gb: f64) -> Result<LoadedModel> {
    let device = if weights_pool_gb > 0.0 {
        let weights = (weights_pool_gb * (1u64 << 30) as f64) as usize;
        CudaDevice::new(
            0,
            PoolSizes {
                weights,
                kv_cache: 1 << 30,
                activations: 1 << 30,
                kv_page_size: 256 << 10,
            },
        )
        .context("create CUDA device")?
    } else {
        CudaDevice::with_default_pools(0).context("create CUDA device")?
    };
    let dev: Arc<dyn Device> = device;
    load_model(dev, path, ModelConfig::default())
}

#[allow(clippy::too_many_arguments)]
fn cmd_serve(
    model_path: &Path,
    bind: SocketAddr,
    model_id: Option<String>,
    api_key: Option<String>,
    max_active: u16,
    prefill_chunk: usize,
    kv_pages: usize,
    weights_pool_gb: f64,
) -> Result<()> {
    let max_active = usize::from(max_active);
    let t0 = Instant::now();
    let (loaded, kv_pages) = load_for_serve(model_path, kv_pages, weights_pool_gb)?;
    tracing::info!(
        "loaded {} ({}) in {:.1}s: {} layers, kv_pages={}",
        model_path.display(),
        loaded.model.weights.descriptor.arch,
        t0.elapsed().as_secs_f32(),
        loaded.model.weights.descriptor.params.block_count,
        kv_pages,
    );

    let template_vars = loaded.bundle.template_vars();
    let eos_ids = loaded.bundle.eos_ids.clone();
    let tokenizer = Arc::new(loaded.bundle.tokenizer);
    // Per-request token budget: the engine caps a sequence at max_seq_len
    // pages; the model itself caps positional embeddings.
    let max_context = loaded
        .model
        .weights
        .descriptor
        .params
        .max_position_embeddings
        .min(ModelConfig::default().max_seq_len);
    let engine = spawn_engine(loaded.model, tokenizer.clone(), max_active, prefill_chunk);

    let cfg = ServerConfig {
        bind,
        model_id: model_id.unwrap_or_else(|| default_model_id(model_path)),
        api_key,
    };
    let state = ServerState::new(
        &cfg,
        engine,
        tokenizer,
        template_vars,
        eos_ids,
        loaded.chat_template,
        max_context,
        max_active,
    );

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(forge_server::serve(state, cfg.bind))
}

/// Drive one engine request to completion, invoking `on_text` per emitted
/// piece. Returns (generated_tokens, prompt_tokens, first_token_at, done_at).
/// Token counts come from the engine's `Done` usage, not from counting text
/// pieces. `first_token_at` is the first VISIBLE text event: the engine only
/// emits `Token` for non-empty decoded pieces, so UTF-8/stop-holdback in the
/// stream decoder can shift it a decode step or two past the true first
/// sampled token.
fn drain_request(
    engine: &EngineHandle,
    req: EngineRequest,
    mut on_text: impl FnMut(&str),
) -> Result<(usize, usize, Instant, Instant)> {
    let rx = engine.submit(req).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut first_token_at = None;
    loop {
        match rx.recv().context("engine stream ended unexpectedly")? {
            EngineEvent::Token { text, .. } => {
                first_token_at.get_or_insert_with(Instant::now);
                on_text(&text);
            }
            EngineEvent::Done {
                tokens,
                prompt_tokens,
                ..
            } => {
                let done_at = Instant::now();
                return Ok((
                    tokens,
                    prompt_tokens,
                    first_token_at.unwrap_or(done_at),
                    done_at,
                ));
            }
            EngineEvent::Error(msg) => bail!("engine error: {msg}"),
        }
    }
}

fn cmd_run(
    model_path: &Path,
    prompt: &str,
    max_tokens: usize,
    temperature: f32,
    chat: bool,
    weights_pool_gb: f64,
) -> Result<()> {
    let t0 = Instant::now();
    let loaded = load_auto(model_path, weights_pool_gb)?;
    eprintln!("model loaded in {:.1}s", t0.elapsed().as_secs_f32());

    let prompt_text = if chat {
        ChatTemplateEngine::new()
            .render(
                &loaded.chat_template,
                &[ChatMessage::text("user", prompt)],
                None,
                true,
                false,
                &loaded.bundle.template_vars(),
            )
            .map_err(|e| anyhow::anyhow!("chat template render failed: {e}"))?
    } else {
        prompt.to_string()
    };

    let eos_ids = loaded.bundle.eos_ids.clone();
    let tokenizer = Arc::new(loaded.bundle.tokenizer);
    let prompt_tokens = tokenizer
        .encode(&prompt_text, true)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let engine = spawn_engine(loaded.model, tokenizer, 1, 16);

    let submit_at = Instant::now();
    let (generated, prompt_len, _first, done_at) = drain_request(
        &engine,
        EngineRequest {
            prompt_tokens,
            max_tokens,
            sampling: SamplingParams {
                temperature,
                ..SamplingParams::default()
            },
            stop: vec![],
            eos_ids,
        },
        |piece| {
            use std::io::Write;
            print!("{piece}");
            std::io::stdout().flush().ok();
        },
    )?;
    println!();
    let dt = done_at.duration_since(submit_at).as_secs_f32();
    eprintln!(
        "{prompt_len} prompt + {generated} generated in {dt:.2}s ({:.1} tok/s overall)",
        (prompt_len + generated) as f32 / dt
    );
    Ok(())
}

fn cmd_bench(model_path: &Path, tokens: usize, prompt_tokens: usize) -> Result<()> {
    if tokens < 2 {
        bail!("--tokens must be at least 2 to measure decode throughput");
    }
    if prompt_tokens == 0 {
        bail!("--prompt-tokens must be at least 1");
    }
    let t0 = Instant::now();
    let loaded = load_auto(model_path, 0.0)?;
    eprintln!("model loaded in {:.1}s", t0.elapsed().as_secs_f32());

    let tokenizer = Arc::new(loaded.bundle.tokenizer);
    // Cycle a natural-language seed to the exact requested prompt length so
    // prefill cost is measured on realistic token ids.
    let seed_ids = tokenizer
        .encode(
            "Jednym z najważniejszych miast w historii Polski jest Kraków. ",
            false,
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let prompt_ids: Vec<u32> = seed_ids
        .iter()
        .cycle()
        .take(prompt_tokens)
        .copied()
        .collect();

    let engine = spawn_engine(loaded.model, tokenizer, 1, 16);
    let submit_at = Instant::now();
    // No EOS ids: the benchmark must decode exactly `tokens` tokens.
    let (generated, prompt_len, first_at, done_at) = drain_request(
        &engine,
        EngineRequest {
            prompt_tokens: prompt_ids,
            max_tokens: tokens,
            sampling: SamplingParams {
                temperature: 0.0,
                ..SamplingParams::default()
            },
            stop: vec![],
            eos_ids: vec![],
        },
        |_| {},
    )?;

    // Honest measurement note: "prefill" is submit → first visible token,
    // which includes at least one decode step (and possibly a couple more if
    // the decoder held back partial UTF-8); "decode" covers the remaining
    // generated tokens as counted by the engine's usage numbers.
    let prefill_s = first_at.duration_since(submit_at).as_secs_f64();
    let decode_s = done_at.duration_since(first_at).as_secs_f64();
    let prefill_tps = prompt_len as f64 / prefill_s.max(1e-9);
    let decode_tps = (generated.saturating_sub(1)) as f64 / decode_s.max(1e-9);

    println!("| phase   | tokens | seconds | tok/s   |");
    println!("|---------|--------|---------|---------|");
    println!("| prefill | {prompt_len:>6} | {prefill_s:>7.3} | {prefill_tps:>7.1} |");
    println!(
        "| decode  | {:>6} | {decode_s:>7.3} | {decode_tps:>7.1} |",
        generated.saturating_sub(1)
    );
    eprintln!("note: prefill is timed to the first visible token, so it includes >=1 decode step");
    Ok(())
}
