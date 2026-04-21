// =============================================================================
// Plik: dispatch/metrics.rs
// Opis: Per-handler metryki dla WSS dispatch. Liczy wywolania, bledy, lacznie
//       czas trwania w mikrosekundach. Eksponowane przez snapshot() do
//       Prometheus exporter (api_dashboard / /metrics endpoint).
//       Wywolywane przez dispatch::dispatch() automatycznie — handlery nie
//       musza nic robic. #[observed] proc-macro tylko ustanawia marker; logika
//       liczenia zyje tutaj.
// =============================================================================

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

use parking_lot::RwLock;

// =============================================================================
// Per-variant counters
// =============================================================================

/// Metryki pojedynczego variantu MessageBody.
#[derive(Default)]
pub struct VariantMetrics {
    /// Lacznie wywolan dispatchu dla tego variantu.
    pub calls_total: AtomicU64,
    /// Wywolania zakonczone bledem (Error response).
    pub errors_total: AtomicU64,
    /// Suma czasow trwania w mikrosekundach (do obliczenia avg = sum/calls).
    pub duration_us_total: AtomicU64,
}

impl VariantMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, duration_us: u64, is_error: bool) {
        self.calls_total.fetch_add(1, Ordering::Relaxed);
        if is_error {
            self.errors_total.fetch_add(1, Ordering::Relaxed);
        }
        self.duration_us_total
            .fetch_add(duration_us, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> VariantMetricsSnapshot {
        let calls = self.calls_total.load(Ordering::Relaxed);
        let errors = self.errors_total.load(Ordering::Relaxed);
        let duration = self.duration_us_total.load(Ordering::Relaxed);
        VariantMetricsSnapshot {
            calls_total: calls,
            errors_total: errors,
            duration_us_total: duration,
            avg_duration_us: if calls == 0 { 0 } else { duration / calls },
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct VariantMetricsSnapshot {
    pub calls_total: u64,
    pub errors_total: u64,
    pub duration_us_total: u64,
    pub avg_duration_us: u64,
}

// =============================================================================
// Globalny registry metryk
// =============================================================================

static METRICS: OnceLock<RwLock<HashMap<&'static str, VariantMetrics>>> = OnceLock::new();

fn registry() -> &'static RwLock<HashMap<&'static str, VariantMetrics>> {
    METRICS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Zapisuje pomiar dla wariantu. Lazy-init wpisu jesli nie istnieje.
pub fn record(variant_name: &'static str, duration_us: u64, is_error: bool) {
    // Fast path: shared read lock
    {
        let guard = registry().read();
        if let Some(m) = guard.get(variant_name) {
            m.record(duration_us, is_error);
            return;
        }
    }
    // Slow path: insert nowy entry
    let mut guard = registry().write();
    let m = guard
        .entry(variant_name)
        .or_insert_with(VariantMetrics::new);
    m.record(duration_us, is_error);
}

/// Snapshot wszystkich metryk dla Prometheus eksportera.
/// Zwraca posortowane Vec dla determinizmu.
pub fn snapshot_all() -> Vec<(&'static str, VariantMetricsSnapshot)> {
    let guard = registry().read();
    let mut entries: Vec<_> = guard.iter().map(|(k, v)| (*k, v.snapshot())).collect();
    entries.sort_by_key(|(k, _)| *k);
    entries
}

/// Zwraca snapshot pojedynczego variantu lub None.
pub fn snapshot_variant(variant_name: &str) -> Option<VariantMetricsSnapshot> {
    registry().read().get(variant_name).map(|m| m.snapshot())
}

/// Resetuje wszystkie metryki (test helper).
#[cfg(test)]
pub fn reset_all() {
    registry().write().clear();
}

// =============================================================================
// Timer helper
// =============================================================================

/// Mierzy czas miedzy `Timer::start()` a `Timer::stop()`. Auto-record w drop
/// jesli nie zawolano stop() jawnie.
pub struct Timer {
    variant_name: &'static str,
    start: Instant,
    finished: bool,
    is_error: bool,
}

impl Timer {
    pub fn start(variant_name: &'static str) -> Self {
        Self {
            variant_name,
            start: Instant::now(),
            finished: false,
            is_error: false,
        }
    }

    pub fn finish(mut self, is_error: bool) {
        self.is_error = is_error;
        self.flush();
    }

    fn flush(&mut self) {
        if self.finished {
            return;
        }
        let elapsed = self.start.elapsed().as_micros() as u64;
        record(self.variant_name, elapsed, self.is_error);
        self.finished = true;
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        // Bezpiecznik na wczesny return / panic — record bedzie z is_error=true.
        if !self.finished {
            self.is_error = true;
            self.flush();
        }
    }
}

// =============================================================================
// Prometheus text format render
// =============================================================================

/// Zwraca metryki w formacie Prometheus text exposition (HELP + TYPE + samples).
/// Pasuje do /metrics endpoint w api_dashboard.
pub fn render_prometheus() -> String {
    let mut out = String::new();
    let snap = snapshot_all();

    out.push_str(
        "# HELP tentaflow_ws_handler_calls_total Total dispatch calls per MessageBody variant.\n",
    );
    out.push_str("# TYPE tentaflow_ws_handler_calls_total counter\n");
    for (name, s) in &snap {
        out.push_str(&format!(
            "tentaflow_ws_handler_calls_total{{variant=\"{}\"}} {}\n",
            name, s.calls_total
        ));
    }

    out.push_str(
        "# HELP tentaflow_ws_handler_errors_total Total dispatch errors per MessageBody variant.\n",
    );
    out.push_str("# TYPE tentaflow_ws_handler_errors_total counter\n");
    for (name, s) in &snap {
        out.push_str(&format!(
            "tentaflow_ws_handler_errors_total{{variant=\"{}\"}} {}\n",
            name, s.errors_total
        ));
    }

    out.push_str(
        "# HELP tentaflow_ws_handler_duration_us_avg Average handler duration in microseconds.\n",
    );
    out.push_str("# TYPE tentaflow_ws_handler_duration_us_avg gauge\n");
    for (name, s) in &snap {
        out.push_str(&format!(
            "tentaflow_ws_handler_duration_us_avg{{variant=\"{}\"}} {}\n",
            name, s.avg_duration_us
        ));
    }

    out
}

// =============================================================================
// Testy
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_increments_counters() {
        // Uzywamy unikalnego variant_name zeby nie kolidowac z innymi testami
        // (registry jest globalny w obrebie procesu testowego).
        let variant = "TestRecordVariantA";
        record(variant, 1500, false);
        record(variant, 2500, false);
        record(variant, 500, true);

        let s = snapshot_variant(variant).unwrap();
        assert_eq!(s.calls_total, 3);
        assert_eq!(s.errors_total, 1);
        assert_eq!(s.duration_us_total, 4500);
        assert_eq!(s.avg_duration_us, 1500);
    }

    #[test]
    fn snapshot_unknown_variant_returns_none() {
        assert!(snapshot_variant("never-recorded-variant").is_none());
    }

    #[test]
    fn timer_records_on_finish() {
        let variant = "TestTimerFinish";
        let timer = Timer::start(variant);
        std::thread::sleep(std::time::Duration::from_micros(100));
        timer.finish(false);

        let s = snapshot_variant(variant).unwrap();
        assert_eq!(s.calls_total, 1);
        assert_eq!(s.errors_total, 0);
        assert!(s.duration_us_total > 0);
    }

    #[test]
    fn timer_records_on_drop_as_error() {
        let variant = "TestTimerDropAsError";
        {
            let _timer = Timer::start(variant);
            // bez finish() — drop jest zaliczony jako error (panic-safe).
        }
        let s = snapshot_variant(variant).unwrap();
        assert_eq!(s.calls_total, 1);
        assert_eq!(s.errors_total, 1);
    }

    #[test]
    fn render_prometheus_includes_all_three_metric_families() {
        let variant = "TestPrometheusRender";
        record(variant, 1000, false);
        let text = render_prometheus();
        assert!(text.contains("tentaflow_ws_handler_calls_total"));
        assert!(text.contains("tentaflow_ws_handler_errors_total"));
        assert!(text.contains("tentaflow_ws_handler_duration_us_avg"));
        assert!(text.contains(variant));
    }
}
