// =============================================================================
// Plik: sentence_buffer.rs
// Opis: Akumuluje delta-tokeny z LLM streamu i emituje kompletne zdania w
//       momencie napotkania granicy (`. ! ? \n`). Bot odpala TTS na kazdym
//       wyemitowanym zdaniu — pierwsze audio leci do mikrofonu zanim LLM
//       dokonczy generowanie reszty odpowiedzi.
// =============================================================================

/// Bufor sklejajacy delta-tokeny do momentu znalezienia granicy zdania.
/// Granica = `.`, `!`, `?` lub `\n`. Pomijamy `...` (multiple punctuation)
/// oraz kropki w liczbach (`3.14`) — emisja zdania `Liczba to 3` zamiast
/// `Liczba to 3.14.` brzmialaby gluptawo w TTS.
pub struct SentenceBuffer {
    buf: String,
}

/// Minimalna dlugosc emitowanego zdania w bajtach. Krotsze fragmenty
/// (np. samo "OK.") sa trzymane razem z kolejnym zdaniem — TTS engines daja
/// artefakty przy bardzo krotkim wejsciu, a fragment "OK." samodzielnie nie
/// niesie informacji.
const MIN_SENTENCE_BYTES: usize = 4;

impl SentenceBuffer {
    pub fn new() -> Self {
        Self {
            buf: String::with_capacity(512),
        }
    }

    /// Dokleja `delta` do bufora i zwraca wszystkie nowe kompletne zdania
    /// (po znormalizowaniu whitespace). Reszta tekstu zostaje w buforze.
    pub fn push(&mut self, delta: &str) -> Vec<String> {
        self.buf.push_str(delta);
        let mut sentences = Vec::new();

        // Skanujemy od poczatku bufora; gdy znajdziemy zbyt krotki fragment,
        // przesuwamy scan_from za jego granice (tekst zostaje w buforze
        // jako prefix nastepnej proby). To gwarantuje progress: kazda kolejna
        // iteracja zaczyna od tej samej pozycji i nie zapetla sie na tej
        // samej granicy. Po znalezieniu wystarczajaco dlugiego zdania
        // przepiszemy bufor i zaczniemy od nowa.
        let mut scan_from: usize = 0;
        loop {
            let bytes = self.buf.as_bytes();
            let Some(rel_idx) = find_sentence_boundary(&bytes[scan_from..]) else {
                break;
            };
            let idx = scan_from + rel_idx;

            // Bajtowy split jest bezpieczny: `.`, `!`, `?`, `\n` sa 1-bajtowe
            // w UTF-8, wiec idx zawsze lezy na granicy code-pointa.
            let sentence = self.buf[..idx].trim().to_string();

            if sentence.len() >= MIN_SENTENCE_BYTES {
                let rest = self.buf[idx..].trim_start().to_string();
                self.buf = rest;
                sentences.push(sentence);
                scan_from = 0;
            } else {
                // Za krotkie — pozostawiamy tekst w buforze i kontynuujemy
                // skanowanie zaraz za biezaca granica, zeby przy nastepnej
                // iteracji znalezc kolejna granice (jezeli tekst po niej juz
                // pozwoli na wyemitowanie razem dluzszego fragmentu).
                scan_from = idx;
            }
        }

        sentences
    }

    /// Zwraca pozostala tresc bufora (np. ostatnie zdanie bez kropki) i
    /// czysci bufor. Wolane po `Done` ze streamu LLM.
    pub fn flush(&mut self) -> Option<String> {
        let remaining = std::mem::take(&mut self.buf);
        let trimmed = remaining.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }
}

impl Default for SentenceBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Skanuje bufor szukajac pierwszej granicy zdania. Zwraca indeks _po_
/// znaku granicznym (suffix = od `idx`). `None` gdy granicy nie ma jeszcze
/// w buforze.
fn find_sentence_boundary(bytes: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'\n' | b'!' | b'?' => return Some(i + 1),
            b'.' => {
                // Pomijamy `..`/`...` — kolejna kropka oznacza ze nie konczymy
                // zdania, tylko ellipsis. Skip do konca ciagu kropek.
                if i + 1 < bytes.len() && bytes[i + 1] == b'.' {
                    while i < bytes.len() && bytes[i] == b'.' {
                        i += 1;
                    }
                    continue;
                }
                // Pomijamy kropki dziesietne (`3.14`, `v1.5`).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_split_on_period() {
        let mut buf = SentenceBuffer::new();
        assert_eq!(buf.push("Hello world."), vec!["Hello world."]);
        assert!(buf.flush().is_none());
    }

    #[test]
    fn buffers_until_boundary() {
        let mut buf = SentenceBuffer::new();
        assert!(buf.push("Hello").is_empty());
        assert!(buf.push(" world").is_empty());
        assert_eq!(buf.push("."), vec!["Hello world."]);
    }

    #[test]
    fn multiple_sentences_in_one_push() {
        let mut buf = SentenceBuffer::new();
        let out = buf.push("First sentence. Second sentence! Third?");
        assert_eq!(
            out,
            vec!["First sentence.", "Second sentence!", "Third?"]
        );
    }

    #[test]
    fn ellipsis_does_not_split() {
        let mut buf = SentenceBuffer::new();
        // Same "..." nie zamyka zdania, ale "?" po koncu juz tak.
        assert_eq!(buf.push("Czekaj... A teraz?"), vec!["Czekaj... A teraz?"]);
    }

    #[test]
    fn decimal_does_not_split() {
        let mut buf = SentenceBuffer::new();
        assert_eq!(buf.push("Liczba to 3.14."), vec!["Liczba to 3.14."]);
    }

    #[test]
    fn newline_is_boundary() {
        let mut buf = SentenceBuffer::new();
        assert_eq!(
            buf.push("Punkt pierwszy\nPunkt drugi\n"),
            vec!["Punkt pierwszy", "Punkt drugi"]
        );
    }

    #[test]
    fn flush_returns_remainder() {
        let mut buf = SentenceBuffer::new();
        assert!(buf.push("Bez kropki na koncu").is_empty());
        assert_eq!(buf.flush(), Some("Bez kropki na koncu".to_string()));
        assert!(buf.flush().is_none());
    }

    #[test]
    fn very_short_fragment_merges_with_next() {
        let mut buf = SentenceBuffer::new();
        // "OK." ma 3 bajty < MIN_SENTENCE_BYTES — zostaje w buforze.
        assert!(buf.push("OK.").is_empty());
        // Po dolaczeniu reszty mamy "OK. To jest dluzsze." — emitujemy razem.
        assert_eq!(
            buf.push(" To jest dluzsze."),
            vec!["OK. To jest dluzsze."]
        );
    }

    #[test]
    fn polish_unicode_text_works() {
        let mut buf = SentenceBuffer::new();
        assert_eq!(
            buf.push("Zażółć gęślą jaźń. Następne zdanie."),
            vec!["Zażółć gęślą jaźń.", "Następne zdanie."]
        );
    }

    #[test]
    fn token_by_token_streaming() {
        // Symulacja LLM streamingu — kazdy token to ~kilka znakow.
        let mut buf = SentenceBuffer::new();
        let tokens = ["Cze", "sc!", " Jak", " sie", " masz", "?"];
        let mut all = Vec::new();
        for t in tokens {
            all.extend(buf.push(t));
        }
        if let Some(rest) = buf.flush() {
            all.push(rest);
        }
        assert_eq!(all, vec!["Czesc!", "Jak sie masz?"]);
    }
}
