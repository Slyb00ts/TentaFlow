// =============================================================================
// Plik: prepad_ringbuffer.rs
// Opis: Mikrobenchmark porownujacy VecDeque (pop_front/push_back per sample) z
//       const-generic ring bufferem PrepadRing dla scenariusza prepad audio.
// =============================================================================

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use std::collections::VecDeque;

#[path = "../src/audio_ring.rs"]
mod audio_ring;

use audio_ring::PrepadRing;

const PREPAD: usize = 4000; // 250ms @ 16 kHz — wartosc PREPAD_SAMPLES w main.rs
const CHUNK: usize = 1024; // typowy rozmiar chunka WS audio z bota

fn make_chunk() -> Vec<i16> {
    (0..CHUNK).map(|i| (i as i16).wrapping_mul(7)).collect()
}

fn bench_prepad(c: &mut Criterion) {
    let chunk = make_chunk();
    let mut g = c.benchmark_group("prepad_ringbuffer");
    g.throughput(Throughput::Elements(CHUNK as u64));

    g.bench_function("vecdeque_push_pop", |b| {
        let mut deque: VecDeque<i16> = VecDeque::with_capacity(PREPAD);
        b.iter(|| {
            for &s in chunk.iter() {
                if deque.len() >= PREPAD {
                    deque.pop_front();
                }
                deque.push_back(s);
            }
            black_box(&deque);
        });
    });

    g.bench_function("ring_array_push", |b| {
        let mut ring: PrepadRing<PREPAD> = PrepadRing::new();
        b.iter(|| {
            for &s in chunk.iter() {
                ring.push(s);
            }
            black_box(&ring as *const _);
        });
    });

    g.bench_function("ring_array_extend_from_slice", |b| {
        let mut ring: PrepadRing<PREPAD> = PrepadRing::new();
        b.iter(|| {
            ring.extend_from_slice(&chunk);
            black_box(&ring as *const _);
        });
    });

    g.finish();
}

criterion_group!(benches, bench_prepad);
criterion_main!(benches);
