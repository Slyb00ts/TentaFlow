// =============================================================================
// Plik: vad_chunk_processing.rs
// Opis: Benchmark konwersji i16 -> f32 dla VAD: alokacja per chunk vs reusable
//       bufor + mnozenie przez odwrotnosc 1/32768 vs dzielenie.
// =============================================================================

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};

const CHUNK_SAMPLES: usize = 4096; // ~256 ms @ 16 kHz mono

fn make_samples() -> Vec<i16> {
    (0..CHUNK_SAMPLES)
        .map(|i| ((i as f32 * 0.1).sin() * 16000.0) as i16)
        .collect()
}

fn bench_alloc_per_chunk(c: &mut Criterion) {
    let samples = make_samples();
    let mut g = c.benchmark_group("vad_i16_to_f32");
    g.throughput(Throughput::Elements(CHUNK_SAMPLES as u64));

    g.bench_function("alloc_collect_div", |b| {
        b.iter(|| {
            let f32: Vec<f32> = samples.iter().map(|&s| s as f32 / 32768.0).collect();
            black_box(f32)
        });
    });

    g.bench_function("alloc_collect_mul", |b| {
        b.iter(|| {
            let f32: Vec<f32> = samples
                .iter()
                .map(|&s| s as f32 * (1.0 / 32768.0))
                .collect();
            black_box(f32)
        });
    });

    g.bench_function("reuse_buf_extend_div", |b| {
        let mut buf: Vec<f32> = Vec::with_capacity(CHUNK_SAMPLES);
        b.iter(|| {
            buf.clear();
            buf.extend(samples.iter().map(|&s| s as f32 / 32768.0));
            black_box(&buf);
        });
    });

    g.bench_function("reuse_buf_mul_const", |b| {
        let mut buf: Vec<f32> = Vec::with_capacity(CHUNK_SAMPLES);
        b.iter(|| {
            buf.clear();
            buf.reserve(samples.len());
            for &s in &samples {
                buf.push(s as f32 * (1.0 / 32768.0));
            }
            black_box(&buf);
        });
    });

    g.finish();
}

criterion_group!(benches, bench_alloc_per_chunk);
criterion_main!(benches);
