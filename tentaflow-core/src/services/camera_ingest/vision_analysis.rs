// =============================================================================
// File: services/camera_ingest/vision_analysis.rs — always-on RF-DETR loop
// =============================================================================
//
// Per-camera always-on CV analysis for the Acme PoC (Phase B). One task per
// camera pulls the latest decoded RGB frame from the running session (via the
// supervisor snapshot path), runs the shared RF-DETR detector, and publishes
// real detections into `detection_bus` — the same contract the dev stub used.
//
// The detector (one 119 MB ONNX session) is a process-wide singleton shared by
// every camera task behind a mutex: analysis is paced at a low fixed rate, so
// serializing inference across cameras keeps a single CPU session predictable.
// A load failure degrades gracefully — the task logs once and exits, leaving
// the camera session and the dashboard untouched (no detections, no crash).

#![cfg(feature = "inference-vision-gpu")]

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::sync::{mpsc, OnceCell};
use tracing::{info, warn};

use crate::services::detection_bus;
use crate::vision::classifier_stan::StateClassifier;
use crate::vision::detector_rfdetr::RfDetrDetector;
use crate::vision::ocr_plate::PlateOcr;

/// Floor interval for `analysis_fps = 0` (unlimited). ~30 fps native cadence —
/// a hard floor so the loop never busy-spins waiting on frames; inference on CPU
/// is the real ceiling anyway.
const UNLIMITED_INTERVAL: Duration = Duration::from_millis(33);

/// Default analysis cadence when no per-camera value is resolvable (10 fps).
const DEFAULT_ANALYSIS_FPS: u32 = 10;

/// Resolves the loop tick interval from a configured analysis FPS. `0` is
/// unlimited (native cadence floored at [`UNLIMITED_INTERVAL`]); any other
/// value maps to `1000 / fps` ms.
fn interval_for_fps(fps: u32) -> Duration {
    if fps == 0 {
        UNLIMITED_INTERVAL
    } else {
        Duration::from_millis((1000 / fps.max(1)) as u64)
    }
}

/// Reads the per-camera analysis FPS from the core DB, falling back to the
/// default when no pool / row is available.
fn resolve_analysis_fps(camera_id: &str) -> u32 {
    match crate::db::global_pool() {
        Some(pool) => {
            crate::db::repository::camera_analysis_fps(&pool, camera_id).unwrap_or(DEFAULT_ANALYSIS_FPS)
        }
        None => DEFAULT_ANALYSIS_FPS,
    }
}

/// Process-wide RF-DETR detector, loaded on first use. `tokio::sync::OnceCell`
/// so a slow load (~hundreds of ms) does not block the async runtime, and a
/// failed load is retried on the next process start rather than poisoning.
/// `None` inside the `OnceCell` Ok means the load failed once and analysis is
/// disabled for the process lifetime.
fn detector() -> &'static OnceCell<Option<std::sync::Arc<Mutex<RfDetrDetector>>>> {
    static DETECTOR: OnceCell<Option<std::sync::Arc<Mutex<RfDetrDetector>>>> = OnceCell::const_new();
    &DETECTOR
}

async fn get_detector() -> Option<std::sync::Arc<Mutex<RfDetrDetector>>> {
    detector()
        .get_or_init(|| async {
            // Loading touches the filesystem + builds an ONNX session; keep it
            // off the async worker thread.
            tokio::task::spawn_blocking(|| match RfDetrDetector::load() {
                Ok(d) => Some(std::sync::Arc::new(Mutex::new(d))),
                Err(e) => {
                    warn!("[vision_analysis] RF-DETR load failed, analysis disabled: {e:#}");
                    None
                }
            })
            .await
            .unwrap_or(None)
        })
        .await
        .clone()
}

/// Process-wide state classifier, loaded on first use with the same lazy
/// `OnceCell` + `spawn_blocking` pattern as the detector. A failed load is
/// `None` for the process lifetime: detections still publish, just without a
/// `stan` (condition is skipped, never a crash).
fn classifier() -> &'static OnceCell<Option<std::sync::Arc<Mutex<StateClassifier>>>> {
    static CLASSIFIER: OnceCell<Option<std::sync::Arc<Mutex<StateClassifier>>>> =
        OnceCell::const_new();
    &CLASSIFIER
}

async fn get_classifier() -> Option<std::sync::Arc<Mutex<StateClassifier>>> {
    classifier()
        .get_or_init(|| async {
            tokio::task::spawn_blocking(|| match StateClassifier::load() {
                Ok(c) => Some(std::sync::Arc::new(Mutex::new(c))),
                Err(e) => {
                    warn!("[vision_analysis] state classifier load failed, stan skipped: {e:#}");
                    None
                }
            })
            .await
            .unwrap_or(None)
        })
        .await
        .clone()
}

/// Process-wide plate OCR runner, loaded on first use with the same lazy
/// `OnceCell` + `spawn_blocking` pattern as the detector. A failed load is
/// `None` for the process lifetime: detections still publish, just without
/// `tekst` (OCR is skipped, never a crash).
fn ocr() -> &'static OnceCell<Option<std::sync::Arc<Mutex<PlateOcr>>>> {
    static OCR: OnceCell<Option<std::sync::Arc<Mutex<PlateOcr>>>> = OnceCell::const_new();
    &OCR
}

async fn get_ocr() -> Option<std::sync::Arc<Mutex<PlateOcr>>> {
    ocr()
        .get_or_init(|| async {
            tokio::task::spawn_blocking(|| match PlateOcr::load() {
                Ok(o) => Some(std::sync::Arc::new(Mutex::new(o))),
                Err(e) => {
                    warn!("[vision_analysis] plate OCR load failed, tekst skipped: {e:#}");
                    None
                }
            })
            .await
            .unwrap_or(None)
        })
        .await
        .clone()
}

/// True for detection classes whose condition we classify (placards/labels and
/// the environmental/temperature marks). License plates (`tablica_*`) are
/// skipped here — they go to OCR later.
fn wants_state(klasa: &str) -> bool {
    klasa.starts_with("nalepka") || klasa == "znak_srodowiskowy" || klasa == "termometr"
}

/// Extracts an RGB24 rectangle from a tightly packed RGB frame (stride = w*3).
/// `x0`/`y0`/`cw`/`ch` are already pixel coordinates clamped to the frame.
fn crop_rgb(frame: &[u8], frame_w: u32, x0: u32, y0: u32, cw: u32, ch: u32) -> Vec<u8> {
    let stride = frame_w as usize * 3;
    let mut out = Vec::with_capacity(cw as usize * ch as usize * 3);
    for row in 0..ch as usize {
        let src_y = y0 as usize + row;
        let start = src_y * stride + x0 as usize * 3;
        out.extend_from_slice(&frame[start..start + cw as usize * 3]);
    }
    out
}

/// Max cameras stacked into a single detector `Session::run`. One GPU launch is
/// amortized across the batch — the fleet throughput lever.
const MAX_BATCH: usize = 16;

/// Longest idle nap when no camera is due. The loop normally runs back-to-back
/// (continuous batching); this only caps how stale the "nothing due" wait can be
/// (e.g. when the registry is empty) so newly added cameras start promptly.
const IDLE_POLL_MAX: Duration = Duration::from_millis(20);

/// Shortest idle nap, to avoid busy-spinning when the next deadline is imminent.
const IDLE_POLL_MIN: Duration = Duration::from_millis(1);

/// How often each camera's `analysis_fps` is re-read from the core DB so an
/// operator's runtime change applies without restarting anything.
const FPS_RECHECK: Duration = Duration::from_secs(3);

/// One registered camera's scheduling state inside the shared engine.
struct CamSlot {
    fps: u32,
    next_due: std::time::Instant,
    next_fps_check: std::time::Instant,
}

/// Active-camera registry driven by the single engine task. Cameras join via
/// `ensure_analysis` and leave via `drain`.
fn cameras() -> &'static Mutex<HashMap<String, CamSlot>> {
    static CAMS: OnceLock<Mutex<HashMap<String, CamSlot>>> = OnceLock::new();
    CAMS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Handle to the single engine task, so `drain` can abort it.
fn engine_handle() -> &'static Mutex<Option<tokio::task::JoinHandle<()>>> {
    static H: OnceLock<Mutex<Option<tokio::task::JoinHandle<()>>>> = OnceLock::new();
    H.get_or_init(|| Mutex::new(None))
}

/// Registers `camera_id` for always-on analysis and starts the shared engine
/// once. Idempotent: re-subscribing a tile does not duplicate the camera.
pub fn ensure_analysis(camera_id: &str) {
    {
        let mut reg = cameras().lock().unwrap();
        if !reg.contains_key(camera_id) {
            let now = std::time::Instant::now();
            let fps = resolve_analysis_fps(camera_id);
            reg.insert(
                camera_id.to_string(),
                CamSlot {
                    fps,
                    next_due: now,
                    next_fps_check: now + FPS_RECHECK,
                },
            );
            info!("[vision_analysis] camera {camera_id} registered (analysis_fps={fps})");
        }
    }
    start_engine_once();
}

/// Removes every camera and aborts the engine task. Wired into camera shutdown
/// so the ONNX sessions and frame-pull loop stop before GStreamer tears down.
pub fn drain() {
    cameras().lock().unwrap().clear();
    if let Some(handle) = engine_handle().lock().unwrap().take() {
        handle.abort();
    }
}

/// Spawns the single engine task if it is not already running.
fn start_engine_once() {
    let mut h = engine_handle().lock().unwrap();
    if h.as_ref().map(|j| j.is_finished()).unwrap_or(true) {
        *h = Some(tokio::spawn(engine_loop()));
    }
}

/// The one process-wide analysis engine: collects cameras whose per-FPS
/// deadline elapsed, batches up to [`MAX_BATCH`] latest frames into a single
/// detector run, then per camera classifies state + reads plates on crops and
/// publishes. Cross-camera batching is what scales to thousands of cameras.
async fn engine_loop() {
    let detector = match get_detector().await {
        Some(d) => d,
        None => return, // load failed earlier — overlay still works, no detections
    };
    info!("[vision_analysis] cross-camera inference engine started (max_batch={MAX_BATCH})");

    // Continuous (adaptive) batching: after each batch we loop IMMEDIATELY and
    // form the next one from whatever became due while the previous inference ran,
    // so the GPU stays back-to-back under load instead of idling to a fixed timer.
    // A fixed tick would cap throughput at MAX_BATCH/tick and waste GPU whenever a
    // batch finishes faster than the tick — which is exactly what FP16/TensorRT do.
    loop {
        let now = std::time::Instant::now();

        // Collect due cameras + which need an FPS re-read (no DB under the lock).
        // Also track the soonest upcoming deadline so an idle wait sleeps exactly
        // until the next camera is due, not a fixed interval.
        let mut due: Vec<String> = Vec::new();
        let mut recheck: Vec<String> = Vec::new();
        let mut earliest_next: Option<std::time::Instant> = None;
        {
            let reg = cameras().lock().unwrap();
            for (id, slot) in reg.iter() {
                if slot.next_due <= now {
                    due.push(id.clone());
                    if slot.next_fps_check <= now {
                        recheck.push(id.clone());
                    }
                } else {
                    earliest_next = Some(match earliest_next {
                        Some(e) => e.min(slot.next_due),
                        None => slot.next_due,
                    });
                }
            }
        }
        if due.is_empty() {
            let wait = earliest_next
                .map(|t| t.saturating_duration_since(now))
                .unwrap_or(IDLE_POLL_MAX)
                .clamp(IDLE_POLL_MIN, IDLE_POLL_MAX);
            tokio::time::sleep(wait).await;
            continue;
        }
        // Re-read changed FPS values outside the lock.
        for id in &recheck {
            let fps = resolve_analysis_fps(id);
            let mut reg = cameras().lock().unwrap();
            if let Some(slot) = reg.get_mut(id) {
                slot.fps = fps;
                slot.next_fps_check = now + FPS_RECHECK;
            }
        }

        // Take this cycle's batch; cameras beyond MAX_BATCH stay overdue and are
        // picked next tick (drop-nothing, just deferred).
        let batch_ids: Vec<String> = due.iter().take(MAX_BATCH).cloned().collect();

        // Pull the latest frame for each batched camera (async snapshot).
        let mut frames: Vec<(String, std::sync::Arc<[u8]>, u32, u32)> = Vec::new();
        for id in &batch_ids {
            if let Some((rgb, w, h)) =
                crate::addon::host_functions::camera::latest_frame_global(id).await
            {
                frames.push((id.clone(), rgb, w, h));
            }
        }

        // Reschedule every batched camera by its own FPS interval.
        {
            let mut reg = cameras().lock().unwrap();
            for id in &batch_ids {
                if let Some(slot) = reg.get_mut(id) {
                    slot.next_due = now + interval_for_fps(slot.fps);
                }
            }
        }
        if frames.is_empty() {
            continue;
        }

        // HOT PATH: one batched detector run only (no OCR/classify here). Empty
        // results publish immediately to clear the overlay; non-empty go to the
        // cold path for per-detection enrichment + publish, keeping the hot loop
        // bounded to detector throughput.
        let detector = detector.clone();
        let detected =
            tokio::task::spawn_blocking(move || detect_only(detector, frames)).await;
        match detected {
            Ok(per_cam) => {
                for (id, frame, w, h, dets) in per_cam {
                    if dets.is_empty() {
                        detection_bus::publish_detections(&id, Vec::new());
                        continue;
                    }
                    let ev = DetectionEvent {
                        camera_id: id,
                        frame,
                        w,
                        h,
                        detections: dets,
                    };
                    // try_send: a full cold queue drops the event (backpressure).
                    // Chunk 0b adds coalescing, per-camera limits and metrics.
                    let _ = cold_sender().try_send(ev);
                }
            }
            Err(e) => warn!("[vision_analysis] detect task panicked: {e}"),
        }
    }
}

/// Bounded cold-path queue capacity. A full queue means enrichment can't keep up
/// with detections; events are dropped (overlay still got raw boxes via the empty
/// publish path is not affected). Chunk 0b refines this into per-camera limits.
const COLD_QUEUE_CAP: usize = 256;

/// One detection frame handed from the hot detector to the cold enrichment path:
/// the raw frame + the detector's boxes, to be enriched (state/OCR) and published.
struct DetectionEvent {
    camera_id: String,
    frame: Arc<[u8]>,
    w: u32,
    h: u32,
    detections: Vec<crate::services::detection_bus::Detection>,
}

/// Process-wide sender into the cold enrichment path. First use spawns the single
/// cold consumer task (which owns the classifier + OCR runners).
fn cold_sender() -> &'static mpsc::Sender<DetectionEvent> {
    static TX: OnceLock<mpsc::Sender<DetectionEvent>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<DetectionEvent>(COLD_QUEUE_CAP);
        tokio::spawn(cold_consumer(rx));
        tx
    })
}

/// Cold path: enriches each detection frame (state classify + plate OCR on crops)
/// off the hot detector loop, then publishes. Owns the classifier/OCR runners.
async fn cold_consumer(mut rx: mpsc::Receiver<DetectionEvent>) {
    let classifier = get_classifier().await;
    let ocr = get_ocr().await;
    info!("[vision_analysis] cold enrichment consumer started");
    while let Some(ev) = rx.recv().await {
        let DetectionEvent {
            camera_id,
            frame,
            w,
            h,
            detections,
        } = ev;
        let classifier = classifier.clone();
        let ocr = ocr.clone();
        let res = tokio::task::spawn_blocking(move || {
            let mut dets = detections;
            enrich_detections(&classifier, &ocr, &frame, w, h, &mut dets);
            (camera_id, dets)
        })
        .await;
        match res {
            Ok((id, dets)) => detection_bus::publish_detections(&id, dets),
            Err(e) => warn!("[vision_analysis] enrich task panicked: {e}"),
        }
    }
}

/// HOT, blocking: one `detect_batch` across the frames. Returns the raw boxes per
/// frame plus the frame buffer, for the cold path to enrich. No OCR/classify here.
fn detect_only(
    detector: Arc<Mutex<RfDetrDetector>>,
    frames: Vec<(String, Arc<[u8]>, u32, u32)>,
) -> Vec<(String, Arc<[u8]>, u32, u32, Vec<crate::services::detection_bus::Detection>)> {
    let batch = {
        let refs: Vec<(&[u8], u32, u32)> =
            frames.iter().map(|(_, rgb, w, h)| (&rgb[..], *w, *h)).collect();
        let mut guard = detector.lock().unwrap();
        match guard.detect_batch(&refs) {
            Ok(b) => b,
            Err(e) => {
                warn!("[vision_analysis] detect_batch failed (n={}): {e:#}", refs.len());
                return Vec::new();
            }
        }
    };
    frames
        .into_iter()
        .zip(batch.into_iter())
        .map(|((id, rgb, w, h), items)| (id, rgb, w, h, items))
        .collect()
}

/// COLD, blocking: per-detection state classify (labels) + plate OCR, mutating
/// `items` in place. Runs off the hot detector loop so its latency never paces
/// detection throughput. Missing runners (`None`) skip that stage, never crash.
fn enrich_detections(
    classifier: &Option<Arc<Mutex<StateClassifier>>>,
    ocr: &Option<Arc<Mutex<PlateOcr>>>,
    rgb: &[u8],
    w: u32,
    h: u32,
    items: &mut [crate::services::detection_bus::Detection],
) {
    for det in items.iter_mut() {
        let fw = w as f32;
        let fh = h as f32;
        let x0 = (det.bbox[0] * fw).round().clamp(0.0, fw) as u32;
        let y0 = (det.bbox[1] * fh).round().clamp(0.0, fh) as u32;
        let cw = (det.bbox[2] * fw).round().max(0.0) as u32;
        let ch = (det.bbox[3] * fh).round().max(0.0) as u32;
        let cw = cw.min(w.saturating_sub(x0));
        let ch = ch.min(h.saturating_sub(y0));
        if cw < 8 || ch < 8 {
            continue;
        }
        if wants_state(&det.klasa) {
            if let Some(classifier) = classifier.as_ref() {
                let crop = crop_rgb(rgb, w, x0, y0, cw, ch);
                match classifier.lock().unwrap().classify(&crop, cw, ch) {
                    Ok(stany) => det.stan = stany,
                    Err(e) => warn!("[vision_analysis] classify failed for {}: {e:#}", det.klasa),
                }
            }
        }
        if det.klasa == "tablica_rejestracyjna" {
            if let Some(ocr) = ocr.as_ref() {
                let crop = crop_rgb(rgb, w, x0, y0, cw, ch);
                match ocr.lock().unwrap().read(&crop, cw, ch) {
                    Ok(Some(plate)) => det.tekst = Some(plate),
                    Ok(None) => {}
                    Err(e) => warn!("[vision_analysis] OCR failed for {}: {e:#}", det.klasa),
                }
            }
        }
    }
}
