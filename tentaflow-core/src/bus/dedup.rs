// =============================================================================
// File: bus/dedup.rs — TentaBus M1: per-record idempotency-key dedup store
// =============================================================================
//
// PLAN.md §3.1 layer 2 (`idempotency_key`, a CEL expression evaluated per
// record — the CEL evaluation itself belongs to `flow_engine/expr.rs`
// integration, out of this file's scope; this module only stores/checks
// already-resolved key bytes) targets >=300k ops/s. An LSM-backed store
// measured well under that target for this per-record workload in earlier
// profiling, which is why this is a dedicated mmapped, fixed-size key store
// instead of a general-purpose key-value engine. Producer idempotency
// (PLAN §3.1 layer 1, `(producer_id, epoch, seq)` per BATCH) stays on fjall
// (`bus/producer.rs`): that workload is one lookup per batch rather than per
// record, so it comfortably fits fjall's throughput.
//
// Design: one preallocated file of `capacity` fixed 32-byte slots, open
// addressing with linear probing, oldest-slot eviction when a probe run
// finds neither an empty slot nor a live duplicate. No key bytes are stored
// — only a 128-bit BLAKE3 hash — so the file size is O(capacity) regardless
// of key length; a 128-bit hash makes an in-window collision astronomically
// unlikely at any capacity this store will realistically run at (birthday
// bound), which is the standard trade-off this kind of cache makes to stay
// O(1) and allocation-free per record.
//
// Concurrency: the slot space is statically partitioned into `shards` equal,
// non-overlapping ranges (capacity is rounded up in `open` so it divides
// evenly by `shards` — see `open`'s doc comment), each guarded by its own
// `parking_lot::Mutex`. Probing for a given key never leaves its shard (the
// shard is chosen from the hash BEFORE probing starts, and linear probing
// wraps modulo the shard's own length), so a slot's shard is a pure
// function of its index — every access to a given slot always goes through
// exactly one mutex, for the whole life of the store. That is what makes
// the raw pointer access below sound under concurrent callers without
// per-slot atomics: it is the same discipline a `Mutex<[Slot]>` would give,
// applied to disjoint sub-slices instead of the whole table, which is what
// lets independent shards run fully in parallel (the concurrency the
// throughput target needs).
//
// Cross-process safety: `open` takes an exclusive OS advisory lock
// (`File::try_lock`) on the backing file and holds it for the store's
// entire lifetime, so a second `open` on the same path — same or a
// different process — fails fast instead of memory-mapping the same bytes
// RW from two uncoordinated writers.

use std::fs::OpenOptions;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use memmap2::MmapMut;
use parking_lot::Mutex;

const SLOT_SIZE: usize = 32;
const HEADER_SIZE: usize = 64;
/// Arbitrary but stable file tag distinguishing a dedup store from any other
/// file an operator might accidentally point this at.
const MAGIC: u64 = 0x5442_5f44_4544_5031; // b"TB_DEDP1" as little-endian u64

/// Floor on a capacity derived from `ttl_ms`/`expected_rate_per_sec`: below
/// this, a short window or low expected rate would collapse the table to a
/// handful of slots, leaving most shards with one or two entries each and
/// defeating the point of sharding. Has no effect when `capacity` is set
/// explicitly.
const MIN_DERIVED_CAPACITY: u64 = 1 << 16; // 65,536 slots = 2 MiB
/// Ceiling on a derived capacity: 16 Mi slots * 32 B/slot = 512 MiB file, a
/// per-topic memory cost `open` accepts as a node-wide default (this is a
/// `BusInitConfig`-level setting, not per-topic — see
/// `DedupConfig::expected_rate_per_sec`'s doc). An operator whose real
/// window*rate would need more than this must set `capacity` explicitly and
/// accept that memory cost themselves; `open` logs a `warn!` whenever this
/// ceiling actually shortens the requested window (see `open`'s doc
/// comment) so that trade-off is never silent.
const MAX_DERIVED_CAPACITY: u64 = 16 * 1024 * 1024;

/// How often (in misses) the eviction-ratio warning below is re-checked.
/// Checking on every miss would mean a division and a float compare on the
/// hot path per record; checking every Nth miss keeps the cost amortized
/// while still catching a sustained problem quickly.
const EVICTION_WARN_CHECK_INTERVAL: u64 = 4096;
/// Above this fraction of misses turning into evictions, the configured
/// dedup window is not actually being honored (entries are being pushed out
/// long before `ttl_ms` elapses) and an operator should know about it.
const EVICTION_WARN_RATIO: f64 = 0.10;

#[derive(Debug, Clone, Copy)]
pub struct DedupConfig {
    /// Explicit slot count, bypassing the `ttl_ms`/`expected_rate_per_sec`
    /// derivation below entirely (including its floor and ceiling). Tests
    /// use this to keep tables small and deterministic; production code
    /// should normally leave this `None` and let capacity track the
    /// configured dedup window.
    pub capacity: Option<usize>,
    /// PLAN §7.1 `dedup_window_ms` (default 24h, configurable 1h-30d per
    /// topic). A slot older than this is treated as absent even if its hash
    /// still matches — a caller does not have to explicitly clear it. Also
    /// feeds the `capacity` derivation below when `capacity` is `None`.
    pub ttl_ms: i64,
    /// Independent lock stripes. Concurrency ceiling: two keys landing in
    /// different shards never contend. Clamped to `[1, capacity]` in
    /// `open`.
    pub shards: usize,
    /// Expected sustained publish rate, used only when `capacity` is
    /// `None`. This is a NODE-level setting (`BusInitConfig.
    /// dedup_expected_rate_per_sec`), not a per-topic one — M1 has no
    /// per-topic plumbing for it yet (deferred to M5's schema/advanced
    /// config registry) — every dedup store on a node derives its capacity
    /// from the same rate. Memory cost is `derived_capacity * 32 bytes`,
    /// where `derived_capacity = (ttl_ms / 1000) * expected_rate_per_sec`
    /// clamped to `[MIN_DERIVED_CAPACITY, MAX_DERIVED_CAPACITY]` and then
    /// rounded up to a multiple of `shards`. E.g. the default (10,000
    /// msg/s, 24h ttl) asks for 864,000,000 slots (~27.7 GiB) and gets
    /// clamped down to the 16 Mi (512 MiB) hard cap, which at 10,000 msg/s
    /// only actually covers ~28 minutes, not 24h — `open` logs a `warn!`
    /// stating both numbers whenever this happens, and `effective_capacity_window_ms`
    /// reports the shorter, real number. An operator who truly needs a
    /// wider window at a given rate must size `capacity` explicitly.
    pub expected_rate_per_sec: u64,
    /// How many slots a single `check_and_insert` probes before giving up
    /// on finding an empty one and evicting the oldest slot it saw instead.
    pub probe_limit: usize,
}

impl Default for DedupConfig {
    fn default() -> Self {
        Self {
            capacity: None,
            ttl_ms: 24 * 3600 * 1000,
            shards: 1024,
            expected_rate_per_sec: 10_000,
            probe_limit: 8,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedupOutcome {
    /// Not seen (or seen but past its TTL window) — now recorded.
    Fresh,
    /// Seen within the TTL window — the caller must treat this record as a
    /// duplicate and skip it.
    Duplicate,
}

/// Derives a slot count from a dedup window and an expected sustained
/// publish rate (see `DedupConfig::expected_rate_per_sec`), clamped to a
/// sane floor/ceiling. A negative or zero `ttl_ms` derives the floor.
/// Returns `(capacity, ceiling_hit)`: `ceiling_hit` is `true` exactly when
/// the raw (unclamped) demand exceeds `MAX_DERIVED_CAPACITY` — the case
/// `open` warns about, because it means the effective window is shorter
/// than `ttl_ms` actually asked for.
fn derive_capacity(ttl_ms: i64, expected_rate_per_sec: u64) -> (usize, bool) {
    let ttl_secs = (ttl_ms.max(0) as u64) / 1000;
    let raw = ttl_secs.saturating_mul(expected_rate_per_sec);
    let capacity = raw.clamp(MIN_DERIVED_CAPACITY, MAX_DERIVED_CAPACITY);
    (capacity as usize, raw > MAX_DERIVED_CAPACITY)
}

/// Rounds `value` up to the nearest multiple of `multiple` (`multiple >
/// 0`). Used so `capacity % shards == 0` always holds, which is what lets
/// every shard use the exact same length and makes every slot index
/// provably `< capacity` (see `open`'s doc comment). `None` when the
/// rounded-up result would overflow `usize` (e.g. a caller-supplied
/// `capacity: Some(usize::MAX)`) — `open` turns that into an `Err` instead
/// of panicking (debug) or silently wrapping to a too-small value that
/// would then divide by zero in `shard_and_start` (release).
fn round_up_to_multiple(value: usize, multiple: usize) -> Option<usize> {
    debug_assert!(multiple > 0);
    if multiple == 0 {
        return None;
    }
    value.div_ceil(multiple).checked_mul(multiple)
}

/// A mmapped, fixed-size, sharded open-addressing key store (see module doc
/// for the full design rationale). One instance owns one on-disk file for
/// its whole lifetime.
#[derive(Debug)]
pub struct MmapDedupStore {
    // Keeps the OS mapping alive; never touched directly after `open` except
    // through `flush()`, which memmap2 defines on `&self`. All slot reads/
    // writes go through `data` instead, under a shard lock (see module doc).
    _mmap: MmapMut,
    data: *mut u8,
    capacity: usize,
    ttl_ms: i64,
    /// The window this store actually honors, which can be shorter than
    /// `ttl_ms` when `capacity` was derived (not set explicitly) and the
    /// `MAX_DERIVED_CAPACITY` ceiling reduced it below what `ttl_ms` would
    /// need at `expected_rate_per_sec` — see `open`'s doc comment and the
    /// `warn!` it logs when that happens. Equal to `ttl_ms` in every other
    /// case (explicit `capacity`, or a derived one that comfortably covers
    /// the requested window).
    effective_capacity_window_ms: i64,
    probe_limit: usize,
    shards: Vec<Mutex<()>>,
    /// Slot count of every shard, uniformly (see `open`: `capacity` is
    /// rounded up so it divides evenly by `shards.len()`).
    shard_size: usize,
    hits: AtomicU64,
    misses: AtomicU64,
    evictions: AtomicU64,
    /// Held for the store's whole lifetime purely for its `Drop` effect:
    /// closing this file releases the OS-level advisory lock acquired in
    /// `open`, so a second `open` on the same path can only succeed after
    /// this one is gone.
    _lock: std::fs::File,
}

// SAFETY: `data` points into `_mmap`'s backing OS mapping. memmap2 never
// moves or reallocates an existing mapping, and `data`/`_mmap` live and die
// together on this struct, so the pointer is valid for exactly the struct's
// lifetime. Every byte range reachable through `data` is only ever read or
// written while the corresponding shard's `Mutex` is held (see
// `check_and_insert`'s SAFETY comment) — the same discipline a
// `Mutex<[Slot]>` per shard would give, just without materializing a Rust
// reference across the whole mapping up front. That per-shard mutual
// exclusion is what makes concurrent access from multiple threads *within
// this process* sound. Across processes, exclusivity instead comes from the
// advisory file lock held in `_lock` (acquired in `open`, released on
// `Drop`) — like any advisory lock, it only protects against other code
// that also calls `try_lock`, not against a process that maps the file
// directly without going through `MmapDedupStore::open`.
unsafe impl Send for MmapDedupStore {}
unsafe impl Sync for MmapDedupStore {}

impl MmapDedupStore {
    /// Opens (creating if absent) the fixed-size backing file at `path`,
    /// takes an exclusive advisory lock on it, and mmaps it.
    ///
    /// `capacity` (from `cfg.capacity` or derived — see
    /// `DedupConfig::expected_rate_per_sec`) is rounded up to the nearest
    /// multiple of `cfg.shards` and that rounded value — not the requested
    /// one — is what gets stored in the file header and used for every
    /// slot-index computation. This is what guarantees `capacity % shards
    /// == 0`, which in turn guarantees every shard has exactly the same
    /// length (`capacity / shards`) and every valid slot index is
    /// `< capacity`: with an even division there is no leftover, non-full
    /// last shard whose empty tail could otherwise compute an out-of-range
    /// index.
    ///
    /// This store treats its backing file as a *cache*, not a durable
    /// source of truth: a file that fails to open cleanly as one — wrong
    /// length for its capacity, a foreign header, or a capacity mismatch
    /// against `cfg` — is recreated as an empty table (with a `warn!` log)
    /// instead of returned as an error. The only way this cache being wrong
    /// can misbehave is a false negative (a duplicate slips through once),
    /// never a false positive (a unique record wrongly dropped) — the safe
    /// direction for a de-duplication layer sitting in front of
    /// at-least-once delivery. A genuinely unrecoverable I/O error (e.g.
    /// permission denied, disk full) still surfaces as `Err`.
    ///
    /// Also surfaces `Err(InvalidInput)` — rather than panicking (debug) or
    /// silently wrapping (release) — when `cfg.capacity` is explicit and so
    /// large that rounding it up to a multiple of `cfg.shards`, or computing
    /// the resulting file length, would overflow (e.g. `capacity:
    /// Some(usize::MAX)`).
    pub fn open(path: &Path, cfg: DedupConfig) -> io::Result<Self> {
        let (requested_capacity, ceiling_hit) = match cfg.capacity {
            Some(explicit) => (explicit.max(1), false),
            None => derive_capacity(cfg.ttl_ms, cfg.expected_rate_per_sec),
        };
        let shards_count = cfg.shards.clamp(1, requested_capacity);
        let capacity = round_up_to_multiple(requested_capacity, shards_count).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "dedup capacity {requested_capacity} rounded up to a multiple of \
                     {shards_count} shards overflows usize"
                ),
            )
        })?;
        let shard_size = capacity / shards_count;
        debug_assert_eq!(
            capacity % shards_count,
            0,
            "capacity must be an exact multiple of shards_count"
        );
        let total_len = (capacity as u64)
            .checked_mul(SLOT_SIZE as u64)
            .and_then(|slots_len| slots_len.checked_add(HEADER_SIZE as u64))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("dedup file size for capacity {capacity} overflows u64"),
                )
            })?;

        // Only meaningful for a DERIVED capacity: an explicit `capacity`
        // is the operator's own sizing decision, not something `open` second-
        // guesses. `ceiling_hit` means the raw demand (ttl_ms *
        // expected_rate_per_sec) exceeded `MAX_DERIVED_CAPACITY`, so the
        // window this store will actually honor under sustained load at
        // `expected_rate_per_sec` is shorter than `ttl_ms` asks for — make
        // that shortfall loud instead of a silent "24h" that is really
        // minutes.
        let effective_capacity_window_ms = if ceiling_hit {
            let effective_secs = capacity as u64 / cfg.expected_rate_per_sec.max(1);
            let effective_ms = (effective_secs.saturating_mul(1000)) as i64;
            tracing::warn!(
                path = %path.display(),
                requested_ttl_ms = cfg.ttl_ms,
                expected_rate_per_sec = cfg.expected_rate_per_sec,
                capacity,
                effective_capacity_window_ms = effective_ms,
                "dedup capacity hit the MAX_DERIVED_CAPACITY ceiling: the configured dedup \
                 window is not actually honored at this expected rate — dedup window \
                 effectively {} minutes, not the requested {} hours",
                effective_ms / 60_000,
                cfg.ttl_ms / 3_600_000,
            );
            effective_ms
        } else {
            cfg.ttl_ms
        };

        // `truncate(false)` explicit (clippy::suspicious_open_options): a
        // reopen of an EXISTING, still-valid dedup file must keep its
        // contents — the whole point of
        // `reopen_with_same_capacity_preserves_entries` is that a restart
        // does not silently wipe recently-deduped keys. Files that turn out
        // NOT to be valid are truncated further down, deliberately and
        // after inspection, not by this open call.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;

        // Exclusive advisory lock, held for the store's entire lifetime via
        // `_lock` below: a second `open` on the same path — same or a
        // different process — must fail fast instead of mapping the same
        // bytes RW from two uncoordinated writers.
        file.try_lock().map_err(|e| match e {
            std::fs::TryLockError::WouldBlock => io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "dedup store file {} is already open by another MmapDedupStore \
                     (same or a different process); only one may hold it at a time",
                    path.display()
                ),
            ),
            std::fs::TryLockError::Error(io_err) => io_err,
        })?;

        let existing_len = file.metadata()?.len();
        let mut reset_reason: Option<String> = None;
        if existing_len != 0 {
            if existing_len != total_len {
                reset_reason = Some(format!(
                    "on-disk length {existing_len} bytes does not match the {total_len} \
                     bytes expected for capacity {capacity}"
                ));
            } else {
                // File is exactly the right size: peek the header (without
                // touching the mapping yet) to decide whether its content
                // is actually usable.
                let mut header = [0u8; 16];
                let mut reader = &file;
                reader.seek(SeekFrom::Start(0))?;
                reader.read_exact(&mut header)?;
                let magic = u64::from_le_bytes(header[0..8].try_into().unwrap());
                if magic != 0 && magic != MAGIC {
                    reset_reason = Some(format!("foreign header magic {magic:#x}"));
                } else if magic == MAGIC {
                    let on_disk_capacity =
                        u64::from_le_bytes(header[8..16].try_into().unwrap()) as usize;
                    if on_disk_capacity != capacity {
                        reset_reason = Some(format!(
                            "on-disk capacity {on_disk_capacity} does not match \
                             requested {capacity}"
                        ));
                    }
                }
                // magic == 0 with the right length: header was never
                // written (e.g. pre-sized by an external tool). Not a
                // broken cache, just first use — the post-mmap "write a
                // fresh header" step below handles it without a reset.
            }
        }

        if let Some(reason) = &reset_reason {
            tracing::warn!(
                path = %path.display(),
                reason = %reason,
                "dedup cache file is unusable; recreating it as an empty cache \
                 (safe direction: a duplicate may slip through once, never a false positive)"
            );
        }

        if existing_len == 0 || reset_reason.is_some() {
            // `set_len(0)` first so a recreate of an oversized/undersized
            // file always ends up zero-filled from byte 0, the same as a
            // brand-new file — never a mix of stale bytes and new length.
            file.set_len(0)?;
            file.set_len(total_len)?;
        }

        let mut mmap = unsafe { MmapMut::map_mut(&file)? };

        let existing_magic = u64::from_le_bytes(mmap[0..8].try_into().unwrap());
        debug_assert!(existing_magic == 0 || existing_magic == MAGIC);
        if existing_magic == 0 {
            mmap[0..8].copy_from_slice(&MAGIC.to_le_bytes());
            mmap[8..16].copy_from_slice(&(capacity as u64).to_le_bytes());
            mmap.flush()?;
        }

        let data = mmap.as_mut_ptr();
        let shards = (0..shards_count).map(|_| Mutex::new(())).collect();

        Ok(Self {
            _mmap: mmap,
            data,
            capacity,
            ttl_ms: cfg.ttl_ms,
            effective_capacity_window_ms,
            probe_limit: cfg.probe_limit.max(1),
            shards,
            shard_size,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            _lock: file,
        })
    }

    fn shard_and_start(&self, hash: &[u8; 16]) -> (usize, usize) {
        let shard_bits = u64::from_le_bytes(hash[0..8].try_into().unwrap());
        let offset_bits = u64::from_le_bytes(hash[8..16].try_into().unwrap());
        let shard = (shard_bits as usize) % self.shards.len();
        let start = (offset_bits as usize) % self.shard_size;
        (shard, start)
    }

    /// Reads slot `idx` without locking — caller must already hold that
    /// slot's shard lock (see the struct-level SAFETY comment).
    ///
    /// SAFETY: `idx < self.capacity` is asserted below and holds by
    /// construction — every caller derives `idx` as `shard * shard_size +
    /// local` with `local < shard_size` and `shard < shards.len()`, and
    /// `open` enforces `capacity == shards.len() * shard_size` exactly (no
    /// remainder), so `idx < capacity` always. That in turn means
    /// `HEADER_SIZE + idx * SLOT_SIZE + SLOT_SIZE <= total_len`, which is
    /// exactly the length `open` sized (and validated) the file to before
    /// mapping it. The caller holding the owning shard's mutex is what
    /// makes concurrent calls from multiple threads not race each other.
    unsafe fn read_slot_unlocked(&self, idx: usize) -> ([u8; 16], i64, bool) {
        debug_assert!(
            idx < self.capacity,
            "slot index {idx} out of bounds for capacity {}",
            self.capacity
        );
        let off = HEADER_SIZE + idx * SLOT_SIZE;
        let ptr = self.data.add(off);
        let mut hash = [0u8; 16];
        std::ptr::copy_nonoverlapping(ptr, hash.as_mut_ptr(), 16);
        let mut ts_bytes = [0u8; 8];
        std::ptr::copy_nonoverlapping(ptr.add(16), ts_bytes.as_mut_ptr(), 8);
        let ts = i64::from_le_bytes(ts_bytes);
        let occupied = std::ptr::read(ptr.add(24)) != 0;
        (hash, ts, occupied)
    }

    /// Writes slot `idx` without locking — same preconditions and safety
    /// argument as `read_slot_unlocked`.
    unsafe fn write_slot_unlocked(&self, idx: usize, hash: [u8; 16], ts_ms: i64) {
        debug_assert!(
            idx < self.capacity,
            "slot index {idx} out of bounds for capacity {}",
            self.capacity
        );
        let off = HEADER_SIZE + idx * SLOT_SIZE;
        let ptr = self.data.add(off);
        std::ptr::copy_nonoverlapping(hash.as_ptr(), ptr, 16);
        std::ptr::copy_nonoverlapping(ts_ms.to_le_bytes().as_ptr(), ptr.add(16), 8);
        std::ptr::write(ptr.add(24), 1u8);
    }

    fn hash_key(key: &[u8]) -> [u8; 16] {
        let h = blake3::hash(key);
        let mut out = [0u8; 16];
        out.copy_from_slice(&h.as_bytes()[..16]);
        out
    }

    /// Checks whether `key` was already recorded within the TTL window and,
    /// if not, records it — one probe sequence per call (the hot path this
    /// module's throughput target is about).
    pub fn check_and_insert(&self, key: &[u8], now_ms: i64) -> DedupOutcome {
        let hash = Self::hash_key(key);
        let (shard, start) = self.shard_and_start(&hash);
        let probes = self.probe_limit.min(self.shard_size);

        // SAFETY: every slot touched inside this block has index
        // `shard * self.shard_size + (offset within shard)`, i.e. it belongs
        // to `shard` by construction, and `self.shards[shard]`'s lock is
        // held for the block's entire duration — no other thread can reach
        // any of these slots concurrently (see struct/module SAFETY notes).
        let _guard = self.shards[shard].lock();
        let mut oldest_idx = shard * self.shard_size + start;
        let mut oldest_ts = i64::MAX;
        for step in 0..probes {
            let local = (start + step) % self.shard_size;
            let idx = shard * self.shard_size + local;
            let (slot_hash, ts, occupied) = unsafe { self.read_slot_unlocked(idx) };
            if !occupied {
                unsafe { self.write_slot_unlocked(idx, hash, now_ms) };
                let miss_count = self.misses.fetch_add(1, Ordering::Relaxed) + 1;
                self.maybe_warn_on_eviction_ratio(miss_count);
                return DedupOutcome::Fresh;
            }
            if slot_hash == hash && now_ms.saturating_sub(ts) < self.ttl_ms {
                self.hits.fetch_add(1, Ordering::Relaxed);
                return DedupOutcome::Duplicate;
            }
            // Occupied but either a different key or past its TTL: an
            // eviction candidate. Track the least-recently-written slot
            // seen so far in this probe run.
            if ts < oldest_ts {
                oldest_ts = ts;
                oldest_idx = idx;
            }
        }
        // No empty slot and no live duplicate within the probe budget:
        // replace the oldest slot seen. Oldest-write eviction needs no
        // extra bookkeeping beyond the timestamp already stored per slot,
        // and it naturally favors currently-active keys surviving pressure
        // over stale ones.
        unsafe { self.write_slot_unlocked(oldest_idx, hash, now_ms) };
        let miss_count = self.misses.fetch_add(1, Ordering::Relaxed) + 1;
        self.evictions.fetch_add(1, Ordering::Relaxed);
        self.maybe_warn_on_eviction_ratio(miss_count);
        DedupOutcome::Fresh
    }

    /// Logs a `warn!` when the running eviction ratio (evictions / misses)
    /// exceeds `EVICTION_WARN_RATIO`, checked only every
    /// `EVICTION_WARN_CHECK_INTERVAL` misses so the hot path pays for a
    /// modulo, not a division-and-log on every call.
    fn maybe_warn_on_eviction_ratio(&self, miss_count: u64) {
        if !miss_count.is_multiple_of(EVICTION_WARN_CHECK_INTERVAL) {
            return;
        }
        let evictions = self.evictions.load(Ordering::Relaxed);
        let ratio = evictions as f64 / miss_count as f64;
        if ratio > EVICTION_WARN_RATIO {
            tracing::warn!(
                evictions,
                misses = miss_count,
                ratio,
                capacity = self.capacity,
                "dedup store eviction ratio is high: the configured dedup window is not \
                 actually being honored under current traffic — entries are being evicted \
                 well before ttl_ms elapses (safe direction: duplicates may slip through \
                 earlier than configured, not the other way around)"
            );
        }
    }

    /// Read-only probe: `true` when `key` is recorded within the TTL window.
    /// Never mutates the table, so a caller can decide before a durable
    /// write and only `insert` after that write succeeded — a rejected or
    /// throttled append must never leave the key behind as a false positive.
    pub fn contains(&self, key: &[u8], now_ms: i64) -> bool {
        let hash = Self::hash_key(key);
        let (shard, start) = self.shard_and_start(&hash);
        let probes = self.probe_limit.min(self.shard_size);
        let _guard = self.shards[shard].lock();
        for step in 0..probes {
            let local = (start + step) % self.shard_size;
            let idx = shard * self.shard_size + local;
            // SAFETY: slot belongs to `shard` by construction and the
            // shard lock is held (see `check_and_insert`).
            let (slot_hash, ts, occupied) = unsafe { self.read_slot_unlocked(idx) };
            if !occupied {
                return false;
            }
            if slot_hash == hash && now_ms.saturating_sub(ts) < self.ttl_ms {
                return true;
            }
        }
        false
    }

    /// Records `key` (no-op when it is already live within the TTL window).
    pub fn insert(&self, key: &[u8], now_ms: i64) {
        let _ = self.check_and_insert(key, now_ms);
    }

    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    pub fn misses(&self) -> u64 {
        self.misses.load(Ordering::Relaxed)
    }

    pub fn evictions(&self) -> u64 {
        self.evictions.load(Ordering::Relaxed)
    }

    /// The dedup window this store actually honors, in milliseconds. Equal
    /// to `DedupConfig::ttl_ms` unless `capacity` was derived (not set
    /// explicitly) and hit `MAX_DERIVED_CAPACITY`, in which case this is the
    /// shorter, real window (`capacity / expected_rate_per_sec` seconds) —
    /// see `open`'s doc comment and the `warn!` it logs when that happens.
    pub fn effective_capacity_window_ms(&self) -> i64 {
        self.effective_capacity_window_ms
    }

    /// Forces the mapping to disk. Not on the hot path (a crash simply loses
    /// the most recent, still-in-page-cache dedup entries — those records
    /// would be re-admitted as "fresh" on retry, which is a safe direction
    /// to fail for a de-duplication cache backed by at-least-once delivery).
    pub fn flush(&self) -> io::Result<()> {
        self._mmap.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Instant;

    /// Opens a fresh store in its own temp directory, which is removed
    /// (backing file included) when the returned `TempDir` is dropped.
    /// Callers must keep the `TempDir` alive for as long as they use the
    /// store.
    fn small_store(
        capacity: usize,
        shards: usize,
        ttl_ms: i64,
    ) -> (tempfile::TempDir, MmapDedupStore) {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("dedup.bin");
        let store = MmapDedupStore::open(
            &path,
            DedupConfig {
                capacity: Some(capacity),
                ttl_ms,
                shards,
                probe_limit: 8,
                expected_rate_per_sec: 10_000,
            },
        )
        .expect("open dedup store");
        (dir, store)
    }

    #[test]
    fn fresh_then_duplicate_within_ttl() {
        let (_dir, store) = small_store(1024, 4, 60_000);
        assert_eq!(
            store.check_and_insert(b"order-1", 1_000),
            DedupOutcome::Fresh
        );
        assert_eq!(
            store.check_and_insert(b"order-1", 1_500),
            DedupOutcome::Duplicate
        );
        // A different key is unaffected.
        assert_eq!(
            store.check_and_insert(b"order-2", 1_600),
            DedupOutcome::Fresh
        );
        assert_eq!(store.hits(), 1);
        assert_eq!(store.misses(), 2);
    }

    #[test]
    fn duplicate_outside_ttl_window_is_fresh_again() {
        let (_dir, store) = small_store(1024, 4, 1_000);
        assert_eq!(store.check_and_insert(b"key", 0), DedupOutcome::Fresh);
        // Still inside the window.
        assert_eq!(store.check_and_insert(b"key", 999), DedupOutcome::Duplicate);
        // Past the window: treated as never seen.
        assert_eq!(store.check_and_insert(b"key", 1_001), DedupOutcome::Fresh);
        // And now it is a duplicate of the refreshed entry.
        assert_eq!(
            store.check_and_insert(b"key", 1_500),
            DedupOutcome::Duplicate
        );
    }

    /// One shard, capacity smaller than the number of distinct keys forced
    /// through it: the oldest entry must be the one evicted, and the
    /// evicted key is reported as `Fresh` again on its next appearance.
    #[test]
    fn eviction_replaces_oldest_slot_when_shard_is_full() {
        let (_dir, store) = small_store(4, 1, 1_000_000_000);
        assert_eq!(store.check_and_insert(b"a", 100), DedupOutcome::Fresh);
        assert_eq!(store.check_and_insert(b"b", 200), DedupOutcome::Fresh);
        assert_eq!(store.check_and_insert(b"c", 300), DedupOutcome::Fresh);
        assert_eq!(store.check_and_insert(b"d", 400), DedupOutcome::Fresh);
        // Table (4 slots, 1 shard) is now full. A 5th distinct key must
        // evict the least-recently-written slot ("a", ts=100).
        assert_eq!(store.check_and_insert(b"e", 500), DedupOutcome::Fresh);
        assert_eq!(store.evictions(), 1);
        // "b"/"c"/"d"/"e" all survived that single eviction. Checked BEFORE
        // re-touching "a" below — the table stays completely full after
        // every insert here (capacity 4, 5 distinct keys seen), so a
        // further insert of "a" would itself evict one of these, and
        // asserting both directions at once would just be testing which
        // key eviction picks a second time, not that eviction is correct.
        assert_eq!(store.check_and_insert(b"b", 700), DedupOutcome::Duplicate);
        assert_eq!(store.check_and_insert(b"c", 700), DedupOutcome::Duplicate);
        assert_eq!(store.check_and_insert(b"d", 700), DedupOutcome::Duplicate);
        assert_eq!(store.check_and_insert(b"e", 700), DedupOutcome::Duplicate);
        // "a" was evicted: it is fresh again (checked last, since this call
        // itself causes one more churn-eviction of whatever is now oldest).
        assert_eq!(store.check_and_insert(b"a", 800), DedupOutcome::Fresh);
        assert_eq!(store.evictions(), 2);
    }

    /// Regression test for the soundness bug where `shard_size` was
    /// computed with `div_ceil` (without rounding `capacity` up to match),
    /// so the last shard's range could extend past `capacity`, and
    /// `shard_len`'s `.max(1)` masked the resulting empty range instead of
    /// surfacing it — together producing `idx >= capacity` and an
    /// out-of-bounds raw pointer write. Exercises three capacity/shard
    /// pairs that do not divide evenly, touching the LAST slot of every
    /// shard directly (the exact spot that used to escape `capacity`).
    #[test]
    fn capacity_not_divisible_by_shards_stays_in_bounds_for_every_shard() {
        for &(requested_capacity, shards) in &[(1500usize, 1024usize), (1000, 300), (500_000, 1024)]
        {
            let dir = tempfile::tempdir().expect("create temp dir");
            let path = dir.path().join("dedup.bin");
            let store = MmapDedupStore::open(
                &path,
                DedupConfig {
                    capacity: Some(requested_capacity),
                    shards,
                    ttl_ms: 60_000,
                    probe_limit: 4,
                    expected_rate_per_sec: 10_000,
                },
            )
            .unwrap_or_else(|e| panic!("open failed for {requested_capacity}/{shards}: {e}"));

            assert_eq!(
                store.capacity % store.shards.len(),
                0,
                "capacity must be an exact multiple of shard count for \
                 {requested_capacity}/{shards}"
            );
            assert!(
                store.capacity >= requested_capacity,
                "rounding must only ever grow capacity"
            );

            // Touch the last slot of every shard directly — the index that
            // used to be computed past `capacity` before this fix — and
            // confirm it round-trips.
            for shard in 0..store.shards.len() {
                let last_local = store.shard_size - 1;
                let idx = shard * store.shard_size + last_local;
                assert!(
                    idx < store.capacity,
                    "shard {shard} last slot {idx} escapes capacity {}",
                    store.capacity
                );
                let _guard = store.shards[shard].lock();
                unsafe {
                    store.write_slot_unlocked(idx, [0xAB; 16], 1);
                    let (h, ts, occupied) = store.read_slot_unlocked(idx);
                    assert_eq!(h, [0xAB; 16]);
                    assert_eq!(ts, 1);
                    assert!(occupied);
                }
            }
        }
    }

    #[test]
    fn reopen_with_same_capacity_preserves_entries() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("dedup.bin");
        let cfg = DedupConfig {
            capacity: Some(4096),
            shards: 4,
            ttl_ms: 3_600_000,
            probe_limit: 8,
            expected_rate_per_sec: 10_000,
        };
        {
            let store = MmapDedupStore::open(&path, cfg).unwrap();
            assert_eq!(
                store.check_and_insert(b"persist-me", 10),
                DedupOutcome::Fresh
            );
            store.flush().unwrap();
        }
        let reopened = MmapDedupStore::open(&path, cfg).unwrap();
        assert_eq!(
            reopened.check_and_insert(b"persist-me", 20),
            DedupOutcome::Duplicate
        );
    }

    #[test]
    fn truncated_file_reopen_recreates_empty_cache_instead_of_erroring() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("dedup.bin");
        let cfg = DedupConfig {
            capacity: Some(1024),
            shards: 4,
            ttl_ms: 3_600_000,
            probe_limit: 8,
            expected_rate_per_sec: 10_000,
        };
        {
            let store = MmapDedupStore::open(&path, cfg).unwrap();
            assert_eq!(store.check_and_insert(b"k", 10), DedupOutcome::Fresh);
            store.flush().unwrap();
        }
        // Simulate a truncated/corrupted cache file (e.g. an interrupted
        // copy or a filesystem that lost the tail).
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(HEADER_SIZE as u64 + 4).unwrap();
        drop(f);

        let reopened = MmapDedupStore::open(&path, cfg)
            .expect("a truncated file must be recreated, not rejected");
        assert_eq!(
            reopened.check_and_insert(b"k", 20),
            DedupOutcome::Fresh,
            "recreated cache must not remember pre-truncation entries"
        );
    }

    #[test]
    fn capacity_mismatch_reopen_recreates_fresh_table_instead_of_erroring() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("dedup.bin");
        {
            let cfg = DedupConfig {
                capacity: Some(256),
                shards: 4,
                ttl_ms: 3_600_000,
                probe_limit: 8,
                expected_rate_per_sec: 10_000,
            };
            let store = MmapDedupStore::open(&path, cfg).unwrap();
            assert_eq!(store.check_and_insert(b"k", 10), DedupOutcome::Fresh);
            store.flush().unwrap();
        }
        let cfg2 = DedupConfig {
            capacity: Some(512),
            shards: 4,
            ttl_ms: 3_600_000,
            probe_limit: 8,
            expected_rate_per_sec: 10_000,
        };
        let reopened = MmapDedupStore::open(&path, cfg2)
            .expect("a capacity mismatch must recreate the file, not error");
        assert_eq!(reopened.capacity, 512);
        assert_eq!(
            reopened.check_and_insert(b"k", 20),
            DedupOutcome::Fresh,
            "recreated cache must not remember the old-capacity table's entries"
        );
    }

    #[test]
    fn second_open_on_same_file_is_rejected_by_the_lock() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("dedup.bin");
        let cfg = DedupConfig {
            capacity: Some(256),
            shards: 4,
            ttl_ms: 60_000,
            probe_limit: 8,
            expected_rate_per_sec: 10_000,
        };
        let _first = MmapDedupStore::open(&path, cfg).unwrap();
        let err = MmapDedupStore::open(&path, cfg)
            .expect_err("a second concurrent open on the same file must fail");
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
    }

    /// An explicit, maximal `capacity` must not panic
    /// (debug: `checked_mul` inside `round_up_to_multiple`/`total_len`
    /// overflowing) or silently wrap to a too-small value that would then
    /// divide by zero in `shard_and_start` (release) — `open` must instead
    /// report a clean `Err(InvalidInput)`.
    #[test]
    fn explicit_usize_max_capacity_is_a_clean_error_not_a_panic_or_divide_by_zero() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("dedup.bin");
        let cfg = DedupConfig {
            capacity: Some(usize::MAX),
            shards: 1024,
            ttl_ms: 60_000,
            probe_limit: 8,
            expected_rate_per_sec: 10_000,
        };
        let err = MmapDedupStore::open(&path, cfg)
            .expect_err("usize::MAX capacity must be rejected, not panic or wrap");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    /// A dedup window that, at the configured expected
    /// rate, would need more slots than `MAX_DERIVED_CAPACITY` allows must
    /// report a shorter `effective_capacity_window_ms` than the `ttl_ms` it was
    /// asked for — the whole point being that this shortfall is visible to
    /// an operator instead of silently assumed to be honored.
    #[test]
    fn clamped_capacity_reports_a_shorter_effective_window_than_requested() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("dedup.bin");
        let cfg = DedupConfig {
            capacity: None,
            shards: 1024,
            ttl_ms: 24 * 3_600_000, // 24h, the PLAN §7.1 default.
            probe_limit: 8,
            expected_rate_per_sec: 10_000, // default node rate.
        };
        let store = MmapDedupStore::open(&path, cfg).unwrap();
        assert_eq!(store.capacity, MAX_DERIVED_CAPACITY as usize);
        assert!(
            store.effective_capacity_window_ms() < cfg.ttl_ms,
            "24h at 10k/s must not fit in the {MAX_DERIVED_CAPACITY}-slot ceiling"
        );
        // At 10,000/s, 16 Mi slots cover ~1677 seconds (~28 minutes) —
        // nowhere near the requested 24h.
        assert!(store.effective_capacity_window_ms() < 30 * 60_000);
    }

    /// A `capacity` derived from a window/rate that comfortably fits under
    /// the ceiling must report the FULL requested window, not a clamped
    /// one — the shortfall above is specific to hitting the ceiling, not a
    /// general property of derived capacities.
    #[test]
    fn unclamped_derived_capacity_reports_the_full_requested_window() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("dedup.bin");
        let cfg = DedupConfig {
            capacity: None,
            shards: 4,
            ttl_ms: 60_000, // 1 minute.
            probe_limit: 8,
            expected_rate_per_sec: 100, // 100/s * 60s = 6,000 slots, well under the floor/ceiling.
        };
        let store = MmapDedupStore::open(&path, cfg).unwrap();
        assert_eq!(store.effective_capacity_window_ms(), cfg.ttl_ms);
    }

    #[test]
    fn contains_is_read_only_and_does_not_affect_misses() {
        let (_dir, store) = small_store(1024, 4, 60_000);
        assert!(!store.contains(b"order-1", 1_000));
        assert_eq!(
            store.misses(),
            0,
            "a read-only probe must not increment misses"
        );
        store.insert(b"order-1", 1_000);
        assert!(store.contains(b"order-1", 1_100));
        // Probing a still-unseen key must also leave the counter untouched.
        assert!(!store.contains(b"order-2", 1_100));
        assert_eq!(
            store.misses(),
            1,
            "only the explicit insert() call above should count as a miss"
        );
    }

    #[test]
    fn contains_respects_ttl_expiry() {
        let (_dir, store) = small_store(1024, 4, 1_000);
        store.insert(b"k", 0);
        assert!(store.contains(b"k", 999));
        assert!(
            !store.contains(b"k", 1_001),
            "entry past ttl_ms must read as absent"
        );
    }

    #[test]
    fn concurrent_unique_keys_are_each_fresh_exactly_once() {
        // Capacity/shards sized for a low load factor (40k keys into 1M
        // slots / 1024 shards => ~39 keys per 1024-slot shard, ~4%): with
        // `probe_limit = 8` and a well-mixed hash, a churn eviction inside
        // any single shard is not expected at this load — unlike a small,
        // near-full table, where a bounded probe run can legitimately fail
        // to find an empty slot even though the shard is not literally full
        // (see `eviction_replaces_oldest_slot_when_shard_is_full`, which
        // deliberately runs at 100%+ load to exercise that path).
        let (_dir, store) = small_store(1 << 20, 1024, 3_600_000);
        let store = Arc::new(store);
        let threads = 8usize;
        let per_thread = 5_000usize;
        let mut handles = Vec::new();
        for t in 0..threads {
            let store = Arc::clone(&store);
            handles.push(std::thread::spawn(move || {
                let mut fresh_count = 0u32;
                for i in 0..per_thread {
                    let key = format!("thread-{t}-key-{i}");
                    if store.check_and_insert(key.as_bytes(), 1_000) == DedupOutcome::Fresh {
                        fresh_count += 1;
                    }
                }
                fresh_count
            }));
        }
        let total_fresh: u32 = handles.into_iter().map(|h| h.join().unwrap()).sum();
        // Every key across every thread is distinct and the table is large
        // enough that no eviction should occur, so all of them are Fresh.
        assert_eq!(total_fresh, (threads * per_thread) as u32);
        assert_eq!(store.evictions(), 0);

        // Re-checking any of them now reports Duplicate.
        assert_eq!(
            store.check_and_insert(b"thread-0-key-0", 1_500),
            DedupOutcome::Duplicate
        );
    }

    /// Smoke sanity check for `check_and_insert` throughput: single-thread
    /// and multi-thread ops/s are printed (run with `--nocapture` to see
    /// them). This is not the authoritative benchmark for the PLAN §5.4
    /// >=300k ops/s target — it runs on shared, possibly loaded hardware,
    /// with warmup included in the measurement — so the assertions below
    /// are set well under that target, purely as a regression floor: the
    /// real number belongs in a dedicated report, not a CI assertion.
    #[test]
    fn check_and_insert_throughput_smoke_check() {
        let (_dir, store) = small_store(1 << 21, 2048, 3_600_000);
        let store = Arc::new(store);

        // Single-thread baseline.
        let n_single = 200_000usize;
        let start = Instant::now();
        for i in 0..n_single {
            let key = (i as u64).to_le_bytes();
            store.check_and_insert(&key, i as i64);
        }
        let elapsed = start.elapsed();
        let single_ops_per_sec = n_single as f64 / elapsed.as_secs_f64();
        println!(
            "dedup single-thread: {n_single} ops in {:?} = {:.0} ops/s",
            elapsed, single_ops_per_sec
        );

        // Multi-thread (8 threads), disjoint key spaces.
        let threads = 8usize;
        let per_thread = 200_000usize;
        let start = Instant::now();
        let mut handles = Vec::new();
        for t in 0..threads {
            let store = Arc::clone(&store);
            handles.push(std::thread::spawn(move || {
                for i in 0..per_thread {
                    let key = ((t as u64) << 32) | i as u64;
                    store.check_and_insert(&key.to_le_bytes(), i as i64);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let elapsed = start.elapsed();
        let total_ops = threads * per_thread;
        let multi_ops_per_sec = total_ops as f64 / elapsed.as_secs_f64();
        println!(
            "dedup {threads}-thread: {total_ops} ops in {:?} = {:.0} ops/s",
            elapsed, multi_ops_per_sec
        );

        assert!(
            single_ops_per_sec > 50_000.0,
            "single-thread dedup throughput regressed hard: {single_ops_per_sec:.0} ops/s"
        );
        assert!(
            multi_ops_per_sec > 100_000.0,
            "multi-thread dedup throughput regressed hard: {multi_ops_per_sec:.0} ops/s"
        );
    }
}
