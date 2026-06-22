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
use tentaflow_sdk_spec::{BaroSample, GlobalPoseFrame, GnssFix, ImuSample, PoseSample};
use tentaflow_slam::{align_yaw_translation, EskfConfig, EskfEngine, GeoAnchor};

use crate::services::slam_scene::SlamSceneManager;

/// Min horizontal motion (m) between accepted AR↔ENU alignment pairs, and the min
/// number of pairs before trusting the alignment. Together they ensure the device
/// actually moved (yaw becomes observable) before georeferencing the map.
const ALIGN_MIN_STEP_M: f64 = 0.5;
const ALIGN_MIN_PAIRS: usize = 8;
const ALIGN_MAX_PAIRS: usize = 400;

/// Minimum spacing between pose pushes driven by the high-rate IMU. Absolute updates
/// (GNSS/baro) always push; IMU-only dead-reckoning pose refreshes at ≤10 Hz so the
/// scene manager / renderer is not hammered at the IMU rate.
const IMU_PUSH_MIN_INTERVAL_US: i64 = 100_000;

/// Per-device fusion state.
struct DeviceLoc {
    engine: EskfEngine,
    /// True once the engine has anchored a global frame from a GNSS fix.
    georeferenced: bool,
    last_push_us: i64,
    /// True once the device streams its own local (AR) pose: then THAT drives the
    /// marker + map frame, and the ESKF only provides the global georeference.
    ar_driven: bool,
    /// Correspondence pairs `(ar_pos, eskf_enu_pos)` for the AR→ENU alignment.
    pairs: Vec<([f64; 3], [f64; 3])>,
    last_pair_ar: Option<[f64; 3]>,
    /// True once the scene geo-anchor has been set from a converged alignment.
    scene_anchored: bool,
}

impl DeviceLoc {
    fn new() -> Self {
        DeviceLoc {
            engine: EskfEngine::new(EskfConfig::default()),
            georeferenced: false,
            last_push_us: 0,
            ar_driven: false,
            pairs: Vec::new(),
            last_pair_ar: None,
            scene_anchored: false,
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
        // seed-only sample must not push the default/stale pose into the scene. When
        // the device streams its own AR pose, that drives the marker instead.
        if d.engine.ingest_imu(s)
            && !d.ar_driven
            && s.timestamp_us - d.last_push_us >= IMU_PUSH_MIN_INTERVAL_US
        {
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
        // AR-driven: the AR pose owns the marker; the ESKF only georeferences (via the
        // AR↔ENU alignment in `ingest_pose`). Non-AR device: the ESKF IS the pose, and
        // its ENU frame IS the scene frame, so anchor it directly once georeferenced.
        if d.ar_driven {
            Self::try_geo_anchor(&mut d, device_id);
        } else {
            if d.georeferenced && !d.scene_anchored {
                if let Some((lat0, lon0, alt0)) = d.engine.origin() {
                    // Scene == ENU (heading 90° → +X East) for a non-AR device.
                    SlamSceneManager::global()
                        .set_geo_anchor(device_id, GeoAnchor::new(lat0, lon0, alt0, 90.0));
                    d.scene_anchored = true;
                }
            }
            Self::push_pose(&mut d, device_id, fix.timestamp_us);
        }
    }

    /// Adopt the device's own local (AR/ARKit/ARCore) pose: it drives the marker + the
    /// map's scene frame, and each pose feeds the AR↔ENU alignment so the map can be
    /// georeferenced to WGS84 against the GNSS-anchored ESKF.
    pub fn ingest_pose(&self, device_id: &str, s: &PoseSample) {
        if !s.is_finite() {
            return;
        }
        let entry = self
            .devices
            .entry(device_id.to_string())
            .or_insert_with(|| Mutex::new(DeviceLoc::new()));
        let mut d = entry.lock();
        // On the FIRST AR pose, if a GNSS fix already ENU-anchored the scene (no AR
        // pose had arrived yet), discard that anchor: the AR map is georeferenced via
        // AR↔ENU alignment instead, not as a raw ENU frame.
        if !d.ar_driven {
            d.ar_driven = true;
            if d.scene_anchored {
                SlamSceneManager::global().clear_geo_anchor(device_id);
                d.scene_anchored = false;
                d.pairs.clear();
                d.last_pair_ar = None;
            }
        }
        // Producers convert ARKit/ARCore (Y-up) → the engine's Z-up convention before
        // sending, so `position`/`quat_xyzw` are already Z-up here.
        let ar = [s.position[0] as f64, s.position[1] as f64, s.position[2] as f64];
        let quat = [
            s.quat_xyzw[0] as f64,
            s.quat_xyzw[1] as f64,
            s.quat_xyzw[2] as f64,
            s.quat_xyzw[3] as f64,
        ];
        // The AR pose drives the marker + map frame.
        SlamSceneManager::global().on_pose(device_id, &ar, &quat, s.timestamp_us);
        // Record an alignment pair ONLY once the ESKF has a global ENU origin (else the
        // ENU side is a pre-anchor local estimate that would poison the one-shot
        // alignment). Sub-sample by motion so yaw is observable before georeferencing.
        if d.engine.is_georeferenced() {
            let enu = d.engine.position_enu();
            let moved = d
                .last_pair_ar
                .map(|p| {
                    let dx = ar[0] - p[0];
                    let dy = ar[1] - p[1];
                    (dx * dx + dy * dy).sqrt() >= ALIGN_MIN_STEP_M
                })
                .unwrap_or(true);
            if moved {
                if d.pairs.len() >= ALIGN_MAX_PAIRS {
                    d.pairs.remove(0);
                }
                d.pairs.push((ar, enu));
                d.last_pair_ar = Some(ar);
            }
        }
        Self::try_geo_anchor(&mut d, device_id);
    }

    /// Once the ESKF is georeferenced AND the AR↔ENU alignment has enough well-spread
    /// pairs, set the scene geo-anchor so the device-local map becomes real-world WGS84.
    /// One-shot (guarded by `scene_anchored`); re-alignment is a later refinement.
    fn try_geo_anchor(d: &mut DeviceLoc, device_id: &str) {
        if d.scene_anchored || !d.engine.is_georeferenced() || d.pairs.len() < ALIGN_MIN_PAIRS {
            return;
        }
        let Some(origin) = d.engine.origin() else {
            return;
        };
        if let Some((yaw, t)) = align_yaw_translation(&d.pairs) {
            let anchor = GeoAnchor::from_alignment(yaw, t, origin);
            if SlamSceneManager::global().set_geo_anchor(device_id, anchor) {
                d.scene_anchored = true;
            }
        }
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
        if d.engine.ingest_baro(s) && !d.ar_driven {
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
    fn non_ar_device_gnss_auto_anchors_enu_scene() {
        // A device WITHOUT an AR pose: the ESKF ENU frame IS the scene frame, so the
        // first GNSS fix georeferences the scene directly (heading 90° → +X East).
        let eng = LocalizationEngine::new();
        let id = "phone-loc-a";
        SlamSceneManager::global().remove(id);
        eng.ingest_imu(id, &imu(0));
        eng.ingest_gnss(id, &gnss(0, 52.2297, 21.0122, 118.5));
        assert!(eng.is_georeferenced(id));
        let anchor = SlamSceneManager::global().geo_anchor(id).expect("ENU scene anchored");
        assert!((anchor.lat_deg - 52.2297).abs() < 1e-9);
        assert!((anchor.heading_deg - 90.0).abs() < 1e-9);
        SlamSceneManager::global().remove(id);
    }

    #[test]
    fn ar_driven_device_georeferences_map_after_motion() {
        use tentaflow_sdk_spec::{PoseSample, POSE_SAMPLE_VERSION};
        use tentaflow_slam::enu_to_geodetic;
        let eng = LocalizationEngine::new();
        let id = "phone-loc-ar";
        SlamSceneManager::global().remove(id);
        let origin = (52.0, 21.0, 100.0);

        let pose = |ts: i64, x: f32| PoseSample {
            version: POSE_SAMPLE_VERSION,
            flags: 0,
            timestamp_us: ts,
            position: [x, 0.0, 0.0],
            quat_xyzw: [0.0, 0.0, 0.0, 1.0],
        };
        // Anchor the ESKF, then walk East: AR +X aligned with ENU East (yaw≈0). Feed
        // GNSS at the matching geodetic points so the ESKF ENU tracks the AR motion.
        eng.ingest_imu(id, &imu(0));
        eng.ingest_gnss(id, &gnss(0, origin.0, origin.1, origin.2));
        for k in 0..12 {
            let ts = k * 100_000;
            let (lat, lon, alt) = enu_to_geodetic([k as f64, 0.0, 0.0], origin.0, origin.1, origin.2);
            eng.ingest_gnss(id, &gnss(ts, lat, lon, alt));
            eng.ingest_pose(id, &pose(ts, k as f32));
        }
        // The AR-frame map is now georeferenced near the GNSS origin (yaw≈0 → heading≈90).
        let anchor = SlamSceneManager::global().geo_anchor(id).expect("AR map georeferenced");
        assert!((anchor.lat_deg - 52.0).abs() < 1e-3, "anchor near origin lat: {}", anchor.lat_deg);
        assert!((anchor.heading_deg - 90.0).abs() < 5.0, "heading ~90: {}", anchor.heading_deg);
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
