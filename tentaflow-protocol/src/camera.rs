// =============================================================================
// File: camera.rs
// Purpose: Admin-side binary protocol for camera discovery + add (F2 P7.a).
//          Packed into a single `CameraAdminPayload` inner enum so the whole
//          camera-wizard surface burns one `MessageBody` discriminant slot
//          (CBOR 0.8 caps `MessageBody` at 256 variants — see profiling.rs
//          / vision.rs for the same pack pattern).
// =============================================================================

use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

/// One device returned by ONVIF WS-Discovery. Mirrors the host-fn
/// `DiscoveredCameraOut` shape so the dashboard wizard and addons share
/// the same field set; the wire-level type lives in this crate so the
/// `tentaflow-core` crate can be built without dragging the dashboard
/// schema into the addon ABI.
#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct DiscoveredCameraInfo {
    /// Source IP of the ProbeMatch UDP packet (the camera's NIC address).
    pub address: String,
    /// ONVIF device-service URLs advertised by the camera, e.g.
    /// `http://192.168.1.50/onvif/device_service`. Usually one entry.
    pub xaddrs: Vec<String>,
    /// ONVIF type tokens, e.g. `dn:NetworkVideoTransmitter`.
    pub types: Vec<String>,
    /// Best-effort manufacturer extracted from scopes (empty if absent).
    pub manufacturer: String,
    /// Best-effort model extracted from scopes (empty if absent).
    pub model: String,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct CameraDiscoverRequest {}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct CameraDiscoverResponse {
    pub discovered: Vec<DiscoveredCameraInfo>,
}

/// Request to add a discovered ONVIF camera as a managed session. The
/// dashboard never re-uses `camera_add_v1` (host-fn / addon-scoped); this
/// admin RPC carries the operator's user-session credentials and binds the
/// resulting row to the org from `ctx.org_context`.
#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct CameraAddOnvifRequest {
    /// Human-readable label shown in the UI.
    pub display_name: String,
    /// ONVIF device-service URL (e.g. `http://192.168.1.50/onvif/device_service`).
    pub device_service_url: String,
    /// Plaintext ONVIF username. Travels over the TLS-protected admin
    /// transport (WT/WS); the server encrypts it via `credentials_cipher`
    /// before persisting and never returns it to the client.
    pub username: String,
    /// Plaintext ONVIF password — same handling rules as `username`.
    pub password: String,
    /// Profile token to bind, or `None` to pick the first profile returned
    /// by `GetProfiles`.
    pub profile_token: Option<String>,
    /// Target capture FPS (1..=60). When `None`, the server picks a sensible
    /// default (15 fps to match the host-fn surface).
    pub target_fps: Option<u32>,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct CameraAddOnvifResponse {
    pub camera_id: String,
    /// RTSP URI derived from `GetStreamUri` against the chosen profile.
    pub rtsp_url: String,
    /// Profile token actually bound (echoes the request when set, otherwise
    /// the first profile that the device advertised).
    pub profile_token: String,
}

/// Live-preview frame URL request — the dashboard `<tf-live-camera-tile>`
/// custom element calls this directly so the panel does not round-trip
/// through the addon WASM `__tentaflow.frame_url__` action. The handler
/// authenticates as the user session, gates on `camera.read`, validates the
/// camera id (UUID v4), enforces a per-user rate limit, and mints a signed
/// `/frames/<ref>?token=...` URL against the latest frame stored for that
/// camera in the in-memory LRU.
#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct CameraFrameUrlRequest {
    /// Camera id (UUID v4 textual form, 36 chars). Strict validation in the
    /// handler — non-UUID values fail with BadRequest before any DB hit.
    pub camera_id: String,
    /// Requested TTL in seconds. Dispatch contract: 5..=300. Out-of-range
    /// values yield BadRequest; the response `expires_at_ms` echoes the
    /// actually-minted expiry.
    pub ttl_secs: u32,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct CameraFrameUrlResponse {
    /// Same-origin signed URL (`/frames/<ref>?token=&exp=&ref=`).
    pub signed_url: String,
    /// Absolute expiry as Unix milliseconds. Mirrors the host-fn
    /// `UrlOut.expires_unix_ms` shape so dashboard + addon paths stay
    /// schema-compatible.
    pub expires_at_ms: i64,
}

/// Subscribe to a per-camera detection overlay stream (server→client). The
/// dashboard `<tf-live-camera-tile>` opens one of these per VISIBLE tile and
/// receives a long-lived stream of `CameraDetectionsFrame` chunks until it
/// cancels (`MetaCancelStream`) or disconnects. The stream is best-effort,
/// latest-wins: when the per-camera broadcast ring overruns a slow subscriber
/// the server drops the lagged frames silently rather than ending the stream.
#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct CameraDetectionsSubscribeRequest {
    /// Camera id (`cam_<uuid v4>`, 40 chars). The handler validates the format
    /// and gates on `camera.read` + org isolation before subscribing.
    pub camera_id: String,
}

/// One detected object on a frame. Mirrors `detection_bus::Detection` field
/// for field so the server-side mapping is an allocation-cheap copy.
#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct DetectionItem {
    /// Class name from our set (e.g. `tablica_adr`).
    pub klasa: String,
    /// `[x, y, w, h]` NORMALIZED 0..1 relative to the frame; the frontend
    /// rescales to the `<video>` element size.
    pub bbox: [f32; 4],
    /// Detection confidence 0..1.
    pub score: f32,
    /// State features (e.g. `["uszkodzona"]`); may be empty.
    pub stan: Vec<String>,
    /// OCR read or `None` (serialized as `null`).
    pub tekst: Option<String>,
    /// Mean OCR confidence (0..1) of the winning `tekst`, or `None` when there
    /// is no text. Lets the dashboard show HOW confident a plate/ADR read is.
    #[serde(default)]
    pub tekst_conf: Option<f32>,
    /// Stable tracking id from the IOU tracker. 0 = unassigned.
    #[serde(default)]
    pub track_id: u32,
    /// Box-center velocity, normalized units/s (X axis). 0 when no time base.
    #[serde(default)]
    pub vx: f32,
    /// Box-center velocity, normalized units/s (Y axis).
    #[serde(default)]
    pub vy: f32,
}

/// One streamed detection frame (server→client chunk). Carries the normalized
/// detections for a single camera frame. `ts_ms` is Unix epoch milliseconds —
/// kept as `u64` for the BigInt-tolerant JS decoders.
#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct CameraDetectionsFrame {
    pub camera_id: String,
    pub ts_ms: u64,
    /// Frame PTS in the media timeline (nanoseconds) — shared clock with the MSE
    /// init segment (`mux_base_pts_ns`) so the client anchors the overlay to the
    /// exact video frame instead of wall-clock. `None` for wall-clock-only sources.
    #[serde(default)]
    pub pts_ns: Option<u64>,
    /// Total per-frame processing time in ms (detection + OCR + state classify).
    /// The client renders this as a latency badge; `0` when unknown.
    #[serde(default)]
    pub proc_ms: u32,
    pub items: Vec<DetectionItem>,
}

/// Inner-enum pack — keeps every admin camera RPC in a single
/// `MessageBody::CameraAdminBody` slot (CBOR 256-variant budget). Matches the
/// `ProfilingPayload` / `VisionInferPayload` pattern.
#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub enum CameraAdminPayload {
    DiscoverRequest(CameraDiscoverRequest),
    DiscoverResponse(CameraDiscoverResponse),
    AddOnvifRequest(CameraAddOnvifRequest),
    AddOnvifResponse(CameraAddOnvifResponse),
    FrameUrlRequest(CameraFrameUrlRequest),
    FrameUrlResponse(CameraFrameUrlResponse),
    DetectionsSubscribeRequest(CameraDetectionsSubscribeRequest),
    DetectionsFrame(CameraDetectionsFrame),
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! round_trip {
        ($ty:ty, $value:expr) => {{
            let bytes = crate::cbor::encode(&$value).expect("encode");
            crate::cbor::decode::<$ty>(&bytes).expect("decode")
        }};
    }

    #[test]
    fn discover_request_round_trip() {
        let v = CameraAdminPayload::DiscoverRequest(CameraDiscoverRequest {});
        assert_eq!(round_trip!(CameraAdminPayload, v.clone()), v);
    }

    #[test]
    fn discover_response_round_trip() {
        let v = CameraAdminPayload::DiscoverResponse(CameraDiscoverResponse {
            discovered: vec![DiscoveredCameraInfo {
                address: "192.168.1.50".into(),
                xaddrs: vec!["http://192.168.1.50/onvif/device_service".into()],
                types: vec!["dn:NetworkVideoTransmitter".into()],
                manufacturer: "ACME".into(),
                model: "Cam-9000".into(),
            }],
        });
        assert_eq!(round_trip!(CameraAdminPayload, v.clone()), v);
    }

    #[test]
    fn add_onvif_request_round_trip() {
        let v = CameraAdminPayload::AddOnvifRequest(CameraAddOnvifRequest {
            display_name: "Front Door".into(),
            device_service_url: "http://192.168.1.50/onvif/device_service".into(),
            username: "admin".into(),
            password: "hunter2".into(),
            profile_token: Some("MainProfile".into()),
            target_fps: Some(15),
        });
        assert_eq!(round_trip!(CameraAdminPayload, v.clone()), v);
    }

    #[test]
    fn add_onvif_response_round_trip() {
        let v = CameraAdminPayload::AddOnvifResponse(CameraAddOnvifResponse {
            camera_id: "cam_abc".into(),
            rtsp_url: "rtsp://192.168.1.50:554/onvif/profile1/media.smp".into(),
            profile_token: "MainProfile".into(),
        });
        assert_eq!(round_trip!(CameraAdminPayload, v.clone()), v);
    }

    // F2 P7.a-bis: ensure the CameraAdminPayload survives a round trip when
    // wrapped in `MessageBody::CameraAdminBody`. The browser-side WASM glue
    // (tentaflow-protocol-wasm) emits frames at this outer layer, so wire
    // compatibility must hold for the full envelope body.
    #[test]
    fn camera_admin_body_discover_request_round_trip() {
        use crate::message_body::MessageBody;
        let body = MessageBody::CameraAdminBody(CameraAdminPayload::DiscoverRequest(
            CameraDiscoverRequest {},
        ));
        let bytes = crate::cbor::encode(&body).expect("encode message body");
        let decoded = crate::cbor::decode::<MessageBody>(&bytes).expect("decode");
        assert_eq!(decoded, body);
    }

    #[test]
    fn camera_admin_body_add_onvif_request_round_trip() {
        use crate::message_body::MessageBody;
        let body = MessageBody::CameraAdminBody(CameraAdminPayload::AddOnvifRequest(
            CameraAddOnvifRequest {
                display_name: "Lobby".into(),
                device_service_url: "http://10.0.0.7/onvif/device_service".into(),
                username: "viewer".into(),
                password: "s3cret".into(),
                profile_token: None,
                target_fps: Some(10),
            },
        ));
        let bytes = crate::cbor::encode(&body).expect("encode message body");
        let decoded = crate::cbor::decode::<MessageBody>(&bytes).expect("decode");
        assert_eq!(decoded, body);
    }

    #[test]
    fn camera_admin_body_discover_response_round_trip() {
        use crate::message_body::MessageBody;
        let body = MessageBody::CameraAdminBody(CameraAdminPayload::DiscoverResponse(
            CameraDiscoverResponse {
                discovered: vec![
                    DiscoveredCameraInfo {
                        address: "10.0.0.21".into(),
                        xaddrs: vec!["http://10.0.0.21/onvif/device_service".into()],
                        types: vec!["dn:NetworkVideoTransmitter".into()],
                        manufacturer: "Hikvision".into(),
                        model: "DS-2CD".into(),
                    },
                    DiscoveredCameraInfo {
                        address: "10.0.0.22".into(),
                        xaddrs: vec![],
                        types: vec![],
                        manufacturer: String::new(),
                        model: String::new(),
                    },
                ],
            },
        ));
        let bytes = crate::cbor::encode(&body).expect("encode message body");
        let decoded = crate::cbor::decode::<MessageBody>(&bytes).expect("decode");
        assert_eq!(decoded, body);
    }

    #[test]
    fn frame_url_request_round_trip() {
        let v = CameraAdminPayload::FrameUrlRequest(CameraFrameUrlRequest {
            camera_id: "550e8400-e29b-41d4-a716-446655440000".into(),
            ttl_secs: 30,
        });
        assert_eq!(round_trip!(CameraAdminPayload, v.clone()), v);
    }

    #[test]
    fn frame_url_response_round_trip() {
        let v = CameraAdminPayload::FrameUrlResponse(CameraFrameUrlResponse {
            signed_url: "/frames/frame_550e8400-e29b-41d4-a716-446655440000?token=ABCD&exp=1700000000000&ref=frame_xyz"
                .into(),
            expires_at_ms: 1_700_000_000_000,
        });
        assert_eq!(round_trip!(CameraAdminPayload, v.clone()), v);
    }

    #[test]
    fn camera_admin_body_frame_url_request_round_trip() {
        use crate::message_body::MessageBody;
        let body = MessageBody::CameraAdminBody(CameraAdminPayload::FrameUrlRequest(
            CameraFrameUrlRequest {
                camera_id: "550e8400-e29b-41d4-a716-446655440000".into(),
                ttl_secs: 30,
            },
        ));
        let bytes = crate::cbor::encode(&body).expect("encode");
        let decoded = crate::cbor::decode::<MessageBody>(&bytes).expect("decode");
        assert_eq!(decoded, body);
    }

    #[test]
    fn camera_admin_body_frame_url_response_round_trip() {
        use crate::message_body::MessageBody;
        let body = MessageBody::CameraAdminBody(CameraAdminPayload::FrameUrlResponse(
            CameraFrameUrlResponse {
                signed_url: "/frames/frame_x?token=A&exp=1&ref=frame_x".into(),
                expires_at_ms: 1,
            },
        ));
        let bytes = crate::cbor::encode(&body).expect("encode");
        let decoded = crate::cbor::decode::<MessageBody>(&bytes).expect("decode");
        assert_eq!(decoded, body);
    }

    #[test]
    fn detections_subscribe_request_round_trip() {
        let v = CameraAdminPayload::DetectionsSubscribeRequest(CameraDetectionsSubscribeRequest {
            camera_id: "cam_550e8400-e29b-41d4-a716-446655440000".into(),
        });
        assert_eq!(round_trip!(CameraAdminPayload, v.clone()), v);
    }

    #[test]
    fn detections_frame_round_trip() {
        let v = CameraAdminPayload::DetectionsFrame(CameraDetectionsFrame {
            camera_id: "cam_550e8400-e29b-41d4-a716-446655440000".into(),
            ts_ms: 1_700_000_000_123,
            pts_ns: Some(1_234_567_890),
            proc_ms: 42,
            items: vec![
                DetectionItem {
                    klasa: "tablica_adr".into(),
                    bbox: [0.41, 0.22, 0.12, 0.06],
                    score: 0.96,
                    stan: Vec::new(),
                    tekst: Some("30/1202".into()),
                    tekst_conf: Some(0.91),
                    track_id: 7,
                    vx: 0.01,
                    vy: -0.02,
                },
                DetectionItem {
                    klasa: "nalepka_3".into(),
                    bbox: [0.30, 0.15, 0.05, 0.07],
                    score: 0.94,
                    stan: vec!["uszkodzona".into()],
                    tekst: None,
                    tekst_conf: None,
                    track_id: 0,
                    vx: 0.,
                    vy: 0.,
                },
            ],
        });
        assert_eq!(round_trip!(CameraAdminPayload, v.clone()), v);
    }

    #[test]
    fn camera_admin_body_detections_round_trip() {
        use crate::message_body::MessageBody;
        let body = MessageBody::CameraAdminBody(CameraAdminPayload::DetectionsFrame(
            CameraDetectionsFrame {
                camera_id: "cam_550e8400-e29b-41d4-a716-446655440000".into(),
                ts_ms: 42,
                pts_ns: None,
                proc_ms: 0,
                items: vec![DetectionItem {
                    klasa: "termometr".into(),
                    bbox: [0.0, 0.0, 0.1, 0.1],
                    score: 0.5,
                    stan: Vec::new(),
                    tekst: None,
                    tekst_conf: None,
                    track_id: 0,
                    vx: 0.,
                    vy: 0.,
                }],
            },
        ));
        let bytes = crate::cbor::encode(&body).expect("encode");
        let decoded = crate::cbor::decode::<MessageBody>(&bytes).expect("decode");
        assert_eq!(decoded, body);
    }

    #[test]
    fn camera_admin_body_add_onvif_response_round_trip() {
        use crate::message_body::MessageBody;
        let body = MessageBody::CameraAdminBody(CameraAdminPayload::AddOnvifResponse(
            CameraAddOnvifResponse {
                camera_id: "cam_xyz".into(),
                rtsp_url: "rtsp://10.0.0.21:554/Streaming/Channels/101".into(),
                profile_token: "Profile_1".into(),
            },
        ));
        let bytes = crate::cbor::encode(&body).expect("encode message body");
        let decoded = crate::cbor::decode::<MessageBody>(&bytes).expect("decode");
        assert_eq!(decoded, body);
    }
}
