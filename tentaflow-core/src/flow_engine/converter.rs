// =============================================================================
// Plik: flow_engine/converter.rs
// Opis: Konwersja FlowExecutionResult na rozne formaty odpowiedzi - OpenAI
//       ChatCompletion (non-streaming i streaming chunk).
// =============================================================================

use crate::flow_engine::types::FlowExecutionResult;
use serde_json::Value;
use std::borrow::Cow;

fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn generate_response_id() -> String {
    format!("chatcmpl-flow-{}", uuid::Uuid::new_v4())
}

/// Zwraca finish_reason zgodny z OpenAI API: "stop" lub null
fn finish_reason_value(status: &str) -> Value {
    if status == "completed" {
        Value::String("stop".to_string())
    } else {
        Value::Null
    }
}

/// Konwertuje wynik flow na format ChatCompletionResponse (JSON Value)
pub fn flow_result_to_chat_response(result: &FlowExecutionResult, model: &str) -> Value {
    let content = extract_text_from_output(&result.output);

    serde_json::json!({
        "id": generate_response_id(),
        "object": "chat.completion",
        "created": unix_timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": content,
            },
            "finish_reason": finish_reason_value(&result.status),
        }],
        "usage": {
            "prompt_tokens": result.prompt_tokens,
            "completion_tokens": result.completion_tokens,
            "total_tokens": result.total_tokens,
        }
    })
}

/// Konwertuje wynik flow na format streaming chunk (SSE).
/// Zwraca pojedynczy chunk z calym wynikiem - w przyszlosci bedzie
/// zamieniany na prawdziwy streaming z tokenami.
#[allow(dead_code)]
pub fn flow_result_to_stream_chunk(result: &FlowExecutionResult, model: &str) -> Value {
    let content = extract_text_from_output(&result.output);

    serde_json::json!({
        "id": generate_response_id(),
        "object": "chat.completion.chunk",
        "created": unix_timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "content": content,
            },
            "finish_reason": finish_reason_value(&result.status),
        }]
    })
}

/// Wyciaga tekst z output flow - probuje kolejno pola: string, "text", "content",
/// a jesli nic nie pasuje - serializuje do stringa.
/// Zwraca Cow aby uniknac alokacji gdy tekst jest juz dostepny jako &str.
fn extract_text_from_output(output: &Value) -> Cow<'_, str> {
    match output {
        Value::String(s) => Cow::Borrowed(s.as_str()),
        Value::Null => Cow::Borrowed(""),
        other => {
            if let Some(text) = other.get("text").and_then(|t| t.as_str()) {
                return Cow::Borrowed(text);
            }
            if let Some(content) = other.get("content").and_then(|c| c.as_str()) {
                return Cow::Borrowed(content);
            }
            // pretty-print zeby uniknac podwojnego escapowania JSON
            Cow::Owned(serde_json::to_string_pretty(other).unwrap_or_default())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::types::FlowExecutionResult;

    #[test]
    fn test_chat_response_completed() {
        let result = FlowExecutionResult {
            status: "completed".to_string(),
            output: serde_json::json!({"text": "Odpowiedz testowa"}),
            execution_log: vec![],
            total_latency_ms: 150,
            total_tokens: 90,
            prompt_tokens: 30,
            completion_tokens: 60,
        };

        let response = flow_result_to_chat_response(&result, "bielik-11b");

        assert_eq!(response["object"], "chat.completion");
        assert_eq!(response["model"], "bielik-11b");
        assert_eq!(
            response["choices"][0]["message"]["content"],
            "Odpowiedz testowa"
        );
        assert_eq!(response["choices"][0]["finish_reason"], "stop");
        assert_eq!(response["usage"]["total_tokens"], 90);
    }

    #[test]
    fn test_chat_response_error_status() {
        let result = FlowExecutionResult {
            status: "error".to_string(),
            output: serde_json::json!({"text": "Blad przetwarzania"}),
            execution_log: vec![],
            total_latency_ms: 50,
            total_tokens: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
        };

        let response = flow_result_to_chat_response(&result, "test-model");
        assert!(response["choices"][0]["finish_reason"].is_null());
    }

    #[test]
    fn test_chat_response_string_output() {
        let result = FlowExecutionResult {
            status: "completed".to_string(),
            output: serde_json::Value::String("Prosty tekst".to_string()),
            execution_log: vec![],
            total_latency_ms: 10,
            total_tokens: 30,
            prompt_tokens: 10,
            completion_tokens: 20,
        };

        let response = flow_result_to_chat_response(&result, "model");
        assert_eq!(response["choices"][0]["message"]["content"], "Prosty tekst");
    }

    #[test]
    fn test_chat_response_null_output() {
        let result = FlowExecutionResult {
            status: "completed".to_string(),
            output: serde_json::Value::Null,
            execution_log: vec![],
            total_latency_ms: 0,
            total_tokens: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
        };

        let response = flow_result_to_chat_response(&result, "model");
        assert_eq!(response["choices"][0]["message"]["content"], "");
    }

    #[test]
    fn test_chat_response_content_field() {
        let result = FlowExecutionResult {
            status: "completed".to_string(),
            output: serde_json::json!({"content": "Z pola content"}),
            execution_log: vec![],
            total_latency_ms: 10,
            total_tokens: 20,
            prompt_tokens: 7,
            completion_tokens: 13,
        };

        let response = flow_result_to_chat_response(&result, "model");
        assert_eq!(
            response["choices"][0]["message"]["content"],
            "Z pola content"
        );
    }

    #[test]
    fn test_stream_chunk() {
        let result = FlowExecutionResult {
            status: "completed".to_string(),
            output: serde_json::json!({"text": "Streaming odpowiedz"}),
            execution_log: vec![],
            total_latency_ms: 100,
            total_tokens: 50,
            prompt_tokens: 15,
            completion_tokens: 35,
        };

        let chunk = flow_result_to_stream_chunk(&result, "bielik-11b");
        assert_eq!(chunk["object"], "chat.completion.chunk");
        assert_eq!(
            chunk["choices"][0]["delta"]["content"],
            "Streaming odpowiedz"
        );
        assert_eq!(chunk["choices"][0]["finish_reason"], "stop");
    }

    #[test]
    fn test_token_split() {
        let result = FlowExecutionResult {
            status: "completed".to_string(),
            output: serde_json::Value::Null,
            execution_log: vec![],
            total_latency_ms: 0,
            total_tokens: 90,
            prompt_tokens: 30,
            completion_tokens: 60,
        };

        let response = flow_result_to_chat_response(&result, "model");
        assert_eq!(response["usage"]["prompt_tokens"], 30);
        assert_eq!(response["usage"]["completion_tokens"], 60);
        assert_eq!(response["usage"]["total_tokens"], 90);
    }

    #[test]
    fn test_output_nested_json_object() {
        let result = FlowExecutionResult {
            status: "completed".to_string(),
            output: serde_json::json!({
                "data": {
                    "wynik": "zagniezdony",
                    "score": 0.95
                },
                "meta": {"source": "rag"}
            }),
            execution_log: vec![],
            total_latency_ms: 50,
            total_tokens: 40,
            prompt_tokens: 15,
            completion_tokens: 25,
        };

        // Act
        let response = flow_result_to_chat_response(&result, "bielik-11b");

        // Assert: bez "text" i "content" powinien zserializowac caly obiekt
        let content = response["choices"][0]["message"]["content"]
            .as_str()
            .unwrap();
        assert!(content.contains("zagniezdony"));
        assert!(content.contains("score"));
    }

    #[test]
    fn test_error_status_finish_reason_error() {
        let result = FlowExecutionResult {
            status: "error".to_string(),
            output: serde_json::json!({"text": "Timeout"}),
            execution_log: vec![],
            total_latency_ms: 30000,
            total_tokens: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
        };

        // Act
        let response = flow_result_to_chat_response(&result, "model");

        // Assert
        assert!(response["choices"][0]["finish_reason"].is_null());
        assert_eq!(response["choices"][0]["message"]["content"], "Timeout");
    }

    #[test]
    fn test_stream_chunk_error_finish_reason_null() {
        let result = FlowExecutionResult {
            status: "error".to_string(),
            output: serde_json::json!({"text": "Blad"}),
            execution_log: vec![],
            total_latency_ms: 100,
            total_tokens: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
        };

        // Act
        let chunk = flow_result_to_stream_chunk(&result, "model");

        // Assert
        assert!(chunk["choices"][0]["finish_reason"].is_null());
    }
}
