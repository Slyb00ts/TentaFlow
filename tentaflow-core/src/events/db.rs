// ===== File: events/db.rs — dedicated SQLite pool + migrations for the event log =====
//
// The timeline lives in `<data>/events.db`, NOT in `tentaflow.db`. The deciding
// argument is not file size: the main database has ONE writer connection and
// every write serialises on it (`db/mod.rs`: "`write()` bierze jedyne
// polaczenie pisarza spod `Mutex`"). An event log is high-frequency, so in the
// main database it would contend for that lock with settings, flows, agents and
// audit writes. The literal precedent is `code_studio/workspace_db.rs` —
// runtime state of a single node, written constantly, outside the Sync Ledger.
//
// ONE file per node, not per session (§2.2): the browser asks across origins,
// runs and actors, and a per-session file would turn every such question into a
// fan-out over hundreds of databases.
//
// `run_events` therefore cannot enter the Sync Ledger (invariant 4). That is a
// property of WHERE it lives, not a rule someone has to remember: the ledger
// only ever reads tables listed in `sync::core_registry::CORE_SYNC_DESCRIPTORS`
// and it reads every one of them from the MAIN pool, so a table in a different
// file with no descriptor is unreachable to it. `sync_run_events_stays_out_of_
// the_ledger` in `store.rs` pins both halves.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use anyhow::Result;
use rusqlite::Connection;
use tracing::info;

use crate::db::DbPool;

/// Global handle to the event-log pool, set once in `init`. Mirrors
/// `project_studio::db` so the writer and the background loops reach the
/// connection without threading a pool through every progress event.
static EVENTS_POOL: OnceLock<DbPool> = OnceLock::new();

/// Returns the event-log pool, or an error if `init` has not run. An error
/// rather than a panic: a missing timeline must never take down the request
/// that was being recorded.
pub fn pool() -> Result<DbPool> {
    EVENTS_POOL
        .get()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("event log database not initialised"))
}

/// Forces a WAL checkpoint (TRUNCATE) so a shutdown does not leave an
/// unflushed `-wal` file. No-op when the pool was never opened, so it is safe
/// to call unconditionally at shutdown.
pub fn checkpoint_wal() -> Result<()> {
    let Some(pool) = EVENTS_POOL.get() else {
        return Ok(());
    };
    let conn = pool
        .write()
        .map_err(|e| anyhow::anyhow!("events pool write: {e}"))?;
    conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")?;
    info!("event log WAL checkpoint done");
    Ok(())
}

/// Opens (creating if absent) the event-log database, applies the pragmas,
/// runs the module migrations and publishes the pool. Idempotent: a second
/// call leaves the original pool in place and returns it.
pub fn init(db_path: &Path) -> Result<DbPool> {
    if let Some(existing) = EVENTS_POOL.get() {
        return Ok(existing.clone());
    }

    info!("opening the event log database: {:?}", db_path);
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let conn = Connection::open(db_path)?;
    apply_pragmas(&conn)?;
    run_migrations(&conn)?;

    let pool = Arc::new(crate::db::Db::from_connection(conn));
    let _ = EVENTS_POOL.set(pool.clone());
    info!("event log database ready");
    Ok(pool)
}

/// `auto_vacuum` is set FIRST and in its own statement on purpose: SQLite only
/// honours a change of it on a database that still has no tables (otherwise it
/// takes a full `VACUUM`), so batching it after `journal_mode` — or after the
/// migrations — would silently leave the file on `NONE`. Incremental rather
/// than full: retention deletes in bulk and the freed pages are reclaimed by
/// the sweeper's `incremental_vacuum` instead of by a stop-the-world rewrite.
fn apply_pragmas(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA auto_vacuum=INCREMENTAL;")?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;\
         PRAGMA foreign_keys=ON;\
         PRAGMA synchronous=NORMAL;\
         PRAGMA cache_size=-65536;\
         PRAGMA mmap_size=268435456;\
         PRAGMA temp_store=MEMORY;\
         PRAGMA busy_timeout=5000;\
         PRAGMA wal_autocheckpoint=2000;",
    )?;
    Ok(())
}

/// Opens an event-log database at `db_path` WITHOUT publishing it as the
/// process-wide pool. Every test gets its own file this way, and the writer
/// stays a function of the pool it is handed rather than of global state.
pub fn open_pool_at(db_path: &Path) -> Result<DbPool> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(db_path)?;
    apply_pragmas(&conn)?;
    run_migrations(&conn)?;
    Ok(Arc::new(crate::db::Db::from_connection(conn)))
}

/// Versioned migration runner for the event log. Tracks applied versions in
/// `events_schema_version` and applies each pending `(version, sql)` step in
/// its own transaction.
fn run_migrations(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS events_schema_version (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )?;

    let current: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM events_schema_version",
        [],
        |row| row.get(0),
    )?;

    for (version, sql) in MIGRATIONS {
        if *version > current {
            info!("event log migration {}", version);
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(sql)?;
            tx.execute(
                "INSERT INTO events_schema_version (version) VALUES (?1)",
                rusqlite::params![version],
            )?;
            tx.commit()?;
        }
    }
    Ok(())
}

const MIGRATIONS: &[(i64, &str)] = &[(1, INITIAL_SCHEMA)];

/// §2.3, verbatim, plus the audit outbox.
///
/// `PRIMARY KEY (run_id, seq)` is load-bearing and not decoration: `seq` is
/// allocated as `MAX(seq) + 1` inside the insert transaction, so a second
/// concurrent writer on the same run collides on the key and fails LOUDLY
/// instead of producing a silently interleaved timeline (invariant 2).
///
/// The outbox deliberately carries NO foreign key to `run_events`. Its row is
/// self-contained — everything `audit_log` needs is in `payload_json` — so the
/// timeline's retention sweep can delete an event without touching an audit
/// obligation that has not been delivered yet. A cascade would let a
/// diagnostic-grade retention term destroy a compliance-grade record; a
/// `RESTRICT` would let one stuck row block the whole sweep. §2.8: the
/// asymmetry is the point — the audit trail may not lose an entry, the
/// timeline may lose its tail.
const INITIAL_SCHEMA: &str = "
CREATE TABLE run_events (
  run_id           TEXT    NOT NULL,
  seq              INTEGER NOT NULL,
  at_ms            INTEGER NOT NULL,
  kind             TEXT    NOT NULL,
  origin           TEXT    NOT NULL,
  actor_kind       TEXT    NOT NULL,
  actor_id         TEXT,
  actor_user_id    TEXT,
  correlation_id   TEXT,
  session_id       TEXT,
  node_id          TEXT,
  call_id          TEXT,
  payload_json     TEXT    NOT NULL,
  idempotency_key  TEXT,
  PRIMARY KEY (run_id, seq)
);
CREATE INDEX ix_run_events_time    ON run_events(at_ms);
CREATE INDEX ix_run_events_origin  ON run_events(origin, at_ms);
CREATE INDEX ix_run_events_actor   ON run_events(actor_id, at_ms);
CREATE INDEX ix_run_events_corr    ON run_events(correlation_id);
CREATE UNIQUE INDEX ux_run_events_idem ON run_events(run_id, idempotency_key)
  WHERE idempotency_key IS NOT NULL;

CREATE TABLE audit_outbox (
  id               INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id           TEXT    NOT NULL,
  seq              INTEGER NOT NULL,
  payload_json     TEXT    NOT NULL,
  created_at       TEXT    NOT NULL,
  attempts         INTEGER NOT NULL DEFAULT 0,
  last_error       TEXT,
  next_attempt_at  TEXT,
  delivered_at     TEXT
);
CREATE INDEX ix_events_audit_outbox_due ON audit_outbox(delivered_at, next_attempt_at, id);
";

#[cfg(test)]
mod tests {
    use crate::events::test_support::events_db;
    use std::collections::BTreeSet;

    /// §2.3 is the contract another track writes SQL against, so it is asserted
    /// against `sqlite_master` — what actually reached the file — and not
    /// against the constant a few lines above.
    #[test]
    fn schema_matches_the_specification() {
        let (_dir, pool) = events_db();
        let conn = pool.read().unwrap();

        let columns: Vec<(String, String, i64)> = {
            let mut stmt = conn.prepare("PRAGMA table_info(run_events)").unwrap();
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })
                .unwrap();
            rows.collect::<rusqlite::Result<Vec<_>>>().unwrap()
        };
        let expected: &[(&str, &str, bool)] = &[
            ("run_id", "TEXT", true),
            ("seq", "INTEGER", true),
            ("at_ms", "INTEGER", true),
            ("kind", "TEXT", true),
            ("origin", "TEXT", true),
            ("actor_kind", "TEXT", true),
            ("actor_id", "TEXT", false),
            ("actor_user_id", "TEXT", false),
            ("correlation_id", "TEXT", false),
            ("session_id", "TEXT", false),
            ("node_id", "TEXT", false),
            ("call_id", "TEXT", false),
            ("payload_json", "TEXT", true),
            ("idempotency_key", "TEXT", false),
        ];
        assert_eq!(
            columns.len(),
            expected.len(),
            "run_events has {} columns, §2.3 lists {}: {columns:?}",
            columns.len(),
            expected.len()
        );
        for (actual, (name, ty, not_null)) in columns.iter().zip(expected) {
            assert_eq!(&actual.0, name, "column order changed: {columns:?}");
            assert_eq!(&actual.1, ty, "column {name} has the wrong type");
            assert_eq!(
                actual.2 != 0,
                *not_null,
                "column {name} has the wrong nullability"
            );
        }

        // PRIMARY KEY (run_id, seq) — the constraint that turns a second
        // concurrent writer into a loud error.
        let pk: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT name FROM pragma_table_info('run_events') WHERE pk > 0 ORDER BY pk")
                .unwrap();
            stmt.query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert_eq!(pk, vec!["run_id".to_string(), "seq".to_string()]);

        let indexes: Vec<(String, Option<String>)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT name, sql FROM sqlite_master \
                     WHERE type = 'index' AND tbl_name = 'run_events'",
                )
                .unwrap();
            stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
        };
        let names: BTreeSet<&str> = indexes.iter().map(|(name, _)| name.as_str()).collect();
        for expected in [
            "ix_run_events_time",
            "ix_run_events_origin",
            "ix_run_events_actor",
            "ix_run_events_corr",
            "ux_run_events_idem",
        ] {
            assert!(names.contains(expected), "missing index {expected}: {names:?}");
        }

        let idem_sql = indexes
            .iter()
            .find(|(name, _)| name == "ux_run_events_idem")
            .and_then(|(_, sql)| sql.clone())
            .expect("ux_run_events_idem has no SQL");
        let normalized = idem_sql.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            normalized.contains("UNIQUE"),
            "the idempotency index is not unique: {normalized}"
        );
        assert!(
            normalized.contains("(run_id, idempotency_key)"),
            "the idempotency index covers the wrong columns: {normalized}"
        );
        // PARTIAL, not plain unique: without the WHERE clause every event that
        // opts out of deduplication would collide on NULL in SQLite's
        // stricter dialects and, more to the point, the index would carry a
        // row for each of them.
        assert!(
            normalized.contains("WHERE idempotency_key IS NOT NULL"),
            "the idempotency index is not partial: {normalized}"
        );

        let auto_vacuum: i64 = conn
            .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            auto_vacuum, 2,
            "auto_vacuum is {auto_vacuum}, expected 2 (INCREMENTAL) — retention \
             deletes in bulk and would otherwise never return a page"
        );
    }

    /// Reopening must not re-run a migration or lose the pragma.
    #[test]
    fn opening_an_existing_log_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let first = super::open_pool_at(&dir.path().join("events.db")).unwrap();
        drop(first);
        let second = super::open_pool_at(&dir.path().join("events.db")).unwrap();
        let conn = second.read().unwrap();
        let versions: i64 = conn
            .query_row("SELECT COUNT(*) FROM events_schema_version", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(versions, 1, "a migration was applied twice");
        let auto_vacuum: i64 = conn
            .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
            .unwrap();
        assert_eq!(auto_vacuum, 2);
    }
}
