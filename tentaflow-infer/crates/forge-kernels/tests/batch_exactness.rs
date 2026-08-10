// ===== File: batch_exactness.rs — czy kernel wsadowy liczy to samo co seryjny =====
// Batch hybrydy wolno włączyć bez zastrzeżeń tylko dla formatu, którego kernel
// wsadowy nie rozjeżdża się z dekodowaniem po jednej sekwencji. Ten plik trzyma
// odpowiedź na to pytanie w postaci wykonywalnej, zamiast w komentarzu.

use std::sync::Arc;

use forge_hal::cuda::PoolSizes;
use forge_hal::{DevBuffer, Device, Pool};
use forge_kernels::Kernels;
use forge_types::MemKind;
use half::f16;

mod common;
use common::build_q4k;

fn device() -> Option<Arc<dyn Device>> {
    match forge_hal::gpu::open(
        0,
        PoolSizes {
            weights: 256 << 20,
            kv_cache: 64 << 20,
            activations: 64 << 20,
            kv_page_size: 256 << 10,
        },
    ) {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("brak urządzenia GPU: {e}");
            None
        }
    }
}

fn fill(i: usize) -> f32 {
    ((i % 17) as f32) - 8.0
}

fn upload_f16(dev: &dyn Device, values: &[f32]) -> DevBuffer {
    let bytes: Vec<u8> = values
        .iter()
        .flat_map(|v| f16::from_f32(*v).to_le_bytes())
        .collect();
    let buf = dev
        .alloc(bytes.len().max(2), MemKind::Device, Pool::Activations)
        .unwrap();
    dev.write(&bytes, &buf, 0).unwrap();
    buf
}

fn download_f16(dev: &dyn Device, buf: &DevBuffer, count: usize) -> Vec<f32> {
    let mut bytes = vec![0u8; count * 2];
    dev.read(buf, 0, &mut bytes).unwrap();
    bytes
        .chunks_exact(2)
        .map(|c| f16::from_le_bytes([c[0], c[1]]).to_f32())
        .collect()
}

/// Batchowy dp4a Q4_K NIE jest bitowo równy seryjnemu GEMV — i to jest powód,
/// dla którego batch dekodowania hybrydy ma własny, luźniejszy kontrakt niż
/// prefill B2 i MTP.
///
/// Źródło rozjazdu: seryjny kernel kwantyzuje aktywację do q8_1 W SOBIE, a
/// wsadowy czyta ją z bufora prekwantyzowanego wspólnego z prefillem. Test
/// pilnuje, żeby ta różnica została RÓŻNICĄ ZAOKRĄGLENIA: gdyby kernel zaczął
/// liczyć co innego, odchylenie wystrzeliłoby daleko poza próg.
#[test]
fn gemv_q4_k_dp4a_batch_rozni_sie_od_seryjnego_tylko_zaokragleniem() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let (rows, cols) = (64usize, 512usize);
    let wq = build_q4k(rows, cols);
    let wb = dev.alloc(wq.len(), MemKind::Device, Pool::Weights).unwrap();
    dev.write(&wq, &wb, 0).unwrap();

    for n_tokens in [2usize, 4, 8, 16] {
        let x: Vec<f32> = (0..n_tokens * cols)
            .map(|i| f16::from_f32(fill(i) * 0.1).to_f32())
            .collect();
        let xb = upload_f16(dev.as_ref(), &x);
        let batched = upload_f16(dev.as_ref(), &vec![0.0; n_tokens * rows]);
        if !kernels
            .gemm_qk_dp4a_batch_at(&batched, &wb, 0, &xb, rows, cols, n_tokens, false, &stream)
            .unwrap()
        {
            eprintln!("pomijam T={n_tokens}: brak kernela wsadowego");
            continue;
        }
        let serial = upload_f16(dev.as_ref(), &vec![0.0; n_tokens * rows]);
        for t in 0..n_tokens {
            let row = upload_f16(dev.as_ref(), &x[t * cols..(t + 1) * cols]);
            let out = upload_f16(dev.as_ref(), &vec![0.0; rows]);
            kernels
                .gemv_q4_k_dp4a_f16_at(&out, &wb, 0, &row, rows, cols, &stream)
                .unwrap();
            dev.synchronize().unwrap();
            let got = download_f16(dev.as_ref(), &out, rows);
            let bytes: Vec<u8> = got
                .iter()
                .flat_map(|v| f16::from_f32(*v).to_le_bytes())
                .collect();
            dev.write(&bytes, &serial, t * rows * 2).unwrap();
        }
        dev.synchronize().unwrap();
        let a = download_f16(dev.as_ref(), &batched, n_tokens * rows);
        let b = download_f16(dev.as_ref(), &serial, n_tokens * rows);
        let mismatch = a
            .iter()
            .zip(b.iter())
            .filter(|(x, y)| x.to_bits() != y.to_bits())
            .count();
        let (mut num, mut den) = (0.0f64, 0.0f64);
        for (x, y) in a.iter().zip(b.iter()) {
            num += ((x - y) as f64) * ((x - y) as f64);
            den += (*y as f64) * (*y as f64);
        }
        let rel_l2 = (num / den.max(f64::MIN_POSITIVE)).sqrt();
        eprintln!(
            "T={n_tokens}: {mismatch} z {} elementów różni się, względne L2 {rel_l2:.3e}",
            a.len()
        );
        assert!(
            rel_l2 < 2e-2,
            "T={n_tokens}: względne L2 {rel_l2:.3e} wobec seryjnego przekracza próg zaokrąglenia"
        );
    }
}
