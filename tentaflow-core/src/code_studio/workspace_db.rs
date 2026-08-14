// ===== File: code_studio/workspace_db.rs — per-workspace runtime database and its pool =====
//
// Sessions, events, effects, patch sets and approvals of ONE workspace live in
// `<workspace>/workspace.db` on the owner node. They are deliberately not in
// the main database: they are runtime state of a single node, they are written
// constantly, and they must not travel through the Sync Ledger.
//
// The pool follows `project_studio/project_db.rs`: a bounded LRU cache of open
// pools with an idle sweeper, migrations on every fresh open, and a
// `checkpoint_all` for shutdown. The bound matters — a node with a hundred
// workspaces must not hold a hundred open SQLite files, and a request must not
// pay a reopen either.
//
// One invariant the schema encodes rather than assumes: `session_events.seq` is
// allocated by a SINGLE writer (the coordinator) inside the same transaction as
// the insert, and `UNIQUE(session_id, seq)` makes a second writer fail loudly
// instead of silently interleaving a broken timeline.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, Result};
use dashmap::DashMap;
use rusqlite::Connection;
use tracing::{info, warn};

use super::paths;
use crate::db::DbPool;

/// Upper bound of simultaneously open workspace pools. Above it the least
/// recently used one is checkpointed and dropped.
const MAX_OPEN_POOLS: usize = 16;

/// A pool untouched for this long is closed by the idle sweeper.
const IDLE_CLOSE: Duration = Duration::from_secs(600);

/// Highest runtime schema version this binary knows.
pub const LATEST_SCHEMA_VERSION: i64 = 1;

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

/// Returns the pool for `workspace_id`, opening `workspace.db` when it is not
/// cached. The directory must already exist — the provisioning saga creates it,
/// and silently creating it here would materialise an empty runtime database
/// for a workspace that was never provisioned, masking the real failure.
pub fn open(workspace_id: &str) -> Result<DbPool> {
    paths::validate_workspace_id(workspace_id)?;
    if FROZEN.load(Ordering::SeqCst) {
        return Err(anyhow!(
            "code studio storage is frozen (migration in progress)"
        ));
    }
    if let Some(entry) = registry().get(workspace_id) {
        entry.last_used_ms.store(now_ms(), Ordering::Relaxed);
        return Ok(entry.pool.clone());
    }

    let dir = paths::workspace_dir(workspace_id)?;
    let (pool, _version) = open_pool_at(&dir)?;
    registry().insert(
        workspace_id.to_string(),
        Arc::new(Entry {
            pool: pool.clone(),
            last_used_ms: AtomicI64::new(now_ms()),
        }),
    );
    evict_lru_over_cap();
    Ok(pool)
}

/// Opens `<dir>/workspace.db`, applies the standard PRAGMAs and runs the
/// runtime migrations. Returns the pool and the applied schema version.
pub fn open_pool_at(dir: &Path) -> Result<(DbPool, i64)> {
    if !dir.is_dir() {
        return Err(anyhow!(
            "workspace directory '{}' does not exist",
            dir.display()
        ));
    }
    let conn = Connection::open(dir.join("workspace.db"))?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;\
         PRAGMA foreign_keys=ON;\
         PRAGMA synchronous=NORMAL;\
         PRAGMA cache_size=-65536;\
         PRAGMA temp_store=MEMORY;\
         PRAGMA busy_timeout=5000;\
         PRAGMA wal_autocheckpoint=2000;",
    )?;
    let version = run_migrations(&conn)?;
    Ok((Arc::new(crate::db::Db::from_connection(conn)), version))
}

/// Checkpoints and drops the cached pool. The SQLite file is NOT removed.
pub fn close(workspace_id: &str) {
    if let Some((_, entry)) = registry().remove(workspace_id) {
        checkpoint_entry(workspace_id, &entry);
    }
}

fn checkpoint_entry(workspace_id: &str, entry: &Entry) {
    match entry.pool.write() {
        Ok(conn) => {
            if let Err(e) = conn.pragma_update(None, "wal_checkpoint", "TRUNCATE") {
                warn!(workspace_id, "workspace.db WAL checkpoint failed: {e}");
            }
        }
        Err(e) => warn!(workspace_id, "workspace.db checkpoint lock failed: {e}"),
    }
}

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
                info!(workspace_id = %key, "closing idle workspace.db pool");
                close(&key);
            }
        }
    });
}

/// Freezes (or unfreezes) workspace SQLite access for a data-directory
/// migration. Freezing checkpoints and drops every cached pool; new opens are
/// rejected until unfrozen.
pub fn set_frozen(frozen: bool) {
    FROZEN.store(frozen, Ordering::SeqCst);
    if frozen {
        let keys: Vec<String> = registry().iter().map(|e| e.key().clone()).collect();
        for key in keys {
            close(&key);
        }
    }
}

/// Checkpoints every open workspace database. Call at shutdown next to
/// `db::checkpoint_wal` so no workspace.db is left with an unflushed WAL.
pub fn checkpoint_all() {
    for item in registry().iter() {
        checkpoint_entry(item.key(), item.value());
    }
}

/// Versioned migration runner for a single workspace.db.
fn run_migrations(conn: &Connection) -> Result<i64> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS workspace_schema_version (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )?;
    let current: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM workspace_schema_version",
        [],
        |row| row.get(0),
    )?;
    let mut latest = current;
    for (version, sql) in MIGRATIONS {
        if *version > current {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(sql)?;
            tx.execute(
                "INSERT INTO workspace_schema_version (version) VALUES (?1)",
                rusqlite::params![version],
            )?;
            tx.commit()?;
            latest = *version;
        }
    }
    Ok(latest)
}

const MIGRATIONS: &[(i64, &str)] = &[(1, WORKSPACE_SCHEMA_V1)];

const WORKSPACE_SCHEMA_V1: &str = r#"
CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    title TEXT NOT NULL,
    branch TEXT NOT NULL,
    autonomy_mode TEXT NOT NULL,
    flow_id TEXT NOT NULL,
    flow_version_id TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN
      ('creating','idle','running','waiting_user','completed','failed','cancelled',
       'interrupted','closing','closed')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    closed_at TEXT
);
CREATE INDEX idx_sessions_user ON sessions(user_id, status);

-- `head_commit` is the start point for a working worktree and the expected_old
-- of the target ref for an integration one; `state='held'` marks a merge whose
-- result is waiting for a conflict to be resolved in a later run, and such a
-- worktree must NOT be removed or the revision run has nothing to work on.
CREATE TABLE worktrees (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    purpose TEXT NOT NULL CHECK(purpose IN ('work','integration')),
    op_id TEXT,
    path TEXT NOT NULL,
    branch TEXT,
    head_commit TEXT NOT NULL,
    base_commit TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN
      ('creating','ready','dirty','clean','held','detaching','removed')),
    created_at TEXT NOT NULL,
    removed_at TEXT,
    UNIQUE(session_id, purpose, op_id)
);

-- Mount access and network access are INDEPENDENT axes. `lease_id` allows
-- several concurrent processes on the same profile (two test runs at once).
CREATE TABLE sandboxes (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    mount_access   TEXT NOT NULL CHECK(mount_access   IN ('ro','cow','rw')),
    network_access TEXT NOT NULL CHECK(network_access IN ('none','gateway')),
    lease_id TEXT,
    owner_run_id TEXT,
    runtime_ref TEXT,
    state TEXT NOT NULL CHECK(state IN ('starting','ready','stopping','stopped','failed')),
    ephemeral INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    stopped_at TEXT
);
CREATE UNIQUE INDEX idx_sandbox_shared ON sandboxes(session_id, mount_access, network_access)
    WHERE ephemeral = 0 AND state != 'stopped';
CREATE INDEX idx_sandbox_lease ON sandboxes(session_id, lease_id);

CREATE TABLE session_runs (
    run_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('root','subagent','cli','revision')),
    trigger TEXT NOT NULL CHECK(trigger IN
      ('user','agent_spawn','cli_delegate','review_rejected','test_failed',
       'merge_conflict','merge_verify_failed','resume')),
    parent_run_id TEXT,
    agent_id TEXT,
    status TEXT NOT NULL,
    started_at TEXT,
    finished_at TEXT,
    UNIQUE(session_id, ordinal)
);

CREATE TABLE cli_instances (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    run_id TEXT NOT NULL REFERENCES session_runs(run_id) ON DELETE CASCADE,
    engine_id TEXT NOT NULL,
    service_id INTEGER NOT NULL,
    vendor_session_id TEXT,
    model TEXT,
    ticket_id TEXT,
    status TEXT NOT NULL CHECK(status IN
      ('starting','ready','busy','idle','ended','failed','reaped')),
    last_seq INTEGER NOT NULL DEFAULT 0,
    os_pid INTEGER,
    started_at TEXT NOT NULL,
    ended_at TEXT
);

CREATE TABLE artifacts (
    sha256 TEXT PRIMARY KEY,
    size_bytes INTEGER NOT NULL,
    kind TEXT NOT NULL,
    refcount INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    last_used_at TEXT
);

-- The timeline is the source of truth. `seq` is allocated by ONE writer inside
-- the insert transaction; `idempotency_key` makes a retried write a no-op
-- rather than a duplicate entry.
CREATE TABLE session_events (
    event_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    idempotency_key TEXT NOT NULL,
    schema_version INTEGER NOT NULL,
    kind TEXT NOT NULL,
    run_id TEXT,
    agent_id TEXT,
    payload_cbor BLOB NOT NULL,
    artifact_ref TEXT REFERENCES artifacts(sha256),
    security_relevant INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    UNIQUE(session_id, seq),
    UNIQUE(session_id, idempotency_key)
);
CREATE INDEX idx_session_events_run ON session_events(run_id, seq);

-- Journal of EFFECTS: typed, with pre/postconditions and git OIDs, so an
-- operation interrupted by a crash can be classified instead of guessed at.
CREATE TABLE session_operations (
    op_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    run_id TEXT,
    origin_kind TEXT NOT NULL CHECK(origin_kind IN
      ('tool_call','terminal','ui','shim','flow_block','coordinator')),
    origin_id TEXT NOT NULL,
    logical_step TEXT NOT NULL,
    op_kind TEXT NOT NULL,
    capability TEXT NOT NULL,
    idempotent INTEGER NOT NULL,
    input_ref TEXT REFERENCES artifacts(sha256),
    precondition_cbor BLOB NOT NULL,
    postcondition_cbor BLOB NOT NULL,
    result_oids TEXT,
    status TEXT NOT NULL CHECK(status IN ('pending','completed','failed','unknown')),
    result_ref TEXT,
    error TEXT,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    UNIQUE(session_id, origin_kind, origin_id, logical_step)
);
CREATE INDEX idx_session_ops_open ON session_operations(session_id, status);

CREATE TABLE patch_sets (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    run_id TEXT REFERENCES session_runs(run_id),
    base_commit TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN
      ('open','in_review','accepted','partially_accepted','rejected','superseded','conflicted')),
    created_at TEXT NOT NULL,
    decided_by TEXT,
    decided_at TEXT
);

-- `patch_base_blob_sha` is frozen when the set opens and `current_blob_sha`
-- moves with every later edit: the difference is how a conflict is detected
-- when the agent keeps working during a review.
CREATE TABLE patch_files (
    id TEXT PRIMARY KEY,
    patch_set_id TEXT NOT NULL REFERENCES patch_sets(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    change_kind TEXT NOT NULL CHECK(change_kind IN ('add','modify','delete','rename')),
    old_path TEXT,
    patch_base_blob_sha TEXT,
    current_blob_sha TEXT,
    accepted_blob_sha TEXT,
    git_blob_oid TEXT,
    mode TEXT NOT NULL DEFAULT '100644',
    status TEXT NOT NULL CHECK(status IN
      ('pending','accepted','partially_accepted','rejected','conflicted')),
    UNIQUE(patch_set_id, path)
);

CREATE TABLE patch_hunks (
    id TEXT PRIMARY KEY,
    patch_file_id TEXT NOT NULL REFERENCES patch_files(id) ON DELETE CASCADE,
    idx INTEGER NOT NULL,
    header TEXT NOT NULL,
    content_ref TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('pending','accepted','rejected')),
    UNIQUE(patch_file_id, idx)
);

CREATE TABLE approvals (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    run_id TEXT,
    interaction_id TEXT NOT NULL,
    capability TEXT NOT NULL,
    target_digest TEXT NOT NULL,
    summary TEXT NOT NULL,
    detail_ref TEXT,
    status TEXT NOT NULL CHECK(status IN ('pending','decided','expired','abandoned')),
    decision TEXT CHECK(decision IN
      ('allow_once','allow_for_run','allow_for_session','always','deny')),
    requested_at TEXT NOT NULL,
    decided_at TEXT,
    decided_by TEXT
);

CREATE TABLE session_grants (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    capability TEXT NOT NULL,
    pattern TEXT NOT NULL,
    granted_by TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (session_id, capability, pattern)
);

-- Durable outbox of the audit mirror: a security event must survive a failure
-- between the two databases, so it is written here ALREADY REDACTED and
-- delivered afterwards.
CREATE TABLE audit_outbox (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL REFERENCES session_events(event_id),
    payload_cbor BLOB NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TEXT NOT NULL,
    delivered_at TEXT
);
CREATE INDEX idx_audit_outbox_pending ON audit_outbox(delivered_at, id);

CREATE TABLE index_state (
    branch TEXT PRIMARY KEY,
    indexed_commit TEXT,
    files INTEGER NOT NULL DEFAULT 0,
    chunks INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT,
    last_error TEXT
);
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_workspace() -> (tempfile::TempDir, DbPool) {
        let dir = tempfile::tempdir().expect("tempdir");
        let (pool, version) = open_pool_at(dir.path()).expect("open workspace.db");
        assert_eq!(version, LATEST_SCHEMA_VERSION);
        (dir, pool)
    }

    fn seed_session(pool: &DbPool, id: &str) {
        let conn = pool.write().unwrap();
        conn.execute(
            "INSERT INTO sessions (id, workspace_id, user_id, title, branch, autonomy_mode, \
              flow_id, flow_version_id, status, created_at, updated_at) \
             VALUES (?1, 'ws-1', 'u-1', 'Session', 'cs/u/1', 'normal', 'flow', 'v1', \
              'creating', datetime('now'), datetime('now'))",
            rusqlite::params![id],
        )
        .expect("insert session");
    }

    #[test]
    fn the_schema_applies_once_and_is_idempotent_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let (_pool, first) = open_pool_at(dir.path()).unwrap();
        let (pool, second) = open_pool_at(dir.path()).unwrap();
        assert_eq!(first, LATEST_SCHEMA_VERSION);
        assert_eq!(second, LATEST_SCHEMA_VERSION);

        let conn = pool.read().unwrap();
        let applied: i64 = conn
            .query_row("SELECT COUNT(*) FROM workspace_schema_version", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(applied, 1, "reopening re-applied a migration");
    }

    #[test]
    fn opening_a_missing_directory_fails_instead_of_creating_a_phantom_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("never-provisioned");
        assert!(open_pool_at(&missing).is_err());
        assert!(!missing.exists(), "a missing workspace was materialised");
    }

    #[test]
    fn two_events_cannot_share_a_sequence_number() {
        let (_dir, pool) = temp_workspace();
        seed_session(&pool, "s-1");
        let conn = pool.write().unwrap();
        let insert = "INSERT INTO session_events \
             (event_id, session_id, seq, idempotency_key, schema_version, kind, payload_cbor, created_at) \
             VALUES (?1, 's-1', ?2, ?3, 1, 'tool_call', X'00', datetime('now'))";
        conn.execute(insert, rusqlite::params!["e-1", 1, "k-1"])
            .unwrap();
        assert!(
            conn.execute(insert, rusqlite::params!["e-2", 1, "k-2"])
                .is_err(),
            "a duplicated seq must fail loudly, not interleave the timeline"
        );
        // The same key twice is a retry, not a new event.
        assert!(conn
            .execute(insert, rusqlite::params!["e-3", 2, "k-1"])
            .is_err());
    }

    #[test]
    fn one_operation_per_origin_and_logical_step() {
        let (_dir, pool) = temp_workspace();
        seed_session(&pool, "s-1");
        let conn = pool.write().unwrap();
        let insert = "INSERT INTO session_operations \
             (op_id, session_id, origin_kind, origin_id, logical_step, op_kind, capability, \
              idempotent, precondition_cbor, postcondition_cbor, status, started_at) \
             VALUES (?1, 's-1', 'tool_call', 'call-1', ?2, 'fs_write', 'fs_write', 0, X'00', X'00', \
              'pending', datetime('now'))";
        conn.execute(insert, rusqlite::params!["op-1", "write"])
            .unwrap();
        assert!(
            conn.execute(insert, rusqlite::params!["op-2", "write"])
                .is_err(),
            "the same logical step was journalled twice"
        );
        // A different step of the same call is a different effect.
        conn.execute(insert, rusqlite::params!["op-3", "chmod"])
            .unwrap();
    }

    #[test]
    fn only_one_shared_sandbox_per_profile_but_many_ephemeral_ones() {
        let (_dir, pool) = temp_workspace();
        seed_session(&pool, "s-1");
        let conn = pool.write().unwrap();
        let insert = "INSERT INTO sandboxes \
             (id, session_id, mount_access, network_access, state, ephemeral, created_at) \
             VALUES (?1, 's-1', 'cow', 'none', 'ready', ?2, datetime('now'))";
        conn.execute(insert, rusqlite::params!["sb-1", 0]).unwrap();
        assert!(
            conn.execute(insert, rusqlite::params!["sb-2", 0]).is_err(),
            "a second shared sandbox appeared on the same profile"
        );
        conn.execute(insert, rusqlite::params!["sb-3", 1]).unwrap();
        conn.execute(insert, rusqlite::params!["sb-4", 1]).unwrap();

        // Stopping the shared one frees the profile again.
        conn.execute("UPDATE sandboxes SET state='stopped' WHERE id='sb-1'", [])
            .unwrap();
        conn.execute(insert, rusqlite::params!["sb-5", 0]).unwrap();
    }

    #[test]
    fn a_session_takes_its_runtime_rows_with_it() {
        let (_dir, pool) = temp_workspace();
        seed_session(&pool, "s-1");
        let conn = pool.write().unwrap();
        conn.execute(
            "INSERT INTO session_runs (run_id, session_id, ordinal, kind, trigger, status) \
             VALUES ('r-1', 's-1', 1, 'root', 'user', 'running')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session_grants (session_id, capability, pattern, granted_by, created_at) \
             VALUES ('s-1', 'exec', 'cargo *', 'u-1', datetime('now'))",
            [],
        )
        .unwrap();

        conn.execute("DELETE FROM sessions WHERE id='s-1'", [])
            .unwrap();
        let runs: i64 = conn
            .query_row("SELECT COUNT(*) FROM session_runs", [], |row| row.get(0))
            .unwrap();
        let grants: i64 = conn
            .query_row("SELECT COUNT(*) FROM session_grants", [], |row| row.get(0))
            .unwrap();
        assert_eq!(runs, 0, "foreign keys are not enforced");
        assert_eq!(grants, 0);
    }

    #[test]
    fn the_status_vocabulary_is_enforced_by_the_schema() {
        let (_dir, pool) = temp_workspace();
        let conn = pool.write().unwrap();
        assert!(conn
            .execute(
                "INSERT INTO sessions (id, workspace_id, user_id, title, branch, autonomy_mode, \
                  flow_id, flow_version_id, status, created_at, updated_at) \
                 VALUES ('s-x', 'ws-1', 'u-1', 't', 'b', 'normal', 'f', 'v', 'whatever', \
                  datetime('now'), datetime('now'))",
                [],
            )
            .is_err());
    }

    #[test]
    fn an_integration_worktree_coexists_with_the_working_one() {
        let (_dir, pool) = temp_workspace();
        seed_session(&pool, "s-1");
        let conn = pool.write().unwrap();
        let insert = "INSERT INTO worktrees \
             (id, session_id, purpose, op_id, path, branch, head_commit, base_commit, state, created_at) \
             VALUES (?1, 's-1', ?2, ?3, ?4, ?5, 'abc', 'abc', 'ready', datetime('now'))";
        conn.execute(
            insert,
            rusqlite::params!["wt-1", "work", None::<String>, "/w", "cs/u/1"],
        )
        .unwrap();
        conn.execute(
            insert,
            rusqlite::params!["wt-2", "integration", "op-1", "/i", None::<String>],
        )
        .unwrap();
        // A second merge attempt of the same operation must not duplicate.
        assert!(conn
            .execute(
                insert,
                rusqlite::params!["wt-3", "integration", "op-1", "/i2", None::<String>],
            )
            .is_err());
    }
}
