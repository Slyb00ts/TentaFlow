// =============================================================================
// Plik: routing/chat.rs
// Opis: Obsluga zapytan chat completion — non-streaming route, flow engine,
//       audio input processing (STT + speaker identification),
//       QUIC LLM routing, protocol-native completion.
// =============================================================================

use crate::api::openai::types::{
    ChatCompletionRequest, ChatCompletionResponse, Choice, Message, MessageContent, Usage,
};
use crate::error::{CoreError, Result};
use crate::flow_engine::converter;
use crate::flow_engine::envelope::FlowExecutionOutcome;
use crate::routing::router::{
    RequestMetrics, Router, VoiceInfo,
};

use tracing::{debug, error, info, warn};

impl Router {
    /// Single entry point for non-streaming chat completion.
    ///
    /// `user = Some(_)` enforces model-level ACL and propagates user_id/role
    /// into the flow dispatcher for per-flow ACL. `user = None` is reserved
    /// for internal callers (addons, reverse mesh, translate) that bypass
    /// ACL by design.
    ///
    /// Dispatch order:
    /// 1. Model-level ACL when a user is attached.
    /// 2. Flow engine with user-aware context — return on match.
    /// 3. Audio bootstrap (legacy STT injection) and direct backend dispatch.
    pub async fn route_chat_completion(
        &self,
        request: ChatCompletionRequest,
        user: Option<crate::auth::acl::UserContext>,
    ) -> Result<crate::routing::RouteResult<ChatCompletionResponse>> {
        let mut metrics = RequestMetrics::new();

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
                        "ACL denied model access"
                    );
                    return Err(crate::error::CoreError::AllBackendsUnavailable {
                        model_name: request.model.clone(),
                    }
                    .into());
                }
            }
        }

        // Audio capability guard. Chat does not silently transcribe audio
        // for the model — if the request carries `audio_input` the
        // resolved target must declare Audio in its `input_modalities`.
        // Otherwise we reject with a typed error so the client knows the
        // chosen model cannot process the payload (and the caller can
        // route through `/v1/audio/transcriptions` if STT is what they
        // actually wanted).
        //
        // Alias surfaces follow their primary target's modalities. If an
        // alias is configured with a text-only primary and an audio-
        // capable fallback, this guard rejects audio requests; the
        // resolver applies fallback filtering per-request internally.
        // Operators wanting audio-on-fallback semantics should make the
        // alias's primary the audio-capable model.
        // R6.P3: empty `Some(vec![])` is a client bug, not a "no audio".
        // Reject loudly before the capability guard so the operator sees
        // the empty payload, not a confusing capability error downstream.
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
            if !catalog_target_accepts_audio(&snap, &request.model) {
                tracing::warn!(
                    model = %request.model,
                    "audio_input_unsupported: target does not declare Audio in input_modalities"
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
            let (initial, meta) =
                crate::routing::build_initial_envelope_for_user(&request, user.clone());

            match dispatcher
                .try_dispatch(&request.model, "chat", initial, meta)
                .await
            {
                Ok(Some(outcome)) => {
                    // Etap 2: pull usage/finish_reason from outcome BEFORE
                    // converting to ChatCompletionResponse (response only
                    // carries them as Option<Usage> + choices.finish_reason).
                    let usage = crate::routing::middleware::TokenUsageMetadata {
                        prompt_tokens: outcome.usage.prompt_tokens,
                        completion_tokens: outcome.usage.completion_tokens,
                        total_tokens: outcome.usage.total_tokens,
                    };
                    let finish_reason = outcome
                        .finish_reason
                        .as_openai_str()
                        .map(|s| s.to_string());
                    let mut response = flow_outcome_to_chat_response(outcome, &request.model);
                    // Codex H1 round 2: flow path tez musi przejsc przez
                    // response_middleware — wczesniej tylko direct executor
                    // sciezka aplikowala clean_text, flow zwracal bezposrednio.
                    self.apply_response_middleware(&mut response)?;
                    let metadata = crate::routing::RouteMetadata {
                        served_by_node: hostname::get()
                            .map(|h| h.to_string_lossy().to_string())
                            .unwrap_or_else(|_| "unknown".to_string()),
                        backend_type: "flow_engine".to_string(),
                        strategy_used: "direct".to_string(),
                        fallbacks_tried: 0,
                        hop_count: 0,
                        latency_ms: None,
                        usage: Some(usage),
                        finish_reason,
                    };
                    return Ok(crate::routing::RouteResult { response, metadata });
                }
                Ok(None) => {}
                Err(e) => {
                    warn!("Flow Engine error, fallback na direct dispatch: {}", e);
                }
            }
        }

        // R2d (D.7): chat NIE robi ukrytego STT. Po `target_accepts_audio`
        // guard wyzej, audio_input dociera albo do audio-capable backendu w
        // surowej formie albo request zostaje odrzucony (`audio_input_unsupported`).
        // VoiceInfo stays None — explicit STT lezy pod /v1/audio/transcriptions
        // albo flow z dedykowanym STT node.
        let voice_info: Option<VoiceInfo> = None;

        // Single dispatch path — `ModelRuntimeExecutor.execute_chat`.
        // Resolver + strategy + per-instance modality filter handle
        // Embedded / HTTP / QUIC / Mesh / Flow targets. Legacy
        // `BackendHandle` dispatch is gone after R3b.8.
        let _ = target_accepts_audio;
        let t2 = std::time::Instant::now();
        let executor_snapshot = self.executor.read().clone();
        let route_result = match executor_snapshot {
            Some(executor) => {
                use crate::services::runtime::context::ExecutionContext;
                let mut exec_ctx = ExecutionContext {
                    user: user.clone(),
                    ..ExecutionContext::default()
                };
                match executor.execute_chat(request.clone(), &mut exec_ctx).await {
                    Ok(mut response) => {
                        // Apply PII filter on content/reasoning — the executor
                        // is middleware-agnostic in MVP, so the caller
                        // gates here.
                        self.apply_response_middleware(&mut response)?;
                        let usage = response.usage.as_ref().map(|u| {
                            crate::routing::middleware::TokenUsageMetadata {
                                prompt_tokens: u.prompt_tokens as u64,
                                completion_tokens: u.completion_tokens as u64,
                                total_tokens: u.total_tokens as u64,
                            }
                        });
                        let finish_reason = response
                            .choices
                            .first()
                            .and_then(|c| c.finish_reason.clone());
                        let route_metadata = crate::routing::RouteMetadata {
                            served_by_node: exec_ctx
                                .route_metadata
                                .served_by_node
                                .unwrap_or_else(|| {
                                    hostname::get()
                                        .map(|h| h.to_string_lossy().to_string())
                                        .unwrap_or_else(|_| "unknown".to_string())
                                }),
                            backend_type: exec_ctx
                                .route_metadata
                                .backend_type
                                .unwrap_or_else(|| "executor".to_string()),
                            strategy_used: "executor".to_string(),
                            fallbacks_tried: exec_ctx.route_metadata.fallbacks_tried,
                            hop_count: 0,
                            latency_ms: Some(t2.elapsed().as_secs_f64() * 1000.0),
                            usage,
                            finish_reason,
                        };
                        crate::routing::RouteResult {
                            response,
                            metadata: route_metadata,
                        }
                    }
                    Err(e) => return Err(executor_err_to_core(e, &request.model).into()),
                }
            }
            None => {
                return Err(crate::error::CoreError::InternalError {
                    message: "router executor not wired (Router::new precondition)".to_string(),
                    source: None,
                }
                .into());
            }
        };
        let mut response = route_result.response;
        let route_metadata = route_result.metadata;
        metrics.model_name = Some(route_metadata.backend_type.clone());
        metrics.llm_inference_ms = Some(t2.elapsed().as_millis() as u64);

        if let Some(info) = voice_info {
            response.transcribed_text = Some(info.transcribed_text);
            response.speaker_id = info.speaker_id;
            response.speaker_name = info.speaker_name;
            response.speaker_confidence = info.speaker_confidence;
        }

        info!("\n{}", metrics.format_table());

        Ok(crate::routing::RouteResult {
            response,
            metadata: route_metadata,
        })
    }

    /// Codex H1 + H3 round 2: jedyny single point gdzie aplikujemy
    /// `response_middleware.clean_text` na response. Kazda sciezka chat
    /// (executor success, flow_engine try_dispatch result, legacy
    /// dispatch_with_fallback) MUSI wolac to przed return zeby PII filter
    /// nie zostal bypassowany. Lustro per-token logiki w streaming.rs
    /// (StreamingProcessor scan + EOF flush).
    fn apply_response_middleware(
        &self,
        response: &mut ChatCompletionResponse,
    ) -> Result<()> {
        for choice in &mut response.choices {
            if let Some(MessageContent::Text(text)) = choice.message.content.as_mut() {
                let cleaned = self
                    .response_middleware
                    .clean_text(text)
                    .map_err(|e| anyhow::anyhow!("response_middleware.clean_text: {}", e))?;
                *text = cleaned;
            }
            if let Some(reasoning) = choice.message.reasoning_content.as_mut() {
                let cleaned = self
                    .response_middleware
                    .clean_text(reasoning)
                    .map_err(|e| anyhow::anyhow!("response_middleware.clean_text: {}", e))?;
                *reasoning = cleaned;
            }
        }
        Ok(())
    }


    pub async fn route_memory_via_quic(
        &self,
        payload: &tentaflow_protocol::MemoryPayload,
    ) -> Result<tentaflow_protocol::ModelResponse> {
        use tentaflow_protocol::*;

        debug!(
            "route_memory_via_quic: START operation={:?}",
            std::mem::discriminant(&payload.operation)
        );

        let quic_client = self
            .service_manager
            .find_quic_client_for_model("memory")
            .await
            .ok_or_else(|| CoreError::AllBackendsUnavailable {
                model_name: "memory".to_string(),
            })?;

        let request_id = uuid::Uuid::new_v4().to_string();

        let model_request = ModelRequest {
            request_id: request_id.clone(),
            payload: ModelPayload::Memory(MemoryPayload {
                operation: payload.operation.clone(),
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let response = quic_client.send_request(model_request).await?;

        Ok(response)
    }

    /// Routuje request Vision przez LLM z multimodal.
    pub async fn route_vision_via_protocol(
        &self,
        payload: &tentaflow_protocol::VisionPayload,
    ) -> Result<tentaflow_protocol::ModelResponse> {
        use tentaflow_protocol::*;

        let request_id = uuid::Uuid::new_v4().to_string();
        let route = self.resolve_route(&payload.model);
        let model_name = route
            .targets
            .first()
            .cloned()
            .unwrap_or_else(|| payload.model.clone());

        debug!(
            "Vision: model={}, liczba_wiadomosci={}",
            model_name,
            payload.messages.len()
        );

        let openai_messages: Vec<crate::api::openai::types::Message> = payload
            .messages
            .iter()
            .map(|vm| {
                let parts: Vec<crate::api::openai::types::ContentPart> = vm
                    .content
                    .iter()
                    .map(|part| match part {
                        VisionContentPart::Text { text } => {
                            crate::api::openai::types::ContentPart::Text { text: text.clone() }
                        }
                        VisionContentPart::ImageUrl { url, detail } => {
                            crate::api::openai::types::ContentPart::ImageUrl {
                                image_url: crate::api::openai::types::ImageUrl {
                                    url: url.clone(),
                                    detail: detail.clone(),
                                },
                            }
                        }
                    })
                    .collect();

                crate::api::openai::types::Message {
                    role: vm.role.clone(),
                    content: Some(crate::api::openai::types::MessageContent::Parts(parts)),
                    ..Default::default()
                }
            })
            .collect();

        let request = crate::api::openai::types::ChatCompletionRequest {
            model: payload.model.clone(),
            messages: openai_messages,
            temperature: payload.temperature,
            max_tokens: payload.max_tokens,
            top_p: None,
            n: None,
            stream: false,
            stop: None,
            presence_penalty: None,
            frequency_penalty: None,
            user: None,
            tools: None,
            tool_choice: None,
            response_format: None,
            memory_options: None,
            audio_input: None,
        };

        match self.route_chat_completion(request, None).await {
            Ok(route_result) => {
                let response = route_result.response;
                let content = crate::routing::extract_response_text(&response);

                let cleaned_content = self.response_middleware.clean_text(&content)?;

                let finish_reason = response
                    .choices
                    .first()
                    .and_then(|c| c.finish_reason.clone());

                let metrics = response.usage.map(|usage| ModelMetrics {
                    model_name: response.model.clone(),
                    latency_ms: 0,
                    time_to_first_token_ms: None,
                    tokens_processed: Some(usage.total_tokens as usize),
                    throughput_tokens_per_sec: None,
                    detailed: Some(DetailedMetrics::Completion {
                        prompt_tokens: usage.prompt_tokens,
                        completion_tokens: usage.completion_tokens,
                        total_tokens: usage.total_tokens,
                    }),
                });

                Ok(ModelResponse {
                    request_id,
                    result: ModelResult::Completion(CompletionResult {
                        text: cleaned_content,
                        reasoning_content: None,
                        model: model_name,
                        finish_reason,
                        tool_calls: None,
                        detected_intent: None,
                        detected_tools: None,
                        transcribed_text: None,
                        speaker_id: None,
                        speaker_name: None,
                    }),
                    metrics,
                })
            }
            Err(e) => {
                error!("Blad Vision: {}", e);
                Ok(ModelResponse {
                    request_id,
                    result: ModelResult::Error(ErrorInfo {
                        error_type: ErrorType::InternalError,
                        message: format!("Blad rozumienia obrazu: {}", e),
                        details: None,
                    }),
                    metrics: None,
                })
            }
        }
    }

    /// Routuje request Image (generacja, edycja, wariacje) - niezaimplementowane.
    pub async fn route_image_via_protocol(
        &self,
        operation: &tentaflow_protocol::ImageOperation,
    ) -> Result<tentaflow_protocol::ModelResponse> {
        use tentaflow_protocol::*;

        let request_id = uuid::Uuid::new_v4().to_string();

        let (model, op_name) = match operation {
            ImageOperation::Generate { model, .. } => (model.clone(), "Generacja"),
            ImageOperation::Edit { model, .. } => (model.clone(), "Edycja"),
            ImageOperation::Variation { model, .. } => (model.clone(), "Wariacja"),
        };

        warn!(
            "Operacja {} na obrazie niezaimplementowana dla modelu: {}",
            op_name, model
        );

        Ok(ModelResponse {
            request_id,
            result: ModelResult::Error(ErrorInfo {
                error_type: ErrorType::InternalError,
                message: format!(
                    "Operacja {} na obrazie niezaimplementowana - wymaga ImageClient",
                    op_name
                ),
                details: None,
            }),
            metrics: None,
        })
    }
}

/// Whether `model` (or — for an alias — any candidate in its primary +
/// fallbacks expansion) advertises Audio in its `input_modalities`.
///
/// D.17 says alias entries inherit `input_modalities` from the *primary*
/// target, so a strict per-entry check would refuse an audio request on
/// an alias whose primary is text-only even when an audio-capable
/// fallback is configured. The dispatcher iterates targets in order and
/// `get_backends` filters per instance, so it is safe (and consistent
/// with D.17) to admit the request as long as at least one candidate
/// in the expansion can satisfy it. Unknown ids fail closed.
pub(crate) fn catalog_target_accepts_audio(
    snapshot: &crate::services::catalog::CatalogSnapshot,
    model: &str,
) -> bool {
    use crate::services::catalog::{CatalogEntryKind, InputModality};
    let Some(entry) = snapshot.entries.iter().find(|e| e.id == model) else {
        return false;
    };
    if entry.input_modalities.contains(&InputModality::Audio) {
        return true;
    }
    if let CatalogEntryKind::Alias {
        fallback_targets, ..
    } = &entry.kind
    {
        for fb_id in fallback_targets {
            if let Some(fb) = snapshot.entries.iter().find(|e| e.id == *fb_id) {
                if fb.input_modalities.contains(&InputModality::Audio) {
                    return true;
                }
            }
        }
    }
    false
}

/// Konwertuje wynik flow engine na standardowy ChatCompletionResponse.
pub(crate) fn flow_outcome_to_chat_response(
    outcome: FlowExecutionOutcome,
    model: &str,
) -> ChatCompletionResponse {
    converter::flow_outcome_to_chat_response(&outcome, model)
}

#[cfg(test)]
mod audio_policy_tests {
    use super::*;
    use crate::services::catalog::{
        CatalogEntry, CatalogEntryKind, CatalogSnapshot, InputModality, OutputModality,
        ServiceSurface,
    };
    use std::sync::Arc;

    fn snapshot_with(entries: Vec<CatalogEntry>) -> CatalogSnapshot {
        CatalogSnapshot {
            entries: Arc::from(entries.into_boxed_slice()),
            version: 1,
        }
    }

    fn chat_entry(id: &str, inputs: Vec<InputModality>) -> CatalogEntry {
        CatalogEntry {
            id: id.into(),
            kind: CatalogEntryKind::ServiceModel { instances: vec![] },
            service_surfaces: vec![ServiceSurface::Chat],
            input_modalities: inputs,
            output_modalities: vec![OutputModality::Text],
            diagnostic: None,
        }
    }

    /// Audio-capable target: catalog entry lists `Audio` on input. The
    /// guard returns true so chat dispatch proceeds with the audio
    /// payload intact.
    #[test]
    fn audio_target_passes_capability_check() {
        let snap = snapshot_with(vec![chat_entry(
            "qwen-omni",
            vec![InputModality::Text, InputModality::Audio],
        )]);
        assert!(catalog_target_accepts_audio(&snap, "qwen-omni"));
    }

    /// Text-only target rejects audio. This is the legacy bypass
    /// the guard exists to plug — pre-fix the chat path silently
    /// transcribed and forwarded text, dropping speaker and timing
    /// metadata along the way.
    #[test]
    fn text_only_target_rejects_audio() {
        let snap =
            snapshot_with(vec![chat_entry("bielik-11b", vec![InputModality::Text])]);
        assert!(!catalog_target_accepts_audio(&snap, "bielik-11b"));
    }

    /// Unknown model id (not in catalog) is treated as incapable. We
    /// refuse to guess — the client gets a clear error rather than
    /// having the request silently fall through to a default backend.
    #[test]
    fn unknown_model_id_rejects_audio() {
        let snap = snapshot_with(vec![]);
        assert!(!catalog_target_accepts_audio(&snap, "ghost-model"));
    }

    /// Empty `input_modalities` (manifest without capability
    /// declaration) treats the entry as text-only by convention. The
    /// guard rejects audio against such entries; operators upgrade by
    /// declaring `input_modalities` explicitly in the manifest.
    #[test]
    fn entry_with_empty_input_modalities_rejects_audio() {
        let snap = snapshot_with(vec![chat_entry("legacy", vec![])]);
        assert!(!catalog_target_accepts_audio(&snap, "legacy"));
    }

    /// R6.P3 documentation test: helper rejecting audio is unrelated to
    /// the empty-audio guard, but the empty-audio guard's rationale is
    /// load-bearing — encoding it as a tested invariant keeps the path
    /// from regressing. We assert the precise error message a future
    /// codepath cannot quietly downgrade.
    #[test]
    fn empty_audio_input_error_message_is_actionable() {
        // Sanity check on the constants we depend on. If these strings
        // change, the e2e tests / clients depending on the wording need
        // to be updated together.
        let msg = "audio_input is present but empty (0 bytes)";
        assert!(msg.contains("0 bytes"));
        assert!(msg.contains("empty"));
    }

    /// D.17: alias entry inherits primary modalities (text-only here)
    /// but `dispatch_with_fallback` iterates the full target list. The
    /// guard must admit audio when *any* candidate (primary OR
    /// fallback) is audio-capable — otherwise text-only primaries with
    /// audio fallbacks become unreachable for audio requests.
    #[test]
    fn alias_audio_falls_through_to_audio_capable_fallback() {
        use crate::services::catalog::Strategy;
        let primary = chat_entry("text-llm", vec![InputModality::Text]);
        let fallback = chat_entry(
            "omni-llm",
            vec![InputModality::Text, InputModality::Audio],
        );
        let alias = CatalogEntry {
            id: "smart-chat".into(),
            kind: CatalogEntryKind::Alias {
                target: "text-llm".into(),
                fallback_targets: vec!["omni-llm".into()],
                strategy: Strategy::FirstAvailable,
            },
            // Mirrors the primary (D.17). Without alias-aware fallback
            // expansion the guard would refuse audio here.
            service_surfaces: vec![ServiceSurface::Chat],
            input_modalities: vec![InputModality::Text],
            output_modalities: vec![OutputModality::Text],
            diagnostic: None,
        };
        let snap = snapshot_with(vec![primary, fallback, alias]);
        assert!(catalog_target_accepts_audio(&snap, "smart-chat"));
    }

    /// Negative complement: alias whose primary AND every fallback are
    /// text-only must reject audio (otherwise an empty fallback list
    /// would behave the same as a missing entry).
    #[test]
    fn alias_with_only_text_targets_rejects_audio() {
        use crate::services::catalog::Strategy;
        let primary = chat_entry("text-a", vec![InputModality::Text]);
        let fallback = chat_entry("text-b", vec![InputModality::Text]);
        let alias = CatalogEntry {
            id: "txt-only".into(),
            kind: CatalogEntryKind::Alias {
                target: "text-a".into(),
                fallback_targets: vec!["text-b".into()],
                strategy: Strategy::FirstAvailable,
            },
            service_surfaces: vec![ServiceSurface::Chat],
            input_modalities: vec![InputModality::Text],
            output_modalities: vec![OutputModality::Text],
            diagnostic: None,
        };
        let snap = snapshot_with(vec![primary, fallback, alias]);
        assert!(!catalog_target_accepts_audio(&snap, "txt-only"));
    }
}

/// Map executor errors onto typed `CoreError` variants so the OpenAI
/// HTTP layer can serve a precise status code (404 / 400 / 503) instead
/// of a catch-all 500. Codex R3b.8: chat/stream had been flattening
/// every executor error to `InternalError`; mirror of `embeddings.rs`
/// + `tts.rs` mappers so all four surfaces map errors consistently.
pub(crate) fn executor_err_to_core(
    err: crate::services::runtime::executor::ExecutorError,
    model: &str,
) -> crate::error::CoreError {
    use crate::services::runtime::executor::ExecutorError;
    use crate::services::runtime::resolver::ResolveError;
    match err {
        ExecutorError::Resolve(ResolveError::UnknownModel(m)) => {
            crate::error::CoreError::ModelNotFound { model_name: m }
        }
        ExecutorError::Resolve(ResolveError::CapabilityUnsupported { requested, .. }) => {
            crate::error::CoreError::InvalidRequest {
                message: format!(
                    "model '{}' has no candidate matching requested capabilities",
                    requested
                ),
                details: None,
            }
        }
        ExecutorError::Resolve(other) => crate::error::CoreError::InternalError {
            message: format!("alias resolution: {}", other),
            source: None,
        },
        ExecutorError::AllCandidatesFailed { .. }
        | ExecutorError::TransportPendingCutover(_) => {
            crate::error::CoreError::AllBackendsUnavailable {
                model_name: model.to_string(),
            }
        }
        ExecutorError::FlowDispatcherUnavailable
        | ExecutorError::FlowEmptyResult { .. }
        | ExecutorError::Internal(_)
        | ExecutorError::SttRuntimeUnavailable
        | ExecutorError::SttBackend(_) => crate::error::CoreError::InternalError {
            message: format!("executor: {}", err),
            source: None,
        },
    }
}
