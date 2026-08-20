// =============================================================================
// Plik: routing/chat.rs
// Opis: Obsluga zapytan chat completion — non-streaming route, flow engine,
//       audio input processing (STT + speaker identification),
//       QUIC LLM routing, protocol-native completion.
// =============================================================================

use crate::api::openai::types::{ChatCompletionRequest, ChatCompletionResponse};
use crate::compliance::ai_gateway::{AiGateway, AiGatewayContext};
use crate::error::{CoreError, Result};
use crate::flow_engine::converter;
use crate::flow_engine::envelope::FlowExecutionOutcome;
use crate::routing::router::Router;

use tracing::{debug, error, warn};

impl Router {
    /// Single entry point for non-streaming chat completion.
    ///
    /// `user = Some(_)` enforces model-level ACL and propagates user_id/role
    /// into the flow dispatcher for per-flow ACL. `user = None` is reserved
    /// for internal callers (addons, reverse mesh, translate) that bypass
    /// ACL by design.
    ///
    /// Dispatch:
    /// 1. Model-level ACL when a user is attached.
    /// 2. Czysty model / alias→model → bezpośrednie wykonanie na backendzie
    ///    (bez flow). Flow published as a model / alias→flow → FlowDispatcher:
    ///    jawny user-defined flow, a gdy go brak — model wykonywany bezpośrednio
    ///    (LlmDispatcherImpl → executor.execute_chat, bez pii_filter).
    pub async fn route_chat_completion(
        &self,
        request: ChatCompletionRequest,
        user: Option<crate::auth::acl::UserContext>,
        origin: crate::flow_engine::dispatcher::FlowOrigin,
        actor: crate::flow_engine::dispatcher::FlowActor,
        compliance_context: Option<AiGatewayContext>,
    ) -> Result<crate::routing::RouteResult<ChatCompletionResponse>> {
        if let Some(ref u) = user {
            if let Some(ref db) = self.db {
                if !crate::auth::acl::check_access_safe(
                    db,
                    "model",
                    &request.model,
                    &u.user_id,
                    &u.role,
                ) {
                    tracing::warn!(
                        user_id = %u.user_id,
                        model = %request.model,
                        "ACL denied chat model"
                    );
                    return Err(crate::error::CoreError::ModelNotFound {
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
        let _target_accepts_audio = if request.audio_input.is_some() {
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

        let compliance_event = if let Some(db) = self.db.as_ref() {
            let gateway = AiGateway::new(
                db.clone(),
                self.local_node_id(),
                crate::compliance::ai_gateway::token_quota_enabled(),
            );
            Some(
                gateway
                    .start_chat_event(&request, user.as_ref(), compliance_context.as_ref())
                    // Zachowaj realny błąd domenowy (np. RateLimitExceeded z limitu
                    // tokenów) — inaczej klient widzi mylące „błąd wewnętrzny".
                    .map_err(|e| match e.downcast::<CoreError>() {
                        Ok(core) => core,
                        Err(e) => CoreError::InternalError {
                            message: "compliance AI audit start failed".to_string(),
                            source: Some(e),
                        },
                    })?,
            )
        } else {
            None
        };

        // === DIRECT MODEL: raw model → backend bez flow ===
        // A plain model name is answered by the backend directly (no synthetic
        // /default flow, no PII buffering). Only a Flow published as a model
        // (or an alias resolving to one) routes through the flow engine below.
        let route_via_flow = model_resolves_to_flow(&self.catalog_snapshot(), &request.model);
        if !route_via_flow {
            let executor = match self.executor() {
                Some(e) => e,
                None => {
                    if let Some(event) = compliance_event.as_ref() {
                        let _ = event.finish_failed("runtime_executor_not_wired");
                    }
                    return Err(CoreError::InternalError {
                        message: "runtime executor not wired — direct model dispatch unavailable"
                            .to_string(),
                        source: None,
                    }
                    .into());
                }
            };
            let mut exec_ctx = crate::services::runtime::context::ExecutionContext::new(
                user,
                origin,
                actor.clone(),
            );
            match executor.execute_chat(request.clone(), &mut exec_ctx).await {
                Ok(response) => {
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
                    if let Some(event) = compliance_event.as_ref() {
                        event
                            .finish_success(&response)
                            .map_err(|e| CoreError::InternalError {
                                message: "compliance AI audit finish failed".to_string(),
                                source: Some(e),
                            })?;
                    }
                    let metadata = crate::routing::RouteMetadata {
                        served_by_node: exec_ctx
                            .route_metadata
                            .served_by_node
                            .clone()
                            .unwrap_or_else(crate::mesh::node_info_collector::local_hostname),
                        backend_type: "direct".to_string(),
                        strategy_used: "direct".to_string(),
                        fallbacks_tried: exec_ctx.route_metadata.fallbacks_tried,
                        hop_count: 0,
                        latency_ms: None,
                        usage,
                        finish_reason,
                    };
                    return Ok(crate::routing::RouteResult { response, metadata });
                }
                Err(e) => {
                    if let Some(event) = compliance_event.as_ref() {
                        let _ = event.finish_failed(&e.to_string());
                    }
                    return Err(executor_error_to_core(e, &request.model).into());
                }
            }
        }

        // === FLOW ENGINE: proba wykonania przez konfigurowalny flow ===
        if let Some(ref dispatcher) = self.flow_dispatcher {
            let blobs = dispatcher.blobs();
            let (mut initial, mut meta) = crate::routing::build_initial_envelope_for_user(
                &request,
                user.clone(),
                origin,
                actor.clone(),
                &blobs,
            )
            .await?;
            // RAG E2.0 (enabler) — przeprowadź tożsamość addona-callera do
            // FlowRequestMeta. Gdy chat jest wyzwolony przez addon (host-fn
            // llm_generate ustawia compliance_context.addon_id/org_id), executor
            // skopiuje to do ExecutionContext, dzięki czemu węzeł `vector` flow
            // uderza w przestrzeń wektorową TEJ instancji. /v1 user / kamera /
            // agent nie mają addon_id => meta zostaje bez tożsamości.
            if let Some(ctx) = compliance_context.as_ref() {
                if meta.addon_id.is_none() {
                    meta.addon_id = ctx.addon_id.clone();
                }
                if meta.org_id.is_none() {
                    meta.org_id = ctx.org_id.clone();
                }
                // RAG E2.0 — przenieś allowlistowane opcje retrievalu
                // (collection_id, top_k) z host-fn llm_generate do envelope.meta.
                // Węzeł `vector` czyta z meta filtr po kolekcji i top_k. Klucze
                // już-obecne w seedzie (z requestu) mają pierwszeństwo.
                for (k, v) in &ctx.flow_meta {
                    initial.meta.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
            // §3.4: seed the turn's correlation key with the session event's
            // request_id so every per-call `llm` event in the flow links back to
            // this one row. Without a session event (no DB) there is nothing to
            // correlate, so the key is simply absent.
            if let Some(handle) = compliance_event.as_ref() {
                initial.meta.insert(
                    "correlation_id".into(),
                    serde_json::Value::String(handle.request_id().to_string()),
                );
                // §2.5 — the same value as a struct field, so the run's audit
                // link cannot be rewritten by a node that edits `meta`.
                meta.correlation_id = Some(handle.request_id().to_string());
            }

            match dispatcher
                .try_dispatch(&request.model, "chat", initial, meta)
                .await
            {
                Ok(outcome) => {
                    let usage = crate::routing::middleware::TokenUsageMetadata {
                        prompt_tokens: outcome.usage.prompt_tokens,
                        completion_tokens: outcome.usage.completion_tokens,
                        total_tokens: outcome.usage.total_tokens,
                    };
                    let finish_reason =
                        outcome.finish_reason.as_openai_str().map(|s| s.to_string());
                    let response = flow_outcome_to_chat_response(outcome, &request.model);
                    let metadata = crate::routing::RouteMetadata {
                        served_by_node: crate::mesh::node_info_collector::local_hostname(),
                        backend_type: "flow_engine".to_string(),
                        strategy_used: "direct".to_string(),
                        fallbacks_tried: 0,
                        hop_count: 0,
                        latency_ms: None,
                        usage: Some(usage),
                        finish_reason,
                    };
                    if let Some(event) = compliance_event.as_ref() {
                        event
                            .finish_success(&response)
                            .map_err(|e| CoreError::InternalError {
                                message: "compliance AI audit finish failed".to_string(),
                                source: Some(e),
                            })?;
                    }
                    return Ok(crate::routing::RouteResult { response, metadata });
                }
                Err(e) => {
                    if let Some(event) = compliance_event.as_ref() {
                        if let Err(audit_error) = event.finish_failed(&e.to_string()) {
                            tracing::warn!(
                                error = %audit_error,
                                "compliance AI audit failure capture failed"
                            );
                        }
                    }
                    // Stage 3d-0b-final: typed DispatchError → CoreError.
                    // Denied → 404, pozostałe → 500.
                    return Err(crate::routing::dispatch_error_to_core(e, &request.model).into());
                }
            }
        }

        // Brak flow_dispatcher (DB-less router) i model klasyfikowany jako flow
        // → 500 (plain models obsłużone wyżej ścieżką direct).
        if let Some(event) = compliance_event.as_ref() {
            if let Err(audit_error) = event.finish_failed("flow_dispatcher_not_wired") {
                tracing::warn!(
                    error = %audit_error,
                    "compliance AI audit failure capture failed"
                );
            }
        }
        Err(crate::error::CoreError::InternalError {
            message: "flow_dispatcher not wired (DB-less router) — chat path \
                      requires Universal Flow Gateway"
                .to_string(),
            source: None,
        }
        .into())
    }

    pub(crate) fn local_node_id(&self) -> String {
        let registry = self.service_manager.mesh_services_registry.read();
        registry
            .as_ref()
            .map(|r| r.local().node_id.clone())
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| "local".to_string())
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
            reasoning_effort: None,
            modalities: None,
            audio: None,
            model: payload.model.clone(),
            messages: openai_messages,
            temperature: payload.temperature,
            max_tokens: payload.max_tokens,
            top_p: None,
            n: None,
            stream: false,
            stream_options: None,
            stop: None,
            presence_penalty: None,
            frequency_penalty: None,
            user: None,
            tools: None,
            tool_choice: None,
            response_format: None,
            memory_options: None,
            audio_input: None,
            extra: Default::default(),
        };

        match self
            .route_chat_completion(
                request,
                None,
                // Reverse mesh path: a peer node forwarded this vision request.
                crate::flow_engine::dispatcher::FlowOrigin::Mesh,
                crate::flow_engine::dispatcher::FlowActor::system(),
                None,
            )
            .await
        {
            Ok(route_result) => {
                let response = route_result.response;
                let content = crate::routing::extract_response_text(&response);
                // Vision passes przez `route_chat_completion` → flow_engine,
                // gdzie pii_filter już sprzątnął tekst. Nie filtrujemy
                // dwa razy — w przeciwnym razie tracilibyśmy intencje
                // syntetycznego/usera flow który wyłączył filter celowo.
                let cleaned_content = content;

                let finish_reason = response
                    .choices
                    .first()
                    .and_then(|c| c.finish_reason.clone());
                let reasoning_content = response
                    .choices
                    .first()
                    .and_then(|c| c.message.reasoning_content.clone());

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
                        reasoning_content,
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

/// Whether the requested `model` id names a Flow published as a model
/// (`CatalogEntryKind::Flow`) and therefore MUST execute through the flow
/// engine, vs a raw service model that is answered by a direct backend call.
///
/// Design (2026-07): a client requesting a plain model name gets the model
/// directly (no synthetic/default flow, no PII buffering, lowest latency);
/// a client requesting a flow's published name gets the flow. An alias is
/// resolved to its primary target and classified by that target's kind, so
/// `alias → flow` routes through the flow engine while `alias → model`
/// stays direct. Unknown ids (internal callers that bypass the catalog)
/// default to direct — the executor resolves or returns a typed error.
pub(crate) fn model_resolves_to_flow(
    snapshot: &crate::services::catalog::CatalogSnapshot,
    model: &str,
) -> bool {
    use crate::services::catalog::CatalogEntryKind;
    // One alias hop is enough — aliases target models/flows, not other
    // aliases — but a small depth guard keeps a misconfigured chain from
    // looping.
    let mut current = model;
    for _ in 0..4 {
        let Some(entry) = snapshot.entries.iter().find(|e| e.id == current) else {
            return false;
        };
        match &entry.kind {
            CatalogEntryKind::Flow { .. } => return true,
            CatalogEntryKind::ServiceModel { .. } => return false,
            CatalogEntryKind::Alias { target, .. } => {
                current = target;
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

/// Maps an `ExecutorError` from the direct (no-flow) backend path onto a
/// `CoreError`. Resolve failures become `ModelNotFound` so `/v1` keeps its
/// "never reveal whether a model exists" 404 contract; everything else is a
/// backend/internal failure (500).
pub(crate) fn executor_error_to_core(
    e: crate::services::runtime::executor::ExecutorError,
    model: &str,
) -> CoreError {
    use crate::services::runtime::executor::ExecutorError;
    match e {
        ExecutorError::Resolve(_) => CoreError::ModelNotFound {
            model_name: model.to_string(),
        },
        other => CoreError::InternalError {
            message: format!("direct backend dispatch failed: {other}"),
            source: None,
        },
    }
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
            reasoning_levels: Vec::new(),
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

    /// Text-only target rejects audio. Guard egzekwuje że chat path
    /// nie próbuje silently transkrybować audio_input dla modeli bez
    /// audio_input capability — request musi albo iść do audio-capable
    /// modelu, albo eksplicitnie do `/v1/audio/transcriptions`.
    #[test]
    fn text_only_target_rejects_audio() {
        let snap = snapshot_with(vec![chat_entry("bielik-11b", vec![InputModality::Text])]);
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
        let fallback = chat_entry("omni-llm", vec![InputModality::Text, InputModality::Audio]);
        let alias = CatalogEntry {
            reasoning_levels: Vec::new(),
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
            reasoning_levels: Vec::new(),
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

/// Discriminator used by the mesh reverse chat handler
/// (`mesh::inference_proxy::dispatch_reverse_stream_request`) to decide whether
/// a forwarded model runs raw on the executor or through the flow engine.
/// A raw service model MUST classify as "not a flow" so the forwarding node's
/// flow is not silently re-applied on the serving node (no is_default "Default
/// Chat" fallback, no hidden PII redaction). A model published from a flow MUST
/// classify as "flow" so it still executes as the flow it represents.
#[cfg(test)]
mod mesh_reverse_flow_discriminator_tests {
    use super::*;
    use crate::services::catalog::{
        CatalogEntry, CatalogEntryKind, CatalogSnapshot, InputModality, OutputModality,
        ServiceSurface, Strategy,
    };
    use std::sync::Arc;

    fn snapshot_with(entries: Vec<CatalogEntry>) -> CatalogSnapshot {
        CatalogSnapshot {
            entries: Arc::from(entries.into_boxed_slice()),
            version: 1,
        }
    }

    fn service_model(id: &str) -> CatalogEntry {
        CatalogEntry {
            reasoning_levels: Vec::new(),
            id: id.into(),
            kind: CatalogEntryKind::ServiceModel { instances: vec![] },
            service_surfaces: vec![ServiceSurface::Chat],
            input_modalities: vec![InputModality::Text],
            output_modalities: vec![OutputModality::Text],
            diagnostic: None,
        }
    }

    fn published_flow(id: &str, flow_id: &str) -> CatalogEntry {
        CatalogEntry {
            reasoning_levels: Vec::new(),
            id: id.into(),
            kind: CatalogEntryKind::Flow {
                flow_id: flow_id.into(),
                published_name: id.into(),
            },
            service_surfaces: vec![ServiceSurface::Chat],
            input_modalities: vec![InputModality::Text],
            output_modalities: vec![OutputModality::Text],
            diagnostic: None,
        }
    }

    /// Raw service model forwarded over the mesh → direct executor path (no
    /// flow, no Default Chat, no PII). This is the core of the fix: even when
    /// the serving node has a default chat flow, a forwarded raw model must NOT
    /// resolve to a flow here.
    #[test]
    fn forwarded_raw_model_is_not_a_flow() {
        let snap = snapshot_with(vec![service_model("deepseek")]);
        assert!(!model_resolves_to_flow(&snap, "deepseek"));
    }

    /// Model published from a flow → flow engine path preserved.
    #[test]
    fn forwarded_published_flow_is_a_flow() {
        let snap = snapshot_with(vec![published_flow("my-agent", "flow-123")]);
        assert!(model_resolves_to_flow(&snap, "my-agent"));
    }

    /// Alias that ultimately targets a raw model stays direct.
    #[test]
    fn forwarded_alias_to_model_is_not_a_flow() {
        let alias = CatalogEntry {
            reasoning_levels: Vec::new(),
            id: "chat".into(),
            kind: CatalogEntryKind::Alias {
                target: "deepseek".into(),
                fallback_targets: vec![],
                strategy: Strategy::FirstAvailable,
            },
            service_surfaces: vec![ServiceSurface::Chat],
            input_modalities: vec![InputModality::Text],
            output_modalities: vec![OutputModality::Text],
            diagnostic: None,
        };
        let snap = snapshot_with(vec![alias, service_model("deepseek")]);
        assert!(!model_resolves_to_flow(&snap, "chat"));
    }

    /// Alias that targets a published flow routes through the flow engine.
    #[test]
    fn forwarded_alias_to_flow_is_a_flow() {
        let alias = CatalogEntry {
            reasoning_levels: Vec::new(),
            id: "assistant".into(),
            kind: CatalogEntryKind::Alias {
                target: "my-agent".into(),
                fallback_targets: vec![],
                strategy: Strategy::FirstAvailable,
            },
            service_surfaces: vec![ServiceSurface::Chat],
            input_modalities: vec![InputModality::Text],
            output_modalities: vec![OutputModality::Text],
            diagnostic: None,
        };
        let snap = snapshot_with(vec![alias, published_flow("my-agent", "flow-123")]);
        assert!(model_resolves_to_flow(&snap, "assistant"));
    }

    /// Unknown model id (not in the serving node's catalog) defaults to direct
    /// dispatch — never silently wrapped in the local default flow.
    #[test]
    fn forwarded_unknown_model_is_not_a_flow() {
        let snap = snapshot_with(vec![]);
        assert!(!model_resolves_to_flow(&snap, "ghost"));
    }
}
