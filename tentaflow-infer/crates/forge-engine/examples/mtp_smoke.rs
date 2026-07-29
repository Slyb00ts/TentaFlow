// =============================================================================
// Plik: mtp_smoke.rs
// Opis: Izolowany test realnego bloku MTP z propozycją K=2 lub K=3 na GPU.
// Przykład: cargo run -p forge-engine --release --example mtp_smoke -- model.gguf 3
// =============================================================================

use std::path::PathBuf;
use std::sync::Arc;

use forge_engine::model::{Model, ModelConfig};
use forge_engine::sample::{GpuSampler, SamplingParams};
use forge_hal::{PoolSizes, gpu};
use forge_hal::Device;
use forge_tokenize::Tokenizer;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(args.next().expect("ścieżka modelu GGUF"));
    let k = args
        .next()
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(3);
    let activations = 1usize << 30;
    let kv_cache = 4usize << 20;
    let reserve = 512usize << 20;
    let free = gpu::free_vram(0)?;
    let weights = free
        .checked_sub(activations + kv_cache + reserve)
        .ok_or("za mało wolnego VRAM na pule MTP")?;
    let device = gpu::open(
        0,
        PoolSizes {
            weights,
            kv_cache,
            activations,
            kv_page_size: PoolSizes::DEFAULT_KV_PAGE,
        },
    )?;
    let dev: Arc<dyn Device> = device;
    let gguf = forge_formats::Gguf::open(&path)?;
    let embedding = gguf
        .tensor("token_embd.weight")
        .ok_or("brak token_embd.weight")?;
    eprintln!(
        "token_embd.weight: {:?} {:?}, {} MiB",
        embedding.dims,
        embedding.quant,
        embedding.size_bytes >> 20
    );
    let vocab = forge_engine::gguf_vocab::gguf_vocab(&gguf)?;
    drop(gguf);
    let tokenizer = Tokenizer::from_gguf_vocab(&vocab)?;
    let mut model = Model::load_gguf(
        dev,
        &path,
        ModelConfig {
            weight_host_budget: 0,
weight_spill_dir: None,
            kv_page_size: 32,
            kv_pages: 4,
            max_seq_len: 128,
            prefix_cache: false,
            native_mtp: true,
            ..ModelConfig::default()
        },
    )?;
    if !model.has_native_mtp() {
        return Err("checkpoint nie zawiera kompletnego deskryptora MTP".into());
    }

    let prompt = tokenizer.encode("The capital of Poland is", true)?;
    let mut seq = model.new_seq();
    model.prefill_chunk(&mut seq, &prompt)?;
    let mut sampler = GpuSampler::new(SamplingParams {
        temperature: 0.0,
        ..SamplingParams::default()
    });
    let target_before = model.sample_last_logits(&mut sampler)?;
    if std::env::var_os("FORGE_MTP_PROFILE_PREFILL").is_some() {
        model.release_seq(&mut seq);
        return Ok(());
    }
    if std::env::var_os("FORGE_MTP_PROFILE_ONCE").is_some() {
        let started = std::time::Instant::now();
        let (draft, accepted, correction) = model.native_mtp_step(&mut seq, target_before, k)?;
        println!(
            "MTP profile K={k}: draft={draft:?}, accepted={accepted}, correction={correction}; {:.3} ms",
            started.elapsed().as_secs_f64() * 1000.0,
        );
        model.release_seq(&mut seq);
        return Ok(());
    }
    let started = std::time::Instant::now();
    let draft = model.mtp_propose_k(&mut seq, target_before, k)?;
    let mut sampler = GpuSampler::new(SamplingParams {
        temperature: 0.0,
        ..SamplingParams::default()
    });
    let target_after = model.sample_last_logits(&mut sampler)?;
    if draft.len() != k || target_after != target_before {
        return Err("MTP zmieniło logity targetu".into());
    }
    println!(
        "MTP K={k}: {draft:?} => {:?}; {:.3} ms, mode={}, host gathers={}",
        tokenizer.decode(&draft, false)?,
        started.elapsed().as_secs_f64() * 1000.0,
        model.mtp_embedding_mode().unwrap_or("brak"),
        model.mtp_host_embedding_gathers()
    );
    let cycle_started = std::time::Instant::now();
    let (cycle_draft, accepted, correction) = model.native_mtp_step(&mut seq, target_before, k)?;
    println!(
        "MTP cycle K={k}: fed={target_before}, draft={cycle_draft:?}, accepted={accepted}, correction={correction}; {:.3} ms",
        cycle_started.elapsed().as_secs_f64() * 1000.0,
    );
    let mut sampler = GpuSampler::new(SamplingParams {
        temperature: 0.0,
        ..SamplingParams::default()
    });
    let native_next = model.step_and_sample(&mut seq, correction, &mut sampler)?;
    model.release_seq(&mut seq);

    let mut serial = model.new_seq();
    model.prefill_chunk(&mut serial, &prompt)?;
    let mut sampler = GpuSampler::new(SamplingParams {
        temperature: 0.0,
        ..SamplingParams::default()
    });
    if model.sample_last_logits(&mut sampler)? != target_before {
        return Err("reset sekwencji zmienił pierwszy token targetu".into());
    }
    let mut sampler = GpuSampler::new(SamplingParams {
        temperature: 0.0,
        ..SamplingParams::default()
    });
    let predicted_after_fed = model.step_and_sample(&mut serial, target_before, &mut sampler)?;
    let expected_after_fed = cycle_draft
        .first()
        .copied()
        .filter(|_| accepted > 0)
        .unwrap_or(correction);
    if predicted_after_fed != expected_after_fed {
        return Err("pierwsza decyzja verifiera nie zgadza się z targetem".into());
    }
    for index in 0..accepted {
        let mut sampler = GpuSampler::new(SamplingParams {
            temperature: 0.0,
            ..SamplingParams::default()
        });
        let predicted = model.step_and_sample(&mut serial, cycle_draft[index], &mut sampler)?;
        let expected = cycle_draft.get(index + 1).copied().unwrap_or(correction);
        if predicted != expected && index + 1 < accepted {
            return Err(format!("verifier błędnie zaakceptował pozycję {index}").into());
        }
        if index + 1 == accepted && predicted != correction {
            return Err("korekta verifiera nie zgadza się z targetem".into());
        }
    }
    let mut sampler = GpuSampler::new(SamplingParams {
        temperature: 0.0,
        ..SamplingParams::default()
    });
    let serial_next = model.step_and_sample(&mut serial, correction, &mut sampler)?;
    if serial_next != native_next {
        return Err("commit KV/SSM różni się od sekwencyjnego targetu".into());
    }
    println!("serial parity: next={serial_next}");
    model.release_seq(&mut serial);
    Ok(())
}
