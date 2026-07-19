// ===== File: cuda_i8mma.rs — CUDA MMQ GEMM vs Mojo i8mma + CPU MMQ reference =====
// The nvcc-compiled Q4_K/Q8_0 prefill GEMM (kernels/cuda/gemm_i8mma.cu, the
// ADR-0001 exception) is validated two ways on byte-identical inputs:
//   1. vs an INDEPENDENT CPU reference — forge_formats weight dequant times the
//      q8_1-quantized activation (the exact MMQ math) — checks absolute
//      correctness incl. weight indexing.
//   2. vs the in-tree Mojo `gemm_i8mma_impl` — rel err <= 5e-4 (integer mma is
//      exact, only f32 accumulation order differs).
// `FORGE_I8MMA_BACKEND` selects the backend in the launcher; a process-global
// mutex serialises the env access across test threads. Skips without a device.

use std::sync::{Arc, Mutex};

use forge_hal::cuda::{CudaDevice, PoolSizes};
use forge_hal::{DevBuffer, Device, Pool};
use forge_kernels::Kernels;
use forge_types::{DType, MemKind, QuantKind};
use half::f16;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn device() -> Option<Arc<CudaDevice>> {
    match CudaDevice::new(
        0,
        PoolSizes {
            weights: 4096 << 20,
            kv_cache: 64 << 20,
            activations: 2048 << 20,
            kv_page_size: 256 << 10,
        },
    ) {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("skipping CUDA i8mma tests: {e}");
            None
        }
    }
}

fn upload_f16(dev: &dyn Device, vals: &[f32]) -> DevBuffer {
    let host: Vec<f16> = vals.iter().map(|&v| f16::from_f32(v)).collect();
    let bytes =
        unsafe { std::slice::from_raw_parts(host.as_ptr() as *const u8, host.len() * 2) };
    let buf = dev.alloc(bytes.len(), MemKind::Device, Pool::Weights).unwrap();
    dev.write(bytes, &buf, 0).unwrap();
    buf
}

fn download_f16(dev: &dyn Device, buf: &DevBuffer, n: usize) -> Vec<f32> {
    let mut bytes = vec![0u8; n * 2];
    dev.read(buf, 0, &mut bytes).unwrap();
    bytes
        .chunks_exact(2)
        .map(|c| f16::from_le_bytes([c[0], c[1]]).to_f32())
        .collect()
}

fn build_q4k(rows: usize, cols: usize) -> Vec<u8> {
    let blocks_per_row = cols / 256;
    let mut wq = Vec::with_capacity(rows * blocks_per_row * 144);
    for r in 0..rows {
        for b in 0..blocks_per_row {
            let d = f16::from_f32(0.008 + ((r + b) % 7) as f32 * 0.004);
            let dmin = f16::from_f32(0.005 + ((r + 2 * b) % 5) as f32 * 0.003);
            wq.extend_from_slice(&d.to_le_bytes());
            wq.extend_from_slice(&dmin.to_le_bytes());
            for i in 0..12 {
                wq.push(((r * 53 + b * 19 + i * 41 + 7) % 256) as u8);
            }
            for i in 0..128 {
                wq.push(((r * 31 + b * 17 + i * 13) % 256) as u8);
            }
        }
    }
    wq
}

fn build_q8_0(rows: usize, cols: usize) -> Vec<u8> {
    let blocks_per_row = cols / 32;
    let mut wq = Vec::with_capacity(rows * blocks_per_row * 34);
    for r in 0..rows {
        for b in 0..blocks_per_row {
            let d = f16::from_f32(0.01 + ((r + b) % 9) as f32 * 0.002);
            wq.extend_from_slice(&d.to_le_bytes());
            for i in 0..32 {
                wq.push((((r * 29 + b * 13 + i * 7 + 3) % 256) as i32 - 128) as u8);
            }
        }
    }
    wq
}

fn xact(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| f16::from_f32((((i * 37 % 61) as f32) - 30.0) * 0.02).to_f32())
        .collect()
}

/// q8_1 activation quant matching `quantize_act_q8_1`: returns the dequantized
/// activation (d * code) per element, which is exactly what the MMQ dot sees.
fn q8_1_dequantized(x: &[f32], n_tokens: usize, cols: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; n_tokens * cols];
    for t in 0..n_tokens {
        for b in 0..cols / 32 {
            let base = t * cols + b * 32;
            let amax = (0..32).map(|i| x[base + i].abs()).fold(0.0f32, f32::max);
            if amax == 0.0 {
                continue;
            }
            let d = amax / 127.0;
            let inv = 127.0 / amax;
            for i in 0..32 {
                let code = (x[base + i] * inv).round().clamp(-128.0, 127.0) as i32;
                out[base + i] = d * code as f32;
            }
        }
    }
    out
}

fn run_backend(
    dev: &Arc<CudaDevice>,
    backend: &str,
    quant: &str,
    wb: &DevBuffer,
    xb: &DevBuffer,
    rows: usize,
    cols: usize,
    n_tokens: usize,
) -> Vec<f32> {
    std::env::set_var("FORGE_I8MMA_BACKEND", backend);
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let yb = upload_f16(dev.as_ref(), &vec![0.0; n_tokens * rows]);
    match quant {
        "q4k" => kernels
            .gemm_q4_k_i8mma_at(&yb, wb, 0, xb, rows, cols, n_tokens, &stream)
            .unwrap(),
        "q8_0" => kernels
            .gemm_q8_0_i8mma_at(&yb, wb, 0, xb, rows, cols, n_tokens, &stream)
            .unwrap(),
        _ => unreachable!(),
    }
    dev.synchronize().unwrap();
    std::env::remove_var("FORGE_I8MMA_BACKEND");
    download_f16(dev.as_ref(), &yb, n_tokens * rows)
}

fn compare(quant: &str, kind: QuantKind, wq: Vec<u8>, rows: usize, cols: usize, n_tokens: usize) {
    let Some(dev) = device() else { return };
    let _guard = ENV_LOCK.lock().unwrap();

    let w_f32 = forge_formats::dequant::dequantize_to_f32(DType::F32, kind, &wq, rows * cols)
        .unwrap();
    let x = xact(n_tokens * cols);
    let x_deq = q8_1_dequantized(&x, n_tokens, cols);

    let wb = dev.alloc(wq.len(), MemKind::Device, Pool::Weights).unwrap();
    dev.write(&wq, &wb, 0).unwrap();
    let xb = upload_f16(dev.as_ref(), &x);

    let cuda = run_backend(&dev, "cuda", quant, &wb, &xb, rows, cols, n_tokens);
    let mojo = run_backend(&dev, "mojo", quant, &wb, &xb, rows, cols, n_tokens);

    // (1) CUDA vs independent CPU MMQ reference (forge_formats weight dequant).
    let mut ref_rel = 0.0f32;
    for t in 0..n_tokens {
        for r in 0..rows {
            let want: f32 = (0..cols).map(|c| w_f32[r * cols + c] * x_deq[t * cols + c]).sum();
            let got = cuda[t * rows + r];
            ref_rel = ref_rel.max((got - want).abs() / (want.abs() + 1.0));
        }
    }

    // (2) CUDA vs Mojo i8mma (both consume identical inputs).
    let mut ab_rel = 0.0f32;
    for (m, c) in mojo.iter().zip(&cuda) {
        ab_rel = ab_rel.max((m - c).abs() / (m.abs() + 1.0));
    }

    println!(
        "{quant} {rows}x{cols} T={n_tokens}: vs_cpu_ref={ref_rel:.2e}  vs_mojo={ab_rel:.2e}"
    );
    assert!(ref_rel <= 3e-3, "{quant}: CUDA vs CPU MMQ ref {ref_rel:.3e} > 3e-3");
    assert!(ab_rel <= 5e-4, "{quant}: CUDA vs Mojo {ab_rel:.3e} > 5e-4");
}

#[test]
fn cuda_q4k_matches_reference() {
    // Wide (BN=128) and small-row (BN=64) tiles, token/row/col tails.
    compare("q4k", QuantKind::Q4K, build_q4k(256, 512), 256, 512, 200);
    compare("q4k", QuantKind::Q4K, build_q4k(96, 1024), 96, 1024, 130);
    compare("q4k", QuantKind::Q4K, build_q4k(517, 768), 517, 768, 300);
}

#[test]
fn cuda_q8_0_matches_reference() {
    compare("q8_0", QuantKind::Q8_0, build_q8_0(256, 512), 256, 512, 200);
    compare("q8_0", QuantKind::Q8_0, build_q8_0(96, 1024), 96, 1024, 130);
    compare("q8_0", QuantKind::Q8_0, build_q8_0(517, 256), 517, 256, 300);
}

fn tops_of(dev: &Arc<CudaDevice>, backend: &str, quant: &str, rows: usize, cols: usize, t: usize) -> f64 {
    std::env::set_var("FORGE_I8MMA_BACKEND", backend);
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let wq = if quant == "q4k" { build_q4k(rows, cols) } else { build_q8_0(rows, cols) };
    let wb = dev.alloc(wq.len(), MemKind::Device, Pool::Weights).unwrap();
    dev.write(&wq, &wb, 0).unwrap();
    let xb = upload_f16(dev.as_ref(), &xact(t * cols));
    let yb = upload_f16(dev.as_ref(), &vec![0.0; t * rows]);
    let launch = |k: &Kernels| {
        if quant == "q4k" {
            k.gemm_q4_k_i8mma_at(&yb, &wb, 0, &xb, rows, cols, t, &stream).unwrap();
        } else {
            k.gemm_q8_0_i8mma_at(&yb, &wb, 0, &xb, rows, cols, t, &stream).unwrap();
        }
    };
    for _ in 0..5 { launch(&kernels); }
    dev.synchronize().unwrap();
    let reps = 50;
    let t0 = std::time::Instant::now();
    for _ in 0..reps { launch(&kernels); }
    dev.synchronize().unwrap();
    let sec = t0.elapsed().as_secs_f64() / reps as f64;
    std::env::remove_var("FORGE_I8MMA_BACKEND");
    2.0 * rows as f64 * cols as f64 * t as f64 / sec / 1e12
}

#[test]
#[ignore]
fn bench_tops() {
    let Some(dev) = device() else { return };
    let _g = ENV_LOCK.lock().unwrap();
    // Mistral FFN shapes: down-proj (N=4096,K=14336) + gate/up (N=14336,K=4096).
    let shapes = [
        ("down", 4096usize, 14336usize),
        ("gate", 14336usize, 4096usize),
    ];
    for q in ["q4k", "q8_0"] {
        for &(name, rows, cols) in &shapes {
            for t in [512usize, 2048] {
                let mojo = tops_of(&dev, "mojo", q, rows, cols, t);
                let cuda = tops_of(&dev, "cuda", q, rows, cols, t);
                println!(
                    "{q} {name} N={rows} K={cols} T={t}: mojo={mojo:.1} cuda={cuda:.1} ({:.2}x)",
                    cuda / mojo
                );
            }
        }
    }
}
