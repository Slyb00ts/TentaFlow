// =============================================================================
// File: protocol/inference.rs — inference host-function ABI payloads
// Purpose: single source of truth for the CBOR request/response structs of the
// LLM streaming host functions (`llm_generate_stream_next_v1`) and the STT
// host function (`stt_transcribe_v1`). Shared verbatim by the core host
// (decode input / encode output) and the addon SDK (encode input / decode
// output) so the wire format cannot drift between the two.
// =============================================================================

use minicbor::{Decode, Encode};

// -----------------------------------------------------------------------------
// LLM streaming
// -----------------------------------------------------------------------------

/// Input for `llm_generate_stream_next`. `callback_id` is the handle returned
/// by `llm_generate_stream_start` (> 0). `timeout_ms` bounds the blocking wait
/// for the FIRST chunk of the batch; it is clamped to the host ceiling
/// (30 000 ms) after decode. Once at least one chunk arrived the host drains
/// everything already queued without further waiting (batch semantics).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct LlmStreamNextInput {
    #[n(0)]
    pub callback_id: i32,
    #[n(1)]
    pub timeout_ms: u64,
}

/// Output of `llm_generate_stream_next`.
///
/// * `chunks` — zero or more text deltas, in generation order. Empty with
///   `finished == false` means the wait timed out (poll again).
/// * `finished` — the stream ended; the callback_id is invalid afterwards.
/// * `finish_reason` — populated on the final batch when the backend reported
///   one (`stop`, `length`, `error`, ...).
/// * `error` — populated instead of further chunks when generation failed;
///   always accompanied by `finished == true`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct LlmStreamNextOutput {
    #[n(0)]
    pub chunks: Vec<String>,
    #[n(1)]
    pub finished: bool,
    #[n(2)]
    pub finish_reason: Option<String>,
    #[n(3)]
    pub error: Option<String>,
}

// -----------------------------------------------------------------------------
// STT
// -----------------------------------------------------------------------------

/// Input for `stt_transcribe_v1`. `audio` carries the encoded audio inline
/// (WAV/Opus/MP3 — anything the STT backend accepts); the host enforces the
/// `PayloadKind::AudioInline` ceiling (25 MiB) before copying. `model` empty /
/// absent routes to the default local STT engine (same fallback as
/// `/v1/audio/transcriptions` without a model).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct SttTranscribeInput {
    #[cbor(n(0), with = "minicbor::bytes")]
    pub audio: Vec<u8>,
    #[n(1)]
    pub mime: String,
    #[n(2)]
    pub sample_rate: Option<u32>,
    #[n(3)]
    pub model: Option<String>,
    #[n(4)]
    pub language: Option<String>,
    #[n(5)]
    pub prompt: Option<String>,
}

/// Output of `stt_transcribe_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct SttTranscribeOutput {
    #[n(0)]
    pub text: String,
    #[n(1)]
    pub detected_language: Option<String>,
    #[n(2)]
    pub duration_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T>(value: &T)
    where
        T: Encode<()> + for<'b> Decode<'b, ()> + PartialEq + std::fmt::Debug,
    {
        let bytes = minicbor::to_vec(value).expect("encode");
        let decoded: T = minicbor::decode(&bytes).expect("decode");
        assert_eq!(&decoded, value);
    }

    #[test]
    fn llm_stream_next_roundtrip() {
        roundtrip(&LlmStreamNextInput {
            callback_id: 7,
            timeout_ms: 2500,
        });
        roundtrip(&LlmStreamNextOutput {
            chunks: vec!["Hel".into(), "lo".into()],
            finished: false,
            finish_reason: None,
            error: None,
        });
        roundtrip(&LlmStreamNextOutput {
            chunks: vec![],
            finished: true,
            finish_reason: Some("stop".into()),
            error: None,
        });
        roundtrip(&LlmStreamNextOutput {
            chunks: vec![],
            finished: true,
            finish_reason: Some("error".into()),
            error: Some("backend unavailable".into()),
        });
    }

    #[test]
    fn stt_transcribe_roundtrip() {
        roundtrip(&SttTranscribeInput {
            audio: vec![0x52, 0x49, 0x46, 0x46],
            mime: "audio/wav".into(),
            sample_rate: Some(16_000),
            model: None,
            language: Some("pl".into()),
            prompt: None,
        });
        roundtrip(&SttTranscribeOutput {
            text: "dzień dobry".into(),
            detected_language: Some("pl".into()),
            duration_ms: Some(1200),
        });
    }
}
