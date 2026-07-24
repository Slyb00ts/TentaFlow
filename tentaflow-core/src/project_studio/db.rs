// ===== File: project_studio/db.rs — dedicated SQLite pool + migrations for Project Studio =====
//
// Project Studio ("Projekty") keeps its registry in a SEPARATE database file
// (`<data>/projects.db`), not in `tentaflow.db`. Because it is a different
// file, `owner_user_id`/`org_id` are application-level references to core
// `user_accounts`/`organizations` (TEXT columns, NO SQL foreign keys);
// identity always comes from the request `HandlerContext`, never from a join.
// Per-project content lives in `<dir_path>/project.db` (see `project_db.rs`).

use std::path::Path;
use std::sync::{Arc, OnceLock};

use anyhow::Result;
use rusqlite::Connection;
use tracing::info;

use crate::db::DbPool;

/// Global handle to the Project Studio registry pool, set once in `init`.
/// Mirrors `ml_studio::db` so repository functions reach the connection
/// without threading the pool through every call site.
static PROJECT_STUDIO_POOL: OnceLock<DbPool> = OnceLock::new();

/// Returns the Project Studio pool, or an error if `init` has not run. The
/// module is initialised at startup next to `ml_studio::init`, so a reachable
/// handler normally finds the pool present; returning an error (instead of
/// panicking) keeps a handler from crashing the worker when the database was
/// never opened.
pub fn pool() -> Result<DbPool> {
    PROJECT_STUDIO_POOL
        .get()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("project_studio database not initialised"))
}

/// Forces a WAL checkpoint (TRUNCATE) on the central Project Studio database
/// so a shutdown does not leave an unflushed `-wal` file. No-op when the pool
/// was never initialised, so it is safe to call unconditionally at shutdown.
pub fn checkpoint_wal() -> Result<()> {
    let Some(pool) = PROJECT_STUDIO_POOL.get() else {
        return Ok(());
    };
    let conn = pool
        .write()
        .map_err(|e| anyhow::anyhow!("project_studio pool write: {}", e))?;
    conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")?;
    info!("WAL checkpoint Project Studio wykonany");
    Ok(())
}

/// Opens (creating if absent) the dedicated Project Studio registry database,
/// applies the same performance PRAGMAs as core `db::init`, runs the module
/// migrations and publishes the pool. Idempotent: a second call leaves the
/// original pool in place and returns it.
pub fn init(db_path: &Path) -> Result<DbPool> {
    if let Some(existing) = PROJECT_STUDIO_POOL.get() {
        return Ok(existing.clone());
    }

    info!("Inicjalizacja bazy Project Studio: {:?}", db_path);
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let conn = Connection::open(db_path)?;
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

    run_migrations(&conn)?;

    let pool = Arc::new(crate::db::Db::from_connection(conn));
    let _ = PROJECT_STUDIO_POOL.set(pool.clone());
    info!("Baza Project Studio zainicjalizowana pomyslnie");
    Ok(pool)
}

/// Versioned migration runner for the central Project Studio database. Tracks
/// applied versions in `project_studio_schema_version` and applies each
/// pending `(version, sql)` step in its own transaction.
fn run_migrations(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS project_studio_schema_version (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )?;

    let current: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM project_studio_schema_version",
        [],
        |row| row.get(0),
    )?;

    for (version, sql) in MIGRATIONS {
        if *version > current {
            info!("Migracja Project Studio {}", version);
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(sql)?;
            tx.execute(
                "INSERT INTO project_studio_schema_version (version) VALUES (?1)",
                rusqlite::params![version],
            )?;
            tx.commit()?;
        }
    }
    Ok(())
}

/// Ordered central-registry schema migrations. Identity columns are TEXT
/// references to core identity (app-level, no SQL FK — different DB file).
const MIGRATIONS: &[(i64, &str)] = &[(1, INITIAL_SCHEMA)];

const INITIAL_SCHEMA: &str = "
CREATE TABLE projects (
    project_id TEXT PRIMARY KEY, org_id TEXT NOT NULL, name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'active' CHECK(status IN ('active','archived')),
    template TEXT NOT NULL DEFAULT '',
    modules_json TEXT NOT NULL DEFAULT '[\"knowledge\",\"chat\"]',
    owner_user_id TEXT NOT NULL, dir_path TEXT NOT NULL,
    schema_version_cache INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(org_id, name)
);
CREATE INDEX idx_projects_org_status ON projects(org_id, status);

CREATE TABLE project_members (
    project_id TEXT NOT NULL, user_id TEXT NOT NULL,
    role TEXT NOT NULL CHECK(role IN ('owner','manager','editor','tester','viewer')),
    invited_by TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (project_id, user_id)
);
CREATE INDEX idx_project_members_user ON project_members(user_id);

CREATE TABLE project_creator_grants (
    user_id TEXT PRIMARY KEY, org_id TEXT NOT NULL,
    granted_by TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_creator_grants_org ON project_creator_grants(org_id);

CREATE TABLE project_chats (
    chat_id TEXT PRIMARY KEY, project_id TEXT NOT NULL, user_id TEXT NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    session_id TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_project_chats_owner ON project_chats(project_id, user_id, updated_at DESC);

CREATE TABLE notifications (
    notification_id TEXT PRIMARY KEY, org_id TEXT NOT NULL, user_id TEXT NOT NULL,
    project_id TEXT NOT NULL DEFAULT '', kind TEXT NOT NULL, title TEXT NOT NULL,
    body TEXT NOT NULL DEFAULT '', link_json TEXT NOT NULL DEFAULT '{}',
    read_at TEXT, created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_notifications_user ON notifications(user_id, read_at, created_at DESC);
";
