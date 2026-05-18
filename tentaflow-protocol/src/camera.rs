// =============================================================================
// File: camera.rs
// Purpose: Admin-side binary protocol for camera discovery + add (F2 P7.a).
//          Packed into a single `CameraAdminPayload` inner enum so the whole
//          camera-wizard surface burns one `MessageBody` discriminant slot
//          (rkyv 0.8 caps `MessageBody` at 256 variants — see profiling.rs
//          / vision.rs for the same pack pattern).
// =============================================================================

use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

/// One device returned by ONVIF WS-Discovery. Mirrors the host-fn
/// `DiscoveredCameraOut` shape so the dashboard wizard and addons share
/// the same field set; the wire-level type lives in this crate so the
/// `tentaflow-core` crate can be built without dragging the dashboard
/// schema into the addon ABI.
#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
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

#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub struct CameraDiscoverRequest {}

#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub struct CameraDiscoverResponse {
    pub discovered: Vec<DiscoveredCameraInfo>,
}

/// Request to add a discovered ONVIF camera as a managed session. The
/// dashboard never re-uses `camera_add_v1` (host-fn / addon-scoped); this
/// admin RPC carries the operator's user-session credentials and binds the
/// resulting row to the org from `ctx.org_context`.
#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
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

#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub struct CameraAddOnvifResponse {
    pub camera_id: String,
    /// RTSP URI derived from `GetStreamUri` against the chosen profile.
    pub rtsp_url: String,
    /// Profile token actually bound (echoes the request when set, otherwise
    /// the first profile that the device advertised).
    pub profile_token: String,
}

/// Inner-enum pack — keeps every admin camera RPC in a single
/// `MessageBody::CameraAdminBody` slot (rkyv 256-variant budget). Matches the
/// `ProfilingPayload` / `VisionInferPayload` pattern.
#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub enum CameraAdminPayload {
    DiscoverRequest(CameraDiscoverRequest),
    DiscoverResponse(CameraDiscoverResponse),
    AddOnvifRequest(CameraAddOnvifRequest),
    AddOnvifResponse(CameraAddOnvifResponse),
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! round_trip {
        ($ty:ty, $value:expr) => {{
            let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&$value).expect("encode");
            rkyv::from_bytes::<$ty, rkyv::rancor::Error>(&bytes).expect("decode")
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
}
