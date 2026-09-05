// ===== File: metrics.rs — TentaBus M2: process-wide engine latency reservoirs (PLAN §8.4) =====
//
// Bounded latency reservoirs sampled by the writer thread for the two
// hot-path timings the Zabbix exporter surfaces as p99 latencies:
// `append_us` (one batch landing in a segment) and `fsync_us` (one fsync
// call against the active segment). Kept in this crate — rather than
// tentaflow-core, which owns the exporter — so the writer thread that
// actually measures these durations can record them with a plain function
// call, no channel or trait indirection on the hot path.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Ring capacity: once a reservoir has recorded more than this many samples,
/// only the most recent `RESERVOIR_SIZE` readings are kept. Good enough for
/// a p99 estimate under a live workload, and bounds each reservoir to a
/// fixed, allocation-free block of memory.
const RESERVOIR_SIZE: usize = 1024;

/// Fixed-size ring of recent latency samples (microseconds) plus a running
/// count/sum, all `AtomicU64`/`AtomicUsize` so `record` never blocks,
/// allocates, or takes a lock.
pub struct Reservoir {
    buf: [AtomicU64; RESERVOIR_SIZE],
    next: AtomicUsize,
    count: AtomicU64,
    sum_us: AtomicU64,
}

impl Reservoir {
    const fn new() -> Self {
        // Array-repeat-expression of a `const` item is allowed for
        // non-`Copy` types (unlike repeating an arbitrary expression), so
        // this builds the ring without needing `Default`.
        const ZERO: AtomicU64 = AtomicU64::new(0);
        Reservoir {
            buf: [ZERO; RESERVOIR_SIZE],
            next: AtomicUsize::new(0),
            count: AtomicU64::new(0),
            sum_us: AtomicU64::new(0),
        }
    }

    /// Records one latency sample in microseconds. Lock-free ring write:
    /// once full, the oldest slot is overwritten.
    pub fn record(&self, value_us: u64) {
        let slot = self.next.fetch_add(1, Ordering::Relaxed) % RESERVOIR_SIZE;
        self.buf[slot].store(value_us, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
        self.sum_us.fetch_add(value_us, Ordering::Relaxed);
    }

    /// 99th percentile over the samples currently held in the ring (0 when
    /// none have been recorded yet). Copies the live slots, sorts them, and
    /// takes the p99 rank — cheap enough for an exporter's `collect` path,
    /// never called from the hot path itself.
    pub fn p99(&self) -> u64 {
        let filled = self
            .count
            .load(Ordering::Relaxed)
            .min(RESERVOIR_SIZE as u64) as usize;
        if filled == 0 {
            return 0;
        }
        let mut values: Vec<u64> = self.buf[..filled]
            .iter()
            .map(|a| a.load(Ordering::Relaxed))
            .collect();
        values.sort_unstable();
        let rank = ((filled as f64) * 0.99).ceil() as usize;
        let idx = rank.saturating_sub(1).min(filled - 1);
        values[idx]
    }

    /// Total samples ever recorded, uncapped by the ring size.
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// Sum of every sample ever recorded, in microseconds.
    pub fn sum_us(&self) -> u64 {
        self.sum_us.load(Ordering::Relaxed)
    }
}

/// Process-wide registry of engine hot-path latency reservoirs.
struct MetricsRegistry {
    append_us: Reservoir,
    fsync_us: Reservoir,
}

static REGISTRY: MetricsRegistry = MetricsRegistry {
    append_us: Reservoir::new(),
    fsync_us: Reservoir::new(),
};

/// Records one append's wall-clock duration in microseconds.
pub fn record_append_us(value_us: u64) {
    REGISTRY.append_us.record(value_us);
}

/// Records one fsync call's wall-clock duration in microseconds.
pub fn record_fsync_us(value_us: u64) {
    REGISTRY.fsync_us.record(value_us);
}

/// Current p99 append latency in microseconds (0 when no samples yet).
pub fn append_p99_us() -> u64 {
    REGISTRY.append_us.p99()
}

/// Current p99 fsync latency in microseconds (0 when no samples yet).
pub fn fsync_p99_us() -> u64 {
    REGISTRY.fsync_us.p99()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p99_of_empty_reservoir_is_zero() {
        let r = Reservoir::new();
        assert_eq!(r.p99(), 0);
        assert_eq!(r.count(), 0);
        assert_eq!(r.sum_us(), 0);
    }

    #[test]
    fn p99_reports_a_sane_high_percentile() {
        let r = Reservoir::new();
        for v in 1..=1000u64 {
            r.record(v);
        }
        assert_eq!(r.count(), 1000);
        assert_eq!(r.sum_us(), (1..=1000u64).sum::<u64>());
        let p99 = r.p99();
        // 1..=1000 recorded in order fits entirely inside the 1024-slot
        // ring, so p99 must land near the top of the range: strictly above
        // the median and at or below the maximum recorded value.
        assert!(p99 >= 980 && p99 <= 1000, "p99 = {p99}");
    }

    #[test]
    fn ring_wraps_and_keeps_only_the_most_recent_samples() {
        let r = Reservoir::new();
        // Fill well past the ring capacity with a low plateau, then a high
        // tail — p99 must reflect only the still-resident high tail, not
        // the overwritten low plateau.
        for _ in 0..(RESERVOIR_SIZE * 2) {
            r.record(1);
        }
        for v in 1..=100u64 {
            r.record(1000 + v);
        }
        assert_eq!(r.count(), (RESERVOIR_SIZE * 2 + 100) as u64);
        assert!(r.p99() >= 1000, "p99 = {}", r.p99());
    }
}
