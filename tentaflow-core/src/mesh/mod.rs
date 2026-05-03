// =============================================================================
// Plik: mesh/mod.rs
// Opis: Mesh networking — gossip protocol, CRDT, odkrywanie peerow.
// =============================================================================

pub mod admin_ops;
pub mod bandwidth_probe;
pub mod cluster_probe;
pub mod command_executor;
pub mod crdt;
pub mod crdt_store;
pub mod frame_policy;
pub mod gossip;
#[cfg(all(feature = "rdma-probe", target_os = "linux"))]
pub mod ibverbs_ffi;
pub mod inference_proxy;
pub mod iroh_manager;
pub mod liveness;
#[cfg(target_os = "macos")]
pub mod macos_gpu_metrics;
pub mod network_config;
pub mod network_interfaces;
pub mod node_info_collector;
pub mod peer_manager;
pub mod peer_registry;
pub mod peer_store;
pub mod pipeline;
pub mod proto_conv;
#[cfg(any(feature = "rdma-probe", target_os = "macos"))]
pub mod rdma_probe;
pub mod reconnect;
pub mod relay_health;
pub mod security;
