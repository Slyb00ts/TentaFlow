// ============================================================================
// File: tests/stt_embedded_lazy_load.rs — embedded whisper self-heals on first
//       transcribe even when the SttManager starts empty (post-restart / a
//       service marked `running` without a warm load).
// ============================================================================
//
// Regression guard for the embedded STT load path dropped during the services
// refactor (commit 4f964408): the old deploy runner called
// `SttManager::ensure_and_load`, the new EmbeddedDeploy did not, so embedded
// whisper services were `running` but `active_engine()` stayed `None` and every
// transcription failed with "no STT engine loaded".
//
// Requires the whisper ggml model present on disk (a deployed Whisper STT
// service downloads it to `<data>/tentaflow/models/whisper/`). Marked
// `#[ignore]` so CI without the model does not pull ~1.5 GB. Run manually:
//
//   cargo test --manifest-path tentaflow-core/Cargo.toml \
//     --test stt_embedded_lazy_load -- --ignored --nocapture

use std::sync::Arc;

use tentaflow_core::api::openai::types::{SttRequestOptions, TranscriptionRequest};
use tentaflow_core::services::stt::SttRuntime;

/// Minimal valid 16 kHz mono 16-bit PCM WAV with `ms` of silence. Symphonia
/// (the decoder behind `decode_to_pcm_f32`) needs a real RIFF/WAVE header.
fn silent_wav_16k(ms: u32) -> Vec<u8> {
    const SAMPLE_RATE: u32 = 16_000;
    const BITS: u16 = 16;
    const CHANNELS: u16 = 1;
    let num_samples = SAMPLE_RATE * ms / 1000;
    let data_len = num_samples * (BITS as u32 / 8) * CHANNELS as u32;
    let byte_rate = SAMPLE_RATE * CHANNELS as u32 * (BITS as u32 / 8);
    let block_align = CHANNELS * (BITS / 8);

    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // audio format = PCM
    wav.extend_from_slice(&CHANNELS.to_le_bytes());
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&BITS.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend(std::iter::repeat(0u8).take(data_len as usize));
    wav
}

fn transcription_request(file: Vec<u8>) -> TranscriptionRequest {
    TranscriptionRequest {
        file: Arc::<[u8]>::from(file),
        filename: "lazy.wav".to_string(),
        model: "whisper-1".to_string(),
        language: Some("en".to_string()),
        prompt: None,
        response_format: None,
        temperature: None,
        timestamp_granularities: None,
        no_speech_threshold: None,
        avg_logprob_threshold: None,
        compression_ratio_threshold: None,
        options: SttRequestOptions::default(),
    }
}

#[tokio::test]
#[ignore = "needs whisper ggml model on disk; run with --ignored"]
async fn embedded_whisper_lazy_loads_on_first_transcribe() {
    let runtime = SttRuntime::new();
    assert!(
        !runtime.is_available_sync(),
        "fresh SttRuntime must start with no engine loaded"
    );

    let response = runtime
        .transcribe(transcription_request(silent_wav_16k(500)))
        .await
        .expect("transcribe must lazy-load the embedded whisper instead of failing");

    assert!(
        runtime.is_available_sync(),
        "engine must be loaded after the first transcribe"
    );
    // Silence may transcribe to empty or a short hallucination — the assertion
    // that matters is that the call succeeded (engine loaded), not the text.
    println!("lazy-load transcript: {:?}", response.text);
}
