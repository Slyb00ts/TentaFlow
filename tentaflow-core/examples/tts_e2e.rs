// Quick e2e test: Apple TTS + Kokoro Apple MLX integration in same binary.
#![cfg(all(feature = "inference-apple-tts", feature = "inference-mlx-kokoro"))]

use anyhow::Result;
use std::path::Path;
use tentaflow_core::tts::{
    apple_tts::AppleTtsEngine,
    mlx_kokoro::{prepare_model, MlxKokoroEngine},
    SynthesizeParams, TtsEngine,
};

fn write_wav(samples: &[f32], sr: u32, path: &str) -> Result<()> {
    use std::io::Write;
    let n = samples.len(); let bps = 2usize; let ds = n * bps;
    let mut b: Vec<u8> = Vec::with_capacity(44 + ds);
    b.extend(b"RIFF"); b.extend(&((36 + ds) as u32).to_le_bytes());
    b.extend(b"WAVE"); b.extend(b"fmt "); b.extend(&16u32.to_le_bytes());
    b.extend(&1u16.to_le_bytes()); b.extend(&1u16.to_le_bytes());
    b.extend(&sr.to_le_bytes()); b.extend(&(sr * bps as u32).to_le_bytes());
    b.extend(&(bps as u16).to_le_bytes()); b.extend(&16u16.to_le_bytes());
    b.extend(b"data"); b.extend(&(ds as u32).to_le_bytes());
    for s in samples { b.extend(&((s.clamp(-1.0,1.0) * 32767.0) as i16).to_le_bytes()); }
    std::fs::File::create(path)?.write_all(&b)?; Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = tracing_subscriber::fmt::try_init();
    println!("=== Apple TTS ===");
    let mut apple = AppleTtsEngine::new();
    apple.load_model(Path::new("system"))?;
    let r = apple.synthesize(SynthesizeParams { text: "Test integracji silnikow".into(), speaker_id: 0, speed: 1.0 })?;
    write_wav(&r.samples, r.sample_rate, "/tmp/tts-test/integ-apple.wav")?;
    println!("apple: {} samples @ {} Hz", r.samples.len(), r.sample_rate);

    println!("=== Kokoro MLX ===");
    let kp = prepare_model("mlx-community/Kokoro-82M-bf16").await?;
    let mut k = MlxKokoroEngine::new();
    k.set_default_voice("af_bella");
    k.load_model(&kp)?;
    let r = k.synthesize(SynthesizeParams { text: "Both engines work in the same Rust binary.".into(), speaker_id: 0, speed: 1.0 })?;
    write_wav(&r.samples, r.sample_rate, "/tmp/tts-test/integ-kokoro.wav")?;
    println!("kokoro: {} samples @ {} Hz", r.samples.len(), r.sample_rate);
    Ok(())
}
