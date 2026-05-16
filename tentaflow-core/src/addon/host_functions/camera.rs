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
    get_camera_for_addon, insert_camera, list_cameras_for_addon,
    set_camera_credentials_encrypted, soft_delete_camera, update_camera, CameraPatch, CameraRow,
};
use crate::services::camera_ingest::{
    credentials::credentials_cipher, start_supervisor, CameraConfig, CameraIngestError,
    CameraIngestSupervisor,
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

const SUPPORTED_VENDORS: &[&str] = &["fake_file", "rtsp"];

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
        QuotaExceeded(_) => AbiError::QuotaExceeded,
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
    /// Optional base64-encoded `user:pass` for the RTSP connector. When
    /// present, decoded, validated, encrypted with the cameras master key,
    /// and stored in `cameras.credentials_encrypted`. The plaintext never
    /// touches the DB and is never logged.
    #[serde(default)]
    credentials_b64: Option<String>,
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
    /// Base64-encoded new `user:pass` string. `None` clears the field,
    /// turning the camera into an open-stream endpoint (URL must then
    /// already work without auth).
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

/// Decode and encrypt an optional `credentials_b64` field. Returns the
/// AES-GCM blob ready for storage, or a static error tag describing why the
/// input was rejected. The decoded plaintext is wiped from the temporary
/// `String` by going out of scope; it is never logged or returned in errors.
fn prepare_credentials_blob(
    b64: Option<&str>,
) -> Result<Option<Vec<u8>>, &'static str> {
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
                '-' | '.' | '_' | '~' | '!' | '$' | '&' | '\'' | '(' | ')' | '*' | '+' | ',' | ';' | '='
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
    let credentials_blob = match prepare_credentials_blob(input.credentials_b64.as_deref()) {
        Ok(v) => v,
        Err(reason) => {
            audit(caller.data(), "camera.add", None, RiskClass::A, "denied", Some(reason));
            return AbiError::Operation.as_i32();
        }
    };

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
        owner_addon_id: Some(addon_id.clone()),
        credentials_encrypted: credentials_blob.clone(),
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
        credentials_blob.as_deref(),
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
            message: format!("vendor '{}' not supported", input.vendor),
        };
        return write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr);
    }
    let out = match input.vendor.as_str() {
        "fake_file" => match crate::services::camera_ingest::fakefile::resolve_file_url(&input.url) {
            Ok(_) => CameraTestConnectionOut {
                ok: true,
                message: "fake_file path readable".to_string(),
            },
            Err(e) => CameraTestConnectionOut {
                ok: false,
                message: e.to_string(),
            },
        },
        "rtsp" => match crate::services::camera_ingest::rtsp::validate_rtsp_url(&input.url) {
            // Surface-level URL validation only — a real RTSP DESCRIBE probe
            // is intentionally out of scope here. Live connectivity is
            // verified by the supervisor once the camera is added.
            Ok(_) => CameraTestConnectionOut {
                ok: true,
                message: "rtsp url well-formed".to_string(),
            },
            Err(e) => CameraTestConnectionOut {
                ok: false,
                message: e.to_string(),
            },
        },
        other => CameraTestConnectionOut {
            ok: false,
            message: format!("vendor '{other}' has no test_connection handler"),
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
    let new_blob = match prepare_credentials_blob(input.new_credentials_b64.as_deref()) {
        Ok(v) => v,
        Err(reason) => {
            audit(caller.data(), "camera.credentials_rotate", Some(&input.camera_id), RiskClass::A, "denied", Some(reason));
            return AbiError::Operation.as_i32();
        }
    };
    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();
    let row = match get_camera_for_addon(&db, &addon_id, &input.camera_id) {
        Ok(Some(r)) => r,
        Ok(None) => {
            audit(caller.data(), "camera.credentials_rotate", Some(&input.camera_id), RiskClass::A, "denied", Some("not_found_or_not_owned"));
            return AbiError::NotFound.as_i32();
        }
        Err(_) => {
            audit(caller.data(), "camera.credentials_rotate", Some(&input.camera_id), RiskClass::A, "error", Some("db_error"));
            return AbiError::Operation.as_i32();
        }
    };
    // Only RTSP carries user-info credentials; fake_file is local filesystem
    // playback and has no auth, so a rotation request makes no sense and is
    // rejected explicitly to avoid storing dead blobs against it.
    if row.vendor != "rtsp" {
        audit(caller.data(), "camera.credentials_rotate", Some(&input.camera_id), RiskClass::A, "denied", Some("vendor_has_no_credentials"));
        let out = CameraCredentialsRotateOut {
            rotated: false,
            reason: format!("vendor '{}' has no credentials field", row.vendor),
        };
        return write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr);
    }
    let blob_ref = new_blob.as_deref();
    let blob_len = blob_ref.map(|b| b.len()).unwrap_or(0);
    if set_camera_credentials_encrypted(&db, &addon_id, &input.camera_id, blob_ref)
        .is_err()
    {
        audit(caller.data(), "camera.credentials_rotate", Some(&input.camera_id), RiskClass::A, "error", Some("db_update_failed"));
        return AbiError::Operation.as_i32();
    }

    // Signal the live session to restart with the fresh credentials. The
    // session task otherwise keeps the previous plaintext in its in-memory
    // `CameraConfig` and would not pick up the rotation until its next
    // independent disconnect — which on a healthy RTSP feed never happens.
    // We build a CameraConfig from the persisted row so the restart sees
    // exactly what `camera_add_v1` would have configured today (vendor +
    // url + fps + resolution + new blob).
    let restart_cfg = CameraConfig {
        camera_id: row.camera_id.clone(),
        vendor: row.vendor.clone(),
        url: row.url.clone(),
        target_fps: row.target_fps as u32,
        resolution: match (row.resolution_width, row.resolution_height) {
            (Some(w), Some(h)) => Some((w as u32, h as u32)),
            _ => None,
        },
        owner_addon_id: Some(addon_id.clone()),
        credentials_encrypted: new_blob.clone(),
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
    audit(caller.data(), "camera.credentials_rotate", Some(&input.camera_id), RiskClass::A, "ok", Some(&reason));
    let out = CameraCredentialsRotateOut {
        rotated: true,
        reason: if new_blob.is_some() {
            "credentials updated".to_string()
        } else {
            "credentials cleared".to_string()
        },
    };
    write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
}

// =============================================================================
// Test surface — drives the host functions through a stable, sync API for
// integration tests that do not spin up a wasmtime Store.
// =============================================================================

/// Pure-Rust core of `camera_add_v1` that operates on raw input bytes and
/// an explicit `AddonState`, with no wasmtime caller. Production code goes
/// through `camera_add_v1`; tests use this entry point to inject malformed
/// TOML and oversized payloads without standing up an InstancePool.
pub(crate) fn camera_add_core(state: &AddonState, raw_input: &[u8]) -> i32 {
    if enforce_payload_size(raw_input.len(), PayloadKind::ServiceCall).is_err() {
        audit(state, "camera.add", None, RiskClass::A, "error", Some("payload_too_large"));
        return AbiError::PayloadTooLarge.as_i32();
    }
    let raw = match std::str::from_utf8(raw_input) {
        Ok(s) => s,
        Err(_) => {
            audit(state, "camera.add", None, RiskClass::A, "error", Some("input_read_failed"));
            return AbiError::Operation.as_i32();
        }
    };
    if !check_permission(state, PERM_CAMERAS_WRITE, None) {
        audit(state, "camera.add", None, RiskClass::A, "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }
    let input: CameraAddInput = match toml::from_str(raw) {
        Ok(v) => v,
        Err(_) => {
            audit(state, "camera.add", None, RiskClass::A, "error", Some("invalid_toml"));
            return AbiError::Operation.as_i32();
        }
    };
    if let Err(reason) = validate_vendor(&input.vendor) {
        let err = if reason == "unsupported_vendor" {
            AbiError::CameraVendorUnsupported
        } else {
            AbiError::Operation
        };
        audit(state, "camera.add", None, RiskClass::A, "denied", Some(reason));
        return err.as_i32();
    }
    if let Err(reason) = validate_url(&input.url) {
        audit(state, "camera.add", None, RiskClass::A, "denied", Some(reason));
        return AbiError::Operation.as_i32();
    }
    if !(1..=60).contains(&input.target_fps) {
        audit(state, "camera.add", None, RiskClass::A, "denied", Some("target_fps_out_of_range"));
        return AbiError::Operation.as_i32();
    }
    if let Err(reason) = validate_retention(&input.retention_class) {
        audit(state, "camera.add", None, RiskClass::A, "denied", Some(reason));
        return AbiError::Operation.as_i32();
    }
    if let Err(reason) = validate_display_name(&input.display_name) {
        audit(state, "camera.add", None, RiskClass::A, "denied", Some(reason));
        return AbiError::Operation.as_i32();
    }
    if let Err(reason) = validate_profile(&input.profile) {
        audit(state, "camera.add", None, RiskClass::A, "denied", Some(reason));
        return AbiError::Operation.as_i32();
    }
    let credentials_blob = match prepare_credentials_blob(input.credentials_b64.as_deref()) {
        Ok(v) => v,
        Err(reason) => {
            audit(state, "camera.add", None, RiskClass::A, "denied", Some(reason));
            return AbiError::Operation.as_i32();
        }
    };

    let camera_id = format!("cam_{}", uuid::Uuid::new_v4());
    let addon_id = state.addon_id.clone();
    let db = state.db.clone();

    let res_w = input.resolution_width.map(|v| v as i64);
    let res_h = input.resolution_height.map(|v| v as i64);

    let cfg = CameraConfig {
        camera_id: camera_id.clone(),
        vendor: input.vendor.clone(),
        url: input.url.clone(),
        target_fps: input.target_fps,
        resolution: match (input.resolution_width, input.resolution_height) {
            (Some(w), Some(h)) => Some((w, h)),
            _ => None,
        },
        owner_addon_id: Some(addon_id.clone()),
        credentials_encrypted: credentials_blob.clone(),
    };
    let sup = match run_async(get_or_init_supervisor()) {
        Ok(s) => s,
        Err(e) => {
            audit(state, "camera.add", Some(&camera_id), RiskClass::A, "error", Some("supervisor_init_failed"));
            return e.as_i32();
        }
    };
    if let Err(e) = run_async(sup.add_camera(cfg)) {
        let mapped = map_ingest_error(&e);
        let reason = match &e {
            CameraIngestError::QuotaExceeded(_) => "quota_exceeded".to_string(),
            other => format!("session_start_failed: {other}"),
        };
        audit(state, "camera.add", Some(&camera_id), RiskClass::A, "error", Some(&reason));
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
        credentials_blob.as_deref(),
    ) {
        warn!("camera.add insert_camera failed (compensating remove_camera): {e}");
        let _ = run_async(sup.remove_camera(&camera_id));
        audit(state, "camera.add", Some(&camera_id), RiskClass::A, "error", Some("db_insert_failed"));
        return AbiError::Operation.as_i32();
    }

    audit(state, "camera.add", Some(&camera_id), RiskClass::A, "ok", None);
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
    /// inject malformed TOML, oversized payloads, and exercise the quota
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
