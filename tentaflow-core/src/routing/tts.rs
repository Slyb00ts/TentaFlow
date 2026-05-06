// =============================================================================
// Plik: routing/tts.rs
// Opis: Synteza mowy (TTS) — synthesize_speech (blocking) oraz
//       synthesize_speech_stream (chunkowane PCM dla niskiej latencji
//       pierwszej probki audio).
// =============================================================================

use crate::error::{CoreError, Result};
use crate::routing::router::Router;

use tracing::debug;

/// TTS dispatch payload — bytes plus the actual container/codec the
/// backend produced. Codex R3b.4 M2: callers (HTTP handler) need the
/// **actual** format to set `Content-Type` correctly when an embedded
/// engine ignores the requested format and emits WAV.
#[derive(Debug, Clone)]
pub struct TtsBytes {
    pub bytes: Vec<u8>,
    pub format: String,
}

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
        user: Option<crate::auth::acl::UserContext>,
    ) -> Result<crate::routing::RouteResult<TtsBytes>> {
        if let Some(ref u) = user {
            if let Some(ref db) = self.db {
                if !crate::auth::acl::check_access_safe(
                    db,
                    "model",
                    &request.model,
                    u.user_id,
                    &u.role,
                ) {
                    tracing::warn!(user_id = u.user_id, model = %request.model, "ACL denied TTS model");
                    return Err(crate::error::CoreError::ModelNotFound {
                        model_name: request.model.clone(),
                    }
                    .into());
                }
            }
        }
        // Propaguj user dalej — Etap 2 odblokował TTS-as-flow, więc
        // FlowDispatcher::acl_allow musi widzieć rzeczywistego callera, a
        // resolver/strategy w ModelRuntimeExecutor mogą gateować per-user.
        self.synthesize_speech(request, user).await
    }

    pub async fn synthesize_speech(
        &self,
        request: &crate::api::openai::types::TTSRequest,
        user: Option<crate::auth::acl::UserContext>,
    ) -> Result<crate::routing::RouteResult<TtsBytes>> {
        // Strip emoji + apply DB-driven `tts_cleaning_rules` BEFORE
        // dispatch. Without this the TTS engine has to pronounce raw
        // emoji / abbreviations / odd patterns and produces silence or
        // mangled prosody. The router cleans here (not the executor)
        // because the rules are DB-backed and per-deployment.
        let cleaned_input = if let Some(ref db) = self.db {
            crate::tts::clean_cache::clean(&request.input, db)
        } else {
            request.input.clone()
        };
        let cleaned_request = crate::api::openai::types::TTSRequest {
            input: cleaned_input,
            ..request.clone()
        };

        debug!(
            "synthesize_speech: model={}, voice={}, format={:?}, input_len={}",
            cleaned_request.model,
            cleaned_request.voice,
            cleaned_request.response_format,
            cleaned_request.input.len()
        );

        let tts_model = cleaned_request.model.clone();
        let t = std::time::Instant::now();
        let executor_snapshot = self.executor.read().clone();
        if let Some(executor) = executor_snapshot {
            use crate::services::runtime::context::ExecutionContext;
            use crate::services::runtime::executor::ExecutorError;

            let mut exec_ctx = ExecutionContext {
                user: user.clone(),
                ..ExecutionContext::default()
            };
            match executor
                .execute_tts(cleaned_request.clone(), &mut exec_ctx)
                .await
            {
                Ok(result) => {
                    if let Some(req_fmt) = cleaned_request.response_format.as_deref() {
                        if req_fmt.eq_ignore_ascii_case(&result.format) == false {
                            tracing::warn!(
                                requested = %req_fmt,
                                actual = %result.format,
                                model = %cleaned_request.model,
                                "TTS backend returned different format than requested — caller's Content-Type may be misleading"
                            );
                        }
                    }
                    let metadata = crate::routing::RouteMetadata {
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
                        latency_ms: Some(t.elapsed().as_secs_f64() * 1000.0),
                    usage: None,
                    finish_reason: None,
                    };
                    return Ok(crate::routing::RouteResult {
                        response: TtsBytes {
                            bytes: result.bytes,
                            format: result.format,
                        },
                        metadata,
                    });
                }
                Err(ExecutorError::TransportPendingCutover(_)) => {
                    debug!(
                        "executor returned TransportPendingCutover for TTS — falling back to legacy mesh dispatch"
                    );
                    // fall through
                }
                Err(e) => {
                    self.log_tts_dispatch_diagnostics(&tts_model);
                    return Err(map_tts_executor_err(e, &tts_model).into());
                }
            }
        }

        // Executor not wired (DB-less router). After R3b.8 the legacy
        // mesh fallback is gone — without an executor we surface a typed
        // error instead of doing duplicate dispatch.
        self.log_tts_dispatch_diagnostics(&tts_model);
        Err(CoreError::AllBackendsUnavailable {
            model_name: tts_model,
        }
        .into())
    }

    /// User-facing diagnostic for "no TTS backend" failures. Inspects the
    /// alias table so an operator hitting `teams-tts` sees whether the
    /// alias is missing, points at an empty target, or points at a service
    /// without a working backend. Codex R3b.4 M1: shared between the
    /// executor failure path and the legacy mesh fallback so both paths
    /// emit the same hint instead of only the mesh fallback doing so.
    fn log_tts_dispatch_diagnostics(&self, tts_model: &str) {
        let Some(ref db) = self.db else { return };
        match crate::db::repository::resolve_model_alias(db, tts_model) {
            Ok(Some(alias)) => {
                if alias.target_model.trim().is_empty() {
                    tracing::warn!(
                        alias = %tts_model,
                        "TTS: alias exists but target_model is empty — deploy any TTS (Apple/Kokoro/Sherpa) so it auto-wires"
                    );
                } else {
                    tracing::warn!(
                        alias = %tts_model,
                        target = %alias.target_model,
                        "TTS: alias points to '{}' but the service has no working backend (deploy?, mesh disconnect?)",
                        alias.target_model
                    );
                }
            }
            Ok(None) => {
                tracing::warn!(
                    model = %tts_model,
                    "TTS: model is not an alias or service — check whether the TTS service is deployed"
                );
            }
            Err(e) => {
                tracing::warn!(
                    model = %tts_model,
                    error = %e,
                    "TTS: failed to inspect alias table for diagnostics"
                );
            }
        }
    }

    pub async fn synthesize_speech_stream<F>(
        &self,
        request: &crate::api::openai::types::TTSRequest,
        user: Option<crate::auth::acl::UserContext>,
        mut chunk_sink: F,
    ) -> Result<()>
    where
        F: FnMut(Vec<u8>) -> Result<()>,
    {
        let route_result = self.synthesize_speech(request, user).await?;
        let mut audio_bytes = route_result.response.bytes;

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

/// Map executor errors onto typed `CoreError` variants. Mirror of
/// `executor_err_to_core` from routing/embeddings.rs but with the
/// `CapabilityUnsupported` variant routed to `InvalidRequest` (Codex
/// R3b.4 L1) — a model that has no audio-output candidate is a client
/// misconfiguration, not an internal failure.
fn map_tts_executor_err(
    err: crate::services::runtime::executor::ExecutorError,
    model: &str,
) -> CoreError {
    use crate::services::runtime::executor::ExecutorError;
    use crate::services::runtime::resolver::ResolveError;
    match err {
        ExecutorError::Resolve(ResolveError::UnknownModel(m)) => {
            CoreError::ModelNotFound { model_name: m }
        }
        ExecutorError::Resolve(ResolveError::CapabilityUnsupported { requested, .. }) => {
            CoreError::InvalidRequest {
                message: format!(
                    "TTS model '{}' has no candidate that emits audio output",
                    requested
                ),
                details: None,
            }
        }
        ExecutorError::Resolve(other) => CoreError::InternalError {
            message: format!("TTS alias resolution: {}", other),
            source: None,
        },
        ExecutorError::AllCandidatesFailed { .. } => CoreError::AllBackendsUnavailable {
            model_name: model.to_string(),
        },
        ExecutorError::TransportPendingCutover(_) => CoreError::AllBackendsUnavailable {
            model_name: model.to_string(),
        },
        ExecutorError::FlowDispatcherUnavailable
        | ExecutorError::FlowEmptyResult { .. }
        | ExecutorError::Internal(_)
        | ExecutorError::SttRuntimeUnavailable
        | ExecutorError::SttBackend(_) => CoreError::InternalError {
            message: format!("executor.execute_tts: {}", err),
            source: None,
        },
    }
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
