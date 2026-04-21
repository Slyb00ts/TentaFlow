// =============================================================================
// Plik: dispatch/addon_perm_broadcast.rs
// Opis: Globalny tokio broadcast channel dla AddonPermissionChangedEvent.
//       Handlery modyfikujace uprawnienia/widocznosc addona publikuja event;
//       ws_binary per-connection subskrybuje i pushuje jako unsolicited frame
//       do klienta — GUI odswieza widok uprawnien bez potrzeby reloadu.
// =============================================================================

use std::sync::OnceLock;
use tentaflow_protocol::AddonPermissionChangedEvent;
use tokio::sync::broadcast;

/// Pojemnosc bufora — gdy klient sie laguje, najstarsze sa droppowane.
const CHANNEL_CAPACITY: usize = 128;

static SENDER: OnceLock<broadcast::Sender<AddonPermissionChangedEvent>> = OnceLock::new();

fn channel() -> &'static broadcast::Sender<AddonPermissionChangedEvent> {
    SENDER.get_or_init(|| {
        let (tx, _rx) = broadcast::channel(CHANNEL_CAPACITY);
        tx
    })
}

/// Publikuje event — kazdy aktywny subscriber go dostanie.
/// No-op gdy brak subscriberow (send returns Err, ignorujemy).
pub fn publish(event: AddonPermissionChangedEvent) {
    let _ = channel().send(event);
}

/// Tworzy nowy receiver — kazdy WS connection wola raz.
pub fn subscribe() -> broadcast::Receiver<AddonPermissionChangedEvent> {
    channel().subscribe()
}
