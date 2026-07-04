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
use crate::services::detection_bus;
use crate::vision::classifier_stan::StateClassifier;
use crate::vision::detector_rfdetr::{RfDetrDetector, MODEL_BATCH};
use crate::vision::ocr_plate::PlateOcr;
use crate::flow_engine::dispatchers_impl::ModelRuntimeSlot;
use crate::services::runtime::context::ExecutionContext as RuntimeContext;
use crate::services::runtime::executor::ModelRuntimeExecutor;
use crate::services::runtime::local_cv::{CameraCvOpLocal, CameraCvRequest, CvFrameLocal};
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
fn detector() -> &'static OnceCell<Option<std::sync::Arc<Mutex<RfDetrDetector>>>> {
    static DETECTOR: OnceCell<Option<std::sync::Arc<Mutex<RfDetrDetector>>>> = OnceCell::const_new();
    &DETECTOR
}

pub(crate) async fn get_detector() -> Option<std::sync::Arc<Mutex<RfDetrDetector>>> {
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

/// Handle to the process-wide classifier/OCR singletons. On the ort path
/// (`inference-supertonic`) the runner is internally pooled + `&self` + Send+Sync,
/// so it is shared bare as `Arc<_>` and every crop rides the concurrency-safe ort
/// pool off the single Burn/wgpu thread. On the Burn path the runner still needs
/// the whole-process wgpu serialization, so it stays behind `Arc<Mutex<_>>` and
/// callers funnel forwards through `burn_backend::run_blocking`.
#[cfg(feature = "inference-supertonic")]
pub(crate) type ClassifierHandle = std::sync::Arc<StateClassifier>;
#[cfg(not(feature = "inference-supertonic"))]
pub(crate) type ClassifierHandle = std::sync::Arc<Mutex<StateClassifier>>;
#[cfg(feature = "inference-supertonic")]
pub(crate) type OcrHandle = std::sync::Arc<PlateOcr>;
#[cfg(not(feature = "inference-supertonic"))]
pub(crate) type OcrHandle = std::sync::Arc<Mutex<PlateOcr>>;

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
    runtime_slot_cell().get().and_then(|s| s.read().as_ref().cloned())
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
pub fn drain() {
    cameras().lock().unwrap().clear();
    // Drain czysci wszystkie kamery naraz — wyczysc rowniez caly stan trackera,
    // by po restarcie analizy nie zostal martwy stan (tracki, licznik id).
    tracker::clear();
    if let Some(handle) = engine_handle().lock().unwrap().take() {
        handle.abort();
    }
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
    enrich_cache().lock().unwrap_or_else(|e| e.into_inner()).clear();
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
            warn_changed(slot, "no CV pipeline resolvable; camera does no analysis".to_string());
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
    info!(
        "[vision_analysis] cross-camera inference engine started (model_batch={MODEL_BATCH}, max_batch_wait={}ms)",
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

        if now.duration_since(last_metrics) >= Duration::from_secs(30) {
            let m = metrics();
            info!(
                "[vision_analysis] cold metrics: emitted={} coalesced={} drop_inflight={} drop_full={} drop_budget={} bytes_inflight={}",
                m.emitted.load(AtomicOrdering::Relaxed),
                m.coalesced.load(AtomicOrdering::Relaxed),
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
                    let cams: Vec<String> =
                        cameras().lock().unwrap().keys().cloned().collect();
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
            let Some((rgb, w, h, captured_ms, pts_ns)) =
                crate::addon::host_functions::camera::latest_frame_global(id).await
            else {
                continue;
            };
            let ident = (captured_ms, pts_ns);
            let mut stages_for_job: Vec<String> = Vec::new();
            for sid in due_stages {
                let key = (id.clone(), sid.clone());
                // Pomiń, gdy to ta sama klatka co ostatnio dołożona dla tego
                // etapu (zero duplikatów).
                let is_new = last_added.get(&key).map(|prev| *prev != ident).unwrap_or(true);
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
                    frame: rgb,
                    w,
                    h,
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
        let now_flush = Instant::now();
        let window_elapsed = pending
            .first()
            .map(|it| now_flush.duration_since(it.added) >= MAX_BATCH_WAIT)
            .unwrap_or(false);
        let keys: Vec<(&str, Option<u32>)> = pending
            .iter()
            .map(|it| (it.alias.as_str(), it.threshold.map(f32::to_bits)))
            .collect();
        let Some(indices) = cv_pipeline::select_flush_batch(&keys, MODEL_BATCH, window_elapsed)
        else {
            // Nic do policzenia w tym ticku: śpij do najbliższego z (a) deadline
            // najwcześniejszego jeszcze-niedue etapu / cfg-checku, (b) deadline
            // flushu pending (`added + MAX_BATCH_WAIT`). Clamp [IDLE_POLL_MIN,
            // IDLE_POLL_MAX] trzyma pętlę responsywną.
            let mut wait = earliest_next
                .map(|t| t.saturating_duration_since(now_flush))
                .unwrap_or(IDLE_POLL_MAX);
            if let Some(it) = pending.first() {
                wait = wait.min((it.added + MAX_BATCH_WAIT).saturating_duration_since(now_flush));
            }
            tokio::time::sleep(wait.clamp(IDLE_POLL_MIN, IDLE_POLL_MAX)).await;
            continue;
        };

        // Wyjmij wybrane pozycje (indeksy rosnące — usuwamy od końca, kolejność
        // FIFO zachowana). Nadmiar grupy zostaje w pending na kolejny flush.
        let mut batch: Vec<PendingItem> = Vec::with_capacity(indices.len());
        for &i in indices.iter().rev() {
            batch.push(pending.remove(i));
        }
        batch.reverse();

        // Klatki batcha zero-copy (klon `Arc`) z jobów. Job pozycji w pending
        // zawsze istnieje (usuwany dopiero po domknięciu wszystkich etapów).
        let mut frames: Vec<CvFrameLocal> = Vec::with_capacity(batch.len());
        for it in &batch {
            if let Some(job) = jobs.get(&it.job_id) {
                frames.push(CvFrameLocal {
                    data: job.frame.clone(),
                    width: job.w,
                    height: job.h,
                });
            }
        }
        if frames.len() != batch.len() {
            warn_throttled("detect", "pending item without a live frame job; batch dropped");
            for item in &batch {
                stage_completed(&mut jobs, item, None, &cold);
            }
            continue;
        }

        // HOT PATH: one batched detect per (alias, threshold) group through the
        // executor (resolve aliasu + failover/mesh). Serializacja forwardów:
        // `.await` blokuje pętlę aż bieżący forward się zakończy (embedded
        // kończy na `run_blocking` — jeden wątek inferencji), więc w danej
        // chwili trwa DOKŁADNIE jeden forward. Świadomie nie odpalamy
        // równoległych forwardów: współbieżny dostęp do backendu GPU powoduje
        // korupcję (patrz walidacja wgpu). `detect_ms` liczony jak dotąd:
        // łączny czas wywołania / liczba klatek batcha (przybliżenie dla badge).
        let alias = batch[0].alias.clone();
        let threshold = batch[0].threshold;
        let detect_start = Instant::now();
        let request = CameraCvRequest {
            model: alias.clone(),
            op: CameraCvOpLocal::Detect { frames, threshold },
        };
        // Wywolanie systemowe (silnik kamer) — brak tozsamosci uzytkownika,
        // swiezy kontekst per wywolanie (jak w vision_impl).
        let mut ctx = RuntimeContext::new(None);
        let result = executor.execute_camera_cv(request, &mut ctx).await;
        let n = batch.len().max(1) as u32;
        let detect_ms = (detect_start.elapsed().as_millis() as u32) / n;

        match result {
            Ok(CameraCvResult::Detections { per_frame }) if per_frame.len() == batch.len() => {
                for (item, dets_cv) in batch.iter().zip(per_frame) {
                    let dets: Vec<Detection> =
                        dets_cv.into_iter().map(detection_from_cv).collect();
                    stage_completed(&mut jobs, item, Some((dets, detect_ms)), &cold);
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
                    stage_completed(&mut jobs, item, None, &cold);
                }
            }
            Ok(_) => {
                warn_throttled(
                    "detect",
                    &format!("detect '{alias}': unexpected camera-cv result variant"),
                );
                for item in &batch {
                    stage_completed(&mut jobs, item, None, &cold);
                }
            }
            Err(e) => {
                warn_throttled("detect", &format!("detect '{alias}': {e}"));
                for item in &batch {
                    stage_completed(&mut jobs, item, None, &cold);
                }
            }
        }
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
    // FAZA 2 (cold): wzbogacenie pod dotychczasowym budzetem / backpressure.
    // Coalesce / rate-limit / byte-budget gate (rezerwuje slot).
    if admit_cold(&job.camera_id, sig, bytes).is_none() {
        return;
    }
    let camera_id = job.camera_id.clone();
    let ev = DetectionEvent {
        camera_id: job.camera_id,
        frame: job.frame,
        w: job.w,
        h: job.h,
        captured_ms: job.captured_ms,
        pts_ns: job.pts_ns,
        detect_ms: job.detect_ms_total,
        pipeline: job.pipeline,
        stage_dets: job.results,
    };
    match cold.try_send(ev) {
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
    dropped_inflight: AtomicU64,
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

/// Coalesce + rate-limit + byte-budget gate. Decides whether this frame's
/// detections become a cold event. Returns the bytes reserved (to release on
/// failure / completion) or None when the event is dropped. Per-camera ordering
/// is guaranteed by `in_flight = 1`: no second event for a camera is admitted
/// until the consumer clears the flag.
fn admit_cold(camera_id: &str, sig: u64, frame_bytes: usize) -> Option<()> {
    let now = Instant::now();
    let mut st = cold_state().lock().unwrap();
    let entry = st.entry(camera_id.to_string()).or_insert(ColdCamState {
        last_sig: u64::MAX,
        last_emit: now - COALESCE_REFRESH * 2,
        in_flight: false,
    });
    if entry.in_flight {
        // A frame arriving while this camera's previous event is still processing
        // is dropped (not queued) → overlay may show the prior scene until the
        // next frame is admitted (~one cold cycle). Acceptable for a slow ADR
        // gate; a per-camera "pending latest" replacement is a future refinement.
        metrics().dropped_inflight.fetch_add(1, AtomicOrdering::Relaxed);
        return None;
    }
    let unchanged = sig == entry.last_sig;
    if unchanged && now.duration_since(entry.last_emit) < COALESCE_REFRESH {
        metrics().coalesced.fetch_add(1, AtomicOrdering::Relaxed);
        return None;
    }
    if cold_bytes().load(AtomicOrdering::Relaxed) + frame_bytes > COLD_BYTE_BUDGET {
        metrics().dropped_budget.fetch_add(1, AtomicOrdering::Relaxed);
        return None;
    }
    // Reserve only. `last_sig`/`last_emit` are committed by `commit_cold` AFTER a
    // successful send, so a dropped (Full/Closed) event does not record its scene
    // as "recently emitted" and starve the next identical/clear frame.
    entry.in_flight = true;
    cold_bytes().fetch_add(frame_bytes, AtomicOrdering::Relaxed);
    Some(())
}

/// Records a successfully-sent event's scene signature + emit time (coalesce base).
fn commit_cold(camera_id: &str, sig: u64) {
    if let Some(slot) = cold_state().lock().unwrap().get_mut(camera_id) {
        slot.last_sig = sig;
        slot.last_emit = Instant::now();
    }
}

/// Releases a camera's in-flight slot + its reserved bytes. `saturating_sub` so a
/// release racing a `drain()` reset can never underflow the byte counter (which
/// would wrap huge and permanently reject the budget gate).
fn release_cold(camera_id: &str, frame_bytes: usize) {
    if let Some(slot) = cold_state().lock().unwrap().get_mut(camera_id) {
        slot.in_flight = false;
    }
    let _ = cold_bytes().fetch_update(AtomicOrdering::Relaxed, AtomicOrdering::Relaxed, |v| {
        Some(v.saturating_sub(frame_bytes))
    });
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
async fn cold_consumer(mut rx: mpsc::Receiver<DetectionEvent>) {
    info!("[vision_analysis] cold enrichment consumer started");
    while let Some(ev) = rx.recv().await {
        let DetectionEvent {
            camera_id,
            frame,
            w,
            h,
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
        // Czas wzbogacenia tej klatki: pelna petla etapow cold. Dla trafien
        // cache (pominiety realny forward) bedzie maly — klatka faktycznie tania.
        let enrich_start = Instant::now();
        run_cold_stages(&camera_id, &frame, w, h, &pipeline, &mut stage_dets).await;
        let enrich_ms = enrich_start.elapsed().as_millis() as u32;
        // proc_ms = calosc obrobki klatki: detekcja (FAZA 1) + etapy cold (FAZA 2).
        let proc_ms = detect_ms + enrich_ms;
        let merged: Vec<Detection> = stage_dets
            .iter()
            .flat_map(|(_, d)| d.iter().cloned())
            .collect();
        detection_bus::publish_detections(&camera_id, captured_ms, pts_ns, proc_ms, merged.clone());
        if !merged.is_empty() {
            if let (Some(flow_id), Some(disp)) = (
                camera_flow_id(&camera_id).await,
                crate::flow_engine::dispatcher::global_flow_dispatcher(),
            ) {
                // Detach: a flow can run up to its per-frame deadline, so awaiting
                // it here would head-of-line block every other camera's enrichment.
                // The spawned task owns `slot`, releasing this camera's in-flight
                // slot + bytes when the flow finishes. Per-camera in-flight = 1
                // (admit_cold) bounds a camera to one concurrent run; the byte
                // budget bounds the fleet — so detaching stays bounded.
                tokio::spawn(async move {
                    let _slot = slot;
                    run_camera_flow(
                        disp, flow_id, camera_id, frame, w, h, captured_ms, pts_ns, proc_ms,
                        merged,
                    )
                    .await;
                });
                continue;
            }
        }
        drop(slot);
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
            publish_flow_detections(&camera_id, captured_ms, pts_ns, proc_ms, detections, outcome);
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

/// Wynik jednego etapu cold dla jednego tracku: stan (classify) LUB odczyt OCR
/// (tylko pole właściwe dla `output` etapu jest niepuste), wraz z chwilą
/// policzenia (`at`) do oceny świeżości względem `ENRICH_TTL`.
#[derive(Clone)]
struct CachedEnrich {
    stan: Vec<String>,
    tekst: Option<String>,
    at: Instant,
}

/// Proces-wide cache wzbogacania kluczowany po (camera_id, stage_id, track_id) —
/// stage_id to etap COLD pipeline'u, wiec dwa etapy classify nad tym samym
/// rodzicem nie koliduja. Pozwala wzbogacic kazdy track RAZ per etap i reuzywac
/// wynik zamiast wolac model per klatka (~10x mniej forwardow).
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

/// Zapisuje wynik etapu cold dla tracku do cache i co `ENRICH_EVICT_EVERY`
/// zapisow usuwa wpisy starsze niz `ENRICH_CACHE_EVICT_AGE` (ewikcja
/// licznikowa), by mapa nie rosla po znikajacych trackach.
fn enrich_cache_put(
    camera_id: &str,
    stage_id: &str,
    track_id: u32,
    stan: Vec<String>,
    tekst: Option<String>,
) {
    static PUT_COUNT: AtomicUsize = AtomicUsize::new(0);
    let mut cache = enrich_cache().lock().unwrap_or_else(|e| e.into_inner());
    cache.insert(
        (camera_id.to_string(), stage_id.to_string(), track_id),
        CachedEnrich {
            stan,
            tekst,
            at: Instant::now(),
        },
    );
    if PUT_COUNT.fetch_add(1, AtomicOrdering::Relaxed) % ENRICH_EVICT_EVERY == 0 {
        cache.retain(|_, c| c.at.elapsed() < ENRICH_CACHE_EVICT_AGE);
    }
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

/// HOT: przypisuje detekcjom etapu `detect_stage_id` swieze wpisy cache
/// wzbogacania wszystkich etapow cold, ktore maja ten etap za rodzica — boxy
/// FAZY 1 niosa stan/tekst natychmiast, bez czekania na cold path.
fn apply_cached_enrichment(
    camera_id: &str,
    pipeline: &CvPipeline,
    detect_stage_id: &str,
    dets: &mut [Detection],
) {
    for stage in cv_pipeline::cold_stages(pipeline) {
        let CvStageInput::Stage { stage_id: parent, classes } = &stage.input else {
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
/// paddingiem etapu i wykonuje operacje przez executor (alias ETAPU):
/// classify → `stan`, ocr → `tekst` (tryb z `params.ocr_mode`), embed →
/// jawny skip z warn-once (surface CameraCv nie ma jeszcze operacji Embed).
/// Petla jest async: executor sam robi `run_blocking` per forward, wiec
/// serializacja GPU jest zachowana (jeden forward naraz). Zaden blad nie
/// wychodzi na zewnatrz — etap bez wyniku zostawia pole puste.
async fn run_cold_stages(
    camera_id: &str,
    frame: &[u8],
    w: u32,
    h: u32,
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
        let CvStageInput::Stage { stage_id: parent, classes } = &stage.input else {
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
        let (pad_x, pad_y) = cv_pipeline::crop_pads(stage);
        let Some((_, dets)) = stage_dets.iter_mut().find(|(sid, _)| sid == parent) else {
            continue;
        };
        for det in dets.iter_mut() {
            if !cv_pipeline::class_matches(classes, &det.klasa) {
                continue;
            }
            // Cache per (etap, track): swiezy wpis reuzywamy bez forwardu —
            // wzbogacamy tylko NOWE tracki albo co ENRICH_TTL (odswiezenie).
            // Detekcje z track_id=0 (brak trackingu) wzbogacamy zawsze, bez
            // cache (nie ma stabilnego klucza).
            if det.track_id > 0 {
                if let Some(c) = enrich_cache_fresh(camera_id, &stage.stage_id, det.track_id) {
                    apply_stage_output(det, stage.output, &c);
                    continue;
                }
            }
            let Some((x0, y0, cw, ch)) = padded_crop_rect(w, h, &det.bbox, pad_x, pad_y) else {
                continue;
            };
            let crop: Arc<[u8]> = Arc::from(crop_rgb(frame, w, x0, y0, cw, ch));
            let (stan, tekst) = match stage.op {
                CvOp::Classify => (
                    classify_crop(&executor, &stage.model, crop, cw, ch, &det.klasa)
                        .await
                        .unwrap_or_default(),
                    None,
                ),
                CvOp::Ocr => {
                    let mode = match cv_pipeline::ocr_mode(stage) {
                        "adr" => CvOcrMode::Adr,
                        "plate" => CvOcrMode::Plate,
                        _ => CvOcrMode::Generic,
                    };
                    (
                        Vec::new(),
                        ocr_crop(&executor, &stage.model, crop, cw, ch, mode, &det.klasa).await,
                    )
                }
                CvOp::Detect | CvOp::Embed => continue,
            };
            let value = CachedEnrich {
                stan,
                tekst,
                at: Instant::now(),
            };
            apply_stage_output(det, stage.output, &value);
            // Zapisz wynik (nawet pusty) pod (etap, track), aby hot path i
            // kolejne klatki tego tracku reuzyly go bez ponownego forwardu.
            if det.track_id > 0 {
                enrich_cache_put(
                    camera_id,
                    &stage.stage_id,
                    det.track_id,
                    value.stan,
                    value.tekst,
                );
            }
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
        assert_eq!(padded_crop_rect(100, 100, &bbox, 0.0, 0.0), Some((10, 20, 40, 30)));
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
        assert_eq!(padded_crop_rect(100, 100, &[0.0, 0.0, 0.05, 0.5], 0.0, 0.0), None);
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
            at: Instant::now(),
        };
        let stan_b = CachedEnrich {
            stan: vec!["czytelna".into()],
            tekst: None,
            at: Instant::now(),
        };
        apply_stage_output(&mut det, Some(CvStageOutput::Stan), &stan_a);
        apply_stage_output(&mut det, Some(CvStageOutput::Stan), &stan_b);
        assert_eq!(det.stan, vec!["ok".to_string(), "czytelna".to_string()]);

        let ocr_hit = CachedEnrich {
            stan: Vec::new(),
            tekst: Some("30/1203".into()),
            at: Instant::now(),
        };
        let ocr_miss = CachedEnrich {
            stan: Vec::new(),
            tekst: None,
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
}
