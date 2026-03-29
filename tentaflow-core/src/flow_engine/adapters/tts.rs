// =============================================================================
// Plik: flow_engine/adapters/tts.rs
// Opis: Adapter wezla TTS (Text-to-Speech) - deleguje synteze mowy do
//       backendu TTS przez QUIC. Obsluguje parametry voice, format i speed.
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

/// Adapter wezla TTS - synteza mowy z tekstu
pub struct TtsNodeAdapter {
    service_manager: Arc<ServiceManager>,
    #[allow(dead_code)]
    config: Arc<RouterConfig>,
}

impl TtsNodeAdapter {
    pub fn new(service_manager: Arc<ServiceManager>, config: Arc<RouterConfig>) -> Self {
        Self {
            service_manager,
            config,
        }
    }

    /// Rozwiazuje tekst wejsciowy z kontekstu flow
    fn resolve_input_text(&self, node_config: &Value, ctx: &FlowContext) -> String {
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

impl NodeAdapter for TtsNodeAdapter {
    async fn execute(&self, node_config: &Value, ctx: &mut FlowContext) -> Result<Value> {
        let voice = node_config
            .get("voice")
            .and_then(|v| v.as_str())
            .unwrap_or("jarvis");

        let model = node_config
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("tts-1");

        let format = node_config
            .get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("wav");

        let speed = node_config
            .get("speed")
            .and_then(|v| v.as_f64())
            .unwrap_or(1.0) as f32;

        let input_text = self.resolve_input_text(node_config, ctx);

        info!(
            voice = voice,
            model = model,
            input_len = input_text.len(),
            "TTS adapter: synteza mowy"
        );

        if input_text.is_empty() {
            debug!("TTS adapter: pusty tekst wejsciowy");
            return Ok(serde_json::json!({
                "audio_base64": "",
                "format": format,
                "duration": 0,
            }));
        }

        // Sprobuj QUIC TTS
        let tts_service_name = self.service_manager.get_first_tts_service_name();

        if let Some(ref service_name) = tts_service_name {
            if self.service_manager.has_quic_tts_service(service_name) {
                if let Some(quic_client) = self.service_manager.get_quic_tts_client(service_name).await {
                    debug!("TTS adapter: uzywam QUIC backend: {}", service_name);

                    let request_id = uuid::Uuid::new_v4().to_string();
                    let model_request = ModelRequest {
                        request_id: request_id.clone(),
                        payload: ModelPayload::Audio(AudioPayload {
                            operation: AudioOperation::TTS {
                                model: model.to_string(),
                                input: input_text.clone(),
                                voice: voice.to_string(),
                                format: Some(format.to_string()),
                                speed: Some(speed),
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
                                        AudioResultData::Audio(audio_bytes) => {
                                            debug!("TTS adapter: synteza OK, {} bajtow", audio_bytes.len());

                                            let audio_base64 = base64::Engine::encode(
                                                &base64::engine::general_purpose::STANDARD,
                                                &audio_bytes,
                                            );

                                            return Ok(serde_json::json!({
                                                "audio_base64": audio_base64,
                                                "format": format,
                                                "duration": 0,
                                                "bytes": audio_bytes.len(),
                                            }));
                                        }
                                        _ => {
                                            warn!("TTS adapter: nieoczekiwany typ wyniku audio");
                                        }
                                    }
                                }
                                ModelResult::Error(err) => {
                                    bail!(
                                        "TTS adapter QUIC error: {:?} - {}",
                                        err.error_type,
                                        err.message
                                    );
                                }
                                _ => {
                                    warn!("TTS adapter: nieoczekiwany typ wyniku");
                                }
                            }
                        }
                        Err(e) => {
                            warn!("TTS adapter: QUIC request failed: {} - probuje fallback", e);
                        }
                    }
                }
            }
        }

        bail!(
            "TTS adapter: brak dostepnego backendu TTS dla voice '{}'",
            voice
        );
    }

    fn node_type(&self) -> &'static str {
        "tts"
    }
}
