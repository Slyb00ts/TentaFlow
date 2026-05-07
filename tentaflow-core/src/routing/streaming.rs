// =============================================================================
// Plik: routing/streaming.rs
// Opis: Streaming SSE — route_chat_completion_stream, route_to_quic_llm_stream.
//       Audio input (STT + speaker ID), PII filtering w strumieniu, TTS
//       buffering.
// =============================================================================

use crate::api::openai::types::{
    ChatCompletionChunk, ChatCompletionRequest, ChunkChoice, Delta, MessageContent,
};
use crate::error::Result;
use crate::routing::chat::flow_outcome_to_chat_response;
use crate::routing::router::Router;

use std::pin::Pin;
use tracing::{debug, warn};

use futures::Stream;

/// Bridge `StreamingExecution.stream` (rkyv `EnvelopeDelta::Llm`) na strumień
/// `ChatCompletionChunk` zgodny z OpenAI SSE. Outcome receiver z executor'a
/// jest spawnowany do background task'a (per plan: routing nie czeka na
/// outcome). Disconnect klienta propaguje się przez `CancelOnDropStream`
/// wstawiony przez SSE wrapper. PII cleaning siedzi w `pii_filter`
/// StreamingNodeAdapter wewnątrz flow_engine (Krok 6) — wire layer już
/// nie filtruje.
fn envelope_stream_to_chunk_stream(
    stream_exec: crate::flow_engine::executor::StreamingExecution,
    model: String,
    include_usage: bool,
) -> std::pin::Pin<
    Box<
        dyn futures::Stream<Item = crate::error::Result<crate::api::openai::types::ChatCompletionChunk>>
            + Send,
    >,
> {
    use crate::api::openai::types::{ChatCompletionChunk, ChunkChoice, Delta, Usage};
    use crate::flow_engine::envelope::{EnvelopeDelta, FlowExecutionOutcome};
    use futures::StreamExt;

    let crate::flow_engine::executor::StreamingExecution { stream, outcome } = stream_exec;
    let id = format!("flow-{}", uuid::Uuid::new_v4());
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    if !include_usage {
        // Pre-Etap-3a path — detached log, brak tail chunk.
        tokio::spawn(async move {
            match outcome.await {
                Ok(o) => tracing::info!(
                    latency_ms = o.total_latency_ms,
                    prompt_tokens = o.usage.prompt_tokens,
                    completion_tokens = o.usage.completion_tokens,
                    error = ?o.error,
                    "flow streaming completed"
                ),
                Err(_) => tracing::warn!("flow finalizer dropped without outcome"),
            }
        });
        let id_for_map = id;
        let model_for_map = model;
        let mapped = stream.map(move |item| match item {
            Ok(EnvelopeDelta::Llm(c)) => Ok(make_chunk(&id_for_map, created, &model_for_map, c)),
            // Etap 3c: chat streaming bridge nigdy nie powinno dostać
            // Audio delta (audio leci przez /v1/audio/speech/stream
            // endpoint, NIE chat stream). Defensywnie mapujemy na error.
            Ok(EnvelopeDelta::Audio(_)) => Err(crate::error::CoreError::InternalError {
                message: "chat stream received Audio delta — flow misconfig".into(),
                source: None,
            }
            .into()),
            Err(e) => Err(crate::error::CoreError::InternalError {
                message: format!("flow stream error: {e}"),
                source: None,
            }
            .into()),
        });
        return Box::pin(mapped);
    }

    // include_usage=true: po stream EOF awaiting outcome, emit tail chunk z usage
    // przed `[DONE]`. State machine pilnuje że tail leci dopiero raz, po EOF.
    let composite = futures::stream::unfold(
        SplitState::Producing {
            stream,
            outcome,
            id,
            created,
            model,
        },
        move |state| async move {
            match state {
                SplitState::Producing {
                    mut stream,
                    outcome,
                    id,
                    created,
                    model,
                } => match stream.next().await {
                    Some(Ok(EnvelopeDelta::Llm(c))) => {
                        let chunk = make_chunk(&id, created, &model, c);
                        Some((
                            Ok(chunk),
                            SplitState::Producing {
                                stream,
                                outcome,
                                id,
                                created,
                                model,
                            },
                        ))
                    }
                    Some(Ok(EnvelopeDelta::Audio(_))) => Some((
                        Err(crate::error::CoreError::InternalError {
                            message: "chat stream received Audio delta — flow misconfig".into(),
                            source: None,
                        }
                        .into()),
                        SplitState::Done,
                    )),
                    Some(Err(e)) => Some((
                        Err(crate::error::CoreError::InternalError {
                            message: format!("flow stream error: {e}"),
                            source: None,
                        }
                        .into()),
                        SplitState::Done,
                    )),
                    None => match outcome.await {
                        Ok(o) => {
                            let tail = build_flow_tail_chunk(&o, &id, created, &model);
                            Some((Ok(tail), SplitState::Done))
                        }
                        Err(_) => {
                            tracing::warn!("flow finalizer dropped without outcome — no usage tail");
                            None
                        }
                    },
                },
                SplitState::Done => None,
            }
        },
    );
    Box::pin(composite)
}

enum SplitState {
    Producing {
        stream: futures::stream::BoxStream<
            'static,
            crate::error::Result<crate::flow_engine::envelope::EnvelopeDelta>,
        >,
        outcome: tokio::sync::oneshot::Receiver<crate::flow_engine::envelope::FlowExecutionOutcome>,
        id: String,
        created: u64,
        model: String,
    },
    Done,
}

fn make_chunk(
    id: &str,
    created: u64,
    model: &str,
    c: crate::flow_engine::envelope::LlmStreamChunk,
) -> crate::api::openai::types::ChatCompletionChunk {
    use crate::api::openai::types::{ChatCompletionChunk, ChunkChoice, Delta};
    ChatCompletionChunk {
        id: id.to_string(),
        object: "chat.completion.chunk".to_string(),
        created,
        model: model.to_string(),
        choices: vec![ChunkChoice {
            // Stage 3d Krok 1: propagate choice_index z LlmStreamChunk
            // (zamiast hardcoded 0). Default 0 dla synthetic + większości
            // backendów; multi-choice n>1 dostaje per-choice value.
            index: c.choice_index,
            delta: Delta {
                role: None,
                content: if c.text_delta.is_empty() {
                    None
                } else {
                    Some(c.text_delta)
                },
                reasoning_content: c.reasoning_delta,
                tool_calls: None,
            },
            finish_reason: c
                .finish_reason
                .and_then(|f| f.as_openai_str().map(|s| s.to_string())),
            logprobs: None,
        }],
        system_fingerprint: None,
        audio: None,
        detected_intent: None,
        detected_tools: None,
        transcribed_text: None,
        speaker_id: None,
        speaker_name: None,
        usage: None,
    }
}

fn build_flow_tail_chunk(
    outcome: &crate::flow_engine::envelope::FlowExecutionOutcome,
    id: &str,
    created: u64,
    model: &str,
) -> crate::api::openai::types::ChatCompletionChunk {
    use crate::api::openai::types::{ChatCompletionChunk, Usage};
    ChatCompletionChunk {
        id: id.to_string(),
        object: "chat.completion.chunk".to_string(),
        created,
        model: model.to_string(),
        choices: vec![],
        system_fingerprint: None,
        audio: None,
        detected_intent: None,
        detected_tools: None,
        transcribed_text: None,
        speaker_id: None,
        speaker_name: None,
        usage: Some(Usage {
            prompt_tokens: outcome.usage.prompt_tokens as u32,
            completion_tokens: outcome.usage.completion_tokens as u32,
            total_tokens: outcome.usage.total_tokens as u32,
        }),
    }
}

/// Etap 3a: state machine która patrzy na `chunk.usage` (stemplowane przez
/// executor.rs::Done arm gdy backend dostarczył DetailedMetrics::Completion).
/// Decyduje per `include_usage` jak wykorzystać:
/// - `false` (default, back-compat): strip `usage` z chunk'u przed wireem.
///   Klient nie prosił, pole nigdy się nie pokazuje.
/// - `true`: emit chunk z `usage: None` (regular finish chunk per OpenAI
///   contract), POTEM emit dodatkowy tail chunk z `choices: []` + `usage`.
///   Dwa chunki z jednego źródłowego (OpenAI requirement).
///
/// Wszystkie chunki bez `usage` (regularne content delta) przepuszczane bez
/// modyfikacji.
fn apply_include_usage_split<S>(
    inner: S,
    include_usage: bool,
) -> std::pin::Pin<
    Box<
        dyn futures::Stream<
                Item = crate::error::Result<crate::api::openai::types::ChatCompletionChunk>,
            > + Send,
    >,
>
where
    S: futures::Stream<Item = crate::error::Result<crate::api::openai::types::ChatCompletionChunk>>
        + Send
        + 'static,
{
    use crate::api::openai::types::ChatCompletionChunk;
    use futures::StreamExt;

    let inner = Box::pin(inner)
        as std::pin::Pin<
            Box<
                dyn futures::Stream<
                        Item = crate::error::Result<ChatCompletionChunk>,
                    > + Send,
            >,
        >;

    let composite = futures::stream::unfold(
        UsageSplitState::Active { inner, include_usage },
        |state| async move {
            match state {
                UsageSplitState::Active {
                    mut inner,
                    include_usage,
                } => {
                    let next = match inner.next().await {
                        Some(Ok(c)) => c,
                        Some(Err(e)) => return Some((Err(e), UsageSplitState::Done)),
                        None => return None,
                    };
                    if next.usage.is_none() {
                        // Regular chunk — przepuszczamy bez zmian.
                        return Some((
                            Ok(next),
                            UsageSplitState::Active { inner, include_usage },
                        ));
                    }
                    // Chunk niesie usage. Decyzja per flag.
                    if !include_usage {
                        // Klient nie prosił — strip usage, emit chunk.
                        let mut stripped = next;
                        stripped.usage = None;
                        return Some((
                            Ok(stripped),
                            UsageSplitState::Active { inner, include_usage },
                        ));
                    }
                    // include_usage=true: split na finish chunk + tail.
                    let metrics = next.usage.clone();
                    let mut finish_chunk = next;
                    finish_chunk.usage = None;
                    let tail = ChatCompletionChunk {
                        id: finish_chunk.id.clone(),
                        object: "chat.completion.chunk".to_string(),
                        created: finish_chunk.created,
                        model: finish_chunk.model.clone(),
                        choices: vec![],
                        system_fingerprint: None,
                        audio: None,
                        detected_intent: None,
                        detected_tools: None,
                        transcribed_text: None,
                        speaker_id: None,
                        speaker_name: None,
                        usage: metrics,
                    };
                    Some((
                        Ok(finish_chunk),
                        UsageSplitState::EmitTail { tail, inner, include_usage },
                    ))
                }
                UsageSplitState::EmitTail {
                    tail,
                    inner,
                    include_usage,
                } => Some((
                    Ok(tail),
                    UsageSplitState::Active { inner, include_usage },
                )),
                UsageSplitState::Done => None,
            }
        },
    );
    Box::pin(composite)
}

enum UsageSplitState {
    Active {
        inner: std::pin::Pin<
            Box<
                dyn futures::Stream<
                        Item = crate::error::Result<crate::api::openai::types::ChatCompletionChunk>,
                    > + Send,
            >,
        >,
        include_usage: bool,
    },
    EmitTail {
        tail: crate::api::openai::types::ChatCompletionChunk,
        inner: std::pin::Pin<
            Box<
                dyn futures::Stream<
                        Item = crate::error::Result<crate::api::openai::types::ChatCompletionChunk>,
                    > + Send,
            >,
        >,
        include_usage: bool,
    },
    Done,
}


impl Router {
    /// Routuje chat completion request (STREAMING MODE) przez flow_engine
    /// (stage 3d Universal Flow Gateway). Synthetic streaming flow `trigger
    /// → llm(model) → output(stream)` aktywuje się gdy admin nie
    /// skonfigurował user-defined flow. User-defined blocking-only flow
    /// jest opakowywany w single-chunk stream (wrapper sync→stream w
    /// FlowDispatcher::try_dispatch_streaming). PII cleaning idzie przez
    /// `pii_filter` StreamingNodeAdapter wewnątrz flow_engine — synthetic
    /// chat-stream wstrzykuje node domyślnie; user-defined flow musi mieć
    /// node explicite (R-SAFETY w Krok 7).
    pub async fn route_chat_completion_stream(
        &self,
        request: ChatCompletionRequest,
        user: Option<crate::auth::acl::UserContext>,
    ) -> Result<
        crate::routing::RouteResult<
            Pin<Box<dyn Stream<Item = Result<ChatCompletionChunk>> + Send>>,
        >,
    > {
        let stream_start = std::time::Instant::now();
        let stream_node_name = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        if let Some(ref u) = user {
            if let Some(ref db) = self.db {
                if !crate::auth::acl::check_access_safe(
                    db,
                    "model",
                    &request.model,
                    u.user_id,
                    &u.role,
                ) {
                    tracing::warn!(
                        user_id = u.user_id,
                        model = %request.model,
                        "ACL denied chat-stream model"
                    );
                    return Err(crate::error::CoreError::ModelNotFound {
                        model_name: request.model.clone(),
                    }
                    .into());
                }
            }
        }

        // Audio capability guard — mirror of the non-streaming path. Without
        // it a client rejected on `POST /v1/chat/completions` could flip
        // `stream:true` and reach the legacy hidden-STT flow below, which
        // the unified catalog explicitly forbids. Alias surfaces follow
        // their primary target's modalities; an alias whose primary is
        // text-only rejects audio even when an audio-capable fallback
        // exists — fallbacks are filtered per-request inside the resolver,
        // not at the handler boundary.
        // R6.P3: empty `Some(vec![])` is a client bug — reject before
        // capability guard so the operator sees the empty payload, not
        // a confusing capability error downstream.
        if let Some(ref bytes) = request.audio_input {
            if bytes.is_empty() {
                return Err(crate::error::CoreError::InvalidRequest {
                    message: "audio_input is present but empty (0 bytes)".to_string(),
                    details: Some(
                        "Send a non-empty audio payload or omit audio_input entirely.".to_string(),
                    ),
                }
                .into());
            }
        }
        let target_accepts_audio = if request.audio_input.is_some() {
            let snap = self.catalog_snapshot();
            if !crate::routing::chat::catalog_target_accepts_audio(&snap, &request.model) {
                tracing::warn!(
                    model = %request.model,
                    "audio_input_unsupported (streaming): target does not declare Audio in input_modalities"
                );
                return Err(crate::error::CoreError::InvalidRequest {
                    message: format!(
                        "audio_input_unsupported: model '{}' does not accept audio input",
                        request.model
                    ),
                    details: Some(
                        "Use /v1/audio/transcriptions for STT, or pick a model with audio_input capability"
                            .to_string(),
                    ),
                }
                .into());
            }
            true
        } else {
            false
        };

        // === FLOW ENGINE: proba wykonania przez konfigurowalny flow ===
        if let Some(ref dispatcher) = self.flow_dispatcher {
            let blobs = dispatcher.blobs();
            // Najpierw streamowa sciezka — tylko gdy flow ma edge from_port="stream".
            let (initial_stream, meta_stream) =
                crate::routing::build_initial_envelope_for_user(
                    &request,
                    user.clone(),
                    &blobs,
                )
                .await?;
            // Disconnect bridge: ten sam cancel_token co w meta dostaje
            // CancelOnDropStream poniżej, więc gdy hyper droppuje SSE body
            // (klient się rozłączył), token zostaje cancelled i finalizer
            // executor'a zauważa to przez biased select! (R7 plan).
            let stream_cancel = meta_stream.cancel_token.clone();
            match dispatcher
                .try_dispatch_streaming(&request.model, "chat", initial_stream, meta_stream)
                .await
            {
                Ok(stream_exec) => {
                    let model_for_stream = request.model.clone();
                    let include_usage = request
                        .stream_options
                        .as_ref()
                        .map(|so| so.include_usage)
                        .unwrap_or(false);
                    let chunk_stream = envelope_stream_to_chunk_stream(
                        stream_exec,
                        model_for_stream,
                        include_usage,
                    );
                    // PII cleaning idzie teraz przez `pii_filter`
                    // StreamingNodeAdapter wewnątrz flow_engine — wire
                    // layer już nie filtruje. Synthetic chat-stream
                    // automatycznie wpina pii_filter; user-defined flowy
                    // muszą deklarować node explicite (R-SAFETY w Krok 7).
                    let filtered = chunk_stream;
                    let cancel_wrapped: std::pin::Pin<
                        Box<
                            dyn futures::Stream<
                                    Item = crate::error::Result<
                                        crate::api::openai::types::ChatCompletionChunk,
                                    >,
                                > + Send,
                        >,
                    > = Box::pin(crate::flow_engine::cancel_on_drop::CancelOnDropStream::new(
                        filtered,
                        stream_cancel,
                    ));
                    let filtered = cancel_wrapped;
                    let metadata = crate::routing::RouteMetadata {
                        served_by_node: stream_node_name.clone(),
                        backend_type: "flow_engine_stream".to_string(),
                        strategy_used: "direct".to_string(),
                        fallbacks_tried: 0,
                        hop_count: 0,
                        latency_ms: Some(stream_start.elapsed().as_secs_f64() * 1000.0),
                    usage: None,
                    finish_reason: None,
                    };
                    return Ok(crate::routing::RouteResult {
                        response: filtered,
                        metadata,
                    });
                }
                Err(e) => {
                    return Err(crate::routing::dispatch_error_to_core(e, &request.model).into());
                }
            }
        }

        // Stage 3d-0b-final: brak flow_dispatcher (DB-less router) → 500.
        // Plan v1.5 wymaga że KAŻDY chat streaming request przechodzi przez
        // flow_engine (synthetic streaming albo user-defined flow). Direct
        // executor.stream_chat fallback wycięty — Universal Flow Gateway
        // jest jedyną ścieżką dispatch.
        let _ = target_accepts_audio;
        let _ = stream_start;
        let _ = stream_node_name;
        Err(crate::error::CoreError::InternalError {
            message: "flow_dispatcher not wired (DB-less router) — chat streaming \
                      path requires Universal Flow Gateway"
                .to_string(),
            source: None,
        }
        .into())
    }
}

#[cfg(test)]
mod include_usage_tests {
    use super::*;
    use crate::api::openai::types::{
        ChatCompletionChunk, ChunkChoice, Delta, Usage,
    };
    use futures::StreamExt;

    fn chunk_with_usage(text: &str, finish: bool, usage: Option<Usage>) -> ChatCompletionChunk {
        ChatCompletionChunk {
            id: "id1".into(),
            object: "chat.completion.chunk".into(),
            created: 0,
            model: "m".into(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: Delta {
                    role: None,
                    content: Some(text.into()),
                    reasoning_content: None,
                    tool_calls: None,
                },
                finish_reason: if finish {
                    Some("stop".into())
                } else {
                    None
                },
                logprobs: None,
            }],
            system_fingerprint: None,
            audio: None,
            detected_intent: None,
            detected_tools: None,
            transcribed_text: None,
            speaker_id: None,
            speaker_name: None,
            usage,
        }
    }

    /// include_usage=false strips usage z finish chunk'u (back compat).
    #[tokio::test]
    async fn split_false_strips_usage_from_finish_chunk() {
        let usage = Some(Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        });
        let chunks = vec![
            Ok(chunk_with_usage("hello", false, None)),
            Ok(chunk_with_usage("", true, usage)),
        ];
        let inner = futures::stream::iter(chunks);
        let mut out = apply_include_usage_split(inner, false);
        let c1 = out.next().await.unwrap().unwrap();
        assert_eq!(c1.choices[0].delta.content.as_deref(), Some("hello"));
        assert!(c1.usage.is_none());
        let c2 = out.next().await.unwrap().unwrap();
        assert_eq!(c2.choices[0].finish_reason.as_deref(), Some("stop"));
        assert!(c2.usage.is_none(), "usage stripped when include_usage=false");
        assert!(out.next().await.is_none());
    }

    /// include_usage=true splits finish chunk na regular finish + dodatkowy tail.
    #[tokio::test]
    async fn split_true_emits_tail_chunk_with_usage() {
        let usage = Some(Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        });
        let chunks = vec![
            Ok(chunk_with_usage("hi", false, None)),
            Ok(chunk_with_usage("", true, usage.clone())),
        ];
        let inner = futures::stream::iter(chunks);
        let mut out = apply_include_usage_split(inner, true);
        // 1: regular content
        let c1 = out.next().await.unwrap().unwrap();
        assert_eq!(c1.choices[0].delta.content.as_deref(), Some("hi"));
        assert!(c1.usage.is_none());
        // 2: finish chunk z usage=None (split)
        let c2 = out.next().await.unwrap().unwrap();
        assert_eq!(c2.choices[0].finish_reason.as_deref(), Some("stop"));
        assert!(c2.usage.is_none());
        // 3: tail chunk z choices:[] + usage:Some
        let c3 = out.next().await.unwrap().unwrap();
        assert!(c3.choices.is_empty());
        assert_eq!(c3.usage.as_ref().unwrap().total_tokens, 15);
        assert!(out.next().await.is_none());
    }

    /// Brak usage na chunkach = wszystkie przepuszczone bez zmian.
    #[tokio::test]
    async fn split_passthrough_when_no_usage_stamped() {
        let chunks = vec![
            Ok(chunk_with_usage("a", false, None)),
            Ok(chunk_with_usage("", true, None)),
        ];
        let inner = futures::stream::iter(chunks);
        let collected: Vec<_> = apply_include_usage_split(inner, true)
            .collect::<Vec<_>>()
            .await;
        assert_eq!(collected.len(), 2);
    }

}
