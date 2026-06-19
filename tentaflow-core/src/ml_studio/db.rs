// ===== File: ml_studio/db.rs — dedicated SQLite pool + migrations for ML Studio =====
//
// ML Studio keeps its state in a SEPARATE database file
// (`<TENTAFLOW_HOME>/data/ml_studio.db`), not in `tentaflow.db`. Because it is
// a different file, `owner_user_id`/`org_id` are application-level references
// to core `user_accounts`/`organizations` (TEXT columns, NO SQL foreign keys);
// identity always comes from the request `HandlerContext`, never from a join.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use anyhow::Result;
use rusqlite::Connection;
use tracing::info;

use crate::db::DbPool;

/// Global handle to the ML Studio pool, set once in `init`. Mirrors the core
/// `db::global_pool` pattern so repository functions reach the connection
/// without threading the pool through every call site.
static ML_STUDIO_POOL: OnceLock<DbPool> = OnceLock::new();

/// Returns the ML Studio pool, or an error if `init` has not run. The module is
/// initialised at startup next to `db::init`, so a reachable handler normally
/// finds the pool present; returning an error (instead of panicking) keeps a
/// handler from crashing the worker when the database was never opened.
pub fn pool() -> Result<DbPool> {
    ML_STUDIO_POOL
        .get()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("ml_studio database not initialised"))
}

/// Forces a WAL checkpoint (TRUNCATE) on the ML Studio database so a shutdown
/// does not leave an unflushed `-wal` file. No-op when the pool was never
/// initialised, so it is safe to call unconditionally at shutdown.
pub fn checkpoint_wal() -> Result<()> {
    let Some(pool) = ML_STUDIO_POOL.get() else {
        return Ok(());
    };
    let conn = pool
        .write()
        .map_err(|e| anyhow::anyhow!("ml_studio pool write: {}", e))?;
    conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")?;
    info!("WAL checkpoint ML Studio wykonany");
    Ok(())
}

/// Opens (creating if absent) the dedicated ML Studio database, applies the
/// same performance PRAGMAs as core `db::init`, runs the ML Studio migrations
/// and publishes the pool. Idempotent: a second call leaves the original pool
/// in place and returns it.
pub fn init(db_path: &Path) -> Result<DbPool> {
    if let Some(existing) = ML_STUDIO_POOL.get() {
        return Ok(existing.clone());
    }

    info!("Inicjalizacja bazy ML Studio: {:?}", db_path);
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
    let _ = ML_STUDIO_POOL.set(pool.clone());
    info!("Baza ML Studio zainicjalizowana pomyslnie");
    Ok(pool)
}

/// Versioned migration runner for the ML Studio database. Tracks applied
/// versions in `ml_studio_schema_version` and applies each pending `(version,
/// sql)` step in its own transaction.
fn run_migrations(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS ml_studio_schema_version (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )?;

    let current: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM ml_studio_schema_version",
        [],
        |row| row.get(0),
    )?;

    for (version, sql) in MIGRATIONS {
        if *version > current {
            info!("Migracja ML Studio {}", version);
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(sql)?;
            tx.execute(
                "INSERT INTO ml_studio_schema_version (version) VALUES (?1)",
                rusqlite::params![version],
            )?;
            tx.commit()?;
        }
    }
    Ok(())
}

/// Ordered ML Studio schema migrations. `owner_user_id`/`org_id` are TEXT
/// references to core identity (app-level, no SQL FK — different DB file).
const MIGRATIONS: &[(i64, &str)] = &[
    (1, INITIAL_SCHEMA),
    (2, PROJECT_MEMBERS),
    (3, DATASET_PROFILE),
    (4, RESOURCE_GRANTS),
    (5, DATASET_RAW_DATA),
];

const INITIAL_SCHEMA: &str = "
CREATE TABLE projects (
    project_id   TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    description  TEXT NOT NULL DEFAULT '',
    project_type TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'active',
    owner_user_id TEXT NOT NULL,
    org_id       TEXT NOT NULL,
    created_at   TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at   TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(org_id, name)
);

CREATE TABLE datasets (
    dataset_id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    name       TEXT NOT NULL,
    kind       TEXT NOT NULL,
    row_count  INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_datasets_project ON datasets(project_id);

CREATE TABLE schemas (
    schema_id  TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    json       TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_schemas_project ON schemas(project_id);

CREATE TABLE annotations (
    annotation_id TEXT PRIMARY KEY,
    project_id    TEXT NOT NULL,
    dataset_id    TEXT NOT NULL,
    json          TEXT NOT NULL,
    created_at    TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_annotations_project ON annotations(project_id);

CREATE TABLE training_runs (
    run_id              TEXT PRIMARY KEY,
    project_id          TEXT NOT NULL,
    model_id            TEXT,
    flow_invocation_id  TEXT,
    status              TEXT NOT NULL DEFAULT 'pending',
    config_json         TEXT NOT NULL DEFAULT '{}',
    started_at          TEXT,
    finished_at         TEXT
);
CREATE INDEX idx_training_runs_project ON training_runs(project_id);

CREATE TABLE models (
    model_id     TEXT PRIMARY KEY,
    project_id   TEXT NOT NULL,
    name         TEXT NOT NULL,
    framework    TEXT NOT NULL DEFAULT '',
    base_model   TEXT NOT NULL DEFAULT '',
    metrics_json TEXT NOT NULL DEFAULT '{}',
    status       TEXT NOT NULL DEFAULT 'draft',
    created_at   TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_models_project ON models(project_id);

CREATE TABLE metrics_history (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id       TEXT NOT NULL,
    step         INTEGER NOT NULL DEFAULT 0,
    metric_key   TEXT NOT NULL,
    metric_value REAL NOT NULL,
    ts           TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_metrics_history_run ON metrics_history(run_id);

CREATE TABLE lookup_dicts (
    dict_id    TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    name       TEXT NOT NULL,
    rows_json  TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX idx_lookup_dicts_project ON lookup_dicts(project_id);

CREATE TABLE service_models (
    id                TEXT PRIMARY KEY,
    name              TEXT NOT NULL,
    capabilities_json TEXT NOT NULL DEFAULT '{}',
    source            TEXT NOT NULL DEFAULT '',
    status            TEXT NOT NULL DEFAULT 'available'
);
";

const PROJECT_MEMBERS: &str = "
CREATE TABLE project_members (
    project_id TEXT NOT NULL,
    user_id    TEXT NOT NULL,
    role       TEXT NOT NULL,
    status     TEXT NOT NULL,
    invited_by TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (project_id, user_id)
);
CREATE INDEX idx_project_members_user ON project_members(user_id);
INSERT OR IGNORE INTO project_members (project_id, user_id, role, status, invited_by)
SELECT project_id, owner_user_id, 'owner', 'active', owner_user_id
FROM projects WHERE owner_user_id IS NOT NULL AND owner_user_id != '';
";

const DATASET_PROFILE: &str = "
ALTER TABLE datasets ADD COLUMN column_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE datasets ADD COLUMN profile_json TEXT NOT NULL DEFAULT '{}';
";

// Admin-managed mesh resource grants (§11.3). One row = one allocation of a
// node resource (gpu/cpu/ram) to a subject (user/group/project). These are
// GRANT records, not live usage: `quota` is free-form text (GPU count, hours,
// or empty). `node_id`/`subject_id`/`granted_by` are app-level identifiers
// (no SQL FK — pool of nodes comes from the mesh registry, not this DB).
const RESOURCE_GRANTS: &str = "
CREATE TABLE resource_grants (
    grant_id      TEXT PRIMARY KEY,
    subject_kind  TEXT NOT NULL,
    subject_id    TEXT NOT NULL,
    node_id       TEXT NOT NULL,
    resource_kind TEXT NOT NULL,
    resource_ref  TEXT NOT NULL DEFAULT '',
    quota         TEXT NOT NULL DEFAULT '',
    granted_by    TEXT NOT NULL,
    created_at    TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_resource_grants_subject ON resource_grants(subject_kind, subject_id);
CREATE INDEX idx_resource_grants_node ON resource_grants(node_id);
";

// Stores the raw uploaded file bytes (already bounded to <= 1 MiB by the upload
// limit) so a later training run can re-parse the original data without keeping
// a separate file store. NULL for datasets created before this migration.
const DATASET_RAW_DATA: &str = "
ALTER TABLE datasets ADD COLUMN raw_data BLOB;
";
