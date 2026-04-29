// =============================================================================
// Plik: benches/noop.rs
// Opis: Placeholder benchmark — sprawdza ze infrastruktura Criterion dziala.
//       Realne benchmarki dorzucane przez kolejne PR-y optymalizacyjne.
// =============================================================================

use criterion::{criterion_group, criterion_main, Criterion};

fn bench_noop(c: &mut Criterion) {
    c.bench_function("noop", |b| {
        b.iter(|| std::hint::black_box(1u64.wrapping_add(2)));
    });
}

criterion_group!(benches, bench_noop);
criterion_main!(benches);
