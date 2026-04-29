// === File: peer_registry/delta.rs — change events and read-only snapshot views ===

use std::sync::Arc;
use std::time::Instant;

use crate::mesh::peer_registry::entry::{
    ArcStr, NodeId, NodeInfoSnapshot, PeerContainerInfo, PeerModelInfo, PeerRole, RetryState,
    TransportHints, TrustStateTag,
};
use crate::mesh::peer_registry::state::{ActivePath, ConnectionState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStateTag {
    Disconnected,
    Connecting,
    Connected,
    Degraded,
    Reconnecting,
    Offline,
}

impl From<&ConnectionState> for ConnectionStateTag {
    fn from(s: &ConnectionState) -> Self {
        match s {
            ConnectionState::Disconnected => ConnectionStateTag::Disconnected,
            ConnectionState::Connecting { .. } => ConnectionStateTag::Connecting,
            ConnectionState::Connected { .. } => ConnectionStateTag::Connected,
            ConnectionState::Degraded { .. } => ConnectionStateTag::Degraded,
            ConnectionState::Reconnecting { .. } => ConnectionStateTag::Reconnecting,
            ConnectionState::Offline { .. } => ConnectionStateTag::Offline,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    Direct,
    Relay,
}

impl From<&ActivePath> for PathKind {
    fn from(p: &ActivePath) -> Self {
        match p {
            ActivePath::Direct { .. } => PathKind::Direct,
            ActivePath::Relay { .. } => PathKind::Relay,
        }
    }
}

#[derive(Debug, Clone)]
pub enum PeerDelta {
    Discovered {
        node_id: NodeId,
    },
    StateChanged {
        node_id: NodeId,
        from: ConnectionStateTag,
        to: ConnectionStateTag,
        at: Instant,
    },
    Heartbeat {
        node_id: NodeId,
        at: Instant,
    },
    NodeInfoUpdated {
        node_id: NodeId,
    },
    TrustChanged {
        node_id: NodeId,
    },
    Forgotten {
        node_id: NodeId,
    },
}

#[derive(Debug, Clone)]
pub enum PeerOutcome {
    NoChange,
    Changed { delta: PeerDelta },
    Created { delta: PeerDelta },
}

#[derive(Debug, Clone)]
pub struct PeerSummary {
    pub node_id: NodeId,
    pub trust: TrustStateTag,
    pub conn_tag: ConnectionStateTag,
    pub conn_path_kind: Option<PathKind>,
    pub since_ms: i64,
    pub hostname: ArcStr,
    pub platform: ArcStr,
    pub role: PeerRole,
    pub last_app_heartbeat_ms: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct PeerDetail {
    pub summary: PeerSummary,
    pub node_info: Option<Arc<NodeInfoSnapshot>>,
    pub models: Arc<[PeerModelInfo]>,
    pub containers: Arc<[PeerContainerInfo]>,
    pub hints: TransportHints,
    pub retry: RetryState,
}
