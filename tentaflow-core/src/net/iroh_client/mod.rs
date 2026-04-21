// =============================================================================
// Plik: net/iroh_client/mod.rs
// Opis: Klient iroh uzywany przez TentaFlow do komunikacji z zewnetrznymi
//       serwisami (embedding, memory, meeting transcript, inne nody). Zastepuje
//       dotychczasowy net::quic::QuicClient z warstwa quinn. Ten sam kontrakt
//       API: `connect`, `send_request(ModelRequest)`, `send_request_stream`,
//       tylko transport to iroh::Endpoint z ALPN `tentaflow-service/v1`.
// =============================================================================

pub mod client;

pub use client::{IrohServiceClient, IrohServiceConfig};

// Backward-compat aliasy — istniejace callery uzywaja tych nazw:
pub use client::IrohServiceClient as QuicClient;
pub use client::IrohServiceConfig as QuicConfig;

/// ALPN dla polaczen client-to-service w TentaFlow (embedding, memory, transcript).
pub const ALPN_SERVICE: &[u8] = b"tentaflow-service/v1";
