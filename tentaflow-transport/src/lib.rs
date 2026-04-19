// =============================================================================
// Plik: tentaflow-transport/src/lib.rs
// Opis: Wspolny crate z transportem iroh + rkyv. Uzywany przez tentaflow-core,
//       tentaflow-client/native oraz tentaflow-containers/sidecar. Wystawia:
//       - `framing`   — length-prefixed rkyv frame po bidi streamie iroh
//       - `client`    — `ServiceClient` z auto-reconnect (client→node, node→sidecar)
//       - `server`    — `serve_model_requests` + trait `ModelHandler`
//       - `endpoint`  — helper do budowania `iroh::Endpoint` dla klienta i serwera
//       - `ALPN_SERVICE` — wspolny ALPN `tentaflow-service/v1`
// =============================================================================

pub mod error;
pub mod framing;
pub mod endpoint;
pub mod client;
pub mod server;

pub use error::TransportError;
pub use framing::{read_frame, write_frame, MAX_FRAME_SIZE};
pub use endpoint::{build_client_endpoint, build_server_endpoint, parse_iroh_url, ServerEndpointConfig};
pub use client::{ServiceClient, ServiceClientConfig};
pub use server::{serve_model_requests, HandleError, ModelHandler, ModelOutcome};

/// ALPN dla bidi streamu request/response w sieci TentaFlow.
/// Obowiazuje wszystkich: client→node, node→sidecar.
pub const ALPN_SERVICE: &[u8] = b"tentaflow-service/v1";
