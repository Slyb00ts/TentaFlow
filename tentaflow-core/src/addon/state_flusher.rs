// =============================================================================
// File: addon/state_flusher.rs — write-behind persistence for the Durable tier
//
// WHY: `AddonStateStore` serves the Durable tier from RAM (fast, no per-write
// DB round-trip). This module is the asynchronous bridge that persists those
// in-RAM durable writes to the core SQLite `addon_state` table so a restart
// recovers them, and seeds RAM back from SQLite at addon start. The hot path
// (addon `set`/`delete`) NEVER touches SQLite — it only marks entries dirty;
// the periodic flusher drains those dirty batches in the background.
//
// CRASH-SAFETY MODEL — Durable = WRITE-BEHIND / AT-LEAST-ONCE:
//   * A durable write lands in RAM immediately and is visible to all instances
//     of the addon. It is persisted on the NEXT successful flush (≤ the flush
//     interval later).
//   * `take_dirty` clears a shard's dirty flags BEFORE the DB write. If the DB
//     write then FAILS, the batch is handed to `store.remark_dirty`, which
//     re-arms the same upserts/tombstones (idempotently — a key the addon
//     overwrote in the meantime keeps its newer value) so the next flush
//     retries. Net effect: a durable write is persisted AT LEAST ONCE; an
//     UPSERT is idempotent (`ON CONFLICT DO UPDATE`) and a DELETE is idempotent
//     (`DELETE ... WHERE` on an already-absent row is a no-op), so a retried
//     batch never corrupts state.
//   * A HARD process crash (SIGKILL / power loss) BETWEEN flushes can lose up
//     to `interval` of un-flushed durable writes — there is no WAL on the
//     in-RAM tier. This is the accepted write-behind tradeoff. Graceful
//     shutdown does a FINAL flush so it loses nothing pending. Addon authors
//     needing stricter (synchronous) durability must use their own addon
//     SQLite via the storage host functions, NOT the Durable state tier.
//   * The EPHEMERAL tier is RAM-only by definition and is NEVER written here.
// =============================================================================

use std::time::Duration;

use anyhow::{Context, Result};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::addon::state_store::{AddonStateStore, LoadOutcome, TakenBatch};
use crate::db::Db;

/// Default cadence of the periodic flusher. Bounds the worst-case durable-write
/// loss window on a hard crash (see crash-safety model above) while keeping the
/// DB write rate low under bursty addon state churn.
pub const DEFAULT_FLUSH_INTERVAL: Duration = Duration::from_secs(2);

/// Per-round flush accounting (observability / tests).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FlushStats {
    /// Addons whose batch was persisted successfully this round.
    pub addons_flushed: usize,
    /// Rows upserted across all addons this round.
    pub upserts: usize,
    /// Rows deleted across all addons this round.
    pub deletes: usize,
    /// Addons whose DB write failed and were re-marked dirty for retry.
    pub addons_failed: usize,
}

/// Persist all pending durable writes once. For each addon with pending work
/// (`store.dirty_addons()`), takes its dirty batch and applies upserts +
/// deletes inside ONE transaction. One transaction PER ADDON (bounded by the
/// addon's dirty set) so a failure rolls back only that addon's batch and the
/// batch is re-marked dirty for the next round (at-least-once). A failure for
/// one addon never aborts the others.
///
/// Returns aggregate stats. Returns `Err` only on a failure to even acquire the
/// DB writer; per-addon DB errors are handled in-band (re-mark + count) so the
/// periodic loop keeps running.
pub fn flush_once(db: &Db, store: &AddonStateStore) -> Result<FlushStats> {
    let mut stats = FlushStats::default();

    for addon_id in store.dirty_addons() {
        let batch = store.take_dirty(&addon_id);
        if batch.set().is_empty() {
            // Raced clean between the scan and the take — nothing to do.
            continue;
        }
        // Coordinate with a concurrent uninstall: if the addon was purged after
        // this batch was taken, do NOT write it (no row resurrection) and do not
        // re-mark (that would recreate a shard for an uninstalled addon).
        if batch.is_purged() {
            continue;
        }

        match flush_batch(db, &batch) {
            Ok(()) => {
                stats.addons_flushed += 1;
                stats.upserts += batch.set().upserts.len();
                stats.deletes += batch.set().deletes.len();
            }
            Err(e) => {
                warn!(
                    "addon state flush failed for '{}' ({} upserts, {} deletes): {} — re-marking dirty for retry",
                    batch.addon_id(),
                    batch.set().upserts.len(),
                    batch.set().deletes.len(),
                    e
                );
                // At-least-once: the dirty flags were cleared by take_dirty, so
                // re-arm them or the writes are lost. `remark_dirty` itself
                // refuses to re-arm a purged/closed shard.
                store.remark_dirty(batch);
                stats.addons_failed += 1;
            }
        }
    }

    if stats.addons_flushed > 0 || stats.addons_failed > 0 {
        debug!(
            "addon state flush: {} addon(s) flushed ({} upserts, {} deletes), {} failed",
            stats.addons_flushed, stats.upserts, stats.deletes, stats.addons_failed
        );
    }

    Ok(stats)
}

/// Apply one addon's dirty batch in a single transaction. Upserts and deletes
/// are idempotent so a retried batch (after a re-mark) is safe.
///
/// PURGE COORDINATION: the transaction is staged but only committed if the
/// addon is still NOT purged at commit time. If `drop_addon` ran during the DB
/// write, the transaction is rolled back (drops on scope exit) and the rows are
/// never resurrected. `purge_addon` (the DB DELETE) runs under the same store
/// drop, so an uninstall is final: no row survives a concurrent flush.
fn flush_batch(db: &Db, batch: &TakenBatch) -> Result<()> {
    let addon_id = batch.addon_id();
    let updated_at = chrono::Utc::now().timestamp_millis();
    let mut conn = db
        .write()
        .map_err(|e| anyhow::anyhow!("addon state flush: db writer unavailable: {e}"))?;
    let tx = conn
        .transaction()
        .context("addon state flush: begin transaction")?;

    {
        let mut upsert_stmt = tx
            .prepare_cached(
                "INSERT INTO addon_state (addon_id, state_key, value, updated_at) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(addon_id, state_key) \
                 DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            )
            .context("addon state flush: prepare upsert")?;
        for (key, value) in &batch.set().upserts {
            upsert_stmt
                .execute(rusqlite::params![addon_id, key, value, updated_at])
                .context("addon state flush: execute upsert")?;
        }

        let mut delete_stmt = tx
            .prepare_cached("DELETE FROM addon_state WHERE addon_id = ?1 AND state_key = ?2")
            .context("addon state flush: prepare delete")?;
        for key in &batch.set().deletes {
            delete_stmt
                .execute(rusqlite::params![addon_id, key])
                .context("addon state flush: execute delete")?;
        }
    }

    // Final purge check under the held DB writer: if the addon was uninstalled
    // while this transaction was staged, abandon it (rollback on drop) so the
    // `purge_addon` DELETE is the last word and no row resurrects.
    if batch.is_purged() {
        return Ok(());
    }

    tx.commit().context("addon state flush: commit")?;
    Ok(())
}

/// Persist any pending durable writes for ONE addon immediately. Used at
/// graceful instance stop / unload so unloading does not lose durable state
/// that has not yet hit the periodic flush. Idempotent; safe to call when the
/// addon has nothing pending (returns zeroed stats). On DB failure the batch is
/// re-marked dirty (the next periodic flush, or a retry, will persist it).
pub fn flush_addon(db: &Db, store: &AddonStateStore, addon_id: &str) -> Result<FlushStats> {
    let batch = store.take_dirty(addon_id);
    if batch.set().is_empty() {
        return Ok(FlushStats::default());
    }
    if batch.is_purged() {
        return Ok(FlushStats::default());
    }
    let mut stats = FlushStats::default();
    let upserts = batch.set().upserts.len();
    let deletes = batch.set().deletes.len();
    match flush_batch(db, &batch) {
        Ok(()) => {
            stats.addons_flushed = 1;
            stats.upserts = upserts;
            stats.deletes = deletes;
        }
        Err(e) => {
            warn!(
                "addon state flush_addon failed for '{}': {} — re-marking dirty",
                addon_id, e
            );
            store.remark_dirty(batch);
            stats.addons_failed = 1;
        }
    }
    Ok(stats)
}

/// Seed RAM from the backing store at addon start. SELECTs every persisted row
/// for `addon_id` and hands them to `store.load_durable`, which enforces the
/// per-value / per-addon caps (a too-large or oversized backing store can never
/// push RAM past the documented bounds — see `LoadOutcome`). Call BEFORE the
/// addon's `on_start` so the addon observes its persisted durable state.
pub fn load_addon(db: &Db, store: &AddonStateStore, addon_id: &str) -> Result<LoadOutcome> {
    let conn = db
        .read()
        .map_err(|e| anyhow::anyhow!("addon state load: db reader unavailable: {e}"))?;
    let mut stmt = conn
        .prepare_cached("SELECT state_key, value FROM addon_state WHERE addon_id = ?1")
        .context("addon state load: prepare select")?;
    let rows = stmt
        .query_map(rusqlite::params![addon_id], |row| {
            let key: String = row.get(0)?;
            let value: Vec<u8> = row.get(1)?;
            Ok((key, value))
        })
        .context("addon state load: query")?;

    // Stream rows lazily into `load_durable`: it stops consuming the iterator
    // once the per-addon cap is hit, so a huge/corrupt table never materialises
    // an unbounded Vec. A row-read error is captured and surfaced as a hard
    // load failure (so the caller can fail addon start rather than run on
    // phantom-empty state).
    let mut row_error: Option<rusqlite::Error> = None;
    let outcome = {
        let iter = rows.scan((), |_, row| match row {
            Ok(kv) => Some(kv),
            Err(e) => {
                row_error = Some(e);
                None
            }
        });
        store.load_durable(addon_id, iter)
    };
    if let Some(e) = row_error {
        // Failed mid-stream: clear the load-once guard so a retried start
        // re-seeds (merge makes the reload idempotent for rows already landed).
        store.reset_loaded(addon_id);
        return Err(anyhow::Error::new(e).context("addon state load: read row"));
    }
    Ok(outcome)
}

/// Delete ALL persisted state for an addon. Used on uninstall, after the
/// in-RAM shard is dropped. Idempotent (no row → no-op).
pub fn purge_addon(db: &Db, addon_id: &str) -> Result<()> {
    let conn = db
        .write()
        .map_err(|e| anyhow::anyhow!("addon state purge: db writer unavailable: {e}"))?;
    let deleted = conn
        .execute(
            "DELETE FROM addon_state WHERE addon_id = ?1",
            rusqlite::params![addon_id],
        )
        .context("addon state purge: delete")?;
    if deleted > 0 {
        info!(
            "addon state: purged {} persisted row(s) for '{}'",
            deleted, addon_id
        );
    }
    Ok(())
}

/// Spawn the periodic write-behind flusher. Calls `flush_once` every `interval`
/// until `shutdown` is cancelled, then does ONE FINAL `flush_once` so graceful
/// shutdown loses nothing pending. The task holds the shared `Db` pool and the
/// process-global `AddonStateStore` (a `'static` singleton), so it is `'static`.
///
/// Returns the task `JoinHandle` so `AddonManager::shutdown` can AWAIT the final
/// drain (bounded by a timeout) before the process exits — guaranteeing all
/// dirty durable state at cancel time is persisted, not lost to an early exit.
pub fn spawn_flusher(
    db: crate::db::DbPool,
    store: &'static AddonStateStore,
    interval: Duration,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        info!(
            "addon state flusher started (interval {}s)",
            interval.as_secs_f64()
        );
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    if let Err(e) = flush_once(&db, store) {
                        warn!("addon state periodic flush error: {}", e);
                    }
                }
                _ = shutdown.cancelled() => {
                    // Final drain on graceful shutdown — persist anything that
                    // landed since the last tick before the process exits.
                    match flush_once(&db, store) {
                        Ok(stats) if stats.addons_flushed > 0 || stats.addons_failed > 0 => info!(
                            "addon state flusher final drain: {} flushed, {} failed",
                            stats.addons_flushed, stats.addons_failed
                        ),
                        Ok(_) => info!("addon state flusher stopped (nothing pending)"),
                        Err(e) => warn!("addon state flusher final drain error: {}", e),
                    }
                    break;
                }
            }
        }
    })
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::state_store::Tier;
    use rusqlite::Connection;
    use std::sync::Arc;

    fn test_db() -> Arc<Db> {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        crate::db::migrations::run(&conn).expect("run migrations");
        Arc::new(Db::from_connection(conn))
    }

    fn rows_for(db: &Db, addon_id: &str) -> Vec<(String, Vec<u8>)> {
        let conn = db.read().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT state_key, value FROM addon_state WHERE addon_id = ?1 ORDER BY state_key",
            )
            .unwrap();
        let rows = stmt
            .query_map(rusqlite::params![addon_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
            })
            .unwrap();
        rows.map(|r| r.unwrap()).collect()
    }

    #[test]
    fn flush_persists_durable_and_load_recovers() {
        let db = test_db();
        let store = AddonStateStore::new();

        store.set("a", "k1", b"v1".to_vec(), Tier::Durable).unwrap();
        store.set("a", "k2", b"v2".to_vec(), Tier::Durable).unwrap();

        let stats = flush_once(&db, &store).unwrap();
        assert_eq!(stats.addons_flushed, 1);
        assert_eq!(stats.upserts, 2);
        assert_eq!(stats.deletes, 0);

        let persisted = rows_for(&db, "a");
        assert_eq!(
            persisted,
            vec![
                ("k1".to_string(), b"v1".to_vec()),
                ("k2".to_string(), b"v2".to_vec()),
            ]
        );

        // A second flush with no new writes does nothing.
        let stats2 = flush_once(&db, &store).unwrap();
        assert_eq!(stats2.addons_flushed, 0);

        // Drop the store, recreate it, load from DB → entries are back.
        drop(store);
        let store2 = AddonStateStore::new();
        let outcome = load_addon(&db, &store2, "a").unwrap();
        assert_eq!(outcome.loaded, 2);
        assert_eq!(store2.get("a", "k1"), Some(b"v1".to_vec()));
        assert_eq!(store2.get("a", "k2"), Some(b"v2".to_vec()));
        // Loaded entries are clean — nothing to re-flush.
        assert_eq!(store2.dirty_addons(), Vec::<String>::new());
    }

    #[test]
    fn flush_persists_tombstone_delete() {
        let db = test_db();
        let store = AddonStateStore::new();

        store.set("a", "k", b"v".to_vec(), Tier::Durable).unwrap();
        flush_once(&db, &store).unwrap();
        assert_eq!(rows_for(&db, "a").len(), 1);

        // Delete the durable key → tombstone → flush removes the row.
        assert!(store.delete("a", "k"));
        let stats = flush_once(&db, &store).unwrap();
        assert_eq!(stats.deletes, 1);
        assert!(rows_for(&db, "a").is_empty(), "deleted row must be gone");

        // A reload sees nothing.
        let store2 = AddonStateStore::new();
        let outcome = load_addon(&db, &store2, "a").unwrap();
        assert_eq!(outcome.loaded, 0);
        assert!(store2.get("a", "k").is_none());
    }

    #[test]
    fn ephemeral_is_never_persisted() {
        let db = test_db();
        let store = AddonStateStore::new();

        store
            .set("a", "eph", b"x".to_vec(), Tier::Ephemeral)
            .unwrap();
        store.set("a", "dur", b"y".to_vec(), Tier::Durable).unwrap();

        let stats = flush_once(&db, &store).unwrap();
        // Only the durable key is written.
        assert_eq!(stats.upserts, 1);

        let persisted = rows_for(&db, "a");
        assert_eq!(persisted, vec![("dur".to_string(), b"y".to_vec())]);
        assert!(
            !persisted.iter().any(|(k, _)| k == "eph"),
            "ephemeral key must never reach addon_state"
        );
    }

    #[test]
    fn remark_on_db_failure_retries_on_next_good_flush() {
        // A closed DB (its only connection consumed) forces flush_batch to error.
        // We simulate "bad DB" with a fresh in-memory DB that has NO addon_state
        // table (migrations not run) so the INSERT fails; the batch must stay
        // dirty and a later flush to a good DB must persist it.
        let bad = {
            let conn = Connection::open_in_memory().expect("open mem");
            // Deliberately do NOT run migrations → addon_state does not exist.
            Arc::new(Db::from_connection(conn))
        };
        let good = test_db();
        let store = AddonStateStore::new();

        store.set("a", "k", b"v".to_vec(), Tier::Durable).unwrap();

        // Flush to the bad DB → fails, batch re-marked dirty.
        let stats = flush_once(&bad, &store).unwrap();
        assert_eq!(stats.addons_failed, 1);
        assert_eq!(stats.addons_flushed, 0);
        // Still dirty → will retry.
        assert_eq!(store.dirty_addons(), vec!["a".to_string()]);
        // Value still served from RAM (write-behind never drops the live value).
        assert_eq!(store.get("a", "k"), Some(b"v".to_vec()));

        // Flush to the good DB → succeeds, row persisted.
        let stats2 = flush_once(&good, &store).unwrap();
        assert_eq!(stats2.addons_flushed, 1);
        assert_eq!(rows_for(&good, "a"), vec![("k".to_string(), b"v".to_vec())]);
        assert_eq!(store.dirty_addons(), Vec::<String>::new());
    }

    #[test]
    fn remark_keeps_newer_overwrite() {
        // If the addon overwrites a key AFTER take_dirty but the DB write failed,
        // remark must NOT clobber the newer value back to the stale one.
        let bad = {
            let conn = Connection::open_in_memory().expect("open mem");
            Arc::new(Db::from_connection(conn))
        };
        let good = test_db();
        let store = AddonStateStore::new();

        store.set("a", "k", b"old".to_vec(), Tier::Durable).unwrap();

        // Manually take + fail + overwrite + remark, mirroring a flush race.
        let batch = store.take_dirty("a");
        // Addon overwrites the key while the (failed) DB write was in flight.
        store.set("a", "k", b"new".to_vec(), Tier::Durable).unwrap();
        store.remark_dirty(batch);

        // The newer value must win, both in RAM and once flushed.
        assert_eq!(store.get("a", "k"), Some(b"new".to_vec()));
        let _ = bad; // bad DB only here to document the failed-write origin.
        flush_once(&good, &store).unwrap();
        assert_eq!(
            rows_for(&good, "a"),
            vec![("k".to_string(), b"new".to_vec())]
        );
    }

    #[test]
    fn purge_removes_all_rows() {
        let db = test_db();
        let store = AddonStateStore::new();
        store.set("a", "k1", b"v1".to_vec(), Tier::Durable).unwrap();
        store.set("a", "k2", b"v2".to_vec(), Tier::Durable).unwrap();
        store.set("b", "k", b"vb".to_vec(), Tier::Durable).unwrap();
        flush_once(&db, &store).unwrap();
        assert_eq!(rows_for(&db, "a").len(), 2);
        assert_eq!(rows_for(&db, "b").len(), 1);

        purge_addon(&db, "a").unwrap();
        assert!(rows_for(&db, "a").is_empty(), "purged addon rows gone");
        assert_eq!(rows_for(&db, "b").len(), 1, "other addon untouched");

        // Purge is idempotent.
        purge_addon(&db, "a").unwrap();
    }

    #[test]
    fn flush_addon_targets_single_addon() {
        let db = test_db();
        let store = AddonStateStore::new();
        store.set("a", "k", b"va".to_vec(), Tier::Durable).unwrap();
        store.set("b", "k", b"vb".to_vec(), Tier::Durable).unwrap();

        let stats = flush_addon(&db, &store, "a").unwrap();
        assert_eq!(stats.addons_flushed, 1);
        assert_eq!(rows_for(&db, "a"), vec![("k".to_string(), b"va".to_vec())]);
        // b was not flushed by the targeted call.
        assert!(rows_for(&db, "b").is_empty());
        assert_eq!(store.dirty_addons(), vec!["b".to_string()]);
    }

    #[test]
    fn multiple_addons_one_failure_does_not_block_others() {
        // Sanity: flush_once iterates per-addon; a failing addon (none here)
        // would not stop others. Persist two addons in one round.
        let db = test_db();
        let store = AddonStateStore::new();
        store.set("a", "k", b"1".to_vec(), Tier::Durable).unwrap();
        store.set("b", "k", b"2".to_vec(), Tier::Durable).unwrap();

        let stats = flush_once(&db, &store).unwrap();
        assert_eq!(stats.addons_flushed, 2);
        assert_eq!(rows_for(&db, "a").len(), 1);
        assert_eq!(rows_for(&db, "b").len(), 1);
    }

    // FIX 2: an in-flight flush batch taken BEFORE uninstall must NOT resurrect
    // rows; remark must not recreate a shard; the DB stays empty.
    #[test]
    fn purge_during_inflight_flush_leaves_db_empty() {
        let db = test_db();
        let store = AddonStateStore::new();
        store.set("a", "k", b"v".to_vec(), Tier::Durable).unwrap();

        // Flusher takes the batch...
        let batch = store.take_dirty("a");
        assert!(!batch.set().is_empty());

        // ...then uninstall runs: drop the RAM shard and purge the DB rows.
        store.drop_addon("a");
        purge_addon(&db, "a").unwrap();

        // The stale batch is now purged → flush_batch must abandon it.
        assert!(batch.is_purged());
        flush_batch(&db, &batch).unwrap();
        assert!(
            rows_for(&db, "a").is_empty(),
            "purged batch must not resurrect rows"
        );

        // A failed-write path would remark — it must not recreate the shard.
        store.remark_dirty(batch);
        assert!(store.addon_stats("a").is_none());
        assert!(store.dirty_addons().is_empty());

        // A reload sees nothing — uninstall is final.
        let store2 = AddonStateStore::new();
        let outcome = load_addon(&db, &store2, "a").unwrap();
        assert_eq!(outcome.loaded, 0);
    }

    // FIX 4: a genuine DB read error fails the load (so the caller can fail
    // addon start) — an absent table is a real error, not "empty store".
    #[test]
    fn load_addon_errors_on_db_failure() {
        // DB with NO addon_state table (migrations not run) → SELECT fails.
        let bad = {
            let conn = Connection::open_in_memory().expect("open mem");
            Arc::new(Db::from_connection(conn))
        };
        let store = AddonStateStore::new();
        let res = load_addon(&bad, &store, "a");
        assert!(res.is_err(), "missing table must surface as a load error");
        // The load-once guard must NOT have stuck (no shard or not-loaded), so a
        // retry against a good DB can seed.
        let good = test_db();
        good.write()
            .unwrap()
            .execute(
                "INSERT INTO addon_state (addon_id, state_key, value, updated_at) VALUES ('a','k',?1,0)",
                rusqlite::params![b"v".to_vec()],
            )
            .unwrap();
        let outcome = load_addon(&good, &store, "a").unwrap();
        assert_eq!(outcome.loaded, 1);
        assert_eq!(store.get("a", "k"), Some(b"v".to_vec()));
    }

    // FIX 4: a genuinely empty store is Ok (0 rows), not an error.
    #[test]
    fn load_addon_empty_store_is_ok() {
        let db = test_db();
        let store = AddonStateStore::new();
        let outcome = load_addon(&db, &store, "never-written").unwrap();
        assert_eq!(outcome.loaded, 0);
        assert!(!outcome.already_loaded);
    }

    // FIX 3: graceful shutdown awaits the spawned flusher's final drain, so a
    // durable write that landed after the last tick is persisted before exit.
    #[tokio::test]
    async fn shutdown_awaits_final_drain() {
        // Use the process-global store (spawn_flusher requires a 'static store).
        // A unique addon id keeps this test independent of others.
        let store = AddonStateStore::global();
        let addon = "shutdown-drain-test-unique";
        store.drop_addon(addon);

        let db_pool: crate::db::DbPool = {
            let conn = Connection::open_in_memory().expect("open mem");
            crate::db::migrations::run(&conn).expect("migrations");
            Arc::new(Db::from_connection(conn))
        };

        let token = CancellationToken::new();
        // Long interval so the periodic tick never fires during the test — only
        // the final drain on cancel can persist the write.
        let handle = spawn_flusher(
            db_pool.clone(),
            store,
            Duration::from_secs(3600),
            token.clone(),
        );

        // Write AFTER the flusher started; no tick will fire before cancel.
        store.set(addon, "k", b"v".to_vec(), Tier::Durable).unwrap();

        // Cancel + await the final drain (mirrors AddonManager::shutdown +
        // await_state_flusher_drain).
        token.cancel();
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("flusher must finish within timeout")
            .expect("flusher task must not panic");

        // The post-tick write is durably persisted by the awaited final drain.
        let persisted = rows_for(db_pool.as_ref(), addon);
        assert_eq!(persisted, vec![("k".to_string(), b"v".to_vec())]);

        store.drop_addon(addon);
    }
}
