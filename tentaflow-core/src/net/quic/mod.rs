// =============================================================================
// Plik: net/quic/mod.rs
// Opis: Transport QUIC — serwer i klient z szyfrowaniem TLS 1.3.
//       Wspolna logika TLS wydzielona do modulu tls.
// =============================================================================

pub mod server;
pub mod client;
pub mod tls;
pub mod handler_impls;

pub use client::{QuicClient, QuicConfig};
pub use server::QuicServer;
