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
use crate::services::frame_storage::{FramePixelFormat, FrameStorage, RawFrameRef, StoredFrame};

fn pixel_format_to_str(fmt: FramePixelFormat) -> &'static str {
    match fmt {
        FramePixelFormat::Rgb24 => "rgb24",
        FramePixelFormat::Nv12 => "nv12",
    }
}

/// Build a `Found` response payload from a resolved frame + the ref it lives at.
fn found(raw_ref: String, request_id: String, frame: StoredFrame) -> FrameProxyResponsePayload {
    let metadata = FrameMetadataWire {
        camera_id: frame.metadata.camera_id.clone(),
        width: frame.metadata.width,
        height: frame.metadata.height,
        pixel_format: pixel_format_to_str(frame.metadata.pixel_format).to_string(),
        timestamp_unix_ms: frame.metadata.timestamp_unix_ms,
    };
    FrameProxyResponsePayload::Found {
        raw_ref,
        request_id,
        bytes: frame.data.to_vec(),
        metadata,
    }
}

/// Resolve a request against `storage` and build a response payload. Pulled out
/// of the network path so it can be tested without spinning up an iroh endpoint.
///
/// The request form is selected by `camera_id`:
/// - by-ref (`camera_id` None/empty) uses `remove()` — one-shot semantics
///   matching a local pickup, so a frame served cross-node cannot be served
///   twice.
/// - latest-for-camera (`camera_id` Some & non-empty) is a NON-consuming read
///   (`latest_for_camera`): the live dashboard tile polls the most recent frame
///   for a robot camera repeatedly, so it must not evict the owner's live feed.
///
/// `advertised_robot_cameras` is the set of camera ids this node currently
/// advertises for its OWN robots. The latest-for-camera path is confined to that
/// set: the owner only serves a camera id that belongs to one of its advertised
/// robot cameras, never an arbitrary local (non-robot) camera. The by-ref path
/// is unaffected — it is gated by the signed `frame_url` the requester holds.
pub(crate) fn build_response(
    storage: &FrameStorage,
    payload: &FrameProxyRequestPayload,
    advertised_robot_cameras: &std::collections::HashSet<String>,
) -> FrameProxyResponsePayload {
    let request_id = payload.request_id.clone();
    match payload.camera_id.as_deref().filter(|c| !c.is_empty()) {
        Some(camera_id) => {
            // Owner-side over-exposure guard: refuse any camera id that is not one
            // of THIS node's advertised robot cameras, so the id-based path can
            // never reach an arbitrary local camera. Fail closed as NotFound — the
            // requester cannot distinguish "not a robot camera" from "no frame",
            // which is intentional (no enumeration of local cameras).
            if !advertised_robot_cameras.contains(camera_id) {
                return FrameProxyResponsePayload::NotFound {
                    raw_ref: camera_id.to_string(),
                    request_id,
                };
            }
            match storage.latest_for_camera(camera_id) {
                Some((frame_ref, frame)) => found(frame_ref.into_string(), request_id, frame),
                // No frame for this camera yet (or evicted). The response carries
                // the camera_id in the raw_ref slot so the requester's NotFound is
                // still diagnosable, mirroring the by-ref miss.
                None => FrameProxyResponsePayload::NotFound {
                    raw_ref: camera_id.to_string(),
                    request_id,
                },
            }
        }
        None => {
            let frame_ref = RawFrameRef::from_string(payload.raw_ref.clone());
            match storage.remove(&frame_ref) {
                Some(frame) => found(payload.raw_ref.clone(), request_id, frame),
                None => FrameProxyResponsePayload::NotFound {
                    raw_ref: payload.raw_ref.clone(),
                    request_id,
                },
            }
        }
    }
}

/// The camera ids this node currently advertises for its OWN robots — the
/// allowlist the latest-for-camera path is confined to. Read from the local
/// entries of the global robot registry (populated by `refresh_local_advertisement`).
fn local_advertised_robot_cameras(local_node_id: &str) -> std::collections::HashSet<String> {
    crate::mesh::robot_dispatch::global()
        .all()
        .into_iter()
        .filter(|r| r.node_id == local_node_id)
        .filter_map(|r| r.camera_id)
        .collect()
}

/// Full request handler — used by the mesh event loop. Looks up the frame
/// in the process-wide `frame_storage()` singleton, builds the payload,
/// encodes with CBOR, and pushes the response back to `from_node_id`. Any
/// encode or send failure is logged and dropped (the requester's timeout
/// handles the no-reply case).
pub async fn handle_request(
    iroh: Arc<IrohMeshManager>,
    from_node_id: String,
    payload: FrameProxyRequestPayload,
) {
    let storage = crate::services::frame_storage();
    let request_id = payload.request_id.clone();
    let advertised = local_advertised_robot_cameras(&iroh.node_id());
    let response = build_response(storage.as_ref(), &payload, &advertised);

    let bytes = match crate::mesh::cbor::encode(&response) {
        Ok(b) => b,
        Err(e) => {
            warn!(
                peer = %from_node_id,
                request_id = %request_id,
                "frame_proxy: failed to encode response: {}",
                e
            );
            return;
        }
    };

    if let Err(e) = iroh.send_frame_proxy_response(&from_node_id, &bytes).await {
        warn!(
            peer = %from_node_id,
            request_id = %request_id,
            "frame_proxy: failed to send response: {}",
            e
        );
    } else {
        debug!(
            peer = %from_node_id,
            request_id = %request_id,
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

    fn cams(ids: &[&str]) -> std::collections::HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

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
            camera_id: None,
        };
        let resp = build_response(&storage, &req, &cams(&[]));
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
            camera_id: None,
        };
        let resp = build_response(&storage, &req, &cams(&[]));
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

    #[test]
    fn test_server_latest_for_camera_is_non_consuming() {
        let storage = FrameStorage::new(4);
        let r = storage.insert(mk_frame("cam-live", &[0xAA, 0xBB]));
        let req = FrameProxyRequestPayload {
            raw_ref: String::new(),
            request_id: "rid-latest".into(),
            camera_id: Some("cam-live".into()),
        };
        // cam-live is one of this node's advertised robot cameras → served.
        let resp = build_response(&storage, &req, &cams(&["cam-live"]));
        match resp {
            FrameProxyResponsePayload::Found {
                raw_ref,
                request_id,
                bytes,
                metadata,
            } => {
                assert_eq!(raw_ref, r.as_str());
                assert_eq!(request_id, "rid-latest");
                assert_eq!(bytes, vec![0xAA, 0xBB]);
                assert_eq!(metadata.camera_id, "cam-live");
            }
            other => panic!("expected Found, got {:?}", other),
        }
        // The live tile polls repeatedly — the frame must survive the fetch.
        assert!(
            storage.get(&r).is_some(),
            "LatestForCamera must NOT consume the entry"
        );
    }

    #[test]
    fn test_server_latest_for_camera_not_found() {
        let storage = FrameStorage::new(4);
        let req = FrameProxyRequestPayload {
            raw_ref: String::new(),
            request_id: "rid-none".into(),
            camera_id: Some("cam-absent".into()),
        };
        // cam-absent IS advertised but has no frame yet → NotFound.
        let resp = build_response(&storage, &req, &cams(&["cam-absent"]));
        match resp {
            FrameProxyResponsePayload::NotFound {
                raw_ref,
                request_id,
            } => {
                assert_eq!(raw_ref, "cam-absent");
                assert_eq!(request_id, "rid-none");
            }
            other => panic!("expected NotFound, got {:?}", other),
        }
    }

    #[test]
    fn test_latest_for_camera_rejects_unadvertised_camera() {
        // A frame exists for cam-secret, but it is NOT in this node's advertised
        // robot camera set → the owner must refuse (fail closed as NotFound) and
        // must NOT consume/serve the frame. Confines the id-based path to robots.
        let storage = FrameStorage::new(4);
        let r = storage.insert(mk_frame("cam-secret", &[0x01, 0x02]));
        let req = FrameProxyRequestPayload {
            raw_ref: String::new(),
            request_id: "rid-deny".into(),
            camera_id: Some("cam-secret".into()),
        };
        let resp = build_response(&storage, &req, &cams(&["cam-robot"]));
        match resp {
            FrameProxyResponsePayload::NotFound {
                raw_ref,
                request_id,
            } => {
                assert_eq!(raw_ref, "cam-secret");
                assert_eq!(request_id, "rid-deny");
            }
            other => panic!("expected NotFound for unadvertised camera, got {:?}", other),
        }
        // The non-robot frame must remain untouched.
        assert!(
            storage.get(&r).is_some(),
            "rejected camera id must not consume or expose the frame"
        );
    }
}
