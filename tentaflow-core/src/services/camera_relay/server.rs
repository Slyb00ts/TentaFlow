// =============================================================================
// File: services/camera_relay/server.rs — owner side (A) of the camera relay
// =============================================================================
//
// Runs on the node that physically owns the camera. Decodes the observer's
// subscribe request, enforces the OWNER-SIDE org-scope gate (the camera must
// belong to a robot advertised BY THIS LOCAL NODE in the requested org), then
// subscribes to the local `camera:<id>` StreamHub source and pumps the init
// segment followed by media chunks down the bi-stream as length-prefixed
// `CameraStreamFrame` CBOR blobs.

use tentaflow_protocol::mesh::{CameraStreamFrame, CameraStreamSubscribePayload};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;

use crate::services::stream_hub::StreamHub;

/// Owner-side relay handler. `payload_bytes` is the CBOR
/// `CameraStreamSubscribePayload` from the observer; `tx` is the BOUNDED
/// bi-stream sink (each item is one CBOR `CameraStreamFrame` body —
/// `iroh_manager` adds the `[u32 len]` prefix and writes it). `local_node_id` is
/// this node's mesh id, used by the org-scope gate.
///
/// Returns when the source closes, the observer disconnects (tx closed), the
/// broadcast lags, the bounded sink fills because the observer is too slow (we
/// cut it so it resubscribes from a fresh init segment rather than buffering
/// forever), or the gate refuses. Returning drops the StreamHub
/// `SubscriptionHandle`, decrementing the owner-side refcount so the camera mux
/// branch can detach when the last (local + remote) subscriber is gone.
pub async fn handle(payload_bytes: Vec<u8>, tx: mpsc::Sender<Vec<u8>>, local_node_id: String) {
    let req: CameraStreamSubscribePayload = match tentaflow_protocol::cbor::decode(&payload_bytes) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(target: "camera::relay", "owner: bad subscribe payload: {e}");
            return;
        }
    };

    // Owner-side org-scope gate: the camera MUST belong to a robot advertised by
    // THIS node in the requested org. The observer also gates (remote_camera_owner
    // with org match), but the owner re-checks independently so a forged request
    // from a trusted-but-mistaken peer cannot read a camera in another tenant.
    let owned = crate::mesh::robot_dispatch::global().all().into_iter().any(|r| {
        r.node_id == local_node_id
            && r.camera_id.as_deref() == Some(req.camera_id.as_str())
            && r.org_id == req.org_id
    });
    if !owned {
        tracing::debug!(
            target: "camera::relay",
            camera_id = %req.camera_id,
            org_id = %req.org_id,
            "owner: refused relay — camera not advertised by this node in this org"
        );
        return;
    }

    let stream_id = format!("camera:{}", req.camera_id);
    let handle = match StreamHub::global().subscribe(&stream_id).await {
        Ok(h) => h,
        Err(e) => {
            tracing::debug!(target: "camera::relay", camera_id = %req.camera_id, "owner: subscribe failed: {e}");
            return;
        }
    };

    // 1) Init segment first (if any) — the observer seals it before media. This
    // MUST be delivered, so we `send().await` (awaitable bounded send) rather
    // than dropping it on a transiently full channel.
    if let Some(init) = handle.init_segment.clone() {
        if send_init_frame(&tx, init.to_vec()).await.is_err() {
            return;
        }
    }

    // 2) Media chunks — drain the broadcast receiver. The `handle` lives on the
    // stack so its Drop (on any return below) releases the owner-side refcount.
    let mut receiver = handle.receiver;
    loop {
        match receiver.recv().await {
            Ok(chunk) => match try_send_media_frame(&tx, chunk.to_vec()) {
                Ok(()) => {}
                // Observer gone (tx closed) → drop handle, detach branch.
                Err(TrySendError::Closed(_)) => return,
                // Observer too slow: the bounded sink is full. Cut it instead of
                // blocking the StreamHub broadcast drain (which would stall every
                // other subscriber on this source). It resubscribes from a fresh
                // init segment rather than appending a torn one.
                Err(TrySendError::Full(_)) => {
                    tracing::debug!(
                        target: "camera::relay",
                        camera_id = %req.camera_id,
                        "owner: cutting slow observer (relay sink full)"
                    );
                    return;
                }
            },
            // Lagged: this relay fell behind. Close so the observer resubscribes
            // from a fresh init segment instead of appending a torn segment.
            Err(RecvError::Lagged(_)) => return,
            // Source unregistered (camera session gone) → close.
            Err(RecvError::Closed) => return,
        }
    }
}

/// Encode + await-send the init frame. Awaitable because the init segment is
/// mandatory and must not be dropped on a transiently full channel.
async fn send_init_frame(tx: &mpsc::Sender<Vec<u8>>, data: Vec<u8>) -> Result<(), ()> {
    let bytes = encode_frame(true, data)?;
    tx.send(bytes).await.map_err(|_| ())
}

/// Encode + non-blocking send one media frame. `try_send` so a slow observer
/// surfaces as `Full` (cut it) rather than blocking the broadcast drain.
fn try_send_media_frame(
    tx: &mpsc::Sender<Vec<u8>>,
    data: Vec<u8>,
) -> Result<(), TrySendError<Vec<u8>>> {
    let bytes = match encode_frame(false, data) {
        Ok(b) => b,
        // A frame that fails to encode is a permanent error — treat it like a
        // closed sink so the handler tears down.
        Err(()) => return Err(TrySendError::Closed(Vec::new())),
    };
    tx.try_send(bytes)
}

/// Encode one `CameraStreamFrame` to CBOR. The `iroh_manager` branch writes the
/// `[u32 len][bytes]` length prefix itself, so we only emit the CBOR body.
fn encode_frame(is_init: bool, data: Vec<u8>) -> Result<Vec<u8>, ()> {
    let frame = CameraStreamFrame { is_init, data };
    tentaflow_protocol::cbor::encode(&frame).map_err(|e| {
        tracing::warn!(target: "camera::relay", "owner: frame encode failed: {e}");
    })
}
