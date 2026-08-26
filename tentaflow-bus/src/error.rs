// ===== File: error.rs — error taxonomy for the log engine =====
//
// One enum for the whole crate (batch codec, segment I/O, index, partition
// writer) instead of per-module errors, because callers on the hot path
// (partition::append_batch) need a single `Result` type to propagate through
// the writer thread's response channel without an extra conversion layer.
//
// Every variant here is actually constructed somewhere in the crate (review
// P3-1: `pub` hides genuinely dead error variants from clippy's dead_code
// lint, so this is enforced by discipline, not tooling — do not add a
// variant nothing ever returns).

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
    /// silently hand a caller bytes that were never part of this record
    /// (review P1-1).
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
        /// caller can retry without paying a clone (review P2-5: losing
        /// this buffer forced every retrying caller to copy their batch).
        batch: Bytes,
    },

    #[error("partition writer is closed")]
    WriterClosed,

    /// The writer thread caught an unexpected panic mid-append and shut
    /// itself down rather than risk continuing with possibly-inconsistent
    /// on-disk state (review P1-2). Distinct from `WriterClosed` (clean
    /// shutdown via `Drop`) so a caller can tell "this partition is
    /// permanently broken" from "this `Partition` handle was dropped".
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
    /// a hard error instead of a silent `as u32` truncation (review P2-7).
    #[error("segment position {pos} exceeds the u32 wire limit for field `{field}`")]
    PositionOverflow { field: &'static str, pos: u64 },

    /// `log_end_offset` is smaller than the active segment's own
    /// `base_offset` — an invariant that must always hold and would
    /// otherwise underflow the `u32` offset-delta computation (review
    /// P1-2/P1-3). Turned into a hard error instead of a panicking
    /// subtraction or a silently wrapped result.
    #[error("offset chain corrupt: log_end_offset {log_end_offset} is behind active segment base_offset {segment_base_offset}")]
    OffsetChainCorrupt {
        log_end_offset: u64,
        segment_base_offset: u64,
    },

    #[error("partition directory {path} is already locked by another process/handle")]
    PartitionLocked { path: PathBuf },
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
