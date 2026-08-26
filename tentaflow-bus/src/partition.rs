// ===== File: partition.rs — single-writer partition, monotonic offsets, readers =====
//
// PLAN.md §5.3.4/§2.2: exactly one writer per partition, fed by a bounded
// `tokio::sync::mpsc` channel; a full channel returns `Throttled` instead of
// growing a buffer. The channel's blocking variants (`try_send`,
// `blocking_recv`) do not need an active Tokio `Runtime`, so this whole
// module — including the dedicated writer thread — stays synchronous by
// default; `append_batch_async` is the async twin for callers already
// running on a Tokio worker thread, where `append_batch`'s
// `resp_rx.blocking_recv()` would panic (review P3-14).
//
// Offset assignment is the writer's job, not the producer's: a batch built
// with `BatchBuilder` carries a placeholder `base_offset`, and the writer
// patches that one 8-byte header field in place before the append. This is
// safe *because* the batch CRC only covers the body (PLAN §2.3: "crc32c nad
// body"), never the header — patching the header cannot invalidate it.
//
// Group commit (review decision #5): the writer thread drains the channel
// up to a small job/byte budget before performing any I/O, does one
// positional write per job, then exactly one fsync covering the whole
// group, then acks every job in the group. Per-producer durability is
// unchanged — a producer's `oneshot` only resolves once the fsync covering
// *its* batch has completed — but N batches now share one fsync instead of
// paying for N, which is the direct fix for the review's core finding that
// an un-potokowany, single-fsync-per-append harness measures fsync latency
// as a throughput ceiling that group commit removes.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bytes::Bytes;
use parking_lot::{Mutex, RwLock};
use tokio::sync::{mpsc, oneshot};

use crate::batch::{BatchHeader, BatchView, BATCH_HEADER_LEN};
use crate::error::{BusError, Result};
use crate::index::{
    floor_offset, floor_time, OffsetEntry, OffsetIndex, SharedEntries, TimeEntry, TimeIndex,
};
use crate::segment::{self, RollPolicy, Segment};

/// Largest single `pread` a fetch will attempt before falling back to an
/// exact-size read for an oversized batch (review P2-11/P3-4): sized to
/// PLAN §5.3.1's default `batch_max_bytes` (1 MiB) so the overwhelming
/// majority of batches are satisfied by exactly one syscall instead of the
/// previous header-then-body double read.
const READAHEAD_BYTES: usize = 1024 * 1024;

/// Group-commit batching limits (review decision #5): the writer drains up
/// to this many queued jobs, or this many bytes, whichever comes first,
/// before doing any I/O for the group.
const GROUP_COMMIT_MAX_JOBS: usize = 64;
const GROUP_COMMIT_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Durability policy applied after each batch append (PLAN §5.3.5).
#[derive(Debug, Clone, Copy)]
pub enum Durability {
    /// No explicit fsync; rely on the OS to flush eventually.
    Os,
    /// fsync (`fdatasync`-equivalent) after every group — the default in
    /// Prod. On Apple platforms this pushes data to the drive's cache, not
    /// necessarily the platter/NAND itself (review P1-4 item 4).
    FsyncBatch,
    /// Like `FsyncBatch`, but on macOS/iOS uses `fcntl(F_FULLFSYNC)` to
    /// force the drive to flush its own write cache — the stronger
    /// durability barrier PLAN §5.3.6 calls out, falling back to the same
    /// behavior as `FsyncBatch` on every other platform.
    FsyncBatchFull,
    /// fsync at most once per interval, batching fsyncs across appends.
    FsyncInterval(Duration),
}

#[derive(Debug, Clone, Copy)]
pub struct AppendResult {
    pub base_offset: u64,
    pub segment_base_offset: u64,
    pub file_pos: u64,
}

struct WriteJob {
    batch: Bytes,
    resp: oneshot::Sender<Result<AppendResult>>,
}

/// Read-side view of one segment, shared between the writer thread (which
/// updates `len` and appends to the index `Arc`s) and every `PartitionReader`
/// (which only ever reads them). No data is duplicated between the writer's
/// own `OffsetIndex`/`TimeIndex` and this descriptor — both hold clones of
/// the same `Arc<RwLock<Vec<_>>>`. Held behind `Arc` (review P3-5) so a
/// reader can snapshot the segment list with a cheap refcount-bump clone
/// instead of holding `PartitionState::segments`'s lock across file I/O.
struct SegmentDescriptor {
    base_offset: u64,
    log_path: PathBuf,
    len: Arc<AtomicU64>,
    offset_entries: SharedEntries<OffsetEntry>,
    time_entries: SharedEntries<TimeEntry>,
    /// Lazily-opened, cached read-only file descriptor, shared across every
    /// `PartitionReader` (they all share the same underlying
    /// `PartitionState`/segment list). Opened once per segment for the
    /// partition's lifetime instead of once per `fetch_from_offset` call
    /// per segment (review P3-3). The writer thread never touches this —
    /// it holds its own `Segment` with its own fd — so "readers never
    /// touch the writer" still holds; this is readers sharing a fd among
    /// themselves.
    reader_fd: Mutex<Option<Arc<File>>>,
}

impl SegmentDescriptor {
    fn reader_file(&self) -> Result<Arc<File>> {
        let mut guard = self.reader_fd.lock();
        if let Some(f) = guard.as_ref() {
            return Ok(Arc::clone(f));
        }
        let file = File::open(&self.log_path).map_err(|e| BusError::io(&self.log_path, e))?;
        let file = Arc::new(file);
        *guard = Some(Arc::clone(&file));
        Ok(file)
    }
}

struct PartitionState {
    segments: RwLock<Vec<Arc<SegmentDescriptor>>>,
    log_end_offset: AtomicU64,
    /// Set once by the writer thread after an unrecoverable panic (review
    /// P1-2's `catch_unwind` defense) and checked by `append_batch`/
    /// `append_batch_async` before ever touching the channel, so a caller
    /// gets a specific `WriterPoisoned` instead of the generic
    /// `WriterClosed` a dropped channel would otherwise report.
    poisoned: AtomicBool,
}

fn list_segment_base_offsets(dir: &Path) -> Result<Vec<u64>> {
    let mut out = Vec::new();
    let rd = std::fs::read_dir(dir).map_err(|e| BusError::io(dir, e))?;
    for entry in rd {
        let entry = entry.map_err(|e| BusError::io(dir, e))?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("log") {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            if let Ok(base_offset) = stem.parse::<u64>() {
                out.push(base_offset);
            }
        }
    }
    out.sort_unstable();
    Ok(out)
}

/// Rewrites the batch's `base_offset` header field (bytes 4..12) in place.
/// Safe without touching the CRC, which only covers the body. Falls back to
/// copying if this buffer is unexpectedly shared (refcount > 1) — the
/// intended call site always hands over a batch nobody else has cloned yet.
fn patch_base_offset(batch: Bytes, base_offset: u64) -> Bytes {
    let mut mutable = match batch.try_into_mut() {
        Ok(m) => m,
        Err(shared) => bytes::BytesMut::from(&shared[..]),
    };
    mutable[4..12].copy_from_slice(&base_offset.to_le_bytes());
    mutable.freeze()
}

struct WriterHandles {
    active_segment: Segment,
    active_offset_index: OffsetIndex,
    active_time_index: TimeIndex,
}

fn roll(
    dir: &Path,
    roll_policy: &RollPolicy,
    state: &PartitionState,
    handles: &mut WriterHandles,
) -> Result<()> {
    // Seal the outgoing segment durably regardless of the durability policy
    // — a segment boundary is a natural, infrequent point to pay for fsync
    // even under `Durability::Os`. Also reclaims any unused preallocated
    // tail (review P1-4's prealloc mechanism) so a segment that rolled
    // early does not permanently occupy `max_bytes` on disk.
    handles.active_segment.fsync()?;
    handles.active_segment.truncate_to_len()?;

    let next_base_offset = state.log_end_offset.load(Ordering::Acquire);
    let new_segment = Segment::create_new(dir, next_base_offset, roll_policy.max_bytes)?;
    let new_offset_index =
        OffsetIndex::open_or_create(segment::offset_index_path(dir, next_base_offset))?;
    let new_time_index =
        TimeIndex::open_or_create(segment::time_index_path(dir, next_base_offset))?;

    state.segments.write().push(Arc::new(SegmentDescriptor {
        base_offset: next_base_offset,
        log_path: segment::log_path(dir, next_base_offset),
        len: Arc::new(AtomicU64::new(0)),
        offset_entries: new_offset_index.shared(),
        time_entries: new_time_index.shared(),
        reader_fd: Mutex::new(None),
    }));

    handles.active_segment = new_segment;
    handles.active_offset_index = new_offset_index;
    handles.active_time_index = new_time_index;
    Ok(())
}

/// One job's outcome after its positional write and index updates, but
/// before the group's fsync/publish step — everything needed to finish
/// publishing it once the group's durability barrier has been decided.
struct Landed {
    desc: Arc<SegmentDescriptor>,
    len_after: u64,
    next_offset: u64,
    append_result: AppendResult,
}

/// Appends one batch: rolls the segment if needed, decodes+patches the
/// header, writes it, and updates both sparse indexes. Does *not* fsync or
/// publish anything visible to readers — that is the group's job, done once
/// for every job landed in this call (see `process_group`).
///
/// `base_offset` is passed in rather than read from
/// `state.log_end_offset` because group commit defers publishing that
/// atomic until *after* the whole group has been appended and fsynced
/// (review P2-1) — if every job in a group re-read the same
/// not-yet-advanced shared counter, every job after the first would be
/// assigned the same offset as the first, silently colliding. The caller
/// (`process_group`) threads a local cursor through successive calls
/// instead.
fn append_one(
    dir: &Path,
    roll_policy: &RollPolicy,
    state: &PartitionState,
    handles: &mut WriterHandles,
    batch: Bytes,
    base_offset: u64,
) -> Result<Landed> {
    if handles.active_segment.should_roll(roll_policy) {
        roll(dir, roll_policy, state, handles)?;
    }

    // Decoding against the whole buffer (not a pre-sliced `&batch[..40]`)
    // means a short buffer is rejected by `BatchHeader::decode`'s own
    // length check instead of panicking at the slice expression itself
    // (review P1-2).
    let header = BatchHeader::decode(&batch)?;
    let patched = patch_base_offset(batch, base_offset);

    let segment_base_offset = handles.active_segment.base_offset();
    let pos_before = handles.active_segment.len();
    let file_pos = handles.active_segment.append(&patched)?;

    // `base_offset` must never be behind the active segment's own base —
    // that would mean the whole partition's offset bookkeeping is already
    // corrupt. Turned into a hard error instead of a panicking or
    // silently-wrapping subtraction (review P1-2/P1-3).
    let offset_delta_u64 =
        base_offset
            .checked_sub(segment_base_offset)
            .ok_or(BusError::OffsetChainCorrupt {
                log_end_offset: base_offset,
                segment_base_offset,
            })?;
    let offset_delta = u32::try_from(offset_delta_u64).map_err(|_| BusError::PositionOverflow {
        field: "offset_delta",
        pos: offset_delta_u64,
    })?;
    let file_pos_u32 = u32::try_from(file_pos).map_err(|_| BusError::PositionOverflow {
        field: "file_pos",
        pos: file_pos,
    })?;

    // If either index write fails (e.g. ENOSPC) after the segment append
    // already landed on disk, roll the segment's logical length back to
    // where it was before this job — `log_end_offset` was never advanced
    // for this job, so a retry with the same batch lands at the same file
    // position and physically overwrites the orphaned bytes instead of
    // leaving a gap the offset index does not know about (review P3-16).
    if let Err(e) = handles.active_offset_index.append(OffsetEntry {
        offset_delta,
        file_pos: file_pos_u32,
    }) {
        handles.active_segment.rollback_len(pos_before);
        return Err(e);
    }
    if let Err(e) = handles.active_time_index.append(TimeEntry {
        ts_ms: header.base_timestamp_ms,
        offset_delta,
    }) {
        handles.active_segment.rollback_len(pos_before);
        return Err(e);
    }

    let next_offset = base_offset + header.record_count as u64;
    let desc = Arc::clone(
        state
            .segments
            .read()
            .last()
            .expect("partition always has at least one segment"),
    );
    let len_after = handles.active_segment.len();

    Ok(Landed {
        desc,
        len_after,
        next_offset,
        append_result: AppendResult {
            base_offset,
            segment_base_offset,
            file_pos,
        },
    })
}

/// Processes one drained group of jobs: appends every job's batch (each
/// independently succeeding or failing), performs exactly one fsync for the
/// whole group per the durability policy, then publishes every successful
/// job's visible state (segment length, then high watermark — in that
/// order, review P2-1) and acks every job.
fn process_group(
    dir: &Path,
    roll_policy: &RollPolicy,
    durability: Durability,
    state: &PartitionState,
    handles: &mut WriterHandles,
    last_fsync: &mut Instant,
    jobs: Vec<WriteJob>,
) {
    let mut cursor = state.log_end_offset.load(Ordering::Acquire);
    let mut landed: Vec<(oneshot::Sender<Result<AppendResult>>, Result<Landed>)> =
        Vec::with_capacity(jobs.len());
    for job in jobs {
        let outcome = append_one(dir, roll_policy, state, handles, job.batch, cursor);
        if let Ok(l) = &outcome {
            // Only a successful append consumes an offset — a job that
            // failed (e.g. a malformed header) leaves `cursor` where it
            // was so the *next* job in this group (or the next group) is
            // assigned the offset the failed job never got to keep.
            cursor = l.next_offset;
        }
        landed.push((job.resp, outcome));
    }

    let any_ok = landed.iter().any(|(_, r)| r.is_ok());
    let fsync_err: Option<String> = if any_ok {
        match durability {
            Durability::Os => None,
            Durability::FsyncBatch => handles.active_segment.fsync().err().map(|e| e.to_string()),
            Durability::FsyncBatchFull => handles
                .active_segment
                .fsync_full()
                .err()
                .map(|e| e.to_string()),
            Durability::FsyncInterval(interval) => {
                if last_fsync.elapsed() >= interval {
                    let r = handles.active_segment.fsync().err().map(|e| e.to_string());
                    *last_fsync = Instant::now();
                    r
                } else {
                    None
                }
            }
        }
    } else {
        None
    };

    if let Some(message) = fsync_err {
        let path = handles.active_segment.path().to_path_buf();
        for (_, r) in landed.iter_mut() {
            if r.is_ok() {
                *r = Err(BusError::FsyncFailed {
                    path: path.clone(),
                    message: message.clone(),
                });
            }
        }
    }

    for (resp, outcome) in landed {
        let result = outcome.map(|l| {
            // Segment length is published *before* the high watermark
            // (review P2-1): a reader gates on `high_watermark()` first,
            // then reads up to `desc.len` — publishing in the other order
            // let a reader observe `high_watermark() > from_offset` for an
            // instant before the corresponding bytes were visible, which
            // `fetch_from_offset`'s "always returns at least one batch"
            // contract does not allow for.
            l.desc.len.store(l.len_after, Ordering::Release);
            state.log_end_offset.store(l.next_offset, Ordering::Release);
            l.append_result
        });
        let _ = resp.send(result);
    }
}

fn writer_loop(
    dir: PathBuf,
    roll_policy: RollPolicy,
    durability: Durability,
    mut rx: mpsc::Receiver<WriteJob>,
    state: Arc<PartitionState>,
    mut handles: WriterHandles,
) {
    let mut last_fsync = Instant::now();
    loop {
        let first = match rx.blocking_recv() {
            Some(job) => job,
            None => break,
        };
        let mut group_bytes = first.batch.len();
        let mut jobs = vec![first];
        while jobs.len() < GROUP_COMMIT_MAX_JOBS && group_bytes < GROUP_COMMIT_MAX_BYTES {
            match rx.try_recv() {
                Ok(job) => {
                    group_bytes += job.batch.len();
                    jobs.push(job);
                }
                Err(_) => break,
            }
        }

        // Defense in depth (review P1-2): P1-1/P1-2's bounds checks close
        // every *known* panic source in this path, but a genuine bug
        // elsewhere (or in a future change) should not be able to silently
        // corrupt partition state. `&mut handles`/`&mut last_fsync` are not
        // `UnwindSafe` by default (mutable references), hence the explicit
        // assertion — after a caught panic this partition refuses all
        // further writes rather than risk continuing with state that may
        // be inconsistent partway through the group.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            process_group(
                &dir,
                &roll_policy,
                durability,
                &state,
                &mut handles,
                &mut last_fsync,
                jobs,
            )
        }));
        if result.is_err() {
            state.poisoned.store(true, Ordering::Release);
            tracing::error!(
                "partition writer thread panicked; partition is now poisoned and accepts no further writes"
            );
            break;
        }
    }
    // Best-effort final fsync so a clean shutdown leaves data durable even
    // under `Durability::Os`.
    let _ = handles.active_segment.fsync();
}

struct PartitionInner {
    state: Arc<PartitionState>,
    tx: Option<mpsc::Sender<WriteJob>>,
    writer_thread: Option<JoinHandle<()>>,
    throttle_hint_ms: u32,
    /// Held for the partition's lifetime purely for its `Drop` effect:
    /// closing this file releases the OS-level advisory lock acquired in
    /// `Partition::open`, so a second `Partition::open` on the same
    /// directory can only succeed after this one is gone (review P2-8).
    _lock: File,
}

impl Drop for PartitionInner {
    fn drop(&mut self) {
        // Dropping the sender closes the channel, which unblocks the
        // writer thread's `blocking_recv()` with `None` and ends its loop.
        self.tx.take();
        if let Some(handle) = self.writer_thread.take() {
            let _ = handle.join();
        }
    }
}

/// A single partition: one directory of segments, one writer thread, many
/// independent readers. Cheaply `Clone`-able (Arc-backed); the writer thread
/// shuts down once the last clone is dropped.
#[derive(Clone)]
pub struct Partition {
    inner: Arc<PartitionInner>,
}

impl Partition {
    /// Opens (or creates) a partition directory. If the directory already
    /// holds segments, every segment but the last is trusted as sealed;
    /// the last one is rescanned from byte 0 and truncated at the last
    /// batch with a valid, chain-consistent header and CRC (PLAN §2.2
    /// crash recovery; review P1-3).
    pub fn open(
        dir: impl AsRef<Path>,
        roll_policy: RollPolicy,
        durability: Durability,
        channel_capacity: usize,
    ) -> Result<Self> {
        // `RollPolicy::max_bytes` becomes a `u32` file position/offset-delta
        // on the wire (`.oidx`'s `file_pos`, PLAN §2.3's "segment ≤ 1 GiB,
        // więc u32 na pozycję wystarcza"); validate the policy once here
        // instead of silently truncating with `as u32` on the hot path
        // (review P2-7).
        if roll_policy.max_bytes > u32::MAX as u64 {
            return Err(BusError::RollPolicyInvalid {
                max_bytes: roll_policy.max_bytes,
            });
        }

        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir).map_err(|e| BusError::io(&dir, e))?;

        // Exclusive directory lock, acquired before anything else touches
        // the directory's contents: a second `Partition::open` on the same
        // path (same or a different process) must fail fast instead of
        // racing the recovery scan/truncate against a live writer (review
        // P2-8). Released automatically when `PartitionInner` (and this
        // `File`) drops.
        let lock_path = dir.join(".lock");
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|e| BusError::io(&lock_path, e))?;
        lock_file.try_lock().map_err(|e| match e {
            std::fs::TryLockError::WouldBlock => BusError::PartitionLocked { path: dir.clone() },
            std::fs::TryLockError::Error(io_err) => BusError::io(&lock_path, io_err),
        })?;

        let base_offsets = list_segment_base_offsets(&dir)?;
        let mut segments_state: Vec<Arc<SegmentDescriptor>> = Vec::new();
        let active_segment;
        let active_offset_index;
        let active_time_index;
        let log_end_offset;

        if base_offsets.is_empty() {
            let seg = Segment::create_new(&dir, 0, roll_policy.max_bytes)?;
            let oidx = OffsetIndex::open_or_create(segment::offset_index_path(&dir, 0))?;
            let tidx = TimeIndex::open_or_create(segment::time_index_path(&dir, 0))?;
            segments_state.push(Arc::new(SegmentDescriptor {
                base_offset: 0,
                log_path: segment::log_path(&dir, 0),
                len: Arc::new(AtomicU64::new(0)),
                offset_entries: oidx.shared(),
                time_entries: tidx.shared(),
                reader_fd: Mutex::new(None),
            }));
            active_segment = seg;
            active_offset_index = oidx;
            active_time_index = tidx;
            log_end_offset = 0;
        } else {
            let (sealed, last_slice) = base_offsets.split_at(base_offsets.len() - 1);
            let last_base = last_slice[0];

            for &base_offset in sealed {
                let seg = Segment::open_sealed(&dir, base_offset)?;
                let oidx =
                    OffsetIndex::open_or_create(segment::offset_index_path(&dir, base_offset))?;
                let tidx = TimeIndex::open_or_create(segment::time_index_path(&dir, base_offset))?;
                segments_state.push(Arc::new(SegmentDescriptor {
                    base_offset,
                    log_path: segment::log_path(&dir, base_offset),
                    len: Arc::new(AtomicU64::new(seg.len())),
                    offset_entries: oidx.shared(),
                    time_entries: tidx.shared(),
                    reader_fd: Mutex::new(None),
                }));
            }

            let (seg, recovered) = Segment::open_active_with_recovery(&dir, last_base)?;
            let mut oidx =
                OffsetIndex::open_or_create(segment::offset_index_path(&dir, last_base))?;
            let mut tidx = TimeIndex::open_or_create(segment::time_index_path(&dir, last_base))?;
            oidx.reset()?;
            tidx.reset()?;
            for rb in &recovered {
                // Safe by construction: `Segment::open_active_with_recovery`
                // only accepts batches whose `base_offset` continues the
                // chain starting at `last_base` (review P1-3), so this
                // subtraction cannot underflow — `checked_sub` here turns a
                // violated invariant into a clean error instead of a panic
                // or a wrapped `as u32` (review P1-2).
                let offset_delta_u64 = rb.header.base_offset.checked_sub(last_base).ok_or(
                    BusError::OffsetChainCorrupt {
                        log_end_offset: rb.header.base_offset,
                        segment_base_offset: last_base,
                    },
                )?;
                let offset_delta =
                    u32::try_from(offset_delta_u64).map_err(|_| BusError::PositionOverflow {
                        field: "offset_delta",
                        pos: offset_delta_u64,
                    })?;
                let file_pos =
                    u32::try_from(rb.file_pos).map_err(|_| BusError::PositionOverflow {
                        field: "file_pos",
                        pos: rb.file_pos,
                    })?;
                oidx.append(OffsetEntry {
                    offset_delta,
                    file_pos,
                })?;
                tidx.append(TimeEntry {
                    ts_ms: rb.header.base_timestamp_ms,
                    offset_delta,
                })?;
            }
            segments_state.push(Arc::new(SegmentDescriptor {
                base_offset: last_base,
                log_path: segment::log_path(&dir, last_base),
                len: Arc::new(AtomicU64::new(seg.len())),
                offset_entries: oidx.shared(),
                time_entries: tidx.shared(),
                reader_fd: Mutex::new(None),
            }));

            // If the active segment's tail scan recovered nothing (it was
            // rolled just before a crash and never received a batch — the
            // segment file exists, empty, its own base_offset *is* its
            // filename), the only trustworthy source for the resulting
            // `log_end_offset` is that filename itself. The previous
            // fallback here read the *previous* sealed segment's `.oidx`
            // tail — but `.oidx` is never fsynced (`index.rs` writes go
            // through plain `write_all`), so its last entry can be stale
            // after a crash and could report an offset *behind*
            // `last_base`, underflowing every subsequent offset-delta
            // computation (review P2-3). `last_base` is always correct
            // here and requires trusting nothing but the filename a prior
            // `roll()` (or this same `Partition::open`, for offset 0)
            // already committed to disk.
            log_end_offset = match recovered.last() {
                Some(last_rb) => last_rb.header.next_offset(),
                None => last_base,
            };

            active_segment = seg;
            active_offset_index = oidx;
            active_time_index = tidx;
        }

        let state = Arc::new(PartitionState {
            segments: RwLock::new(segments_state),
            log_end_offset: AtomicU64::new(log_end_offset),
            poisoned: AtomicBool::new(false),
        });

        let (tx, rx) = mpsc::channel(channel_capacity.max(1));
        let handles = WriterHandles {
            active_segment,
            active_offset_index,
            active_time_index,
        };
        let state_for_thread = Arc::clone(&state);
        let dir_for_thread = dir.clone();
        let writer_thread = thread::Builder::new()
            .name("tentaflow-bus-partition-writer".into())
            .spawn(move || {
                writer_loop(
                    dir_for_thread,
                    roll_policy,
                    durability,
                    rx,
                    state_for_thread,
                    handles,
                )
            })
            .map_err(|e| BusError::io(&dir, e))?;

        Ok(Partition {
            inner: Arc::new(PartitionInner {
                state,
                tx: Some(tx),
                writer_thread: Some(writer_thread),
                throttle_hint_ms: 5,
                _lock: lock_file,
            }),
        })
    }

    /// Submits one already-built batch to the single writer. Blocks the
    /// calling *thread* (not just the async task — see `append_batch_async`
    /// for a Tokio-friendly version) until the writer durably (per the
    /// configured `Durability`) appends it and reports back — never blocks
    /// on a full channel, returning `Throttled` instead (PLAN §5.3.7). The
    /// returned `Throttled` carries the batch back so a retrying caller
    /// does not have to keep its own clone (review P2-5).
    pub fn append_batch(&self, batch: Bytes) -> Result<AppendResult> {
        if self.inner.state.poisoned.load(Ordering::Acquire) {
            return Err(BusError::WriterPoisoned);
        }
        let tx = self.inner.tx.as_ref().ok_or(BusError::WriterClosed)?;
        let (resp_tx, resp_rx) = oneshot::channel();
        match tx.try_send(WriteJob {
            batch,
            resp: resp_tx,
        }) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(job)) => {
                return Err(BusError::Throttled {
                    retry_after_ms: self.inner.throttle_hint_ms,
                    batch: job.batch,
                });
            }
            Err(mpsc::error::TrySendError::Closed(_)) => return Err(BusError::WriterClosed),
        }
        resp_rx
            .blocking_recv()
            .map_err(|_| BusError::WriterClosed)?
    }

    /// Async counterpart to `append_batch` (review P3-14): submission
    /// (`try_send`) is still non-blocking and throttles exactly like the
    /// sync path, but waiting for the writer's ack goes through the
    /// `oneshot::Receiver` future instead of `blocking_recv()`, so this is
    /// safe to call from a Tokio task without `spawn_blocking` — unlike
    /// `append_batch`, whose `blocking_recv()` panics if driven directly on
    /// a runtime worker thread.
    pub async fn append_batch_async(&self, batch: Bytes) -> Result<AppendResult> {
        if self.inner.state.poisoned.load(Ordering::Acquire) {
            return Err(BusError::WriterPoisoned);
        }
        let tx = self.inner.tx.as_ref().ok_or(BusError::WriterClosed)?;
        let (resp_tx, resp_rx) = oneshot::channel();
        match tx.try_send(WriteJob {
            batch,
            resp: resp_tx,
        }) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(job)) => {
                return Err(BusError::Throttled {
                    retry_after_ms: self.inner.throttle_hint_ms,
                    batch: job.batch,
                });
            }
            Err(mpsc::error::TrySendError::Closed(_)) => return Err(BusError::WriterClosed),
        }
        resp_rx.await.map_err(|_| BusError::WriterClosed)?
    }

    pub fn log_end_offset(&self) -> u64 {
        self.inner.state.log_end_offset.load(Ordering::Acquire)
    }

    /// M0 has no replication yet, so every durable append is immediately
    /// visible — kept as its own method so call sites do not change once M2
    /// gates this on ISR acknowledgement.
    pub fn high_watermark(&self) -> u64 {
        self.log_end_offset()
    }

    pub fn open_reader(&self) -> PartitionReader {
        PartitionReader {
            state: Arc::clone(&self.inner.state),
        }
    }
}

/// An independent read handle: its own file descriptors (cached per segment,
/// review P3-3), never the writer's. Cheap to clone (Arc-backed shared
/// index/state), matching the "fan-out to N independent readers" scenario
/// in PLAN §5.2 P3.
#[derive(Clone)]
pub struct PartitionReader {
    state: Arc<PartitionState>,
}

/// One read call's scratch buffer for the readahead-then-fallback read
/// strategy (review P2-11/P3-4): reused across every batch/segment visited
/// during a single `fetch_from_offset`/`fetch_from_timestamp` call instead
/// of allocating fresh per batch.
struct ReadBuf {
    buf: Vec<u8>,
}

impl ReadBuf {
    fn new() -> Self {
        Self {
            buf: vec![0u8; READAHEAD_BYTES],
        }
    }

    /// Reads one batch at `pos` (which must satisfy
    /// `pos + BATCH_HEADER_LEN <= seg_len`) with as few syscalls as the
    /// batch size allows: one `pread` of up to `READAHEAD_BYTES` covers
    /// header *and* body for any batch that fits in that window (the
    /// common case — PLAN's default `batch_max_bytes` is 1 MiB); a batch
    /// larger than the readahead window falls back to a second, exact-size
    /// `pread` for the remainder, still without ever re-reading the header
    /// bytes already in hand.
    fn read_batch(
        &mut self,
        file: &File,
        path: &Path,
        pos: u64,
        seg_len: u64,
    ) -> Result<(Bytes, u64)> {
        let want = READAHEAD_BYTES.min((seg_len - pos) as usize);
        if self.buf.len() < want {
            self.buf.resize(want, 0);
        }
        segment::pread_exact(file, pos, &mut self.buf[..want])
            .map_err(|e| BusError::io(path, e))?;
        let header = BatchHeader::decode(&self.buf[..BATCH_HEADER_LEN])?;
        let total = BATCH_HEADER_LEN as u64 + header.body_len as u64;
        if total <= want as u64 {
            let bytes = Bytes::copy_from_slice(&self.buf[..total as usize]);
            return Ok((bytes, total));
        }
        // Oversized batch: the readahead window only covered the header
        // (and part of the body). Read the exact remainder rather than
        // re-fetching bytes already in `self.buf`.
        let mut raw = vec![0u8; total as usize];
        raw[..want].copy_from_slice(&self.buf[..want]);
        segment::pread_exact(file, pos + want as u64, &mut raw[want..])
            .map_err(|e| BusError::io(path, e))?;
        Ok((Bytes::from(raw), total))
    }
}

impl PartitionReader {
    pub fn high_watermark(&self) -> u64 {
        self.state.log_end_offset.load(Ordering::Acquire)
    }

    /// Reads batches starting at `from_offset`, accumulating up to
    /// `max_bytes` of on-wire batch bytes. Always returns at least one
    /// batch if `from_offset < high_watermark()`, even if that single batch
    /// alone exceeds `max_bytes`.
    ///
    /// Returns *whole* batches (review P3-13): the sparse `.oidx` only
    /// gives a floor position, so the first returned batch may start at an
    /// offset before `from_offset` — the same semantics Kafka's fetch API
    /// has. A caller that needs an exact starting record should iterate the
    /// first batch through `BatchView::records_from(from_offset)` rather
    /// than assuming every record in the result is `>= from_offset`.
    pub fn fetch_from_offset(&self, from_offset: u64, max_bytes: usize) -> Result<Vec<BatchView>> {
        if from_offset >= self.high_watermark() {
            return Ok(Vec::new());
        }

        // Snapshot the segment list (cheap: a `Vec` of `Arc` clones) and
        // drop the lock immediately — holding `segments.read()` across the
        // I/O below would make `roll()`'s write-lock acquisition (and thus
        // every future append) wait on however long this fetch's disk I/O
        // takes (review P3-5).
        let segments: Vec<Arc<SegmentDescriptor>> = self.state.segments.read().clone();
        let mut seg_idx = segments
            .partition_point(|s| s.base_offset <= from_offset)
            .saturating_sub(1);

        let mut out = Vec::new();
        let mut consumed = 0usize;
        let mut want_offset = from_offset;
        let mut readbuf = ReadBuf::new();

        while seg_idx < segments.len() {
            let desc = &segments[seg_idx];
            let target_delta = want_offset.saturating_sub(desc.base_offset) as u32;
            let start_pos = {
                let g = desc.offset_entries.read();
                floor_offset(&g, target_delta)
                    .map(|e| e.file_pos as u64)
                    .unwrap_or(0)
            };
            let file = desc.reader_file()?;
            let seg_len = desc.len.load(Ordering::Acquire);

            let mut pos = start_pos;
            while pos + BATCH_HEADER_LEN as u64 <= seg_len {
                let (raw, total) = readbuf.read_batch(&file, &desc.log_path, pos, seg_len)?;
                let header = BatchHeader::decode(&raw[..BATCH_HEADER_LEN])?;
                if header.next_offset() <= want_offset {
                    // The index floor landed on/before a batch that ends
                    // before the requested offset — keep scanning forward.
                    pos += total;
                    continue;
                }

                let view = BatchView::parse(raw)?;
                consumed += view.wire_len();
                out.push(view);
                if consumed >= max_bytes {
                    return Ok(out);
                }
                pos += total;
            }

            seg_idx += 1;
            if seg_idx < segments.len() {
                want_offset = segments[seg_idx].base_offset;
            }
        }

        Ok(out)
    }

    /// Seeks to the first batch whose `base_timestamp_ms >= from_ts_ms`
    /// (using the time index's floor entry as a cheap starting point, then
    /// scanning forward — the same two-step search Kafka's time index
    /// uses), then reads forward from there like `fetch_from_offset`.
    pub fn fetch_from_timestamp(
        &self,
        from_ts_ms: i64,
        max_bytes: usize,
    ) -> Result<Vec<BatchView>> {
        let found_offset = {
            let segments: Vec<Arc<SegmentDescriptor>> = self.state.segments.read().clone();
            let mut found = None;
            'segments: for desc in segments.iter() {
                let start_delta = {
                    let g = desc.time_entries.read();
                    floor_time(&g, from_ts_ms)
                        .map(|e| e.offset_delta)
                        .unwrap_or(0)
                };
                let start_pos = {
                    let g = desc.offset_entries.read();
                    floor_offset(&g, start_delta)
                        .map(|e| e.file_pos as u64)
                        .unwrap_or(0)
                };
                let file = desc.reader_file()?;
                let seg_len = desc.len.load(Ordering::Acquire);
                let mut pos = start_pos;
                while pos + BATCH_HEADER_LEN as u64 <= seg_len {
                    let mut hdr = [0u8; BATCH_HEADER_LEN];
                    segment::pread_exact(&file, pos, &mut hdr)
                        .map_err(|e| BusError::io(&desc.log_path, e))?;
                    let header = BatchHeader::decode(&hdr)?;
                    let total = BATCH_HEADER_LEN as u64 + header.body_len as u64;
                    if pos + total > seg_len {
                        break;
                    }
                    if header.base_timestamp_ms >= from_ts_ms {
                        found = Some(header.base_offset);
                        break 'segments;
                    }
                    pos += total;
                }
            }
            found
        };

        match found_offset {
            Some(offset) => self.fetch_from_offset(offset, max_bytes),
            None => Ok(Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::batch::RecordInput;
    use crate::segment::log_path;
    use crate::test_support::temp_dir;

    fn one_record_batch(ts_ms: i64, payload_len: usize) -> Bytes {
        let mut b = crate::batch::BatchBuilder::new(0, 1); // base_offset is a placeholder; the writer patches it
        b.push(RecordInput::new(
            Bytes::from(vec![0x11; payload_len]),
            ts_ms,
        ))
        .unwrap();
        b.build().unwrap()
    }

    #[test]
    fn append_assigns_monotonic_offsets_and_reads_back() {
        let dir = temp_dir("partition-basic");
        let part = Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();

        let r0 = part.append_batch(one_record_batch(1_000, 16)).unwrap();
        let r1 = part.append_batch(one_record_batch(1_010, 16)).unwrap();
        let r2 = part.append_batch(one_record_batch(1_020, 16)).unwrap();
        assert_eq!(r0.base_offset, 0);
        assert_eq!(r1.base_offset, 1);
        assert_eq!(r2.base_offset, 2);
        assert_eq!(part.log_end_offset(), 3);

        let reader = part.open_reader();
        assert_eq!(reader.high_watermark(), 3);
        let batches = reader.fetch_from_offset(0, 1024 * 1024).unwrap();
        assert_eq!(batches.len(), 3);
        for (i, view) in batches.iter().enumerate() {
            assert_eq!(view.header().base_offset, i as u64);
        }

        // Seeking mid-stream skips exactly the requested prefix.
        let from1 = reader.fetch_from_offset(1, 1024 * 1024).unwrap();
        assert_eq!(from1.len(), 2);
        assert_eq!(from1[0].header().base_offset, 1);

        // Nothing at/after the watermark.
        assert!(reader.fetch_from_offset(3, 1024).unwrap().is_empty());
    }

    #[test]
    fn fetch_from_timestamp_finds_first_batch_at_or_after() {
        let dir = temp_dir("partition-time-seek");
        let part = Partition::open(&dir, RollPolicy::default(), Durability::Os, 8).unwrap();
        part.append_batch(one_record_batch(1_000, 8)).unwrap();
        part.append_batch(one_record_batch(2_000, 8)).unwrap();
        part.append_batch(one_record_batch(3_000, 8)).unwrap();

        let reader = part.open_reader();
        let from_1500 = reader.fetch_from_timestamp(1_500, 1024 * 1024).unwrap();
        assert_eq!(from_1500.len(), 2);
        assert_eq!(from_1500[0].header().base_timestamp_ms, 2_000);

        let from_0 = reader.fetch_from_timestamp(0, 1024 * 1024).unwrap();
        assert_eq!(from_0.len(), 3);

        let from_future = reader.fetch_from_timestamp(999_999, 1024 * 1024).unwrap();
        assert!(from_future.is_empty());
    }

    #[test]
    fn roll_policy_creates_new_segment_files() {
        let dir = temp_dir("partition-roll");
        let policy = RollPolicy {
            max_batches: 1,
            ..RollPolicy::default()
        };
        let part = Partition::open(&dir, policy, Durability::Os, 8).unwrap();
        part.append_batch(one_record_batch(0, 8)).unwrap();
        part.append_batch(one_record_batch(0, 8)).unwrap();
        part.append_batch(one_record_batch(0, 8)).unwrap();

        let mut base_offsets = list_segment_base_offsets(&dir).unwrap();
        base_offsets.sort_unstable();
        assert_eq!(base_offsets, vec![0, 1, 2]);
    }

    /// Review test gap #3: reading forward across a segment boundary must
    /// actually return the records on both sides of the roll, not just
    /// prove new segment files exist on disk.
    #[test]
    fn fetch_reads_across_segment_boundary() {
        let dir = temp_dir("partition-read-across-roll");
        let policy = RollPolicy {
            max_batches: 2,
            ..RollPolicy::default()
        };
        let part = Partition::open(&dir, policy, Durability::FsyncBatch, 8).unwrap();
        for i in 0..6i64 {
            part.append_batch(one_record_batch(i * 10, 8)).unwrap();
        }
        let mut base_offsets = list_segment_base_offsets(&dir).unwrap();
        base_offsets.sort_unstable();
        assert_eq!(base_offsets, vec![0, 2, 4], "rolled every 2 batches");

        let reader = part.open_reader();

        // From the very start, across all three segments.
        let all = reader.fetch_from_offset(0, 1024 * 1024).unwrap();
        assert_eq!(all.len(), 6);
        for (i, view) in all.iter().enumerate() {
            assert_eq!(view.header().base_offset, i as u64);
        }

        // From inside the middle segment, not at its base offset.
        let from3 = reader.fetch_from_offset(3, 1024 * 1024).unwrap();
        let offsets: Vec<u64> = from3.iter().map(|v| v.header().base_offset).collect();
        assert_eq!(offsets, vec![3, 4, 5]);

        // Exactly at a later segment's base offset.
        let from4 = reader.fetch_from_offset(4, 1024 * 1024).unwrap();
        let offsets4: Vec<u64> = from4.iter().map(|v| v.header().base_offset).collect();
        assert_eq!(offsets4, vec![4, 5]);
    }

    #[test]
    fn reopen_after_clean_shutdown_preserves_offset() {
        let dir = temp_dir("partition-reopen");
        {
            let part =
                Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
            part.append_batch(one_record_batch(0, 8)).unwrap();
            part.append_batch(one_record_batch(0, 8)).unwrap();
        } // Partition dropped here: writer thread joins, segment fsynced.

        let reopened =
            Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
        assert_eq!(reopened.log_end_offset(), 2);
        let r3 = reopened.append_batch(one_record_batch(0, 8)).unwrap();
        assert_eq!(r3.base_offset, 2);
    }

    /// Review P2-8: a second `Partition::open` on the same directory while
    /// the first handle is still alive must fail, not silently truncate the
    /// live writer's active segment.
    #[test]
    fn concurrent_open_on_same_directory_is_rejected() {
        let dir = temp_dir("partition-lock");
        let first = Partition::open(&dir, RollPolicy::default(), Durability::Os, 8).unwrap();
        let second = Partition::open(&dir, RollPolicy::default(), Durability::Os, 8);
        assert!(matches!(second, Err(BusError::PartitionLocked { .. })));
        drop(first);
        // Once the first handle is gone, the lock is released.
        let third = Partition::open(&dir, RollPolicy::default(), Durability::Os, 8);
        assert!(third.is_ok());
    }

    /// Crash-recovery through the full `Partition::open` path (not just
    /// `Segment` in isolation): two batches durably written, a third torn
    /// mid-body directly on disk (simulating a kill mid-append), then a
    /// fresh `Partition::open` on the same directory must recover to
    /// exactly the two complete batches and keep assigning offsets from
    /// there without gaps or corruption.
    #[test]
    fn partition_open_recovers_from_torn_write() {
        let dir = temp_dir("partition-crash-recovery");
        {
            let part =
                Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
            part.append_batch(one_record_batch(0, 64)).unwrap();
            part.append_batch(one_record_batch(0, 64)).unwrap();
        }

        let good_len = std::fs::metadata(log_path(&dir, 0)).unwrap().len();
        let torn = one_record_batch(0, 64);
        let torn_len = BATCH_HEADER_LEN + (torn.len() - BATCH_HEADER_LEN) / 2;
        {
            use std::os::unix::fs::FileExt;
            let file = OpenOptions::new()
                .write(true)
                .open(log_path(&dir, 0))
                .unwrap();
            file.write_all_at(&torn[..torn_len], good_len).unwrap();
        }
        assert_eq!(
            std::fs::metadata(log_path(&dir, 0)).unwrap().len(),
            good_len + torn_len as u64
        );

        let recovered =
            Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
        assert_eq!(recovered.log_end_offset(), 2);
        assert_eq!(
            std::fs::metadata(log_path(&dir, 0)).unwrap().len(),
            good_len
        );

        let reader = recovered.open_reader();
        let batches = reader.fetch_from_offset(0, 1024 * 1024).unwrap();
        assert_eq!(batches.len(), 2);

        let r = recovered.append_batch(one_record_batch(0, 8)).unwrap();
        assert_eq!(r.base_offset, 2);
    }

    /// Review test gap #2: a crash immediately after a segment roll — the
    /// new active segment exists but is completely empty, so its own tail
    /// scan recovers nothing. `log_end_offset` must come back as that
    /// segment's own base offset (its filename), not from the previous
    /// segment's (never-fsynced) `.oidx` tail.
    #[test]
    fn recovers_across_multiple_segments_with_empty_active_segment() {
        let dir = temp_dir("partition-multi-segment-empty-active");
        {
            let policy = RollPolicy {
                max_batches: 2,
                ..RollPolicy::default()
            };
            let part = Partition::open(&dir, policy, Durability::FsyncBatch, 8).unwrap();
            part.append_batch(one_record_batch(0, 8)).unwrap();
            part.append_batch(one_record_batch(0, 8)).unwrap();
            // This third append rolls into a brand-new, empty segment
            // (base offset 2) *before* appending, so segment 2 receives
            // this batch.
            part.append_batch(one_record_batch(0, 8)).unwrap();
        }
        // Simulate a crash right after a roll with nothing written yet:
        // create an empty segment 3 by hand (as `roll()` would have,
        // just before the writer received its next job).
        {
            let policy = RollPolicy {
                max_batches: 1,
                ..RollPolicy::default()
            };
            std::fs::write(log_path(&dir, 3), []).unwrap();
            let _ = policy; // not reopened under this policy; file crafted directly
        }

        let recovered =
            Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
        // The empty active segment (base offset 3) recovers zero batches;
        // log_end_offset must fall back to 3, not silently regress.
        assert_eq!(recovered.log_end_offset(), 3);

        let reader = recovered.open_reader();
        let batches = reader.fetch_from_offset(0, 1024 * 1024).unwrap();
        assert_eq!(batches.len(), 3);

        let r = recovered.append_batch(one_record_batch(0, 8)).unwrap();
        assert_eq!(r.base_offset, 3);
    }

    /// Backpressure smoke test: many threads hammer a partition with a
    /// deliberately tiny channel. We do not assert `Throttled` actually
    /// fires (that depends on scheduling/disk speed and would make the
    /// test flaky) — what must hold unconditionally is that every attempt
    /// resolves to either success or `Throttled` (never panics/deadlocks),
    /// and that the final log has exactly as many records as attempts that
    /// reported success, with no gaps.
    #[test]
    fn concurrent_producers_never_lose_or_duplicate_committed_offsets() {
        let dir = temp_dir("partition-concurrency");
        let part = Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 1).unwrap();

        let threads: Vec<_> = (0..8)
            .map(|_| {
                let part = part.clone();
                thread::spawn(move || {
                    let mut successes = 0u32;
                    for _ in 0..25 {
                        loop {
                            match part.append_batch(one_record_batch(0, 32)) {
                                Ok(_) => {
                                    successes += 1;
                                    break;
                                }
                                Err(BusError::Throttled { .. }) => continue,
                                Err(e) => panic!("unexpected error: {e}"),
                            }
                        }
                    }
                    successes
                })
            })
            .collect();

        let total_successes: u32 = threads.into_iter().map(|h| h.join().unwrap()).sum();
        assert_eq!(total_successes, 8 * 25);
        assert_eq!(part.log_end_offset(), 8 * 25);

        let reader = part.open_reader();
        let batches = reader.fetch_from_offset(0, 64 * 1024 * 1024).unwrap();
        let total_records: u32 = batches.iter().map(|b| b.header().record_count).sum();
        assert_eq!(total_records, 8 * 25);
    }

    /// Review test gap #4: a reader running concurrently with a live writer
    /// must never observe `high_watermark() > from_offset` without the
    /// corresponding batch actually being fetchable — the exact ordering
    /// bug described in P2-1. Runs many rounds under a small channel/group
    /// size to maximize the chance of hitting the old race window.
    #[test]
    fn concurrent_reader_never_sees_watermark_ahead_of_data() {
        let dir = temp_dir("partition-reader-writer-race");
        let part = Partition::open(&dir, RollPolicy::default(), Durability::Os, 4).unwrap();
        let reader = part.open_reader();

        let writer = part.clone();
        let writer_handle = thread::spawn(move || {
            for i in 0..500i64 {
                loop {
                    match writer.append_batch(one_record_batch(i, 16)) {
                        Ok(_) => break,
                        Err(BusError::Throttled { .. }) => continue,
                        Err(e) => panic!("unexpected error: {e}"),
                    }
                }
            }
        });

        let reader_handle = thread::spawn(move || {
            let mut last_seen_hw = 0u64;
            while last_seen_hw < 500 {
                let hw = reader.high_watermark();
                if hw > last_seen_hw {
                    // The contract: everything up to (not including) `hw`
                    // must be fetchable right now, in this exact instant —
                    // not "eventually consistent" a moment later.
                    let batches = reader
                        .fetch_from_offset(last_seen_hw, 4 * 1024 * 1024)
                        .unwrap();
                    assert!(
                        !batches.is_empty(),
                        "high_watermark()={hw} advanced past from_offset={last_seen_hw} with no fetchable batch"
                    );
                    last_seen_hw = hw;
                }
            }
        });

        writer_handle.join().unwrap();
        reader_handle.join().unwrap();
    }

    /// Review test gap #9: a batch built with `Codec::Lz4` must round-trip
    /// through the full append -> disk -> fetch path, not just
    /// `BatchBuilder`/`BatchView` in isolation (`batch.rs` only tests the
    /// codec directly).
    #[test]
    fn lz4_batch_round_trips_through_partition() {
        let dir = temp_dir("partition-lz4-roundtrip");
        let part = Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();

        let mut b = crate::batch::BatchBuilder::new(0, 1).with_codec(crate::batch::Codec::Lz4);
        for i in 0..50u32 {
            b.push(RecordInput::new(
                Bytes::from(format!("payload-{i}-{}", "x".repeat(64))),
                i as i64,
            ))
            .unwrap();
        }
        let batch = b.build().unwrap();
        part.append_batch(batch).unwrap();

        let reader = part.open_reader();
        let fetched = reader.fetch_from_offset(0, 1024 * 1024).unwrap();
        assert_eq!(fetched.len(), 1);
        assert!(matches!(
            fetched[0].header().codec().unwrap(),
            crate::batch::Codec::Lz4
        ));
        let records: Vec<_> = fetched[0].records().collect::<Result<_>>().unwrap();
        assert_eq!(records.len(), 50);
        assert_eq!(
            records[7].payload.as_ref(),
            format!("payload-7-{}", "x".repeat(64)).as_bytes()
        );
    }

    /// Review test gap #6: a deterministic `Throttled` path — capacity 1,
    /// `FsyncBatch` durability (so the writer thread is genuinely busy on
    /// disk I/O, not just instantaneously free again), one thread holding
    /// the channel's only slot via a job that has not been drained yet.
    #[test]
    fn throttled_returns_the_batch_deterministically() {
        use std::sync::Barrier;

        // A large batch (many records, real body bytes) makes each group's
        // pwrite+fsync take long enough (real disk I/O, `FsyncBatch`
        // durability) to widen the race window past microseconds: with
        // `channel_capacity == 1`, at most one producer can have a job
        // *queued* while the writer thread is busy inside one group's fsync
        // — every other concurrently-racing producer during that window
        // must observe `Throttled`, not just "might". Releasing many
        // threads at once via a `Barrier` (rather than relying on OS
        // thread-spawn scheduling jitter) makes the contention itself
        // deterministic; only the exact identity of which threads win vs.
        // get throttled is left to the scheduler.
        const PRODUCERS: usize = 24;
        fn large_batch(seed: i64) -> Bytes {
            let mut b = crate::batch::BatchBuilder::new(0, 1);
            for i in 0..512u32 {
                b.push(RecordInput::new(
                    Bytes::from(vec![0x77u8; 1024]),
                    seed + i as i64,
                ))
                .unwrap();
            }
            b.build().unwrap()
        }

        let dir = temp_dir("partition-throttle-deterministic");
        let part = Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 1).unwrap();
        let barrier = Arc::new(Barrier::new(PRODUCERS));

        let handles: Vec<_> = (0..PRODUCERS)
            .map(|i| {
                let part = part.clone();
                let barrier = Arc::clone(&barrier);
                let batch = large_batch(i as i64 * 1000);
                thread::spawn(move || {
                    barrier.wait();
                    let batch_len = batch.len();
                    match part.append_batch(batch) {
                        Ok(_) => None,
                        Err(BusError::Throttled {
                            retry_after_ms,
                            batch,
                        }) => {
                            assert!(retry_after_ms > 0);
                            // Review P2-5: the producer's buffer must come
                            // back unconsumed, not require a clone to retry.
                            assert_eq!(batch.len(), batch_len);
                            Some(())
                        }
                        Err(e) => panic!("unexpected error: {e}"),
                    }
                })
            })
            .collect();

        let throttled_count = handles
            .into_iter()
            .filter_map(|h| h.join().unwrap())
            .count();
        assert!(
            throttled_count > 0,
            "expected at least one Throttled response among {PRODUCERS} producers racing a \
             capacity=1 channel against FsyncBatch durability on real disk I/O"
        );
    }
}
