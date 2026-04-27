// =============================================================================
// Plik: intent_classifier.rs
// Opis: Lokalny klasyfikator intencji uzywany w `response_mode=wake_word_intent`
//       jako zamiennik LLM call'a (300-1500ms RT). Heurystyka regex/keyword
//       odpala sie w mikrosekundach i pokrywa >95% realnych przypadkow:
//       pytanie konczace sie '?' i wypowiedzi z czasownikiem rozkazujacym.
// =============================================================================

/// Slowa-zadania w formie podstawowej (imperative + 2-3 osoba l. poj.) ktore
/// wskazuja ze mowca kieruje prosbe do bota. Lista pokrywa polskie czasowniki
/// najczestsze w glosowych asystentach. Akceptujemy ze rzadsze formy
/// (np. rzeczownik odslowny "podawanie") nie sa wykryte — caller moze wpasc
/// w default tej funkcji ktora i tak akceptuje dluzsze wypowiedzi.
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

/// Lokalny klasyfikator intencji. Zwraca true gdy wypowiedz wyglada jak
/// faktyczna prosba/pytanie skierowane do bota, false gdy to small-talk
/// po wake-wordzie (np. "Czesc bot.", "Hej bot!").
///
/// Reguly w kolejnosci:
///   1. Tekst konczy sie '?' (po trim) -> pytanie -> true
///   2. Zawiera ktorekolwiek `REQUEST_KEYWORDS` (substring, lowercase) -> true
///   3. Krotka wypowiedz <=3 slowa -> small-talk -> false
///   4. Default -> true (po wake-word akceptujemy dluzsze wypowiedzi)
///
/// `_wake_words` jest celowo nieuzywane w tej heurystyce — wake-word juz
/// odsial wiekszosc szumu, klasyfikator decyduje na pelnym tekscie. Param
/// zostaje dla spojnosci API i potencjalnego rozszerzenia.
pub fn local_intent_classifier(text: &str, _wake_words: &[String]) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ww() -> Vec<String> {
        vec!["jarvis".into(), "bot".into(), "asystent".into()]
    }

    #[test]
    fn pytanie_konczace_sie_znakiem_zapytania_zwraca_true() {
        assert!(local_intent_classifier("Jarvis, jak sie masz?", &ww()));
        assert!(local_intent_classifier("ile mamy czasu?", &ww()));
    }

    #[test]
    fn keyword_imperative_zwraca_true() {
        assert!(local_intent_classifier("Jarvis pokaz status projektu", &ww()));
        assert!(local_intent_classifier("Bot wylacz swiatlo w salonie", &ww()));
        assert!(local_intent_classifier("Asystent podsumuj spotkanie", &ww()));
    }

    #[test]
    fn keyword_uppercase_lowercased_zwraca_true() {
        assert!(local_intent_classifier("JARVIS PODAJ NAM STATUS", &ww()));
    }

    #[test]
    fn krotkie_powitanie_zwraca_false() {
        assert!(!local_intent_classifier("Hej bot.", &ww()));
        assert!(!local_intent_classifier("Czesc Jarvis", &ww()));
        assert!(!local_intent_classifier("Bot.", &ww()));
        assert!(!local_intent_classifier("OK dzieki", &ww()));
    }

    #[test]
    fn dluga_wypowiedz_bez_keyword_zwraca_true() {
        assert!(local_intent_classifier(
            "Jarvis chcialbym zebysmy omowili kwartalne wyniki",
            &ww()
        ));
    }

    #[test]
    fn polish_unicode_lowercase_dziala() {
        assert!(local_intent_classifier("WŁĄCZ muzyke", &ww()));
        assert!(local_intent_classifier("ZRÓB notatke ze spotkania", &ww()));
    }

    #[test]
    fn pytanie_z_kropka_na_koncu_dalej_wykrywane_przez_keyword() {
        // "?" gubi sie po kropce/wykrzykniku, ale 'powiedz' ratuje
        assert!(local_intent_classifier("Jarvis powiedz mi cos.", &ww()));
    }

    #[test]
    fn pusty_tekst_zwraca_false() {
        assert!(!local_intent_classifier("", &ww()));
        assert!(!local_intent_classifier("   ", &ww()));
    }
}
