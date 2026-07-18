// ===== File: server.rs — engine service: request queue + iteration-level scheduler =====
// The GPU worker owns the Model on a dedicated thread and interleaves active
// sequences one token per iteration (continuous batching semantics; kernel-
// level batching replaces the inner loop later without changing this API).
// Admission control projects KV page demand before accepting work (SPEC §9.1).

use std::collections::VecDeque;
use std::sync::mpsc;
use std::sync::Arc;

use forge_tokenize::{StopMatcher, StreamDecoder, Tokenizer};
use forge_types::{ForgeError, Result};

use crate::generate::FinishReason;
use crate::kv::SeqKv;
use crate::metrics::{EngineMetrics, SeqTiming};
use crate::model::{Model, MAX_PREFILL_CHUNK, MAX_SPEC_DRAFT};
use crate::sample::{
    apply_logit_bias, compute_logprob, suppress_eos, GpuSampler, Sampler, SamplingParams,
    SeqSampleParams, TokenLogprob,
};
use crate::speculation::{CascadeComposer, NgramProposer, SpeculativeState};

/// Shortest draft that is worth a verify forward (SPEC §6). The verify runs the
/// ungraphed prefill path, so on a launch-bound small model it only wins when it
/// can replace several graphed decode steps; shorter drafts fall back to the
/// plain single-token step. Long enough that ordinary prose (only short
/// coincidental drafts) never verifies, low enough that genuine recurring
/// context (which drafts to the full budget) always does.
const MIN_VERIFY_DRAFT: usize = 8;

/// Speculative decoding configuration (SPEC §6). `enabled` off = today's decode
/// loop, byte-for-byte. When on, greedy sequences on the standard dense
/// paged-KV path draft `draft_tokens` continuation tokens per step with an
/// n-gram proposer and verify them in one forward; output stays identical to
/// non-speculative greedy decode. Requests that are not greedy, use a
/// repetition penalty, need host logits (grammar / logit_bias / min_tokens /
/// logprobs), or run on an ineligible model silently fall back to plain decode.
#[derive(Clone, Copy, Debug)]
pub struct SpeculativeConfig {
    pub enabled: bool,
    /// Draft length per step (n-gram budget), clamped to `MAX_SPEC_DRAFT`.
    pub draft_tokens: usize,
}

impl SpeculativeConfig {
    pub fn off() -> Self {
        Self {
            enabled: false,
            draft_tokens: 0,
        }
    }

    /// N-gram speculation with a `draft_tokens` budget (clamped to
    /// `1..=MAX_SPEC_DRAFT`).
    pub fn ngram(draft_tokens: usize) -> Self {
        Self {
            enabled: true,
            draft_tokens: draft_tokens.clamp(1, MAX_SPEC_DRAFT),
        }
    }
}

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
    },
    Error(String),
}

struct Submission {
    req: EngineRequest,
    events: mpsc::Sender<EngineEvent>,
}

/// Cheap cloneable handle; the worker thread ends when all handles drop.
#[derive(Clone)]
pub struct EngineHandle {
    tx: mpsc::Sender<Submission>,
    metrics: Arc<EngineMetrics>,
}

impl EngineHandle {
    /// Queue a request; events stream on the returned receiver.
    pub fn submit(&self, req: EngineRequest) -> Result<mpsc::Receiver<EngineEvent>> {
        let (etx, erx) = mpsc::channel();
        self.tx
            .send(Submission { req, events: etx })
            .map_err(|_| ForgeError::Scheduler("engine worker stopped".into()))?;
        Ok(erx)
    }

    /// Live observability counters/gauges/histograms (SPEC §8.3), shared with
    /// the worker thread. Read-only for callers.
    pub fn metrics(&self) -> &Arc<EngineMetrics> {
        &self.metrics
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
    /// Speculative-decode state (SPEC §6): `Some` only for an eligible greedy
    /// sequence with speculation enabled. Holds the n-gram proposer's history
    /// index and per-proposer acceptance stats; drives draft/verify/commit.
    spec: Option<SpeculativeState>,
    /// Draft budget per speculative step (0 when `spec` is `None`).
    spec_budget: usize,
    /// Verification forwards run for this sequence (each yields 1..=k+1 tokens).
    spec_forwards: u64,
    /// Draft tokens accepted across all verifications (excludes correction/bonus
    /// tokens, which every forward also produces).
    spec_accepted: u64,
    /// TTFT / inter-token / decode-tps timing feeding the metrics histograms.
    timing: SeqTiming,
    /// Set once `finish` has emitted `Done`; distinguishes a clean completion
    /// from an errored / hung-up teardown for the requests_errored counter.
    finished_cleanly: bool,
}

/// Spawn the GPU worker thread. `prefill_chunk` bounds how many prompt tokens
/// one sequence may prefill per scheduler iteration, protecting decode ITL of
/// the other active sequences (chunked prefill).
pub fn spawn_engine(
    model: Model,
    tokenizer: Arc<Tokenizer>,
    max_active: usize,
    prefill_chunk: usize,
) -> EngineHandle {
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
) -> EngineHandle {
    // Speculation needs the standard dense paged-KV path; log once if the model
    // is ineligible so an operator who asked for it is not left guessing.
    let spec = if spec.enabled && !model.speculation_eligible() {
        tracing::warn!(
            "speculative decoding requested but this model/config is ineligible \
             (needs dense F16 paged KV, no tier, no prefix cache, f16/q8_0 head); \
             running without speculation"
        );
        SpeculativeConfig::off()
    } else {
        spec
    };
    if spec.enabled {
        tracing::info!(
            "speculative decoding: n-gram proposer, draft budget {}",
            spec.draft_tokens
        );
    }
    let (tx, rx) = mpsc::channel::<Submission>();
    let metrics = Arc::new(EngineMetrics::new());
    let worker_metrics = metrics.clone();
    std::thread::Builder::new()
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
                &worker_metrics,
            )
        })
        .expect("spawn engine worker");
    EngineHandle { tx, metrics }
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
    metrics: &EngineMetrics,
) {
    let mut active: Vec<ActiveSeq<'t>> = Vec::new();
    let mut waiting: VecDeque<Submission> = VecDeque::new();
    // Total KV pages is fixed at startup; export it once as a gauge baseline.
    EngineMetrics::set(&metrics.kv_pages_total, model.kv.cfg.n_pages as u64);

    // Provision the batched-decode scratch + graph buckets for the full active
    // width once. A failure here (VRAM pressure) is surfaced per-request by the
    // per-batch re-ensure inside `batched_decode`.
    let _ = model.ensure_batch(max_active);

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
            let Some(sub) = waiting.front() else { break };
            let page = model.kv.cfg.page_size;
            // A request that can never fit is rejected permanently; one that
            // only exceeds the currently free pages waits for a slot. The
            // error strings differ so the API layer can map them to 400 vs
            // transient handling.
            let need_pages = sub
                .req
                .prompt_tokens
                .len()
                .checked_add(sub.req.max_tokens)
                .map(|total| total.div_ceil(page));
            let capacity = model.max_request_pages();
            let permanently_too_large = match need_pages {
                None => true,
                Some(n) => n > capacity,
            };
            if permanently_too_large {
                let sub = waiting.pop_front().unwrap();
                let _ = sub.events.send(EngineEvent::Error(match need_pages {
                    Some(n) => format!(
                        "request needs {n} KV pages, cache has {capacity} total"
                    ),
                    None => "request size overflows: prompt_tokens + max_tokens".into(),
                }));
                EngineMetrics::inc(&metrics.requests_errored);
                continue;
            }
            // A prefix-cache hit (SPEC §5.2) shrinks the pages this request must
            // prefill: the shared prefix is already resident. The projection
            // uses a read-only match; the actual borrow happens once the
            // sequence exists. Admission counts reclaimable cached pages as
            // available, so a full-but-reclaimable cache never blocks work.
            let cache_read_pages = model.prefix_match_len(&sub.req.prompt_tokens) / page;
            let admit_floor = if model.tier_enabled() {
                need_pages
                    .unwrap()
                    .min(crate::tier::min_resident_pages(page))
            } else {
                need_pages.unwrap().saturating_sub(cache_read_pages)
            };
            if admit_floor > model.available_pages() {
                break; // transient KV pressure: retry when a sequence finishes
            }
            let sub = waiting.pop_front().unwrap();
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
            let sampler = if !host_logits && model.gpu_sampling_supported(&sub.req.sampling) {
                SeqSampler::Gpu(GpuSampler::new(sub.req.sampling.clone()))
            } else {
                SeqSampler::Cpu(Sampler::new(sub.req.sampling.clone()))
            };
            // Borrow the longest cached prefix (pins shared pages); only the
            // divergent suffix stays to prefill.
            let mut seq = model.new_seq();
            let cache_read = model.acquire_prefix(&mut seq, &prompt);
            let pending_prompt: VecDeque<u32> = prompt[cache_read..].iter().copied().collect();
            // Speculative decoding engages only for a greedy, penalty-free,
            // GPU-sampled sequence (the n-gram verifier reproduces greedy argmax
            // exactly; a repetition penalty or host-logit feature would diverge).
            // The proposer indexes the whole prompt so repeated/structured
            // prefixes draft immediately.
            let greedy = matches!(sampler, SeqSampler::Gpu(_))
                && sub.req.sampling.clone().sanitized().temperature <= 0.0
                && sub.req.sampling.repetition_penalty == 1.0;
            let (spec_state, spec_budget) = if spec.enabled && greedy {
                // A 3-gram floor keeps ordinary prose (where short-gram
                // coincidences abound) on the plain decode path and only
                // speculates on genuinely recurring context.
                let composer =
                    CascadeComposer::new(vec![Box::new(NgramProposer::with_min_gram(3))]);
                let mut st = SpeculativeState::new(composer);
                st.observe_all(&prompt);
                (Some(st), spec.draft_tokens)
            } else {
                (None, 0)
            };
            active.push(ActiveSeq {
                seq,
                sampler,
                decoder: StreamDecoder::new(tokenizer, true),
                stops: StopMatcher::new(sub.req.stop.clone()),
                events: sub.events,
                pending_prompt,
                generated: Vec::new(),
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
                spec_budget,
                spec_forwards: 0,
                spec_accepted: 0,
                timing: SeqTiming::new(),
                finished_cleanly: false,
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
        batch_gpu_decode(model, &mut active, batch_min, metrics);

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
    if a.eos_ids.contains(&next) {
        finish(a, FinishReason::Eos, metrics)?;
        return Ok(StepOutcome::Finished);
    }
    a.generated.push(next);
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
            a.next = Some(match &mut a.sampler {
                SeqSampler::Cpu(_) => PendingNext::Logits(logits),
                // The prefill logits are still device-resident: draw now,
                // before another sequence overwrites the shared buffer.
                SeqSampler::Gpu(g) => {
                    let t = model.sample_last_logits(g)?;
                    // Keep the proposer's history in lock-step: the first token
                    // is confirmed here, before it is ever used as a draft base.
                    if let Some(s) = &mut a.spec {
                        s.observe(t);
                    }
                    PendingNext::Token(t)
                }
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
                let id = s.sample(&logits, &a.generated)?;
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
    metrics: &EngineMetrics,
) {
    let vocab = model.weights.descriptor.params.vocab_size;

    // Phase 1: emit each ready sequence's pending token; collect the indices of
    // survivors that still need to be fed one more token.
    let mut feed_idx: Vec<usize> = Vec::new();
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
                if a.spec.is_some() {
                    speculative_step(model, a, metrics);
                } else {
                    feed_idx.push(i);
                }
            }
            Ok(StepOutcome::Finished) => {}
            Err(e) => {
                let _ = a.events.send(EngineEvent::Error(e.to_string()));
                a.dead = true;
            }
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
    if feed_idx.len() < batch_min.max(2) {
        for &i in &feed_idx {
            serial_step(model, &mut active[i]);
        }
        return;
    }

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
        s.draft(budget)
    };

    // A verify forward runs the ungraphed prefill path; on a small model each
    // graphed single-token decode step is so cheap that a verify only pays off
    // when it can replace several of them. Verifying a short draft would lose
    // (its wasted rejections cost more than the few decodes it could save), so a
    // draft below the gate falls back to the plain graphed step — this is what
    // keeps ordinary prose (which only ever yields short coincidental drafts)
    // from regressing.
    if draft.len() < MIN_VERIFY_DRAFT.min(budget) {
        // Short or absent draft — plain single-token greedy step (the
        // `serial_step` path), then record the confirmed token in the history.
        let SeqSampler::Gpu(g) = &mut a.sampler else {
            unreachable!("spec sequences are GPU-sampled")
        };
        g.note_token(fed);
        match model.step_and_sample(&mut a.seq, fed, g) {
            Ok(t) => {
                if let Some(s) = &mut a.spec {
                    s.observe(t);
                }
                a.next = Some(PendingNext::Token(t));
            }
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
        s.commit(&draft, accepted);
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
    // The correction/bonus token is confirmed but not yet resident in KV (the
    // next iteration feeds it); record it in the proposer history now.
    if let Some(s) = &mut a.spec {
        s.observe(correction);
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
        }
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
    });
    a.dead = true;
    a.finished_cleanly = true;
    Ok(())
}
