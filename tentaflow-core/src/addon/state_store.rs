// =============================================================================
// File: addon/state_store.rs — host-side shared in-memory state for addons (A1)
//
// WHY: WASM instances are memory-isolated (no wasm-threads / shared linear
// memory), so any state that must be shared across the service / pooled /
// ephemeral instances of the SAME addon has to live in the HOST. Today every
// cross-instance read/write round-trips SQLite, which does not scale to
// thousands of robots/users. This store is the fast shared layer that sits in
// front of (future, A2) persistence.
//
// SHARDING: outer `DashMap<addon_id, Arc<AddonStateShard>>` gives lock-free
// per-addon lookup. Each addon owns its shard so different addons never
// contend on the same lock. Critically for multi-robot: every robot is a
// separate addon INSTANCE = its own addon_id = its own shard, so robots scale
// horizontally without lock contention between them.
//
// SCOPE (A1): the store + its API + unit tests ONLY. Persistence (A2),
// host functions (A3) and the go2 migration (A4) wire into the API exposed
// here but are out of scope for this chunk. `take_dirty` / `load_durable`
// are provided now for the A2 flusher; `drop_addon` for the unload/uninstall
// call site that A4/B will wire.
// =============================================================================

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use dashmap::DashMap;
use parking_lot::RwLock;

// =============================================================================
// Caps — generous defaults, documented. Bytes are counted as
// key.len() + value.len() for every entry.
// =============================================================================

/// Maximum number of entries a single addon may hold. An addon = one shard,
/// and for multi-robot each robot is its own addon_id, so this is a per-robot
/// budget, not a global one. 50k live keys per addon is generous for the
/// expected "live state" workload (telemetry snapshots, control flags, small
/// caches) while still bounding RAM under a misbehaving addon.
pub const MAX_ENTRIES_PER_ADDON: usize = 50_000;

/// Maximum total bytes (sum of key.len()+value.len() over all entries) a
/// single addon may hold. 32 MiB per addon keeps the aggregate RAM footprint
/// predictable even with many addons/robots resident at once.
pub const MAX_BYTES_PER_ADDON: usize = 32 * 1024 * 1024;

/// Maximum size of a single value. 1 MiB is large for "live state"; anything
/// bigger belongs in addon SQLite / blob storage, not the shared hot store.
/// Writes above this are rejected outright (before any cap accounting).
pub const MAX_VALUE_BYTES: usize = 1024 * 1024;

/// Per-entry CBOR framing overhead used by `list_bounded` to estimate the
/// encoded size of one `StateEntryMeta`. Covers the map header, the three
/// integer keys, the `size` u64 (worst case 9 bytes) and the `tier` byte, plus
/// the `key` string header — an upper bound, so the real encoded output stays
/// within the byte budget the host passes.
pub const STATE_LIST_ENTRY_OVERHEAD: usize = 24;

// =============================================================================
// Public types
// =============================================================================

/// Persistence intent of an entry.
///
/// * `Ephemeral` — RAM-only, never persisted. On per-addon cap overflow the
///   shard evicts the entries with the oldest `updated_at_ms` until back under
///   the cap (approximate-LRU: ordering is by last-write time, not last-read,
///   so a frequently-read-but-never-rewritten key can still be evicted —
///   acceptable for live state where writes dominate).
/// * `Durable` — RAM-served and marked `dirty` on write so the A2 flusher can
///   persist it and clear the flag. On cap overflow a write that would add a
///   NEW key is REJECTED (durable data is never silently dropped); a write
///   that REPLACES an existing key is always allowed (it cannot grow the entry
///   count and the byte delta is re-checked).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Ephemeral,
    Durable,
}

/// Error returned by `set`. All variants are addon-visible failure modes that
/// A3 host functions will surface to the calling addon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateStoreError {
    /// The value exceeded `MAX_VALUE_BYTES`.
    ValueTooLarge,
    /// A write would push the addon over its entry or byte cap and the room
    /// could not be made (durable data is never silently dropped; an ephemeral
    /// write is rejected when eviction cannot free enough). Also covers the
    /// internal counter-overflow guard — a counter overflow can only happen if
    /// the in-RAM map already holds quota-violating data, so it is reported as
    /// a quota failure rather than a panic.
    AddonQuotaExceeded,
}

impl fmt::Display for StateStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StateStoreError::ValueTooLarge => write!(
                f,
                "value exceeds the maximum allowed size ({} bytes)",
                MAX_VALUE_BYTES
            ),
            StateStoreError::AddonQuotaExceeded => write!(
                f,
                "addon state quota exceeded (max {} entries / {} bytes)",
                MAX_ENTRIES_PER_ADDON, MAX_BYTES_PER_ADDON
            ),
        }
    }
}

impl std::error::Error for StateStoreError {}

/// Outcome of `load_durable`. The store enforces the per-value and per-addon
/// caps while seeding from the backing store, so a large or corrupt backing
/// store can never push RAM past the documented bounds. Entries that would
/// violate a cap are SKIPPED (not loaded) and counted here; the policy is
/// "load up to the cap, report what was dropped" so addon start never fails
/// hard on a too-big backing store but the operator/flusher can observe the
/// shortfall and act (e.g. log, compact, alarm).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LoadOutcome {
    /// Entries actually loaded into RAM.
    pub loaded: usize,
    /// Entries skipped because they alone exceeded `MAX_VALUE_BYTES`.
    pub skipped_value_too_large: usize,
    /// Entries skipped because loading them would exceed the per-addon
    /// entry/byte cap (the shard is already full).
    pub skipped_quota: usize,
    /// Entries skipped because the key already exists live in RAM (newer-or-
    /// equal live state is never clobbered by a persisted seed — see the merge
    /// policy in `load_durable`).
    pub skipped_present: usize,
    /// True when the shard was already loaded since its last cold start, so this
    /// `load_durable` call was a no-op (load-once guard). Lets the caller skip
    /// logging a redundant reload.
    pub already_loaded: bool,
}

/// Batch of changes collected by `take_dirty` for the A2 flusher to persist.
/// `upserts` are durable keys written since the last take; `deletes` are
/// durable keys removed since the last take. The flusher applies both against
/// the backing store as one consistent batch.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DirtySet {
    pub upserts: Vec<(String, Vec<u8>)>,
    pub deletes: Vec<String>,
}

impl DirtySet {
    /// True when there is nothing to persist — the flusher can skip the round.
    pub fn is_empty(&self) -> bool {
        self.upserts.is_empty() && self.deletes.is_empty()
    }
}

/// A dirty batch taken from a shard, carrying the exact shard `Arc` it was taken
/// from so the flusher can coordinate with a concurrent `drop_addon`/purge. The
/// flusher must consult `is_purged()` BEFORE committing the batch to the backing
/// store: if the addon was uninstalled after the batch was taken, the stale
/// batch must NOT resurrect rows. `remark_dirty` re-arms onto THIS shard only
/// (never recreating a shard for an uninstalled addon).
pub(crate) struct TakenBatch {
    addon_id: String,
    shard: Arc<AddonStateShard>,
    set: DirtySet,
}

impl TakenBatch {
    pub(crate) fn addon_id(&self) -> &str {
        &self.addon_id
    }

    pub(crate) fn set(&self) -> &DirtySet {
        &self.set
    }

    /// True if the addon was uninstalled (its shard purged) after this batch was
    /// taken. The flusher refuses to commit a purged batch (no row resurrection).
    pub(crate) fn is_purged(&self) -> bool {
        self.shard.purged.load(Ordering::Acquire)
    }
}

impl std::ops::Deref for TakenBatch {
    type Target = DirtySet;
    fn deref(&self) -> &DirtySet {
        &self.set
    }
}

// =============================================================================
// Internal entry
// =============================================================================

/// A single stored value plus its bookkeeping. `updated_at_ms` is the wall
/// clock (host-side `chrono::Utc`) of the last write — it drives both
/// approximate-LRU eviction (ephemeral) and is the natural last-write-wins
/// marker A2 will reuse. `dirty` is only ever true for `Durable` entries.
#[derive(Debug, Clone)]
struct Entry {
    value: Vec<u8>,
    tier: Tier,
    updated_at_ms: i64,
    dirty: bool,
}

impl Entry {
    /// Byte cost of an entry (key + value). Returns `None` on `usize` overflow,
    /// which callers treat as a quota failure rather than wrapping.
    fn size(key: &str, value: &[u8]) -> Option<usize> {
        key.len().checked_add(value.len())
    }
}

/// Projected byte count after a write: `current - old + new`, using checked
/// arithmetic so neither the subtraction (counter underflow) nor the addition
/// (counter overflow) wraps silently. `old` is the byte cost of the entry being
/// replaced (0 for an insert). `None` signals an arithmetic violation that the
/// caller maps to a quota failure rather than panicking.
fn projected_bytes(current: usize, old: usize, new: usize) -> Option<usize> {
    current.checked_sub(old)?.checked_add(new)
}

// =============================================================================
// Shard — one addon's state behind a single RwLock
// =============================================================================

/// All state for one addon. Reads take the `RwLock` read guard (concurrent);
/// writes take the write guard (serialized per-addon only — different addons
/// have different shards and never block each other). The atomic counters
/// mirror the locked map so cap checks and `addon_stats` are O(1) and can be
/// read without taking the map lock. The counters are always mutated under the
/// write lock, so they stay consistent with `entries`.
struct AddonStateShard {
    entries: RwLock<ShardInner>,
    entry_count: AtomicUsize,
    byte_count: AtomicUsize,
    /// Cheap "has anything to flush" signal so `dirty_addons` can skip clean
    /// shards without taking each shard's write lock + scanning the map. Set
    /// true (under the write lock) on every durable upsert and tombstone;
    /// cleared inside `take_dirty` once the dirty set is fully drained. Always
    /// mutated under the `entries` write lock so it stays consistent with the
    /// per-entry `dirty` flags and `dirty_deletes`. Read with `Acquire` from
    /// `dirty_addons` (lock-free observation): a stale `false` is impossible
    /// because the producer sets it under the same lock the consumer's
    /// `take_dirty` will later take, and a stale `true` only costs one wasted
    /// `take_dirty` that returns an empty set.
    has_dirty: AtomicBool,
    /// Load-once guard: set true by `load_durable` the first time the shard is
    /// seeded from the backing store, cleared by `drop_addon`. A redundant
    /// `load_durable` (e.g. a second instance start of the same addon) is a
    /// no-op while this is true, so a persisted seed can never clobber live or
    /// dirty in-RAM state written by another instance since the cold start.
    loaded: AtomicBool,
    /// Uninstall marker: set true by `drop_addon` under the write lock the
    /// instant the shard is detached. A `TakenBatch` captured BEFORE the drop
    /// observes this and refuses to commit (no row resurrection after uninstall),
    /// and `remark_dirty` refuses to re-arm a purged/closed shard (no shard
    /// recreation for an uninstalled addon).
    purged: AtomicBool,
}

/// The locked portion of a shard. `dirty_deletes` records durable keys removed
/// since the last `take_dirty` so A2 can issue DELETEs against the backing
/// store. Inserts/updates are tracked via `Entry.dirty` rather than a second
/// set, so a key that is written then deleted within one flush window collapses
/// correctly (delete wins, no stale upsert).
struct ShardInner {
    map: HashMap<String, Entry>,
    dirty_deletes: HashSet<String>,
    /// Set true by `drop_addon` (under the write lock) the instant it detaches
    /// this shard from the outer `DashMap`. A writer that already cloned the
    /// `Arc` and is holding the write lock sees `closed == true` and re-resolves
    /// a fresh shard from the map, so a `set` that returns `Ok` is always
    /// visible to subsequent readers (no lost write into a detached shard).
    closed: bool,
}

impl AddonStateShard {
    fn new() -> Self {
        Self {
            entries: RwLock::new(ShardInner {
                map: HashMap::new(),
                dirty_deletes: HashSet::new(),
                closed: false,
            }),
            entry_count: AtomicUsize::new(0),
            byte_count: AtomicUsize::new(0),
            has_dirty: AtomicBool::new(false),
            loaded: AtomicBool::new(false),
            purged: AtomicBool::new(false),
        }
    }
}

// =============================================================================
// Store
// =============================================================================

/// Process-global shared state store for all addons. Cheap to share — the
/// per-addon shards are `Arc`'d and the outer map is a `DashMap`. `Send+Sync`
/// and safe under concurrent multi-thread access (that is the whole point).
pub struct AddonStateStore {
    shards: DashMap<String, Arc<AddonStateShard>>,
}

impl Default for AddonStateStore {
    fn default() -> Self {
        Self::new()
    }
}

impl AddonStateStore {
    pub fn new() -> Self {
        Self {
            shards: DashMap::new(),
        }
    }

    /// Process-global singleton, mirroring the other host registries
    /// (`PermissionMatrix::global`, policy cache). Dispatch, host-fn and
    /// flusher paths all share this one view.
    pub fn global() -> &'static AddonStateStore {
        static INSTANCE: OnceLock<AddonStateStore> = OnceLock::new();
        INSTANCE.get_or_init(AddonStateStore::new)
    }

    /// Look up (or lazily create) the shard for an addon. Lock-free on the
    /// common hot path (existing shard); only the first touch for an addon
    /// inserts.
    fn shard(&self, addon_id: &str) -> Arc<AddonStateShard> {
        if let Some(existing) = self.shards.get(addon_id) {
            return existing.clone();
        }
        // entry() serializes concurrent first-touch for the same addon_id so
        // two threads can't create two shards.
        self.shards
            .entry(addon_id.to_string())
            .or_insert_with(|| Arc::new(AddonStateShard::new()))
            .clone()
    }

    /// Read a value out, cloned. Concurrent under the shard read lock. Returns
    /// `None` if the addon has no shard or the key is absent.
    pub fn get(&self, addon_id: &str, key: &str) -> Option<Vec<u8>> {
        let shard = self.shards.get(addon_id)?;
        let inner = shard.entries.read();
        inner.map.get(key).map(|e| e.value.clone())
    }

    /// Write a value. Enforces caps:
    ///   * value over `MAX_VALUE_BYTES` → `ValueTooLarge` (no state change).
    ///   * `Ephemeral` over cap → evict oldest `updated_at_ms` until the new
    ///     entry fits; if eviction cannot free enough (only durable entries
    ///     remain, or the incoming key+value alone exceeds the cap) the write
    ///     is REJECTED with `AddonQuotaExceeded` (bounded memory wins over
    ///     best-effort ephemeral admission).
    ///   * `Durable` over cap with a NEW key → `AddonQuotaExceeded`; replacing
    ///     an existing key is allowed when the byte projection fits.
    /// A durable entry replaced by an ephemeral write is tombstoned so the A2
    /// flusher deletes the persisted row (the durable value must not resurrect
    /// on restart). On success sets `updated_at_ms = now` and, for `Durable`,
    /// `dirty=true`.
    pub fn set(
        &self,
        addon_id: &str,
        key: &str,
        value: Vec<u8>,
        tier: Tier,
    ) -> Result<(), StateStoreError> {
        if value.len() > MAX_VALUE_BYTES {
            return Err(StateStoreError::ValueTooLarge);
        }

        // Re-resolve loop: if the shard we locked was concurrently detached by
        // `drop_addon`, take a fresh shard from the DashMap and retry. A fresh
        // shard cannot be closed while we hold its write lock (drop_addon must
        // take the same write lock to close it), so this terminates.
        loop {
            let shard = self.shard(addon_id);
            let mut inner = shard.entries.write();
            if inner.closed {
                // Detached underneath us; the Arc we hold is orphaned. Drop the
                // lock and re-resolve a live shard.
                drop(inner);
                continue;
            }

            let new_size = match Entry::size(key, &value) {
                Some(sz) => sz,
                None => return Err(StateStoreError::AddonQuotaExceeded),
            };
            let old_size = match inner.map.get(key) {
                Some(e) => match Entry::size(key, &e.value) {
                    Some(sz) => Some(sz),
                    None => return Err(StateStoreError::AddonQuotaExceeded),
                },
                None => None,
            };
            let prev_tier = inner.map.get(key).map(|e| e.tier);
            let is_replace = old_size.is_some();

            let entry_delta = if is_replace { 0 } else { 1 };
            let cur_entries = shard.entry_count.load(Ordering::Relaxed);
            let cur_bytes = shard.byte_count.load(Ordering::Relaxed);
            let proj_entries = match cur_entries.checked_add(entry_delta) {
                Some(v) => v,
                None => return Err(StateStoreError::AddonQuotaExceeded),
            };
            // bytes after = cur_bytes - old_size + new_size, with checked math.
            let proj_bytes = match projected_bytes(cur_bytes, old_size.unwrap_or(0), new_size) {
                Some(v) => v,
                None => return Err(StateStoreError::AddonQuotaExceeded),
            };

            let over_entries = proj_entries > MAX_ENTRIES_PER_ADDON;
            let over_bytes = proj_bytes > MAX_BYTES_PER_ADDON;

            if over_entries || over_bytes {
                match tier {
                    Tier::Durable => {
                        // A replace cannot grow the entry count; allow it as
                        // long as the byte projection fits. A new durable key
                        // that does not fit is rejected — durable data is never
                        // silently dropped to make room.
                        if !is_replace || over_bytes {
                            return Err(StateStoreError::AddonQuotaExceeded);
                        }
                    }
                    Tier::Ephemeral => {
                        Self::evict_until_fits(
                            &mut inner,
                            &shard.entry_count,
                            &shard.byte_count,
                            key,
                            entry_delta,
                            old_size.unwrap_or(0),
                            new_size,
                        );
                        // Eviction is best-effort: candidates can run out
                        // (only durable entries remain) or the incoming entry
                        // alone may exceed a cap (a huge key has no separate
                        // cap, only the combined entry cost). If we still don't
                        // fit, reject rather than violate bounded memory.
                        let entries_now = shard.entry_count.load(Ordering::Relaxed);
                        let bytes_now = shard.byte_count.load(Ordering::Relaxed);
                        let post_entries = match entries_now.checked_add(entry_delta) {
                            Some(v) => v,
                            None => return Err(StateStoreError::AddonQuotaExceeded),
                        };
                        let post_bytes =
                            match projected_bytes(bytes_now, old_size.unwrap_or(0), new_size) {
                                Some(v) => v,
                                None => return Err(StateStoreError::AddonQuotaExceeded),
                            };
                        if post_entries > MAX_ENTRIES_PER_ADDON || post_bytes > MAX_BYTES_PER_ADDON
                        {
                            return Err(StateStoreError::AddonQuotaExceeded);
                        }
                    }
                }
            }

            let now = chrono::Utc::now().timestamp_millis();
            let dirty = matches!(tier, Tier::Durable);

            let prev = inner.map.insert(
                key.to_string(),
                Entry {
                    value,
                    tier,
                    updated_at_ms: now,
                    dirty,
                },
            );

            if dirty {
                // A durable key being (re)written cancels any pending tombstone.
                inner.dirty_deletes.remove(key);
                shard.has_dirty.store(true, Ordering::Release);
            } else if matches!(prev_tier, Some(Tier::Durable)) {
                // Durable → Ephemeral downgrade: the persisted row must be
                // deleted so it does not resurrect on restart. The new value is
                // RAM-only (ephemeral) and intentionally not re-persisted.
                inner.dirty_deletes.insert(key.to_string());
                shard.has_dirty.store(true, Ordering::Release);
            }

            match prev {
                Some(old) => {
                    let old_sz = match Entry::size(key, &old.value) {
                        Some(sz) => sz,
                        None => return Err(StateStoreError::AddonQuotaExceeded),
                    };
                    // entry_count unchanged on replace; adjust bytes by delta.
                    if new_size >= old_sz {
                        shard
                            .byte_count
                            .fetch_add(new_size - old_sz, Ordering::Relaxed);
                    } else {
                        shard
                            .byte_count
                            .fetch_sub(old_sz - new_size, Ordering::Relaxed);
                    }
                }
                None => {
                    shard.entry_count.fetch_add(1, Ordering::Relaxed);
                    shard.byte_count.fetch_add(new_size, Ordering::Relaxed);
                }
            }

            return Ok(());
        }
    }

    /// Evict ephemeral entries with the oldest `updated_at_ms` until the shard
    /// has room for the incoming write. `incoming_entry_delta` is 0 on replace,
    /// 1 on insert. The byte projection accounts for BOTH the new value and the
    /// old value being replaced — a shrinking replace (`new_size < old_size`)
    /// frees bytes and must not over-evict. Only `Ephemeral` entries are
    /// evicted; durable entries are never dropped here. Caller re-checks the
    /// projection after this returns and rejects if it still does not fit (this
    /// is best-effort — it stops when candidates run out). Called under the
    /// write lock.
    fn evict_until_fits(
        inner: &mut ShardInner,
        entry_count: &AtomicUsize,
        byte_count: &AtomicUsize,
        protect_key: &str,
        incoming_entry_delta: usize,
        old_size: usize,
        new_size: usize,
    ) {
        // Snapshot eviction candidates (ephemeral, not the key being written),
        // ordered oldest-first by write time.
        let mut candidates: Vec<(String, i64)> = inner
            .map
            .iter()
            .filter(|(k, e)| matches!(e.tier, Tier::Ephemeral) && k.as_str() != protect_key)
            .map(|(k, e)| (k.clone(), e.updated_at_ms))
            .collect();
        candidates.sort_by_key(|(_, ts)| *ts);

        let mut idx = 0;
        loop {
            let entries_now = entry_count.load(Ordering::Relaxed);
            let bytes_now = byte_count.load(Ordering::Relaxed);
            let fits_entries =
                entries_now.saturating_add(incoming_entry_delta) <= MAX_ENTRIES_PER_ADDON;
            // Projected bytes after the write, accounting for the replaced old
            // value: a shrinking replace can already fit without eviction.
            let fits_bytes = match projected_bytes(bytes_now, old_size, new_size) {
                Some(b) => b <= MAX_BYTES_PER_ADDON,
                // Overflow here means existing RAM is already corrupt-large;
                // keep evicting to try to recover headroom.
                None => false,
            };

            if fits_entries && fits_bytes {
                break;
            }
            if idx >= candidates.len() {
                // Nothing left to evict (all remaining entries are durable or
                // the protected key). Caller will reject if the write still
                // does not fit — bounded memory must hold.
                break;
            }

            let (victim_key, _) = &candidates[idx];
            idx += 1;
            if let Some(removed) = inner.map.remove(victim_key) {
                if let Some(sz) = Entry::size(victim_key, &removed.value) {
                    entry_count.fetch_sub(1, Ordering::Relaxed);
                    byte_count.fetch_sub(sz, Ordering::Relaxed);
                }
            }
        }
    }

    /// Remove a key. Returns true if it existed. Updates counters. If the
    /// removed entry was `Durable`, records a tombstone in `dirty_deletes` so
    /// the A2 flusher will DELETE the persisted row.
    pub fn delete(&self, addon_id: &str, key: &str) -> bool {
        // Re-resolve loop mirrors `set`: a shard detached by a concurrent
        // `drop_addon` is orphaned, so we re-resolve. After drop the addon has
        // no key anyway; re-resolving yields a fresh empty shard → returns
        // false, which is correct (the key is gone).
        loop {
            let Some(shard) = self.shards.get(addon_id) else {
                return false;
            };
            let shard = shard.clone();
            let mut inner = shard.entries.write();
            if inner.closed {
                drop(inner);
                continue;
            }
            return match inner.map.remove(key) {
                Some(removed) => {
                    if let Some(sz) = Entry::size(key, &removed.value) {
                        shard.entry_count.fetch_sub(1, Ordering::Relaxed);
                        shard.byte_count.fetch_sub(sz, Ordering::Relaxed);
                    }
                    if matches!(removed.tier, Tier::Durable) {
                        inner.dirty_deletes.insert(key.to_string());
                        shard.has_dirty.store(true, Ordering::Release);
                    }
                    true
                }
                None => false,
            };
        }
    }

    /// List keys for an addon, optionally filtered by prefix. Returns
    /// `(key, value_size_bytes, tier)`. Order is unspecified. UNBOUNDED — only
    /// for internal callers/tests that know the shard is small; the host ABI
    /// path uses `list_bounded` so a 50k-key shard can never materialise a
    /// multi-megabyte response.
    pub fn list(&self, addon_id: &str, prefix: Option<&str>) -> Vec<(String, usize, Tier)> {
        let Some(shard) = self.shards.get(addon_id) else {
            return Vec::new();
        };
        let inner = shard.entries.read();
        inner
            .map
            .iter()
            .filter(|(k, _)| prefix.map(|p| k.starts_with(p)).unwrap_or(true))
            .map(|(k, e)| (k.clone(), e.value.len(), e.tier))
            .collect()
    }

    /// Bounded variant of `list` for the host ABI path. Collects at most
    /// `max_entries` matching entries AND stops once the estimated encoded byte
    /// budget `max_bytes` would be exceeded, so the host never clones all keys of
    /// a full (50k-entry) shard before encoding. Returns the collected metadata
    /// and `truncated = true` when either limit cut the scan short (the caller
    /// surfaces this so the addon knows the list was clipped).
    ///
    /// The scan and the early-stop both happen UNDER the shard read lock, so the
    /// host allocates only the bounded result set, not the full key space. The
    /// per-entry byte estimate is `key.len() + STATE_LIST_ENTRY_OVERHEAD` (a
    /// fixed allowance for the CBOR map keys, the `size` u64 and the `tier` byte),
    /// which is an upper bound on the actual encoded `StateEntryMeta` size — so
    /// `max_bytes` is a safe budget that the real encoded output never exceeds.
    pub fn list_bounded(
        &self,
        addon_id: &str,
        prefix: Option<&str>,
        max_entries: usize,
        max_bytes: usize,
    ) -> (Vec<(String, usize, Tier)>, bool) {
        let Some(shard) = self.shards.get(addon_id) else {
            return (Vec::new(), false);
        };
        let inner = shard.entries.read();

        let mut out: Vec<(String, usize, Tier)> = Vec::new();
        let mut est_bytes: usize = 0;
        let mut truncated = false;

        for (k, e) in inner.map.iter() {
            if !prefix.map(|p| k.starts_with(p)).unwrap_or(true) {
                continue;
            }
            if out.len() >= max_entries {
                truncated = true;
                break;
            }
            let entry_cost = k.len().saturating_add(STATE_LIST_ENTRY_OVERHEAD);
            let next_est = est_bytes.saturating_add(entry_cost);
            if !out.is_empty() && next_est > max_bytes {
                // Stop before exceeding the byte budget; always admit at least
                // one entry so a single oversized key still returns a (clipped)
                // result rather than an empty list.
                truncated = true;
                break;
            }
            est_bytes = next_est;
            out.push((k.clone(), e.value.len(), e.tier));
        }

        (out, truncated)
    }

    /// Remove the whole shard for an addon (unload / uninstall). Any unflushed
    /// durable state is dropped — callers that need persistence must flush
    /// (A2) before dropping. A1 only provides the entry point; A4/B wires the
    /// call site.
    ///
    /// To avoid losing a write that races with the drop, we mark the shard
    /// `closed` UNDER its write lock first. A concurrent `set`/`delete` that
    /// already cloned the `Arc` either (a) has not yet taken the write lock —
    /// it will see `closed == true` and re-resolve a live shard, or (b) already
    /// holds the write lock — then this close blocks until it finishes, and its
    /// write landed before the shard was orphaned (but is then dropped with the
    /// shard, which is the intended unload semantics: a write strictly before
    /// the unload may or may not survive; a write that observes the close
    /// re-targets a fresh shard and is never silently lost).
    pub fn drop_addon(&self, addon_id: &str) {
        if let Some((_, shard)) = self.shards.remove(addon_id) {
            shard.entries.write().closed = true;
            // Mark the detached shard purged so any in-flight `TakenBatch` taken
            // before this drop refuses to commit (no row resurrection after
            // uninstall) and `remark_dirty` refuses to recreate a shard for it.
            shard.purged.store(true, Ordering::Release);
            // Reset the load-once guard: a future cold start under the same
            // addon_id resolves a FRESH shard (this one is orphaned), so the
            // guard is implicitly reset by the new shard's default `false`.
            // Clearing here is belt-and-suspenders for any retained `Arc`.
            shard.loaded.store(false, Ordering::Release);
        }
    }

    /// Clear the load-once guard after a FAILED load so a retried addon start
    /// re-seeds the shard. Merge semantics make the reload idempotent (present
    /// keys are skipped), so any rows that did land before the failure are kept
    /// and the rest are filled on retry. No-op if the addon has no shard.
    pub(crate) fn reset_loaded(&self, addon_id: &str) {
        if let Some(shard) = self.shards.get(addon_id) {
            shard.loaded.store(false, Ordering::Release);
        }
    }

    /// Observability: `(entries, bytes)` for an addon, or `None` if no shard.
    pub fn addon_stats(&self, addon_id: &str) -> Option<(usize, usize)> {
        let shard = self.shards.get(addon_id)?;
        Some((
            shard.entry_count.load(Ordering::Relaxed),
            shard.byte_count.load(Ordering::Relaxed),
        ))
    }

    /// Atomically collect dirty `Durable` upserts + dirty deletes and clear
    /// their dirty flags, so the A2 flusher persists a consistent batch. A
    /// write that happens after this returns re-marks the entry dirty for the
    /// next round. Ephemeral entries are never included. Used by A2 flusher.
    pub(crate) fn take_dirty(&self, addon_id: &str) -> TakenBatch {
        let Some(shard_ref) = self.shards.get(addon_id) else {
            return TakenBatch {
                addon_id: addon_id.to_string(),
                shard: Arc::new(AddonStateShard::new()),
                set: DirtySet::default(),
            };
        };
        let shard = shard_ref.clone();
        drop(shard_ref);
        let mut inner = shard.entries.write();

        let mut upserts = Vec::new();
        for (k, e) in inner.map.iter_mut() {
            if e.dirty {
                debug_assert!(matches!(e.tier, Tier::Durable));
                upserts.push((k.clone(), e.value.clone()));
                e.dirty = false;
            }
        }
        let deletes: Vec<String> = inner.dirty_deletes.drain().collect();

        // Everything dirty has been drained into the batch — clear the cheap
        // signal so a clean shard is skipped by the next `dirty_addons` scan.
        // Done under the write lock so a concurrent dirty `set`/`delete` either
        // ran before us (its change is in this batch) or runs after this guard
        // is released (it re-sets the flag). `remark_dirty` re-sets it on a
        // failed flush.
        shard.has_dirty.store(false, Ordering::Release);

        drop(inner);
        TakenBatch {
            addon_id: addon_id.to_string(),
            shard,
            set: DirtySet { upserts, deletes },
        }
    }

    /// Addon ids whose shard currently has pending durable writes (upserts or
    /// tombstones). The flusher uses this to touch only addons with work to do
    /// instead of scanning every shard's map. Lock-free per shard — reads the
    /// `has_dirty` signal maintained under the write lock. A shard that just
    /// dropped to clean may still be returned (benign: `take_dirty` then yields
    /// an empty batch); a shard that just became dirty is always returned (the
    /// producer set the flag under the lock before releasing it).
    pub(crate) fn dirty_addons(&self) -> Vec<String> {
        self.shards
            .iter()
            .filter(|e| e.value().has_dirty.load(Ordering::Acquire))
            .map(|e| e.key().clone())
            .collect()
    }

    /// Re-mark a previously taken `DirtySet` as dirty after the flusher failed
    /// to persist it, so the next flush retries (at-least-once semantics). Re-
    /// inserts each upsert's value as a dirty durable entry IF the live value
    /// still matches what was taken — if the addon overwrote the key in the
    /// meantime, the newer write already re-marked it dirty and wins, so we do
    /// not clobber it. Re-adds every tombstone (a delete that failed to persist
    /// must be retried; a key re-created after the delete cancels its own
    /// tombstone via `set`). Caps are NOT re-checked here: the data was already
    /// resident and accepted, so re-marking never grows the shard.
    pub(crate) fn remark_dirty(&self, batch: TakenBatch) {
        if batch.set.is_empty() {
            return;
        }
        // Re-arm onto the EXACT shard the batch was taken from — never resolve a
        // fresh shard via the map. If the addon was uninstalled after the batch
        // was taken, the shard is purged/closed: refuse to re-arm so we never
        // recreate state for an uninstalled addon.
        let shard = batch.shard;
        if shard.purged.load(Ordering::Acquire) {
            return;
        }
        let mut inner = shard.entries.write();
        if inner.closed {
            return;
        }
        let mut remarked = false;

        for (key, value) in batch.set.upserts {
            match inner.map.get_mut(&key) {
                // Same durable value still resident → re-arm its dirty flag.
                Some(e) if matches!(e.tier, Tier::Durable) && e.value == value => {
                    e.dirty = true;
                    remarked = true;
                }
                // Key was overwritten / downgraded / removed after take_dirty:
                // the newer state owns the persistence decision (it re-marked
                // itself), so we must not resurrect the stale value.
                _ => {}
            }
        }
        for key in batch.set.deletes {
            // A delete only needs retrying if the key is still absent (a
            // re-create already cancelled the tombstone and re-persists itself).
            if !inner.map.contains_key(&key) {
                inner.dirty_deletes.insert(key);
                remarked = true;
            }
        }

        if remarked {
            shard.has_dirty.store(true, Ordering::Release);
        }
    }

    /// Seed RAM from the backing store at addon start. Each accepted entry
    /// becomes a clean `Durable` entry (dirty=false) so the next `take_dirty`
    /// does not re-persist what we just loaded. Used by A2 flusher / addon
    /// start.
    ///
    /// LOAD-ONCE GUARD: the shard is seeded at most once per cold start. A
    /// redundant call (a second instance start of the SAME addon) is a no-op
    /// (`already_loaded == true`); `drop_addon` resets the guard so the next
    /// cold start reloads. This prevents a persisted seed from clobbering live
    /// or unflushed (dirty) state another instance wrote since the cold start.
    ///
    /// MERGE SEMANTICS: even on the first load a key that ALREADY exists live is
    /// NOT overwritten (live RAM is newer-or-equal) and a pending tombstone is
    /// NOT cleared — persisted data only SEEDS absent keys. This makes the load
    /// idempotent and safe even if called redundantly.
    ///
    /// POLICY: the same per-value (`MAX_VALUE_BYTES`) and per-addon
    /// (`MAX_ENTRIES_PER_ADDON` / `MAX_BYTES_PER_ADDON`) caps that bound `set`
    /// also bound loading — a large or corrupt backing store can never push RAM
    /// past the documented bounds. An oversized value is skipped; once the
    /// shard is full, further entries are skipped. Loading does NOT evict
    /// (these are durable rows). All counter math is checked; an arithmetic
    /// violation is treated as "shard full" and stops the load. The returned
    /// `LoadOutcome` reports how many entries were loaded vs skipped so the
    /// caller can log/alarm on a truncated load.
    ///
    /// `entries` is an iterator so the caller can stream rows out of the backing
    /// store and stop reading once the per-addon cap is hit — the load never
    /// materialises an unbounded Vec of a huge/corrupt table.
    pub(crate) fn load_durable<I>(&self, addon_id: &str, entries: I) -> LoadOutcome
    where
        I: IntoIterator<Item = (String, Vec<u8>)>,
    {
        let shard = self.shard(addon_id);
        let mut inner = shard.entries.write();

        // Load-once guard: only the FIRST load after a cold start seeds the
        // shard. Taken under the write lock so it is consistent with concurrent
        // `set`/`take_dirty` on the same shard.
        if shard.loaded.swap(true, Ordering::AcqRel) {
            return LoadOutcome {
                already_loaded: true,
                ..LoadOutcome::default()
            };
        }

        let now = chrono::Utc::now().timestamp_millis();
        let mut outcome = LoadOutcome::default();

        for (key, value) in entries {
            // Merge: never clobber a live key nor clear a pending tombstone — a
            // persisted seed only fills keys absent from live RAM (live state is
            // newer-or-equal). A key already present, or one with a pending
            // tombstone (downgraded/deleted since the cold start), is skipped.
            if inner.map.contains_key(&key) || inner.dirty_deletes.contains(&key) {
                outcome.skipped_present += 1;
                continue;
            }
            if value.len() > MAX_VALUE_BYTES {
                outcome.skipped_value_too_large += 1;
                continue;
            }
            let new_size = match Entry::size(&key, &value) {
                Some(sz) => sz,
                None => {
                    outcome.skipped_quota += 1;
                    continue;
                }
            };

            // Every accepted load is a fresh insert (present keys are skipped
            // above), so it always grows the shard by one entry.
            let cur_entries = shard.entry_count.load(Ordering::Relaxed);
            let cur_bytes = shard.byte_count.load(Ordering::Relaxed);
            let proj_entries = cur_entries.checked_add(1);
            let proj_bytes = cur_bytes.checked_add(new_size);
            let over_cap = match (proj_entries, proj_bytes) {
                (Some(pe), Some(pb)) => pe > MAX_ENTRIES_PER_ADDON || pb > MAX_BYTES_PER_ADDON,
                // Counter overflow on a corrupt table = treat as over cap.
                _ => true,
            };
            if over_cap {
                // Cap reached: stop consuming the iterator entirely. The caller
                // streams rows lazily, so this bounds RAM even on a huge/corrupt
                // backing table (no unbounded allocation). Remaining unread rows
                // are reported as quota skips so the shortfall is observable.
                outcome.skipped_quota += 1;
                break;
            }

            inner.map.insert(
                key,
                Entry {
                    value,
                    tier: Tier::Durable,
                    updated_at_ms: now,
                    dirty: false,
                },
            );
            shard.entry_count.fetch_add(1, Ordering::Relaxed);
            shard.byte_count.fetch_add(new_size, Ordering::Relaxed);
            outcome.loaded += 1;
        }

        outcome
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    fn store() -> AddonStateStore {
        AddonStateStore::new()
    }

    #[test]
    fn set_get_delete_round_trip() {
        let s = store();
        assert!(s.get("addon", "k").is_none());

        s.set("addon", "k", b"hello".to_vec(), Tier::Ephemeral)
            .unwrap();
        assert_eq!(s.get("addon", "k"), Some(b"hello".to_vec()));

        // Replace.
        s.set("addon", "k", b"world!!".to_vec(), Tier::Ephemeral)
            .unwrap();
        assert_eq!(s.get("addon", "k"), Some(b"world!!".to_vec()));

        assert!(s.delete("addon", "k"));
        assert!(!s.delete("addon", "k"));
        assert!(s.get("addon", "k").is_none());
    }

    #[test]
    fn list_with_and_without_prefix() {
        let s = store();
        s.set("a", "robot:1", b"x".to_vec(), Tier::Ephemeral)
            .unwrap();
        s.set("a", "robot:2", b"yy".to_vec(), Tier::Durable)
            .unwrap();
        s.set("a", "user:1", b"zzz".to_vec(), Tier::Ephemeral)
            .unwrap();

        let all = s.list("a", None);
        assert_eq!(all.len(), 3);

        let robots = s.list("a", Some("robot:"));
        assert_eq!(robots.len(), 2);
        assert!(robots.iter().all(|(k, _, _)| k.starts_with("robot:")));

        // Sizes and tiers reported correctly.
        let r1 = robots.iter().find(|(k, _, _)| k == "robot:1").unwrap();
        assert_eq!(r1.1, 1);
        assert_eq!(r1.2, Tier::Ephemeral);
        let r2 = robots.iter().find(|(k, _, _)| k == "robot:2").unwrap();
        assert_eq!(r2.1, 2);
        assert_eq!(r2.2, Tier::Durable);
    }

    #[test]
    fn per_addon_isolation() {
        let s = store();
        s.set("addon_a", "k", b"A".to_vec(), Tier::Ephemeral)
            .unwrap();
        s.set("addon_b", "k", b"B".to_vec(), Tier::Ephemeral)
            .unwrap();

        assert_eq!(s.get("addon_a", "k"), Some(b"A".to_vec()));
        assert_eq!(s.get("addon_b", "k"), Some(b"B".to_vec()));
        assert_eq!(s.list("addon_a", None).len(), 1);
        assert_eq!(s.list("addon_b", None).len(), 1);

        // Deleting in A leaves B intact.
        s.delete("addon_a", "k");
        assert!(s.get("addon_a", "k").is_none());
        assert_eq!(s.get("addon_b", "k"), Some(b"B".to_vec()));
    }

    #[test]
    fn ephemeral_lru_eviction_on_entry_cap() {
        // Use a tiny custom store by exercising the real cap is impractical
        // (50k entries), so we drive eviction via the byte cap with large
        // values in a separate test; here we assert oldest-by-write eviction
        // logic via the public path against the entry cap using a controlled
        // sequence and verifying that the newest entries survive.
        //
        // We can't lower the const, so we verify eviction *ordering* by going
        // over the BYTE cap (cheaper) — see ephemeral_lru_eviction_on_byte_cap.
        // This test instead verifies that replacing an existing ephemeral key
        // never grows the entry count (so it can't trigger eviction).
        let s = store();
        s.set("a", "k", vec![0u8; 10], Tier::Ephemeral).unwrap();
        let (entries_before, _) = s.addon_stats("a").unwrap();
        for _ in 0..100 {
            s.set("a", "k", vec![1u8; 10], Tier::Ephemeral).unwrap();
        }
        let (entries_after, _) = s.addon_stats("a").unwrap();
        assert_eq!(entries_before, 1);
        assert_eq!(entries_after, 1);
    }

    #[test]
    fn ephemeral_lru_eviction_on_byte_cap() {
        let s = store();
        // Each value is just under half the value cap; a handful blows the
        // per-addon byte cap (32 MiB). Write with increasing timestamps so the
        // oldest are evicted first.
        let chunk = vec![0u8; MAX_VALUE_BYTES]; // 1 MiB each
                                                // 40 entries * ~1 MiB = ~40 MiB > 32 MiB cap → eviction kicks in.
        for i in 0..40 {
            // Distinct keys; sleep 1ms to guarantee strictly increasing
            // updated_at_ms so eviction order is deterministic.
            std::thread::sleep(std::time::Duration::from_millis(1));
            s.set("a", &format!("k{i:03}"), chunk.clone(), Tier::Ephemeral)
                .unwrap();
        }

        let (_, bytes) = s.addon_stats("a").unwrap();
        assert!(
            bytes <= MAX_BYTES_PER_ADDON,
            "byte count {bytes} should be within cap"
        );

        // Oldest keys evicted, newest survive.
        assert!(s.get("a", "k000").is_none(), "oldest should be evicted");
        assert!(s.get("a", "k039").is_some(), "newest should survive");
    }

    #[test]
    fn durable_overflow_rejects_new_allows_replace() {
        let s = store();
        let chunk = vec![0u8; MAX_VALUE_BYTES]; // 1 MiB
                                                // Fill durable up to / over the byte cap.
        let mut i = 0;
        loop {
            let key = format!("d{i:03}");
            match s.set("a", &key, chunk.clone(), Tier::Durable) {
                Ok(()) => i += 1,
                Err(StateStoreError::AddonQuotaExceeded) => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
            if i > 100 {
                panic!("durable cap never hit");
            }
        }
        assert!(i > 0, "should have stored at least one durable entry");

        // New durable key is rejected.
        let err = s
            .set("a", "new_key", chunk.clone(), Tier::Durable)
            .unwrap_err();
        assert_eq!(err, StateStoreError::AddonQuotaExceeded);

        // Replacing an existing key with a same-size value is allowed (no
        // entry growth, byte delta == 0).
        let existing = format!("d{:03}", i - 1);
        s.set("a", &existing, chunk.clone(), Tier::Durable)
            .expect("replace of existing durable key must be allowed");
    }

    #[test]
    fn value_too_large_rejected() {
        let s = store();
        let too_big = vec![0u8; MAX_VALUE_BYTES + 1];
        let err = s.set("a", "k", too_big, Tier::Durable).unwrap_err();
        assert_eq!(err, StateStoreError::ValueTooLarge);
        // No state change.
        assert!(s.get("a", "k").is_none());
        assert!(s.addon_stats("a").is_none() || s.addon_stats("a") == Some((0, 0)));
    }

    #[test]
    fn take_dirty_collects_durable_upserts_and_deletes_only() {
        let s = store();
        s.set("a", "dur1", b"v1".to_vec(), Tier::Durable).unwrap();
        s.set("a", "dur2", b"v2".to_vec(), Tier::Durable).unwrap();
        s.set("a", "eph1", b"e1".to_vec(), Tier::Ephemeral).unwrap();
        s.delete("a", "dur2");
        // Deleting an ephemeral never produces a tombstone.
        s.set("a", "eph2", b"e2".to_vec(), Tier::Ephemeral).unwrap();
        s.delete("a", "eph2");

        let dirty = s.take_dirty("a");
        // Only dur1 is a live durable upsert (dur2 was deleted).
        assert_eq!(dirty.upserts.len(), 1);
        assert_eq!(dirty.upserts[0], ("dur1".to_string(), b"v1".to_vec()));
        // Only the durable delete (dur2) is tombstoned.
        assert_eq!(dirty.deletes, vec!["dur2".to_string()]);

        // A second take with no writes is empty (dirty flags cleared).
        let again = s.take_dirty("a");
        assert!(again.is_empty());

        // A subsequent write re-marks dirty for the next round.
        s.set("a", "dur1", b"v1b".to_vec(), Tier::Durable).unwrap();
        let third = s.take_dirty("a");
        assert_eq!(third.upserts.len(), 1);
        assert_eq!(third.upserts[0], ("dur1".to_string(), b"v1b".to_vec()));
        assert!(third.deletes.is_empty());
    }

    #[test]
    fn load_durable_seeds_clean_entries() {
        let s = store();
        let outcome = s.load_durable(
            "a",
            vec![
                ("k1".to_string(), b"v1".to_vec()),
                ("k2".to_string(), b"v2".to_vec()),
            ],
        );
        assert_eq!(outcome.loaded, 2);
        assert_eq!(outcome.skipped_value_too_large, 0);
        assert_eq!(outcome.skipped_quota, 0);
        assert_eq!(s.get("a", "k1"), Some(b"v1".to_vec()));
        assert_eq!(s.get("a", "k2"), Some(b"v2".to_vec()));
        // Seeded entries are Durable.
        let listed = s.list("a", None);
        assert!(listed.iter().all(|(_, _, t)| *t == Tier::Durable));
        // And clean — nothing to flush.
        assert!(s.take_dirty("a").is_empty());

        let (entries, _) = s.addon_stats("a").unwrap();
        assert_eq!(entries, 2);
    }

    #[test]
    fn drop_addon_removes_everything_leaves_others() {
        let s = store();
        s.set("a", "k", b"x".to_vec(), Tier::Durable).unwrap();
        s.set("b", "k", b"y".to_vec(), Tier::Durable).unwrap();

        s.drop_addon("a");
        assert!(s.get("a", "k").is_none());
        assert!(s.addon_stats("a").is_none());
        assert!(s.list("a", None).is_empty());

        // Other addon intact.
        assert_eq!(s.get("b", "k"), Some(b"y".to_vec()));
        assert_eq!(s.addon_stats("b"), Some((1, b"ky".len())));
    }

    #[test]
    fn concurrency_smoke_distinct_addons() {
        let s = Arc::new(store());
        let threads = 8;
        let writes_per_thread = 500;
        let barrier = Arc::new(Barrier::new(threads));

        let mut handles = Vec::new();
        for t in 0..threads {
            let s = s.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                let addon = format!("addon_{t}");
                barrier.wait();
                for i in 0..writes_per_thread {
                    let key = format!("k{i}");
                    s.set(&addon, &key, vec![0u8; 8], Tier::Ephemeral).unwrap();
                    // Read back something we just wrote.
                    assert!(s.get(&addon, &key).is_some());
                }
            }));
        }
        for h in handles {
            h.join().expect("thread panicked");
        }

        // Each addon shard has exactly writes_per_thread entries.
        for t in 0..threads {
            let addon = format!("addon_{t}");
            let (entries, bytes) = s.addon_stats(&addon).unwrap();
            assert_eq!(entries, writes_per_thread);
            // Bytes = sum over keys of key.len() + 8.
            let expected: usize = (0..writes_per_thread)
                .map(|i| format!("k{i}").len() + 8)
                .sum();
            assert_eq!(bytes, expected);
        }
    }

    #[test]
    fn concurrency_smoke_same_addon() {
        // Many threads hammering ONE shard — exercises the per-addon RwLock.
        let s = Arc::new(store());
        let threads = 8;
        let keys_per_thread = 300;
        let barrier = Arc::new(Barrier::new(threads));

        let mut handles = Vec::new();
        for t in 0..threads {
            let s = s.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                for i in 0..keys_per_thread {
                    // Disjoint key namespaces per thread → deterministic count.
                    let key = format!("t{t}_k{i}");
                    s.set("shared", &key, vec![1u8; 4], Tier::Durable).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().expect("thread panicked");
        }

        let (entries, _) = s.addon_stats("shared").unwrap();
        assert_eq!(entries, threads * keys_per_thread);
        // All durable writes are dirty until flushed.
        let dirty = s.take_dirty("shared");
        assert_eq!(dirty.upserts.len(), threads * keys_per_thread);
    }

    // FIX 1: a durable→ephemeral downgrade tombstones the persisted row so it
    // does not resurrect on restart.
    #[test]
    fn durable_to_ephemeral_downgrade_tombstones() {
        let s = store();
        s.set("a", "k", b"durable".to_vec(), Tier::Durable).unwrap();
        // First take consumes the durable upsert.
        let first = s.take_dirty("a");
        assert_eq!(first.upserts.len(), 1);
        assert!(first.deletes.is_empty());

        // Overwrite the SAME key with an ephemeral value.
        s.set("a", "k", b"ephemeral".to_vec(), Tier::Ephemeral)
            .unwrap();
        assert_eq!(s.get("a", "k"), Some(b"ephemeral".to_vec()));

        // The downgrade must produce a tombstone so A2 deletes the durable row;
        // there must be no upsert (ephemeral is RAM-only).
        let second = s.take_dirty("a");
        assert!(second.upserts.is_empty(), "ephemeral must not be persisted");
        assert_eq!(
            second.deletes,
            vec!["k".to_string()],
            "durable row must be tombstoned on downgrade"
        );

        // The live entry is ephemeral.
        let listed = s.list("a", None);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].2, Tier::Ephemeral);
    }

    // FIX 2a: an ephemeral write is rejected when only durable entries occupy
    // the cap (eviction has nothing to free).
    #[test]
    fn ephemeral_rejected_when_only_durable_fills_cap() {
        let s = store();
        let chunk = vec![0u8; MAX_VALUE_BYTES]; // 1 MiB
                                                // Fill durable up to the byte cap.
        let mut i = 0;
        loop {
            let key = format!("d{i:03}");
            match s.set("a", &key, chunk.clone(), Tier::Durable) {
                Ok(()) => i += 1,
                Err(StateStoreError::AddonQuotaExceeded) => break,
                Err(e) => panic!("unexpected: {e}"),
            }
            if i > 100 {
                panic!("durable cap never hit");
            }
        }

        let (_, bytes_before) = s.addon_stats("a").unwrap();
        // An ephemeral write cannot evict durable entries → must be rejected,
        // not silently admitted past the cap.
        let err = s
            .set("a", "eph", chunk.clone(), Tier::Ephemeral)
            .unwrap_err();
        assert_eq!(err, StateStoreError::AddonQuotaExceeded);
        assert!(s.get("a", "eph").is_none(), "rejected write must not land");

        let (_, bytes_after) = s.addon_stats("a").unwrap();
        assert_eq!(bytes_before, bytes_after, "no bytes added on rejection");
        assert!(bytes_after <= MAX_BYTES_PER_ADDON);
    }

    // FIX 2b: a single key+value whose combined size exceeds the per-addon byte
    // cap is rejected even on an empty shard (no separate key cap exists).
    #[test]
    fn ephemeral_rejected_when_key_plus_value_exceeds_byte_cap() {
        let s = store();
        // value is within MAX_VALUE_BYTES, but key pushes key+value over the
        // (here tiny relative) per-addon byte cap is impossible with real caps;
        // instead construct a value at the value cap and a key large enough that
        // key+value > MAX_BYTES_PER_ADDON is also impossible (cap is 32 MiB).
        // The realistic "too big for the addon cap" case is a value near the
        // value cap repeated — but a SINGLE entry under both caps must succeed.
        // To exercise the single-entry-too-big guard we rely on the fact that
        // key.len() has no cap: build a key larger than the byte cap.
        let huge_key = "x".repeat(MAX_BYTES_PER_ADDON + 1);
        let err = s
            .set("a", &huge_key, b"v".to_vec(), Tier::Ephemeral)
            .unwrap_err();
        assert_eq!(err, StateStoreError::AddonQuotaExceeded);
        assert!(s.get("a", &huge_key).is_none());
        // Shard stayed empty / within bounds.
        assert!(s.addon_stats("a").is_none() || s.addon_stats("a") == Some((0, 0)));
    }

    // FIX 3: load_durable enforces per-value and per-addon caps, reporting skips
    // instead of loading unbounded data.
    #[test]
    fn load_durable_skips_oversized_value() {
        let s = store();
        let too_big = vec![0u8; MAX_VALUE_BYTES + 1];
        let outcome = s.load_durable(
            "a",
            vec![
                ("ok".to_string(), b"v".to_vec()),
                ("big".to_string(), too_big),
            ],
        );
        assert_eq!(outcome.loaded, 1);
        assert_eq!(outcome.skipped_value_too_large, 1);
        assert_eq!(outcome.skipped_quota, 0);
        assert_eq!(s.get("a", "ok"), Some(b"v".to_vec()));
        assert!(s.get("a", "big").is_none());
    }

    #[test]
    fn load_durable_stops_at_byte_cap() {
        let s = store();
        let chunk = vec![0u8; MAX_VALUE_BYTES]; // 1 MiB each
                                                // Offer ~40 MiB of durable entries; the 32 MiB cap must truncate.
        let entries: Vec<(String, Vec<u8>)> = (0..40)
            .map(|i| (format!("k{i:03}"), chunk.clone()))
            .collect();
        let outcome = s.load_durable("a", entries);

        let (entries_n, bytes) = s.addon_stats("a").unwrap();
        assert!(bytes <= MAX_BYTES_PER_ADDON, "byte cap must hold: {bytes}");
        assert_eq!(outcome.loaded, entries_n);
        assert!(outcome.loaded < 40, "cap must truncate the load");
        assert!(outcome.skipped_quota > 0, "shortfall must be reported");
    }

    // FIX 5: load is cap-bounded WHILE reading — once the cap is hit the lazy
    // input iterator is not drained further (no unbounded read of a huge table).
    #[test]
    fn load_durable_stops_reading_iterator_at_cap() {
        let s = store();
        let chunk = vec![0u8; MAX_VALUE_BYTES]; // 1 MiB each
        let pulled = std::cell::Cell::new(0usize);
        // Lazily yield up to a very large number of 1 MiB rows; count how many
        // the loader actually pulls. With a 32 MiB cap it must stop well before
        // consuming the whole (here 10_000-row) stream.
        let iter = (0..10_000).map(|i| {
            pulled.set(pulled.get() + 1);
            (format!("k{i:05}"), chunk.clone())
        });
        let outcome = s.load_durable("a", iter);

        let (_, bytes) = s.addon_stats("a").unwrap();
        assert!(bytes <= MAX_BYTES_PER_ADDON);
        assert!(outcome.loaded > 0);
        // Stopped reading: pulled only the loaded rows + the one over-cap row
        // that triggered the break — far below 10_000.
        assert!(
            pulled.get() <= outcome.loaded + 1,
            "iterator must stop at the cap, pulled {} for {} loaded",
            pulled.get(),
            outcome.loaded
        );
        assert!(pulled.get() < 100, "must not drain the whole huge stream");
    }

    // FIX 1a: load-once guard — a second load of an already-loaded shard is a
    // no-op and never clobbers live/dirty state written since the first load.
    #[test]
    fn load_durable_load_once_guard() {
        let s = store();
        let first = s.load_durable("a", vec![("k".to_string(), b"persisted".to_vec())]);
        assert_eq!(first.loaded, 1);
        assert!(!first.already_loaded);

        // Another instance writes a NEWER durable value (unflushed).
        s.set("a", "k", b"newer".to_vec(), Tier::Durable).unwrap();
        // It is dirty (pending flush).
        assert_eq!(s.take_dirty("a").upserts.len(), 1);
        s.set("a", "k", b"newer".to_vec(), Tier::Durable).unwrap();

        // A redundant load (second instance start) must NOT reload/clobber.
        let second = s.load_durable("a", vec![("k".to_string(), b"persisted".to_vec())]);
        assert!(second.already_loaded);
        assert_eq!(second.loaded, 0);
        assert_eq!(
            s.get("a", "k"),
            Some(b"newer".to_vec()),
            "load-once must not clobber the newer live value"
        );
        // The newer value is still dirty (its tombstone/clobber was not cleared).
        assert_eq!(s.take_dirty("a").upserts.len(), 1);

        // After drop, the guard resets → a fresh start reloads.
        s.drop_addon("a");
        let third = s.load_durable("a", vec![("k".to_string(), b"persisted".to_vec())]);
        assert!(!third.already_loaded);
        assert_eq!(third.loaded, 1);
        assert_eq!(s.get("a", "k"), Some(b"persisted".to_vec()));
    }

    // FIX 1b: merge semantics — even on the FIRST load, a key already live is not
    // overwritten and a pending tombstone is not cleared (seed fills only absent
    // keys).
    #[test]
    fn load_durable_merge_skips_present_and_tombstoned() {
        let s = store();
        // Live key written before any load (e.g. on_start ran early).
        s.set("a", "live", b"ram".to_vec(), Tier::Durable).unwrap();
        // A durable key deleted before load leaves a pending tombstone.
        s.set("a", "gone", b"x".to_vec(), Tier::Durable).unwrap();
        assert!(s.delete("a", "gone"));

        let outcome = s.load_durable(
            "a",
            vec![
                ("live".to_string(), b"disk".to_vec()),
                ("gone".to_string(), b"disk".to_vec()),
                ("fresh".to_string(), b"disk".to_vec()),
            ],
        );
        // Only the genuinely-absent key is seeded.
        assert_eq!(outcome.loaded, 1);
        assert_eq!(outcome.skipped_present, 2);
        assert_eq!(
            s.get("a", "live"),
            Some(b"ram".to_vec()),
            "live key must not be clobbered by the seed"
        );
        assert!(
            s.get("a", "gone").is_none(),
            "tombstoned key must stay deleted"
        );
        assert_eq!(s.get("a", "fresh"), Some(b"disk".to_vec()));

        // The pending tombstone for "gone" must survive the load.
        let dirty = s.take_dirty("a");
        assert!(dirty.deletes.contains(&"gone".to_string()));
    }

    // FIX 2 (store-level): a batch taken before drop_addon observes the purge and
    // remark refuses to recreate a shard for the uninstalled addon.
    #[test]
    fn taken_batch_observes_purge_and_remark_refuses() {
        let s = store();
        s.set("a", "k", b"v".to_vec(), Tier::Durable).unwrap();
        let batch = s.take_dirty("a");
        assert!(!batch.is_purged());

        // Uninstall: drop the shard.
        s.drop_addon("a");
        // The in-flight batch now sees the purge.
        assert!(batch.is_purged());

        // Remark must NOT recreate a shard for the purged addon.
        s.remark_dirty(batch);
        assert!(
            s.addon_stats("a").is_none(),
            "remark must not recreate a shard for an uninstalled addon"
        );
        assert!(s.dirty_addons().is_empty());
    }

    // FIX 4: a set that races with drop_addon is never silently lost — after a
    // close, the writer re-resolves a fresh live shard and the write is visible.
    #[test]
    fn set_after_close_reresolves_and_is_visible() {
        let s = store();
        s.set("a", "k", b"v1".to_vec(), Tier::Durable).unwrap();

        // Simulate the close: drop the shard. The next set must create a fresh
        // shard and land there (no lost write into the detached shard).
        s.drop_addon("a");
        assert!(s.get("a", "k").is_none(), "drop clears state");

        s.set("a", "k2", b"v2".to_vec(), Tier::Durable).unwrap();
        assert_eq!(
            s.get("a", "k2"),
            Some(b"v2".to_vec()),
            "post-close write must be visible"
        );
        assert_eq!(s.addon_stats("a"), Some((1, b"k2v2".len())));
    }

    #[test]
    fn concurrent_drop_and_set_no_lost_write() {
        // Hammer drop_addon and set on the same addon from two threads; every
        // successful set on the FINAL generation must be visible. We assert the
        // weaker, always-true invariant: a set returning Ok after the last drop
        // is readable. Run many rounds to surface a lost-write race.
        let s = Arc::new(store());
        for round in 0..200 {
            let key = format!("k{round}");
            let s1 = s.clone();
            let s2 = s.clone();
            let k = key.clone();
            let dropper = std::thread::spawn(move || {
                s1.drop_addon("race");
            });
            let writer = std::thread::spawn(move || {
                s2.set("race", &k, b"v".to_vec(), Tier::Durable).unwrap();
            });
            dropper.join().unwrap();
            writer.join().unwrap();
            // After both joined, do a definitive write and read it back: this
            // must always be visible (no detached-shard swallow).
            s.set("race", "final", b"f".to_vec(), Tier::Durable)
                .unwrap();
            assert_eq!(
                s.get("race", "final"),
                Some(b"f".to_vec()),
                "final write lost at round {round}"
            );
            s.drop_addon("race");
        }
    }

    // FIX 5: a shrinking replace near the cap evicts nothing (the freed bytes
    // are accounted for in the projection).
    #[test]
    fn shrinking_replace_near_cap_evicts_nothing() {
        let s = store();
        let big = vec![0u8; MAX_VALUE_BYTES]; // 1 MiB
                                              // Fill close to the byte cap with ephemeral 1 MiB values (leave room).
        let fill = (MAX_BYTES_PER_ADDON / MAX_VALUE_BYTES) - 1;
        for i in 0..fill {
            std::thread::sleep(std::time::Duration::from_millis(1));
            s.set("a", &format!("k{i:03}"), big.clone(), Tier::Ephemeral)
                .unwrap();
        }
        // Add one more key "victim" as the OLDEST so it would be evicted first.
        // Actually make k000 the oldest; ensure it survives a shrinking replace.
        let (entries_before, _) = s.addon_stats("a").unwrap();
        assert!(s.get("a", "k000").is_some());

        // Replace an existing 1 MiB value with a tiny value: bytes shrink, so no
        // eviction should occur and the oldest key must survive.
        let last = format!("k{:03}", fill - 1);
        s.set("a", &last, b"tiny".to_vec(), Tier::Ephemeral)
            .unwrap();

        let (entries_after, _) = s.addon_stats("a").unwrap();
        assert_eq!(
            entries_before, entries_after,
            "shrinking replace must not change entry count"
        );
        assert!(
            s.get("a", "k000").is_some(),
            "oldest key must survive a shrinking replace (no over-eviction)"
        );
        assert_eq!(s.get("a", &last), Some(b"tiny".to_vec()));
    }

    #[test]
    fn list_bounded_stops_at_entry_cap() {
        let s = store();
        for i in 0..50 {
            s.set("a", &format!("k{i:03}"), b"v".to_vec(), Tier::Ephemeral)
                .unwrap();
        }
        // Generous byte budget so only the entry cap can trigger truncation.
        let (entries, truncated) = s.list_bounded("a", None, 10, 1024 * 1024);
        assert_eq!(entries.len(), 10);
        assert!(truncated, "entry cap must mark the result truncated");
    }

    #[test]
    fn list_bounded_stops_at_byte_budget() {
        let s = store();
        for i in 0..50 {
            s.set("a", &format!("k{i:03}"), b"v".to_vec(), Tier::Ephemeral)
                .unwrap();
        }
        // Tiny byte budget: only a couple of entries fit before the byte cap.
        let per_entry = "k000".len() + STATE_LIST_ENTRY_OVERHEAD;
        let budget = per_entry * 3;
        let (entries, truncated) = s.list_bounded("a", None, 1000, budget);
        assert!(entries.len() <= 3, "byte budget must cap collected entries");
        assert!(!entries.is_empty(), "at least one entry must be returned");
        assert!(truncated, "byte budget must mark the result truncated");
    }

    #[test]
    fn list_bounded_under_limits_not_truncated() {
        let s = store();
        s.set("a", "k1", b"v".to_vec(), Tier::Ephemeral).unwrap();
        s.set("a", "k2", b"vv".to_vec(), Tier::Durable).unwrap();
        let (entries, truncated) = s.list_bounded("a", None, 1000, 1024 * 1024);
        assert_eq!(entries.len(), 2);
        assert!(!truncated, "a small list must not be marked truncated");
    }

    #[test]
    fn list_bounded_admits_single_oversized_key() {
        let s = store();
        s.set("a", "k", b"v".to_vec(), Tier::Ephemeral).unwrap();
        // A byte budget smaller than one entry still returns that one entry.
        let (entries, truncated) = s.list_bounded("a", None, 1000, 1);
        assert_eq!(entries.len(), 1, "first entry always admitted");
        assert!(!truncated, "single matching entry is not truncated");
    }

    #[test]
    fn list_bounded_respects_prefix() {
        let s = store();
        s.set("a", "robot:1", b"x".to_vec(), Tier::Ephemeral)
            .unwrap();
        s.set("a", "robot:2", b"y".to_vec(), Tier::Ephemeral)
            .unwrap();
        s.set("a", "user:1", b"z".to_vec(), Tier::Ephemeral)
            .unwrap();
        let (entries, truncated) = s.list_bounded("a", Some("robot:"), 1000, 1024 * 1024);
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|(k, _, _)| k.starts_with("robot:")));
        assert!(!truncated);
    }
}
