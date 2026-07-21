// ===== File: stage_metrics.rs — per-camera analysis stage timings for the ingest log =====
//
// `detect_ms` and `enrich_ms` are measured on every analysed frame in
// `vision_analysis` but were never surfaced anywhere: no log line, no health
// field, so the only visible symptom of a slow stage was the frame rate sagging.
// The RTSP session owns the periodic per-camera metrics line but runs in a
// different task, so the two meet here.
//
// Deliberately lossy: a running sum plus a sample count per camera, drained by the
// reader. Losing a sample to a race costs nothing — this feeds an operator log
// line, not billing.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

/// Accumulated stage timings for one camera since the last drain.
#[derive(Default)]
struct CameraStages {
    detect_ms_sum: AtomicU64,
    enrich_ms_sum: AtomicU64,
    samples: AtomicU64,
}

type Registry = RwLock<HashMap<String, Arc<CameraStages>>>;

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

fn entry(camera_id: &str) -> Arc<CameraStages> {
    if let Some(e) = registry().read().unwrap().get(camera_id) {
        return e.clone();
    }
    registry()
        .write()
        .unwrap()
        .entry(camera_id.to_string())
        .or_default()
        .clone()
}

/// Record one analysed frame's stage timings. Called from the cold analysis stage.
pub fn record(camera_id: &str, detect_ms: u32, enrich_ms: u32) {
    let e = entry(camera_id);
    e.detect_ms_sum
        .fetch_add(detect_ms as u64, Ordering::Relaxed);
    e.enrich_ms_sum
        .fetch_add(enrich_ms as u64, Ordering::Relaxed);
    e.samples.fetch_add(1, Ordering::Relaxed);
}

/// Mean detect/enrich milliseconds since the previous drain, and the sample count
/// they were averaged over. `None` when no frame was analysed in the window — the
/// caller then knows the analysis path is idle rather than fast.
pub fn drain_mean(camera_id: &str) -> Option<(u32, u32, u64)> {
    let e = entry(camera_id);
    let samples = e.samples.swap(0, Ordering::Relaxed);
    let detect = e.detect_ms_sum.swap(0, Ordering::Relaxed);
    let enrich = e.enrich_ms_sum.swap(0, Ordering::Relaxed);
    if samples == 0 {
        return None;
    }
    Some((
        (detect / samples) as u32,
        (enrich / samples) as u32,
        samples,
    ))
}

/// Drop a camera's accumulator when its session ends for good.
pub fn forget(camera_id: &str) {
    registry().write().unwrap().remove(camera_id);
}
