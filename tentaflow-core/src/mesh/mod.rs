// =============================================================================
// Plik: mesh/mod.rs
// Opis: Mesh networking — gossip protocol, CRDT, odkrywanie peerow.
// =============================================================================

pub mod gossip;
pub mod crdt;
pub mod crdt_store;
pub mod discovery;
pub mod peer_manager;
pub mod quic_mesh;
pub mod peer_store;
pub mod node_info_collector;
pub mod pipeline;
pub mod security;
pub mod command_executor;
pub mod service_registry;
pub mod network_config;
