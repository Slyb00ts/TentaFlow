// =============================================================================
// Plik: benches/intent_classifier.rs
// Opis: Benchmark lokalnego klasyfikatora intencji ktory zastapil LLM call
//       w trybie `wake_word_intent` (RT-4 z audytu). Mierzy hot-path:
//       5 reprezentatywnych wypowiedzi z meeting bota. Bench uzywa inline
//       kopii modulu — teams-bot to binarka, nie crate library, wiec import
//       przez `tentaflow_meeting::...` nie jest mozliwy (analogicznie do
//       `wake_word_match.rs`).
// =============================================================================

use criterion::{black_box, criterion_group, criterion_main, Criterion};

const REQUEST_KEYWORDS: &[&str] = &[
    "podaj", "powiedz", "pokaż", "pokaz", "sprawdź", "sprawdz",
    "wyświetl", "wyswietl", "wyślij", "wyslij", "zrób", "zrob",
    "stwórz", "stworz", "dodaj", "usuń", "usun", "edytuj",
    "znajdź", "znajdz", "wyszukaj", "uruchom", "zatrzymaj",
    "włącz", "wlacz", "wyłącz", "wylacz", "otwórz", "otworz",
    "zamknij", "kontynuuj", "przerwij", "anuluj",
    "podsumuj", "wytłumacz", "wytlumacz", "wyjaśnij", "wyjasnij",
    "opisz", "przeczytaj",
];

fn local_intent_classifier(text: &str, _wake_words: &[String]) -> bool {
    let lower = text.to_lowercase();
    let trimmed = lower.trim_end_matches(|c: char| c.is_whitespace() || c == '.' || c == '!');
    let trimmed = trimmed.trim();

    if trimmed.ends_with('?') {
        return true;
    }
    for keyword in REQUEST_KEYWORDS {
        if lower.contains(keyword) {
            return true;
        }
    }
    let word_count = trimmed.split_whitespace().count();
    if word_count <= 3 {
        return false;
    }
    true
}

fn bench_intent(c: &mut Criterion) {
    let wake_words: Vec<String> = vec!["jarvis".into(), "bot".into(), "asystent".into()];
    let texts = [
        "Hej bot, podaj mi status projektu",
        "Czesc Jarvis",
        "Jak sie masz?",
        "Bot wylacz swiatlo w salonie",
        "OK, dziekuje",
    ];

    c.bench_function("intent_classify_5_texts", |b| {
        b.iter(|| {
            for t in texts.iter() {
                black_box(local_intent_classifier(black_box(t), black_box(&wake_words)));
            }
        });
    });
}

criterion_group!(benches, bench_intent);
criterion_main!(benches);
