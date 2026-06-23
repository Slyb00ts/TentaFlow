// =============================================================================
// File: services/camera_ingest/depth_mapping.rs — camera → metric depth → SLAM
// =============================================================================
//
// Per-camera depth-mapping loop. For a camera with depth mapping enabled (and a
// robot bound), it pulls the latest decoded RGB frame from the SAME stream
// TentaVision uses (Branch A frame mailbox, no extra capture), runs it through a
// metric depth model exposed as a local `depth` service (POST /v1/depth), back-
// projects the metric depth map into a 3-D point cloud via pinhole intrinsics
// derived from the camera's horizontal FOV, transforms the cloud into the robot's
// SCENE frame using its latest pose, and folds it into the shared SLAM map
// (`SlamSceneManager::on_lidar_frame`) — exactly the path a real LiDAR uses, so a
// phone's photos build the same map as a robot's LiDAR.
//
// The loop is paced far slower than detection (depth inference is heavy) and is a
// strict best-effort: a missing pose, no running depth service, a non-metric
// model, or a transient HTTP error simply skips that tick — the camera session
// and dashboard are never affected.

#![cfg(feature = "camera")]

use std::io::Cursor;
use std::sync::OnceLock;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine};
use dashmap::DashMap;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use tentaflow_sdk_spec::{
    LidarFrameHeader, LIDAR_FRAME_VERSION, LIDAR_HEADER_LEN, LIDAR_LAYOUT_XYZ,
};
use tentaflow_slam::Pose;

use crate::db::repository::DepthMappingConfig;
use crate::services::slam_scene::SlamSceneManager;

/// Voxel grid resolution (m) the depth cloud dedups into — matches the Go2 LiDAR
/// grid so a phone's map and a robot's map share one cell size.
const MAP_RESOLUTION_M: f32 = 0.05;

/// Drop depth samples beyond this range (m). Monocular metric models grow noisy
/// far out; near walls/objects carry the useful structure for an indoor map.
const MAX_DEPTH_M: f32 = 12.0;

/// Pixel stride when sampling the depth map. A dense map (e.g. 504×378 ≈ 190k px)
/// is decimated to keep the per-frame cloud near a LiDAR's point budget; the
/// voxel grid dedups the rest anyway.
const PIXEL_STRIDE: usize = 3;

/// Registry of running per-camera loops, so `ensure_depth_mapping` is idempotent
/// and `stop`/`drain` can abort cleanly.
fn registry() -> &'static DashMap<String, JoinHandle<()>> {
    static REG: OnceLock<DashMap<String, JoinHandle<()>>> = OnceLock::new();
    REG.get_or_init(DashMap::new)
}

/// Start the depth-mapping loop for a camera IF mapping is enabled for it (and a
/// robot is bound). Idempotent: a no-op when mapping is off or a loop is already
/// running. Called at camera hydrate and on pushed-camera registration.
pub fn ensure_depth_mapping(camera_id: &str) {
    let Some(pool) = crate::db::global_pool() else {
        return;
    };
    let cfg = match crate::db::repository::camera_depth_mapping_config(&pool, camera_id) {
        Ok(Some(c)) => c,
        Ok(None) => return,
        Err(e) => {
            warn!("[depth_mapping] config read failed for {camera_id}: {e}");
            return;
        }
    };
    // Atomic check-and-spawn: hold the entry across the liveness check + insert so
    // two concurrent calls (hydrate racing pushed-camera re-register) can't both
    // spawn a loop and leak the loser as an untracked duplicate.
    use dashmap::mapref::entry::Entry;
    match registry().entry(camera_id.to_string()) {
        Entry::Occupied(mut e) => {
            if !e.get().is_finished() {
                return; // a live loop already owns this camera
            }
            info!(
                "[depth_mapping] restarting loop camera={} robot={} fps={} fov={:.0}",
                cfg.camera_id, cfg.robot_id, cfg.fps, cfg.fov_deg
            );
            e.insert(tokio::spawn(run_loop(cfg)));
        }
        Entry::Vacant(e) => {
            info!(
                "[depth_mapping] starting loop camera={} robot={} fps={} fov={:.0}",
                cfg.camera_id, cfg.robot_id, cfg.fps, cfg.fov_deg
            );
            e.insert(tokio::spawn(run_loop(cfg)));
        }
    }
}

/// Stop one camera's depth-mapping loop (camera removed / mapping disabled).
pub fn stop_depth_mapping(camera_id: &str) {
    if let Some((_, h)) = registry().remove(camera_id) {
        h.abort();
    }
}

/// Abort every depth-mapping loop (process shutdown). Mirrors `vision_analysis::drain`.
pub fn drain() {
    let keys: Vec<String> = registry().iter().map(|e| e.key().clone()).collect();
    for k in keys {
        if let Some((_, h)) = registry().remove(&k) {
            h.abort();
        }
    }
}

async fn run_loop(initial: DepthMappingConfig) {
    let camera_id = initial.camera_id.clone();
    let interval = Duration::from_millis((1000 / initial.fps.max(1)) as u64);
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Bound each request so a hung depth endpoint skips the tick instead of
    // wedging the loop forever (inference is heavy but should finish in seconds).
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let mut seq: u32 = 0;

    loop {
        tick.tick().await;

        // Re-read config each tick so live edits (the FOV calibration knob, robot
        // binding, disable) take effect WITHOUT a restart — the calibration workflow
        // depends on tuning FOV against the lidar cloud in real time. `None` ⇒ mapping
        // was disabled or the camera removed ⇒ exit the loop (frees the registry slot;
        // `ensure_depth_mapping` restarts it if re-enabled). `fps` change needs a
        // re-enable (the tick interval is fixed at spawn).
        let cfg = match crate::db::global_pool()
            .and_then(|p| crate::db::repository::camera_depth_mapping_config(&p, &camera_id).ok())
            .flatten()
        {
            Some(c) => c,
            None => return,
        };

        // A pose is mandatory: without the camera's scene pose the cloud cannot be
        // placed in the map frame. Uses the POSE SOURCE robot (may differ from the
        // store id during calibration). Skip quietly until that robot is localized.
        let Some(pose) = SlamSceneManager::global().latest_scene_pose(&cfg.pose_robot_id) else {
            continue;
        };
        let Some((rgb, w, h)) =
            crate::addon::host_functions::camera::latest_frame_global(&cfg.camera_id).await
        else {
            continue;
        };
        let Some(endpoint) = resolve_depth_endpoint() else {
            continue;
        };
        let Some(jpeg) = encode_jpeg(&rgb, w, h) else {
            continue;
        };

        let depth = match request_depth(&client, &endpoint, &jpeg).await {
            Ok(d) => d,
            Err(e) => {
                debug!("[depth_mapping] depth request failed ({}): {e}", cfg.camera_id);
                continue;
            }
        };
        if !depth.is_metric {
            // A non-metric model can't produce a lidar-compatible (metre-scaled) map.
            // Stop the loop entirely rather than burn GPU on results we always discard;
            // deploying a metric model + re-registering the camera restarts it (the
            // finished handle lets `ensure_depth_mapping` spawn a fresh loop).
            warn!(
                "[depth_mapping] depth service model is not metric — stopping mapping for {} \
                 (deploy a *-Metric-* / ZoeDepth model to build a map from this camera)",
                cfg.camera_id
            );
            return;
        }

        let points = backproject_to_scene(&depth, cfg.fov_deg, &pose);
        if points.is_empty() {
            continue;
        }
        seq = seq.wrapping_add(1);
        let frame = encode_lidar_frame(&points, MAP_RESOLUTION_M, seq, depth_timestamp_us());
        SlamSceneManager::global().on_lidar_frame(&cfg.robot_id, &frame);
    }
}

/// Decoded depth response: a metric (or not) depth map sized `width × height`.
struct DepthMap {
    width: u32,
    height: u32,
    is_metric: bool,
    /// Row-major depth values (metres when `is_metric`).
    depth: Vec<f32>,
}

/// Resolve a running local `depth` service endpoint (Running first, then Degraded),
/// mirroring `web_research::resolve_local_service_endpoint`. `None` when none is up.
fn resolve_depth_endpoint() -> Option<String> {
    let pool = crate::db::global_pool()?;
    let conn = pool.read().ok()?;
    let services = crate::services_repo::services::list_all(&conn).ok()?;
    use crate::services_repo::services::ServiceStatus;
    services
        .iter()
        .find(|s| {
            s.engine_id == "depth"
                && !s.paused
                && s.status == ServiceStatus::Running
                && s.endpoint_url.is_some()
        })
        .or_else(|| {
            services.iter().find(|s| {
                s.engine_id == "depth"
                    && !s.paused
                    && s.status == ServiceStatus::Degraded
                    && s.endpoint_url.is_some()
            })
        })
        .and_then(|s| s.endpoint_url.clone())
}

/// Encode an RGB24 buffer as JPEG (small payload, lossless detail is irrelevant
/// for depth). `None` on a size mismatch or encoder error.
fn encode_jpeg(rgb: &[u8], w: u32, h: u32) -> Option<Vec<u8>> {
    if w == 0 || h == 0 || rgb.len() != (w as usize) * (h as usize) * 3 {
        return None;
    }
    let buf = image::RgbImage::from_raw(w, h, rgb.to_vec())?;
    let mut out = Vec::new();
    image::DynamicImage::ImageRgb8(buf)
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Jpeg)
        .ok()?;
    Some(out)
}

/// POST a JPEG to the depth service `/v1/depth` and parse the f32 depth map.
async fn request_depth(
    client: &reqwest::Client,
    endpoint: &str,
    jpeg: &[u8],
) -> Result<DepthMap, String> {
    let url = format!("{}/v1/depth", endpoint.trim_end_matches('/'));
    let data_url = format!("data:image/jpeg;base64,{}", STANDARD.encode(jpeg));
    let body = serde_json::json!({
        "input": [{ "type": "image_url", "url": data_url }],
    });
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("status {}", resp.status()));
    }
    let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let item = v
        .get("data")
        .and_then(|d| d.get(0))
        .ok_or_else(|| "missing data[0]".to_string())?;
    let width = item.get("width").and_then(|x| x.as_u64()).ok_or("no width")? as u32;
    let height = item.get("height").and_then(|x| x.as_u64()).ok_or("no height")? as u32;
    let is_metric = item.get("is_metric").and_then(|x| x.as_bool()).unwrap_or(false);
    let b64 = item
        .get("depth_base64")
        .and_then(|x| x.as_str())
        .ok_or("no depth_base64")?;
    let raw = STANDARD.decode(b64).map_err(|e| e.to_string())?;
    if raw.len() != (width as usize) * (height as usize) * 4 {
        return Err("depth byte length mismatch".to_string());
    }
    let depth: Vec<f32> = raw
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    Ok(DepthMap { width, height, is_metric, depth })
}

/// Back-project a metric depth map into the robot's SCENE frame.
///
/// Intrinsics come from the horizontal FOV (no per-camera calibration table yet):
/// `fx = (w/2) / tan(fov/2)`, `fy = fx` (square pixels), principal point at centre.
/// Optical convention is `+x right, +y down, +z forward`; it maps to the pose's
/// body frame (FLU, Z-up) as `body = (z, -x, -y)` before the scene transform.
fn backproject_to_scene(depth: &DepthMap, fov_deg: f32, pose: &Pose) -> Vec<f32> {
    let w = depth.width as usize;
    let h = depth.height as usize;
    if w == 0 || h == 0 || depth.depth.len() != w * h {
        return Vec::new();
    }
    let cx = depth.width as f32 / 2.0;
    let cy = depth.height as f32 / 2.0;
    let fx = (depth.width as f32 / 2.0) / (fov_deg.to_radians() / 2.0).tan();
    let fy = fx;

    let mut out: Vec<f32> = Vec::with_capacity((w / PIXEL_STRIDE) * (h / PIXEL_STRIDE) * 3);
    let mut v = 0usize;
    while v < h {
        let row = v * w;
        let mut u = 0usize;
        while u < w {
            let d = depth.depth[row + u];
            if d.is_finite() && d > 0.05 && d <= MAX_DEPTH_M {
                let x_opt = (u as f32 - cx) * d / fx;
                let y_opt = (v as f32 - cy) * d / fy;
                let z_opt = d;
                // optical → body (FLU, Z-up)
                let bx = z_opt;
                let by = -x_opt;
                let bz = -y_opt;
                let world = pose.transform_point([bx as f64, by as f64, bz as f64]);
                out.push(world[0] as f32);
                out.push(world[1] as f32);
                out.push(world[2] as f32);
            }
            u += PIXEL_STRIDE;
        }
        v += PIXEL_STRIDE;
    }
    out
}

/// Encode world points into a canonical v2 XYZ LiDAR frame the SLAM ingest expects.
fn encode_lidar_frame(points: &[f32], resolution: f32, seq: u32, timestamp_us: i64) -> Vec<u8> {
    let count = (points.len() / 3) as u32;
    let header = LidarFrameHeader {
        version: LIDAR_FRAME_VERSION,
        layout: LIDAR_LAYOUT_XYZ,
        flags: 0,
        point_count: count,
        frame_seq: seq,
        timestamp_us,
        host_send_us: 0,
        resolution,
        origin: [0.0, 0.0, 0.0],
    };
    let mut out = header.encode_header().to_vec();
    debug_assert_eq!(out.len(), LIDAR_HEADER_LEN);
    for p in points {
        out.extend_from_slice(&p.to_le_bytes());
    }
    out
}

/// Wall-clock microseconds for the frame timestamp. A clock read here is fine —
/// this is a frame stamp, not a deterministic-replay value.
fn depth_timestamp_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_depth(w: u32, h: u32, val: f32) -> DepthMap {
        DepthMap {
            width: w,
            height: h,
            is_metric: true,
            depth: vec![val; (w * h) as usize],
        }
    }

    #[test]
    fn backproject_depth_is_x_forward_under_identity_pose() {
        // 3×3 metric map at 2 m, 90° FOV, identity pose. Stride 3 samples only the
        // corner pixel (0,0): cx=cy=1.5, fx=fy=(3/2)/tan(45°)=1.5, so
        // x_opt=y_opt=(0-1.5)*2/1.5=-2, z_opt=2 → body (z,-x,-y)=(2,2,2) → world (2,2,2).
        // The key invariant: depth maps to +X (forward) in the Z-up body/scene frame.
        let dm = flat_depth(3, 3, 2.0);
        let pts = backproject_to_scene(&dm, 90.0, &Pose::identity());
        assert_eq!(pts.len(), 3, "one sampled pixel → one point");
        assert!((pts[0] - 2.0).abs() < 1e-4, "x forward = depth, got {}", pts[0]);
        assert!((pts[1] - 2.0).abs() < 1e-4, "y from -x_opt, got {}", pts[1]);
        assert!((pts[2] - 2.0).abs() < 1e-4, "z from -y_opt, got {}", pts[2]);
    }

    #[test]
    fn backproject_axis_pixel_is_purely_forward() {
        // An even-width map whose principal point lands ON a sampled pixel proves
        // depth → +X with zero lateral/vertical offset. w=h=6, stride 3 samples
        // (0,0) and (3,3); pixel (3,3) sits at cx=cy=3.0 → optical (0,0,d) →
        // body (d,0,0) → world (d,0,0).
        let dm = flat_depth(6, 6, 4.0);
        let pts = backproject_to_scene(&dm, 90.0, &Pose::identity());
        // Find the on-axis point (the one with ~zero y and z).
        let mut found = false;
        for p in pts.chunks_exact(3) {
            if p[1].abs() < 1e-4 && p[2].abs() < 1e-4 {
                assert!((p[0] - 4.0).abs() < 1e-4, "on-axis depth = 4, got {}", p[0]);
                found = true;
            }
        }
        assert!(found, "an on-axis pixel must back-project purely forward");
    }

    #[test]
    fn out_of_range_and_nonfinite_depth_dropped() {
        let mut dm = flat_depth(3, 3, f32::NAN);
        dm.depth[4] = 0.0; // centre zero → dropped
        let pts = backproject_to_scene(&dm, 90.0, &Pose::identity());
        assert!(pts.is_empty(), "no finite in-range samples → empty cloud");
    }

    #[test]
    fn lidar_frame_roundtrips_header() {
        let pts = vec![1.0_f32, 2.0, 3.0, 4.0, 5.0, 6.0];
        let frame = encode_lidar_frame(&pts, MAP_RESOLUTION_M, 7, 1234);
        let hdr = LidarFrameHeader::decode_header(&frame).expect("header");
        assert_eq!(hdr.point_count, 2);
        assert_eq!(hdr.frame_seq, 7);
        assert_eq!(hdr.version, LIDAR_FRAME_VERSION);
        assert!((hdr.resolution - MAP_RESOLUTION_M).abs() < 1e-6);
    }
}
