// =============================================================================
// File: bus/retention.rs — TentaBus M1: segment-level retention (PLAN §2.5)
// =============================================================================
//
// Execution model: delete WHOLE closed segments (unlink = O(1) per segment,
// independent of record count). Never touches the active segment (the
// engine's `sealed_segments()`/`delete_sealed_segment()` already refuse
// that categorically — see `tentaflow_bus::Partition`) and never needs a
// high-watermark check of its own: M1 has no replication, so
// `high_watermark() == log_end_offset()` always, and a sealed segment's
// offsets are by definition below the active segment's base, hence below
// the watermark.
//
// Byte-budget accounting only considers SEALED segments (the deletable
// set), not the active one — the active segment's size is bounded by its
// own `RollPolicy::max_bytes` (default 256 MiB, M1-R2 decision 5 — was
// 1 GiB with preallocation on; preallocation is now off by default too)
// regardless, a small, bounded slack against a `retention_bytes` budget
// that defaults to 10 GiB/partition (PLAN §7.1).
//
// Compliance hook: `min_retention_ms` is a plain `i64` parameter here, not
// a lookup this module performs itself. PLAN §2.5 calls for a
// `RetentionScopeKind::BusTopic` variant in `compliance/models.rs` so a
// topic's effective retention floor comes from `compliance::
// resolve_retention_policy` ("polityka compliance zawsze wygrywa, gdy jest
// wyższa") — that variant DOES NOT EXIST YET and wiring it in is explicitly
// DEFERRED to compliance integration, not done as part of this module.
// Until it lands: every caller of `sweep_partition` in this crate
// (`BusService::run_retention_sweep`) passes `min_retention_ms = 0`, which
// means "no floor beyond the topic's own `retention_ms`" — a topic's
// configured retention is authoritative on its own, compliance can only
// ever RAISE it once the real resolution is wired in (never lower it, per
// `max(retention_ms, min_retention_ms)` below), so this deferral cannot
// cause a topic to be retained for LESS time than its own setting promises.

use std::path::Path;
use std::time::UNIX_EPOCH;

use tentaflow_bus::Partition;

use super::BusServiceError;

#[derive(Debug, Clone, Copy, Default)]
pub struct RetentionOutcome {
    pub deleted_segments: u32,
    pub deleted_bytes: u64,
}

/// Sweeps one partition's sealed segments, deleting the oldest ones until
/// both the effective age budget and the byte budget are satisfied.
/// `min_retention_ms` is the compliance floor (0 = no floor beyond the
/// topic's own `retention_ms`).
pub fn sweep_partition(
    partition: &Partition,
    retention_ms: i64,
    retention_bytes_per_partition: i64,
    min_retention_ms: i64,
    now_ms: i64,
) -> Result<RetentionOutcome, BusServiceError> {
    let effective_retention_ms = retention_ms.max(min_retention_ms);
    let sealed = partition.sealed_segments(); // oldest first
    let mut running_bytes: i64 = sealed.iter().map(|s| s.len as i64).sum();
    let mut outcome = RetentionOutcome::default();

    for seg in &sealed {
        let age_ms = segment_age_ms(&seg.log_path, now_ms)?;
        let over_age = effective_retention_ms >= 0 && age_ms > effective_retention_ms;
        let over_bytes = running_bytes > retention_bytes_per_partition;
        if !over_age && !over_bytes {
            // Segments are evaluated oldest-first with monotonically
            // non-increasing age and a monotonically shrinking remaining
            // byte total, so once one is within budget every newer segment
            // is too — safe to stop scanning.
            break;
        }
        partition.delete_sealed_segment(seg.base_offset)?;
        outcome.deleted_segments += 1;
        outcome.deleted_bytes += seg.len;
        running_bytes -= seg.len as i64;
    }

    Ok(outcome)
}

fn segment_age_ms(path: &Path, now_ms: i64) -> Result<i64, BusServiceError> {
    let meta = std::fs::metadata(path).map_err(|e| BusServiceError::Io(e.to_string()))?;
    let modified = meta
        .modified()
        .map_err(|e| BusServiceError::Io(e.to_string()))?;
    let modified_ms = modified
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    Ok((now_ms - modified_ms).max(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use tentaflow_bus::{BatchBuilder, Durability, RecordInput, RollPolicy};

    /// Fresh temp directory per test, removed when the returned `TempDir`
    /// is dropped. Callers must keep it alive for as long as they use the
    /// path (e.g. as long as the `Partition` opened under it is in use).
    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("create temp dir")
    }

    fn one_record_batch(payload_len: usize) -> Bytes {
        let mut b = BatchBuilder::new(0, 1);
        b.push(RecordInput::new(Bytes::from(vec![0x22; payload_len]), 0))
            .unwrap();
        b.build().unwrap()
    }

    /// Retention must never touch the active segment, even when the byte
    /// budget is already exceeded by everything written so far.
    #[test]
    fn never_deletes_the_active_segment() {
        let dir = temp_dir();
        let part = Partition::open(dir.path(), RollPolicy::default(), Durability::Os, 8).unwrap();
        for _ in 0..5 {
            part.append_batch(one_record_batch(1024)).unwrap();
        }
        let outcome = sweep_partition(&part, i64::MAX, 0, 0, 0).unwrap();
        assert_eq!(outcome.deleted_segments, 0);
        assert_eq!(part.sealed_segments().len(), 0);
    }

    /// Byte-budget eviction: oldest sealed segments go first, stopping as
    /// soon as the remaining sealed total fits the budget. Deterministic
    /// (does not depend on file mtimes/wall-clock age), unlike the
    /// age-based path.
    #[test]
    fn evicts_oldest_sealed_segments_first_to_satisfy_byte_budget() {
        let dir = temp_dir();
        let policy = RollPolicy {
            max_batches: 1,
            ..RollPolicy::default()
        };
        let part = Partition::open(dir.path(), policy, Durability::Os, 8).unwrap();
        // 5 batches -> 5 segments (4 sealed + 1 active), each the same size.
        for _ in 0..5 {
            part.append_batch(one_record_batch(1024)).unwrap();
        }
        let sealed_before = part.sealed_segments();
        assert_eq!(sealed_before.len(), 4);
        let one_segment_bytes = sealed_before[0].len as i64;

        // Budget for 1.5 segments worth of sealed data: only the newest
        // sealed segment should survive.
        let budget = one_segment_bytes + one_segment_bytes / 2;
        let outcome = sweep_partition(&part, i64::MAX, budget, 0, 0).unwrap();
        assert_eq!(outcome.deleted_segments, 3);

        let remaining = part.sealed_segments();
        assert_eq!(remaining.len(), 1);
        assert_eq!(
            remaining[0].base_offset,
            sealed_before.last().unwrap().base_offset,
            "the newest sealed segment survives"
        );
    }

    /// Age-based eviction: a fresh segment (just written) is well within a
    /// generous retention window, so nothing is deleted.
    #[test]
    fn fresh_segments_survive_a_generous_age_budget() {
        let dir = temp_dir();
        let policy = RollPolicy {
            max_batches: 1,
            ..RollPolicy::default()
        };
        let part = Partition::open(dir.path(), policy, Durability::Os, 8).unwrap();
        for _ in 0..3 {
            part.append_batch(one_record_batch(64)).unwrap();
        }
        assert_eq!(part.sealed_segments().len(), 2);
        let now_ms = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let outcome = sweep_partition(&part, 24 * 3_600_000, i64::MAX, 0, now_ms).unwrap();
        assert_eq!(outcome.deleted_segments, 0);
    }

    /// The compliance floor (`min_retention_ms`) can only raise the
    /// effective retention window, never lower it below the topic's own
    /// setting — a `retention_ms` of 0 with a nonzero floor still keeps
    /// everything younger than the floor.
    #[test]
    fn compliance_floor_raises_effective_retention() {
        let dir = temp_dir();
        let policy = RollPolicy {
            max_batches: 1,
            ..RollPolicy::default()
        };
        let part = Partition::open(dir.path(), policy, Durability::Os, 8).unwrap();
        for _ in 0..3 {
            part.append_batch(one_record_batch(64)).unwrap();
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        // retention_ms = 0 would delete everything, but a 1-day compliance
        // floor overrides it.
        let outcome = sweep_partition(&part, 0, i64::MAX, 24 * 3_600_000, now_ms).unwrap();
        assert_eq!(outcome.deleted_segments, 0);
    }
}
