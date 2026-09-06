// =============================================================================
// File: bus/replication/metrics.rs — M2 leader-side replication gauges
// (PLAN-M2 §1b item 5)
// =============================================================================
//
// Plain atomics, instance-owned (not a global `static`) — matching this
// crate's existing metrics shape (`bus/dedup.rs`'s cache counters) rather
// than introducing a process-wide singleton. `manager.rs` (agent EL) is
// expected to own one `Arc<LeaderMetrics>` per node and hand a clone to
// every `PartitionLeader` it constructs, so every partition this node
// leads reports into the same set of gauges — that is also why every
// counter here is node-wide ("the worst/most recent value across every
// partition this node leads"), not per-partition: a per-partition split
// belongs to `ReplicationSnapshot`/`PartitionReplicaInfo`
// (`bus/mod.rs`, agent EL/S), not to this file.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

/// Number of recent `await_acks` latencies kept for the `ack_wait_p99_us`
/// estimate — a small fixed-size ring buffer ("HDR-lite": PLAN-M2 §1b),
/// not a real HDR histogram, since a few hundred recent samples are
/// already a stable-enough p99 estimate for a UI gauge and pulling in the
/// `hdrhistogram` crate for this one metric would be scope creep.
const ACK_WAIT_RESERVOIR_CAP: usize = 1024;

/// Point-in-time read of every `LeaderMetrics` gauge (PLAN-M2 §1b item 5),
/// for the M06 UI / Zabbix export to consume without touching atomics
/// directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LeaderMetricsSnapshot {
    /// Smallest ISR size (this node's own leadership included) observed
    /// across every `reconcile`/ack since this `LeaderMetrics` was
    /// created. `0` means no partition has reported an ISR size yet.
    pub isr_size_min: u32,
    pub isr_shrink_total: u64,
    /// Highest leader epoch any `PartitionLeader` sharing this
    /// `LeaderMetrics` has been constructed with.
    pub leader_epoch_max: u32,
    /// Worst (largest) per-follower unacknowledged-bytes lag observed.
    pub replication_lag_bytes_max: u64,
    pub failover_total: u64,
    /// p99 of recent `await_acks` latencies, microseconds. `0` if no
    /// sample has been recorded yet.
    pub ack_wait_p99_us: u64,
}

/// A small fixed-capacity ring buffer of recent latency samples
/// (microseconds), guarded by the same lock `LeaderMetrics::record_ack_wait`
/// and `snapshot` both take — contention is a non-issue here (one push per
/// `await_acks` return, one read per UI/Zabbix poll, both far off any hot
/// path).
struct AckWaitReservoir {
    samples: Vec<u64>,
    next: usize,
}

impl AckWaitReservoir {
    fn new() -> Self {
        Self {
            samples: Vec::with_capacity(ACK_WAIT_RESERVOIR_CAP),
            next: 0,
        }
    }

    fn push(&mut self, micros: u64) {
        if self.samples.len() < ACK_WAIT_RESERVOIR_CAP {
            self.samples.push(micros);
        } else {
            self.samples[self.next] = micros;
            self.next = (self.next + 1) % ACK_WAIT_RESERVOIR_CAP;
        }
    }

    /// Nearest-rank p99 over the samples currently held. `0` when empty.
    fn p99(&self) -> u64 {
        if self.samples.is_empty() {
            return 0;
        }
        let mut sorted = self.samples.clone();
        sorted.sort_unstable();
        // Nearest-rank method: ceil(0.99 * n) - 1, clamped into range.
        let rank = ((sorted.len() as f64) * 0.99).ceil() as usize;
        let idx = rank.saturating_sub(1).min(sorted.len() - 1);
        sorted[idx]
    }
}

/// Leader-side replication gauges (PLAN-M2 §1b item 5): `isr_size_min`,
/// `isr_shrink_total`, `leader_epoch_max`, `replication_lag_bytes_max`,
/// `failover_total`, `ack_wait_p99_us`. Shared (`Arc`) across every
/// `PartitionLeader` on this node.
pub struct LeaderMetrics {
    isr_size_min: AtomicU32,
    isr_shrink_total: AtomicU64,
    leader_epoch_max: AtomicU32,
    replication_lag_bytes_max: AtomicU64,
    failover_total: AtomicU64,
    ack_wait: parking_lot::Mutex<AckWaitReservoir>,
}

impl Default for LeaderMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl LeaderMetrics {
    pub fn new() -> Self {
        Self {
            // `u32::MAX` sentinel for "nothing recorded yet" — `snapshot`
            // maps it back to `0` rather than exposing the sentinel, so a
            // UI/Zabbix consumer never has to special-case it.
            isr_size_min: AtomicU32::new(u32::MAX),
            isr_shrink_total: AtomicU64::new(0),
            leader_epoch_max: AtomicU32::new(0),
            replication_lag_bytes_max: AtomicU64::new(0),
            failover_total: AtomicU64::new(0),
            ack_wait: parking_lot::Mutex::new(AckWaitReservoir::new()),
        }
    }

    /// Records one ISR-size observation (this node's own leadership
    /// counted in). Called on every `PartitionLeader::reconcile_follower`
    /// and ack, so `isr_size_min` reflects the worst point any partition
    /// on this node has been at, not just the moments an ISR actually
    /// changed.
    pub fn record_isr_size(&self, isr_size: u32) {
        self.isr_size_min.fetch_min(isr_size, Ordering::AcqRel);
    }

    pub fn record_isr_shrink(&self) {
        self.isr_shrink_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_leader_epoch(&self, epoch: u32) {
        self.leader_epoch_max.fetch_max(epoch, Ordering::AcqRel);
    }

    pub fn record_lag_bytes(&self, lag_bytes: u64) {
        self.replication_lag_bytes_max
            .fetch_max(lag_bytes, Ordering::AcqRel);
    }

    pub fn record_failover(&self) {
        self.failover_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_ack_wait(&self, elapsed: Duration) {
        let micros = elapsed.as_micros().min(u64::MAX as u128) as u64;
        self.ack_wait.lock().push(micros);
    }

    pub fn snapshot(&self) -> LeaderMetricsSnapshot {
        let isr_size_min = match self.isr_size_min.load(Ordering::Acquire) {
            u32::MAX => 0,
            v => v,
        };
        LeaderMetricsSnapshot {
            isr_size_min,
            isr_shrink_total: self.isr_shrink_total.load(Ordering::Relaxed),
            leader_epoch_max: self.leader_epoch_max.load(Ordering::Acquire),
            replication_lag_bytes_max: self.replication_lag_bytes_max.load(Ordering::Acquire),
            failover_total: self.failover_total.load(Ordering::Relaxed),
            ack_wait_p99_us: self.ack_wait.lock().p99(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_is_all_zero_before_anything_is_recorded() {
        let m = LeaderMetrics::new();
        let s = m.snapshot();
        assert_eq!(s, LeaderMetricsSnapshot::default());
    }

    #[test]
    fn isr_size_min_tracks_the_lowest_observed_value() {
        let m = LeaderMetrics::new();
        m.record_isr_size(3);
        m.record_isr_size(1);
        m.record_isr_size(2);
        assert_eq!(m.snapshot().isr_size_min, 1);
    }

    #[test]
    fn leader_epoch_max_and_lag_bytes_max_track_the_highest_observed_value() {
        let m = LeaderMetrics::new();
        m.record_leader_epoch(2);
        m.record_leader_epoch(5);
        m.record_leader_epoch(4);
        assert_eq!(m.snapshot().leader_epoch_max, 5);

        m.record_lag_bytes(10);
        m.record_lag_bytes(999);
        m.record_lag_bytes(500);
        assert_eq!(m.snapshot().replication_lag_bytes_max, 999);
    }

    #[test]
    fn shrink_and_failover_counters_are_cumulative() {
        let m = LeaderMetrics::new();
        for _ in 0..3 {
            m.record_isr_shrink();
        }
        m.record_failover();
        m.record_failover();
        let s = m.snapshot();
        assert_eq!(s.isr_shrink_total, 3);
        assert_eq!(s.failover_total, 2);
    }

    #[test]
    fn ack_wait_p99_reflects_recorded_samples() {
        let m = LeaderMetrics::new();
        // 100 samples, 1..=100 ms in microseconds; p99 (nearest-rank) of a
        // uniform 1..=100 population is the 99th value: 99_000us.
        for i in 1..=100u64 {
            m.record_ack_wait(Duration::from_millis(i));
        }
        assert_eq!(m.snapshot().ack_wait_p99_us, 99_000);
    }

    #[test]
    fn ack_wait_reservoir_wraps_without_growing_past_capacity() {
        let m = LeaderMetrics::new();
        for i in 0..(ACK_WAIT_RESERVOIR_CAP * 3) {
            m.record_ack_wait(Duration::from_micros(i as u64));
        }
        // Only the most recent ACK_WAIT_RESERVOIR_CAP samples survive:
        // values before the last full window are gone, so p99 is close to
        // the maximum ever pushed, not artificially low from stale data.
        let p99 = m.snapshot().ack_wait_p99_us;
        assert!(p99 > (ACK_WAIT_RESERVOIR_CAP * 2) as u64);
    }
}
