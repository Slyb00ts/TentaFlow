// ===== File: main.rs — `forge` CLI: serve an OpenAI API, run one-shot generation, benchmark =====

mod bench;
mod hf;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use forge_engine::kv::KvQuant;
use forge_engine::model::{ModelConfig, Nvfp4GgufLayout, MAX_SPEC_DRAFT};
use forge_engine::sample::SamplingParams;
use forge_engine::server::{
    BenchmarkTimings, EngineEvent, EngineHandle, EngineRequest, SpeculativeConfig,
};
use forge_engine::speculation::{ProposerKind, SpeculationKind};
use forge_engine::tier::{KvTierConfig, KvTierMode};
use forge_engine::weights::NvFp4CtLayoutPolicy;
use forge_formats::PoolingType;
use forge_hal::{gpu, PoolSizes};
use forge_hal::Device;
use forge_engine::model::Fp8PackOutcome;
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
    /// Download a model from the HuggingFace Hub (GGUF file or safetensors snapshot).
    Pull {
        /// HF repo id, e.g. `Qwen/Qwen3-0.6B-GGUF` or `bartowski/...`.
        repo: String,
        /// Specific GGUF file to fetch (required when a repo has multiple quants
        /// and no Q4_K_M default). Ignored for safetensors snapshots.
        #[arg(long)]
        file: Option<String>,
        /// Git revision (branch, tag, or commit).
        #[arg(long, default_value = "main")]
        revision: String,
        /// HuggingFace token for gated/private repos (else uses HF_TOKEN).
        #[arg(long)]
        token: Option<String>,
        /// Destination directory (default: XDG cache under forge/hub/<repo>).
        #[arg(long)]
        dir: Option<PathBuf>,
    },
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
        /// Maksymalna liczba równocześnie dekodowanych sekwencji.
        /// Domyślnie 1 dla MTP, w pozostałych trybach 8.
        #[arg(long, value_parser = clap::value_parser!(u16).range(1..))]
        max_active: Option<u16>,
        /// Minimum simultaneously-decoding sequences before the batched forward
        /// path engages. Default: automatic — 2 on models with small-batch
        /// decode kernels (NVFP4), else 12 (token-tile GEMM formats only
        /// amortize the flat tile cost at that concurrency).
        #[arg(long, value_parser = clap::value_parser!(u16).range(2..))]
        batch_min: Option<u16>,
        /// Prompt tokens one sequence may prefill per scheduler iteration
        /// (larger = better TTFT and throughput, smaller = better decode ITL
        /// of the other active sequences during a long prefill; measured C=16
        /// p1024 sweet spot is 1024 — 628 tok/s vs 606 @512 and 499 @256).
        #[arg(long, default_value_t = 1024)]
        prefill_chunk: usize,
        /// KV cache pages (32 tokens each) shared by all sequences. Raised to
        /// at least one full `--ctx` window if smaller.
        #[arg(long, default_value_t = 512)]
        kv_pages: usize,
        /// Max context length per request in tokens (0 = the model's maximum).
        #[arg(long = "ctx", default_value_t = 0)]
        ctx: usize,
        /// VRAM weights-pool size in GiB (0 = automatic split of free VRAM,
        /// which also leaves room for the auto fp8 prefill packs).
        #[arg(long, default_value_t = 0.0)]
        weights_pool_gb: f64,
        /// Wagi, które nie mieszczą się w VRAM, trafiają do pamięci przypiętej
        /// hosta — GPU czyta je wprost przez PCIe (~28 GB/s zamiast ~500 GB/s).
        /// Budżet w GiB; 0 wyłącza, więc brak miejsca w VRAM jest błędem.
        #[arg(long = "weight-host-gb", default_value_t = 0.0)]
        weight_host_gb: f64,
        /// Katalog na plik zrzutu wag ekspertów MoE. Eksperci, którzy nie
        /// zmieszczą się w VRAM ani w budżecie hosta, lądują tam i są
        /// stronicowani na żądanie. Bez tej opcji brak pamięci jest błędem.
        #[arg(long = "weight-spill-dir")]
        weight_spill_dir: Option<std::path::PathBuf>,
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
        /// KVFlash: fix the VRAM hot KV pool to N pages (32 tokens each)
        /// regardless of --ctx, streaming the rest of the context via the
        /// tier. VRAM stays constant as context grows. Requires --kv-tier
        /// ram|nvme. 0 = full-context VRAM pool (default behavior).
        #[arg(long = "kv-hot-pages", default_value_t = 0)]
        kv_hot_pages: usize,
        /// KVFlash shorthand: enable the NVMe tier and a small default hot
        /// pool (256 pages = 8k tokens) unless --kv-tier / --kv-hot-pages are
        /// set explicitly, so a single sequence's context is bounded by
        /// RAM+disk, not VRAM.
        #[arg(long = "kvflash", default_value_t = false)]
        kvflash: bool,
        /// Radix-tree prefix caching (SPEC §5.2): on | off. Dedups shared KV
        /// prefixes (system prompts, few-shot, multi-turn) so a request sharing
        /// a prefix skips re-prefilling it. Strict optimization; auto-inactive
        /// with tiering / rot KV / hybrid arch.
        #[arg(long = "prefix-cache", default_value = "on")]
        prefix_cache: String,
        /// Dekodowanie spekulatywne: off | on | ngram[:k] | mtp[:2|3]. `on`
        /// używa proposera n-gram. MTP wymaga greedy bez kar; `max-active > 1`
        /// przechodzi startup preflight pamięci. Spekulacja wyłącza prefix cache.
        #[arg(long = "speculative", default_value = "off")]
        speculative: String,
        /// Podział FFN na dodatkowe karty (tensor parallel): numery kart po
        /// przecinku, np. `--tp-cards 1`. Karta modelu jest zawsze pierwsza,
        /// wymienia się tylko te, które mają ją wesprzeć. Obejmuje dekodowanie
        /// modeli gęstych z wagami Q8_0; prefill zostaje na jednej karcie.
        #[arg(long = "tp-cards")]
        tp_cards: Option<String>,
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
        /// Wagi, które nie mieszczą się w VRAM, trafiają do pamięci przypiętej
        /// hosta — GPU czyta je wprost przez PCIe (~28 GB/s zamiast ~500 GB/s).
        /// Budżet w GiB; 0 wyłącza, więc brak miejsca w VRAM jest błędem.
        #[arg(long = "weight-host-gb", default_value_t = 0.0)]
        weight_host_gb: f64,
        /// Katalog na plik zrzutu wag ekspertów MoE. Eksperci, którzy nie
        /// zmieszczą się w VRAM ani w budżecie hosta, lądują tam i są
        /// stronicowani na żądanie. Bez tej opcji brak pamięci jest błędem.
        #[arg(long = "weight-spill-dir")]
        weight_spill_dir: Option<std::path::PathBuf>,
    },
    /// One-shot generation streamed to stdout.
    Run {
        /// GGUF file or HF snapshot directory.
        model_path: PathBuf,
        prompt: String,
        /// Max tokens to generate.
        #[arg(short = 'n', long = "max-tokens", default_value_t = 256)]
        max_tokens: usize,
        /// Temperatura samplingu. Bez tej flagi bierze się z profilu modelu
        /// (`forge-engine/src/model_profile/`).
        #[arg(long = "temp")]
        temperature: Option<f32>,
        /// Owija prompt w szablon czatu modelu. Bez tej flagi decyduje profil
        /// modelu; `--no-chat` wymusza surowy prompt.
        #[arg(long, overrides_with = "no_chat")]
        chat: bool,
        #[arg(long = "no-chat", overrides_with = "chat")]
        no_chat: bool,
        /// VRAM weights-pool size in GiB (0 = automatic split of free VRAM).
        #[arg(long = "weights-pool-gb", default_value_t = 0.0)]
        weights_pool_gb: f64,
        /// Wagi, które nie mieszczą się w VRAM, trafiają do pamięci przypiętej
        /// hosta — GPU czyta je wprost przez PCIe (~28 GB/s zamiast ~500 GB/s).
        /// Budżet w GiB; 0 wyłącza, więc brak miejsca w VRAM jest błędem.
        #[arg(long = "weight-host-gb", default_value_t = 0.0)]
        weight_host_gb: f64,
        /// Katalog na plik zrzutu wag ekspertów MoE. Eksperci, którzy nie
        /// zmieszczą się w VRAM ani w budżecie hosta, lądują tam i są
        /// stronicowani na żądanie. Bez tej opcji brak pamięci jest błędem.
        #[arg(long = "weight-spill-dir")]
        weight_spill_dir: Option<std::path::PathBuf>,
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
        /// KVFlash: fix the VRAM hot KV pool to N pages (32 tokens each)
        /// regardless of --ctx, streaming the rest of the context via the
        /// tier. VRAM stays constant as context grows. Requires --kv-tier
        /// ram|nvme. 0 = full-context VRAM pool (default behavior).
        #[arg(long = "kv-hot-pages", default_value_t = 0)]
        kv_hot_pages: usize,
        /// KVFlash shorthand: enable the NVMe tier and a small default hot
        /// pool (256 pages = 8k tokens) unless --kv-tier / --kv-hot-pages are
        /// set explicitly.
        #[arg(long = "kvflash", default_value_t = false)]
        kvflash: bool,
        /// Radix-tree prefix caching (SPEC §5.2): on | off (see `forge serve`).
        #[arg(long = "prefix-cache", default_value = "on")]
        prefix_cache: String,
        /// Dekodowanie spekulatywne: off | on | ngram[:k] | mtp[:2|3].
        /// Włączenie wymusza `--prefix-cache off`.
        #[arg(long = "speculative", default_value = "off")]
        speculative: String,
        /// Podział FFN na dodatkowe karty (tensor parallel): numery kart po
        /// przecinku, np. `--tp-cards 1`. Karta modelu jest zawsze pierwsza,
        /// wymienia się tylko te, które mają ją wesprzeć. Obejmuje dekodowanie
        /// modeli gęstych z wagami Q8_0; prefill zostaje na jednej karcie.
        #[arg(long = "tp-cards")]
        tp_cards: Option<String>,
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
    /// Load an ONNX model, print its op coverage, and (for Silero VAD) run it
    /// on the GPU for a synthesized audio frame.
    OnnxRun {
        /// ONNX model file (opset 17+ subset).
        model_path: PathBuf,
        /// Samples in the synthesized frame (Silero VAD 16 kHz expects 512).
        #[arg(long, default_value_t = 512)]
        samples: usize,
        /// Sample rate passed to the model's `sr` input.
        #[arg(long, default_value_t = 16000)]
        sr: i64,
        /// Test signal for the frame: sine | zero | ones.
        #[arg(long, default_value = "sine")]
        signal: String,
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
        /// Dokładne tokeny promptu jako kolejne little-endian `u32`.
        #[arg(long = "prompt-token-ids")]
        prompt_token_ids: Option<PathBuf>,
        /// Rozmiar puli wag w GiB; 0 dobiera pulę z aktualnie wolnego VRAM.
        #[arg(long = "weights-pool-gb", default_value_t = 0.0)]
        weights_pool_gb: f64,
        /// Wagi, które nie mieszczą się w VRAM, trafiają do pamięci przypiętej
        /// hosta — GPU czyta je wprost przez PCIe (~28 GB/s zamiast ~500 GB/s).
        /// Budżet w GiB; 0 wyłącza, więc brak miejsca w VRAM jest błędem.
        #[arg(long = "weight-host-gb", default_value_t = 0.0)]
        weight_host_gb: f64,
        /// Katalog na plik zrzutu wag ekspertów MoE. Eksperci, którzy nie
        /// zmieszczą się w VRAM ani w budżecie hosta, lądują tam i są
        /// stronicowani na żądanie. Bez tej opcji brak pamięci jest błędem.
        #[arg(long = "weight-spill-dir")]
        weight_spill_dir: Option<std::path::PathBuf>,
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
        /// KVFlash: fix the VRAM hot KV pool to N pages (32 tokens each)
        /// regardless of --ctx, streaming the rest of the context via the
        /// tier. Requires --kv-tier ram|nvme. 0 = full-context VRAM pool.
        #[arg(long = "kv-hot-pages", default_value_t = 0)]
        kv_hot_pages: usize,
        /// KVFlash shorthand: enable the NVMe tier and a small default hot
        /// pool (256 pages = 8k tokens) unless --kv-tier / --kv-hot-pages are
        /// set explicitly.
        #[arg(long = "kvflash", default_value_t = false)]
        kvflash: bool,
        /// Radix-tree prefix caching (SPEC §5.2): on | off. Domyślnie OFF,
        /// inaczej niż w `serve`: kolejne powtórzenia trafiałyby w cache i
        /// przeliczały tylko rozbieżny ogon promptu, więc raportowany prefill
        /// byłby zawyżony. Trafienie w cache kończy benchmark błędem.
        #[arg(long = "prefix-cache", default_value = "off")]
        prefix_cache: String,
        /// Dekodowanie spekulatywne: off | on | ngram[:k] | mtp[:2|3].
        /// Włączenie wymusza `--prefix-cache off`.
        #[arg(long = "speculative", default_value = "off")]
        speculative: String,
        /// Liczba mierzonych prób po jednym osobnym przebiegu rozgrzewającym.
        #[arg(long = "reps", default_value_t = 5)]
        reps: usize,
        /// Podział FFN na dodatkowe karty (tensor parallel): numery kart po
        /// przecinku, np. `--tp-cards 1`.
        #[arg(long = "tp-cards")]
        tp_cards: Option<String>,
        /// Narzucony podział wymiaru pośredniego, np. `8704,8704`. Bez tego
        /// podział idzie ze zmierzonej mocy kart, co dla kart tej samej
        /// architektury bywa obarczone błędem samego pomiaru.
        #[arg(long = "tp-split")]
        tp_split: Option<String>,
    },
    /// Measure next-token perplexity on a fixed held-out passage (W4A8 quality
    /// gate). Runs whatever GEMM `FORGE_GEMM` selects (W4A8 is calibrated first).
    Ppl {
        /// GGUF file or HF snapshot directory.
        model_path: PathBuf,
        /// Max context length in tokens (0 = the model's own maximum).
        #[arg(long = "ctx", default_value_t = 0)]
        ctx: usize,
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
        Command::Pull {
            repo,
            file,
            revision,
            token,
            dir,
        } => cmd_pull(repo, file, revision, token, dir),
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
            weight_host_gb,
            weight_spill_dir,
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
            kv_hot_pages,
            kvflash,
            prefix_cache,
            speculative,
            tp_cards,
        } => {
            let (hot_pages, tier) = resolve_kvflash(
                kvflash,
                kv_hot_pages,
                &kv_tier,
                kv_tier_dir,
                kv_tier_ram_gb,
                kv_tier_watermark,
            )?;
            cmd_serve(
                &model_path,
                bind,
                model_id,
                api_key,
                max_active,
                batch_min,
                prefill_chunk,
                kv_pages,
                weights_pool_gb,
                weight_host_gb,
                weight_spill_dir.clone(),
                tool_call_parser,
                whisper_model.as_deref(),
                embed_model.as_deref(),
                parse_kv_quant(&kv_cache, kv_residual_window, kv_activate_at)?,
                tier,
                ctx,
                hot_pages,
                parse_prefix_cache(&prefix_cache)?,
                parse_speculative(&speculative)?,
                parse_tp_cards(tp_cards.as_deref())?,
            )
        }
        Command::Embed {
            model_path,
            text,
            pooling,
            dimensions,
            weights_pool_gb,
            weight_host_gb,
            weight_spill_dir,
        } => cmd_embed(
            &model_path,
            &text,
            pooling.as_deref(),
            dimensions,
            weights_pool_gb,
            weight_host_gb,
            weight_spill_dir,
        ),
        Command::Run {
            model_path,
            prompt,
            max_tokens,
            temperature,
            chat,
            no_chat,
            weights_pool_gb,
            weight_host_gb,
            weight_spill_dir,
            kv_cache,
            kv_residual_window,
            kv_activate_at,
            ctx,
            kv_pages,
            kv_tier,
            kv_tier_dir,
            kv_tier_ram_gb,
            kv_tier_watermark,
            kv_hot_pages,
            kvflash,
            prefix_cache,
            speculative,
            tp_cards,
        } => {
            let (hot_pages, tier) = resolve_kvflash(
                kvflash,
                kv_hot_pages,
                &kv_tier,
                kv_tier_dir,
                kv_tier_ram_gb,
                kv_tier_watermark,
            )?;
            cmd_run(
                &model_path,
                &prompt,
                max_tokens,
                temperature,
                chat,
                no_chat,
                weights_pool_gb,
                weight_host_gb,
                weight_spill_dir.clone(),
                parse_kv_quant(&kv_cache, kv_residual_window, kv_activate_at)?,
                tier,
                ctx,
                kv_pages,
                hot_pages,
                parse_prefix_cache(&prefix_cache)?,
                parse_speculative(&speculative)?,
                parse_tp_cards(tp_cards.as_deref())?,
            )
        }
        Command::Transcribe {
            model_dir,
            wav_path,
            language,
        } => cmd_transcribe(&model_dir, &wav_path, language.as_deref()),
        Command::OnnxRun {
            model_path,
            samples,
            sr,
            signal,
        } => cmd_onnx_run(&model_path, samples, sr, &signal),
        Command::Bench {
            model_path,
            tokens,
            prompt_tokens,
            prompt_token_ids,
            weights_pool_gb,
            weight_host_gb,
            weight_spill_dir,
            kv_cache,
            kv_residual_window,
            kv_activate_at,
            ctx,
            kv_pages,
            kv_tier,
            kv_tier_dir,
            kv_tier_ram_gb,
            kv_tier_watermark,
            kv_hot_pages,
            kvflash,
            prefix_cache,
            speculative,
            reps,
            tp_cards,
            tp_split,
        } => {
            let (hot_pages, tier) = resolve_kvflash(
                kvflash,
                kv_hot_pages,
                &kv_tier,
                kv_tier_dir,
                kv_tier_ram_gb,
                kv_tier_watermark,
            )?;
            cmd_bench(
                &model_path,
                tokens,
                prompt_tokens,
                prompt_token_ids.as_deref(),
                weights_pool_gb,
                weight_host_gb,
                weight_spill_dir.clone(),
                parse_kv_quant(&kv_cache, kv_residual_window, kv_activate_at)?,
                tier,
                ctx,
                kv_pages,
                hot_pages,
                parse_prefix_cache(&prefix_cache)?,
                parse_speculative(&speculative)?,
                reps,
                parse_tp_cards(tp_cards.as_deref())?,
                parse_tp_split(tp_split.as_deref())?,
            )
        }
        Command::Ppl { model_path, ctx } => cmd_ppl(&model_path, ctx),
    }
}

/// Download a model from the HuggingFace Hub and print the path to pass to
/// `forge run` / `forge serve`. Runs the async downloader on a lightweight
/// current-thread runtime (no GPU / engine needed).
fn cmd_pull(
    repo: String,
    file: Option<String>,
    revision: String,
    token: Option<String>,
    dir: Option<PathBuf>,
) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build download runtime")?;
    let path = rt.block_on(hf::pull(repo, file, revision, token, dir))?;
    eprintln!("done. run it with:");
    println!("{}", path.display());
    Ok(())
}

fn parse_kv_quant(s: &str, residual_window: usize, activate_at: usize) -> Result<KvQuant> {
    match s {
        "f16" => Ok(KvQuant::F16),
        "fp8" => Ok(KvQuant::Fp8),
        "rot4" => Ok(KvQuant::Rot {
            bits: 4,
            residual_window,
            activate_at,
        }),
        "rot3" => Ok(KvQuant::Rot {
            bits: 3,
            residual_window,
            activate_at,
        }),
        other => bail!("unsupported --kv-cache '{other}' (expected f16 | fp8 | rot4 | rot3)"),
    }
}

/// Parsuje `--speculative`: `off` | `on` | `ngram[:k]` | `mtp[:2|3]` | `mtp+ngram:2|3`.
fn parse_speculative(s: &str) -> Result<SpeculativeConfig> {
    // The verify forward runs the ungraphed prefill path, so a long draft
    // (amortizing its per-op launch overhead over many accepted tokens) is what
    // makes it a net win; 16 measured best on qwen3-0.6b.
    const DEFAULT_DRAFT: usize = 16;
    if s == "off" {
        return Ok(SpeculativeConfig::off());
    }
    let explicit_budget = s.contains(':');
    let (name, budget) = match s.split_once(':') {
        Some((name, raw_budget)) => {
            let budget = raw_budget
                .parse::<usize>()
                .with_context(|| format!("invalid draft budget in --speculative '{s}'"))?;
            if !(1..=MAX_SPEC_DRAFT).contains(&budget) {
                bail!("--speculative draft budget must be in 1..={MAX_SPEC_DRAFT}");
            }
            (name, budget)
        }
        None => (
            s,
            if matches!(s, "mtp" | "mtp+ngram") {
                3
            } else {
                DEFAULT_DRAFT
            },
        ),
    };
    match name {
        "on" if explicit_budget => {
            bail!("--speculative 'on' does not accept a draft budget; use ngram:<k>")
        }
        "on" | "ngram" => SpeculativeConfig::ngram(budget).map_err(Into::into),
        "mtp" => SpeculativeConfig::chain(vec![ProposerKind::Mtp], budget).map_err(Into::into),
        "mtp+ngram" => SpeculativeConfig::chain(
            vec![ProposerKind::Mtp, ProposerKind::Ngram],
            budget,
        )
        .map_err(Into::into),
        "draft-model" | "eagle" | "dflash" | "dspark" => bail!(
            "--speculative proposer '{name}' requires an implemented neural loader and forge-speculation.json"
        ),
        other => bail!("unsupported --speculative proposer '{other}'"),
    }
}

fn resolve_max_active(
    max_active: Option<u16>,
    spec: &SpeculativeConfig,
    hybrid_model: bool,
) -> Result<usize> {
    let max_active = usize::from(max_active.unwrap_or_else(|| {
        if hybrid_model
            || matches!(
                spec.kind(),
                SpeculationKind::NativeMtp | SpeculationKind::NativeMtpNgram
            )
        {
            1
        } else {
            8
        }
    }));
    if max_active == 0 {
        bail!("--max-active musi być większe od zera");
    }
    Ok(max_active)
}

fn resolve_bench_nvfp4_gguf_layout_from_env(
    speculation: SpeculationKind,
    max_active: usize,
) -> Result<Nvfp4GgufLayout> {
    let value = match std::env::var("FORGE_BENCH_NVFP4_TILE") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => "0".into(),
        Err(std::env::VarError::NotUnicode(_)) => {
            bail!("FORGE_BENCH_NVFP4_TILE musi być poprawnym UTF-8 i mieć wartość 0 albo 1")
        }
    };
    resolve_bench_nvfp4_gguf_layout(speculation, max_active, &value)
}

fn resolve_bench_nvfp4_gguf_layout(
    speculation: SpeculationKind,
    max_active: usize,
    value: &str,
) -> Result<Nvfp4GgufLayout> {
    match value {
        "0" => Ok(Nvfp4GgufLayout::RowMajor36),
        "1" if speculation != SpeculationKind::Off => {
            bail!("FORGE_BENCH_NVFP4_TILE=1 wymaga --speculative off")
        }
        "1" if max_active != 1 => {
            bail!("FORGE_BENCH_NVFP4_TILE=1 wymaga dokładnie jednej aktywnej sekwencji")
        }
        "1" => Ok(Nvfp4GgufLayout::TileN128K64),
        _ => bail!("FORGE_BENCH_NVFP4_TILE musi mieć wartość 0 albo 1"),
    }
}

/// Włącza podział FFN, gdy operator wskazał dodatkowe karty.
///
/// Pula wag kart pomocniczych jest tej samej wielkości co pula karty modelu:
/// fragment jest mniejszy od całych wag, więc to margines, nie wymaganie.
fn enable_tp_ffn(
    model: &mut forge_engine::model::Model,
    model_path: &Path,
    cards: &[forge_hal::gpu::DeviceId],
    weights_pool_gb: f64,
) -> Result<()> {
    enable_tp_ffn_with(model, model_path, cards, weights_pool_gb, None)
}

fn enable_tp_ffn_with(
    model: &mut forge_engine::model::Model,
    model_path: &Path,
    cards: &[forge_hal::gpu::DeviceId],
    weights_pool_gb: f64,
    forced: Option<&[usize]>,
) -> Result<()> {
    if cards.is_empty() {
        return Ok(());
    }
    // Karta wspierająca trzyma WYŁĄCZNIE fragmenty FFN i garść buforów roboczych
    // — żadnego KV, bo uwaga zostaje na karcie modelu. Domyślna pula z
    // `--weights-pool-gb 0` to jeden gibibajt, czyli mniej niż połowa FFN
    // dwudziestosiedmiomiliardowego modelu, więc bez tego podział kończy się
    // brakiem pamięci przy pierwszej warstwie.
    let weights = if weights_pool_gb > 0.0 {
        (weights_pool_gb * (1u64 << 30) as f64) as usize
    } else {
        let mut free = usize::MAX;
        for card in cards {
            free = free.min(forge_hal::gpu::free_vram(card.ordinal).with_context(|| {
                format!("odczyt wolnego VRAM karty {}", card.ordinal)
            })?);
        }
        free / 10 * 8
    };
    let pools = forge_hal::PoolSizes {
        weights,
        kv_cache: forge_hal::PoolSizes::DEFAULT_KV_PAGE,
        activations: 256 << 20,
        kv_page_size: forge_hal::PoolSizes::DEFAULT_KV_PAGE,
    };
    model.enable_tp_ffn(model_path, cards, pools, None, forced)?;
    let tp = model.tp_ffn().expect("podział właśnie włączony");
    eprintln!(
        "podział FFN na {} kart (P2P {}): {:?} wierszy pośrednich warstwy 0",
        tp.cards(),
        tp.peer_access(),
        tp.split_of(0)
    );
    Ok(())
}

/// Narzucony podział wymiaru pośredniego, np. `8704,8704`.
fn parse_tp_split(spec: Option<&str>) -> Result<Option<Vec<usize>>> {
    let Some(spec) = spec else { return Ok(None) };
    let mut out = Vec::new();
    for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        out.push(
            part.parse::<usize>()
                .map_err(|_| anyhow::anyhow!("--tp-split: `{part}` nie jest liczbą kolumn"))?,
        );
    }
    if out.len() < 2 {
        return Err(anyhow::anyhow!("--tp-split wymaga udziału dla każdej karty"));
    }
    Ok(Some(out))
}

/// Numery kart do podziału FFN, np. `1` albo `1,2`. `None` znaczy jedną kartę.
fn parse_tp_cards(spec: Option<&str>) -> Result<Vec<forge_hal::gpu::DeviceId>> {
    let Some(spec) = spec else {
        return Ok(Vec::new());
    };
    let visible = forge_hal::gpu::enumerate();
    let mut out = Vec::new();
    for part in spec.split(',') {
        let index: usize = part
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("--tp-cards: `{part}` nie jest numerem karty"))?;
        let id = *visible.get(index).ok_or_else(|| {
            anyhow::anyhow!(
                "--tp-cards: nie ma karty {index}, widocznych jest {}",
                visible.len()
            )
        })?;
        if out.contains(&id) {
            return Err(anyhow::anyhow!("--tp-cards: karta {index} podana dwa razy"));
        }
        out.push(id);
    }
    Ok(out)
}

fn parse_prefix_cache(s: &str) -> Result<bool> {
    match s {
        "on" => Ok(true),
        "off" => Ok(false),
        other => bail!("unsupported --prefix-cache '{other}' (expected on | off)"),
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

/// KVFlash default hot pool when `--kvflash` is given without an explicit
/// `--kv-hot-pages`: 256 pages × 32 tokens = 8k tokens kept in VRAM, the rest
/// of the context streaming from RAM/NVMe.
const KVFLASH_DEFAULT_HOT_PAGES: usize = 256;

/// Resolve the KVFlash hot-pool + tier settings shared by serve/run/bench.
/// `--kvflash` is shorthand: enable the NVMe tier and a small default hot pool
/// unless the user set `--kv-tier` / `--kv-hot-pages` explicitly. Returns the
/// effective hot-page count (0 = full-context VRAM pool, today's behavior) and
/// the parsed tier config. A fixed hot pool needs somewhere to spill the rest
/// of the context, so a non-zero hot pool without a tier is a hard error.
fn resolve_kvflash(
    kvflash: bool,
    kv_hot_pages: usize,
    tier_mode: &str,
    tier_dir: Option<PathBuf>,
    tier_ram_gb: f64,
    tier_watermark: f64,
) -> Result<(usize, KvTierConfig)> {
    let tier_mode = if kvflash && tier_mode == "off" {
        "nvme"
    } else {
        tier_mode
    };
    let hot_pages = if kvflash && kv_hot_pages == 0 {
        KVFLASH_DEFAULT_HOT_PAGES
    } else {
        kv_hot_pages
    };
    let tier = parse_kv_tier(tier_mode, tier_dir, tier_ram_gb, tier_watermark)?;
    if hot_pages > 0 {
        if !tier.enabled() {
            bail!(
                "--kv-hot-pages requires --kv-tier ram|nvme (a fixed VRAM hot pool needs a \
                 tier to spill the rest of the context to; use --kvflash for the NVMe default)"
            );
        }
        let floor = forge_engine::tier::min_resident_pages(ModelConfig::default().kv_page_size);
        if hot_pages < floor {
            bail!(
                "--kv-hot-pages {hot_pages} is below the engine minimum residency of {floor} \
                 pages (one prefill chunk + hot tail); raise it so a sequence never deadlocks"
            );
        }
    }
    Ok((hot_pages, tier))
}

/// VRAM bytes of the streamed-tier staging (two full-context single-layer
/// slabs); lives in the weights pool alongside the KV slabs.
fn tier_stage_bytes(
    desc: &forge_formats::ModelDescriptor,
    kv_page_size: usize,
    ctx_pages: usize,
    quant: KvQuant,
) -> Result<usize> {
    2usize
        .checked_mul(ctx_pages)
        .and_then(|value| value.checked_mul(desc.params.kv_cache_heads()))
        .and_then(|value| value.checked_mul(kv_page_size))
        .and_then(|value| value.checked_mul(desc.params.kv_cache_head_dim()))
        .and_then(|value| value.checked_mul(quant.slab_dtype().size()))
        .context("rozmiar bufora staging KV przekracza zakres usize")
}

fn default_model_id(path: &Path) -> String {
    path.file_stem()
        .or_else(|| path.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "model".into())
}

#[derive(Debug, Eq, PartialEq)]
struct KvPoolLayout {
    pages: usize,
    bytes: usize,
}

#[allow(clippy::too_many_arguments)]
fn resolve_kv_pool_layout(
    desc: &forge_formats::ModelDescriptor,
    page_size: usize,
    ctx_pages: usize,
    requested_pages: usize,
    hot_pages: usize,
    tier: &KvTierConfig,
    quant: KvQuant,
    native_mtp: bool,
) -> Result<KvPoolLayout> {
    if hot_pages > 0 && !tier.enabled() {
        bail!("--kv-hot-pages wymaga --kv-tier ram|nvme");
    }
    let pages = if hot_pages > 0 {
        hot_pages
    } else if tier.enabled() {
        if requested_pages == 0 {
            ctx_pages
        } else {
            requested_pages.min(ctx_pages)
        }
    } else if requested_pages == 0 {
        ctx_pages
    } else {
        requested_pages.max(ctx_pages)
    };
    if tier.enabled() {
        let floor = forge_engine::tier::min_resident_pages(page_size);
        if pages < floor {
            bail!(
                "efektywna pula KV ma {pages} stron, poniżej minimum rezydencji {floor}; zwiększ --kv-pages lub --kv-hot-pages"
            );
        }
    }
    Ok(KvPoolLayout {
        pages,
        bytes: kv_pool_bytes(desc, page_size, pages, quant, native_mtp)?,
    })
}

fn kv_full_context_admission_capacity(
    pages: usize,
    ctx_pages: usize,
    page_size: usize,
    tier_enabled: bool,
    max_active: usize,
) -> usize {
    let pages_per_request = if tier_enabled {
        ctx_pages.min(forge_engine::tier::min_resident_pages(page_size))
    } else {
        ctx_pages
    };
    if pages_per_request == 0 {
        return 0;
    }
    max_active.min(pages / pages_per_request)
}

fn pool_reserve_bytes(kv_bytes: usize, activation_bytes: usize) -> Result<usize> {
    kv_bytes
        .checked_add(activation_bytes)
        .and_then(|value| value.checked_add(512 << 20))
        .context("suma pul KV, aktywacji i rezerwy przekracza zakres usize")
}

/// Ładuje model w stałym układzie pul `serve`.
#[allow(clippy::too_many_arguments)]
fn activation_pool_bytes(_native_mtp: bool, hybrid: bool) -> usize {
    if hybrid {
        9usize << 27
    } else {
        1usize << 30
    }
}

fn resolve_nvfp4_ct_layout(value: &str) -> Result<NvFp4CtLayoutPolicy> {
    match value {
        "row" => Ok(NvFp4CtLayoutPolicy::RowMajorE4M3),
        "s0" => Ok(NvFp4CtLayoutPolicy::S0N64K128),
        "auto" => Ok(NvFp4CtLayoutPolicy::Auto),
        _ => bail!("FORGE_NVFP4_CT_LAYOUT wymaga row, s0 albo auto"),
    }
}

fn resolve_nvfp4_ct_layout_from_env() -> Result<NvFp4CtLayoutPolicy> {
    resolve_nvfp4_ct_layout(
        std::env::var("FORGE_NVFP4_CT_LAYOUT")
            .as_deref()
            .unwrap_or("auto"),
    )
}

#[allow(clippy::too_many_arguments)]
fn load_for_serve(
    path: &Path,
    kv_pages: usize,
    weights_pool_gb: f64,
    weight_host_gb: f64,
    weight_spill_dir: Option<std::path::PathBuf>,
    kv_quant: KvQuant,
    kv_tier: KvTierConfig,
    ctx: usize,
    hot_pages: usize,
    prefix_cache: bool,
    native_mtp: bool,
    nvfp4_gguf_layout: Nvfp4GgufLayout,
) -> Result<(LoadedModel, usize, usize)> {
    let kv_page_size = ModelConfig::default().kv_page_size;
    let desc = read_descriptor(path)?;
    // Without tiering the shared KV pool must hold at least one full-context
    // sequence; with tiering the pool is only the hot working set and the
    // rest of the window spills to RAM/NVMe. KVFlash (`hot_pages > 0`) fixes
    // the VRAM pool to exactly that many pages regardless of --ctx, so VRAM
    // stays constant as the context grows.
    let (max_seq_len, ctx_pages) =
        resolve_ctx(desc.params.max_position_embeddings, ctx, kv_page_size);
    let layout = resolve_kv_pool_layout(
        &desc,
        kv_page_size,
        ctx_pages,
        kv_pages,
        hot_pages,
        &kv_tier,
        kv_quant,
        native_mtp,
    )?;
    let kv_pages = layout.pages;
    let kv_pool = layout.bytes;
    let stage = if kv_tier.enabled() {
        tier_stage_bytes(&desc, kv_page_size, ctx_pages, kv_quant)?
    } else {
        0
    };
    let activations = activation_pool_bytes(native_mtp, desc.params.ssm.is_some());
    // Clamp the requested weights pool so weights + KV + activations always fit
    // free VRAM — a large --ctx grows KV and must not OOM the pool arenas.
    let free = gpu::free_vram(0).context("query free VRAM")?;
    let weights_budget = free.saturating_sub(pool_reserve_bytes(kv_pool, activations)?);
    let weights = if weights_pool_gb > 0.0 {
        ((weights_pool_gb * (1u64 << 30) as f64) as usize)
            .checked_add(stage)
            .context("rozmiar puli wag i staging przekracza zakres usize")?
            .min(weights_budget)
            .max(1 << 30)
    } else {
        // 0 = automatic: hand the weights pool everything KV + activations do
        // not reserve, leaving room for the auto fp8 prefill packs (staging for
        // KV tiering lives inside the pool, so the budget already covers it).
        weights_budget.max(1 << 30)
    };
    let dev: Arc<dyn Device> = gpu::open(
        0,
        PoolSizes {
            weights,
            kv_cache: kv_pool,
            activations,
            kv_page_size: PoolSizes::DEFAULT_KV_PAGE,
        },
    )
    .context("open GPU device")?;
    let cfg = ModelConfig {
        weight_host_budget: (weight_host_gb * (1u64 << 30) as f64) as usize,
        weight_spill_dir,
        kv_page_size,
        kv_pages,
        kv_quant,
        kv_tier,
        max_seq_len,
        prefix_cache,
        native_mtp,
        nvfp4_gguf_layout,
        nvfp4_ct_layout: resolve_nvfp4_ct_layout_from_env()?,
        layer_range: None,
    };
    let loaded = load_model(dev, path, cfg)?;
    Ok((loaded, kv_pages, max_seq_len))
}

/// Load a model for one-shot commands, sizing pools from free VRAM.
/// Resolve the usable context length and the KV page count that covers it.
/// `requested == 0` defaults to the model's own maximum; a non-zero request is
/// honored as-is (down to a single page, up to the model's positional limit).
/// Whether the resulting KV pool fits VRAM is decided later by pool sizing.
/// Dolna granica automatycznego zmniejszania kontekstu. Ponizej tej wartosci
/// model przestaje byc uzyteczny, wiec zamiast ciac dalej zostawiamy resztę
/// wag na sciezce offloadu.
const MIN_AUTO_CTX_TOKENS: usize = 8192;

fn resolve_ctx(
    max_position_embeddings: usize,
    requested: usize,
    page_size: usize,
) -> (usize, usize) {
    let model_max = max_position_embeddings.max(page_size);
    let target = if requested == 0 {
        model_max
    } else {
        requested.min(model_max).max(1)
    };
    (target, target.div_ceil(page_size))
}

#[allow(clippy::too_many_arguments)]

/// Ile bajtów wag niesie checkpoint. Dla GGUF to rozmiar pliku (nagłówek jest
/// pomijalny), dla katalogu safetensors suma plików tensorów.
fn checkpoint_weight_bytes(path: &Path) -> Result<usize> {
    if path.is_dir() {
        let mut total = 0u64;
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            if entry.path().extension().is_some_and(|e| e == "safetensors") {
                total += entry.metadata()?.len();
            }
        }
        return Ok(total as usize);
    }
    Ok(std::fs::metadata(path)?.len() as usize)
}

/// Pamięć hosta, którą wolno zająć pod wagi. `MemAvailable` z `/proc/meminfo`
/// jest jedyną wiarygodną miarą „ile da się wziąć bez wpychania systemu w swap";
/// bierzemy z niej połowę, żeby zostawić miejsce na resztę procesu.
fn host_offload_headroom() -> usize {
    let Ok(text) = std::fs::read_to_string("/proc/meminfo") else {
        return 0;
    };
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemAvailable:") {
            let kib: usize = rest
                .split_whitespace()
                .next()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            return (kib / 2) * 1024;
        }
    }
    0
}

/// Dobiera budżet RAM i katalog zrzutu, gdy wagi nie mieszczą się w VRAM.
/// Jawne `--weight-host-gb` / `--weight-spill-dir` mają pierwszeństwo i nie są
/// ruszane — automat działa tylko tam, gdzie użytkownik nic nie podał.
fn resolve_offload(
    path: &Path,
    vram_weights_pool: usize,
    weight_host_gb: f64,
    weight_spill_dir: Option<std::path::PathBuf>,
) -> Result<(usize, Option<std::path::PathBuf>)> {
    let explicit_host = (weight_host_gb * (1u64 << 30) as f64) as usize;
    if explicit_host > 0 && weight_spill_dir.is_some() {
        return Ok((explicit_host, weight_spill_dir));
    }
    let needed = checkpoint_weight_bytes(path).unwrap_or(0);
    if needed == 0 || needed <= vram_weights_pool {
        return Ok((explicit_host, weight_spill_dir));
    }
    // Zapas 15%: pula wag trzyma nie tylko same tensory, więc realny niedobór
    // jest większy niż różnica rozmiarów.
    let missing = (needed - vram_weights_pool) * 115 / 100;
    let host = if explicit_host > 0 {
        explicit_host
    } else {
        missing.min(host_offload_headroom())
    };
    // Zrzut na dysk wystawiamy ZAWSZE, gdy model nie mieści się w VRAM. Katalog
    // sam z siebie nic nie kosztuje, a jest jedyną deską ratunku, gdy RAM też
    // się skończy w trakcie ładowania.
    let spill = match weight_spill_dir {
        Some(dir) => Some(dir),
        None => Some(default_spill_dir()?),
    };
    eprintln!(
        "wagi {} MiB nie mieszczą się w puli VRAM {} MiB — RAM {} MiB{}",
        needed >> 20,
        vram_weights_pool >> 20,
        host >> 20,
        match spill.as_ref() {
            Some(dir) => format!(", zrzut na NVMe: {}", dir.display()),
            None => String::new(),
        }
    );
    Ok((host, spill))
}

/// Katalog zrzutu wag na dysku. Trafia do cache użytkownika, nie do `/tmp` —
/// `/tmp` bywa tmpfs w RAM, więc zrzut „na dysk" zjadłby tę samą pamięć.
fn default_spill_dir() -> Result<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))
        .context("nie da się ustalić katalogu cache na zrzut wag")?;
    let dir = base.join("forge").join("weight-spill");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("utworzenie katalogu zrzutu {}", dir.display()))?;
    Ok(dir)
}

fn load_auto(
    path: &Path,
    weights_pool_gb: f64,
    weight_host_gb: f64,
    weight_spill_dir: Option<std::path::PathBuf>,
    kv_quant: KvQuant,
    kv_tier: KvTierConfig,
    ctx: usize,
    kv_pages_flag: usize,
    hot_pages: usize,
    prefix_cache: bool,
    native_mtp: bool,
    nvfp4_gguf_layout: Nvfp4GgufLayout,
) -> Result<LoadedModel> {
    // Read hyperparameters before loading weights so the KV cache is sized for
    // the requested context up front (its slabs are allocated during load).
    let desc = read_descriptor(path)?;
    let page_size = ModelConfig::default().kv_page_size;
    let (max_seq_len, ctx_pages) = resolve_ctx(desc.params.max_position_embeddings, ctx, page_size);
    // KVFlash (`hot_pages > 0`) fixes the VRAM pool to a constant page count
    // regardless of --ctx; else 0 = a pool covering the whole context (today's
    // behavior) and an explicit --kv-pages caps the hot VRAM working set.
    let mut layout = resolve_kv_pool_layout(
        &desc,
        page_size,
        ctx_pages,
        kv_pages_flag,
        hot_pages,
        &kv_tier,
        kv_quant,
        native_mtp,
    )?;
    let activations = activation_pool_bytes(native_mtp, desc.params.ssm.is_some());
    // Automatycznie dobrany kontekst NIE MOZE zaglodzic wag. Kazdy token czyta
    // cale wagi, a strony KV zapelniaja sie dopiero wraz z dlugoscia sekwencji,
    // wiec oddanie rezydentnych wag za wiekszy kontekst jest zawsze zla
    // zamiana: Qwen3.6-27B na karcie 20 GiB schodzil tak do 1,6 GiB w VRAM i
    // czytal 89% wag przez PCIe. Kontekst podany recznie zostaje nietkniety —
    // to wybor uzytkownika.
    let mut max_seq_len = max_seq_len;
    let mut ctx_pages = ctx_pages;
    if ctx == 0 && weights_pool_gb == 0.0 && hot_pages == 0 && kv_pages_flag == 0 {
        let needed = checkpoint_weight_bytes(path).unwrap_or(0);
        let free = gpu::free_vram(0).context("query free VRAM")?;
        let floor_pages = MIN_AUTO_CTX_TOKENS.div_ceil(page_size);
        let full_seq_len = max_seq_len;
        while ctx_pages > floor_pages
            && free.saturating_sub(pool_reserve_bytes(layout.bytes, activations)?) < needed
        {
            ctx_pages = (ctx_pages / 2).max(floor_pages);
            max_seq_len = ctx_pages * page_size;
            layout = resolve_kv_pool_layout(
                &desc,
                page_size,
                ctx_pages,
                kv_pages_flag,
                hot_pages,
                &kv_tier,
                kv_quant,
                native_mtp,
            )?;
        }
        if max_seq_len < full_seq_len {
            eprintln!(
                "kontekst zmniejszony z {full_seq_len} do {max_seq_len} tokenow, zeby wagi \
                 ({} MiB) zostaly w VRAM — podaj --ctx, zeby wymusic wiekszy",
                needed >> 20
            );
        }
    }
    let kv_pages = layout.pages;
    let stage = if kv_tier.enabled() {
        tier_stage_bytes(&desc, page_size, ctx_pages, kv_quant)?
    } else {
        0
    };

    // `KvCache` alokuje wszystkie slaby targetu i opcjonalnego MTP w dedykowanej
    // puli; rozmiar uwzględnia wyrównanie każdego bufora do granulacji slabów.
    let hal_kv = layout.bytes;
    let weights = if weights_pool_gb > 0.0 {
        ((weights_pool_gb * (1u64 << 30) as f64) as usize)
            .checked_add(stage)
            .context("rozmiar puli wag i staging przekracza zakres usize")?
    } else {
        let free = gpu::free_vram(0).context("query free VRAM")?;
        free.saturating_sub(pool_reserve_bytes(hal_kv, activations)?)
            .max(1 << 30)
    };
    let dev: Arc<dyn Device> = gpu::open(
        0,
        PoolSizes {
            weights,
            kv_cache: hal_kv,
            activations,
            kv_page_size: PoolSizes::DEFAULT_KV_PAGE,
        },
    )
    .context("open GPU device")?;
    // Wagi, które nie mieszczą się w puli VRAM, mają zjechać do RAM, a potem na
    // NVMe — automatycznie. Bez tego model większy od karty kończy się błędem
    // pamięci, mimo że w maszynie jest i RAM, i dysk.
    let (weight_host_budget, weight_spill_dir) =
        resolve_offload(path, weights, weight_host_gb, weight_spill_dir)?;
    load_model(
        dev,
        path,
        ModelConfig {
        weight_host_budget,
        weight_spill_dir,
            kv_quant,
            kv_tier,
            kv_page_size: page_size,
            kv_pages,
            max_seq_len,
            prefix_cache,
            native_mtp,
            nvfp4_gguf_layout,
            nvfp4_ct_layout: resolve_nvfp4_ct_layout_from_env()?,
            layer_range: None,
        },
    )
}

/// Load a Whisper model on its own GPU device with pools sized from the
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
    let dev: Arc<dyn Device> = gpu::open(
        0,
        PoolSizes {
            weights: tensor_bytes as usize + (1 << 30),
            kv_cache: 4 << 20,
            activations: 32 << 20,
            kv_page_size: 256 << 10,
        },
    )
    .context("open GPU device for whisper")?;
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

/// Load an embedding model on its own GPU device (like Whisper), resolving its
/// pooling and normalization from metadata. A `None` pooling degrades to mean.
/// Pools auto-size from free VRAM (the model loads after the chat engine has
/// already taken its own device pool).
fn load_embed(path: &Path, model_id: String) -> Result<SharedEmbed> {
    let dev: Arc<dyn Device> =
        gpu::open_default_pools(0).context("open GPU device for embedding model")?;
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
    weight_host_gb: f64,
    weight_spill_dir: Option<std::path::PathBuf>,
) -> Result<()> {
    let t0 = Instant::now();
    let loaded = load_auto(
        model_path,
        weights_pool_gb,
        weight_host_gb,
        weight_spill_dir,
        KvQuant::F16,
        KvTierConfig::default(),
        8192,
        0,
        0,
        false,
        false,
        Nvfp4GgufLayout::RowMajor36,
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

/// Load an ONNX model, print its op coverage, and smoke-run it on the GPU.
/// Silero VAD (inputs `input`/`state`/`sr`) is executed end-to-end and its
/// speech probability printed; other models report their parsed graph only,
/// since running them needs model-specific inputs.
fn cmd_onnx_run(model_path: &Path, samples: usize, sr: i64, signal: &str) -> Result<()> {
    let model = forge_onnx::load_model(model_path)
        .with_context(|| format!("parse {}", model_path.display()))?;
    let hist = forge_onnx::op_histogram(&model);
    let total: usize = hist.values().sum();
    println!("model: {}", model_path.display());
    if let Some((_, v)) = model.opset_import.iter().find(|(d, _)| d.is_empty()) {
        println!("opset (default domain): {v}");
    }
    println!("graph inputs : {:?}", model.graph.input);
    println!("graph outputs: {:?}", model.graph.output);
    println!("op types ({total} nodes across graph + subgraphs):");
    for (op, n) in &hist {
        println!("  {op:<16} {n}");
    }

    let inputs = &model.graph.input;
    let is_vad = ["input", "state", "sr"]
        .iter()
        .all(|n| inputs.iter().any(|i| i == n));
    if !is_vad {
        println!(
            "\nparsed {} nodes; no Silero VAD input signature (input/state/sr) — \
             pass such a model to run it on the GPU.",
            model.graph.node.len()
        );
        return Ok(());
    }

    let frame: Vec<f32> = (0..samples)
        .map(|i| match signal {
            "zero" => 0.0,
            "ones" => 1.0,
            _ => (i as f32 * 0.1).sin() * 0.1,
        })
        .collect();

    let dev: Arc<dyn Device> = gpu::open(
        0,
        PoolSizes {
            weights: 64 << 20,
            kv_cache: 4 << 20,
            activations: 256 << 20,
            kv_page_size: PoolSizes::DEFAULT_KV_PAGE,
        },
    )
    .context("open GPU device")?;
    let session = forge_onnx::load_session(dev, model_path)?;

    let mut named = std::collections::HashMap::new();
    named.insert(
        "input".to_string(),
        forge_onnx::Tensor::from_f32(vec![1, samples], frame),
    );
    named.insert(
        "state".to_string(),
        forge_onnx::Tensor::from_f32(vec![2, 1, 128], vec![0.0; 256]),
    );
    named.insert("sr".to_string(), forge_onnx::Tensor::scalar_i64(sr));

    let t0 = Instant::now();
    let out = session.run(named).map_err(|e| anyhow::anyhow!("{e}"))?;
    let dt = t0.elapsed();
    let prob = out
        .get("output")
        .context("model has no `output` tensor")?
        .to_f32_vec()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!(
        "\nSilero VAD ({signal}, {samples} samples @ {sr} Hz): speech probability = {:.6}  ({:.2} ms)",
        prob.first().copied().unwrap_or(f32::NAN),
        dt.as_secs_f64() * 1e3
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_serve(
    model_path: &Path,
    bind: SocketAddr,
    model_id: Option<String>,
    api_key: Option<String>,
    max_active: Option<u16>,
    batch_min: Option<u16>,
    prefill_chunk: usize,
    kv_pages: usize,
    weights_pool_gb: f64,
    weight_host_gb: f64,
    weight_spill_dir: Option<std::path::PathBuf>,
    tool_call_parser: Option<String>,
    whisper_model: Option<&Path>,
    embed_model: Option<&Path>,
    kv_quant: KvQuant,
    kv_tier: KvTierConfig,
    ctx: usize,
    hot_pages: usize,
    prefix_cache: bool,
    spec: SpeculativeConfig,
    tp_cards: Vec<forge_hal::gpu::DeviceId>,
) -> Result<()> {
    // Speculation appends + rolls back draft KV on the plain paged cache; the
    // radix prefix cache donates/borrows pages and is mutually exclusive with
    // it, so enabling speculation forces prefix caching off.
    let prefix_cache = prefix_cache && !spec.is_enabled();
    if spec.is_enabled() {
        tracing::info!("speculative decoding enabled; prefix cache disabled");
    }
    let descriptor = read_descriptor(model_path)?;
    let max_active = resolve_max_active(max_active, &spec, descriptor.params.ssm.is_some())?;
    let t0 = Instant::now();
    let (mut loaded, kv_pages, max_seq_len) = load_for_serve(
        model_path,
        kv_pages,
        weights_pool_gb,
        weight_host_gb,
        weight_spill_dir,
        kv_quant,
        kv_tier,
        ctx,
        hot_pages,
        prefix_cache,
        matches!(
            spec.kind(),
            SpeculationKind::NativeMtp | SpeculationKind::NativeMtpNgram
        ),
        Nvfp4GgufLayout::RowMajor36,
    )?;
    enable_tp_ffn(&mut loaded.model, model_path, &tp_cards, weights_pool_gb)?;
    let full_context_admission = kv_full_context_admission_capacity(
        kv_pages,
        max_seq_len.div_ceil(ModelConfig::default().kv_page_size),
        ModelConfig::default().kv_page_size,
        loaded.model.tier_enabled(),
        max_active,
    );
    // Koszt jednej strony KV pozwala przeliczyć deficyt puli wag na liczbę
    // stron, które operator musi zwolnić, gdy paczki FP8 się nie mieszczą.
    let native_mtp = matches!(
        spec.kind(),
        SpeculationKind::NativeMtp | SpeculationKind::NativeMtpNgram
    );
    let kv_descriptor = loaded.model.weights.descriptor.clone();
    let kv_bytes_for = |pages: usize| {
        kv_pool_bytes(
            &kv_descriptor,
            ModelConfig::default().kv_page_size,
            pages,
            kv_quant,
            native_mtp,
        )
        .ok()
    };
    let kv_probe = KvPoolProbe {
        pages: kv_pages,
        bytes_for: &kv_bytes_for,
    };
    maybe_calibrate_w4a8(
        &mut loaded.model,
        model_path,
        &loaded.bundle.tokenizer,
        true,
        Some(&kv_probe),
    )?;
    tracing::info!(
        "loaded {} ({}) in {:.1}s: {} layers, kv_pages={}, full_context_admission={}",
        model_path.display(),
        loaded.model.weights.descriptor.arch,
        t0.elapsed().as_secs_f32(),
        loaded.model.weights.descriptor.params.block_count,
        kv_pages,
        full_context_admission,
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
        batch_min.map(usize::from).unwrap_or(0),
        spec,
    )?;

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
/// piece. Returns counts, first visible token, completion and profil benchmarku.
/// Token counts come from the engine's `Done` usage, not from counting text
/// pieces. `first_token_at` is the first VISIBLE text event: the engine only
/// emits `Token` for non-empty decoded pieces, so UTF-8/stop-holdback in the
/// stream decoder can shift it a decode step or two past the true first
/// sampled token.
/// Wynik jednego przebiegu żądania. `cache_read_tokens` jest tu celowo
/// widoczny: benchmark musi wiedzieć, ile tokenów promptu NIE zostało
/// policzonych, bo inaczej raportuje przepustowość prefillu za prompt, którego
/// nigdy nie przeliczył.
struct DrainOutcome {
    generated: usize,
    prompt_tokens: usize,
    cache_read_tokens: usize,
    first_token_at: Instant,
    done_at: Instant,
    benchmark: Option<BenchmarkTimings>,
}

fn drain_request(
    engine: &EngineHandle,
    req: EngineRequest,
    mut on_token: impl FnMut(u32, &str),
) -> Result<DrainOutcome> {
    let rx = engine.submit(req).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut first_token_at = None;
    loop {
        match rx.recv().context("engine stream ended unexpectedly")? {
            EngineEvent::Token { id, text, .. } => {
                first_token_at.get_or_insert_with(Instant::now);
                on_token(id, &text);
            }
            EngineEvent::Done {
                tokens,
                prompt_tokens,
                cache_read_tokens,
                benchmark,
                ..
            } => {
                let done_at = Instant::now();
                return Ok(DrainOutcome {
                    generated: tokens,
                    prompt_tokens,
                    cache_read_tokens,
                    first_token_at: first_token_at.unwrap_or(done_at),
                    done_at,
                    benchmark,
                });
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
    temperature: Option<f32>,
    chat: bool,
    no_chat: bool,
    weights_pool_gb: f64,
    weight_host_gb: f64,
    weight_spill_dir: Option<std::path::PathBuf>,
    kv_quant: KvQuant,
    kv_tier: KvTierConfig,
    ctx: usize,
    kv_pages: usize,
    hot_pages: usize,
    prefix_cache: bool,
    spec: SpeculativeConfig,
    tp_cards: Vec<forge_hal::gpu::DeviceId>,
) -> Result<()> {
    // Ustawienia domyślne bierze profil modelu; flagi CLI je nadpisują. Bez tego
    // `forge run <model> "<prompt>"` losowałby przy temperaturze 0,7 i podawał
    // surowy prompt modelowi instrukcyjnemu — jedno i drugie wygląda jak awaria
    // modelu, a jest tylko złym ustawieniem domyślnym.
    let profile = forge_engine::model_profile::resolve(
        &forge_engine::model_profile::identify(model_path),
    );
    let temperature = temperature.unwrap_or(profile.temperature);
    // `--ctx 0` znaczy „pełne okno modelu"; profil podaje wartość tam, gdzie
    // pełne okno jest niepraktyczne (Gemma 4 żąda wtedy ponad 100 GB VRAM).
    let ctx = if ctx == 0 {
        profile.default_ctx.unwrap_or(0)
    } else {
        ctx
    };
    let chat = if chat {
        true
    } else if no_chat {
        false
    } else {
        profile.chat_template
    };
    eprintln!(
        "profil modelu: {} (temp {temperature}, szablon czatu {})",
        profile.label, chat
    );

    let t0 = Instant::now();
    // Speculation is mutually exclusive with the radix prefix cache (both manage
    // paged KV ownership); enabling it forces prefix caching off.
    let prefix_cache = prefix_cache && !spec.is_enabled();
    let mut loaded = load_auto(
        model_path,
        weights_pool_gb,
        weight_host_gb,
        weight_spill_dir,
        kv_quant,
        kv_tier,
        ctx,
        kv_pages,
        hot_pages,
        prefix_cache,
        matches!(
            spec.kind(),
            SpeculationKind::NativeMtp | SpeculationKind::NativeMtpNgram
        ),
        Nvfp4GgufLayout::RowMajor36,
    )?;
    eprintln!("model loaded in {:.1}s", t0.elapsed().as_secs_f32());
    enable_tp_ffn(&mut loaded.model, model_path, &tp_cards, weights_pool_gb)?;
    maybe_calibrate_w4a8(&mut loaded.model, model_path, &loaded.bundle.tokenizer, true, None)?;

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
    // Single sequence: full-size prefill chunks, no other decode ITL to protect.
    let engine = forge_engine::server::spawn_engine_batched(
        loaded.model,
        tokenizer,
        1,
        forge_engine::model::MAX_PREFILL_CHUNK,
        12,
        spec,
    )?;

    let submit_at = Instant::now();
    let outcome = drain_request(
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
            grammar: None,
            ..Default::default()
        },
        |_, piece| {
            use std::io::Write;
            print!("{piece}");
            std::io::stdout().flush().ok();
        },
    )?;
    println!();
    let dt = outcome.done_at.duration_since(submit_at).as_secs_f32();
    eprintln!(
        "{} prompt + {} generated in {dt:.2}s ({:.1} tok/s overall)",
        outcome.prompt_tokens,
        outcome.generated,
        (outcome.prompt_tokens + outcome.generated) as f32 / dt
    );
    shutdown_engine(engine)?;
    Ok(())
}

/// Zatrzymuje wątek roboczy silnika przed wyjściem z procesu.
///
/// Wątek trzyma model i zasoby urządzenia. Bez dołączenia go proces wychodzi w
/// chwili, gdy on jeszcze je zwalnia, a sterownik GPU jest już w trakcie własnej
/// rozbiórki — na ROCm kończy się to uszkodzeniem sterty hosta.
fn shutdown_engine(engine: EngineHandle) -> Result<()> {
    engine.shutdown().map_err(|e| anyhow::anyhow!(e))
}

/// Held-out passage for the perplexity gate — deliberately DISTINCT from the
/// W4A8 calibration text, so the score measures generalization, not overfit to
/// calibration statistics.
const PPL_HELDOUT_TEXT: &str = "\
The history of computing hardware spans several centuries, beginning with early \
mechanical calculating devices and culminating in the electronic digital \
computers that pervade modern life. The abacus, one of the earliest known \
calculating tools, was used by ancient civilizations for arithmetic. In the \
nineteenth century, Charles Babbage designed the Analytical Engine, a mechanical \
general-purpose computer, while Ada Lovelace wrote what is often considered the \
first algorithm intended to be processed by a machine. The twentieth century \
saw the transition from electromechanical relays to vacuum tubes, then to \
transistors, and finally to integrated circuits, each step dramatically \
reducing size and cost while increasing speed and reliability. Today a single \
graphics processing unit performs trillions of floating-point operations every \
second, enabling the training and deployment of large neural networks that \
translate languages, recognize images, and generate fluent text across dozens \
of domains and writing styles with remarkable and often surprising competence.";

/// Compute mean next-token perplexity of the held-out passage under the active
/// GEMM backend (W4A8 when `FORGE_GEMM=w4a8`, else the committed Q4_K path).
fn cmd_ppl(model_path: &Path, ctx: usize) -> Result<()> {
    let t0 = Instant::now();
    let mut loaded = load_auto(
        model_path,
        0.0,
        0.0,
        None,
        KvQuant::F16,
        KvTierConfig::default(),
        ctx,
        0,
        0,
        false,
        false,
        Nvfp4GgufLayout::RowMajor36,
    )?;
    eprintln!("model loaded in {:.1}s", t0.elapsed().as_secs_f32());
    maybe_calibrate_w4a8(&mut loaded.model, model_path, &loaded.bundle.tokenizer, false, None)?;

    let tokens = loaded
        .bundle
        .tokenizer
        .encode(PPL_HELDOUT_TEXT, true)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let (mean_nll, count) = loaded.model.perplexity(&tokens)?;
    let backend = std::env::var("FORGE_GEMM").unwrap_or_else(|_| "default(q4_k)".into());
    println!(
        "ppl backend={backend} tokens_scored={count} mean_nll={mean_nll:.5} perplexity={:.4}",
        mean_nll.exp()
    );
    Ok(())
}

/// Build the W4A8 SmoothQuant packs when `FORGE_GEMM=w4a8` is selected. A
/// one-time calibration over the embedded passage; GGUF sources only (the dense
/// W4A8 gate model). No-op for any other GEMM backend.
///
/// Pozwala przeliczyć deficyt puli wag na liczbę stron KV, których zwolnienie
/// REALNIE go pokryje. Slab każdej warstwy jest zaokrąglany do granulacji
/// alokatora osobno, więc pula maleje skokami i średnia na stronę zaniża wynik.
struct KvPoolProbe<'a> {
    pages: usize,
    bytes_for: &'a dyn Fn(usize) -> Option<usize>,
}

impl KvPoolProbe<'_> {
    fn pages_to_free(&self, deficit: usize) -> Option<usize> {
        let current = (self.bytes_for)(self.pages)?;
        (1..self.pages).find(|freed| {
            (self.bytes_for)(self.pages - freed)
                .is_some_and(|smaller| current.saturating_sub(smaller) >= deficit)
        })
    }
}

/// Zgłasza operatorowi, że szybki prefill FP8 wypadł WYŁĄCZNIE przez budżet
/// puli wag — wcześniej ten przypadek był nieodróżnialny od braku wsparcia
/// sprzętowego i cicho zostawiał serwer na wolniejszym prefillu NVFP4.
fn report_fp8_pool_shortfall(
    label: &str,
    required: usize,
    available: usize,
    kv_probe: Option<&KvPoolProbe<'_>>,
) {
    let gib = |bytes: usize| bytes as f64 / (1u64 << 30) as f64;
    let deficit = required.saturating_sub(available);
    let remedy = match kv_probe.and_then(|probe| probe.pages_to_free(deficit)) {
        Some(pages) => format!(
            "zmniejsz --kv-pages o co najmniej {pages} (albo --ctx), lub podnieś --weights-pool-gb"
        ),
        None => "zmniejsz --kv-pages albo --ctx, lub podnieś --weights-pool-gb".to_string(),
    };
    tracing::warn!(
        required_bytes = required,
        available_bytes = available,
        deficit_bytes = deficit,
        "paczki FP8 nie mieszczą się w puli wag"
    );
    eprintln!(
        "{label}: paczki FP8 wymagają {:.2} GiB, pula wag ma {:.2} GiB (brakuje {:.2} GiB) \
         — prefill zostaje na wolniejszej ścieżce; {remedy}",
        gib(required),
        gib(available),
        gib(deficit)
    );
}

/// `auto_gguf_fp8` (serve, run, bench — everything except `ppl`, which is a
/// quality gate documented to run exactly the GEMM `FORGE_GEMM` names): with
/// FORGE_GEMM unset, a dense GGUF model on
/// an fp8-native device whose projection shapes all have committed Modular fp8
/// instances gets the fp8mod prefill automatically (near-lossless, ~2× the
/// native int8 prefill); `FORGE_GEMM=mojo` keeps the native path explicitly.
fn maybe_calibrate_w4a8(
    model: &mut forge_engine::model::Model,
    path: &Path,
    tokenizer: &forge_tokenize::Tokenizer,
    auto_gguf_fp8: bool,
    kv_probe: Option<&KvPoolProbe<'_>>,
) -> Result<()> {
    let gemm = std::env::var("FORGE_GEMM").ok();
    let auto_fp8_ffn = gemm.is_none() && path.is_dir();
    if gemm.as_deref() == Some("fp8mod-ffn") || auto_fp8_ffn {
        if !path.is_dir() {
            bail!("FORGE_GEMM=fp8mod-ffn wymaga katalogu checkpointu NVFP4");
        }
        let t0 = Instant::now();
        match model.build_fp8_ffn()? {
            Fp8PackOutcome::Built => eprintln!(
                "paczki FP8 Q/O/FFN/lm_head zbudowane na GPU w {:.3}s (K/V i warstwy decode pozostają NVFP4)",
                t0.elapsed().as_secs_f32()
            ),
            Fp8PackOutcome::Unsupported => {
                eprintln!("fp8mod-ffn niedostępny dla urządzenia lub kształtów; pozostaje NVFP4")
            }
            Fp8PackOutcome::PoolShortfall {
                required,
                available,
            } => report_fp8_pool_shortfall("fp8mod-ffn", required, available, kv_probe),
        }
        return Ok(());
    }
    if gemm.is_none() && auto_gguf_fp8 && path.extension().and_then(|e| e.to_str()) == Some("gguf")
    {
        let t0 = Instant::now();
        match model.build_fp8_modular_auto(path)? {
            Fp8PackOutcome::Built => eprintln!(
                "auto fp8mod: paczki e4m3 zbudowane w {:.1}s (prefill Modular fp8, decode zostaje na natywnym GGUF)",
                t0.elapsed().as_secs_f32()
            ),
            Fp8PackOutcome::Unsupported => {
                eprintln!("auto fp8mod niedostępny dla urządzenia lub kształtów; prefill zostaje natywny")
            }
            Fp8PackOutcome::PoolShortfall {
                required,
                available,
            } => report_fp8_pool_shortfall("auto fp8mod", required, available, kv_probe),
        }
        return Ok(());
    }
    if matches!(gemm.as_deref(), Some("fp8") | Some("fp8mod")) {
        if path.extension().and_then(|e| e.to_str()) != Some("gguf") {
            bail!(
                "FORGE_GEMM={} currently supports GGUF models only",
                gemm.unwrap()
            );
        }
        let t0 = Instant::now();
        model.build_fp8(path)?;
        let variant = if gemm.as_deref() == Some("fp8mod") {
            "Modular multistage"
        } else {
            "hand kernel"
        };
        eprintln!(
            "fp8 (e4m3) requant packs built in {:.1}s (per-row scale, no calibration; {variant} GEMM)",
            t0.elapsed().as_secs_f32()
        );
        return Ok(());
    }
    if std::env::var("FORGE_GEMM").ok().as_deref() != Some("w4a8") {
        return Ok(());
    }
    if path.extension().and_then(|e| e.to_str()) != Some("gguf") {
        bail!("FORGE_GEMM=w4a8 calibration currently supports GGUF models only");
    }
    let t0 = Instant::now();
    let toks = tokenizer
        .encode(forge_engine::model::W4A8_CALIB_TEXT, false)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    model.calibrate_w4a8(path, &toks)?;
    eprintln!(
        "W4A8 requant packs built in {:.1}s (SmoothQuant off by default; \
         FORGE_W4A8_ALPHA=<0..1> to enable)",
        t0.elapsed().as_secs_f32()
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_bench(
    model_path: &Path,
    tokens: usize,
    prompt_tokens: usize,
    prompt_token_ids: Option<&Path>,
    weights_pool_gb: f64,
    weight_host_gb: f64,
    weight_spill_dir: Option<std::path::PathBuf>,
    kv_quant: KvQuant,
    kv_tier: KvTierConfig,
    ctx: usize,
    kv_pages: usize,
    hot_pages: usize,
    prefix_cache: bool,
    spec: SpeculativeConfig,
    reps: usize,
    tp_cards: Vec<forge_hal::gpu::DeviceId>,
    tp_split: Option<Vec<usize>>,
) -> Result<()> {
    if tokens < 2 {
        bail!("--tokens must be at least 2 to measure decode throughput");
    }
    if prompt_tokens == 0 && prompt_token_ids.is_none() {
        bail!("--prompt-tokens must be at least 1");
    }
    if reps == 0 {
        bail!("--reps must be at least 1");
    }
    let t0 = Instant::now();
    let prefix_cache = prefix_cache && !spec.is_enabled();
    let mut loaded = load_auto(
        model_path,
        weights_pool_gb,
        weight_host_gb,
        weight_spill_dir,
        kv_quant,
        kv_tier,
        ctx,
        kv_pages,
        hot_pages,
        prefix_cache,
        matches!(
            spec.kind(),
            SpeculationKind::NativeMtp | SpeculationKind::NativeMtpNgram
        ),
        resolve_bench_nvfp4_gguf_layout_from_env(spec.kind(), 1)?,
    )?;
    eprintln!("model loaded in {:.1}s", t0.elapsed().as_secs_f32());
    enable_tp_ffn_with(
        &mut loaded.model,
        model_path,
        &tp_cards,
        weights_pool_gb,
        tp_split.as_deref(),
    )?;
    maybe_calibrate_w4a8(&mut loaded.model, model_path, &loaded.bundle.tokenizer, true, None)?;

    let tokenizer = Arc::new(loaded.bundle.tokenizer);
    let (prompt_ids, prompt_sha256, prompt_source) = if let Some(path) = prompt_token_ids {
        let input = bench::TokenInput::read_u32le(path)?;
        (input.ids, input.sha256, path.display().to_string())
    } else {
        let seed_ids = tokenizer
            .encode(
                "Jednym z najważniejszych miast w historii Polski jest Kraków. ",
                false,
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let ids: Vec<u32> = seed_ids
            .iter()
            .cycle()
            .take(prompt_tokens)
            .copied()
            .collect();
        let sha = bench::sha256_ids(&ids);
        (ids, sha, "tokenizer-seed".into())
    };
    if prompt_ids.is_empty() {
        bail!("benchmark prompt must contain at least one token");
    }
    let model_sha256 = model_path
        .is_file()
        .then(|| bench::sha256_file(model_path))
        .transpose()?;
    let (nvfp4_gguf_layout, nvfp4_repacked_weights) = loaded.model.nvfp4_gguf_layout_summary();
    let bos_id = tokenizer.bos_id();
    eprintln!(
        "{}",
        serde_json::json!({
            "benchmark_input": {
                "format": "u32le",
                "source": prompt_source,
                "sha256": prompt_sha256,
                "token_count": prompt_ids.len(),
                "first_token_id": prompt_ids.first(),
                "tokenizer_bos_id": bos_id,
                "starts_with_bos": bos_id.is_some_and(|id| prompt_ids.first() == Some(&id)),
            },
            "model": {
                "path": model_path.display().to_string(),
                "sha256": model_sha256,
            },
            "config": {
                "ctx": ctx,
                "kv_pages": kv_pages,
                "hot_pages": hot_pages,
                "weights_pool_gb": weights_pool_gb,
                "kv_cache": format!("{kv_quant:?}"),
                "prefix_cache": prefix_cache,
                "speculation": format!("{:?}", spec.kind()),
                "nvfp4_gguf_layout": format!("{nvfp4_gguf_layout:?}"),
                "nvfp4_repacked_weights": nvfp4_repacked_weights,
                "warmup_runs": 1,
                "measured_runs": reps,
            }
        })
    );

    loaded
        .model
        .prepare_prefill_profiles(prompt_ids.len(), reps + 1)?;

    // Single-sequence bench: full-size prefill chunks, no ITL to protect.
    let engine = forge_engine::server::spawn_engine_batched(
        loaded.model,
        tokenizer,
        1,
        forge_engine::model::MAX_PREFILL_CHUNK,
        12,
        spec,
    )?;
    let mut target_ms = Vec::with_capacity(reps);
    let mut catchup_ms = Vec::with_capacity(reps);
    let mut ttft_ms = Vec::with_capacity(reps);
    let mut decode_s = Vec::with_capacity(reps);
    let mut last_prompt_len = 0usize;
    let mut last_generated = 0usize;
    let mut generated_sha256 = None;
    for rep in 0..=reps {
        let submit_at = Instant::now();
        let mut generated_ids = Vec::with_capacity(tokens);
        // No EOS ids: the benchmark must decode exactly `tokens` tokens.
        let outcome = drain_request(
            &engine,
            EngineRequest {
                prompt_tokens: prompt_ids.clone(),
                max_tokens: tokens,
                sampling: SamplingParams {
                    temperature: 0.0,
                    ..SamplingParams::default()
                },
                stop: vec![],
                eos_ids: vec![],
                grammar: None,
                ..Default::default()
            },
            |id, _| generated_ids.push(id),
        )?;
        let DrainOutcome {
            generated,
            prompt_tokens: prompt_len,
            cache_read_tokens,
            first_token_at: first_at,
            done_at,
            benchmark: profile,
        } = outcome;
        // Prefill mierzy się po PEŁNYM promptcie. Trafienie w cache prefiksów
        // przelicza tylko rozbieżny ogon, a czas nadal dzielilibyśmy przez całą
        // długość promptu — stąd zawyżone tok/s (zmierzone 44 537 zamiast 14 775
        // na Mistralu-7B Q4_K_M). Dlatego to błąd, nie ostrzeżenie.
        if cache_read_tokens != 0 {
            bail!(
                "cache prefiksów obsłużył {cache_read_tokens} z {prompt_len} tokenów promptu w powtórzeniu {rep}; przepustowość prefillu byłaby zawyżona — uruchom benchmark z --prefix-cache off"
            );
        }
        let run_sha256 = bench::sha256_ids(&generated_ids);
        if let Some(expected) = &generated_sha256 {
            if expected != &run_sha256 {
                bail!("greedy token IDs differ between benchmark repetitions");
            }
        } else {
            generated_sha256 = Some(run_sha256.clone());
        }
        let visible_ttft_s = first_at.duration_since(submit_at).as_secs_f64();
        let decode_elapsed_s = done_at.duration_since(first_at).as_secs_f64();
        last_prompt_len = prompt_len;
        last_generated = generated;
        let profile = profile.context("worker nie zwrócił profilu prefill")?;
        let target = profile
            .target_gpu_ms
            .context("backend nie udostępnia czasu GPU target prefill")?;
        let catchup = profile
            .mtp_catchup_gpu_ms
            .context("backend nie udostępnia czasu GPU MTP catch-up")?;
        let prefill_tps = prompt_len as f64 / (target / 1000.0).max(1e-9);
        let decode_tps = (generated.saturating_sub(1)) as f64 / decode_elapsed_s.max(1e-9);
        eprintln!(
            "{} {}/{}: target {:.3} ms ({prefill_tps:.1} tok/s) | MTP catch-up {:.3} ms | TTFT {:.3} ms | visible TTFT {:.3} ms | decode {:.1} tok/s",
            if rep == 0 { "warmup" } else { "rep" },
            if rep == 0 { 1 } else { rep },
            if rep == 0 { 1 } else { reps },
            target,
            catchup,
            profile.ttft_ms,
            visible_ttft_s * 1000.0,
            decode_tps,
        );
        eprintln!("generated token SHA256: {run_sha256}");
        if rep == 0 {
            continue;
        }
        target_ms.push(target);
        catchup_ms.push(catchup);
        ttft_ms.push(profile.ttft_ms);
        decode_s.push(decode_elapsed_s);
    }
    let target = bench::Distribution::from_samples(&target_ms)?;
    let catchup = bench::Distribution::from_samples(&catchup_ms)?;
    let ttft = bench::Distribution::from_samples(&ttft_ms)?;
    let decode = bench::Distribution::from_samples(&decode_s)?;
    println!("| phase | tokens | p10 ms | median ms | p90 ms | median tok/s |");
    println!("|---|---:|---:|---:|---:|---:|");
    println!(
        "| target prefill | {last_prompt_len} | {:.3} | {:.3} | {:.3} | {:.1} |",
        target.p10,
        target.median,
        target.p90,
        last_prompt_len as f64 / (target.median / 1000.0).max(1e-9)
    );
    println!(
        "| MTP catch-up | {last_prompt_len} | {:.3} | {:.3} | {:.3} | - |",
        catchup.p10, catchup.median, catchup.p90
    );
    println!(
        "| TTFT | 1 | {:.3} | {:.3} | {:.3} | - |",
        ttft.p10, ttft.median, ttft.p90
    );
    println!(
        "| decode | {} | {:.3} | {:.3} | {:.3} | {:.1} |",
        last_generated.saturating_sub(1),
        decode.p10 * 1000.0,
        decode.median * 1000.0,
        decode.p90 * 1000.0,
        last_generated.saturating_sub(1) as f64 / decode.median.max(1e-9)
    );
    shutdown_engine(engine)?;
    Ok(())
}

#[cfg(test)]
mod speculation_cli_tests {
    use super::{
        activation_pool_bytes, kv_full_context_admission_capacity, parse_speculative,
        resolve_bench_nvfp4_gguf_layout, resolve_kv_pool_layout, resolve_max_active,
        resolve_nvfp4_ct_layout,
    };
    use forge_engine::kv::KvQuant;
    use forge_engine::model::Nvfp4GgufLayout;
    use forge_engine::weights::NvFp4CtLayoutPolicy;
    use forge_engine::speculation::{ProposerKind, SpeculationKind};
    use forge_engine::tier::{KvTierConfig, KvTierMode};
    use forge_formats::{HfConfig, LayerKind, ModelDescriptor};

    fn qwen_hybrid_descriptor() -> ModelDescriptor {
        let config: HfConfig = serde_json::from_str(
            r#"{
                "architectures": ["LlamaForCausalLM"],
                "model_type": "llama",
                "hidden_size": 4096,
                "num_hidden_layers": 64,
                "num_attention_heads": 16,
                "num_key_value_heads": 4,
                "head_dim": 256,
                "intermediate_size": 12288,
                "vocab_size": 248320,
                "max_position_embeddings": 262144
            }"#,
        )
        .unwrap();
        let mut descriptor = ModelDescriptor::from_hf(&config).unwrap();
        descriptor.layer_kinds = [
            vec![LayerKind::DeltaNet; 48],
            vec![LayerKind::Attention; 16],
        ]
        .concat();
        descriptor
    }

    fn ram_tier() -> KvTierConfig {
        KvTierConfig {
            mode: KvTierMode::Ram,
            ..Default::default()
        }
    }

    #[test]
    fn layout_serve_dla_4096_tokenow_nie_ma_progu_jeden_gib() {
        let descriptor = qwen_hybrid_descriptor();

        let layout = resolve_kv_pool_layout(
            &descriptor,
            32,
            128,
            128,
            0,
            &KvTierConfig::default(),
            KvQuant::F16,
            false,
        )
        .unwrap();

        assert_eq!(layout.bytes, 320 << 20);
    }

    #[test]
    fn layout_serve_zachowuje_domyslne_512_stron() {
        let descriptor = qwen_hybrid_descriptor();

        let layout = resolve_kv_pool_layout(
            &descriptor,
            32,
            128,
            512,
            0,
            &KvTierConfig::default(),
            KvQuant::F16,
            false,
        )
        .unwrap();

        assert_eq!(layout.pages, 512);
    }

    #[test]
    fn layout_tier_zero_oznacza_pelny_kontekst() {
        let descriptor = qwen_hybrid_descriptor();

        let layout =
            resolve_kv_pool_layout(&descriptor, 32, 128, 0, 0, &ram_tier(), KvQuant::F16, false)
                .unwrap();

        assert_eq!(layout.pages, 128);
    }

    #[test]
    fn layout_tier_odrzuca_pule_ponizej_minimum_rezydencji() {
        let descriptor = qwen_hybrid_descriptor();

        let result =
            resolve_kv_pool_layout(&descriptor, 32, 128, 1, 0, &ram_tier(), KvQuant::F16, false);

        assert!(result.is_err());
    }

    #[test]
    fn layout_hot_ma_pierwszenstwo_nad_liczba_stron() {
        let descriptor = qwen_hybrid_descriptor();

        let layout = resolve_kv_pool_layout(
            &descriptor,
            32,
            128,
            512,
            64,
            &ram_tier(),
            KvQuant::F16,
            false,
        )
        .unwrap();

        assert_eq!(layout.pages, 64);
    }

    #[test]
    fn admission_pelnego_kontekstu_respektuje_max_active_i_pule() {
        let capacity = kv_full_context_admission_capacity(512, 128, 32, false, 8);

        assert_eq!(capacity, 4);
    }

    #[test]
    fn tile_nvfp4_benchmark_domyslnie_pozostaje_wylaczony() {
        assert_eq!(
            resolve_bench_nvfp4_gguf_layout(SpeculationKind::Off, 1, "0").unwrap(),
            Nvfp4GgufLayout::RowMajor36
        );
    }

    #[test]
    fn parser_layoutu_nvfp4_ct_obsluguje_wszystkie_tryby() {
        assert_eq!(
            resolve_nvfp4_ct_layout("row").unwrap(),
            NvFp4CtLayoutPolicy::RowMajorE4M3
        );
        assert_eq!(
            resolve_nvfp4_ct_layout("s0").unwrap(),
            NvFp4CtLayoutPolicy::S0N64K128
        );
        assert_eq!(
            resolve_nvfp4_ct_layout("auto").unwrap(),
            NvFp4CtLayoutPolicy::Auto
        );
        assert!(resolve_nvfp4_ct_layout("1").is_err());
    }

    #[test]
    fn tile_nvfp4_benchmark_wymaga_jawnego_opt_in() {
        assert_eq!(
            resolve_bench_nvfp4_gguf_layout(SpeculationKind::Off, 1, "1").unwrap(),
            Nvfp4GgufLayout::TileN128K64
        );
    }

    #[test]
    fn tile_nvfp4_benchmark_odrzuca_spekulacje_i_batch() {
        for kind in [
            SpeculationKind::HostProposer,
            SpeculationKind::NativeMtp,
            SpeculationKind::NativeMtpNgram,
        ] {
            assert_eq!(
                resolve_bench_nvfp4_gguf_layout(kind, 1, "0").unwrap(),
                Nvfp4GgufLayout::RowMajor36
            );
            assert!(resolve_bench_nvfp4_gguf_layout(kind, 1, "1").is_err());
        }
        assert_eq!(
            resolve_bench_nvfp4_gguf_layout(SpeculationKind::Off, 2, "0").unwrap(),
            Nvfp4GgufLayout::RowMajor36
        );
        assert!(resolve_bench_nvfp4_gguf_layout(SpeculationKind::Off, 2, "1").is_err());
        assert!(resolve_bench_nvfp4_gguf_layout(SpeculationKind::Off, 1, "auto").is_err());
    }

    #[test]
    fn admission_tier_uzywa_minimum_stron_rezydentnych() {
        let floor = forge_engine::tier::min_resident_pages(32);

        let capacity = kv_full_context_admission_capacity(floor * 3, 128, 32, true, 8);

        assert_eq!(capacity, 3);
    }

    #[test]
    fn parser_rejects_unsupported_budget_and_neural_proposer() {
        assert!(parse_speculative("ngram:0").is_err());
        assert!(parse_speculative("ngram:17").is_err());
        assert!(parse_speculative("on:8").is_err());
        assert!(parse_speculative("dspark:8").is_err());
        assert!(parse_speculative("mtp:1").is_err());
        assert!(parse_speculative("mtp:4").is_err());
    }

    #[test]
    fn parser_accepts_explicit_ngram_budget() {
        let config = parse_speculative("ngram:8").expect("konfiguracja powinna być poprawna");
        assert_eq!(config.proposers(), &[ProposerKind::Ngram]);
        assert_eq!(config.draft_tokens(), 8);
    }

    #[test]
    fn parser_accepts_hybrid_ngram_budgets() {
        for budget in [2, 3] {
            let config = parse_speculative(&format!("ngram:{budget}"))
                .expect("budżet hybrydowego n-gram powinien być poprawny");
            assert_eq!(config.kind(), SpeculationKind::HostProposer);
            assert_eq!(config.draft_tokens(), budget);
            assert_eq!(resolve_max_active(None, &config, true).unwrap(), 1);
            assert_eq!(resolve_max_active(None, &config, false).unwrap(), 8);
            assert!(!config.proposers().contains(&ProposerKind::Mtp));
        }
    }

    #[test]
    fn parser_accepts_native_mtp_budgets() {
        for (input, expected_budget) in [("mtp", 3), ("mtp:2", 2), ("mtp:3", 3)] {
            let config = parse_speculative(input).expect("MTP powinno być obsługiwane");
            assert_eq!(config.proposers(), &[ProposerKind::Mtp]);
            assert_eq!(config.kind(), SpeculationKind::NativeMtp);
            assert_eq!(config.draft_tokens(), expected_budget);
        }
    }

    #[test]
    fn parser_accepts_mtp_ngram_router() {
        for (input, expected_budget) in [("mtp+ngram", 3), ("mtp+ngram:2", 2), ("mtp+ngram:3", 3)] {
            let config = parse_speculative(input).expect("router powinien być obsługiwany");
            assert_eq!(
                config.proposers(),
                &[ProposerKind::Mtp, ProposerKind::Ngram]
            );
            assert_eq!(config.kind(), SpeculationKind::NativeMtpNgram);
            assert_eq!(config.draft_tokens(), expected_budget);
            assert_eq!(resolve_max_active(None, &config, true).unwrap(), 1);
            assert_eq!(resolve_max_active(Some(2), &config, true).unwrap(), 2);
        }
    }

    #[test]
    fn mtp_domyslnie_ogranicza_serwer_do_jednej_sekwencji() {
        let mtp = parse_speculative("mtp").unwrap();
        let off = parse_speculative("off").unwrap();
        assert_eq!(resolve_max_active(None, &mtp, true).unwrap(), 1);
        assert_eq!(resolve_max_active(None, &off, false).unwrap(), 8);
        assert_eq!(resolve_max_active(None, &off, true).unwrap(), 1);
        assert_eq!(resolve_max_active(Some(1), &mtp, true).unwrap(), 1);
        assert_eq!(resolve_max_active(Some(2), &mtp, true).unwrap(), 2);
        assert!(resolve_max_active(Some(0), &mtp, true).is_err());
    }

    #[test]
    fn model_hybrydowy_dostaje_pule_aktywacji_1152_mib() {
        assert_eq!(activation_pool_bytes(true, true), 1152 << 20);
        assert_eq!(activation_pool_bytes(false, true), 1152 << 20);
        assert_eq!(activation_pool_bytes(true, false), 1 << 30);
    }
}
