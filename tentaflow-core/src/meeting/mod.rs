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
pub mod manager;
pub mod native;
pub mod port_pool;

pub use manager::{MeetingManager, SessionDescriptor, StartSessionRequest};
