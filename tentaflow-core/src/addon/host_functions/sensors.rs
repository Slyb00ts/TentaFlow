// =============================================================================
// File: addon/host_functions/sensors.rs
// Purpose: positioning-sensor publish ABIs — a device addon (e.g. a phone) pushes its
//          canonical IMU / GNSS / barometer samples to Core's per-device fusion engine
//          (LocalizationEngine → ESKF → GlobalPose → shared map georeference). Mirrors
//          lidar_publish_v1: one bounded copy out of guest memory, fixed-size binary
//          message (no JSON), audit only on deny/malformed (these run at sensor rate).
//
//          PER-SENSOR permissions (`sensor.imu` / `sensor.gps` / `sensor.baro`) so a
//          user can grant e.g. lidar+IMU but not GPS — an ungranted sensor's publish
//          is refused and the estimate simply widens (graceful degradation).
// =============================================================================

use bytes::Bytes;
use tentaflow_sdk_spec::{
    BaroSample, GnssFix, ImuSample, LidarFrameHeader, MagSample, PoseSample, BARO_SAMPLE_LEN,
    GNSS_FIX_LEN, IMU_SAMPLE_LEN,
};

use super::{
    audit_log_with_risk, check_permission, get_memory, read_guest_bytes, AddonState, WasmCaller,
    ABI_ERR_OPERATION, ABI_ERR_PERMISSION, ABI_OK,
};
use crate::audit::RiskClass;
use crate::services::lidar_hub::LidarStreamHub;
use crate::services::localization::LocalizationEngine;
use crate::services::mobile_sensors::{
    MobileSensorQueue, SENSOR_KIND_BARO, SENSOR_KIND_DEPTH, SENSOR_KIND_GNSS, SENSOR_KIND_IMU,
    SENSOR_KIND_MAG, SENSOR_KIND_POSE,
};
use crate::services::slam_scene::SlamSceneManager;

/// Per-sensor capabilities. Low risk: the addon emits data it already owns; an absent
/// grant just means that constraint never reaches the fusion engine.
const PERM_SENSOR_IMU: &str = "sensor.imu";
const PERM_SENSOR_GPS: &str = "sensor.gps";
const PERM_SENSOR_BARO: &str = "sensor.baro";
/// Depth/LiDAR frames reuse the same grant as a robot's point-cloud publish.
const PERM_LIDAR_PUBLISH: &str = "lidar.publish";
/// The device's AR pose is a camera-derived (visual-inertial) product → camera grant.
const PERM_SENSOR_CAMERA: &str = "sensor.camera";

// ----- Shared feed helpers (used by both the direct publish ABIs and the drain) -----
// Each decodes one canonical message and routes it to the right engine, keyed by the
// caller's `device_id` (addon_id == robot_id). Permission is checked by the caller.

fn feed_imu(device_id: &str, bytes: &[u8]) -> bool {
    match ImuSample::decode(bytes) {
        Some(s) => {
            LocalizationEngine::global().ingest_imu(device_id, &s);
            true
        }
        None => false,
    }
}

fn feed_gnss(device_id: &str, bytes: &[u8]) -> bool {
    match GnssFix::decode(bytes) {
        Some(f) => {
            LocalizationEngine::global().ingest_gnss(device_id, &f);
            true
        }
        None => false,
    }
}

fn feed_baro(device_id: &str, bytes: &[u8]) -> bool {
    match BaroSample::decode(bytes) {
        Some(s) => {
            LocalizationEngine::global().ingest_baro(device_id, &s);
            true
        }
        None => false,
    }
}

fn feed_mag(device_id: &str, bytes: &[u8]) -> bool {
    match MagSample::decode(bytes) {
        Some(m) => {
            LocalizationEngine::global().ingest_mag(device_id, &m);
            true
        }
        None => false,
    }
}

fn feed_pose(device_id: &str, bytes: &[u8]) -> bool {
    match PoseSample::decode(bytes) {
        Some(p) => {
            LocalizationEngine::global().ingest_pose(device_id, &p);
            true
        }
        None => false,
    }
}

/// A canonical depth/LiDAR frame: fold into the shared map (georeferenced by the
/// device's auto geo-anchor) AND publish to the LiDAR hub so the browser live view +
/// scene stream work for the phone exactly as for a robot.
fn feed_depth(device_id: &str, bytes: &[u8]) -> bool {
    let Some(header) = LidarFrameHeader::decode_header(bytes) else {
        return false;
    };
    SlamSceneManager::global().on_lidar_frame(device_id, bytes);
    LidarStreamHub::global().publish(device_id, header.frame_seq, Bytes::copy_from_slice(bytes));
    true
}

/// Largest accepted sensor message (bounds the guest copy). All current sensor
/// messages are well under this; a larger `in_len` is refused before any copy.
const MAX_SENSOR_BYTES: usize = 256;

fn audit(state: &AddonState, op: &str, result: &str, reason: Option<&str>) {
    audit_log_with_risk(
        state,
        op,
        Some("sensor"),
        Some(&state.addon_id),
        RiskClass::Unclassified,
        None,
        None,
        result,
        reason,
    );
}

/// Shared front half: permission gate + one bounded copy of exactly `expected_len`
/// bytes out of guest memory. Returns the owned bytes, or `Err(abi_code)` having
/// already audited the failure.
fn read_sensor_bytes(
    caller: &mut WasmCaller<'_, AddonState>,
    op: &str,
    perm: &str,
    expected_len: usize,
    in_ptr: i32,
    in_len: i32,
) -> Result<Vec<u8>, i32> {
    if !check_permission(caller.data(), perm, None) {
        audit(caller.data(), op, "denied", Some("missing_permission"));
        return Err(ABI_ERR_PERMISSION);
    }
    if in_len < 0 || in_len as usize > MAX_SENSOR_BYTES {
        audit(caller.data(), op, "error", Some("too_large"));
        return Err(ABI_ERR_OPERATION);
    }
    let memory = match get_memory(caller) {
        Some(m) => m,
        None => {
            audit(caller.data(), op, "error", Some("no_memory"));
            return Err(ABI_ERR_OPERATION);
        }
    };
    let bytes = match read_guest_bytes(&memory, caller, in_ptr, in_len) {
        Some(b) if b.len() >= expected_len => b[..expected_len].to_vec(),
        _ => {
            audit(caller.data(), op, "error", Some("short_or_oob"));
            return Err(ABI_ERR_OPERATION);
        }
    };
    Ok(bytes)
}

/// `imu_publish_v1(in_ptr, in_len) -> i32` — one canonical `ImuSample`.
pub fn imu_publish_v1(mut caller: WasmCaller<'_, AddonState>, in_ptr: i32, in_len: i32) -> i32 {
    let bytes = match read_sensor_bytes(
        &mut caller,
        "sensor.imu",
        PERM_SENSOR_IMU,
        IMU_SAMPLE_LEN,
        in_ptr,
        in_len,
    ) {
        Ok(b) => b,
        Err(code) => return code,
    };
    let device_id = caller.data().addon_id.clone();
    if !feed_imu(&device_id, &bytes) {
        audit(caller.data(), "sensor.imu", "error", Some("bad_sample"));
        return ABI_ERR_OPERATION;
    }
    ABI_OK
}

/// `gnss_publish_v1(in_ptr, in_len) -> i32` — one canonical `GnssFix`.
pub fn gnss_publish_v1(mut caller: WasmCaller<'_, AddonState>, in_ptr: i32, in_len: i32) -> i32 {
    let bytes = match read_sensor_bytes(
        &mut caller,
        "sensor.gps",
        PERM_SENSOR_GPS,
        GNSS_FIX_LEN,
        in_ptr,
        in_len,
    ) {
        Ok(b) => b,
        Err(code) => return code,
    };
    let device_id = caller.data().addon_id.clone();
    if !feed_gnss(&device_id, &bytes) {
        audit(caller.data(), "sensor.gps", "error", Some("bad_fix"));
        return ABI_ERR_OPERATION;
    }
    ABI_OK
}

/// `baro_publish_v1(in_ptr, in_len) -> i32` — one canonical `BaroSample`.
pub fn baro_publish_v1(mut caller: WasmCaller<'_, AddonState>, in_ptr: i32, in_len: i32) -> i32 {
    let bytes = match read_sensor_bytes(
        &mut caller,
        "sensor.baro",
        PERM_SENSOR_BARO,
        BARO_SAMPLE_LEN,
        in_ptr,
        in_len,
    ) {
        Ok(b) => b,
        Err(code) => return code,
    };
    let device_id = caller.data().addon_id.clone();
    if !feed_baro(&device_id, &bytes) {
        audit(caller.data(), "sensor.baro", "error", Some("bad_sample"));
        return ABI_ERR_OPERATION;
    }
    ABI_OK
}

/// `mobile_sensor_drain_v1() -> i32` — drain the native MobileSensorQueue into the
/// fusion engine + shared map, called from the phone addon's tick. Each sample's kind
/// is gated by THIS addon's per-sensor permission (so "lidar yes, camera no, GPS no"
/// is enforced here): an ungranted kind is dropped, never fed. Returns the count fed.
/// Device id = the caller's addon_id (== the robot_id the same addon advertises).
pub fn mobile_sensor_drain_v1(caller: WasmCaller<'_, AddonState>) -> i32 {
    let device_id = caller.data().addon_id.clone();
    let samples = MobileSensorQueue::global().drain();
    let mut fed = 0i32;
    for (kind, bytes) in samples {
        let ok = match kind {
            SENSOR_KIND_IMU => {
                check_permission(caller.data(), PERM_SENSOR_IMU, None)
                    && feed_imu(&device_id, &bytes)
            }
            SENSOR_KIND_GNSS => {
                check_permission(caller.data(), PERM_SENSOR_GPS, None)
                    && feed_gnss(&device_id, &bytes)
            }
            SENSOR_KIND_BARO => {
                check_permission(caller.data(), PERM_SENSOR_BARO, None)
                    && feed_baro(&device_id, &bytes)
            }
            SENSOR_KIND_DEPTH => {
                check_permission(caller.data(), PERM_LIDAR_PUBLISH, None)
                    && feed_depth(&device_id, &bytes)
            }
            SENSOR_KIND_POSE => {
                check_permission(caller.data(), PERM_SENSOR_CAMERA, None)
                    && feed_pose(&device_id, &bytes)
            }
            // Magnetometer rides the IMU grant (an orientation/inertial aid).
            SENSOR_KIND_MAG => {
                check_permission(caller.data(), PERM_SENSOR_IMU, None)
                    && feed_mag(&device_id, &bytes)
            }
            _ => false,
        };
        if ok {
            fed += 1;
        }
    }
    fed
}
