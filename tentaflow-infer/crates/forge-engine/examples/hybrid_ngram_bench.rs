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
use forge_engine::speculation::{SpeculationCoordinator, SpeculativeConfig, SpeculativeState};
use forge_hal::{PoolSizes, gpu};
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

fn interleaving_audit(
    model: &mut Model,
    first_prompt: &[u32],
    second_prompt: &[u32],
    target: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let first_serial = serial_trial(model, first_prompt, target)?.tokens;
    let second_serial = serial_trial(model, second_prompt, target)?.tokens;
    let (mut first_seq, mut first_fed) = prepare(model, first_prompt)?;
    let (mut second_seq, mut second_fed) = prepare(model, second_prompt)?;
    let mut first = Vec::with_capacity(target);
    let mut second = Vec::with_capacity(target);
    for _ in 0..target {
        first.push(first_fed);
        first_fed = model.step_and_sample(&mut first_seq, first_fed, &mut greedy_sampler())?;
        second.push(second_fed);
        second_fed = model.step_and_sample(&mut second_seq, second_fed, &mut greedy_sampler())?;
    }
    model.release_seq(&mut first_seq);
    model.release_seq(&mut second_seq);
    if first != first_serial || second != second_serial {
        return Err("przeplatane stany DeltaNet różnią się od przebiegów serialnych".into());
    }
    println!("hybrid interleaving A/B: {target} + {target} tokenów, parity PASS");
    Ok(())
}

struct MtpLane {
    seq: forge_engine::kv::SeqKv,
    fed: u32,
    tokens: Vec<u32>,
}

fn advance_pure_mtp(
    model: &mut Model,
    lane: &mut MtpLane,
    budget: usize,
    target: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    if lane.tokens.len() >= target {
        return Ok(());
    }
    lane.tokens.push(lane.fed);
    if lane.tokens.len() >= target {
        return Ok(());
    }
    let (draft, accepted, correction) = model.native_mtp_step(&mut lane.seq, lane.fed, budget)?;
    for &token in &draft[..accepted] {
        if lane.tokens.len() < target {
            lane.tokens.push(token);
        }
    }
    lane.fed = correction;
    Ok(())
}

fn pure_mtp_tokens(
    model: &mut Model,
    prompt: &[u32],
    budget: usize,
    target: usize,
) -> Result<Vec<u32>, Box<dyn std::error::Error>> {
    let (seq, fed) = prepare(model, prompt)?;
    let mut lane = MtpLane {
        seq,
        fed,
        tokens: Vec::with_capacity(target),
    };
    while lane.tokens.len() < target {
        advance_pure_mtp(model, &mut lane, budget, target)?;
    }
    model.release_seq(&mut lane.seq);
    Ok(lane.tokens)
}

struct ComboLane {
    seq: forge_engine::kv::SeqKv,
    fed: u32,
    tokens: Vec<u32>,
    proposer: SpeculativeState,
}

fn advance_mtp_ngram(
    model: &mut Model,
    lane: &mut ComboLane,
    budget: usize,
    target: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    if lane.tokens.len() >= target {
        return Ok(());
    }
    lane.tokens.push(lane.fed);
    lane.proposer.observe(lane.fed);
    if lane.tokens.len() >= target {
        return Ok(());
    }
    let draft = lane.proposer.draft(budget)?;
    let (accepted, correction) = if draft.len() == budget {
        let result = model.verify_greedy_draft_with_mtp_catchup(&mut lane.seq, lane.fed, &draft)?;
        lane.proposer.commit(&draft, result.0)?;
        result
    } else {
        lane.proposer.cancel_draft();
        let (mtp_draft, accepted, correction) =
            model.native_mtp_step(&mut lane.seq, lane.fed, budget)?;
        lane.proposer.observe_all(&mtp_draft[..accepted]);
        for &token in &mtp_draft[..accepted] {
            if lane.tokens.len() < target {
                lane.tokens.push(token);
            }
        }
        lane.fed = correction;
        return Ok(());
    };
    for &token in &draft[..accepted] {
        if lane.tokens.len() < target {
            lane.tokens.push(token);
        }
    }
    lane.fed = correction;
    Ok(())
}

fn mtp_interleaving_audit(
    model: &mut Model,
    first_prompt: &[u32],
    second_prompt: &[u32],
    budget: usize,
    target: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let first_serial = pure_mtp_tokens(model, first_prompt, budget, target)?;
    let second_serial = pure_mtp_tokens(model, second_prompt, budget, target)?;
    let (mut same_first_seq, same_first_fed) = prepare(model, first_prompt)?;
    let (mut same_second_seq, same_second_fed) = prepare(model, first_prompt)?;
    let same_first_snapshot = model.debug_mtp_state_snapshot(&same_first_seq)?;
    let same_second_snapshot = model.debug_mtp_state_snapshot(&same_second_seq)?;
    compare_snapshots(&same_first_snapshot, &same_second_snapshot)?;
    let same_first_draft = model.mtp_propose_k(&mut same_first_seq, same_first_fed, budget)?;
    let same_second_draft = model.mtp_propose_k(&mut same_second_seq, same_second_fed, budget)?;
    model.release_seq(&mut same_first_seq);
    model.release_seq(&mut same_second_seq);
    if same_first_fed != same_second_fed || same_first_draft != same_second_draft {
        return Err(format!(
            "dwa świeże sloty MTP różnią się dla jednego promptu: fed {same_first_fed}/{same_second_fed}, draft {same_first_draft:?}/{same_second_draft:?}"
        )
        .into());
    }
    let (mut first_probe_seq, first_probe_fed) = prepare(model, first_prompt)?;
    let first_probe = model.mtp_propose_k(&mut first_probe_seq, first_probe_fed, budget)?;
    model.release_seq(&mut first_probe_seq);
    let (mut second_probe_seq, second_probe_fed) = prepare(model, second_prompt)?;
    let second_probe = model.mtp_propose_k(&mut second_probe_seq, second_probe_fed, budget)?;
    model.release_seq(&mut second_probe_seq);
    let (mut first_probe_seq, first_probe_fed_ab) = prepare(model, first_prompt)?;
    let (mut second_probe_seq, second_probe_fed_ab) = prepare(model, second_prompt)?;
    let first_probe_ab = model.mtp_propose_k(&mut first_probe_seq, first_probe_fed_ab, budget)?;
    let second_probe_ab =
        model.mtp_propose_k(&mut second_probe_seq, second_probe_fed_ab, budget)?;
    model.release_seq(&mut first_probe_seq);
    model.release_seq(&mut second_probe_seq);
    if first_probe_fed != first_probe_fed_ab
        || second_probe_fed != second_probe_fed_ab
        || first_probe != first_probe_ab
        || second_probe != second_probe_ab
    {
        return Err(format!(
            "proposer MTP różni się serial/A-B: A {first_probe_fed} {first_probe:?} / {first_probe_fed_ab} {first_probe_ab:?}; B {second_probe_fed} {second_probe:?} / {second_probe_fed_ab} {second_probe_ab:?}"
        )
        .into());
    }
    let (first_seq, first_fed) = prepare(model, first_prompt)?;
    let (second_seq, second_fed) = prepare(model, second_prompt)?;
    let mut first = MtpLane {
        seq: first_seq,
        fed: first_fed,
        tokens: Vec::with_capacity(target),
    };
    let mut second = MtpLane {
        seq: second_seq,
        fed: second_fed,
        tokens: Vec::with_capacity(target),
    };
    while first.tokens.len() < target || second.tokens.len() < target {
        advance_pure_mtp(model, &mut first, budget, target)?;
        advance_pure_mtp(model, &mut second, budget, target)?;
    }
    model.release_seq(&mut first.seq);
    model.release_seq(&mut second.seq);
    if first.tokens != first_serial || second.tokens != second_serial {
        let first_mismatch = first
            .tokens
            .iter()
            .zip(&first_serial)
            .position(|(actual, expected)| actual != expected);
        let second_mismatch = second
            .tokens
            .iter()
            .zip(&second_serial)
            .position(|(actual, expected)| actual != expected);
        return Err(format!(
            "przeplatany pure MTP różni się od serialnego: A={first_mismatch:?} actual={:?} expected={:?}; B={second_mismatch:?} actual={:?} expected={:?}",
            first.tokens, first_serial, second.tokens, second_serial
        )
        .into());
    }

    let first_combo = ngram_trial(model, first_prompt, budget, target, None, true)?.tokens;
    let second_combo = ngram_trial(model, second_prompt, budget, target, None, true)?.tokens;
    let coordinator = SpeculationCoordinator::new(SpeculativeConfig::ngram(budget)?)?;
    let (first_seq, first_fed) = prepare(model, first_prompt)?;
    let (second_seq, second_fed) = prepare(model, second_prompt)?;
    let mut first = ComboLane {
        seq: first_seq,
        fed: first_fed,
        tokens: Vec::with_capacity(target),
        proposer: coordinator
            .new_state(first_prompt)?
            .expect("n-gram ma stan hostowy"),
    };
    let mut second = ComboLane {
        seq: second_seq,
        fed: second_fed,
        tokens: Vec::with_capacity(target),
        proposer: coordinator
            .new_state(second_prompt)?
            .expect("n-gram ma stan hostowy"),
    };
    while first.tokens.len() < target || second.tokens.len() < target {
        advance_mtp_ngram(model, &mut first, budget, target)?;
        advance_mtp_ngram(model, &mut second, budget, target)?;
    }
    model.release_seq(&mut first.seq);
    model.release_seq(&mut second.seq);
    if first.tokens != first_combo || second.tokens != second_combo {
        return Err("przeplatany MTP+n-gram różni się od przebiegów serialnych".into());
    }
    let (mut cancel_seq, cancel_fed) = prepare(model, first_prompt)?;
    let before_cancel = model.debug_mtp_state_snapshot(&cancel_seq)?;
    let first_draft = model.mtp_propose_k(&mut cancel_seq, cancel_fed, budget)?;
    let after_cancel = model.debug_mtp_state_snapshot(&cancel_seq)?;
    if before_cancel != after_cancel {
        return Err("cancel MTP nie odtworzył stanu sekwencji".into());
    }
    model.release_seq(&mut cancel_seq);
    let (mut reused_seq, reused_fed) = prepare(model, first_prompt)?;
    let reused_draft = model.mtp_propose_k(&mut reused_seq, reused_fed, budget)?;
    model.release_seq(&mut reused_seq);
    if cancel_fed != reused_fed || first_draft != reused_draft {
        return Err("release/reuse slotu MTP zmienił deterministyczny draft".into());
    }
    println!("MTP interleaving pure + n-gram: {target} + {target} tokenów, parity PASS");
    Ok(())
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
        .then(|| model.debug_mtp_state_snapshot(&batch_seq))
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
        .then(|| model.debug_mtp_state_snapshot(&serial_seq))
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
    let first_prompt_mtp = model.debug_mtp_state_snapshot(&first_prompt_seq)?;
    model.release_seq(&mut first_prompt_seq);
    let (mut second_prompt_seq, _) = prepare(model, prompt)?;
    let second_prompt_mtp = model.debug_mtp_state_snapshot(&second_prompt_seq)?;
    model.release_seq(&mut second_prompt_seq);
    compare_snapshots(&first_prompt_mtp, &second_prompt_mtp)?;
    let (mut verifier_seq, verifier_fed) = prepare(model, prompt)?;
    let verifier_mtp_before = model.debug_mtp_state_snapshot(&verifier_seq)?;
    let _ = model.verify_greedy_draft(&mut verifier_seq, verifier_fed, &oracle[1..=budget])?;
    let verifier_mtp_after = model.debug_mtp_state_snapshot(&verifier_seq)?;
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
            model.debug_mtp_catchup_token(&mut serial_seq, token)?;
        }
        let serial_target = model.debug_hybrid_state_snapshot()?;
        let serial_mtp = model.debug_mtp_state_snapshot(&serial_seq)?;
        let serial_x = serial_target
            .iter()
            .find(|(name, _, _)| name == "x")
            .expect("snapshot targetu ma x");
        let serial_hidden = serial_mtp
            .iter()
            .find(|(name, _, _)| name == "mtp.hidden")
            .expect("snapshot MTP ma hidden");
        if serial_x.2 != serial_hidden.2 {
            return Err(format!(
                "serial recurrent_hidden różni się od target x dla accepted={expected}"
            )
            .into());
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
        let batch_mtp = model.debug_mtp_state_snapshot(&batch_seq)?;
        let batch_x = batch_target
            .iter()
            .find(|(name, _, _)| name == "x")
            .expect("snapshot targetu ma x");
        let batch_hidden = batch_mtp
            .iter()
            .find(|(name, _, _)| name == "mtp.hidden")
            .expect("snapshot MTP ma hidden");
        if batch_x.2 != batch_hidden.2 {
            return Err(format!(
                "batch recurrent_hidden różni się od target x dla accepted={expected}"
            )
            .into());
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
    let free = gpu::free_vram(0)?;
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
    let device: Arc<dyn Device> = gpu::open(
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
    let second_prompt = tokenizer.encode(
        "Write one concise sentence about deterministic GPU execution.",
        true,
    )?;
    let mut model = Model::load_gguf(
        device,
        &path,
        ModelConfig {
            weight_host_budget: 0,
weight_spill_dir: None,
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
    if std::env::var_os("FORGE_HYBRID_INTERLEAVE_AUDIT").is_some() {
        return interleaving_audit(&mut model, &prompt, &second_prompt, target.min(32));
    }
    if std::env::var_os("FORGE_MTP_INTERLEAVE_AUDIT").is_some() {
        if !mtp_router {
            return Err("audyt przeplatania MTP wymaga trybu mtp+ngram".into());
        }
        model.preflight_hybrid_state_slots(2)?;
        return mtp_interleaving_audit(&mut model, &prompt, &second_prompt, budget, target.min(32));
    }

    let warm_oracle = serial_trial(&mut model, &prompt, 8)?;
    ngram_trial(&mut model, &prompt, budget, 8, None, mtp_router)?;
    oracle_ngram_trial(&mut model, &prompt, &warm_oracle.tokens, budget, mtp_router)?;
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
    let oracle_ngram = oracle_ngram_trial(&mut model, &prompt, &serial.tokens, budget, mtp_router)?;
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
