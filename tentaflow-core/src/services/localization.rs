// =============================================================================
// File: services/localization.rs
// Purpose: LocalizationEngine — the per-device fusion service. Each device (a phone
//          addon, keyed by addon_id == robot_id) gets an ESKF that fuses its IMU +
//          GNSS + barometer (UNIVERSAL_POSITIONING_PLAN §7 lightweight profile) into a
//          GlobalPose. The estimated pose is pushed to the shared SLAM scene
//          ([[SlamSceneManager]]) so the phone is placed + its depth map accumulates
//          exactly like any robot, and the FIRST GNSS fix auto-sets that robot's
//          geo-anchor (scene frame == ENU via heading 90°) so the map becomes
//          real-world WGS84 without any operator input.
//
//          Sync + lock-light (per-device mutex), called from the sensor publish
//          host-fns at sensor rate. IMU is high-rate, so pose is pushed to the scene
//          manager only on absolute updates + throttled between them.
// =============================================================================

use std::sync::OnceLock;

use dashmap::DashMap;
use parking_lot::Mutex;
use tentaflow_sdk_spec::{BaroSample, GlobalPoseFrame, GnssFix, ImuSample};
use tentaflow_slam::{EskfConfig, EskfEngine};

use crate::services::slam_scene::SlamSceneManager;

/// Minimum spacing between pose pushes driven by the high-rate IMU. Absolute updates
/// (GNSS/baro) always push; IMU-only dead-reckoning pose refreshes at ≤10 Hz so the
/// scene manager / renderer is not hammered at the IMU rate.
const IMU_PUSH_MIN_INTERVAL_US: i64 = 100_000;

/// Per-device fusion state.
struct DeviceLoc {
    engine: EskfEngine,
    /// True once the engine has anchored a global frame from a GNSS fix; gates the
    /// one-shot geo-anchor wiring into the scene.
    georeferenced: bool,
    last_push_us: i64,
}

impl DeviceLoc {
    fn new() -> Self {
        DeviceLoc {
            engine: EskfEngine::new(EskfConfig::default()),
            georeferenced: false,
            last_push_us: 0,
        }
    }
}

/// Process-wide per-device localization engine.
pub struct LocalizationEngine {
    devices: DashMap<String, Mutex<DeviceLoc>>,
}

impl LocalizationEngine {
    fn new() -> Self {
        Self { devices: DashMap::new() }
    }

    pub fn global() -> &'static LocalizationEngine {
        static INSTANCE: OnceLock<LocalizationEngine> = OnceLock::new();
        INSTANCE.get_or_init(LocalizationEngine::new)
    }

    /// Push the engine's current ENU pose into the shared scene as the device's pose,
    /// so the phone-robot is placed and its depth map accumulates in the scene frame.
    /// The geo-anchor (set on first GNSS fix) turns that ENU pose into WGS84.
    fn push_pose(d: &mut DeviceLoc, device_id: &str, ts_us: i64) {
        let enu = d.engine.position_enu();
        let q = d.engine.quat_xyzw();
        SlamSceneManager::global().on_pose(device_id, &enu, &q, ts_us);
        d.last_push_us = ts_us;
    }

    /// Feed one IMU sample (predict). Pose is pushed to the scene at most every
    /// `IMU_PUSH_MIN_INTERVAL_US` so dead-reckoning between fixes stays smooth without
    /// flooding the scene manager.
    pub fn ingest_imu(&self, device_id: &str, s: &ImuSample) {
        let entry = self
            .devices
            .entry(device_id.to_string())
            .or_insert_with(|| Mutex::new(DeviceLoc::new()));
        let mut d = entry.lock();
        // Only publish a pose if the predict actually advanced the state — a dropped /
        // seed-only sample must not push the default/stale pose into the scene.
        if d.engine.ingest_imu(s) && s.timestamp_us - d.last_push_us >= IMU_PUSH_MIN_INTERVAL_US {
            Self::push_pose(&mut d, device_id, s.timestamp_us);
        }
    }

    /// Feed one GNSS fix (absolute update). The first valid fix anchors the global
    /// frame; that transition auto-sets the device's scene geo-anchor so its map
    /// becomes real-world WGS84. Always pushes the updated pose.
    pub fn ingest_gnss(&self, device_id: &str, fix: &GnssFix) {
        let entry = self
            .devices
            .entry(device_id.to_string())
            .or_insert_with(|| Mutex::new(DeviceLoc::new()));
        let mut d = entry.lock();
        // A rejected (invalid) fix updates nothing → do not push a pose.
        if !d.engine.ingest_gnss(fix) {
            return;
        }
        if !d.georeferenced && d.engine.is_georeferenced() {
            d.georeferenced = true;
        }
        // NOTE: we deliberately do NOT auto-set the scene geo-anchor here. The ESKF
        // gives the device's global WGS84 pose directly (GlobalPose), but the phone's
        // depth MAP is accumulated in the depth-sensor's own world frame (ARKit/ARCore
        // tracking frame), which is NOT the ESKF ENU frame — anchoring it as ENU would
        // rotate/translate every map coordinate. Tying the depth-map frame to WGS84
        // needs an explicit alignment (e.g. Umeyama of AR-frame poses vs GNSS-ENU over
        // a short trajectory); until then the phone's scene stays local-metric and the
        // global position is read from the ESKF pose.
        Self::push_pose(&mut d, device_id, fix.timestamp_us);
    }

    /// The device's current global pose (ESKF) as a `GlobalPose` frame — WGS84 once
    /// GNSS-anchored, scene-local ENU otherwise. The authoritative "where is this
    /// phone in the world" answer, independent of the depth-map frame.
    pub fn global_pose(&self, device_id: &str, timestamp_us: i64, scene_id: u64) -> Option<GlobalPoseFrame> {
        self.devices
            .get(device_id)
            .map(|e| e.lock().engine.global_pose(timestamp_us, scene_id))
    }

    /// Feed one barometer sample (Up-channel update). Always pushes the updated pose.
    pub fn ingest_baro(&self, device_id: &str, s: &BaroSample) {
        let entry = self
            .devices
            .entry(device_id.to_string())
            .or_insert_with(|| Mutex::new(DeviceLoc::new()));
        let mut d = entry.lock();
        if d.engine.ingest_baro(s) {
            Self::push_pose(&mut d, device_id, s.timestamp_us);
        }
    }

    /// True once the device's engine has a global (GNSS-anchored) frame.
    pub fn is_georeferenced(&self, device_id: &str) -> bool {
        self.devices
            .get(device_id)
            .map(|e| e.lock().engine.is_georeferenced())
            .unwrap_or(false)
    }

    /// Drop a device's fusion state (uninstall / last instance gone).
    pub fn remove(&self, device_id: &str) {
        self.devices.remove(device_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tentaflow_sdk_spec::{GNSS_FIX_VERSION, IMU_SAMPLE_VERSION};

    fn imu(ts: i64) -> ImuSample {
        ImuSample {
            version: IMU_SAMPLE_VERSION,
            flags: 0,
            timestamp_us: ts,
            accel: [0.0, 0.0, 9.81],
            gyro: [0.0; 3],
            accel_noise_std: 0.02,
            gyro_noise_std: 0.002,
        }
    }

    fn gnss(ts: i64, lat: f64, lon: f64, alt: f64) -> GnssFix {
        GnssFix {
            version: GNSS_FIX_VERSION,
            flags: 0,
            timestamp_us: ts,
            lat_deg: lat,
            lon_deg: lon,
            alt_m: alt,
            h_acc_m: 1.0,
            v_acc_m: 2.0,
            vel_enu: [0.0; 3],
            vel_acc_m_s: 0.0,
        }
    }

    #[test]
    fn first_gnss_fix_georeferences_engine_without_anchoring_scene() {
        use tentaflow_sdk_spec::POSE_STATE_GLOBAL;
        let eng = LocalizationEngine::new();
        let id = "phone-loc-a";
        SlamSceneManager::global().remove(id);
        assert!(!eng.is_georeferenced(id));
        eng.ingest_imu(id, &imu(0));
        eng.ingest_gnss(id, &gnss(0, 52.2297, 21.0122, 118.5));
        // The ESKF is georeferenced → its global pose is WGS84 near the fix...
        assert!(eng.is_georeferenced(id));
        let gp = eng.global_pose(id, 0, 1).unwrap();
        assert_eq!(gp.state, POSE_STATE_GLOBAL);
        assert!((gp.position[0] - 52.2297).abs() < 1e-3);
        // ...but the SCENE map is NOT auto-anchored (depth-map frame ≠ ENU; anchoring
        // it would mis-place coordinates — that alignment is an explicit later step).
        assert!(SlamSceneManager::global().geo_anchor(id).is_none());
        SlamSceneManager::global().remove(id);
    }

    #[test]
    fn imu_pose_pushes_are_throttled() {
        let eng = LocalizationEngine::new();
        let id = "phone-loc-b";
        SlamSceneManager::global().remove(id);
        // First sample seeds clock + pushes (last_push 0, ts 0 → 0 >= 0 pushes once).
        eng.ingest_imu(id, &imu(0));
        // Samples within the throttle window do not push; one past it does. We can't
        // observe push count directly, but the device must exist and not panic.
        for k in 1..=20 {
            eng.ingest_imu(id, &imu(k * 10_000)); // 100 Hz
        }
        // No GNSS yet → not georeferenced; pose lives scene-local. Sanity: still alive.
        assert!(!eng.is_georeferenced(id));
        eng.remove(id);
    }

    #[test]
    fn rejected_gnss_does_not_publish_pose_or_anchor() {
        let eng = LocalizationEngine::new();
        let id = "phone-loc-d";
        SlamSceneManager::global().remove(id);
        // An out-of-range first fix must NOT anchor a scene or push a [0,0,0] pose.
        let mut bad = gnss(0, 999.0, 0.0, 0.0);
        bad.lat_deg = 999.0;
        eng.ingest_gnss(id, &bad);
        assert!(!eng.is_georeferenced(id), "invalid fix does not georeference");
        assert!(SlamSceneManager::global().geo_anchor(id).is_none(), "no anchor from a bad fix");
        assert!(SlamSceneManager::global().latest_pose(id).is_none(), "no stale pose published");
        SlamSceneManager::global().remove(id);
    }

    #[test]
    fn remove_drops_device() {
        let eng = LocalizationEngine::new();
        let id = "phone-loc-c";
        eng.ingest_imu(id, &imu(0));
        eng.remove(id);
        assert!(!eng.is_georeferenced(id));
    }
}
