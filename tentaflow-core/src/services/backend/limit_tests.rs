//! `max_concurrent` as a real in-flight limit, observed from the server side of
//! a fake HTTP backend: the server counts the requests it holds open at once.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, Semaphore};

use super::BackendClient;
use crate::api::openai::types::{ChatCompletionRequest, TTSRequest};
use crate::config::{ConnectionType, ServiceBackend};
use crate::error::CoreError;
use crate::services::limiter::{configure, limiter_for, ServiceLimiter};
use crate::services::runtime::{CircuitBreakerConfig, CircuitState};

/// No wait in these tests may take longer; a hang fails the test instead of
/// stalling the run.
const HANG: Duration = Duration::from_secs(10);

/// Server of one test. A request is "in flight" from the moment its body is read
/// until the test lets it answer.
struct FakeBackend {
    url: String,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
    arrived: Semaphore,
    arrivals: AtomicUsize,
    /// One permit lets one held request answer.
    answer: Semaphore,
    /// Streams end when this is notified (one waiter per stream).
    end_streams: Notify,
}

impl FakeBackend {
    async fn start() -> Arc<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend = Arc::new(Self {
            url: format!("http://{}", listener.local_addr().unwrap()),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
            arrived: Semaphore::new(0),
            arrivals: AtomicUsize::new(0),
            answer: Semaphore::new(0),
            end_streams: Notify::new(),
        });
        let server = backend.clone();
        tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(server.clone().serve(socket));
            }
        });
        backend
    }

    async fn serve(self: Arc<Self>, mut socket: TcpStream) {
        let mut buf = Vec::new();
        let body_start = loop {
            let mut chunk = [0u8; 4096];
            match socket.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
        };
        let head = String::from_utf8_lossy(&buf[..body_start]).to_ascii_lowercase();
        let length: usize = head
            .lines()
            .find_map(|l| l.strip_prefix("content-length:"))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);
        while buf.len() < body_start + length {
            let mut chunk = [0u8; 4096];
            match socket.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        let streaming = String::from_utf8_lossy(&buf[body_start..]).contains("\"stream\":true");

        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(now, Ordering::SeqCst);
        self.arrivals.fetch_add(1, Ordering::SeqCst);
        self.arrived.add_permits(1);

        if streaming {
            let _ = socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
                )
                .await;
            self.end_streams.notified().await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            let _ = socket.write_all(b"data: [DONE]\n\n").await;
        } else {
            self.answer.acquire().await.unwrap().forget();
            // Counted out before the answer is written: the client frees its
            // slot only after it has read the answer.
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc")
                .await;
        }
        let _ = socket.shutdown().await;
    }

    /// Waits until `n` more requests reached the server.
    async fn wait_for_arrivals(&self, n: u32) {
        tokio::time::timeout(HANG, self.arrived.acquire_many(n))
            .await
            .expect("the server never saw the request")
            .unwrap()
            .forget();
    }

    fn let_answer(&self, n: usize) {
        self.answer.add_permits(n);
    }
}

fn client(backend: &FakeBackend, limiter: Arc<ServiceLimiter>) -> Arc<BackendClient> {
    client_with_breaker(backend, limiter, None)
}

fn client_with_breaker(
    backend: &FakeBackend,
    limiter: Arc<ServiceLimiter>,
    breaker: Option<CircuitBreakerConfig>,
) -> Arc<BackendClient> {
    let config = ServiceBackend {
        connection: ConnectionType::OpenAIApi {
            url: backend.url.clone(),
            api_key: Some("test".into()),
            api_key_env: None,
            extra_headers: Vec::new(),
            custom_endpoint: None,
            request_format: None,
            tts_config: None,
        },
        // Deliberately not the limit: it only sizes the idle pool.
        max_concurrent: 64,
        timeout_ms: 30_000,
        weight: 1,
        model_name_override: None,
        health_check_path: None,
    };
    Arc::new(
        BackendClient::new(config, breaker)
            .expect("client")
            .with_limiter(limiter),
    )
}

fn speech() -> TTSRequest {
    TTSRequest {
        model: "m".into(),
        input: "hello".into(),
        voice: "alloy".into(),
        response_format: None,
        speed: None,
        language: None,
    }
}

async fn call(client: Arc<BackendClient>) -> crate::error::Result<Vec<u8>> {
    client.audio_speech(&speech()).await
}

fn is_busy(result: &crate::error::Result<Vec<u8>>) -> bool {
    matches!(
        result
            .as_ref()
            .err()
            .and_then(|e| e.downcast_ref::<CoreError>()),
        Some(CoreError::BackendBusy { .. })
    )
}

fn chat_request() -> ChatCompletionRequest {
    serde_json::from_value(serde_json::json!({
        "model": "m",
        "messages": [{"role": "user", "content": "hi"}],
    }))
    .expect("chat request")
}

async fn done<T>(task: tokio::task::JoinHandle<T>) -> T {
    tokio::time::timeout(HANG, task)
        .await
        .expect("task hung")
        .expect("task panicked")
}

/// Polls `condition` (yielding between polls) until it holds.
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(HANG, async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the condition never held");
}

/// A negative ("nothing else arrived") can only be observed over a window.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(150)).await;
}

#[tokio::test]
async fn five_concurrent_requests_never_exceed_two_in_flight() {
    let backend = FakeBackend::start().await;
    let limiter = ServiceLimiter::with_limit("t", 2, Duration::from_secs(20));
    let client = client(&backend, limiter.clone());

    let calls: Vec<_> = (0..5).map(|_| tokio::spawn(call(client.clone()))).collect();

    backend.wait_for_arrivals(2).await;
    // Two are in flight, two wait (the queue is as long as the limit) and the
    // fifth is refused at once.
    until(|| limiter.stats().waiting == 2 && limiter.stats().rejected == 1).await;
    settle().await;
    assert_eq!(backend.in_flight.load(Ordering::SeqCst), 2);
    assert_eq!(
        backend.arrivals.load(Ordering::SeqCst),
        2,
        "the waiting ones are not sent"
    );

    backend.let_answer(5);
    let mut served = 0;
    let mut refused = 0;
    for call in calls {
        match done(call).await {
            Ok(body) => {
                assert_eq!(body, b"abc");
                served += 1;
            }
            Err(e) => {
                assert!(matches!(
                    e.downcast_ref::<CoreError>(),
                    Some(CoreError::BackendBusy { .. })
                ));
                refused += 1;
            }
        }
    }
    assert_eq!((served, refused), (4, 1));
    assert_eq!(backend.arrivals.load(Ordering::SeqCst), 4);
    assert_eq!(backend.max_in_flight.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_request_waits_for_the_timeout_then_gets_a_typed_busy_error() {
    let backend = FakeBackend::start().await;
    let limiter = ServiceLimiter::with_limit("t", 1, Duration::from_millis(300));
    let client = client(&backend, limiter);

    let held = tokio::spawn(call(client.clone()));
    backend.wait_for_arrivals(1).await;

    let started = Instant::now();
    let refused = call(client.clone()).await;
    assert!(is_busy(&refused), "got {refused:?}");
    assert!(
        started.elapsed() >= Duration::from_millis(300),
        "waited only {:?}",
        started.elapsed()
    );
    assert_eq!(backend.arrivals.load(Ordering::SeqCst), 1, "never sent");

    backend.let_answer(2);
    done(held).await.expect("the held request is served");
    call(client).await.expect("a freed slot admits again");
}

#[tokio::test]
async fn recreating_the_client_does_not_reset_the_limit() {
    let backend = FakeBackend::start().await;
    let node = "limit-test-recreate";
    configure(node, 1, Some(1), Duration::from_millis(100));

    let first = client(&backend, limiter_for(node, 1));
    let held = tokio::spawn(call(first));
    backend.wait_for_arrivals(1).await;

    // What a handle rebuild does: a new client, the limiter fetched from the map.
    let rebuilt = client(&backend, limiter_for(node, 1));
    let refused = call(rebuilt.clone()).await;
    assert!(is_busy(&refused), "the slot of the old client still counts");

    backend.let_answer(2);
    done(held).await.expect("served");
    call(rebuilt).await.expect("served once the slot is free");
    assert_eq!(backend.max_in_flight.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn resizing_in_place_neither_cuts_calls_short_nor_over_admits() {
    let backend = FakeBackend::start().await;
    let node = "limit-test-resize";
    configure(node, 1, Some(3), Duration::from_millis(150));
    let limiter = limiter_for(node, 1);
    let client = client(&backend, limiter.clone());

    let in_flight: Vec<_> = (0..3).map(|_| tokio::spawn(call(client.clone()))).collect();
    backend.wait_for_arrivals(3).await;

    // Shrink under load: the three keep running and nothing new is admitted.
    configure(node, 1, Some(1), Duration::from_millis(150));
    assert!(is_busy(&call(client.clone()).await));
    assert_eq!(backend.in_flight.load(Ordering::SeqCst), 3);

    // Two end: one is still in flight, which is the whole new limit.
    backend.let_answer(2);
    until(|| backend.in_flight.load(Ordering::SeqCst) == 1).await;
    until(|| limiter.in_flight() == 1).await;
    assert!(is_busy(&call(client.clone()).await), "1 in flight, limit 1");

    // The last one ends and the slot is free again.
    backend.let_answer(1);
    for call in in_flight {
        done(call).await.expect("no call was cut short");
    }
    until(|| limiter.in_flight() == 0).await;
    let before = backend.arrivals.load(Ordering::SeqCst);

    // The long wait is configured BEFORE the probes start, or they would give
    // up on the old 150 ms while the test is still looking at them.
    configure(node, 1, Some(1), HANG);
    let probes: Vec<_> = (0..2).map(|_| tokio::spawn(call(client.clone()))).collect();
    backend.wait_for_arrivals(1).await;
    until(|| limiter.stats().waiting == 1).await;
    assert_eq!(backend.arrivals.load(Ordering::SeqCst), before + 1);
    assert_eq!(backend.in_flight.load(Ordering::SeqCst), 1, "limit 1");

    // Grow: the waiting probe is let in by the new slots.
    configure(node, 1, Some(3), HANG);
    backend.wait_for_arrivals(1).await;
    assert_eq!(backend.in_flight.load(Ordering::SeqCst), 2);
    backend.let_answer(2);
    for probe in probes {
        done(probe).await.expect("served");
    }
}

#[tokio::test]
async fn a_streaming_request_holds_its_slot_until_the_stream_ends() {
    let backend = FakeBackend::start().await;
    let limiter = ServiceLimiter::with_limit("t", 1, Duration::from_millis(150));
    let client = client(&backend, limiter);

    let mut stream = client
        .chat_completion_stream(chat_request())
        .await
        .expect("stream starts once the headers are in");
    backend.wait_for_arrivals(1).await;

    // The headers are in and the call returned, but the generation is going on.
    assert!(is_busy(&call(client.clone()).await), "slot held mid-stream");
    assert_eq!(backend.arrivals.load(Ordering::SeqCst), 1);

    backend.end_streams.notify_one();
    tokio::time::timeout(HANG, async { while stream.next().await.is_some() {} })
        .await
        .expect("the stream never ended");

    backend.let_answer(1);
    call(client)
        .await
        .expect("slot returned at the end of the stream");
}

#[tokio::test]
async fn a_dropped_stream_returns_its_slot() {
    let backend = FakeBackend::start().await;
    let limiter = ServiceLimiter::with_limit("t", 1, Duration::from_millis(150));
    let client = client(&backend, limiter);

    let stream = client
        .chat_completion_stream(chat_request())
        .await
        .expect("stream");
    backend.wait_for_arrivals(1).await;
    assert!(is_busy(&call(client.clone()).await));

    drop(stream);
    backend.let_answer(1);
    call(client).await.expect("slot returned on drop");
}

#[tokio::test]
async fn every_http_surface_takes_a_slot() {
    let backend = FakeBackend::start().await;
    let limiter = ServiceLimiter::with_limit("t", 1, Duration::from_millis(100));
    let client = client(&backend, limiter);

    let held = tokio::spawn(call(client.clone()));
    backend.wait_for_arrivals(1).await;

    let busy = |e: anyhow::Error| {
        matches!(
            e.downcast_ref::<CoreError>(),
            Some(CoreError::BackendBusy { .. })
        )
    };
    assert!(busy(
        client.chat_completion(chat_request()).await.unwrap_err()
    ));
    assert!(busy(
        client
            .embeddings_request(
                serde_json::from_value(serde_json::json!({"model": "m", "input": "x"})).unwrap()
            )
            .await
            .unwrap_err()
    ));
    assert!(busy(
        client
            .rerank_request(
                serde_json::from_value(
                    serde_json::json!({"model": "m", "query": "q", "documents": ["d"]})
                )
                .unwrap()
            )
            .await
            .unwrap_err()
    ));
    assert!(busy(
        client
            .parse_document("m", b"img", "image/png")
            .await
            .unwrap_err()
    ));
    assert!(busy(
        client
            .document_infer("m", b"img", "ocr", "image/png")
            .await
            .unwrap_err()
    ));
    assert!(busy(
        client
            .vision("m".into(), Vec::new(), None)
            .await
            .unwrap_err()
    ));
    assert_eq!(backend.arrivals.load(Ordering::SeqCst), 1, "none was sent");

    backend.let_answer(1);
    done(held).await.expect("served");
}

#[tokio::test]
async fn a_busy_timeout_does_not_use_up_the_half_open_probe() {
    let backend = FakeBackend::start().await;
    let limiter = ServiceLimiter::with_limit("t", 1, Duration::from_millis(100));
    let client = client_with_breaker(
        &backend,
        limiter.clone(),
        Some(CircuitBreakerConfig {
            threshold: 1,
            timeout_ms: 20,
            half_open_max_calls: 1,
        }),
    );

    // Open the circuit, let its timeout pass, and move it to half-open.
    let breaker = client.circuit_breaker();
    breaker.record_failure();
    tokio::time::sleep(Duration::from_millis(60)).await;
    drop(breaker.admit().expect("the timeout has passed"));
    assert_eq!(breaker.state(), CircuitState::HalfOpen);

    // A call that gives up waiting for a slot never reached the breaker.
    let held = limiter.acquire().await.unwrap();
    assert!(is_busy(&call(client.clone()).await));
    drop(held);

    // So the next call is still allowed to probe, and closes the circuit.
    backend.let_answer(1);
    call(client.clone())
        .await
        .expect("the probe was not used up by the busy call");
    assert_eq!(breaker.state(), CircuitState::Closed);
}
