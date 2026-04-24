// =============================================================================
// Plik: dispatch/system_event_broadcast.rs
// Opis: Globalny tokio broadcast channel dla SystemEventPayload (ServiceStatus
//       + MeshPeerStatus itp.). ws_binary per-connection subskrybuje i pushuje
//       jako unsolicited frame do klienta — GUI dostaje natychmiast info gdy
//       zmienia sie status uslugi QUIC lub peera mesh.
// =============================================================================

use std::sync::OnceLock;
use tentaflow_protocol::SystemEventPayload;
use tokio::sync::broadcast;

// Buffer event-ow systemowych. Kazdy WS client subskrybuje — przy burst
// eventow (mass pair/unpair, flap mesh, service announce z wielu nodow)
// bufor 256 sie wypelnial i klienci dostawali Lagged(n) → tracimy eventy.
// 8192 = ~2MB pamieci per-process (SystemEventPayload ~200B), zaniedbywalne.
const CHANNEL_CAPACITY: usize = 8192;

static SENDER: OnceLock<broadcast::Sender<SystemEventPayload>> = OnceLock::new();

fn channel() -> &'static broadcast::Sender<SystemEventPayload> {
    SENDER.get_or_init(|| {
        let (tx, _rx) = broadcast::channel(CHANNEL_CAPACITY);
        tx
    })
}

pub fn publish(event: SystemEventPayload) {
    let _ = channel().send(event);
}

pub fn subscribe() -> broadcast::Receiver<SystemEventPayload> {
    channel().subscribe()
}

/// Helper — publish `ServiceStatusChanged` event.
pub fn publish_service_status(
    service_name: &str,
    service_type: &str,
    status: &str,
    message: &str,
) {
    publish(SystemEventPayload::ServiceStatusChanged {
        service_name: service_name.to_string(),
        service_type: service_type.to_string(),
        status: status.to_string(),
        message: message.to_string(),
    });
}

/// Helper — publish `MeshPeerStatusChanged` event.
pub fn publish_mesh_peer_status(
    node_id: &str,
    hostname: &str,
    status: &str,
    message: &str,
) {
    publish(SystemEventPayload::MeshPeerStatusChanged {
        node_id: node_id.to_string(),
        hostname: hostname.to_string(),
        status: status.to_string(),
        message: message.to_string(),
    });
}
