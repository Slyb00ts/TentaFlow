// =============================================================================
// File: services/frame_proxy/server.rs — incoming frame proxy request handler.
// =============================================================================
//
// Runs on the node that owns the frame. Looks the `raw_ref` up in the local
// frame_storage LRU, builds the matching FrameProxyResponsePayload (Found /
// NotFound / Unavailable), and pushes it back over the trust-paired mesh
// stream to the requester. The lookup uses `remove()` so a successful
// cross-node fetch enforces the same one-shot semantics as a local pickup —
// the frame cannot be served twice, whether the second consumer is local or
// remote.

use std::sync::Arc;

use tentaflow_protocol::mesh::{
    FrameMetadataWire, FrameProxyRequestPayload, FrameProxyResponsePayload,
};
use tracing::{debug, warn};

use crate::mesh::iroh_manager::IrohMeshManager;
use crate::services::frame_storage::{FramePixelFormat, FrameStorage, RawFrameRef};

fn pixel_format_to_str(fmt: FramePixelFormat) -> &'static str {
    match fmt {
        FramePixelFormat::Rgb24 => "rgb24",
    }
}

/// Look up `raw_ref` in `storage` and build a response payload. Pulled out
/// of the network path so it can be tested without spinning up an iroh
/// endpoint.
pub(crate) fn build_response(
    storage: &FrameStorage,
    payload: &FrameProxyRequestPayload,
) -> FrameProxyResponsePayload {
    let frame_ref = RawFrameRef::from_string(payload.raw_ref.clone());
    match storage.remove(&frame_ref) {
        Some(frame) => {
            let metadata = FrameMetadataWire {
                camera_id: frame.metadata.camera_id.clone(),
                width: frame.metadata.width,
                height: frame.metadata.height,
                pixel_format: pixel_format_to_str(frame.metadata.pixel_format).to_string(),
                timestamp_unix_ms: frame.metadata.timestamp_unix_ms,
            };
            FrameProxyResponsePayload::Found {
                raw_ref: payload.raw_ref.clone(),
                request_id: payload.request_id.clone(),
                bytes: frame.data.to_vec(),
                metadata,
            }
        }
        None => FrameProxyResponsePayload::NotFound {
            raw_ref: payload.raw_ref.clone(),
            request_id: payload.request_id.clone(),
        },
    }
}

/// Full request handler — used by the mesh event loop. Looks up the frame
/// in the process-wide `frame_storage()` singleton, builds the payload,
/// encodes with rkyv, and pushes the response back to `from_node_id`. Any
/// encode or send failure is logged and dropped (the requester's timeout
/// handles the no-reply case).
pub async fn handle_request(
    iroh: Arc<IrohMeshManager>,
    from_node_id: String,
    payload: FrameProxyRequestPayload,
) {
    let storage = crate::services::frame_storage();
    let response = build_response(storage.as_ref(), &payload);

    let bytes = match rkyv::to_bytes::<rkyv::rancor::Error>(&response) {
        Ok(b) => b,
        Err(e) => {
            warn!(
                peer = %from_node_id,
                request_id = %payload.request_id,
                "frame_proxy: failed to encode response: {}",
                e
            );
            return;
        }
    };

    if let Err(e) = iroh.send_frame_proxy_response(&from_node_id, &bytes).await {
        warn!(
            peer = %from_node_id,
            request_id = %payload.request_id,
            "frame_proxy: failed to send response: {}",
            e
        );
    } else {
        debug!(
            peer = %from_node_id,
            raw_ref = %payload.raw_ref,
            request_id = %payload.request_id,
            "frame_proxy: response dispatched"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Instant;

    use super::*;
    use crate::services::frame_storage::{
        FrameMetadata, FramePixelFormat, FrameStorage, StoredFrame,
    };

    fn mk_frame(camera_id: &str, payload: &[u8]) -> StoredFrame {
        StoredFrame {
            metadata: FrameMetadata {
                camera_id: camera_id.into(),
                width: 16,
                height: 8,
                pixel_format: FramePixelFormat::Rgb24,
                timestamp_unix_ms: 42,
                pts: None,
                frame_size_bytes: payload.len(),
            },
            data: Arc::from(payload.to_vec().into_boxed_slice()),
            created_at: Instant::now(),
        }
    }

    #[test]
    fn test_server_handles_request_returns_response() {
        let storage = FrameStorage::new(4);
        let r = storage.insert(mk_frame("cam-1", &[0x11, 0x22, 0x33]));
        let req = FrameProxyRequestPayload {
            raw_ref: r.as_str().to_string(),
            request_id: "rid-found".into(),
        };
        let resp = build_response(&storage, &req);
        match resp {
            FrameProxyResponsePayload::Found {
                raw_ref,
                request_id,
                bytes,
                metadata,
            } => {
                assert_eq!(raw_ref, r.as_str());
                assert_eq!(request_id, "rid-found");
                assert_eq!(bytes, vec![0x11, 0x22, 0x33]);
                assert_eq!(metadata.camera_id, "cam-1");
                assert_eq!(metadata.width, 16);
                assert_eq!(metadata.height, 8);
                assert_eq!(metadata.pixel_format, "rgb24");
                assert_eq!(metadata.timestamp_unix_ms, 42);
            }
            other => panic!("expected Found, got {:?}", other),
        }
        // The one-shot remove semantics must have consumed the entry.
        assert!(storage.get(&r).is_none(), "Found must consume the entry");
    }

    #[test]
    fn test_server_returns_not_found_when_lru_missing() {
        let storage = FrameStorage::new(4);
        let req = FrameProxyRequestPayload {
            raw_ref: "frame_missing".into(),
            request_id: "rid-miss".into(),
        };
        let resp = build_response(&storage, &req);
        match resp {
            FrameProxyResponsePayload::NotFound {
                raw_ref,
                request_id,
            } => {
                assert_eq!(raw_ref, "frame_missing");
                assert_eq!(request_id, "rid-miss");
            }
            other => panic!("expected NotFound, got {:?}", other),
        }
    }
}
