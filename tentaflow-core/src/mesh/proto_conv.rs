// === File: mesh/proto_conv.rs — PeerRegistry snapshots -> tentaflow_protocol::Mesh* ===

use std::time::{SystemTime, UNIX_EPOCH};

use tentaflow_protocol::{MeshConnState, MeshConnectionInfo, MeshConnectionPathInfo};

use crate::mesh::iroh_manager::ConnectionSnapshot;
use crate::mesh::peer_registry::{ConnectionStateTag, PathKind, PeerSummary};

/// Wall-clock unix epoch in ms used as the reference for converting registry
/// elapsed values into absolute timestamps for the wire.
pub fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn state_tag_to_proto(tag: ConnectionStateTag) -> MeshConnState {
    match tag {
        ConnectionStateTag::Disconnected => MeshConnState::Disconnected,
        ConnectionStateTag::Connecting => MeshConnState::Connecting,
        ConnectionStateTag::Connected => MeshConnState::Connected,
        ConnectionStateTag::Degraded => MeshConnState::Degraded,
        ConnectionStateTag::Reconnecting => MeshConnState::Reconnecting,
        ConnectionStateTag::Offline => MeshConnState::Offline,
    }
}

fn path_kind_to_str(kind: PathKind) -> &'static str {
    match kind {
        PathKind::Direct => "p2p",
        PathKind::Relay => "relay",
    }
}

/// Build the rkyv `MeshConnectionInfo` describing this peer's transport state.
///
/// `summary` is the authoritative source for the connection state machine
/// (state, since, heartbeat). `iroh_snapshot`, when available, fills in the
/// physical path list (every observed iroh path: direct + relay).
pub fn build_conn_info(
    summary: &PeerSummary,
    iroh_snapshot: Option<&ConnectionSnapshot>,
    now_ms: i64,
) -> MeshConnectionInfo {
    let state = state_tag_to_proto(summary.conn_tag);

    // Registry returns elapsed (relative to a monotonic Instant). Convert to
    // absolute unix epoch ms for the wire so GUI can render "since 14:32".
    let since_ms = if summary.since_ms > 0 {
        now_ms.saturating_sub(summary.since_ms)
    } else {
        0
    };
    let last_app_heartbeat_ms = match summary.last_app_heartbeat_ms {
        Some(elapsed) if elapsed >= 0 => now_ms.saturating_sub(elapsed),
        _ => 0,
    };

    // Per-path data from iroh manager (selected path drives `transport`/
    // `address`/`relay_url`). Without an iroh snapshot we fall back to the
    // path kind known to the registry (no physical paths available).
    let (transport, scope, address, relay_url, paths): (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Vec<MeshConnectionPathInfo>,
    ) = if let Some(snap) = iroh_snapshot {
        (
            snap.transport.clone(),
            snap.scope.clone(),
            snap.address.clone(),
            snap.relay_url.clone(),
            snap.paths
                .iter()
                .map(|p| MeshConnectionPathInfo {
                    transport: p.transport.clone(),
                    address: p.address.clone(),
                    selected: p.selected,
                    closed: p.closed,
                })
                .collect(),
        )
    } else {
        let transport = summary
            .conn_path_kind
            .map(|k| path_kind_to_str(k).to_string())
            .unwrap_or_else(|| "unknown".to_string());
        (transport, None, None, None, Vec::new())
    };

    MeshConnectionInfo {
        state,
        transport,
        scope,
        address,
        relay_url,
        paths,
        since_ms,
        last_app_heartbeat_ms,
    }
}
