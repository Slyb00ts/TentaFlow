// =============================================================================
// Plik: dispatch/audit_broadcast.rs
// Opis: Globalny tokio broadcast channel dla AuditEvent. log_audit wrappery
//       publikuja eventy; ws_binary per-connection subskrybuje i pushuje
//       jako unsolicited frame do klienta (Audit screen sluchanie).
// =============================================================================

use std::sync::OnceLock;
use tentaflow_protocol::AuditEvent;
use tokio::sync::broadcast;

/// Pojemnosc bufora — gdy klient sie laguje, najstarsze sa droppowane.
const CHANNEL_CAPACITY: usize = 256;

static SENDER: OnceLock<broadcast::Sender<AuditEvent>> = OnceLock::new();

fn channel() -> &'static broadcast::Sender<AuditEvent> {
    SENDER.get_or_init(|| {
        let (tx, _rx) = broadcast::channel(CHANNEL_CAPACITY);
        tx
    })
}

/// Publikuje event — kazdy aktywny subscriber go dostanie.
/// No-op gdy brak subscriberow (send returns Err, ignorujemy).
pub fn publish(event: AuditEvent) {
    let _ = channel().send(event);
}

/// Tworzy nowy receiver — kazdy WS connection wola raz.
pub fn subscribe() -> broadcast::Receiver<AuditEvent> {
    channel().subscribe()
}
