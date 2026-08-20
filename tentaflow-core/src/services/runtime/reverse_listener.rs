// ===== File: services/runtime/reverse_listener.rs — accept_bi loop for service-initiated requests =====
//
// Core is the iroh CLIENT of a sidecar (it dials the container), but some
// sidecars — the meeting bot — need to call Core back (STT/TTS/flow turns,
// meeting events). They do that by `open_bi` on the connection Core opened.
// Nothing accepts those streams by default: this listener is attached only to
// handles whose engine manifest declares `reverse_requests = true`.

use std::sync::Arc;

use tokio::sync::{watch, Semaphore};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use tentaflow_protocol::{
    ErrorInfo, ErrorType, ModelRequest, ModelResponse, ModelResult, ModelStreamChunk,
    StreamChunkType,
};
use tentaflow_transport::{read_frame, write_frame, write_raw_frame};

use crate::mesh::inference_proxy::{
    dispatch_reverse_request, dispatch_reverse_stream_request, ReverseCaller,
};
use crate::net::quic::QuicClient;
use crate::routing::Router;

/// Upper bound of concurrently served reverse streams per service. A meeting
/// bot opens one FlowInvoke per speech segment plus short event streams; more
/// than this means a misbehaving sidecar, not load.
pub const DEFAULT_MAX_REVERSE_STREAMS: usize = 8;

/// Everything the listener needs besides the connection: the router that
/// dispatches requests and the identity under which the service is registered
/// (`meeting-bot-<session_id>`), which the FlowInvoke path matches against the
/// session row so a bot can only drive its own meeting.
#[derive(Clone)]
pub struct ReverseWiring {
    pub router: Router,
    pub service_name: String,
    pub max_streams: usize,
}

/// Runs the accept loop on the client's connection until `shutdown_rx` flips.
/// Survives reconnects: `QuicClient::iroh_connection` returns the live
/// `Connection` and re-dials when the previous one closed.
pub fn spawn_reverse_listener(
    client: Arc<QuicClient>,
    wiring: ReverseWiring,
    shutdown_rx: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        listener_loop(client, wiring, shutdown_rx).await;
    })
}

async fn listener_loop(
    client: Arc<QuicClient>,
    wiring: ReverseWiring,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let service = wiring.service_name.clone();
    let streams = Arc::new(Semaphore::new(wiring.max_streams.max(1)));
    info!(service = %service, "reverse listener started");

    loop {
        if *shutdown_rx.borrow() {
            break;
        }
        let conn = match client.iroh_connection().await {
            Ok(c) => c,
            Err(e) => {
                debug!(service = %service, "reverse listener: no connection: {e}");
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
                    _ = shutdown_signalled(&mut shutdown_rx) => break,
                }
                continue;
            }
        };

        tokio::select! {
            accepted = conn.accept_bi() => {
                match accepted {
                    Ok((send, recv)) => {
                        let Ok(permit) = streams.clone().try_acquire_owned() else {
                            warn!(service = %service, limit = wiring.max_streams,
                                "reverse stream refused: concurrency limit");
                            tokio::spawn(refuse_stream(send, recv, "reverse stream limit reached"));
                            continue;
                        };
                        let router = wiring.router.clone();
                        let caller = ReverseCaller { service_name: service.clone() };
                        tokio::spawn(async move {
                            let _permit = permit;
                            serve_stream(send, recv, router, caller).await;
                        });
                    }
                    Err(e) => {
                        // Connection gone — the next iteration re-dials through
                        // `iroh_connection`; a short pause avoids a hot loop
                        // while the peer is down.
                        debug!(service = %service, "reverse listener: accept_bi: {e}");
                        tokio::select! {
                            _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
                            _ = shutdown_signalled(&mut shutdown_rx) => break,
                        }
                    }
                }
            }
            _ = shutdown_signalled(&mut shutdown_rx) => break,
        }
    }
    info!(service = %service, "reverse listener stopped");
}

/// Resolves once the listener must stop: either the flag flipped to `true`, or
/// every sender was dropped. A bare `changed()` would return `Err` forever in
/// the latter case and spin this loop at full speed.
async fn shutdown_signalled(rx: &mut watch::Receiver<bool>) {
    loop {
        if *rx.borrow() {
            return;
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}

/// How long a peer may keep an accepted stream open without sending its
/// request. A sidecar writes the frame immediately after `open_bi`; anything
/// slower is a stuck or hostile peer holding one of the few stream permits.
const REQUEST_FRAME_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// The same read on a stream we already decided to refuse: it happens without
/// a permit, only to shape the error, so it waits far less.
const REFUSED_FRAME_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Reads the opening `[len][ModelRequest]` frame under a deadline.
async fn read_request(
    recv: &mut iroh::endpoint::RecvStream,
    service: &str,
    deadline: std::time::Duration,
) -> Option<ModelRequest> {
    match tokio::time::timeout(deadline, read_frame::<ModelRequest>(recv)).await {
        Ok(Ok(Some(r))) => Some(r),
        Ok(Ok(None)) => None,
        Ok(Err(e)) => {
            warn!(service = %service, "reverse stream: bad request frame: {e}");
            None
        }
        Err(_) => {
            warn!(service = %service, "reverse stream: no request frame within timeout");
            None
        }
    }
}

/// One service-initiated stream: `[len][ModelRequest]` in, either one
/// `ModelResponse` or a sequence of `ModelStreamChunk` frames out.
async fn serve_stream(
    mut send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
    router: Router,
    caller: ReverseCaller,
) {
    let Some(request) =
        read_request(&mut recv, &caller.service_name, REQUEST_FRAME_TIMEOUT).await
    else {
        return;
    };
    let request_id = request.request_id.clone();

    if request.stream {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        let producer = tokio::spawn(async move {
            dispatch_reverse_stream_request(&router, request, tx, Some(&caller)).await;
        });
        while let Some(bytes) = rx.recv().await {
            if let Err(e) = write_raw_frame(&mut send, &bytes).await {
                debug!(request_id = %request_id, "reverse stream: peer stopped reading: {e}");
                // Abort drops the producer future, which drops the flow turn's
                // cancellation guard — the flow stops instead of running to its
                // deadline for a consumer that is already gone.
                producer.abort();
                break;
            }
        }
        let _ = send.finish();
        return;
    }

    let response = dispatch_reverse_request(&router, request, Some(&caller)).await;
    if let Err(e) = write_frame(&mut send, &response).await {
        debug!(request_id = %request_id, "reverse stream: response write failed: {e}");
    }
    let _ = send.finish();
}

/// Refuses a stream we have no permit for. The refusal has to be shaped like
/// the answer the caller decodes: a streaming request expects
/// `ModelStreamChunk` frames, so a `ModelResponse` would surface on the bot as
/// a decode error instead of the rate limit that actually happened. The
/// request frame is read first for exactly that reason.
async fn refuse_stream(
    mut send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
    message: &str,
) {
    let (request_id, streaming) =
        match read_request(&mut recv, "reverse-limit", REFUSED_FRAME_TIMEOUT).await {
            Some(r) => (r.request_id, r.stream),
            None => (String::new(), false),
        };
    let error = ErrorInfo {
        error_type: ErrorType::RateLimitExceeded,
        message: message.to_string(),
        details: None,
    };
    if streaming {
        let chunk = ModelStreamChunk {
            request_id,
            chunk: StreamChunkType::Error(error),
        };
        let _ = write_frame(&mut send, &chunk).await;
    } else {
        let response = ModelResponse {
            request_id,
            result: ModelResult::Error(error),
            metrics: None,
        };
        let _ = write_frame(&mut send, &response).await;
    }
    let _ = send.finish();
}

#[cfg(test)]
mod tests {
    use super::*;
    use tentaflow_protocol::{FlowInvokePayload, ModelPayload};
    use tentaflow_transport::{build_server_endpoint, ServerEndpointConfig, ALPN_SERVICE};

    fn flow_invoke(stream: bool) -> ModelRequest {
        ModelRequest {
            request_id: if stream { "turn-stream" } else { "turn-unary" }.to_string(),
            payload: ModelPayload::FlowInvoke(FlowInvokePayload {
                flow_id: None,
                audio: None,
                text: Some("hello".into()),
                meta: vec![("meeting_id".to_string(), "mtg-unknown".to_string())],
                session_id: None,
            }),
            stream,
            metadata: None,
            session_id: None,
        }
    }

    // In-process iroh pair: a "bot" endpoint accepts Core's connection, then
    // opens two streams back (one streaming FlowInvoke, one unary request).
    // Core must answer both on the connection IT dialled — the exact path a
    // meeting bot uses — and reject the FlowInvoke because the meeting is
    // unknown, which proves the request reached the flow-turn validator.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn listener_serves_bot_initiated_streams_over_iroh() {
        let mut cfg = ServerEndpointConfig::ephemeral(vec![ALPN_SERVICE.to_vec()]);
        cfg.bind_addr = "127.0.0.1:0".parse().unwrap();
        cfg.enable_lan_discovery = false;
        cfg.enable_dht_discovery = false;
        let bot = build_server_endpoint(cfg).await.expect("bot endpoint");
        let bot_id = bot.id();
        let port = bot
            .bound_sockets()
            .into_iter()
            .find(|a| a.is_ipv4())
            .expect("ipv4 bind")
            .port();

        let bot_task = tokio::spawn(async move {
            let incoming = bot.accept().await.expect("incoming");
            let conn = incoming.await.expect("connection");

            let (mut send, mut recv) = conn.open_bi().await.expect("open_bi stream");
            write_frame(&mut send, &flow_invoke(true)).await.unwrap();
            let _ = send.finish();
            let chunk = read_frame::<ModelStreamChunk>(&mut recv)
                .await
                .expect("read chunk")
                .expect("chunk present");

            let (mut send, mut recv) = conn.open_bi().await.expect("open_bi unary");
            write_frame(&mut send, &flow_invoke(false)).await.unwrap();
            let _ = send.finish();
            let response = read_frame::<ModelResponse>(&mut recv)
                .await
                .expect("read response")
                .expect("response present");
            (chunk, response)
        });

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = crate::db::init(&path).expect("db");
        let router = Router::new(crate::config::RouterConfig::default(), Some(db)).expect("router");

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let qcfg = crate::net::quic::QuicConfig {
            name: "meeting-bot-1".into(),
            url: format!("iroh://{}", hex::encode(bot_id.as_bytes())),
            tls_ca: None,
            server_name: None,
            alpn: "tentaflow-service/v1".into(),
            timeout_ms: 10_000,
            auto_reconnect: true,
            reconnect_interval_ms: 500,
            keepalive_interval_ms: 5_000,
            skip_tls_verify: true,
            direct_addrs: vec![format!("127.0.0.1:{port}")],
        };
        let client = Arc::new(
            QuicClient::connect(qcfg, shutdown_rx.clone())
                .await
                .expect("core dials bot"),
        );
        let listener = spawn_reverse_listener(
            client,
            ReverseWiring {
                router,
                service_name: "meeting-bot-1".into(),
                max_streams: 2,
            },
            shutdown_rx,
        );

        let (chunk, response) = tokio::time::timeout(std::time::Duration::from_secs(30), bot_task)
            .await
            .expect("bot side finished in time")
            .expect("bot task");

        assert_eq!(chunk.request_id, "turn-stream");
        match chunk.chunk {
            StreamChunkType::Error(e) => {
                assert!(e.message.contains("unknown meeting_id"), "got: {}", e.message)
            }
            other => panic!("expected Error chunk, got {other:?}"),
        }
        assert_eq!(response.request_id, "turn-unary");
        assert!(matches!(response.result, ModelResult::Error(_)));

        let _ = shutdown_tx.send(true);
        listener.abort();
    }

    // A refusal must decode on the caller: a streaming request expects
    // `ModelStreamChunk` frames, so the rate-limit answer has to be a chunk.
    // Sending a `ModelResponse` there gave the bot a decode error and hid the
    // real reason it was turned away.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn refusal_matches_the_shape_the_caller_reads() {
        let mut cfg = ServerEndpointConfig::ephemeral(vec![ALPN_SERVICE.to_vec()]);
        cfg.bind_addr = "127.0.0.1:0".parse().unwrap();
        cfg.enable_lan_discovery = false;
        cfg.enable_dht_discovery = false;
        let bot = build_server_endpoint(cfg).await.expect("bot endpoint");
        let bot_id = bot.id();
        let port = bot
            .bound_sockets()
            .into_iter()
            .find(|a| a.is_ipv4())
            .expect("ipv4 bind")
            .port();

        let bot_task = tokio::spawn(async move {
            let incoming = bot.accept().await.expect("incoming");
            let conn = incoming.await.expect("connection");

            let (mut send, mut recv) = conn.open_bi().await.expect("open_bi stream");
            write_frame(&mut send, &flow_invoke(true)).await.unwrap();
            let _ = send.finish();
            let chunk = read_frame::<ModelStreamChunk>(&mut recv)
                .await
                .expect("read chunk")
                .expect("chunk present");

            let (mut send, mut recv) = conn.open_bi().await.expect("open_bi unary");
            write_frame(&mut send, &flow_invoke(false)).await.unwrap();
            let _ = send.finish();
            let response = read_frame::<ModelResponse>(&mut recv)
                .await
                .expect("read response")
                .expect("response present");
            (chunk, response)
        });

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let qcfg = crate::net::quic::QuicConfig {
            name: "meeting-bot-refused".into(),
            url: format!("iroh://{}", hex::encode(bot_id.as_bytes())),
            tls_ca: None,
            server_name: None,
            alpn: "tentaflow-service/v1".into(),
            timeout_ms: 10_000,
            auto_reconnect: true,
            reconnect_interval_ms: 500,
            keepalive_interval_ms: 5_000,
            skip_tls_verify: true,
            direct_addrs: vec![format!("127.0.0.1:{port}")],
        };
        let client = QuicClient::connect(qcfg, shutdown_rx.clone())
            .await
            .expect("core dials bot");
        let conn = client.iroh_connection().await.expect("core connection");

        // Both streams are refused, as the concurrency guard would.
        for _ in 0..2 {
            let (send, recv) = conn.accept_bi().await.expect("accept_bi");
            tokio::spawn(refuse_stream(send, recv, "reverse stream limit reached"));
        }

        let (chunk, response) = tokio::time::timeout(std::time::Duration::from_secs(30), bot_task)
            .await
            .expect("bot side finished in time")
            .expect("bot task");

        assert_eq!(chunk.request_id, "turn-stream");
        match chunk.chunk {
            StreamChunkType::Error(e) => {
                assert!(matches!(e.error_type, ErrorType::RateLimitExceeded));
                assert!(e.message.contains("limit"), "got: {}", e.message);
            }
            other => panic!("expected Error chunk, got {other:?}"),
        }
        assert_eq!(response.request_id, "turn-unary");
        match response.result {
            ModelResult::Error(e) => {
                assert!(matches!(e.error_type, ErrorType::RateLimitExceeded))
            }
            other => panic!("expected Error response, got {other:?}"),
        }

        let _ = shutdown_tx.send(true);
    }
}
