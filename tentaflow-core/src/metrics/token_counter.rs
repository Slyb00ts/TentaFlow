// =============================================================================
// Plik: metrics/token_counter.rs
// Opis: Szybka estymacja liczby tokenow na podstawie dlugosci tekstu (chars/4).
// =============================================================================

/// Szybka estymacja liczby tokenow dla dowolnego tekstu.
///
/// Uzywa heurystyki chars / 4 (zaokraglenie w gore).
/// Dokladnosc ~80% dla tekstu angielskiego, ~60% dla polskiego.
pub fn estimate_tokens(text: &str) -> u64 {
    let len = text.len() as u64;
    // Zaokraglenie w gore: (len + 3) / 4
    (len + 3) / 4
}

/// Zlicza estymowane tokeny wejsciowe z tablicy wiadomosci chat.
///
/// Iteruje po polach "content" kazdej wiadomosci i sumuje estymacje.
pub fn count_request_tokens(messages: &[serde_json::Value]) -> u64 {
    messages
        .iter()
        .filter_map(|msg| msg.get("content"))
        .map(|content| match content {
            serde_json::Value::String(s) => estimate_tokens(s),
            // Multimodal content (tablica z czesciami text/image)
            serde_json::Value::Array(parts) => parts
                .iter()
                .filter_map(|part| part.get("text").and_then(|t| t.as_str()))
                .map(estimate_tokens)
                .sum(),
            _ => 0,
        })
        .sum()
}

/// Zlicza estymowane tokeny wyjsciowe z tekstu odpowiedzi.
pub fn count_response_tokens(text: &str) -> u64 {
    estimate_tokens(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_tokens_empty() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn test_estimate_tokens_short() {
        // 5 znakow -> (5+3)/4 = 2
        assert_eq!(estimate_tokens("hello"), 2);
    }

    #[test]
    fn test_estimate_tokens_exact_multiple() {
        // 8 znakow -> (8+3)/4 = 2 (zaokraglenie)
        assert_eq!(estimate_tokens("abcdefgh"), 2);
    }

    #[test]
    fn test_count_request_tokens() {
        let messages = vec![
            serde_json::json!({"role": "system", "content": "Jestes asystentem."}),
            serde_json::json!({"role": "user", "content": "Czesc!"}),
        ];
        let tokens = count_request_tokens(&messages);
        assert!(tokens > 0);
    }

    #[test]
    fn test_count_request_tokens_multimodal() {
        let messages = vec![serde_json::json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "Co widzisz na obrazku?"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,..."}}
            ]
        })];
        let tokens = count_request_tokens(&messages);
        assert!(tokens > 0);
    }

    #[test]
    fn test_count_response_tokens() {
        let text = "To jest odpowiedz modelu AI.";
        let tokens = count_response_tokens(text);
        assert_eq!(tokens, estimate_tokens(text));
    }
}
