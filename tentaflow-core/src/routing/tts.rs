// =============================================================================
// Plik: routing/tts.rs
// Opis: Synteza mowy (TTS) — synthesize_speech z QUIC TTS (preferowany)
//       i fallbackiem na jarvis voice.
// =============================================================================

use crate::error::{CoreError, Result};
use crate::routing::router::Router;

use tracing::debug;

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
    ) -> Result<crate::routing::RouteResult<Vec<u8>>> {
        use crate::routing::middleware::BackendHandle;
        use tentaflow_protocol::*;

        let model = &request.model;
        let input = &request.input;
        let voice = &request.voice;
        let speed = request.speed.unwrap_or(1.0);
        let format = request.response_format.as_deref().unwrap_or("wav");

        debug!(
            "synthesize_speech: model={}, voice={}, format={}, input_len={}",
            model,
            voice,
            format,
            input.len()
        );

        let tts_model = model.clone();
        let route_result = {
            let this = self.clone();
            let model_c = model.clone();
            let input_c = input.clone();
            let voice_c = voice.clone();
            let format_c = format.to_string();
            self.dispatch_with_fallback(model, 0, |handle| {
                let this = this.clone();
                let model_c = model_c.clone();
                let input_c = input_c.clone();
                let voice_c = voice_c.clone();
                let format_c = format_c.clone();
                let handle = handle.clone();
                async move {
                    match &handle {
                        BackendHandle::QuicTts(name) => {
                            let quic_client = this
                                .service_manager
                                .get_quic_tts_client(name)
                                .await
                                .ok_or_else(|| {
                                    anyhow::anyhow!("QUIC TTS service {} nie polaczony", name)
                                })?;

                            debug!("Using QUIC TTS backend: {}", name);
                            let request_id = uuid::Uuid::new_v4().to_string();
                            let model_request = ModelRequest {
                                request_id: request_id.clone(),
                                payload: ModelPayload::Audio(AudioPayload {
                                    operation: AudioOperation::TTS {
                                        model: model_c,
                                        input: input_c,
                                        voice: voice_c,
                                        format: Some(format_c),
                                        speed: Some(speed),
                                    },
                                }),
                                stream: false,
                                metadata: None,
                                session_id: None,
                            };

                            let response = quic_client
                                .send_request(model_request)
                                .await
                                .map_err(|e| anyhow::anyhow!("QUIC TTS request failed: {}", e))?;

                            match response.result {
                                ModelResult::Audio(audio_result) => match audio_result.data {
                                    AudioResultData::Audio(audio_bytes) => {
                                        debug!("QUIC TTS success: {} bytes", audio_bytes.len());
                                        Ok(audio_bytes)
                                    }
                                    _ => Err(anyhow::anyhow!(
                                        "QUIC TTS zwrocil nieoczekiwany typ wyniku"
                                    )),
                                },
                                ModelResult::Error(err) => Err(anyhow::anyhow!(
                                    "QUIC TTS error: {:?} - {}",
                                    err.error_type,
                                    err.message
                                )),
                                _ => Err(anyhow::anyhow!(
                                    "QUIC TTS zwrocil nieoczekiwany typ odpowiedzi"
                                )),
                            }
                        }
                        _ => Err(anyhow::anyhow!("Nieobslugiwany backend dla TTS")),
                    }
                }
            })
            .await
        };

        match route_result {
            Ok(result) => Ok(result),
            Err(_) => Err(CoreError::ModelNotFound {
                model_name: format!("TTS nie znaleziono backendow dla modelu '{}'", tts_model),
            }
            .into()),
        }
    }
}
