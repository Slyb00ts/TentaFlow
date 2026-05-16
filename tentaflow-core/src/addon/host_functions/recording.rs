// =============================================================================
// File: recording.rs — Recording + frame_url host functions (M1.W8 F1a TentaVision)
// =============================================================================
//
// 7 host functions that bridge addon-side WASM calls to the
// `services::recording` filesystem layer and the `services::signed_urls`
// HMAC issuers. Each call:
//   1. enforces input payload size BEFORE materializing a String,
//   2. parses TOML, validates ownership / refs / lengths,
//   3. enforces permission,
//   4. mutates filesystem + DB and/or issues a signed URL,
//   5. records an audit-log entry on every exit path (ok / denied / error),
//   6. enforces output payload max before write_output_with_retry_semantics.
//
// F1a scope: PNG snapshots, MP4 segments from `file://` only. No automatic
// retention — `recording_purge_v1` is manual. Signed URLs are multi-use
// HMAC-SHA256 with per-scope keys (frame: 60-600s, recording: 60-3600s).
// HTTP handler that serves the bytes is Chunk D.

#![cfg(feature = "camera")]
#![allow(clippy::too_many_arguments)]

use base64::Engine;
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::abi_helpers::{enforce_payload_size, write_output_with_retry_semantics, PayloadKind};
use super::{
    audit_log_with_risk, check_permission, get_memory, read_guest_bytes, AddonState, WasmCaller,
};
use crate::addon::errors::AbiError;
use crate::audit::RiskClass;
use crate::db::repository::{
    get_camera_for_addon, get_recording_for_addon, insert_recording, recording_stats_for_addon,
    soft_delete_recording, RecordingStatsAggregate,
};
use crate::services::frame_storage::RawFrameRef;
use crate::services::recording::{
    purge_recording, read_recording, save_snapshot_rgb24, RecordingError, SavedRecording,
};
#[cfg(feature = "camera")]
use crate::services::recording::save_segment_mp4;
use crate::services::{frame_storage, frame_url_issuer, recording_url_issuer};

// =============================================================================
// Permission constants
// =============================================================================

const PERM_RECORDING_READ: &str = "recording.read";
const PERM_RECORDING_WRITE: &str = "recording.write";

// =============================================================================
// Validators + length caps
// =============================================================================

const MAX_REF: usize = 256;
const MAX_SOURCE_URL: usize = 4096;
const MAX_RETENTION_CLASS: usize = 32;

fn retention_class_valid(rc: &str) -> bool {
    matches!(rc, "A" | "B" | "C" | "Unclassified")
}

fn validate_recording_ref(s: &str) -> Result<(), &'static str> {
    if s.is_empty() || s.len() > MAX_REF {
        return Err("recording_ref_invalid");
    }
    if !(s.starts_with("snap_") || s.starts_with("clip_")) {
        return Err("recording_ref_invalid_prefix");
    }
    Ok(())
}

fn validate_frame_ref(s: &str) -> Result<(), &'static str> {
    if s.is_empty() || s.len() > MAX_REF {
        return Err("frame_ref_invalid");
    }
    if !s.starts_with("frame_") {
        return Err("frame_ref_invalid_prefix");
    }
    Ok(())
}

// =============================================================================
// Payload structs — input / output
// =============================================================================

#[derive(Debug, Deserialize)]
struct SaveSnapshotInput {
    camera_id: String,
    frame_ref: String,
    retention_class: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SaveSegmentInput {
    camera_id: String,
    source_url: String,
    duration_secs: u32,
    retention_class: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RecordingRefInput {
    recording_ref: String,
}

#[derive(Debug, Deserialize)]
struct GetUrlInput {
    recording_ref: String,
    ttl_secs: u64,
}

#[derive(Debug, Deserialize, Default)]
struct StatsInput {
    #[serde(default)]
    camera_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FrameUrlInput {
    frame_ref: String,
    ttl_secs: u64,
}

#[derive(Debug, Serialize)]
struct SaveRecordingOut {
    recording_ref: String,
    file_path: String,
    file_size_bytes: u64,
    duration_ms: Option<u32>,
    width: Option<u32>,
    height: Option<u32>,
    hash_sha256: String,
    created_at: u64,
}

#[derive(Debug, Serialize)]
struct UrlOut {
    url: String,
    expires_unix_ms: u64,
}

#[derive(Debug, Serialize)]
struct GetStreamOut {
    data_b64: String,
    file_size_bytes: u64,
    hash_sha256: String,
}

#[derive(Debug, Serialize)]
struct PurgeOut {
    purged: bool,
}

#[derive(Debug, Serialize)]
struct StatsPerCamera {
    camera_id: String,
    snapshots: u64,
    segments: u64,
    size_bytes: u64,
}

#[derive(Debug, Serialize)]
struct StatsTotals {
    total_snapshots: u64,
    total_segments: u64,
    total_size_bytes: u64,
}

#[derive(Debug, Serialize)]
struct StatsOut {
    stats: StatsTotals,
    per_camera: Vec<StatsPerCamera>,
}

// =============================================================================
// Helpers — TOML io + audit + risk mapping
// =============================================================================

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
        Some("recording"),
        resource_id,
        risk,
        None,
        None,
        result,
        reason,
    );
}

/// Map a retention class string to `RiskClass`. F1a uses retention class as
/// the audit-chain risk proxy: A => stricter (PII / RODO-protected), B/C
/// progressively less so, Unclassified falls back to Unclassified.
fn risk_for_retention(rc: &str) -> RiskClass {
    match rc {
        "A" => RiskClass::A,
        "B" => RiskClass::B,
        "C" => RiskClass::C,
        _ => RiskClass::Unclassified,
    }
}

fn map_recording_error(e: &RecordingError) -> AbiError {
    use RecordingError::*;
    match e {
        Io(_) => AbiError::Operation,
        PngEncode(_) => AbiError::Operation,
        GstPipeline(_) => AbiError::Operation,
        InvalidCameraId => AbiError::Operation,
        InvalidRetentionClass(_) => AbiError::Operation,
        BaseDirUnavailable(_) => AbiError::Operation,
        InvalidDimensions(_, _, _) => AbiError::Operation,
    }
}

fn run_async<F, T>(fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
}

// =============================================================================
// Host function: recording_save_snapshot_v1
// =============================================================================

pub fn recording_save_snapshot_v1(
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
            audit(caller.data(), "recording.save_snapshot", None, RiskClass::A, "error",
                Some(if e == AbiError::PayloadTooLarge { "payload_too_large" } else { "input_read_failed" }));
            return e.as_i32();
        }
    };
    if !check_permission(caller.data(), PERM_RECORDING_WRITE, None) {
        audit(caller.data(), "recording.save_snapshot", None, RiskClass::A, "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }
    let input: SaveSnapshotInput = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            audit(caller.data(), "recording.save_snapshot", None, RiskClass::A, "error", Some("invalid_toml"));
            return AbiError::Operation.as_i32();
        }
    };
    if let Err(reason) = validate_frame_ref(&input.frame_ref) {
        audit(caller.data(), "recording.save_snapshot", Some(&input.camera_id), RiskClass::A, "denied", Some(reason));
        return AbiError::Operation.as_i32();
    }
    if let Some(rc) = input.retention_class.as_ref() {
        if rc.len() > MAX_RETENTION_CLASS || !retention_class_valid(rc) {
            audit(caller.data(), "recording.save_snapshot", Some(&input.camera_id), RiskClass::A, "denied", Some("invalid_retention_class"));
            return AbiError::Operation.as_i32();
        }
    }

    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();
    // Ownership check + retention pull from cameras table.
    let cam_row = match get_camera_for_addon(&db, &addon_id, &input.camera_id) {
        Ok(Some(r)) => r,
        Ok(None) => {
            audit(caller.data(), "recording.save_snapshot", Some(&input.camera_id), RiskClass::A, "denied", Some("camera_not_found_or_not_owned"));
            return AbiError::NotFound.as_i32();
        }
        Err(_) => {
            audit(caller.data(), "recording.save_snapshot", Some(&input.camera_id), RiskClass::A, "error", Some("db_error"));
            return AbiError::Operation.as_i32();
        }
    };
    let retention_class = input.retention_class.clone().unwrap_or_else(|| cam_row.retention_class.clone());
    let risk = risk_for_retention(&retention_class);

    // Pull frame from LRU. peek via get; if the frame metadata's camera does
    // not match the validated `camera_id`, treat as NotFound (no cross-camera
    // capture).
    let stored = match frame_storage().get(&RawFrameRef::from_string(input.frame_ref.clone())) {
        Some(f) => f,
        None => {
            audit(caller.data(), "recording.save_snapshot", Some(&input.frame_ref), risk, "denied", Some("frame_ref_not_found"));
            return AbiError::NotFound.as_i32();
        }
    };
    if stored.metadata.camera_id != input.camera_id {
        audit(caller.data(), "recording.save_snapshot", Some(&input.frame_ref), risk, "denied", Some("frame_camera_mismatch"));
        return AbiError::NotFound.as_i32();
    }

    let width = stored.metadata.width;
    let height = stored.metadata.height;
    let camera_id = input.camera_id.clone();
    let data: Vec<u8> = stored.data.to_vec();
    let saved: SavedRecording = match run_async(save_snapshot_rgb24(&camera_id, &data, width, height)) {
        Ok(v) => v,
        Err(e) => {
            let mapped = map_recording_error(&e);
            audit(caller.data(), "recording.save_snapshot", Some(&camera_id), risk, "error", Some(&format!("save_failed: {e}")));
            return mapped.as_i32();
        }
    };

    let file_path_str = saved.file_path.to_string_lossy().to_string();
    if let Err(e) = insert_recording(
        &db,
        saved.recording_ref.as_str(),
        "snapshot",
        &addon_id,
        &camera_id,
        &file_path_str,
        saved.file_size_bytes as i64,
        None,
        saved.width.map(|v| v as i64),
        saved.height.map(|v| v as i64),
        saved.pixel_format.as_deref(),
        &saved.hash_sha256,
        &retention_class,
    ) {
        warn!("recording.save_snapshot insert_recording failed (compensating purge): {e}");
        let _ = run_async(purge_recording(&saved.file_path));
        audit(caller.data(), "recording.save_snapshot", Some(&camera_id), risk, "error", Some("db_insert_failed"));
        return AbiError::Operation.as_i32();
    }

    let recording_ref = saved.recording_ref.as_str().to_string();
    audit(caller.data(), "recording.save_snapshot", Some(&recording_ref), risk, "ok", None);
    let out = SaveRecordingOut {
        recording_ref,
        file_path: file_path_str,
        file_size_bytes: saved.file_size_bytes,
        duration_ms: saved.duration_ms,
        width: saved.width,
        height: saved.height,
        hash_sha256: saved.hash_sha256,
        created_at: saved.created_at,
    };
    write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
}

// =============================================================================
// Host function: recording_save_segment_v1
// =============================================================================

pub fn recording_save_segment_v1(
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
            audit(caller.data(), "recording.save_segment", None, RiskClass::A, "error",
                Some(if e == AbiError::PayloadTooLarge { "payload_too_large" } else { "input_read_failed" }));
            return e.as_i32();
        }
    };
    if !check_permission(caller.data(), PERM_RECORDING_WRITE, None) {
        audit(caller.data(), "recording.save_segment", None, RiskClass::A, "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }
    let input: SaveSegmentInput = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            audit(caller.data(), "recording.save_segment", None, RiskClass::A, "error", Some("invalid_toml"));
            return AbiError::Operation.as_i32();
        }
    };
    if input.source_url.is_empty() || input.source_url.len() > MAX_SOURCE_URL {
        audit(caller.data(), "recording.save_segment", Some(&input.camera_id), RiskClass::A, "denied", Some("source_url_length"));
        return AbiError::Operation.as_i32();
    }
    if !input.source_url.starts_with("file://") {
        audit(caller.data(), "recording.save_segment", Some(&input.camera_id), RiskClass::A, "denied", Some("source_url_scheme_unsupported"));
        return AbiError::Operation.as_i32();
    }
    if !(1..=60).contains(&input.duration_secs) {
        audit(caller.data(), "recording.save_segment", Some(&input.camera_id), RiskClass::A, "denied", Some("duration_out_of_range"));
        return AbiError::Operation.as_i32();
    }
    if let Some(rc) = input.retention_class.as_ref() {
        if rc.len() > MAX_RETENTION_CLASS || !retention_class_valid(rc) {
            audit(caller.data(), "recording.save_segment", Some(&input.camera_id), RiskClass::A, "denied", Some("invalid_retention_class"));
            return AbiError::Operation.as_i32();
        }
    }

    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();
    let cam_row = match get_camera_for_addon(&db, &addon_id, &input.camera_id) {
        Ok(Some(r)) => r,
        Ok(None) => {
            audit(caller.data(), "recording.save_segment", Some(&input.camera_id), RiskClass::A, "denied", Some("camera_not_found_or_not_owned"));
            return AbiError::NotFound.as_i32();
        }
        Err(_) => {
            audit(caller.data(), "recording.save_segment", Some(&input.camera_id), RiskClass::A, "error", Some("db_error"));
            return AbiError::Operation.as_i32();
        }
    };
    let retention_class = input.retention_class.clone().unwrap_or_else(|| cam_row.retention_class.clone());
    let risk = risk_for_retention(&retention_class);

    let camera_id = input.camera_id.clone();
    let source_url = input.source_url.clone();
    let saved: SavedRecording = match run_async(save_segment_mp4(&camera_id, &source_url, input.duration_secs)) {
        Ok(v) => v,
        Err(e) => {
            let mapped = map_recording_error(&e);
            audit(caller.data(), "recording.save_segment", Some(&camera_id), risk, "error", Some(&format!("save_failed: {e}")));
            return mapped.as_i32();
        }
    };

    let file_path_str = saved.file_path.to_string_lossy().to_string();
    if let Err(e) = insert_recording(
        &db,
        saved.recording_ref.as_str(),
        "segment",
        &addon_id,
        &camera_id,
        &file_path_str,
        saved.file_size_bytes as i64,
        saved.duration_ms.map(|v| v as i64),
        None,
        None,
        None,
        &saved.hash_sha256,
        &retention_class,
    ) {
        warn!("recording.save_segment insert_recording failed (compensating purge): {e}");
        let _ = run_async(purge_recording(&saved.file_path));
        audit(caller.data(), "recording.save_segment", Some(&camera_id), risk, "error", Some("db_insert_failed"));
        return AbiError::Operation.as_i32();
    }

    let recording_ref = saved.recording_ref.as_str().to_string();
    audit(caller.data(), "recording.save_segment", Some(&recording_ref), risk, "ok", None);
    let out = SaveRecordingOut {
        recording_ref,
        file_path: file_path_str,
        file_size_bytes: saved.file_size_bytes,
        duration_ms: saved.duration_ms,
        width: None,
        height: None,
        hash_sha256: saved.hash_sha256,
        created_at: saved.created_at,
    };
    write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
}

// =============================================================================
// Host function: recording_get_url_v1
// =============================================================================

pub fn recording_get_url_v1(
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
            audit(caller.data(), "recording.get_url", None, RiskClass::B, "error",
                Some(if e == AbiError::PayloadTooLarge { "payload_too_large" } else { "input_read_failed" }));
            return e.as_i32();
        }
    };
    if !check_permission(caller.data(), PERM_RECORDING_READ, None) {
        audit(caller.data(), "recording.get_url", None, RiskClass::B, "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }
    let input: GetUrlInput = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            audit(caller.data(), "recording.get_url", None, RiskClass::B, "error", Some("invalid_toml"));
            return AbiError::Operation.as_i32();
        }
    };
    if let Err(reason) = validate_recording_ref(&input.recording_ref) {
        audit(caller.data(), "recording.get_url", None, RiskClass::B, "denied", Some(reason));
        return AbiError::Operation.as_i32();
    }

    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();
    match get_recording_for_addon(&db, &addon_id, &input.recording_ref) {
        Ok(Some(_)) => {}
        Ok(None) => {
            audit(caller.data(), "recording.get_url", Some(&input.recording_ref), RiskClass::B, "denied", Some("not_found_or_not_owned"));
            return AbiError::NotFound.as_i32();
        }
        Err(_) => {
            audit(caller.data(), "recording.get_url", Some(&input.recording_ref), RiskClass::B, "error", Some("db_error"));
            return AbiError::Operation.as_i32();
        }
    }

    let issued = match recording_url_issuer().issue(input.recording_ref.clone(), input.ttl_secs) {
        Ok(u) => u,
        Err(e) => {
            audit(caller.data(), "recording.get_url", Some(&input.recording_ref), RiskClass::B, "denied", Some(&format!("issue_failed: {e}")));
            return AbiError::Operation.as_i32();
        }
    };
    let url = format!("/recordings/{}?{}", input.recording_ref, issued.query_string());
    audit(caller.data(), "recording.get_url", Some(&input.recording_ref), RiskClass::B, "ok", None);
    let out = UrlOut { url, expires_unix_ms: issued.expiry_unix_ms };
    write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
}

// =============================================================================
// Host function: recording_get_stream_v1
// =============================================================================

pub fn recording_get_stream_v1(
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
            audit(caller.data(), "recording.get_stream", None, RiskClass::B, "error",
                Some(if e == AbiError::PayloadTooLarge { "payload_too_large" } else { "input_read_failed" }));
            return e.as_i32();
        }
    };
    if !check_permission(caller.data(), PERM_RECORDING_READ, None) {
        audit(caller.data(), "recording.get_stream", None, RiskClass::B, "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }
    let input: RecordingRefInput = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            audit(caller.data(), "recording.get_stream", None, RiskClass::B, "error", Some("invalid_toml"));
            return AbiError::Operation.as_i32();
        }
    };
    if let Err(reason) = validate_recording_ref(&input.recording_ref) {
        audit(caller.data(), "recording.get_stream", None, RiskClass::B, "denied", Some(reason));
        return AbiError::Operation.as_i32();
    }

    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();
    let row = match get_recording_for_addon(&db, &addon_id, &input.recording_ref) {
        Ok(Some(r)) => r,
        Ok(None) => {
            audit(caller.data(), "recording.get_stream", Some(&input.recording_ref), RiskClass::B, "denied", Some("not_found_or_not_owned"));
            return AbiError::NotFound.as_i32();
        }
        Err(_) => {
            audit(caller.data(), "recording.get_stream", Some(&input.recording_ref), RiskClass::B, "error", Some("db_error"));
            return AbiError::Operation.as_i32();
        }
    };
    // Enforce the absolute ServiceCall ceiling BEFORE reading the file —
    // bails fast on a multi-MB segment without pulling it into RAM only to
    // reject the response.
    if row.file_size_bytes > 0 && (row.file_size_bytes as usize) > PayloadKind::ServiceCall.max_bytes() {
        audit(caller.data(), "recording.get_stream", Some(&input.recording_ref), RiskClass::B, "error", Some("payload_too_large"));
        return AbiError::PayloadTooLarge.as_i32();
    }

    let file_path = std::path::PathBuf::from(&row.file_path);
    let bytes = match run_async(read_recording(&file_path)) {
        Ok(b) => b,
        Err(e) => {
            audit(caller.data(), "recording.get_stream", Some(&input.recording_ref), RiskClass::B, "error", Some(&format!("read_failed: {e}")));
            return AbiError::Operation.as_i32();
        }
    };
    if enforce_payload_size(bytes.len(), PayloadKind::ServiceCall).is_err() {
        audit(caller.data(), "recording.get_stream", Some(&input.recording_ref), RiskClass::B, "error", Some("payload_too_large"));
        return AbiError::PayloadTooLarge.as_i32();
    }

    let data_b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let out = GetStreamOut {
        data_b64,
        file_size_bytes: bytes.len() as u64,
        hash_sha256: row.hash_sha256,
    };
    audit(caller.data(), "recording.get_stream", Some(&input.recording_ref), RiskClass::B, "ok", None);
    write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
}

// =============================================================================
// Host function: recording_purge_v1
// =============================================================================

pub fn recording_purge_v1(
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
            audit(caller.data(), "recording.purge", None, RiskClass::A, "error",
                Some(if e == AbiError::PayloadTooLarge { "payload_too_large" } else { "input_read_failed" }));
            return e.as_i32();
        }
    };
    if !check_permission(caller.data(), PERM_RECORDING_WRITE, None) {
        audit(caller.data(), "recording.purge", None, RiskClass::A, "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }
    let input: RecordingRefInput = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            audit(caller.data(), "recording.purge", None, RiskClass::A, "error", Some("invalid_toml"));
            return AbiError::Operation.as_i32();
        }
    };
    if let Err(reason) = validate_recording_ref(&input.recording_ref) {
        audit(caller.data(), "recording.purge", None, RiskClass::A, "denied", Some(reason));
        return AbiError::Operation.as_i32();
    }

    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();
    let row = match get_recording_for_addon(&db, &addon_id, &input.recording_ref) {
        Ok(Some(r)) => r,
        Ok(None) => {
            audit(caller.data(), "recording.purge", Some(&input.recording_ref), RiskClass::A, "denied", Some("not_found_or_not_owned"));
            return AbiError::NotFound.as_i32();
        }
        Err(_) => {
            audit(caller.data(), "recording.purge", Some(&input.recording_ref), RiskClass::A, "error", Some("db_error"));
            return AbiError::Operation.as_i32();
        }
    };

    let file_path = std::path::PathBuf::from(&row.file_path);
    if let Err(e) = run_async(purge_recording(&file_path)) {
        warn!("recording.purge file removal failed (continuing with DB soft-delete): {e}");
    }
    if let Err(_e) = soft_delete_recording(&db, &addon_id, &input.recording_ref) {
        audit(caller.data(), "recording.purge", Some(&input.recording_ref), RiskClass::A, "error", Some("db_soft_delete_failed"));
        return AbiError::Operation.as_i32();
    }

    audit(caller.data(), "recording.purge", Some(&input.recording_ref), RiskClass::A, "ok", None);
    let out = PurgeOut { purged: true };
    write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
}

// =============================================================================
// Host function: recording_stats_v1
// =============================================================================

pub fn recording_stats_v1(
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
    // Empty payload is OK — `StatsInput` defaults to "no filter".
    let raw = if input_len <= 0 {
        String::new()
    } else {
        match read_input_toml(&memory, &caller, input_ptr, input_len) {
            Ok(s) => s,
            Err(e) => {
                audit(caller.data(), "recording.stats", None, RiskClass::B, "error",
                    Some(if e == AbiError::PayloadTooLarge { "payload_too_large" } else { "input_read_failed" }));
                return e.as_i32();
            }
        }
    };
    if !check_permission(caller.data(), PERM_RECORDING_READ, None) {
        audit(caller.data(), "recording.stats", None, RiskClass::B, "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }
    let input: StatsInput = if raw.is_empty() {
        StatsInput::default()
    } else {
        match toml::from_str(&raw) {
            Ok(v) => v,
            Err(_) => {
                audit(caller.data(), "recording.stats", None, RiskClass::B, "error", Some("invalid_toml"));
                return AbiError::Operation.as_i32();
            }
        }
    };

    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();
    let agg: RecordingStatsAggregate =
        match recording_stats_for_addon(&db, &addon_id, input.camera_id.as_deref()) {
            Ok(a) => a,
            Err(_) => {
                audit(caller.data(), "recording.stats", None, RiskClass::B, "error", Some("db_error"));
                return AbiError::Operation.as_i32();
            }
        };

    let per_camera: Vec<StatsPerCamera> = agg
        .per_camera
        .into_iter()
        .map(|(camera_id, snapshots, segments, size_bytes)| StatsPerCamera {
            camera_id,
            snapshots,
            segments,
            size_bytes,
        })
        .collect();
    let out = StatsOut {
        stats: StatsTotals {
            total_snapshots: agg.total_snapshots,
            total_segments: agg.total_segments,
            total_size_bytes: agg.total_size_bytes,
        },
        per_camera,
    };
    audit(caller.data(), "recording.stats", None, RiskClass::B, "ok", None);
    write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
}

// =============================================================================
// Host function: frame_url_v1
// =============================================================================

pub fn frame_url_v1(
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
            audit(caller.data(), "recording.frame_url", None, RiskClass::B, "error",
                Some(if e == AbiError::PayloadTooLarge { "payload_too_large" } else { "input_read_failed" }));
            return e.as_i32();
        }
    };
    if !check_permission(caller.data(), PERM_RECORDING_READ, None) {
        audit(caller.data(), "recording.frame_url", None, RiskClass::B, "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }
    let input: FrameUrlInput = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            audit(caller.data(), "recording.frame_url", None, RiskClass::B, "error", Some("invalid_toml"));
            return AbiError::Operation.as_i32();
        }
    };
    if let Err(reason) = validate_frame_ref(&input.frame_ref) {
        audit(caller.data(), "recording.frame_url", None, RiskClass::B, "denied", Some(reason));
        return AbiError::Operation.as_i32();
    }

    let stored = match frame_storage().get(&RawFrameRef::from_string(input.frame_ref.clone())) {
        Some(f) => f,
        None => {
            audit(caller.data(), "recording.frame_url", Some(&input.frame_ref), RiskClass::B, "denied", Some("frame_ref_not_found"));
            return AbiError::NotFound.as_i32();
        }
    };
    // Ownership: the frame's `camera_id` must resolve to a camera owned by
    // the calling addon. We swallow the DB row beyond ownership — the
    // frame_url doesn't expose camera metadata.
    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();
    match get_camera_for_addon(&db, &addon_id, &stored.metadata.camera_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            audit(caller.data(), "recording.frame_url", Some(&input.frame_ref), RiskClass::B, "denied", Some("camera_not_owned"));
            return AbiError::NotFound.as_i32();
        }
        Err(_) => {
            audit(caller.data(), "recording.frame_url", Some(&input.frame_ref), RiskClass::B, "error", Some("db_error"));
            return AbiError::Operation.as_i32();
        }
    }

    let issued = match frame_url_issuer().issue(input.frame_ref.clone(), input.ttl_secs) {
        Ok(u) => u,
        Err(e) => {
            audit(caller.data(), "recording.frame_url", Some(&input.frame_ref), RiskClass::B, "denied", Some(&format!("issue_failed: {e}")));
            return AbiError::Operation.as_i32();
        }
    };
    let url = format!("/frames/{}?{}", input.frame_ref, issued.query_string());
    audit(caller.data(), "recording.frame_url", Some(&input.frame_ref), RiskClass::B, "ok", None);
    let out = UrlOut { url, expires_unix_ms: issued.expiry_unix_ms };
    write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
}

// =============================================================================
// Test surface — sync entry points so integration tests can drive the host
// functions without standing up a wasmtime Store.
// =============================================================================

#[doc(hidden)]
pub mod test_api {
    use super::*;

    /// Wraps `recording_save_snapshot_v1` for tests: accepts a TOML payload as
    /// raw bytes and the AddonState directly, returning the same TOML output
    /// (or an empty Vec on error) plus the ABI return code. Skips the wasmtime
    /// caller indirection that requires an InstancePool.
    pub fn save_snapshot_with_raw_input(state: &AddonState, raw_input: &[u8]) -> (i32, Vec<u8>) {
        run_input(state, raw_input, save_snapshot_core)
    }

    pub fn save_segment_with_raw_input(state: &AddonState, raw_input: &[u8]) -> (i32, Vec<u8>) {
        run_input(state, raw_input, save_segment_core)
    }

    pub fn get_url_with_raw_input(state: &AddonState, raw_input: &[u8]) -> (i32, Vec<u8>) {
        run_input(state, raw_input, get_url_core)
    }

    pub fn get_stream_with_raw_input(state: &AddonState, raw_input: &[u8]) -> (i32, Vec<u8>) {
        run_input(state, raw_input, get_stream_core)
    }

    pub fn purge_with_raw_input(state: &AddonState, raw_input: &[u8]) -> (i32, Vec<u8>) {
        run_input(state, raw_input, purge_core)
    }

    pub fn stats_with_raw_input(state: &AddonState, raw_input: &[u8]) -> (i32, Vec<u8>) {
        run_input(state, raw_input, stats_core)
    }

    pub fn frame_url_with_raw_input(state: &AddonState, raw_input: &[u8]) -> (i32, Vec<u8>) {
        run_input(state, raw_input, frame_url_core)
    }

    fn run_input(
        state: &AddonState,
        raw_input: &[u8],
        f: fn(&AddonState, &str) -> CoreResult,
    ) -> (i32, Vec<u8>) {
        if enforce_payload_size(raw_input.len(), PayloadKind::ServiceCall).is_err() {
            return (AbiError::PayloadTooLarge.as_i32(), Vec::new());
        }
        let raw = match std::str::from_utf8(raw_input) {
            Ok(s) => s,
            Err(_) => return (AbiError::Operation.as_i32(), Vec::new()),
        };
        match f(state, raw) {
            CoreResult::Ok(bytes) => (AbiError::Ok.as_i32(), bytes),
            CoreResult::Err(code) => (code, Vec::new()),
        }
    }
}

#[doc(hidden)]
pub enum CoreResult {
    Ok(Vec<u8>),
    Err(i32),
}

fn serialize<T: Serialize>(v: &T) -> CoreResult {
    match toml::to_string(v) {
        Ok(s) => {
            if enforce_payload_size(s.len(), PayloadKind::ServiceCall).is_err() {
                return CoreResult::Err(AbiError::PayloadTooLarge.as_i32());
            }
            CoreResult::Ok(s.into_bytes())
        }
        Err(_) => CoreResult::Err(AbiError::Operation.as_i32()),
    }
}

fn save_snapshot_core(state: &AddonState, raw: &str) -> CoreResult {
    if !check_permission(state, PERM_RECORDING_WRITE, None) {
        audit(state, "recording.save_snapshot", None, RiskClass::A, "denied", Some("missing_permission"));
        return CoreResult::Err(AbiError::Permission.as_i32());
    }
    let input: SaveSnapshotInput = match toml::from_str(raw) {
        Ok(v) => v,
        Err(_) => {
            audit(state, "recording.save_snapshot", None, RiskClass::A, "error", Some("invalid_toml"));
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    };
    if let Err(reason) = validate_frame_ref(&input.frame_ref) {
        audit(state, "recording.save_snapshot", Some(&input.camera_id), RiskClass::A, "denied", Some(reason));
        return CoreResult::Err(AbiError::Operation.as_i32());
    }
    if let Some(rc) = input.retention_class.as_ref() {
        if rc.len() > MAX_RETENTION_CLASS || !retention_class_valid(rc) {
            audit(state, "recording.save_snapshot", Some(&input.camera_id), RiskClass::A, "denied", Some("invalid_retention_class"));
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    }
    let cam_row = match get_camera_for_addon(&state.db, &state.addon_id, &input.camera_id) {
        Ok(Some(r)) => r,
        Ok(None) => {
            audit(state, "recording.save_snapshot", Some(&input.camera_id), RiskClass::A, "denied", Some("camera_not_found_or_not_owned"));
            return CoreResult::Err(AbiError::NotFound.as_i32());
        }
        Err(_) => {
            audit(state, "recording.save_snapshot", Some(&input.camera_id), RiskClass::A, "error", Some("db_error"));
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    };
    let retention_class = input.retention_class.clone().unwrap_or_else(|| cam_row.retention_class.clone());
    let risk = risk_for_retention(&retention_class);

    let stored = match frame_storage().get(&RawFrameRef::from_string(input.frame_ref.clone())) {
        Some(f) => f,
        None => {
            audit(state, "recording.save_snapshot", Some(&input.frame_ref), risk, "denied", Some("frame_ref_not_found"));
            return CoreResult::Err(AbiError::NotFound.as_i32());
        }
    };
    if stored.metadata.camera_id != input.camera_id {
        audit(state, "recording.save_snapshot", Some(&input.frame_ref), risk, "denied", Some("frame_camera_mismatch"));
        return CoreResult::Err(AbiError::NotFound.as_i32());
    }
    let width = stored.metadata.width;
    let height = stored.metadata.height;
    let data: Vec<u8> = stored.data.to_vec();
    let saved: SavedRecording = match run_async(save_snapshot_rgb24(&input.camera_id, &data, width, height)) {
        Ok(v) => v,
        Err(e) => {
            let mapped = map_recording_error(&e);
            audit(state, "recording.save_snapshot", Some(&input.camera_id), risk, "error", Some(&format!("save_failed: {e}")));
            return CoreResult::Err(mapped.as_i32());
        }
    };
    let file_path_str = saved.file_path.to_string_lossy().to_string();
    if let Err(e) = insert_recording(
        &state.db,
        saved.recording_ref.as_str(),
        "snapshot",
        &state.addon_id,
        &input.camera_id,
        &file_path_str,
        saved.file_size_bytes as i64,
        None,
        saved.width.map(|v| v as i64),
        saved.height.map(|v| v as i64),
        saved.pixel_format.as_deref(),
        &saved.hash_sha256,
        &retention_class,
    ) {
        warn!("recording.save_snapshot insert_recording failed (compensating purge): {e}");
        let _ = run_async(purge_recording(&saved.file_path));
        audit(state, "recording.save_snapshot", Some(&input.camera_id), risk, "error", Some("db_insert_failed"));
        return CoreResult::Err(AbiError::Operation.as_i32());
    }
    audit(state, "recording.save_snapshot", Some(saved.recording_ref.as_str()), risk, "ok", None);
    let out = SaveRecordingOut {
        recording_ref: saved.recording_ref.as_str().to_string(),
        file_path: file_path_str,
        file_size_bytes: saved.file_size_bytes,
        duration_ms: saved.duration_ms,
        width: saved.width,
        height: saved.height,
        hash_sha256: saved.hash_sha256,
        created_at: saved.created_at,
    };
    serialize(&out)
}

fn save_segment_core(state: &AddonState, raw: &str) -> CoreResult {
    if !check_permission(state, PERM_RECORDING_WRITE, None) {
        audit(state, "recording.save_segment", None, RiskClass::A, "denied", Some("missing_permission"));
        return CoreResult::Err(AbiError::Permission.as_i32());
    }
    let input: SaveSegmentInput = match toml::from_str(raw) {
        Ok(v) => v,
        Err(_) => {
            audit(state, "recording.save_segment", None, RiskClass::A, "error", Some("invalid_toml"));
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    };
    if input.source_url.is_empty() || input.source_url.len() > MAX_SOURCE_URL {
        audit(state, "recording.save_segment", Some(&input.camera_id), RiskClass::A, "denied", Some("source_url_length"));
        return CoreResult::Err(AbiError::Operation.as_i32());
    }
    if !input.source_url.starts_with("file://") {
        audit(state, "recording.save_segment", Some(&input.camera_id), RiskClass::A, "denied", Some("source_url_scheme_unsupported"));
        return CoreResult::Err(AbiError::Operation.as_i32());
    }
    if !(1..=60).contains(&input.duration_secs) {
        audit(state, "recording.save_segment", Some(&input.camera_id), RiskClass::A, "denied", Some("duration_out_of_range"));
        return CoreResult::Err(AbiError::Operation.as_i32());
    }
    if let Some(rc) = input.retention_class.as_ref() {
        if rc.len() > MAX_RETENTION_CLASS || !retention_class_valid(rc) {
            audit(state, "recording.save_segment", Some(&input.camera_id), RiskClass::A, "denied", Some("invalid_retention_class"));
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    }
    let cam_row = match get_camera_for_addon(&state.db, &state.addon_id, &input.camera_id) {
        Ok(Some(r)) => r,
        Ok(None) => {
            audit(state, "recording.save_segment", Some(&input.camera_id), RiskClass::A, "denied", Some("camera_not_found_or_not_owned"));
            return CoreResult::Err(AbiError::NotFound.as_i32());
        }
        Err(_) => {
            audit(state, "recording.save_segment", Some(&input.camera_id), RiskClass::A, "error", Some("db_error"));
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    };
    let retention_class = input.retention_class.clone().unwrap_or_else(|| cam_row.retention_class.clone());
    let risk = risk_for_retention(&retention_class);

    let saved: SavedRecording = match run_async(save_segment_mp4(&input.camera_id, &input.source_url, input.duration_secs)) {
        Ok(v) => v,
        Err(e) => {
            let mapped = map_recording_error(&e);
            audit(state, "recording.save_segment", Some(&input.camera_id), risk, "error", Some(&format!("save_failed: {e}")));
            return CoreResult::Err(mapped.as_i32());
        }
    };
    let file_path_str = saved.file_path.to_string_lossy().to_string();
    if let Err(e) = insert_recording(
        &state.db,
        saved.recording_ref.as_str(),
        "segment",
        &state.addon_id,
        &input.camera_id,
        &file_path_str,
        saved.file_size_bytes as i64,
        saved.duration_ms.map(|v| v as i64),
        None,
        None,
        None,
        &saved.hash_sha256,
        &retention_class,
    ) {
        warn!("recording.save_segment insert_recording failed (compensating purge): {e}");
        let _ = run_async(purge_recording(&saved.file_path));
        audit(state, "recording.save_segment", Some(&input.camera_id), risk, "error", Some("db_insert_failed"));
        return CoreResult::Err(AbiError::Operation.as_i32());
    }
    audit(state, "recording.save_segment", Some(saved.recording_ref.as_str()), risk, "ok", None);
    let out = SaveRecordingOut {
        recording_ref: saved.recording_ref.as_str().to_string(),
        file_path: file_path_str,
        file_size_bytes: saved.file_size_bytes,
        duration_ms: saved.duration_ms,
        width: None,
        height: None,
        hash_sha256: saved.hash_sha256,
        created_at: saved.created_at,
    };
    serialize(&out)
}

fn get_url_core(state: &AddonState, raw: &str) -> CoreResult {
    if !check_permission(state, PERM_RECORDING_READ, None) {
        audit(state, "recording.get_url", None, RiskClass::B, "denied", Some("missing_permission"));
        return CoreResult::Err(AbiError::Permission.as_i32());
    }
    let input: GetUrlInput = match toml::from_str(raw) {
        Ok(v) => v,
        Err(_) => {
            audit(state, "recording.get_url", None, RiskClass::B, "error", Some("invalid_toml"));
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    };
    if let Err(reason) = validate_recording_ref(&input.recording_ref) {
        audit(state, "recording.get_url", None, RiskClass::B, "denied", Some(reason));
        return CoreResult::Err(AbiError::Operation.as_i32());
    }
    match get_recording_for_addon(&state.db, &state.addon_id, &input.recording_ref) {
        Ok(Some(_)) => {}
        Ok(None) => {
            audit(state, "recording.get_url", Some(&input.recording_ref), RiskClass::B, "denied", Some("not_found_or_not_owned"));
            return CoreResult::Err(AbiError::NotFound.as_i32());
        }
        Err(_) => {
            audit(state, "recording.get_url", Some(&input.recording_ref), RiskClass::B, "error", Some("db_error"));
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    }
    let issued = match recording_url_issuer().issue(input.recording_ref.clone(), input.ttl_secs) {
        Ok(u) => u,
        Err(e) => {
            audit(state, "recording.get_url", Some(&input.recording_ref), RiskClass::B, "denied", Some(&format!("issue_failed: {e}")));
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    };
    audit(state, "recording.get_url", Some(&input.recording_ref), RiskClass::B, "ok", None);
    let url = format!("/recordings/{}?{}", input.recording_ref, issued.query_string());
    serialize(&UrlOut { url, expires_unix_ms: issued.expiry_unix_ms })
}

fn get_stream_core(state: &AddonState, raw: &str) -> CoreResult {
    if !check_permission(state, PERM_RECORDING_READ, None) {
        audit(state, "recording.get_stream", None, RiskClass::B, "denied", Some("missing_permission"));
        return CoreResult::Err(AbiError::Permission.as_i32());
    }
    let input: RecordingRefInput = match toml::from_str(raw) {
        Ok(v) => v,
        Err(_) => {
            audit(state, "recording.get_stream", None, RiskClass::B, "error", Some("invalid_toml"));
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    };
    if let Err(reason) = validate_recording_ref(&input.recording_ref) {
        audit(state, "recording.get_stream", None, RiskClass::B, "denied", Some(reason));
        return CoreResult::Err(AbiError::Operation.as_i32());
    }
    let row = match get_recording_for_addon(&state.db, &state.addon_id, &input.recording_ref) {
        Ok(Some(r)) => r,
        Ok(None) => {
            audit(state, "recording.get_stream", Some(&input.recording_ref), RiskClass::B, "denied", Some("not_found_or_not_owned"));
            return CoreResult::Err(AbiError::NotFound.as_i32());
        }
        Err(_) => {
            audit(state, "recording.get_stream", Some(&input.recording_ref), RiskClass::B, "error", Some("db_error"));
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    };
    if row.file_size_bytes > 0 && (row.file_size_bytes as usize) > PayloadKind::ServiceCall.max_bytes() {
        audit(state, "recording.get_stream", Some(&input.recording_ref), RiskClass::B, "error", Some("payload_too_large"));
        return CoreResult::Err(AbiError::PayloadTooLarge.as_i32());
    }
    let file_path = std::path::PathBuf::from(&row.file_path);
    let bytes = match run_async(read_recording(&file_path)) {
        Ok(b) => b,
        Err(e) => {
            audit(state, "recording.get_stream", Some(&input.recording_ref), RiskClass::B, "error", Some(&format!("read_failed: {e}")));
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    };
    if enforce_payload_size(bytes.len(), PayloadKind::ServiceCall).is_err() {
        audit(state, "recording.get_stream", Some(&input.recording_ref), RiskClass::B, "error", Some("payload_too_large"));
        return CoreResult::Err(AbiError::PayloadTooLarge.as_i32());
    }
    audit(state, "recording.get_stream", Some(&input.recording_ref), RiskClass::B, "ok", None);
    let out = GetStreamOut {
        data_b64: base64::engine::general_purpose::STANDARD.encode(&bytes),
        file_size_bytes: bytes.len() as u64,
        hash_sha256: row.hash_sha256,
    };
    serialize(&out)
}

fn purge_core(state: &AddonState, raw: &str) -> CoreResult {
    if !check_permission(state, PERM_RECORDING_WRITE, None) {
        audit(state, "recording.purge", None, RiskClass::A, "denied", Some("missing_permission"));
        return CoreResult::Err(AbiError::Permission.as_i32());
    }
    let input: RecordingRefInput = match toml::from_str(raw) {
        Ok(v) => v,
        Err(_) => {
            audit(state, "recording.purge", None, RiskClass::A, "error", Some("invalid_toml"));
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    };
    if let Err(reason) = validate_recording_ref(&input.recording_ref) {
        audit(state, "recording.purge", None, RiskClass::A, "denied", Some(reason));
        return CoreResult::Err(AbiError::Operation.as_i32());
    }
    let row = match get_recording_for_addon(&state.db, &state.addon_id, &input.recording_ref) {
        Ok(Some(r)) => r,
        Ok(None) => {
            audit(state, "recording.purge", Some(&input.recording_ref), RiskClass::A, "denied", Some("not_found_or_not_owned"));
            return CoreResult::Err(AbiError::NotFound.as_i32());
        }
        Err(_) => {
            audit(state, "recording.purge", Some(&input.recording_ref), RiskClass::A, "error", Some("db_error"));
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    };
    let file_path = std::path::PathBuf::from(&row.file_path);
    if let Err(e) = run_async(purge_recording(&file_path)) {
        warn!("recording.purge file removal failed (continuing with DB soft-delete): {e}");
    }
    if soft_delete_recording(&state.db, &state.addon_id, &input.recording_ref).is_err() {
        audit(state, "recording.purge", Some(&input.recording_ref), RiskClass::A, "error", Some("db_soft_delete_failed"));
        return CoreResult::Err(AbiError::Operation.as_i32());
    }
    audit(state, "recording.purge", Some(&input.recording_ref), RiskClass::A, "ok", None);
    serialize(&PurgeOut { purged: true })
}

fn stats_core(state: &AddonState, raw: &str) -> CoreResult {
    if !check_permission(state, PERM_RECORDING_READ, None) {
        audit(state, "recording.stats", None, RiskClass::B, "denied", Some("missing_permission"));
        return CoreResult::Err(AbiError::Permission.as_i32());
    }
    let input: StatsInput = if raw.trim().is_empty() {
        StatsInput::default()
    } else {
        match toml::from_str(raw) {
            Ok(v) => v,
            Err(_) => {
                audit(state, "recording.stats", None, RiskClass::B, "error", Some("invalid_toml"));
                return CoreResult::Err(AbiError::Operation.as_i32());
            }
        }
    };
    let agg = match recording_stats_for_addon(&state.db, &state.addon_id, input.camera_id.as_deref()) {
        Ok(a) => a,
        Err(_) => {
            audit(state, "recording.stats", None, RiskClass::B, "error", Some("db_error"));
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    };
    let per_camera: Vec<StatsPerCamera> = agg
        .per_camera
        .into_iter()
        .map(|(camera_id, snapshots, segments, size_bytes)| StatsPerCamera {
            camera_id,
            snapshots,
            segments,
            size_bytes,
        })
        .collect();
    let out = StatsOut {
        stats: StatsTotals {
            total_snapshots: agg.total_snapshots,
            total_segments: agg.total_segments,
            total_size_bytes: agg.total_size_bytes,
        },
        per_camera,
    };
    audit(state, "recording.stats", None, RiskClass::B, "ok", None);
    serialize(&out)
}

fn frame_url_core(state: &AddonState, raw: &str) -> CoreResult {
    if !check_permission(state, PERM_RECORDING_READ, None) {
        audit(state, "recording.frame_url", None, RiskClass::B, "denied", Some("missing_permission"));
        return CoreResult::Err(AbiError::Permission.as_i32());
    }
    let input: FrameUrlInput = match toml::from_str(raw) {
        Ok(v) => v,
        Err(_) => {
            audit(state, "recording.frame_url", None, RiskClass::B, "error", Some("invalid_toml"));
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    };
    if let Err(reason) = validate_frame_ref(&input.frame_ref) {
        audit(state, "recording.frame_url", None, RiskClass::B, "denied", Some(reason));
        return CoreResult::Err(AbiError::Operation.as_i32());
    }
    let stored = match frame_storage().get(&RawFrameRef::from_string(input.frame_ref.clone())) {
        Some(f) => f,
        None => {
            audit(state, "recording.frame_url", Some(&input.frame_ref), RiskClass::B, "denied", Some("frame_ref_not_found"));
            return CoreResult::Err(AbiError::NotFound.as_i32());
        }
    };
    match get_camera_for_addon(&state.db, &state.addon_id, &stored.metadata.camera_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            audit(state, "recording.frame_url", Some(&input.frame_ref), RiskClass::B, "denied", Some("camera_not_owned"));
            return CoreResult::Err(AbiError::NotFound.as_i32());
        }
        Err(_) => {
            audit(state, "recording.frame_url", Some(&input.frame_ref), RiskClass::B, "error", Some("db_error"));
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    }
    let issued = match frame_url_issuer().issue(input.frame_ref.clone(), input.ttl_secs) {
        Ok(u) => u,
        Err(e) => {
            audit(state, "recording.frame_url", Some(&input.frame_ref), RiskClass::B, "denied", Some(&format!("issue_failed: {e}")));
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    };
    audit(state, "recording.frame_url", Some(&input.frame_ref), RiskClass::B, "ok", None);
    let url = format!("/frames/{}?{}", input.frame_ref, issued.query_string());
    serialize(&UrlOut { url, expires_unix_ms: issued.expiry_unix_ms })
}
