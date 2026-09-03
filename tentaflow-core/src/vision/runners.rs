// =============================================================================
// File: vision/runners.rs — process-wide model runner singletons
// =============================================================================
//
// Detector (RF-DETR), vehicle detector (YOLOv8), state classifier and plate OCR
// are loaded once per process and shared by every consumer: the camera analysis
// engine, the flow-engine vision node, the embedded local-CV handler and the
// inference batcher. They live here — not under `services::camera_ingest` —
// because only the first of those consumers is camera-bound, while this module
// is compiled in every build (`vision` is not behind the `camera` feature).
//
// Each runner sits in a `tokio::sync::OnceCell`: a slow load (hundreds of ms)
// must not block the async runtime, and a failed load resolves to `None` for
// the process lifetime instead of poisoning the cell — the caller degrades
// (skips `stan`/`tekst`, publishes detections) rather than crashing.

use std::sync::Mutex;

use tokio::sync::OnceCell;
use tracing::warn;

use crate::vision::classifier_stan::StateClassifier;
use crate::vision::detector_rfdetr::RfDetrDetector;
use crate::vision::ocr_plate::PlateOcr;

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
                    warn!("[vision::runners] RF-DETR load failed, analysis disabled: {e:#}");
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
                    warn!("[vision::runners] state classifier load failed, stan skipped: {e:#}");
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
                    warn!("[vision::runners] plate OCR load failed, tekst skipped: {e:#}");
                    None
                }
            })
            .await
            .unwrap_or(None)
        })
        .await
        .clone()
}
