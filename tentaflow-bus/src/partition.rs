// ===== File: partition.rs — single-writer partition, monotonic offsets, readers =====
//
// PLAN.md §5.3.4/§2.2: exactly one writer per partition, fed by a bounded
// `tokio::sync::mpsc` channel; a full channel returns `Throttled` instead of
// growing a buffer. The channel's blocking variants (`try_send`,
// `blocking_recv`) do not need an active Tokio `Runtime`, so this whole
// module — including the dedicated writer thread — stays synchronous by
// default; `append_batch_async` is the async twin for callers already
// running on a Tokio worker thread, where `append_batch`'s
// `resp_rx.blocking_recv()` would panic.
//
// Offset assignment is the writer's job, not the producer's: a batch built
// with `BatchBuilder` carries a placeholder `base_offset`, and the writer
// patches that one 8-byte header field in place before the append. This is
// safe *because* the batch CRC only covers the body (PLAN §2.3: "crc32c nad
// body"), never the header — patching the header cannot invalidate it.
//
// Group commit: the writer thread drains the channel up to a small
// job/byte budget before performing any I/O, does one positional write per
// job, then exactly one fsync covering the whole group, then acks every job
// in the group. Per-producer durability is unchanged — a producer's
// `oneshot` only resolves once the fsync covering *its* batch has completed
// — but N batches now share one fsync instead of paying for N, which is
// what keeps a single-fsync-per-append cost from becoming the throughput
// ceiling under concurrent producers.
//
// A group's fsync only ever covers the *currently active* segment, so a
// failure that follows a roll partway through the same group needs two
// different responses depending on which side of the roll a job landed:
// the segment that got rolled *away* was already fsynced and truncated by
// `roll()` itself, so its bytes are durable regardless of what happens
// next, while the new active segment's bytes are not and get rolled back.
// The asymmetry that remains even after that rollback is offsets, not
// bytes: publishing `log_end_offset` for the durable, rolled-away jobs
// would be safe on its own, but partially publishing one group (some jobs
// visible, some not) is not something the rest of this module is written
// to do correctly, and *not* publishing them leaves the next append free to
// reuse their offset for different bytes on the next segment. Rather than
// solve partial-group publishing, `process_group` poisons the partition
// (`BusError::PartitionPoisoned`, see `append_batch`'s doc) whenever this
// straddling case is detected: every following append is refused until the
// directory is reopened, and `Partition::open`'s crash recovery — which
// only re-validates the tail of the *last* segment and trusts every sealed
// segment's own already-written `.oidx`/`.tidx` — naturally resumes from
// the last group that finished publishing, forgetting the unpublished
// batch rather than risking two batches sharing one offset.
//
// M2 (PLAN-M2 §1a) turns the single writer channel into a small command
// set, `WriterCommand`: `Append`/`AppendReplicated` (grouped and
// group-committed exactly as before, via the internal `AppendJob`/
// `AppendKind`), plus `Truncate` and `PersistMeta`. Routing every one of
// these through the same channel is not an implementation convenience —
// it is *the* mechanism that keeps `truncate_to_offset` from racing group
// commit (PLAN-M2 §4.2 M2-R3): a `Truncate` command can only ever be
// processed strictly before or strictly after any given `Append`, never
// concurrently with one, because both pass through this module's one
// writer thread. `high_watermark` similarly grows a role-gated tracking
// mode (`HwTracking`): `FollowLeo` (the default, M1's `hw == leo` behavior
// unchanged) auto-advances `high_watermark` on every successful append the
// way `process_group` always did; `Manual` (switched on by a
// `ReplicationCoordinator` once this partition has a real leader/follower
// role, wave 1 agents RL/RF) stops that auto-advance so `high_watermark`
// only ever moves via an explicit `set_high_watermark` call driven by ISR
// acknowledgement. `partition.meta` (`meta.rs`) persists `high_watermark`/
// `leader_epoch` so both survive a restart — see `Partition::open`'s doc
// for the exact fallback when the file does not exist (M1 partitions).

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bytes::Bytes;
use parking_lot::{Mutex, RwLock};
// Two independent `mpsc`s in this module, deliberately never confused:
// `mpsc` (tokio's) carries `WriterCommand`s to the writer thread, whose
// `Sender::try_send`/hot-path `Receiver::blocking_recv()` are safe without
// an active Runtime by design; `std_mpsc` (std's) carries the *reply* for
// `Truncate`/`PersistMeta` specifically because tokio's own
// `blocking_send`/`blocking_recv` panic when called from *any* Tokio
// runtime context — see `send_and_wait_via_writer_thread`'s doc.
use std::sync::mpsc as std_mpsc;
use tokio::sync::{mpsc, oneshot};

use crate::batch::{BatchHeader, BatchView, BATCH_HEADER_LEN};
use crate::error::{BusError, Result};
use crate::index::{
    floor_offset, floor_time, OffsetEntry, OffsetIndex, SharedEntries, TimeEntry, TimeIndex,
};
use crate::segment::{self, RollPolicy, Segment};

/// Largest single `pread` a fetch will attempt before falling back to an
/// exact-size read for an oversized batch: sized to PLAN §5.3.1's default
/// `batch_max_bytes` (1 MiB) so the overwhelming
/// majority of batches are satisfied by exactly one syscall instead of the
/// previous header-then-body double read.
const READAHEAD_BYTES: usize = 1024 * 1024;

/// Group-commit batching limits: the writer drains up to this many queued
/// jobs, or this many bytes, whichever comes first, before doing any I/O
/// for the group.
const GROUP_COMMIT_MAX_JOBS: usize = 64;
const GROUP_COMMIT_MAX_BYTES: usize = 8 * 1024 * 1024;

/// The writer's own opportunistic `partition.meta` persistence cadence
/// (PLAN-M2 §1a: "co `hw_persist_interval` (500 ms, kadencja heartbeatu)").
/// Applied only when `high_watermark` has actually changed since the last
/// persist and only right after the writer thread has done other work —
/// see `writer_loop`'s doc on why an idle writer needs no separate timer.
const META_PERSIST_INTERVAL: Duration = Duration::from_millis(500);

/// Durability policy applied after each batch append (PLAN §5.3.5).
#[derive(Debug, Clone, Copy)]
pub enum Durability {
    /// No explicit fsync; rely on the OS to flush eventually.
    Os,
    /// fsync (`fdatasync`-equivalent) after every group — the default in
    /// Prod. On Apple platforms this pushes data to the drive's cache, not
    /// necessarily the platter/NAND itself.
    FsyncBatch,
    /// Like `FsyncBatch`, but on macOS/iOS uses `fcntl(F_FULLFSYNC)` to
    /// force the drive to flush its own write cache — the stronger
    /// durability barrier PLAN §5.3.6 calls out, falling back to the same
    /// behavior as `FsyncBatch` on every other platform.
    FsyncBatchFull,
    /// fsync at most once per interval, batching fsyncs across appends.
    FsyncInterval(Duration),
}

/// How `Partition::high_watermark` is advanced (M2, PLAN-M2 §1a). Every
/// partition starts in `FollowLeo` (`Partition::open`'s doc), which is
/// exactly M1's `hw == leo` behavior: nothing else needs to change for a
/// partition nobody ever hands to a `ReplicationCoordinator` (RF=1).
///
/// Invariant that holds under *either* mode: `high_watermark` never
/// decreases and never exceeds `log_end_offset` — `set_high_watermark`
/// enforces both unconditionally, regardless of tracking mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HwTracking {
    /// The writer thread advances `high_watermark` to `log_end_offset` on
    /// every successful append (`process_group`'s original, M1 behavior).
    FollowLeo,
    /// The writer thread never advances `high_watermark` on its own;
    /// only an explicit `Partition::set_high_watermark` call does. Set by
    /// a `ReplicationCoordinator` once this partition has a real leader or
    /// follower role — a leader drives it from ISR acknowledgement, a
    /// follower drives it from the leader's `hw` field on incoming
    /// frames. `Partition::append_replicated` never advances
    /// `high_watermark` either way (see its own doc) — a follower's own
    /// append landing is not by itself evidence of quorum.
    Manual,
}

#[derive(Debug, Clone, Copy)]
pub struct AppendResult {
    pub base_offset: u64,
    pub segment_base_offset: u64,
    pub file_pos: u64,
}

/// One batch's exact on-disk bytes (header + body, verbatim — the same
/// buffer `PartitionReader::fetch_raw_from_offset` read off the segment
/// file with zero re-encoding), plus the header fields a replication
/// feeder needs without decoding `bytes` itself. `next_offset` is what
/// `Partition::append_replicated`'s `expected_base_offset` argument (or,
/// on the wire, `ReplBatchHeader::base_offset`, PLAN-M2 §1b) must equal
/// for the *following* batch — i.e. this batch's own
/// `base_offset + record_count`.
#[derive(Debug, Clone)]
pub struct RawBatch {
    pub base_offset: u64,
    pub record_count: u32,
    pub next_offset: u64,
    pub bytes: Bytes,
}

/// One sealed (rolled, immutable) segment's retention-relevant facts,
/// returned by `Partition::sealed_segments` (M1 retention.rs, PLAN §2.5).
/// Age is deliberately NOT tracked here — the caller derives it from
/// `log_path`'s filesystem mtime, since a rolled segment is never written
/// to again and its mtime is exactly the time its last batch landed.
#[derive(Debug, Clone)]
pub struct SealedSegmentInfo {
    pub base_offset: u64,
    pub len: u64,
    pub log_path: PathBuf,
}

/// One command on the writer thread's channel (M2, PLAN-M2 §1a). `Append`
/// and `AppendReplicated` are grouped and group-committed together (see
/// `drain_append_group`); `Truncate` and `PersistMeta` are each handled on
/// their own, never batched with anything else, but still strictly
/// serialized against every append by virtue of sharing this one channel.
enum WriterCommand {
    Append {
        batch: Bytes,
        resp: oneshot::Sender<Result<AppendResult>>,
    },
    /// Follower-side append (`Partition::append_replicated`): `batch` is
    /// written to disk exactly as received, with no `patch_base_offset`
    /// call — see `append_one`'s `AppendKind::Replicated` handling for the
    /// `header.base_offset`/`leader_epoch` checks this relies on instead.
    AppendReplicated {
        batch: Bytes,
        leader_epoch: u32,
        resp: oneshot::Sender<Result<AppendResult>>,
    },
    /// `resp` is a plain `std::sync::mpsc::SyncSender`, not tokio's
    /// `oneshot::Sender` — `Truncate`/`PersistMeta` have no async twin in
    /// the frozen contract, so their callers (`Partition::
    /// truncate_to_offset`/`flush_meta`, and `set_leader_epoch` through
    /// the latter) may be running on a Tokio task with no way to `.await`
    /// a reply; tokio's own blocking primitives panic in that situation,
    /// a plain std channel does not. See
    /// `send_and_wait_via_writer_thread`'s doc.
    Truncate {
        to_offset: u64,
        resp: std_mpsc::SyncSender<Result<u64>>,
    },
    PersistMeta {
        resp: std_mpsc::SyncSender<Result<()>>,
    },
}

/// What kind of append `append_one` is performing — the one place the
/// "patch the placeholder header" (`Append`) and "write the bytes as
/// received" (`Replicated`, zero-copy) paths diverge.
enum AppendKind {
    Fresh(Bytes),
    Replicated { batch: Bytes, leader_epoch: u32 },
}

/// One `Append`/`AppendReplicated` command, converted out of
/// `WriterCommand` at the point it is drained into a group — everything
/// `process_group`/`append_one` need, independent of which of the two wire
/// commands it started life as.
struct AppendJob {
    kind: AppendKind,
    resp: oneshot::Sender<Result<AppendResult>>,
}

/// Read-side view of one segment, shared between the writer thread (which
/// updates `len` and appends to the index `Arc`s) and every `PartitionReader`
/// (which only ever reads them). No data is duplicated between the writer's
/// own `OffsetIndex`/`TimeIndex` and this descriptor — both hold clones of
/// the same `Arc<RwLock<Vec<_>>>`. Held behind `Arc` so a reader can
/// snapshot the segment list with a cheap refcount-bump clone instead of
/// holding `PartitionState::segments`'s lock across file I/O.
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
    /// per segment. The writer thread never touches this —
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
    /// M2 (PLAN-M2 §1a): the offset up to which records are visible to
    /// consumers (`PartitionReader::fetch_from_offset` bounds on this, not
    /// on `log_end_offset`). Backed by its own atomic instead of aliasing
    /// `log_end_offset` so a leader can gate visibility on ISR
    /// acknowledgement (wave 1, agent RL) rather than on the local fsync
    /// alone. The writer thread (`process_group`) advances it to
    /// `log_end_offset` on every successful append by default — the same
    /// "everything durable is immediately visible" behavior M0/M1 had —
    /// so a partition nobody calls `set_high_watermark` on (RF=1, no
    /// `ReplicationCoordinator`) stays bit-for-bit identical to M1.
    high_watermark: AtomicU64,
    /// M2 (PLAN-M2 §1a): `false` (`HwTracking::FollowLeo`) until a
    /// `ReplicationCoordinator` calls `Partition::set_hw_tracking(Manual)`.
    /// `process_group`'s publish step only auto-advances `high_watermark`
    /// to `log_end_offset` while this is `false` — see `HwTracking`'s doc.
    hw_manual: AtomicBool,
    /// M2 (PLAN-M2 §1a): the epoch of the leader this partition currently
    /// recognizes. `0` until a `ReplicationCoordinator` calls
    /// `set_leader_epoch`. Persisted to `partition.meta` — see
    /// `Partition::flush_meta`/`meta.rs`.
    leader_epoch: AtomicU32,
    /// M2 (PLAN-M2 §1a): wakes replication feeders (`subscribe_leo`)
    /// without polling. Published alongside `log_end_offset` in
    /// `process_group`, right after the atomic itself so a receiver that
    /// observes a change is guaranteed the corresponding bytes are already
    /// visible to readers gated on `log_end_offset`/`high_watermark`.
    leo_watch_tx: tokio::sync::watch::Sender<u64>,
    /// Set once by the writer thread after an unrecoverable panic (caught by
    /// the `catch_unwind` defense in `writer_loop`) and checked by
    /// `append_batch`/`append_batch_async` before ever touching the
    /// channel, so a caller
    /// gets a specific `WriterPoisoned` instead of the generic
    /// `WriterClosed` a dropped channel would otherwise report.
    poisoned: AtomicBool,
    /// Set once by a group-commit fsync failure that straddled a segment
    /// roll (see `BusError::PartitionPoisoned`). Checked by every
    /// operation that reads or advances committed state, alongside
    /// `poisoned` — kept as a separate flag because the cause (and the
    /// caller-facing error) is different: the writer thread here is not
    /// panicking, it is refusing to risk reusing an offset.
    fsync_poisoned: AtomicBool,
    /// Set once by `Partition::detach` (the owning topic/organization was
    /// deleted). Every read/write operation checks this before touching
    /// `segments`, which `detach` has cleared.
    detached: AtomicBool,
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

/// `Segment::create_new`'s `prealloc_bytes` argument for a given policy —
/// `0` (no preallocation) unless `RollPolicy::preallocate` opts back in,
/// which `Segment::create_new` already treats as "skip preallocation
/// entirely" (M1-R2 decision 5, `RollPolicy::preallocate`'s doc).
fn prealloc_bytes(roll_policy: &RollPolicy) -> u64 {
    if roll_policy.preallocate {
        roll_policy.max_bytes
    } else {
        0
    }
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
    // tail so a segment that rolled early does not permanently occupy
    // `max_bytes` on disk.
    handles.active_segment.fsync()?;
    handles.active_segment.truncate_to_len()?;

    let next_base_offset = state.log_end_offset.load(Ordering::Acquire);
    let new_segment = Segment::create_new(dir, next_base_offset, prealloc_bytes(roll_policy))?;
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

    // Best-effort (PLAN-M2 §1a: persisted "przy roll()"): a roll is
    // infrequent enough to afford this, and it keeps `partition.meta`
    // reasonably fresh without waiting for the writer's own 500 ms
    // cadence. Never fails the roll itself — a stale-but-still-safe
    // previously-persisted `hw` (never higher than what was ever true) is
    // no worse than what every other restart-durability gap in this
    // module already tolerates (`Partition::open`'s doc).
    if let Err(e) = persist_meta_now(dir, state) {
        tracing::warn!(
            path = %dir.display(),
            error = %e,
            "failed to persist partition.meta at segment roll; continuing"
        );
    }
    Ok(())
}

/// Snapshots `high_watermark`/`leader_epoch`/`log_end_offset` and writes
/// them to `partition.meta` (`meta.rs`). Called only from the writer
/// thread (directly by `roll()` and the writer's own shutdown/periodic
/// paths, or indirectly via `WriterCommand::PersistMeta`) so two writers
/// can never race the same tmp path — see `meta.rs`'s doc.
fn persist_meta_now(dir: &Path, state: &PartitionState) -> Result<()> {
    let meta = crate::meta::PartitionMeta {
        high_watermark: state.high_watermark.load(Ordering::Acquire),
        leader_epoch: state.leader_epoch.load(Ordering::Acquire),
        leo_hint: state.log_end_offset.load(Ordering::Acquire),
    };
    crate::meta::write_meta(dir, &meta)
}

/// One job's outcome after its positional write and index updates, but
/// before the group's fsync/publish step — everything needed to finish
/// publishing it once the group's durability barrier has been decided.
struct Landed {
    desc: Arc<SegmentDescriptor>,
    /// This job's active-segment length immediately before its append —
    /// the value `process_group` rolls the segment back to if the group's
    /// fsync subsequently fails.
    pos_before: u64,
    len_after: u64,
    next_offset: u64,
    append_result: AppendResult,
    /// `true` for `AppendKind::Replicated` (M2, PLAN-M2 §1a): a follower's
    /// own append landing is not evidence of quorum, so `process_group`'s
    /// publish step must never let this job advance `high_watermark`,
    /// regardless of `HwTracking` mode — only an explicit
    /// `set_high_watermark` call (driven by the leader's own `hw`) may.
    skip_hw: bool,
}

/// Appends one batch: rolls the segment if needed, decodes+patches the
/// header, writes it, and updates both sparse indexes. Does *not* fsync or
/// publish anything visible to readers — that is the group's job, done once
/// for every job landed in this call (see `process_group`).
///
/// `base_offset` is passed in rather than read from
/// `state.log_end_offset` because group commit defers publishing that
/// atomic until *after* the whole group has been appended and fsynced —
/// if every job in a group re-read the same not-yet-advanced shared
/// counter, every job after the first would be
/// assigned the same offset as the first, silently colliding. The caller
/// (`process_group`) threads a local cursor through successive calls
/// instead.
///
/// `kind` selects between `Append`'s "decode, then patch the placeholder
/// header to `base_offset`" path and `AppendReplicated`'s zero-copy path:
/// the batch is written exactly as received (`patch_base_offset` is never
/// called), after independently verifying its own header's `base_offset`
/// against `base_offset` (`OffsetMismatch`) and its `leader_epoch` against
/// this partition's own (`LeaderEpochStale`) — the authoritative check for
/// a replicated append; `Partition::append_replicated`'s pre-check against
/// `log_end_offset` before ever submitting to this thread is a fast-fail
/// convenience only, not a substitute for this one, which runs against
/// whatever the cursor actually is *at the moment this job is processed*.
fn append_one(
    dir: &Path,
    roll_policy: &RollPolicy,
    state: &PartitionState,
    handles: &mut WriterHandles,
    kind: AppendKind,
    base_offset: u64,
) -> Result<Landed> {
    if handles.active_segment.should_roll(roll_policy) {
        roll(dir, roll_policy, state, handles)?;
    }

    let (header, patched, skip_hw) = match kind {
        AppendKind::Fresh(batch) => {
            // Decoding against the whole buffer (not a pre-sliced
            // `&batch[..40]`) means a short buffer is rejected by
            // `BatchHeader::decode`'s own length check instead of
            // panicking at the slice expression itself.
            let header = BatchHeader::decode(&batch)?;
            let patched = patch_base_offset(batch, base_offset);
            (header, patched, false)
        }
        AppendKind::Replicated {
            batch,
            leader_epoch,
        } => {
            let header = BatchHeader::decode(&batch)?;
            if header.base_offset != base_offset {
                return Err(BusError::OffsetMismatch {
                    expected: base_offset,
                    got: header.base_offset,
                });
            }
            let have_epoch = state.leader_epoch.load(Ordering::Acquire);
            if leader_epoch < have_epoch {
                return Err(BusError::LeaderEpochStale {
                    have: have_epoch,
                    got: leader_epoch,
                });
            }
            // Zero-copy: `batch` is written verbatim, no
            // `patch_base_offset` call — its header already carries the
            // exact `base_offset` the leader assigned, just verified above.
            (header, batch, true)
        }
    };

    let segment_base_offset = handles.active_segment.base_offset();
    let pos_before = handles.active_segment.len();
    let file_pos = handles.active_segment.append(&patched)?;

    // `base_offset` must never be behind the active segment's own base —
    // that would mean the whole partition's offset bookkeeping is already
    // corrupt. Turned into a hard error instead of a panicking or
    // silently-wrapping subtraction.
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
    // leaving a gap the offset index does not know about.
    if let Err(e) = handles.active_offset_index.append(OffsetEntry {
        offset_delta,
        file_pos: file_pos_u32,
    }) {
        handles.active_segment.rollback_len(pos_before, 1);
        return Err(e);
    }
    if let Err(e) = handles.active_time_index.append(TimeEntry {
        ts_ms: header.base_timestamp_ms,
        offset_delta,
    }) {
        handles.active_segment.rollback_len(pos_before, 1);
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
        pos_before,
        len_after,
        next_offset,
        append_result: AppendResult {
            base_offset,
            segment_base_offset,
            file_pos,
        },
        skip_hw,
    })
}

/// Decides how a group's fsync failure must be handled, given every
/// successfully-appended job's `(segment_base_offset, pos_before)` and the
/// segment that is *currently* active: which bytes to physically roll back
/// (the lowest `pos_before` among jobs on the active segment, since that
/// segment's durability genuinely depends on the fsync that just failed),
/// and whether any job landed on a segment already rolled away earlier in
/// this same group — that segment's bytes are independently durable
/// (`roll()` fsyncs the outgoing segment before this ever runs), but this
/// group cannot safely publish that job's offset either way, so the caller
/// must poison the partition instead of risking that offset getting reused.
/// A free function (rather than inlined into `process_group`) purely so
/// this decision is testable without real file I/O or a genuine fsync
/// failure, neither of which this crate has a way to force deterministically.
fn plan_fsync_failure_response(
    landed_ok: &[(u64, u64)],
    active_base: u64,
) -> (Option<(u64, u32)>, bool) {
    let mut rollback: Option<(u64, u32)> = None;
    let mut straddled = false;
    for &(segment_base_offset, pos_before) in landed_ok {
        if segment_base_offset == active_base {
            rollback = Some(match rollback {
                Some((len, count)) => (len.min(pos_before), count + 1),
                None => (pos_before, 1),
            });
        } else {
            straddled = true;
        }
    }
    (rollback, straddled)
}

/// Processes one drained group of jobs: appends every job's batch (each
/// independently succeeding or failing), performs exactly one fsync for the
/// whole group per the durability policy, then publishes every successful
/// job's visible state (segment length, then high watermark, in exactly
/// that order) and acks every job.
///
/// Returns `true` if this group's fsync failure straddled a segment roll —
/// i.e. the group appended to a segment, rolled it, and then failed to
/// fsync the new active segment while jobs from the rolled-away segment
/// were also part of this group. The caller must stop this writer thread
/// for good in that case (see `writer_loop` and `BusError::PartitionPoisoned`).
fn process_group(
    dir: &Path,
    roll_policy: &RollPolicy,
    durability: Durability,
    state: &PartitionState,
    handles: &mut WriterHandles,
    last_fsync: &mut Instant,
    jobs: Vec<AppendJob>,
) -> bool {
    let mut cursor = state.log_end_offset.load(Ordering::Acquire);
    let mut landed: Vec<(oneshot::Sender<Result<AppendResult>>, Result<Landed>)> =
        Vec::with_capacity(jobs.len());
    for job in jobs {
        // Metrics (PLAN §8.4): wall duration of one append attempt,
        // success or failure, feeding the `tentaflow_bus_append_p99_us`
        // reservoir. Only runs here — once per job actually drained off
        // the writer channel — so an idle writer records nothing.
        let append_start = Instant::now();
        let outcome = append_one(dir, roll_policy, state, handles, job.kind, cursor);
        crate::metrics::record_append_us(append_start.elapsed().as_micros() as u64);
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
    // Metrics (PLAN §8.4): each branch below records `fsync_us` only when
    // it actually calls `fsync`/`fsync_full` — `Durability::Os` never
    // fsyncs, and `FsyncInterval` skips it between ticks — so the
    // `tentaflow_bus_fsync_p99_us` reservoir stays empty on a durability
    // policy or cadence that never runs an fsync.
    let fsync_err: Option<String> = if any_ok {
        match durability {
            Durability::Os => None,
            Durability::FsyncBatch => {
                let fsync_start = Instant::now();
                let r = handles.active_segment.fsync().err().map(|e| e.to_string());
                crate::metrics::record_fsync_us(fsync_start.elapsed().as_micros() as u64);
                r
            }
            Durability::FsyncBatchFull => {
                let fsync_start = Instant::now();
                let r = handles
                    .active_segment
                    .fsync_full()
                    .err()
                    .map(|e| e.to_string());
                crate::metrics::record_fsync_us(fsync_start.elapsed().as_micros() as u64);
                r
            }
            Durability::FsyncInterval(interval) => {
                if last_fsync.elapsed() >= interval {
                    let fsync_start = Instant::now();
                    let r = handles.active_segment.fsync().err().map(|e| e.to_string());
                    crate::metrics::record_fsync_us(fsync_start.elapsed().as_micros() as u64);
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

    let mut straddled_poison = false;
    if let Some(message) = fsync_err {
        let path = handles.active_segment.path().to_path_buf();
        let active_base = handles.active_segment.base_offset();

        // Every job in this group that landed on the segment which is
        // *currently* active is about to be reported to its producer as
        // failed below — roll the segment's logical length back to the
        // earliest of those jobs' `pos_before` (same discipline as
        // `append_one`'s index-error branches) so those bytes are not
        // recoverable as valid log content after a restart: without this,
        // `active_segment.len()` stays advanced past this point, so
        // crash-recovery's tail scan would pick the "failed" batches back
        // up as legitimate on the next `Partition::open`, silently
        // contradicting the error every producer in this group just
        // received. A job that landed on a segment rolled *earlier*
        // in this same group is unaffected: `roll()` already fsyncs and
        // truncates the outgoing segment before this group's fsync ever
        // runs, so that segment's bytes are durable independent of this
        // failure and must not be rolled back.
        // A job that landed on a segment rolled earlier in this same group
        // is durable (see above) but this group never publishes
        // `log_end_offset` past it below (that publish only happens for
        // jobs still `Ok` once this whole `if` finishes, and every `Ok`
        // entry here is about to become `Err`). Left as-is, the *next*
        // append would read the stale `log_end_offset` and reuse that
        // job's offset for different bytes on a different segment —
        // exactly the same-`base_offset`-twice corruption `Landed`'s doc
        // above the caller warns about. There is no safe way to finish
        // publishing only part of this group after the fact from here, so
        // this partition is poisoned instead: every future append fails
        // until the directory is reopened, whose crash recovery starts
        // clean from the last fully-published group.
        let landed_on_active: Vec<(u64, u64)> = landed
            .iter()
            .filter_map(|(_, r)| r.as_ref().ok())
            .map(|l| (l.append_result.segment_base_offset, l.pos_before))
            .collect();
        let (rollback, straddled) = plan_fsync_failure_response(&landed_on_active, active_base);
        straddled_poison = straddled;
        if let Some((len, count)) = rollback {
            handles.active_segment.rollback_len(len, count);
        }

        if straddled_poison {
            state.fsync_poisoned.store(true, Ordering::Release);
            tracing::error!(
                path = %path.display(),
                message = %message,
                "group-commit fsync failed after a segment roll within the group; \
                 partition is now poisoned to avoid reusing offsets, reopen it to resume writing"
            );
        }

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
            // Segment length is published *before* the high watermark: a
            // reader gates on `high_watermark()` first, then reads up to
            // `desc.len` — publishing in the other order would let a reader
            // observe `high_watermark() > from_offset` for an instant
            // before the corresponding bytes were visible, which
            // `fetch_from_offset`'s "always returns at least one batch"
            // contract does not allow for.
            l.desc.len.store(l.len_after, Ordering::Release);
            state.log_end_offset.store(l.next_offset, Ordering::Release);
            // M2 (PLAN-M2 §1a): `high_watermark` only auto-tracks
            // `log_end_offset` (`fetch_max`, never decreases) while this
            // partition is in `HwTracking::FollowLeo` (the default — a
            // partition with no active `ReplicationCoordinator`, RF=1,
            // stays exactly `hw == leo`, M1 behavior unchanged) — and
            // never for a replicated job regardless of tracking mode
            // (`skip_hw`, see `Landed`'s doc: a follower's own append
            // landing is not evidence of quorum). Once a
            // `ReplicationCoordinator` switches this partition to `Manual`
            // (`Partition::set_hw_tracking`), only an explicit
            // `set_high_watermark` call advances it.
            if !l.skip_hw && !state.hw_manual.load(Ordering::Acquire) {
                state
                    .high_watermark
                    .fetch_max(l.next_offset, Ordering::AcqRel);
            }
            // Published regardless of `HwTracking`/`skip_hw`: `subscribe_leo`
            // tracks `log_end_offset`, not `high_watermark` — see its own
            // doc. This send happens strictly after the `log_end_offset`
            // store above and strictly after the fsync this whole
            // publish loop only runs once decided, so a receiver that
            // observes this change is guaranteed the corresponding bytes
            // are already durable (group-commit fsync) and visible to
            // readers (`desc.len` was just published above too).
            let _ = state.leo_watch_tx.send(l.next_offset);
            l.append_result
        });
        let _ = resp.send(result);
    }

    straddled_poison
}

/// `boundary.new_len` as a `u32` file position, for `OffsetIndex::
/// truncate_to_file_pos`. `boundary` always came from a real segment
/// (bounded by `RollPolicy::max_bytes`, itself validated `<= u32::MAX` by
/// `Partition::open`), so this only fails if that invariant is somehow
/// violated — turned into a hard error rather than a silent truncating
/// cast, the same discipline `append_one` uses for `file_pos`/
/// `offset_delta`.
fn truncate_boundary_len_u32(boundary: &segment::TruncateBoundary) -> Result<u32> {
    u32::try_from(boundary.new_len).map_err(|_| BusError::PositionOverflow {
        field: "new_len",
        pos: boundary.new_len,
    })
}

/// `boundary.new_next_offset - segment_base_offset` as a `u32` offset
/// delta, for `TimeIndex::truncate_to_offset_delta`. `new_next_offset` is
/// always `>= segment_base_offset` by construction (`scan_truncate_
/// boundary` only ever advances it forward from that starting point), so
/// `checked_sub` here guards a genuinely-should-never-happen invariant
/// violation rather than a case this function's caller needs to handle.
fn truncate_boundary_delta_u32(
    boundary: &segment::TruncateBoundary,
    segment_base_offset: u64,
) -> Result<u32> {
    let delta = boundary
        .new_next_offset
        .checked_sub(segment_base_offset)
        .ok_or(BusError::OffsetChainCorrupt {
            log_end_offset: boundary.new_next_offset,
            segment_base_offset,
        })?;
    u32::try_from(delta).map_err(|_| BusError::PositionOverflow {
        field: "offset_delta",
        pos: delta,
    })
}

/// Executes `WriterCommand::Truncate` (M2, PLAN-M2 §1a) on the writer
/// thread — serialized against every append by construction (M2-R3), since
/// both travel the same channel. Refuses to discard anything at or below
/// `high_watermark` (`TruncateBelowHighWatermark`: those records may
/// already be visible to a consumer). Otherwise finds the batch boundary
/// at or before `to_offset` (which may land inside the still-open active
/// segment, or inside an already-sealed one — `segment::
/// scan_truncate_boundary`'s doc on why a straddling batch is dropped
/// whole), deletes every segment entirely past that boundary, and
/// truncates the boundary segment itself, promoting it to the new active
/// segment if it was not already one. Returns the resulting
/// `log_end_offset`.
fn truncate(
    dir: &Path,
    state: &PartitionState,
    handles: &mut WriterHandles,
    to_offset: u64,
) -> Result<u64> {
    let hw = state.high_watermark.load(Ordering::Acquire);
    if to_offset < hw {
        return Err(BusError::TruncateBelowHighWatermark { hw, to: to_offset });
    }
    let leo = state.log_end_offset.load(Ordering::Acquire);
    if to_offset >= leo {
        // Nothing to discard — the request is already satisfied.
        return Ok(leo);
    }

    let mut segments = state.segments.write();
    let target_idx = segments
        .partition_point(|s| s.base_offset <= to_offset)
        .saturating_sub(1);
    let target_base = segments[target_idx].base_offset;
    let target_was_active = target_idx == segments.len() - 1;

    // Delete every segment strictly after the target one — file, both
    // index siblings, and the live descriptor — highest `base_offset`
    // first, so a crash mid-loop never leaves a higher-numbered segment
    // on disk whose predecessor is already gone. This always includes the
    // *previous* active segment whenever `target_idx` lands on an
    // already-sealed segment.
    while segments.len() > target_idx + 1 {
        let desc = segments
            .pop()
            .expect("segments.len() > target_idx + 1 checked by the loop condition");
        let _ = std::fs::remove_file(segment::offset_index_path(dir, desc.base_offset));
        let _ = std::fs::remove_file(segment::time_index_path(dir, desc.base_offset));
        std::fs::remove_file(&desc.log_path).map_err(|e| BusError::io(&desc.log_path, e))?;
    }

    let new_leo = if target_was_active {
        let boundary = segment::scan_truncate_boundary(
            handles.active_segment.file(),
            target_base,
            handles.active_segment.len(),
            to_offset,
        );
        handles.active_segment.truncate_to_boundary(&boundary)?;
        let new_len_u32 = truncate_boundary_len_u32(&boundary)?;
        let new_delta_u32 = truncate_boundary_delta_u32(&boundary, target_base)?;
        handles
            .active_offset_index
            .truncate_to_file_pos(new_len_u32)?;
        handles
            .active_time_index
            .truncate_to_offset_delta(new_delta_u32)?;
        segments[target_idx]
            .len
            .store(boundary.new_len, Ordering::Release);
        // The file's own content/length just changed underneath any
        // reader that had already cached an fd for it; force a reopen so
        // a subsequent `PartitionReader::reader_file()` picks it up (reads
        // are bounded by `desc.len`, which is already updated above, so
        // this is a belt-and-suspenders reset, not a correctness gap on
        // its own).
        *segments[target_idx].reader_fd.lock() = None;
        boundary.new_next_offset
    } else {
        // `target_idx` was a sealed (read-only) segment until now; every
        // segment after it — including the old active one — was just
        // deleted above. Reopen it for writing and rebuild fresh index
        // handles to promote it to the new active segment.
        let mut reopened = Segment::reopen_for_write(dir, target_base)?;
        let boundary = segment::scan_truncate_boundary(
            reopened.file(),
            target_base,
            reopened.len(),
            to_offset,
        );
        reopened.truncate_to_boundary(&boundary)?;
        let new_len_u32 = truncate_boundary_len_u32(&boundary)?;
        let new_delta_u32 = truncate_boundary_delta_u32(&boundary, target_base)?;
        let mut oidx = OffsetIndex::open_or_create(segment::offset_index_path(dir, target_base))?;
        let mut tidx = TimeIndex::open_or_create(segment::time_index_path(dir, target_base))?;
        oidx.truncate_to_file_pos(new_len_u32)?;
        tidx.truncate_to_offset_delta(new_delta_u32)?;

        segments[target_idx] = Arc::new(SegmentDescriptor {
            base_offset: target_base,
            log_path: segment::log_path(dir, target_base),
            len: Arc::new(AtomicU64::new(boundary.new_len)),
            offset_entries: oidx.shared(),
            time_entries: tidx.shared(),
            reader_fd: Mutex::new(None),
        });

        handles.active_segment = reopened;
        handles.active_offset_index = oidx;
        handles.active_time_index = tidx;
        boundary.new_next_offset
    };
    drop(segments);

    state.log_end_offset.store(new_leo, Ordering::Release);
    let _ = state.leo_watch_tx.send(new_leo);

    // Rare and critical, like a leader-epoch change (PLAN-M2 §1a: "fix leo
    // + index + watch, persist meta") — persisted synchronously rather
    // than waiting for the writer's periodic cadence so a crash right
    // after a truncate cannot resurrect the discarded tail on reopen.
    if let Err(e) = persist_meta_now(dir, state) {
        tracing::warn!(
            path = %dir.display(),
            error = %e,
            "failed to persist partition.meta after truncate_to_offset; continuing"
        );
    }

    Ok(new_leo)
}

/// What `writer_loop`'s dispatch of one drained group/command decided:
/// keep serving future commands, or stop the writer thread for good.
enum LoopControl {
    Continue,
    Stop,
}

/// Runs one drained group of `Append`/`AppendReplicated` jobs through
/// `process_group`, wrapped in the same `catch_unwind` defense-in-depth
/// this path always applied: a genuine bug here must leave the partition
/// refusing further writes rather than risk continuing with state that may
/// be inconsistent partway through a group.
fn run_append_group(
    dir: &Path,
    roll_policy: &RollPolicy,
    durability: Durability,
    state: &PartitionState,
    handles: &mut WriterHandles,
    last_fsync: &mut Instant,
    jobs: Vec<AppendJob>,
) -> LoopControl {
    // `&mut handles`/`&mut last_fsync` are not `UnwindSafe` by default
    // (mutable references), hence the explicit assertion.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        process_group(
            dir,
            roll_policy,
            durability,
            state,
            handles,
            last_fsync,
            jobs,
        )
    }));
    match result {
        Err(_) => {
            state.poisoned.store(true, Ordering::Release);
            tracing::error!(
                "partition writer thread panicked; partition is now poisoned and accepts no further writes"
            );
            LoopControl::Stop
        }
        // `process_group` already set `state.fsync_poisoned` and logged
        // why; stop taking further commands so nothing else can be
        // appended (and no offset reused) by this writer thread before the
        // directory is reopened. Every already-checked-in producer in this
        // last group has already been acked with the result
        // `process_group` decided (success, `FsyncFailed`, or the
        // survivors of the rollback) — new callers get `PartitionPoisoned`
        // from `append_batch`/`append_batch_async` before ever reaching
        // this channel.
        Ok(true) => LoopControl::Stop,
        Ok(false) => LoopControl::Continue,
    }
}

/// Drains up to the group-commit budget from `rx` into `jobs`, but only
/// while the next queued command is itself an append (`Append` or
/// `AppendReplicated` — both group-commit together below). A `Truncate`/
/// `PersistMeta` command pulled off the channel by `try_recv` while
/// looking for more appends is handed back via the returned `Option`
/// instead of being silently dropped or reordered behind appends that
/// arrived *after* it on the channel.
fn drain_append_group(
    rx: &mut mpsc::Receiver<WriterCommand>,
    jobs: &mut Vec<AppendJob>,
    group_bytes: &mut usize,
) -> Option<WriterCommand> {
    while jobs.len() < GROUP_COMMIT_MAX_JOBS && *group_bytes < GROUP_COMMIT_MAX_BYTES {
        match rx.try_recv() {
            Ok(WriterCommand::Append { batch, resp }) => {
                *group_bytes += batch.len();
                jobs.push(AppendJob {
                    kind: AppendKind::Fresh(batch),
                    resp,
                });
            }
            Ok(WriterCommand::AppendReplicated {
                batch,
                leader_epoch,
                resp,
            }) => {
                *group_bytes += batch.len();
                jobs.push(AppendJob {
                    kind: AppendKind::Replicated {
                        batch,
                        leader_epoch,
                    },
                    resp,
                });
            }
            Ok(other) => return Some(other),
            Err(_) => return None,
        }
    }
    None
}

/// Runs `truncate` wrapped in the same `catch_unwind` defense-in-depth
/// `run_append_group` applies to appends, and sends its result (or
/// `WriterPoisoned` on a caught panic) to `resp`.
fn handle_truncate_command(
    dir: &Path,
    state: &PartitionState,
    handles: &mut WriterHandles,
    to_offset: u64,
    resp: std_mpsc::SyncSender<Result<u64>>,
) -> LoopControl {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        truncate(dir, state, handles, to_offset)
    }));
    match result {
        Err(_) => {
            state.poisoned.store(true, Ordering::Release);
            tracing::error!(
                "partition writer thread panicked during truncate_to_offset; partition is now poisoned and accepts no further writes"
            );
            let _ = resp.send(Err(BusError::WriterPoisoned));
            LoopControl::Stop
        }
        Ok(r) => {
            let _ = resp.send(r);
            LoopControl::Continue
        }
    }
}

/// Persists `partition.meta` and, on success, refreshes the writer's own
/// `last_meta_flush`/`last_persisted_hw` bookkeeping — shared between the
/// explicit `WriterCommand::PersistMeta` handler and the periodic
/// piggyback at the bottom of `writer_loop`'s main loop.
fn persist_meta_and_track(
    dir: &Path,
    state: &PartitionState,
    last_meta_flush: &mut Instant,
    last_persisted_hw: &mut u64,
) -> Result<()> {
    let result = persist_meta_now(dir, state);
    if result.is_ok() {
        *last_meta_flush = Instant::now();
        *last_persisted_hw = state.high_watermark.load(Ordering::Acquire);
    }
    result
}

/// Drives the partition's single writer thread. Every `WriterCommand`
/// (`Append`/`AppendReplicated`/`Truncate`/`PersistMeta`) passes through
/// this one channel and is handled here — the single point that serializes
/// `truncate_to_offset` against group commit (PLAN-M2 §4.2 M2-R3) and every
/// `partition.meta` write against every other one (`meta.rs`'s doc).
///
/// The 500 ms `partition.meta` persistence cadence (PLAN-M2 §1a) is
/// applied opportunistically right after whatever this loop iteration just
/// did, rather than on a dedicated timer thread: an idle writer (blocked in
/// `rx.blocking_recv()` below) has no in-flight `high_watermark` change to
/// miss, because every way `high_watermark` can change in this build
/// (`AppendReplicated`/`Truncate` applying a leader's frames, or a direct
/// `flush_meta`/`PersistMeta` call) itself keeps this thread busy — there
/// is no code path that advances `high_watermark` without also sending
/// this thread a command.
fn writer_loop(
    dir: PathBuf,
    roll_policy: RollPolicy,
    durability: Durability,
    mut rx: mpsc::Receiver<WriterCommand>,
    state: Arc<PartitionState>,
    mut handles: WriterHandles,
) {
    let mut last_fsync = Instant::now();
    let mut last_meta_flush = Instant::now();
    let mut last_persisted_hw = state.high_watermark.load(Ordering::Acquire);

    loop {
        let first = match rx.blocking_recv() {
            Some(cmd) => cmd,
            None => break,
        };

        let mut pending_control: Option<WriterCommand> = None;
        let control = match first {
            WriterCommand::Append { batch, resp } => {
                let mut group_bytes = batch.len();
                let mut jobs = vec![AppendJob {
                    kind: AppendKind::Fresh(batch),
                    resp,
                }];
                pending_control = drain_append_group(&mut rx, &mut jobs, &mut group_bytes);
                run_append_group(
                    &dir,
                    &roll_policy,
                    durability,
                    &state,
                    &mut handles,
                    &mut last_fsync,
                    jobs,
                )
            }
            WriterCommand::AppendReplicated {
                batch,
                leader_epoch,
                resp,
            } => {
                let mut group_bytes = batch.len();
                let mut jobs = vec![AppendJob {
                    kind: AppendKind::Replicated {
                        batch,
                        leader_epoch,
                    },
                    resp,
                }];
                pending_control = drain_append_group(&mut rx, &mut jobs, &mut group_bytes);
                run_append_group(
                    &dir,
                    &roll_policy,
                    durability,
                    &state,
                    &mut handles,
                    &mut last_fsync,
                    jobs,
                )
            }
            WriterCommand::Truncate { to_offset, resp } => {
                handle_truncate_command(&dir, &state, &mut handles, to_offset, resp)
            }
            WriterCommand::PersistMeta { resp } => {
                let result = persist_meta_and_track(
                    &dir,
                    &state,
                    &mut last_meta_flush,
                    &mut last_persisted_hw,
                );
                let _ = resp.send(result);
                LoopControl::Continue
            }
        };
        if matches!(control, LoopControl::Stop) {
            break;
        }

        // A non-append command `drain_append_group` pulled off the channel
        // while looking for more appends must still be handled this same
        // iteration — it was already removed from the channel and cannot
        // be put back.
        if let Some(cmd) = pending_control {
            let control = match cmd {
                WriterCommand::Truncate { to_offset, resp } => {
                    handle_truncate_command(&dir, &state, &mut handles, to_offset, resp)
                }
                WriterCommand::PersistMeta { resp } => {
                    let result = persist_meta_and_track(
                        &dir,
                        &state,
                        &mut last_meta_flush,
                        &mut last_persisted_hw,
                    );
                    let _ = resp.send(result);
                    LoopControl::Continue
                }
                WriterCommand::Append { .. } | WriterCommand::AppendReplicated { .. } => {
                    unreachable!("drain_append_group only ever hands back a non-append command")
                }
            };
            if matches!(control, LoopControl::Stop) {
                break;
            }
        }

        let hw_now = state.high_watermark.load(Ordering::Acquire);
        if hw_now != last_persisted_hw && last_meta_flush.elapsed() >= META_PERSIST_INTERVAL {
            let _ =
                persist_meta_and_track(&dir, &state, &mut last_meta_flush, &mut last_persisted_hw);
        }
    }

    // Best-effort: persist whatever `high_watermark`/`leader_epoch` this
    // partition last knew (PLAN-M2 §1a: persisted "przy ... Drop" — every
    // shutdown path, including the panicked and fsync-poisoned ones,
    // `break`s down to here), then fsync so a clean shutdown leaves data
    // durable even under `Durability::Os`.
    if let Err(e) = persist_meta_now(&dir, &state) {
        tracing::warn!(
            path = %dir.display(),
            error = %e,
            "failed to persist partition.meta on writer shutdown"
        );
    }
    let _ = handles.active_segment.fsync();
}

struct PartitionInner {
    state: Arc<PartitionState>,
    tx: Option<mpsc::Sender<WriterCommand>>,
    writer_thread: Option<JoinHandle<()>>,
    throttle_hint_ms: u32,
    /// Held for the partition's lifetime purely for its `Drop` effect:
    /// closing this file releases the OS-level advisory lock acquired in
    /// `Partition::open`, so a second `Partition::open` on the same
    /// directory can only succeed after this one is gone.
    _lock: File,
}

/// Upper bound `Drop for PartitionInner` waits for the writer thread to
/// finish its current group and return after the channel closes. Ordinary
/// shutdown (nothing in flight, or a group that is already mid-fsync)
/// returns in well under this; the bound only guards against a writer
/// thread wedged on a stuck disk, so dropping the last `Partition`/
/// `PartitionReader` clone (e.g. from a retention sweeper that opens a
/// partition just to read it) can never hang the caller's thread forever.
const WRITER_SHUTDOWN_JOIN_TIMEOUT: Duration = Duration::from_secs(5);

impl Drop for PartitionInner {
    fn drop(&mut self) {
        // Dropping the sender closes the channel, which unblocks the
        // writer thread's `blocking_recv()` with `None` and ends its loop.
        self.tx.take();
        if let Some(handle) = self.writer_thread.take() {
            // `handle.join()` itself has no timeout, so a watcher thread
            // (not this one) blocks on it and reports back over a
            // `recv_timeout`-bounded channel. If the writer thread is
            // genuinely wedged, both it and this watcher leak rather than
            // making `Drop` — and therefore every caller dropping their
            // last handle — hang indefinitely. `_lock` (below, dropped
            // right after this method returns) is released either way, so
            // a fresh `Partition::open` on the same directory can proceed
            // even while a wedged writer thread is still leaked in the
            // background; that writer thread no longer holds the lock but
            // may still be mid-syscall against the directory's files, a
            // known, accepted risk of bounding the wait at all.
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let watcher = thread::Builder::new()
                .name("tentaflow-bus-partition-writer-join".into())
                .spawn(move || {
                    let _ = handle.join();
                    let _ = done_tx.send(());
                });
            if let Ok(watcher) = watcher {
                if done_rx.recv_timeout(WRITER_SHUTDOWN_JOIN_TIMEOUT).is_err() {
                    tracing::error!(
                        "partition writer thread did not exit within {WRITER_SHUTDOWN_JOIN_TIMEOUT:?} \
                         of shutdown; leaking it and its join watcher"
                    );
                } else {
                    let _ = watcher.join();
                }
            }
        }
    }
}

/// How long `send_writer_command_blocking` waits between `try_send` retries
/// while the writer's command queue is momentarily full. `Truncate`/
/// `PersistMeta` are rare, administrative operations (one per failover,
/// leader-epoch change, or explicit flush) racing a channel whose normal
/// occupant is the hot append path, so a short fixed backoff — rather than
/// `blocking_send`'s internal parking — is simplest and, unlike
/// `blocking_send`, never risks tokio's "cannot block from within a
/// runtime" panic (`try_send` does not check for a Tokio runtime at all).
const CONTROL_COMMAND_RETRY_INTERVAL: Duration = Duration::from_millis(1);

/// Sends `cmd` to the writer thread, retrying `try_send` until it succeeds
/// or the channel is closed. Deliberately never uses tokio's
/// `Sender::blocking_send`: that method panics with "Cannot block the
/// current thread from within a runtime" whenever called from *any* Tokio
/// runtime context (current-thread or multi-thread alike), which is
/// exactly the situation `Truncate`/`PersistMeta` callers are in when
/// `Partition::truncate_to_offset`/`flush_meta`/`set_leader_epoch` (all
/// frozen-contract sync fns with no async twin) are invoked from async
/// code such as `bus::replication::follower`. `try_send` performs no such
/// check — it is a plain, always-safe non-blocking attempt — so retrying
/// it in a loop sidesteps the panic entirely, at the cost of a small fixed
/// poll interval instead of being woken the instant a slot frees up; an
/// acceptable trade for operations this rare.
fn send_writer_command_blocking(
    tx: &mpsc::Sender<WriterCommand>,
    mut cmd: WriterCommand,
) -> Result<()> {
    loop {
        match tx.try_send(cmd) {
            Ok(()) => return Ok(()),
            Err(mpsc::error::TrySendError::Full(returned)) => {
                cmd = returned;
                std::thread::sleep(CONTROL_COMMAND_RETRY_INTERVAL);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => return Err(BusError::WriterClosed),
        }
    }
}

/// Runs `wait` (send `cmd` to the writer thread, then block on `resp_rx`
/// for its reply) either directly or wrapped in `tokio::task::
/// block_in_place`, depending on what — if anything — is currently driving
/// this thread.
///
/// This is the fix for a real bug (not a hypothetical one):
/// `Partition::truncate_to_offset`/`flush_meta`/`set_leader_epoch` are sync
/// fns with no async twin in the frozen contract (PLAN-M2 §1a), so a
/// caller already running on a Tokio task — `bus::replication::follower`'s
/// `Truncate`/`Hello` handling, in particular — has no way to avoid
/// calling them directly from async code. The previous implementation
/// waited via tokio's own `oneshot::Receiver::blocking_recv()`
/// (and `Sender::blocking_send()` for the send half), both of which panic
/// with "Cannot block the current thread from within a runtime" the moment
/// they detect *any* current Tokio runtime — current-thread flavor
/// included, not just multi-thread. The reply channel here is a plain
/// `std::sync::mpsc::SyncSender`/`Receiver` instead: neither has any
/// awareness of Tokio at all, so blocking on `Receiver::recv()` never
/// triggers that check, on any runtime or none.
///
/// What differs by runtime is only *how considerately* this blocks:
/// - No current Tokio runtime (a plain thread, e.g. most producers): block
///   directly. Nothing to be considerate of.
/// - A `current_thread` runtime (`#[tokio::test]`'s default flavor):
///   `block_in_place` is unavailable here — tokio documents it as a hard
///   panic on this flavor, since a `current_thread` runtime has exactly one
///   worker thread and nowhere to move other queued tasks to. Block
///   directly; the wait is bounded by the writer thread's own
///   responsiveness (an independent OS thread making progress the whole
///   time), so this stalls that runtime's other tasks for a short, bounded
///   window rather than risking a panic or a deadlock — an accepted
///   tradeoff for a rare, control-path-only API with no async twin.
/// - A `multi_thread` runtime: wrap the wait in `block_in_place`, so Tokio
///   moves this worker's other queued tasks to a different worker instead
///   of stalling them for the wait's duration — multi-thread runtimes have
///   somewhere else to put that work, so there is no reason not to ask for
///   it.
///
/// Never uses `Handle::block_on` (which panics identically to
/// `blocking_recv` when called from within a runtime, and for the same
/// reason) — only `block_in_place`, which is the API tokio provides
/// specifically to permit this pattern.
fn send_and_wait_via_writer_thread<T>(wait: impl FnOnce() -> Result<T>) -> Result<T> {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(wait)
        }
        _ => wait(),
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
    /// crash recovery).
    pub fn open(
        dir: impl AsRef<Path>,
        roll_policy: RollPolicy,
        durability: Durability,
        channel_capacity: usize,
    ) -> Result<Self> {
        // `RollPolicy::max_bytes` becomes a `u32` file position/offset-delta
        // on the wire (`.oidx`'s `file_pos`, PLAN §2.3's "segment ≤ 1 GiB,
        // więc u32 na pozycję wystarcza"); validate the policy once here
        // instead of silently truncating with `as u32` on the hot path.
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
        // racing the recovery scan/truncate against a live writer. Released
        // automatically when `PartitionInner` (and this `File`) drops.
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
            let seg = Segment::create_new(&dir, 0, prealloc_bytes(&roll_policy))?;
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
                // chain starting at `last_base`, so this subtraction cannot
                // underflow — `checked_sub` here turns a violated invariant
                // into a clean error instead of a panic or a wrapped `as u32`.
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
            // computation. `last_base` is always correct
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

        // `partition.meta` (M2, PLAN-M2 §1a / `meta.rs`): a missing file —
        // every partition written by M1, and a brand-new one before its
        // first roll/leader-epoch-change/persist — opens with `hw = leo`,
        // both the correct M1-compatibility fallback ("brak pliku → hw =
        // leo") and the correct steady state for a partition nobody ever
        // calls `set_high_watermark` on. A *present* file's `high_watermark`
        // is clamped to the freshly-recovered `log_end_offset` (never the
        // other way around): `log_end_offset` just came from this scan, the
        // only source of truth for it, so a persisted `hw` that somehow
        // exceeds it (should not happen — `set_high_watermark` itself
        // clamps to `leo` before every persist) still cannot make
        // `high_watermark` exceed `log_end_offset` on open. `leo_hint` is
        // never read into `log_end_offset` at all — see `meta.rs`'s doc.
        let persisted_meta = crate::meta::read_meta(&dir);
        let initial_hw = match persisted_meta {
            Some(m) => m.high_watermark.min(log_end_offset),
            None => log_end_offset,
        };
        let initial_leader_epoch = persisted_meta.map(|m| m.leader_epoch).unwrap_or(0);

        let (leo_watch_tx, _leo_watch_rx) = tokio::sync::watch::channel(log_end_offset);
        let state = Arc::new(PartitionState {
            segments: RwLock::new(segments_state),
            log_end_offset: AtomicU64::new(log_end_offset),
            high_watermark: AtomicU64::new(initial_hw),
            hw_manual: AtomicBool::new(false),
            leader_epoch: AtomicU32::new(initial_leader_epoch),
            leo_watch_tx,
            poisoned: AtomicBool::new(false),
            fsync_poisoned: AtomicBool::new(false),
            detached: AtomicBool::new(false),
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
    /// does not have to keep its own clone.
    ///
    /// Returns `BusError::PartitionPoisoned` once a prior group's fsync has
    /// failed for a batch that landed on a segment already rolled away
    /// within that same group: those bytes are durable (the roll itself
    /// fsyncs the outgoing segment), but nothing safe can publish their
    /// offset after the fact, and leaving the partition open would let the
    /// *next* append reuse that unpublished offset for different bytes.
    /// Every append fails this way until the directory is reopened —
    /// `Partition::open`'s crash recovery only re-validates the tail of the
    /// last segment, so it naturally forgets the unpublished batch and
    /// resumes cleanly from the last group that was fully published.
    pub fn append_batch(&self, batch: Bytes) -> Result<AppendResult> {
        if self.inner.state.detached.load(Ordering::Acquire) {
            return Err(BusError::PartitionDetached);
        }
        if self.inner.state.poisoned.load(Ordering::Acquire) {
            return Err(BusError::WriterPoisoned);
        }
        if self.inner.state.fsync_poisoned.load(Ordering::Acquire) {
            return Err(BusError::PartitionPoisoned);
        }
        let tx = self.inner.tx.as_ref().ok_or(BusError::WriterClosed)?;
        let (resp_tx, resp_rx) = oneshot::channel();
        match tx.try_send(WriterCommand::Append {
            batch,
            resp: resp_tx,
        }) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(cmd)) => {
                let batch = match cmd {
                    WriterCommand::Append { batch, .. } => batch,
                    _ => unreachable!("this call only ever sends WriterCommand::Append"),
                };
                return Err(BusError::Throttled {
                    retry_after_ms: self.inner.throttle_hint_ms,
                    batch,
                });
            }
            Err(mpsc::error::TrySendError::Closed(_)) => return Err(BusError::WriterClosed),
        }
        resp_rx
            .blocking_recv()
            .map_err(|_| BusError::WriterClosed)?
    }

    /// Async counterpart to `append_batch`: submission (`try_send`) is
    /// still non-blocking and throttles exactly like the
    /// sync path, but waiting for the writer's ack goes through the
    /// `oneshot::Receiver` future instead of `blocking_recv()`, so this is
    /// safe to call from a Tokio task without `spawn_blocking` — unlike
    /// `append_batch`, whose `blocking_recv()` panics if driven directly on
    /// a runtime worker thread.
    pub async fn append_batch_async(&self, batch: Bytes) -> Result<AppendResult> {
        if self.inner.state.detached.load(Ordering::Acquire) {
            return Err(BusError::PartitionDetached);
        }
        if self.inner.state.poisoned.load(Ordering::Acquire) {
            return Err(BusError::WriterPoisoned);
        }
        if self.inner.state.fsync_poisoned.load(Ordering::Acquire) {
            return Err(BusError::PartitionPoisoned);
        }
        let tx = self.inner.tx.as_ref().ok_or(BusError::WriterClosed)?;
        let (resp_tx, resp_rx) = oneshot::channel();
        match tx.try_send(WriterCommand::Append {
            batch,
            resp: resp_tx,
        }) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(cmd)) => {
                let batch = match cmd {
                    WriterCommand::Append { batch, .. } => batch,
                    _ => unreachable!("this call only ever sends WriterCommand::Append"),
                };
                return Err(BusError::Throttled {
                    retry_after_ms: self.inner.throttle_hint_ms,
                    batch,
                });
            }
            Err(mpsc::error::TrySendError::Closed(_)) => return Err(BusError::WriterClosed),
        }
        resp_rx.await.map_err(|_| BusError::WriterClosed)?
    }

    pub fn log_end_offset(&self) -> u64 {
        self.inner.state.log_end_offset.load(Ordering::Acquire)
    }

    /// The offset up to which records are visible to consumers (PLAN-M2
    /// §1a). Backed by its own atomic rather than aliasing
    /// `log_end_offset` — see `PartitionState::high_watermark`'s doc for
    /// how it stays equal to `log_end_offset` when no
    /// `ReplicationCoordinator` is active (RF=1, M1 behavior unchanged).
    pub fn high_watermark(&self) -> u64 {
        self.inner.state.high_watermark.load(Ordering::Acquire)
    }

    /// Advances `high_watermark`, never past `log_end_offset` and never
    /// backwards (K-M2-1: `hw` is monotonic and persisted, never
    /// regresses). Returns the value actually in effect after the call
    /// (which may be lower than `hw` if it was clamped, or unchanged if
    /// `hw` was already at or ahead of it). Works under either
    /// `HwTracking` mode — this call itself is always the caller's
    /// explicit request, distinct from the writer thread's own
    /// `FollowLeo` auto-advance (`process_group`).
    pub fn set_high_watermark(&self, hw: u64) -> u64 {
        let leo = self.log_end_offset();
        let clamped = hw.min(leo);
        self.inner
            .state
            .high_watermark
            .fetch_max(clamped, Ordering::AcqRel);
        self.inner.state.high_watermark.load(Ordering::Acquire)
    }

    /// Switches how `high_watermark` is advanced (M2, PLAN-M2 §1a — see
    /// `HwTracking`'s doc). A `ReplicationCoordinator` calls this with
    /// `Manual` once this partition has a real leader/follower role, and
    /// never switches back to `FollowLeo` for the lifetime of that role.
    pub fn set_hw_tracking(&self, mode: HwTracking) {
        self.inner
            .state
            .hw_manual
            .store(mode == HwTracking::Manual, Ordering::Release);
    }

    /// The current `HwTracking` mode — `FollowLeo` until a
    /// `ReplicationCoordinator` calls `set_hw_tracking(Manual)`.
    pub fn hw_tracking(&self) -> HwTracking {
        if self.inner.state.hw_manual.load(Ordering::Acquire) {
            HwTracking::Manual
        } else {
            HwTracking::FollowLeo
        }
    }

    /// The leader epoch this partition currently recognizes. `0` until a
    /// `ReplicationCoordinator` calls `set_leader_epoch` (PLAN-M2 §1a).
    pub fn leader_epoch(&self) -> u32 {
        self.inner.state.leader_epoch.load(Ordering::Acquire)
    }

    /// Advances the recognized leader epoch. Monotonic: rejects an `epoch`
    /// older than the one already stored with `LeaderEpochStale` instead of
    /// silently ignoring it, so a caller driving an election (wave 1, agent
    /// EL) learns immediately that it lost a race rather than believing its
    /// stale epoch was accepted. Epoch changes are rare (one per failover),
    /// so this persists meta immediately rather than waiting for the next
    /// periodic flush — `flush_meta`'s doc explains what "persist" means in
    /// this build.
    pub fn set_leader_epoch(&self, epoch: u32) -> Result<()> {
        let have = self.inner.state.leader_epoch.load(Ordering::Acquire);
        if epoch < have {
            return Err(BusError::LeaderEpochStale { have, got: epoch });
        }
        self.inner
            .state
            .leader_epoch
            .store(epoch, Ordering::Release);
        self.flush_meta()
    }

    /// Follower-side append (PLAN-M2 §1a): the leader has already assigned
    /// `expected_base_offset` and the batch's own header carries that same
    /// offset, so unlike `append_batch` this call must not silently land at
    /// a different position, and unlike `append_batch` it never
    /// re-serializes the batch — no `patch_base_offset` call, `batch` is
    /// written to disk exactly as received.
    ///
    /// `check_replicated_preconditions` is a fast-fail check against this
    /// partition's *currently visible* `log_end_offset`/`leader_epoch`,
    /// run before ever touching the writer thread; the authoritative check
    /// (against the batch's own `header.base_offset` and the writer's
    /// actual cursor at the moment this command is processed) runs inside
    /// `append_one` on the writer thread itself, which is what actually
    /// decides `OffsetMismatch`/`LeaderEpochStale` under concurrency — see
    /// its doc. `high_watermark` is left untouched either way (`Landed::
    /// skip_hw`): a follower's own append landing is not evidence of
    /// quorum.
    pub fn append_replicated(
        &self,
        batch: Bytes,
        expected_base_offset: u64,
        leader_epoch: u32,
    ) -> Result<AppendResult> {
        self.check_replicated_preconditions(expected_base_offset, leader_epoch)?;
        let tx = self.inner.tx.as_ref().ok_or(BusError::WriterClosed)?;
        let (resp_tx, resp_rx) = oneshot::channel();
        match tx.try_send(WriterCommand::AppendReplicated {
            batch,
            leader_epoch,
            resp: resp_tx,
        }) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(cmd)) => {
                let batch = match cmd {
                    WriterCommand::AppendReplicated { batch, .. } => batch,
                    _ => unreachable!("this call only ever sends WriterCommand::AppendReplicated"),
                };
                return Err(BusError::Throttled {
                    retry_after_ms: self.inner.throttle_hint_ms,
                    batch,
                });
            }
            Err(mpsc::error::TrySendError::Closed(_)) => return Err(BusError::WriterClosed),
        }
        resp_rx
            .blocking_recv()
            .map_err(|_| BusError::WriterClosed)?
    }

    /// Async twin of `append_replicated` — see `append_batch_async` for why
    /// the sync/async split exists.
    pub async fn append_replicated_async(
        &self,
        batch: Bytes,
        expected_base_offset: u64,
        leader_epoch: u32,
    ) -> Result<AppendResult> {
        self.check_replicated_preconditions(expected_base_offset, leader_epoch)?;
        let tx = self.inner.tx.as_ref().ok_or(BusError::WriterClosed)?;
        let (resp_tx, resp_rx) = oneshot::channel();
        match tx.try_send(WriterCommand::AppendReplicated {
            batch,
            leader_epoch,
            resp: resp_tx,
        }) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(cmd)) => {
                let batch = match cmd {
                    WriterCommand::AppendReplicated { batch, .. } => batch,
                    _ => unreachable!("this call only ever sends WriterCommand::AppendReplicated"),
                };
                return Err(BusError::Throttled {
                    retry_after_ms: self.inner.throttle_hint_ms,
                    batch,
                });
            }
            Err(mpsc::error::TrySendError::Closed(_)) => return Err(BusError::WriterClosed),
        }
        resp_rx.await.map_err(|_| BusError::WriterClosed)?
    }

    /// Shared precondition check for both `append_replicated` variants:
    /// the offset the leader assigned must be exactly this partition's
    /// current `log_end_offset` (a gap or overlap means a frame was lost
    /// or reordered — `OffsetMismatch`), and the leader's epoch must not be
    /// older than the one this partition already recognizes
    /// (`LeaderEpochStale`). Also checks `poisoned`/`fsync_poisoned` up
    /// front, matching `append_batch`'s guards, since both `append_replicated`
    /// variants build their own `WriterCommand` directly instead of
    /// delegating to `append_batch`.
    fn check_replicated_preconditions(
        &self,
        expected_base_offset: u64,
        leader_epoch: u32,
    ) -> Result<()> {
        if self.inner.state.detached.load(Ordering::Acquire) {
            return Err(BusError::PartitionDetached);
        }
        if self.inner.state.poisoned.load(Ordering::Acquire) {
            return Err(BusError::WriterPoisoned);
        }
        if self.inner.state.fsync_poisoned.load(Ordering::Acquire) {
            return Err(BusError::PartitionPoisoned);
        }
        let leo = self.log_end_offset();
        if expected_base_offset != leo {
            return Err(BusError::OffsetMismatch {
                expected: leo,
                got: expected_base_offset,
            });
        }
        let have = self.inner.state.leader_epoch.load(Ordering::Acquire);
        if leader_epoch < have {
            return Err(BusError::LeaderEpochStale {
                have,
                got: leader_epoch,
            });
        }
        Ok(())
    }

    /// Truncates the log tail down to `to_offset` (PLAN-M2 §1a): a follower
    /// promoted past a former leader's un-replicated tail, or an old
    /// leader rejoining after a failover, discards offsets beyond the new
    /// leader's chain. Always refuses to truncate below `high_watermark`
    /// (`TruncateBelowHighWatermark`) — those records may already be
    /// visible to a consumer. Returns the resulting `log_end_offset`
    /// (unchanged if `to_offset` was already `>= log_end_offset`).
    ///
    /// Executed as `WriterCommand::Truncate` on the writer thread — see
    /// `truncate`'s doc — so it can never race a concurrent group commit
    /// (PLAN-M2 §4.2 M2-R3). The check here against the currently-visible
    /// `high_watermark` is a fast-fail convenience only: `hw` never
    /// decreases (K-M2-1), so the writer's own re-check can only ever be
    /// *stricter*, never let through something this one would have refused.
    ///
    /// Synchronous with no async twin (frozen contract): callers already
    /// on a Tokio task (`bus::replication::follower`) call this directly,
    /// so the wait for the writer's reply is runtime-aware — see
    /// `send_and_wait_via_writer_thread`'s doc for exactly what that means
    /// and why it never panics.
    pub fn truncate_to_offset(&self, to_offset: u64) -> Result<u64> {
        if self.inner.state.detached.load(Ordering::Acquire) {
            return Err(BusError::PartitionDetached);
        }
        if self.inner.state.poisoned.load(Ordering::Acquire) {
            return Err(BusError::WriterPoisoned);
        }
        if self.inner.state.fsync_poisoned.load(Ordering::Acquire) {
            return Err(BusError::PartitionPoisoned);
        }
        let hw = self.high_watermark();
        if to_offset < hw {
            return Err(BusError::TruncateBelowHighWatermark { hw, to: to_offset });
        }
        let tx = self.inner.tx.as_ref().ok_or(BusError::WriterClosed)?;
        let (resp_tx, resp_rx) = std_mpsc::sync_channel(1);
        send_and_wait_via_writer_thread(move || {
            send_writer_command_blocking(
                tx,
                WriterCommand::Truncate {
                    to_offset,
                    resp: resp_tx,
                },
            )?;
            resp_rx.recv().map_err(|_| BusError::WriterClosed)?
        })
    }

    /// Subscribes to `log_end_offset` changes without polling — the
    /// replication feeder (wave 1, agent RL) `select!`s on this instead of
    /// spinning on `log_end_offset()`. Published by the writer thread right
    /// after `log_end_offset` itself (see `process_group`), so a change
    /// observed here always has its bytes already durable (post-fsync) and
    /// visible to readers — a leader driven by this watch can never ship
    /// bytes that could still be rolled back by a later fsync failure in
    /// the same group.
    pub fn subscribe_leo(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.state.leo_watch_tx.subscribe()
    }

    /// Persists `high_watermark`/`leader_epoch` to `partition.meta`
    /// (`meta.rs`) immediately, via `WriterCommand::PersistMeta` — routed
    /// through the writer thread (like every other `partition.meta` write:
    /// `roll()`, the writer's own shutdown/periodic paths) so concurrent
    /// callers can never race the same tmp path. A no-op for an already-
    /// `detach()`ed partition (nothing left worth persisting, and the
    /// directory may be about to disappear).
    ///
    /// Synchronous with no async twin (`set_leader_epoch` calls this
    /// directly for the same reason) — see `truncate_to_offset`'s doc and
    /// `send_and_wait_via_writer_thread`'s for why calling this from a
    /// Tokio task is safe.
    pub fn flush_meta(&self) -> Result<()> {
        if self.inner.state.detached.load(Ordering::Acquire) {
            return Ok(());
        }
        let tx = self.inner.tx.as_ref().ok_or(BusError::WriterClosed)?;
        let (resp_tx, resp_rx) = std_mpsc::sync_channel(1);
        send_and_wait_via_writer_thread(move || {
            send_writer_command_blocking(tx, WriterCommand::PersistMeta { resp: resp_tx })?;
            resp_rx.recv().map_err(|_| BusError::WriterClosed)?
        })
    }

    pub fn open_reader(&self) -> PartitionReader {
        PartitionReader {
            state: Arc::clone(&self.inner.state),
        }
    }

    /// The oldest offset any reader can currently fetch: the current oldest
    /// segment's `base_offset`. Retention only ever deletes whole sealed
    /// segments oldest-first, so this is exactly the floor
    /// `fetch_from_offset` enforces, and callers (e.g. a consumer deciding
    /// whether to reset to "earliest" after `OffsetOutOfRange`) can read it
    /// without provoking that error themselves.
    pub fn earliest_offset(&self) -> u64 {
        self.inner
            .state
            .segments
            .read()
            .first()
            .map(|s| s.base_offset)
            .unwrap_or(0)
    }

    /// Every rolled (immutable) segment, oldest first — the active segment
    /// (still receiving writes) is deliberately excluded. Retention (PLAN
    /// §2.5 "usuwanie CAŁYCH zamkniętych segmentów") only ever inspects and
    /// deletes from this list, so a delete can never race the writer's own
    /// in-flight append.
    pub fn sealed_segments(&self) -> Vec<SealedSegmentInfo> {
        let segments = self.inner.state.segments.read();
        segments
            .iter()
            .take(segments.len().saturating_sub(1))
            .map(|desc| SealedSegmentInfo {
                base_offset: desc.base_offset,
                len: desc.len.load(Ordering::Acquire),
                log_path: desc.log_path.clone(),
            })
            .collect()
    }

    /// The still-growing active segment's current on-disk (logical) length
    /// in bytes — the byte-count counterpart to `sealed_segments()`, which
    /// deliberately excludes it. A caller wanting this partition's total
    /// log size on disk needs `sealed_segments().iter().map(|s| s.len).sum::<u64>()
    /// + active_segment_len()`; `sealed_segments()` alone is only a lower
    /// bound (`SealedSegmentInfo`'s doc used to warn about exactly this: a
    /// partition below its first roll always reported `0` even with real
    /// data on disk — M1-R2 review N-6). Reads the same live `AtomicU64`
    /// the writer thread publishes after every append
    /// (`process_group`), so this is always up to date, never a stale
    /// snapshot from partition-open time. `0` for a not-yet-existent
    /// partition is impossible: every `Partition` always has at least one
    /// segment (`Partition::open`'s own invariant).
    pub fn active_segment_len(&self) -> u64 {
        self.inner
            .state
            .segments
            .read()
            .last()
            .map(|desc| desc.len.load(Ordering::Acquire))
            .unwrap_or(0)
    }

    /// Physically deletes one sealed segment's log file and its `.oidx`/
    /// `.tidx` siblings, then drops it from the in-memory segment list.
    /// Refuses to touch the active segment or a `base_offset` that is not
    /// currently sealed (PLAN §2.5 "nigdy: aktywny segment"): the caller
    /// (retention.rs) is expected to have already filtered by
    /// `sealed_segments()`, but this is re-checked here so a caller bug can
    /// never corrupt live data. The log file is unlinked last, after both
    /// index files, so a crash mid-delete never leaves an index pointing at
    /// data whose log file already vanished while the reverse (orphaned
    /// index files after the log is gone) is merely inert litter.
    pub fn delete_sealed_segment(&self, base_offset: u64) -> Result<()> {
        if self.inner.state.detached.load(Ordering::Acquire) {
            return Err(BusError::PartitionDetached);
        }
        let mut segments = self.inner.state.segments.write();
        if segments.len() <= 1 {
            return Err(BusError::SegmentNotDeletable {
                base_offset,
                reason: "partition has no sealed segments",
            });
        }
        let idx = segments
            .iter()
            .position(|d| d.base_offset == base_offset)
            .ok_or(BusError::SegmentNotDeletable {
                base_offset,
                reason: "not a known segment of this partition",
            })?;
        if idx == segments.len() - 1 {
            return Err(BusError::SegmentNotDeletable {
                base_offset,
                reason: "refusing to delete the active segment",
            });
        }
        let desc = segments.remove(idx);
        let dir = desc
            .log_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let _ = std::fs::remove_file(segment::offset_index_path(&dir, desc.base_offset));
        let _ = std::fs::remove_file(segment::time_index_path(&dir, desc.base_offset));
        std::fs::remove_file(&desc.log_path).map_err(|e| BusError::io(&desc.log_path, e))?;
        Ok(())
    }

    /// Permanently retires this partition's read/write surface: the owning
    /// topic or organization has been deleted, and the caller (`BusService`
    /// in tentaflow-core) is about to `remove_dir_all` the partition's
    /// directory out from under any handle that has not yet noticed. Clears
    /// the segment list — which holds every descriptor, active segment
    /// included, so this also drops the last reference readers use to reach
    /// the active segment's file — under the same write lock every other
    /// segment-list mutation uses, then sets `detached` so every later
    /// `fetch_from_offset`/`fetch_from_timestamp`/`append_batch`/
    /// `append_batch_async`/`delete_sealed_segment` call on this handle (and
    /// on every `PartitionReader` cloned from it, since they share this
    /// `PartitionState`) fails fast with `BusError::PartitionDetached`
    /// instead of racing the deletion for a raw ENOENT. Idempotent: calling
    /// it again is a no-op (the list is already empty).
    ///
    /// Does not stop the writer thread or release the directory lock — the
    /// caller is expected to drop every `Partition`/`PartitionReader` handle
    /// right after this call, which does both (see `Drop for
    /// PartitionInner`).
    pub fn detach(&self) {
        self.inner.state.segments.write().clear();
        self.inner.state.detached.store(true, Ordering::Release);
    }

    /// A non-owning reference to this partition's shared writer/flock state
    /// (`Arc::downgrade` under the hood) — does not keep the writer thread
    /// or the directory lock alive by itself. Lets a registry track every
    /// `Partition` clone a long-lived caller (a `ConsumerHandle`) is
    /// holding WITHOUT that registry itself becoming a second owner: a
    /// registry entry that outlives every real clone must not be the
    /// reason the writer thread/flock never shut down. `upgrade()` recovers
    /// a real `Partition` clone as long as at least one strong owner
    /// (typically the `ConsumerHandle`) is still alive.
    pub fn downgrade(&self) -> WeakPartition {
        WeakPartition {
            inner: Arc::downgrade(&self.inner),
        }
    }
}

/// See `Partition::downgrade`. `Clone` because a registry keyed by
/// `(org, topic, partition)` needs to hand out independent handles to more
/// than one caller inspecting/detaching the same live partition.
#[derive(Clone)]
pub struct WeakPartition {
    inner: Weak<PartitionInner>,
}

impl WeakPartition {
    /// Recovers a strong `Partition` clone, or `None` if every strong owner
    /// (every `Partition`/`ConsumerHandle` clone) has already dropped —
    /// meaning the writer thread has exited and the directory flock is
    /// already released, so there is nothing left to `detach()`.
    pub fn upgrade(&self) -> Option<Partition> {
        self.inner.upgrade().map(|inner| Partition { inner })
    }
}

/// An independent read handle: its own file descriptors (cached per
/// segment), never the writer's. Cheap to clone (Arc-backed shared
/// index/state), matching the "fan-out to N independent readers" scenario
/// in PLAN §5.2 P3.
#[derive(Clone)]
pub struct PartitionReader {
    state: Arc<PartitionState>,
}

/// One read call's scratch buffer for the readahead-then-fallback read
/// strategy: reused across every batch/segment visited during a single
/// `fetch_from_offset`/`fetch_from_timestamp` call instead of allocating
/// fresh per batch.
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
    /// See `Partition::high_watermark` — `fetch_from_offset` bounds reads
    /// on this, not on `log_end_offset`, so it now gates consumer
    /// visibility on the same atomic a `ReplicationCoordinator` (wave 1)
    /// drives via `Partition::set_high_watermark`.
    pub fn high_watermark(&self) -> u64 {
        self.state.high_watermark.load(Ordering::Acquire)
    }

    /// See `Partition::log_end_offset` — reachable from a standalone reader
    /// handle like `high_watermark`/`earliest_offset` above. Only
    /// `fetch_raw_to_end_of_log` needs it: that read's bound is this value
    /// rather than the high watermark.
    pub fn log_end_offset(&self) -> u64 {
        self.state.log_end_offset.load(Ordering::Acquire)
    }

    /// `true` once `Partition::detach()` has run on this partition (the
    /// owning topic/org has been deleted/purged). `high_watermark`/
    /// `earliest_offset` do NOT check this themselves — both are simple,
    /// infallible reads of `PartitionState` with no `Result` to return a
    /// "gone" signal through, and changing that is a breaking API change
    /// this crate is not making for M1. A caller that needs to distinguish
    /// "empty partition" from "detached partition" (`ConsumerHandle::
    /// seek_to_earliest`/`lag` in `tentaflow-core`) checks this FIRST and
    /// maps a `true` result to its own "not found" error instead.
    pub fn is_detached(&self) -> bool {
        self.state.detached.load(Ordering::Acquire)
    }

    /// See `Partition::earliest_offset` — same value, reachable from a
    /// standalone reader handle (e.g. a `ConsumerHandle`) that never kept
    /// the `Partition` itself around.
    pub fn earliest_offset(&self) -> u64 {
        self.state
            .segments
            .read()
            .first()
            .map(|s| s.base_offset)
            .unwrap_or(0)
    }

    /// Reads batches starting at `from_offset`, accumulating up to
    /// `max_bytes` of on-wire batch bytes. Always returns at least one
    /// batch if `from_offset < high_watermark()`, even if that single batch
    /// alone exceeds `max_bytes`.
    ///
    /// Returns *whole* batches: the sparse `.oidx` only gives a floor
    /// position, so the first returned batch may start at an
    /// offset before `from_offset` — the same semantics Kafka's fetch API
    /// has. A caller that needs an exact starting record should iterate the
    /// first batch through `BatchView::records_from(from_offset)` rather
    /// than assuming every record in the result is `>= from_offset`.
    pub fn fetch_from_offset(&self, from_offset: u64, max_bytes: usize) -> Result<Vec<BatchView>> {
        if self.state.detached.load(Ordering::Acquire) {
            return Err(BusError::PartitionDetached);
        }
        if from_offset >= self.high_watermark() {
            return Ok(Vec::new());
        }

        // Snapshot the segment list (cheap: a `Vec` of `Arc` clones) and
        // drop the lock immediately — holding `segments.read()` across the
        // I/O below would make `roll()`'s write-lock acquisition (and thus
        // every future append) wait on however long this fetch's disk I/O
        // takes. Re-taken (see the ENOENT retry below) if retention
        // deletes a segment out from under this snapshot.
        let segments: Vec<Arc<SegmentDescriptor>> = self.state.segments.read().clone();
        self.fetch_from_offset_with_snapshot(segments, from_offset, max_bytes)
    }

    /// Core of `fetch_from_offset`, parameterized over the starting segment
    /// snapshot so a test can hand it a snapshot taken *before* a
    /// concurrent `delete_sealed_segment` call, deterministically
    /// reproducing the ENOENT race `fetch_from_offset` itself can only hit
    /// under real thread scheduling.
    fn fetch_from_offset_with_snapshot(
        &self,
        mut segments: Vec<Arc<SegmentDescriptor>>,
        from_offset: u64,
        max_bytes: usize,
    ) -> Result<Vec<BatchView>> {
        if self.state.detached.load(Ordering::Acquire) {
            return Err(BusError::PartitionDetached);
        }
        // Retention (`delete_sealed_segment`) only ever removes the oldest
        // sealed segments, so the current oldest surviving segment's
        // `base_offset` is the hard floor of what any reader can serve. A
        // request below that floor is a genuine gap for the caller, not
        // something to paper over by starting later than asked — Kafka
        // reports the same situation as `OffsetOutOfRangeException`.
        let earliest = segments.first().map(|s| s.base_offset).unwrap_or(0);
        if from_offset < earliest {
            return Err(BusError::OffsetOutOfRange {
                requested: from_offset,
                earliest,
                latest: self.high_watermark(),
            });
        }

        let mut seg_idx = segments
            .partition_point(|s| s.base_offset <= from_offset)
            .saturating_sub(1);

        let mut out = Vec::new();
        let mut consumed = 0usize;
        let mut want_offset = from_offset;
        let mut readbuf = ReadBuf::new();
        // Bounds the ENOENT-retry branch below to exactly one re-snapshot
        // for this whole call. A segment legitimately vanishing between
        // this reader's snapshot and its first `reader_file()` open only
        // ever needs one refresh (retention only ever removes segments, it
        // never re-adds one this reader could still miss); a segment that
        // is *still* missing after that refresh is not a transient race —
        // something outside normal retention removed the file — and must
        // fail loudly instead of looping forever re-reading the same stale
        // descriptor.
        let mut resnapshotted = false;

        while seg_idx < segments.len() {
            // Cloning the `Arc` (rather than borrowing `segments[seg_idx]`)
            // keeps `desc` alive independently of `segments`, which the
            // ENOENT retry below needs to be able to replace.
            let desc = Arc::clone(&segments[seg_idx]);
            let target_delta = want_offset.saturating_sub(desc.base_offset) as u32;
            let start_pos = {
                let g = desc.offset_entries.read();
                floor_offset(&g, target_delta)
                    .map(|e| e.file_pos as u64)
                    .unwrap_or(0)
            };
            let file = match desc.reader_file() {
                Ok(f) => f,
                Err(BusError::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::NotFound && !resnapshotted =>
                {
                    // This reader snapshotted the segment list before
                    // retention unlinked this segment's log file and
                    // dropped its descriptor from `state.segments` — the
                    // descriptor we are holding is already stale. Take a
                    // fresh snapshot (once) and resume from `want_offset`
                    // instead of surfacing a spurious I/O error for a
                    // segment that legitimately no longer exists.
                    resnapshotted = true;
                    segments = self.state.segments.read().clone();
                    let earliest = segments.first().map(|s| s.base_offset).unwrap_or(0);
                    if want_offset < earliest {
                        return Err(BusError::OffsetOutOfRange {
                            requested: want_offset,
                            earliest,
                            latest: self.high_watermark(),
                        });
                    }
                    seg_idx = segments
                        .partition_point(|s| s.base_offset <= want_offset)
                        .saturating_sub(1);
                    continue;
                }
                // Falls through here (instead of matching the ENOENT arm
                // above) once `resnapshotted` is already `true`: a segment
                // still listed but missing on disk *after* one refresh is
                // not the benign delete-vs-snapshot race, it is a genuine
                // inconsistency between `state.segments` and disk — surface
                // it as a plain I/O error instead of retrying indefinitely.
                Err(e) => return Err(e),
            };
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

    /// Zero-copy twin of `fetch_from_offset`: same bounds
    /// (`high_watermark`), same `OffsetOutOfRange`/`PartitionDetached`
    /// behavior, same "always returns at least one whole batch" contract —
    /// but returns each batch's exact on-disk bytes (`RawBatch::bytes`)
    /// instead of a parsed `BatchView`. Exists for the replication leader
    /// feeder (`bus/replication/leader.rs`, PLAN-M2 §1b): it forwards
    /// these bytes to a follower's `Partition::append_replicated` as-is,
    /// so re-parsing them here into a `BatchView` and re-encoding on the
    /// way out would be pure waste on the hot replication path.
    pub fn fetch_raw_from_offset(
        &self,
        from_offset: u64,
        max_bytes: usize,
    ) -> Result<Vec<RawBatch>> {
        if self.state.detached.load(Ordering::Acquire) {
            return Err(BusError::PartitionDetached);
        }
        if from_offset >= self.high_watermark() {
            return Ok(Vec::new());
        }
        let segments: Vec<Arc<SegmentDescriptor>> = self.state.segments.read().clone();
        self.fetch_raw_from_offset_with_snapshot(segments, from_offset, max_bytes)
    }

    /// Zero-copy read bounded by `log_end_offset` instead of
    /// `high_watermark`, for the replication leader's feeder.
    ///
    /// Why its own entry point instead of a knob on `fetch_raw_from_offset`: the
    /// two bounds are opposite requirements, and the collision between them is
    /// the defect this exists to fix. A CONSUMER must never see a record the
    /// chain has not committed, so `fetch_from_offset`/`fetch_raw_from_offset`
    /// stop at `high_watermark` (PLAN-M2 §4.2). A LEADER FEEDING A FOLLOWER is
    /// the mirror image: it has to send precisely the records that are NOT yet
    /// committed, because the follower's ACK of them is what moves
    /// `high_watermark` at all (PLAN-M2 §4.1). Read through the consumer bound,
    /// a leader whose partition sits in `HwTracking::Manual` asks for batches
    /// below `hw == 0`, is handed none, so nothing is ACKed, so `hw` stays 0 —
    /// the loop closes over itself and an `acks=quorum` topic replicates nothing
    /// that was ever published.
    ///
    /// Everything but that entry gate is `fetch_raw_from_offset`'s: the same
    /// segment-list snapshot, index-floor-then-scan search, "at least one whole
    /// batch" contract, `OffsetOutOfRange`/`PartitionDetached` behavior, and the
    /// same exact on-disk bytes.
    ///
    /// Not for consumers, and not a widening of `fetch_raw_from_offset`: a batch
    /// above `high_watermark` can still be cut away by a K-M2-1 truncate on a
    /// leader change, so reading it is only sound for a caller that IS the
    /// authority being replicated from.
    pub fn fetch_raw_to_end_of_log(
        &self,
        from_offset: u64,
        max_bytes: usize,
    ) -> Result<Vec<RawBatch>> {
        if self.state.detached.load(Ordering::Acquire) {
            return Err(BusError::PartitionDetached);
        }
        if from_offset >= self.log_end_offset() {
            return Ok(Vec::new());
        }
        let segments: Vec<Arc<SegmentDescriptor>> = self.state.segments.read().clone();
        self.fetch_raw_from_offset_with_snapshot(segments, from_offset, max_bytes)
    }

    /// Core of `fetch_raw_from_offset` — structurally identical to
    /// `fetch_from_offset_with_snapshot` (same segment-list snapshot, same
    /// index-floor-then-scan search, same bounded ENOENT-retry-on-
    /// retention-race, deliberately kept in lockstep with it rather than
    /// factored into one generic function: the two outputs' construction
    /// diverges at exactly one point — `BatchView::parse` vs. building a
    /// `RawBatch` straight from the already-decoded `header` and the raw
    /// buffer `ReadBuf::read_batch` already produced — and a shared core
    /// parameterized over that one difference read worse than the two
    /// bodies side by side for a loop this deeply nested already).
    fn fetch_raw_from_offset_with_snapshot(
        &self,
        mut segments: Vec<Arc<SegmentDescriptor>>,
        from_offset: u64,
        max_bytes: usize,
    ) -> Result<Vec<RawBatch>> {
        if self.state.detached.load(Ordering::Acquire) {
            return Err(BusError::PartitionDetached);
        }
        let earliest = segments.first().map(|s| s.base_offset).unwrap_or(0);
        if from_offset < earliest {
            return Err(BusError::OffsetOutOfRange {
                requested: from_offset,
                earliest,
                latest: self.high_watermark(),
            });
        }

        let mut seg_idx = segments
            .partition_point(|s| s.base_offset <= from_offset)
            .saturating_sub(1);

        let mut out = Vec::new();
        let mut consumed = 0usize;
        let mut want_offset = from_offset;
        let mut readbuf = ReadBuf::new();
        let mut resnapshotted = false;

        while seg_idx < segments.len() {
            let desc = Arc::clone(&segments[seg_idx]);
            let target_delta = want_offset.saturating_sub(desc.base_offset) as u32;
            let start_pos = {
                let g = desc.offset_entries.read();
                floor_offset(&g, target_delta)
                    .map(|e| e.file_pos as u64)
                    .unwrap_or(0)
            };
            let file = match desc.reader_file() {
                Ok(f) => f,
                Err(BusError::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::NotFound && !resnapshotted =>
                {
                    resnapshotted = true;
                    segments = self.state.segments.read().clone();
                    let earliest = segments.first().map(|s| s.base_offset).unwrap_or(0);
                    if want_offset < earliest {
                        return Err(BusError::OffsetOutOfRange {
                            requested: want_offset,
                            earliest,
                            latest: self.high_watermark(),
                        });
                    }
                    seg_idx = segments
                        .partition_point(|s| s.base_offset <= want_offset)
                        .saturating_sub(1);
                    continue;
                }
                Err(e) => return Err(e),
            };
            let seg_len = desc.len.load(Ordering::Acquire);

            let mut pos = start_pos;
            while pos + BATCH_HEADER_LEN as u64 <= seg_len {
                let (raw, total) = readbuf.read_batch(&file, &desc.log_path, pos, seg_len)?;
                let header = BatchHeader::decode(&raw[..BATCH_HEADER_LEN])?;
                if header.next_offset() <= want_offset {
                    pos += total;
                    continue;
                }

                consumed += raw.len();
                out.push(RawBatch {
                    base_offset: header.base_offset,
                    record_count: header.record_count,
                    next_offset: header.next_offset(),
                    bytes: raw,
                });
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
    /// Consumers: the `read_perf` bench (P3 fan-out by time) and the planned
    /// "seek to timestamp" action of the consumer-group UI (M3a).
    pub fn fetch_from_timestamp(
        &self,
        from_ts_ms: i64,
        max_bytes: usize,
    ) -> Result<Vec<BatchView>> {
        if self.state.detached.load(Ordering::Acquire) {
            return Err(BusError::PartitionDetached);
        }
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

    /// Like `one_record_batch`, but with a *real* `base_offset` baked into
    /// the header instead of the usual placeholder — `append_replicated`'s
    /// zero-copy path (PLAN-M2 §1a) never patches this field, so a test
    /// exercising it must build a batch whose header already carries the
    /// offset the writer is expected to land it at.
    fn one_record_batch_at(base_offset: u64, ts_ms: i64, payload_len: usize) -> Bytes {
        let mut b = crate::batch::BatchBuilder::new(base_offset, 1);
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

    /// Reading forward across a segment boundary must actually return the
    /// records on both sides of the roll, not just prove new segment files
    /// exist on disk.
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

    /// M1 retention (PLAN §2.5): only sealed segments are listed/deletable,
    /// never the active one, and a deleted segment's records become
    /// unreadable while the rest of the log stays intact.
    #[test]
    fn sealed_segments_lists_and_deletes_but_never_the_active_one() {
        let dir = temp_dir("partition-retention");
        let policy = RollPolicy {
            max_batches: 2,
            ..RollPolicy::default()
        };
        let part = Partition::open(&dir, policy, Durability::FsyncBatch, 8).unwrap();
        for i in 0..6i64 {
            part.append_batch(one_record_batch(i * 10, 8)).unwrap();
        }
        // Three segments (base offsets 0, 2, 4); only 0 and 2 are sealed.
        let sealed = part.sealed_segments();
        let sealed_bases: Vec<u64> = sealed.iter().map(|s| s.base_offset).collect();
        assert_eq!(sealed_bases, vec![0, 2]);
        assert!(sealed.iter().all(|s| s.len > 0));

        // Refuses to delete the active segment.
        let err = part.delete_sealed_segment(4).unwrap_err();
        assert!(matches!(
            err,
            BusError::SegmentNotDeletable { base_offset: 4, .. }
        ));

        // Deletes the oldest sealed segment; its log file is gone from disk.
        part.delete_sealed_segment(0).unwrap();
        assert!(!log_path(&dir, 0).exists());
        assert_eq!(
            part.sealed_segments()
                .iter()
                .map(|s| s.base_offset)
                .collect::<Vec<_>>(),
            vec![2]
        );

        // The rest of the log is untouched: offsets 2..6 still read back.
        let reader = part.open_reader();
        let from2 = reader.fetch_from_offset(2, 1024 * 1024).unwrap();
        let offsets: Vec<u64> = from2.iter().map(|v| v.header().base_offset).collect();
        assert_eq!(offsets, vec![2, 3, 4, 5]);

        // Deleting the same segment again is refused (no longer known).
        let err = part.delete_sealed_segment(0).unwrap_err();
        assert!(matches!(err, BusError::SegmentNotDeletable { .. }));
    }

    /// `active_segment_len` — the byte-count counterpart to
    /// `sealed_segments()` — must reflect real appended bytes even before
    /// any segment has rolled (M1-R2 review N-6: `size_bytes` was
    /// structurally always `0` below the first roll because only sealed
    /// segments were summed).
    #[test]
    fn active_segment_len_tracks_appends_before_any_roll() {
        let dir = temp_dir("partition-active-len");
        let part = Partition::open(&dir, RollPolicy::default(), Durability::Os, 8).unwrap();
        assert_eq!(part.active_segment_len(), 0);
        assert!(part.sealed_segments().is_empty());

        let batch = one_record_batch(0, 32);
        let batch_len = batch.len() as u64;
        part.append_batch(batch).unwrap();
        assert_eq!(part.active_segment_len(), batch_len);
        assert!(
            part.sealed_segments().is_empty(),
            "still nothing sealed — the active segment stays excluded from sealed_segments()"
        );

        part.append_batch(one_record_batch(0, 32)).unwrap();
        assert_eq!(part.active_segment_len(), batch_len * 2);
    }

    /// A directory created under the OLD default `RollPolicy` (1 GiB,
    /// preallocated) must still open and recover correctly under the NEW
    /// default (256 MiB, no preallocation) — recovery rescans the active
    /// segment's actual batch content (`Segment::open_active_with_recovery`)
    /// and truncates away any unused preallocated tail; it never trusts the
    /// file's on-disk length, so it does not care that the file was
    /// preallocated far past both the new and the old `max_bytes`
    /// (M1-R2 decision 5).
    #[test]
    fn opens_a_directory_preallocated_under_the_old_1gib_policy() {
        let dir = temp_dir("partition-legacy-prealloc");
        let legacy_policy = RollPolicy {
            max_bytes: 1024 * 1024 * 1024,
            preallocate: true,
            ..RollPolicy::default()
        };
        {
            let part = Partition::open(&dir, legacy_policy, Durability::FsyncBatch, 8).unwrap();
            part.append_batch(one_record_batch(1_000, 16)).unwrap();
            part.append_batch(one_record_batch(1_010, 16)).unwrap();
        }
        // The segment file's logical length (`Metadata::len`) is tiny —
        // preallocation (`F_PREALLOCATE`/`fallocate`) reserves disk EXTENTS
        // without moving `st_size` on the platforms this crate supports —
        // but the blocks actually reserved on disk cover the full 1 GiB,
        // exactly the "27 GiB preallocated for 2500 small records" disk
        // usage the R2 critique measured via `stat`/`du`.
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let meta = std::fs::metadata(log_path(&dir, 0)).unwrap();
            assert!(
                meta.blocks() * 512 >= 1024 * 1024 * 1024,
                "fixture must actually reserve ~1 GiB of disk blocks to reproduce the old \
                 behavior, got {} blocks ({} bytes)",
                meta.blocks(),
                meta.blocks() * 512
            );
        }

        // Reopen with the new default policy (256 MiB, no preallocation).
        let part = Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
        assert_eq!(part.log_end_offset(), 2);
        let reader = part.open_reader();
        let batches = reader.fetch_from_offset(0, 1024 * 1024).unwrap();
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].header().base_offset, 0);
        assert_eq!(batches[1].header().base_offset, 1);

        // Recovery truncated away the preallocated tail, regardless of the
        // policy this second `open` used.
        let recovered_len = std::fs::metadata(log_path(&dir, 0)).unwrap().len();
        assert!(
            recovered_len < 1024 * 1024,
            "recovery must discard the unused preallocated tail, got {recovered_len}"
        );
        assert_eq!(part.active_segment_len(), recovered_len);
    }

    /// Once retention has deleted the sealed segment holding a given
    /// offset, fetching that offset must report `OffsetOutOfRange` — never
    /// silently rebase to whatever is now the oldest surviving segment —
    /// while offsets still covered by surviving segments keep working.
    #[test]
    fn fetch_from_deleted_offset_range_returns_offset_out_of_range() {
        let dir = temp_dir("partition-fetch-after-retention");
        let policy = RollPolicy {
            max_batches: 2,
            ..RollPolicy::default()
        };
        let part = Partition::open(&dir, policy, Durability::FsyncBatch, 8).unwrap();
        for i in 0..6i64 {
            part.append_batch(one_record_batch(i * 10, 8)).unwrap();
        }
        // Segments at base offsets 0, 2, 4; only 0 and 2 are sealed.
        part.delete_sealed_segment(0).unwrap();

        let reader = part.open_reader();

        // Offset 0 and 1 lived exclusively in the now-deleted segment.
        for requested in [0u64, 1u64] {
            let err = reader
                .fetch_from_offset(requested, 1024 * 1024)
                .unwrap_err();
            match err {
                BusError::OffsetOutOfRange {
                    requested: got_requested,
                    earliest,
                    latest,
                } => {
                    assert_eq!(got_requested, requested);
                    assert_eq!(earliest, 2);
                    assert_eq!(latest, 6);
                }
                other => panic!("expected OffsetOutOfRange, got {other:?}"),
            }
        }
        assert_eq!(part.earliest_offset(), 2);
        assert_eq!(reader.earliest_offset(), 2);

        // Offset 2 onward is still fully readable.
        let from2 = reader.fetch_from_offset(2, 1024 * 1024).unwrap();
        let offsets: Vec<u64> = from2.iter().map(|v| v.header().base_offset).collect();
        assert_eq!(offsets, vec![2, 3, 4, 5]);
    }

    /// A reader that snapshotted the segment list *before*
    /// `delete_sealed_segment` unlinked a segment's log file (but had not
    /// yet opened that segment's fd) must skip the vanished segment by
    /// refreshing its snapshot, not fail the whole fetch with a raw I/O
    /// error. Exercised directly against the private snapshot-parameterized
    /// core (`fetch_from_offset_with_snapshot`) since reproducing the exact
    /// interleaving through the public API depends on thread scheduling.
    #[test]
    fn fetch_skips_a_segment_deleted_after_the_snapshot_was_taken() {
        let dir = temp_dir("partition-fetch-enoent-race");
        let policy = RollPolicy {
            max_batches: 2,
            ..RollPolicy::default()
        };
        let part = Partition::open(&dir, policy, Durability::FsyncBatch, 8).unwrap();
        for i in 0..6i64 {
            part.append_batch(one_record_batch(i * 10, 8)).unwrap();
        }
        let reader = part.open_reader();

        // Take a stale snapshot that still includes the soon-to-be-deleted
        // segment (base_offset 0), mirroring a reader that read the
        // segment list right before retention ran.
        let stale_segments: Vec<Arc<SegmentDescriptor>> = reader.state.segments.read().clone();
        assert_eq!(stale_segments[0].base_offset, 0);

        // Now retention deletes it for real: the live list drops the
        // descriptor and the log file is unlinked.
        part.delete_sealed_segment(0).unwrap();

        // Fetching from offset 0 against the stale snapshot: offset 0 is
        // genuinely gone, so this must still be `OffsetOutOfRange`, not an
        // I/O error, even though the stale snapshot's own `earliest_offset`
        // check would pass (it still contains segment 0).
        let err = reader
            .fetch_from_offset_with_snapshot(stale_segments.clone(), 0, 1024 * 1024)
            .unwrap_err();
        assert!(
            matches!(
                err,
                BusError::OffsetOutOfRange {
                    requested: 0,
                    earliest: 2,
                    latest: 6
                }
            ),
            "expected OffsetOutOfRange after refreshing past the deleted segment, got {err:?}"
        );

        // Fetching an offset that is still valid (survives in segment 2)
        // must succeed by refreshing past the deleted segment 0 entry
        // instead of raising ENOENT for it.
        let batches = reader
            .fetch_from_offset_with_snapshot(stale_segments, 3, 1024 * 1024)
            .unwrap();
        let offsets: Vec<u64> = batches.iter().map(|v| v.header().base_offset).collect();
        assert_eq!(offsets, vec![3, 4, 5]);
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

    /// A second `Partition::open` on the same directory while the first
    /// handle is still alive must fail, not silently truncate the live
    /// writer's active segment.
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

    /// A crash immediately after a segment roll — the new active segment
    /// exists but is completely empty, so its own tail scan recovers
    /// nothing. `log_end_offset` must come back as that segment's own base
    /// offset (its filename), not from the previous segment's
    /// (never-fsynced) `.oidx` tail.
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

    /// A reader running concurrently with a live writer must never observe
    /// `high_watermark() > from_offset` without the corresponding batch
    /// actually being fetchable. Runs many rounds under a small
    /// channel/group size to maximize the chance of hitting that race
    /// window.
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

    /// A batch built with `Codec::Lz4` must round-trip through the full
    /// append -> disk -> fetch path, not just `BatchBuilder`/`BatchView` in
    /// isolation (`batch.rs` only tests the codec directly).
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

    /// A deterministic `Throttled` path — capacity 1, `FsyncBatch`
    /// durability (so the writer thread is genuinely busy on disk I/O, not
    /// just instantaneously free again), one thread holding the channel's
    /// only slot via a job that has not been drained yet.
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
                            // The producer's buffer must come back
                            // unconsumed, not require a clone to retry.
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

    /// Polls a future exactly once with a no-op waker — enough to exercise
    /// `append_batch_async`'s synchronous early-return checks
    /// (detached/poisoned) without pulling a Tokio runtime into this crate's
    /// dev-dependencies just for one test (the crate only depends on
    /// `tokio`'s "sync" feature, PLAN §2.3).
    fn poll_once<F: std::future::Future>(fut: F) -> std::task::Poll<F::Output> {
        fn no_op(_: *const ()) {}
        fn clone(_: *const ()) -> std::task::RawWaker {
            raw_waker()
        }
        fn raw_waker() -> std::task::RawWaker {
            static VTABLE: std::task::RawWakerVTable =
                std::task::RawWakerVTable::new(clone, no_op, no_op, no_op);
            std::task::RawWaker::new(std::ptr::null(), &VTABLE)
        }
        let waker = unsafe { std::task::Waker::from_raw(raw_waker()) };
        let mut cx = std::task::Context::from_waker(&waker);
        let mut fut = Box::pin(fut);
        fut.as_mut().poll(&mut cx)
    }

    /// A segment whose log file has been unlinked
    /// out from under `state.segments` by something other than
    /// `delete_sealed_segment` (so, unlike the benign race
    /// `fetch_skips_a_segment_deleted_after_the_snapshot_was_taken` covers,
    /// the descriptor is *never* removed from the live list) must not send
    /// `fetch_from_offset` into an unbounded ENOENT/re-snapshot loop. The
    /// bounded retry re-snapshots exactly once, sees the same still-listed,
    /// still-missing segment, and gives up with a plain I/O error instead.
    #[test]
    fn fetch_returns_io_error_instead_of_looping_when_segment_still_listed_but_missing_on_disk() {
        let dir = temp_dir("partition-fetch-enoent-still-listed");
        let policy = RollPolicy {
            max_batches: 2,
            ..RollPolicy::default()
        };
        let part = Partition::open(&dir, policy, Durability::FsyncBatch, 8).unwrap();
        for i in 0..6i64 {
            part.append_batch(one_record_batch(i * 10, 8)).unwrap();
        }
        let reader = part.open_reader();

        // Unlink segment 0's log file directly, bypassing
        // `delete_sealed_segment` (which would also drop the descriptor
        // from `state.segments`). This reproduces the inconsistency
        // `delete_topic`/`purge_org`'s `remove_dir_all` can leave behind in
        // tentaflow-core: the file is gone, but the descriptor is not.
        std::fs::remove_file(log_path(&dir, 0)).unwrap();

        let err = reader.fetch_from_offset(0, 1024 * 1024).unwrap_err();
        assert!(
            matches!(err, BusError::Io { .. }),
            "expected a plain I/O error after the bounded retry, got {err:?}"
        );
    }

    /// Once a partition is `detach()`ed, every read and
    /// write on it (and on readers cloned from it beforehand) must report
    /// `PartitionDetached` instead of racing a concurrent directory removal
    /// for a raw I/O error.
    #[test]
    fn detach_fails_every_later_read_and_write_with_partition_detached() {
        let dir = temp_dir("partition-detach");
        let part = Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
        part.append_batch(one_record_batch(0, 8)).unwrap();
        // Cloned/opened *before* detach, like a `ConsumerHandle` that was
        // already live when an admin ran `delete_topic`.
        let reader = part.open_reader();

        part.detach();

        assert!(matches!(
            part.append_batch(one_record_batch(0, 8)),
            Err(BusError::PartitionDetached)
        ));
        assert!(matches!(
            poll_once(part.append_batch_async(one_record_batch(0, 8))),
            std::task::Poll::Ready(Err(BusError::PartitionDetached))
        ));
        assert!(matches!(
            part.delete_sealed_segment(0),
            Err(BusError::PartitionDetached)
        ));
        assert!(matches!(
            reader.fetch_from_offset(0, 1024),
            Err(BusError::PartitionDetached)
        ));
        assert!(matches!(
            reader.fetch_from_timestamp(0, 1024),
            Err(BusError::PartitionDetached)
        ));

        // Calling `detach()` again is a no-op, not a panic or a different
        // error.
        part.detach();
        assert!(matches!(
            part.append_batch(one_record_batch(0, 8)),
            Err(BusError::PartitionDetached)
        ));
    }

    /// A `PartitionReader` shares `PartitionState` through its
    /// own `Arc`, independent of `PartitionInner` (writer thread + directory
    /// lock) — dropping every `Partition` handle must stop the writer
    /// thread and release the lock (so the directory can be reopened) while
    /// leaving reads already-published data through a surviving
    /// `PartitionReader` fully working.
    #[test]
    fn reader_outlives_partition_and_keeps_reading_after_drop() {
        let dir = temp_dir("partition-reader-outlives-partition");
        let part = Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
        part.append_batch(one_record_batch(0, 8)).unwrap();
        part.append_batch(one_record_batch(0, 8)).unwrap();
        let reader = part.open_reader();

        drop(part);

        assert_eq!(reader.high_watermark(), 2);
        let batches = reader.fetch_from_offset(0, 1024 * 1024).unwrap();
        assert_eq!(batches.len(), 2);

        // The directory's flock was released by dropping the last
        // `Partition` handle, so a fresh `Partition::open` on the same
        // directory succeeds even while `reader` is still alive.
        let reopened =
            Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
        assert_eq!(reopened.log_end_offset(), 2);

        // The original reader still works after the reopen too.
        assert_eq!(reader.fetch_from_offset(0, 1024 * 1024).unwrap().len(), 2);
    }

    /// `WeakPartition` must not, by itself, keep the writer thread/flock
    /// alive: once every strong `Partition` clone is dropped, `upgrade()`
    /// returns `None` even though the registry entry (a `WeakPartition`)
    /// still exists.
    #[test]
    fn weak_partition_upgrades_while_a_strong_clone_lives_and_fails_once_all_are_dropped() {
        let dir = temp_dir("partition-weak-upgrade");
        let part = Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
        let weak = part.downgrade();

        let upgraded = weak.upgrade().expect("strong clone still alive");
        upgraded.append_batch(one_record_batch(0, 8)).unwrap();
        assert_eq!(upgraded.log_end_offset(), 1);
        drop(upgraded);

        // The original `part` is still alive, so the writer thread/flock
        // are still held even after the upgraded clone above was dropped.
        assert!(weak.upgrade().is_some());

        drop(part);
        assert!(
            weak.upgrade().is_none(),
            "no strong owner left, so upgrade must fail"
        );

        // The directory's flock was released — a fresh `Partition::open` on
        // the same path succeeds.
        let reopened =
            Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
        assert_eq!(reopened.log_end_offset(), 1);
    }

    /// Calling `detach()` through a `WeakPartition::upgrade()`'d clone must
    /// be visible to every OTHER clone and to readers opened from any of
    /// them — proving a registry that only ever holds `WeakPartition`s can
    /// still force-close a partition a live caller (e.g. a `ConsumerHandle`)
    /// is holding its own strong clone of.
    #[test]
    fn detach_through_an_upgraded_weak_clone_is_visible_to_every_other_handle() {
        let dir = temp_dir("partition-weak-detach");
        let part = Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
        part.append_batch(one_record_batch(0, 8)).unwrap();
        let held_by_consumer = part.clone();
        let reader = held_by_consumer.open_reader();
        let weak = part.downgrade();
        drop(part);

        let via_registry = weak.upgrade().expect("consumer's clone keeps this alive");
        via_registry.detach();

        assert!(reader.is_detached());
        assert!(matches!(
            reader.fetch_from_offset(0, 1024),
            Err(BusError::PartitionDetached)
        ));
        assert!(matches!(
            held_by_consumer.append_batch(one_record_batch(0, 8)),
            Err(BusError::PartitionDetached)
        ));
    }

    /// State-transition level: the pure decision this
    /// crate has no way to reach through a genuine fsync failure in a unit
    /// test (see `plan_fsync_failure_response`'s doc). A job on the
    /// currently-active segment gets rolled back on failure; a job on a
    /// segment already rolled away earlier in the same group is left alone
    /// (it is durable) but flags the group as straddled.
    #[test]
    fn plan_fsync_failure_response_rolls_back_active_only_and_flags_straddle() {
        // All jobs on the active segment (base offset 10): rolls back to
        // the smallest `pos_before`, no straddle.
        let (rollback, straddled) = plan_fsync_failure_response(&[(10, 100), (10, 50)], 10);
        assert_eq!(rollback, Some((50, 2)));
        assert!(!straddled);

        // One job on a segment rolled away earlier in the group (base
        // offset 0) plus one on the active segment (base offset 10): the
        // active-segment job still rolls back, and the straddle is flagged.
        let (rollback, straddled) = plan_fsync_failure_response(&[(0, 200), (10, 50)], 10);
        assert_eq!(rollback, Some((50, 1)));
        assert!(straddled);

        // No jobs landed at all (e.g. the whole group failed before any
        // fsync was attempted): nothing to roll back, no straddle.
        let (rollback, straddled) = plan_fsync_failure_response(&[], 10);
        assert_eq!(rollback, None);
        assert!(!straddled);
    }

    /// Consequence level: once a partition is
    /// `fsync_poisoned` (which `process_group` sets internally when
    /// `plan_fsync_failure_response` reports a straddle — exercised
    /// directly here since this crate has no hook to force a genuine fsync
    /// syscall failure), every further append must fail with
    /// `PartitionPoisoned` and stay that way until the directory is
    /// reopened; reads of already-published data are unaffected.
    #[test]
    fn fsync_poisoned_partition_refuses_appends_until_reopen() {
        let dir = temp_dir("partition-fsync-poison");
        let part = Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
        part.append_batch(one_record_batch(0, 8)).unwrap();

        // What `process_group` does internally after detecting a straddled
        // fsync failure.
        part.inner
            .state
            .fsync_poisoned
            .store(true, Ordering::Release);

        assert!(matches!(
            part.append_batch(one_record_batch(0, 8)),
            Err(BusError::PartitionPoisoned)
        ));
        assert!(matches!(
            poll_once(part.append_batch_async(one_record_batch(0, 8))),
            std::task::Poll::Ready(Err(BusError::PartitionPoisoned))
        ));

        let reader = part.open_reader();
        // Poisoning blocks writes, not reads of already-durable data.
        assert_eq!(reader.fetch_from_offset(0, 1024).unwrap().len(), 1);

        drop(part);
        drop(reader);

        // Reopening the directory starts from fresh, un-poisoned state.
        let reopened =
            Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
        reopened.append_batch(one_record_batch(0, 8)).unwrap();
    }

    // ===== M2 (PLAN-M2 §1a): high_watermark/leader_epoch/replication contract =====

    /// A1/A2 (PLAN-M2 §4.1): no `partition.meta` file exists yet, so both a
    /// brand-new partition and one reopened after writes (simulating an M1
    /// partition upgraded in place) must observe `hw == leo` — the
    /// fallback this wave's contract promises.
    #[test]
    fn high_watermark_tracks_log_end_offset_with_no_replication_coordinator() {
        let dir = temp_dir("partition-hw-default");
        let part = Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
        assert_eq!(part.high_watermark(), 0);
        assert_eq!(part.high_watermark(), part.log_end_offset());

        part.append_batch(one_record_batch(0, 8)).unwrap();
        part.append_batch(one_record_batch(0, 8)).unwrap();
        assert_eq!(part.high_watermark(), 2);
        assert_eq!(part.high_watermark(), part.log_end_offset());

        let reader = part.open_reader();
        assert_eq!(reader.high_watermark(), 2);

        drop(part);
        drop(reader);

        // Reopen: no partition.meta exists, so hw must fall back to leo,
        // not reset to 0.
        let reopened =
            Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
        assert_eq!(reopened.log_end_offset(), 2);
        assert_eq!(reopened.high_watermark(), 2);
    }

    #[test]
    fn set_high_watermark_is_monotonic_and_clamped_to_leo() {
        let dir = temp_dir("partition-hw-set");
        let part = Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
        part.append_batch(one_record_batch(0, 8)).unwrap();
        part.append_batch(one_record_batch(0, 8)).unwrap();
        let leo = part.log_end_offset();
        assert_eq!(leo, 2);
        // The writer thread's own auto-tracking already pushed hw to leo.
        assert_eq!(part.high_watermark(), leo);

        // Clamped: asking for more than leo caps at leo.
        assert_eq!(part.set_high_watermark(leo + 100), leo);
        assert_eq!(part.high_watermark(), leo);

        // Monotonic: asking for less than the current value is a no-op.
        assert_eq!(part.set_high_watermark(0), leo);
        assert_eq!(part.high_watermark(), leo);
    }

    #[test]
    fn subscribe_leo_wakes_on_append() {
        let dir = temp_dir("partition-leo-watch");
        let part = Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
        let mut rx = part.subscribe_leo();
        assert_eq!(*rx.borrow(), 0);

        part.append_batch(one_record_batch(0, 8)).unwrap();

        // `has_changed()` is infallible here: the sender (owned by
        // `PartitionState`, held alive by `part`) cannot have been dropped.
        assert!(rx.has_changed().unwrap());
        assert_eq!(*rx.borrow_and_update(), 1);

        part.append_batch(one_record_batch(0, 8)).unwrap();
        assert!(rx.has_changed().unwrap());
        assert_eq!(*rx.borrow_and_update(), 2);
    }

    /// M2 task 5 (PLAN-M2 §1a): `subscribe_leo` must only notify a
    /// receiver AFTER the corresponding bytes are durable (post-fsync) and
    /// visible to readers — a replication feeder driven by this watch must
    /// never be able to ship bytes a later fsync failure in the same group
    /// could still roll back. Runs a live writer concurrently under
    /// `FsyncBatch` (real fsync I/O) so the assertion exercises the actual
    /// timing window between the watch firing and `desc.len`/`hw` being
    /// published, not just the source order of two statements.
    #[test]
    fn subscribe_leo_only_wakes_after_group_commit_durability() {
        let dir = temp_dir("partition-leo-watch-durability");
        let part = Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 4).unwrap();
        let reader = part.open_reader();
        let mut rx = part.subscribe_leo();

        let writer = part.clone();
        let writer_handle = thread::spawn(move || {
            for i in 0..200i64 {
                loop {
                    match writer.append_batch(one_record_batch(i, 16)) {
                        Ok(_) => break,
                        Err(BusError::Throttled { .. }) => continue,
                        Err(e) => panic!("unexpected error: {e}"),
                    }
                }
            }
        });

        let mut last_seen = 0u64;
        while last_seen < 200 {
            if rx.has_changed().unwrap_or(false) {
                let leo = *rx.borrow_and_update();
                if leo > last_seen {
                    let batches = reader
                        .fetch_from_offset(last_seen, 4 * 1024 * 1024)
                        .unwrap();
                    assert!(
                        !batches.is_empty(),
                        "subscribe_leo fired for leo={leo} but from_offset={last_seen} has \
                         no durable, fetchable batch yet"
                    );
                    last_seen = leo;
                }
            }
        }

        writer_handle.join().unwrap();
    }

    #[test]
    fn leader_epoch_defaults_to_zero_and_set_leader_epoch_is_monotonic() {
        let dir = temp_dir("partition-leader-epoch");
        let part = Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
        assert_eq!(part.leader_epoch(), 0);

        part.set_leader_epoch(1).unwrap();
        assert_eq!(part.leader_epoch(), 1);

        part.set_leader_epoch(5).unwrap();
        assert_eq!(part.leader_epoch(), 5);

        let err = part.set_leader_epoch(3).unwrap_err();
        assert!(matches!(
            err,
            BusError::LeaderEpochStale { have: 5, got: 3 }
        ));
        // A rejected, stale epoch must not have mutated the stored one.
        assert_eq!(part.leader_epoch(), 5);
    }

    /// Both the fast pre-check (against the caller-supplied
    /// `expected_base_offset`) and the writer's own authoritative check
    /// (against the batch's real `header.base_offset`, decoded fresh from
    /// the zero-copy bytes) must independently reject a mismatch — every
    /// batch here carries its *real* base_offset (`one_record_batch_at`),
    /// unlike the placeholder every other test batch uses, because
    /// `append_replicated` never patches it.
    #[test]
    fn append_replicated_accepts_the_expected_offset_and_rejects_a_mismatch() {
        let dir = temp_dir("partition-append-replicated");
        let part = Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
        part.set_leader_epoch(2).unwrap();

        let r0 = part
            .append_replicated(one_record_batch_at(0, 0, 8), 0, 2)
            .unwrap();
        assert_eq!(r0.base_offset, 0);
        assert_eq!(part.log_end_offset(), 1);

        // Wrong offset: leo is now 1, not 0 (the batch's own header still
        // claims base_offset 0 too, so this exercises the fast pre-check).
        let err = part
            .append_replicated(one_record_batch_at(0, 0, 8), 0, 2)
            .unwrap_err();
        assert!(matches!(
            err,
            BusError::OffsetMismatch {
                expected: 1,
                got: 0
            }
        ));

        // Stale leader epoch: this partition already recognizes epoch 2.
        // Uses the correct base_offset (1) so only the epoch check can be
        // at fault.
        let err = part
            .append_replicated(one_record_batch_at(1, 0, 8), 1, 1)
            .unwrap_err();
        assert!(matches!(
            err,
            BusError::LeaderEpochStale { have: 2, got: 1 }
        ));

        // The two rejected calls above must not have advanced the log.
        assert_eq!(part.log_end_offset(), 1);

        let r1 = part
            .append_replicated(one_record_batch_at(1, 0, 8), 1, 2)
            .unwrap();
        assert_eq!(r1.base_offset, 1);
        assert_eq!(part.log_end_offset(), 2);
    }

    /// M2 task 4 (PLAN-M2 §1a): the zero-copy path must write the exact
    /// same bytes `append_batch` would have written for an identical batch
    /// — compared byte-for-byte at the segment-file level, not just
    /// through the decoded view — and the result must still decode
    /// correctly through the normal read path.
    #[test]
    fn append_replicated_is_zero_copy_and_byte_identical_to_the_leaders_segment() {
        let leader_dir = temp_dir("partition-repl-leader");
        let leader = Partition::open(
            &leader_dir,
            RollPolicy::default(),
            Durability::FsyncBatch,
            8,
        )
        .unwrap();
        // base_offset 0 is already correct for the first append, so
        // `append_batch`'s patch is a no-op — these exact bytes are what a
        // real leader would have put on the wire for a follower.
        let wire_bytes = one_record_batch(12_345, 64);
        let leader_result = leader.append_batch(wire_bytes.clone()).unwrap();
        assert_eq!(leader_result.base_offset, 0);

        let follower_dir = temp_dir("partition-repl-follower");
        let follower = Partition::open(
            &follower_dir,
            RollPolicy::default(),
            Durability::FsyncBatch,
            8,
        )
        .unwrap();
        follower.set_leader_epoch(1).unwrap();
        let follower_result = follower
            .append_replicated(wire_bytes.clone(), 0, 1)
            .unwrap();
        assert_eq!(follower_result.base_offset, 0);

        let leader_bytes = std::fs::read(log_path(&leader_dir, 0)).unwrap();
        let follower_bytes = std::fs::read(log_path(&follower_dir, 0)).unwrap();
        assert_eq!(
            leader_bytes, follower_bytes,
            "append_replicated must write the exact same bytes append_batch wrote for an \
             identical batch"
        );

        // `append_replicated` never advances `high_watermark` (`skip_hw`)
        // — a follower's own append landing is not evidence of quorum —
        // so raise it explicitly here the way a real follower would from
        // the leader's heartbeat/ack frames, before reading it back.
        assert_eq!(follower.high_watermark(), 0);
        follower.set_high_watermark(1);

        let follower_reader = follower.open_reader();
        let batches = follower_reader.fetch_from_offset(0, 1024 * 1024).unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].header().base_offset, 0);
        let records: Vec<_> = batches[0].records().collect::<Result<_>>().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].payload.len(), 64);
    }

    /// The zero-copy leader-feeder path (`PartitionReader::
    /// fetch_raw_from_offset`, added alongside `bus/replication/leader.rs`):
    /// every `RawBatch::bytes` must be the exact on-disk slice (no
    /// re-encoding), and feeding those bytes straight into a follower's
    /// `Partition::append_replicated` must land byte-identical segment
    /// files — the whole point of returning raw bytes instead of a parsed
    /// `BatchView` here.
    #[test]
    fn fetch_raw_from_offset_returns_exact_bytes_and_replicates_byte_identically() {
        let leader_dir = temp_dir("partition-fetch-raw-leader");
        let leader = Partition::open(
            &leader_dir,
            RollPolicy::default(),
            Durability::FsyncBatch,
            8,
        )
        .unwrap();
        for i in 0..5i64 {
            leader.append_batch(one_record_batch(i * 10, 16)).unwrap();
        }
        assert_eq!(leader.log_end_offset(), 5);

        let reader = leader.open_reader();
        let raw_batches = reader.fetch_raw_from_offset(0, 1024 * 1024).unwrap();
        assert_eq!(raw_batches.len(), 5);

        // Bounded by `high_watermark`, same as `fetch_from_offset` — this
        // partition never left `HwTracking::FollowLeo`, so hw == leo == 5.
        assert!(reader.fetch_raw_from_offset(5, 1024).unwrap().is_empty());

        // Every `RawBatch` must be the exact slice of the segment file it
        // came from, in order, covering the file exactly once.
        let file_bytes = std::fs::read(log_path(&leader_dir, 0)).unwrap();
        let mut pos = 0usize;
        for (i, rb) in raw_batches.iter().enumerate() {
            assert_eq!(rb.base_offset, i as u64);
            assert_eq!(rb.record_count, 1);
            assert_eq!(rb.next_offset, i as u64 + 1);
            let slice = &file_bytes[pos..pos + rb.bytes.len()];
            assert_eq!(
                rb.bytes.as_ref(),
                slice,
                "RawBatch {i} bytes must match the segment file slice verbatim"
            );
            pos += rb.bytes.len();
        }
        assert_eq!(
            pos,
            file_bytes.len(),
            "RawBatches must cover the segment file exactly"
        );

        // Feed the raw bytes straight into a follower via
        // `append_replicated` — no decode/re-encode step in between.
        let follower_dir = temp_dir("partition-fetch-raw-follower");
        let follower = Partition::open(
            &follower_dir,
            RollPolicy::default(),
            Durability::FsyncBatch,
            8,
        )
        .unwrap();
        follower.set_leader_epoch(1).unwrap();
        for rb in &raw_batches {
            let result = follower
                .append_replicated(rb.bytes.clone(), rb.base_offset, 1)
                .unwrap();
            assert_eq!(result.base_offset, rb.base_offset);
        }
        assert_eq!(follower.log_end_offset(), 5);

        let leader_bytes = std::fs::read(log_path(&leader_dir, 0)).unwrap();
        let follower_bytes = std::fs::read(log_path(&follower_dir, 0)).unwrap();
        assert_eq!(
            leader_bytes, follower_bytes,
            "segment files must be byte-identical after zero-copy raw replication"
        );
    }

    /// The feeder bound (`fetch_raw_to_end_of_log`, added once the three-node
    /// suite proved that a leader feeding through the CONSUMER bound sends
    /// nothing): with `HwTracking::Manual` and no replica ACK yet, `hw` is 0
    /// while `leo` is not, so only a `leo`-bounded read can reach the records
    /// whose ACK is supposed to move `hw` in the first place.
    #[test]
    fn fetch_raw_to_end_of_log_feeds_what_the_high_watermark_bound_hides() {
        let dir = temp_dir("partition-fetch-raw-leo");
        let part = Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
        part.set_hw_tracking(HwTracking::Manual);
        for i in 0..5i64 {
            part.append_batch(one_record_batch(i * 10, 16)).unwrap();
        }
        assert_eq!(part.log_end_offset(), 5);
        assert_eq!(part.high_watermark(), 0, "Manual: nothing has been ACKed");

        let reader = part.open_reader();
        assert_eq!(reader.log_end_offset(), 5);
        // The bound this path replaces is still exactly as blind as it was:
        assert!(
            reader
                .fetch_raw_from_offset(0, 1024 * 1024)
                .unwrap()
                .is_empty(),
            "a hw-bounded read must stay empty below hw == 0"
        );

        let raw = reader.fetch_raw_to_end_of_log(0, 1024 * 1024).unwrap();
        assert_eq!(raw.len(), 5, "the leo-bounded read must feed all five");
        assert_eq!(raw[0].base_offset, 0);
        assert_eq!(raw[4].next_offset, 5);

        // Bound is the only difference: once `hw` catches up, both entry points
        // hand back the same bytes.
        part.set_high_watermark(5);
        assert_eq!(
            reader.fetch_raw_to_end_of_log(0, 1024 * 1024).unwrap()[0]
                .bytes
                .as_ref(),
            reader.fetch_raw_from_offset(0, 1024 * 1024).unwrap()[0]
                .bytes
                .as_ref(),
            "the two reads must agree byte-for-byte wherever both can see"
        );

        // And it is still a bound, not a free-for-all: at `leo` it sends nothing,
        // and it grows exactly as the log grows.
        assert!(reader.fetch_raw_to_end_of_log(5, 1024).unwrap().is_empty());
        part.append_batch(one_record_batch(50, 16)).unwrap();
        assert_eq!(reader.fetch_raw_to_end_of_log(5, 1024).unwrap().len(), 1);
    }

    #[test]
    fn append_replicated_async_rejects_offset_mismatch_without_touching_the_writer() {
        let dir = temp_dir("partition-append-replicated-async");
        let part = Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();

        let poll = poll_once(part.append_replicated_async(one_record_batch(0, 8), 5, 0));
        assert!(matches!(
            poll,
            std::task::Poll::Ready(Err(BusError::OffsetMismatch {
                expected: 0,
                got: 5
            }))
        ));
        assert_eq!(part.log_end_offset(), 0);
    }

    #[test]
    fn truncate_to_offset_rejects_below_high_watermark() {
        let dir = temp_dir("partition-truncate-below-hw");
        let part = Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
        part.append_batch(one_record_batch(0, 8)).unwrap();
        part.append_batch(one_record_batch(0, 8)).unwrap();
        let hw = part.high_watermark();
        assert_eq!(hw, 2);

        let err = part.truncate_to_offset(0).unwrap_err();
        assert!(matches!(
            err,
            BusError::TruncateBelowHighWatermark { hw: 2, to: 0 }
        ));
    }

    /// A partition never driven by a `ReplicationCoordinator` stays in
    /// `HwTracking::FollowLeo`, where `hw` always equals `leo` — so every
    /// truncate test below first switches to `Manual` *before* appending
    /// anything, holding `hw` at 0 while `leo` advances, to reproduce the
    /// real scenario `truncate_to_offset` exists for: a leader/follower
    /// whose un-acknowledged tail must be discarded after a failover.
    fn open_with_hw_pinned_at_zero(dir: &Path) -> Partition {
        let part = Partition::open(dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
        part.set_hw_tracking(HwTracking::Manual);
        part
    }

    /// Truncate inside the still-open active segment (no roll involved):
    /// the tail beyond the cut is discarded, the rest survives, and the
    /// segment's own on-disk length actually shrinks.
    #[test]
    fn truncate_to_offset_inside_the_active_segment_drops_the_tail() {
        let dir = temp_dir("partition-truncate-active");
        let part = open_with_hw_pinned_at_zero(&dir);
        for i in 0..5i64 {
            part.append_batch(one_record_batch(i, 8)).unwrap();
        }
        assert_eq!(part.log_end_offset(), 5);
        let full_len = std::fs::metadata(log_path(&dir, 0)).unwrap().len();

        let new_leo = part.truncate_to_offset(3).unwrap();
        assert_eq!(new_leo, 3);
        assert_eq!(part.log_end_offset(), 3);
        // hw is untouched by a truncate at/above it.
        assert_eq!(part.high_watermark(), 0);

        let shrunk_len = std::fs::metadata(log_path(&dir, 0)).unwrap().len();
        assert!(
            shrunk_len < full_len,
            "truncate must physically shrink the active segment's file, {shrunk_len} >= {full_len}"
        );

        // `fetch_from_offset` gates on `high_watermark`, which the
        // truncate above deliberately left at 0 (`Manual` tracking, never
        // advanced yet); raise it to the new leo to read back the
        // survivors, the same way replication code would once it has
        // re-established quorum over them.
        part.set_high_watermark(3);
        let reader = part.open_reader();
        let batches = reader.fetch_from_offset(0, 1024 * 1024).unwrap();
        let offsets: Vec<u64> = batches.iter().map(|v| v.header().base_offset).collect();
        assert_eq!(offsets, vec![0, 1, 2]);
    }

    /// Truncate that lands inside an already-*sealed* segment: every
    /// segment after it (including the previously-active one) is deleted
    /// from disk, and the sealed segment is promoted to active.
    #[test]
    fn truncate_to_offset_across_a_sealed_segment_boundary_removes_later_segment_files() {
        let dir = temp_dir("partition-truncate-sealed-boundary");
        let policy = RollPolicy {
            max_batches: 2,
            ..RollPolicy::default()
        };
        let part = Partition::open(&dir, policy, Durability::FsyncBatch, 8).unwrap();
        part.set_hw_tracking(HwTracking::Manual);
        for i in 0..6i64 {
            part.append_batch(one_record_batch(i, 8)).unwrap();
        }
        // Segments at base offsets 0, 2, 4 (0 and 2 sealed, 4 active).
        assert_eq!(part.sealed_segments().len(), 2);
        assert!(log_path(&dir, 4).exists());

        // Target offset 3 lands inside segment base=2 (offsets 2,3):
        // batch 2 (next_offset=3) is kept, batch 3 (next_offset=4) is
        // dropped — segment 4 (the old active one) is deleted whole.
        let new_leo = part.truncate_to_offset(3).unwrap();
        assert_eq!(new_leo, 3);
        assert_eq!(part.log_end_offset(), 3);
        assert!(
            !log_path(&dir, 4).exists(),
            "the segment that used to be active must be removed from disk"
        );
        assert!(
            log_path(&dir, 0).exists(),
            "an untouched earlier segment survives"
        );
        assert!(
            log_path(&dir, 2).exists(),
            "the promoted segment keeps its own filename"
        );

        part.set_high_watermark(3);
        let reader = part.open_reader();
        let batches = reader.fetch_from_offset(0, 1024 * 1024).unwrap();
        let offsets: Vec<u64> = batches.iter().map(|v| v.header().base_offset).collect();
        assert_eq!(offsets, vec![0, 1, 2]);
    }

    /// After a truncate, the writer resumes exactly at the new
    /// `log_end_offset` — no gap, and the appended batch is readable back.
    #[test]
    fn append_after_truncate_continues_from_the_new_leo() {
        let dir = temp_dir("partition-truncate-then-append");
        let part = open_with_hw_pinned_at_zero(&dir);
        for i in 0..5i64 {
            part.append_batch(one_record_batch(i, 8)).unwrap();
        }
        assert_eq!(part.truncate_to_offset(3).unwrap(), 3);

        let r = part.append_batch(one_record_batch(99, 8)).unwrap();
        assert_eq!(r.base_offset, 3);
        assert_eq!(part.log_end_offset(), 4);

        part.set_high_watermark(4);
        let reader = part.open_reader();
        let batches = reader.fetch_from_offset(0, 1024 * 1024).unwrap();
        let offsets: Vec<u64> = batches.iter().map(|v| v.header().base_offset).collect();
        assert_eq!(offsets, vec![0, 1, 2, 3]);
        assert_eq!(batches[3].header().base_timestamp_ms, 99);
    }

    /// A truncate that crosses a sealed segment boundary, followed by a
    /// clean shutdown and reopen, must recover to exactly the
    /// post-truncate state — not resurrect the deleted segment's data, and
    /// not lose the `high_watermark`/`leader_epoch` persisted by the
    /// truncate itself (`truncate`'s own `persist_meta_now` call).
    #[test]
    fn recovery_after_truncate_and_reopen_matches_the_post_truncate_state() {
        let dir = temp_dir("partition-truncate-recovery");
        {
            let policy = RollPolicy {
                max_batches: 2,
                ..RollPolicy::default()
            };
            let part = Partition::open(&dir, policy, Durability::FsyncBatch, 8).unwrap();
            part.set_hw_tracking(HwTracking::Manual);
            part.set_leader_epoch(7).unwrap();
            for i in 0..6i64 {
                part.append_batch(one_record_batch(i, 8)).unwrap();
            }
            part.set_high_watermark(2);
            assert_eq!(part.truncate_to_offset(3).unwrap(), 3);
            // `truncate` persists meta synchronously — dropping right after
            // still exercises the writer's own best-effort shutdown persist
            // for the *same* (unchanged) values.
        }

        let reopened =
            Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
        assert_eq!(reopened.log_end_offset(), 3);
        assert_eq!(reopened.high_watermark(), 2);
        assert_eq!(reopened.leader_epoch(), 7);
        assert!(!log_path(&dir, 4).exists());

        let reader = reopened.open_reader();
        reopened.set_high_watermark(3);
        let batches = reader.fetch_from_offset(0, 1024 * 1024).unwrap();
        let offsets: Vec<u64> = batches.iter().map(|v| v.header().base_offset).collect();
        assert_eq!(offsets, vec![0, 1, 2]);

        let r = reopened.append_batch(one_record_batch(0, 8)).unwrap();
        assert_eq!(r.base_offset, 3);
    }

    #[test]
    fn flush_meta_persists_hw_and_leader_epoch_across_reopen() {
        let dir = temp_dir("partition-flush-meta");
        {
            let part =
                Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
            part.append_batch(one_record_batch(0, 8)).unwrap();
            part.set_leader_epoch(4).unwrap();
            part.flush_meta().unwrap();
        }
        let reopened =
            Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
        assert_eq!(reopened.leader_epoch(), 4);
        assert_eq!(reopened.high_watermark(), 1);
    }

    /// A1/A2 (PLAN-M2 §4.1): opening a directory that has no
    /// `partition.meta` at all — exactly what every M1 partition looks
    /// like, since that file did not exist until this crate added it —
    /// must fall back to `hw = leo`, not `hw = 0`. The directory here is
    /// built with real segments/data through the current (M2) engine and
    /// then has its `partition.meta` removed by hand, since this build's
    /// own writer thread persists that file on every clean shutdown
    /// (`writer_loop`'s doc) — the removal is what stands in for "a
    /// directory an M1 binary wrote and never touched again".
    #[test]
    fn open_on_a_directory_without_partition_meta_falls_back_to_hw_equals_leo() {
        let dir = temp_dir("partition-no-meta-file");
        {
            let part =
                Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
            part.append_batch(one_record_batch(0, 8)).unwrap();
            part.append_batch(one_record_batch(0, 8)).unwrap();
        }
        assert!(crate::meta::meta_path(&dir).exists());
        std::fs::remove_file(crate::meta::meta_path(&dir)).unwrap();

        let reopened =
            Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
        assert_eq!(reopened.log_end_offset(), 2);
        assert_eq!(reopened.high_watermark(), 2);
        assert_eq!(reopened.leader_epoch(), 0);
    }

    // ===== Runtime-safety regression: sync writer API called from inside a
    // Tokio task (bus::replication::follower's exact call shape) must never
    // panic with "Cannot block the current thread from within a runtime" —
    // see `send_and_wait_via_writer_thread`'s doc for the fix. =====

    /// Exercises `set_leader_epoch`/`flush_meta`/`truncate_to_offset` from
    /// inside an `async fn` body driven by a `multi_thread` runtime — the
    /// `block_in_place` path in `send_and_wait_via_writer_thread`. Before
    /// the fix, this reproduced the coordinator-reported panic exactly
    /// (tokio's `blocking_send`/`blocking_recv` on the old `oneshot`-based
    /// reply channel).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sync_writer_api_is_runtime_safe_on_a_multi_thread_runtime() {
        let dir = temp_dir("partition-runtime-safe-mt");
        let part = Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
        // `append_batch_async` (not `append_batch`) to seed the partition —
        // the sync `append_batch` has no async-twin exemption in this fix
        // (it is not one of the three functions the coordinator reported
        // panicking, since `bus::replication::follower` always uses
        // `append_batch_async`/`append_replicated_async` on its hot path)
        // and would panic here for the same underlying reason this test is
        // guarding against, just via a different code path.
        part.append_batch_async(one_record_batch(0, 8))
            .await
            .unwrap();

        part.set_leader_epoch(3).unwrap();
        assert_eq!(part.leader_epoch(), 3);

        part.flush_meta().unwrap();

        part.set_hw_tracking(HwTracking::Manual);
        assert_eq!(part.truncate_to_offset(1).unwrap(), 1);
        assert_eq!(part.log_end_offset(), 1);
    }

    /// Same three calls, but under the `current_thread` flavor
    /// (`#[tokio::test]`'s default without `flavor = "multi_thread"`).
    /// `block_in_place` panics outright on this flavor (tokio's own
    /// documented restriction — a `current_thread` runtime has exactly one
    /// worker and nowhere to move other queued tasks to), so
    /// `send_and_wait_via_writer_thread` takes the plain-blocking branch
    /// here instead: the wait is bounded by the writer thread's own
    /// responsiveness (an independent OS thread, always making progress),
    /// so this briefly stalls the runtime's single worker rather than
    /// panicking or deadlocking — the accepted tradeoff documented on
    /// `send_and_wait_via_writer_thread` for a control-path API with no
    /// async twin.
    #[tokio::test]
    async fn sync_writer_api_is_runtime_safe_on_a_current_thread_runtime() {
        let dir = temp_dir("partition-runtime-safe-ct");
        let part = Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8).unwrap();
        // See the multi_thread test above for why this seeds via
        // `append_batch_async`, not the sync `append_batch`.
        part.append_batch_async(one_record_batch(0, 8))
            .await
            .unwrap();

        part.set_leader_epoch(3).unwrap();
        assert_eq!(part.leader_epoch(), 3);

        part.flush_meta().unwrap();

        part.set_hw_tracking(HwTracking::Manual);
        assert_eq!(part.truncate_to_offset(1).unwrap(), 1);
        assert_eq!(part.log_end_offset(), 1);
    }
}
