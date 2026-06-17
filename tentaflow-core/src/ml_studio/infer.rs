// ===== File: ml_studio/infer.rs — lokalna inferencja wdrożonego modelu FT =====
//
// Wspólna ścieżka dla zapytań do modelu FT wdrożonego jako lokalny silnik
// (`/v1` alias). Używa jej zarówno handler A (model lokalny), jak i executor
// mesh na B (model wdrożony na B, zlecenie z A komendą `MlChat`). Wołanie idzie
// PROSTO przez `Router::route_chat_completion` — bez REST i bez klucza API
// (wywołanie wewnętrzne, `user=None`).

use std::sync::Arc;

use crate::api::openai::types::{ChatCompletionRequest, Message, MessageContent};
use crate::routing::router::Router;

/// Zadaje jedno pytanie modelowi `model_name` (alias w routingu tego węzła) i
/// zwraca wygenerowany tekst. `max_tokens` ogranicza długość odpowiedzi.
pub async fn run_local_chat(
    router: &Arc<Router>,
    model_name: &str,
    message: &str,
    max_tokens: u32,
) -> anyhow::Result<String> {
    let request = ChatCompletionRequest {
        model: model_name.to_string(),
        messages: vec![Message {
            role: "user".to_string(),
            content: Some(MessageContent::Text(message.to_string())),
            reasoning_content: None,
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }],
        temperature: Some(0.7),
        max_tokens: Some(max_tokens),
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        stream: false,
        stream_options: None,
        user: None,
        response_format: None,
        tools: None,
        tool_choice: None,
        n: None,
        memory_options: None,
        audio_input: None,
    };

    let route_result = router
        .route_chat_completion(request, None, None)
        .await
        .map_err(|e| anyhow::anyhow!("inferencja modelu '{}' nie powiodła się: {}", model_name, e))?;

    let answer = route_result
        .response
        .choices
        .first()
        .and_then(|choice| choice.message.content.as_ref())
        .map(|content| match content {
            MessageContent::Text(text) => text.clone(),
            MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|p| {
                    if let crate::api::openai::types::ContentPart::Text { text } = p {
                        Some(text.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join(""),
        })
        .unwrap_or_default();

    Ok(answer)
}
