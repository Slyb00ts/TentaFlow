// =============================================================================
// Plik: flow_engine/adapters/stt.rs
// Opis: Adapter wezla STT (Speech-to-Text) - deleguje transkrypcje audio
//       do backendu STT przez QUIC lub OpenAI API.
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

/// Adapter wezla STT - transkrypcja audio na tekst
pub struct SttNodeAdapter {
    service_manager: Arc<ServiceManager>,
    config: Arc<RouterConfig>,
}

impl SttNodeAdapter {
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
}

impl NodeAdapter for SttNodeAdapter {
    async fn execute(&self, node_config: &Value, ctx: &mut FlowContext) -> Result<Value> {
        let model_name = node_config
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("whisper");

        let model_name = self.resolve_model_alias(model_name);

        let language = node_config
            .get("language")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        info!(
            model = %model_name,
            "STT adapter: transkrypcja audio"
        );

        // Pobierz dane audio z kontekstu flow.
        let audio_data = self.resolve_audio_data(node_config, ctx);

        match audio_data {
            Some(data) if !data.is_empty() => {
                // Sprobuj QUIC STT
                if self.service_manager.has_quic_stt_service(&model_name) {
                    if let Some(quic_client) = self.service_manager.get_quic_stt_client(&model_name).await {
                        debug!("STT adapter: uzywam QUIC backend: {}", model_name);

                        let request_id = uuid::Uuid::new_v4().to_string();
                        let model_request = ModelRequest {
                            request_id: request_id.clone(),
                            payload: ModelPayload::Audio(AudioPayload {
                                operation: AudioOperation::STT {
                                    model: model_name.clone(),
                                    audio_data: data,
                                    language: language.clone(),
                                    response_format: None,
                                    prompt: None,
                                    temperature: None,
                                    timestamp_granularities: None,
                                    no_speech_threshold: None,
                                    avg_logprob_threshold: None,
                                    compression_ratio_threshold: None,
                                },
                            }),
                            stream: false,
                            metadata: None,
                            session_id: None,
                        };

                        match quic_client.send_request(model_request).await {
                            Ok(response) => {
                                match response.result {
                                    ModelResult::Audio(audio_result) => {
                                        match audio_result.data {
                                            AudioResultData::Text(text) => {
                                                debug!("STT adapter: transkrypcja OK, {} znakow", text.len());
                                                return Ok(serde_json::json!({
                                                    "text": text,
                                                    "language": language.unwrap_or_else(|| "pl".to_string()),
                                                    "duration": 0,
                                                }));
                                            }
                                            AudioResultData::Detailed { text, language: lang, duration, .. } => {
                                                debug!("STT adapter: transkrypcja OK (detailed), {} znakow", text.len());
                                                return Ok(serde_json::json!({
                                                    "text": text,
                                                    "language": lang,
                                                    "duration": duration,
                                                }));
                                            }
                                            _ => {
                                                warn!("STT adapter: nieoczekiwany typ wyniku audio");
                                            }
                                        }
                                    }
                                    ModelResult::Error(err) => {
                                        bail!(
                                            "STT adapter QUIC error: {:?} - {}",
                                            err.error_type,
                                            err.message
                                        );
                                    }
                                    _ => {
                                        warn!("STT adapter: nieoczekiwany typ wyniku");
                                    }
                                }
                            }
                            Err(e) => {
                                bail!("STT adapter: QUIC request failed: {}", e);
                            }
                        }
                    } else {
                        warn!("STT adapter: QUIC serwis '{}' nie jest polaczony", model_name);
                    }
                }

                // HTTP backend jako fallback
                let backends = self.service_manager.get_service_backends_cloned(&model_name);
                if let Some(ref backends) = backends {
                    if !backends.is_empty() {
                        debug!("STT adapter: uzywam HTTP backend (fallback)");
                        // TODO: Implementacja HTTP STT - wymaga TranscriptionRequest
                    }
                }

                bail!(
                    "STT adapter: brak dostepnego backendu STT dla modelu '{}'",
                    model_name
                );
            }
            _ => {
                // Brak danych audio - zwroc pusty wynik
                debug!("STT adapter: brak danych audio w kontekscie");
                Ok(serde_json::json!({
                    "text": "",
                    "language": language.unwrap_or_else(|| "pl".to_string()),
                    "duration": 0,
                }))
            }
        }
    }

    fn node_type(&self) -> &'static str {
        "stt"
    }
}

impl SttNodeAdapter {
    /// Pobiera dane audio z kontekstu flow.
    fn resolve_audio_data(&self, node_config: &Value, ctx: &FlowContext) -> Option<Vec<u8>> {
        if let Some(audio_var) = node_config.get("audio_variable").and_then(|v| v.as_str()) {
            if let Some(val) = ctx.variables.get(audio_var) {
                if let Some(b64) = val.as_str() {
                    if let Ok(decoded) = base64::Engine::decode(
                        &base64::engine::general_purpose::STANDARD,
                        b64,
                    ) {
                        return Some(decoded);
                    }
                }
            }
        }

        if let Some(val) = ctx.variables.get("audio_input") {
            if let Some(b64) = val.as_str() {
                if let Ok(decoded) = base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD,
                    b64,
                ) {
                    return Some(decoded);
                }
            }
        }

        if let Some(input_from) = node_config.get("input_from").and_then(|v| v.as_str()) {
            if let Some(prev_result) = ctx.node_results.get(input_from) {
                if let Some(b64) = prev_result.get("audio_base64").and_then(|v| v.as_str()) {
                    if let Ok(decoded) = base64::Engine::decode(
                        &base64::engine::general_purpose::STANDARD,
                        b64,
                    ) {
                        return Some(decoded);
                    }
                }
            }
        }

        None
    }
}
