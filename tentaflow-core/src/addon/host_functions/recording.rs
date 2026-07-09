// =============================================================================
// File: recording.rs — Recording + frame_url host functions (M1.W8 F1a TentaVision)
// =============================================================================
//
// 7 host functions that bridge addon-side WASM calls to the
// `services::recording` filesystem layer and the `services::signed_urls`
// HMAC issuers. Each call:
//   1. enforces permission,
//   2. enforces input payload size BEFORE materializing bytes and decodes CBOR,
//   3. validates ownership / refs / lengths,
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

use std::sync::OnceLock;

use base64::Engine;
use regex::Regex;
use tentaflow_sdk_spec::{
    FrameUrlInput, GetStreamOut, PurgeOut, RecordingGetUrlInput, RecordingRefInput,
    RecordingSaveSegmentInput, RecordingSaveSnapshotInput, RecordingStatsInput, SaveRecordingOut,
    StatsOut, StatsPerCamera, StatsTotals, UrlOut,
};
use tracing::warn;

use super::abi_helpers::{enforce_payload_size, PayloadKind};
use super::cbor_io::{decode_cbor_exact, read_input_cbor, write_cbor_capped};
use super::{audit_log_with_risk, check_permission, get_memory, AddonState, WasmCaller};
use crate::addon::errors::AbiError;
use crate::audit::RiskClass;
use crate::db::repository::{
    get_camera_for_addon, get_recording_for_addon, insert_recording, recording_stats_for_addon,
    soft_delete_recording, RecordingStatsAggregate,
};
use crate::services::frame_storage::RawFrameRef;
#[cfg(feature = "camera")]
use crate::services::recording::save_segment_mp4;
use crate::services::recording::{
    purge_recording, read_recording, save_snapshot_rgb24, RecordingError, SavedRecording,
};
use crate::services::{frame_storage, frame_url_issuer, recording_url_issuer};

// =============================================================================
// Permission constants
// =============================================================================

const PERM_RECORDING_READ: &str = "recording.read";
const PERM_RECORDING_WRITE: &str = "recording.write";

// =============================================================================
// Validators + length caps
// =============================================================================

const MAX_RETENTION_CLASS: usize = 32;

fn retention_class_valid(rc: &str) -> bool {
    matches!(rc, "A" | "B" | "C" | "Unclassified")
}

// Strict format checkers: prefix + exactly one canonical UUIDv4 string. This
// rules out path-traversal payloads ("snap_../../../etc/passwd") and any other
// character outside `[0-9a-f-]` — letting downstream code interpolate the ref
// into URL paths and filesystem helpers without further escaping.
fn validate_recording_ref(s: &str) -> Result<(), &'static str> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"^(snap|clip)_[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
            .expect("recording_ref regex compiles")
    });
    if re.is_match(s) {
        Ok(())
    } else {
        Err("recording_ref_invalid_format")
    }
}

fn validate_frame_ref(s: &str) -> Result<(), &'static str> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"^frame_[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
            .expect("frame_ref regex compiles")
    });
    if re.is_match(s) {
        Ok(())
    } else {
        Err("frame_ref_invalid_format")
    }
}

// =============================================================================
// Helpers — audit + risk mapping + output estimation
// =============================================================================

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

/// Estimate the CBOR output size for `recording_get_stream_v1` given a raw file
/// size in bytes. Returns `None` if any arithmetic step would overflow.
/// `data_b64` expands to `ceil(N/3)*4` bytes; we add a 256 B envelope allowance
/// for map keys, the SHA-256 hex (64 B) and numeric fields.
fn estimate_get_stream_output(file_size_bytes: i64) -> Option<usize> {
    if file_size_bytes < 0 {
        return None;
    }
    let n = file_size_bytes as u64;
    let b64 = n.checked_add(2)?.checked_div(3)?.checked_mul(4)?;
    let total = b64.checked_add(256)?;
    usize::try_from(total).ok()
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
    if !check_permission(caller.data(), PERM_RECORDING_WRITE, None) {
        audit(
            caller.data(),
            "recording.save_snapshot",
            None,
            RiskClass::A,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let input: RecordingSaveSnapshotInput = match read_input_cbor(
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
                "recording.save_snapshot",
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
    match save_snapshot_core(caller.data(), &input) {
        CoreResult::Ok(out) => write_cbor_capped(
            &memory,
            &mut caller,
            &out,
            out_ptr,
            out_cap,
            out_len_ptr,
            PayloadKind::ServiceCall,
        ),
        CoreResult::Err(code) => code,
    }
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
    if !check_permission(caller.data(), PERM_RECORDING_WRITE, None) {
        audit(
            caller.data(),
            "recording.save_segment",
            None,
            RiskClass::A,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let input: RecordingSaveSegmentInput = match read_input_cbor(
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
                "recording.save_segment",
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
    match save_segment_core(caller.data(), &input) {
        CoreResult::Ok(out) => write_cbor_capped(
            &memory,
            &mut caller,
            &out,
            out_ptr,
            out_cap,
            out_len_ptr,
            PayloadKind::ServiceCall,
        ),
        CoreResult::Err(code) => code,
    }
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
    if !check_permission(caller.data(), PERM_RECORDING_READ, None) {
        audit(
            caller.data(),
            "recording.get_url",
            None,
            RiskClass::B,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let input: RecordingGetUrlInput = match read_input_cbor(
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
                "recording.get_url",
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
    match get_url_core(caller.data(), &input) {
        CoreResult::Ok(out) => write_cbor_capped(
            &memory,
            &mut caller,
            &out,
            out_ptr,
            out_cap,
            out_len_ptr,
            PayloadKind::ServiceCall,
        ),
        CoreResult::Err(code) => code,
    }
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
    if !check_permission(caller.data(), PERM_RECORDING_READ, None) {
        audit(
            caller.data(),
            "recording.get_stream",
            None,
            RiskClass::B,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let input: RecordingRefInput = match read_input_cbor(
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
                "recording.get_stream",
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
    match get_stream_core(caller.data(), &input) {
        CoreResult::Ok(out) => write_cbor_capped(
            &memory,
            &mut caller,
            &out,
            out_ptr,
            out_cap,
            out_len_ptr,
            PayloadKind::ServiceCall,
        ),
        CoreResult::Err(code) => code,
    }
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
    if !check_permission(caller.data(), PERM_RECORDING_WRITE, None) {
        audit(
            caller.data(),
            "recording.purge",
            None,
            RiskClass::A,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let input: RecordingRefInput = match read_input_cbor(
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
                "recording.purge",
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
    match purge_core(caller.data(), &input) {
        CoreResult::Ok(out) => write_cbor_capped(
            &memory,
            &mut caller,
            &out,
            out_ptr,
            out_cap,
            out_len_ptr,
            PayloadKind::ServiceCall,
        ),
        CoreResult::Err(code) => code,
    }
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
    if !check_permission(caller.data(), PERM_RECORDING_READ, None) {
        audit(
            caller.data(),
            "recording.stats",
            None,
            RiskClass::B,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    // Empty payload is OK — `RecordingStatsInput` defaults to "no filter". A
    // negative `input_len` is always a protocol error and must surface as an
    // error rather than being silently re-interpreted as "no filter".
    if input_len < 0 {
        audit(
            caller.data(),
            "recording.stats",
            None,
            RiskClass::B,
            "error",
            Some("invalid_input_len"),
        );
        return AbiError::Operation.as_i32();
    }
    let input: RecordingStatsInput = if input_len == 0 {
        RecordingStatsInput::default()
    } else {
        match read_input_cbor(
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
                    "recording.stats",
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
        }
    };
    match stats_core(caller.data(), &input) {
        CoreResult::Ok(out) => write_cbor_capped(
            &memory,
            &mut caller,
            &out,
            out_ptr,
            out_cap,
            out_len_ptr,
            PayloadKind::ServiceCall,
        ),
        CoreResult::Err(code) => code,
    }
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
    if !check_permission(caller.data(), PERM_RECORDING_READ, None) {
        audit(
            caller.data(),
            "recording.frame_url",
            None,
            RiskClass::B,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let input: FrameUrlInput = match read_input_cbor(
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
                "recording.frame_url",
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
    match frame_url_core(caller.data(), &input) {
        CoreResult::Ok(out) => write_cbor_capped(
            &memory,
            &mut caller,
            &out,
            out_ptr,
            out_cap,
            out_len_ptr,
            PayloadKind::ServiceCall,
        ),
        CoreResult::Err(code) => code,
    }
}

// =============================================================================
// Pure-Rust cores — operate on decoded sdk-spec inputs and an explicit
// `AddonState`, returning a typed sdk-spec output or an ABI error code. The
// permission gate lives in the host wrappers (and the test_api shim) before
// decode; the cores assume permission was already granted.
// =============================================================================

#[doc(hidden)]
pub enum CoreResult<T> {
    Ok(T),
    Err(i32),
}

fn save_snapshot_core(
    state: &AddonState,
    input: &RecordingSaveSnapshotInput,
) -> CoreResult<SaveRecordingOut> {
    if let Err(reason) = validate_frame_ref(&input.frame_ref) {
        audit(
            state,
            "recording.save_snapshot",
            Some(&input.camera_id),
            RiskClass::A,
            "denied",
            Some(reason),
        );
        return CoreResult::Err(AbiError::Operation.as_i32());
    }
    if let Some(rc) = input.retention_class.as_ref() {
        if rc.len() > MAX_RETENTION_CLASS || !retention_class_valid(rc) {
            audit(
                state,
                "recording.save_snapshot",
                Some(&input.camera_id),
                RiskClass::A,
                "denied",
                Some("invalid_retention_class"),
            );
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    }
    let cam_row = match get_camera_for_addon(
        &state.db,
        &state.addon_id,
        &input.camera_id,
        state.org_id.as_deref(),
    ) {
        Ok(Some(r)) => r,
        Ok(None) => {
            audit(
                state,
                "recording.save_snapshot",
                Some(&input.camera_id),
                RiskClass::A,
                "denied",
                Some("camera_not_found_or_not_owned"),
            );
            return CoreResult::Err(AbiError::NotFound.as_i32());
        }
        Err(_) => {
            audit(
                state,
                "recording.save_snapshot",
                Some(&input.camera_id),
                RiskClass::A,
                "error",
                Some("db_error"),
            );
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    };
    let retention_class = input
        .retention_class
        .clone()
        .unwrap_or_else(|| cam_row.retention_class.clone());
    let risk = risk_for_retention(&retention_class);

    // Pull frame from LRU; if the frame metadata's camera does not match the
    // validated `camera_id`, treat as NotFound (no cross-camera capture).
    let stored = match frame_storage().get(&RawFrameRef::from_string(input.frame_ref.clone())) {
        Some(f) => f,
        None => {
            audit(
                state,
                "recording.save_snapshot",
                Some(&input.frame_ref),
                risk,
                "denied",
                Some("frame_ref_not_found"),
            );
            return CoreResult::Err(AbiError::NotFound.as_i32());
        }
    };
    if stored.metadata.camera_id != input.camera_id {
        audit(
            state,
            "recording.save_snapshot",
            Some(&input.frame_ref),
            risk,
            "denied",
            Some("frame_camera_mismatch"),
        );
        return CoreResult::Err(AbiError::NotFound.as_i32());
    }
    let width = stored.metadata.width;
    let height = stored.metadata.height;
    let data: Vec<u8> = stored.data.to_vec();
    let saved: SavedRecording =
        match run_async(save_snapshot_rgb24(&input.camera_id, &data, width, height)) {
            Ok(v) => v,
            Err(e) => {
                let mapped = map_recording_error(&e);
                audit(
                    state,
                    "recording.save_snapshot",
                    Some(&input.camera_id),
                    risk,
                    "error",
                    Some(&format!("save_failed: {e}")),
                );
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
        state.org_id.as_deref(),
        None,
    ) {
        warn!("recording.save_snapshot insert_recording failed (compensating purge): {e}");
        let _ = run_async(purge_recording(&saved.file_path));
        audit(
            state,
            "recording.save_snapshot",
            Some(&input.camera_id),
            risk,
            "error",
            Some("db_insert_failed"),
        );
        return CoreResult::Err(AbiError::Operation.as_i32());
    }
    audit(
        state,
        "recording.save_snapshot",
        Some(saved.recording_ref.as_str()),
        risk,
        "ok",
        None,
    );
    CoreResult::Ok(SaveRecordingOut {
        recording_ref: saved.recording_ref.as_str().to_string(),
        file_path: file_path_str,
        file_size_bytes: saved.file_size_bytes,
        duration_ms: saved.duration_ms,
        width: saved.width,
        height: saved.height,
        hash_sha256: saved.hash_sha256,
        created_at: saved.created_at,
    })
}

fn save_segment_core(
    state: &AddonState,
    input: &RecordingSaveSegmentInput,
) -> CoreResult<SaveRecordingOut> {
    if !(1..=60).contains(&input.duration_secs) {
        audit(
            state,
            "recording.save_segment",
            Some(&input.camera_id),
            RiskClass::A,
            "denied",
            Some("duration_out_of_range"),
        );
        return CoreResult::Err(AbiError::Operation.as_i32());
    }
    if let Some(rc) = input.retention_class.as_ref() {
        if rc.len() > MAX_RETENTION_CLASS || !retention_class_valid(rc) {
            audit(
                state,
                "recording.save_segment",
                Some(&input.camera_id),
                RiskClass::A,
                "denied",
                Some("invalid_retention_class"),
            );
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    }
    let cam_row = match get_camera_for_addon(
        &state.db,
        &state.addon_id,
        &input.camera_id,
        state.org_id.as_deref(),
    ) {
        Ok(Some(r)) => r,
        Ok(None) => {
            audit(
                state,
                "recording.save_segment",
                Some(&input.camera_id),
                RiskClass::A,
                "denied",
                Some("camera_not_found_or_not_owned"),
            );
            return CoreResult::Err(AbiError::NotFound.as_i32());
        }
        Err(_) => {
            audit(
                state,
                "recording.save_segment",
                Some(&input.camera_id),
                RiskClass::A,
                "error",
                Some("db_error"),
            );
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    };
    let retention_class = input
        .retention_class
        .clone()
        .unwrap_or_else(|| cam_row.retention_class.clone());
    let risk = risk_for_retention(&retention_class);

    // Source URL is always the camera row's stored URL — never accepted from
    // the addon — so an addon can't pivot recording into reading arbitrary
    // host files. F1a only supports `vendor='fake_file'`; reject anything else
    // before invoking the GStreamer pipeline.
    if cam_row.vendor != "fake_file" {
        audit(
            state,
            "recording.save_segment",
            Some(&input.camera_id),
            risk,
            "denied",
            Some("vendor_unsupported"),
        );
        return CoreResult::Err(AbiError::Operation.as_i32());
    }
    let source_url = if cam_row.url.starts_with("file://") {
        cam_row.url.clone()
    } else {
        format!("file://{}", cam_row.url)
    };
    let saved: SavedRecording = match run_async(save_segment_mp4(
        &input.camera_id,
        &source_url,
        input.duration_secs,
    )) {
        Ok(v) => v,
        Err(e) => {
            let mapped = map_recording_error(&e);
            audit(
                state,
                "recording.save_segment",
                Some(&input.camera_id),
                risk,
                "error",
                Some(&format!("save_failed: {e}")),
            );
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
        state.org_id.as_deref(),
        None,
    ) {
        warn!("recording.save_segment insert_recording failed (compensating purge): {e}");
        let _ = run_async(purge_recording(&saved.file_path));
        audit(
            state,
            "recording.save_segment",
            Some(&input.camera_id),
            risk,
            "error",
            Some("db_insert_failed"),
        );
        return CoreResult::Err(AbiError::Operation.as_i32());
    }
    audit(
        state,
        "recording.save_segment",
        Some(saved.recording_ref.as_str()),
        risk,
        "ok",
        None,
    );
    CoreResult::Ok(SaveRecordingOut {
        recording_ref: saved.recording_ref.as_str().to_string(),
        file_path: file_path_str,
        file_size_bytes: saved.file_size_bytes,
        duration_ms: saved.duration_ms,
        width: None,
        height: None,
        hash_sha256: saved.hash_sha256,
        created_at: saved.created_at,
    })
}

fn get_url_core(state: &AddonState, input: &RecordingGetUrlInput) -> CoreResult<UrlOut> {
    if let Err(reason) = validate_recording_ref(&input.recording_ref) {
        audit(
            state,
            "recording.get_url",
            None,
            RiskClass::B,
            "denied",
            Some(reason),
        );
        return CoreResult::Err(AbiError::Operation.as_i32());
    }
    match get_recording_for_addon(
        &state.db,
        &state.addon_id,
        &input.recording_ref,
        state.org_id.as_deref(),
    ) {
        Ok(Some(_)) => {}
        Ok(None) => {
            audit(
                state,
                "recording.get_url",
                Some(&input.recording_ref),
                RiskClass::B,
                "denied",
                Some("not_found_or_not_owned"),
            );
            return CoreResult::Err(AbiError::NotFound.as_i32());
        }
        Err(_) => {
            audit(
                state,
                "recording.get_url",
                Some(&input.recording_ref),
                RiskClass::B,
                "error",
                Some("db_error"),
            );
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    }
    let issued = match recording_url_issuer().issue(input.recording_ref.clone(), input.ttl_secs) {
        Ok(u) => u,
        Err(e) => {
            audit(
                state,
                "recording.get_url",
                Some(&input.recording_ref),
                RiskClass::B,
                "denied",
                Some(&format!("issue_failed: {e}")),
            );
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    };
    audit(
        state,
        "recording.get_url",
        Some(&input.recording_ref),
        RiskClass::B,
        "ok",
        None,
    );
    // ref validated by `validate_recording_ref` — safe to interpolate into a
    // URL path (only [a-f0-9-] plus the snap_/clip_ prefix).
    let url = format!(
        "/recordings/{}?{}",
        input.recording_ref,
        issued.query_string()
    );
    CoreResult::Ok(UrlOut {
        url,
        expires_unix_ms: issued.expiry_unix_ms,
    })
}

fn get_stream_core(state: &AddonState, input: &RecordingRefInput) -> CoreResult<GetStreamOut> {
    if let Err(reason) = validate_recording_ref(&input.recording_ref) {
        audit(
            state,
            "recording.get_stream",
            None,
            RiskClass::B,
            "denied",
            Some(reason),
        );
        return CoreResult::Err(AbiError::Operation.as_i32());
    }
    let row = match get_recording_for_addon(
        &state.db,
        &state.addon_id,
        &input.recording_ref,
        state.org_id.as_deref(),
    ) {
        Ok(Some(r)) => r,
        Ok(None) => {
            audit(
                state,
                "recording.get_stream",
                Some(&input.recording_ref),
                RiskClass::B,
                "denied",
                Some("not_found_or_not_owned"),
            );
            return CoreResult::Err(AbiError::NotFound.as_i32());
        }
        Err(_) => {
            audit(
                state,
                "recording.get_stream",
                Some(&input.recording_ref),
                RiskClass::B,
                "error",
                Some("db_error"),
            );
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    };
    // Enforce the ServiceCall ceiling BEFORE reading the file, accounting for
    // base64 expansion + CBOR envelope — addons calling get_stream on a
    // multi-MB file shouldn't blow host RAM only to have the response rejected
    // after the read.
    match estimate_get_stream_output(row.file_size_bytes) {
        Some(est) if est <= PayloadKind::ServiceCall.max_bytes() => {}
        _ => {
            audit(
                state,
                "recording.get_stream",
                Some(&input.recording_ref),
                RiskClass::B,
                "error",
                Some("payload_too_large"),
            );
            return CoreResult::Err(AbiError::PayloadTooLarge.as_i32());
        }
    }
    let file_path = std::path::PathBuf::from(&row.file_path);
    let bytes = match run_async(read_recording(&file_path)) {
        Ok(b) => b,
        Err(e) => {
            audit(
                state,
                "recording.get_stream",
                Some(&input.recording_ref),
                RiskClass::B,
                "error",
                Some(&format!("read_failed: {e}")),
            );
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    };
    if enforce_payload_size(bytes.len(), PayloadKind::ServiceCall).is_err() {
        audit(
            state,
            "recording.get_stream",
            Some(&input.recording_ref),
            RiskClass::B,
            "error",
            Some("payload_too_large"),
        );
        return CoreResult::Err(AbiError::PayloadTooLarge.as_i32());
    }
    audit(
        state,
        "recording.get_stream",
        Some(&input.recording_ref),
        RiskClass::B,
        "ok",
        None,
    );
    CoreResult::Ok(GetStreamOut {
        data_b64: base64::engine::general_purpose::STANDARD.encode(&bytes),
        file_size_bytes: bytes.len() as u64,
        hash_sha256: row.hash_sha256,
    })
}

fn purge_core(state: &AddonState, input: &RecordingRefInput) -> CoreResult<PurgeOut> {
    if let Err(reason) = validate_recording_ref(&input.recording_ref) {
        audit(
            state,
            "recording.purge",
            None,
            RiskClass::A,
            "denied",
            Some(reason),
        );
        return CoreResult::Err(AbiError::Operation.as_i32());
    }
    let row = match get_recording_for_addon(
        &state.db,
        &state.addon_id,
        &input.recording_ref,
        state.org_id.as_deref(),
    ) {
        Ok(Some(r)) => r,
        Ok(None) => {
            audit(
                state,
                "recording.purge",
                Some(&input.recording_ref),
                RiskClass::A,
                "denied",
                Some("not_found_or_not_owned"),
            );
            return CoreResult::Err(AbiError::NotFound.as_i32());
        }
        Err(_) => {
            audit(
                state,
                "recording.purge",
                Some(&input.recording_ref),
                RiskClass::A,
                "error",
                Some("db_error"),
            );
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    };
    let file_path = std::path::PathBuf::from(&row.file_path);
    // Honest audit: if FS removal fails, do not soft-delete the DB row and do
    // not claim success — the addon must be able to retry. `purge_recording`
    // already treats NotFound as Ok (idempotent), so a returned Err here means
    // a real I/O failure.
    if let Err(e) = run_async(purge_recording(&file_path)) {
        warn!("recording.purge file removal failed (aborting purge): {e}");
        audit(
            state,
            "recording.purge",
            Some(&input.recording_ref),
            RiskClass::A,
            "error",
            Some("purge_io_error"),
        );
        return CoreResult::Err(AbiError::Operation.as_i32());
    }
    if soft_delete_recording(
        &state.db,
        &state.addon_id,
        &input.recording_ref,
        state.org_id.as_deref(),
    )
    .is_err()
    {
        audit(
            state,
            "recording.purge",
            Some(&input.recording_ref),
            RiskClass::A,
            "error",
            Some("db_soft_delete_failed"),
        );
        return CoreResult::Err(AbiError::Operation.as_i32());
    }
    audit(
        state,
        "recording.purge",
        Some(&input.recording_ref),
        RiskClass::A,
        "ok",
        None,
    );
    CoreResult::Ok(PurgeOut { purged: true })
}

fn stats_core(state: &AddonState, input: &RecordingStatsInput) -> CoreResult<StatsOut> {
    let agg: RecordingStatsAggregate = match recording_stats_for_addon(
        &state.db,
        &state.addon_id,
        input.camera_id.as_deref(),
        state.org_id.as_deref(),
    ) {
        Ok(a) => a,
        Err(_) => {
            audit(
                state,
                "recording.stats",
                None,
                RiskClass::B,
                "error",
                Some("db_error"),
            );
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    };
    let per_camera: Vec<StatsPerCamera> = agg
        .per_camera
        .into_iter()
        .map(|r| StatsPerCamera {
            camera_id: r.camera_id,
            snapshots: r.snapshots,
            segments: r.segments,
            size_bytes: r.size_bytes,
        })
        .collect();
    audit(state, "recording.stats", None, RiskClass::B, "ok", None);
    CoreResult::Ok(StatsOut {
        stats: StatsTotals {
            total_snapshots: agg.total_snapshots,
            total_segments: agg.total_segments,
            total_size_bytes: agg.total_size_bytes,
        },
        per_camera,
    })
}

fn frame_url_core(state: &AddonState, input: &FrameUrlInput) -> CoreResult<UrlOut> {
    if let Err(reason) = validate_frame_ref(&input.frame_ref) {
        audit(
            state,
            "recording.frame_url",
            None,
            RiskClass::B,
            "denied",
            Some(reason),
        );
        return CoreResult::Err(AbiError::Operation.as_i32());
    }
    let stored = match frame_storage().get(&RawFrameRef::from_string(input.frame_ref.clone())) {
        Some(f) => f,
        None => {
            audit(
                state,
                "recording.frame_url",
                Some(&input.frame_ref),
                RiskClass::B,
                "denied",
                Some("frame_ref_not_found"),
            );
            return CoreResult::Err(AbiError::NotFound.as_i32());
        }
    };
    // Ownership: the frame's `camera_id` must resolve to a camera owned by the
    // calling addon. We swallow the DB row beyond ownership — the frame_url
    // doesn't expose camera metadata.
    match get_camera_for_addon(
        &state.db,
        &state.addon_id,
        &stored.metadata.camera_id,
        state.org_id.as_deref(),
    ) {
        Ok(Some(_)) => {}
        Ok(None) => {
            audit(
                state,
                "recording.frame_url",
                Some(&input.frame_ref),
                RiskClass::B,
                "denied",
                Some("camera_not_owned"),
            );
            return CoreResult::Err(AbiError::NotFound.as_i32());
        }
        Err(_) => {
            audit(
                state,
                "recording.frame_url",
                Some(&input.frame_ref),
                RiskClass::B,
                "error",
                Some("db_error"),
            );
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    }
    let issued = match frame_url_issuer().issue(input.frame_ref.clone(), input.ttl_secs) {
        Ok(u) => u,
        Err(e) => {
            audit(
                state,
                "recording.frame_url",
                Some(&input.frame_ref),
                RiskClass::B,
                "denied",
                Some(&format!("issue_failed: {e}")),
            );
            return CoreResult::Err(AbiError::Operation.as_i32());
        }
    };
    audit(
        state,
        "recording.frame_url",
        Some(&input.frame_ref),
        RiskClass::B,
        "ok",
        None,
    );
    let url = format!("/frames/{}?{}", input.frame_ref, issued.query_string());
    CoreResult::Ok(UrlOut {
        url,
        expires_unix_ms: issued.expiry_unix_ms,
    })
}

// =============================================================================
// Test surface — sync entry points so integration tests can drive the host
// functions without standing up a wasmtime Store. Inputs are raw CBOR bytes
// (the only allowed ABI format); outputs are raw CBOR bytes plus the ABI code.
// =============================================================================

#[doc(hidden)]
pub mod test_api {
    use super::*;

    pub fn save_snapshot_with_raw_input(state: &AddonState, raw_input: &[u8]) -> (i32, Vec<u8>) {
        run_input(
            state,
            raw_input,
            PERM_RECORDING_WRITE,
            "recording.save_snapshot",
            |s, i| save_snapshot_core(s, &i),
        )
    }

    pub fn save_segment_with_raw_input(state: &AddonState, raw_input: &[u8]) -> (i32, Vec<u8>) {
        run_input(
            state,
            raw_input,
            PERM_RECORDING_WRITE,
            "recording.save_segment",
            |s, i| save_segment_core(s, &i),
        )
    }

    pub fn get_url_with_raw_input(state: &AddonState, raw_input: &[u8]) -> (i32, Vec<u8>) {
        run_input(
            state,
            raw_input,
            PERM_RECORDING_READ,
            "recording.get_url",
            |s, i| get_url_core(s, &i),
        )
    }

    pub fn get_stream_with_raw_input(state: &AddonState, raw_input: &[u8]) -> (i32, Vec<u8>) {
        run_input(
            state,
            raw_input,
            PERM_RECORDING_READ,
            "recording.get_stream",
            |s, i| get_stream_core(s, &i),
        )
    }

    pub fn purge_with_raw_input(state: &AddonState, raw_input: &[u8]) -> (i32, Vec<u8>) {
        run_input(
            state,
            raw_input,
            PERM_RECORDING_WRITE,
            "recording.purge",
            |s, i| purge_core(s, &i),
        )
    }

    /// Stats accepts an empty payload (no filter); a non-empty payload is a
    /// CBOR `RecordingStatsInput`.
    pub fn stats_with_raw_input(state: &AddonState, raw_input: &[u8]) -> (i32, Vec<u8>) {
        if !check_permission(state, PERM_RECORDING_READ, None) {
            audit(
                state,
                "recording.stats",
                None,
                RiskClass::B,
                "denied",
                Some("missing_permission"),
            );
            return (AbiError::Permission.as_i32(), Vec::new());
        }
        if enforce_payload_size(raw_input.len(), PayloadKind::ServiceCall).is_err() {
            return (AbiError::PayloadTooLarge.as_i32(), Vec::new());
        }
        let input = if raw_input.is_empty() {
            RecordingStatsInput::default()
        } else {
            match decode_cbor_exact::<RecordingStatsInput>(raw_input) {
                Ok(v) => v,
                Err(_) => {
                    audit(
                        state,
                        "recording.stats",
                        None,
                        RiskClass::B,
                        "error",
                        Some("invalid_payload"),
                    );
                    return (AbiError::Operation.as_i32(), Vec::new());
                }
            }
        };
        finish(stats_core(state, &input))
    }

    pub fn frame_url_with_raw_input(state: &AddonState, raw_input: &[u8]) -> (i32, Vec<u8>) {
        run_input(
            state,
            raw_input,
            PERM_RECORDING_READ,
            "recording.frame_url",
            |s, i| frame_url_core(s, &i),
        )
    }

    fn run_input<I, O, F>(
        state: &AddonState,
        raw_input: &[u8],
        perm: &str,
        action: &str,
        f: F,
    ) -> (i32, Vec<u8>)
    where
        I: for<'b> minicbor::Decode<'b, ()>,
        O: minicbor::Encode<()>,
        F: FnOnce(&AddonState, I) -> CoreResult<O>,
    {
        let risk = if perm == PERM_RECORDING_WRITE {
            RiskClass::A
        } else {
            RiskClass::B
        };
        if !check_permission(state, perm, None) {
            audit(
                state,
                action,
                None,
                risk,
                "denied",
                Some("missing_permission"),
            );
            return (AbiError::Permission.as_i32(), Vec::new());
        }
        if enforce_payload_size(raw_input.len(), PayloadKind::ServiceCall).is_err() {
            return (AbiError::PayloadTooLarge.as_i32(), Vec::new());
        }
        let input: I = match decode_cbor_exact(raw_input) {
            Ok(v) => v,
            Err(_) => {
                audit(state, action, None, risk, "error", Some("invalid_payload"));
                return (AbiError::Operation.as_i32(), Vec::new());
            }
        };
        finish(f(state, input))
    }

    fn finish<O: minicbor::Encode<()>>(result: CoreResult<O>) -> (i32, Vec<u8>) {
        match result {
            CoreResult::Ok(out) => {
                let mut buf = Vec::new();
                if minicbor::encode(&out, &mut buf).is_err() {
                    return (AbiError::Operation.as_i32(), Vec::new());
                }
                if enforce_payload_size(buf.len(), PayloadKind::ServiceCall).is_err() {
                    return (AbiError::PayloadTooLarge.as_i32(), Vec::new());
                }
                (AbiError::Ok.as_i32(), buf)
            }
            CoreResult::Err(code) => (code, Vec::new()),
        }
    }
}
