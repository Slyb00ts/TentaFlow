// =============================================================================
// Plik: flow_engine/adapters/llm.rs
// Opis: Adapter wezla LLM - deleguje generowanie tekstu do backendu LLM
//       przez ServiceManager routera. Obsluguje konfiguracje modelu,
//       temperature, max_tokens i system prompt z definicji wezla.
// =============================================================================

use anyhow::{anyhow, bail, Result};
use serde_json::Value;
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::api::openai::types::{ChatCompletionRequest, Message, MessageContent};
use crate::config::RouterConfig;
use crate::flow_engine::adapters::{AdapterChunkStream, NodeAdapter};
use crate::flow_engine::types::FlowContext;
use crate::routing::service_manager::ServiceManager;
use crate::routing::stream_helpers::quic_stream_to_openai_chunks;

/// Adapter wezla LLM - generowanie tekstu przez backend LLM.
/// Trzyma Arc do ServiceManager i konfiguracji routera.
pub struct LlmNodeAdapter {
    service_manager: Arc<ServiceManager>,
    /// Trzymany dla zachowania sygnatury konstruktora (callerzy migruja
    /// w kroku N7.3); aliasy modeli pochodza z DB, nie z config.toml.
    #[allow(dead_code)]
    config: Arc<RouterConfig>,
    /// R2a: shared slot na unified runtime executor. None tylko w testach
    /// ktore omijaja `Router::new`. Adapter lockuje slot, klonuje `Arc`,
    /// uzywa — single point of dispatch dla LLM (alias resolution +
    /// strategy + per-instance modality filter).
    executor_slot: crate::flow_engine::dispatcher::ExecutorSlot,
}

impl LlmNodeAdapter {
    pub fn new(
        service_manager: Arc<ServiceManager>,
        config: Arc<RouterConfig>,
        executor_slot: crate::flow_engine::dispatcher::ExecutorSlot,
    ) -> Self {
        Self {
            service_manager,
            config,
            executor_slot,
        }
    }

    /// Rozwiazuje alias modelu na nazwe kanoniczna. Config-driven aliasy
    /// zostaly skasowane (krok N7.1a); DB `service_aliases` jest rozwiazywany
    /// przez middleware route resolver przed wejsciem do flow.
    fn resolve_model_alias(&self, model: &str) -> String {
        model.to_string()
    }

    /// Buduje ChatCompletionRequest z konfiguracji wezla i kontekstu flow.
    /// `streaming` steruje polem `request.stream` wysylanym do backendu.
    fn build_request(
        &self,
        node_config: &Value,
        ctx: &FlowContext,
        streaming: bool,
    ) -> ChatCompletionRequest {
        let use_messages_context = node_config
            .get("use_messages_context")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let model = node_config
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| ctx.model.clone());

        let temperature = node_config
            .get("temperature")
            .and_then(|v| v.as_f64())
            .map(|t| t as f32);

        let max_tokens = node_config
            .get("max_tokens")
            .and_then(|v| v.as_u64())
            .map(|t| t as u32);

        let messages = if use_messages_context && !ctx.messages.is_empty() {
            // The `content` field may arrive as a plain string (legacy
            // text message) or as an array of typed fragments
            // (`MessageContent::Parts` — vision / future multimodal).
            // Deserialise the whole value through serde so Parts land
            // as `MessageContent::Parts` instead of being collapsed to
            // empty text by an `as_str()` shortcut.
            let mut msgs: Vec<Message> = ctx
                .messages
                .iter()
                .filter_map(|v| {
                    let role = v.get("role")?.as_str()?.to_string();
                    let raw_content = v.get("content").cloned().unwrap_or(serde_json::Value::Null);
                    let content = match raw_content {
                        serde_json::Value::Null => None,
                        other => serde_json::from_value::<MessageContent>(other).ok(),
                    };
                    Some(Message {
                        role,
                        content,
                        name: None,
                        tool_calls: None,
                        tool_call_id: None,
                        reasoning_content: None,
                    })
                })
                .collect();

            // Jesli prompt_id ustawiony i brak system message -> prepend
            let resolved_prompt = node_config
                .get("prompt_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .and_then(|pid| self.service_manager.prompt_registry.get_content(pid))
                .map(|s| s.to_string());

            if let Some(prompt) = resolved_prompt {
                let has_system = msgs.first().map(|m| m.role == "system").unwrap_or(false);
                if !has_system {
                    msgs.insert(
                        0,
                        Message {
                            role: "system".to_string(),
                            content: Some(MessageContent::Text(prompt)),
                            name: None,
                            tool_calls: None,
                            tool_call_id: None,
                            reasoning_content: None,
                        },
                    );
                }
            }

            msgs
        } else {
            // Rozwiaz prompt: prompt_id z rejestru lub fallback na system_prompt z config
            let resolved_prompt = node_config
                .get("prompt_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .and_then(|pid| self.service_manager.prompt_registry.get_content(pid))
                .map(|s| s.to_string())
                .or_else(|| {
                    node_config
                        .get("system_prompt")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                });

            let mut msgs = Vec::new();

            if let Some(prompt) = resolved_prompt {
                msgs.push(Message {
                    role: "system".to_string(),
                    content: Some(MessageContent::Text(prompt)),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                });
            }

            let input_text = self.resolve_input_text(node_config, ctx);

            msgs.push(Message {
                role: "user".to_string(),
                content: Some(MessageContent::Text(input_text)),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            });

            msgs
        };

        ChatCompletionRequest {
            model,
            messages,
            temperature,
            max_tokens,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            stream: streaming,
            user: None,
            response_format: None,
            tools: None,
            tool_choice: None,
            n: None,
            memory_options: None,
            audio_input: None,
        }
    }

    /// Rozwiazuje tekst wejsciowy z konfiguracji wezla lub kontekstu
    fn resolve_input_text(&self, node_config: &Value, ctx: &FlowContext) -> String {
        // Jesli wezel ma skonfigurowane zrodlo danych
        if let Some(input_from) = node_config.get("input_from").and_then(|v| v.as_str()) {
            if let Some(prev_result) = ctx.node_results.get(input_from) {
                if let Some(text) = prev_result.get("text").and_then(|v| v.as_str()) {
                    return text.to_string();
                }
                if let Some(content) = prev_result.get("content").and_then(|v| v.as_str()) {
                    return content.to_string();
                }
                return prev_result.to_string();
            }
        }

        // Ostatni wynik z jakiegokolwiek wezla
        if let Some(last_log) = ctx.execution_log.last() {
            if let Some(prev_result) = ctx.node_results.get(&last_log.node_id) {
                if let Some(text) = prev_result.get("text").and_then(|v| v.as_str()) {
                    return text.to_string();
                }
            }
        }

        ctx.input.clone()
    }
}

impl NodeAdapter for LlmNodeAdapter {
    async fn execute(&self, node_config: &Value, ctx: &mut FlowContext) -> Result<Value> {
        let request = self.build_request(node_config, ctx, false);
        let model_name = self.resolve_model_alias(&request.model);

        info!(
            model = %model_name,
            input_len = ctx.input.len(),
            "LLM adapter: wywolanie serwisu"
        );

        // R2a: jednolity dispatch przez `ModelRuntimeExecutor` — alias
        // resolution + strategy + per-instance modality filter w jednym
        // miejscu (`services/runtime/executor.rs`). Embedded / HTTP /
        // QUIC LLM idzie przez executor; legacy mini-router zostaje na
        // wypadek braku executor'a w testach (DB-less Router).
        //
        // Snapshot Arc przed `.await` zeby trzymanie guard'a parking_lot
        // nie przeszlo przez yield point — inaczej future nie jest Send.
        let executor_snapshot = self.executor_slot.read().clone();
        if let Some(executor) = executor_snapshot {
            use crate::services::runtime::context::ExecutionContext;
            let mut exec_ctx = ExecutionContext::default();
            match executor.execute_chat(request.clone(), &mut exec_ctx).await {
                Ok(response) => {
                    // Codex H1 round 2: aplikuj PII filter na content
                    // PRZED zwroceniem do flow context. Direct executor
                    // path w `chat::route_chat_completion` robi to
                    // przez `apply_response_middleware`; flow path tutaj
                    // ma wlasna sciezke (executor → adapter → flow ctx)
                    // i tez musi czyscic. Reuse `ResponseMiddleware` z
                    // ServiceManager — adapter trzyma Arc<ServiceManager>.
                    let raw_content = response
                        .choices
                        .first()
                        .and_then(|c| c.message.content.as_ref())
                        .map(|c| match c {
                            MessageContent::Text(text) => text.clone(),
                            MessageContent::Parts(parts) => parts
                                .iter()
                                .filter_map(|p| {
                                    if let crate::api::openai::types::ContentPart::Text {
                                        text,
                                    } = p
                                    {
                                        Some(text.clone())
                                    } else {
                                        None
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join(" "),
                        })
                        .unwrap_or_default();
                    // ServiceManager nie wystawia handle do ResponseMiddleware;
                    // tworzymy lekka instancje per-call (no state, only enabled
                    // flag z RouterConfig). To samo co Router::new robi przy
                    // konstrukcji `response_middleware`.
                    let rm = crate::middleware::ResponseMiddleware::new(
                        self.config.middleware.response_filtering_enabled,
                    );
                    let content = rm.clean_text(&raw_content)?;
                    let tokens_prompt = response
                        .usage
                        .as_ref()
                        .map(|u| u.prompt_tokens as i64)
                        .unwrap_or(0);
                    let tokens_completion = response
                        .usage
                        .as_ref()
                        .map(|u| u.completion_tokens as i64)
                        .unwrap_or(0);
                    return Ok(serde_json::json!({
                        "content": content,
                        "tokens": { "prompt": tokens_prompt, "completion": tokens_completion },
                        "model": model_name,
                        "text": content,
                    }));
                }
                Err(e) => {
                    warn!(
                        model = %model_name,
                        "LLM adapter: executor dispatch failed: {} — fallback na legacy path",
                        e
                    );
                }
            }
        }

        let resolved_quic_client = self
            .service_manager
            .find_quic_client_for_model(&model_name)
            .await;
        if let Some(quic_client) = resolved_quic_client {
            // Inline the original "QUIC available" arm.
            {
                {
                    let request_id = uuid::Uuid::new_v4().to_string();

                    // Convert messages to the wire protocol. The QUIC
                    // `protocol::Message.content` is a plain `String`,
                    // so multimodal Parts cannot ride through it as
                    // typed payload — extract every text fragment and
                    // log a warning when a non-text part is dropped so
                    // an operator can spot the silent capability loss.
                    // A typed multimodal protocol slot is the correct
                    // long-term fix; until that lands the QUIC path is
                    // explicitly text-only.
                    let protocol_messages: Vec<tentaflow_protocol::Message> = request
                        .messages
                        .iter()
                        .map(|m| {
                            let content = match &m.content {
                                Some(MessageContent::Text(text)) => text.clone(),
                                Some(MessageContent::Parts(parts)) => {
                                    extract_text_parts_with_warning(&m.role, parts)
                                }
                                None => String::new(),
                            };
                            tentaflow_protocol::Message {
                                role: m.role.clone(),
                                content,
                            }
                        })
                        .collect();

                    let model_request = tentaflow_protocol::ModelRequest {
                        request_id: request_id.clone(),
                        payload: tentaflow_protocol::ModelPayload::Completion(
                            tentaflow_protocol::CompletionPayload {
                                model: model_name.clone(),
                                prompt: None,
                                messages: protocol_messages,
                                temperature: request.temperature,
                                max_tokens: request.max_tokens,
                                top_p: request.top_p,
                                stop: request.stop.clone(),
                                presence_penalty: request.presence_penalty,
                                frequency_penalty: request.frequency_penalty,
                                tts_options: None,
                                memory_options: None,
                                audio_input: None,
                                prefix_cache_id: None,
                                prefix_text: None,
                            },
                        ),
                        stream: false,
                        metadata: None,
                        session_id: None,
                    };

                    match quic_client.send_request(model_request).await {
                        Ok(response) => {
                            // Pobierz tokeny z metryk response
                            let (tokens_prompt, tokens_completion) = response
                                .metrics
                                .as_ref()
                                .and_then(|m| {
                                    if let Some(tentaflow_protocol::DetailedMetrics::Completion {
                                        prompt_tokens,
                                        completion_tokens,
                                        ..
                                    }) = &m.detailed
                                    {
                                        Some((*prompt_tokens as i64, *completion_tokens as i64))
                                    } else {
                                        None
                                    }
                                })
                                .unwrap_or((0, 0));

                            match response.result {
                                tentaflow_protocol::ModelResult::Completion(completion_result) => {
                                    return Ok(serde_json::json!({
                                        "content": completion_result.text,
                                        "tokens": {
                                            "prompt": tokens_prompt,
                                            "completion": tokens_completion,
                                        },
                                        "model": model_name,
                                        "text": completion_result.text,
                                    }));
                                }
                                tentaflow_protocol::ModelResult::Error(err) => {
                                    bail!("QUIC LLM error: {:?} - {}", err.error_type, err.message);
                                }
                                _ => {
                                    warn!("QUIC LLM zwrocil nieoczekiwany typ wyniku");
                                }
                            }
                        }
                        Err(e) => {
                            warn!("QUIC LLM request failed: {} - fallback na HTTP", e);
                        }
                    }
                }
            }
        }

        let backend_opt = self
            .service_manager
            .find_http_backend_for_model(&model_name)
            .or_else(|| {
                self.service_manager
                    .resolve_http_backends_via_snapshot(&model_name)
                    .and_then(|v| v.into_iter().next())
            });
        match backend_opt {
            Some(backend) => {
                debug!("LLM adapter: HTTP backend {}", backend.url(),);

                let response = backend.chat_completion(request).await?;

                let content = response
                    .choices
                    .first()
                    .and_then(|c| c.message.content.as_ref())
                    .map(|c| match c {
                        MessageContent::Text(text) => text.clone(),
                        MessageContent::Parts(parts) => parts
                            .iter()
                            .filter_map(|p| {
                                if let crate::api::openai::types::ContentPart::Text { text } = p {
                                    Some(text.clone())
                                } else {
                                    None
                                }
                            })
                            .collect::<Vec<_>>()
                            .join(" "),
                    })
                    .unwrap_or_default();

                let tokens_prompt = response
                    .usage
                    .as_ref()
                    .map(|u| u.prompt_tokens as i64)
                    .unwrap_or(0);
                let tokens_completion = response
                    .usage
                    .as_ref()
                    .map(|u| u.completion_tokens as i64)
                    .unwrap_or(0);

                Ok(serde_json::json!({
                    "content": content,
                    "tokens": {
                        "prompt": tokens_prompt,
                        "completion": tokens_completion,
                    },
                    "model": model_name,
                    "text": content,
                }))
            }
            _ => {
                bail!(
                    "LLM adapter: brak dostepnego backendu dla modelu '{}'",
                    model_name
                );
            }
        }
    }

    async fn execute_streaming(
        &self,
        node_config: &Value,
        ctx: &mut FlowContext,
    ) -> Option<Result<AdapterChunkStream>> {
        let request = self.build_request(node_config, ctx, true);
        let model_name = self.resolve_model_alias(&request.model);

        info!(
            model = %model_name,
            input_len = ctx.input.len(),
            "LLM adapter: streaming"
        );

        let resolved_quic_client = self
            .service_manager
            .find_quic_client_for_model(&model_name)
            .await;
        if let Some(quic_client) = resolved_quic_client {
            {
                {
                    // See `extract_text_parts_with_warning` rationale on
                    // the blocking path — same QUIC text-only constraint
                    // applies for streaming dispatch.
                    let protocol_messages: Vec<tentaflow_protocol::Message> = request
                        .messages
                        .iter()
                        .map(|m| {
                            let content = match &m.content {
                                Some(MessageContent::Text(text)) => text.clone(),
                                Some(MessageContent::Parts(parts)) => {
                                    extract_text_parts_with_warning(&m.role, parts)
                                }
                                None => String::new(),
                            };
                            tentaflow_protocol::Message {
                                role: m.role.clone(),
                                content,
                            }
                        })
                        .collect();

                    let model_request = tentaflow_protocol::ModelRequest {
                        request_id: uuid::Uuid::new_v4().to_string(),
                        payload: tentaflow_protocol::ModelPayload::Completion(
                            tentaflow_protocol::CompletionPayload {
                                model: model_name.clone(),
                                prompt: None,
                                messages: protocol_messages,
                                temperature: request.temperature,
                                max_tokens: request.max_tokens,
                                top_p: request.top_p,
                                stop: request.stop.clone(),
                                presence_penalty: request.presence_penalty,
                                frequency_penalty: request.frequency_penalty,
                                tts_options: None,
                                memory_options: None,
                                audio_input: None,
                                prefix_cache_id: None,
                                prefix_text: None,
                            },
                        ),
                        stream: true,
                        metadata: None,
                        session_id: None,
                    };

                    match quic_client.send_request_stream(model_request).await {
                        Ok(stream) => {
                            let converted =
                                quic_stream_to_openai_chunks(stream, model_name.clone());
                            return Some(Ok(converted));
                        }
                        Err(e) => {
                            warn!("QUIC LLM stream failed: {} — fallback na HTTP", e);
                        }
                    }
                }
            }
        }

        let backend_opt = self
            .service_manager
            .find_http_backend_for_model(&model_name)
            .or_else(|| {
                self.service_manager
                    .resolve_http_backends_via_snapshot(&model_name)
                    .and_then(|v| v.into_iter().next())
            });
        match backend_opt {
            Some(backend) => {
                debug!("LLM adapter streaming: HTTP backend {}", backend.url());
                match backend.chat_completion_stream(request).await {
                    Ok(stream) => Some(Ok(stream)),
                    Err(e) => Some(Err(e)),
                }
            }
            None => Some(Err(anyhow!(
                "LLM adapter (stream): brak backendu dla modelu '{}'",
                model_name
            ))),
        }
    }

    fn node_type(&self) -> &'static str {
        "llm"
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supported_output_ports(&self) -> &'static [&'static str] {
        &["stream", "full"]
    }
}

/// Extract every text fragment from a `MessageContent::Parts` list and
/// concatenate them; emit a `tracing::warn!` whenever a non-text part
/// (image URL, future audio) is dropped so the downgrade is visible in
/// production logs. The QUIC wire protocol carries `String` content
/// only, so a richer wire shape requires extending
/// `tentaflow_protocol::Message` — until that lands this helper is the
/// honest path: keep the prompt readable, surface the loss.
fn extract_text_parts_with_warning(
    role: &str,
    parts: &[crate::api::openai::types::ContentPart],
) -> String {
    let mut text = String::new();
    let mut dropped = 0usize;
    for part in parts {
        match part {
            crate::api::openai::types::ContentPart::Text { text: t } => {
                if !text.is_empty() && !text.ends_with(char::is_whitespace) {
                    text.push(' ');
                }
                text.push_str(t);
            }
            crate::api::openai::types::ContentPart::ImageUrl { .. } => dropped += 1,
        }
    }
    if dropped > 0 {
        tracing::warn!(
            role = role,
            dropped_parts = dropped,
            "QUIC dispatch is text-only; non-text MessageContent::Parts \
             fragments dropped — extend tentaflow_protocol::Message to \
             carry typed multimodal payloads"
        );
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::openai::types::{ContentPart, ImageUrl, MessageContent};
    use serde_json::json;

    /// `MessageContent` is `#[serde(untagged)]`; a JSON value carrying an
    /// array of typed parts must deserialise into `Parts`, not collapse
    /// into the empty `as_str()` fallback the old build_request used.
    #[test]
    fn message_content_parts_deserialise_round_trip() {
        let raw = json!([
            { "type": "text", "text": "hello" },
            { "type": "image_url", "image_url": { "url": "data:image/png;base64,XXX" } }
        ]);
        let decoded: MessageContent =
            serde_json::from_value(raw).expect("Parts payload must deserialise");
        match decoded {
            MessageContent::Parts(parts) => {
                assert_eq!(parts.len(), 2);
                assert!(matches!(&parts[0], ContentPart::Text { text } if text == "hello"));
                assert!(matches!(&parts[1], ContentPart::ImageUrl { .. }));
            }
            other => panic!("expected Parts, got {:?}", other),
        }
    }

    /// QUIC wire is text-only. The extractor walks Parts, joins text
    /// fragments with whitespace, and silently drops every non-text
    /// part — but emits a tracing warn (asserted indirectly by the
    /// `dropped > 0` branch). The visible behaviour is: text in,
    /// image-url out, single concatenated string returned.
    #[test]
    fn extract_text_parts_concatenates_text_and_drops_images() {
        let parts = vec![
            ContentPart::Text { text: "Look at this".into() },
            ContentPart::ImageUrl {
                image_url: ImageUrl {
                    url: "data:image/png;base64,X".into(),
                    detail: None,
                },
            },
            ContentPart::Text { text: "carefully.".into() },
        ];
        let extracted = extract_text_parts_with_warning("user", &parts);
        assert_eq!(extracted, "Look at this carefully.");
    }

    /// All-text Parts produce no warning and the full prompt — used to
    /// confirm the helper is a no-op when there is nothing to drop.
    #[test]
    fn extract_text_parts_passes_text_only_through_unchanged() {
        let parts = vec![
            ContentPart::Text { text: "First.".into() },
            ContentPart::Text { text: "Second.".into() },
        ];
        let extracted = extract_text_parts_with_warning("system", &parts);
        assert_eq!(extracted, "First. Second.");
    }

    /// Image-only Parts collapse to empty string after the warning —
    /// caller will end up sending a no-content message, which is the
    /// honest text-only behaviour for QUIC. Asserts we never panic on
    /// pure-image input.
    #[test]
    fn extract_text_parts_image_only_returns_empty_string() {
        let parts = vec![ContentPart::ImageUrl {
            image_url: ImageUrl {
                url: "data:image/png;base64,X".into(),
                detail: Some("low".into()),
            },
        }];
        let extracted = extract_text_parts_with_warning("user", &parts);
        assert!(extracted.is_empty());
    }
}
