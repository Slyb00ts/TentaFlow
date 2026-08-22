// ===== File: services/graph/collection.rs — graph collection registry per (org, addon) =====
//
// The graph mirror of `vector::namespace::NamespaceManager`. Keeps a process-wide
// cache `(org_id, addon_id, collection) -> Arc<GraphEntry>` (DashMap, lock-free
// read on the hot path), where every open collection corresponds to a row in
// `addon_graph_collections` (PK `(org_id, addon_id, collection)`) and to a `.cozo`
// file. Default location: `<orgs_dir>/<org>/addons/<addon>/graph/<collection>.cozo`;
// a caller that keeps its graph outside the addon tree passes its own directory
// to `ensure_collection_at`.
//
// FILE LOCATION: for a collection that already exists, the
// `addon_graph_collections.file_path` column is the SOURCE OF TRUTH — open,
// delete and the `collection_file_path` accessor read it from the row, and the
// path is derived from the key (`file_path_for`) ONLY when creating a collection
// with no directory given. Mirrors `vector::namespace` (`get_or_create_at`):
// honouring a different directory on reopen would fork the collection into two
// files.
//
// Multi-tenant isolation: lookup is ALWAYS by `(org_id, addon_id, collection)`;
// `org_id` takes part in every SELECT/INSERT/UPDATE/DELETE, so the registries of
// two organizations never mix. On the default path (`file_path_for`) `org_id`
// takes part in the file path as well, so the same `addon_id` in two
// organizations writes to separate files. On the `ensure_collection_at` path the
// directory is caller-supplied and `org_id` does NOT take part in it: two
// organizations given the SAME directory map to the same file, so keeping them
// apart is the caller's job.
//
// MANAGER-OWNED LIFETIME (round 2 codex point A): `GraphManager` NEVER hands an
// `Arc<CozoBackend>` outside. Every cache entry keeps its backend behind a
// `RwLock<Option<CozoBackend>>` (`Option`, because eviction/delete takes the
// backend out and drops it under the write lock). All graph operations
// (upsert/query/neighbors/count/export) run INSIDE the manager while holding the
// per-collection lock — the caller gets a RESULT, not a handle. That at once:
//   - makes quota check+mutate ATOMIC (the write lock spans the count and the
//     Cozo mutation — two parallel writers can NOT exceed the limit, bug #4),
//   - makes delete/eviction safe: write lock -> `take()` the backend -> drop it
//     under the lock (sled flush+close) -> remove from the map -> delete the
//     files. No operation in flight, no deletion under a live handle (bug #5),
//   - makes `MAX_OPEN_GRAPHS` a REAL limit on open sled databases, because
//     eviction really closes the backend (no external `Arc` holds it, bug #3).
//
// COUNTER MODEL (round 3 codex bug F): Cozo is the source of truth for the GRAPH
// (the real number of nodes/edges), while the `node_count`/`edge_count` columns
// in `addon_graph_collections` are an ATOMIC QUOTA RESERVATION LEDGER per
// (org, addon). The global per-addon quota sums ACROSS collections, so it cannot
// be enforced by the per-collection lock alone — two writers to DIFFERENT
// collections of the same addon must compete for one global counter. A
// `BEGIN IMMEDIATE` transaction (the single SQLite writer) does that:
// `SELECT SUM(node_count) WHERE org,addon` -> if `+delta > limit` reject ->
// `UPDATE node_count += delta WHERE collection` -> COMMIT (the reservation). Only
// then comes the Cozo mutation; when Cozo fails we compensate with
// `node_count -= delta` (releasing the reservation). Drift between the ledger and
// Cozo (e.g. after a crash between the reservation and the mutation) is corrected
// by `reconcile_counts` when the collection is opened — it sets the counter to the
// real count from Cozo. The ledger may briefly overestimate (a reservation without
// a mutation) but never underestimates under live traffic, so the quota is safe
// (fail-safe towards rejection).
//
// Memory limit: sled takes a 1 GiB cache + a 500 ms flush for EVERY open database
// and Cozo does NOT allow tuning that (see backend.rs), so the manager uses
// lazy-open + LRU eviction: cap `MAX_OPEN_GRAPHS` simultaneously open backends,
// evicting the least recently used (LRU by `last_used`). Eviction closes the
// backend (dropped under the write lock); the on-disk data stays and the next
// access restores it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use dashmap::DashMap;

use super::backend::{CozoBackend, GraphBackend, GraphEngine};
use super::csr::Csr;
use super::error::{GraphError, Result};
use super::provenance::{self, DocRemoval};
use crate::db::DbPool;
use crate::services::vector::namespace::{
    validate_addon_id, validate_custom_dir, validate_namespace_name, validate_org_id,
};

/// Hard limit on graph collections per (org, addon). Every open collection is a
/// separate Cozo file + handle — kept in line with vector (10 namespaces).
pub const MAX_COLLECTIONS_PER_ADDON: u32 = 10;

/// Hard limit on nodes per (org, addon) (summed across collections). The default
/// ceiling when `addon_resource_limits.graph_nodes_max` is 0 (unset).
pub const MAX_NODES_PER_ADDON: u64 = 1_000_000;

/// Hard limit on edges per (org, addon) (summed across collections). The default
/// ceiling when `addon_resource_limits.graph_edges_max` is 0 (unset).
pub const MAX_EDGES_PER_ADDON: u64 = 5_000_000;

/// Maximum number of `CozoBackend` backends open SIMULTANEOUSLY in the cache.
/// Above the threshold the manager closes the least recently used one (LRU) —
/// every open sled is real memory overhead (1 GiB default cache, not tunable
/// through Cozo). On a phone (Android/iOS) the threshold is lower: device memory
/// is much smaller and a few open sled databases at ~1 GiB cache each would
/// saturate it immediately.
#[cfg(any(target_os = "android", target_os = "ios"))]
pub const MAX_OPEN_GRAPHS: usize = 4;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub const MAX_OPEN_GRAPHS: usize = 16;

/// Maximum number of re-fetch attempts in the `with_read`/`with_write` loop before
/// we force the canonical entry open WITHOUT eviction (a progress guarantee).
/// Under pressure (active key set > `MAX_OPEN_GRAPHS`) a caller could keep hitting
/// an entry evicted just before use (starvation/livelock); once the attempts are
/// exhausted we accept a TEMPORARY over-cap just to guarantee progress.
const MAX_REFETCH_ATTEMPTS: u32 = 64;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct GraphKey {
    org_id: String,
    addon_id: String,
    collection: String,
}

/// Backend open state within a single entry. The transitions are protected by the
/// entry write lock, so an open can NOT be duplicated (two parallel openers of the
/// same collection serialize on the write lock — sled takes an exclusive file lock
/// and two parallel `sled::open` calls on the same directory would end in
/// `WouldBlock`).
///
/// `Removed` is the entry's TERMINAL STATE (round 3 codex bug G/H): the entry was
/// evicted (eviction) or deleted (delete). Once in `Removed` the backend is closed
/// and the entry can no longer open the database — no stale `Arc<GraphEntry>` held
/// by another thread can resurrect a deleted/evicted database. A thread that sees
/// `Removed` re-fetches the canonical entry from the map (`entry_get*`) and
/// retries. Eviction and delete differ only in whether the DB row stays: eviction
/// keeps it (a re-fetch reopens the same file), delete removes it (a re-fetch sees
/// no row -> a fresh collection).
enum BackendSlot {
    Closed,
    Open(CozoBackend),
    Removed,
}

/// Result of lazily opening a backend (`ensure_open`). Distinguishes whether this
/// thread really opened the database, found it already open, or the entry is
/// terminally `Removed` (which requires a re-fetch in the calling loop).
enum OpenOutcome {
    Opened,
    AlreadyOpen,
    Removed,
}

/// Cache entry: a lazily opened backend behind an `RwLock` + the data needed to
/// (re)open it + the LRU marker. The backend is opened only in `with_read`/
/// `with_write` under the write lock (open dedup). `engine`/`file_path` are
/// immutable once the entry exists — they allow the backend to be restored after
/// eviction.
struct GraphEntry {
    slot: RwLock<BackendSlot>,
    engine: GraphEngine,
    file_path: PathBuf,
    /// Monotonic marker of the last access (from `GraphManager::clock`) — the
    /// smallest one is the least recently used, the eviction candidate.
    last_used: AtomicU64,
}

pub struct GraphManager {
    pool: DbPool,
    collections: DashMap<GraphKey, Arc<GraphEntry>>,
    /// Logical LRU clock — incremented on every access to an entry.
    clock: AtomicU64,
    /// Number of CURRENTLY OPEN sled backends (slot `Open`). Incremented on
    /// `Closed->Open`, decremented on `Open->{Closed,Removed}`. A hard account of
    /// open databases — no path opens a backend without incrementing it, so a
    /// stale `Arc` cannot open a database beside the counter (bug G).
    open_backends: AtomicU64,
    /// On-disk directory override — production uses `dirs::home_dir()`, tests
    /// inject a tempdir (see `tests::tempdir`) so they do not litter `~`.
    root_override: Option<PathBuf>,
}

impl GraphManager {
    pub fn new(pool: DbPool) -> Self {
        Self {
            pool,
            collections: DashMap::new(),
            clock: AtomicU64::new(0),
            open_backends: AtomicU64::new(0),
            root_override: None,
        }
    }

    /// Constructor pinning the data directory under `root` instead of
    /// `~/.tentaflow`. Used by integration tests and a future CLI.
    pub fn with_root(pool: DbPool, root: PathBuf) -> Self {
        Self {
            pool,
            collections: DashMap::new(),
            clock: AtomicU64::new(0),
            open_backends: AtomicU64::new(0),
            root_override: Some(root),
        }
    }

    /// Next logical clock marker (monotonic, process-wide).
    fn tick(&self) -> u64 {
        self.clock.fetch_add(1, Ordering::Relaxed)
    }

    /// `<root>/<org>/<addon>/graph/<collection>.cozo` (test override) or
    /// `<orgs_dir>/<org>/addons/<addon>/graph/<collection>.cozo` — the root comes
    /// from `paths::orgs_dir()` (which respects `addons_data_dir` from Settings).
    fn file_path_for(&self, org_id: &str, addon_id: &str, collection: &str) -> Result<PathBuf> {
        if let Some(root) = &self.root_override {
            Ok(root
                .join(org_id)
                .join(addon_id)
                .join("graph")
                .join(format!("{collection}.cozo")))
        } else {
            Ok(crate::paths::orgs_dir()
                .join(org_id)
                .join("addons")
                .join(addon_id)
                .join("graph")
                .join(format!("{collection}.cozo")))
        }
    }

    /// Collection file path: from the registry row when the collection exists,
    /// otherwise derived from the key. Public for the uninstall tests (the file
    /// must disappear after a successful delete and survive a failed one). Does
    /// not open the database.
    pub fn collection_file_path(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
    ) -> Result<PathBuf> {
        validate_org_id(org_id).map_err(map_vector_err)?;
        validate_addon_id(addon_id).map_err(map_vector_err)?;
        validate_namespace_name(collection).map_err(map_vector_err)?;
        match self.load_row(org_id, addon_id, collection)? {
            Some((_, file_path)) => Ok(file_path),
            None => self.file_path_for(org_id, addon_id, collection),
        }
    }

    /// Eviction: while the map holds more entries than `MAX_OPEN_GRAPHS`, closes
    /// the least recently used one (smallest `last_used`). The close is REAL and
    /// TERMINAL for the entry (bug G): under the slot write lock we set `Removed`
    /// (dropping the backend -> sled flush+close, decrementing the open counter)
    /// and ONLY THEN remove the entry from the map. The `mark_removed` BEFORE
    /// `remove` order is crucial: a stale `Arc<GraphEntry>` held by another thread
    /// waiting for the write lock sees `Removed` once the lock is released and
    /// re-fetches the canonical entry instead of opening an evicted database (no
    /// double-open `WouldBlock`, no exceeding `MAX_OPEN_GRAPHS`).
    fn evict_to_cap(&self) {
        while self.collections.len() > MAX_OPEN_GRAPHS {
            let victim = self
                .collections
                .iter()
                .min_by_key(|e| e.value().last_used.load(Ordering::Relaxed))
                .map(|e| e.key().clone());
            match victim {
                Some(k) => {
                    // `mark_removed` first (slot -> Removed under the write lock),
                    // and ONLY THEN take it off the map. The reverse order opened
                    // a window: a thread with a stale Arc (slot Closed) took the
                    // write lock and OPENED the backend between `remove` and
                    // `mark_removed`, while a parallel re-fetch interned a second
                    // entry for the same file (double-open, over-cap). With this
                    // order the stale Arc takes the slot lock, sees Removed and
                    // re-fetches the canonical entry.
                    if let Some(entry) = self.collections.get(&k).map(|e| e.value().clone()) {
                        self.mark_removed(&entry);
                        self.collections
                            .remove_if(&k, |_, v| Arc::ptr_eq(v, &entry));
                    }
                }
                None => break,
            }
        }
    }

    /// Switches the entry slot to the terminal `Removed` state under the write
    /// lock, dropping a live backend (sled flush+close) and decrementing the open
    /// counter. Called by eviction and delete — afterwards the entry never opens
    /// the database again. Idempotent.
    fn mark_removed(&self, entry: &Arc<GraphEntry>) {
        if let Ok(mut guard) = entry.slot.write() {
            if matches!(&*guard, BackendSlot::Open(_)) {
                self.open_backends.fetch_sub(1, Ordering::AcqRel);
            }
            *guard = BackendSlot::Removed;
        }
    }

    /// Atomically get-or-insert the entry for `key` in the map WITHOUT opening the
    /// backend (slot `Closed`). The backend opens lazily in `with_read`/
    /// `with_write` under the entry write lock, so two parallel accesses to the
    /// same collection share ONE entry and ONE open (dedup, bug #6). Bumps the LRU
    /// and evicts the excess if needed.
    fn intern_entry(
        &self,
        key: GraphKey,
        engine: GraphEngine,
        file_path: PathBuf,
    ) -> Arc<GraphEntry> {
        let now = self.tick();
        let entry = self
            .collections
            .entry(key)
            .or_insert_with(|| {
                Arc::new(GraphEntry {
                    slot: RwLock::new(BackendSlot::Closed),
                    engine,
                    file_path,
                    last_used: AtomicU64::new(now),
                })
            })
            .value()
            .clone();
        entry.last_used.store(now, Ordering::Relaxed);
        self.evict_to_cap();
        entry
    }

    /// Best-effort synchronization of the `node_count`/`edge_count` cache in SQLite
    /// with the real Cozo state (the source of truth). Called when a collection is
    /// opened.
    fn reconcile_counts(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        backend: &CozoBackend,
    ) {
        let (Ok(nodes), Ok(edges)) = (backend.node_count(), backend.edge_count()) else {
            return;
        };
        if let Ok(conn) = self.pool.write() {
            let _ = self.update_counts_locked(
                &conn,
                org_id,
                addon_id,
                collection,
                Some(nodes),
                Some(edges),
            );
        }
    }

    /// Interns (or fetches) the CANONICAL cache entry for the key WITHOUT touching
    /// the DB.
    /// Takes the engine and the file path from the registry row when one exists;
    /// for a NEW collection the path is `create_dir/<collection>.cozo`, or — when
    /// no directory was given — the path derived from the key (`file_path_for`).
    /// The backend is NOT opened here and the DB row is NOT created here — that
    /// happens UNDER the slot write lock in `with_write` (`ensure_row` +
    /// `open_backend`).
    ///
    /// `create_dir` applies ONLY to a collection that does not exist yet; on
    /// reopen it is ignored, because the stored `file_path` is the only source of
    /// truth about where the data lives (see the file header).
    ///
    /// Key to the cold-key create-vs-delete bug: the per-key serialization point
    /// (the slot lock of the canonical entry) is established BEFORE any DB/file
    /// side effect. Delete takes the slot lock of THE SAME canonical entry
    /// (`canonical_entry_for`), so cold-create and delete are mutually exclusive —
    /// live files/backend without an `addon_graph_collections` row can never come
    /// into existence.
    fn entry_get_or_create(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        create_dir: Option<&Path>,
    ) -> Result<Arc<GraphEntry>> {
        validate_org_id(org_id).map_err(map_vector_err)?;
        validate_addon_id(addon_id).map_err(map_vector_err)?;
        validate_namespace_name(collection).map_err(map_vector_err)?;
        // Validate before the cache lookup so a bad path is rejected regardless
        // of whether the collection happens to be cached right now.
        if let Some(dir) = create_dir {
            validate_custom_dir(dir).map_err(map_vector_err)?;
        }

        let key = GraphKey {
            org_id: org_id.to_string(),
            addon_id: addon_id.to_string(),
            collection: collection.to_string(),
        };
        if let Some(entry) = self.collections.get(&key) {
            entry.last_used.store(self.tick(), Ordering::Relaxed);
            return Ok(entry.value().clone());
        }

        // Pure row read — does NOT create the row.
        let (engine, file_path) = match self.load_row(org_id, addon_id, collection)? {
            Some((engine, file_path)) => (Self::parse_engine(&engine)?, file_path),
            None => (
                GraphEngine::default_for_build(),
                match create_dir {
                    Some(dir) => dir.join(format!("{collection}.cozo")),
                    None => self.file_path_for(org_id, addon_id, collection)?,
                },
            ),
        };
        Ok(self.intern_entry(key, engine, file_path))
    }

    /// Inserts the `addon_graph_collections` row when it is missing
    /// (insert-if-missing), under the slot write lock of the canonical entry — all
    /// DB side effects for a given key happen here, mutually exclusive with delete
    /// (the same slot lock). Idempotent: an existing row -> no-op (it preserves the
    /// ledger counters). The collection quota is enforced atomically in
    /// `insert_row` (`BEGIN IMMEDIATE`). Called ONLY while holding the entry slot
    /// write lock.
    fn ensure_row(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        engine: GraphEngine,
        file_path: &Path,
    ) -> Result<()> {
        if self.load_row(org_id, addon_id, collection)?.is_some() {
            return Ok(());
        }
        match self.insert_row(org_id, addon_id, collection, engine, file_path) {
            Ok(()) => Ok(()),
            // A parallel insert of the same new collection (bug #6): the second
            // thread gets a UNIQUE violation -> the row is already there, treat it
            // as success. Quota-exceeded is propagated normally.
            Err(GraphError::Db(_)) if self.load_row(org_id, addon_id, collection)?.is_some() => {
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Fetches the cache entry WITHOUT creating it (the read path). An error when
    /// the collection does not exist in the registry. The backend is opened lazily
    /// in `with_*`.
    fn entry_get(&self, org_id: &str, addon_id: &str, collection: &str) -> Result<Arc<GraphEntry>> {
        validate_org_id(org_id).map_err(map_vector_err)?;
        validate_addon_id(addon_id).map_err(map_vector_err)?;
        validate_namespace_name(collection).map_err(map_vector_err)?;

        let key = GraphKey {
            org_id: org_id.to_string(),
            addon_id: addon_id.to_string(),
            collection: collection.to_string(),
        };
        if let Some(entry) = self.collections.get(&key) {
            entry.last_used.store(self.tick(), Ordering::Relaxed);
            return Ok(entry.value().clone());
        }
        // Path from the row — the row is the source of truth about where the
        // data lives, so a collection created in an owner directory (Projects)
        // reopens from there and not from the addon tree.
        let Some((engine, file_path)) = self.load_row(org_id, addon_id, collection)? else {
            return Err(GraphError::CollectionNotFound {
                org_id: org_id.to_string(),
                addon_id: addon_id.to_string(),
                collection: collection.to_string(),
            });
        };
        Ok(self.intern_entry(key, Self::parse_engine(&engine)?, file_path))
    }

    /// Runs `f` under the READ lock of the collection backend (the
    /// query/neighbors/count/export paths). Re-fetch loop (bug G): we take the
    /// canonical entry from the map and try to open/use it; if the entry is
    /// `Removed` (evicted or deleted by a parallel thread) we do NOT open it — we
    /// re-fetch a fresh entry from the map and retry. Without creation (the read
    /// path): the re-fetch will not find a row -> `CollectionNotFound`.
    fn with_read<T>(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        mut f: impl FnMut(&CozoBackend) -> Result<T>,
    ) -> Result<T> {
        for attempt in 0..MAX_REFETCH_ATTEMPTS {
            let entry = self.entry_get(org_id, addon_id, collection)?;
            {
                let guard = entry
                    .slot
                    .read()
                    .map_err(|_| GraphError::Backend("graph entry lock poisoned".into()))?;
                match &*guard {
                    BackendSlot::Open(backend) => return f(backend),
                    BackendSlot::Removed => {} // re-fetch below
                    BackendSlot::Closed => {
                        drop(guard);
                        match self.ensure_open(&entry, org_id, addon_id, collection)? {
                            OpenOutcome::Opened | OpenOutcome::AlreadyOpen => {
                                let guard = entry.slot.read().map_err(|_| {
                                    GraphError::Backend("graph entry lock poisoned".into())
                                })?;
                                // Open -> use it; otherwise it became Removed and
                                // the loop below re-fetches.
                                if let BackendSlot::Open(backend) = &*guard {
                                    return f(backend);
                                }
                            }
                            OpenOutcome::Removed => {} // re-fetch below
                        }
                    }
                }
            }
            // `Removed` observed — refresh the entry and try again. A small
            // backoff against livelock under eviction pressure (bug 3).
            self.collections
                .remove_if(&self.key_of(org_id, addon_id, collection), |_, v| {
                    matches!(&*v.slot.read().unwrap(), BackendSlot::Removed)
                });
            self.refetch_backoff(attempt);
        }
        // Attempts exhausted — force the canonical entry open WITHOUT eviction
        // (a deliberate, temporary over-cap: progress instead of starvation).
        let entry = self.entry_get(org_id, addon_id, collection)?;
        let guard = self.force_open(&entry, org_id, addon_id, collection, false)?;
        match &*guard {
            BackendSlot::Open(backend) => f(backend),
            _ => Err(GraphError::Backend("transient: open contention".into())),
        }
    }

    /// Runs `f` under the WRITE lock of the collection backend (the mutation +
    /// quota paths). The lock spans the (lazy) open AND the whole of `f`, so a
    /// per-collection count+mutation is atomic against other writers and does not
    /// collide with delete/eviction (which take the write lock too). Re-fetch loop
    /// (bug G): `Removed` -> re-fetch the canonical entry from the map (creating a
    /// fresh one, because `create=true`) and retry.
    fn with_write<T>(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        create_dir: Option<&Path>,
        mut f: impl FnMut(&CozoBackend) -> Result<T>,
    ) -> Result<T> {
        for attempt in 0..MAX_REFETCH_ATTEMPTS {
            let entry = self.entry_get_or_create(org_id, addon_id, collection, create_dir)?;
            {
                let mut guard = entry
                    .slot
                    .write()
                    .map_err(|_| GraphError::Backend("graph entry lock poisoned".into()))?;
                match &*guard {
                    BackendSlot::Open(backend) => return f(backend),
                    BackendSlot::Closed => {
                        // DB/file side effects for a cold key happen HERE, under
                        // the slot lock (mutually exclusive with a delete of the
                        // same entry): first the `addon_graph_collections` row and
                        // only then the backend open. Without that, cold-create
                        // raced delete (live files/backend without a DB row).
                        self.ensure_row(
                            org_id,
                            addon_id,
                            collection,
                            entry.engine,
                            &entry.file_path,
                        )?;
                        let backend = self.open_backend(&entry, org_id, addon_id, collection)?;
                        self.open_backends.fetch_add(1, Ordering::AcqRel);
                        *guard = BackendSlot::Open(backend);
                        match &*guard {
                            BackendSlot::Open(backend) => return f(backend),
                            _ => unreachable!("just set Open under the write lock"),
                        }
                    }
                    BackendSlot::Removed => {} // re-fetch below
                }
            }
            self.collections
                .remove_if(&self.key_of(org_id, addon_id, collection), |_, v| {
                    matches!(&*v.slot.read().unwrap(), BackendSlot::Removed)
                });
            self.refetch_backoff(attempt);
        }
        // Attempts exhausted — force the canonical entry open WITHOUT eviction
        // (a deliberate, temporary over-cap: progress instead of starvation).
        let entry = self.entry_get_or_create(org_id, addon_id, collection, create_dir)?;
        let guard = self.force_open(&entry, org_id, addon_id, collection, true)?;
        match &*guard {
            BackendSlot::Open(backend) => f(backend),
            _ => Err(GraphError::Backend("transient: open contention".into())),
        }
    }

    /// Short backoff in the re-fetch loop: the first attempts yield the CPU
    /// (`yield_now`), later ones sleep a micro-interval so a stale entry does not
    /// keep coming back under eviction pressure (bug 3, anti-livelock).
    fn refetch_backoff(&self, attempt: u32) {
        if attempt < 8 {
            std::thread::yield_now();
        } else {
            std::thread::sleep(std::time::Duration::from_micros(50));
        }
    }

    /// Forces the backend of the canonical entry open under the write lock WITHOUT
    /// eviction and returns the held write guard with the slot `Open` (unless the
    /// entry is `Removed` — then the guard carries `Removed` and the caller returns
    /// a transient error). The progress guarantee once the re-fetch loop is
    /// exhausted: we accept a TEMPORARY over-cap instead of a livelock. A
    /// deliberate transient over-cap — the next `intern_entry`/eviction pulls the
    /// number of open backends back to the cap.
    ///
    /// `create_row=true` (the write path) forces `ensure_row` UNDER the slot lock
    /// before the open — the same cold-key vs delete serialization as in the main
    /// `with_write` loop. Read passes `false` (it does NOT create a row). The key
    /// is always needed to reconcile the counters in `open_backend`.
    fn force_open<'a>(
        &self,
        entry: &'a Arc<GraphEntry>,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        create_row: bool,
    ) -> Result<std::sync::RwLockWriteGuard<'a, BackendSlot>> {
        let mut guard = entry
            .slot
            .write()
            .map_err(|_| GraphError::Backend("graph entry lock poisoned".into()))?;
        if let BackendSlot::Closed = &*guard {
            if create_row {
                self.ensure_row(org_id, addon_id, collection, entry.engine, &entry.file_path)?;
            }
            let backend = self.open_backend(entry, org_id, addon_id, collection)?;
            self.open_backends.fetch_add(1, Ordering::AcqRel);
            *guard = BackendSlot::Open(backend);
        }
        Ok(guard)
    }

    /// Lazily opens the entry backend under the write lock (when `Closed`). Dedup:
    /// the first thread opens, the rest see `Open`. `Removed` (an eviction/delete
    /// in progress) -> `OpenOutcome::Removed`, without opening the database (bug G).
    fn ensure_open(
        &self,
        entry: &Arc<GraphEntry>,
        org_id: &str,
        addon_id: &str,
        collection: &str,
    ) -> Result<OpenOutcome> {
        let mut guard = entry
            .slot
            .write()
            .map_err(|_| GraphError::Backend("graph entry lock poisoned".into()))?;
        match &*guard {
            BackendSlot::Open(_) => Ok(OpenOutcome::AlreadyOpen),
            BackendSlot::Removed => Ok(OpenOutcome::Removed),
            BackendSlot::Closed => {
                let backend = self.open_backend(entry, org_id, addon_id, collection)?;
                self.open_backends.fetch_add(1, Ordering::AcqRel);
                *guard = BackendSlot::Open(backend);
                Ok(OpenOutcome::Opened)
            }
        }
    }

    /// Opens a `CozoBackend` from the entry data and reconciles the registry
    /// counters with Cozo (the source of truth). Called ONLY while holding the
    /// entry slot write lock.
    ///
    /// The `(org, addon, collection)` key is PASSED IN by the caller (which always
    /// has it), NOT reconstructed by scanning the DashMap. Scanning the map under
    /// the slot write lock was a deadlock source: a writer held the slot lock and
    /// waited for the read lock of a DashMap shard, while a parallel delete
    /// (`canonical_entry_for` -> `collections.entry`) held the write lock of that
    /// same shard and waited for this slot lock (AB-BA). With no map scan under the
    /// slot lock the cycle disappears.
    fn open_backend(
        &self,
        entry: &Arc<GraphEntry>,
        org_id: &str,
        addon_id: &str,
        collection: &str,
    ) -> Result<CozoBackend> {
        let backend = CozoBackend::open_or_create(&entry.file_path, entry.engine)?;
        self.reconcile_counts(org_id, addon_id, collection, &backend);
        Ok(backend)
    }

    /// Map key from its components (a re-fetch loop helper).
    fn key_of(&self, org_id: &str, addon_id: &str, collection: &str) -> GraphKey {
        GraphKey {
            org_id: org_id.to_string(),
            addon_id: addon_id.to_string(),
            collection: collection.to_string(),
        }
    }

    /// Creates the collection (when it does not exist) and opens its backend. The
    /// public equivalent of the former `get_or_create`, but it does NOT return a
    /// handle — it only confirms existence. Used by paths that want creation
    /// guaranteed before a series of operations.
    pub fn ensure_collection(&self, org_id: &str, addon_id: &str, collection: &str) -> Result<()> {
        // The DB row (`ensure_row`) and the backend open happen UNDER the slot
        // write lock inside `with_write` — the same cold-key vs delete
        // serialization as for upserts. The collection quota is checked atomically
        // in `insert_row` (BEGIN IMMEDIATE).
        self.with_write(org_id, addon_id, collection, None, |_| Ok(()))
    }

    /// Like [`Self::ensure_collection`], but a collection created by this call
    /// lands in `dir/<collection>.cozo` instead of the addon graph tree — for a
    /// caller that keeps its graph next to the rest of its data. The directory is
    /// caller-supplied and the caller owns its uniqueness (`org_id` does not take
    /// part in it); validation and the per (org, addon) quotas are identical to
    /// `ensure_collection`.
    ///
    /// When the collection ALREADY exists (a registry row is present), `dir` is
    /// ignored and the stored `file_path` wins — the row is the only source of
    /// truth about where the data lives, so honouring a different directory on
    /// reopen would fork the collection and show the existing graph as empty.
    /// Mirrors `NamespaceManager::get_or_create_at`.
    pub fn ensure_collection_at(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        dir: &Path,
    ) -> Result<()> {
        self.with_write(org_id, addon_id, collection, Some(dir), |_| Ok(()))
    }

    /// Whether the collection exists in the registry (without opening the backend).
    pub fn collection_exists(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
    ) -> Result<bool> {
        validate_org_id(org_id).map_err(map_vector_err)?;
        validate_addon_id(addon_id).map_err(map_vector_err)?;
        validate_namespace_name(collection).map_err(map_vector_err)?;
        Ok(self.load_row(org_id, addon_id, collection)?.is_some())
    }

    /// Node count of the collection (from Cozo, the source of truth). Read lock.
    pub fn node_count(&self, org_id: &str, addon_id: &str, collection: &str) -> Result<u64> {
        self.with_read(org_id, addon_id, collection, |b| b.node_count())
    }

    /// Edge count of the collection (from Cozo, the source of truth). Read lock.
    pub fn edge_count(&self, org_id: &str, addon_id: &str, collection: &str) -> Result<u64> {
        self.with_read(org_id, addon_id, collection, |b| b.edge_count())
    }

    /// CSR export of the collection (for PPR in Rust). Opens the collection when
    /// needed. The read lock spans the whole export, so the CSR is a consistent
    /// snapshot.
    pub fn export_csr(&self, org_id: &str, addon_id: &str, collection: &str) -> Result<Csr> {
        self.with_read(org_id, addon_id, collection, |b| b.export_edges())
    }

    /// Neighbours of a node (out/in/both, optional relation filter, limit). Read lock.
    #[allow(clippy::too_many_arguments)]
    pub fn neighbors(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        node: &str,
        direction: super::backend::NeighborDir,
        rel: Option<&str>,
        limit: u32,
    ) -> Result<Vec<(String, String, f64)>> {
        self.with_read(org_id, addon_id, collection, |b| {
            b.neighbors(node, direction, rel, limit)
        })
    }

    /// Global PageRank, top-N descending. Read lock.
    ///
    /// Computed in Rust over the CSR (`personalized_pagerank` with EMPTY seeds =
    /// uniform teleportation = classic global PageRank), because the built-in Cozo
    /// PageRank (`graph-algo`) pulls in the `graph_builder` crate, which conflicts
    /// with the binary's rayon version (E0271/E0308 in a full build). This is
    /// exactly the same semantics as the former cozo `<~ PageRank`, but without the
    /// incompatible dependency.
    pub fn pagerank(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        top_n: u32,
        damping: f64,
        iterations: u32,
    ) -> Result<Vec<(String, f64)>> {
        let csr = self.export_csr(org_id, addon_id, collection)?;
        let mut scored = super::ppr::personalized_pagerank(&csr, &[], damping, iterations as usize);
        scored.truncate(top_n as usize);
        Ok(scored)
    }

    /// Personalized PageRank computed in Rust over the CSR from Cozo (`ppr.rs`).
    /// The seeds are the node ids that form the personalization vector; unknown ids
    /// are skipped. Returns the top-N `(id, score)` descending. The read lock spans
    /// the CSR export, so PPR is computed over a consistent graph snapshot.
    ///
    /// SEED SEMANTICS (retrieval with EXPLICIT anchors): this path is always called
    /// with an explicitly given seed list (host-fn `graph_ppr_v1`, `graph_search`
    /// op=ppr, GraphRAG). We therefore distinguish two cases:
    ///   * `seeds` EMPTY — the caller gave no anchors -> global PageRank (uniform
    ///     teleportation in `personalized_pagerank`); a legal input.
    ///   * `seeds` NON-EMPTY, but NONE of them exists in the graph (all dropped by
    ///     the `id_index` filter) -> an EMPTY result. Personalized PageRank with no
    ///     anchors means no result, NOT a global ranking — otherwise a query about
    ///     entities outside the KG would get the top global entities (noise, and it
    ///     breaks the "no entities -> vector only" degradation).
    #[allow(clippy::too_many_arguments)]
    pub fn ppr(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        seeds: &[(String, f64)],
        top_n: u32,
        damping: f64,
        iterations: u32,
    ) -> Result<Vec<(String, f64)>> {
        let csr = self.export_csr(org_id, addon_id, collection)?;
        let index = csr.id_index();
        // Map `(id, weight)` -> `(idx, weight)`; unknown ids are skipped. The
        // weights are carried by the `personalized_pagerank` personalization
        // vector (P_init, R6).
        let seed_indices: Vec<(usize, f64)> = seeds
            .iter()
            .filter_map(|(id, w)| index.get(id.as_str()).map(|&idx| (idx, *w)))
            .collect();
        // Explicit seeds were given, but none hit the graph -> no valid anchors.
        // Return empty instead of degenerating into global PageRank.
        if !seeds.is_empty() && seed_indices.is_empty() {
            return Ok(Vec::new());
        }
        let mut scored =
            super::ppr::personalized_pagerank(&csr, &seed_indices, damping, iterations as usize);
        scored.truncate(top_n as usize);
        Ok(scored)
    }

    /// PPR with the full structure-aware P_init signal (MemGraphRAG §6.2) computed
    /// over ONE CSR snapshot. This is the GraphRAG retrieval path: the log-degree
    /// penalty, the anchor cap and PPR itself MUST see the same graph, so we export
    /// the CSR exactly once (otherwise the degrees and the ranking would come from
    /// different snapshots, and an anchor capped before reweighting would never be
    /// considered).
    ///
    /// `seeds` are the BASE weights (`base × relevance`, NOT capped yet) — the
    /// caller (the RAG adapter) adds the relevance boost, because it depends on the
    /// vector passages. Here we close P_init over that same CSR:
    ///   1. KNOWN FILTER: map the candidates onto CSR indices and REJECT the unknown
    ///      ones BEFORE anything is capped. A high-weight seed outside the graph
    ///      must not push a known anchor out of the cap — otherwise PPR would get an
    ///      empty/impoverished vector despite known anchors being present.
    ///   2. LOG-DEGREE PENALTY: `w /= 1 + ln(1 + degree)` on the KNOWN anchors from
    ///      this CSR (a hub node is a weak, poorly selective anchor).
    ///   3. CAP AFTER REWEIGHTING: sort the KNOWN anchors by their FINAL weight
    ///      (descending) and truncate to `max_seeds`. An anchor with a high weight
    ///      after log-degree/relevance but lexically outside the first `max_seeds`
    ///      IS therefore considered.
    ///
    /// Empty/unknown anchor semantics as in [`Self::ppr`]: explicit but entirely
    /// unknown seeds -> an empty result (we do not degenerate into global PageRank).
    #[allow(clippy::too_many_arguments)]
    pub fn ppr_with_p_init(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        seeds: &[(String, f64)],
        max_seeds: usize,
        top_n: u32,
        damping: f64,
        iterations: u32,
    ) -> Result<Vec<(String, f64)>> {
        let csr = self.export_csr(org_id, addon_id, collection)?;
        let index = csr.id_index();
        let degrees = csr.total_degrees();

        // Step 1: map onto indices of THIS CSR and FILTER OUT the unknown ones
        // BEFORE the cap — an unknown, high-weight anchor must not push a known one
        // out of max_seeds.
        let mut weighted: Vec<(usize, f64)> = seeds
            .iter()
            .filter_map(|(id, w)| index.get(id.as_str()).map(|&idx| (idx, *w)))
            .collect();
        // Explicit anchors were given, but none hit the graph -> empty (degradation).
        if !seeds.is_empty() && weighted.is_empty() {
            return Ok(Vec::new());
        }

        // Step 2: log-degree penalty on the KNOWN anchors from this snapshot.
        for (idx, w) in &mut weighted {
            *w /= 1.0 + (1.0 + degrees[*idx] as f64).ln();
        }

        // Step 3: cap AFTER reweighting — sort by final weight, truncate to
        // max_seeds. The cap covers ONLY known anchors, so unknown ones take no slots.
        weighted.sort_by(|a, b| b.1.total_cmp(&a.1));
        weighted.truncate(max_seeds);

        let seed_indices = weighted;
        let mut scored =
            super::ppr::personalized_pagerank(&csr, &seed_indices, damping, iterations as usize);
        scored.truncate(top_n as usize);
        Ok(scored)
    }

    /// Soft-delete (tombstone) of node `id` + exclusion of its edges from
    /// retrieval. The node row STAYS (an O(1) `:put` of the marker), so the node
    /// count and the quota ledger do NOT change — a physical purge is a later
    /// compaction. Incident edges are skipped by retrieval (a join with
    /// non-tombstone nodes), not physically deleted. Write lock. Returns
    /// `(removed, node_count, edge_count)`.
    pub fn delete_node_in(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        id: &str,
    ) -> Result<(bool, u64, u64)> {
        self.with_write(org_id, addon_id, collection, None, |backend| {
            let removed = backend.delete_node(id)?;
            let nodes = backend.node_count()?;
            let edges = backend.edge_count()?;
            Ok((removed, nodes, edges))
        })
    }

    /// Soft-delete of a single edge `(src, rel, dst)` (`alive=false`, O(1)). The
    /// row stays (the quota ledger is unchanged); retrieval skips it. Write lock.
    pub fn delete_edge_in(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        src: &str,
        rel: &str,
        dst: &str,
    ) -> Result<(bool, u64, u64)> {
        self.with_write(org_id, addon_id, collection, None, |backend| {
            let removed = backend.delete_edge(src, rel, dst)?;
            let nodes = backend.node_count()?;
            let edges = backend.edge_count()?;
            Ok((removed, nodes, edges))
        })
    }

    /// Withdraws one DOCUMENT from the collection: every node and edge whose
    /// stored `provenance` names it drops that membership, and only a row left
    /// with NO document behind it is soft-deleted. A row several documents named
    /// survives with a shrunken set (see `provenance::without_doc`), which is why
    /// `(nodes_removed, edges_removed)` counts TOMBSTONES, not touched rows.
    ///
    /// The whole sweep runs under ONE write lock, so a concurrent ingest of the
    /// same document cannot interleave a half-deleted state with fresh writes.
    /// Provenance is scanned in Rust rather than matched in Datalog because the
    /// column holds an opaque JSON document, not a typed field — a substring
    /// match would also hit a `source_id` or a `path` that happens to contain the
    /// same text, and delete another document's rows.
    pub fn delete_document_in(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        doc_id: &str,
    ) -> Result<(u64, u64)> {
        // A collection that was never created contributed nothing — and going
        // through `with_write` would CREATE it, in the default addon tree.
        if !self.collection_exists(org_id, addon_id, collection)? {
            return Ok((0, 0));
        }
        self.with_write(org_id, addon_id, collection, None, |backend| {
            let edge_rows = backend.run_query("?[src, rel, dst, provenance] := *edges{src, rel, dst, provenance}")?;
            let mut edges_removed = 0u64;
            for row in &edge_rows.rows {
                let (Some(src), Some(rel), Some(dst), Some(prov)) = (
                    row.first().and_then(|v| v.get_str()),
                    row.get(1).and_then(|v| v.get_str()),
                    row.get(2).and_then(|v| v.get_str()),
                    row.get(3).and_then(|v| v.get_str()),
                ) else {
                    continue;
                };
                match provenance::without_doc(prov, doc_id) {
                    DocRemoval::NotNamed => {}
                    DocRemoval::Shrunk(remaining) => {
                        backend.set_edge_provenance(src, rel, dst, &remaining)?
                    }
                    DocRemoval::Emptied => {
                        if backend.delete_edge(src, rel, dst)? {
                            edges_removed += 1;
                        }
                    }
                }
            }

            let node_rows = backend.run_query("?[id, provenance] := *nodes{id, provenance}")?;
            let mut nodes_removed = 0u64;
            for row in &node_rows.rows {
                let (Some(id), Some(prov)) = (
                    row.first().and_then(|v| v.get_str()),
                    row.get(1).and_then(|v| v.get_str()),
                ) else {
                    continue;
                };
                match provenance::without_doc(prov, doc_id) {
                    DocRemoval::NotNamed => {}
                    DocRemoval::Shrunk(remaining) => backend.set_node_provenance(id, &remaining)?,
                    DocRemoval::Emptied => {
                        if backend.delete_node(id)? {
                            nodes_removed += 1;
                        }
                    }
                }
            }
            Ok((nodes_removed, edges_removed))
        })
    }

    /// Node soft-delete alias for the `GraphDeleteTarget::Tombstone` variant — the
    /// same semantics as `delete_node_in` (a node delete in Stage 0 = a tombstone).
    pub fn tombstone_node_in(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        id: &str,
    ) -> Result<(bool, u64, u64)> {
        self.delete_node_in(org_id, addon_id, collection, id)
    }

    /// Node upsert enforcing the GLOBAL node quota per (org, addon), atomic ACROSS
    /// collections too (bug F). Protocol: under the collection write lock we read
    /// the stored provenance, which answers at once `is_new` (an absent row) and
    /// which documents already name this node (replacing an existing id does not
    /// change the sum, so it reserves no quota). For a new id we reserve 1 unit in
    /// the atomic SQLite ledger (`reserve_node_quota`: `BEGIN IMMEDIATE` ->
    /// `SELECT SUM(node_count) WHERE org,addon` -> if `+1 > limit` reject ->
    /// `UPDATE node_count+=1 WHERE collection` -> COMMIT). The global SQLite writer
    /// serializes this across ALL collections of the addon — two writers to
    /// different collections compete for the same SUM, so together they cannot
    /// exceed the limit. Then comes the Cozo mutation; when it fails -> the
    /// `node_count-=1` compensation (releasing the reservation) and the error is
    /// propagated. The id existence check is parameterized (`$id`), not `format!()`.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_node_with_quota(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        id: &str,
        label: &str,
        props_json: &str,
        provenance_json: &str,
    ) -> Result<u64> {
        let max_nodes = self.resolve_node_limit(addon_id);

        self.with_write(org_id, addon_id, collection, None, |backend| {
            let stored = backend.node_provenance(id)?;
            let is_new = stored.is_none();
            // Cozo's `:put` is last-writer-wins with no set-union, so the union
            // of the document sets is computed HERE, inside the write lock that
            // already spans the quota check+mutate. Doing it in the caller would
            // read a set another ingest may replace before this `:put` lands.
            let provenance_json = provenance::merge(stored.as_deref(), provenance_json);
            if is_new {
                // Reservation in the atomic ledger BEFORE the Cozo mutation.
                self.reserve_node_quota(org_id, addon_id, collection, max_nodes)?;
                // Graph mutation; on error release the reservation.
                if let Err(e) = backend.upsert_node(id, label, props_json, &provenance_json) {
                    self.release_node_quota(org_id, addon_id, collection);
                    return Err(e);
                }
            } else {
                backend.upsert_node(id, label, props_json, &provenance_json)?;
            }
            backend.node_count()
        })
    }

    /// Edge upsert enforcing the GLOBAL edge quota per (org, addon), atomic across
    /// collections (bug F). Symmetric to `upsert_node_with_quota`: a reservation in
    /// the atomic ledger -> the Cozo mutation -> compensation on error.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_edge_with_quota(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        src: &str,
        rel: &str,
        dst: &str,
        weight: f64,
        props_json: &str,
        provenance_json: &str,
    ) -> Result<u64> {
        let max_edges = self.resolve_edge_limit(addon_id);

        self.with_write(org_id, addon_id, collection, None, |backend| {
            let stored = backend.edge_provenance(src, rel, dst)?;
            let is_new = stored.is_none();
            let provenance_json = provenance::merge(stored.as_deref(), provenance_json);
            if is_new {
                self.reserve_edge_quota(org_id, addon_id, collection, max_edges)?;
                if let Err(e) =
                    backend.upsert_edge(src, rel, dst, weight, props_json, &provenance_json)
                {
                    self.release_edge_quota(org_id, addon_id, collection);
                    return Err(e);
                }
            } else {
                backend.upsert_edge(src, rel, dst, weight, props_json, &provenance_json)?;
            }
            backend.edge_count()
        })
    }

    /// Atomic reservation of 1 node in the global quota ledger (bug F). In a single
    /// `BEGIN IMMEDIATE`: sums `node_count` over ALL collections of (org, addon);
    /// when `sum + 1 > max` it returns `NodeQuotaExceeded` (rollback), otherwise it
    /// increments `node_count` of the current collection by 1 and commits. The
    /// global SQLite writer makes this mutually exclusive across collections.
    fn reserve_node_quota(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        max_nodes: u64,
    ) -> Result<()> {
        let conn = self
            .pool
            .write()
            .map_err(|_| GraphError::Db("pool mutex poisoned".into()))?;
        conn.execute("BEGIN IMMEDIATE", [])
            .map_err(|e| GraphError::Db(e.to_string()))?;

        let total: i64 = match conn.query_row(
            "SELECT COALESCE(SUM(node_count), 0) FROM addon_graph_collections \
             WHERE org_id = ?1 AND addon_id = ?2",
            rusqlite::params![org_id, addon_id],
            |r| r.get(0),
        ) {
            Ok(t) => t,
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                return Err(GraphError::Db(e.to_string()));
            }
        };
        let current = total.max(0) as u64;
        if current.saturating_add(1) > max_nodes {
            let _ = conn.execute("ROLLBACK", []);
            return Err(GraphError::NodeQuotaExceeded {
                addon_id: addon_id.to_string(),
                current,
                max: max_nodes,
            });
        }
        if let Err(e) = conn.execute(
            "UPDATE addon_graph_collections SET node_count = node_count + 1 \
             WHERE org_id = ?1 AND addon_id = ?2 AND collection = ?3",
            rusqlite::params![org_id, addon_id, collection],
        ) {
            let _ = conn.execute("ROLLBACK", []);
            return Err(GraphError::Db(e.to_string()));
        }
        conn.execute("COMMIT", [])
            .map_err(|e| GraphError::Db(e.to_string()))?;
        Ok(())
    }

    /// Releases the reservation of 1 node (compensation when the Cozo mutation
    /// failed). Best effort — `reconcile_counts` on open corrects the drift from
    /// Cozo anyway.
    fn release_node_quota(&self, org_id: &str, addon_id: &str, collection: &str) {
        if let Ok(conn) = self.pool.write() {
            let _ = conn.execute(
                "UPDATE addon_graph_collections \
                 SET node_count = MAX(node_count - 1, 0) \
                 WHERE org_id = ?1 AND addon_id = ?2 AND collection = ?3",
                rusqlite::params![org_id, addon_id, collection],
            );
        }
    }

    /// Atomic reservation of 1 edge in the global quota ledger (bug F).
    fn reserve_edge_quota(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        max_edges: u64,
    ) -> Result<()> {
        let conn = self
            .pool
            .write()
            .map_err(|_| GraphError::Db("pool mutex poisoned".into()))?;
        conn.execute("BEGIN IMMEDIATE", [])
            .map_err(|e| GraphError::Db(e.to_string()))?;

        let total: i64 = match conn.query_row(
            "SELECT COALESCE(SUM(edge_count), 0) FROM addon_graph_collections \
             WHERE org_id = ?1 AND addon_id = ?2",
            rusqlite::params![org_id, addon_id],
            |r| r.get(0),
        ) {
            Ok(t) => t,
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                return Err(GraphError::Db(e.to_string()));
            }
        };
        let current = total.max(0) as u64;
        if current.saturating_add(1) > max_edges {
            let _ = conn.execute("ROLLBACK", []);
            return Err(GraphError::EdgeQuotaExceeded {
                addon_id: addon_id.to_string(),
                current,
                max: max_edges,
            });
        }
        if let Err(e) = conn.execute(
            "UPDATE addon_graph_collections SET edge_count = edge_count + 1 \
             WHERE org_id = ?1 AND addon_id = ?2 AND collection = ?3",
            rusqlite::params![org_id, addon_id, collection],
        ) {
            let _ = conn.execute("ROLLBACK", []);
            return Err(GraphError::Db(e.to_string()));
        }
        conn.execute("COMMIT", [])
            .map_err(|e| GraphError::Db(e.to_string()))?;
        Ok(())
    }

    /// Releases the reservation of 1 edge (compensation on a Cozo error).
    fn release_edge_quota(&self, org_id: &str, addon_id: &str, collection: &str) {
        if let Ok(conn) = self.pool.write() {
            let _ = conn.execute(
                "UPDATE addon_graph_collections \
                 SET edge_count = MAX(edge_count - 1, 0) \
                 WHERE org_id = ?1 AND addon_id = ?2 AND collection = ?3",
                rusqlite::params![org_id, addon_id, collection],
            );
        }
    }

    fn update_counts_locked(
        &self,
        conn: &rusqlite::Connection,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        node_count: impl Into<Option<u64>>,
        edge_count: impl Into<Option<u64>>,
    ) -> Result<()> {
        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let node_count = node_count.into();
        let edge_count = edge_count.into();
        match (node_count, edge_count) {
            (Some(n), Some(e)) => conn.execute(
                "UPDATE addon_graph_collections SET node_count = ?1, edge_count = ?2, updated_at = ?3 \
                 WHERE org_id = ?4 AND addon_id = ?5 AND collection = ?6",
                rusqlite::params![n as i64, e as i64, now, org_id, addon_id, collection],
            ),
            (Some(n), None) => conn.execute(
                "UPDATE addon_graph_collections SET node_count = ?1, updated_at = ?2 \
                 WHERE org_id = ?3 AND addon_id = ?4 AND collection = ?5",
                rusqlite::params![n as i64, now, org_id, addon_id, collection],
            ),
            (None, Some(e)) => conn.execute(
                "UPDATE addon_graph_collections SET edge_count = ?1, updated_at = ?2 \
                 WHERE org_id = ?3 AND addon_id = ?4 AND collection = ?5",
                rusqlite::params![e as i64, now, org_id, addon_id, collection],
            ),
            (None, None) => return Ok(()),
        }
        .map_err(|e| GraphError::Db(e.to_string()))?;
        Ok(())
    }

    /// Node limit: the `graph_nodes_max` column (>0) wins, otherwise the hard
    /// constant. Takes its own read lock (outside the per-collection write lock).
    fn resolve_node_limit(&self, addon_id: &str) -> u64 {
        let Ok(conn) = self.pool.read() else {
            return MAX_NODES_PER_ADDON;
        };
        let v: i64 = conn
            .query_row(
                "SELECT graph_nodes_max FROM addon_resource_limits WHERE addon_id = ?1",
                rusqlite::params![addon_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if v > 0 {
            v as u64
        } else {
            MAX_NODES_PER_ADDON
        }
    }

    fn resolve_edge_limit(&self, addon_id: &str) -> u64 {
        let Ok(conn) = self.pool.read() else {
            return MAX_EDGES_PER_ADDON;
        };
        let v: i64 = conn
            .query_row(
                "SELECT graph_edges_max FROM addon_resource_limits WHERE addon_id = ?1",
                rusqlite::params![addon_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if v > 0 {
            v as u64
        } else {
            MAX_EDGES_PER_ADDON
        }
    }

    /// Checks the collection limit per (org, addon) before a new one is created.
    /// Atomicity of the check+insert itself is provided by `insert_row`
    /// (PK `(org_id, addon_id, collection)` + a `BEGIN IMMEDIATE` transaction).
    pub fn check_collection_quota(&self, org_id: &str, addon_id: &str) -> Result<()> {
        let conn = self
            .pool
            .read()
            .map_err(|_| GraphError::Db("pool mutex poisoned".into()))?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM addon_graph_collections \
                 WHERE org_id = ?1 AND addon_id = ?2",
                rusqlite::params![org_id, addon_id],
                |r| r.get(0),
            )
            .map_err(|e| GraphError::Db(e.to_string()))?;
        if count as u32 >= MAX_COLLECTIONS_PER_ADDON {
            return Err(GraphError::CollectionQuotaExceeded {
                addon_id: addon_id.to_string(),
                current: count as u32,
                max: MAX_COLLECTIONS_PER_ADDON,
            });
        }
        Ok(())
    }

    /// Deletes a single collection. Protocol (bug H — per-key serialization even on
    /// a cache miss): EVERYTHING happens under the slot write lock of the CANONICAL
    /// entry (we intern it when it is missing — see `seal_key_for_delete`). Under
    /// that lock, in order (files-before-row): close the backend and mark the slot
    /// `Removed`, then delete the `.cozo` FILES, and only at the end
    /// `DELETE FROM addon_graph_collections` plus taking the entry off the map. The
    /// row disappears ONLY after the files are removed successfully, so an I/O error
    /// aborts before the `DELETE` (row + files stay, a retry is possible) and orphan
    /// files without a row can NEVER appear. The `Removed` slot under the lock makes
    /// a parallel `get_or_create` create a FRESH empty collection after a success
    /// (no row) instead of resurrecting the old files, and reopen the same database
    /// from the preserved files after a failure. Deleting the file is atomic with
    /// respect to every other operation on that key, so it never runs in parallel
    /// with `sled::open` (no corruption). Idempotent (no row / no file => OK).
    pub fn delete_collection(&self, org_id: &str, addon_id: &str, collection: &str) -> Result<()> {
        validate_org_id(org_id).map_err(map_vector_err)?;
        validate_addon_id(addon_id).map_err(map_vector_err)?;
        validate_namespace_name(collection).map_err(map_vector_err)?;

        let key = GraphKey {
            org_id: org_id.to_string(),
            addon_id: addon_id.to_string(),
            collection: collection.to_string(),
        };
        self.seal_key_for_delete(&key)
    }

    /// The canonical entry for `key` WITHOUT opening the backend and WITHOUT
    /// eviction (the delete is about to remove it). Interns the entry when it is
    /// missing — that guarantees the delete and a parallel `get_or_create` share
    /// THE SAME `Arc<GraphEntry>` and serialize on its write lock (bug H on a cache
    /// miss).
    fn canonical_entry_for(
        &self,
        key: &GraphKey,
        engine: GraphEngine,
        file_path: PathBuf,
    ) -> Arc<GraphEntry> {
        self.collections
            .entry(key.clone())
            .or_insert_with(|| {
                Arc::new(GraphEntry {
                    slot: RwLock::new(BackendSlot::Closed),
                    engine,
                    file_path,
                    last_used: AtomicU64::new(self.tick()),
                })
            })
            .value()
            .clone()
    }

    /// The full delete protocol under the slot write lock of the canonical entry
    /// (bug H).
    /// The file to delete is resolved AFTER the lock is taken (`seal_key_with_seed`),
    /// so the deleter and a creator racing it always agree on one file: the
    /// registry row when it exists (it knows whether the collection lives in the
    /// addon tree or in a caller directory given to `ensure_collection_at`),
    /// otherwise the path interned in the shared canonical entry — the same path
    /// a creator waiting on this lock will create. A path read before the lock
    /// could name a different file than the one that exists by the time the lock
    /// is held, which is exactly how orphan files without a row appeared.
    ///
    /// ORDER (crash consistency): close the handle -> delete the FILES -> and only
    /// then delete the registry ROW -> take the entry off the map. The row
    /// disappears ONLY after the files are removed successfully, so an I/O error
    /// while deleting ABORTS before the `DELETE` — the row stays, the operation is
    /// retry-able and orphan files without a row can NEVER appear. We set the slot
    /// to `Removed` under the lock, so a parallel `get_or_create` waiting on that
    /// lock re-fetches: after a success (no row) it creates a fresh empty
    /// collection, after a failure (row + files still present) it reopens the same
    /// database — the retry state stays consistent.
    fn seal_key_for_delete(&self, key: &GraphKey) -> Result<()> {
        let seed = self.delete_entry_seed(key)?;
        self.seal_key_with_seed(key, seed)
    }

    /// Seed `(engine, file_path)` used to INTERN the canonical entry when the key
    /// is cold — read BEFORE the slot lock, therefore possibly stale. It never
    /// decides which file is deleted (see `seal_key_with_seed`); it only gives a
    /// fresh entry a starting path, and only when no other thread interned one
    /// first.
    fn delete_entry_seed(&self, key: &GraphKey) -> Result<(GraphEngine, PathBuf)> {
        match self.load_row(&key.org_id, &key.addon_id, &key.collection)? {
            // An unreadable `engine` column must NOT block the delete — the
            // delete is the repair for such a row, and the entry ends up
            // `Removed` and never opens the database anyway.
            Some((engine, path)) => Ok((
                GraphEngine::parse(&engine).unwrap_or_else(GraphEngine::default_for_build),
                path,
            )),
            None => Ok((
                GraphEngine::default_for_build(),
                self.file_path_for(&key.org_id, &key.addon_id, &key.collection)?,
            )),
        }
    }

    /// Locked half of the delete protocol: interns/takes the canonical entry for
    /// `key` (seeding a fresh one with `seed`), then runs the whole protocol under
    /// its slot write lock.
    fn seal_key_with_seed(&self, key: &GraphKey, seed: (GraphEngine, PathBuf)) -> Result<()> {
        let (engine, seed_path) = seed;
        let entry = self.canonical_entry_for(key, engine, seed_path);

        let mut guard = entry
            .slot
            .write()
            .map_err(|_| GraphError::Backend("graph entry lock poisoned".into()))?;

        // 0) File path resolved HERE, under the slot lock — never from the read
        //    that preceded the lock. A creator racing this delete interns the
        //    canonical entry with ITS directory (`ensure_collection_at`) and
        //    inserts the row under this very lock, so a path resolved before the
        //    lock can name a different file than the one that now exists. The row
        //    wins when it exists (it is the source of truth for an existing
        //    collection); with no row the interned entry decides, and that is the
        //    path a creator blocked on this lock will use.
        let path = match self.load_row(&key.org_id, &key.addon_id, &key.collection)? {
            Some((_, path)) => path,
            None => entry.file_path.clone(),
        };

        // 1) Close the backend (decrementing the counter when it was open) and mark
        //    the slot `Removed` — under the lock no other thread touches this database.
        if matches!(&*guard, BackendSlot::Open(_)) {
            self.open_backends.fetch_sub(1, Ordering::AcqRel);
        }
        *guard = BackendSlot::Removed;

        // 2) Delete the FILES. On error -> ABORT before the row `DELETE`: the row
        //    stays, the files stay, the operation is retry-able, no orphan files.
        if let Err(e) = remove_cozo_files(&path) {
            // The entry stays in the map as `Removed`; the next access re-fetches
            // the row (still present) and reopens the database from the same files.
            drop(guard);
            return Err(e);
        }

        // 3) Files deleted -> only now delete the registry ROW.
        {
            let conn = self
                .pool
                .write()
                .map_err(|_| GraphError::Db("pool mutex poisoned".into()))?;
            conn.execute(
                "DELETE FROM addon_graph_collections \
                 WHERE org_id = ?1 AND addon_id = ?2 AND collection = ?3",
                rusqlite::params![key.org_id, key.addon_id, key.collection],
            )
            .map_err(|e| GraphError::Db(e.to_string()))?;
        }
        drop(guard);

        // 4) Take the canonical entry off only when it is STILL the same Arc (a
        //    parallel get_or_create may already have replaced it after our `Removed`).
        self.collections
            .remove_if(key, |_, v| Arc::ptr_eq(v, &entry));

        Ok(())
    }

    /// Deletes ALL graph collections of an addon WITHIN A GIVEN ORGANIZATION: keyed
    /// by `(org_id, addon_id)`, NEVER by `addon_id` alone — another tenant with the
    /// same `addon_id` stays untouched. Close the backends -> DB rows -> files.
    /// Wired into `uninstall` in slice B2.
    pub fn delete_all_for_addon(&self, org_id: &str, addon_id: &str) -> Result<()> {
        validate_org_id(org_id).map_err(map_vector_err)?;
        validate_addon_id(addon_id).map_err(map_vector_err)?;

        let collections: Vec<String> = {
            let conn = self
                .pool
                .read()
                .map_err(|_| GraphError::Db("pool mutex poisoned".into()))?;
            // No registry table == the addon has no graph collections at all ->
            // there is nothing to clean up. This happens on installation paths that
            // never created the graph schema (e.g. the minimal DBs in unit tests).
            // Only `no such table` is treated as an empty list; every other DB error
            // is propagated (we do not mask corruption).
            if !table_exists(&conn, "addon_graph_collections")? {
                return Ok(());
            }
            let mut stmt = conn
                .prepare(
                    "SELECT collection FROM addon_graph_collections \
                     WHERE org_id = ?1 AND addon_id = ?2",
                )
                .map_err(|e| GraphError::Db(e.to_string()))?;
            let mapped = stmt
                .query_map(rusqlite::params![org_id, addon_id], |r| {
                    r.get::<_, String>(0)
                })
                .map_err(|e| GraphError::Db(e.to_string()))?;
            mapped
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| GraphError::Db(e.to_string()))?
        };

        // Every collection is deleted with the same per-key serialized protocol as
        // `delete_collection` (bug H) — the DB row + files under the slot write lock.
        let mut first_err: Option<GraphError> = None;
        for collection in &collections {
            let key = GraphKey {
                org_id: org_id.to_string(),
                addon_id: addon_id.to_string(),
                collection: collection.clone(),
            };
            if let Err(e) = self.seal_key_for_delete(&key) {
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }

        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Drops all open backends of an addon from the cache (across ALL orgs) WITHOUT
    /// deleting on-disk data — the next access rebuilds the backend from a fresh
    /// entry. The slot moves to `Removed` (terminal) under the write lock, so no
    /// stale `Arc` reopens the same database beside the fresh entry (bug G); the
    /// on-disk data stays, so a re-fetch reopens the same file. Wired into
    /// `materialize_addon_derived_state` (slice B2).
    pub fn invalidate_addon(&self, addon_id: &str) {
        let entries: Vec<(GraphKey, Arc<GraphEntry>)> = self
            .collections
            .iter()
            .filter(|e| e.key().addon_id == addon_id)
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect();
        for (key, entry) in entries {
            // `mark_removed` first (slot -> Removed under the write lock), and ONLY
            // THEN take it off the map. The reverse order opened a window for a
            // stale Arc to re-open beside the fresh entry (double-open on the same
            // file). With this order the stale Arc sees Removed and re-fetches.
            self.mark_removed(&entry);
            self.collections
                .remove_if(&key, |_, v| Arc::ptr_eq(v, &entry));
        }
    }

    /// Closes ALL open backends (the addon data-directory migration — sled keeps
    /// files open that must be released before the directory is moved). The same
    /// `mark_removed` -> `remove_if` order as in `invalidate_addon`. On-disk data
    /// survives, but the next access reads the path from the registry row — a
    /// data-directory migration MUST rewrite `file_path` in
    /// `addon_graph_collections` (`storage_admin::run_live_migration` does it).
    pub fn invalidate_all(&self) {
        let entries: Vec<(GraphKey, Arc<GraphEntry>)> = self
            .collections
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect();
        for (key, entry) in entries {
            self.mark_removed(&entry);
            self.collections
                .remove_if(&key, |_, v| Arc::ptr_eq(v, &entry));
        }
    }

    /// Test accessor for the shared pool — lets tests assert on
    /// `addon_graph_collections` rows through the same (in-memory) connection.
    #[cfg(test)]
    pub(crate) fn pool(&self) -> &DbPool {
        &self.pool
    }

    /// Test seam: the half of `delete_collection` that runs BEFORE the slot lock.
    /// Captured on its own, it lets a test replay the create-vs-delete
    /// interleaving deterministically — the seed is taken while the collection
    /// does not exist, a creator then interns the key with its own directory and
    /// inserts the row, and only afterwards does the deleter run its locked half.
    #[cfg(test)]
    pub(crate) fn delete_seed(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
    ) -> Result<(GraphEngine, PathBuf)> {
        self.delete_entry_seed(&self.key_of(org_id, addon_id, collection))
    }

    /// Test seam: the locked half of `delete_collection`, driven with a seed
    /// captured earlier by [`Self::delete_seed`].
    #[cfg(test)]
    pub(crate) fn seal_with_seed(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        seed: (GraphEngine, PathBuf),
    ) -> Result<()> {
        self.seal_key_with_seed(&self.key_of(org_id, addon_id, collection), seed)
    }

    /// Number of currently open backends (slot `Open`). Eviction test: it counts
    /// really live sled backends, not entries with a lazily closed slot.
    #[cfg(test)]
    pub(crate) fn open_handles(&self) -> usize {
        self.collections
            .iter()
            .filter(|e| {
                e.value()
                    .slot
                    .read()
                    .map(|g| matches!(&*g, BackendSlot::Open(_)))
                    .unwrap_or(false)
            })
            .count()
    }

    /// Number of entries in the map (open + lazily closed). The eviction cap works
    /// on that number; open backends are a subset of it.
    #[cfg(test)]
    pub(crate) fn cached_entries(&self) -> usize {
        self.collections.len()
    }

    /// Value of the `open_backends` counter (the account of open sled databases
    /// kept at open/close/evict/delete). Bug G test: the counter must neither
    /// exceed the cap nor diverge from the slot state under load.
    #[cfg(test)]
    pub(crate) fn open_backends_counter(&self) -> u64 {
        self.open_backends.load(Ordering::Acquire)
    }

    /// Reads the collection row: the raw engine name (metadata) + `file_path`
    /// (the source of truth about where the data lives). `None` when there is no
    /// row.
    ///
    /// The engine is left unparsed because the policy belongs to the caller:
    /// paths that open the database need a readable value (`parse_engine`), while
    /// delete must work on a row with a corrupted column too.
    fn load_row(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
    ) -> Result<Option<(String, PathBuf)>> {
        let conn = self
            .pool
            .read()
            .map_err(|_| GraphError::Db("pool mutex poisoned".into()))?;
        let row = conn
            .query_row(
                "SELECT engine, file_path FROM addon_graph_collections \
                 WHERE org_id = ?1 AND addon_id = ?2 AND collection = ?3",
                rusqlite::params![org_id, addon_id, collection],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .ok();
        Ok(row.map(|(engine, file_path)| (engine, PathBuf::from(file_path))))
    }

    /// Engine name from the row -> variant. An error when the column is
    /// unreadable: paths that open the database must not guess the backend.
    fn parse_engine(engine: &str) -> Result<GraphEngine> {
        GraphEngine::parse(engine)
            .ok_or_else(|| GraphError::Db(format!("invalid engine '{engine}' in DB row")))
    }

    /// Inserts the collection row atomically with respect to the quota:
    /// `BEGIN IMMEDIATE` takes the SQLite write lock for the duration (collection
    /// count + INSERT), so two parallel `get_or_create` calls at the
    /// `MAX_COLLECTIONS_PER_ADDON` threshold cannot insert both. A PK conflict (a
    /// race for the same collection) is ruled out too — the second INSERT fails,
    /// the transaction is rolled back, and `entry_get_or_create` loads the existing
    /// row (bug #6).
    fn insert_row(
        &self,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        engine: GraphEngine,
        file_path: &Path,
    ) -> Result<()> {
        let conn = self
            .pool
            .write()
            .map_err(|_| GraphError::Db("pool mutex poisoned".into()))?;
        conn.execute("BEGIN IMMEDIATE", [])
            .map_err(|e| GraphError::Db(e.to_string()))?;

        let max = MAX_COLLECTIONS_PER_ADDON as i64;
        let count: i64 = match conn.query_row(
            "SELECT COUNT(*) FROM addon_graph_collections WHERE org_id = ?1 AND addon_id = ?2",
            rusqlite::params![org_id, addon_id],
            |r| r.get(0),
        ) {
            Ok(c) => c,
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                return Err(GraphError::Db(e.to_string()));
            }
        };
        if count >= max {
            let _ = conn.execute("ROLLBACK", []);
            return Err(GraphError::CollectionQuotaExceeded {
                addon_id: addon_id.to_string(),
                current: count as u32,
                max: MAX_COLLECTIONS_PER_ADDON,
            });
        }

        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        if let Err(e) = conn.execute(
            "INSERT INTO addon_graph_collections \
             (org_id, addon_id, collection, file_path, engine, node_count, edge_count, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 0, 0, ?6, ?6)",
            rusqlite::params![
                org_id,
                addon_id,
                collection,
                file_path.to_string_lossy().to_string(),
                engine.as_str(),
                now,
            ],
        ) {
            let _ = conn.execute("ROLLBACK", []);
            return Err(GraphError::Db(e.to_string()));
        }
        conn.execute("COMMIT", [])
            .map_err(|e| GraphError::Db(e.to_string()))?;
        Ok(())
    }
}

/// Maps a validation error from the vector layer onto
/// `GraphError::InvalidCollectionName`. The name validators are shared
/// (`validate_org_id` was made public), so we translate their
/// `VectorError::InvalidNamespaceName` into the graph equivalent instead of
/// leaking the vector type.
fn map_vector_err(e: crate::services::vector::VectorError) -> GraphError {
    match e {
        crate::services::vector::VectorError::InvalidNamespaceName(name) => {
            GraphError::InvalidCollectionName(name)
        }
        // `validate_custom_dir` rejects a path through `VectorError::Db`; that is
        // not a graph backend failure, so it stays in the `Db` class.
        crate::services::vector::VectorError::Db(msg) => GraphError::Db(msg),
        other => GraphError::Backend(other.to_string()),
    }
}

/// Whether the table exists in the current database. Lets `delete_all_for_addon`
/// tolerate a missing graph registry (installation DBs that never created the
/// graph schema) without masking other DB errors by rigid error-string matching.
fn table_exists(conn: &rusqlite::Connection, table: &str) -> Result<bool> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            rusqlite::params![table],
            |r| r.get(0),
        )
        .map_err(|e| GraphError::Db(e.to_string()))?;
    Ok(count > 0)
}

/// Removes the Cozo collection file together with the SQLite auxiliary files
/// (`-wal`/`-shm`). Idempotent: no file => OK. Tolerates Windows (a file briefly
/// held by a closing handle): a short retry before it returns an I/O error.
fn remove_cozo_files(path: &Path) -> Result<()> {
    let candidates = [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.to_string_lossy())),
        PathBuf::from(format!("{}-shm", path.to_string_lossy())),
    ];
    for p in candidates {
        if !p.exists() {
            continue;
        }
        let mut last_err = None;
        for attempt in 0..5 {
            let res = if p.is_dir() {
                std::fs::remove_dir_all(&p)
            } else {
                std::fs::remove_file(&p)
            };
            match res {
                Ok(()) => {
                    last_err = None;
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    if attempt < 4 {
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                }
            }
        }
        if let Some(e) = last_err {
            // The file may have disappeared between the loops (another thread) —
            // that is fine.
            if p.exists() {
                return Err(GraphError::Io {
                    path: Some(p),
                    source: e,
                });
            }
        }
    }
    Ok(())
}
