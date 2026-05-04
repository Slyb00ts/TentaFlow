// =============================================================================
// Plik: services/tts/processor.rs
// Opis: Procesor buforujacy tokeny dla TTS — dzieli tekst na segmenty po
//       sentence boundary, czyści tekst z emotikon i skrotow, wysyla do syntezy.
// =============================================================================

use crate::error::Result;

use regex::Regex;
use std::sync::OnceLock;
use tracing::debug;

/// Regex do usuwania wielokrotnych spacji
fn spaces_regex() -> &'static Regex {
    static C: OnceLock<Regex> = OnceLock::new();
    C.get_or_init(|| Regex::new(r"\s{2,}").unwrap())
}

/// Regex do usuwania emotikon (wszystkie Unicode emoji)
fn emoji_regex() -> &'static Regex {
    static C: OnceLock<Regex> = OnceLock::new();
    C.get_or_init(|| {
        Regex::new(concat!(
            r"[\x{1F600}-\x{1F64F}]|",  // Emoticons
            r"[\x{1F300}-\x{1F5FF}]|",  // Misc Symbols and Pictographs
            r"[\x{1F680}-\x{1F6FF}]|",  // Transport and Map
            r"[\x{1F700}-\x{1F77F}]|",  // Alchemical Symbols
            r"[\x{1F780}-\x{1F7FF}]|",  // Geometric Shapes Extended
            r"[\x{1F800}-\x{1F8FF}]|",  // Supplemental Arrows-C
            r"[\x{1F900}-\x{1F9FF}]|",  // Supplemental Symbols and Pictographs
            r"[\x{1FA00}-\x{1FA6F}]|",  // Chess Symbols
            r"[\x{1FA70}-\x{1FAFF}]|",  // Symbols and Pictographs Extended-A
            r"[\x{2600}-\x{26FF}]|",    // Misc symbols (sun, moon, etc.)
            r"[\x{2700}-\x{27BF}]|",    // Dingbats
            r"[\x{FE00}-\x{FE0F}]|",    // Variation Selectors
            r"[\x{1F000}-\x{1F02F}]|",  // Mahjong Tiles
            r"[\x{1F0A0}-\x{1F0FF}]"    // Playing Cards
        ))
        .unwrap()
    })
}

/// Case-insensitive zamiana tekstu bez kompilacji regex przy kazdym wywolaniu.
/// Indeksy z haystack_lower odpowiadaja bajtom haystack (lowercase nie zmienia
/// pozycji w ASCII/latin, a match_indices zwraca pozycje w haystack_lower).
fn case_insensitive_replace(haystack: &str, needle: &str, replacement: &str) -> String {
    let needle_lower = needle.to_lowercase();
    let haystack_lower = haystack.to_lowercase();
    let mut result = String::with_capacity(haystack.len());
    let mut last_end = 0;

    for (start, matched) in haystack_lower.match_indices(&needle_lower) {
        // Jesli lowercase zmienilo dlugosc bajtowa (np. ss->ss), pomijamy trafienie
        if !haystack.is_char_boundary(start) || !haystack.is_char_boundary(start + matched.len()) {
            continue;
        }
        result.push_str(&haystack[last_end..start]);
        result.push_str(replacement);
        last_end = start + matched.len();
    }
    result.push_str(&haystack[last_end..]);
    result
}

/// Czysci tekst przed wyslaniem do TTS:
/// - Usuwa emotikony (bo TTS je dziwnie czyta)
/// - Usuwa znaki formatowania markdown (* dla bold)
/// - Rozwija popularne skroty polskie
/// - Usuwa nadmiarowe spacje
fn clean_text_for_tts(text: &str) -> String {
    let mut result = text.to_string();

    // Usun emotikony
    result = emoji_regex().replace_all(&result, "").to_string();

    // Usun znaki markdown formatting (gwiazdki do bold/italic)
    result = result.replace("**", "").replace("*", "");

    // Rozwin polskie skroty (case-insensitive gdzie ma sens)
    let abbreviations: &[(&str, &str)] = &[
        ("SI", "es i"),
        ("AI", "ej aj"),
        ("np.", "na przyklad"),
        ("m.in.", "miedzy innymi"),
        ("itd.", "i tak dalej"),
        ("itp.", "i tym podobne"),
        ("tzw.", "tak zwany"),
        ("tzn.", "to znaczy"),
        ("tj.", "to jest"),
        ("dr.", "doktor"),
        ("dr ", "doktor "),
        ("mgr.", "magister"),
        ("mgr ", "magister "),
        ("inz.", "inzynier"),
        ("inz ", "inzynier "),
        ("prof.", "profesor"),
        ("prof ", "profesor "),
        ("ul.", "ulica"),
        ("al.", "aleja"),
        ("pl.", "plac"),
        ("os.", "osiedle"),
        ("nr.", "numer"),
        ("nr ", "numer "),
        ("tel.", "telefon"),
        ("godz.", "godzina"),
        ("min.", "minut"),
        ("sek.", "sekund"),
        ("pkt.", "punkt"),
        ("pkt ", "punkt "),
        ("str.", "strona"),
        ("r.", "roku"),
        ("w.", "wiek"),
        ("ok.", "okolo"),
        ("ok ", "okolo "),
        ("wg.", "wedlug"),
        ("wg ", "wedlug "),
        ("dot.", "dotyczacy"),
        ("ds.", "do spraw"),
        ("ws.", "w sprawie"),
        ("zob.", "zobacz"),
        ("por.", "porownaj"),
        ("przyp.", "przypis"),
        ("red.", "redakcja"),
        ("wyd.", "wydanie"),
        ("zl.", "zlotych"),
        ("zl ", "zlotych "),
        ("gr.", "groszy"),
        ("gr ", "groszy "),
        ("tys.", "tysiecy"),
        ("mln.", "milionow"),
        ("mln ", "milionow "),
        ("mld.", "miliardow"),
        ("mld ", "miliardow "),
        ("cdn.", "ciag dalszy nastapi"),
        ("c.d.n.", "ciag dalszy nastapi"),
        ("ps.", "post scriptum"),
        ("ps ", "post scriptum "),
        ("sp.", "swietej pamieci"),
        ("jw.", "jak wyzej"),
        ("dz.", "dzien"),
        ("mies.", "miesiac"),
        ("tyg.", "tydzien"),
        ("pn.", "poniedzialek"),
        ("wt.", "wtorek"),
        ("sr.", "sroda"),
        ("czw.", "czwartek"),
        ("pt.", "piatek"),
        ("sob.", "sobota"),
        ("niedz.", "niedziela"),
        ("ndz.", "niedziela"),
        ("sty.", "styczen"),
        ("lut.", "luty"),
        ("mar.", "marzec"),
        ("kwi.", "kwiecien"),
        ("maj.", "maj"),
        ("cze.", "czerwiec"),
        ("lip.", "lipiec"),
        ("sie.", "sierpien"),
        ("wrz.", "wrzesien"),
        ("paz.", "pazdziernik"),
        ("lis.", "listopad"),
        ("gru.", "grudzien"),
        ("itp", "i tym podobne"),
        ("itd", "i tak dalej"),
    ];

    for (abbr, expanded) in abbreviations {
        result = case_insensitive_replace(&result, abbr, expanded);
    }

    // Korekty fonetyczne dla slow zle wymawianych przez TTS (Piper/espeak-ng)
    let phonetic_fixes: &[(&str, &str)] = &[
        ("dzisiaj", "dzisaj"),
        ("dzisiejszy", "dzisejszy"),
        ("ze", "ze"),
        ("sie", "sie"),
    ];

    for (word, phonetic) in phonetic_fixes {
        result = result.replace(word, phonetic);
    }

    // Usun wielokrotne spacje
    result = spaces_regex().replace_all(&result, " ").to_string();

    // Trim
    result.trim().to_string()
}

/// Procesor buforujacy tokeny dla TTS.
///
/// Buforuje tokeny az do wykrycia sentence boundary (. ! ? ... ;).
/// NIE dzielimy po X tokenach - to tnie slowa w polowie!
///
/// Callback do syntezy mowy - pozwala na uzycie roznych backendow (QUIC/HTTP)
pub type SynthesizeCallback = Box<
    dyn Fn(
            String,
            String,
            String,
            f32,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<u8>>> + Send>>
        + Send
        + Sync,
>;

pub struct TTSBufferingProcessor {
    /// Callback do syntezy mowy (uzywa Router.synthesize_speech pod spodem)
    synthesize_fn: SynthesizeCallback,

    /// Bufor tekstowy - jeden String zamiast VecDeque<String> (mniej alokacji)
    text_buffer: String,

    /// Licznik tokenow w buforze (do logowania)
    token_count: usize,

    /// Maksymalny rozmiar bufora w bajtach - fallback flush nawet bez sentence boundary
    max_buffer_size: usize,

    /// Znaki sentence boundary (wysylamy chunk gdy je wykryjemy)
    sentence_boundaries: &'static [char],

    /// Parametry TTS (z requestu)
    model: String,
    voice: String,
    #[allow(dead_code)]
    format: String,
    speed: f32,
}

impl TTSBufferingProcessor {
    /// Tworzy nowy TTS buffering processor z callback do syntezy.
    ///
    /// Parametry:
    /// - `synthesize_fn`: Callback do syntezy mowy (model, input, voice, speed) -> audio bytes
    /// - `model`: Model TTS (np. "tentaflow-tts")
    /// - `voice`: Glos (np. "jarvis", "justyna")
    /// - `format`: Format audio (np. "wav", "mp3")
    /// - `speed`: Predkosc mowy (1.0 = normalna)
    pub fn new(
        synthesize_fn: SynthesizeCallback,
        model: String,
        voice: String,
        format: String,
        speed: f32,
    ) -> Self {
        Self {
            synthesize_fn,
            text_buffer: String::new(),
            token_count: 0,
            max_buffer_size: 1000,
            sentence_boundaries: &['.', '!', '?', '\u{2026}', ';'],
            model,
            voice,
            format,
            speed,
        }
    }

    /// Przetwarza kolejny token ze streamu (po PII filtering).
    ///
    /// Dodaje token do bufora. Jesli wykryje sentence boundary lub bufor
    /// przekroczyl limit — flushuje i zwraca audio bytes.
    pub async fn process_token(&mut self, token: &str) -> Result<Option<Vec<u8>>> {
        self.text_buffer.push_str(token);
        self.token_count += 1;

        // Flush przy sentence boundary lub gdy bufor przekroczyl limit bajtow
        let has_boundary = token.chars().any(|c| self.sentence_boundaries.contains(&c));
        let over_limit = self.text_buffer.len() >= self.max_buffer_size;

        if has_boundary || over_limit {
            self.flush_buffer("TTS sentence flush").await
        } else {
            Ok(None)
        }
    }

    /// Flush pozostalych tokenow z bufora (koniec streamu).
    ///
    /// Wywolaj na koncu streamu zeby wyslac ostatnie tokeny ktore pozostaly w buforze.
    pub async fn flush(&mut self) -> Result<Option<Vec<u8>>> {
        if self.text_buffer.is_empty() {
            return Ok(None);
        }
        self.flush_buffer("TTS final flush").await
    }

    /// Wspolna logika flush - czysci bufor, przetwarza tekst i syntezuje audio
    async fn flush_buffer(&mut self, log_label: &str) -> Result<Option<Vec<u8>>> {
        let raw_text = std::mem::take(&mut self.text_buffer);
        let token_count = self.token_count;
        self.token_count = 0;

        let text = clean_text_for_tts(&raw_text);

        if text.is_empty() {
            return Ok(None);
        }

        let text_preview: String = text.chars().take(50).collect();
        debug!(
            "{}: {} tokenow -> {} bajtow (po czyszczeniu: {}), text='{}'",
            log_label,
            token_count,
            raw_text.len(),
            text.len(),
            text_preview
        );

        let audio_bytes =
            (self.synthesize_fn)(self.model.clone(), text, self.voice.clone(), self.speed).await?;

        Ok(Some(audio_bytes))
    }

    /// Zwraca rozmiar aktualnego bufora w bajtach (dla debugging/monitoring)
    pub fn buffer_len(&self) -> usize {
        self.text_buffer.len()
    }

    /// Zwraca glos uzywany do syntezy
    pub fn voice(&self) -> &str {
        &self.voice
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_tts_processor_buffering() {
        // Test wymaga TTS API key w zmiennej srodowiskowej
        // W prawdziwych testach uzyjemy mock servera (wiremock)
    }

    #[tokio::test]
    async fn test_tts_processor_sentence_boundary() {
        // Test weryfikujacy flush przy sentence boundary
    }
}
