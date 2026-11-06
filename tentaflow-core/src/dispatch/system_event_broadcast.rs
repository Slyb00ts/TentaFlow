// =============================================================================
// Plik: dispatch/system_event_broadcast.rs
// Opis: Globalny tokio broadcast channel dla SystemEventPayload (ServiceStatus
//       + MeshPeerStatus itp.). ws_binary per-connection subskrybuje i pushuje
//       jako unsolicited frame do klienta — GUI dostaje natychmiast info gdy
//       zmienia sie status uslugi QUIC lub peera mesh.
// =============================================================================

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
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
pub fn publish_service_status(service_name: &str, service_type: &str, status: &str, message: &str) {
    publish(SystemEventPayload::ServiceStatusChanged {
        service_name: service_name.to_string(),
        service_type: service_type.to_string(),
        status: status.to_string(),
        message: message.to_string(),
    });
}

/// Helper — publish `MeshPeerStatusChanged` event.
pub fn publish_mesh_peer_status(node_id: &str, hostname: &str, status: &str, message: &str) {
    publish(SystemEventPayload::MeshPeerStatusChanged {
        node_id: node_id.to_string(),
        hostname: hostname.to_string(),
        status: status.to_string(),
        message: message.to_string(),
    });
}

// Authenticated dashboard sockets that receive personal events. A personal
// event pushed while its person has no socket is lost, so a sender that must
// not lose it (process incident notices) asks who is connected first.
static LISTENERS: OnceLock<Mutex<HashMap<String, Vec<(u64, Instant)>>>> = OnceLock::new();
static NEXT_LISTENER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn listeners() -> std::sync::MutexGuard<'static, HashMap<String, Vec<(u64, Instant)>>> {
    LISTENERS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Held for the life of one authenticated socket's event task.
pub struct ListenerGuard {
    user_id: String,
    id: u64,
}

pub fn register_listener(user_id: &str) -> ListenerGuard {
    let id = NEXT_LISTENER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    listeners()
        .entry(user_id.to_string())
        .or_default()
        .push((id, Instant::now()));
    ListenerGuard {
        user_id: user_id.to_string(),
        id,
    }
}

impl Drop for ListenerGuard {
    fn drop(&mut self) {
        let mut all = listeners();
        if let Some(sockets) = all.get_mut(&self.user_id) {
            sockets.retain(|(id, _)| *id != self.id);
            if sockets.is_empty() {
                all.remove(&self.user_id);
            }
        }
    }
}

/// People with a socket that has been open for at least `settled`: a socket that
/// opened a moment ago may not have attached its listener yet.
pub fn live_users(settled: Duration) -> Vec<String> {
    listeners()
        .iter()
        .filter(|(_, sockets)| sockets.iter().any(|(_, since)| since.elapsed() >= settled))
        .map(|(user, _)| user.clone())
        .collect()
}

#[cfg(test)]
mod listener_tests {
    use super::*;

    #[test]
    fn a_person_is_live_only_while_a_settled_socket_is_held() {
        let user = "listener-test-user";
        assert!(!live_users(Duration::ZERO).contains(&user.to_string()));
        let first = register_listener(user);
        let second = register_listener(user);
        assert!(live_users(Duration::ZERO).contains(&user.to_string()));
        assert!(
            !live_users(Duration::from_secs(60)).contains(&user.to_string()),
            "a fresh socket is not settled"
        );
        drop(first);
        assert!(
            live_users(Duration::ZERO).contains(&user.to_string()),
            "the second socket keeps the person live"
        );
        drop(second);
        assert!(!live_users(Duration::ZERO).contains(&user.to_string()));
    }
}
