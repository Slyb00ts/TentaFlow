// =============================================================================
// File: examples/pipeline_bench.rs — AUTHORITATIVE end-to-end camera CV bench
// =============================================================================
//
// Unlike `cam_scale.rs` (a micro-bench that REIMPLEMENTS the per-frame work by
// calling the models directly), this bench drives the EXACT production pipeline
// end to end on ONE GPU:
//
//   real GStreamer ingest session (fake_file)  →  FrameMailbox
//     →  camera::latest_frame_global (supervisor snapshot)
//       →  vision_analysis::engine_loop  (cross-camera detect batching,
//          select_flush_batch, inflight Semaphore, executor.execute_camera_cv)
//         →  finalize_job → detection_bus (FAZA 1, hot) + cold path admit
//           →  cold_consumer → run_cold_stages → inference_batcher (state/plate/adr)
//             →  detection_bus (FAZA 2, enriched)
//
// Nothing here re-implements a stage: the models are resolved and invoked
// through the real `ModelRuntimeExecutor` (alias → catalog → resolver →
// `LocalCameraCvHandler`), so the numbers reflect production. Use `cam_scale`
// only for isolated per-model micro-timing; use THIS bench for capacity.
//
// The pipeline is the seeded default "Analiza domyślna (ADR)" (detect +
// nalepka-stan + plate-ocr + adr) — the ADR placard use case — resolved exactly
// as production does from the `camera_cv_pipelines` DB row via the cfg-check
// tick. Cameras are real `fake_file` sessions decoding a looped synthetic clip;
// the CODE PATH is what matters, not the pixels.
//
//   cargo run --release \
//     --features inference-vision-gpu,inference-supertonic --example pipeline_bench -- \
//     --gpus 0 --levels 5,10,20,40 --secs 8 --fps 10
//
// Prereqs (same as any real deploy): the vision model weights must be
// provisioned on disk (RF-DETR / nalepka-stan / plate-ocr / adr) and the ort
// GPU dylib present, otherwise detection resolves to an error and throughput is
// zero. Point `--video <path>` at real ADR footage for a representative
// enrichment (cold-path) load; the default synthetic clip exercises every code
// path but may yield few confident detections. All knobs are CLI flags — the
// bench sets the process-wide vision settings itself (vision::settings::init)
// and flips the fakefile/forced-detect bench hooks programmatically; there are
// no environment variables.
#![cfg(all(
    feature = "inference-vision-gpu",
    feature = "inference-supertonic",
    feature = "camera"
))]

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gstreamer as gst;
use gstreamer::prelude::*;
use parking_lot::Mutex as PlMutex;
use tokio::sync::broadcast::error::RecvError;

use tentaflow_core::addon::host_functions::camera::{add_fake_file_camera, remove_camera_global};
use tentaflow_core::db;
use tentaflow_core::db::repository::insert_camera;
use tentaflow_core::inference::local::LocalInferenceHandler;
use tentaflow_core::inference::shared_inference_manager;
use tentaflow_core::services::camera_ingest::fakefile::ensure_gst_initialized;
use tentaflow_core::services::camera_ingest::vision_analysis::set_runtime_slot;
use tentaflow_core::services::catalog::CatalogProvider;
use tentaflow_core::services::detection_bus;
use tentaflow_core::services::handles_cache::LiveHandlesCache;
use tentaflow_core::services::mesh_registry::MeshServicesRegistry;
use tentaflow_core::services::runtime::executor::ModelRuntimeExecutor;
use tentaflow_core::services::runtime::resolver::AliasResolver;
use tentaflow_core::vision::ort_common::ensure_ort_dylib;
use tentaflow_protocol::{RequestTimeParameters, ServiceInfo, ServiceModelEntry};

/// Stable local node id shared by the mesh registry, the live-handles cache and
/// the resolver so every embedded vision service resolves as a `Local` target.
const LOCAL_NODE: &str = "pipeline-bench-node";

/// The three embedded vision engines the ADR pipeline resolves through: the
/// alias target (`model_name`, the catalog entry id the seeded
/// `tentavision-*` aliases point at) and the embedded `engine_id`
/// `LocalCameraCvHandler` dispatches on. Matches the seeded aliases in
/// `db::seed::seed_camera_cv_aliases` and the manifests under
/// `tentaflow-containers/vision/_services/`.
const VISION_ENGINES: &[(&str, &str)] = &[
    ("rfdetr-adr", "rfdetr-adr-base"),
    ("nalepka-stan", "nalepka-stan-mnv4"),
    ("plate-ocr", "plate-ocr-fast"),
];

struct Args {
    levels: Vec<usize>,
    secs: u64,
    fps: u32,
    warmup_secs: u64,
    /// Stage-1 gate: drive the detect stage from a raw-NV12 detect frame through
    /// the GPU-resident device path (`detect_batch_gpu`) instead of the RGB
    /// executor path. Verifies the NV12 detect path end-to-end without a live
    /// camera. Flips the fakefile connector's NV12-detect bench hook.
    nv12_detect: bool,
    /// Stage-3 gate: drive BOTH detect AND enrichment on NV12 end to end. The
    /// crops frame is delivered raw NV12 (no full-frame `videoconvert`), so
    /// enrichment cuts crops via `crop_nv12`; forced detection makes the cold path
    /// run every frame. Verifies enriched/s > 0 on the NV12 crops path with the
    /// full videoconvert absent. Flips the fakefile full-NV12 bench hook and the
    /// forced-detect load generator.
    nv12: bool,
    /// Force the cold enrichment path on frames the detector left empty
    /// (`vision_analysis::set_force_detect`). Implied by `--nv12`.
    force_detect: bool,
    /// `[vision] gpus` spec (same grammar: count or comma list). Empty = device 0.
    gpus: String,
    /// Source video for the fake_file cameras; default = e2e sample / synthetic clip.
    video: Option<PathBuf>,
}

fn parse_args() -> Args {
    let mut a = Args {
        levels: vec![5, 10, 20, 40],
        secs: 8,
        fps: 10,
        warmup_secs: 20,
        nv12_detect: false,
        nv12: false,
        force_detect: false,
        gpus: String::new(),
        video: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        match k.as_str() {
            "--levels" => {
                if let Some(v) = it.next() {
                    a.levels = v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                }
            }
            "--secs" => a.secs = it.next().and_then(|v| v.parse().ok()).unwrap_or(a.secs),
            "--fps" => a.fps = it.next().and_then(|v| v.parse().ok()).unwrap_or(a.fps),
            "--warmup-secs" => {
                a.warmup_secs = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(a.warmup_secs)
            }
            "--nv12-detect" => a.nv12_detect = true,
            "--nv12" => a.nv12 = true,
            "--force-detect" => a.force_detect = true,
            "--gpus" => a.gpus = it.next().unwrap_or_default(),
            "--video" => a.video = it.next().map(PathBuf::from),
            _ => {}
        }
    }
    // The seeded analysis cadence clamps to 0..=30 and the source clip runs at
    // 30 fps, so an analysis fps above that can never pull a fresh frame.
    a.fps = a.fps.clamp(1, 30);
    // The ramp only ADDS cameras between levels (no per-camera supervisor
    // teardown), so levels must be ascending and unique.
    a.levels.sort_unstable();
    a.levels.dedup();
    a.levels.retain(|&n| n > 0);
    a
}

/// First GPU index from the `--gpus` spec (default 0) — the card we sample
/// with `nvidia-smi`. Mirrors `cam_scale`.
fn gpu_index(gpus: &str) -> String {
    gpus.split([',', ' '])
        .next()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "0".to_string())
}

fn sample_gpu_util(gpu: &str) -> Option<u32> {
    let out = Command::new("nvidia-smi")
        .args([
            "--query-gpu=utilization.gpu",
            "--format=csv,noheader,nounits",
            "-i",
            gpu,
        ])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u32>()
        .ok()
}

fn pctl(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// One running LOCAL embedded vision service, shaped exactly as the deploy path
/// projects a running `services` row (`snapshot_builder::project_service_row`)
/// into the mesh registry: `transport = embedded`, `status = running`, the
/// preset advertised as the sole model. `service_surfaces`/`input_modalities`
/// are additionally derived by the catalog from the compiled-in engine manifest
/// (`camera_cv` + `image`), so the resolver's capability check passes.
fn embedded_service(id: i64, engine_id: &str, model_name: &str) -> ServiceInfo {
    ServiceInfo {
        id,
        node_id: LOCAL_NODE.to_string(),
        engine_id: engine_id.to_string(),
        category: "vision".to_string(),
        display_name: engine_id.to_string(),
        deploy_method: "native_embedded".to_string(),
        transport: "embedded".to_string(),
        status: "running".to_string(),
        pinned: false,
        paused: false,
        runtime_pid: None,
        runtime_port: None,
        sidecar_quic_port: None,
        endpoint_url: None,
        restart_count: 0,
        health_last_err: None,
        active_deploy_id: String::new(),
        last_deploy_id: String::new(),
        deployment_progress_pct: 100,
        progress_message: None,
        models: vec![ServiceModelEntry {
            model_name: model_name.to_string(),
            display_name: Some(model_name.to_string()),
            capabilities: Vec::new(),
            context_length: None,
            quantization: None,
            is_default: true,
            service_surfaces: vec!["camera_cv".to_string()],
        }],
        update_available: false,
        created_at: String::new(),
        updated_at: String::new(),
        request_time_parameters: RequestTimeParameters::default(),
        gpu_selection: String::new(),
    }
}

/// Wires the real model router the way `Router::new` does — catalog + resolver +
/// executor over a live-handles cache holding the three embedded vision engines
/// — and installs it into the always-on camera engine via `set_runtime_slot`.
/// This is the exact construction production uses; only the network/health
/// supervisor scaffolding is omitted (not needed to drive CV analysis).
fn wire_executor(pool: &db::DbPool) -> anyhow::Result<()> {
    let registry = MeshServicesRegistry::new();
    let live_handles = Arc::new(LiveHandlesCache::new());

    let services: Vec<ServiceInfo> = VISION_ENGINES
        .iter()
        .enumerate()
        .map(|(i, (engine_id, model_name))| embedded_service(i as i64 + 1, engine_id, model_name))
        .collect();
    // Register the running local services + their embedded backend handles
    // exactly as the supervisor's `reconcile_handles` does at boot.
    registry.replace_local(LOCAL_NODE.to_string(), services.clone());
    for svc in &services {
        live_handles.upsert_service_info(svc, None)?;
    }
    // Build the catalog snapshot from the registry (alias entries + service
    // models) — same call the router/supervisor make.
    let catalog = Arc::new(CatalogProvider::new());
    catalog.rebuild(&registry, pool)?;

    // Static local-node-id provider (production uses a ServiceManager-backed
    // closure; a fixed id is correct here since our registry never changes).
    let local_node_id: tentaflow_core::services::runtime::resolver::LocalNodeIdProvider =
        Arc::new(|| LOCAL_NODE.to_string());
    let resolver = Arc::new(AliasResolver::new(live_handles, local_node_id));
    let local_inference = Arc::new(LocalInferenceHandler::new(shared_inference_manager()));

    let executor = Arc::new(ModelRuntimeExecutor::new(
        catalog,
        resolver,
        None, // no flow dispatcher — no camera is assigned an analysis flow
        local_inference,
        Arc::new(parking_lot::RwLock::new(None)), // stt_runtime
        Arc::new(parking_lot::RwLock::new(None)), // mesh_manager
        Arc::new(parking_lot::RwLock::new(None)), // model_residency
        Some(pool.clone()),
    ));

    set_runtime_slot(Arc::new(parking_lot::RwLock::new(Some(executor))));
    Ok(())
}

/// Resolves a video file the `fake_file` connector can loop. Preference:
/// the `--video` flag, then the e2e sample, then a synthetic clip generated
/// once via GStreamer next to the crate (so `resolve_file_url`'s no-symlink
/// containment holds).
fn ensure_sample_video(video: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(p) = video {
        if p.is_file() {
            return Ok(p.to_path_buf());
        }
        anyhow::bail!("--video does not point at a file: {}", p.display());
    }
    let asset_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/test");
    let sample = asset_dir.join("sample_traffic.mp4");
    if sample.is_file() {
        return Ok(sample);
    }
    let synth = asset_dir.join("pipeline_bench_source.mkv");
    if synth.is_file() {
        return Ok(synth);
    }
    std::fs::create_dir_all(&asset_dir)?;
    generate_synthetic_clip(&synth)?;
    Ok(synth)
}

/// Encodes a short 1280×720 test-pattern clip (motion JPEG in Matroska) with
/// GStreamer so the bench needs no external ffmpeg. `decodebin` in the
/// `fake_file` session loops it. Content is irrelevant — the decode → analysis
/// code path is what the bench exercises.
fn generate_synthetic_clip(out: &Path) -> anyhow::Result<()> {
    ensure_gst_initialized().map_err(|e| anyhow::anyhow!("gst init: {e}"))?;
    let loc = out.to_string_lossy().replace('"', "\\\"");
    let desc = format!(
        "videotestsrc num-buffers=300 pattern=ball ! video/x-raw,width=1280,height=720,framerate=30/1 \
         ! videoconvert ! jpegenc ! matroskamux ! filesink location=\"{loc}\""
    );
    let pipeline = gst::parse::launch(&desc)
        .map_err(|e| anyhow::anyhow!("build synth pipeline (need jpegenc + matroskamux): {e}"))?
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow::anyhow!("synth element is not a pipeline"))?;
    pipeline
        .set_state(gst::State::Playing)
        .map_err(|e| anyhow::anyhow!("synth play: {e}"))?;
    let bus = pipeline
        .bus()
        .ok_or_else(|| anyhow::anyhow!("synth pipeline has no bus"))?;
    for msg in bus.iter_timed(gst::ClockTime::NONE) {
        match msg.view() {
            gst::MessageView::Eos(_) => break,
            gst::MessageView::Error(e) => {
                let _ = pipeline.set_state(gst::State::Null);
                anyhow::bail!("synth encode error: {}", e.error());
            }
            _ => {}
        }
    }
    pipeline
        .set_state(gst::State::Null)
        .map_err(|e| anyhow::anyhow!("synth stop: {e}"))?;
    println!("generated synthetic source clip: {}", out.display());
    Ok(())
}

/// Shared measurement state, reset per ramp level. A published detection message
/// is counted once per (camera, captured_ms): the FIRST publication marks a
/// frame that completed the detect stage through the full engine; a later
/// publication for the same key is the enriched (FAZA 2, cold-path) re-publish.
#[derive(Default)]
struct Metrics {
    measuring: bool,
    seen: HashSet<(String, u64)>,
    detect_count: u64,
    enriched_count: u64,
    lats_ms: Vec<f64>,
}

impl Metrics {
    fn reset(&mut self) {
        self.measuring = false;
        self.seen.clear();
        self.detect_count = 0;
        self.enriched_count = 0;
        self.lats_ms.clear();
    }
}

/// Subscribes to one camera's detection bus and folds every publication into the
/// shared metrics (only while a measurement window is open). Lives for the whole
/// run — cameras are added cumulatively across ramp levels.
fn spawn_subscriber(camera_id: String, metrics: Arc<PlMutex<Metrics>>) {
    let mut rx = detection_bus::subscribe(&camera_id);
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(msg) => {
                    let recv = now_ms();
                    let mut m = metrics.lock();
                    if !m.measuring {
                        continue;
                    }
                    let key = (camera_id.clone(), msg.ts_ms);
                    if m.seen.insert(key) {
                        m.detect_count += 1;
                        m.lats_ms.push((recv.saturating_sub(msg.ts_ms)) as f64);
                    } else {
                        // Same captured frame re-published → cold-path enrichment.
                        m.enriched_count += 1;
                    }
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    });
}

/// Adds one `fake_file` camera end to end: a `cameras` DB row (so the engine
/// resolves the seeded ADR pipeline + `analysis_fps`), a real ingest session +
/// always-on analysis, and a detection-bus subscriber.
async fn add_camera(
    pool: &db::DbPool,
    camera_id: &str,
    video: &str,
    analysis_fps: u32,
    metrics: &Arc<PlMutex<Metrics>>,
) -> anyhow::Result<()> {
    insert_camera(
        pool,
        camera_id,
        "pipeline-bench",
        camera_id,
        "fake_file",
        video,
        30,                  // target_fps (source is 30 fps)
        analysis_fps as i64, // analysis cadence the engine paces by
        None,
        None,
        "C",
        "default",
        None,
        None,
        None,
        None, // org → org-default, which owns the seeded default ADR pipeline
    )?;
    // Tolerate "already exists": the supervisor may have picked the camera up from
    // the DB insert (reconcile) before this explicit add, or it lingers from a prior
    // run — either way the real camera + analysis are live, so reuse it.
    if let Err(e) = add_fake_file_camera(camera_id, video, 30).await {
        if !e.to_string().contains("already exists") {
            return Err(e);
        }
    }
    spawn_subscriber(camera_id.to_string(), metrics.clone());
    Ok(())
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    let args = parse_args();
    let gpu = gpu_index(&args.gpus);
    let budget_ms = 1000.0 / args.fps as f64;

    // Freeze the process-wide vision settings from the CLI flags BEFORE any
    // model singleton or camera pipeline can read them.
    tentaflow_core::vision::settings::init(tentaflow_core::config::VisionConfig {
        gpus: args.gpus.clone(),
        ..Default::default()
    })?;

    ensure_ort_dylib();

    // Stage-1 NV12 gate: make the fakefile connector tee the decoded NV12 into a
    // raw-NV12 detect appsink so the engine routes detect through the GPU-resident
    // device path (`detect_batch_gpu`). Flipped BEFORE any camera session builds
    // its pipeline. The warmup detect count then proves the NV12 path publishes.
    tentaflow_core::services::camera_ingest::fakefile::set_nv12_bench_mode(
        args.nv12_detect,
        args.nv12,
    );
    if args.nv12_detect {
        println!(
            "NV12 detect mode: detect stage runs from raw NV12 via detect_batch_gpu (GPU device path)\n"
        );
    }
    // Stage-3 full NV12 mode: crops frame delivered raw NV12 (no full videoconvert)
    // and forced detection so the cold path exercises enrichment on NV12 crops.
    if args.nv12 {
        println!(
            "NV12 full mode: detect + enrichment run NV12 end to end (crops via crop_nv12, no full \
             videoconvert); forced detection drives the cold path every frame\n"
        );
    }
    // Forced-enrichment load generator (implied by --nv12): the cold path runs
    // even on frames the detector left empty.
    tentaflow_core::services::camera_ingest::vision_analysis::set_force_detect(
        args.force_detect || args.nv12,
    );

    // Real DB init: runs migrations + seed (default ADR pipeline + tentavision-*
    // aliases + org-default) and installs the global pool the engine reads.
    let db_path = std::env::temp_dir().join(format!("pipeline_bench_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    let pool = db::init(&db_path)?;

    wire_executor(&pool)?;

    let video = ensure_sample_video(args.video.as_deref())?;
    let video = video.to_string_lossy().to_string();

    let cap = tentaflow_core::services::camera_ingest::MAX_CAMERAS_GLOBAL;
    println!(
        "pipeline_bench (GPU {gpu}) — real engine end-to-end\n\
         pipeline = seeded ADR (detect + nalepka-stan + plate-ocr + adr)\n\
         source   = {video}\n\
         analysis fps {} | budget {:.1} ms/frame | {} s/level | supervisor cap {}\n",
        args.fps, budget_ms, args.secs, cap
    );

    let metrics = Arc::new(PlMutex::new(Metrics::default()));

    // Warmup: register up to the LARGEST ramp level so every batch-size TRT engine
    // the ramp will hit (detect + cold classify/plate/adr) is built BEFORE any
    // measurement — otherwise a level pays a multi-second engine build mid-window
    // (the symptom is 0/s then a burst, p99 in the tens of seconds). The warmup
    // cameras are then torn down and the ascending ramp re-adds FRESH cameras
    // against the now-warm engines (the process-wide model singletons keep their
    // compiled engines across a camera remove). `measuring` is on so forced
    // enrichment (if enabled) also builds the cold engines.
    let warm_n = (*args.levels.iter().max().unwrap_or(&1)).min(cap);
    println!(
        "warmup ({}s, {warm_n} cameras): loading models + building detect/enrich engines...",
        args.warmup_secs
    );
    {
        let mut m = metrics.lock();
        m.reset();
        m.measuring = true;
    }
    let mut warm_ids: Vec<String> = Vec::new();
    for i in 0..warm_n {
        let id = format!("cam-warmup-{i:04}");
        if let Err(e) = add_camera(&pool, &id, &video, args.fps, &metrics).await {
            eprintln!("warmup add {id} failed: {e}");
            break;
        }
        warm_ids.push(id);
    }
    tokio::time::sleep(Duration::from_secs(args.warmup_secs.max(1))).await;
    let (warm_detect, warm_enriched) = {
        let mut m = metrics.lock();
        m.measuring = false;
        (m.detect_count, m.enriched_count)
    };
    for id in &warm_ids {
        let _ = remove_camera_global(id).await;
    }
    // Give the torn-down sessions a moment to fully stop before the ramp begins.
    tokio::time::sleep(Duration::from_secs(1)).await;
    println!("warmup done (detect {warm_detect}, enriched {warm_enriched}).\n");

    // Stage-1 gate: the NV12 device detect path MUST publish detections. Zero
    // means the raw-NV12 detect frame never reached `detect_batch_gpu` (or the
    // GPU preprocess/forward failed) — fail loudly so the bench is a real gate.
    if (args.nv12_detect || args.nv12) && warm_detect == 0 {
        anyhow::bail!(
            "NV12 detect gate FAILED: 0 detections via the GPU device path after {}s warmup \
             (expected the raw-NV12 detect frame to drive detect_batch_gpu)",
            args.warmup_secs
        );
    }
    // Stage-3 gate: NV12 crops must produce enrichment. Forced detection is on, so
    // the cold path (crop_nv12 → classify/ocr) should complete every frame; zero
    // means the NV12 crop convert or the cold path failed.
    if args.nv12 && warm_enriched == 0 {
        anyhow::bail!(
            "NV12 enrichment gate FAILED: 0 enriched frames after {}s warmup \
             (expected crop_nv12 → cold path to enrich every forced detection)",
            args.warmup_secs
        );
    }

    let mut active: Vec<String> = Vec::new();

    println!(
        "{:>5} {:>10} {:>10} {:>11} {:>8} {:>8} {:>8} {:>6}  keeps-up",
        "cams", "target/s", "actual/s", "enriched/s", "p50 ms", "p99 ms", "max ms", "gpu%"
    );

    for &n in &args.levels {
        if n > cap {
            println!("{n:>5}  skipped — exceeds supervisor MAX_CAMERAS_GLOBAL ({cap})");
            continue;
        }
        // Ramp is monotonic (like cam_scale): only add the delta.
        let mut idx = active.len();
        while active.len() < n {
            let id = format!("cam-bench-{idx:04}");
            match add_camera(&pool, &id, &video, args.fps, &metrics).await {
                Ok(()) => active.push(id),
                Err(e) => {
                    eprintln!("add camera {idx} failed: {e}");
                    break;
                }
            }
            idx += 1;
        }
        // Let freshly added sessions reach steady state before measuring (engines
        // are already warm from the max-level warmup, so a short settle suffices).
        tokio::time::sleep(Duration::from_secs(2)).await;

        {
            let mut m = metrics.lock();
            m.reset();
            m.measuring = true;
        }

        // GPU util sampler for the window.
        let stop = Arc::new(AtomicBool::new(false));
        let util_samples = Arc::new(PlMutex::new(Vec::<u32>::new()));
        let sampler = {
            let stop = stop.clone();
            let util_samples = util_samples.clone();
            let gpu = gpu.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    if let Some(u) = sample_gpu_util(&gpu) {
                        util_samples.lock().push(u);
                    }
                    std::thread::sleep(Duration::from_millis(150));
                }
            })
        };

        tokio::time::sleep(Duration::from_secs(args.secs)).await;

        let (detect, enriched, mut lats) = {
            let mut m = metrics.lock();
            m.measuring = false;
            (
                m.detect_count,
                m.enriched_count,
                std::mem::take(&mut m.lats_ms),
            )
        };
        stop.store(true, Ordering::Relaxed);
        let _ = sampler.join();

        let actual = detect as f64 / args.secs as f64;
        let enriched_ps = enriched as f64 / args.secs as f64;
        let target = active.len() as f64 * args.fps as f64;
        lats.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let p50 = pctl(&lats, 0.50);
        let p99 = pctl(&lats, 0.99);
        let mx = lats.last().copied().unwrap_or(0.0);
        let utils = util_samples.lock();
        let avg_util = if utils.is_empty() {
            0
        } else {
            utils.iter().sum::<u32>() / utils.len() as u32
        };
        // Keeps up = delivered ≥ 97% of offered analysis frames AND per-frame p99
        // within budget. Enrichment is event-driven/coalesced, so it is reported
        // but NOT a keeps-up criterion.
        let keeps = actual >= 0.97 * target && p99 <= budget_ms && p99 > 0.0;
        println!(
            "{:>5} {:>10.0} {:>10.0} {:>11.0} {:>8.1} {:>8.1} {:>8.1} {:>5}%  {}",
            active.len(),
            target,
            actual,
            enriched_ps,
            p50,
            p99,
            mx,
            avg_util,
            if keeps { "YES" } else { "NO (saturated)" }
        );
    }

    println!(
        "\nreal cameras/GPU (this build + models) = the LAST 'YES' row.\n\
         'enriched/s' = cold-path (state/plate/adr) completions; 0 means the source\n\
         produced no confident ADR detections — point --video at real\n\
         ADR footage to load the enrichment path. This bench drives the REAL engine\n\
         loop, detect-flush batching, executor and cold path — cam_scale does not."
    );

    // Best-effort teardown so GStreamer sessions stop before the process exits.
    tentaflow_core::addon::host_functions::camera::shutdown_camera_supervisor_global().await;
    let _ = std::fs::remove_file(&db_path);
    Ok(())
}
