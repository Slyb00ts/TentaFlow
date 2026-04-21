// ============================================================================
// RESPONSE MIDDLEWARE - Filtrowanie PII z odpowiedzi LLM
// ============================================================================
//
// CEL:
// Implementacja middleware który filtruje PII (Personal Identifiable Information)
// z odpowiedzi LLM zanim trafi do klienta lub TTS. Obsługuje zarówno non-streaming
// (cały tekst naraz) jak i streaming mode (token po tokenie z buforem).
//
// JAK DZIAŁA:
// **Non-streaming mode:**
// 1. Otrzymuje cały tekst naraz
// 2. Skanuje patterns (NIP, PESEL, email, telefon, imię+nazwisko, adres)
// 3. Zastępuje wykryte PII placeholderami
// 4. Zwraca oczyszczony tekst
//
// **Streaming mode:**
// 1. Buforuje ~6 tokenów (lub do sentence boundary: . ! ? , ;)
// 2. Gdy bufor pełny lub boundary → skanuje cały chunk
// 3. Zastępuje PII w chunku
// 4. Zwraca oczyszczone chunki do klienta/TTS
// 5. Repeat dla kolejnych tokenów
//
// Dlaczego buffering? Bo PII może składać się z wielu tokenów:
// - "Jan" + " " + "Kowalski" → trzeba zobaczyć razem żeby wykryć imię+nazwisko
// - "123" + "456" + "78" + "90" → trzeba zobaczyć razem żeby wykryć NIP
//
// PRZYKŁAD UŻYCIA:
// ```rust
// let middleware = ResponseMiddleware::new();
//
// // Non-streaming
// let cleaned = middleware.clean_text("Jan Kowalski ma NIP 1234567890")?;
// // => "[IMIĘ NAZWISKO] ma NIP [NIP]"
//
// // Streaming
// let mut processor = middleware.streaming_processor();
// for token in llm_tokens {
//     if let Some(chunks) = processor.process_token(&token)? {
//         for chunk in chunks {
//             send_to_client(chunk).await?;
//         }
//     }
// }
// // Flush ostatnich tokenów z bufora
// let remaining = processor.flush()?;
// ```
//
// KLUCZOWE KONCEPCJE:
// - Smart buffering: 6 tokenów OR sentence boundary (. ! ? , ;) - co nastąpi pierwsze
// - Priority scanning: Sprawdzamy w kolejności (full names, NIP, PESEL, email, phone, address)
// - Placeholder types: Różne dla każdego typu PII (dla auditability)
// - Stateless: Każdy chunk jest niezależny (nie ma global state między chunkami)
//
// UWAGI:
// - Buffering wprowadza opóźnienie ~6 tokenów w streaming mode
// - Trade-off: accuracy (więcej wykrytego PII) vs latency (szybsza odpowiedź)
// - False positives są OK (lepiej zredagować za dużo niż za mało)
//
// ============================================================================

use crate::error::Result;
use crate::middleware::pii;
use std::mem;
use tracing::debug;

/// ResponseMiddleware - filtrowanie PII z odpowiedzi LLM.
///
/// Singleton struct - może być współdzielony między wieloma requestami (thread-safe).
/// Wszystkie metody są &self (immutable reference).
pub struct ResponseMiddleware {
    /// Czy middleware jest włączony (z config)
    enabled: bool,
}

impl ResponseMiddleware {
    /// Tworzy nową instancję ResponseMiddleware.
    ///
    /// Parametry:
    /// - `enabled`: Czy middleware ma faktycznie filtrować (false = noop passthrough)
    ///
    /// Zwraca: Nową instancję middleware
    pub fn new(enabled: bool) -> Self {
        debug!("ResponseMiddleware utworzony (enabled: {})", enabled);
        Self { enabled }
    }

    /// Czyści tekst z PII (non-streaming mode).
    ///
    /// Skanuje cały tekst naraz i zastępuje wszystkie wykryte PII placeholderami.
    ///
    /// Algorytm:
    /// 1. Jeśli disabled → zwróć tekst bez zmian
    /// 2. Wykryj i zastąp pełne imiona+nazwiska → "[IMIĘ NAZWISKO]"
    /// 3. Wykryj i zastąp NIPy → "[NIP]"
    /// 4. Wykryj i zastąp PESELe → "[PESEL]"
    /// 5. Wykryj i zastąp emaile → "[EMAIL]"
    /// 6. Wykryj i zastąp telefony → "[TELEFON]"
    /// 7. Wykryj i zastąp adresy → "[ADRES]"
    ///
    /// Parametry:
    /// - `text`: Tekst do wyczyszczenia
    ///
    /// Zwraca: Tekst z zastąpionymi PII
    pub fn clean_text(&self, text: &str) -> Result<String> {
        if !self.enabled {
            return Ok(text.to_string());
        }

        let (cleaned, redacted) = pii::sanitize_pii(text);

        if redacted {
            debug!("Zredagowano PII w tekscie");
        }

        Ok(cleaned)
    }

    /// Tworzy StreamingProcessor dla streaming mode.
    ///
    /// StreamingProcessor utrzymuje bufor tokenów i emituje oczyszczone chunki
    /// gdy bufor osiągnie próg (6 tokenów) lub sentence boundary.
    ///
    /// Zwraca: Nowy processor gotowy do przetwarzania tokenów
    pub fn streaming_processor(&self) -> StreamingProcessor {
        StreamingProcessor::new(self.enabled)
    }
}

/// Processor dla streaming mode - buforuje tokeny i emituje oczyszczone chunki.
///
/// Użycie:
/// ```rust,ignore
/// let mut processor = middleware.streaming_processor();
/// for token in stream {
///     if let Some(chunks) = processor.process_token(&token)? {
///         for chunk in chunks {
///             send_to_client(chunk).await?;
///         }
///     }
/// }
/// let remaining = processor.flush()?; // Na koniec streamu
/// ```
pub struct StreamingProcessor {
    /// Czy middleware jest włączony
    enabled: bool,

    /// Bufor tekstowy (jeden String zamiast N osobnych alokacji)
    text_buffer: String,

    /// Licznik tokenow w buforze
    token_count: usize,

    /// Maksymalny rozmiar bufora (liczba tokenów)
    max_buffer_size: usize,

    /// Znaki sentence boundary (wysyłamy chunk gdy je wykryjemy)
    sentence_boundaries: &'static [char],
}

impl StreamingProcessor {
    // Tworzy nowy StreamingProcessor.
    //
    // Parametry:
    // - `enabled`: Czy middleware ma filtrować (false = passthrough)
    //
    // Zwraca: Nowy processor z pustym buforem
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            text_buffer: String::with_capacity(256),
            token_count: 0,
            max_buffer_size: 6, // ~6 tokenow dla balance accuracy vs latency
            sentence_boundaries: &['.', '!', '?', ',', ';'],
        }
    }

    /// Przetwarza kolejny token ze streamu LLM.
    ///
    /// Algorytm:
    /// 1. Dodaj token do bufora
    /// 2. Sprawdź warunki flush:
    ///    a) Bufor pełny (>= max_buffer_size) LUB
    ///    b) Token kończy się sentence boundary
    /// 3. Jeśli flush → wyczyść bufor z PII i zwróć jako chunki
    /// 4. Jeśli nie → zwróć None (czekaj na więcej tokenów)
    ///
    /// Parametry:
    /// - `token`: Kolejny token ze streamu LLM
    ///
    /// Zwraca: Option<Vec<String>> - chunki do wysłania (lub None jeśli buforujemy)
    pub fn process_token(&mut self, token: &str) -> Result<Option<Vec<String>>> {
        if token.is_empty() {
            return Ok(None);
        }

        // Jesli disabled - passthrough bez buforowania
        if !self.enabled {
            return Ok(Some(vec![token.to_string()]));
        }

        // Dodaj token do bufora tekstowego (jedna alokacja zamiast N)
        self.text_buffer.push_str(token);
        self.token_count += 1;

        // Sprawdz czy flush warunki
        let should_flush = self.token_count >= self.max_buffer_size
            || token.chars().any(|c| self.sentence_boundaries.contains(&c));

        if should_flush {
            let text = mem::take(&mut self.text_buffer);
            self.token_count = 0;

            let cleaned = self.clean_chunk(&text)?;
            Ok(Some(vec![cleaned]))
        } else {
            Ok(None)
        }
    }

    /// Flush pozostałych tokenów z bufora (koniec streamu).
    ///
    /// Wywołaj na końcu streamu żeby wysłać ostatnie tokeny które pozostały w buforze.
    ///
    /// Zwraca: Vec<String> - ostatnie chunki do wysłania
    pub fn flush(&mut self) -> Result<Vec<String>> {
        if !self.enabled || self.text_buffer.is_empty() {
            return Ok(Vec::new());
        }

        let text = mem::take(&mut self.text_buffer);
        self.token_count = 0;

        let cleaned = self.clean_chunk(&text)?;

        Ok(vec![cleaned])
    }

    // Czyści pojedynczy chunk tekstu z PII.
    //
    // Używa tej samej logiki co ResponseMiddleware::clean_text()
    // ale dla pojedynczego chunka.
    //
    // Parametry:
    // - `chunk`: Chunk tekstu do wyczyszczenia
    //
    // Zwraca: Chunk z zastąpionymi PII
    fn clean_chunk(&self, chunk: &str) -> Result<String> {
        let (cleaned, _) = pii::sanitize_pii(chunk);
        Ok(cleaned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_text_full_name() {
        let middleware = ResponseMiddleware::new(true);
        let result = middleware
            .clean_text("Jan Kowalski ma NIP 1234567890")
            .unwrap();
        assert!(result.contains("[IMIĘ NAZWISKO]"));
        assert!(result.contains("[NIP]"));
    }

    #[test]
    fn test_clean_text_email() {
        let middleware = ResponseMiddleware::new(true);
        let result = middleware
            .clean_text("Kontakt: jan.kowalski@example.com")
            .unwrap();
        assert!(result.contains("[EMAIL]"));
    }

    #[test]
    fn test_clean_text_phone() {
        let middleware = ResponseMiddleware::new(true);
        let result = middleware.clean_text("Telefon: +48 123 456 789").unwrap();
        assert!(result.contains("[TELEFON]"));
    }

    #[test]
    fn test_disabled_middleware() {
        let middleware = ResponseMiddleware::new(false);
        let original = "Jan Kowalski ma NIP 1234567890";
        let result = middleware.clean_text(original).unwrap();
        assert_eq!(result, original); // Brak zmian gdy disabled
    }

    #[test]
    fn test_streaming_processor_buffering() {
        let middleware = ResponseMiddleware::new(true);
        let mut processor = middleware.streaming_processor();

        // Dodaj 5 tokenów - nie powinno flush
        for i in 0..5 {
            let result = processor.process_token(&format!("token{}", i)).unwrap();
            assert!(result.is_none()); // Buforujemy
        }

        // 6ty token → flush
        let result = processor.process_token("token5").unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn test_streaming_processor_sentence_boundary() {
        let middleware = ResponseMiddleware::new(true);
        let mut processor = middleware.streaming_processor();

        // Dodaj 3 tokeny
        processor.process_token("Jan").unwrap();
        processor.process_token(" ").unwrap();

        // Token z kropką → flush (sentence boundary)
        let result = processor.process_token("Kowalski.").unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn test_streaming_flush() {
        let middleware = ResponseMiddleware::new(true);
        let mut processor = middleware.streaming_processor();

        // Dodaj 3 tokeny (mniej niż buffer size)
        processor.process_token("Jan").unwrap();
        processor.process_token(" ").unwrap();
        processor.process_token("Kowalski").unwrap();

        // Flush powinien zwrócić bufferowane tokeny
        let result = processor.flush().unwrap();
        assert!(!result.is_empty());
        assert!(result[0].contains("[IMIĘ NAZWISKO]"));
    }
}
