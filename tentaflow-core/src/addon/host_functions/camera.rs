// ============ File: camera.rs — Camera ingest host functions (M1.W6 F1a TentaVision) ============
//
// Implements the 10 host functions that bridge addon-side WASM calls to the
// `services::camera_ingest::CameraIngestSupervisor`. Each call:
//   1. enforces input payload size BEFORE materializing a String,
//   2. decodes CBOR, validates ownership / vendor / lengths / format,
//   3. enforces permission,
//   4. mutates the supervisor registry and/or persists the change in DB,
//   5. records an audit-log entry on every exit path (ok / denied / error),
//   6. enforces output payload max before write_output_with_retry_semantics.
//
// F1a scope is `vendor='fake_file'` only — RTSP / ONVIF discovery, credential
// rotation, and SnapshotRef indirection arrive in later milestones.
//
// Supervisor lifetime: a process-wide singleton initialized lazily on first
// host-function call (via `tokio::sync::OnceCell`). The supervisor exposes
// `drain(&self)` which stops all sessions but leaves the singleton in place.
// `shutdown_camera_supervisor_global()` is invoked from the process-level
// shutdown hook in `tentaflow/src/main.rs` before router shutdown.

#![cfg(feature = "camera")]
#![allow(clippy::too_many_arguments)]

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use tokio::sync::OnceCell;
use tracing::warn;

use tentaflow_sdk_spec::{
    CameraAddInput, CameraAddOutput, CameraAnalysisFlowOut, CameraAnalysisFlowsOut,
    CameraCredentialsRotateInput, CameraCredentialsRotateOut, CameraDiscoverOut, CameraGrantInfo,
    CameraGrantInput, CameraGrantListInput, CameraGrantListOut, CameraGrantOut, CameraHealthOut,
    CameraIdInput, CameraInfoOut, CameraListOut, CameraRemoveOut, CameraRevokeInput,
    CameraSnapshotOut, CameraTestConnectionInput, CameraTestConnectionOut, CameraUpdateInput,
    DiscoveredCameraOut, LocalCameraDeviceOut, LocalCameraDevicesOut,
};

use super::abi_helpers::{enforce_payload_size, PayloadKind};
use super::cbor_io::{decode_cbor_exact, read_input_cbor, write_cbor_capped};
use super::{
    audit_log_with_risk, check_permission, get_memory, read_guest_bytes, write_guest_output,
    AddonState, WasmCaller,
};
use crate::addon::errors::AbiError;
use crate::audit::RiskClass;
use crate::db::repository::{
    can_read_camera, get_camera_for_addon, get_camera_in_org, grant_camera, insert_camera,
    list_accessible_cameras, list_camera_grants, list_cameras_for_addon, revoke_camera_grant,
    set_camera_credentials_encrypted, set_camera_onvif_resolved, soft_delete_camera, update_camera,
    CameraPatch, CameraRow,
};
use crate::services::camera_ingest::{
    credentials::credentials_cipher, list_local_devices, start_supervisor, CameraConfig,
    CameraIngestError, CameraIngestSupervisor,
};

// =============================================================================
// Permission constants
// =============================================================================

const PERM_CAMERAS_READ: &str = "cameras.read";
const PERM_CAMERAS_WRITE: &str = "cameras.write";
const PERM_CAMERAS_SNAPSHOT: &str = "cameras.snapshot";

// =============================================================================
// Vendor whitelist (F1a)
// =============================================================================

/// Vendors that `camera_add_v1` will persist as a managed session. ONVIF
/// is accepted: the host calls `GetProfiles` + `GetStreamUri` to derive an
/// RTSP URI from the device-service URL and persists the derivation
/// (`onvif_url` + `onvif_profile_token`) so a later credentials rotation
/// can re-resolve without re-running discovery.
const ADDABLE_VENDORS: &[&str] = &["fake_file", "rtsp", "onvif", "local_camera", "v4l2"];

/// Vendors `camera_test_connection_v1` knows how to probe. ONVIF is included
/// — we probe its device-service HTTP endpoint as a reachability check.
const TESTABLE_VENDORS: &[&str] = &["fake_file", "rtsp", "onvif", "local_camera", "v4l2"];

fn vendor_addable(v: &str) -> bool {
    ADDABLE_VENDORS.iter().any(|s| *s == v)
}

fn vendor_testable(v: &str) -> bool {
    TESTABLE_VENDORS.iter().any(|s| *s == v)
}

fn retention_class_valid(rc: &str) -> bool {
    matches!(rc, "A" | "B" | "C" | "Unclassified")
}

/// Accepted AI analysis frame rates. The UI offers a fixed ladder
/// (1/5/10/15/unlimited), but the host also tolerates any value in `0..=30`
/// (`0` = unlimited / native cadence) so a future preset cannot be rejected
/// by an over-tight allowlist.
fn analysis_fps_valid(fps: u32) -> bool {
    fps <= 30
}

// =============================================================================
// String length + format validators
// =============================================================================

const MAX_DISPLAY_NAME: usize = 256;
const MAX_URL: usize = 4096;
const MAX_PROFILE: usize = 128;
const MAX_VENDOR: usize = 64;
const MAX_RETENTION_CLASS: usize = 32;
const MAX_CREDENTIALS_B64: usize = 16 * 1024;

/// camera_id format: `cam_<uuid-v4>`. The UUID portion is the standard 36-char
/// hyphenated lowercase hex form. We accept the conservative pattern so that
/// any DB row produced by `camera_add_v1` survives the validator on later
/// calls, and any addon-supplied id that does not match is rejected before
/// we touch the registry or the DB.
fn camera_id_valid(s: &str) -> bool {
    let rest = match s.strip_prefix("cam_") {
        Some(r) => r,
        None => return false,
    };
    if rest.len() != 36 {
        return false;
    }
    // Positions 8, 13, 18, 23 must be '-'; the rest must be lowercase hex.
    for (i, ch) in rest.chars().enumerate() {
        let is_dash_pos = matches!(i, 8 | 13 | 18 | 23);
        if is_dash_pos {
            if ch != '-' {
                return false;
            }
        } else if !ch.is_ascii_hexdigit() || ch.is_ascii_uppercase() {
            return false;
        }
    }
    true
}

fn display_name_valid(s: &str) -> bool {
    let trimmed = s.trim();
    if trimmed.is_empty() || s.len() > MAX_DISPLAY_NAME {
        return false;
    }
    s.chars().all(|c| {
        c.is_alphanumeric()
            || c.is_whitespace()
            || matches!(
                c,
                '-' | '_' | '.' | ',' | '(' | ')' | ':' | '\'' | '"' | '!' | '?'
            )
    })
}

fn profile_valid(s: &str) -> bool {
    if s.is_empty() || s.len() > MAX_PROFILE {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

// =============================================================================
// Supervisor singleton + graceful shutdown
// =============================================================================

static SUPERVISOR: OnceCell<Arc<CameraIngestSupervisor>> = OnceCell::const_new();

/// Boot-time wrapper around the supervisor lazy-init. Triggers
/// `hydrate_supervisor_from_db` so RTSP sessions for active cameras come up
/// before any addon UI is opened — pipeline must run for analysis Flow even
/// if no user is currently watching the dashboard.
pub async fn ensure_supervisor_started() -> Result<(), anyhow::Error> {
    get_or_init_supervisor()
        .await
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("supervisor init failed: {:?}", e))
}

async fn get_or_init_supervisor() -> Result<Arc<CameraIngestSupervisor>, AbiError> {
    SUPERVISOR
        .get_or_try_init(|| async {
            let sup = start_supervisor().await.map(Arc::new).map_err(|e| {
                warn!("camera_ingest supervisor init failed: {e}");
                AbiError::Operation
            })?;
            // Hydrate sessions for every active camera surviving from the
            // previous process lifecycle. Without this, cameras stay
            // "starting" forever after a restart because the supervisor
            // registry is empty and `get_health()` returns NotFound.
            hydrate_supervisor_from_db(&sup).await;
            Ok(sup)
        })
        .await
        .cloned()
}

/// Re-spawns one session per active camera in `cameras` table on supervisor
/// init. Errors are logged at warn — a single bad camera must not block
/// the rest from coming online. Uses the global DB pool because the
/// supervisor singleton has no caller context at first-init time.
async fn hydrate_supervisor_from_db(sup: &Arc<CameraIngestSupervisor>) {
    let pool = match crate::db::global_pool() {
        Some(p) => p,
        None => {
            warn!("camera_ingest: no global DB pool — skipping hydrate");
            return;
        }
    };
    let rows = match crate::db::repository::list_all_active_cameras(&pool) {
        Ok(rows) => rows,
        Err(e) => {
            warn!("camera_ingest: hydrate query failed: {e}");
            return;
        }
    };
    let count = rows.len();
    tracing::info!(
        "camera_ingest: hydrating {} camera session(s) from DB",
        count
    );
    for row in rows {
        // Backed (WebRTC) cameras have no live track after a restart — the
        // channel that fed them is gone. Hard-delete the stale row instead of
        // trying to bring up a dead session; the addon re-registers on reconnect.
        if row.vendor == "webrtc" {
            let _ = crate::db::repository::delete_camera_hard(
                &pool,
                &row.owner_addon_id,
                &row.camera_id,
            );
            continue;
        }
        let resolution = match (row.resolution_width, row.resolution_height) {
            (Some(w), Some(h)) if w > 0 && h > 0 => Some((w as u32, h as u32)),
            _ => None,
        };
        let cfg = CameraConfig {
            camera_id: row.camera_id.clone(),
            vendor: row.vendor.clone(),
            url: row.url.clone(),
            target_fps: row.target_fps.max(1) as u32,
            resolution,
            owner_addon_id: Some(row.owner_addon_id.clone()),
            credentials_encrypted: row.credentials_encrypted.clone(),
            decoder_override: None,
        };
        match sup.add_camera(cfg).await {
            Ok(_) => {
                // Always-on analysis: start the cross-camera RF-DETR engine for
                // this camera at boot, independent of any dashboard viewer — the
                // analysis must run even when nobody is watching. Idempotent.
                #[cfg(feature = "inference-vision-gpu")]
                crate::services::camera_ingest::vision_analysis::ensure_analysis(&row.camera_id);
            }
            Err(e) => warn!(
                "camera_ingest: failed to hydrate camera_id={} vendor={}: {}",
                row.camera_id, row.vendor, e
            ),
        }
    }
    tracing::info!("camera_ingest: hydrate complete ({} camera(s))", count);
}

/// Drains every camera session on the process-wide supervisor without
/// consuming the singleton. Safe to call multiple times: subsequent calls
/// drain an already-empty registry. Wired into the main binary's shutdown
/// path (see `tentaflow/src/main.rs`) so GStreamer pipelines stop before
/// the router begins releasing locks.
pub async fn shutdown_camera_supervisor_global() {
    #[cfg(feature = "inference-vision-gpu")]
    crate::services::camera_ingest::vision_analysis::drain();
    if let Some(sup) = SUPERVISOR.get() {
        sup.drain().await;
    }
}

/// Tear down a backed (WebRTC) camera — called by the webrtc host module when a
/// channel closes / its addon unloads, so the supervisor session + DB row do not
/// leak. Best-effort: the supervisor removal is spawned (non-blocking) and the
/// row is hard-deleted (ephemeral camera, no audit retention).
pub fn remove_backed_camera(owner_addon_id: &str, camera_id: &str) {
    if let (Some(sup), Ok(handle)) = (SUPERVISOR.get(), tokio::runtime::Handle::try_current()) {
        let sup = sup.clone();
        let cid = camera_id.to_string();
        handle.spawn(async move {
            let _ = sup.remove_camera(&cid).await;
        });
    }
    if let Some(pool) = crate::db::global_pool() {
        let _ = crate::db::repository::delete_camera_hard(&pool, owner_addon_id, camera_id);
    }
}

/// Bind a WebRTC channel's video track to a camera consumable by the normal
/// camera/streaming stack. Takes the channel's H.264 byte stream, starts a
/// backed supervisor session, persists a `vendor='webrtc'` row, and records the
/// channel→camera link so teardown removes it.
pub fn camera_register_backed_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    if !check_permission(caller.data(), PERM_CAMERAS_WRITE, None) {
        audit(
            caller.data(),
            "camera.register_backed",
            None,
            RiskClass::A,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let input: tentaflow_sdk_spec::WebRtcRegisterCameraInput =
        match read_input_cbor(&memory, &caller, input_ptr, input_len, PayloadKind::ServiceCall) {
            Ok(v) => v,
            Err(e) => return e.as_i32(),
        };
    let target_fps = input.target_fps.clamp(1, 60);
    let analysis_fps = input.analysis_fps.clamp(1, 60);
    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();
    let org_id_for_insert = caller.data().org_id.clone();

    // Take the channel's video stream (single consumer; destructive).
    let rx = match crate::addon::host_functions::webrtc::take_channel_video(
        &addon_id,
        &input.channel_id,
    ) {
        Some(rx) => rx,
        None => {
            audit(
                caller.data(),
                "camera.register_backed",
                Some(&input.channel_id),
                RiskClass::A,
                "error",
                Some("no_video_or_taken"),
            );
            return AbiError::NotFound.as_i32();
        }
    };

    let camera_id = format!("cam_{}", uuid::Uuid::new_v4());
    let cfg = CameraConfig {
        camera_id: camera_id.clone(),
        vendor: "webrtc".to_string(),
        url: input.channel_id.clone(), // marker only; the source is the live rx
        target_fps,
        resolution: None,
        owner_addon_id: Some(addon_id.clone()),
        credentials_encrypted: None,
        decoder_override: None,
    };
    let sup = match run_async(get_or_init_supervisor()) {
        Ok(s) => s,
        Err(e) => return e.as_i32(),
    };
    if let Err(e) = run_async(sup.add_webrtc_camera(cfg, rx)) {
        audit(
            caller.data(),
            "camera.register_backed",
            Some(&camera_id),
            RiskClass::A,
            "error",
            Some(&format!("session_start_failed: {e}")),
        );
        return map_ingest_error(&e).as_i32();
    }
    // Record the reverse-index BEFORE the DB insert so a concurrent
    // webrtc_close always tears the backed session down (no orphan).
    crate::addon::host_functions::webrtc::bind_camera(&addon_id, &input.channel_id, &camera_id);
    if let Err(e) = insert_camera(
        &db,
        &camera_id,
        &addon_id,
        &input.display_name,
        "webrtc",
        &input.channel_id,
        target_fps as i64,
        analysis_fps as i64,
        None,
        None,
        "C",
        "default",
        None,
        None,
        None,
        org_id_for_insert.as_deref(),
    ) {
        warn!("camera.register_backed insert_camera failed (compensating): {e}");
        let _ = run_async(sup.remove_camera(&camera_id));
        audit(
            caller.data(),
            "camera.register_backed",
            Some(&camera_id),
            RiskClass::A,
            "error",
            Some("db_insert_failed"),
        );
        return AbiError::Operation.as_i32();
    }
    audit(
        caller.data(),
        "camera.register_backed",
        Some(&camera_id),
        RiskClass::A,
        "ok",
        None,
    );
    let out = tentaflow_sdk_spec::WebRtcRegisterCameraOutput { camera_id };
    write_cbor_capped(
        &memory,
        &mut caller,
        &out,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::ServiceCall,
    )
}

/// Register a PUSHED camera source (the phone's native encoder feeds H.264 over the
/// mobile FFI, not a WebRTC peer). Creates the H.264 channel, registers it as a
/// normal `webrtc`-vendor camera (so the GStreamer tee fans it out to the MSE tile +
/// the decoded-frame mailbox that TentaVision + depth-AI read — one stream, three
/// consumers), records the sender for the FFI push, and returns the new `camera_id`.
///
/// String ABI (the phone addon does string I/O): `display_name` UTF-8 in, `camera_id`
/// UTF-8 out. Gated by `cameras.write`.
pub fn camera_register_pushed_v1(
    mut caller: WasmCaller<'_, AddonState>,
    name_ptr: i32,
    name_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    if !check_permission(caller.data(), PERM_CAMERAS_WRITE, None) {
        audit(caller.data(), "camera.register_pushed", None, RiskClass::A, "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }
    let display_name = match read_guest_bytes(&memory, &caller, name_ptr, name_len) {
        Some(b) => String::from_utf8_lossy(b).into_owned(),
        None => return AbiError::Operation.as_i32(),
    };
    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();
    let org_id_for_insert = caller.data().org_id.clone();

    // Idempotent: reuse an existing pushed camera for this addon (re-attach a fresh
    // channel) so a restart does NOT leak a new supervisor session + DB row each time.
    let existing = list_cameras_for_addon(&db, &addon_id, org_id_for_insert.as_deref())
        .ok()
        .and_then(|rows| {
            rows.into_iter()
                .find(|r| r.vendor == "webrtc" && r.url.starts_with("phone:"))
                .map(|r| r.camera_id)
        });
    let camera_id = existing.clone().unwrap_or_else(|| format!("cam_{}", uuid::Uuid::new_v4()));

    // The H.264 channel: native encoder → FFI → tx → appsrc. A few seconds of access
    // units buffered; latest-wins drop under backpressure (live video).
    let (tx, rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(120);
    let cfg = CameraConfig {
        camera_id: camera_id.clone(),
        vendor: "webrtc".to_string(),
        url: format!("phone:{addon_id}"), // marker only; source is the live rx
        target_fps: 30,
        resolution: None,
        owner_addon_id: Some(addon_id.clone()),
        credentials_encrypted: None,
        decoder_override: None,
    };
    let sup = match run_async(get_or_init_supervisor()) {
        Ok(s) => s,
        Err(e) => return e.as_i32(),
    };
    // Reused id: drop any stale session for it before re-attaching the fresh channel.
    if existing.is_some() {
        let _ = run_async(sup.remove_camera(&camera_id));
    }
    if let Err(e) = run_async(sup.add_webrtc_camera(cfg, rx)) {
        audit(caller.data(), "camera.register_pushed", Some(&camera_id), RiskClass::A, "error", Some(&format!("session_start_failed: {e}")));
        return map_ingest_error(&e).as_i32();
    }
    crate::services::mobile_camera::MobileCameraIngest::global().set_sender(&addon_id, tx);
    // Only insert a DB row for a NEW camera; a reused one already has its row.
    if existing.is_none() {
        if let Err(e) = insert_camera(
            &db, &camera_id, &addon_id, &display_name, "webrtc", &format!("phone:{addon_id}"),
            30, 5, None, None, "C", "default", None, None, None, org_id_for_insert.as_deref(),
        ) {
            warn!("camera.register_pushed insert_camera failed (compensating): {e}");
            let _ = run_async(sup.remove_camera(&camera_id));
            crate::services::mobile_camera::MobileCameraIngest::global().remove(&addon_id);
            return AbiError::Operation.as_i32();
        }
    }
    audit(caller.data(), "camera.register_pushed", Some(&camera_id), RiskClass::A, "ok", None);
    write_guest_output(&memory, &mut caller, out_ptr, out_cap, out_len_ptr, camera_id.as_bytes())
}

/// Latest decoded RGB24 frame for a camera, taken from the running session's
/// frame mailbox via the process-wide supervisor. `None` when the supervisor
/// is not initialized, the camera is not registered, or no frame has landed
/// yet. Used by the always-on vision analysis loop to pull frames without
/// reaching into session internals.
#[cfg(feature = "inference-vision-gpu")]
pub async fn latest_frame_global(camera_id: &str) -> Option<(std::sync::Arc<[u8]>, u32, u32)> {
    let sup = SUPERVISOR.get()?;
    match sup.snapshot(camera_id).await {
        Ok(snap) => Some((std::sync::Arc::from(snap.data), snap.width, snap.height)),
        Err(_) => None,
    }
}

fn run_async<F, T>(fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
}

fn map_ingest_error(e: &CameraIngestError) -> AbiError {
    use CameraIngestError::*;
    match e {
        UnsupportedVendor(_) => AbiError::CameraVendorUnsupported,
        InvalidUrl(_) | InvalidConfig(_) => AbiError::Operation,
        FileNotFound(_) | SymlinkNotAllowed(_) => AbiError::CameraUnreachable,
        AlreadyExists(_) => AbiError::Conflict,
        NotFound(_) => AbiError::NotFound,
        GstInit(_) | PipelineBuild(_) | PipelineState(_) | Internal(_) => AbiError::Operation,
        SessionCrashed(_) | SnapshotFailed(_) => AbiError::CameraUnreachable,
        SnapshotTimeout => AbiError::Timeout,
        QuotaExceeded(_) => AbiError::QuotaExceeded,
    }
}

// =============================================================================
// Camera ABI payload structs (input/output) live in `tentaflow-sdk-spec`
// (`protocol::camera`) as the single CBOR source of truth shared with the
// addon SDK. Imported at the top of this module.
// =============================================================================

/// Hard upper bound on the SOAP-resolve timeout (10 s default, 30 s ceiling
/// — same as `onvif_media::MAX_TIMEOUT_MS`). Cameras that take longer than
/// this on discovery are not usable in the wizard flow.
const ONVIF_RESOLVE_TIMEOUT_MS: u32 = 10_000;

// =============================================================================
// Helpers — status mapping + audit + io
// =============================================================================

fn status_to_str(s: crate::services::camera_ingest::CameraStatus) -> &'static str {
    use crate::services::camera_ingest::CameraStatus::*;
    match s {
        Offline => "offline",
        Starting => "starting",
        Online => "online",
        Error => "error",
        Stopping => "stopping",
    }
}

async fn build_camera_info(sup: &CameraIngestSupervisor, row: CameraRow) -> CameraInfoOut {
    let mut status = row.status.clone();
    let mut status_message = row.status_message.clone();
    let mut fps_actual = row.fps_actual;
    let mut last_frame_at = row.last_frame_at;
    if let Ok(h) = sup.get_health(&row.camera_id).await {
        status = status_to_str(h.status).to_string();
        status_message = h.status_message;
        fps_actual = h.fps_actual.map(|v| v as f64);
        last_frame_at = h.last_frame_at.map(|v| v as i64);
    }
    CameraInfoOut {
        camera_id: row.camera_id,
        display_name: row.display_name,
        vendor: row.vendor,
        url: row.url,
        target_fps: row.target_fps,
        resolution_width: row.resolution_width,
        resolution_height: row.resolution_height,
        status,
        status_message,
        fps_actual,
        last_frame_at,
        retention_class: row.retention_class,
        profile: row.profile,
        analysis_flow_id: row.analysis_flow_id,
        owner_addon_id: None,
        access_level: None,
    }
}

fn audit(
    state: &AddonState,
    action: &str,
    resource_id: Option<&str>,
    risk: RiskClass,
    result: &str,
    reason: Option<&str>,
) {
    audit_log_with_risk(
        state,
        action,
        Some("camera"),
        resource_id,
        risk,
        None,
        None,
        result,
        reason,
    );
}

/// Validates the static input pieces shared by `add` / `update` / `test`.
/// Returns `Err((abi_error, reason_str))` on failure. The reason string is
/// stored verbatim in the audit log so operators can triage rejection cause.
fn validate_display_name(name: &str) -> Result<(), &'static str> {
    if name.len() > MAX_DISPLAY_NAME {
        return Err("display_name_too_long");
    }
    if !display_name_valid(name) {
        return Err("display_name_invalid");
    }
    Ok(())
}

fn validate_url(url: &str) -> Result<(), &'static str> {
    if url.is_empty() {
        return Err("url_empty");
    }
    if url.len() > MAX_URL {
        return Err("url_too_long");
    }
    Ok(())
}

fn validate_profile(profile: &str) -> Result<(), &'static str> {
    if !profile_valid(profile) {
        return Err("profile_invalid");
    }
    Ok(())
}

fn validate_vendor(v: &str) -> Result<(), &'static str> {
    if v.is_empty() || v.len() > MAX_VENDOR {
        return Err("vendor_length");
    }
    if !vendor_addable(v) {
        return Err("unsupported_vendor");
    }
    Ok(())
}

fn validate_retention(rc: &str) -> Result<(), &'static str> {
    if rc.len() > MAX_RETENTION_CLASS {
        return Err("retention_class_too_long");
    }
    if !retention_class_valid(rc) {
        return Err("invalid_retention_class");
    }
    Ok(())
}

/// Decode and encrypt an optional `credentials_b64` field. Returns the
/// AES-GCM blob ready for storage, or a static error tag describing why the
/// input was rejected. The decoded plaintext is wiped from the temporary
/// `String` by going out of scope; it is never logged or returned in errors.
fn prepare_credentials_blob(b64: Option<&str>) -> Result<Option<Vec<u8>>, &'static str> {
    let Some(s) = b64 else {
        return Ok(None);
    };
    if s.is_empty() {
        return Ok(None);
    }
    if s.len() > MAX_CREDENTIALS_B64 {
        return Err("credentials_b64_too_long");
    }
    let raw = base64::engine::general_purpose::STANDARD
        .decode(s.as_bytes())
        .map_err(|_| "credentials_b64_invalid")?;
    let plain = std::str::from_utf8(&raw).map_err(|_| "credentials_not_utf8")?;
    if plain.len() > crate::services::camera_ingest::credentials::MAX_PLAINTEXT_LEN {
        return Err("credentials_plaintext_too_long");
    }
    validate_userinfo_plaintext(plain)?;
    let blob = credentials_cipher()
        .encrypt(plain)
        .map_err(|_| "credentials_encrypt_failed")?;
    Ok(Some(blob))
}

/// Reject `user:pass` plaintexts that would break URL parsing or open up
/// URL-injection vectors when later overlaid into the rtsp:// location.
/// Accepts RFC 3986 `unreserved` plus a small set of `sub-delims` that are
/// safe inside the userinfo component (`!$&'()*+,;=`). Anything that would
/// require percent-encoding (`@`, `/`, `?`, `#`, `[`, `]`, `%`, whitespace,
/// control chars, multi-byte) is rejected so callers cannot smuggle a
/// `user:pass@evil.host/x` into the eventual GStreamer URL.
fn validate_userinfo_plaintext(plain: &str) -> Result<(), &'static str> {
    let (user, pass) = plain
        .split_once(':')
        .ok_or("credentials_missing_user_pass_separator")?;
    if user.is_empty() {
        return Err("credentials_user_empty");
    }
    if pass.is_empty() {
        return Err("credentials_pass_empty");
    }
    let safe = |c: char| {
        c.is_ascii_alphanumeric()
            || matches!(
                c,
                '-' | '.'
                    | '_'
                    | '~'
                    | '!'
                    | '$'
                    | '&'
                    | '\''
                    | '('
                    | ')'
                    | '*'
                    | '+'
                    | ','
                    | ';'
                    | '='
            )
    };
    if !user.chars().all(safe) {
        return Err("credentials_user_unsafe_chars");
    }
    if !pass.chars().all(safe) {
        return Err("credentials_pass_unsafe_chars");
    }
    Ok(())
}

// =============================================================================
// Live-probe helpers used by camera_test_connection_v1
// =============================================================================

/// One-shot RTSP OPTIONS probe. Opens a plain TCP connection (RTSP/1.0 over
/// TCP), sends an OPTIONS request and reads up to 1 KiB of reply. Anything
/// other than a `RTSP/1.0 2xx` / `RTSP/2.0 2xx` / `401 Unauthorized` status
/// line is reported as failure. `401` is accepted as a positive signal that
/// the server responded — `test_connection` is anonymous so an auth-required
/// camera should not be flagged unreachable. All embedded credentials are
/// stripped from error messages via `redact_rtsp_url` / `redact_url_in_text`.
async fn rtsp_test_connection(url: &str, timeout_secs: u64) -> Result<(), String> {
    use crate::services::camera_ingest::rtsp::{redact_rtsp_url, redact_url_in_text};
    let parsed = url::Url::parse(url)
        .map_err(|e| format!("invalid URL: {}", redact_url_in_text(&e.to_string())))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| "missing host".to_string())?
        .to_string();
    // `rtsps://` is RTSP tunneled over TLS (UniFi Protect, some Axis/Hanwha).
    // Default RTSPS port is 322; plain RTSP is 554.
    let tls = parsed.scheme().eq_ignore_ascii_case("rtsps");
    let port = parsed.port().unwrap_or(if tls { 322 } else { 554 });
    let path = if parsed.path().is_empty() {
        "/"
    } else {
        parsed.path()
    };
    let scheme = if tls { "rtsps" } else { "rtsp" };
    let request_uri = format!("{scheme}://{host}:{port}{path}");
    let redacted = redact_rtsp_url(url);

    let dur = Duration::from_secs(timeout_secs);
    let tcp = tokio::time::timeout(dur, tokio::net::TcpStream::connect((host.as_str(), port)))
        .await
        .map_err(|_| format!("connect timeout: {redacted}"))?
        .map_err(|e| format!("tcp connect failed: {e}"))?;

    // Plain RTSP probes the bare TCP socket; RTSPS must complete a TLS handshake
    // first (a plaintext OPTIONS to a TLS port makes the server hang up without
    // replying). Cameras almost always present self-signed certs, so this probe
    // skips chain validation — it tests reachability, not transport trust.
    if tls {
        let connector = rtsps_tls_connector()?;
        let server_name = rustls::pki_types::ServerName::try_from(host.clone())
            .map_err(|_| format!("invalid TLS host: {redacted}"))?;
        let stream = tokio::time::timeout(dur, connector.connect(server_name, tcp))
            .await
            .map_err(|_| "tls handshake timeout".to_string())?
            .map_err(|e| format!("tls handshake failed: {e}"))?;
        rtsp_options_exchange(stream, &request_uri, dur).await
    } else {
        rtsp_options_exchange(tcp, &request_uri, dur).await
    }
}

/// Builds a rustls `TlsConnector` that accepts any server certificate. Used only
/// by the RTSPS reachability probe — never for data transfer. Explicit ring
/// provider so the probe works even if no process-wide default is installed.
fn rtsps_tls_connector() -> Result<tokio_rustls::TlsConnector, String> {
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| format!("tls config: {e}"))?
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(AcceptAnyServerCert))
    .with_no_client_auth();
    Ok(tokio_rustls::TlsConnector::from(Arc::new(config)))
}

/// Certificate verifier that accepts every server certificate. SOLELY for the
/// RTSPS connection-test probe (self-signed camera certs); not used anywhere a
/// real trust decision matters.
#[derive(Debug)]
struct AcceptAnyServerCert;

impl rustls::client::danger::ServerCertVerifier for AcceptAnyServerCert {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Sends an RTSP `OPTIONS` over an established stream (plain TCP or TLS) and
/// treats ANY `RTSP/1.x`/`RTSP/2.x` status line as reachable — a 4xx (wrong
/// path / unsupported method) still proves the server is alive and speaking
/// RTSP, which is all a connection test verifies.
async fn rtsp_options_exchange<S>(mut stream: S, request_uri: &str, dur: Duration) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use crate::services::camera_ingest::rtsp::redact_url_in_text;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let req =
        format!("OPTIONS {request_uri} RTSP/1.0\r\nCSeq: 1\r\nUser-Agent: TentaFlow/F1b\r\n\r\n");
    tokio::time::timeout(dur, stream.write_all(req.as_bytes()))
        .await
        .map_err(|_| "write timeout".to_string())?
        .map_err(|e| format!("write failed: {e}"))?;

    let mut buf = vec![0u8; 1024];
    let n = tokio::time::timeout(dur, stream.read(&mut buf))
        .await
        .map_err(|_| "read timeout".to_string())?
        .map_err(|e| format!("read failed: {e}"))?;
    if n == 0 {
        return Err("server closed connection without reply".into());
    }
    let response = std::str::from_utf8(&buf[..n]).unwrap_or("(non-utf8 response)");
    let status = response.lines().next().unwrap_or("");
    let ok = status.starts_with("RTSP/1.0 ") || status.starts_with("RTSP/2.0 ");
    if !ok {
        return Err(format!(
            "unexpected response: {}",
            redact_url_in_text(status)
        ));
    }
    Ok(())
}

/// HTTP HEAD probe against the ONVIF device-service endpoint. We force the
/// URL path under `/onvif/` so an addon cannot use this entry point to probe
/// arbitrary HTTP targets on the local network. Any HTTP reply (200 / 401 /
/// 405) means the device is reachable; only network failures / timeouts are
/// reported as unreachable.
fn force_onvif_path(url: &str) -> Result<url::Url, String> {
    let mut parsed = url::Url::parse(url).map_err(|e| format!("invalid URL: {e}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!(
            "ONVIF probe requires http(s) URL, got: {}",
            parsed.scheme()
        ));
    }
    if !parsed.path().contains("/onvif/") {
        parsed.set_path("/onvif/device_service");
    }
    Ok(parsed)
}

async fn onvif_test_connection(url: &str, timeout_secs: u64) -> Result<String, String> {
    let parsed = force_onvif_path(url)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .map_err(|e| format!("http client init: {e}"))?;
    let resp = client
        .head(parsed.clone())
        .send()
        .await
        .map_err(|e| format!("http: {e}"))?;
    Ok(format!(
        "ONVIF endpoint responded HTTP {}",
        resp.status().as_u16()
    ))
}

/// Result of an ONVIF SOAP resolve performed during `camera_add_v1` /
/// `camera_add_core` for `vendor='onvif'`. Carries the derived RTSP URI
/// that replaces the device-service URL in the supervisor session, plus
/// the profile token actually chosen (for persistence).
struct OnvifResolveOk {
    rtsp_uri: String,
    profile_token: String,
}

/// Reason tag for the audit log when an ONVIF resolve fails. Returned as a
/// short stable string so operators can grep — never include user-supplied
/// data here (host / credentials must not leak into audit rows).
fn map_onvif_resolve_error(
    e: &crate::services::camera_ingest::onvif_media::OnvifError,
) -> (AbiError, &'static str) {
    use crate::services::camera_ingest::onvif_media::OnvifError::*;
    match e {
        AuthFailed => (AbiError::Permission, "onvif_auth_failed"),
        NoProfiles => (AbiError::CameraUnreachable, "onvif_no_profiles"),
        ProfileNotFound(_) => (AbiError::NotFound, "onvif_profile_not_found"),
        Timeout(_) => (AbiError::Timeout, "onvif_timeout"),
        Transport(_) => (AbiError::CameraUnreachable, "onvif_transport_failure"),
        SoapFault(_) | MalformedResponse(_) => (AbiError::Operation, "onvif_invalid_response"),
    }
}

/// Resolve an ONVIF device-service URL into a streamable RTSP URI by
/// running `GetProfiles` + `GetStreamUri`. Plaintext `user:pass` is taken
/// from the already-decoded credentials blob (decrypted with the master
/// key) so the SOAP digest can be built — the plaintext is dropped at the
/// end of this function and never returned to the caller / logged.
fn resolve_onvif_one_click(
    device_service_url: &str,
    credentials_blob: &[u8],
    profile_token: Option<&str>,
) -> Result<OnvifResolveOk, (AbiError, &'static str)> {
    use crate::services::camera_ingest::credentials::credentials_cipher;
    use crate::services::camera_ingest::onvif_media;

    let plain = match credentials_cipher().decrypt(credentials_blob) {
        Ok(p) => p,
        Err(_) => return Err((AbiError::Operation, "credentials_decrypt_failed")),
    };
    let (username, password) = match plain.split_once(':') {
        Some((u, p)) if !u.is_empty() && !p.is_empty() => (u.to_string(), p.to_string()),
        _ => {
            return Err((
                AbiError::Operation,
                "credentials_missing_user_pass_separator",
            ))
        }
    };
    let creds = onvif_media::OnvifCredentials { username, password };
    let res = run_async(onvif_media::derive_rtsp_uri(
        device_service_url,
        &creds,
        profile_token,
        ONVIF_RESOLVE_TIMEOUT_MS,
    ));
    drop(creds); // best-effort scrub; AES-GCM decrypt buffer lives in `plain`
    drop(plain);
    match res {
        Ok(stream) => Ok(OnvifResolveOk {
            rtsp_uri: stream.rtsp_uri,
            profile_token: stream.profile_token,
        }),
        Err(e) => Err(map_onvif_resolve_error(&e)),
    }
}

// =============================================================================
// Host function: camera_add_v1
// =============================================================================

pub fn camera_add_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    if !check_permission(caller.data(), PERM_CAMERAS_WRITE, None) {
        audit(
            caller.data(),
            "camera.add",
            None,
            RiskClass::A,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let mut input: CameraAddInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::ServiceCall,
    ) {
        Ok(v) => v,
        Err(e) => {
            audit(
                caller.data(),
                "camera.add",
                None,
                RiskClass::A,
                "error",
                Some(if e == AbiError::PayloadTooLarge {
                    "payload_too_large"
                } else {
                    "invalid_payload"
                }),
            );
            return e.as_i32();
        }
    };
    // Resolve the legacy TOML defaults for the optional-on-wire fields so a
    // minimal payload behaves exactly like the old TOML path (30 / "C" /
    // "default").
    let target_fps = input.target_fps_or_default();
    let analysis_fps = input.analysis_fps_or_default();
    let retention_class = input.retention_class_or_default();
    let profile = input.profile_or_default();
    if !analysis_fps_valid(analysis_fps) {
        audit(
            caller.data(),
            "camera.add",
            None,
            RiskClass::A,
            "denied",
            Some("analysis_fps_out_of_range"),
        );
        return AbiError::Operation.as_i32();
    }
    if let Err(reason) = validate_vendor(&input.vendor) {
        let err = if reason == "unsupported_vendor" {
            AbiError::CameraVendorUnsupported
        } else {
            AbiError::Operation
        };
        audit(
            caller.data(),
            "camera.add",
            None,
            RiskClass::A,
            "denied",
            Some(reason),
        );
        return err.as_i32();
    }
    if let Err(reason) = validate_url(&input.url) {
        audit(
            caller.data(),
            "camera.add",
            None,
            RiskClass::A,
            "denied",
            Some(reason),
        );
        return AbiError::Operation.as_i32();
    }
    if !(1..=60).contains(&target_fps) {
        audit(
            caller.data(),
            "camera.add",
            None,
            RiskClass::A,
            "denied",
            Some("target_fps_out_of_range"),
        );
        return AbiError::Operation.as_i32();
    }
    if let Err(reason) = validate_retention(&retention_class) {
        audit(
            caller.data(),
            "camera.add",
            None,
            RiskClass::A,
            "denied",
            Some(reason),
        );
        return AbiError::Operation.as_i32();
    }
    if let Err(reason) = validate_display_name(&input.display_name) {
        audit(
            caller.data(),
            "camera.add",
            None,
            RiskClass::A,
            "denied",
            Some(reason),
        );
        return AbiError::Operation.as_i32();
    }
    if let Err(reason) = validate_profile(&profile) {
        audit(
            caller.data(),
            "camera.add",
            None,
            RiskClass::A,
            "denied",
            Some(reason),
        );
        return AbiError::Operation.as_i32();
    }
    let credentials_blob = match prepare_credentials_blob(input.credentials_b64.as_deref()) {
        Ok(v) => v,
        Err(reason) => {
            audit(
                caller.data(),
                "camera.add",
                None,
                RiskClass::A,
                "denied",
                Some(reason),
            );
            return AbiError::Operation.as_i32();
        }
    };

    // For `vendor='onvif'` the supplied URL is the device-service endpoint —
    // not a streamable URI. Resolve it via SOAP (GetProfiles + GetStreamUri)
    // to an `rtsp://` URI before the supervisor session starts. Credentials
    // are mandatory in this mode; the same encrypted blob feeds both the
    // SOAP UsernameToken digest and the RTSP connector at session start.
    let onvif_url_to_persist;
    let onvif_token_to_persist;
    if input.vendor == "onvif" {
        let Some(blob) = credentials_blob.as_deref() else {
            audit(
                caller.data(),
                "camera.add",
                None,
                RiskClass::A,
                "denied",
                Some("missing_credentials"),
            );
            return AbiError::Operation.as_i32();
        };
        if let Some(tok) = &input.onvif_profile_token {
            if !profile_valid(tok) {
                audit(
                    caller.data(),
                    "camera.add",
                    None,
                    RiskClass::A,
                    "denied",
                    Some("onvif_profile_token_invalid"),
                );
                return AbiError::Operation.as_i32();
            }
        }
        match resolve_onvif_one_click(&input.url, blob, input.onvif_profile_token.as_deref()) {
            Ok(ok) => {
                onvif_url_to_persist = Some(input.url.clone());
                onvif_token_to_persist = Some(ok.profile_token);
                input.url = ok.rtsp_uri;
            }
            Err((err, reason)) => {
                audit(
                    caller.data(),
                    "camera.add",
                    None,
                    RiskClass::A,
                    "error",
                    Some(reason),
                );
                return err.as_i32();
            }
        }
    } else {
        onvif_url_to_persist = None;
        onvif_token_to_persist = None;
    }

    let camera_id = format!("cam_{}", uuid::Uuid::new_v4());
    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();
    let org_id_for_insert = caller.data().org_id.clone();

    let res_w = input.resolution_width.map(|v| v as i64);
    let res_h = input.resolution_height.map(|v| v as i64);

    // Supervisor session first — if the pipeline fails we never write a row
    // and so never need a compensating delete. If the host crashes between
    // supervisor start and DB insert the in-memory registry dies with the
    // process; reconciliation at lazy-init drives the steady-state.
    // ONVIF cameras are streamed as RTSP (the derived URI), so the
    // supervisor vendor is rewritten to `rtsp` while the DB row preserves
    // `onvif` for UI and re-derivation lookups.
    let session_vendor = if input.vendor == "onvif" {
        "rtsp"
    } else {
        input.vendor.as_str()
    };
    let cfg = CameraConfig {
        camera_id: camera_id.clone(),
        vendor: session_vendor.to_string(),
        url: input.url.clone(),
        target_fps,
        resolution: match (input.resolution_width, input.resolution_height) {
            (Some(w), Some(h)) => Some((w, h)),
            _ => None,
        },
        owner_addon_id: Some(addon_id.clone()),
        credentials_encrypted: credentials_blob.clone(),
        decoder_override: None,
    };
    let sup = match run_async(get_or_init_supervisor()) {
        Ok(s) => s,
        Err(e) => {
            audit(
                caller.data(),
                "camera.add",
                Some(&camera_id),
                RiskClass::A,
                "error",
                Some("supervisor_init_failed"),
            );
            return e.as_i32();
        }
    };
    if let Err(e) = run_async(sup.add_camera(cfg)) {
        let mapped = map_ingest_error(&e);
        audit(
            caller.data(),
            "camera.add",
            Some(&camera_id),
            RiskClass::A,
            "error",
            Some(&format!("session_start_failed: {e}")),
        );
        return mapped.as_i32();
    }

    if let Err(e) = insert_camera(
        &db,
        &camera_id,
        &addon_id,
        &input.display_name,
        &input.vendor,
        &input.url,
        target_fps as i64,
        analysis_fps as i64,
        res_w,
        res_h,
        &retention_class,
        &profile,
        credentials_blob.as_deref(),
        onvif_url_to_persist.as_deref(),
        onvif_token_to_persist.as_deref(),
        org_id_for_insert.as_deref(),
    ) {
        warn!("camera.add insert_camera failed (compensating remove_camera): {e}");
        // Compensate the started session so the registry stays consistent.
        let _ = run_async(sup.remove_camera(&camera_id));
        audit(
            caller.data(),
            "camera.add",
            Some(&camera_id),
            RiskClass::A,
            "error",
            Some("db_insert_failed"),
        );
        return AbiError::Operation.as_i32();
    }

    audit(
        caller.data(),
        "camera.add",
        Some(&camera_id),
        RiskClass::A,
        "ok",
        None,
    );
    let out = CameraAddOutput {
        camera_id: camera_id.clone(),
        status: "starting".to_string(),
    };
    write_cbor_capped(
        &memory,
        &mut caller,
        &out,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::ServiceCall,
    )
}

// =============================================================================
// Host function: camera_list_v1
// =============================================================================

pub fn camera_list_v1(
    mut caller: WasmCaller<'_, AddonState>,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    if !check_permission(caller.data(), PERM_CAMERAS_READ, None) {
        audit(
            caller.data(),
            "camera.list",
            None,
            RiskClass::B,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();
    let org_id_for_query = caller.data().org_id.clone();
    let rows = match list_cameras_for_addon(&db, &addon_id, org_id_for_query.as_deref()) {
        Ok(v) => v,
        Err(_) => {
            audit(
                caller.data(),
                "camera.list",
                None,
                RiskClass::B,
                "error",
                Some("db_error"),
            );
            return AbiError::Operation.as_i32();
        }
    };
    let out = run_async(async {
        let sup = match get_or_init_supervisor().await {
            Ok(s) => s,
            Err(_) => return Err(AbiError::Operation),
        };
        let mut list = Vec::with_capacity(rows.len());
        for r in rows {
            list.push(build_camera_info(&sup, r).await);
        }
        Ok(CameraListOut { camera: list })
    });
    let out = match out {
        Ok(v) => v,
        Err(e) => {
            audit(
                caller.data(),
                "camera.list",
                None,
                RiskClass::B,
                "error",
                Some("supervisor_unavailable"),
            );
            return e.as_i32();
        }
    };
    audit(caller.data(), "camera.list", None, RiskClass::B, "ok", None);
    write_cbor_capped(
        &memory,
        &mut caller,
        &out,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::ServiceCall,
    )
}

// =============================================================================
// Host function: camera_list_accessible_v1
// =============================================================================
//
// Cross-addon discovery: lists every active camera the calling addon may read in
// its org — cameras it owns UNION cameras granted to it (or `'*'`). Each entry
// carries `owner_addon_id` and `access_level` ("owner"|"granted") so a consumer
// addon (e.g. TentaVision) can tell which cameras come from a grant.

pub fn camera_list_accessible_v1(
    mut caller: WasmCaller<'_, AddonState>,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    if !check_permission(caller.data(), PERM_CAMERAS_READ, None) {
        audit(
            caller.data(),
            "camera.list_accessible",
            None,
            RiskClass::B,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();
    let org = caller.data().org_id.clone();
    let rows = match list_accessible_cameras(&db, &addon_id, org.as_deref()) {
        Ok(v) => v,
        Err(_) => {
            audit(
                caller.data(),
                "camera.list_accessible",
                None,
                RiskClass::B,
                "error",
                Some("db_error"),
            );
            return AbiError::Operation.as_i32();
        }
    };
    let out = run_async(async {
        let sup = match get_or_init_supervisor().await {
            Ok(s) => s,
            Err(_) => return Err(AbiError::Operation),
        };
        let mut list = Vec::with_capacity(rows.len());
        for r in rows {
            let owner = r.owner_addon_id.clone();
            let mut info = build_camera_info(&sup, r).await;
            let is_owner = owner == addon_id;
            info.owner_addon_id = Some(owner);
            info.access_level = Some(if is_owner { "owner" } else { "granted" }.to_string());
            // The url can embed credentials (e.g. rtsp://user:pass@host) — never
            // expose it to a non-owner grantee.
            if !is_owner {
                info.url = String::new();
            }
            list.push(info);
        }
        Ok(CameraListOut { camera: list })
    });
    let out = match out {
        Ok(v) => v,
        Err(e) => {
            audit(
                caller.data(),
                "camera.list_accessible",
                None,
                RiskClass::B,
                "error",
                Some("supervisor_unavailable"),
            );
            return e.as_i32();
        }
    };
    audit(
        caller.data(),
        "camera.list_accessible",
        None,
        RiskClass::B,
        "ok",
        None,
    );
    write_cbor_capped(
        &memory,
        &mut caller,
        &out,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::ServiceCall,
    )
}

// =============================================================================
// Grant authorization helper
// =============================================================================

/// Authorizes a grant-management op (grant / revoke / list grants) on
/// `camera_id`. Allowed when the caller is an explicit system call OR owns the
/// active camera in its org. A non-owner non-system addon is denied even if it
/// merely holds a read grant — only the owner (or the host) administers grants.
///
/// Returns `Ok(true)` when authorized, `Ok(false)` when denied (caller surfaces
/// `NotFound` to avoid leaking existence), `Err` on DB failure.
fn authorize_grant_admin(
    state: &AddonState,
    camera_id: &str,
) -> std::result::Result<bool, AbiError> {
    if state.is_system_call {
        return Ok(true);
    }
    match get_camera_for_addon(&state.db, &state.addon_id, camera_id, state.org_id.as_deref()) {
        Ok(Some(_)) => Ok(true),
        Ok(None) => Ok(false),
        Err(_) => Err(AbiError::Operation),
    }
}

/// Allowlisted grant levels. v1 supports cross-addon read only.
fn grant_level_valid(level: &str) -> bool {
    matches!(level, "read")
}

// =============================================================================
// Host function: camera_grant_v1
// =============================================================================

pub fn camera_grant_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    if !check_permission(caller.data(), PERM_CAMERAS_WRITE, None) {
        audit(
            caller.data(),
            "camera.grant",
            None,
            RiskClass::A,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let input: CameraGrantInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::ServiceCall,
    ) {
        Ok(v) => v,
        Err(e) => {
            audit(
                caller.data(),
                "camera.grant",
                None,
                RiskClass::A,
                "error",
                Some(if e == AbiError::PayloadTooLarge {
                    "payload_too_large"
                } else {
                    "invalid_payload"
                }),
            );
            return e.as_i32();
        }
    };
    if !camera_id_valid(&input.camera_id) {
        audit(
            caller.data(),
            "camera.grant",
            None,
            RiskClass::A,
            "denied",
            Some("camera_id_invalid"),
        );
        return AbiError::Operation.as_i32();
    }
    if !grant_level_valid(&input.level) {
        audit(
            caller.data(),
            "camera.grant",
            Some(&input.camera_id),
            RiskClass::A,
            "denied",
            Some("level_invalid"),
        );
        return AbiError::Operation.as_i32();
    }
    if input.grantee_addon_id.is_empty() || input.grantee_addon_id.len() > 256 {
        audit(
            caller.data(),
            "camera.grant",
            Some(&input.camera_id),
            RiskClass::A,
            "denied",
            Some("grantee_invalid"),
        );
        return AbiError::Operation.as_i32();
    }
    match authorize_grant_admin(caller.data(), &input.camera_id) {
        Ok(true) => {}
        Ok(false) => {
            audit(
                caller.data(),
                "camera.grant",
                Some(&input.camera_id),
                RiskClass::A,
                "denied",
                Some("not_owner"),
            );
            return AbiError::NotFound.as_i32();
        }
        Err(e) => {
            audit(
                caller.data(),
                "camera.grant",
                Some(&input.camera_id),
                RiskClass::A,
                "error",
                Some("db_error"),
            );
            return e.as_i32();
        }
    }
    let org = caller
        .data()
        .org_id
        .clone()
        .unwrap_or_else(|| crate::services::org::DEFAULT_ORG_ID.to_string());
    let created_by = caller
        .data()
        .user_id
        .clone()
        .unwrap_or_else(|| caller.data().addon_id.clone());
    if grant_camera(
        &caller.data().db.clone(),
        &input.camera_id,
        &input.grantee_addon_id,
        &input.level,
        &org,
        &created_by,
    )
    .is_err()
    {
        audit(
            caller.data(),
            "camera.grant",
            Some(&input.camera_id),
            RiskClass::A,
            "error",
            Some("db_error"),
        );
        return AbiError::Operation.as_i32();
    }
    audit(
        caller.data(),
        "camera.grant",
        Some(&input.camera_id),
        RiskClass::A,
        "ok",
        None,
    );
    write_cbor_capped(
        &memory,
        &mut caller,
        &CameraGrantOut { ok: true },
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::ServiceCall,
    )
}

// =============================================================================
// Host function: camera_revoke_v1
// =============================================================================

pub fn camera_revoke_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    if !check_permission(caller.data(), PERM_CAMERAS_WRITE, None) {
        audit(
            caller.data(),
            "camera.revoke",
            None,
            RiskClass::A,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let input: CameraRevokeInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::ServiceCall,
    ) {
        Ok(v) => v,
        Err(e) => {
            audit(
                caller.data(),
                "camera.revoke",
                None,
                RiskClass::A,
                "error",
                Some(if e == AbiError::PayloadTooLarge {
                    "payload_too_large"
                } else {
                    "invalid_payload"
                }),
            );
            return e.as_i32();
        }
    };
    if !camera_id_valid(&input.camera_id) {
        audit(
            caller.data(),
            "camera.revoke",
            None,
            RiskClass::A,
            "denied",
            Some("camera_id_invalid"),
        );
        return AbiError::Operation.as_i32();
    }
    if !grant_level_valid(&input.level) {
        audit(
            caller.data(),
            "camera.revoke",
            Some(&input.camera_id),
            RiskClass::A,
            "denied",
            Some("level_invalid"),
        );
        return AbiError::Operation.as_i32();
    }
    match authorize_grant_admin(caller.data(), &input.camera_id) {
        Ok(true) => {}
        Ok(false) => {
            audit(
                caller.data(),
                "camera.revoke",
                Some(&input.camera_id),
                RiskClass::A,
                "denied",
                Some("not_owner"),
            );
            return AbiError::NotFound.as_i32();
        }
        Err(e) => {
            audit(
                caller.data(),
                "camera.revoke",
                Some(&input.camera_id),
                RiskClass::A,
                "error",
                Some("db_error"),
            );
            return e.as_i32();
        }
    }
    let org = caller
        .data()
        .org_id
        .clone()
        .unwrap_or_else(|| crate::services::org::DEFAULT_ORG_ID.to_string());
    let removed = match revoke_camera_grant(
        &caller.data().db.clone(),
        &input.camera_id,
        &input.grantee_addon_id,
        &input.level,
        &org,
    ) {
        Ok(n) => n,
        Err(_) => {
            audit(
                caller.data(),
                "camera.revoke",
                Some(&input.camera_id),
                RiskClass::A,
                "error",
                Some("db_error"),
            );
            return AbiError::Operation.as_i32();
        }
    };
    audit(
        caller.data(),
        "camera.revoke",
        Some(&input.camera_id),
        RiskClass::A,
        if removed > 0 { "ok" } else { "ok_noop" },
        None,
    );
    write_cbor_capped(
        &memory,
        &mut caller,
        &CameraGrantOut { ok: removed > 0 },
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::ServiceCall,
    )
}

// =============================================================================
// Host function: camera_grants_list_v1
// =============================================================================

pub fn camera_grants_list_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    if !check_permission(caller.data(), PERM_CAMERAS_READ, None) {
        audit(
            caller.data(),
            "camera.grants_list",
            None,
            RiskClass::B,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let input: CameraGrantListInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::ServiceCall,
    ) {
        Ok(v) => v,
        Err(e) => {
            audit(
                caller.data(),
                "camera.grants_list",
                None,
                RiskClass::B,
                "error",
                Some(if e == AbiError::PayloadTooLarge {
                    "payload_too_large"
                } else {
                    "invalid_payload"
                }),
            );
            return e.as_i32();
        }
    };
    if !camera_id_valid(&input.camera_id) {
        audit(
            caller.data(),
            "camera.grants_list",
            None,
            RiskClass::B,
            "denied",
            Some("camera_id_invalid"),
        );
        return AbiError::Operation.as_i32();
    }
    match authorize_grant_admin(caller.data(), &input.camera_id) {
        Ok(true) => {}
        Ok(false) => {
            audit(
                caller.data(),
                "camera.grants_list",
                Some(&input.camera_id),
                RiskClass::B,
                "denied",
                Some("not_owner"),
            );
            return AbiError::NotFound.as_i32();
        }
        Err(e) => {
            audit(
                caller.data(),
                "camera.grants_list",
                Some(&input.camera_id),
                RiskClass::B,
                "error",
                Some("db_error"),
            );
            return e.as_i32();
        }
    }
    let org = caller
        .data()
        .org_id
        .clone()
        .unwrap_or_else(|| crate::services::org::DEFAULT_ORG_ID.to_string());
    let grants = match list_camera_grants(&caller.data().db.clone(), &input.camera_id, &org) {
        Ok(v) => v,
        Err(_) => {
            audit(
                caller.data(),
                "camera.grants_list",
                Some(&input.camera_id),
                RiskClass::B,
                "error",
                Some("db_error"),
            );
            return AbiError::Operation.as_i32();
        }
    };
    let out = CameraGrantListOut {
        grants: grants
            .into_iter()
            .map(|(grantee_addon_id, level, created_by)| CameraGrantInfo {
                grantee_addon_id,
                level,
                created_by,
            })
            .collect(),
    };
    audit(
        caller.data(),
        "camera.grants_list",
        Some(&input.camera_id),
        RiskClass::B,
        "ok",
        None,
    );
    write_cbor_capped(
        &memory,
        &mut caller,
        &out,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::ServiceCall,
    )
}

// =============================================================================
// Host function: camera_local_devices_v1
// =============================================================================
//
// Enumerates locally attached camera devices (USB / v4l2) so a wizard can offer
// a device dropdown instead of a free-text path. Read-only discovery, so it
// reuses `cameras.read` rather than introducing a new permission. Enumeration
// is Linux/v4l2 today; other platforms return an empty list (not an error).

pub fn camera_local_devices_v1(
    mut caller: WasmCaller<'_, AddonState>,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    if !check_permission(caller.data(), PERM_CAMERAS_READ, None) {
        audit(
            caller.data(),
            "camera.local_devices",
            None,
            RiskClass::B,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let devices = list_local_devices()
        .into_iter()
        .map(|d| LocalCameraDeviceOut {
            device_path: d.device_path,
            label: d.label,
            vendor: d.vendor,
        })
        .collect();
    let out = LocalCameraDevicesOut { devices };
    audit(
        caller.data(),
        "camera.local_devices",
        None,
        RiskClass::B,
        "ok",
        None,
    );
    write_cbor_capped(
        &memory,
        &mut caller,
        &out,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::ServiceCall,
    )
}

// =============================================================================
// Host function: camera_get_v1
// =============================================================================

pub fn camera_get_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    if !check_permission(caller.data(), PERM_CAMERAS_READ, None) {
        audit(
            caller.data(),
            "camera.get",
            None,
            RiskClass::B,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let input: CameraIdInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::ServiceCall,
    ) {
        Ok(v) => v,
        Err(e) => {
            audit(
                caller.data(),
                "camera.get",
                None,
                RiskClass::B,
                "error",
                Some(if e == AbiError::PayloadTooLarge {
                    "payload_too_large"
                } else {
                    "invalid_payload"
                }),
            );
            return e.as_i32();
        }
    };
    if !camera_id_valid(&input.camera_id) {
        audit(
            caller.data(),
            "camera.get",
            None,
            RiskClass::B,
            "denied",
            Some("camera_id_invalid"),
        );
        return AbiError::Operation.as_i32();
    }
    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();
    let org = caller.data().org_id.clone();
    // Read widens to granted addons: gate on `can_read_camera` (owner OR grant),
    // then fetch ignoring the owner filter. A denied caller gets NotFound so the
    // existence of another addon's camera id is never leaked.
    match can_read_camera(&db, &addon_id, &input.camera_id, org.as_deref()) {
        Ok(true) => {}
        Ok(false) => {
            audit(
                caller.data(),
                "camera.get",
                Some(&input.camera_id),
                RiskClass::B,
                "denied",
                Some("not_found_or_not_authorized"),
            );
            return AbiError::NotFound.as_i32();
        }
        Err(_) => {
            audit(
                caller.data(),
                "camera.get",
                Some(&input.camera_id),
                RiskClass::B,
                "error",
                Some("db_error"),
            );
            return AbiError::Operation.as_i32();
        }
    }
    let row = match get_camera_in_org(&db, &input.camera_id, org.as_deref()) {
        Ok(Some(r)) => r,
        Ok(None) => {
            audit(
                caller.data(),
                "camera.get",
                Some(&input.camera_id),
                RiskClass::B,
                "denied",
                Some("not_found_or_not_authorized"),
            );
            return AbiError::NotFound.as_i32();
        }
        Err(_) => {
            audit(
                caller.data(),
                "camera.get",
                Some(&input.camera_id),
                RiskClass::B,
                "error",
                Some("db_error"),
            );
            return AbiError::Operation.as_i32();
        }
    };
    let owner = row.owner_addon_id.clone();
    let is_owner = owner == addon_id;
    let info = run_async(async {
        match get_or_init_supervisor().await {
            Ok(sup) => Some(build_camera_info(&sup, row.clone()).await),
            Err(_) => None,
        }
    });
    let mut info = match info {
        Some(v) => v,
        None => CameraInfoOut {
            camera_id: row.camera_id,
            display_name: row.display_name,
            vendor: row.vendor,
            url: row.url,
            target_fps: row.target_fps,
            resolution_width: row.resolution_width,
            resolution_height: row.resolution_height,
            status: row.status,
            status_message: row.status_message,
            fps_actual: row.fps_actual,
            last_frame_at: row.last_frame_at,
            retention_class: row.retention_class,
            profile: row.profile,
            analysis_flow_id: row.analysis_flow_id,
            owner_addon_id: None,
            access_level: None,
        },
    };
    info.owner_addon_id = Some(owner);
    info.access_level = Some(if is_owner { "owner" } else { "granted" }.to_string());
    // The url can embed credentials (e.g. rtsp://user:pass@host) — never expose
    // it to a non-owner grantee.
    if !is_owner {
        info.url = String::new();
    }
    audit(
        caller.data(),
        "camera.get",
        Some(&info.camera_id),
        RiskClass::B,
        "ok",
        None,
    );
    write_cbor_capped(
        &memory,
        &mut caller,
        &info,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::ServiceCall,
    )
}

// =============================================================================
// Host function: camera_analysis_flows_list_v1
// =============================================================================

/// Lists the active flows assignable as a camera's analysis flow (id + name),
/// for the per-camera flow selector. Read-only, gated on `cameras.read`. Scoped
/// to `service_type='camera_analysis'` so an addon cannot enumerate unrelated
/// flows through this surface.
pub fn camera_analysis_flows_list_v1(
    mut caller: WasmCaller<'_, AddonState>,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    if !check_permission(caller.data(), PERM_CAMERAS_READ, None) {
        audit(
            caller.data(),
            "camera.analysis_flows_list",
            None,
            RiskClass::B,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let db = caller.data().db.clone();
    let flows = match crate::db::repository::list_camera_analysis_flows(&db) {
        Ok(v) => v,
        Err(_) => {
            audit(
                caller.data(),
                "camera.analysis_flows_list",
                None,
                RiskClass::B,
                "error",
                Some("db_error"),
            );
            return AbiError::Operation.as_i32();
        }
    };
    let out = CameraAnalysisFlowsOut {
        flows: flows
            .into_iter()
            .map(|(id, name)| CameraAnalysisFlowOut { id, name })
            .collect(),
    };
    write_cbor_capped(
        &memory,
        &mut caller,
        &out,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::ServiceCall,
    )
}

// =============================================================================
// Host function: camera_update_v1
// =============================================================================

pub fn camera_update_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    if !check_permission(caller.data(), PERM_CAMERAS_WRITE, None) {
        audit(
            caller.data(),
            "camera.update",
            None,
            RiskClass::A,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let input: CameraUpdateInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::ServiceCall,
    ) {
        Ok(v) => v,
        Err(e) => {
            audit(
                caller.data(),
                "camera.update",
                None,
                RiskClass::A,
                "error",
                Some(if e == AbiError::PayloadTooLarge {
                    "payload_too_large"
                } else {
                    "invalid_payload"
                }),
            );
            return e.as_i32();
        }
    };
    if !camera_id_valid(&input.camera_id) {
        audit(
            caller.data(),
            "camera.update",
            None,
            RiskClass::A,
            "denied",
            Some("camera_id_invalid"),
        );
        return AbiError::Operation.as_i32();
    }

    if let Some(fps) = input.target_fps {
        if !(1..=60).contains(&fps) {
            audit(
                caller.data(),
                "camera.update",
                Some(&input.camera_id),
                RiskClass::A,
                "denied",
                Some("target_fps_out_of_range"),
            );
            return AbiError::Operation.as_i32();
        }
    }
    if let Some(fps) = input.analysis_fps {
        if !analysis_fps_valid(fps) {
            audit(
                caller.data(),
                "camera.update",
                Some(&input.camera_id),
                RiskClass::A,
                "denied",
                Some("analysis_fps_out_of_range"),
            );
            return AbiError::Operation.as_i32();
        }
    }
    if let Some(rc) = input.retention_class.as_ref() {
        if let Err(reason) = validate_retention(rc) {
            audit(
                caller.data(),
                "camera.update",
                Some(&input.camera_id),
                RiskClass::A,
                "denied",
                Some(reason),
            );
            return AbiError::Operation.as_i32();
        }
    }
    if let Some(n) = input.display_name.as_ref() {
        if let Err(reason) = validate_display_name(n) {
            audit(
                caller.data(),
                "camera.update",
                Some(&input.camera_id),
                RiskClass::A,
                "denied",
                Some(reason),
            );
            return AbiError::Operation.as_i32();
        }
    }
    if let Some(p) = input.profile.as_ref() {
        if let Err(reason) = validate_profile(p) {
            audit(
                caller.data(),
                "camera.update",
                Some(&input.camera_id),
                RiskClass::A,
                "denied",
                Some(reason),
            );
            return AbiError::Operation.as_i32();
        }
    }

    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();

    // Assignment is the authorization gate for the system-triggered camera flow
    // runner (which dispatches with no user, so per-user ACL does not apply
    // there). A non-empty `analysis_flow_id` must reference an existing, active
    // flow; an empty string clears the assignment. `flows` are global (no
    // org_id column), so there is no per-org flow scope to enforce here.
    let analysis_flow_id: Option<Option<String>> = match input.analysis_flow_id.as_ref() {
        None => None,
        Some(s) if s.is_empty() => Some(None),
        Some(id) => {
            match crate::db::repository::get_flow(&db, id) {
                Ok(Some(flow)) if flow.status == "active" => Some(Some(id.clone())),
                Ok(Some(_)) => {
                    audit(
                        caller.data(),
                        "camera.update",
                        Some(&input.camera_id),
                        RiskClass::A,
                        "denied",
                        Some("analysis_flow_not_active"),
                    );
                    return AbiError::Operation.as_i32();
                }
                Ok(None) => {
                    audit(
                        caller.data(),
                        "camera.update",
                        Some(&input.camera_id),
                        RiskClass::A,
                        "denied",
                        Some("analysis_flow_not_found"),
                    );
                    return AbiError::NotFound.as_i32();
                }
                Err(_) => {
                    audit(
                        caller.data(),
                        "camera.update",
                        Some(&input.camera_id),
                        RiskClass::A,
                        "error",
                        Some("analysis_flow_lookup_failed"),
                    );
                    return AbiError::Operation.as_i32();
                }
            }
        }
    };

    match get_camera_for_addon(
        &db,
        &addon_id,
        &input.camera_id,
        caller.data().org_id.as_deref(),
    ) {
        Ok(Some(_)) => {}
        Ok(None) => {
            audit(
                caller.data(),
                "camera.update",
                Some(&input.camera_id),
                RiskClass::A,
                "denied",
                Some("not_found_or_not_owned"),
            );
            return AbiError::NotFound.as_i32();
        }
        Err(_) => {
            audit(
                caller.data(),
                "camera.update",
                Some(&input.camera_id),
                RiskClass::A,
                "error",
                Some("db_error"),
            );
            return AbiError::Operation.as_i32();
        }
    }

    let mut diff: Vec<&'static str> = Vec::new();
    if input.display_name.is_some() {
        diff.push("display_name");
    }
    if input.target_fps.is_some() {
        diff.push("target_fps");
    }
    if input.analysis_fps.is_some() {
        diff.push("analysis_fps");
    }
    if input.resolution_width.is_some() {
        diff.push("resolution_width");
    }
    if input.resolution_height.is_some() {
        diff.push("resolution_height");
    }
    if input.retention_class.is_some() {
        diff.push("retention_class");
    }
    if input.profile.is_some() {
        diff.push("profile");
    }
    if analysis_flow_id.is_some() {
        diff.push("analysis_flow_id");
    }
    let patch = CameraPatch {
        display_name: input.display_name.clone(),
        target_fps: input.target_fps.map(|v| v as i64),
        analysis_fps: input.analysis_fps.map(|v| v as i64),
        resolution_width: input.resolution_width.map(|v| Some(v as i64)),
        resolution_height: input.resolution_height.map(|v| Some(v as i64)),
        retention_class: input.retention_class.clone(),
        profile: input.profile.clone(),
        analysis_flow_id,
    };

    if update_camera(
        &db,
        &addon_id,
        &input.camera_id,
        &patch,
        caller.data().org_id.as_deref(),
    )
    .is_err()
    {
        audit(
            caller.data(),
            "camera.update",
            Some(&input.camera_id),
            RiskClass::A,
            "error",
            Some("db_update_failed"),
        );
        return AbiError::Operation.as_i32();
    }

    let row = match get_camera_for_addon(
        &db,
        &addon_id,
        &input.camera_id,
        caller.data().org_id.as_deref(),
    ) {
        Ok(Some(r)) => r,
        Ok(None) => {
            audit(
                caller.data(),
                "camera.update",
                Some(&input.camera_id),
                RiskClass::A,
                "error",
                Some("row_disappeared_after_update"),
            );
            return AbiError::Operation.as_i32();
        }
        Err(_) => {
            audit(
                caller.data(),
                "camera.update",
                Some(&input.camera_id),
                RiskClass::A,
                "error",
                Some("db_error_after_update"),
            );
            return AbiError::Operation.as_i32();
        }
    };
    let info = run_async(async {
        if let Ok(sup) = get_or_init_supervisor().await {
            build_camera_info(&sup, row.clone()).await
        } else {
            CameraInfoOut {
                camera_id: row.camera_id,
                display_name: row.display_name,
                vendor: row.vendor,
                url: row.url,
                target_fps: row.target_fps,
                resolution_width: row.resolution_width,
                resolution_height: row.resolution_height,
                status: row.status,
                status_message: row.status_message,
                fps_actual: row.fps_actual,
                last_frame_at: row.last_frame_at,
                retention_class: row.retention_class,
                profile: row.profile,
                analysis_flow_id: row.analysis_flow_id,
                owner_addon_id: None,
                access_level: None,
            }
        }
    });

    let reason = format!("fields={}", diff.join(","));
    audit(
        caller.data(),
        "camera.update",
        Some(&info.camera_id),
        RiskClass::A,
        "ok",
        Some(&reason),
    );
    write_cbor_capped(
        &memory,
        &mut caller,
        &info,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::ServiceCall,
    )
}

// =============================================================================
// Host function: camera_remove_v1
// =============================================================================

pub fn camera_remove_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    if !check_permission(caller.data(), PERM_CAMERAS_WRITE, None) {
        audit(
            caller.data(),
            "camera.remove",
            None,
            RiskClass::A,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let input: CameraIdInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::ServiceCall,
    ) {
        Ok(v) => v,
        Err(e) => {
            audit(
                caller.data(),
                "camera.remove",
                None,
                RiskClass::A,
                "error",
                Some(if e == AbiError::PayloadTooLarge {
                    "payload_too_large"
                } else {
                    "invalid_payload"
                }),
            );
            return e.as_i32();
        }
    };
    if !camera_id_valid(&input.camera_id) {
        audit(
            caller.data(),
            "camera.remove",
            None,
            RiskClass::A,
            "denied",
            Some("camera_id_invalid"),
        );
        return AbiError::Operation.as_i32();
    }
    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();

    match get_camera_for_addon(
        &db,
        &addon_id,
        &input.camera_id,
        caller.data().org_id.as_deref(),
    ) {
        Ok(Some(_)) => {}
        Ok(None) => {
            audit(
                caller.data(),
                "camera.remove",
                Some(&input.camera_id),
                RiskClass::A,
                "denied",
                Some("not_found_or_not_owned"),
            );
            return AbiError::NotFound.as_i32();
        }
        Err(_) => {
            audit(
                caller.data(),
                "camera.remove",
                Some(&input.camera_id),
                RiskClass::A,
                "error",
                Some("db_error"),
            );
            return AbiError::Operation.as_i32();
        }
    }

    // DB soft-delete first — once committed the row is hidden from `list`,
    // so even if the supervisor remove fails (timeout, NotFound) the camera
    // is effectively gone from the addon's perspective. A leftover in-memory
    // session is bounded by process lifetime and falls off at next restart
    // because reconciliation skips `removed_at IS NOT NULL` rows.
    match soft_delete_camera(
        &db,
        &addon_id,
        &input.camera_id,
        caller.data().org_id.as_deref(),
    ) {
        Ok(true) => {}
        Ok(false) => {
            audit(
                caller.data(),
                "camera.remove",
                Some(&input.camera_id),
                RiskClass::A,
                "denied",
                Some("not_found"),
            );
            return AbiError::NotFound.as_i32();
        }
        Err(_) => {
            audit(
                caller.data(),
                "camera.remove",
                Some(&input.camera_id),
                RiskClass::A,
                "error",
                Some("db_error"),
            );
            return AbiError::Operation.as_i32();
        }
    }

    let sup_result = run_async(async {
        match get_or_init_supervisor().await {
            Ok(sup) => sup.remove_camera(&input.camera_id).await,
            Err(_) => Ok(()),
        }
    });
    if let Err(e) = sup_result {
        if !matches!(e, CameraIngestError::NotFound(_)) {
            warn!("camera.remove supervisor.remove_camera (post-soft-delete): {e}");
        }
    }

    audit(
        caller.data(),
        "camera.remove",
        Some(&input.camera_id),
        RiskClass::A,
        "ok",
        None,
    );
    let out = CameraRemoveOut { removed: true };
    write_cbor_capped(
        &memory,
        &mut caller,
        &out,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::ServiceCall,
    )
}

// =============================================================================
// Host function: camera_snapshot_v1
// =============================================================================

pub fn camera_snapshot_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    if !check_permission(caller.data(), PERM_CAMERAS_SNAPSHOT, None) {
        audit(
            caller.data(),
            "camera.snapshot",
            None,
            RiskClass::A,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let input: CameraIdInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::ServiceCall,
    ) {
        Ok(v) => v,
        Err(e) => {
            audit(
                caller.data(),
                "camera.snapshot",
                None,
                RiskClass::A,
                "error",
                Some(if e == AbiError::PayloadTooLarge {
                    "payload_too_large"
                } else {
                    "invalid_payload"
                }),
            );
            return e.as_i32();
        }
    };
    if !camera_id_valid(&input.camera_id) {
        audit(
            caller.data(),
            "camera.snapshot",
            None,
            RiskClass::A,
            "denied",
            Some("camera_id_invalid"),
        );
        return AbiError::Operation.as_i32();
    }
    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();
    match get_camera_for_addon(
        &db,
        &addon_id,
        &input.camera_id,
        caller.data().org_id.as_deref(),
    ) {
        Ok(Some(_)) => {}
        Ok(None) => {
            audit(
                caller.data(),
                "camera.snapshot",
                Some(&input.camera_id),
                RiskClass::A,
                "denied",
                Some("not_found_or_not_owned"),
            );
            return AbiError::NotFound.as_i32();
        }
        Err(_) => {
            audit(
                caller.data(),
                "camera.snapshot",
                Some(&input.camera_id),
                RiskClass::A,
                "error",
                Some("db_error"),
            );
            return AbiError::Operation.as_i32();
        }
    }

    let snap = run_async(async {
        let sup = get_or_init_supervisor().await?;
        sup.snapshot(&input.camera_id)
            .await
            .map_err(|e| map_ingest_error(&e))
    });
    let snap = match snap {
        Ok(v) => v,
        Err(e) => {
            audit(
                caller.data(),
                "camera.snapshot",
                Some(&input.camera_id),
                RiskClass::A,
                "error",
                Some(&format!("abi_error={}", e.as_i32())),
            );
            return e.as_i32();
        }
    };

    let data_b64 = base64::engine::general_purpose::STANDARD.encode(&snap.data);
    let out = CameraSnapshotOut {
        camera_id: snap.camera_id,
        width: snap.width,
        height: snap.height,
        pixel_format: "rgb24".to_string(),
        timestamp_unix_ms: snap.timestamp_unix_ms,
        data_b64,
    };

    let bytes_size = snap.data.len();
    audit(
        caller.data(),
        "camera.snapshot",
        Some(&out.camera_id),
        RiskClass::A,
        "ok",
        Some(&format!(
            "w={} h={} bytes={}",
            out.width, out.height, bytes_size
        )),
    );
    write_cbor_capped(
        &memory,
        &mut caller,
        &out,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::ServiceCall,
    )
}

// =============================================================================
// Host function: camera_health_v1
// =============================================================================

pub fn camera_health_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    if !check_permission(caller.data(), PERM_CAMERAS_READ, None) {
        audit(
            caller.data(),
            "camera.health",
            None,
            RiskClass::B,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let input: CameraIdInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::ServiceCall,
    ) {
        Ok(v) => v,
        Err(e) => {
            audit(
                caller.data(),
                "camera.health",
                None,
                RiskClass::B,
                "error",
                Some(if e == AbiError::PayloadTooLarge {
                    "payload_too_large"
                } else {
                    "invalid_payload"
                }),
            );
            return e.as_i32();
        }
    };
    if !camera_id_valid(&input.camera_id) {
        audit(
            caller.data(),
            "camera.health",
            None,
            RiskClass::B,
            "denied",
            Some("camera_id_invalid"),
        );
        return AbiError::Operation.as_i32();
    }
    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();
    let row = match get_camera_for_addon(
        &db,
        &addon_id,
        &input.camera_id,
        caller.data().org_id.as_deref(),
    ) {
        Ok(Some(r)) => r,
        Ok(None) => {
            audit(
                caller.data(),
                "camera.health",
                Some(&input.camera_id),
                RiskClass::B,
                "denied",
                Some("not_found_or_not_owned"),
            );
            return AbiError::NotFound.as_i32();
        }
        Err(_) => {
            audit(
                caller.data(),
                "camera.health",
                Some(&input.camera_id),
                RiskClass::B,
                "error",
                Some("db_error"),
            );
            return AbiError::Operation.as_i32();
        }
    };
    let out = run_async(async {
        let sup = match get_or_init_supervisor().await {
            Ok(s) => s,
            Err(_) => {
                return CameraHealthOut {
                    camera_id: row.camera_id.clone(),
                    status: row.status.clone(),
                    status_message: row.status_message.clone().unwrap_or_default(),
                    fps_actual: row.fps_actual.unwrap_or(0.0),
                    last_frame_at: row.last_frame_at.unwrap_or(0),
                    frames_total: 0,
                    frames_dropped: 0,
                };
            }
        };
        match sup.get_health(&row.camera_id).await {
            Ok(h) => CameraHealthOut {
                camera_id: h.camera_id,
                status: status_to_str(h.status).to_string(),
                status_message: h.status_message.unwrap_or_default(),
                fps_actual: h.fps_actual.unwrap_or(0.0) as f64,
                last_frame_at: h.last_frame_at.map(|v| v as i64).unwrap_or(0),
                frames_total: h.frames_total,
                frames_dropped: h.frames_dropped,
            },
            Err(_) => CameraHealthOut {
                camera_id: row.camera_id.clone(),
                status: row.status.clone(),
                status_message: "session missing".to_string(),
                fps_actual: row.fps_actual.unwrap_or(0.0),
                last_frame_at: row.last_frame_at.unwrap_or(0),
                frames_total: 0,
                frames_dropped: 0,
            },
        }
    });
    audit(
        caller.data(),
        "camera.health",
        Some(&out.camera_id),
        RiskClass::B,
        "ok",
        None,
    );
    write_cbor_capped(
        &memory,
        &mut caller,
        &out,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::ServiceCall,
    )
}

// =============================================================================
// Host function: camera_discover_v1 — WS-Discovery on the local LAN (Risk B)
// =============================================================================

pub fn camera_discover_v1(
    mut caller: WasmCaller<'_, AddonState>,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    if !check_permission(caller.data(), PERM_CAMERAS_WRITE, None) {
        audit(
            caller.data(),
            "camera.discover",
            None,
            RiskClass::B,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let cameras = run_async(async {
        crate::services::camera_ingest::onvif_discovery::discover(
            crate::services::camera_ingest::onvif_discovery::DiscoveryOptions::default(),
        )
        .await
    });
    let discovered: Vec<DiscoveredCameraOut> = match cameras {
        Ok(list) => list
            .into_iter()
            .map(|c| DiscoveredCameraOut {
                address: c.address,
                xaddrs: c.xaddrs,
                types: c.types,
                manufacturer: c.manufacturer,
                model: c.model,
            })
            .collect(),
        Err(e) => {
            warn!("camera.discover ws-discovery failed: {e}");
            audit(
                caller.data(),
                "camera.discover",
                None,
                RiskClass::B,
                "error",
                Some("ws_discovery_failed"),
            );
            Vec::new()
        }
    };
    audit(
        caller.data(),
        "camera.discover",
        None,
        RiskClass::B,
        "ok",
        Some(&format!("count={}", discovered.len())),
    );
    let out = CameraDiscoverOut { discovered };
    write_cbor_capped(
        &memory,
        &mut caller,
        &out,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::ServiceCall,
    )
}

// =============================================================================
// Host function: camera_test_connection_v1 — active probe → Risk A
// =============================================================================

pub fn camera_test_connection_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    if !check_permission(caller.data(), PERM_CAMERAS_WRITE, None) {
        audit(
            caller.data(),
            "camera.test_connection",
            None,
            RiskClass::A,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let input: CameraTestConnectionInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::ServiceCall,
    ) {
        Ok(v) => v,
        Err(e) => {
            audit(
                caller.data(),
                "camera.test_connection",
                None,
                RiskClass::A,
                "error",
                Some(if e == AbiError::PayloadTooLarge {
                    "payload_too_large"
                } else {
                    "invalid_payload"
                }),
            );
            return e.as_i32();
        }
    };
    if input.vendor.is_empty() || input.vendor.len() > MAX_VENDOR {
        audit(
            caller.data(),
            "camera.test_connection",
            None,
            RiskClass::A,
            "denied",
            Some("vendor_length"),
        );
        return AbiError::Operation.as_i32();
    }
    if let Err(reason) = validate_url(&input.url) {
        audit(
            caller.data(),
            "camera.test_connection",
            None,
            RiskClass::A,
            "denied",
            Some(reason),
        );
        return AbiError::Operation.as_i32();
    }
    if !vendor_testable(&input.vendor) {
        audit(
            caller.data(),
            "camera.test_connection",
            None,
            RiskClass::A,
            "ok",
            Some("unsupported_vendor"),
        );
        let out = CameraTestConnectionOut {
            ok: false,
            message: format!("vendor '{}' not supported", input.vendor),
        };
        return write_cbor_capped(
            &memory,
            &mut caller,
            &out,
            out_ptr,
            out_cap,
            out_len_ptr,
            PayloadKind::ServiceCall,
        );
    }
    let out = match input.vendor.as_str() {
        "fake_file" => match crate::services::camera_ingest::fakefile::resolve_file_url(&input.url)
        {
            Ok(_) => CameraTestConnectionOut {
                ok: true,
                message: "fake_file path readable".to_string(),
            },
            Err(e) => CameraTestConnectionOut {
                ok: false,
                message: e.to_string(),
            },
        },
        "rtsp" => {
            if let Err(e) = crate::services::camera_ingest::rtsp::validate_rtsp_url(&input.url) {
                CameraTestConnectionOut {
                    ok: false,
                    message: e.to_string(),
                }
            } else {
                match run_async(rtsp_test_connection(&input.url, 5)) {
                    Ok(()) => CameraTestConnectionOut {
                        ok: true,
                        message: "rtsp OPTIONS 200 OK".to_string(),
                    },
                    Err(msg) => CameraTestConnectionOut {
                        ok: false,
                        message: msg,
                    },
                }
            }
        }
        "onvif" => match run_async(onvif_test_connection(&input.url, 5)) {
            Ok(note) => CameraTestConnectionOut {
                ok: true,
                message: note,
            },
            Err(msg) => CameraTestConnectionOut {
                ok: false,
                message: msg,
            },
        },
        "local_camera" | "v4l2" => {
            match crate::services::camera_ingest::local::validate_local_source(
                &input.vendor,
                &input.url,
            ) {
                Ok(()) => CameraTestConnectionOut {
                    ok: true,
                    message: format!("{} source accepted", input.vendor),
                },
                Err(e) => CameraTestConnectionOut {
                    ok: false,
                    message: e.to_string(),
                },
            }
        }
        other => CameraTestConnectionOut {
            ok: false,
            message: format!("vendor '{other}' has no test_connection handler"),
        },
    };
    audit(
        caller.data(),
        "camera.test_connection",
        None,
        RiskClass::A,
        "ok",
        Some(&format!("ok={}", out.ok)),
    );
    write_cbor_capped(
        &memory,
        &mut caller,
        &out,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::ServiceCall,
    )
}

// =============================================================================
// Host function: camera_credentials_rotate_v1 — F1a no-op
// =============================================================================

pub fn camera_credentials_rotate_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    if !check_permission(caller.data(), PERM_CAMERAS_WRITE, None) {
        audit(
            caller.data(),
            "camera.credentials_rotate",
            None,
            RiskClass::A,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let input: CameraCredentialsRotateInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::ServiceCall,
    ) {
        Ok(v) => v,
        Err(e) => {
            audit(
                caller.data(),
                "camera.credentials_rotate",
                None,
                RiskClass::A,
                "error",
                Some(if e == AbiError::PayloadTooLarge {
                    "payload_too_large"
                } else {
                    "invalid_payload"
                }),
            );
            return e.as_i32();
        }
    };
    if !camera_id_valid(&input.camera_id) {
        audit(
            caller.data(),
            "camera.credentials_rotate",
            None,
            RiskClass::A,
            "denied",
            Some("camera_id_invalid"),
        );
        return AbiError::Operation.as_i32();
    }
    let new_blob = match prepare_credentials_blob(input.new_credentials_b64.as_deref()) {
        Ok(v) => v,
        Err(reason) => {
            audit(
                caller.data(),
                "camera.credentials_rotate",
                Some(&input.camera_id),
                RiskClass::A,
                "denied",
                Some(reason),
            );
            return AbiError::Operation.as_i32();
        }
    };
    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();
    let row = match get_camera_for_addon(
        &db,
        &addon_id,
        &input.camera_id,
        caller.data().org_id.as_deref(),
    ) {
        Ok(Some(r)) => r,
        Ok(None) => {
            audit(
                caller.data(),
                "camera.credentials_rotate",
                Some(&input.camera_id),
                RiskClass::A,
                "denied",
                Some("not_found_or_not_owned"),
            );
            return AbiError::NotFound.as_i32();
        }
        Err(_) => {
            audit(
                caller.data(),
                "camera.credentials_rotate",
                Some(&input.camera_id),
                RiskClass::A,
                "error",
                Some("db_error"),
            );
            return AbiError::Operation.as_i32();
        }
    };
    // Only vendors that carry user-info credentials accept rotation. fake_file
    // is local filesystem playback (no auth); other unknown vendors are
    // rejected explicitly to avoid storing dead blobs against them.
    if row.vendor != "rtsp" && row.vendor != "onvif" {
        audit(
            caller.data(),
            "camera.credentials_rotate",
            Some(&input.camera_id),
            RiskClass::A,
            "denied",
            Some("vendor_has_no_credentials"),
        );
        let out = CameraCredentialsRotateOut {
            rotated: false,
            reason: format!("vendor '{}' has no credentials field", row.vendor),
        };
        return write_cbor_capped(
            &memory,
            &mut caller,
            &out,
            out_ptr,
            out_cap,
            out_len_ptr,
            PayloadKind::ServiceCall,
        );
    }
    let blob_ref = new_blob.as_deref();
    let blob_len = blob_ref.map(|b| b.len()).unwrap_or(0);

    // For vendor='onvif' the new credentials must be able to mint a fresh
    // RTSP URI via GetStreamUri (the original URL was the device-service
    // endpoint, not the stream itself). Re-derive BEFORE persisting the blob
    // so a bad rotation cannot leave the row pointing at a stream the new
    // password cannot reach.
    let (rotated_url, rotated_profile_token) = if row.vendor == "onvif" {
        let onvif_url = match row.onvif_url.as_deref() {
            Some(u) => u,
            None => {
                audit(
                    caller.data(),
                    "camera.credentials_rotate",
                    Some(&input.camera_id),
                    RiskClass::A,
                    "error",
                    Some("onvif_url_missing"),
                );
                return AbiError::Operation.as_i32();
            }
        };
        let blob = match blob_ref {
            Some(b) => b,
            None => {
                audit(
                    caller.data(),
                    "camera.credentials_rotate",
                    Some(&input.camera_id),
                    RiskClass::A,
                    "denied",
                    Some("credentials_required_for_onvif"),
                );
                return AbiError::Operation.as_i32();
            }
        };
        match resolve_onvif_one_click(onvif_url, blob, row.onvif_profile_token.as_deref()) {
            Ok(ok) => (ok.rtsp_uri, Some(ok.profile_token)),
            Err((abi, reason)) => {
                audit(
                    caller.data(),
                    "camera.credentials_rotate",
                    Some(&input.camera_id),
                    RiskClass::A,
                    "error",
                    Some(reason),
                );
                return abi.as_i32();
            }
        }
    } else {
        (row.url.clone(), row.onvif_profile_token.clone())
    };

    if set_camera_credentials_encrypted(
        &db,
        &addon_id,
        &input.camera_id,
        blob_ref,
        caller.data().org_id.as_deref(),
    )
    .is_err()
    {
        audit(
            caller.data(),
            "camera.credentials_rotate",
            Some(&input.camera_id),
            RiskClass::A,
            "error",
            Some("db_update_failed"),
        );
        return AbiError::Operation.as_i32();
    }

    // For ONVIF, update the derived URL + (possibly refreshed) profile token
    // alongside the credential rotation so the row stays self-consistent.
    if row.vendor == "onvif" {
        if let Err(_) = set_camera_onvif_resolved(
            &db,
            &addon_id,
            &input.camera_id,
            &rotated_url,
            rotated_profile_token.as_deref(),
            caller.data().org_id.as_deref(),
        ) {
            audit(
                caller.data(),
                "camera.credentials_rotate",
                Some(&input.camera_id),
                RiskClass::A,
                "error",
                Some("db_update_failed_onvif"),
            );
            return AbiError::Operation.as_i32();
        }
    }

    // Signal the live session to restart with the fresh credentials. The
    // session task otherwise keeps the previous plaintext in its in-memory
    // `CameraConfig` and would not pick up the rotation until its next
    // independent disconnect — which on a healthy RTSP feed never happens.
    // For ONVIF rows the supervisor still runs the session as `rtsp` against
    // the derived URI (matches camera_add_v1's session_vendor translation).
    let session_vendor = if row.vendor == "onvif" {
        "rtsp"
    } else {
        row.vendor.as_str()
    };
    let restart_cfg = CameraConfig {
        camera_id: row.camera_id.clone(),
        vendor: session_vendor.to_string(),
        url: rotated_url,
        target_fps: row.target_fps as u32,
        resolution: match (row.resolution_width, row.resolution_height) {
            (Some(w), Some(h)) => Some((w as u32, h as u32)),
            _ => None,
        },
        owner_addon_id: Some(addon_id.clone()),
        credentials_encrypted: new_blob.clone(),
        decoder_override: None,
    };
    let restart_result = run_async(async {
        let sup = get_or_init_supervisor().await?;
        sup.restart_camera(&row.camera_id, restart_cfg)
            .await
            .map_err(|e| map_ingest_error(&e))
    });
    let restart_note = match restart_result {
        Ok(()) => "session_restart_signaled",
        // A missing session (e.g. process restarted before the rotation but
        // host singleton not yet warmed) is non-fatal — the persisted blob
        // will be picked up when the supervisor reconciles. Surface it in
        // the audit reason so operators can correlate.
        Err(AbiError::NotFound) => "session_not_running",
        Err(_) => "session_restart_failed",
    };

    let reason = format!(
        "blob_len={blob_len} cleared={} {}",
        new_blob.is_none(),
        restart_note
    );
    audit(
        caller.data(),
        "camera.credentials_rotate",
        Some(&input.camera_id),
        RiskClass::A,
        "ok",
        Some(&reason),
    );
    let out = CameraCredentialsRotateOut {
        rotated: true,
        reason: if new_blob.is_some() {
            "credentials updated".to_string()
        } else {
            "credentials cleared".to_string()
        },
    };
    write_cbor_capped(
        &memory,
        &mut caller,
        &out,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::ServiceCall,
    )
}

// =============================================================================
// Test surface — drives the host functions through a stable, sync API for
// integration tests that do not spin up a wasmtime Store.
// =============================================================================

/// Pure-Rust core of `camera_add_v1` that operates on raw CBOR input bytes and
/// an explicit `AddonState`, with no wasmtime caller. Production code goes
/// through `camera_add_v1`; tests use this entry point to inject malformed
/// CBOR and oversized payloads without standing up an InstancePool.
pub(crate) fn camera_add_core(state: &AddonState, raw_input: &[u8]) -> i32 {
    if enforce_payload_size(raw_input.len(), PayloadKind::ServiceCall).is_err() {
        audit(
            state,
            "camera.add",
            None,
            RiskClass::A,
            "error",
            Some("payload_too_large"),
        );
        return AbiError::PayloadTooLarge.as_i32();
    }
    if !check_permission(state, PERM_CAMERAS_WRITE, None) {
        audit(
            state,
            "camera.add",
            None,
            RiskClass::A,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let mut input: CameraAddInput = match decode_cbor_exact(raw_input) {
        Ok(v) => v,
        Err(_) => {
            audit(
                state,
                "camera.add",
                None,
                RiskClass::A,
                "error",
                Some("invalid_payload"),
            );
            return AbiError::Operation.as_i32();
        }
    };
    // Resolve the legacy TOML defaults for the optional-on-wire fields so a
    // minimal payload behaves exactly like the old TOML path (30 / "C" /
    // "default").
    let target_fps = input.target_fps_or_default();
    let analysis_fps = input.analysis_fps_or_default();
    let retention_class = input.retention_class_or_default();
    let profile = input.profile_or_default();
    if !analysis_fps_valid(analysis_fps) {
        audit(
            state,
            "camera.add",
            None,
            RiskClass::A,
            "denied",
            Some("analysis_fps_out_of_range"),
        );
        return AbiError::Operation.as_i32();
    }
    if let Err(reason) = validate_vendor(&input.vendor) {
        let err = if reason == "unsupported_vendor" {
            AbiError::CameraVendorUnsupported
        } else {
            AbiError::Operation
        };
        audit(
            state,
            "camera.add",
            None,
            RiskClass::A,
            "denied",
            Some(reason),
        );
        return err.as_i32();
    }
    if let Err(reason) = validate_url(&input.url) {
        audit(
            state,
            "camera.add",
            None,
            RiskClass::A,
            "denied",
            Some(reason),
        );
        return AbiError::Operation.as_i32();
    }
    if !(1..=60).contains(&target_fps) {
        audit(
            state,
            "camera.add",
            None,
            RiskClass::A,
            "denied",
            Some("target_fps_out_of_range"),
        );
        return AbiError::Operation.as_i32();
    }
    if let Err(reason) = validate_retention(&retention_class) {
        audit(
            state,
            "camera.add",
            None,
            RiskClass::A,
            "denied",
            Some(reason),
        );
        return AbiError::Operation.as_i32();
    }
    if let Err(reason) = validate_display_name(&input.display_name) {
        audit(
            state,
            "camera.add",
            None,
            RiskClass::A,
            "denied",
            Some(reason),
        );
        return AbiError::Operation.as_i32();
    }
    if let Err(reason) = validate_profile(&profile) {
        audit(
            state,
            "camera.add",
            None,
            RiskClass::A,
            "denied",
            Some(reason),
        );
        return AbiError::Operation.as_i32();
    }
    let credentials_blob = match prepare_credentials_blob(input.credentials_b64.as_deref()) {
        Ok(v) => v,
        Err(reason) => {
            audit(
                state,
                "camera.add",
                None,
                RiskClass::A,
                "denied",
                Some(reason),
            );
            return AbiError::Operation.as_i32();
        }
    };

    // Mirrors the ONVIF resolve in `camera_add_v1`: replace the device-service
    // URL with the SOAP-derived RTSP URI before the supervisor session starts.
    let onvif_url_to_persist;
    let onvif_token_to_persist;
    if input.vendor == "onvif" {
        let Some(blob) = credentials_blob.as_deref() else {
            audit(
                state,
                "camera.add",
                None,
                RiskClass::A,
                "denied",
                Some("missing_credentials"),
            );
            return AbiError::Operation.as_i32();
        };
        if let Some(tok) = &input.onvif_profile_token {
            if !profile_valid(tok) {
                audit(
                    state,
                    "camera.add",
                    None,
                    RiskClass::A,
                    "denied",
                    Some("onvif_profile_token_invalid"),
                );
                return AbiError::Operation.as_i32();
            }
        }
        match resolve_onvif_one_click(&input.url, blob, input.onvif_profile_token.as_deref()) {
            Ok(ok) => {
                onvif_url_to_persist = Some(input.url.clone());
                onvif_token_to_persist = Some(ok.profile_token);
                input.url = ok.rtsp_uri;
            }
            Err((err, reason)) => {
                audit(
                    state,
                    "camera.add",
                    None,
                    RiskClass::A,
                    "error",
                    Some(reason),
                );
                return err.as_i32();
            }
        }
    } else {
        onvif_url_to_persist = None;
        onvif_token_to_persist = None;
    }

    let camera_id = format!("cam_{}", uuid::Uuid::new_v4());
    let addon_id = state.addon_id.clone();
    let db = state.db.clone();
    let org_id_for_insert = state.org_id.clone();

    let res_w = input.resolution_width.map(|v| v as i64);
    let res_h = input.resolution_height.map(|v| v as i64);

    let session_vendor = if input.vendor == "onvif" {
        "rtsp"
    } else {
        input.vendor.as_str()
    };
    let cfg = CameraConfig {
        camera_id: camera_id.clone(),
        vendor: session_vendor.to_string(),
        url: input.url.clone(),
        target_fps,
        resolution: match (input.resolution_width, input.resolution_height) {
            (Some(w), Some(h)) => Some((w, h)),
            _ => None,
        },
        owner_addon_id: Some(addon_id.clone()),
        credentials_encrypted: credentials_blob.clone(),
        decoder_override: None,
    };
    let sup = match run_async(get_or_init_supervisor()) {
        Ok(s) => s,
        Err(e) => {
            audit(
                state,
                "camera.add",
                Some(&camera_id),
                RiskClass::A,
                "error",
                Some("supervisor_init_failed"),
            );
            return e.as_i32();
        }
    };
    if let Err(e) = run_async(sup.add_camera(cfg)) {
        let mapped = map_ingest_error(&e);
        let reason = match &e {
            CameraIngestError::QuotaExceeded(_) => "quota_exceeded".to_string(),
            other => format!("session_start_failed: {other}"),
        };
        audit(
            state,
            "camera.add",
            Some(&camera_id),
            RiskClass::A,
            "error",
            Some(&reason),
        );
        return mapped.as_i32();
    }

    if let Err(e) = insert_camera(
        &db,
        &camera_id,
        &addon_id,
        &input.display_name,
        &input.vendor,
        &input.url,
        target_fps as i64,
        analysis_fps as i64,
        res_w,
        res_h,
        &retention_class,
        &profile,
        credentials_blob.as_deref(),
        onvif_url_to_persist.as_deref(),
        onvif_token_to_persist.as_deref(),
        org_id_for_insert.as_deref(),
    ) {
        warn!("camera.add insert_camera failed (compensating remove_camera): {e}");
        let _ = run_async(sup.remove_camera(&camera_id));
        audit(
            state,
            "camera.add",
            Some(&camera_id),
            RiskClass::A,
            "error",
            Some("db_insert_failed"),
        );
        return AbiError::Operation.as_i32();
    }

    audit(
        state,
        "camera.add",
        Some(&camera_id),
        RiskClass::A,
        "ok",
        None,
    );
    AbiError::Ok.as_i32()
}

#[doc(hidden)]
pub mod test_api {
    use super::*;

    #[doc(hidden)]
    pub async fn supervisor_for_tests() -> Result<Arc<CameraIngestSupervisor>, AbiError> {
        get_or_init_supervisor().await
    }

    /// Direct entry point that skips the wasmtime caller so tests can
    /// inject malformed CBOR, oversized payloads, and exercise the quota
    /// path with full audit-log coverage. Returns the ABI return code that
    /// `camera_add_v1` would have produced.
    #[doc(hidden)]
    pub fn camera_add_with_raw_input(state: &AddonState, raw_input: &[u8]) -> i32 {
        super::camera_add_core(state, raw_input)
    }

    /// Drains every session on the shared supervisor. Tests that mutate the
    /// supervisor should call this at teardown (or via a `Drop` guard) to
    /// keep singleton state from leaking between tests. Idempotent.
    #[doc(hidden)]
    pub async fn reset_supervisor_for_test() {
        if let Some(sup) = SUPERVISOR.get() {
            sup.drain().await;
        }
    }

    #[doc(hidden)]
    pub fn camera_id_valid_for_test(s: &str) -> bool {
        super::camera_id_valid(s)
    }

    #[doc(hidden)]
    pub fn display_name_valid_for_test(s: &str) -> bool {
        super::display_name_valid(s)
    }

    #[doc(hidden)]
    pub fn profile_valid_for_test(s: &str) -> bool {
        super::profile_valid(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_onvif_test_connection_forces_onvif_path() {
        // Bare host → forced to /onvif/device_service.
        let u = force_onvif_path("http://10.0.0.1/").unwrap();
        assert_eq!(u.path(), "/onvif/device_service");

        // Already an ONVIF sub-service path → preserved.
        let u = force_onvif_path("http://10.0.0.1/onvif/media_service").unwrap();
        assert_eq!(u.path(), "/onvif/media_service");

        // Arbitrary non-ONVIF path → rewritten (SSRF defense).
        let u = force_onvif_path("http://10.0.0.1/api/admin").unwrap();
        assert_eq!(u.path(), "/onvif/device_service");

        // Non-http(s) scheme rejected.
        assert!(force_onvif_path("file:///etc/passwd").is_err());
    }
}
