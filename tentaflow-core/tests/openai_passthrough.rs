// =============================================================================
// File: tests/openai_passthrough.rs
// Purpose: Guards the /v1 gateway passthrough of `response_format.json_schema`
//          and unknown vendor fields (`guided_json`, ...) through the typed
//          ChatCompletionRequest round-trip and the real BackendClient HTTP
//          path against a wiremock fake vLLM.
// =============================================================================

use serde_json::{json, Value};
use tentaflow_core::api::openai::types::{ChatCompletionRequest, MessageContent, ResponseFormat};
use tentaflow_core::config::{ConnectionType, ServiceBackend};
use tentaflow_core::services::backend::BackendClient;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn client_request_json() -> Value {
    json!({
        "model": "qwen",
        "messages": [{"role": "user", "content": "Give me a person"}],
        "max_tokens": 64,
        "stream": false,
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "person",
                "strict": true,
                "schema": {
                    "type": "object",
                    "properties": {
                        "name": {"type": "string"},
                        "age": {"type": "integer"}
                    },
                    "required": ["name", "age"]
                }
            },
            "vendor_hint": "keep-me"
        },
        "guided_json": {"type": "object", "properties": {"name": {"type": "string"}}},
        "chat_template_kwargs": {"enable_thinking": false},
        "top_k": 40,
        "min_p": 0.05,
        "seed": 7
    })
}

#[test]
fn chat_request_round_trip_keeps_vendor_fields() {
    let input = client_request_json();
    let parsed: ChatCompletionRequest = serde_json::from_value(input.clone()).unwrap();

    let rf: &ResponseFormat = parsed.response_format.as_ref().unwrap();
    assert_eq!(rf.format_type, "json_schema");
    assert_eq!(
        rf.json_schema,
        Some(input["response_format"]["json_schema"].clone())
    );
    assert_eq!(rf.extra.get("vendor_hint"), Some(&json!("keep-me")));
    assert_eq!(parsed.max_tokens, Some(64));
    assert!(!parsed.stream);
    assert_eq!(parsed.extra.get("top_k"), Some(&json!(40)));
    assert_eq!(parsed.extra.get("seed"), Some(&json!(7)));
    // Typed fields never leak into the vendor map.
    assert!(!parsed.extra.contains_key("model"));
    assert!(!parsed.extra.contains_key("response_format"));

    let out = serde_json::to_value(&parsed).unwrap();
    for key in [
        "model",
        "messages",
        "max_tokens",
        "response_format",
        "guided_json",
        "chat_template_kwargs",
        "top_k",
        "min_p",
        "seed",
    ] {
        assert_eq!(out[key], input[key], "field {key} changed in round-trip");
    }
}

#[tokio::test]
async fn backend_client_forwards_json_schema_and_guided_json() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(|req: &Request| {
            let body: Value = serde_json::from_slice(&req.body).expect("json body");
            let expected = client_request_json();
            assert_eq!(body["response_format"], expected["response_format"]);
            assert_eq!(body["guided_json"], expected["guided_json"]);
            assert_eq!(
                body["chat_template_kwargs"],
                expected["chat_template_kwargs"]
            );
            assert_eq!(body["top_k"], json!(40));
            assert_eq!(body["model"], json!("qwen"));
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl-1",
                "object": "chat.completion",
                "created": 1,
                "model": "qwen",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "{\"name\":\"Ada\",\"age\":36}"},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 5, "completion_tokens": 9, "total_tokens": 14}
            }))
        })
        .expect(1)
        .mount(&server)
        .await;

    let backend = ServiceBackend {
        connection: ConnectionType::OpenAIApi {
            url: format!("{}/v1", server.uri()),
            api_key: Some("test-key".into()),
            api_key_env: None,
            extra_headers: Vec::new(),
            custom_endpoint: None,
            request_format: None,
            tts_config: None,
        },
        max_concurrent: 2,
        timeout_ms: 5_000,
        weight: 1,
        model_name_override: None,
        health_check_path: None,
    };
    let client = BackendClient::new(backend, None).expect("backend client");

    let request: ChatCompletionRequest = serde_json::from_value(client_request_json()).unwrap();
    let response = client
        .chat_completion(request)
        .await
        .expect("chat completion");

    let content = match response.choices[0].message.content.as_ref() {
        Some(MessageContent::Text(text)) => text.clone(),
        other => panic!("expected text content, got {other:?}"),
    };
    let parsed: Value = serde_json::from_str(&content).expect("schema-conformant JSON");
    assert_eq!(parsed["name"], json!("Ada"));
    assert_eq!(parsed["age"], json!(36));
}
