// =============================================================================
// Plik: mesh/mod.rs
// Opis: Mesh networking — odkrywanie peerow, transport i protokol UFP/2.
// =============================================================================

pub mod admin_ops;
pub mod bandwidth_probe;
pub mod cbor;
pub mod cluster_probe;
pub mod command_executor;
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
pub mod peer_registry;
pub mod peer_store;
pub mod pipeline;
pub mod proto_conv;
#[cfg(any(feature = "rdma-probe", target_os = "macos"))]
pub mod rdma_probe;
pub mod reconnect;
pub mod relay_health;
pub mod robot_control;
pub mod robot_dispatch;
pub mod security;
pub mod token_coordinator;
pub mod ufp2;
pub mod vector_transport;
