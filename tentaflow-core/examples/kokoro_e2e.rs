// =============================================================================
// Plik: examples/kokoro_e2e.rs
// Opis: End-to-end Kokoro: prepare_model (HF download) -> load_model (Swift
//       bridge) -> synthesize -> zapisz WAV.
//
//   cargo run --release --example kokoro_e2e \
//     --features "inference-mlx-kokoro dashboard-api" -- \
//     "Hello world" af_heart /tmp/whisper-test/kokoro.wav
// =============================================================================

#![cfg(feature = "inference-mlx-kokoro")]

use anyhow::Result;
use std::path::PathBuf;
use tentaflow_core::tts::{
    mlx_kokoro::{prepare_model, MlxKokoroEngine},
    SynthesizeParams, TtsEngine,
};

fn write_wav(samples: &[f32], sample_rate: u32, path: &str) -> Result<()> {
    use std::io::Write;
    let n = samples.len();
    let bytes_per_sample = 2usize;
    let data_size = n * bytes_per_sample;
    let mut buf: Vec<u8> = Vec::with_capacity(44 + data_size);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + data_size as u32).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());     // PCM
    buf.extend_from_slice(&1u16.to_le_bytes());     // mono
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&(sample_rate as u32 * bytes_per_sample as u32).to_le_bytes());
    buf.extend_from_slice(&(bytes_per_sample as u16).to_le_bytes());
    buf.extend_from_slice(&16u16.to_le_bytes());    // bits/sample
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&(data_size as u32).to_le_bytes());
    for s in samples {
        let clamped = s.clamp(-1.0, 1.0);
        let i16v = (clamped * 32767.0) as i16;
        buf.extend_from_slice(&i16v.to_le_bytes());
    }
    let mut f = std::fs::File::create(path)?;
    f.write_all(&buf)?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = tracing_subscriber::fmt::try_init();
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("Usage: {} <text> <voice> <out.wav> [language]", args[0]);
        std::process::exit(2);
    }
    let text = &args[1];
    let voice = &args[2];
    let out_path = &args[3];
    let language = args.get(4).cloned().unwrap_or_else(|| "en-us".to_string());

    println!("[e2e] preparing kokoro model");
    let model_path: PathBuf = prepare_model("mlx-community/Kokoro-82M-bf16").await?;
    println!("[e2e] model = {}", model_path.display());

    let mut engine = MlxKokoroEngine::new();
    engine.set_default_voice(voice.clone());
    let info = engine.load_model(&model_path)?;
    println!("[e2e] loaded: backend={} sr={}", info.backend, info.sample_rate);

    let result = engine.synthesize(SynthesizeParams {
        text: text.clone(),
        speaker_id: 0,
        speed: 1.0,
    })?;
    println!("[e2e] {} samples @ {} Hz", result.samples.len(), result.sample_rate);
    write_wav(&result.samples, result.sample_rate, out_path)?;
    println!("[e2e] saved: {}", out_path);
    Ok(())
}
