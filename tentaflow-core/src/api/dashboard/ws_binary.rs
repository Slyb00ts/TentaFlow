// =============================================================================
// Plik: api/dashboard/ws_binary.rs
// Opis: Binary WebSocket handler dla protokołu CBOR (Envelope + MessageBody).
//       Zastapi REST w kolejnych fazach (#36). Na razie obsluguje handshake
//       schema version + kilka bootstrap wariantow (ModelListRequest,
//       MetaHeartbeat, MetaCancelStream).
//       Pelny dispatch tablicy variantow dokonczy sie po #27 (proc-macro + inventory).
// =============================================================================

use futures::{stream::SplitSink, FutureExt, SinkExt, StreamExt};
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tentaflow_protocol::{
    envelope::{Envelope, EnvelopeFlags, Routing},
    message_body::{MessageBody, ProtocolError, ProtocolErrorCode},
    CameraAdminPayload, SessionAuth, StreamPayload, SCHEMA_VERSION,
};
use tokio::sync::{mpsc, oneshot, Notify};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use tracing::{debug, error, warn};

use crate::dispatch::{
    self, addon_perm_broadcast, audit_broadcast, meeting_live_broadcast, resume_token,
    subscription::{self, SubscriptionEvent},
    AppState, HandlerContext,
};
use tentaflow_protocol::MessageBody as Mb;

/// Limit rozmiaru pojedynczego binary frame (bajty). Wiecej = close 1009 (message too big).
/// Konserwatywnie 1 MiB — typowe requesty sa <1 KiB, deploy manifests mieszcza sie w 64 KiB.
const MAX_FRAME_SIZE: usize = 1_048_576;

/// Pojemnosc kanalu control (odpowiedzi sync, eventy unsolicited, Pong, Close).
/// Maly ruch — `send().await` z backpressure jest tu dopuszczalny.
const CONTROL_QUEUE_CAPACITY: usize = 256;

/// Pojemnosc kolejki media (chunki wideo fMP4, ramki detekcji). Mala celowo:
/// przy pelnym oknie TCP klienta WAN najstarsze elementy wypadaja zamiast
/// blokowac reszte ruchu polaczenia.
const MEDIA_QUEUE_CAPACITY: usize = 64;

/// Limit czasu pojedynczego zapisu do sinka WS. Polmartwe polaczenie TCP
/// (pelne okno, brak ACK/RST) nie moze wisiec w `send()` w nieskonczonosc —
/// po przekroczeniu limitu writer zamyka polaczenie, odblokowujac nadawcow
/// czekajacych na kanale control.
const SINK_WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Interwal serwerowego WS Ping (pisany przez writer-task wprost do sinka,
/// z pominieciem kolejek). Klient (przegladarka) odpowiada Pongiem na
/// poziomie protokolu, co zasila read-idle-timeout — martwe polaczenie
/// (zamknieta karta, zerwana siec) wykrywamy w ~30 s zamiast czekac na
/// spozniony blad odczytu TLS (~90-150 s).
const KEEPALIVE_PING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// Read-idle-timeout: jesli od peera nie przyszla ZADNA ramka (w tym Pong na
/// nasz keepalive) przez ten czas, polaczenie jest martwe — przerywamy petle
/// odczytu, co uruchamia istniejacy cleanup (subskrypcje, writer, sesje UI).
const READ_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Monotonic counter for unique per-connection identifiers. Used as key in the
/// UI SessionRegistry so panel lifecycle state is scoped per WS socket.
static NEXT_CONNECTION_ID: AtomicU64 = AtomicU64::new(1);

/// Ramka wychodzaca zbudowana przez nadawce. Sequence NIE jest tu nadawany —
/// writer-task przydziela go tuz przed zapisem, dzieki czemu numeracja na
/// drucie jest monotoniczna niezaleznie od kolejnosci kolejkowania.
struct OutFrame {
    correlation_id: u64,
    message_kind: u16,
    flags: EnvelopeFlags,
    body_bytes: Vec<u8>,
}

/// Element kanalu control per polaczenie.
enum ControlFrame {
    /// Ramka Envelope do zakodowania i wyslania.
    Body(OutFrame),
    /// Gotowa ramka protokolu WS (Pong, Close) — pisana bez kopertowania.
    Raw(Message),
}

/// Element kolejki media: chunk streamu + informacja czy stream jest lossless
/// (fMP4 — utrata chunka psuje strumien MSE i wymaga resynca).
struct MediaFrame {
    frame: OutFrame,
    lossless: bool,
}

/// Wynik proby wstawienia ramki do kolejki media.
enum MediaPush {
    /// Ramka przyjeta. Lista = correlation_id streamow lossless, ktorych
    /// chunki wypadly z kolejki (do anulowania w SubscriptionRegistry).
    Accepted(Vec<u64>),
    /// Stream tej ramki jest zatruty (stracil wczesniej chunk) — ramka
    /// odrzucona; nadawca wysyla ramke terminalna kanalem control i konczy.
    /// Lista jak w `Accepted` (drop-oldest mogl zatruc takze inne streamy).
    Poisoned(Vec<u64>),
    /// Writer-task polaczenia juz nie zyje.
    Closed,
}

/// Stan kolejki media pod jednym mutexem: ramki + zbior zatrutych streamow.
struct MediaQueueInner {
    items: VecDeque<MediaFrame>,
    /// Correlation_id streamow lossless, ktore stracily chunk — ich kolejne
    /// ramki sa odrzucane az wlasciciel domknie stream (purge czysci wpis).
    poisoned: HashSet<u64>,
}

/// Kolejka media per polaczenie z polityka drop-oldest. Przy pelnej kolejce
/// najstarszy element wypada; jesli byl to chunk streamu lossless, stream
/// jest zatruwany (dalsze ramki odrzucane) i trafia na liste do anulowania —
/// klient dostaje ramke terminalna kanalem control i odbudowuje MSE.
struct MediaQueue {
    inner: std::sync::Mutex<MediaQueueInner>,
    notify: Notify,
    closed: AtomicBool,
}

impl MediaQueue {
    fn new() -> Self {
        Self {
            inner: std::sync::Mutex::new(MediaQueueInner {
                items: VecDeque::with_capacity(MEDIA_QUEUE_CAPACITY),
                poisoned: HashSet::new(),
            }),
            notify: Notify::new(),
            closed: AtomicBool::new(false),
        }
    }

    /// Kolejkuje ramke media wedlug polityki drop-oldest + zatruwanie.
    fn push(&self, item: MediaFrame) -> MediaPush {
        if self.closed.load(Ordering::Acquire) {
            return MediaPush::Closed;
        }
        let own_cid = item.frame.correlation_id;
        let mut cancels: Vec<u64> = Vec::new();
        let own_poisoned;
        {
            let mut inner = self.inner.lock().unwrap();
            if inner.poisoned.contains(&own_cid) {
                // Stream stracil wczesniej chunk — kazda kolejna ramka jest
                // bezuzyteczna dla MSE; nadawca ma domknac stream terminalem.
                return MediaPush::Poisoned(cancels);
            }
            while inner.items.len() >= MEDIA_QUEUE_CAPACITY {
                let Some(dropped) = inner.items.pop_front() else {
                    break;
                };
                if dropped.lossless {
                    // Strumien fMP4 stracil chunk — usuwamy zalegle ramki i
                    // zatruwamy correlation_id, zeby ramki wyslane PO dziurze
                    // nigdy nie dotarly do klienta.
                    let cid = dropped.frame.correlation_id;
                    inner.items.retain(|it| it.frame.correlation_id != cid);
                    inner.poisoned.insert(cid);
                    cancels.push(cid);
                }
            }
            own_poisoned = item.lossless && inner.poisoned.contains(&own_cid);
            if !own_poisoned {
                inner.items.push_back(item);
            }
        }
        self.notify.notify_one();
        if own_poisoned {
            MediaPush::Poisoned(cancels)
        } else {
            MediaPush::Accepted(cancels)
        }
    }

    fn pop(&self) -> Option<MediaFrame> {
        self.inner.lock().unwrap().items.pop_front()
    }

    /// Domyka stan streamu w kolejce: usuwa zalegle chunki (zeby ramka
    /// terminalna nie zostala wyprzedzona przez nieaktualne dane) i czysci
    /// zatrucie — correlation_id moze zostac ponownie uzyty po resubscribe.
    fn purge(&self, correlation_id: u64) {
        let mut inner = self.inner.lock().unwrap();
        inner
            .items
            .retain(|it| it.frame.correlation_id != correlation_id);
        inner.poisoned.remove(&correlation_id);
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }
}

/// Klasyfikuje chunk streamu: `Some(lossless)` = kolejka media, `None` = kanal
/// control (male chunki, np. tokeny LLM, SubscribeResponse, ramki koncowe).
fn media_class(body: &MessageBody) -> Option<bool> {
    match body {
        MessageBody::StreamBody(StreamPayload::Frame(f)) => {
            // Chmury punktow (lidar/scene) sa latest-wins — drop jest bezpieczny.
            // fMP4 kamery wymaga resynca MSE po utracie chunka.
            let lossy = f.stream_id.starts_with("lidar:")
                || f.stream_id.starts_with("scene:")
                || f.stream_id.starts_with("scene-depth:");
            Some(!lossy)
        }
        MessageBody::CameraAdminBody(CameraAdminPayload::DetectionsFrame(_)) => Some(false),
        _ => None,
    }
}

/// Obsluguje pojedyncze polaczenie binary-WS. Petla odczytu dispatchuje frame'y;
/// caly zapis idzie przez dedykowany writer-task (jedyny wlasciciel sinka),
/// zasilany kanalem control i kolejka media — powolny odbiorca WAN nie blokuje
/// odpowiedzi sync ani innych streamow (brak head-of-line blocking na mutexie).
///
/// `user_id` + `role` z JWT claims (extract_ws_user_session w server.rs).
/// None = degraduje do Anonymous session — handler dispatch sprawdzi czy wariant
/// na to pozwala.
/// `resume_secret` = HMAC key dla SubscribeResumeOffer tokens emitowanych przy
/// IS_STREAM_END (zwykle reuse jwt_secret).
pub async fn handle_ws_connection<S>(
    stream: S,
    user_id: Option<String>,
    role: Option<String>,
    resume_secret: std::sync::Arc<Vec<u8>>,
    app_state: std::sync::Arc<AppState>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let connection_id = NEXT_CONNECTION_ID.fetch_add(1, Ordering::Relaxed);

    let session = match user_id
        .as_deref()
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
    {
        Some(uuid) => SessionAuth::UserSession {
            user_id: *uuid.as_bytes(),
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

    // Atomic sequence wspolny dla writer-taska (nadaje numery przy zapisie)
    // i taskow streamow (resume token czyta biezaca wartosc).
    // P1 FIX: u64 zeby uniknac overflow na long-lived connections.
    let next_server_sequence = Arc::new(AtomicU64::new(1));
    let mut last_client_sequence: u64 = 0;
    let mut handshake_done = false;
    // Tracking subskrypcji utworzonych przez to polaczenie — sprzatamy je przy
    // disconnect zeby uniknac memory leak w global SubscriptionRegistry.
    let mut owned_subscription_ids: Vec<u64> = Vec::new();
    // Org context jest staly dla sesji (zalezy tylko od user_id) — rozwiazywany
    // raz per polaczenie i cache'owany. Zewnetrzny Option = "czy juz rozwiazano".
    let mut cached_org_context: Option<Option<crate::services::rbac::OrgContext>> = None;

    // Writer-task: jedyny wlasciciel sinka. Kanal control ma bezwzgledny
    // priorytet nad kolejka media; shutdown przychodzi z petli odczytu.
    let (control_tx, control_rx) = mpsc::channel::<ControlFrame>(CONTROL_QUEUE_CAPACITY);
    let media_queue = Arc::new(MediaQueue::new());
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    tokio::spawn(connection_writer(
        sink,
        control_rx,
        Arc::clone(&media_queue),
        Arc::clone(&next_server_sequence),
        shutdown_rx,
    ));

    debug!("binary-WS: nowe polaczenie");

    // Spawnuj task ktory pushuje audit eventy jako unsolicited frames.
    {
        let tx_audit = control_tx.clone();
        let mut audit_rx = audit_broadcast::subscribe();
        tokio::spawn(async move {
            while let Ok(event) = audit_rx.recv().await {
                if send_body(
                    &tx_audit,
                    0, // unsolicited — correlation_id 0 (no matching request)
                    tentaflow_protocol::envelope::message_kind::META_HEARTBEAT,
                    &Mb::AuditEventBody(event),
                    EnvelopeFlags::empty(),
                )
                .await
                .is_err()
                {
                    break;
                }
            }
        });
    }

    // Spawnuj task pushujacy SystemEvent jako unsolicited frames — service status
    // + mesh peer status. GUI nasluchuje przez ApiBinary.onUnsolicited i pokazuje
    // toasty/odswieza karty bez pollowania.
    {
        let tx_sys = control_tx.clone();
        let mut sys_rx = crate::dispatch::system_event_broadcast::subscribe();
        tokio::spawn(async move {
            while let Ok(event) = sys_rx.recv().await {
                if send_body(
                    &tx_sys,
                    0,
                    tentaflow_protocol::envelope::message_kind::META_HEARTBEAT,
                    &Mb::SystemEventBody(event),
                    EnvelopeFlags::empty(),
                )
                .await
                .is_err()
                {
                    break;
                }
            }
        });
    }

    // Spawnuj task pushujacy AddonPermissionChangedEvent jako unsolicited frames.
    {
        let tx_perm = control_tx.clone();
        let mut perm_rx = addon_perm_broadcast::subscribe();
        tokio::spawn(async move {
            while let Ok(event) = perm_rx.recv().await {
                if send_body(
                    &tx_perm,
                    0,
                    tentaflow_protocol::envelope::message_kind::META_HEARTBEAT,
                    &Mb::AddonPermissionChangedEventBody(event),
                    EnvelopeFlags::empty(),
                )
                .await
                .is_err()
                {
                    break;
                }
            }
        });
    }

    // Spawnuj task pushujacy MeetingLiveEvent jako unsolicited frames. Filtr
    // ownership: tylko wlasciciel sesji (meeting_sessions.owner_user_id == uid)
    // dostaje frame. Sesje bez owner_user_id (legacy) widoczne dla wszystkich
    // zalogowanych — zgodne z list_sessions(owner_user_id=Some(uid)) ktory tez
    // pokazuje OR IS NULL. Anonimowe polaczenia i connecty bez user_id nie
    // dostaja niczego.
    if let Some(uid) = user_id.clone() {
        let tx_meet = control_tx.clone();
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
                // zeby nie blokowac watkow tokio dla innych broadcastow.
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
                if send_body(
                    &tx_meet,
                    0,
                    tentaflow_protocol::envelope::message_kind::META_HEARTBEAT,
                    &Mb::MeetingLiveEventBody(event),
                    EnvelopeFlags::empty(),
                )
                .await
                .is_err()
                {
                    break;
                }
            }
        });
    }

    // Spawnuj task pushujacy UI CBOR messages (addon→frontend) jako unsolicited
    // UiChannelCbor frames. Filtr: source_user musi odpowiadac user_id polaczenia.
    if let Some(uid) = user_id.clone() {
        let tx_ui = control_tx.clone();
        let mut ui_rx = crate::dispatch::ui_cbor_broadcast::subscribe();
        tokio::spawn(async move {
            loop {
                let push = match ui_rx.recv().await {
                    Ok(p) => p,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                if push.user_id != uid {
                    continue;
                }
                if send_body(
                    &tx_ui,
                    0,
                    tentaflow_protocol::envelope::message_kind::META_HEARTBEAT,
                    &Mb::UiChannelCbor(push.cbor.to_vec()),
                    EnvelopeFlags::empty(),
                )
                .await
                .is_err()
                {
                    break;
                }
            }
        });
    }

    loop {
        // Read-idle-timeout: timeout restartuje sie przy kazdej ramce (takze
        // Pong na serwerowy keepalive) — brak jakiejkolwiek ramki przez
        // READ_IDLE_TIMEOUT oznacza martwe polaczenie.
        let msg = match tokio::time::timeout(READ_IDLE_TIMEOUT, source.next()).await {
            Ok(Some(m)) => m,
            Ok(None) => break,
            Err(_) => {
                warn!(
                    "binary-WS: brak ramek od klienta przez {}s — zamykam martwe polaczenie",
                    READ_IDLE_TIMEOUT.as_secs()
                );
                break;
            }
        };
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
                    let _ = control_tx
                        .send(ControlFrame::Raw(Message::Close(Some(close_frame(
                            1009,
                            "message too big",
                        )))))
                        .await;
                    break;
                }

                let envelope = match tentaflow_protocol::cbor::decode::<Envelope>(&bytes) {
                    Ok(env) => env,
                    Err(e) => {
                        warn!("binary-WS: malformed envelope: {}", e);
                        let _ = control_tx
                            .send(ControlFrame::Raw(Message::Close(Some(close_frame(
                                1002,
                                "malformed envelope",
                            )))))
                            .await;
                        break;
                    }
                };

                if !matches!(envelope.routing, Routing::Direct) {
                    warn!("binary-WS: forward routing nie wspierany (jeszcze) w GUI WS");
                    let _ = send_protocol_error(
                        &control_tx,
                        envelope.correlation_id,
                        ProtocolErrorCode::NotImplemented,
                        "forward routing not supported on this endpoint",
                    )
                    .await;
                    continue;
                }

                // Klient JS po setJwt() / reconnect / nowej karcie startuje
                // licznik od 1 (`GLOBAL_NEXT_SEQUENCE` w binary-ws-client.js).
                // Jezeli ta sama TCP/WS connection przezyla taki reset (rzadki
                // race przy switchu auth), serwerowy `last_client_sequence` ma
                // jeszcze stara wartosc — `sequence == 1` traktujemy jako
                // legalny sygnal "fresh client" i zerujemy licznik. Replay
                // protection ma sens dla mesh peer-to-peer (publiczna siec);
                // dla dashboard WS pod TLS+JWT to tylko sanity check kolejnosci.
                if envelope.sequence == 1 && last_client_sequence > 0 {
                    tracing::info!(
                        prev = last_client_sequence,
                        "binary-WS: client sequence reset to 1 — przyjmuje jako fresh client"
                    );
                    last_client_sequence = 0;
                }
                // Dashboard WS runs over TLS+JWT, but the client can legitimately
                // deliver frames out of order (WebTransport datagrams, async send
                // races). Replay protection is a mesh-peer concern; here it is only
                // an ordering sanity check, so an out-of-order/duplicate frame is
                // LOGGED but still processed. Dropping it (the old behavior) silently
                // killed stream subscribes — e.g. a live-view tile stuck on
                // "connecting" because its SubscribeRequest frame was discarded.
                if envelope.sequence <= last_client_sequence {
                    tracing::debug!(
                        "binary-WS: sequence {} <= {} (out-of-order, przetwarzam mimo to)",
                        envelope.sequence,
                        last_client_sequence
                    );
                }
                last_client_sequence = last_client_sequence.max(envelope.sequence);

                let body = match tentaflow_protocol::cbor::decode::<MessageBody>(&envelope.body) {
                    Ok(b) => b,
                    Err(e) => {
                        warn!("binary-WS: malformed body: {}", e);
                        let _ = send_protocol_error(
                            &control_tx,
                            envelope.correlation_id,
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
                                asset_build_hash: super::static_files::ASSET_BUILD_HASH.to_string(),
                            };
                            let _ = send_body(
                                &control_tx,
                                envelope.correlation_id,
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
                                &control_tx,
                                envelope.correlation_id,
                                ProtocolErrorCode::AuthRequired,
                                "handshake required (MetaSchemaVersionCheck)",
                            )
                            .await;
                            break;
                        }
                    }
                }

                // F2 P1.b — OrgContext zalezy wylacznie od user_id sesji (pin
                // przez X-Org-Id dojdzie z org-switcherem w P1.c), wiec jest
                // staly dla polaczenia: rozwiazujemy raz (rusqlite jest sync —
                // pierwsze rozwiazanie idzie przez spawn_blocking, zeby nie
                // blokowac petli odczytu) i cache'ujemy. Blad przejsciowy DB
                // nie jest cache'owany — kolejna wiadomosc sprobuje ponownie.
                let org_context = match &cached_org_context {
                    Some(ctx) => ctx.clone(),
                    None => match &session {
                        SessionAuth::UserSession { user_id, .. } => {
                            let user_id_str = uuid::Uuid::from_bytes(*user_id).to_string();
                            let db = app_state.db.clone();
                            let uid = user_id_str.clone();
                            let resolved = tokio::task::spawn_blocking(move || {
                                crate::services::rbac::resolve_org_context(&db, &uid, None)
                            })
                            .await;
                            match resolved {
                                Ok(Ok(ctx)) => {
                                    cached_org_context = Some(Some(ctx.clone()));
                                    Some(ctx)
                                }
                                // NoMembership is an expected steady-state for a
                                // user who has not yet been added to any org — log
                                // at warn (informative for the operator, not
                                // alarming). Every other variant indicates either
                                // a DB problem or a malformed header that the
                                // operator does need to see at `error`.
                                Ok(Err(crate::services::rbac::OrgContextError::NoMembership(
                                    uid,
                                ))) => {
                                    warn!(
                                        "binary-WS: user '{}' has no org membership — org_context=None",
                                        uid
                                    );
                                    cached_org_context = Some(None);
                                    None
                                }
                                Ok(Err(e)) => {
                                    error!(
                                        "binary-WS: org_context resolution failed (user_id={}): {}",
                                        user_id_str, e
                                    );
                                    None
                                }
                                Err(e) => {
                                    error!(
                                        "binary-WS: org_context resolution task failed (user_id={}): {}",
                                        user_id_str, e
                                    );
                                    None
                                }
                            }
                        }
                        _ => {
                            cached_org_context = Some(None);
                            None
                        }
                    },
                };
                let ctx = HandlerContext {
                    session: session.clone(),
                    correlation_id: envelope.correlation_id,
                    connection_id,
                    resume_secret: Some(resume_secret.clone()),
                    state: app_state.clone(),
                    org_context,
                };

                let variant_name = dispatch::variant_name_of(&body);

                // P1 FIX: streaming = osobny tokio task, NIE blokuje main read loop.
                // Wiele streamow moze biec rownolegle; chunki media ida osobna
                // kolejka (drop-oldest), wiec powolny klient nie zatrzymuje
                // odpowiedzi sync ani innych streamow.
                if let Some(stream_meta) = subscription::find_stream_handler(variant_name) {
                    if !stream_meta.required_auth.session_satisfies(&session) {
                        let _ = send_protocol_error(
                            &control_tx,
                            envelope.correlation_id,
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

                    // Spawn task drenujacy rx — chunki media do kolejki media,
                    // reszta (male chunki, ramki koncowe) kanalem control.
                    let control_tx_stream = control_tx.clone();
                    let media_stream = Arc::clone(&media_queue);
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
                                SubscriptionEvent::Chunk(chunk_body) => match media_class(
                                    &chunk_body,
                                ) {
                                    Some(lossless) => {
                                        let body_bytes =
                                            match tentaflow_protocol::cbor::encode(&chunk_body) {
                                                Ok(b) => b,
                                                Err(e) => {
                                                    warn!(
                                                        "binary-WS: encode body failed: {}",
                                                        e
                                                    );
                                                    continue;
                                                }
                                            };
                                        let frame = OutFrame {
                                            correlation_id,
                                            message_kind,
                                            flags: EnvelopeFlags::IS_STREAM_CHUNK,
                                            body_bytes,
                                        };
                                        let (cancels, own_poisoned) =
                                            match media_stream.push(MediaFrame { frame, lossless })
                                            {
                                                MediaPush::Accepted(c) => (c, false),
                                                MediaPush::Poisoned(c) => (c, true),
                                                MediaPush::Closed => break,
                                            };
                                        // Streamy lossless, ktore stracily chunki w kolejce —
                                        // zatrzymujemy producentow (best-effort). Ramke
                                        // terminalna kazdego z nich dostarcza NIEZAWODNIE task
                                        // wlasciciela: przy nastepnym push dostanie Poisoned
                                        // (galaz nizej) albo skonsumuje event Error z cancel.
                                        for cid in cancels {
                                            if cid != correlation_id {
                                                subscription::global().cancel(cid);
                                            }
                                        }
                                        if own_poisoned {
                                            // Wlasny stream stracil chunk — ramka terminalna
                                            // idzie kanalem control (bounded + send().await),
                                            // nie kolejka media, wiec dociera nawet przy
                                            // przeciazeniu; klient robi resubscribe i odbudowuje
                                            // MSE od nowego init segmentu.
                                            media_stream.purge(correlation_id);
                                            let _ = send_body(
                                                &control_tx_stream,
                                                correlation_id,
                                                message_kind,
                                                &MessageBody::Error(ProtocolError {
                                                    code: ProtocolErrorCode::StreamCancelled,
                                                    message: "subscriber_lagged: media queue \
                                                              overflow — stream requires resync"
                                                        .to_string(),
                                                    trace_id: None,
                                                }),
                                                EnvelopeFlags::IS_ERROR
                                                    | EnvelopeFlags::IS_STREAM_END,
                                            )
                                            .await;
                                            break;
                                        }
                                    }
                                    None => {
                                        if send_body(
                                            &control_tx_stream,
                                            correlation_id,
                                            message_kind,
                                            &chunk_body,
                                            EnvelopeFlags::IS_STREAM_CHUNK,
                                        )
                                        .await
                                        .is_err()
                                        {
                                            break;
                                        }
                                    }
                                },
                                SubscriptionEvent::End(final_body) => {
                                    media_stream.purge(correlation_id);
                                    let token = resume_token::issue(
                                        correlation_id as u128,
                                        seq_clone.load(Ordering::SeqCst),
                                        originating_user_id,
                                        &resume_secret_clone,
                                    );
                                    let _ = send_body(
                                        &control_tx_stream,
                                        correlation_id,
                                        message_kind,
                                        &MessageBody::SubscribeResumeOffer {
                                            resume_token: token,
                                        },
                                        EnvelopeFlags::empty(),
                                    )
                                    .await;
                                    let body =
                                        final_body.unwrap_or_else(|| MessageBody::MetaCancelStream);
                                    let _ = send_body(
                                        &control_tx_stream,
                                        correlation_id,
                                        message_kind,
                                        &body,
                                        EnvelopeFlags::IS_STREAM_END,
                                    )
                                    .await;
                                    break;
                                }
                                SubscriptionEvent::Error(err) => {
                                    media_stream.purge(correlation_id);
                                    let _ = send_body(
                                        &control_tx_stream,
                                        correlation_id,
                                        message_kind,
                                        &MessageBody::Error(err),
                                        EnvelopeFlags::IS_ERROR | EnvelopeFlags::IS_STREAM_END,
                                    )
                                    .await;
                                    break;
                                }
                            }
                        }
                        // Cleanup po naturalnym koncu (task wie kiedy stream sie konczy).
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
                    &control_tx,
                    envelope.correlation_id,
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
                let _ = control_tx
                    .send(ControlFrame::Raw(Message::Close(Some(close_frame(
                        1003,
                        "text frames not supported",
                    )))))
                    .await;
                break;
            }
            Message::Ping(data) => {
                let _ = control_tx.send(ControlFrame::Raw(Message::Pong(data))).await;
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

    // Zatrzymaj writer-task: kolejka media przestaje przyjmowac ramki, a sygnal
    // shutdown kaze writerowi dopisac zalegle ramki control (np. Close) i
    // zamknac sink.
    media_queue.close();
    let _ = shutdown_tx.send(());

    app_state.ui_sessions.remove(connection_id);

    debug!("binary-WS: polaczenie zamkniete");
}

/// Writer-task per polaczenie — jedyny wlasciciel SplitSink. Pisze ramki z
/// kanalu control (priorytet bezwzgledny) i kolejki media; sequence nadaje
/// przy zapisie, dzieki czemu numeracja na drucie jest monotoniczna.
async fn connection_writer<S>(
    mut sink: SplitSink<WebSocketStream<S>, Message>,
    mut control_rx: mpsc::Receiver<ControlFrame>,
    media: Arc<MediaQueue>,
    seq: Arc<AtomicU64>,
    mut shutdown_rx: oneshot::Receiver<()>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    // Serwerowy keepalive: WS Ping pisany wprost do sinka co interwal,
    // bezwarunkowo — klient czysto odbierajacy generuje ruch przychodzacy
    // (resetujacy read-idle-timeout serwera) wylacznie Pongiem na nasz Ping.
    let mut ping_tick = tokio::time::interval(KEEPALIVE_PING_INTERVAL);
    ping_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    'outer: loop {
        // Najpierw oprozniamy control bez czekania — odpowiedzi sync i Pong
        // zawsze wyprzedzaja chunki media, a pilne ramki (Close, terminal
        // error) nie moga czekac na zapis Pinga.
        loop {
            match control_rx.try_recv() {
                Ok(frame) => {
                    if write_control_frame(&mut sink, frame, &seq).await.is_err() {
                        break 'outer;
                    }
                }
                Err(mpsc::error::TryRecvError::Empty)
                | Err(mpsc::error::TryRecvError::Disconnected) => break,
            }
        }
        // Tick sprawdzany bez czekania w kazdej iteracji (juz PO drenazu
        // control), zeby Ping wychodzil takze pod ciaglym ruchem media
        // (petla nie dociera wtedy do selecta na dole).
        if ping_tick.tick().now_or_never().is_some()
            && send_with_timeout(
                &mut sink,
                Message::Ping(tokio_tungstenite::tungstenite::Bytes::new()),
            )
            .await
            .is_err()
        {
            break;
        }
        if let Some(item) = media.pop() {
            if let Some(bytes) = encode_envelope(item.frame, next_seq(&seq)) {
                if send_with_timeout(&mut sink, Message::Binary(bytes.into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            continue;
        }
        tokio::select! {
            biased;
            _ = &mut shutdown_rx => {
                // Petla odczytu skonczyla prace — dopisz zalegle ramki control
                // (np. Close z powodem) i zamknij polaczenie.
                while let Ok(frame) = control_rx.try_recv() {
                    if write_control_frame(&mut sink, frame, &seq).await.is_err() {
                        break;
                    }
                }
                break;
            }
            maybe = control_rx.recv() => match maybe {
                Some(frame) => {
                    if write_control_frame(&mut sink, frame, &seq).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            _ = media.notify.notified() => {}
            _ = ping_tick.tick() => {
                if send_with_timeout(
                    &mut sink,
                    Message::Ping(tokio_tungstenite::tungstenite::Bytes::new()),
                )
                .await
                .is_err()
                {
                    break;
                }
            }
        }
    }
    media.close();
    // Zamkniecie tez z limitem czasu — polmartwe TCP nie moze unieruchomic
    // writer-taska w nieskonczonosc.
    let _ = tokio::time::timeout(SINK_WRITE_TIMEOUT, sink.close()).await;
}

/// Zapis pojedynczej ramki WS z limitem czasu. Err zarowno przy bledzie
/// zapisu, jak i przekroczeniu SINK_WRITE_TIMEOUT (polmartwe TCP) — caller
/// zamyka polaczenie.
async fn send_with_timeout<S>(
    sink: &mut SplitSink<WebSocketStream<S>, Message>,
    msg: Message,
) -> Result<(), ()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    match tokio::time::timeout(SINK_WRITE_TIMEOUT, sink.send(msg)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) | Err(_) => Err(()),
    }
}

/// Pisze jedna ramke control do sinka. Err = zapis padl (blad lub timeout)
/// albo wyslano ramke Close — w obu przypadkach writer konczy prace.
async fn write_control_frame<S>(
    sink: &mut SplitSink<WebSocketStream<S>, Message>,
    frame: ControlFrame,
    seq: &AtomicU64,
) -> Result<(), ()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    match frame {
        ControlFrame::Body(out) => {
            let Some(bytes) = encode_envelope(out, next_seq(seq)) else {
                return Ok(());
            };
            send_with_timeout(sink, Message::Binary(bytes.into())).await
        }
        ControlFrame::Raw(msg) => {
            let is_close = matches!(msg, Message::Close(_));
            let sent = send_with_timeout(sink, msg).await;
            if is_close || sent.is_err() {
                Err(())
            } else {
                Ok(())
            }
        }
    }
}

/// Koduje MessageBody i kolejkuje ramke w kanale control polaczenia.
/// Sequence nadaje writer-task przy zapisie. Err = writer juz nie zyje.
async fn send_body(
    control_tx: &mpsc::Sender<ControlFrame>,
    correlation_id: u64,
    message_kind: u16,
    body: &MessageBody,
    flags: EnvelopeFlags,
) -> Result<(), ()> {
    let body_bytes = match tentaflow_protocol::cbor::encode(body) {
        Ok(b) => b,
        Err(e) => {
            warn!("binary-WS: encode body failed: {}", e);
            return Ok(());
        }
    };
    control_tx
        .send(ControlFrame::Body(OutFrame {
            correlation_id,
            message_kind,
            flags,
            body_bytes,
        }))
        .await
        .map_err(|_| ())
}

/// Koduje Envelope z nadanym sequence. Zwraca None tylko gdy CBOR padlo
/// (loguje sam, caller pomija ten frame).
fn encode_envelope(frame: OutFrame, sequence: u64) -> Option<Vec<u8>> {
    let mut env = Envelope::new_direct(
        frame.correlation_id,
        sequence,
        frame.message_kind,
        frame.body_bytes,
    );
    env.flags = frame.flags;
    match tentaflow_protocol::cbor::encode(&env) {
        Ok(b) => Some(b),
        Err(e) => {
            warn!("binary-WS: encode envelope failed: {}", e);
            None
        }
    }
}

async fn send_protocol_error(
    control_tx: &mpsc::Sender<ControlFrame>,
    correlation_id: u64,
    code: ProtocolErrorCode,
    message: &str,
) -> Result<(), ()> {
    let err = MessageBody::Error(ProtocolError {
        code,
        message: message.to_string(),
        trace_id: None,
    });
    send_body(
        control_tx,
        correlation_id,
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
