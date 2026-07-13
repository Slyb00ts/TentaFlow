// =============================================================================
// Plik: addon/host_functions/stt.rs
// Opis: Host function STT — transkrypcja audio przez te sama sciezke co node
//       stt flow engine (Router::route_audio_transcription_for_user →
//       FlowDispatcher → SttDispatcher → executor.execute_stt).
// Uprawnienia: "stt" (fail-closed, audit log per outcome). Audio inline z
//       pamieci gościa z limitem PayloadKind::AudioInline (25 MiB).
// =============================================================================

use tracing::{info, warn};

use tentaflow_sdk_spec::{SttTranscribeInput, SttTranscribeOutput};

use super::abi_helpers::PayloadKind;
use super::cbor_io::{read_input_cbor, write_cbor_capped};
use super::{audit_log, check_permission, get_memory, AddonState, WasmCaller};
use crate::addon::errors::AbiError;
use crate::addon::rate_limiter::ResourceType;
use crate::api::openai::types::{SttRequestOptions, TranscriptionRequest};

const PERM_STT: &str = "stt";

/// Rozszerzenie nazwy pliku z MIME — backend STT rozpoznaje format po nazwie
/// pliku (gateway multipart robi to samo).
fn filename_for_mime(mime: &str) -> &'static str {
    match mime {
        "audio/wav" | "audio/x-wav" | "audio/wave" => "audio.wav",
        "audio/ogg" | "audio/opus" => "audio.ogg",
        "audio/webm" => "audio.webm",
        "audio/mpeg" | "audio/mp3" => "audio.mp3",
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" => "audio.m4a",
        "audio/flac" | "audio/x-flac" => "audio.flac",
        _ => "audio.bin",
    }
}

/// Host function: transkrybuje audio przez skonfigurowana sciezke STT.
///
/// ABI (CBOR):
/// - input: `SttTranscribeInput { audio, mime, sample_rate?, model?, language?, prompt? }`
/// - output: `SttTranscribeOutput { text, detected_language?, duration_ms? }`
/// - Zwraca: AbiError (0 = OK)
///
/// Pusty / nieobecny `model` trafia do domyslnego lokalnego silnika STT (ten
/// sam fallback co `/v1/audio/transcriptions` bez modelu).
pub fn stt_transcribe_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };

    // Fail-closed: uprawnienie "stt" PRZED odczytem audio.
    if !check_permission(caller.data(), PERM_STT, None) {
        audit_log(
            caller.data(),
            "stt.transcribe",
            Some("stt"),
            None,
            "denied",
            None,
        );
        return AbiError::Permission.as_i32();
    }

    let input: SttTranscribeInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::AudioInline,
    ) {
        Ok(v) => v,
        Err(e) => {
            audit_log(
                caller.data(),
                "stt.transcribe",
                Some("stt"),
                None,
                "error",
                Some(if e == AbiError::PayloadTooLarge {
                    "payload_too_large"
                } else {
                    "invalid_payload"
                }),
            );
            return e.as_i32();
        }
    };

    if input.audio.is_empty() {
        audit_log(
            caller.data(),
            "stt.transcribe",
            Some("stt"),
            None,
            "error",
            Some("empty_audio"),
        );
        return AbiError::Operation.as_i32();
    }

    let addon_id = caller.data().addon_id.clone();

    // Rate limit — audio przybliżamy do tokenow LLM po rozmiarze wejscia
    // (wspolny budzet inferencji addonu).
    if let Some(ref rate_limiter) = caller.data().rate_limiter {
        if rate_limiter
            .check(&addon_id, ResourceType::LlmTokens)
            .is_err()
        {
            audit_log(
                caller.data(),
                "stt.transcribe",
                Some("stt"),
                input.model.as_deref(),
                "error",
                Some("rate limit exceeded"),
            );
            return AbiError::QuotaExceeded.as_i32();
        }
    }

    let router = match caller.data().router.as_ref() {
        Some(r) => r.clone(),
        None => {
            warn!(
                "stt_transcribe: router niedostepny dla addon='{}'",
                addon_id
            );
            audit_log(
                caller.data(),
                "stt.transcribe",
                Some("stt"),
                input.model.as_deref(),
                "error",
                Some("router unavailable"),
            );
            return AbiError::Operation.as_i32();
        }
    };

    let model = input.model.clone().unwrap_or_default();
    info!(
        "stt_transcribe: addon='{}', model='{}', mime='{}', bytes={}",
        addon_id,
        model,
        input.mime,
        input.audio.len()
    );

    let request = TranscriptionRequest {
        file: input.audio.into(),
        filename: filename_for_mime(&input.mime).to_string(),
        model,
        language: input.language.clone(),
        prompt: input.prompt.clone(),
        response_format: Some("verbose_json".to_string()),
        temperature: None,
        timestamp_granularities: None,
        no_speech_threshold: None,
        avg_logprob_threshold: None,
        compression_ratio_threshold: None,
        options: SttRequestOptions::default(),
    };

    // Most async→sync jak w llm_generate — TA SAMA sciezka co node stt
    // (FlowDispatcher: jawny flow albo direct STT execution).
    let result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(router.route_audio_transcription_for_user(request, None))
    });

    let response = match result {
        Ok(rr) => rr.response,
        Err(e) => {
            warn!(
                "stt_transcribe: blad transkrypcji dla addon='{}': {}",
                addon_id, e
            );
            audit_log(
                caller.data(),
                "stt.transcribe",
                Some("stt"),
                None,
                "error",
                Some(&e.to_string()),
            );
            return AbiError::Operation.as_i32();
        }
    };

    if let Some(ref rate_limiter) = caller.data().rate_limiter {
        let estimated_tokens = (response.text.len() / 4).max(1) as u64;
        rate_limiter.record_usage(&addon_id, ResourceType::LlmTokens, estimated_tokens);
    }

    audit_log(
        caller.data(),
        "stt.transcribe",
        Some("stt"),
        None,
        "ok",
        None,
    );

    let out = SttTranscribeOutput {
        text: response.text,
        detected_language: response.language,
        duration_ms: response
            .duration
            .filter(|d| d.is_finite() && *d >= 0.0)
            .map(|d| (d * 1000.0) as u64),
    };

    write_cbor_capped(
        &memory,
        &mut caller,
        &out,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::ServiceCall,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_maps_known_mimes() {
        assert_eq!(filename_for_mime("audio/wav"), "audio.wav");
        assert_eq!(filename_for_mime("audio/ogg"), "audio.ogg");
        assert_eq!(filename_for_mime("audio/mpeg"), "audio.mp3");
        assert_eq!(filename_for_mime("application/octet-stream"), "audio.bin");
    }
}
