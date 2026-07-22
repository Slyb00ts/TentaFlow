// ===== File: server.rs — engine service: request queue + iteration-level scheduler =====
// The GPU worker owns the Model on a dedicated thread and interleaves active
// sequences one token per iteration (continuous batching semantics; kernel-
// level batching replaces the inner loop later without changing this API).
// Admission control projects KV page demand before accepting work (SPEC §9.1).

use std::collections::VecDeque;
use std::sync::mpsc;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::Instant;
use std::time::Duration;

use forge_tokenize::{StopMatcher, StreamDecoder, Tokenizer};
use forge_types::{ForgeError, Result};

use crate::generate::FinishReason;
use crate::kv::SeqKv;
use crate::metrics::{EngineMetrics, SeqTiming};
use crate::model::{Model, PrefillProfile, MAX_PREFILL_CHUNK};
use crate::sample::{
    apply_logit_bias, compute_logprob, suppress_eos, GpuSampler, Sampler, SamplingParams,
    SeqSampleParams, TokenLogprob, apply_penalties,
};
pub use crate::speculation::SpeculativeConfig;
use crate::speculation::{SpeculationCoordinator, SpeculationKind, SpeculativeState};

/// Shortest draft that is worth a verify forward (SPEC §6). The verify runs the
/// ungraphed prefill path, so on a launch-bound small model it only wins when it
/// can replace several graphed decode steps; shorter drafts fall back to the
/// plain single-token step. Long enough that ordinary prose (only short
/// coincidental drafts) never verifies, low enough that genuine recurring
/// context (which drafts to the full budget) always does.
const MIN_VERIFY_DRAFT: usize = 8;

#[derive(Default)]
pub struct EngineRequest {
    pub prompt_tokens: Vec<u32>,
    pub max_tokens: usize,
    pub sampling: SamplingParams,
    pub stop: Vec<String>,
    pub eos_ids: Vec<u32>,
    /// Constrained decoding (SPEC §8.1.2): when set, the sequence samples on
    /// the CPU with a per-step grammar logit mask, so its output can only
    /// conform to the grammar.
    pub grammar: Option<forge_grammar::GrammarProgram>,
    /// `logit_bias` (SPEC §8.1.2): additive bias per token id on the host
    /// logits. Non-empty forces the CPU sampler.
    pub logit_bias: Vec<(u32, f32)>,
    /// `min_tokens` (SPEC §8.1.2): suppress every EOS id until this many tokens
    /// are produced. Non-zero forces the CPU sampler.
    pub min_tokens: usize,
    /// `logprobs`/`top_logprobs` (SPEC §8.1.2): report each token's
    /// log-probability plus this many top alternatives. Forces the CPU sampler.
    pub logprobs: Option<usize>,
}

#[derive(Debug)]
pub enum EngineEvent {
    Token {
        id: u32,
        text: String,
        /// Per-token log-probability report (SPEC §8.1.2), present only when
        /// the request asked for `logprobs`.
        logprob: Option<TokenLogprob>,
    },
    Done {
        reason: FinishReason,
        tokens: usize,
        prompt_tokens: usize,
        /// Prompt tokens served from the prefix cache (SPEC §5.2 prefix hit).
        cache_read_tokens: usize,
        /// Diagnostyka dostępna tylko, gdy model przygotował profil benchmarku.
        benchmark: Option<BenchmarkTimings>,
    },
    Error(String),
}

#[derive(Clone, Copy, Debug)]
pub struct BenchmarkTimings {
    pub target_gpu_ms: Option<f64>,
    pub mtp_catchup_gpu_ms: Option<f64>,
    pub ttft_ms: f64,
}

struct Submission {
    req: EngineRequest,
    events: mpsc::Sender<EngineEvent>,
    submitted_at: Instant,
    bypasses: usize,
}

/// Cheap cloneable handle; the worker thread ends when all handles drop.
#[derive(Clone)]
pub struct EngineHandle {
    tx: mpsc::Sender<Submission>,
    metrics: Arc<EngineMetrics>,
    worker: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl EngineHandle {
    /// Queue a request; events stream on the returned receiver.
    pub fn submit(&self, req: EngineRequest) -> Result<mpsc::Receiver<EngineEvent>> {
        let (etx, erx) = mpsc::channel();
        self.tx
            .send(Submission {
                req,
                events: etx,
                submitted_at: Instant::now(),
                bypasses: 0,
            })
            .map_err(|_| ForgeError::Scheduler("engine worker stopped".into()))?;
        Ok(erx)
    }

    /// Live observability counters/gauges/histograms (SPEC §8.3), shared with
    /// the worker thread. Read-only for callers.
    pub fn metrics(&self) -> &Arc<EngineMetrics> {
        &self.metrics
    }

    /// Zamyka ostatni handle i czeka na zwolnienie modelu przez worker.
    pub fn shutdown(self) -> Result<()> {
        let Self {
            tx,
            metrics: _,
            worker,
        } = self;
        if Arc::strong_count(&worker) != 1 {
            return Err(ForgeError::Scheduler(
                "shutdown wymaga ostatniego klonu EngineHandle".into(),
            ));
        }
        drop(tx);
        let handle = worker
            .lock()
            .map_err(|_| ForgeError::Scheduler("blokada workera jest zatruta".into()))?
            .take()
            .ok_or_else(|| ForgeError::Scheduler("worker został już zatrzymany".into()))?;
        handle
            .join()
            .map_err(|_| ForgeError::Scheduler("worker zakończył się panic".into()))
    }
}

/// Per-sequence sampling strategy: GPU (only the sampled id leaves the
/// device) whenever the params fit the sampling kernels, CPU otherwise
/// (unbounded top-k, future logprobs reporting).
enum SeqSampler {
    Cpu(Sampler),
    Gpu(GpuSampler),
}

/// The pending next-token state between scheduler iterations: the CPU path
/// snapshots host logits and defers the draw; the GPU path draws immediately
/// after its own step (the shared device logits buffer is overwritten by
/// whichever sequence runs next), so it carries the drawn token instead.
enum PendingNext {
    Logits(Vec<f32>),
    Token(u32),
}

struct ActiveSeq<'t> {
    seq: SeqKv,
    sampler: SeqSampler,
    decoder: StreamDecoder<'t>,
    stops: StopMatcher,
    events: mpsc::Sender<EngineEvent>,
    /// Prompt tokens not yet prefilled (front = next).
    pending_prompt: VecDeque<u32>,
    generated: Vec<u32>,
    /// Prompt i wygenerowane tokeny widoczne dla kar samplera CPU.
    sampling_history: Vec<u32>,
    max_tokens: usize,
    eos_ids: Vec<u32>,
    /// `logit_bias` applied to the host logits before selection (CPU path).
    logit_bias: Vec<(u32, f32)>,
    /// EOS floor: suppress EOS until this many tokens are produced (CPU path).
    min_tokens: usize,
    /// When set, each token carries a `logprobs` report with this many top
    /// alternatives (CPU path).
    logprobs: Option<usize>,
    /// Live grammar state for constrained decoding; `None` = unconstrained.
    matcher: Option<forge_grammar::GrammarMatcher>,
    prompt_len: usize,
    /// Prompt tokens served from the prefix cache (SPEC §5.2), reported in the
    /// completion usage as `cached_tokens`.
    cache_read: usize,
    next: Option<PendingNext>,
    /// Client hang-up detected; sequence is torn down at the next iteration.
    dead: bool,
    /// Stan hostowego proposera; natywne MTP używa stanu per sekwencja w puli GPU.
    spec: Option<SpeculativeState>,
    /// Jawny tryb spekulacji; natywne MTP nie ma hostowego `SpeculativeState`
    /// ani CPU proposera.
    spec_kind: SpeculationKind,
    /// Budżet draftu na krok, równy 0 wyłącznie dla `SpeculationKind::Off`.
    spec_budget: usize,
    /// Verification forwards run for this sequence (each yields 1..=k+1 tokens).
    spec_forwards: u64,
    /// Draft tokens accepted across all verifications (excludes correction/bonus
    /// tokens, which every forward also produces).
    spec_accepted: u64,
    /// Liczba pełnych draftów n-gram zweryfikowanych przez router MTP+n-gram.
    ngram_forwards: u64,
    /// Liczba kroków, w których brak pełnego draftu uruchomił proposer MTP.
    mtp_fallback_forwards: u64,
    mtp_k2_rate: Option<f64>,
    mtp_k3_rate: Option<f64>,
    /// TTFT / inter-token / decode-tps timing feeding the metrics histograms.
    timing: SeqTiming,
    /// Set once `finish` has emitted `Done`; distinguishes a clean completion
    /// from an errored / hung-up teardown for the requests_errored counter.
    finished_cleanly: bool,
    submitted_at: Instant,
    prefill_profile: Option<PrefillProfile>,
    benchmark_ttft_ms: Option<f64>,
    /// Maksymalna liczba stron, do której admission zobowiązał pulę KV.
    kv_page_budget: usize,
}

fn request_speculation_kind(spec: &SpeculativeConfig, greedy: bool) -> Result<SpeculationKind> {
    match (spec.kind(), greedy) {
        (
            SpeculationKind::HostProposer
            | SpeculationKind::NativeMtp
            | SpeculationKind::NativeMtpNgram,
            false,
        )
        | (SpeculationKind::Off, _) => {
            Ok(SpeculationKind::Off)
        }
        (kind, true) => Ok(kind),
    }
}

fn validate_speculation_server_config(
    _kind: SpeculationKind,
    max_active: usize,
    _hybrid_target: bool,
) -> Result<()> {
    if max_active == 0 {
        return Err(ForgeError::Scheduler(
            "max_active musi być większe od zera".into(),
        ));
    }
    Ok(())
}

fn native_mtp_step_budget(available: usize) -> Option<usize> {
    match available {
        2 | 3 => Some(available),
        _ => None,
    }
}

fn native_mtp_adaptive_budget(
    available: usize,
    configured: usize,
    forwards: u64,
    k2_rate: Option<f64>,
    k3_rate: Option<f64>,
) -> Option<usize> {
    let maximum = native_mtp_step_budget(available)?;
    if configured != 3 {
        return native_mtp_step_budget(maximum.min(configured));
    }
    if maximum != 3 {
        return Some(maximum);
    }
    if forwards < 4 {
        return Some(if forwards.is_multiple_of(2) { 3 } else { 2 });
    }
    let preferred = if k3_rate.unwrap_or(0.0) >= k2_rate.unwrap_or(0.0) {
        3
    } else {
        2
    };
    if forwards.is_multiple_of(16) {
        Some(if preferred == 3 { 2 } else { 3 })
    } else {
        Some(preferred)
    }
}

fn parse_native_mtp_b2(value: Option<&str>) -> Result<bool> {
    match value {
        None | Some("1") => Ok(true),
        Some("0") => Ok(false),
        Some(value) => Err(ForgeError::Scheduler(format!(
            "FORGE_NATIVE_MTP_B2 wymaga wartości 0 lub 1, otrzymano {value:?}"
        ))),
    }
}

fn update_mtp_rate(rate: &mut Option<f64>, sample: f64) {
    *rate = Some(rate.map_or(sample, |previous| previous * 0.75 + sample * 0.25));
}

enum AdmissionDisposition {
    Admit { page_budget: usize },
    Reject(String),
    Wait,
}

fn reservation_fits(available_pages: usize, reserved_pages: usize, page_budget: usize) -> bool {
    page_budget <= available_pages.saturating_sub(reserved_pages)
}

fn admission_disposition(
    model: &Model,
    request: &EngineRequest,
    reserved_pages: usize,
) -> AdmissionDisposition {
    let page = model.kv.cfg.page_size;
    let need_pages = request
        .prompt_tokens
        .len()
        .checked_add(request.max_tokens.max(1))
        .map(|total| total.div_ceil(page));
    let capacity = model.max_request_pages();
    let Some(need_pages) = need_pages else {
        return AdmissionDisposition::Reject(
            "request size overflows: prompt_tokens + max_tokens".into(),
        );
    };
    if need_pages > capacity {
        return AdmissionDisposition::Reject(format!(
            "request needs {need_pages} KV pages, cache has {capacity} total"
        ));
    }
    let page_budget = if model.tier_enabled() {
        need_pages.min(crate::tier::min_resident_pages(page))
    } else {
        need_pages
    };
    if reservation_fits(model.available_pages(), reserved_pages, page_budget) {
        AdmissionDisposition::Admit { page_budget }
    } else {
        AdmissionDisposition::Wait
    }
}

fn first_actionable_index(
    len: usize,
    scan_window: usize,
    oldest_bypasses: usize,
    bypass_budget: usize,
    mut classify: impl FnMut(usize) -> AdmissionDisposition,
) -> Option<(usize, AdmissionDisposition)> {
    let limit = if oldest_bypasses >= bypass_budget {
        len.min(1)
    } else {
        len.min(scan_window)
    };
    (0..limit).find_map(|index| {
        let disposition = classify(index);
        (!matches!(&disposition, AdmissionDisposition::Wait)).then_some((index, disposition))
    })
}

fn reserved_future_pages(active: &[ActiveSeq<'_>], tier_enabled: bool) -> usize {
    active
        .iter()
        .filter(|seq| !seq.dead)
        .map(|seq| {
            let allocated = if tier_enabled {
                seq.seq.pages.iter().filter(|&&page| page >= 0).count()
            } else {
                seq.seq.pages.len()
            };
            seq.kv_page_budget.saturating_sub(allocated)
        })
        .fold(0usize, usize::saturating_add)
}

/// Spawn the GPU worker thread. `prefill_chunk` bounds how many prompt tokens
/// one sequence may prefill per scheduler iteration, protecting decode ITL of
/// the other active sequences (chunked prefill).
pub fn spawn_engine(
    model: Model,
    tokenizer: Arc<Tokenizer>,
    max_active: usize,
    prefill_chunk: usize,
) -> Result<EngineHandle> {
    spawn_engine_batched(
        model,
        tokenizer,
        max_active,
        prefill_chunk,
        default_batch_min(),
        SpeculativeConfig::off(),
    )
}

/// Default minimum decode concurrency before the batched forward path engages
/// (env override `FORGE_BATCH_MIN`). Below this many simultaneously-decoding
/// sequences the scheduler serializes the tuned fused single-seq path (faster
/// at low concurrency); at/above it the batched pass amortizes its flat
/// per-step cost across the batch.
fn default_batch_min() -> usize {
    // Measured crossover on the RTX 4090 (qwen3-0.6b-q8_0 and Bielik-7B-NVFP4):
    // the batched aggregate throughput overtakes serialized fused decoding at
    // ~12 concurrent sequences (both are bound by the fixed GEMM token-tile
    // cost, so per-step time is nearly flat in the batch size). Default just
    // past that so the batched path only engages when it wins.
    std::env::var("FORGE_BATCH_MIN")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&v| v >= 2)
        .unwrap_or(12)
}

/// `spawn_engine` with an explicit batched-path engagement threshold.
pub fn spawn_engine_batched(
    mut model: Model,
    tokenizer: Arc<Tokenizer>,
    max_active: usize,
    prefill_chunk: usize,
    batch_min: usize,
    spec: SpeculativeConfig,
) -> Result<EngineHandle> {
    let native_mtp_b2 = parse_native_mtp_b2(std::env::var("FORGE_NATIVE_MTP_B2").ok().as_deref())?;
    let coordinator = SpeculationCoordinator::new(spec.clone())?;
    validate_speculation_server_config(spec.kind(), max_active, model.is_hybrid())?;
    if spec.is_enabled() {
        match spec.kind() {
            SpeculationKind::NativeMtp | SpeculationKind::NativeMtpNgram
                if !model.has_native_mtp() =>
            {
                return Err(ForgeError::Unsupported(
                    "natywne MTP wymaga obsługiwanego runtime per sekwencja".into(),
                ));
            }
            SpeculationKind::NativeMtp | SpeculationKind::NativeMtpNgram => {
                model.validate_native_mtp_target()?
            }
            SpeculationKind::HostProposer => {
                model.validate_speculation_target(spec.draft_tokens())?
            }
            SpeculationKind::Off => {}
        }
        let proposer_names = spec
            .proposers()
            .iter()
            .map(|kind| kind.as_str())
            .collect::<Vec<_>>()
            .join("+");
        tracing::info!(
            "speculative decoding: {} proposer, draft budget {}",
            proposer_names,
            spec.draft_tokens()
        );
    }
    if model.is_hybrid() {
        model.preflight_hybrid_state_slots(max_active)?;
        if max_active >= 2
            && spec.kind() == SpeculationKind::Off
            && model.hybrid_batch_b2_capable()
        {
            model.ensure_batch(2)?;
        }
    }
    let (tx, rx) = mpsc::channel::<Submission>();
    let metrics = Arc::new(EngineMetrics::new());
    let worker_metrics = metrics.clone();
    let worker = std::thread::Builder::new()
        .name("forge-engine-worker".into())
        .spawn(move || {
            worker(
                &mut model,
                &tokenizer,
                &rx,
                max_active,
                prefill_chunk,
                batch_min,
                spec,
                coordinator,
                native_mtp_b2,
                &worker_metrics,
            )
        })
        .map_err(ForgeError::Io)?;
    Ok(EngineHandle {
        tx,
        metrics,
        worker: Arc::new(Mutex::new(Some(worker))),
    })
}

#[allow(clippy::too_many_arguments)]
fn worker<'t>(
    model: &mut Model,
    tokenizer: &'t Tokenizer,
    rx: &mpsc::Receiver<Submission>,
    max_active: usize,
    prefill_chunk: usize,
    batch_min: usize,
    spec: SpeculativeConfig,
    coordinator: SpeculationCoordinator,
    native_mtp_b2: bool,
    metrics: &EngineMetrics,
) {
    let mut active: Vec<ActiveSeq<'t>> = Vec::new();
    let mut waiting: VecDeque<Submission> = VecDeque::new();
    let admission_scan_window = max_active.saturating_mul(2).clamp(2, 16);
    let admission_bypass_budget = admission_scan_window.saturating_mul(2);
    // Total KV pages is fixed at startup; export it once as a gauge baseline.
    EngineMetrics::set(&metrics.kv_pages_total, model.kv.cfg.n_pages as u64);

    // Provision the batched-decode scratch + graph buckets for the full active
    // width once. A failure here (VRAM pressure) is surfaced per-request by the
    // per-batch re-ensure inside `batched_decode`.
    if !model.is_hybrid() {
        let _ = model.ensure_batch(max_active);
    }

    loop {
        // Drain the submission queue without blocking while work is active;
        // block when fully idle to avoid spinning.
        loop {
            match if active.is_empty() && waiting.is_empty() {
                rx.recv().map_err(|_| ())
            } else {
                rx.try_recv().map_err(|_| ())
            } {
                Ok(sub) => waiting.push_back(sub),
                Err(()) => break,
            }
            if active.is_empty() && waiting.is_empty() {
                return; // all handles dropped
            }
        }
        if active.is_empty() && waiting.is_empty() {
            // recv() disconnected
            return;
        }

        // Admission: KV projection — prompt + max_tokens must fit in pages
        // (the VRAM pool, or the tier-extended context window when tiering
        // is on — a tiered sequence only needs its hot working set resident).
        while active.len() < max_active {
            let reserved_pages = reserved_future_pages(&active, model.tier_enabled());
            let oldest_bypasses = waiting.front().map_or(0, |sub| sub.bypasses);
            let Some((index, disposition)) = first_actionable_index(
                waiting.len(),
                admission_scan_window,
                oldest_bypasses,
                admission_bypass_budget,
                |index| admission_disposition(model, &waiting[index].req, reserved_pages),
            ) else {
                break;
            };
            for skipped in waiting.iter_mut().take(index) {
                skipped.bypasses = skipped.bypasses.saturating_add(1);
            }
            waiting.rotate_left(index);
            let sub = waiting
                .pop_front()
                .expect("wybrany request admission istnieje w kolejce");
            waiting.rotate_right(index);
            let page_budget = match disposition {
                AdmissionDisposition::Admit { page_budget } => page_budget,
                AdmissionDisposition::Reject(error) => {
                    let _ = sub.events.send(EngineEvent::Error(error));
                    EngineMetrics::inc(&metrics.requests_errored);
                    continue;
                }
                AdmissionDisposition::Wait => unreachable!("wait nie jest wybierany przez admission"),
            };
            if sub.req.prompt_tokens.is_empty() {
                let _ = sub.events.send(EngineEvent::Error("empty prompt".into()));
                EngineMetrics::inc(&metrics.requests_errored);
                continue;
            }
            let prompt = sub.req.prompt_tokens;
            let prompt_len = prompt.len();
            // The CPU sampler runs whenever the request needs the full host
            // logits before selection: the grammar mask, `logit_bias`,
            // `min_tokens` (EOS suppression) or `logprobs` (host log-softmax).
            // Otherwise the GPU sampler is kept whenever it fits.
            let matcher = sub.req.grammar.as_ref().map(|g| g.matcher());
            let host_logits = matcher.is_some()
                || !sub.req.logit_bias.is_empty()
                || sub.req.min_tokens > 0
                || sub.req.logprobs.is_some();
            let mut sampler = if !host_logits && model.gpu_sampling_supported(&sub.req.sampling) {
                SeqSampler::Gpu(GpuSampler::new(sub.req.sampling.clone()))
            } else {
                SeqSampler::Cpu(Sampler::new(sub.req.sampling.clone()))
            };
            if let SeqSampler::Gpu(gpu_sampler) = &mut sampler {
                gpu_sampler.note_tokens(&prompt);
            }
            let sampling_history = if matches!(sampler, SeqSampler::Cpu(_))
                && sub.req.sampling.clone().sanitized().has_penalties()
            {
                prompt.clone()
            } else {
                Vec::new()
            };
            // Speculative decoding engages only for a greedy, penalty-free,
            // GPU-sampled sequence (the n-gram verifier reproduces greedy argmax
            // exactly; a repetition penalty or host-logit feature would diverge).
            // The proposer indexes the whole prompt so repeated/structured
            // prefixes draft immediately.
            let greedy = matches!(sampler, SeqSampler::Gpu(_))
                && sub.req.sampling.clone().sanitized().temperature <= 0.0
                && !sub.req.sampling.clone().sanitized().has_penalties();
            let request_spec_kind = match request_speculation_kind(&spec, greedy) {
                Ok(kind) => kind,
                Err(error) => {
                    let _ = sub.events.send(EngineEvent::Error(error.to_string()));
                    EngineMetrics::inc(&metrics.requests_errored);
                    continue;
                }
            };
            // Najdłuższy prefiks jest przypinany dopiero po sprawdzeniu MTP,
            // aby odrzucone żądanie nie pozostawiło zajętych stron.
            let mut seq = model.new_seq();
            let cache_read = model.acquire_prefix(&mut seq, &prompt);
            let pending_prompt: VecDeque<u32> = prompt[cache_read..].iter().copied().collect();
            if spec.is_enabled() && !greedy {
                tracing::info!(
                    configured = ?spec.kind(),
                    fallback = ?request_spec_kind,
                    "spekulacja wyłączona dla żądania wymagającego non-greedy lub host logits"
                );
            }
            let (spec_state, spec_kind, spec_budget) = if request_spec_kind != SpeculationKind::Off {
                let state = match coordinator.new_state(&prompt) {
                    Ok(state) => state,
                    Err(error) => {
                        let _ = sub.events.send(EngineEvent::Error(error.to_string()));
                        continue;
                    }
                };
                (state, request_spec_kind, spec.draft_tokens())
            } else {
                (None, SpeculationKind::Off, 0)
            };
            active.push(ActiveSeq {
                seq,
                sampler,
                decoder: StreamDecoder::new(tokenizer, true),
                stops: StopMatcher::new(sub.req.stop.clone()),
                events: sub.events,
                pending_prompt,
                generated: Vec::new(),
                sampling_history,
                max_tokens: sub.req.max_tokens.max(1),
                eos_ids: sub.req.eos_ids,
                logit_bias: sub.req.logit_bias,
                min_tokens: sub.req.min_tokens,
                logprobs: sub.req.logprobs,
                matcher,
                prompt_len,
                cache_read,
                next: None,
                dead: false,
                spec: spec_state,
                spec_kind,
                spec_budget,
                spec_forwards: 0,
                spec_accepted: 0,
                ngram_forwards: 0,
                mtp_fallback_forwards: 0,
                mtp_k2_rate: None,
                mtp_k3_rate: None,
                timing: SeqTiming::new(),
                finished_cleanly: false,
                submitted_at: sub.submitted_at,
                prefill_profile: None,
                benchmark_ttft_ms: None,
                kv_page_budget: page_budget,
            });
            EngineMetrics::inc(&metrics.requests_started);
        }

        // Gauges reflect the worker's current view each iteration: how many
        // sequences decode/prefill, how deep the admission queue is, and how
        // many KV pages are off the free stack (held by seqs + prefix tree).
        EngineMetrics::set(&metrics.active_sequences, active.len() as u64);
        EngineMetrics::set(&metrics.queued_sequences, waiting.len() as u64);
        EngineMetrics::set(
            &metrics.kv_pages_used,
            (model.kv.cfg.n_pages.saturating_sub(model.kv.free_page_count())) as u64,
        );

        // Cross-sequence tier balance: before the iteration touches the pool,
        // spill the globally coldest pages (across all active sequences) so
        // the upcoming prefill quanta and decode appends fit with the
        // watermark reserve intact — one long-context request no longer
        // stalls behind neighbors' cold history.
        if model.tier_enabled() {
            let page = model.kv.cfg.page_size;
            let quantum = prefill_chunk.clamp(32, MAX_PREFILL_CHUNK);
            let upcoming_pages: usize = active
                .iter()
                .filter(|a| !a.dead)
                .map(|a| {
                    if a.pending_prompt.is_empty() {
                        1
                    } else {
                        quantum.min(a.pending_prompt.len()).div_ceil(page) + 1
                    }
                })
                .sum();
            let mut seqs: Vec<&mut SeqKv> = active
                .iter_mut()
                .filter(|a| !a.dead)
                .map(|a| &mut a.seq)
                .collect();
            if let Err(e) = model.tier_balance(&mut seqs, upcoming_pages) {
                // Balance failure (e.g. RAM-tier budget exhausted) surfaces
                // per-request when the affected sequence next grows.
                tracing::warn!("kv tier balance failed: {e}");
            }
        }

        // One scheduler iteration. Prefilling sequences and any CPU-sampled
        // decode sequence advance individually (chunked prefill / one token);
        // every GPU-sampled decode sequence runs through ONE batched forward
        // pass (`batch_gpu_decode`) — the continuous-batching throughput path.
        for a in active.iter_mut() {
            if a.dead {
                continue;
            }
            let gpu = matches!(a.sampler, SeqSampler::Gpu(_));
            let decoding = a.pending_prompt.is_empty();
            if gpu && decoding {
                continue; // handled by the batch below
            }
            if let Err(e) = advance(model, a, prefill_chunk, metrics) {
                let _ = a.events.send(EngineEvent::Error(e.to_string()));
                a.dead = true;
            }
        }
        batch_gpu_decode(model, &mut active, batch_min, native_mtp_b2, metrics);

        // Tear down finished/dead sequences and release their pages (and any
        // tier chunks they spilled).
        active.retain_mut(|a| {
            if a.dead {
                // A dead sequence that never reached `finish` (engine error,
                // batched-decode failure, or client hang-up) is an errored
                // outcome for the counter; clean completions already counted in
                // `finish`.
                if !a.finished_cleanly {
                    EngineMetrics::inc(&metrics.requests_errored);
                }
                model.release_seq(&mut a.seq);
                false
            } else {
                true
            }
        });
    }
}

/// Whether emitting a token finished the sequence (stop/eos/length/client
/// hang-up) or it should keep decoding.
enum StepOutcome {
    Continue,
    Finished,
}

/// Emit one sampled token through the decoder + stop matcher and apply the
/// termination checks (eos, stop string, max tokens, client hang-up). Shared
/// by the per-sequence (`advance`) and batched (`batch_gpu_decode`) paths.
fn emit_token(
    a: &mut ActiveSeq<'_>,
    next: u32,
    logprob: Option<TokenLogprob>,
    metrics: &EngineMetrics,
) -> Result<StepOutcome> {
    if a.benchmark_ttft_ms.is_none() && a.prefill_profile.is_some() {
        a.benchmark_ttft_ms = Some(a.submitted_at.elapsed().as_secs_f64() * 1000.0);
    }
    if a.eos_ids.contains(&next) {
        finish(a, FinishReason::Eos, metrics)?;
        return Ok(StepOutcome::Finished);
    }
    a.generated.push(next);
    if !a.sampling_history.is_empty() {
        a.sampling_history.push(next);
    }
    // A produced token: feeds TTFT (first) or the inter-token gap (later).
    a.timing.record_token(metrics);

    let piece = a.decoder.push(next)?;
    let mut emit_text = String::new();
    let mut matched = false;
    if !piece.is_empty() {
        let step = a.stops.push(&piece);
        emit_text = step.emit;
        matched = step.matched.is_some();
    }
    // Send a token event when there is text to surface or a per-token
    // `logprobs` report to deliver (the latter must reach the client even for
    // a token whose byte-level piece is still buffered by the decoder).
    if (!emit_text.is_empty() || logprob.is_some())
        && a.events
            .send(EngineEvent::Token {
                id: next,
                text: emit_text,
                logprob,
            })
            .is_err()
    {
        // Client hung up — cancel generation, free the slot.
        a.dead = true;
        return Ok(StepOutcome::Finished);
    }
    if matched {
        finish(a, FinishReason::Stop, metrics)?;
        return Ok(StepOutcome::Finished);
    }

    if a.generated.len() >= a.max_tokens {
        finish(a, FinishReason::Length, metrics)?;
        return Ok(StepOutcome::Finished);
    }
    Ok(StepOutcome::Continue)
}

/// Advance one sequence by one scheduler quantum (prefill chunk or a single
/// CPU-sampled decode step). GPU-sampled decode goes through the batched path.
fn advance(
    model: &mut Model,
    a: &mut ActiveSeq<'_>,
    prefill_chunk: usize,
    metrics: &EngineMetrics,
) -> Result<()> {
    if !a.pending_prompt.is_empty() {
        // Chunked prefill: one quantum per iteration keeps decode ITL of the
        // other sequences bounded; the 32-token floor keeps tiny configured
        // quanta from wasting the batched kernels.
        let quantum = prefill_chunk.clamp(32, MAX_PREFILL_CHUNK);
        let take = quantum.min(a.pending_prompt.len());
        let chunk: Vec<u32> = a.pending_prompt.drain(..take).collect();
        let logits = model.prefill_chunk(&mut a.seq, &chunk)?;
        if a.pending_prompt.is_empty() {
            a.prefill_profile = model.take_prefill_profile()?;
            a.next = Some(match &mut a.sampler {
                SeqSampler::Cpu(_) => PendingNext::Logits(logits),
                // The prefill logits are still device-resident: draw now,
                // before another sequence overwrites the shared buffer.
                SeqSampler::Gpu(g) => PendingNext::Token(model.sample_last_logits(g)?),
            });
        }
        return Ok(());
    }

    // CPU-sampled decode (GPU decode is batched elsewhere).
    let (next, logprob) = match a
        .next
        .take()
        .ok_or_else(|| ForgeError::Scheduler("missing next-token state".into()))?
    {
        PendingNext::Logits(mut logits) => match &mut a.sampler {
            SeqSampler::Cpu(s) => {
                // Apply `logit_bias`, suppress EOS below `min_tokens`, then the
                // grammar mask — every non-conforming token forbidden before
                // selection so the sampled id always advances the constraints.
                apply_logit_bias(&mut logits, &a.logit_bias);
                suppress_eos(&mut logits, &a.eos_ids, a.generated.len(), a.min_tokens);
                if let Some(m) = &a.matcher {
                    m.apply_mask(&mut logits);
                }
                apply_penalties(&mut logits, &a.sampling_history, s.params());
                let id = s.sample_preprocessed(&logits)?;
                let lp = a.logprobs.map(|n| compute_logprob(&logits, id, n));
                (id, lp)
            }
            SeqSampler::Gpu(_) => {
                return Err(ForgeError::Scheduler(
                    "GPU-sampled sequence carried host logits".into(),
                ))
            }
        },
        PendingNext::Token(t) => (t, None),
    };
    if let Some(m) = &mut a.matcher {
        m.accept_token(next);
    }

    if let StepOutcome::Finished = emit_token(a, next, logprob, metrics)? {
        return Ok(());
    }

    a.next = Some(match &mut a.sampler {
        SeqSampler::Cpu(_) => PendingNext::Logits(model.step(&mut a.seq, next)?),
        SeqSampler::Gpu(g) => {
            g.note_token(next);
            PendingNext::Token(model.step_and_sample(&mut a.seq, next, g)?)
        }
    });
    Ok(())
}

/// Run every GPU-sampled decode sequence through one batched forward pass.
/// Each sequence's current token is emitted and termination-checked first;
/// survivors are fed together into `Model::batched_decode`, which samples all
/// their successors on the GPU in one launch set.
fn batch_gpu_decode(
    model: &mut Model,
    active: &mut [ActiveSeq<'_>],
    batch_min: usize,
    native_mtp_b2: bool,
    metrics: &EngineMetrics,
) {
    // Phase 1: emit each ready sequence's pending token; collect the indices of
    // survivors that still need to be fed one more token.
    let mut feed_idx: Vec<usize> = Vec::new();
    let mut mtp_idx: Vec<usize> = Vec::new();
    for (i, a) in active.iter_mut().enumerate() {
        if a.dead || !a.pending_prompt.is_empty() {
            continue;
        }
        if !matches!(a.sampler, SeqSampler::Gpu(_)) {
            continue;
        }
        let next = match a.next.take() {
            Some(PendingNext::Token(t)) => t,
            Some(PendingNext::Logits(_)) => {
                let _ = a
                    .events
                    .send(EngineEvent::Error("GPU sequence carried host logits".into()));
                a.dead = true;
                continue;
            }
            None => continue,
        };
        match emit_token(a, next, None, metrics) {
            // A speculative sequence runs its own draft/verify step here and
            // never joins the batched (or serial fallback) feed set — the n-gram
            // path is a single-sequence latency win, disabled under batch load.
            Ok(StepOutcome::Continue) => {
                match a.spec_kind {
                    SpeculationKind::HostProposer => speculative_step(model, a, metrics),
                    SpeculationKind::NativeMtp => mtp_idx.push(i),
                    SpeculationKind::NativeMtpNgram => {
                        native_mtp_ngram_step(model, a, metrics)
                    }
                    SpeculationKind::Off => feed_idx.push(i),
                }
            }
            Ok(StepOutcome::Finished) => {}
            Err(e) => {
                let _ = a.events.send(EngineEvent::Error(e.to_string()));
                a.dead = true;
            }
        }
    }
    if native_mtp_b2 && !mtp_idx.is_empty() {
        batch_native_mtp_decode(model, active, &mtp_idx, metrics);
    } else {
        for index in mtp_idx {
            native_mtp_step(model, &mut active[index], metrics);
        }
    }
    if feed_idx.is_empty() {
        return;
    }

    // Below `batch_min` sequences, the tuned fused single-seq decode path
    // (6 launches/layer, graphed) run once per survivor is faster than the
    // tensor-core batched pass, whose per-step cost is nearly flat in the batch
    // size (the GEMMs process a fixed token tile). Serializing them here keeps
    // single-stream and low-concurrency latency from regressing; the batched
    // path engages once the flat cost amortizes across enough sequences.
    // MoE i rot utrzymują wiele aktywnych sekwencji, ale dekodują je pojedynczo,
    // ponieważ ich stan nie ma jeszcze batchowego kernela forward.
    let hybrid_b2 = model.hybrid_batch_b2_capable() && feed_idx.len() >= 2;
    let serial_only = model.weights.is_moe()
        || model.kv.cfg.quant.is_rot()
        || (model.is_hybrid() && !hybrid_b2);
    if (!hybrid_b2 && feed_idx.len() < batch_min.max(2)) || serial_only {
        for &i in &feed_idx {
            serial_step(model, &mut active[i]);
        }
        return;
    }

    if hybrid_b2 {
        let mut pairs = feed_idx.chunks_exact(2);
        for pair in &mut pairs {
            decode_gpu_group(model, active, pair);
        }
        if let Some(&tail) = pairs.remainder().first() {
            serial_step(model, &mut active[tail]);
        }
        return;
    }

    decode_gpu_group(model, active, &feed_idx);
}

fn batch_native_mtp_decode(
    model: &mut Model,
    active: &mut [ActiveSeq<'_>],
    mtp_idx: &[usize],
    metrics: &EngineMetrics,
) {
    let mut by_budget = [Vec::new(), Vec::new()];
    for &index in mtp_idx {
        let a = &active[index];
        let available = model.native_mtp_available_budget(&a.seq, a.spec_budget);
        match native_mtp_adaptive_budget(
            available,
            a.spec_budget,
            a.spec_forwards,
            a.mtp_k2_rate,
            a.mtp_k3_rate,
        ) {
            Some(2) => by_budget[0].push(index),
            Some(3) => by_budget[1].push(index),
            _ => serial_step(model, &mut active[index]),
        }
    }

    for (slot, indices) in by_budget.iter().enumerate() {
        let budget = slot + 2;
        let mut pairs = indices.chunks_exact(2);
        for pair in &mut pairs {
            let first = pair[0];
            let second = pair[1];
            let capable = model.native_mtp_b2_capable(
                [&active[first].seq, &active[second].seq],
                budget,
            );
            if capable {
                native_mtp_step_b2(model, active, [first, second], budget, metrics);
            } else {
                native_mtp_step(model, &mut active[first], metrics);
                native_mtp_step(model, &mut active[second], metrics);
            }
        }
        if let Some(&tail) = pairs.remainder().first() {
            native_mtp_step(model, &mut active[tail], metrics);
        }
    }
}

fn decode_gpu_group(
    model: &mut Model,
    active: &mut [ActiveSeq<'_>],
    feed_idx: &[usize],
) {
    let vocab = model.weights.descriptor.params.vocab_size;
    // Phase 2: gather the batch. Disjoint field borrows let one pass hand the
    // KV handle to the model while snapshotting each sampler's params.
    let mut seqs: Vec<&mut SeqKv> = Vec::with_capacity(feed_idx.len());
    let mut tokens: Vec<u32> = Vec::with_capacity(feed_idx.len());
    let mut params: Vec<SeqSampleParams> = Vec::with_capacity(feed_idx.len());
    let mut fi = 0usize;
    for (i, a) in active.iter_mut().enumerate() {
        if fi >= feed_idx.len() || feed_idx[fi] != i {
            continue;
        }
        fi += 1;
        let fed = *a.generated.last().expect("emit_token pushed the fed token");
        let seq = &mut a.seq;
        let SeqSampler::Gpu(g) = &mut a.sampler else {
            unreachable!("feed set is GPU-only")
        };
        g.note_token(fed);
        params.push(g.batch_params(vocab));
        tokens.push(fed);
        seqs.push(seq);
    }

    let results = match model.batched_decode(&mut seqs, &tokens, &params) {
        Ok(r) => r,
        Err(e) => {
            drop(seqs);
            let msg = e.to_string();
            let mut ri = 0usize;
            for (i, a) in active.iter_mut().enumerate() {
                if ri < feed_idx.len() && feed_idx[ri] == i {
                    ri += 1;
                    let _ = a.events.send(EngineEvent::Error(msg.clone()));
                    a.dead = true;
                }
            }
            return;
        }
    };
    drop(seqs);

    // Phase 3: stash each successor as the next iteration's pending token.
    let mut ri = 0usize;
    for (i, a) in active.iter_mut().enumerate() {
        if ri < feed_idx.len() && feed_idx[ri] == i {
            a.next = Some(PendingNext::Token(results[ri]));
            ri += 1;
        }
    }
}

/// Advance one GPU-sampled decode sequence by a single fused (or streamed)
/// step, stashing its successor as the next iteration's pending token.
fn serial_step(model: &mut Model, a: &mut ActiveSeq<'_>) {
    let fed = *a.generated.last().expect("emit_token pushed the fed token");
    let SeqSampler::Gpu(g) = &mut a.sampler else {
        unreachable!("feed set is GPU-only")
    };
    g.note_token(fed);
    match model.step_and_sample(&mut a.seq, fed, g) {
        Ok(t) => a.next = Some(PendingNext::Token(t)),
        Err(e) => {
            let _ = a.events.send(EngineEvent::Error(e.to_string()));
            a.dead = true;
        }
    }
}

/// One speculative decode step for a greedy, eligible sequence (SPEC §6). The
/// just-emitted token is the draft base `fed`; the n-gram proposer drafts a
/// continuation from the sequence's own history, ONE verify forward checks it,
/// accepted drafts are emitted as output and the correction/bonus token is
/// stashed as the next pending token. An empty draft (no suffix match) falls
/// back to a single fused greedy step, identical to `serial_step`. Output is
/// token-for-token identical to non-speculative greedy decode.
fn speculative_step(model: &mut Model, a: &mut ActiveSeq<'_>, metrics: &EngineMetrics) {
    let fed = *a.generated.last().expect("emit_token pushed the fed token");
    let budget = a.spec_budget;
    let draft = {
        let s = a.spec.as_mut().expect("speculative_step on a spec sequence");
        s.observe(fed);
        match s.draft(budget) {
            Ok(draft) => draft,
            Err(error) => {
                let _ = a.events.send(EngineEvent::Error(error.to_string()));
                a.dead = true;
                return;
            }
        }
    };

    // A verify forward runs the ungraphed prefill path; on a small model each
    // graphed single-token decode step is so cheap that a verify only pays off
    // when it can replace several of them. Verifying a short draft would lose
    // (its wasted rejections cost more than the few decodes it could save), so a
    // draft below the gate falls back to the plain graphed step — this is what
    // keeps ordinary prose (which only ever yields short coincidental drafts)
    // from regressing.
    if draft.len() < MIN_VERIFY_DRAFT.min(budget) {
        if let Some(s) = &mut a.spec {
            s.cancel_draft();
        }
        // Short or absent draft — plain single-token greedy step (the
        // `serial_step` path), then record the confirmed token in the history.
        let SeqSampler::Gpu(g) = &mut a.sampler else {
            unreachable!("spec sequences are GPU-sampled")
        };
        g.note_token(fed);
        match model.step_and_sample(&mut a.seq, fed, g) {
            Ok(t) => a.next = Some(PendingNext::Token(t)),
            Err(e) => {
                let _ = a.events.send(EngineEvent::Error(e.to_string()));
                a.dead = true;
            }
        }
        return;
    }

    let (accepted, correction) = match model.verify_greedy_draft(&mut a.seq, fed, &draft) {
        Ok(r) => r,
        Err(e) => {
            if let Some(s) = &mut a.spec {
                s.cancel_draft();
            }
            let _ = a.events.send(EngineEvent::Error(e.to_string()));
            a.dead = true;
            return;
        }
    };
    a.spec_forwards += 1;
    a.spec_accepted += accepted as u64;
    // Acceptance feedback: advances the proposer's own history over the accepted
    // drafts and updates the adaptive-disable stats (SPEC §6 sleep-on-no-gain).
    if let Some(s) = &mut a.spec {
        if let Err(error) = s.commit(&draft, accepted) {
            let _ = a.events.send(EngineEvent::Error(error.to_string()));
            a.dead = true;
            return;
        }
    }

    // Emit accepted drafts as generated output, in order. A stop / eos / length
    // / hang-up here finishes the sequence (KV is already rolled back to the
    // accepted prefix) — exactly what sequential decode would have produced.
    for &tok in &draft[..accepted] {
        match emit_token(a, tok, None, metrics) {
            Ok(StepOutcome::Continue) => {}
            Ok(StepOutcome::Finished) => return,
            Err(e) => {
                let _ = a.events.send(EngineEvent::Error(e.to_string()));
                a.dead = true;
                return;
            }
        }
    }
    // Token korekcyjny zostanie dodany do historii po emisji w następnym kroku.
    a.next = Some(PendingNext::Token(correction));
}

/// Priorytetowy router: pełny draft n-gram omija proposer MTP, a brak pełnego
/// draftu uruchamia zwykły krok natywnego MTP.
fn native_mtp_ngram_step(model: &mut Model, a: &mut ActiveSeq<'_>, metrics: &EngineMetrics) {
    let fed = *a.generated.last().expect("emit_token pushed the fed token");
    let budget = a.spec_budget;
    let draft = {
        let state = a
            .spec
            .as_mut()
            .expect("router MTP+n-gram ma stan hostowy");
        state.observe(fed);
        match state.draft(budget) {
            Ok(draft) => draft,
            Err(error) => {
                let _ = a.events.send(EngineEvent::Error(error.to_string()));
                a.dead = true;
                return;
            }
        }
    };
    if draft.len() != budget {
        if let Some(state) = &mut a.spec {
            state.cancel_draft();
        }
        let generated_before = a.generated.len();
        a.mtp_fallback_forwards += 1;
        native_mtp_step(model, a, metrics);
        let accepted = a.generated[generated_before..].to_vec();
        if let Some(state) = &mut a.spec {
            state.observe_all(&accepted);
        }
        return;
    }

    let SeqSampler::Gpu(sampler) = &mut a.sampler else {
        unreachable!("router MTP+n-gram wymaga samplera GPU")
    };
    sampler.note_token(fed);
    let (accepted, correction) =
        match model.verify_greedy_draft_with_mtp_catchup(&mut a.seq, fed, &draft) {
            Ok(result) => result,
            Err(error) => {
                if let Some(state) = &mut a.spec {
                    state.cancel_draft();
                }
                let _ = a.events.send(EngineEvent::Error(error.to_string()));
                a.dead = true;
                return;
            }
        };
    if let Some(state) = &mut a.spec {
        if let Err(error) = state.commit(&draft, accepted) {
            let _ = a.events.send(EngineEvent::Error(error.to_string()));
            a.dead = true;
            return;
        }
    }
    a.spec_forwards += 1;
    a.ngram_forwards += 1;
    a.spec_accepted += accepted as u64;
    for &token in &draft[..accepted] {
        match emit_token(a, token, None, metrics) {
            Ok(StepOutcome::Continue) => {}
            Ok(StepOutcome::Finished) => return,
            Err(error) => {
                let _ = a.events.send(EngineEvent::Error(error.to_string()));
                a.dead = true;
                return;
            }
        }
    }
    a.next = Some(PendingNext::Token(correction));
}

/// Jeden krok natywnego MTP. Model jest właścicielem draftu, weryfikatora i
/// checkpointów; serwer emituje zaakceptowany prefiks i zachowuje token
/// korekcyjny jako wejście następnej iteracji.
fn native_mtp_step(model: &mut Model, a: &mut ActiveSeq<'_>, metrics: &EngineMetrics) {
    let fed = *a.generated.last().expect("emit_token pushed the fed token");
    let available = model.native_mtp_available_budget(&a.seq, a.spec_budget);
    let Some(budget) = native_mtp_adaptive_budget(
        available,
        a.spec_budget,
        a.spec_forwards,
        a.mtp_k2_rate,
        a.mtp_k3_rate,
    ) else {
        serial_step(model, a);
        return;
    };
    let SeqSampler::Gpu(sampler) = &mut a.sampler else {
        unreachable!("native MTP is GPU-sampled")
    };
    sampler.note_token(fed);
    let started = Instant::now();
    let (draft, accepted, correction) = match model.native_mtp_step(&mut a.seq, fed, budget) {
        Ok(result) => result,
        Err(error) => {
            let _ = a.events.send(EngineEvent::Error(error.to_string()));
            a.dead = true;
            return;
        }
    };
    finish_native_mtp_step(
        a,
        budget,
        draft,
        accepted,
        correction,
        started.elapsed(),
        metrics,
    );
}

fn native_mtp_step_b2(
    model: &mut Model,
    active: &mut [ActiveSeq<'_>],
    indices: [usize; 2],
    budget: usize,
    metrics: &EngineMetrics,
) {
    let [first_index, second_index] = indices;
    let (left, right) = active.split_at_mut(second_index);
    let first = &mut left[first_index];
    let second = &mut right[0];
    let fed = [
        *first.generated.last().expect("emit_token dodał fed lane0"),
        *second.generated.last().expect("emit_token dodał fed lane1"),
    ];
    let SeqSampler::Gpu(first_sampler) = &mut first.sampler else {
        unreachable!("native MTP B2 wymaga samplera GPU")
    };
    first_sampler.note_token(fed[0]);
    let SeqSampler::Gpu(second_sampler) = &mut second.sampler else {
        unreachable!("native MTP B2 wymaga samplera GPU")
    };
    second_sampler.note_token(fed[1]);
    let started = Instant::now();
    let result = model.native_mtp_step_b2(
        &mut [&mut first.seq, &mut second.seq],
        fed,
        budget,
    );
    let elapsed = started.elapsed();
    let [first_result, second_result] = match result {
        Ok(results) => results,
        Err(error) => {
            let message = error.to_string();
            for a in [first, second] {
                let _ = a.events.send(EngineEvent::Error(message.clone()));
                a.dead = true;
            }
            return;
        }
    };
    finish_native_mtp_step(
        first,
        budget,
        first_result.0,
        first_result.1,
        first_result.2,
        elapsed,
        metrics,
    );
    finish_native_mtp_step(
        second,
        budget,
        second_result.0,
        second_result.1,
        second_result.2,
        elapsed,
        metrics,
    );
}

fn finish_native_mtp_step(
    a: &mut ActiveSeq<'_>,
    budget: usize,
    draft: Vec<u32>,
    accepted: usize,
    correction: u32,
    elapsed: Duration,
    metrics: &EngineMetrics,
) {
    if a.spec_forwards > 0 {
        let rate = (accepted + 1) as f64 / elapsed.as_secs_f64();
        if budget == 2 {
            update_mtp_rate(&mut a.mtp_k2_rate, rate);
        } else {
            update_mtp_rate(&mut a.mtp_k3_rate, rate);
        }
    }
    if draft.len() != budget || accepted > draft.len() {
        let _ = a.events.send(EngineEvent::Error(
            "native MTP returned an invalid draft or acceptance length".into(),
        ));
        a.dead = true;
        return;
    }
    a.spec_forwards += 1;
    a.spec_accepted += accepted as u64;
    for &token in &draft[..accepted] {
        match emit_token(a, token, None, metrics) {
            Ok(StepOutcome::Continue) => {}
            Ok(StepOutcome::Finished) => return,
            Err(error) => {
                let _ = a.events.send(EngineEvent::Error(error.to_string()));
                a.dead = true;
                return;
            }
        }
    }
    a.next = Some(PendingNext::Token(correction));
}

fn finish(a: &mut ActiveSeq<'_>, mut reason: FinishReason, metrics: &EngineMetrics) -> Result<()> {
    // Flush held text through the same stop/event path as live tokens.
    if reason != FinishReason::Stop {
        let last_id = a.generated.last().copied().unwrap_or(0);
        let tail = a.decoder.finish()?;
        if !tail.is_empty() {
            let step = a.stops.push(&tail);
            if !step.emit.is_empty() {
                let _ = a.events.send(EngineEvent::Token {
                    id: last_id,
                    text: step.emit,
                    logprob: None,
                });
            }
            if step.matched.is_some() {
                reason = FinishReason::Stop;
            }
        }
        if reason != FinishReason::Stop {
            let rest = a.stops.finish();
            if !rest.is_empty() {
                let _ = a.events.send(EngineEvent::Token {
                    id: last_id,
                    text: rest,
                    logprob: None,
                });
            }
        }
    }
    // Speculation report (SPEC §6): tokens produced per verify forward and the
    // effective decode speedup (each forward also yields one correction/bonus
    // token, so a forward emits `1 + accepted` tokens).
    if let Some(s) = &a.spec {
        if a.spec_forwards > 0 {
            let forwards = a.spec_forwards as f64;
            let tokens_from_spec = forwards + a.spec_accepted as f64;
            tracing::info!(
                "speculation: {} verify forwards, {} accepted draft tokens \
                 ({:.2} accepted/step, {:.2}x tokens/forward); proposer stats {:?}",
                a.spec_forwards,
                a.spec_accepted,
                a.spec_accepted as f64 / forwards,
                tokens_from_spec / forwards,
                s.stats(),
            );
            if a.spec_kind == SpeculationKind::NativeMtpNgram {
                tracing::info!(
                    "router MTP+n-gram: {} n-gram verify, {} MTP fallback",
                    a.ngram_forwards,
                    a.mtp_fallback_forwards,
                );
            }
        }
    } else if a.spec_kind == SpeculationKind::NativeMtp && a.spec_forwards > 0 {
        let forwards = a.spec_forwards as f64;
        let tokens_from_spec = forwards + a.spec_accepted as f64;
        tracing::info!(
            "native MTP: {} verify forwards, {} accepted draft tokens \
             ({:.2} accepted/step, {:.2}x tokens/forward)",
            a.spec_forwards,
            a.spec_accepted,
            a.spec_accepted as f64 / forwards,
            tokens_from_spec / forwards,
        );
    }
    // Terminal accounting (SPEC §8.3): a finished request contributes its
    // prompt/generated/cache-read token totals, speculation acceptance and its
    // decode throughput. `finish` runs exactly once per sequence (it sets
    // `dead`), so no double counting.
    EngineMetrics::inc(&metrics.requests_finished);
    EngineMetrics::add(&metrics.prompt_tokens_total, a.prompt_len as u64);
    EngineMetrics::add(&metrics.generated_tokens_total, a.generated.len() as u64);
    EngineMetrics::add(&metrics.cache_read_tokens_total, a.cache_read as u64);
    EngineMetrics::add(&metrics.spec_forwards_total, a.spec_forwards);
    EngineMetrics::add(&metrics.spec_accepted_total, a.spec_accepted);
    a.timing.record_decode_tps(metrics);
    let _ = a.events.send(EngineEvent::Done {
        reason,
        tokens: a.generated.len(),
        prompt_tokens: a.prompt_len,
        cache_read_tokens: a.cache_read,
        benchmark: a.prefill_profile.zip(a.benchmark_ttft_ms).map(|(profile, ttft_ms)| {
            BenchmarkTimings {
                target_gpu_ms: profile.target_gpu_ms,
                mtp_catchup_gpu_ms: profile.mtp_catchup_gpu_ms,
                ttft_ms,
            }
        }),
    });
    a.dead = true;
    a.finished_cleanly = true;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        AdmissionDisposition, first_actionable_index, native_mtp_adaptive_budget,
        native_mtp_step_budget, parse_native_mtp_b2, request_speculation_kind,
        reservation_fits, update_mtp_rate, validate_speculation_server_config,
    };
    use crate::speculation::{ProposerKind, SpeculationKind, SpeculativeConfig};

    #[test]
    fn przelacznik_mtp_b2_wymaga_scislej_wartosci_logicznej() {
        assert!(parse_native_mtp_b2(None).unwrap());
        assert!(parse_native_mtp_b2(Some("1")).unwrap());
        assert!(!parse_native_mtp_b2(Some("0")).unwrap());
        assert!(parse_native_mtp_b2(Some("true")).is_err());
        assert!(parse_native_mtp_b2(Some("")).is_err());
    }

    #[test]
    fn admission_omija_chwilowo_zablokowany_front_bez_zmiany_fifo() {
        let states = [false, true, true];
        let (first, _) = first_actionable_index(states.len(), 3, 0, 4, |index| {
            if states[index] {
                AdmissionDisposition::Admit { page_budget: 1 }
            } else {
                AdmissionDisposition::Wait
            }
        })
        .expect("drugi request powinien mieścić się w KV");
        assert_eq!(first, 1);

        let blocked = first_actionable_index(2, 2, 0, 4, |_| AdmissionDisposition::Wait);
        assert!(blocked.is_none());
    }

    #[test]
    fn admission_ogranicza_okno_i_blokuje_bypass_po_zestarzeniu() {
        let outside_window = first_actionable_index(4, 2, 0, 4, |index| {
            if index == 2 {
                AdmissionDisposition::Admit { page_budget: 1 }
            } else {
                AdmissionDisposition::Wait
            }
        });
        assert!(outside_window.is_none());

        let aged = first_actionable_index(3, 3, 4, 4, |index| {
            if index == 1 {
                AdmissionDisposition::Admit { page_budget: 1 }
            } else {
                AdmissionDisposition::Wait
            }
        });
        assert!(aged.is_none());
    }

    #[test]
    fn admission_odejmuje_wczesniejsze_zobowiazania_od_dostepnych_stron() {
        assert!(reservation_fits(8, 3, 5));
        assert!(!reservation_fits(8, 4, 5));
        assert!(!reservation_fits(3, 8, 1));
    }

    #[test]
    fn native_mtp_przechodzi_na_zwykly_decode_dla_non_greedy() {
        for budget in [2, 3] {
            let spec = SpeculativeConfig::chain(vec![ProposerKind::Mtp], budget)
                .expect("konfiguracja MTP powinna być poprawna");
            assert_eq!(
                request_speculation_kind(&spec, true).expect("greedy MTP powinno działać"),
                SpeculationKind::NativeMtp
            );
            assert_eq!(
                request_speculation_kind(&spec, false)
                    .expect("non-greedy powinno wyłączyć spekulację per request"),
                SpeculationKind::Off
            );
        }
        let router = SpeculativeConfig::chain(
            vec![ProposerKind::Mtp, ProposerKind::Ngram],
            3,
        )
        .expect("konfiguracja routera powinna być poprawna");
        assert_eq!(
            request_speculation_kind(&router, true).expect("greedy router powinien działać"),
            SpeculationKind::NativeMtpNgram
        );
        assert_eq!(
            request_speculation_kind(&router, false)
                .expect("non-greedy powinno wyłączyć router per request"),
            SpeculationKind::Off
        );
    }

    #[test]
    fn host_proposer_keeps_existing_fallback() {
        let spec = SpeculativeConfig::ngram(8).expect("konfiguracja n-gram");
        assert_eq!(
            request_speculation_kind(&spec, false).expect("n-gram może wyłączyć się per request"),
            SpeculationKind::Off
        );
        assert_eq!(
            request_speculation_kind(&spec, true).expect("greedy n-gram powinien działać"),
            SpeculationKind::HostProposer
        );
    }

    #[test]
    fn hybrid_i_mtp_dopuszczaja_wiele_aktywnych_sekwencji() {
        assert!(validate_speculation_server_config(SpeculationKind::NativeMtp, 0, true).is_err());
        assert!(validate_speculation_server_config(SpeculationKind::NativeMtp, 1, true).is_ok());
        assert!(validate_speculation_server_config(SpeculationKind::NativeMtp, 2, true).is_ok());
        assert!(validate_speculation_server_config(
            SpeculationKind::NativeMtpNgram,
            1,
            true
        )
        .is_ok());
        assert!(validate_speculation_server_config(
            SpeculationKind::NativeMtpNgram,
            2,
            true
        )
        .is_ok());
        assert!(validate_speculation_server_config(SpeculationKind::HostProposer, 8, false).is_ok());
        assert!(validate_speculation_server_config(SpeculationKind::HostProposer, 8, true).is_ok());
        assert!(validate_speculation_server_config(SpeculationKind::HostProposer, 1, true).is_ok());
        assert!(validate_speculation_server_config(SpeculationKind::Off, 8, true).is_ok());
        assert!(validate_speculation_server_config(SpeculationKind::Off, 1, true).is_ok());
    }

    #[test]
    fn native_mtp_clips_budget_or_selects_serial_step() {
        assert_eq!(native_mtp_step_budget(3), Some(3));
        assert_eq!(native_mtp_step_budget(2), Some(2));
        assert_eq!(native_mtp_step_budget(0), None);
        assert_eq!(native_mtp_step_budget(1), None);
        assert_eq!(native_mtp_step_budget(4), None);
    }

    #[test]
    fn native_mtp_adapts_budget_to_measured_cycle_rate() {
        assert_eq!(native_mtp_adaptive_budget(3, 3, 0, None, None), Some(3));
        assert_eq!(native_mtp_adaptive_budget(3, 3, 1, None, None), Some(2));
        assert_eq!(native_mtp_adaptive_budget(3, 3, 4, Some(40.0), Some(30.0)), Some(2));
        assert_eq!(native_mtp_adaptive_budget(3, 3, 5, Some(30.0), Some(40.0)), Some(3));
        assert_eq!(native_mtp_adaptive_budget(3, 2, 5, Some(30.0), Some(40.0)), Some(2));
        assert_eq!(native_mtp_adaptive_budget(2, 3, 5, Some(30.0), Some(40.0)), Some(2));
    }

    #[test]
    fn native_mtp_rate_uses_exponential_average() {
        let mut rate = None;
        update_mtp_rate(&mut rate, 20.0);
        update_mtp_rate(&mut rate, 40.0);
        assert_eq!(rate, Some(25.0));
    }
}
