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
//   * Spawns a tokio task that subscribes to `EventBindingCalled` and routes
//     each event JSON to either the meeting protocol (lifecycle / participant
//     update) or an internal state-machine (lobby/joined detection so
//     `wait_for_joined` can replace the polling loop in browser.rs).
//
// The matching JS side lives in `browser_inject.js` — a single MutationObserver
// scheduled through requestAnimationFrame that pushes events when the Teams
// DOM transitions actually happen.
// =============================================================================

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use chromiumoxide::cdp::js_protocol::runtime::{AddBindingParams, EventBindingCalled};
use chromiumoxide::page::Page;
use futures::StreamExt;
use serde::Deserialize;
use tentaflow_protocol::{
    MeetingEventPayload, LIFECYCLE_JOINED, LIFECYCLE_LOBBY_WAITING,
};
use tokio::sync::{watch, Mutex};

use crate::quic_server::RouterClient;

pub type RouterHandle = Arc<Mutex<Option<Arc<RouterClient>>>>;

/// JS-side event name registered as a CDP binding. Must match the constant
/// used inside `browser_inject.js`.
const BINDING_NAME: &str = "__tentaflowEvent";

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
    ParticipantJoined { id: String, name: Option<String> },
    ParticipantLeft { id: String, name: Option<String> },
    ActiveSpeaker { id: Option<String>, name: Option<String> },
    /// Untyped fallback so a JS-side schema bump doesn't kill the listener.
    #[serde(other)]
    Unknown,
}

pub struct DomObserver {
    shutdown_tx: watch::Sender<bool>,
    state_rx: watch::Receiver<JoinState>,
    join: tokio::task::JoinHandle<()>,
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
        let _ = self.join.await;
    }
}

/// Register the JS binding, subscribe to the resulting CDP events, and spawn
/// the forwarding task. Must be called *before* the page navigates to the
/// Teams URL — `evaluate_on_new_document` already injects the JS observer,
/// and the binding has to exist when that JS runs.
pub async fn start(
    page: Page,
    router: RouterHandle,
    meeting_key: String,
    bot_name: String,
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

    let join = tokio::spawn(async move {
        // Per-tile last known display name, keyed on data-tid. Lets us emit
        // `participant_left` with a meaningful name field when a tile vanishes
        // from the DOM. Bot's own tile is filtered by name on emit so we
        // never broadcast ourselves as a remote participant.
        let mut known: HashMap<String, String> = HashMap::new();
        let mut current_speaker: Option<String> = None;

        loop {
            tokio::select! {
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow() {
                        tracing::debug!("dom_observer: shutdown");
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
                                emit_lifecycle(&router, &meeting_key, LIFECYCLE_LOBBY_WAITING).await;
                                let _ = state_tx.send(JoinState::Lobby);
                            }
                        }
                        DomEvent::Joined => {
                            if !matches!(*state_tx.borrow(), JoinState::Joined) {
                                tracing::info!("dom_observer: joined");
                                emit_lifecycle(&router, &meeting_key, LIFECYCLE_JOINED).await;
                                let _ = state_tx.send(JoinState::Joined);
                            }
                        }
                        DomEvent::ParticipantJoined { id, name } => {
                            let who = name.unwrap_or_else(|| id.clone());
                            // Teams dla anonimowych dolaczen dodaje sufix
                            // "(Unverified)" / " (External)" do display name.
                            // Filtrujemy bot przez prefix match — "DevBot"
                            // pasuje do "DevBot (Unverified)" itp.
                            if who.starts_with(&bot_name) {
                                continue;
                            }
                            if known.insert(id.clone(), who.clone()).is_none() {
                                tracing::info!(participant = %who, "dom_observer: participant joined");
                                emit_participant(&router, &meeting_key, &id, &who, "joined").await;
                            }
                        }
                        DomEvent::ParticipantLeft { id, name } => {
                            let who = match known.remove(&id) {
                                Some(prev) => prev,
                                None => name.unwrap_or_else(|| id.clone()),
                            };
                            // Teams dla anonimowych dolaczen dodaje sufix
                            // "(Unverified)" / " (External)" do display name.
                            // Filtrujemy bot przez prefix match — "DevBot"
                            // pasuje do "DevBot (Unverified)" itp.
                            if who.starts_with(&bot_name) {
                                continue;
                            }
                            tracing::info!(participant = %who, "dom_observer: participant left");
                            emit_participant(&router, &meeting_key, &id, &who, "left").await;
                        }
                        DomEvent::ActiveSpeaker { id, name } => {
                            let who = name.or_else(|| id.as_ref().and_then(|tid| known.get(tid).cloned()));
                            if current_speaker == who { continue; }
                            current_speaker = who.clone();
                            if let Some(n) = who {
                                tracing::info!(speaker = %n, "dom_observer: active speaker");
                                emit_participant(&router, &meeting_key, id.as_deref().unwrap_or(&n), &n, "speaking").await;
                            } else {
                                tracing::info!("dom_observer: active speaker cleared");
                            }
                        }
                        DomEvent::Unknown => {}
                    }
                }
            }
        }
    });

    Ok(DomObserver { shutdown_tx, state_rx, join })
}

async fn emit_lifecycle(router: &RouterHandle, meeting_key: &str, stage: &str) {
    let client = {
        let guard = router.lock().await;
        guard.as_ref().cloned()
    };
    let Some(client) = client else { return; };
    let ts = ts_ms();
    if let Err(e) = client
        .send_meeting_event(
            meeting_key,
            ts,
            MeetingEventPayload::LifecycleUpdate {
                stage: stage.to_string(),
                details: None,
            },
        )
        .await
    {
        tracing::warn!("dom_observer emit_lifecycle({}): {}", stage, e);
    }
}

async fn emit_participant(
    router: &RouterHandle,
    meeting_key: &str,
    speaker_id: &str,
    speaker_name: &str,
    status: &str,
) {
    let client = {
        let guard = router.lock().await;
        guard.as_ref().cloned()
    };
    let Some(client) = client else { return; };
    let ts = ts_ms();
    if let Err(e) = client
        .send_meeting_event(
            meeting_key,
            ts,
            MeetingEventPayload::ParticipantUpdate {
                speaker_id: speaker_id.to_string(),
                speaker_name: Some(speaker_name.to_string()),
                status: status.to_string(),
                last_spoken_ago_sec: None,
            },
        )
        .await
    {
        tracing::warn!("dom_observer emit_participant({}, {}): {}", speaker_name, status, e);
    }
}

fn ts_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
