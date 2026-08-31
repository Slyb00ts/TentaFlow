// ===== File: segment.rs — append-only log file, roll policy, crash recovery =====
//
// PLAN.md §2.2/§2.3: one segment is one `{base_offset:020}.log` file, the
// unit a partition rolls at (configurable, `RollPolicy::default()` is
// 256 MiB / 1 h / 100k batches — M1-R2 decision 5, down from the original
// PLAN default of 1 GiB after M0-WYNIKI found preallocation had no
// measurable effect under real fsync). Writes go through
// `pwrite` (positional, no shared file cursor) so a future concurrent
// reader on the same fd — or this same struct used from a single writer
// thread that never seeks — cannot race a cursor. Recovery only rescans the
// *active* (last) segment: earlier segments were sealed by a clean roll
// (fsynced, closed) and PLAN treats them as trustworthy without a scan.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::batch::{BatchHeader, BATCH_HEADER_LEN};
use crate::error::{BusError, Result};

pub fn log_path(dir: &Path, base_offset: u64) -> PathBuf {
    dir.join(format!("{base_offset:020}.log"))
}

pub fn offset_index_path(dir: &Path, base_offset: u64) -> PathBuf {
    dir.join(format!("{base_offset:020}.oidx"))
}

pub fn time_index_path(dir: &Path, base_offset: u64) -> PathBuf {
    dir.join(format!("{base_offset:020}.tidx"))
}

#[cfg(unix)]
pub fn pread_exact(file: &File, pos: u64, buf: &mut [u8]) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(buf, pos)
}

#[cfg(unix)]
pub fn pwrite_all(file: &File, pos: u64, buf: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.write_all_at(buf, pos)
}

#[cfg(windows)]
pub fn pread_exact(file: &File, pos: u64, buf: &mut [u8]) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut off = pos;
    let mut read = 0;
    while read < buf.len() {
        let n = file.seek_read(&mut buf[read..], off)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "pread_exact: short read",
            ));
        }
        read += n;
        off += n as u64;
    }
    Ok(())
}

#[cfg(windows)]
pub fn pwrite_all(file: &File, pos: u64, buf: &[u8]) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut off = pos;
    let mut written = 0;
    while written < buf.len() {
        let n = file.seek_write(&buf[written..], off)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "pwrite_all: short write",
            ));
        }
        written += n;
        off += n as u64;
    }
    Ok(())
}

/// Best-effort file-extent preallocation to `len` bytes. Purely a
/// performance hint: both `device_ceiling`'s prealloc variant and
/// `Segment::create_new` call this so a growing-file journal-update cost is
/// not baked into every fsync. Never fails the caller — an unsupported
/// filesystem/platform just gets ordinary grow-on-write behavior, which is
/// what every path did before this change.
#[cfg(target_os = "macos")]
fn preallocate_file(file: &File, len: u64) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let mut store = libc::fstore_t {
        fst_flags: libc::F_ALLOCATECONTIG,
        fst_posmode: libc::F_PEOFPOSMODE,
        fst_offset: 0,
        fst_length: len as libc::off_t,
        fst_bytesalloc: 0,
    };
    let fd = file.as_raw_fd();
    // SAFETY: `fd` is a valid, open file descriptor owned by `file` for the
    // duration of this call; `store` is a validly-initialized `fstore_t`
    // whose lifetime covers the `fcntl` call. `F_PREALLOCATE` only reserves
    // extents and does not resize the file (`st_size` is unaffected), so
    // this cannot corrupt already-durable data.
    let ret = unsafe { libc::fcntl(fd, libc::F_PREALLOCATE, &mut store) };
    if ret == -1 {
        // Retry without the contiguous-allocation hint — some filesystems
        // (e.g. non-APFS volumes) reject F_ALLOCATECONTIG but accept a
        // fragmented preallocation.
        store.fst_flags = libc::F_ALLOCATEALL;
        // SAFETY: same as above.
        let ret2 = unsafe { libc::fcntl(fd, libc::F_PREALLOCATE, &mut store) };
        if ret2 == -1 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn preallocate_file(file: &File, len: u64) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    // SAFETY: `fd` is a valid, open file descriptor owned by `file` for the
    // duration of this call. `fallocate(mode=0)` reserves the byte range
    // and may extend `st_size` up to `len`, which is fine — `Segment::len`
    // tracks the *logical* write position independently and never reads
    // file metadata to determine it.
    let ret = unsafe { libc::fallocate(fd, 0, 0, len as libc::off_t) };
    if ret == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
fn preallocate_file(_file: &File, _len: u64) -> std::io::Result<()> {
    // No portable preallocation primitive (Windows, iOS, other Unixes not
    // covered above); grow-on-write is the fallback everywhere already.
    Ok(())
}

/// fsyncs a directory so that the directory *entry* for a file just created
/// in it (or truncated within it) is itself durable — `File::sync_data` /
/// `sync_all` on the file only guarantees the file's own bytes/metadata,
/// not that the containing directory recorded the name — a crash right
/// after a confirmed `fsync_batch` append could otherwise make the whole
/// segment file disappear on reboot even though its data was flushed.
fn fsync_dir(dir: &Path) -> Result<()> {
    File::open(dir)
        .and_then(|f| f.sync_all())
        .map_err(|e| BusError::io(dir, e))
}

/// Segment roll thresholds — first one crossed wins (PLAN §2.3).
#[derive(Debug, Clone, Copy)]
pub struct RollPolicy {
    pub max_bytes: u64,
    pub max_age: Duration,
    pub max_batches: u32,
    /// Whether `Segment::create_new` should best-effort preallocate the new
    /// segment file's extents up to `max_bytes` (`preallocate_file`'s doc).
    /// Defaults to `false` (TentaBus M1-R2 decision 5, `M0-WYNIKI.md`):
    /// benchmarking under real `fsync` found no measurable throughput
    /// benefit from preallocation, while every low-traffic install pays its
    /// full cost up front — a topic with 8 partitions used to reserve 8 ×
    /// `max_bytes` on disk (8 GiB at the old 1 GiB default) on its very
    /// first write, regardless of how much data it ever held. Left
    /// configurable rather than removed outright: a deployment that DOES
    /// measure a benefit on its own storage (e.g. a filesystem/device where
    /// extent-growth journaling is the bottleneck) can still opt back in.
    pub preallocate: bool,
}

impl Default for RollPolicy {
    fn default() -> Self {
        Self {
            // 256 MiB (down from 1 GiB): together with `preallocate: false`
            // below, this bounds how much a single still-growing segment can
            // grow before a crash-recovery rescan (`Segment::
            // open_active_with_recovery`, bounded by `max_bytes`) has to
            // walk — M1-R2 decision 5.
            max_bytes: 256 * 1024 * 1024,
            max_age: Duration::from_secs(3600),
            max_batches: 100_000,
            preallocate: false,
        }
    }
}

/// One batch found intact during active-segment recovery, with its byte
/// position — everything the caller needs to rebuild the sparse index for
/// the rescanned tail without re-reading the file a second time.
#[derive(Debug, Clone)]
pub struct RecoveredBatch {
    pub file_pos: u64,
    pub header: BatchHeader,
}

/// The result of walking a segment forward to find where a
/// `Partition::truncate_to_offset` (PLAN-M2 §1a) cut lands: the byte length
/// and batch count to roll the segment back to, and the resulting
/// `log_end_offset` (the base_offset of the first dropped batch, or the
/// segment's own end if nothing was dropped).
#[derive(Debug, Clone, Copy)]
pub struct TruncateBoundary {
    pub new_len: u64,
    pub new_batch_count: u32,
    pub new_next_offset: u64,
}

/// Scans a segment file (`base_offset` is this segment's own, `full_len` its
/// current on-disk length) from byte 0 to find the last batch boundary at
/// or before `target_offset` — the largest valid `log_end_offset` that does
/// not exceed `target_offset`. Reuses the same self-validating offset-chain
/// and CRC walk `Segment::open_active_with_recovery` performs: batches are
/// atomic append units, so a `target_offset` that falls *inside* a batch
/// (rather than exactly on its boundary) discards that whole batch, never a
/// partial one — the stop condition below is on `next_offset() >
/// target_offset`, not `base_offset >= target_offset`, precisely to keep a
/// straddling batch out of the retained region instead of half-keeping it.
///
/// A free function taking a plain `&File` (rather than a `&Segment` method)
/// so `Partition`'s writer-thread truncate handler can call it against
/// either the still-open active segment or a freshly reopened previously-
/// sealed one without needing a `Segment` in both cases up front. Never
/// fails: an I/O error or a decode failure partway through is treated the
/// same as reaching a boundary — stop and report what was validated so
/// far — matching `open_active_with_recovery`'s own tolerant scan.
pub fn scan_truncate_boundary(
    file: &File,
    base_offset: u64,
    full_len: u64,
    target_offset: u64,
) -> TruncateBoundary {
    let mut pos: u64 = 0;
    let mut expected_offset = base_offset;
    let mut batch_count: u32 = 0;
    loop {
        if pos + BATCH_HEADER_LEN as u64 > full_len {
            break;
        }
        let mut hdr_buf = [0u8; BATCH_HEADER_LEN];
        if pread_exact(file, pos, &mut hdr_buf).is_err() {
            break;
        }
        let header = match BatchHeader::decode(&hdr_buf) {
            Ok(h) => h,
            Err(_) => break,
        };
        if header.base_offset != expected_offset {
            break;
        }
        let total = BATCH_HEADER_LEN as u64 + header.body_len as u64;
        if pos + total > full_len {
            break;
        }
        if header.next_offset() > target_offset {
            // This batch (and everything after it) is past the cut —
            // stop *before* including it, even though its own
            // `base_offset` may still be `< target_offset`.
            break;
        }
        let mut body_buf = vec![0u8; header.body_len as usize];
        if pread_exact(file, pos + BATCH_HEADER_LEN as u64, &mut body_buf).is_err() {
            break;
        }
        if crc32c::crc32c(&body_buf) != header.crc32c {
            break;
        }
        expected_offset = header.next_offset();
        batch_count += 1;
        pos += total;
    }
    TruncateBoundary {
        new_len: pos,
        new_batch_count: batch_count,
        new_next_offset: expected_offset,
    }
}

/// An append-only segment file. Owned exclusively by the partition's single
/// writer thread; readers never touch this struct, they open their own
/// `File` on the same path (PLAN §5.3.4: "czytelnicy nigdy nie dotykają
/// pisarza").
pub struct Segment {
    base_offset: u64,
    path: PathBuf,
    file: File,
    len: u64,
    batch_count: u32,
    created_at: Instant,
}

impl Segment {
    pub fn base_offset(&self) -> u64 {
        self.base_offset
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn batch_count(&self) -> u32 {
        self.batch_count
    }

    /// The raw file handle, for `scan_truncate_boundary`'s positional
    /// reads. Not exposed for writing through this accessor — every
    /// mutation still goes through `Segment`'s own methods.
    pub(crate) fn file(&self) -> &File {
        &self.file
    }

    /// Creates a brand-new, empty segment file, preallocated (best-effort)
    /// to `prealloc_bytes` — the roll policy's `max_bytes`, so the segment
    /// occupies its worst-case footprint on disk from the first write
    /// instead of growing (and paying journal/metadata update cost on every
    /// `fsync`) one append at a time. The *logical* length (`len()`)
    /// still starts at 0 and only grows via
    /// `append` — preallocation never changes what a reader sees as valid
    /// data. Fails if a file already exists at that base offset — a
    /// partition never overwrites history.
    pub fn create_new(dir: &Path, base_offset: u64, prealloc_bytes: u64) -> Result<Self> {
        std::fs::create_dir_all(dir).map_err(|e| BusError::io(dir, e))?;
        let path = log_path(dir, base_offset);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| BusError::io(&path, e))?;
        if prealloc_bytes > 0 {
            if let Err(e) = preallocate_file(&file, prealloc_bytes) {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "segment preallocation failed; falling back to grow-on-write"
                );
            }
        }
        fsync_dir(dir)?;
        Ok(Self {
            base_offset,
            path,
            file,
            len: 0,
            batch_count: 0,
            created_at: Instant::now(),
        })
    }

    /// Opens a sealed (non-active) segment without scanning its content —
    /// PLAN §2.2 recovery only re-validates the tail of the *last* segment;
    /// earlier segments were rolled cleanly (fsynced, then closed) and are
    /// trusted as-is. `batch_count` is left at 0 here since roll decisions
    /// are only ever made against the active segment. Opened without write
    /// access — sealed segments are read-only history.
    pub fn open_sealed(dir: &Path, base_offset: u64) -> Result<Self> {
        let path = log_path(dir, base_offset);
        let file = OpenOptions::new()
            .read(true)
            .open(&path)
            .map_err(|e| BusError::io(&path, e))?;
        let len = file.metadata().map_err(|e| BusError::io(&path, e))?.len();
        Ok(Self {
            base_offset,
            path,
            file,
            len,
            batch_count: 0,
            created_at: Instant::now(),
        })
    }

    /// Reopens an existing segment file for read+write without scanning its
    /// content — used only by `Partition::truncate_to_offset` (PLAN-M2
    /// §1a) to promote a previously-sealed segment back to active once
    /// truncation has deleted every segment that used to come after it.
    /// Unlike `open_active_with_recovery`, the caller is responsible for
    /// finding and applying the correct boundary itself
    /// (`scan_truncate_boundary` + `truncate_to_boundary`) — this
    /// constructor only opens the fd; `batch_count` is a placeholder (`0`)
    /// until that call fixes it up.
    pub fn reopen_for_write(dir: &Path, base_offset: u64) -> Result<Self> {
        let path = log_path(dir, base_offset);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| BusError::io(&path, e))?;
        let len = file.metadata().map_err(|e| BusError::io(&path, e))?.len();
        Ok(Self {
            base_offset,
            path,
            file,
            len,
            batch_count: 0,
            created_at: Instant::now(),
        })
    }

    /// Opens the active (last) segment, scanning it from byte 0 to find the
    /// last batch with an intact header, an offset chain consistent with
    /// this segment's `base_offset`, and a matching body CRC, then
    /// truncates anything after it. This is the crash-recovery path from
    /// PLAN §2.2: "otwarcie partycji skanuje ogon ostatniego segmentu, ucina
    /// po ostatnim batchu z poprawnym CRC". A full-segment scan is bounded
    /// and cheap here (segments cap at 1 GiB / 100k batches) and only runs
    /// once at startup, never on the append hot path.
    ///
    /// Chain validation: the batch header's 36 non-CRC bytes
    /// (including `base_offset`/`record_count`) are not themselves checksum
    /// -protected (PLAN §2.3: "crc32c nad body" only). A single corrupted
    /// bit in `base_offset` with a coincidentally-valid body CRC would
    /// otherwise be accepted as-is, producing an offset index entry that
    /// points anywhere and can underflow every offset-delta computation
    /// downstream. The scan tracks `expected`, starting at this segment's
    /// own `base_offset`, and requires each accepted batch's
    /// `header.base_offset == expected`, then advances `expected` to that
    /// batch's `next_offset()` — turning an unprotected header into a
    /// self-validating chain and stopping recovery exactly where a
    /// corrupted-but-CRC-valid batch would otherwise start.
    ///
    /// Returns the recovered segment plus every batch found intact, so the
    /// caller can rebuild that segment's sparse index from the scan instead
    /// of trying to reconcile a possibly-stale on-disk index.
    pub fn open_active_with_recovery(
        dir: &Path,
        base_offset: u64,
    ) -> Result<(Self, Vec<RecoveredBatch>)> {
        let path = log_path(dir, base_offset);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| BusError::io(&path, e))?;
        let full_len = file.metadata().map_err(|e| BusError::io(&path, e))?.len();

        let mut pos: u64 = 0;
        let mut expected_offset = base_offset;
        let mut recovered = Vec::new();
        loop {
            if pos + BATCH_HEADER_LEN as u64 > full_len {
                break;
            }
            let mut hdr_buf = [0u8; BATCH_HEADER_LEN];
            if pread_exact(&file, pos, &mut hdr_buf).is_err() {
                break;
            }
            let header = match BatchHeader::decode(&hdr_buf) {
                Ok(h) => h,
                Err(_) => break, // not a valid batch boundary — stop here
            };
            if header.base_offset != expected_offset {
                // Either genuine corruption of an unprotected header field,
                // or (equally) the tail of a torn write that happened to
                // decode as a plausible-looking header. Either way, nothing
                // past this point in the file can be trusted.
                tracing::warn!(
                    path = %path.display(),
                    pos,
                    expected_offset,
                    found_base_offset = header.base_offset,
                    "segment recovery stopped: batch base_offset breaks the expected offset chain"
                );
                break;
            }
            let total = BATCH_HEADER_LEN as u64 + header.body_len as u64;
            if pos + total > full_len {
                break; // declared body_len runs past EOF: torn write
            }
            let mut body_buf = vec![0u8; header.body_len as usize];
            if pread_exact(&file, pos + BATCH_HEADER_LEN as u64, &mut body_buf).is_err() {
                break;
            }
            if crc32c::crc32c(&body_buf) != header.crc32c {
                break; // header landed intact, body is torn/corrupt
            }
            expected_offset = header.next_offset();
            recovered.push(RecoveredBatch {
                file_pos: pos,
                header,
            });
            pos += total;
        }

        if pos != full_len {
            tracing::warn!(
                path = %path.display(),
                valid_len = pos,
                on_disk_len = full_len,
                "truncating segment tail after the last valid batch (torn write, corruption, or unused preallocated space)"
            );
            file.set_len(pos).map_err(|e| BusError::io(&path, e))?;
            file.sync_all().map_err(|e| BusError::io(&path, e))?;
            fsync_dir(dir)?;
        }

        let batch_count = recovered.len() as u32;
        let segment = Segment {
            base_offset,
            path,
            file,
            len: pos,
            batch_count,
            created_at: Instant::now(),
        };
        Ok((segment, recovered))
    }

    /// Whether this segment should be sealed and rolled before the next
    /// append. `created_at` resets on process restart (this struct does not
    /// persist segment creation time across recovery), so the age threshold
    /// effectively restarts after a crash — acceptable for M0, revisit if
    /// M1 needs an exact wall-clock roll cadence.
    pub fn should_roll(&self, policy: &RollPolicy) -> bool {
        self.len >= policy.max_bytes
            || self.batch_count >= policy.max_batches
            || self.created_at.elapsed() >= policy.max_age
    }

    /// Appends one already-built batch buffer at the current end of the
    /// file via a positional write. Returns the byte offset it landed at.
    pub fn append(&mut self, batch: &[u8]) -> Result<u64> {
        let pos = self.len;
        pwrite_all(&self.file, pos, batch).map_err(|e| BusError::io(&self.path, e))?;
        self.len += batch.len() as u64;
        self.batch_count += 1;
        Ok(pos)
    }

    /// Reverts the segment's logical bookkeeping (`len`, `batch_count`) to
    /// `len`, which must be a value this segment's `len()` held at some
    /// earlier point, and `batch_count` back by `batches_rolled_back`. Used
    /// whenever bytes were physically written but a subsequent step failed
    /// to make them durable/indexed, so the offset(s) for those bytes were
    /// never published: a single failed index append rolls back exactly one
    /// batch (`append_one`'s own error branches), while a group-commit fsync
    /// failure rolls back every batch this group appended to the segment
    /// that is *currently* active (`process_group`) in one call. Either
    /// way, the next append lands at exactly `len` again and
    /// physically overwrites the orphaned bytes. Does not touch the file
    /// itself: the orphaned bytes stay on disk past the new logical `len`,
    /// invisible to any reader (which never reads past a published segment
    /// length) and self-healing on the next crash-recovery scan even if a
    /// crash lands in the narrow window before the retry overwrites them.
    ///
    /// Invariant this exists to preserve: a batch must never be simultaneously
    /// (a) reported to its producer as failed and (b) recoverable as valid
    /// log content after a restart. Failing to roll back `len` here after a
    /// reported failure is exactly how that invariant breaks — bytes stay
    /// "published" via the segment's logical length even though the caller
    /// was told they did not land.
    pub(crate) fn rollback_len(&mut self, len: u64, batches_rolled_back: u32) {
        debug_assert!(len <= self.len);
        self.len = len;
        self.batch_count = self.batch_count.saturating_sub(batches_rolled_back);
    }

    /// Truncates the file down to the segment's logical length, discarding
    /// any unwritten preallocated tail. Called when sealing a segment at
    /// roll time so a segment that never grew all the way to
    /// `RollPolicy::max_bytes` does not permanently waste that preallocated
    /// disk space, applied without leaking that preallocated cost past
    /// the segment's own lifetime.
    pub fn truncate_to_len(&self) -> Result<()> {
        self.file
            .set_len(self.len)
            .map_err(|e| BusError::io(&self.path, e))
    }

    /// Physically truncates this segment's file down to `boundary.new_len`
    /// (from `scan_truncate_boundary`), discarding every byte from the
    /// first dropped batch onward, and fsyncs the result — a truncate is
    /// exactly as durability-sensitive as a roll (both change what a
    /// restart's crash recovery will see), so it is not left to the next
    /// group's fsync policy to cover.
    pub fn truncate_to_boundary(&mut self, boundary: &TruncateBoundary) -> Result<()> {
        self.file
            .set_len(boundary.new_len)
            .map_err(|e| BusError::io(&self.path, e))?;
        self.len = boundary.new_len;
        self.batch_count = boundary.new_batch_count;
        self.fsync()
    }

    /// `fdatasync`-equivalent durability for the segment's data — the
    /// portable floor every platform supports.
    pub fn fsync(&self) -> Result<()> {
        self.file
            .sync_data()
            .map_err(|e| BusError::io(&self.path, e))
    }

    /// Stronger durability barrier on macOS/iOS: `fcntl(F_FULLFSYNC)` forces
    /// the drive itself to flush its write cache, unlike `fsync`/`fdatasync`,
    /// which on Apple platforms only pushes data to the drive's volatile
    /// cache — `sync_data()` alone is not a durability barrier on macOS
    /// (PLAN §5.3.6). Falls back to `fsync()` on every other platform and if
    /// the drive/FS rejects the fcntl (e.g. some external/virtual disks
    /// return `ENOTSUP`) — reported to the caller as the ordinary `fsync()`
    /// result in that case rather than failing durability outright.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn fsync_full(&self) -> Result<()> {
        use std::os::unix::io::AsRawFd;
        let fd = self.file.as_raw_fd();
        // SAFETY: `fd` is a valid, open file descriptor owned by `self.file`
        // for the duration of this call; `F_FULLFSYNC` takes no argument
        // pointer.
        let ret = unsafe { libc::fcntl(fd, libc::F_FULLFSYNC) };
        if ret == -1 {
            tracing::warn!(
                path = %self.path.display(),
                error = %std::io::Error::last_os_error(),
                "F_FULLFSYNC rejected by filesystem/device; falling back to fsync_data"
            );
            return self.fsync();
        }
        Ok(())
    }

    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    pub fn fsync_full(&self) -> Result<()> {
        self.fsync()
    }
}

/// Maps a segment file read-only for `benches/read_perf.rs`'s pread-vs-mmap
/// comparison (PLAN §2.1: reader mmap is opt-in, decided by that bench's
/// data, never the default). Not used by `Partition`/`PartitionReader` —
/// M0 keeps the production read path on `pread` until a later milestone
/// flips the default based on measured numbers.
#[cfg(feature = "mmap-read")]
pub fn mmap_open(path: &Path) -> Result<memmap2::Mmap> {
    let file = File::open(path).map_err(|e| BusError::io(path, e))?;
    // SAFETY: this crate does not guarantee the mapped file is free of
    // concurrent truncation/roll while the mapping is alive (the classic
    // mmap hazard, SIGBUS on a torn-away page). Acceptable for an opt-in,
    // default-off benchmark probe against segments this same process just
    // finished writing and sealing; not acceptable as-is for a production
    // read path without an explicit lifetime/roll-coordination contract.
    unsafe { memmap2::Mmap::map(&file) }.map_err(|e| BusError::io(path, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::batch::{BatchBuilder, RecordInput};
    use crate::test_support::temp_dir;
    use bytes::Bytes;
    use std::os::unix::fs::FileExt;

    const TEST_PREALLOC: u64 = 1024 * 1024 * 1024;

    fn build_batch(base_offset: u64, n_records: usize) -> Bytes {
        let mut b = BatchBuilder::new(base_offset, 1);
        for i in 0..n_records {
            b.push(RecordInput::new(Bytes::from(vec![0x42; 32]), i as i64))
                .unwrap();
        }
        b.build().unwrap()
    }

    #[test]
    fn append_advances_length_and_batch_count() {
        let dir = temp_dir("segment-append");
        let mut seg = Segment::create_new(&dir, 0, TEST_PREALLOC).unwrap();
        let batch1 = build_batch(0, 3);
        let pos1 = seg.append(&batch1).unwrap();
        assert_eq!(pos1, 0);
        let batch2 = build_batch(3, 2);
        let pos2 = seg.append(&batch2).unwrap();
        assert_eq!(pos2, batch1.len() as u64);
        assert_eq!(seg.len(), (batch1.len() + batch2.len()) as u64);
        assert_eq!(seg.batch_count(), 2);
    }

    /// `rollback_len` must be able to undo more than one batch in a single
    /// call — the shape `process_group` needs when a group-commit fsync
    /// fails after several jobs landed on the same active segment.
    /// Rolling back must not touch the file itself (the orphaned bytes stay
    /// on disk, past the new logical `len`) and a subsequent append must
    /// land at exactly the rolled-back position, overwriting them.
    #[test]
    fn rollback_len_undoes_multiple_batches_and_next_append_overwrites_them() {
        let dir = temp_dir("segment-rollback-multi");
        let mut seg = Segment::create_new(&dir, 0, TEST_PREALLOC).unwrap();

        let batch1 = build_batch(0, 3);
        let batch2 = build_batch(3, 2);
        let batch3 = build_batch(5, 1);
        seg.append(&batch1).unwrap();
        let pos_before_group = seg.len();
        seg.append(&batch2).unwrap();
        seg.append(&batch3).unwrap();
        assert_eq!(seg.batch_count(), 3);
        let len_with_all_three = seg.len();
        assert!(len_with_all_three > pos_before_group);

        // Simulate a group-commit fsync failure covering batch2 and batch3:
        // roll back both in one call.
        seg.rollback_len(pos_before_group, 2);
        assert_eq!(seg.len(), pos_before_group);
        assert_eq!(seg.batch_count(), 1);

        // The physically-written bytes from batch2 are still on disk past
        // the rolled-back logical length — `rollback_len` never touches
        // the file itself.
        let mut orphaned = vec![0u8; batch2.len()];
        seg.file
            .read_exact_at(&mut orphaned, pos_before_group)
            .unwrap();
        assert_eq!(orphaned, batch2.as_ref());

        // ...but a retry with the same batch lands at exactly the
        // rolled-back position and overwrites them, rather than appending
        // past a gap.
        let retry_pos = seg.append(&batch2).unwrap();
        assert_eq!(retry_pos, pos_before_group);
        assert_eq!(seg.len(), pos_before_group + batch2.len() as u64);
        assert_eq!(seg.batch_count(), 2);

        let mut readback = vec![0u8; batch2.len()];
        seg.file.read_exact_at(&mut readback, retry_pos).unwrap();
        assert_eq!(readback, batch2.as_ref());
    }

    #[test]
    fn should_roll_on_max_bytes_and_max_batches() {
        let dir = temp_dir("segment-roll");
        let mut seg = Segment::create_new(&dir, 0, TEST_PREALLOC).unwrap();
        let batch = build_batch(0, 1);
        seg.append(&batch).unwrap();

        let tight_bytes = RollPolicy {
            max_bytes: batch.len() as u64,
            ..RollPolicy::default()
        };
        assert!(seg.should_roll(&tight_bytes));

        let tight_batches = RollPolicy {
            max_batches: 1,
            ..RollPolicy::default()
        };
        assert!(seg.should_roll(&tight_batches));

        assert!(!seg.should_roll(&RollPolicy::default()));
    }

    /// Simulates a process kill mid-append: two batches are written and
    /// durable, then a third batch's bytes are written directly to the file
    /// (bypassing `Segment::append`, standing in for an OS-level partial
    /// write before a crash) but truncated partway through. Recovery must
    /// land exactly at the end of the second batch.
    #[test]
    fn recovery_truncates_torn_write_to_last_complete_batch() {
        let dir = temp_dir("segment-recovery");
        let (good_len, batch1_len) = {
            let mut seg = Segment::create_new(&dir, 0, TEST_PREALLOC).unwrap();
            let batch1 = build_batch(0, 4);
            let batch1_len = batch1.len();
            seg.append(&batch1).unwrap();
            let batch2 = build_batch(4, 6);
            seg.append(&batch2).unwrap();
            seg.fsync().unwrap();
            (seg.len(), batch1_len)
        };

        // Append a torn batch 3 directly: a well-formed 40-byte header
        // declaring a body, but only half the body bytes actually landed.
        let batch3 = build_batch(10, 5);
        let torn_len = BATCH_HEADER_LEN + (batch3.len() - BATCH_HEADER_LEN) / 2;
        {
            let path = log_path(&dir, 0);
            let file = OpenOptions::new().write(true).open(&path).unwrap();
            file.write_all_at(&batch3[..torn_len], good_len).unwrap();
        }
        let on_disk_len_before_recovery = std::fs::metadata(log_path(&dir, 0)).unwrap().len();
        assert_eq!(on_disk_len_before_recovery, good_len + torn_len as u64);

        let (recovered_seg, recovered_batches) =
            Segment::open_active_with_recovery(&dir, 0).unwrap();

        assert_eq!(recovered_batches.len(), 2);
        assert_eq!(recovered_batches[0].file_pos, 0);
        assert_eq!(recovered_batches[0].header.record_count, 4);
        assert_eq!(recovered_batches[1].file_pos, batch1_len as u64);
        assert_eq!(recovered_batches[1].header.record_count, 6);

        assert_eq!(recovered_seg.len(), good_len);
        assert_eq!(recovered_seg.batch_count(), 2);

        let on_disk_len_after_recovery = std::fs::metadata(log_path(&dir, 0)).unwrap().len();
        assert_eq!(
            on_disk_len_after_recovery, good_len,
            "the torn tail must be physically truncated from the file, not just ignored in memory"
        );

        // The recovered segment must still be writable, and the next batch
        // must land right after the last valid one — no gap, no overlap.
        let mut seg = recovered_seg;
        let batch4 = build_batch(10, 1);
        let pos4 = seg.append(&batch4).unwrap();
        assert_eq!(pos4, good_len);
    }

    #[test]
    fn recovery_with_no_corruption_recovers_everything() {
        let dir = temp_dir("segment-recovery-clean");
        {
            let mut seg = Segment::create_new(&dir, 0, TEST_PREALLOC).unwrap();
            for i in 0..5u64 {
                let batch = build_batch(i, 1);
                seg.append(&batch).unwrap();
            }
            seg.fsync().unwrap();
        }
        let (seg, recovered) = Segment::open_active_with_recovery(&dir, 0).unwrap();
        assert_eq!(recovered.len(), 5);
        assert_eq!(seg.batch_count(), 5);
        assert_eq!(
            seg.len(),
            std::fs::metadata(log_path(&dir, 0)).unwrap().len()
        );
    }

    /// A header that is otherwise well-formed (valid magic, in-bounds
    /// `body_len`, matching body CRC) but whose `base_offset` does not
    /// continue the segment's offset chain must stop recovery at that point
    /// rather than being accepted.
    #[test]
    fn recovery_stops_at_offset_chain_break() {
        let dir = temp_dir("segment-recovery-chain-break");
        {
            let mut seg = Segment::create_new(&dir, 0, TEST_PREALLOC).unwrap();
            let batch1 = build_batch(0, 4);
            seg.append(&batch1).unwrap();
            seg.fsync().unwrap();
        }
        // Append a second, individually well-formed and CRC-correct batch,
        // but built with the wrong base_offset for this chain (10 instead
        // of the expected 4) — simulating a corrupted `base_offset` field
        // that happens not to affect the body/CRC at all.
        let wrong_chain_batch = build_batch(10, 3);
        {
            let path = log_path(&dir, 0);
            let file = OpenOptions::new().write(true).open(&path).unwrap();
            let good_len = std::fs::metadata(&path).unwrap().len();
            file.write_all_at(&wrong_chain_batch, good_len).unwrap();
        }

        let (seg, recovered) = Segment::open_active_with_recovery(&dir, 0).unwrap();
        assert_eq!(
            recovered.len(),
            1,
            "only the first, chain-consistent batch is kept"
        );
        assert_eq!(recovered[0].header.base_offset, 0);
        // The chain-breaking batch's bytes must be physically truncated
        // away, exactly like a torn write.
        assert_eq!(seg.len(), build_batch(0, 4).len() as u64);
    }

    #[test]
    fn recovery_handles_header_shorter_than_40_bytes_at_tail() {
        let dir = temp_dir("segment-recovery-short-header");
        let good_len = {
            let mut seg = Segment::create_new(&dir, 0, TEST_PREALLOC).unwrap();
            let batch1 = build_batch(0, 2);
            seg.append(&batch1).unwrap();
            seg.fsync().unwrap();
            seg.len()
        };
        // Append fewer than BATCH_HEADER_LEN stray bytes at the tail —
        // stands in for a crash mid-header-write.
        {
            let path = log_path(&dir, 0);
            let file = OpenOptions::new().write(true).open(&path).unwrap();
            file.write_all_at(&[0xAAu8; 10], good_len).unwrap();
        }
        let (seg, recovered) = Segment::open_active_with_recovery(&dir, 0).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(seg.len(), good_len);
        assert_eq!(
            std::fs::metadata(log_path(&dir, 0)).unwrap().len(),
            good_len
        );
    }

    #[test]
    fn recovery_handles_all_zero_tail_from_preallocation() {
        let dir = temp_dir("segment-recovery-zero-tail");
        let good_len = {
            let mut seg = Segment::create_new(&dir, 0, TEST_PREALLOC).unwrap();
            let batch1 = build_batch(0, 2);
            seg.append(&batch1).unwrap();
            seg.fsync().unwrap();
            seg.len()
        };
        // Zero-filled tail, e.g. from preallocation that was never written.
        // magic_version decodes as 0 (!= MAGIC_V1) -> BadMagic -> stop.
        {
            let path = log_path(&dir, 0);
            let file = OpenOptions::new().write(true).open(&path).unwrap();
            file.write_all_at(&[0u8; 4096], good_len).unwrap();
        }
        let (seg, recovered) = Segment::open_active_with_recovery(&dir, 0).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(seg.len(), good_len);
        assert_eq!(
            std::fs::metadata(log_path(&dir, 0)).unwrap().len(),
            good_len,
            "zero-filled preallocated tail must be truncated away like any other invalid trailer"
        );
    }

    #[test]
    fn recovery_handles_body_len_pointing_past_eof() {
        let dir = temp_dir("segment-recovery-body-past-eof");
        let good_len = {
            let mut seg = Segment::create_new(&dir, 0, TEST_PREALLOC).unwrap();
            let batch1 = build_batch(0, 2);
            seg.append(&batch1).unwrap();
            seg.fsync().unwrap();
            seg.len()
        };
        // A syntactically valid 40-byte header (right magic, right offset
        // chain) whose body_len claims far more bytes than actually follow
        // it in the file.
        let header = BatchHeader {
            body_len: 10_000_000,
            base_offset: 2,
            record_count: 1,
            last_offset_delta: 0,
            base_timestamp_ms: 0,
            magic_version: crate::batch::MAGIC_V1,
            flags: 0,
            producer_epoch: 1,
            crc32c: 0xDEAD_BEEF, // irrelevant, the EOF check must fire first
        };
        let mut buf = [0u8; BATCH_HEADER_LEN];
        header.encode(&mut buf);
        {
            let path = log_path(&dir, 0);
            let file = OpenOptions::new().write(true).open(&path).unwrap();
            file.write_all_at(&buf, good_len).unwrap();
        }
        let (seg, recovered) = Segment::open_active_with_recovery(&dir, 0).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(seg.len(), good_len);
    }

    #[test]
    fn recovery_handles_valid_header_with_garbage_body() {
        let dir = temp_dir("segment-recovery-garbage-body");
        let good_len = {
            let mut seg = Segment::create_new(&dir, 0, TEST_PREALLOC).unwrap();
            let batch1 = build_batch(0, 2);
            seg.append(&batch1).unwrap();
            seg.fsync().unwrap();
            seg.len()
        };
        // Header correctly describes a small, in-bounds body, but the body
        // bytes are garbage that will fail the CRC check.
        let header = BatchHeader {
            body_len: 16,
            base_offset: 2,
            record_count: 1,
            last_offset_delta: 0,
            base_timestamp_ms: 0,
            magic_version: crate::batch::MAGIC_V1,
            flags: 0,
            producer_epoch: 1,
            crc32c: 0x1234_5678, // will not match the garbage body below
        };
        let mut buf = [0u8; BATCH_HEADER_LEN];
        header.encode(&mut buf);
        let mut full = buf.to_vec();
        full.extend_from_slice(&[0x99u8; 16]);
        {
            let path = log_path(&dir, 0);
            let file = OpenOptions::new().write(true).open(&path).unwrap();
            file.write_all_at(&full, good_len).unwrap();
        }
        let (seg, recovered) = Segment::open_active_with_recovery(&dir, 0).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(seg.len(), good_len);
        assert_eq!(
            std::fs::metadata(log_path(&dir, 0)).unwrap().len(),
            good_len
        );
    }
}
