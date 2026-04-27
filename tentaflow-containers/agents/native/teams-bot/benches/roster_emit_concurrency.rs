// =============================================================================
// Plik: benches/roster_emit_concurrency.rs
// Opis: Mierzy wplyw R-1 (rozdzielenie listener/writer + concurrent emity)
//       w dom_observer. Symuluje 5 uczestnikow dolaczajacych w jednym scan'ie
//       DOM, kazdy z mockowanym RT QUIC = 100 ms. Porownuje petle sequencyjna
//       (stare zachowanie: `emit_participant(...).await` w loopie) z
//       `for_each_concurrent(8)` (nowe zachowanie: writer task z semaphore'em).
// =============================================================================

use std::time::Duration;

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use futures::stream::{self, StreamExt};
use tokio::runtime::Runtime;

const NUM_PARTICIPANTS: usize = 5;
const MOCK_RT_MS: u64 = 100;
const CONCURRENCY: usize = 8;

async fn mock_emit() {
    tokio::time::sleep(Duration::from_millis(MOCK_RT_MS)).await;
}

async fn sequential_emits(n: usize) {
    for _ in 0..n {
        mock_emit().await;
    }
}

async fn concurrent_emits(n: usize, concurrency: usize) {
    stream::iter(0..n)
        .for_each_concurrent(concurrency, |_| mock_emit())
        .await;
}

fn bench_emits(c: &mut Criterion) {
    let rt = Runtime::new().expect("tokio runtime");
    let mut g = c.benchmark_group("roster_emits");
    // Kazda iteracja blokuje sie na realnym `tokio::time::sleep` 100ms — bez
    // tego sample_size domyslnie probowalby zrobic 100 iteracji = 10s+.
    g.sample_size(10);
    g.measurement_time(Duration::from_secs(6));

    g.bench_function("sequential_5", |b| {
        b.to_async(&rt).iter(|| sequential_emits(black_box(NUM_PARTICIPANTS)));
    });

    g.bench_function("concurrent_5_cap8", |b| {
        b.to_async(&rt)
            .iter(|| concurrent_emits(black_box(NUM_PARTICIPANTS), CONCURRENCY));
    });

    g.finish();
}

criterion_group!(benches, bench_emits);
criterion_main!(benches);
