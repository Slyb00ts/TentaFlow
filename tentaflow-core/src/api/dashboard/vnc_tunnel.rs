// =============================================================================
// File: api/dashboard/vnc_tunnel.rs - same-node WSS <-> websockify TCP bridge.
// =============================================================================
//
// The dashboard front-end opens a streaming subscription `VncTunnelOpenRequest`
// and receives the container's RFB bytes wrapped in `VncTunnelChunk` frames.
// Reverse direction (keyboard/mouse input) arrives as one-shot
// `VncTunnelSendRequest` messages routed through `write_to_tunnel`. The bridge
// task terminates when either side closes: dropping the write half on close
// makes the container-side TCP go EOF, which wakes the read loop and emits
// `VncTunnelStreamEnd` + `push_end` to the subscription.

use std::sync::Arc;

use dashmap::DashMap;
use tentaflow_protocol::{MessageBody, VncTunnelChunk, VncTunnelPayload, VncTunnelStreamEnd};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::dispatch::subscription::{push_chunk, push_end, Subscription};

/// Per-tunnel bridge state owned by `AppState::vnc_tunnels`. Dropping the entry
/// drops the write half; the container-side TCP sees EOF on its reverse read
/// loop and the bridge task wakes up with `Ok(0)` / read-error, finishing
/// cleanup on the reader side.
pub struct VncTunnelEntry {
    /// Owner for the BOLA check on `VncTunnelSendRequest` / `VncTunnelCloseRequest`.
    pub owner_user_id: i64,
    /// Write half of the TCP connection to the container's websockify port.
    /// Wrapped in a Mutex so concurrent `Send` requests serialise writes.
    pub write_half: Arc<Mutex<OwnedWriteHalf>>,
}

/// Maximum simultaneously active tunnels per owning user. Each live tunnel
/// holds one TCP fd and one tokio task, so we bound the fan-out explicitly.
pub const MAX_TUNNELS_PER_USER: usize = 3;

/// Counts how many tunnel entries already belong to `user_id`.
pub fn count_for_user(registry: &DashMap<String, VncTunnelEntry>, user_id: i64) -> usize {
    registry
        .iter()
        .filter(|e| e.value().owner_user_id == user_id)
        .count()
}

/// Opens the TCP bridge to `127.0.0.1:port` and, on success, registers an entry
/// under `tunnel_id` and spawns the read loop that pumps RFB bytes into the
/// subscription. On TCP connect failure we push `VncTunnelStreamEnd` followed
/// by `push_end` - the caller's `ResOpen` frame with `status="ok"` has already
/// been sent, so the browser sees an immediate tear-down.
pub fn spawn_tunnel_bridge(
    registry: Arc<DashMap<String, VncTunnelEntry>>,
    tunnel_id: String,
    owner_user_id: i64,
    port: u16,
    sub: Arc<Subscription>,
) {
    tokio::spawn(async move {
        let stream = match TcpStream::connect(("127.0.0.1", port)).await {
            Ok(s) => s,
            Err(e) => {
                let end = VncTunnelStreamEnd {
                    tunnel_id: tunnel_id.clone(),
                    reason: format!("connect failed: {e}"),
                };
                let _ = push_end(
                    &sub,
                    Some(MessageBody::VncTunnelBody(VncTunnelPayload::StreamEnd(end))),
                );
                return;
            }
        };
        // Disable Nagle - RFB is latency-sensitive for input echo.
        let _ = stream.set_nodelay(true);

        let (mut read_half, write_half) = stream.into_split();
        registry.insert(
            tunnel_id.clone(),
            VncTunnelEntry {
                owner_user_id,
                write_half: Arc::new(Mutex::new(write_half)),
            },
        );

        let mut buf = vec![0u8; 8192];
        let reason = loop {
            match read_half.read(&mut buf).await {
                Ok(0) => break "eof".to_string(),
                Ok(n) => {
                    let chunk = VncTunnelChunk {
                        tunnel_id: tunnel_id.clone(),
                        bytes: buf[..n].to_vec(),
                    };
                    if let Err(e) = push_chunk(
                        &sub,
                        MessageBody::VncTunnelBody(VncTunnelPayload::Chunk(chunk)),
                    ) {
                        // Subscription channel closed/backpressured - tear down.
                        debug!(tunnel_id = %tunnel_id, "vnc tunnel push_chunk stopped: {e}");
                        break "subscription closed".to_string();
                    }
                }
                Err(e) => break format!("read error: {e}"),
            }
        };

        // Drop the map entry first so any concurrent `Send` gets "not found"
        // rather than writing into a half-closed socket.
        registry.remove(&tunnel_id);

        let end = VncTunnelStreamEnd {
            tunnel_id: tunnel_id.clone(),
            reason,
        };
        let _ = push_end(
            &sub,
            Some(MessageBody::VncTunnelBody(VncTunnelPayload::StreamEnd(end))),
        );
    });
}

/// Serialised write from browser input to the container TCP socket. Returns an
/// error string on I/O failure; the caller maps it to `VncTunnelSendResponse`.
pub async fn write_to_tunnel(
    registry: &DashMap<String, VncTunnelEntry>,
    tunnel_id: &str,
    bytes: &[u8],
) -> Result<(), String> {
    let write_half = {
        let Some(entry) = registry.get(tunnel_id) else {
            return Err("tunnel not found".to_string());
        };
        Arc::clone(&entry.value().write_half)
    };
    let mut guard = write_half.lock().await;
    guard
        .write_all(bytes)
        .await
        .map_err(|e| format!("tcp write failed: {e}"))?;
    // Small frames (RFB input events) - flush immediately so the VNC server
    // reacts without waiting for the next read to coalesce them.
    if let Err(e) = guard.flush().await {
        warn!(tunnel_id = %tunnel_id, "vnc tunnel flush error: {e}");
    }
    Ok(())
}

/// Removes the entry; dropping the write half shuts the TCP socket which ends
/// the bridge read loop and emits `VncTunnelStreamEnd` via push_end.
pub fn close_tunnel(registry: &DashMap<String, VncTunnelEntry>, tunnel_id: &str) -> bool {
    registry.remove(tunnel_id).is_some()
}
