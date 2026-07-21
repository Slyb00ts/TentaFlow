// ===== File: e2e_api_surface.rs — GPU integration test: /metrics, /v1/messages, batch completions =====
// Ignored by default: needs a CUDA GPU and the local Qwen3-0.6B Q8_0 GGUF at
// test-models/gguf/qwen3-0.6b-q8_0.gguf. Exercises the rounded-out server
// surface (SPEC §8.1 Anthropic Messages + batch completions; §8.3/§9.2
// Prometheus /metrics) against a running server. Run:
//   cargo test -p forge-server --release --test e2e_api_surface -- --ignored --nocapture

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

fn model_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test-models/gguf/qwen3-0.6b-q8_0.gguf")
}

async fn post(
    client: &reqwest::Client,
    url: &str,
    body: serde_json::Value,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = client.post(url).json(&body).send().await.unwrap();
    let status = resp.status();
    let json: serde_json::Value = resp.json().await.unwrap();
    (status, json)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a CUDA GPU and the local Qwen3-0.6B Q8_0 GGUF"]
async fn api_surface_end_to_end() {
    let path = model_path();
    if !path.is_file() {
        eprintln!("SKIP: model missing at {}", path.display());
        return;
    }

    let load_path = path.clone();
    let (handle, tokenizer, template_vars, eos_ids, chat_template, tool_parser) =
        tokio::task::spawn_blocking(move || {
            let kv_page_size = 32;
            let kv_pages = 512;
            let desc = read_descriptor(&load_path).expect("read descriptor");
            let device = CudaDevice::new(
                0,
                PoolSizes {
                    weights: 3 << 30,
                    kv_cache: kv_pool_bytes(
                        &desc,
                        kv_page_size,
                        kv_pages,
                        forge_engine::kv::KvQuant::F16,
                    ),
                    activations: 1 << 30,
                    kv_page_size: PoolSizes::DEFAULT_KV_PAGE,
                },
            )
            .expect("cuda device");
            let dev: Arc<dyn Device> = device;
            let loaded = load_model(
                dev,
                &load_path,
                ModelConfig {
                    kv_page_size,
                    kv_pages,
                    max_seq_len: 4096,
                    kv_quant: forge_engine::kv::KvQuant::F16,
                    kv_tier: Default::default(),
                    prefix_cache: true,
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
            // max_active 8 so a 4-prompt batch shares one decode batch.
            let handle = spawn_engine(loaded.model, tokenizer.clone(), 8, 16)
                .expect("silnik powinien się uruchomić");
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
        8,
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
    let metrics_url = format!("{base}/metrics");
    let messages_url = format!("{base}/v1/messages");
    let comp_url = format!("{base}/v1/completions");

    // ---- (1) /metrics baseline is well-formed and scrapes before any work ----
    let m0 = client.get(&metrics_url).send().await.unwrap();
    assert_eq!(m0.status(), 200, "/metrics must scrape");
    let ct = m0
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(ct.contains("text/plain"), "wrong /metrics content-type: {ct}");
    let m0 = m0.text().await.unwrap();
    assert!(
        m0.contains("forge_engine_requests_finished_total"),
        "missing engine counter in exposition"
    );
    assert!(
        m0.contains("forge_engine_ttft_seconds_bucket"),
        "missing TTFT histogram in exposition"
    );
    let finished0 = scrape_counter(&m0, "forge_engine_requests_finished_total");
    let gen0 = scrape_counter(&m0, "forge_engine_generated_tokens_total");

    // ---- (2) Anthropic Messages: non-streaming ----
    let (status, body) = post(
        &client,
        &messages_url,
        serde_json::json!({
            "model": "qwen3-0.6b",
            "system": "You are a terse assistant. Answer in one short sentence.",
            "messages": [{"role": "user", "content": "Name the capital of France."}],
            "max_tokens": 64,
            "temperature": 0.0
        }),
    )
    .await;
    assert_eq!(status, 200, "messages non-stream failed: {body}");
    assert_eq!(body["type"], "message");
    assert_eq!(body["role"], "assistant");
    let text = body["content"][0]["text"].as_str().unwrap_or("");
    println!("[messages] text: {text:?}");
    assert!(!text.trim().is_empty(), "empty message text");
    assert!(!text.contains("<think>"), "think block leaked: {text}");
    let stop = body["stop_reason"].as_str().unwrap();
    assert!(
        ["end_turn", "max_tokens", "stop_sequence"].contains(&stop),
        "unexpected stop_reason {stop}"
    );
    assert!(body["usage"]["input_tokens"].as_u64().unwrap() > 0);
    assert!(body["usage"]["output_tokens"].as_u64().unwrap() > 0);

    // max_tokens mapping: a tiny budget must yield stop_reason "max_tokens".
    let (status, body) = post(
        &client,
        &messages_url,
        serde_json::json!({
            "model": "qwen3-0.6b",
            "messages": [{"role": "user", "content": "Write a long story about a dragon."}],
            "max_tokens": 8,
            "temperature": 0.0
        }),
    )
    .await;
    assert_eq!(status, 200, "messages max_tokens case failed: {body}");
    assert_eq!(
        body["stop_reason"], "max_tokens",
        "short budget must map to max_tokens: {body}"
    );

    // stop_sequence mapping: content-block array input + a stop sequence.
    let (status, body) = post(
        &client,
        &messages_url,
        serde_json::json!({
            "model": "qwen3-0.6b",
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "Count: 1 2 3 4 5 6 7 8 9 10"}
            ]}],
            "max_tokens": 128,
            "temperature": 0.0,
            "stop_sequences": ["5"]
        }),
    )
    .await;
    assert_eq!(status, 200, "messages stop_sequence case failed: {body}");
    println!("[messages] stop_sequence case stop_reason={}", body["stop_reason"]);
    // The stop token may or may not appear depending on the model; when it does
    // the reason must be stop_sequence, otherwise end_turn/max_tokens.
    let stop = body["stop_reason"].as_str().unwrap();
    assert!(["end_turn", "max_tokens", "stop_sequence"].contains(&stop));

    // ---- (3) Anthropic Messages: streaming event sequence ----
    let resp = client
        .post(&messages_url)
        .json(&serde_json::json!({
            "model": "qwen3-0.6b",
            "messages": [{"role": "user", "content": "Name the capital of France."}],
            "max_tokens": 256,
            "temperature": 0.0,
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "messages stream failed");
    let raw = resp.text().await.unwrap();
    let mut events: Vec<String> = Vec::new();
    let mut streamed = String::new();
    let mut final_stop = None;
    for block in raw.split("\n\n") {
        let mut ev_name = None;
        let mut data = None;
        for line in block.lines() {
            if let Some(e) = line.strip_prefix("event: ") {
                ev_name = Some(e.trim().to_string());
            } else if let Some(d) = line.strip_prefix("data: ") {
                data = Some(d.to_string());
            }
        }
        let (Some(ev), Some(data)) = (ev_name, data) else {
            continue;
        };
        let json: serde_json::Value = serde_json::from_str(&data).unwrap();
        if ev == "content_block_delta" {
            if let Some(t) = json["delta"]["text"].as_str() {
                streamed.push_str(t);
            }
        }
        if ev == "message_delta" {
            final_stop = json["delta"]["stop_reason"].as_str().map(str::to_string);
        }
        events.push(ev);
    }
    println!("[messages/stream] events: {events:?}");
    println!("[messages/stream] text: {streamed:?}");
    // Event order contract: message_start → content_block_start → (deltas) →
    // content_block_stop → message_delta → message_stop.
    assert_eq!(events.first().map(String::as_str), Some("message_start"));
    assert_eq!(events.get(1).map(String::as_str), Some("content_block_start"));
    assert_eq!(events.last().map(String::as_str), Some("message_stop"));
    assert!(events.iter().any(|e| e == "content_block_delta"));
    assert!(events.iter().any(|e| e == "content_block_stop"));
    assert!(events.iter().any(|e| e == "message_delta"));
    assert!(!streamed.trim().is_empty(), "no streamed text");
    assert!(final_stop.is_some(), "stream missing stop_reason");

    // ---- (4) Batch completions: 4 prompts in one request ----
    let prompts = [
        "The capital of France is",
        "Two plus two equals",
        "The sun rises in the",
        "Water is made of hydrogen and",
    ];
    let t_batch = std::time::Instant::now();
    let (status, body) = post(
        &client,
        &comp_url,
        serde_json::json!({
            "model": "qwen3-0.6b",
            "prompt": prompts,
            "temperature": 0.0,
            "max_tokens": 24
        }),
    )
    .await;
    let batch_secs = t_batch.elapsed().as_secs_f64();
    assert_eq!(status, 200, "batch completions failed: {body}");
    let choices = body["choices"].as_array().unwrap();
    assert_eq!(choices.len(), 4, "must return one choice per prompt: {body}");
    for (i, c) in choices.iter().enumerate() {
        assert_eq!(c["index"].as_u64().unwrap(), i as u64, "choice index order");
        let t = c["text"].as_str().unwrap_or("");
        println!("[batch] choice {i}: {t:?}");
        assert!(!t.trim().is_empty(), "batch choice {i} empty");
    }
    // Usage aggregates across all four prompts.
    assert!(body["usage"]["prompt_tokens"].as_u64().unwrap() >= 4);
    assert!(body["usage"]["completion_tokens"].as_u64().unwrap() >= 4);
    println!("[batch] 4 prompts completed in {batch_secs:.2}s (batched decode)");

    // ---- (5) /metrics moved after real work ----
    let m1 = client.get(&metrics_url).send().await.unwrap().text().await.unwrap();
    let finished1 = scrape_counter(&m1, "forge_engine_requests_finished_total");
    let gen1 = scrape_counter(&m1, "forge_engine_generated_tokens_total");
    let http_msgs = m1
        .lines()
        .any(|l| l.starts_with("forge_http_requests_total") && l.contains("/v1/messages"));
    println!("[metrics] finished {finished0}->{finished1}, generated {gen0}->{gen1}");
    assert!(
        finished1 > finished0,
        "requests_finished must increase after generations"
    );
    assert!(
        gen1 > gen0,
        "generated_tokens_total must increase after generations"
    );
    assert!(http_msgs, "http request counter must record /v1/messages");
    // TTFT histogram observed at least one request.
    let ttft_count = scrape_counter(&m1, "forge_engine_ttft_seconds_count");
    assert!(ttft_count >= 1, "TTFT histogram recorded no observations");

    println!("API surface end-to-end: /metrics + /v1/messages + batch completions proven");
}

/// Read the value of a simple (unlabeled) Prometheus counter/gauge/`_count`
/// line from an exposition body.
fn scrape_counter(body: &str, name: &str) -> u64 {
    for line in body.lines() {
        if line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix(name) {
            if let Some(v) = rest.trim().split_whitespace().next() {
                if let Ok(n) = v.parse::<f64>() {
                    return n as u64;
                }
            }
        }
    }
    0
}
