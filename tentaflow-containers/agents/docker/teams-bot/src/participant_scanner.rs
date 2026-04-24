// =============================================================================
// File: participant_scanner.rs — periodic Teams DOM scan to emit ParticipantUpdate
// join/leave events even when nobody is speaking.
// =============================================================================

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use chromiumoxide::Page;
use tentaflow_protocol::MeetingEventPayload;
use tokio::sync::watch;

use crate::quic_server::RouterClient;

/// Scan cadence. Teams tile mount/unmount is effectively instant; 3s keeps the
/// roster fresh without hammering the render thread of the headless browser.
const SCAN_INTERVAL: Duration = Duration::from_secs(3);

/// JS evaluated in the Teams page context. Collects `data-tid` of every video
/// tile (both remote participants and the bot itself). Returns a JSON array so
/// chromiumoxide can decode it as `Vec<String>`.
const SCAN_JS: &str = r#"
(function() {
    const tiles = document.querySelectorAll('[data-tid][data-stream-type]');
    const names = new Set();
    for (const tile of tiles) {
        const name = tile.getAttribute('data-tid');
        if (name) { names.add(name); }
    }
    return Array.from(names);
})()
"#;

/// Periodic scanner. Compares the current Teams roster against the previous
/// snapshot and emits `ParticipantUpdate { status: "joined" | "left" }` only on
/// deltas. The bot's own tile is filtered out so we never broadcast ourselves
/// as a participant. Shutdown is cooperative via the watch channel shared with
/// the rest of the main loop.
pub async fn run(
    page: Page,
    router: Arc<tokio::sync::Mutex<Option<Arc<RouterClient>>>>,
    meeting_key: String,
    bot_name: String,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let mut known: HashSet<String> = HashSet::new();
    let mut ticker = tokio::time::interval(SCAN_INTERVAL);
    // Skip the immediate-fire tick so the first scan happens after SCAN_INTERVAL
    // — gives Teams a moment to render tiles after join_meeting returns.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;

    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    tracing::debug!("participant_scanner: shutdown requested");
                    return;
                }
            }
            _ = ticker.tick() => {}
        }

        let eval = match page.evaluate(SCAN_JS).await {
            Ok(v) => v,
            Err(e) => {
                // Page can be closed/navigating (LeaveMeeting, auth expired).
                // Keep looping — the outer main loop owns lifecycle and will
                // stop this task via shutdown_rx when the session ends.
                tracing::warn!("participant_scanner: page.evaluate failed: {}", e);
                continue;
            }
        };

        let names: Vec<String> = match eval.into_value::<Vec<String>>() {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("participant_scanner: decode names failed: {}", e);
                continue;
            }
        };

        let current: HashSet<String> = names
            .into_iter()
            .filter(|n| !n.is_empty() && n != &bot_name)
            .collect();

        let joined: Vec<&String> = current.difference(&known).collect();
        let left: Vec<&String> = known.difference(&current).collect();

        if joined.is_empty() && left.is_empty() {
            continue;
        }

        let client = {
            let guard = router.lock().await;
            guard.as_ref().cloned()
        };
        let Some(client) = client else {
            // No router right now; don't update `known` so the deltas get
            // re-emitted once the router reconnects.
            tracing::debug!("participant_scanner: router unavailable, retry next tick");
            continue;
        };

        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        for name in &joined {
            if let Err(e) = client
                .send_meeting_event(
                    &meeting_key,
                    ts,
                    MeetingEventPayload::ParticipantUpdate {
                        speaker_id: (*name).clone(),
                        speaker_name: Some((*name).clone()),
                        status: "joined".to_string(),
                        last_spoken_ago_sec: None,
                    },
                )
                .await
            {
                tracing::warn!("participant_scanner: emit joined({}) failed: {}", name, e);
            } else {
                tracing::info!(participant = %name, "participant_scanner: joined");
            }
        }

        for name in &left {
            if let Err(e) = client
                .send_meeting_event(
                    &meeting_key,
                    ts,
                    MeetingEventPayload::ParticipantUpdate {
                        speaker_id: (*name).clone(),
                        speaker_name: Some((*name).clone()),
                        status: "left".to_string(),
                        last_spoken_ago_sec: None,
                    },
                )
                .await
            {
                tracing::warn!("participant_scanner: emit left({}) failed: {}", name, e);
            } else {
                tracing::info!(participant = %name, "participant_scanner: left");
            }
        }

        known = current;
    }
}
