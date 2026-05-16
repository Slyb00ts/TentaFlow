// =============================================================================
// File: services/frame_proxy/client.rs — outgoing frame proxy client.
// =============================================================================
//
// Requester side. `fetch_from_peer` registers a oneshot in the pending map
// keyed by request_id, sends the rkyv-encoded FrameProxyRequestPayload over
// the trust-paired mesh stream, and awaits the matching response with a
// timeout. The event loop in `mesh/pipeline.rs` calls
// `handle_response` when a FrameProxyResponseReceived event lands; that
// path looks up the request_id and resolves the oneshot.

use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use thiserror::Error;
use tokio::sync::oneshot;
use uuid::Uuid;

use tentaflow_protocol::mesh::{
    FrameMetadataWire, FrameProxyRequestPayload, FrameProxyResponsePayload,
};

use crate::mesh::iroh_manager::IrohMeshManager;

/// Default wall-clock timeout for a single fetch. Frame proxy is a
/// best-effort optimisation — if the peer cannot answer in 5 s the caller
/// should fall back to surfacing the pickup error (HTTP 404 / 503).
pub const DEFAULT_FETCH_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Error)]
pub enum FrameProxyError {
    #[error("peer reported NotFound for raw_ref {0}")]
    NotFound(String),
    #[error("peer reported Unavailable for raw_ref {raw_ref}: {reason}")]
    Unavailable { raw_ref: String, reason: String },
    #[error("frame proxy request timed out after {0:?}")]
    Timeout(Duration),
    #[error("failed to encode FrameProxyRequest: {0}")]
    Encode(String),
    #[error("failed to send FrameProxyRequest to {peer}: {source}")]
    Send {
        peer: String,
        #[source]
        source: anyhow::Error,
    },
    #[error("pending oneshot dropped before response arrived")]
    OneshotDropped,
}

pub struct FrameProxyClient {
    pending: Arc<DashMap<String, oneshot::Sender<FrameProxyResponsePayload>>>,
}

impl FrameProxyClient {
    pub(super) fn new() -> Self {
        Self {
            pending: Arc::new(DashMap::new()),
        }
    }

    /// Number of outstanding requests — diagnostic only.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Register a freshly generated request_id and return its receiver. The
    /// caller is expected to forward the request_id in the
    /// FrameProxyRequestPayload it sends to the peer.
    fn register(&self, request_id: &str) -> oneshot::Receiver<FrameProxyResponsePayload> {
        let (tx, rx) = oneshot::channel();
        self.pending.insert(request_id.to_string(), tx);
        rx
    }

    /// Cancel a registered request (timeout, send failure). Drops the sender
    /// half so a late-arriving response is harmlessly discarded by
    /// `handle_response`.
    fn cancel(&self, request_id: &str) {
        self.pending.remove(request_id);
    }

    /// Called by the mesh event loop when a FrameProxyResponseReceived event
    /// lands. Looks up the request_id and resolves the oneshot. A response
    /// for an unknown request_id (late arrival after timeout, duplicate, or
    /// stray) is silently dropped — by design the requester owns the
    /// timeout contract, not the responder.
    pub fn handle_response(&self, payload: FrameProxyResponsePayload) {
        let request_id = match &payload {
            FrameProxyResponsePayload::Found { request_id, .. }
            | FrameProxyResponsePayload::NotFound { request_id, .. }
            | FrameProxyResponsePayload::Unavailable { request_id, .. } => request_id.clone(),
        };
        if let Some((_, tx)) = self.pending.remove(&request_id) {
            let _ = tx.send(payload);
        }
    }
}

/// Top-level entry point. Sends the request, awaits the matching response,
/// returns the bytes + metadata on Found. The pending oneshot is removed
/// from the map on every exit path (Ok or Err) so the map cannot grow
/// unbounded under repeated timeouts.
pub async fn fetch_from_peer(
    iroh: &IrohMeshManager,
    peer_id: &str,
    raw_ref: &str,
    timeout: Duration,
) -> Result<(Vec<u8>, FrameMetadataWire), FrameProxyError> {
    let client = super::frame_proxy_client();
    let request_id = format!("fp-{}", Uuid::new_v4());

    let rx = client.register(&request_id);

    let request = FrameProxyRequestPayload {
        raw_ref: raw_ref.to_string(),
        request_id: request_id.clone(),
    };
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&request)
        .map_err(|e| {
            client.cancel(&request_id);
            FrameProxyError::Encode(e.to_string())
        })?;

    if let Err(e) = iroh.send_frame_proxy_request(peer_id, &bytes).await {
        client.cancel(&request_id);
        return Err(FrameProxyError::Send {
            peer: peer_id.to_string(),
            source: e,
        });
    }

    let resp = match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(resp)) => resp,
        Ok(Err(_)) => {
            client.cancel(&request_id);
            return Err(FrameProxyError::OneshotDropped);
        }
        Err(_) => {
            client.cancel(&request_id);
            return Err(FrameProxyError::Timeout(timeout));
        }
    };

    match resp {
        FrameProxyResponsePayload::Found {
            bytes, metadata, ..
        } => Ok((bytes, metadata)),
        FrameProxyResponsePayload::NotFound { raw_ref, .. } => {
            Err(FrameProxyError::NotFound(raw_ref))
        }
        FrameProxyResponsePayload::Unavailable {
            raw_ref, reason, ..
        } => Err(FrameProxyError::Unavailable { raw_ref, reason }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_pending_map_register_resolve() {
        let client = FrameProxyClient::new();
        let rx = client.register("rid-1");
        assert_eq!(client.pending_len(), 1);

        let payload = FrameProxyResponsePayload::NotFound {
            raw_ref: "ref-a".into(),
            request_id: "rid-1".into(),
        };
        client.handle_response(payload);

        let got = rx.await.expect("oneshot resolved");
        match got {
            FrameProxyResponsePayload::NotFound { raw_ref, request_id } => {
                assert_eq!(raw_ref, "ref-a");
                assert_eq!(request_id, "rid-1");
            }
            other => panic!("expected NotFound, got {:?}", other),
        }
        assert_eq!(client.pending_len(), 0);
    }

    #[tokio::test]
    async fn test_pending_map_timeout() {
        let client = FrameProxyClient::new();
        let rx = client.register("rid-2");
        let res = tokio::time::timeout(Duration::from_millis(50), rx).await;
        assert!(res.is_err(), "must time out when no response arrives");
        // After timeout the entry is still in the map until cancel() is
        // called — emulate the fetch_from_peer cleanup path.
        client.cancel("rid-2");
        assert_eq!(client.pending_len(), 0);
    }

    #[tokio::test]
    async fn test_response_for_unknown_request_id_is_dropped() {
        let client = FrameProxyClient::new();
        // No register for "rid-orphan" — handle_response must be a no-op.
        client.handle_response(FrameProxyResponsePayload::NotFound {
            raw_ref: "x".into(),
            request_id: "rid-orphan".into(),
        });
        assert_eq!(client.pending_len(), 0);
    }
}
