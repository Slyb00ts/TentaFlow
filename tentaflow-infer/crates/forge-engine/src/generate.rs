// ===== File: generate.rs — generation loop: prefill, decode, stream, stop =====

use forge_tokenize::{StopMatcher, StreamDecoder, Tokenizer};
use forge_types::{ForgeError, Result};

use crate::model::Model;
use crate::sample::{GpuSampler, Sampler, SamplingParams};

pub struct GenerateRequest {
    pub prompt_tokens: Vec<u32>,
    pub max_tokens: usize,
    pub sampling: SamplingParams,
    pub stop: Vec<String>,
    /// EOS ids terminate generation silently (not emitted).
    pub eos_ids: Vec<u32>,
    /// Constrained decoding (SPEC §8.1.2): when set, a per-step logit mask
    /// forces the output to conform to the grammar. Forces the CPU sampler
    /// (full logits on the host) so the mask applies before selection.
    pub grammar: Option<forge_grammar::GrammarProgram>,
}

#[derive(Debug)]
pub enum StreamEvent {
    Token { id: u32, text: String },
    Done { reason: FinishReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    Length,
    Eos,
}

#[derive(Debug)]
pub struct Generated {
    pub text: String,
    pub tokens: Vec<u32>,
    pub finish: FinishReason,
    pub prompt_tokens: usize,
}

/// Blocking generation; `on_event` receives streamed tokens as they decode.
pub fn generate(
    model: &mut Model,
    tokenizer: &Tokenizer,
    req: &GenerateRequest,
    mut on_event: impl FnMut(StreamEvent),
) -> Result<Generated> {
    if req.prompt_tokens.is_empty() {
        return Err(ForgeError::Scheduler("empty prompt".into()));
    }

    let mut seq = model.new_seq();
    let result = run(model, tokenizer, req, &mut seq, &mut on_event);
    model.release_seq(&mut seq);
    result
}

/// Where the next token comes from: the CPU sampler over downloaded logits,
/// or the on-GPU sampler over device-resident logits (only the sampled id
/// crosses PCIe). The GPU path is preferred whenever the params fit the
/// sampling kernels; the CPU path remains for configs that need full logits
/// on the host (unbounded top-k, future logprobs reporting).
enum NextSource {
    Cpu(Sampler),
    Gpu(GpuSampler),
}

fn run(
    model: &mut Model,
    tokenizer: &Tokenizer,
    req: &GenerateRequest,
    seq: &mut crate::kv::SeqKv,
    on_event: &mut impl FnMut(StreamEvent),
) -> Result<Generated> {
    // Constrained decoding runs on the CPU sampler: the mask must apply to the
    // full host logits before selection. Unconstrained requests keep the GPU
    // sampler whenever the params fit its kernels (path byte-identical).
    let mut matcher = req.grammar.as_ref().map(|g| g.matcher());
    let mut source = if matcher.is_none() && model.gpu_sampling_supported(&req.sampling) {
        NextSource::Gpu(GpuSampler::new(req.sampling.clone()))
    } else {
        NextSource::Cpu(Sampler::new(req.sampling.clone()))
    };
    let mut decoder = StreamDecoder::new(tokenizer, true);
    let mut stops = StopMatcher::new(req.stop.clone());

    // Batched prefill; only the last chunk's logits matter.
    let mut logits = Vec::new();
    for chunk in req.prompt_tokens.chunks(crate::model::MAX_PREFILL_CHUNK) {
        logits = model.prefill_chunk(seq, chunk)?;
    }

    let mut text = String::new();
    let mut tokens = Vec::new();
    let mut finish = FinishReason::Length;

    // First draw comes from the prefill logits (still resident on device for
    // the GPU path); subsequent draws ride the decode step.
    let mut next = match &mut source {
        NextSource::Cpu(s) => {
            if let Some(m) = &matcher {
                m.apply_mask(&mut logits);
            }
            s.sample(&logits, &tokens)?
        }
        NextSource::Gpu(g) => model.sample_last_logits(g)?,
    };
    if let Some(m) = &mut matcher {
        m.accept_token(next);
    }
    for _ in 0..req.max_tokens {
        if req.eos_ids.contains(&next) {
            finish = FinishReason::Eos;
            break;
        }
        tokens.push(next);

        let piece = decoder.push(next)?;
        if !piece.is_empty() {
            let step = stops.push(&piece);
            if !step.emit.is_empty() {
                text.push_str(&step.emit);
                on_event(StreamEvent::Token {
                    id: next,
                    text: step.emit,
                });
            }
            if step.matched.is_some() {
                finish = FinishReason::Stop;
                break;
            }
        }

        if tokens.len() == req.max_tokens {
            break;
        }
        next = match &mut source {
            NextSource::Cpu(s) => {
                logits = model.step(seq, next)?;
                if let Some(m) = &matcher {
                    m.apply_mask(&mut logits);
                }
                s.sample(&logits, &tokens)?
            }
            NextSource::Gpu(g) => {
                g.note_token(next);
                model.step_and_sample(seq, next, g)?
            }
        };
        if let Some(m) = &mut matcher {
            m.accept_token(next);
        }
    }

    // Flush the decoder tail unless a stop consumed the stream. Flushed text
    // goes through the same stop-matching and event path as live tokens so
    // streaming clients see the full output and stops still terminate.
    if finish != FinishReason::Stop {
        let last_id = tokens.last().copied().unwrap_or(0);
        let mut emit = |s: String, on_event: &mut dyn FnMut(StreamEvent)| {
            if !s.is_empty() {
                text.push_str(&s);
                on_event(StreamEvent::Token {
                    id: last_id,
                    text: s,
                });
            }
        };
        let tail = decoder.finish()?;
        if !tail.is_empty() {
            let step = stops.push(&tail);
            emit(step.emit, on_event);
            if step.matched.is_some() {
                finish = FinishReason::Stop;
            }
        }
        if finish != FinishReason::Stop {
            emit(stops.finish(), on_event);
        }
    }

    on_event(StreamEvent::Done { reason: finish });
    Ok(Generated {
        text,
        tokens,
        finish,
        prompt_tokens: req.prompt_tokens.len(),
    })
}
