// =============================================================================
// File: services/camera_ingest/vision_analysis.rs — always-on CV analysis loop
// =============================================================================
//
// Per-camera always-on CV analysis driven by the camera's configurable
// `CvPipeline` (resolved from `camera_cv_pipelines`, see `cv_pipeline.rs`).
// The engine knows NO model aliases and NO class lists: every hot (frame)
// stage schedules at its own fps and runs the stage's model alias through
// `ModelRuntimeExecutor::execute_camera_cv`; every cold (per-crop) stage is
// interpreted generically (classify → `stan`, ocr → `tekst`).
//
// A pipeline resolve/parse failure keeps the last good pipeline; a camera
// that never resolved a pipeline does no analysis (no detections, no crash).

#![cfg(feature = "inference-vision-gpu")]

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, OnceCell};
use tracing::{debug, error, info, warn};

use crate::services::detection_bus::Detection;

use super::cv_pipeline::{self, CvOp, CvPipeline, CvStageInput, CvStageOutput};
use super::tracker;
use crate::flow_engine::dispatcher::{FlowActor, FlowOrigin};
use crate::flow_engine::dispatchers_impl::ModelRuntimeSlot;
use crate::services::detection_bus;
use crate::services::runtime::context::ExecutionContext as RuntimeContext;
use crate::services::runtime::executor::ModelRuntimeExecutor;
use crate::services::runtime::local_cv::{CameraCvOpLocal, CameraCvRequest, CvFrameLocal};
use crate::vision::classifier_stan::StateClassifier;
use crate::vision::detector_rfdetr::{RfDetrDetector, MODEL_BATCH};
use crate::vision::ocr_plate::PlateOcr;
use tentaflow_protocol::{CameraCvResult, CvDetection, CvOcrMode};

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

/// Process-wide RF-DETR detector, loaded on first use. `tokio::sync::OnceCell`
/// so a slow load (~hundreds of ms) does not block the async runtime, and a
/// failed load is retried on the next process start rather than poisoning.
/// `None` inside the `OnceCell` Ok means the load failed once and analysis is
/// disabled for the process lifetime. Used by the executor's embedded local
/// handler (`local_cv`), not directly by the analysis engine.
fn detector() -> &'static OnceCell<Option<DetectorHandle>> {
    static DETECTOR: OnceCell<Option<DetectorHandle>> = OnceCell::const_new();
    &DETECTOR
}

pub(crate) async fn get_detector() -> Option<DetectorHandle> {
    detector()
        .get_or_init(|| async {
            // Loading touches the filesystem + builds the ONNX session pool; keep
            // it off the async worker thread.
            tokio::task::spawn_blocking(|| match RfDetrDetector::load() {
                Ok(d) => Some(wrap_detector(d)),
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

/// Handle to the process-wide classifier/OCR singletons. On the ort path
/// (`vision-ort`) the runner is internally pooled + `&self` + Send+Sync,
/// so it is shared bare as `Arc<_>` and every crop rides the concurrency-safe ort
/// pool off the single Burn/wgpu thread. On the Burn path the runner still needs
/// the whole-process wgpu serialization, so it stays behind `Arc<Mutex<_>>` and
/// callers funnel forwards through `burn_backend::run_blocking`.
#[cfg(feature = "vision-ort")]
pub(crate) type DetectorHandle = std::sync::Arc<RfDetrDetector>;
#[cfg(not(feature = "vision-ort"))]
pub(crate) type DetectorHandle = std::sync::Arc<Mutex<RfDetrDetector>>;
#[cfg(feature = "vision-ort")]
pub(crate) type ClassifierHandle = std::sync::Arc<StateClassifier>;
#[cfg(not(feature = "vision-ort"))]
pub(crate) type ClassifierHandle = std::sync::Arc<Mutex<StateClassifier>>;
#[cfg(feature = "vision-ort")]
pub(crate) type OcrHandle = std::sync::Arc<PlateOcr>;
#[cfg(not(feature = "vision-ort"))]
pub(crate) type OcrHandle = std::sync::Arc<Mutex<PlateOcr>>;

#[cfg(feature = "vision-ort")]
fn wrap_detector(d: RfDetrDetector) -> DetectorHandle {
    std::sync::Arc::new(d)
}
#[cfg(not(feature = "vision-ort"))]
fn wrap_detector(d: RfDetrDetector) -> DetectorHandle {
    std::sync::Arc::new(Mutex::new(d))
}
#[cfg(feature = "vision-ort")]
fn wrap_classifier(c: StateClassifier) -> ClassifierHandle {
    std::sync::Arc::new(c)
}
#[cfg(not(feature = "vision-ort"))]
fn wrap_classifier(c: StateClassifier) -> ClassifierHandle {
    std::sync::Arc::new(Mutex::new(c))
}
#[cfg(feature = "vision-ort")]
fn wrap_ocr(o: PlateOcr) -> OcrHandle {
    std::sync::Arc::new(o)
}
#[cfg(not(feature = "vision-ort"))]
fn wrap_ocr(o: PlateOcr) -> OcrHandle {
    std::sync::Arc::new(Mutex::new(o))
}

/// Process-wide YOLOv8 vehicle detector, loaded on first use — the SECOND
/// detector run in parallel with RF-DETR. Own ort session pool (independent CUDA
/// streams), so a `tokio::join!` of the two forwards costs ~max(DETR, YOLO). A
/// failed/absent load is `None` for the process lifetime: association degrades
/// to RF-DETR-only (no vehicle boxes, every sign keeps `vehicle_id = 0`), never
/// a crash. Only the ort path builds a real detector; the Burn path has no
/// YOLOv8 vehicle graph, so it is always `None`.
#[cfg(feature = "vision-ort")]
pub(crate) type VehicleHandle = std::sync::Arc<crate::vision::detector_vehicle::VehicleDetector>;

#[cfg(feature = "vision-ort")]
fn vehicle_detector() -> &'static OnceCell<Option<VehicleHandle>> {
    static VEHICLE: OnceCell<Option<VehicleHandle>> = OnceCell::const_new();
    &VEHICLE
}

#[cfg(feature = "vision-ort")]
pub(crate) async fn get_vehicle_detector() -> Option<VehicleHandle> {
    vehicle_detector()
        .get_or_init(|| async {
            tokio::task::spawn_blocking(|| {
                match crate::vision::detector_vehicle::VehicleDetector::load() {
                    Ok(d) => Some(std::sync::Arc::new(d)),
                    Err(e) => {
                        warn!(
                            "[vision_analysis] YOLOv8 vehicle detector load failed, \
                             per-truck association disabled: {e:#}"
                        );
                        None
                    }
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
fn classifier() -> &'static OnceCell<Option<ClassifierHandle>> {
    static CLASSIFIER: OnceCell<Option<ClassifierHandle>> = OnceCell::const_new();
    &CLASSIFIER
}

pub(crate) async fn get_classifier() -> Option<ClassifierHandle> {
    classifier()
        .get_or_init(|| async {
            tokio::task::spawn_blocking(|| match StateClassifier::load() {
                Ok(c) => Some(wrap_classifier(c)),
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
fn ocr() -> &'static OnceCell<Option<OcrHandle>> {
    static OCR: OnceCell<Option<OcrHandle>> = OnceCell::const_new();
    &OCR
}

pub(crate) async fn get_ocr() -> Option<OcrHandle> {
    ocr()
        .get_or_init(|| async {
            tokio::task::spawn_blocking(|| match PlateOcr::load() {
                Ok(o) => Some(wrap_ocr(o)),
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

/// Slot executora runtime współdzielony z routerem (`Router.executor`).
/// Ustawiany raz przez `set_runtime_slot` przy inicjalizacji routera; sam slot
/// jest `RwLock<Option<..>>`, więc executor może pojawić się później — pętla
/// silnika czyta go świeżo co iterację i CZEKA (retry z krótkim snem), dopóki
/// nie jest wpięty. Etapy pipeline'u rozwiązują modele WYŁĄCZNIE przez executor
/// (aliasy katalogowe) — nie ma ścieżki bezpośredniej do singletonów.
fn runtime_slot_cell() -> &'static OnceLock<ModelRuntimeSlot> {
    static S: OnceLock<ModelRuntimeSlot> = OnceLock::new();
    &S
}

/// Wpina slot executora routera do silnika analizy. Idempotentne — pierwszy
/// zapis wygrywa (router tworzy jeden slot na proces).
pub fn set_runtime_slot(slot: ModelRuntimeSlot) {
    let _ = runtime_slot_cell().set(slot);
}

/// Aktualny executor runtime albo `None` (slot niewpięty / jeszcze pusty).
fn runtime_executor() -> Option<Arc<ModelRuntimeExecutor>> {
    runtime_slot_cell()
        .get()
        .and_then(|s| s.read().as_ref().cloned())
}

/// Ostrzeżenie ograniczone do jednego wpisu na ~30 s per klucz — błąd executora
/// może dotyczyć każdej klatki, więc bez limitu log byłby zalany przy
/// 10 fps × N kamer.
fn warn_throttled(key: &'static str, msg: &str) {
    const WARN_EVERY: Duration = Duration::from_secs(30);
    static LAST: OnceLock<Mutex<HashMap<&'static str, Instant>>> = OnceLock::new();
    let map = LAST.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
    let due = guard
        .get(key)
        .map(|t| t.elapsed() >= WARN_EVERY)
        .unwrap_or(true);
    if due {
        guard.insert(key, Instant::now());
        warn!("[vision_analysis] {msg}");
    }
}

/// Ostrzeżenie logowane dokładnie raz per klucz na życie procesu (np. jawny
/// skip etapu `embed`, dopóki surface CameraCv nie ma operacji Embed).
fn warn_once(key: &str, msg: &str) {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let set = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    if set
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key.to_string())
    {
        warn!("[vision_analysis] {msg}");
    }
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

/// Cut an RGB24 crop for one detection out of the full-res crops `frame`,
/// dispatching on its pixel format. `Rgb24` → [`crop_rgb`] (unchanged host path);
/// `Nv12` → [`super::fakefile::crop_nv12`], which converts ONLY the crop rectangle
/// (never the full 4K frame) to RGB24 using the parity-matched NV12→RGB formula.
/// Even x/y origins for the NV12 chroma alignment are handled inside `crop_nv12`;
/// the returned crop's real (possibly even-snapped) dims are returned so the
/// caller feeds the model the exact bytes it produced.
///
/// Zero-copy crops (`frame_device` = `Some`, `frame` empty): the crop is cut
/// straight off the DEVICE NV12 surface — only the small crop sub-rectangle is
/// downloaded, never the full 4K frame — via
/// [`super::fakefile::DeviceCropsFrame::crop_detection_rgb`], which reuses the
/// SAME host `crop_nv12` on the downloaded sub-frame so the RGB is bit-identical
/// to the host path. On any map/download error it falls back to the host `frame`
/// (which the caller materialized) — never loses enrichment.
fn crop_for_detection(
    frame: &[u8],
    frame_device: Option<&super::fakefile::DeviceCropsFrame>,
    frame_w: u32,
    frame_h: u32,
    format: &super::fakefile::DetectFrameFormat,
    x0: u32,
    y0: u32,
    cw: u32,
    ch: u32,
) -> (Vec<u8>, u32, u32) {
    // Zero-copy crops: cut off the device surface (small per-crop download).
    #[cfg(all(
        any(target_os = "linux", target_os = "windows"),
        feature = "inference-vision-gpu",
        feature = "vision-ort",
        feature = "vision-cuda-preprocess"
    ))]
    if let Some(dev) = frame_device {
        if let Some(out) = dev.crop_detection_rgb(x0, y0, cw, ch) {
            return out;
        }
    }
    let _ = frame_device;
    match *format {
        super::fakefile::DetectFrameFormat::Rgb24 => {
            (crop_rgb(frame, frame_w, x0, y0, cw, ch), cw, ch)
        }
        super::fakefile::DetectFrameFormat::Nv12 {
            y_stride,
            uv_stride,
            y_offset,
            uv_offset,
            kr,
            kb,
            full_range,
        } => {
            let (rgb, _, _, ecw, ech) = super::fakefile::crop_nv12(
                frame, frame_w, frame_h, y_stride, uv_stride, y_offset, uv_offset, kr, kb,
                full_range, x0, y0, cw, ch,
            );
            (rgb, ecw, ech)
        }
    }
}

/// Remaining zero-copy CROPS verify frames. `[vision] zerocopy_crops_verify`
/// seeds it (8 crops); each verified crop decrements it. `0` (off, the default)
/// makes [`verify_zerocopy_crop`] a cheap no-op.
fn verify_crops_counter() -> &'static AtomicU64 {
    static C: OnceLock<AtomicU64> = OnceLock::new();
    C.get_or_init(|| {
        AtomicU64::new(if crate::vision::settings::get().zerocopy_crops_verify {
            8
        } else {
            0
        })
    })
}

/// VERIFY gate (`[vision] zerocopy_crops_verify`): for the first few crops, cut
/// the SAME detection rect from the full host-downloaded NV12 frame and assert the
/// RGB is byte-identical to the zero-copy device crop (`device_rgb`). Correctness
/// gate for the per-crop device path. Logged, never panics — a mismatch surfaces
/// without killing a live camera. No-op when the gate is off or no device
/// frame is present.
#[allow(clippy::too_many_arguments)]
fn verify_zerocopy_crop(
    frame_device: Option<&super::fakefile::DeviceCropsFrame>,
    frame_w: u32,
    frame_h: u32,
    _format: &super::fakefile::DetectFrameFormat,
    x0: u32,
    y0: u32,
    cw: u32,
    ch: u32,
    device_rgb: &[u8],
) {
    let Some(dev) = frame_device else {
        return;
    };
    if verify_crops_counter().load(AtomicOrdering::Relaxed) == 0 {
        return;
    }
    // Reference: download the FULL NV12 and cut the same rect on the host.
    let Some((nv12, fmt)) = dev.download_full_nv12() else {
        return;
    };
    let super::fakefile::DetectFrameFormat::Nv12 {
        y_stride,
        uv_stride,
        y_offset,
        uv_offset,
        kr,
        kb,
        full_range,
    } = fmt
    else {
        return;
    };
    let (host_rgb, _, _, _, _) = super::fakefile::crop_nv12(
        &nv12, frame_w, frame_h, y_stride, uv_stride, y_offset, uv_offset, kr, kb, full_range, x0,
        y0, cw, ch,
    );
    verify_crops_counter().fetch_sub(1, AtomicOrdering::Relaxed);
    if host_rgb.len() != device_rgb.len() {
        error!(
            "[vision_analysis] zero-copy crops VERIFY: length mismatch device={} host={}",
            device_rgb.len(),
            host_rgb.len()
        );
        return;
    }
    let mismatches = host_rgb
        .iter()
        .zip(device_rgb.iter())
        .filter(|(a, b)| a != b)
        .count();
    if mismatches == 0 {
        info!(
            "[vision_analysis] zero-copy crops VERIFY: device crop MATCHES host download (bytes={})",
            device_rgb.len()
        );
    } else {
        error!(
            "[vision_analysis] zero-copy crops VERIFY: MISMATCH device vs host ({mismatches}/{} bytes differ)",
            device_rgb.len()
        );
    }
}

/// Returns an RGB24 view of the full crops frame, converting from NV12 only when
/// needed (rare paths: analysis-flow image blob). RGB frames pass through as the
/// same `Arc` (zero copy).
fn nv12_to_rgb_if_needed(
    frame: &Arc<[u8]>,
    w: u32,
    h: u32,
    format: &super::fakefile::DetectFrameFormat,
) -> Arc<[u8]> {
    match super::fakefile::nv12_frame_to_rgb24(frame, w, h, format) {
        Some(rgb) => Arc::from(rgb),
        None => frame.clone(),
    }
}

/// Piksele cropu detekcji z opcjonalnym paddingiem (ułamek szer./wys. boxa,
/// z każdej strony, zaklamrowany do granic klatki). Bbox detektora często
/// UCINA prawą część tablicy — padding pod OCR łapie uciętą część. Zwraca
/// `(x0, y0, cw, ch)` albo `None`, gdy box (przed lub po paddingu) jest
/// mniejszy niż 8 px w którymkolwiek wymiarze.
fn padded_crop_rect(
    w: u32,
    h: u32,
    bbox: &[f32; 4],
    pad_x: f32,
    pad_y: f32,
) -> Option<(u32, u32, u32, u32)> {
    let fw = w as f32;
    let fh = h as f32;
    let x0 = (bbox[0] * fw).round().clamp(0.0, fw) as u32;
    let y0 = (bbox[1] * fh).round().clamp(0.0, fh) as u32;
    let cw = (bbox[2] * fw).round().max(0.0) as u32;
    let ch = (bbox[3] * fh).round().max(0.0) as u32;
    let cw = cw.min(w.saturating_sub(x0));
    let ch = ch.min(h.saturating_sub(y0));
    if cw < 8 || ch < 8 {
        return None;
    }
    let px = (cw as f32 * pad_x).round() as u32;
    let py = (ch as f32 * pad_y).round() as u32;
    let nx0 = x0.saturating_sub(px);
    let ny0 = y0.saturating_sub(py);
    let right = (x0 + cw + px).min(w);
    let bottom = (y0 + ch + py).min(h);
    let ncw = right.saturating_sub(nx0);
    let nch = bottom.saturating_sub(ny0);
    if ncw < 8 || nch < 8 {
        return None;
    }
    Some((nx0, ny0, ncw, nch))
}

/// Okno flush time-batchingu: co ~8 ms robimy flush z tym, co aktualnie jest w
/// `pending`, NIEZALEŻNIE od tego ile się nazbierało — batch NIE musi być pełny.
/// Wcześniejszy flush (bez czekania na okno) następuje tylko, gdy jakaś grupa
/// (alias, threshold) osiągnie pełny chunk `MODEL_BATCH`. Krótkie okno trzyma
/// latencję nisko przy MAŁEJ liczbie kamer: klatka nie czeka na dopełnienie
/// batcha, lecz wychodzi do forwardu w ciągu ~8 ms.
const MAX_BATCH_WAIT: Duration = Duration::from_millis(8);

/// Longest idle nap when no camera is due. The loop normally runs back-to-back
/// (continuous batching); this only caps how stale the "nothing due" wait can be
/// (e.g. when the registry is empty) so newly added cameras start promptly.
const IDLE_POLL_MAX: Duration = Duration::from_millis(20);

/// Shortest idle nap, to avoid busy-spinning when the next deadline is imminent.
const IDLE_POLL_MIN: Duration = Duration::from_millis(1);

/// How often each camera's config (`analysis_fps` + resolved CV pipeline) is
/// re-read from the core DB so an operator's runtime change (GUI) applies
/// within seconds without restarting anything.
const CFG_RECHECK: Duration = Duration::from_secs(3);

/// Sleep between engine retries while the runtime executor slot is still
/// empty (bootstrap): stages resolve models only via the executor, so the
/// engine waits for it instead of falling back to direct singletons.
const EXECUTOR_WAIT: Duration = Duration::from_millis(200);

/// After this much continuous waiting for the executor the engine escalates
/// to `error!` — a bootstrap should fill the slot in well under 30 s, so a
/// longer wait means analysis is silently stalled.
const EXECUTOR_STALL_ERROR_AFTER: Duration = Duration::from_secs(30);

/// Minimum spacing between repeated stall `error!` lines while still waiting.
const EXECUTOR_STALL_ERROR_EVERY: Duration = Duration::from_secs(300);

/// A detection forward that hangs (blocked on a lock/resource, GPU idle) leaves
/// its job in `jobs`, and the per-camera ordering gate then blocks EVERY new
/// frame of that camera until it clears — observed as a 2–4 min analysis outage
/// (emitted+coalesced frozen, overlay blank). Nothing aborts a stuck
/// `spawn_blocking`, so the loop force-evicts any job older than this, freeing the
/// gate; a fresh forward spawns on a free inflight permit and analysis resumes in
/// seconds. The evicted forward, when it finally returns, hits a missing job in
/// `stage_completed` and is dropped. Chosen well above a normal forward (~20 ms)
/// so healthy frames are never evicted.
const FORWARD_STALL_TIMEOUT: Duration = Duration::from_secs(4);

/// One registered camera's scheduling state inside the shared engine.
struct CamSlot {
    /// Camera-level `cameras.analysis_fps` — the fallback cadence for frame
    /// stages that do not set their own `fps`.
    fps: u32,
    /// Last good parsed pipeline. `None` until the first successful resolve —
    /// the camera does no analysis without a pipeline.
    pipeline: Option<Arc<CvPipeline>>,
    /// Raw `(pipeline_id, pipeline_json)` of the installed pipeline — cheap
    /// change detection without reparsing on every recheck.
    pipeline_raw: Option<(String, String)>,
    /// Per frame-stage next deadline (stage_id → due time).
    stage_due: HashMap<String, std::time::Instant>,
    next_cfg_check: std::time::Instant,
    /// Last config warning already logged — resolve/parse failures repeat
    /// every recheck, so we warn once per DISTINCT message, not per tick.
    last_cfg_err: Option<String>,
    /// Detection zones (`cameras.zones_json`), parsed into normalized polygons.
    /// EMPTY = no zones = whole frame live. Shared with each frame job by
    /// `Arc` clone so the hot path never re-parses or copies point data.
    zones: Arc<Vec<Vec<(f32, f32)>>>,
    /// Raw zones JSON of the parsed value — cheap change detection on recheck.
    zones_raw: String,
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
            // Start bez zapytania DB (funkcja jest sync i bywa wolana z watku
            // tokio); `next_cfg_check = now` kaze petli silnika odczytac fps i
            // pipeline z DB na puli blocking przy pierwszym ticku.
            reg.insert(
                camera_id.to_string(),
                CamSlot {
                    fps: DEFAULT_ANALYSIS_FPS,
                    pipeline: None,
                    pipeline_raw: None,
                    stage_due: HashMap::new(),
                    next_cfg_check: now,
                    last_cfg_err: None,
                    zones: Arc::new(Vec::new()),
                    zones_raw: String::new(),
                },
            );
            info!("[vision_analysis] camera {camera_id} registered");
        }
    }
    start_engine_once();
}

/// Removes every camera and aborts the engine task. Wired into camera shutdown
/// so the ONNX sessions and frame-pull loop stop before GStreamer tears down.
pub async fn drain() {
    cameras().lock().unwrap().clear();
    // Poproś pętlę o graceful shutdown i POCZEKAJ, aż dokończy trwające forwardy
    // (i wyjdzie), zamiast tylko ją abortować: abort nie anuluje biegnącego
    // `spawn_blocking` GPU-forwardu, więc stara inferencja mogłaby nałożyć się na
    // restart i K przestałoby ograniczać realną pracę GPU. Await-drain to gwarancja
    // braku nakładki.
    shutdown_flag().store(true, AtomicOrdering::Relaxed);
    let handle = engine_handle().lock().unwrap().take();
    if let Some(handle) = handle {
        let abort = handle.abort_handle();
        // Bounded await: forwardy kończą się w dziesiątkach ms; po limicie abort
        // jako fallback. WTEDY biegnący `spawn_blocking` może jeszcze trwać na puli
        // blocking — jest to jednak ograniczone (pula sesji ort serializuje per
        // sesja) i zdarza się tylko przy realnie zawieszonym forwardzie.
        if tokio::time::timeout(FORWARD_DRAIN_TIMEOUT, handle)
            .await
            .is_err()
        {
            warn!(
                "[vision_analysis] engine drain timed out after {}s; aborting (a blocking forward may still finish on the ort pool)",
                FORWARD_DRAIN_TIMEOUT.as_secs()
            );
            abort.abort();
        }
    }
    shutdown_flag().store(false, AtomicOrdering::Relaxed);
    // Po wyjściu pętli (żaden forward już nie działa) wyczyść stan trackera, by po
    // restarcie analizy nie zostal martwy stan (tracki, licznik id).
    tracker::clear();
    // Tear down the cold path too, so its enrichment stops and a later
    // `ensure_analysis` restarts a fresh consumer (the channel is recreated).
    if let Some(handle) = cold_handle().lock().unwrap().take() {
        handle.abort();
    }
    *cold_chan().lock().unwrap() = None;
    // Reset coalescer state + byte counter: the aborted consumer never released
    // its in-flight slots, so stale `in_flight = true` would permanently block
    // those cameras after a restart.
    cold_state().lock().unwrap().clear();
    cold_bytes().store(0, AtomicOrdering::Relaxed);
    // Cache wzbogacania trzyma stan/OCR per (camera_id, stage_id, track_id) —
    // po drainie tracki znikaja (tracker::clear resetuje licznik id), wiec stare
    // wpisy musza zniknac, by nie przypisac stanu do przypadkiem powtorzonego id.
    enrich_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

/// Usuwa proces-wide stan analizy JEDNEJ kamery przy jej teardownie (obok
/// `tracker::remove`): wpisy cache wzbogacania kluczowane
/// (camera_id, stage_id, track_id). Bez tego szybkie usunięcie + ponowne
/// dodanie kamery o tym samym id mogłoby w oknie `ENRICH_TTL` przypisać nowym
/// trackom stan/tekst poprzedniej sesji (licznik track_id startuje od 1).
pub(crate) fn forget_camera(camera_id: &str) {
    enrich_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(cam, _, _), _| cam != camera_id);
}

/// Spawns the single engine task if it is not already running.
fn start_engine_once() {
    let mut h = engine_handle().lock().unwrap();
    if h.as_ref().map(|j| j.is_finished()).unwrap_or(true) {
        // Wyczyść ewentualną flagę shutdown po poprzednim `drain`, żeby świeża
        // pętla nie weszła od razu w tryb graceful-exit.
        shutdown_flag().store(false, AtomicOrdering::Relaxed);
        *h = Some(tokio::spawn(engine_loop()));
    }
}

/// Reads a camera's runtime config from the core DB on the blocking pool:
/// `analysis_fps` plus the resolved `(pipeline_id, pipeline_json)` pair.
/// The pipeline result distinguishes "resolved to nothing" (`Ok(None)` — no
/// pipeline exists at all, camera stops analyzing) from a resolve FAILURE
/// (`Err` — DB error, keep the last good pipeline).
async fn resolve_camera_config(
    camera_id: &str,
) -> (u32, Result<Option<(String, String)>, String>, String) {
    let id = camera_id.to_string();
    tokio::task::spawn_blocking(move || match crate::db::global_pool() {
        Some(pool) => {
            let fps = crate::db::repository::camera_analysis_fps(&pool, &id)
                .unwrap_or(DEFAULT_ANALYSIS_FPS);
            let pipeline = crate::db::repository::resolve_camera_cv_pipeline(&pool, &id)
                .map_err(|e| e.to_string());
            // Zones are advisory: a read failure must never stop analysis, it
            // just means "no zones this tick" (whole frame stays live).
            let zones = crate::db::repository::camera_zones_json(&pool, &id)
                .unwrap_or_else(|_| "[]".to_string());
            (fps, pipeline, zones)
        }
        None => (
            DEFAULT_ANALYSIS_FPS,
            Err("no global DB pool".to_string()),
            "[]".to_string(),
        ),
    })
    .await
    .unwrap_or((
        DEFAULT_ANALYSIS_FPS,
        Err("camera config task panicked".to_string()),
        "[]".to_string(),
    ))
}

/// Applies a freshly resolved camera config to the registry slot. Invalid or
/// unresolvable pipelines never crash: a failure keeps the last good pipeline
/// (warned once per distinct message), `Ok(None)` clears it (no analysis).
/// Parses `cameras.zones_json` into normalized polygons. Shape:
/// `[[[x,y],[x,y],...], ...]` with every coordinate in `0.0..=1.0`. Anything
/// malformed is skipped rather than failing the camera — a bad zone must never
/// take analysis down, it just does not constrain it. Polygons with fewer than
/// three points cannot bound an area and are dropped.
fn parse_zone_polygons(raw: &str) -> Vec<Vec<(f32, f32)>> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    let Some(list) = value.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for poly in list {
        let Some(points) = poly.as_array() else {
            continue;
        };
        let mut pts: Vec<(f32, f32)> = Vec::with_capacity(points.len());
        for p in points {
            let Some(pair) = p.as_array() else { continue };
            if pair.len() < 2 {
                continue;
            }
            let (Some(x), Some(y)) = (pair[0].as_f64(), pair[1].as_f64()) else {
                continue;
            };
            pts.push((x as f32, y as f32));
        }
        if pts.len() >= 3 {
            out.push(pts);
        }
    }
    out
}

/// Ray-casting point-in-polygon on normalized coordinates. Points exactly on an
/// edge may land either way — irrelevant for a hand-drawn operator zone.
fn point_in_polygon(x: f32, y: f32, poly: &[(f32, f32)]) -> bool {
    let mut inside = false;
    let n = poly.len();
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = poly[i];
        let (xj, yj) = poly[j];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Keeps only detections whose box CENTRE falls inside at least one zone. With
/// no zones every detection passes, so cameras without drawn zones behave
/// exactly as before. Applied to the raw detector output — an out-of-zone box
/// never reaches tracking, enrichment (OCR/classify), the overlay or the event
/// recorder, so a zone also buys back the per-frame budget it excludes.
fn retain_in_zones(dets: &mut Vec<Detection>, zones: &[Vec<(f32, f32)>]) {
    if zones.is_empty() {
        return;
    }
    dets.retain(|d| {
        let cx = d.bbox[0] + d.bbox[2] * 0.5;
        let cy = d.bbox[1] + d.bbox[3] * 0.5;
        zones.iter().any(|p| point_in_polygon(cx, cy, p))
    });
}

fn apply_camera_config(
    camera_id: &str,
    fps: u32,
    resolved: Result<Option<(String, String)>, String>,
    zones_raw: String,
    now: std::time::Instant,
) {
    let mut reg = cameras().lock().unwrap();
    let Some(slot) = reg.get_mut(camera_id) else {
        return;
    };
    slot.fps = fps;
    // Reparse zones only when the stored JSON actually changed — the recheck
    // runs on every camera every few seconds.
    if slot.zones_raw != zones_raw {
        let parsed = parse_zone_polygons(&zones_raw);
        info!(
            "[vision_analysis] camera {camera_id}: {} detection zone(s) active",
            parsed.len()
        );
        slot.zones = Arc::new(parsed);
        slot.zones_raw = zones_raw;
    }
    slot.next_cfg_check = now + CFG_RECHECK;
    let warn_changed = |slot: &mut CamSlot, msg: String| {
        if slot.last_cfg_err.as_deref() != Some(msg.as_str()) {
            warn!("[vision_analysis] camera {camera_id}: {msg}");
            slot.last_cfg_err = Some(msg);
        }
    };
    match resolved {
        Err(e) => warn_changed(slot, format!("pipeline resolve failed, keeping last: {e}")),
        Ok(None) => {
            if slot.pipeline.is_some() {
                info!("[vision_analysis] camera {camera_id}: no CV pipeline resolvable, analysis stopped");
            }
            slot.pipeline = None;
            slot.pipeline_raw = None;
            slot.stage_due.clear();
            warn_changed(
                slot,
                "no CV pipeline resolvable; camera does no analysis".to_string(),
            );
        }
        Ok(Some(raw)) => {
            if slot.pipeline_raw.as_ref() == Some(&raw) {
                return;
            }
            let parsed = serde_json::from_str::<CvPipeline>(&raw.1)
                .map_err(|e| e.to_string())
                .and_then(|p| {
                    cv_pipeline::validate(&p)
                        .map(|_| p)
                        .map_err(|e| e.to_string())
                });
            match parsed {
                Ok(p) => {
                    // Zachowaj deadline'y etapow, ktore przetrwaly edycje;
                    // nowe etapy startuja natychmiast, usuniete znikaja.
                    let mut due = HashMap::new();
                    for fs in cv_pipeline::frame_stages(&p) {
                        let t = slot.stage_due.get(&fs.stage_id).copied().unwrap_or(now);
                        due.insert(fs.stage_id.clone(), t);
                    }
                    info!(
                        "[vision_analysis] camera {camera_id}: pipeline '{}' loaded ({} stages, {} frame)",
                        raw.0,
                        p.stages.len(),
                        due.len()
                    );
                    slot.stage_due = due;
                    slot.pipeline = Some(Arc::new(p));
                    slot.pipeline_raw = Some(raw);
                    slot.last_cfg_err = None;
                }
                Err(e) => warn_changed(
                    slot,
                    format!("invalid pipeline '{}', keeping last: {e}", raw.0),
                ),
            }
        }
    }
}

/// Jedna klatka jednego etapu `frame` czekająca w buforze hot path. Batch
/// grupuje pozycje po `(alias, threshold)` — jeden forward nigdy nie miesza
/// modeli. `job_id` wskazuje wspólny [`FrameJob`] klatki (etapy tej samej
/// klatki scala jedna publikacja).
struct PendingItem {
    job_id: u64,
    stage_id: String,
    alias: String,
    threshold: Option<f32>,
    added: Instant,
}

/// Jedna zaanalizowana klatka kamery: wspólny bufor RGB + zbiorcze wyniki
/// wszystkich etapów `frame` pipeline'u. Publikacja (FAZA 1) i zdarzenie cold
/// path powstają dopiero, gdy WSZYSTKIE etapy klatki mają wynik — jedna
/// wiadomość overlay per przeanalizowana klatka.
struct FrameJob {
    camera_id: String,
    /// Detection zones of this camera at the moment the frame was queued.
    /// EMPTY = whole frame live. Snapshotted per job so the hot path never
    /// touches the camera registry lock.
    zones: Arc<Vec<Vec<(f32, f32)>>>,
    frame: Arc<[u8]>,
    /// Zero-copy CROPS path ONLY: a DEVICE reference to the full-res NV12 frame.
    /// When `Some`, `frame` is EMPTY and enrichment cuts each detection's crop
    /// straight off this device surface (small per-crop download). The held
    /// `gst::Sample` keeps the surface alive until the cold event that consumes it
    /// finishes (bounded ~1 in-flight + 1 pending per camera). `None` otherwise.
    frame_device: Option<super::fakefile::DeviceCropsFrame>,
    w: u32,
    h: u32,
    /// Pixel layout of the full-res crops `frame`: `Rgb24` for every non-NVDEC
    /// path, `Nv12` (packed `[Y | UV]` + strides + color) on the GPU-resident
    /// path. Enrichment cuts crops with [`crop_nv12`] for NV12 and [`crop_rgb`]
    /// otherwise, so the per-frame full videoconvert never runs.
    frame_format: super::fakefile::DetectFrameFormat,
    /// Detector input frame: GPU-scaled 560×560 when the pipeline's detect
    /// branch is active, otherwise the same buffer as `frame` (detector then
    /// CPU-resizes it). The HOT detect forward reads this; enrichment crops
    /// always read the full-res `frame`/`w`/`h`, so crop→bbox math is unchanged.
    detect_frame: Arc<[u8]>,
    detect_w: u32,
    detect_h: u32,
    /// Pixel layout of `detect_frame`: `Rgb24` (host detect through the executor)
    /// or `Nv12` (device preprocess through `detect_batch_gpu`, GPU-resident
    /// path). Set from the ingest pipeline's detect branch.
    detect_format: super::fakefile::DetectFrameFormat,
    /// Zero-copy (Stage 4) ONLY: an owned, already-preprocessed device tensor
    /// (`[1,3,560,560]`) produced from the NVDEC surface in the appsink callback.
    /// When `Some`, the detect forward runs `detect_device_tensor` on it and
    /// ignores `detect_frame`/`detect_format`. Read only under the ORT GPU
    /// features; `None` on every other path.
    #[cfg_attr(
        not(all(
            any(target_os = "linux", target_os = "windows"),
            feature = "vision-ort"
        )),
        allow(dead_code)
    )]
    detect_device: Option<super::fakefile::DeviceDetectTensor>,
    captured_ms: u64,
    pts_ns: Option<u64>,
    pipeline: Arc<CvPipeline>,
    /// Etapy `frame` tej klatki bez wyniku (jeszcze w pending albo w locie).
    open_stages: Vec<String>,
    /// Wyniki ukończonych etapów: (stage_id, detekcje po trackerze i cache).
    results: Vec<(String, Vec<Detection>)>,
    /// Suma czasów forwardów etapów tej klatki (przybliżenie dla badge proc_ms).
    detect_ms_total: u32,
    /// Liczba etapów zakończonych błędem executora (bez wyniku).
    failed_stages: usize,
    /// Boxy pojazdow (`klasa="vehicle"`) wykryte przez YOLOv8 RÓWNOLEGLE z
    /// RF-DETR na TEJ SAMEJ klatce (tokio::join!). Trakowane osobnym trackerem
    /// IOU `(kamera,"vehicles")` w `stage_completed`, potem propagowane do cold
    /// path do asocjacji znak→pojazd. Puste, gdy model pojazdow niedostepny
    /// (degradacja do RF-DETR-only).
    vehicles: Vec<Detection>,
    /// Kiedy job trafił do `jobs` (in-flight). Bramka kolejności blokuje kolejne
    /// klatki kamery, dopóki jej job tu wisi; gdy forward inferencji zawiśnie na
    /// locku/zasobie, job nigdy się nie domyka i analiza kamery STOI na minuty.
    /// Pętla po [`FORWARD_STALL_TIMEOUT`] siłą usuwa taki job — bramka się zwalnia
    /// i analiza rusza w sekundę (zawieszony forward, gdy w końcu skończy, trafia
    /// na brak joba w `stage_completed` i jest pomijany).
    submitted_at: std::time::Instant,
}

/// Wynik jednego współbieżnego forwardu batcha zwrócony ze spawnowanego zadania
/// do pętli silnika. Sam FORWARD biegnie współbieżnie (do K naraz na puli sesji
/// ort); ten wynik jest STOSOWANY z powrotem WYŁĄCZNIE na pętli, więc `jobs`,
/// tracker i cache wzbogacania mają nadal jednego właściciela.
struct ForwardOutput {
    alias: String,
    detect_ms: u32,
    /// Wynik executora spłaszczony do `String` błędu — `Send` i prosty do
    /// przeniesienia przez granicę zadania (log identyczny z dawnym inline).
    outcome: Result<CameraCvResult, String>,
    /// Boxy pojazdow per klatka batcha (kolejnosc == kolejnosc batcha), policzone
    /// RÓWNOLEGLE z forwardem detekcji przez `tokio::join!` na osobnej puli sesji.
    /// Puste gdy model pojazdow niedostepny albo forward pojazdow zawiodl
    /// (degradacja do RF-DETR-only). `apply_forward_result` doklada je do jobów.
    vehicles: Vec<Vec<Detection>>,
}

/// Górny limit współbieżnych forwardów. Odzwierciedla sufit puli sesji ort
/// (`ort_common::MAX_SESSIONS_PER_MODEL` = 16) — więcej równoległych forwardów
/// niż sesji nie ma sensu (i tak czekałyby na slot puli). Zdefiniowany lokalnie,
/// bo pula ort istnieje tylko pod `vision-ort`, a ten limit musi
/// obowiązywać na każdej ścieżce.
const MAX_INFLIGHT: usize = 16;

/// Liczba współbieżnych forwardów K. Jawny opt-in `[vision] inflight` wygrywa;
/// w przeciwnym razie odwzorowuje rozmiar puli detektora
/// (`[vision] detector_sessions`, domyślnie 4): przy N sesjach GPU liczy N
/// detektów naraz, więc N in-flight to naturalny sufit. Z 4 sesjami w puli 4
/// pipelinowane batched forwardy dają ~3× przepustowości detektora
/// (~1300 vs ~430 frames/s na jednym GPU) względem serializacji.
fn inflight_limit() -> usize {
    let vision = crate::vision::settings::get();
    resolve_inflight_limit(vision.inflight, vision.detector_sessions)
}

/// Pure resolution behind [`inflight_limit`], split out so the precedence and
/// clamps are unit-testable without touching the frozen process settings.
fn resolve_inflight_limit(inflight: Option<usize>, detector_sessions: usize) -> usize {
    inflight.unwrap_or(detector_sessions).clamp(1, MAX_INFLIGHT)
}

/// Grouping key that keeps a detect flush batch homogeneous: `None` for RGB
/// (executor path), `Some(color-bits)` for NV12 (device path — the bits pin the
/// YUV→RGB matrix so one `detect_batch_gpu` call applies a single conversion).
/// Without the ort GPU features NV12 frames are never produced, so every frame
/// keys as RGB.
#[cfg(all(feature = "vision-ort", feature = "vision-cuda-preprocess"))]
fn detect_batch_key(fmt: &super::fakefile::DetectFrameFormat) -> Option<(u32, u32, u32)> {
    match fmt {
        super::fakefile::DetectFrameFormat::Rgb24 => None,
        super::fakefile::DetectFrameFormat::Nv12 {
            kr, kb, full_range, ..
        } => Some((kr.to_bits(), kb.to_bits(), *full_range as u32)),
    }
}
#[cfg(not(all(feature = "vision-ort", feature = "vision-cuda-preprocess")))]
fn detect_batch_key(_fmt: &super::fakefile::DetectFrameFormat) -> Option<(u32, u32, u32)> {
    None
}

/// Sentinel batch key for the zero-copy DEVICE detect path — distinct from RGB
/// (`None`) and any NV12 colorimetry key, so a device job never groups into an
/// RGB/NV12 flush batch (its input is a preprocessed device tensor, not pixels).
#[cfg(feature = "vision-ort")]
const DEVICE_BATCH_KEY: (u32, u32, u32) = (u32::MAX, u32::MAX, u32::MAX);

/// Job-level flush grouping key: the device sentinel when the job carries a
/// zero-copy device tensor, else the pixel-format key ([`detect_batch_key`]).
/// Defined for both feature sets so the flush call site is uniform (without the
/// ort GPU features `detect_device` is always `None`, so this is just the format
/// key).
fn job_batch_key(job: &FrameJob) -> Option<(u32, u32, u32)> {
    #[cfg(all(
        any(target_os = "linux", target_os = "windows"),
        feature = "vision-ort",
        feature = "vision-cuda-preprocess"
    ))]
    if job.detect_device.is_some() {
        return Some(DEVICE_BATCH_KEY);
    }
    detect_batch_key(&job.detect_format)
}

/// One NV12 detect-frame descriptor collected off a [`FrameJob`] for the device
/// forward: the packed `[Y | UV]` bytes plus plane strides/offsets and frame
/// dims. Owned (Arc-cloned) so the spawned forward can outlive the loop tick.
#[cfg(all(
    any(target_os = "linux", target_os = "windows"),
    feature = "vision-ort",
    feature = "vision-cuda-preprocess"
))]
#[derive(Clone)]
struct Nv12DetectInput {
    data: Arc<[u8]>,
    width: u32,
    height: u32,
    y_stride: u32,
    uv_stride: u32,
    y_offset: u32,
    uv_offset: u32,
}

/// YUV→RGB coefficients for an NV12 detect frame, or `None` for RGB.
#[cfg(all(
    any(target_os = "linux", target_os = "windows"),
    feature = "vision-ort",
    feature = "vision-cuda-preprocess"
))]
fn nv12_color(
    fmt: &super::fakefile::DetectFrameFormat,
) -> Option<crate::vision::gpu_preprocess::ColorCoeffs> {
    match fmt {
        super::fakefile::DetectFrameFormat::Nv12 {
            kr, kb, full_range, ..
        } => Some(crate::vision::gpu_preprocess::ColorCoeffs {
            kr: *kr,
            kb: *kb,
            full_range: *full_range,
        }),
        super::fakefile::DetectFrameFormat::Rgb24 => None,
    }
}

/// Runs one homogeneous NV12 detect batch through the RF-DETR detector's device
/// path (`detect_batch_gpu`): GPU YUV→RGB + resize + normalize + forward, with
/// zero CPU pixel work. Bypasses the executor (a device frame has no RGB wire
/// form for mesh failover); the returned [`CameraCvResult::Detections`] mirrors
/// the executor's Detect op so `apply_forward_result` treats both paths alike.
#[cfg(all(
    any(target_os = "linux", target_os = "windows"),
    feature = "vision-ort",
    feature = "vision-cuda-preprocess"
))]
async fn run_nv12_detect_forward(
    frames: Vec<Nv12DetectInput>,
    color: crate::vision::gpu_preprocess::ColorCoeffs,
    threshold: Option<f32>,
) -> Result<CameraCvResult, String> {
    let detector = get_detector()
        .await
        .ok_or_else(|| "detektor RF-DETR niedostępny (load nie powiódł się)".to_string())?;
    let batch = tokio::task::spawn_blocking(move || {
        use crate::vision::gpu_preprocess::Nv12Frame;
        let nv12: Vec<Nv12Frame> = frames
            .iter()
            .map(|f| Nv12Frame {
                y: &f.data[f.y_offset as usize..],
                y_stride: f.y_stride as usize,
                uv: &f.data[f.uv_offset as usize..],
                uv_stride: f.uv_stride as usize,
                w: f.width,
                h: f.height,
            })
            .collect();
        detector
            .detect_batch_gpu(&nv12, color, threshold)
            .map_err(|e| format!("detect_batch_gpu: {e:#}"))
    })
    .await
    .map_err(|e| format!("camera-cv nv12 detect executor: {e}"))??;
    let per_frame = batch
        .into_iter()
        .map(|dets| {
            dets.into_iter()
                .map(|d| CvDetection {
                    klasa: d.klasa,
                    bbox: d.bbox,
                    score: d.score,
                })
                .collect()
        })
        .collect();
    Ok(CameraCvResult::Detections { per_frame })
}

/// Runs a batch of zero-copy DEVICE detect frames: each carries an owned,
/// already-preprocessed `[1,3,560,560]` device tensor (produced from the NVDEC
/// surface in the appsink callback), so this ONLY runs the ORT forward + decode
/// (`detect_device_tensor`) — no preprocess. Results mirror the executor's Detect
/// op so `apply_forward_result` treats every detect path alike. Each tensor is a
/// separate device buffer, so they run one at a time (n=1) rather than as one
/// concatenated ORT input; the batch just amortizes the loop bookkeeping.
#[cfg(all(
    any(target_os = "linux", target_os = "windows"),
    feature = "vision-ort",
    feature = "vision-cuda-preprocess"
))]
async fn run_device_detect_forward(
    handles: Vec<super::fakefile::DeviceDetectTensor>,
    threshold: Option<f32>,
) -> Result<CameraCvResult, String> {
    let detector = get_detector()
        .await
        .ok_or_else(|| "detektor RF-DETR niedostępny (load nie powiódł się)".to_string())?;
    let batch = tokio::task::spawn_blocking(move || {
        let mut per_frame: Vec<Vec<CvDetection>> = Vec::with_capacity(handles.len());
        for h in &handles {
            let tensor =
                h.0.clone()
                    .downcast::<crate::vision::gpu_preprocess::OwnedDeviceTensor>()
                    .map_err(|_| "device detect handle type mismatch".to_string())?;
            let dets = detector
                .detect_device_tensor(&tensor, threshold)
                .map_err(|e| format!("detect_device_tensor: {e:#}"))?;
            per_frame.push(
                dets.into_iter()
                    .map(|d| CvDetection {
                        klasa: d.klasa,
                        bbox: d.bbox,
                        score: d.score,
                    })
                    .collect(),
            );
        }
        Ok::<_, String>(CameraCvResult::Detections { per_frame })
    })
    .await
    .map_err(|e| format!("camera-cv device detect executor: {e}"))??;
    Ok(batch)
}

/// Runs the YOLOv8 vehicle detector on a batch of NV12 detect frames — the
/// PARALLEL half of the NV12 detect closure's `tokio::join!`. Its own ort pool
/// gives independent CUDA streams, so this overlaps the RF-DETR forward. Any
/// failure (model absent, forward error) degrades to an all-empty result, so
/// vehicle detection can NEVER block or fail the primary detection.
#[cfg(all(
    any(target_os = "linux", target_os = "windows"),
    feature = "vision-ort",
    feature = "vision-cuda-preprocess"
))]
async fn run_nv12_vehicle_forward(
    frames: Vec<Nv12DetectInput>,
    color: crate::vision::gpu_preprocess::ColorCoeffs,
) -> Vec<Vec<Detection>> {
    let n = frames.len();
    let Some(detector) = get_vehicle_detector().await else {
        return vec![Vec::new(); n];
    };
    let out = tokio::task::spawn_blocking(move || {
        use crate::vision::gpu_preprocess::Nv12Frame;
        let nv12: Vec<Nv12Frame> = frames
            .iter()
            .map(|f| Nv12Frame {
                y: &f.data[f.y_offset as usize..],
                y_stride: f.y_stride as usize,
                uv: &f.data[f.uv_offset as usize..],
                uv_stride: f.uv_stride as usize,
                w: f.width,
                h: f.height,
            })
            .collect();
        detector.detect_batch_gpu(&nv12, color)
    })
    .await;
    match out {
        Ok(Ok(v)) if v.len() == n => v,
        Ok(Ok(_)) => vec![Vec::new(); n],
        Ok(Err(e)) => {
            warn_throttled("vehicle", &format!("nv12 vehicle detect: {e:#}"));
            vec![Vec::new(); n]
        }
        Err(e) => {
            warn_throttled("vehicle", &format!("nv12 vehicle detect task: {e}"));
            vec![Vec::new(); n]
        }
    }
}

/// Runs the YOLOv8 vehicle detector on a batch of RGB detect frames (the device
/// zero-copy path has no YOLO-usable pixels — its `OwnedDeviceTensor` is already
/// RF-DETR-normalized at 560 — so the launcher passes the full-res RGB `frame`
/// here instead). Same degrade-to-empty guard as the NV12 path.
#[cfg(feature = "vision-ort")]
async fn run_rgb_vehicle_forward(frames: Vec<CvFrameLocal>) -> Vec<Vec<Detection>> {
    let n = frames.len();
    if n == 0 {
        return Vec::new();
    }
    let Some(detector) = get_vehicle_detector().await else {
        return vec![Vec::new(); n];
    };
    let out = tokio::task::spawn_blocking(move || {
        let refs: Vec<(&[u8], u32, u32)> = frames
            .iter()
            .map(|f| (f.data.as_ref(), f.width, f.height))
            .collect();
        detector.detect_batch(&refs)
    })
    .await;
    match out {
        Ok(Ok(v)) if v.len() == n => v,
        Ok(Ok(_)) => vec![Vec::new(); n],
        Ok(Err(e)) => {
            warn_throttled("vehicle", &format!("rgb vehicle detect: {e:#}"));
            vec![Vec::new(); n]
        }
        Err(e) => {
            warn_throttled("vehicle", &format!("rgb vehicle detect task: {e}"));
            vec![Vec::new(); n]
        }
    }
}

/// Stosuje wynik JEDNEGO batcha forwardu z powrotem na pętli: routuje detekcje
/// do właściwych jobów po `job_id` każdego [`PendingItem`] (batch może obejmować
/// wiele kamer) i domyka etapy przez [`stage_completed`]. Panika/abort zadania
/// forwardu NIGDY nie może zostawić otwartych etapów — inaczej ordering gate
/// (jeden job/kamera) zablokowałby te kamery na zawsze — więc błąd joina domyka
/// wszystkie pozycje batcha jako porażkę.
fn apply_forward_result(
    jobs: &mut HashMap<u64, FrameJob>,
    batch: Vec<PendingItem>,
    out: Result<ForwardOutput, tokio::task::JoinError>,
    cold: &mpsc::Sender<DetectionEvent>,
) {
    let ForwardOutput {
        alias,
        detect_ms,
        outcome,
        mut vehicles,
    } = match out {
        Ok(o) => o,
        Err(e) => {
            warn_throttled("detect", &format!("detect forward task failed: {e}"));
            for item in &batch {
                stage_completed(jobs, item, None, None, cold);
            }
            return;
        }
    };
    // Pad/truncate the per-frame vehicle results to the batch length so the zip
    // below always aligns (a degraded vehicle half returns `[]`, not per-frame).
    if vehicles.len() != batch.len() {
        vehicles = vec![Vec::new(); batch.len()];
    }
    match outcome {
        Ok(CameraCvResult::Detections { per_frame }) if per_frame.len() == batch.len() => {
            for ((item, dets_cv), veh) in batch.iter().zip(per_frame).zip(vehicles) {
                let dets: Vec<Detection> = dets_cv.into_iter().map(detection_from_cv).collect();
                stage_completed(jobs, item, Some((dets, detect_ms)), Some(veh), cold);
            }
        }
        Ok(CameraCvResult::Detections { per_frame }) => {
            warn_throttled(
                "detect",
                &format!(
                    "detect '{alias}': {} per_frame results for {} batch frames (contract broken)",
                    per_frame.len(),
                    batch.len()
                ),
            );
            for item in &batch {
                stage_completed(jobs, item, None, None, cold);
            }
        }
        Ok(_) => {
            warn_throttled(
                "detect",
                &format!("detect '{alias}': unexpected camera-cv result variant"),
            );
            for item in &batch {
                stage_completed(jobs, item, None, None, cold);
            }
        }
        Err(e) => {
            warn_throttled("detect", &format!("detect '{alias}': {e}"));
            for item in &batch {
                stage_completed(jobs, item, None, None, cold);
            }
        }
    }
}

/// Odbiera jeden ukończony forward z [`tokio::task::JoinSet`] i stosuje go na
/// pętli. Batch trzymany jest po stronie pętli pod `task Id` (mapa `inflight`) —
/// więc nawet gdy zadanie spanikuje, znamy jego pozycje i domykamy je jako
/// porażkę zamiast wyciekać otwarte etapy.
fn apply_joined(
    jobs: &mut HashMap<u64, FrameJob>,
    inflight: &mut HashMap<tokio::task::Id, Vec<PendingItem>>,
    joined: Option<Result<(tokio::task::Id, ForwardOutput), tokio::task::JoinError>>,
    cold: &mpsc::Sender<DetectionEvent>,
) {
    let Some(joined) = joined else {
        return;
    };
    let (id, out) = match joined {
        Ok((id, output)) => (id, Ok(output)),
        Err(e) => (e.id(), Err(e)),
    };
    let Some(batch) = inflight.remove(&id) else {
        return;
    };
    apply_forward_result(jobs, batch, out, cold);
}

/// Non-blocking: stosuje WSZYSTKIE już-ukończone forwardy z JoinSetu bez czekania
/// na trwające. Wołane na początku sekcji flush KAŻDEJ iteracji, więc ukończony
/// forward jest zaaplikowany (`stage_completed`/`finalize_job`/publish) ZANIM
/// pętla zarezerwuje permit i spawnie kolejny — między tym drenażem a spawnem nie
/// ma awaitu, więc żaden forward nie ukończy się „pomiędzy". Przy K=1 przywraca
/// dokładną semantykę inline-await: spawn → czekaj → zastosuj → spawn następny.
fn drain_ready_forwards(
    forwards: &mut tokio::task::JoinSet<ForwardOutput>,
    jobs: &mut HashMap<u64, FrameJob>,
    inflight: &mut HashMap<tokio::task::Id, Vec<PendingItem>>,
    cold: &mpsc::Sender<DetectionEvent>,
) {
    while let Some(joined) = forwards.try_join_next_with_id() {
        apply_joined(jobs, inflight, Some(joined), cold);
    }
}

/// Górny limit czekania `drain` na dokończenie trwających forwardów. Forwardy
/// kończą się zwykle w dziesiątkach ms; limit chroni shutdown przed zawieszonym
/// forwardem (po nim abort jako fallback).
const FORWARD_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Flaga graceful-shutdown pętli silnika. `drain` ją ustawia, a pętla po jej
/// zauważeniu dokańcza WSZYSTKIE trwające forwardy (await, nie abort — abort nie
/// anuluje biegnącego `spawn_blocking`, więc GPU-forward mógłby nałożyć się na
/// restart) i wychodzi. Reset przy starcie pętli (`start_engine_once`) i na końcu
/// `drain`, żeby kolejny start nie wystartował od razu w trybie shutdown.
fn shutdown_flag() -> &'static std::sync::atomic::AtomicBool {
    static F: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    &F
}

/// The one process-wide analysis engine: collects (camera, frame-stage) pairs
/// whose per-stage deadline elapsed, akumuluje ich NOWE klatki w buforze
/// `pending` i flushuje je do executora chunkami po [`MODEL_BATCH`]
/// pogrupowanymi po (alias, threshold), gdy grupa się wypełni ALBO gdy
/// najstarsza pozycja czeka dłużej niż [`MAX_BATCH_WAIT`]. Time-batching
/// sprawia, że nawet POJEDYNCZA szybka kamera wypełnia batch własnymi
/// klatkami, a przy wielu kamerach flush jest cross-camera i natychmiastowy.
async fn engine_loop() {
    let cold = ensure_cold_started();
    let mut last_metrics = Instant::now();
    // Współbieżność forwardów (Chunk 4): do K batchowanych forwardów naraz na puli
    // sesji ort. K=1 (domyślnie) = dawna serializacja (jeden forward w locie).
    let k = inflight_limit();
    let inflight_sem = Arc::new(tokio::sync::Semaphore::new(k));
    // Trwające forwardy: JoinSet daje po zakończeniu `(Id, ForwardOutput)`, a przy
    // drainie (abort pętli → drop JoinSetu) abortuje wszystkie zadania, więc żaden
    // forward nie zostaje osierocony i jego bufor klatki jest zwalniany.
    let mut forwards: tokio::task::JoinSet<ForwardOutput> = tokio::task::JoinSet::new();
    // Batch każdego trwającego forwardu (routing wyników) trzymany po stronie pętli
    // pod `task Id` — panika zadania nie gubi pozycji do domknięcia.
    let mut inflight: HashMap<tokio::task::Id, Vec<PendingItem>> = HashMap::new();
    info!(
        "[vision_analysis] cross-camera inference engine started (model_batch={MODEL_BATCH}, max_batch_wait={}ms, inflight={k})",
        MAX_BATCH_WAIT.as_millis()
    );

    // Bufor akumulacyjny time-batchingu: NOWE klatki due-etapów zbierane między
    // flushami. FIFO — flush bierze najstarsze pozycje wybranej grupy.
    let mut pending: Vec<PendingItem> = Vec::new();
    // Klatki w trakcie analizy (job_id → FrameJob). Job żyje, dopóki którys z
    // jego etapów czeka w `pending` — otwarty job blokuje przyjęcie kolejnej
    // klatki TEJ KAMERY (ordering gate, patrz pętla zbierania klatek).
    let mut jobs: HashMap<u64, FrameJob> = HashMap::new();
    let mut next_job_id: u64 = 0;
    // Ostatnio dołożona tożsamość klatki per (kamera, etap) (`captured_ms`,
    // `pts_ns`), by NIE dublować tej samej klatki, gdy kamera nie wyprodukowała
    // nowej między tickami (latest_frame_global zwróci wtedy tę samą klatkę).
    let mut last_added: HashMap<(String, String), (u64, Option<u64>)> = HashMap::new();
    // Ciągłe oczekiwanie na executor: początek czekania + ostatnia eskalacja
    // `error!` (patrz [`EXECUTOR_STALL_ERROR_AFTER`] / [`EXECUTOR_STALL_ERROR_EVERY`]).
    let mut executor_wait_since: Option<Instant> = None;
    let mut executor_stall_logged: Option<Instant> = None;

    // Continuous (adaptive) batching: after each batch we loop IMMEDIATELY and
    // form the next one from whatever became due while the previous inference ran,
    // so the GPU stays back-to-back under load instead of idling to a fixed timer.
    loop {
        let now = std::time::Instant::now();

        // Anti-stall: force-evict any job whose forward has been in flight past
        // FORWARD_STALL_TIMEOUT. A hung forward otherwise keeps the job in `jobs`,
        // and the ordering gate below (`jobs.values().any(camera_id == id)`) then
        // blocks every new frame of that camera indefinitely — the multi-minute
        // "analysis stopped, overlay blank" outage. Evicting frees the gate; the
        // next iteration spawns a fresh forward on a free inflight permit.
        {
            let stale: Vec<(u64, String, Vec<String>, u64)> = jobs
                .iter()
                .filter(|(_, j)| now.duration_since(j.submitted_at) >= FORWARD_STALL_TIMEOUT)
                .map(|(id, j)| {
                    (
                        *id,
                        j.camera_id.clone(),
                        j.open_stages.clone(),
                        now.duration_since(j.submitted_at).as_millis() as u64,
                    )
                })
                .collect();
            for (job_id, cam, open, age_ms) in stale {
                tracing::error!(
                    camera_id = %cam,
                    age_ms,
                    open_stages = ?open,
                    "[vision_analysis] forward stalled — evicting job to unblock the camera (a detection forward hung on a lock/resource; ordering gate was blocking every new frame)"
                );
                jobs.remove(&job_id);
            }
        }

        // Graceful shutdown (drain): dokończ WSZYSTKIE trwające forwardy —
        // awaitem, nie abortem — aplikując ich wyniki (finalne publikacje), po
        // czym wyjdź. Abort anulowałby tylko async-task; biegnący `spawn_blocking`
        // GPU-forward trwałby dalej i mógłby nałożyć się na restart, więc czekamy
        // aż realnie się zakończą (K przestaje wtedy ograniczać starą pracę GPU).
        if shutdown_flag().load(AtomicOrdering::Relaxed) {
            while let Some(joined) = forwards.join_next_with_id().await {
                apply_joined(&mut jobs, &mut inflight, Some(joined), &cold);
            }
            info!("[vision_analysis] engine loop drained in-flight forwards, exiting");
            return;
        }

        if now.duration_since(last_metrics) >= Duration::from_secs(30) {
            let m = metrics();
            info!(
                "[vision_analysis] cold metrics: emitted={} coalesced={} pended={} superseded={} drop_full={} drop_budget={} bytes_inflight={}",
                m.emitted.load(AtomicOrdering::Relaxed),
                m.coalesced.load(AtomicOrdering::Relaxed),
                m.pended.load(AtomicOrdering::Relaxed),
                m.dropped_inflight.load(AtomicOrdering::Relaxed),
                m.dropped_full.load(AtomicOrdering::Relaxed),
                m.dropped_budget.load(AtomicOrdering::Relaxed),
                cold_bytes().load(AtomicOrdering::Relaxed),
            );
            last_metrics = now;
        }

        // Collect due (camera, frame-stage) pairs + which cameras need a config
        // re-read (no DB under the lock). Also track the soonest upcoming
        // deadline so an idle wait sleeps exactly until the next stage is due.
        let mut due: Vec<(
            String,
            Arc<CvPipeline>,
            Vec<String>,
            Arc<Vec<Vec<(f32, f32)>>>,
        )> = Vec::new();
        let mut recheck: Vec<String> = Vec::new();
        let mut earliest_next: Option<std::time::Instant> = None;
        {
            let fold = |t: std::time::Instant, earliest: &mut Option<std::time::Instant>| {
                *earliest = Some(match *earliest {
                    Some(e) => e.min(t),
                    None => t,
                });
            };
            let reg = cameras().lock().unwrap();
            for (id, slot) in reg.iter() {
                if slot.next_cfg_check <= now {
                    recheck.push(id.clone());
                } else {
                    fold(slot.next_cfg_check, &mut earliest_next);
                }
                let Some(pipeline) = slot.pipeline.as_ref() else {
                    continue;
                };
                let mut due_stages: Vec<String> = Vec::new();
                for fs in cv_pipeline::frame_stages(pipeline) {
                    match slot.stage_due.get(&fs.stage_id) {
                        Some(&t) if t <= now => due_stages.push(fs.stage_id.clone()),
                        Some(&t) => fold(t, &mut earliest_next),
                        // Freshly installed stage without a deadline yet.
                        None => due_stages.push(fs.stage_id.clone()),
                    }
                }
                if !due_stages.is_empty() {
                    due.push((id.clone(), pipeline.clone(), due_stages, slot.zones.clone()));
                }
            }
        }

        // Re-read changed configs (fps + pipeline) outside the lock. This runs
        // BEFORE the executor gate, so pipelines keep refreshing while the
        // engine waits — the moment the slot arrives, analysis starts instantly.
        for id in &recheck {
            let (fps, resolved, zones_raw) = resolve_camera_config(id).await;
            apply_camera_config(id, fps, resolved, zones_raw, now);
        }

        // Etapy rozwiązują modele TYLKO przez executor — pusty slot (bootstrap,
        // router jeszcze się inicjalizuje) = czekaj, nie analizuj i nie crashuj.
        // Po [`EXECUTOR_STALL_ERROR_AFTER`] ciągłego czekania eskalujemy do
        // `error!` (powtarzany co [`EXECUTOR_STALL_ERROR_EVERY`]) — cichy brak
        // analizy CV to awaria, nie szum.
        let Some(executor) = runtime_executor() else {
            let since = *executor_wait_since.get_or_insert(now);
            if now.duration_since(since) >= EXECUTOR_STALL_ERROR_AFTER {
                let due_log = executor_stall_logged
                    .map(|t| now.duration_since(t) >= EXECUTOR_STALL_ERROR_EVERY)
                    .unwrap_or(true);
                if due_log {
                    let cams: Vec<String> = cameras().lock().unwrap().keys().cloned().collect();
                    error!(
                        "[vision_analysis] CV analysis stalled: runtime executor not initialized after {}s; cameras waiting: [{}]",
                        now.duration_since(since).as_secs(),
                        cams.join(", ")
                    );
                    executor_stall_logged = Some(now);
                }
            } else {
                warn_throttled(
                    "executor-missing",
                    "runtime executor slot empty; analysis waiting for router init",
                );
            }
            tokio::time::sleep(EXECUTOR_WAIT).await;
            continue;
        };
        executor_wait_since = None;
        executor_stall_logged = None;

        // Dołóż do `pending` NOWE klatki wszystkich due-etapów (async snapshot).
        // Czas przechwycenia klatki (`captured_ms`, unix epoch ms) + `pts_ns`
        // niesiemy razem z ramka az do publish, zeby overlay kotwiczyl detekcje
        // na wlasciwej klatce. Jedna kamera pobiera JEDNĄ klatkę per tick —
        // wszystkie jej due-etapy analizują TĘ SAMĄ klatkę (wspólny FrameJob),
        // więc ich wyniki scala jedna publikacja.
        for (id, pipeline, due_stages, zones) in &due {
            // Ordering gate: jedna klatka w locie per KAMERA (przez wszystkie
            // etapy). Szybszy etap nie może otworzyć joba dla captured_ms T+1,
            // dopóki wieloetapowy job T jest otwarty — inaczej starszy merge
            // (T) opublikowałby się PO nowszym (T+1) i cofnął overlay w czasie.
            // Przy jednym etapie detect to dokładnie dawne "max 1 klatka w
            // locie per kamera" (odrzucamy, nie kolejkujemy).
            if jobs.values().any(|j| j.camera_id == *id) {
                continue;
            }
            let Some((crops, w, h, captured_ms, pts_ns, crops_format, detect, crops_device)) =
                crate::addon::host_functions::camera::latest_frame_global(id).await
            else {
                continue;
            };
            // Detector input: the detect-branch frame when present (GPU-scaled
            // RGB 560, or raw NV12 on the GPU-resident path), else the full-res
            // crops frame (detector CPU-resizes it). Enrichment crops keep using
            // `rgb`/`w`/`h` below. `detect_format` routes RGB→executor,
            // NV12→`detect_batch_gpu`.
            let (detect_frame, detect_w, detect_h, detect_format, detect_device) = match detect {
                Some((d, dw, dh, fmt, dev)) => (d, dw, dh, fmt, dev),
                None => (crops.clone(), w, h, crops_format, None),
            };
            let ident = (captured_ms, pts_ns);
            let mut stages_for_job: Vec<String> = Vec::new();
            for sid in due_stages {
                let key = (id.clone(), sid.clone());
                // Pomiń, gdy to ta sama klatka co ostatnio dołożona dla tego
                // etapu (zero duplikatów).
                let is_new = last_added
                    .get(&key)
                    .map(|prev| *prev != ident)
                    .unwrap_or(true);
                if is_new {
                    stages_for_job.push(sid.clone());
                }
            }
            if stages_for_job.is_empty() {
                continue;
            }
            let job_id = next_job_id;
            next_job_id += 1;
            let added = Instant::now();
            for sid in &stages_for_job {
                last_added.insert((id.clone(), sid.clone()), ident);
                let Some(stage) = pipeline.stages.iter().find(|s| &s.stage_id == sid) else {
                    continue;
                };
                pending.push(PendingItem {
                    job_id,
                    stage_id: sid.clone(),
                    alias: stage.model.clone(),
                    threshold: stage.threshold,
                    added,
                });
            }
            jobs.insert(
                job_id,
                FrameJob {
                    camera_id: id.clone(),
                    zones: zones.clone(),
                    frame: crops,
                    frame_device: crops_device,
                    w,
                    h,
                    frame_format: crops_format,
                    detect_frame,
                    detect_w,
                    detect_h,
                    detect_format,
                    detect_device,
                    captured_ms,
                    pts_ns,
                    pipeline: pipeline.clone(),
                    open_stages: stages_for_job,
                    results: Vec::new(),
                    detect_ms_total: 0,
                    failed_stages: 0,
                    vehicles: Vec::new(),
                    submitted_at: now,
                },
            );
        }

        // Reschedule every due stage by its own FPS interval (stage fps, or the
        // camera-level analysis_fps when the stage does not set one).
        {
            let mut reg = cameras().lock().unwrap();
            for (id, pipeline, due_stages, _zones) in &due {
                if let Some(slot) = reg.get_mut(id) {
                    for sid in due_stages {
                        let Some(stage) = pipeline.stages.iter().find(|s| &s.stage_id == sid)
                        else {
                            continue;
                        };
                        let fps = cv_pipeline::stage_fps(stage, slot.fps);
                        slot.stage_due
                            .insert(sid.clone(), now + interval_for_fps(fps));
                    }
                }
            }
        }

        // Warunek flush: pełny chunk MODEL_BATCH JEDNEJ grupy (alias, threshold)
        // — cross-camera przy wielu kamerach albo nazbierane klatki jednej
        // kamery — ALBO upłynęło okno MAX_BATCH_WAIT od najstarszej pozycji
        // (bound latencji przy małej liczbie kamer; flushuje grupę najstarszej).
        // P1: zastosuj wszystkie GOTOWE forwardy zanim uformujemy/wypuścimy nowy
        // batch. Gwarantuje, że wynik ukończonego forwardu jest zaaplikowany
        // (publikacja + kolejność etapów) PRZED spawnem kolejnego. Sekcja
        // flush/acquire/spawn poniżej jest w całości synchroniczna (brak awaitu),
        // więc żaden forward nie ukończy się między tym drenażem a spawnem.
        drain_ready_forwards(&mut forwards, &mut jobs, &mut inflight, &cold);

        let now_flush = Instant::now();
        let window_elapsed = pending
            .first()
            .map(|it| now_flush.duration_since(it.added) >= MAX_BATCH_WAIT)
            .unwrap_or(false);
        let keys: Vec<(&str, Option<u32>)> = pending
            .iter()
            .map(|it| (it.alias.as_str(), it.threshold.map(f32::to_bits)))
            .collect();
        let batch_indices = cv_pipeline::select_flush_batch(&keys, MODEL_BATCH, window_elapsed);

        // Rezerwuj slot forwardu ZANIM wyjmiemy batch. Semaphore(K) to
        // backpressure, które dawał inline `.await`: gdy K forwardów już biegnie,
        // `try_acquire_owned` zawodzi i pętla przestaje wypuszczać nowe batche
        // (pending pozostaje ograniczony ordering gate'm = ≤1 klatka/kamera).
        // Permit wędruje do zadania i zwalnia się, gdy forward się kończy.
        let permit = batch_indices
            .as_ref()
            .and_then(|_| inflight_sem.clone().try_acquire_owned().ok());

        let Some(indices) = batch_indices.filter(|_| permit.is_some()) else {
            // Nic do policzenia (brak due grupy) ALBO wszystkie K slotów zajęte:
            // czekaj do najbliższego z (a) deadline najwcześniejszego jeszcze-niedue
            // etapu / cfg-checku, (b) deadline flushu pending (`added +
            // MAX_BATCH_WAIT`), LUB do zakończenia któregoś trwającego forwardu
            // (zwolni slot; wynik stosujemy na pętli — jedyny właściciel `jobs`).
            // Clamp [IDLE_POLL_MIN, IDLE_POLL_MAX] trzyma pętlę responsywną.
            let mut wait = earliest_next
                .map(|t| t.saturating_duration_since(now_flush))
                .unwrap_or(IDLE_POLL_MAX);
            if let Some(it) = pending.first() {
                wait = wait.min((it.added + MAX_BATCH_WAIT).saturating_duration_since(now_flush));
            }
            let wait = wait.clamp(IDLE_POLL_MIN, IDLE_POLL_MAX);
            // Precondycja `!forwards.is_empty()`: pusty JoinSet rozwiązuje
            // `join_next_with_id` natychmiast na `None` — bez guarda select
            // busy-spinowałby. Gdy pusty, tylko sleep prowadzi oczekiwanie.
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                joined = forwards.join_next_with_id(), if !forwards.is_empty() => {
                    apply_joined(&mut jobs, &mut inflight, joined, &cold);
                }
            }
            continue;
        };
        let permit = permit.expect("permit present when a batch is selected");

        // Keep the flush batch homogeneous in DETECT FRAME FORMAT (and, for NV12,
        // colorimetry): an RGB batch goes through the executor (cross-node
        // failover), while an NV12 batch is device-preprocessed locally by ONE
        // `detect_batch_gpu` call that applies a SINGLE YUV→RGB matrix. Items
        // whose key differs from the first selected item stay in `pending` for
        // the next flush (same as group-overflow — never dropped). In a uniform
        // deployment every camera shares the ingest path, so this filters
        // nothing; it only guards a mixed transition (a camera falling back to
        // CPU while another is NV12).
        let mut indices = indices;
        let target_key = indices
            .first()
            .and_then(|&i| pending.get(i))
            .and_then(|it| jobs.get(&it.job_id))
            .map(job_batch_key);
        indices.retain(|&i| {
            pending
                .get(i)
                .and_then(|it| jobs.get(&it.job_id))
                .map(job_batch_key)
                == target_key
        });
        // A homogeneous batch is device (zero-copy), NV12 (download preprocess) or
        // RGB (executor). Device jobs carry the sentinel key so they never mix.
        #[cfg(all(feature = "vision-ort", feature = "vision-cuda-preprocess"))]
        let batch_is_device = matches!(target_key, Some(Some(k)) if k == DEVICE_BATCH_KEY);
        #[allow(unused_variables)]
        let batch_is_nv12 = matches!(target_key, Some(Some(_))) && {
            #[cfg(all(feature = "vision-ort", feature = "vision-cuda-preprocess"))]
            {
                !batch_is_device
            }
            #[cfg(not(all(feature = "vision-ort", feature = "vision-cuda-preprocess")))]
            {
                true
            }
        };

        // Wyjmij wybrane pozycje (indeksy rosnące — usuwamy od końca, kolejność
        // FIFO zachowana). Nadmiar grupy zostaje w pending na kolejny flush.
        let mut batch: Vec<PendingItem> = Vec::with_capacity(indices.len());
        for &i in indices.iter().rev() {
            batch.push(pending.remove(i));
        }
        batch.reverse();

        // Zero-copy (Stage 4) device detect: the batch's frames each carry an
        // owned, already-preprocessed device tensor (no NV12 pixels). Skip
        // preprocess entirely and run only the ORT forward + decode. Bypasses the
        // executor (a device tensor has no RGB wire form for mesh failover); the
        // result shape mirrors the executor's Detect op so `apply_forward_result`
        // handles it identically to every other detect path.
        #[cfg(all(
            any(target_os = "linux", target_os = "windows"),
            feature = "vision-ort",
            feature = "vision-cuda-preprocess"
        ))]
        if batch_is_device {
            let mut handles: Vec<super::fakefile::DeviceDetectTensor> =
                Vec::with_capacity(batch.len());
            for it in &batch {
                if let Some(job) = jobs.get(&it.job_id) {
                    if let Some(dev) = &job.detect_device {
                        handles.push(dev.clone());
                    }
                }
            }
            if handles.len() != batch.len() {
                warn_throttled(
                    "detect",
                    "pending item without a live device frame job; batch dropped",
                );
                for item in &batch {
                    stage_completed(&mut jobs, item, None, None, &cold);
                }
                drop(permit);
                continue;
            }
            let alias = batch[0].alias.clone();
            let threshold = batch[0].threshold;
            let n = batch.len().max(1) as u32;
            // Zero-copy DEVICE detect: the detect input is an already-RF-DETR-
            // preprocessed device tensor (560, ImageNet-normalized) — there are NO
            // YOLO-usable raw pixels here without downloading the surface (the very
            // cost this path avoids). Vehicle detection therefore degrades to empty
            // on the zero-copy path; use NV12/RGB ingest for per-truck association.
            let vehicle_frames = batch.len();
            let handle = forwards.spawn(async move {
                let _permit = permit;
                let detect_start = Instant::now();
                let outcome = run_device_detect_forward(handles, threshold).await;
                let detect_ms = (detect_start.elapsed().as_millis() as u32) / n;
                ForwardOutput {
                    alias,
                    detect_ms,
                    outcome,
                    vehicles: vec![Vec::new(); vehicle_frames],
                }
            });
            inflight.insert(handle.id(), batch);
            continue;
        }

        // GPU-resident NV12 detect: a homogeneous NV12 batch bypasses the
        // executor and runs `detect_batch_gpu` directly (device preprocess; mesh
        // failover / RGB wire serialization do not apply to a device frame). The
        // result shape mirrors the executor's Detect op so `apply_forward_result`
        // handles both paths identically. Only compiled with the ort GPU
        // features; without them NV12 frames are never produced (see
        // `resolve_ingest_path` / `nv12_detect_bench_enabled`).
        #[cfg(all(
            any(target_os = "linux", target_os = "windows"),
            feature = "vision-ort",
            feature = "vision-cuda-preprocess"
        ))]
        if batch_is_nv12 {
            let color = batch
                .first()
                .and_then(|it| jobs.get(&it.job_id))
                .and_then(|j| nv12_color(&j.detect_format))
                .unwrap_or_else(crate::vision::gpu_preprocess::ColorCoeffs::bt709_limited);
            let mut nv12: Vec<Nv12DetectInput> = Vec::with_capacity(batch.len());
            for it in &batch {
                if let Some(job) = jobs.get(&it.job_id) {
                    if let super::fakefile::DetectFrameFormat::Nv12 {
                        y_stride,
                        uv_stride,
                        y_offset,
                        uv_offset,
                        ..
                    } = job.detect_format
                    {
                        nv12.push(Nv12DetectInput {
                            data: job.detect_frame.clone(),
                            width: job.detect_w,
                            height: job.detect_h,
                            y_stride,
                            uv_stride,
                            y_offset,
                            uv_offset,
                        });
                    }
                }
            }
            if nv12.len() != batch.len() {
                warn_throttled(
                    "detect",
                    "pending item without a live NV12 frame job; batch dropped",
                );
                for item in &batch {
                    stage_completed(&mut jobs, item, None, None, &cold);
                }
                drop(permit);
                continue;
            }
            let alias = batch[0].alias.clone();
            let threshold = batch[0].threshold;
            let n = batch.len().max(1) as u32;
            let vehicle_nv12 = nv12.clone();
            let handle = forwards.spawn(async move {
                let _permit = permit;
                let detect_start = Instant::now();
                // RF-DETR and YOLOv8-vehicle run CONCURRENTLY on the SAME frame:
                // separate ort pools → independent CUDA streams → wall time ≈
                // max(DETR, YOLO), NOT the sum. The vehicle half degrades to empty
                // internally, so it never blocks or fails detection.
                let (outcome, vehicles) = tokio::join!(
                    run_nv12_detect_forward(nv12, color, threshold),
                    run_nv12_vehicle_forward(vehicle_nv12, color),
                );
                let detect_ms = (detect_start.elapsed().as_millis() as u32) / n;
                ForwardOutput {
                    alias,
                    detect_ms,
                    outcome,
                    vehicles,
                }
            });
            inflight.insert(handle.id(), batch);
            continue;
        }

        // Klatki batcha zero-copy (klon `Arc`) z jobów. Job pozycji w pending
        // zawsze istnieje (usuwany dopiero po domknięciu wszystkich etapów).
        let mut frames: Vec<CvFrameLocal> = Vec::with_capacity(batch.len());
        for it in &batch {
            if let Some(job) = jobs.get(&it.job_id) {
                // Detect forward consumes the (GPU-scaled) detector frame — 560
                // hits the detector's copy fast-path and skips the CPU resize.
                frames.push(CvFrameLocal {
                    data: job.detect_frame.clone(),
                    width: job.detect_w,
                    height: job.detect_h,
                });
            }
        }
        if frames.len() != batch.len() {
            warn_throttled(
                "detect",
                "pending item without a live frame job; batch dropped",
            );
            for item in &batch {
                stage_completed(&mut jobs, item, None, None, &cold);
            }
            // Slot był tylko zarezerwowany — brak forwardu, drop zwalnia go od razu.
            drop(permit);
            continue;
        }

        // HOT PATH: one batched detect per (alias, threshold) group through the
        // executor (resolve aliasu + failover/mesh). Sam FORWARD biegnie
        // współbieżnie (do K naraz) w spawnowanym zadaniu — pula sesji ort jest
        // `&self` + Send+Sync, więc równoległe detekty liczą się na osobnych
        // sesjach GPU bez korupcji (Chunki 1-3). APLIKACJA wyników wraca na
        // pętlę przez `join_next`, więc `jobs`/tracker/enrich-cache ma nadal
        // jednego właściciela — współbieżny jest WYŁĄCZNIE forward. `detect_ms`
        // liczony jak dotąd: łączny czas wywołania / liczba klatek batcha.
        let alias = batch[0].alias.clone();
        let threshold = batch[0].threshold;
        let executor_task = executor.clone();
        let n = batch.len().max(1) as u32;
        // Vehicle detector runs on a CLONE of the SAME detect frames (Arc-cheap),
        // concurrently with the executor detect (own ort pool). Only on the ort
        // path — the Burn path has no YOLOv8 vehicle graph.
        #[cfg(feature = "vision-ort")]
        let vehicle_frames = frames.clone();
        let handle = forwards.spawn(async move {
            // Permit trzymany przez CAŁY forward — dropuje się z zadaniem (koniec
            // lub abort przy drainie), zwalniając slot Semaphore.
            let _permit = permit;
            let request = CameraCvRequest {
                model: alias.clone(),
                op: CameraCvOpLocal::Detect { frames, threshold },
            };
            // Wywolanie systemowe (silnik kamer) — brak tozsamosci uzytkownika,
            // swiezy kontekst per wywolanie (jak w vision_impl).
            // §2.5 — the camera pipeline, with no human in the loop. One batch
            // carries frames from SEVERAL cameras, so there is no single camera
            // id to name: the actor is the detect stage itself.
            let mut ctx = RuntimeContext::new(
                None,
                FlowOrigin::Camera,
                FlowActor::system_component("vision_detect"),
            );
            let detect_start = Instant::now();
            #[cfg(feature = "vision-ort")]
            let (outcome, vehicles) = tokio::join!(
                async {
                    executor_task
                        .execute_camera_cv(request, &mut ctx)
                        .await
                        .map_err(|e| e.to_string())
                },
                run_rgb_vehicle_forward(vehicle_frames),
            );
            #[cfg(not(feature = "vision-ort"))]
            let (outcome, vehicles) = (
                executor_task
                    .execute_camera_cv(request, &mut ctx)
                    .await
                    .map_err(|e| e.to_string()),
                Vec::new(),
            );
            let detect_ms = (detect_start.elapsed().as_millis() as u32) / n;
            ForwardOutput {
                alias,
                detect_ms,
                outcome,
                vehicles,
            }
        });
        inflight.insert(handle.id(), batch);
        // Continuous batching: wracamy natychmiast, by uformować i wypuścić
        // kolejny batch, dopóki są wolne sloty K (greedy). Gdy sloty się wyczerpią,
        // `try_acquire_owned` zawiedzie i pętla przejdzie w idle-wait powyżej.
    }
}

/// Domyka jeden etap `frame` klatki: przy sukcesie nadaje track_id (tracker per
/// (kamera, etap)), dokłada świeże wpisy cache wzbogacania i zapisuje wynik do
/// joba; przy błędzie tylko odnotowuje porażkę. Gdy to był ostatni otwarty etap
/// klatki — finalizuje job (publikacja FAZY 1 + ewentualne zdarzenie cold path).
fn stage_completed(
    jobs: &mut HashMap<u64, FrameJob>,
    item: &PendingItem,
    outcome: Option<(Vec<Detection>, u32)>,
    vehicles: Option<Vec<Detection>>,
    cold: &mpsc::Sender<DetectionEvent>,
) {
    let Some(job) = jobs.get_mut(&item.job_id) else {
        return;
    };
    job.open_stages.retain(|s| s != &item.stage_id);
    // Vehicle boxes ride the SAME frame as this detect forward. Run a dedicated
    // IOU tracker keyed `(camera, "vehicles")` so each vehicle box gets a stable
    // track_id (== its vehicle_id for association). Attached to the job; the last
    // detect stage of a multi-stage frame wins (all share the frame).
    if let Some(mut veh) = vehicles {
        // Zones gate the WHOLE frame: a vehicle outside every drawn zone is not
        // detected at all — no track, no overlay box, no event, no reads.
        retain_in_zones(&mut veh, &job.zones);
        if !veh.is_empty() {
            tracker::update(
                &tracker::key(&job.camera_id, "vehicles"),
                &mut veh,
                job.pts_ns,
            );
        }
        job.vehicles = veh;
    }
    match outcome {
        Some((mut dets, detect_ms)) => {
            // Same gate for signs/plates: out-of-zone detections never reach the
            // tracker, enrichment (OCR/classify), the overlay or the recorder.
            retain_in_zones(&mut dets, &job.zones);
            // Tracker IOU per (kamera, etap detekcji): nadaje stabilne `track_id`
            // + prędkość (vx,vy) KAŻDEJ detekcji przed publikacją, tak by FAZA 1
            // i FAZA 2 niosly juz spojne identyfikatory sledzenia.
            tracker::update(
                &tracker::key(&job.camera_id, &item.stage_id),
                &mut dets,
                job.pts_ns,
            );
            // Cache wzbogacania (per (kamera, cold-stage, track)): stan/OCR NIE
            // zmieniaja sie klatka-po-klatce, a tracki maja stabilne id. Swieze
            // wpisy przypisujemy OD RAZU w hot path — boxy FAZY 1 niosa stan
            // natychmiast, bez czekania na cold path.
            apply_cached_enrichment(&job.camera_id, &job.pipeline, &item.stage_id, &mut dets);
            job.detect_ms_total += detect_ms;
            job.results.push((item.stage_id.clone(), dets));
        }
        None => job.failed_stages += 1,
    }
    if job.open_stages.is_empty() {
        if let Some(job) = jobs.remove(&item.job_id) {
            finalize_job(job, cold);
        }
    }
}

/// Bench-only load-generator toggle, flipped programmatically by the
/// `pipeline_bench` example via [`set_force_detect`]. When on, the cold
/// enrichment path runs on frames the detector left empty (see
/// [`synthesize_forced_detections`]). OFF by default, so production behaviour
/// is unchanged.
static FORCE_DETECT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Bench-only programmatic switch for the forced-enrichment load generator.
pub fn set_force_detect(enabled: bool) {
    FORCE_DETECT.store(enabled, AtomicOrdering::Relaxed);
}

fn force_detect_enabled() -> bool {
    FORCE_DETECT.load(AtomicOrdering::Relaxed)
}

/// Bench-only load generator (behind [`force_detect_enabled`]): synthesizes one
/// detection per DISTINCT enrichment-stage class so every cold stage
/// (classify → stan, ocr → tekst incl. ADR) gets a matching crop and its REAL
/// forward runs through the inference batcher. The class is derived from each
/// cold stage's own `classes` filter (a trailing-`*` pattern becomes its prefix,
/// an exact pattern is used as-is) so [`cv_pipeline::class_matches`] accepts it
/// for ANY pipeline. The bbox sweeps horizontally with `captured_ms` so
/// consecutive frames carry distinct scene signatures and the cold-path
/// coalescer does not drop them — worst-case per-frame enrichment load.
fn synthesize_forced_detections(pipeline: &CvPipeline, captured_ms: u64) -> Vec<Detection> {
    let mut classes: Vec<String> = Vec::new();
    for stage in cv_pipeline::cold_stages(pipeline) {
        let CvStageInput::Stage {
            classes: patterns, ..
        } = &stage.input
        else {
            continue;
        };
        let Some(first) = patterns.first() else {
            continue;
        };
        let klasa = first.strip_suffix('*').unwrap_or(first).to_string();
        if !klasa.is_empty() && !classes.contains(&klasa) {
            classes.push(klasa);
        }
    }
    // Horizontal sweep (> BBOX_BUCKET per frame at ≥5 fps) makes each frame a new
    // scene for the coalescer; classes are stacked vertically so crops differ.
    let phase = (captured_ms % 2000) as f32 / 2000.0;
    let x = 0.05 + 0.6 * phase;
    classes
        .into_iter()
        .enumerate()
        .map(|(i, klasa)| Detection {
            klasa,
            bbox: [x, (0.1 + 0.18 * i as f32).min(0.8), 0.15, 0.15],
            score: 0.99,
            stan: Vec::new(),
            tekst: None,
            tekst_conf: None,
            tekst_thumb_ref: None,
            track_id: 0,
            vehicle_id: 0,
            vx: 0.0,
            vy: 0.0,
        })
        .collect()
}

/// Per-camera motion estimator state (previous luma for directional flow). Held
/// process-wide because `finalize_job` is stateless per call. The map lock is held
/// only to fetch/insert the `Arc`; the estimate itself runs on the per-camera mutex.
fn motion_estimators() -> &'static Mutex<HashMap<String, Arc<Mutex<super::motion::MotionEstimator>>>>
{
    static M: OnceLock<Mutex<HashMap<String, Arc<Mutex<super::motion::MotionEstimator>>>>> =
        OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Directional zone motion for this frame, converted to the always-compiled wire
/// type. Non-moving default when no host luma is available (device zero-copy path)
/// or the frame is malformed. NV12 uses the Y plane directly; RGB24 is converted to
/// luma once (BT.601 integer approximation).
fn compute_zone_motion(
    camera_id: &str,
    frame: &[u8],
    format: &super::fakefile::DetectFrameFormat,
    w: u32,
    h: u32,
    zones: &[Vec<(f32, f32)>],
) -> detection_bus::MotionSignal {
    use super::fakefile::DetectFrameFormat;
    if frame.is_empty() || w == 0 || h == 0 {
        return detection_bus::MotionSignal::default();
    }
    let (luma, y_stride, y_offset): (std::borrow::Cow<[u8]>, u32, u32) = match *format {
        DetectFrameFormat::Nv12 {
            y_stride, y_offset, ..
        } => (std::borrow::Cow::Borrowed(frame), y_stride, y_offset),
        DetectFrameFormat::Rgb24 => {
            let n = w as usize * h as usize;
            if frame.len() < n * 3 {
                return detection_bus::MotionSignal::default();
            }
            let mut y = vec![0u8; n];
            for (i, px) in y.iter_mut().enumerate() {
                let p = i * 3;
                *px =
                    ((77 * frame[p] as u32 + 150 * frame[p + 1] as u32 + 29 * frame[p + 2] as u32)
                        >> 8) as u8;
            }
            (std::borrow::Cow::Owned(y), w, 0)
        }
    };
    let est = {
        let mut map = motion_estimators().lock().unwrap();
        map.entry(camera_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(super::motion::MotionEstimator::new())))
            .clone()
    };
    let sig = est
        .lock()
        .unwrap()
        .estimate(&luma, w, h, y_stride, y_offset, zones);
    detection_bus::MotionSignal {
        moving: sig.moving,
        dir_x: sig.dir_x,
        magnitude: sig.magnitude,
        centroid_x: sig.centroid_x,
        coherence: sig.coherence,
    }
}

/// Finalizacja klatki po domknięciu wszystkich jej etapów `frame`: scala wyniki
/// etapów w JEDNĄ publikację overlay (FAZA 1, kolejność etapów pipeline'u) i —
/// dla niepustych zestawów — oddaje ramkę do cold path (FAZA 2, wzbogacenie).
fn finalize_job(mut job: FrameJob, cold: &mpsc::Sender<DetectionEvent>) {
    // Kazdy etap klatki zawiódł — jak dawna porażka detektora: nic nie
    // publikujemy (overlay zostaje przy poprzedniej ramce, zero czyszczenia).
    if job.results.is_empty() {
        return;
    }
    job.results
        .sort_by_key(|(sid, _)| cv_pipeline::stage_index(&job.pipeline, sid));
    // Bench-only forced enrichment load (`set_force_detect`, off by
    // default): when the real detector left this frame empty, inject synthetic
    // detections into the frame-stage results so the REAL cold path runs. This
    // MUST land here — before the empty-set gate below — because the cold event
    // is only created for a non-empty publication, so an injection inside
    // `run_cold_stages` would never be reached for an empty frame.
    if force_detect_enabled() && job.results.iter().all(|(_, d)| d.is_empty()) {
        if let Some((_, dets)) = job.results.first_mut() {
            dets.extend(synthesize_forced_detections(&job.pipeline, job.captured_ms));
        }
    }
    let merged: Vec<Detection> = job
        .results
        .iter()
        .flat_map(|(_, d)| d.iter().cloned())
        .collect();
    // FAZA 1 (hot, natychmiast): publikuj scalone detekcje wszystkich etapów
    // klatki od razu, jeszcze przed wzbogaceniem. Overlay kotwiczy je po
    // `captured_ms`, wiec boxy lądują na wlasciwej klatce z opoznieniem samego
    // dekodu + inferencji, a nie +OCR. Pusty zestaw tez publikujemy — czysci
    // overlay bez czekania na cold path. FAZA 1 zna tylko sume czasow detekcji;
    // pelny `proc_ms` publikuje FAZA 2, nadpisujac ramke dla tego samego
    // captured_ms. Boxy pojazdow doklejamy TYLKO do publikacji overlayu (front
    // rysuje ramki cieżarowek), NIE do `merged` uzytego do sygnatury/eventu.
    let mut overlay = merged.clone();
    overlay.extend(job.vehicles.iter().cloned());
    // Directional zone motion for this frame — the event recorder's trigger. Runs
    // on the host luma; the device zero-copy path yields a non-moving default.
    let motion = compute_zone_motion(
        &job.camera_id,
        &job.frame,
        &job.frame_format,
        job.w,
        job.h,
        &job.zones,
    );
    detection_bus::publish_detections(
        &job.camera_id,
        job.captured_ms,
        job.pts_ns,
        job.detect_ms_total,
        // FAZA 1: hot overlay only — signs are NOT yet stamped with `vehicle_id`
        // and enrichment is cache-only, so the event recorder must NOT bucket
        // these (they would flood the unassigned `vehicle_id = 0` bucket).
        false,
        overlay,
        motion,
    );
    // Pusty zestaw nie wymaga wzbogacenia: FAZA 1 juz wyczyscila overlay. Sam
    // pojazd bez znaku/tablicy NIE tworzy eventu (trigger to detekcje RF-DETR).
    if merged.is_empty() {
        return;
    }
    let sig = detection_sig(&merged);
    let bytes = job.frame.len();
    let camera_id = job.camera_id.clone();
    let ev = DetectionEvent {
        camera_id: job.camera_id,
        frame: job.frame,
        frame_device: job.frame_device,
        w: job.w,
        h: job.h,
        frame_format: job.frame_format,
        captured_ms: job.captured_ms,
        pts_ns: job.pts_ns,
        detect_ms: job.detect_ms_total,
        pipeline: job.pipeline,
        stage_dets: job.results,
        vehicles: job.vehicles,
    };
    // FAZA 2 (cold): coalesce / byte-budget gate with latest-pending backpressure.
    // Busy camera → the event is HELD as the freshest pending (promoted by
    // `release_cold`), not dropped, so labels converge to the latest scene.
    match admit_or_pend_cold(sig, bytes, ev) {
        ColdAdmit::Send(ev) => match cold.try_send(ev) {
            Ok(()) => {
                commit_cold(&camera_id, sig);
                metrics().emitted.fetch_add(1, AtomicOrdering::Relaxed);
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                metrics().dropped_full.fetch_add(1, AtomicOrdering::Relaxed);
                release_cold(&camera_id, bytes);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                warn!("[vision_analysis] cold path closed; detections dropped");
                release_cold(&camera_id, bytes);
            }
        },
        // Held as freshest pending, or coalesced/over-budget: nothing to send.
        ColdAdmit::Held | ColdAdmit::Dropped => {}
    }
}

/// Mapuje detekcję surface'u CameraCv na typ magistrali detekcji. Pola
/// wzbogacenia/śledzenia startują puste — nadaje je tracker (track_id, vx, vy)
/// i cold path (stan, tekst).
fn detection_from_cv(d: CvDetection) -> Detection {
    Detection {
        klasa: d.klasa,
        bbox: d.bbox,
        score: d.score,
        stan: Vec::new(),
        tekst: None,
        tekst_conf: None,
        tekst_thumb_ref: None,
        track_id: 0,
        vehicle_id: 0,
        vx: 0.0,
        vy: 0.0,
    }
}

/// Bounded cold-path queue capacity. Each NON-empty event carries a full RGB
/// frame (≈6 MB @1080p), so the cap is deliberately small to bound memory; empty
/// events never reach the cold path.
const COLD_QUEUE_CAP: usize = 32;

/// One detection frame handed from the hot detector to the cold enrichment path.
/// Cold path niesie WYLACZNIE niepuste ramki (FAZA 2 — wzbogacenie): puste
/// zestawy publikuje juz FAZA 1 w hot loopie (czyszczenie overlay), wiec nigdy
/// tu nie trafiaja i nie zajmuja pamieci ani slotu.
struct DetectionEvent {
    camera_id: String,
    frame: Arc<[u8]>,
    /// Zero-copy CROPS path ONLY: DEVICE reference to the full-res NV12 frame (the
    /// held `gst::Sample` keeps the surface valid until this event is enriched).
    /// When `Some`, `frame` is EMPTY and `run_cold_stages` cuts each crop off the
    /// device surface. `None` otherwise. Bounded surface pinning: ≤1 in-flight +
    /// ≤1 pending per camera (see the cold admission gate).
    frame_device: Option<super::fakefile::DeviceCropsFrame>,
    w: u32,
    h: u32,
    /// Pixel layout of `frame` (`Rgb24` or GPU-resident `Nv12`). Cold enrichment
    /// cuts crops with [`crop_nv12`] for NV12, keeping the RGB crop path unchanged.
    frame_format: super::fakefile::DetectFrameFormat,
    /// Czas przechwycenia klatki (unix epoch ms) — propagowany do publish jako
    /// `ts_ms`, zeby overlay kotwiczyl detekcje na wlasciwej klatce.
    captured_ms: u64,
    /// PTS klatki w osi mediów (nanosekundy) — propagowany do publish jako
    /// `pts_ns`, wspolna oś czasu z init-segmentem MSE (`mux_base_pts_ns`).
    pts_ns: Option<u64>,
    /// Suma czasów forwardów etapów `frame` (ms) zmierzona w FAZIE 1 (hot).
    /// Niesiona do FAZY 2, gdzie sumuje sie z `enrich_ms` w pelny `proc_ms`.
    detect_ms: u32,
    /// Pipeline kamery z chwili analizy — cold path interpretuje jego etapy
    /// `stage` (classify/ocr/embed) bez ponownego resolve.
    pipeline: Arc<CvPipeline>,
    /// Wyniki etapów `frame` tej klatki: (stage_id, detekcje). Etapy cold
    /// wybieraja rodzica po `stage_id`; publikacja scala grupy w kolejności
    /// etapów pipeline'u.
    stage_dets: Vec<(String, Vec<Detection>)>,
    /// Boxy pojazdow (trakowane) TEJ klatki — cold path asocjuje znaki do nich.
    vehicles: Vec<Detection>,
}

/// Live cold-path sender + consumer handle, so `drain` can tear the cold path
/// down and a later `ensure_analysis` can restart it cleanly (an `OnceLock` could
/// not be reset, leaving a dead sender after drain).
fn cold_chan() -> &'static Mutex<Option<mpsc::Sender<DetectionEvent>>> {
    static C: OnceLock<Mutex<Option<mpsc::Sender<DetectionEvent>>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}
fn cold_handle() -> &'static Mutex<Option<tokio::task::JoinHandle<()>>> {
    static H: OnceLock<Mutex<Option<tokio::task::JoinHandle<()>>>> = OnceLock::new();
    H.get_or_init(|| Mutex::new(None))
}

/// Returns a sender into the cold path, spawning the consumer if absent or dead.
fn ensure_cold_started() -> mpsc::Sender<DetectionEvent> {
    let mut chan = cold_chan().lock().unwrap();
    let dead = cold_handle()
        .lock()
        .unwrap()
        .as_ref()
        .map(|j| j.is_finished())
        .unwrap_or(true);
    if let (Some(tx), false) = (chan.as_ref(), dead) {
        return tx.clone();
    }
    let (tx, rx) = mpsc::channel::<DetectionEvent>(COLD_QUEUE_CAP);
    *cold_handle().lock().unwrap() = Some(tokio::spawn(cold_consumer(rx)));
    *chan = Some(tx.clone());
    tx
}

/// Coalescing knobs. Unchanged scenes re-emit at most every `COALESCE_REFRESH`
/// (a parked truck is not re-OCR'd every frame); changed scenes emit as fast as
/// the cold path drains (per-camera in-flight = 1). `BBOX_BUCKET` quantizes boxes
/// so sub-pixel jitter doesn't count as a change.
const COALESCE_REFRESH: Duration = Duration::from_secs(2);
const BBOX_BUCKET: f32 = 0.02;
/// Total frame bytes allowed in flight on the cold path (memory bound that makes
/// the queue cap meaningful — the count cap alone could pin GiBs of RGB frames).
const COLD_BYTE_BUDGET: usize = 256 * 1024 * 1024;

/// Per-camera cold-path scheduling state.
struct ColdCamState {
    last_sig: u64,
    last_emit: Instant,
    in_flight: bool,
    /// Freshest frame that arrived while `in_flight` — held (not dropped) and
    /// promoted by `release_cold` when the current enrichment finishes, so the
    /// overlay always converges to the LATEST scene instead of stalling on the
    /// one that happened to win the slot. `(sig, event)`; a newer arrival
    /// replaces an older pending (the superseded one counts as dropped).
    pending: Option<(u64, DetectionEvent)>,
}

fn cold_state() -> &'static Mutex<HashMap<String, ColdCamState>> {
    static S: OnceLock<Mutex<HashMap<String, ColdCamState>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Bytes of frame buffers currently queued/in-flight on the cold path.
fn cold_bytes() -> &'static AtomicUsize {
    static B: AtomicUsize = AtomicUsize::new(0);
    &B
}

/// Backpressure counters (cumulative). Logged periodically by the engine loop.
#[derive(Default)]
struct ColdMetrics {
    emitted: AtomicU64,
    coalesced: AtomicU64,
    /// Frames superseded while a newer pending replaced them (real loss). With
    /// latest-pending this is the only "inflight drop": one held frame per camera
    /// survives, older held frames are dropped in favour of the freshest.
    dropped_inflight: AtomicU64,
    /// Frames HELD as the freshest pending (not lost — enriched when the slot frees).
    pended: AtomicU64,
    dropped_full: AtomicU64,
    dropped_budget: AtomicU64,
}
fn metrics() -> &'static ColdMetrics {
    static M: OnceLock<ColdMetrics> = OnceLock::new();
    M.get_or_init(ColdMetrics::default)
}

/// Order-independent signature of a frame's worthy detections (class + bucketed
/// box). Same signature ⇒ "same scene" ⇒ coalesce.
fn detection_sig(dets: &[Detection]) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut keys: Vec<(u32, u32, u32, u32, &str)> = dets
        .iter()
        .map(|d| {
            (
                (d.bbox[0] / BBOX_BUCKET) as u32,
                (d.bbox[1] / BBOX_BUCKET) as u32,
                (d.bbox[2] / BBOX_BUCKET) as u32,
                (d.bbox[3] / BBOX_BUCKET) as u32,
                d.klasa.as_str(),
            )
        })
        .collect();
    keys.sort_unstable();
    let mut h = DefaultHasher::new();
    keys.hash(&mut h);
    h.finish()
}

/// Outcome of the cold admission gate.
enum ColdAdmit {
    /// Slot reserved — send this event to the consumer now.
    Send(DetectionEvent),
    /// Camera busy — event stored as the freshest pending; `release_cold` will
    /// promote it when the current enrichment finishes. Nothing to send now.
    Held,
    /// Coalesced (identical recent scene) or over byte budget — genuinely skipped.
    Dropped,
}

/// Coalesce + rate-limit + byte-budget gate with LATEST-PENDING backpressure.
/// When the camera is idle it reserves the slot and returns `Send(ev)`. When the
/// camera is already enriching, it does NOT drop the frame — it keeps `ev` as the
/// per-camera freshest pending (`Held`), so the overlay converges to the latest
/// scene rather than stalling on whichever frame won the slot. Only identical
/// recent scenes (coalesce) and budget overflow are truly `Dropped`. Per-camera
/// ordering is still guaranteed by `in_flight = 1` plus the single pending slot.
fn admit_or_pend_cold(sig: u64, frame_bytes: usize, ev: DetectionEvent) -> ColdAdmit {
    let now = Instant::now();
    let mut st = cold_state().lock().unwrap();
    let entry = st.entry(ev.camera_id.clone()).or_insert(ColdCamState {
        last_sig: u64::MAX,
        last_emit: now - COALESCE_REFRESH * 2,
        in_flight: false,
        pending: None,
    });
    let unchanged = sig == entry.last_sig;
    if entry.in_flight {
        // Identical recent scene: no point re-enriching, coalesce.
        if unchanged && now.duration_since(entry.last_emit) < COALESCE_REFRESH {
            metrics().coalesced.fetch_add(1, AtomicOrdering::Relaxed);
            return ColdAdmit::Dropped;
        }
        // Keep the FRESHEST frame. Replacing an older pending drops the superseded
        // one (real loss); a first pending is merely held. Pending frames are NOT
        // byte-reserved here (≤1 per camera bounds memory) — reservation happens
        // in `release_cold` at promotion, matching that run's later release.
        if entry.pending.is_some() {
            metrics()
                .dropped_inflight
                .fetch_add(1, AtomicOrdering::Relaxed);
        } else {
            metrics().pended.fetch_add(1, AtomicOrdering::Relaxed);
        }
        entry.pending = Some((sig, ev));
        return ColdAdmit::Held;
    }
    if unchanged && now.duration_since(entry.last_emit) < COALESCE_REFRESH {
        metrics().coalesced.fetch_add(1, AtomicOrdering::Relaxed);
        return ColdAdmit::Dropped;
    }
    if cold_bytes().load(AtomicOrdering::Relaxed) + frame_bytes > COLD_BYTE_BUDGET {
        metrics()
            .dropped_budget
            .fetch_add(1, AtomicOrdering::Relaxed);
        return ColdAdmit::Dropped;
    }
    // Reserve only. `last_sig`/`last_emit` are committed by `commit_cold` AFTER a
    // successful send, so a dropped (Full/Closed) event does not record its scene
    // as "recently emitted" and starve the next identical/clear frame.
    entry.in_flight = true;
    cold_bytes().fetch_add(frame_bytes, AtomicOrdering::Relaxed);
    ColdAdmit::Send(ev)
}

/// Records a successfully-sent event's scene signature + emit time (coalesce base).
fn commit_cold(camera_id: &str, sig: u64) {
    if let Some(slot) = cold_state().lock().unwrap().get_mut(camera_id) {
        slot.last_sig = sig;
        slot.last_emit = Instant::now();
    }
}

/// Releases a camera's in-flight slot + its reserved bytes, then PROMOTES a held
/// pending frame (latest-pending) if one is waiting: re-reserves its bytes, keeps
/// the slot, and re-enqueues it on the cold channel so the freshest scene runs
/// next. `saturating_sub` so a release racing a `drain()` reset can never underflow
/// the byte counter (which would wrap huge and permanently reject the budget gate).
fn release_cold(camera_id: &str, frame_bytes: usize) {
    // Free the finished event's bytes first.
    let _ = cold_bytes().fetch_update(AtomicOrdering::Relaxed, AtomicOrdering::Relaxed, |v| {
        Some(v.saturating_sub(frame_bytes))
    });
    // Clear the slot and take any freshest pending under the lock.
    let promoted = {
        let mut st = cold_state().lock().unwrap();
        match st.get_mut(camera_id) {
            Some(slot) => {
                slot.in_flight = false;
                match slot.pending.take() {
                    Some(pending) => {
                        // Hold the slot for the promoted run (no window where a new
                        // arrival could jump the queue ahead of this pending).
                        slot.in_flight = true;
                        Some(pending)
                    }
                    None => None,
                }
            }
            None => None,
        }
    };
    let Some((sig, ev)) = promoted else {
        return;
    };
    let camera = ev.camera_id.clone();
    let pending_bytes = ev.frame.len();
    // Reserve bytes for the promoted run (balances its own later release_cold).
    cold_bytes().fetch_add(pending_bytes, AtomicOrdering::Relaxed);
    let sender = cold_chan().lock().unwrap().clone();
    match sender {
        Some(tx) => match tx.try_send(ev) {
            Ok(()) => {
                commit_cold(&camera, sig);
                metrics().emitted.fetch_add(1, AtomicOrdering::Relaxed);
            }
            Err(mpsc::error::TrySendError::Full(_)) | Err(mpsc::error::TrySendError::Closed(_)) => {
                // Channel saturated/closed: drop the promotion and unwind its slot +
                // bytes (recurses once; the pending slot is already emptied above, so
                // it clears `in_flight` and returns without further promotion).
                metrics().dropped_full.fetch_add(1, AtomicOrdering::Relaxed);
                release_cold(&camera, pending_bytes);
            }
        },
        None => release_cold(&camera, pending_bytes),
    }
}

/// RAII release: guarantees a received cold event's slot + bytes are released
/// exactly once, even if enrichment or publish panics (else the camera leaks its
/// in-flight slot forever and is never analyzed again).
struct ColdSlot {
    camera_id: String,
    bytes: usize,
    released: bool,
}
impl Drop for ColdSlot {
    fn drop(&mut self) {
        if !self.released {
            release_cold(&self.camera_id, self.bytes);
            self.released = true;
        }
    }
}

/// Cold path (FAZA 2): interpretuje etapy `stage` pipeline'u (classify → stan,
/// ocr → tekst) na cropach detekcji rodzica, publikuje wzbogacony zestaw dla
/// TEGO SAMEGO `captured_ms` co FAZA 1 — overlay podmienia surowe boxy na
/// wzbogacone etykiety. Gdy kamera ma przypisany flow analizy
/// (`analysis_flow_id`), flow biegnie PO etapach cold pipeline'u i dostaje w
/// meta już wzbogacone detekcje; publikacja flow może je nadpisać
/// (`publish_flow_detections`).
/// Max cameras whose cold enrichment runs CONCURRENTLY. Serial consumption meant
/// the cross-camera inference batcher only ever saw one frame's crops per window;
/// running up to `COLD_WORKERS` events at once lets crops from many cameras coalesce
/// into one big GPU forward per model. Per-camera in-flight is still 1 (admit_cold)
/// and the byte budget still bounds memory — this only widens CROSS-camera concurrency.
fn cold_workers() -> usize {
    crate::vision::settings::get().cold_workers.clamp(1, 1024)
}

async fn cold_consumer(mut rx: mpsc::Receiver<DetectionEvent>) {
    let workers = cold_workers();
    info!("[vision_analysis] cold enrichment consumer started (≤{workers} concurrent cameras)");
    let sem = Arc::new(tokio::sync::Semaphore::new(workers));
    while let Some(ev) = rx.recv().await {
        // Bound concurrency so we never spawn an unbounded task pile, but many
        // cameras' crops still reach the batcher window together.
        let Ok(permit) = sem.clone().acquire_owned().await else {
            break;
        };
        tokio::spawn(async move {
            let _permit = permit;
            let DetectionEvent {
                camera_id,
                frame,
                frame_device,
                w,
                h,
                frame_format,
                captured_ms,
                pts_ns,
                detect_ms,
                pipeline,
                mut stage_dets,
                mut vehicles,
            } = ev;
            let bytes = frame.len();
            // RAII: releases this camera's in-flight slot + bytes on drop — including
            // an unwind if publish/enrich panics — so a camera can never be wedged.
            let slot = ColdSlot {
                camera_id: camera_id.clone(),
                bytes,
                released: false,
            };
            let enrich_start = Instant::now();
            run_cold_stages(
                &camera_id,
                &frame,
                frame_device.as_ref(),
                w,
                h,
                &frame_format,
                &pipeline,
                &mut stage_dets,
                &mut vehicles,
            )
            .await;
            let enrich_ms = enrich_start.elapsed().as_millis() as u32;
            let proc_ms = detect_ms + enrich_ms;
            // Feed the ingest session's periodic metrics line — these timings are
            // otherwise measured and thrown away.
            super::stage_metrics::record(&camera_id, detect_ms, enrich_ms);
            // Enriched signs now carry `vehicle_id`. Publish them WITH the vehicle
            // boxes (self-assigned vehicle_id) so the event recorder groups per
            // truck and the overlay keeps drawing vehicle rectangles.
            let mut merged: Vec<Detection> = stage_dets
                .iter()
                .flat_map(|(_, d)| d.iter().cloned())
                .collect();
            for v in vehicles.iter_mut() {
                v.vehicle_id = v.track_id;
            }
            merged.extend(vehicles);
            detection_bus::publish_detections(
                &camera_id,
                captured_ms,
                pts_ns,
                proc_ms,
                // FAZA 2: signs now carry final `vehicle_id` + OCR/stan enrichment —
                // this is the ONLY publish the event recorder buckets per vehicle.
                true,
                merged.clone(),
                // Motion is estimated once per frame on the hot FAZA 1 publish; the
                // cold FAZA 2 republish of the same frame carries no fresh signal.
                detection_bus::MotionSignal::default(),
            );
            if !merged.is_empty() {
                if let (Some(flow_id), Some(disp)) = (
                    camera_flow_id(&camera_id).await,
                    crate::flow_engine::dispatcher::global_flow_dispatcher(),
                ) {
                    let _slot = slot;
                    // The analysis flow reads the frame as an RGB24 image blob; on
                    // the GPU-resident path `frame` is NV12, so convert once here
                    // (only when a camera actually has an assigned flow). Zero-copy
                    // crops: `frame` is empty, so download the full NV12 on demand.
                    let host_frame = if frame.is_empty() {
                        match frame_device.as_ref().and_then(|d| d.download_full_nv12()) {
                            Some((nv12, _fmt)) => nv12,
                            None => frame.clone(),
                        }
                    } else {
                        frame.clone()
                    };
                    let rgb_frame = nv12_to_rgb_if_needed(&host_frame, w, h, &frame_format);
                    run_camera_flow(
                        disp,
                        flow_id,
                        camera_id,
                        rgb_frame,
                        w,
                        h,
                        captured_ms,
                        pts_ns,
                        proc_ms,
                        merged,
                    )
                    .await;
                    return;
                }
            }
            drop(slot);
        });
    }
}

/// Cached per-camera analysis-flow assignment. The cold path consults this for
/// every detection event; the short TTL keeps a UI re-assignment visible within
/// a few seconds without acquiring the SQLite mutex on every event (events are
/// already coalesced, but a per-event DB hit across 100 cameras is avoidable
/// load on the tokio worker). `None` is cached too, so an unassigned camera
/// does not re-query each frame.
struct FlowIdCacheEntry {
    flow_id: Option<String>,
    fetched: Instant,
}
const FLOW_ID_TTL: Duration = Duration::from_secs(5);

/// Per-frame deadline for a camera analysis flow. Well under the dispatcher's
/// 120 s default so a misbehaving flow releases the camera's in-flight slot in
/// seconds, not minutes (the slot is held for the whole detached run).
const CAMERA_FLOW_DEADLINE: Duration = Duration::from_secs(15);

fn flow_id_cache() -> &'static Mutex<HashMap<String, FlowIdCacheEntry>> {
    static C: OnceLock<Mutex<HashMap<String, FlowIdCacheEntry>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Returns the camera's assigned analysis flow id, or `None` when unassigned.
/// Cached with [`FLOW_ID_TTL`]. Odczyt DB (rusqlite, sync) przy chybieniu cache
/// biegnie na puli blocking, nie na watku tokio.
async fn camera_flow_id(camera_id: &str) -> Option<String> {
    if let Some(e) = flow_id_cache().lock().unwrap().get(camera_id) {
        if e.fetched.elapsed() < FLOW_ID_TTL {
            return e.flow_id.clone();
        }
    }
    let query_id = camera_id.to_string();
    let flow_id = tokio::task::spawn_blocking(move || {
        crate::db::global_pool()
            .and_then(|pool| crate::db::repository::camera_analysis_flow_id(&pool, &query_id).ok())
            .flatten()
    })
    .await
    .ok()
    .flatten();
    flow_id_cache().lock().unwrap().insert(
        camera_id.to_string(),
        FlowIdCacheEntry {
            flow_id: flow_id.clone(),
            fetched: Instant::now(),
        },
    );
    flow_id
}

/// §2.5 — the camera entry point's stamp, as one named function so it is
/// testable and so there is a single place that decides what a camera run
/// reports as.
///
/// System-triggered execution: there is no user in the loop, so no per-user ACL
/// applies (the dispatcher treats `user_id = None` as allow). The real
/// authorization gate is the camera→flow assignment write path, which is
/// admin-only and validates flow access before persisting `analysis_flow_id`.
/// The camera is named as a SYSTEM component: it says which camera acted
/// without claiming a person did.
fn camera_flow_request_meta(camera_id: &str) -> crate::flow_engine::dispatcher::FlowRequestMeta {
    crate::flow_engine::dispatcher::FlowRequestMeta::new(
        format!("cam-{camera_id}"),
        FlowOrigin::Camera,
        FlowActor::system_component(camera_id),
    )
}

/// Runs a camera's assigned analysis Flow on one detection frame: stores the raw
/// RGB frame as an Image blob, builds the initial envelope (payload = Image, meta
/// carries the pipeline-enriched detections + camera id), dispatches by flow id
/// and publishes the resulting detections back to the bus so the live overlay
/// reflects the flow's enrichment/verdict. Errors are logged, never fatal — the
/// cold path keeps draining and the slot is released by the caller's `ColdSlot`.
async fn run_camera_flow(
    disp: Arc<crate::flow_engine::dispatcher::FlowDispatcher>,
    flow_id: String,
    camera_id: String,
    frame: Arc<[u8]>,
    w: u32,
    h: u32,
    captured_ms: u64,
    pts_ns: Option<u64>,
    detect_ms: u32,
    detections: Vec<Detection>,
) {
    use crate::flow_engine::envelope::{FlowEnvelope, FlowValue};

    // Czas wykonania flow to koszt dodatkowej obrobki tej klatki; proc_ms =
    // detekcja + etapy cold (FAZA 2) + flow.
    let enrich_start = Instant::now();

    let dets_json = match serde_json::to_value(&detections) {
        Ok(v) => v,
        Err(e) => {
            warn!("[vision_analysis] flow {flow_id}: serialize detections failed: {e}");
            return;
        }
    };
    // Ephemeral frame store: the frame lives in memory only for the duration of
    // this flow run (read by the flow's nodes via the composite ctx.blobs) and is
    // deleted below — it never touches the durable blob store / disk.
    let frame_blobs = disp.frame_blobs();
    let blob_ref = match frame_blobs.put(frame.to_vec(), "image/x-rgb24").await {
        Ok(r) => r,
        Err(e) => {
            warn!("[vision_analysis] flow {flow_id}: blob put failed: {e:#}");
            return;
        }
    };
    let mut env = FlowEnvelope::with_payload(FlowValue::Image {
        blob_ref: blob_ref.clone(),
        mime: "image/x-rgb24".to_string(),
        dims: Some((w, h)),
    });
    env.meta.insert(
        "camera_id".into(),
        serde_json::Value::String(camera_id.clone()),
    );
    env.meta.insert("detections".into(), dets_json);

    let mut meta = camera_flow_request_meta(&camera_id);
    meta.deadline = Some(Instant::now() + CAMERA_FLOW_DEADLINE);
    match disp.dispatch_by_flow_id(flow_id.clone(), env, meta).await {
        Ok(outcome) => {
            // One concise per-run line so operators can see the assigned flow
            // actually executed on a detection frame + its verdict.
            let verdict = outcome
                .final_envelope
                .meta
                .get("verdict")
                .and_then(|v| v.get("decision"))
                .and_then(|d| d.as_str())
                .unwrap_or("-");
            debug!(
                "[vision_analysis] flow {flow_id} ran for {camera_id}: {} detections, verdict={verdict}",
                detections.len()
            );
            let proc_ms = detect_ms + enrich_start.elapsed().as_millis() as u32;
            publish_flow_detections(
                &camera_id,
                captured_ms,
                pts_ns,
                proc_ms,
                detections,
                outcome,
            );
        }
        Err(e) => warn!("[vision_analysis] flow {flow_id} dispatch failed: {e}"),
    }
    // Ephemeral frame: drop the in-memory blob now that the flow has read it, so
    // the cold path never accumulates per-frame frames in memory.
    if let Err(e) = frame_blobs.delete(&blob_ref).await {
        warn!("[vision_analysis] flow {flow_id}: frame blob cleanup failed: {e:#}");
    }
}

/// Publishes a finished flow's detections to the live overlay. A flow that
/// enriches (OCR/classify/verdict) writes the updated detection array back into
/// `meta["detections"]`; when present and parseable we publish those, otherwise
/// we publish the original detection set so the overlay still shows it.
fn publish_flow_detections(
    camera_id: &str,
    captured_ms: u64,
    pts_ns: Option<u64>,
    proc_ms: u32,
    original: Vec<Detection>,
    outcome: crate::flow_engine::envelope::FlowExecutionOutcome,
) {
    let enriched = match outcome.final_envelope.meta.get("detections") {
        Some(v) => match serde_json::from_value::<Vec<Detection>>(v.clone()) {
            Ok(dets) => Some(dets),
            Err(e) => {
                // The flow left a detections value the overlay can't read — fall
                // back to the detector boxes rather than silently dropping them.
                warn!("[vision_analysis] flow {camera_id}: meta[detections] unparseable, publishing original: {e}");
                None
            }
        },
        None => None,
    };
    detection_bus::publish_detections(
        camera_id,
        captured_ms,
        pts_ns,
        proc_ms,
        // Flow overlay: OCR-enriched but NOT vehicle-stamped (the flow engine has
        // no vehicle association), so it feeds the overlay only — never bucketing.
        false,
        enriched.unwrap_or(original),
        detection_bus::MotionSignal::default(),
    );
}

/// Okno swiezosci wpisu w cache wzbogacania. Stan tablicy/nalepki i odczyt OCR
/// sa stabilne w czasie zycia tracku, wiec przez ~3 s reuzywamy raz policzony
/// wynik zamiast liczyc go per klatka. Po uplywie tego okna track jest wzbogacany
/// ponownie (odswiezenie), co lapie realne zmiany (np. tablica zmienila stan).
const ENRICH_TTL: Duration = Duration::from_secs(3);

/// Wiek, po ktorym wpis cache jest usuwany przy ewikcji — wyrazniej dluzszy niz
/// `ENRICH_TTL`, by track, ktory chwilowo zniknal i wrocil, wciaz mial swoj stan.
const ENRICH_CACHE_EVICT_AGE: Duration = Duration::from_secs(10);

/// Co ile zapisow do cache uruchamiamy ewikcje przestarzalych wpisow. Ewikcja
/// licznikowa (analogicznie do leak-fixu trackera) trzyma mape ograniczona bez
/// osobnego watku — martwe tracki znikaja przy okazji kolejnych zapisow.
const ENRICH_EVICT_EVERY: usize = 256;

/// Wynik jednego etapu cold dla jednego tracku. Po przejściu na batchowanie każda
/// klatka liczy classify i OCR OD ZERA (żaden zły pierwszy odczyt się nie utrwala),
/// więc `stan` jest nakładany bezpośrednio i NIE jest już cache'owany. Jedynym
/// stanem cross-frame trzymanym w tej strukturze jest histogram głosów OCR.
///
/// `tekst` niesie AKTUALNIE zwycięski odczyt OCR PO BRAMCE pewności+zgodności —
/// nie surowy odczyt jednej klatki. `tekst_conf` to średnia pewność zwycięzcy
/// (0..1). `tekst_votes` to CONFIDENCE-WEIGHTED histogram głosów per track: OCR
/// chwieje się o ±1 znak klatka-do-klatki (OKR7408↔ORR7408↔DRR7408); poprawne
/// znaki są stałe i czytane z wysoką pewnością, więc ważone głosowanie wygrywa
/// prawdziwą tablicę, a niepewny szum (tablica zasłonięta/rozmyta) nie zbiera ani
/// dużej wagi, ani zgody — bramka zwraca wtedy "nieczytelna" (`None`) zamiast
/// zmyślonego numeru. `at` datuje ostatnie dotknięcie wpisu.
#[derive(Clone)]
struct CachedEnrich {
    stan: Vec<String>,
    tekst: Option<String>,
    tekst_conf: Option<f32>,
    tekst_votes: Vec<TekstVote>,
    at: Instant,
}

/// One variant in a track's confidence-weighted OCR vote. `weight` accumulates
/// the OCR confidence of every frame that read this exact string (so a confident
/// read counts more than a blurry one); `conf_sum`/`count` recover the variant's
/// mean confidence for the gate.
#[derive(Clone)]
struct TekstVote {
    text: String,
    weight: f32,
    conf_sum: f32,
    count: u32,
}

/// Outcome of the confidence+agreement gate over a track's weighted vote.
struct VoteOutcome {
    /// The reported string, or `None` when the evidence is too weak
    /// (unreadable). The winner's mean confidence when `text` is `Some`.
    text: Option<String>,
    confidence: Option<f32>,
}

/// Proces-wide stan glosowania OCR kluczowany po (camera_id, stage_id, track_id) —
/// stage_id to etap COLD pipeline'u, wiec dwa etapy OCR nad tym samym rodzicem nie
/// koliduja. Trzyma histogram glosow per track (`tekst_votes`) + aktualnego
/// zwyciezce; classify NIE jest juz cache'owany (kazda klatka liczy od zera).
fn enrich_cache() -> &'static Mutex<HashMap<(String, String, u32), CachedEnrich>> {
    static C: OnceLock<Mutex<HashMap<(String, String, u32), CachedEnrich>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Zwraca swiezy (`at.elapsed() < ENRICH_TTL`) wpis cache dla (etap, track) albo
/// `None` (brak wpisu lub przeterminowany). Klon jest tani — `stan` to zwykle
/// 0-2 stringi.
fn enrich_cache_fresh(camera_id: &str, stage_id: &str, track_id: u32) -> Option<CachedEnrich> {
    let cache = enrich_cache().lock().unwrap_or_else(|e| e.into_inner());
    cache
        .get(&(camera_id.to_string(), stage_id.to_string(), track_id))
        .filter(|c| c.at.elapsed() < ENRICH_TTL)
        .cloned()
}

/// Maks. liczba różnych stringów trzymanych w histogramie głosów tracku. Kody
/// tablic/ADR są krótkie, a track oscyluje tylko między kilkoma niemal
/// identycznymi odczytami, więc mała mapa wystarcza; przy przepełnieniu wypada
/// najsłabszy wariant.
const OCR_VOTE_MAX_VARIANTS: usize = 8;

/// Górny limit sumy zliczonych odczytów per track. Klamrowanie utrzymuje głos
/// adaptacyjnym: gdy tablica realnie się zmieni (nowy pojazd dostał to samo
/// track_id), stary zwycięzca nie może prowadzić w nieskończoność — po dojściu
/// do limitu histogram jest wykładniczo wygaszany, więc nowe odczyty przejmują
/// prowadzenie.
const OCR_VOTE_MAX_TOTAL: u32 = 30;

/// Weight floor for a single read so a 0-confidence read still nudges its
/// variant (the confidence is a mean softmax prob and is never exactly 0 for a
/// real decode, but this keeps the vote well-defined).
const OCR_VOTE_MIN_WEIGHT: f32 = 0.01;

/// `(min_confidence, min_agreement)` gate thresholds for the OCR vote, resolved
/// from `[vision]` per read mode: plate reads use `plate_min_*`, ADR placards
/// use `adr_min_*` (the ADR UN already passes the `snap_adr` catalog snap, so
/// its confidence floor is lower). `Generic` follows the plate thresholds.
fn ocr_gate_thresholds(mode: &CvOcrMode) -> (f32, f32) {
    let v = crate::vision::settings::get();
    match mode {
        CvOcrMode::Adr => (v.adr_min_confidence, v.adr_min_agreement),
        CvOcrMode::Plate | CvOcrMode::Generic => (v.plate_min_confidence, v.plate_min_agreement),
    }
}

/// Wciela jeden zwalidowany odczyt OCR (string + pewność 0..1) do CONFIDENCE-
/// WEIGHTED histogramu głosów tracku, po czym stosuje bramkę pewności+zgodności:
///   * każdy odczyt dokłada `max(conf, floor)` do wagi swojego wariantu,
///   * zwycięzca = wariant o największej wadze,
///   * `agreement` = waga_zwycięzcy / suma_wag,
///   * zwycięzca jest EMITOWANY tylko gdy jego średnia pewność ≥ `min_conf`
///     ORAZ `agreement ≥ min_agreement`; inaczej `None` ("nieczytelna").
/// To odrzuca powtarzany, ale niepewny i niespójny błąd (zasłonięta tablica),
/// a wpuszcza jeden mocny, zgodny odczyt (agreement 1.0).
fn ocr_vote(
    votes: &mut Vec<TekstVote>,
    read: &str,
    conf: f32,
    min_conf: f32,
    min_agreement: f32,
    prev: Option<&str>,
) -> VoteOutcome {
    let total_count: u32 = votes.iter().map(|v| v.count).sum();
    // Wykładnicze zapominanie: po zliczeniu limitu odczytów połowimy wszystko,
    // aby realnie zmieniona tablica mogła wyprzedzić przestarzałego zwycięzcę.
    if total_count >= OCR_VOTE_MAX_TOTAL {
        votes.retain_mut(|v| {
            v.weight *= 0.5;
            v.conf_sum *= 0.5;
            v.count /= 2;
            v.count > 0
        });
    }
    let w = conf.max(OCR_VOTE_MIN_WEIGHT);
    match votes.iter_mut().find(|v| v.text == read) {
        Some(v) => {
            v.weight += w;
            v.conf_sum += conf;
            v.count += 1;
        }
        None => {
            if votes.len() >= OCR_VOTE_MAX_VARIANTS {
                // Wyrzuć najlżejszy wariant, by ograniczyć mapę — wobble rodzi
                // tylko kilka wariantów, więc ewikcja odpala się rzadko.
                if let Some(pos) = votes
                    .iter()
                    .enumerate()
                    .min_by(|(_, a), (_, b)| a.weight.total_cmp(&b.weight))
                    .map(|(i, _)| i)
                {
                    votes.swap_remove(pos);
                }
            }
            votes.push(TekstVote {
                text: read.to_string(),
                weight: w,
                conf_sum: conf,
                count: 1,
            });
        }
    }
    gate_votes(votes, min_conf, min_agreement, prev)
}

/// Applies the confidence+agreement gate to a track's current weighted votes
/// WITHOUT adding a read — used both after a vote and when a frame produced no
/// read (the winner may still be strong enough from earlier frames). Returns the
/// gated winner or an unreadable outcome.
fn gate_votes(
    votes: &[TekstVote],
    min_conf: f32,
    min_agreement: f32,
    prev: Option<&str>,
) -> VoteOutcome {
    // Winner = MOST-READ variant by RAW COUNT, never by confidence weight. The
    // plate-OCR softmax confidence is near-uniform (~0.05) with per-frame noise,
    // so a confidence-weighted winner could hand the plate to a 21-read blur over
    // a 2018-read correct plate (observed: NM2356 beat WGM1416P). Raw majority
    // over the whole event is the robust signal; agreement = winner_count/total.
    let total_count: u32 = votes.iter().map(|v| v.count).sum();
    if total_count == 0 {
        return VoteOutcome {
            text: None,
            confidence: None,
        };
    }
    let max_count = votes.iter().map(|v| v.count).max().unwrap_or(0);
    // Tie-break to the previously emitted string so a stable read does not
    // flicker between two equally-read variants.
    let winner = prev
        .and_then(|p| votes.iter().find(|v| v.text == p && v.count == max_count))
        .or_else(|| votes.iter().find(|v| v.count == max_count));
    let Some(winner) = winner else {
        return VoteOutcome {
            text: None,
            confidence: None,
        };
    };
    let agreement = winner.count as f32 / total_count as f32;
    let mean_conf = if winner.count > 0 {
        winner.conf_sum / winner.count as f32
    } else {
        0.0
    };
    // Confidence floor stays a no-op unless configured (the model confidence is
    // unreliable); the real gate is agreement. `min_conf` is honored only when a
    // deployment sets it > 0.
    if mean_conf >= min_conf && agreement >= min_agreement {
        VoteOutcome {
            text: Some(winner.text.clone()),
            confidence: Some(mean_conf),
        }
    } else {
        VoteOutcome {
            text: None,
            confidence: None,
        }
    }
}

/// COLD, ścieżka OCR: pod JEDNYM lockiem wciela jeden odczyt (string + pewność)
/// do CONFIDENCE-WEIGHTED histogramu głosów tracku, stosuje bramkę
/// pewności+zgodności i zwraca aktualnego zwycięzcę (albo `None` = nieczytelna).
/// W odróżnieniu od classify OCR nie jest cache-skipowany — re-czytamy co cykl
/// cold (GPU-tani ~2-3 ms) i pozwalamy ważonemu głosowaniu ustabilizować
/// chwiejny znak. `read == None` (odczyt niezwalidowany/nieudany) NIE jest
/// głosowany, ale wciąż PRZELICZA bramkę nad dotychczasowymi głosami (zwycięzca
/// może być już wystarczająco mocny) i odświeża `at`, aby histogram przeżył
/// ewikcję, dopóki track żyje. Ustawia `entry.tekst`/`entry.tekst_conf`.
fn enrich_cache_vote_ocr(
    camera_id: &str,
    stage_id: &str,
    track_id: u32,
    read: Option<(String, f32)>,
    min_conf: f32,
    min_agreement: f32,
) -> (Option<String>, Option<f32>) {
    static PUT_COUNT: AtomicUsize = AtomicUsize::new(0);
    let mut cache = enrich_cache().lock().unwrap_or_else(|e| e.into_inner());
    let entry = cache
        .entry((camera_id.to_string(), stage_id.to_string(), track_id))
        .or_insert_with(|| CachedEnrich {
            stan: Vec::new(),
            tekst: None,
            tekst_conf: None,
            tekst_votes: Vec::new(),
            at: Instant::now(),
        });
    let prev = entry.tekst.clone();
    let outcome = match read {
        Some((read, conf)) => ocr_vote(
            &mut entry.tekst_votes,
            &read,
            conf,
            min_conf,
            min_agreement,
            prev.as_deref(),
        ),
        // No read this frame: re-gate the accumulated votes so an already-strong
        // winner keeps showing and a weak one stays unreadable.
        None => gate_votes(&entry.tekst_votes, min_conf, min_agreement, prev.as_deref()),
    };
    entry.tekst = outcome.text.clone();
    entry.tekst_conf = outcome.confidence;
    entry.at = Instant::now();
    if PUT_COUNT.fetch_add(1, AtomicOrdering::Relaxed) % ENRICH_EVICT_EVERY == 0 {
        cache.retain(|_, c| c.at.elapsed() < ENRICH_CACHE_EVICT_AGE);
    }
    (outcome.text, outcome.confidence)
}

/// Przypisuje detekcji wynik etapu cold zgodnie z jego `output`: classify
/// dokłada etykiety do `stan` (dwa etapy classify nad tym samym rodzicem
/// scalają się), OCR ustawia `tekst` (niepusty wynik wygrywa).
fn apply_stage_output(det: &mut Detection, output: Option<CvStageOutput>, value: &CachedEnrich) {
    match output {
        Some(CvStageOutput::Stan) => det.stan.extend(value.stan.iter().cloned()),
        Some(CvStageOutput::Tekst) => {
            if value.tekst.is_some() {
                det.tekst = value.tekst.clone();
                det.tekst_conf = value.tekst_conf;
            }
        }
        None => {}
    }
}

/// HOT: przypisuje detekcjom etapu `detect_stage_id` aktualnego zwyciezce
/// glosowania OCR (`enrich_cache_fresh`) dla etapow cold, ktore maja ten etap za
/// rodzica — box FAZY 1 niesie ustabilizowany `tekst` natychmiast, bez czekania na
/// cold path. Classify nie jest juz cache'owany, wiec `stan` domalowuje dopiero
/// FAZA 2 (swiezy odczyt per klatka).
fn apply_cached_enrichment(
    camera_id: &str,
    pipeline: &CvPipeline,
    detect_stage_id: &str,
    dets: &mut [Detection],
) {
    for stage in cv_pipeline::cold_stages(pipeline) {
        let CvStageInput::Stage {
            stage_id: parent,
            classes,
        } = &stage.input
        else {
            continue;
        };
        if parent != detect_stage_id {
            continue;
        }
        for det in dets.iter_mut() {
            if det.track_id == 0 || !cv_pipeline::class_matches(classes, &det.klasa) {
                continue;
            }
            if let Some(c) = enrich_cache_fresh(camera_id, &stage.stage_id, det.track_id) {
                apply_stage_output(det, stage.output, &c);
            }
        }
    }
}

// -----------------------------------------------------------------------------
// Event thumbnail capture (full downscaled frame at the best OCR read)
// -----------------------------------------------------------------------------
//
// When a track's plate/ADR OCR winner reaches a NEW best confidence, we snapshot
// the WHOLE camera frame (downscaled), NOT a crop — the operator wants the scene
// where the plate/ADR is clearly visible. The snap ref rides the `Detection`
// (`tekst_thumb_ref`) to the event recorder, which promotes it into the
// `recordings.plate_thumb_ref`/`adr_thumb_ref` list thumbnail. Capture is
// THROTTLED per (camera, ocr_mode, track): a save happens only when the read's
// confidence beats the track's previous best by a margin, so I/O is bounded to a
// handful of snaps per vehicle instead of one per frame.

/// Longest edge of a saved event thumbnail. Full-scene context at a fraction of
/// the frame bytes so the recordings list stays light.
const THUMB_MAX_EDGE: u32 = 480;

/// A read must beat the track's previous best confidence by at least this margin
/// to trigger a fresh thumbnail save — bounds churn from frame-to-frame jitter.
const THUMB_CONF_IMPROVE_MARGIN: f32 = 0.03;

/// Per-(camera, ocr_mode, track) best OCR confidence for which a thumbnail was
/// already captured. Keyed like the enrich cache; reused via the same eviction
/// cadence so dead tracks fall out without a separate sweeper.
struct ThumbBest {
    best_conf: f32,
    at: Instant,
}

fn thumb_best_cache() -> &'static Mutex<HashMap<(String, String, u32), ThumbBest>> {
    static C: OnceLock<Mutex<HashMap<(String, String, u32), ThumbBest>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Returns `true` when `conf` is a new best for this track worth capturing (and
/// records it), `false` otherwise. Under one lock; opportunistically evicts
/// stale entries so the map stays bounded without a background task.
fn thumb_should_capture(camera_id: &str, mode_key: &str, track_id: u32, conf: f32) -> bool {
    static PUT_COUNT: AtomicUsize = AtomicUsize::new(0);
    let mut cache = thumb_best_cache().lock().unwrap_or_else(|e| e.into_inner());
    let key = (camera_id.to_string(), mode_key.to_string(), track_id);
    let capture = match cache.get(&key) {
        Some(prev) => conf >= prev.best_conf + THUMB_CONF_IMPROVE_MARGIN,
        None => true,
    };
    if capture {
        cache.insert(
            key,
            ThumbBest {
                best_conf: conf,
                at: Instant::now(),
            },
        );
    }
    if PUT_COUNT.fetch_add(1, AtomicOrdering::Relaxed) % ENRICH_EVICT_EVERY == 0 {
        cache.retain(|_, b| b.at.elapsed() < ENRICH_CACHE_EVICT_AGE);
    }
    capture
}

/// Minimum wall-time between scene-thumbnail captures for a camera. Guarantees
/// every event gets a photo even when the plate/ADR is never read, without
/// snapshotting every frame.
const SCENE_THUMB_INTERVAL: Duration = Duration::from_secs(3);

/// True at most once per [`SCENE_THUMB_INTERVAL`] per camera — so a vehicle
/// present but unread (no plate/ADR) still yields a full-frame thumbnail.
fn scene_thumb_should_capture(camera_id: &str) -> bool {
    static LAST: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    let mut map = LAST
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    match map.get(camera_id) {
        Some(t) if now.duration_since(*t) < SCENE_THUMB_INTERVAL => false,
        _ => {
            map.insert(camera_id.to_string(), now);
            true
        }
    }
}

/// Downscales the full RGB24 frame so its longest edge is at most
/// [`THUMB_MAX_EDGE`] (keeping aspect, never upscaling), persists it via
/// `save_snapshot_rgb24`, and catalogs a `kind = "snapshot"` `recordings` row so
/// the ref resolves through the signed `/recordings/<ref>` image endpoint the
/// panel renders. Attribution goes to the camera's owning addon/org/retention
/// (same identity the event recorder uses), so the thumbnail lives in the same
/// tenant scope as the clip. Returns the snapshot ref on success, `None` on any
/// failure (a missing thumbnail degrades to a placeholder — never fatal to
/// enrichment). On a DB-insert failure the just-written file is purged so no
/// orphan is left behind, mirroring the host-function snapshot path.
async fn save_event_thumbnail(camera_id: &str, rgb24: &[u8], w: u32, h: u32) -> Option<String> {
    if w == 0 || h == 0 || rgb24.len() != (w as usize) * (h as usize) * 3 {
        return None;
    }
    let longest = w.max(h);
    let (tw, th, data) = if longest > THUMB_MAX_EDGE {
        let scale = THUMB_MAX_EDGE as f32 / longest as f32;
        let tw = ((w as f32 * scale).round() as u32).max(1);
        let th = ((h as f32 * scale).round() as u32).max(1);
        let img = image::RgbImage::from_raw(w, h, rgb24.to_vec())?;
        let resized = image::imageops::resize(&img, tw, th, image::imageops::FilterType::Triangle);
        (tw, th, resized.into_raw())
    } else {
        (w, h, rgb24.to_vec())
    };
    let saved =
        match crate::services::recording::save_snapshot_rgb24(camera_id, &data, tw, th).await {
            Ok(saved) => saved,
            Err(e) => {
                warn_throttled("thumb", &format!("event thumbnail save failed: {e}"));
                return None;
            }
        };
    // Catalog the snapshot so the signed-URL endpoint can serve it. Without a row
    // the ref is un-resolvable and the panel shows a placeholder.
    let camera_id_owned = camera_id.to_string();
    let saved_ref = saved.recording_ref.as_str().to_string();
    let file_path = saved.file_path.clone();
    let cataloged = tokio::task::spawn_blocking(move || {
        let Some(pool) = crate::db::global_pool() else {
            return Err(anyhow::anyhow!("no global DB pool"));
        };
        let (owner_addon_id, org_id, retention_class) =
            match crate::db::repository::camera_recording_identity(&pool, &camera_id_owned)? {
                Some(v) => v,
                None => return Err(anyhow::anyhow!("camera not node-local")),
            };
        crate::db::repository::insert_recording(
            &pool,
            &saved_ref,
            "snapshot",
            &owner_addon_id,
            &camera_id_owned,
            &saved.file_path.to_string_lossy(),
            saved.file_size_bytes as i64,
            None,
            saved.width.map(|v| v as i64),
            saved.height.map(|v| v as i64),
            saved.pixel_format.as_deref(),
            &saved.hash_sha256,
            &retention_class,
            Some(&org_id),
            None,
            None,
            None,
            None,
            None,
        )
        .map(|_| saved_ref)
        .map_err(anyhow::Error::from)
    })
    .await;
    match cataloged {
        Ok(Ok(r)) => Some(r),
        Ok(Err(e)) => {
            warn_throttled("thumb", &format!("event thumbnail catalog failed: {e}"));
            let _ = crate::services::recording::purge_recording(&file_path).await;
            None
        }
        Err(e) => {
            warn_throttled("thumb", &format!("event thumbnail catalog task: {e}"));
            None
        }
    }
}

/// Minimum overlap fraction `area(s∩v)/area(s)` for the max-overlap FALLBACK: a
/// sign whose center lands in no vehicle box is still assigned to the vehicle it
/// overlaps most, but only if that overlap covers at least this fraction of the
/// sign. Below it the sign is left unassigned (`vehicle_id = 0`).
const MIN_VEHICLE_OVERLAP: f32 = 0.3;

/// One vehicle box for association: normalized `[x, y, w, h]` + its stable
/// `vehicle_id` (the "vehicles" tracker track_id). Decoupled from `Detection`
/// so [`assign_vehicle`] is a pure, testable function with no pipeline deps.
#[derive(Debug, Clone, Copy)]
struct VehicleBox {
    bbox: [f32; 4],
    vehicle_id: u32,
}

/// Assigns a sign/plate/sticker box to the vehicle it sits on. PURE + testable.
///
/// Rule (per the per-truck plan):
///   1. CENTER-IN-BOX: every vehicle whose box contains the sign's center is a
///      candidate. Ties (overlapping vehicles both containing the center) break
///      by (a) larger containment fraction `area(s∩v)/area(s)`, then (b) smaller
///      vehicle area (the tighter box is the real owner), then (c) smaller
///      `vehicle_id` (stable, deterministic).
///   2. FALLBACK — no vehicle contains the center: the vehicle with the largest
///      overlap fraction, accepted only if it is ≥ [`MIN_VEHICLE_OVERLAP`].
///   3. Otherwise `0` (unassigned): kept for overlay, excluded from per-truck
///      grouping.
fn assign_vehicle(sign_bbox: &[f32; 4], vehicles: &[VehicleBox]) -> u32 {
    if vehicles.is_empty() {
        return 0;
    }
    let (sx, sy, sw, sh) = (sign_bbox[0], sign_bbox[1], sign_bbox[2], sign_bbox[3]);
    let sign_area = (sw.max(0.0)) * (sh.max(0.0));
    let cx = sx + sw * 0.5;
    let cy = sy + sh * 0.5;

    // Containment fraction area(s∩v)/area(s) of the sign inside a vehicle box.
    let contain_frac = |v: &VehicleBox| -> f32 {
        if sign_area <= 0.0 {
            return 0.0;
        }
        let (vx, vy, vw, vh) = (v.bbox[0], v.bbox[1], v.bbox[2], v.bbox[3]);
        let ix1 = sx.max(vx);
        let iy1 = sy.max(vy);
        let ix2 = (sx + sw).min(vx + vw);
        let iy2 = (sy + sh).min(vy + vh);
        let iw = (ix2 - ix1).max(0.0);
        let ih = (iy2 - iy1).max(0.0);
        (iw * ih) / sign_area
    };
    let vehicle_area = |v: &VehicleBox| (v.bbox[2].max(0.0)) * (v.bbox[3].max(0.0));

    // CENTER-IN-BOX candidates.
    let mut best: Option<&VehicleBox> = None;
    for v in vehicles {
        let (vx, vy, vw, vh) = (v.bbox[0], v.bbox[1], v.bbox[2], v.bbox[3]);
        let inside = cx >= vx && cx <= vx + vw && cy >= vy && cy <= vy + vh;
        if !inside {
            continue;
        }
        best = Some(match best {
            None => v,
            Some(cur) => {
                // Larger containment fraction wins; then smaller vehicle area;
                // then smaller vehicle_id.
                let (fv, fc) = (contain_frac(v), contain_frac(cur));
                if fv > fc + f32::EPSILON {
                    v
                } else if fc > fv + f32::EPSILON {
                    cur
                } else {
                    let (av, ac) = (vehicle_area(v), vehicle_area(cur));
                    if av < ac - f32::EPSILON {
                        v
                    } else if ac < av - f32::EPSILON {
                        cur
                    } else if v.vehicle_id < cur.vehicle_id {
                        v
                    } else {
                        cur
                    }
                }
            }
        });
    }
    if let Some(v) = best {
        return v.vehicle_id;
    }

    // FALLBACK: max overlap ≥ MIN_VEHICLE_OVERLAP.
    let mut best_overlap = 0.0f32;
    let mut best_id = 0u32;
    for v in vehicles {
        let f = contain_frac(v);
        if f > best_overlap {
            best_overlap = f;
            best_id = v.vehicle_id;
        }
    }
    if best_overlap >= MIN_VEHICLE_OVERLAP {
        best_id
    } else {
        0
    }
}

/// Stamps `vehicle_id` on every sign/plate/sticker detection of a frame from the
/// tracked vehicle boxes. Vehicle boxes themselves (`klasa == "vehicle"`) get
/// their own track_id as `vehicle_id` (self-assignment) so the overlay/grouping
/// is uniform. Called at the TAIL of `run_cold_stages` (the full frame is known
/// and enrichment is done). No-op when `vehicles` is empty (degrades to today's
/// single-bag behavior — every sign keeps `vehicle_id = 0`).
fn stamp_vehicle_ids(stage_dets: &mut [(String, Vec<Detection>)], vehicles: &[VehicleBox]) {
    for (_, dets) in stage_dets.iter_mut() {
        for det in dets.iter_mut() {
            if det.klasa == "vehicle" {
                det.vehicle_id = det.track_id;
                continue;
            }
            det.vehicle_id = assign_vehicle(&det.bbox, vehicles);
        }
    }
}

/// A cold stage resolved to its parent detections and pre-cut crops, ready for a
/// batched forward. Built in one sequential pass so the forward (phase 2) borrows
/// nothing from `stage_dets` (`items` owns index + crop) and independent stages
/// overlap; results are applied back in pipeline order (phase 3).
struct ColdStagePlan {
    /// Detect-stage id whose `stage_dets` entry holds the parent detections.
    parent: String,
    stage_id: String,
    op: CvOp,
    ocr_mode: CvOcrMode,
    model: String,
    output: Option<CvStageOutput>,
    /// Local batchable engine alias when the model resolves to one, else `None`
    /// (mesh/remote/onnx-cv → per-crop executor fallback).
    engine: Option<String>,
    /// `(detection index in parent dets, crop, crop_w, crop_h, class)` per crop.
    items: Vec<(usize, Arc<[u8]>, u32, u32, String)>,
}

/// Runs ONE cold stage's batched forward over its pre-cut crops and returns a
/// `CachedEnrich` per crop (order == `items`). Touches no shared detection state,
/// so several stages' forwards overlap via `join_all`; the caller applies the
/// results sequentially in pipeline order. Batchowana sciezka lokalna dla znanych
/// silnikow; kazdy inny przypadek (w tym zwrot None / niezgodna dlugosc batcha)
/// schodzi na per-crop fallback, wiec porazka batcha nigdy cicho nie gubi
/// wzbogacenia.
async fn cold_stage_forward(
    executor: &Arc<ModelRuntimeExecutor>,
    model: &str,
    op: CvOp,
    ocr_mode: &CvOcrMode,
    engine: Option<&str>,
    items: &[(usize, Arc<[u8]>, u32, u32, String)],
    crops: &[(Arc<[u8]>, u32, u32)],
) -> Vec<CachedEnrich> {
    match (op, engine) {
        (CvOp::Classify, Some("nalepka-stan")) => match classify_batch_local(crops).await {
            Some(labels) if labels.len() == crops.len() => labels
                .into_iter()
                .map(|stan| CachedEnrich {
                    stan,
                    tekst: None,
                    tekst_conf: None,
                    tekst_votes: Vec::new(),
                    at: Instant::now(),
                })
                .collect(),
            _ => cold_stage_per_crop(executor, model, op, ocr_mode.clone(), items).await,
        },
        (CvOp::Ocr, Some("plate-ocr"))
            if matches!(ocr_mode, CvOcrMode::Plate | CvOcrMode::Generic) =>
        {
            match read_batch_local(crops).await {
                Some(reads) if reads.len() == crops.len() => reads
                    .into_iter()
                    .map(|(tekst, conf)| CachedEnrich {
                        stan: Vec::new(),
                        tekst_conf: tekst.as_ref().map(|_| conf),
                        tekst,
                        tekst_votes: Vec::new(),
                        at: Instant::now(),
                    })
                    .collect(),
                _ => cold_stage_per_crop(executor, model, op, ocr_mode.clone(), items).await,
            }
        }
        // ADR placards: submit every crop to the cross-camera ADR batcher — the
        // rows of ALL placards (from all cameras) run as ONE forward instead of one
        // tiny 2-row forward per placard. A crop the CRNN could not read (or whose
        // UN failed the `snap_adr` catalog snap) keeps today's per-crop executor
        // path, which retries the CRNN and then falls back to PP-OCRv5 — exactly
        // `ocr_adr_local`'s semantics, so no read is ever lost to batching.
        #[cfg(all(
            feature = "vision-ort",
            not(any(target_os = "macos", target_os = "ios"))
        ))]
        (CvOp::Ocr, Some("plate-ocr")) if matches!(ocr_mode, CvOcrMode::Adr) => {
            match adr_batch_local(crops).await {
                Some(reads) if reads.len() == crops.len() => {
                    let mut outputs: Vec<CachedEnrich> = reads
                        .into_iter()
                        .map(|(tekst, conf)| CachedEnrich {
                            stan: Vec::new(),
                            tekst_conf: tekst.as_ref().map(|_| conf),
                            tekst,
                            tekst_votes: Vec::new(),
                            at: Instant::now(),
                        })
                        .collect();
                    let missed: Vec<usize> = outputs
                        .iter()
                        .enumerate()
                        .filter(|(_, o)| o.tekst.is_none())
                        .map(|(i, _)| i)
                        .collect();
                    if !missed.is_empty() {
                        let missed_items: Vec<_> =
                            missed.iter().map(|&i| items[i].clone()).collect();
                        let fallback = cold_stage_per_crop(
                            executor,
                            model,
                            op,
                            ocr_mode.clone(),
                            &missed_items,
                        )
                        .await;
                        for (&i, value) in missed.iter().zip(fallback) {
                            outputs[i] = value;
                        }
                    }
                    outputs
                }
                _ => cold_stage_per_crop(executor, model, op, ocr_mode.clone(), items).await,
            }
        }
        _ => cold_stage_per_crop(executor, model, op, ocr_mode.clone(), items).await,
    }
}

/// COLD: generyczny interpreter etapow `stage` pipeline'u. Dla kazdego etapu
/// wybiera detekcje rodzica pasujace klasami (`class_matches`), wycina crop z
/// paddingiem etapu i wzbogaca: classify → `stan`, ocr → `tekst` (tryb z
/// `params.ocr_mode`), embed → jawny skip z warn-once. Wszystkie cropy etapu ida
/// JEDNYM zbatchowanym forwardem gdy alias rozwiazuje sie do lokalnego silnika
/// (`classify_batch`/`read_batch`); inaczej per-crop fallback przez executor
/// (`cold_stage_per_crop`). Kazda klatka czyta OD ZERA — brak cache-skipu, jedynym
/// stanem cross-frame jest histogram glosow OCR. Zaden blad nie wychodzi na
/// zewnatrz — etap bez wyniku zostawia pole puste. Na koncu asocjuje kazdy
/// znak/tablice z pojazdem (`stamp_vehicle_ids`).
async fn run_cold_stages(
    camera_id: &str,
    frame: &[u8],
    frame_device: Option<&super::fakefile::DeviceCropsFrame>,
    w: u32,
    h: u32,
    frame_format: &super::fakefile::DetectFrameFormat,
    pipeline: &CvPipeline,
    stage_dets: &mut [(String, Vec<Detection>)],
    vehicles: &mut [Detection],
) {
    let Some(executor) = runtime_executor() else {
        warn_throttled(
            "cold-executor",
            "runtime executor unavailable; cold enrichment skipped",
        );
        // Even without enrichment, associate signs to vehicles for overlay/grouping.
        stamp_vehicle_ids(stage_dets, &vehicle_boxes(vehicles));
        return;
    };
    // Wzbogacenie budowane od zera z cache/forwardow etapow — hot path zdazyl
    // juz przypisac swieze wpisy cache tej samej klatce (FAZA 1), wiec reset
    // chroni przed zdublowanym `stan` przy ponownym `extend`.
    for (_, dets) in stage_dets.iter_mut() {
        for det in dets.iter_mut() {
            det.stan.clear();
            det.tekst = None;
        }
    }
    // PHASE 1 (sequential, cheap): resolve each enabled cold stage to its parent
    // detections and cut every matching crop. Reads `stage_dets` immutably. The
    // produced `items` own their (index + crop) data, so the forwards in phase 2
    // borrow nothing from `stage_dets` and can overlap. The pipeline validator's
    // depth-2 invariant (`cv_pipeline::validate`: a crop stage may only hang off a
    // detect/frame stage, never another crop stage) guarantees no cold stage reads
    // another cold stage's output — the crops select on `bbox`/`klasa`/`track_id`,
    // which cold stages never write — so the stages are provably independent.
    let mut plans: Vec<ColdStagePlan> = Vec::new();
    for stage in cv_pipeline::cold_stages(pipeline) {
        let CvStageInput::Stage {
            stage_id: parent,
            classes,
        } = &stage.input
        else {
            continue;
        };
        if stage.op == CvOp::Embed {
            // Jedyny dopuszczony brak wykonania: walidator przyjmuje op=embed,
            // ale silnik nie ma jeszcze operacji Embed na surface CameraCv —
            // etap jest JAWNIE pomijany (nigdy cicho).
            warn_once(
                &format!("embed:{}", stage.stage_id),
                &format!(
                    "stage '{}': op=embed not executable yet (CameraCv surface has no Embed op); stage skipped",
                    stage.stage_id
                ),
            );
            continue;
        }
        let op = stage.op;
        // Tylko classify/ocr wzbogacaja — detect nie ma tu forwardu (embed
        // odfiltrowany wyzej), wiec pomijamy caly etap bez zbierania cropow.
        if !matches!(op, CvOp::Classify | CvOp::Ocr) {
            continue;
        }
        let (pad_x, pad_y) = cv_pipeline::crop_pads(stage);
        let ocr_mode = match cv_pipeline::ocr_mode(stage) {
            "adr" => CvOcrMode::Adr,
            "plate" => CvOcrMode::Plate,
            _ => CvOcrMode::Generic,
        };
        let Some((_, dets)) = stage_dets.iter().find(|(sid, _)| sid == parent) else {
            continue;
        };
        // Zbierz WSZYSTKIE pasujace klasami detekcje tego etapu w JEDNA liste
        // (indeks detekcji + crop Arc + wymiary + klasa do logu). NIE ma juz
        // skipu cache po tracku: kazda klatka czyta od zera (zly pierwszy odczyt
        // nie moze sie utrwalic) — jedynym stanem cross-frame jest histogram
        // glosow OCR (`enrich_cache_vote_ocr`). `items` niesie dane WLASNE, wiec
        // batchowany forward / fallback nie pozycza `dets` przez await; wyniki
        // nakladamy mutowalnie PO obliczeniu (indeks wraca po pozycji w `items`).
        let mut items: Vec<(usize, Arc<[u8]>, u32, u32, String)> = Vec::new();
        for (idx, det) in dets.iter().enumerate() {
            if !cv_pipeline::class_matches(classes, &det.klasa) {
                continue;
            }
            let Some((x0, y0, cw, ch)) = padded_crop_rect(w, h, &det.bbox, pad_x, pad_y) else {
                continue;
            };
            // NV12 crops are cut + converted per-crop (small, off the full-frame
            // convert); the crop may be even-snapped, so use its returned dims.
            let (rgb, acw, ach) =
                crop_for_detection(frame, frame_device, w, h, frame_format, x0, y0, cw, ch);
            // VERIFY gate: compare the device crop against the host-download crop.
            verify_zerocopy_crop(frame_device, w, h, frame_format, x0, y0, cw, ch, &rgb);
            let crop: Arc<[u8]> = Arc::from(rgb);
            items.push((idx, crop, acw, ach, det.klasa.clone()));
        }
        if items.is_empty() {
            continue;
        }

        // Czy alias etapu rozwiazuje sie do LOKALNEGO wbudowanego silnika, ktory
        // umiemy zbatchowac jednym forwardem? Jesli tak — omijamy per-crop
        // indirection egzekutora i wolamy singleton wprost (`classify_batch` /
        // `read_batch`). Jesli nie (mesh/zdalny/onnx-cv/tryb ADR) — fallback na
        // istniejaca sciezke per-crop przez `execute_camera_cv`.
        let engine = {
            // §2.5 — a resolver PROBE (does this alias land on a local embedded
            // engine?), not a dispatch: nothing is executed under this context.
            let mut ctx = RuntimeContext::new(
                None,
                FlowOrigin::Camera,
                FlowActor::system_component("vision_stage_probe"),
            );
            executor.local_camera_cv_engine(&stage.model, &mut ctx)
        };
        plans.push(ColdStagePlan {
            parent: parent.clone(),
            stage_id: stage.stage_id.clone(),
            op,
            ocr_mode,
            model: stage.model.clone(),
            output: stage.output,
            engine,
            items,
        });
    }

    // PHASE 2 (concurrent, expensive): overlap the independent stage forwards.
    // OCR now dominates the cold path (enrich_ms≈70-86 vs detect_ms≈18-32) and the
    // plate-OCR / ADR-OCR / classify stages operate on disjoint crops with no data
    // dependency, so their batched forwards run at max(stage) latency instead of
    // sum(stages). Each forward carries owned `items`/`crops` and touches no shared
    // detection state; the process-wide batchers are built for concurrent submit.
    let forwards = plans.iter().map(|plan| {
        let executor = executor.clone();
        let crops: Vec<(Arc<[u8]>, u32, u32)> = plan
            .items
            .iter()
            .map(|(_, crop, cw, ch, _)| (crop.clone(), *cw, *ch))
            .collect();
        async move {
            cold_stage_forward(
                &executor,
                &plan.model,
                plan.op,
                &plan.ocr_mode,
                plan.engine.as_deref(),
                &plan.items,
                &crops,
            )
            .await
        }
    });
    let all_outputs: Vec<Vec<CachedEnrich>> = futures::future::join_all(forwards).await;

    // PHASE 3 (sequential, cheap): apply each stage's results IN PIPELINE ORDER so
    // OCR vote histograms, thumbnail throttles and `stan` extends stay byte-for-byte
    // identical to the pre-concurrency sequential path. Naloz wyniki na wlasciwe
    // detekcje po indeksie z `items` (bez pomieszania cropow). OCR: glosowanie
    // temporalne per track stabilizuje chwiejny znak (surowy odczyt wciela sie do
    // histogramu, `det.tekst` = zwyciezca); track bez id (track_id=0) emituje
    // surowy odczyt wprost. Classify: `stan` przypisany od zera, bez cache-put.
    for (plan, outputs) in plans.iter().zip(all_outputs) {
        let Some((_, dets)) = stage_dets.iter_mut().find(|(sid, _)| sid == &plan.parent) else {
            continue;
        };
        let (min_conf, min_agreement) = ocr_gate_thresholds(&plan.ocr_mode);
        // Key thumbnail throttling by the READ MODE (plate vs adr) so a plate and
        // an ADR read on the same track_id keep independent best-confidence
        // baselines and each produces its own scene thumbnail.
        let thumb_mode_key = match &plan.ocr_mode {
            CvOcrMode::Adr => "adr",
            CvOcrMode::Plate | CvOcrMode::Generic => "plate",
        };
        // Lazily built full-scene RGB frame (whole camera image), produced at most
        // once per stage and only when a capture actually fires — the NV12
        // download/convert on the zero-copy path is not free.
        for ((idx, _, _, _, _), value) in plan.items.iter().zip(outputs) {
            let det = &mut dets[*idx];
            if plan.op == CvOp::Ocr && det.track_id > 0 {
                let read = value.tekst.map(|t| (t, value.tekst_conf.unwrap_or(0.0)));
                let (tekst, conf) = enrich_cache_vote_ocr(
                    camera_id,
                    &plan.stage_id,
                    det.track_id,
                    read,
                    min_conf,
                    min_agreement,
                );
                // Snapshot the WHOLE downscaled frame (not the crop) as the event
                // thumbnail: on a new best read for this track, OR on the per-camera
                // scene throttle so an event ALWAYS gets a photo even when the plate/
                // ADR is never read (a vehicle is present — this OCR stage is running
                // on its placard/plate detection). Never overwrite a thumb already
                // captured for this detection.
                let want_best = conf
                    .map(|c| thumb_should_capture(camera_id, thumb_mode_key, det.track_id, c))
                    .unwrap_or(false);
                if det.tekst_thumb_ref.is_none()
                    && (want_best || scene_thumb_should_capture(camera_id))
                {
                    if let Some(rgb) = cold_full_rgb(frame, frame_device, w, h, frame_format) {
                        det.tekst_thumb_ref = save_event_thumbnail(camera_id, &rgb, w, h).await;
                    }
                }
                det.tekst = tekst;
                det.tekst_conf = conf;
            } else {
                apply_stage_output(det, plan.output, &value);
            }
        }
    }
    // TAIL: per-truck association. Now that every stage's boxes carry a stable
    // track_id (hot path) and their reads (this cold pass), stamp each sign/plate
    // with the vehicle it sits on so the recorder can group per truck.
    stamp_vehicle_ids(stage_dets, &vehicle_boxes(vehicles));

    // GUARANTEE a photo for EVERY event: the read-driven thumbnail above only
    // fires inside an OCR stage, so a truck whose plate/ADR is never read (or one
    // that shows only stickers) would get no picture. If no detection captured a
    // thumbnail this frame, snapshot the whole scene once (per-camera throttled)
    // and attach it to the largest vehicle box — else the first sign — so the
    // recorder always promotes a list thumbnail.
    let any_thumb = stage_dets
        .iter()
        .flat_map(|(_, d)| d.iter())
        .any(|d| d.tekst_thumb_ref.is_some());
    if !any_thumb && scene_thumb_should_capture(camera_id) {
        if let Some(rgb) = cold_full_rgb(frame, frame_device, w, h, frame_format) {
            if let Some(thumb_ref) = save_event_thumbnail(camera_id, &rgb, w, h).await {
                let vbox_area = |d: &Detection| (d.bbox[2].max(0.0)) * (d.bbox[3].max(0.0));
                if let Some(v) = vehicles
                    .iter_mut()
                    .max_by(|a, b| vbox_area(a).total_cmp(&vbox_area(b)))
                {
                    v.tekst_thumb_ref = Some(thumb_ref);
                } else if let Some(d) = stage_dets.iter_mut().flat_map(|(_, d)| d.iter_mut()).next()
                {
                    d.tekst_thumb_ref = Some(thumb_ref);
                }
            }
        }
    }
}

/// Maps published vehicle detections to the `[VehicleBox]` association inputs,
/// dropping any with `track_id = 0` (an untracked box has no stable vehicle_id
/// to attribute signs to).
fn vehicle_boxes(vehicles: &[Detection]) -> Vec<VehicleBox> {
    vehicles
        .iter()
        .filter(|d| d.track_id != 0)
        .map(|d| VehicleBox {
            bbox: d.bbox,
            vehicle_id: d.track_id,
        })
        .collect()
}

/// Materializes the FULL RGB24 camera frame for a thumbnail capture. On the host
/// path `frame` already holds RGB (or NV12 that `nv12_to_rgb_if_needed`
/// converts); on the GPU zero-copy path `frame` is empty, so the full NV12 is
/// downloaded from the device once and converted. `None` when no frame is
/// recoverable (capture is then simply skipped this cycle).
fn cold_full_rgb(
    frame: &[u8],
    frame_device: Option<&super::fakefile::DeviceCropsFrame>,
    w: u32,
    h: u32,
    frame_format: &super::fakefile::DetectFrameFormat,
) -> Option<Arc<[u8]>> {
    if !frame.is_empty() {
        let arc: Arc<[u8]> = Arc::from(frame.to_vec());
        return Some(nv12_to_rgb_if_needed(&arc, w, h, frame_format));
    }
    let (nv12, fmt) = frame_device?.download_full_nv12()?;
    Some(nv12_to_rgb_if_needed(&nv12, w, h, &fmt))
}

/// COLD fallback per-crop: gdy alias etapu NIE rozwiazuje sie do lokalnego
/// batchowalnego silnika (mesh/zdalny/onnx-cv/ADR), kazdy crop idzie osobno przez
/// `execute_camera_cv` (`classify_crop`/`ocr_crop`). Forwardy lecą RÓWNOLEGLE
/// (`join_all`) — pule sesji sa concurrency-safe, wiec latencja etapu to max(crop)
/// zamiast sum(crops). Zwraca `CachedEnrich` per crop w kolejnosci `items`.
async fn cold_stage_per_crop(
    executor: &Arc<ModelRuntimeExecutor>,
    alias: &str,
    op: CvOp,
    ocr_mode: CvOcrMode,
    items: &[(usize, Arc<[u8]>, u32, u32, String)],
) -> Vec<CachedEnrich> {
    let jobs = items.iter().map(|(_, crop, cw, ch, klasa)| {
        let executor = executor.clone();
        let model = alias.to_string();
        let crop = crop.clone();
        let (cw, ch) = (*cw, *ch);
        let klasa = klasa.clone();
        let ocr_mode = ocr_mode.clone();
        async move {
            let (stan, tekst) = match op {
                CvOp::Classify => (
                    classify_crop(&executor, &model, crop, cw, ch, &klasa)
                        .await
                        .unwrap_or_default(),
                    None,
                ),
                CvOp::Ocr => (
                    Vec::new(),
                    ocr_crop(&executor, &model, crop, cw, ch, ocr_mode, &klasa).await,
                ),
                CvOp::Detect | CvOp::Embed => (Vec::new(), None),
            };
            // The executor path (mesh/remote/PP-OCRv5) carries NO per-read
            // confidence — its `CameraCvResult::Text` has already passed the
            // engine's own validation (`waliduj_tablice_pl` / `snap_adr`), so a
            // hit is treated as fully confident (`1.0`) for the vote. It then
            // still gates on AGREEMENT, matching the pre-confidence behavior of
            // this fallback while the primary batched path gates on real scores.
            let tekst_conf = tekst.as_ref().map(|_| EXECUTOR_READ_CONFIDENCE);
            CachedEnrich {
                stan,
                tekst,
                tekst_conf,
                tekst_votes: Vec::new(),
                at: Instant::now(),
            }
        }
    });
    futures::future::join_all(jobs).await
}

/// Confidence assigned to an executor-path (per-crop fallback) OCR hit, which
/// carries no numeric score of its own. The read already passed the engine's
/// validation, so it clears the confidence floor and is gated on agreement only.
const EXECUTOR_READ_CONFIDENCE: f32 = 1.0;

/// COLD batched local classify: every crop of the stage is SUBMITTED to the
/// process-wide cross-camera state batcher, so crops from ALL cameras aggregate
/// into one big `StateClassifier::classify_batch` forward instead of a tiny
/// per-camera batch-1. `None` (batcher unavailable / any per-crop error) →
/// caller drops to the per-crop fallback (never loses enrichment). The blocking
/// `submit_all` runs on the blocking pool, off the tokio worker.
#[cfg(feature = "vision-ort")]
async fn classify_batch_local(crops: &[(Arc<[u8]>, u32, u32)]) -> Option<Vec<Vec<String>>> {
    let batcher = crate::vision::inference_batcher::state_batcher().await?;
    let crops = crops.to_vec();
    let results = match tokio::task::spawn_blocking(move || batcher.submit_all(&crops)).await {
        Ok(results) => results,
        Err(e) => {
            warn_throttled("classify", &format!("state batcher task: {e}"));
            return None;
        }
    };
    let mut labels = Vec::with_capacity(results.len());
    for r in results {
        match r {
            Ok(l) => labels.push(l),
            Err(e) => {
                warn_throttled("classify", &format!("state batcher submit: {e:#}"));
                return None;
            }
        }
    }
    Some(labels)
}

/// Ścieżka Burn: forward serializowany na jednym watku wgpu przez `run_blocking`
/// + `Mutex` (jak `classify_local`); `classify_batch` sam petli po cropach.
#[cfg(not(feature = "vision-ort"))]
async fn classify_batch_local(crops: &[(Arc<[u8]>, u32, u32)]) -> Option<Vec<Vec<String>>> {
    let classifier = get_classifier().await?;
    let crops = crops.to_vec();
    match crate::vision::burn_backend::run_blocking(move || {
        let guard = classifier.lock().unwrap_or_else(|e| e.into_inner());
        guard.classify_batch(&crops)
    })
    .await
    {
        Ok(Ok(labels)) => Some(labels),
        Ok(Err(e)) => {
            warn_throttled("classify", &format!("classify_batch: {e:#}"));
            None
        }
        Err(e) => {
            warn_throttled("classify", &format!("classify_batch task: {e}"));
            None
        }
    }
}

/// COLD batched local plate OCR: every crop of the stage is SUBMITTED to the
/// process-wide cross-camera plate batcher, so crops from ALL cameras aggregate
/// into one big `PlateOcr::read_batch` forward instead of a tiny per-camera
/// batch-1. `None` (batcher unavailable / any per-crop error) → caller drops to
/// the per-crop fallback. `submit_all` blocks on the blocking pool.
#[cfg(feature = "vision-ort")]
async fn read_batch_local(crops: &[(Arc<[u8]>, u32, u32)]) -> Option<Vec<(Option<String>, f32)>> {
    let batcher = crate::vision::inference_batcher::plate_batcher().await?;
    let crops = crops.to_vec();
    let results = match tokio::task::spawn_blocking(move || batcher.submit_all(&crops)).await {
        Ok(results) => results,
        Err(e) => {
            warn_throttled("ocr", &format!("plate batcher task: {e}"));
            return None;
        }
    };
    let mut reads = Vec::with_capacity(results.len());
    for r in results {
        match r {
            Ok(t) => reads.push(t),
            Err(e) => {
                warn_throttled("ocr", &format!("plate batcher submit: {e:#}"));
                return None;
            }
        }
    }
    Some(reads)
}

/// COLD batched local ADR OCR: every placard crop of the stage is SUBMITTED to
/// the process-wide cross-camera ADR batcher, so the rows of ALL placards from
/// ALL cameras aggregate into one `AdrOcr::read_adr_batch` forward instead of a
/// tiny 2-row forward per placard. The raw `(kemler, un)` read gets the same
/// `snap_adr` catalog snap as `ocr_adr_local`; a per-crop `None` (nothing read /
/// UN not in the catalog) is a RESULT — the caller re-routes just those crops
/// through the per-crop executor path to keep the PP-OCRv5 fallback. Outer
/// `None` (batcher unavailable / any submit error) → the caller falls back
/// per-crop for the whole stage (never loses enrichment). `submit_all` blocks
/// on the blocking pool, off the tokio worker.
#[cfg(all(
    feature = "vision-ort",
    not(any(target_os = "macos", target_os = "ios"))
))]
async fn adr_batch_local(crops: &[(Arc<[u8]>, u32, u32)]) -> Option<Vec<(Option<String>, f32)>> {
    let batcher = crate::vision::inference_batcher::adr_batcher().await?;
    let crops = crops.to_vec();
    let results = match tokio::task::spawn_blocking(move || batcher.submit_all(&crops)).await {
        Ok(results) => results,
        Err(e) => {
            warn_throttled("ocr", &format!("adr batcher task: {e}"));
            return None;
        }
    };
    let mut reads = Vec::with_capacity(results.len());
    for r in results {
        match r {
            // Keep the read's confidence alongside the catalog-snapped UN; a UN
            // that fails `snap_adr` is a miss (None) and its confidence is moot.
            Ok(read) => reads.push(match read {
                Some((_, un, conf)) => (crate::vision::adr::snap_adr(&un), conf),
                None => (None, 0.0),
            }),
            Err(e) => {
                warn_throttled("ocr", &format!("adr batcher submit: {e:#}"));
                return None;
            }
        }
    }
    Some(reads)
}

/// Ścieżka Burn: forward serializowany na jednym watku wgpu przez `run_blocking`
/// + `Mutex` (jak `ocr_local`); `read_batch` sam petli po cropach.
#[cfg(not(feature = "vision-ort"))]
async fn read_batch_local(crops: &[(Arc<[u8]>, u32, u32)]) -> Option<Vec<(Option<String>, f32)>> {
    let ocr = get_ocr().await?;
    let crops = crops.to_vec();
    match crate::vision::burn_backend::run_blocking(move || {
        let guard = ocr.lock().unwrap_or_else(|e| e.into_inner());
        guard.read_batch(&crops)
    })
    .await
    {
        Ok(Ok(reads)) => Some(reads),
        Ok(Err(e)) => {
            warn_throttled("ocr", &format!("read_batch: {e:#}"));
            None
        }
        Err(e) => {
            warn_throttled("ocr", &format!("read_batch task: {e}"));
            None
        }
    }
}

/// COLD: klasyfikacja stanu jednego cropu przez executor (alias etapu).
/// `None` = etap bez wyniku (blad executora / nieoczekiwany wariant) — nigdy
/// blad na zewnatrz.
async fn classify_crop(
    executor: &Arc<ModelRuntimeExecutor>,
    alias: &str,
    crop: Arc<[u8]>,
    cw: u32,
    ch: u32,
    klasa: &str,
) -> Option<Vec<String>> {
    let request = CameraCvRequest {
        model: alias.to_string(),
        op: CameraCvOpLocal::ClassifyState {
            crop: CvFrameLocal {
                data: crop,
                width: cw,
                height: ch,
            },
        },
    };
    // §2.5 — camera cold path; the crop helper knows its stage, not the camera.
    let mut ctx = RuntimeContext::new(
        None,
        FlowOrigin::Camera,
        FlowActor::system_component("vision_classify"),
    );
    match executor.execute_camera_cv(request, &mut ctx).await {
        Ok(CameraCvResult::Labels { stan }) => Some(stan),
        Ok(_) => {
            warn_throttled(
                "classify",
                &format!("classify {klasa} via '{alias}': unexpected camera-cv result variant"),
            );
            None
        }
        Err(e) => {
            warn_throttled("classify", &format!("classify {klasa} via '{alias}': {e}"));
            None
        }
    }
}

/// COLD: OCR jednego cropu przez executor (alias etapu, tryb z parametrow
/// etapu). Sukces z `tekst = None` znaczy "nic nie rozpoznano" — to wynik,
/// nie blad. `None` przy bledzie executora — nigdy blad na zewnatrz.
async fn ocr_crop(
    executor: &Arc<ModelRuntimeExecutor>,
    alias: &str,
    crop: Arc<[u8]>,
    cw: u32,
    ch: u32,
    mode: CvOcrMode,
    klasa: &str,
) -> Option<String> {
    let request = CameraCvRequest {
        model: alias.to_string(),
        op: CameraCvOpLocal::Ocr {
            crop: CvFrameLocal {
                data: crop,
                width: cw,
                height: ch,
            },
            mode,
        },
    };
    // §2.5 — camera cold path; the crop helper knows its stage, not the camera.
    let mut ctx = RuntimeContext::new(
        None,
        FlowOrigin::Camera,
        FlowActor::system_component("vision_ocr"),
    );
    match executor.execute_camera_cv(request, &mut ctx).await {
        Ok(CameraCvResult::Text { tekst }) => tekst,
        Ok(_) => {
            warn_throttled(
                "ocr",
                &format!("ocr {klasa} via '{alias}': unexpected camera-cv result variant"),
            );
            None
        }
        Err(e) => {
            warn_throttled("ocr", &format!("ocr {klasa} via '{alias}': {e}"));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vb(id: u32, x: f32, y: f32, w: f32, h: f32) -> VehicleBox {
        VehicleBox {
            bbox: [x, y, w, h],
            vehicle_id: id,
        }
    }

    /// Two overlapping vehicles: the sign's center lands inside BOTH, so the
    /// tie-break (larger containment, then smaller vehicle area, then smaller id)
    /// must pick the vehicle that actually contains the sign more tightly.
    #[test]
    fn assign_vehicle_two_overlapping_picks_tighter() {
        // Big vehicle 1 covers the left half; small vehicle 2 is a tight box
        // fully around the sign, both containing the sign center.
        let big = vb(1, 0.0, 0.0, 0.6, 1.0);
        let small = vb(2, 0.35, 0.30, 0.20, 0.20);
        // Sign fully inside the small box (and inside the big one too).
        let sign = [0.40, 0.35, 0.05, 0.05];
        // Small box fully contains the sign (frac 1.0) and has smaller area → wins.
        assert_eq!(assign_vehicle(&sign, &[big, small]), 2);
    }

    /// A sign whose center is NOT inside any vehicle but overlaps one ≥ 0.3 is
    /// assigned by the max-overlap fallback; below the floor it stays unassigned.
    #[test]
    fn assign_vehicle_cutoff_box_uses_overlap_fallback() {
        // Vehicle occupies the top-left quadrant.
        let v = vb(7, 0.0, 0.0, 0.5, 0.5);
        // Sign straddles the right edge: center at x=0.5 is ON the border (inside),
        // so nudge it just outside to force the fallback. Half of the sign overlaps.
        let sign = [0.45, 0.20, 0.20, 0.10]; // center x=0.55 outside the box
                                             // Overlap = x in [0.45,0.5] → 0.05 wide × 0.10 tall = 0.005; sign area
                                             // 0.20×0.10 = 0.02 → frac 0.25 < 0.3 → unassigned.
        assert_eq!(assign_vehicle(&sign, &[v]), 0);
        // Widen the vehicle so ≥30% of the sign overlaps → assigned.
        let v2 = vb(7, 0.0, 0.0, 0.6, 0.5);
        // Overlap x in [0.45,0.6] → 0.15 wide but sign ends at 0.65, so overlap
        // width = min(0.65,0.6)-0.45 = 0.15 × 0.10 = 0.015; frac 0.75 ≥ 0.3 → 7.
        assert_eq!(assign_vehicle(&sign, &[v2]), 7);
    }

    /// A background sign far from every vehicle stays unassigned (id 0).
    #[test]
    fn assign_vehicle_background_sign_unassigned() {
        let v = vb(3, 0.0, 0.0, 0.3, 0.3);
        let sign = [0.80, 0.80, 0.05, 0.05];
        assert_eq!(assign_vehicle(&sign, &[v]), 0);
        // No vehicles at all → unassigned.
        assert_eq!(assign_vehicle(&sign, &[]), 0);
    }

    /// A single vehicle containing the sign's center is assigned directly.
    #[test]
    fn assign_vehicle_single_vehicle_center_in_box() {
        let v = vb(42, 0.1, 0.1, 0.6, 0.6);
        let sign = [0.30, 0.30, 0.05, 0.05];
        assert_eq!(assign_vehicle(&sign, &[v]), 42);
    }

    /// `stamp_vehicle_ids`: vehicle boxes self-assign (vehicle_id = track_id),
    /// signs get their owning vehicle, background signs get 0.
    #[test]
    fn stamp_vehicle_ids_routes_signs_and_self_assigns_vehicles() {
        let mk = |klasa: &str, bbox: [f32; 4], track: u32| {
            let mut d = detection_from_cv(CvDetection {
                klasa: klasa.into(),
                bbox,
                score: 0.9,
            });
            d.track_id = track;
            d
        };
        let mut stage_dets = vec![(
            "adr".to_string(),
            vec![
                mk("tablica_adr", [0.30, 0.30, 0.04, 0.04], 5), // inside vehicle 9
                mk("tablica_rejestracyjna", [0.90, 0.90, 0.03, 0.03], 6), // background
            ],
        )];
        let vehicles = vec![VehicleBox {
            bbox: [0.1, 0.1, 0.6, 0.6],
            vehicle_id: 9,
        }];
        // A vehicle box detection self-assigns.
        stage_dets.push((
            "veh".to_string(),
            vec![mk("vehicle", [0.1, 0.1, 0.6, 0.6], 9)],
        ));
        stamp_vehicle_ids(&mut stage_dets, &vehicles);
        assert_eq!(stage_dets[0].1[0].vehicle_id, 9, "ADR sits on vehicle 9");
        assert_eq!(
            stage_dets[0].1[1].vehicle_id, 0,
            "background plate unassigned"
        );
        assert_eq!(stage_dets[1].1[0].vehicle_id, 9, "vehicle box self-assigns");
    }

    /// Mapowanie CvDetection→Detection: pola detektora przechodzą 1:1, pola
    /// wzbogacenia/śledzenia startują puste (nadaje je tracker i cold path).
    #[test]
    fn detection_from_cv_mapuje_pola_i_zeruje_wzbogacenie() {
        let d = detection_from_cv(CvDetection {
            klasa: "tablica_adr".into(),
            bbox: [0.1, 0.2, 0.3, 0.4],
            score: 0.9,
        });
        assert_eq!(d.klasa, "tablica_adr");
        assert_eq!(d.bbox, [0.1, 0.2, 0.3, 0.4]);
        assert_eq!(d.score, 0.9);
        assert!(d.stan.is_empty());
        assert!(d.tekst.is_none());
        assert_eq!(d.track_id, 0);
        assert_eq!(d.vx, 0.0);
        assert_eq!(d.vy, 0.0);
    }

    /// Crop bez paddingu = dokładny box detekcji (dawna ścieżka classify);
    /// z paddingiem 15%/10% = poszerzony box zaklamrowany do klatki (dawna
    /// ścieżka OCR). Box < 8 px w którymkolwiek wymiarze → None.
    #[test]
    fn padded_crop_rect_matches_legacy_classify_and_ocr_paths() {
        // 100x100 frame, box (10,20,40,30) px → bbox znormalizowany.
        let bbox = [0.10, 0.20, 0.40, 0.30];
        assert_eq!(
            padded_crop_rect(100, 100, &bbox, 0.0, 0.0),
            Some((10, 20, 40, 30))
        );
        // pad_x = 0.15*40 = 6 px, pad_y = 0.10*30 = 3 px z każdej strony.
        assert_eq!(
            padded_crop_rect(100, 100, &bbox, 0.15, 0.10),
            Some((4, 17, 52, 36))
        );
        // Padding zaklamrowany do granic klatki (box przy krawędzi).
        let edge = [0.0, 0.0, 0.40, 0.30];
        assert_eq!(
            padded_crop_rect(100, 100, &edge, 0.15, 0.10),
            Some((0, 0, 46, 33))
        );
        // Box mniejszy niż 8 px → brak cropu.
        assert_eq!(
            padded_crop_rect(100, 100, &[0.0, 0.0, 0.05, 0.5], 0.0, 0.0),
            None
        );
    }

    /// `apply_stage_output`: classify dokłada etykiety do `stan` (dwa etapy
    /// się scalają), OCR ustawia `tekst` tylko przy niepustym wyniku.
    #[test]
    fn apply_stage_output_extends_stan_and_sets_tekst() {
        let mut det = detection_from_cv(CvDetection {
            klasa: "tablica_adr".into(),
            bbox: [0.1, 0.2, 0.3, 0.4],
            score: 0.9,
        });
        let stan_a = CachedEnrich {
            stan: vec!["ok".into()],
            tekst: None,
            tekst_conf: None,
            tekst_votes: Vec::new(),
            at: Instant::now(),
        };
        let stan_b = CachedEnrich {
            stan: vec!["czytelna".into()],
            tekst: None,
            tekst_conf: None,
            tekst_votes: Vec::new(),
            at: Instant::now(),
        };
        apply_stage_output(&mut det, Some(CvStageOutput::Stan), &stan_a);
        apply_stage_output(&mut det, Some(CvStageOutput::Stan), &stan_b);
        assert_eq!(det.stan, vec!["ok".to_string(), "czytelna".to_string()]);

        let ocr_hit = CachedEnrich {
            stan: Vec::new(),
            tekst: Some("30/1203".into()),
            tekst_conf: None,
            tekst_votes: Vec::new(),
            at: Instant::now(),
        };
        let ocr_miss = CachedEnrich {
            stan: Vec::new(),
            tekst: None,
            tekst_conf: None,
            tekst_votes: Vec::new(),
            at: Instant::now(),
        };
        apply_stage_output(&mut det, Some(CvStageOutput::Tekst), &ocr_hit);
        // Pusty wynik OCR nie kasuje wcześniejszego odczytu.
        apply_stage_output(&mut det, Some(CvStageOutput::Tekst), &ocr_miss);
        assert_eq!(det.tekst.as_deref(), Some("30/1203"));
        // Etap bez outputu (detect/embed) niczego nie zmienia.
        apply_stage_output(&mut det, None, &ocr_hit);
        assert_eq!(det.stan.len(), 2);
    }

    /// Gate defaults used by the vote unit tests (mirror the `[vision]` plate
    /// defaults so the tests read like the production gate).
    const MC: f32 = 0.5;
    const MA: f32 = 0.5;

    /// `ocr_vote` (confidence-weighted): a stream of consistent HIGH-confidence
    /// reads with ±1-char wobble still resolves to the true plate (majority of
    /// the weight), and the tie-break holds the previously emitted string.
    #[test]
    fn ocr_vote_majority_stabilizes_wobble() {
        let mut votes: Vec<TekstVote> = Vec::new();
        // High-confidence wobble OKR7408↔ORR7408↔DRR7408 around one true plate.
        assert_eq!(
            ocr_vote(&mut votes, "OKR7408", 0.9, MC, MA, None)
                .text
                .as_deref(),
            Some("OKR7408")
        );
        assert_eq!(
            ocr_vote(&mut votes, "OKR7408", 0.9, MC, MA, Some("OKR7408"))
                .text
                .as_deref(),
            Some("OKR7408")
        );
        // A single low-weight wobble variant does not dislodge the leader.
        assert_eq!(
            ocr_vote(&mut votes, "ORR7408", 0.6, MC, MA, Some("OKR7408"))
                .text
                .as_deref(),
            Some("OKR7408")
        );
    }

    /// (a) 7 DISAGREEING low-confidence reads (the field case) → unreadable:
    /// neither confidence nor agreement clears the gate, so no plate is emitted.
    #[test]
    fn ocr_vote_low_conf_disagreement_is_unreadable() {
        let mut votes: Vec<TekstVote> = Vec::new();
        let mut out: Option<String> = None;
        for s in [
            "M88901", "M88901", "N59156", "B67K71", "M88901", "DRR740", "SR9961",
        ] {
            out = ocr_vote(&mut votes, s, 0.30, MC, MA, out.as_deref()).text;
        }
        assert!(
            out.is_none(),
            "occluded plate must be unreadable, not a guess"
        );
    }

    /// (b) ONE high-confidence valid read → reported immediately (agreement 1.0);
    /// the gate must not require many frames.
    #[test]
    fn ocr_vote_single_high_conf_read_is_reported() {
        let mut votes: Vec<TekstVote> = Vec::new();
        let out = ocr_vote(&mut votes, "WPL5HJ2", 0.94, MC, MA, None);
        assert_eq!(out.text.as_deref(), Some("WPL5HJ2"));
        assert!((out.confidence.unwrap() - 0.94).abs() < 1e-6);
    }

    /// (c) 5 consistent high-confidence reads → reported with agreement ~1.0.
    #[test]
    fn ocr_vote_consistent_high_conf_reported_full_agreement() {
        let mut votes: Vec<TekstVote> = Vec::new();
        let mut prev: Option<String> = None;
        for _ in 0..5 {
            prev = ocr_vote(&mut votes, "WWL7322", 0.9, MC, MA, prev.as_deref()).text;
        }
        assert_eq!(prev.as_deref(), Some("WWL7322"));
        let total: f32 = votes.iter().map(|v| v.weight).sum();
        let winner = votes.iter().find(|v| v.text == "WWL7322").unwrap();
        assert!((winner.weight / total - 1.0).abs() < 1e-6, "agreement ~1.0");
    }

    /// (d) The winner is decided by RAW READ COUNT, never by confidence weight.
    ///
    /// This deliberately reverses the original confidence-weighted rule. The
    /// plate-OCR softmax is near-uniform in production (~0.05) and its per-frame
    /// noise does not correlate with correctness, so weighting handed the plate
    /// to a 21-read blur over a 2018-read correct plate on real traffic. Raw
    /// majority is the robust signal: 4×"M88901" beats 2×"WPL5HJ2" here, and on
    /// real data the true plate is the one that accumulates the reads.
    #[test]
    fn ocr_vote_winner_is_raw_count_not_confidence() {
        let mut votes: Vec<TekstVote> = Vec::new();
        let mut prev: Option<String> = None;
        // Both variants clear the confidence floor, so this isolates the WINNER
        // rule from the readability gate: only the read COUNT may decide.
        for _ in 0..4 {
            prev = ocr_vote(&mut votes, "M88901", 0.60, MC, MA, prev.as_deref()).text;
        }
        for _ in 0..2 {
            prev = ocr_vote(&mut votes, "WPL5HJ2", 0.95, MC, MA, prev.as_deref()).text;
        }
        let final_outcome = gate_votes(&votes, MC, MA, None);
        assert_eq!(
            final_outcome.text.as_deref(),
            Some("M88901"),
            "most-read variant wins even though the other read scored higher"
        );
    }

    /// Histogram jest ograniczony: >OCR_VOTE_MAX_VARIANTS różnych stringów nie
    /// rozdyma mapy — najlżejszy wariant wypada.
    #[test]
    fn ocr_vote_bounds_variant_count() {
        let mut votes: Vec<TekstVote> = Vec::new();
        for i in 0..(OCR_VOTE_MAX_VARIANTS + 5) {
            ocr_vote(&mut votes, &format!("PLATE{i}"), 0.8, MC, MA, None);
        }
        assert!(votes.len() <= OCR_VOTE_MAX_VARIANTS);
    }

    /// Pipeline jednoetapowy (jeden `detect` na klatce) — minimum do złożenia
    /// [`FrameJob`] w testach domykania.
    fn detect_pipeline(stage: &str) -> CvPipeline {
        CvPipeline {
            stages: vec![cv_pipeline::CvStage {
                stage_id: stage.to_string(),
                enabled: true,
                op: CvOp::Detect,
                model: "m".into(),
                input: CvStageInput::Frame { fps: None },
                threshold: None,
                params: serde_json::Map::new(),
                output: None,
            }],
        }
    }

    /// Zones are a FULL attention gate: with zones drawn, a detection whose box
    /// centre sits outside every polygon is dropped before tracking/enrichment;
    /// with no zones nothing is filtered (existing cameras behave as before).
    #[test]
    fn zones_gate_detections_by_box_centre() {
        let det = |x: f32, y: f32| Detection {
            klasa: "tablica_adr".into(),
            bbox: [x, y, 0.05, 0.05],
            score: 0.9,
            stan: Vec::new(),
            tekst: None,
            tekst_conf: None,
            tekst_thumb_ref: None,
            track_id: 0,
            vehicle_id: 0,
            vx: 0.,
            vy: 0.,
        };
        // Square covering the LEFT half of the frame.
        let zones = parse_zone_polygons("[[[0.0,0.0],[0.5,0.0],[0.5,1.0],[0.0,1.0]]]");
        assert_eq!(zones.len(), 1, "one polygon parsed");

        let mut dets = vec![det(0.10, 0.10), det(0.80, 0.10)];
        retain_in_zones(&mut dets, &zones);
        assert_eq!(dets.len(), 1, "only the in-zone detection survives");
        assert!(dets[0].bbox[0] < 0.5);

        // No zones → pass-through (backward compatible).
        let mut all = vec![det(0.10, 0.10), det(0.80, 0.10)];
        retain_in_zones(&mut all, &[]);
        assert_eq!(all.len(), 2, "no zones means no filtering");

        // Malformed / degenerate polygons are ignored, never partially applied.
        assert!(parse_zone_polygons("nonsense").is_empty());
        assert!(
            parse_zone_polygons("[[[0.1,0.1],[0.2,0.2]]]").is_empty(),
            "2 points is not an area"
        );
    }

    fn make_job(cam: &str, captured: u64, pipeline: &Arc<CvPipeline>, stage: &str) -> FrameJob {
        FrameJob {
            camera_id: cam.to_string(),
            zones: Arc::new(Vec::new()),
            frame: Arc::from(vec![0u8; 12]),
            frame_device: None,
            w: 2,
            h: 2,
            frame_format: crate::services::camera_ingest::fakefile::DetectFrameFormat::Rgb24,
            detect_frame: Arc::from(vec![0u8; 12]),
            detect_device: None,
            detect_w: 2,
            detect_h: 2,
            detect_format: crate::services::camera_ingest::fakefile::DetectFrameFormat::Rgb24,
            captured_ms: captured,
            pts_ns: None,
            pipeline: pipeline.clone(),
            open_stages: vec![stage.to_string()],
            results: Vec::new(),
            detect_ms_total: 0,
            failed_stages: 0,
            vehicles: Vec::new(),
            submitted_at: std::time::Instant::now(),
        }
    }

    fn empty_detections() -> ForwardOutput {
        ForwardOutput {
            alias: "m".into(),
            detect_ms: 3,
            outcome: Ok(CameraCvResult::Detections {
                per_frame: vec![Vec::new()],
            }),
            vehicles: vec![Vec::new()],
        }
    }

    /// K>1: gdy `[vision] inflight` jest ustawione, wygrywa; inaczej K
    /// odwzorowuje `[vision] detector_sessions`; przy configu domyślnym → K=4,
    /// lustrzanie do domyślnej puli detektora (4 pipelinowane forwardy ≈ 3×
    /// przepustowości detektora vs serializacja).
    #[test]
    fn inflight_limit_follows_detector_sessions_and_honors_override() {
        let defaults = crate::config::VisionConfig::default();
        assert_eq!(
            resolve_inflight_limit(defaults.inflight, defaults.detector_sessions),
            4,
            "default config → K=4 (default detector pool)"
        );
        assert_eq!(
            resolve_inflight_limit(None, 4),
            4,
            "K mirrors the detector pool"
        );
        assert_eq!(
            resolve_inflight_limit(Some(2), 4),
            2,
            "explicit inflight wins over the detector pool"
        );
        assert_eq!(
            resolve_inflight_limit(Some(999), 4),
            MAX_INFLIGHT,
            "upper clamp to MAX_INFLIGHT"
        );
        assert_eq!(resolve_inflight_limit(Some(0), 4), 1, "lower clamp to 1");
    }

    /// Ordering gate: kamera z otwartym jobem NIE przyjmuje drugiej klatki, więc
    /// nawet przy K współbieżnych forwardach jedna kamera ma ≤1 klatkę w locie —
    /// jej publikacje pozostają monotoniczne po `captured_ms`.
    #[test]
    fn ordering_gate_blocks_second_frame_while_camera_job_open() {
        let pipeline = Arc::new(detect_pipeline("det"));
        let mut jobs: HashMap<u64, FrameJob> = HashMap::new();
        jobs.insert(1, make_job("camX", 100, &pipeline, "det"));
        assert!(jobs.values().any(|j| j.camera_id == "camX"));
        assert!(!jobs.values().any(|j| j.camera_id == "camY"));
    }

    /// K>1 poza kolejnością: dwie kamery, po jednej klatce w locie (ordering gate
    /// gwarantuje ≤1 job/kamera). Forwardy kończą się W ODWROTNEJ kolejności
    /// (młodsza kamera pierwsza) — wynik każdego routuje się do WŁASNEGO joba po
    /// `job_id`, więc publikacja niesie `captured_ms` tej właśnie kamery. Dowodzi,
    /// że współbieżne, nie-w-kolejności zakończenia nie mieszają strumieni kamer
    /// i nie regresują per-kamerowej kolejności publikacji.
    #[test]
    fn out_of_order_completion_routes_each_camera_independently() {
        let nonce = std::process::id();
        let cam_a = format!("test-cam-a-{nonce}");
        let cam_b = format!("test-cam-b-{nonce}");
        let mut rx_a = detection_bus::subscribe(&cam_a);
        let mut rx_b = detection_bus::subscribe(&cam_b);

        let pipeline = Arc::new(detect_pipeline("det"));
        let mut jobs: HashMap<u64, FrameJob> = HashMap::new();
        jobs.insert(1, make_job(&cam_a, 100, &pipeline, "det"));
        jobs.insert(2, make_job(&cam_b, 50, &pipeline, "det"));

        let (cold_tx, _cold_rx) = mpsc::channel::<DetectionEvent>(4);
        let item_a = PendingItem {
            job_id: 1,
            stage_id: "det".into(),
            alias: "m".into(),
            threshold: None,
            added: Instant::now(),
        };
        let item_b = PendingItem {
            job_id: 2,
            stage_id: "det".into(),
            alias: "m".into(),
            threshold: None,
            added: Instant::now(),
        };

        // Kamera B (młodsza klatka, job_id=2) kończy PIERWSZA — celowo poza
        // kolejnością względem starszej kamery A.
        apply_forward_result(&mut jobs, vec![item_b], Ok(empty_detections()), &cold_tx);
        assert!(jobs.contains_key(&1), "job kamery A wciąż otwarty");
        assert!(!jobs.contains_key(&2), "job kamery B sfinalizowany");
        apply_forward_result(&mut jobs, vec![item_a], Ok(empty_detections()), &cold_tx);
        assert!(jobs.is_empty(), "oba joby sfinalizowane");

        let msg_a = rx_a.try_recv().expect("kamera A opublikowana");
        let msg_b = rx_b.try_recv().expect("kamera B opublikowana");
        assert_eq!(msg_a.ts_ms, 100, "publikacja A niesie captured_ms kamery A");
        assert_eq!(msg_b.ts_ms, 50, "publikacja B niesie captured_ms kamery B");
    }

    /// Panika/abort zadania forwardu (`JoinError`) NIE może zostawić otwartych
    /// etapów — inaczej ordering gate zablokowałby kamerę na zawsze. Błąd joina
    /// domyka wszystkie pozycje batcha jako porażkę i finalizuje job.
    #[test]
    fn join_error_closes_batch_stages_no_leak() {
        let pipeline = Arc::new(detect_pipeline("det"));
        let mut jobs: HashMap<u64, FrameJob> = HashMap::new();
        jobs.insert(7, make_job("cam-join-err", 10, &pipeline, "det"));
        let (cold_tx, _cold_rx) = mpsc::channel::<DetectionEvent>(4);
        let item = PendingItem {
            job_id: 7,
            stage_id: "det".into(),
            alias: "m".into(),
            threshold: None,
            added: Instant::now(),
        };
        // Symulacja porażki joina: cała pozycja domknięta jako porażka. Job bez
        // żadnego udanego etapu finalizuje się bez publikacji (results puste).
        apply_forward_result(&mut jobs, vec![item], Ok(force_error_output()), &cold_tx);
        assert!(jobs.is_empty(), "job domknięty mimo błędu — brak wycieku");
    }

    fn force_error_output() -> ForwardOutput {
        ForwardOutput {
            alias: "m".into(),
            detect_ms: 0,
            outcome: Err("simulated forward failure".into()),
            vehicles: Vec::new(),
        }
    }

    /// P1: gdy forward ukończy się, jego permit jest zwolniony ZANIM wynik zostanie
    /// zaaplikowany — to okno, które drenaż-przed-acquire zamyka. Test dowodzi obu
    /// faktów (permit wolny + wynik wciąż w JoinSet) oraz dyscypliny „apply przed
    /// acquire": po zastosowaniu wyniku permit jest ponownie dostępny.
    #[tokio::test]
    async fn completed_forward_frees_permit_before_result_is_applied() {
        let sem = Arc::new(tokio::sync::Semaphore::new(1));
        let mut forwards: tokio::task::JoinSet<u64> = tokio::task::JoinSet::new();
        let permit = sem
            .clone()
            .try_acquire_owned()
            .expect("permit wolny na starcie");
        forwards.spawn(async move {
            let _p = permit;
            1u64
        });
        // Forward w locie ⇒ K=1 wyczerpane ⇒ nowy forward NIE spawnuje się.
        assert!(
            sem.clone().try_acquire_owned().is_err(),
            "permit zajęty w trakcie forwardu (K=1)"
        );
        // Poczekaj aż zadanie się zakończy (permit zwolniony), ale NIE aplikuj.
        for _ in 0..1000 {
            if sem.available_permits() == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        // Okno hazardu: permit JUŻ wolny, ale wynik WCIĄŻ nieaplikowany w JoinSet.
        assert_eq!(
            sem.available_permits(),
            1,
            "permit zwolniony po zakończeniu"
        );
        let joined = forwards
            .try_join_next()
            .expect("wynik gotowy, lecz jeszcze niezaaplikowany");
        assert_eq!(
            joined.unwrap(),
            1,
            "drenaż stosuje wynik ukończonego forwardu"
        );
    }

    /// P1: `drain_ready_forwards` stosuje ukończone forwardy (finalizuje joby)
    /// przez ten sam realny path co pętla (`apply_joined`).
    #[tokio::test]
    async fn drain_ready_forwards_applies_completed_jobs() {
        let pipeline = Arc::new(detect_pipeline("det"));
        let mut jobs: HashMap<u64, FrameJob> = HashMap::new();
        jobs.insert(5, make_job("cam-drain-ready", 77, &pipeline, "det"));
        let (cold_tx, _cold_rx) = mpsc::channel::<DetectionEvent>(4);
        let mut forwards: tokio::task::JoinSet<ForwardOutput> = tokio::task::JoinSet::new();
        let mut inflight: HashMap<tokio::task::Id, Vec<PendingItem>> = HashMap::new();
        let item = PendingItem {
            job_id: 5,
            stage_id: "det".into(),
            alias: "m".into(),
            threshold: None,
            added: Instant::now(),
        };
        let handle = forwards.spawn(async move { empty_detections() });
        inflight.insert(handle.id(), vec![item]);
        // Powtarzaj realny drenaż aż forward się zakończy i job sfinalizuje.
        for _ in 0..1000 {
            drain_ready_forwards(&mut forwards, &mut jobs, &mut inflight, &cold_tx);
            if jobs.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            jobs.is_empty(),
            "ukończony forward zaaplikowany, job sfinalizowany"
        );
        assert!(
            inflight.is_empty(),
            "wpis routingu usunięty po zastosowaniu"
        );
    }

    /// P2: graceful-drain AWAITuje trwający (wolny) forward do końca i APLIKUJE go —
    /// nie porzuca/abortuje. Dowód: job z 50 ms forwardem finalizuje się po pętli
    /// `join_next_with_id().await` (ta sama pętla co w silniku na shutdown).
    #[tokio::test]
    async fn graceful_drain_awaits_and_applies_inflight_forward() {
        let pipeline = Arc::new(detect_pipeline("det"));
        let mut jobs: HashMap<u64, FrameJob> = HashMap::new();
        jobs.insert(9, make_job("cam-grace", 33, &pipeline, "det"));
        let (cold_tx, _cold_rx) = mpsc::channel::<DetectionEvent>(4);
        let mut forwards: tokio::task::JoinSet<ForwardOutput> = tokio::task::JoinSet::new();
        let mut inflight: HashMap<tokio::task::Id, Vec<PendingItem>> = HashMap::new();
        let item = PendingItem {
            job_id: 9,
            stage_id: "det".into(),
            alias: "m".into(),
            threshold: None,
            added: Instant::now(),
        };
        let handle = forwards.spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            empty_detections()
        });
        inflight.insert(handle.id(), vec![item]);
        // Pętla graceful-drain silnika: await KAŻDEGO trwającego forwardu + apply.
        while let Some(joined) = forwards.join_next_with_id().await {
            apply_joined(&mut jobs, &mut inflight, Some(joined), &cold_tx);
        }
        assert!(
            jobs.is_empty(),
            "trwający forward dokończony i zaaplikowany (await), nie porzucony"
        );
    }

    /// §2.5 — the camera entry point stamps `camera` origin and names the CAMERA
    /// as a system actor, with no user behind it. Drives the production builder
    /// `run_camera_flow` uses, so changing the entry point's stamp fails here;
    /// constructing a `FlowRequestMeta` in the test body and asserting its own
    /// arguments would prove nothing about the entry point.
    #[test]
    fn camera_entry_point_stamps_camera_origin_and_names_the_camera() {
        use crate::flow_engine::dispatcher::ActorKind;

        let meta = camera_flow_request_meta("front-door");
        assert_eq!(meta.origin, FlowOrigin::Camera);
        assert_eq!(meta.actor_kind, ActorKind::System);
        assert_eq!(meta.actor_id.as_deref(), Some("front-door"));
        // No human in the loop: an actor_user_id here would attribute an
        // unattended pipeline run to a person.
        assert_eq!(meta.actor_user_id, None);
        assert!(meta.user_id.is_none());
        assert_eq!(meta.request_id, "cam-front-door");

        // Distinct cameras stay distinguishable in the log.
        let other = camera_flow_request_meta("gate-2");
        assert_eq!(other.actor_id.as_deref(), Some("gate-2"));
    }
}
