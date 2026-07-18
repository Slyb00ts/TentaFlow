// ===== File: main.rs — `forge` CLI: serve an OpenAI API, run one-shot generation, benchmark =====

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use forge_engine::kv::KvQuant;
use forge_engine::model::ModelConfig;
use forge_engine::tier::{KvTierConfig, KvTierMode};
use forge_engine::sample::SamplingParams;
use forge_engine::server::{spawn_engine, EngineEvent, EngineHandle, EngineRequest};
use forge_formats::PoolingType;
use forge_hal::cuda::{CudaDevice, PoolSizes};
use forge_hal::Device;
use forge_server::source::{
    kv_pool_bytes, load_model, read_descriptor, resolve_normalize, resolve_pooling, LoadedModel,
};
use forge_server::toolcall::ToolParserKind;
use forge_server::{EmbedModel, ServerConfig, ServerState, SharedEmbed};
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
        /// Minimum simultaneously-decoding sequences before the batched forward
        /// path engages (below it the tuned fused single-seq path is faster).
        #[arg(long, default_value_t = 12, value_parser = clap::value_parser!(u16).range(2..))]
        batch_min: u16,
        /// Prompt tokens one sequence may prefill per scheduler iteration.
        #[arg(long, default_value_t = 16)]
        prefill_chunk: usize,
        /// KV cache pages (32 tokens each) shared by all sequences. Raised to
        /// at least one full `--ctx` window if smaller.
        #[arg(long, default_value_t = 512)]
        kv_pages: usize,
        /// Max context length per request in tokens (0 = the model's maximum).
        #[arg(long = "ctx", default_value_t = 0)]
        ctx: usize,
        /// Weights pool size in GiB.
        #[arg(long, default_value_t = 16.0)]
        weights_pool_gb: f64,
        /// Tool-call output parser: hermes | llama3 | none (default: auto-detect).
        #[arg(long)]
        tool_call_parser: Option<String>,
        /// Whisper HF snapshot directory enabling /v1/audio/transcriptions.
        #[arg(long)]
        whisper_model: Option<PathBuf>,
        /// GGUF file or HF snapshot enabling /v1/embeddings. When omitted and
        /// the served model is itself an embedding model, that model is reused.
        #[arg(long)]
        embed_model: Option<PathBuf>,
        /// KV cache mode: f16 | fp8 | rot4 | rot3 (fp8 halves KV bytes; rot4/rot3
        /// are rotational low-bit — rot4 recommended, rot3 lossier).
        #[arg(long = "kv-cache", default_value = "f16")]
        kv_cache: String,
        /// Rot modes: most-recent tokens kept at f16 fidelity (SPEC default 128).
        #[arg(long = "kv-residual-window", default_value_t = 128)]
        kv_residual_window: usize,
        /// Rot modes: context length past which a sequence uses the rotational
        /// store (SPEC default 4096).
        #[arg(long = "kv-activate-at", default_value_t = 4096)]
        kv_activate_at: usize,
        /// KV tiering: off | ram | nvme. Spills cold KV pages to pinned RAM
        /// (and NVMe) in 4-16 MB chunks, unlocking contexts beyond VRAM.
        #[arg(long = "kv-tier", default_value = "off")]
        kv_tier: String,
        /// NVMe spill directory (--kv-tier nvme). Default: a per-process
        /// directory under the system temp dir, removed on exit.
        #[arg(long = "kv-tier-dir")]
        kv_tier_dir: Option<PathBuf>,
        /// Pinned host RAM budget for warm KV chunks, in GiB.
        #[arg(long = "kv-tier-ram-gb", default_value_t = 8.0)]
        kv_tier_ram_gb: f64,
        /// Spill proactively when free VRAM KV pages fall below this fraction
        /// of the pool.
        #[arg(long = "kv-tier-watermark", default_value_t = 0.10)]
        kv_tier_watermark: f64,
    },
    /// Print an embedding vector for one text (dims + L2 norm + first values).
    Embed {
        /// GGUF file or HF snapshot directory of an embedding model.
        model_path: PathBuf,
        /// Text to embed.
        text: String,
        /// Pooling override: mean | cls | last (default: model metadata).
        #[arg(long)]
        pooling: Option<String>,
        /// Matryoshka truncation: keep the first N dimensions and renormalize.
        #[arg(long)]
        dimensions: Option<usize>,
        /// VRAM weights-pool size in GiB (0 = automatic split of free VRAM).
        #[arg(long = "weights-pool-gb", default_value_t = 0.0)]
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
        /// KV cache mode: f16 | fp8 | rot4 | rot3 (fp8 halves KV bytes; rot4/rot3
        /// are rotational low-bit — rot4 recommended, rot3 lossier).
        #[arg(long = "kv-cache", default_value = "f16")]
        kv_cache: String,
        /// Rot modes: most-recent tokens kept at f16 fidelity (SPEC default 128).
        #[arg(long = "kv-residual-window", default_value_t = 128)]
        kv_residual_window: usize,
        /// Rot modes: context length past which a sequence uses the rotational
        /// store (SPEC default 4096).
        #[arg(long = "kv-activate-at", default_value_t = 4096)]
        kv_activate_at: usize,
        /// Max context length in tokens (0 = the model's own maximum). Any
        /// value is honored; the KV cache is sized to fit it (VRAM permitting).
        #[arg(long = "ctx", default_value_t = 0)]
        ctx: usize,
        /// VRAM KV pages (32 tokens each; 0 = enough for the full --ctx
        /// window). With --kv-tier, a smaller pool caps the hot working set
        /// and the rest of the context spills to RAM/NVMe.
        #[arg(long = "kv-pages", default_value_t = 0)]
        kv_pages: usize,
        /// KV tiering: off | ram | nvme (see `forge serve --help`).
        #[arg(long = "kv-tier", default_value = "off")]
        kv_tier: String,
        /// NVMe spill directory (--kv-tier nvme).
        #[arg(long = "kv-tier-dir")]
        kv_tier_dir: Option<PathBuf>,
        /// Pinned host RAM budget for warm KV chunks, in GiB.
        #[arg(long = "kv-tier-ram-gb", default_value_t = 8.0)]
        kv_tier_ram_gb: f64,
        /// Spill proactively when free VRAM KV pages fall below this fraction
        /// of the pool.
        #[arg(long = "kv-tier-watermark", default_value_t = 0.10)]
        kv_tier_watermark: f64,
    },
    /// Transcribe a WAV file with a Whisper model.
    Transcribe {
        /// Whisper HF snapshot directory (safetensors + tokenizer + configs).
        model_dir: PathBuf,
        /// WAV file (PCM i16/i24/i32 or f32; resampled to 16 kHz mono).
        wav_path: PathBuf,
        /// Language code, e.g. pl | en | de (default: en).
        #[arg(long)]
        language: Option<String>,
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
        /// KV cache mode: f16 | fp8 | rot4 | rot3 (fp8 halves KV bytes; rot4/rot3
        /// are rotational low-bit — rot4 recommended, rot3 lossier).
        #[arg(long = "kv-cache", default_value = "f16")]
        kv_cache: String,
        /// Rot modes: most-recent tokens kept at f16 fidelity (SPEC default 128).
        #[arg(long = "kv-residual-window", default_value_t = 128)]
        kv_residual_window: usize,
        /// Rot modes: context length past which a sequence uses the rotational
        /// store (SPEC default 4096).
        #[arg(long = "kv-activate-at", default_value_t = 4096)]
        kv_activate_at: usize,
        /// Max context length in tokens (0 = the model's own maximum).
        #[arg(long = "ctx", default_value_t = 0)]
        ctx: usize,
        /// VRAM KV pages (32 tokens each; 0 = enough for the full context).
        #[arg(long = "kv-pages", default_value_t = 0)]
        kv_pages: usize,
        /// KV tiering: off | ram | nvme (see `forge serve --help`).
        #[arg(long = "kv-tier", default_value = "off")]
        kv_tier: String,
        /// NVMe spill directory (--kv-tier nvme).
        #[arg(long = "kv-tier-dir")]
        kv_tier_dir: Option<PathBuf>,
        /// Pinned host RAM budget for warm KV chunks, in GiB.
        #[arg(long = "kv-tier-ram-gb", default_value_t = 8.0)]
        kv_tier_ram_gb: f64,
        /// Spill proactively when free VRAM KV pages fall below this fraction
        /// of the pool.
        #[arg(long = "kv-tier-watermark", default_value_t = 0.10)]
        kv_tier_watermark: f64,
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
            batch_min,
            prefill_chunk,
            kv_pages,
            weights_pool_gb,
            tool_call_parser,
            whisper_model,
            embed_model,
            kv_cache,
            kv_residual_window,
            kv_activate_at,
            kv_tier,
            kv_tier_dir,
            kv_tier_ram_gb,
            kv_tier_watermark,
            ctx,
        } => cmd_serve(
            &model_path,
            bind,
            model_id,
            api_key,
            max_active,
            batch_min,
            prefill_chunk,
            kv_pages,
            weights_pool_gb,
            tool_call_parser,
            whisper_model.as_deref(),
            embed_model.as_deref(),
            parse_kv_quant(&kv_cache, kv_residual_window, kv_activate_at)?,
            parse_kv_tier(&kv_tier, kv_tier_dir, kv_tier_ram_gb, kv_tier_watermark)?,
            ctx,
        ),
        Command::Embed {
            model_path,
            text,
            pooling,
            dimensions,
            weights_pool_gb,
        } => cmd_embed(
            &model_path,
            &text,
            pooling.as_deref(),
            dimensions,
            weights_pool_gb,
        ),
        Command::Run {
            model_path,
            prompt,
            max_tokens,
            temperature,
            chat,
            weights_pool_gb,
            kv_cache,
            kv_residual_window,
            kv_activate_at,
            ctx,
            kv_pages,
            kv_tier,
            kv_tier_dir,
            kv_tier_ram_gb,
            kv_tier_watermark,
        } => cmd_run(
            &model_path,
            &prompt,
            max_tokens,
            temperature,
            chat,
            weights_pool_gb,
            parse_kv_quant(&kv_cache, kv_residual_window, kv_activate_at)?,
            parse_kv_tier(&kv_tier, kv_tier_dir, kv_tier_ram_gb, kv_tier_watermark)?,
            ctx,
            kv_pages,
        ),
        Command::Transcribe {
            model_dir,
            wav_path,
            language,
        } => cmd_transcribe(&model_dir, &wav_path, language.as_deref()),
        Command::Bench {
            model_path,
            tokens,
            prompt_tokens,
            kv_cache,
            kv_residual_window,
            kv_activate_at,
            ctx,
            kv_pages,
            kv_tier,
            kv_tier_dir,
            kv_tier_ram_gb,
            kv_tier_watermark,
        } => cmd_bench(
            &model_path,
            tokens,
            prompt_tokens,
            parse_kv_quant(&kv_cache, kv_residual_window, kv_activate_at)?,
            parse_kv_tier(&kv_tier, kv_tier_dir, kv_tier_ram_gb, kv_tier_watermark)?,
            ctx,
            kv_pages,
        ),
    }
}

fn parse_kv_quant(s: &str, residual_window: usize, activate_at: usize) -> Result<KvQuant> {
    match s {
        "f16" => Ok(KvQuant::F16),
        "fp8" => Ok(KvQuant::Fp8),
        "rot4" => Ok(KvQuant::Rot { bits: 4, residual_window, activate_at }),
        "rot3" => Ok(KvQuant::Rot { bits: 3, residual_window, activate_at }),
        other => bail!("unsupported --kv-cache '{other}' (expected f16 | fp8 | rot4 | rot3)"),
    }
}

fn parse_kv_tier(
    mode: &str,
    dir: Option<PathBuf>,
    ram_gb: f64,
    watermark: f64,
) -> Result<KvTierConfig> {
    let mode = match mode {
        "off" => KvTierMode::Off,
        "ram" => KvTierMode::Ram,
        "nvme" => KvTierMode::Nvme,
        other => bail!("unsupported --kv-tier '{other}' (expected off | ram | nvme)"),
    };
    if !(0.0..1.0).contains(&watermark) {
        bail!("--kv-tier-watermark must be in [0, 1), got {watermark}");
    }
    if ram_gb <= 0.0 {
        bail!("--kv-tier-ram-gb must be positive, got {ram_gb}");
    }
    Ok(KvTierConfig {
        mode,
        dir,
        ram_budget_bytes: (ram_gb * (1u64 << 30) as f64) as usize,
        watermark,
    })
}

/// VRAM bytes of the streamed-tier staging (two full-context single-layer
/// slabs); lives in the weights pool alongside the KV slabs.
fn tier_stage_bytes(
    desc: &forge_formats::ModelDescriptor,
    kv_page_size: usize,
    ctx_pages: usize,
    quant: KvQuant,
) -> usize {
    2 * ctx_pages * desc.params.n_kv_heads * kv_page_size * desc.params.head_dim
        * quant.slab_dtype().size()
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
    kv_quant: KvQuant,
    kv_tier: KvTierConfig,
    ctx: usize,
) -> Result<(LoadedModel, usize, usize)> {
    let kv_page_size = ModelConfig::default().kv_page_size;
    let desc = read_descriptor(path)?;
    // Without tiering the shared KV pool must hold at least one full-context
    // sequence; with tiering the pool is only the hot working set and the
    // rest of the window spills to RAM/NVMe.
    let (max_seq_len, ctx_pages) =
        resolve_ctx(desc.params.max_position_embeddings, ctx, kv_page_size);
    let kv_pages = if kv_tier.enabled() {
        kv_pages.min(ctx_pages)
    } else {
        kv_pages.max(ctx_pages)
    };
    let kv_pool = kv_pool_bytes(&desc, kv_page_size, kv_pages, kv_quant).max(1 << 30);
    let stage = if kv_tier.enabled() {
        tier_stage_bytes(&desc, kv_page_size, ctx_pages, kv_quant)
    } else {
        0
    };
    let activations = 1usize << 30;
    // Clamp the requested weights pool so weights + KV + activations always fit
    // free VRAM — a large --ctx grows KV and must not OOM the pool arenas.
    let free = CudaDevice::free_vram(0).context("query free VRAM")?;
    let weights_budget = free.saturating_sub(kv_pool + activations + (512 << 20));
    let weights = ((weights_pool_gb * (1u64 << 30) as f64) as usize + stage)
        .min(weights_budget)
        .max(1 << 30);
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
        kv_quant,
        kv_tier,
        max_seq_len,
    };
    let loaded = load_model(dev, path, cfg)?;
    Ok((loaded, kv_pages, max_seq_len))
}

/// Load a model for one-shot commands, sizing pools from free VRAM.
/// Resolve the usable context length and the KV page count that covers it.
/// `requested == 0` defaults to the model's own maximum; a non-zero request is
/// honored as-is (down to a single page, up to the model's positional limit).
/// Whether the resulting KV pool fits VRAM is decided later by pool sizing.
fn resolve_ctx(max_position_embeddings: usize, requested: usize, page_size: usize) -> (usize, usize) {
    let model_max = max_position_embeddings.max(page_size);
    let target = if requested == 0 {
        model_max
    } else {
        requested.min(model_max).max(1)
    };
    (target, target.div_ceil(page_size))
}

fn load_auto(
    path: &Path,
    weights_pool_gb: f64,
    kv_quant: KvQuant,
    kv_tier: KvTierConfig,
    ctx: usize,
    kv_pages_flag: usize,
) -> Result<LoadedModel> {
    // Read hyperparameters before loading weights so the KV cache is sized for
    // the requested context up front (its slabs are allocated during load).
    let desc = read_descriptor(path)?;
    let page_size = ModelConfig::default().kv_page_size;
    let (max_seq_len, ctx_pages) =
        resolve_ctx(desc.params.max_position_embeddings, ctx, page_size);
    // 0 = a pool covering the whole context (today's behavior); an explicit
    // count caps the hot VRAM working set (useful with --kv-tier).
    let kv_pages = if kv_pages_flag > 0 {
        kv_pages_flag.min(ctx_pages)
    } else {
        ctx_pages
    };
    let kv_pool = kv_pool_bytes(&desc, page_size, kv_pages, kv_quant).max(1 << 30);
    let stage = if kv_tier.enabled() {
        tier_stage_bytes(&desc, page_size, ctx_pages, kv_quant)
    } else {
        0
    };

    // Activations pool holds decode scratch + persistent decode buffers.
    let activations = 1usize << 30;
    let weights = if weights_pool_gb > 0.0 {
        (weights_pool_gb * (1u64 << 30) as f64) as usize + stage
    } else {
        // Give the KV cache its computed budget first, then the rest (minus a
        // safety margin) to weights, so a long context never starves the KV
        // arena the way a fixed 60/30/10 split could.
        let free = CudaDevice::free_vram(0).context("query free VRAM")?;
        free.saturating_sub(kv_pool + activations + (512 << 20))
            .max(1 << 30)
    };
    let device = CudaDevice::new(
        0,
        PoolSizes {
            weights,
            kv_cache: kv_pool,
            activations,
            kv_page_size: PoolSizes::DEFAULT_KV_PAGE,
        },
    )
    .context("create CUDA device")?;
    let dev: Arc<dyn Device> = device;
    load_model(
        dev,
        path,
        ModelConfig {
            kv_quant,
            kv_tier,
            kv_page_size: page_size,
            kv_pages,
            max_seq_len,
        },
    )
}

/// Load a Whisper model on its own CudaDevice with pools sized from the
/// snapshot: safetensors bytes bound the f16 upload (most tensors are f32,
/// halved on upload) and 1 GiB covers the persistent activation scratch.
/// A separate device instance keeps Whisper's weights-pool allocations away
/// from the LLM engine's pools.
fn load_whisper(dir: &Path) -> Result<forge_server::SharedWhisper> {
    let mut tensor_bytes: u64 = 0;
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        if entry.path().extension().is_some_and(|e| e == "safetensors") {
            tensor_bytes += entry.metadata()?.len();
        }
    }
    if tensor_bytes == 0 {
        bail!("no .safetensors files in {}", dir.display());
    }
    let device = CudaDevice::new(
        0,
        PoolSizes {
            weights: tensor_bytes as usize + (1 << 30),
            kv_cache: 4 << 20,
            activations: 32 << 20,
            kv_page_size: 256 << 10,
        },
    )
    .context("create CUDA device for whisper")?;
    let dev: Arc<dyn Device> = device;
    let model = forge_whisper::WhisperModel::load(dev, dir)
        .with_context(|| format!("load whisper model from {}", dir.display()))?;
    Ok(Arc::new(tokio::sync::Mutex::new(model)))
}

fn parse_pooling(s: &str) -> Result<PoolingType> {
    match s {
        "mean" => Ok(PoolingType::Mean),
        "cls" => Ok(PoolingType::Cls),
        "last" => Ok(PoolingType::Last),
        other => bail!("unsupported --pooling '{other}' (expected mean | cls | last)"),
    }
}

/// Load an embedding model on its own CudaDevice (like Whisper), resolving its
/// pooling and normalization from metadata. A `None` pooling degrades to mean.
/// Pools auto-size from free VRAM (the model loads after the chat engine has
/// already taken its own device pool).
fn load_embed(path: &Path, model_id: String) -> Result<SharedEmbed> {
    let device = CudaDevice::with_default_pools(0)
        .context("create CUDA device for embedding model")?;
    let dev: Arc<dyn Device> = device;
    let loaded = load_model(dev, path, ModelConfig::default())
        .with_context(|| format!("load embedding model from {}", path.display()))?;
    let params = &loaded.model.weights.descriptor.params;
    let dim = params.hidden_size;
    let max_context = params
        .max_position_embeddings
        .min(ModelConfig::default().max_seq_len);
    let pooling = match resolve_pooling(path, &loaded.model.weights.descriptor) {
        PoolingType::None => PoolingType::Mean,
        other => other,
    };
    let normalize = resolve_normalize(path);
    let tokenizer = Arc::new(loaded.bundle.tokenizer);
    Ok(Arc::new(EmbedModel {
        model: tokio::sync::Mutex::new(loaded.model),
        tokenizer,
        pooling,
        normalize,
        dim,
        max_context,
        model_id,
    }))
}

fn cmd_embed(
    model_path: &Path,
    text: &str,
    pooling_override: Option<&str>,
    dimensions: Option<usize>,
    weights_pool_gb: f64,
) -> Result<()> {
    let t0 = Instant::now();
    let loaded = load_auto(
        model_path,
        weights_pool_gb,
        KvQuant::F16,
        KvTierConfig::default(),
        8192,
        0,
    )?;
    eprintln!("model loaded in {:.1}s", t0.elapsed().as_secs_f32());

    let pooling = match pooling_override {
        Some(s) => parse_pooling(s)?,
        None => match resolve_pooling(model_path, &loaded.model.weights.descriptor) {
            PoolingType::None => PoolingType::Mean,
            other => other,
        },
    };
    let normalize = resolve_normalize(model_path);
    let tokenizer = loaded.bundle.tokenizer;
    let ids = tokenizer
        .encode(text, true)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut model = loaded.model;
    let mut v = model
        .embed(&ids, pooling, normalize)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if let Some(d) = dimensions {
        if d < v.len() {
            v.truncate(d);
            if normalize {
                forge_engine::model::l2_normalize(&mut v);
            }
        }
    }
    let l2 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    eprintln!(
        "tokens={} pooling={pooling:?} normalize={normalize} dim={} l2={l2:.6}",
        ids.len(),
        v.len()
    );
    let head: Vec<String> = v.iter().take(8).map(|x| format!("{x:.5}")).collect();
    println!("[{}, ...]", head.join(", "));
    Ok(())
}

fn cmd_transcribe(model_dir: &Path, wav_path: &Path, language: Option<&str>) -> Result<()> {
    let t0 = Instant::now();
    let whisper = load_whisper(model_dir)?;
    eprintln!("whisper model loaded in {:.1}s", t0.elapsed().as_secs_f32());
    let samples = forge_whisper::audio::load_wav(wav_path)
        .with_context(|| format!("load {}", wav_path.display()))?;
    let t1 = Instant::now();
    let mut model = whisper.try_lock().expect("freshly created mutex");
    let text = model.transcribe(&samples, language)?;
    eprintln!(
        "transcribed {:.1}s of audio in {:.2}s",
        samples.len() as f32 / 16_000.0,
        t1.elapsed().as_secs_f32()
    );
    println!("{text}");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_serve(
    model_path: &Path,
    bind: SocketAddr,
    model_id: Option<String>,
    api_key: Option<String>,
    max_active: u16,
    batch_min: u16,
    prefill_chunk: usize,
    kv_pages: usize,
    weights_pool_gb: f64,
    tool_call_parser: Option<String>,
    whisper_model: Option<&Path>,
    embed_model: Option<&Path>,
    kv_quant: KvQuant,
    kv_tier: KvTierConfig,
    ctx: usize,
) -> Result<()> {
    let max_active = usize::from(max_active);
    let t0 = Instant::now();
    let (loaded, kv_pages, max_seq_len) =
        load_for_serve(model_path, kv_pages, weights_pool_gb, kv_quant, kv_tier, ctx)?;
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
    // The engine caps a sequence at the configured context; the model's own
    // positional limit is already folded into max_seq_len by resolve_ctx.
    let max_context = max_seq_len;
    let tool_parser = ToolParserKind::resolve(
        tool_call_parser.as_deref(),
        &loaded.model.weights.descriptor.arch,
        &loaded.chat_template,
    )
    .map_err(|e| anyhow::anyhow!(e))?;
    tracing::info!("tool-call parser: {tool_parser:?}");
    let served_id = model_id.unwrap_or_else(|| default_model_id(model_path));
    // Detect whether the served model is itself an embedding model before its
    // weights move into the engine worker.
    let served_is_embed =
        resolve_pooling(model_path, &loaded.model.weights.descriptor) != PoolingType::None;
    let engine = forge_engine::server::spawn_engine_batched(
        loaded.model,
        tokenizer.clone(),
        max_active,
        prefill_chunk,
        batch_min as usize,
    );

    let whisper = match whisper_model {
        Some(dir) => {
            let t = Instant::now();
            let w = load_whisper(dir)?;
            tracing::info!(
                "whisper model {} loaded in {:.1}s",
                dir.display(),
                t.elapsed().as_secs_f32()
            );
            Some(w)
        }
        None => None,
    };

    // Embedding model: an explicit --embed-model, else the served model when
    // it is itself an embedding model (loaded a second time on its own device).
    let embed = match embed_model {
        Some(path) => {
            let t = Instant::now();
            let e = load_embed(path, default_model_id(path))?;
            tracing::info!(
                "embedding model {} loaded in {:.1}s (dim={}, pooling={:?})",
                path.display(),
                t.elapsed().as_secs_f32(),
                e.dim,
                e.pooling
            );
            Some(e)
        }
        None if served_is_embed => {
            let t = Instant::now();
            let e = load_embed(model_path, served_id.clone())?;
            tracing::info!(
                "served model is an embedding model; /v1/embeddings enabled in {:.1}s (dim={}, pooling={:?})",
                t.elapsed().as_secs_f32(),
                e.dim,
                e.pooling
            );
            Some(e)
        }
        None => None,
    };

    let cfg = ServerConfig {
        bind,
        model_id: served_id,
        api_key,
        tool_call_parser,
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
        tool_parser,
        whisper,
        embed,
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

#[allow(clippy::too_many_arguments)]
fn cmd_run(
    model_path: &Path,
    prompt: &str,
    max_tokens: usize,
    temperature: f32,
    chat: bool,
    weights_pool_gb: f64,
    kv_quant: KvQuant,
    kv_tier: KvTierConfig,
    ctx: usize,
    kv_pages: usize,
) -> Result<()> {
    let t0 = Instant::now();
    let loaded = load_auto(model_path, weights_pool_gb, kv_quant, kv_tier, ctx, kv_pages)?;
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

#[allow(clippy::too_many_arguments)]
fn cmd_bench(
    model_path: &Path,
    tokens: usize,
    prompt_tokens: usize,
    kv_quant: KvQuant,
    kv_tier: KvTierConfig,
    ctx: usize,
    kv_pages: usize,
) -> Result<()> {
    if tokens < 2 {
        bail!("--tokens must be at least 2 to measure decode throughput");
    }
    if prompt_tokens == 0 {
        bail!("--prompt-tokens must be at least 1");
    }
    let t0 = Instant::now();
    let loaded = load_auto(model_path, 0.0, kv_quant, kv_tier, ctx, kv_pages)?;
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

    // Single-sequence bench: full-size prefill chunks, no ITL to protect.
    let engine = spawn_engine(loaded.model, tokenizer, 1, forge_engine::model::MAX_PREFILL_CHUNK);
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
    // generated tokens as counted by the engine's usage numbers. Prefill runs
    // through the batched chunked path (one chunk per scheduler iteration).
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
