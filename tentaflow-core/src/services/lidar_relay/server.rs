// =============================================================================
// File: services/lidar_relay/server.rs — owner side (A) of the LiDAR relay
// =============================================================================
//
// Runs on the node that physically owns the robot. Decodes the observer's
// subscribe request, enforces the OWNER-SIDE org-scope gate (the robot must be
// advertised BY THIS LOCAL NODE in the requested org), then subscribes to the
// local `lidar:<robot_id>` StreamHub source (the `LocalLidarStreamSource` pump)
// and pumps the seed frame followed by live frames down the bi-stream as
// length-prefixed `LidarStreamFrame` CBOR blobs.
//
// Mirror of `camera_relay::server` MINUS the `is_init` distinction: every LiDAR
// frame is a complete, self-describing point cloud. The first thing we send is
// the hub's init segment (the latest retained frame) so the observer renders
// immediately; thereafter we drain the broadcast and forward each frame.

use tentaflow_protocol::mesh::{LidarStreamFrame, LidarStreamSubscribePayload};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;

use crate::services::stream_hub::StreamHub;

/// Owner-side org-scope gate: the robot MUST be advertised by THIS node in the
/// requested org. Factored out as a pure predicate over the registry snapshot so
/// it is unit-testable without a live mesh. The observer also gates
/// (`remote_lidar_owner` with org match), but the owner re-checks independently so
/// a forged request from a trusted-but-mistaken peer cannot read a robot in
/// another tenant.
fn robot_owned_by_node(
    robots: &[crate::mesh::robot_dispatch::AdvertisedRobot],
    local_node_id: &str,
    robot_id: &str,
    org_id: &str,
) -> bool {
    robots
        .iter()
        .any(|r| r.node_id == local_node_id && r.robot_id == robot_id && r.org_id == org_id)
}

/// Owner-side relay handler. `payload_bytes` is the CBOR
/// `LidarStreamSubscribePayload` from the observer; `tx` is the BOUNDED bi-stream
/// sink (each item is one CBOR `LidarStreamFrame` body — `iroh_manager` adds the
/// `[u32 len]` prefix and writes it). `local_node_id` is this node's mesh id, used
/// by the org-scope gate.
///
/// Returns when the source closes, the observer disconnects (tx closed), the
/// broadcast lags, the bounded sink fills because the observer is too slow (we cut
/// it so it resubscribes from a fresh seed frame rather than buffering forever),
/// or the gate refuses. Returning drops the StreamHub `SubscriptionHandle`,
/// decrementing the owner-side refcount so the source can detach when the last
/// (local + remote) subscriber is gone.
pub async fn handle(payload_bytes: Vec<u8>, tx: mpsc::Sender<Vec<u8>>, local_node_id: String) {
    let req: LidarStreamSubscribePayload = match tentaflow_protocol::cbor::decode(&payload_bytes) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(target: "lidar::relay", "owner: bad subscribe payload: {e}");
            return;
        }
    };

    let robots = crate::mesh::robot_dispatch::global().all();
    if !robot_owned_by_node(&robots, &local_node_id, &req.robot_id, &req.org_id) {
        tracing::debug!(
            target: "lidar::relay",
            robot_id = %req.robot_id,
            org_id = %req.org_id,
            "owner: refused relay — robot not advertised by this node in this org"
        );
        return;
    }

    // Subscribe to the LOCAL StreamHub (the LocalLidarStreamSource pump), NOT the
    // LidarStreamHub directly, so the owner side benefits from the same fan-out /
    // lifecycle as a local browser subscriber.
    let stream_id = format!("lidar:{}", req.robot_id);
    let handle = match StreamHub::global().subscribe(&stream_id).await {
        Ok(h) => h,
        Err(e) => {
            tracing::debug!(target: "lidar::relay", robot_id = %req.robot_id, "owner: subscribe failed: {e}");
            return;
        }
    };

    // 1) Seed frame first (if any) — the observer caches it as its dynamic init.
    // This MUST be delivered, so we `send().await` (awaitable bounded send) rather
    // than dropping it on a transiently full channel.
    if let Some(init) = handle.init_segment.clone() {
        if send_frame(&tx, init.to_vec()).await.is_err() {
            return;
        }
    }

    // 2) Live frames — drain the broadcast receiver. The `handle` lives on the
    // stack so its Drop (on any return below) releases the owner-side refcount.
    let mut receiver = handle.receiver;
    loop {
        match receiver.recv().await {
            Ok(chunk) => match try_send_frame(&tx, chunk.to_vec()) {
                Ok(()) => {}
                // Observer gone (tx closed) → drop handle, detach source.
                Err(TrySendError::Closed(_)) => return,
                // Observer too slow: the bounded sink is full. Cut it instead of
                // blocking the StreamHub broadcast drain (which would stall every
                // other subscriber on this source). It resubscribes from a fresh
                // seed frame.
                Err(TrySendError::Full(_)) => {
                    tracing::debug!(
                        target: "lidar::relay",
                        robot_id = %req.robot_id,
                        "owner: cutting slow observer (relay sink full)"
                    );
                    return;
                }
            },
            // Lagged: this relay fell behind. Close so the observer resubscribes
            // from a fresh seed frame (latest-wins — never a torn backlog).
            Err(RecvError::Lagged(_)) => return,
            // Source unregistered (robot slot gone) → close.
            Err(RecvError::Closed) => return,
        }
    }
}

/// Encode + await-send the seed frame. Awaitable because the seed is the
/// observer's dynamic init and must not be dropped on a transiently full channel.
async fn send_frame(tx: &mpsc::Sender<Vec<u8>>, data: Vec<u8>) -> Result<(), ()> {
    let bytes = encode_frame(data)?;
    tx.send(bytes).await.map_err(|_| ())
}

/// Encode + non-blocking send one frame. `try_send` so a slow observer surfaces
/// as `Full` (cut it) rather than blocking the broadcast drain.
fn try_send_frame(tx: &mpsc::Sender<Vec<u8>>, data: Vec<u8>) -> Result<(), TrySendError<Vec<u8>>> {
    let bytes = match encode_frame(data) {
        Ok(b) => b,
        // A frame that fails to encode is a permanent error — treat it like a
        // closed sink so the handler tears down.
        Err(()) => return Err(TrySendError::Closed(Vec::new())),
    };
    tx.try_send(bytes)
}

/// Encode one `LidarStreamFrame` to CBOR. The `iroh_manager` branch writes the
/// `[u32 len][bytes]` length prefix itself, so we only emit the CBOR body.
fn encode_frame(data: Vec<u8>) -> Result<Vec<u8>, ()> {
    let frame = LidarStreamFrame { data };
    tentaflow_protocol::cbor::encode(&frame).map_err(|e| {
        tracing::warn!(target: "lidar::relay", "owner: frame encode failed: {e}");
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::robot_dispatch::AdvertisedRobot;

    fn robot(node_id: &str, robot_id: &str, org_id: &str) -> AdvertisedRobot {
        AdvertisedRobot {
            robot_id: robot_id.to_string(),
            package_id: "go2".to_string(),
            kind: Some("quadruped".to_string()),
            node_id: node_id.to_string(),
            org_id: org_id.to_string(),
            camera_id: None,
            status: "online".to_string(),
            battery_percent: None,
            rtt_ms: None,
            capabilities: Vec::new(),
            actions_meta: Vec::new(),
            telemetry: None,
            lidar: None,
        }
    }

    /// The owner gate proceeds only when THIS node advertises the robot in the
    /// requested org; a wrong org or wrong owning node is refused (defence in
    /// depth — the observer gates first, the owner re-checks).
    #[test]
    fn gate_owned_in_org_proceeds_others_refused() {
        let robots = vec![
            robot("node-A", "go2-1", "org-1"),
            robot("node-A", "go2-2", "org-2"),
            robot("node-B", "go2-3", "org-1"),
        ];

        // Owned by this node in the requested org → proceed.
        assert!(robot_owned_by_node(&robots, "node-A", "go2-1", "org-1"));

        // Right robot/node but WRONG org → refuse (cross-tenant).
        assert!(!robot_owned_by_node(&robots, "node-A", "go2-1", "org-2"));

        // Right robot/org but it is owned by ANOTHER node → refuse (we are not
        // the owner; this node must not relay a robot it does not host).
        assert!(!robot_owned_by_node(&robots, "node-A", "go2-3", "org-1"));

        // Unknown robot → refuse.
        assert!(!robot_owned_by_node(&robots, "node-A", "ghost", "org-1"));
    }

    /// Protocol round-trip: the owner-encoded `LidarStreamFrame` CBOR body decodes
    /// back to the same bytes the observer will broadcast.
    #[test]
    fn frame_cbor_round_trip() {
        let payload = b"\x01\x02header-and-packed-f32\xff".to_vec();
        let bytes = encode_frame(payload.clone()).expect("encode");
        let decoded: LidarStreamFrame =
            tentaflow_protocol::cbor::decode(&bytes).expect("decode");
        assert_eq!(decoded.data, payload);
    }

    /// Protocol round-trip for the subscribe payload (observer→owner request).
    #[test]
    fn subscribe_payload_cbor_round_trip() {
        let req = LidarStreamSubscribePayload {
            robot_id: "go2-xyz".to_string(),
            org_id: "org-default".to_string(),
        };
        let bytes = tentaflow_protocol::cbor::encode(&req).expect("encode");
        let decoded: LidarStreamSubscribePayload =
            tentaflow_protocol::cbor::decode(&bytes).expect("decode");
        assert_eq!(decoded.robot_id, req.robot_id);
        assert_eq!(decoded.org_id, req.org_id);
    }
}
