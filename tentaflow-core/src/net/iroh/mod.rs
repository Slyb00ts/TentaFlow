// =============================================================================
// Plik: net/iroh/mod.rs
// Opis: Transport iroh. Eksportuje `IrohEndpoint` oraz stale ALPN dla trzech
//       kanalow: mesh (node-to-node), pairing (handshake nowego peera), api
//       (browser i zewnetrzne klienty). Rdzen oparty o `iroh::Endpoint`
//       z discovery LAN (mDNS), DHT (pkarr-mainline) oraz relayem publicznym
//       `use.iroh.network` z mozliwoscia podmiany na self-hosted.
// =============================================================================

pub mod endpoint;
pub mod handler;
pub mod pairing;
pub mod relay;
pub mod relay_server;

pub use endpoint::{IrohConfig, IrohEndpoint, IrohEndpointError};
pub use handler::{IrohConnection, IrohStreamError};
pub use pairing::{initiate_pairing_over_iroh, PairingHandler, PairingRequest, PairingResponse};
pub use relay::{load_relay_url, DEFAULT_RELAY_URL, RELAY_URL_SETTING_KEY};
pub use relay_server::{spawn_relay_server, RelayServerConfig};

/// ALPN dla komunikacji mesh node-to-node. Rkyv `MessageBody` z kind discrim
/// 0x10-0x18 (heartbeat, CRDT, gossip, forwarding).
pub const ALPN_MESH: &[u8] = b"tentaflow-mesh/v1";

/// ALPN dla pairing handshake (PIN + Ed25519 proof). Rkyv discrim 0x20-0x22.
pub const ALPN_PAIRING: &[u8] = b"tentaflow-pairing/v1";

/// ALPN dla API/browser (GUI, SDK). Rkyv `MessageBody` bez mesh discriminantow.
pub const ALPN_API: &[u8] = b"tentaflow-api/v1";
