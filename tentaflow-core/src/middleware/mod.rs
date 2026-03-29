// ============================================================================
// MIDDLEWARE - Request/Response Filtering Pipeline
// ============================================================================
//
// CEL:
// Ten moduł implementuje middleware pipeline dla requestów i responsów.
// Główna funkcja to ResponseMiddleware - filtrowanie PII (Personal Identifiable
// Information) z odpowiedzi LLM zanim trafi do klienta lub TTS.
//
// JAK DZIAŁA:
// ResponseMiddleware skanuje tekst w poszukiwaniu wrażliwych danych:
// - Imiona i nazwiska (polskie patronimy, nazwiska)
// - NIP (10 cyfr: 123-456-78-90 lub 1234567890)
// - PESEL (11 cyfr: 12345678901)
// - Adresy email (jan.kowalski@example.com)
// - Numery telefonów (+48 123 456 789, 123-456-789)
// - Adresy (ul. Długa 5/10, 00-123 Warszawa)
//
// Dla każdego wykrytego PII, tekst jest zastępowany placeholderem:
// - "[IMIĘ NAZWISKO]" dla imion i nazwisk
// - "[NIP]" dla numerów NIP
// - "[PESEL]" dla numerów PESEL
// - "[EMAIL]" dla adresów email
// - "[TELEFON]" dla numerów telefonów
// - "[ADRES]" dla adresów pocztowych
//
// PRZYKŁAD UŻYCIA:
// ```rust
// let middleware = ResponseMiddleware::new(config)?;
//
// // Non-streaming mode (cały tekst naraz)
// let cleaned = middleware.clean_text("Jan Kowalski ma NIP 1234567890")?;
// // Result: "[IMIĘ NAZWISKO] ma NIP [NIP]"
//
// // Streaming mode (chunk po chunku z buforem)
// let mut processor = middleware.streaming_processor();
// for token in llm_stream {
//     if let Some(chunks) = processor.process_token(&token)? {
//         for chunk in chunks {
//             send_to_client(chunk).await?;
//         }
//     }
// }
// ```
//
// KLUCZOWE KONCEPCJE:
// - PII (Personal Identifiable Information): Dane osobowe wymagające ochrony
// - Pattern matching: Wykrywanie PII przez regex patterns
// - Buffering: W streaming mode buforujemy ~6 tokenów żeby wykryć multi-token PII
// - Sentence boundary: Preferujemy wysyłanie chunków do końca zdania (. ! ? , ;)
//
// UWAGI:
// - Patterns są zoptymalizowane dla polskich danych (NIP, PESEL, polskie nazwiska)
// - W streaming mode może być opóźnienie ~6 tokenów (buffering dla accuracy)
// - ResponseMiddleware NIE modyfikuje metadanych - tylko content
//
// ============================================================================

pub mod pii;
pub mod response;

pub use response::{ResponseMiddleware, StreamingProcessor};
