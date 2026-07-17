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
use crate::sample::{GpuSampler, Sampler, SamplingParams};

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
    next: Option<PendingNext>,
    /// Client hang-up detected; sequence is torn down at the next iteration.
    dead: bool,
}

/// Spawn the GPU worker thread. `prefill_chunk` bounds how many prompt tokens
/// one sequence may prefill per scheduler iteration, protecting decode ITL of
/// the other active sequences (chunked prefill).
pub fn spawn_engine(
    mut model: Model,
    tokenizer: Arc<Tokenizer>,
    max_active: usize,
    prefill_chunk: usize,
) -> EngineHandle {
    let (tx, rx) = mpsc::channel::<Submission>();
    std::thread::Builder::new()
        .name("forge-engine-worker".into())
        .spawn(move || worker(&mut model, &tokenizer, &rx, max_active, prefill_chunk))
        .expect("spawn engine worker");
    EngineHandle { tx }
}

fn worker<'t>(
    model: &mut Model,
    tokenizer: &'t Tokenizer,
    rx: &mpsc::Receiver<Submission>,
    max_active: usize,
    prefill_chunk: usize,
) {
    let mut active: Vec<ActiveSeq<'t>> = Vec::new();
    let mut waiting: VecDeque<Submission> = VecDeque::new();

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

        // Admission: KV projection — prompt + max_tokens must fit in pages.
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
            let permanently_too_large = match need_pages {
                None => true,
                Some(n) => n > model.kv.cfg.n_pages,
            };
            if permanently_too_large {
                let sub = waiting.pop_front().unwrap();
                let _ = sub.events.send(EngineEvent::Error(match need_pages {
                    Some(n) => format!(
                        "request needs {n} KV pages, cache has {} total",
                        model.kv.cfg.n_pages
                    ),
                    None => "request size overflows: prompt_tokens + max_tokens".into(),
                }));
                continue;
            }
            if need_pages.unwrap() > model.kv.free_page_count() {
                break; // transient KV pressure: retry when a sequence finishes
            }
            let sub = waiting.pop_front().unwrap();
            if sub.req.prompt_tokens.is_empty() {
                let _ = sub.events.send(EngineEvent::Error("empty prompt".into()));
                continue;
            }
            let prompt_len = sub.req.prompt_tokens.len();
            let sampler = if model.gpu_sampling_supported(&sub.req.sampling) {
                SeqSampler::Gpu(GpuSampler::new(sub.req.sampling.clone()))
            } else {
                SeqSampler::Cpu(Sampler::new(sub.req.sampling.clone()))
            };
            active.push(ActiveSeq {
                seq: model.new_seq(),
                sampler,
                decoder: StreamDecoder::new(tokenizer, true),
                stops: StopMatcher::new(sub.req.stop.clone()),
                events: sub.events,
                pending_prompt: sub.req.prompt_tokens.into(),
                generated: Vec::new(),
                max_tokens: sub.req.max_tokens.max(1),
                eos_ids: sub.req.eos_ids,
                prompt_len,
                next: None,
                dead: false,
            });
        }

        // One scheduler iteration: each active sequence advances — prefill
        // sequences by up to `prefill_chunk` tokens, decode sequences by one.
        for a in active.iter_mut() {
            if a.dead {
                continue;
            }
            let r = advance(model, a, prefill_chunk);
            if let Err(e) = r {
                let _ = a.events.send(EngineEvent::Error(e.to_string()));
                a.dead = true;
            }
        }

        // Tear down finished/dead sequences and release their pages.
        active.retain_mut(|a| {
            if a.dead {
                model.kv.release(&mut a.seq);
                false
            } else {
                true
            }
        });
    }
}

/// Advance one sequence by one scheduler quantum. Sets `dead` on completion.
fn advance(model: &mut Model, a: &mut ActiveSeq<'_>, prefill_chunk: usize) -> Result<()> {
    if !a.pending_prompt.is_empty() {
        // Batched prefill: one quantum per iteration keeps decode ITL of the
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

    if a.eos_ids.contains(&next) {
        finish(a, FinishReason::Eos)?;
        return Ok(());
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
            return Ok(());
        }
        if step.matched.is_some() {
            finish(a, FinishReason::Stop)?;
            return Ok(());
        }
    }

    if a.generated.len() >= a.max_tokens {
        finish(a, FinishReason::Length)?;
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
    });
    a.dead = true;
    Ok(())
}
