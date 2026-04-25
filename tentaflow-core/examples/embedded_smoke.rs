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

#[cfg(not(feature = "inference-llamacpp"))]
fn main() {
    eprintln!("BUILD WITHOUT inference-llamacpp feature — skipping");
    std::process::exit(78);
}

#[cfg(feature = "inference-llamacpp")]
#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> anyhow::Result<()> {
    use std::path::PathBuf;
    use tentaflow_core::inference::{
        llamacpp::LlamaCppEngine, GenerateParams, InferenceEngine,
    };

    let model_path = std::env::args()
        .nth(1)
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
}
