// =============================================================================
// File: benches/streaming_pickup_perf.rs — M1.W7 perf acceptance benches
// =============================================================================
//
// Criterion benches that drive the hot paths used by `service_call_v1` and the
// Service-to-Core pickup API. Targets from `notes/tentavision-plan.md` §17.8:
//
//   * stream_next poll latency (buffer non-empty)  : < 1 ms p99
//   * PickupToken issuance (HMAC)                  : < 1 ms p99
//   * Frame pickup roundtrip (token+LRU+audit)     : < 20 ms p99
//   * service_call overhead (no inference)         : < 5 ms p99
//
// We cover the underlying primitives. The service_call_overhead target is
// approximated from BELOW by `pickup_core_model` (mint + verify + consume +
// LRU remove + audit row) — this is the pickup segment of `service_call_v1`,
// NOT the full call. Router lookup, rate limit, QUIC dispatch, and the
// `alias_calls` audit write live outside this micro-bench. The
// `pickup_roundtrip` group adds the metadata-formatting cost the in-process
// `handle_pickup` does. A dedicated end-to-end `service_call_v1_overhead`
// bench (router fixtures + mock dispatch + audit) is queued for M3.
//
// Run: `cargo bench --bench streaming_pickup_perf` (camera feature optional —
// these benches are camera-free).

use std::sync::Arc;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use std::hint::black_box;

use tentaflow_core::api::frame_pickup::{handle_pickup, PickupOutcome, PickupRequest};
use tentaflow_core::db::DbPool;
use tentaflow_core::services::frame_storage::{
    FrameMetadata, FramePixelFormat, FrameStorage, StoredFrame,
};
use tentaflow_core::services::pickup_tokens::PickupTokenIssuer;
use tentaflow_core::services::streaming::{StreamFilter, StreamingBus};

// -----------------------------------------------------------------------------
// Fixtures
// -----------------------------------------------------------------------------

fn make_db() -> DbPool {
    tentaflow_core::db::init(std::path::Path::new(":memory:")).expect("db init")
}

fn rgb24_frame(width: u32, height: u32) -> StoredFrame {
    let payload = vec![0xCDu8; (width * height * 3) as usize];
    StoredFrame {
        metadata: FrameMetadata {
            camera_id: "bench-cam".into(),
            width,
            height,
            pixel_format: FramePixelFormat::Rgb24,
            timestamp_unix_ms: 1_715_500_000_000,
            pts: Some(7),
            frame_size_bytes: payload.len(),
        },
        data: Arc::from(payload.into_boxed_slice()),
        created_at: std::time::Instant::now(),
    }
}

fn issuer() -> PickupTokenIssuer {
    // Skip the background sweeper so the bench harness does not require a
    // running tokio runtime.
    PickupTokenIssuer::new_for_tests([0xA5u8; 32], Duration::from_secs(30))
}

// -----------------------------------------------------------------------------
// 1. PickupToken issuance (HMAC) — target < 1 ms p99
// -----------------------------------------------------------------------------

fn bench_pickup_token_issue(c: &mut Criterion) {
    let iss = issuer();
    let mut group = c.benchmark_group("pickup_token");
    group.bench_function("issue", |b| {
        let mut counter = 0u64;
        b.iter(|| {
            counter = counter.wrapping_add(1);
            let (tok, _) = iss.issue(
                black_box(format!("frame_{}", counter)),
                black_box("yolo-svc".to_string()),
                black_box(format!("req_{}", counter)),
            );
            black_box(tok);
        });
    });
    group.finish();
}

// -----------------------------------------------------------------------------
// 2. verify_only + consume_one_shot — building blocks of the pickup path.
// -----------------------------------------------------------------------------

fn bench_pickup_token_verify_only(c: &mut Criterion) {
    let iss = issuer();
    // Pre-issue a batch so verify_only has a real entry to find.
    let mut wires = Vec::with_capacity(4096);
    for i in 0..4096 {
        let (tok, _) = iss.issue(format!("frame_{i}"), "svc".into(), format!("req_{i}"));
        wires.push(tok.wire());
    }
    let mut idx = 0usize;
    c.bench_function("pickup_token_verify_only", |b| {
        b.iter(|| {
            idx = (idx + 1) % wires.len();
            let r = iss.verify_only(black_box(&wires[idx]));
            black_box(r.is_ok());
        });
    });
}

fn bench_pickup_token_consume(c: &mut Criterion) {
    let iss = issuer();
    let mut group = c.benchmark_group("pickup_token");
    group.bench_function("consume_one_shot", |b| {
        b.iter_batched(
            || {
                let (tok, _) = iss.issue("frame_x".into(), "svc".into(), "req_x".into());
                tok.wire()
            },
            |wire| {
                let r = iss.consume_one_shot(black_box(&wire));
                black_box(r.is_ok());
            },
            criterion::BatchSize::SmallInput,
        );
    });
    group.finish();
}

// -----------------------------------------------------------------------------
// 3. Frame storage micro-benches — insert, get, remove.
// -----------------------------------------------------------------------------

fn bench_frame_storage(c: &mut Criterion) {
    let mut group = c.benchmark_group("frame_storage");
    for (w, h) in [(320u32, 240u32), (1280, 720)] {
        let label = format!("{}x{}", w, h);
        let frame = rgb24_frame(w, h);
        group.bench_with_input(BenchmarkId::new("insert", &label), &frame, |b, frame| {
            let storage = FrameStorage::new(2048);
            b.iter(|| {
                let r = storage.insert(black_box(frame.clone()));
                black_box(r);
            });
        });
        group.bench_with_input(BenchmarkId::new("get", &label), &frame, |b, frame| {
            let storage = FrameStorage::new(2048);
            let mut refs = Vec::with_capacity(1024);
            for _ in 0..1024 {
                refs.push(storage.insert(frame.clone()));
            }
            let mut idx = 0usize;
            b.iter(|| {
                idx = (idx + 1) % refs.len();
                let g = storage.get(black_box(&refs[idx]));
                black_box(g.is_some());
            });
        });
    }
    group.finish();
}

// -----------------------------------------------------------------------------
// 4. Streaming bus — broadcast + subscriber poll on a hot buffer.
// -----------------------------------------------------------------------------

fn bench_streaming_bus(c: &mut Criterion) {
    let mut group = c.benchmark_group("streaming_bus");

    let bus = StreamingBus::new();
    let sub = bus.subscribe("bench-cam", StreamFilter::default());
    let _keep = sub; // keep the channel alive for the broadcast bench
    let meta = rgb24_frame(320, 240).metadata;
    group.bench_function("broadcast_no_drop", |b| {
        // Use a fresh bus per-call so we never trip backpressure.
        b.iter_batched(
            || {
                let bus = StreamingBus::new();
                let s = bus.subscribe_with_capacity("c", StreamFilter::default(), 1024);
                (bus, s)
            },
            |(bus, sub)| {
                bus.broadcast(
                    black_box("c"),
                    tentaflow_core::services::frame_storage::RawFrameRef::new(),
                    black_box(meta.clone()),
                );
                black_box(sub.dropped_pending());
            },
            criterion::BatchSize::SmallInput,
        );
    });

    // stream_next poll latency target: < 1 ms p99 with buffer non-empty.
    // We pre-fill the channel with N frames per batch and poll once; the
    // per-iteration cost is one `try_recv` round-trip (criterion-driven).
    group.bench_function("stream_next_hot_buffer", |b| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        b.iter_custom(|iters| {
            rt.block_on(async {
                let bus = StreamingBus::new();
                let mut sub = bus.subscribe_with_capacity(
                    "c",
                    StreamFilter::default(),
                    (iters as usize).max(16),
                );
                for _ in 0..iters {
                    bus.broadcast(
                        "c",
                        tentaflow_core::services::frame_storage::RawFrameRef::new(),
                        meta.clone(),
                    );
                }
                let start = std::time::Instant::now();
                for _ in 0..iters {
                    let _ = sub.next(Duration::from_millis(50)).await;
                }
                start.elapsed()
            })
        });
    });
    group.finish();
}

// -----------------------------------------------------------------------------
// 5a. Pickup handler direct (in-process pure-function path) — baseline.
//     Skips the Bytes::copy_from_slice the production handler does when it
//     builds the HTTP response body, so this is a LOWER BOUND on the real
//     roundtrip cost. Group 5b below measures the full hyper/reqwest path.
// -----------------------------------------------------------------------------

fn bench_pickup_handler_direct(c: &mut Criterion) {
    let mut group = c.benchmark_group("pickup_handler_direct");
    for (w, h) in [(320u32, 240u32), (1280, 720)] {
        let label = format!("{}x{}", w, h);
        let template = rgb24_frame(w, h);
        group.bench_function(&label, |b| {
            // Fresh issuer + storage + db per call so consume_one_shot has
            // something live to remove. The audit row write hits in-memory
            // SQLite — that is the realistic production cost.
            b.iter_batched(
                || {
                    let iss = issuer();
                    let storage = FrameStorage::new(8);
                    let db = make_db();
                    let raw_ref = storage.insert(template.clone());
                    let (tok, _) = iss.issue(
                        raw_ref.as_str().to_string(),
                        "yolo-svc".into(),
                        "req-bench".into(),
                    );
                    (iss, storage, db, raw_ref, tok.wire())
                },
                |(iss, storage, db, raw_ref, wire)| {
                    let pr = PickupRequest {
                        pickup_token: Some(&wire),
                        frame_ref: Some(raw_ref.as_str()),
                        service_id: Some("yolo-svc"),
                        request_id: Some("req-bench"),
                    };
                    let outcome = handle_pickup(pr, &iss, &storage, &db);
                    debug_assert!(matches!(outcome, PickupOutcome::Ok { .. }));
                    black_box(outcome);
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

// -----------------------------------------------------------------------------
// 5b. Pickup HTTP roundtrip — full wire path through a real hyper server and
//     a reqwest client over loopback. This exercises Bytes::copy_from_slice
//     in the response builder + TCP loopback + serde header parsing — i.e.
//     what production actually pays per service-to-core pickup.
// -----------------------------------------------------------------------------

mod http_bench {
    use super::*;
    use http_body_util::{BodyExt, Full};
    use hyper::body::Bytes;
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use hyper::{Method, Request, Response, StatusCode};
    use hyper_util::rt::TokioIo;
    use std::convert::Infallible;
    use std::net::SocketAddr;
    use tentaflow_core::api::frame_pickup::{
        HDR_FRAME_HEIGHT, HDR_FRAME_PIXEL_FORMAT, HDR_FRAME_PTS, HDR_FRAME_REF, HDR_FRAME_TS_MS,
        HDR_FRAME_WIDTH, HDR_PICKUP_TOKEN, HDR_REQUEST_ID, HDR_SERVICE_ID,
    };
    use tokio::net::TcpListener;

    pub struct Server {
        pub addr: SocketAddr,
        pub issuer: Arc<PickupTokenIssuer>,
        pub storage: Arc<FrameStorage>,
        // Held to keep the in-memory SQLite alive for the spawned server's
        // handler closure; the driver bench function never reads it back.
        #[allow(dead_code)]
        pub db: DbPool,
    }

    pub async fn spawn() -> Server {
        let issuer = Arc::new(issuer());
        let storage = Arc::new(FrameStorage::new(4096));
        let db = make_db();
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let i = issuer.clone();
        let s = storage.clone();
        let d = db.clone();
        tokio::spawn(async move {
            loop {
                let (sock, _) = match listener.accept().await {
                    Ok(x) => x,
                    Err(_) => continue,
                };
                let i = i.clone();
                let s = s.clone();
                let d = d.clone();
                tokio::spawn(async move {
                    let svc = service_fn(move |req| {
                        let i = i.clone();
                        let s = s.clone();
                        let d = d.clone();
                        async move {
                            Ok::<_, Infallible>(handler(req, &i, &s, &d).await)
                        }
                    });
                    let _ = http1::Builder::new()
                        .serve_connection(TokioIo::new(sock), svc)
                        .await;
                });
            }
        });
        Server {
            addr,
            issuer,
            storage,
            db,
        }
    }

    async fn handler(
        req: Request<hyper::body::Incoming>,
        issuer: &PickupTokenIssuer,
        storage: &FrameStorage,
        db: &DbPool,
    ) -> Response<Full<Bytes>> {
        if req.method() != Method::POST || req.uri().path() != "/core/frame/pickup" {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Full::new(Bytes::new()))
                .unwrap();
        }
        let hdr = |n: &str| {
            req.headers()
                .get(n)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        };
        let token = hdr(HDR_PICKUP_TOKEN);
        let frame_ref = hdr(HDR_FRAME_REF);
        let service_id = hdr(HDR_SERVICE_ID);
        let request_id = hdr(HDR_REQUEST_ID);
        let _ = req.into_body().collect().await.ok();
        let pr = PickupRequest {
            pickup_token: token.as_deref(),
            frame_ref: frame_ref.as_deref(),
            service_id: service_id.as_deref(),
            request_id: request_id.as_deref(),
        };
        let outcome = handle_pickup(pr, issuer, storage, db);
        let status = outcome.http_status();
        match outcome {
            PickupOutcome::Ok {
                bytes,
                width,
                height,
                pixel_format,
                timestamp_unix_ms,
                pts,
            } => {
                let mut b = Response::builder()
                    .status(status)
                    .header("Content-Type", "application/octet-stream")
                    .header(HDR_FRAME_WIDTH, width.to_string())
                    .header(HDR_FRAME_HEIGHT, height.to_string())
                    .header(HDR_FRAME_PIXEL_FORMAT, pixel_format)
                    .header(HDR_FRAME_TS_MS, timestamp_unix_ms.to_string());
                if let Some(p) = pts {
                    b = b.header(HDR_FRAME_PTS, p.to_string());
                }
                b.body(Full::new(Bytes::copy_from_slice(&bytes))).unwrap()
            }
            _ => Response::builder()
                .status(status)
                .body(Full::new(Bytes::new()))
                .unwrap(),
        }
    }
}

fn bench_pickup_http_roundtrip(c: &mut Criterion) {
    use tentaflow_core::api::frame_pickup::{
        HDR_FRAME_REF, HDR_PICKUP_TOKEN, HDR_REQUEST_ID, HDR_SERVICE_ID,
    };

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .expect("rt");
    // One hyper server + one reqwest client shared across all sizes — keeps
    // the per-iteration cost focused on the request, not server boot.
    let server = rt.block_on(http_bench::spawn());
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(8)
        .build()
        .expect("client");
    let url = format!("http://{}/core/frame/pickup", server.addr);

    let mut group = c.benchmark_group("pickup_http_roundtrip");
    for (w, h) in [(320u32, 240u32), (1280, 720)] {
        let label = format!("{}x{}", w, h);
        let template = rgb24_frame(w, h);
        group.bench_function(&label, |b| {
            b.iter_custom(|iters| {
                rt.block_on(async {
                    // Pre-issue `iters` distinct (token, raw_ref) pairs so each
                    // iteration consumes a fresh one — consume_one_shot would
                    // otherwise reject after the first call.
                    let mut prepared = Vec::with_capacity(iters as usize);
                    for i in 0..iters {
                        let raw_ref = server.storage.insert(template.clone());
                        let req_id = format!("req-bench-{}-{}", w, i);
                        let (tok, _) = server.issuer.issue(
                            raw_ref.as_str().to_string(),
                            "yolo-svc".into(),
                            req_id.clone(),
                        );
                        prepared.push((raw_ref, tok.wire(), req_id));
                    }
                    let start = std::time::Instant::now();
                    for (raw_ref, wire, req_id) in &prepared {
                        let resp = client
                            .post(&url)
                            .header(HDR_PICKUP_TOKEN, wire)
                            .header(HDR_FRAME_REF, raw_ref.as_str())
                            .header(HDR_SERVICE_ID, "yolo-svc")
                            .header(HDR_REQUEST_ID, req_id)
                            .send()
                            .await
                            .expect("send");
                        let status = resp.status();
                        let body = resp.bytes().await.expect("body");
                        debug_assert!(status.is_success());
                        black_box(body);
                    }
                    start.elapsed()
                })
            });
        });
    }
    group.finish();
}

// -----------------------------------------------------------------------------
// 6. pickup_core_model — mint + verify + consume + LRU remove + audit row.
//    This is the *pickup segment* of `service_call_v1`, NOT the full call:
//    router lookup, rate limit, QUIC dispatch, and `alias_calls` audit live
//    outside this micro-bench. A dedicated `service_call_v1_overhead` end-to-end
//    bench (router + audit + mock dispatch) is queued for M3 once the router
//    fixtures land. The numbers below therefore lower-bound the real overhead.
// -----------------------------------------------------------------------------

fn bench_pickup_core_model(c: &mut Criterion) {
    let iss = issuer();
    let storage = FrameStorage::new(4096);
    let db = make_db();
    let template = rgb24_frame(320, 240);

    c.bench_function("pickup_core_model", |b| {
        b.iter_batched(
            || {
                let raw_ref = storage.insert(template.clone());
                let (tok, _) = iss.issue(
                    raw_ref.as_str().to_string(),
                    "yolo-svc".into(),
                    "req-bench".into(),
                );
                (raw_ref, tok.wire())
            },
            |(raw_ref, wire)| {
                let pr = PickupRequest {
                    pickup_token: Some(&wire),
                    frame_ref: Some(raw_ref.as_str()),
                    service_id: Some("yolo-svc"),
                    request_id: Some("req-bench"),
                };
                let outcome = handle_pickup(pr, &iss, &storage, &db);
                debug_assert!(matches!(outcome, PickupOutcome::Ok { .. }));
                black_box(outcome);
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

criterion_group!(
    benches,
    bench_pickup_token_issue,
    bench_pickup_token_verify_only,
    bench_pickup_token_consume,
    bench_frame_storage,
    bench_streaming_bus,
    bench_pickup_handler_direct,
    bench_pickup_http_roundtrip,
    bench_pickup_core_model,
);
criterion_main!(benches);
