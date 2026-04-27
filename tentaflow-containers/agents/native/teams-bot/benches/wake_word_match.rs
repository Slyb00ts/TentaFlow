// =============================================================================
// Plik: benches/wake_word_match.rs
// Opis: Benchmark porownujacy naive matches_wake_word (split+trim+lowercase
//       per call) z wersja precompiled (wake_words znormalizowane raz przy
//       starcie konfiguracji). Mierzy hot-path STT meeting bota.
// =============================================================================

use criterion::{black_box, criterion_group, criterion_main, Criterion};

const TEXT: &str = "Hej bot, podaj mi prosze status projektu i powiedz co dzisiaj wydarzylo sie na spotkaniu";
const WAKE_CSV: &str = "Hej bot, asystent, kolega, towarzysz, hej towarzysz, towarzyszu";

/// Wariant naive — to co bylo przed P-optymalizacja: kazde wywolanie
/// alokuje Vec<String> z splitu CSV i String z `text.to_lowercase`.
fn match_naive(text: &str, csv: &str) -> bool {
    let words: Vec<String> = csv
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    if words.is_empty() {
        return true;
    }
    let lower = text.to_lowercase();
    words.iter().any(|w| lower.contains(w.as_str()))
}

/// Wariant precompiled — wake_words sa znormalizowane raz przy starcie
/// (`MeetingConfig::validate`). Per call zostaje tylko `text.to_lowercase`.
fn match_precompiled(text: &str, words: &[String]) -> bool {
    if words.is_empty() {
        return true;
    }
    let lower = text.to_lowercase();
    words.iter().any(|w| lower.contains(w.as_str()))
}

fn bench_wake_word(c: &mut Criterion) {
    let precompiled: Vec<String> = WAKE_CSV
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();

    let mut g = c.benchmark_group("wake_word_match");

    g.bench_function("naive_per_call", |b| {
        b.iter(|| black_box(match_naive(black_box(TEXT), black_box(WAKE_CSV))))
    });

    g.bench_function("precompiled", |b| {
        b.iter(|| black_box(match_precompiled(black_box(TEXT), black_box(&precompiled))))
    });

    g.finish();
}

criterion_group!(benches, bench_wake_word);
criterion_main!(benches);
