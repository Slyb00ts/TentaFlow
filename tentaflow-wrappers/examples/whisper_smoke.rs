// =============================================================================
// Plik: whisper_smoke.rs
// Opis: Minimalny test uruchomienia modelu whisper.cpp przez tentaflow-wrappers.
// Przykład: cargo run --features whisper --example whisper_smoke -- --model model.bin
// =============================================================================

use std::env;
use std::path::PathBuf;

use tentaflow_wrappers::whisper::{WhisperLoadConfig, WhisperRuntime, WhisperTranscribeConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let model = arg_value(&args, "--model").ok_or("missing --model <path>")?;
    let seconds = arg_value(&args, "--silence-seconds")
        .and_then(|value| value.parse::<f32>().ok())
        .unwrap_or(1.0);
    let use_gpu = !args.iter().any(|arg| arg == "--cpu");

    let runtime = WhisperRuntime::load(
        PathBuf::from(model),
        WhisperLoadConfig {
            use_gpu,
            ..Default::default()
        },
    )?;
    let samples = vec![0.0_f32; (seconds.max(0.1) * 16000.0) as usize];
    let output = runtime.transcribe(&WhisperTranscribeConfig::default(), &samples)?;

    println!("loaded: true");
    println!("audio_seconds: {:.2}", output.duration_seconds);
    println!("segments: {}", output.segments.len());
    println!("text: {}", output.text);

    Ok(())
}

fn arg_value<'a>(args: &'a [String], key: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|pair| pair[0] == key)
        .map(|pair| pair[1].as_str())
}
