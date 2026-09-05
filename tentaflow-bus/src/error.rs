// ===== File: error.rs — error taxonomy for the log engine =====
//
// One enum for the whole crate (batch codec, segment I/O, index, partition
// writer) instead of per-module errors, because callers on the hot path
// (partition::append_batch) need a single `Result` type to propagate through
// the writer thread's response channel without an extra conversion layer.
//
// Every variant here is actually constructed somewhere in the crate: `pub`
// hides genuinely dead error variants from clippy's dead_code lint, so this
// is enforced by discipline, not tooling — do not add a variant nothing
// ever returns. `NotReplicaWritable` is the one deliberate exception: it
// was constructed by wave 0's `truncate_to_offset` stub and is kept,
// unconstructed, only because `bus::replication::follower` (a different
// wave-1 module in `tentaflow-core`) already matches on it against that
// frozen error surface — see its own doc.

use std::path::PathBuf;

use bytes::Bytes;

/// Errors produced by the batch codec, segment/index I/O and the partition
/// writer. Recoverable conditions (throttling, not-found) are distinguished
/// from corruption/I/O failures so callers can decide whether to retry.
#[derive(Debug, thiserror::Error)]
pub enum BusError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("batch must contain at least one record")]
    EmptyBatch,

    #[error("batch body of {len} bytes exceeds the u32 body_len wire limit")]
    BatchTooLarge { len: usize },

    #[error("record field `{field}` of {len} bytes exceeds the u32 wire limit")]
    RecordFieldTooLarge { field: &'static str, len: usize },

    #[error("record has {count} headers, exceeding the u16 header_count wire limit")]
    TooManyHeaders { count: usize },

    #[error("truncated batch: header/body needs {needed} bytes, only {available} available")]
    TruncatedBatch { needed: usize, available: usize },

    /// A record's declared field lengths (`key_len`/`header key_len`/
    /// `header val_len`/`payload_len`) run past the record's own `rec_len`
    /// boundary. Distinct from `TruncatedBatch` (which means the *buffer*
    /// ran out) — here the buffer may have plenty of bytes left, they just
    /// belong to the next record or are past EOF; slicing them in would
    /// silently hand a caller bytes that were never part of this record.
    #[error("record field `{field}` at byte {pos} claims length {len}, which runs past the record boundary at {record_end}")]
    RecordFieldOutOfBounds {
        field: &'static str,
        pos: usize,
        len: usize,
        record_end: usize,
    },

    #[error("batch CRC mismatch: header says {expected:#010x}, computed {computed:#010x}")]
    CrcMismatch { expected: u32, computed: u32 },

    #[error("unsupported batch magic/version {0:#06x}")]
    BadMagic(u16),

    #[error("unknown codec bits {0:#04x} in batch flags")]
    UnknownCodec(u16),

    #[error("lz4 (de)compression failed: {0}")]
    Compression(String),

    #[error("producer throttled, retry after {retry_after_ms} ms")]
    Throttled {
        retry_after_ms: u32,
        /// The batch handed to `append_batch`, returned unconsumed so the
        /// caller can retry without paying a clone — losing this buffer
        /// forced every retrying caller to copy their batch.
        batch: Bytes,
    },

    #[error("partition writer is closed")]
    WriterClosed,

    /// The writer thread caught an unexpected panic mid-append and shut
    /// itself down rather than risk continuing with possibly-inconsistent
    /// on-disk state. Distinct from `WriterClosed` (clean shutdown via
    /// `Drop`) so a caller can tell "this partition is permanently broken"
    /// from "this `Partition` handle was dropped".
    #[error(
        "partition writer thread panicked and poisoned the partition; it accepts no further writes"
    )]
    WriterPoisoned,

    #[error("fsync failed for segment at {path}: {message}")]
    FsyncFailed { path: PathBuf, message: String },

    #[error("RollPolicy::max_bytes ({max_bytes}) exceeds the u32 file-position wire limit")]
    RollPolicyInvalid { max_bytes: u64 },

    /// A file position or offset delta that the wire format encodes as
    /// `u32` overflowed during conversion from its `u64`/`usize` source.
    /// Reachable only if `RollPolicyInvalid` was somehow bypassed — kept as
    /// a hard error instead of a silent `as u32` truncation.
    #[error("segment position {pos} exceeds the u32 wire limit for field `{field}`")]
    PositionOverflow { field: &'static str, pos: u64 },

    /// `log_end_offset` is smaller than the active segment's own
    /// `base_offset` — an invariant that must always hold and would
    /// otherwise underflow the `u32` offset-delta computation. Turned into
    /// a hard error instead of a panicking subtraction or a silently
    /// wrapped result.
    #[error("offset chain corrupt: log_end_offset {log_end_offset} is behind active segment base_offset {segment_base_offset}")]
    OffsetChainCorrupt {
        log_end_offset: u64,
        segment_base_offset: u64,
    },

    #[error("partition directory {path} is already locked by another process/handle")]
    PartitionLocked { path: PathBuf },

    /// Guards `Partition::delete_sealed_segment` (M1 retention, PLAN §2.5):
    /// the active segment and any `base_offset` that is not a currently
    /// sealed segment must never be deleted.
    #[error("segment at base_offset {base_offset} cannot be deleted: {reason}")]
    SegmentNotDeletable {
        base_offset: u64,
        reason: &'static str,
    },

    /// `fetch_from_offset`/`fetch_from_timestamp` was asked for an offset
    /// below `earliest_offset()`: retention has already deleted the segment
    /// that would contain it. Returned explicitly instead of silently
    /// rebasing the read to the oldest surviving segment, so a consumer
    /// that fell behind retention learns about the gap instead of quietly
    /// resuming later than it asked for (PLAN §2.5, matches Kafka's
    /// `OffsetOutOfRangeException`).
    #[error(
        "requested offset {requested} is below the earliest retained offset {earliest} (latest {latest})"
    )]
    OffsetOutOfRange {
        requested: u64,
        earliest: u64,
        latest: u64,
    },

    /// `Partition::detach` was called (the owning topic/organization was
    /// deleted) and this handle's segment list has been cleared. Distinct
    /// from `WriterClosed`/`WriterPoisoned`: the process/thread is still
    /// alive and other `Partition`/`PartitionReader` handles to *other*
    /// partitions keep working — only this one directory has been
    /// permanently retired. Returned instead of a raw ENOENT so a caller
    /// racing a delete gets an unambiguous, permanent answer rather than a
    /// transient-looking I/O error it might be tempted to retry.
    #[error("partition has been detached (its topic or organization was deleted); no further reads or writes are possible")]
    PartitionDetached,

    /// A group-commit fsync failed after the group had already rolled to a
    /// new segment: the batches that landed on the segment rolled earlier
    /// in the same group are durable (`roll()` fsyncs the outgoing segment
    /// before any of this happens), but publishing their offsets was
    /// skipped to avoid the alternative — reusing those offsets for a
    /// different batch on the next append, which would leave two batches
    /// sharing one `base_offset` after a restart. Set once and permanent
    /// for this `Partition` handle's lifetime; the only way out is to
    /// reopen the directory, whose crash recovery starts from the last
    /// completed group and truncates the incomplete tail.
    #[error("partition is poisoned by a group-commit fsync failure that could not safely publish every appended offset; reopen the partition to resume writing")]
    PartitionPoisoned,

    /// `Partition::append_replicated`/`append_replicated_async` (M2,
    /// PLAN-M2 §1a): the follower's log_end_offset does not match the
    /// base_offset the leader assigned this batch. The leader and follower
    /// have diverged (a dropped/reordered frame, or the follower missed a
    /// `Truncate`) and must not silently accept a batch at the wrong
    /// position — that would either overwrite unrelated bytes or leave a
    /// gap the offset index does not know about.
    #[error("replicated append offset mismatch: expected base_offset {expected}, got {got}")]
    OffsetMismatch { expected: u64, got: u64 },

    /// A caller (leader or follower side of replication) presented a
    /// `leader_epoch` older than the one this partition already knows
    /// about. Rejected instead of silently accepted so a partitioned-away
    /// former leader that reconnects cannot resume writing over a newer
    /// leader's data.
    #[error("leader epoch is stale: this partition is at epoch {have}, got {got}")]
    LeaderEpochStale { have: u32, got: u32 },

    /// `Partition::truncate_to_offset` was asked to truncate to an offset
    /// below the partition's high watermark — that would discard records a
    /// consumer may already have read. Refused unconditionally; only the
    /// tail beyond `hw` (unacknowledged by the former leader after it
    /// returns following a failover) is ever a legal truncate target.
    #[error("cannot truncate to offset {to}: below high watermark {hw}")]
    TruncateBelowHighWatermark { hw: u64, to: u64 },

    /// Reserved for the API surface frozen in wave 0 (PLAN-M2 §1a). Used by
    /// the wave-0 stub of `Partition::truncate_to_offset` for any request
    /// at/above `hw`; that stub has been replaced by the real
    /// writer-thread truncate path (`WriterCommand::Truncate`,
    /// `segment::scan_truncate_boundary`), so this crate no longer
    /// constructs this variant itself. Kept defined (not removed) because
    /// it is part of the frozen public error surface other wave-1 modules
    /// (`bus::replication::follower`) already match on.
    #[error("this partition does not support the requested replica-write operation")]
    NotReplicaWritable,
}

impl BusError {
    /// Attaches the path a raw `std::io::Error` occurred on — every I/O call
    /// site in this crate touches a specific file, and the bare `io::Error`
    /// Display never includes it.
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        BusError::Io {
            path: path.into(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, BusError>;
