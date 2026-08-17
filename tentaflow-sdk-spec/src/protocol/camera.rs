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
    /// Per-camera AI analysis frame rate honored by the always-on analysis
    /// loop. `0` means unlimited (run at the native frame cadence). `None`
    /// resolves to [`CAMERA_DEFAULT_ANALYSIS_FPS`].
    #[n(10)]
    pub analysis_fps: Option<u32>,
}

/// Legacy TOML default for `target_fps` when the payload omits it.
pub const CAMERA_ADD_DEFAULT_TARGET_FPS: u32 = 30;

/// Default AI analysis frame rate when `analysis_fps` is absent on the wire.
pub const CAMERA_DEFAULT_ANALYSIS_FPS: u32 = 10;
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

    /// `analysis_fps` with the default applied when absent (`0` = unlimited).
    pub fn analysis_fps_or_default(&self) -> u32 {
        self.analysis_fps.unwrap_or(CAMERA_DEFAULT_ANALYSIS_FPS)
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

/// Detection zones of a camera — normalized polygons serialized as JSON,
/// mirroring the `cameras.zones_json` column. An empty list means "analyse the
/// whole frame".
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraZonesOut {
    #[n(0)]
    pub camera_id: String,
    #[n(1)]
    pub zones_json: String,
}

/// Input for `camera_zones_set_v1` — replaces the whole polygon set.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraZonesSetInput {
    #[n(0)]
    pub camera_id: String,
    #[n(1)]
    pub zones_json: String,
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
    /// Patch the per-camera AI analysis frame rate. `Some(0)` = unlimited,
    /// `None` leaves the stored value untouched.
    #[n(7)]
    pub analysis_fps: Option<u32>,
    /// Patch the per-camera analysis Flow id (the cold path runs it on a
    /// detection event). `None` leaves the assignment untouched; `Some("")`
    /// clears it (back to the built-in enrichment); `Some(id)` assigns that
    /// flow. The host validates the flow exists and is active before persisting.
    #[n(8)]
    pub analysis_flow_id: Option<String>,
    /// Patch the per-camera CV pipeline id (`camera_cv_pipelines.id`). Same
    /// tri-state encoding as `analysis_flow_id`: `None` leaves the assignment
    /// untouched, `Some("")` clears it (camera falls back to the default
    /// pipeline), `Some(id)` assigns after the host verifies the pipeline
    /// exists.
    #[n(9)]
    pub cv_pipeline_id: Option<String>,
}

impl CameraUpdateInput {
    /// `analysis_fps` with the default applied when absent (`0` = unlimited).
    pub fn analysis_fps_or_default(&self) -> u32 {
        self.analysis_fps.unwrap_or(CAMERA_DEFAULT_ANALYSIS_FPS)
    }
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
    /// Per-camera analysis Flow id (empty/absent = none assigned). Lets the UI
    /// show + preselect the camera's current analysis flow.
    #[n(13)]
    pub analysis_flow_id: Option<String>,
    /// Owning addon id of the camera. Populated by `camera_list_accessible_v1`
    /// so a consumer addon can tell which cameras it owns vs. has via a grant.
    /// `None` on legacy owner-only surfaces that never set it.
    #[n(14)]
    pub owner_addon_id: Option<String>,
    /// Access level of the calling addon to this camera: `"owner"` when the
    /// caller owns it, `"granted"` when reached through a cross-addon grant.
    /// `None` on surfaces that do not compute it.
    #[n(15)]
    pub access_level: Option<String>,
    /// Per-camera CV pipeline id (empty/absent = the default pipeline). Lets
    /// the UI preselect the camera's current pipeline assignment.
    #[n(16)]
    pub cv_pipeline_id: Option<String>,
}

/// Output of `camera_list_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraListOut {
    #[n(0)]
    pub camera: Vec<CameraInfoOut>,
}

/// Input for `camera_grant_v1` — creates a cross-addon read grant. `level` is
/// an allowlisted access level (currently only `"read"`).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraGrantInput {
    #[n(0)]
    pub camera_id: String,
    #[n(1)]
    pub grantee_addon_id: String,
    #[n(2)]
    pub level: String,
}

/// Input for `camera_revoke_v1` — removes a previously issued grant.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraRevokeInput {
    #[n(0)]
    pub camera_id: String,
    #[n(1)]
    pub grantee_addon_id: String,
    #[n(2)]
    pub level: String,
}

/// Output of `camera_grant_v1` / `camera_revoke_v1`. `ok` is `true` when the
/// grant was created (grant) or an existing grant was removed (revoke).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraGrantOut {
    #[n(0)]
    pub ok: bool,
}

/// One grant on a camera, as listed by `camera_grants_list_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraGrantInfo {
    #[n(0)]
    pub grantee_addon_id: String,
    #[n(1)]
    pub level: String,
    #[n(2)]
    pub created_by: String,
}

/// Input for `camera_grants_list_v1` — the camera whose grants to list.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraGrantListInput {
    #[n(0)]
    pub camera_id: String,
}

/// Output of `camera_grants_list_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraGrantListOut {
    #[n(0)]
    pub grants: Vec<CameraGrantInfo>,
}

/// One assignable camera-analysis flow (id + display name), for the per-camera
/// analysis-flow selector.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraAnalysisFlowOut {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub name: String,
}

/// Output of `camera_analysis_flows_list_v1` — the active flows assignable as a
/// camera's analysis flow (scoped to `service_type='camera_analysis'`).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraAnalysisFlowsOut {
    #[n(0)]
    pub flows: Vec<CameraAnalysisFlowOut>,
}

/// One camera CV pipeline as listed by `camera_cv_pipelines_list_v1` (the
/// per-camera pipeline picker + the pipeline manager list). The JSON body is
/// fetched per-pipeline via `camera_cv_pipeline_get_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraCvPipelineSummary {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub name: String,
    /// Seed-owned default pipeline (cannot be deleted; cameras without an
    /// explicit assignment resolve to it).
    #[n(2)]
    pub is_default: bool,
    #[n(3)]
    pub updated_at: i64,
}

/// Output of `camera_cv_pipelines_list_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraCvPipelinesOut {
    #[n(0)]
    pub pipelines: Vec<CameraCvPipelineSummary>,
}

/// Input carrying a single pipeline id — shared by `camera_cv_pipeline_get_v1`
/// and `camera_cv_pipeline_delete_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraCvPipelineIdInput {
    #[n(0)]
    pub id: String,
}

/// Output of `camera_cv_pipeline_get_v1` — one pipeline with its full JSON
/// body (`{"stages":[...]}` per `cv_pipeline::CvPipeline`).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraCvPipelineOut {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub name: String,
    #[n(2)]
    pub pipeline_json: String,
}

/// Input for `camera_cv_pipeline_save_v1`. `id = None` creates a new pipeline
/// under a fresh uuid; `Some(id)` upserts that row (the seed-owned default
/// flag is never writable through this surface).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraCvPipelineSaveInput {
    #[n(0)]
    pub id: Option<String>,
    #[n(1)]
    pub name: String,
    #[n(2)]
    pub pipeline_json: String,
}

/// Output of `camera_cv_pipeline_save_v1`. Validation failures (structural
/// `cv_pipeline::validate` + alias existence) come back as `id = None` +
/// a human-readable `error` the addon can display verbatim.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraCvPipelineSaveOut {
    #[n(0)]
    pub id: Option<String>,
    #[n(1)]
    pub error: Option<String>,
}

/// Output of `camera_cv_pipeline_delete_v1`. A refused delete (default
/// pipeline, still referenced by cameras, missing row) comes back as
/// `deleted = false` + a human-readable `error`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CameraCvPipelineDeleteOut {
    #[n(0)]
    pub deleted: bool,
    #[n(1)]
    pub error: Option<String>,
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

/// A single locally attached camera device enumerated by
/// `camera_local_devices_v1`. `device_path` is the value to pass back as the
/// camera `url` (e.g. `/dev/video0` on Linux), `vendor` is the matching stable
/// TentaFlow vendor (`v4l2` on Linux, `local_camera` elsewhere).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct LocalCameraDeviceOut {
    #[n(0)]
    pub device_path: String,
    #[n(1)]
    pub label: String,
    #[n(2)]
    pub vendor: String,
}

/// Output of `camera_local_devices_v1`. An empty list is a valid result on
/// platforms without enumeration support — it is not an error.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct LocalCameraDevicesOut {
    #[n(0)]
    pub devices: Vec<LocalCameraDeviceOut>,
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
            analysis_fps: Some(5),
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
            analysis_fps: None,
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
            analysis_fps: None,
        };
        let mut buf = Vec::new();
        minicbor::encode(&minimal, &mut buf).unwrap();
        let decoded: CameraAddInput = minicbor::decode(&buf).unwrap();
        assert_eq!(decoded.target_fps_or_default(), 30);
        assert_eq!(decoded.retention_class_or_default(), "C");
        assert_eq!(decoded.profile_or_default(), "default");
        assert_eq!(decoded.analysis_fps_or_default(), 10);
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
            analysis_flow_id: Some("00000000-0000-4000-8000-000000000020".into()),
            owner_addon_id: Some("go2".into()),
            access_level: Some("granted".into()),
            cv_pipeline_id: Some("00000000-0000-4000-8000-000000000030".into()),
        });
    }

    #[test]
    fn roundtrip_cv_pipeline_payloads() {
        roundtrip(&CameraCvPipelinesOut {
            pipelines: vec![CameraCvPipelineSummary {
                id: "00000000-0000-4000-8000-000000000030".into(),
                name: "Analiza domyślna (ADR)".into(),
                is_default: true,
                updated_at: 1_700_000_000,
            }],
        });
        roundtrip(&CameraCvPipelineIdInput {
            id: "00000000-0000-4000-8000-000000000030".into(),
        });
        roundtrip(&CameraCvPipelineOut {
            id: "00000000-0000-4000-8000-000000000030".into(),
            name: "Analiza domyślna (ADR)".into(),
            pipeline_json: "{\"stages\":[]}".into(),
        });
        roundtrip(&CameraCvPipelineSaveInput {
            id: None,
            name: "Custom".into(),
            pipeline_json: "{\"stages\":[]}".into(),
        });
        roundtrip(&CameraCvPipelineSaveOut {
            id: Some("00000000-0000-4000-8000-000000000031".into()),
            error: None,
        });
        roundtrip(&CameraCvPipelineSaveOut {
            id: None,
            error: Some("invalid pipeline: duplicate stage_id 'detect'".into()),
        });
        roundtrip(&CameraCvPipelineDeleteOut {
            deleted: false,
            error: Some("the default pipeline cannot be deleted".into()),
        });
    }

    #[test]
    fn roundtrip_grant_input() {
        roundtrip(&CameraGrantInput {
            camera_id: "cam_00000000-0000-0000-0000-000000000000".into(),
            grantee_addon_id: "tentavision".into(),
            level: "read".into(),
        });
    }

    #[test]
    fn roundtrip_revoke_input() {
        roundtrip(&CameraRevokeInput {
            camera_id: "cam_00000000-0000-0000-0000-000000000000".into(),
            grantee_addon_id: "tentavision".into(),
            level: "read".into(),
        });
    }

    #[test]
    fn roundtrip_grant_out() {
        roundtrip(&CameraGrantOut { ok: true });
    }

    #[test]
    fn roundtrip_grant_info() {
        roundtrip(&CameraGrantInfo {
            grantee_addon_id: "tentavision".into(),
            level: "read".into(),
            created_by: "user_42".into(),
        });
    }

    #[test]
    fn roundtrip_grant_list_input() {
        roundtrip(&CameraGrantListInput {
            camera_id: "cam_00000000-0000-0000-0000-000000000000".into(),
        });
    }

    #[test]
    fn roundtrip_grant_list_out() {
        roundtrip(&CameraGrantListOut { grants: vec![] });
        roundtrip(&CameraGrantListOut {
            grants: vec![CameraGrantInfo {
                grantee_addon_id: "*".into(),
                level: "read".into(),
                created_by: "go2".into(),
            }],
        });
    }

    #[test]
    fn roundtrip_list_out() {
        roundtrip(&CameraListOut { camera: vec![] });
    }

    #[test]
    fn roundtrip_local_devices_out() {
        roundtrip(&LocalCameraDevicesOut { devices: vec![] });
        roundtrip(&LocalCameraDevicesOut {
            devices: vec![LocalCameraDeviceOut {
                device_path: "/dev/video0".into(),
                label: "HD Webcam".into(),
                vendor: "v4l2".into(),
            }],
        });
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
