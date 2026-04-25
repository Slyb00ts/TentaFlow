// =============================================================================
// Plik: api/dashboard/ws_binary.rs
// Opis: Binary WebSocket handler dla nowego protokolu rkyv (Envelope + MessageBody).
//       Zastapi REST w kolejnych fazach (#36). Na razie obsluguje handshake
//       schema version + kilka bootstrap wariantow (ModelListRequest,
//       MetaHeartbeat, MetaCancelStream).
//       Pelny dispatch tablicy variantow dokonczy sie po #27 (proc-macro + inventory).
// =============================================================================

use futures::{stream::SplitSink, SinkExt, StreamExt};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tentaflow_protocol::{
    envelope::{Envelope, EnvelopeFlags, Routing},
    message_body::{MessageBody, ProtocolError, ProtocolErrorCode},
    SessionAuth, SCHEMA_VERSION,
};
use tokio::sync::Mutex as AsyncMutex;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use tracing::{debug, warn};

use crate::dispatch::{
    self, addon_perm_broadcast, audit_broadcast, meeting_live_broadcast, resume_token,
    subscription::{self, SubscriptionEvent},
    AppState, HandlerContext,
};
use tentaflow_protocol::MessageBody as Mb;

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
    app_state: std::sync::Arc<AppState>,
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

    // Spawnuj task ktory pushuje audit eventy jako unsolicited frames.
    {
        let sink_audit = Arc::clone(&sink);
        let seq_audit = Arc::clone(&next_server_sequence);
        let mut audit_rx = audit_broadcast::subscribe();
        tokio::spawn(async move {
            while let Ok(event) = audit_rx.recv().await {
                let _ = send_body(
                    &sink_audit,
                    0, // unsolicited — correlation_id 0 (no matching request)
                    next_seq(&seq_audit),
                    tentaflow_protocol::envelope::message_kind::META_HEARTBEAT,
                    &Mb::AuditEventBody(event),
                    EnvelopeFlags::empty(),
                )
                .await;
            }
        });
    }

    // Spawnuj task pushujacy SystemEvent jako unsolicited frames — service status
    // + mesh peer status. GUI nasluchuje przez ApiBinary.onUnsolicited i pokazuje
    // toasty/odswieza karty bez pollowania.
    {
        let sink_sys = Arc::clone(&sink);
        let seq_sys = Arc::clone(&next_server_sequence);
        let mut sys_rx = crate::dispatch::system_event_broadcast::subscribe();
        tokio::spawn(async move {
            while let Ok(event) = sys_rx.recv().await {
                let _ = send_body(
                    &sink_sys,
                    0,
                    next_seq(&seq_sys),
                    tentaflow_protocol::envelope::message_kind::META_HEARTBEAT,
                    &Mb::SystemEventBody(event),
                    EnvelopeFlags::empty(),
                )
                .await;
            }
        });
    }

    // Spawnuj task pushujacy AddonPermissionChangedEvent jako unsolicited frames.
    {
        let sink_perm = Arc::clone(&sink);
        let seq_perm = Arc::clone(&next_server_sequence);
        let mut perm_rx = addon_perm_broadcast::subscribe();
        tokio::spawn(async move {
            while let Ok(event) = perm_rx.recv().await {
                let _ = send_body(
                    &sink_perm,
                    0,
                    next_seq(&seq_perm),
                    tentaflow_protocol::envelope::message_kind::META_HEARTBEAT,
                    &Mb::AddonPermissionChangedEventBody(event),
                    EnvelopeFlags::empty(),
                )
                .await;
            }
        });
    }

    // Spawnuj task pushujacy MeetingLiveEvent jako unsolicited frames. Filtr
    // ownership: tylko wlasciciel sesji (meeting_sessions.owner_user_id == uid)
    // dostaje frame. Sesje bez owner_user_id (legacy) widoczne dla wszystkich
    // zalogowanych — zgodne z list_sessions(owner_user_id=Some(uid)) ktory tez
    // pokazuje OR IS NULL. Anonimowe polaczenia i connecty bez user_id nie
    // dostaja niczego.
    if let Some(uid) = user_id {
        let sink_meet = Arc::clone(&sink);
        let seq_meet = Arc::clone(&next_server_sequence);
        let db = app_state.db.clone();
        let mut meet_rx = meeting_live_broadcast::subscribe();
        tokio::spawn(async move {
            loop {
                let event = match meet_rx.recv().await {
                    Ok(e) => e,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                // Ownership lookup blocking (rusqlite) — zwijamy w spawn_blocking
                // zeby nie blokowac writer task dla innych broadcastow.
                let key = event.meeting_key.clone();
                let db2 = db.clone();
                let ownership = tokio::task::spawn_blocking(move || {
                    crate::db::repository::transcripts::owner_of_meeting_key(&db2, &key)
                })
                .await;
                let should_deliver = match ownership {
                    Ok(Ok(Some(Some(owner)))) => owner == uid,
                    // Sesja bez ownera — legacy, doreczamy kazdemu zalogowanemu.
                    Ok(Ok(Some(None))) => true,
                    // Sesja nie istnieje lub blad DB — pomijamy frame (bezpieczny default).
                    _ => false,
                };
                if !should_deliver {
                    continue;
                }
                let _ = send_body(
                    &sink_meet,
                    0,
                    next_seq(&seq_meet),
                    tentaflow_protocol::envelope::message_kind::META_HEARTBEAT,
                    &Mb::MeetingLiveEventBody(event),
                    EnvelopeFlags::empty(),
                )
                .await;
            }
        });
    }

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

                let body =
                    match rkyv::from_bytes::<MessageBody, rkyv::rancor::Error>(&envelope.body) {
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
                    state: app_state.clone(),
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
                        // Batch writer: drain do BATCH_MAX events w jednym
                        // sink.lock, feed po kolei (no flush per frame),
                        // flush raz na koniec batch'u. Dla szybkich silnikow
                        // (~900 tok/s na hi-end GPU) zmniejsza liczbe syscall
                        // write i mutex acquire ~16x. recv_many naturalnie
                        // grupuje pending events bez explicit timera —
                        // pierwsza recv blokuje, kolejne sa drained from
                        // channel buffer (subscription mpsc cap 64).
                        const BATCH_MAX: usize = 16;
                        let mut buffer: Vec<SubscriptionEvent> =
                            Vec::with_capacity(BATCH_MAX);
                        let mut stream_finished = false;

                        loop {
                            buffer.clear();
                            let n = rx.recv_many(&mut buffer, BATCH_MAX).await;
                            if n == 0 {
                                break; // channel closed
                            }

                            // Encode wszystkie frame'y z batch'u przed sink.lock —
                            // rkyv encode jest CPU bound, lepiej zrobic to bez
                            // trzymania mutex'a (inne writer task moga rownolegle).
                            type Frame = (Vec<u8>, bool); // (bytes, is_terminal)
                            let mut frames: Vec<Frame> = Vec::with_capacity(n * 3);

                            for event in buffer.drain(..) {
                                match event {
                                    SubscriptionEvent::Chunk(chunk_body) => {
                                        if let Some(b) = encode_envelope_bytes(
                                            correlation_id,
                                            next_seq(&seq_clone),
                                            message_kind,
                                            &chunk_body,
                                            EnvelopeFlags::IS_STREAM_CHUNK,
                                        ) {
                                            frames.push((b, false));
                                        }
                                    }
                                    SubscriptionEvent::End(final_body) => {
                                        let token = resume_token::issue(
                                            correlation_id as u128,
                                            seq_clone.load(Ordering::SeqCst),
                                            originating_user_id,
                                            &resume_secret_clone,
                                        );
                                        if let Some(b) = encode_envelope_bytes(
                                            correlation_id,
                                            next_seq(&seq_clone),
                                            message_kind,
                                            &MessageBody::SubscribeResumeOffer {
                                                resume_token: token,
                                            },
                                            EnvelopeFlags::empty(),
                                        ) {
                                            frames.push((b, false));
                                        }
                                        let body = final_body
                                            .unwrap_or(MessageBody::MetaCancelStream);
                                        if let Some(b) = encode_envelope_bytes(
                                            correlation_id,
                                            next_seq(&seq_clone),
                                            message_kind,
                                            &body,
                                            EnvelopeFlags::IS_STREAM_END,
                                        ) {
                                            frames.push((b, true));
                                        }
                                        stream_finished = true;
                                    }
                                    SubscriptionEvent::Error(err) => {
                                        if let Some(b) = encode_envelope_bytes(
                                            correlation_id,
                                            next_seq(&seq_clone),
                                            message_kind,
                                            &MessageBody::Error(err),
                                            EnvelopeFlags::IS_ERROR
                                                | EnvelopeFlags::IS_STREAM_END,
                                        ) {
                                            frames.push((b, true));
                                        }
                                        stream_finished = true;
                                    }
                                }
                            }

                            // Single sink.lock acquire — send wszystkie frames
                            // pod tym samym mutexem (zmniejsza kontestowanie
                            // z innymi writer tasks: broadcasty heartbeat,
                            // mesh updates). Uzywamy `send` (= feed + flush)
                            // per frame zamiast feed-N + flush bo tungstenite
                            // moze miec write buffer limit ktory zatka caly
                            // stream gdy zbyt wiele feed'ow przed flush.
                            if !frames.is_empty() {
                                let mut guard = sink_clone.lock().await;
                                for (bytes, _) in frames {
                                    if guard.send(Message::Binary(bytes.into())).await.is_err() {
                                        stream_finished = true;
                                        break;
                                    }
                                }
                            }

                            if stream_finished {
                                break;
                            }
                        }
                        // Cleanup po naturalnym koncu (writer task wie kiedy stream sie konczy).
                        subscription::global().cancel(correlation_id);
                    });
                    continue;
                }

                // Zunifikowany async dispatch — sync handlery wrapowane przez makro.
                let (resp_body, is_error) = dispatch::dispatch(&body, &ctx).await;
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
                warn!(
                    "binary-WS: otrzymano text frame ({} bajtow) — zamykam",
                    t.len()
                );
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
    let env_bytes = match encode_envelope_bytes(correlation_id, sequence, message_kind, body, flags)
    {
        Some(b) => b,
        None => return Ok(()),
    };
    let mut guard = sink.lock().await;
    guard.send(Message::Binary(env_bytes.into())).await
}

/// Bytes-only encoder — uzywany przez batch writer task ktory feed'uje
/// wiele frame'ow w jednym sink.lock + flush. Zwraca None tylko gdy rkyv
/// padlo (loguje sam, caller pomija ten frame).
fn encode_envelope_bytes(
    correlation_id: u64,
    sequence: u64,
    message_kind: u16,
    body: &MessageBody,
    flags: EnvelopeFlags,
) -> Option<Vec<u8>> {
    let body_bytes = match rkyv::to_bytes::<rkyv::rancor::Error>(body) {
        Ok(b) => b.to_vec(),
        Err(e) => {
            warn!("binary-WS: encode body failed: {}", e);
            return None;
        }
    };
    let mut env = Envelope::new_direct(correlation_id, sequence, message_kind, body_bytes);
    env.flags = flags;
    match rkyv::to_bytes::<rkyv::rancor::Error>(&env) {
        Ok(b) => Some(b.to_vec()),
        Err(e) => {
            warn!("binary-WS: encode envelope failed: {}", e);
            None
        }
    }
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
) -> tokio_tungstenite::tungstenite::protocol::CloseFrame {
    tokio_tungstenite::tungstenite::protocol::CloseFrame {
        code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::from(code),
        reason: reason.into(),
    }
}

// Dispatch pokryty w `crate::dispatch::tests` — te scenariusze sa teraz testowane
// tam. ws_binary testy end-to-end (Envelope->Dispatcher->Response) pojda w #34.
