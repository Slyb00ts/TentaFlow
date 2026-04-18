// =============================================================================
// Plik: api/dashboard/ws_binary.rs
// Opis: Binary WebSocket handler dla nowego protokolu rkyv (Envelope + MessageBody).
//       Zastapi REST w kolejnych fazach (#36). Na razie obsluguje handshake
//       schema version + kilka bootstrap wariantow (NodeListRequest,
//       ModelListRequest, MetaHeartbeat, MetaCancelStream, NodeInfoRequest).
//       Pelny dispatch tablicy variantow dokonczy sie po #27 (proc-macro + inventory).
// =============================================================================

use futures::{stream::SplitSink, SinkExt, StreamExt};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tentaflow_protocol::{
    envelope::{Envelope, EnvelopeFlags, Routing},
    message_body::{MessageBody, ProtocolError, ProtocolErrorCode},
    SCHEMA_VERSION, SessionAuth,
};
use tokio::sync::Mutex as AsyncMutex;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use tracing::{debug, warn};

use crate::dispatch::{
    self, resume_token,
    subscription::{self, SubscriptionEvent},
    HandlerContext,
};

/// Sink wrapped in async mutex zeby main read loop + streaming tasks moga
/// dzielic write side WS bez wzajemnego blokowania read side.
type SharedSink<S> = Arc<AsyncMutex<SplitSink<WebSocketStream<S>, Message>>>;

/// Limit rozmiaru pojedynczego binary frame (bajty). Wiecej = close 1009 (message too big).
/// Konserwatywnie 1 MiB — typowe requesty sa <1 KiB, deploy manifests mieszcza sie w 64 KiB.
const MAX_FRAME_SIZE: usize = 1_048_576;

/// Mapuje SQLite i64 user_id do 16-bajtowego SessionAuth user_id.
/// Format zeby odroznic od stub `[0u8; 16]` (system user / nieuwierzytelniony):
///   bajt 0    = 0xFF (marker "i64-derived")
///   bajt 1-7  = 0x00 (reserved)
///   bajt 8-15 = LE u64 reprezentacja i64 (sign-extended)
/// Real UUIDv4 nigdy nie ma 0xFF na pozycji 0 (variant=10xx, version=4xxx),
/// wiec konflikt z UUID space wykluczony.
fn user_id_to_bytes(user_id: i64) -> [u8; 16] {
    let mut buf = [0u8; 16];
    buf[0] = 0xFF;
    buf[8..].copy_from_slice(&(user_id as u64).to_le_bytes());
    buf
}

/// Odwrotnosc `user_id_to_bytes` — przy walidacji ze user_id ma marker 0xFF.
/// Zwraca None gdy bajty nie sa formatu i64-derived (system stub lub real UUID).
#[allow(dead_code)]
pub fn bytes_to_user_id(bytes: &[u8; 16]) -> Option<i64> {
    if bytes[0] != 0xFF || bytes[1..8].iter().any(|&b| b != 0) {
        return None;
    }
    let mut le = [0u8; 8];
    le.copy_from_slice(&bytes[8..]);
    Some(i64::from_le_bytes(le))
}

/// Obsluguje pojedyncze polaczenie binary-WS. Single-threaded loop read/write,
/// kazdy frame dispatch synchroniczny (dla streamingu bedzie osobny task per stream).
///
/// `user_id` + `role` z JWT claims (extract_ws_user_session w server.rs).
/// None = degraduje do Anonymous session — handler dispatch sprawdzi czy wariant
/// na to pozwala.
/// `resume_secret` = HMAC key dla SubscribeResumeOffer tokens emitowanych przy
/// IS_STREAM_END (zwykle reuse jwt_secret).
pub async fn handle_ws_connection<S>(
    stream: S,
    user_id: Option<i64>,
    role: Option<String>,
    resume_secret: std::sync::Arc<Vec<u8>>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let session = match user_id {
        Some(id) => SessionAuth::UserSession {
            user_id: user_id_to_bytes(id),
            role: role.clone(),
        },
        None => SessionAuth::Anonymous,
    };

    let ws = WebSocketStream::from_raw_socket(
        stream,
        tokio_tungstenite::tungstenite::protocol::Role::Server,
        None,
    )
    .await;
    let (sink, mut source) = ws.split();
    let sink: SharedSink<S> = Arc::new(AsyncMutex::new(sink));

    // Atomic sequence shared miedzy main loop a streaming tasks.
    // P1 FIX: u64 zeby uniknac overflow na long-lived connections.
    let next_server_sequence = Arc::new(AtomicU64::new(1));
    let mut last_client_sequence: u64 = 0;
    let mut handshake_done = false;
    // Tracking subskrypcji utworzonych przez to polaczenie — sprzatamy je przy
    // disconnect zeby uniknac memory leak w global SubscriptionRegistry.
    let mut owned_subscription_ids: Vec<u64> = Vec::new();

    debug!("binary-WS: nowe polaczenie");

    while let Some(msg) = source.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                warn!("binary-WS: blad odczytu frame: {}", e);
                break;
            }
        };

        match msg {
            Message::Binary(bytes) => {
                if bytes.len() > MAX_FRAME_SIZE {
                    warn!(
                        "binary-WS: frame {} bajtow > limit {} — zamykam",
                        bytes.len(),
                        MAX_FRAME_SIZE
                    );
                    let mut guard = sink.lock().await;
                    let _ = guard
                        .send(Message::Close(Some(close_frame(1009, "message too big"))))
                        .await;
                    break;
                }

                let envelope = match rkyv::from_bytes::<Envelope, rkyv::rancor::Error>(&bytes) {
                    Ok(env) => env,
                    Err(e) => {
                        warn!("binary-WS: malformed envelope: {}", e);
                        let _ = send_protocol_error(
                            &sink,
                            0,
                            next_seq(&next_server_sequence),
                            ProtocolErrorCode::InvalidFrame,
                            "malformed envelope",
                        )
                        .await;
                        continue;
                    }
                };

                if !matches!(envelope.routing, Routing::Direct) {
                    warn!("binary-WS: forward routing nie wspierany (jeszcze) w GUI WS");
                    let _ = send_protocol_error(
                        &sink,
                        envelope.correlation_id,
                        next_seq(&next_server_sequence),
                        ProtocolErrorCode::NotImplemented,
                        "forward routing not supported on this endpoint",
                    )
                    .await;
                    continue;
                }

                if envelope.sequence <= last_client_sequence {
                    warn!(
                        "binary-WS: sequence {} <= {} — odrzucam (replay)",
                        envelope.sequence, last_client_sequence
                    );
                    let _ = send_protocol_error(
                        &sink,
                        envelope.correlation_id,
                        next_seq(&next_server_sequence),
                        ProtocolErrorCode::InvalidFrame,
                        "sequence not monotonically increasing",
                    )
                    .await;
                    continue;
                }
                last_client_sequence = envelope.sequence;

                let body = match rkyv::from_bytes::<MessageBody, rkyv::rancor::Error>(
                    &envelope.body,
                ) {
                    Ok(b) => b,
                    Err(e) => {
                        warn!("binary-WS: malformed body: {}", e);
                        let _ = send_protocol_error(
                            &sink,
                            envelope.correlation_id,
                            next_seq(&next_server_sequence),
                            ProtocolErrorCode::InvalidFrame,
                            "malformed body",
                        )
                        .await;
                        continue;
                    }
                };

                if !handshake_done {
                    match body {
                        MessageBody::MetaSchemaVersionCheck { client_version } => {
                            let accepted = client_version == SCHEMA_VERSION;
                            let response = MessageBody::MetaSchemaVersionAck {
                                server_version: SCHEMA_VERSION,
                                accepted,
                            };
                            let _ = send_body(
                                &sink,
                                envelope.correlation_id,
                                next_seq(&next_server_sequence),
                                envelope.message_kind,
                                &response,
                                EnvelopeFlags::empty(),
                            )
                            .await;
                            if !accepted {
                                warn!(
                                    "binary-WS: schema mismatch client={} server={}",
                                    client_version, SCHEMA_VERSION
                                );
                                break;
                            }
                            handshake_done = true;
                            continue;
                        }
                        _ => {
                            let _ = send_protocol_error(
                                &sink,
                                envelope.correlation_id,
                                next_seq(&next_server_sequence),
                                ProtocolErrorCode::AuthRequired,
                                "handshake required (MetaSchemaVersionCheck)",
                            )
                            .await;
                            break;
                        }
                    }
                }

                let ctx = HandlerContext {
                    session: session.clone(),
                    correlation_id: envelope.correlation_id,
                    resume_secret: Some(resume_secret.clone()),
                };

                let variant_name = dispatch::variant_name_of(&body);

                // P1 FIX: streaming = osobny tokio task, NIE blokuje main read loop.
                // Wiele streamow moze biec rownolegle; klient moze cancel/heartbeat
                // miedzy chunkami (sink jest w Mutex — write contention znikoma vs
                // korzysci concurrency).
                if let Some(stream_meta) = subscription::find_stream_handler(variant_name) {
                    if !stream_meta.required_auth.session_satisfies(&session) {
                        let _ = send_protocol_error(
                            &sink,
                            envelope.correlation_id,
                            next_seq(&next_server_sequence),
                            ProtocolErrorCode::PolicyDenied,
                            "stream handler requires elevated session",
                        )
                        .await;
                        continue;
                    }
                    let registry = subscription::global();
                    let (sub, rx) = registry.create(envelope.correlation_id, None);
                    owned_subscription_ids.push(envelope.correlation_id);
                    (stream_meta.handler_fn)(body.clone(), ctx.clone(), sub);

                    // Spawn writer task — drain rx, push frames przez sink (Mutex'd).
                    let sink_clone = Arc::clone(&sink);
                    let seq_clone = Arc::clone(&next_server_sequence);
                    let resume_secret_clone = Arc::clone(&resume_secret);
                    let originating_user_id = match &session {
                        SessionAuth::UserSession { user_id, .. } => *user_id,
                        _ => [0u8; 16],
                    };
                    let correlation_id = envelope.correlation_id;
                    let message_kind = envelope.message_kind;

                    tokio::spawn(async move {
                        let mut rx = rx;
                        while let Some(event) = rx.recv().await {
                            match event {
                                SubscriptionEvent::Chunk(chunk_body) => {
                                    let _ = send_body(
                                        &sink_clone,
                                        correlation_id,
                                        next_seq(&seq_clone),
                                        message_kind,
                                        &chunk_body,
                                        EnvelopeFlags::IS_STREAM_CHUNK,
                                    )
                                    .await;
                                }
                                SubscriptionEvent::End(final_body) => {
                                    let token = resume_token::issue(
                                        correlation_id as u128,
                                        seq_clone.load(Ordering::SeqCst),
                                        originating_user_id,
                                        &resume_secret_clone,
                                    );
                                    let _ = send_body(
                                        &sink_clone,
                                        correlation_id,
                                        next_seq(&seq_clone),
                                        message_kind,
                                        &MessageBody::SubscribeResumeOffer {
                                            resume_token: token,
                                        },
                                        EnvelopeFlags::empty(),
                                    )
                                    .await;
                                    let body = final_body
                                        .unwrap_or_else(|| MessageBody::MetaCancelStream);
                                    let _ = send_body(
                                        &sink_clone,
                                        correlation_id,
                                        next_seq(&seq_clone),
                                        message_kind,
                                        &body,
                                        EnvelopeFlags::IS_STREAM_END,
                                    )
                                    .await;
                                    break;
                                }
                                SubscriptionEvent::Error(err) => {
                                    let _ = send_body(
                                        &sink_clone,
                                        correlation_id,
                                        next_seq(&seq_clone),
                                        message_kind,
                                        &MessageBody::Error(err),
                                        EnvelopeFlags::IS_ERROR | EnvelopeFlags::IS_STREAM_END,
                                    )
                                    .await;
                                    break;
                                }
                            }
                        }
                        // Cleanup po naturalnym koncu (writer task wie kiedy stream sie konczy).
                        subscription::global().cancel(correlation_id);
                    });
                    continue;
                }

                // Sync handler — pojedyncza odpowiedz.
                let (resp_body, is_error) = dispatch::dispatch(&body, &ctx);
                let flags = if is_error {
                    EnvelopeFlags::IS_ERROR
                } else {
                    EnvelopeFlags::empty()
                };
                let _ = send_body(
                    &sink,
                    envelope.correlation_id,
                    next_seq(&next_server_sequence),
                    envelope.message_kind,
                    &resp_body,
                    flags,
                )
                .await;
            }
            Message::Text(t) => {
                warn!("binary-WS: otrzymano text frame ({} bajtow) — zamykam", t.len());
                let mut guard = sink.lock().await;
                let _ = guard
                    .send(Message::Close(Some(close_frame(
                        1003,
                        "text frames not supported",
                    ))))
                    .await;
                break;
            }
            Message::Ping(data) => {
                let mut guard = sink.lock().await;
                let _ = guard.send(Message::Pong(data)).await;
            }
            Message::Pong(_) => {}
            Message::Close(_) => break,
            Message::Frame(_) => {}
        }
    }

    // Cleanup wszystkich subskrypcji utworzonych przez to polaczenie zeby
    // unikngac memory leak w global SubscriptionRegistry.
    if !owned_subscription_ids.is_empty() {
        let registry = subscription::global();
        let cleanup_count = owned_subscription_ids
            .iter()
            .filter(|&&id| registry.cancel(id))
            .count();
        debug!(
            cleanup_count,
            owned = owned_subscription_ids.len(),
            "binary-WS: cleanup subskrypcji przy disconnect"
        );
    }

    debug!("binary-WS: polaczenie zamkniete");
}

async fn send_body<S>(
    sink: &SharedSink<S>,
    correlation_id: u64,
    sequence: u64,
    message_kind: u16,
    body: &MessageBody,
    flags: EnvelopeFlags,
) -> Result<(), tokio_tungstenite::tungstenite::Error>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let body_bytes = match rkyv::to_bytes::<rkyv::rancor::Error>(body) {
        Ok(b) => b.to_vec(),
        Err(e) => {
            warn!("binary-WS: encode body failed: {}", e);
            return Ok(());
        }
    };
    let mut env = Envelope::new_direct(correlation_id, sequence, message_kind, body_bytes);
    env.flags = flags;
    let env_bytes = match rkyv::to_bytes::<rkyv::rancor::Error>(&env) {
        Ok(b) => b.to_vec(),
        Err(e) => {
            warn!("binary-WS: encode envelope failed: {}", e);
            return Ok(());
        }
    };
    let mut guard = sink.lock().await;
    guard.send(Message::Binary(env_bytes)).await
}

async fn send_protocol_error<S>(
    sink: &SharedSink<S>,
    correlation_id: u64,
    sequence: u64,
    code: ProtocolErrorCode,
    message: &str,
) -> Result<(), tokio_tungstenite::tungstenite::Error>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let err = MessageBody::Error(ProtocolError {
        code,
        message: message.to_string(),
        trace_id: None,
    });
    send_body(
        sink,
        correlation_id,
        sequence,
        tentaflow_protocol::envelope::message_kind::META_PROTOCOL_ERROR,
        &err,
        EnvelopeFlags::IS_ERROR,
    )
    .await
}

/// Helper: pobierz nastepny server sequence (atomic).
fn next_seq(counter: &AtomicU64) -> u64 {
    counter.fetch_add(1, Ordering::SeqCst)
}

fn close_frame(
    code: u16,
    reason: &'static str,
) -> tokio_tungstenite::tungstenite::protocol::CloseFrame<'static> {
    tokio_tungstenite::tungstenite::protocol::CloseFrame {
        code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::from(code),
        reason: std::borrow::Cow::Borrowed(reason),
    }
}

// Dispatch pokryty w `crate::dispatch::tests` — te scenariusze sa teraz testowane
// tam. ws_binary testy end-to-end (Envelope->Dispatcher->Response) pojda w #34.
