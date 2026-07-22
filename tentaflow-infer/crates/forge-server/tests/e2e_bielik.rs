// ===== File: e2e_bielik.rs — GPU integration test: serve the Bielik NVFP4 snapshot over HTTP =====
// Ignored by default: needs a CUDA GPU and the local model snapshot. Run:
//   cargo test -p forge-server --release -- --ignored

use std::path::Path;
use std::sync::Arc;

use forge_engine::model::ModelConfig;
use forge_engine::server::spawn_engine;
use forge_hal::cuda::{CudaDevice, PoolSizes};
use forge_hal::Device;
use forge_server::source::{kv_pool_bytes, load_model, read_descriptor};
use forge_server::{build_router, ServerConfig, ServerState};

const MODEL_DIR: &str = "/home/critix/repos/rust/TentaFlow/.runtime/models/models--TentaFlow--Bielik-PL-Minitron-7B-NVFP4/snapshots/831550e879fd7d700e3f6d79dffc14373deda3a7";

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a CUDA GPU and the local Bielik snapshot"]
async fn chat_completions_end_to_end() {
    let model_dir = Path::new(MODEL_DIR);
    assert!(
        model_dir.is_dir(),
        "test model snapshot missing at {MODEL_DIR}"
    );

    // Model load (~45 s) happens off the async runtime.
    let engine = tokio::task::spawn_blocking(move || {
        let kv_page_size = 32;
        let kv_pages = 160;
        let desc = read_descriptor(model_dir).expect("read model descriptor");
        let device = CudaDevice::new(
            0,
            PoolSizes {
                weights: 8 << 30,
                kv_cache: kv_pool_bytes(&desc, kv_page_size, kv_pages, forge_engine::kv::KvQuant::F16, false),
                activations: 1 << 30,
                kv_page_size: PoolSizes::DEFAULT_KV_PAGE,
            },
        )
        .expect("cuda device");
        let dev: Arc<dyn Device> = device;
        let loaded = load_model(
            dev,
            model_dir,
            ModelConfig {
                kv_page_size,
                kv_pages,
                max_seq_len: 4096,
                kv_quant: forge_engine::kv::KvQuant::F16,
                kv_tier: Default::default(),
                prefix_cache: false,
                native_mtp: false,
            },
        )
        .expect("load model");
        let template_vars = loaded.bundle.template_vars();
        let eos_ids = loaded.bundle.eos_ids.clone();
        let tokenizer = Arc::new(loaded.bundle.tokenizer);
        let handle = spawn_engine(loaded.model, tokenizer.clone(), 4, 16)
            .expect("silnik powinien się uruchomić");
        (
            handle,
            tokenizer,
            template_vars,
            eos_ids,
            loaded.chat_template,
        )
    })
    .await
    .expect("model load task");
    let (handle, tokenizer, template_vars, eos_ids, chat_template) = engine;

    let cfg = ServerConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        model_id: "bielik-7b-nvfp4".into(),
        api_key: None,
        tool_call_parser: None,
    };
    let state = ServerState::new(
        &cfg,
        handle,
        tokenizer,
        template_vars,
        eos_ids,
        chat_template,
        4096,
        4,
        forge_server::toolcall::ToolParserKind::None,
        None,
        None,
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, build_router(state)).await.unwrap();
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .unwrap();
    let base = format!("http://{addr}");

    // Health check.
    let health: serde_json::Value = client
        .get(format!("{base}/healthz"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["status"], "ok");

    // Wrong model id → 404 model_not_found.
    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "some-other-model",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let err: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(err["error"]["code"], "model_not_found");

    // n > 1 → 400.
    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "bielik-7b-nvfp4",
            "messages": [{"role": "user", "content": "hi"}],
            "n": 2
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // Over-budget max_tokens → 400 context_length_exceeded.
    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "bielik-7b-nvfp4",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 100000
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let err: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(err["error"]["code"], "context_length_exceeded");

    // Non-streaming chat completion.
    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "bielik-7b-nvfp4",
            "messages": [
                {"role": "system", "content": "Odpowiadasz krótko po polsku."},
                {"role": "user", "content": "Napisz jedno zdanie o Krakowie."}
            ],
            "temperature": 0.0,
            "max_tokens": 64
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let content = body["choices"][0]["message"]["content"].as_str().unwrap();
    println!("non-stream content: {content}");
    println!("usage: {}", body["usage"]);
    assert!(!content.trim().is_empty(), "empty completion content");
    assert!(
        content.contains("Krak"),
        "completion should mention Kraków, got: {content}"
    );
    let finish = body["choices"][0]["finish_reason"].as_str().unwrap();
    assert!(finish == "stop" || finish == "length");
    assert!(body["usage"]["prompt_tokens"].as_u64().unwrap() > 0);
    assert!(body["usage"]["completion_tokens"].as_u64().unwrap() > 0);

    // Streaming chat completion.
    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "bielik-7b-nvfp4",
            "messages": [
                {"role": "system", "content": "Odpowiadasz krótko po polsku."},
                {"role": "user", "content": "Jak nazywa się stolica Polski?"}
            ],
            "temperature": 0.0,
            "max_tokens": 48,
            "stream": true,
            "stream_options": {"include_usage": true}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));

    let raw = resp.text().await.unwrap();
    let mut streamed = String::new();
    let mut finish_reason = None;
    let mut usage_completion = 0u64;
    let mut saw_done = false;
    for line in raw.lines() {
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        if data == "[DONE]" {
            saw_done = true;
            break;
        }
        let chunk: serde_json::Value = serde_json::from_str(data).unwrap();
        assert_eq!(chunk["object"], "chat.completion.chunk");
        if chunk["choices"].as_array().is_some_and(|c| c.is_empty()) {
            // include_usage: usage arrives as a dedicated final chunk with
            // an empty choices array; the finish chunk carries no usage.
            usage_completion = chunk["usage"]["completion_tokens"].as_u64().unwrap_or(0);
            assert!(finish_reason.is_some(), "usage chunk must follow finish");
            continue;
        }
        if let Some(piece) = chunk["choices"][0]["delta"]["content"].as_str() {
            streamed.push_str(piece);
        }
        if let Some(f) = chunk["choices"][0]["finish_reason"].as_str() {
            finish_reason = Some(f.to_string());
            assert!(
                chunk.get("usage").is_none(),
                "finish chunk must not carry usage unless include_usage puts it in its own chunk"
            );
        }
    }
    println!("streamed content: {streamed}");
    println!("streamed finish_reason: {finish_reason:?}, completion_tokens: {usage_completion}");
    assert!(saw_done, "stream must end with data: [DONE]");
    assert!(!streamed.trim().is_empty(), "empty streamed content");
    assert!(
        streamed.contains("Warszawa"),
        "streamed answer should mention Warszawa, got: {streamed}"
    );
    let finish_reason = finish_reason.expect("final chunk must carry finish_reason");
    assert!(finish_reason == "stop" || finish_reason == "length");
    assert!(usage_completion > 0);
}
