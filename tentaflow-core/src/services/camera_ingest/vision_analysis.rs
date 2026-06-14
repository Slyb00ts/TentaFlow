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
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use tokio::sync::OnceCell;
use tracing::{info, warn};

use crate::services::detection_bus;
use crate::vision::classifier_stan::StateClassifier;
use crate::vision::detector_rfdetr::RfDetrDetector;

/// Analysis cadence. Starts conservative (2 fps) — always-on CV on CPU does
/// not need full frame rate for placard/label tracking.
const ANALYSIS_INTERVAL: Duration = Duration::from_millis(500);

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

/// Per-camera analysis task registry. At most one task per camera regardless of
/// how many tiles subscribe or how often a tile re-subscribes. Tasks live for
/// the process lifetime (always-on) until aborted by `drain`.
fn registry() -> &'static Mutex<HashMap<String, tokio::task::JoinHandle<()>>> {
    static REG: OnceLock<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Ensures exactly one always-on analysis task for `camera_id`. A finished /
/// aborted handle is replaced; a live one is left untouched.
pub fn ensure_analysis(camera_id: &str) {
    let mut reg = registry().lock().unwrap();
    if let Some(handle) = reg.get(camera_id) {
        if !handle.is_finished() {
            return;
        }
    }
    let handle = spawn_analysis(camera_id.to_string());
    reg.insert(camera_id.to_string(), handle);
}

/// Aborts every running analysis task. Wired into camera shutdown so the
/// ONNX session and frame-pull loops stop before GStreamer tears down.
pub fn drain() {
    let mut reg = registry().lock().unwrap();
    for (_, handle) in reg.drain() {
        handle.abort();
    }
}

fn spawn_analysis(camera_id: String) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let detector = match get_detector().await {
            Some(d) => d,
            None => {
                // Load failed earlier — nothing to run. The camera session and
                // overlay keep working; there are simply no real detections.
                return;
            }
        };
        // Optional: a missing classifier just leaves `stan` empty.
        let classifier = get_classifier().await;
        info!("[vision_analysis] starting analysis loop for {camera_id}");

        let mut ticker = tokio::time::interval(ANALYSIS_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;

            let frame =
                crate::addon::host_functions::camera::latest_frame_global(&camera_id).await;
            let (rgb, w, h) = match frame {
                Some(f) => f,
                // No frame yet (session warming up or transiently offline).
                // Keep ticking — the next frame will land.
                None => continue,
            };

            // Inference is blocking + CPU-bound; run detection then per-crop
            // state classification off the async worker, serialized across
            // cameras through the shared model mutexes. The frame buffer moves
            // into the blocking task so crops are cut from the full-res RGB.
            let detector = detector.clone();
            let classifier = classifier.clone();
            let result = tokio::task::spawn_blocking(move || {
                let mut items = {
                    let mut guard = detector.lock().unwrap();
                    guard.detect(&rgb, w, h)?
                };

                if let Some(classifier) = classifier {
                    let mut guard = classifier.lock().unwrap();
                    for det in items.iter_mut() {
                        if !wants_state(&det.klasa) {
                            continue;
                        }
                        // bbox is [x, y, w, h] normalized 0..1 → pixels, clamped.
                        let fw = w as f32;
                        let fh = h as f32;
                        let x0 = (det.bbox[0] * fw).round().clamp(0.0, fw) as u32;
                        let y0 = (det.bbox[1] * fh).round().clamp(0.0, fh) as u32;
                        let raw_cw = (det.bbox[2] * fw).round().max(0.0) as u32;
                        let raw_ch = (det.bbox[3] * fh).round().max(0.0) as u32;
                        let cw = raw_cw.min(w.saturating_sub(x0));
                        let ch = raw_ch.min(h.saturating_sub(y0));
                        if cw < 8 || ch < 8 {
                            continue;
                        }
                        let crop = crop_rgb(&rgb, w, x0, y0, cw, ch);
                        match guard.classify(&crop, cw, ch) {
                            Ok(stany) => det.stan = stany,
                            Err(e) => {
                                warn!("[vision_analysis] classify failed for {}: {e:#}", det.klasa)
                            }
                        }
                    }
                }

                anyhow::Ok(items)
            })
            .await;

            match result {
                Ok(Ok(items)) => detection_bus::publish_detections(&camera_id, items),
                Ok(Err(e)) => warn!("[vision_analysis] detect failed for {camera_id}: {e:#}"),
                Err(e) => warn!("[vision_analysis] inference task panicked for {camera_id}: {e}"),
            }
        }
    })
}
