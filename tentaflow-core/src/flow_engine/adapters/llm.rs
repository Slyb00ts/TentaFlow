// =============================================================================
// Plik: flow_engine/adapters/llm.rs
// Opis: Adapter wezla LLM - deleguje generowanie tekstu do backendu LLM
//       przez ServiceManager routera. Obsluguje konfiguracje modelu,
//       temperature, max_tokens i system prompt z definicji wezla.
// =============================================================================

use anyhow::{bail, Result};
use serde_json::Value;
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::config::RouterConfig;
use crate::flow_engine::adapters::NodeAdapter;
use crate::flow_engine::types::FlowContext;
use crate::api::openai::types::{
    ChatCompletionRequest, Message, MessageContent,
};
use crate::routing::service_manager::ServiceManager;

/// Adapter wezla LLM - generowanie tekstu przez backend LLM.
/// Trzyma Arc do ServiceManager i konfiguracji routera.
pub struct LlmNodeAdapter {
    service_manager: Arc<ServiceManager>,
    config: Arc<RouterConfig>,
}

impl LlmNodeAdapter {
    pub fn new(service_manager: Arc<ServiceManager>, config: Arc<RouterConfig>) -> Self {
        Self {
            service_manager,
            config,
        }
    }

    /// Rozwiazuje alias modelu na nazwe kanoniczna
    fn resolve_model_alias(&self, model: &str) -> String {
        for alias in &self.config.service_aliases {
            if alias.alias == model {
                return alias.target.clone();
            }
        }
        model.to_string()
    }

    /// Buduje ChatCompletionRequest z konfiguracji wezla i kontekstu flow
    fn build_request(
        &self,
        node_config: &Value,
        ctx: &FlowContext,
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
            // Konwertuj ctx.messages na Vec<Message>
            let mut msgs: Vec<Message> = ctx.messages.iter().filter_map(|v| {
                let role = v.get("role")?.as_str()?.to_string();
                let content = v.get("content")?.as_str()?.to_string();
                Some(Message {
                    role,
                    content: Some(MessageContent::Text(content)),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                })
            }).collect();

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
                    msgs.insert(0, Message {
                        role: "system".to_string(),
                        content: Some(MessageContent::Text(prompt)),
                        name: None,
                        tool_calls: None,
                        tool_call_id: None,
                        reasoning_content: None,
                    });
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

            // Jesli poprzedni wezel to RAG - polacz kontekst z oryginalnym pytaniem
            let input_text = if let Some(rag_context) = self.detect_rag_context(node_config, ctx) {
                format!("Kontekst:\n{}\n\nPytanie: {}", rag_context, ctx.input)
            } else {
                self.resolve_input_text(node_config, ctx)
            };

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
            stream: false,
            user: None,
            response_format: None,
            tools: None,
            tool_choice: None,
            n: None,
            rag_options: None,
            memory_options: None,
            audio_input: None,
        }
    }

    /// Wykrywa kontekst RAG z poprzedniego wezla
    fn detect_rag_context(&self, node_config: &Value, ctx: &FlowContext) -> Option<String> {
        let prev_result = if let Some(input_from) = node_config.get("input_from").and_then(|v| v.as_str()) {
            ctx.node_results.get(input_from)
        } else if let Some(last_log) = ctx.execution_log.last() {
            ctx.node_results.get(&last_log.node_id)
        } else {
            None
        };

        let prev = prev_result?;

        // Jesli wynik zawiera "context" i "sources" - to RAG output
        if prev.get("context").is_some() && prev.get("sources").is_some() {
            prev.get("context").and_then(|v| v.as_str()).map(|s| s.to_string())
        } else {
            None
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
        let request = self.build_request(node_config, ctx);
        let model_name = self.resolve_model_alias(&request.model);

        info!(
            model = %model_name,
            input_len = ctx.input.len(),
            "LLM adapter: wywolanie serwisu"
        );

        // Sprawdz czy to QUIC LLM
        if self.service_manager.has_quic_llm_service(&model_name) {
            let quic_handle = { self.service_manager.quic_llm_services.read().get(&model_name).cloned() };
            if let Some(quic_handle) = quic_handle {
                if let Some(quic_client) = quic_handle.get_client().await {
                    debug!("LLM adapter: uzywam QUIC backend: {}", model_name);

                    let request_id = uuid::Uuid::new_v4().to_string();

                    // Konwertuj messages do formatu protocol
                    let protocol_messages: Vec<tentaflow_protocol::Message> = request
                        .messages
                        .iter()
                        .map(|m| {
                            let content = match &m.content {
                                Some(MessageContent::Text(text)) => text.clone(),
                                _ => String::new(),
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
                            let (tokens_prompt, tokens_completion) = response.metrics
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
                                    bail!(
                                        "QUIC LLM error: {:?} - {}",
                                        err.error_type,
                                        err.message
                                    );
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

        // HTTP backend
        let backends = self.service_manager.get_service_backends(&model_name);
        match backends {
            Some(backends) if !backends.is_empty() => {
                let strategy = self.service_manager.get_strategy(&model_name);
                let backend_idx = match strategy {
                    Some(s) => s.select_backend(backends)?,
                    None => 0,
                };
                let backend = &backends[backend_idx];

                debug!(
                    "LLM adapter: HTTP backend {} [{}]",
                    backend.url(),
                    backend_idx
                );

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

                let tokens_prompt = response.usage.as_ref()
                    .map(|u| u.prompt_tokens as i64)
                    .unwrap_or(0);
                let tokens_completion = response.usage.as_ref()
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

    fn node_type(&self) -> &'static str {
        "llm"
    }

    fn supports_streaming(&self) -> bool {
        true
    }
}
