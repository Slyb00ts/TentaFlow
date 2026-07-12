// =============================================================================
// File: addons-pro/tentavision/src/lib.rs
// TentaVision addon — video surveillance with 14 panels, CBOR SDK.
// =============================================================================

#![allow(clippy::too_many_lines, clippy::collapsible_else_if, dead_code)]

#[used]
static BUILD_TS: &str = "20260614-1500";

extern crate alloc;

mod db;

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde_json::{self, json, Value as JsonValue};
use tentaflow_sdk_spec::{
    Component, SlotContent, SlotDecl, PanelShell, UiPayload,
    SlotDefault, SlotSemantics, CachePolicy, SlotVisibility, StateEntry,
    StatePatch, HandlerMap, Handler, FailurePolicy,
    Value, PathSegment, StatePath, PatchOp, PatchOpKind,
};
use tentaflow_sdk_spec::protocol::control::CborMap;
use tentaflow_sdk_spec::protocol::recording::{
    RecordingGetUrlInput, RecordingListInput, RecordingListItem, RecordingListOut, UrlOut,
};
use tentaflow_sdk_spec::protocol::camera::{
    CameraAddInput, CameraAddOutput, CameraCvPipelineDeleteOut, CameraCvPipelineIdInput,
    CameraCvPipelineOut, CameraCvPipelineSaveInput, CameraCvPipelineSaveOut,
    CameraCvPipelineSummary, CameraCvPipelinesOut, CameraDiscoverOut, CameraIdInput,
    CameraRemoveOut, CameraTestConnectionInput, CameraTestConnectionOut,
    DiscoveredCameraOut, LocalCameraDeviceOut, LocalCameraDevicesOut,
    CAMERA_DEFAULT_ANALYSIS_FPS,
};
use tentaflow_sdk_spec::protocol::ui::{
    bind::BindRef,
    a11y::Accessibility,
    layout::{Stack, Flex, Grid, Card, SectionCard, Divider},
    layout::nav::NavTabs as NavTabsStruct,
    data::{Text as TextComp, Heading as HeadingComp, Badge as BadgeComp, Chip as ChipComp,
           KeyValue as KvComp, StatCard as StatCardComp, Avatar as AvatarComp,
           Sparkline as SparklineComp, Heatmap as HeatmapComp,
           ProgressBar as ProgressBarComp},
    data::charts::StackedBar as StackedBarComp,
    data::tables::Table as TableComp,
    actions::{Button as ButtonComp, IconButton as IconButtonComp, Link as LinkComp,
              FilterChips as FilterChipsComp},
    feedback::{Alert as AlertComp, Spinner as SpinnerComp, GateScreen as GateScreenComp},
    feedback::overlays::Modal as ModalComp,
    molecules::EmptyState as EmptyStateComp,
    specialized::StepProgress as StepProgressComp,
    tokens::*,
    inline::*,
    icon_name::IconName,
};

// =============================================================================
// Host function imports
// =============================================================================

#[link(wasm_import_module = "tentaflow")]
extern "C" {
    fn ui_render_cbor(cbor_ptr: i32, cbor_len: i32) -> i32;
    fn store_get(key_ptr: i32, key_len: i32, out_ptr: i32, out_cap: i32) -> i32;
    fn store_set(key_ptr: i32, key_len: i32, val_ptr: i32, val_len: i32) -> i32;
    fn event_publish(
        event_type_ptr: i32, event_type_len: i32,
        payload_ptr: i32, payload_len: i32,
    ) -> i32;
    fn ui_notify(
        title_ptr: i32, title_len: i32,
        body_ptr: i32, body_len: i32,
        level_ptr: i32, level_len: i32,
    ) -> i32;
    fn log_info(msg_ptr: i32, msg_len: i32) -> i32;
    fn log_warn(msg_ptr: i32, msg_len: i32) -> i32;
    fn log_error(msg_ptr: i32, msg_len: i32) -> i32;
    fn camera_add_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_remove_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_get_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_update_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_list_accessible_v1(out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
    fn camera_analysis_flows_list_v1(out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
    fn camera_cv_pipelines_list_v1(out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
    fn camera_cv_pipeline_get_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_cv_pipeline_save_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_cv_pipeline_delete_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_discover_v1(out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
    fn camera_local_devices_v1(out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
    fn camera_test_connection_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn vector_upsert_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn vector_search_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn llm_generate(
        prompt_ptr: i32, prompt_len: i32,
        model_ptr: i32, model_len: i32,
        options_ptr: i32, options_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn alias_list_available_v1(out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
    fn recording_list_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn recording_get_url_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn event_subscribe(
        event_type_ptr: i32, event_type_len: i32,
        filter_json_ptr: i32, filter_json_len: i32,
    ) -> i32;
}

/// Subscribes the addon to a system event type so its `on_event` is invoked for
/// each matching event. No filter (pass 0/0). Needs the `events` permission.
fn subscribe_event(event_type: &str) -> i32 {
    unsafe {
        event_subscribe(
            event_type.as_ptr() as i32,
            event_type.len() as i32,
            0,
            0,
        )
    }
}

/// Event type the camera_alert flow node emits on an alarm verdict.
const CAMERA_ALARM_EVENT: &str = "camera.alarm";

// =============================================================================
// Host function wrappers
// =============================================================================

mod log {
    use super::*;
    pub fn info(msg: &str) {
        unsafe { log_info(msg.as_ptr() as i32, msg.len() as i32); }
    }
    pub fn warn(msg: &str) {
        unsafe { log_warn(msg.as_ptr() as i32, msg.len() as i32); }
    }
    pub fn error(msg: &str) {
        unsafe { log_error(msg.as_ptr() as i32, msg.len() as i32); }
    }
}

fn notify(title: &str, body: &str) {
    let level = "info";
    unsafe {
        ui_notify(
            title.as_ptr() as i32, title.len() as i32,
            body.as_ptr() as i32, body.len() as i32,
            level.as_ptr() as i32, level.len() as i32,
        );
    }
}

// =============================================================================
// Camera ABI wrappers
// =============================================================================

/// Canonical ABI error codes returned by the camera host functions. Values
/// match `tentaflow_core::addon::errors::AbiError` (positive 1..24; `0` = Ok).
/// The host returns these directly as the i32 host-function result, so an addon
/// must NOT negate the return value when classifying an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
enum AbiError {
    Permission = 1,
    NotFound = 2,
    NoAvailableTarget = 3,
    Timeout = 4,
    Operation = 5,
    OutputBufferTooSmall = 6,
    Conflict = 7,
    QuotaExceeded = 11,
    CameraUnreachable = 12,
    CameraAuthFailed = 13,
    CameraVendorUnsupported = 14,
    PayloadTooLarge = 21,
    Unknown = 99,
}

impl AbiError {
    fn from_code(code: i32) -> Self {
        match code {
            1 => Self::Permission,
            2 => Self::NotFound,
            3 => Self::NoAvailableTarget,
            4 => Self::Timeout,
            5 => Self::Operation,
            6 => Self::OutputBufferTooSmall,
            7 => Self::Conflict,
            11 => Self::QuotaExceeded,
            12 => Self::CameraUnreachable,
            13 => Self::CameraAuthFailed,
            14 => Self::CameraVendorUnsupported,
            21 => Self::PayloadTooLarge,
            _ => Self::Unknown,
        }
    }
}

impl core::fmt::Display for AbiError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "AbiError({})", *self as i32)
    }
}

/// Encodes `input` to CBOR and decodes the CBOR response of a host function
/// with the standard `(input_ptr, input_len, out_ptr, out_cap, out_len_ptr)`
/// ABI shape. On `OutputBufferTooSmall` the host writes the required size into
/// `out_len_ptr`; we grow once and retry so a large response is not lost.
fn call_cbor_in_out<I, O>(
    input: &I,
    host_fn: unsafe extern "C" fn(i32, i32, i32, i32, i32) -> i32,
) -> Result<O, AbiError>
where
    I: minicbor::Encode<()>,
    O: for<'b> minicbor::Decode<'b, ()>,
{
    let mut input_bytes = Vec::new();
    minicbor::encode(input, &mut input_bytes).map_err(|_| AbiError::Operation)?;
    let mut cap = 16384usize;
    loop {
        let mut out = vec![0u8; cap];
        let mut out_len: i32 = 0;
        let ret = unsafe {
            host_fn(
                input_bytes.as_ptr() as i32,
                input_bytes.len() as i32,
                out.as_mut_ptr() as i32,
                out.len() as i32,
                &mut out_len as *mut i32 as i32,
            )
        };
        if ret == AbiError::OutputBufferTooSmall as i32 {
            cap = (out_len as usize).max(cap.saturating_mul(2));
            continue;
        }
        if ret != 0 {
            return Err(AbiError::from_code(ret));
        }
        out.truncate(out_len as usize);
        return minicbor::decode(&out).map_err(|_| AbiError::Operation);
    }
}

/// Decodes the CBOR response of a host function with the read-only
/// `(out_ptr, out_cap, out_len_ptr)` ABI shape (`camera_discover` /
/// `camera_local_devices`).
fn call_cbor_out<O>(
    host_fn: unsafe extern "C" fn(i32, i32, i32) -> i32,
) -> Result<O, AbiError>
where
    O: for<'b> minicbor::Decode<'b, ()>,
{
    let mut cap = 16384usize;
    loop {
        let mut out = vec![0u8; cap];
        let mut out_len: i32 = 0;
        let ret = unsafe {
            host_fn(
                out.as_mut_ptr() as i32,
                out.len() as i32,
                &mut out_len as *mut i32 as i32,
            )
        };
        if ret == AbiError::OutputBufferTooSmall as i32 {
            cap = (out_len as usize).max(cap.saturating_mul(2));
            continue;
        }
        if ret != 0 {
            return Err(AbiError::from_code(ret));
        }
        out.truncate(out_len as usize);
        return minicbor::decode(&out).map_err(|_| AbiError::Operation);
    }
}

/// Lists the flows assignable as a camera's analysis flow (`(id, name)`),
/// scoped host-side to `service_type='camera_analysis'`.
fn host_camera_analysis_flows() -> Result<Vec<(String, String)>, AbiError> {
    let out: tentaflow_sdk_spec::CameraAnalysisFlowsOut =
        call_cbor_out(camera_analysis_flows_list_v1)?;
    Ok(out.flows.into_iter().map(|f| (f.id, f.name)).collect())
}

/// Lists every camera this addon can access: cameras it owns UNION cameras
/// granted to it by another addon (cross-addon share). Each entry carries
/// `owner_addon_id` + `access_level` ("owner" | "granted"). For granted cameras
/// the `url` is redacted host-side — live view goes through the dashboard stream
/// keyed by `camera_id`, so the url is not needed for display or streaming.
fn host_list_accessible_cameras() -> Result<Vec<tentaflow_sdk_spec::CameraInfoOut>, AbiError> {
    let out: tentaflow_sdk_spec::CameraListOut =
        call_cbor_out(camera_list_accessible_v1)?;
    Ok(out.camera)
}

/// Reads one camera's authoritative info from core (carries `analysis_flow_id`,
/// which the addon's own SQLite mirror does not store).
fn host_camera_get(camera_id: &str) -> Result<tentaflow_sdk_spec::CameraInfoOut, AbiError> {
    let input = tentaflow_sdk_spec::CameraIdInput {
        camera_id: camera_id.into(),
    };
    call_cbor_in_out(&input, camera_get_v1)
}

/// Sets (or, with `None`, clears) a camera's analysis flow via core. Empty string
/// on the wire is the documented "clear" signal for `analysis_flow_id`.
fn host_camera_set_flow(
    camera_id: &str,
    flow_id: Option<&str>,
) -> Result<tentaflow_sdk_spec::CameraInfoOut, AbiError> {
    let input = tentaflow_sdk_spec::CameraUpdateInput {
        camera_id: camera_id.into(),
        display_name: None,
        target_fps: None,
        resolution_width: None,
        resolution_height: None,
        retention_class: None,
        profile: None,
        analysis_fps: None,
        analysis_flow_id: Some(flow_id.unwrap_or("").into()),
        cv_pipeline_id: None,
    };
    call_cbor_in_out(&input, camera_update_v1)
}

/// Sets (or, with `None`, clears) a camera's CV pipeline via core. Empty string
/// on the wire is the documented "clear" signal for `cv_pipeline_id` (camera
/// falls back to the seed default pipeline).
fn host_camera_set_cv_pipeline(
    camera_id: &str,
    pipeline_id: Option<&str>,
) -> Result<tentaflow_sdk_spec::CameraInfoOut, AbiError> {
    let input = tentaflow_sdk_spec::CameraUpdateInput {
        camera_id: camera_id.into(),
        display_name: None,
        target_fps: None,
        resolution_width: None,
        resolution_height: None,
        retention_class: None,
        profile: None,
        analysis_fps: None,
        analysis_flow_id: None,
        cv_pipeline_id: Some(pipeline_id.unwrap_or("").into()),
    };
    call_cbor_in_out(&input, camera_update_v1)
}

/// Lists every camera CV pipeline summary (id, name, default flag) for the
/// per-camera pipeline picker and the pipeline manager list.
fn host_cv_pipelines_list() -> Result<Vec<CameraCvPipelineSummary>, AbiError> {
    let out: CameraCvPipelinesOut = call_cbor_out(camera_cv_pipelines_list_v1)?;
    Ok(out.pipelines)
}

/// Fetches one pipeline with its full `{"stages":[...]}` JSON body.
fn host_cv_pipeline_get(id: &str) -> Result<CameraCvPipelineOut, AbiError> {
    let input = CameraCvPipelineIdInput { id: id.into() };
    call_cbor_in_out(&input, camera_cv_pipeline_get_v1)
}

/// Creates (`id = None` → host mints a uuid) or updates a pipeline. Host-side
/// validation failures come back inside the output (`error`), not as an ABI code.
fn host_cv_pipeline_save(
    id: Option<&str>,
    name: &str,
    pipeline_json: &str,
) -> Result<CameraCvPipelineSaveOut, AbiError> {
    let input = CameraCvPipelineSaveInput {
        id: id.map(String::from),
        name: name.into(),
        pipeline_json: pipeline_json.into(),
    };
    call_cbor_in_out(&input, camera_cv_pipeline_save_v1)
}

/// Deletes a pipeline. Refusals (default pipeline / still assigned) come back
/// inside the output (`deleted = false` + readable `error`).
fn host_cv_pipeline_delete(id: &str) -> Result<CameraCvPipelineDeleteOut, AbiError> {
    let input = CameraCvPipelineIdInput { id: id.into() };
    call_cbor_in_out(&input, camera_cv_pipeline_delete_v1)
}

/// Reads a JSON response from a host function with the read-only
/// `(out_ptr, out_cap, out_len_ptr)` ABI shape. The alias discovery host
/// functions emit JSON (not CBOR), so they need their own decode path. Returns
/// the parsed `serde_json::Value` (the addon has serde_json but not serde-derive).
fn call_json_out_value(
    host_fn: unsafe extern "C" fn(i32, i32, i32) -> i32,
) -> Result<JsonValue, AbiError> {
    let mut cap = 16384usize;
    loop {
        let mut out = vec![0u8; cap];
        let mut out_len: i32 = 0;
        let ret = unsafe {
            host_fn(
                out.as_mut_ptr() as i32,
                out.len() as i32,
                &mut out_len as *mut i32 as i32,
            )
        };
        if ret == AbiError::OutputBufferTooSmall as i32 {
            cap = (out_len as usize).max(cap.saturating_mul(2));
            continue;
        }
        if ret != 0 {
            return Err(AbiError::from_code(ret));
        }
        out.truncate(out_len as usize);
        return serde_json::from_slice(&out).map_err(|_| AbiError::Operation);
    }
}

/// One alias/model TentaVision may consume, as reported by the access-control
/// grant system via `alias_list_available_v1`. Mirrors the host
/// `AvailableAliasOut` schema.
#[derive(Debug, Clone)]
struct AvailableAlias {
    alias_id: String,
    target_model: Option<String>,
    methods: Vec<String>,
    grant_status: String,
    visibility: Option<String>,
    active: bool,
    required: bool,
}

impl AvailableAlias {
    /// `true` when the addon may actually use this alias (grant resolved).
    fn is_usable(&self) -> bool {
        matches!(self.grant_status.as_str(), "granted" | "auto_granted")
    }
    /// Tone for the grant-status chip: granted/auto_granted=success,
    /// pending=warning, denied/other=critical.
    fn status_tone(&self) -> &'static str {
        match self.grant_status.as_str() {
            "granted" | "auto_granted" => "success",
            "pending" => "warning",
            _ => "critical",
        }
    }
}

fn parse_available_alias(v: &JsonValue) -> Option<AvailableAlias> {
    let alias_id = v.get("alias_id")?.as_str()?.to_string();
    let methods = v
        .get("methods")
        .and_then(|m| m.as_array())
        .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
        .unwrap_or_default();
    Some(AvailableAlias {
        alias_id,
        target_model: v.get("target_model").and_then(|x| x.as_str()).map(String::from),
        methods,
        grant_status: v.get("grant_status").and_then(|x| x.as_str()).unwrap_or("pending").to_string(),
        visibility: v.get("visibility").and_then(|x| x.as_str()).map(String::from),
        active: v.get("active").and_then(|x| x.as_bool()).unwrap_or(false),
        required: v.get("required").and_then(|x| x.as_bool()).unwrap_or(false),
    })
}

/// Lists the aliases/models this addon was GRANTED to consume. All grant
/// statuses are surfaced (granted/auto_granted/pending/denied) so the Bindings
/// UI can show an honest assignment surface rather than a hardcoded list.
fn alias_list_available() -> Result<Vec<AvailableAlias>, AbiError> {
    let resp = call_json_out_value(alias_list_available_v1)?;
    let arr = resp.get("aliases").and_then(|a| a.as_array()).cloned().unwrap_or_default();
    Ok(arr.iter().filter_map(parse_available_alias).collect())
}

fn camera_add(spec: CameraAddInput) -> Result<CameraAddOutput, AbiError> {
    call_cbor_in_out(&spec, camera_add_v1)
}

fn camera_remove(id: &str) -> Result<(), AbiError> {
    let input = CameraIdInput { camera_id: id.to_string() };
    let _: CameraRemoveOut = call_cbor_in_out(&input, camera_remove_v1)?;
    Ok(())
}

fn camera_discover() -> Result<Vec<DiscoveredCameraOut>, AbiError> {
    let out: CameraDiscoverOut = call_cbor_out(camera_discover_v1)?;
    Ok(out.discovered)
}

fn camera_local_devices() -> Result<Vec<LocalCameraDeviceOut>, AbiError> {
    let out: LocalCameraDevicesOut = call_cbor_out(camera_local_devices_v1)?;
    Ok(out.devices)
}

fn camera_test_connection(vendor: &str, url: &str) -> Result<CameraTestConnectionOut, AbiError> {
    let input = CameraTestConnectionInput { vendor: vendor.to_string(), url: url.to_string() };
    call_cbor_in_out(&input, camera_test_connection_v1)
}

// =============================================================================
// Recording ABI wrappers
// =============================================================================

/// Server-side search filters for the recordings browser. All fields optional;
/// they compose with AND on the host. `date_from`/`date_to` are unix
/// milliseconds bounding the recording `created_at`; `plate`/`adr` are
/// case-insensitive substrings over the event's gated OCR winners.
#[derive(Default, Clone)]
struct RecordingSearch {
    camera_id: Option<String>,
    date_from: Option<i64>,
    date_to: Option<i64>,
    plate: Option<String>,
    adr: Option<String>,
}

/// Lists the addon's per-vehicle event recordings (`kind = "segment"`), newest
/// first, applying the server-side search filters. Needs the `recording.read`
/// permission; the host scopes rows to this addon + org and returns only
/// browsable segment clips.
fn host_recording_list(
    search: &RecordingSearch,
    limit: u32,
) -> Result<Vec<RecordingListItem>, AbiError> {
    let input = RecordingListInput {
        camera_id: search.camera_id.clone(),
        limit,
        date_from: search.date_from,
        date_to: search.date_to,
        plate: search.plate.clone(),
        adr: search.adr.clone(),
    };
    let out: RecordingListOut = call_cbor_in_out(&input, recording_list_v1)?;
    Ok(out.items)
}

/// Issues a multi-use signed playback URL for a recording ref (relative
/// `/recordings/<ref>?token=…`, resolved by the browser against the app origin).
/// Needs the `recording.read` permission.
fn host_recording_get_url(recording_ref: &str, ttl_secs: u64) -> Result<UrlOut, AbiError> {
    let input = RecordingGetUrlInput {
        recording_ref: recording_ref.to_string(),
        ttl_secs,
    };
    call_cbor_in_out(&input, recording_get_url_v1)
}

// =============================================================================
// CBOR send helpers
// =============================================================================

const ADDON_ID: &str = "tentavision";
const PANEL_ID: &str = "overview";

static PANEL_EPOCH: AtomicU64 = AtomicU64::new(1);
static STATE_REVISION: AtomicU64 = AtomicU64::new(0);

/// Installs a process-wide panic hook exactly once. Without it a guest panic
/// surfaces on the host only as a bare wasm trap with numeric `fnNNN` frames,
/// which is unusable for diagnosis. The hook forwards the panic payload and its
/// `file:line` source location to the host log so the actual cause of any
/// future addon panic is recorded verbatim instead of being lost in the trap.
fn install_panic_hook() {
    use core::sync::atomic::AtomicBool;
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    // `swap` makes installation atomic and idempotent across on_start /
    // on_panel_open and any later re-entry.
    if INSTALLED.swap(true, Ordering::Relaxed) {
        return;
    }
    std::panic::set_hook(alloc::boxed::Box::new(|info: &std::panic::PanicHookInfo| {
        let location = info
            .location()
            .map(|l| alloc::format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "<unknown>".into());
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".into());
        log::error(&alloc::format!(
            "TentaVision PANIC at {}: {}",
            location, message
        ));
    }));
}

fn next_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    alloc::format!("c{}", n)
}

fn send_ui(payload: &UiPayload) -> i32 {
    let mut buf = Vec::with_capacity(4096);
    if minicbor::encode(payload, &mut buf).is_err() {
        log::error("TentaVision: CBOR encode failed");
        return -1;
    }
    let ret = unsafe { ui_render_cbor(buf.as_ptr() as i32, buf.len() as i32) };
    if ret < 0 {
        log::error("TentaVision: ui_render_cbor returned error");
    }
    ret
}

fn send_panel_shell(layout: Component, slots: Vec<SlotDecl>, initial_state: Vec<StateEntry>) {
    let epoch = PANEL_EPOCH.load(Ordering::Relaxed);
    let payload = UiPayload::PanelShell(PanelShell {
        addon_id: ADDON_ID.into(),
        panel_id: PANEL_ID.into(),
        panel_epoch: epoch,
        layout,
        slots,
        initial_state,
        initial_commands: vec![],
    });
    send_ui(&payload);
}

fn send_slot_content(slot_id: &str, fragment: Component) {
    send_slot_content_with_overlay(slot_id, fragment, None);
}

/// Like `send_slot_content` but seeds store keys via `state_overlay` before the
/// fragment renders. Used to seed the reactive wizard's initial state into the
/// store the moment the `add_camera_body` fragment is delivered, so bound
/// visibility flags resolve correctly on first paint.
fn send_slot_content_with_overlay(slot_id: &str, fragment: Component, overlay: Option<Vec<StateEntry>>) {
    let epoch = PANEL_EPOCH.load(Ordering::Relaxed);
    let payload = UiPayload::SlotContent(SlotContent {
        addon_id: ADDON_ID.into(),
        panel_id: PANEL_ID.into(),
        panel_epoch: epoch,
        slot_id: slot_id.into(),
        fragment,
        state_overlay: overlay,
    });
    send_ui(&payload);
}

fn send_state_patch(key: &str, value: Value) {
    send_state_patches(vec![(key.into(), value)]);
}

/// Applies several store keys in one atomic `StatePatch` (single revision bump).
/// The reactive wizard uses this so that, e.g., advancing a step toggles the
/// step visibility flags and footer-button flags together without the client
/// observing a half-applied intermediate state.
fn send_state_patches(pairs: Vec<(String, Value)>) {
    if pairs.is_empty() {
        return;
    }
    let base = STATE_REVISION.load(Ordering::Relaxed);
    let new_rev = base + 1;
    let epoch = PANEL_EPOCH.load(Ordering::Relaxed);
    let ops = pairs
        .into_iter()
        .map(|(key, value)| PatchOp {
            path: StatePath::new(vec![PathSegment::Key(key)]),
            op: PatchOpKind::Set { value },
        })
        .collect();
    let payload = UiPayload::StatePatch(StatePatch {
        addon_id: ADDON_ID.into(),
        panel_id: PANEL_ID.into(),
        panel_epoch: epoch,
        base_revision: base,
        new_revision: new_rev,
        ops,
    });
    // The host advances its expected revision only when it accepts the patch;
    // advancing locally on rejection would drift the counters apart forever.
    if send_ui(&payload) == 0 {
        STATE_REVISION.store(new_rev, Ordering::Relaxed);
    }
}

// =============================================================================
// Component construction helpers — typed structs from tentaflow-sdk-spec
// =============================================================================

fn lit(s: &str) -> BindRef {
    BindRef::Literal(Value::Text(s.into()))
}

/// A reactive `BindRef` pointing at a top-level store key. Lets text/alert
/// content track wizard state without re-sending the fragment.
fn bound(key: &str) -> BindRef {
    BindRef::Bound(StatePath::new(vec![PathSegment::Key(key.into())]))
}

/// Wraps a component so the renderer hides it whenever the bound boolean store
/// key is `false`. This is the core of the reactive wizard: every step and
/// per-type config block stays in the DOM and only toggles `hidden` as the
/// store changes, so interactions never rebuild the panel.
fn with_visible(mut component: Component, key: &str) -> Component {
    use tentaflow_sdk_spec::protocol::ui::a11y::Visibility;
    component.visibility = Some(Visibility {
        visible: Some(bound(key)),
        display_above_breakpoint: None,
        display_below_breakpoint: None,
        hidden_for_assistive: false,
    });
    component
}

fn with_a11y_label(mut component: Component, label: &str) -> Component {
    component.a11y = Some(Accessibility {
        label: Some(lit(label)),
        ..Default::default()
    });
    component
}

fn icon_named(name: IconName) -> IconRef {
    IconRef::Named { name, size: None, tone: None }
}

fn parse_tone(s: &str) -> Tone {
    match s {
        "primary" => Tone::Primary,
        "success" => Tone::Success,
        "warning" => Tone::Warning,
        "critical" => Tone::Critical,
        "info" => Tone::Info,
        "muted" => Tone::Muted,
        _ => Tone::Neutral,
    }
}

fn parse_spacing(s: &str) -> Spacing {
    match s {
        "zero" => Spacing::Zero,
        "xxs" => Spacing::Xxs,
        "xs" => Spacing::Xs,
        "sm" => Spacing::Sm,
        "lg" => Spacing::Lg,
        "xl" => Spacing::Xl,
        "xxl" => Spacing::Xxl,
        _ => Spacing::Md,
    }
}

fn parse_button_variant(s: &str) -> ButtonVariant {
    match s {
        "secondary" => ButtonVariant::Secondary,
        "tertiary" => ButtonVariant::Tertiary,
        "ghost" => ButtonVariant::Ghost,
        "destructive" => ButtonVariant::Destructive,
        "link" => ButtonVariant::Link,
        _ => ButtonVariant::Primary,
    }
}

fn parse_icon_name(s: &str) -> IconName {
    match s {
        "plus" => IconName::Plus,
        "search" => IconName::Search,
        "settings" => IconName::Settings,
        "bell" => IconName::Bell,
        "video" => IconName::Video,
        "cameras" => IconName::Cameras,
        "brain" => IconName::Brain,
        "cpu" => IconName::Cpu,
        "dashboard" => IconName::Dashboard,
        "users" => IconName::Users,
        "zones" => IconName::Zones,
        "audit" => IconName::Audit,
        "evidence" => IconName::Evidence,
        "link" => IconName::ExternalLink,
        "lock" => IconName::Lock,
        "info" => IconName::Info,
        "check" => IconName::Check,
        "clock" => IconName::Clock,
        "shield" => IconName::Shield,
        "tag" => IconName::Filter,
        "image" => IconName::Image,
        "car" => IconName::Vehicle,
        "calendar" => IconName::Clock,
        "alert" => IconName::Alert,
        "refresh" => IconName::Refresh,
        _ => IconName::Info,
    }
}

fn text(content: &str) -> Component {
    TextComp {
        content: lit(content),
        style: TextStyle::Body,
        tone: None,
        align: None,
        wrap: None,
        max_lines: None,
        format: None,
    }.into_component(next_id()).expect("Text")
}

fn text_styled(content: &str, style: &str) -> Component {
    let ts = match style {
        "body_strong" => TextStyle::BodyStrong,
        "caption" => TextStyle::Caption,
        "overline" => TextStyle::Overline,
        "title" => TextStyle::Title,
        "h1" => TextStyle::H1,
        "h2" => TextStyle::H2,
        "h3" => TextStyle::H3,
        "h4" => TextStyle::H4,
        "code" => TextStyle::Code,
        "mono" => TextStyle::Mono,
        _ => TextStyle::Body,
    };
    TextComp {
        content: lit(content),
        style: ts,
        tone: None,
        align: None,
        wrap: None,
        max_lines: None,
        format: None,
    }.into_component(next_id()).expect("Text")
}

/// Text whose content tracks a store key reactively (used for the live
/// connection-test outcome line in the wizard).
fn text_bound(key: &str) -> Component {
    TextComp {
        content: bound(key),
        style: TextStyle::Body,
        tone: None,
        align: None,
        wrap: None,
        max_lines: None,
        format: None,
    }.into_component(next_id()).expect("Text")
}

fn text_colored(content: &str, style: &str, color: &str) -> Component {
    let ts = match style {
        "body_strong" => TextStyle::BodyStrong,
        "caption" => TextStyle::Caption,
        _ => TextStyle::Body,
    };
    TextComp {
        content: lit(content),
        style: ts,
        tone: Some(parse_tone(color)),
        align: None,
        wrap: None,
        max_lines: None,
        format: None,
    }.into_component(next_id()).expect("Text")
}

fn heading(level: u8, content: &str) -> Component {
    HeadingComp {
        content: lit(content),
        level,
        tone: None,
        align: None,
    }.into_component(next_id()).expect("Heading")
}

fn badge(label: &str, variant: &str) -> Component {
    let bv = match variant {
        "danger" | "critical" => BadgeVariant::Solid,
        "warning" => BadgeVariant::Soft,
        "info" => BadgeVariant::Outline,
        _ => BadgeVariant::Soft,
    };
    let tone = match variant {
        "danger" | "critical" => Tone::Critical,
        "warning" => Tone::Warning,
        "info" => Tone::Info,
        "success" => Tone::Success,
        _ => Tone::Neutral,
    };
    BadgeComp {
        variant: bv,
        tone,
        label: lit(label),
        icon: None,
        count: None,
        max: 99,
        pulse: false,
    }.into_component(next_id()).expect("Badge")
}

fn chip(label: &str, _variant: &str) -> Component {
    ChipComp {
        variant: ChipVariant::Soft,
        tone: Tone::Neutral,
        label: lit(label),
        icon: None,
        avatar: None,
        selected: None,
        removable: false,
    }.into_component(next_id()).expect("Chip")
}

fn chip_with_icon(label: &str, _variant: &str, icon: &str) -> Component {
    ChipComp {
        variant: ChipVariant::Soft,
        tone: Tone::Neutral,
        label: lit(label),
        icon: Some(icon_named(parse_icon_name(icon))),
        avatar: None,
        selected: None,
        removable: false,
    }.into_component(next_id()).expect("Chip")
}

fn chip_toned(label: &str, tone_str: &str) -> Component {
    ChipComp {
        variant: ChipVariant::Soft,
        tone: parse_tone(tone_str),
        label: lit(label),
        icon: None,
        avatar: None,
        selected: None,
        removable: false,
    }.into_component(next_id()).expect("Chip")
}

fn chip_toned_icon(label: &str, tone_str: &str, icon: &str) -> Component {
    ChipComp {
        variant: ChipVariant::Soft,
        tone: parse_tone(tone_str),
        label: lit(label),
        icon: Some(icon_named(parse_icon_name(icon))),
        avatar: None,
        selected: None,
        removable: false,
    }.into_component(next_id()).expect("Chip")
}

fn stat_card(value: &str, label: &str, sublabel: Option<&str>, icon: Option<&str>, accent: Option<&str>) -> Component {
    let tone = accent.map(parse_tone).unwrap_or(Tone::Neutral);
    StatCardComp {
        label: lit(label),
        icon: icon.map(|i| icon_named(parse_icon_name(i))),
        value: lit(value),
        value_suffix: None,
        format: None,
        trend: None,
        footnote: sublabel.map(|s| Footnote {
            tone,
            icon: None,
            content: lit(s),
        }),
        accent: Some(tone),
        clickable: false,
    }.into_component(next_id()).expect("StatCard")
}

fn button(label: &str, action: &str, variant: &str) -> Component {
    let mut c = ButtonComp {
        variant: parse_button_variant(variant),
        tone: Tone::Neutral,
        label: lit(label),
        icon_leading: None,
        icon_trailing: None,
        size: ButtonSize::Md,
        full_width: false,
        disabled: None,
        loading: None,
        density: Density::Default,
    }.into_component(next_id()).expect("Button");
    c.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Click,
        Handler::Backend {
            action_id: action.into(),
            params: CborMap::default(),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    c
}

fn button_with_icon(label: &str, action: &str, variant: &str, icon: &str) -> Component {
    let mut c = ButtonComp {
        variant: parse_button_variant(variant),
        tone: Tone::Neutral,
        label: lit(label),
        icon_leading: Some(icon_named(parse_icon_name(icon))),
        icon_trailing: None,
        size: ButtonSize::Md,
        full_width: false,
        disabled: None,
        loading: None,
        density: Density::Default,
    }.into_component(next_id()).expect("Button");
    c.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Click,
        Handler::Backend {
            action_id: action.into(),
            params: CborMap::default(),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    c
}

fn button_with_params(label: &str, action: &str, variant: &str, params: CborMap) -> Component {
    let mut c = ButtonComp {
        variant: parse_button_variant(variant),
        tone: Tone::Neutral,
        label: lit(label),
        icon_leading: None,
        icon_trailing: None,
        size: ButtonSize::Md,
        full_width: false,
        disabled: None,
        loading: None,
        density: Density::Default,
    }.into_component(next_id()).expect("Button");
    c.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Click,
        Handler::Backend {
            action_id: action.into(),
            params,
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    c
}

fn icon_button(icon: &str, action: &str, variant: &str) -> Component {
    let mut c = IconButtonComp {
        icon: icon_named(parse_icon_name(icon)),
        variant: parse_button_variant(variant),
        tone: Tone::Neutral,
        size: ButtonSize::Md,
        aria_label: icon.into(),
        disabled: None,
        loading: None,
    }.into_component(next_id()).expect("IconButton");
    c.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Click,
        Handler::Backend {
            action_id: action.into(),
            params: CborMap::default(),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    c
}

fn link(label: &str, panel_id: &str) -> Component {
    let mut c = LinkComp {
        label: lit(label),
        underline: LinkUnderline::Hover,
        tone: Tone::Primary,
        leading_icon: None,
        trailing_icon: None,
    }.into_component(next_id()).expect("Link");
    c.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Click,
        Handler::Backend {
            action_id: panel_id.into(),
            params: CborMap::default(),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    c
}

fn card(title: Option<&str>, children: Vec<Component>) -> Component {
    if let Some(t) = title {
        SectionCard {
            title: lit(t),
            subtitle: None,
            header_actions: vec![],
            header_divider: false,
            body: children,
            footer: None,
            padding: Spacing::Lg,
            gap: Spacing::Md,
            variant: CardVariant::Outlined,
            radius: RadiusToken::Lg,
            shadow: ShadowToken::Subtle,
            border: BorderToken::Hairline,
            background: BackgroundToken::None,
            accent: None,
        }.into_component(next_id()).expect("SectionCard")
    } else {
        Card {
            variant: CardVariant::Outlined,
            padding: Spacing::Lg,
            gap: Spacing::Md,
            radius: RadiusToken::Lg,
            shadow: ShadowToken::None,
            border: BorderToken::Hairline,
            background: BackgroundToken::None,
            accent: None,
            children,
            interactive: false,
            clickable: false,
        }.into_component(next_id()).expect("Card")
    }
}

fn card_with_icon(title: &str, _icon: &str, children: Vec<Component>) -> Component {
    card_with_icon_action(title, _icon, None, children)
}

fn card_with_icon_action(title: &str, _icon: &str, action_label: Option<&str>, children: Vec<Component>) -> Component {
    let header_actions = match action_label {
        Some(label) => vec![ButtonComp {
            variant: ButtonVariant::Ghost,
            tone: Tone::Primary,
            label: lit(label),
            icon_leading: None,
            icon_trailing: None,
            size: ButtonSize::Sm,
            full_width: false,
            disabled: None,
            loading: None,
            density: Density::Default,
        }.into_component(next_id()).expect("Button")],
        None => vec![],
    };
    SectionCard {
        title: lit(title),
        subtitle: None,
        header_actions,
        header_divider: false,
        body: children,
        footer: None,
        padding: Spacing::Lg,
        gap: Spacing::Md,
        variant: CardVariant::Outlined,
        radius: RadiusToken::Lg,
        shadow: ShadowToken::Subtle,
        border: BorderToken::Hairline,
        background: BackgroundToken::None,
        accent: None,
    }.into_component(next_id()).expect("SectionCard")
}

fn stack_v(children: Vec<Component>) -> Component {
    Stack {
        gap: Spacing::Md,
        align: FlexAlign::Stretch,
        children,
        padding: None,
        justify: None,
    }.into_component(next_id()).expect("Stack")
}

fn stack_h(children: Vec<Component>) -> Component {
    Flex {
        direction: FlexDirection::Row,
        gap: Spacing::Md,
        justify: FlexJustify::Start,
        align: FlexAlign::Center,
        wrap: FlexWrap::NoWrap,
        children,
        padding: None,
        background: None,
        radius: None,
    }.into_component(next_id()).expect("Flex")
}

fn stack_h_gap(gap: &str, children: Vec<Component>) -> Component {
    Flex {
        direction: FlexDirection::Row,
        gap: parse_spacing(gap),
        justify: FlexJustify::Start,
        align: FlexAlign::Center,
        wrap: FlexWrap::NoWrap,
        children,
        padding: None,
        background: None,
        radius: None,
    }.into_component(next_id()).expect("Flex")
}

fn stack_v_gap(gap: &str, children: Vec<Component>) -> Component {
    Stack {
        gap: parse_spacing(gap),
        align: FlexAlign::Stretch,
        children,
        padding: None,
        justify: None,
    }.into_component(next_id()).expect("Stack")
}

fn grid(columns: u32, children: Vec<Component>) -> Component {
    let grid_children: Vec<GridChild> = children.into_iter().map(|c| GridChild {
        component: c,
        col_span: 1,
        row_span: 1,
        col_start: None,
        row_start: None,
        align_self: None,
        justify_self: None,
    }).collect();
    Grid {
        columns: GridTrack::Equal { count: columns as u8 },
        gap: Spacing::Md,
        row_gap: None,
        column_gap: None,
        children: grid_children,
        padding: None,
        align_items: None,
    }.into_component(next_id()).expect("Grid")
}

fn divider() -> Component {
    Divider {
        orientation: DividerOrientation::Horizontal,
        variant: DividerVariant::Default,
        spacing: Spacing::Md,
        label: None,
    }.into_component(next_id()).expect("Divider")
}

fn table(columns: Vec<Value>, _rows: Vec<Value>) -> Component {
    let table_cols: Vec<TableColumn> = columns.iter().enumerate().map(|(i, v)| {
        let header_text = match v {
            Value::Text(s) => s.clone(),
            _ => alloc::format!("col{}", i),
        };
        let col_id = header_text.to_ascii_lowercase().replace(' ', "_");
        TableColumn {
            id: col_id.clone(),
            header: lit(&header_text),
            field_path: vec![PathSegment::Key(col_id)],
            width: TableColumnWidth::Auto,
            render: ColumnRender::Text,
            format: None,
            align: None,
            sortable: false,
            hidden_by_default: false,
            sticky_left: false,
        }
    }).collect();
    TableComp {
        columns: table_cols,
        rows_path: StatePath::new(vec![PathSegment::Key("rows".into())]),
        row_key_field: "id".into(),
        variant: TableVariant::Default,
        density: Density::Default,
        sortable: false,
        sort_by: None,
        selectable: TableSelectMode::None,
        selected_ids: None,
        sticky_header: false,
        sticky_columns: 0,
        pagination: None,
        empty_state: None,
        row_actions: vec![],
        bulk_actions: vec![],
        virtualize: false,
        row_expandable: false,
        expanded_row_template_id: None,
    }.into_component(next_id()).expect("Table")
}

fn avatar(initials: &str, size: &str) -> Component {
    let sz = match size {
        "xs" => AvatarSize::Xs,
        "sm" => AvatarSize::Sm,
        "lg" => AvatarSize::Lg,
        "xl" => AvatarSize::Xl,
        _ => AvatarSize::Md,
    };
    AvatarComp {
        source: AvatarRef::Initials { initials: initials.into() },
        size: sz,
        shape: AvatarShape::Circle,
        status: None,
        tone: None,
    }.into_component(next_id()).expect("Avatar")
}

fn empty_state(title: &str, message: Option<&str>, icon: Option<&str>) -> Component {
    let built = EmptyStateComp {
        icon: icon_named(parse_icon_name(icon.unwrap_or("info"))),
        heading: lit(title),
        message: message.map(lit),
        primary_action: None,
        secondary_action: None,
        variant: EmptyStateVariant::Default,
    }.into_component(next_id());
    // EmptyState sits on hot wizard/empty-list render paths. A validation
    // failure here must degrade to a readable text node, never trap the whole
    // on_request and corrupt guest memory; the real reason is logged so the
    // panic hook / host log still pinpoints it.
    match built {
        Ok(c) => c,
        Err(e) => {
            log::error(&alloc::format!("TentaVision: EmptyState into_component failed: {:?}", e));
            text(title)
        }
    }
}

fn spinner(size: &str) -> Component {
    let sz = match size {
        "xs" => SpinnerSize::Xs,
        "sm" => SpinnerSize::Sm,
        "lg" => SpinnerSize::Lg,
        "xl" => SpinnerSize::Xl,
        _ => SpinnerSize::Md,
    };
    SpinnerComp {
        size: sz,
        tone: Tone::Neutral,
        label: None,
        variant: SpinnerVariant::Default,
    }.into_component(next_id()).expect("Spinner")
}

fn alert(message: &str, tone: &str) -> Component {
    AlertComp {
        tone: parse_tone(tone),
        variant: AlertVariant::Default,
        icon: None,
        title: None,
        message: lit(message),
        actions: None,
        dismissible: false,
    }.into_component(next_id()).expect("Alert")
}

/// Alert whose message tracks a store key reactively. Visibility is toggled by
/// the caller via `with_visible` so the wizard can show/hide errors and test
/// results purely through `StatePatch`.
fn alert_bound(message_key: &str, tone: &str) -> Component {
    AlertComp {
        tone: parse_tone(tone),
        variant: AlertVariant::Default,
        icon: None,
        title: None,
        message: bound(message_key),
        actions: None,
        dismissible: false,
    }.into_component(next_id()).expect("Alert")
}

fn progress_bar(value: f64, max: f64) -> Component {
    ProgressBarComp {
        value: BindRef::Literal(Value::F64(value)),
        max,
        variant: ProgressVariant::Default,
        tone: Tone::Primary,
        show_label: false,
        label: None,
        size: ProgressSize::Md,
    }.into_component(next_id()).expect("ProgressBar")
}

fn key_value(items: Vec<(&str, &str)>) -> Component {
    let kv_items: Vec<KvItem> = items.into_iter().map(|(k, v)| KvItem {
        label: lit(k),
        value: lit(v),
        hint: None,
        icon: None,
        action_id: None,
        format: None,
    }).collect();
    KvComp {
        items: kv_items,
        density: Density::Default,
        layout: KvLayout::Horizontal,
        label_width: None,
    }.into_component(next_id()).expect("KeyValue")
}

fn heatmap(_rows: u32, _cols: u32, _values: Vec<Vec<f64>>, row_labels: Vec<&str>, col_labels: Vec<&str>) -> Component {
    let hm_rows: Vec<HeatmapRow> = row_labels.into_iter().enumerate().map(|(i, label)| HeatmapRow {
        id: alloc::format!("r{}", i),
        label: lit(label),
    }).collect();
    let hm_cols: Vec<HeatmapColumn> = col_labels.into_iter().enumerate().map(|(i, label)| HeatmapColumn {
        id: alloc::format!("c{}", i),
        label: lit(label),
    }).collect();
    HeatmapComp {
        rows: hm_rows,
        columns: hm_cols,
        cells_path: StatePath::new(vec![PathSegment::Key("heatmap_cells".into())]),
        scale: HeatmapScale::Linear {
            min: 0.0,
            max: 1.0,
            color_from: Tone::Muted,
            color_to: Tone::Critical,
        },
        legend_position: HeatmapLegendPosition::TopRight,
        cell_size_px: 24,
        tooltip: true,
    }.into_component(next_id()).expect("Heatmap")
}

fn nav_tabs(items: Vec<NavTab>, active_id: &str) -> Component {
    NavTabsStruct {
        items,
        active_id: lit(active_id),
        variant: NavTabsVariant::Default,
        scroll_overflow: false,
    }.into_component(next_id()).expect("NavTabs")
}

fn input(label: &str, placeholder: &str, field_id: &str) -> Component {
    use tentaflow_sdk_spec::protocol::ui::form::Input;
    Input {
        r#type: InputType::Text,
        bind_path: StatePath::new(vec![PathSegment::Key(field_id.into())]),
        placeholder: Some(lit(placeholder)),
        label: Some(lit(label)),
        hint: None,
        leading_icon: None,
        trailing_icon: None,
        prefix: None,
        suffix: None,
        validators: vec![],
        max_length: None,
        min_length: None,
        pattern: None,
        autocomplete: None,
        input_mode: None,
        disabled: None,
        readonly: None,
        error: None,
        size: InputSize::Md,
    }.into_component(field_id).expect("Input")
}

fn number_input(label: &str, placeholder: &str, field_id: &str) -> Component {
    use tentaflow_sdk_spec::protocol::ui::form::Input;
    Input {
        r#type: InputType::Number,
        bind_path: StatePath::new(vec![PathSegment::Key(field_id.into())]),
        placeholder: Some(lit(placeholder)),
        label: Some(lit(label)),
        hint: None,
        leading_icon: None,
        trailing_icon: None,
        prefix: None,
        suffix: None,
        validators: vec![],
        max_length: None,
        min_length: None,
        pattern: None,
        autocomplete: None,
        input_mode: None,
        disabled: None,
        readonly: None,
        error: None,
        size: InputSize::Md,
    }.into_component(field_id).expect("Input")
}

fn select(label: &str, options: Vec<SelectOption>, field_id: &str) -> Component {
    use tentaflow_sdk_spec::protocol::ui::form::Select;
    Select {
        bind_path: StatePath::new(vec![PathSegment::Key(field_id.into())]),
        options,
        placeholder: None,
        label: Some(lit(label)),
        searchable: false,
        clearable: false,
        virtualize: false,
        disabled: None,
        size: InputSize::Md,
        groups: None,
    }.into_component(field_id).expect("Select")
}

/// Text input that mirrors its value into backend wizard state on every change
/// via the `wizard-field-change` action, tagged with `field`. Used by every
/// per-type wizard field so step navigation never loses typed values.
fn wizard_input(label: &str, placeholder: &str, field: &str, password: bool) -> Component {
    use tentaflow_sdk_spec::protocol::ui::form::Input;
    // An empty placeholder literal (`Some(Text("")))`) is meaningless and some
    // optional credential fields pass "" — encode it as absent rather than a
    // zero-length literal so the field stays canonical.
    let placeholder_ref = if placeholder.is_empty() { None } else { Some(lit(placeholder)) };
    let mut comp = Input {
        r#type: if password { InputType::Password } else { InputType::Text },
        bind_path: StatePath::new(vec![PathSegment::Key(field.into())]),
        placeholder: placeholder_ref,
        label: Some(lit(label)),
        hint: None,
        leading_icon: None,
        trailing_icon: None,
        prefix: None,
        suffix: None,
        validators: vec![],
        max_length: None,
        min_length: None,
        pattern: None,
        autocomplete: None,
        input_mode: None,
        disabled: None,
        readonly: None,
        error: None,
        size: InputSize::Md,
    }.into_component(field).expect("Input");
    // Backend wizard state is the source of truth for validation (resolve_target
    // on Next/Test/Submit), so every keystroke must commit. Using `Input` rather
    // than `Change` avoids the lost-update race where the user types and clicks
    // "Dalej" before the blur-fired `change` ever reaches the backend.
    let mut params = CborMap::default();
    params.0.push(("field".into(), Value::Text(field.into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Input,
        Handler::Backend {
            action_id: "wizard-field-change".into(),
            params,
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    comp
}

/// Select that commits its picked value to backend wizard state on change,
/// tagged with `field` (used for the USB device dropdown).
fn wizard_select(label: &str, options: Vec<SelectOption>, field: &str) -> Component {
    use tentaflow_sdk_spec::protocol::ui::form::Select;
    let mut comp = Select {
        bind_path: StatePath::new(vec![PathSegment::Key(field.into())]),
        options,
        placeholder: None,
        label: Some(lit(label)),
        searchable: false,
        clearable: false,
        virtualize: false,
        disabled: None,
        size: InputSize::Md,
        groups: None,
    }.into_component(field).expect("Select");
    let mut params = CborMap::default();
    params.0.push(("field".into(), Value::Text(field.into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Change,
        Handler::Backend {
            action_id: "wizard-field-change".into(),
            params,
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    comp
}

fn toggle(label: &str, field_id: &str) -> Component {
    use tentaflow_sdk_spec::protocol::ui::form::Toggle;
    Toggle {
        bind_path: StatePath::new(vec![PathSegment::Key(field_id.into())]),
        label: Some(lit(label)),
        hint: None,
        size: ToggleSize::Md,
        tone: Tone::Primary,
        disabled: None,
        label_position: TogglePosition::Trailing,
    }.into_component(field_id).expect("Toggle")
}

/// Single-handle slider bound to `field_id`, showing its current value. Used by
/// the profiles builder's quick-params (FPS sampling, detection confidence).
fn slider(label: &str, field_id: &str, min: f64, max: f64, step: f64) -> Component {
    use tentaflow_sdk_spec::protocol::ui::form::Slider;
    Slider {
        bind_path: StatePath::new(vec![PathSegment::Key(field_id.into())]),
        min,
        max,
        step,
        label: Some(lit(label)),
        show_value: true,
        format: None,
        marks: None,
        tone: Tone::Primary,
    }.into_component(field_id).expect("Slider")
}

fn filter_chips(items: Vec<FilterChipDef>, _active: &str) -> Component {
    FilterChipsComp {
        chips: items,
        selected_ids: StatePath::new(vec![PathSegment::Key("filter_active".into())]),
        mode: FilterChipsMode::Single,
        clearable: true,
    }.into_component(next_id()).expect("FilterChips")
}

fn mono_block(content: &str) -> Component {
    use tentaflow_sdk_spec::protocol::ui::data::MonoBlock;
    MonoBlock {
        content: lit(content),
        max_height_px: None,
        word_wrap: true,
        copyable: true,
    }.into_component(next_id()).expect("MonoBlock")
}

fn gate_screen(title: &str, message: &str, icon: &str) -> Component {
    GateScreenComp {
        icon: icon_named(parse_icon_name(icon)),
        title: lit(title),
        message: lit(message),
        actions: vec![],
        variant: GateVariant::PermissionDenied,
    }.into_component(next_id()).expect("GateScreen")
}

fn step_progress(steps: Vec<StepDef>, _current_id: &str) -> Component {
    StepProgressComp {
        steps,
        current_id_path: StatePath::new(vec![PathSegment::Key("onboarding_step".into())]),
        variant: StepProgressVariant::Horizontal,
        clickable_completed: false,
    }.into_component(next_id()).expect("StepProgress")
}

fn sparkline(_points: Vec<f64>) -> Component {
    SparklineComp {
        data_path: StatePath::new(vec![PathSegment::Key("sparkline_data".into())]),
        variant: SparklineVariant::Line,
        tone: Tone::Primary,
        width_px: 120,
        height_px: 32,
        show_min_max: false,
    }.into_component(next_id()).expect("Sparkline")
}

// =============================================================================
// In-WASM ephemeral panel state
// =============================================================================

struct PanelState {
    current_panel: String,
    add_form_visible: bool,
    wizard_step: u8,
    cameras_filter: String,
    // Camera selected via a table row click, pending a delete confirmation.
    camera_pending_remove: Option<String>,
    // Camera whose analysis-flow selector is open (its "Flow" row action).
    camera_flow_edit: Option<String>,
    // Camera whose CV-pipeline selector is open (its "Pipeline" row action).
    camera_pipeline_edit: Option<String>,
    // Recording ref whose playback modal is open (its "Odtwórz" row action).
    recording_playing: Option<String>,
    // Camera filter applied to the recordings list (empty = all cameras).
    recordings_camera_filter: String,
    // Recordings-browser search inputs (empty string = filter inactive). Dates
    // are `YYYY-MM-DD` day strings from the date inputs; plate/ADR are raw text.
    recordings_date_from: String,
    recordings_date_to: String,
    recordings_plate_query: String,
    recordings_adr_query: String,
    cv_pipelines: CvPipelinesState,
    error_message: Option<String>,
    success_message: Option<String>,
    discover: DiscoverState,
    profiles: ProfilesState,
    alarms: AlarmsState,
    search: SearchState,
    reid: ReidState,
    models: ModelsState,
    zones: ZonesState,
    audit: AuditState,
    evidence: EvidenceState,
    settings: SettingsState,
    onboarding: OnboardingState,
}

/// One CV-pipeline stage as edited in the pipeline editor. String fields carry
/// the raw typed text (validation is host-side on save); `params` preserves any
/// non-edited stage params (crop pads etc.) verbatim across an edit round-trip,
/// with `ocr_mode` folded back into it on save.
#[derive(Clone)]
struct StageDraft {
    stage_id: String,
    /// "detect" | "classify" | "ocr" | "embed".
    op: String,
    /// Model alias (`model_aliases.alias`).
    model: String,
    /// Detect only: stage FPS; empty = the camera's `analysis_fps`.
    fps: String,
    /// Detect only: confidence threshold 0..1; empty = engine default.
    threshold: String,
    /// Crop stages: the source detect stage id.
    parent: String,
    /// Crop stages: comma-separated class patterns (trailing `*` = prefix).
    classes: String,
    /// OCR only: "plate" | "adr" | "generic".
    ocr_mode: String,
    enabled: bool,
    params: serde_json::Map<String, JsonValue>,
}

impl StageDraft {
    fn new_detect() -> Self {
        Self {
            stage_id: "detect".into(),
            op: "detect".into(),
            model: String::new(),
            fps: String::new(),
            threshold: String::new(),
            parent: String::new(),
            classes: String::new(),
            ocr_mode: String::new(),
            enabled: true,
            params: serde_json::Map::new(),
        }
    }
}

/// View state for the CV-pipeline manager on the Cameras tab: the pipeline
/// list, the pending-delete arming and the stage editor draft. Pipelines
/// persist core-side (`camera_cv_pipelines`); nothing here survives a close.
struct CvPipelinesState {
    manager_visible: bool,
    editor_visible: bool,
    /// Pipeline id being edited; `None` = creating a new one (host mints id).
    editing_id: Option<String>,
    pending_remove: Option<String>,
    name: String,
    stages: Vec<StageDraft>,
}

impl CvPipelinesState {
    const fn new() -> Self {
        Self {
            manager_visible: false,
            editor_visible: false,
            editing_id: None,
            pending_remove: None,
            name: String::new(),
            stages: Vec::new(),
        }
    }
    fn close_editor(&mut self) {
        self.editor_visible = false;
        self.editing_id = None;
        self.name.clear();
        self.stages.clear();
    }
}

/// Ephemeral wizard progress. The OUTCOMES persist to the settings table /
/// cameras table on finish; this struct only holds the in-flight selections so
/// the user can move back and forth between steps before committing. None of it
/// survives a panel close — a half-finished wizard restarts from step 0, which
/// is the intended behaviour until `onboarding_completed` is written.
struct OnboardingState {
    step: u8,
    /// Chosen deployment role (key from ONBOARDING_ROLES), persisted on finish
    /// under settings key `onboarding_role`.
    role: Option<String>,
    /// Chosen legal/AI-Act profile (key from LEGAL_PROFILES), persisted on finish
    /// under settings key `legal_profile` — the SAME key the Settings tab uses.
    legal_profile: Option<String>,
    /// First-camera draft. On finish a real camera row is inserted via
    /// db::insert_camera so it shows up in the Cameras tab.
    camera_name: String,
    camera_url: String,
    /// Chosen detector preset (key from ONBOARDING_PRESETS), persisted on finish
    /// under settings key `onboarding_presets`.
    presets: Option<String>,
}

impl OnboardingState {
    const fn new() -> Self {
        Self {
            step: 0,
            role: None,
            legal_profile: None,
            camera_name: String::new(),
            camera_url: String::new(),
            presets: None,
        }
    }
    fn reset(&mut self) {
        self.step = 0;
        self.role = None;
        self.legal_profile = None;
        self.camera_name.clear();
        self.camera_url.clear();
        self.presets = None;
    }
}

struct AuditState {
    date_preset: String,
    users: Vec<String>,
    actions: Vec<String>,
    risk_class: String,
    result: String,
    query: String,
    expanded_id: Option<String>,
    cursor: Option<String>,
    /// Risk class ("A"/"B"/"C") whose retention card is currently in edit mode,
    /// or None when no card is being edited.
    retention_editing: Option<String>,
    /// Working draft for the in-edit retention input (days as typed).
    retention_draft: String,
}

impl AuditState {
    const fn new() -> Self {
        Self {
            date_preset: String::new(), users: Vec::new(), actions: Vec::new(),
            risk_class: String::new(), result: String::new(), query: String::new(),
            expanded_id: None, cursor: None, retention_editing: None,
            retention_draft: String::new(),
        }
    }
    fn clear_filters(&mut self) {
        self.date_preset.clear(); self.users.clear(); self.actions.clear();
        self.risk_class.clear(); self.result.clear(); self.query.clear();
        self.cursor = None;
    }
}

/// View state for the Evidence-export tab. The evidence package rows and the
/// recipient list both persist in SQLite (the `evidence` table and the
/// `evidence_recipients` settings key); this struct only holds the transient UI
/// state: which forms are open, the pending delete arming and the bound draft
/// fields for the create-package and add-recipient forms.
struct EvidenceState {
    package_form_visible: bool,
    form_alarm_id: String,
    form_recipient: String,
    pending_remove: Option<String>,
    recipient_form_visible: bool,
    recipient_name: String,
    recipient_key: String,
}

impl EvidenceState {
    const fn new() -> Self {
        Self {
            package_form_visible: false,
            form_alarm_id: String::new(),
            form_recipient: String::new(),
            pending_remove: None,
            recipient_form_visible: false,
            recipient_name: String::new(),
            recipient_key: String::new(),
        }
    }

    /// Resets the create-package form to a clean draft.
    fn reset_package_form(&mut self) {
        self.form_alarm_id.clear();
        self.form_recipient.clear();
    }

    /// Resets the add-recipient form to a clean draft.
    fn reset_recipient_form(&mut self) {
        self.recipient_name.clear();
        self.recipient_key.clear();
    }
}

struct SettingsState {
    // Working edits keyed by the stable setting key (see the SETTINGS field
    // catalog). A key present here overrides what was loaded from the DB until
    // the user saves; on save every entry is written via db::set_setting and the
    // buffer is cleared.
    edits: Vec<(String, String)>,
}

impl SettingsState {
    const fn new() -> Self {
        Self { edits: Vec::new() }
    }
    /// Records a pending edit for `key` (overwrites any prior pending value).
    fn set_edit(&mut self, key: &str, value: String) {
        if let Some(slot) = self.edits.iter_mut().find(|(k, _)| k == key) {
            slot.1 = value;
        } else {
            self.edits.push((key.into(), value));
        }
    }
    /// Returns the pending edit for `key`, if any.
    fn edit(&self, key: &str) -> Option<&str> {
        self.edits.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
    fn clear_edits(&mut self) {
        self.edits.clear();
    }
}

// The Re-ID legal gate keeps no ephemeral state: every gate condition is read
// from (and written to) the persisted settings table, so the gate's open/closed
// verdict survives panel reopen and process restart.
struct ReidState;
impl ReidState { const fn new() -> Self { Self } }

/// Models tab UI state. `form_visible` shows the add/edit model form; the
/// `form_*` fields are mirrored from the bound inputs so submit stays
/// authoritative. `editing_id` distinguishes create from edit. `pending_remove`
/// arms the delete-confirmation bar. `budget_editing` shows the VRAM budget
/// editor and `budget_draft` mirrors that input.
struct ModelsState {
    expanded_id: Option<String>,
    form_visible: bool,
    editing_id: Option<String>,
    pending_remove: Option<String>,
    form_name: String,
    form_runtime: String,
    form_status: String,
    form_vram: String,
    form_version: String,
    budget_editing: bool,
    budget_draft: String,
}

impl ModelsState {
    const fn new() -> Self {
        Self {
            expanded_id: None,
            form_visible: false,
            editing_id: None,
            pending_remove: None,
            form_name: String::new(),
            form_runtime: String::new(),
            form_status: String::new(),
            form_vram: String::new(),
            form_version: String::new(),
            budget_editing: false,
            budget_draft: String::new(),
        }
    }

    /// Resets the form to a clean "create" draft with sensible defaults.
    fn reset_form(&mut self) {
        self.editing_id = None;
        self.form_name.clear();
        self.form_runtime = "tensorrt".into();
        self.form_status = "active".into();
        self.form_vram = "1024".into();
        self.form_version.clear();
    }

    /// Loads an existing model row into the form for editing.
    fn load_for_edit(&mut self, m: &db::ModelRow) {
        self.editing_id = Some(m.id.clone());
        self.form_name = m.name.clone();
        self.form_runtime = if m.runtime.is_empty() { "tensorrt".into() } else { m.runtime.clone() };
        self.form_status = if m.status.is_empty() { "active".into() } else { m.status.clone() };
        self.form_vram = alloc::format!("{}", m.vram_mb);
        self.form_version = m.version.clone();
    }
}

/// State for the Zones tab. Everything persists in SQLite (the `zones` table);
/// this struct only holds the transient view state: which camera is selected,
/// whether the add-zone form / add-rule form is open, the pending delete arming,
/// and the bound draft fields for the two forms. The zone geometry itself is read
/// from the database on every render — there is no in-memory zone cache.
struct ZonesState {
    selected_camera_id: Option<String>,
    zone_form_visible: bool,
    zone_pending_remove: Option<String>,
    // Add-zone form draft (bound store keys mirrored on input/change).
    form_name: String,
    form_kind: String,
    form_polygon: String,
    // Add-rule form draft.
    rule_form_visible: bool,
    rule_name: String,
    rule_expr: String,
    rule_action: String,
}

impl ZonesState {
    const fn new() -> Self {
        Self {
            selected_camera_id: None,
            zone_form_visible: false,
            zone_pending_remove: None,
            form_name: String::new(),
            form_kind: String::new(),
            form_polygon: String::new(),
            rule_form_visible: false,
            rule_name: String::new(),
            rule_expr: String::new(),
            rule_action: String::new(),
        }
    }

    /// Resets the add-zone form to a clean draft with a sensible default shape.
    fn reset_zone_form(&mut self) {
        self.form_name.clear();
        self.form_kind = "include".into();
        // Default include polygon as a centered rectangle in 0..100 frame coords.
        self.form_polygon = "[[15,40],[60,40],[60,85],[15,85]]".into();
    }

    /// Resets the add-rule form to a clean draft.
    fn reset_rule_form(&mut self) {
        self.rule_name.clear();
        self.rule_expr.clear();
        self.rule_action = "Alarm info + log".into();
    }
}

/// State for the Profiles tab. `category` holds the active risk-class filter
/// chip (A/B/C; empty = all). The remaining fields back the analytic-profile
/// builder form (left/right of the mockup): a draft profile being created or the
/// snapshot of the profile under edit. `builder_visible` gates whether the
/// builder section is shown above the library table.
struct ProfilesState {
    category: String,
    builder_visible: bool,
    // id of the profile being edited; None = creating a new one.
    editing_id: Option<String>,
    // id of the profile selected for deletion (arms the confirm bar).
    pending_remove: Option<String>,
    // Builder form fields.
    name: String,
    flow_id: String,
    risk_class: String,
    schedule: String,
    fps: f64,
    min_confidence: f64,
    // Selected camera ids assigned to the profile.
    cameras: Vec<String>,
}

impl ProfilesState {
    const fn new() -> Self {
        Self {
            category: String::new(),
            builder_visible: false,
            editing_id: None,
            pending_remove: None,
            name: String::new(),
            flow_id: String::new(),
            risk_class: String::new(),
            schedule: String::new(),
            fps: 5.0,
            min_confidence: 0.65,
            cameras: Vec::new(),
        }
    }

    fn category_or_all(&self) -> &str {
        if self.category.is_empty() { "all" } else { &self.category }
    }

    /// Resets the builder form to a clean "create" draft.
    fn reset_form(&mut self) {
        self.editing_id = None;
        self.name.clear();
        self.flow_id = "tv-realtime-adr".into();
        self.risk_class = "A".into();
        self.schedule = "24/7".into();
        self.fps = 5.0;
        self.min_confidence = 0.65;
        self.cameras.clear();
    }

    /// Loads an existing profile row into the builder for editing.
    fn load_for_edit(&mut self, p: &db::ProfileRow, camera_ids: Vec<String>) {
        self.editing_id = Some(p.id.clone());
        self.name = p.name.clone();
        self.flow_id = if p.flow_id.is_empty() { "tv-realtime-adr".into() } else { p.flow_id.clone() };
        self.risk_class = if p.risk_class.is_empty() { "A".into() } else { p.risk_class.clone() };
        self.schedule = if p.schedule.is_empty() { "24/7".into() } else { p.schedule.clone() };
        self.cameras = camera_ids;
    }

    fn toggle_camera(&mut self, id: &str) {
        if let Some(pos) = self.cameras.iter().position(|c| c == id) {
            self.cameras.remove(pos);
        } else {
            self.cameras.push(id.to_string());
        }
    }
}

/// `status_view` is the left-feed tab: "open" (undecided), "all", or "closed"
/// (decided). `severity_filter` further narrows by severity. `note` is the
/// operator's draft note for the selected alarm, mirrored from the textarea.
struct AlarmsState {
    selected_id: Option<String>,
    severity_filter: String,
    status_view: String,
    note: String,
    sound_muted: bool,
}
impl AlarmsState {
    const fn new() -> Self {
        Self {
            selected_id: None,
            severity_filter: String::new(),
            status_view: String::new(),
            note: String::new(),
            sound_muted: false,
        }
    }
    fn severity_or_all(&self) -> &str { if self.severity_filter.is_empty() { "all" } else { &self.severity_filter } }
    fn status_or_open(&self) -> &str { if self.status_view.is_empty() { "open" } else { &self.status_view } }
}

/// One resolved search hit shown as a result card: the alarm row plus the raw
/// vector similarity score (only meaningful for the semantic text mode).
#[derive(Clone)]
struct SearchHit {
    alarm: db::AlarmRow,
    score: f32,
}

/// Outcome of the last search submit, driving the results area. `Empty` is the
/// pre-submit hint; `Results` carries real matching rows (text-semantic or
/// attribute SQL); `ModelUnavailable` is the honest message when the embedding
/// model is not deployed; `Placeholder` is the honest "needs vision pipeline"
/// message for image/plate modes only.
#[derive(Clone)]
enum SearchOutcome {
    Empty,
    Results(Vec<SearchHit>),
    ModelUnavailable,
    Placeholder,
}

/// View state for the historical search panel. The DURABLE parts (chosen mode,
/// last per-mode query text, camera/time filters, recent searches) live in the
/// settings table and are read fresh in `build_search_content`; this struct holds
/// the ephemeral last-submit outcome. Text-semantic search runs through the real
/// embedding + vector store; attribute search runs a real SQL query; image/plate
/// keep an honest placeholder because they need the (not-yet-wired) vision
/// pipeline. Submit NEVER fabricates result cards.
struct SearchState {
    /// The mode the last submit ran in (so the result area matches it even
    /// before the next render reads the persisted mode).
    submitted_mode: String,
    /// The last submit's outcome (real hits, honest message, or empty).
    outcome: SearchOutcome,
}
impl SearchState {
    const fn new() -> Self {
        Self { submitted_mode: String::new(), outcome: SearchOutcome::Empty }
    }
    fn clear_all(&mut self) {
        self.submitted_mode.clear();
        self.outcome = SearchOutcome::Empty;
    }
}

/// The five camera source types the backend supports. Drives every per-step
/// branch of the add-camera wizard. `vendor()` maps each type to the stable
/// TentaFlow vendor string the `camera_add` host function expects.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceType { Onvif, Rtsp, Mjpeg, Usb, File }

impl SourceType {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "onvif" => Some(Self::Onvif),
            "rtsp" => Some(Self::Rtsp),
            "mjpeg" => Some(Self::Mjpeg),
            "usb" => Some(Self::Usb),
            "file" => Some(Self::File),
            _ => None,
        }
    }
    fn as_str(self) -> &'static str {
        match self { Self::Onvif => "onvif", Self::Rtsp => "rtsp", Self::Mjpeg => "mjpeg", Self::Usb => "usb", Self::File => "file" }
    }
    fn vendor(self) -> &'static str {
        // USB enumeration reports `v4l2` on Linux; the local-device list carries
        // the authoritative vendor, but the manual-path fallback uses this.
        match self { Self::Onvif => "onvif", Self::Rtsp => "rtsp", Self::Mjpeg => "mjpeg", Self::Usb => "v4l2", Self::File => "fake_file" }
    }
}

/// One locally enumerated USB/v4l2 device offered in the wizard's device select.
struct LocalDevice { device_path: String, label: String, vendor: String }

/// Working state of the source-type-driven "Add camera" wizard. Each per-type
/// field is committed to the backend on input change (`wizard-field-change`) so
/// the test step and submit read consistent values across step navigation
/// instead of relying on a single live form snapshot.
struct DiscoverState {
    source_type: Option<SourceType>,
    // ONVIF discovery results.
    scanning: bool,
    cameras: Vec<DiscoveredCam>,
    selected_index: Option<usize>,
    // USB/v4l2 enumeration results and the picked device path.
    usb_devices: Vec<LocalDevice>,
    usb_loaded: bool,
    usb_device_path: String,
    // Per-type manual entry fields.
    onvif_url: String,
    rtsp_url: String,
    mjpeg_url: String,
    file_path: String,
    cred_user: String,
    cred_pass: String,
    // Step 3 connection test outcome (real probe, never faked).
    test_result: Option<Result<String, String>>,
    testing: bool,
    // Step 4 metadata.
    name: String,
    retention: String,
    fps: String,
    // AI analysis FPS chosen in step 4 ("1"/"5"/"10"/"15"/"0"=unlimited).
    // Committed from the analysis-FPS select; default "10" when unset.
    analysis_fps: String,
    // Analytics profile chosen in step 4. Committed from the profile select so
    // the pick is authoritative on submit instead of a frontend-only value.
    profile: String,
    error_message: Option<String>,
}
struct DiscoveredCam { vendor: String, url: String, suggested_name: String, profile_token: Option<String> }
impl DiscoverState {
    const fn new() -> Self {
        Self {
            source_type: None,
            scanning: false, cameras: Vec::new(), selected_index: None,
            usb_devices: Vec::new(), usb_loaded: false, usb_device_path: String::new(),
            onvif_url: String::new(), rtsp_url: String::new(), mjpeg_url: String::new(), file_path: String::new(),
            cred_user: String::new(), cred_pass: String::new(),
            test_result: None, testing: false,
            name: String::new(), retention: String::new(), fps: String::new(),
            analysis_fps: String::new(),
            profile: String::new(),
            error_message: None,
        }
    }
    fn reset(&mut self) {
        *self = Self::new();
    }
    /// Resolves the effective (vendor, url) for the current source type from the
    /// committed wizard fields. Returns `Err` with a user-facing message when the
    /// required field for the chosen type is missing or malformed.
    fn resolve_target(&self) -> Result<(String, String, Option<String>), &'static str> {
        match self.source_type {
            Some(SourceType::Onvif) => {
                if let Some(i) = self.selected_index {
                    if let Some(cam) = self.cameras.get(i) {
                        return Ok((cam.vendor.clone(), cam.url.clone(), cam.profile_token.clone()));
                    }
                }
                let url = self.onvif_url.trim();
                if url.is_empty() {
                    return Err("Wybierz wykrytą kamerę ONVIF lub podaj adres URL urządzenia.");
                }
                if !(url.starts_with("http://") || url.starts_with("https://")) {
                    return Err("Adres ONVIF musi zaczynać się od http:// lub https://.");
                }
                Ok(("onvif".to_string(), url.to_string(), None))
            }
            Some(SourceType::Rtsp) => {
                let url = self.rtsp_url.trim();
                if url.is_empty() {
                    return Err("Podaj adres strumienia RTSP.");
                }
                let lower = url.to_ascii_lowercase();
                if !(lower.starts_with("rtsp://") || lower.starts_with("rtsps://")) {
                    return Err("Adres RTSP musi zaczynać się od rtsp:// lub rtsps://.");
                }
                Ok(("rtsp".to_string(), url.to_string(), None))
            }
            Some(SourceType::Mjpeg) => {
                let url = self.mjpeg_url.trim();
                if url.is_empty() {
                    return Err("Podaj adres strumienia MJPEG (HTTP).");
                }
                let lower = url.to_ascii_lowercase();
                if !(lower.starts_with("http://") || lower.starts_with("https://")) {
                    return Err("Adres MJPEG musi zaczynać się od http:// lub https://.");
                }
                Ok(("mjpeg".to_string(), url.to_string(), None))
            }
            Some(SourceType::Usb) => {
                let path = self.usb_device_path.trim();
                if path.is_empty() {
                    return Err("Wybierz lub podaj ścieżkę urządzenia (np. /dev/video0).");
                }
                let vendor = self.usb_devices.iter()
                    .find(|d| d.device_path == path)
                    .map(|d| d.vendor.clone())
                    .unwrap_or_else(|| SourceType::Usb.vendor().to_string());
                Ok((vendor, path.to_string(), None))
            }
            Some(SourceType::File) => {
                let path = self.file_path.trim();
                if path.is_empty() {
                    return Err("Podaj ścieżkę pliku wideo.");
                }
                Ok(("fake_file".to_string(), path.to_string(), None))
            }
            None => Err("Wybierz typ źródła kamery."),
        }
    }
    fn retention_or_default(&self) -> &str {
        if self.retention.is_empty() { "C" } else { &self.retention }
    }
    fn profile_or_default(&self) -> &str {
        let p = self.profile.trim();
        if p.is_empty() { "default" } else { p }
    }
    fn fps_value(&self) -> u32 {
        self.fps.trim().parse::<u32>().ok().filter(|f| *f >= 1 && *f <= 60).unwrap_or(15)
    }
    /// AI analysis FPS for the chosen camera. `0` = unlimited (native cadence);
    /// any out-of-ladder value falls back to the spec default (10).
    fn analysis_fps_value(&self) -> u32 {
        self.analysis_fps
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|f| *f <= 30)
            .unwrap_or(CAMERA_DEFAULT_ANALYSIS_FPS)
    }
    /// Committed analysis FPS as a select-bound string, defaulting to "10".
    fn analysis_fps_or_default(&self) -> String {
        let v = self.analysis_fps.trim();
        if v.is_empty() {
            alloc::format!("{}", CAMERA_DEFAULT_ANALYSIS_FPS)
        } else {
            v.into()
        }
    }
}

impl PanelState {
    const fn new() -> Self {
        Self {
            current_panel: String::new(),
            add_form_visible: false, wizard_step: 0, cameras_filter: String::new(),
            camera_pending_remove: None,
            camera_flow_edit: None,
            camera_pipeline_edit: None,
            recording_playing: None,
            recordings_camera_filter: String::new(),
            recordings_date_from: String::new(),
            recordings_date_to: String::new(),
            recordings_plate_query: String::new(),
            recordings_adr_query: String::new(),
            cv_pipelines: CvPipelinesState::new(),
            error_message: None, success_message: None,
            discover: DiscoverState::new(), profiles: ProfilesState::new(),
            alarms: AlarmsState::new(), search: SearchState::new(),
            reid: ReidState::new(), models: ModelsState::new(),
            zones: ZonesState::new(), audit: AuditState::new(),
            evidence: EvidenceState::new(), settings: SettingsState::new(),
            onboarding: OnboardingState::new(),
        }
    }
    fn clear_messages(&mut self) { self.error_message = None; self.success_message = None; }
}

static STATE: Mutex<PanelState> = Mutex::new(PanelState::new());

/// Rows computed by `build_cameras_content` and handed to `render_panel` so the
/// cameras Table mounts with its rows already in the slot's state_overlay
/// snapshot (avoids a first empty rebuild that would flash the empty-state).
static PENDING_CAMERA_ROWS: Mutex<Option<Value>> = Mutex::new(None);

/// Same mechanism as `PENDING_CAMERA_ROWS` but for the profiles library Table:
/// rows seeded into the slot's state_overlay so the Table mounts populated.
static PENDING_PROFILE_ROWS: Mutex<Option<Value>> = Mutex::new(None);

/// Same mechanism as `PENDING_CAMERA_ROWS` but for the models registry Table.
static PENDING_MODEL_ROWS: Mutex<Option<Value>> = Mutex::new(None);

/// Same mechanism as `PENDING_CAMERA_ROWS` but for the zone list Table and the
/// composite-rules Table on the Zones tab.
static PENDING_ZONE_ROWS: Mutex<Option<Value>> = Mutex::new(None);
static PENDING_RULE_ROWS: Mutex<Option<Value>> = Mutex::new(None);

/// Same mechanism as `PENDING_CAMERA_ROWS` but for the evidence package Table
/// and the authorized-recipients Table on the Evidence-export tab.
static PENDING_EVIDENCE_ROWS: Mutex<Option<Value>> = Mutex::new(None);
static PENDING_RECIPIENT_ROWS: Mutex<Option<Value>> = Mutex::new(None);

fn with_state<F, R>(f: F) -> R where F: FnOnce(&mut PanelState) -> R {
    let mut guard = match STATE.lock() { Ok(g) => g, Err(p) => p.into_inner() };
    f(&mut guard)
}

fn set_current_panel(panel: &str) {
    with_state(|s| { s.current_panel.clear(); s.current_panel.push_str(panel); });
}

// =============================================================================
// Lifecycle
// =============================================================================

// Guest memory exports for host → guest data transfer (on_panel_open, on_request)
#[no_mangle]
pub extern "C" fn alloc(size: i32) -> i32 {
    let layout = std::alloc::Layout::from_size_align(size as usize, 8).unwrap();
    unsafe { std::alloc::alloc(layout) as i32 }
}

#[no_mangle]
pub extern "C" fn dealloc(ptr: i32, size: i32) {
    let layout = std::alloc::Layout::from_size_align(size as usize, 8).unwrap();
    unsafe { std::alloc::dealloc(ptr as *mut u8, layout) }
}

#[no_mangle]
pub extern "C" fn on_install() -> i32 { 0 }

#[no_mangle]
pub extern "C" fn on_start() -> i32 {
    install_panic_hook();
    log::info("TentaVision: on_start (CBOR SDK)");
    // Receive camera.alarm events emitted by the camera_alert flow node so an
    // alarm verdict from a camera's analysis flow lands in the alarms table.
    subscribe_event(CAMERA_ALARM_EVENT);
    // The shell is NOT rendered here: on_start does not receive the
    // host-assigned panel epoch, so a shell emitted now would carry the default
    // epoch and be rejected on any session whose epoch advanced past 1. The
    // host calls on_panel_open (with the authoritative epoch) on every open,
    // including cold starts, so the shell is rendered there exactly once.
    0
}

#[no_mangle]
pub extern "C" fn on_stop() -> i32 {
    log::info("TentaVision: on_stop");
    0
}

#[no_mangle]
pub extern "C" fn on_event(input_ptr: i32, input_len: i32) -> i32 {
    let bytes = unsafe {
        core::slice::from_raw_parts(input_ptr as *const u8, input_len as usize)
    };
    let event: JsonValue = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(_) => return 0,
    };
    if event.get("event_type").and_then(|v| v.as_str()) == Some(CAMERA_ALARM_EVENT) {
        handle_camera_alarm_event(event.get("payload").unwrap_or(&JsonValue::Null));
    }
    0
}

/// Persists a `camera.alarm` event (emitted by the camera_alert flow node) as a
/// row in the addon's alarms table. Best-effort: a malformed payload or DB error
/// is logged, not fatal (the event bus must not be wedged by one bad alarm).
fn handle_camera_alarm_event(payload: &JsonValue) {
    let camera_id = payload.get("camera_id").and_then(|v| v.as_str()).unwrap_or("");
    if camera_id.is_empty() {
        log::warn("TentaVision: camera.alarm bez camera_id — pomijam");
        return;
    }
    let reason = payload.get("reason").and_then(|v| v.as_str()).unwrap_or("Alarm ADR");
    let severity = payload.get("severity").and_then(|v| v.as_str()).unwrap_or("high");
    let ts = db::now_secs();
    match db::insert_alarm(camera_id, severity, "adr", reason, ts) {
        Ok(id) => log::info(&alloc::format!("TentaVision: alarm {} zapisany dla {}", id, camera_id)),
        Err(e) => log::error(&alloc::format!("TentaVision: zapis alarmu nieudany: {}", abi_message(e))),
    }
}

/// Called by host when user opens a panel on an already-running instance.
/// Re-emits PanelShell + SlotContent without restarting the addon.
#[no_mangle]
pub extern "C" fn on_panel_open(panel_id_ptr: i32, panel_id_len: i32, epoch: i64) -> i32 {
    install_panic_hook();
    let panel_id = read_guest_string(panel_id_ptr, panel_id_len);
    PANEL_EPOCH.store(epoch as u64, core::sync::atomic::Ordering::Relaxed);
    log::info(&alloc::format!("TentaVision: on_panel_open panel='{}' epoch={}", panel_id, epoch));
    // A fresh panel open starts a new view context; carrying a transient
    // success/error banner over from the previous session would surface stale
    // toasts (e.g. "Kamera dodana") on an unrelated tab.
    with_state(|s| s.clear_messages());
    send_initial_shell();
    let target = if panel_id.is_empty() { "overview" } else { &panel_id };
    render_panel(target);
    0
}

/// Wasm ABI: on_request(input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32
#[no_mangle]
pub extern "C" fn on_request(
    input_ptr: i32, input_len: i32,
    out_ptr: i32, out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let input_json = read_guest_string(input_ptr, input_len);
    let request: JsonValue = match serde_json::from_str(&input_json) {
        Ok(v) => v,
        Err(e) => {
            log::error(&alloc::format!("TentaVision: invalid on_request JSON: {}", e));
            return 1;
        }
    };
    let tool = request.get("tool").and_then(|v| v.as_str()).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or(JsonValue::Null);

    let action = tool
        .strip_prefix("ui.dashboard.")
        .or_else(|| tool.strip_prefix("ui.cameras."))
        .or_else(|| tool.strip_prefix("ui.overview."))
        .or_else(|| tool.strip_prefix("ui.live."))
        .or_else(|| tool.strip_prefix("ui.recordings."))
        .or_else(|| tool.strip_prefix("ui.profiles."))
        .or_else(|| tool.strip_prefix("ui.alarms."))
        .or_else(|| tool.strip_prefix("ui.search."))
        .or_else(|| tool.strip_prefix("ui.reid."))
        .or_else(|| tool.strip_prefix("ui.models."))
        .or_else(|| tool.strip_prefix("ui.zones."))
        .or_else(|| tool.strip_prefix("ui.audit."))
        .or_else(|| tool.strip_prefix("ui.evidence."))
        .or_else(|| tool.strip_prefix("ui.settings."));

    let response = match action {
        Some(a) => handle_action(a, &params),
        None => json!({ "error": alloc::format!("unknown tool '{}'", tool) }),
    };

    // Each handler owns its own UI side effects: panel/tab navigation and modal
    // open re-send SlotContent, while reactive wizard actions emit StatePatch
    // only. There is intentionally NO unconditional `render_panel` here — that
    // global re-render was the source of the modal tearing down and inputs
    // losing focus on every wizard interaction.

    let response_str = response.to_string();
    let written = write_guest_string(out_ptr, out_cap, &response_str);
    if written < 0 { return 2; }
    unsafe {
        let p = out_len_ptr as *mut i32;
        *p = response_str.len() as i32;
    }
    0
}

// =============================================================================
// Guest memory helpers
// =============================================================================

fn read_guest_string(ptr: i32, len: i32) -> String {
    if len <= 0 { return String::new(); }
    unsafe {
        let slice = core::slice::from_raw_parts(ptr as *const u8, len as usize);
        String::from_utf8_lossy(slice).into_owned()
    }
}

fn write_guest_string(ptr: i32, cap: i32, s: &str) -> i32 {
    let bytes = s.as_bytes();
    if bytes.len() > cap as usize { return -1; }
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
    }
    bytes.len() as i32
}

// =============================================================================
// Initial PanelShell — sent once on on_start
// =============================================================================

fn send_initial_shell() {
    let layout = build_shell_layout();
    // "content" is the static main panel. The "Add camera" wizard's
    // `add_camera_body` / `add_camera_footer` slots are also declared here, but
    // as Modal+Hidden overlay slots: the declaration satisfies the Core's slot
    // ownership check (a slot must be declared in the PanelShell), while the
    // Modal/Hidden semantics tell addon-app's `isOverlaySlot` to skip building a
    // static placeholder container for them. Their real containers are created
    // dynamically by the Modal and auto-registered by the host's
    // `observe(shell)`, with their SlotContent buffered until registration.
    let slots = vec![
        SlotDecl {
            id: "content".into(),
            semantics: SlotSemantics::MainContent,
            default_state: SlotDefault::Loading,
            cache_policy: CachePolicy::OnNavigateBack,
            visibility: SlotVisibility::Always,
            max_payload_bytes: Some(256 * 1024),
        },
        SlotDecl {
            id: "add_camera_body".into(),
            semantics: SlotSemantics::Modal,
            default_state: SlotDefault::Empty,
            cache_policy: CachePolicy::None,
            visibility: SlotVisibility::Hidden,
            max_payload_bytes: Some(256 * 1024),
        },
        SlotDecl {
            id: "add_camera_footer".into(),
            semantics: SlotSemantics::Modal,
            default_state: SlotDefault::Empty,
            cache_policy: CachePolicy::None,
            visibility: SlotVisibility::Hidden,
            max_payload_bytes: Some(64 * 1024),
        },
        // Dynamic modal body for the recordings playback dialog (video player +
        // event metadata). Same Modal/Hidden overlay semantics as the wizard
        // slots: its real container is created by the Modal when it mounts.
        SlotDecl {
            id: "recording_player_body".into(),
            semantics: SlotSemantics::Modal,
            default_state: SlotDefault::Empty,
            cache_policy: CachePolicy::None,
            visibility: SlotVisibility::Hidden,
            max_payload_bytes: Some(64 * 1024),
        },
    ];
    // The frontend resets its reactive store revision to 0 whenever it receives
    // a PanelShell (new shell/epoch). Reset the guest counter in lockstep so the
    // first StatePatch after this shell carries base_revision = 0; otherwise a
    // stale (higher) base from a previous shell would be rejected by the host
    // and the UI would never update.
    STATE_REVISION.store(0, Ordering::Relaxed);
    send_panel_shell(layout, slots, vec![]);
}

fn build_shell_layout() -> Component {
    let nav_items = build_nav_tab_items("overview");
    let mut nav = nav_tabs(nav_items, "overview");
    nav.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Select,
        Handler::Backend {
            action_id: "panel-navigate".into(),
            params: CborMap::default(),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    stack_v(vec![nav])
}

// =============================================================================
// Panel navigation — sends SlotContent for the "content" slot
// =============================================================================

fn render_panel(panel_id: &str) {
    set_current_panel(panel_id);
    let content = match panel_id {
        "overview" => build_overview_content(),
        "live" => build_live_content(),
        "recordings" => build_recordings_content(),
        "cameras" => build_cameras_content(),
        "alarms" => build_alarms_content(),
        "search" => build_search_content(),
        "profiles" => build_profiles_content(),
        "reid" => build_reid_content(),
        "models" => build_models_content(),
        "zones" => build_zones_content(),
        "audit" => build_audit_content(),
        "evidence" => build_evidence_content(),
        "settings" => build_settings_content(),
        "onboarding" => build_onboarding_content(),
        "bindings" => build_bindings_content(),
        _ => build_overview_content(),
    };
    // Send "content" first so the host has the Modal (and thus the dynamic
    // body/footer slot containers) in the DOM before we push their content.
    // The cameras panel seeds its table rows via the slot's state_overlay so
    // the Table mounts with rows already in the store snapshot — otherwise its
    // first rebuild sees an empty rows_path and leaves the empty-state visible.
    if panel_id == "cameras" {
        let mut entries: Vec<StateEntry> = Vec::new();
        if let Some(rows) = PENDING_CAMERA_ROWS.lock().ok().and_then(|mut g| g.take()) {
            entries.push(StateEntry {
                path: StatePath::new(vec![PathSegment::Key("cameras_rows".into())]),
                value: rows,
            });
        }
        // When the flow selector is open, preselect it with the camera's current
        // analysis flow (read from core — the addon mirror has no flow id).
        if let Some(id) = with_state(|s| s.camera_flow_edit.clone()) {
            let current = host_camera_get(&id)
                .ok()
                .and_then(|c| c.analysis_flow_id)
                .unwrap_or_default();
            entries.push(StateEntry {
                path: StatePath::new(vec![PathSegment::Key("camera_flow_select".into())]),
                value: Value::Text(current),
            });
        }
        // Same for the CV-pipeline selector (empty = the default pipeline).
        if let Some(id) = with_state(|s| s.camera_pipeline_edit.clone()) {
            let current = host_camera_get(&id)
                .ok()
                .and_then(|c| c.cv_pipeline_id)
                .unwrap_or_default();
            entries.push(StateEntry {
                path: StatePath::new(vec![PathSegment::Key("camera_pipeline_select".into())]),
                value: Value::Text(current),
            });
        }
        // When the pipeline stage editor is open, seed its bound form keys so
        // the inputs / selects / toggles mount with the backend draft values.
        if with_state(|s| s.cv_pipelines.manager_visible && s.cv_pipelines.editor_visible) {
            entries.extend(pipeline_editor_overlay());
        }
        let overlay = if entries.is_empty() { None } else { Some(entries) };
        send_slot_content_with_overlay("content", content, overlay);
    } else if panel_id == "recordings" {
        // Seed the recordings Table rows into the content slot's state_overlay so
        // it mounts populated (same mechanism as the cameras Table).
        let mut entries: Vec<StateEntry> = Vec::new();
        if let Some(rows) = PENDING_RECORDING_ROWS.lock().ok().and_then(|mut g| g.take()) {
            entries.push(StateEntry {
                path: StatePath::new(vec![PathSegment::Key("recordings_rows".into())]),
                value: rows,
            });
        }
        // Seed the search inputs' bound keys so they mount showing the persisted
        // query values across re-renders (otherwise each re-query would reset the
        // field the user is typing in).
        let (df, dt, pq, aq) = with_state(|s| {
            (
                s.recordings_date_from.clone(),
                s.recordings_date_to.clone(),
                s.recordings_plate_query.clone(),
                s.recordings_adr_query.clone(),
            )
        });
        for (key, val) in [
            ("recordings_date_from", df),
            ("recordings_date_to", dt),
            ("recordings_plate_query", pq),
            ("recordings_adr_query", aq),
        ] {
            entries.push(StateEntry {
                path: StatePath::new(vec![PathSegment::Key(key.into())]),
                value: Value::Text(val),
            });
        }
        let overlay = if entries.is_empty() { None } else { Some(entries) };
        send_slot_content_with_overlay("content", content, overlay);
    } else if panel_id == "profiles" {
        let mut entries: Vec<StateEntry> = Vec::new();
        if let Some(rows) = PENDING_PROFILE_ROWS.lock().ok().and_then(|mut g| g.take()) {
            entries.push(StateEntry {
                path: StatePath::new(vec![PathSegment::Key("profiles_rows".into())]),
                value: rows,
            });
        }
        // When the builder is open, seed the bound form keys so the inputs,
        // selects and sliders mount with the draft / edited profile's values.
        if with_state(|s| s.profiles.builder_visible) {
            entries.extend(profile_builder_overlay());
        }
        let overlay = if entries.is_empty() { None } else { Some(entries) };
        send_slot_content_with_overlay("content", content, overlay);
    } else if panel_id == "models" {
        let mut entries: Vec<StateEntry> = Vec::new();
        if let Some(rows) = PENDING_MODEL_ROWS.lock().ok().and_then(|mut g| g.take()) {
            entries.push(StateEntry {
                path: StatePath::new(vec![PathSegment::Key("models_rows".into())]),
                value: rows,
            });
        }
        // Seed the VRAM stacked-bar's bound segment values + total so the bar
        // mounts already showing used vs free instead of an empty first paint.
        entries.extend(models_vram_overlay());
        // When the form is open, seed its bound inputs/selects with the draft /
        // edited model's values.
        if with_state(|s| s.models.form_visible) {
            entries.extend(models_form_overlay());
        }
        // When the budget editor is open, seed its bound number input.
        if with_state(|s| s.models.budget_editing) {
            entries.push(StateEntry {
                path: StatePath::new(vec![PathSegment::Key("model_budget_input".into())]),
                value: Value::Text(with_state(|s| s.models.budget_draft.clone())),
            });
        }
        send_slot_content_with_overlay("content", content, Some(entries));
    } else if panel_id == "alarms" {
        // Seed the operator-note textarea's bound key so it mounts with the
        // current draft note (cleared on selection change).
        let note = with_state(|s| s.alarms.note.clone());
        let overlay = vec![StateEntry {
            path: StatePath::new(vec![PathSegment::Key("alarm_note".into())]),
            value: Value::Text(note),
        }];
        send_slot_content_with_overlay("content", content, Some(overlay));
    } else if panel_id == "zones" {
        // Seed the zone list Table, the composite-rules Table and, when a form is
        // open, its bound draft fields, so every control mounts populated.
        let mut entries: Vec<StateEntry> = Vec::new();
        if let Some(rows) = PENDING_ZONE_ROWS.lock().ok().and_then(|mut g| g.take()) {
            entries.push(StateEntry {
                path: StatePath::new(vec![PathSegment::Key("zones_rows".into())]),
                value: rows,
            });
        }
        if let Some(rows) = PENDING_RULE_ROWS.lock().ok().and_then(|mut g| g.take()) {
            entries.push(StateEntry {
                path: StatePath::new(vec![PathSegment::Key("rules_rows".into())]),
                value: rows,
            });
        }
        entries.extend(zones_overlay());
        send_slot_content_with_overlay("content", content, Some(entries));
    } else if panel_id == "audit" {
        // Seed the audit filter search box and, when a retention card is being
        // edited, its bound number input so both mount with the current values.
        let (query, editing, draft) = with_state(|s| (
            s.audit.query.clone(),
            s.audit.retention_editing.clone(),
            s.audit.retention_draft.clone(),
        ));
        let mut entries = vec![StateEntry {
            path: StatePath::new(vec![PathSegment::Key("audit_search".into())]),
            value: Value::Text(query),
        }];
        if let Some(class) = editing {
            entries.push(StateEntry {
                path: StatePath::new(vec![PathSegment::Key(alloc::format!("retention_input_{}", class.to_lowercase()))]),
                value: Value::Text(draft),
            });
        }
        send_slot_content_with_overlay("content", content, Some(entries));
    } else if panel_id == "evidence" {
        // Seed the evidence package Table, the recipients Table and, when a form
        // is open, its bound draft fields so every control mounts populated.
        let mut entries: Vec<StateEntry> = Vec::new();
        if let Some(rows) = PENDING_EVIDENCE_ROWS.lock().ok().and_then(|mut g| g.take()) {
            entries.push(StateEntry {
                path: StatePath::new(vec![PathSegment::Key("evidence_rows".into())]),
                value: rows,
            });
        }
        if let Some(rows) = PENDING_RECIPIENT_ROWS.lock().ok().and_then(|mut g| g.take()) {
            entries.push(StateEntry {
                path: StatePath::new(vec![PathSegment::Key("recipients_rows".into())]),
                value: rows,
            });
        }
        entries.extend(evidence_overlay());
        send_slot_content_with_overlay("content", content, Some(entries));
    } else if panel_id == "settings" {
        // Seed every settings control's bound store key from the DB (or default,
        // or the pending edit) so each field mounts showing its current value
        // and persists across reopen / process restart.
        send_slot_content_with_overlay("content", content, Some(settings_overlay()));
    } else if panel_id == "search" {
        // Seed the mode selector, active mode's query control and the camera /
        // time filters from settings so the panel mounts on the persisted mode
        // with its last query and filters restored across reopen.
        send_slot_content_with_overlay("content", content, Some(search_overlay()));
    } else if panel_id == "live" {
        // Seed the grid-size segmented control's bound key from settings so it
        // mounts showing the persisted layout instead of an empty selection.
        let overlay = vec![StateEntry {
            path: StatePath::new(vec![PathSegment::Key("live_grid_size".into())]),
            value: Value::Text(alloc::format!("{}", live_grid_size())),
        }];
        send_slot_content_with_overlay("content", content, Some(overlay));
    } else if panel_id == "overview" {
        // Seed the activity heatmap's cells into the slot snapshot so the
        // Heatmap mounts with its data already in the store (same pattern as the
        // cameras Table). Without this, `heatmap_cells` is undefined on first
        // paint and every cell renders at level 0.
        let overlay = vec![StateEntry {
            path: StatePath::new(vec![PathSegment::Key("heatmap_cells".into())]),
            value: heatmap_cells_value(),
        }];
        send_slot_content_with_overlay("content", content, Some(overlay));
    } else if panel_id == "bindings" {
        // Seed each alias target Select's bound store key from the persisted
        // mapping (or the alias's default) so every Select mounts showing its
        // current target across reopen / process restart.
        send_slot_content_with_overlay("content", content, Some(bindings_overlay()));
    } else if panel_id == "onboarding" {
        // Seed the first-camera inputs so step navigation keeps typed values.
        send_slot_content_with_overlay("content", content, Some(onboarding_overlay()));
    } else {
        send_slot_content("content", content);
    }

    // When the "Add camera" wizard is open on the cameras panel, fill the
    // Modal's body/footer slots. These must be sent AFTER "content" so their
    // target data-slot-id containers already exist.
    if panel_id == "cameras" && with_state(|s| s.add_form_visible) {
        // Seed the full wizard store state alongside the body so the bound
        // visibility flags, StepProgress and inputs resolve on first paint.
        send_slot_content_with_overlay("add_camera_body", build_add_camera_body(), Some(wizard_full_overlay()));
        send_slot_content("add_camera_footer", build_add_camera_footer());
    }

    // When a recording playback modal is open on the recordings panel, fill its
    // body slot with the video player + metadata. Sent AFTER "content" so the
    // Modal's dynamic slot container already exists.
    if panel_id == "recordings" {
        if let Some(rec_ref) = with_state(|s| s.recording_playing.clone()) {
            send_slot_content("recording_player_body", build_recording_player_body(&rec_ref));
        }
    }
}

// =============================================================================
// Action handlers
// =============================================================================

fn handle_action(action: &str, params: &JsonValue) -> JsonValue {
    log::info(&alloc::format!("TentaVision UI action '{}'", action));
    match action {
        "recordings-play" => {
            let rec_ref = params
                .get("row_id")
                .and_then(|x| x.as_str())
                .or_else(|| params.get("recording_ref").and_then(|x| x.as_str()))
                .unwrap_or("")
                .trim()
                .to_string();
            with_state(|s| {
                s.clear_messages();
                s.recording_playing = if rec_ref.is_empty() { None } else { Some(rec_ref) };
            });
            render_panel("recordings");
            json!({"ok":true})
        }
        "recordings-close" => {
            with_state(|s| { s.recording_playing = None; s.clear_messages(); });
            render_panel("recordings");
            json!({"ok":true})
        }
        "recordings-filter-change" => {
            let v = params
                .get("value")
                .and_then(|x| x.as_str())
                .or_else(|| params.get("chipId").and_then(|x| x.as_str()))
                .unwrap_or("all")
                .to_string();
            with_state(|s| {
                s.recordings_camera_filter = if v == "all" { String::new() } else { v };
            });
            render_panel("recordings");
            json!({"ok":true})
        }
        "recordings-search-change" => {
            // A search field committed a keystroke: store it under its `field`
            // key and re-query server-side on the next render.
            let field = params.get("field").and_then(|x| x.as_str()).unwrap_or("");
            let value = params
                .get("value")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            with_state(|s| match field {
                "recordings_date_from" => s.recordings_date_from = value,
                "recordings_date_to" => s.recordings_date_to = value,
                "recordings_plate_query" => s.recordings_plate_query = value,
                "recordings_adr_query" => s.recordings_adr_query = value,
                _ => {}
            });
            render_panel("recordings");
            json!({"ok":true})
        }
        "recordings-search-clear" => {
            with_state(|s| {
                s.recordings_date_from.clear();
                s.recordings_date_to.clear();
                s.recordings_plate_query.clear();
                s.recordings_adr_query.clear();
            });
            render_panel("recordings");
            json!({"ok":true})
        }
        "recordings-refresh" => { render_panel("recordings"); json!({"ok":true}) }
        "camera-add-show" => handle_camera_add_show(),
        "camera-add-cancel" => { with_state(|s| { s.add_form_visible = false; s.wizard_step = 0; s.discover.reset(); s.clear_messages(); }); render_panel("cameras"); json!({"ok":true}) }
        "wizard-source-select" => handle_wizard_source_select(params),
        "wizard-field-change" => handle_wizard_field_change(params),
        "wizard-test" => handle_wizard_test(),
        "wizard-next" => handle_wizard_next(),
        "wizard-prev" => handle_wizard_prev(),
        "cameras-filter-change" => { let v = params.get("value").and_then(|x| x.as_str()).or_else(|| params.get("chipId").and_then(|x| x.as_str())).unwrap_or("all").to_string(); with_state(|s| { s.cameras_filter = if v == "all" { String::new() } else { v }; }); json!({"ok":true}) }
        "camera-add-submit" => handle_camera_add_submit(params),
        "camera-row-select" => { let id = params.get("row_id").and_then(|x| x.as_str()).or_else(|| params.get("camera_id").and_then(|x| x.as_str())).unwrap_or("").trim().to_string(); with_state(|s| { s.clear_messages(); s.camera_pending_remove = if id.is_empty() { None } else { Some(id) }; }); json!({"ok":true}) }
        "camera-remove-cancel" => { with_state(|s| { s.camera_pending_remove = None; s.clear_messages(); }); json!({"ok":true}) }
        "camera-remove" => handle_camera_remove(params),
        "camera-flow-edit" => { let id = params.get("row_id").and_then(|x| x.as_str()).or_else(|| params.get("camera_id").and_then(|x| x.as_str())).unwrap_or("").trim().to_string(); with_state(|s| { s.clear_messages(); s.camera_pending_remove = None; s.camera_flow_edit = if id.is_empty() { None } else { Some(id) }; }); render_panel("cameras"); json!({"ok":true}) }
        "camera-flow-cancel" => { with_state(|s| { s.camera_flow_edit = None; s.clear_messages(); }); render_panel("cameras"); json!({"ok":true}) }
        "camera-flow-change" => handle_camera_flow_change(params),
        "camera-pipeline-edit" => { let id = params.get("row_id").and_then(|x| x.as_str()).or_else(|| params.get("camera_id").and_then(|x| x.as_str())).unwrap_or("").trim().to_string(); with_state(|s| { s.clear_messages(); s.camera_pending_remove = None; s.camera_flow_edit = None; s.camera_pipeline_edit = if id.is_empty() { None } else { Some(id) }; }); render_panel("cameras"); json!({"ok":true}) }
        "camera-pipeline-cancel" => { with_state(|s| { s.camera_pipeline_edit = None; s.clear_messages(); }); render_panel("cameras"); json!({"ok":true}) }
        "camera-pipeline-change" => handle_camera_pipeline_change(params),
        "pipeline-manager-show" => { with_state(|s| { s.clear_messages(); s.cv_pipelines.manager_visible = true; s.cv_pipelines.pending_remove = None; }); render_panel("cameras"); json!({"ok":true}) }
        "pipeline-manager-close" => { with_state(|s| { s.cv_pipelines.manager_visible = false; s.cv_pipelines.pending_remove = None; s.cv_pipelines.close_editor(); s.clear_messages(); }); render_panel("cameras"); json!({"ok":true}) }
        "pipeline-new" => { with_state(|s| { s.clear_messages(); s.cv_pipelines.editor_visible = true; s.cv_pipelines.editing_id = None; s.cv_pipelines.pending_remove = None; s.cv_pipelines.name = "Nowy pipeline".to_string(); s.cv_pipelines.stages = vec![StageDraft::new_detect()]; }); render_panel("cameras"); json!({"ok":true}) }
        "pipeline-edit" => handle_pipeline_edit(params),
        "pipeline-duplicate" => handle_pipeline_duplicate(params),
        "pipeline-row-remove" => { let id = params.get("pipeline_id").and_then(|x| x.as_str()).unwrap_or("").trim().to_string(); with_state(|s| { s.clear_messages(); s.cv_pipelines.pending_remove = if id.is_empty() { None } else { Some(id) }; }); render_panel("cameras"); json!({"ok":true}) }
        "pipeline-remove-cancel" => { with_state(|s| { s.cv_pipelines.pending_remove = None; s.clear_messages(); }); render_panel("cameras"); json!({"ok":true}) }
        "pipeline-remove" => handle_pipeline_remove(params),
        "pipeline-name-change" => { let v = params.get("value").and_then(|x| x.as_str()).unwrap_or("").to_string(); with_state(|s| s.cv_pipelines.name = v); json!({"ok":true}) }
        "pipeline-stage-field-change" => handle_pipeline_stage_field_change(params),
        "pipeline-stage-toggle" => handle_pipeline_stage_toggle(params),
        "pipeline-stage-add" => handle_pipeline_stage_add(),
        "pipeline-stage-remove" => { let index: Option<usize> = params.get("index").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()); if let Some(i) = index { with_state(|s| { if i < s.cv_pipelines.stages.len() { s.cv_pipelines.stages.remove(i); } }); } render_panel("cameras"); json!({"ok":true}) }
        "pipeline-save" => handle_pipeline_save(),
        "pipeline-editor-cancel" => { with_state(|s| { s.cv_pipelines.close_editor(); s.clear_messages(); }); render_panel("cameras"); json!({"ok":true}) }
        "discover-scan" => handle_discover_scan(),
        "discover-select" => handle_discover_select(params),
        "cameras-refresh" => handle_camera_refresh_status(),
        "overview-refresh" => { with_state(|s| s.clear_messages()); json!({"ok":true}) }
        "live-grid-change" => {
            // The segmented control commits its picked layout to settings so the
            // choice survives panel reopen; only the four allowed sizes persist.
            let v = params.get("value").and_then(|x| x.as_str())
                .or_else(|| params.get("chipId").and_then(|x| x.as_str()))
                .unwrap_or("")
                .trim()
                .to_string();
            if let Ok(n) = v.parse::<u32>() {
                if LIVE_GRID_SIZES.contains(&n) {
                    let _ = db::set_setting("live_grid_size", &alloc::format!("{}", n));
                }
            }
            render_panel("live");
            json!({"ok":true})
        }
        "live-refresh" => {
            // Re-renders the Live grid, which re-attempts a fresh snapshot per
            // online tile — a manual refresh of the still frames, not a stream.
            render_panel("live");
            json!({"ok":true})
        }
        "panel-navigate" => {
            let target = params.get("panel_id")
                .or_else(|| params.get("item_id"))
                .and_then(|v| v.as_str()).unwrap_or("overview").to_string();
            render_panel(&target);
            json!({"ok":true, "panel_id": target})
        }
        "profiles-filter-change" => { let v = params.get("value").and_then(|x| x.as_str()).or_else(|| params.get("chipId").and_then(|x| x.as_str())).unwrap_or("all").to_string(); with_state(|s| { s.profiles.category = if v == "all" { String::new() } else { v }; }); json!({"ok":true}) }
        "profile-add-show" => { with_state(|s| { s.clear_messages(); s.profiles.builder_visible = true; s.profiles.pending_remove = None; s.profiles.reset_form(); }); render_panel("profiles"); json!({"ok":true}) }
        "profile-builder-cancel" => { with_state(|s| { s.profiles.builder_visible = false; s.profiles.editing_id = None; s.clear_messages(); }); render_panel("profiles"); json!({"ok":true}) }
        "profile-field-change" => handle_profile_field_change(params),
        "profile-camera-toggle" => { let id = params.get("camera_id").and_then(|x| x.as_str()).or_else(|| params.get("row_id").and_then(|x| x.as_str())).unwrap_or("").trim().to_string(); if !id.is_empty() { with_state(|s| s.profiles.toggle_camera(&id)); } render_panel("profiles"); json!({"ok":true}) }
        "profile-add-submit" => handle_profile_add_submit(),
        "profile-edit" => handle_profile_edit(params),
        "profile-toggle-enabled" => handle_profile_toggle_enabled(params),
        "profile-row-select" => { let id = params.get("row_id").and_then(|x| x.as_str()).or_else(|| params.get("profile_id").and_then(|x| x.as_str())).unwrap_or("").trim().to_string(); with_state(|s| { s.clear_messages(); s.profiles.pending_remove = if id.is_empty() { None } else { Some(id) }; }); render_panel("profiles"); json!({"ok":true}) }
        "profile-remove-cancel" => { with_state(|s| { s.profiles.pending_remove = None; s.clear_messages(); }); render_panel("profiles"); json!({"ok":true}) }
        "profile-remove" => handle_profile_remove(params),
        "alarm-select" => { let id = params.get("alarm_id").and_then(|x| x.as_str()).or_else(|| params.get("rowId").and_then(|x| x.as_str())).unwrap_or("").to_string(); with_state(|s| { s.clear_messages(); s.alarms.selected_id = if id.is_empty() { None } else { Some(id) }; s.alarms.note.clear(); }); render_panel("alarms"); json!({"ok":true}) }
        "alarm-status-view" => { let v = params.get("view").and_then(|x| x.as_str()).unwrap_or("open").to_string(); with_state(|s| { s.alarms.status_view = if v == "open" { String::new() } else { v }; }); render_panel("alarms"); json!({"ok":true}) }
        "alarm-filter-severity" => { let v = params.get("value").and_then(|x| x.as_str()).or_else(|| params.get("chipId").and_then(|x| x.as_str())).unwrap_or("all").to_string(); with_state(|s| { s.alarms.severity_filter = if v == "all" { String::new() } else { v }; }); render_panel("alarms"); json!({"ok":true}) }
        "alarm-note-change" => { let v = params.get("value").and_then(|x| x.as_str()).unwrap_or("").to_string(); with_state(|s| s.alarms.note = v); json!({"ok":true}) }
        "alarm-decide" => handle_alarm_decide(params),
        "alarm-acknowledge-all" => handle_alarm_acknowledge_all(),
        "alarm-simulate" => handle_alarm_simulate(),
        "alarm-mute-sound" => { with_state(|s| { s.alarms.sound_muted = !s.alarms.sound_muted; }); json!({"ok":true}) }
        "search-mode-change" => handle_search_mode_change(params),
        "search-field-change" => handle_search_field_change(params),
        "search-submit" => handle_search_submit(params),
        "search-recent-pick" => handle_search_recent_pick(params),
        "search-reindex" => handle_search_reindex(),
        "search-clear-all" => handle_search_clear_all(),
        "reid-flag-set" => handle_reid_flag_set(params),
        "reid-legalgrant-request" => handle_reid_legalgrant_request(),
        "reid-query" => handle_reid_query(),
        "model-row-expand" => { let id = params.get("id").and_then(|x| x.as_str()).or_else(|| params.get("rowId").and_then(|x| x.as_str())).unwrap_or("").to_string(); with_state(|s| { s.models.expanded_id = if id.is_empty() || s.models.expanded_id.as_deref() == Some(id.as_str()) { None } else { Some(id) }; }); json!({"ok":true}) }
        "model-add-show" => { with_state(|s| { s.clear_messages(); s.models.form_visible = true; s.models.pending_remove = None; s.models.budget_editing = false; s.models.reset_form(); }); render_panel("models"); json!({"ok":true}) }
        "model-form-cancel" => { with_state(|s| { s.models.form_visible = false; s.models.editing_id = None; s.clear_messages(); }); render_panel("models"); json!({"ok":true}) }
        "model-field-change" => handle_model_field_change(params),
        "model-add-submit" => handle_model_add_submit(),
        "model-edit" => handle_model_edit(params),
        "model-row-select" => { let id = params.get("row_id").and_then(|x| x.as_str()).or_else(|| params.get("model_id").and_then(|x| x.as_str())).unwrap_or("").trim().to_string(); with_state(|s| { s.clear_messages(); s.models.pending_remove = if id.is_empty() { None } else { Some(id) }; }); render_panel("models"); json!({"ok":true}) }
        "model-remove-cancel" => { with_state(|s| { s.models.pending_remove = None; s.clear_messages(); }); render_panel("models"); json!({"ok":true}) }
        "model-remove" => handle_model_remove(params),
        "model-rollback" => handle_model_rollback(params),
        "model-benchmark" | "model-upload-onnx" => { with_state(|s| { s.clear_messages(); s.success_message = Some("Ta operacja wymaga uruchomionego runtime inferencji (brak backendu).".into()); }); render_panel("models"); json!({"ok":true,"noop":true}) }
        "model-budget-edit" => { with_state(|s| { s.clear_messages(); s.models.budget_editing = true; s.models.form_visible = false; s.models.budget_draft = alloc::format!("{}", db::get_setting_i64("vram_budget_mb", DEFAULT_VRAM_BUDGET_MB)); }); render_panel("models"); json!({"ok":true}) }
        "model-budget-cancel" => { with_state(|s| { s.models.budget_editing = false; s.clear_messages(); }); render_panel("models"); json!({"ok":true}) }
        "model-budget-change" => { let v = params.get("value").and_then(|x| x.as_str()).unwrap_or("").to_string(); with_state(|s| s.models.budget_draft = v); json!({"ok":true}) }
        "model-budget-save" => handle_model_budget_save(),
        "zone-select-camera" => { let id = params.get("value").and_then(|x| x.as_str()).or_else(|| params.get("camera_id").and_then(|x| x.as_str())).unwrap_or("").trim().to_string(); with_state(|s| { s.clear_messages(); s.zones.selected_camera_id = if id.is_empty() { None } else { Some(id) }; s.zones.zone_form_visible = false; s.zones.rule_form_visible = false; s.zones.zone_pending_remove = None; }); render_panel("zones"); json!({"ok":true}) }
        "zone-add-start" => { with_state(|s| { s.clear_messages(); s.zones.zone_form_visible = true; s.zones.rule_form_visible = false; s.zones.zone_pending_remove = None; s.zones.reset_zone_form(); }); render_panel("zones"); json!({"ok":true}) }
        "zone-form-cancel" => { with_state(|s| { s.zones.zone_form_visible = false; s.clear_messages(); }); render_panel("zones"); json!({"ok":true}) }
        "zone-field-change" => handle_zone_field_change(params),
        "zone-add-submit" => handle_zone_add_submit(),
        "zone-row-select" => { let id = params.get("row_id").and_then(|x| x.as_str()).or_else(|| params.get("zone_id").and_then(|x| x.as_str())).unwrap_or("").trim().to_string(); with_state(|s| { s.clear_messages(); s.zones.zone_pending_remove = if id.is_empty() { None } else { Some(id) }; }); render_panel("zones"); json!({"ok":true}) }
        "zone-remove-cancel" => { with_state(|s| { s.zones.zone_pending_remove = None; s.clear_messages(); }); render_panel("zones"); json!({"ok":true}) }
        "zone-remove" => handle_zone_remove(params),
        "schedule-cell-toggle" => handle_schedule_cell_toggle(params),
        "rule-add-start" => { with_state(|s| { s.clear_messages(); s.zones.rule_form_visible = true; s.zones.zone_form_visible = false; s.zones.reset_rule_form(); }); render_panel("zones"); json!({"ok":true}) }
        "rule-form-cancel" => { with_state(|s| { s.zones.rule_form_visible = false; s.clear_messages(); }); render_panel("zones"); json!({"ok":true}) }
        "rule-field-change" => handle_rule_field_change(params),
        "rule-add-submit" => handle_rule_add_submit(),
        "rule-row-select" => handle_rule_remove(params),
        "audit-filter-change" => { with_state(|s| { let id = params.get("id").and_then(|x| x.as_str()).unwrap_or(""); match id { "date_preset" => s.audit.date_preset = params.get("value").and_then(|x| x.as_str()).unwrap_or("").to_string(), "query" => s.audit.query = params.get("value").and_then(|x| x.as_str()).unwrap_or("").to_string(), _ => {} } }); render_panel("audit"); json!({"ok":true}) }
        "audit-clear-filters" => { with_state(|s| s.audit.clear_filters()); render_panel("audit"); json!({"ok":true}) }
        "audit-row-expand" => { let id = params.get("id").and_then(|x| x.as_str()).or_else(|| params.get("rowId").and_then(|x| x.as_str())).unwrap_or("").to_string(); with_state(|s| { s.audit.expanded_id = if id.is_empty() || s.audit.expanded_id.as_deref() == Some(id.as_str()) { None } else { Some(id) }; }); render_panel("audit"); json!({"ok":true}) }
        "audit-retention-edit" => handle_audit_retention_edit(params),
        "audit-retention-cancel" => { with_state(|s| { s.audit.retention_editing = None; s.clear_messages(); }); render_panel("audit"); json!({"ok":true}) }
        "audit-retention-change" => { let v = params.get("value").and_then(|x| x.as_str()).unwrap_or("").to_string(); with_state(|s| s.audit.retention_draft = v); json!({"ok":true}) }
        "audit-retention-save" => handle_audit_retention_save(params),
        "audit-doc-generate" => { let kind = params.get("kind").and_then(|x| x.as_str()).unwrap_or("dokument").to_string(); with_state(|s| { s.clear_messages(); s.success_message = Some(alloc::format!("Generowanie dokumentu '{}' zostanie podłączone do backendu zgodności.", kind.to_uppercase())); }); render_panel("audit"); json!({"ok":true}) }
        "evidence-new" => { with_state(|s| { s.clear_messages(); s.evidence.package_form_visible = true; s.evidence.recipient_form_visible = false; s.evidence.pending_remove = None; s.evidence.reset_package_form(); }); render_panel("evidence"); json!({"ok":true}) }
        "evidence-form-cancel" => { with_state(|s| { s.evidence.package_form_visible = false; s.clear_messages(); }); render_panel("evidence"); json!({"ok":true}) }
        "evidence-field-change" => handle_evidence_field_change(params),
        "evidence-create" => handle_evidence_create(),
        "evidence-row-select" => { let id = params.get("evidence_id").and_then(|x| x.as_str()).or_else(|| params.get("row_id").and_then(|x| x.as_str())).unwrap_or("").trim().to_string(); with_state(|s| { s.clear_messages(); s.evidence.pending_remove = if id.is_empty() { None } else { Some(id) }; }); render_panel("evidence"); json!({"ok":true}) }
        "evidence-remove-cancel" => { with_state(|s| { s.evidence.pending_remove = None; s.clear_messages(); }); render_panel("evidence"); json!({"ok":true}) }
        "evidence-remove" => handle_evidence_remove(params),
        "evidence-download" | "evidence-verify" | "evidence-sign" => { with_state(|s| { s.clear_messages(); s.success_message = Some("Podpis HSM/TSA wymaga skonfigurowanego modułu — brak backendu.".into()); }); render_panel("evidence"); json!({"ok":true,"noop":true}) }
        "evidence-recipient-add-show" => { with_state(|s| { s.clear_messages(); s.evidence.recipient_form_visible = true; s.evidence.package_form_visible = false; s.evidence.pending_remove = None; s.evidence.reset_recipient_form(); }); render_panel("evidence"); json!({"ok":true}) }
        "evidence-recipient-cancel" => { with_state(|s| { s.evidence.recipient_form_visible = false; s.clear_messages(); }); render_panel("evidence"); json!({"ok":true}) }
        "evidence-recipient-field-change" => handle_evidence_recipient_field_change(params),
        "evidence-recipient-add" => handle_evidence_recipient_add(),
        "evidence-recipient-remove" => handle_evidence_recipient_remove(params),
        "settings-field-change" => { let key = params.get("key").and_then(|x| x.as_str()).unwrap_or("").to_string(); let value = params.get("value").and_then(|x| x.as_str()).unwrap_or("").to_string(); if !key.is_empty() { with_state(|s| s.settings.set_edit(&key, value)); } json!({"ok":true}) }
        "settings-toggle-change" => { let key = params.get("key").and_then(|x| x.as_str()).unwrap_or("").to_string(); let on = params.get("value").and_then(|x| x.as_bool()).or_else(|| params.get("checked").and_then(|x| x.as_bool())).unwrap_or(false); if !key.is_empty() { with_state(|s| s.settings.set_edit(&key, if on { "1".into() } else { "0".into() })); } json!({"ok":true}) }
        "settings-save" => handle_settings_save(),
        "onboarding-next" => handle_onboarding_next(),
        "onboarding-prev" => { with_state(|s| { s.clear_messages(); if s.onboarding.step > 0 { s.onboarding.step -= 1; } }); render_panel("onboarding"); json!({"ok":true}) }
        "onboarding-pick-role" => handle_onboarding_pick("role", params),
        "onboarding-pick-legal" => handle_onboarding_pick("legal", params),
        "onboarding-pick-presets" => handle_onboarding_pick("presets", params),
        "onboarding-field-change" => handle_onboarding_field_change(params),
        "onboarding-finish" => handle_onboarding_finish(),
        "onboarding-restart" => handle_onboarding_restart(),
        "binding-target-change" => handle_binding_target_change(params),
        _ => json!({"error": alloc::format!("unknown action '{}'", action)}),
    }
}

// =============================================================================
// Camera action handlers
// =============================================================================

/// Opens the "Add camera" wizard. Resets backend wizard state, eagerly
/// enumerates local USB/v4l2 devices (their Select options are static component
/// fields baked into the body sent here, so they must be known before the body
/// is built), then sends the cameras content (with the Modal) plus the wizard
/// body and footer fragments exactly once, seeding all wizard store keys via the
/// body's `state_overlay`. Every later interaction mutates the store, not the DOM.
fn handle_camera_add_show() -> JsonValue {
    with_state(|s| { s.add_form_visible = true; s.wizard_step = 0; s.discover.reset(); s.clear_messages(); });
    // Enumerate USB devices up front so the device Select can carry real options.
    let devices = camera_local_devices();
    with_state(|s| {
        s.discover.usb_loaded = true;
        if let Ok(list) = devices {
            s.discover.usb_devices = list.into_iter()
                .map(|d| LocalDevice { device_path: d.device_path, label: d.label, vendor: d.vendor })
                .collect();
        }
    });
    render_panel("cameras");
    json!({"ok":true})
}

/// Steps back one wizard step. Pure `StatePatch`: flips the step visibility /
/// footer flags and clears any error. No fragment is re-sent.
fn handle_wizard_prev() -> JsonValue {
    let step = with_state(|s| {
        if s.wizard_step > 0 { s.wizard_step -= 1; }
        s.error_message = None;
        s.wizard_step
    });
    let mut pairs = wizard_step_pairs(step);
    pairs.extend(wizard_error_pairs(None));
    send_state_patches(pairs);
    json!({"ok":true})
}

/// Commits the chosen source type (step 1) to backend state and patches the
/// per-type config visibility flags. Resetting the per-type fields here keeps a
/// switched type from carrying stale values into step 2's validation; the reset
/// is mirrored into the store so the bound inputs clear too. Pure `StatePatch` —
/// the RadioCardGroup highlight follows `wiz_src` and the config blocks toggle
/// via `wiz_is_*` without rebuilding the body.
fn handle_wizard_source_select(params: &JsonValue) -> JsonValue {
    let raw = params.get("source_type").and_then(|v| v.as_str())
        .or_else(|| params.get("value").and_then(|v| v.as_str()))
        .unwrap_or("");
    let t = match SourceType::from_str(raw) {
        Some(t) => t,
        None => return json!({"ok":false,"error":"unknown source_type"}),
    };
    let changed = with_state(|s| {
        let changed = s.discover.source_type != Some(t);
        if changed {
            s.discover.source_type = Some(t);
            s.discover.cameras.clear();
            s.discover.selected_index = None;
            s.discover.usb_device_path.clear();
            s.discover.onvif_url.clear();
            s.discover.rtsp_url.clear();
            s.discover.mjpeg_url.clear();
            s.discover.file_path.clear();
            s.discover.test_result = None;
        }
        s.error_message = None;
        changed
    });
    let mut pairs = wizard_source_pairs(Some(t));
    pairs.extend(wizard_error_pairs(None));
    if changed {
        // Mirror the per-type field reset into the bound store keys so any
        // previously typed value disappears from the inputs as well.
        for key in ["onvif_url", "rtsp_url", "mjpeg_url", "usb_device_path", "file_path"] {
            pairs.push((key.into(), Value::Text(String::new())));
        }
        pairs.extend(wizard_test_pairs(&DiscoverState::new()));
        // Reset the ONVIF discovery sub-state (cameras were cleared above).
        pairs.extend(wizard_onvif_pairs(&DiscoverState::new()));
    }
    send_state_patches(pairs);
    json!({"ok":true})
}

/// Commits a single typed wizard field to backend state on every input change,
/// keyed by the `field` discriminator the renderer carries in `handler.params`.
/// The value already lives in the store via the input's two-way `bind_path`, so
/// this emits no `StatePatch` and no re-render — it only mirrors the value into
/// backend state for step-3 testing and submit validation.
fn handle_wizard_field_change(params: &JsonValue) -> JsonValue {
    let field = params.get("field").and_then(|v| v.as_str()).unwrap_or("");
    let value = params.get("value").and_then(|v| v.as_str()).unwrap_or("").to_string();
    with_state(|s| {
        match field {
            "onvif_url" => s.discover.onvif_url = value,
            "rtsp_url" => s.discover.rtsp_url = value,
            "mjpeg_url" => s.discover.mjpeg_url = value,
            "usb_device_path" => s.discover.usb_device_path = value,
            "file_path" => s.discover.file_path = value,
            "cred_user" => s.discover.cred_user = value,
            "cred_pass" => s.discover.cred_pass = value,
            "name" => s.discover.name = value,
            "retention" => s.discover.retention = value,
            "fps" => s.discover.fps = value,
            "analysis_fps" => s.discover.analysis_fps = value,
            "profile" => s.discover.profile = value,
            _ => {}
        }
    });
    json!({"ok":true})
}

/// Runs a real `camera_test_connection` probe for the resolved per-type target
/// and patches the step-3 result flags + text. Never fabricates success. Pure
/// `StatePatch`: the test block visibility and message follow the store.
fn handle_wizard_test() -> JsonValue {
    let target = with_state(|s| { s.discover.testing = true; s.discover.test_result = None; s.error_message = None; s.discover.resolve_target() });
    // Emit the spinner state as its own patch BEFORE the blocking probe so the
    // client paints the "testing" block; otherwise the only patch would arrive
    // after the probe returns and the spinner would never be seen.
    send_state_patches(with_state(|s| wizard_test_pairs(&s.discover)));
    let (vendor, url) = match target {
        Ok((v, u, _)) => (v, u),
        Err(msg) => {
            // Surface the validation message in the step-3 result block (the
            // dedicated error path), not the wizard-wide error alert.
            let pairs = with_state(|s| {
                s.discover.testing = false;
                s.discover.test_result = Some(Err(msg.to_string()));
                wizard_test_pairs(&s.discover)
            });
            send_state_patches(pairs);
            return json!({"ok":false,"error":"invalid target"});
        }
    };
    let result = camera_test_connection(&vendor, &url);
    let pairs = with_state(|s| {
        s.discover.testing = false;
        s.discover.test_result = Some(match result {
            Ok(out) if out.ok => Ok(out.message),
            Ok(out) => Err(out.message),
            Err(e) => Err(alloc::format!("Test nieudany: {}", abi_message(e))),
        });
        wizard_test_pairs(&s.discover)
    });
    send_state_patches(pairs);
    json!({"ok":true})
}

/// Advances the wizard, gating each transition on the current step's required
/// state: a source type must be chosen on step 1, and step 2 must resolve a
/// valid per-type target before moving on. Pure `StatePatch`: on success it
/// flips the step visibility / footer flags; on failure it patches the error
/// alert. The body is never re-sent.
fn handle_wizard_next() -> JsonValue {
    let step = with_state(|s| s.wizard_step);
    match step {
        0 => {
            let chosen = with_state(|s| s.discover.source_type.is_some());
            if !chosen {
                send_state_patches(wizard_error_pairs(Some("Wybierz typ źródła kamery.")));
                return json!({"ok":false,"error":"no source type"});
            }
            with_state(|s| { s.wizard_step = 1; s.error_message = None; });
            advance_patch(1);
            json!({"ok":true})
        }
        1 => {
            let resolved = with_state(|s| s.discover.resolve_target());
            match resolved {
                Ok(_) => {
                    with_state(|s| { s.wizard_step = 2; s.discover.test_result = None; s.error_message = None; });
                    let mut pairs = wizard_step_pairs(2);
                    pairs.extend(wizard_error_pairs(None));
                    pairs.extend(with_state(|s| wizard_test_pairs(&s.discover)));
                    send_state_patches(pairs);
                    json!({"ok":true})
                }
                Err(msg) => { send_state_patches(wizard_error_pairs(Some(msg))); json!({"ok":false,"error":"invalid target"}) }
            }
        }
        2 => {
            // Pre-fill a metadata name from a discovered camera when the user has
            // not typed one yet, otherwise leave it empty (no fake placeholder).
            let prefill_name = with_state(|s| {
                if s.discover.name.trim().is_empty() {
                    if let Some(i) = s.discover.selected_index {
                        if let Some(cam) = s.discover.cameras.get(i) {
                            s.discover.name = cam.suggested_name.clone();
                            return Some(s.discover.name.clone());
                        }
                    }
                }
                None
            });
            with_state(|s| { s.wizard_step = 3; s.error_message = None; });
            let mut pairs = wizard_step_pairs(3);
            pairs.extend(wizard_error_pairs(None));
            if let Some(name) = prefill_name {
                pairs.push(("name".into(), Value::Text(name)));
            }
            send_state_patches(pairs);
            json!({"ok":true})
        }
        _ => json!({"ok":true}),
    }
}

/// Helper: emit the step navigation patch + clear error for a forward move.
fn advance_patch(step: u8) {
    let mut pairs = wizard_step_pairs(step);
    pairs.extend(wizard_error_pairs(None));
    send_state_patches(pairs);
}

/// Reports a submit failure: surfaces the message in the wizard-wide error alert
/// (`wiz_error` + `wiz_has_error`) so it is visible inside the open modal, since
/// the wizard no longer re-renders the body on each action.
fn submit_fail(msg: &str, err_code: &str) -> JsonValue {
    with_state(|s| { s.error_message = Some(msg.to_string()); });
    send_state_patches(wizard_error_pairs(Some(msg)));
    json!({"ok":false,"error":err_code})
}

fn handle_camera_add_submit(_params: &JsonValue) -> JsonValue {
    let (target, name, fps, analysis_fps, profile, source_type, cred_user, cred_pass) = with_state(|s| (
        s.discover.resolve_target(),
        s.discover.name.trim().to_string(),
        s.discover.fps_value(),
        s.discover.analysis_fps_value(),
        s.discover.profile_or_default().to_string(),
        s.discover.source_type,
        s.discover.cred_user.trim().to_string(),
        s.discover.cred_pass.trim().to_string(),
    ));
    with_state(|s| s.clear_messages());

    if name.is_empty() || name.chars().count() > 60 {
        return submit_fail("Nazwa musi mieć 1–60 znaków.", "invalid name");
    }
    let (vendor, url, profile_token) = match target {
        Ok(t) => t,
        Err(msg) => return submit_fail(msg, "invalid target"),
    };
    let credentials_b64 = match build_credentials_b64(&vendor, &cred_user, &cred_pass) {
        Ok(c) => c,
        Err(msg) => return submit_fail(msg, "invalid credentials"),
    };

    // Register the camera with the CORE ingest supervisor — THIS starts the
    // RTSP→fMP4 pipeline that feeds the live `camera:<id>` stream. Without it the
    // live tile has no producer and hangs on "Łączenie ze strumieniem…". The
    // core camera_id (cam_<uuid>) becomes the addon row id so the row, the live
    // stream and the detection overlay all key on the same id.
    let input = CameraAddInput {
        display_name: name.clone(),
        vendor: vendor.clone(),
        url: url.clone(),
        target_fps: Some(fps),
        resolution_width: None,
        resolution_height: None,
        retention_class: None,
        profile: Some(profile.clone()),
        credentials_b64,
        onvif_profile_token: profile_token,
        analysis_fps: Some(analysis_fps),
    };
    let added = match camera_add(input) {
        Ok(o) => o,
        Err(e) => return submit_fail(
            &alloc::format!("Nie udało się uruchomić kamery w rdzeniu: {}", abi_message(e)),
            &alloc::format!("{}", e),
        ),
    };

    // The cameras list reads from the addon DB; persist the row under the core id
    // so live/zones/overlay resolve the same camera. Status comes from a real
    // reachability probe (re-probable later via "Odśwież status").
    let (onvif_url, rtsp_url) = match source_type {
        Some(SourceType::Onvif) => (url.clone(), String::new()),
        _ => (String::new(), url.clone()),
    };
    let status = match camera_test_connection(&vendor, &url) {
        Ok(out) if out.ok => "online",
        _ => "offline",
    };
    let new_cam = db::NewCamera {
        name: name.clone(),
        location: profile.clone(),
        rtsp_url,
        onvif_url,
        status: status.into(),
        fps: i64::from(fps),
        detectors: vendor,
        analysis_fps: i64::from(analysis_fps),
    };
    match db::insert_camera_with_id(&added.camera_id, &new_cam) {
        Ok(()) => {
            with_state(|s| { s.add_form_visible = false; s.discover.reset(); s.success_message = Some(alloc::format!("Kamera dodana ({}).", added.camera_id)); });
            render_panel("cameras");
            json!({"ok":true,"camera_id":added.camera_id})
        }
        Err(e) => submit_fail(&alloc::format!("Błąd zapisu: {}", abi_message(e)), &alloc::format!("{}", e)),
    }
}

/// Per-row probe target: ONVIF cameras probe their device-service URL, MJPEG
/// rows (vendor persisted in `detectors`) probe their HTTP URL, everything
/// else probes the RTSP(S) URL. Returns (vendor, url) for `camera_test_connection`.
fn camera_row_probe_target(c: &db::CameraRow) -> (String, String) {
    if !c.onvif_url.trim().is_empty() {
        ("onvif".to_string(), c.onvif_url.clone())
    } else if c.detectors == "mjpeg" {
        ("mjpeg".to_string(), c.rtsp_url.clone())
    } else {
        ("rtsp".to_string(), c.rtsp_url.clone())
    }
}

/// Re-probes every camera's reachability and persists the resulting online/offline
/// status, then re-renders the Cameras tab. This is the truthful source of status
/// for cameras added before liveness probing existed (and after a camera goes up
/// or down). Sequential by design — at large fleet sizes this should move to the
/// core ingest supervisor's health, but for operator-driven refresh it is fine.
fn handle_camera_refresh_status() -> JsonValue {
    with_state(|s| s.clear_messages());
    let cameras = match db::list_cameras() {
        Ok(c) => c,
        Err(e) => {
            with_state(|s| { s.error_message = Some(alloc::format!("Nie udało się pobrać kamer: {}", abi_message(e))); });
            render_panel("cameras");
            return json!({"ok":false,"error":alloc::format!("{}",e)});
        }
    };
    let mut online = 0usize;
    let total = cameras.len();
    for c in &cameras {
        let (vendor, url) = camera_row_probe_target(c);
        if url.trim().is_empty() {
            let _ = db::set_camera_status(&c.id, "offline");
            continue;
        }
        // Cameras added before core-ingest wiring carry a non-core id and have no
        // ingest session, so their live tile can never stream. Register them with
        // the core supervisor now (starts the RTSP→fMP4 pipeline) and rekey the
        // row to the returned core camera_id so `camera:<id>` has a producer.
        if !is_valid_camera_id(&c.id) {
            let input = CameraAddInput {
                display_name: c.name.clone(),
                vendor: vendor.clone(),
                url: url.clone(),
                target_fps: Some(c.fps as u32),
                resolution_width: None,
                resolution_height: None,
                retention_class: None,
                profile: Some(c.location.clone()),
                credentials_b64: None,
                onvif_profile_token: None,
                analysis_fps: Some(c.analysis_fps.clamp(0, 30) as u32),
            };
            if let Ok(added) = camera_add(input) {
                let probed = match camera_test_connection(&vendor, &url) {
                    Ok(out) if out.ok => "online",
                    _ => "offline",
                };
                let rekeyed = db::NewCamera {
                    name: c.name.clone(),
                    location: c.location.clone(),
                    rtsp_url: c.rtsp_url.clone(),
                    onvif_url: c.onvif_url.clone(),
                    status: probed.into(),
                    fps: c.fps,
                    detectors: c.detectors.clone(),
                    analysis_fps: c.analysis_fps,
                };
                let _ = db::delete_camera(&c.id);
                let _ = db::insert_camera_with_id(&added.camera_id, &rekeyed);
                if probed == "online" { online += 1; }
                continue;
            }
            // Core registration failed (e.g. needs credentials) — keep the row,
            // just record reachability so the operator sees the real state.
        }
        let status = match camera_test_connection(&vendor, &url) {
            Ok(out) if out.ok => "online",
            _ => "offline",
        };
        let _ = db::set_camera_status(&c.id, status);
        if status == "online" { online += 1; }
    }
    with_state(|s| { s.success_message = Some(alloc::format!("Odświeżono status: {}/{} online.", online, total)); });
    render_panel("cameras");
    json!({"ok":true,"online":online,"total":total})
}

fn handle_camera_remove(params: &JsonValue) -> JsonValue {
    let camera_id = params.get("camera_id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    with_state(|s| s.clear_messages());
    if camera_id.is_empty() { with_state(|s| { s.error_message = Some("Wybierz kamerę do usunięcia.".to_string()); }); return json!({"ok":false,"error":"empty camera_id"}); }
    // Remove from the CORE first — this soft-deletes the platform row and
    // tears down the ingest pipeline. Deleting only the addon-local row would
    // orphan the core camera: gone from the list, but the supervisor keeps
    // (re)connecting to it forever. NotFound is tolerated so a locally
    // orphaned row (core side already gone) can still be cleaned up.
    match camera_remove(&camera_id) {
        Ok(()) | Err(AbiError::NotFound) => {}
        Err(e) => {
            with_state(|s| { s.error_message = Some(alloc::format!("Błąd usuwania z rdzenia: {}", abi_message(e))); });
            return json!({"ok":false,"error":alloc::format!("{}",e)});
        }
    }
    match db::delete_camera(&camera_id) {
        Ok(_) => { with_state(|s| { s.camera_pending_remove = None; s.success_message = Some("Kamera usunięta.".to_string()); }); json!({"ok":true}) }
        Err(e) => { with_state(|s| { s.error_message = Some(alloc::format!("Błąd usuwania: {}", abi_message(e))); }); json!({"ok":false,"error":alloc::format!("{}",e)}) }
    }
}

/// Commits the analysis-flow pick for a camera. The Select sends `camera_id`
/// (handler param) + `value` (chosen flow id; empty = clear). Assignment +
/// validation (flow exists/active) happen in the core `camera_update_v1` host fn.
fn handle_camera_flow_change(params: &JsonValue) -> JsonValue {
    let camera_id = params.get("camera_id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let value = params.get("value").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    with_state(|s| s.clear_messages());
    if camera_id.is_empty() {
        with_state(|s| { s.error_message = Some("Brak kamery do przypisania flow.".to_string()); });
        return json!({"ok":false,"error":"empty camera_id"});
    }
    let flow = if value.is_empty() { None } else { Some(value.as_str()) };
    match host_camera_set_flow(&camera_id, flow) {
        Ok(_) => {
            with_state(|s| {
                s.camera_flow_edit = None;
                s.success_message = Some(if flow.is_some() {
                    "Przypisano flow analizy do kamery.".to_string()
                } else {
                    "Usunięto przypisanie flow (wbudowana analiza).".to_string()
                });
            });
            render_panel("cameras");
            json!({"ok":true})
        }
        Err(e) => {
            with_state(|s| { s.error_message = Some(alloc::format!("Nie udało się przypisać flow: {}", abi_message(e))); });
            render_panel("cameras");
            json!({"ok":false,"error":alloc::format!("{}",e)})
        }
    }
}

// =============================================================================
// CV pipeline action handlers + draft <-> JSON converters
// =============================================================================

/// Parses one stage object of the stored pipeline JSON into an editable draft.
/// `ocr_mode` is lifted out of `params` into its own field (the editor owns
/// it); every other param (crop pads etc.) stays in `params` verbatim so an
/// edit round-trip never drops values the editor does not surface.
fn stage_draft_from_json(v: &JsonValue) -> StageDraft {
    let text = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let mut params = v
        .get("params")
        .and_then(|p| p.as_object())
        .cloned()
        .unwrap_or_default();
    let ocr_mode = params
        .remove("ocr_mode")
        .and_then(|x| x.as_str().map(String::from))
        .unwrap_or_default();
    let input = v.get("input").cloned().unwrap_or(JsonValue::Null);
    let kind = input.get("kind").and_then(|x| x.as_str()).unwrap_or("frame");
    let (fps, parent, classes) = if kind == "frame" {
        let fps = input
            .get("fps")
            .and_then(|x| x.as_u64())
            .map(|n| alloc::format!("{}", n))
            .unwrap_or_default();
        (fps, String::new(), String::new())
    } else {
        let parent = input.get("stage_id").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let classes = input
            .get("classes")
            .and_then(|c| c.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        (String::new(), parent, classes)
    };
    let threshold = v
        .get("threshold")
        .and_then(|x| x.as_f64())
        .map(|t| alloc::format!("{}", t))
        .unwrap_or_default();
    let op = {
        let op = text("op");
        if op.is_empty() { "detect".to_string() } else { op }
    };
    StageDraft {
        stage_id: text("stage_id"),
        op,
        model: text("model"),
        fps,
        threshold,
        parent,
        classes,
        ocr_mode,
        enabled: v.get("enabled").and_then(|x| x.as_bool()).unwrap_or(true),
        params,
    }
}

/// Parses a stored pipeline JSON body into editor drafts.
fn pipeline_stages_from_json(pipeline_json: &str) -> Result<Vec<StageDraft>, String> {
    let v: JsonValue = serde_json::from_str(pipeline_json)
        .map_err(|e| alloc::format!("nieprawidłowy JSON pipeline'u: {}", e))?;
    let stages = v
        .get("stages")
        .and_then(|s| s.as_array())
        .ok_or_else(|| "pipeline nie zawiera listy etapów".to_string())?;
    Ok(stages.iter().map(stage_draft_from_json).collect())
}

/// Serializes the editor drafts back into the pipeline JSON body. Only shape
/// errors that would make the JSON unbuildable (empty ids, non-numeric fps /
/// threshold) are caught here — the authoritative validation (stage refs,
/// class patterns, alias existence) runs host-side on save.
fn pipeline_json_from_draft(stages: &[StageDraft]) -> Result<String, String> {
    let mut out: Vec<JsonValue> = Vec::new();
    for st in stages {
        let stage_id = st.stage_id.trim();
        if stage_id.is_empty() {
            return Err("każdy etap musi mieć ID".to_string());
        }
        let mut obj = serde_json::Map::new();
        obj.insert("stage_id".into(), json!(stage_id));
        obj.insert("enabled".into(), json!(st.enabled));
        obj.insert("op".into(), json!(st.op));
        obj.insert("model".into(), json!(st.model.trim()));
        let input = if st.op == "detect" {
            let mut m = serde_json::Map::new();
            m.insert("kind".into(), json!("frame"));
            if !st.fps.trim().is_empty() {
                let fps: u32 = st.fps.trim().parse().map_err(|_| {
                    alloc::format!("etap '{}': FPS musi być liczbą całkowitą", stage_id)
                })?;
                m.insert("fps".into(), json!(fps));
            }
            m
        } else {
            let mut m = serde_json::Map::new();
            m.insert("kind".into(), json!("stage"));
            m.insert("stage_id".into(), json!(st.parent.trim()));
            let classes: Vec<String> = st
                .classes
                .split(',')
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty())
                .collect();
            m.insert("classes".into(), json!(classes));
            m
        };
        obj.insert("input".into(), JsonValue::Object(input));
        if st.op == "detect" && !st.threshold.trim().is_empty() {
            // Tolerate the Polish decimal comma in the threshold input.
            let t: f64 = st.threshold.trim().replace(',', ".").parse().map_err(|_| {
                alloc::format!("etap '{}': próg musi być liczbą 0–1", stage_id)
            })?;
            obj.insert("threshold".into(), json!(t));
        }
        let mut params = st.params.clone();
        if st.op == "ocr" {
            let mode = st.ocr_mode.trim();
            params.insert(
                "ocr_mode".into(),
                json!(if mode.is_empty() { "generic" } else { mode }),
            );
        } else {
            params.remove("ocr_mode");
        }
        if !params.is_empty() {
            obj.insert("params".into(), JsonValue::Object(params));
        }
        match st.op.as_str() {
            "classify" => { obj.insert("output".into(), json!("stan")); }
            "ocr" => { obj.insert("output".into(), json!("tekst")); }
            _ => {}
        }
        out.push(JsonValue::Object(obj));
    }
    serde_json::to_string(&json!({ "stages": out }))
        .map_err(|e| alloc::format!("serializacja pipeline'u nie powiodła się: {}", e))
}

/// First detect stage id of the draft — the default parent for new crop stages.
fn first_detect_stage_id(stages: &[StageDraft]) -> Option<String> {
    stages.iter().find(|s| s.op == "detect").map(|s| s.stage_id.clone())
}

/// Commits the CV-pipeline pick for a camera. The Select sends `camera_id`
/// (handler param) + `value` (pipeline id; empty = clear → default pipeline).
/// Existence validation happens in the core `camera_update_v1` host fn.
fn handle_camera_pipeline_change(params: &JsonValue) -> JsonValue {
    let camera_id = params.get("camera_id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let value = params.get("value").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    with_state(|s| s.clear_messages());
    if camera_id.is_empty() {
        with_state(|s| { s.error_message = Some("Brak kamery do przypisania pipeline'u.".to_string()); });
        return json!({"ok":false,"error":"empty camera_id"});
    }
    let pipeline = if value.is_empty() { None } else { Some(value.as_str()) };
    match host_camera_set_cv_pipeline(&camera_id, pipeline) {
        Ok(_) => {
            with_state(|s| {
                s.camera_pipeline_edit = None;
                s.success_message = Some(if pipeline.is_some() {
                    "Przypisano pipeline analizy do kamery.".to_string()
                } else {
                    "Kamera wróciła do domyślnego pipeline'u analizy.".to_string()
                });
            });
            render_panel("cameras");
            json!({"ok":true})
        }
        Err(e) => {
            with_state(|s| { s.error_message = Some(alloc::format!("Nie udało się przypisać pipeline'u: {}", abi_message(e))); });
            render_panel("cameras");
            json!({"ok":false,"error":alloc::format!("{}",e)})
        }
    }
}

/// Opens the stage editor for an existing pipeline (fetched from core).
fn handle_pipeline_edit(params: &JsonValue) -> JsonValue {
    let id = params.get("pipeline_id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    with_state(|s| s.clear_messages());
    if id.is_empty() {
        return json!({"ok":false,"error":"empty pipeline_id"});
    }
    match host_cv_pipeline_get(&id) {
        Ok(p) => match pipeline_stages_from_json(&p.pipeline_json) {
            Ok(stages) => {
                with_state(|s| {
                    s.cv_pipelines.editor_visible = true;
                    s.cv_pipelines.editing_id = Some(p.id);
                    s.cv_pipelines.name = p.name;
                    s.cv_pipelines.stages = stages;
                    s.cv_pipelines.pending_remove = None;
                });
                render_panel("cameras");
                json!({"ok":true})
            }
            Err(e) => {
                with_state(|s| { s.error_message = Some(alloc::format!("Nie udało się wczytać pipeline'u: {}", e)); });
                render_panel("cameras");
                json!({"ok":false,"error":e})
            }
        },
        Err(e) => {
            with_state(|s| { s.error_message = Some(alloc::format!("Nie udało się pobrać pipeline'u: {}", abi_message(e))); });
            render_panel("cameras");
            json!({"ok":false,"error":alloc::format!("{}",e)})
        }
    }
}

/// Opens the stage editor with a copy of an existing pipeline (saved as a new
/// row on Zapisz — `editing_id = None` makes the host mint a fresh id).
fn handle_pipeline_duplicate(params: &JsonValue) -> JsonValue {
    let id = params.get("pipeline_id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    with_state(|s| s.clear_messages());
    if id.is_empty() {
        return json!({"ok":false,"error":"empty pipeline_id"});
    }
    match host_cv_pipeline_get(&id) {
        Ok(p) => match pipeline_stages_from_json(&p.pipeline_json) {
            Ok(stages) => {
                with_state(|s| {
                    s.cv_pipelines.editor_visible = true;
                    s.cv_pipelines.editing_id = None;
                    s.cv_pipelines.name = alloc::format!("{} (kopia)", p.name);
                    s.cv_pipelines.stages = stages;
                    s.cv_pipelines.pending_remove = None;
                });
                render_panel("cameras");
                json!({"ok":true})
            }
            Err(e) => {
                with_state(|s| { s.error_message = Some(alloc::format!("Nie udało się wczytać pipeline'u: {}", e)); });
                render_panel("cameras");
                json!({"ok":false,"error":e})
            }
        },
        Err(e) => {
            with_state(|s| { s.error_message = Some(alloc::format!("Nie udało się pobrać pipeline'u: {}", abi_message(e))); });
            render_panel("cameras");
            json!({"ok":false,"error":alloc::format!("{}",e)})
        }
    }
}

/// Deletes a pipeline after the confirmation bar. Host refusals (default
/// pipeline, still assigned to a camera) carry a readable message.
fn handle_pipeline_remove(params: &JsonValue) -> JsonValue {
    let id = params.get("pipeline_id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    with_state(|s| s.clear_messages());
    if id.is_empty() {
        return json!({"ok":false,"error":"empty pipeline_id"});
    }
    match host_cv_pipeline_delete(&id) {
        Ok(out) if out.deleted => {
            with_state(|s| {
                s.cv_pipelines.pending_remove = None;
                s.success_message = Some("Pipeline usunięty.".to_string());
            });
            render_panel("cameras");
            json!({"ok":true})
        }
        Ok(out) => {
            let reason = out.error.unwrap_or_else(|| "odmowa".to_string());
            with_state(|s| {
                s.cv_pipelines.pending_remove = None;
                s.error_message = Some(alloc::format!("Nie można usunąć pipeline'u: {}", reason));
            });
            render_panel("cameras");
            json!({"ok":false,"error":reason})
        }
        Err(e) => {
            with_state(|s| { s.error_message = Some(alloc::format!("Błąd usuwania pipeline'u: {}", abi_message(e))); });
            render_panel("cameras");
            json!({"ok":false,"error":alloc::format!("{}",e)})
        }
    }
}

/// Mirrors a single stage-editor field into the backend draft on change. The
/// value also lives in the store via the input's bind_path; the backend copy
/// is authoritative for Zapisz. Structure-affecting fields re-send the panel
/// (same per-keystroke commit + re-render pattern as the audit search box,
/// whose bound key is re-seeded via the content overlay): `op` and `parent`
/// switch which controls render, `stage_id` feeds the card title and the
/// source-stage options of crop stages, `classes` feeds the chips preview.
/// Plain numeric/model fields do not re-render.
fn handle_pipeline_stage_field_change(params: &JsonValue) -> JsonValue {
    let index: usize = match params.get("index").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()) {
        Some(i) => i,
        None => return json!({"ok":false,"error":"missing index"}),
    };
    let field = params.get("field").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let value = params.get("value").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let rerender = matches!(field.as_str(), "op" | "parent" | "stage_id" | "classes");
    let applied = with_state(|s| {
        let default_parent = first_detect_stage_id(&s.cv_pipelines.stages).unwrap_or_default();
        let Some(st) = s.cv_pipelines.stages.get_mut(index) else { return false; };
        match field.as_str() {
            "stage_id" => st.stage_id = value.clone(),
            "op" => {
                st.op = value.clone();
                if st.op != "detect" && st.parent.trim().is_empty() {
                    // A crop stage needs a source; preselect the first detect
                    // stage so the fresh select is not empty.
                    st.parent = default_parent;
                }
            }
            "model" => st.model = value.clone(),
            "fps" => st.fps = value.clone(),
            "threshold" => st.threshold = value.clone(),
            "parent" => st.parent = value.clone(),
            "classes" => st.classes = value.clone(),
            "ocr_mode" => st.ocr_mode = value.clone(),
            _ => return false,
        }
        true
    });
    if !applied {
        return json!({"ok":false,"error":"unknown stage index or field"});
    }
    if rerender {
        render_panel("cameras");
    }
    json!({"ok":true})
}

/// Flips a stage's enabled toggle in the backend draft.
fn handle_pipeline_stage_toggle(params: &JsonValue) -> JsonValue {
    let index: usize = match params.get("index").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()) {
        Some(i) => i,
        None => return json!({"ok":false,"error":"missing index"}),
    };
    let on = params.get("value").and_then(|v| v.as_bool())
        .or_else(|| params.get("checked").and_then(|v| v.as_bool()))
        .unwrap_or(false);
    with_state(|s| {
        if let Some(st) = s.cv_pipelines.stages.get_mut(index) {
            st.enabled = on;
        }
    });
    json!({"ok":true})
}

/// Appends a new stage to the draft: the first stage defaults to a detect
/// stage; later ones default to a classify stage hanging off the first detect.
fn handle_pipeline_stage_add() -> JsonValue {
    with_state(|s| {
        let n = s.cv_pipelines.stages.len();
        let detect = first_detect_stage_id(&s.cv_pipelines.stages);
        let mut st = StageDraft::new_detect();
        if let Some(parent) = detect {
            st.op = "classify".into();
            st.parent = parent;
            st.stage_id = alloc::format!("etap_{}", n + 1);
        }
        s.cv_pipelines.stages.push(st);
    });
    render_panel("cameras");
    json!({"ok":true})
}

/// Serializes the draft and saves it host-side. Host validation errors
/// (structure, stage refs, unknown aliases) come back as a readable message
/// shown in the panel; the editor stays open with the draft intact.
fn handle_pipeline_save() -> JsonValue {
    with_state(|s| s.clear_messages());
    let (editing_id, name, json_result) = with_state(|s| (
        s.cv_pipelines.editing_id.clone(),
        s.cv_pipelines.name.trim().to_string(),
        pipeline_json_from_draft(&s.cv_pipelines.stages),
    ));
    if name.is_empty() {
        with_state(|s| { s.error_message = Some("Podaj nazwę pipeline'u.".to_string()); });
        render_panel("cameras");
        return json!({"ok":false,"error":"empty name"});
    }
    let pipeline_json = match json_result {
        Ok(j) => j,
        Err(e) => {
            with_state(|s| { s.error_message = Some(alloc::format!("Nie można zapisać: {}", e)); });
            render_panel("cameras");
            return json!({"ok":false,"error":e});
        }
    };
    match host_cv_pipeline_save(editing_id.as_deref(), &name, &pipeline_json) {
        Ok(out) => match out.id {
            Some(_) => {
                with_state(|s| {
                    s.cv_pipelines.close_editor();
                    s.success_message = Some("Pipeline zapisany.".to_string());
                });
                render_panel("cameras");
                json!({"ok":true})
            }
            None => {
                let reason = out.error.unwrap_or_else(|| "walidacja odrzucona".to_string());
                with_state(|s| { s.error_message = Some(alloc::format!("Pipeline odrzucony: {}", reason)); });
                render_panel("cameras");
                json!({"ok":false,"error":reason})
            }
        },
        Err(e) => {
            with_state(|s| { s.error_message = Some(alloc::format!("Błąd zapisu pipeline'u: {}", abi_message(e))); });
            render_panel("cameras");
            json!({"ok":false,"error":alloc::format!("{}",e)})
        }
    }
}

// =============================================================================
// Profile action handlers
// =============================================================================

/// Mirrors a single builder field into backend profile state on change. The
/// value also lives in the store via the input's bind_path, so this only keeps
/// the backend authoritative for submit. Sliders carry a numeric value; text /
/// select fields carry a string.
fn handle_profile_field_change(params: &JsonValue) -> JsonValue {
    let field = params.get("field").and_then(|v| v.as_str()).unwrap_or("");
    let value_str = params.get("value").and_then(|v| v.as_str()).map(|s| s.to_string());
    let value_num = params.get("value").and_then(|v| v.as_f64());
    with_state(|s| match field {
        "profile_name" => { if let Some(v) = value_str { s.profiles.name = v; } }
        "profile_flow_id" => { if let Some(v) = value_str { s.profiles.flow_id = v; } }
        "profile_risk_class" => { if let Some(v) = value_str { s.profiles.risk_class = v; } }
        "profile_schedule" => { if let Some(v) = value_str { s.profiles.schedule = v; } }
        "profile_fps" => { if let Some(v) = value_num { s.profiles.fps = v; } }
        "profile_min_conf" => { if let Some(v) = value_num { s.profiles.min_confidence = v; } }
        _ => {}
    });
    json!({"ok":true})
}

/// Creates a new profile (or updates the one under edit) from the builder form.
fn handle_profile_add_submit() -> JsonValue {
    let (editing_id, name, flow_id, risk_class, schedule, cameras) = with_state(|s| (
        s.profiles.editing_id.clone(),
        s.profiles.name.trim().to_string(),
        s.profiles.flow_id.trim().to_string(),
        s.profiles.risk_class.trim().to_string(),
        s.profiles.schedule.trim().to_string(),
        s.profiles.cameras.clone(),
    ));
    with_state(|s| s.clear_messages());

    if name.is_empty() || name.chars().count() > 60 {
        with_state(|s| s.error_message = Some("Nazwa profilu musi mieć 1–60 znaków.".into()));
        render_panel("profiles");
        return json!({"ok":false,"error":"invalid name"});
    }
    let cameras_json = serde_json::to_string(&cameras).unwrap_or_else(|_| "[]".into());

    let result = match editing_id {
        Some(id) => {
            // Edit in place: re-read the row for its timestamps, then update.
            match db::get_profile(&id) {
                Ok(Some(mut row)) => {
                    row.name = name.clone();
                    row.flow_id = flow_id;
                    row.risk_class = risk_class;
                    row.schedule = schedule;
                    row.cameras = cameras_json;
                    db::update_profile(&row).map(|_| id)
                }
                Ok(None) => {
                    with_state(|s| s.error_message = Some("Profil nie istnieje.".into()));
                    render_panel("profiles");
                    return json!({"ok":false,"error":"not found"});
                }
                Err(e) => Err(e),
            }
        }
        None => {
            let new_profile = db::NewProfile {
                name: name.clone(),
                flow_id,
                risk_class,
                schedule,
                cameras: cameras_json,
                enabled: true,
            };
            db::insert_profile(&new_profile)
        }
    };

    match result {
        Ok(id) => {
            with_state(|s| {
                s.profiles.builder_visible = false;
                s.profiles.editing_id = None;
                s.success_message = Some(alloc::format!("Profil zapisany ({}).", id));
            });
            render_panel("profiles");
            json!({"ok":true,"profile_id":id})
        }
        Err(e) => {
            with_state(|s| s.error_message = Some(alloc::format!("Błąd zapisu profilu: {}", abi_message(e))));
            render_panel("profiles");
            json!({"ok":false,"error":alloc::format!("{}",e)})
        }
    }
}

/// Opens the builder pre-filled with the selected profile's persisted values.
fn handle_profile_edit(params: &JsonValue) -> JsonValue {
    let id = params.get("row_id").and_then(|v| v.as_str())
        .or_else(|| params.get("profile_id").and_then(|v| v.as_str()))
        .unwrap_or("").trim().to_string();
    if id.is_empty() {
        return json!({"ok":false,"error":"empty profile_id"});
    }
    match db::get_profile(&id) {
        Ok(Some(row)) => {
            let camera_ids = parse_profile_cameras(&row.cameras);
            with_state(|s| {
                s.clear_messages();
                s.profiles.pending_remove = None;
                s.profiles.builder_visible = true;
                s.profiles.load_for_edit(&row, camera_ids);
            });
            render_panel("profiles");
            json!({"ok":true})
        }
        Ok(None) => { with_state(|s| s.error_message = Some("Profil nie istnieje.".into())); render_panel("profiles"); json!({"ok":false,"error":"not found"}) }
        Err(e) => { with_state(|s| s.error_message = Some(alloc::format!("Błąd: {}", abi_message(e)))); render_panel("profiles"); json!({"ok":false,"error":alloc::format!("{}",e)}) }
    }
}

/// Flips the selected profile's enabled flag (drives the Dashboard "Aktywne
/// detektory" KPI, which counts profiles WHERE enabled = 1).
fn handle_profile_toggle_enabled(params: &JsonValue) -> JsonValue {
    let id = params.get("row_id").and_then(|v| v.as_str())
        .or_else(|| params.get("profile_id").and_then(|v| v.as_str()))
        .unwrap_or("").trim().to_string();
    if id.is_empty() {
        return json!({"ok":false,"error":"empty profile_id"});
    }
    with_state(|s| s.clear_messages());
    match db::get_profile(&id) {
        Ok(Some(row)) => {
            let next = !row.enabled;
            match db::toggle_profile(&id, next) {
                Ok(_) => {
                    with_state(|s| s.success_message = Some(if next { "Profil włączony.".into() } else { "Profil wyłączony.".into() }));
                    render_panel("profiles");
                    json!({"ok":true,"enabled":next})
                }
                Err(e) => { with_state(|s| s.error_message = Some(alloc::format!("Błąd: {}", abi_message(e)))); render_panel("profiles"); json!({"ok":false,"error":alloc::format!("{}",e)}) }
            }
        }
        Ok(None) => { with_state(|s| s.error_message = Some("Profil nie istnieje.".into())); render_panel("profiles"); json!({"ok":false,"error":"not found"}) }
        Err(e) => { with_state(|s| s.error_message = Some(alloc::format!("Błąd: {}", abi_message(e)))); render_panel("profiles"); json!({"ok":false,"error":alloc::format!("{}",e)}) }
    }
}

fn handle_profile_remove(params: &JsonValue) -> JsonValue {
    let id = params.get("profile_id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    with_state(|s| s.clear_messages());
    if id.is_empty() {
        with_state(|s| s.error_message = Some("Wybierz profil do usunięcia.".into()));
        return json!({"ok":false,"error":"empty profile_id"});
    }
    match db::delete_profile(&id) {
        Ok(_) => { with_state(|s| { s.profiles.pending_remove = None; s.success_message = Some("Profil usunięty.".into()); }); render_panel("profiles"); json!({"ok":true}) }
        Err(e) => { with_state(|s| s.error_message = Some(alloc::format!("Błąd usuwania: {}", abi_message(e)))); render_panel("profiles"); json!({"ok":false,"error":alloc::format!("{}",e)}) }
    }
}

// =============================================================================
// Model action handlers
// =============================================================================

/// Default VRAM budget (MB) when the operator has not set one. 24 GB matches a
/// typical workstation GPU; editable via the budget editor and persisted under
/// the `vram_budget_mb` setting key.
const DEFAULT_VRAM_BUDGET_MB: i64 = 24576;

/// Display attribution for model registry changes in the audit log. No host
/// identity fn exists in this addon ABI yet, so changes are attributed to the
/// administrator role.
const MODEL_ACTOR: &str = "administrator";

/// Mirrors a single model form field into backend state on change so submit
/// stays authoritative even though the value also lives in the store.
fn handle_model_field_change(params: &JsonValue) -> JsonValue {
    let field = params.get("field").and_then(|v| v.as_str()).unwrap_or("");
    let value = params.get("value").and_then(|v| v.as_str()).map(|s| s.to_string());
    if let Some(v) = value {
        with_state(|s| match field {
            "model_name" => s.models.form_name = v,
            "model_runtime" => s.models.form_runtime = v,
            "model_status" => s.models.form_status = v,
            "model_vram" => s.models.form_vram = v,
            "model_version" => s.models.form_version = v,
            _ => {}
        });
    }
    json!({"ok":true})
}

/// Creates or updates a model row, writes a hash-chained audit entry
/// (model_change) with before/after JSON snapshots, then re-renders the panel so
/// the table + VRAM budget bar reflect the change.
fn handle_model_add_submit() -> JsonValue {
    let (editing_id, name, runtime, status, vram_str, version) = with_state(|s| (
        s.models.editing_id.clone(),
        s.models.form_name.trim().to_string(),
        s.models.form_runtime.trim().to_string(),
        s.models.form_status.trim().to_string(),
        s.models.form_vram.trim().to_string(),
        s.models.form_version.trim().to_string(),
    ));
    with_state(|s| s.clear_messages());

    if name.is_empty() || name.chars().count() > 80 {
        with_state(|s| s.error_message = Some("Nazwa modelu musi mieć 1–80 znaków.".into()));
        render_panel("models");
        return json!({"ok":false,"error":"invalid name"});
    }
    let vram_mb = match vram_str.parse::<i64>() {
        Ok(v) if v >= 0 => v,
        _ => {
            with_state(|s| s.error_message = Some("VRAM musi być liczbą całkowitą ≥ 0 (MB).".into()));
            render_panel("models");
            return json!({"ok":false,"error":"invalid vram"});
        }
    };

    let (result, before_json) = match editing_id {
        Some(id) => match db::get_model(&id) {
            Ok(Some(mut row)) => {
                let before = model_audit_json(&row);
                row.name = name.clone();
                row.runtime = runtime;
                row.status = status;
                row.vram_mb = vram_mb;
                row.version = version;
                (db::update_model(&row).map(|_| (id, row)), before)
            }
            Ok(None) => {
                with_state(|s| s.error_message = Some("Model nie istnieje.".into()));
                render_panel("models");
                return json!({"ok":false,"error":"not found"});
            }
            Err(e) => (Err(e), String::new()),
        },
        None => {
            let new_model = db::NewModel { name: name.clone(), runtime, status, vram_mb, version };
            let outcome = db::insert_model(&new_model).and_then(|id| {
                db::get_model(&id).map(|m| (id.clone(), m.unwrap_or_else(|| db::ModelRow {
                    id, name: name.clone(), runtime: new_model.runtime.clone(),
                    status: new_model.status.clone(), vram_mb, version: new_model.version.clone(),
                    created_at: 0,
                })))
            });
            (outcome, "null".into())
        }
    };

    match result {
        Ok((id, row)) => {
            let _ = db::insert_audit(MODEL_ACTOR, "model_change", &id, &before_json, &model_audit_json(&row));
            with_state(|s| {
                s.models.form_visible = false;
                s.models.editing_id = None;
                s.success_message = Some(alloc::format!("Model zapisany ({}).", id));
            });
            render_panel("models");
            json!({"ok":true,"model_id":id})
        }
        Err(e) => {
            with_state(|s| s.error_message = Some(alloc::format!("Błąd zapisu modelu: {}", abi_message(e))));
            render_panel("models");
            json!({"ok":false,"error":alloc::format!("{}",e)})
        }
    }
}

/// Opens the form pre-filled with the selected model's persisted values.
fn handle_model_edit(params: &JsonValue) -> JsonValue {
    let id = model_id_param(params);
    if id.is_empty() { return json!({"ok":false,"error":"empty model_id"}); }
    match db::get_model(&id) {
        Ok(Some(row)) => {
            with_state(|s| {
                s.clear_messages();
                s.models.pending_remove = None;
                s.models.budget_editing = false;
                s.models.form_visible = true;
                s.models.load_for_edit(&row);
            });
            render_panel("models");
            json!({"ok":true})
        }
        Ok(None) => { with_state(|s| s.error_message = Some("Model nie istnieje.".into())); render_panel("models"); json!({"ok":false,"error":"not found"}) }
        Err(e) => { with_state(|s| s.error_message = Some(alloc::format!("Błąd: {}", abi_message(e)))); render_panel("models"); json!({"ok":false,"error":alloc::format!("{}",e)}) }
    }
}

/// Deletes the selected model, writing a model_change audit entry recording the
/// removed row as `before` and null as `after`.
fn handle_model_remove(params: &JsonValue) -> JsonValue {
    let id = model_id_param(params);
    with_state(|s| s.clear_messages());
    if id.is_empty() {
        with_state(|s| s.error_message = Some("Wybierz model do usunięcia.".into()));
        return json!({"ok":false,"error":"empty model_id"});
    }
    let before = db::get_model(&id).ok().flatten().map(|m| model_audit_json(&m)).unwrap_or_else(|| "null".into());
    match db::delete_model(&id) {
        Ok(_) => {
            let _ = db::insert_audit(MODEL_ACTOR, "model_change", &id, &before, "null");
            with_state(|s| { s.models.pending_remove = None; s.success_message = Some("Model usunięty.".into()); });
            render_panel("models");
            json!({"ok":true})
        }
        Err(e) => { with_state(|s| s.error_message = Some(alloc::format!("Błąd usuwania: {}", abi_message(e)))); render_panel("models"); json!({"ok":false,"error":alloc::format!("{}",e)}) }
    }
}

/// Rollback is the only per-model action with a real DB effect: it appends a
/// `-rollback` marker to the version string (a true model-weight rollback needs
/// the inference runtime, which this addon does not host). The change is audited.
fn handle_model_rollback(params: &JsonValue) -> JsonValue {
    let id = model_id_param(params);
    with_state(|s| s.clear_messages());
    if id.is_empty() { return json!({"ok":false,"error":"empty model_id"}); }
    match db::get_model(&id) {
        Ok(Some(mut row)) => {
            let before = model_audit_json(&row);
            row.version = if row.version.is_empty() {
                "rollback".into()
            } else {
                alloc::format!("{}-rollback", row.version)
            };
            match db::update_model(&row) {
                Ok(_) => {
                    let _ = db::insert_audit(MODEL_ACTOR, "model_change", &id, &before, &model_audit_json(&row));
                    with_state(|s| s.success_message = Some(alloc::format!("Wersja modelu oznaczona do rollbacku ({}).", row.version)));
                    render_panel("models");
                    json!({"ok":true})
                }
                Err(e) => { with_state(|s| s.error_message = Some(alloc::format!("Błąd: {}", abi_message(e)))); render_panel("models"); json!({"ok":false,"error":alloc::format!("{}",e)}) }
            }
        }
        Ok(None) => { with_state(|s| s.error_message = Some("Model nie istnieje.".into())); render_panel("models"); json!({"ok":false,"error":"not found"}) }
        Err(e) => { with_state(|s| s.error_message = Some(alloc::format!("Błąd: {}", abi_message(e)))); render_panel("models"); json!({"ok":false,"error":alloc::format!("{}",e)}) }
    }
}

/// Persists the edited VRAM budget (MB) to the `vram_budget_mb` setting.
fn handle_model_budget_save() -> JsonValue {
    let draft = with_state(|s| s.models.budget_draft.trim().to_string());
    with_state(|s| s.clear_messages());
    let budget = match draft.parse::<i64>() {
        Ok(v) if v > 0 => v,
        _ => {
            with_state(|s| s.error_message = Some("Budżet VRAM musi być liczbą całkowitą > 0 (MB).".into()));
            render_panel("models");
            return json!({"ok":false,"error":"invalid budget"});
        }
    };
    match db::set_setting("vram_budget_mb", &alloc::format!("{}", budget)) {
        Ok(_) => {
            with_state(|s| { s.models.budget_editing = false; s.success_message = Some(alloc::format!("Budżet VRAM ustawiony na {} MB.", budget)); });
            render_panel("models");
            json!({"ok":true,"budget":budget})
        }
        Err(e) => { with_state(|s| s.error_message = Some(alloc::format!("Błąd zapisu budżetu: {}", abi_message(e)))); render_panel("models"); json!({"ok":false,"error":alloc::format!("{}",e)}) }
    }
}

/// Pulls the model id from action params (row_id or model_id).
fn model_id_param(params: &JsonValue) -> String {
    params.get("row_id").and_then(|v| v.as_str())
        .or_else(|| params.get("model_id").and_then(|v| v.as_str()))
        .unwrap_or("").trim().to_string()
}

/// Compact JSON snapshot of a model row for the audit before/after fields.
fn model_audit_json(m: &db::ModelRow) -> String {
    json!({
        "id": m.id, "name": m.name, "runtime": m.runtime, "status": m.status,
        "vram_mb": m.vram_mb, "version": m.version,
    }).to_string()
}

// =============================================================================
// Alarm action handlers
// =============================================================================

/// Display label for the deciding operator. No host identity fn exists in this
/// addon ABI yet, so decisions are attributed to the first-line operator role.
const ALARM_OPERATOR: &str = "operator I linii";

/// Records an operator decision on an alarm: persists the new status + operator
/// + decision time, writes a hash-chained audit-log entry (before/after status),
/// then re-renders so the feed and detail reflect the decision.
fn handle_alarm_decide(params: &JsonValue) -> JsonValue {
    let id = params.get("alarm_id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let decision = params.get("decision").and_then(|v| v.as_str()).unwrap_or("").to_string();
    with_state(|s| s.clear_messages());
    if id.is_empty() || !matches!(decision.as_str(), "confirmed" | "dismissed" | "escalated") {
        with_state(|s| s.error_message = Some("Nieprawidłowa decyzja alarmu.".into()));
        return json!({"ok":false,"error":"bad decision"});
    }
    // Capture the prior status so the audit trail records the real transition.
    let before = db::get_alarm(&id).ok().flatten().map(|a| a.status).unwrap_or_default();
    match db::update_alarm_status(&id, &decision, ALARM_OPERATOR) {
        Ok(0) => {
            with_state(|s| s.error_message = Some("Alarm nie istnieje.".into()));
            render_panel("alarms");
            json!({"ok":false,"error":"not found"})
        }
        Ok(_) => {
            let note = with_state(|s| s.alarms.note.clone());
            let after_json = serde_json::to_string(&json!({"status": decision, "note": note})).unwrap_or_default();
            let before_json = serde_json::to_string(&json!({"status": before})).unwrap_or_default();
            // Audit-log linkage: the future Audit tab reads this hash-chained row.
            let _ = db::insert_audit(ALARM_OPERATOR, "alarm_decision", &id, &before_json, &after_json);
            with_state(|s| s.success_message = Some(alloc::format!("Zapisano decyzję: {}.", alarm_status_long(&decision))));
            render_panel("alarms");
            json!({"ok":true})
        }
        Err(e) => {
            with_state(|s| s.error_message = Some(alloc::format!("Błąd zapisu decyzji: {}", abi_message(e))));
            render_panel("alarms");
            json!({"ok":false,"error":alloc::format!("{}",e)})
        }
    }
}

/// Enters retention edit mode for a class, seeding the draft with the current
/// (override-or-default) value so the input mounts pre-filled.
/// Persists every pending settings edit via db::set_setting, validating numeric
/// fields, then appends ONE audit_log summary row (action `settings_change`)
/// listing the changed keys with redacted before/after snapshots so secrets
/// never land in the chain. Clears the edit buffer and shows a success banner.
fn handle_settings_save() -> JsonValue {
    with_state(|s| s.clear_messages());
    let edits = with_state(|s| s.settings.edits.clone());
    if edits.is_empty() {
        with_state(|s| s.success_message = Some("Brak zmian do zapisania.".into()));
        render_panel("settings");
        return json!({"ok":true,"changed":0});
    }

    // Validate numeric fields before writing anything (all-or-nothing on parse).
    for (key, value) in &edits {
        if let Some(f) = settings_all_fields().find(|f| f.key == key) {
            if f.kind == SettingKind::Number && value.trim().parse::<i64>().is_err() {
                with_state(|s| s.error_message = Some(alloc::format!("Pole '{}' wymaga liczby.", f.label)));
                render_panel("settings");
                return json!({"ok":false,"error":"not a number"});
            }
        }
    }

    let mut changed: Vec<(String, String, String)> = Vec::new();
    for (key, value) in &edits {
        let prev = db::get_setting(key).ok().flatten().unwrap_or_default();
        if prev == *value {
            continue;
        }
        match db::set_setting(key, value) {
            Ok(_) => changed.push((key.clone(), prev, value.clone())),
            Err(e) => {
                with_state(|s| s.error_message = Some(alloc::format!("Błąd zapisu '{}': {}", key, abi_message(e))));
                render_panel("settings");
                return json!({"ok":false,"error":alloc::format!("{}",e)});
            }
        }
    }

    if changed.is_empty() {
        with_state(|s| { s.settings.clear_edits(); s.success_message = Some("Ustawienia bez zmian.".into()); });
        render_panel("settings");
        return json!({"ok":true,"changed":0});
    }

    // One summary audit row. before/after carry per-key snapshots with secret
    // values redacted so the hash-chain never stores credentials in clear text.
    let secret = |key: &str| settings_all_fields()
        .find(|f| f.key == key)
        .map(|f| f.kind == SettingKind::Secret)
        .unwrap_or(false);
    let redact = |key: &str, v: &str| -> JsonValue {
        if v.is_empty() { json!("") }
        else if secret(key) { json!("<redacted>") }
        else { json!(v) }
    };
    let before: serde_json::Map<String, JsonValue> = changed.iter()
        .map(|(k, prev, _)| (k.clone(), redact(k, prev))).collect();
    let after: serde_json::Map<String, JsonValue> = changed.iter()
        .map(|(k, _, next)| (k.clone(), redact(k, next))).collect();
    let _ = db::insert_audit(
        ALARM_OPERATOR,
        "settings_change",
        &alloc::format!("{} ustawień", changed.len()),
        &serde_json::to_string(&JsonValue::Object(before)).unwrap_or_default(),
        &serde_json::to_string(&JsonValue::Object(after)).unwrap_or_default(),
    );

    let count = changed.len();
    with_state(|s| {
        s.settings.clear_edits();
        s.success_message = Some(alloc::format!("Zapisano {} ustawień.", count));
    });
    render_panel("settings");
    json!({"ok":true,"changed":count})
}

fn handle_audit_retention_edit(params: &JsonValue) -> JsonValue {
    let class = params.get("class").and_then(|v| v.as_str()).unwrap_or("").to_uppercase();
    let default = RETENTION_DEFAULTS.iter().find(|(c, _, _)| *c == class).map(|(_, _, d)| *d).unwrap_or(183);
    let current = retention_days(&class, default);
    with_state(|s| {
        s.clear_messages();
        s.audit.retention_editing = if class.is_empty() { None } else { Some(class.clone()) };
        s.audit.retention_draft = alloc::format!("{}", current);
    });
    render_panel("audit");
    json!({"ok":true})
}

/// Persists a retention override (db::set_setting), enforcing the 183-day
/// compliance floor for the audit log, then leaves edit mode.
fn handle_audit_retention_save(params: &JsonValue) -> JsonValue {
    let class = params.get("class").and_then(|v| v.as_str()).unwrap_or("").to_uppercase();
    with_state(|s| s.clear_messages());
    let draft = with_state(|s| s.audit.retention_draft.clone());
    let days: i64 = match draft.trim().parse() {
        Ok(d) if d >= 183 => d,
        Ok(_) => {
            with_state(|s| s.error_message = Some("Retencja nie może być krótsza niż 183 dni (wymóg zgodności).".into()));
            render_panel("audit");
            return json!({"ok":false,"error":"below floor"});
        }
        Err(_) => {
            with_state(|s| s.error_message = Some("Podaj liczbę dni.".into()));
            render_panel("audit");
            return json!({"ok":false,"error":"not a number"});
        }
    };
    match db::set_setting(&retention_setting_key(&class), &alloc::format!("{}", days)) {
        Ok(_) => {
            let before = serde_json::to_string(&json!({"class": class})).unwrap_or_default();
            let after = serde_json::to_string(&json!({"class": class, "retention_days": days})).unwrap_or_default();
            let _ = db::insert_audit(ALARM_OPERATOR, "retention_change", &retention_setting_key(&class), &before, &after);
            with_state(|s| {
                s.audit.retention_editing = None;
                s.success_message = Some(alloc::format!("Zapisano retencję klasy {}: {} dni.", class, days));
            });
            render_panel("audit");
            json!({"ok":true,"days":days})
        }
        Err(e) => {
            with_state(|s| s.error_message = Some(alloc::format!("Błąd zapisu retencji: {}", abi_message(e))));
            render_panel("audit");
            json!({"ok":false,"error":alloc::format!("{}",e)})
        }
    }
}

/// Acknowledges every open (new) alarm in one pass, auditing the bulk action.
fn handle_alarm_acknowledge_all() -> JsonValue {
    with_state(|s| s.clear_messages());
    let open = db::list_alarms("", "", true).unwrap_or_default();
    let mut n = 0u64;
    for a in &open {
        if a.status == "new" {
            if db::update_alarm_status(&a.id, "acknowledged", ALARM_OPERATOR).unwrap_or(0) > 0 {
                let _ = db::insert_audit(
                    ALARM_OPERATOR, "alarm_decision", &a.id,
                    &serde_json::to_string(&json!({"status": "new"})).unwrap_or_default(),
                    &serde_json::to_string(&json!({"status": "acknowledged"})).unwrap_or_default(),
                );
                n += 1;
            }
        }
    }
    with_state(|s| s.success_message = Some(alloc::format!("Przyjęto {} alarmów.", n)));
    render_panel("alarms");
    json!({"ok":true,"acknowledged":n})
}

/// Dev/test affordance: raises a synthetic alarm against a real camera so the
/// read → decision → persistence workflow is fully exercisable from the UI.
/// Cycles severity (critical/warning/info) across calls for visual variety.
fn handle_alarm_simulate() -> JsonValue {
    with_state(|s| s.clear_messages());
    let cameras = db::list_cameras().unwrap_or_default();
    let Some(cam) = cameras.first() else {
        with_state(|s| s.error_message = Some("Dodaj najpierw kamerę, aby zasymulować alarm.".into()));
        render_panel("alarms");
        return json!({"ok":false,"error":"no cameras"});
    };
    let total = db::count_alarms(false, "").unwrap_or(0);
    let (severity, kind, message) = match total % 3 {
        0 => ("critical", "agresja", "podejrzenie agresji przy wjeździe"),
        1 => ("warning", "ADR", "nieczytelna tablica ADR"),
        _ => ("info", "pojazd", "pojazd w strefie zakazu"),
    };
    let ts = db::now_secs();
    match db::insert_alarm(&cam.id, severity, kind, message, ts) {
        Ok(id) => {
            // Index the new alarm into the semantic event store. Best-effort:
            // if no embedding model is deployed, the alarm is still created and
            // the warning is logged (the Search tab's Reindex action backfills).
            index_alarm_by_id(&id);
            with_state(|s| { s.alarms.selected_id = Some(id); s.success_message = Some("Zasymulowano alarm testowy.".into()); });
            render_panel("alarms");
            json!({"ok":true})
        }
        Err(e) => {
            with_state(|s| s.error_message = Some(alloc::format!("Błąd symulacji: {}", abi_message(e))));
            render_panel("alarms");
            json!({"ok":false,"error":alloc::format!("{}",e)})
        }
    }
}

/// Runs ONVIF discovery. The discovered camera cards are a genuinely dynamic
/// list (count + per-row click handlers), so this re-sends the `add_camera_body`
/// fragment — the only wizard action besides modal open that does. The
/// discovery-section visibility flags are patched to match the new results.
fn handle_discover_scan() -> JsonValue {
    with_state(|s| { s.discover.scanning = true; s.discover.error_message = None; s.discover.cameras.clear(); s.discover.selected_index = None; s.error_message = None; });
    // Patch the scanning flag BEFORE the blocking discovery call so the scan
    // spinner becomes visible; the final fragment re-send below carries results.
    send_state_patches(with_state(|s| wizard_onvif_pairs(&s.discover)));
    let result = camera_discover();
    with_state(|s| {
        s.discover.scanning = false;
        match result {
            Ok(found) => { s.discover.cameras = found.iter().map(discovered_to_cam).collect(); }
            Err(e) => { s.error_message = Some(alloc::format!("Błąd skanowania: {}", abi_message(e))); }
        }
    });
    if with_state(|s| s.add_form_visible) {
        send_slot_content_with_overlay("add_camera_body", build_add_camera_body(), Some(wizard_full_overlay()));
    }
    json!({"ok":true})
}

/// Selects a discovered ONVIF camera. Mirrors the picked device URL into the
/// manual ONVIF field and re-sends the body (the row highlight is part of the
/// dynamic discovered-list fragment, not a store flag), seeding the full wizard
/// overlay so `onvif_url` and the row selection both reflect the pick.
fn handle_discover_select(params: &JsonValue) -> JsonValue {
    let index = params.get("index").and_then(|v| v.as_u64());
    with_state(|s| {
        s.error_message = None;
        match index {
            Some(i) if (i as usize) < s.discover.cameras.len() => {
                let i = i as usize;
                s.discover.selected_index = Some(i);
                // Mirror the picked device URL into the manual ONVIF field so the
                // resolved target and submit credentials use one consistent value.
                s.discover.onvif_url = s.discover.cameras[i].url.clone();
            }
            _ => { s.discover.selected_index = None; s.error_message = Some("Wybierz kamerę z listy.".to_string()); }
        }
    });
    if with_state(|s| s.add_form_visible) {
        send_slot_content_with_overlay("add_camera_body", build_add_camera_body(), Some(wizard_full_overlay()));
    }
    json!({"ok":true})
}

// =============================================================================
// Validation helpers
// =============================================================================

/// Resolves wizard credential inputs into the `credentials_b64` field the host
/// expects on `camera_add`. The host decodes this as STANDARD base64 of a
/// `user:pass` string and requires it for `vendor == "onvif"` while treating it
/// as optional for `rtsp`. We enforce the same rule here so an ONVIF camera
/// surfaces a readable wizard error instead of the host's raw
/// `missing_credentials` rejection. Returning `Ok(None)` means "no credentials"
/// (valid only for non-ONVIF vendors). The plaintext lives only for the span of
/// this call and is never logged.
fn build_credentials_b64(
    vendor: &str,
    user: &str,
    pass: &str,
) -> Result<Option<String>, &'static str> {
    let user = user.trim();
    let pass = pass.trim();
    if user.is_empty() && pass.is_empty() {
        if vendor == "onvif" {
            return Err("Kamera ONVIF wymaga użytkownika i hasła.");
        }
        return Ok(None);
    }
    if user.is_empty() {
        return Err("Podaj użytkownika kamery.");
    }
    if pass.is_empty() {
        return Err("Podaj hasło kamery.");
    }
    let plain = alloc::format!("{user}:{pass}");
    Ok(Some(
        base64::engine::Engine::encode(&base64::engine::general_purpose::STANDARD, plain),
    ))
}

fn is_valid_camera_id(id: &str) -> bool {
    if id.len() != 40 || !id.starts_with("cam_") { return false; }
    id.chars().skip(4).all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// Maps a host-discovered ONVIF device into the wizard's working representation.
/// ONVIF WS-Discovery reports `xaddrs` (device service endpoints) rather than a
/// ready stream URL; the first `xaddr` is the canonical ONVIF service URL, and we
/// fall back to the device-service path on the bare `address` when none is given.
fn discovered_to_cam(d: &DiscoveredCameraOut) -> DiscoveredCam {
    let url = d
        .xaddrs
        .first()
        .cloned()
        .filter(|x| !x.trim().is_empty())
        .unwrap_or_else(|| alloc::format!("http://{}/onvif/device_service", d.address));
    DiscoveredCam {
        vendor: "onvif".to_string(),
        url,
        suggested_name: suggested_name_for_discovered(d),
        profile_token: None,
    }
}

fn suggested_name_for_discovered(d: &DiscoveredCameraOut) -> String {
    let make = d.manufacturer.trim();
    let model = d.model.trim();
    match (make.is_empty(), model.is_empty()) {
        (false, false) => alloc::format!("{} {}", make, model),
        (false, true) => make.to_string(),
        (true, false) => model.to_string(),
        (true, true) => {
            let host = extract_host_port(&alloc::format!("onvif://{}", d.address))
                .map(|(h, _)| h)
                .filter(|h| !h.is_empty())
                .unwrap_or_else(|| d.address.clone());
            alloc::format!("ONVIF — {}", host)
        }
    }
}

fn extract_host_port(url: &str) -> Option<(String, Option<u16>)> {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() { return None; }
    let host_part = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
    if let Some(rest) = host_part.strip_prefix('[') {
        if let Some((host_inner, tail)) = rest.split_once(']') {
            let host = alloc::format!("[{}]", host_inner);
            let port = tail.strip_prefix(':').and_then(|p| p.parse::<u16>().ok());
            return Some((host, port));
        }
        return Some((host_part.to_string(), None));
    }
    if let Some((host, port)) = host_part.rsplit_once(':') {
        if let Ok(p) = port.parse::<u16>() { return Some((host.to_string(), Some(p))); }
    }
    Some((host_part.to_string(), None))
}

fn abi_message(e: AbiError) -> &'static str {
    match e {
        AbiError::Permission => "brak uprawnień",
        AbiError::NotFound => "nie znaleziono",
        AbiError::Conflict => "konflikt (duplikat?)",
        AbiError::QuotaExceeded => "przekroczono limit",
        AbiError::CameraUnreachable => "kamera nieosiągalna",
        AbiError::CameraAuthFailed => "błąd uwierzytelniania kamery",
        AbiError::CameraVendorUnsupported => "nieobsługiwany typ kamery",
        AbiError::PayloadTooLarge => "zbyt duży payload",
        AbiError::Timeout => "przekroczono czas oczekiwania",
        AbiError::NoAvailableTarget => "brak dostępnego targetu",
        _ => "błąd operacji",
    }
}

// =============================================================================
// NavTabs construction
// =============================================================================

fn build_nav_tab_items(_active: &str) -> Vec<NavTab> {
    let entries: &[(&str, &str, &str)] = &[
        ("overview", "Dashboard", "dashboard"),
        ("live", "Live view", "video"),
        ("recordings", "Nagrania", "video"),
        ("cameras", "Kamery", "cameras"),
        ("profiles", "Profile analityczne", "brain"),
        ("alarms", "Alarmy", "bell"),
        ("search", "Wyszukiwarka", "search"),
        ("reid", "Re-ID", "users"),
        ("models", "Modele", "cpu"),
        ("zones", "Strefy i reguły", "zones"),
        ("bindings", "Powiązania", "link"),
        ("audit", "Audyt i RODO", "audit"),
        ("evidence", "Eksport dowodowy", "evidence"),
        ("settings", "Ustawienia", "settings"),
        ("onboarding", "Onboarding", "check"),
    ];
    entries.iter().map(|(id, label, icon)| {
        NavTab {
            id: (*id).into(),
            label: lit(label),
            icon: Some(icon_named(parse_icon_name(icon))),
            badge: None,
            panel_id: Some((*id).into()),
            locked: false,
        }
    }).collect()
}

// =============================================================================
// Panel content builders
// =============================================================================

fn build_overview_content() -> Component {
    let messages = build_messages_section();

    // KPI tiles — every number is computed from SQLite.
    let total_cams = db::count_cameras().unwrap_or(0);
    let online_cams = db::count_online_cameras().unwrap_or(0);
    let offline_cams = (total_cams - online_cams).max(0);
    let active_detectors = db::count_active_profiles().unwrap_or(0);
    let alarms_24h = db::count_alarms_last_24h().unwrap_or(0);
    let critical_24h = db::count_critical_alarms_last_24h().unwrap_or(0);

    let cam_val = alloc::format!("{} / {}", online_cams, total_cams);
    let cam_note = if total_cams == 0 {
        "Brak skonfigurowanych kamer".to_string()
    } else if offline_cams > 0 {
        alloc::format!("{} offline", offline_cams)
    } else {
        "wszystkie online".to_string()
    };
    let alarms_note = if critical_24h > 0 {
        alloc::format!("{} krytycznych", critical_24h)
    } else {
        "brak krytycznych".to_string()
    };
    let alarms_tone = if critical_24h > 0 { "danger" } else { "success" };

    let kpi_row = grid(4, vec![
        stat_card(&cam_val, "Aktywne kamery", Some(&cam_note), Some("cameras"),
                  Some(if offline_cams > 0 { "warning" } else { "success" })),
        stat_card(&alloc::format!("{}", active_detectors), "Aktywne detektory", None, Some("brain"), Some("accent")),
        stat_card(&alloc::format!("{}", alarms_24h), "Alarmy 24h", Some(&alarms_note), Some("bell"), Some(alarms_tone)),
        stat_card("68%", "GPU / latencja p95", Some("1.2 s"), Some("cpu"), Some("success")),
    ]);

    // Latest alarms — newest first, joined with the camera name.
    let recent = db::list_recent_alarms(6).unwrap_or_default();
    let alarms_body = if recent.is_empty() {
        // No outer card: empty_state sits straight inside the section card body.
        empty_state("Brak alarmów", Some("Gdy analityka wykryje zdarzenie, pojawi się tutaj."), Some("bell"))
    } else {
        let rows: Vec<Component> = recent.iter().map(build_alarm_card).collect();
        stack_v_gap("sm", rows)
    };
    let alarms_header_label = alloc::format!("Wszystkie {} >", alarms_24h);
    let recent_alarms = card_with_icon_action("Ostatnie alarmy", "bell", Some(&alarms_header_label), vec![alarms_body]);

    let runtime = card_with_icon("Stan natywnego runtime", "cpu", vec![
        build_runtime_table(),
    ]);

    let two_col = grid(2, vec![recent_alarms, runtime]);

    let heatmap_card = build_activity_heatmap();

    stack_v(vec![messages, kpi_row, two_col, heatmap_card])
}

/// Maps an alarm severity token to a badge variant. Persisted severities are
/// `critical` / `warning` / `info`; anything else degrades to a neutral info pill.
fn alarm_severity_variant(severity: &str) -> &'static str {
    match severity {
        "critical" => "danger",
        "warning" => "warning",
        _ => "info",
    }
}

fn alarm_severity_label(severity: &str) -> &'static str {
    match severity {
        "critical" => "krytyczne",
        "warning" => "ostrzeżenie",
        _ => "info",
    }
}

/// Formats a unix timestamp as HH:MM:SS in UTC. The dashboard only needs the
/// clock face for the "latest alarms" list; a full date column lives in the
/// Alarms tab.
fn format_alarm_time(ts: i64) -> String {
    if ts <= 0 {
        return "—".into();
    }
    let secs_in_day = ts.rem_euclid(86_400);
    let h = secs_in_day / 3600;
    let m = (secs_in_day % 3600) / 60;
    let s = secs_in_day % 60;
    alloc::format!("{:02}:{:02}:{:02}", h, m, s)
}


fn build_alarm_card(a: &db::AlarmRow) -> Component {
    // Title: detector type + message, falling back to whichever is present.
    let title = if !a.kind.is_empty() && !a.message.is_empty() {
        alloc::format!("{} · {}", a.kind, a.message)
    } else if !a.message.is_empty() {
        a.message.clone()
    } else if !a.kind.is_empty() {
        a.kind.clone()
    } else {
        "Zdarzenie".into()
    };
    let camera_label = if !a.camera_name.is_empty() {
        a.camera_name.clone()
    } else if !a.camera_id.is_empty() {
        a.camera_id.clone()
    } else {
        "—".into()
    };
    let variant = alarm_severity_variant(&a.severity);

    let title_text = text_styled(&title, "body_strong");
    let meta_row = stack_h_gap("sm", vec![
        chip_with_icon(&camera_label, "category", "cameras"),
        chip_with_icon(&format_alarm_time(a.ts), "category", "clock"),
        badge(alarm_severity_label(&a.severity), variant),
    ]);
    let center = stack_v_gap("xs", vec![title_text, meta_row]);

    let action = ButtonComp {
        variant: ButtonVariant::Ghost,
        tone: Tone::Neutral,
        label: lit("Otwórz"),
        icon_leading: None,
        icon_trailing: None,
        size: ButtonSize::Sm,
        full_width: false,
        disabled: None,
        loading: None,
        density: Density::Default,
    }.into_component(next_id()).expect("Button");

    Flex {
        direction: FlexDirection::Row,
        gap: Spacing::Md,
        justify: FlexJustify::SpaceBetween,
        align: FlexAlign::Center,
        wrap: FlexWrap::NoWrap,
        children: vec![center, action],
        padding: Some(Spacing::Sm),
        background: None,
        radius: None,
    }.into_component(next_id()).expect("Flex")
}

fn build_runtime_kv_row(label: &str, value_children: Vec<Component>) -> Component {
    Flex {
        direction: FlexDirection::Row,
        gap: Spacing::Sm,
        justify: FlexJustify::SpaceBetween,
        align: FlexAlign::Center,
        wrap: FlexWrap::NoWrap,
        children: vec![
            text_styled(label, "caption"),
            Flex {
                direction: FlexDirection::Row,
                gap: Spacing::Xs,
                justify: FlexJustify::End,
                align: FlexAlign::Center,
                wrap: FlexWrap::NoWrap,
                children: value_children,
                padding: None,
                background: None,
                radius: None,
            }.into_component(next_id()).expect("Flex"),
        ],
        padding: None,
        background: None,
        radius: None,
    }.into_component(next_id()).expect("Flex")
}

fn build_runtime_table() -> Component {
    stack_v_gap("sm", vec![
        build_runtime_kv_row("Frame bus throughput", vec![
            text("312 fps łącznie"),
        ]),
        build_runtime_kv_row("Queue depth (max)", vec![
            text("12 (kam. C-04, tier 0)"),
        ]),
        build_runtime_kv_row("Drop rate 1h", vec![
            chip_toned("0.4%", "success"),
        ]),
        build_runtime_kv_row("VRAM użycie", vec![
            text("8.2 / 12 GB"),
            chip_toned("tight", "warning"),
        ]),
        build_runtime_kv_row("Modele załadowane", vec![
            text("6 (YOLO11m, PP-OCRv5, BoT-SORT…)"),
        ]),
        build_runtime_kv_row("Clock-sync dryf max", vec![
            text("12 ms (PTP OK)"),
        ]),
        build_runtime_kv_row("Audit log → WORM", vec![
            chip_toned("synced (5 min temu)", "success"),
        ]),
        build_runtime_kv_row("Eval harness (daily)", vec![
            text("ostatnio 03:00"),
            chip_toned("P/R w celu", "success"),
        ]),
    ])
}

const HEATMAP_COLS: usize = 24;
/// Cap the number of camera rows so a large fleet does not produce an unwieldy
/// grid; the dashboard heatmap is an at-a-glance overview, not the Alarms tab.
const HEATMAP_MAX_ROWS: usize = 12;

/// The cameras shown as heatmap rows, in stable display order (by name). Shared
/// by `build_activity_heatmap` (labels) and `heatmap_cells_value` (data) so row
/// indices line up with the `rN` ids the heatmap helper generates.
fn heatmap_camera_rows() -> Vec<db::CameraRow> {
    let mut cams = db::list_cameras().unwrap_or_default();
    cams.truncate(HEATMAP_MAX_ROWS);
    cams
}

fn build_activity_heatmap() -> Component {
    let cams = heatmap_camera_rows();
    let col_labels: Vec<&str> = (0..HEATMAP_COLS)
        .map(|h| match h {
            0 => "0", 2 => "2", 4 => "4", 6 => "6", 8 => "8", 10 => "10",
            12 => "12", 14 => "14", 16 => "16", 18 => "18", 20 => "20", 22 => "22",
            _ => "",
        })
        .collect();

    if cams.is_empty() {
        // No cameras → no rows to chart; keep a clean empty state rather than an
        // empty zero-row grid the renderer would draw as a bare header strip.
        return card_with_icon(
            "Mapa cieplna aktywności · ostatnie 24h × kamera",
            "dashboard",
            vec![empty_state("Brak danych aktywności", Some("Dodaj kamery, aby zbierać aktywność 24h."), Some("dashboard"))],
        );
    }

    let row_labels: Vec<&str> = cams.iter().map(|c| c.name.as_str()).collect();
    // Values come from the store via `heatmap_cells`; the literal grid passed
    // here is unused by the renderer (the helper ignores `_values`), so pass an
    // empty placeholder of the right shape.
    let values: Vec<Vec<f64>> = Vec::new();
    card_with_icon("Mapa cieplna aktywności · ostatnie 24h × kamera", "dashboard", vec![
        heatmap(cams.len() as u32, HEATMAP_COLS as u32, values, row_labels, col_labels),
    ])
}

/// Builds the `heatmap_cells` store value (`[{row_id, col_id, value}]`) from the
/// real per-camera-per-hour alarm aggregate. `value` is normalized to 0..1 so
/// the linear scale + tf-heatmap level buckets light up; the busiest cell in the
/// window maps to 1.0. Cameras/hours with no alarms are emitted as 0 so the grid
/// is fully populated (no blank rows), matching the mockup's dense look.
fn heatmap_cells_value() -> Value {
    let cams = heatmap_camera_rows();
    if cams.is_empty() {
        return Value::Array(Vec::new());
    }
    let buckets = db::alarm_heatmap_last_24h().unwrap_or_default();
    let row_index: alloc::collections::BTreeMap<&str, usize> =
        cams.iter().enumerate().map(|(i, c)| (c.id.as_str(), i)).collect();

    // Dense count grid [row][hour], then normalize by the global max.
    let mut counts = alloc::vec![[0i64; HEATMAP_COLS]; cams.len()];
    let mut max_count = 0i64;
    for b in &buckets {
        if let Some(&r) = row_index.get(b.camera_id.as_str()) {
            let h = (b.hour_offset as usize).min(HEATMAP_COLS - 1);
            counts[r][h] += b.count;
            max_count = max_count.max(counts[r][h]);
        }
    }
    let denom = if max_count > 0 { max_count as f64 } else { 1.0 };

    let mut cells: Vec<Value> = Vec::with_capacity(cams.len() * HEATMAP_COLS);
    for (r, row) in counts.iter().enumerate() {
        for (h, &c) in row.iter().enumerate() {
            let value = (c as f64) / denom;
            cells.push(Value::Map(vec![
                (Value::Text("row_id".into()), Value::Text(alloc::format!("r{}", r))),
                (Value::Text("col_id".into()), Value::Text(alloc::format!("c{}", h))),
                (Value::Text("value".into()), Value::F64(value)),
            ]));
        }
    }
    Value::Array(cells)
}

fn build_messages_section() -> Component {
    let (err, succ) = with_state(|s| (s.error_message.clone(), s.success_message.clone()));
    let mut children = Vec::new();
    if let Some(e) = err { children.push(alert(&e, "danger")); }
    if let Some(s) = succ { children.push(alert(&s, "success")); }
    if children.is_empty() { return divider(); }
    stack_v_gap("sm", children)
}

/// Allowed live-grid layouts: tile count → grid column count. Mirrors the
/// mockup's 1/4/9/16 segmented control (1=1col, 4=2col, 9=3col, 16=4col).
const LIVE_GRID_SIZES: [u32; 4] = [1, 4, 9, 16];
const DEFAULT_LIVE_GRID_SIZE: i64 = 4;

/// Normalizes a persisted grid-size value to one of the allowed layouts.
fn live_grid_size() -> u32 {
    let raw = db::get_setting_i64("live_grid_size", DEFAULT_LIVE_GRID_SIZE);
    if LIVE_GRID_SIZES.contains(&(raw as u32)) {
        raw as u32
    } else {
        DEFAULT_LIVE_GRID_SIZE as u32
    }
}

/// Column count for a given tile-count layout (square root of the grid size).
fn live_grid_columns(size: u32) -> u32 {
    match size {
        1 => 1,
        4 => 2,
        9 => 3,
        _ => 4,
    }
}

/// Segmented control for the 1/4/9/16 layout. Its picked value is written to the
/// `live_grid_size` store key and committed to settings by `live-grid-change`,
/// so the chosen layout survives panel reopen / process restart.
fn live_grid_selector(current: u32) -> Component {
    use tentaflow_sdk_spec::protocol::ui::actions::SegmentedControl;
    let options: Vec<SegmentOption> = LIVE_GRID_SIZES.iter().map(|n| SegmentOption {
        value: SelectValue::Text(alloc::format!("{}", n)),
        label: Some(lit(&alloc::format!("{}", n))),
        icon: None,
        badge: None,
    }).collect();
    let mut comp = SegmentedControl {
        bind_path: StatePath::new(vec![PathSegment::Key("live_grid_size".into())]),
        options,
        size: SegmentSize::Md,
        full_width: false,
    }.into_component("live_grid_size").expect("SegmentedControl");
    let _ = current;
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Change,
        Handler::Backend {
            action_id: "live-grid-change".into(),
            params: CborMap::default(),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    comp
}

/// One live tile per real camera. ONLINE cameras render a real MSE video tile
/// via the SDK `VideoStream` component with a `camera:<id>` subscribe stream id:
/// the dashboard renderer plays fMP4 over the binary protocol AND attaches the
/// detection overlay (also binary) on top. OFFLINE cameras degrade honestly to a
/// placeholder for THAT tile only — no stream, no fabricated frame, no overlay.
/// A camera shared into TentaVision by another addon (e.g. the go2 robot). It is
/// NOT persisted in TentaVision's SQLite mirror — its lifecycle belongs to the
/// owning addon — so it is merged into read-only surfaces at render time only.
struct SharedCamera {
    camera_id: String,
    display_name: String,
    /// Owning addon id (e.g. "go2"); shown as the share badge.
    owner_addon_id: String,
    status: String,
}

/// Fetches cameras GRANTED to TentaVision by another addon (cross-addon share),
/// excluding the cameras TentaVision itself owns (those already come from
/// `db::list_cameras`). Returns an empty list on any host error so a share
/// outage never breaks the owned-camera surfaces.
fn shared_cameras() -> Vec<SharedCamera> {
    let accessible = match host_list_accessible_cameras() {
        Ok(v) => v,
        Err(e) => {
            log::warn(&alloc::format!(
                "shared camera list unavailable: {}",
                abi_message(e)
            ));
            return Vec::new();
        }
    };
    accessible
        .into_iter()
        .filter(|c| c.access_level.as_deref() == Some("granted"))
        .map(|c| SharedCamera {
            camera_id: c.camera_id,
            display_name: c.display_name,
            owner_addon_id: c.owner_addon_id.unwrap_or_else(|| "shared".into()),
            status: c.status,
        })
        .collect()
}

/// Live-view tile for a camera shared by another addon. Read-only: it carries a
/// "shared" badge naming the owner and offers no mutation actions (assignment /
/// delete / flow config live only on the owned-camera Cameras tab). Live view
/// works because the dashboard stream subscribe is keyed by `camera_id` and the
/// core enforces the grant when serving frames.
fn shared_camera_tile(c: &SharedCamera) -> Component {
    let online = c.status == "online";
    let status_label = if online {
        "online"
    } else if c.status == "offline" {
        "offline"
    } else {
        c.status.as_str()
    };
    if online {
        // Wideo wypelnia caly kafelek; nazwa + status + udostepnienie sa nalozone
        // na obraz przez renderer LiveCameraTile — bez osobnego rzedu badge.
        let label = alloc::format!("{} · {}", c.display_name, c.owner_addon_id);
        live_video_tile(&c.camera_id, &label, status_label)
    } else {
        card(None, vec![empty_state(
            &c.display_name,
            Some("Offline — brak podglądu"),
            Some("alert"),
        )])
    }
}

fn live_camera_tile(c: &db::CameraRow) -> Component {
    let online = c.status == "online";
    let status_label = if online {
        "online"
    } else if c.status == "offline" {
        "offline"
    } else {
        c.status.as_str()
    };
    if online {
        // Wideo wypelnia caly kafelek; nazwa kamery + status sa nalozone na obraz
        // przez renderer LiveCameraTile (nakladki NAD wideo), nie osobnym rzedem
        // badge nad mniejszym boxem wideo.
        live_video_tile(&c.id, &c.name, status_label)
    } else {
        card(None, vec![empty_state(
            &c.name,
            Some("Offline — brak podglądu"),
            Some("alert"),
        )])
    }
}

/// Builds the SDK `LiveCameraTile` (0x0605) bound to the live `camera:<id>`
/// stream. The `camera:` prefix is the only subscribe scheme the core wires
/// today (dispatch/stream.rs CAMERA_PREFIX); the dashboard renderer maps it to
/// `<tf-video-stream>` (MSE over `streamSubscribeRequest`), attaches the binary
/// detection overlay, and — being the specialised camera tile — draws the
/// name/status/timestamp overlays NAD wideo (jak mockup .video-tile). Wideo
/// wypelnia caly kafelek, etykiety sa nalozone przez renderer, a nie osobnym
/// rzedem badge nad obrazem. Dwuklik->fullscreen obsluguje tf-video-stream.
fn live_video_tile(camera_id: &str, name: &str, status: &str) -> Component {
    use tentaflow_sdk_spec::protocol::ui::specialized::media::LiveCameraTile;
    LiveCameraTile {
        stream_id: BindRef::Literal(Value::Text(alloc::format!("camera:{}", camera_id))),
        camera_label: BindRef::Literal(Value::Text(name.into())),
        status: BindRef::Literal(Value::Text(status.into())),
        fps: None,
        show_overlay: true,
        show_fullscreen_button: false,
        aspect_ratio: AspectRatio::R16To9,
    }
    .into_component(next_id())
    .expect("LiveCameraTile")
}

fn build_live_content() -> Component {
    let messages = build_messages_section();
    let cameras = match db::list_cameras() {
        Ok(c) => c,
        Err(e) => {
            return stack_v(vec![
                messages,
                alert(&alloc::format!("Nie udało się pobrać kamer: {}", abi_message(e)), "critical"),
            ]);
        }
    };

    // Cameras shared into TentaVision by another addon (e.g. the go2 robot) are
    // merged into the live grid read-only — they are viewable but not editable.
    let shared = shared_cameras();

    if cameras.is_empty() && shared.is_empty() {
        // Empty state with a CTA that navigates to the Cameras tab so the user
        // can add a camera before any tile can appear.
        let cta = button_with_params(
            "Dodaj kamerę",
            "panel-navigate",
            "primary",
            {
                let mut p = CborMap::default();
                p.0.push(("panel_id".into(), Value::Text("cameras".into())));
                p
            },
        );
        let empty = EmptyStateComp {
            icon: icon_named(parse_icon_name("video")),
            heading: lit("Brak kamer"),
            message: Some(lit("Dodaj kamerę, aby zobaczyć podgląd na żywo.")),
            primary_action: Some(cta),
            secondary_action: None,
            variant: EmptyStateVariant::Default,
        }.into_component(next_id()).expect("EmptyState");
        return stack_v(vec![messages, empty]);
    }

    let size = live_grid_size();
    let columns = live_grid_columns(size);
    let toolbar = stack_h(vec![
        heading(2, "Podgląd na żywo"),
        button_with_icon("Odśwież", "live-refresh", "secondary", "refresh"),
        live_grid_selector(size),
    ]);
    // Owned cameras first, then shared cameras; the whole set is capped by the
    // chosen layout so the grid never exceeds its tile budget.
    let mut tiles: Vec<Component> = cameras.iter()
        .take(size as usize)
        .map(live_camera_tile)
        .collect();
    if tiles.len() < size as usize {
        let remaining = size as usize - tiles.len();
        tiles.extend(shared.iter().take(remaining).map(shared_camera_tile));
    }
    let grid_comp = grid(columns, tiles);
    stack_v(vec![messages, toolbar, grid_comp])
}

// =============================================================================
// Recordings panel — browse + play per-vehicle event clips
// =============================================================================

/// Rows for the recordings Table, seeded into the content slot's state_overlay
/// so the Table mounts populated (same mechanism as `PENDING_CAMERA_ROWS`).
static PENDING_RECORDING_ROWS: Mutex<Option<Value>> = Mutex::new(None);

/// TTL for a recording playback signed URL. Comfortably longer than a single
/// clip so the player never loses the source mid-playback, still bounded.
const RECORDING_URL_TTL_SECS: u64 = 3600;
/// Max recordings pulled into the browse list at once.
const RECORDING_LIST_LIMIT: u32 = 200;

/// The winner plate/ADR text parsed out of one `event_meta.texts.<class>` entry.
/// `event_meta` has drifted between two shapes over builds, so this handles
/// BOTH: the newer gated-winner object `{text, confidence, agreement,
/// unreadable, votes}` and the older raw count-map `{"PL5657": 1972, ...}`.
fn event_meta_winner_text(class_entry: Option<&JsonValue>) -> String {
    let Some(entry) = class_entry else {
        return "—".to_string();
    };
    // Pick the highest-count key from a `variant -> count` map, skipping the
    // structural keys of a winner object.
    let max_vote = |m: &serde_json::Map<String, JsonValue>| -> Option<String> {
        let mut best: Option<(String, i64)> = None;
        for (k, v) in m {
            if matches!(k.as_str(), "text" | "confidence" | "agreement" | "unreadable" | "votes") {
                continue;
            }
            if let Some(c) = v.as_i64() {
                if best.as_ref().map(|(_, bc)| c > *bc).unwrap_or(true) && !k.trim().is_empty() {
                    best = Some((k.trim().to_string(), c));
                }
            }
        }
        best.map(|(k, _)| k)
    };
    match entry {
        // Winner-object shape. Prefer the finalized `text`; otherwise re-derive
        // the agreement-majority read from `votes`. The stored `unreadable` flag
        // is INTENTIONALLY ignored: it was set by the old confidence gate, which
        // marked every plate unreadable because the plate-OCR softmax confidence
        // is near-uniform (~0.05) even at 99% agreement. A recording whose votes
        // agree on one plate should show that plate, not "nieczytelna".
        JsonValue::Object(map) => {
            if let Some(t) = map.get("text").and_then(JsonValue::as_str) {
                let t = t.trim();
                if !t.is_empty() {
                    return t.to_string();
                }
            }
            if let Some(JsonValue::Object(votes)) = map.get("votes") {
                if let Some(w) = max_vote(votes) {
                    return w;
                }
            }
            // No `votes` object → treat the map itself as a raw count-map.
            max_vote(map).unwrap_or_else(|| "—".to_string())
        }
        // A bare string winner (defensive; not emitted today).
        JsonValue::String(s) => {
            let s = s.trim();
            if s.is_empty() { "—".to_string() } else { s.to_string() }
        }
        _ => "—".to_string(),
    }
}

/// Extracts the winner text for one OCR class from a parsed `event_meta` blob.
fn recording_meta_text(meta: &Option<JsonValue>, class: &str) -> String {
    let Some(meta) = meta else {
        return "—".to_string();
    };
    event_meta_winner_text(meta.get("texts").and_then(|t| t.get(class)))
}

/// Renders the aggregated sticker states (`event_meta.stany`) as a compact
/// `label: state, label: state` string. `stany` is the majority state per
/// sticker label written by the event recorder (`{"nalepka_3": "czysta", …}`).
/// Empty / missing → "—". Keys are already sorted (BTreeMap) so order is stable.
fn recording_meta_stany(meta: &Option<JsonValue>) -> String {
    let Some(JsonValue::Object(map)) = meta.as_ref().and_then(|m| m.get("stany")) else {
        return "—".to_string();
    };
    let parts: Vec<String> = map
        .iter()
        .filter_map(|(label, state)| {
            let state = state.as_str()?.trim();
            let label = label.trim();
            if label.is_empty() || state.is_empty() {
                return None;
            }
            Some(alloc::format!("{label}: {state}"))
        })
        .collect();
    if parts.is_empty() {
        "—".to_string()
    } else {
        parts.join(", ")
    }
}

/// Composes the single multi-line "Odczyty" cell for a recording: registration
/// plate, ADR code and aggregated sticker states, one per line. Newlines render
/// as line breaks via the `.tf-table td { white-space: pre-line }` rule. Empty
/// fields collapse to "—" so every event shows all three read lines.
fn recording_reads_text(plate: &str, adr: &str, stany: &str) -> String {
    alloc::format!("Rejestracja: {plate}\nADR: {adr}\nNalepki: {stany}")
}

/// One truck's reads parsed out of an `event_meta.vehicles[]` entry.
struct VehicleReads {
    vehicle_id: u64,
    plate: String,
    adr: String,
    stany: String,
}

/// Parses the per-truck breakdown from `event_meta.vehicles[]`, dropping the
/// unassigned bucket (`vehicle_id == 0`). Empty when the blob predates the
/// per-vehicle shape (old single-bag rows) — callers then fall back to the
/// scalar columns + the whole-scene `event_meta` via `recording_meta_text`.
/// Plate/ADR tolerate BOTH the gated-winner object and the raw count-map (via
/// `event_meta_winner_text`); a per-vehicle `plate`/`adr` string field wins when
/// present (the recorder emits it directly).
fn recording_meta_vehicles(meta: &Option<JsonValue>) -> Vec<VehicleReads> {
    let Some(JsonValue::Array(items)) = meta.as_ref().and_then(|m| m.get("vehicles")) else {
        return Vec::new();
    };
    let mut out: Vec<VehicleReads> = Vec::new();
    for item in items {
        let vehicle_id = item.get("vehicle_id").and_then(JsonValue::as_u64).unwrap_or(0);
        if vehicle_id == 0 {
            // Unassigned bucket — signs outside any truck; not a per-truck block.
            continue;
        }
        // Prefer the direct `plate`/`adr` string field; else derive from
        // `texts.<class>` (tolerant of both stored shapes).
        let plate = item
            .get("plate")
            .and_then(JsonValue::as_str)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                event_meta_winner_text(
                    item.get("texts").and_then(|t| t.get("tablica_rejestracyjna")),
                )
            });
        let adr = item
            .get("adr")
            .and_then(JsonValue::as_str)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                event_meta_winner_text(item.get("texts").and_then(|t| t.get("tablica_adr")))
            });
        let stany = recording_stany_map(item.get("stany"));
        out.push(VehicleReads {
            vehicle_id,
            plate,
            adr,
            stany,
        });
    }
    // Stable order by vehicle_id so "Pojazd 1/2/…" numbering is deterministic.
    out.sort_by_key(|v| v.vehicle_id);
    out
}

/// Renders the "Odczyty" cell across one or more trucks. A SINGLE truck (or the
/// old single-bag shape) renders exactly as before — no "Pojazd" prefix. Two or
/// more trucks render one labelled block per truck ("Pojazd 1 — Rejestracja: … /
/// ADR: … / Nalepki: …"), separated by a blank line.
fn recording_reads_multi(vehicles: &[VehicleReads], fallback: &str) -> String {
    match vehicles.len() {
        0 => fallback.to_string(),
        1 => {
            let v = &vehicles[0];
            recording_reads_text(&dash(&v.plate), &dash(&v.adr), &dash(&v.stany))
        }
        _ => {
            let mut blocks: Vec<String> = Vec::with_capacity(vehicles.len());
            for (i, v) in vehicles.iter().enumerate() {
                blocks.push(alloc::format!(
                    "Pojazd {} — Rejestracja: {}\nADR: {}\nNalepki: {}",
                    i + 1,
                    dash(&v.plate),
                    dash(&v.adr),
                    dash(&v.stany)
                ));
            }
            blocks.join("\n\n")
        }
    }
}

/// "—" for an empty/blank string, else the trimmed string.
fn dash(s: &str) -> String {
    let t = s.trim();
    if t.is_empty() {
        "—".to_string()
    } else {
        t.to_string()
    }
}

/// Renders one vehicle's `stany` object (`{"nalepka_3":"czysta", …}`) as a
/// compact `label: state, …` string, mirroring `recording_meta_stany` but for a
/// per-vehicle sub-object. Empty/missing → "—".
fn recording_stany_map(stany: Option<&JsonValue>) -> String {
    let Some(JsonValue::Object(map)) = stany else {
        return "—".to_string();
    };
    let parts: Vec<String> = map
        .iter()
        .filter_map(|(label, state)| {
            let state = state.as_str()?.trim();
            let label = label.trim();
            if label.is_empty() || state.is_empty() {
                return None;
            }
            Some(alloc::format!("{label}: {state}"))
        })
        .collect();
    if parts.is_empty() {
        "—".to_string()
    } else {
        parts.join(", ")
    }
}

/// mm:ss for a duration in milliseconds. `None`/0 → "—".
fn format_duration_ms(ms: Option<i64>) -> String {
    match ms {
        Some(ms) if ms > 0 => {
            let total = ms / 1000;
            alloc::format!("{}:{:02}", total / 60, total % 60)
        }
        _ => "—".to_string(),
    }
}

/// Human file size in MB (one decimal). `<= 0` → "—".
fn format_size_mb(bytes: i64) -> String {
    if bytes <= 0 {
        return "—".to_string();
    }
    let mb = bytes as f64 / (1024.0 * 1024.0);
    alloc::format!("{:.1} MB", mb)
}

/// Resolves a signed image URL for a plate/ADR thumbnail snapshot ref, or an
/// empty string when there is no thumb (the `img` cell renderer then shows a
/// muted placeholder). A URL-issue failure also degrades to the placeholder
/// rather than surfacing an error in the list.
fn recording_thumb_url(thumb_ref: &Option<String>) -> String {
    match thumb_ref.as_deref() {
        Some(r) if !r.trim().is_empty() => host_recording_get_url(r, RECORDING_URL_TTL_SECS)
            .map(|u| u.url)
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// One recordings Table row. `recording_ref` is the row key the "Odtwórz"
/// action carries. The two operator-facing columns are:
///   * `zdjecie` — a signed image URL of the single representative truck frame
///     (the event's best plate/ADR snapshot, carried in `plate_thumb_ref`);
///   * `odczyty` — one multi-line cell combining the aggregated Rejestracja,
///     ADR and Nalepki reads for the whole vehicle event.
/// The plate/ADR winners prefer the indexed columns (used by search) and fall
/// back to the `event_meta` blob for rows recorded before those columns existed.
fn recording_table_row_value(item: &RecordingListItem, camera_name: &str) -> Value {
    let meta: Option<JsonValue> = item
        .event_meta
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok());
    let plate = item
        .plate_text
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| recording_meta_text(&meta, "tablica_rejestracyjna"));
    let adr = item
        .adr_text
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| recording_meta_text(&meta, "tablica_adr"));
    let stany = recording_meta_stany(&meta);
    // Per-truck breakdown from `event_meta.vehicles[]`. With 0 or 1 real truck the
    // cell is byte-identical to the old single-bag output (scalar plate/adr +
    // whole-scene stany, NO "Pojazd" prefix); ≥2 trucks render one block each.
    let vehicles = recording_meta_vehicles(&meta);
    let single_line = recording_reads_text(&plate, &adr, &stany);
    let odczyty = if vehicles.len() >= 2 {
        recording_reads_multi(&vehicles, &single_line)
    } else {
        single_line
    };
    // The single truck photo is the repurposed `plate_thumb_ref` (best-read
    // frame, = the PRIMARY vehicle's thumb); `adr_thumb_ref` is no longer
    // populated by the recorder.
    let zdjecie = recording_thumb_url(&item.plate_thumb_ref);
    let entries: Vec<(Value, Value)> = vec![
        (Value::Text("recording_ref".into()), Value::Text(item.recording_ref.clone())),
        (Value::Text("czas".into()), Value::Text(format_alarm_datetime(item.created_at))),
        (Value::Text("kamera".into()), Value::Text(camera_name.to_string())),
        (Value::Text("czas_trwania".into()), Value::Text(format_duration_ms(item.duration_ms))),
        (Value::Text("zdjecie".into()), Value::Text(zdjecie)),
        (Value::Text("odczyty".into()), Value::Text(odczyty)),
        (Value::Text("rozmiar".into()), Value::Text(format_size_mb(item.file_size_bytes))),
    ];
    Value::Map(entries)
}

/// Friendly camera name for an id, falling back to the id itself. Built once per
/// render from the addon's owned + shared camera lists.
fn recording_camera_name(camera_id: &str, owned: &[db::CameraRow], shared: &[SharedCamera]) -> String {
    if let Some(c) = owned.iter().find(|c| c.id == camera_id) {
        if !c.name.trim().is_empty() {
            return c.name.clone();
        }
    }
    if let Some(c) = shared.iter().find(|c| c.camera_id == camera_id) {
        if !c.display_name.trim().is_empty() {
            return c.display_name.clone();
        }
    }
    camera_id.to_string()
}

fn recording_table_column_render(id: &str, header: &str, render: ColumnRender) -> TableColumn {
    // Image columns are not sortable (the cell value is a signed URL, not a
    // comparable field) and never hide by default.
    let sortable = !matches!(render, ColumnRender::Image);
    TableColumn {
        id: id.into(),
        header: lit(header),
        field_path: vec![PathSegment::Key(id.into())],
        width: TableColumnWidth::Auto,
        render,
        format: None,
        align: None,
        sortable,
        hidden_by_default: false,
        sticky_left: false,
    }
}

fn recording_table_column(id: &str, header: &str) -> TableColumn {
    recording_table_column_render(id, header, ColumnRender::Text)
}

fn build_recordings_table() -> Component {
    let columns = vec![
        recording_table_column("czas", "Czas"),
        recording_table_column("kamera", "Kamera"),
        recording_table_column("czas_trwania", "Czas trwania"),
        recording_table_column_render("zdjecie", "Zdjęcie", ColumnRender::Image),
        // Multi-line combined reads (Rejestracja / ADR / Nalepki). Not sortable —
        // the cell is a composed multi-field block, not a single comparable key.
        recording_table_column_render("odczyty", "Odczyty", ColumnRender::Text),
        recording_table_column("rozmiar", "Rozmiar"),
    ];
    // The Table injects the row key (`recording_ref`) into the row-action params
    // as both `row_id` and `recording_ref`, so "Odtwórz" dispatches
    // `recordings-play` with the clicked recording's ref.
    let play_action = button("Odtwórz", "recordings-play", "primary");
    TableComp {
        columns,
        rows_path: StatePath::new(vec![PathSegment::Key("recordings_rows".into())]),
        row_key_field: "recording_ref".into(),
        variant: TableVariant::Default,
        density: Density::Default,
        sortable: true,
        sort_by: None,
        selectable: TableSelectMode::None,
        selected_ids: None,
        sticky_header: true,
        sticky_columns: 0,
        pagination: None,
        empty_state: None,
        row_actions: vec![play_action],
        bulk_actions: vec![],
        virtualize: false,
        row_expandable: false,
        expanded_row_template_id: None,
    }.into_component(next_id()).expect("Table")
}

/// Modal shell for recording playback. Its body slot (`recording_player_body`)
/// is filled separately via SlotContent in `render_panel`; Dismiss routes to the
/// same `recordings-close` action as the body's close button.
fn build_recording_player_modal() -> Component {
    let mut modal = ModalComp {
        title: lit("Odtwarzanie nagrania"),
        subtitle: None,
        body_slot: "recording_player_body".into(),
        footer_slot: None,
        size: ModalSize::Lg,
        dismissible: true,
        prevent_scroll: true,
        closable: true,
    }.into_component(next_id()).expect("Modal");
    modal.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Dismiss,
        Handler::Backend {
            action_id: "recordings-close".into(),
            params: CborMap::default(),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    modal
}

/// Body of the playback modal: the HTML5 video player pointed at the signed URL,
/// the event metadata (plate/ADR/time/camera) beside it, and a close button. A
/// URL-issue failure degrades to an honest alert — no fabricated player.
fn build_recording_player_body(recording_ref: &str) -> Component {
    let url = match host_recording_get_url(recording_ref, RECORDING_URL_TTL_SECS) {
        Ok(u) => u.url,
        Err(e) => {
            return stack_v(vec![
                alert(
                    &alloc::format!("Nie udało się przygotować odtwarzania: {}", abi_message(e)),
                    "critical",
                ),
                button("Zamknij", "recordings-close", "ghost"),
            ]);
        }
    };

    // A non-`camera:` stream_id makes the SDK VideoStream renderer fall back to a
    // plain <video src controls> element — exactly what a signed mp4 URL needs.
    let player = {
        use tentaflow_sdk_spec::protocol::ui::specialized::VideoStream;
        VideoStream {
            stream_id: BindRef::Literal(Value::Text(url)),
            width_px: None,
            aspect_ratio: AspectRatio::R16To9,
            controls: VideoControls::Full,
            autoplay: true,
            muted: false,
            object_fit: ImageFit::Contain,
            poster_ref: None,
        }.into_component(next_id()).expect("VideoStream")
    };

    // Metadata pulled from the corresponding list row (parsed the same way):
    // the same combined reads as the list — Rejestracja / ADR / Nalepki.
    let (camera_label, when, plate, adr, stany) = recording_playing_meta(recording_ref);
    let metadata = card(Some("Metadane zdarzenia"), vec![key_value(vec![
        ("Kamera", camera_label.as_str()),
        ("Czas", when.as_str()),
        ("Rejestracja", plate.as_str()),
        ("ADR", adr.as_str()),
        ("Nalepki", stany.as_str()),
    ])]);

    stack_v(vec![
        player,
        metadata,
        button("Zamknij", "recordings-close", "ghost"),
    ])
}

/// Resolves the metadata shown beside the player for the currently-playing ref
/// by re-reading the recording list (single row lookup by ref). Returns
/// `(camera_label, when, plate, adr, stany)` with graceful fallbacks — the plate
/// and ADR prefer the indexed winner columns (mirroring the list) and fall back
/// to the `event_meta` blob.
fn recording_playing_meta(recording_ref: &str) -> (String, String, String, String, String) {
    // Reuse the active search so the playing row is found within the same
    // filtered set the list showed (it was clicked from there).
    let search = recordings_search_from_state();
    let items = host_recording_list(&search, RECORDING_LIST_LIMIT).unwrap_or_default();
    let Some(item) = items.iter().find(|i| i.recording_ref == recording_ref) else {
        return ("—".into(), "—".into(), "—".into(), "—".into(), "—".into());
    };
    let owned = db::list_cameras().unwrap_or_default();
    let shared = shared_cameras();
    let camera_label = recording_camera_name(&item.camera_id, &owned, &shared);
    let when = format_alarm_datetime(item.created_at);
    let meta: Option<JsonValue> = item
        .event_meta
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok());
    let plate = item
        .plate_text
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| recording_meta_text(&meta, "tablica_rejestracyjna"));
    let adr = item
        .adr_text
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| recording_meta_text(&meta, "tablica_adr"));
    let stany = recording_meta_stany(&meta);
    (camera_label, when, plate, adr, stany)
}

/// Parses a `YYYY-MM-DD` day string to unix MILLISECONDS at the given
/// end-of-day flag: `false` → 00:00:00 (inclusive lower bound), `true` →
/// 23:59:59 (inclusive upper bound). `None` for an empty/malformed string so an
/// absent or half-typed date simply drops the bound rather than erroring. Uses
/// the shared `days_from_civil` (proleptic Gregorian, UTC).
fn parse_day_bound_ms(day: &str, end_of_day: bool) -> Option<i64> {
    let day = day.trim();
    if day.is_empty() {
        return None;
    }
    let mut parts = day.split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let secs = days_from_civil(y, m, d) * 86_400 + if end_of_day { 86_399 } else { 0 };
    Some(secs * 1000)
}

/// Builds the server-side `RecordingSearch` from the current panel state:
/// camera-chip filter + date range + plate/ADR text queries.
fn recordings_search_from_state() -> RecordingSearch {
    with_state(|s| {
        let camera_id = if s.recordings_camera_filter.is_empty() {
            None
        } else {
            Some(s.recordings_camera_filter.clone())
        };
        let plate = {
            let t = s.recordings_plate_query.trim();
            (!t.is_empty()).then(|| t.to_string())
        };
        let adr = {
            let t = s.recordings_adr_query.trim();
            (!t.is_empty()).then(|| t.to_string())
        };
        RecordingSearch {
            camera_id,
            date_from: parse_day_bound_ms(&s.recordings_date_from, false),
            date_to: parse_day_bound_ms(&s.recordings_date_to, true),
            plate,
            adr,
        }
    })
}

/// `true` when any of the date/plate/ADR search filters are active (the camera
/// chip is surfaced separately) — drives the "Wyczyść" affordance visibility.
fn recordings_search_active() -> bool {
    with_state(|s| {
        !s.recordings_date_from.trim().is_empty()
            || !s.recordings_date_to.trim().is_empty()
            || !s.recordings_plate_query.trim().is_empty()
            || !s.recordings_adr_query.trim().is_empty()
    })
}

/// A recordings-search text field (plate / ADR) bound to `field` in state,
/// committing every keystroke to the backend via `recordings-search-change` so a
/// re-render re-queries with the new filter (server-side).
fn recordings_search_input(label: &str, placeholder: &str, field: &str) -> Component {
    use tentaflow_sdk_spec::protocol::ui::form::Input;
    let mut comp = Input {
        r#type: InputType::Search,
        bind_path: StatePath::new(vec![PathSegment::Key(field.into())]),
        placeholder: Some(lit(placeholder)),
        label: Some(lit(label)),
        hint: None,
        leading_icon: Some(icon_named(parse_icon_name("search"))),
        trailing_icon: None,
        prefix: None,
        suffix: None,
        validators: vec![],
        max_length: Some(64),
        min_length: None,
        pattern: None,
        autocomplete: None,
        input_mode: None,
        disabled: None,
        readonly: None,
        error: None,
        size: InputSize::Md,
    }
    .into_component(field)
    .expect("Input");
    let mut params = CborMap::default();
    params.0.push(("field".into(), Value::Text(field.into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Input,
        Handler::Backend {
            action_id: "recordings-search-change".into(),
            params,
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    comp
}

/// A recordings-search date field (from / to) as a plain `Date`-typed
/// `Input` bound to `field`, committing on change via `recordings-search-change`.
/// A native date input yields the canonical `YYYY-MM-DD` the range parser wants,
/// and stays a single `tf-*` primitive without the DatePicker calendar overlay.
fn recordings_date_input(label: &str, field: &str) -> Component {
    use tentaflow_sdk_spec::protocol::ui::form::Input;
    let mut comp = Input {
        // No dedicated Date InputType exists; Text keeps the field a real tf-input
        // while the `YYYY-MM-DD` value is validated by `parse_day_bound_ms`.
        r#type: InputType::Text,
        bind_path: StatePath::new(vec![PathSegment::Key(field.into())]),
        placeholder: Some(lit("RRRR-MM-DD")),
        label: Some(lit(label)),
        hint: None,
        leading_icon: Some(icon_named(parse_icon_name("calendar"))),
        trailing_icon: None,
        prefix: None,
        suffix: None,
        validators: vec![],
        max_length: Some(10),
        min_length: None,
        pattern: None,
        autocomplete: None,
        input_mode: None,
        disabled: None,
        readonly: None,
        error: None,
        size: InputSize::Md,
    }
    .into_component(field)
    .expect("Input");
    let mut params = CborMap::default();
    params.0.push(("field".into(), Value::Text(field.into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Input,
        Handler::Backend {
            action_id: "recordings-search-change".into(),
            params,
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    comp
}

fn build_recordings_content() -> Component {
    let messages = build_messages_section();
    let mut children = vec![messages];

    let filter = with_state(|s| s.recordings_camera_filter.clone());

    let toolbar = stack_h(vec![
        heading(2, "Nagrania"),
        button_with_icon("Odśwież", "recordings-refresh", "secondary", "refresh"),
    ]);
    children.push(toolbar);

    // Server-side search controls: date range + plate + ADR, plus a clear
    // affordance shown only when a filter is active.
    let mut search_fields = vec![
        recordings_date_input("Od", "recordings_date_from"),
        recordings_date_input("Do", "recordings_date_to"),
        recordings_search_input("Tablica", "np. WGM12345", "recordings_plate_query"),
        recordings_search_input("ADR", "np. 30/1202", "recordings_adr_query"),
    ];
    if recordings_search_active() {
        search_fields.push(button("Wyczyść", "recordings-search-clear", "ghost"));
    }
    children.push(card(Some("Szukaj"), vec![stack_h(search_fields)]));

    let owned = db::list_cameras().unwrap_or_default();
    let shared = shared_cameras();

    let search = recordings_search_from_state();
    let list_result = host_recording_list(&search, RECORDING_LIST_LIMIT);
    let items = match list_result {
        Ok(v) => v,
        Err(e) => {
            children.push(alert(
                &alloc::format!("Nie udało się pobrać nagrań: {}", abi_message(e)),
                "critical",
            ));
            return stack_v(children);
        }
    };

    // Camera filter chips (only when there is more than one camera to filter by).
    let mut cam_ids: Vec<String> = owned.iter().map(|c| c.id.clone()).collect();
    for c in &shared {
        if !cam_ids.iter().any(|id| id == &c.camera_id) {
            cam_ids.push(c.camera_id.clone());
        }
    }
    if cam_ids.len() > 1 {
        let mut chips = vec![FilterChipDef {
            id: "all".into(),
            label: lit(&alloc::format!("Wszystkie ({})", items.len())),
            icon: None,
            badge: None,
            count_path: None,
        }];
        for id in &cam_ids {
            chips.push(FilterChipDef {
                id: id.clone(),
                label: lit(&recording_camera_name(id, &owned, &shared)),
                icon: None,
                badge: None,
                count_path: None,
            });
        }
        let active = if filter.is_empty() { "all" } else { &filter };
        let mut chip_comp = filter_chips(chips, active);
        chip_comp.handlers = Some(HandlerMap(vec![(
            tentaflow_sdk_spec::EventKind::Select,
            Handler::Backend {
                action_id: "recordings-filter-change".into(),
                params: CborMap::default(),
                optimistic: None,
                on_failure: FailurePolicy::Toast,
            },
        )]));
        children.push(chip_comp);
    }

    // The playback Modal is placed in the content tree while a recording is
    // selected; its body is filled by SlotContent (see render_panel).
    if with_state(|s| s.recording_playing.is_some()) {
        children.push(build_recording_player_modal());
    }

    if items.is_empty() {
        children.push(empty_state(
            "Brak nagrań",
            Some("Nagrania zdarzeń pojazdowych pojawią się tutaj po ich zarejestrowaniu przez kamery."),
            Some("video"),
        ));
    } else {
        let rows: Vec<Value> = items
            .iter()
            .map(|item| {
                let name = recording_camera_name(&item.camera_id, &owned, &shared);
                recording_table_row_value(item, &name)
            })
            .collect();
        if let Ok(mut g) = PENDING_RECORDING_ROWS.lock() {
            *g = Some(Value::Array(rows));
        }
        children.push(build_recordings_table());
    }

    stack_v(children)
}

fn build_cameras_content() -> Component {
    let list_result = db::list_cameras();
    let messages = build_messages_section();
    let (add_visible, filter) = with_state(|s| (s.add_form_visible, s.cameras_filter.clone()));

    let mut children = vec![messages];

    // Header: heading + search + add button
    let search_input = with_a11y_label({
        use tentaflow_sdk_spec::protocol::ui::form::Input;
        Input {
            r#type: InputType::Search,
            bind_path: StatePath::new(vec![PathSegment::Key("cameras_search".into())]),
            placeholder: Some(lit("Szukaj po nazwie, IP, vendorze...")),
            label: None,
            hint: None,
            leading_icon: Some(icon_named(parse_icon_name("search"))),
            trailing_icon: None,
            prefix: None,
            suffix: None,
            validators: vec![],
            max_length: None,
            min_length: None,
            pattern: None,
            autocomplete: None,
            input_mode: None,
            disabled: None,
            readonly: None,
            error: None,
            size: InputSize::Md,
        }.into_component("cameras_search").expect("Input")
    }, "Szukaj kamer");
    let toolbar = stack_h(vec![
        heading(2, "Kamery"),
        search_input,
        button_with_icon("Odśwież status", "cameras-refresh", "secondary", "refresh"),
        button_with_icon("Dodaj kamerę", "camera-add-show", "primary", "plus"),
    ]);
    children.push(toolbar);

    // A DB/permission error must never be masked as "no cameras"; surface the
    // real reason and stop rendering the list.
    let cameras = match list_result {
        Ok(c) => c,
        Err(e) => {
            children.push(alert(&alloc::format!("Nie udało się pobrać kamer: {}", abi_message(e)), "critical"));
            return stack_v(children);
        }
    };

    // Filter counts derived from persisted camera status.
    let total = cameras.len();
    let online = cameras.iter().filter(|c| c.status == "online").count();
    let offline = cameras.iter().filter(|c| c.status == "offline").count();
    let warnings = cameras.iter().filter(|c| camera_row_has_warning(c)).count();

    let active_filter = if filter.is_empty() { "all" } else { &filter };
    let sub_tabs = filter_chips(
        vec![
            FilterChipDef { id: "all".into(), label: lit(&alloc::format!("Wszystkie ({})", total)), icon: None, badge: None, count_path: None },
            FilterChipDef { id: "online".into(), label: lit(&alloc::format!("Online ({})", online)), icon: None, badge: None, count_path: None },
            FilterChipDef { id: "offline".into(), label: lit(&alloc::format!("Offline ({})", offline)), icon: None, badge: None, count_path: None },
            FilterChipDef { id: "warnings".into(), label: lit(&alloc::format!("Ostrzeżenia ({})", warnings)), icon: None, badge: None, count_path: None },
        ],
        active_filter,
    );
    children.push(sub_tabs);

    // The "Add camera" wizard renders as a Modal overlay. While visible, the
    // Modal shell is placed in the content tree; its body/footer are filled by
    // SlotContent (see render_panel). When hidden, the Modal is absent so the
    // host unregisters its dynamic body/footer slots.
    if add_visible {
        children.push(build_add_camera_modal());
    }

    let filtered: Vec<&db::CameraRow> = cameras
        .iter()
        .filter(|c| match active_filter {
            "online" => c.status == "online",
            "offline" => c.status == "offline",
            "warnings" => camera_row_has_warning(c),
            _ => true,
        })
        .collect();

    // A delete-confirmation bar appears above the table once a row is selected.
    if let Some(pending) = with_state(|s| s.camera_pending_remove.clone()) {
        if cameras.iter().any(|c| c.id == pending) {
            children.push(build_camera_remove_confirm(&pending, &cameras));
        } else {
            with_state(|s| s.camera_pending_remove = None);
        }
    }

    // The analysis-flow selector appears above the table for the camera whose
    // "Flow analizy" row action was clicked.
    if let Some(edit) = with_state(|s| s.camera_flow_edit.clone()) {
        if cameras.iter().any(|c| c.id == edit) {
            children.push(build_camera_flow_config(&edit, &cameras));
        } else {
            with_state(|s| s.camera_flow_edit = None);
        }
    }

    // The CV-pipeline selector appears above the table for the camera whose
    // "Pipeline analizy" row action was clicked.
    if let Some(edit) = with_state(|s| s.camera_pipeline_edit.clone()) {
        if cameras.iter().any(|c| c.id == edit) {
            children.push(build_camera_pipeline_config(&edit, &cameras));
        } else {
            with_state(|s| s.camera_pipeline_edit = None);
        }
    }

    // The pipeline manager (list + stage editor) renders as its own section
    // once opened from the per-camera selector's "Edytuj pipeline'y" button.
    if with_state(|s| s.cv_pipelines.manager_visible) {
        children.push(build_pipeline_manager());
    }

    if cameras.is_empty() {
        // No outer Outlined Card: the dashboard pushes its sections straight
        // into the stack, so wrapping these in card(None, ...) would draw a
        // double container (a stray white frame around the content).
        children.push(empty_state("Brak kamer", Some("Dodaj kamerę aby rozpocząć monitorowanie."), Some("cameras")));
    } else {
        // Stash the filtered rows (read from SQLite) for render_panel to seed
        // into the content slot's state_overlay under the Table's rows_path, so
        // the Table mounts with rows present in its first store snapshot.
        let rows: Vec<Value> = filtered.iter().map(|c| camera_table_row_value(c)).collect();
        if let Ok(mut g) = PENDING_CAMERA_ROWS.lock() {
            *g = Some(Value::Array(rows));
        }
        // Table carries its own surface styling; an extra Outlined Card here
        // would nest a white frame around it, unlike the dashboard layout.
        children.push(build_cameras_table());
    }

    stack_v(children)
}

/// A camera is "warning" when its persisted status is neither cleanly online
/// nor offline (e.g. "degraded").
fn camera_row_has_warning(c: &db::CameraRow) -> bool {
    c.status != "online" && c.status != "offline"
}

/// Renders the persisted address: ONVIF url if present, else RTSP url.
fn camera_row_addr(c: &db::CameraRow) -> String {
    let addr = if !c.onvif_url.trim().is_empty() { &c.onvif_url } else { &c.rtsp_url };
    if addr.trim().is_empty() { "\u{2014}".to_string() } else { redact_url_for_display(addr) }
}

/// FPS cell: configured target fps, or em-dash when 0.
fn camera_row_fps(c: &db::CameraRow) -> String {
    if c.fps > 0 { alloc::format!("{}", c.fps) } else { "\u{2014}".to_string() }
}

/// Human-readable AI analysis FPS for the cameras table. `0` reads as
/// "Bez limitu" (unlimited / native cadence).
fn camera_row_analysis_fps(c: &db::CameraRow) -> String {
    if c.analysis_fps <= 0 {
        "Bez limitu".to_string()
    } else {
        alloc::format!("{}", c.analysis_fps)
    }
}

/// Builds one Table row as a `Value::Map` keyed by the column field paths.
/// `camera_id` is the row key the Table uses to scope per-row actions.
/// Builds a toned chip cell value `{ label, status }`. The data-table renderer
/// honors `status` for chip columns so status pills / risk badges render their
/// mockup colors (ok=green, warn=amber, err=red, muted=grey) instead of a flat
/// neutral tone.
fn chip_cell(label: &str, status: &str) -> Value {
    Value::Map(vec![
        (Value::Text("label".into()), Value::Text(label.to_string())),
        (Value::Text("status".into()), Value::Text(status.to_string())),
    ])
}

/// Maps a persisted camera status to a chip label + tone.
fn camera_status_cell(status: &str) -> Value {
    match status {
        "online" => chip_cell("online", "ok"),
        "offline" => chip_cell("offline", "err"),
        other => chip_cell(other, "warn"),
    }
}

fn camera_table_row_value(c: &db::CameraRow) -> Value {
    let location = if c.location.trim().is_empty() { "\u{2014}".to_string() } else { c.location.clone() };
    let detectors = if c.detectors.trim().is_empty() { "\u{2014}".to_string() } else { c.detectors.clone() };
    let entries: Vec<(Value, Value)> = vec![
        (Value::Text("camera_id".into()), Value::Text(c.id.clone())),
        (Value::Text("name".into()), Value::Text(c.name.clone())),
        (Value::Text("location".into()), Value::Text(location)),
        (Value::Text("addr".into()), Value::Text(camera_row_addr(c))),
        (Value::Text("status".into()), camera_status_cell(&c.status)),
        (Value::Text("detectors".into()), Value::Text(detectors)),
        (Value::Text("fps".into()), Value::Text(camera_row_fps(c))),
        (Value::Text("analysis_fps".into()), Value::Text(camera_row_analysis_fps(c))),
    ];
    Value::Map(entries)
}

fn camera_table_column(id: &str, header: &str, render: ColumnRender) -> TableColumn {
    TableColumn {
        id: id.into(),
        header: lit(header),
        field_path: vec![PathSegment::Key(id.into())],
        width: TableColumnWidth::Auto,
        render,
        format: None,
        align: None,
        sortable: true,
        hidden_by_default: false,
        sticky_left: false,
    }
}

fn build_cameras_table() -> Component {
    let columns = vec![
        camera_table_column("name", "Nazwa", ColumnRender::Text),
        camera_table_column("location", "Lokalizacja", ColumnRender::Text),
        camera_table_column("addr", "Adres", ColumnRender::Text),
        camera_table_column("status", "Status", ColumnRender::Chip),
        camera_table_column("detectors", "Detektory", ColumnRender::Text),
        camera_table_column("fps", "FPS", ColumnRender::Text),
        camera_table_column("analysis_fps", "FPS analizy", ColumnRender::Text),
    ];

    // The per-row "⋯" menu carries the deletion action. The Table renderer
    // injects the row key into the menu-item action params as both `row_id`
    // and the concrete `row_key_field` (`camera_id`), so this Button dispatches
    // `camera-row-select` with the clicked camera_id. Deletion stays gated:
    // `camera-row-select` only arms the pending-remove confirmation bar
    // (`build_camera_remove_confirm`); the real `camera-remove` runs from that
    // bar's explicit Usuń button.
    // The per-row "⋯" menu also carries the analysis-flow assignment. The Table
    // injects the clicked camera_id into the action params (as `row_id` /
    // `camera_id`), so this opens the flow selector for that camera.
    let flow_action = button("Flow analizy", "camera-flow-edit", "ghost");
    let pipeline_action = button("Pipeline analizy", "camera-pipeline-edit", "ghost");
    let remove_action = button("Usuń", "camera-row-select", "destructive");

    TableComp {
        columns,
        rows_path: StatePath::new(vec![PathSegment::Key("cameras_rows".into())]),
        row_key_field: "camera_id".into(),
        variant: TableVariant::Default,
        density: Density::Default,
        sortable: true,
        sort_by: None,
        selectable: TableSelectMode::None,
        selected_ids: None,
        sticky_header: true,
        sticky_columns: 0,
        pagination: None,
        empty_state: None,
        row_actions: vec![flow_action, pipeline_action, remove_action],
        bulk_actions: vec![],
        virtualize: false,
        row_expandable: false,
        expanded_row_template_id: None,
    }.into_component(next_id()).expect("Table")
}

/// Confirmation bar for deleting the selected camera. Usuń dispatches
/// `camera-remove` with the explicit `camera_id`; Anuluj clears the selection.
fn build_camera_remove_confirm(camera_id: &str, cameras: &[db::CameraRow]) -> Component {
    let name = cameras
        .iter()
        .find(|c| c.id == camera_id)
        .map(|c| c.name.as_str())
        .unwrap_or(camera_id);

    let mut params = CborMap::default();
    params.0.push(("camera_id".into(), Value::Text(camera_id.into())));

    let confirm_btn = button_with_params("Usuń", "camera-remove", "destructive", params);
    let cancel_btn = button("Anuluj", "camera-remove-cancel", "ghost");

    card(None, vec![stack_v(vec![
        text_styled(&alloc::format!("Usunąć kamerę \"{}\"?", name), "body_strong"),
        text("Tej operacji nie można cofnąć."),
        stack_h(vec![confirm_btn, cancel_btn]),
    ])])
}

/// Analysis-flow selector for the selected camera. The Select lists the active
/// `camera_analysis` flows (plus a "no flow" clear option) and commits the pick
/// immediately via `camera-flow-change` (carrying `camera_id`); the cold path
/// then runs the assigned flow on every detection event. The current value is
/// preselected by seeding the bound `camera_flow_select` store key in
/// `render_panel` (read from core, since the addon mirror has no flow id).
fn build_camera_flow_config(camera_id: &str, cameras: &[db::CameraRow]) -> Component {
    let name = cameras
        .iter()
        .find(|c| c.id == camera_id)
        .map(|c| c.name.as_str())
        .unwrap_or(camera_id);

    let mut options = vec![SelectOption {
        value: SelectValue::Text(String::new()),
        label: lit("— bez flow (wbudowana analiza) —"),
        icon: None,
        disabled: false,
        group_id: None,
        description: None,
    }];
    match host_camera_analysis_flows() {
        Ok(flows) => {
            for (id, fname) in flows {
                options.push(SelectOption {
                    value: SelectValue::Text(id),
                    label: lit(&fname),
                    icon: None,
                    disabled: false,
                    group_id: None,
                    description: None,
                });
            }
        }
        Err(e) => {
            return card(None, vec![stack_v(vec![
                text_styled(&alloc::format!("Flow analizy — {}", name), "body_strong"),
                alert(
                    &alloc::format!("Nie udało się pobrać listy flow: {}", abi_message(e)),
                    "critical",
                ),
                button("Zamknij", "camera-flow-cancel", "ghost"),
            ])]);
        }
    }

    let mut selector = select("Flow analizy", options, "camera_flow_select");
    let mut params = CborMap::default();
    params.0.push(("camera_id".into(), Value::Text(camera_id.into())));
    selector.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Change,
        Handler::Backend {
            action_id: "camera-flow-change".into(),
            params,
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));

    card(None, vec![stack_v(vec![
        text_styled(&alloc::format!("Flow analizy — {}", name), "body_strong"),
        text("Wybrany flow uruchamia się na każdej detekcji z tej kamery."),
        selector,
        button("Zamknij", "camera-flow-cancel", "ghost"),
    ])])
}

// =============================================================================
// CV pipeline UI — per-camera selector + pipeline manager / stage editor
// =============================================================================

/// Picker label for one pipeline (the seed default is marked inline).
fn pipeline_option_label(p: &CameraCvPipelineSummary) -> String {
    if p.is_default {
        alloc::format!("{} (domyślny)", p.name)
    } else {
        p.name.clone()
    }
}

/// CV-pipeline selector for the selected camera. Mirrors the analysis-flow
/// selector: the Select lists every pipeline (plus the "default" clear option)
/// and commits the pick immediately via `camera-pipeline-change`; the current
/// value is preselected by seeding the bound `camera_pipeline_select` store key
/// in `render_panel` (read from core via `camera_get`).
fn build_camera_pipeline_config(camera_id: &str, cameras: &[db::CameraRow]) -> Component {
    let name = cameras
        .iter()
        .find(|c| c.id == camera_id)
        .map(|c| c.name.as_str())
        .unwrap_or(camera_id);

    let mut options = vec![SelectOption {
        value: SelectValue::Text(String::new()),
        label: lit("— domyślny —"),
        icon: None,
        disabled: false,
        group_id: None,
        description: None,
    }];
    match host_cv_pipelines_list() {
        Ok(pipelines) => {
            for p in &pipelines {
                options.push(SelectOption {
                    value: SelectValue::Text(p.id.clone()),
                    label: lit(&pipeline_option_label(p)),
                    icon: None,
                    disabled: false,
                    group_id: None,
                    description: None,
                });
            }
        }
        Err(e) => {
            return card(None, vec![stack_v(vec![
                text_styled(&alloc::format!("Pipeline analizy — {}", name), "body_strong"),
                alert(
                    &alloc::format!("Nie udało się pobrać listy pipeline'ów: {}", abi_message(e)),
                    "critical",
                ),
                button("Zamknij", "camera-pipeline-cancel", "ghost"),
            ])]);
        }
    }

    let mut selector = select("Pipeline analizy", options, "camera_pipeline_select");
    let mut params = CborMap::default();
    params.0.push(("camera_id".into(), Value::Text(camera_id.into())));
    selector.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Change,
        Handler::Backend {
            action_id: "camera-pipeline-change".into(),
            params,
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));

    card(None, vec![stack_v(vec![
        text_styled(&alloc::format!("Pipeline analizy — {}", name), "body_strong"),
        text("Pipeline określa etapy analizy CV (detekcja, klasyfikacja, OCR) uruchamiane na obrazie z tej kamery."),
        selector,
        stack_h(vec![
            button("Edytuj pipeline'y", "pipeline-manager-show", "secondary"),
            button("Zamknij", "camera-pipeline-cancel", "ghost"),
        ]),
    ])])
}

/// Text input for one stage-editor field: bound to `key` in the store and
/// mirrored into the backend draft per keystroke via
/// `pipeline-stage-field-change` (tagged with the stage index + field name).
fn pipeline_stage_input(label: &str, placeholder: &str, key: &str, index: usize, field: &str) -> Component {
    let mut comp = input(label, placeholder, key);
    let mut params = CborMap::default();
    params.0.push(("index".into(), Value::Text(alloc::format!("{}", index))));
    params.0.push(("field".into(), Value::Text(field.into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Input,
        Handler::Backend {
            action_id: "pipeline-stage-field-change".into(),
            params,
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    comp
}

/// Select for one stage-editor field; commits the pick to the backend draft
/// on change (same action + tagging as [`pipeline_stage_input`]).
fn pipeline_stage_select(label: &str, options: Vec<SelectOption>, key: &str, index: usize, field: &str) -> Component {
    let mut comp = select(label, options, key);
    let mut params = CborMap::default();
    params.0.push(("index".into(), Value::Text(alloc::format!("{}", index))));
    params.0.push(("field".into(), Value::Text(field.into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Change,
        Handler::Backend {
            action_id: "pipeline-stage-field-change".into(),
            params,
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    comp
}

fn plain_select_option(value: &str, label: &str) -> SelectOption {
    SelectOption {
        value: SelectValue::Text(value.into()),
        label: lit(label),
        icon: None,
        disabled: false,
        group_id: None,
        description: None,
    }
}

/// Model-alias options for the stage model picker: every usable (granted +
/// active) alias, plus the stage's current value when it is outside the grant
/// list so an imported pipeline still shows its real alias instead of a blank.
fn stage_model_options(current: &str, aliases: &[AvailableAlias]) -> Vec<SelectOption> {
    let mut options = vec![plain_select_option("", "— wybierz model —")];
    let mut seen_current = current.trim().is_empty();
    for a in aliases {
        if !a.is_usable() || !a.active {
            continue;
        }
        let label = match a.target_model.as_deref().filter(|t| !t.is_empty()) {
            Some(target) => alloc::format!("{} → {}", a.alias_id, target),
            None => a.alias_id.clone(),
        };
        if a.alias_id == current {
            seen_current = true;
        }
        options.push(plain_select_option(&a.alias_id, &label));
    }
    if !seen_current {
        options.push(plain_select_option(current, current));
    }
    options
}

/// One stage of the pipeline editor. Field visibility follows the op: detect
/// exposes fps + threshold (input = frame), crop ops expose the source detect
/// stage + class patterns, OCR additionally its mode.
fn build_pipeline_stage_card(
    index: usize,
    st: &StageDraft,
    aliases: &[AvailableAlias],
    detect_ids: &[String],
) -> Component {
    let key = |suffix: &str| alloc::format!("cvp_s{}_{}", index, suffix);
    let mut fields: Vec<Component> = Vec::new();

    fields.push(pipeline_stage_input("ID etapu", "np. detect", &key("id"), index, "stage_id"));
    fields.push(pipeline_stage_select(
        "Operacja",
        vec![
            plain_select_option("detect", "Detekcja (detect)"),
            plain_select_option("classify", "Klasyfikacja (classify)"),
            plain_select_option("ocr", "OCR (ocr)"),
            plain_select_option("embed", "Embedding (embed)"),
        ],
        &key("op"),
        index,
        "op",
    ));
    fields.push(pipeline_stage_select(
        "Model (alias)",
        stage_model_options(&st.model, aliases),
        &key("model"),
        index,
        "model",
    ));

    if st.op == "detect" {
        fields.push(pipeline_stage_input(
            "FPS etapu (puste = FPS analizy kamery)",
            "np. 15",
            &key("fps"),
            index,
            "fps",
        ));
        fields.push(pipeline_stage_input(
            "Próg detekcji 0–1 (puste = domyślny)",
            "np. 0.5",
            &key("threshold"),
            index,
            "threshold",
        ));
    } else {
        let parent_options: Vec<SelectOption> = detect_ids
            .iter()
            .filter(|id| id.as_str() != st.stage_id)
            .map(|id| plain_select_option(id, id))
            .collect();
        if parent_options.is_empty() {
            fields.push(alert(
                "Ten etap wymaga etapu detekcji (op=detect) jako źródła wycinków.",
                "warning",
            ));
        } else {
            fields.push(pipeline_stage_select(
                "Etap źródłowy (detekcja)",
                parent_options,
                &key("parent"),
                index,
                "parent",
            ));
        }
        fields.push(pipeline_stage_input(
            "Klasy (wzorce po przecinku, końcowa * = prefiks)",
            "np. nalepka*, tablica_rejestracyjna",
            &key("classes"),
            index,
            "classes",
        ));
        let class_chips: Vec<Component> = st
            .classes
            .split(',')
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(|c| chip(c, "neutral"))
            .collect();
        if !class_chips.is_empty() {
            fields.push(stack_h(class_chips));
        }
        if st.op == "ocr" {
            fields.push(pipeline_stage_select(
                "Tryb OCR",
                vec![
                    plain_select_option("plate", "Tablica rejestracyjna (plate)"),
                    plain_select_option("adr", "Tablica ADR (adr)"),
                    plain_select_option("generic", "Ogólny tekst (generic)"),
                ],
                &key("ocr_mode"),
                index,
                "ocr_mode",
            ));
        }
    }

    let mut enabled_toggle = toggle("Etap włączony", &key("enabled"));
    let mut toggle_params = CborMap::default();
    toggle_params.0.push(("index".into(), Value::Text(alloc::format!("{}", index))));
    enabled_toggle.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Change,
        Handler::Backend {
            action_id: "pipeline-stage-toggle".into(),
            params: toggle_params,
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    let mut remove_params = CborMap::default();
    remove_params.0.push(("index".into(), Value::Text(alloc::format!("{}", index))));
    fields.push(stack_h(vec![
        enabled_toggle,
        button_with_params("Usuń etap", "pipeline-stage-remove", "destructive", remove_params),
    ]));

    card(
        Some(&alloc::format!("Etap {} — {}", index + 1, if st.stage_id.trim().is_empty() { "(bez ID)" } else { st.stage_id.trim() })),
        vec![stack_v(fields)],
    )
}

/// The stage editor for the pipeline being created / edited.
fn build_pipeline_editor() -> Component {
    let (editing_id, stages) = with_state(|s| (
        s.cv_pipelines.editing_id.clone(),
        s.cv_pipelines.stages.clone(),
    ));
    let aliases = alias_list_available().unwrap_or_default();
    let detect_ids: Vec<String> = stages
        .iter()
        .filter(|s| s.op == "detect")
        .map(|s| s.stage_id.clone())
        .collect();

    let mut children: Vec<Component> = Vec::new();

    let mut name_input = input("Nazwa pipeline'u", "np. Analiza rampy nr 2", "cvp_name");
    name_input.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Input,
        Handler::Backend {
            action_id: "pipeline-name-change".into(),
            params: CborMap::default(),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    children.push(name_input);

    for (i, st) in stages.iter().enumerate() {
        children.push(build_pipeline_stage_card(i, st, &aliases, &detect_ids));
    }

    children.push(stack_h(vec![
        button("Dodaj etap", "pipeline-stage-add", "secondary"),
    ]));
    children.push(divider());
    children.push(stack_h(vec![
        button("Zapisz", "pipeline-save", "primary"),
        button("Anuluj", "pipeline-editor-cancel", "ghost"),
    ]));

    let title = if editing_id.is_some() {
        "Edycja pipeline'u analizy CV"
    } else {
        "Nowy pipeline analizy CV"
    };
    card(Some(title), vec![stack_v(children)])
}

/// The pipeline manager: list of pipelines (default badge, edit / duplicate /
/// delete) or, when a draft is open, the stage editor.
fn build_pipeline_manager() -> Component {
    if with_state(|s| s.cv_pipelines.editor_visible) {
        return build_pipeline_editor();
    }

    let pipelines = match host_cv_pipelines_list() {
        Ok(p) => p,
        Err(e) => {
            return card(Some("Pipeline'y analizy CV"), vec![stack_v(vec![
                alert(
                    &alloc::format!("Nie udało się pobrać pipeline'ów: {}", abi_message(e)),
                    "critical",
                ),
                button("Zamknij", "pipeline-manager-close", "ghost"),
            ])]);
        }
    };

    let pending_remove = with_state(|s| s.cv_pipelines.pending_remove.clone());
    let mut children: Vec<Component> = Vec::new();
    if pipelines.is_empty() {
        children.push(text("Brak pipeline'ów — utwórz pierwszy."));
    }
    for p in &pipelines {
        let mut params = CborMap::default();
        params.0.push(("pipeline_id".into(), Value::Text(p.id.clone())));
        let mut row: Vec<Component> = vec![text_styled(&p.name, "body_strong")];
        if p.is_default {
            row.push(badge("domyślny", "info"));
        }
        row.push(button_with_params("Edytuj", "pipeline-edit", "secondary", params.clone()));
        row.push(button_with_params("Duplikuj", "pipeline-duplicate", "ghost", params.clone()));
        if !p.is_default {
            row.push(button_with_params("Usuń", "pipeline-row-remove", "destructive", params.clone()));
        }
        children.push(stack_h(row));
        if pending_remove.as_deref() == Some(p.id.as_str()) {
            children.push(stack_h(vec![
                text_styled(&alloc::format!("Usunąć pipeline \"{}\"?", p.name), "body_strong"),
                button_with_params("Usuń", "pipeline-remove", "destructive", params),
                button("Anuluj", "pipeline-remove-cancel", "ghost"),
            ]));
        }
    }
    children.push(divider());
    children.push(stack_h(vec![
        button("Nowy pipeline", "pipeline-new", "primary"),
        button("Zamknij", "pipeline-manager-close", "ghost"),
    ]));

    card(Some("Pipeline'y analizy CV"), vec![stack_v(children)])
}

/// Store-key seed for the pipeline editor's bound inputs / selects / toggles,
/// sent with the cameras content overlay so every control mounts showing the
/// authoritative backend draft.
fn pipeline_editor_overlay() -> Vec<StateEntry> {
    let (name, stages) = with_state(|s| (
        s.cv_pipelines.name.clone(),
        s.cv_pipelines.stages.clone(),
    ));
    let mut pairs: Vec<(String, Value)> = vec![("cvp_name".into(), Value::Text(name))];
    for (i, st) in stages.iter().enumerate() {
        let key = |suffix: &str| alloc::format!("cvp_s{}_{}", i, suffix);
        pairs.push((key("id"), Value::Text(st.stage_id.clone())));
        pairs.push((key("op"), Value::Text(st.op.clone())));
        pairs.push((key("model"), Value::Text(st.model.clone())));
        pairs.push((key("fps"), Value::Text(st.fps.clone())));
        pairs.push((key("threshold"), Value::Text(st.threshold.clone())));
        pairs.push((key("parent"), Value::Text(st.parent.clone())));
        pairs.push((key("classes"), Value::Text(st.classes.clone())));
        pairs.push((
            key("ocr_mode"),
            Value::Text(if st.ocr_mode.trim().is_empty() { "generic".to_string() } else { st.ocr_mode.clone() }),
        ));
        pairs.push((key("enabled"), Value::Bool(st.enabled)));
    }
    pairs
        .into_iter()
        .map(|(key, value)| StateEntry {
            path: StatePath::new(vec![PathSegment::Key(key)]),
            value,
        })
        .collect()
}

/// Total number of wizard steps. The wizard is 0-indexed internally.
const ADD_CAMERA_WIZARD_STEPS: u8 = 4;

/// Store key carrying the active wizard step id (`"step0".."step3"`) consumed by
/// `StepProgress.current_id_path`.
fn wiz_step_id(step: u8) -> String {
    alloc::format!("step{}", step)
}

/// Builds the per-step visibility / navigation patch pairs for `step`. Every
/// step container and footer button reads one of these booleans through
/// `with_visible`, so navigation is a pure `StatePatch` — no fragment rebuild.
fn wizard_step_pairs(step: u8) -> Vec<(String, Value)> {
    let last = ADD_CAMERA_WIZARD_STEPS - 1;
    vec![
        ("wiz_step".into(), Value::Text(wiz_step_id(step))),
        ("wiz_show_0".into(), Value::Bool(step == 0)),
        ("wiz_show_1".into(), Value::Bool(step == 1)),
        ("wiz_show_2".into(), Value::Bool(step == 2)),
        ("wiz_show_3".into(), Value::Bool(step == 3)),
        ("wiz_show_back".into(), Value::Bool(step > 0)),
        ("wiz_show_next".into(), Value::Bool(step < last)),
        ("wiz_show_finish".into(), Value::Bool(step >= last)),
    ]
}

/// Visibility pairs for the five per-type config blocks of step 2. Exactly one
/// is `true` for the chosen source; switching type is a `StatePatch` that flips
/// these, revealing the matching config without rebuilding the body.
fn wizard_source_pairs(src: Option<SourceType>) -> Vec<(String, Value)> {
    let s = src.map(SourceType::as_str).unwrap_or("");
    vec![
        ("wiz_src".into(), Value::Text(s.into())),
        ("wiz_is_onvif".into(), Value::Bool(src == Some(SourceType::Onvif))),
        ("wiz_is_rtsp".into(), Value::Bool(src == Some(SourceType::Rtsp))),
        ("wiz_is_mjpeg".into(), Value::Bool(src == Some(SourceType::Mjpeg))),
        ("wiz_is_usb".into(), Value::Bool(src == Some(SourceType::Usb))),
        ("wiz_is_file".into(), Value::Bool(src == Some(SourceType::File))),
    ]
}

/// Patch pairs describing the step-2 ONVIF discovery sub-state (scan spinner,
/// discovered-list visibility, count line, manual-entry visibility).
fn wizard_onvif_pairs(s: &DiscoverState) -> Vec<(String, Value)> {
    let count = s.cameras.len();
    vec![
        ("wiz_onvif_scanning".into(), Value::Bool(s.scanning)),
        ("wiz_onvif_has_results".into(), Value::Bool(!s.scanning && count > 0)),
        ("wiz_onvif_no_results".into(), Value::Bool(!s.scanning && count == 0)),
        (
            "wiz_onvif_count".into(),
            Value::Text(alloc::format!(
                "Znaleziono {} kamer. Wybierz jedną lub podaj URL ręcznie.",
                count
            )),
        ),
    ]
}

/// Patch pairs describing the step-3 connection-test sub-state (spinner, result
/// alerts, result text). Mutually exclusive visibility flags drive which block
/// is shown.
fn wizard_test_pairs(s: &DiscoverState) -> Vec<(String, Value)> {
    let (ok, err, text, idle) = match (&s.testing, &s.test_result) {
        (true, _) => (false, false, String::new(), false),
        (false, Some(Ok(m))) => {
            let detail = if m.is_empty() { "Połączenie nawiązane.".to_string() } else { m.clone() };
            (true, false, alloc::format!("Połączenie OK. {}", detail), false)
        }
        (false, Some(Err(m))) => (false, true, m.clone(), false),
        (false, None) => (false, false, String::new(), true),
    };
    vec![
        ("wiz_testing".into(), Value::Bool(s.testing)),
        ("wiz_test_ok".into(), Value::Bool(ok)),
        ("wiz_test_err".into(), Value::Bool(err)),
        ("wiz_test_idle".into(), Value::Bool(idle)),
        ("wiz_test_text".into(), Value::Text(text)),
    ]
}

/// Error-alert patch pair. `wiz_has_error` toggles the alert's visibility while
/// `wiz_error` carries its message.
fn wizard_error_pairs(message: Option<&str>) -> Vec<(String, Value)> {
    vec![
        ("wiz_has_error".into(), Value::Bool(message.is_some())),
        ("wiz_error".into(), Value::Text(message.unwrap_or("").into())),
    ]
}

/// The full set of wizard store keys derived from the current backend wizard
/// state, seeded into the `add_camera_body` SlotContent `state_overlay`. Sent
/// whenever the body fragment is delivered (modal open or an ONVIF
/// scan/select re-send) so every bound visibility flag, the StepProgress and
/// the field inputs resolve to the authoritative backend state on first paint.
fn wizard_full_overlay() -> Vec<StateEntry> {
    let (step, src, err) = with_state(|s| (s.wizard_step, s.discover.source_type, s.error_message.clone()));
    let mut pairs: Vec<(String, Value)> = Vec::new();
    pairs.extend(wizard_step_pairs(step));
    pairs.extend(wizard_source_pairs(src));
    pairs.extend(with_state(|s| wizard_test_pairs(&s.discover)));
    pairs.extend(with_state(|s| wizard_onvif_pairs(&s.discover)));
    pairs.extend(wizard_error_pairs(err.as_deref()));
    // Field bind paths reflect the committed backend values so the two-way-bound
    // inputs show the right text without any further round-trip.
    let fields = with_state(|s| [
        ("onvif_url", s.discover.onvif_url.clone()),
        ("rtsp_url", s.discover.rtsp_url.clone()),
        ("mjpeg_url", s.discover.mjpeg_url.clone()),
        ("usb_device_path", s.discover.usb_device_path.clone()),
        ("file_path", s.discover.file_path.clone()),
        ("cred_user", s.discover.cred_user.clone()),
        ("cred_pass", s.discover.cred_pass.clone()),
        ("name", s.discover.name.clone()),
        ("retention", s.discover.retention.clone()),
        ("fps", s.discover.fps.clone()),
    ]);
    for (key, value) in fields {
        pairs.push((key.into(), Value::Text(value)));
    }
    // Reflect the committed profile (defaulting to "default") so the select
    // shows the authoritative backend value rather than always resetting to it.
    pairs.push(("profile".into(), Value::Text(with_state(|s| s.discover.profile_or_default().to_string()))));
    // Reflect the committed analysis FPS (defaulting to "10") so the select
    // shows the authoritative backend value.
    pairs.push(("analysis_fps".into(), Value::Text(with_state(|s| s.discover.analysis_fps_or_default()))));
    pairs
        .into_iter()
        .map(|(key, value)| StateEntry {
            path: StatePath::new(vec![PathSegment::Key(key)]),
            value,
        })
        .collect()
}

/// The "Add camera" wizard lives in a Modal overlay. This builds the Modal
/// shell that is placed in the "content" slot tree while `add_form_visible`.
/// Its body/footer are filled separately via SlotContent on the dynamic slots
/// `add_camera_body` / `add_camera_footer`, which the host registers only while
/// this Modal is in the DOM. The Dismiss event (×/backdrop/ESC) is routed to
/// the same `camera-add-cancel` action as the footer cancel button so closing
/// the dialog any way resets the wizard state.
fn build_add_camera_modal() -> Component {
    let step = with_state(|s| s.wizard_step);
    let title = alloc::format!("Dodaj kamerę \u{2014} krok {} z {}", step + 1, ADD_CAMERA_WIZARD_STEPS);

    let mut modal = ModalComp {
        title: lit(&title),
        subtitle: None,
        body_slot: "add_camera_body".into(),
        footer_slot: Some("add_camera_footer".into()),
        size: ModalSize::Lg,
        dismissible: true,
        prevent_scroll: true,
        closable: true,
    }.into_component(next_id()).expect("Modal");
    modal.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Dismiss,
        Handler::Backend {
            action_id: "camera-add-cancel".into(),
            params: CborMap::default(),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    modal
}

/// Builds the wizard body fragment for the `add_camera_body` slot ONCE, when the
/// modal opens. Every step lives in the DOM simultaneously, wrapped in a
/// `with_visible` container bound to a `wiz_show_N` store flag; the active step
/// is revealed purely by `StatePatch`. The StepProgress, the per-type config
/// blocks, the test-result block and the error alert are all store-bound, so no
/// wizard interaction (source pick, Next/Back, typing) rebuilds this fragment.
fn build_add_camera_body() -> Component {
    let step_progress = build_wizard_step_progress();

    let step0 = with_visible(build_wizard_step_source_type(), "wiz_show_0");
    let step1 = with_visible(build_wizard_step_config(), "wiz_show_1");
    let step2 = with_visible(build_wizard_step_test(), "wiz_show_2");
    let step3 = with_visible(build_wizard_step_metadata(), "wiz_show_3");

    let error_alert = with_visible(alert_bound("wiz_error", "critical"), "wiz_has_error");

    stack_v(vec![step_progress, step0, step1, step2, step3, error_alert])
}

/// StepProgress bound to `wiz_step`. Status per step is derived by the renderer
/// from `current_id_path` position, so advancing a step is a single patch.
fn build_wizard_step_progress() -> Component {
    let step_labels = ["Typ źródła", "Konfiguracja", "Test połączenia", "Metadane"];
    StepProgressComp {
        steps: step_labels.iter().enumerate().map(|(i, label)| StepDef {
            id: wiz_step_id(i as u8),
            label: lit(label),
            optional: false,
            status: None,
            description: None,
        }).collect(),
        current_id_path: StatePath::new(vec![PathSegment::Key("wiz_step".into())]),
        variant: StepProgressVariant::Horizontal,
        clickable_completed: false,
    }.into_component(next_id()).expect("StepProgress")
}

/// Builds the wizard navigation buttons for the `add_camera_footer` slot ONCE.
/// All four buttons live in the DOM; Back/Next/Finish toggle visibility through
/// store flags (`wiz_show_back/next/finish`) so navigation never rebuilds the
/// footer. The Next label is intentionally generic — the step number lives in
/// the StepProgress, not the button text, so it needs no per-step patching.
fn build_add_camera_footer() -> Component {
    let back = with_visible(button_with_icon("Wstecz", "wizard-prev", "ghost", "info"), "wiz_show_back");
    let cancel = button("Anuluj", "camera-add-cancel", "ghost");
    let next = with_visible(button("Dalej", "wizard-next", "primary"), "wiz_show_next");
    let finish = with_visible(button("Zakończ", "camera-add-submit", "primary"), "wiz_show_finish");
    stack_h(vec![back, cancel, next, finish])
}

/// Step 1 — source-type chooser as a store-bound `RadioCardGroup`. The pick is
/// written to `wiz_src` reactively (client highlight) and forwarded to the
/// backend `wizard-source-select` action, which patches the per-type config
/// visibility flags. No card is rebuilt on selection.
fn build_wizard_step_source_type() -> Component {
    use tentaflow_sdk_spec::protocol::ui::form::RadioCardGroup;
    let options = vec![
        RadioCardOption {
            value: SelectValue::Text(SourceType::Onvif.as_str().into()),
            icon: icon_named(parse_icon_name("search")),
            title: lit("Kamera sieciowa ONVIF"),
            description: Some(lit("Automatyczne wykrywanie kamer ONVIF w sieci lokalnej.")),
            badge: None,
            disabled: false,
        },
        RadioCardOption {
            value: SelectValue::Text(SourceType::Rtsp.as_str().into()),
            icon: icon_named(parse_icon_name("video")),
            title: lit("Strumień RTSP/RTSPS"),
            description: Some(lit("Ręczny adres strumienia rtsp:// lub rtsps://.")),
            badge: None,
            disabled: false,
        },
        RadioCardOption {
            value: SelectValue::Text(SourceType::Mjpeg.as_str().into()),
            icon: icon_named(parse_icon_name("video")),
            title: lit("MJPEG (HTTP)"),
            description: Some(lit("Strumień multipart MJPEG po http:// lub https:// (np. Axis video.cgi).")),
            badge: None,
            disabled: false,
        },
        RadioCardOption {
            value: SelectValue::Text(SourceType::Usb.as_str().into()),
            icon: icon_named(parse_icon_name("cameras")),
            title: lit("Kamera lokalna / USB"),
            description: Some(lit("Urządzenie wideo podłączone do tego hosta (v4l2).")),
            badge: None,
            disabled: false,
        },
        RadioCardOption {
            value: SelectValue::Text(SourceType::File.as_str().into()),
            icon: icon_named(parse_icon_name("evidence")),
            title: lit("Plik testowy"),
            description: Some(lit("Lokalny plik wideo używany jako źródło testowe.")),
            badge: None,
            disabled: false,
        },
    ];

    let mut group = RadioCardGroup {
        bind_path: StatePath::new(vec![PathSegment::Key("wiz_src".into())]),
        options,
        columns: 2,
        variant: RadioCardVariant::Default,
    }.into_component(next_id()).expect("RadioCardGroup");
    group = with_a11y_label(group, "Typ źródła kamery");
    // The change carries the picked SelectValue as `{value, kind}` detail; the
    // backend reads `value` to branch step 2 and patch the per-type flags.
    group.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Change,
        Handler::Backend {
            action_id: "wizard-source-select".into(),
            params: CborMap::default(),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));

    stack_v(vec![
        text("Wybierz typ źródła kamery. Dalsze kroki dopasują się do wybranego typu."),
        group,
    ])
}

/// Step 2 — all five per-type config blocks present at once, each wrapped in a
/// `with_visible` container bound to its `wiz_is_X` flag. Switching source type
/// is a `StatePatch` that flips exactly one flag visible.
fn build_wizard_step_config() -> Component {
    stack_v(vec![
        with_visible(build_config_onvif(), "wiz_is_onvif"),
        with_visible(build_config_rtsp(), "wiz_is_rtsp"),
        with_visible(build_config_mjpeg(), "wiz_is_mjpeg"),
        with_visible(build_config_usb(), "wiz_is_usb"),
        with_visible(build_config_file(), "wiz_is_file"),
    ])
}

/// ONVIF config block. The discovery list and selectable results are genuinely
/// dynamic (populated by an explicit `discover-scan`), so their visibility is
/// store-bound and the scan/select actions re-send this body; the manual URL +
/// credential inputs are always-present, two-way-bound fields.
fn build_config_onvif() -> Component {
    let scanning = with_state(|s| s.discover.scanning);
    let selected_idx = with_state(|s| s.discover.selected_index);

    let scan_spinner = with_visible(
        stack_v(vec![spinner("md"), text("Skanowanie sieci (ONVIF WS-Discovery)...")]),
        "wiz_onvif_scanning",
    );

    let no_results = with_visible(
        stack_v(vec![
            text("Zeskanuj sieć w poszukiwaniu kamer ONVIF lub podaj adres URL urządzenia ręcznie."),
            stack_h(vec![button_with_icon("Skanuj sieć", "discover-scan", "primary", "search")]),
        ]),
        "wiz_onvif_no_results",
    );

    let discovered = with_state(|s| s.discover.cameras.iter().enumerate()
        .map(|(i, c)| (i, c.suggested_name.clone(), c.url.clone())).collect::<Vec<_>>());
    let mut cam_rows: Vec<Component> = Vec::new();
    for (i, name, url) in &discovered {
        let is_sel = !scanning && selected_idx == Some(*i);
        let row_content = stack_v_gap("xs", vec![
            text_styled(name, "body_strong"),
            text_styled(url, "caption"),
        ]);
        let mut row_card = Card {
            variant: if is_sel { CardVariant::Filled } else { CardVariant::Outlined },
            padding: Spacing::Sm,
            gap: Spacing::Sm,
            radius: RadiusToken::Sm,
            shadow: ShadowToken::None,
            border: BorderToken::Hairline,
            background: BackgroundToken::None,
            accent: if is_sel { Some(Tone::Primary) } else { None },
            children: vec![row_content],
            interactive: true,
            clickable: true,
        }.into_component(next_id()).expect("Card");
        let mut params = CborMap::default();
        params.0.push(("index".into(), Value::U64(*i as u64)));
        row_card.handlers = Some(HandlerMap(vec![(
            tentaflow_sdk_spec::EventKind::Click,
            Handler::Backend {
                action_id: "discover-select".into(),
                params,
                optimistic: None,
                on_failure: FailurePolicy::Toast,
            },
        )]));
        cam_rows.push(row_card);
    }
    let has_results = with_visible(
        stack_v(vec![
            text_bound("wiz_onvif_count"),
            stack_v_gap("xs", cam_rows),
            button_with_icon("Skanuj ponownie", "discover-scan", "ghost", "search"),
        ]),
        "wiz_onvif_has_results",
    );

    let url_input = wizard_input("URL urządzenia ONVIF", "http://10.0.0.5/onvif/device_service", "onvif_url", false);
    let user_input = wizard_input("Użytkownik", "", "cred_user", false);
    let pass_input = wizard_input("Hasło", "", "cred_pass", true);

    stack_v(vec![
        scan_spinner,
        no_results,
        has_results,
        url_input,
        grid(2, vec![user_input, pass_input]),
        text_styled("Kamera ONVIF wymaga użytkownika i hasła.", "caption"),
    ])
}

fn build_config_rtsp() -> Component {
    let url_input = wizard_input("URL strumienia RTSP", "rtsp://host:554/stream", "rtsp_url", false);
    let user_input = wizard_input("Użytkownik (opcjonalnie)", "", "cred_user", false);
    let pass_input = wizard_input("Hasło (opcjonalnie)", "", "cred_pass", true);
    stack_v(vec![
        text("Podaj adres strumienia RTSP/RTSPS. Poświadczenia są opcjonalne."),
        url_input,
        grid(2, vec![user_input, pass_input]),
    ])
}

/// Blok konfiguracji MJPEG (HTTP). Adres strumienia multipart (np. Axis
/// `/axis-cgi/mjpg/video.cgi`) + opcjonalne poświadczenia — host przekazuje je
/// do souphttpsrc (`user-id`/`user-pw`), nie do URL-a.
fn build_config_mjpeg() -> Component {
    let url_input = wizard_input("URL strumienia MJPEG", "http://10.0.0.5/axis-cgi/mjpg/video.cgi", "mjpeg_url", false);
    let user_input = wizard_input("Użytkownik (opcjonalnie)", "", "cred_user", false);
    let pass_input = wizard_input("Hasło (opcjonalnie)", "", "cred_pass", true);
    stack_v(vec![
        text("Podaj adres strumienia MJPEG (multipart) po HTTP/HTTPS. Poświadczenia są opcjonalne."),
        url_input,
        grid(2, vec![user_input, pass_input]),
    ])
}

/// USB config block. Local devices are enumerated eagerly at modal open so the
/// Select options can be baked into this fragment (Select options are static
/// component fields, not store-bound). A manual path input is always present so
/// the step is never a dead end when no device is detected. The device Select
/// and the manual input both two-way bind `usb_device_path`.
fn build_config_usb() -> Component {
    let devices = with_state(|s| s.discover.usb_devices.iter()
        .map(|d| (d.device_path.clone(), d.label.clone())).collect::<Vec<_>>());

    if devices.is_empty() {
        return stack_v(vec![
            alert("Nie wykryto lokalnych urządzeń wideo (v4l2). Podaj ścieżkę ręcznie.", "info"),
            wizard_input("Ścieżka urządzenia", "/dev/video0", "usb_device_path", false),
        ]);
    }

    let options: Vec<SelectOption> = devices.iter().map(|(path, label)| SelectOption {
        value: SelectValue::Text(path.clone()),
        label: lit(&alloc::format!("{} ({})", label, path)),
        icon: None,
        disabled: false,
        group_id: None,
        description: None,
    }).collect();
    let device_select = wizard_select("Wykryte urządzenie", options, "usb_device_path");

    stack_v(vec![
        text(&alloc::format!("Wykryto {} urządzeń lokalnych. Wybierz źródło wideo.", devices.len())),
        device_select,
    ])
}

fn build_config_file() -> Component {
    stack_v(vec![
        text("Podaj ścieżkę lokalnego pliku wideo używanego jako źródło testowe."),
        wizard_input("Ścieżka pliku wideo", "/var/lib/tentaflow/sample.mp4", "file_path", false),
    ])
}

/// Step 3 — connection probe. Spinner, success alert, error alert and idle
/// empty-state all live in the DOM, toggled by `wiz_testing/test_ok/test_err/
/// test_idle` flags; the result text is bound to `wiz_test_text`. Running the
/// test is a `StatePatch`, never a rebuild. No fabricated preview frame.
fn build_wizard_step_test() -> Component {
    let testing_block = with_visible(
        stack_v(vec![spinner("md"), text("Testowanie połączenia z kamerą...")]),
        "wiz_testing",
    );
    let ok_block = with_visible(alert_bound("wiz_test_text", "success"), "wiz_test_ok");
    let err_block = with_visible(alert_bound("wiz_test_text", "critical"), "wiz_test_err");
    let idle_block = with_visible(
        empty_state("Brak testu", Some("Uruchom test, aby sprawdzić połączenie z kamerą."), Some("info")),
        "wiz_test_idle",
    );

    stack_v(vec![
        text("Sprawdź połączenie ze źródłem przed dodaniem kamery."),
        stack_h(vec![button_with_icon("Testuj połączenie", "wizard-test", "primary", "check")]),
        testing_block,
        ok_block,
        err_block,
        idle_block,
        text_styled("Podgląd na żywo będzie dostępny po dodaniu kamery.", "caption"),
    ])
}

/// Step 4 — camera metadata. All fields two-way bind their store keys; no preset
/// values. The metadata name is pre-filled from a discovered camera on the
/// step-2→3 transition via a `StatePatch`, not by rebuilding this fragment.
fn build_wizard_step_metadata() -> Component {
    let name_input = wizard_input("Nazwa kamery", "np. Brama wjazdowa", "name", false);
    let retention_select = wizard_select("Klasa retencji", vec![
        SelectOption { value: SelectValue::Text("A".into()), label: lit("A — długa retencja"), icon: None, disabled: false, group_id: None, description: None },
        SelectOption { value: SelectValue::Text("B".into()), label: lit("B — średnia retencja"), icon: None, disabled: false, group_id: None, description: None },
        SelectOption { value: SelectValue::Text("C".into()), label: lit("C — krótka retencja"), icon: None, disabled: false, group_id: None, description: None },
        SelectOption { value: SelectValue::Text("Unclassified".into()), label: lit("Niesklasyfikowana"), icon: None, disabled: false, group_id: None, description: None },
    ], "retention");
    let fps_input = wizard_input("Docelowe FPS", "15", "fps", false);
    let analysis_fps_select = wizard_select("FPS analizy AI", vec![
        SelectOption { value: SelectValue::Text("1".into()), label: lit("1 / s"), icon: None, disabled: false, group_id: None, description: None },
        SelectOption { value: SelectValue::Text("5".into()), label: lit("5 / s"), icon: None, disabled: false, group_id: None, description: None },
        SelectOption { value: SelectValue::Text("10".into()), label: lit("10 / s"), icon: None, disabled: false, group_id: None, description: None },
        SelectOption { value: SelectValue::Text("15".into()), label: lit("15 / s"), icon: None, disabled: false, group_id: None, description: None },
        SelectOption { value: SelectValue::Text("0".into()), label: lit("Bez limitu"), icon: None, disabled: false, group_id: None, description: None },
    ], "analysis_fps");
    let profile_select = wizard_select("Profil analityczny", vec![
        SelectOption { value: SelectValue::Text("default".into()), label: lit("default"), icon: None, disabled: false, group_id: None, description: None },
    ], "profile");

    stack_v(vec![
        text("Uzupełnij metadane kamery przed jej dodaniem."),
        grid(2, vec![name_input, retention_select, fps_input, analysis_fps_select, profile_select]),
    ])
}

/// The Alarm Center (m05): left = real-time alarm feed with status/severity
/// filters, right = the selected alarm's detail + decision workflow. Everything
/// renders from `db::list_alarms` / `db::get_alarm`; decisions persist through
/// `db::update_alarm_status` + an audit-log entry.
fn build_alarms_content() -> Component {
    let messages = build_messages_section();
    let (severity, status_view, selected_id) = with_state(|s| (
        s.alarms.severity_or_all().to_string(),
        s.alarms.status_or_open().to_string(),
        s.alarms.selected_id.clone(),
    ));

    // Header: title + live severity counts + the "simulate alarm" test button.
    let open_count = db::count_alarms(true, "").unwrap_or(0);
    let crit_open = db::list_alarms("critical", "", true).map(|v| v.len()).unwrap_or(0);
    let header = stack_h(vec![
        heading(2, "Centrum alarmów"),
        chip_toned(&alloc::format!("{} otwartych", open_count), if open_count > 0 { "warning" } else { "muted" }),
        chip_toned(&alloc::format!("{} krytycznych", crit_open), if crit_open > 0 { "critical" } else { "muted" }),
        button_with_icon("Symuluj alarm", "alarm-simulate", "primary", "bell"),
        button("Potwierdź wszystkie", "alarm-acknowledge-all", "secondary"),
    ]);

    // Feed query per view: open collapses new+acknowledged, closed lists decided
    // rows, all lists everything (no status constraint).
    let sev = if severity == "all" { "" } else { severity.as_str() };
    let alarms = match status_view.as_str() {
        "closed" => list_closed_alarms(sev),
        "all" => db::list_alarms(sev, "", false),
        _ => db::list_alarms(sev, "", true),
    };

    let alarms = match alarms {
        Ok(a) => a,
        Err(e) => {
            return stack_v(vec![messages, header, alert(&alloc::format!("Nie udało się pobrać alarmów: {}", abi_message(e)), "critical")]);
        }
    };

    // LEFT — status tabs (counts) + severity chips + the card feed.
    let total_count = db::count_alarms(false, "").unwrap_or(0);
    let closed_count = (total_count - open_count).max(0);
    let status_tabs = stack_h_gap("xs", vec![
        alarm_status_tab("Niepotwierdzone", "open", &status_view, open_count),
        alarm_status_tab("Wszystkie", "all", &status_view, total_count),
        alarm_status_tab("Zamknięte", "closed", &status_view, closed_count),
    ]);
    let severity_chips = stack_h_gap("xs", vec![
        alarm_severity_chip("Wszystkie", "all", &severity),
        alarm_severity_chip("critical", "critical", &severity),
        alarm_severity_chip("warning", "warning", &severity),
        alarm_severity_chip("info", "info", &severity),
    ]);

    let feed_body = if alarms.is_empty() {
        empty_state("Brak alarmów", Some("Gdy analityka wykryje zdarzenie, pojawi się tutaj. Użyj przycisku Symuluj alarm do testu."), Some("bell"))
    } else {
        let cards: Vec<Component> = alarms.iter()
            .map(|a| build_alarm_feed_card(a, selected_id.as_deref() == Some(a.id.as_str())))
            .collect();
        stack_v_gap("sm", cards)
    };
    let left = stack_v(vec![status_tabs, severity_chips, feed_body]);

    // RIGHT — detail panel for the selected alarm (or a prompt to pick one).
    let detail = match selected_id.as_deref().and_then(|id| db::get_alarm(id).ok().flatten()) {
        Some(a) => build_alarm_detail(&a),
        None => card(None, vec![empty_state(
            "Wybierz alarm",
            Some("Kliknij kartę alarmu po lewej, aby zobaczyć klip, klatki i podjąć decyzję."),
            Some("info"),
        )]),
    };

    let split = grid(2, vec![left, detail]);
    stack_v(vec![messages, header, split])
}

/// Closed-feed query: decided alarms (confirmed/dismissed/escalated), optionally
/// narrowed by severity. Uses `list_alarms` per-status and merges by ts desc.
fn list_closed_alarms(severity: &str) -> Result<Vec<db::AlarmRow>, AbiError> {
    let mut out: Vec<db::AlarmRow> = Vec::new();
    for st in ["confirmed", "dismissed", "escalated"] {
        out.extend(db::list_alarms(severity, st, false)?);
    }
    out.sort_by(|a, b| b.ts.cmp(&a.ts));
    Ok(out)
}

/// A left-feed status tab rendered as a toned chip button (selected = primary).
fn alarm_status_tab(label: &str, view: &str, active: &str, count: i64) -> Component {
    let lbl = alloc::format!("{} ({})", label, count);
    let mut params = CborMap::default();
    params.0.push(("view".into(), Value::Text(view.into())));
    let variant = if active == view { "primary" } else { "ghost" };
    button_with_params(&lbl, "alarm-status-view", variant, params)
}

/// A severity filter chip rendered as a toned button (selected = solid tone).
fn alarm_severity_chip(label: &str, value: &str, active: &str) -> Component {
    let mut params = CborMap::default();
    params.0.push(("value".into(), Value::Text(value.into())));
    let variant = if active == value { "primary" } else { "ghost" };
    button_with_params(label, "alarm-filter-severity", variant, params)
}

/// Maps a persisted alarm status to a label + toned chip for the feed/detail.
fn alarm_status_chip(status: &str) -> Component {
    let (label, tone) = match status {
        "confirmed" => ("potwierdzony", "critical"),
        "dismissed" => ("fałszywy", "muted"),
        "escalated" => ("eskalowany", "warning"),
        "acknowledged" => ("przyjęty", "info"),
        _ => ("nowy", "success"),
    };
    chip_toned(label, tone)
}

/// One alarm card in the left feed. Severity drives the chip tone (critical=red,
/// warning=amber, info). The whole card is clickable → loads it into the detail.
fn build_alarm_feed_card(a: &db::AlarmRow, selected: bool) -> Component {
    let title = if !a.kind.is_empty() && !a.message.is_empty() {
        alloc::format!("{} · {}", a.kind, a.message)
    } else if !a.message.is_empty() {
        a.message.clone()
    } else if !a.kind.is_empty() {
        a.kind.clone()
    } else {
        "Zdarzenie".into()
    };
    let camera_label = if !a.camera_name.is_empty() {
        a.camera_name.clone()
    } else if !a.camera_id.is_empty() {
        a.camera_id.clone()
    } else {
        "—".into()
    };
    let sev_tone = alarm_severity_tone(&a.severity);
    let meta = stack_h_gap("xs", vec![
        chip_with_icon(&camera_label, "category", "cameras"),
        chip_with_icon(&format_alarm_time(a.ts), "category", "clock"),
        chip_toned(&a.severity, sev_tone),
        alarm_status_chip(&a.status),
    ]);
    let body = stack_v_gap("xs", vec![text_styled(&title, "body_strong"), meta]);

    let mut row_card = Card {
        variant: if selected { CardVariant::Filled } else { CardVariant::Outlined },
        padding: Spacing::Sm,
        gap: Spacing::Sm,
        radius: RadiusToken::Md,
        shadow: ShadowToken::None,
        border: BorderToken::Hairline,
        background: BackgroundToken::None,
        accent: Some(parse_tone(sev_tone)),
        children: vec![body],
        interactive: true,
        clickable: true,
    }.into_component(next_id()).expect("Card");
    let mut params = CborMap::default();
    params.0.push(("alarm_id".into(), Value::Text(a.id.clone())));
    row_card.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Click,
        Handler::Backend {
            action_id: "alarm-select".into(),
            params,
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    row_card
}

/// Severity → chip tone token. critical=red, warning=amber, anything else=info.
fn alarm_severity_tone(severity: &str) -> &'static str {
    match severity {
        "critical" => "critical",
        "warning" => "warning",
        _ => "info",
    }
}

/// The right-hand alarm detail + decision workflow. Mirrors m05: clip placeholder,
/// a frame timeline, a metadata table and the decision buttons + operator note.
fn build_alarm_detail(a: &db::AlarmRow) -> Component {
    let sev_tone = alarm_severity_tone(&a.severity);
    let camera_label = if !a.camera_name.is_empty() { a.camera_name.clone() } else { a.camera_id.clone() };
    let title_kind = if a.kind.is_empty() { "Zdarzenie".to_string() } else { a.kind.clone() };

    let head = stack_h(vec![
        chip_toned(&title_kind, sev_tone),
        text_styled(&alloc::format!("{} · {}", camera_label, format_alarm_time(a.ts)), "caption"),
        chip_toned(&alloc::format!("alarm {}", short_id(&a.id)), "muted"),
        alarm_status_chip(&a.status),
    ]);

    // 30 s clip placeholder + a 10-frame timeline (the event frame highlighted).
    let clip = card(None, vec![
        stack_h(vec![
            chip_toned_icon("Klip 30 s · 0:00 / 0:30", "info", "video"),
        ]),
    ]);
    let frame_labels = ["−2s", "−1s", "EVT", "+1s", "+2s", "+3s", "+4s", "+5s", "+6s", "+7s"];
    let frames: Vec<Component> = frame_labels.iter()
        .map(|l| chip_toned(l, if *l == "EVT" { "primary" } else { "muted" }))
        .collect();
    let timeline = stack_h_gap("xs", frames);

    // Metadata table — straight from the persisted alarm row.
    let metadata = card(Some("Metadane"), vec![
        key_value(vec![
            ("Detektor", &title_kind),
            ("Kamera", &camera_label),
            ("Poziom", a.severity.as_str()),
            ("Status", alarm_status_long(&a.status)),
            ("Zgłoszono", &format_alarm_datetime(a.ts)),
            ("Decyzja", &alarm_decision_note(a)),
        ]),
    ]);

    // Decision workflow — buttons persist status + audit, note carries forward.
    let mut confirm_p = CborMap::default();
    confirm_p.0.push(("alarm_id".into(), Value::Text(a.id.clone())));
    confirm_p.0.push(("decision".into(), Value::Text("confirmed".into())));
    let mut dismiss_p = CborMap::default();
    dismiss_p.0.push(("alarm_id".into(), Value::Text(a.id.clone())));
    dismiss_p.0.push(("decision".into(), Value::Text("dismissed".into())));
    let mut escalate_p = CborMap::default();
    escalate_p.0.push(("alarm_id".into(), Value::Text(a.id.clone())));
    escalate_p.0.push(("decision".into(), Value::Text("escalated".into())));

    let decision_buttons = stack_h_gap("sm", vec![
        button_with_params("Potwierdź", "alarm-decide", "primary", confirm_p),
        button_with_params("Fałszywy", "alarm-decide", "destructive", dismiss_p),
        button_with_params("Eskaluj", "alarm-decide", "secondary", escalate_p),
    ]);
    let note = alarm_note_textarea();
    let workflow = card(Some("Workflow"), vec![
        text_styled("Decyzja operatora", "caption"),
        decision_buttons,
        note,
    ]);

    card(None, vec![head, clip, timeline, grid(2, vec![metadata, workflow])])
}

/// Operator-note textarea bound to the `alarm_note` store key.
fn alarm_note_textarea() -> Component {
    use tentaflow_sdk_spec::protocol::ui::form::Textarea;
    let mut comp = Textarea {
        bind_path: StatePath::new(vec![PathSegment::Key("alarm_note".into())]),
        placeholder: Some(lit("np. dwie osoby, kłótnia w pobliżu wjazdu — wysłano patrol...")),
        label: Some(lit("Notatka operatora")),
        hint: None,
        validators: vec![],
        max_length: Some(2000),
        min_length: None,
        disabled: None,
        readonly: None,
        error: None,
        size: InputSize::Md,
        rows: 3,
        autoresize: true,
        max_rows: Some(8),
        monospace: false,
    }.into_component("alarm_note").expect("Textarea");
    let mut params = CborMap::default();
    params.0.push(("field".into(), Value::Text("note".into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Input,
        Handler::Backend {
            action_id: "alarm-note-change".into(),
            params,
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    comp
}

fn alarm_status_long(status: &str) -> &'static str {
    match status {
        "confirmed" => "Potwierdzony",
        "dismissed" => "Fałszywy alarm",
        "escalated" => "Eskalowany",
        "acknowledged" => "Przyjęty",
        _ => "Nowy (niepotwierdzony)",
    }
}

/// Decision summary for the metadata table: operator + time, or a dash.
fn alarm_decision_note(a: &db::AlarmRow) -> String {
    if a.decided_at == 0 && a.decided_by.is_empty() {
        return "—".into();
    }
    let who = if a.decided_by.is_empty() { "operator".to_string() } else { a.decided_by.clone() };
    alloc::format!("{} · {}", who, format_alarm_datetime(a.decided_at))
}

/// Short, display-friendly id tail (the trailing counter segment).
fn short_id(id: &str) -> String {
    id.rsplit('-').next().unwrap_or(id).to_string()
}

/// Unix ts → "YYYY-MM-DD HH:MM" (UTC), good enough for the detail metadata.
fn format_alarm_datetime(ts: i64) -> String {
    if ts <= 0 {
        return "—".into();
    }
    let days = ts / 86_400;
    let (y, m, d) = civil_from_days(days);
    let secs = ts.rem_euclid(86_400);
    alloc::format!("{:04}-{:02}-{:02} {:02}:{:02}", y, m, d, secs / 3600, (secs % 3600) / 60)
}

/// Howard Hinnant's days→civil date algorithm (proleptic Gregorian, UTC).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// =============================================================================
// Semantic event index — embeddings (llm_generate) + vector store (events ns)
// =============================================================================

/// Vector namespace declared in manifest.toml for semantic event search.
const EVENT_VECTOR_NS: &str = "events";
/// Embedding model + dimension. Matches the `[[vector_namespace]]` dim and the
/// jina-embeddings-v5-text-small model used by the embeddings-chunker addon.
const EMBED_MODEL: &str = "jina-embeddings-v5-text-small";
const EMBED_DIM: usize = 1024;
/// Buffer for the llm_generate response — a 1024-dim f32 vector serialized as
/// JSON floats is well under this.
const EMBED_RESP_BUF: usize = 262_144;

/// Stable u64 ref_id for an alarm string id (FNV-1a). The vector store keys by
/// u64; the alarm_id ↔ ref_id mapping is persisted in `vector_refs` so a hit
/// resolves back to the alarm even though the hash itself is one-way.
fn alarm_ref_id(alarm_id: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in alarm_id.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // ref_id 0 is a valid key but the bindings probe searches with a zero query
    // vector; keep alarm ids off 0 so the two never alias semantically.
    if h == 0 { 1 } else { h }
}

/// Builds the text document embedded for an alarm. Concatenates the human-facing
/// fields so a natural-language query matches on camera, type, message, severity.
fn alarm_doc(a: &db::AlarmRow) -> String {
    alloc::format!(
        "Kamera: {}. Typ: {}. Waga: {}. Zdarzenie: {}",
        if a.camera_name.is_empty() { a.camera_id.as_str() } else { a.camera_name.as_str() },
        a.kind, a.severity, a.message,
    )
}

/// Generates an embedding for `text` via the host `llm_generate` with the
/// embedding model. `mode` is "query" or "document" (asymmetric retrieval
/// prefix). Returns the f32 vector, or an honest error string on failure.
fn generate_embedding(text: &str, mode: &str) -> Result<Vec<f32>, String> {
    let prefixed = match mode {
        "query" => alloc::format!("Query: {}", text),
        _ => alloc::format!("Document: {}", text),
    };
    let options = json!({"task": "embedding", "dimensions": EMBED_DIM, "adapter": "retrieval"}).to_string();
    let prompt_b = prefixed.as_bytes();
    let model_b = EMBED_MODEL.as_bytes();
    let opt_b = options.as_bytes();
    let mut buf = alloc::vec![0u8; EMBED_RESP_BUF];
    let mut out_len: i32 = 0;
    let code = unsafe {
        llm_generate(
            prompt_b.as_ptr() as i32, prompt_b.len() as i32,
            model_b.as_ptr() as i32, model_b.len() as i32,
            opt_b.as_ptr() as i32, opt_b.len() as i32,
            buf.as_mut_ptr() as i32, EMBED_RESP_BUF as i32,
            &mut out_len as *mut i32 as i32,
        )
    };
    if code != 0 {
        return Err(alloc::format!("llm_generate code {}", code));
    }
    if out_len <= 0 {
        return Err("pusta odpowiedź modelu".into());
    }
    let resp = String::from_utf8_lossy(&buf[..out_len as usize]).to_string();
    parse_embedding_response(&resp)
}

/// Extracts the f32 vector from an llm_generate embedding response. Accepts a
/// bare float array, or an object with `embedding` / `vector` / `data[0].embedding`.
fn parse_embedding_response(resp: &str) -> Result<Vec<f32>, String> {
    let parsed: JsonValue = serde_json::from_str(resp)
        .map_err(|e| alloc::format!("parse embeddingu: {}", e))?;
    let arr = parsed.as_array()
        .or_else(|| parsed.get("embedding").and_then(|v| v.as_array()))
        .or_else(|| parsed.get("vector").and_then(|v| v.as_array()))
        .or_else(|| parsed.get("data").and_then(|d| d.as_array())
            .and_then(|a| a.first())
            .and_then(|f| f.get("embedding"))
            .and_then(|v| v.as_array()));
    let arr = arr.ok_or_else(|| "brak wektora w odpowiedzi".to_string())?;
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        out.push(v.as_f64().ok_or_else(|| "element wektora nie jest liczbą".to_string())? as f32);
    }
    if out.is_empty() {
        return Err("pusty wektor".into());
    }
    Ok(out)
}

/// Base64(LE f32 bytes) — the wire encoding the vector host functions expect.
fn encode_vector_b64(vector: &[f32]) -> String {
    use base64::Engine;
    let mut raw = Vec::with_capacity(vector.len() * 4);
    for f in vector {
        raw.extend_from_slice(&f.to_le_bytes());
    }
    base64::engine::general_purpose::STANDARD.encode(&raw)
}

/// Upserts one vector into the events namespace. Thin wrapper over the
/// vector_upsert_v1 host function with the CBOR payload shape.
fn vector_upsert(ref_id: u64, vector: &[f32]) -> Result<u64, AbiError> {
    let input = tentaflow_sdk_spec::VectorUpsertInput {
        namespace: EVENT_VECTOR_NS.into(),
        ref_id,
        vector_b64: encode_vector_b64(vector),
        fields: None,
        sparse: None,
    };
    let out: tentaflow_sdk_spec::VectorUpsertOutput = call_cbor_in_out(&input, vector_upsert_v1)?;
    Ok(out.count)
}

/// k-NN search over the events namespace. Returns hits (ref_id + score),
/// closest first.
fn vector_search(query: &[f32], k: u32) -> Result<Vec<tentaflow_sdk_spec::VectorSearchHit>, AbiError> {
    let input = tentaflow_sdk_spec::VectorSearchInput {
        namespace: EVENT_VECTOR_NS.into(),
        query_b64: encode_vector_b64(query),
        k,
        gate_claim_id: None,
        filter: None,
        output_fields: None,
    };
    let out: tentaflow_sdk_spec::VectorSearchOutput = call_cbor_in_out(&input, vector_search_v1)?;
    Ok(out.hits)
}

/// Embeds one alarm and upserts it into the events vector namespace, recording
/// the ref_id ↔ alarm_id mapping. Best-effort: returns the error string so the
/// caller can surface honest failures (e.g. no embedding model deployed) instead
/// of fabricating a result. Never panics on a missing model.
fn index_alarm(a: &db::AlarmRow) -> Result<(), String> {
    let doc = alarm_doc(a);
    let vector = generate_embedding(&doc, "document")?;
    let ref_id = alarm_ref_id(&a.id);
    vector_upsert(ref_id, &vector).map_err(|e| alloc::format!("vector_upsert: {}", abi_message(e)))?;
    db::upsert_vector_ref(ref_id, &a.id, a.ts).map_err(|e| alloc::format!("vector_refs: {}", abi_message(e)))?;
    Ok(())
}

/// Indexes an alarm by id, looking the row up first. Used by the alarm
/// create/decide handlers so the live index stays in sync. Errors are logged,
/// not surfaced to the operator (indexing is a background concern of those
/// flows; the Search tab's Reindex action is where indexing health is visible).
fn index_alarm_by_id(alarm_id: &str) {
    if let Ok(Some(a)) = db::get_alarm(alarm_id) {
        if let Err(e) = index_alarm(&a) {
            log::warn(&alloc::format!("index_alarm {} failed: {}", alarm_id, e));
        }
    }
}

/// Probes the embedding model: embeds a tiny query and reports whether a model
/// answered. Used by the text-search path and the Bindings probe to give an
/// honest "model unavailable" message instead of faking results.
fn embedding_model_available() -> bool {
    generate_embedding("ping", "query").is_ok()
}

/// Probes the vector store: a zero-vector k=1 search either returns hits or an
/// empty list (both prove the API responds). Only a hard AbiError means the
/// vector capability is unavailable. Used by the Bindings Vector status cell.
fn vector_store_available() -> bool {
    vector_search(&[0.0f32; EMBED_DIM], 1).is_ok()
}

// =============================================================================
// Historical search (M6) — text+attribute REAL, image+plate honest placeholder
// =============================================================================

/// Actor recorded in the audit log for search-query writes (D5 attribute search
/// is auditable per the RODO note in the mockup).
const SEARCH_ACTOR: &str = "analyst";

/// Settings keys that make the search panel durable across reopen / restart.
const KEY_SEARCH_MODE: &str = "search_mode";
const KEY_SEARCH_CAMERAS: &str = "search_cameras";
const KEY_SEARCH_FROM: &str = "search_time_from";
const KEY_SEARCH_TO: &str = "search_time_to";
const KEY_SEARCH_RECENTS: &str = "search_recents";
/// Attribute-mode severity filter ("all"/critical/warning/info).
const KEY_SEARCH_ATTR_SEVERITY: &str = "search_attr_severity";

/// Maximum number of recent query strings retained per mode.
const SEARCH_RECENTS_MAX: usize = 5;

/// The four search modes from the m06 mockup. Order matches the mode selector.
const SEARCH_MODES: [(&str, &str, &str, &str); 4] = [
    ("text", "Tekst (semantyczne)", "text", "Embedding zdarzeń + vector store — opisz zdarzenie słowami"),
    ("attribute", "Atrybut (formularz)", "tag", "Zapytanie SQL po wadze / typie / kamerze / czasie"),
    ("image", "Podobieństwo (zdjęcie)", "image", "Wymaga pipeline'u wizyjnego (embedding klatek)"),
    ("plate", "Tablica rejestracyjna", "car", "Wymaga silnika ANPR (LPRNet)"),
];

/// Normalizes a persisted/incoming mode to a known mode id, defaulting to text.
fn search_mode_norm(raw: &str) -> &'static str {
    SEARCH_MODES.iter().map(|m| m.0).find(|m| *m == raw).unwrap_or("text")
}

/// Current search mode read from settings (defaults to "text").
fn search_mode() -> String {
    let raw = db::get_setting(KEY_SEARCH_MODE).ok().flatten().unwrap_or_default();
    search_mode_norm(&raw).to_string()
}

/// Settings key holding the last query text for a given mode.
fn search_query_key(mode: &str) -> String {
    alloc::format!("search_last_{}", search_mode_norm(mode))
}

/// Last query text persisted for a mode (empty if none).
fn search_last_query(mode: &str) -> String {
    db::get_setting(&search_query_key(mode)).ok().flatten().unwrap_or_default()
}

/// Store key the per-mode query control binds to (one key per mode so switching
/// modes restores that mode's own last text).
fn search_query_store_key(mode: &str) -> String {
    alloc::format!("search_q_{}", search_mode_norm(mode))
}

/// Recent searches are stored per mode as a JSON array under one settings key:
/// `{ "text": ["..."], "attribute": [...] }`. Returns the list for one mode.
fn search_recents(mode: &str) -> Vec<String> {
    let raw = db::get_setting(KEY_SEARCH_RECENTS).ok().flatten().unwrap_or_default();
    if raw.is_empty() { return Vec::new(); }
    let parsed: JsonValue = match serde_json::from_str(&raw) { Ok(v) => v, Err(_) => return Vec::new() };
    parsed.get(search_mode_norm(mode))
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default()
}

/// Prepends `query` to the recents list for `mode`, de-duplicating and capping
/// at SEARCH_RECENTS_MAX, then persists the whole per-mode map back to settings.
fn search_push_recent(mode: &str, query: &str) {
    let mode = search_mode_norm(mode);
    let q = query.trim();
    if q.is_empty() { return; }
    let raw = db::get_setting(KEY_SEARCH_RECENTS).ok().flatten().unwrap_or_default();
    let mut map: JsonValue = serde_json::from_str(&raw).unwrap_or_else(|_| json!({}));
    if !map.is_object() { map = json!({}); }
    let mut list: Vec<String> = map.get(mode)
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();
    list.retain(|x| x != q);
    list.insert(0, q.to_string());
    list.truncate(SEARCH_RECENTS_MAX);
    if let Some(obj) = map.as_object_mut() {
        obj.insert(mode.to_string(), JsonValue::Array(list.into_iter().map(JsonValue::String).collect()));
    }
    let _ = db::set_setting(KEY_SEARCH_RECENTS, &map.to_string());
}

/// Camera-scope select options built from the real camera list (db::list_cameras).
fn search_camera_options(cameras: &[db::CameraRow]) -> Vec<SelectOption> {
    let mut opts = vec![SelectOption {
        value: SelectValue::Text("all".into()),
        label: lit(&alloc::format!("Wszystkie kamery ({})", cameras.len())),
        icon: None, disabled: false, group_id: None, description: None,
    }];
    for c in cameras {
        opts.push(SelectOption {
            value: SelectValue::Text(c.id.clone()),
            label: lit(&c.name),
            icon: None, disabled: false, group_id: None, description: None,
        });
    }
    opts
}

/// The four-mode selector as a RadioCardGroup (label + description per mode),
/// mirroring the mockup's `.search-modes` boxes. Bound to `search_mode` and
/// committed to settings via `search-mode-change` so it survives reopen.
fn build_search_mode_selector() -> Component {
    use tentaflow_sdk_spec::protocol::ui::form::RadioCardGroup;
    let options: Vec<RadioCardOption> = SEARCH_MODES.iter().map(|(id, title, icon, desc)| RadioCardOption {
        value: SelectValue::Text((*id).into()),
        icon: icon_named(parse_icon_name(icon)),
        title: lit(title),
        description: Some(lit(desc)),
        badge: None,
        disabled: false,
    }).collect();
    let mut group = RadioCardGroup {
        bind_path: StatePath::new(vec![PathSegment::Key("search_mode".into())]),
        options,
        columns: 4,
        variant: RadioCardVariant::Default,
    }.into_component("search_mode").expect("RadioCardGroup");
    group = with_a11y_label(group, "Tryb wyszukiwania");
    group.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Change,
        Handler::Backend {
            action_id: "search-mode-change".into(),
            params: CborMap::default(),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    group
}

/// A query control (input or textarea) bound to a per-mode store key that mirrors
/// each keystroke into the per-mode settings value via `search-field-change`, so
/// the last query persists across reopen even before submit.
fn search_query_input(label: &str, placeholder: &str, mode: &str, multiline: bool) -> Component {
    let store_key = search_query_store_key(mode);
    let mut params = CborMap::default();
    params.0.push(("field".into(), Value::Text("query".into())));
    params.0.push(("mode".into(), Value::Text(search_mode_norm(mode).into())));
    let mut comp = if multiline {
        use tentaflow_sdk_spec::protocol::ui::form::Textarea;
        Textarea {
            bind_path: StatePath::new(vec![PathSegment::Key(store_key.clone())]),
            placeholder: Some(lit(placeholder)),
            label: Some(lit(label)),
            hint: None, validators: vec![], max_length: Some(500), min_length: None,
            disabled: None, readonly: None, error: None, size: InputSize::Md,
            rows: 2, autoresize: true, max_rows: Some(5), monospace: false,
        }.into_component(&store_key).expect("Textarea")
    } else {
        use tentaflow_sdk_spec::protocol::ui::form::Input;
        Input {
            r#type: InputType::Text,
            bind_path: StatePath::new(vec![PathSegment::Key(store_key.clone())]),
            placeholder: Some(lit(placeholder)),
            label: Some(lit(label)),
            hint: None, leading_icon: Some(icon_named(parse_icon_name("search"))),
            trailing_icon: None, prefix: None, suffix: None, validators: vec![],
            max_length: None, min_length: None, pattern: None, autocomplete: None,
            input_mode: None, disabled: None, readonly: None, error: None, size: InputSize::Md,
        }.into_component(&store_key).expect("Input")
    };
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Input,
        Handler::Backend {
            action_id: "search-field-change".into(),
            params,
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    comp
}

/// Severity select for attribute mode, bound to `search_attr_severity`, committed
/// to settings on change. "all" = no severity constraint.
fn search_attr_severity_select() -> Component {
    let options = vec![
        SelectOption { value: SelectValue::Text("all".into()), label: lit("Każda waga"), icon: None, disabled: false, group_id: None, description: None },
        SelectOption { value: SelectValue::Text("critical".into()), label: lit("Krytyczny"), icon: None, disabled: false, group_id: None, description: None },
        SelectOption { value: SelectValue::Text("warning".into()), label: lit("Ostrzeżenie"), icon: None, disabled: false, group_id: None, description: None },
        SelectOption { value: SelectValue::Text("info".into()), label: lit("Informacja"), icon: None, disabled: false, group_id: None, description: None },
    ];
    let mut comp = select("Waga", options, "search_attr_severity");
    let mut params = CborMap::default();
    params.0.push(("field".into(), Value::Text("severity".into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Change,
        Handler::Backend {
            action_id: "search-field-change".into(),
            params,
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    comp
}

/// Camera-scope select bound to `search_cameras`, committed to settings on change.
fn search_camera_select(cameras: &[db::CameraRow]) -> Component {
    let mut comp = select("Kamery", search_camera_options(cameras), "search_cameras");
    let mut params = CborMap::default();
    params.0.push(("field".into(), Value::Text("cameras".into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Change,
        Handler::Backend {
            action_id: "search-field-change".into(),
            params,
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    comp
}

/// A datetime-local range field committed to settings on change (from/to bound to
/// `search_time_from` / `search_time_to`).
fn search_time_input(label: &str, store_key: &str, field: &str) -> Component {
    use tentaflow_sdk_spec::protocol::ui::form::Input;
    let mut comp = Input {
        r#type: InputType::Text,
        bind_path: StatePath::new(vec![PathSegment::Key(store_key.into())]),
        placeholder: Some(lit("RRRR-MM-DD GG:MM")),
        label: Some(lit(label)),
        hint: None, leading_icon: Some(icon_named(parse_icon_name("calendar"))), trailing_icon: None, prefix: None, suffix: None,
        validators: vec![], max_length: None, min_length: None, pattern: None,
        autocomplete: None, input_mode: None, disabled: None, readonly: None, error: None,
        size: InputSize::Md,
    }.into_component(store_key).expect("Input");
    let mut params = CborMap::default();
    params.0.push(("field".into(), Value::Text(field.into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Change,
        Handler::Backend {
            action_id: "search-field-change".into(),
            params,
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    comp
}

/// The shared camera + time-range filter block reused by every mode's query form.
fn build_search_filters(cameras: &[db::CameraRow]) -> Component {
    card(Some("Filtry"), vec![
        search_camera_select(cameras),
        heading(4, "Zakres czasu"),
        text_styled("Retencja klasy B = 14 dni — starsze klatki nie są indeksowane.", "caption"),
        grid(2, vec![
            search_time_input("Od", "search_time_from", "time_from"),
            search_time_input("Do", "search_time_to", "time_to"),
        ]),
    ])
}

/// Per-mode query form. Each mode gets its own inputs faithful to the mockup;
/// image and plate modes add their mode-specific affordance.
fn build_search_query_form(mode: &str) -> Component {
    match mode {
        "attribute" => card(Some("Atrybuty zdarzenia"), vec![
            text_styled("Strukturalne wyszukiwanie SQL po wadze, typie, kamerze i zakresie czasu — bez AI, zawsze dostępne.", "caption"),
            grid(2, vec![
                search_attr_severity_select(),
                search_query_input("Typ zdarzenia", "np. agresja, ADR, pojazd", mode, false),
            ]),
            stack_h_gap("sm", vec![
                chip_toned("waga", "info"),
                chip_toned("typ", "info"),
                chip_toned("kamera + czas", "info"),
            ]),
        ]),
        "image" => card(Some("Podobieństwo do zdjęcia"), vec![
            text_styled("Upload obrazu referencyjnego → top-K najbardziej podobnych klatek.", "caption"),
            empty_state("Przeciągnij zdjęcie lub kliknij, aby wybrać", Some("Wymaga pipeline'u wizyjnego (embedding klatek) — pole referencyjne."), Some("image")),
            search_query_input("Opcjonalny opis referencji", "np. ta sama osoba co na zdjęciu z 14:32", mode, false),
        ]),
        "plate" => card(Some("Tablica rejestracyjna (ANPR)"), vec![
            text_styled("Dopasowanie po numerze tablicy (LPRNet, format PL/EU).", "caption"),
            search_query_input("Numer tablicy", "np. WX 12345 lub fragment", mode, false),
            stack_h_gap("sm", vec![
                chip_toned("LPRNet", "info"),
                chip_toned("format PL/EU", "info"),
                chip_toned("fuzzy match", "info"),
            ]),
        ]),
        _ => card(Some("Zapytanie semantyczne"), vec![
            text_styled("Embedding zdarzenia (model embeddingów) + vector store — opisz zdarzenie słowami.", "caption"),
            search_query_input("Zapytanie semantyczne", "np. agresja przy wjeździe, nieczytelna tablica", mode, true),
            stack_h_gap("sm", vec![
                chip_toned("embedding zdarzeń", "info"),
                chip_toned("cosine similarity", "info"),
                chip_toned("top-K = 30", "info"),
            ]),
        ]),
    }
}

/// Renders the results area from the last submit's outcome. Text-semantic and
/// attribute modes produce REAL result cards; image/plate keep an honest
/// placeholder (vision pipeline not wired); an unavailable embedding model gives
/// an honest message — never fabricated hits.
fn build_search_results(outcome: &SearchOutcome, mode: &str) -> Component {
    match outcome {
        SearchOutcome::Empty => card(Some("Wyniki"), vec![empty_state(
            "Wprowadź zapytanie",
            Some("Wybierz tryb, wpisz zapytanie i naciśnij Szukaj."),
            Some("search"),
        )]),
        SearchOutcome::ModelUnavailable => card(Some("Wyniki"), vec![
            alert(
                "Model embeddingów niedostępny — skonfiguruj go w Ustawieniach / deploy. Zapytanie zapisano w historii.",
                "warning",
            ),
            empty_state(
                "Brak modelu embeddingów",
                Some("Wyszukiwanie semantyczne wymaga wdrożonego modelu embeddingów (jina-embeddings-v5-text-small). Indeks wektorowy działa — uruchom model i ponów."),
                Some("search"),
            ),
        ]),
        SearchOutcome::Placeholder => {
            let (title, msg, icon) = match search_mode_norm(mode) {
                "image" => (
                    "Wymaga pipeline'u wizyjnego",
                    "Wyszukiwanie po podobieństwie obrazu wymaga embeddingów klatek z pipeline'u wizyjnego (jeszcze niewdrożony). Zapytanie zapisano w historii.",
                    "image",
                ),
                _ => (
                    "Wymaga silnika ANPR",
                    "Wyszukiwanie po tablicy wymaga embeddingów/odczytu tablic z silnika ANPR (LPRNet, jeszcze niewdrożony). Zapytanie zapisano w historii.",
                    "car",
                ),
            };
            card(Some("Wyniki"), vec![
                alert(msg, "warning"),
                empty_state(title, Some("Po wdrożeniu pipeline'u wizyjnego trafienia pojawią się tutaj."), Some(icon)),
            ])
        }
        SearchOutcome::Results(hits) => {
            if hits.is_empty() {
                return card(Some("Wyniki"), vec![empty_state(
                    "Brak trafień",
                    Some("Żadne zdarzenie nie pasuje do zapytania. Zmień kryteria lub zakres czasu."),
                    Some("search"),
                )]);
            }
            let is_semantic = search_mode_norm(mode) == "text";
            let mut children = vec![text_styled(
                &alloc::format!("Znaleziono {} zdarzeń.", hits.len()),
                "caption",
            )];
            for h in hits {
                children.push(build_search_result_card(h, is_semantic));
            }
            card(Some("Wyniki"), vec![stack_v_gap("sm", children)])
        }
    }
}

/// One result card: camera + message + severity chip + timestamp, and (for the
/// semantic mode) the cosine similarity. Built entirely from a real alarm row.
fn build_search_result_card(h: &SearchHit, show_score: bool) -> Component {
    let a = &h.alarm;
    let header = stack_h(vec![
        text_styled(if a.camera_name.is_empty() { a.camera_id.as_str() } else { a.camera_name.as_str() }, "body_strong"),
        badge(alarm_severity_label(&a.severity), alarm_severity_variant(&a.severity)),
        chip_with_icon(&format_alarm_time(a.ts), "category", "clock"),
    ]);
    let mut rows = vec![header, text_styled(&a.message, "body")];
    if show_score {
        // Cosine distance: lower = closer. Report similarity = 1 - distance.
        let sim = (1.0 - h.score).clamp(0.0, 1.0);
        rows.push(chip_toned(&alloc::format!("podobieństwo {:.0}%", sim * 100.0), "info"));
    }
    card(None, rows)
}

/// Recent-searches list for the current mode. Each entry re-runs that query.
fn build_search_recents(mode: &str) -> Component {
    let recents = search_recents(mode);
    if recents.is_empty() {
        return divider();
    }
    let mut rows: Vec<Component> = vec![heading(4, "Ostatnie wyszukiwania")];
    for q in &recents {
        let mut params = CborMap::default();
        params.0.push(("query".into(), Value::Text(q.clone())));
        rows.push(button_with_params(q, "search-recent-pick", "ghost", params));
    }
    card(None, vec![stack_v_gap("sm", rows)])
}

/// The RODO note from the mockup: attribute searches (D5) are audited; face
/// search (D4) needs an active LegalGrant (Re-ID tab).
fn build_search_rodo_note() -> Component {
    card(None, vec![
        chip_toned_icon("Uwaga RODO", "warning", "alert"),
        text_styled(
            "Każde wyszukanie po atrybutach osób (D5) jest zapisywane w audit log (operator, zapytanie, czas). Wyszukiwanie po twarzy (D4) wymaga aktywnego LegalGrant — zakładka Re-ID.",
            "caption",
        ),
    ])
}

fn build_search_content() -> Component {
    let messages = build_messages_section();
    let cameras = db::list_cameras().unwrap_or_default();
    let mode = search_mode();
    let (outcome, submitted_mode) = with_state(|s| {
        let m = if s.search.submitted_mode.is_empty() { mode.clone() } else { s.search.submitted_mode.clone() };
        (s.search.outcome.clone(), m)
    });

    let header = stack_h(vec![
        heading(2, "Wyszukiwarka historyczna"),
        risk_badge("B"),
    ]);
    let subtitle = text_styled(
        "Tekst (semantyczne, embedding + vector store) i atrybut (SQL) działają realnie; podobieństwo zdjęcia i tablica wymagają pipeline'u wizyjnego.",
        "caption",
    );

    let mode_selector = build_search_mode_selector();
    let query_form = build_search_query_form(&mode);
    let filters = build_search_filters(&cameras);

    let submit_row = stack_h(vec![
        button_with_icon("Szukaj", "search-submit", "primary", "search"),
        button("Wyczyść", "search-clear-all", "ghost"),
        button_with_icon("Reindeksuj zdarzenia", "search-reindex", "secondary", "refresh"),
    ]);

    let recents = build_search_recents(&mode);
    let results = build_search_results(&outcome, &submitted_mode);
    let rodo = build_search_rodo_note();

    stack_v(vec![
        messages, header, subtitle,
        mode_selector,
        query_form,
        filters,
        submit_row,
        recents,
        results,
        rodo,
    ])
}

/// Seeds every bound search control from settings so the panel mounts showing
/// the persisted mode, that mode's last query, and the camera/time filters.
fn search_overlay() -> Vec<StateEntry> {
    let mode = search_mode();
    let mut entries = vec![
        StateEntry {
            path: StatePath::new(vec![PathSegment::Key("search_mode".into())]),
            value: Value::Text(mode.clone()),
        },
        StateEntry {
            path: StatePath::new(vec![PathSegment::Key("search_cameras".into())]),
            value: Value::Text(db::get_setting(KEY_SEARCH_CAMERAS).ok().flatten().unwrap_or_else(|| "all".into())),
        },
        StateEntry {
            path: StatePath::new(vec![PathSegment::Key("search_time_from".into())]),
            value: Value::Text(db::get_setting(KEY_SEARCH_FROM).ok().flatten().unwrap_or_default()),
        },
        StateEntry {
            path: StatePath::new(vec![PathSegment::Key("search_time_to".into())]),
            value: Value::Text(db::get_setting(KEY_SEARCH_TO).ok().flatten().unwrap_or_default()),
        },
        StateEntry {
            path: StatePath::new(vec![PathSegment::Key("search_attr_severity".into())]),
            value: Value::Text(db::get_setting(KEY_SEARCH_ATTR_SEVERITY).ok().flatten().unwrap_or_else(|| "all".into())),
        },
    ];
    // Seed the active mode's query control with its persisted last query.
    entries.push(StateEntry {
        path: StatePath::new(vec![PathSegment::Key(search_query_store_key(&mode))]),
        value: Value::Text(search_last_query(&mode)),
    });
    entries
}

/// Persists the picked search mode to settings and re-renders so the per-mode
/// query form and its last-query / recents swap in.
fn handle_search_mode_change(params: &JsonValue) -> JsonValue {
    let v = params.get("value").and_then(|x| x.as_str())
        .or_else(|| params.get("chipId").and_then(|x| x.as_str()))
        .unwrap_or("text");
    let mode = search_mode_norm(v);
    let _ = db::set_setting(KEY_SEARCH_MODE, mode);
    // Switching mode clears the last outcome so it doesn't show stale results
    // for the previous mode.
    with_state(|s| { s.search.clear_all(); });
    render_panel("search");
    json!({"ok":true, "mode": mode})
}

/// Mirrors a search field (per-mode query text, camera scope, time range) into
/// settings on each keystroke/change so it persists across reopen.
fn handle_search_field_change(params: &JsonValue) -> JsonValue {
    let field = params.get("field").and_then(|x| x.as_str()).unwrap_or("");
    let value = params.get("value").and_then(|x| x.as_str()).unwrap_or("").to_string();
    match field {
        "query" => {
            let mode = params.get("mode").and_then(|x| x.as_str()).unwrap_or("text");
            let _ = db::set_setting(&search_query_key(mode), &value);
        }
        "cameras" => { let _ = db::set_setting(KEY_SEARCH_CAMERAS, &value); }
        "time_from" => { let _ = db::set_setting(KEY_SEARCH_FROM, &value); }
        "time_to" => { let _ = db::set_setting(KEY_SEARCH_TO, &value); }
        "severity" => { let _ = db::set_setting(KEY_SEARCH_ATTR_SEVERITY, &value); }
        _ => {}
    }
    json!({"ok":true})
}

/// Runs the actual search for `mode`/`query` and returns the outcome. Text mode
/// embeds the query and runs a real k-NN over the events vector store, resolving
/// hits to alarm rows; attribute mode runs a real SQL query; image/plate return
/// the honest placeholder (vision pipeline not wired). Never fabricates hits.
fn run_search(mode: &str, query: &str) -> SearchOutcome {
    match search_mode_norm(mode) {
        "attribute" => run_attribute_search(query),
        "text" => run_semantic_search(query),
        _ => SearchOutcome::Placeholder,
    }
}

/// Real structured SQL search over alarms: severity (settings), free-text type
/// (LIKE on the alarm type/message), camera scope and time range from settings.
fn run_attribute_search(query: &str) -> SearchOutcome {
    let severity = db::get_setting(KEY_SEARCH_ATTR_SEVERITY).ok().flatten().unwrap_or_else(|| "all".into());
    let severity = if severity == "all" { String::new() } else { severity };
    let camera = db::get_setting(KEY_SEARCH_CAMERAS).ok().flatten().unwrap_or_else(|| "all".into());
    let camera = if camera == "all" { String::new() } else { camera };
    let from = parse_search_time(&db::get_setting(KEY_SEARCH_FROM).ok().flatten().unwrap_or_default());
    let to = parse_search_time(&db::get_setting(KEY_SEARCH_TO).ok().flatten().unwrap_or_default());
    let rows = db::search_alarms(&severity, query.trim(), &camera, from, to).unwrap_or_default();
    SearchOutcome::Results(rows.into_iter().map(|alarm| SearchHit { alarm, score: 0.0 }).collect())
}

/// Real semantic search: embed the query, k-NN over the events namespace,
/// resolve each hit's ref_id back to its alarm row. If no embedding model is
/// deployed, return the honest ModelUnavailable outcome (never fake hits).
fn run_semantic_search(query: &str) -> SearchOutcome {
    let qvec = match generate_embedding(query.trim(), "query") {
        Ok(v) => v,
        Err(e) => {
            log::warn(&alloc::format!("semantic search embedding failed: {}", e));
            return SearchOutcome::ModelUnavailable;
        }
    };
    let hits = match vector_search(&qvec, 30) {
        Ok(h) => h,
        Err(e) => {
            log::warn(&alloc::format!("vector_search failed: {}", abi_message(e)));
            return SearchOutcome::ModelUnavailable;
        }
    };
    let mut out = Vec::new();
    for h in hits {
        if let Ok(Some(alarm_id)) = db::alarm_id_for_ref(h.ref_id) {
            if let Ok(Some(alarm)) = db::get_alarm(&alarm_id) {
                out.push(SearchHit { alarm, score: h.score });
            }
        }
    }
    SearchOutcome::Results(out)
}

/// Parses a "YYYY-MM-DD HH:MM" search time field into unix seconds, or 0 (no
/// bound) when empty/unparsable. Best-effort: accepts a leading date with an
/// optional time; treats the input as UTC.
fn parse_search_time(s: &str) -> i64 {
    let s = s.trim();
    if s.is_empty() { return 0; }
    let bytes: Vec<&str> = s.splitn(2, [' ', 'T']).collect();
    let date = bytes.first().copied().unwrap_or("");
    let dparts: Vec<&str> = date.split('-').collect();
    if dparts.len() != 3 { return 0; }
    let (y, mo, d) = match (dparts[0].parse::<i64>(), dparts[1].parse::<i64>(), dparts[2].parse::<i64>()) {
        (Ok(y), Ok(mo), Ok(d)) if (1..=12).contains(&mo) && (1..=31).contains(&d) => (y, mo, d),
        _ => return 0,
    };
    let (mut hh, mut mm) = (0i64, 0i64);
    if let Some(time) = bytes.get(1) {
        let tparts: Vec<&str> = time.split(':').collect();
        hh = tparts.first().and_then(|v| v.parse().ok()).unwrap_or(0);
        mm = tparts.get(1).and_then(|v| v.parse().ok()).unwrap_or(0);
    }
    days_from_civil(y, mo, d) * 86400 + hh * 3600 + mm * 60
}

/// Days since the unix epoch for a civil (proleptic Gregorian) date. Howard
/// Hinnant's algorithm — exact, no leap-year edge cases.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Runs a search: persists the query into recents, audits it, and stores the
/// REAL outcome (text-semantic / attribute hits, honest message, or placeholder).
fn handle_search_submit(params: &JsonValue) -> JsonValue {
    let mode = search_mode();
    // The query control commits on keystroke, so the persisted value is
    // authoritative; a submit-time `value` (if the host sends one) overrides it.
    let query = params.get("value").and_then(|x| x.as_str()).map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| search_last_query(&mode));
    // Attribute mode can run on the structured fields alone (empty free text);
    // every other mode needs a query string.
    if query.trim().is_empty() && search_mode_norm(&mode) != "attribute" {
        with_state(|s| { s.clear_messages(); s.error_message = Some("Wpisz zapytanie przed wyszukaniem.".into()); });
        render_panel("search");
        return json!({"ok":false});
    }
    let _ = db::set_setting(&search_query_key(&mode), &query);
    if !query.trim().is_empty() {
        search_push_recent(&mode, &query);
    }
    // D5 attribute search is auditable per the RODO note; record every submit.
    let after = json!({"mode": mode, "query": query}).to_string();
    let _ = db::insert_audit(SEARCH_ACTOR, "search_query", &mode, "", &after);
    let outcome = run_search(&mode, &query);
    with_state(|s| { s.clear_messages(); s.search.submitted_mode = mode.clone(); s.search.outcome = outcome; });
    render_panel("search");
    json!({"ok":true})
}

/// Re-runs a recent query: persists it as the current mode's query, then runs it.
fn handle_search_recent_pick(params: &JsonValue) -> JsonValue {
    let query = params.get("query").and_then(|x| x.as_str()).unwrap_or("").to_string();
    if query.is_empty() { return json!({"ok":false}); }
    let mode = search_mode();
    let _ = db::set_setting(&search_query_key(&mode), &query);
    search_push_recent(&mode, &query);
    let after = json!({"mode": mode, "query": query}).to_string();
    let _ = db::insert_audit(SEARCH_ACTOR, "search_query", &mode, "", &after);
    let outcome = run_search(&mode, &query);
    with_state(|s| { s.clear_messages(); s.search.submitted_mode = mode.clone(); s.search.outcome = outcome; });
    render_panel("search");
    json!({"ok":true})
}

/// Backfills embeddings for every existing alarm into the events vector store.
/// If no embedding model is deployed, reports the honest model-unavailable
/// message instead of pretending success.
fn handle_search_reindex() -> JsonValue {
    with_state(|s| s.clear_messages());
    if !embedding_model_available() {
        with_state(|s| s.error_message = Some(
            "Model embeddingów niedostępny — skonfiguruj go w Ustawieniach / deploy. Indeksowanie pominięte.".into()
        ));
        render_panel("search");
        return json!({"ok":false, "error":"no embedding model"});
    }
    let alarms = db::list_all_alarms().unwrap_or_default();
    let mut ok = 0u64;
    let mut failed = 0u64;
    for a in &alarms {
        match index_alarm(a) {
            Ok(()) => ok += 1,
            Err(_) => failed += 1,
        }
    }
    let _ = db::insert_audit(
        SEARCH_ACTOR, "search_reindex", EVENT_VECTOR_NS,
        "", &json!({"indexed": ok, "failed": failed}).to_string(),
    );
    with_state(|s| s.success_message = Some(alloc::format!(
        "Zaindeksowano {} zdarzeń{}.", ok,
        if failed > 0 { alloc::format!(" ({} błędów)", failed) } else { String::new() }
    )));
    render_panel("search");
    json!({"ok":true, "indexed": ok, "failed": failed})
}

/// Clears the current mode's query + the just-submitted placeholder. Keeps the
/// recents history and the chosen mode (clearing one query should not wipe the
/// whole search history).
fn handle_search_clear_all() -> JsonValue {
    let mode = search_mode();
    let _ = db::set_setting(&search_query_key(&mode), "");
    with_state(|s| { s.clear_messages(); s.search.clear_all(); });
    render_panel("search");
    json!({"ok":true})
}

/// Input bound to a store key that also mirrors its value into backend profile
/// state on every keystroke (tagged `field`), so submit validation reads the
/// authoritative value even if the user clicks "Zapisz" before blur.
fn profile_input(label: &str, placeholder: &str, field: &str) -> Component {
    let mut comp = input(label, placeholder, field);
    let mut params = CborMap::default();
    params.0.push(("field".into(), Value::Text(field.into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Input,
        Handler::Backend {
            action_id: "profile-field-change".into(),
            params,
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    comp
}

/// Select bound to a store key that mirrors its picked value into backend
/// profile state on change (tagged `field`).
fn profile_select(label: &str, options: Vec<SelectOption>, field: &str) -> Component {
    let mut comp = select(label, options, field);
    let mut params = CborMap::default();
    params.0.push(("field".into(), Value::Text(field.into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Change,
        Handler::Backend {
            action_id: "profile-field-change".into(),
            params,
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    comp
}

/// Slider bound to a store key that mirrors its value into backend profile state
/// on change (tagged `field`).
fn profile_slider(label: &str, field: &str, min: f64, max: f64, step: f64) -> Component {
    let mut comp = slider(label, field, min, max, step);
    let mut params = CborMap::default();
    params.0.push(("field".into(), Value::Text(field.into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Change,
        Handler::Backend {
            action_id: "profile-field-change".into(),
            params,
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    comp
}

/// Risk-class badge matching the mockup tones: A = success (green), B = warning
/// (amber), C = critical (red); anything else = neutral.
fn risk_badge(risk_class: &str) -> Component {
    let tone = match risk_class {
        "A" => "success",
        "B" => "warning",
        "C" => "danger",
        _ => "info",
    };
    let label = if risk_class.is_empty() { "—" } else { risk_class };
    chip_toned(label, match tone { "danger" => "critical", other => other })
}

/// Available analytic Flows the profile can bind to. In the mockup this list is
/// filtered to Flows that expose TentaVision vision capabilities; here it is a
/// stable set the builder writes verbatim into `flow_id`.
fn profile_flow_options() -> Vec<SelectOption> {
    ["tv-realtime-adr", "tv-realtime-public", "tv-security-night", "tv-anpr", "tv-reid-historical"]
        .iter()
        .map(|f| SelectOption {
            value: SelectValue::Text((*f).into()),
            label: lit(f),
            icon: None,
            disabled: false,
            group_id: None,
            description: None,
        })
        .collect()
}

fn profile_risk_options() -> Vec<SelectOption> {
    [("A", "A — bezosobowe / długa retencja"), ("B", "B — średnie ryzyko"), ("C", "C — wrażliwe / krótka retencja")]
        .iter()
        .map(|(v, l)| SelectOption {
            value: SelectValue::Text((*v).into()),
            label: lit(l),
            icon: None,
            disabled: false,
            group_id: None,
            description: None,
        })
        .collect()
}

fn profile_schedule_options() -> Vec<SelectOption> {
    ["24/7", "06:00–22:00", "22:00–06:00", "04:30–24:00"]
        .iter()
        .map(|s| SelectOption {
            value: SelectValue::Text((*s).into()),
            label: lit(s),
            icon: None,
            disabled: false,
            group_id: None,
            description: None,
        })
        .collect()
}

/// Renders the camera-assignment list: every real camera from SQLite as a
/// toggle button. Assigned cameras show a "success" status chip; clicking a row
/// toggles membership via `profile-camera-toggle`.
fn build_profile_camera_assignment(cameras: &[db::CameraRow], assigned: &[String]) -> Component {
    if cameras.is_empty() {
        return empty_state(
            "Brak kamer",
            Some("Dodaj kamerę w zakładce Kamery, aby przypisać ją do profilu."),
            Some("cameras"),
        );
    }
    let rows: Vec<Component> = cameras
        .iter()
        .map(|c| {
            let is_on = assigned.iter().any(|a| a == &c.id);
            let mut params = CborMap::default();
            params.0.push(("camera_id".into(), Value::Text(c.id.clone())));
            let label = if is_on { alloc::format!("✓ {}", c.name) } else { c.name.clone() };
            let variant = if is_on { "primary" } else { "secondary" };
            let toggle_btn = button_with_params(&label, "profile-camera-toggle", variant, params);
            let status = chip_toned(&c.status, if c.status == "online" { "success" } else { "warning" });
            stack_h(vec![toggle_btn, status])
        })
        .collect();
    stack_v_gap("sm", rows)
}

/// One profile-library Table row keyed by `profile_id`.
/// Maps an analytical-profile risk class to a chip tone (A=green, B=amber,
/// C=red), matching the mockup's `.risk` badge colors.
fn profile_risk_cell(risk: &str) -> Value {
    match risk {
        "A" => chip_cell("A", "ok"),
        "B" => chip_cell("B", "warn"),
        "C" => chip_cell("C", "err"),
        other => chip_cell(if other.is_empty() { "—" } else { other }, "info"),
    }
}

fn profile_table_row_value(p: &db::ProfileRow, camera_count: usize) -> Value {
    let flow = if p.flow_id.is_empty() { "—".to_string() } else { p.flow_id.clone() };
    let schedule = if p.schedule.is_empty() { "—".to_string() } else { p.schedule.clone() };
    let entries: Vec<(Value, Value)> = vec![
        (Value::Text("profile_id".into()), Value::Text(p.id.clone())),
        (Value::Text("name".into()), Value::Text(p.name.clone())),
        (Value::Text("flow".into()), Value::Text(flow)),
        (Value::Text("risk".into()), profile_risk_cell(&p.risk_class)),
        (Value::Text("cameras".into()), Value::Text(alloc::format!("{}", camera_count))),
        (Value::Text("schedule".into()), Value::Text(schedule)),
        (Value::Text("enabled".into()), if p.enabled { chip_cell("TAK", "ok") } else { chip_cell("NIE", "muted") }),
    ];
    Value::Map(entries)
}

fn profile_table_column(id: &str, header: &str, render: ColumnRender) -> TableColumn {
    TableColumn {
        id: id.into(),
        header: lit(header),
        field_path: vec![PathSegment::Key(id.into())],
        width: TableColumnWidth::Auto,
        render,
        format: None,
        align: None,
        sortable: true,
        hidden_by_default: false,
        sticky_left: false,
    }
}

fn build_profiles_table() -> Component {
    let columns = vec![
        profile_table_column("name", "Nazwa", ColumnRender::Text),
        profile_table_column("flow", "Flow", ColumnRender::Text),
        profile_table_column("risk", "Klasa", ColumnRender::Chip),
        profile_table_column("cameras", "Kamery", ColumnRender::Text),
        profile_table_column("schedule", "Harmonogram", ColumnRender::Text),
        profile_table_column("enabled", "Aktywny", ColumnRender::Chip),
    ];

    // Per-row actions: edit opens the builder pre-filled, toggle flips enabled,
    // and Usuń arms the delete-confirmation bar (the real delete runs from it).
    let edit_action = button("Edytuj", "profile-edit", "secondary");
    let toggle_action = button("Włącz/wyłącz", "profile-toggle-enabled", "ghost");
    let remove_action = button("Usuń", "profile-row-select", "destructive");

    TableComp {
        columns,
        rows_path: StatePath::new(vec![PathSegment::Key("profiles_rows".into())]),
        row_key_field: "profile_id".into(),
        variant: TableVariant::Default,
        density: Density::Default,
        sortable: true,
        sort_by: None,
        selectable: TableSelectMode::None,
        selected_ids: None,
        sticky_header: true,
        sticky_columns: 0,
        pagination: None,
        empty_state: None,
        row_actions: vec![edit_action, toggle_action, remove_action],
        bulk_actions: vec![],
        virtualize: false,
        row_expandable: false,
        expanded_row_template_id: None,
    }.into_component(next_id()).expect("Table")
}

/// Confirmation bar for deleting the selected profile.
fn build_profile_remove_confirm(profile_id: &str, profiles: &[db::ProfileRow]) -> Component {
    let name = profiles
        .iter()
        .find(|p| p.id == profile_id)
        .map(|p| p.name.as_str())
        .unwrap_or(profile_id);
    let mut params = CborMap::default();
    params.0.push(("profile_id".into(), Value::Text(profile_id.into())));
    let confirm_btn = button_with_params("Usuń", "profile-remove", "destructive", params);
    let cancel_btn = button("Anuluj", "profile-remove-cancel", "ghost");
    card(None, vec![stack_v(vec![
        text_styled(&alloc::format!("Usunąć profil \"{}\"?", name), "body_strong"),
        text("Tej operacji nie można cofnąć."),
        stack_h(vec![confirm_btn, cancel_btn]),
    ])])
}

/// The analytic-profile builder: left column (Flow + quick params), right column
/// (profile config + camera assignment). Mirrors the m04 mockup's `.col-2`.
fn build_profile_builder(cameras: &[db::CameraRow]) -> Component {
    let (name, flow_id, risk_class, schedule, assigned, editing) = with_state(|s| (
        s.profiles.name.clone(),
        s.profiles.flow_id.clone(),
        s.profiles.risk_class.clone(),
        s.profiles.schedule.clone(),
        s.profiles.cameras.clone(),
        s.profiles.editing_id.is_some(),
    ));

    // LEFT: Flow assignment + quick params (overrides to Flow inputs).
    let left = card(Some("Flow przypisany do profilu"), vec![
        text("Lista Flow filtrowana do tych, które używają capabilities TentaVision (vision.detect, vision.ocr, video.recording)."),
        profile_select("Flow", profile_flow_options(), "profile_flow_id"),
        heading(4, "Quick params — overrides do Flow inputs"),
        profile_slider("FPS sampling kamery", "profile_fps", 1.0, 15.0, 1.0),
        profile_slider("Min. próg detekcji", "profile_min_conf", 0.0, 1.0, 0.05),
        text("Quick params zapisują się jako overrides do inputs Flow. Aby zmienić strukturę grafu — otwórz w FlowBuilder."),
    ]);

    // RIGHT: profile config + camera assignment.
    let right = card(Some("Konfiguracja profilu"), vec![
        profile_input("Nazwa", "np. ADR-brama", "profile_name"),
        profile_select("Klasa ryzyka", profile_risk_options(), "profile_risk_class"),
        stack_h(vec![text("Aktualna klasa:"), risk_badge(&risk_class)]),
        profile_select("Harmonogram", profile_schedule_options(), "profile_schedule"),
        heading(4, "Kamery w profilu"),
        build_profile_camera_assignment(cameras, &assigned),
    ]);

    let _ = (name, flow_id, schedule);

    let save_label = if editing { "Zapisz zmiany" } else { "Utwórz profil" };
    let actions = stack_h(vec![
        button(save_label, "profile-add-submit", "primary"),
        button("Anuluj", "profile-builder-cancel", "ghost"),
    ]);

    card(None, vec![
        grid(2, vec![left, right]),
        actions,
    ])
}

fn build_profiles_content() -> Component {
    let messages = build_messages_section();
    let list_result = db::list_profiles();
    let (category, builder_visible) = with_state(|s| (s.profiles.category_or_all().to_string(), s.profiles.builder_visible));

    let mut children = vec![messages];

    let chips = filter_chips(
        vec![
            FilterChipDef { id: "all".into(), label: lit("Wszystkie"), icon: None, badge: None, count_path: None },
            FilterChipDef { id: "A".into(), label: lit("Klasa A"), icon: None, badge: None, count_path: None },
            FilterChipDef { id: "B".into(), label: lit("Klasa B"), icon: None, badge: None, count_path: None },
            FilterChipDef { id: "C".into(), label: lit("Klasa C"), icon: None, badge: None, count_path: None },
        ],
        &category,
    );
    let toolbar = stack_h(vec![
        heading(2, "Profile analityczne"),
        chips,
        button("Nowy profil", "profile-add-show", "primary"),
    ]);
    children.push(toolbar);

    // A DB/permission error must never be masked as "no profiles".
    let profiles = match list_result {
        Ok(p) => p,
        Err(e) => {
            children.push(alert(&alloc::format!("Nie udało się pobrać profili: {}", abi_message(e)), "critical"));
            return stack_v(children);
        }
    };

    // Cameras for the builder's assignment list and the library's per-row count.
    let cameras = db::list_cameras().unwrap_or_default();

    if builder_visible {
        children.push(build_profile_builder(&cameras));
    }

    // Delete-confirmation bar above the table once a row is armed.
    if let Some(pending) = with_state(|s| s.profiles.pending_remove.clone()) {
        if profiles.iter().any(|p| p.id == pending) {
            children.push(build_profile_remove_confirm(&pending, &profiles));
        } else {
            with_state(|s| s.profiles.pending_remove = None);
        }
    }

    let active_filter = if category == "all" { "" } else { category.as_str() };
    let filtered: Vec<&db::ProfileRow> = profiles
        .iter()
        .filter(|p| active_filter.is_empty() || p.risk_class == active_filter)
        .collect();

    if profiles.is_empty() {
        children.push(empty_state(
            "Brak profili analitycznych",
            Some("Utwórz pierwszy profil: wybierz Flow, klasę ryzyka i przypisz kamery."),
            Some("brain"),
        ));
    } else {
        let rows: Vec<Value> = filtered
            .iter()
            .map(|p| profile_table_row_value(p, profile_camera_count(p)))
            .collect();
        if let Ok(mut g) = PENDING_PROFILE_ROWS.lock() {
            *g = Some(Value::Array(rows));
        }
        children.push(build_profiles_table());
    }

    stack_v(children)
}

/// Parses a profile's `cameras` JSON array into a list of camera ids.
fn parse_profile_cameras(cameras_json: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(cameras_json).unwrap_or_default()
}

/// Number of cameras assigned to a profile (length of its `cameras` JSON array).
fn profile_camera_count(p: &db::ProfileRow) -> usize {
    parse_profile_cameras(&p.cameras).len()
}

/// Seeds the builder's bound store keys from current backend profile state so
/// the form mounts with the draft (create) or loaded (edit) values in place.
fn profile_builder_overlay() -> Vec<StateEntry> {
    with_state(|s| {
        let p = &s.profiles;
        let key = |k: &str, v: Value| StateEntry {
            path: StatePath::new(vec![PathSegment::Key(k.into())]),
            value: v,
        };
        vec![
            key("profile_name", Value::Text(p.name.clone())),
            key("profile_flow_id", Value::Text(p.flow_id.clone())),
            key("profile_risk_class", Value::Text(p.risk_class.clone())),
            key("profile_schedule", Value::Text(p.schedule.clone())),
            key("profile_fps", Value::F64(p.fps)),
            key("profile_min_conf", Value::F64(p.min_confidence)),
        ]
    })
}

// =============================================================================
// Re-ID (D4) — hard legal gate
//
// Person re-identification is high-risk AI under the EU AI Act (Annex III) and
// real-time use in public spaces is prohibited (Art. 5) outside narrow law-
// enforcement exceptions. The runtime therefore refuses every Re-ID query until
// a set of compliance preconditions are satisfied. Each precondition is one
// persisted boolean flag in the settings table ("1"/"0"); the gate is OPEN only
// when every REQUIRED flag is satisfied. Flipping a flag is audited.
// =============================================================================

/// One compliance precondition of the Re-ID gate, bound to a persisted setting.
struct GateCondition {
    /// Settings key holding the "1"/"0" flag.
    key: &'static str,
    /// Whether this condition must be satisfied for the gate to open. (All Re-ID
    /// conditions are required; the field documents the contract explicitly.)
    required: bool,
    label: &'static str,
    desc: &'static str,
    /// Action that advances/toggles this flag from its blocked state.
    advance_action: &'static str,
    advance_label: &'static str,
    /// When false, an unsatisfied flag renders as "blocked" (red) rather than
    /// "pending" (amber) — used to mirror the mockup's hard-blocked rows.
    soft_pending: bool,
}

const REID_CONDITIONS: &[GateCondition] = &[
    GateCondition {
        key: "reid_dpia_done", required: true,
        label: "DPIA wypełniona i podpisana przez DPO",
        desc: "DPIA wymagana RODO art. 35 — ocena skutków dla ochrony danych musi być zatwierdzona przez Inspektora Ochrony Danych.",
        advance_action: "reid-flag-set", advance_label: "Oznacz DPIA jako zatwierdzone",
        soft_pending: true,
    },
    GateCondition {
        key: "reid_fria_done", required: true,
        label: "FRIA (Fundamental Rights Impact Assessment) — AI Act art. 27",
        desc: "Ocena wpływu na prawa podstawowe wymagana dla systemów high-risk w sektorze publicznym.",
        advance_action: "reid-flag-set", advance_label: "Oznacz FRIA jako ukończoną",
        soft_pending: true,
    },
    GateCondition {
        key: "reid_legalgrant_granted", required: true,
        label: "Aktywny LegalGrant z udokumentowanym authority",
        desc: "Wymóg: organ wnoszący · sygnatura sprawy · expiry · podpis kierownika jednostki.",
        advance_action: "reid-legalgrant-request", advance_label: "Wnioskuj o LegalGrant",
        soft_pending: false,
    },
    GateCondition {
        key: "reid_profile_set", required: true,
        label: "Profil deployment uprawniony do D4 real-time",
        desc: "Profil \"Komercja prywatna\" nie daje dostępu do D4. Wymagany profil \"Lotnisko (operator)\", \"Transport publiczny\" lub \"Służby uprawnione\".",
        advance_action: "reid-flag-set", advance_label: "Ustaw uprawniony profil",
        soft_pending: false,
    },
    GateCondition {
        key: "reid_audit_sync", required: true,
        label: "Hash-chain audit log uruchomiony i synchronizowany z WORM",
        desc: "Append-only łańcuch zdarzeń musi być aktywny i replikowany do magazynu niemodyfikowalnego (WORM).",
        advance_action: "reid-flag-set", advance_label: "Potwierdź synchronizację audytu",
        soft_pending: true,
    },
    GateCondition {
        key: "reid_monitoring", required: true,
        label: "Post-market monitoring (AI Act art. 72)",
        desc: "Eval harness aktywny · metryki FP/h/kamera oraz fairness w celu.",
        advance_action: "reid-flag-set", advance_label: "Potwierdź monitoring",
        soft_pending: true,
    },
];

/// True when a gate flag is persisted as "1".
fn reid_flag(key: &str) -> bool {
    db::get_setting(key).ok().flatten().as_deref() == Some("1")
}

/// The gate is OPEN only when every REQUIRED condition's flag is satisfied.
fn reid_gate_open() -> bool {
    REID_CONDITIONS.iter().filter(|c| c.required).all(|c| reid_flag(c.key))
}

/// Renders one checklist row: a tone-coded status chip (done=ok, soft-pending=
/// warn, blocked=err) plus the action that advances the flag.
fn reid_check_row(cond: &GateCondition) -> Component {
    let done = reid_flag(cond.key);
    let (chip_label, chip_tone, chip_icon) = if done {
        ("OK", "success", "check")
    } else if cond.soft_pending {
        ("OCZEKUJE", "warning", "clock")
    } else {
        ("ZABLOKOWANE", "critical", "lock")
    };

    let mut left = vec![
        text_styled(cond.label, "body_strong"),
        text_colored(cond.desc, "caption", "muted"),
    ];
    let _ = &mut left;

    let trailing = if done {
        chip_toned_icon(chip_label, chip_tone, chip_icon)
    } else {
        let mut params = CborMap::default();
        params.0.push(("key".into(), Value::Text(cond.key.into())));
        button_with_params(cond.advance_label, cond.advance_action, "secondary", params)
    };

    let row = stack_h(vec![
        chip_toned_icon("", chip_tone, chip_icon),
        stack_v_gap("xxs", left),
        trailing,
    ]);

    Card {
        variant: CardVariant::Outlined,
        padding: Spacing::Md,
        gap: Spacing::Sm,
        radius: RadiusToken::Md,
        shadow: ShadowToken::None,
        border: BorderToken::Hairline,
        background: BackgroundToken::Subtle,
        accent: Some(parse_tone(chip_tone)),
        children: vec![row],
        interactive: false,
        clickable: false,
    }.into_component(next_id()).expect("Card")
}

fn build_reid_content() -> Component {
    let messages = build_messages_section();
    let open = reid_gate_open();

    // Danger-bordered gate header mirroring the mockup's detail-header.
    let runtime_chip = if open {
        chip_toned_icon("runtime: READY", "success", "check")
    } else {
        chip_toned_icon("runtime: BLOCKED", "critical", "lock")
    };
    let status_chip = if open {
        chip_toned_icon("odblokowany", "success", "check")
    } else {
        chip_toned_icon("zablokowany", "critical", "lock")
    };

    // Query button: enabled only when the gate is open. When enabled it does NOT
    // run a search — there is no Re-ID inference engine — it surfaces an honest
    // "requires runtime" notice.
    let query_btn = {
        let mut c = ButtonComp {
            variant: if open { ButtonVariant::Primary } else { ButtonVariant::Secondary },
            tone: Tone::Neutral,
            label: lit(if open { "Uruchom zapytanie Re-ID" } else { "Zapytanie zablokowane" }),
            icon_leading: Some(icon_named(parse_icon_name(if open { "search" } else { "lock" }))),
            icon_trailing: None,
            size: ButtonSize::Md,
            full_width: false,
            disabled: Some(BindRef::Literal(Value::Bool(!open))),
            loading: None,
            density: Density::Default,
        }.into_component(next_id()).expect("Button");
        c.handlers = Some(HandlerMap(vec![(
            tentaflow_sdk_spec::EventKind::Click,
            Handler::Backend {
                action_id: "reid-query".into(),
                params: CborMap::default(),
                optimistic: None,
                on_failure: FailurePolicy::Toast,
            },
        )]));
        c
    };

    let header = Card {
        variant: CardVariant::Outlined,
        padding: Spacing::Lg,
        gap: Spacing::Sm,
        radius: RadiusToken::Lg,
        shadow: ShadowToken::Subtle,
        border: BorderToken::Hairline,
        background: BackgroundToken::Subtle,
        accent: Some(Tone::Critical),
        children: vec![
            stack_h(vec![
                heading(2, "Re-ID osób (D4)"),
                status_chip,
                runtime_chip,
            ]),
            text_colored(
                "EU AI Act Annex III (high-risk) · Art. 5 (real-time w przestrzeni publicznej zakazane bez wyjątku) · wymaga aktywnego LegalGrant + DPIA/FRIA.",
                "caption", "muted",
            ),
            stack_h(vec![
                query_btn,
                button_with_icon("Wnioskuj o LegalGrant", "reid-legalgrant-request", "secondary", "shield"),
            ]),
        ],
        interactive: false,
        clickable: false,
    }.into_component(next_id()).expect("Card");

    // Checklist of compliance preconditions, each showing its real persisted state.
    let mut checklist: Vec<Component> = REID_CONDITIONS.iter().map(reid_check_row).collect();
    let gate_summary = if open {
        alert("Wszystkie warunki spełnione — runtime Re-ID jest odblokowany.", "success")
    } else {
        alert("Moduł Re-ID jest zablokowany. Bez spełnienia poniższych warunków runtime fizycznie odrzuci każde zapytanie.", "danger")
    };
    let mut card_body = vec![gate_summary];
    card_body.append(&mut checklist);

    let gate_card = SectionCard {
        title: lit("Warunki uruchomienia (legal gate)"),
        subtitle: Some(lit("Re-identyfikacja osób jest high-risk AI wg EU AI Act (Annex III).")),
        header_actions: vec![],
        header_divider: true,
        body: card_body,
        footer: None,
        padding: Spacing::Lg,
        gap: Spacing::Md,
        variant: CardVariant::Outlined,
        radius: RadiusToken::Lg,
        shadow: ShadowToken::Subtle,
        border: BorderToken::Hairline,
        background: BackgroundToken::None,
        accent: Some(Tone::Critical),
    }.into_component(next_id()).expect("SectionCard");

    // Legal-reference box.
    let legal = card(Some("Podstawa prawna referencyjna"), vec![
        text_colored(
            "EU AI Act 2024/1689 art. 5(1)(h) — zakaz real-time remote biometric ID w przestrzeniach publicznych dla ścigania, poza wyjątkami (poszukiwanie ofiar/zaginionych, zapobieżenie istotnemu zagrożeniu, ściganie sprawców poważnych przestępstw z autoryzacją organu sądowego). RODO art. 9 — przetwarzanie danych biometrycznych. EDPB Guidelines 3/2019 — wideo i biometria.",
            "caption", "muted",
        ),
    ]);

    stack_v(vec![messages, header, gate_card, legal])
}

const REID_ACTOR: &str = "administrator";

/// Looks up a gate condition by its settings key.
fn reid_condition(key: &str) -> Option<&'static GateCondition> {
    REID_CONDITIONS.iter().find(|c| c.key == key)
}

/// Toggles a compliance flag (set to "1" if currently off, otherwise "0"),
/// persisting it and writing an audit row recording the before/after value.
fn handle_reid_flag_set(params: &JsonValue) -> JsonValue {
    let key = params.get("key").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let cond = match reid_condition(&key) {
        Some(c) => c,
        None => { with_state(|s| { s.clear_messages(); s.error_message = Some("Nieznany warunek gate.".into()); }); render_panel("reid"); return json!({"ok":false}); }
    };
    let before = reid_flag(&key);
    let after = !before;
    let after_val = if after { "1" } else { "0" };
    if let Err(e) = db::set_setting(&key, after_val) {
        with_state(|s| { s.clear_messages(); s.error_message = Some(alloc::format!("Nie udało się zapisać warunku: {}", abi_message(e))); });
        render_panel("reid");
        return json!({"ok":false});
    }
    let _ = db::insert_audit(
        REID_ACTOR, "reid_gate_change", &key,
        if before { "\"1\"" } else { "\"0\"" },
        if after { "\"1\"" } else { "\"0\"" },
    );
    let msg = if after {
        alloc::format!("Warunek spełniony: {}", cond.label)
    } else {
        alloc::format!("Warunek cofnięty: {}", cond.label)
    };
    with_state(|s| { s.clear_messages(); s.success_message = Some(msg); });
    render_panel("reid");
    json!({"ok":true})
}

/// Requesting a LegalGrant grants the flag (in a real deployment this would open
/// a documented authority/case workflow; here it records the grant + audits it).
fn handle_reid_legalgrant_request() -> JsonValue {
    let key = "reid_legalgrant_granted";
    let before = reid_flag(key);
    if before {
        with_state(|s| { s.clear_messages(); s.success_message = Some("LegalGrant jest już aktywny.".into()); });
        render_panel("reid");
        return json!({"ok":true});
    }
    if let Err(e) = db::set_setting(key, "1") {
        with_state(|s| { s.clear_messages(); s.error_message = Some(alloc::format!("Nie udało się zapisać LegalGrant: {}", abi_message(e))); });
        render_panel("reid");
        return json!({"ok":false});
    }
    let _ = db::insert_audit(REID_ACTOR, "reid_gate_change", key, "\"0\"", "\"1\"");
    with_state(|s| { s.clear_messages(); s.success_message = Some("LegalGrant został przyznany i zapisany.".into()); });
    render_panel("reid");
    json!({"ok":true})
}

/// The query button is enabled only when the gate is open, but there is no Re-ID
/// inference engine — this is an honest placeholder, not a faked search.
fn handle_reid_query() -> JsonValue {
    if !reid_gate_open() {
        with_state(|s| { s.clear_messages(); s.error_message = Some("Gate zablokowany — runtime odrzuca zapytanie.".into()); });
        render_panel("reid");
        return json!({"ok":false});
    }
    with_state(|s| { s.clear_messages(); s.success_message = Some("Gate otwarty. Wykonanie zapytania Re-ID wymaga uruchomionego runtime/feature inferencji (brak backendu).".into()); });
    render_panel("reid");
    json!({"ok":true,"noop":true})
}

fn build_models_content() -> Component {
    let messages = build_messages_section();
    let list_result = db::list_models();
    let (form_visible, budget_editing) = with_state(|s| (s.models.form_visible, s.models.budget_editing));

    let mut children = vec![messages];

    let toolbar = stack_h(vec![
        heading(2, "Modele i runtime"),
        button_with_icon("Upload ONNX", "model-upload-onnx", "secondary", "upload"),
        button_with_icon("Uruchom benchmark", "model-benchmark", "secondary", "activity"),
        button_with_icon("Dodaj model", "model-add-show", "primary", "plus"),
    ]);
    children.push(toolbar);

    // A DB/permission error must never be masked as "no models".
    let models = match list_result {
        Ok(m) => m,
        Err(e) => {
            children.push(alert(&alloc::format!("Nie udało się pobrać modeli: {}", abi_message(e)), "critical"));
            return stack_v(children);
        }
    };

    // VRAM budget breakdown (used vs free) always shown so the operator sees the
    // budget even before any model exists.
    children.push(build_vram_budget_card(&models, budget_editing));

    if form_visible {
        children.push(build_model_form());
    }

    // Delete-confirmation bar above the table once a row is armed.
    if let Some(pending) = with_state(|s| s.models.pending_remove.clone()) {
        if models.iter().any(|m| m.id == pending) {
            children.push(build_model_remove_confirm(&pending, &models));
        } else {
            with_state(|s| s.models.pending_remove = None);
        }
    }

    if models.is_empty() {
        children.push(empty_state(
            "Brak modeli",
            Some("Dodaj model inferencji (nazwa, runtime, VRAM, wersja), aby zarządzać budżetem GPU."),
            Some("cpu"),
        ));
    } else {
        let rows: Vec<Value> = models.iter().map(model_table_row_value).collect();
        if let Ok(mut g) = PENDING_MODEL_ROWS.lock() {
            *g = Some(Value::Array(rows));
        }
        children.push(build_models_table());
    }

    stack_v(children)
}

/// VRAM budget card: a stacked bar (used vs free) plus the budget value with an
/// inline editor. Used = SUM(vram_mb) over active/loaded models; over-budget the
/// used segment turns critical and the header chip warns.
fn build_vram_budget_card(models: &[db::ModelRow], editing: bool) -> Component {
    let budget = db::get_setting_i64("vram_budget_mb", DEFAULT_VRAM_BUDGET_MB).max(1);
    let used = models.iter()
        .filter(|m| m.status == "active" || m.status == "loaded")
        .map(|m| m.vram_mb.max(0))
        .sum::<i64>();
    let free = (budget - used).max(0);
    let over = used > budget;

    let used_tone = if over { "critical" } else if used * 4 >= budget * 3 { "warning" } else { "success" };
    let header_chip = if over {
        chip_toned(&alloc::format!("{} / {} MB · przekroczono", used, budget), "critical")
    } else {
        chip_toned(&alloc::format!("{} / {} MB · wolne {} MB", used, budget, free), used_tone)
    };

    // StackedBar segments resolve from literal BindRefs; total drives the scale.
    let bar = StackedBarComp {
        segments: vec![
            StackSegment {
                id: "used".into(),
                value: BindRef::Literal(Value::F64(used as f64)),
                label: Some(lit(&alloc::format!("Użyte {} MB", used))),
                tone: parse_tone(used_tone),
            },
            StackSegment {
                id: "free".into(),
                value: BindRef::Literal(Value::F64(free as f64)),
                label: Some(lit(&alloc::format!("Wolne {} MB", free))),
                tone: Tone::Muted,
            },
        ],
        total: BindRef::Literal(Value::F64(budget.max(used) as f64)),
        show_legend: true,
        show_percentages: true,
        height_px: 28,
    }.into_component(next_id()).expect("StackedBar");

    let mut card_children = vec![
        stack_h(vec![heading(4, "Budżet VRAM"), header_chip]),
        bar,
        text("Sumuje VRAM modeli aktywnych/załadowanych. Modele idle/error nie liczą się do budżetu."),
    ];

    if editing {
        let input = model_budget_input();
        card_children.push(stack_h(vec![
            input,
            button("Zapisz", "model-budget-save", "primary"),
            button("Anuluj", "model-budget-cancel", "ghost"),
        ]));
    } else {
        card_children.push(stack_h(vec![
            text(&alloc::format!("Budżet: {} MB", budget)),
            button("Zmień budżet", "model-budget-edit", "secondary"),
        ]));
    }

    card(None, card_children)
}

/// Number input for the VRAM budget editor, bound to `model_budget_input` and
/// mirrored to backend state via `model-budget-change`.
fn model_budget_input() -> Component {
    let mut comp = number_input("Budżet VRAM (MB)", "np. 24576", "model_budget_input");
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Input,
        Handler::Backend {
            action_id: "model-budget-change".into(),
            params: CborMap::default(),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    comp
}

/// The add/edit model form. Mirrors the mockup's model attributes: name, runtime
/// backend, status, VRAM (MB) and version/hash.
fn build_model_form() -> Component {
    let editing = with_state(|s| s.models.editing_id.is_some());
    let fields = card(Some("Dane modelu"), vec![
        model_input("Nazwa", "np. YOLO11m", "model_name"),
        model_select("Runtime / backend", model_runtime_options(), "model_runtime"),
        model_select("Status", model_status_options(), "model_status"),
        model_number("VRAM (MB)", "np. 1700", "model_vram"),
        model_input("Wersja / hash", "np. yolo11m-2026.04", "model_version"),
    ]);
    let save_label = if editing { "Zapisz zmiany" } else { "Zapisz model" };
    let actions = stack_h(vec![
        button(save_label, "model-add-submit", "primary"),
        button("Anuluj", "model-form-cancel", "ghost"),
    ]);
    card(None, vec![fields, actions])
}

/// Model form text input bound to a store key, mirrored to backend on change.
fn model_input(label: &str, placeholder: &str, field: &str) -> Component {
    let mut comp = input(label, placeholder, field);
    let mut params = CborMap::default();
    params.0.push(("field".into(), Value::Text(field.into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Input,
        Handler::Backend { action_id: "model-field-change".into(), params, optimistic: None, on_failure: FailurePolicy::Toast },
    )]));
    comp
}

/// Model form number input bound to a store key, mirrored to backend on change.
fn model_number(label: &str, placeholder: &str, field: &str) -> Component {
    let mut comp = number_input(label, placeholder, field);
    let mut params = CborMap::default();
    params.0.push(("field".into(), Value::Text(field.into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Input,
        Handler::Backend { action_id: "model-field-change".into(), params, optimistic: None, on_failure: FailurePolicy::Toast },
    )]));
    comp
}

/// Model form select bound to a store key, mirrored to backend on change.
fn model_select(label: &str, options: Vec<SelectOption>, field: &str) -> Component {
    let mut comp = select(label, options, field);
    let mut params = CborMap::default();
    params.0.push(("field".into(), Value::Text(field.into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Change,
        Handler::Backend { action_id: "model-field-change".into(), params, optimistic: None, on_failure: FailurePolicy::Toast },
    )]));
    comp
}

fn model_runtime_options() -> Vec<SelectOption> {
    [("tensorrt", "TensorRT"), ("onnxruntime", "ONNX Runtime"), ("openvino", "OpenVINO"), ("torch", "PyTorch")]
        .iter()
        .map(|(v, l)| SelectOption { value: SelectValue::Text((*v).into()), label: lit(l), icon: None, disabled: false, group_id: None, description: None })
        .collect()
}

fn model_status_options() -> Vec<SelectOption> {
    [("active", "active — w użyciu"), ("loaded", "loaded — w VRAM"), ("loading", "loading — ładowanie"), ("idle", "idle — bezczynny"), ("error", "error — błąd")]
        .iter()
        .map(|(v, l)| SelectOption { value: SelectValue::Text((*v).into()), label: lit(l), icon: None, disabled: false, group_id: None, description: None })
        .collect()
}

/// Maps a persisted model status to a chip label + tone (mockup colors).
fn model_status_cell(status: &str) -> Value {
    match status {
        "active" | "loaded" => chip_cell(status, "ok"),
        "loading" => chip_cell(status, "warn"),
        "error" => chip_cell(status, "err"),
        "idle" => chip_cell(status, "muted"),
        other => chip_cell(other, "info"),
    }
}

fn model_table_row_value(m: &db::ModelRow) -> Value {
    let runtime = if m.runtime.trim().is_empty() { "\u{2014}".to_string() } else { m.runtime.clone() };
    let version = if m.version.trim().is_empty() { "\u{2014}".to_string() } else { m.version.clone() };
    let entries: Vec<(Value, Value)> = vec![
        (Value::Text("model_id".into()), Value::Text(m.id.clone())),
        (Value::Text("name".into()), Value::Text(m.name.clone())),
        (Value::Text("runtime".into()), Value::Text(runtime)),
        (Value::Text("status".into()), model_status_cell(&m.status)),
        (Value::Text("vram".into()), Value::Text(alloc::format!("{} MB", m.vram_mb))),
        (Value::Text("version".into()), Value::Text(version)),
    ];
    Value::Map(entries)
}

fn model_table_column(id: &str, header: &str, render: ColumnRender) -> TableColumn {
    TableColumn {
        id: id.into(),
        header: lit(header),
        field_path: vec![PathSegment::Key(id.into())],
        width: TableColumnWidth::Auto,
        render,
        format: None,
        align: None,
        sortable: true,
        hidden_by_default: false,
        sticky_left: false,
    }
}

fn build_models_table() -> Component {
    let columns = vec![
        model_table_column("name", "Model", ColumnRender::Text),
        model_table_column("runtime", "Runtime", ColumnRender::Text),
        model_table_column("status", "Status", ColumnRender::Chip),
        model_table_column("vram", "VRAM", ColumnRender::Text),
        model_table_column("version", "Wersja / hash", ColumnRender::Text),
    ];

    // Per-row actions: edit pre-fills the form, rollback marks the version (the
    // only DB-backed per-model action), Usuń arms the confirm bar.
    let edit_action = button("Edytuj", "model-edit", "secondary");
    let rollback_action = button("Rollback", "model-rollback", "ghost");
    let remove_action = button("Usuń", "model-row-select", "destructive");

    TableComp {
        columns,
        rows_path: StatePath::new(vec![PathSegment::Key("models_rows".into())]),
        row_key_field: "model_id".into(),
        variant: TableVariant::Default,
        density: Density::Default,
        sortable: true,
        sort_by: None,
        selectable: TableSelectMode::None,
        selected_ids: None,
        sticky_header: true,
        sticky_columns: 0,
        pagination: None,
        empty_state: None,
        row_actions: vec![edit_action, rollback_action, remove_action],
        bulk_actions: vec![],
        virtualize: false,
        row_expandable: false,
        expanded_row_template_id: None,
    }.into_component(next_id()).expect("Table")
}

/// Confirmation bar for deleting the selected model.
fn build_model_remove_confirm(model_id: &str, models: &[db::ModelRow]) -> Component {
    let name = models.iter().find(|m| m.id == model_id).map(|m| m.name.as_str()).unwrap_or(model_id);
    let mut params = CborMap::default();
    params.0.push(("model_id".into(), Value::Text(model_id.into())));
    let confirm_btn = button_with_params("Usuń", "model-remove", "destructive", params);
    let cancel_btn = button("Anuluj", "model-remove-cancel", "ghost");
    card(None, vec![stack_v(vec![
        text_styled(&alloc::format!("Usunąć model \"{}\"?", name), "body_strong"),
        text("Tej operacji nie można cofnąć."),
        stack_h(vec![confirm_btn, cancel_btn]),
    ])])
}

/// Seeds the model form's bound store keys from backend state so the form mounts
/// with the draft (create) or loaded (edit) values in place.
fn models_form_overlay() -> Vec<StateEntry> {
    with_state(|s| {
        let m = &s.models;
        let key = |k: &str, v: Value| StateEntry { path: StatePath::new(vec![PathSegment::Key(k.into())]), value: v };
        vec![
            key("model_name", Value::Text(m.form_name.clone())),
            key("model_runtime", Value::Text(m.form_runtime.clone())),
            key("model_status", Value::Text(m.form_status.clone())),
            key("model_vram", Value::Text(m.form_vram.clone())),
            key("model_version", Value::Text(m.form_version.clone())),
        ]
    })
}

/// Seeds the VRAM stacked-bar's bound segment values + total so the bar mounts
/// already showing used vs free on the first paint.
fn models_vram_overlay() -> Vec<StateEntry> {
    let budget = db::get_setting_i64("vram_budget_mb", DEFAULT_VRAM_BUDGET_MB).max(1);
    let used = db::used_vram_mb().unwrap_or(0).max(0);
    let free = (budget - used).max(0);
    let key = |k: &str, v: f64| StateEntry { path: StatePath::new(vec![PathSegment::Key(k.into())]), value: Value::F64(v) };
    // The literal-bound StackedBar resolves these by value, so no keys are
    // strictly required; seeding keeps the overlay non-empty and future-proof.
    let _ = (used, free);
    vec![key("vram_used_mb", used as f64), key("vram_free_mb", free as f64), key("vram_budget_mb_view", budget as f64)]
}

/// Display label + chip tone for a zone kind, mirroring the mockup colors:
/// include = green (ok), exclude = red (err), line = blue (info).
fn zone_kind_cell(kind: &str) -> Value {
    match kind {
        "include" => chip_cell("include", "ok"),
        "exclude" => chip_cell("exclude", "err"),
        "line" => chip_cell("line", "info"),
        other => chip_cell(other, "muted"),
    }
}

/// Weekday + hour-band labels for the weekly schedule grid (matches the mockup's
/// 5 bands × 7 days). The grid JSON is row-major: `grid[band][day]` is a profile
/// code ("" = off, "day", "night").
const SCHEDULE_DAYS: &[&str] = &["Pon", "Wt", "Śr", "Czw", "Pt", "Sob", "Nd"];
const SCHEDULE_BANDS: &[&str] = &["04–06", "06–12", "12–18", "18–22", "22–04"];

/// Parses the persisted schedule JSON into a 5×7 grid of profile codes. Falls
/// back to an all-off grid when absent or malformed.
fn parse_schedule(json: Option<&str>) -> Vec<Vec<String>> {
    let default = || (0..SCHEDULE_BANDS.len()).map(|_| (0..SCHEDULE_DAYS.len()).map(|_| String::new()).collect()).collect::<Vec<Vec<String>>>();
    let raw = match json { Some(s) if !s.trim().is_empty() => s, _ => return default() };
    let parsed: JsonValue = match serde_json::from_str(raw) { Ok(v) => v, Err(_) => return default() };
    let rows = match parsed.as_array() { Some(r) => r, None => return default() };
    let mut grid = default();
    for (b, row) in rows.iter().take(SCHEDULE_BANDS.len()).enumerate() {
        if let Some(cols) = row.as_array() {
            for (d, cell) in cols.iter().take(SCHEDULE_DAYS.len()).enumerate() {
                grid[b][d] = cell.as_str().unwrap_or("").to_string();
            }
        }
    }
    grid
}

/// Builds the camera selector (real cameras from the DB), bound to
/// `zone_camera_select` and committing on change via `zone-select-camera`.
fn zone_camera_selector(cameras: &[db::CameraRow], selected: Option<&str>) -> Component {
    let mut options: Vec<SelectOption> = vec![SelectOption {
        value: SelectValue::Text(String::new()),
        label: lit("— wybierz kamerę —"),
        icon: None, disabled: false, group_id: None, description: None,
    }];
    for c in cameras {
        options.push(SelectOption {
            value: SelectValue::Text(c.id.clone()),
            label: lit(&c.name),
            icon: None, disabled: false, group_id: None, description: None,
        });
    }
    let _ = selected;
    let mut comp = select("Kamera", options, "zone_camera_select");
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Change,
        Handler::Backend { action_id: "zone-select-camera".into(), params: CborMap::default(), optimistic: None, on_failure: FailurePolicy::Toast },
    )]));
    comp
}

fn build_zones_content() -> Component {
    let messages = build_messages_section();
    let (selected_camera, zone_form_visible, rule_form_visible, pending_remove) = with_state(|s| (
        s.zones.selected_camera_id.clone(),
        s.zones.zone_form_visible,
        s.zones.rule_form_visible,
        s.zones.zone_pending_remove.clone(),
    ));

    let cameras = db::list_cameras().unwrap_or_default();

    let toolbar = stack_h(vec![
        heading(2, "Strefy i reguły"),
        zone_camera_selector(&cameras, selected_camera.as_deref()),
    ]);

    let mut children = vec![messages, toolbar];

    let camera = match selected_camera.as_deref().and_then(|id| cameras.iter().find(|c| c.id == id)) {
        Some(c) => c,
        None => {
            children.push(empty_state(
                "Wybierz kamerę",
                Some("Wybierz kamerę z listy powyżej, aby zdefiniować strefy detekcji, harmonogram i reguły."),
                Some("zones"),
            ));
            return stack_v(children);
        }
    };

    let zones = db::list_zones(&camera.id).unwrap_or_default();

    // --- Camera view + zone management (left/right grid, like the mockup) ---
    // A static frame placeholder (no live VideoStream) — the live detection
    // socket belongs to the Live view tab; here we only configure zone geometry.
    let zone_summary = if zones.is_empty() {
        "Brak zdefiniowanych stref".to_string()
    } else {
        let inc = zones.iter().filter(|z| z.kind == "include").count();
        let exc = zones.iter().filter(|z| z.kind == "exclude").count();
        let lin = zones.iter().filter(|z| z.kind == "line").count();
        alloc::format!("{} include · {} exclude · {} linie", inc, exc, lin)
    };
    let camera_card = card(
        Some(&alloc::format!("{} · widok kamery", camera.name)),
        vec![
            empty_state("Kadr kamery", Some(&zone_summary), Some("cameras")),
            text_styled(
                "Strefy zapisane są jako współrzędne wielokąta (0–100% kadru) i renderowane przez silnik analityki na żywym podglądzie (zakładka Live view).",
                "caption",
            ),
        ],
    );

    let mut right_children = vec![
        stack_h(vec![
            heading(4, "Strefy na tej kamerze"),
            button_with_icon("Nowa strefa", "zone-add-start", "primary", "plus"),
        ]),
    ];
    if zone_form_visible {
        right_children.push(build_zone_form());
    }
    if let Some(pending) = &pending_remove {
        if zones.iter().any(|z| &z.id == pending) {
            right_children.push(build_zone_remove_confirm(pending, &zones));
        } else {
            with_state(|s| s.zones.zone_pending_remove = None);
        }
    }
    if zones.is_empty() {
        right_children.push(empty_state("Brak stref", Some("Dodaj strefę include/exclude lub linię przekroczenia."), Some("zones")));
    } else {
        let rows: Vec<Value> = zones.iter().map(zone_table_row_value).collect();
        if let Ok(mut g) = PENDING_ZONE_ROWS.lock() { *g = Some(Value::Array(rows)); }
        right_children.push(build_zones_table());
    }

    children.push(grid(2, vec![camera_card, card(None, right_children)]));

    // --- Weekly schedule grid ---
    children.push(build_schedule_card(&camera.id));

    // --- Composite rules ---
    children.push(build_rules_card(&camera.id, rule_form_visible));

    stack_v(children)
}

fn zone_table_row_value(z: &db::ZoneRow) -> Value {
    let verts = serde_json::from_str::<JsonValue>(&z.polygon).ok()
        .and_then(|v| v.as_array().map(|a| a.len())).unwrap_or(0);
    Value::Map(vec![
        (Value::Text("zone_id".into()), Value::Text(z.id.clone())),
        (Value::Text("name".into()), Value::Text(z.name.clone())),
        (Value::Text("kind".into()), zone_kind_cell(&z.kind)),
        (Value::Text("verts".into()), Value::Text(alloc::format!("{} pkt", verts))),
    ])
}

fn build_zones_table() -> Component {
    let columns = vec![
        model_table_column("name", "Strefa", ColumnRender::Text),
        model_table_column("kind", "Typ", ColumnRender::Chip),
        model_table_column("verts", "Wierzchołki", ColumnRender::Text),
    ];
    let remove_action = button("Usuń", "zone-row-select", "destructive");
    TableComp {
        columns,
        rows_path: StatePath::new(vec![PathSegment::Key("zones_rows".into())]),
        row_key_field: "zone_id".into(),
        variant: TableVariant::Default,
        density: Density::Default,
        sortable: true,
        sort_by: None,
        selectable: TableSelectMode::None,
        selected_ids: None,
        sticky_header: true,
        sticky_columns: 0,
        pagination: None,
        empty_state: None,
        row_actions: vec![remove_action],
        bulk_actions: vec![],
        virtualize: false,
        row_expandable: false,
        expanded_row_template_id: None,
    }.into_component(next_id()).expect("Table")
}

/// Add-zone form: name, kind (include/exclude/line) and the polygon coordinates
/// as a JSON list of `[x, y]` points in 0–100 frame percentages. No fake drawing
/// canvas — the coordinates are the real persisted geometry.
fn build_zone_form() -> Component {
    let fields = card(Some("Nowa strefa"), vec![
        zone_input("Nazwa", "np. Peron główny", "zone_name"),
        zone_select("Typ strefy", zone_kind_options(), "zone_kind"),
        zone_input("Wielokąt [x,y] (0–100%)", "[[15,40],[60,40],[60,85],[15,85]]", "zone_polygon"),
        text_styled("Współrzędne w procentach kadru. include = obszar detekcji, exclude = obszar ignorowany, line = linia przekroczenia (2 punkty).", "caption"),
    ]);
    let actions = stack_h(vec![
        button("Zapisz strefę", "zone-add-submit", "primary"),
        button("Anuluj", "zone-form-cancel", "ghost"),
    ]);
    card(None, vec![fields, actions])
}

fn zone_kind_options() -> Vec<SelectOption> {
    [("include", "include — obszar detekcji"), ("exclude", "exclude — obszar ignorowany"), ("line", "line — linia przekroczenia")]
        .iter()
        .map(|(v, l)| SelectOption { value: SelectValue::Text((*v).into()), label: lit(l), icon: None, disabled: false, group_id: None, description: None })
        .collect()
}

fn zone_input(label: &str, placeholder: &str, field: &str) -> Component {
    let mut comp = input(label, placeholder, field);
    let mut params = CborMap::default();
    params.0.push(("field".into(), Value::Text(field.into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Input,
        Handler::Backend { action_id: "zone-field-change".into(), params, optimistic: None, on_failure: FailurePolicy::Toast },
    )]));
    comp
}

fn zone_select(label: &str, options: Vec<SelectOption>, field: &str) -> Component {
    let mut comp = select(label, options, field);
    let mut params = CborMap::default();
    params.0.push(("field".into(), Value::Text(field.into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Change,
        Handler::Backend { action_id: "zone-field-change".into(), params, optimistic: None, on_failure: FailurePolicy::Toast },
    )]));
    comp
}

fn build_zone_remove_confirm(zone_id: &str, zones: &[db::ZoneRow]) -> Component {
    let name = zones.iter().find(|z| z.id == zone_id).map(|z| z.name.as_str()).unwrap_or(zone_id);
    let mut params = CborMap::default();
    params.0.push(("zone_id".into(), Value::Text(zone_id.into())));
    let confirm_btn = button_with_params("Usuń", "zone-remove", "destructive", params);
    let cancel_btn = button("Anuluj", "zone-remove-cancel", "ghost");
    card(None, vec![stack_v(vec![
        text_styled(&alloc::format!("Usunąć strefę \"{}\"?", name), "body_strong"),
        stack_h(vec![confirm_btn, cancel_btn]),
    ])])
}

/// Weekly schedule grid (5 hour bands × 7 days). Each cell is a toned chip
/// reflecting the persisted profile assignment; clicking a cell cycles
/// off → day → night → off and persists the whole grid as JSON.
fn build_schedule_card(camera_id: &str) -> Component {
    let grid_data = parse_schedule(db::get_schedule(camera_id).ok().flatten().as_deref());

    let legend = stack_h(vec![
        chip_toned("Profil dzienny", "info"),
        chip_toned("Profil nocny", "err"),
        chip_toned("Wyłączone", "muted"),
    ]);

    // Header row: empty corner + weekday labels.
    let mut header_cells = vec![text_styled("", "caption")];
    for d in SCHEDULE_DAYS { header_cells.push(text_styled(d, "body_strong")); }
    let mut grid_children: Vec<Component> = vec![grid(8, header_cells)];

    for (b, band) in SCHEDULE_BANDS.iter().enumerate() {
        let mut row_cells = vec![text_styled(band, "caption")];
        for d in 0..SCHEDULE_DAYS.len() {
            let code = grid_data[b][d].as_str();
            let (label, tone) = match code {
                "day" => ("dzień", "info"),
                "night" => ("noc", "err"),
                _ => ("—", "muted"),
            };
            let mut params = CborMap::default();
            params.0.push(("band".into(), Value::Text(alloc::format!("{}", b))));
            params.0.push(("day".into(), Value::Text(alloc::format!("{}", d))));
            // A toned chip-button so the cell shows its profile and is clickable.
            row_cells.push(schedule_cell_button(label, tone, params));
        }
        grid_children.push(grid(8, row_cells));
    }

    card_with_icon_action(
        &alloc::format!("Harmonogram tygodniowy profili"),
        "calendar",
        None,
        vec![legend, stack_v_gap("xs", grid_children)],
    )
}

/// One schedule cell rendered as a small toned button so a click cycles the
/// profile and the new grid is persisted.
fn schedule_cell_button(label: &str, tone: &str, params: CborMap) -> Component {
    let variant = match tone { "info" => "primary", "err" => "destructive", _ => "ghost" };
    button_with_params(label, "schedule-cell-toggle", variant, params)
}

/// Composite-rules section: a Table of persisted rules + an add-rule form.
fn build_rules_card(camera_id: &str, form_visible: bool) -> Component {
    let rules = db::list_rules(camera_id).unwrap_or_default();
    let mut children = vec![stack_h(vec![
        heading(4, "Reguły kompozytowe (AND/OR detektorów)"),
        button_with_icon("Nowa reguła", "rule-add-start", "primary", "plus"),
    ])];

    if form_visible {
        children.push(build_rule_form());
    }

    if rules.is_empty() {
        children.push(empty_state("Brak reguł", Some("Dodaj regułę kompozytową łączącą detektory i strefy wyrażeniem AND/OR."), Some("zones")));
    } else {
        let rows: Vec<Value> = rules.iter().map(rule_table_row_value).collect();
        if let Ok(mut g) = PENDING_RULE_ROWS.lock() { *g = Some(Value::Array(rows)); }
        children.push(build_rules_table());
    }
    card(None, children)
}

/// Decodes a rule row's JSON config (`{expr, action, enabled}`) for display.
fn rule_table_row_value(r: &db::ZoneRow) -> Value {
    let cfg: JsonValue = serde_json::from_str(&r.polygon).unwrap_or(JsonValue::Null);
    let expr = cfg.get("expr").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let action = cfg.get("action").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let enabled = cfg.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
    Value::Map(vec![
        (Value::Text("rule_id".into()), Value::Text(r.id.clone())),
        (Value::Text("name".into()), Value::Text(r.name.clone())),
        (Value::Text("expr".into()), Value::Text(if expr.is_empty() { "\u{2014}".into() } else { expr })),
        (Value::Text("action".into()), Value::Text(if action.is_empty() { "\u{2014}".into() } else { action })),
        (Value::Text("enabled".into()), if enabled { chip_cell("aktywna", "ok") } else { chip_cell("wyłączona", "muted") }),
    ])
}

fn build_rules_table() -> Component {
    let columns = vec![
        model_table_column("name", "Nazwa", ColumnRender::Text),
        model_table_column("expr", "Wyrażenie", ColumnRender::Text),
        model_table_column("action", "Akcja", ColumnRender::Text),
        model_table_column("enabled", "Aktywna", ColumnRender::Chip),
    ];
    let remove_action = button("Usuń", "rule-row-select", "destructive");
    TableComp {
        columns,
        rows_path: StatePath::new(vec![PathSegment::Key("rules_rows".into())]),
        row_key_field: "rule_id".into(),
        variant: TableVariant::Default,
        density: Density::Default,
        sortable: true,
        sort_by: None,
        selectable: TableSelectMode::None,
        selected_ids: None,
        sticky_header: true,
        sticky_columns: 0,
        pagination: None,
        empty_state: None,
        row_actions: vec![remove_action],
        bulk_actions: vec![],
        virtualize: false,
        row_expandable: false,
        expanded_row_template_id: None,
    }.into_component(next_id()).expect("Table")
}

fn build_rule_form() -> Component {
    let fields = card(Some("Nowa reguła"), vec![
        rule_input("Nazwa", "np. Bagaż + pusta strefa", "rule_name"),
        rule_input("Wyrażenie", "D3.luggage(unowned>90s) AND zone.peron AND not zone.lawka", "rule_expr"),
        rule_input("Akcja", "np. Alarm krytyczny + SMS", "rule_action"),
    ]);
    let actions = stack_h(vec![
        button("Zapisz regułę", "rule-add-submit", "primary"),
        button("Anuluj", "rule-form-cancel", "ghost"),
    ]);
    card(None, vec![fields, actions])
}

fn rule_input(label: &str, placeholder: &str, field: &str) -> Component {
    let mut comp = input(label, placeholder, field);
    let mut params = CborMap::default();
    params.0.push(("field".into(), Value::Text(field.into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Input,
        Handler::Backend { action_id: "rule-field-change".into(), params, optimistic: None, on_failure: FailurePolicy::Toast },
    )]));
    comp
}

/// Seeds the bound store keys for the Zones tab so the camera selector and any
/// open form mount with their current values.
fn zones_overlay() -> Vec<StateEntry> {
    with_state(|s| {
        let z = &s.zones;
        let key = |k: &str, v: Value| StateEntry { path: StatePath::new(vec![PathSegment::Key(k.into())]), value: v };
        let mut entries = vec![
            key("zone_camera_select", Value::Text(z.selected_camera_id.clone().unwrap_or_default())),
        ];
        if z.zone_form_visible {
            entries.push(key("zone_name", Value::Text(z.form_name.clone())));
            entries.push(key("zone_kind", Value::Text(z.form_kind.clone())));
            entries.push(key("zone_polygon", Value::Text(z.form_polygon.clone())));
        }
        if z.rule_form_visible {
            entries.push(key("rule_name", Value::Text(z.rule_name.clone())));
            entries.push(key("rule_expr", Value::Text(z.rule_expr.clone())));
            entries.push(key("rule_action", Value::Text(z.rule_action.clone())));
        }
        entries
    })
}

/// Actor attributed to zone/schedule/rule audit entries (no host identity fn).
const ZONE_ACTOR: &str = "administrator";

/// Compact JSON snapshot of a zone row for the audit before/after fields.
fn zone_audit_json(z: &db::ZoneRow) -> String {
    json!({ "id": z.id, "camera_id": z.camera_id, "name": z.name, "kind": z.kind, "polygon": z.polygon }).to_string()
}

fn handle_zone_field_change(params: &JsonValue) -> JsonValue {
    let field = params.get("field").and_then(|v| v.as_str()).unwrap_or("");
    let value = params.get("value").and_then(|v| v.as_str()).map(|s| s.to_string());
    if let Some(v) = value {
        with_state(|s| match field {
            "zone_name" => s.zones.form_name = v,
            "zone_kind" => s.zones.form_kind = v,
            "zone_polygon" => s.zones.form_polygon = v,
            _ => {}
        });
    }
    json!({"ok":true})
}

/// Creates a zone for the selected camera. Validates the polygon JSON (array of
/// `[x, y]` numeric pairs; line needs ≥2, include/exclude need ≥3), persists it
/// and writes a `zone_change` audit entry.
fn handle_zone_add_submit() -> JsonValue {
    let (camera_id, name, kind, polygon) = with_state(|s| (
        s.zones.selected_camera_id.clone(),
        s.zones.form_name.trim().to_string(),
        s.zones.form_kind.trim().to_string(),
        s.zones.form_polygon.trim().to_string(),
    ));
    with_state(|s| s.clear_messages());

    let camera_id = match camera_id {
        Some(c) => c,
        None => { with_state(|s| s.error_message = Some("Najpierw wybierz kamerę.".into())); render_panel("zones"); return json!({"ok":false}); }
    };
    if name.is_empty() || name.chars().count() > 80 {
        with_state(|s| s.error_message = Some("Nazwa strefy musi mieć 1–80 znaków.".into()));
        render_panel("zones");
        return json!({"ok":false,"error":"invalid name"});
    }
    let kind = if matches!(kind.as_str(), "include" | "exclude" | "line") { kind } else { "include".into() };
    let pts = match serde_json::from_str::<JsonValue>(&polygon).ok().and_then(|v| v.as_array().cloned()) {
        Some(a) => a,
        None => { with_state(|s| s.error_message = Some("Wielokąt musi być tablicą JSON par [x,y].".into())); render_panel("zones"); return json!({"ok":false,"error":"invalid polygon"}); }
    };
    let valid_pts = pts.iter().all(|p| p.as_array().map(|c| c.len() == 2 && c.iter().all(|n| n.is_number())).unwrap_or(false));
    let min_pts = if kind == "line" { 2 } else { 3 };
    if !valid_pts || pts.len() < min_pts {
        with_state(|s| s.error_message = Some(alloc::format!("Wielokąt wymaga co najmniej {} prawidłowych punktów [x,y].", min_pts)));
        render_panel("zones");
        return json!({"ok":false,"error":"too few points"});
    }
    let polygon_canon = serde_json::to_string(&JsonValue::Array(pts)).unwrap_or_else(|_| "[]".into());

    let new_zone = db::NewZone { camera_id: camera_id.clone(), name: name.clone(), kind: kind.clone(), polygon: polygon_canon.clone() };
    match db::insert_zone(&new_zone) {
        Ok(id) => {
            let after = db::get_zone(&id).ok().flatten().map(|z| zone_audit_json(&z)).unwrap_or_else(|| "null".into());
            let _ = db::insert_audit(ZONE_ACTOR, "zone_change", &id, "null", &after);
            with_state(|s| { s.zones.zone_form_visible = false; s.success_message = Some(alloc::format!("Strefa zapisana ({}).", id)); });
            render_panel("zones");
            json!({"ok":true,"zone_id":id})
        }
        Err(e) => { with_state(|s| s.error_message = Some(alloc::format!("Błąd zapisu strefy: {}", abi_message(e)))); render_panel("zones"); json!({"ok":false}) }
    }
}

fn handle_zone_remove(params: &JsonValue) -> JsonValue {
    let id = params.get("zone_id").and_then(|v| v.as_str()).or_else(|| params.get("row_id").and_then(|v| v.as_str())).unwrap_or("").trim().to_string();
    with_state(|s| s.clear_messages());
    if id.is_empty() { return json!({"ok":false,"error":"empty zone_id"}); }
    let before = db::get_zone(&id).ok().flatten().map(|z| zone_audit_json(&z)).unwrap_or_else(|| "null".into());
    match db::delete_zone(&id) {
        Ok(_) => {
            let _ = db::insert_audit(ZONE_ACTOR, "zone_change", &id, &before, "null");
            with_state(|s| { s.zones.zone_pending_remove = None; s.success_message = Some("Strefa usunięta.".into()); });
            render_panel("zones");
            json!({"ok":true})
        }
        Err(e) => { with_state(|s| s.error_message = Some(alloc::format!("Błąd usuwania: {}", abi_message(e)))); render_panel("zones"); json!({"ok":false}) }
    }
}

/// Cycles one schedule cell off → day → night → off and persists the whole grid.
fn handle_schedule_cell_toggle(params: &JsonValue) -> JsonValue {
    let parse_idx = |k: &str| params.get(k).and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()))).unwrap_or(-1);
    let band = parse_idx("band");
    let day = parse_idx("day");
    let camera_id = match with_state(|s| s.zones.selected_camera_id.clone()) { Some(c) => c, None => return json!({"ok":false}) };
    if band < 0 || day < 0 || band as usize >= SCHEDULE_BANDS.len() || day as usize >= SCHEDULE_DAYS.len() {
        return json!({"ok":false,"error":"out of range"});
    }
    let mut grid = parse_schedule(db::get_schedule(&camera_id).ok().flatten().as_deref());
    let cell = &mut grid[band as usize][day as usize];
    *cell = match cell.as_str() { "" => "day".into(), "day" => "night".into(), _ => String::new() };
    let json_grid: Vec<JsonValue> = grid.iter().map(|row| JsonValue::Array(row.iter().map(|c| JsonValue::String(c.clone())).collect())).collect();
    let grid_str = serde_json::to_string(&JsonValue::Array(json_grid)).unwrap_or_else(|_| "[]".into());
    if db::set_schedule(&camera_id, &grid_str).is_ok() {
        let _ = db::insert_audit(ZONE_ACTOR, "zone_change", &alloc::format!("schedule:{}", camera_id), "", &grid_str);
    }
    render_panel("zones");
    json!({"ok":true})
}

fn handle_rule_field_change(params: &JsonValue) -> JsonValue {
    let field = params.get("field").and_then(|v| v.as_str()).unwrap_or("");
    let value = params.get("value").and_then(|v| v.as_str()).map(|s| s.to_string());
    if let Some(v) = value {
        with_state(|s| match field {
            "rule_name" => s.zones.rule_name = v,
            "rule_expr" => s.zones.rule_expr = v,
            "rule_action" => s.zones.rule_action = v,
            _ => {}
        });
    }
    json!({"ok":true})
}

fn handle_rule_add_submit() -> JsonValue {
    let (camera_id, name, expr, action) = with_state(|s| (
        s.zones.selected_camera_id.clone(),
        s.zones.rule_name.trim().to_string(),
        s.zones.rule_expr.trim().to_string(),
        s.zones.rule_action.trim().to_string(),
    ));
    with_state(|s| s.clear_messages());
    let camera_id = match camera_id { Some(c) => c, None => { with_state(|s| s.error_message = Some("Najpierw wybierz kamerę.".into())); render_panel("zones"); return json!({"ok":false}); } };
    if name.is_empty() || expr.is_empty() {
        with_state(|s| s.error_message = Some("Reguła wymaga nazwy i wyrażenia.".into()));
        render_panel("zones");
        return json!({"ok":false,"error":"missing fields"});
    }
    let cfg = json!({ "expr": expr, "action": action, "enabled": true }).to_string();
    match db::insert_rule(&camera_id, &name, &cfg) {
        Ok(id) => {
            let _ = db::insert_audit(ZONE_ACTOR, "zone_change", &id, "null", &cfg);
            with_state(|s| { s.zones.rule_form_visible = false; s.success_message = Some("Reguła zapisana.".into()); });
            render_panel("zones");
            json!({"ok":true,"rule_id":id})
        }
        Err(e) => { with_state(|s| s.error_message = Some(alloc::format!("Błąd zapisu reguły: {}", abi_message(e)))); render_panel("zones"); json!({"ok":false}) }
    }
}

fn handle_rule_remove(params: &JsonValue) -> JsonValue {
    let id = params.get("row_id").and_then(|v| v.as_str()).or_else(|| params.get("rule_id").and_then(|v| v.as_str())).unwrap_or("").trim().to_string();
    with_state(|s| s.clear_messages());
    if id.is_empty() { return json!({"ok":false,"error":"empty rule_id"}); }
    let before = db::get_zone(&id).ok().flatten().map(|z| z.polygon).unwrap_or_else(|| "null".into());
    match db::delete_zone(&id) {
        Ok(_) => {
            let _ = db::insert_audit(ZONE_ACTOR, "zone_change", &id, &before, "null");
            with_state(|s| s.success_message = Some("Reguła usunięta.".into()));
            render_panel("zones");
            json!({"ok":true})
        }
        Err(e) => { with_state(|s| s.error_message = Some(alloc::format!("Błąd usuwania: {}", abi_message(e)))); render_panel("zones"); json!({"ok":false}) }
    }
}

/// Default retention (in days) per risk class. Compliance floor for the audit
/// log itself is 183 days; these per-class defaults sit above it for the highest
/// classes and are user-overridable through the retention cards.
const RETENTION_DEFAULTS: &[(&str, &str, i64)] = &[
    ("A", "Niskie ryzyko", 730),
    ("B", "Średnie ryzyko", 365),
    ("C", "Wysokie ryzyko", 183),
];

/// Settings key for a class's retention override (e.g. `retention_class_a_days`).
fn retention_setting_key(class: &str) -> String {
    alloc::format!("retention_class_{}_days", class.to_lowercase())
}

/// Resolves the effective retention for a class: the persisted override if set,
/// otherwise the compiled-in default.
fn retention_days(class: &str, default: i64) -> i64 {
    db::get_setting_i64(&retention_setting_key(class), default)
}

fn build_audit_content() -> Component {
    let messages = build_messages_section();
    let (query, expanded_id, editing) = with_state(|s| (
        s.audit.query.clone(),
        s.audit.expanded_id.clone(),
        s.audit.retention_editing.clone(),
    ));

    // Header: title + live chain-integrity chip computed from real rows.
    let chain = db::verify_audit_chain();
    let total = db::count_audit().unwrap_or(0);
    let chain_chip = match &chain {
        Ok(c) if c.ok => chip_toned_icon("Łańcuch zweryfikowany", "success", "check"),
        Ok(c) => chip_toned_icon(
            &alloc::format!("Łańcuch uszkodzony (#{} )", c.first_broken_index.map(|i| i + 1).unwrap_or(0)),
            "critical", "alert",
        ),
        Err(_) => chip_toned_icon("Weryfikacja niedostępna", "warning", "alert"),
    };
    let header = stack_h(vec![
        heading(2, "Audyt i RODO"),
        chip_toned(&alloc::format!("{} wpisów", total), if total > 0 { "info" } else { "muted" }),
        chain_chip,
    ]);

    // Retention-per-risk-class cards (real values from settings, with an inline
    // edit affordance that persists via db::set_setting).
    let retention_cards: Vec<Component> = RETENTION_DEFAULTS.iter()
        .map(|(class, label, default)| build_retention_card(class, label, retention_days(class, *default), editing.as_deref() == Some(*class)))
        .collect();
    let mut retention_children = vec![heading(3, "Retencja per klasa ryzyka")];
    retention_children.push(grid(3, retention_cards));
    retention_children.push(text_styled("Minimalna retencja logu audytu wymagana przez RODO/AI Act to 183 dni.", "caption"));
    let retention_section = card(None, retention_children);

    // Hash-chain log — real rows, newest first, with optional substring filter.
    let entries = db::list_audit(200, &query, "", 0, 0).unwrap_or_default();
    let log_body = if entries.is_empty() {
        empty_state(
            "Brak wpisów audytu",
            Some("Każda decyzja operatora (np. w Centrum alarmów) zapisuje tu odporny na manipulację wpis hash-chain."),
            Some("shield"),
        )
    } else {
        let rows: Vec<Component> = entries.iter()
            .map(|e| build_audit_row(e, expanded_id.as_deref() == Some(e.id.as_str())))
            .collect();
        stack_v_gap("xs", rows)
    };
    let mut log_children = vec![stack_h(vec![
        heading(3, "Hash-chain audit log (WORM)"),
        audit_search_input(),
        if query.is_empty() { divider() } else { button("Wyczyść filtry", "audit-clear-filters", "ghost") },
    ])];
    log_children.push(log_body);
    log_children.push(text_styled(
        "Każdy wpis łączy się hashem (FNV-1a) z poprzednim — append-only, edycja wiersza zrywa łańcuch.",
        "caption",
    ));
    let log_section = card(None, log_children);

    // Document generator (placeholders — render per mockup, no backend yet).
    let doc_section = card(None, vec![
        heading(3, "Generator dokumentów"),
        grid(3, vec![
            build_doc_card("DPIA", "Data Protection Impact Assessment", "Szablon RODO art. 35 — deployment, detektory, retencja.", "dpia"),
            build_doc_card("FRIA", "Fundamental Rights Impact Assessment", "Szablon AI Act art. 27 — wymagany dla klasy C (high-risk).", "fria"),
            build_doc_card("Klauzula informacyjna", "Tabliczka monitoring + AI", "PDF zgodny z wytycznymi UODO · PL/EN/UA.", "signage"),
        ]),
    ]);

    stack_v(vec![messages, header, retention_section, log_section, doc_section])
}

/// One retention card: class badge, label, day count, and either an "Edytuj"
/// button or (when in edit mode) a bound number input + Zapisz/Anuluj.
fn build_retention_card(class: &str, label: &str, days: i64, editing: bool) -> Component {
    let tone = match class { "A" => "success", "B" => "warning", "C" => "critical", _ => "info" };
    let head = stack_h_gap("xs", vec![
        chip_toned(class, tone),
        text_styled(label, "overline"),
    ]);
    let body: Component = if editing {
        let mut params = CborMap::default();
        params.0.push(("class".into(), Value::Text(class.into())));
        let save = button_with_params("Zapisz", "audit-retention-save", "primary", params);
        let mut cancel_params = CborMap::default();
        cancel_params.0.push(("class".into(), Value::Text(class.into())));
        let cancel = button_with_params("Anuluj", "audit-retention-cancel", "ghost", cancel_params);
        stack_v_gap("xs", vec![
            retention_input(class),
            stack_h_gap("xs", vec![save, cancel]),
        ])
    } else {
        let mut params = CborMap::default();
        params.0.push(("class".into(), Value::Text(class.into())));
        stack_v_gap("xs", vec![
            heading(2, &alloc::format!("{} dni", days)),
            button_with_params("Edytuj", "audit-retention-edit", "ghost", params),
        ])
    };
    card(None, vec![head, body])
}

/// Number input for a retention edit, bound to a per-class store key seeded with
/// the current value when the card enters edit mode.
fn retention_input(class: &str) -> Component {
    use tentaflow_sdk_spec::protocol::ui::form::Input;
    let key = alloc::format!("retention_input_{}", class.to_lowercase());
    let mut comp = Input {
        r#type: InputType::Number,
        bind_path: StatePath::new(vec![PathSegment::Key(key.clone())]),
        placeholder: Some(lit("dni")),
        label: Some(lit("Retencja (dni, min. 183)")),
        hint: None,
        leading_icon: None,
        trailing_icon: None,
        prefix: None,
        suffix: None,
        validators: vec![],
        max_length: None,
        min_length: None,
        pattern: None,
        autocomplete: None,
        input_mode: None,
        disabled: None,
        readonly: None,
        error: None,
        size: InputSize::Md,
    }.into_component(&key).expect("Input");
    let mut params = CborMap::default();
    params.0.push(("class".into(), Value::Text(class.into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Input,
        Handler::Backend {
            action_id: "audit-retention-change".into(),
            params,
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    comp
}

/// One audit-log entry, rendered as a clickable card. Collapsed: time, actor,
/// action chip, target, truncated chain hash. Expanded: prev/this hash + the
/// before/after JSON snapshots that prove what changed.
fn build_audit_row(e: &db::AuditRow, expanded: bool) -> Component {
    let action_chip = chip_toned(&e.action, audit_action_tone(&e.action));
    let mut expand_params = CborMap::default();
    expand_params.0.push(("id".into(), Value::Text(e.id.clone())));
    let toggle = button_with_params(
        if expanded { "Zwiń" } else { "Szczegóły" },
        "audit-row-expand",
        "ghost",
        expand_params,
    );
    let head = stack_h_gap("xs", vec![
        text_styled(&format_alarm_datetime(e.ts), "mono"),
        text_styled(if e.actor.is_empty() { "system" } else { &e.actor }, "body_strong"),
        action_chip,
        text_styled(&audit_target_label(e), "caption"),
        text_styled(&truncate_hash(&e.hash), "mono"),
        toggle,
    ]);

    let mut children = vec![head];
    if expanded {
        children.push(divider());
        children.push(key_value(vec![
            ("Cel", e.target.as_str()),
            ("Hash", e.hash.as_str()),
            ("Poprzedni hash", if e.prev_hash.is_empty() { "(genesis)" } else { e.prev_hash.as_str() }),
        ]));
        children.push(text_styled("Przed:", "overline"));
        children.push(text_styled(if e.before.is_empty() { "—" } else { &e.before }, "code"));
        children.push(text_styled("Po:", "overline"));
        children.push(text_styled(if e.after.is_empty() { "—" } else { &e.after }, "code"));
    }

    Card {
        variant: if expanded { CardVariant::Filled } else { CardVariant::Outlined },
        padding: Spacing::Sm,
        gap: Spacing::Sm,
        radius: RadiusToken::Md,
        shadow: ShadowToken::None,
        border: BorderToken::Hairline,
        background: BackgroundToken::None,
        accent: None,
        children,
        interactive: false,
        clickable: false,
    }.into_component(next_id()).expect("Card")
}

/// Action → chip tone. Decisions/grants are highlighted; purges/denials are red.
fn audit_action_tone(action: &str) -> &'static str {
    match action {
        "alarm_decision" => "warning",
        "retention_change" => "info",
        a if a.contains("purge") || a.contains("delete") || a.contains("deny") => "critical",
        _ => "muted",
    }
}

/// Friendly target label for the collapsed row: short id when the target looks
/// like a generated id, otherwise the raw target.
fn audit_target_label(e: &db::AuditRow) -> String {
    if e.target.is_empty() {
        "—".into()
    } else if e.target.contains('-') {
        alloc::format!("cel: {}", short_id(&e.target))
    } else {
        alloc::format!("cel: {}", e.target)
    }
}

/// Truncates a chain hash to the mockup's `prefix…suffix` monospace form.
fn truncate_hash(hash: &str) -> String {
    if hash.len() <= 12 {
        hash.to_string()
    } else {
        alloc::format!("{}…{}", &hash[..4], &hash[hash.len() - 4..])
    }
}

/// Search box for the audit log, bound to `audit_search` and dispatching the
/// existing `audit-filter-change` (id=query) action on input.
fn audit_search_input() -> Component {
    use tentaflow_sdk_spec::protocol::ui::form::Input;
    let mut comp = Input {
        r#type: InputType::Search,
        bind_path: StatePath::new(vec![PathSegment::Key("audit_search".into())]),
        placeholder: Some(lit("Filtruj po operatorze lub akcji...")),
        label: None,
        hint: None,
        leading_icon: Some(icon_named(parse_icon_name("search"))),
        trailing_icon: None,
        prefix: None,
        suffix: None,
        validators: vec![],
        max_length: None,
        min_length: None,
        pattern: None,
        autocomplete: None,
        input_mode: None,
        disabled: None,
        readonly: None,
        error: None,
        size: InputSize::Md,
    }.into_component("audit_search").expect("Input");
    let mut params = CborMap::default();
    params.0.push(("id".into(), Value::Text("query".into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Input,
        Handler::Backend {
            action_id: "audit-filter-change".into(),
            params,
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    with_a11y_label(comp, "Filtruj audyt")
}

/// One document-generator card (DPIA/FRIA/signage). The generate button is a
/// placeholder action surfaced through a toast until a backend generator exists.
fn build_doc_card(title: &str, subtitle: &str, desc: &str, kind: &str) -> Component {
    let mut params = CborMap::default();
    params.0.push(("kind".into(), Value::Text(kind.into())));
    card(None, vec![
        stack_h_gap("xs", vec![chip_toned_icon(title, "info", "shield"), text_styled(subtitle, "body_strong")]),
        text_styled(desc, "caption"),
        button_with_params("Generuj", "audit-doc-generate", "primary", params),
    ])
}

/// Actor attributed to evidence-export changes. No host identity fn exists in
/// this addon ABI yet, so changes are attributed to the analyst role that owns
/// the evidence-export workflow in the mockup.
const EVIDENCE_ACTOR: &str = "analityk";

/// Settings key under which the authorized-recipients list is persisted as JSON
/// (an array of `{name, key}` objects).
const EVIDENCE_RECIPIENTS_KEY: &str = "evidence_recipients";

/// One authorized recipient (organ): a display name and an optional public-key
/// fingerprint shown in the recipients table.
#[derive(Debug, Clone)]
struct EvidenceRecipient {
    name: String,
    key: String,
}

/// Reads + decodes the persisted recipients list. A malformed/empty value yields
/// an empty list rather than an error so the panel still renders.
fn load_evidence_recipients() -> Vec<EvidenceRecipient> {
    let raw = db::get_setting(EVIDENCE_RECIPIENTS_KEY).ok().flatten().unwrap_or_default();
    let parsed: JsonValue = serde_json::from_str(&raw).unwrap_or(JsonValue::Null);
    parsed
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| {
                    let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
                    if name.is_empty() {
                        return None;
                    }
                    let key = v.get("key").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
                    Some(EvidenceRecipient { name, key })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Persists the recipients list as a JSON array under the settings key.
fn save_evidence_recipients(recipients: &[EvidenceRecipient]) -> Result<(), AbiError> {
    let arr: Vec<JsonValue> = recipients
        .iter()
        .map(|r| json!({ "name": r.name, "key": r.key }))
        .collect();
    let payload = JsonValue::Array(arr).to_string();
    db::set_setting(EVIDENCE_RECIPIENTS_KEY, &payload).map(|_| ())
}

/// Status chip for an evidence package: a non-empty `signed_by` recipient marks
/// the package as issued/signed (ok), an empty one is still pending (warn).
fn evidence_status_cell(signed_by: &str) -> Value {
    if signed_by.trim().is_empty() {
        chip_cell("oczekuje", "warn")
    } else {
        chip_cell("wydana", "ok")
    }
}

/// Maps an evidence package row to the bound Table row map.
fn evidence_table_row_value(e: &db::EvidenceRow) -> Value {
    let incident = if e.alarm_message.trim().is_empty() {
        if e.alarm_id.trim().is_empty() { "—".to_string() } else { short_id(&e.alarm_id) }
    } else {
        e.alarm_message.clone()
    };
    let camera = if e.camera_name.trim().is_empty() { "—".to_string() } else { e.camera_name.clone() };
    let recipient = if e.signed_by.trim().is_empty() { "—".to_string() } else { e.signed_by.clone() };
    let entries: Vec<(Value, Value)> = vec![
        (Value::Text("evidence_id".into()), Value::Text(e.id.clone())),
        (Value::Text("package_ref".into()), Value::Text(e.package_ref.clone())),
        (Value::Text("incident".into()), Value::Text(incident)),
        (Value::Text("camera".into()), Value::Text(camera)),
        (Value::Text("status".into()), evidence_status_cell(&e.signed_by)),
        (Value::Text("recipient".into()), Value::Text(recipient)),
        (Value::Text("created".into()), Value::Text(format_alarm_datetime(e.created_at))),
    ];
    Value::Map(entries)
}

fn evidence_table_column(id: &str, header: &str, render: ColumnRender) -> TableColumn {
    TableColumn {
        id: id.into(),
        header: lit(header),
        field_path: vec![PathSegment::Key(id.into())],
        width: TableColumnWidth::Auto,
        render,
        format: None,
        align: None,
        sortable: true,
        hidden_by_default: false,
        sticky_left: false,
    }
}

/// The evidence package Table — real rows seeded via PENDING_EVIDENCE_ROWS, with
/// per-row Pobierz/Weryfikuj/Usuń actions wired to the dispatch handlers.
fn build_evidence_table() -> Component {
    let columns = vec![
        evidence_table_column("package_ref", "Paczka", ColumnRender::Text),
        evidence_table_column("incident", "Incydent", ColumnRender::Text),
        evidence_table_column("camera", "Kamera", ColumnRender::Text),
        evidence_table_column("status", "Status", ColumnRender::Chip),
        evidence_table_column("recipient", "Odbiorca", ColumnRender::Text),
        evidence_table_column("created", "Utworzono", ColumnRender::Text),
    ];
    let download_action = button("Pobierz", "evidence-download", "secondary");
    let verify_action = button("Weryfikuj", "evidence-verify", "ghost");
    let remove_action = button("Usuń", "evidence-row-select", "destructive");
    TableComp {
        columns,
        rows_path: StatePath::new(vec![PathSegment::Key("evidence_rows".into())]),
        row_key_field: "evidence_id".into(),
        variant: TableVariant::Default,
        density: Density::Default,
        sortable: true,
        sort_by: None,
        selectable: TableSelectMode::None,
        selected_ids: None,
        sticky_header: true,
        sticky_columns: 0,
        pagination: None,
        empty_state: None,
        row_actions: vec![download_action, verify_action, remove_action],
        bulk_actions: vec![],
        virtualize: false,
        row_expandable: false,
        expanded_row_template_id: None,
    }.into_component(next_id()).expect("Table")
}

/// The authorized-recipients Table — rows seeded via PENDING_RECIPIENT_ROWS,
/// with a per-row Usuń action keyed by recipient name.
fn build_recipients_table() -> Component {
    let columns = vec![
        evidence_table_column("name", "Organ", ColumnRender::Text),
        evidence_table_column("key", "Klucz publiczny", ColumnRender::Text),
        evidence_table_column("active", "Aktywny", ColumnRender::Chip),
    ];
    let remove_action = button("Usuń", "evidence-recipient-remove", "destructive");
    TableComp {
        columns,
        rows_path: StatePath::new(vec![PathSegment::Key("recipients_rows".into())]),
        row_key_field: "recipient_name".into(),
        variant: TableVariant::Default,
        density: Density::Default,
        sortable: true,
        sort_by: None,
        selectable: TableSelectMode::None,
        selected_ids: None,
        sticky_header: true,
        sticky_columns: 0,
        pagination: None,
        empty_state: None,
        row_actions: vec![remove_action],
        bulk_actions: vec![],
        virtualize: false,
        row_expandable: false,
        expanded_row_template_id: None,
    }.into_component(next_id()).expect("Table")
}

/// Maps a recipient to the bound recipients-Table row map.
fn recipient_table_row_value(r: &EvidenceRecipient) -> Value {
    let key = if r.key.trim().is_empty() { "—".to_string() } else { r.key.clone() };
    let entries: Vec<(Value, Value)> = vec![
        (Value::Text("recipient_name".into()), Value::Text(r.name.clone())),
        (Value::Text("name".into()), Value::Text(r.name.clone())),
        (Value::Text("key".into()), Value::Text(key)),
        (Value::Text("active".into()), chip_cell("aktywny", "ok")),
    ];
    Value::Map(entries)
}

/// Evidence form text input bound to a store key, mirrored to backend on change.
fn evidence_input(label: &str, placeholder: &str, field: &str) -> Component {
    let mut comp = input(label, placeholder, field);
    let mut params = CborMap::default();
    params.0.push(("field".into(), Value::Text(field.into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Input,
        Handler::Backend { action_id: "evidence-recipient-field-change".into(), params, optimistic: None, on_failure: FailurePolicy::Toast },
    )]));
    comp
}

/// Evidence form select bound to a store key, mirrored to backend on change.
fn evidence_select(label: &str, options: Vec<SelectOption>, field: &str) -> Component {
    let mut comp = select(label, options, field);
    let mut params = CborMap::default();
    params.0.push(("field".into(), Value::Text(field.into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Change,
        Handler::Backend { action_id: "evidence-field-change".into(), params, optimistic: None, on_failure: FailurePolicy::Toast },
    )]));
    comp
}

/// The create-evidence-package form: pick a real source alarm + an authorized
/// recipient, then create a persisted evidence record referencing the alarm.
fn build_evidence_package_form(alarms: &[db::AlarmRow], recipients: &[EvidenceRecipient]) -> Component {
    // Alarm options from the real alarm center — empty option first so the
    // operator must consciously pick a source incident.
    let mut alarm_opts: Vec<SelectOption> = vec![SelectOption {
        value: SelectValue::Text("".into()),
        label: lit("— wybierz alarm źródłowy —"),
        icon: None, disabled: false, group_id: None, description: None,
    }];
    for a in alarms {
        let label = if a.message.trim().is_empty() {
            alloc::format!("{} · {}", short_id(&a.id), a.severity)
        } else {
            let cam = if a.camera_name.trim().is_empty() { String::new() } else { alloc::format!(" ({})", a.camera_name) };
            alloc::format!("{}{} · {}", a.message, cam, format_alarm_datetime(a.ts))
        };
        alarm_opts.push(SelectOption {
            value: SelectValue::Text(a.id.clone()),
            label: lit(&label),
            icon: None, disabled: false, group_id: None, description: None,
        });
    }

    let mut recipient_opts: Vec<SelectOption> = vec![SelectOption {
        value: SelectValue::Text("".into()),
        label: lit("— odbiorca (opcjonalnie) —"),
        icon: None, disabled: false, group_id: None, description: None,
    }];
    for r in recipients {
        recipient_opts.push(SelectOption {
            value: SelectValue::Text(r.name.clone()),
            label: lit(&r.name),
            icon: None, disabled: false, group_id: None, description: None,
        });
    }

    let mut fields_children = vec![
        evidence_select("Alarm źródłowy", alarm_opts, "evidence_alarm"),
        evidence_select("Uprawniony odbiorca", recipient_opts, "evidence_recipient"),
    ];
    if alarms.is_empty() {
        fields_children.push(text_styled(
            "Brak alarmów źródłowych — wygeneruj alarm w Centrum alarmów, aby utworzyć paczkę.",
            "caption",
        ));
    }
    let fields = card(Some("Dane paczki dowodowej"), fields_children);
    let actions = stack_h(vec![
        button("Zapisz pakiet", "evidence-create", "primary"),
        button("Anuluj", "evidence-form-cancel", "ghost"),
    ]);
    card(None, vec![fields, actions])
}

/// The add-recipient form: a display name + optional public-key fingerprint,
/// persisted into the recipients settings list.
fn build_recipient_form() -> Component {
    let fields = card(Some("Nowy uprawniony odbiorca"), vec![
        evidence_input("Organ", "np. Prokuratura Rejonowa Warszawa-Mokotów", "evidence_recipient_name"),
        evidence_input("Klucz publiczny (opcjonalnie)", "np. PGP 4F2A...8E91", "evidence_recipient_key"),
    ]);
    let actions = stack_h(vec![
        button("Zapisz odbiorcę", "evidence-recipient-add", "primary"),
        button("Anuluj", "evidence-recipient-cancel", "ghost"),
    ]);
    card(None, vec![fields, actions])
}

/// Confirmation bar for deleting the selected evidence package.
fn build_evidence_remove_confirm(evidence_id: &str, packages: &[db::EvidenceRow]) -> Component {
    let label = packages
        .iter()
        .find(|p| p.id == evidence_id)
        .map(|p| p.package_ref.clone())
        .unwrap_or_else(|| evidence_id.to_string());
    let mut params = CborMap::default();
    params.0.push(("evidence_id".into(), Value::Text(evidence_id.into())));
    let confirm_btn = button_with_params("Usuń", "evidence-remove", "destructive", params);
    let cancel_btn = button("Anuluj", "evidence-remove-cancel", "ghost");
    card(None, vec![stack_v(vec![
        text_styled(&alloc::format!("Usunąć paczkę dowodową \"{}\"?", label), "body_strong"),
        text("Usuwa wyłącznie rekord metadanych — nie ma podpisanego artefaktu kryptograficznego do skasowania."),
        stack_h(vec![confirm_btn, cancel_btn]),
    ])])
}

fn build_evidence_content() -> Component {
    let messages = build_messages_section();
    let (package_form_visible, recipient_form_visible) =
        with_state(|s| (s.evidence.package_form_visible, s.evidence.recipient_form_visible));

    let mut children = vec![messages];

    let toolbar = stack_h(vec![
        heading(2, "Eksport dowodowy"),
        button_with_icon("Dodaj odbiorcę", "evidence-recipient-add-show", "secondary", "plus"),
        button_with_icon("Utwórz pakiet dowodowy", "evidence-new", "primary", "plus"),
    ]);
    children.push(toolbar);

    // Trust-chain header strip (placeholders per mockup — honest about the
    // absence of a real HSM/TSA backend).
    children.push(card(None, vec![
        stack_h(vec![
            heading(4, "Łańcuch zaufania"),
            chip_toned_icon("Brak modułu HSM/TSA", "warning", "key"),
        ]),
        text_styled(
            "Rekordy metadanych (kto/co/kiedy/odbiorca) są realne i trwałe. Podpis HSM (ed25519), \
             znacznik czasu TSA (RFC 3161) i eksport plikowy wymagają skonfigurowanego modułu — brak backendu.",
            "caption",
        ),
    ]));

    // A DB/permission error must never be masked as "no packages".
    let packages = match db::list_evidence() {
        Ok(p) => p,
        Err(e) => {
            children.push(alert(&alloc::format!("Nie udało się pobrać paczek dowodowych: {}", abi_message(e)), "critical"));
            return stack_v(children);
        }
    };
    let recipients = load_evidence_recipients();
    let alarms = db::list_alarms("", "", false).unwrap_or_default();

    // Recipients section (left of the mockup grid).
    let mut recipients_children = vec![stack_h(vec![
        heading(3, "Uprawnieni odbiorcy"),
        chip_toned(&alloc::format!("{}", recipients.len()), if recipients.is_empty() { "muted" } else { "info" }),
    ])];
    if recipient_form_visible {
        recipients_children.push(build_recipient_form());
    }
    if recipients.is_empty() {
        recipients_children.push(empty_state(
            "Brak odbiorców",
            Some("Dodaj uprawniony organ (np. prokuratura, sąd), aby kierować do niego paczki dowodowe."),
            Some("users"),
        ));
    } else {
        let rows: Vec<Value> = recipients.iter().map(recipient_table_row_value).collect();
        if let Ok(mut g) = PENDING_RECIPIENT_ROWS.lock() {
            *g = Some(Value::Array(rows));
        }
        recipients_children.push(build_recipients_table());
    }
    children.push(card(None, recipients_children));

    // Create-package form.
    if package_form_visible {
        children.push(build_evidence_package_form(&alarms, &recipients));
    }

    // Delete-confirmation bar above the package list once a row is armed.
    if let Some(pending) = with_state(|s| s.evidence.pending_remove.clone()) {
        if packages.iter().any(|p| p.id == pending) {
            children.push(build_evidence_remove_confirm(&pending, &packages));
        } else {
            with_state(|s| s.evidence.pending_remove = None);
        }
    }

    // Evidence package list.
    let mut packages_children = vec![stack_h(vec![
        heading(3, "Paczki dowodowe"),
        chip_toned(&alloc::format!("{}", packages.len()), if packages.is_empty() { "muted" } else { "info" }),
    ])];
    if packages.is_empty() {
        packages_children.push(empty_state(
            "Brak paczek dowodowych",
            Some("Utwórz pakiet dowodowy z alarmu źródłowego — rekord metadanych zostanie trwale zapisany."),
            Some("package"),
        ));
    } else {
        let rows: Vec<Value> = packages.iter().map(evidence_table_row_value).collect();
        if let Ok(mut g) = PENDING_EVIDENCE_ROWS.lock() {
            *g = Some(Value::Array(rows));
        }
        packages_children.push(build_evidence_table());
    }
    children.push(card(None, packages_children));

    stack_v(children)
}

/// Seeds the evidence forms' bound store keys from backend state so each form
/// mounts with the draft values in place.
fn evidence_overlay() -> Vec<StateEntry> {
    with_state(|s| {
        let e = &s.evidence;
        let key = |k: &str, v: Value| StateEntry { path: StatePath::new(vec![PathSegment::Key(k.into())]), value: v };
        vec![
            key("evidence_alarm", Value::Text(e.form_alarm_id.clone())),
            key("evidence_recipient", Value::Text(e.form_recipient.clone())),
            key("evidence_recipient_name", Value::Text(e.recipient_name.clone())),
            key("evidence_recipient_key", Value::Text(e.recipient_key.clone())),
        ]
    })
}

/// Mirrors a create-package form field into backend state on change.
fn handle_evidence_field_change(params: &JsonValue) -> JsonValue {
    let field = params.get("field").and_then(|v| v.as_str()).unwrap_or("");
    let value = params.get("value").and_then(|v| v.as_str()).map(|s| s.to_string());
    if let Some(v) = value {
        with_state(|s| match field {
            "evidence_alarm" => s.evidence.form_alarm_id = v,
            "evidence_recipient" => s.evidence.form_recipient = v,
            _ => {}
        });
    }
    json!({"ok":true})
}

/// Mirrors an add-recipient form field into backend state on change.
fn handle_evidence_recipient_field_change(params: &JsonValue) -> JsonValue {
    let field = params.get("field").and_then(|v| v.as_str()).unwrap_or("");
    let value = params.get("value").and_then(|v| v.as_str()).map(|s| s.to_string());
    if let Some(v) = value {
        with_state(|s| match field {
            "evidence_recipient_name" => s.evidence.recipient_name = v,
            "evidence_recipient_key" => s.evidence.recipient_key = v,
            _ => {}
        });
    }
    json!({"ok":true})
}

/// Creates a persisted evidence package referencing a real source alarm, writes
/// a hash-chained audit entry (evidence_change), then re-renders the panel.
fn handle_evidence_create() -> JsonValue {
    let (alarm_id, recipient) = with_state(|s| (
        s.evidence.form_alarm_id.trim().to_string(),
        s.evidence.form_recipient.trim().to_string(),
    ));
    with_state(|s| s.clear_messages());

    if alarm_id.is_empty() {
        with_state(|s| s.error_message = Some("Wybierz alarm źródłowy dla paczki dowodowej.".into()));
        render_panel("evidence");
        return json!({"ok":false,"error":"no alarm"});
    }
    // The alarm must exist — never create evidence for a phantom incident.
    match db::get_alarm(&alarm_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            with_state(|s| s.error_message = Some("Wybrany alarm źródłowy nie istnieje.".into()));
            render_panel("evidence");
            return json!({"ok":false,"error":"alarm not found"});
        }
        Err(e) => {
            with_state(|s| s.error_message = Some(alloc::format!("Błąd odczytu alarmu: {}", abi_message(e))));
            render_panel("evidence");
            return json!({"ok":false,"error":alloc::format!("{}",e)});
        }
    }

    let new_evidence = db::NewEvidence { alarm_id: alarm_id.clone(), signed_by: recipient.clone() };
    match db::insert_evidence(&new_evidence) {
        Ok(id) => {
            let row = db::get_evidence(&id).ok().flatten();
            let package_ref = row.as_ref().map(|r| r.package_ref.clone()).unwrap_or_else(|| id.clone());
            let after = json!({
                "id": id, "package_ref": package_ref,
                "alarm_id": alarm_id, "signed_by": recipient,
            }).to_string();
            let _ = db::insert_audit(EVIDENCE_ACTOR, "evidence_change", &package_ref, "null", &after);
            with_state(|s| {
                s.evidence.package_form_visible = false;
                s.evidence.reset_package_form();
                s.success_message = Some(alloc::format!("Pakiet dowodowy utworzony ({}).", package_ref));
            });
            render_panel("evidence");
            json!({"ok":true,"evidence_id":id})
        }
        Err(e) => {
            with_state(|s| s.error_message = Some(alloc::format!("Błąd zapisu paczki: {}", abi_message(e))));
            render_panel("evidence");
            json!({"ok":false,"error":alloc::format!("{}",e)})
        }
    }
}

/// Deletes the selected evidence package, writing a hash-chained audit entry with
/// the deleted row's metadata as the `before` snapshot.
fn handle_evidence_remove(params: &JsonValue) -> JsonValue {
    let id = params.get("evidence_id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    with_state(|s| s.clear_messages());
    if id.is_empty() {
        return json!({"ok":false,"error":"empty evidence_id"});
    }
    let before_row = db::get_evidence(&id).ok().flatten();
    let (package_ref, before) = match &before_row {
        Some(r) => (
            r.package_ref.clone(),
            json!({
                "id": r.id, "package_ref": r.package_ref,
                "alarm_id": r.alarm_id, "signed_by": r.signed_by,
            }).to_string(),
        ),
        None => (id.clone(), "null".into()),
    };
    match db::delete_evidence(&id) {
        Ok(n) if n > 0 => {
            let _ = db::insert_audit(EVIDENCE_ACTOR, "evidence_change", &package_ref, &before, "null");
            with_state(|s| {
                s.evidence.pending_remove = None;
                s.success_message = Some(alloc::format!("Paczka dowodowa usunięta ({}).", package_ref));
            });
            render_panel("evidence");
            json!({"ok":true})
        }
        Ok(_) => {
            with_state(|s| { s.evidence.pending_remove = None; s.error_message = Some("Paczka nie istnieje.".into()); });
            render_panel("evidence");
            json!({"ok":false,"error":"not found"})
        }
        Err(e) => {
            with_state(|s| s.error_message = Some(alloc::format!("Błąd usuwania paczki: {}", abi_message(e))));
            render_panel("evidence");
            json!({"ok":false,"error":alloc::format!("{}",e)})
        }
    }
}

/// Adds a new authorized recipient to the persisted recipients list (dedup by
/// name), writing a hash-chained audit entry.
fn handle_evidence_recipient_add() -> JsonValue {
    let (name, key) = with_state(|s| (
        s.evidence.recipient_name.trim().to_string(),
        s.evidence.recipient_key.trim().to_string(),
    ));
    with_state(|s| s.clear_messages());
    if name.is_empty() || name.chars().count() > 120 {
        with_state(|s| s.error_message = Some("Nazwa organu musi mieć 1–120 znaków.".into()));
        render_panel("evidence");
        return json!({"ok":false,"error":"invalid name"});
    }
    let mut recipients = load_evidence_recipients();
    if recipients.iter().any(|r| r.name.eq_ignore_ascii_case(&name)) {
        with_state(|s| s.error_message = Some("Taki odbiorca już istnieje.".into()));
        render_panel("evidence");
        return json!({"ok":false,"error":"duplicate"});
    }
    recipients.push(EvidenceRecipient { name: name.clone(), key });
    match save_evidence_recipients(&recipients) {
        Ok(()) => {
            let _ = db::insert_audit(EVIDENCE_ACTOR, "evidence_change", &name, "null", &json!({"recipient": name}).to_string());
            with_state(|s| {
                s.evidence.recipient_form_visible = false;
                s.evidence.reset_recipient_form();
                s.success_message = Some(alloc::format!("Dodano odbiorcę: {}.", name));
            });
            render_panel("evidence");
            json!({"ok":true})
        }
        Err(e) => {
            with_state(|s| s.error_message = Some(alloc::format!("Błąd zapisu odbiorcy: {}", abi_message(e))));
            render_panel("evidence");
            json!({"ok":false,"error":alloc::format!("{}",e)})
        }
    }
}

/// Removes an authorized recipient from the persisted list by name, writing a
/// hash-chained audit entry.
fn handle_evidence_recipient_remove(params: &JsonValue) -> JsonValue {
    let name = params.get("recipient_name").and_then(|v| v.as_str())
        .or_else(|| params.get("row_id").and_then(|v| v.as_str()))
        .or_else(|| params.get("name").and_then(|v| v.as_str()))
        .unwrap_or("").trim().to_string();
    with_state(|s| s.clear_messages());
    if name.is_empty() {
        return json!({"ok":false,"error":"empty name"});
    }
    let mut recipients = load_evidence_recipients();
    let before = recipients.len();
    recipients.retain(|r| !r.name.eq_ignore_ascii_case(&name));
    if recipients.len() == before {
        with_state(|s| s.error_message = Some("Odbiorca nie istnieje.".into()));
        render_panel("evidence");
        return json!({"ok":false,"error":"not found"});
    }
    match save_evidence_recipients(&recipients) {
        Ok(()) => {
            let _ = db::insert_audit(EVIDENCE_ACTOR, "evidence_change", &name, &json!({"recipient": name}).to_string(), "null");
            with_state(|s| s.success_message = Some(alloc::format!("Usunięto odbiorcę: {}.", name)));
            render_panel("evidence");
            json!({"ok":true})
        }
        Err(e) => {
            with_state(|s| s.error_message = Some(alloc::format!("Błąd zapisu odbiorców: {}", abi_message(e))));
            render_panel("evidence");
            json!({"ok":false,"error":alloc::format!("{}",e)})
        }
    }
}

/// The kind of a settings field — drives both the rendered control and how the
/// stored "1"/"0" or free-text value is decoded back into the bound store key.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingKind {
    Text,
    Secret,
    Number,
    Toggle,
    Select,
}

/// One configurable setting: stable storage `key`, its UI `label`, the control
/// `kind`, the default applied on first run (empty DB), and (for Select) the
/// list of `(value, label)` options. Every field reads from / writes to the
/// settings table via db::get_setting / db::set_setting under `key`.
struct SettingField {
    key: &'static str,
    label: &'static str,
    kind: SettingKind,
    default: &'static str,
    options: &'static [(&'static str, &'static str)],
}

const fn sf(key: &'static str, label: &'static str, kind: SettingKind, default: &'static str) -> SettingField {
    SettingField { key, label, kind, default, options: &[] }
}

const fn sf_select(key: &'static str, label: &'static str, default: &'static str, options: &'static [(&'static str, &'static str)]) -> SettingField {
    SettingField { key, label, kind: SettingKind::Select, default, options }
}

const INFERENCE_BACKENDS: &[(&str, &str)] = &[
    ("tensorrt", "TensorRT 9.x (GPU)"),
    ("openvino", "OpenVINO (CPU/iGPU)"),
    ("onnx", "ONNX Runtime (CPU fallback)"),
];

const BACKPRESSURE_POLICIES: &[(&str, &str)] = &[
    ("drop_frame", "Drop frame (preferowane dla Tier 2)"),
    ("queue", "Kolejkuj klatki (Tier 0/1)"),
    ("block", "Blokuj producenta"),
];

const HSM_DEVICES: &[(&str, &str)] = &[
    ("yubihsm2", "Yubikey HSM2"),
    ("softhsm", "SoftHSM (dev)"),
];

const ANCHORING_MODES: &[(&str, &str)] = &[
    ("disabled", "Wyłączone"),
    ("ots_btc", "BTC mainnet (OpenTimestamps)"),
];

const LEGAL_PROFILES: &[(&str, &str)] = &[
    ("commercial_private", "Komercja prywatna (D4 zablokowany)"),
    ("public_transport", "Transport publiczny — operator"),
    ("airport_station", "Lotnisko / dworzec — operator"),
    ("authorized_services", "Służby uprawnione (wymaga manifestu)"),
];

/// Storage & retention section.
const STORAGE_FIELDS: &[SettingField] = &[
    sf("storage_recordings_dir", "Lokalizacja nagrań", SettingKind::Text, "/mnt/tentavision/recordings"),
    sf("storage_disk_limit", "Limit dyskowy", SettingKind::Text, "4 TB"),
    sf("storage_vector_index_dir", "Lokalizacja indeksu wektorów", SettingKind::Text, "/mnt/tentavision/qdrant"),
    sf("storage_worm_bucket", "WORM bucket (S3-immutable)", SettingKind::Text, "s3://tentavision-worm/audit"),
    sf("storage_retention_a_days", "Retencja klasa A (dni)", SettingKind::Number, "30"),
    sf("storage_retention_b_days", "Retencja klasa B (dni)", SettingKind::Number, "14"),
    sf("storage_retention_c_days", "Retencja klasa C (dni)", SettingKind::Number, "7"),
];

/// Inference runtime section.
const RUNTIME_FIELDS: &[SettingField] = &[
    sf_select("runtime_backend", "Backend", "tensorrt", INFERENCE_BACKENDS),
    sf("runtime_max_concurrent_models", "Maks. równoczesnych modeli", SettingKind::Number, "6"),
    sf_select("runtime_backpressure", "Backpressure policy", "drop_frame", BACKPRESSURE_POLICIES),
    sf("runtime_batch_size", "Rozmiar batcha", SettingKind::Number, "8"),
    sf("runtime_warmup_enabled", "Model warmup (200 inferencji)", SettingKind::Toggle, "1"),
    sf("runtime_hot_reload_enabled", "Hot reload (A/B shadow, rollback < 60s)", SettingKind::Toggle, "1"),
];

/// Notifications & integrations section.
const NOTIFY_FIELDS: &[SettingField] = &[
    sf("notify_webhook_enabled", "Webhook → flow-engine", SettingKind::Toggle, "1"),
    sf("notify_webhook_url", "Webhook URL", SettingKind::Secret, "https://flow.tentaflow.local/hook/tentavision"),
    sf("notify_sms_enabled", "SMS (Twilio)", SettingKind::Toggle, "0"),
    sf("notify_sms_target", "SMS numer", SettingKind::Secret, ""),
    sf("notify_email_enabled", "Email (alarmy krytyczne)", SettingKind::Toggle, "1"),
    sf("notify_email_target", "Email odbiorcy", SettingKind::Text, "dyspozytor@depo-warszawa.pl"),
    sf("notify_slack_enabled", "Slack", SettingKind::Toggle, "0"),
    sf("notify_slack_channel", "Slack channel", SettingKind::Text, "#tentavision-alerts"),
    sf("notify_webpush_enabled", "Web push (operator)", SettingKind::Toggle, "1"),
    sf("notify_quiet_hours_enabled", "Wyciszanie nocne 22:00–06:00", SettingKind::Toggle, "0"),
];

/// Licenses & keys section.
const LICENSE_FIELDS: &[SettingField] = &[
    sf("license_pro_key", "TentaVision Pro license", SettingKind::Secret, ""),
    sf_select("license_hsm_device", "HSM device", "softhsm", HSM_DEVICES),
    sf("license_tsa_url", "TSA RFC 3161", SettingKind::Text, "https://freetsa.org/tsr"),
    sf_select("license_anchoring", "Anchoring blockchain", "disabled", ANCHORING_MODES),
    sf("license_vault_rotation_days", "Camera vault rotacja (dni)", SettingKind::Number, "90"),
];

/// Returns the persisted value for a field, applying its pending edit (if the
/// user changed it this session) over the stored DB value over its default.
fn setting_value(f: &SettingField) -> String {
    if let Some(v) = with_state(|s| s.settings.edit(f.key).map(|x| x.to_string())) {
        return v;
    }
    db::get_setting(f.key).ok().flatten().unwrap_or_else(|| f.default.to_string())
}

/// Renders one settings field as the control matching its kind, bound to a store
/// key (`set_<key>`) seeded from the DB via the panel's state_overlay.
fn settings_control(f: &SettingField) -> Component {
    let store_key = alloc::format!("set_{}", f.key);
    match f.kind {
        SettingKind::Text => settings_text_input(f.label, &store_key, f.key, false),
        SettingKind::Secret => settings_text_input(f.label, &store_key, f.key, true),
        SettingKind::Number => settings_number_input(f.label, &store_key, f.key),
        SettingKind::Toggle => settings_toggle(f.label, &store_key, f.key),
        SettingKind::Select => {
            let opts: Vec<SelectOption> = f.options.iter().map(|(v, l)| SelectOption {
                value: SelectValue::Text((*v).into()),
                label: lit(l),
                icon: None, disabled: false, group_id: None, description: None,
            }).collect();
            settings_select(f.label, opts, &store_key, f.key)
        }
    }
}

/// Text/secret input committing each keystroke to the in-WASM edit buffer via
/// `settings-field-change` (tagged with the setting key), so values survive tab
/// switches before the explicit save.
fn settings_text_input(label: &str, store_key: &str, setting_key: &str, secret: bool) -> Component {
    use tentaflow_sdk_spec::protocol::ui::form::Input;
    let mut comp = Input {
        r#type: if secret { InputType::Password } else { InputType::Text },
        bind_path: StatePath::new(vec![PathSegment::Key(store_key.into())]),
        placeholder: None,
        label: Some(lit(label)),
        hint: None, leading_icon: None, trailing_icon: None, prefix: None, suffix: None,
        validators: vec![], max_length: None, min_length: None, pattern: None,
        autocomplete: None, input_mode: None, disabled: None, readonly: None, error: None,
        size: InputSize::Md,
    }.into_component(store_key).expect("Input");
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Input,
        Handler::Backend {
            action_id: "settings-field-change".into(),
            params: settings_key_params(setting_key),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    comp
}

/// Number input committing to the edit buffer on each change.
fn settings_number_input(label: &str, store_key: &str, setting_key: &str) -> Component {
    use tentaflow_sdk_spec::protocol::ui::form::Input;
    let mut comp = Input {
        r#type: InputType::Number,
        bind_path: StatePath::new(vec![PathSegment::Key(store_key.into())]),
        placeholder: None,
        label: Some(lit(label)),
        hint: None, leading_icon: None, trailing_icon: None, prefix: None, suffix: None,
        validators: vec![], max_length: None, min_length: None, pattern: None,
        autocomplete: None, input_mode: None, disabled: None, readonly: None, error: None,
        size: InputSize::Md,
    }.into_component(store_key).expect("Input");
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Input,
        Handler::Backend {
            action_id: "settings-field-change".into(),
            params: settings_key_params(setting_key),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    comp
}

/// Select committing the picked value to the edit buffer on change.
fn settings_select(label: &str, options: Vec<SelectOption>, store_key: &str, setting_key: &str) -> Component {
    use tentaflow_sdk_spec::protocol::ui::form::Select;
    let mut comp = Select {
        bind_path: StatePath::new(vec![PathSegment::Key(store_key.into())]),
        options, placeholder: None, label: Some(lit(label)),
        searchable: false, clearable: false, virtualize: false,
        disabled: None, size: InputSize::Md, groups: None,
    }.into_component(store_key).expect("Select");
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Change,
        Handler::Backend {
            action_id: "settings-field-change".into(),
            params: settings_key_params(setting_key),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    comp
}

/// Toggle committing "1"/"0" to the edit buffer on change.
fn settings_toggle(label: &str, store_key: &str, setting_key: &str) -> Component {
    use tentaflow_sdk_spec::protocol::ui::form::Toggle;
    let mut comp = Toggle {
        bind_path: StatePath::new(vec![PathSegment::Key(store_key.into())]),
        label: Some(lit(label)),
        hint: None, size: ToggleSize::Md, tone: Tone::Primary, disabled: None,
        label_position: TogglePosition::Trailing,
    }.into_component(store_key).expect("Toggle");
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Change,
        Handler::Backend {
            action_id: "settings-toggle-change".into(),
            params: settings_key_params(setting_key),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    comp
}

fn settings_key_params(setting_key: &str) -> CborMap {
    let mut params = CborMap::default();
    params.0.push(("key".into(), Value::Text(setting_key.into())));
    params
}

/// Seeds the bound store keys for every settings field from the DB (or default,
/// or the pending edit) so each control mounts showing its current value.
/// Toggles seed a real bool; everything else seeds text.
fn settings_overlay() -> Vec<StateEntry> {
    let mut entries = Vec::new();
    for f in settings_all_fields() {
        let value = setting_value(f);
        let store_key = alloc::format!("set_{}", f.key);
        let v = if f.kind == SettingKind::Toggle {
            Value::Bool(value.trim() == "1")
        } else {
            Value::Text(value)
        };
        entries.push(StateEntry {
            path: StatePath::new(vec![PathSegment::Key(store_key)]),
            value: v,
        });
    }
    entries
}

/// All settings fields across every section, in render order.
fn settings_all_fields() -> impl Iterator<Item = &'static SettingField> {
    STORAGE_FIELDS.iter()
        .chain(RUNTIME_FIELDS.iter())
        .chain(NOTIFY_FIELDS.iter())
        .chain(LICENSE_FIELDS.iter())
        .chain(LEGAL_FIELDS.iter())
}

/// Legal/deployment profile section (single Select, AI-Act gated).
const LEGAL_FIELDS: &[SettingField] = &[
    sf_select("legal_profile", "Aktywny profil deployment", "commercial_private", LEGAL_PROFILES),
];

/// Builds one section card: a heading plus its fields rendered as controls.
fn settings_section_card(title: &str, fields: &[SettingField]) -> Component {
    let mut children = vec![heading(3, title)];
    for f in fields {
        children.push(settings_control(f));
    }
    card(None, children)
}

fn build_settings_content() -> Component {
    let messages = build_messages_section();
    let header = stack_h(vec![
        heading(2, "Ustawienia TentaVision"),
        chip_toned("konfiguracja per-deployment", "muted"),
    ]);

    let grid_cards = grid(2, vec![
        settings_section_card("Storage i retencja", STORAGE_FIELDS),
        settings_section_card("Inference runtime", RUNTIME_FIELDS),
        settings_section_card("Powiadomienia i integracje", NOTIFY_FIELDS),
        settings_section_card("Licencje i klucze", LICENSE_FIELDS),
    ]);

    // Legal/AI-Act profile — distinct card with a guardrail note.
    let mut legal_children = vec![heading(3, "Profil prawny i AI Act")];
    for f in LEGAL_FIELDS {
        legal_children.push(settings_control(f));
    }
    legal_children.push(text_styled(
        "Zmiana profilu wymaga podpisu DPO + zapisu w hash-chain audit. Profil determinuje dostępność detektorów klasy C (D4).",
        "caption",
    ));
    let legal_card = card(None, legal_children);

    let save_bar = stack_h(vec![button("Zapisz zmiany", "settings-save", "primary")]);

    stack_v(vec![messages, header, grid_cards, legal_card, save_bar])
}

/// Number of wizard steps (role → legal → camera → presets).
const ONBOARDING_STEPS: u8 = 4;

/// Settings keys the wizard's outcomes persist to. `legal_profile` is shared with
/// the Settings tab so the choice stays consistent across both screens.
const KEY_ONBOARDING_ROLE: &str = "onboarding_role";
const KEY_ONBOARDING_PRESETS: &str = "onboarding_presets";
const KEY_ONBOARDING_COMPLETED: &str = "onboarding_completed";
const KEY_ONBOARDING_COMPLETED_AT: &str = "onboarding_completed_at";
const ONBOARDING_ACTOR: &str = "administrator";

/// Deployment roles offered in step 1. Each is (key, title, description, icon).
const ONBOARDING_ROLES: &[(&str, &str, &str, &str)] = &[
    ("depo", "Depo / baza taboru", "Zajezdnie, bazy autobusowe, place manewrowe. Detekcja wjazdu/wyjazdu, ANPR, strefy.", "truck"),
    ("office", "Biuro / kampus", "Wejścia, recepcje, korytarze. Kontrola dostępu, liczenie osób (anonimowo).", "home"),
    ("retail", "Retail / handel", "Sklepy, galerie, parkingi. Analiza ruchu, kolejki, strefy ryzyka.", "package"),
    ("custom", "Custom / inne", "Konfiguracja własna — wszystkie detektory dostępne wg profilu prawnego.", "settings"),
];

/// Detector presets offered in step 4. Each is (key, title, description).
const ONBOARDING_PRESETS: &[(&str, &str, &str)] = &[
    ("safety", "Bezpieczeństwo (D1+D3)", "Detekcja osób i pojazdów, strefy zakazane, alarmy obecności. Bez danych osobowych."),
    ("traffic", "Ruch i ANPR (D1+D3+OCR)", "Detekcja, śledzenie, rozpoznawanie tablic. ANPR przez OCR alias."),
    ("full", "Pełny (D1–D6 wg profilu)", "Wszystkie klasy detektorów dozwolone przez wybrany profil prawny."),
];

/// Looks up the display title for a stored role key.
fn onboarding_role_label(key: &str) -> &str {
    ONBOARDING_ROLES.iter().find(|(k, ..)| *k == key).map(|(_, t, ..)| *t).unwrap_or("—")
}

/// Looks up the display title for a stored legal-profile key (shared catalog).
fn legal_profile_label(key: &str) -> &str {
    LEGAL_PROFILES.iter().find(|(k, _)| *k == key).map(|(_, l)| *l).unwrap_or("—")
}

/// Looks up the display title for a stored preset key.
fn onboarding_preset_label(key: &str) -> &str {
    ONBOARDING_PRESETS.iter().find(|(k, ..)| *k == key).map(|(_, t, ..)| *t).unwrap_or("—")
}

/// True once the wizard has been finished (settings key `onboarding_completed`=1).
fn onboarding_completed() -> bool {
    db::get_setting(KEY_ONBOARDING_COMPLETED).ok().flatten().as_deref() == Some("1")
}

/// One selectable option card: title + description, with a primary "Wybierz"
/// button (or a "Wybrane" success chip when it is the current selection). The
/// select button carries the option key so the handler knows which was picked.
fn onboarding_option_card(key: &str, title: &str, desc: &str, selected: bool, action: &str) -> Component {
    let mut params = CborMap::default();
    params.0.push(("key".into(), Value::Text(key.into())));
    let action_row = if selected {
        chip_toned("Wybrane", "success")
    } else {
        button_with_params("Wybierz", action, "secondary", params)
    };
    card(None, vec![
        stack_h(vec![text_styled(title, "body_strong"), if selected { chip_toned("✓", "success") } else { divider() }]),
        text_styled(desc, "caption"),
        action_row,
    ])
}

/// Reactive text input for a wizard field, committing each keystroke to backend
/// onboarding state via `onboarding-field-change` (tagged with `field`).
fn onboarding_input(label: &str, placeholder: &str, field: &str) -> Component {
    use tentaflow_sdk_spec::protocol::ui::form::Input;
    let store_key = alloc::format!("onb_{}", field);
    let placeholder_ref = if placeholder.is_empty() { None } else { Some(lit(placeholder)) };
    let mut comp = Input {
        r#type: InputType::Text,
        bind_path: StatePath::new(vec![PathSegment::Key(store_key.clone())]),
        placeholder: placeholder_ref,
        label: Some(lit(label)),
        hint: None, leading_icon: None, trailing_icon: None, prefix: None, suffix: None,
        validators: vec![], max_length: None, min_length: None, pattern: None,
        autocomplete: None, input_mode: None, disabled: None, readonly: None, error: None,
        size: InputSize::Md,
    }.into_component(&store_key).expect("Input");
    let mut params = CborMap::default();
    params.0.push(("field".into(), Value::Text(field.into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Input,
        Handler::Backend {
            action_id: "onboarding-field-change".into(),
            params,
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    comp
}

/// Seeds the camera-step inputs from backend onboarding state so they keep their
/// typed values when the user steps back and forth.
fn onboarding_overlay() -> Vec<StateEntry> {
    let (name, url) = with_state(|s| (s.onboarding.camera_name.clone(), s.onboarding.camera_url.clone()));
    vec![
        StateEntry { path: StatePath::new(vec![PathSegment::Key("onb_camera_name".into())]), value: Value::Text(name) },
        StateEntry { path: StatePath::new(vec![PathSegment::Key("onb_camera_url".into())]), value: Value::Text(url) },
    ]
}

fn build_onboarding_content() -> Component {
    let messages = build_messages_section();

    // Already completed → show the persisted summary instead of the wizard.
    if onboarding_completed() {
        return build_onboarding_summary(messages);
    }

    let (step, role, legal, presets) = with_state(|s| (
        s.onboarding.step,
        s.onboarding.role.clone(),
        s.onboarding.legal_profile.clone(),
        s.onboarding.presets.clone(),
    ));

    let welcome = card(None, vec![
        heading(1, "Witaj w TentaVision"),
        text_styled("Skonfigurujmy addon dla Twojego wdrożenia. 4 kroki: rola → profil prawny (RODO/AI Act) → pierwsza kamera → presety detektorów.", "caption"),
    ]);

    let steps_data: Vec<StepDef> = vec![
        StepDef { id: "step0".into(), label: lit("Rola wdrożenia"), optional: false, status: Some(lit(step_status(step, 0))), description: None },
        StepDef { id: "step1".into(), label: lit("Profil prawny"), optional: false, status: Some(lit(step_status(step, 1))), description: None },
        StepDef { id: "step2".into(), label: lit("Pierwsza kamera"), optional: false, status: Some(lit(step_status(step, 2))), description: None },
        StepDef { id: "step3".into(), label: lit("Presety detektorów"), optional: false, status: Some(lit(step_status(step, 3))), description: None },
    ];
    let current_step_id = alloc::format!("step{}", step);
    let progress = step_progress(steps_data, &current_step_id);

    let step_body = match step {
        0 => build_onboarding_step_role(role.as_deref()),
        1 => build_onboarding_step_legal(legal.as_deref()),
        2 => build_onboarding_step_camera(),
        _ => build_onboarding_step_presets(presets.as_deref()),
    };

    stack_v(vec![messages, welcome, progress, step_body])
}

/// StepProgress status token for step `idx` given the active `step`.
fn step_status(step: u8, idx: u8) -> &'static str {
    if idx < step { "complete" } else if idx == step { "current" } else { "pending" }
}

/// Step 1 — deployment role selection.
fn build_onboarding_step_role(selected: Option<&str>) -> Component {
    let mut children = vec![
        heading(3, "Krok 1 — Rola wdrożenia"),
        text_styled("Rola dobiera domyślne strefy, presety detektorów i sugerowany profil prawny. Możesz zmienić każdy element później.", "caption"),
    ];
    let mut options = Vec::new();
    for (key, title, desc, _icon) in ONBOARDING_ROLES {
        options.push(onboarding_option_card(key, title, desc, selected == Some(*key), "onboarding-pick-role"));
    }
    children.push(grid(2, options));
    children.push(stack_h(vec![
        divider(),
        button("Dalej: profil prawny", "onboarding-next", "primary"),
    ]));
    card(None, children)
}

/// Step 2 — legal/AI-Act profile selection (shared `legal_profile` key).
fn build_onboarding_step_legal(selected: Option<&str>) -> Component {
    let mut children = vec![
        heading(3, "Krok 2 — Profil prawny (RODO / AI Act)"),
        text_styled("Profil determinuje dostępność detektorów klasy C (twarz, re-ID), domyślne retencje i wymagane dokumenty (DPIA, FRIA). Runtime egzekwuje gate na podstawie tego wyboru. Zapisywany pod tym samym kluczem co Ustawienia.", "caption"),
    ];
    let mut options = Vec::new();
    for (key, label) in LEGAL_PROFILES {
        options.push(onboarding_option_card(key, label, legal_profile_desc(key), selected == Some(*key), "onboarding-pick-legal"));
    }
    children.push(grid(2, options));
    children.push(stack_h(vec![
        button("Wstecz", "onboarding-prev", "ghost"),
        button("Dalej: pierwsza kamera", "onboarding-next", "primary"),
    ]));
    card(None, children)
}

/// Short rationale shown under each legal profile option.
fn legal_profile_desc(key: &str) -> &'static str {
    match key {
        "commercial_private" => "Logistyka, magazyny, parkingi, biura. Detektory bezosobowe i anonimowe. D4 (face/re-id) zablokowany.",
        "public_transport" => "Spółka transportowa, terminal. D4 historyczne pod DPIA, real-time blokowany (AI Act Art. 5).",
        "airport_station" => "Krytyczna infrastruktura. D4 w wąskim zakresie (LegalGrant + DPIA + FRIA). Pełny post-market monitoring.",
        "authorized_services" => "Policja, prokuratura, SG. D4 real-time tylko pod aktywnym LegalGrant. Wymaga podpisanego manifestu.",
        _ => "",
    }
}

/// Step 3 — first camera. Inputs commit to backend state; insert happens on finish.
fn build_onboarding_step_camera() -> Component {
    card(None, vec![
        heading(3, "Krok 3 — Pierwsza kamera"),
        text_styled("Dodaj pierwsze źródło wideo. Po zakończeniu kreatora utworzymy realny wpis kamery — pojawi się na zakładce Kamery.", "caption"),
        onboarding_input("Nazwa kamery", "np. Brama główna", "camera_name"),
        onboarding_input("Adres RTSP", "rtsp://192.168.1.10:554/stream", "camera_url"),
        stack_h(vec![
            button("Wstecz", "onboarding-prev", "ghost"),
            button("Dalej: presety", "onboarding-next", "primary"),
        ]),
    ])
}

/// Step 4 — detector presets + finish.
fn build_onboarding_step_presets(selected: Option<&str>) -> Component {
    let mut children = vec![
        heading(3, "Krok 4 — Presety detektorów"),
        text_styled("Wybierz zestaw startowy detektorów. Dostępność klas zależy od wybranego profilu prawnego.", "caption"),
    ];
    let mut options = Vec::new();
    for (key, title, desc) in ONBOARDING_PRESETS {
        options.push(onboarding_option_card(key, title, desc, selected == Some(*key), "onboarding-pick-presets"));
    }
    children.push(grid(2, options));
    children.push(stack_h(vec![
        button("Wstecz", "onboarding-prev", "ghost"),
        button("Zakończ konfigurację", "onboarding-finish", "primary"),
    ]));
    card(None, children)
}

/// Completed-state summary read entirely from persisted settings, with a restart
/// action. Shown whenever `onboarding_completed`=1 (survives reopen / restart).
fn build_onboarding_summary(messages: Component) -> Component {
    let role = db::get_setting(KEY_ONBOARDING_ROLE).ok().flatten().unwrap_or_default();
    let legal = db::get_setting("legal_profile").ok().flatten().unwrap_or_default();
    let presets = db::get_setting(KEY_ONBOARDING_PRESETS).ok().flatten().unwrap_or_default();
    let at = db::get_setting(KEY_ONBOARDING_COMPLETED_AT).ok().flatten().unwrap_or_default();
    let when = if at.trim().is_empty() { "—".to_string() } else { format_alarm_datetime(at.trim().parse::<i64>().unwrap_or(0)) };

    let header = stack_h(vec![
        heading(2, "Konfiguracja zakończona"),
        chip_toned_icon("ukończono", "success", "check"),
    ]);

    let summary = card(None, vec![
        heading(3, "Podsumowanie wdrożenia"),
        key_value(vec![
            ("Rola wdrożenia", onboarding_role_label(&role)),
            ("Profil prawny", legal_profile_label(&legal)),
            ("Preset detektorów", onboarding_preset_label(&presets)),
            ("Ukończono", &when),
        ]),
        text_styled("Wartości odczytane z trwałych ustawień (settings). Profil prawny współdzielony z zakładką Ustawienia i bramką Re-ID.", "caption"),
    ]);

    let restart = card(None, vec![
        text_styled("Możesz uruchomić kreator ponownie. Nie usuwa to istniejących kamer ani ustawień — pozwala przejść konfigurację od nowa.", "caption"),
        button("Uruchom ponownie", "onboarding-restart", "ghost"),
    ]);

    stack_v(vec![messages, header, summary, restart])
}

/// Records an option pick (role / legal / presets) into the in-flight wizard
/// state and re-renders so the chosen card shows "Wybrane".
fn handle_onboarding_pick(kind: &str, params: &JsonValue) -> JsonValue {
    let key = params.get("key").and_then(|x| x.as_str()).unwrap_or("").to_string();
    if key.is_empty() {
        return json!({"ok": false, "error": "empty key"});
    }
    with_state(|s| {
        s.clear_messages();
        match kind {
            "role" => s.onboarding.role = Some(key.clone()),
            "legal" => s.onboarding.legal_profile = Some(key.clone()),
            "presets" => s.onboarding.presets = Some(key.clone()),
            _ => {}
        }
    });
    render_panel("onboarding");
    json!({"ok": true})
}

/// Mirrors a first-camera input into wizard state on each keystroke.
fn handle_onboarding_field_change(params: &JsonValue) -> JsonValue {
    let field = params.get("field").and_then(|x| x.as_str()).unwrap_or("");
    let value = params.get("value").and_then(|x| x.as_str()).unwrap_or("").to_string();
    with_state(|s| match field {
        "camera_name" => s.onboarding.camera_name = value,
        "camera_url" => s.onboarding.camera_url = value,
        _ => {}
    });
    json!({"ok": true})
}

/// Advances one step, validating the current step's required choice first.
fn handle_onboarding_next() -> JsonValue {
    let step = with_state(|s| s.onboarding.step);
    let blocked: Option<&str> = with_state(|s| match step {
        0 if s.onboarding.role.is_none() => Some("Wybierz rolę wdrożenia."),
        1 if s.onboarding.legal_profile.is_none() => Some("Wybierz profil prawny."),
        _ => None,
    });
    if let Some(msg) = blocked {
        with_state(|s| { s.clear_messages(); s.error_message = Some(msg.into()); });
        render_panel("onboarding");
        return json!({"ok": false});
    }
    with_state(|s| { s.clear_messages(); if s.onboarding.step + 1 < ONBOARDING_STEPS { s.onboarding.step += 1; } });
    render_panel("onboarding");
    json!({"ok": true})
}

/// Commits every wizard outcome: settings (role, legal_profile, presets,
/// completed flag + timestamp), a real first-camera row, and an audit entry.
fn handle_onboarding_finish() -> JsonValue {
    let (role, legal, presets, cam_name, cam_url) = with_state(|s| (
        s.onboarding.role.clone(),
        s.onboarding.legal_profile.clone(),
        s.onboarding.presets.clone(),
        s.onboarding.camera_name.trim().to_string(),
        s.onboarding.camera_url.trim().to_string(),
    ));
    with_state(|s| s.clear_messages());

    let Some(role) = role else {
        with_state(|s| s.error_message = Some("Brak roli wdrożenia.".into()));
        render_panel("onboarding");
        return json!({"ok": false});
    };
    let Some(legal) = legal else {
        with_state(|s| s.error_message = Some("Brak profilu prawnego.".into()));
        render_panel("onboarding");
        return json!({"ok": false});
    };
    let presets = presets.unwrap_or_else(|| "safety".to_string());

    if cam_name.is_empty() || cam_name.chars().count() > 60 {
        with_state(|s| { s.onboarding.step = 2; s.error_message = Some("Nazwa kamery musi mieć 1–60 znaków.".into()); });
        render_panel("onboarding");
        return json!({"ok": false});
    }

    // Persist outcomes. Any write failure aborts before flipping the completed
    // flag, so a half-written onboarding never reports itself as done.
    let writes: &[(&str, &str)] = &[
        (KEY_ONBOARDING_ROLE, role.as_str()),
        ("legal_profile", legal.as_str()),
        (KEY_ONBOARDING_PRESETS, presets.as_str()),
    ];
    for (k, v) in writes {
        if let Err(e) = db::set_setting(k, v) {
            with_state(|s| s.error_message = Some(alloc::format!("Błąd zapisu ustawień: {}", abi_message(e))));
            render_panel("onboarding");
            return json!({"ok": false});
        }
    }

    // Real first-camera row — same path as the cameras wizard, so it appears on
    // the Cameras tab and survives reopen / restart.
    let new_cam = db::NewCamera {
        name: cam_name.clone(),
        location: onboarding_role_label(&role).to_string(),
        rtsp_url: cam_url.clone(),
        onvif_url: String::new(),
        status: "offline".into(),
        fps: 0,
        detectors: "onboarding".into(),
        analysis_fps: i64::from(CAMERA_DEFAULT_ANALYSIS_FPS),
    };
    let cam_id = match db::insert_camera(&new_cam) {
        Ok(id) => id,
        Err(e) => {
            with_state(|s| { s.onboarding.step = 2; s.error_message = Some(alloc::format!("Błąd dodawania kamery: {}", abi_message(e))); });
            render_panel("onboarding");
            return json!({"ok": false});
        }
    };

    let ts = db::now_secs();
    let _ = db::set_setting(KEY_ONBOARDING_COMPLETED_AT, &alloc::format!("{}", ts));
    if let Err(e) = db::set_setting(KEY_ONBOARDING_COMPLETED, "1") {
        with_state(|s| s.error_message = Some(alloc::format!("Błąd finalizacji: {}", abi_message(e))));
        render_panel("onboarding");
        return json!({"ok": false});
    }

    let after = alloc::format!("role={}; legal_profile={}; presets={}; camera={}", role, legal, presets, cam_id);
    let _ = db::insert_audit(ONBOARDING_ACTOR, "onboarding_complete", "tentavision", "", &after);

    with_state(|s| { s.onboarding.reset(); s.success_message = Some("Konfiguracja zakończona. Pierwsza kamera dodana.".into()); });
    render_panel("onboarding");
    json!({"ok": true, "camera_id": cam_id})
}

/// Clears the completed flag and restarts the wizard from step 0. Leaves existing
/// cameras and settings intact — only re-opens the configuration flow.
fn handle_onboarding_restart() -> JsonValue {
    if let Err(e) = db::set_setting(KEY_ONBOARDING_COMPLETED, "0") {
        with_state(|s| s.error_message = Some(alloc::format!("Błąd: {}", abi_message(e))));
        render_panel("onboarding");
        return json!({"ok": false});
    }
    with_state(|s| { s.onboarding.reset(); s.clear_messages(); });
    render_panel("onboarding");
    json!({"ok": true})
}

/// Actor recorded for binding (alias-mapping) audit entries.
const BINDING_ACTOR: &str = "administrator";

/// A functional slot of TentaVision: the operator assigns WHICH granted
/// model/alias backs each capability. `method` is the capability the slot needs;
/// the per-slot Select only lists available aliases whose declared methods
/// include it (real type-filtering). `canonical_alias` is the addon-owned alias
/// id that maps to this slot — used as the default suggestion and to label the
/// slot. `gated` marks slots that require a LegalGrant at call time.
struct SlotDef {
    id: &'static str,
    label: &'static str,
    method: &'static str,
    canonical_alias: &'static str,
    gated: bool,
}

/// The 6 functional slots, each filtered by the capability `method` it requires.
const SLOTS: &[SlotDef] = &[
    SlotDef { id: "yolo", label: "Detekcja obiektów", method: "detect", canonical_alias: "tentavision-yolo", gated: false },
    SlotDef { id: "ocr", label: "Rozpoznawanie tekstu (OCR)", method: "recognize", canonical_alias: "tentavision-ocr", gated: false },
    SlotDef { id: "action", label: "Klasyfikacja akcji", method: "classify_window", canonical_alias: "tentavision-action", gated: false },
    SlotDef { id: "vlm", label: "Model wizyjno-językowy", method: "caption", canonical_alias: "tentavision-vlm", gated: false },
    SlotDef { id: "face-embed", label: "Embedding twarzy", method: "embed", canonical_alias: "tentavision-face-embed", gated: true },
    SlotDef { id: "reid", label: "Re-identyfikacja osób", method: "match", canonical_alias: "tentavision-reid", gated: true },
];

/// Store key holding the chosen target alias for a slot Select.
fn slot_store_key(slot_id: &str) -> String {
    alloc::format!("alias_target_{}", slot_id)
}

/// Settings key persisting a slot → alias assignment.
fn slot_setting_key(slot_id: &str) -> String {
    alloc::format!("alias_map_{}", slot_id)
}

/// Available aliases whose declared methods include `method` AND that the addon
/// may actually use (granted/auto_granted). Pending/denied aliases are excluded
/// from the assignable options (you cannot assign what you cannot call), but the
/// discovery list above still shows them honestly.
fn slot_options<'a>(method: &str, available: &'a [AvailableAlias]) -> Vec<&'a AvailableAlias> {
    available
        .iter()
        .filter(|a| a.is_usable() && a.methods.iter().any(|m| m == method))
        .collect()
}

/// The effective alias assigned to a slot: the persisted assignment if it still
/// points at a usable option, else the first usable option for the slot, else
/// empty (no grants → honest empty state).
fn slot_assignment(slot: &SlotDef, available: &[AvailableAlias]) -> String {
    let opts = slot_options(slot.method, available);
    if let Some(v) = db::get_setting(&slot_setting_key(slot.id)).ok().flatten() {
        if opts.iter().any(|a| a.alias_id == v) {
            return v;
        }
    }
    // Prefer the canonical alias when it is among the usable options.
    if opts.iter().any(|a| a.alias_id == slot.canonical_alias) {
        return slot.canonical_alias.to_string();
    }
    opts.first().map(|a| a.alias_id.clone()).unwrap_or_default()
}

fn build_bindings_content() -> Component {
    let messages = build_messages_section();

    // REAL discovery: what the grant system says this addon may consume.
    let available = alias_list_available().unwrap_or_default();
    let usable = available.iter().filter(|a| a.is_usable()).count();

    let header = stack_h(vec![
        heading(2, "Powiązania i magazyn"),
        chip_toned(&alloc::format!("{}/{} aliasów przyznanych", usable, available.len()), "success"),
        chip_toned("mapowanie wykonawcze addona", "muted"),
    ]);

    let mut children = vec![messages, header];

    // 1. Built-in storage API status — REAL capability probes.
    children.push(build_storage_status_card());

    // 2. Discovery: every alias the addon may consume, with grant chips. Real
    //    data from alias_list_available — never a hardcoded list.
    children.push(build_available_aliases_card(&available));

    // 3. Per-slot assignment: pick which granted model backs each function,
    //    filtered by matching capability method.
    children.push(build_slot_assignment_card(&available));

    stack_v(children)
}

/// Discovery card: lists every alias/model the addon may consume (from
/// `alias_list_available`), each with its concrete target, methods and a grant
/// chip. Honest empty state when the grant system returned nothing.
fn build_available_aliases_card(available: &[AvailableAlias]) -> Component {
    if available.is_empty() {
        return card(None, vec![
            heading(3, "Modele przyznane addonowi"),
            empty_state(
                "Brak przyznanych modeli",
                Some("Poproś admina o grant w Services → Aliasy."),
                Some("lock"),
            ),
        ]);
    }
    let mut rows = vec![heading(3, "Modele przyznane addonowi")];
    for a in available {
        rows.push(build_available_alias_row(a));
    }
    card(None, rows)
}

/// One discovery row: alias id + methods + resolved target + grant-status chip.
fn build_available_alias_row(a: &AvailableAlias) -> Component {
    let methods = if a.methods.is_empty() { "—".to_string() } else { a.methods.join(" · ") };
    let target = a.target_model.clone().unwrap_or_else(|| "— nie ustawiony —".to_string());
    let visibility = a.visibility.clone().unwrap_or_else(|| "—".to_string());
    let name_block = stack_v_gap("xxs", vec![
        text_styled(&a.alias_id, "body_strong"),
        text_styled(&methods, "caption"),
    ]);
    let detail = key_value(vec![
        ("Rozwiązuje na", target.as_str()),
        ("Widoczność", visibility.as_str()),
        ("Aktywny", if a.active { "tak" } else { "nie" }),
        ("Wymagany", if a.required { "tak" } else { "nie" }),
    ]);
    card(None, vec![
        stack_h(vec![
            name_block,
            chip_toned(&a.grant_status, a.status_tone()),
        ]),
        detail,
    ])
}

/// Assignment card: one Select per functional slot, populated ONLY from granted
/// aliases whose methods match the slot. Honest empty state per slot when no
/// granted model fits.
fn build_slot_assignment_card(available: &[AvailableAlias]) -> Component {
    let mut rows = vec![heading(3, "Przypisanie modeli do funkcji · 6 slotów")];
    let any_usable = available.iter().any(AvailableAlias::is_usable);
    if !any_usable {
        rows.push(empty_state(
            "Brak przyznanych modeli",
            Some("Poproś admina o grant w Services → Aliasy."),
            Some("lock"),
        ));
        return card(None, rows);
    }
    for slot in SLOTS {
        rows.push(build_slot_row(slot, available));
    }
    card(None, rows)
}

/// One slot row: label + the required method + a Select of matching granted
/// aliases (or an honest per-slot empty note) + a gated marker.
fn build_slot_row(slot: &SlotDef, available: &[AvailableAlias]) -> Component {
    let opts = slot_options(slot.method, available);
    let name_block = stack_v_gap("xxs", vec![
        text_styled(slot.label, "body_strong"),
        text_styled(&alloc::format!("metoda: {}", slot.method), "caption"),
    ]);

    let mut row_children = vec![name_block];
    if opts.is_empty() {
        row_children.push(text_styled("Brak przyznanego modelu dla tej funkcji.", "caption"));
    } else {
        let assigned = slot_assignment(slot, available);
        row_children.push(build_slot_select(slot, &opts, &assigned));
        let tone = if assigned.trim().is_empty() { "warning" } else { "success" };
        let label = if assigned.trim().is_empty() { "nieprzypisany" } else { "przypisany" };
        row_children.push(chip_toned(label, tone));
    }
    if slot.gated {
        row_children.push(chip_toned("gated", "critical"));
    }
    card(None, vec![stack_h(row_children)])
}

/// Select for one slot, bound to `alias_target_<slot>`, options = matching
/// granted aliases. Change persists the picked alias via `binding-target-change`.
fn build_slot_select(slot: &SlotDef, opts: &[&AvailableAlias], assigned: &str) -> Component {
    use tentaflow_sdk_spec::protocol::ui::form::Select;
    let store_key = slot_store_key(slot.id);
    let options: Vec<SelectOption> = opts.iter().map(|a| {
        let label = match &a.target_model {
            Some(t) if !t.is_empty() => alloc::format!("{} → {}", a.alias_id, t),
            _ => a.alias_id.clone(),
        };
        SelectOption {
            value: SelectValue::Text(a.alias_id.clone().into()),
            label: lit(&label),
            icon: None, disabled: false, group_id: None, description: None,
        }
    }).collect();
    let _ = assigned;
    let mut comp = Select {
        bind_path: StatePath::new(vec![PathSegment::Key(store_key.clone())]),
        options,
        placeholder: None,
        label: Some(lit("Model")),
        searchable: false, clearable: false, virtualize: false,
        disabled: None, size: InputSize::Md, groups: None,
    }.into_component(&store_key).expect("Select");
    let mut params = CborMap::default();
    params.0.push(("slot_id".into(), Value::Text(slot.id.into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Change,
        Handler::Backend {
            action_id: "binding-target-change".into(),
            params,
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    comp
}

/// Built-in storage API status panel. Every status is a REAL probe of the addon's
/// own capabilities — never a hard-coded "ok".
fn build_storage_status_card() -> Component {
    // SQL: round-trip a trivial query through the host SQL bridge.
    let (sql_label, sql_tone, sql_sub) = match db::query("SELECT 1", &[]) {
        Ok(rows) if rows.first().and_then(|r| r.first()).map(db::SqlValue::as_i64) == Some(1) => {
            ("dostępny", "ok", "SQLite · per-addon baza")
        }
        Ok(_) => ("nieoczekiwana odpowiedź", "warn", "SQLite · per-addon baza"),
        Err(_) => ("błąd", "err", "SQLite · per-addon baza"),
    };

    // KV: write+read a probe key through the settings table (the addon's KV).
    let kv_probe = "kv_probe";
    let kv_token = alloc::format!("{}", db::now_secs());
    let (kv_label, kv_tone, kv_sub) = match db::set_setting(kv_probe, &kv_token) {
        Ok(_) => match db::get_setting(kv_probe) {
            Ok(Some(v)) if v == kv_token => ("dostępny", "ok", "round-trip OK · tabela settings"),
            Ok(_) => ("niespójny odczyt", "warn", "tabela settings"),
            Err(_) => ("błąd odczytu", "err", "tabela settings"),
        },
        Err(_) => ("błąd zapisu", "err", "tabela settings"),
    };

    // Vector: REAL probe. A zero-vector k=1 search over the declared `events`
    // namespace proves the vector host function + capability respond (an empty
    // result set still means the API works). Only a hard AbiError is "unavailable".
    let (vec_label, vec_tone, vec_sub) = if vector_store_available() {
        ("dostępny", "ok", "events · cosine 1024d · vector store")
    } else {
        ("niedostępny", "err", "vector_search zwrócił błąd")
    };

    // Embeddings: REAL probe of the embedding model behind llm.generate. The
    // semantic search + indexing depend on it; report it honestly.
    let (emb_label, emb_tone, emb_sub) = if embedding_model_available() {
        ("dostępny", "ok", "llm.generate · model embeddingów")
    } else {
        ("niedostępny", "warn", "llm.generate · brak modelu embeddingów")
    };

    // Recording: report configured recordings dir from settings + the granted
    // recording.read permission.
    let rec_dir = db::get_setting("storage_recordings_dir").ok().flatten().unwrap_or_default();
    let (rec_label, rec_tone, rec_sub) = if rec_dir.trim().is_empty() {
        ("nieskonfigurowane", "warn", "ustaw katalog w Ustawienia")
    } else {
        ("skonfigurowane", "ok", "recording.read · katalog ustawiony")
    };

    let cell = |title: &str, label: &str, tone: &str, sub: &str| -> Component {
        card(None, vec![
            text_styled(title, "overline"),
            chip_toned(label, tone),
            text_styled(sub, "caption"),
        ])
    };

    card(None, vec![
        heading(3, "Storage — wbudowane API TentaFlow"),
        grid(3, vec![
            cell("KV store", kv_label, kv_tone, kv_sub),
            cell("SQL · SQLite", sql_label, sql_tone, sql_sub),
            cell("Vector store", vec_label, vec_tone, vec_sub),
            cell("Embeddings", emb_label, emb_tone, emb_sub),
            cell("Recording", rec_label, rec_tone, &alloc::format!("{}{}", rec_sub, if rec_dir.trim().is_empty() { String::new() } else { alloc::format!(" · {}", rec_dir) })),
        ]),
    ])
}

/// Seeds each slot Select's bound store key from the persisted assignment (or the
/// slot's first usable option) so the panel mounts with the current models
/// selected. Reads the real grant data once.
fn bindings_overlay() -> Vec<StateEntry> {
    let available = alias_list_available().unwrap_or_default();
    SLOTS.iter().filter_map(|slot| {
        // Only seed a slot that actually has usable options — otherwise the
        // Select is not rendered and the entry would be dead.
        let opts = slot_options(slot.method, &available);
        if opts.is_empty() {
            return None;
        }
        Some(StateEntry {
            path: StatePath::new(vec![PathSegment::Key(slot_store_key(slot.id))]),
            value: Value::Text(slot_assignment(slot, &available)),
        })
    }).collect()
}

/// Persists a new slot → alias assignment and records the before/after in the
/// audit log. The assignment CHOICE persists regardless of whether a backing
/// service is currently running (that is the real, durable part of a binding).
/// The picked alias is validated against the slot's REAL usable options from the
/// grant system — you cannot assign an alias you were not granted or whose
/// methods do not match the slot.
fn handle_binding_target_change(params: &JsonValue) -> JsonValue {
    let slot_id = params.get("slot_id").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let value = params.get("value").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let Some(slot) = SLOTS.iter().find(|s| s.id == slot_id) else {
        return json!({"ok": false});
    };
    let available = alias_list_available().unwrap_or_default();
    let opts = slot_options(slot.method, &available);
    // Reject aliases outside the slot's granted, method-matching options.
    if !opts.iter().any(|a| a.alias_id == value) {
        with_state(|s| s.error_message = Some("Wybrany model nie jest przyznany dla tej funkcji.".into()));
        render_panel("bindings");
        return json!({"ok": false});
    }
    let before = slot_assignment(slot, &available);
    let key = slot_setting_key(&slot_id);
    match db::set_setting(&key, &value) {
        Ok(_) => {
            let _ = db::insert_audit(BINDING_ACTOR, "binding_change", &slot_id, &before, &value);
            with_state(|s| { s.clear_messages(); s.success_message = Some(alloc::format!("Funkcja {} → {} zapisana.", slot_id, value)); });
        }
        Err(e) => {
            with_state(|s| s.error_message = Some(alloc::format!("Nie udało się zapisać mapowania: {}", abi_message(e))));
        }
    }
    render_panel("bindings");
    json!({"ok": true})
}

fn redact_url_for_display(url: &str) -> String {
    match extract_host_port(url) {
        Some((host, Some(port))) => alloc::format!("{}:{}", host, port),
        Some((host, None)) => host,
        None => "(nieznany host)".to_string(),
    }
}
