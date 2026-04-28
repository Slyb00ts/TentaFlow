// =============================================================================
// File: dom_observer.rs — push-based DOM event bridge for the Teams page.
//
// Why:
//   The previous design polled `Runtime.evaluate` from Rust every 500ms-3s
//   (`participant_scanner` + `detect_meeting_progress`) to decide whether the
//   bot was in lobby/joined and which participants existed. Each evaluate is
//   a CDP roundtrip plus a `document.body.innerText` serialization on the
//   Chromium side — 50-200ms a pop, plus the polling cadence on top. Result:
//   500-3000ms latency between a real DOM change and the GUI noticing it.
//
// What this does:
//   * Registers a CDP binding `__tentaflowEvent` on the page. JavaScript can
//     now call `window.__tentaflowEvent(jsonStr)` and Chromium emits a
//     `Runtime.bindingCalled` event over the same DevTools WebSocket that
//     chromiumoxide already uses. No polling, no roundtrip per check.
//   * Splits processing into two cooperating tasks:
//       - listener: konsumuje `EventBindingCalled`, robi LOKALNE update'y
//         stanu (znani uczestnicy, aktywny mowca, lifecycle watch) i wpycha
//         wynikowy `MeetingEvent` do mpsc.
//       - writer: drainuje mpsc i wykonuje QUIC `send_meeting_event`
//         rownolegle (semaphore cap = 8). Bez tego 5 osob dolaczajacych w
//         jednym DOM scan'ie powodowalo 5 x ~150ms RT sequencyjnie.
//   * Buduje JSON snapshot rosteru (Arc<String>) atomowo przy KAZDEJ zmianie
//     `known` mapy i wystawia go przez `ArcSwap`. STT hot path w main.rs
//     bierze go jednym `load_full()` zamiast `RwLock.read().await + serde_json
//     ::to_string` per segment.
//
// The matching JS side lives in `browser_inject.js` — a single MutationObserver
// scheduled through requestAnimationFrame that pushes events when the Teams
// DOM transitions actually happen.
// =============================================================================

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use arc_swap::ArcSwap;
use base64::Engine;
use chromiumoxide::cdp::js_protocol::runtime::{AddBindingParams, EventBindingCalled};
use chromiumoxide::page::Page;
use futures::StreamExt;
use serde::Deserialize;
use tentaflow_protocol::{
    MeetingEventPayload, RosterEntry, LIFECYCLE_JOINED, LIFECYCLE_LOBBY_WAITING,
};
use tokio::sync::{mpsc, watch, Mutex, RwLock, Semaphore};

use crate::quic_server::RouterClient;

pub type RouterHandle = Arc<Mutex<Option<Arc<RouterClient>>>>;
pub type SpeakerState = Arc<RwLock<Option<String>>>;
/// Atomowy snapshot rosteru jako gotowy JSON (Vec<String> nazw uczestnikow,
/// po sanityzacji). Pusta lista koduje sie jako `"[]"`. Czytanie z STT hot
/// path: `roster_snapshot.load_full()` -> `Arc<String>` (zero alokacji,
/// jeden atomic load).
pub type RosterSnapshotJson = Arc<ArcSwap<String>>;

/// JS-side event name registered as a CDP binding. Must match the constant
/// used inside `browser_inject.js`.
const BINDING_NAME: &str = "__tentaflowEvent";

/// Maksymalna liczba rownoleglych emitow QUIC. Przy 5 uczestnikach dolaczajacych
/// w tym samym DOM scan'ie chcemy zeby wszystkie poszly rownolegle, ale nie
/// pozwalamy zdziczec gdy Teams "wybuchnie" listą 50 osob.
const EMIT_CONCURRENCY: usize = 8;

/// Limity sanityzacji rosteru — chronia przed metadata-bomb z zlosliwie
/// zbugowanej strony Teams. 500 nazw pokrywa nawet duze wszechfirmowe
/// town-halle; wczesniej 50 ucinalo realny roster przy 60+ uczestnikach.
const MAX_ROSTER_NAMES: usize = 500;
const MAX_ROSTER_NAME_LEN: usize = 128;

/// Maksymalny rozmiar dekodowanej klatki wideo. JPEG q=0.6 przy 320px
/// realistycznie ma 15-40 KB, ale Teams custom backgrounds + slajdy o duzym
/// kontracie potrafia wyskoczyc do ~200 KB. 2 MB to twardy limit anty-DoS:
/// klatka wieksza znaczy ze cos jest zle (zly encode, zlosliwy payload).
const MAX_VIDEO_FRAME_BYTES: usize = 2 * 1024 * 1024;

/// State of the join flow as inferred from DOM events. The polling loop in
/// `browser.rs` previously baked this into a 500ms ticker; now the observer
/// task pushes transitions and `wait_for_joined` awaits them on a watch
/// channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinState {
    NotYet,
    Lobby,
    Joined,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum DomEvent {
    Lobby,
    Joined,
    /// Pełny snapshot rosteru — JS emituje to raz na DOM scan zamiast
    /// per-tile joined/left. `entries` zawiera *aktualną* listę kafelków
    /// (poza bot'em — filtrujemy w listenerze po nazwie).
    RosterSnapshot { entries: Vec<RosterTile> },
    ActiveSpeaker { id: Option<String>, name: Option<String> },
    /// Klatka wideo zescrappowana z `<video>` jednego kafelka. JS koduje JPEG
    /// jako base64 — binding `__tentaflowEvent` nosi tylko JSON, wiec binarka
    /// musi przejechac base64. W listenerze dekodujemy do `Vec<u8>` raz i
    /// dalej idzie binarnie przez rkyv.
    VideoFrame {
        participant_id: String,
        name: Option<String>,
        ts_ms: u64,
        jpeg_b64: String,
    },
    /// Untyped fallback so a JS-side schema bump doesn't kill the listener.
    #[serde(other)]
    Unknown,
}

/// Pojedynczy kafelek z DOM Teams — surowy zapis z `browser_inject.js`.
/// Konwertujemy do `RosterEntry` dopiero przy emit do routera (po filtracji
/// bot'a i sanityzacji nazwy). Pola `has_*` / `in_*` sa Option<bool> —
/// browser_inject.js domyslnie je wystawia, ale defaultujemy do `false`,
/// zeby starszy injectowany skrypt (po hot-reloadzie samego Rust binarka,
/// przed odswiezeniem strony Teams) nie wywalil deserializacji.
#[derive(Debug, Deserialize)]
struct RosterTile {
    id: String,
    name: Option<String>,
    #[serde(default)]
    has_video: bool,
    #[serde(default)]
    has_audio: bool,
    #[serde(default)]
    in_stage: bool,
    #[serde(default)]
    in_roster: bool,
}

/// Pojedynczy rekord przekazywany z listener -> writer. `meeting_key` jest
/// klonowany per-event (caly listener trzyma jeden String, ale writer spawnuje
/// async taski rownolegle — kazdy potrzebuje wlasnej kopii).
struct PendingEmit {
    meeting_key: String,
    timestamp_ms: i64,
    payload: MeetingEventPayload,
}

/// Lokalny stan kafelka po deduplikacji rosteru. Agreguje sygnaly
/// `in_stage`/`in_roster` (ten sam uczestnik moze sie pojawic w obu miejscach
/// — sceny i panelu People) zamiast raportowac dwa razy.
#[derive(Debug, Clone)]
struct RosterTileMeta {
    name: String,
    has_video: bool,
    has_audio: bool,
    in_stage: bool,
    in_roster: bool,
}

pub struct DomObserver {
    shutdown_tx: watch::Sender<bool>,
    state_rx: watch::Receiver<JoinState>,
    listener_join: tokio::task::JoinHandle<()>,
    writer_join: tokio::task::JoinHandle<()>,
}

impl DomObserver {
    /// Wait for the page to reach `JoinState::Joined`. The wait splits into two
    /// budgets so we don't kill the join when the host just leaves the bot in
    /// lobby for a long time:
    ///   * `presence_timeout` — max time to see ANY join surface (Lobby or
    ///     Joined). If we never even see lobby, Teams is broken; we fail fast.
    ///   * `lobby_grace` — max time to spend WAITING in lobby. Long enough
    ///     that a host can admit the bot whenever they get back to the meeting.
    /// Total worst case: presence_timeout + lobby_grace.
    pub async fn wait_for_joined(
        &self,
        presence_timeout: Duration,
        lobby_grace: Duration,
    ) -> Result<()> {
        let mut rx = self.state_rx.clone();

        if matches!(*rx.borrow(), JoinState::Joined) {
            return Ok(());
        }

        let presence_deadline = tokio::time::Instant::now() + presence_timeout;
        loop {
            match *rx.borrow() {
                JoinState::Joined => return Ok(()),
                JoinState::Lobby => break,
                JoinState::NotYet => {}
            }
            let remaining = presence_deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(anyhow!(
                    "dom_observer: nie wykryto ani lobby ani joined w {:?} — Teams zepsuty?",
                    presence_timeout
                ));
            }
            match tokio::time::timeout(remaining, rx.changed()).await {
                Ok(Ok(())) => continue,
                Ok(Err(e)) => return Err(anyhow!("dom_observer: state channel closed: {}", e)),
                Err(_) => continue,
            }
        }

        let lobby_deadline = tokio::time::Instant::now() + lobby_grace;
        loop {
            if matches!(*rx.borrow(), JoinState::Joined) {
                return Ok(());
            }
            let remaining = lobby_deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(anyhow!(
                    "dom_observer: lobby trwa dluzej niz {:?} bez admit — host nieobecny",
                    lobby_grace
                ));
            }
            match tokio::time::timeout(remaining, rx.changed()).await {
                Ok(Ok(())) => continue,
                Ok(Err(e)) => return Err(anyhow!("dom_observer: state channel closed: {}", e)),
                Err(_) => continue,
            }
        }
    }

    pub async fn stop(self) {
        let _ = self.shutdown_tx.send(true);
        // Listener zamyka mpsc tx -> writer wychodzi z petli.
        let _ = self.listener_join.await;
        let _ = self.writer_join.await;
    }
}

/// Register the JS binding, subscribe to the resulting CDP events, and spawn
/// the forwarding tasks. Must be called *before* the page navigates to the
/// Teams URL — `evaluate_on_new_document` already injects the JS observer,
/// and the binding has to exist when that JS runs.
///
/// `speaker_state` jest dalej `RwLock<Option<String>>` — main.rs odczytuje go
/// w STT hot path (cheap clone Option<String>), a contention jest praktycznie
/// zerowy. `roster_snapshot` to ArcSwap: dom_observer publikuje gotowy JSON
/// po kazdej zmianie `known`, main.rs robi tylko `load_full()`.
pub async fn start(
    page: Page,
    router: RouterHandle,
    meeting_key: String,
    bot_name: String,
    speaker_state: SpeakerState,
    roster_snapshot: RosterSnapshotJson,
) -> Result<DomObserver> {
    page.execute(AddBindingParams::new(BINDING_NAME))
        .await
        .map_err(|e| anyhow!("AddBinding({}) failed: {}", BINDING_NAME, e))?;

    let mut listener = page
        .event_listener::<EventBindingCalled>()
        .await
        .map_err(|e| anyhow!("event_listener<BindingCalled> failed: {}", e))?;

    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let (state_tx, state_rx) = watch::channel(JoinState::NotYet);

    // Bounded buffor — przy 50+ uczestnikach jeden DOM scan generuje
    // 1 RosterSnapshot + N×VideoFrame (1 fps per kafelek z kamera). Po stronie
    // writera trzymamy <= EMIT_CONCURRENCY in-flight, reszta czeka w mpsc.
    // 1024 daje bezpieczny zapas (ok. 32 KB pamieci na slot pointerow), zeby
    // burst dolaczen + pierwsza fala klatek wideo nie wpadl od razu w drop.
    let (emit_tx, mut emit_rx) = mpsc::channel::<PendingEmit>(1024);

    // Listener: caly state-machine + sanityzacja rosteru. Zero awaitow QUIC.
    let listener_meeting_key = meeting_key.clone();
    let listener_bot_name = bot_name.clone();
    let listener_speaker_state = speaker_state.clone();
    let listener_roster_snapshot = roster_snapshot.clone();
    let listener_emit_tx = emit_tx.clone();
    let mut listener_shutdown_rx = shutdown_rx.clone();
    let listener_join = tokio::spawn(async move {
        // Per-tile last known display name, keyed on data-tid. Lets us emit
        // `participant_left` with a meaningful name field when a tile vanishes
        // from the DOM. Bot's own tile is filtered by name on emit so we
        // never broadcast ourselves as a remote participant.
        let mut known: HashMap<String, RosterTileMeta> = HashMap::new();
        let mut current_speaker: Option<String> = None;

        // Inicjalny snapshot — pusta lista. Bez tego pierwszy STT przed
        // pierwszym ParticipantJoined dostalby Default::default() z ArcSwap'a
        // (czyli niezdefiniowany kontrakt).
        listener_roster_snapshot.store(Arc::new("[]".to_string()));

        loop {
            tokio::select! {
                changed = listener_shutdown_rx.changed() => {
                    if changed.is_err() || *listener_shutdown_rx.borrow() {
                        tracing::debug!("dom_observer: listener shutdown");
                        return;
                    }
                }
                event = listener.next() => {
                    let Some(ev) = event else {
                        tracing::debug!("dom_observer: binding stream ended");
                        return;
                    };
                    if ev.name != BINDING_NAME {
                        continue;
                    }
                    let parsed: DomEvent = match serde_json::from_str(&ev.payload) {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(payload = %ev.payload, "dom_observer: bad payload: {}", e);
                            continue;
                        }
                    };
                    match parsed {
                        DomEvent::Lobby => {
                            if !matches!(*state_tx.borrow(), JoinState::Lobby | JoinState::Joined) {
                                tracing::info!("dom_observer: lobby");
                                queue_lifecycle(
                                    &listener_emit_tx,
                                    &listener_meeting_key,
                                    LIFECYCLE_LOBBY_WAITING,
                                );
                                let _ = state_tx.send(JoinState::Lobby);
                            }
                        }
                        DomEvent::Joined => {
                            if !matches!(*state_tx.borrow(), JoinState::Joined) {
                                tracing::info!("dom_observer: joined");
                                queue_lifecycle(
                                    &listener_emit_tx,
                                    &listener_meeting_key,
                                    LIFECYCLE_JOINED,
                                );
                                let _ = state_tx.send(JoinState::Joined);
                            }
                        }
                        DomEvent::RosterSnapshot { entries } => {
                            // Snapshot z JS to UNION sceny + panelu People.
                            // Browser_inject deduplikuje po lower-case nazwie,
                            // ale dla bezpieczenstwa robimy merge tutaj — gdyby
                            // ten sam uczestnik trafil dwa razy (np. ze swoim
                            // `data-tid` w MixedStage i z innym id w roster
                            // panel), agregujemy `in_stage`/`in_roster` flagi.
                            let mut next: HashMap<String, RosterTileMeta> = HashMap::new();
                            for tile in &entries {
                                let who = tile.name.clone().unwrap_or_else(|| tile.id.clone());
                                // Teams dla anonimowych dolaczen dodaje sufix
                                // " (Unverified)" / " (External)" do display
                                // name. Bota filtrujemy gdy nazwa jest dokladnie
                                // jego albo gdy ma sufix " (..." — prefix match
                                // wycinal "Botanik" gdy bot nazywal sie "Bot".
                                let is_bot = who == listener_bot_name
                                    || who.starts_with(&format!("{} (", listener_bot_name));
                                if is_bot {
                                    continue;
                                }
                                let entry = next.entry(tile.id.clone()).or_insert_with(|| {
                                    RosterTileMeta {
                                        name: who.clone(),
                                        has_video: false,
                                        has_audio: false,
                                        in_stage: false,
                                        in_roster: false,
                                    }
                                });
                                entry.has_video |= tile.has_video;
                                entry.has_audio |= tile.has_audio;
                                entry.in_stage |= tile.in_stage;
                                entry.in_roster |= tile.in_roster;
                            }
                            for (id, meta) in &next {
                                if !known.contains_key(id) {
                                    tracing::info!(participant = %meta.name, "dom_observer: participant joined");
                                }
                            }
                            for (id, meta) in &known {
                                if !next.contains_key(id) {
                                    tracing::info!(participant = %meta.name, "dom_observer: participant left");
                                }
                            }
                            known = next;
                            publish_roster_snapshot(&listener_roster_snapshot, &known);
                            queue_roster_snapshot(
                                &listener_emit_tx,
                                &listener_meeting_key,
                                &known,
                                &listener_bot_name,
                            );
                        }
                        DomEvent::VideoFrame { participant_id, name, ts_ms, jpeg_b64 } => {
                            // Base64 → bajty raz, na granicy CDP. Przy zlosliwie
                            // wielkim payloadzie (poza limitem) dropujemy bez
                            // dispatchu — chronimy router przed payload bombem.
                            let jpeg = match base64::engine::general_purpose::STANDARD
                                .decode(jpeg_b64.as_bytes())
                            {
                                Ok(b) => b,
                                Err(e) => {
                                    tracing::warn!(
                                        participant = %participant_id,
                                        "video_frame: bad base64: {}",
                                        e
                                    );
                                    continue;
                                }
                            };
                            if jpeg.len() > MAX_VIDEO_FRAME_BYTES {
                                tracing::warn!(
                                    participant = %participant_id,
                                    bytes = jpeg.len(),
                                    limit = MAX_VIDEO_FRAME_BYTES,
                                    "video_frame: drop (rozmiar powyzej limitu)"
                                );
                                continue;
                            }
                            tracing::debug!(
                                participant = %name.clone().unwrap_or_else(|| participant_id.clone()),
                                size_kb = jpeg.len() / 1024,
                                "video frame received"
                            );
                            queue_video_frame(
                                &listener_emit_tx,
                                &listener_meeting_key,
                                participant_id,
                                name,
                                ts_ms,
                                jpeg,
                            );
                        }
                        DomEvent::ActiveSpeaker { id, name } => {
                            let who = name.or_else(|| {
                                id.as_ref().and_then(|tid| known.get(tid).map(|m| m.name.clone()))
                            });
                            if current_speaker == who { continue; }
                            current_speaker = who.clone();
                            // Wpisanie do RwLock<Option<String>> jest swapem — taniej
                            // niz utrzymanie kolejnego ArcSwap, a STT hot path i tak
                            // klonuje Option<String> kazdorazowo.
                            *listener_speaker_state.write().await = current_speaker.clone();
                            if let Some(ref n) = current_speaker {
                                // Active speaker zmienia sie przy kazdym przebiciu
                                // glosu — przy zywej dyskusji 5-10 razy/min. Info
                                // poziom zalewa logi; debug wystarcza do diagnozy.
                                tracing::debug!(speaker = %n, "dom_observer: active speaker");
                            } else {
                                tracing::debug!("dom_observer: active speaker cleared");
                            }
                            // Active speaker jest osobno raportowany do GUI poprzez
                            // następny RosterSnapshot (status="speaking" dla aktualnego
                            // mówcy). Nie wysyłamy osobnego eventu per zmianę mówcy —
                            // przy żywej dyskusji to byłoby 5-10 RT/min bez wartości
                            // dodanej (snapshot i tak idzie z każdym DOM scan'em).
                        }
                        DomEvent::Unknown => {}
                    }
                }
            }
        }
    });

    // Drop'nij oryginalny tx — tylko klony w listenerze trzymaja kanal otwarty.
    // Gdy listener zakonczy, writer zauwazy `recv() == None` i wyjdzie.
    drop(emit_tx);

    let writer_router = router.clone();
    let writer_join = tokio::spawn(async move {
        let semaphore = Arc::new(Semaphore::new(EMIT_CONCURRENCY));
        let mut inflight: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow() {
                        break;
                    }
                }
                maybe = emit_rx.recv() => {
                    let Some(emit) = maybe else { break; };
                    // Permit ogranicza liczbe rownoleglych QUIC RT do EMIT_CONCURRENCY.
                    // Acquire jest async, ale w praktyce blokuje tylko gdy 8+ emitow
                    // wisi w locie — wtedy chcemy backpressure, nie spawn floodu.
                    let permit = match semaphore.clone().acquire_owned().await {
                        Ok(p) => p,
                        Err(_) => break,
                    };
                    let router_inner = writer_router.clone();
                    inflight.spawn(async move {
                        let _permit = permit;
                        dispatch_emit(&router_inner, emit).await;
                    });
                    // Gdy taski sie konczą, sprzątaj — bez tego JoinSet rosnie
                    // przez cala sesje (kazdy zakonczony task zostaje az do drain).
                    while let Some(res) = inflight.try_join_next() {
                        if let Err(e) = res {
                            tracing::debug!("emit task join: {}", e);
                        }
                    }
                }
            }
        }
        // Drainuj pozostale w locie zanim wyjdziemy — inaczej zostaja bez
        // czekania, czesto odcinajac wlasnie wyslane PairingConfirm/Joined.
        while let Some(res) = inflight.join_next().await {
            if let Err(e) = res {
                tracing::debug!("emit task join (drain): {}", e);
            }
        }
    });

    Ok(DomObserver { shutdown_tx, state_rx, listener_join, writer_join })
}

/// Wpycha lifecycle event do mpsc. Przy pelnym kanale (256 wpisow w locie)
/// trace warning i drop — to safety valve gdy router wisi i nie chcemy zjesc
/// pamieci listenera.
fn queue_lifecycle(tx: &mpsc::Sender<PendingEmit>, meeting_key: &str, stage: &str) {
    let event = PendingEmit {
        meeting_key: meeting_key.to_string(),
        timestamp_ms: ts_ms(),
        payload: MeetingEventPayload::LifecycleUpdate {
            stage: stage.to_string(),
            details: None,
        },
    };
    if let Err(e) = tx.try_send(event) {
        tracing::warn!("dom_observer: lifecycle queue full ({}): {}", stage, e);
    }
}

/// Buduje `RosterSnapshot` z aktualnego stanu `known` i wpycha do mpsc.
/// Wszystkie wpisy mają `status="joined"` — opuszczenie sesji jest reprezentowane
/// przez NIEOBECNOŚĆ wpisu w kolejnym snapshocie (bo to snapshot, nie diff).
fn queue_roster_snapshot(
    tx: &mpsc::Sender<PendingEmit>,
    meeting_key: &str,
    known: &HashMap<String, RosterTileMeta>,
    bot_name: &str,
) {
    // Bot jest filtrowany z DOM rosteru (żeby dom_observer nie raportował go
    // jako zdalnego uczestnika), ale GUI musi go widzieć — wstrzykujemy go
    // jako pierwszy entry. ID = nazwa bota (stabilne, unikalne w obrębie sesji).
    let mut entries: Vec<RosterEntry> = Vec::with_capacity(known.len() + 1);
    entries.push(RosterEntry {
        speaker_id: bot_name.to_string(),
        speaker_name: Some(bot_name.to_string()),
        status: "joined".to_string(),
        last_spoken_ago_sec: None,
        has_video: false,
        has_audio: false,
        in_stage: false,
        in_roster: true,
    });
    entries.extend(known.iter().map(|(id, meta)| RosterEntry {
        speaker_id: id.clone(),
        speaker_name: Some(meta.name.clone()),
        status: "joined".to_string(),
        last_spoken_ago_sec: None,
        has_video: meta.has_video,
        has_audio: meta.has_audio,
        in_stage: meta.in_stage,
        in_roster: meta.in_roster,
    }));
    let event = PendingEmit {
        meeting_key: meeting_key.to_string(),
        timestamp_ms: ts_ms(),
        payload: MeetingEventPayload::RosterSnapshot { entries },
    };
    if let Err(e) = tx.try_send(event) {
        // Pelny kanal = router stoi i 1024 sloty zalane. Logujemy ERROR
        // (nie WARN) zeby rozroznic to od pojedynczego retry'a — kolejny
        // snapshot pojdzie automatycznie z nastepnego scan'u, wiec nie
        // tracimy danych dlugofalowo, ale operator musi zobaczyc problem.
        tracing::error!(
            "dom_observer: roster snapshot queue full (size={}): {} — drop",
            known.len(),
            e
        );
    }
}

/// Wpycha klatke wideo do mpsc. Drop przy pelnym kanale jest akceptowalny —
/// wideo idzie 1 fps i kolejna klatka pojdzie za 1s. Liczymy sie z tym, ze
/// pod ciezkim obciazeniem GUI dostanie nizszy fps zamiast OOM lub stall.
fn queue_video_frame(
    tx: &mpsc::Sender<PendingEmit>,
    meeting_key: &str,
    participant_id: String,
    name: Option<String>,
    ts_ms_value: u64,
    jpeg: Vec<u8>,
) {
    let event = PendingEmit {
        meeting_key: meeting_key.to_string(),
        timestamp_ms: ts_ms(),
        payload: MeetingEventPayload::VideoFrame {
            participant_id,
            name,
            ts_ms: ts_ms_value,
            jpeg,
        },
    };
    if let Err(e) = tx.try_send(event) {
        tracing::debug!("dom_observer: video frame queue full: {}", e);
    }
}

/// Wykonuje pojedynczy `send_meeting_event`. Lock na router jest brany na czas
/// jednego emit'a — gdy router rekonektuje sie miedzy eventami, nastepny dostanie
/// nowy Arc<RouterClient>.
async fn dispatch_emit(router: &RouterHandle, emit: PendingEmit) {
    let client = {
        let guard = router.lock().await;
        guard.as_ref().cloned()
    };
    let Some(client) = client else { return; };
    let label = emit_label(&emit.payload);
    if let Err(e) = client
        .send_meeting_event(&emit.meeting_key, emit.timestamp_ms, emit.payload)
        .await
    {
        tracing::warn!("dom_observer emit({}) failed: {}", label, e);
    }
}

/// Przebudowuje JSON snapshot rosteru i atomowo go publikuje. Wykonywane
/// SYNCHRONICZNIE w listenerze przy kazdej zmianie `known` — alokacja jednego
/// Vec<String> + `serde_json::to_string` kosztuje ~10us przy 50 nazwach.
/// Dzieki temu STT hot path bierze gotowy `Arc<String>` jednym `load_full()`.
fn publish_roster_snapshot(slot: &RosterSnapshotJson, known: &HashMap<String, RosterTileMeta>) {
    let mut names: Vec<String> = known
        .values()
        .take(MAX_ROSTER_NAMES)
        .map(|meta| {
            meta.name
                .chars()
                .filter(|c| !c.is_control())
                .take(MAX_ROSTER_NAME_LEN)
                .collect::<String>()
        })
        .filter(|s| !s.is_empty())
        .collect();
    names.sort();
    let json = serde_json::to_string(&names).unwrap_or_else(|_| "[]".to_string());
    slot.store(Arc::new(json));
}

fn emit_label(payload: &MeetingEventPayload) -> &'static str {
    match payload {
        MeetingEventPayload::LifecycleUpdate { .. } => "lifecycle",
        MeetingEventPayload::RosterSnapshot { .. } => "roster",
        MeetingEventPayload::TranscriptEntry { .. } => "transcript",
        MeetingEventPayload::SummaryUpdate { .. } => "summary",
        MeetingEventPayload::ActionItemsUpdate { .. } => "action_items",
        MeetingEventPayload::BackendUpdate { .. } => "backend",
        MeetingEventPayload::VideoFrame { .. } => "video_frame",
        // ParticipantAttributes (emocje + wiek + plec z vision pipeline)
        // emituje router po inferencji na VideoFrame, NIE bot. Wariant
        // wlasnie w match dla kompletnosci zeby protocol-only zmiany w
        // enum nie wywalaly buildu teams-bota.
        MeetingEventPayload::ParticipantAttributes { .. } => "participant_attributes",
    }
}

fn ts_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
