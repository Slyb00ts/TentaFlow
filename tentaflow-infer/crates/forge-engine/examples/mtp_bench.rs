// =============================================================================
// Plik: mtp_bench.rs
// Opis: Porównuje sekwencyjny greedy decode z natywnym MTP K=2/3 bez
//       ponownego ładowania wag między próbami.
// Przykład: cargo run -p forge-engine --release --example mtp_bench -- model.gguf 3 128 512 prose
// =============================================================================

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use forge_engine::model::{Model, ModelConfig};
use forge_engine::sample::{GpuSampler, SamplingParams};
use forge_hal::cuda::{CudaDevice, PoolSizes};
use forge_hal::Device;
use forge_tokenize::Tokenizer;

struct Trial {
    elapsed: Duration,
    emitted: usize,
    accepted: usize,
    accepted_by_position: Vec<usize>,
    drafted: usize,
    drafted_by_position: Vec<usize>,
    cycles: usize,
    tokens: Vec<u32>,
}

fn greedy_sampler() -> GpuSampler {
    GpuSampler::new(SamplingParams {
        temperature: 0.0,
        ..SamplingParams::default()
    })
}

fn prepare(model: &mut Model, prompt: &[u32]) -> Result<(forge_engine::kv::SeqKv, u32), Box<dyn std::error::Error>> {
    let mut seq = model.new_seq();
    model.prefill_chunk(&mut seq, prompt)?;
    let first = model.sample_last_logits(&mut greedy_sampler())?;
    Ok((seq, first))
}

fn serial_trial(
    model: &mut Model,
    prompt: &[u32],
    target: usize,
) -> Result<Trial, Box<dyn std::error::Error>> {
    let (mut seq, mut fed) = prepare(model, prompt)?;
    let mut tokens = Vec::with_capacity(target);
    let started = Instant::now();
    for _ in 0..target {
        tokens.push(fed);
        fed = model.step_and_sample(&mut seq, fed, &mut greedy_sampler())?;
    }
    let elapsed = started.elapsed();
    model.release_seq(&mut seq);
    Ok(Trial {
        elapsed,
        emitted: target,
        accepted: 0,
        accepted_by_position: Vec::new(),
        drafted: 0,
        drafted_by_position: Vec::new(),
        cycles: target,
        tokens,
    })
}

fn mtp_trial(
    model: &mut Model,
    prompt: &[u32],
    budget: usize,
    target: usize,
) -> Result<Trial, Box<dyn std::error::Error>> {
    let (mut seq, mut fed) = prepare(model, prompt)?;
    let mut accepted_total = 0usize;
    let mut accepted_by_position = vec![0usize; budget];
    let mut drafted_by_position = vec![0usize; budget];
    let mut cycles = 0usize;
    let mut tokens = Vec::with_capacity(target);
    let started = Instant::now();
    while tokens.len() < target {
        tokens.push(fed);
        let (draft, accepted, correction) = model
            .native_mtp_step(&mut seq, fed, budget)
            .map_err(|error| format!("cykl MTP {cycles}, fed={fed}: {error}"))?;
        for &token in &draft[..accepted] {
            if tokens.len() < target {
                tokens.push(token);
            }
        }
        for position in accepted_by_position.iter_mut().take(accepted) {
            *position += 1;
        }
        for position in &mut drafted_by_position {
            *position += 1;
        }
        accepted_total += accepted;
        cycles += 1;
        fed = correction;
    }
    let elapsed = started.elapsed();
    model.release_seq(&mut seq);
    Ok(Trial {
        elapsed,
        emitted: tokens.len(),
        accepted: accepted_total,
        accepted_by_position,
        drafted: cycles * budget,
        drafted_by_position,
        cycles,
        tokens,
    })
}

fn adaptive_mtp_trial(
    model: &mut Model,
    prompt: &[u32],
    target: usize,
) -> Result<Trial, Box<dyn std::error::Error>> {
    let (mut seq, mut fed) = prepare(model, prompt)?;
    let mut accepted_total = 0usize;
    let mut accepted_by_position = vec![0usize; 3];
    let mut drafted_total = 0usize;
    let mut drafted_by_position = vec![0usize; 3];
    let mut cycles = 0usize;
    let mut k2_rate = None;
    let mut k3_rate = None;
    let mut tokens = Vec::with_capacity(target);
    let started = Instant::now();
    while tokens.len() < target {
        tokens.push(fed);
        let preferred = if cycles < 4 {
            if cycles.is_multiple_of(2) { 3 } else { 2 }
        } else if k3_rate.unwrap_or(0.0) >= k2_rate.unwrap_or(0.0) {
            3
        } else {
            2
        };
        let budget = if cycles >= 4 && cycles.is_multiple_of(16) {
            if preferred == 3 { 2 } else { 3 }
        } else {
            preferred
        };
        let cycle_started = Instant::now();
        let (draft, accepted, correction) = model
            .native_mtp_step(&mut seq, fed, budget)
            .map_err(|error| format!("adaptacyjny cykl MTP {cycles}, fed={fed}: {error}"))?;
        let cycle_rate = (accepted + 1) as f64 / cycle_started.elapsed().as_secs_f64();
        if cycles > 0 {
            let rate = if budget == 2 { &mut k2_rate } else { &mut k3_rate };
            *rate = Some(rate.map_or(cycle_rate, |previous| previous * 0.75 + cycle_rate * 0.25));
        }
        for &token in &draft[..accepted] {
            if tokens.len() < target {
                tokens.push(token);
            }
        }
        for position in accepted_by_position.iter_mut().take(accepted) {
            *position += 1;
        }
        for position in drafted_by_position.iter_mut().take(budget) {
            *position += 1;
        }
        accepted_total += accepted;
        drafted_total += budget;
        cycles += 1;
        fed = correction;
    }
    let elapsed = started.elapsed();
    model.release_seq(&mut seq);
    Ok(Trial {
        elapsed,
        emitted: tokens.len(),
        accepted: accepted_total,
        accepted_by_position,
        drafted: drafted_total,
        drafted_by_position,
        cycles,
        tokens,
    })
}

fn percentile_ms(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(args.next().expect("ścieżka modelu GGUF"));
    let mode = args.next().unwrap_or_else(|| "3".into());
    let adaptive = mode == "adaptive";
    let budget = if adaptive { 3 } else { mode.parse()? };
    let target = args.next().map(|v| v.parse()).transpose()?.unwrap_or(32);
    let prompt_tokens: usize = args.next().map(|v| v.parse()).transpose()?.unwrap_or(128);
    let prompt_kind = args.next().unwrap_or_else(|| "repeat".into());
    let max_seq_len = prompt_tokens
        .checked_add(target)
        .and_then(|length| length.checked_add(budget + 8))
        .ok_or("przepełnienie długości benchmarku")?;
    let activations = 1usize << 30;
    let kv_cache = 64usize << 20;
    let reserve = 512usize << 20;
    let free = CudaDevice::free_vram(0)?;
    eprintln!("VRAM free={} MiB, max_seq_len={max_seq_len}", free >> 20);
    let weights = free
        .checked_sub(activations + kv_cache + reserve)
        .ok_or("za mało wolnego VRAM na benchmark MTP")?;
    let device = CudaDevice::new(
        0,
        PoolSizes {
            weights,
            kv_cache,
            activations,
            kv_page_size: PoolSizes::DEFAULT_KV_PAGE,
        },
    )?;
    eprintln!("pule CUDA gotowe");
    let gguf = forge_formats::Gguf::open(&path)?;
    let vocab = forge_engine::gguf_vocab::gguf_vocab(&gguf)?;
    drop(gguf);
    let tokenizer = Tokenizer::from_gguf_vocab(&vocab)?;
    let prompt_text = match prompt_kind.as_str() {
        "repeat" => " abc",
        "prose" => {
            "Kraków przez stulecia był ważnym ośrodkiem nauki, kultury i handlu. Jego historia łączy średniowieczną architekturę z codziennym życiem współczesnego miasta. "
        }
        _ => return Err("rodzaj promptu musi być równy repeat albo prose".into()),
    };
    let unit = tokenizer.encode(prompt_text, false)?;
    if unit.is_empty() {
        return Err("tokenizer zwrócił pusty wzorzec promptu".into());
    }
    let mut prompt = Vec::with_capacity(prompt_tokens);
    while prompt.len() < prompt_tokens {
        prompt.extend_from_slice(&unit);
    }
    prompt.truncate(prompt_tokens);
    let device: Arc<dyn Device> = device;
    let mut model = Model::load_gguf(
        device,
        &path,
        ModelConfig {
            kv_page_size: 32,
            kv_pages: max_seq_len.div_ceil(32) + 2,
            max_seq_len,
            prefix_cache: false,
            native_mtp: true,
            ..ModelConfig::default()
        },
    )?;
    eprintln!("model załadowany, prompt={}", prompt.len());

    serial_trial(&mut model, &prompt, 8)?;
    eprintln!("warmup serial gotowy");
    if adaptive {
        adaptive_mtp_trial(&mut model, &prompt, 8)?;
    } else {
        mtp_trial(&mut model, &prompt, budget, 8)?;
    }
    eprintln!("warmup MTP gotowy");

    let mut serial = Vec::new();
    let mut mtp = Vec::new();
    for _ in 0..3 {
        serial.push(serial_trial(&mut model, &prompt, target)?);
        mtp.push(if adaptive {
            adaptive_mtp_trial(&mut model, &prompt, target)?
        } else {
            mtp_trial(&mut model, &prompt, budget, target)?
        });
    }
    for (serial_trial, mtp_trial) in serial.iter().zip(&mtp) {
        if serial_trial.tokens != mtp_trial.tokens {
            return Err("wielocyklowe MTP różni się od sekwencyjnego greedy".into());
        }
    }
    let serial_tps: Vec<f64> = serial
        .iter()
        .map(|trial| trial.emitted as f64 / trial.elapsed.as_secs_f64())
        .collect();
    let mtp_tps: Vec<f64> = mtp
        .iter()
        .map(|trial| trial.emitted as f64 / trial.elapsed.as_secs_f64())
        .collect();
    let mut cycle_ms: Vec<f64> = mtp
        .iter()
        .map(|trial| trial.elapsed.as_secs_f64() * 1000.0 / trial.cycles as f64)
        .collect();
    let accepted: usize = mtp.iter().map(|trial| trial.accepted).sum();
    let drafted = mtp.iter().map(|trial| trial.drafted).sum::<usize>();
    let accepted_by_position: Vec<f64> = (0..budget)
        .map(|position| {
            let accepted = mtp
                .iter()
                .map(|trial| trial.accepted_by_position[position])
                .sum::<usize>();
            let drafted = mtp
                .iter()
                .map(|trial| trial.drafted_by_position[position])
                .sum::<usize>();
            if drafted == 0 { 0.0 } else { accepted as f64 * 100.0 / drafted as f64 }
        })
        .collect();
    println!(
        "prompt={prompt_tokens}; kind={prompt_kind}; serial tok/s={serial_tps:?}; mtp mode={mode} tok/s={mtp_tps:?}; acceptance={:.1}%; acceptance_by_position={accepted_by_position:?}; cycle_p50={:.3} ms",
        accepted as f64 * 100.0 / drafted as f64,
        percentile_ms(&mut cycle_ms),
    );
    Ok(())
}
