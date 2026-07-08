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

#[cfg(not(feature = "inference-vision-gpu"))]
use std::io::Cursor;
use std::sync::OnceLock;
use std::time::Duration;

#[cfg(not(feature = "inference-vision-gpu"))]
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

/// Drop depth samples beyond this range (m, AFTER scale). Monocular metric depth grows
/// unreliable with range (error ∝ distance) and over-spreads under a pinhole model, so
/// the far tail is the sprawling "garbage" — only the near structure is trustworthy.
/// 3 m keeps the reliable bulk (Go2 indoor p50 ≈ 2.2 m after scale) and cuts the tail.
const MAX_DEPTH_M: f32 = 3.0;

/// Pixel stride when sampling the depth map. A dense map (e.g. 504×378 ≈ 190k px)
/// is decimated to keep the per-frame cloud near a LiDAR's point budget; the
/// voxel grid dedups the rest anyway.
const PIXEL_STRIDE: usize = 3;

/// Set of cameras with depth mapping ON — the work-list the SINGLE central worker
/// iterates each tick. One shared model + ONE GPU launch is amortized across ALL of
/// them, so adding a camera/robot does NOT spin up another inference loop.
fn active() -> &'static DashMap<String, ()> {
    static A: OnceLock<DashMap<String, ()>> = OnceLock::new();
    A.get_or_init(DashMap::new)
}

/// The single central batched worker handle. Started lazily by the first active
/// camera; self-exits when `active` empties; `ensure_depth_mapping` restarts it.
fn worker() -> &'static std::sync::Mutex<Option<JoinHandle<()>>> {
    static W: OnceLock<std::sync::Mutex<Option<JoinHandle<()>>>> = OnceLock::new();
    W.get_or_init(|| std::sync::Mutex::new(None))
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
        Ok(None) => {
            active().remove(camera_id);
            return;
        }
        Err(e) => {
            warn!("[depth_mapping] config read failed for {camera_id}: {e}");
            return;
        }
    };
    // Pre-load + autotune the native depth model ONCE, off the loop (the first GPU
    // forward compiles kernels, ~20 s on wgpu). Doing it here — as soon as ANY camera
    // enables depth mapping, before frames/pose flow — means the loop's first real
    // frame is already fast instead of stalling on autotune.
    #[cfg(feature = "inference-vision-gpu")]
    {
        static PREWARM: std::sync::Once = std::sync::Once::new();
        PREWARM.call_once(|| {
            // Prewarm the model on the shared inference thread (not a one-off thread), so
            // its first forward runs on the same thread/cubecl state as real inference.
            tokio::spawn(async {
                match crate::vision::burn_backend::run_blocking(
                    crate::vision::depth_anything::prewarm,
                )
                .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => warn!("[depth_mapping] depth prewarm failed: {e}"),
                    Err(e) => warn!("[depth_mapping] depth prewarm thread dropped: {e}"),
                }
            });
        });
    }
    // Add this camera to the work-list and make sure the single central worker is
    // running. The worker batches ALL active cameras into one forward per tick.
    let newly = active().insert(camera_id.to_string(), ()).is_none();
    if newly {
        info!(
            "[depth_mapping] camera {} enabled (robot={} fov={:.0}) — {} active",
            cfg.camera_id,
            cfg.robot_id,
            cfg.fov_deg,
            active().len()
        );
    }
    let mut w = worker().lock().unwrap_or_else(|e| e.into_inner());
    if w.as_ref().map(|h| h.is_finished()).unwrap_or(true) {
        *w = Some(tokio::spawn(central_worker()));
    }
}

/// Disable depth mapping for one camera (removed / toggled off). The central worker
/// stays alive idling on an empty work-list; `drain` stops it at shutdown.
pub fn stop_depth_mapping(camera_id: &str) {
    active().remove(camera_id);
}

/// Stop all depth mapping (process shutdown). Mirrors `vision_analysis::drain`.
pub fn drain() {
    active().clear();
    if let Some(h) = worker().lock().unwrap_or_else(|e| e.into_inner()).take() {
        h.abort();
    }
}

/// One camera's per-tick inputs, gathered before the shared batched forward.
struct Job {
    cfg: DepthMappingConfig,
    pose: Pose,
    rgb: std::sync::Arc<[u8]>,
    w: u32,
    h: u32,
}

/// Central batched depth worker. Each tick it gathers the FRESHEST frame + pose from
/// EVERY active depth-mapped camera, runs ONE batched forward (GPU: a single
/// `[N,3,518,518]` launch amortized across all sources; HTTP: per-camera serial),
/// and folds each resulting cloud into its robot's scene. Model-paced with
/// frame-skip (always the latest frame, intermediates dropped). Exits when no camera
/// is active — `ensure_depth_mapping` restarts it.
async fn central_worker() {
    // Native depth is in-process + fast (~30 ms/batch) — let the MODEL pace the loop,
    // not an artificial fps cap; a tight tick + `Skip` keeps each pass on the freshest
    // frames. The HTTP path is heavier/remote, so pace it slower.
    #[cfg(feature = "inference-vision-gpu")]
    let interval = Duration::from_millis(33);
    #[cfg(not(feature = "inference-vision-gpu"))]
    let interval = Duration::from_millis(500);
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let mut seq: u32 = 0;

    loop {
        tick.tick().await;

        let cams: Vec<String> = active().iter().map(|e| e.key().clone()).collect();
        if cams.is_empty() {
            continue; // idle — stay alive so a re-enabled camera needs no restart race
        }

        // Gather one job per camera that has a live config + pose + frame. Config is
        // re-read each tick so live edits (FOV/pitch/scale calibration, robot binding,
        // disable) take effect with no restart.
        let t_gather = std::time::Instant::now();
        let mut jobs: Vec<Job> = Vec::with_capacity(cams.len());
        for cam in &cams {
            let Some(cfg) = crate::db::global_pool()
                .and_then(|p| crate::db::repository::camera_depth_mapping_config(&p, cam).ok())
                .flatten()
            else {
                active().remove(cam); // mapping turned off / camera gone
                continue;
            };
            // Grab the freshest frame WITH its capture timestamp, then place it with the
            // pose from THAT time — not "latest". The camera (WebRTC decode) lags the
            // light pose telemetry, so using the latest pose smears the cloud by the
            // camera latency × angular velocity whenever the robot turns.
            // Depth needs the full-res crops frame; the detect frame (last) is
            // ignored — it is the small 560 detector input.
            let Some((crops, w, h, captured_ms, _pts_ns, crops_format, _detect, crops_device)) =
                crate::addon::host_functions::camera::latest_frame_global(cam).await
            else {
                continue; // no frame yet
            };
            // Zero-copy crops: the crops bytes are empty (device-resident). Depth
            // runs at a low cadence, so download the full NV12 on demand here.
            let crops = if crops.is_empty() {
                match crops_device.as_ref().and_then(|d| d.download_full_nv12()) {
                    Some((nv12, _fmt)) => nv12,
                    None => crops,
                }
            } else {
                crops
            };
            // Depth back-projection assumes RGB24. On the GPU-resident NVDEC path
            // the crops frame is NV12 — convert on demand (depth runs at a low
            // cadence, so a per-pulled-frame convert here is acceptable).
            let rgb: std::sync::Arc<[u8]> = match crops_format {
                crate::services::camera_ingest::fakefile::DetectFrameFormat::Rgb24 => crops,
                crate::services::camera_ingest::fakefile::DetectFrameFormat::Nv12 { .. } => {
                    match crate::services::camera_ingest::fakefile::nv12_frame_to_rgb24(
                        &crops,
                        w,
                        h,
                        &crops_format,
                    ) {
                        Some(v) => std::sync::Arc::from(v),
                        None => continue,
                    }
                }
            };
            let Some(pose) = SlamSceneManager::global()
                .scene_pose_at(&cfg.pose_robot_id, (captured_ms as i64) * 1000)
            else {
                continue; // robot not localized yet
            };
            jobs.push(Job {
                cfg,
                pose,
                rgb,
                w,
                h,
            });
        }
        if jobs.is_empty() {
            continue;
        }
        let t_pull_ms = t_gather.elapsed().as_secs_f64() * 1000.0;

        // ONE batched inference across all gathered cameras.
        let t_infer = std::time::Instant::now();
        let depths = acquire_depth_batch(&jobs, &client).await;
        let t_infer_ms = t_infer.elapsed().as_secs_f64() * 1000.0;

        // Per-camera back-project + fold into each robot's scene.
        let t_rest = std::time::Instant::now();
        let mut total_pts = 0usize;
        for (job, depth) in jobs.iter().zip(depths) {
            let Some(depth) = depth else { continue };
            if !depth.is_metric {
                // A non-metric model can't build a metre-scaled map — drop this camera
                // from the work-list instead of burning a batch slot on it every tick.
                warn!(
                    "[depth_mapping] non-metric depth for {} — disabling its mapping (deploy a *-Metric-* model)",
                    job.cfg.camera_id
                );
                active().remove(&job.cfg.camera_id);
                continue;
            }
            maybe_dump_calibration(&depth, job);
            let points = backproject_to_scene(
                &depth,
                job.cfg.fov_deg,
                job.cfg.fov_v_deg,
                job.cfg.pitch_deg,
                job.cfg.scale,
                &job.pose,
            );
            if points.is_empty() {
                continue;
            }
            seq = seq.wrapping_add(1);
            let frame = encode_lidar_frame(&points, MAP_RESOLUTION_M, seq, depth_timestamp_us());
            SlamSceneManager::global().on_lidar_frame(&job.cfg.robot_id, &frame);
            total_pts += points.len() / 3;
        }
        let t_rest_ms = t_rest.elapsed().as_secs_f64() * 1000.0;

        // Batch-level host latency breakdown. The browser logs net + decode + render
        // separately (`[lidar latency]`).
        info!(
            "[depth_pipeline] batch={n} pull={pull:.1} infer={infer:.1} \
             backproject+fold={rest:.1} host_total={tot:.1}ms pts={pts}",
            n = jobs.len(),
            pull = t_pull_ms,
            infer = t_infer_ms,
            rest = t_rest_ms,
            tot = t_pull_ms + t_infer_ms + t_rest_ms,
            pts = total_pts,
        );
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

/// Metric depth for a BATCH of camera frames, in input order. GPU build: one
/// `[N,3,518,518]` Burn forward (zero IPC) on a blocking thread — the whole point of
/// batching is one GPU launch for all sources. A per-frame failure is `None` at that
/// slot (the batch as a whole succeeds or all-`None`).
#[cfg(feature = "inference-vision-gpu")]
async fn acquire_depth_batch(jobs: &[Job], _client: &reqwest::Client) -> Vec<Option<DepthMap>> {
    let inputs: Vec<(std::sync::Arc<[u8]>, u32, u32)> =
        jobs.iter().map(|j| (j.rgb.clone(), j.w, j.h)).collect();
    let n = inputs.len();
    // Runs on a large-stack thread — the burn-generated forward overruns the default
    // blocking-thread stack in debug builds. See `burn_backend::run_blocking`.
    let result = crate::vision::burn_backend::run_blocking(move || {
        let refs: Vec<(&[u8], u32, u32)> = inputs
            .iter()
            .map(|(r, w, h)| (r.as_ref(), *w, *h))
            .collect();
        crate::vision::depth_anything::infer_global_batch(&refs)
    })
    .await;
    match result {
        Ok(Ok(maps)) => maps
            .into_iter()
            .map(|(depth, width, height)| {
                Some(DepthMap {
                    width,
                    height,
                    is_metric: true,
                    depth,
                })
            })
            .collect(),
        Ok(Err(e)) => {
            debug!("[depth_mapping] batch depth inference failed: {e}");
            (0..n).map(|_| None).collect()
        }
        Err(e) => {
            debug!("[depth_mapping] depth-infer thread dropped: {e}");
            (0..n).map(|_| None).collect()
        }
    }
}

/// HTTP build (no GPU vision stack): the local `depth` service is per-request, so the
/// "batch" is just each camera in turn.
#[cfg(not(feature = "inference-vision-gpu"))]
async fn acquire_depth_batch(jobs: &[Job], client: &reqwest::Client) -> Vec<Option<DepthMap>> {
    let mut out = Vec::with_capacity(jobs.len());
    for j in jobs {
        let endpoint = match resolve_depth_endpoint() {
            Some(e) => e,
            None => {
                out.push(None);
                continue;
            }
        };
        let depth = match encode_jpeg(&j.rgb, j.w, j.h) {
            Some(jpeg) => request_depth(client, &endpoint, &jpeg).await.ok(),
            None => None,
        };
        out.push(depth);
    }
    out
}

/// Resolve a running local `depth` service endpoint (Running first, then Degraded),
/// mirroring `web_research::resolve_local_service_endpoint`. `None` when none is up.
#[cfg(not(feature = "inference-vision-gpu"))]
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
#[cfg(not(feature = "inference-vision-gpu"))]
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
#[cfg(not(feature = "inference-vision-gpu"))]
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
    let width = item
        .get("width")
        .and_then(|x| x.as_u64())
        .ok_or("no width")? as u32;
    let height = item
        .get("height")
        .and_then(|x| x.as_u64())
        .ok_or("no height")? as u32;
    let is_metric = item
        .get("is_metric")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
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
    Ok(DepthMap {
        width,
        height,
        is_metric,
        depth,
    })
}

/// One-shot calibration capture (env `TENTAFLOW_CALIB_DUMP=1`): writes the raw metric
/// depth map + camera pose + current (fov,pitch,scale) and the real robot's accumulated
/// lidar cloud (ground truth) to `/tmp/tf_calib/{depth,lidar}.bin`. Lets the offline
/// `depth_calib` example optimize the extrinsics against lidar without the robot live.
fn maybe_dump_calibration(depth: &DepthMap, job: &Job) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static DONE: AtomicBool = AtomicBool::new(false);
    if std::env::var("TENTAFLOW_CALIB_DUMP").is_err() {
        return;
    }
    let lidar = match SlamSceneManager::global().snapshot(&job.cfg.pose_robot_id) {
        Some(s) if s.points.len() >= 300 => s.points, // need a real lidar cloud to fit against
        _ => return,
    };
    if DONE.swap(true, Ordering::SeqCst) {
        return;
    }
    let dir = std::path::Path::new("/tmp/tf_calib");
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let t = job.pose.translation();
    let q = job.pose.quat_xyzw();
    let mut d = Vec::with_capacity(64 + depth.depth.len() * 4);
    d.extend_from_slice(&0x4445_5054u32.to_le_bytes()); // "DEPT"
    d.extend_from_slice(&depth.width.to_le_bytes());
    d.extend_from_slice(&depth.height.to_le_bytes());
    for v in [job.cfg.fov_deg, job.cfg.pitch_deg, job.cfg.scale] {
        d.extend_from_slice(&v.to_le_bytes());
    }
    for v in t {
        d.extend_from_slice(&v.to_le_bytes());
    }
    for v in q {
        d.extend_from_slice(&v.to_le_bytes());
    }
    for v in &depth.depth {
        d.extend_from_slice(&v.to_le_bytes());
    }
    let mut l = Vec::with_capacity(8 + lidar.len() * 4);
    l.extend_from_slice(&0x4C49_4441u32.to_le_bytes()); // "LIDA"
    l.extend_from_slice(&((lidar.len() / 3) as u32).to_le_bytes());
    for v in &lidar {
        l.extend_from_slice(&v.to_le_bytes());
    }
    let _ = std::fs::write(dir.join("depth.bin"), &d);
    let _ = std::fs::write(dir.join("lidar.bin"), &l);
    info!(
        "[depth_calib] dumped capture: depth {}x{}, lidar {} pts, pose t={:?} -> /tmp/tf_calib",
        depth.width,
        depth.height,
        lidar.len() / 3,
        t
    );
}

/// Back-project a metric depth map into the robot's SCENE frame.
///
/// Intrinsics come from the horizontal FOV (no per-camera calibration table yet):
/// `fx = (w/2) / tan(fov/2)`, `fy = fx` (square pixels), principal point at centre.
/// Optical convention is `+x right, +y down, +z forward`; it maps to the pose's
/// body frame (FLU, Z-up) as `body = (z, -x, -y)`. Extrinsic calibration: `scale`
/// corrects the monocular metric scale; `pitch_deg` rotates the camera frame about
/// the body LEFT axis (a down-angled mount like the Go2's needs negative pitch) so
/// the cloud lands on the lidar instead of floating off. Then the scene transform.
fn backproject_to_scene(
    depth: &DepthMap,
    fov_deg: f32,
    fov_v_deg: f32,
    pitch_deg: f32,
    scale: f32,
    pose: &Pose,
) -> Vec<f32> {
    let w = depth.width as usize;
    let h = depth.height as usize;
    if w == 0 || h == 0 || depth.depth.len() != w * h {
        return Vec::new();
    }
    let cx = depth.width as f32 / 2.0;
    let cy = depth.height as f32 / 2.0;
    let fx = (depth.width as f32 / 2.0) / (fov_deg.to_radians() / 2.0).tan();
    // Vertical FOV is decoupled when set (>0): the depth model runs on a square frame
    // STRETCHED from the camera's wide 16:9 stream, so the true vertical FOV is far
    // narrower than the horizontal. `0` ⇒ square pixels (legacy `fy = fx`).
    let fy = if fov_v_deg > 0.0 {
        (depth.height as f32 / 2.0) / (fov_v_deg.to_radians() / 2.0).tan()
    } else {
        fx
    };
    // Mount-pitch rotation about the body LEFT (+Y) axis. Sign convention: NEGATIVE
    // pitch tilts the forward +X ray DOWN (toward -Z) — the Go2 camera case — and
    // positive tilts up. (Angle negated so the forward ray's bz = bx*sin(pitch).)
    let (sp, cp) = (-pitch_deg).to_radians().sin_cos();

    let mut out: Vec<f32> = Vec::with_capacity((w / PIXEL_STRIDE) * (h / PIXEL_STRIDE) * 3);
    let mut v = 0usize;
    while v < h {
        let row = v * w;
        let mut u = 0usize;
        while u < w {
            let d = depth.depth[row + u] * scale;
            if d.is_finite() && d > 0.05 && d <= MAX_DEPTH_M {
                let x_opt = (u as f32 - cx) * d / fx;
                let y_opt = (v as f32 - cy) * d / fy;
                let z_opt = d;
                // optical → body (FLU, Z-up)
                let bx0 = z_opt;
                let by = -x_opt;
                let bz0 = -y_opt;
                // apply mount pitch about +Y (left)
                let bx = bx0 * cp + bz0 * sp;
                let bz = -bx0 * sp + bz0 * cp;
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
        let pts = backproject_to_scene(&dm, 90.0, 0.0, 0.0, 1.0, &Pose::identity());
        assert_eq!(pts.len(), 3, "one sampled pixel → one point");
        assert!(
            (pts[0] - 2.0).abs() < 1e-4,
            "x forward = depth, got {}",
            pts[0]
        );
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
        let pts = backproject_to_scene(&dm, 90.0, 0.0, 0.0, 1.0, &Pose::identity());
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
        let pts = backproject_to_scene(&dm, 90.0, 0.0, 0.0, 1.0, &Pose::identity());
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
