// =============================================================================
// Plik: examples/iroh_test_client.rs
// Opis: Maly klient do realnego testowania sidecara w kontenerze docker.
//       Laczy sie z podanym endpoint_id przez direct addr (host port mapped do
//       UDP 5000 w kontenerze), wysyla CompletionPayload i drukuje wynik.
//
//   cargo run -p tentaflow-transport --example iroh_test_client --release -- \
//     <ENDPOINT_ID_HEX> <DIRECT_ADDR> [MODEL] [PROMPT]
//
//   Przyklad:
//     cargo run -p tentaflow-transport --example iroh_test_client --release -- \
//       a1b2c3...64hex 127.0.0.1:8000 Qwen/Qwen2.5-0.5B-Instruct "Hello"
// =============================================================================

use std::net::SocketAddr;
use std::str::FromStr;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use iroh::EndpointId;
use tokio::sync::watch;
use tracing_subscriber::EnvFilter;

use tentaflow_protocol::{
    CompletionPayload, Message, ModelPayload, ModelRequest, ModelResult,
};
use tentaflow_transport::{build_client_endpoint, ServiceClient, ServiceClientConfig, ALPN_SERVICE};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,iroh=warn")))
        .init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "Usage: {} <ENDPOINT_ID_HEX> <DIRECT_ADDR> [MODEL] [PROMPT] [TIMEOUT_S]",
            args[0]
        );
        std::process::exit(2);
    }
    let endpoint_id_hex = &args[1];
    let direct_addr_str = &args[2];
    let model = args.get(3).cloned().unwrap_or_else(|| "default".to_string());
    let prompt = args
        .get(4)
        .cloned()
        .unwrap_or_else(|| "Say hello in one short sentence.".to_string());
    let timeout_s: u64 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(120);

    let endpoint_id = parse_endpoint_id(endpoint_id_hex)?;
    let direct_addr: SocketAddr = direct_addr_str
        .parse()
        .map_err(|e| anyhow!("zly direct_addr {}: {}", direct_addr_str, e))?;

    println!("[client] endpoint_id = {}", endpoint_id.fmt_short());
    println!("[client] direct_addr = {}", direct_addr);
    println!("[client] model       = {}", model);
    println!("[client] prompt      = {}", prompt);

    let endpoint = build_client_endpoint(vec![ALPN_SERVICE.to_vec()]).await?;

    let mut cfg = ServiceClientConfig::new("e2e-test", endpoint_id);
    cfg.alpn = ALPN_SERVICE.to_vec();
    cfg.request_timeout = Duration::from_secs(timeout_s);
    cfg.auto_reconnect = false;
    cfg.direct_addrs = vec![direct_addr];

    println!("[client] dial...");
    let (_tx, rx) = watch::channel(false);
    let dial_start = Instant::now();
    let client = ServiceClient::connect(endpoint.clone(), cfg, rx).await?;
    println!("[client] connected in {:?}", dial_start.elapsed());

    let payload = CompletionPayload {
        model: model.clone(),
        prompt: None,
        messages: vec![Message {
            role: "user".to_string(),
            content: prompt.clone(),
        }],
        temperature: Some(0.7),
        max_tokens: Some(64),
        top_p: Some(0.9),
        stop: None,
        presence_penalty: None,
        frequency_penalty: None,
        tts_options: None,
        memory_options: None,
        audio_input: None,
        prefix_cache_id: None,
        prefix_text: None,
    };
    let request = ModelRequest {
        request_id: format!("e2e-{}", uuid_like()),
        payload: ModelPayload::Completion(payload),
        stream: false,
        metadata: None,
        session_id: None,
    };

    println!("[client] send_request...");
    let send_start = Instant::now();
    let response = client.request(request).await?;
    println!("[client] response in {:?}", send_start.elapsed());

    match response.result {
        ModelResult::Completion(c) => {
            println!("[client] OK model={} finish_reason={:?}", c.model, c.finish_reason);
            println!("[client] >>> {}", c.text);
        }
        ModelResult::Error(e) => {
            eprintln!("[client] ERROR from upstream: {}", e.message);
            std::process::exit(1);
        }
        other => {
            eprintln!("[client] nieoczekiwany result: {:?}", other);
            std::process::exit(1);
        }
    }

    client.close().await;
    Ok(())
}

fn parse_endpoint_id(hex: &str) -> Result<EndpointId> {
    let bytes = hex::decode(hex.trim()).map_err(|e| anyhow!("zly hex endpoint_id: {}", e))?;
    if bytes.len() != 32 {
        return Err(anyhow!("endpoint_id hex musi mieć 64 znaki (32 bajty), jest {}", bytes.len()));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    EndpointId::from_str(&hex::encode(arr)).map_err(|e| anyhow!("EndpointId::from_str: {}", e))
}

fn uuid_like() -> String {
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}", nanos)
}
