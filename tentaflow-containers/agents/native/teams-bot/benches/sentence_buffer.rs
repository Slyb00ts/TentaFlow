// =============================================================================
// Plik: benches/sentence_buffer.rs
// Opis: Benchmark sentence-boundary parsera uzywanego w streaming pipeline
//       LLM->TTS. Mierzy hot path: kazdy delta-token z LLM pcha bufor i
//       trigeruje skan granic zdania.
// =============================================================================

use criterion::{black_box, criterion_group, criterion_main, Criterion};

// Skopiowana logika z src/sentence_buffer.rs — bot to bin, wiec bench
// nie moze zaimportowac modulu jako biblioteki. Wszystkie zmiany w
// algorytmie nalezy odbic w obu miejscach (test pokrywa src; bench
// kopia jest tylko do pomiaru wydajnosci).
const MIN_SENTENCE_BYTES: usize = 4;

struct SentenceBuffer {
    buf: String,
}

impl SentenceBuffer {
    fn new() -> Self {
        Self {
            buf: String::with_capacity(512),
        }
    }

    fn push(&mut self, delta: &str) -> Vec<String> {
        self.buf.push_str(delta);
        let mut sentences = Vec::new();
        let mut scan_from: usize = 0;
        loop {
            let bytes = self.buf.as_bytes();
            let Some(rel_idx) = find_sentence_boundary(&bytes[scan_from..]) else {
                break;
            };
            let idx = scan_from + rel_idx;
            let sentence = self.buf[..idx].trim().to_string();
            if sentence.len() >= MIN_SENTENCE_BYTES {
                let rest = self.buf[idx..].trim_start().to_string();
                self.buf = rest;
                sentences.push(sentence);
                scan_from = 0;
            } else {
                scan_from = idx;
            }
        }
        sentences
    }
}

fn find_sentence_boundary(bytes: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'\n' | b'!' | b'?' => return Some(i + 1),
            b'.' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'.' {
                    while i < bytes.len() && bytes[i] == b'.' {
                        i += 1;
                    }
                    continue;
                }
                if i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
                    i += 1;
                    continue;
                }
                return Some(i + 1);
            }
            _ => {}
        }
        i += 1;
    }
    None
}

const TYPICAL_TOKENS: &[&str] = &[
    "Witaj",
    "!",
    " Jak",
    " moge",
    " ci",
    " pomoc",
    "?",
    " Mam",
    " dzis",
    " duzo",
    " czasu",
    " na",
    " rozmowe",
    ".",
    " Czy",
    " chcesz",
    " omowic",
    " plan",
    " spotkania",
    "?",
];

fn bench_sentence_buffer(c: &mut Criterion) {
    let mut g = c.benchmark_group("sentence_buffer");

    g.bench_function("typical_stream_20_tokens", |b| {
        b.iter(|| {
            let mut buf = SentenceBuffer::new();
            let mut total = 0usize;
            for t in TYPICAL_TOKENS {
                total += buf.push(black_box(t)).len();
            }
            black_box(total)
        })
    });

    g.bench_function("single_long_sentence", |b| {
        let long = "To jest dluzsze zdanie z wieloma wyrazami pokazujace ze parser dziala szybko nawet dla 100 znakow w jednym pushu.";
        b.iter(|| {
            let mut buf = SentenceBuffer::new();
            black_box(buf.push(black_box(long)))
        })
    });

    g.bench_function("many_decimals", |b| {
        let txt = "Wartosc 3.14, druga 2.71, trzecia 1.41, czwarta 9.81. Koniec.";
        b.iter(|| {
            let mut buf = SentenceBuffer::new();
            black_box(buf.push(black_box(txt)))
        })
    });

    g.finish();
}

criterion_group!(benches, bench_sentence_buffer);
criterion_main!(benches);
