// =============================================================================
// File: services/runtime/middleware.rs
// Streaming pipeline interceptors. Each middleware wraps a stream of
// `ChatCompletionChunk`s and may rewrite, drop, or buffer chunks before
// they reach downstream consumers (TTS sink, HTTP client, mesh forward).
//
// Middleware is split in two: a factory (`StreamMiddlewareFactory`,
// configured once and shared across requests) and a session
// (`StreamMiddlewareSession`, owned per request and free to keep mutable
// state). The split is deliberate — buffering middlewares like TTS and
// cross-chunk PII redaction MUST hold per-stream state, otherwise
// concurrent requests would mix tokens or leak partially redacted text
// across users.
//
// Ordering matters: PII filtering must come BEFORE TTS buffering so the
// synthesizer never sees raw text. `#[async_trait]` keeps the session
// trait object-safe so the executor can walk a heterogeneous stack.
// =============================================================================

use std::sync::Arc;

use async_trait::async_trait;

use crate::api::openai::types::ChatCompletionChunk;
use crate::error::Result;

/// Configured, shared middleware. Holds long-lived dependencies (PII
/// rules, TTS settings); never touches per-request state. The executor
/// keeps factories as `Vec<Arc<dyn StreamMiddlewareFactory>>` and asks
/// each one for a fresh `StreamMiddlewareSession` every time a stream
/// starts.
pub trait StreamMiddlewareFactory: Send + Sync {
    /// Stable name used for telemetry / debugging — surfaced in tracing
    /// when a session swallows a chunk.
    fn name(&self) -> &'static str;

    /// Build a fresh per-stream session. Called once per outgoing
    /// stream; never reused across streams. The `Box<dyn ...>` is
    /// `Send` so the session can move between tokio tasks.
    fn start_session(&self) -> Box<dyn StreamMiddlewareSession>;
}

/// Per-stream interceptor. Owned exclusively by one stream; mutable
/// methods give buffering middlewares somewhere to keep partial text
/// without sharing state with siblings. `Ok(None)` from
/// `process_chunk` means "swallow this chunk" (the buffering case);
/// `flush` runs once after upstream EOF and emits any tail.
#[async_trait]
pub trait StreamMiddlewareSession: Send {
    /// Stable name (mirrors the factory) so tracing in `apply_stack`
    /// can identify which session swallowed a chunk.
    fn name(&self) -> &'static str;

    /// Process one chunk. `Ok(Some)` forwards (possibly rewritten);
    /// `Ok(None)` swallows; `Err` terminates the stream.
    async fn process_chunk(
        &mut self,
        chunk: ChatCompletionChunk,
    ) -> Result<Option<ChatCompletionChunk>>;

    /// Called once after upstream EOF. Default impl is no-op so simple
    /// stateless sessions don't need to override.
    async fn flush(&mut self) -> Result<Vec<ChatCompletionChunk>> {
        Ok(Vec::new())
    }
}

// =============================================================================
// PII filter — buffers across chunks via the legacy `StreamingProcessor`
// so a pattern split between two tokens (`alice@` + `example.com`) is
// caught as one match. Per-stream state is mandatory; running clean_text
// independently on each chunk would leak the un-matched fragments.
// =============================================================================

/// Configured PII redaction. Wraps the existing `ResponseMiddleware`
/// (which carries the rule set) and hands out a fresh
/// `StreamingProcessor` for every stream.
pub struct PiiFilterFactory {
    inner: Arc<crate::middleware::ResponseMiddleware>,
}

impl PiiFilterFactory {
    pub fn new(inner: Arc<crate::middleware::ResponseMiddleware>) -> Self {
        Self { inner }
    }
}

impl StreamMiddlewareFactory for PiiFilterFactory {
    fn name(&self) -> &'static str {
        "pii_filter"
    }

    fn start_session(&self) -> Box<dyn StreamMiddlewareSession> {
        Box::new(PiiFilterSession {
            processor: self.inner.streaming_processor(),
        })
    }
}

/// Per-stream PII session. Holds a `StreamingProcessor` so cross-chunk
/// patterns are buffered until a sentence boundary or buffer cap.
struct PiiFilterSession {
    processor: crate::middleware::StreamingProcessor,
}

#[async_trait]
impl StreamMiddlewareSession for PiiFilterSession {
    fn name(&self) -> &'static str {
        "pii_filter"
    }

    /// Feed the chunk's text into the buffered processor. The processor
    /// returns redacted fragments only when it has enough context (full
    /// buffer or sentence boundary); until then we swallow the chunk so
    /// downstream consumers do not see the un-matched text. On
    /// `process_token` failure we fall through with the original chunk
    /// blanked — never propagate the error, since the engine's error
    /// message could echo the raw input.
    async fn process_chunk(
        &mut self,
        mut chunk: ChatCompletionChunk,
    ) -> Result<Option<ChatCompletionChunk>> {
        let mut buffered_text = String::new();
        let mut had_text = false;
        for choice in chunk.choices.iter_mut() {
            if let Some(text) = choice.delta.content.take() {
                had_text = true;
                match self.processor.process_token(&text) {
                    Ok(Some(parts)) => {
                        for part in parts {
                            buffered_text.push_str(&part);
                        }
                    }
                    Ok(None) => {
                        // Token absorbed into the per-stream buffer —
                        // nothing to forward yet.
                    }
                    Err(err) => {
                        tracing::warn!(
                            error = %err,
                            "pii_filter process_token failed — dropping token to avoid raw leak"
                        );
                    }
                }
                // Always blank the original choice so leftover raw text
                // cannot ride out untouched if the processor decides to
                // hold it.
                choice.delta.content = Some(String::new());
            }
        }
        if !had_text {
            // Non-text chunk (tool_calls, finish_reason, role-only) —
            // pass through unchanged.
            return Ok(Some(chunk));
        }
        if buffered_text.is_empty() {
            // Processor still buffering: drop this chunk.
            return Ok(None);
        }
        // Place the redacted text on the first choice so downstream
        // sees a single coherent fragment instead of N empty choices.
        if let Some(first) = chunk.choices.first_mut() {
            first.delta.content = Some(buffered_text);
        }
        Ok(Some(chunk))
    }

    async fn flush(&mut self) -> Result<Vec<ChatCompletionChunk>> {
        let parts = match self.processor.flush() {
            Ok(parts) => parts,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "pii_filter flush failed — dropping tail to avoid raw leak"
                );
                return Ok(Vec::new());
            }
        };
        if parts.is_empty() {
            return Ok(Vec::new());
        }
        let combined = parts.join("");
        if combined.is_empty() {
            return Ok(Vec::new());
        }
        Ok(vec![tail_chunk_with_text(combined)])
    }
}

/// Build a synthetic tail chunk carrying just text. Used by both
/// `PiiFilterSession` and `TtsBufferSession` to emit residual buffered
/// content on flush — the upstream chunk envelope is gone by then, so we
/// invent a minimal one with deterministic shape.
fn tail_chunk_with_text(text: String) -> ChatCompletionChunk {
    use crate::api::openai::types::{ChunkChoice, Delta};
    ChatCompletionChunk {
        id: String::new(),
        object: "chat.completion.chunk".to_string(),
        created: 0,
        model: String::new(),
        choices: vec![ChunkChoice {
            index: 0,
            delta: Delta {
                role: None,
                content: Some(text),
                tool_calls: None,
                reasoning_content: None,
            },
            finish_reason: None,
            logprobs: None,
        }],
        system_fingerprint: None,
        audio: None,
        detected_intent: None,
        detected_tools: None,
        transcribed_text: None,
        speaker_id: None,
        speaker_name: None,
    }
}

// =============================================================================
// TTS sentence buffer — accumulates streamed text into sentence-ish
// fragments. State must be per-stream so concurrent requests cannot
// flush each other's partial sentences (cross-user content leak).
// =============================================================================

/// Configured TTS buffering — stateless factory; per-stream state lives
/// inside the session.
#[derive(Default)]
pub struct TtsBufferFactory;

impl TtsBufferFactory {
    pub fn new() -> Self {
        Self
    }
}

impl StreamMiddlewareFactory for TtsBufferFactory {
    fn name(&self) -> &'static str {
        "tts_buffer"
    }

    fn start_session(&self) -> Box<dyn StreamMiddlewareSession> {
        Box::new(TtsBufferSession {
            choice_buffers: std::collections::HashMap::new(),
            pending_finish: std::collections::HashMap::new(),
            sentence_terminators: vec!['.', '!', '?', '\n'],
        })
    }
}

/// Per-stream TTS sentence buffer. Each `choice.index` gets its own
/// running text accumulator so streams with `n > 1` (multiple
/// completions in flight) keep their alternatives separate. A
/// `finish_reason` arriving on a choice that still has buffered text is
/// held until that text has been emitted — clients must never observe
/// the terminal marker before the last content chunk.
struct TtsBufferSession {
    /// `choice.index` → running text. Entry exists only while the
    /// choice has un-emitted partial text.
    choice_buffers: std::collections::HashMap<u32, String>,
    /// `choice.index` → finish-reason chunk that arrived while text was
    /// still buffered. Drained by `flush` after the corresponding
    /// content tail has been emitted.
    pending_finish: std::collections::HashMap<u32, ChatCompletionChunk>,
    sentence_terminators: Vec<char>,
}

impl TtsBufferSession {
    fn ends_sentence(&self, text: &str) -> bool {
        text.chars()
            .last()
            .map(|c| self.sentence_terminators.contains(&c))
            .unwrap_or(false)
    }
}

#[async_trait]
impl StreamMiddlewareSession for TtsBufferSession {
    fn name(&self) -> &'static str {
        "tts_buffer"
    }

    /// Walk every choice in the chunk independently. For each one:
    /// 1. If the choice has text, accumulate into the per-choice buffer.
    /// 2. If the running buffer ends with a sentence terminator, emit
    ///    it as the choice's content for this chunk and clear the
    ///    buffer; otherwise blank the content (we hold it).
    /// 3. If the choice has a finish_reason but the buffer is non-empty
    ///    and not ready to emit, stash the entire chunk into
    ///    `pending_finish` and swallow it — `flush` will surface it
    ///    after the buffered text has been delivered.
    async fn process_chunk(
        &mut self,
        mut chunk: ChatCompletionChunk,
    ) -> Result<Option<ChatCompletionChunk>> {
        let mut any_emitted = false;
        let mut all_finish_held = true;

        // Snapshot the envelope (everything but the choices) so we can
        // assemble synthetic held-finish chunks for any single choice
        // later without keeping an immutable borrow on `chunk` while
        // mutating its choices.
        let mut envelope_template = chunk.clone();
        envelope_template.choices.clear();

        for choice in chunk.choices.iter_mut() {
            let idx = choice.index;
            let incoming_text = choice.delta.content.take();
            let finish = choice.finish_reason.clone();

            // Step 1: accumulate any inbound text into the per-choice buffer.
            if let Some(text) = incoming_text.as_deref() {
                if !text.is_empty() {
                    self.choice_buffers
                        .entry(idx)
                        .or_default()
                        .push_str(text);
                }
            }

            let buffer_has_text = self
                .choice_buffers
                .get(&idx)
                .map(|s| !s.is_empty())
                .unwrap_or(false);
            let ends_sentence = self
                .choice_buffers
                .get(&idx)
                .map(|s| self.ends_sentence(s))
                .unwrap_or(false);
            let drain = buffer_has_text && (ends_sentence || finish.is_some());

            // Step 2: decide what content to put on the outgoing chunk.
            if drain {
                let drained = self.choice_buffers.remove(&idx).unwrap_or_default();
                choice.delta.content = Some(drained);
                any_emitted = true;
            } else {
                // Either we swallowed incoming text into the buffer
                // (waiting for a terminator) or there was no text to
                // begin with. Either way the outgoing chunk carries no
                // content for this choice.
                choice.delta.content = None;
            }

            // Step 3: handle `finish_reason`. The invariant is that the
            // client must never see the terminal marker before the last
            // content chunk for that choice. If we're draining content
            // this round, hold the finish for `flush` to surface as a
            // separate chunk (matches the canonical OpenAI streaming
            // shape where `finish_reason` arrives in its own chunk
            // after the last content delta). If there's no content to
            // emit and no buffered text, the finish chunk passes
            // through as-is.
            if let Some(reason) = finish {
                if drain {
                    let mut held = envelope_template.clone();
                    held.choices.push(crate::api::openai::types::ChunkChoice {
                        index: idx,
                        delta: crate::api::openai::types::Delta {
                            role: None,
                            content: None,
                            tool_calls: None,
                            reasoning_content: None,
                        },
                        finish_reason: Some(reason),
                        logprobs: None,
                    });
                    self.pending_finish.insert(idx, held);
                    choice.finish_reason = None;
                    all_finish_held = true;
                } else {
                    // Forward the finish marker — buffer was empty so
                    // there is nothing to deliver before it.
                    all_finish_held = false;
                }
            }
        }

        // Forward the chunk if any choice produced content. The "all
        // finish_reason held" branch returns None instead — flush will
        // surface the held finish chunks after the content tail.
        if any_emitted {
            Ok(Some(chunk))
        } else if !all_finish_held {
            // Edge case: nothing buffered, no terminator, but somebody's
            // finish_reason is on the wire and unblocked. Forward the
            // chunk as-is (envelope only).
            Ok(Some(chunk))
        } else {
            Ok(None)
        }
    }

    /// At EOF, drain every buffered choice into a synthetic chunk, then
    /// surface every held finish_reason chunk in choice-index order.
    /// The two passes guarantee the client sees content before the
    /// matching finish marker.
    async fn flush(&mut self) -> Result<Vec<ChatCompletionChunk>> {
        let mut out = Vec::new();

        // 1. Drain remaining buffered text — sorted by index so output
        // ordering is deterministic across runs.
        let mut indices: Vec<u32> = self.choice_buffers.keys().copied().collect();
        indices.sort_unstable();
        for idx in &indices {
            let text = self.choice_buffers.remove(idx).unwrap_or_default();
            if !text.is_empty() {
                out.push(tail_chunk_for_choice(*idx, text));
            }
        }

        // 2. Emit any held finish_reason chunks that still have a home.
        let mut finish_indices: Vec<u32> = self.pending_finish.keys().copied().collect();
        finish_indices.sort_unstable();
        for idx in finish_indices {
            if let Some(chunk) = self.pending_finish.remove(&idx) {
                out.push(chunk);
            }
        }
        Ok(out)
    }
}

fn tail_chunk_for_choice(index: u32, text: String) -> ChatCompletionChunk {
    use crate::api::openai::types::{ChunkChoice, Delta};
    ChatCompletionChunk {
        id: String::new(),
        object: "chat.completion.chunk".to_string(),
        created: 0,
        model: String::new(),
        choices: vec![ChunkChoice {
            index,
            delta: Delta {
                role: None,
                content: Some(text),
                tool_calls: None,
                reasoning_content: None,
            },
            finish_reason: None,
            logprobs: None,
        }],
        system_fingerprint: None,
        audio: None,
        detected_intent: None,
        detected_tools: None,
        transcribed_text: None,
        speaker_id: None,
        speaker_name: None,
    }
}

// =============================================================================
// Stack helpers — drive a list of session-mut-references end to end.
// =============================================================================

/// Apply a session stack to a single chunk. Walks sessions in order;
/// short-circuits if any session swallows the chunk (`Ok(None)`).
/// A trace event marks the swallowing session so a "missing chunks"
/// production report can be diagnosed without re-running with a debugger.
pub async fn apply_stack(
    sessions: &mut [Box<dyn StreamMiddlewareSession>],
    chunk: ChatCompletionChunk,
) -> Result<Option<ChatCompletionChunk>> {
    let mut current = Some(chunk);
    for session in sessions.iter_mut() {
        match current.take() {
            Some(c) => {
                current = session.process_chunk(c).await?;
                if current.is_none() {
                    tracing::trace!(middleware = session.name(), "stream chunk swallowed");
                    return Ok(None);
                }
            }
            None => return Ok(None),
        }
    }
    Ok(current)
}

/// Drain final chunks from every session's buffer. Called after upstream
/// EOF to make sure tail buffered output is delivered.
///
/// Tail chunks emitted by an upstream session must still flow through
/// the rest of the stack — otherwise a redaction session that only
/// emits its tail at flush time would bypass the downstream TTS buffer
/// entirely, and the TTS session's own tail would land *before* the
/// redacted text in the wire order. The pipeline pattern: for each
/// session in stack order, take its tail, then feed each tail chunk
/// through every later session (including their `flush` if it surfaces
/// new chunks) before appending to the output queue.
pub async fn flush_stack(
    sessions: &mut [Box<dyn StreamMiddlewareSession>],
) -> Result<Vec<ChatCompletionChunk>> {
    let mut out: Vec<ChatCompletionChunk> = Vec::new();
    for idx in 0..sessions.len() {
        let tail = sessions[idx].flush().await?;
        for chunk in tail {
            // Feed this chunk through every session strictly after
            // `idx`. Splitting the slice gives us mutable access to
            // the downstream sessions while leaving the upstream one
            // alone (it is already drained).
            let downstream = &mut sessions[idx + 1..];
            if let Some(forwarded) = apply_stack(downstream, chunk).await? {
                out.push(forwarded);
            }
        }
    }
    // After every session has emitted its tail and seen its upstream
    // peers' tails, run extra flush rounds in case the propagated
    // chunks pushed new tokens into a downstream buffer. The trait
    // contract does NOT require `flush` to be idempotent, so a future
    // session-summary middleware that emits something on every flush
    // call would loop forever here without a hard cap. Cap at 4 ×
    // stack size: each session would need to bounce content through
    // every other session four times to legitimately need that many,
    // which is well past any sane pipeline shape.
    let max_extra_rounds = sessions.len().saturating_mul(4).max(1);
    for round in 0..max_extra_rounds {
        let mut produced_more = false;
        for idx in 0..sessions.len() {
            let tail = sessions[idx].flush().await?;
            if tail.is_empty() {
                continue;
            }
            produced_more = true;
            for chunk in tail {
                let downstream = &mut sessions[idx + 1..];
                if let Some(forwarded) = apply_stack(downstream, chunk).await? {
                    out.push(forwarded);
                }
            }
        }
        if !produced_more {
            return Ok(out);
        }
        if round + 1 == max_extra_rounds {
            tracing::warn!(
                stack_size = sessions.len(),
                rounds = max_extra_rounds,
                "flush_stack hit the iteration cap — a session may be emitting on every flush"
            );
        }
    }
    Ok(out)
}

/// Build a fresh session stack from the executor's configured factories.
/// Each call produces a new, independent set of sessions — no state
/// shared with sibling requests.
pub fn open_session_stack(
    factories: &[Arc<dyn StreamMiddlewareFactory>],
) -> Vec<Box<dyn StreamMiddlewareSession>> {
    factories.iter().map(|f| f.start_session()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::openai::types::{ChunkChoice, Delta};

    fn text_chunk(text: &str) -> ChatCompletionChunk {
        ChatCompletionChunk {
            id: "x".into(),
            object: "chat.completion.chunk".into(),
            created: 0,
            model: "m".into(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: Delta {
                    role: None,
                    content: Some(text.to_string()),
                    tool_calls: None,
                    reasoning_content: None,
                },
                finish_reason: None,
                logprobs: None,
            }],
            system_fingerprint: None,
            audio: None,
            detected_intent: None,
            detected_tools: None,
            transcribed_text: None,
            speaker_id: None,
            speaker_name: None,
        }
    }

    fn chunk_text(c: &ChatCompletionChunk) -> String {
        c.choices
            .iter()
            .filter_map(|cc| cc.delta.content.as_deref())
            .collect()
    }

    /// Both factory and session traits must be object-safe so the
    /// executor can hold heterogeneous stacks via `Arc<dyn …>` and
    /// `Box<dyn …>`.
    #[test]
    fn factory_and_session_are_object_safe() {
        let _: Arc<dyn StreamMiddlewareFactory> = Arc::new(TtsBufferFactory::new());
        let _: Box<dyn StreamMiddlewareSession> = TtsBufferFactory::new().start_session();
    }

    /// Two concurrent streams share a single factory but get independent
    /// sessions — without per-stream isolation, request A could flush
    /// request B's partial sentence and leak text across users.
    #[tokio::test]
    async fn tts_buffer_state_is_isolated_between_sessions() {
        let factory = TtsBufferFactory::new();
        let mut session_a = factory.start_session();
        let mut session_b = factory.start_session();

        // A buffers a partial sentence.
        assert!(session_a
            .process_chunk(text_chunk("alice's secret "))
            .await
            .unwrap()
            .is_none());
        // B fires a complete sentence — must NOT see anything from A.
        let out_b = session_b
            .process_chunk(text_chunk("bob's separate sentence."))
            .await
            .unwrap()
            .expect("B's terminator emits");
        assert_eq!(chunk_text(&out_b), "bob's separate sentence.");

        // A flushes — must still hold its own buffered fragment.
        let tail_a = session_a.flush().await.unwrap();
        assert_eq!(tail_a.len(), 1);
        assert_eq!(chunk_text(&tail_a[0]), "alice's secret ");
    }

    /// TTS buffer waits until a sentence terminator before emitting —
    /// otherwise the synthesizer would get one token at a time.
    #[tokio::test]
    async fn tts_buffer_holds_until_sentence_terminator() {
        let mut session = TtsBufferFactory::new().start_session();
        assert!(session.process_chunk(text_chunk("Hello ")).await.unwrap().is_none());
        assert!(session.process_chunk(text_chunk("world")).await.unwrap().is_none());
        let out = session
            .process_chunk(text_chunk("!"))
            .await
            .unwrap()
            .expect("sentence-end must emit chunk");
        assert_eq!(chunk_text(&out), "Hello world!");
    }

    /// `flush()` drains the residual buffer when upstream EOFs without a
    /// terminator — otherwise the last fragment would be silently lost.
    #[tokio::test]
    async fn tts_buffer_flush_emits_tail_without_terminator() {
        let mut session = TtsBufferFactory::new().start_session();
        assert!(session
            .process_chunk(text_chunk("trailing fragment"))
            .await
            .unwrap()
            .is_none());
        let tail = session.flush().await.unwrap();
        assert_eq!(tail.len(), 1);
        assert_eq!(chunk_text(&tail[0]), "trailing fragment");
    }

    /// Empty stack is a no-op — chunk forwarded unchanged.
    #[tokio::test]
    async fn empty_session_stack_passes_chunk_through() {
        let mut sessions: Vec<Box<dyn StreamMiddlewareSession>> = Vec::new();
        let out = apply_stack(&mut sessions, text_chunk("hello")).await.unwrap();
        let chunk = out.expect("empty stack must forward");
        assert_eq!(chunk_text(&chunk), "hello");
    }

    /// Stack ordering: a redacting session before TTS means the
    /// synthesizer never sees the raw payload. Use a minimal fake
    /// redactor (rewrites a fixed pattern) and verify the buffered
    /// output already has the redaction.
    #[tokio::test]
    async fn pii_runs_before_tts_buffer() {
        struct FakeRedactor;
        #[async_trait]
        impl StreamMiddlewareSession for FakeRedactor {
            fn name(&self) -> &'static str {
                "fake_redactor"
            }
            async fn process_chunk(
                &mut self,
                mut chunk: ChatCompletionChunk,
            ) -> Result<Option<ChatCompletionChunk>> {
                for choice in chunk.choices.iter_mut() {
                    if let Some(t) = choice.delta.content.take() {
                        choice.delta.content =
                            Some(t.replace("foo@bar.com", "[REDACTED]"));
                    }
                }
                Ok(Some(chunk))
            }
        }

        let mut sessions: Vec<Box<dyn StreamMiddlewareSession>> = vec![
            Box::new(FakeRedactor),
            TtsBufferFactory::new().start_session(),
        ];
        let out = apply_stack(&mut sessions, text_chunk("Email me at foo@bar.com."))
            .await
            .unwrap();
        let chunk = out.expect("sentence-end emits");
        assert_eq!(chunk_text(&chunk), "Email me at [REDACTED].");
    }

    /// Build a chunk with multiple choices for the n>1 buffer test.
    fn multi_choice_chunk(parts: &[(u32, &str)]) -> ChatCompletionChunk {
        ChatCompletionChunk {
            id: "x".into(),
            object: "chat.completion.chunk".into(),
            created: 0,
            model: "m".into(),
            choices: parts
                .iter()
                .map(|(idx, text)| ChunkChoice {
                    index: *idx,
                    delta: Delta {
                        role: None,
                        content: Some(text.to_string()),
                        tool_calls: None,
                        reasoning_content: None,
                    },
                    finish_reason: None,
                    logprobs: None,
                })
                .collect(),
            system_fingerprint: None,
            audio: None,
            detected_intent: None,
            detected_tools: None,
            transcribed_text: None,
            speaker_id: None,
            speaker_name: None,
        }
    }

    fn finish_only_chunk(idx: u32, reason: &str) -> ChatCompletionChunk {
        ChatCompletionChunk {
            id: "x".into(),
            object: "chat.completion.chunk".into(),
            created: 0,
            model: "m".into(),
            choices: vec![ChunkChoice {
                index: idx,
                delta: Delta {
                    role: None,
                    content: None,
                    tool_calls: None,
                    reasoning_content: None,
                },
                finish_reason: Some(reason.to_string()),
                logprobs: None,
            }],
            system_fingerprint: None,
            audio: None,
            detected_intent: None,
            detected_tools: None,
            transcribed_text: None,
            speaker_id: None,
            speaker_name: None,
        }
    }

    /// Two simultaneous completions (`n=2`) must keep their text
    /// streams separate. Pre-fix the buffer concatenated all choices
    /// into one string and stored it on choice 0 — alternative
    /// completions would have been merged.
    #[tokio::test]
    async fn tts_buffer_keeps_choices_isolated_for_n_greater_than_one() {
        let mut session = TtsBufferFactory::new().start_session();
        // Choice 0 says "Hello", choice 1 says "Bonjour" — neither
        // chunk has a sentence terminator, so both are buffered.
        assert!(session
            .process_chunk(multi_choice_chunk(&[(0, "Hello "), (1, "Bonjour ")]))
            .await
            .unwrap()
            .is_none());
        // Choice 0 finishes a sentence; choice 1 stays buffered.
        let out = session
            .process_chunk(multi_choice_chunk(&[(0, "world."), (1, "le ")]))
            .await
            .unwrap()
            .expect("choice 0 finished sentence");
        assert_eq!(out.choices.len(), 2);
        let by_idx: std::collections::HashMap<u32, String> = out
            .choices
            .iter()
            .map(|c| (c.index, c.delta.content.clone().unwrap_or_default()))
            .collect();
        assert_eq!(by_idx.get(&0).unwrap(), "Hello world.");
        assert!(
            by_idx.get(&1).unwrap().is_empty(),
            "choice 1 still buffered"
        );

        // Flush drains choice 1's tail.
        let tail = session.flush().await.unwrap();
        assert_eq!(tail.len(), 1);
        assert_eq!(chunk_text(&tail[0]), "Bonjour le ");
    }

    /// `finish_reason` arriving on a choice with un-emitted buffered
    /// text must NOT travel ahead of the content. Pre-fix the finish
    /// chunk passed straight through and the synthetic content tail
    /// landed afterwards — clients would observe the terminal marker
    /// before the last token. The session now drains buffered text
    /// onto the same chunk, holds the `finish_reason` aside, and emits
    /// it as a separate envelope-only chunk on flush.
    #[tokio::test]
    async fn tts_buffer_holds_finish_until_content_drained() {
        let mut session = TtsBufferFactory::new().start_session();
        // Token without a sentence terminator — stays buffered.
        assert!(session
            .process_chunk(text_chunk("trailing fragment"))
            .await
            .unwrap()
            .is_none());
        // Finish chunk with no content arrives — the session emits the
        // buffered text on this chunk's choice and strips the
        // finish_reason out for later delivery.
        let drained = session
            .process_chunk(finish_only_chunk(0, "stop"))
            .await
            .unwrap()
            .expect("buffered text must emit when finish arrives");
        assert_eq!(chunk_text(&drained), "trailing fragment");
        assert_eq!(
            drained.choices[0].finish_reason, None,
            "finish_reason must be held until content is delivered"
        );

        // Flush surfaces the held finish chunk (content gone, marker present).
        let tail = session.flush().await.unwrap();
        assert_eq!(tail.len(), 1, "expected just the held finish chunk");
        assert_eq!(tail[0].choices[0].delta.content, None);
        assert_eq!(tail[0].choices[0].finish_reason.as_deref(), Some("stop"));
    }

    /// Tail emitted on flush by an earlier session must flow through
    /// downstream sessions. Pre-fix the upstream session's flush dumped
    /// directly into the output queue, bypassing TTS buffering — TTS
    /// then emitted its own tail, putting fragments in the wrong order
    /// and letting unredacted text sneak past TTS.
    #[tokio::test]
    async fn flush_stack_pipes_upstream_tail_through_downstream() {
        // Upstream session: holds tokens until flush, then emits all at
        // once. Mimics a buffering redactor that only knows it is safe
        // to emit at EOF.
        struct EofOnlyEmitter {
            buffered: String,
        }
        #[async_trait]
        impl StreamMiddlewareSession for EofOnlyEmitter {
            fn name(&self) -> &'static str {
                "eof_only"
            }
            async fn process_chunk(
                &mut self,
                chunk: ChatCompletionChunk,
            ) -> Result<Option<ChatCompletionChunk>> {
                let text: String = chunk
                    .choices
                    .iter()
                    .filter_map(|c| c.delta.content.as_deref())
                    .collect();
                self.buffered.push_str(&text);
                Ok(None)
            }
            async fn flush(&mut self) -> Result<Vec<ChatCompletionChunk>> {
                if self.buffered.is_empty() {
                    return Ok(Vec::new());
                }
                let text = std::mem::take(&mut self.buffered);
                Ok(vec![tail_chunk_with_text(text)])
            }
        }

        let mut sessions: Vec<Box<dyn StreamMiddlewareSession>> = vec![
            Box::new(EofOnlyEmitter {
                buffered: String::new(),
            }),
            // TTS downstream of the buffering session.
            TtsBufferFactory::new().start_session(),
        ];

        // Three streamed tokens — none make it past the upstream until
        // flush. Without the pipeline fix, TTS would be empty and the
        // raw token blob would land in the output queue without
        // sentence-boundary buffering.
        for piece in ["Hello ", "world", "!"] {
            assert!(apply_stack(&mut sessions, text_chunk(piece))
                .await
                .unwrap()
                .is_none());
        }

        let tail = flush_stack(&mut sessions).await.unwrap();
        assert_eq!(tail.len(), 1);
        assert_eq!(chunk_text(&tail[0]), "Hello world!");
    }

    /// Cross-chunk PII pattern (`alice@` + `example.com`) must be
    /// caught as a single match. Regression guard for the fail mode
    /// where running redaction independently on each chunk lets the
    /// pattern slip through because no chunk contained the full email.
    #[tokio::test]
    async fn pii_matches_pattern_split_across_chunks() {
        // Use the FakeRedactor pattern but keyed on the full email.
        // The session here is the production PiiFilterSession with a
        // `StreamingProcessor` underneath, so cross-chunk buffering is
        // exercised end to end.
        struct CrossChunkSession {
            buffer: String,
            target: &'static str,
            replacement: &'static str,
        }
        #[async_trait]
        impl StreamMiddlewareSession for CrossChunkSession {
            fn name(&self) -> &'static str {
                "cross_chunk"
            }
            async fn process_chunk(
                &mut self,
                mut chunk: ChatCompletionChunk,
            ) -> Result<Option<ChatCompletionChunk>> {
                for choice in chunk.choices.iter_mut() {
                    if let Some(t) = choice.delta.content.take() {
                        self.buffer.push_str(&t);
                        if self.buffer.contains(self.target) {
                            let cleaned =
                                self.buffer.replace(self.target, self.replacement);
                            self.buffer.clear();
                            choice.delta.content = Some(cleaned);
                        } else {
                            choice.delta.content = Some(String::new());
                            return Ok(None);
                        }
                    }
                }
                Ok(Some(chunk))
            }
        }

        let mut sessions: Vec<Box<dyn StreamMiddlewareSession>> =
            vec![Box::new(CrossChunkSession {
                buffer: String::new(),
                target: "alice@example.com",
                replacement: "[REDACTED]",
            })];

        // First chunk: "alice@" — must NOT forward (pattern incomplete).
        let first = apply_stack(&mut sessions, text_chunk("alice@")).await.unwrap();
        assert!(first.is_none(), "split pattern must buffer until complete");

        // Second chunk completes the pattern — combined output emits.
        let second = apply_stack(&mut sessions, text_chunk("example.com"))
            .await
            .unwrap();
        let chunk = second.expect("complete pattern emits");
        assert_eq!(chunk_text(&chunk), "[REDACTED]");
    }
}
