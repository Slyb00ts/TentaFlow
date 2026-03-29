// =============================================================================
// Plik: routing/tts.rs
// Opis: Synteza mowy (TTS) — synthesize_speech z QUIC TTS (preferowany)
//       i fallbackiem na jarvis voice.
// =============================================================================

use crate::error::{Result, CoreError};
use crate::routing::router::Router;

use tracing::{debug, warn, error};

impl Router {
    /// Syntezuje mowe z tekstu uzywajac QUIC TTS lub HTTP TTS.
    ///
    /// Flow:
    /// 1. Probuje znalezc QUIC TTS client (preferowany - lokalne modele Sherpa)
    /// 2. Jesli brak QUIC -> fallback do HTTP TTS (OpenAI API)
    /// 3. Wysyla request i zwraca raw audio bytes
    ///
    /// Parametry:
    /// - `request`: TTSRequest z OpenAI API format (model, input, voice, format, speed)
    pub async fn synthesize_speech(
        &self,
        request: &crate::api::openai::types::TTSRequest,
    ) -> Result<Vec<u8>> {
        use tentaflow_protocol::*;

        let model = &request.model;
        let input = &request.input;
        let voice = &request.voice;
        let speed = request.speed.unwrap_or(1.0);
        let format = request.response_format.as_deref().unwrap_or("wav");

        debug!("synthesize_speech: model={}, voice={}, format={}, input_len={}", model, voice, format, input.len());

        // === KROK 1: Sprobuj QUIC TTS (preferowany) ===
        // Szukaj po nazwie modelu lub uzyj pierwszego dostepnego QUIC TTS
        let quic_tts_service_name = self.service_manager.get_first_tts_service_name();

        if let Some(ref service_name) = quic_tts_service_name {
            if self.service_manager.has_quic_tts_service(service_name) {
                if let Some(quic_client) = self.service_manager.get_quic_tts_client(service_name).await {
                    debug!("Using QUIC TTS backend: {}", service_name);

                    // Utworz ModelRequest z AudioPayload
                    let request_id = uuid::Uuid::new_v4().to_string();
                    let model_request = ModelRequest {
                        request_id: request_id.clone(),
                        payload: ModelPayload::Audio(AudioPayload {
                            operation: AudioOperation::TTS {
                                model: model.clone(),
                                input: input.clone(),
                                voice: voice.clone(),
                                format: Some(format.to_string()),
                                speed: Some(speed),
                            },
                        }),
                        stream: false,
                        metadata: None,
                        session_id: None,
                    };

                    // Wyslij przez QUIC
                    match quic_client.send_request(model_request).await {
                        Ok(response) => {
                            match response.result {
                                ModelResult::Audio(audio_result) => {
                                    match audio_result.data {
                                        AudioResultData::Audio(audio_bytes) => {
                                            debug!("QUIC TTS success: {} bytes", audio_bytes.len());
                                            return Ok(audio_bytes);
                                        }
                                        _ => {
                                            warn!("QUIC TTS returned unexpected result type");
                                        }
                                    }
                                }
                                ModelResult::Error(err) => {
                                    warn!("QUIC TTS error: {:?} - {}", err.error_type, err.message);
                                }
                                _ => {
                                    warn!("QUIC TTS returned unexpected result type");
                                }
                            }
                        }
                        Err(e) => {
                            warn!("QUIC TTS request failed: {}", e);
                        }
                    }
                }
            }
        }

        // === KROK 2: Fallback na jarvis (lokalny Sherpa) ===
        if voice != "jarvis" {
            warn!("Voice '{}' failed, falling back to 'jarvis'", voice);

            if let Some(ref service_name) = quic_tts_service_name {
                if let Some(quic_client) = self.service_manager.get_quic_tts_client(service_name).await {
                    let request_id = uuid::Uuid::new_v4().to_string();
                    let model_request = ModelRequest {
                        request_id: request_id.clone(),
                        payload: ModelPayload::Audio(AudioPayload {
                            operation: AudioOperation::TTS {
                                model: model.clone(),
                                input: input.clone(),
                                voice: "jarvis".to_string(), // Fallback voice
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
                            if let ModelResult::Audio(audio_result) = response.result {
                                if let AudioResultData::Audio(audio_bytes) = audio_result.data {
                                    debug!("Fallback to jarvis success: {} bytes", audio_bytes.len());
                                    return Ok(audio_bytes);
                                }
                            }
                        }
                        Err(e) => {
                            error!("Fallback to jarvis also failed: {}", e);
                        }
                    }
                }
            }
        }

        Err(CoreError::ModelNotFound {
            model_name: format!("TTS failed for voice '{}' and fallback 'jarvis'", voice),
        }.into())
    }
}
