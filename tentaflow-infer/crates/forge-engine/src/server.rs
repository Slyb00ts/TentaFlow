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
use crate::model::{Model, MAX_PREFILL_CHUNK};
use crate::sample::{GpuSampler, Sampler, SamplingParams, SeqSampleParams};

pub struct EngineRequest {
    pub prompt_tokens: Vec<u32>,
    pub max_tokens: usize,
    pub sampling: SamplingParams,
    pub stop: Vec<String>,
    pub eos_ids: Vec<u32>,
}

#[derive(Debug)]
pub enum EngineEvent {
    Token {
        id: u32,
        text: String,
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
    prompt_len: usize,
    /// Prompt tokens served from the prefix cache (SPEC §5.2), reported in the
    /// completion usage as `cached_tokens`.
    cache_read: usize,
    next: Option<PendingNext>,
    /// Client hang-up detected; sequence is torn down at the next iteration.
    dead: bool,
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
    spawn_engine_batched(model, tokenizer, max_active, prefill_chunk, default_batch_min())
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
) -> EngineHandle {
    let (tx, rx) = mpsc::channel::<Submission>();
    std::thread::Builder::new()
        .name("forge-engine-worker".into())
        .spawn(move || worker(&mut model, &tokenizer, &rx, max_active, prefill_chunk, batch_min))
        .expect("spawn engine worker");
    EngineHandle { tx }
}

fn worker<'t>(
    model: &mut Model,
    tokenizer: &'t Tokenizer,
    rx: &mpsc::Receiver<Submission>,
    max_active: usize,
    prefill_chunk: usize,
    batch_min: usize,
) {
    let mut active: Vec<ActiveSeq<'t>> = Vec::new();
    let mut waiting: VecDeque<Submission> = VecDeque::new();

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
                continue;
            }
            let prompt = sub.req.prompt_tokens;
            let prompt_len = prompt.len();
            let sampler = if model.gpu_sampling_supported(&sub.req.sampling) {
                SeqSampler::Gpu(GpuSampler::new(sub.req.sampling.clone()))
            } else {
                SeqSampler::Cpu(Sampler::new(sub.req.sampling.clone()))
            };
            // Borrow the longest cached prefix (pins shared pages); only the
            // divergent suffix stays to prefill.
            let mut seq = model.new_seq();
            let cache_read = model.acquire_prefix(&mut seq, &prompt);
            let pending_prompt: VecDeque<u32> = prompt[cache_read..].iter().copied().collect();
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
                prompt_len,
                cache_read,
                next: None,
                dead: false,
            });
        }

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
            if let Err(e) = advance(model, a, prefill_chunk) {
                let _ = a.events.send(EngineEvent::Error(e.to_string()));
                a.dead = true;
            }
        }
        batch_gpu_decode(model, &mut active, batch_min);

        // Tear down finished/dead sequences and release their pages (and any
        // tier chunks they spilled).
        active.retain_mut(|a| {
            if a.dead {
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
fn emit_token(a: &mut ActiveSeq<'_>, next: u32) -> Result<StepOutcome> {
    if a.eos_ids.contains(&next) {
        finish(a, FinishReason::Eos)?;
        return Ok(StepOutcome::Finished);
    }
    a.generated.push(next);

    let piece = a.decoder.push(next)?;
    if !piece.is_empty() {
        let step = a.stops.push(&piece);
        if !step.emit.is_empty()
            && a.events
                .send(EngineEvent::Token {
                    id: next,
                    text: step.emit,
                })
                .is_err()
        {
            // Client hung up — cancel generation, free the slot.
            a.dead = true;
            return Ok(StepOutcome::Finished);
        }
        if step.matched.is_some() {
            finish(a, FinishReason::Stop)?;
            return Ok(StepOutcome::Finished);
        }
    }

    if a.generated.len() >= a.max_tokens {
        finish(a, FinishReason::Length)?;
        return Ok(StepOutcome::Finished);
    }
    Ok(StepOutcome::Continue)
}

/// Advance one sequence by one scheduler quantum (prefill chunk or a single
/// CPU-sampled decode step). GPU-sampled decode goes through the batched path.
fn advance(model: &mut Model, a: &mut ActiveSeq<'_>, prefill_chunk: usize) -> Result<()> {
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
                SeqSampler::Gpu(g) => PendingNext::Token(model.sample_last_logits(g)?),
            });
        }
        return Ok(());
    }

    // CPU-sampled decode (GPU decode is batched elsewhere).
    let next = match a
        .next
        .take()
        .ok_or_else(|| ForgeError::Scheduler("missing next-token state".into()))?
    {
        PendingNext::Logits(logits) => match &mut a.sampler {
            SeqSampler::Cpu(s) => s.sample(&logits, &a.generated)?,
            SeqSampler::Gpu(_) => {
                return Err(ForgeError::Scheduler(
                    "GPU-sampled sequence carried host logits".into(),
                ))
            }
        },
        PendingNext::Token(t) => t,
    };

    if let StepOutcome::Finished = emit_token(a, next)? {
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
fn batch_gpu_decode(model: &mut Model, active: &mut [ActiveSeq<'_>], batch_min: usize) {
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
        match emit_token(a, next) {
            Ok(StepOutcome::Continue) => feed_idx.push(i),
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

fn finish(a: &mut ActiveSeq<'_>, mut reason: FinishReason) -> Result<()> {
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
                });
            }
        }
    }
    let _ = a.events.send(EngineEvent::Done {
        reason,
        tokens: a.generated.len(),
        prompt_tokens: a.prompt_len,
        cache_read_tokens: a.cache_read,
    });
    a.dead = true;
    Ok(())
}
