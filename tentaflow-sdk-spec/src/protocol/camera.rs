// =============================================================================
// File: protocol/camera.rs — camera host-function ABI payloads
// Purpose: single source of truth for the CBOR request/response structs of the
// ten `camera_*_v1` host functions. Shared verbatim by the core host (decode
// input / encode output) and the addon SDK (encode input / decode output) so
// the wire format cannot drift between the two. Maps use integer keys (compact
// canonical form) via `#[cbor(map)]` + `#[n(N)]`.
// =============================================================================

use minicbor::{Decode, Encode};

// -----------------------------------------------------------------------------
// Input payloads
// -----------------------------------------------------------------------------

/// Input for `camera_add_v1`. `credentials_b64` carries an optional
/// base64-encoded `user:pass`; for `vendor='onvif'` it is required and also
/// feeds the SOAP UsernameToken. `onvif_profile_token` pins a media profile.
///
/// `target_fps`, `retention_class` and `profile` are `Option` on the wire so a
/// minimal payload can omit them; the host resolves the legacy TOML defaults
/// (`30` / `"C"` / `"default"`) right after decode via
/// [`CameraAddInput::target_fps_or_default`] and friends.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraAddInput {
    #[n(0)]
    pub display_name: String,
    #[n(1)]
    pub vendor: String,
    #[n(2)]
    pub url: String,
    #[n(3)]
    pub target_fps: Option<u32>,
    #[n(4)]
    pub resolution_width: Option<u32>,
    #[n(5)]
    pub resolution_height: Option<u32>,
    #[n(6)]
    pub retention_class: Option<String>,
    #[n(7)]
    pub profile: Option<String>,
    #[n(8)]
    pub credentials_b64: Option<String>,
    #[n(9)]
    pub onvif_profile_token: Option<String>,
}

/// Legacy TOML default for `target_fps` when the payload omits it.
pub const CAMERA_ADD_DEFAULT_TARGET_FPS: u32 = 30;
/// Legacy TOML default for `retention_class` when the payload omits it.
pub const CAMERA_ADD_DEFAULT_RETENTION_CLASS: &str = "C";
/// Legacy TOML default for `profile` when the payload omits it.
pub const CAMERA_ADD_DEFAULT_PROFILE: &str = "default";

impl CameraAddInput {
    /// `target_fps` with the legacy default applied when absent.
    pub fn target_fps_or_default(&self) -> u32 {
        self.target_fps.unwrap_or(CAMERA_ADD_DEFAULT_TARGET_FPS)
    }

    /// `retention_class` with the legacy default applied when absent.
    pub fn retention_class_or_default(&self) -> String {
        self.retention_class
            .clone()
            .unwrap_or_else(|| CAMERA_ADD_DEFAULT_RETENTION_CLASS.to_string())
    }

    /// `profile` with the legacy default applied when absent.
    pub fn profile_or_default(&self) -> String {
        self.profile
            .clone()
            .unwrap_or_else(|| CAMERA_ADD_DEFAULT_PROFILE.to_string())
    }
}

/// Input carrying a single `camera_id` — shared by `get` / `remove` /
/// `snapshot` / `health`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraIdInput {
    #[n(0)]
    pub camera_id: String,
}

/// Input for `camera_update_v1`. Every field except `camera_id` is optional —
/// only present fields are patched.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraUpdateInput {
    #[n(0)]
    pub camera_id: String,
    #[n(1)]
    pub display_name: Option<String>,
    #[n(2)]
    pub target_fps: Option<u32>,
    #[n(3)]
    pub resolution_width: Option<u32>,
    #[n(4)]
    pub resolution_height: Option<u32>,
    #[n(5)]
    pub retention_class: Option<String>,
    #[n(6)]
    pub profile: Option<String>,
}

/// Input for `camera_test_connection_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraTestConnectionInput {
    #[n(0)]
    pub vendor: String,
    #[n(1)]
    pub url: String,
}

/// Input for `camera_credentials_rotate_v1`. `new_credentials_b64 = None`
/// clears the stored credential.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraCredentialsRotateInput {
    #[n(0)]
    pub camera_id: String,
    #[n(1)]
    pub new_credentials_b64: Option<String>,
}

// -----------------------------------------------------------------------------
// Output payloads
// -----------------------------------------------------------------------------

/// Output of `camera_add_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraAddOutput {
    #[n(0)]
    pub camera_id: String,
    #[n(1)]
    pub status: String,
}

/// One camera as returned by `camera_get_v1` / `camera_update_v1` and as an
/// element of `camera_list_v1`. Runtime metrics fall back to DB values when no
/// live supervisor session exists.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraInfoOut {
    #[n(0)]
    pub camera_id: String,
    #[n(1)]
    pub display_name: String,
    #[n(2)]
    pub vendor: String,
    #[n(3)]
    pub url: String,
    #[n(4)]
    pub target_fps: i64,
    #[n(5)]
    pub resolution_width: Option<i64>,
    #[n(6)]
    pub resolution_height: Option<i64>,
    #[n(7)]
    pub status: String,
    #[n(8)]
    pub status_message: Option<String>,
    #[n(9)]
    pub fps_actual: Option<f64>,
    #[n(10)]
    pub last_frame_at: Option<i64>,
    #[n(11)]
    pub retention_class: String,
    #[n(12)]
    pub profile: String,
}

/// Output of `camera_list_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraListOut {
    #[n(0)]
    pub camera: Vec<CameraInfoOut>,
}

/// Output of `camera_snapshot_v1`. `data_b64` is the base64-encoded RGB24
/// frame buffer.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraSnapshotOut {
    #[n(0)]
    pub camera_id: String,
    #[n(1)]
    pub width: u32,
    #[n(2)]
    pub height: u32,
    #[n(3)]
    pub pixel_format: String,
    #[n(4)]
    pub timestamp_unix_ms: u64,
    #[n(5)]
    pub data_b64: String,
}

/// Output of `camera_health_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraHealthOut {
    #[n(0)]
    pub camera_id: String,
    #[n(1)]
    pub status: String,
    #[n(2)]
    pub status_message: String,
    #[n(3)]
    pub fps_actual: f64,
    #[n(4)]
    pub last_frame_at: i64,
    #[n(5)]
    pub frames_total: u64,
    #[n(6)]
    pub frames_dropped: u64,
}

/// Output of `camera_remove_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraRemoveOut {
    #[n(0)]
    pub removed: bool,
}

/// A single discovered ONVIF device in `camera_discover_v1`. Not yet persisted,
/// so it carries no `camera_id`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct DiscoveredCameraOut {
    #[n(0)]
    pub address: String,
    #[n(1)]
    pub xaddrs: Vec<String>,
    #[n(2)]
    pub types: Vec<String>,
    #[n(3)]
    pub manufacturer: String,
    #[n(4)]
    pub model: String,
}

/// Output of `camera_discover_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraDiscoverOut {
    #[n(0)]
    pub discovered: Vec<DiscoveredCameraOut>,
}

/// Output of `camera_test_connection_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraTestConnectionOut {
    #[n(0)]
    pub ok: bool,
    #[n(1)]
    pub message: String,
}

/// Output of `camera_credentials_rotate_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraCredentialsRotateOut {
    #[n(0)]
    pub rotated: bool,
    #[n(1)]
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T>(value: &T)
    where
        T: Encode<()> + for<'b> Decode<'b, ()> + PartialEq + core::fmt::Debug,
    {
        let mut buf = Vec::new();
        minicbor::encode(value, &mut buf).unwrap();
        let decoded: T = minicbor::decode(&buf).unwrap();
        assert_eq!(&decoded, value);
    }

    #[test]
    fn roundtrip_add_input_full() {
        roundtrip(&CameraAddInput {
            display_name: "front door".into(),
            vendor: "onvif".into(),
            url: "http://10.0.0.5/onvif/device_service".into(),
            target_fps: Some(25),
            resolution_width: Some(1920),
            resolution_height: Some(1080),
            retention_class: Some("A".into()),
            profile: Some("default".into()),
            credentials_b64: Some("dXNlcjpwYXNz".into()),
            onvif_profile_token: Some("profile_1".into()),
        });
    }

    #[test]
    fn roundtrip_add_input_minimal() {
        roundtrip(&CameraAddInput {
            display_name: "cam".into(),
            vendor: "fake_file".into(),
            url: "/tmp/sample.mp4".into(),
            target_fps: None,
            resolution_width: None,
            resolution_height: None,
            retention_class: None,
            profile: None,
            credentials_b64: None,
            onvif_profile_token: None,
        });
    }

    #[test]
    fn omitted_optional_fields_resolve_to_legacy_defaults() {
        let minimal = CameraAddInput {
            display_name: "cam".into(),
            vendor: "fake_file".into(),
            url: "/tmp/sample.mp4".into(),
            target_fps: None,
            resolution_width: None,
            resolution_height: None,
            retention_class: None,
            profile: None,
            credentials_b64: None,
            onvif_profile_token: None,
        };
        let mut buf = Vec::new();
        minicbor::encode(&minimal, &mut buf).unwrap();
        let decoded: CameraAddInput = minicbor::decode(&buf).unwrap();
        assert_eq!(decoded.target_fps_or_default(), 30);
        assert_eq!(decoded.retention_class_or_default(), "C");
        assert_eq!(decoded.profile_or_default(), "default");
    }

    #[test]
    fn roundtrip_info_out() {
        roundtrip(&CameraInfoOut {
            camera_id: "cam_00000000-0000-0000-0000-000000000000".into(),
            display_name: "cam".into(),
            vendor: "rtsp".into(),
            url: "rtsp://host/stream".into(),
            target_fps: 30,
            resolution_width: Some(1280),
            resolution_height: Some(720),
            status: "online".into(),
            status_message: None,
            fps_actual: Some(29.97),
            last_frame_at: Some(1_700_000_000_000),
            retention_class: "C".into(),
            profile: "default".into(),
        });
    }

    #[test]
    fn roundtrip_list_out() {
        roundtrip(&CameraListOut { camera: vec![] });
    }

    #[test]
    fn roundtrip_discover_out() {
        roundtrip(&CameraDiscoverOut {
            discovered: vec![DiscoveredCameraOut {
                address: "10.0.0.5".into(),
                xaddrs: vec!["http://10.0.0.5/onvif/device_service".into()],
                types: vec!["NetworkVideoTransmitter".into()],
                manufacturer: "ACME".into(),
                model: "X1".into(),
            }],
        });
    }
}
