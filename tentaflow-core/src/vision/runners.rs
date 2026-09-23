// =============================================================================
// File: vision/runners.rs — process-wide model runner singletons
// =============================================================================
//
// Detector (RF-DETR), COCO detector (YOLOX), face detector (YuNet), state
// classifier and plate OCR are loaded once per process and shared by every
// consumer: the camera analysis engine, the flow-engine vision node, the
// embedded local-CV handler and the inference batcher. They live here — not under `services::camera_ingest` —
// because only the first of those consumers is camera-bound, while this module
// is compiled in every build (`vision` is not behind the `camera` feature).
//
// The RF-DETR / classifier / OCR runners sit in a `tokio::sync::OnceCell`: a
// slow load (hundreds of ms) must not block the async runtime, and a failed
// load resolves to `None` for the process lifetime instead of poisoning the
// cell — the caller degrades (skips `stan`/`tekst`, publishes detections)
// rather than crashing. The COCO and face detectors use [`RetryingModel`]
// instead: their models are installed from the catalog at runtime.

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

/// A model loaded on first use that, unlike the `OnceCell` runners above,
/// RETRIES after a failed load. The privacy and vehicle models are installed
/// from the catalog while the node runs; caching a failure for the process
/// lifetime would keep privacy blind until a restart. A failure is remembered
/// for [`RETRY_AFTER`], so a node without the model pays one cheap
/// file-existence check per window instead of one per frame, and concurrent
/// callers share a single in-flight load (the async mutex) instead of racing
/// to build duplicate ort pools.
#[cfg(feature = "vision-ort")]
pub(crate) struct RetryingModel<T> {
    loaded: std::sync::OnceLock<std::sync::Arc<T>>,
    /// When the last load failed, and how many failed in a row.
    failure: Mutex<Option<(std::time::Instant, u32)>>,
    loading: tokio::sync::Mutex<()>,
    label: &'static str,
}

/// How long a failed load is remembered before the next attempt.
#[cfg(feature = "vision-ort")]
const RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(30);

#[cfg(feature = "vision-ort")]
impl<T: Send + Sync + 'static> RetryingModel<T> {
    pub(crate) const fn new(label: &'static str) -> Self {
        Self {
            loaded: std::sync::OnceLock::new(),
            failure: Mutex::new(None),
            loading: tokio::sync::Mutex::const_new(()),
            label,
        }
    }

    /// The loaded model, loading it (off the async runtime) when absent and
    /// the retry window of the last failure has passed. `None` while the model
    /// is unavailable; callers degrade and ask again later.
    pub(crate) async fn get(&self, load: fn() -> anyhow::Result<T>) -> Option<std::sync::Arc<T>> {
        if let Some(m) = self.loaded.get() {
            return Some(m.clone());
        }
        if self.in_backoff() {
            return None;
        }
        let _guard = self.loading.lock().await;
        // A caller that waited on the lock finds the previous holder's result.
        if let Some(m) = self.loaded.get() {
            return Some(m.clone());
        }
        if self.in_backoff() {
            return None;
        }
        match tokio::task::spawn_blocking(load).await {
            Ok(Ok(model)) => {
                let model = std::sync::Arc::new(model);
                let _ = self.loaded.set(model.clone());
                *self.failure.lock().unwrap_or_else(|p| p.into_inner()) = None;
                Some(model)
            }
            Ok(Err(e)) => {
                self.record_failure(&format!("{e:#}"));
                None
            }
            Err(e) => {
                self.record_failure(&format!("load task: {e}"));
                None
            }
        }
    }

    fn in_backoff(&self) -> bool {
        self.failure
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some_and(|(at, _)| at.elapsed() < RETRY_AFTER)
    }

    fn record_failure(&self, err: &str) {
        let mut failure = self.failure.lock().unwrap_or_else(|p| p.into_inner());
        let count = failure.map_or(0, |(_, n)| n) + 1;
        *failure = Some((std::time::Instant::now(), count));
        // The first failure is news; the repeats every 30 s while the model is
        // simply not installed are not.
        if count == 1 {
            warn!(
                "[vision::runners] {} load failed, retrying every {}s: {err}",
                self.label,
                RETRY_AFTER.as_secs()
            );
        } else {
            tracing::debug!(
                "[vision::runners] {} load failed ({count}x): {err}",
                self.label
            );
        }
    }
}

/// Handle to the process-wide YOLOX COCO detector — the SECOND detector run in
/// parallel with RF-DETR (vehicles, own ort session pool so a `tokio::join!` of
/// the two forwards costs ~max(DETR, YOLOX)) and the privacy probe's person
/// detector. Only the ort path builds it.
#[cfg(feature = "vision-ort")]
pub(crate) type CocoHandle = std::sync::Arc<crate::vision::detector_coco::CocoDetector>;

#[cfg(feature = "vision-ort")]
static COCO: RetryingModel<crate::vision::detector_coco::CocoDetector> =
    RetryingModel::new("YOLOX COCO detector");

/// The YOLOX COCO detector, or `None` while its model is not installed (then
/// vehicle association degrades to RF-DETR-only: every sign keeps
/// `vehicle_id = 0`). Picks up a model installed after startup.
#[cfg(feature = "vision-ort")]
pub(crate) async fn get_coco_detector() -> Option<CocoHandle> {
    COCO.get(crate::vision::detector_coco::CocoDetector::load)
        .await
}

/// Handle to the process-wide YuNet face detector (privacy probe).
#[cfg(feature = "vision-ort")]
pub(crate) type FaceHandle = std::sync::Arc<crate::vision::face_yunet::YuNetDetector>;

#[cfg(feature = "vision-ort")]
static FACE: RetryingModel<crate::vision::face_yunet::YuNetDetector> =
    RetryingModel::new("YuNet face detector");

/// The YuNet face detector, or `None` while its model is not installed. Picks
/// up a model installed after startup.
#[cfg(feature = "vision-ort")]
pub(crate) async fn get_face_detector() -> Option<FaceHandle> {
    FACE.get(crate::vision::face_yunet::YuNetDetector::load)
        .await
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
