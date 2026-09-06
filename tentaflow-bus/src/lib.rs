// ===== File: lib.rs — TentaBus M0: segmented append-only log engine =====
//
// Prototype scope only (PLAN.md §5.4 "M0", §9 "M0"): the batch wire format,
// segment files with crash recovery, sparse offset/time indexes, and a
// single-writer partition with independent pull-based readers — exactly
// enough for the benchmark harness in `benches/` to measure append and read
// throughput. Everything else on the TentaBus roadmap (a `TopicLog` layer
// spanning multiple partitions, retention, consumer groups, dedup,
// RBAC/audit, replication) is explicitly out of scope until M1+ and is not
// stubbed here.

pub mod batch;
pub mod error;
pub mod index;
pub mod meta;
pub mod metrics;
pub mod partition;
pub mod segment;

pub use batch::{
    BatchBuilder, BatchHeader, BatchView, Codec, HeaderPair, RecordInput, RecordIter, RecordView,
};
pub use error::{BusError, Result};
pub use index::{OffsetEntry, OffsetIndex, TimeEntry, TimeIndex};
pub use meta::PartitionMeta;
pub use partition::{
    AppendResult, Durability, HwTracking, Partition, PartitionReader, RawBatch, SealedSegmentInfo,
    WeakPartition,
};
pub use segment::{RecoveredBatch, RollPolicy, Segment};

/// Test-only scratch directory helper shared by every module's unit tests.
/// A hand-rolled `std::env::temp_dir()` wrapper instead of the `tempfile`
/// crate, since `tempfile` is not in this crate's approved dependency list
/// (PLAN.md §2.3) and pulling it in just for tests would be scope creep.
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    pub fn temp_dir(label: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "tentaflow-bus-test-{}-{}-{}",
            std::process::id(),
            label,
            n
        ));
        std::fs::create_dir_all(&dir).expect("create test temp dir");
        dir
    }
}
