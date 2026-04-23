// =============================================================================
// Plik: dispatch/bench.rs
// Opis: Smoke-benchmark dispatchu dla Week 2-3 checkpointu (Task #33). Mierzy:
//       - cold registry build (pierwsze wywolanie `find`)
//       - warm dispatch roundtrip (10_000 wywolan na gorcu)
//       - per-handler overhead (registry lookup vs raw match)
//       Uruchamiaj: `cargo test --release --lib dispatch::bench -- --nocapture --ignored`
//       Test nie failuje gdy wynik przekroczy prog — log tylko liczby. Prawdziwy
//       regression threshold ustawia sie po baseline measurement w CI (#35).
// =============================================================================

#[cfg(test)]
mod tests {
    use super::super::{dispatch, find, handler_count, HandlerContext};
    use std::time::Instant;
    use tentaflow_protocol::{MessageBody, SessionAuth};

    /// Cold path: pierwsze wywolanie `find` powinno zbudowac HashMap registry
    /// z inventory entries. Mierzone tylko raz (OnceLock). Pozniejsze wywolania
    /// sa O(1) hashmap lookup.
    #[test]
    #[ignore = "benchmark — run manually with --ignored --release"]
    fn bench_cold_registry_build() {
        let start = Instant::now();
        let h = find("ModelListRequest");
        let elapsed = start.elapsed();
        assert!(h.is_some());
        println!("cold registry build + first find: {:?}", elapsed);
        println!("registered handlers: {}", handler_count());
    }

    /// Warm path: dispatchuj 10 roznych wariantow 10_000 razy.
    /// Oczekiwany koszt: ~100 ns per dispatch (HashMap find + fn pointer call + match).
    #[tokio::test]
    #[ignore = "benchmark — run manually with --ignored --release"]
    async fn bench_warm_dispatch_roundtrip() {
        let _ = find("ModelListRequest"); // warm-up

        let ctx = HandlerContext {
            session: SessionAuth::UserSession {
                user_id: [0u8; 16],
                role: None,
            },
            correlation_id: 1,
            resume_secret: None,
            state: super::super::state::AppState::for_test(),
        };

        let variants = [
            MessageBody::ModelListRequest,
            MessageBody::ApiKeyListRequest,
            MessageBody::AuthMeRequest,
            MessageBody::MetaHeartbeat {
                sent_at_epoch: 1_700_000_000,
            },
        ];

        const ITERATIONS: usize = 10_000;
        let start = Instant::now();
        for i in 0..ITERATIONS {
            let body = &variants[i % variants.len()];
            let (_resp, _is_err) = dispatch(body, &ctx).await;
        }
        let elapsed = start.elapsed();
        let ns_per = elapsed.as_nanos() as f64 / ITERATIONS as f64;
        println!(
            "warm dispatch roundtrip: {} iters in {:?} = {:.1} ns/dispatch",
            ITERATIONS, elapsed, ns_per
        );
    }

    /// Tylko registry lookup (bez policy check / dispatch_fn call).
    /// Oczekiwany koszt: <50 ns.
    #[test]
    #[ignore = "benchmark — run manually with --ignored --release"]
    fn bench_registry_lookup_only() {
        let _ = find("ModelListRequest"); // warm-up

        let variants = [
            "ModelListRequest",
            "ApiKeyListRequest",
            "AuthMeRequest",
            "MetaHeartbeat",
        ];

        const ITERATIONS: usize = 100_000;
        let start = Instant::now();
        for i in 0..ITERATIONS {
            let v = variants[i % variants.len()];
            let h = find(v);
            std::hint::black_box(h);
        }
        let elapsed = start.elapsed();
        let ns_per = elapsed.as_nanos() as f64 / ITERATIONS as f64;
        println!(
            "registry lookup only: {} iters in {:?} = {:.1} ns/lookup",
            ITERATIONS, elapsed, ns_per
        );
    }

    /// Smoke test: uruchamiany zawsze (nie --ignored). Potwierdza ze >= 10
    /// handlerow jest zarejestrowanych — threshold #33 osiagniety.
    #[test]
    fn ten_handler_threshold_reached() {
        let count = handler_count();
        assert!(
            count >= 10,
            "Week 2-3 checkpoint (#33) wymaga >= 10 handlerow, mam {}",
            count
        );
    }
}
