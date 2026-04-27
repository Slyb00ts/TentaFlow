// =============================================================================
// Plik: routing/tts.rs
// Opis: Synteza mowy (TTS) — synthesize_speech (blocking) oraz
//       synthesize_speech_stream (chunkowane PCM dla niskiej latencji
//       pierwszej probki audio).
// =============================================================================

use crate::error::{CoreError, Result};
use crate::routing::router::Router;

use tracing::debug;

/// Domyslny rozmiar chunku PCM dla streamingu TTS — 100 ms audio
/// (16 kHz mono i16 LE = 16_000 * 0.1 * 2 bajty = 3200 bajtow).
/// Mniejsze chunki = nizsza pierwsza-probka latency, ale wiecej overhead'u
/// na ramke (nagłowek length-prefix + rkyv). 100 ms to kompromis: pierwszy
/// chunk dociera do mikrofonu w ~50 ms od momentu zakończenia syntezy,
/// jednoczesnie pojedynczy frame ma ~3 KB, co jest tanie.
const TTS_STREAM_CHUNK_BYTES: usize = 3_200;

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
    /// Wariant z user context — ACL gate przed wywolaniem backendu.
    pub async fn synthesize_speech_for_user(
        &self,
        request: &crate::api::openai::types::TTSRequest,
        user: Option<crate::routing::acl::UserContext>,
    ) -> Result<crate::routing::RouteResult<Vec<u8>>> {
        if let Some(ref u) = user {
            if let Some(ref db) = self.db {
                if !crate::routing::acl::check_access_safe(db, "model", &request.model, u.user_id, &u.role) {
                    tracing::warn!(user_id = u.user_id, model = %request.model, "ACL denied TTS model");
                    return Err(crate::error::CoreError::ModelNotFound {
                        model_name: request.model.clone(),
                    }.into());
                }
            }
        }
        self.synthesize_speech(request).await
    }

    pub async fn synthesize_speech(
        &self,
        request: &crate::api::openai::types::TTSRequest,
    ) -> Result<crate::routing::RouteResult<Vec<u8>>> {
        use crate::routing::middleware::BackendHandle;
        use tentaflow_protocol::*;

        let model = &request.model;
        // Wyczysc input przed dispatch: strip emoji + aplikuj reguly
        // `tts_cleaning_rules` z DB (cachowane w pamieci, refresh przy CRUD).
        // Bez tego TTS musial wymawiac surowe emoji / skroty / dziwne pattern'y
        // co dawalo cisze albo zlamana prozodie. Dziala tylko gdy router ma
        // db Pool — fallback do raw inputu gdy db=None.
        let cleaned_input = if let Some(ref db) = self.db {
            crate::tts::clean_cache::clean(&request.input, db)
        } else {
            request.input.clone()
        };
        let input = &cleaned_input;
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
                        BackendHandle::MeshForward(node_id, svc) => {
                            // Mesh-remote TTS — iroh robi relay multi-hop automatycznie.
                            debug!(target_node = %node_id, service = %svc, "MeshForward TTS");
                            let quic_client = this
                                .service_manager
                                .get_quic_tts_client(svc)
                                .await
                                .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "Mesh TTS serwis '{}' na nodzie {} nie polaczony",
                                    svc,
                                    node_id
                                )
                            })?;
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
                                .map_err(|e| anyhow::anyhow!("Mesh TTS request failed: {}", e))?;
                            match response.result {
                                ModelResult::Audio(audio_result) => match audio_result.data {
                                    AudioResultData::Audio(audio_bytes) => Ok(audio_bytes),
                                    _ => Err(anyhow::anyhow!(
                                        "Mesh TTS zwrocil nieoczekiwany typ wyniku"
                                    )),
                                },
                                ModelResult::Error(err) => Err(anyhow::anyhow!(
                                    "Mesh TTS error: {:?} - {}",
                                    err.error_type,
                                    err.message
                                )),
                                _ => Err(anyhow::anyhow!(
                                    "Mesh TTS zwrocil nieoczekiwany typ odpowiedzi"
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

    /// Streamujaca synteza mowy. Pierwsza iteracja: wywoluje pelne
    /// `synthesize_speech` a nastepnie tnie wynikowy bufor PCM na chunki po
    /// `TTS_STREAM_CHUNK_BYTES` bajtow. Zysk wzgledem blocking variant:
    /// klient (np. teams-bot) wpycha probki do mikrofonu jednoczesnie z
    /// transmisja kolejnych chunkow zamiast czekac na pelny WAV przed
    /// odtwarzaniem — eliminuje dodatkowy "first-byte" stall na sieci/
    /// deserializacji duzej ramki.
    ///
    /// Pelny end-to-end streaming (callback bezposrednio z silnika TTS,
    /// np. sherpa `create_with_callback`) wymaga refaktoru `TtsEngine`
    /// trait i osobnego dispatch path — to zostaje na nastepna iteracje.
    ///
    /// `chunk_sink` dostaje raw PCM bajty (bez WAV headera). Caller
    /// powinien zazadac `format = "pcm"` w `TTSRequest` zeby uniknac
    /// stripowania headera w pierwszym chunku.
    pub async fn synthesize_speech_stream<F>(
        &self,
        request: &crate::api::openai::types::TTSRequest,
        mut chunk_sink: F,
    ) -> Result<()>
    where
        F: FnMut(Vec<u8>) -> Result<()>,
    {
        let route_result = self.synthesize_speech(request).await?;
        let mut audio_bytes = route_result.response;

        // Strip WAV header gdy backend zignorowal `format=pcm` i zwrocil
        // jednak RIFF/WAVE — pierwszy chunk musi byc czystym PCM, inaczej
        // klient slyszy klikniecie/szum z naglowka.
        if audio_bytes.len() >= 12
            && &audio_bytes[0..4] == b"RIFF"
            && &audio_bytes[8..12] == b"WAVE"
        {
            audio_bytes = strip_wav_header(&audio_bytes)?;
        }

        for chunk in audio_bytes.chunks(TTS_STREAM_CHUNK_BYTES) {
            chunk_sink(chunk.to_vec())?;
        }
        Ok(())
    }
}

/// Wyciaga raw PCM z WAV po skanowaniu chunkow `fmt `/`data` (parser
/// tolerancyjny na opcjonalne LIST/INFO chunki). Wymaga PCM16 mono;
/// inaczej zwraca blad — caller zostaje w blocking ścieżce.
fn strip_wav_header(bytes: &[u8]) -> Result<Vec<u8>> {
    let err = |msg: &str| CoreError::InternalError {
        message: format!("WAV strip: {}", msg),
        source: None,
    };
    let mut cursor = 12usize;
    let mut data_start: Option<usize> = None;
    while cursor + 8 <= bytes.len() {
        let chunk_id = &bytes[cursor..cursor + 4];
        let chunk_size = u32::from_le_bytes(
            bytes[cursor + 4..cursor + 8]
                .try_into()
                .map_err(|_| err("chunk size"))?,
        ) as usize;
        let body = cursor + 8;
        if chunk_id == b"data" {
            data_start = Some(body);
            break;
        }
        cursor = body + chunk_size + (chunk_size & 1);
    }
    let start = data_start.ok_or_else(|| err("brak data chunk"))?;
    Ok(bytes[start..].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_wav_header_extracts_pcm() {
        // Minimalny WAV: RIFF + fmt(16B PCM16 mono 16k) + data(4B PCM)
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&36u32.to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&1u16.to_le_bytes()); // mono
        wav.extend_from_slice(&16_000u32.to_le_bytes());
        wav.extend_from_slice(&32_000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&4u32.to_le_bytes());
        wav.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);

        let pcm = strip_wav_header(&wav).expect("strip ok");
        assert_eq!(pcm, vec![0xAA, 0xBB, 0xCC, 0xDD]);
    }

    #[test]
    fn chunk_bytes_constant_matches_100ms_pcm16_16k_mono() {
        // Walidacja stalej: 16000 Hz * 0.1 s * 2 B/sample = 3200 B
        assert_eq!(TTS_STREAM_CHUNK_BYTES, 16_000 / 10 * 2);
    }
}
