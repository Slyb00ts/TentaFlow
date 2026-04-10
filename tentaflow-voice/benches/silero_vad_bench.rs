// =============================================================================
// Plik: benches/silero_vad_bench.rs
// Opis: Benchmarki matvec SIMD vs naiwny + LSTM step per chunk.
// =============================================================================

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use tentaflow_voice::ops::{matvec_f32, matvec_f32_simd};

fn bench_matvec(c: &mut Criterion) {
    // Rozmiary podobne do Silero LSTM: input 64, hidden 128 → matvec 4*128 x 64
    let m_rows = 512; // 4 * 128
    let k_cols = 64;

    let matrix: Vec<f32> = (0..m_rows * k_cols).map(|i| (i as f32) * 0.001).collect();
    let vec: Vec<f32> = (0..k_cols).map(|i| (i as f32) * 0.01).collect();

    let mut group = c.benchmark_group("matvec");

    group.bench_function("naive", |b| {
        let mut out = vec![0.0_f32; m_rows];
        b.iter(|| {
            matvec_f32(
                black_box(&matrix),
                black_box(&vec),
                m_rows,
                k_cols,
                &mut out,
            );
        });
    });

    group.bench_function("simd_f32x8", |b| {
        let mut out = vec![0.0_f32; m_rows];
        b.iter(|| {
            matvec_f32_simd(
                black_box(&matrix),
                black_box(&vec),
                m_rows,
                k_cols,
                &mut out,
            );
        });
    });

    group.finish();
}

criterion_group!(benches, bench_matvec);
criterion_main!(benches);
