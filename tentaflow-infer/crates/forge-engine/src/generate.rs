// ===== File: generate.rs — generation loop: prefill, decode, stream, stop =====

use forge_tokenize::{StopMatcher, StreamDecoder, Tokenizer};
use forge_types::{ForgeError, Result};

use crate::model::Model;
use crate::sample::{Sampler, SamplingParams};

pub struct GenerateRequest {
    pub prompt_tokens: Vec<u32>,
    pub max_tokens: usize,
    pub sampling: SamplingParams,
    pub stop: Vec<String>,
    /// EOS ids terminate generation silently (not emitted).
    pub eos_ids: Vec<u32>,
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

fn run(
    model: &mut Model,
    tokenizer: &Tokenizer,
    req: &GenerateRequest,
    seq: &mut crate::kv::SeqKv,
    on_event: &mut impl FnMut(StreamEvent),
) -> Result<Generated> {
    let mut sampler = Sampler::new(req.sampling.clone());
    let mut decoder = StreamDecoder::new(tokenizer, true);
    let mut stops = StopMatcher::new(req.stop.clone());

    // Prefill: v0 pushes prompt tokens through the decode path one by one;
    // only the last token's logits matter.
    let mut logits = Vec::new();
    for &t in &req.prompt_tokens {
        logits = model.step(seq, t)?;
    }

    let mut text = String::new();
    let mut tokens = Vec::new();
    let mut finish = FinishReason::Length;

    for _ in 0..req.max_tokens {
        let next = sampler.sample(&logits, &tokens)?;
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
        logits = model.step(seq, next)?;
    }

    // Flush the decoder tail unless a stop consumed the stream.
    if finish != FinishReason::Stop {
        let tail = decoder.finish()?;
        if !tail.is_empty() {
            let step = stops.push(&tail);
            if !step.emit.is_empty() {
                text.push_str(&step.emit);
            }
        }
        let rest = stops.finish();
        if !rest.is_empty() {
            text.push_str(&rest);
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
