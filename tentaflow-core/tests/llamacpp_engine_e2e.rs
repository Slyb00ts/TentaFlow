// =============================================================================
// Plik: llamacpp_engine_e2e.rs
// Opis: Integracyjny test silnika llama.cpp na poziomie core (LlamaCppEngine):
//       load_model + generate + generate_stream na realnym GGUF, weryfikacja
//       poprawnej odpowiedzi oraz zwolnienia zasobów (unload). Wymaga modelu
//       wskazanego przez env TENTAFLOW_LLAMA_TEST_MODEL i jest #[ignore], bo
//       ładuje wagi na GPU (uruchamiaj jawnie: cargo test --features
//       inference-llamacpp,gpu-cuda --test llamacpp_engine_e2e -- --ignored).
// =============================================================================

#![cfg(feature = "inference-llamacpp")]

use std::path::PathBuf;

use tentaflow_core::inference::llamacpp::LlamaCppEngine;
use tentaflow_core::inference::{
    DeployParamsSnapshot, GenerateParams, InferenceEngine, StopReason,
};

fn model_path() -> Option<PathBuf> {
    std::env::var("TENTAFLOW_LLAMA_TEST_MODEL")
        .ok()
        .map(PathBuf::from)
        .filter(|p| p.exists())
}

fn deploy_params() -> DeployParamsSnapshot {
    let mut s = DeployParamsSnapshot::default();
    s.llamacpp
        .insert("n_gpu_layers".into(), serde_json::json!(99));
    s.llamacpp
        .insert("ctx_size".into(), serde_json::json!(2048));
    s.llamacpp.insert("n_parallel".into(), serde_json::json!(2));
    s
}

#[tokio::test]
#[ignore = "wymaga GPU + GGUF wskazanego przez TENTAFLOW_LLAMA_TEST_MODEL"]
async fn generate_and_stream_then_release() {
    let Some(path) = model_path() else {
        eprintln!("pominięto: brak TENTAFLOW_LLAMA_TEST_MODEL");
        return;
    };

    let engine = LlamaCppEngine::new();
    let info = engine
        .load_model(&path, &deploy_params())
        .await
        .expect("load_model powiodło się");
    assert!(info.loaded, "model oznaczony jako załadowany");
    assert_eq!(info.backend, "llamacpp");

    // Blokujące generate: sensowna odpowiedź + realne prompt_tokens (CR-004).
    let params = GenerateParams {
        prompt: "Wymień trzy kolory tęczy, oddzielone przecinkami.".to_string(),
        max_tokens: 64,
        temperature: 0.7,
        ..GenerateParams::default()
    };
    let result = engine
        .generate(params)
        .await
        .expect("generate powiodło się");
    assert!(
        !result.text.trim().is_empty(),
        "generate zwrócił niepustą odpowiedź"
    );
    assert!(result.tokens_generated > 0, "generate zgłosił >0 tokenów");
    assert!(
        result.prompt_tokens > 0,
        "CR-004: realne prompt_tokens z silnika (nie 0)"
    );
    assert!(
        matches!(
            result.stop_reason,
            StopReason::EndOfText | StopReason::MaxTokens | StopReason::StopSequence(_)
        ),
        "naturalny powód zakończenia, nie błąd"
    );

    // Streaming generate_stream: zbierz tokeny, sprawdź finalny token bez błędu.
    let stream_params = GenerateParams {
        prompt: "Napisz jedno zdanie o oceanach.".to_string(),
        max_tokens: 64,
        ..GenerateParams::default()
    };
    let mut rx = engine
        .generate_stream(stream_params)
        .await
        .expect("generate_stream powiodło się");
    let mut streamed = String::new();
    let mut final_error: Option<String> = None;
    let mut saw_final = false;
    while let Some(token) = rx.recv().await {
        streamed.push_str(&token.text);
        if token.is_final {
            saw_final = true;
            final_error = token.error.clone();
            // CR-003: na finale silnik niesie realny finish_reason (lub błąd).
            assert!(
                token.finish_reason.is_some() || token.error.is_some(),
                "finalny token ma finish_reason albo error"
            );
            break;
        }
    }
    assert!(saw_final, "strumień dostarczył token finalny");
    assert!(
        final_error.is_none(),
        "CR-003: brak błędu silnika w streamie"
    );
    assert!(!streamed.trim().is_empty(), "strumień zwrócił tekst");

    // Zwolnienie zasobów: unload nie błądzi i model_info znika.
    engine
        .unload_model()
        .await
        .expect("unload_model powiodło się");
    assert!(
        engine.model_info().is_none(),
        "po unload model_info() jest None — zasoby zwolnione"
    );
}
