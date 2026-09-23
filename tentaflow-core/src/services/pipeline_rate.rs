// =============================================================================
// File: services/pipeline_rate.rs — per-stage throughput of live sensor paths.
// Purpose: when a robot's LiDAR shows 3 fps instead of 7, the only useful
// question is WHERE the frames go missing: at the radio, in the addon tick, on
// publish, or on the way to the browser. Each stage notes its events here and
// one line per stage and robot is logged every window, so the answer is in the
// log instead of in a guess.
//
// Deliberately a log, not a metric: the people reading it are debugging one
// robot on one node, and a Prometheus scrape interval hides exactly the burst
// pattern (two frames in one tick, then none) this exists to show.
// =============================================================================

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

const WINDOW: Duration = Duration::from_secs(10);

struct Window {
    started: Instant,
    events: u64,
    samples: u64,
    sum_us: u64,
    max_us: u64,
}

fn windows() -> &'static Mutex<HashMap<(&'static str, String), Window>> {
    static W: OnceLock<Mutex<HashMap<(&'static str, String), Window>>> = OnceLock::new();
    W.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Counts `n` events of `stage` for `key` (usually the robot's addon id).
pub fn note(stage: &'static str, key: &str, n: u64) {
    record(stage, key, n, None);
}

/// Counts one event of `stage` for `key` that took `took`.
pub fn note_duration(stage: &'static str, key: &str, took: Duration) {
    record(stage, key, 1, Some(took.as_micros() as u64));
}

fn record(stage: &'static str, key: &str, n: u64, took_us: Option<u64>) {
    let mut map = windows().lock();
    let now = Instant::now();
    let w = map
        .entry((stage, key.to_string()))
        .or_insert_with(|| Window {
            started: now,
            events: 0,
            samples: 0,
            sum_us: 0,
            max_us: 0,
        });
    w.events += n;
    if let Some(us) = took_us {
        w.samples += 1;
        w.sum_us += us;
        w.max_us = w.max_us.max(us);
    }
    let elapsed = now.duration_since(w.started);
    if elapsed < WINDOW {
        return;
    }
    let secs = elapsed.as_secs_f64();
    if w.samples > 0 {
        tracing::info!(
            stage,
            key,
            per_sec = format!("{:.1}", w.events as f64 / secs),
            mean_ms = format!("{:.2}", w.sum_us as f64 / w.samples as f64 / 1000.0),
            max_ms = format!("{:.2}", w.max_us as f64 / 1000.0),
            "pipeline rate"
        );
    } else {
        tracing::info!(
            stage,
            key,
            per_sec = format!("{:.1}", w.events as f64 / secs),
            "pipeline rate"
        );
    }
    *w = Window {
        started: now,
        events: 0,
        samples: 0,
        sum_us: 0,
        max_us: 0,
    };
}

/// Times the scope it lives in and records it under `stage` on drop, so a
/// function with several early returns is measured on every one of them.
pub struct Timer {
    stage: &'static str,
    key: String,
    started: Instant,
}

pub fn timer(stage: &'static str, key: &str) -> Timer {
    Timer {
        stage,
        key: key.to_string(),
        started: Instant::now(),
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        note_duration(self.stage, &self.key, self.started.elapsed());
    }
}
