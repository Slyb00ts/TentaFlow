// =============================================================================
// Plik: examples/embedded_smoke.rs
// Opis: Smoke test embedded inference engines (llama-cpp + whisper) — laduje
//       lokalny GGUF model, generuje krotka odpowiedz, raportuje sukces.
//       Inny code path niz python_venv::deploy() — embedded engines nie sa
//       Python bundle, sa wkompilowane przez Cargo features.
//
// Uzycie:
//   cargo run --example embedded_smoke --no-default-features \
//     --features inference-llamacpp,gpu-cuda,dashboard-api \
//     -- /mnt/d/models/Qwen3.5-0.8B-Q4_0.gguf
//
//   cargo run --example embedded_smoke --no-default-features \
//     --features inference-whisper,gpu-cuda-whisper,dashboard-api \
//     -- whisper /path/to/sample.wav
// =============================================================================

#[cfg(not(any(feature = "inference-llamacpp", feature = "inference-sherpa")))]
fn main() {
    eprintln!("BUILD WITHOUT inference-llamacpp/inference-sherpa feature — skipping");
    std::process::exit(78);
}

#[cfg(feature = "inference-sherpa")]
fn run_sherpa(model_dir: std::path::PathBuf) -> anyhow::Result<()> {
    use std::time::Instant;
    use tentaflow_core::tts::{
        sherpa::SherpaTtsEngine, SynthesizeParams, TtsEngine,
    };

    eprintln!("=== embedded_smoke: sherpa-onnx ===");
    eprintln!("model dir = {}", model_dir.display());

    let started = Instant::now();
    let mut engine = SherpaTtsEngine::new();
    let info = engine.load_model(&model_dir)?;
    eprintln!(
        "+++ loaded in {:.1}s — name={} backend={}",
        started.elapsed().as_secs_f32(),
        info.name,
        info.backend,
    );

    let gen_started = Instant::now();
    let result = engine.synthesize(SynthesizeParams {
        text: "Hello world, this is a sherpa onnx test.".to_string(),
        speaker_id: 0,
        speed: 1.0,
    })?;
    eprintln!(
        "+++ synthesize ok in {:.2}s — samples={} sample_rate={}",
        gen_started.elapsed().as_secs_f32(),
        result.samples.len(),
        result.sample_rate,
    );

    println!(
        "=== SUMMARY sherpa-onnx embedded: load_ok=true synthesize_ok=true total={:.1}s samples={} sr={} ===",
        started.elapsed().as_secs_f32(),
        result.samples.len(),
        result.sample_rate,
    );
    Ok(())
}

#[cfg(any(feature = "inference-llamacpp", feature = "inference-sherpa"))]
#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> anyhow::Result<()> {
    use std::path::PathBuf;

    // Pierwszy arg moze byc 'sherpa' jako tryb TTS — wtedy drugi to dir
    // modelu VITS Piper. Bez 'sherpa' uruchamiamy llama-cpp na GGUF.
    let mut args = std::env::args().skip(1);
    let first = args.next();
    if first.as_deref() == Some("sherpa") {
        #[cfg(feature = "inference-sherpa")]
        {
            let model_dir = args
                .next()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/mnt/d/models/sherpa-vits-piper-en"));
            return run_sherpa(model_dir);
        }
        #[cfg(not(feature = "inference-sherpa"))]
        {
            anyhow::bail!("rebuild with --features inference-sherpa to use 'sherpa' mode");
        }
    }

    #[cfg(not(feature = "inference-llamacpp"))]
    {
        anyhow::bail!("rebuild with --features inference-llamacpp to use llama-cpp mode");
    }

    #[cfg(feature = "inference-llamacpp")]
    {
        use tentaflow_core::inference::{
            llamacpp::LlamaCppEngine, GenerateParams, InferenceEngine,
        };

    let model_path = first
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/mnt/d/models/Qwen3.5-0.8B-Q4_0.gguf"));

    if !model_path.exists() {
        anyhow::bail!("model file not found: {}", model_path.display());
    }
    eprintln!("=== embedded_smoke: llama-cpp ===");
    eprintln!("model = {}", model_path.display());

    let started = std::time::Instant::now();
    let engine = LlamaCppEngine::new();
    let info = engine.load_model(&model_path, Some(99)).await?;
    eprintln!(
        "+++ loaded in {:.1}s — name={} ctx={} backend={} vram={}MB",
        started.elapsed().as_secs_f32(),
        info.name,
        info.context_length,
        info.backend,
        info.vram_used_mb,
    );

    let params = GenerateParams {
        prompt: "Reply with exactly one word: OK".to_string(),
        max_tokens: 16,
        temperature: 0.0,
        top_p: 1.0,
        top_k: 1,
        repeat_penalty: 1.0,
        stop_sequences: vec![],
        system_prompt: None,
    };

    let gen_started = std::time::Instant::now();
    let result = engine.generate(params).await?;
    let gen_elapsed = gen_started.elapsed();
    eprintln!(
        "+++ generate ok in {:.2}s — text={:?} tokens={}",
        gen_elapsed.as_secs_f32(),
        result.text,
        result.tokens_generated,
    );

    println!(
        "=== SUMMARY llama-cpp embedded: load_ok=true generate_ok=true total={:.1}s text={:?} ===",
        started.elapsed().as_secs_f32(),
        result.text,
    );
    Ok(())
    } // cfg(feature = "inference-llamacpp")
}
