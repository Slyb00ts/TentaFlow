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

#[link(name = "dl")]
unsafe extern "C" {
    fn dlopen(filename: *const std::ffi::c_char, flags: std::ffi::c_int)
        -> *mut std::ffi::c_void;
    fn dlsym(
        handle: *mut std::ffi::c_void,
        symbol: *const std::ffi::c_char,
    ) -> *mut std::ffi::c_void;
}

fn cuda_profiler_call(symbol: &[u8]) -> TestResult<()> {
    let library = unsafe { dlopen(c"libcudart.so.13".as_ptr(), 1) };
    if library.is_null() {
        return Err("nie można otworzyć libcudart.so.13".into());
    }
    let function = unsafe { dlsym(library, symbol.as_ptr().cast()) };
    if function.is_null() {
        return Err("brak funkcji profilera CUDA".into());
    }
    let function: unsafe extern "C" fn() -> i32 = unsafe { std::mem::transmute(function) };
    let status = unsafe { function() };
    if status != 0 {
        return Err(format!("funkcja profilera CUDA zwróciła {status}").into());
    }
    Ok(())
}

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
        assert_eq!(model.mtp_embedding_mode(), Some("device"));
        assert!(
            model.native_mtp_b2_capable([&first_seq, &second_seq], budget),
            "device-only embedding musi spełniać kontrakt B2 dla K={budget}"
        );
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

fn run_mtp_ngram_b2_retained_matrix(path: &Path) -> TestResult<()> {
    let Some(mut model) = load_model(path, true) else {
        return Ok(());
    };
    model.preflight_hybrid_state_slots(2)?;
    let vocab = model.weights.descriptor.params.vocab_size;
    let prompts = [prompt(vocab, 941, 8), prompt(vocab, 1171, 8)];
    for budget in [2usize, 3] {
        let greedy = [
            generate(&mut model, &prompts[0], budget + 2)?,
            generate(&mut model, &prompts[1], budget + 2)?,
        ];
        for retained0 in 1..=budget + 1 {
            for retained1 in 1..=budget + 1 {
                let retained = [retained0, retained1];
                let mut drafts: [Vec<u32>; 2] = std::array::from_fn(|lane| {
                    greedy[lane][1..=budget].to_vec()
                });
                for lane in 0..2 {
                    if retained[lane] <= budget {
                        let index = retained[lane] - 1;
                        let expected = greedy[lane][retained[lane]];
                        let mut mismatch = expected.wrapping_add(1) % vocab as u32;
                        if mismatch == expected {
                            mismatch = expected.wrapping_add(2) % vocab as u32;
                        }
                        drafts[lane][index] = mismatch;
                    }
                }

                let mut expected_results = [(0usize, 0u32); 2];
                let mut expected_states: [Vec<(String, usize, Vec<u8>)>; 2] =
                    std::array::from_fn(|_| Vec::new());
                for lane in 0..2 {
                    let (mut seq, _, fed) = prepare(&mut model, &prompts[lane])?;
                    expected_results[lane] = model.verify_greedy_draft_with_mtp_catchup(
                        &mut seq,
                        fed,
                        &drafts[lane],
                    )?;
                    expected_states[lane] = model.debug_mtp_state_snapshot(&seq)?;
                    model.release_seq(&mut seq);
                    assert_eq!(expected_results[lane].0 + 1, retained[lane]);
                    assert_eq!(expected_results[lane].1, greedy[lane][retained[lane]]);
                }

                let (mut first, _, first_fed) = prepare(&mut model, &prompts[0])?;
                let (mut second, _, second_fed) = prepare(&mut model, &prompts[1])?;
                assert!(model.mtp_ngram_b2_capable([&first, &second], budget));
                let actual = model.verify_greedy_draft_with_mtp_catchup_b2(
                    &mut [&mut first, &mut second],
                    [first_fed, second_fed],
                    [&drafts[0], &drafts[1]],
                )?;
                assert_eq!(actual, expected_results, "K={budget}, retained={retained:?}");
                assert_mtp_snapshot_eq(
                    &model.debug_mtp_state_snapshot(&first)?,
                    &expected_states[0],
                    &format!("lane0 K={budget}, retained={retained:?}"),
                );
                assert_mtp_snapshot_eq(
                    &model.debug_mtp_state_snapshot(&second)?,
                    &expected_states[1],
                    &format!("lane1 K={budget}, retained={retained:?}"),
                );
                model.release_seq(&mut first);
                model.release_seq(&mut second);
            }
        }
    }
    Ok(())
}

fn run_mtp_routed_b2_source_masks(path: &Path) -> TestResult<()> {
    let Some(mut model) = load_model(path, true) else {
        return Ok(());
    };
    model.preflight_hybrid_state_slots(2)?;
    let vocab = model.weights.descriptor.params.vocab_size;
    let prompts = [prompt(vocab, 1301, 8), prompt(vocab, 1601, 8)];
    for budget in [2usize, 3] {
        let greedy = [
            generate(&mut model, &prompts[0], budget + 2)?,
            generate(&mut model, &prompts[1], budget + 2)?,
        ];
        for source_mask in 0u8..4 {
            let retained_values = std::array::from_fn::<_, 2, _>(|lane| {
                if source_mask & (1 << lane) != 0 {
                    (1..=budget + 1).collect::<Vec<_>>()
                } else {
                    vec![0]
                }
            });
            for &retained0 in &retained_values[0] {
                for &retained1 in &retained_values[1] {
                    let retained = [retained0, retained1];
                    let mut drafts: [Vec<u32>; 2] =
                        std::array::from_fn(|lane| greedy[lane][1..=budget].to_vec());
                    for lane in 0..2 {
                        if retained[lane] > 0 && retained[lane] <= budget {
                            let index = retained[lane] - 1;
                            let expected = greedy[lane][index + 1];
                            drafts[lane][index] = expected.wrapping_add(1) % vocab as u32;
                        }
                    }
                    let mut expected: [(Vec<u32>, usize, u32); 2] =
                        std::array::from_fn(|_| (Vec::new(), 0, 0));
                    let mut expected_states: [Vec<(String, usize, Vec<u8>)>; 2] =
                        std::array::from_fn(|_| Vec::new());
                    for lane in 0..2 {
                        let (mut seq, _, fed) = prepare(&mut model, &prompts[lane])?;
                        expected[lane] = if retained[lane] > 0 {
                            let (accepted, correction) = model.verify_greedy_draft_with_mtp_catchup(
                                &mut seq,
                                fed,
                                &drafts[lane],
                            )?;
                            assert_eq!(accepted + 1, retained[lane]);
                            (drafts[lane].clone(), accepted, correction)
                        } else {
                            model.native_mtp_step(&mut seq, fed, budget)?
                        };
                        expected_states[lane] = model.debug_mtp_state_snapshot(&seq)?;
                        model.release_seq(&mut seq);
                    }

                    let (mut first, _, first_fed) = prepare(&mut model, &prompts[0])?;
                    let (mut second, _, second_fed) = prepare(&mut model, &prompts[1])?;
                    let external = [
                        (retained[0] > 0).then_some(drafts[0].as_slice()),
                        (retained[1] > 0).then_some(drafts[1].as_slice()),
                    ];
                    let available = [
                        model.native_mtp_available_budget(&first, budget),
                        model.native_mtp_available_budget(&second, budget),
                    ];
                    let model_capable = model.mtp_ngram_b2_model_capable();
                    let embedding_mode = model.mtp_embedding_mode();
                    let actual = model.native_mtp_routed_step_b2(
                        &mut [&mut first, &mut second],
                        [first_fed, second_fed],
                        budget,
                        external,
                    ).map_err(|error| {
                        format!(
                            "routed source_mask={source_mask:02b}, K={budget}, retained={retained:?}, available={available:?}, model_capable={model_capable}, embedding={embedding_mode:?}: {error}"
                        )
                    })?;
                    assert_eq!(
                        actual, expected,
                        "source_mask={source_mask:02b}, K={budget}, retained={retained:?}"
                    );
                    assert_mtp_snapshot_eq(
                        &model.debug_mtp_state_snapshot(&first)?,
                        &expected_states[0],
                        &format!(
                            "lane0 source_mask={source_mask:02b}, K={budget}, retained={retained:?}"
                        ),
                    );
                    assert_mtp_snapshot_eq(
                        &model.debug_mtp_state_snapshot(&second)?,
                        &expected_states[1],
                        &format!(
                            "lane1 source_mask={source_mask:02b}, K={budget}, retained={retained:?}"
                        ),
                    );
                    model.release_seq(&mut first);
                    model.release_seq(&mut second);
                }
            }
        }
    }
    Ok(())
}

fn run_mtp_ngram_b2_retained_one_lane_orders(path: &Path) -> TestResult<()> {
    let Some(mut model) = load_model(path, true) else {
        return Ok(());
    };
    model.preflight_hybrid_state_slots(2)?;
    let vocab = model.weights.descriptor.params.vocab_size;
    let source_prompts = [prompt(vocab, 941, 8), prompt(vocab, 1171, 8)];
    for order in [[0usize, 1usize], [1usize, 0usize]] {
        let prompts = [
            source_prompts[order[0]].clone(),
            source_prompts[order[1]].clone(),
        ];
        let greedy = [
            generate(&mut model, &prompts[0], 4)?,
            generate(&mut model, &prompts[1], 4)?,
        ];
        let mut drafts: [Vec<u32>; 2] =
            std::array::from_fn(|lane| greedy[lane][1..=2].to_vec());
        for lane in 0..2 {
            let expected = greedy[lane][1];
            let mut mismatch = expected.wrapping_add(1) % vocab as u32;
            if mismatch == expected {
                mismatch = expected.wrapping_add(2) % vocab as u32;
            }
            drafts[lane][0] = mismatch;
        }

        let mut expected_results = [(0usize, 0u32); 2];
        let mut expected_states: [Vec<(String, usize, Vec<u8>)>; 2] =
            std::array::from_fn(|_| Vec::new());
        for lane in 0..2 {
            let (mut seq, _, fed) = prepare(&mut model, &prompts[lane])?;
            expected_results[lane] =
                model.verify_greedy_draft_with_mtp_catchup(&mut seq, fed, &drafts[lane])?;
            expected_states[lane] = model.debug_mtp_state_snapshot(&seq)?;
            model.release_seq(&mut seq);
        }

        let (mut first, _, first_fed) = prepare(&mut model, &prompts[0])?;
        let (mut second, _, second_fed) = prepare(&mut model, &prompts[1])?;
        let actual = model.verify_greedy_draft_with_mtp_catchup_b2(
            &mut [&mut first, &mut second],
            [first_fed, second_fed],
            [&drafts[0], &drafts[1]],
        )?;
        assert_eq!(actual, expected_results, "kolejność lane={order:?}");
        assert_mtp_snapshot_eq(
            &model.debug_mtp_state_snapshot(&first)?,
            &expected_states[0],
            &format!("lane0 retained=[1,1], kolejność={order:?}"),
        );
        assert_mtp_snapshot_eq(
            &model.debug_mtp_state_snapshot(&second)?,
            &expected_states[1],
            &format!("lane1 retained=[1,1], kolejność={order:?}"),
        );
        model.release_seq(&mut first);
        model.release_seq(&mut second);
    }
    Ok(())
}

fn assert_mtp_snapshot_eq(
    actual: &[(String, usize, Vec<u8>)],
    expected: &[(String, usize, Vec<u8>)],
    context: &str,
) {
    assert_eq!(actual.len(), expected.len(), "liczba buforów {context}");
    for ((actual_name, actual_element_bytes, actual_bytes), (expected_name, expected_element_bytes, expected_bytes)) in
        actual.iter().zip(expected)
    {
        assert_eq!(actual_name, expected_name, "nazwa bufora {context}");
        assert_eq!(
            actual_element_bytes, expected_element_bytes,
            "rozmiar elementu {actual_name} {context}"
        );
        assert_eq!(actual_bytes.len(), expected_bytes.len(), "długość {actual_name} {context}");
        if let Some(index) = actual_bytes
            .iter()
            .zip(expected_bytes)
            .position(|(actual_byte, expected_byte)| actual_byte != expected_byte)
        {
            panic!(
                "pierwsza różnica {actual_name} {context}: bajt {index}, actual={}, expected={}",
                actual_bytes[index], expected_bytes[index]
            );
        }
    }
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

fn generate_combo_b1_oracle(
    model: &mut Model,
    prompt: &[u32],
    target: usize,
) -> TestResult<Vec<u32>> {
    let coordinator = SpeculationCoordinator::new(SpeculativeConfig::chain(
        vec![ProposerKind::Mtp, ProposerKind::Ngram],
        3,
    )?)?;
    let (seq, _, next) = prepare(model, prompt)?;
    let mut lane = ComboLane {
        seq,
        next,
        tokens: Vec::with_capacity(target),
        proposer: coordinator
            .new_state(prompt)?
            .expect("n-gram powinien mieć stan hostowy"),
        ngram_forwards: 0,
    };
    while lane.tokens.len() < target {
        advance_combo(model, &mut lane, target)?;
    }
    model.release_seq(&mut lane.seq);
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
    let activations = std::env::var("FORGE_TEST_ACTIVATIONS_MB")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .map(|value| value << 20)
        .unwrap_or_else(|| if native_mtp { 3usize << 29 } else { 1usize << 30 });
    let kv_cache = 256usize << 20;
    let reserve = std::env::var("FORGE_TEST_POOL_RESERVE_MB")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(512)
        << 20;
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

fn id_server_request(prompt_tokens: Vec<u32>, max_tokens: usize) -> EngineRequest {
    EngineRequest {
        emit_empty_tokens: true,
        ..server_request(prompt_tokens, max_tokens)
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
    let expect_mixed = spec.kind() == forge_engine::speculation::SpeculationKind::NativeMtpNgram
        && std::env::var("FORGE_MTP_NGRAM_MIXED_BATCH").is_ok_and(|value| value == "1");
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
        collect_events(oracle.submit(id_server_request(first_prompt.clone(), STEPS))?)?;
    let second_oracle =
        collect_events(oracle.submit(id_server_request(second_prompt.clone(), STEPS))?)?;
    let replacement_oracle = collect_events(
        oracle.submit(id_server_request(replacement_prompt.clone(), STEPS))?,
    )?;
    let reused_oracle =
        collect_events(oracle.submit(id_server_request(reused_prompt.clone(), STEPS))?)?;
    oracle.shutdown()?;

    let Some(model) = load_model(path, true) else {
        return Err("CUDA zniknęła po wykonaniu oracle serwera".into());
    };
    assert_eq!(model.weights.descriptor.params.vocab_size, vocab);
    let engine = spawn_engine_batched(model, Arc::new(tokenizer(path)?), 2, 32, 12, spec)?;
    let profile = std::env::var_os("FORGE_PROFILE_SERVER_CONCURRENCY").is_some();
    if profile {
        cuda_profiler_call(b"cudaProfilerStart\0")?;
    }
    let started = Instant::now();
    let first = engine.submit(id_server_request(first_prompt, STEPS))?;
    let second = engine.submit(id_server_request(second_prompt, STEPS))?;
    let first_tokens = collect_events(first)?;
    let second_tokens = collect_events(second)?;
    assert_eq!(first_tokens, first_oracle);
    assert_eq!(second_tokens, second_oracle);

    let cancelled = engine.submit(id_server_request(prompt(vocab, 257, 8), STEPS * 2))?;
    drop(cancelled);
    let replacement = engine.submit(id_server_request(replacement_prompt, STEPS))?;
    assert_eq!(collect_events(replacement)?, replacement_oracle);
    let reused = engine.submit(id_server_request(reused_prompt, STEPS))?;
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
    if expect_mixed {
        assert!(
            metrics.mtp_routed_nm_b2_steps_total.load(Ordering::Relaxed)
                + metrics.mtp_routed_mm_b2_steps_total.load(Ordering::Relaxed)
                > 0,
            "mixed rollout nie wykonał pary N/M ani M/M"
        );
    }
    if profile {
        cuda_profiler_call(b"cudaProfilerStop\0")?;
    }
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

#[derive(Clone, Copy)]
enum ExactB2Mode {
    Native,
    MtpNgram,
}

fn run_exact_native_mtp_b2_matrix(
    path: &Path,
    prompt_path: Option<&Path>,
    mode: ExactB2Mode,
) -> TestResult<()> {
    const OUTPUT_TOKENS: usize = 128;
    let reps = std::env::var("FORGE_BENCH_REPS")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(3);
    if reps == 0 {
        return Err("FORGE_BENCH_REPS musi być większe od zera".into());
    }
    let (prompt, second_prompt) = if let Some(prompt_path) = prompt_path {
        let first = read_raw_token_ids(prompt_path)?;
        let second = std::env::var_os("FORGE_BENCH_PROMPT_IDS_SECOND")
            .map(PathBuf::from)
            .map(|path| read_raw_token_ids(&path))
            .transpose()?
            .unwrap_or_else(|| first.clone());
        (first, second)
    } else {
        let length = std::env::var("FORGE_BENCH_SYNTHETIC_PROMPT_TOKENS")
            .ok()
            .map(|value| value.parse::<usize>())
            .transpose()?
            .unwrap_or(128);
        let gguf = Gguf::open(path)?;
        let vocab = gguf_vocab(&gguf)?.tokens.len();
        (prompt(vocab, 1709, length), prompt(vocab, 2903, length))
    };
    let max_seq_len = prompt.len().max(second_prompt.len()) + OUTPUT_TOKENS + 32;
    let kv_pages = 2 * max_seq_len.div_ceil(32) + 4;
    let Some(mut oracle_model) = load_model_sized(path, true, max_seq_len, kv_pages) else {
        return Err("oracle B1 wymaga dostępnego CUDA".into());
    };
    let oracle_outputs = match mode {
        ExactB2Mode::Native => [
            generate_mtp(&mut oracle_model, &prompt, OUTPUT_TOKENS)?,
            generate_mtp(&mut oracle_model, &second_prompt, OUTPUT_TOKENS)?,
        ],
        ExactB2Mode::MtpNgram => [
            generate_combo_b1_oracle(&mut oracle_model, &prompt, OUTPUT_TOKENS)?,
            generate_combo_b1_oracle(&mut oracle_model, &second_prompt, OUTPUT_TOKENS)?,
        ],
    };
    drop(oracle_model);
    let Some(model) = load_model_sized(path, true, max_seq_len, kv_pages) else {
        return Err("macierz MTP B2 wymaga dostępnego CUDA".into());
    };
    let spec = match mode {
        ExactB2Mode::Native => SpeculativeConfig::chain(vec![ProposerKind::Mtp], 3)?,
        ExactB2Mode::MtpNgram => {
            SpeculativeConfig::chain(vec![ProposerKind::Mtp, ProposerKind::Ngram], 3)?
        }
    };
    let engine = spawn_engine_batched(
        model,
        Arc::new(tokenizer(path)?),
        2,
        32,
        12,
        spec,
    )?;
    let profile = std::env::var_os("FORGE_PROFILE_MTP_MATRIX").is_some();
    for rep in 0..=reps {
        let metrics = engine.metrics();
        let generated_before = metrics.generated_tokens_total.load(Ordering::Relaxed);
        let forwards_before = metrics.spec_forwards_total.load(Ordering::Relaxed);
        let accepted_before = metrics.spec_accepted_total.load(Ordering::Relaxed);
        let legacy_b2_before = match mode {
            ExactB2Mode::Native => metrics.native_mtp_b2_steps_total.load(Ordering::Relaxed),
            ExactB2Mode::MtpNgram => metrics.mtp_ngram_b2_steps_total.load(Ordering::Relaxed),
        };
        let routed_before = [
            metrics.mtp_routed_nn_b2_steps_total.load(Ordering::Relaxed),
            metrics.mtp_routed_nm_b2_steps_total.load(Ordering::Relaxed),
            metrics.mtp_routed_mm_b2_steps_total.load(Ordering::Relaxed),
        ];
        let ttft_before = metrics.ttft_seconds.snapshot();
        let itl_before = metrics.inter_token_seconds.snapshot();
        if profile && rep == 1 {
            cuda_profiler_call(b"cudaProfilerStart\0")?;
        }
        let started = Instant::now();
        let receivers = [
            engine.submit(id_server_request(prompt.clone(), OUTPUT_TOKENS))?,
            engine.submit(id_server_request(second_prompt.clone(), OUTPUT_TOKENS))?,
        ];
        let (outputs, first_token_at, finished) = collect_pair_events(receivers)?;
        if profile && rep == 1 {
            cuda_profiler_call(b"cudaProfilerStop\0")?;
        }
        let elapsed = finished.duration_since(started).as_secs_f64();
        let completion_elapsed = finished.duration_since(first_token_at).as_secs_f64();
        assert_eq!(outputs[0], oracle_outputs[0], "lane0 różni się od oracle B1");
        assert_eq!(outputs[1], oracle_outputs[1], "lane1 różni się od oracle B1");
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
        let legacy_b2_steps = match mode {
            ExactB2Mode::Native => metrics.native_mtp_b2_steps_total.load(Ordering::Relaxed),
            ExactB2Mode::MtpNgram => metrics.mtp_ngram_b2_steps_total.load(Ordering::Relaxed),
        }
        .saturating_sub(legacy_b2_before);
        let routed_steps = [
            metrics
                .mtp_routed_nn_b2_steps_total
                .load(Ordering::Relaxed)
                .saturating_sub(routed_before[0]),
            metrics
                .mtp_routed_nm_b2_steps_total
                .load(Ordering::Relaxed)
                .saturating_sub(routed_before[1]),
            metrics
                .mtp_routed_mm_b2_steps_total
                .load(Ordering::Relaxed)
                .saturating_sub(routed_before[2]),
        ];
        let routed_total = routed_steps.iter().sum::<u64>();
        let b2_expected = matches!(mode, ExactB2Mode::Native)
            || std::env::var("FORGE_MTP_NGRAM_BATCH").map_or(true, |value| value == "1");
        if b2_expected {
            assert!(routed_total > 0, "benchmark serwera nie wykonał oczekiwanej ścieżki B2");
        } else {
            assert_eq!(routed_total, 0, "wyłączona ścieżka routed wykonała B2");
        }
        if matches!(mode, ExactB2Mode::MtpNgram) {
            let mixed_enabled = std::env::var("FORGE_MTP_NGRAM_MIXED_BATCH")
                .is_ok_and(|value| value == "1");
            if mixed_enabled && prompt != second_prompt {
                assert!(
                    routed_steps[1] + routed_steps[2] > 0,
                    "włączony mixed routing nie wykonał N/M ani M/M"
                );
            } else if !mixed_enabled {
                assert_eq!(
                    [routed_steps[1], routed_steps[2]],
                    [0, 0],
                    "wyłączony mixed routing wykonał N/M albo M/M"
                );
            }
        }
        let ttft = metrics.ttft_seconds.snapshot();
        let itl = metrics.inter_token_seconds.snapshot();
        let ttft_count = ttft.count.saturating_sub(ttft_before.count);
        let itl_count = itl.count.saturating_sub(itl_before.count);
        println!(
            "exact {} B2 {} {}/{} raw={}/{} generated={} aggregate_completion={:.2} tok/s aggregate_e2e={:.2} tok/s TTFT={:.2} ms effective_ITL={:.2} ms forwards={} accepted={} accepted/forward={:.3} legacy_b2={} routed_NN/NM/MM={}/{}/{} IDs={:?}/{:?}",
            match mode {
                ExactB2Mode::Native => "MTP",
                ExactB2Mode::MtpNgram => "MTP+n-gram",
            },
            if rep == 0 { "warmup" } else { "rep" },
            if rep == 0 { 1 } else { rep },
            if rep == 0 { 1 } else { reps },
            prompt.len(),
            second_prompt.len(),
            generated,
            generated.saturating_sub(1) as f64 / completion_elapsed,
            generated as f64 / elapsed,
            (ttft.sum - ttft_before.sum) * 1e3 / ttft_count.max(1) as f64,
            (itl.sum - itl_before.sum) * 1e3 / itl_count.max(1) as f64,
            forwards,
            accepted,
            accepted as f64 / forwards.max(1) as f64,
            legacy_b2_steps,
            routed_steps[0],
            routed_steps[1],
            routed_steps[2],
            outputs[0],
            outputs[1],
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
fn hybrid_prefill_b2_t32_jest_bitowo_zgodny_z_dwoma_serialnymi_lane() -> TestResult<()> {
    let Some(path) = std::env::var_os("FORGE_TEST_HYBRID_GGUF").map(PathBuf::from) else {
        eprintln!("pominięto parity prefill B2 T32: brak FORGE_TEST_HYBRID_GGUF");
        return Ok(());
    };
    assert!(path.is_file(), "FORGE_TEST_HYBRID_GGUF nie wskazuje pliku");
    let _gpu_lock = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(mut model) = load_model_sized(&path, true, 64, 8) else {
        return Ok(());
    };
    if !model.hybrid_prefill_b2_capable(32) {
        let p = &model.weights.descriptor.params;
        let split_qkv = model
            .weights
            .layers
            .iter()
            .filter(|layer| matches!(
                &layer.mixer,
                forge_engine::weights::LayerMixer::Attention(attention)
                    if matches!(attention.attn_qkv, forge_engine::weights::QkvWeights::Split { .. })
            ))
            .count();
        let split_ffn = model
            .weights
            .layers
            .iter()
            .filter(|layer| matches!(
                &layer.ffn,
                forge_engine::weights::LayerFfn::Dense(ffn)
                    if matches!(ffn.gate_up, forge_engine::weights::GateUpWeights::Split { .. })
            ))
            .count();
        eprintln!(
            "pominięto parity prefill B2 T32: arch={} hd={} ssm={:?} moe={} mtp_embedding={:?} base_b2={} split_qkv={split_qkv} split_ffn={split_ffn}/{} lm_head={}",
            model.weights.descriptor.arch,
            p.head_dim,
            p.ssm.as_ref().map(|ssm| (ssm.d_state, ssm.d_conv)),
            model.weights.is_moe(),
            model.mtp_embedding_mode(),
            model.hybrid_batch_b2_capable(),
            model.weights.layers.len(),
            match model.weights.lm_head {
                forge_engine::weights::DevWeight::F16 { .. } => "f16",
                forge_engine::weights::DevWeight::Q8_0 { .. } => "q8_0",
                forge_engine::weights::DevWeight::NvFp4Gguf { .. } => "nvfp4_gguf",
                _ => "inne",
            }
        );
        return Ok(());
    }
    let vocab = model.weights.descriptor.params.vocab_size;
    let prompts = [prompt(vocab, 1709, 32), prompt(vocab, 2903, 32)];
    let mut serial_seqs = [model.new_seq(), model.new_seq()];
    let serial_logits = [
        model.prefill_chunk(&mut serial_seqs[0], &prompts[0])?,
        model.prefill_chunk(&mut serial_seqs[1], &prompts[1])?,
    ];
    let serial_snapshots = [
        model.debug_hybrid_sequence_snapshot(&mut serial_seqs[0])?,
        model.debug_hybrid_sequence_snapshot(&mut serial_seqs[1])?,
    ];
    for seq in &mut serial_seqs {
        model.release_seq(seq);
    }

    let mut batch_seqs = [model.new_seq(), model.new_seq()];
    let [first, second] = &mut batch_seqs;
    let mut lanes = [first, second];
    let batch_logits = model.hybrid_prefill_b2_t32(
        &mut lanes,
        [&prompts[0], &prompts[1]],
    )?;
    for lane in 0..2 {
        let batch_snapshot = model.debug_hybrid_sequence_snapshot(lanes[lane])?;
        if batch_snapshot != serial_snapshots[lane] {
            let divergence = batch_snapshot
                .iter()
                .zip(&serial_snapshots[lane])
                .find(|(batch, serial)| batch != serial)
                .map(|(batch, serial)| {
                    let byte = batch
                        .2
                        .iter()
                        .zip(&serial.2)
                        .position(|(left, right)| left != right);
                    format!("{} vs {}, pierwszy bajt {byte:?}", batch.0, serial.0)
                })
                .unwrap_or_else(|| "inna liczba buforów".into());
            return Err(format!("stan/KV lane {lane}: {divergence}").into());
        }
        if batch_logits[lane] != serial_logits[lane] {
            let first = batch_logits[lane]
                .iter()
                .zip(&serial_logits[lane])
                .position(|(batch, serial)| batch != serial);
            return Err(format!("logity lane {lane}: pierwszy element {first:?}").into());
        }
        let serial_top1 = serial_logits[lane]
            .iter()
            .enumerate()
            .max_by(|left, right| left.1.total_cmp(right.1))
            .map(|(index, _)| index);
        let batch_top1 = batch_logits[lane]
            .iter()
            .enumerate()
            .max_by(|left, right| left.1.total_cmp(right.1))
            .map(|(index, _)| index);
        assert_eq!(batch_top1, serial_top1, "top1 lane {lane}");
        assert_eq!(lanes[lane].len, 32);
        assert_eq!(lanes[lane].pages.len(), 1);
    }
    model.release_seq(lanes[0]);
    model.release_seq(lanes[1]);
    Ok(())
}

#[test]
fn hybrid_prefill_b2_gpu_sampler_zachowuje_parametry_seed_i_kary_lane() -> TestResult<()> {
    let Some(path) = std::env::var_os("FORGE_TEST_HYBRID_GGUF").map(PathBuf::from) else {
        eprintln!("pominięto sampler prefill B2: brak FORGE_TEST_HYBRID_GGUF");
        return Ok(());
    };
    let _gpu_lock = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(mut model) = load_model_sized(&path, true, 64, 8) else {
        return Ok(());
    };
    if !model.hybrid_prefill_b2_capable(32) {
        return Ok(());
    }
    let vocab = model.weights.descriptor.params.vocab_size;
    let prompts = [prompt(vocab, 1709, 32), prompt(vocab, 2903, 32)];
    let sampling = [
        SamplingParams {
            temperature: 0.0,
            repetition_penalty: 1.08,
            frequency_penalty: 0.15,
            presence_penalty: -0.1,
            repeat_last_n: 17,
            seed: Some(11),
            ..SamplingParams::default()
        },
        SamplingParams {
            temperature: 0.8,
            top_k: 8,
            top_p: 0.9,
            min_p: 0.05,
            repetition_penalty: 1.04,
            frequency_penalty: -0.2,
            presence_penalty: 0.25,
            repeat_last_n: 23,
            seed: Some(29),
        },
    ];

    let mut serial_seqs = [model.new_seq(), model.new_seq()];
    let [first, second] = &mut serial_seqs;
    let mut serial_lanes = [first, second];
    model.hybrid_prefill_b2_t32(&mut serial_lanes, [&prompts[0], &prompts[1]])?;
    let mut serial_samplers = [
        GpuSampler::new(sampling[0].clone()),
        GpuSampler::new(sampling[1].clone()),
    ];
    serial_samplers[0].note_tokens(&prompts[0]);
    serial_samplers[1].note_tokens(&prompts[1]);
    let expected = [
        model.sample_hybrid_prefill_b2_logits(0, &mut serial_samplers[0])?,
        model.sample_hybrid_prefill_b2_logits(1, &mut serial_samplers[1])?,
    ];
    model.release_seq(serial_lanes[0]);
    model.release_seq(serial_lanes[1]);

    let mut batch_seqs = [model.new_seq(), model.new_seq()];
    let [first, second] = &mut batch_seqs;
    let mut batch_lanes = [first, second];
    model.hybrid_prefill_b2_t32_device(&mut batch_lanes, [&prompts[0], &prompts[1]])?;
    let mut batch_samplers = [
        GpuSampler::new(sampling[0].clone()),
        GpuSampler::new(sampling[1].clone()),
    ];
    batch_samplers[0].note_tokens(&prompts[0]);
    batch_samplers[1].note_tokens(&prompts[1]);
    let [first_sampler, second_sampler] = &mut batch_samplers;
    let actual = model.sample_hybrid_prefill_b2_logits_batched(&mut [
        first_sampler,
        second_sampler,
    ])?;
    assert_eq!(actual, expected);
    model.release_seq(batch_lanes[0]);
    model.release_seq(batch_lanes[1]);
    Ok(())
}

#[test]
fn hybrid_prefill_b2_t32_przywraca_obie_sekwencje_po_bledzie_lane() -> TestResult<()> {
    let Some(path) = std::env::var_os("FORGE_TEST_HYBRID_GGUF").map(PathBuf::from) else {
        eprintln!("pominięto rollback prefill B2 T32: brak FORGE_TEST_HYBRID_GGUF");
        return Ok(());
    };
    assert!(path.is_file(), "FORGE_TEST_HYBRID_GGUF nie wskazuje pliku");
    let _gpu_lock = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(mut model) = load_model_sized(&path, true, 64, 8) else {
        return Ok(());
    };
    if !model.hybrid_prefill_b2_capable(32) {
        eprintln!("pominięto rollback prefill B2 T32: model bez capability");
        return Ok(());
    }
    let vocab = model.weights.descriptor.params.vocab_size;
    for failed_lane in 0..2 {
        let mut seqs = [model.new_seq(), model.new_seq()];
        model.prefill_chunk(&mut seqs[0], &prompt(vocab, 401 + failed_lane, 1))?;
        model.prefill_chunk(&mut seqs[1], &prompt(vocab, 809 + failed_lane, 1))?;
        let snapshots = [
            model.debug_hybrid_sequence_snapshot(&mut seqs[0])?,
            model.debug_hybrid_sequence_snapshot(&mut seqs[1])?,
        ];
        let lengths = [seqs[0].len, seqs[1].len];
        let pages = [seqs[0].pages.clone(), seqs[1].pages.clone()];
        let chunks = [
            prompt(vocab, 1709 + failed_lane, 32),
            prompt(vocab, 2903 + failed_lane, 32),
        ];
        let [first, second] = &mut seqs;
        let mut lanes = [first, second];
        let error = model
            .debug_hybrid_prefill_b2_t32_rollback(
                &mut lanes,
                [&chunks[0], &chunks[1]],
                failed_lane,
            )
            .expect_err("test wymusza błąd po zapisie stanu");
        assert!(error.to_string().contains(&format!("lane {failed_lane}")));
        for lane in 0..2 {
            assert_eq!(lanes[lane].len, lengths[lane]);
            assert_eq!(lanes[lane].pages, pages[lane]);
            assert_eq!(
                model.debug_hybrid_sequence_snapshot(lanes[lane])?,
                snapshots[lane],
                "rollback stanu lane {lane} po błędzie lane {failed_lane}"
            );
        }
        model.release_seq(lanes[0]);
        model.release_seq(lanes[1]);
    }
    Ok(())
}

#[test]
fn hybrid_prefill_mtp_catchup_b2_przywraca_pare_po_bledzie_lane() -> TestResult<()> {
    let Some(path) = std::env::var_os("FORGE_TEST_HYBRID_GGUF").map(PathBuf::from) else {
        eprintln!("pominięto rollback catch-up MTP B2: brak FORGE_TEST_HYBRID_GGUF");
        return Ok(());
    };
    let _gpu_lock = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(mut model) = load_model_sized(&path, true, 64, 8) else {
        return Ok(());
    };
    if !model.hybrid_prefill_b2_capable(32) {
        return Ok(());
    }
    let vocab = model.weights.descriptor.params.vocab_size;
    for failed_lane in 0..2 {
        let chunks = [
            prompt(vocab, 1201 + failed_lane, 32),
            prompt(vocab, 1601 + failed_lane, 32),
        ];
        let mut seqs = [model.new_seq(), model.new_seq()];
        let [first, second] = &mut seqs;
        let mut lanes = [first, second];
        model.hybrid_prefill_b2_t32_device(&mut lanes, [&chunks[0], &chunks[1]])?;
        let before = [
            model.debug_mtp_state_snapshot(lanes[0])?,
            model.debug_mtp_state_snapshot(lanes[1])?,
        ];
        let error = model
            .debug_hybrid_prefill_mtp_catchup_b2_rollback(
                &mut lanes,
                [&chunks[0], &chunks[1]],
                [true, true],
                failed_lane,
            )
            .expect_err("test wymusza błąd catch-up MTP");
        assert!(error.to_string().contains(&format!("lane {failed_lane}")));
        for lane in 0..2 {
            assert_mtp_snapshot_eq(
                &model.debug_mtp_state_snapshot(lanes[lane])?,
                &before[lane],
                &format!("rollback catch-up lane {lane} po błędzie {failed_lane}"),
            );
        }
        model.release_seq(lanes[0]);
        model.release_seq(lanes[1]);
    }
    Ok(())
}

#[test]
fn hybrid_prefill_mtp_catchup_b2_kwarantannuje_pare_po_bledzie_rollbacku() -> TestResult<()> {
    let Some(path) = std::env::var_os("FORGE_TEST_HYBRID_GGUF").map(PathBuf::from) else {
        eprintln!("pominięto błąd rollbacku catch-up MTP B2: brak FORGE_TEST_HYBRID_GGUF");
        return Ok(());
    };
    let _gpu_lock = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(mut model) = load_model_sized(&path, true, 64, 8) else {
        return Ok(());
    };
    if !model.hybrid_prefill_b2_capable(32) {
        return Ok(());
    }
    let vocab = model.weights.descriptor.params.vocab_size;
    let chunks = [prompt(vocab, 2201, 32), prompt(vocab, 2601, 32)];
    let mut seqs = [model.new_seq(), model.new_seq()];
    let [first, second] = &mut seqs;
    let mut lanes = [first, second];
    model.hybrid_prefill_b2_t32_device(&mut lanes, [&chunks[0], &chunks[1]])?;

    let error = model
        .debug_hybrid_prefill_mtp_catchup_b2_rollback_failure(
            &mut lanes,
            [&chunks[0], &chunks[1]],
            [true, true],
            1,
            0,
        )
        .expect_err("test wymusza błąd lane i rollbacku pary");
    assert!(error.to_string().contains("rollback pary nie powiódł się"));

    let reuse = model
        .debug_hybrid_prefill_mtp_catchup_b2_rollback(
            &mut lanes,
            [&chunks[0], &chunks[1]],
            [true, true],
            0,
        )
        .expect_err("zatruta pula nie może ponownie wydać pary MTP");
    assert!(reuse.to_string().contains("zatruta"));
    Ok(())
}

#[test]
fn benchmark_hybrid_prefill_b2_t32_kontra_dwa_serialne() -> TestResult<()> {
    if std::env::var_os("FORGE_BENCH_HYBRID_PREFILL_B2").is_none() {
        eprintln!("pominięto benchmark prefill B2 T32: brak FORGE_BENCH_HYBRID_PREFILL_B2");
        return Ok(());
    }
    let path = std::env::var_os("FORGE_TEST_HYBRID_GGUF")
        .map(PathBuf::from)
        .ok_or("benchmark wymaga FORGE_TEST_HYBRID_GGUF")?;
    let repetitions = std::env::var("FORGE_BENCH_REPS")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(10);
    if repetitions < 10 {
        return Err("benchmark prefill B2 wymaga co najmniej 10 powtórzeń".into());
    }
    let _gpu_lock = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(mut model) = load_model_sized(&path, true, 64, 8) else {
        return Ok(());
    };
    if !model.hybrid_prefill_b2_capable(32) {
        return Err("model benchmarku nie spełnia capability prefill B2 T32".into());
    }
    let vocab = model.weights.descriptor.params.vocab_size;
    let chunks = [prompt(vocab, 1709, 32), prompt(vocab, 2903, 32)];

    let mut warm_serial = [model.new_seq(), model.new_seq()];
    model.prefill_chunk(&mut warm_serial[0], &chunks[0])?;
    model.prefill_chunk(&mut warm_serial[1], &chunks[1])?;
    for seq in &mut warm_serial {
        model.release_seq(seq);
    }
    let mut warm_batch = [model.new_seq(), model.new_seq()];
    let [first, second] = &mut warm_batch;
    let mut warm_lanes = [first, second];
    model.hybrid_prefill_b2_t32(&mut warm_lanes, [&chunks[0], &chunks[1]])?;
    model.release_seq(warm_lanes[0]);
    model.release_seq(warm_lanes[1]);

    let mut serial_ms = Vec::with_capacity(repetitions);
    let mut batch_ms = Vec::with_capacity(repetitions);
    for _ in 0..repetitions {
        let mut serial = [model.new_seq(), model.new_seq()];
        let (serial_first, first_ms) =
            model.debug_prefill_chunk_gpu_ms(&mut serial[0], &chunks[0])?;
        let (serial_second, second_ms) =
            model.debug_prefill_chunk_gpu_ms(&mut serial[1], &chunks[1])?;
        let serial_snapshots = [
            model.debug_hybrid_sequence_snapshot(&mut serial[0])?,
            model.debug_hybrid_sequence_snapshot(&mut serial[1])?,
        ];
        serial_ms.push(first_ms + second_ms);
        for seq in &mut serial {
            model.release_seq(seq);
        }

        let mut batch = [model.new_seq(), model.new_seq()];
        let [first, second] = &mut batch;
        let mut lanes = [first, second];
        let (batch_logits, elapsed) = model.debug_hybrid_prefill_b2_t32_gpu_ms(
            &mut lanes,
            [&chunks[0], &chunks[1]],
        )?;
        batch_ms.push(elapsed);
        assert_eq!(batch_logits[0], serial_first);
        assert_eq!(batch_logits[1], serial_second);
        for lane in 0..2 {
            assert_eq!(
                model.debug_hybrid_sequence_snapshot(lanes[lane])?,
                serial_snapshots[lane]
            );
        }
        model.release_seq(lanes[0]);
        model.release_seq(lanes[1]);
    }
    let average = |values: &[f32]| values.iter().copied().sum::<f32>() / values.len() as f32;
    let serial_average = average(&serial_ms);
    let batch_average = average(&batch_ms);
    println!(
        "hybrid_prefill_b2_t32 reps={repetitions} serial_gpu_ms={serial_average:.3} b2_gpu_ms={batch_average:.3} serial_tok_s={:.1} b2_tok_s={:.1} speedup={:.3} scratch_bytes={}",
        64_000.0 / serial_average,
        64_000.0 / batch_average,
        serial_average / batch_average,
        model.debug_hybrid_prefill_b2_scratch_bytes(),
    );
    Ok(())
}

#[test]
fn profil_launchy_hybrid_prefill_t32() -> TestResult<()> {
    let Some(mode) = std::env::var_os("FORGE_PROFILE_HYBRID_PREFILL") else {
        eprintln!("pominięto profil launchy prefill");
        return Ok(());
    };
    let mode = mode.to_string_lossy();
    if mode != "serial" && mode != "b2" {
        return Err("profil prefill wymaga trybu serial lub b2".into());
    }
    let path = std::env::var_os("FORGE_TEST_HYBRID_GGUF")
        .map(PathBuf::from)
        .ok_or("profil wymaga FORGE_TEST_HYBRID_GGUF")?;
    let _gpu_lock = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(mut model) = load_model_sized(&path, true, 64, 8) else {
        return Ok(());
    };
    let vocab = model.weights.descriptor.params.vocab_size;
    let chunks = [prompt(vocab, 1709, 32), prompt(vocab, 2903, 32)];
    if mode == "serial" {
        let mut warm = [model.new_seq(), model.new_seq()];
        model.prefill_chunk(&mut warm[0], &chunks[0])?;
        model.prefill_chunk(&mut warm[1], &chunks[1])?;
        for seq in &mut warm {
            model.release_seq(seq);
        }
    } else {
        let mut warm = [model.new_seq(), model.new_seq()];
        let [first, second] = &mut warm;
        let mut lanes = [first, second];
        model.hybrid_prefill_b2_t32(&mut lanes, [&chunks[0], &chunks[1]])?;
        model.release_seq(lanes[0]);
        model.release_seq(lanes[1]);
    }
    cuda_profiler_call(b"cudaProfilerStart\0")?;
    if mode == "serial" {
        let mut seqs = [model.new_seq(), model.new_seq()];
        model.prefill_chunk(&mut seqs[0], &chunks[0])?;
        model.prefill_chunk(&mut seqs[1], &chunks[1])?;
        for seq in &mut seqs {
            model.release_seq(seq);
        }
    } else {
        let mut seqs = [model.new_seq(), model.new_seq()];
        let [first, second] = &mut seqs;
        let mut lanes = [first, second];
        model.hybrid_prefill_b2_t32(&mut lanes, [&chunks[0], &chunks[1]])?;
        model.release_seq(lanes[0]);
        model.release_seq(lanes[1]);
    }
    cuda_profiler_call(b"cudaProfilerStop\0")?;
    println!("profil prefill {mode} zakończony");
    Ok(())
}

#[test]
fn benchmark_server_prefill_b2_raw() -> TestResult<()> {
    if std::env::var_os("FORGE_BENCH_HYBRID_PREFILL_SERVER").is_none() {
        eprintln!("pominięto benchmark serwera prefill B2");
        return Ok(());
    }
    let path = std::env::var_os("FORGE_TEST_HYBRID_GGUF")
        .map(PathBuf::from)
        .ok_or("benchmark wymaga FORGE_TEST_HYBRID_GGUF")?;
    let prompt_tokens = std::env::var("FORGE_BENCH_PROMPT_TOKENS")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(128);
    if !matches!(prompt_tokens, 128 | 512) {
        return Err("benchmark raw wymaga 128 lub 512 tokenów".into());
    }
    let repetitions = std::env::var("FORGE_BENCH_REPS")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(5);
    if repetitions < 5 {
        return Err("benchmark serwera wymaga co najmniej pięciu prób".into());
    }
    let _gpu_lock = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let max_seq_len = prompt_tokens + 16;
    let kv_pages = 2 * max_seq_len.div_ceil(32) + 4;
    let Some(mut oracle) = load_model_sized(&path, true, max_seq_len, kv_pages) else {
        return Ok(());
    };
    let vocab = oracle.weights.descriptor.params.vocab_size;
    let prompts = [
        prompt(vocab, 1709, prompt_tokens),
        prompt(vocab, 2903, prompt_tokens),
    ];
    let expected = [
        generate(&mut oracle, &prompts[0], 8)?,
        generate(&mut oracle, &prompts[1], 8)?,
    ];
    drop(oracle);
    let Some(model) = load_model_sized(&path, true, max_seq_len, kv_pages) else {
        return Err("CUDA zniknęła przed benchmarkiem serwera".into());
    };
    let engine = spawn_engine_batched(
        model,
        Arc::new(tokenizer(&path)?),
        2,
        32,
        12,
        SpeculativeConfig::off(),
    )?;
    let enabled = std::env::var("FORGE_HYBRID_PREFILL_BATCH").is_ok_and(|value| value == "1");
    let cpu_fallback = std::env::var_os("FORGE_BENCH_PREFILL_CPU_FALLBACK").is_some();
    let profile = std::env::var_os("FORGE_PROFILE_SERVER_PREFILL").is_some();
    for repetition in 0..=repetitions {
        let metrics = engine.metrics();
        let steps_before = metrics.hybrid_prefill_b2_steps_total.load(Ordering::Relaxed);
        let tokens_before = metrics.hybrid_prefill_b2_tokens_total.load(Ordering::Relaxed);
        if profile && repetition == 1 {
            cuda_profiler_call(b"cudaProfilerStart\0")?;
        }
        let started = Instant::now();
        let mut first_request = id_server_request(prompts[0].clone(), 8);
        let mut second_request = id_server_request(prompts[1].clone(), 8);
        if cpu_fallback {
            first_request.logprobs = Some(0);
            second_request.logprobs = Some(0);
        }
        let receivers = [engine.submit(first_request)?, engine.submit(second_request)?];
        let mut outputs = [Vec::new(), Vec::new()];
        let mut first_token = [None, None];
        let mut done = [false, false];
        while done.iter().any(|&value| !value) {
            let mut progressed = false;
            for lane in 0..2 {
                if done[lane] {
                    continue;
                }
                match receivers[lane].try_recv() {
                    Ok(EngineEvent::Token { id, .. }) => {
                        first_token[lane].get_or_insert_with(Instant::now);
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
                        return Err("worker rozłączył benchmark prefill".into());
                    }
                }
            }
            if !progressed {
                std::thread::sleep(Duration::from_micros(100));
            }
        }
        let finished = Instant::now();
        if profile && repetition == 1 {
            cuda_profiler_call(b"cudaProfilerStop\0")?;
        }
        assert_eq!(outputs, expected);
        let ttft = [
            first_token[0].ok_or("brak TTFT lane0")?.duration_since(started),
            first_token[1].ok_or("brak TTFT lane1")?.duration_since(started),
        ];
        let prefill_elapsed = ttft[0].max(ttft[1]).as_secs_f64();
        let steps = metrics
            .hybrid_prefill_b2_steps_total
            .load(Ordering::Relaxed)
            .saturating_sub(steps_before);
        let routed_tokens = metrics
            .hybrid_prefill_b2_tokens_total
            .load(Ordering::Relaxed)
            .saturating_sub(tokens_before);
        println!(
            "server_prefill_raw={} gate={} sampler={} {}={}/{} input_tok_s={:.1} ttft_lane_ms={:.2}/{:.2} e2e_ms={:.2} b2_steps={} b2_tokens={}",
            prompt_tokens,
            if enabled { "on" } else { "off" },
            if cpu_fallback { "cpu" } else { "gpu" },
            if repetition == 0 { "warmup" } else { "rep" },
            if repetition == 0 { 1 } else { repetition },
            if repetition == 0 { 1 } else { repetitions },
            (2 * prompt_tokens) as f64 / prefill_elapsed,
            ttft[0].as_secs_f64() * 1e3,
            ttft[1].as_secs_f64() * 1e3,
            finished.duration_since(started).as_secs_f64() * 1e3,
            steps,
            routed_tokens,
        );
        if enabled {
            assert_eq!(steps, (prompt_tokens / 32) as u64);
            assert_eq!(routed_tokens, (2 * prompt_tokens) as u64);
        } else {
            assert_eq!((steps, routed_tokens), (0, 0));
        }
    }
    engine.shutdown()?;
    Ok(())
}

#[test]
fn mtp_ngram_b2_zachowuje_golden_dla_k2_k3_i_macierzy_retained() -> TestResult<()> {
    let Some(path) = std::env::var_os("FORGE_TEST_HYBRID_GGUF").map(PathBuf::from) else {
        eprintln!("pominięto E2E MTP+n-gram B2: brak FORGE_TEST_HYBRID_GGUF");
        return Ok(());
    };
    assert!(path.is_file(), "FORGE_TEST_HYBRID_GGUF nie wskazuje pliku");
    let _gpu_lock = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    run_mtp_ngram_b2_retained_matrix(&path)
}

#[test]
fn mtp_routed_b2_zachowuje_golden_dla_masek_zrodel() -> TestResult<()> {
    let Some(path) = std::env::var_os("FORGE_TEST_HYBRID_GGUF").map(PathBuf::from) else {
        eprintln!("pominięto E2E routed MTP B2: brak FORGE_TEST_HYBRID_GGUF");
        return Ok(());
    };
    assert!(path.is_file(), "FORGE_TEST_HYBRID_GGUF nie wskazuje pliku");
    let _gpu_lock = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    run_mtp_routed_b2_source_masks(&path)
}

#[test]
fn mtp_ngram_b2_retained_jeden_zachowuje_golden_po_zamianie_lane() -> TestResult<()> {
    let Some(path) = std::env::var_os("FORGE_TEST_HYBRID_GGUF").map(PathBuf::from) else {
        eprintln!("pominięto E2E MTP+n-gram B2 lane swap: brak FORGE_TEST_HYBRID_GGUF");
        return Ok(());
    };
    assert!(path.is_file(), "FORGE_TEST_HYBRID_GGUF nie wskazuje pliku");
    let _gpu_lock = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    run_mtp_ngram_b2_retained_one_lane_orders(&path)
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
fn server_mtp_ngram_mixed_zachowuje_oracle_anulowanie_i_reuse() -> TestResult<()> {
    if std::env::var_os("FORGE_TEST_MTP_NGRAM_MIXED_SERVER").is_none() {
        eprintln!("pominięto E2E mixed MTP+n-gram");
        return Ok(());
    }
    let path = std::env::var_os("FORGE_TEST_HYBRID_GGUF")
        .map(PathBuf::from)
        .ok_or("E2E mixed wymaga FORGE_TEST_HYBRID_GGUF")?;
    let _gpu_lock = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    run_server_concurrency_two(
        &path,
        SpeculativeConfig::chain(vec![ProposerKind::Mtp, ProposerKind::Ngram], 3)?,
    )
}

#[test]
fn server_prefill_b2_t32_zachowuje_id_dla_ogonow_anulowania_i_reuse() -> TestResult<()> {
    if std::env::var_os("FORGE_TEST_HYBRID_PREFILL_SERVER").is_none() {
        eprintln!("pominięto E2E serwera prefill B2: brak FORGE_TEST_HYBRID_PREFILL_SERVER");
        return Ok(());
    }
    let path = std::env::var_os("FORGE_TEST_HYBRID_GGUF")
        .map(PathBuf::from)
        .ok_or("E2E prefill B2 wymaga FORGE_TEST_HYBRID_GGUF")?;
    let _gpu_lock = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(mut oracle) = load_model_sized(&path, true, 96, 8) else {
        return Ok(());
    };
    let vocab = oracle.weights.descriptor.params.vocab_size;
    let prompts = [
        prompt(vocab, 29, 64),
        prompt(vocab, 173, 47),
        prompt(vocab, 313, 32),
        prompt(vocab, 367, 32),
        prompt(vocab, 419, 32),
    ];
    let mut expected = Vec::new();
    for tokens in &prompts {
        expected.push(generate(&mut oracle, tokens, 8)?);
    }
    drop(oracle);

    let Some(model) = load_model_sized(&path, true, 96, 8) else {
        return Err("CUDA zniknęła przed E2E prefill B2".into());
    };
    let engine = spawn_engine_batched(
        model,
        Arc::new(tokenizer(&path)?),
        2,
        32,
        12,
        SpeculativeConfig::off(),
    )?;
    let first = engine.submit(id_server_request(prompts[0].clone(), 8))?;
    let second = engine.submit(id_server_request(prompts[1].clone(), 8))?;
    assert_eq!(collect_events(first)?, expected[0]);
    assert_eq!(collect_events(second)?, expected[1]);

    let cancelled = engine.submit(id_server_request(prompt(vocab, 257, 64), 16))?;
    let replacement = engine.submit(id_server_request(prompts[2].clone(), 8))?;
    drop(cancelled);
    assert_eq!(collect_events(replacement)?, expected[2]);

    let reused = engine.submit(id_server_request(prompts[3].clone(), 8))?;
    let companion = engine.submit(id_server_request(prompts[4].clone(), 8))?;
    assert_eq!(collect_events(reused)?, expected[3]);
    assert_eq!(collect_events(companion)?, expected[4]);
    let metrics = engine.metrics();
    assert!(metrics.hybrid_prefill_b2_steps_total.load(Ordering::Relaxed) >= 3);
    assert!(metrics.hybrid_prefill_b2_tokens_total.load(Ordering::Relaxed) >= 192);
    engine.shutdown()?;
    Ok(())
}

#[test]
fn server_prefill_b2_mieszany_gpu_cpu_uzywa_wspolnego_host_fallback() -> TestResult<()> {
    if std::env::var_os("FORGE_TEST_HYBRID_PREFILL_SERVER").is_none() {
        eprintln!("pominięto E2E mieszanego samplera prefill B2");
        return Ok(());
    }
    let path = std::env::var_os("FORGE_TEST_HYBRID_GGUF")
        .map(PathBuf::from)
        .ok_or("E2E mieszanego samplera wymaga FORGE_TEST_HYBRID_GGUF")?;
    let _gpu_lock = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(mut oracle) = load_model_sized(&path, true, 64, 8) else {
        return Ok(());
    };
    let vocab = oracle.weights.descriptor.params.vocab_size;
    let prompts = [prompt(vocab, 701, 32), prompt(vocab, 809, 32)];
    let expected = [
        generate(&mut oracle, &prompts[0], 8)?,
        generate(&mut oracle, &prompts[1], 8)?,
    ];
    drop(oracle);
    let Some(model) = load_model_sized(&path, true, 64, 8) else {
        return Err("CUDA zniknęła przed E2E mieszanego samplera".into());
    };
    let engine = spawn_engine_batched(
        model,
        Arc::new(tokenizer(&path)?),
        2,
        32,
        12,
        SpeculativeConfig::off(),
    )?;
    let gpu_request = id_server_request(prompts[0].clone(), 8);
    let mut cpu_request = id_server_request(prompts[1].clone(), 8);
    cpu_request.logprobs = Some(0);
    let gpu = engine.submit(gpu_request)?;
    let cpu = engine.submit(cpu_request)?;
    assert_eq!(collect_events(gpu)?, expected[0]);
    assert_eq!(collect_events(cpu)?, expected[1]);
    assert_eq!(
        engine
            .metrics()
            .hybrid_prefill_b2_steps_total
            .load(Ordering::Relaxed),
        1
    );
    engine.shutdown()?;
    Ok(())
}

#[test]
fn server_prefill_b2_nie_blokuje_live_decode_i_rotuje_pary() -> TestResult<()> {
    if std::env::var_os("FORGE_TEST_HYBRID_PREFILL_PRIORITY").is_none() {
        eprintln!("pominięto E2E priorytetu decode nad prefill B2");
        return Ok(());
    }
    let path = std::env::var_os("FORGE_TEST_HYBRID_GGUF")
        .map(PathBuf::from)
        .ok_or("E2E priorytetu wymaga FORGE_TEST_HYBRID_GGUF")?;
    let _gpu_lock = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(mut oracle) = load_model_sized(&path, true, 96, 24) else {
        return Ok(());
    };
    let vocab = oracle.weights.descriptor.params.vocab_size;
    let decode_prompt = prompt(vocab, 101, 32);
    let prefill_prompts = [
        prompt(vocab, 211, 64),
        prompt(vocab, 307, 64),
        prompt(vocab, 401, 64),
        prompt(vocab, 503, 64),
    ];
    let expected_decode = generate(&mut oracle, &decode_prompt, 8)?;
    let mut expected_prefill = Vec::new();
    for prompt in &prefill_prompts {
        expected_prefill.push(generate(&mut oracle, prompt, 2)?);
    }
    drop(oracle);

    let Some(model) = load_model_sized(&path, true, 96, 24) else {
        return Err("CUDA zniknęła przed E2E priorytetu prefill".into());
    };
    let engine = spawn_engine_batched(
        model,
        Arc::new(tokenizer(&path)?),
        5,
        32,
        12,
        SpeculativeConfig::off(),
    )?;
    let decode = engine.submit(id_server_request(decode_prompt, 8))?;
    let (first_id, first_at) = loop {
        match decode.recv()? {
            EngineEvent::Token { id, .. } => break (id, Instant::now()),
            EngineEvent::Error(error) => return Err(error.into()),
            EngineEvent::Done { .. } => return Err("decode zakończył się bez tokenu".into()),
        }
    };
    let prefill = prefill_prompts
        .into_iter()
        .map(|tokens| engine.submit(id_server_request(tokens, 2)))
        .collect::<Result<Vec<_>, _>>()?;
    let (second_id, second_at) = loop {
        match decode.recv_timeout(Duration::from_secs(2))? {
            EngineEvent::Token { id, .. } => break (id, Instant::now()),
            EngineEvent::Error(error) => return Err(error.into()),
            EngineEvent::Done { .. } => return Err("decode zakończył się po jednym tokenie".into()),
        }
    };
    assert_eq!([first_id, second_id], expected_decode[..2]);
    let live_itl = second_at.duration_since(first_at);
    assert!(
        live_itl < Duration::from_millis(500),
        "prefill zwiększył live ITL do {:.2} ms",
        live_itl.as_secs_f64() * 1e3
    );
    let mut decode_ids = vec![first_id, second_id];
    loop {
        match decode.recv()? {
            EngineEvent::Token { id, .. } => decode_ids.push(id),
            EngineEvent::Done { tokens, .. } => {
                assert_eq!(decode_ids.len(), tokens);
                break;
            }
            EngineEvent::Error(error) => return Err(error.into()),
        }
    }
    assert_eq!(decode_ids, expected_decode);
    for (lane, receiver) in prefill.into_iter().enumerate() {
        assert_eq!(collect_events(receiver)?, expected_prefill[lane]);
    }
    let metrics = engine.metrics();
    assert!(metrics.hybrid_prefill_b2_steps_total.load(Ordering::Relaxed) >= 4);
    println!(
        "live decode ITL z czterema prefill={:.2} ms, kroki B2={}",
        live_itl.as_secs_f64() * 1e3,
        metrics.hybrid_prefill_b2_steps_total.load(Ordering::Relaxed),
    );
    engine.shutdown()?;
    Ok(())
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
    let prompt_path = std::env::var_os("FORGE_BENCH_PROMPT_IDS").map(PathBuf::from);
    let _gpu_lock = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    run_exact_native_mtp_b2_matrix(&path, prompt_path.as_deref(), ExactB2Mode::Native)
}

#[test]
fn benchmark_exact_mtp_ngram_b2_dwa_identyczne_requesty() -> TestResult<()> {
    if std::env::var_os("FORGE_BENCH_MTP_NGRAM_B2_MATRIX").is_none() {
        eprintln!("pominięto dokładną macierz MTP+n-gram B2: brak FORGE_BENCH_MTP_NGRAM_B2_MATRIX");
        return Ok(());
    }
    let path = std::env::var_os("FORGE_TEST_HYBRID_GGUF")
        .map(PathBuf::from)
        .ok_or("macierz wymaga FORGE_TEST_HYBRID_GGUF")?;
    let prompt_path = std::env::var_os("FORGE_BENCH_PROMPT_IDS").map(PathBuf::from);
    let _gpu_lock = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    run_exact_native_mtp_b2_matrix(&path, prompt_path.as_deref(), ExactB2Mode::MtpNgram)
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
