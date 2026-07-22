// ===== File: generate.rs — generation loop: prefill, decode, stream, stop =====

use forge_tokenize::{StopMatcher, StreamDecoder, Tokenizer};
use forge_types::{ForgeError, Result};

use crate::model::Model;
use crate::sample::{
    apply_logit_bias, compute_logprob, suppress_eos, GpuSampler, Sampler, SamplingParams,
    TokenLogprob, apply_penalties,
};

#[derive(Default)]
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
    /// `logit_bias` (SPEC §8.1.2): additive bias per token id, applied to the
    /// host logits before selection. Non-empty forces the CPU sampler.
    pub logit_bias: Vec<(u32, f32)>,
    /// `min_tokens` (SPEC §8.1.2): suppress every EOS id until this many tokens
    /// have been produced. Non-zero forces the CPU sampler.
    pub min_tokens: usize,
    /// `logprobs`/`top_logprobs` (SPEC §8.1.2): when set, report each token's
    /// log-probability plus this many top alternatives. Forces the CPU sampler
    /// (needs the full host logits for the log-softmax).
    pub logprobs: Option<usize>,
}

#[derive(Debug)]
pub enum StreamEvent {
    Token {
        id: u32,
        text: String,
        logprob: Option<TokenLogprob>,
    },
    Done {
        reason: FinishReason,
    },
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

/// CPU-path token draw over the full host logits: apply `logit_bias`, suppress
/// EOS for `min_tokens`, apply the grammar mask, sample, and (when requested)
/// compute the log-probability report over the post-processing distribution.
fn sample_cpu(
    sampler: &mut Sampler,
    matcher: &Option<forge_grammar::GrammarMatcher>,
    logits: &mut [f32],
    history: &[u32],
    generated_len: usize,
    req: &GenerateRequest,
) -> Result<(u32, Option<TokenLogprob>)> {
    apply_logit_bias(logits, &req.logit_bias);
    suppress_eos(logits, &req.eos_ids, generated_len, req.min_tokens);
    if let Some(m) = matcher {
        m.apply_mask(logits);
    }
    apply_penalties(logits, history, sampler.params());
    let id = sampler.sample_preprocessed(logits)?;
    let lp = req.logprobs.map(|n| compute_logprob(logits, id, n));
    Ok((id, lp))
}

fn run(
    model: &mut Model,
    tokenizer: &Tokenizer,
    req: &GenerateRequest,
    seq: &mut crate::kv::SeqKv,
    on_event: &mut impl FnMut(StreamEvent),
) -> Result<Generated> {
    // The CPU sampler runs whenever the request needs the full host logits
    // before selection: constrained decoding (grammar mask), `logit_bias`,
    // `min_tokens` (EOS suppression) or `logprobs` (host log-softmax).
    // Unconstrained requests keep the GPU sampler whenever the params fit its
    // kernels (path byte-identical).
    let mut matcher = req.grammar.as_ref().map(|g| g.matcher());
    let host_logits = matcher.is_some()
        || !req.logit_bias.is_empty()
        || req.min_tokens > 0
        || req.logprobs.is_some();
    let mut source = if !host_logits && model.gpu_sampling_supported(&req.sampling) {
        NextSource::Gpu(GpuSampler::new(req.sampling.clone()))
    } else {
        NextSource::Cpu(Sampler::new(req.sampling.clone()))
    };
    if let NextSource::Gpu(sampler) = &mut source {
        sampler.note_tokens(&req.prompt_tokens);
    }
    let mut decoder = StreamDecoder::new(tokenizer, true);
    let mut stops = StopMatcher::new(req.stop.clone());

    // Batched prefill; only the last chunk's logits matter.
    let mut logits = Vec::new();
    for chunk in req.prompt_tokens.chunks(crate::model::MAX_PREFILL_CHUNK) {
        logits = model.prefill_chunk(seq, chunk)?;
    }

    let mut text = String::new();
    let mut tokens = Vec::new();
    let mut sampling_history = if matches!(source, NextSource::Cpu(_))
        && req.sampling.clone().sanitized().has_penalties()
    {
        req.prompt_tokens.clone()
    } else {
        Vec::new()
    };
    let mut finish = FinishReason::Length;

    // First draw comes from the prefill logits (still resident on device for
    // the GPU path); subsequent draws ride the decode step.
    let (mut next, mut next_lp) = match &mut source {
        NextSource::Cpu(s) => sample_cpu(s, &matcher, &mut logits, &sampling_history, 0, req)?,
        NextSource::Gpu(g) => (model.sample_last_logits(g)?, None),
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
        if !sampling_history.is_empty() {
            sampling_history.push(next);
        }

        let piece = decoder.push(next)?;
        if !piece.is_empty() {
            let step = stops.push(&piece);
            if !step.emit.is_empty() {
                text.push_str(&step.emit);
                on_event(StreamEvent::Token {
                    id: next,
                    text: step.emit,
                    logprob: next_lp.take(),
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
        match &mut source {
            NextSource::Cpu(s) => {
                logits = model.step(seq, next)?;
                let (id, lp) = sample_cpu(
                    s,
                    &matcher,
                    &mut logits,
                    &sampling_history,
                    tokens.len(),
                    req,
                )?;
                next = id;
                next_lp = lp;
            }
            NextSource::Gpu(g) => {
                g.note_token(next);
                next = model.step_and_sample(seq, next, g)?;
                next_lp = None;
            }
        }
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
                    logprob: None,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_logprob_uzywa_logitow_po_karze_promptu() {
        let req = GenerateRequest {
            sampling: SamplingParams {
                temperature: 0.0,
                repetition_penalty: 2.0,
                ..SamplingParams::default()
            },
            logprobs: Some(2),
            ..GenerateRequest::default()
        };
        let mut sampler = Sampler::new(req.sampling.clone());
        let mut logits = [0.0, 5.0, 4.0];

        let (id, report) = sample_cpu(&mut sampler, &None, &mut logits, &[1], 0, &req).unwrap();

        assert_eq!(id, 2);
        assert_eq!(logits[1], 2.5);
        assert_eq!(report.unwrap().top[0].0, 2);
    }

    #[test]
    fn prompt_nie_wlicza_sie_do_min_tokens() {
        let req = GenerateRequest {
            sampling: SamplingParams {
                temperature: 0.0,
                ..SamplingParams::default()
            },
            eos_ids: vec![1],
            min_tokens: 2,
            ..GenerateRequest::default()
        };
        let mut sampler = Sampler::new(req.sampling.clone());
        let mut logits = [1.0, 5.0];

        let (id, _) = sample_cpu(&mut sampler, &None, &mut logits, &[7, 8, 9], 0, &req).unwrap();

        assert_eq!(id, 0);
        assert_eq!(logits[1], f32::NEG_INFINITY);
    }
}
