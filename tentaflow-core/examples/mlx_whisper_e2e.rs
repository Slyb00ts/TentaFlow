// =============================================================================
// Plik: examples/mlx_whisper_e2e.rs
// Opis: End-to-end test: prepare_model (HF download + merge) → load_model
//       (Swift dylib) → transcribe na zadanym WAV. Uruchamiac:
//
//         cargo run --release --example mlx_whisper_e2e --features inference-mlx-whisper -- \
//           mlx-community/whisper-large-v3-turbo-4bit /tmp/whisper-test/long.wav en
// =============================================================================

#![cfg(feature = "inference-mlx-whisper")]

use std::path::PathBuf;

use anyhow::Result;
use tentaflow_core::stt::{
    mlx_whisper::{prepare_model, MlxWhisperEngine},
    SttEngine, TranscribeParams,
};

#[tokio::main]
async fn main() -> Result<()> {
    let _ = tracing_subscriber::fmt::try_init();
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "Usage: {} <hf_repo_id_or_path> <wav_file> [language]",
            args[0]
        );
        std::process::exit(2);
    }
    let model_arg = &args[1];
    let wav = PathBuf::from(&args[2]);
    let language = args.get(3).cloned().unwrap_or_else(|| "en".to_string());

    println!("[e2e] preparing model: {}", model_arg);
    let model_path = if std::path::Path::new(model_arg).exists() {
        PathBuf::from(model_arg)
    } else {
        prepare_model(model_arg).await?
    };
    println!("[e2e] model_path = {}", model_path.display());

    let engine = MlxWhisperEngine::new();
    println!("[e2e] loading model into Swift bridge...");
    let info = engine.load_model(&model_path, None).await?;
    println!(
        "[e2e] loaded: backend={} size={} MB",
        info.backend,
        info.size_bytes / 1_048_576
    );

    let audio_data = std::fs::read(&wav)?;
    println!("[e2e] transcribing {} ({} bytes)...", wav.display(), audio_data.len());
    let params = TranscribeParams {
        audio_data,
        language: Some(language.clone()),
        ..Default::default()
    };
    let result = engine.transcribe(params).await?;
    println!("===");
    println!("LANG: {}", result.language);
    println!("DURATION: {:.2}s", result.duration_seconds);
    println!("TEXT: {}", result.text);

    engine.unload_model().await?;
    Ok(())
}
