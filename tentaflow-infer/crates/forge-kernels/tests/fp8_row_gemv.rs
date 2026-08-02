// ===== File: fp8_row_gemv.rs — GEMV na wagach FP8 ze skalą na wiersz =====
//
// Kernel istnieje dla DeepSeeka V4, którego wagi nieekspertowe są w E4M3 z
// osobną skalą na wiersz. Wariant zapisujący f32 obsługiwał dotąd tylko głowę
// logitów; ten karmi kolejne warstwy, więc zawęża wynik w kernelu zamiast
// osobnym przejściem po całym wyjściu.
//
// Test porównuje z referencją policzoną na CPU w f32. Zgodność jest przybliżona,
// bo kernel akumuluje w innej kolejności i zapisuje w f16 — próg dobrany tak, by
// mieścić te dwa efekty, a nie mieścić pomylonej skali ani zamienionych wymiarów.

use std::sync::Arc;

use forge_hal::cuda::PoolSizes;
use forge_hal::{DevBuffer, Device, Pool};
use forge_kernels::Kernels;
use forge_types::MemKind;
use half::f16;

/// Te kernele są w PTX, więc biegną wyłącznie na CUDA. Na maszynie bez niej
/// test nie ma czego sprawdzić i mówi to wprost, zamiast wywalać cały zestaw.
fn device() -> Option<Arc<dyn Device>> {
    forge_hal::gpu::open(
        0,
        PoolSizes {
            weights: 256 << 20,
            kv_cache: 16 << 20,
            activations: 64 << 20,
            kv_page_size: 256 << 10,
        },
    )
    .map_err(|e| eprintln!("pomijam {}: {e}", "GEMV fp8"))
    .ok()
}

/// Dekoduje bajt E4M3 tak samo jak kernel.
fn e4m3_to_f32(byte: u8) -> f32 {
    let sign = if byte & 0x80 != 0 { -1.0 } else { 1.0 };
    let exponent = ((byte >> 3) & 0x0F) as i32;
    let mantissa = (byte & 0x07) as f32;
    let value = if exponent == 0 {
        mantissa / 8.0 * (2f32).powi(-6)
    } else {
        (1.0 + mantissa / 8.0) * (2f32).powi(exponent - 7)
    };
    sign * value
}

fn upload(dev: &dyn Device, bytes: &[u8], pool: Pool) -> DevBuffer {
    let buf = dev.alloc(bytes.len(), MemKind::Device, pool).unwrap();
    dev.write(bytes, &buf, 0).unwrap();
    buf
}

#[test]
fn fp8_row_gemv_matches_cpu_reference() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    // Kształt jak w projekcjach DeepSeeka: kolumny wielokrotnością 256.
    let (rows, cols) = (129usize, 1024usize);

    // Wagi obejmujące cały zakres kodów E4M3 poza NaN, plus skale wierszowe
    // rozrzucone o kilka rzędów wielkości — pomylona skala nie przejdzie.
    let weight: Vec<u8> = (0..rows * cols)
        .map(|i| {
            let code = ((i * 37 + 11) % 255) as u8;
            if code == 0x7F || code == 0xFF {
                0x38
            } else {
                code
            }
        })
        .collect();
    let scales: Vec<f32> = (0..rows)
        .map(|r| (2f32).powi((r % 9) as i32 - 4) * 0.75)
        .collect();
    let x: Vec<f32> = (0..cols)
        .map(|i| (((i * 29 + 7) % 41) as f32 - 20.0) * 0.05)
        .collect();

    let w_dev = upload(dev.as_ref(), &weight, Pool::Weights);
    let s_dev = upload(dev.as_ref(), bytemuck::cast_slice(&scales), Pool::Weights);
    let x_host: Vec<f16> = x.iter().map(|v| f16::from_f32(*v)).collect();
    let x_dev = upload(
        dev.as_ref(),
        bytemuck::cast_slice(&x_host),
        Pool::Activations,
    );
    let y_dev = dev
        .alloc(rows * 2, MemKind::Device, Pool::Activations)
        .unwrap();

    kernels
        .gemv_fp8_row_f16(&y_dev, &w_dev, &s_dev, &x_dev, rows, cols, &stream)
        .unwrap();
    stream.synchronize().unwrap();

    let mut bytes = vec![0u8; rows * 2];
    dev.read(&y_dev, 0, &mut bytes).unwrap();
    let got: Vec<f32> = bytes
        .chunks_exact(2)
        .map(|c| f16::from_le_bytes([c[0], c[1]]).to_f32())
        .collect();

    let mut worst = 0f32;
    for row in 0..rows {
        let want: f32 = (0..cols)
            .map(|col| e4m3_to_f32(weight[row * cols + col]) * x_host[col].to_f32())
            .sum::<f32>()
            * scales[row];
        let rel = (got[row] - want).abs() / want.abs().max(1e-6);
        worst = worst.max(rel);
    }
    assert!(
        worst < 5e-3,
        "największy błąd względny wiersza to {worst:.3e}"
    );

    // Wynik zerowy przechodziłby powyższy próg trywialnie.
    assert!(
        got.iter().any(|v| v.abs() > 1e-3),
        "kernel zwrócił same zera"
    );
}

/// Liczba kolumn niebędąca wielokrotnością 256 musi być odrzucona, a nie
/// policzona po części.
#[test]
fn fp8_row_gemv_rejects_unsupported_shape() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let w = dev.alloc(300, MemKind::Device, Pool::Weights).unwrap();
    let s = dev.alloc(4, MemKind::Device, Pool::Weights).unwrap();
    let x = dev.alloc(600, MemKind::Device, Pool::Activations).unwrap();
    let y = dev.alloc(2, MemKind::Device, Pool::Activations).unwrap();
    assert!(kernels
        .gemv_fp8_row_f16(&y, &w, &s, &x, 1, 300, &stream)
        .is_err());
}
