// =============================================================================
// Plik: flow_engine/adapters/stt.rs
// Opis: Adapter wezla STT (Speech-to-Text) - deleguje transkrypcje audio
//       przez SttRuntime (single owner STT path, D.3).
// =============================================================================

use anyhow::{anyhow, bail, Result};
use serde_json::Value;
use std::sync::Arc;
use tracing::{debug, info};

use crate::api::openai::types::{SpeakerSegment, SttRequestOptions, TranscriptionRequest};
use crate::flow_engine::adapters::NodeAdapter;
use crate::flow_engine::dispatcher::SttRuntimeSlot;
use crate::flow_engine::types::FlowContext;

/// Adapter wezla STT - transkrypcja audio na tekst.
///
/// Codex M1 round 2 + L1' round 3: jedyna sciezka to delegacja przez
/// SttRuntime (ten sam owned-STT path co handler `/v1/audio/transcriptions`).
/// Direct QUIC/HTTP fallback usuniety calkowicie. Bez `service_manager` /
/// `config` — adapter potrzebuje wylacznie slotu z runtime.
pub struct SttNodeAdapter {
    stt_runtime: SttRuntimeSlot,
}

impl SttNodeAdapter {
    pub fn new(stt_runtime: SttRuntimeSlot) -> Self {
        Self { stt_runtime }
    }
}

impl NodeAdapter for SttNodeAdapter {
    async fn execute(&self, node_config: &Value, ctx: &mut FlowContext) -> Result<Value> {
        let model_name = node_config
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("whisper")
            .to_string();

        let language = node_config
            .get("language")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        info!(
            model = %model_name,
            "STT adapter: transkrypcja audio"
        );

        // Codex M2' round 3: `resolve_audio_data` zwraca `Some(_)` zawsze
        // gdy caller jawnie podpial sciezke audio (ctx.audio_input lub
        // audio_variable z ctx.variables) — nawet gdy bytes sa puste.
        // Pusty `Some(_)` przechodzi do guard'u ponizej zeby blad
        // sygnalizowal sie jak najwczesniej; legalny "no-op flow bez
        // audio" daje `None`.
        let audio_data = self.resolve_audio_data(node_config, ctx);
        if let Some(ref data) = audio_data {
            if data.is_empty() {
                bail!(
                    "STT adapter: ctx.audio_input dostarczyl 0 bajtow dla modelu '{}' \
                     — caller zglosil sciezke audio bez payloadu",
                    model_name
                );
            }
        }

        let Some(data) = audio_data else {
            // Brak danych audio — legalny no-op (flow z opcjonalnym STT node).
            debug!("STT adapter: brak danych audio w kontekscie");
            return Ok(serde_json::json!({
                "text": "",
                "language": language.unwrap_or_else(|| "pl".to_string()),
                "duration": 0,
                "speakers": Value::Null,
            }));
        };

        // Codex M1 round 2: deleguj przez SttRuntime — ten sam owned-STT
        // path co handler `/v1/audio/transcriptions`. Slot jest pusty
        // tylko podczas Router::new (przed Router::start); wszystkie
        // realne dispatch sciezki wpinaja runtime przed pierwszym
        // executem flow.
        let runtime = self.stt_runtime.read().clone().ok_or_else(|| {
            anyhow!(
                "STT adapter: SttRuntime nie wpiety (Router::start nie wywolany?) \
                 dla modelu '{}'",
                model_name
            )
        })?;

        debug!("STT adapter: deleguje przez SttRuntime ({})", model_name);

        // Codex M3' round 3: mapuj node_config na SttRequestOptions zeby
        // diarization / speaker_identification / timestamps z seedowanego
        // flow ("Audio Chat") faktycznie trafialy do runtime'u zamiast
        // byc cicho dropowane.
        let options = parse_stt_options(node_config);

        let request = TranscriptionRequest {
            file: Arc::from(data.into_boxed_slice()),
            filename: "flow-audio.wav".to_string(),
            model: model_name.clone(),
            language: language.clone(),
            prompt: node_config
                .get("prompt")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            response_format: options.response_format.clone(),
            temperature: node_config
                .get("temperature")
                .and_then(|v| v.as_f64())
                .map(|f| f as f32),
            timestamp_granularities: options
                .timestamps
                .as_ref()
                .map(|t| vec![t.clone()]),
            no_speech_threshold: None,
            avg_logprob_threshold: None,
            compression_ratio_threshold: None,
            options,
        };

        let response = runtime
            .transcribe(request)
            .await
            .map_err(|e| anyhow!("STT adapter: SttRuntime transcribe failed: {}", e))?;

        debug!(
            "STT adapter: transkrypcja OK, {} znakow",
            response.text.len()
        );

        let resolved_lang = response
            .language
            .or(language)
            .unwrap_or_else(|| "pl".to_string());

        Ok(serde_json::json!({
            "text": response.text,
            "language": resolved_lang,
            "duration": response.duration.unwrap_or(0.0),
            "speakers": response.speakers.map(speakers_to_json).unwrap_or(Value::Null),
        }))
    }

    fn node_type(&self) -> &'static str {
        "stt"
    }
}

impl SttNodeAdapter {
    /// Pobiera dane audio z kontekstu flow. Zwraca `Some(_)` gdy caller
    /// jawnie wskazal sciezke audio (nawet pusta — guard adapter'a
    /// odrzuci pusty payload). `None` znaczy "brak audio" (legalny no-op).
    fn resolve_audio_data(&self, node_config: &Value, ctx: &FlowContext) -> Option<Vec<u8>> {
        if let Some(audio_var) = node_config.get("audio_variable").and_then(|v| v.as_str()) {
            if let Some(val) = ctx.variables.get(audio_var) {
                if let Some(b64) = val.as_str() {
                    if let Ok(decoded) =
                        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)
                    {
                        return Some(decoded);
                    }
                }
            }
        }

        // ChatCompletionRequest.audio_input zostaje skopiowane do
        // FlowContext.audio_input przez build_flow_context_inner (R4.B).
        // To jest naturalna sciezka dla audio chat → flow z STT, omija
        // base64-w-stringu w `ctx.variables`.
        if let Some(bytes) = &ctx.audio_input {
            return Some(bytes.clone());
        }

        if let Some(val) = ctx.variables.get("audio_input") {
            if let Some(b64) = val.as_str() {
                if let Ok(decoded) =
                    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)
                {
                    return Some(decoded);
                }
            }
        }

        if let Some(input_from) = node_config.get("input_from").and_then(|v| v.as_str()) {
            if let Some(prev_result) = ctx.node_results.get(input_from) {
                if let Some(b64) = prev_result.get("audio_base64").and_then(|v| v.as_str()) {
                    if let Ok(decoded) =
                        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)
                    {
                        return Some(decoded);
                    }
                }
            }
        }

        None
    }
}

fn parse_stt_options(node_config: &Value) -> SttRequestOptions {
    let bool_field = |key: &str| {
        node_config
            .get(key)
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    };
    let str_field = |key: &str| {
        node_config
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };
    SttRequestOptions {
        speaker_identification: bool_field("speaker_identification"),
        diarization: bool_field("diarization"),
        timestamps: str_field("timestamps"),
        response_format: str_field("response_format"),
    }
}

fn speakers_to_json(speakers: Vec<SpeakerSegment>) -> Value {
    Value::Array(
        speakers
            .into_iter()
            .map(|s| {
                serde_json::json!({
                    "start": s.start,
                    "end": s.end,
                    "text": s.text,
                    "speaker_label": s.speaker_label,
                    "speaker_id": s.speaker_id,
                    "similarity": s.similarity,
                })
            })
            .collect(),
    )
}
