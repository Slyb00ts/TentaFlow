// =============================================================================
// Plik: meeting/mod.rs
// Opis: Moduł Meeting Bot — orkiestracja per-spotkanie. Każde wywołanie
//       `MeetingManager::start_session` spawnuje osobny kontener teams-bot
//       z dynamicznie zaalokowanymi portami (QUIC UDP, VNC TCP, noVNC TCP),
//       zapisuje sesję w DB, zwraca identyfikator sesji + porty do klienta.
//       `leave_session` zatrzymuje kontener, zwalnia porty, oznacza sesję
//       jako ended. Summary jest generowane on-demand przez LLM.
// =============================================================================

pub mod container;
pub mod flow_turn;
pub mod manager;
pub mod native;
pub mod port_pool;

pub use manager::{MeetingManager, SessionDescriptor, StartSessionRequest};

/// Name the sidecar is known by, in Docker and in the native subprocess alike:
/// deterministic from `session_id`, so `leave_session` finds it after a Core
/// restart, and identical to the service name the reverse listener registers —
/// which is what `flow_turn::lookup_owned_session` matches a meeting against.
pub fn container_name(session_id: i64) -> String {
    format!("meeting-bot-{}", session_id)
}
