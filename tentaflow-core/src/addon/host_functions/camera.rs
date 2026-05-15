// ============ File: camera.rs — Camera ingest host functions (M1.W6 F1a TentaVision) ============
//
// Implements the 10 host functions that bridge addon-side WASM calls to the
// `services::camera_ingest::CameraIngestSupervisor`. Each call:
//   1. enforces input payload size BEFORE materializing a String,
//   2. parses TOML, validates ownership / vendor / lengths / format,
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

use base64::Engine;
use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use tracing::warn;

use super::abi_helpers::{enforce_payload_size, write_output_with_retry_semantics, PayloadKind};
use super::{
    audit_log_with_risk, check_permission, get_memory, read_guest_bytes, AddonState, WasmCaller,
};
use crate::addon::errors::AbiError;
use crate::audit::RiskClass;
use crate::db::repository::{
    get_camera_for_addon, insert_camera, list_cameras_for_addon, soft_delete_camera,
    update_camera, CameraPatch, CameraRow,
};
use crate::services::camera_ingest::{
    start_supervisor, CameraConfig, CameraIngestError, CameraIngestSupervisor,
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

const SUPPORTED_VENDORS: &[&str] = &["fake_file"];

fn vendor_supported(v: &str) -> bool {
    SUPPORTED_VENDORS.iter().any(|s| *s == v)
}

fn retention_class_valid(rc: &str) -> bool {
    matches!(rc, "A" | "B" | "C" | "Unclassified")
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
            || matches!(c, '-' | '_' | '.' | ',' | '(' | ')' | ':' | '\'' | '"' | '!' | '?')
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

async fn get_or_init_supervisor() -> Result<Arc<CameraIngestSupervisor>, AbiError> {
    SUPERVISOR
        .get_or_try_init(|| async {
            start_supervisor().await.map(Arc::new).map_err(|e| {
                warn!("camera_ingest supervisor init failed: {e}");
                AbiError::Operation
            })
        })
        .await
        .cloned()
}

/// Drains every camera session on the process-wide supervisor without
/// consuming the singleton. Safe to call multiple times: subsequent calls
/// drain an already-empty registry. Wired into the main binary's shutdown
/// path (see `tentaflow/src/main.rs`) so GStreamer pipelines stop before
/// the router begins releasing locks.
pub async fn shutdown_camera_supervisor_global() {
    if let Some(sup) = SUPERVISOR.get() {
        sup.drain().await;
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
    }
}

// =============================================================================
// Payload structs — input
// =============================================================================

#[derive(Debug, Deserialize)]
struct CameraAddInput {
    display_name: String,
    vendor: String,
    url: String,
    #[serde(default = "default_target_fps")]
    target_fps: u32,
    resolution_width: Option<u32>,
    resolution_height: Option<u32>,
    #[serde(default = "default_retention_class")]
    retention_class: String,
    #[serde(default = "default_profile")]
    profile: String,
}

fn default_target_fps() -> u32 {
    30
}
fn default_retention_class() -> String {
    "C".to_string()
}
fn default_profile() -> String {
    "default".to_string()
}

#[derive(Debug, Deserialize)]
struct CameraIdInput {
    camera_id: String,
}

#[derive(Debug, Deserialize)]
struct CameraUpdateInput {
    camera_id: String,
    display_name: Option<String>,
    target_fps: Option<u32>,
    resolution_width: Option<u32>,
    resolution_height: Option<u32>,
    retention_class: Option<String>,
    profile: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CameraTestConnectionInput {
    vendor: String,
    url: String,
}

#[derive(Debug, Deserialize)]
struct CameraCredentialsRotateInput {
    camera_id: String,
    #[allow(dead_code)]
    #[serde(default)]
    new_credentials_b64: Option<String>,
}

// =============================================================================
// Payload structs — output
// =============================================================================

#[derive(Debug, Serialize)]
struct CameraAddOutput {
    camera_id: String,
    status: String,
}

#[derive(Debug, Serialize)]
struct CameraInfoOut {
    camera_id: String,
    display_name: String,
    vendor: String,
    url: String,
    target_fps: i64,
    resolution_width: Option<i64>,
    resolution_height: Option<i64>,
    status: String,
    status_message: Option<String>,
    fps_actual: Option<f64>,
    last_frame_at: Option<i64>,
    retention_class: String,
    profile: String,
}

#[derive(Debug, Serialize)]
struct CameraListOut {
    camera: Vec<CameraInfoOut>,
}

#[derive(Debug, Serialize)]
struct CameraSnapshotOut {
    camera_id: String,
    width: u32,
    height: u32,
    pixel_format: String,
    timestamp_unix_ms: u64,
    data_b64: String,
}

#[derive(Debug, Serialize)]
struct CameraHealthOut {
    camera_id: String,
    status: String,
    status_message: String,
    fps_actual: f64,
    last_frame_at: i64,
    frames_total: u64,
    frames_dropped: u64,
}

#[derive(Debug, Serialize)]
struct CameraRemoveOut {
    removed: bool,
}

#[derive(Debug, Serialize)]
struct CameraDiscoverOut {
    discovered: Vec<CameraInfoOut>,
}

#[derive(Debug, Serialize)]
struct CameraTestConnectionOut {
    ok: bool,
    message: String,
}

#[derive(Debug, Serialize)]
struct CameraCredentialsRotateOut {
    rotated: bool,
    reason: String,
}

// =============================================================================
// Helpers — encoding + status mapping + audit + io
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

async fn build_camera_info(
    sup: &CameraIngestSupervisor,
    row: CameraRow,
) -> CameraInfoOut {
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
    }
}

/// Reads a TOML input from guest memory while enforcing the payload size
/// limit BEFORE materializing a `String` on the host heap. Prevents an
/// adversarial addon from forcing GB allocations with `input_len = i32::MAX`.
fn read_input_toml(
    memory: &super::super::runtime::WasmMemory,
    caller: &WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
) -> Result<String, AbiError> {
    if input_len < 0 {
        return Err(AbiError::Operation);
    }
    if enforce_payload_size(input_len as usize, PayloadKind::ServiceCall).is_err() {
        return Err(AbiError::PayloadTooLarge);
    }
    let bytes = read_guest_bytes(memory, caller, input_ptr, input_len)
        .ok_or(AbiError::Operation)?;
    std::str::from_utf8(bytes)
        .map(|s| s.to_string())
        .map_err(|_| AbiError::Operation)
}

/// Serializes `value` to TOML and writes through the retry helper, but only
/// after re-checking the absolute PayloadKind::ServiceCall ceiling. Without
/// the absolute check a buggy or malicious state could blow past the 8 MiB
/// limit (e.g. very large blob lists from a future schema).
fn write_toml_capped<T: Serialize>(
    memory: &super::super::runtime::WasmMemory,
    caller: &mut WasmCaller<'_, AddonState>,
    value: &T,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let serialized = match toml::to_string(value) {
        Ok(s) => s,
        Err(_) => return AbiError::Operation.as_i32(),
    };
    if enforce_payload_size(serialized.len(), PayloadKind::ServiceCall).is_err() {
        return AbiError::PayloadTooLarge.as_i32();
    }
    write_output_with_retry_semantics(
        memory,
        caller,
        serialized.as_bytes(),
        out_ptr,
        out_cap,
        out_len_ptr,
    )
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
    if !vendor_supported(v) {
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
    let raw = match read_input_toml(&memory, &caller, input_ptr, input_len) {
        Ok(s) => s,
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
                    "input_read_failed"
                }),
            );
            return e.as_i32();
        }
    };
    if !check_permission(caller.data(), PERM_CAMERAS_WRITE, None) {
        audit(caller.data(), "camera.add", None, RiskClass::A, "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }
    let input: CameraAddInput = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            audit(caller.data(), "camera.add", None, RiskClass::A, "error", Some("invalid_toml"));
            return AbiError::Operation.as_i32();
        }
    };
    if let Err(reason) = validate_vendor(&input.vendor) {
        let err = if reason == "unsupported_vendor" {
            AbiError::CameraVendorUnsupported
        } else {
            AbiError::Operation
        };
        audit(caller.data(), "camera.add", None, RiskClass::A, "denied", Some(reason));
        return err.as_i32();
    }
    if let Err(reason) = validate_url(&input.url) {
        audit(caller.data(), "camera.add", None, RiskClass::A, "denied", Some(reason));
        return AbiError::Operation.as_i32();
    }
    if !(1..=60).contains(&input.target_fps) {
        audit(caller.data(), "camera.add", None, RiskClass::A, "denied", Some("target_fps_out_of_range"));
        return AbiError::Operation.as_i32();
    }
    if let Err(reason) = validate_retention(&input.retention_class) {
        audit(caller.data(), "camera.add", None, RiskClass::A, "denied", Some(reason));
        return AbiError::Operation.as_i32();
    }
    if let Err(reason) = validate_display_name(&input.display_name) {
        audit(caller.data(), "camera.add", None, RiskClass::A, "denied", Some(reason));
        return AbiError::Operation.as_i32();
    }
    if let Err(reason) = validate_profile(&input.profile) {
        audit(caller.data(), "camera.add", None, RiskClass::A, "denied", Some(reason));
        return AbiError::Operation.as_i32();
    }

    let camera_id = format!("cam_{}", uuid::Uuid::new_v4());
    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();

    let res_w = input.resolution_width.map(|v| v as i64);
    let res_h = input.resolution_height.map(|v| v as i64);

    // Supervisor session first — if the pipeline fails we never write a row
    // and so never need a compensating delete. If the host crashes between
    // supervisor start and DB insert the in-memory registry dies with the
    // process; reconciliation at lazy-init drives the steady-state.
    let cfg = CameraConfig {
        camera_id: camera_id.clone(),
        vendor: input.vendor.clone(),
        url: input.url.clone(),
        target_fps: input.target_fps,
        resolution: match (input.resolution_width, input.resolution_height) {
            (Some(w), Some(h)) => Some((w, h)),
            _ => None,
        },
    };
    let sup = match run_async(get_or_init_supervisor()) {
        Ok(s) => s,
        Err(e) => {
            audit(caller.data(), "camera.add", Some(&camera_id), RiskClass::A, "error", Some("supervisor_init_failed"));
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
        input.target_fps as i64,
        res_w,
        res_h,
        &input.retention_class,
        &input.profile,
    ) {
        warn!("camera.add insert_camera failed (compensating remove_camera): {e}");
        // Compensate the started session so the registry stays consistent.
        let _ = run_async(sup.remove_camera(&camera_id));
        audit(caller.data(), "camera.add", Some(&camera_id), RiskClass::A, "error", Some("db_insert_failed"));
        return AbiError::Operation.as_i32();
    }

    audit(caller.data(), "camera.add", Some(&camera_id), RiskClass::A, "ok", None);
    let out = CameraAddOutput {
        camera_id: camera_id.clone(),
        status: "starting".to_string(),
    };
    write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
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
        audit(caller.data(), "camera.list", None, RiskClass::B, "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }
    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();
    let rows = match list_cameras_for_addon(&db, &addon_id) {
        Ok(v) => v,
        Err(_) => {
            audit(caller.data(), "camera.list", None, RiskClass::B, "error", Some("db_error"));
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
            audit(caller.data(), "camera.list", None, RiskClass::B, "error", Some("supervisor_unavailable"));
            return e.as_i32();
        }
    };
    audit(caller.data(), "camera.list", None, RiskClass::B, "ok", None);
    write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
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
    let raw = match read_input_toml(&memory, &caller, input_ptr, input_len) {
        Ok(s) => s,
        Err(e) => {
            audit(caller.data(), "camera.get", None, RiskClass::B, "error",
                Some(if e == AbiError::PayloadTooLarge { "payload_too_large" } else { "input_read_failed" }));
            return e.as_i32();
        }
    };
    if !check_permission(caller.data(), PERM_CAMERAS_READ, None) {
        audit(caller.data(), "camera.get", None, RiskClass::B, "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }
    let input: CameraIdInput = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            audit(caller.data(), "camera.get", None, RiskClass::B, "error", Some("invalid_toml"));
            return AbiError::Operation.as_i32();
        }
    };
    if !camera_id_valid(&input.camera_id) {
        audit(caller.data(), "camera.get", None, RiskClass::B, "denied", Some("camera_id_invalid"));
        return AbiError::Operation.as_i32();
    }
    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();
    let row = match get_camera_for_addon(&db, &addon_id, &input.camera_id) {
        Ok(Some(r)) => r,
        Ok(None) => {
            audit(caller.data(), "camera.get", Some(&input.camera_id), RiskClass::B, "denied", Some("not_found_or_not_owned"));
            return AbiError::NotFound.as_i32();
        }
        Err(_) => {
            audit(caller.data(), "camera.get", Some(&input.camera_id), RiskClass::B, "error", Some("db_error"));
            return AbiError::Operation.as_i32();
        }
    };
    let info = run_async(async {
        match get_or_init_supervisor().await {
            Ok(sup) => Some(build_camera_info(&sup, row.clone()).await),
            Err(_) => None,
        }
    });
    let info = match info {
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
        },
    };
    audit(caller.data(), "camera.get", Some(&info.camera_id), RiskClass::B, "ok", None);
    write_toml_capped(&memory, &mut caller, &info, out_ptr, out_cap, out_len_ptr)
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
    let raw = match read_input_toml(&memory, &caller, input_ptr, input_len) {
        Ok(s) => s,
        Err(e) => {
            audit(caller.data(), "camera.update", None, RiskClass::A, "error",
                Some(if e == AbiError::PayloadTooLarge { "payload_too_large" } else { "input_read_failed" }));
            return e.as_i32();
        }
    };
    if !check_permission(caller.data(), PERM_CAMERAS_WRITE, None) {
        audit(caller.data(), "camera.update", None, RiskClass::A, "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }
    let input: CameraUpdateInput = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            audit(caller.data(), "camera.update", None, RiskClass::A, "error", Some("invalid_toml"));
            return AbiError::Operation.as_i32();
        }
    };
    if !camera_id_valid(&input.camera_id) {
        audit(caller.data(), "camera.update", None, RiskClass::A, "denied", Some("camera_id_invalid"));
        return AbiError::Operation.as_i32();
    }

    if let Some(fps) = input.target_fps {
        if !(1..=60).contains(&fps) {
            audit(caller.data(), "camera.update", Some(&input.camera_id), RiskClass::A, "denied", Some("target_fps_out_of_range"));
            return AbiError::Operation.as_i32();
        }
    }
    if let Some(rc) = input.retention_class.as_ref() {
        if let Err(reason) = validate_retention(rc) {
            audit(caller.data(), "camera.update", Some(&input.camera_id), RiskClass::A, "denied", Some(reason));
            return AbiError::Operation.as_i32();
        }
    }
    if let Some(n) = input.display_name.as_ref() {
        if let Err(reason) = validate_display_name(n) {
            audit(caller.data(), "camera.update", Some(&input.camera_id), RiskClass::A, "denied", Some(reason));
            return AbiError::Operation.as_i32();
        }
    }
    if let Some(p) = input.profile.as_ref() {
        if let Err(reason) = validate_profile(p) {
            audit(caller.data(), "camera.update", Some(&input.camera_id), RiskClass::A, "denied", Some(reason));
            return AbiError::Operation.as_i32();
        }
    }

    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();

    match get_camera_for_addon(&db, &addon_id, &input.camera_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            audit(caller.data(), "camera.update", Some(&input.camera_id), RiskClass::A, "denied", Some("not_found_or_not_owned"));
            return AbiError::NotFound.as_i32();
        }
        Err(_) => {
            audit(caller.data(), "camera.update", Some(&input.camera_id), RiskClass::A, "error", Some("db_error"));
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
    let patch = CameraPatch {
        display_name: input.display_name.clone(),
        target_fps: input.target_fps.map(|v| v as i64),
        resolution_width: input.resolution_width.map(|v| Some(v as i64)),
        resolution_height: input.resolution_height.map(|v| Some(v as i64)),
        retention_class: input.retention_class.clone(),
        profile: input.profile.clone(),
    };

    if update_camera(&db, &addon_id, &input.camera_id, &patch).is_err() {
        audit(caller.data(), "camera.update", Some(&input.camera_id), RiskClass::A, "error", Some("db_update_failed"));
        return AbiError::Operation.as_i32();
    }

    let row = match get_camera_for_addon(&db, &addon_id, &input.camera_id) {
        Ok(Some(r)) => r,
        Ok(None) => {
            audit(caller.data(), "camera.update", Some(&input.camera_id), RiskClass::A, "error", Some("row_disappeared_after_update"));
            return AbiError::Operation.as_i32();
        }
        Err(_) => {
            audit(caller.data(), "camera.update", Some(&input.camera_id), RiskClass::A, "error", Some("db_error_after_update"));
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
            }
        }
    });

    let reason = format!("fields={}", diff.join(","));
    audit(caller.data(), "camera.update", Some(&info.camera_id), RiskClass::A, "ok", Some(&reason));
    write_toml_capped(&memory, &mut caller, &info, out_ptr, out_cap, out_len_ptr)
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
    let raw = match read_input_toml(&memory, &caller, input_ptr, input_len) {
        Ok(s) => s,
        Err(e) => {
            audit(caller.data(), "camera.remove", None, RiskClass::A, "error",
                Some(if e == AbiError::PayloadTooLarge { "payload_too_large" } else { "input_read_failed" }));
            return e.as_i32();
        }
    };
    if !check_permission(caller.data(), PERM_CAMERAS_WRITE, None) {
        audit(caller.data(), "camera.remove", None, RiskClass::A, "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }
    let input: CameraIdInput = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            audit(caller.data(), "camera.remove", None, RiskClass::A, "error", Some("invalid_toml"));
            return AbiError::Operation.as_i32();
        }
    };
    if !camera_id_valid(&input.camera_id) {
        audit(caller.data(), "camera.remove", None, RiskClass::A, "denied", Some("camera_id_invalid"));
        return AbiError::Operation.as_i32();
    }
    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();

    match get_camera_for_addon(&db, &addon_id, &input.camera_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            audit(caller.data(), "camera.remove", Some(&input.camera_id), RiskClass::A, "denied", Some("not_found_or_not_owned"));
            return AbiError::NotFound.as_i32();
        }
        Err(_) => {
            audit(caller.data(), "camera.remove", Some(&input.camera_id), RiskClass::A, "error", Some("db_error"));
            return AbiError::Operation.as_i32();
        }
    }

    // DB soft-delete first — once committed the row is hidden from `list`,
    // so even if the supervisor remove fails (timeout, NotFound) the camera
    // is effectively gone from the addon's perspective. A leftover in-memory
    // session is bounded by process lifetime and falls off at next restart
    // because reconciliation skips `removed_at IS NOT NULL` rows.
    match soft_delete_camera(&db, &addon_id, &input.camera_id) {
        Ok(true) => {}
        Ok(false) => {
            audit(caller.data(), "camera.remove", Some(&input.camera_id), RiskClass::A, "denied", Some("not_found"));
            return AbiError::NotFound.as_i32();
        }
        Err(_) => {
            audit(caller.data(), "camera.remove", Some(&input.camera_id), RiskClass::A, "error", Some("db_error"));
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

    audit(caller.data(), "camera.remove", Some(&input.camera_id), RiskClass::A, "ok", None);
    let out = CameraRemoveOut { removed: true };
    write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
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
    let raw = match read_input_toml(&memory, &caller, input_ptr, input_len) {
        Ok(s) => s,
        Err(e) => {
            audit(caller.data(), "camera.snapshot", None, RiskClass::A, "error",
                Some(if e == AbiError::PayloadTooLarge { "payload_too_large" } else { "input_read_failed" }));
            return e.as_i32();
        }
    };
    if !check_permission(caller.data(), PERM_CAMERAS_SNAPSHOT, None) {
        audit(caller.data(), "camera.snapshot", None, RiskClass::A, "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }
    let input: CameraIdInput = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            audit(caller.data(), "camera.snapshot", None, RiskClass::A, "error", Some("invalid_toml"));
            return AbiError::Operation.as_i32();
        }
    };
    if !camera_id_valid(&input.camera_id) {
        audit(caller.data(), "camera.snapshot", None, RiskClass::A, "denied", Some("camera_id_invalid"));
        return AbiError::Operation.as_i32();
    }
    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();
    match get_camera_for_addon(&db, &addon_id, &input.camera_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            audit(caller.data(), "camera.snapshot", Some(&input.camera_id), RiskClass::A, "denied", Some("not_found_or_not_owned"));
            return AbiError::NotFound.as_i32();
        }
        Err(_) => {
            audit(caller.data(), "camera.snapshot", Some(&input.camera_id), RiskClass::A, "error", Some("db_error"));
            return AbiError::Operation.as_i32();
        }
    }

    let snap = run_async(async {
        let sup = get_or_init_supervisor().await?;
        sup.snapshot(&input.camera_id).await.map_err(|e| map_ingest_error(&e))
    });
    let snap = match snap {
        Ok(v) => v,
        Err(e) => {
            audit(caller.data(), "camera.snapshot", Some(&input.camera_id), RiskClass::A, "error", Some(&format!("abi_error={}", e.as_i32())));
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
        Some(&format!("w={} h={} bytes={}", out.width, out.height, bytes_size)),
    );
    write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
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
    let raw = match read_input_toml(&memory, &caller, input_ptr, input_len) {
        Ok(s) => s,
        Err(e) => {
            audit(caller.data(), "camera.health", None, RiskClass::B, "error",
                Some(if e == AbiError::PayloadTooLarge { "payload_too_large" } else { "input_read_failed" }));
            return e.as_i32();
        }
    };
    if !check_permission(caller.data(), PERM_CAMERAS_READ, None) {
        audit(caller.data(), "camera.health", None, RiskClass::B, "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }
    let input: CameraIdInput = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            audit(caller.data(), "camera.health", None, RiskClass::B, "error", Some("invalid_toml"));
            return AbiError::Operation.as_i32();
        }
    };
    if !camera_id_valid(&input.camera_id) {
        audit(caller.data(), "camera.health", None, RiskClass::B, "denied", Some("camera_id_invalid"));
        return AbiError::Operation.as_i32();
    }
    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();
    let row = match get_camera_for_addon(&db, &addon_id, &input.camera_id) {
        Ok(Some(r)) => r,
        Ok(None) => {
            audit(caller.data(), "camera.health", Some(&input.camera_id), RiskClass::B, "denied", Some("not_found_or_not_owned"));
            return AbiError::NotFound.as_i32();
        }
        Err(_) => {
            audit(caller.data(), "camera.health", Some(&input.camera_id), RiskClass::B, "error", Some("db_error"));
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
    audit(caller.data(), "camera.health", Some(&out.camera_id), RiskClass::B, "ok", None);
    write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
}

// =============================================================================
// Host function: camera_discover_v1 — F1a no-op (enumeration only → Risk B)
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
        audit(caller.data(), "camera.discover", None, RiskClass::B, "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }
    audit(caller.data(), "camera.discover", None, RiskClass::B, "ok", Some("f1a_empty"));
    let out = CameraDiscoverOut { discovered: Vec::new() };
    write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
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
    let raw = match read_input_toml(&memory, &caller, input_ptr, input_len) {
        Ok(s) => s,
        Err(e) => {
            audit(caller.data(), "camera.test_connection", None, RiskClass::A, "error",
                Some(if e == AbiError::PayloadTooLarge { "payload_too_large" } else { "input_read_failed" }));
            return e.as_i32();
        }
    };
    if !check_permission(caller.data(), PERM_CAMERAS_WRITE, None) {
        audit(caller.data(), "camera.test_connection", None, RiskClass::A, "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }
    let input: CameraTestConnectionInput = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            audit(caller.data(), "camera.test_connection", None, RiskClass::A, "error", Some("invalid_toml"));
            return AbiError::Operation.as_i32();
        }
    };
    if input.vendor.is_empty() || input.vendor.len() > MAX_VENDOR {
        audit(caller.data(), "camera.test_connection", None, RiskClass::A, "denied", Some("vendor_length"));
        return AbiError::Operation.as_i32();
    }
    if let Err(reason) = validate_url(&input.url) {
        audit(caller.data(), "camera.test_connection", None, RiskClass::A, "denied", Some(reason));
        return AbiError::Operation.as_i32();
    }
    if !vendor_supported(&input.vendor) {
        audit(caller.data(), "camera.test_connection", None, RiskClass::A, "ok", Some("unsupported_vendor"));
        let out = CameraTestConnectionOut {
            ok: false,
            message: format!("vendor '{}' not supported (F1a: fake_file only)", input.vendor),
        };
        return write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr);
    }
    let out = match crate::services::camera_ingest::fakefile::resolve_file_url(&input.url) {
        Ok(_) => CameraTestConnectionOut {
            ok: true,
            message: "fake_file path readable".to_string(),
        },
        Err(e) => CameraTestConnectionOut {
            ok: false,
            message: e.to_string(),
        },
    };
    audit(caller.data(), "camera.test_connection", None, RiskClass::A, "ok", Some(&format!("ok={}", out.ok)));
    write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
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
    let raw = match read_input_toml(&memory, &caller, input_ptr, input_len) {
        Ok(s) => s,
        Err(e) => {
            audit(caller.data(), "camera.credentials_rotate", None, RiskClass::A, "error",
                Some(if e == AbiError::PayloadTooLarge { "payload_too_large" } else { "input_read_failed" }));
            return e.as_i32();
        }
    };
    if !check_permission(caller.data(), PERM_CAMERAS_WRITE, None) {
        audit(caller.data(), "camera.credentials_rotate", None, RiskClass::A, "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }
    let input: CameraCredentialsRotateInput = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            audit(caller.data(), "camera.credentials_rotate", None, RiskClass::A, "error", Some("invalid_toml"));
            return AbiError::Operation.as_i32();
        }
    };
    if !camera_id_valid(&input.camera_id) {
        audit(caller.data(), "camera.credentials_rotate", None, RiskClass::A, "denied", Some("camera_id_invalid"));
        return AbiError::Operation.as_i32();
    }
    if let Some(c) = input.new_credentials_b64.as_ref() {
        if c.len() > MAX_CREDENTIALS_B64 {
            audit(caller.data(), "camera.credentials_rotate", Some(&input.camera_id), RiskClass::A, "denied", Some("credentials_b64_too_long"));
            return AbiError::Operation.as_i32();
        }
    }
    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();
    match get_camera_for_addon(&db, &addon_id, &input.camera_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            audit(caller.data(), "camera.credentials_rotate", Some(&input.camera_id), RiskClass::A, "denied", Some("not_found_or_not_owned"));
            return AbiError::NotFound.as_i32();
        }
        Err(_) => {
            audit(caller.data(), "camera.credentials_rotate", Some(&input.camera_id), RiskClass::A, "error", Some("db_error"));
            return AbiError::Operation.as_i32();
        }
    }
    audit(caller.data(), "camera.credentials_rotate", Some(&input.camera_id), RiskClass::A, "ok", Some("f1a_noop"));
    let out = CameraCredentialsRotateOut {
        rotated: false,
        reason: "f1a_noop_fake_file_has_no_credentials".to_string(),
    };
    write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
}

// =============================================================================
// Test surface — drives the host functions through a stable, sync API for
// integration tests that do not spin up a wasmtime Store.
// =============================================================================

#[doc(hidden)]
pub mod test_api {
    use super::*;

    #[doc(hidden)]
    pub async fn supervisor_for_tests() -> Result<Arc<CameraIngestSupervisor>, AbiError> {
        get_or_init_supervisor().await
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
