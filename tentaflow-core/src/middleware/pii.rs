// ============================================================================
// PII DETECTION - Wzorce dla danych osobowych
// ============================================================================
//
// CEL:
// Zbiór regex patterns i logiki wykrywania PII (Personal Identifiable Information)
// w tekście. Zoptymalizowane dla polskich danych osobowych.
//
// JAK DZIAŁA:
// Każdy typ PII ma własny regex pattern dopasowany do polskiego formatu:
// - NIP: 10 cyfr z opcjonalnymi myślnikami (123-456-78-90 lub 1234567890)
// - PESEL: 11 cyfr (12345678901)
// - Email: standard RFC 5322 (uproszczony)
// - Telefon: polski format z opcjonalnym +48
// - Nazwiska: Lista najczęstszych polskich nazwisk + patronimy (-ski, -cki, -dzki)
// - Adresy: ul./al./pl. + nazwa + numer + kod pocztowy
//
// KLUCZOWE KONCEPCJE:
// - Lazy static: Patterns są kompilowane raz przy starcie (nie za każdym razem)
// - Priority matching: Sprawdzamy w kolejności od najbardziej specyficznych
// - False positives: Preferujemy wykryć za dużo niż za mało (bezpieczeństwo > accuracy)
//
// UWAGI:
// - Patterns mogą mieć false positives (np. losowe 10 cyfr wykryte jako NIP)
// - To jest trade-off: lepiej zredagować za dużo niż za mało
// - Nazwiska sprawdzamy tylko dla najczęstszych (top 1000) + patronimy
//
// ============================================================================

use lazy_static::lazy_static;
use regex::Regex;
use std::collections::HashSet;

lazy_static! {
    /// Regex dla numeru NIP (10 cyfr, opcjonalnie z myślnikami)
    ///
    /// Formaty:
    /// - 1234567890
    /// - 123-456-78-90
    /// - 123 456 78 90
    pub static ref NIP_PATTERN: Regex = Regex::new(
        r"\b\d{3}[-\s]?\d{3}[-\s]?\d{2}[-\s]?\d{2}\b"
    ).unwrap();

    /// Regex dla numeru PESEL (11 cyfr)
    ///
    /// Format: 12345678901 (bez myślników)
    pub static ref PESEL_PATTERN: Regex = Regex::new(
        r"\b\d{11}\b"
    ).unwrap();

    /// Regex dla adresu email
    ///
    /// Uproszczona wersja RFC 5322 - wystarczająco dobra dla 99% przypadków
    pub static ref EMAIL_PATTERN: Regex = Regex::new(
        r"\b[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}\b"
    ).unwrap();

    /// Regex dla numeru telefonu (polski format)
    ///
    /// Formaty:
    /// - +48 123 456 789
    /// - 48 123 456 789
    /// - 123 456 789
    /// - 123-456-789
    /// - (12) 345-67-89 (stacjonarny)
    pub static ref PHONE_PATTERN: Regex = Regex::new(
        r"\b(?:\+?48[\s-]?)?(?:\(?\d{2,3}\)?[\s-]?)?\d{3}[\s-]?\d{3}[\s-]?\d{2,3}\b"
    ).unwrap();

    /// Regex dla adresu pocztowego (ulica + numer + opcjonalnie kod pocztowy)
    ///
    /// Formaty:
    /// - ul. Długa 5
    /// - al. Jerozolimskie 123/45
    /// - pl. Zamkowy 1, 00-123 Warszawa
    pub static ref ADDRESS_PATTERN: Regex = Regex::new(
        r"(?:ul\.|al\.|pl\.|os\.|rynek)\s+[A-ZĄĆĘŁŃÓŚŹŻ][a-ząćęłńóśźż]+(?:\s+[A-ZĄĆĘŁŃÓŚŹŻ][a-ząćęłńóśźż]+)*\s+\d+(?:/\d+)?(?:,?\s+\d{2}-\d{3}\s+[A-ZĄĆĘŁŃÓŚŹŻ][a-ząćęłńóśźż]+)?"
    ).unwrap();

    /// Regex dla polskich nazwisk (patronimy: -ski, -cki, -dzki, -ny, -ny, etc.)
    ///
    /// Wykrywa nazwiska kończące się typowymi polskimi sufiksami
    /// UWAGA: To może dać false positives dla przymiotników
    pub static ref SURNAME_PATTERN: Regex = Regex::new(
        r"\b[A-ZĄĆĘŁŃÓŚŹŻ][a-ząćęłńóśźż]+(?:ski|cki|dzki|ska|cka|dzka|wicz|owicz|ewicz|ak|ek|ik|czyk|yk)\b"
    ).unwrap();

    /// HashSet imion dla O(1) lookup zamiast liniowego przeszukiwania tablicy
    static ref COMMON_NAMES_SET: HashSet<&'static str> =
        COMMON_POLISH_NAMES.iter().copied().collect();

    /// HashSet nazwisk dla O(1) lookup zamiast liniowego przeszukiwania tablicy
    static ref COMMON_SURNAMES_SET: HashSet<&'static str> =
        COMMON_POLISH_SURNAMES.iter().copied().collect();
}

/// Lista najczęstszych polskich imion (dla zwiększenia accuracy)
///
/// Top 100 najpopularniejszych imion męskich i żeńskich w Polsce.
/// Używamy tej listy aby zredukować false positives z NAME_PATTERN.
pub const COMMON_POLISH_NAMES: &[&str] = &[
    // Imiona męskie (top 50)
    "Jan",
    "Piotr",
    "Paweł",
    "Andrzej",
    "Krzysztof",
    "Tomasz",
    "Marcin",
    "Michał",
    "Kamil",
    "Jakub",
    "Adam",
    "Łukasz",
    "Mateusz",
    "Wojciech",
    "Marek",
    "Grzegorz",
    "Rafał",
    "Dariusz",
    "Bartosz",
    "Maciej",
    "Mariusz",
    "Jacek",
    "Artur",
    "Robert",
    "Zbigniew",
    "Stanisław",
    "Jerzy",
    "Tadeusz",
    "Zenon",
    "Zdzisław",
    "Kazimierz",
    "Władysław",
    "Bogdan",
    "Ryszard",
    "Henryk",
    "Janusz",
    "Mirosław",
    "Leszek",
    "Czesław",
    "Józef",
    "Witold",
    "Eugeniusz",
    "Sławomir",
    "Ireneusz",
    "Damian",
    "Sebastian",
    "Filip",
    "Karol",
    "Szymon",
    "Dawid",
    // Imiona żeńskie (top 50)
    "Anna",
    "Maria",
    "Katarzyna",
    "Małgorzata",
    "Agnieszka",
    "Barbara",
    "Ewa",
    "Krystyna",
    "Elżbieta",
    "Magdalena",
    "Joanna",
    "Teresa",
    "Zofia",
    "Jadwiga",
    "Danuta",
    "Irena",
    "Halina",
    "Helena",
    "Beata",
    "Aleksandra",
    "Dorota",
    "Jolanta",
    "Renata",
    "Grażyna",
    "Stanisława",
    "Wanda",
    "Janina",
    "Marianna",
    "Urszula",
    "Bożena",
    "Iwona",
    "Justyna",
    "Monika",
    "Sylwia",
    "Karolina",
    "Natalia",
    "Paulina",
    "Weronika",
    "Martyna",
    "Agata",
    "Julia",
    "Zuzanna",
    "Oliwia",
    "Maja",
    "Lena",
    "Alicja",
    "Amelia",
    "Nikola",
    "Wiktoria",
    "Emilia",
];

/// Lista najczęstszych polskich nazwisk (dla zwiększenia accuracy)
///
/// Top 100 najpopularniejszych nazwisk w Polsce.
/// Używamy tej listy razem z SURNAME_PATTERN dla lepszej detekcji.
pub const COMMON_POLISH_SURNAMES: &[&str] = &[
    "Nowak",
    "Kowalski",
    "Wiśniewski",
    "Dąbrowski",
    "Lewandowski",
    "Wójcik",
    "Kamiński",
    "Kowalczyk",
    "Zieliński",
    "Szymański",
    "Woźniak",
    "Kozłowski",
    "Jankowski",
    "Wojciechowski",
    "Kwiatkowski",
    "Kaczmarek",
    "Mazur",
    "Krawczyk",
    "Piotrowski",
    "Grabowski",
    "Nowakowski",
    "Pawłowski",
    "Michalski",
    "Nowicki",
    "Adamczyk",
    "Dudek",
    "Zając",
    "Wieczorek",
    "Jabłoński",
    "Król",
    "Majewski",
    "Olszewski",
    "Jaworski",
    "Wróbel",
    "Malinowski",
    "Pawlak",
    "Witkowski",
    "Walczak",
    "Stępień",
    "Górski",
    "Rutkowski",
    "Michalak",
    "Sikora",
    "Ostrowski",
    "Baran",
    "Duda",
    "Szewczyk",
    "Tomaszewski",
    "Pietrzak",
    "Marciniak",
    "Wróblewski",
    "Zalewski",
    "Jakubowski",
    "Jasiński",
    "Zawadzki",
    "Sadowski",
    "Bąk",
    "Chmielewski",
    "Włodarczyk",
    "Borkowski",
    "Czarnecki",
    "Sawicki",
    "Sokołowski",
    "Urbański",
    "Kubiak",
    "Maciejewski",
    "Szczepański",
    "Kucharski",
    "Wilk",
    "Kalinowski",
    "Lis",
    "Mazurek",
    "Wysocki",
    "Adamski",
    "Kaźmierczak",
    "Wasilewski",
    "Sobczak",
    "Czerwiński",
    "Andrzejewski",
    "Cieślak",
    "Głowacki",
    "Zakrzewski",
    "Kołodziej",
    "Sikorski",
    "Krajewski",
    "Gajewski",
    "Szymczak",
    "Laskowski",
    "Ziółkowski",
    "Makowski",
    "Baranowski",
    "Urbanek",
    "Kaczmarczyk",
    "Kozak",
    "Komiński",
    "Nawrocki",
    "Przybylski",
    "Wyszyński",
    "Markowski",
    "Małecki",
    "Kasprzak",
];

/// Sprawdza czy słowo jest popularnym polskim imieniem.
///
/// Używamy do filtrowania false positives z NAME_PATTERN.
///
/// Parametry:
/// - `word`: Słowo do sprawdzenia (case-sensitive)
///
/// Zwraca: true jeśli jest w whitelist imion
pub fn is_common_name(word: &str) -> bool {
    COMMON_NAMES_SET.contains(word)
}

/// Sprawdza czy słowo jest popularnym polskim nazwiskiem.
///
/// Sprawdza zarówno whitelistę jak i patronimy (-ski, -cki, etc.).
///
/// Parametry:
/// - `word`: Słowo do sprawdzenia (case-sensitive)
///
/// Zwraca: true jeśli jest nazwiskiem
pub fn is_common_surname(word: &str) -> bool {
    COMMON_SURNAMES_SET.contains(word) || has_polish_surname_suffix(word)
}

/// Sufiksy typowe dla polskich nazwisk patronimicznych
const POLISH_SURNAME_SUFFIXES: &[&str] = &[
    "dzki", "dzka", "ski", "ska", "cki", "cka", "owicz", "ewicz", "wicz", "czyk", "ak", "ek", "ik",
    "yk",
];

/// Sprawdza czy slowo konczy sie typowym sufiksem polskiego nazwiska.
/// Wymaga wielkiej litery na poczatku i co najmniej 3 znakow.
fn has_polish_surname_suffix(word: &str) -> bool {
    if word.len() < 3 {
        return false;
    }
    let first = word.chars().next().unwrap_or(' ');
    if !first.is_uppercase() {
        return false;
    }
    POLISH_SURNAME_SUFFIXES
        .iter()
        .any(|suffix| word.ends_with(suffix))
}

/// Wykrywa pełne imię i nazwisko w tekście (format: "Imię Nazwisko").
///
/// Algorytm:
/// 1. Szuka pary słów z wielkiej litery oddzielonych spacją
/// 2. Sprawdza czy pierwsze jest imieniem (whitelist)
/// 3. Sprawdza czy drugie jest nazwiskiem (whitelist lub patronim)
/// 4. Jeśli oba pasują → wykryte imię+nazwisko
///
/// Parametry:
/// - `text`: Tekst do przeskanowania
///
/// Zwraca: Vec pozycji (start, end) gdzie wykryto imię+nazwisko
pub fn detect_full_names(text: &str) -> Vec<(usize, usize)> {
    if text.is_empty() {
        return Vec::new();
    }

    let mut results = Vec::new();
    let mut prev_word: Option<(&str, usize)> = None;

    // Iterujemy po slowach z pozycjami bajtowymi bez alokacji Vec
    for word in text.split_whitespace() {
        let offset = word.as_ptr() as usize - text.as_ptr() as usize;
        if let Some((prev, prev_start)) = prev_word {
            if is_common_name(prev) && is_common_surname(word) {
                let end = offset + word.len();
                results.push((prev_start, end));
            }
        }
        prev_word = Some((word, offset));
    }

    results
}

/// Pelne czyszczenie tekstu z PII - wykrywa imiona+nazwiska, NIP, PESEL, email, telefon, adres.
/// Zwraca oczyszczony tekst i flage czy cos zostalo zredagowane.
pub fn sanitize_pii(text: &str) -> (String, bool) {
    let mut cleaned = text.to_string();
    let mut redacted = false;

    // Imiona i nazwiska (najwyzszy priorytet, przed regex patterns)
    let full_names = detect_full_names(&cleaned);
    for (start, end) in full_names.iter().rev() {
        cleaned.replace_range(*start..*end, "[IMIĘ NAZWISKO]");
        redacted = true;
    }

    let patterns: &[(&regex::Regex, &str)] = &[
        (&NIP_PATTERN, "[NIP]"),
        (&PESEL_PATTERN, "[PESEL]"),
        (&EMAIL_PATTERN, "[EMAIL]"),
        (&PHONE_PATTERN, "[TELEFON]"),
        (&ADDRESS_PATTERN, "[ADRES]"),
    ];

    for &(pattern, replacement) in patterns {
        let result = pattern.replace_all(&cleaned, replacement);
        if let std::borrow::Cow::Owned(replaced) = result {
            cleaned = replaced;
            redacted = true;
        }
    }

    (cleaned, redacted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nip_detection() {
        assert!(NIP_PATTERN.is_match("1234567890"));
        assert!(NIP_PATTERN.is_match("123-456-78-90"));
        assert!(NIP_PATTERN.is_match("123 456 78 90"));
        assert!(!NIP_PATTERN.is_match("12345")); // Za krótki
    }

    #[test]
    fn test_pesel_detection() {
        assert!(PESEL_PATTERN.is_match("12345678901"));
        assert!(!PESEL_PATTERN.is_match("1234567890")); // Za krótki (10 cyfr)
    }

    #[test]
    fn test_email_detection() {
        assert!(EMAIL_PATTERN.is_match("jan.kowalski@example.com"));
        assert!(EMAIL_PATTERN.is_match("test@test.pl"));
        assert!(!EMAIL_PATTERN.is_match("invalid@")); // Nieprawidłowy
    }

    #[test]
    fn test_phone_detection() {
        assert!(PHONE_PATTERN.is_match("+48 123 456 789"));
        assert!(PHONE_PATTERN.is_match("123-456-789"));
        assert!(PHONE_PATTERN.is_match("123 456 789"));
    }

    #[test]
    fn test_surname_detection() {
        assert!(is_common_surname("Kowalski"));
        assert!(is_common_surname("Nowak"));
        assert!(is_common_surname("Wiśniewski")); // Patronim
    }

    #[test]
    fn test_name_detection() {
        assert!(is_common_name("Jan"));
        assert!(is_common_name("Anna"));
        assert!(!is_common_name("Xyz")); // Nie ma w whitelist
    }

    #[test]
    fn test_full_name_detection() {
        let text = "Jan Kowalski ma NIP 1234567890";
        let names = detect_full_names(text);
        assert_eq!(names.len(), 1);
        assert_eq!(&text[names[0].0..names[0].1], "Jan Kowalski");
    }
}
