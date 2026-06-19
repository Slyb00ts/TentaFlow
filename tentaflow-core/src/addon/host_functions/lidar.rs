// =============================================================================
// File: addon/host_functions/lidar.rs
// Purpose: lidar_publish_v1 host ABI — a robot-driver addon publishes ONE
//          canonical, vendor-agnostic point-cloud frame (packed f32, the
//          tentaflow-sdk-spec `LidarFrameHeader` layout) to Core. Core stays a
//          dumb pipe: it validates the header, bounds the size, and hands the
//          frame to the `LidarStreamHub` keyed by the caller's `addon_id`
//          (== robot_id; one robot per install). No JSON, one WASM->host copy
//          per frame.
// =============================================================================

use bytes::Bytes;
use tentaflow_sdk_spec::LidarFrameHeader;

use super::{
    audit_log_with_risk, check_permission, get_memory, read_guest_bytes, AddonState, WasmCaller,
    ABI_ERR_OPERATION, ABI_ERR_PERMISSION, ABI_OK,
};
use crate::audit::RiskClass;
use crate::services::lidar_hub::LidarStreamHub;

/// Capability a robot addon declares to publish its OWN sensor frames to Core.
/// Low risk: the addon emits data it already owns; it never reads other addons'
/// frames through this fn (the hub is keyed by the caller's own addon_id).
const PERM_LIDAR_PUBLISH: &str = "lidar.publish";

/// Hard cap on a single published canonical frame (header + packed f32 body).
/// At LIDAR_LAYOUT_XYZI that is ~262k points; the Go2 voxel map is far sparser
/// (a few 10k points). A larger frame is rejected before any copy out of guest
/// memory so a malformed length cannot make Core allocate without bound.
const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

fn audit(state: &AddonState, result: &str, reason: Option<&str>) {
    audit_log_with_risk(
        state,
        "lidar.publish",
        Some("lidar"),
        Some(&state.addon_id),
        RiskClass::Unclassified,
        None,
        None,
        result,
        reason,
    );
}

/// Host function: publish ONE canonical LiDAR frame.
///
/// ABI: `lidar_publish_v1(in_ptr, in_len) -> i32`. `in_ptr/in_len` point at the
/// packed canonical frame in guest memory: a `LidarFrameHeader` followed by
/// `point_count * stride` little-endian f32. Returns `ABI_OK`, or an error code
/// on a missing permission / oversized / malformed frame.
///
/// INVARIANT: exactly one bounded copy from guest memory per frame; the bulk is
/// packed f32 (never CBOR/JSON); the header self-describes the body length so a
/// truncated or inconsistent frame is rejected, never partially stored. The frame
/// is keyed in the hub by `addon_id` (== robot_id): one robot per install, so the
/// service instance is the single publisher and a consumer finds the frame by the
/// stable robot_id, not the per-run instance UUID.
pub fn lidar_publish_v1(mut caller: WasmCaller<'_, AddonState>, in_ptr: i32, in_len: i32) -> i32 {
    if !check_permission(caller.data(), PERM_LIDAR_PUBLISH, None) {
        audit(caller.data(), "denied", Some("missing_permission"));
        return ABI_ERR_PERMISSION;
    }

    if in_len < 0 || in_len as usize > MAX_FRAME_BYTES {
        audit(caller.data(), "error", Some("frame_too_large"));
        return ABI_ERR_OPERATION;
    }

    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => {
            audit(caller.data(), "error", Some("no_memory"));
            return ABI_ERR_OPERATION;
        }
    };

    // Validate the header and confirm the declared body length matches the
    // supplied buffer BEFORE retaining anything — never store a partial frame.
    let header = {
        let bytes = match read_guest_bytes(&memory, &caller, in_ptr, in_len) {
            Some(b) => b,
            None => {
                audit(caller.data(), "error", Some("oob_read"));
                return ABI_ERR_OPERATION;
            }
        };
        let header = match LidarFrameHeader::decode_header(bytes) {
            Some(h) => h,
            None => {
                audit(caller.data(), "error", Some("bad_header"));
                return ABI_ERR_OPERATION;
            }
        };
        match header.frame_len() {
            Some(expected) if expected == bytes.len() => {}
            _ => {
                audit(caller.data(), "error", Some("length_mismatch"));
                return ABI_ERR_OPERATION;
            }
        }
        header
    };

    // Single copy out of guest memory into an owned Bytes (the retained frame).
    let frame: Bytes = {
        let bytes = match read_guest_bytes(&memory, &caller, in_ptr, in_len) {
            Some(b) => b,
            None => {
                audit(caller.data(), "error", Some("oob_read"));
                return ABI_ERR_OPERATION;
            }
        };
        Bytes::copy_from_slice(bytes)
    };

    // Success path is lock-light and DB-free: this fn runs at frame rate from the
    // robot drain tick, so it must NOT take the audit/DB writer lock or grow
    // audit_log per frame. Audit rows are written only for the rare, security-
    // relevant deny + malformed-frame cases above. Hand the single owned copy to
    // the latest-wins hub keyed by addon_id (== robot_id) and return.
    let addon_id = caller.data().addon_id.clone();
    LidarStreamHub::global().publish(&addon_id, header.frame_seq, frame);
    ABI_OK
}
