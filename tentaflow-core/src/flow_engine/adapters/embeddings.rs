// =============================================================================
// Plik: flow_engine/adapters/embeddings.rs
// Opis: Adapter wezla Embeddings - generuje wektory embedding z tekstu
//       przez QUIC lub HTTP backend.
// =============================================================================

use anyhow::{bail, Result};
use serde_json::Value;
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::config::RouterConfig;
use crate::flow_engine::adapters::NodeAdapter;
use crate::flow_engine::types::FlowContext;
use crate::routing::service_manager::ServiceManager;
use tentaflow_protocol::*;

/// Adapter wezla Embeddings - generowanie wektorow embedding
pub struct EmbeddingsNodeAdapter {
    service_manager: Arc<ServiceManager>,
    /// Trzymany dla zachowania sygnatury konstruktora (callerzy migruja
    /// w kroku N7.3); aliasy modeli pochodza z DB, nie z config.toml.
    #[allow(dead_code)]
    config: Arc<RouterConfig>,
}

impl EmbeddingsNodeAdapter {
    pub fn new(service_manager: Arc<ServiceManager>, config: Arc<RouterConfig>) -> Self {
        Self {
            service_manager,
            config,
        }
    }

    /// Rozwiazuje alias modelu na nazwe kanoniczna. Config-driven aliasy
    /// zostaly skasowane (krok N7.1a); DB `service_aliases` jest rozwiazywany
    /// przez middleware route resolver przed wejsciem do flow.
    fn resolve_model_alias(&self, model: &str) -> String {
        model.to_string()
    }

    /// Rozwiazuje tekst wejsciowy z kontekstu flow
    fn resolve_input_text(&self, node_config: &Value, ctx: &FlowContext) -> String {
        if let Some(text) = node_config.get("text").and_then(|v| v.as_str()) {
            return text.to_string();
        }

        if let Some(input_from) = node_config.get("input_from").and_then(|v| v.as_str()) {
            if let Some(prev_result) = ctx.node_results.get(input_from) {
                if let Some(text) = prev_result.get("text").and_then(|v| v.as_str()) {
                    return text.to_string();
                }
                if let Some(content) = prev_result.get("content").and_then(|v| v.as_str()) {
                    return content.to_string();
                }
            }
        }

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

impl NodeAdapter for EmbeddingsNodeAdapter {
    async fn execute(&self, node_config: &Value, ctx: &mut FlowContext) -> Result<Value> {
        let model_name = node_config
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("embeddings-gemma");

        let model_name = self.resolve_model_alias(model_name);

        let input_text = self.resolve_input_text(node_config, ctx);

        info!(
            model = %model_name,
            input_len = input_text.len(),
            "Embeddings adapter: generowanie wektorow"
        );

        if input_text.is_empty() {
            debug!("Embeddings adapter: pusty tekst wejsciowy");
            return Ok(serde_json::json!({
                "embedding": [],
                "dimensions": 0,
            }));
        }

        let resolved_quic_client = self
            .service_manager
            .find_quic_client_for_model(&model_name)
            .await;
        if let Some(quic_client) = resolved_quic_client {
            {
                debug!("Embeddings adapter: uzywam QUIC backend: {}", model_name);

                let embeddings_model_name = model_name
                    .strip_prefix("embeddings-")
                    .unwrap_or(&model_name)
                    .to_string();

                let request_id = uuid::Uuid::new_v4().to_string();
                let model_request = ModelRequest {
                    request_id: request_id.clone(),
                    payload: ModelPayload::Embeddings(EmbeddingsPayload {
                        model: embeddings_model_name,
                        input: vec![input_text.clone()],
                        normalize: true,
                    }),
                    stream: false,
                    metadata: None,
                    session_id: None,
                };

                match quic_client.send_request(model_request).await {
                    Ok(response) => match response.result {
                        ModelResult::Embeddings(embeddings_result) => {
                            if let Some(first_embedding) = embeddings_result.embeddings.first() {
                                let dimensions = first_embedding.len();
                                debug!(
                                    "Embeddings adapter: wygenerowano wektor o {} wymiarach",
                                    dimensions
                                );

                                return Ok(serde_json::json!({
                                    "embedding": first_embedding,
                                    "dimensions": dimensions,
                                }));
                            } else {
                                bail!("Embeddings adapter: pusta odpowiedz z backendu");
                            }
                        }
                        ModelResult::Error(err) => {
                            bail!(
                                "Embeddings adapter QUIC error: {:?} - {}",
                                err.error_type,
                                err.message
                            );
                        }
                        _ => {
                            warn!("Embeddings adapter: nieoczekiwany typ wyniku");
                        }
                    },
                    Err(e) => {
                        warn!(
                            "Embeddings adapter: QUIC request failed: {} - probuje fallback",
                            e
                        );
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

        if let Some(backend) = backend_opt {
            debug!("Embeddings adapter: uzywam HTTP backend: {}", backend.url());

            let request = crate::api::openai::types::EmbeddingRequest {
                model: model_name.clone(),
                input: crate::api::openai::types::EmbeddingInput::Single(input_text),
                encoding_format: None,
                dimensions: None,
                user: None,
            };

            let response = backend.embeddings_request(request).await?;

            if let Some(first) = response.data.first() {
                let dimensions = first.embedding.len();
                return Ok(serde_json::json!({
                    "embedding": first.embedding,
                    "dimensions": dimensions,
                }));
            }
        }

        bail!(
            "Embeddings adapter: brak dostepnego backendu dla modelu '{}'",
            model_name
        );
    }

    fn node_type(&self) -> &'static str {
        "embeddings"
    }
}
