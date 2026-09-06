// =============================================================================
// Plik: net/iroh/mod.rs
// Opis: Transport iroh. Eksportuje `IrohEndpoint` oraz stale ALPN dla trzech
//       kanalow: mesh (node-to-node), pairing (handshake nowego peera), api
//       (browser i zewnetrzne klienty). Rdzen oparty o `iroh::Endpoint`
//       z discovery LAN (mDNS), DHT (pkarr-mainline) oraz relayami z presetu
//       N0 (`*.relay.n0.iroh-canary.iroh.link`) — custom relay mozna wstrzyknac
//       przez config.toml lub DB settings.
// =============================================================================

pub mod endpoint;
pub mod handler;
pub mod pairing;
pub mod relay;
pub mod relay_server;

pub use endpoint::{IrohConfig, IrohEndpoint, IrohEndpointError};
pub use handler::{IrohConnection, IrohStreamError};
pub use pairing::{initiate_pairing_over_iroh, PairingHandler};
pub use relay::{load_relay_url, RELAY_URL_SETTING_KEY};
pub use relay_server::{spawn_relay_server, RelayServerConfig};

/// ALPN dla komunikacji mesh node-to-node. CBOR `MessageBody` z kind discrim
/// 0x10-0x18 (heartbeat, gossip, forwarding).
pub const ALPN_MESH: &[u8] = b"tentaflow-mesh/v1";

/// ALPN dla pairing handshake pierwszego kontaktu. Payload jest CBOR.
pub const ALPN_PAIRING: &[u8] = b"tentaflow-pairing/v2";

/// ALPN dla transferu baseline-adopt po juz-zaufanym pairingu. Joiner dialuje,
/// donor akceptuje; sekwencja `BaselineElect` -> `BaselineAck` ->
/// `BaselineHeader` -> `BaselineChunk`* (z `BaselineChunkAck` per chunk). Ramki
/// to len-prefixed CBOR. Osobny ALPN (nie pairing) bo to inna faza zycia peera
/// (po confirm), inna maszyna stanow, i moze biec rownolegle do mesh heartbeatow.
pub const ALPN_BASELINE: &[u8] = b"tentaflow-baseline/v1";

/// ALPN dla API/browser (GUI, SDK). CBOR `MessageBody` bez mesh discriminantow.
pub const ALPN_API: &[u8] = b"tentaflow-api/v1";

/// ALPN dla bulk-transferu artefaktów modeli (ZIP) między węzłami. Osobny ALPN
/// bo to jeden duży strumień bajtów (open_bi), nie request/response komend mesh —
/// unika tysięcy round-tripów przy przenoszeniu modelu (np. MLX z B na C).
pub const ALPN_ARTIFACT: &[u8] = b"tentaflow-artifact/v1";

/// ALPN dla replikacji TentaBus (M2, PLAN-M2 §1d). Osobny ALPN, nie
/// `ALPN_MESH`: jeden bidi stream per (org, topik, partycja, follower) niosący
/// ramki `bus::replication::frames::ReplFrame` (Hello/Batch/Ack/Heartbeat/…,
/// len-prefixed CBOR — PLAN-M2 §1b), żyjący tak długo jak sama replikacja, nie
/// jak pojedynczy request/response komend mesh. Ten sam wzorzec co
/// `ALPN_ARTIFACT` (bulk stream), inny cel: `ALPN_ARTIFACT` to jeden transfer
/// i koniec, tu strumień trwa cały czas trwania roli leader/follower danej
/// partycji i musi przetrwać wiele batchy bez ponownego handshake'u.
pub const ALPN_BUS: &[u8] = b"tentaflow-bus/v1";
