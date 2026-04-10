// =============================================================================
// Plik: examples/test_silero.rs
// Opis: Test load + forward pass Silero VAD na prostym sygnale.
//       Uzyte do weryfikacji czy pipeline koncoza sie bez bledow.
// =============================================================================

use tentaflow_voice::SileroVadStreaming;

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/home/critix/repos/rust/TentaFlow/models/diarization/silero_vad.onnx".to_string());

    println!("Ladowanie modelu: {}", path);
    let mut vad = SileroVadStreaming::from_file(&path)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    println!("Model zaladowany OK\n");

    // Test 1: cisza (wszystkie zera) — oczekujemy niskie prob
    let silence = vec![0.0_f32; 512];
    let prob_silence = vad.predict(&silence).map_err(|e| anyhow::anyhow!("{}", e))?;
    println!("Cisza (zera):       prob = {:.6}", prob_silence);

    vad.reset();

    // Test 2: bialy szum (losowe) — oczekujemy srednie
    let noise: Vec<f32> = (0..512)
        .map(|i| (((i * 17) % 100) as f32 / 100.0 - 0.5) * 0.1)
        .collect();
    let prob_noise = vad.predict(&noise).map_err(|e| anyhow::anyhow!("{}", e))?;
    println!("Bialy szum:         prob = {:.6}", prob_noise);

    vad.reset();

    // Test 3: sinusoida 440 Hz (ton glosu) — oczekujemy wysokie
    let sine: Vec<f32> = (0..512)
        .map(|i| {
            let t = i as f32 / 16000.0;
            (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.5
        })
        .collect();
    let prob_sine = vad.predict(&sine).map_err(|e| anyhow::anyhow!("{}", e))?;
    println!("Sinus 440Hz:        prob = {:.6}", prob_sine);

    vad.reset();

    // Test 4: kilka chunkow sinusa (streaming)
    println!("\nStreaming 5 chunkow sinusa:");
    for i in 0..5 {
        let offset = i * 512;
        let chunk: Vec<f32> = (0..512)
            .map(|j| {
                let t = (offset + j) as f32 / 16000.0;
                (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.5
            })
            .collect();
        let prob = vad.predict(&chunk).map_err(|e| anyhow::anyhow!("{}", e))?;
        println!("  chunk {}: prob = {:.6}", i, prob);
    }

    // Benchmark: 100 chunkow
    println!("\nBenchmark 100 chunkow ciszy...");
    vad.reset();
    let start = std::time::Instant::now();
    for _ in 0..100 {
        vad.predict(&silence).map_err(|e| anyhow::anyhow!("{}", e))?;
    }
    let elapsed = start.elapsed();
    let per_chunk_us = elapsed.as_micros() / 100;
    println!(
        "100 chunkow: {:.2}ms total, {}us per chunk (chunk = 32ms audio)",
        elapsed.as_secs_f64() * 1000.0,
        per_chunk_us
    );
    let rtf = (per_chunk_us as f64) / 32_000.0;
    println!("Real-time factor: {:.4}x (nizsze = szybsze)", rtf);

    Ok(())
}
