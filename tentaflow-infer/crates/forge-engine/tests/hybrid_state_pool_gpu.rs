// =============================================================================
// Plik: hybrid_state_pool_gpu.rs
// Opis: Sprawdza na CUDA izolację, eventy i reuse stanów DeltaNet oraz MTP.
// Przykład: FORGE_TEST_HYBRID_GGUF=model.gguf cargo test -p forge-engine --release --test hybrid_state_pool_gpu -- --nocapture
// =============================================================================

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use forge_engine::gguf_vocab::gguf_vocab;
use forge_engine::kv::SeqKv;
use forge_engine::model::{Model, ModelConfig};
use forge_engine::sample::{GpuSampler, SamplingParams};
use forge_engine::server::{spawn_engine_batched, EngineEvent, EngineRequest};
use forge_engine::speculation::{
    ProposerKind, SpeculationCoordinator, SpeculativeConfig, SpeculativeState,
};
use forge_engine::tier::{KvTierConfig, KvTierMode};
use forge_formats::Gguf;
use forge_hal::cuda::{CudaDevice, PoolSizes};
use forge_hal::Device;
use forge_tokenize::Tokenizer;

const STEPS: usize = 6;
const EXTRA_STEPS: usize = 3;
static GPU_TEST_LOCK: Mutex<()> = Mutex::new(());

type TestResult<T> = Result<T, Box<dyn Error>>;

fn greedy_sampler() -> GpuSampler {
    GpuSampler::new(SamplingParams {
        temperature: 0.0,
        ..SamplingParams::default()
    })
}

fn prompt(vocab: usize, salt: usize, len: usize) -> Vec<u32> {
    assert!(vocab > 2, "model testowy musi mieć niepusty słownik");
    (0..len)
        .map(|index| 1 + ((salt + index * 17) % (vocab - 1)) as u32)
        .collect()
}

fn prepare(model: &mut Model, prompt: &[u32]) -> TestResult<(SeqKv, GpuSampler, u32)> {
    let mut seq = model.new_seq();
    let mut sampler = greedy_sampler();
    model.prefill_chunk(&mut seq, prompt)?;
    let next = model.sample_last_logits(&mut sampler)?;
    Ok((seq, sampler, next))
}

fn advance(
    model: &mut Model,
    seq: &mut SeqKv,
    sampler: &mut GpuSampler,
    token: u32,
) -> TestResult<u32> {
    sampler.note_token(token);
    Ok(model.step_and_sample(seq, token, sampler)?)
}

fn generate(model: &mut Model, prompt: &[u32], steps: usize) -> TestResult<Vec<u32>> {
    let (mut seq, mut sampler, mut next) = prepare(model, prompt)?;
    let result = (|| {
        let mut tokens = Vec::with_capacity(steps);
        for _ in 0..steps {
            tokens.push(next);
            next = advance(model, &mut seq, &mut sampler, next)?;
        }
        Ok(tokens)
    })();
    model.release_seq(&mut seq);
    result
}

struct MtpLane {
    seq: SeqKv,
    next: u32,
    tokens: Vec<u32>,
}

fn advance_mtp_budget(
    model: &mut Model,
    lane: &mut MtpLane,
    target: usize,
    budget: usize,
) -> TestResult<()> {
    if lane.tokens.len() >= target {
        return Ok(());
    }
    lane.tokens.push(lane.next);
    if lane.tokens.len() >= target {
        return Ok(());
    }
    let (draft, accepted, correction) =
        model.native_mtp_step(&mut lane.seq, lane.next, budget)?;
    for &token in &draft[..accepted] {
        if lane.tokens.len() < target {
            lane.tokens.push(token);
        }
    }
    lane.next = correction;
    Ok(())
}

fn generate_mtp(model: &mut Model, prompt: &[u32], target: usize) -> TestResult<Vec<u32>> {
    generate_mtp_budget(model, prompt, target, 3)
}

fn generate_mtp_budget(
    model: &mut Model,
    prompt: &[u32],
    target: usize,
    budget: usize,
) -> TestResult<Vec<u32>> {
    let (seq, _, next) = prepare(model, prompt)?;
    let mut lane = MtpLane {
        seq,
        next,
        tokens: Vec::with_capacity(target),
    };
    while lane.tokens.len() < target {
        advance_mtp_budget(model, &mut lane, target, budget)?;
    }
    model.release_seq(&mut lane.seq);
    Ok(lane.tokens)
}

fn run_native_mtp_b2_full_id_parity(path: &Path) -> TestResult<()> {
    let Some(mut model) = load_model(path, true) else {
        return Ok(());
    };
    model.preflight_hybrid_state_slots(2)?;
    let vocab = model.weights.descriptor.params.vocab_size;
    let prompts = [prompt(vocab, 613, 8), prompt(vocab, 829, 8)];

    for budget in [2, 3] {
        let expected = [
            generate_mtp_budget(&mut model, &prompts[0], STEPS, budget)?,
            generate_mtp_budget(&mut model, &prompts[1], STEPS, budget)?,
        ];
        let (first_seq, _, first_next) = prepare(&mut model, &prompts[0])?;
        let (second_seq, _, second_next) = prepare(&mut model, &prompts[1])?;
        let mut lanes = [
            MtpLane {
                seq: first_seq,
                next: first_next,
                tokens: Vec::with_capacity(STEPS),
            },
            MtpLane {
                seq: second_seq,
                next: second_next,
                tokens: Vec::with_capacity(STEPS),
            },
        ];
        while lanes.iter().any(|lane| lane.tokens.len() < STEPS) {
            for lane in &mut lanes {
                if lane.tokens.len() < STEPS {
                    lane.tokens.push(lane.next);
                }
            }
            if lanes.iter().all(|lane| lane.tokens.len() >= STEPS) {
                break;
            }
            let fed = [lanes[0].next, lanes[1].next];
            let (first, second) = lanes.split_at_mut(1);
            let decisions = model.native_mtp_step_b2(
                &mut [&mut first[0].seq, &mut second[0].seq],
                fed,
                budget,
            )?;
            for (lane, (draft, accepted, correction)) in lanes.iter_mut().zip(decisions) {
                for token in draft.into_iter().take(accepted) {
                    if lane.tokens.len() < STEPS {
                        lane.tokens.push(token);
                    }
                }
                lane.next = correction;
            }
        }
        assert_eq!(lanes[0].tokens, expected[0], "pełne ID MTP B2 lane0 K={budget}");
        assert_eq!(lanes[1].tokens, expected[1], "pełne ID MTP B2 lane1 K={budget}");
        model.release_seq(&mut lanes[0].seq);
        model.release_seq(&mut lanes[1].seq);
    }
    Ok(())
}

fn run_mtp_grouped_proposer_parity(path: &Path) -> TestResult<()> {
    let Some(mut model) = load_model(path, true) else {
        return Ok(());
    };
    model.preflight_hybrid_state_slots(2)?;
    let vocab = model.weights.descriptor.params.vocab_size;
    let (mut first, _, first_next) = prepare(&mut model, &prompt(vocab, 401, 8))?;
    let (mut second, _, second_next) = prepare(&mut model, &prompt(vocab, 557, 8))?;

    for budget in [2, 3] {
        let before_first = model.debug_mtp_state_snapshot(&first)?;
        let before_second = model.debug_mtp_state_snapshot(&second)?;
        let expected_first = model.mtp_propose_k(&mut first, first_next, budget)?;
        let expected_second = model.mtp_propose_k(&mut second, second_next, budget)?;
        let actual = model.mtp_propose_k_b2(
            &mut [&mut first, &mut second],
            [first_next, second_next],
            budget,
        )?;
        assert_eq!(actual[0], expected_first, "draft MTP B2 lane0 K={budget}");
        assert_eq!(actual[1], expected_second, "draft MTP B2 lane1 K={budget}");
        assert_eq!(model.debug_mtp_state_snapshot(&first)?, before_first);
        assert_eq!(model.debug_mtp_state_snapshot(&second)?, before_second);
    }
    model.release_seq(&mut first);
    model.release_seq(&mut second);
    Ok(())
}

struct ComboLane {
    seq: SeqKv,
    next: u32,
    tokens: Vec<u32>,
    proposer: SpeculativeState,
    ngram_forwards: usize,
}

fn seeded_ngram_state(
    coordinator: &SpeculationCoordinator,
    prompt: &[u32],
    next: u32,
    vocab: usize,
) -> TestResult<SpeculativeState> {
    let mut history = prompt.to_vec();
    let x = next.wrapping_add(4) % vocab as u32;
    let y = next.wrapping_add(5) % vocab as u32;
    history.extend([
        x,
        y,
        next,
        next.wrapping_add(1) % vocab as u32,
        next.wrapping_add(2) % vocab as u32,
        next.wrapping_add(3) % vocab as u32,
        x,
        y,
    ]);
    Ok(coordinator
        .new_state(&history)?
        .expect("n-gram powinien mieć stan hostowy"))
}

fn advance_combo(model: &mut Model, lane: &mut ComboLane, target: usize) -> TestResult<()> {
    if lane.tokens.len() >= target {
        return Ok(());
    }
    lane.tokens.push(lane.next);
    lane.proposer.observe(lane.next);
    if lane.tokens.len() >= target {
        return Ok(());
    }
    let draft = lane.proposer.draft(3)?;
    let (accepted, correction) = if draft.len() == 3 {
        lane.ngram_forwards += 1;
        let result =
            model.verify_greedy_draft_with_mtp_catchup(&mut lane.seq, lane.next, &draft)?;
        lane.proposer.commit(&draft, result.0)?;
        result
    } else {
        lane.proposer.cancel_draft();
        let (mtp_draft, accepted, correction) =
            model.native_mtp_step(&mut lane.seq, lane.next, 3)?;
        lane.proposer.observe_all(&mtp_draft[..accepted]);
        for &token in &mtp_draft[..accepted] {
            if lane.tokens.len() < target {
                lane.tokens.push(token);
            }
        }
        lane.next = correction;
        return Ok(());
    };
    for &token in &draft[..accepted] {
        if lane.tokens.len() < target {
            lane.tokens.push(token);
        }
    }
    lane.next = correction;
    Ok(())
}

fn generate_combo(model: &mut Model, prompt: &[u32], target: usize) -> TestResult<Vec<u32>> {
    let coordinator = SpeculationCoordinator::new(SpeculativeConfig::ngram(3)?)?;
    let (seq, _, next) = prepare(model, prompt)?;
    let vocab = model.weights.descriptor.params.vocab_size;
    let mut lane = ComboLane {
        seq,
        next,
        tokens: Vec::with_capacity(target),
        proposer: seeded_ngram_state(&coordinator, prompt, next, vocab)?,
        ngram_forwards: 0,
    };
    while lane.tokens.len() < target {
        advance_combo(model, &mut lane, target)?;
    }
    model.release_seq(&mut lane.seq);
    assert!(lane.ngram_forwards > 0, "serial MTP+n-gram nie wykonał pełnego draftu");
    Ok(lane.tokens)
}

fn load_model(path: &Path, native_mtp: bool) -> Option<Model> {
    load_model_sized(path, native_mtp, 32, 8)
}

fn load_model_sized(
    path: &Path,
    native_mtp: bool,
    max_seq_len: usize,
    kv_pages: usize,
) -> Option<Model> {
    load_model_sized_with_tier(
        path,
        native_mtp,
        max_seq_len,
        kv_pages,
        KvTierConfig::default(),
    )
}

fn load_model_sized_with_tier(
    path: &Path,
    native_mtp: bool,
    max_seq_len: usize,
    kv_pages: usize,
    kv_tier: KvTierConfig,
) -> Option<Model> {
    let free = match CudaDevice::free_vram(0) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("pominięto test puli hybrydowej: brak CUDA: {error}");
            return None;
        }
    };
    let activations = if native_mtp { 3usize << 29 } else { 1usize << 30 };
    let kv_cache = 256usize << 20;
    let reserve = 512usize << 20;
    let Some(weights) = free.checked_sub(activations + kv_cache + reserve) else {
        eprintln!("pominięto test puli hybrydowej: za mało wolnego VRAM");
        return None;
    };
    let device: Arc<dyn Device> = match CudaDevice::new(
        0,
        PoolSizes {
            weights,
            kv_cache,
            activations,
            kv_page_size: PoolSizes::DEFAULT_KV_PAGE,
        },
    ) {
        Ok(device) => device,
        Err(error) => {
            eprintln!("pominięto test puli hybrydowej: nie można utworzyć CUDA: {error}");
            return None;
        }
    };
    Some(
        Model::load_gguf(
            device,
            path,
            ModelConfig {
                kv_page_size: 32,
                kv_pages,
                max_seq_len,
                prefix_cache: false,
                native_mtp,
                kv_tier,
                ..ModelConfig::default()
            },
        )
        .expect("model hybrydowy powinien się załadować"),
    )
}

fn tokenizer(path: &Path) -> TestResult<Tokenizer> {
    let gguf = Gguf::open(path)?;
    let vocab = gguf_vocab(&gguf)?;
    Ok(Tokenizer::from_gguf_vocab(&vocab)?)
}

fn collect_events(rx: Receiver<EngineEvent>) -> TestResult<Vec<u32>> {
    let mut tokens = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_secs(120))? {
            EngineEvent::Token { id, .. } => tokens.push(id),
            EngineEvent::Done { tokens: count, .. } => {
                assert_eq!(tokens.len(), count);
                return Ok(tokens);
            }
            EngineEvent::Error(error) => return Err(error.into()),
        }
    }
}

fn wait_for_engine_state(
    description: &str,
    predicate: impl Fn() -> bool,
) -> TestResult<()> {
    let deadline = Instant::now() + Duration::from_secs(120);
    while !predicate() {
        if Instant::now() >= deadline {
            return Err(format!("timeout oczekiwania na stan silnika: {description}").into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

fn server_request(prompt_tokens: Vec<u32>, max_tokens: usize) -> EngineRequest {
    EngineRequest {
        prompt_tokens,
        max_tokens,
        sampling: SamplingParams {
            temperature: 0.0,
            ..SamplingParams::default()
        },
        ..EngineRequest::default()
    }
}

fn mixed_server_request(prompt_tokens: Vec<u32>, max_tokens: usize, lane: usize) -> EngineRequest {
    let sampling = match lane {
        0 => SamplingParams {
            temperature: 0.0,
            ..SamplingParams::default()
        },
        1 => SamplingParams {
            temperature: 0.8,
            top_k: 24,
            seed: Some(101),
            ..SamplingParams::default()
        },
        2 => SamplingParams {
            temperature: 0.65,
            top_k: 16,
            repetition_penalty: 1.12,
            frequency_penalty: 0.15,
            presence_penalty: 0.1,
            repeat_last_n: 16,
            seed: Some(202),
            ..SamplingParams::default()
        },
        _ => SamplingParams {
            temperature: 0.9,
            top_k: 32,
            top_p: 0.85,
            min_p: 0.03,
            seed: Some(303),
            ..SamplingParams::default()
        },
    };
    EngineRequest {
        prompt_tokens,
        max_tokens,
        sampling,
        ..EngineRequest::default()
    }
}

fn run_server_concurrency_two(path: &Path, spec: SpeculativeConfig) -> TestResult<()> {
    let Some(oracle_model) = load_model(path, true) else {
        return Ok(());
    };
    let vocab = oracle_model.weights.descriptor.params.vocab_size;
    let first_prompt = prompt(vocab, 29, 8);
    let second_prompt = prompt(vocab, 173, 8);
    let replacement_prompt = prompt(vocab, 313, 8);
    let reused_prompt = prompt(vocab, 367, 8);
    let oracle = spawn_engine_batched(
        oracle_model,
        Arc::new(tokenizer(path)?),
        1,
        32,
        12,
        spec.clone(),
    )?;
    let first_oracle =
        collect_events(oracle.submit(server_request(first_prompt.clone(), STEPS))?)?;
    let second_oracle =
        collect_events(oracle.submit(server_request(second_prompt.clone(), STEPS))?)?;
    let replacement_oracle = collect_events(
        oracle.submit(server_request(replacement_prompt.clone(), STEPS))?,
    )?;
    let reused_oracle =
        collect_events(oracle.submit(server_request(reused_prompt.clone(), STEPS))?)?;
    oracle.shutdown()?;

    let Some(model) = load_model(path, true) else {
        return Err("CUDA zniknęła po wykonaniu oracle serwera".into());
    };
    assert_eq!(model.weights.descriptor.params.vocab_size, vocab);
    let engine = spawn_engine_batched(model, Arc::new(tokenizer(path)?), 2, 32, 12, spec)?;
    let started = Instant::now();
    let first = engine.submit(server_request(first_prompt, STEPS))?;
    let second = engine.submit(server_request(second_prompt, STEPS))?;
    let first_tokens = collect_events(first)?;
    let second_tokens = collect_events(second)?;
    assert_eq!(first_tokens, first_oracle);
    assert_eq!(second_tokens, second_oracle);

    let cancelled = engine.submit(server_request(prompt(vocab, 257, 8), STEPS * 2))?;
    drop(cancelled);
    let replacement = engine.submit(server_request(replacement_prompt, STEPS))?;
    assert_eq!(collect_events(replacement)?, replacement_oracle);
    let reused = engine.submit(server_request(reused_prompt, STEPS))?;
    assert_eq!(collect_events(reused)?, reused_oracle);

    let metrics = engine.metrics();
    for _ in 0..200 {
        if metrics.requests_errored.load(Ordering::Relaxed) > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let generated = metrics.generated_tokens_total.load(Ordering::Relaxed);
    let elapsed = started.elapsed().as_secs_f64();
    let ttft = metrics.ttft_seconds.snapshot();
    let itl = metrics.inter_token_seconds.snapshot();
    println!(
        "server max_active=2: generated={generated} aggregate={:.2} tok/s ttft_avg={:.2} ms itl_avg={:.2} ms",
        generated as f64 / elapsed,
        ttft.sum * 1e3 / ttft.count.max(1) as f64,
        itl.sum * 1e3 / itl.count.max(1) as f64,
    );
    assert!(metrics.requests_started.load(Ordering::Relaxed) >= 5);
    assert!(metrics.requests_finished.load(Ordering::Relaxed) >= 4);
    assert!(metrics.requests_errored.load(Ordering::Relaxed) >= 1);
    engine.shutdown()?;
    Ok(())
}

fn run_server_tier_fallback(path: &Path) -> TestResult<()> {
    let Some(oracle_model) = load_model(path, true) else {
        return Ok(());
    };
    let vocab = oracle_model.weights.descriptor.params.vocab_size;
    let first_prompt = prompt(vocab, 419, 8);
    let second_prompt = prompt(vocab, 557, 8);
    let oracle = spawn_engine_batched(
        oracle_model,
        Arc::new(tokenizer(path)?),
        1,
        32,
        12,
        SpeculativeConfig::off(),
    )?;
    let first_oracle =
        collect_events(oracle.submit(server_request(first_prompt.clone(), STEPS))?)?;
    let second_oracle =
        collect_events(oracle.submit(server_request(second_prompt.clone(), STEPS))?)?;
    oracle.shutdown()?;

    let tier = KvTierConfig {
        mode: KvTierMode::Ram,
        ram_budget_bytes: 256 << 20,
        watermark: 0.25,
        ..KvTierConfig::default()
    };
    let Some(model) = load_model_sized_with_tier(path, true, 32, 40, tier) else {
        return Err("CUDA zniknęła przed testem fallbacku tieringu".into());
    };
    assert!(!model.hybrid_batch_b2_capable());
    let engine = spawn_engine_batched(
        model,
        Arc::new(tokenizer(path)?),
        2,
        32,
        12,
        SpeculativeConfig::off(),
    )?;
    let first = engine.submit(server_request(first_prompt, STEPS))?;
    let second = engine.submit(server_request(second_prompt, STEPS))?;
    assert_eq!(collect_events(first)?, first_oracle);
    assert_eq!(collect_events(second)?, second_oracle);
    assert_eq!(engine.metrics().requests_errored.load(Ordering::Relaxed), 0);
    engine.shutdown()?;
    Ok(())
}

fn run_server_hybrid_width(path: &Path, width: usize) -> TestResult<()> {
    let Some(oracle_model) = load_model(path, false) else {
        return Ok(());
    };
    let vocab = oracle_model.weights.descriptor.params.vocab_size;
    let prompts = (0..width)
        .map(|lane| prompt(vocab, 811 + lane * 137, 8))
        .collect::<Vec<_>>();
    let oracle = spawn_engine_batched(
        oracle_model,
        Arc::new(tokenizer(path)?),
        1,
        32,
        12,
        SpeculativeConfig::off(),
    )?;
    let mut expected = Vec::with_capacity(width);
    for lane_prompt in &prompts {
        expected.push(collect_events(
            oracle.submit(server_request(lane_prompt.clone(), STEPS))?,
        )?);
    }
    oracle.shutdown()?;

    let Some(model) = load_model(path, false) else {
        return Err("CUDA zniknęła przed testem szerokości hybrid batch".into());
    };
    let engine = spawn_engine_batched(
        model,
        Arc::new(tokenizer(path)?),
        width,
        32,
        12,
        SpeculativeConfig::off(),
    )?;
    let receivers = prompts
        .into_iter()
        .map(|lane_prompt| engine.submit(server_request(lane_prompt, STEPS)))
        .collect::<Result<Vec<_>, _>>()?;
    for (lane, receiver) in receivers.into_iter().enumerate() {
        let actual = collect_events(receiver)?;
        assert_eq!(
            actual,
            expected[lane],
            "lane {lane} B={width}"
        );
        println!("hybrid parity B={width} lane={lane}: IDs={actual:?}");
    }
    assert_eq!(engine.metrics().requests_errored.load(Ordering::Relaxed), 0);
    engine.shutdown()?;
    Ok(())
}

fn run_server_dynamic_width_and_sampling(path: &Path) -> TestResult<()> {
    let Some(oracle_model) = load_model_sized(path, false, 96, 8) else {
        return Ok(());
    };
    let vocab = oracle_model.weights.descriptor.params.vocab_size;
    let prompts = (0..5)
        .map(|lane| prompt(vocab, 1301 + lane * 97, 8))
        .collect::<Vec<_>>();
    let budgets = [88, 88, 84, 80];
    let replacement_budget = 76;
    let oracle = spawn_engine_batched(
        oracle_model,
        Arc::new(tokenizer(path)?),
        1,
        32,
        12,
        SpeculativeConfig::off(),
    )?;
    let mut expected = Vec::with_capacity(4);
    for lane in [0, 2, 3] {
        expected.push((lane, collect_events(oracle.submit(mixed_server_request(
            prompts[lane].clone(),
            budgets[lane],
            lane,
        ))?)?));
    }
    let replacement_expected = collect_events(oracle.submit(mixed_server_request(
        prompts[4].clone(),
        replacement_budget,
        2,
    ))?)?;
    oracle.shutdown()?;

    let Some(model) = load_model_sized(path, false, 96, 12) else {
        return Err("CUDA zniknęła przed testem dynamicznego batchu".into());
    };
    let engine = spawn_engine_batched(
        model,
        Arc::new(tokenizer(path)?),
        4,
        32,
        12,
        SpeculativeConfig::off(),
    )?;
    let mut receivers = (0..4)
        .map(|lane| {
            engine.submit(mixed_server_request(
                prompts[lane].clone(),
                budgets[lane],
                lane,
            ))
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(Some)
        .collect::<Vec<_>>();

    let metrics = engine.metrics();
    wait_for_engine_state("cztery aktywne lane'y", || {
        metrics.requests_started.load(Ordering::Relaxed) >= 4
            && metrics.active_sequences.load(Ordering::Relaxed) == 4
    })?;
    let cancelled = receivers[1]
        .take()
        .expect("środkowy lane powinien być aktywny");
    match cancelled.recv_timeout(Duration::from_secs(120))? {
        EngineEvent::Token { .. } => {}
        EngineEvent::Done { .. } => return Err("cancel test zakończył request przed drop".into()),
        EngineEvent::Error(error) => return Err(error.into()),
    }
    assert_eq!(
        metrics.requests_finished.load(Ordering::Relaxed),
        0,
        "cancel musi nastąpić, gdy pozostałe lane'y nadal są aktywne"
    );
    drop(cancelled);
    wait_for_engine_state("trzy lane'y po anulowaniu", || {
        metrics.requests_errored.load(Ordering::Relaxed) >= 1
            && metrics.active_sequences.load(Ordering::Relaxed) == 3
    })?;
    let replacement = engine.submit(mixed_server_request(
        prompts[4].clone(),
        replacement_budget,
        2,
    ))?;
    wait_for_engine_state("ponownie cztery lane'y po reuse", || {
        metrics.requests_started.load(Ordering::Relaxed) >= 5
            && metrics.active_sequences.load(Ordering::Relaxed) == 4
    })?;
    for (lane, lane_expected) in expected {
        let receiver = receivers[lane]
            .take()
            .expect("zdrowy lane powinien zachować receiver");
        assert_eq!(collect_events(receiver)?, lane_expected);
    }
    assert_eq!(collect_events(replacement)?, replacement_expected);
    assert!(metrics.requests_errored.load(Ordering::Relaxed) > 0);
    assert_eq!(metrics.requests_finished.load(Ordering::Relaxed), 4);
    engine.shutdown()?;
    Ok(())
}

fn run_model_batch_preflight_rollback(path: &Path) -> TestResult<()> {
    let Some(mut model) = load_model_sized(path, false, 32, 4) else {
        return Ok(());
    };
    let vocab = model.weights.descriptor.params.vocab_size;
    let (mut first, mut first_sampler, first_next) =
        prepare(&mut model, &prompt(vocab, 1069, 8))?;
    let (mut exhausted, mut exhausted_sampler, exhausted_next) =
        prepare(&mut model, &prompt(vocab, 1117, 32))?;
    let first_len = first.len;
    let first_pages = first.pages.clone();
    let exhausted_len = exhausted.len;
    let exhausted_pages = exhausted.pages.clone();
    let params = [
        first_sampler.batch_params(vocab),
        exhausted_sampler.batch_params(vocab),
    ];

    let result = model.batched_decode(
        &mut [&mut first, &mut exhausted],
        &[first_next, exhausted_next],
        &params,
    );
    assert!(result.is_err());
    assert_eq!(first.len, first_len);
    assert_eq!(first.pages, first_pages);
    assert_eq!(exhausted.len, exhausted_len);
    assert_eq!(exhausted.pages, exhausted_pages);
    model.release_seq(&mut first);
    model.release_seq(&mut exhausted);

    drop(model);
    let Some(mut model) = load_model_sized(path, false, 64, 2) else {
        return Err("CUDA zniknęła przed testem rezerwacji KV".into());
    };
    let vocab = model.weights.descriptor.params.vocab_size;
    let (mut growing, mut growing_sampler, growing_next) =
        prepare(&mut model, &prompt(vocab, 1181, 32))?;
    let (mut stable, mut stable_sampler, stable_next) =
        prepare(&mut model, &prompt(vocab, 1237, 8))?;
    let growing_snapshot = (
        growing.len,
        growing.tokens.clone(),
        growing.pages.clone(),
    );
    let stable_snapshot = (stable.len, stable.tokens.clone(), stable.pages.clone());
    let free_pages = model.kv.free_page_count();
    assert_eq!(free_pages, 0);
    let params = [
        growing_sampler.batch_params(vocab),
        stable_sampler.batch_params(vocab),
    ];

    let result = model.batched_decode(
        &mut [&mut growing, &mut stable],
        &[growing_next, stable_next],
        &params,
    );
    assert!(result.is_err());
    assert_eq!(
        (growing.len, growing.tokens.clone(), growing.pages.clone()),
        growing_snapshot
    );
    assert_eq!(
        (stable.len, stable.tokens.clone(), stable.pages.clone()),
        stable_snapshot
    );
    assert_eq!(model.kv.free_page_count(), free_pages);
    model.release_seq(&mut growing);
    model.release_seq(&mut stable);
    Ok(())
}

fn run_server_hol_admission(path: &Path) -> TestResult<()> {
    let Some(model) = load_model_sized(path, false, 192, 6) else {
        return Ok(());
    };
    let vocab = model.weights.descriptor.params.vocab_size;
    let engine = spawn_engine_batched(
        model,
        Arc::new(tokenizer(path)?),
        2,
        32,
        12,
        SpeculativeConfig::off(),
    )?;
    let blocker = engine.submit(server_request(prompt(vocab, 919, 8), 88))?;
    match blocker.recv_timeout(Duration::from_secs(120))? {
        EngineEvent::Token { .. } => {}
        EngineEvent::Done { .. } => {
            return Err("request blokujący skończył się przed testem HOL".into());
        }
        EngineEvent::Error(error) => return Err(error.into()),
    }
    let delayed = engine.submit(server_request(prompt(vocab, 977, 8), 120))?;
    let small = engine.submit(server_request(prompt(vocab, 1031, 8), STEPS))?;
    assert_eq!(collect_events(small)?.len(), STEPS);
    assert!(matches!(delayed.try_recv(), Err(TryRecvError::Empty)));
    drop((blocker, delayed));
    engine.shutdown()?;
    Ok(())
}

struct ServerMeasurement {
    completion_tps: f64,
    end_to_end_tps: f64,
    ttft_ms: f64,
    itl_ms: f64,
}

fn measure_server(
    path: &Path,
    max_active: usize,
    spec: SpeculativeConfig,
    native_mtp: bool,
) -> TestResult<ServerMeasurement> {
    const BENCH_TOKENS: usize = 128;
    let kv_pages = max_active * (8 + BENCH_TOKENS).div_ceil(32) + 2;
    let Some(model) = load_model_sized(path, native_mtp, 160, kv_pages) else {
        return Err("benchmark wymaga dostępnego CUDA".into());
    };
    let vocab = model.weights.descriptor.params.vocab_size;
    let engine =
        spawn_engine_batched(model, Arc::new(tokenizer(path)?), max_active, 160, 12, spec)?;

    let warmups = (0..max_active)
        .map(|slot| engine.submit(server_request(prompt(vocab, 701 + slot * 101, 8), 12)))
        .collect::<Result<Vec<_>, _>>()?;
    for warmup in warmups {
        assert_eq!(collect_events(warmup)?.len(), 12);
    }

    let metrics = engine.metrics();
    let generated_before = metrics.generated_tokens_total.load(Ordering::Relaxed);
    let ttft_before = metrics.ttft_seconds.snapshot();
    let itl_before = metrics.inter_token_seconds.snapshot();
    let started = Instant::now();
    let mut receivers = (0..max_active)
        .map(|slot| {
            engine.submit(server_request(
                prompt(vocab, 29 + slot * 144, 8),
                BENCH_TOKENS,
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut counts = vec![0usize; max_active];
    let mut done = vec![false; max_active];
    let mut first_token_at = None;
    while done.iter().any(|&finished| !finished) {
        let mut progressed = false;
        for (index, receiver) in receivers.iter_mut().enumerate() {
            if done[index] {
                continue;
            }
            match receiver.try_recv() {
                Ok(EngineEvent::Token { .. }) => {
                    first_token_at.get_or_insert_with(Instant::now);
                    counts[index] += 1;
                    progressed = true;
                }
                Ok(EngineEvent::Done { tokens, .. }) => {
                    assert_eq!(counts[index], tokens);
                    done[index] = true;
                    progressed = true;
                }
                Ok(EngineEvent::Error(error)) => return Err(error.into()),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    return Err("worker zamknął kanał benchmarku przed Done".into());
                }
            }
        }
        if !progressed {
            std::thread::sleep(Duration::from_micros(100));
        }
    }
    let finished = Instant::now();
    let first_token_at = first_token_at.ok_or("benchmark nie wyemitował tokenu")?;
    assert!(counts.iter().all(|&count| count == BENCH_TOKENS));
    let generated = metrics
        .generated_tokens_total
        .load(Ordering::Relaxed)
        .saturating_sub(generated_before);
    let ttft_after = metrics.ttft_seconds.snapshot();
    let itl_after = metrics.inter_token_seconds.snapshot();
    let ttft_count = ttft_after.count.saturating_sub(ttft_before.count);
    let itl_count = itl_after.count.saturating_sub(itl_before.count);
    let measurement = ServerMeasurement {
        completion_tps: generated as f64 / finished.duration_since(first_token_at).as_secs_f64(),
        end_to_end_tps: generated as f64 / finished.duration_since(started).as_secs_f64(),
        ttft_ms: (ttft_after.sum - ttft_before.sum) * 1e3 / ttft_count.max(1) as f64,
        itl_ms: (itl_after.sum - itl_before.sum) * 1e3 / itl_count.max(1) as f64,
    };
    engine.shutdown()?;
    Ok(measurement)
}

fn read_raw_token_ids(path: &Path) -> TestResult<Vec<u32>> {
    let bytes = std::fs::read(path)?;
    if bytes.is_empty() || bytes.len() % 4 != 0 {
        return Err("plik raw tokenów musi zawierać niepusty ciąg u32le".into());
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|chunk| u32::from_le_bytes(chunk.try_into().expect("chunk ma cztery bajty")))
        .collect())
}

fn collect_pair_events(
    receivers: [Receiver<EngineEvent>; 2],
) -> TestResult<([Vec<u32>; 2], Instant, Instant)> {
    let mut outputs = [Vec::new(), Vec::new()];
    let mut done = [false, false];
    let mut first_token_at = None;
    while done.iter().any(|&finished| !finished) {
        let mut progressed = false;
        for lane in 0..2 {
            if done[lane] {
                continue;
            }
            match receivers[lane].try_recv() {
                Ok(EngineEvent::Token { id, .. }) => {
                    first_token_at.get_or_insert_with(Instant::now);
                    outputs[lane].push(id);
                    progressed = true;
                }
                Ok(EngineEvent::Done { tokens, .. }) => {
                    assert_eq!(outputs[lane].len(), tokens);
                    done[lane] = true;
                    progressed = true;
                }
                Ok(EngineEvent::Error(error)) => return Err(error.into()),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    return Err("kanał benchmarku zamknięty przed Done".into());
                }
            }
        }
        if !progressed {
            std::thread::sleep(Duration::from_micros(100));
        }
    }
    Ok((
        outputs,
        first_token_at.ok_or("benchmark nie zwrócił tokenu")?,
        Instant::now(),
    ))
}

fn run_exact_native_mtp_b2_matrix(path: &Path, prompt_path: &Path) -> TestResult<()> {
    const OUTPUT_TOKENS: usize = 128;
    let reps = std::env::var("FORGE_BENCH_REPS")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(3);
    if reps == 0 {
        return Err("FORGE_BENCH_REPS musi być większe od zera".into());
    }
    let prompt = read_raw_token_ids(prompt_path)?;
    let max_seq_len = prompt.len() + OUTPUT_TOKENS + 32;
    let kv_pages = 2 * max_seq_len.div_ceil(32) + 4;
    let Some(model) = load_model_sized(path, true, max_seq_len, kv_pages) else {
        return Err("macierz MTP B2 wymaga dostępnego CUDA".into());
    };
    let engine = spawn_engine_batched(
        model,
        Arc::new(tokenizer(path)?),
        2,
        32,
        12,
        SpeculativeConfig::chain(vec![ProposerKind::Mtp], 3)?,
    )?;
    for rep in 0..=reps {
        let metrics = engine.metrics();
        let generated_before = metrics.generated_tokens_total.load(Ordering::Relaxed);
        let forwards_before = metrics.spec_forwards_total.load(Ordering::Relaxed);
        let accepted_before = metrics.spec_accepted_total.load(Ordering::Relaxed);
        let ttft_before = metrics.ttft_seconds.snapshot();
        let itl_before = metrics.inter_token_seconds.snapshot();
        let started = Instant::now();
        let receivers = [
            engine.submit(server_request(prompt.clone(), OUTPUT_TOKENS))?,
            engine.submit(server_request(prompt.clone(), OUTPUT_TOKENS))?,
        ];
        let (outputs, first_token_at, finished) = collect_pair_events(receivers)?;
        let elapsed = finished.duration_since(started).as_secs_f64();
        let completion_elapsed = finished.duration_since(first_token_at).as_secs_f64();
        assert_eq!(outputs[0], outputs[1], "identyczne lane'y zwróciły różne ID");
        assert_eq!(outputs[0].len(), OUTPUT_TOKENS);
        let generated = metrics
            .generated_tokens_total
            .load(Ordering::Relaxed)
            .saturating_sub(generated_before);
        let forwards = metrics
            .spec_forwards_total
            .load(Ordering::Relaxed)
            .saturating_sub(forwards_before);
        let accepted = metrics
            .spec_accepted_total
            .load(Ordering::Relaxed)
            .saturating_sub(accepted_before);
        let ttft = metrics.ttft_seconds.snapshot();
        let itl = metrics.inter_token_seconds.snapshot();
        let ttft_count = ttft.count.saturating_sub(ttft_before.count);
        let itl_count = itl.count.saturating_sub(itl_before.count);
        println!(
            "exact MTP B2 {} {}/{} raw={} generated={} aggregate_completion={:.2} tok/s aggregate_e2e={:.2} tok/s TTFT={:.2} ms effective_ITL={:.2} ms forwards={} accepted={} accepted/forward={:.3} IDs={:?}",
            if rep == 0 { "warmup" } else { "rep" },
            if rep == 0 { 1 } else { rep },
            if rep == 0 { 1 } else { reps },
            prompt.len(),
            generated,
            generated.saturating_sub(1) as f64 / completion_elapsed,
            generated as f64 / elapsed,
            (ttft.sum - ttft_before.sum) * 1e3 / ttft_count.max(1) as f64,
            (itl.sum - itl_before.sum) * 1e3 / itl_count.max(1) as f64,
            forwards,
            accepted,
            accepted as f64 / forwards.max(1) as f64,
            outputs[0],
        );
    }
    engine.shutdown()?;
    Ok(())
}

fn run_fixed_native_mtp_b2_matrix(path: &Path, prompt_path: &Path) -> TestResult<()> {
    const OUTPUT_TOKENS: usize = 128;
    let reps = std::env::var("FORGE_BENCH_REPS")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(3);
    let budget = std::env::var("FORGE_BENCH_FIXED_K")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(3);
    if reps == 0 || !matches!(budget, 2 | 3) {
        return Err("stały benchmark wymaga reps>0 i K=2 lub K=3".into());
    }
    let prompt = read_raw_token_ids(prompt_path)?;
    let max_seq_len = prompt.len() + OUTPUT_TOKENS + 32;
    let kv_pages = 2 * max_seq_len.div_ceil(32) + 4;
    let Some(mut model) = load_model_sized(path, true, max_seq_len, kv_pages) else {
        return Err("stały benchmark MTP B2 wymaga dostępnego CUDA".into());
    };
    model.preflight_hybrid_state_slots(2)?;
    for rep in 0..=reps {
        let (first_seq, _, first_next) = prepare(&mut model, &prompt)?;
        let (second_seq, _, second_next) = prepare(&mut model, &prompt)?;
        let mut lanes = [
            MtpLane {
                seq: first_seq,
                next: first_next,
                tokens: Vec::with_capacity(OUTPUT_TOKENS),
            },
            MtpLane {
                seq: second_seq,
                next: second_next,
                tokens: Vec::with_capacity(OUTPUT_TOKENS),
            },
        ];
        let started = Instant::now();
        let mut forwards = 0usize;
        while lanes[0].tokens.len() < OUTPUT_TOKENS {
            for lane in &mut lanes {
                lane.tokens.push(lane.next);
            }
            if lanes[0].tokens.len() >= OUTPUT_TOKENS {
                break;
            }
            let [first, second] = &mut lanes;
            let results = model.native_mtp_step_b2(
                &mut [&mut first.seq, &mut second.seq],
                [first.next, second.next],
                budget,
            )?;
            forwards += 1;
            for (lane, (draft, accepted, correction)) in
                lanes.iter_mut().zip(results)
            {
                for token in draft.into_iter().take(accepted) {
                    if lane.tokens.len() < OUTPUT_TOKENS {
                        lane.tokens.push(token);
                    }
                }
                lane.next = correction;
            }
        }
        let elapsed = started.elapsed().as_secs_f64();
        assert_eq!(lanes[0].tokens, lanes[1].tokens);
        println!(
            "fixed MTP B2 {} {}/{} raw={} K={} generated={} completion={:.2} tok/s forwards={} IDs={:?}",
            if rep == 0 { "warmup" } else { "rep" },
            if rep == 0 { 1 } else { rep },
            if rep == 0 { 1 } else { reps },
            prompt.len(),
            budget,
            2 * OUTPUT_TOKENS,
            (2 * OUTPUT_TOKENS) as f64 / elapsed,
            forwards,
            lanes[0].tokens,
        );
        model.release_seq(&mut lanes[0].seq);
        model.release_seq(&mut lanes[1].seq);
    }
    Ok(())
}

fn measure_serial_round_robin(path: &Path, width: usize) -> TestResult<f64> {
    const BENCH_TOKENS: usize = 128;
    let kv_pages = width * (8 + BENCH_TOKENS).div_ceil(32) + 2;
    let Some(mut model) = load_model_sized(path, false, 160, kv_pages) else {
        return Err("benchmark serialny wymaga dostępnego CUDA".into());
    };
    let vocab = model.weights.descriptor.params.vocab_size;
    let mut lanes = (0..width)
        .map(|lane| prepare(&mut model, &prompt(vocab, 29 + lane * 144, 8)))
        .collect::<TestResult<Vec<_>>>()?;
    for _ in 0..12 {
        for (seq, sampler, next) in &mut lanes {
            *next = advance(&mut model, seq, sampler, *next)?;
        }
    }
    let started = Instant::now();
    for _ in 0..BENCH_TOKENS {
        for (seq, sampler, next) in &mut lanes {
            *next = advance(&mut model, seq, sampler, *next)?;
        }
    }
    let aggregate = (width * BENCH_TOKENS) as f64 / started.elapsed().as_secs_f64();
    for (mut seq, _, _) in lanes {
        model.release_seq(&mut seq);
    }
    Ok(aggregate)
}

fn median(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}

#[test]
fn cuda_przeplatanie_release_reuse_cancel_i_error_zachowuja_stan() -> TestResult<()> {
    let Some(path) = std::env::var_os("FORGE_TEST_HYBRID_GGUF").map(PathBuf::from) else {
        eprintln!("pominięto test puli hybrydowej: brak FORGE_TEST_HYBRID_GGUF");
        return Ok(());
    };
    assert!(path.is_file(), "FORGE_TEST_HYBRID_GGUF nie wskazuje pliku");
    let _gpu_lock = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(mut model) = load_model(&path, false) else {
        return Ok(());
    };
    assert!(model.is_hybrid(), "test wymaga modelu hybrydowego DeltaNet");

    let vocab = model.weights.descriptor.params.vocab_size;
    let first_prompt = prompt(vocab, 11, 8);
    let second_prompt = prompt(vocab, 101, 8);
    let first_oracle = generate(&mut model, &first_prompt, STEPS)?;
    let second_oracle = generate(&mut model, &second_prompt, STEPS + EXTRA_STEPS)?;

    let (mut first_seq, mut first_sampler, mut first_next) = prepare(&mut model, &first_prompt)?;
    let (mut second_seq, mut second_sampler, mut second_next) =
        prepare(&mut model, &second_prompt)?;
    let mut first_tokens = Vec::with_capacity(STEPS);
    let mut second_tokens = Vec::with_capacity(STEPS + EXTRA_STEPS);
    for _ in 0..STEPS {
        first_tokens.push(first_next);
        first_next = advance(&mut model, &mut first_seq, &mut first_sampler, first_next)?;
        second_tokens.push(second_next);
        second_next = advance(
            &mut model,
            &mut second_seq,
            &mut second_sampler,
            second_next,
        )?;
    }
    assert_eq!(first_tokens, first_oracle);
    assert_eq!(second_tokens, second_oracle[..STEPS]);

    // Slot pierwszej sekwencji wraca do puli, gdy druga nadal zachowuje swój stan.
    model.release_seq(&mut first_seq);
    let reused = generate(&mut model, &first_prompt, STEPS)?;
    assert_eq!(reused, first_oracle);
    for _ in 0..EXTRA_STEPS {
        second_tokens.push(second_next);
        second_next = advance(
            &mut model,
            &mut second_seq,
            &mut second_sampler,
            second_next,
        )?;
    }
    model.release_seq(&mut second_seq);
    assert_eq!(second_tokens, second_oracle);

    // Wcześniejsze zwolnienie symuluje anulowanie requestu i musi wyzerować reuse.
    let (mut cancelled, mut cancelled_sampler, cancelled_next) =
        prepare(&mut model, &first_prompt)?;
    let _ = advance(
        &mut model,
        &mut cancelled,
        &mut cancelled_sampler,
        cancelled_next,
    )?;
    model.release_seq(&mut cancelled);
    assert_eq!(generate(&mut model, &first_prompt, STEPS)?, first_oracle);

    // Pełna strona przy limicie jednej strony wymusza błąd przed kolejnym forwardem.
    let full_prompt = prompt(vocab, 211, 32);
    let (mut failed, mut failed_sampler, failed_next) = prepare(&mut model, &full_prompt)?;
    assert!(advance(&mut model, &mut failed, &mut failed_sampler, failed_next,).is_err());
    model.release_seq(&mut failed);
    assert_eq!(generate(&mut model, &first_prompt, STEPS)?, first_oracle);

    Ok(())
}

#[test]
fn cuda_mtp_i_mtp_ngram_izoluja_przeplatane_sekwencje() -> TestResult<()> {
    let Some(path) = std::env::var_os("FORGE_TEST_HYBRID_GGUF").map(PathBuf::from) else {
        eprintln!("pominięto test puli MTP: brak FORGE_TEST_HYBRID_GGUF");
        return Ok(());
    };
    assert!(path.is_file(), "FORGE_TEST_HYBRID_GGUF nie wskazuje pliku");
    let _gpu_lock = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(mut model) = load_model(&path, true) else {
        return Ok(());
    };
    model.validate_native_mtp_target()?;
    model.preflight_hybrid_state_slots(2)?;

    let vocab = model.weights.descriptor.params.vocab_size;
    let first_prompt = prompt(vocab, 17, 8);
    let second_prompt = prompt(vocab, 131, 8);
    let first_oracle = generate_mtp(&mut model, &first_prompt, STEPS)?;
    let second_oracle = generate_mtp(&mut model, &second_prompt, STEPS)?;

    let (first_seq, _, first_next) = prepare(&mut model, &first_prompt)?;
    let (second_seq, _, second_next) = prepare(&mut model, &second_prompt)?;
    let mut first = MtpLane {
        seq: first_seq,
        next: first_next,
        tokens: Vec::with_capacity(STEPS),
    };
    let mut second = MtpLane {
        seq: second_seq,
        next: second_next,
        tokens: Vec::with_capacity(STEPS),
    };
    while first.tokens.len() < STEPS || second.tokens.len() < STEPS {
        advance_mtp_budget(&mut model, &mut first, STEPS, 3)?;
        advance_mtp_budget(&mut model, &mut second, STEPS, 3)?;
    }
    model.release_seq(&mut first.seq);
    model.release_seq(&mut second.seq);
    assert_eq!(first.tokens, first_oracle);
    assert_eq!(second.tokens, second_oracle);

    let first_combo_oracle = generate_combo(&mut model, &first_prompt, STEPS)?;
    let second_combo_oracle = generate_combo(&mut model, &second_prompt, STEPS)?;
    let coordinator = SpeculationCoordinator::new(SpeculativeConfig::ngram(3)?)?;
    let (first_seq, _, first_next) = prepare(&mut model, &first_prompt)?;
    let (second_seq, _, second_next) = prepare(&mut model, &second_prompt)?;
    let mut first = ComboLane {
        seq: first_seq,
        next: first_next,
        tokens: Vec::with_capacity(STEPS),
        proposer: seeded_ngram_state(&coordinator, &first_prompt, first_next, vocab)?,
        ngram_forwards: 0,
    };
    let mut second = ComboLane {
        seq: second_seq,
        next: second_next,
        tokens: Vec::with_capacity(STEPS),
        proposer: seeded_ngram_state(&coordinator, &second_prompt, second_next, vocab)?,
        ngram_forwards: 0,
    };
    while first.tokens.len() < STEPS || second.tokens.len() < STEPS {
        advance_combo(&mut model, &mut first, STEPS)?;
        advance_combo(&mut model, &mut second, STEPS)?;
    }
    model.release_seq(&mut first.seq);
    model.release_seq(&mut second.seq);
    assert_eq!(first.tokens, first_combo_oracle);
    assert_eq!(second.tokens, second_combo_oracle);
    assert!(first.ngram_forwards > 0, "lane A nie wykonała pełnego draftu n-gram");
    assert!(second.ngram_forwards > 0, "lane B nie wykonała pełnego draftu n-gram");

    let (mut cancelled, _, cancelled_next) = prepare(&mut model, &first_prompt)?;
    let before_cancel = model.debug_mtp_state_snapshot(&cancelled)?;
    let cancelled_draft = model.mtp_propose_k(&mut cancelled, cancelled_next, 3)?;
    let after_cancel = model.debug_mtp_state_snapshot(&cancelled)?;
    assert_eq!(after_cancel, before_cancel);
    model.release_seq(&mut cancelled);

    let (mut reused, _, reused_next) = prepare(&mut model, &first_prompt)?;
    let reused_draft = model.mtp_propose_k(&mut reused, reused_next, 3)?;
    model.release_seq(&mut reused);
    assert_eq!(reused_next, cancelled_next);
    assert_eq!(reused_draft, cancelled_draft);

    Ok(())
}

#[test]
fn native_mtp_b2_zachowuje_pelne_id_dla_k2_i_k3() -> TestResult<()> {
    let Some(path) = std::env::var_os("FORGE_TEST_HYBRID_GGUF").map(PathBuf::from) else {
        eprintln!("pominięto E2E MTP B2: brak FORGE_TEST_HYBRID_GGUF");
        return Ok(());
    };
    assert!(path.is_file(), "FORGE_TEST_HYBRID_GGUF nie wskazuje pliku");
    let _gpu_lock = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    run_native_mtp_b2_full_id_parity(&path)
}

#[test]
fn server_continuous_admission_mtp_i_router_obsluguje_concurrency_dwa() -> TestResult<()> {
    let Some(path) = std::env::var_os("FORGE_TEST_HYBRID_GGUF").map(PathBuf::from) else {
        eprintln!("pominięto E2E serwera MTP: brak FORGE_TEST_HYBRID_GGUF");
        return Ok(());
    };
    assert!(path.is_file(), "FORGE_TEST_HYBRID_GGUF nie wskazuje pliku");
    let _gpu_lock = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    run_server_concurrency_two(&path, SpeculativeConfig::off())?;
    run_server_concurrency_two(&path, SpeculativeConfig::chain(vec![ProposerKind::Mtp], 3)?)?;
    run_server_concurrency_two(
        &path,
        SpeculativeConfig::chain(vec![ProposerKind::Mtp, ProposerKind::Ngram], 3)?,
    )
}

#[test]
fn server_hybrid_target_paruje_b3_i_b4_z_serialnym_ogonem() -> TestResult<()> {
    let Some(path) = std::env::var_os("FORGE_TEST_HYBRID_GGUF").map(PathBuf::from) else {
        eprintln!("pominięto E2E hybrid B3/B4: brak FORGE_TEST_HYBRID_GGUF");
        return Ok(());
    };
    assert!(path.is_file(), "FORGE_TEST_HYBRID_GGUF nie wskazuje pliku");
    let _gpu_lock = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    run_mtp_grouped_proposer_parity(&path)?;
    run_model_batch_preflight_rollback(&path)?;
    run_server_hybrid_width(&path, 3)?;
    run_server_hybrid_width(&path, 4)?;
    run_server_dynamic_width_and_sampling(&path)
}

#[test]
fn server_admission_omija_duzy_request_czekajacy_na_kv() -> TestResult<()> {
    let Some(path) = std::env::var_os("FORGE_TEST_HYBRID_GGUF").map(PathBuf::from) else {
        eprintln!("pominięto E2E HOL admission: brak FORGE_TEST_HYBRID_GGUF");
        return Ok(());
    };
    assert!(path.is_file(), "FORGE_TEST_HYBRID_GGUF nie wskazuje pliku");
    let _gpu_lock = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    run_server_hol_admission(&path)
}

#[test]
fn hybrid_tiering_concurrency_dwa_uzywa_serial_fallback() -> TestResult<()> {
    let Some(path) = std::env::var_os("FORGE_TEST_HYBRID_GGUF").map(PathBuf::from) else {
        eprintln!("pominięto E2E tieringu B2: brak FORGE_TEST_HYBRID_GGUF");
        return Ok(());
    };
    assert!(path.is_file(), "FORGE_TEST_HYBRID_GGUF nie wskazuje pliku");
    let _gpu_lock = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    run_server_tier_fallback(&path)
}

#[test]
fn benchmark_server_mtp_max_active_jeden_kontra_dwa() -> TestResult<()> {
    if std::env::var_os("FORGE_BENCH_HYBRID_MTP").is_none() {
        eprintln!("pominięto benchmark serwera MTP: brak FORGE_BENCH_HYBRID_MTP");
        return Ok(());
    }
    let path = std::env::var_os("FORGE_TEST_HYBRID_GGUF")
        .map(PathBuf::from)
        .ok_or("benchmark wymaga FORGE_TEST_HYBRID_GGUF")?;
    let _gpu_lock = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let selected = std::env::var("FORGE_BENCH_MAX_ACTIVE")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()?
        .map_or_else(|| vec![1, 2], |max_active| vec![max_active]);
    for max_active in selected {
        assert!(matches!(max_active, 1 | 2));
        let measurement = measure_server(
            &path,
            max_active,
            SpeculativeConfig::chain(vec![ProposerKind::Mtp], 3)?,
            true,
        )?;
        println!(
            "server MTP max_active={max_active}: completion={:.2} tok/s end_to_end={:.2} tok/s TTFT={:.2} ms ITL={:.2} ms",
            measurement.completion_tps,
            measurement.end_to_end_tps,
            measurement.ttft_ms,
            measurement.itl_ms,
        );
    }
    Ok(())
}

#[test]
fn benchmark_exact_native_mtp_b2_dwa_identyczne_requesty() -> TestResult<()> {
    if std::env::var_os("FORGE_BENCH_MTP_B2_MATRIX").is_none() {
        eprintln!("pominięto dokładną macierz MTP B2: brak FORGE_BENCH_MTP_B2_MATRIX");
        return Ok(());
    }
    let path = std::env::var_os("FORGE_TEST_HYBRID_GGUF")
        .map(PathBuf::from)
        .ok_or("macierz wymaga FORGE_TEST_HYBRID_GGUF")?;
    let prompt_path = std::env::var_os("FORGE_BENCH_PROMPT_IDS")
        .map(PathBuf::from)
        .ok_or("macierz wymaga FORGE_BENCH_PROMPT_IDS")?;
    let _gpu_lock = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    run_exact_native_mtp_b2_matrix(&path, &prompt_path)
}

#[test]
fn benchmark_stale_k_native_mtp_b2_dwa_identyczne_requesty() -> TestResult<()> {
    if std::env::var_os("FORGE_BENCH_MTP_B2_FIXED").is_none() {
        eprintln!("pominięto stały benchmark MTP B2: brak FORGE_BENCH_MTP_B2_FIXED");
        return Ok(());
    }
    let path = std::env::var_os("FORGE_TEST_HYBRID_GGUF")
        .map(PathBuf::from)
        .ok_or("stały benchmark wymaga FORGE_TEST_HYBRID_GGUF")?;
    let prompt_path = std::env::var_os("FORGE_BENCH_PROMPT_IDS")
        .map(PathBuf::from)
        .ok_or("stały benchmark wymaga FORGE_BENCH_PROMPT_IDS")?;
    let _gpu_lock = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    run_fixed_native_mtp_b2_matrix(&path, &prompt_path)
}

#[test]
fn benchmark_server_hybrid_target_b1_kontra_b2() -> TestResult<()> {
    if std::env::var_os("FORGE_BENCH_HYBRID_TARGET").is_none() {
        eprintln!("pominięto benchmark targetu hybrydowego: brak FORGE_BENCH_HYBRID_TARGET");
        return Ok(());
    }
    let path = std::env::var_os("FORGE_TEST_HYBRID_GGUF")
        .map(PathBuf::from)
        .ok_or("benchmark wymaga FORGE_TEST_HYBRID_GGUF")?;
    let _gpu_lock = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let selected = std::env::var("FORGE_BENCH_MAX_ACTIVE")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()?
        .map_or_else(|| vec![1, 2, 3, 4], |max_active| vec![max_active]);
    for max_active in selected {
        assert!((1..=4).contains(&max_active));
        let repetitions = if max_active >= 3 { 3 } else { 1 };
        let mut serial_samples = Vec::with_capacity(repetitions);
        let mut paired_samples = Vec::with_capacity(repetitions);
        let mut last_measurement = None;
        for _ in 0..repetitions {
            if max_active >= 3 {
                serial_samples.push(measure_serial_round_robin(&path, max_active)?);
            }
            let measurement =
                measure_server(&path, max_active, SpeculativeConfig::off(), false)?;
            paired_samples.push(measurement.completion_tps);
            last_measurement = Some(measurement);
        }
        let measurement = last_measurement.expect("benchmark wykonuje co najmniej jedną próbę");
        println!(
            "server hybrid target max_active={max_active}: completion={:.2} tok/s end_to_end={:.2} tok/s TTFT={:.2} ms ITL={:.2} ms",
            measurement.completion_tps,
            measurement.end_to_end_tps,
            measurement.ttft_ms,
            measurement.itl_ms,
        );
        if max_active >= 3 {
            let serial = median(serial_samples.clone());
            let paired = median(paired_samples.clone());
            println!(
                "hybrid target B={max_active}: serial samples={serial_samples:?}, paired samples={paired_samples:?}, serial median={serial:.2} tok/s, paired median={paired:.2} tok/s, difference={:.2}%, speedup={:.3}x",
                (paired / serial - 1.0) * 100.0,
                paired / serial,
            );
        }
    }
    Ok(())
}
