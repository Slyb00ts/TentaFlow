// =============================================================================
// Plik: hybrid_ngram_bench.rs
// Opis: Porównuje sekwencyjny decode z n-gramem albo routerem MTP+n-gram
//       korzystającym z verifiera hybrydowego T3/T4.
// Przykład: cargo run -p forge-engine --release --example hybrid_ngram_bench -- model.gguf 3 128 128 prose mtp+ngram
// =============================================================================

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use forge_engine::model::{Model, ModelConfig};
use forge_engine::sample::{GpuSampler, SamplingParams};
use forge_engine::speculation::{SpeculationCoordinator, SpeculativeConfig};
use forge_hal::cuda::{CudaDevice, PoolSizes};
use forge_hal::Device;
use forge_tokenize::Tokenizer;
use half::f16;

struct Trial {
    elapsed: Duration,
    tokens: Vec<u32>,
    accepted: usize,
    drafted: usize,
    forwards: usize,
    ngram_forwards: usize,
    mtp_fallbacks: usize,
}

fn greedy_sampler() -> GpuSampler {
    GpuSampler::new(SamplingParams {
        temperature: 0.0,
        ..SamplingParams::default()
    })
}

fn prepare(
    model: &mut Model,
    prompt: &[u32],
) -> Result<(forge_engine::kv::SeqKv, u32), Box<dyn std::error::Error>> {
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
    while tokens.len() < target {
        tokens.push(fed);
        fed = model.step_and_sample(&mut seq, fed, &mut greedy_sampler())?;
    }
    let elapsed = started.elapsed();
    model.release_seq(&mut seq);
    Ok(Trial {
        elapsed,
        tokens,
        accepted: 0,
        drafted: 0,
        forwards: target,
        ngram_forwards: 0,
        mtp_fallbacks: 0,
    })
}

fn ngram_trial(
    model: &mut Model,
    prompt: &[u32],
    budget: usize,
    target: usize,
    oracle: Option<&[u32]>,
    mtp_router: bool,
) -> Result<Trial, Box<dyn std::error::Error>> {
    let (mut seq, mut fed) = prepare(model, prompt)?;
    let coordinator = SpeculationCoordinator::new(SpeculativeConfig::ngram(budget)?)?;
    let mut state = coordinator
        .new_state(prompt)?
        .expect("n-gram ma stan hostowy");
    let mut tokens = Vec::with_capacity(target);
    let mut accepted_total = 0usize;
    let mut drafted_total = 0usize;
    let mut forwards = 0usize;
    let mut ngram_forwards = 0usize;
    let mut mtp_fallbacks = 0usize;
    let started = Instant::now();
    while tokens.len() < target {
        let output_index = tokens.len();
        let base = seq.len;
        tokens.push(fed);
        state.observe(fed);
        let draft = state.draft(budget)?;
        if draft.len() != budget {
            state.cancel_draft();
            if mtp_router {
                let (mtp_draft, accepted, correction) =
                    model.native_mtp_step(&mut seq, fed, budget)?;
                for &token in &mtp_draft[..accepted] {
                    if tokens.len() < target {
                        tokens.push(token);
                    }
                }
                state.observe_all(&mtp_draft[..accepted]);
                accepted_total += accepted;
                drafted_total += mtp_draft.len();
                fed = correction;
                mtp_fallbacks += 1;
            } else {
                fed = model.step_and_sample(&mut seq, fed, &mut greedy_sampler())?;
            }
            forwards += 1;
            if std::env::var_os("FORGE_NGRAM_TRACE").is_some() && output_index >= 112 {
                eprintln!(
                    "cycle={forwards} output={output_index} fallback base={base} next={fed} oracle_next={:?} seq={} history={}",
                    oracle.and_then(|tokens| tokens.get(output_index + 1)),
                    seq.len,
                    state.history().len(),
                );
            }
            continue;
        }
        let (accepted, correction) = if mtp_router {
            model.verify_greedy_draft_with_mtp_catchup(&mut seq, fed, &draft)?
        } else {
            model.verify_greedy_draft(&mut seq, fed, &draft)?
        };
        state.commit(&draft, accepted)?;
        for &token in &draft[..accepted] {
            if tokens.len() < target {
                tokens.push(token);
            }
        }
        accepted_total += accepted;
        drafted_total += draft.len();
        forwards += 1;
        ngram_forwards += 1;
        if std::env::var_os("FORGE_NGRAM_TRACE").is_some() && output_index >= 112 {
            eprintln!(
                "cycle={forwards} output={output_index} base={base} fed={fed} draft={draft:?} accepted={accepted} correction={correction} oracle_correction={:?} seq={} history={}",
                oracle.and_then(|tokens| tokens.get(output_index + accepted + 1)),
                seq.len,
                state.history().len(),
            );
        }
        fed = correction;
    }
    let elapsed = started.elapsed();
    model.release_seq(&mut seq);
    Ok(Trial {
        elapsed,
        tokens,
        accepted: accepted_total,
        drafted: drafted_total,
        forwards,
        ngram_forwards,
        mtp_fallbacks,
    })
}

fn oracle_ngram_trial(
    model: &mut Model,
    prompt: &[u32],
    oracle: &[u32],
    budget: usize,
    mtp_router: bool,
) -> Result<Trial, Box<dyn std::error::Error>> {
    let (mut seq, mut fed) = prepare(model, prompt)?;
    let mut tokens = Vec::with_capacity(oracle.len());
    let mut accepted_total = 0usize;
    let mut drafted_total = 0usize;
    let mut forwards = 0usize;
    let started = Instant::now();
    while tokens.len() < oracle.len() {
        if fed != oracle[tokens.len()] {
            return Err(format!(
                "stan górnej granicy n-gram różni się od oracle na tokenie {}: {fed} != {}",
                tokens.len(),
                oracle[tokens.len()]
            )
            .into());
        }
        tokens.push(fed);
        let remaining = oracle.len() - tokens.len();
        if remaining < budget {
            fed = model.step_and_sample(&mut seq, fed, &mut greedy_sampler())?;
            forwards += 1;
            continue;
        }
        let draft = oracle[tokens.len()..tokens.len() + budget].to_vec();
        let (accepted, correction) = if mtp_router {
            model.verify_greedy_draft_with_mtp_catchup(&mut seq, fed, &draft)?
        } else {
            model.verify_greedy_draft(&mut seq, fed, &draft)?
        };
        tokens.extend_from_slice(&draft[..accepted]);
        accepted_total += accepted;
        drafted_total += draft.len();
        forwards += 1;
        fed = correction;
    }
    let elapsed = started.elapsed();
    model.release_seq(&mut seq);
    Ok(Trial {
        elapsed,
        tokens,
        accepted: accepted_total,
        drafted: drafted_total,
        forwards,
        ngram_forwards: forwards,
        mtp_fallbacks: 0,
    })
}

fn prompt_tokens(
    tokenizer: &Tokenizer,
    kind: &str,
    count: usize,
) -> Result<Vec<u32>, Box<dyn std::error::Error>> {
    let text = match kind {
        "repeat" => " abc",
        "prose" => {
            "Kraków przez stulecia był ważnym ośrodkiem nauki, kultury i handlu. Jego historia łączy średniowieczną architekturę z codziennym życiem współczesnego miasta. "
        }
        _ => return Err("rodzaj promptu musi być równy repeat albo prose".into()),
    };
    let unit = tokenizer.encode(text, false)?;
    if unit.is_empty() {
        return Err("tokenizer zwrócił pusty wzorzec promptu".into());
    }
    let mut prompt = Vec::with_capacity(count);
    while prompt.len() < count {
        prompt.extend_from_slice(&unit);
    }
    prompt.truncate(count);
    Ok(prompt)
}

fn compare_snapshots(
    batch: &[(String, usize, Vec<u8>)],
    serial: &[(String, usize, Vec<u8>)],
) -> Result<(), Box<dyn std::error::Error>> {
    if batch.len() != serial.len() {
        return Err("snapshoty mają różną liczbę buforów".into());
    }
    for ((batch_name, batch_width, batch_bytes), (serial_name, serial_width, serial_bytes)) in
        batch.iter().zip(serial)
    {
        if batch_name != serial_name
            || batch_width != serial_width
            || batch_bytes.len() != serial_bytes.len()
        {
            return Err(format!("niezgodny kształt snapshotu {batch_name}").into());
        }
        let first = batch_bytes
            .chunks_exact(*batch_width)
            .zip(serial_bytes.chunks_exact(*serial_width))
            .position(|(batch, serial)| batch != serial);
        let max_error = match batch_width {
            1 => batch_bytes
                .iter()
                .zip(serial_bytes)
                .map(|(batch, serial)| batch.abs_diff(*serial) as f32)
                .fold(0.0f32, f32::max),
            2 => batch_bytes
                .chunks_exact(2)
                .zip(serial_bytes.chunks_exact(2))
                .map(|(batch, serial)| {
                    let batch = f16::from_bits(u16::from_le_bytes([batch[0], batch[1]])).to_f32();
                    let serial =
                        f16::from_bits(u16::from_le_bytes([serial[0], serial[1]])).to_f32();
                    (batch - serial).abs()
                })
                .fold(0.0f32, f32::max),
            4 => batch_bytes
                .chunks_exact(4)
                .zip(serial_bytes.chunks_exact(4))
                .map(|(batch, serial)| {
                    let batch = f32::from_le_bytes(batch.try_into().unwrap());
                    let serial = f32::from_le_bytes(serial.try_into().unwrap());
                    (batch - serial).abs()
                })
                .fold(0.0f32, f32::max),
            _ => return Err("nieobsługiwana szerokość elementu snapshotu".into()),
        };
        eprintln!("snapshot {batch_name}: first_mismatch={first:?} max_error={max_error:e}");
        if first.is_some() {
            return Err(format!("snapshot {batch_name} różni się od referencji").into());
        }
    }
    Ok(())
}

fn diagnose_first_commit(
    model: &mut Model,
    prompt: &[u32],
    budget: usize,
    mtp_router: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let coordinator = SpeculationCoordinator::new(SpeculativeConfig::ngram(budget)?)?;
    let (mut batch_seq, batch_fed) = prepare(model, prompt)?;
    let mut state = coordinator
        .new_state(prompt)?
        .expect("n-gram ma stan hostowy");
    state.observe(batch_fed);
    let draft = state.draft(budget)?;
    if draft.len() != budget {
        model.release_seq(&mut batch_seq);
        return Err("pierwszy draft diagnostyczny nie wykorzystuje pełnego budżetu".into());
    }
    let (accepted, batch_correction) = if mtp_router {
        model.verify_greedy_draft_with_mtp_catchup(&mut batch_seq, batch_fed, &draft)?
    } else {
        model.verify_greedy_draft(&mut batch_seq, batch_fed, &draft)?
    };
    let batch = model.debug_hybrid_state_snapshot()?;
    let batch_mtp = mtp_router
        .then(|| model.debug_mtp_state_snapshot())
        .transpose()?;
    model.release_seq(&mut batch_seq);

    let (mut serial_seq, serial_fed) = prepare(model, prompt)?;
    if serial_fed != batch_fed {
        return Err("prefill diagnostyczny nie jest deterministyczny".into());
    }
    let mut serial_correction = serial_fed;
    for token in std::iter::once(serial_fed).chain(draft[..accepted].iter().copied()) {
        serial_correction = model.step_and_sample(&mut serial_seq, token, &mut greedy_sampler())?;
    }
    let serial = model.debug_hybrid_state_snapshot()?;
    let serial_mtp = mtp_router
        .then(|| model.debug_mtp_state_snapshot())
        .transpose()?;
    model.release_seq(&mut serial_seq);
    eprintln!(
        "first commit: fed={batch_fed} draft={draft:?} accepted={accepted} batch_correction={batch_correction} serial_correction={serial_correction}"
    );
    compare_snapshots(&batch, &serial)?;
    if let (Some(batch), Some(serial)) = (batch_mtp, serial_mtp) {
        compare_snapshots(&batch, &serial)?;
    }
    Ok(())
}

fn diagnose_mtp_catchup_prefixes(
    model: &mut Model,
    prompt: &[u32],
    budget: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let oracle = serial_trial(model, prompt, budget + 2)?.tokens;
    let vocab = model.weights.descriptor.params.vocab_size as u32;
    let (mut first_prompt_seq, _) = prepare(model, prompt)?;
    let first_prompt_mtp = model.debug_mtp_state_snapshot()?;
    model.release_seq(&mut first_prompt_seq);
    let (mut second_prompt_seq, _) = prepare(model, prompt)?;
    let second_prompt_mtp = model.debug_mtp_state_snapshot()?;
    model.release_seq(&mut second_prompt_seq);
    compare_snapshots(&first_prompt_mtp, &second_prompt_mtp)?;
    let (mut verifier_seq, verifier_fed) = prepare(model, prompt)?;
    let verifier_mtp_before = model.debug_mtp_state_snapshot()?;
    let _ = model.verify_greedy_draft(
        &mut verifier_seq,
        verifier_fed,
        &oracle[1..=budget],
    )?;
    let verifier_mtp_after = model.debug_mtp_state_snapshot()?;
    compare_snapshots(&verifier_mtp_before, &verifier_mtp_after)?;
    model.release_seq(&mut verifier_seq);
    for expected in 0..=budget {
        let (mut serial_seq, serial_fed) = prepare(model, prompt)?;
        if serial_fed != oracle[0] {
            return Err("serial catch-up ma inny token fed".into());
        }
        let mut serial_correction = serial_fed;
        for &token in &oracle[..=expected] {
            serial_correction =
                model.step_and_sample(&mut serial_seq, token, &mut greedy_sampler())?;
            model.debug_mtp_catchup_token(token)?;
        }
        let serial_target = model.debug_hybrid_state_snapshot()?;
        let serial_mtp = model.debug_mtp_state_snapshot()?;
        let serial_x = serial_target
            .iter()
            .find(|(name, _, _)| name == "x")
            .expect("snapshot targetu ma x");
        let serial_hidden = serial_mtp
            .iter()
            .find(|(name, _, _)| name == "mtp.hidden")
            .expect("snapshot MTP ma hidden");
        if serial_x.2 != serial_hidden.2 {
            return Err(format!("serial recurrent_hidden różni się od target x dla accepted={expected}").into());
        }
        model.release_seq(&mut serial_seq);

        let (mut batch_seq, batch_fed) = prepare(model, prompt)?;
        let mut draft = oracle[1..=budget].to_vec();
        if expected < budget {
            draft[expected] = (draft[expected] + 1) % vocab;
        }
        let (accepted, correction) =
            model.verify_greedy_draft_with_mtp_catchup(&mut batch_seq, batch_fed, &draft)?;
        if accepted != expected || correction != serial_correction {
            return Err(format!(
                "catch-up accepted={accepted}/{expected}, correction={correction}/{serial_correction}"
            )
            .into());
        }
        let batch_target = model.debug_hybrid_state_snapshot()?;
        let batch_mtp = model.debug_mtp_state_snapshot()?;
        let batch_x = batch_target
            .iter()
            .find(|(name, _, _)| name == "x")
            .expect("snapshot targetu ma x");
        let batch_hidden = batch_mtp
            .iter()
            .find(|(name, _, _)| name == "mtp.hidden")
            .expect("snapshot MTP ma hidden");
        if batch_x.2 != batch_hidden.2 {
            return Err(format!("batch recurrent_hidden różni się od target x dla accepted={expected}").into());
        }
        model.release_seq(&mut batch_seq);
        compare_snapshots(&batch_target, &serial_target)?;
        compare_snapshots(&batch_mtp, &serial_mtp)?;
        eprintln!("catch-up accepted={expected}: parity PASS");
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(args.next().expect("ścieżka modelu GGUF"));
    let budget: usize = args.next().unwrap_or_else(|| "3".into()).parse()?;
    if !matches!(budget, 2 | 3) {
        return Err("hybrydowy n-gram wymaga budżetu 2 lub 3".into());
    }
    let target: usize = args.next().map(|v| v.parse()).transpose()?.unwrap_or(128);
    let prompt_len: usize = args.next().map(|v| v.parse()).transpose()?.unwrap_or(128);
    let prompt_kind = args.next().unwrap_or_else(|| "repeat".into());
    let mode = args.next().unwrap_or_else(|| "ngram".into());
    let mtp_router = match mode.as_str() {
        "ngram" => false,
        "mtp+ngram" => true,
        _ => return Err("tryb benchmarku musi być równy ngram albo mtp+ngram".into()),
    };
    let max_seq_len = prompt_len + target + budget + 8;
    let free = CudaDevice::free_vram(0)?;
    let activations = if mtp_router {
        9usize << 27
    } else {
        1usize << 30
    };
    let kv_cache = 256usize << 20;
    let reserve = 512usize << 20;
    let weights = free
        .checked_sub(activations + kv_cache + reserve)
        .ok_or("za mało wolnego VRAM na benchmark")?;
    let device: Arc<dyn Device> = CudaDevice::new(
        0,
        PoolSizes {
            weights,
            kv_cache,
            activations,
            kv_page_size: PoolSizes::DEFAULT_KV_PAGE,
        },
    )?;
    let gguf = forge_formats::Gguf::open(&path)?;
    let vocab = forge_engine::gguf_vocab::gguf_vocab(&gguf)?;
    drop(gguf);
    let tokenizer = Tokenizer::from_gguf_vocab(&vocab)?;
    let prompt = prompt_tokens(&tokenizer, &prompt_kind, prompt_len)?;
    let mut model = Model::load_gguf(
        device,
        &path,
        ModelConfig {
            kv_page_size: 32,
            kv_pages: max_seq_len.div_ceil(32) + 2,
            max_seq_len,
            prefix_cache: false,
            native_mtp: mtp_router,
            ..ModelConfig::default()
        },
    )?;
    if mtp_router {
        model.validate_native_mtp_target()?;
    } else {
        model.validate_speculation_target(budget)?;
    }

    if std::env::var_os("FORGE_NGRAM_STATE_AUDIT").is_some() {
        diagnose_first_commit(&mut model, &prompt, budget, mtp_router)?;
    }
    if std::env::var_os("FORGE_MTP_CATCHUP_AUDIT").is_some() {
        if !mtp_router {
            return Err("audyt catch-up wymaga trybu mtp+ngram".into());
        }
        diagnose_mtp_catchup_prefixes(&mut model, &prompt, budget)?;
    }

    let warm_oracle = serial_trial(&mut model, &prompt, 8)?;
    ngram_trial(&mut model, &prompt, budget, 8, None, mtp_router)?;
    oracle_ngram_trial(
        &mut model,
        &prompt,
        &warm_oracle.tokens,
        budget,
        mtp_router,
    )?;
    let serial = serial_trial(&mut model, &prompt, target)?;
    let ngram = ngram_trial(
        &mut model,
        &prompt,
        budget,
        target,
        Some(&serial.tokens),
        mtp_router,
    )?;
    if serial.tokens != ngram.tokens {
        let index = serial
            .tokens
            .iter()
            .zip(&ngram.tokens)
            .position(|(serial, ngram)| serial != ngram)
            .unwrap_or(serial.tokens.len().min(ngram.tokens.len()));
        return Err(format!(
            "n-gram różni się od serial na tokenie {index}: {:?} != {:?}",
            serial.tokens.get(index),
            ngram.tokens.get(index)
        )
        .into());
    }
    let oracle_ngram =
        oracle_ngram_trial(&mut model, &prompt, &serial.tokens, budget, mtp_router)?;
    if serial.tokens != oracle_ngram.tokens {
        return Err("górna granica n-gram różni się tokenami od sekwencyjnego greedy".into());
    }
    let serial_tps = serial.tokens.len() as f64 / serial.elapsed.as_secs_f64();
    let ngram_tps = ngram.tokens.len() as f64 / ngram.elapsed.as_secs_f64();
    let oracle_tps = oracle_ngram.tokens.len() as f64 / oracle_ngram.elapsed.as_secs_f64();
    let acceptance = if ngram.drafted == 0 {
        0.0
    } else {
        ngram.accepted as f64 * 100.0 / ngram.drafted as f64
    };
    println!(
        "prompt={prompt_len}; kind={prompt_kind}; mode={mode}; budget={budget}; serial={serial_tps:.3} tok/s; actual={ngram_tps:.3} tok/s; speedup={:.3}x; acceptance={acceptance:.1}%; forwards={}; ngram_forwards={}; mtp_fallbacks={}; oracle_upper={oracle_tps:.3} tok/s; oracle_speedup={:.3}x",
        ngram_tps / serial_tps,
        ngram.forwards,
        ngram.ngram_forwards,
        ngram.mtp_fallbacks,
        oracle_tps / serial_tps,
    );
    Ok(())
}
