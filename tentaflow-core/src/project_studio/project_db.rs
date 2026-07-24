// ===== File: project_studio/project_db.rs — pool cache for per-project project.db files =====
//
// Every project stores its content (sources, files, ingest jobs, activity,
// settings, tags) in its own SQLite file `<dir_path>/project.db`. This module
// keeps a bounded cache of open pools (LRU eviction + idle sweeper) so the
// process never holds hundreds of open SQLite files while still avoiding a
// reopen-per-request. Migrations run on EVERY open, so an upgraded binary
// transparently upgrades a project the first time it is touched.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, Result};
use dashmap::DashMap;
use rusqlite::Connection;
use tracing::{info, warn};

use crate::db::DbPool;

/// Upper bound of simultaneously open per-project pools. Above it the least
/// recently used idle pool is checkpointed and dropped.
const MAX_OPEN_POOLS: usize = 16;

/// A pool untouched for this long is closed by the idle sweeper.
const IDLE_CLOSE: Duration = Duration::from_secs(600);

struct Entry {
    pool: DbPool,
    last_used_ms: AtomicI64,
}

fn registry() -> &'static DashMap<String, Arc<Entry>> {
    static REG: OnceLock<DashMap<String, Arc<Entry>>> = OnceLock::new();
    REG.get_or_init(DashMap::new)
}

static FROZEN: AtomicBool = AtomicBool::new(false);

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Validates a project id used as a registry key and (indirectly) a path
/// component. Project ids are server-minted UUIDv4 (lowercase hex + hyphens);
/// anything outside `[a-z0-9-]` — in particular `/`, `\` and `..` — is
/// rejected so a hostile wire value can never traverse out of the projects
/// tree, even though `dir_path` itself is read from the registry and never
/// recomputed from the id.
pub fn validate_project_id(project_id: &str) -> Result<()> {
    if project_id.is_empty() || project_id.len() > 64 {
        return Err(anyhow!("invalid project_id"));
    }
    if !project_id
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(anyhow!("invalid project_id"));
    }
    Ok(())
}

/// Returns the pool for `project_id`, opening `<dir_path>/project.db` when it
/// is not cached. `dir_path` is read from the central registry (single source
/// of truth — NOT recomputed from the id), migrations run on every fresh open
/// and the resulting schema version is mirrored into
/// `projects.schema_version_cache`.
pub fn open(project_id: &str) -> Result<DbPool> {
    validate_project_id(project_id)?;
    if FROZEN.load(Ordering::SeqCst) {
        return Err(anyhow!("project storage is frozen (migration in progress)"));
    }

    if let Some(entry) = registry().get(project_id) {
        entry.last_used_ms.store(now_ms(), Ordering::Relaxed);
        return Ok(entry.pool.clone());
    }

    let dir_path: String = {
        let central = super::db::pool()?;
        let conn = central
            .read()
            .map_err(|e| anyhow!("projects registry read: {e}"))?;
        conn.query_row(
            "SELECT dir_path FROM projects WHERE project_id = ?1",
            rusqlite::params![project_id],
            |row| row.get(0),
        )
        .map_err(|_| anyhow!("project not found in registry"))?
    };

    let (pool, version) = open_pool_at(Path::new(&dir_path))?;

    // Fresh open only: close jobs orphaned by a restart, then GC stale upload
    // parts and unreferenced blobs — both need the pool but must not observe a
    // half-initialised registry entry.
    super::ingest::recover_orphaned_jobs(&pool);
    super::ingest::cleanup_files_dir(&pool, Path::new(&dir_path));

    // Mirror the applied schema version so admin tooling can spot projects
    // that still need a data upgrade without opening each file.
    if let Ok(central) = super::db::pool() {
        if let Ok(conn) = central.write() {
            let _ = conn.execute(
                "UPDATE projects SET schema_version_cache = ?1 WHERE project_id = ?2",
                rusqlite::params![version, project_id],
            );
        }
    }

    let entry = Arc::new(Entry {
        pool: pool.clone(),
        last_used_ms: AtomicI64::new(now_ms()),
    });
    registry().insert(project_id.to_string(), entry);
    evict_lru_over_cap();
    Ok(pool)
}

/// Opens `<dir>/project.db`, applies the standard PRAGMAs and runs the
/// per-project migrations. Returns the pool and the applied schema version.
/// The directory must already exist (project create builds it): silently
/// creating it here would materialise an empty project.db under a wrong or
/// stale `dir_path` and mask the real problem.
pub(crate) fn open_pool_at(dir: &Path) -> Result<(DbPool, i64)> {
    if !dir.is_dir() {
        return Err(anyhow!(
            "project directory '{}' does not exist",
            dir.display()
        ));
    }
    let db_path = dir.join("project.db");
    let conn = Connection::open(&db_path)?;
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
    let version = run_project_migrations(&conn)?;
    Ok((Arc::new(crate::db::Db::from_connection(conn)), version))
}

/// Checkpoints and drops the cached pool for `project_id`. The SQLite file is
/// NOT removed. Safe to call for projects that were never opened.
pub fn close(project_id: &str) {
    if let Some((_, entry)) = registry().remove(project_id) {
        checkpoint_entry(project_id, &entry);
    }
}

fn checkpoint_entry(project_id: &str, entry: &Entry) {
    match entry.pool.write() {
        Ok(conn) => {
            if let Err(e) = conn.pragma_update(None, "wal_checkpoint", "TRUNCATE") {
                warn!(project_id, "project.db WAL checkpoint failed: {e}");
            }
        }
        Err(e) => warn!(project_id, "project.db checkpoint lock failed: {e}"),
    }
}

/// Drops the least recently used entries while the cache is over
/// `MAX_OPEN_POOLS`. Called after every insert; the pool `Arc` held by an
/// in-flight job stays alive until that job drops it.
fn evict_lru_over_cap() {
    while registry().len() > MAX_OPEN_POOLS {
        let mut oldest: Option<(String, i64)> = None;
        for item in registry().iter() {
            let ts = item.value().last_used_ms.load(Ordering::Relaxed);
            if oldest.as_ref().map(|(_, o)| ts < *o).unwrap_or(true) {
                oldest = Some((item.key().clone(), ts));
            }
        }
        match oldest {
            Some((key, _)) => close(&key),
            None => break,
        }
    }
}

/// Spawns the background sweeper closing pools idle for longer than
/// `IDLE_CLOSE`. Call once at startup from within the tokio runtime.
pub fn spawn_idle_sweeper() {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        loop {
            tick.tick().await;
            let cutoff = now_ms() - IDLE_CLOSE.as_millis() as i64;
            let idle: Vec<String> = registry()
                .iter()
                .filter(|e| e.value().last_used_ms.load(Ordering::Relaxed) < cutoff)
                .map(|e| e.key().clone())
                .collect();
            for key in idle {
                info!(project_id = %key, "closing idle project.db pool");
                close(&key);
            }
        }
    });
}

/// Freezes (or unfreezes) per-project SQLite access for the duration of a
/// data-directory migration. Freezing checkpoints and drops every cached
/// pool; new opens are rejected until unfrozen (same contract as
/// `addon::storage_sql::set_addon_storage_frozen`).
pub fn set_frozen(frozen: bool) {
    FROZEN.store(frozen, Ordering::SeqCst);
    if frozen {
        let keys: Vec<String> = registry().iter().map(|e| e.key().clone()).collect();
        for key in keys {
            close(&key);
        }
    }
}

/// Checkpoints every open per-project database. Call at shutdown next to
/// `db::checkpoint_wal` so no project.db is left with an unflushed WAL.
pub fn checkpoint_all() {
    for item in registry().iter() {
        checkpoint_entry(item.key(), item.value());
    }
}

/// Versioned migration runner for a single project.db. Tracks applied
/// versions in `project_schema_version`; returns the highest applied version.
fn run_project_migrations(conn: &Connection) -> Result<i64> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS project_schema_version (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )?;

    let current: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM project_schema_version",
        [],
        |row| row.get(0),
    )?;

    let mut latest = current;
    for (version, sql) in MIGRATIONS_PROJECT {
        if *version > current {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(sql)?;
            tx.execute(
                "INSERT INTO project_schema_version (version) VALUES (?1)",
                rusqlite::params![version],
            )?;
            tx.commit()?;
            latest = *version;
        }
    }
    Ok(latest)
}

/// Ordered per-project schema migrations (F2 tables land as further entries).
const MIGRATIONS_PROJECT: &[(i64, &str)] = &[(1, PROJECT_SCHEMA_V1)];

const PROJECT_SCHEMA_V1: &str = "
CREATE TABLE sources (
    source_id TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK(kind IN ('document','url','git','zip','api_spec')),
    name TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','indexing','ready','error','cancelled')),
    config_json TEXT NOT NULL DEFAULT '{}', error TEXT NOT NULL DEFAULT '',
    created_by TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE source_files (
    file_id TEXT PRIMARY KEY, source_id TEXT NOT NULL, path TEXT NOT NULL,
    sha256 TEXT NOT NULL DEFAULT '', size_bytes INTEGER NOT NULL DEFAULT 0,
    mime TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','indexing','ready','skipped','error')),
    error TEXT NOT NULL DEFAULT '', chunk_count INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(source_id, path)
);
CREATE INDEX idx_source_files_source ON source_files(source_id, status);
CREATE TABLE ingest_jobs (
    job_id TEXT PRIMARY KEY, source_id TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'running' CHECK(status IN ('running','success','failed','cancelled')),
    files_total INTEGER NOT NULL DEFAULT 0, files_done INTEGER NOT NULL DEFAULT 0,
    chunks_done INTEGER NOT NULL DEFAULT 0, error TEXT NOT NULL DEFAULT '',
    started_by TEXT NOT NULL, started_at TEXT NOT NULL DEFAULT (datetime('now')),
    finished_at TEXT
);
CREATE INDEX idx_ingest_jobs_source ON ingest_jobs(source_id, started_at DESC);
CREATE TABLE activity_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    actor_user_id TEXT NOT NULL DEFAULT '',
    actor_kind TEXT NOT NULL DEFAULT 'user' CHECK(actor_kind IN ('user','agent','system')),
    action TEXT NOT NULL, object_type TEXT NOT NULL DEFAULT '',
    object_id TEXT NOT NULL DEFAULT '', details_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE settings ( key TEXT PRIMARY KEY, value TEXT NOT NULL DEFAULT '' );
CREATE TABLE tags (
    tag_id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE COLLATE NOCASE,
    created_by TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_project_id_rejects_traversal_and_separators() {
        assert!(validate_project_id("").is_err());
        assert!(validate_project_id("..").is_err());
        assert!(validate_project_id("../etc").is_err());
        assert!(validate_project_id("a/b").is_err());
        assert!(validate_project_id("a\\b").is_err());
        assert!(validate_project_id("UPPER").is_err());
        assert!(validate_project_id("id with space").is_err());
        assert!(validate_project_id(&"x".repeat(65)).is_err());
        assert!(validate_project_id("0a1b2c3d-4e5f-4a6b-8c9d-0e1f2a3b4c5d").is_ok());
    }

    #[test]
    fn open_pool_at_migrates_and_reopens() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("proj");

        // A missing directory is a hard error, never a silent empty database.
        assert!(open_pool_at(&dir).is_err());
        std::fs::create_dir_all(&dir).expect("create project dir");

        let (pool, version) = open_pool_at(&dir).expect("first open");
        assert!(version >= 1);
        {
            let conn = pool.write().expect("write");
            conn.execute(
                "INSERT INTO sources (source_id, kind, name, created_by) \
                 VALUES ('s1', 'document', 'doc', 'u1')",
                [],
            )
            .expect("insert source");
        }
        drop(pool);

        // Reopen: migrations are idempotent and previously written data survives.
        let (pool2, version2) = open_pool_at(&dir).expect("reopen");
        assert_eq!(version, version2);
        let conn = pool2.read().expect("read");
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM sources", [], |r| r.get(0))
            .expect("count");
        assert_eq!(n, 1);
        let applied: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM project_schema_version WHERE version = 1",
                [],
                |r| r.get(0),
            )
            .expect("version row");
        assert_eq!(applied, 1, "migration recorded exactly once");
    }
}
