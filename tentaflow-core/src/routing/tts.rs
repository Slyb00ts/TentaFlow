// =============================================================================
// Plik: routing/tts.rs
// Opis: Synteza mowy (TTS) — blocking `synthesize_speech`. Streaming idzie
//       przez flow_engine `TtsDispatcher::stream_synthesize` (Etap 3c),
//       a HTTP endpoint /v1/audio/speech/stream w api/openai/server.rs.
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

        // Stage 3d-0b-2: TTS path zawsze przez FlowDispatcher (Universal
        // Flow Gateway). Synthetic flow `trigger → tts(model) → output`
        // aktywuje się gdy admin nie skonfigurował user-defined flow.
        // Direct executor.execute_tts zostaje jako fallback (CompileFailed
        // / no flow_dispatcher) — będzie wycięte w finalnym 0b commit.
        if let Some(ref dispatcher) = self.flow_dispatcher {
            let (initial, meta) = crate::services::runtime::executor::tts_request_to_initial_envelope(
                &cleaned_request,
                user.clone(),
            );
            match dispatcher.try_dispatch(&cleaned_request.model, "tts", initial, meta).await {
                Ok(Some(outcome)) => {
                    let result = crate::services::runtime::executor::flow_outcome_to_tts_result(
                        outcome,
                        dispatcher.blobs(),
                    )
                    .await
                    .map_err(|e| crate::error::CoreError::InternalError {
                        message: format!("tts flow result: {e}"),
                        source: None,
                    })?;
                    if let Some(req_fmt) = cleaned_request.response_format.as_deref() {
                        if !req_fmt.eq_ignore_ascii_case(&result.format) {
                            tracing::warn!(
                                requested = %req_fmt,
                                actual = %result.format,
                                model = %cleaned_request.model,
                                "TTS flow returned different format than requested"
                            );
                        }
                    }
                    let metadata = crate::routing::RouteMetadata {
                        served_by_node: hostname::get()
                            .map(|h| h.to_string_lossy().to_string())
                            .unwrap_or_else(|_| "unknown".to_string()),
                        backend_type: "flow_engine".to_string(),
                        strategy_used: "flow_dispatch".to_string(),
                        fallbacks_tried: 0,
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
                Ok(None) => {
                    // Stage 3d-0b-final: Ok(None) = CompileFailed albo
                    // unsupported service_type. Brak fallback do executor —
                    // klient dostaje 500. Admin musi naprawić flow_json
                    // albo sprawdzić synthetic builder.
                    return Err(crate::error::CoreError::InternalError {
                        message: format!(
                            "flow_dispatcher returned no result for tts model '{}' — \
                             user-defined flow nie kompiluje się albo synthetic builder \
                             nie wspiera service_type='tts'",
                            cleaned_request.model
                        ),
                        source: None,
                    }
                    .into());
                }
                Err(e) => {
                    self.log_tts_dispatch_diagnostics(&tts_model);
                    return Err(crate::error::CoreError::InternalError {
                        message: format!("tts flow dispatch: {e}"),
                        source: None,
                    }
                    .into());
                }
            }
        }

        // Stage 3d-0b-final: brak flow_dispatcher (DB-less router) → 500.
        // Direct executor.execute_tts fallback wycięty. Plan v1.5 wymaga
        // że KAŻDY TTS request przechodzi przez flow_engine (synthetic
        // albo user-defined).
        let _ = t;
        self.log_tts_dispatch_diagnostics(&tts_model);
        Err(crate::error::CoreError::InternalError {
            message: format!(
                "flow_dispatcher not wired for tts model '{}' — DB-less router \
                 nie wspiera Universal Flow Gateway",
                tts_model
            ),
            source: None,
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

}


