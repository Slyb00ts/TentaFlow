// ===== File: cuda_w4a8.rs — in-tree W4A8 GEMM vs CPU int4xint8 golden =====
// Proves the committed W4A8 cubin (kernels/cuda/w4a8_gemm.cu, QServe dense_kernel0)
// runs correctly THROUGH FORGE's HAL with the Rust-side QServe packer
// (forge_formats::w4a8). Independent CPU golden: C[m][n] = Sum_k (ascale[m]*a_i8)
// * recon[n][k], where recon = s1*int8_wrap(s2*(q4-zero)) is the exact weight the
// kernel applies. This de-risks the weight interleave + per-token int8 activation
// quant + launcher on the real GPU before any engine routing. Skips without a
// device. Matches the standalone scratch/w4a8/harness.cu (relL2 ~2e-4 = fp16).

use std::sync::Arc;

use forge_formats::w4a8::{w4a8_pack, w4a8_reconstruct, W4A8_GROUP};
use forge_hal::cuda::{CudaDevice, PoolSizes};
use forge_hal::{DevBuffer, Device, Pool};
use forge_kernels::Kernels;
use forge_types::{MemKind, Result};
use half::f16;

fn device() -> Option<Arc<CudaDevice>> {
    // `catch_unwind`, nie sam wariant `Err`: bez biblioteki sterownika CUDA —
    // czyli na kazdym Macu — cudarc panikuje przy jej leniwym ladowaniu, wiec
    // ponizsze pomijanie nigdy nie dochodzilo do skutku.
    let created = std::panic::catch_unwind(|| {
        CudaDevice::new(
            0,
            PoolSizes {
                weights: 2048 << 20,
                kv_cache: 16 << 20,
                activations: 1024 << 20,
                kv_page_size: 256 << 10,
            },
        )
    });
    match created {
        Ok(Ok(d)) => Some(d),
        Ok(Err(e)) => {
            eprintln!("skipping CUDA w4a8 tests: {e}");
            None
        }
        Err(_) => {
            eprintln!("skipping CUDA w4a8 tests: brak sterownika CUDA");
            None
        }
    }
}

fn upload(dev: &dyn Device, bytes: &[u8]) -> DevBuffer {
    let buf = dev
        .alloc(bytes.len().max(1), MemKind::Device, Pool::Weights)
        .unwrap();
    dev.write(bytes, &buf, 0).unwrap();
    buf
}

fn upload_f16_bits(dev: &dyn Device, bits: &[u16]) -> DevBuffer {
    let bytes: Vec<u8> = bits.iter().flat_map(|v| v.to_le_bytes()).collect();
    upload(dev, &bytes)
}

fn download_f16(dev: &dyn Device, buf: &DevBuffer, n: usize) -> Vec<f32> {
    let mut bytes = vec![0u8; n * 2];
    dev.read(buf, 0, &mut bytes).unwrap();
    bytes
        .chunks_exact(2)
        .map(|c| f16::from_le_bytes([c[0], c[1]]).to_f32())
        .collect()
}

// deterministic pseudo-random fp32 in [-scale, scale]
fn rnd(seed: usize, scale: f32) -> f32 {
    let x = (seed.wrapping_mul(2654435761) >> 8) & 0xFFFF;
    (x as f32 / 32768.0 - 1.0) * scale
}

// per-token int8 activation quant: a_i8[m][k], ascale[m] fp16.
fn quant_act(a: &[f32], m: usize, k: usize) -> (Vec<i8>, Vec<u16>, Vec<f32>) {
    let mut ai8 = vec![0i8; m * k];
    let mut asc_bits = vec![0u16; m];
    let mut asc = vec![0f32; m];
    for row in 0..m {
        let mut amax = 0f32;
        for &v in &a[row * k..row * k + k] {
            amax = amax.max(v.abs());
        }
        let s = f16::from_f32(if amax > 0.0 { amax / 127.0 } else { 1.0 }).to_f32();
        asc_bits[row] = f16::from_f32(s).to_bits();
        asc[row] = s;
        for kk in 0..k {
            ai8[row * k + kk] = (a[row * k + kk] / s).round().clamp(-127.0, 127.0) as i8;
        }
    }
    (ai8, asc_bits, asc)
}

fn run_case(m: usize, n: usize, k: usize) -> Result<()> {
    let Some(dev) = device() else { return Ok(()) };
    let kernels = Kernels::load(dev.clone())?;
    let stream = dev.create_stream()?;

    let w: Vec<f32> = (0..n * k).map(|i| rnd(i * 7 + 1, 0.3)).collect();
    let a: Vec<f32> = (0..m * k).map(|i| rnd(i * 13 + 5, 0.5)).collect();

    let packed = w4a8_pack(&w, n, k, W4A8_GROUP);
    let recon = w4a8_reconstruct(&w, n, k, W4A8_GROUP);
    let (ai8, asc_bits, asc) = quant_act(&a, m, k);

    // independent CPU golden
    let mut gold = vec![0f32; m * n];
    for row in 0..n {
        for mm in 0..m {
            let mut acc = 0f32;
            for kk in 0..k {
                acc += (asc[mm] * ai8[mm * k + kk] as f32) * recon[row * k + kk];
            }
            gold[mm * n + row] = acc;
        }
    }

    let a_bytes: Vec<u8> = ai8.iter().map(|&v| v as u8).collect();
    let d_a = upload(dev.as_ref(), &a_bytes);
    let d_w = upload(dev.as_ref(), &packed.qweight);
    let d_z = upload(dev.as_ref(), &packed.s2_zeros);
    let d_s2 = upload(dev.as_ref(), &packed.s2_scales);
    let d_ws = upload_f16_bits(dev.as_ref(), &packed.s1_scales);
    let d_as = upload_f16_bits(dev.as_ref(), &asc_bits);
    let d_y = upload_f16_bits(dev.as_ref(), &vec![0u16; m * n]);

    kernels.w4a8_gemm(
        &d_y, &d_a, &d_w, &d_z, &d_s2, &d_ws, &d_as, m, n, k, &stream,
    )?;
    dev.synchronize()?;

    let got = download_f16(dev.as_ref(), &d_y, m * n);
    let mut se = 0f64;
    let mut sref = 0f64;
    let mut maxabs = 0f64;
    for i in 0..m * n {
        let d = (gold[i] - got[i]) as f64;
        se += d * d;
        sref += (gold[i] as f64) * (gold[i] as f64);
        maxabs = maxabs.max(d.abs());
    }
    let rel = (se / sref.max(1e-30)).sqrt();
    println!("w4a8 M={m} N={n} K={k}: relL2={rel:.2e} maxabs={maxabs:.3e}");
    assert!(rel < 2e-2, "w4a8 {m}x{n}x{k}: relL2 {rel:.3e} > 2e-2");
    Ok(())
}

#[test]
fn w4a8_matches_cpu_golden() {
    // All CTA-config branches + Mistral FFN shapes (N%64==0, K%128==0).
    let shapes = [
        (256, 128, 256), // m128 branch
        (256, 256, 512),
        (129, 256, 512),   // m128 branch (M just over 128)
        (128, 256, 256),   // m64_ksm (M==128, K<=4096)
        (128, 256, 8192),  // m64_klg (M==128, K>4096)
        (64, 256, 512),    // m32 branch (M<128)
        (256, 4096, 4096), // Mistral q/o
        (512, 4096, 4096),
        (192, 14336, 4096), // gate/up
        (192, 4096, 14336), // down
    ];
    for (m, n, k) in shapes {
        run_case(m, n, k).unwrap();
    }
}

fn bench_tops(dev: &Arc<CudaDevice>, kernels: &Kernels, m: usize, n: usize, k: usize) -> f64 {
    let stream = dev.create_stream().unwrap();
    let w: Vec<f32> = (0..n * k).map(|i| rnd(i * 7 + 1, 0.3)).collect();
    let a: Vec<f32> = (0..m * k).map(|i| rnd(i * 13 + 5, 0.5)).collect();
    let packed = w4a8_pack(&w, n, k, W4A8_GROUP);
    let (ai8, asc_bits, _) = quant_act(&a, m, k);
    let a_bytes: Vec<u8> = ai8.iter().map(|&v| v as u8).collect();
    let d_a = upload(dev.as_ref(), &a_bytes);
    let d_w = upload(dev.as_ref(), &packed.qweight);
    let d_z = upload(dev.as_ref(), &packed.s2_zeros);
    let d_s2 = upload(dev.as_ref(), &packed.s2_scales);
    let d_ws = upload_f16_bits(dev.as_ref(), &packed.s1_scales);
    let d_as = upload_f16_bits(dev.as_ref(), &asc_bits);
    let d_y = upload_f16_bits(dev.as_ref(), &vec![0u16; m * n]);
    let go = || {
        kernels
            .w4a8_gemm(
                &d_y, &d_a, &d_w, &d_z, &d_s2, &d_ws, &d_as, m, n, k, &stream,
            )
            .unwrap();
    };
    for _ in 0..30 {
        go();
    }
    dev.synchronize().unwrap();
    // sustained warmup to reach boost clock
    for _ in 0..200 {
        go();
    }
    dev.synchronize().unwrap();
    let reps = 60;
    let t0 = std::time::Instant::now();
    for _ in 0..reps {
        go();
    }
    dev.synchronize().unwrap();
    let sec = t0.elapsed().as_secs_f64() / reps as f64;
    2.0 * n as f64 * k as f64 * m as f64 / sec / 1e12
}

#[test]
#[ignore]
fn bench_w4a8_tops() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    for &(name, n, k) in &[
        ("gate", 14336usize, 4096usize),
        ("down", 4096usize, 14336usize),
    ] {
        for &t in &[512usize, 2048, 4096] {
            let tops = bench_tops(&dev, &kernels, t, n, k);
            println!("w4a8 {name} N={n} K={k} T={t}: {tops:.1} TFLOP-eq");
        }
    }
}
