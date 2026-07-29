// ===== File: kv_tier_longctx.rs — KV tiering proof harness: bit-identity, needle recall, throughput =====
// Runs greedy generation over a long needle-in-haystack prompt with a
// configurable VRAM KV pool and tier mode, printing the generated token ids
// (for diffing tiered vs untiered runs), the decoded text (needle recall) and
// prefill/decode throughput.
//
// Usage: cargo run -p forge-engine --release --example kv_tier_longctx -- <model> \
//          [--ctx N] [--kv-pages N] [--tier off|ram|nvme] [--ram-gb F] \
//          [--prompt-tokens N] [--decode N] [--needle] [--prompt TEXT]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use forge_engine::model::{Model, ModelConfig};
use forge_engine::sample::{GpuSampler, SamplingParams};
use forge_engine::tier::{KvTierConfig, KvTierMode};
use forge_hal::cuda::{CudaDevice, PoolSizes};
use forge_hal::Device;
use forge_tokenize::{StreamDecoder, Tokenizer};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(args.next().expect("model path"));
    let mut ctx = 8192usize;
    let mut kv_pages = 0usize;
    let mut tier_mode = KvTierMode::Off;
    let mut ram_gb = 8.0f64;
    let mut prompt_tokens = 0usize;
    let mut decode = 16usize;
    let mut needle = false;
    let mut prompt_text: Option<String> = None;
    let mut weights_gb = 12.0f64;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--ctx" => ctx = args.next().unwrap().parse()?,
            "--kv-pages" => kv_pages = args.next().unwrap().parse()?,
            "--weights-gb" => weights_gb = args.next().unwrap().parse()?,
            "--tier" => {
                tier_mode = match args.next().unwrap().as_str() {
                    "off" => KvTierMode::Off,
                    "ram" => KvTierMode::Ram,
                    "nvme" => KvTierMode::Nvme,
                    other => panic!("unknown tier {other}"),
                }
            }
            "--ram-gb" => ram_gb = args.next().unwrap().parse()?,
            "--prompt-tokens" => prompt_tokens = args.next().unwrap().parse()?,
            "--decode" => decode = args.next().unwrap().parse()?,
            "--needle" => needle = true,
            "--prompt" => prompt_text = Some(args.next().unwrap()),
            other => panic!("unknown arg {other}"),
        }
    }

    let page_size = 32usize;
    let ctx_pages = ctx.div_ceil(page_size);
    let kv_pages = if kv_pages == 0 { ctx_pages } else { kv_pages };
    let cfg = ModelConfig {
        weight_host_budget: 0,
weight_spill_dir: None,
        kv_page_size: page_size,
        kv_pages,
        max_seq_len: ctx,
        kv_quant: forge_engine::kv::KvQuant::F16,
        kv_tier: KvTierConfig {
            mode: tier_mode,
            dir: None,
            ram_budget_bytes: (ram_gb * (1u64 << 30) as f64) as usize,
            watermark: 0.10,
        },
        prefix_cache: false,
        layer_range: None,
        native_mtp: false,
        nvfp4_gguf_layout: forge_engine::model::Nvfp4GgufLayout::RowMajor36,
        nvfp4_ct_layout: forge_engine::weights::NvFp4CtLayoutPolicy::RowMajorE4M3,
    };
    // The engine's paged KV slabs + (hybrid) SSM state allocate from the
    // WEIGHTS pool, not the HAL kv_cache pool, so keep the latter tiny (a full
    // multi-GiB kv_cache arena would needlessly starve the weights pool on a
    // 20 GB model). Size weights from --weights-gb for large models.
    let device = CudaDevice::new(
        0,
        PoolSizes {
            weights: (weights_gb * (1u64 << 30) as f64) as usize,
            kv_cache: 4 << 20,
            activations: 512 << 20,
            kv_page_size: 256 << 10,
        },
    )?;
    let dev: Arc<dyn Device> = device;

    let t_load = Instant::now();
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
    eprintln!(
        "loaded in {:.1}s (kv_pages={} = {} tok VRAM, ctx={}, tier={:?})",
        t_load.elapsed().as_secs_f32(),
        kv_pages,
        kv_pages * page_size,
        ctx,
        tier_mode
    );

    // Prompt: either literal text, or a padded haystack with the needle early
    // (inside the region that will be spilled) and the question at the end.
    // Long prompts are assembled from per-piece encodes: Bielik's
    // tokenizer.json ships with truncation at 2048 tokens, which a single
    // whole-string encode would silently apply.
    let prompt_ids: Vec<u32> = match (prompt_text, needle) {
        (Some(t), _) => tokenizer.encode(&t, true)?,
        (None, true) => {
            let mut ids = tokenizer.encode(
                "Zapamiętaj dokładnie: tajne hasło dnia to \"żółty żuraw 1697\". \
                 To hasło pojawia się tylko raz.\n\n",
                true,
            )?;
            let filler_ids = tokenizer.encode(
                "Kraków jest jednym z najstarszych miast Polski, a jego historia \
                 sięga początków państwa polskiego. ",
                false,
            )?;
            let tail_ids = tokenizer.encode(
                "\n\nPytanie: jakie jest tajne hasło dnia? Odpowiedz dokładnie.\n\
                 Odpowiedź: tajne hasło dnia to",
                false,
            )?;
            let target = prompt_tokens.max(1024);
            while ids.len() + filler_ids.len() + tail_ids.len() < target {
                ids.extend_from_slice(&filler_ids);
            }
            ids.extend_from_slice(&tail_ids);
            ids
        }
        (None, false) => {
            let mut ids = tokenizer.encode("", true)?;
            let filler_ids = tokenizer.encode(
                "Jednym z najważniejszych miast w historii Polski jest Kraków. ",
                false,
            )?;
            while ids.len() + filler_ids.len() <= prompt_tokens.max(filler_ids.len() + 1) {
                ids.extend_from_slice(&filler_ids);
            }
            ids
        }
    };
    eprintln!("prompt: {} tokens", prompt_ids.len());

    let mut seq = model.new_seq();
    let mut sampler = GpuSampler::new(SamplingParams {
        temperature: 0.0,
        ..SamplingParams::default()
    });

    let t_prefill = Instant::now();
    for chunk in prompt_ids.chunks(1024) {
        model.prefill_chunk(&mut seq, chunk)?;
    }
    let prefill_s = t_prefill.elapsed().as_secs_f64();
    eprintln!(
        "prefill {} tok in {:.2}s ({:.1} tok/s)",
        prompt_ids.len(),
        prefill_s,
        prompt_ids.len() as f64 / prefill_s
    );

    let mut decoder = StreamDecoder::new(&tokenizer, true);
    let mut ids = vec![model.sample_last_logits(&mut sampler)?];
    let t_decode = Instant::now();
    while ids.len() < decode {
        let last = *ids.last().unwrap();
        sampler.note_token(last);
        ids.push(model.step_and_sample(&mut seq, last, &mut sampler)?);
    }
    let decode_s = t_decode.elapsed().as_secs_f64();
    let mut txt = String::new();
    for &id in &ids {
        txt.push_str(&decoder.push(id)?);
    }
    txt.push_str(&decoder.finish()?);

    println!("ids: {ids:?}");
    println!("text: {txt:?}");
    eprintln!(
        "decode {} tok in {:.2}s ({:.1} tok/s)",
        ids.len().saturating_sub(1),
        decode_s,
        ids.len().saturating_sub(1) as f64 / decode_s
    );
    model.release_seq(&mut seq);
    Ok(())
}
