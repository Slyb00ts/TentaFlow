// =============================================================================
// File: services/frame_proxy/mod.rs — F1b P3.C-2 cross-node frame fetch.
// =============================================================================
//
// When a service's signed `frame_url` resolves to a `raw_ref` that lives on a
// peer node (the local frame_storage LRU has no entry, but mesh-fallback
// HMAC verify identified a known peer as the token's source), the local
// pickup handler asks that peer for the bytes via two mesh messages:
//
//   MESH_MSG_FRAME_PROXY_REQUEST  (A → B)   FrameProxyRequestPayload
//   MESH_MSG_FRAME_PROXY_RESPONSE (B → A)   FrameProxyResponsePayload
//
// The exchange is async over the trust-paired mesh stream — multiple in-flight
// requests share the same connection, so the response is matched to the
// originating request by `request_id` via a process-wide pending map (one
// oneshot::Sender per outstanding request_id). On Found we return the bytes +
// metadata to the caller; on NotFound / Unavailable / timeout we surface a
// matching error.
//
// Server side (`server.rs`) consumes the local frame_storage entry one-shot
// before serializing the bytes so a successful cross-node fetch enforces the
// same single-pickup semantics as a local one. NotFound is sent when the LRU
// has already evicted the frame; Unavailable is reserved for transient errors
// (encode failure, etc.) that the requester might retry.

mod client;
mod server;

pub use client::{
    fetch_from_peer, FrameProxyClient, FrameProxyError, DEFAULT_FETCH_TIMEOUT,
};
pub use server::handle_request;

use std::sync::Arc;
use std::sync::OnceLock;

static FRAME_PROXY_CLIENT: OnceLock<Arc<FrameProxyClient>> = OnceLock::new();

/// Process-wide singleton. The pending map lives here so request-side and
/// event-loop response-side both see the same set of outstanding oneshots.
pub fn frame_proxy_client() -> &'static Arc<FrameProxyClient> {
    FRAME_PROXY_CLIENT.get_or_init(|| Arc::new(FrameProxyClient::new()))
}
