// =============================================================================
// Plik: benches/audio_pipeline_clones.rs
// Opis: Mierzy koszt przekazania 15s audio (i16 mono 16 kHz = 480 KB Vec<u8>)
//       przez pipeline STT. Porownuje stara sciezke (5 .clone() na Vec<u8>)
//       z nowa (5 Arc::clone na Arc<[u8]>).
// =============================================================================

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use std::sync::Arc;

const SECS: usize = 15;
const SAMPLE_RATE: usize = 16_000;
const PCM_BYTES: usize = SECS * SAMPLE_RATE * 2; // i16 mono LE

fn make_audio() -> Vec<u8> {
    vec![0u8; PCM_BYTES]
}

fn bench_pipeline_clones(c: &mut Criterion) {
    let mut g = c.benchmark_group("stt_pipeline_clone");
    g.throughput(Throughput::Bytes(PCM_BYTES as u64));

    g.bench_function("vec_5_clones", |b| {
        let audio = make_audio();
        b.iter(|| {
            let a1 = audio.clone();
            let a2 = a1.clone();
            let a3 = a2.clone();
            let a4 = a3.clone();
            let a5 = a4.clone();
            black_box(a5)
        });
    });

    g.bench_function("arc_5_clones", |b| {
        let audio: Arc<[u8]> = Arc::from(make_audio().into_boxed_slice());
        b.iter(|| {
            let a1 = Arc::clone(&audio);
            let a2 = Arc::clone(&a1);
            let a3 = Arc::clone(&a2);
            let a4 = Arc::clone(&a3);
            let a5 = Arc::clone(&a4);
            black_box(a5)
        });
    });

    g.finish();
}

criterion_group!(benches, bench_pipeline_clones);
criterion_main!(benches);
