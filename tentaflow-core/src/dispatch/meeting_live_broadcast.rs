// =============================================================================
// Plik: dispatch/meeting_live_broadcast.rs
// Opis: Globalny tokio broadcast channel dla MeetingLiveEvent. Router po
//       sukcesie `persist_meeting_event` publikuje event; ws_binary
//       per-connection subskrybuje i pushuje jako unsolicited frame do
//       klienta — dashboard GUI aktualizuje widok live meetingu bez
//       pollowania. Filtr ownership (owner_user_id sesji) jest stosowany
//       po stronie writer task zanim frame trafi na socket.
// =============================================================================

use std::sync::OnceLock;
use tentaflow_protocol::MeetingLiveEvent;
use tokio::sync::broadcast;

/// Pojemnosc bufora — gdy subscriber sie laguje, najstarsze eventy sa
/// droppowane. 256 bo meeting sypie szybkimi eventami (transcript entry co
/// ~sekunde per speaker + summary co 10-15s).
const CHANNEL_CAPACITY: usize = 256;

static SENDER: OnceLock<broadcast::Sender<MeetingLiveEvent>> = OnceLock::new();

fn channel() -> &'static broadcast::Sender<MeetingLiveEvent> {
    SENDER.get_or_init(|| {
        let (tx, _rx) = broadcast::channel(CHANNEL_CAPACITY);
        tx
    })
}

/// Publikuje event — kazdy aktywny subscriber go dostanie. No-op gdy brak
/// subscriberow (send zwraca Err, ignorujemy).
pub fn publish(event: MeetingLiveEvent) {
    let _ = channel().send(event);
}

/// Tworzy nowy receiver — kazde WS connection wola raz. Writer task filtruje
/// eventy po ownership przed wyslaniem do klienta.
pub fn subscribe() -> broadcast::Receiver<MeetingLiveEvent> {
    channel().subscribe()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tentaflow_protocol::MeetingEventPayload;

    fn sample_event(key: &str) -> MeetingLiveEvent {
        MeetingLiveEvent {
            meeting_key: key.to_string(),
            timestamp_ms: 1_700_000_000_000,
            payload: MeetingEventPayload::SummaryUpdate {
                decisions_text: "d".to_string(),
                summary_text: "s".to_string(),
                model: "m".to_string(),
            },
        }
    }

    /// Filtruje eventy po unikalnym kluczu per-test — testy dziela globalny
    /// SENDER (OnceLock), wiec w paralelnym runie `cargo test` subscriber
    /// dostaje eventy z innych testow. Lapiemy tylko wlasne po meeting_key.
    async fn recv_with_key(
        rx: &mut broadcast::Receiver<MeetingLiveEvent>,
        key: &str,
    ) -> MeetingLiveEvent {
        loop {
            match rx.recv().await {
                Ok(ev) if ev.meeting_key == key => return ev,
                Ok(_) => continue,
                Err(e) => panic!("recv failed: {:?}", e),
            }
        }
    }

    #[tokio::test]
    async fn publish_without_subscribers_is_noop() {
        // Bez subscribera send zwraca Err w broadcast channel — my ignorujemy.
        // Test sprawdza ze publish nie panikuje i nie blokuje.
        publish(sample_event("noop-nobody-listens"));
    }

    #[tokio::test]
    async fn publish_delivers_to_active_subscriber() {
        let mut rx = subscribe();
        publish(sample_event("delivers-abc"));
        let got = recv_with_key(&mut rx, "delivers-abc").await;
        assert_eq!(got.meeting_key, "delivers-abc");
    }

    #[tokio::test]
    async fn multiple_subscribers_all_receive() {
        let mut rx1 = subscribe();
        let mut rx2 = subscribe();
        publish(sample_event("multi-both"));
        assert_eq!(
            recv_with_key(&mut rx1, "multi-both").await.meeting_key,
            "multi-both"
        );
        assert_eq!(
            recv_with_key(&mut rx2, "multi-both").await.meeting_key,
            "multi-both"
        );
    }
}
