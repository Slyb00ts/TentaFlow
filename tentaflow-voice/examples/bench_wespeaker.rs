// =============================================================================
// Plik: bench_wespeaker.rs
// Opis: Mikrobenchmark per-layer WeSpeaker. Pokazuje gdzie dokladnie idzie czas
//       zeby wiedziec co jeszcze warto optymalizowac.
// =============================================================================

use tentaflow_voice::{compute_fbank, WeSpeaker};

const MODEL: &str = "/home/critix/repos/rust/TentaFlow/models/diarization/embedding.onnx";
const AUDIO: &str = "/tmp/test_speech.wav";

fn main() -> anyhow::Result<()> {
    let samples_i16 = read_wav(AUDIO)?;
    let samples: Vec<f32> = samples_i16.iter().map(|&s| s as f32 / 32768.0).collect();
    println!("Audio: {} probek, {:.2}s", samples.len(), samples.len() as f32 / 16000.0);

    let model = WeSpeaker::from_file(MODEL).map_err(|e| anyhow::anyhow!("{}", e))?;

    // Warm up — pierwsza inwokacja JIT'uje rayon pool + cache warm
    for _ in 0..3 {
        let _ = model.extract(&samples).map_err(|e| anyhow::anyhow!("{}", e))?;
    }

    // Measurement — N iteracji, srednia
    const N: u32 = 20;
    let mut times = Vec::with_capacity(N as usize);
    for _ in 0..N {
        let t0 = std::time::Instant::now();
        let _ = model.extract(&samples).map_err(|e| anyhow::anyhow!("{}", e))?;
        times.push(t0.elapsed());
    }
    let sum: std::time::Duration = times.iter().sum();
    let avg = sum / N;
    let min = times.iter().min().unwrap();
    let max = times.iter().max().unwrap();

    println!("\nFull extract ({} iter):", N);
    println!("  avg: {:?}", avg);
    println!("  min: {:?}", min);
    println!("  max: {:?}", max);
    println!("  audio: {:.2}s  RTF: {:.4}x",
        samples.len() as f32 / 16000.0,
        avg.as_secs_f32() / (samples.len() as f32 / 16000.0));

    // Per-layer breakdown via extract_with_timing
    println!("\n=== Per-layer breakdown (20 iter avg) ===");
    let mut sums = tentaflow_voice::LayerTimings::default();
    for _ in 0..N {
        let (_, t) = model.extract_with_timing(&samples).map_err(|e| anyhow::anyhow!("{}", e))?;
        sums.fbank += t.fbank;
        sums.layer1 += t.layer1;
        sums.block2 += t.block2;
        sums.block3 += t.block3;
        sums.block4 += t.block4;
        sums.concat_layers += t.concat_layers;
        sums.aggregation += t.aggregation;
        sums.global_stats += t.global_stats;
        sums.pool_linear1 += t.pool_linear1;
        sums.attention_pool += t.attention_pool;
        sums.final_layers += t.final_layers;
        sums.total += t.total;
    }
    let d = |v: std::time::Duration| v / N;
    println!("  Fbank:             {:>10?}", d(sums.fbank));
    println!("  layer1 (Conv k=5): {:>10?}", d(sums.layer1));
    println!("  SE-Res2 block2:    {:>10?}", d(sums.block2));
    println!("  SE-Res2 block3:    {:>10?}", d(sums.block3));
    println!("  SE-Res2 block4:    {:>10?}", d(sums.block4));
    println!("  Concat layers:     {:>10?}", d(sums.concat_layers));
    println!("  Aggregation Conv:  {:>10?}", d(sums.aggregation));
    println!("  Global mean/std:   {:>10?}", d(sums.global_stats));
    println!("  Pool linear1+tanh: {:>10?}", d(sums.pool_linear1));
    println!("  Attention pool:    {:>10?}", d(sums.attention_pool));
    println!("  Final BN+Linear:   {:>10?}", d(sums.final_layers));
    println!("  ---");
    println!("  Total:             {:>10?}", d(sums.total));

    // Test na roznych dlugosciach audio
    println!("\n=== Scaling ===");
    for duration_s in [0.5_f32, 1.0, 1.5] {
        let n_samples = (16000.0 * duration_s) as usize;
        if n_samples > samples.len() {
            continue;
        }
        let slice = &samples[..n_samples];
        // Warm
        let _ = model.extract(slice);
        let t0 = std::time::Instant::now();
        for _ in 0..10 {
            let _ = model.extract(slice);
        }
        let avg = t0.elapsed() / 10;
        println!("  {:.1}s audio: {:?}  (RTF {:.4}x)",
            duration_s, avg, avg.as_secs_f32() / duration_s);
    }

    // Mini-benchmark dla samych GEMM operacji w izolacji
    println!("\n=== Raw GEMM microbenchmarks ===");
    use tentaflow_voice::ops::{gemm, gemm_accumulate};

    // Aggregation-like: M=1536, K=1536, N=141
    {
        let m = 1536;
        let k = 1536;
        let n = 141;
        let a: Vec<f32> = vec![0.5; m * k];
        let b: Vec<f32> = vec![0.3; k * n];
        let mut c = vec![0.0_f32; m * n];
        for _ in 0..3 { gemm(&a, &b, &mut c, m, n, k, None); } // warm
        let t0 = std::time::Instant::now();
        const ITER: u32 = 50;
        for _ in 0..ITER { gemm(&a, &b, &mut c, m, n, k, None); }
        let avg = t0.elapsed() / ITER;
        let gflops = (2.0 * m as f64 * k as f64 * n as f64) / avg.as_secs_f64() / 1e9;
        println!("  gemm(1536, 141, 1536) = aggregation: {:?}  ({:.1} GFLOPS)", avg, gflops);
    }

    // Block-like: M=512, K=512, N=141
    {
        let m = 512;
        let k = 512;
        let n = 141;
        let a: Vec<f32> = vec![0.5; m * k];
        let b: Vec<f32> = vec![0.3; k * n];
        let mut c = vec![0.0_f32; m * n];
        for _ in 0..3 { gemm(&a, &b, &mut c, m, n, k, None); }
        let t0 = std::time::Instant::now();
        const ITER: u32 = 200;
        for _ in 0..ITER { gemm(&a, &b, &mut c, m, n, k, None); }
        let avg = t0.elapsed() / ITER;
        let gflops = (2.0 * m as f64 * k as f64 * n as f64) / avg.as_secs_f64() / 1e9;
        println!("  gemm(512, 141, 512) = pre/post Res2: {:?}  ({:.1} GFLOPS)", avg, gflops);
    }

    // Pool linear1: M=128, K=4608, N=141
    {
        let m = 128;
        let k = 4608;
        let n = 141;
        let a: Vec<f32> = vec![0.5; m * k];
        let b: Vec<f32> = vec![0.3; k * n];
        let mut c = vec![0.0_f32; m * n];
        for _ in 0..3 { gemm(&a, &b, &mut c, m, n, k, None); }
        let t0 = std::time::Instant::now();
        const ITER: u32 = 200;
        for _ in 0..ITER { gemm(&a, &b, &mut c, m, n, k, None); }
        let avg = t0.elapsed() / ITER;
        let gflops = (2.0 * m as f64 * k as f64 * n as f64) / avg.as_secs_f64() / 1e9;
        println!("  gemm(128, 141, 4608) = pool linear1: {:?}  ({:.1} GFLOPS)", avg, gflops);
    }

    // Res2-like k=3: M=64, K=64, N=141, emulowane jako 3x gemm_accumulate
    {
        let m = 64;
        let k_ic = 64;
        let n = 141;
        let w: Vec<f32> = vec![0.5; m * k_ic];
        let b: Vec<f32> = vec![0.3; k_ic * n];
        let mut c = vec![0.0_f32; m * n];
        // Warm
        for _ in 0..3 {
            c.iter_mut().for_each(|v| *v = 0.0);
            for _kp in 0..3 {
                gemm_accumulate(&w, &b, &mut c, m, n, k_ic);
            }
        }
        let t0 = std::time::Instant::now();
        const ITER: u32 = 1000;
        for _ in 0..ITER {
            c.iter_mut().for_each(|v| *v = 0.0);
            for _kp in 0..3 {
                gemm_accumulate(&w, &b, &mut c, m, n, k_ic);
            }
        }
        let avg = t0.elapsed() / ITER;
        let gflops = (2.0 * m as f64 * k_ic as f64 * n as f64 * 3.0) / avg.as_secs_f64() / 1e9;
        println!("  res2 conv (3x accumulate 64,141,64): {:?}  ({:.1} GFLOPS)", avg, gflops);
    }

    Ok(())
}

fn read_wav(path: &str) -> anyhow::Result<Vec<i16>> {
    let bytes = std::fs::read(path)?;
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        anyhow::bail!("Not a WAV");
    }
    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let cid = &bytes[pos..pos + 4];
        let csz = u32::from_le_bytes([bytes[pos+4], bytes[pos+5], bytes[pos+6], bytes[pos+7]]) as usize;
        pos += 8;
        if cid == b"data" {
            let pcm = &bytes[pos..pos + csz];
            return Ok(pcm.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect());
        }
        pos += csz;
    }
    anyhow::bail!("no data chunk")
}
