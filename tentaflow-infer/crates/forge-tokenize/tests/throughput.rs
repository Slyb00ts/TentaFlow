// ===== File: throughput.rs — tokenizer throughput on a real vocabulary =====
// Mierzy, ile tekstu na sekundę przepuszcza nasza ścieżka `encode`. Liczba jest
// potrzebna, żeby ocenić, czy tokenizacja bywa wąskim gardłem wobec prefillu.
use std::time::Instant;

fn tokenizer_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
        "../../../.runtime/models/models--TentaFlow--Bielik-PL-Minitron-7B-NVFP4/snapshots/831550e879fd7d700e3f6d79dffc14373deda3a7/tokenizer.json",
    )
}

#[test]
#[ignore = "pomiar wydajności; wymaga lokalnego tokenizer.json"]
fn encode_throughput() {
    let path = tokenizer_path();
    if !path.is_file() {
        eprintln!("pominięto: brak {}", path.display());
        return;
    }
    let tok = forge_tokenize::Tokenizer::from_file(&path).expect("wczytanie tokenizera");
    let unit = "W systemach rozproszonych algorytmy konsensusu, takie jak Raft i Paxos, \
                koordynują replikowane maszyny stanowe. Każda replika dopisuje wpisy do \
                trwałego logu, a lider przydziela monotonicznie rosnące indeksy. ";
    let text = unit.repeat(4096);
    let bytes = text.len();

    let warm = tok.encode(&text, false).expect("encode");
    let tokens = warm.len();
    let started = Instant::now();
    const REPS: usize = 5;
    for _ in 0..REPS {
        let ids = tok.encode(&text, false).expect("encode");
        assert_eq!(ids.len(), tokens);
    }
    let seconds = started.elapsed().as_secs_f64() / REPS as f64;
    eprintln!(
        "tokenizer: {:.1} MB/s, {:.2} M tok/s ({} tokenów z {} bajtów w {:.1} ms)",
        bytes as f64 / seconds / 1e6,
        tokens as f64 / seconds / 1e6,
        tokens,
        bytes,
        seconds * 1e3
    );
}
