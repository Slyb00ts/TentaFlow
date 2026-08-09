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
use std::time::{Duration, Instant};

use forge_tokenize::{StopMatcher, StreamDecoder, Tokenizer};
use forge_types::{ForgeError, Result, Vendor};

use crate::generate::FinishReason;
use crate::kv::SeqKv;
use crate::metrics::{EngineMetrics, SeqTiming};
use crate::model::caps::hybrid_group_size;
use crate::model::{hybrid_prefill_b2_backend_capable, Model, PrefillProfile, MAX_PREFILL_CHUNK};
use crate::sample::{
    apply_logit_bias, apply_penalties, compute_logprob, suppress_eos, GpuSampler, Sampler,
    SamplingParams, SeqSampleParams, TokenLogprob,
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
    /// Emituj także ID tokenów, których fragment tekstu jest pusty.
    pub emit_empty_tokens: bool,
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
///
/// `tx` jest w `Option`, żeby `Drop` mógł ROZŁĄCZYĆ kanał przed dołączeniem
/// wątku — inaczej join czekałby na wątek, który nadal widzi żywego nadawcę.
#[derive(Clone)]
pub struct EngineHandle {
    tx: Option<mpsc::Sender<Submission>>,
    metrics: Arc<EngineMetrics>,
    worker: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl EngineHandle {
    /// Queue a request; events stream on the returned receiver.
    pub fn submit(&self, req: EngineRequest) -> Result<mpsc::Receiver<EngineEvent>> {
        let (etx, erx) = mpsc::channel();
        self.tx
            .as_ref()
            .ok_or_else(|| ForgeError::Scheduler("engine worker stopped".into()))?
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
    pub fn shutdown(mut self) -> Result<()> {
        if Arc::strong_count(&self.worker) != 1 {
            return Err(ForgeError::Scheduler(
                "shutdown wymaga ostatniego klonu EngineHandle".into(),
            ));
        }
        match self.stop() {
            Some(Ok(())) => Ok(()),
            Some(Err(_)) => Err(ForgeError::Scheduler(
                "worker zakończył się panic".into(),
            )),
            None => Err(ForgeError::Scheduler(
                "worker został już zatrzymany".into(),
            )),
        }
    }

    /// Rozłącza kanał i dołącza wątek roboczy, gdy to ostatni klon uchwytu.
    /// `None` oznacza, że nie było czego zatrzymywać (inny klon jeszcze żyje
    /// albo worker już stanął).
    fn stop(&mut self) -> Option<std::thread::Result<()>> {
        if Arc::strong_count(&self.worker) != 1 {
            return None;
        }
        self.tx = None;
        let handle = self.worker.lock().ok()?.take()?;
        Some(handle.join())
    }
}

impl Drop for EngineHandle {
    /// Wątek roboczy trzyma model i zasoby urządzenia, więc musi stanąć ZANIM
    /// proces zacznie je zwalniać. Bez tego wyjście przez błąd (albo dowolną
    /// inną ścieżkę, która pomija `shutdown`) zostawia go w trakcie pracy, a
    /// sterownik AMD wywraca wtedy stertę hosta przy rozbiórce procesu.
    fn drop(&mut self) {
        let _ = self.stop();
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
    emit_empty_tokens: bool,
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
        | (SpeculationKind::Off, _) => Ok(SpeculationKind::Off),
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

fn native_mtp_budget(available: usize, configured: usize) -> Option<usize> {
    let maximum = native_mtp_step_budget(available)?;
    native_mtp_step_budget(maximum.min(configured))
}

fn full_mtp_ngram_draft_fits(draft_len: usize, available: usize, configured: usize) -> bool {
    draft_len == configured
        && matches!(configured, 2 | 3)
        && native_mtp_budget(available, configured) == Some(configured)
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MtpNgramBatchMode {
    Auto,
    Off,
    ForceOn,
}

fn parse_mtp_ngram_batch(value: Option<&str>) -> Result<MtpNgramBatchMode> {
    match value {
        None => Ok(MtpNgramBatchMode::Auto),
        Some("0") => Ok(MtpNgramBatchMode::Off),
        Some("1") => Ok(MtpNgramBatchMode::ForceOn),
        Some(value) => Err(ForgeError::Scheduler(format!(
            "FORGE_MTP_NGRAM_BATCH wymaga wartości 0 lub 1, otrzymano {value:?}"
        ))),
    }
}

fn parse_mtp_ngram_mixed_batch(value: Option<&str>) -> Result<bool> {
    match value {
        None | Some("0") => Ok(false),
        Some("1") => Ok(true),
        Some(value) => Err(ForgeError::Scheduler(format!(
            "FORGE_MTP_NGRAM_MIXED_BATCH wymaga wartości 0 lub 1, otrzymano {value:?}"
        ))),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HybridPrefillBatchMode {
    Auto,
    Off,
    ForceOn,
}

fn parse_hybrid_prefill_batch(value: Option<&str>) -> Result<HybridPrefillBatchMode> {
    match value {
        None => Ok(HybridPrefillBatchMode::Auto),
        Some("0") => Ok(HybridPrefillBatchMode::Off),
        Some("1") => Ok(HybridPrefillBatchMode::ForceOn),
        Some(value) => Err(ForgeError::Scheduler(format!(
            "FORGE_HYBRID_PREFILL_BATCH wymaga wartości 0 lub 1, otrzymano {value:?}"
        ))),
    }
}

fn resolve_hybrid_prefill_batch(
    mode: HybridPrefillBatchMode,
    vendor: Vendor,
    warp_size: u32,
    model_capable: bool,
) -> Result<bool> {
    let backend_capable = hybrid_prefill_b2_backend_capable(vendor, warp_size);
    match mode {
        HybridPrefillBatchMode::Auto => Ok(backend_capable && model_capable),
        HybridPrefillBatchMode::Off => Ok(false),
        HybridPrefillBatchMode::ForceOn if backend_capable && model_capable => Ok(true),
        HybridPrefillBatchMode::ForceOn if !backend_capable => Err(ForgeError::Unsupported(
            format!(
                "FORGE_HYBRID_PREFILL_BATCH=1 wymaga zweryfikowanego backendu NVIDIA warp32, otrzymano {vendor:?} warp{warp_size}"
            ),
        )),
        HybridPrefillBatchMode::ForceOn => Err(ForgeError::Unsupported(
            "FORGE_HYBRID_PREFILL_BATCH=1 wymaga pełnego capability modelu i artefaktów prefill B2 T32".into(),
        )),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DensePrefillBatchMode {
    Auto,
    Off,
    ForceOn,
}

fn parse_dense_prefill_batch(value: Option<&str>) -> Result<DensePrefillBatchMode> {
    match value {
        None => Ok(DensePrefillBatchMode::Auto),
        Some("0") => Ok(DensePrefillBatchMode::Off),
        Some("1") => Ok(DensePrefillBatchMode::ForceOn),
        Some(value) => Err(ForgeError::Scheduler(format!(
            "FORGE_DENSE_PREFILL_BATCH wymaga wartości 0 lub 1, otrzymano {value:?}"
        ))),
    }
}

/// Wymagania sprzetowe rownego dense prefillu — te same, ktore sprawdza
/// `Kernels::dense_prefill_batch_capable`. O kompletnosc artefaktow pyta
/// osobno `rollout_capable`, wiec producent nie ma tu nic do rzeczy.
fn dense_prefill_auto_backend_capable(warp_size: u32, max_threads_per_block: u32) -> bool {
    warp_size == 32 && max_threads_per_block >= 256
}

fn resolve_dense_prefill_batch(
    mode: DensePrefillBatchMode,
    warp_size: u32,
    max_threads_per_block: u32,
    rollout_capable: bool,
) -> Result<bool> {
    let backend_capable = dense_prefill_auto_backend_capable(warp_size, max_threads_per_block);
    match mode {
        DensePrefillBatchMode::Auto => Ok(backend_capable && rollout_capable),
        DensePrefillBatchMode::Off => Ok(false),
        DensePrefillBatchMode::ForceOn if backend_capable && rollout_capable => Ok(true),
        DensePrefillBatchMode::ForceOn => Err(ForgeError::Unsupported(
            "FORGE_DENSE_PREFILL_BATCH=1 wymaga fali 32, limitu 256 wątków, dense F16 KV i pełnych artefaktów B4/B8/B16".into(),
        )),
    }
}

fn mtp_ngram_auto_backend(vendor: Vendor, warp_size: u32) -> bool {
    forge_types::nvidia_warp32(vendor, warp_size)
}

fn resolve_mtp_ngram_batch(
    mode: MtpNgramBatchMode,
    vendor: Vendor,
    warp_size: u32,
    model_capable: bool,
) -> Result<bool> {
    match mode {
        MtpNgramBatchMode::Auto => Ok(model_capable && mtp_ngram_auto_backend(vendor, warp_size)),
        MtpNgramBatchMode::Off => Ok(false),
        MtpNgramBatchMode::ForceOn if model_capable => Ok(true),
        MtpNgramBatchMode::ForceOn => Err(ForgeError::Unsupported(
            "FORGE_MTP_NGRAM_BATCH=1 wymaga strukturalnej obsługi MTP N/N B2 modelu".into(),
        )),
    }
}

fn mtp_ngram_batch_plan(budgets: &[Option<usize>]) -> (Vec<[usize; 2]>, Vec<usize>) {
    let mut pending: [Option<usize>; 2] = [None, None];
    let mut pairs = Vec::new();
    let mut singles = Vec::new();
    for (position, budget) in budgets.iter().copied().enumerate() {
        let Some(budget) = budget else {
            continue;
        };
        let Some(group) = budget.checked_sub(2).filter(|&group| group < pending.len()) else {
            singles.push(position);
            continue;
        };
        if let Some(first) = pending[group].take() {
            pairs.push([first, position]);
        } else {
            pending[group] = Some(position);
        }
    }
    singles.extend(pending.into_iter().flatten());
    singles.sort_unstable();
    (pairs, singles)
}

fn hybrid_prefill_batch_plan(pending_lengths: &[usize]) -> (Vec<[usize; 2]>, Vec<usize>) {
    const CHUNK: usize = 32;
    let mut pending = None;
    let mut pairs = Vec::new();
    let mut paired = vec![false; pending_lengths.len()];
    for (index, &length) in pending_lengths.iter().enumerate() {
        if length < CHUNK {
            continue;
        }
        if let Some(first) = pending.take() {
            pairs.push([first, index]);
            paired[first] = true;
            paired[index] = true;
        } else {
            pending = Some(index);
        }
    }
    let singles = paired
        .iter()
        .enumerate()
        .filter_map(|(index, &is_paired)| (!is_paired).then_some(index))
        .collect();
    (pairs, singles)
}

fn dense_prefill_batch_plan(
    pending: &[(usize, usize)],
    quantum: usize,
) -> Option<(Vec<usize>, usize, bool)> {
    for batch in [16usize, 8, 4] {
        let chunk = quantum.min(MAX_PREFILL_CHUNK / batch);
        if chunk == 0 {
            continue;
        }
        for final_chunk in [false, true] {
            let indices = pending
                .iter()
                .filter(|(_, length)| *length >= chunk && (*length == chunk) == final_chunk)
                .map(|&(index, _)| index)
                .take(batch)
                .collect::<Vec<_>>();
            if indices.len() == batch {
                return Some((indices, chunk, final_chunk));
            }
        }
    }
    None
}

fn dense_prefill_batch_route(enabled: bool, capable: bool) -> bool {
    enabled && capable
}

fn dense_prefill_scheduler_quantum(configured: usize, pending: usize) -> usize {
    for batch in [16usize, 8, 4] {
        if pending >= batch {
            return configured.min(MAX_PREFILL_CHUNK / batch);
        }
    }
    configured
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IdleCoalescingDecision {
    Dispatch,
    Wait(Duration),
}

fn idle_dense_coalescing_decision(
    elapsed: Duration,
    ready: usize,
    max_active: usize,
) -> IdleCoalescingDecision {
    const SINGLE_WINDOW: Duration = Duration::from_millis(1);
    const BURST_WINDOW: Duration = Duration::from_millis(5);

    if ready >= max_active {
        return IdleCoalescingDecision::Dispatch;
    }
    let deadline = if ready >= 2 {
        BURST_WINDOW
    } else {
        SINGLE_WINDOW
    };
    if elapsed >= deadline {
        IdleCoalescingDecision::Dispatch
    } else {
        IdleCoalescingDecision::Wait(deadline - elapsed)
    }
}

fn mtp_routed_pair_enabled(
    has_native_source: bool,
    native_mtp_b2: bool,
    mtp_ngram_mixed_batch: bool,
) -> bool {
    !has_native_source || (native_mtp_b2 && mtp_ngram_mixed_batch)
}

enum MtpRoutedSource {
    Native { observe_ngram: bool },
    Ngram(Vec<u32>),
}

struct MtpRoutedCandidate {
    index: usize,
    budget: usize,
    source: MtpRoutedSource,
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
        0,
        SpeculativeConfig::off(),
    )
}

/// Minimum decode concurrency before the batched forward path engages
/// (env override `FORGE_BATCH_MIN`). Measured on the RTX 4090 (serve
/// p1024/o128, isolated A/B 2026-07-24): with the NVFP4 small-batch decode
/// kernels the batched pass wins from 2 concurrent sequences (TPOT 11-14 ms
/// vs 28-58 ms serialized), while token-tile GEMM formats (Q4_K/Q8_0/f16) pay
/// a flat >=64-token tile per step that only amortizes at ~12 sequences
/// (Mistral Q4_K C=4: batched 46 ms vs 26 ms serialized).
fn default_batch_min_for(model: &Model) -> usize {
    std::env::var("FORGE_BATCH_MIN")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&v| v >= 2)
        .unwrap_or_else(|| {
            // Hybryda nie przechodzi przez token-tile GEMM, którego dotyczy próg
            // 12, a `small_batch_decode_capable` odrzuca ją zawsze (każda warstwa
            // DeltaNet), więc bez tego jej batch nie włączał się nigdy.
            if model.hybrid_batch_capable() || model.weights.small_batch_decode_capable() {
                2
            } else {
                12
            }
        })
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
    // 0 = auto: model-aware default (see `default_batch_min_for`).
    let batch_min = if batch_min == 0 {
        let resolved = default_batch_min_for(&model);
        tracing::info!(batch_min = resolved, "auto batch-min");
        resolved
    } else {
        batch_min
    };
    // Bez tego „batch nie wchodzi" i „batch nie pomaga" wyglądają identycznie:
    // płaska przepustowość zbiorcza przy rosnącej liczbie sekwencji.
    if model.is_hybrid() {
        let batched_decode = model.hybrid_batch_capable();
        tracing::info!(batched_decode, "hybrydowy batch dekodowania");
    }
    let native_mtp_b2 = parse_native_mtp_b2(std::env::var("FORGE_NATIVE_MTP_B2").ok().as_deref())?;
    let mtp_ngram_mode =
        parse_mtp_ngram_batch(std::env::var("FORGE_MTP_NGRAM_BATCH").ok().as_deref())?;
    let mtp_ngram_mixed_batch =
        parse_mtp_ngram_mixed_batch(std::env::var("FORGE_MTP_NGRAM_MIXED_BATCH").ok().as_deref())?;
    let hybrid_prefill_mode =
        parse_hybrid_prefill_batch(std::env::var("FORGE_HYBRID_PREFILL_BATCH").ok().as_deref())?;
    let dense_prefill_mode =
        parse_dense_prefill_batch(std::env::var("FORGE_DENSE_PREFILL_BATCH").ok().as_deref())?;
    let caps = model.device.caps();
    let dense_prefill_batch = resolve_dense_prefill_batch(
        dense_prefill_mode,
        caps.warp_size,
        caps.max_threads_per_block,
        model.dense_prefill_rollout_capable(),
    )?;
    let hybrid_prefill_capable = model.hybrid_prefill_b2_capable(32);
    let hybrid_prefill_batch = resolve_hybrid_prefill_batch(
        hybrid_prefill_mode,
        caps.vendor,
        caps.warp_size,
        hybrid_prefill_capable,
    )?;
    let mtp_ngram_batch = resolve_mtp_ngram_batch(
        mtp_ngram_mode,
        caps.vendor,
        caps.warp_size,
        model.mtp_ngram_b2_model_capable(),
    )?;
    if mtp_ngram_mixed_batch && !mtp_ngram_batch {
        return Err(ForgeError::Scheduler(
            "FORGE_MTP_NGRAM_MIXED_BATCH=1 wymaga aktywnego FORGE_MTP_NGRAM_BATCH".into(),
        ));
    }
    tracing::info!(
        mode = ?mtp_ngram_mode,
        enabled = mtp_ngram_batch,
        vendor = ?caps.vendor,
        warp_size = caps.warp_size,
        "rollout MTP+n-gram N/N B2"
    );
    tracing::info!(
        enabled = mtp_ngram_mixed_batch,
        "eksperymentalny rollout MTP+n-gram N/M i M/M B2"
    );
    tracing::info!(
        mode = ?hybrid_prefill_mode,
        enabled = hybrid_prefill_batch,
        vendor = ?caps.vendor,
        warp_size = caps.warp_size,
        capable = hybrid_prefill_capable,
        "eksperymentalny rollout hybrydowego prefill B2 T32"
    );
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
        if max_active >= 2 && spec.kind() == SpeculationKind::Off && model.hybrid_batch_capable()
        {
            model.ensure_batch(max_active)?;
        }
    }
    let (tx, rx) = mpsc::channel::<Submission>();
    let metrics = Arc::new(EngineMetrics::new());
    EngineMetrics::set(
        &metrics.hybrid_prefill_b2_scratch_bytes,
        model.debug_hybrid_prefill_b2_scratch_bytes() as u64,
    );
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
                mtp_ngram_batch,
                mtp_ngram_mixed_batch,
                hybrid_prefill_batch,
                dense_prefill_batch,
                &worker_metrics,
            )
        })
        .map_err(ForgeError::Io)?;
    Ok(EngineHandle {
        tx: Some(tx),
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
    mtp_ngram_batch: bool,
    mtp_ngram_mixed_batch: bool,
    hybrid_prefill_batch: bool,
    dense_prefill_batch: bool,
    metrics: &EngineMetrics,
) {
    let mut active: Vec<ActiveSeq<'t>> = Vec::new();
    let mut waiting: VecDeque<Submission> = VecDeque::new();
    // Mixed prefill+decode step (decode rows folded into the prefill chunk's
    // forward); `FORGE_MIXED_STEP=0` is the kill-switch.
    let mixed_step_enabled = std::env::var("FORGE_MIXED_STEP")
        .map(|v| v != "0")
        .unwrap_or(true);
    let mut hybrid_prefill_pair_cursor = 0usize;
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
        if active.is_empty() && waiting.is_empty() {
            let Ok(submission) = rx.recv() else {
                return;
            };
            waiting.push_back(submission);
            if dense_prefill_batch && max_active >= 4 {
                let started = Instant::now();
                loop {
                    let IdleCoalescingDecision::Wait(timeout) =
                        idle_dense_coalescing_decision(
                            started.elapsed(),
                            waiting.len(),
                            max_active,
                        )
                    else {
                        break;
                    };
                    match rx.recv_timeout(timeout) {
                        Ok(submission) => waiting.push_back(submission),
                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            }
        }
        while let Ok(submission) = rx.try_recv() {
            waiting.push_back(submission);
        }
        if active.is_empty() && waiting.is_empty() {
            return;
        }
        if terminate_poisoned_worker(model, &mut active, &mut waiting, rx, metrics) {
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
                AdmissionDisposition::Wait => {
                    unreachable!("wait nie jest wybierany przez admission")
                }
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
            let (spec_state, spec_kind, spec_budget) = if request_spec_kind != SpeculationKind::Off
            {
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
                emit_empty_tokens: sub.req.emit_empty_tokens,
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
            (model
                .kv
                .cfg
                .n_pages
                .saturating_sub(model.kv.free_page_count())) as u64,
        );

        // Cross-sequence tier balance: before the iteration touches the pool,
        // spill the globally coldest pages (across all active sequences) so
        // the upcoming prefill quanta and decode appends fit with the
        // watermark reserve intact — one long-context request no longer
        // stalls behind neighbors' cold history.
        if model.tier_enabled() {
            let page = model.kv.cfg.page_size;
            let has_live_decode = active
                .iter()
                .any(|a| !a.dead && a.pending_prompt.is_empty());
            let quantum = layer_major_scheduler_quantum(
                model.hybrid_layer_major_prefill_limit(),
                prefill_chunk,
                has_live_decode,
            );
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

        // Gotowe decode ma pierwszeństwo przed pracą prefill, aby długi prompt
        // nie zwiększał ITL już generowanych sekwencji. Jeśli czeka prefill,
        // krok decode może pojechać w jego chunku (mixed step).
        let mixed_prefill = if mixed_step_enabled {
            active
                .iter()
                .position(|a| {
                    !a.dead
                        && !a.pending_prompt.is_empty()
                        && matches!(a.sampler, SeqSampler::Gpu(_))
                })
                .map(|pidx| {
                    let quantum = layer_major_scheduler_quantum(
                        model.hybrid_layer_major_prefill_limit(),
                        prefill_chunk,
                        true,
                    );
                    (pidx, quantum)
                })
        } else {
            None
        };
        let mixed_done = batch_gpu_decode(
            model,
            &mut active,
            batch_min,
            native_mtp_b2,
            mtp_ngram_batch,
            mtp_ngram_mixed_batch,
            mixed_prefill,
            metrics,
        );
        for a in active.iter_mut() {
            if a.dead || !a.pending_prompt.is_empty() || matches!(a.sampler, SeqSampler::Gpu(_)) {
                continue;
            }
            if let Err(e) = advance(model, a, prefill_chunk, false, metrics) {
                let _ = a.events.send(EngineEvent::Error(e.to_string()));
                a.dead = true;
            }
        }

        let mut prefilled_b2 = None;
        let mut deferred_b2 = None;
        if hybrid_prefill_batch {
            let pending_indices = active
                .iter()
                .enumerate()
                .filter(|(_, a)| !a.dead && !a.pending_prompt.is_empty())
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            let pending = pending_indices
                .iter()
                .map(|&index| active[index].pending_prompt.len())
                .collect::<Vec<_>>();
            let (pairs, singles) = hybrid_prefill_batch_plan(&pending);
            tracing::trace!(?pairs, ?singles, "plan hybrydowego prefill B2 T32");
            if model.hybrid_prefill_b2_capable(32) && !pairs.is_empty() {
                deferred_b2 = Some(Vec::with_capacity(pairs.len().saturating_sub(1) * 2));
                let selected = hybrid_prefill_pair_cursor % pairs.len();
                hybrid_prefill_pair_cursor = hybrid_prefill_pair_cursor.wrapping_add(1);
                for (pair_index, [first, second]) in pairs.into_iter().enumerate() {
                    let indices = [pending_indices[first], pending_indices[second]];
                    if pair_index == selected {
                        advance_hybrid_prefill_b2(model, &mut active, indices, metrics);
                        prefilled_b2 = Some(indices);
                    } else {
                        deferred_b2
                            .as_mut()
                            .expect("lista odroczonych par jest gotowa")
                            .extend(indices);
                    }
                }
            } else {
                EngineMetrics::add(
                    &metrics.hybrid_prefill_b2_fallbacks_total,
                    pairs.len() as u64,
                );
            }
        }

        // One scheduler iteration. Prefilling sequences and any CPU-sampled
        // decode sequence advance individually (chunked prefill / one token);
        // every GPU-sampled decode sequence runs through ONE batched forward
        // pass (`batch_gpu_decode`) — the continuous-batching throughput path.
        let has_live_decode = active
            .iter()
            .any(|a| !a.dead && a.pending_prompt.is_empty());
        let quantum = layer_major_scheduler_quantum(
            model.hybrid_layer_major_prefill_limit(),
            prefill_chunk,
            has_live_decode,
        );
        let dense_pending = active
            .iter()
            .enumerate()
            .filter(|(index, active)| {
                !active.dead
                    && !active.pending_prompt.is_empty()
                    && matches!(active.sampler, SeqSampler::Gpu(_))
                    && !prefilled_b2.is_some_and(|pair| pair.contains(index))
                    && !deferred_b2
                        .as_ref()
                        .is_some_and(|indices| indices.contains(index))
            })
            .map(|(index, active)| (index, active.pending_prompt.len()))
            .collect::<Vec<_>>();
        let mut prefilled_dense = Vec::new();
        // Grouped prefill only while nothing is decoding (cold burst): a
        // group's prompts all finish at the group's tail, so with live decode
        // the FIFO serial path below wins median TTFT (182 vs 416 ms at C=8,
        // p1024), while a cold burst still gets the batched pass's throughput.
        if dense_prefill_batch && !has_live_decode {
            let dense_quantum = dense_prefill_scheduler_quantum(quantum, dense_pending.len());
            if let Some((indices, chunk, final_chunk)) =
                dense_prefill_batch_plan(&dense_pending, dense_quantum)
            {
                if dense_prefill_batch_route(
                    dense_prefill_batch,
                    model.dense_prefill_batch_capable(indices.len(), chunk),
                ) {
                    advance_dense_prefill_batch(model, &mut active, &indices, chunk, final_chunk);
                    if terminate_poisoned_worker(model, &mut active, &mut waiting, rx, metrics) {
                        return;
                    }
                    prefilled_dense = indices;
                }
            }
        }
        // FIFO: at most ONE pending sequence advances a serial prefill quantum
        // per iteration (`active` keeps arrival order). A burst of new prompts
        // used to stack one chunk PER sequence between decode steps, so at
        // C=16 a decode step waited for ~16 chunks (~800 ms ITL spike) and
        // every prompt's TTFT degenerated to the burst's tail. The batched
        // dense path above still takes whole groups when they align.
        let mut advanced_serial = mixed_done;
        for (index, a) in active.iter_mut().enumerate() {
            if a.dead || advanced_serial {
                continue;
            }
            if prefilled_b2.is_some_and(|pair| pair.contains(&index))
                || deferred_b2
                    .as_ref()
                    .is_some_and(|indices| indices.contains(&index))
                || prefilled_dense.contains(&index)
                || a.pending_prompt.is_empty()
            {
                continue;
            }
            if let Err(e) = advance(model, a, prefill_chunk, has_live_decode, metrics) {
                let _ = a.events.send(EngineEvent::Error(e.to_string()));
                a.dead = true;
            }
            advanced_serial = true;
        }
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

fn terminate_poisoned_worker<'t>(
    model: &Model,
    active: &mut Vec<ActiveSeq<'t>>,
    waiting: &mut VecDeque<Submission>,
    rx: &mpsc::Receiver<Submission>,
    metrics: &EngineMetrics,
) -> bool {
    let Some(reason) = model.kv_reuse_poison_reason() else {
        return false;
    };
    drain_poisoned_worker(reason, active, waiting, rx, metrics);
    true
}

fn drain_poisoned_worker<'t>(
    reason: &str,
    active: &mut Vec<ActiveSeq<'t>>,
    waiting: &mut VecDeque<Submission>,
    rx: &mpsc::Receiver<Submission>,
    metrics: &EngineMetrics,
) {
    let message = format!("engine zatrzymany po fatalnym błędzie synchronizacji KV: {reason}");
    tracing::error!(
        active = active.len(),
        waiting = waiting.len(),
        "zatrzymanie workera i kwarantanna wszystkich sekwencji: {reason}"
    );
    for sequence in active.drain(..) {
        let _ = sequence.events.send(EngineEvent::Error(message.clone()));
        EngineMetrics::inc(&metrics.requests_errored);
    }
    for submission in waiting.drain(..) {
        let _ = submission.events.send(EngineEvent::Error(message.clone()));
        EngineMetrics::inc(&metrics.requests_errored);
    }
    while let Ok(submission) = rx.try_recv() {
        let _ = submission.events.send(EngineEvent::Error(message.clone()));
        EngineMetrics::inc(&metrics.requests_errored);
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
    if (a.emit_empty_tokens || !emit_text.is_empty() || logprob.is_some())
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

fn finish_hybrid_prefill_b2_lane(
    model: &mut Model,
    active: &mut ActiveSeq<'_>,
    logits: Vec<f32>,
    lane: usize,
) -> Result<()> {
    if !active.pending_prompt.is_empty() {
        return Ok(());
    }
    active.next = Some(match &mut active.sampler {
        SeqSampler::Cpu(_) => PendingNext::Logits(logits),
        SeqSampler::Gpu(sampler) => {
            PendingNext::Token(model.sample_hybrid_prefill_b2_logits(lane, sampler)?)
        }
    });
    Ok(())
}

fn advance_hybrid_prefill_b2(
    model: &mut Model,
    active: &mut [ActiveSeq<'_>],
    indices: [usize; 2],
    metrics: &EngineMetrics,
) {
    const STEPS: usize = 32;
    let [first_index, second_index] = indices;
    let (left, right) = active.split_at_mut(second_index);
    let first = &mut left[first_index];
    let second = &mut right[0];
    let chunks = [
        first
            .pending_prompt
            .iter()
            .take(STEPS)
            .copied()
            .collect::<Vec<_>>(),
        second
            .pending_prompt
            .iter()
            .take(STEPS)
            .copied()
            .collect::<Vec<_>>(),
    ];
    if chunks.iter().any(|chunk| chunk.len() != STEPS) {
        EngineMetrics::inc(&metrics.hybrid_prefill_b2_fallbacks_total);
        return;
    }
    let reset_mtp = [first.seq.len == 0, second.seq.len == 0];
    let result = (|| {
        let gpu_pair = matches!(first.sampler, SeqSampler::Gpu(_))
            && matches!(second.sampler, SeqSampler::Gpu(_));
        if gpu_pair {
            let final_lanes = [
                first.pending_prompt.len() == STEPS,
                second.pending_prompt.len() == STEPS,
            ];
            model.hybrid_prefill_b2_t32_device(
                &mut [&mut first.seq, &mut second.seq],
                [&chunks[0], &chunks[1]],
            )?;
            model.hybrid_prefill_mtp_catchup_b2(
                &mut [&mut first.seq, &mut second.seq],
                [&chunks[0], &chunks[1]],
                reset_mtp,
            )?;
            let mut ids = [None, None];
            match final_lanes {
                [true, true] => {
                    let sampled = match (&mut first.sampler, &mut second.sampler) {
                        (SeqSampler::Gpu(first_sampler), SeqSampler::Gpu(second_sampler)) => model
                            .sample_hybrid_prefill_b2_logits_batched(&mut [
                                first_sampler,
                                second_sampler,
                            ])?,
                        _ => unreachable!("gpu_pair sprawdził oba samplery"),
                    };
                    ids = sampled.map(Some);
                }
                [true, false] => {
                    let SeqSampler::Gpu(sampler) = &mut first.sampler else {
                        unreachable!("gpu_pair sprawdził sampler lane 0")
                    };
                    ids[0] = Some(model.sample_hybrid_prefill_b2_logits(0, sampler)?);
                }
                [false, true] => {
                    let SeqSampler::Gpu(sampler) = &mut second.sampler else {
                        unreachable!("gpu_pair sprawdził sampler lane 1")
                    };
                    ids[1] = Some(model.sample_hybrid_prefill_b2_logits(1, sampler)?);
                }
                [false, false] => {}
            };
            first.pending_prompt.drain(..STEPS);
            second.pending_prompt.drain(..STEPS);
            if let Some(id) = ids[0] {
                first.next = Some(PendingNext::Token(id));
            }
            if let Some(id) = ids[1] {
                second.next = Some(PendingNext::Token(id));
            }
        } else {
            let [first_logits, second_logits] = model.hybrid_prefill_b2_t32(
                &mut [&mut first.seq, &mut second.seq],
                [&chunks[0], &chunks[1]],
            )?;
            model.hybrid_prefill_mtp_catchup_b2(
                &mut [&mut first.seq, &mut second.seq],
                [&chunks[0], &chunks[1]],
                reset_mtp,
            )?;
            first.pending_prompt.drain(..STEPS);
            second.pending_prompt.drain(..STEPS);
            finish_hybrid_prefill_b2_lane(model, first, first_logits, 0)?;
            finish_hybrid_prefill_b2_lane(model, second, second_logits, 1)?;
        }
        Ok::<(), ForgeError>(())
    })();
    EngineMetrics::set(
        &metrics.hybrid_prefill_b2_scratch_bytes,
        model.debug_hybrid_prefill_b2_scratch_bytes() as u64,
    );
    match result {
        Ok(()) => {
            EngineMetrics::inc(&metrics.hybrid_prefill_b2_steps_total);
            EngineMetrics::add(&metrics.hybrid_prefill_b2_tokens_total, (2 * STEPS) as u64);
        }
        Err(error) => {
            let message = error.to_string();
            let _ = first.events.send(EngineEvent::Error(message.clone()));
            let _ = second.events.send(EngineEvent::Error(message));
            first.dead = true;
            second.dead = true;
        }
    }
}

fn advance_dense_prefill_batch(
    model: &mut Model,
    active: &mut [ActiveSeq<'_>],
    indices: &[usize],
    chunk_size: usize,
    final_chunk: bool,
) {
    let mut lanes = active
        .iter_mut()
        .enumerate()
        .filter(|(index, _)| indices.contains(index))
        .map(|(_, active)| active)
        .collect::<Vec<_>>();
    let chunks = lanes
        .iter()
        .map(|active| {
            active
                .pending_prompt
                .iter()
                .take(chunk_size)
                .copied()
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    if chunks.iter().any(|chunk| chunk.len() != chunk_size) {
        return;
    }
    let result = (|| {
        let token_lanes = chunks.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let mut seqs = lanes
            .iter_mut()
            .map(|active| &mut active.seq)
            .collect::<Vec<_>>();
        let sampled = if final_chunk {
            model.prefill_batch_device_logits(&mut seqs, &token_lanes)?;
            drop(seqs);
            let mut samplers = lanes
                .iter_mut()
                .map(|active| match &mut active.sampler {
                    SeqSampler::Gpu(sampler) => Ok(sampler),
                    SeqSampler::Cpu(_) => Err(ForgeError::Scheduler(
                        "dense batch prefill wymaga samplera GPU".into(),
                    )),
                })
                .collect::<Result<Vec<_>>>()?;
            Some(model.sample_prefill_batch_logits(&mut samplers)?)
        } else {
            model.prefill_batch_device_sync(&mut seqs, &token_lanes)?;
            None
        };
        for (lane, active) in lanes.iter_mut().enumerate() {
            active.pending_prompt.drain(..chunk_size);
            if let Some(ids) = &sampled {
                active.next = Some(PendingNext::Token(ids[lane]));
            }
        }
        Ok::<(), ForgeError>(())
    })();
    if let Err(error) = result {
        let message = error.to_string();
        for active in lanes {
            let _ = active.events.send(EngineEvent::Error(message.clone()));
            active.dead = true;
        }
    }
}

/// Advance one sequence by one scheduler quantum (prefill chunk or a single
/// CPU-sampled decode step). GPU-sampled decode goes through the batched path.
fn advance(
    model: &mut Model,
    a: &mut ActiveSeq<'_>,
    prefill_chunk: usize,
    has_competing_decode: bool,
    metrics: &EngineMetrics,
) -> Result<()> {
    if !a.pending_prompt.is_empty() {
        // Chunked prefill: one quantum per iteration keeps decode ITL of the
        // other sequences bounded; the 32-token floor keeps tiny configured
        // quanta from wasting the batched kernels.
        let quantum = layer_major_scheduler_quantum(
            model.hybrid_layer_major_prefill_limit(),
            prefill_chunk,
            has_competing_decode,
        );
        let take = quantum.min(a.pending_prompt.len());
        let chunk: Vec<u32> = a.pending_prompt.drain(..take).collect();
        let final_chunk = a.pending_prompt.is_empty();
        let device_logits = matches!(a.sampler, SeqSampler::Gpu(_)) && !model.is_hybrid();
        let logits = if device_logits {
            if final_chunk {
                model.prefill_chunk_device_logits(&mut a.seq, &chunk)?;
            } else {
                model.prefill_chunk_device_sync(&mut a.seq, &chunk)?;
            }
            None
        } else {
            Some(model.prefill_chunk(&mut a.seq, &chunk)?)
        };
        if final_chunk {
            a.next = Some(match &mut a.sampler {
                SeqSampler::Cpu(_) => {
                    PendingNext::Logits(logits.expect("CPU sampler wymaga hostowych logits"))
                }
                // The prefill logits are still device-resident: draw now,
                // before another sequence overwrites the shared buffer.
                SeqSampler::Gpu(g) => PendingNext::Token(model.sample_last_logits(g)?),
            });
            a.prefill_profile = model.take_prefill_profile()?;
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

fn layer_major_scheduler_quantum(
    layer_major_limit: Option<usize>,
    configured_prefill: usize,
    has_competing_decode: bool,
) -> usize {
    layer_major_limit
        .map(|limit| {
            if has_competing_decode {
                limit.min(MAX_PREFILL_CHUNK)
            } else {
                limit
            }
        })
        .unwrap_or_else(|| configured_prefill.clamp(32, MAX_PREFILL_CHUNK))
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
    mtp_ngram_batch: bool,
    mtp_ngram_mixed_batch: bool,
    mixed_prefill: Option<(usize, usize)>,
    metrics: &EngineMetrics,
) -> bool {
    // Phase 1: emit each ready sequence's pending token; collect the indices of
    // survivors that still need to be fed one more token.
    let mut feed_idx: Vec<usize> = Vec::new();
    let mut mtp_idx: Vec<usize> = Vec::new();
    let mut mtp_ngram_idx: Vec<usize> = Vec::new();
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
                let _ = a.events.send(EngineEvent::Error(
                    "GPU sequence carried host logits".into(),
                ));
                a.dead = true;
                continue;
            }
            None => continue,
        };
        match emit_token(a, next, None, metrics) {
            // A speculative sequence runs its own draft/verify step here and
            // never joins the batched (or serial fallback) feed set — the n-gram
            // path is a single-sequence latency win, disabled under batch load.
            Ok(StepOutcome::Continue) => match a.spec_kind {
                SpeculationKind::HostProposer => speculative_step(model, a, metrics),
                SpeculationKind::NativeMtp => mtp_idx.push(i),
                SpeculationKind::NativeMtpNgram => {
                    if mtp_ngram_batch {
                        mtp_ngram_idx.push(i);
                    } else {
                        native_mtp_ngram_step(model, a, metrics)
                    }
                }
                SpeculationKind::Off => feed_idx.push(i),
            },
            Ok(StepOutcome::Finished) => {}
            Err(e) => {
                let _ = a.events.send(EngineEvent::Error(e.to_string()));
                a.dead = true;
            }
        }
    }
    if mtp_ngram_batch && (!mtp_idx.is_empty() || !mtp_ngram_idx.is_empty()) {
        let routed_mtp_indices = if mtp_ngram_mixed_batch {
            mtp_idx.as_slice()
        } else {
            &[]
        };
        batch_native_mtp_routed_decode(
            model,
            active,
            routed_mtp_indices,
            &mtp_ngram_idx,
            native_mtp_b2,
            mtp_ngram_mixed_batch,
            metrics,
        );
    } else {
        for index in mtp_ngram_idx {
            native_mtp_ngram_step(model, &mut active[index], metrics);
        }
    }
    if (!mtp_ngram_batch || !mtp_ngram_mixed_batch) && native_mtp_b2 && !mtp_idx.is_empty() {
        batch_native_mtp_decode(model, active, &mtp_idx, metrics);
    } else if !mtp_ngram_batch || !mtp_ngram_mixed_batch {
        for index in mtp_idx {
            native_mtp_step(model, &mut active[index], metrics);
        }
    }
    if feed_idx.is_empty() {
        return false;
    }

    let hybrid_batch = model.hybrid_batch_capable() && feed_idx.len() >= 2;
    let serial_only = model.weights.is_moe()
        || model.kv.cfg.quant.is_rot()
        || (model.is_hybrid() && !hybrid_batch);
    // Mixed step: fold this decode step into the oldest pending sequence's
    // prefill chunk — the decode rows ride the chunk's GEMMs, so a long
    // prompt costs the decoding sequences (almost) no ITL. Falls back to the
    // plain paths when the model or the chunk cannot take it.
    if let Some((pidx, quantum)) = mixed_prefill {
        if !serial_only
            && !feed_idx.contains(&pidx)
            && model.mixed_step_capable(feed_idx.len())
            && mixed_gpu_group(model, active, &feed_idx, pidx, quantum)
        {
            return true;
        }
    }

    // Below `batch_min` sequences, the tuned fused single-seq decode path
    // (6 launches/layer, graphed) run once per survivor is faster than the
    // tensor-core batched pass, whose per-step cost is nearly flat in the batch
    // size (the GEMMs process a fixed token tile). Serializing them here keeps
    // single-stream and low-concurrency latency from regressing; the batched
    // path engages once the flat cost amortizes across enough sequences.
    // MoE i rot utrzymują wiele aktywnych sekwencji, ale dekodują je pojedynczo,
    // ponieważ ich stan nie ma jeszcze batchowego kernela forward.
    if (!hybrid_batch && feed_idx.len() < batch_min.max(2)) || serial_only {
        for &i in &feed_idx {
            serial_step(model, &mut active[i]);
        }
        return false;
    }

    if hybrid_batch {
        // Grupy zamiast par: mixery i tak są serialne per lane, ale norm, FFN i
        // głowa logitów płacą wtedy raz za odczyt wag zamiast raz na parę.
        // Szerokość grupy musi trafiać w kernel — patrz `hybrid_group_size`.
        let mut rest = feed_idx.as_slice();
        while rest.len() >= 2 {
            let take = hybrid_group_size(rest.len());
            decode_gpu_group(model, active, &rest[..take]);
            EngineMetrics::inc(&metrics.hybrid_decode_batch_steps_total);
            EngineMetrics::add(&metrics.hybrid_decode_batch_lanes_total, take as u64);
            rest = &rest[take..];
        }
        if let Some(&tail) = rest.first() {
            serial_step(model, &mut active[tail]);
        }
        return false;
    }

    decode_gpu_group(model, active, &feed_idx);
    false
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
        match native_mtp_budget(available, a.spec_budget) {
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
            let capable =
                model.native_mtp_b2_capable([&active[first].seq, &active[second].seq], budget);
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

fn batch_native_mtp_routed_decode(
    model: &mut Model,
    active: &mut [ActiveSeq<'_>],
    mtp_indices: &[usize],
    ngram_indices: &[usize],
    native_mtp_b2: bool,
    mtp_ngram_mixed_batch: bool,
    metrics: &EngineMetrics,
) {
    let mut prepared = Vec::with_capacity(mtp_indices.len() + ngram_indices.len());
    for &index in mtp_indices {
        let a = &active[index];
        let available = model.native_mtp_available_budget(&a.seq, a.spec_budget);
        if let Some(budget) = native_mtp_budget(available, a.spec_budget) {
            prepared.push(MtpRoutedCandidate {
                index,
                budget,
                source: MtpRoutedSource::Native {
                    observe_ngram: false,
                },
            });
        } else {
            serial_step(model, &mut active[index]);
        }
    }
    for &index in ngram_indices {
        let a = &mut active[index];
        let fed = *a.generated.last().expect("emit_token dodał fed routera");
        let configured_budget = a.spec_budget;
        let available = model.native_mtp_available_budget(&a.seq, configured_budget);
        let draft = {
            let state = a.spec.as_mut().expect("router MTP+n-gram ma stan hostowy");
            state.observe(fed);
            match state.draft(configured_budget) {
                Ok(draft) => draft,
                Err(error) => {
                    let _ = a.events.send(EngineEvent::Error(error.to_string()));
                    a.dead = true;
                    continue;
                }
            }
        };
        if full_mtp_ngram_draft_fits(draft.len(), available, configured_budget) {
            prepared.push(MtpRoutedCandidate {
                index,
                budget: configured_budget,
                source: MtpRoutedSource::Ngram(draft),
            });
            continue;
        }
        if let Some(state) = &mut a.spec {
            state.cancel_draft();
        }
        if let Some(budget) = native_mtp_budget(available, configured_budget) {
            a.mtp_fallback_forwards += 1;
            prepared.push(MtpRoutedCandidate {
                index,
                budget,
                source: MtpRoutedSource::Native {
                    observe_ngram: true,
                },
            });
        } else {
            serial_step(model, a);
        }
    }

    prepared.sort_by_key(|candidate| candidate.index);
    let budgets = prepared
        .iter()
        .map(|candidate| Some(candidate.budget))
        .collect::<Vec<_>>();
    let (pairs, singles) = mtp_ngram_batch_plan(&budgets);
    for [first_position, second_position] in pairs {
        let first = &prepared[first_position];
        let second = &prepared[second_position];
        let has_native_source = matches!(first.source, MtpRoutedSource::Native { .. })
            || matches!(second.source, MtpRoutedSource::Native { .. });
        let capable = model.mtp_ngram_b2_capable(
            [&active[first.index].seq, &active[second.index].seq],
            first.budget,
        );
        if !capable
            || !mtp_routed_pair_enabled(has_native_source, native_mtp_b2, mtp_ngram_mixed_batch)
        {
            run_mtp_routed_b1(model, active, first, metrics);
            run_mtp_routed_b1(model, active, second, metrics);
            continue;
        }
        native_mtp_routed_step_b2_prepared(model, active, [first, second], metrics);
    }
    for position in singles {
        run_mtp_routed_b1(model, active, &prepared[position], metrics);
    }
}

fn run_mtp_routed_b1(
    model: &mut Model,
    active: &mut [ActiveSeq<'_>],
    candidate: &MtpRoutedCandidate,
    metrics: &EngineMetrics,
) {
    match &candidate.source {
        MtpRoutedSource::Ngram(draft) => {
            verify_native_mtp_ngram_prepared(model, &mut active[candidate.index], draft, metrics)
        }
        MtpRoutedSource::Native { observe_ngram } => {
            let generated_before = active[candidate.index].generated.len();
            native_mtp_step(model, &mut active[candidate.index], metrics);
            if *observe_ngram {
                let accepted = active[candidate.index].generated[generated_before..].to_vec();
                if let Some(state) = &mut active[candidate.index].spec {
                    state.observe_all(&accepted);
                }
            }
        }
    }
}

/// One mixed forward: the feed set's decode tokens ride the prefill chunk of
/// `active[pidx]` (see `Model::mixed_prefill_decode_step`). Returns `false`
/// without touching any state when the chunk cannot be taken; on a model
/// error every involved sequence is failed (the chunk is already consumed).
fn mixed_gpu_group(
    model: &mut Model,
    active: &mut [ActiveSeq<'_>],
    feed_idx: &[usize],
    pidx: usize,
    quantum: usize,
) -> bool {
    let b = feed_idx.len();
    let pending = active[pidx].pending_prompt.len();
    let cap = quantum.min(MAX_PREFILL_CHUNK);
    // Wiersze decode liczą się do kwantu, ale skracanie go ma sens TYLKO wtedy,
    // gdy prompt i tak nie zmieści się w jednym chunku — wtedy nie dokłada
    // granicy. Jeśli resztka promptu mieści się w kwancie, skrócenie wymusza
    // DODATKOWY chunk, czyli kolejne pełne przejście po wagach (~11 ms na
    // Bieliku) zamiast jednego kafla GEMM więcej (~6% jednego przejścia).
    // Zmierzone: polityka skracająca dawała 780 tok/s przy p1024, bez niej 824.
    let take = if pending + b <= cap {
        pending
    } else {
        cap.saturating_sub(b).min(pending)
    };
    if take == 0 {
        return false;
    }
    let vocab = model.weights.descriptor.params.vocab_size;

    let mut seqs: Vec<&mut SeqKv> = Vec::with_capacity(b);
    let mut tokens: Vec<u32> = Vec::with_capacity(b);
    let mut params: Vec<SeqSampleParams> = Vec::with_capacity(b);
    let mut prefill: Option<&mut ActiveSeq<'_>> = None;
    let mut fi = 0usize;
    for (i, a) in active.iter_mut().enumerate() {
        if i == pidx {
            prefill = Some(a);
            continue;
        }
        if fi >= feed_idx.len() || feed_idx[fi] != i {
            continue;
        }
        fi += 1;
        let fed = *a.generated.last().expect("emit_token pushed the fed token");
        let SeqSampler::Gpu(g) = &mut a.sampler else {
            unreachable!("feed set is GPU-only")
        };
        g.note_token(fed);
        params.push(g.batch_params(vocab));
        tokens.push(fed);
        seqs.push(&mut a.seq);
    }
    let pf = prefill.expect("pending prefill index in bounds");
    let chunk: Vec<u32> = pf.pending_prompt.drain(..take).collect();
    let final_chunk = pf.pending_prompt.is_empty();
    let final_params = if final_chunk {
        let SeqSampler::Gpu(g) = &mut pf.sampler else {
            // CPU-sampled prompt needs host logits — not producible here;
            // restore the chunk and let the serial path prefill it.
            for &token in chunk.iter().rev() {
                pf.pending_prompt.push_front(token);
            }
            return false;
        };
        Some(g.batch_params(vocab))
    } else {
        None
    };

    let result =
        model.mixed_prefill_decode_step(&mut seqs, &tokens, &params, &mut pf.seq, &chunk, final_params);
    drop(seqs);
    let pf_result = match &result {
        Ok((_, final_id)) => Some(*final_id),
        Err(_) => None,
    };
    match result {
        Ok((next_ids, _)) => {
            let mut ri = 0usize;
            for (i, a) in active.iter_mut().enumerate() {
                if i == pidx {
                    if let Some(Some(id)) = pf_result {
                        a.next = Some(PendingNext::Token(id));
                        a.prefill_profile = match model.take_prefill_profile() {
                            Ok(profile) => profile,
                            Err(_) => None,
                        };
                    }
                    continue;
                }
                if ri < feed_idx.len() && feed_idx[ri] == i {
                    a.next = Some(PendingNext::Token(next_ids[ri]));
                    ri += 1;
                }
            }
            tracing::trace!(b, take, final_chunk, "mixed prefill+decode step");
            true
        }
        Err(e) => {
            let msg = e.to_string();
            let mut ri = 0usize;
            for (i, a) in active.iter_mut().enumerate() {
                if i == pidx || (ri < feed_idx.len() && feed_idx[ri] == i) {
                    if i != pidx {
                        ri += 1;
                    }
                    let _ = a.events.send(EngineEvent::Error(msg.clone()));
                    a.dead = true;
                }
            }
            true
        }
    }
}

fn decode_gpu_group(model: &mut Model, active: &mut [ActiveSeq<'_>], feed_idx: &[usize]) {
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
        let s = a
            .spec
            .as_mut()
            .expect("speculative_step on a spec sequence");
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
fn finish_native_mtp_ngram_verified(
    a: &mut ActiveSeq<'_>,
    draft: &[u32],
    accepted: usize,
    correction: u32,
    metrics: &EngineMetrics,
) {
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

fn verify_native_mtp_ngram_prepared(
    model: &mut Model,
    a: &mut ActiveSeq<'_>,
    draft: &[u32],
    metrics: &EngineMetrics,
) {
    let fed = *a.generated.last().expect("emit_token dodał fed routera");
    let SeqSampler::Gpu(sampler) = &mut a.sampler else {
        unreachable!("router MTP+n-gram wymaga samplera GPU")
    };
    sampler.note_token(fed);
    if let Err(error) = a
        .spec
        .as_ref()
        .expect("router MTP+n-gram ma stan hostowy")
        .validate_commit(draft, 0)
    {
        if let Some(state) = &mut a.spec {
            state.cancel_draft();
        }
        let _ = a.events.send(EngineEvent::Error(error.to_string()));
        a.dead = true;
        return;
    }
    let (accepted, correction) =
        match model.verify_greedy_draft_with_mtp_catchup(&mut a.seq, fed, draft) {
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
    a.spec
        .as_mut()
        .expect("router MTP+n-gram ma stan hostowy")
        .commit_validated(draft, accepted);
    finish_native_mtp_ngram_verified(a, draft, accepted, correction, metrics);
}

fn native_mtp_routed_step_b2_prepared(
    model: &mut Model,
    active: &mut [ActiveSeq<'_>],
    candidates: [&MtpRoutedCandidate; 2],
    metrics: &EngineMetrics,
) {
    let indices = [candidates[0].index, candidates[1].index];
    let [first_index, second_index] = indices;
    if first_index >= second_index || second_index >= active.len() {
        let error = ForgeError::Scheduler(
            "MTP+n-gram B2 wymaga dwóch rosnących, różnych indeksów sekwencji".into(),
        );
        let active_len = active.len();
        for index in indices.into_iter().filter(|&index| index < active_len) {
            let a = &mut active[index];
            if let Some(state) = &mut a.spec {
                state.cancel_draft();
            }
            let _ = a.events.send(EngineEvent::Error(error.to_string()));
            a.dead = true;
        }
        return;
    }
    let (left, right) = active.split_at_mut(second_index);
    let first = &mut left[first_index];
    let second = &mut right[0];
    let fed = [
        *first.generated.last().expect("emit_token dodał fed lane0"),
        *second.generated.last().expect("emit_token dodał fed lane1"),
    ];
    let drafts = candidates.map(|candidate| match &candidate.source {
        MtpRoutedSource::Native { .. } => None,
        MtpRoutedSource::Ngram(draft) => Some(draft.as_slice()),
    });
    let validation = drafts
        .iter()
        .enumerate()
        .filter_map(|(lane, draft)| draft.map(|draft| (lane, draft)))
        .try_for_each(|(lane, draft)| {
            let a = if lane == 0 { &*first } else { &*second };
            a.spec
                .as_ref()
                .expect("lane N routera ma stan")
                .validate_commit(draft, 0)
        });
    if let Err(error) = validation {
        for (lane, a) in [first, second].into_iter().enumerate() {
            if drafts[lane].is_some() {
                if let Some(state) = &mut a.spec {
                    state.cancel_draft();
                }
            }
            let _ = a.events.send(EngineEvent::Error(error.to_string()));
            a.dead = true;
        }
        return;
    }
    let SeqSampler::Gpu(first_sampler) = &mut first.sampler else {
        unreachable!("MTP+n-gram B2 wymaga samplera GPU")
    };
    first_sampler.note_token(fed[0]);
    let SeqSampler::Gpu(second_sampler) = &mut second.sampler else {
        unreachable!("MTP+n-gram B2 wymaga samplera GPU")
    };
    second_sampler.note_token(fed[1]);
    let result = model.native_mtp_routed_step_b2(
        &mut [&mut first.seq, &mut second.seq],
        fed,
        candidates[0].budget,
        drafts,
    );
    let [first_result, second_result] = match result {
        Ok(results) => results,
        Err(error) => {
            let message = error.to_string();
            for (lane, a) in [first, second].into_iter().enumerate() {
                if drafts[lane].is_some() {
                    if let Some(state) = &mut a.spec {
                        state.cancel_draft();
                    }
                }
                let _ = a.events.send(EngineEvent::Error(message.clone()));
                a.dead = true;
            }
            return;
        }
    };
    match [drafts[0].is_some(), drafts[1].is_some()] {
        [true, true] => {
            EngineMetrics::inc(&metrics.mtp_ngram_b2_steps_total);
            EngineMetrics::inc(&metrics.mtp_routed_nn_b2_steps_total);
        }
        [false, false] => {
            EngineMetrics::inc(&metrics.native_mtp_b2_steps_total);
            EngineMetrics::inc(&metrics.mtp_routed_mm_b2_steps_total);
        }
        _ => EngineMetrics::inc(&metrics.mtp_routed_nm_b2_steps_total),
    }
    finish_mtp_routed_lane(first, candidates[0], first_result, metrics);
    finish_mtp_routed_lane(second, candidates[1], second_result, metrics);
}

fn finish_mtp_routed_lane(
    a: &mut ActiveSeq<'_>,
    candidate: &MtpRoutedCandidate,
    result: (Vec<u32>, usize, u32),
    metrics: &EngineMetrics,
) {
    match &candidate.source {
        MtpRoutedSource::Ngram(draft) => {
            a.spec
                .as_mut()
                .expect("lane N routera ma stan")
                .commit_validated(draft, result.1);
            finish_native_mtp_ngram_verified(a, draft, result.1, result.2, metrics);
        }
        MtpRoutedSource::Native { observe_ngram } => {
            let generated_before = a.generated.len();
            finish_native_mtp_step(a, candidate.budget, result.0, result.1, result.2, metrics);
            if *observe_ngram {
                let accepted = a.generated[generated_before..].to_vec();
                if let Some(state) = &mut a.spec {
                    state.observe_all(&accepted);
                }
            }
        }
    }
}

fn native_mtp_ngram_step(model: &mut Model, a: &mut ActiveSeq<'_>, metrics: &EngineMetrics) {
    let fed = *a.generated.last().expect("emit_token pushed the fed token");
    let budget = a.spec_budget;
    let available = model.native_mtp_available_budget(&a.seq, budget);
    let draft = {
        let state = a.spec.as_mut().expect("router MTP+n-gram ma stan hostowy");
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
    if !full_mtp_ngram_draft_fits(draft.len(), available, budget) {
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

    verify_native_mtp_ngram_prepared(model, a, &draft, metrics);
}

/// Jeden krok natywnego MTP. Model jest właścicielem draftu, weryfikatora i
/// checkpointów; serwer emituje zaakceptowany prefiks i zachowuje token
/// korekcyjny jako wejście następnej iteracji.
fn native_mtp_step(model: &mut Model, a: &mut ActiveSeq<'_>, metrics: &EngineMetrics) {
    let fed = *a.generated.last().expect("emit_token pushed the fed token");
    let available = model.native_mtp_available_budget(&a.seq, a.spec_budget);
    let Some(budget) = native_mtp_budget(available, a.spec_budget) else {
        serial_step(model, a);
        return;
    };
    let SeqSampler::Gpu(sampler) = &mut a.sampler else {
        unreachable!("native MTP is GPU-sampled")
    };
    sampler.note_token(fed);
    let (draft, accepted, correction) = match model.native_mtp_step(&mut a.seq, fed, budget) {
        Ok(result) => result,
        Err(error) => {
            let _ = a.events.send(EngineEvent::Error(error.to_string()));
            a.dead = true;
            return;
        }
    };
    finish_native_mtp_step(a, budget, draft, accepted, correction, metrics);
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
    let result = model.native_mtp_step_b2(&mut [&mut first.seq, &mut second.seq], fed, budget);
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
    EngineMetrics::inc(&metrics.native_mtp_b2_steps_total);
    finish_native_mtp_step(
        first,
        budget,
        first_result.0,
        first_result.1,
        first_result.2,
        metrics,
    );
    finish_native_mtp_step(
        second,
        budget,
        second_result.0,
        second_result.1,
        second_result.2,
        metrics,
    );
}

fn finish_native_mtp_step(
    a: &mut ActiveSeq<'_>,
    budget: usize,
    draft: Vec<u32>,
    accepted: usize,
    correction: u32,
    metrics: &EngineMetrics,
) {
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
        benchmark: a
            .prefill_profile
            .zip(a.benchmark_ttft_ms)
            .map(|(profile, ttft_ms)| BenchmarkTimings {
                target_gpu_ms: profile.target_gpu_ms,
                mtp_catchup_gpu_ms: profile.mtp_catchup_gpu_ms,
                ttft_ms,
            }),
    });
    a.dead = true;
    a.finished_cleanly = true;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::mtp_routed_pair_enabled;
    use super::parse_mtp_ngram_mixed_batch;
    use super::{
        dense_prefill_auto_backend_capable, dense_prefill_batch_plan, dense_prefill_batch_route,
        drain_poisoned_worker, hybrid_prefill_batch_plan, parse_dense_prefill_batch,
        parse_hybrid_prefill_batch, resolve_dense_prefill_batch, resolve_hybrid_prefill_batch,
        ActiveSeq, DensePrefillBatchMode, EngineEvent, EngineRequest, HybridPrefillBatchMode,
        Submission, dense_prefill_scheduler_quantum,
    };
    use super::{
        first_actionable_index, full_mtp_ngram_draft_fits, mtp_ngram_auto_backend,
        mtp_ngram_batch_plan, native_mtp_budget, native_mtp_step_budget, parse_mtp_ngram_batch,
        parse_native_mtp_b2, request_speculation_kind, reservation_fits, resolve_mtp_ngram_batch,
        validate_speculation_server_config, AdmissionDisposition, MtpNgramBatchMode,
    };
    use crate::metrics::EngineMetrics;
    use crate::model::hybrid_prefill_b2_backend_capable;
    use std::time::Duration;
    use crate::speculation::{ProposerKind, SpeculationKind, SpeculativeConfig};
    use forge_types::Vendor;
    use std::collections::VecDeque;
    use std::sync::mpsc;
    use std::time::Instant;

    #[test]
    fn przelacznik_mtp_b2_wymaga_scislej_wartosci_logicznej() {
        assert!(parse_native_mtp_b2(None).unwrap());
        assert!(parse_native_mtp_b2(Some("1")).unwrap());
        assert!(!parse_native_mtp_b2(Some("0")).unwrap());
        assert!(parse_native_mtp_b2(Some("true")).is_err());
        assert!(parse_native_mtp_b2(Some("")).is_err());
    }

    #[test]
    fn batch_mtp_ngram_ma_scisla_semantyke_rolloutu() {
        assert_eq!(
            parse_mtp_ngram_batch(None).unwrap(),
            MtpNgramBatchMode::Auto
        );
        assert_eq!(
            parse_mtp_ngram_batch(Some("0")).unwrap(),
            MtpNgramBatchMode::Off
        );
        assert_eq!(
            parse_mtp_ngram_batch(Some("1")).unwrap(),
            MtpNgramBatchMode::ForceOn
        );
        assert!(parse_mtp_ngram_batch(Some("true")).is_err());
        assert!(parse_mtp_ngram_batch(Some("")).is_err());
        assert!(!parse_mtp_ngram_mixed_batch(None).unwrap());
        assert!(!parse_mtp_ngram_mixed_batch(Some("0")).unwrap());
        assert!(parse_mtp_ngram_mixed_batch(Some("1")).unwrap());
        assert!(parse_mtp_ngram_mixed_batch(Some("true")).is_err());
        assert!(parse_mtp_ngram_mixed_batch(Some("")).is_err());
    }

    #[test]
    fn hybrid_prefill_batch_ma_scisla_flage_i_stabilny_plan_t32() {
        assert_eq!(
            parse_hybrid_prefill_batch(None).unwrap(),
            HybridPrefillBatchMode::Auto
        );
        assert_eq!(
            parse_hybrid_prefill_batch(Some("0")).unwrap(),
            HybridPrefillBatchMode::Off
        );
        assert_eq!(
            parse_hybrid_prefill_batch(Some("1")).unwrap(),
            HybridPrefillBatchMode::ForceOn
        );
        assert!(parse_hybrid_prefill_batch(Some("true")).is_err());
        assert!(parse_hybrid_prefill_batch(Some("")).is_err());

        let (pairs, singles) = hybrid_prefill_batch_plan(&[32, 31, 96, 1, 32]);
        assert_eq!(pairs, vec![[0, 2]]);
        assert_eq!(singles, vec![1, 3, 4]);
        assert_eq!(
            hybrid_prefill_batch_plan(&[31, 31]),
            (Vec::new(), vec![0, 1])
        );
    }

    #[test]
    fn dense_prefill_wybiera_pelny_kubel_i_zostawia_ragged_tail() {
        let pending = (0..16)
            .map(|index| (index, if index == 15 { 65 } else { 128 }))
            .collect::<Vec<_>>();
        let (indices, chunk, final_chunk) =
            dense_prefill_batch_plan(&pending, 128).expect("pełny B16");
        assert_eq!(indices, (0..16).collect::<Vec<_>>());
        assert_eq!(chunk, 64);
        assert!(!final_chunk);

        let final_pending = (0..8).map(|index| (index, 128)).collect::<Vec<_>>();
        let (indices, chunk, final_chunk) =
            dense_prefill_batch_plan(&final_pending, 128).expect("pełny final B8");
        assert_eq!(indices, (0..8).collect::<Vec<_>>());
        assert_eq!(chunk, 128);
        assert!(final_chunk);

        let ragged = vec![(0, 63), (1, 64), (2, 127), (3, 1)];
        assert!(dense_prefill_batch_plan(&ragged, 128).is_none());
        assert_eq!(dense_prefill_scheduler_quantum(1024, 1), 1024);
        assert_eq!(dense_prefill_scheduler_quantum(1024, 3), 1024);
        assert_eq!(dense_prefill_scheduler_quantum(1024, 4), 256);
        assert_eq!(dense_prefill_scheduler_quantum(1024, 8), 128);
        assert_eq!(dense_prefill_scheduler_quantum(1024, 16), 64);
        assert_eq!(dense_prefill_scheduler_quantum(64, 16), 64);
    }

    #[test]
    fn idle_coalescing_ogranicza_b1_i_zbiera_burst_do_b16() {
        assert_eq!(
            super::idle_dense_coalescing_decision(Duration::ZERO, 1, 16),
            super::IdleCoalescingDecision::Wait(Duration::from_millis(1))
        );
        assert_eq!(
            super::idle_dense_coalescing_decision(Duration::from_millis(1), 1, 16),
            super::IdleCoalescingDecision::Dispatch
        );
        assert_eq!(
            super::idle_dense_coalescing_decision(Duration::from_millis(1), 4, 16),
            super::IdleCoalescingDecision::Wait(Duration::from_millis(4))
        );
        assert_eq!(
            super::idle_dense_coalescing_decision(Duration::from_millis(4), 8, 16),
            super::IdleCoalescingDecision::Wait(Duration::from_millis(1))
        );
        assert_eq!(
            super::idle_dense_coalescing_decision(Duration::from_millis(5), 8, 16),
            super::IdleCoalescingDecision::Dispatch
        );
        assert_eq!(
            super::idle_dense_coalescing_decision(Duration::from_millis(2), 16, 16),
            super::IdleCoalescingDecision::Dispatch
        );
    }

    #[test]
    fn dense_prefill_ma_scisly_force_on_i_kontrolowany_blad_capability() {
        assert_eq!(
            parse_dense_prefill_batch(None).unwrap(),
            DensePrefillBatchMode::Auto
        );
        assert_eq!(
            parse_dense_prefill_batch(Some("0")).unwrap(),
            DensePrefillBatchMode::Off
        );
        assert_eq!(
            parse_dense_prefill_batch(Some("1")).unwrap(),
            DensePrefillBatchMode::ForceOn
        );
        assert!(parse_dense_prefill_batch(Some("true")).is_err());
        assert!(parse_dense_prefill_batch(Some("auto")).is_err());
        assert!(parse_dense_prefill_batch(Some("")).is_err());
        assert!(resolve_dense_prefill_batch(
            DensePrefillBatchMode::Auto,
            32,
            1024,
            true,
        )
        .unwrap());
        assert!(!resolve_dense_prefill_batch(
            DensePrefillBatchMode::Auto,
            32,
            1024,
            false,
        )
        .unwrap());
        assert!(!resolve_dense_prefill_batch(
            DensePrefillBatchMode::Off,
            32,
            1024,
            true,
        )
        .unwrap());
        assert!(resolve_dense_prefill_batch(
            DensePrefillBatchMode::ForceOn,
            32,
            1024,
            true,
        )
        .unwrap());
        assert!(resolve_dense_prefill_batch(
            DensePrefillBatchMode::ForceOn,
            32,
            1024,
            false,
        )
        .is_err());
        assert!(dense_prefill_batch_route(true, true));
        assert!(!dense_prefill_batch_route(true, false));
        assert!(!dense_prefill_batch_route(false, true));
    }

    #[test]
    fn dense_prefill_auto_odrzuca_wave64_i_brak_artefaktow() {
        // Fala 32 i blok 256 wystarcza u obu producentow; fala 64 nie.
        assert!(dense_prefill_auto_backend_capable(32, 1024));
        assert!(!dense_prefill_auto_backend_capable(64, 1024));
        assert!(!dense_prefill_auto_backend_capable(32, 128));
        for (warp_size, max_threads, rollout_capable) in [
            (64, 1024, true),
            (32, 128, true),
            (32, 1024, false),
        ] {
            assert!(!resolve_dense_prefill_batch(
                DensePrefillBatchMode::Auto,
                warp_size,
                max_threads,
                rollout_capable,
            )
            .unwrap());
        }
    }

    #[test]
    fn fatalny_poison_oproznia_kolejki_i_zatrzymuje_przyszle_submit() {
        let submission = || {
            let (events, receiver) = mpsc::channel();
            (
                Submission {
                    req: EngineRequest::default(),
                    events,
                    submitted_at: Instant::now(),
                    bypasses: 0,
                },
                receiver,
            )
        };
        let (waiting_submission, waiting_receiver) = submission();
        let (queued_submission, queued_receiver) = submission();
        let (work_tx, work_rx) = mpsc::channel();
        work_tx.send(queued_submission).unwrap();
        let mut active: Vec<ActiveSeq<'static>> = Vec::new();
        let mut waiting = VecDeque::from([waiting_submission]);
        let metrics = EngineMetrics::default();

        drain_poisoned_worker(
            "wstrzyknięty poison",
            &mut active,
            &mut waiting,
            &work_rx,
            &metrics,
        );

        assert!(active.is_empty());
        assert!(waiting.is_empty());
        assert!(matches!(
            waiting_receiver.recv().unwrap(),
            EngineEvent::Error(message) if message.contains("wstrzyknięty poison")
        ));
        assert!(matches!(
            queued_receiver.recv().unwrap(),
            EngineEvent::Error(message) if message.contains("wstrzyknięty poison")
        ));
        drop(work_rx);
        let (future_submission, _) = submission();
        assert!(work_tx.send(future_submission).is_err());
    }

    #[test]
    fn hybrid_prefill_b2_wymaga_zweryfikowanego_backendu_nvidia_warp32() {
        assert!(hybrid_prefill_b2_backend_capable(Vendor::Nvidia, 32));
        assert!(!hybrid_prefill_b2_backend_capable(Vendor::Nvidia, 64));
        assert!(!hybrid_prefill_b2_backend_capable(Vendor::Amd, 32));
        assert!(!hybrid_prefill_b2_backend_capable(Vendor::Amd, 64));
        assert!(!hybrid_prefill_b2_backend_capable(Vendor::Apple, 32));
        assert!(!hybrid_prefill_b2_backend_capable(Vendor::Cpu, 32));
        assert!(!hybrid_prefill_b2_backend_capable(Vendor::Intel, 32));

        assert!(resolve_hybrid_prefill_batch(
            HybridPrefillBatchMode::Auto,
            Vendor::Nvidia,
            32,
            true,
        )
        .unwrap());
        assert!(!resolve_hybrid_prefill_batch(
            HybridPrefillBatchMode::Auto,
            Vendor::Nvidia,
            32,
            false,
        )
        .unwrap());
        assert!(
            !resolve_hybrid_prefill_batch(HybridPrefillBatchMode::Auto, Vendor::Amd, 32, true,)
                .unwrap()
        );
        assert!(!resolve_hybrid_prefill_batch(
            HybridPrefillBatchMode::Off,
            Vendor::Nvidia,
            32,
            true,
        )
        .unwrap());
        assert!(resolve_hybrid_prefill_batch(
            HybridPrefillBatchMode::ForceOn,
            Vendor::Nvidia,
            32,
            true,
        )
        .unwrap());
        assert!(resolve_hybrid_prefill_batch(
            HybridPrefillBatchMode::ForceOn,
            Vendor::Nvidia,
            32,
            false,
        )
        .is_err());
        for (vendor, warp_size) in [
            (Vendor::Nvidia, 64),
            (Vendor::Amd, 32),
            (Vendor::Amd, 64),
            (Vendor::Apple, 32),
            (Vendor::Cpu, 32),
        ] {
            assert!(!resolve_hybrid_prefill_batch(
                HybridPrefillBatchMode::Auto,
                vendor,
                warp_size,
                true,
            )
            .unwrap());
            assert!(resolve_hybrid_prefill_batch(
                HybridPrefillBatchMode::ForceOn,
                vendor,
                warp_size,
                true,
            )
            .is_err());
        }
    }

    #[test]
    fn auto_mtp_ngram_wymaga_nvidia_warp32_i_capability_modelu() {
        assert!(mtp_ngram_auto_backend(forge_types::Vendor::Nvidia, 32));
        assert!(!mtp_ngram_auto_backend(forge_types::Vendor::Nvidia, 64));
        assert!(!mtp_ngram_auto_backend(forge_types::Vendor::Amd, 32));
        assert!(!mtp_ngram_auto_backend(forge_types::Vendor::Amd, 64));
        assert!(!mtp_ngram_auto_backend(forge_types::Vendor::Apple, 32));
        assert!(!mtp_ngram_auto_backend(forge_types::Vendor::Cpu, 32));
        assert!(resolve_mtp_ngram_batch(
            MtpNgramBatchMode::Auto,
            forge_types::Vendor::Nvidia,
            32,
            true,
        )
        .unwrap());
        assert!(!resolve_mtp_ngram_batch(
            MtpNgramBatchMode::Auto,
            forge_types::Vendor::Nvidia,
            32,
            false,
        )
        .unwrap());
        assert!(resolve_mtp_ngram_batch(
            MtpNgramBatchMode::ForceOn,
            forge_types::Vendor::Amd,
            64,
            true,
        )
        .unwrap());
        assert!(resolve_mtp_ngram_batch(
            MtpNgramBatchMode::ForceOn,
            forge_types::Vendor::Nvidia,
            32,
            false,
        )
        .is_err());
        assert!(!resolve_mtp_ngram_batch(
            MtpNgramBatchMode::Off,
            forge_types::Vendor::Nvidia,
            32,
            true,
        )
        .unwrap());
    }

    #[test]
    fn scheduler_routed_paruje_mieszane_zrodla_po_k_i_zostawia_tail_b1() {
        let sources = [false, true, true, false, false];
        let (pairs, singles) = mtp_ngram_batch_plan(&[Some(2), Some(3), Some(2), Some(3), Some(2)]);
        assert_eq!(pairs, vec![[0, 2], [1, 3]]);
        assert_eq!(singles, vec![4]);
        assert_eq!([sources[pairs[0][0]], sources[pairs[0][1]]], [false, true]);
        assert_eq!([sources[pairs[1][0]], sources[pairs[1][1]]], [true, false]);
        assert_eq!(mtp_ngram_batch_plan(&[Some(3), Some(2)]).1, vec![0, 1]);
        assert!(mtp_routed_pair_enabled(false, false, false));
        assert!(mtp_routed_pair_enabled(false, true, false));
        assert!(!mtp_routed_pair_enabled(true, true, false));
        assert!(!mtp_routed_pair_enabled(true, false, true));
        assert!(mtp_routed_pair_enabled(true, true, true));
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
        let router = SpeculativeConfig::chain(vec![ProposerKind::Mtp, ProposerKind::Ngram], 3)
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
        assert!(
            validate_speculation_server_config(SpeculationKind::NativeMtpNgram, 1, true).is_ok()
        );
        assert!(
            validate_speculation_server_config(SpeculationKind::NativeMtpNgram, 2, true).is_ok()
        );
        assert!(
            validate_speculation_server_config(SpeculationKind::HostProposer, 8, false).is_ok()
        );
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
    fn native_mtp_honors_configured_budget_and_clips_only_to_availability() {
        assert_eq!(native_mtp_budget(3, 3), Some(3));
        assert_eq!(native_mtp_budget(2, 3), Some(2));
        assert_eq!(native_mtp_budget(3, 2), Some(2));
        assert_eq!(native_mtp_budget(2, 2), Some(2));
        assert_eq!(native_mtp_budget(1, 3), None);
        assert_eq!(native_mtp_budget(0, 3), None);
    }

    #[test]
    fn mtp_ngram_k3_requires_full_availability_before_single_or_b2_routing() {
        assert!(full_mtp_ngram_draft_fits(3, 3, 3));
        assert!(full_mtp_ngram_draft_fits(2, 2, 2));
        assert!(!full_mtp_ngram_draft_fits(3, 2, 3));
        assert!(!full_mtp_ngram_draft_fits(2, 3, 3));
        assert!(!full_mtp_ngram_draft_fits(3, 1, 3));
        assert!(!full_mtp_ngram_draft_fits(3, 0, 3));
    }

    #[test]
    fn layer_major_kwant_chroni_decode_i_nie_tnie_samotnego_prefillu() {
        assert_eq!(
            super::layer_major_scheduler_quantum(Some(4096), 128, false),
            4096
        );
        assert_eq!(
            super::layer_major_scheduler_quantum(Some(4096), 128, true),
            1024
        );
        assert_eq!(super::layer_major_scheduler_quantum(None, 128, false), 128);
    }
}
