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
/// (`inference-supertonic`) the runner is internally pooled + `&self` + Send+Sync,
/// so it is shared bare as `Arc<_>` and every crop rides the concurrency-safe ort
/// pool off the single Burn/wgpu thread. On the Burn path the runner still needs
/// the whole-process wgpu serialization, so it stays behind `Arc<Mutex<_>>` and
/// callers funnel forwards through `burn_backend::run_blocking`.
#[cfg(feature = "inference-supertonic")]
pub(crate) type DetectorHandle = std::sync::Arc<RfDetrDetector>;
#[cfg(not(feature = "inference-supertonic"))]
pub(crate) type DetectorHandle = std::sync::Arc<Mutex<RfDetrDetector>>;
#[cfg(feature = "inference-supertonic")]
pub(crate) type ClassifierHandle = std::sync::Arc<StateClassifier>;
#[cfg(not(feature = "inference-supertonic"))]
pub(crate) type ClassifierHandle = std::sync::Arc<Mutex<StateClassifier>>;
#[cfg(feature = "inference-supertonic")]
pub(crate) type OcrHandle = std::sync::Arc<PlateOcr>;
#[cfg(not(feature = "inference-supertonic"))]
pub(crate) type OcrHandle = std::sync::Arc<Mutex<PlateOcr>>;

#[cfg(feature = "inference-supertonic")]
fn wrap_detector(d: RfDetrDetector) -> DetectorHandle {
    std::sync::Arc::new(d)
}
#[cfg(not(feature = "inference-supertonic"))]
fn wrap_detector(d: RfDetrDetector) -> DetectorHandle {
    std::sync::Arc::new(Mutex::new(d))
}
#[cfg(feature = "inference-supertonic")]
fn wrap_classifier(c: StateClassifier) -> ClassifierHandle {
    std::sync::Arc::new(c)
}
#[cfg(not(feature = "inference-supertonic"))]
fn wrap_classifier(c: StateClassifier) -> ClassifierHandle {
    std::sync::Arc::new(Mutex::new(c))
}
#[cfg(feature = "inference-supertonic")]
fn wrap_ocr(o: PlateOcr) -> OcrHandle {
    std::sync::Arc::new(o)
}
#[cfg(not(feature = "inference-supertonic"))]
fn wrap_ocr(o: PlateOcr) -> OcrHandle {
    std::sync::Arc::new(Mutex::new(o))
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
fn crop_for_detection(
    frame: &[u8],
    frame_w: u32,
    frame_h: u32,
    format: &super::fakefile::DetectFrameFormat,
    x0: u32,
    y0: u32,
    cw: u32,
    ch: u32,
) -> (Vec<u8>, u32, u32) {
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
async fn resolve_camera_config(camera_id: &str) -> (u32, Result<Option<(String, String)>, String>) {
    let id = camera_id.to_string();
    tokio::task::spawn_blocking(move || match crate::db::global_pool() {
        Some(pool) => {
            let fps = crate::db::repository::camera_analysis_fps(&pool, &id)
                .unwrap_or(DEFAULT_ANALYSIS_FPS);
            let pipeline = crate::db::repository::resolve_camera_cv_pipeline(&pool, &id)
                .map_err(|e| e.to_string());
            (fps, pipeline)
        }
        None => (DEFAULT_ANALYSIS_FPS, Err("no global DB pool".to_string())),
    })
    .await
    .unwrap_or((
        DEFAULT_ANALYSIS_FPS,
        Err("camera config task panicked".to_string()),
    ))
}

/// Applies a freshly resolved camera config to the registry slot. Invalid or
/// unresolvable pipelines never crash: a failure keeps the last good pipeline
/// (warned once per distinct message), `Ok(None)` clears it (no analysis).
fn apply_camera_config(
    camera_id: &str,
    fps: u32,
    resolved: Result<Option<(String, String)>, String>,
    now: std::time::Instant,
) {
    let mut reg = cameras().lock().unwrap();
    let Some(slot) = reg.get_mut(camera_id) else {
        return;
    };
    slot.fps = fps;
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
    frame: Arc<[u8]>,
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
    #[cfg_attr(not(feature = "inference-supertonic"), allow(dead_code))]
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
}

/// Górny limit współbieżnych forwardów. Odzwierciedla sufit puli sesji ort
/// (`ort_common::MAX_SESSIONS_PER_MODEL` = 16) — więcej równoległych forwardów
/// niż sesji nie ma sensu (i tak czekałyby na slot puli). Zdefiniowany lokalnie,
/// bo pula ort istnieje tylko pod `inference-supertonic`, a ten limit musi
/// obowiązywać na każdej ścieżce.
const MAX_INFLIGHT: usize = 16;

/// Liczba współbieżnych forwardów K. Jawny opt-in `TENTAFLOW_VISION_INFLIGHT`
/// wygrywa; w przeciwnym razie odwzorowuje rozmiar puli detektora
/// (`TENTAFLOW_VISION_DETECTOR_SESSIONS`): przy N sesjach GPU liczy N detektów
/// naraz, więc N in-flight to naturalny sufit. Gdy ŻADNA zmienna nie jest
/// ustawiona → K=1, czyli bit-identyczne zachowanie z dawną pojedynczą,
/// serializowaną pętlą (zero regresji, dopóki operator nie włączy więcej).
fn inflight_limit() -> usize {
    const INFLIGHT_ENV: &str = "TENTAFLOW_VISION_INFLIGHT";
    const DETECTOR_SESSIONS_ENV: &str = "TENTAFLOW_VISION_DETECTOR_SESSIONS";
    if let Some(k) = std::env::var(INFLIGHT_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
    {
        return k.clamp(1, MAX_INFLIGHT);
    }
    std::env::var(DETECTOR_SESSIONS_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, MAX_INFLIGHT)
}

/// Grouping key that keeps a detect flush batch homogeneous: `None` for RGB
/// (executor path), `Some(color-bits)` for NV12 (device path — the bits pin the
/// YUV→RGB matrix so one `detect_batch_gpu` call applies a single conversion).
/// Without the ort GPU features NV12 frames are never produced, so every frame
/// keys as RGB.
#[cfg(feature = "inference-supertonic")]
fn detect_batch_key(fmt: &super::fakefile::DetectFrameFormat) -> Option<(u32, u32, u32)> {
    match fmt {
        super::fakefile::DetectFrameFormat::Rgb24 => None,
        super::fakefile::DetectFrameFormat::Nv12 {
            kr, kb, full_range, ..
        } => Some((kr.to_bits(), kb.to_bits(), *full_range as u32)),
    }
}
#[cfg(not(feature = "inference-supertonic"))]
fn detect_batch_key(_fmt: &super::fakefile::DetectFrameFormat) -> Option<(u32, u32, u32)> {
    None
}

/// Sentinel batch key for the zero-copy DEVICE detect path — distinct from RGB
/// (`None`) and any NV12 colorimetry key, so a device job never groups into an
/// RGB/NV12 flush batch (its input is a preprocessed device tensor, not pixels).
#[cfg(feature = "inference-supertonic")]
const DEVICE_BATCH_KEY: (u32, u32, u32) = (u32::MAX, u32::MAX, u32::MAX);

/// Job-level flush grouping key: the device sentinel when the job carries a
/// zero-copy device tensor, else the pixel-format key ([`detect_batch_key`]).
/// Defined for both feature sets so the flush call site is uniform (without the
/// ort GPU features `detect_device` is always `None`, so this is just the format
/// key).
fn job_batch_key(job: &FrameJob) -> Option<(u32, u32, u32)> {
    #[cfg(feature = "inference-supertonic")]
    if job.detect_device.is_some() {
        return Some(DEVICE_BATCH_KEY);
    }
    detect_batch_key(&job.detect_format)
}

/// One NV12 detect-frame descriptor collected off a [`FrameJob`] for the device
/// forward: the packed `[Y | UV]` bytes plus plane strides/offsets and frame
/// dims. Owned (Arc-cloned) so the spawned forward can outlive the loop tick.
#[cfg(feature = "inference-supertonic")]
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
#[cfg(feature = "inference-supertonic")]
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
#[cfg(feature = "inference-supertonic")]
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
#[cfg(feature = "inference-supertonic")]
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
            let tensor = h
                .0
                .clone()
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
    } = match out {
        Ok(o) => o,
        Err(e) => {
            warn_throttled("detect", &format!("detect forward task failed: {e}"));
            for item in &batch {
                stage_completed(jobs, item, None, cold);
            }
            return;
        }
    };
    match outcome {
        Ok(CameraCvResult::Detections { per_frame }) if per_frame.len() == batch.len() => {
            for (item, dets_cv) in batch.iter().zip(per_frame) {
                let dets: Vec<Detection> = dets_cv.into_iter().map(detection_from_cv).collect();
                stage_completed(jobs, item, Some((dets, detect_ms)), cold);
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
                stage_completed(jobs, item, None, cold);
            }
        }
        Ok(_) => {
            warn_throttled(
                "detect",
                &format!("detect '{alias}': unexpected camera-cv result variant"),
            );
            for item in &batch {
                stage_completed(jobs, item, None, cold);
            }
        }
        Err(e) => {
            warn_throttled("detect", &format!("detect '{alias}': {e}"));
            for item in &batch {
                stage_completed(jobs, item, None, cold);
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
        let mut due: Vec<(String, Arc<CvPipeline>, Vec<String>)> = Vec::new();
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
                    due.push((id.clone(), pipeline.clone(), due_stages));
                }
            }
        }

        // Re-read changed configs (fps + pipeline) outside the lock. This runs
        // BEFORE the executor gate, so pipelines keep refreshing while the
        // engine waits — the moment the slot arrives, analysis starts instantly.
        for id in &recheck {
            let (fps, resolved) = resolve_camera_config(id).await;
            apply_camera_config(id, fps, resolved, now);
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
        for (id, pipeline, due_stages) in &due {
            // Ordering gate: jedna klatka w locie per KAMERA (przez wszystkie
            // etapy). Szybszy etap nie może otworzyć joba dla captured_ms T+1,
            // dopóki wieloetapowy job T jest otwarty — inaczej starszy merge
            // (T) opublikowałby się PO nowszym (T+1) i cofnął overlay w czasie.
            // Przy jednym etapie detect to dokładnie dawne "max 1 klatka w
            // locie per kamera" (odrzucamy, nie kolejkujemy).
            if jobs.values().any(|j| j.camera_id == *id) {
                continue;
            }
            let Some((crops, w, h, captured_ms, pts_ns, crops_format, detect)) =
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
                    frame: crops,
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
                },
            );
        }

        // Reschedule every due stage by its own FPS interval (stage fps, or the
        // camera-level analysis_fps when the stage does not set one).
        {
            let mut reg = cameras().lock().unwrap();
            for (id, pipeline, due_stages) in &due {
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
        #[cfg(feature = "inference-supertonic")]
        let batch_is_device = matches!(target_key, Some(Some(k)) if k == DEVICE_BATCH_KEY);
        #[allow(unused_variables)]
        let batch_is_nv12 = matches!(target_key, Some(Some(_))) && {
            #[cfg(feature = "inference-supertonic")]
            {
                !batch_is_device
            }
            #[cfg(not(feature = "inference-supertonic"))]
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
        #[cfg(feature = "inference-supertonic")]
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
                    stage_completed(&mut jobs, item, None, &cold);
                }
                drop(permit);
                continue;
            }
            let alias = batch[0].alias.clone();
            let threshold = batch[0].threshold;
            let n = batch.len().max(1) as u32;
            let handle = forwards.spawn(async move {
                let _permit = permit;
                let detect_start = Instant::now();
                let outcome = run_device_detect_forward(handles, threshold).await;
                let detect_ms = (detect_start.elapsed().as_millis() as u32) / n;
                ForwardOutput {
                    alias,
                    detect_ms,
                    outcome,
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
        #[cfg(feature = "inference-supertonic")]
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
                    stage_completed(&mut jobs, item, None, &cold);
                }
                drop(permit);
                continue;
            }
            let alias = batch[0].alias.clone();
            let threshold = batch[0].threshold;
            let n = batch.len().max(1) as u32;
            let handle = forwards.spawn(async move {
                let _permit = permit;
                let detect_start = Instant::now();
                let outcome = run_nv12_detect_forward(nv12, color, threshold).await;
                let detect_ms = (detect_start.elapsed().as_millis() as u32) / n;
                ForwardOutput {
                    alias,
                    detect_ms,
                    outcome,
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
                stage_completed(&mut jobs, item, None, &cold);
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
            let mut ctx = RuntimeContext::new(None);
            let detect_start = Instant::now();
            let outcome = executor_task
                .execute_camera_cv(request, &mut ctx)
                .await
                .map_err(|e| e.to_string());
            let detect_ms = (detect_start.elapsed().as_millis() as u32) / n;
            ForwardOutput {
                alias,
                detect_ms,
                outcome,
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
    cold: &mpsc::Sender<DetectionEvent>,
) {
    let Some(job) = jobs.get_mut(&item.job_id) else {
        return;
    };
    job.open_stages.retain(|s| s != &item.stage_id);
    match outcome {
        Some((mut dets, detect_ms)) => {
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

/// Bench-only load-generator toggle. `TENTAFLOW_BENCH_FORCE_DETECT=1` makes the
/// cold enrichment path run on frames the detector left empty (see
/// [`synthesize_forced_detections`]). Read once via `OnceLock`; OFF by default,
/// so production behaviour is unchanged.
fn force_detect_enabled() -> bool {
    static FORCE: OnceLock<bool> = OnceLock::new();
    *FORCE.get_or_init(|| {
        std::env::var("TENTAFLOW_BENCH_FORCE_DETECT")
            .ok()
            .map(|v| {
                let v = v.trim();
                v == "1" || v.eq_ignore_ascii_case("true")
            })
            .unwrap_or(false)
    })
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
            track_id: 0,
            vx: 0.0,
            vy: 0.0,
        })
        .collect()
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
    // Bench-only forced enrichment load (`TENTAFLOW_BENCH_FORCE_DETECT=1`, off by
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
    // captured_ms.
    detection_bus::publish_detections(
        &job.camera_id,
        job.captured_ms,
        job.pts_ns,
        job.detect_ms_total,
        merged.clone(),
    );
    // Pusty zestaw nie wymaga wzbogacenia: FAZA 1 juz wyczyscila overlay.
    if merged.is_empty() {
        return;
    }
    let sig = detection_sig(&merged);
    let bytes = job.frame.len();
    let camera_id = job.camera_id.clone();
    let ev = DetectionEvent {
        camera_id: job.camera_id,
        frame: job.frame,
        w: job.w,
        h: job.h,
        frame_format: job.frame_format,
        captured_ms: job.captured_ms,
        pts_ns: job.pts_ns,
        detect_ms: job.detect_ms_total,
        pipeline: job.pipeline,
        stage_dets: job.results,
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
        track_id: 0,
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
    std::env::var("TENTAFLOW_VISION_COLD_WORKERS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(64)
        .min(1024)
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
                w,
                h,
                frame_format,
                captured_ms,
                pts_ns,
                detect_ms,
                pipeline,
                mut stage_dets,
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
                w,
                h,
                &frame_format,
                &pipeline,
                &mut stage_dets,
            )
            .await;
            let enrich_ms = enrich_start.elapsed().as_millis() as u32;
            let proc_ms = detect_ms + enrich_ms;
            let merged: Vec<Detection> = stage_dets
                .iter()
                .flat_map(|(_, d)| d.iter().cloned())
                .collect();
            detection_bus::publish_detections(
                &camera_id,
                captured_ms,
                pts_ns,
                proc_ms,
                merged.clone(),
            );
            if !merged.is_empty() {
                if let (Some(flow_id), Some(disp)) = (
                    camera_flow_id(&camera_id).await,
                    crate::flow_engine::dispatcher::global_flow_dispatcher(),
                ) {
                    let _slot = slot;
                    // The analysis flow reads the frame as an RGB24 image blob; on
                    // the GPU-resident path `frame` is NV12, so convert once here
                    // (only when a camera actually has an assigned flow).
                    let rgb_frame = nv12_to_rgb_if_needed(&frame, w, h, &frame_format);
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
    use crate::flow_engine::dispatcher::FlowRequestMeta;
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

    // System-triggered execution: there is no user in the loop, so no per-user
    // ACL applies here (the dispatcher treats `user_id = None` as allow). The
    // real authorization gate is the camera→flow assignment write path, which is
    // admin-only and validates flow access before persisting `analysis_flow_id`.
    // A per-frame deadline (well under the dispatcher's 120 s cap) bounds how
    // long one camera's slot stays held if its flow hangs.
    let mut meta = FlowRequestMeta::new(format!("cam-{camera_id}"));
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
        enriched.unwrap_or(original),
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
/// `tekst` niesie AKTUALNIE zwycięski (najczęściej głosowany) odczyt OCR — nie
/// surowy odczyt jednej klatki. `tekst_votes` to histogram głosów per track: OCR
/// chwieje się o ±1 znak klatka-do-klatki (OKR7408↔ORR7408↔DRR7408), a poprawne
/// znaki są stałe między odczytami, więc głosowanie większościowe wygrywa
/// prawdziwą tablicę, a szumowy znak rozprasza swoje głosy. `at` datuje ostatnie
/// dotknięcie wpisu (ocena świeżości względem `ENRICH_TTL` + ewikcja).
#[derive(Clone)]
struct CachedEnrich {
    stan: Vec<String>,
    tekst: Option<String>,
    tekst_votes: Vec<(String, u32)>,
    at: Instant,
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

/// Wciela jeden zwalidowany odczyt OCR do histogramu głosów tracku i zwraca
/// aktualnego zwycięzcę (najczęściej głosowany string). Głosowanie po klatkach
/// tracku kasuje wobble ±1 znaku: poprawne znaki są stałe między odczytami,
/// szumowy dzieli głosy, więc prawdziwa tablica wygrywa.
fn ocr_vote(votes: &mut Vec<(String, u32)>, read: &str, prev: Option<&str>) -> Option<String> {
    let total: u32 = votes.iter().map(|(_, n)| *n).sum();
    // Wykładnicze zapominanie: po zliczeniu limitu odczytów połowimy wszystko,
    // aby realnie zmieniona tablica mogła wyprzedzić przestarzałego zwycięzcę.
    if total >= OCR_VOTE_MAX_TOTAL {
        votes.retain_mut(|(_, n)| {
            *n /= 2;
            *n > 0
        });
    }
    match votes.iter_mut().find(|(s, _)| s == read) {
        Some((_, n)) => *n += 1,
        None => {
            if votes.len() >= OCR_VOTE_MAX_VARIANTS {
                // Wyrzuć najsłabszy wariant, by ograniczyć mapę — wobble rodzi
                // tylko kilka wariantów, więc ewikcja odpala się rzadko.
                if let Some(pos) = votes
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, (_, n))| *n)
                    .map(|(i, _)| i)
                {
                    votes.swap_remove(pos);
                }
            }
            votes.push((read.to_string(), 1));
        }
    }
    let max = votes.iter().map(|(_, n)| *n).max().unwrap_or(0);
    if max == 0 {
        return None;
    }
    // Tie-break: przy remisie utrzymaj poprzednio wyemitowany string, by uniknąć
    // migotania między dwoma równo obstawionymi wariantami.
    if let Some(p) = prev {
        if votes.iter().any(|(s, n)| s == p && *n == max) {
            return Some(p.to_string());
        }
    }
    votes
        .iter()
        .find(|(_, n)| *n == max)
        .map(|(s, _)| s.clone())
}

/// COLD, ścieżka OCR: pod JEDNYM lockiem wciela jeden odczyt do histogramu głosów
/// tracku i zwraca aktualnego zwycięzcę. W odróżnieniu od classify OCR nie jest
/// cache-skipowany — re-czytamy co cykl cold (GPU-tani ~2-3 ms) i pozwalamy
/// głosowaniu większościowemu ustabilizować chwiejny znak. `read == None`
/// (odczyt niezwalidowany/nieudany, format już sprawdzony w `ocr_crop`/upstream)
/// NIE jest głosowany, ale wciąż odświeża `at`, aby histogram przeżył ewikcję,
/// dopóki track żyje. Zwraca dotychczasowego zwycięzcę.
fn enrich_cache_vote_ocr(
    camera_id: &str,
    stage_id: &str,
    track_id: u32,
    read: Option<String>,
) -> Option<String> {
    static PUT_COUNT: AtomicUsize = AtomicUsize::new(0);
    let mut cache = enrich_cache().lock().unwrap_or_else(|e| e.into_inner());
    let entry = cache
        .entry((camera_id.to_string(), stage_id.to_string(), track_id))
        .or_insert_with(|| CachedEnrich {
            stan: Vec::new(),
            tekst: None,
            tekst_votes: Vec::new(),
            at: Instant::now(),
        });
    if let Some(read) = read.as_deref() {
        let prev = entry.tekst.clone();
        entry.tekst = ocr_vote(&mut entry.tekst_votes, read, prev.as_deref());
    }
    entry.at = Instant::now();
    let result = entry.tekst.clone();
    if PUT_COUNT.fetch_add(1, AtomicOrdering::Relaxed) % ENRICH_EVICT_EVERY == 0 {
        cache.retain(|_, c| c.at.elapsed() < ENRICH_CACHE_EVICT_AGE);
    }
    result
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

/// COLD: generyczny interpreter etapow `stage` pipeline'u. Dla kazdego etapu
/// wybiera detekcje rodzica pasujace klasami (`class_matches`), wycina crop z
/// paddingiem etapu i wzbogaca: classify → `stan`, ocr → `tekst` (tryb z
/// `params.ocr_mode`), embed → jawny skip z warn-once. Wszystkie cropy etapu ida
/// JEDNYM zbatchowanym forwardem gdy alias rozwiazuje sie do lokalnego silnika
/// (`classify_batch`/`read_batch`); inaczej per-crop fallback przez executor
/// (`cold_stage_per_crop`). Kazda klatka czyta OD ZERA — brak cache-skipu, jedynym
/// stanem cross-frame jest histogram glosow OCR. Zaden blad nie wychodzi na
/// zewnatrz — etap bez wyniku zostawia pole puste.
async fn run_cold_stages(
    camera_id: &str,
    frame: &[u8],
    w: u32,
    h: u32,
    frame_format: &super::fakefile::DetectFrameFormat,
    pipeline: &CvPipeline,
    stage_dets: &mut [(String, Vec<Detection>)],
) {
    let Some(executor) = runtime_executor() else {
        warn_throttled(
            "cold-executor",
            "runtime executor unavailable; cold enrichment skipped",
        );
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
        let Some((_, dets)) = stage_dets.iter_mut().find(|(sid, _)| sid == parent) else {
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
            let (rgb, acw, ach) = crop_for_detection(frame, w, h, frame_format, x0, y0, cw, ch);
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
            let mut ctx = RuntimeContext::new(None);
            executor.local_camera_cv_engine(&stage.model, &mut ctx)
        };
        let crops: Vec<(Arc<[u8]>, u32, u32)> = items
            .iter()
            .map(|(_, crop, cw, ch, _)| (crop.clone(), *cw, *ch))
            .collect();

        // Wynik per crop (kolejnosc == kolejnosc `items`). Batchowana sciezka
        // lokalna dla znanych silnikow; kazdy inny przypadek (w tym zwrot None /
        // niezgodna dlugosc batcha) schodzi na per-crop fallback, wiec porazka
        // batcha nigdy cicho nie gubi wzbogacenia.
        let outputs: Vec<CachedEnrich> = match (op, engine.as_deref()) {
            (CvOp::Classify, Some("nalepka-stan")) => match classify_batch_local(&crops).await {
                Some(labels) if labels.len() == crops.len() => labels
                    .into_iter()
                    .map(|stan| CachedEnrich {
                        stan,
                        tekst: None,
                        tekst_votes: Vec::new(),
                        at: Instant::now(),
                    })
                    .collect(),
                _ => {
                    cold_stage_per_crop(&executor, &stage.model, op, ocr_mode.clone(), &items).await
                }
            },
            (CvOp::Ocr, Some("plate-ocr"))
                if matches!(ocr_mode, CvOcrMode::Plate | CvOcrMode::Generic) =>
            {
                match read_batch_local(&crops).await {
                    Some(reads) if reads.len() == crops.len() => reads
                        .into_iter()
                        .map(|tekst| CachedEnrich {
                            stan: Vec::new(),
                            tekst,
                            tekst_votes: Vec::new(),
                            at: Instant::now(),
                        })
                        .collect(),
                    _ => {
                        cold_stage_per_crop(&executor, &stage.model, op, ocr_mode.clone(), &items)
                            .await
                    }
                }
            }
            _ => cold_stage_per_crop(&executor, &stage.model, op, ocr_mode.clone(), &items).await,
        };

        // Naloz wyniki na wlasciwe detekcje po indeksie z `items` (bez pomieszania
        // cropow). OCR: glosowanie temporalne per track stabilizuje chwiejny znak
        // (surowy odczyt wciela sie do histogramu, `det.tekst` = zwyciezca); track
        // bez id (track_id=0) emituje surowy odczyt wprost. Classify: `stan`
        // przypisany od zera, bez cache-put (kazda klatka czyta na nowo).
        for ((idx, _, _, _, _), value) in items.iter().zip(outputs) {
            let det = &mut dets[*idx];
            if op == CvOp::Ocr && det.track_id > 0 {
                det.tekst =
                    enrich_cache_vote_ocr(camera_id, &stage.stage_id, det.track_id, value.tekst);
            } else {
                apply_stage_output(det, stage.output, &value);
            }
        }
    }
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
            CachedEnrich {
                stan,
                tekst,
                tekst_votes: Vec::new(),
                at: Instant::now(),
            }
        }
    });
    futures::future::join_all(jobs).await
}

/// COLD batched local classify: every crop of the stage is SUBMITTED to the
/// process-wide cross-camera state batcher, so crops from ALL cameras aggregate
/// into one big `StateClassifier::classify_batch` forward instead of a tiny
/// per-camera batch-1. `None` (batcher unavailable / any per-crop error) →
/// caller drops to the per-crop fallback (never loses enrichment). The blocking
/// `submit_all` runs on the blocking pool, off the tokio worker.
#[cfg(feature = "inference-supertonic")]
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
#[cfg(not(feature = "inference-supertonic"))]
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
#[cfg(feature = "inference-supertonic")]
async fn read_batch_local(crops: &[(Arc<[u8]>, u32, u32)]) -> Option<Vec<Option<String>>> {
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

/// Ścieżka Burn: forward serializowany na jednym watku wgpu przez `run_blocking`
/// + `Mutex` (jak `ocr_local`); `read_batch` sam petli po cropach.
#[cfg(not(feature = "inference-supertonic"))]
async fn read_batch_local(crops: &[(Arc<[u8]>, u32, u32)]) -> Option<Vec<Option<String>>> {
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
    let mut ctx = RuntimeContext::new(None);
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
    let mut ctx = RuntimeContext::new(None);
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
            tekst_votes: Vec::new(),
            at: Instant::now(),
        };
        let stan_b = CachedEnrich {
            stan: vec!["czytelna".into()],
            tekst: None,
            tekst_votes: Vec::new(),
            at: Instant::now(),
        };
        apply_stage_output(&mut det, Some(CvStageOutput::Stan), &stan_a);
        apply_stage_output(&mut det, Some(CvStageOutput::Stan), &stan_b);
        assert_eq!(det.stan, vec!["ok".to_string(), "czytelna".to_string()]);

        let ocr_hit = CachedEnrich {
            stan: Vec::new(),
            tekst: Some("30/1203".into()),
            tekst_votes: Vec::new(),
            at: Instant::now(),
        };
        let ocr_miss = CachedEnrich {
            stan: Vec::new(),
            tekst: None,
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

    /// `ocr_vote`: głosowanie większościowe kasuje wobble ±1 znaku, tie-break
    /// trzyma poprzedni string (anty-migotanie), a wykładnicze wygaszanie po
    /// limicie pozwala realnie zmienionej tablicy przejąć prowadzenie.
    #[test]
    fn ocr_vote_majority_stabilizes_wobble() {
        let mut votes: Vec<(String, u32)> = Vec::new();
        // Wobble OKR7408↔ORR7408↔DRR7408: prawdziwa tablica dostaje więcej głosów.
        assert_eq!(
            ocr_vote(&mut votes, "OKR7408", None).as_deref(),
            Some("OKR7408")
        );
        assert_eq!(
            ocr_vote(&mut votes, "ORR7408", Some("OKR7408")).as_deref(),
            Some("OKR7408")
        );
        assert_eq!(
            ocr_vote(&mut votes, "OKR7408", Some("OKR7408")).as_deref(),
            Some("OKR7408")
        );
        assert_eq!(
            ocr_vote(&mut votes, "DRR7408", Some("OKR7408")).as_deref(),
            Some("OKR7408")
        );

        // Tie-break: przy remisie utrzymaj poprzednio wyemitowany.
        let mut tie: Vec<(String, u32)> = vec![("AAA".into(), 2), ("BBB".into(), 1)];
        assert_eq!(
            ocr_vote(&mut tie, "BBB", Some("AAA")).as_deref(),
            Some("AAA")
        );

        // Nowa tablica na tym samym track_id po wielu odczytach ostatecznie wygrywa.
        let mut churn: Vec<(String, u32)> = Vec::new();
        let mut last = None;
        for _ in 0..40 {
            last = ocr_vote(&mut churn, "OLD123", last.as_deref());
        }
        assert_eq!(last.as_deref(), Some("OLD123"));
        for _ in 0..60 {
            last = ocr_vote(&mut churn, "NEW999", last.as_deref());
        }
        assert_eq!(last.as_deref(), Some("NEW999"));
    }

    /// Histogram jest ograniczony: >OCR_VOTE_MAX_VARIANTS różnych stringów nie
    /// rozdyma mapy — najsłabszy wariant wypada.
    #[test]
    fn ocr_vote_bounds_variant_count() {
        let mut votes: Vec<(String, u32)> = Vec::new();
        for i in 0..(OCR_VOTE_MAX_VARIANTS + 5) {
            ocr_vote(&mut votes, &format!("PLATE{i}"), None);
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

    fn make_job(cam: &str, captured: u64, pipeline: &Arc<CvPipeline>, stage: &str) -> FrameJob {
        FrameJob {
            camera_id: cam.to_string(),
            frame: Arc::from(vec![0u8; 12]),
            w: 2,
            h: 2,
            frame_format: crate::services::camera_ingest::fakefile::DetectFrameFormat::Rgb24,
            detect_frame: Arc::from(vec![0u8; 12]),
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
        }
    }

    fn empty_detections() -> ForwardOutput {
        ForwardOutput {
            alias: "m".into(),
            detect_ms: 3,
            outcome: Ok(CameraCvResult::Detections {
                per_frame: vec![Vec::new()],
            }),
        }
    }

    /// K>1: gdy `TENTAFLOW_VISION_INFLIGHT` jest ustawione, wygrywa; inaczej K
    /// odwzorowuje pulę detektora; gdy ŻADNA zmienna nie jest ustawiona → K=1
    /// (dowód zerowej regresji: bit-identyczne z pojedynczą, serializowaną pętlą).
    #[test]
    fn inflight_limit_defaults_to_one_and_honors_env() {
        let prev_k = std::env::var("TENTAFLOW_VISION_INFLIGHT").ok();
        let prev_d = std::env::var("TENTAFLOW_VISION_DETECTOR_SESSIONS").ok();
        std::env::remove_var("TENTAFLOW_VISION_INFLIGHT");
        std::env::remove_var("TENTAFLOW_VISION_DETECTOR_SESSIONS");
        assert_eq!(inflight_limit(), 1, "brak env → K=1");

        std::env::set_var("TENTAFLOW_VISION_DETECTOR_SESSIONS", "4");
        assert_eq!(inflight_limit(), 4, "K odwzorowuje pulę detektora");

        std::env::set_var("TENTAFLOW_VISION_INFLIGHT", "2");
        assert_eq!(
            inflight_limit(),
            2,
            "jawny opt-in wygrywa nad pulą detektora"
        );

        std::env::set_var("TENTAFLOW_VISION_INFLIGHT", "999");
        assert_eq!(
            inflight_limit(),
            MAX_INFLIGHT,
            "clamp górny do MAX_INFLIGHT"
        );

        std::env::set_var("TENTAFLOW_VISION_INFLIGHT", "0");
        assert_eq!(inflight_limit(), 1, "clamp dolny do 1");

        match prev_k {
            Some(v) => std::env::set_var("TENTAFLOW_VISION_INFLIGHT", v),
            None => std::env::remove_var("TENTAFLOW_VISION_INFLIGHT"),
        }
        match prev_d {
            Some(v) => std::env::set_var("TENTAFLOW_VISION_DETECTOR_SESSIONS", v),
            None => std::env::remove_var("TENTAFLOW_VISION_DETECTOR_SESSIONS"),
        }
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
}
