// ===== File: e2e_toolcall.rs — GPU integration test: Qwen3-0.6B tool calling over HTTP =====
// Ignored by default: needs a CUDA GPU and the local Qwen3-0.6B snapshot at
// test-models/qwen3-0.6b (may still be downloading — the test waits for a
// stable, complete safetensors file and skips cleanly if it never appears).
// Run: cargo test -p forge-server --release --test e2e_toolcall -- --ignored --nocapture

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use forge_engine::model::ModelConfig;
use forge_engine::server::spawn_engine;
use forge_hal::cuda::{CudaDevice, PoolSizes};
use forge_hal::Device;
use forge_server::source::{kv_pool_bytes, load_model, read_descriptor};
use forge_server::toolcall::ToolParserKind;
use forge_server::{build_router, ServerConfig, ServerState};

fn model_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test-models/qwen3-0.6b")
}

/// Wait until the snapshot looks complete: sidecar files present and the
/// safetensors file large and size-stable across consecutive polls (it may
/// be mid-download when the test starts). Returns false on timeout.
async fn wait_for_model(dir: &Path, timeout: Duration) -> bool {
    const MIN_WEIGHTS_BYTES: u64 = 1_000_000_000;
    let deadline = std::time::Instant::now() + timeout;
    let mut last_size = None;
    while std::time::Instant::now() < deadline {
        let sidecars_ready = ["config.json", "tokenizer.json", "tokenizer_config.json"]
            .iter()
            .all(|f| dir.join(f).is_file());
        let size = std::fs::metadata(dir.join("model.safetensors"))
            .ok()
            .map(|m| m.len());
        if sidecars_ready {
            if let Some(size) = size {
                if size >= MIN_WEIGHTS_BYTES && last_size == Some(size) {
                    return true;
                }
                last_size = Some(size);
            }
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a CUDA GPU and the local Qwen3-0.6B snapshot"]
async fn qwen3_tool_calls_end_to_end() {
    let dir = model_dir();
    if !wait_for_model(&dir, Duration::from_secs(300)).await {
        eprintln!(
            "SKIP: Qwen3-0.6B snapshot not available/complete at {}",
            dir.display()
        );
        return;
    }

    let load_dir = dir.clone();
    let (handle, tokenizer, template_vars, eos_ids, chat_template, tool_parser) =
        tokio::task::spawn_blocking(move || {
            let kv_page_size = 32;
            let kv_pages = 256;
            let desc = read_descriptor(&load_dir).expect("read model descriptor");
            let device = CudaDevice::new(
                0,
                PoolSizes {
                    weights: 3 << 30,
                    kv_cache: kv_pool_bytes(&desc, kv_page_size, kv_pages, forge_engine::kv::KvQuant::F16),
                    activations: 1 << 30,
                    kv_page_size: PoolSizes::DEFAULT_KV_PAGE,
                },
            )
            .expect("cuda device");
            let dev: Arc<dyn Device> = device;
            let loaded = load_model(
                dev,
                &load_dir,
                ModelConfig {
                    kv_page_size,
                    kv_pages,
                    max_seq_len: 4096,
                    kv_quant: forge_engine::kv::KvQuant::F16,
                    kv_tier: Default::default(),
                },
            )
            .expect("load model");
            let tool_parser = ToolParserKind::resolve(
                None,
                &loaded.model.weights.descriptor.arch,
                &loaded.chat_template,
            )
            .expect("resolve tool parser");
            let template_vars = loaded.bundle.template_vars();
            let eos_ids = loaded.bundle.eos_ids.clone();
            let tokenizer = Arc::new(loaded.bundle.tokenizer);
            let handle = spawn_engine(loaded.model, tokenizer.clone(), 2, 16);
            (
                handle,
                tokenizer,
                template_vars,
                eos_ids,
                loaded.chat_template,
                tool_parser,
            )
        })
        .await
        .expect("model load task");
    // Qwen3's template carries Hermes markers, so detection must pick Hermes.
    assert_eq!(tool_parser, ToolParserKind::Hermes);

    let cfg = ServerConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        model_id: "qwen3-0.6b".into(),
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
        2,
        tool_parser,
        None,
        None,
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, build_router(state)).await.unwrap();
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
        .unwrap();
    let base = format!("http://{addr}");

    let tools = serde_json::json!([{
        "type": "function",
        "function": {
            "name": "get_weather",
            "description": "Get the current weather for a city",
            "parameters": {
                "type": "object",
                "properties": {
                    "city": {"type": "string", "description": "City name"}
                },
                "required": ["city"]
            }
        }
    }]);

    // tool_choice "required" is not implemented and must fail honestly.
    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "qwen3-0.6b",
            "messages": [{"role": "user", "content": "hi"}],
            "tools": tools,
            "tool_choice": "required"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // Non-streaming tool call.
    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "qwen3-0.6b",
            "messages": [{"role": "user", "content": "Jaka jest pogoda w Krakowie?"}],
            "tools": tools,
            "temperature": 0.0,
            "max_tokens": 1024
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let message = &body["choices"][0]["message"];
    println!("non-stream message: {message}");
    println!("finish_reason: {}", body["choices"][0]["finish_reason"]);

    let tool_calls = message["tool_calls"].as_array();
    match tool_calls {
        Some(calls) if !calls.is_empty() => {
            println!("RESULT: model emitted a structured tool call");
            assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");
            let call = &calls[0];
            assert!(call["id"].as_str().unwrap().starts_with("call_"));
            assert_eq!(call["type"], "function");
            assert_eq!(call["function"]["name"], "get_weather");
            let args = call["function"]["arguments"].as_str().unwrap();
            let args_json: serde_json::Value = serde_json::from_str(args).unwrap();
            println!("arguments: {args_json}");
            assert!(
                args.contains("Krak"),
                "arguments should mention Kraków, got: {args}"
            );
            // Raw markers must never leak into content.
            let content = message["content"].as_str().unwrap_or("");
            assert!(!content.contains("<tool_call>"));
        }
        _ => {
            // The 0.6B model may answer in prose instead of calling the tool;
            // report which path happened and require non-empty output.
            println!("RESULT: model did NOT call the tool; raw content follows");
            let content = message["content"].as_str().unwrap_or("");
            let reasoning = message["reasoning_content"].as_str().unwrap_or("");
            println!("content: {content}");
            println!("reasoning_content: {reasoning}");
            assert!(
                !content.trim().is_empty() || !reasoning.trim().is_empty(),
                "model produced no output at all"
            );
        }
    }
    // Qwen3 thinks by default; reasoning must be extracted, never in content.
    if let Some(content) = message["content"].as_str() {
        assert!(!content.contains("<think>"), "think block leaked: {content}");
    }

    // Streaming: same request; tool calls arrive as incremental deltas.
    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "qwen3-0.6b",
            "messages": [{"role": "user", "content": "Jaka jest pogoda w Krakowie?"}],
            "tools": tools,
            "temperature": 0.0,
            "max_tokens": 1024,
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let raw = resp.text().await.unwrap();
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut calls: Vec<serde_json::Value> = Vec::new();
    let mut finish_reason = None;
    for line in raw.lines() {
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        if data == "[DONE]" {
            break;
        }
        let chunk: serde_json::Value = serde_json::from_str(data).unwrap();
        let delta = &chunk["choices"][0]["delta"];
        if let Some(piece) = delta["content"].as_str() {
            content.push_str(piece);
        }
        if let Some(piece) = delta["reasoning_content"].as_str() {
            reasoning.push_str(piece);
        }
        if let Some(tc) = delta["tool_calls"].as_array() {
            calls.extend(tc.iter().cloned());
        }
        if let Some(f) = chunk["choices"][0]["finish_reason"].as_str() {
            finish_reason = Some(f.to_string());
        }
    }
    println!("stream content: {content}");
    println!("stream reasoning ({} chars)", reasoning.len());
    println!("stream tool_calls: {calls:?}");
    println!("stream finish_reason: {finish_reason:?}");
    assert!(finish_reason.is_some(), "stream must carry a finish_reason");
    assert!(
        !content.contains("<tool_call>") && !content.contains("<think>"),
        "markers leaked into streamed content: {content}"
    );
    if !calls.is_empty() {
        assert_eq!(finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!(calls[0]["index"], 0);
        assert_eq!(calls[0]["function"]["name"], "get_weather");
        assert!(calls[0]["function"]["arguments"].as_str().unwrap().contains("Krak"));
    } else {
        assert!(
            !content.trim().is_empty() || !reasoning.trim().is_empty(),
            "stream produced no output at all"
        );
    }
}
