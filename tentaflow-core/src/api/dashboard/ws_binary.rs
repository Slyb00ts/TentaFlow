// =============================================================================
// Plik: api/dashboard/ws_binary.rs
// Opis: Binary WebSocket handler dla nowego protokolu rkyv (Envelope + MessageBody).
//       Zastapi REST w kolejnych fazach (#36). Na razie obsluguje handshake
//       schema version + kilka bootstrap wariantow (NodeListRequest,
//       ModelListRequest, MetaHeartbeat, MetaCancelStream, NodeInfoRequest).
//       Pelny dispatch tablicy variantow dokonczy sie po #27 (proc-macro + inventory).
// =============================================================================

use futures::{SinkExt, StreamExt};
use tentaflow_protocol::{
    envelope::{Envelope, EnvelopeFlags, Routing},
    message_body::{MessageBody, ProtocolError, ProtocolErrorCode},
    SCHEMA_VERSION, SessionAuth,
};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use tracing::{debug, warn};

use crate::dispatch::{self, HandlerContext};

/// Limit rozmiaru pojedynczego binary frame (bajty). Wiecej = close 1009 (message too big).
/// Konserwatywnie 1 MiB — typowe requesty sa <1 KiB, deploy manifests mieszcza sie w 64 KiB.
const MAX_FRAME_SIZE: usize = 1_048_576;

/// Obsluguje pojedyncze polaczenie binary-WS. Single-threaded loop read/write,
/// kazdy frame dispatch synchroniczny (dla streamingu bedzie osobny task per stream).
pub async fn handle_ws_connection<S>(stream: S)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let ws = WebSocketStream::from_raw_socket(
        stream,
        tokio_tungstenite::tungstenite::protocol::Role::Server,
        None,
    )
    .await;
    let (mut sink, mut source) = ws.split();

    let mut next_server_sequence: u32 = 1;
    let mut last_client_sequence: u32 = 0;
    let mut handshake_done = false;

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
                    let _ = sink
                        .send(Message::Close(Some(close_frame(
                            1009,
                            "message too big",
                        ))))
                        .await;
                    break;
                }

                let envelope = match rkyv::from_bytes::<Envelope, rkyv::rancor::Error>(&bytes) {
                    Ok(env) => env,
                    Err(e) => {
                        warn!("binary-WS: malformed envelope: {}", e);
                        let _ = send_protocol_error(
                            &mut sink,
                            0,
                            next_server_sequence,
                            ProtocolErrorCode::InvalidFrame,
                            "malformed envelope",
                        )
                        .await;
                        next_server_sequence += 1;
                        continue;
                    }
                };

                if !matches!(envelope.routing, Routing::Direct) {
                    warn!("binary-WS: forward routing nie wspierany (jeszcze) w GUI WS");
                    let _ = send_protocol_error(
                        &mut sink,
                        envelope.correlation_id,
                        next_server_sequence,
                        ProtocolErrorCode::NotImplemented,
                        "forward routing not supported on this endpoint",
                    )
                    .await;
                    next_server_sequence += 1;
                    continue;
                }

                if envelope.sequence <= last_client_sequence {
                    warn!(
                        "binary-WS: sequence {} <= {} — odrzucam (replay)",
                        envelope.sequence, last_client_sequence
                    );
                    let _ = send_protocol_error(
                        &mut sink,
                        envelope.correlation_id,
                        next_server_sequence,
                        ProtocolErrorCode::InvalidFrame,
                        "sequence not monotonically increasing",
                    )
                    .await;
                    next_server_sequence += 1;
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
                            &mut sink,
                            envelope.correlation_id,
                            next_server_sequence,
                            ProtocolErrorCode::InvalidFrame,
                            "malformed body",
                        )
                        .await;
                        next_server_sequence += 1;
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
                                &mut sink,
                                envelope.correlation_id,
                                next_server_sequence,
                                envelope.message_kind,
                                &response,
                                EnvelopeFlags::empty(),
                            )
                            .await;
                            next_server_sequence += 1;
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
                                &mut sink,
                                envelope.correlation_id,
                                next_server_sequence,
                                ProtocolErrorCode::AuthRequired,
                                "handshake required (MetaSchemaVersionCheck)",
                            )
                            .await;
                            next_server_sequence += 1;
                            break;
                        }
                    }
                }

                // Dispatch przez registry (handlerzy rejestrowani przez `#[handler]`).
                // Auth na razie = UserSession placeholder (JWT validator w validate_ws_upgrade
                // juz zweryfikowal token; user_id trzeba bedzie propagowac w #36).
                let ctx = HandlerContext {
                    session: SessionAuth::UserSession { user_id: [0u8; 16] },
                    correlation_id: envelope.correlation_id,
                };
                let (resp_body, is_error) = dispatch::dispatch(&body, &ctx);
                let flags = if is_error {
                    EnvelopeFlags::IS_ERROR
                } else {
                    EnvelopeFlags::empty()
                };
                let _ = send_body(
                    &mut sink,
                    envelope.correlation_id,
                    next_server_sequence,
                    envelope.message_kind,
                    &resp_body,
                    flags,
                )
                .await;
                next_server_sequence += 1;
            }
            Message::Text(t) => {
                warn!("binary-WS: otrzymano text frame ({} bajtow) — zamykam", t.len());
                let _ = sink
                    .send(Message::Close(Some(close_frame(
                        1003,
                        "text frames not supported",
                    ))))
                    .await;
                break;
            }
            Message::Ping(data) => {
                let _ = sink.send(Message::Pong(data)).await;
            }
            Message::Pong(_) => {}
            Message::Close(_) => break,
            Message::Frame(_) => {}
        }
    }

    debug!("binary-WS: polaczenie zamkniete");
}

async fn send_body<S>(
    sink: &mut futures::stream::SplitSink<WebSocketStream<S>, Message>,
    correlation_id: u64,
    sequence: u32,
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
    sink.send(Message::Binary(env_bytes)).await
}

async fn send_protocol_error<S>(
    sink: &mut futures::stream::SplitSink<WebSocketStream<S>, Message>,
    correlation_id: u64,
    sequence: u32,
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
