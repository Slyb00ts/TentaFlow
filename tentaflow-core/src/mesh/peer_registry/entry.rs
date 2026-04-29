// === File: peer_registry/entry.rs — peer entry data types and trust/role enums ===

use smallvec::SmallVec;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use crate::mesh::peer_registry::state::ConnectionState;

/// Ed25519 public key fingerprint identifying a peer.
pub type NodeId = [u8; 32];

/// Cheap-to-clone shared string used across snapshot fields.
pub type ArcStr = Arc<str>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerRole {
    Node,
    Edge,
    Relay,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustState {
    Discovered,
    PendingPairing { pin_hash: [u8; 32] },
    Trusted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustStateTag {
    Discovered,
    PendingPairing,
    Trusted,
}

impl From<&TrustState> for TrustStateTag {
    fn from(value: &TrustState) -> Self {
        match value {
            TrustState::Discovered => TrustStateTag::Discovered,
            TrustState::PendingPairing { .. } => TrustStateTag::PendingPairing,
            TrustState::Trusted => TrustStateTag::Trusted,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransportHints {
    pub addresses: SmallVec<[SocketAddr; 4]>,
    pub relay_url: Option<ArcStr>,
    pub hostname_dns: Option<ArcStr>,
}

#[derive(Debug, Clone, Default)]
pub struct RetryState {
    pub attempts: u32,
    pub next_attempt: Option<Instant>,
    pub last_err: Option<ArcStr>,
}

#[derive(Debug, Clone, Default)]
pub struct GpuInfo {
    pub vendor: ArcStr,
    pub model: ArcStr,
    pub vram_used_mb: u64,
    pub vram_total_mb: u64,
}

#[derive(Debug, Clone, Default)]
pub struct NodeInfoSnapshot {
    pub hostname: ArcStr,
    pub platform: ArcStr,
    pub cpu_pct: f32,
    pub ram_used_mb: u64,
    pub ram_total_mb: u64,
    pub gpu: Vec<GpuInfo>,
    pub docker_running: u32,
}

#[derive(Debug, Clone)]
pub struct PeerModelInfo {
    pub id: ArcStr,
    pub size_mb: u64,
}

#[derive(Debug, Clone)]
pub struct PeerContainerInfo {
    pub id: ArcStr,
    pub status: ArcStr,
}

/// Authoritative per-peer state. Fields are kept flat so that callers can
/// snapshot cheaply (Arc<...> for the heavy bits) without locking the whole
/// registry.
pub struct PeerEntry {
    pub node_id: NodeId,
    /// Long-term identity public key for this peer (raw bytes — typically
    /// 32B Ed25519 or 64B Ed25519+X25519). `None` until learned via pairing
    /// confirmation, hello frame, or hydrate-from-db. Required for the
    /// persistence writer to emit `UpsertEntry` (peer_persisted.pubkey is
    /// NOT NULL).
    pub pubkey: Option<Arc<[u8]>>,
    pub hostname: ArcStr,
    pub platform: ArcStr,
    pub role: PeerRole,
    pub trust: TrustState,
    pub conn: ConnectionState,
    pub hints: TransportHints,
    pub last_transport_event: Instant,
    pub last_app_heartbeat: Option<Instant>,
    pub node_info: Option<Arc<NodeInfoSnapshot>>,
    pub models: Arc<[PeerModelInfo]>,
    pub containers: Arc<[PeerContainerInfo]>,
    pub retry: RetryState,
    pub dirty: bool,
    pub persisted_version: u64,
}

impl PeerEntry {
    pub fn new_discovered(node_id: NodeId, hints: TransportHints, now: Instant) -> Self {
        Self {
            node_id,
            pubkey: None,
            hostname: Arc::<str>::from(""),
            platform: Arc::<str>::from(""),
            role: PeerRole::Node,
            trust: TrustState::Discovered,
            conn: ConnectionState::Disconnected,
            hints,
            last_transport_event: now,
            last_app_heartbeat: None,
            node_info: None,
            models: Arc::from(Vec::<PeerModelInfo>::new()),
            containers: Arc::from(Vec::<PeerContainerInfo>::new()),
            retry: RetryState::default(),
            dirty: true,
            persisted_version: 0,
        }
    }
}
