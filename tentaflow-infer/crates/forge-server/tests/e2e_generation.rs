// ===== File: e2e_generation.rs — GPU integration test: generation API completeness (SPEC §8.1.2) =====
// Ignored by default: needs a CUDA GPU and the local Qwen3-0.6B Q8_0 GGUF at
// test-models/gguf/qwen3-0.6b-q8_0.gguf. Proves the OpenAI generation-API
// features: logit_bias (force / ban), min_tokens, echo, logprobs/top_logprobs
// shape + top-1==sampled at temperature 0, and n (multiple deterministic
// completions). Run:
//   cargo test -p forge-server --release --test e2e_generation -- --ignored --nocapture

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use forge_engine::model::ModelConfig;
use forge_engine::server::spawn_engine;
use forge_hal::Device;
use forge_hal::{gpu, PoolSizes};
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
async fn generation_api_end_to_end() {
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
            let device = gpu::open(
                0,
                PoolSizes {
                    weights: 3 << 30,
                    kv_cache: kv_pool_bytes(
                        &desc,
                        kv_page_size,
                        kv_pages,
                        forge_engine::kv::KvQuant::F16,
                        false,
                    )
                    .unwrap(),
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
                    weight_spill_dir: None,
                    weight_host_budget: 0,
                    kv_page_size,
                    kv_pages,
                    max_seq_len: 4096,
                    kv_quant: forge_engine::kv::KvQuant::F16,
                    kv_tier: Default::default(),
                    prefix_cache: true,
                    layer_range: None,
                    tp_shard: forge_formats::TpShard { rank: 0, world: 1 },
                    native_mtp: false,
                    nvfp4_gguf_layout: forge_engine::model::Nvfp4GgufLayout::RowMajor36,
                    nvfp4_ct_layout: forge_engine::weights::NvFp4CtLayoutPolicy::Auto,
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
            let handle = spawn_engine(loaded.model, tokenizer.clone(), 4, 16)
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
        default_sampling: Default::default(),
        default_stop: vec![],
    };
    let state = ServerState::new(
        &cfg,
        handle,
        tokenizer.clone(),
        template_vars,
        eos_ids,
        chat_template,
        4096,
        4,
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
    let chat_url = format!("http://{addr}/v1/chat/completions");
    let comp_url = format!("http://{addr}/v1/completions");

    // ---- (1) logit_bias: force and ban a token ----
    // Greedy baseline, then bias a specific token id up (+100 force) / down
    // (-100 ban) and confirm the first generated token changes as expected.
    let base = serde_json::json!({
        "model": "qwen3-0.6b",
        "prompt": "The capital of France is",
        "temperature": 0.0,
        "max_tokens": 1,
        "logprobs": 5
    });
    let (status, body) = post(&client, &comp_url, base.clone()).await;
    assert_eq!(status, 200, "baseline failed: {body}");
    let baseline_tok = body["choices"][0]["logprobs"]["tokens"][0]
        .as_str()
        .unwrap()
        .to_string();
    // A distinct token id from the top alternatives to force.
    let alt_id = tokenizer
        .token_to_id(" London")
        .or_else(|| tokenizer.token_to_id("London"))
        .expect("has a London token");
    let force = serde_json::json!({
        "model": "qwen3-0.6b",
        "prompt": "The capital of France is",
        "temperature": 0.0,
        "max_tokens": 1,
        "logit_bias": { alt_id.to_string(): 100 }
    });
    let (status, body) = post(&client, &comp_url, force).await;
    assert_eq!(status, 200, "forced request failed: {body}");
    let forced_text = body["choices"][0]["text"].as_str().unwrap().to_string();
    println!("[logit_bias] baseline={baseline_tok:?} forced(+100 on {alt_id})={forced_text:?}");
    assert!(
        forced_text.to_lowercase().contains("london"),
        "forcing token {alt_id} should surface 'London', got {forced_text:?}"
    );

    // Ban the baseline token: the greedy pick must change. Re-encode the
    // decoded piece to recover its id (byte-level pieces don't round-trip
    // through `token_to_id`).
    let baseline_id = *tokenizer
        .encode(&baseline_tok, false)
        .expect("encode baseline piece")
        .first()
        .expect("baseline piece has at least one token");
    let ban = serde_json::json!({
        "model": "qwen3-0.6b",
        "prompt": "The capital of France is",
        "temperature": 0.0,
        "max_tokens": 1,
        "logit_bias": { baseline_id.to_string(): -100 }
    });
    let (status, body) = post(&client, &comp_url, ban).await;
    assert_eq!(status, 200, "banned request failed: {body}");
    let banned_text = body["choices"][0]["text"].as_str().unwrap().to_string();
    println!("[logit_bias] banned {baseline_id} -> {banned_text:?}");
    assert_ne!(
        banned_text, baseline_tok,
        "banning the baseline token must change the output"
    );

    // ---- (2) min_tokens: no early EOS ----
    // A prompt the model would normally answer in a few tokens; min_tokens 20
    // must force at least 20 completion tokens.
    let (status, body) = post(
        &client,
        &chat_url,
        serde_json::json!({
            "model": "qwen3-0.6b",
            "messages": [{"role": "user", "content": "Reply with just: OK"}],
            "temperature": 0.0,
            "max_tokens": 128,
            "min_tokens": 20
        }),
    )
    .await;
    assert_eq!(status, 200, "min_tokens request failed: {body}");
    let produced = body["usage"]["completion_tokens"].as_u64().unwrap();
    println!("[min_tokens] produced {produced} tokens (floor 20)");
    assert!(produced >= 20, "min_tokens 20 not honored: {produced}");

    // ---- (3) echo (completions) ----
    let prompt = "Once upon a time";
    let (status, body) = post(
        &client,
        &comp_url,
        serde_json::json!({
            "model": "qwen3-0.6b",
            "prompt": prompt,
            "temperature": 0.0,
            "max_tokens": 8,
            "echo": true
        }),
    )
    .await;
    assert_eq!(status, 200, "echo request failed: {body}");
    let echoed = body["choices"][0]["text"].as_str().unwrap();
    println!("[echo] -> {echoed:?}");
    assert!(
        echoed.starts_with(prompt),
        "echo must prepend the prompt, got {echoed:?}"
    );

    // ---- (4) logprobs / top_logprobs (chat) ----
    // Greedy: the sampled token must be the top-1 logprob token; all values <= 0
    // and each position's top-N exp-sum <= 1 (they are a prefix of the softmax).
    let (status, body) = post(
        &client,
        &chat_url,
        serde_json::json!({
            "model": "qwen3-0.6b",
            "messages": [{"role": "user", "content": "Say a single word."}],
            "temperature": 0.0,
            "max_tokens": 8,
            "logprobs": true,
            "top_logprobs": 5
        }),
    )
    .await;
    assert_eq!(status, 200, "logprobs request failed: {body}");
    let content = body["choices"][0]["logprobs"]["content"]
        .as_array()
        .expect("logprobs.content is an array");
    assert!(!content.is_empty(), "no logprob entries returned");
    for entry in content {
        let lp = entry["logprob"].as_f64().unwrap();
        assert!(lp <= 1e-4, "token logprob must be <= 0, got {lp}");
        let top = entry["top_logprobs"].as_array().unwrap();
        assert_eq!(top.len(), 5, "top_logprobs must have N=5 entries");
        // Ordered most-probable first.
        let mut prev = f64::INFINITY;
        let mut sum_exp = 0.0f64;
        for t in top {
            let v = t["logprob"].as_f64().unwrap();
            assert!(v <= prev + 1e-6, "top_logprobs not descending");
            prev = v;
            sum_exp += v.exp();
        }
        assert!(
            sum_exp <= 1.0 + 1e-3,
            "top-N prob mass exceeds 1: {sum_exp}"
        );
        // Greedy: the sampled token is the top-1 alternative.
        assert_eq!(
            entry["token"].as_str().unwrap(),
            top[0]["token"].as_str().unwrap(),
            "at temp 0 the sampled token must be the top-1 logprob token"
        );
        assert!(
            (entry["logprob"].as_f64().unwrap() - top[0]["logprob"].as_f64().unwrap()).abs() < 1e-6
        );
    }
    println!("[logprobs] {} well-formed entries", content.len());

    // ---- (5) n=3: three deterministic completions ----
    // Raw completions endpoint (no thinking template) so the sampled text is
    // surfaced directly.
    let n_body = serde_json::json!({
        "model": "qwen3-0.6b",
        "prompt": "Three fantasy place names:\n1.",
        "temperature": 0.9,
        "max_tokens": 16,
        "n": 3,
        "seed": 12345
    });
    let (status, body) = post(&client, &comp_url, n_body.clone()).await;
    assert_eq!(status, 200, "n=3 request failed: {body}");
    let choices = body["choices"].as_array().unwrap();
    assert_eq!(choices.len(), 3, "n=3 must return 3 choices");
    for (i, c) in choices.iter().enumerate() {
        assert_eq!(c["index"].as_u64().unwrap(), i as u64);
    }
    let texts: Vec<String> = choices
        .iter()
        .map(|c| c["text"].as_str().unwrap_or("").to_string())
        .collect();
    println!("[n=3] {texts:#?}");
    // Distinct seeds per completion → the three differ (temperature 0.9).
    assert!(
        texts[0] != texts[1] || texts[1] != texts[2],
        "n completions should not all be identical"
    );

    // Deterministic: repeating the same seeded request reproduces the choices.
    let (status, body2) = post(&client, &comp_url, n_body).await;
    assert_eq!(status, 200);
    let texts2: Vec<String> = body2["choices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["text"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(
        texts, texts2,
        "seeded n-way generation must be deterministic"
    );
    println!("[n=3] deterministic across runs");

    // Streaming with n>1 is rejected.
    let (status, _) = post(
        &client,
        &chat_url,
        serde_json::json!({
            "model": "qwen3-0.6b",
            "messages": [{"role": "user", "content": "hi"}],
            "n": 2, "stream": true
        }),
    )
    .await;
    assert_eq!(status, 400, "streaming n>1 must be a 400");

    println!("generation API end-to-end: all features proven");
}
