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

/// Project id of a pool handed out by [`open`]. A `DbPool` carries no path, so
/// pointer identity against this cache is the only way back from a handle to
/// its project — the terminal-run path (`schedules::settle`) has nothing else
/// to resolve a notification target from.
pub fn project_id_of(pool: &DbPool) -> Option<String> {
    registry()
        .iter()
        .find(|entry| Arc::ptr_eq(&entry.value().pool, pool))
        .map(|entry| entry.key().clone())
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

/// Highest per-project schema version this binary knows. An archive produced by
/// a NEWER node is refused on import rather than migrated blindly.
pub const LATEST_SCHEMA_VERSION: i64 = 4;

/// Ordered per-project schema migrations (F4+ tables land as further entries).
const MIGRATIONS_PROJECT: &[(i64, &str)] = &[
    (1, PROJECT_SCHEMA_V1),
    (2, PROJECT_SCHEMA_V2),
    (3, PROJECT_SCHEMA_V3),
    (4, PROJECT_SCHEMA_V4),
];

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

/// F2: manual test cases + versions + tags, suites, runs with pinned item
/// snapshots, tasks/defects with comments and agent generation runs. Cases
/// carry `review_state` — agent output stays 'pending' (hidden everywhere)
/// until review. Run items snapshot case_version + case_title, steps copy
/// action/expected from the case content, so later edits never mutate a
/// running execution.
const PROJECT_SCHEMA_V2: &str = "
CREATE TABLE test_cases (
    case_id TEXT PRIMARY KEY,
    kind TEXT NOT NULL DEFAULT 'manual' CHECK(kind IN ('manual','ui','api','unit','perf','security')),
    title TEXT NOT NULL,
    priority TEXT NOT NULL DEFAULT 'medium' CHECK(priority IN ('low','medium','high','critical')),
    status TEXT NOT NULL DEFAULT 'draft' CHECK(status IN ('draft','review','approved','deprecated')),
    status_reason TEXT NOT NULL DEFAULT '',
    review_state TEXT NOT NULL DEFAULT '' CHECK(review_state IN ('','pending','accepted')),
    origin TEXT NOT NULL DEFAULT 'user' CHECK(origin IN ('user','agent')),
    generation_run_id TEXT NOT NULL DEFAULT '',
    provenance_json TEXT NOT NULL DEFAULT '{}',
    linked_sources_json TEXT NOT NULL DEFAULT '[]',
    attachments_json TEXT NOT NULL DEFAULT '[]',
    language TEXT NOT NULL DEFAULT 'pl',
    current_version INTEGER NOT NULL DEFAULT 1,
    content_json TEXT NOT NULL DEFAULT '{}',
    created_by TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_test_cases_filter ON test_cases(kind, status, review_state, updated_at DESC);
CREATE INDEX idx_test_cases_generation ON test_cases(generation_run_id);
CREATE TABLE test_case_versions (
    case_id TEXT NOT NULL, version INTEGER NOT NULL,
    content_json TEXT NOT NULL DEFAULT '{}',
    change_note TEXT NOT NULL DEFAULT '',
    created_by TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (case_id, version)
);
CREATE TABLE case_tags (
    case_id TEXT NOT NULL, tag_id TEXT NOT NULL,
    PRIMARY KEY (case_id, tag_id)
);
CREATE INDEX idx_case_tags_tag ON case_tags(tag_id);
CREATE TABLE test_suites (
    suite_id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE COLLATE NOCASE,
    description TEXT NOT NULL DEFAULT '',
    created_by TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE suite_cases (
    suite_id TEXT NOT NULL, case_id TEXT NOT NULL,
    position INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (suite_id, case_id)
);
CREATE TABLE test_runs (
    run_id TEXT PRIMARY KEY,
    run_no INTEGER NOT NULL UNIQUE,
    name TEXT NOT NULL,
    suite_id TEXT NOT NULL DEFAULT '',
    run_type TEXT NOT NULL DEFAULT 'manual',
    environment_id TEXT NOT NULL DEFAULT '',
    env_note TEXT NOT NULL DEFAULT '',
    assignment_mode TEXT NOT NULL CHECK(assignment_mode IN ('single','per_case','pool')),
    status TEXT NOT NULL DEFAULT 'running' CHECK(status IN ('running','completed','cancelled')),
    closed_by TEXT NOT NULL DEFAULT '',
    created_by TEXT NOT NULL,
    started_at TEXT NOT NULL DEFAULT (datetime('now')),
    finished_at TEXT
);
CREATE TABLE test_run_items (
    item_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    case_id TEXT NOT NULL,
    case_title TEXT NOT NULL,
    case_version INTEGER NOT NULL,
    position INTEGER NOT NULL DEFAULT 0,
    assigned_to TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','in_progress','passed','failed','blocked','skipped')),
    result_note TEXT NOT NULL DEFAULT '',
    tester_config TEXT NOT NULL DEFAULT '',
    duration_secs INTEGER NOT NULL DEFAULT 0,
    attachments_json TEXT NOT NULL DEFAULT '[]',
    claimed_at TEXT,
    finished_at TEXT,
    UNIQUE(run_id, case_id)
);
CREATE INDEX idx_run_items_run ON test_run_items(run_id, position);
CREATE INDEX idx_run_items_assignee ON test_run_items(assigned_to, status);
CREATE TABLE test_run_steps (
    item_id TEXT NOT NULL, step_index INTEGER NOT NULL,
    action TEXT NOT NULL DEFAULT '',
    expected TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT '',
    note TEXT NOT NULL DEFAULT '',
    attachments_json TEXT NOT NULL DEFAULT '[]',
    PRIMARY KEY (item_id, step_index)
);
CREATE TABLE tasks (
    task_id TEXT PRIMARY KEY,
    task_no INTEGER NOT NULL UNIQUE,
    task_type TEXT NOT NULL DEFAULT 'task' CHECK(task_type IN ('task','defect')),
    title TEXT NOT NULL,
    description_md TEXT NOT NULL DEFAULT '',
    severity TEXT NOT NULL DEFAULT '' CHECK(severity IN ('','low','medium','high','critical')),
    priority TEXT NOT NULL DEFAULT 'medium' CHECK(priority IN ('low','medium','high','critical')),
    status TEXT NOT NULL DEFAULT 'todo' CHECK(status IN ('todo','in_progress','review','done')),
    assigned_to TEXT NOT NULL DEFAULT '',
    due_date TEXT NOT NULL DEFAULT '',
    links_json TEXT NOT NULL DEFAULT '[]',
    attachments_json TEXT NOT NULL DEFAULT '[]',
    created_by TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_tasks_filter ON tasks(task_type, status, updated_at DESC);
CREATE INDEX idx_tasks_assignee ON tasks(assigned_to, status);
CREATE TABLE task_comments (
    comment_id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    author_user_id TEXT NOT NULL,
    body_md TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    edited_at TEXT
);
CREATE INDEX idx_task_comments_task ON task_comments(task_id, created_at);
CREATE TABLE generation_runs (
    gen_id TEXT PRIMARY KEY,
    kind TEXT NOT NULL DEFAULT 'manual',
    status TEXT NOT NULL DEFAULT 'running' CHECK(status IN ('running','review','accepted','rejected','failed','cancelled')),
    agent_id TEXT NOT NULL,
    agent_run_id TEXT NOT NULL DEFAULT '',
    source_ids_json TEXT NOT NULL DEFAULT '[]',
    instructions TEXT NOT NULL DEFAULT '',
    requested_count INTEGER NOT NULL DEFAULT 0,
    max_cases INTEGER NOT NULL DEFAULT 10,
    cases_generated INTEGER NOT NULL DEFAULT 0,
    cases_accepted INTEGER NOT NULL DEFAULT 0,
    cases_rejected INTEGER NOT NULL DEFAULT 0,
    error TEXT NOT NULL DEFAULT '',
    started_by TEXT NOT NULL,
    started_at TEXT NOT NULL DEFAULT (datetime('now')),
    finished_at TEXT
);
CREATE INDEX idx_generation_runs_started ON generation_runs(started_at DESC);
CREATE INDEX idx_generation_runs_agent_run ON generation_runs(agent_run_id);
CREATE TABLE generation_run_sources (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    gen_id TEXT NOT NULL,
    source_id TEXT NOT NULL,
    case_id TEXT NOT NULL DEFAULT '',
    excerpt TEXT NOT NULL DEFAULT ''
);
CREATE INDEX idx_generation_run_sources_gen ON generation_run_sources(gen_id);
";

/// F3: test environments (admin-gated for private addresses), build profiles
/// for unit-test sources, automated-run metadata (runner binding + watchdog +
/// perf aggregates), run artifacts and the git-source access token.
///
/// test_runs / test_run_items are REBUILT (SQLite cannot alter a CHECK
/// constraint) via the documented 12-step procedure: create the new table
/// with the extended status CHECK, copy every row with an explicit column
/// list, drop the old table, rename, recreate the indexes. The v2 schema has
/// no FOREIGN KEY constraints, so no foreign_keys toggling is needed and the
/// whole rebuild stays inside the migration transaction. `run_no INTEGER
/// UNIQUE` and `UNIQUE(run_id, case_id)` are declared again in the new DDL,
/// so their implicit indexes survive the rebuild too.
const PROJECT_SCHEMA_V3: &str = "
CREATE TABLE environments (
    environment_id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE COLLATE NOCASE,
    env_type TEXT NOT NULL DEFAULT 'web' CHECK(env_type IN ('web','api')),
    base_url TEXT NOT NULL,
    auth_type TEXT NOT NULL DEFAULT 'none' CHECK(auth_type IN ('none','bearer','api_key','basic')),
    secret_enc TEXT NOT NULL DEFAULT '',
    extra_headers_json TEXT NOT NULL DEFAULT '{}',
    host_allowlist_json TEXT NOT NULL DEFAULT '[]',
    approval_status TEXT NOT NULL DEFAULT 'pending' CHECK(approval_status IN ('pending','approved','rejected')),
    approval_reason TEXT NOT NULL DEFAULT '',
    is_private_address INTEGER NOT NULL DEFAULT 0,
    justification TEXT NOT NULL DEFAULT '',
    requested_by TEXT NOT NULL,
    decided_by TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    decided_at TEXT
);
CREATE INDEX idx_environments_status ON environments(approval_status);
CREATE TABLE build_profiles (
    profile_id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL UNIQUE,
    toolchain TEXT NOT NULL CHECK(toolchain IN ('python','node','dotnet','jvm','rust','go')),
    base_image TEXT NOT NULL DEFAULT '',
    install_cmd TEXT NOT NULL DEFAULT '',
    test_cmd TEXT NOT NULL DEFAULT '',
    workdir TEXT NOT NULL DEFAULT '',
    proposed_by TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE auto_run_meta (
    run_id TEXT PRIMARY KEY,
    environment_id TEXT NOT NULL DEFAULT '',
    runner_service_id TEXT NOT NULL DEFAULT '',
    runner_endpoint TEXT NOT NULL DEFAULT '',
    runner_job_id TEXT NOT NULL DEFAULT '',
    perf_profile_json TEXT NOT NULL DEFAULT '{}',
    perf_summary_json TEXT NOT NULL DEFAULT '[]',
    perf_timeline_json TEXT NOT NULL DEFAULT '[]',
    last_poll_at TEXT NOT NULL DEFAULT '',
    failed_polls INTEGER NOT NULL DEFAULT 0,
    watchdog_deadline_ms INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE run_artifacts (
    artifact_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    item_id TEXT NOT NULL DEFAULT '',
    name TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'other' CHECK(kind IN ('log','screenshot','trace','junit','perf_stats','har','other')),
    rel_path TEXT NOT NULL,
    sha256 TEXT NOT NULL DEFAULT '',
    size_bytes INTEGER NOT NULL DEFAULT 0,
    mime TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_run_artifacts_run ON run_artifacts(run_id);
CREATE INDEX idx_run_artifacts_item ON run_artifacts(item_id);
ALTER TABLE sources ADD COLUMN secret_enc TEXT NOT NULL DEFAULT '';
CREATE TABLE test_runs_v3 (
    run_id TEXT PRIMARY KEY,
    run_no INTEGER NOT NULL UNIQUE,
    name TEXT NOT NULL,
    suite_id TEXT NOT NULL DEFAULT '',
    run_type TEXT NOT NULL DEFAULT 'manual',
    environment_id TEXT NOT NULL DEFAULT '',
    env_note TEXT NOT NULL DEFAULT '',
    assignment_mode TEXT NOT NULL CHECK(assignment_mode IN ('single','per_case','pool')),
    status TEXT NOT NULL DEFAULT 'running' CHECK(status IN ('running','completed','cancelled','error')),
    closed_by TEXT NOT NULL DEFAULT '',
    created_by TEXT NOT NULL,
    started_at TEXT NOT NULL DEFAULT (datetime('now')),
    finished_at TEXT
);
INSERT INTO test_runs_v3 (run_id, run_no, name, suite_id, run_type, environment_id,
    env_note, assignment_mode, status, closed_by, created_by, started_at, finished_at)
SELECT run_id, run_no, name, suite_id, run_type, environment_id,
    env_note, assignment_mode, status, closed_by, created_by, started_at, finished_at
FROM test_runs;
DROP TABLE test_runs;
ALTER TABLE test_runs_v3 RENAME TO test_runs;
CREATE TABLE test_run_items_v3 (
    item_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    case_id TEXT NOT NULL,
    case_title TEXT NOT NULL,
    case_version INTEGER NOT NULL,
    position INTEGER NOT NULL DEFAULT 0,
    assigned_to TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','in_progress','passed','failed','blocked','skipped','running','error')),
    result_note TEXT NOT NULL DEFAULT '',
    tester_config TEXT NOT NULL DEFAULT '',
    duration_secs INTEGER NOT NULL DEFAULT 0,
    attachments_json TEXT NOT NULL DEFAULT '[]',
    claimed_at TEXT,
    finished_at TEXT,
    UNIQUE(run_id, case_id)
);
INSERT INTO test_run_items_v3 (item_id, run_id, case_id, case_title, case_version,
    position, assigned_to, status, result_note, tester_config, duration_secs,
    attachments_json, claimed_at, finished_at)
SELECT item_id, run_id, case_id, case_title, case_version,
    position, assigned_to, status, result_note, tester_config, duration_secs,
    attachments_json, claimed_at, finished_at
FROM test_run_items;
DROP TABLE test_run_items;
ALTER TABLE test_run_items_v3 RENAME TO test_run_items;
CREATE INDEX idx_run_items_run ON test_run_items(run_id, position);
CREATE INDEX idx_run_items_assignee ON test_run_items(assigned_to, status);
";

/// F4: run schedules with their trigger history and ML Studio links.
///
/// Pure additive migration — no table is rebuilt, because no existing CHECK
/// changes: `test_runs.run_type` has never been constrained (so the new 'perf'
/// schedules write it freely) and `tasks.status` already allows exactly the
/// four kanban columns.
///
/// `next_run_at` is NULLABLE on purpose: the loop selects `next_run_at <= now`
/// and an empty string would sort BEFORE every timestamp, making a finished
/// one-shot schedule permanently "due". NULL never satisfies the comparison.
///
/// `assignment_mode` keeps the run-level CHECK; automated and perf schedules
/// store 'pool' (what `create_auto_run` writes), manual ones the tester's choice.
const PROJECT_SCHEMA_V4: &str = "
CREATE TABLE schedules (
    schedule_id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    auto_disabled INTEGER NOT NULL DEFAULT 0,
    run_type TEXT NOT NULL DEFAULT 'manual' CHECK(run_type IN ('manual','auto','perf')),
    suite_id TEXT NOT NULL DEFAULT '',
    case_ids_json TEXT NOT NULL DEFAULT '[]',
    environment_id TEXT NOT NULL DEFAULT '',
    runner_service_id TEXT NOT NULL DEFAULT '',
    perf_profile_json TEXT NOT NULL DEFAULT '{}',
    assignment_mode TEXT NOT NULL DEFAULT 'pool' CHECK(assignment_mode IN ('single','per_case','pool')),
    assignees_json TEXT NOT NULL DEFAULT '[]',
    schedule_kind TEXT NOT NULL CHECK(schedule_kind IN ('once','interval','cron')),
    schedule_expr TEXT NOT NULL,
    timezone TEXT NOT NULL DEFAULT 'UTC',
    next_run_at TEXT,
    last_trigger_at TEXT NOT NULL DEFAULT '',
    last_run_id TEXT NOT NULL DEFAULT '',
    last_status TEXT NOT NULL DEFAULT '',
    last_reason TEXT NOT NULL DEFAULT '',
    consecutive_failures INTEGER NOT NULL DEFAULT 0,
    created_by TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_schedules_due ON schedules(enabled, next_run_at);
CREATE TABLE schedule_runs (
    trigger_id TEXT PRIMARY KEY,
    schedule_id TEXT NOT NULL,
    scheduled_for TEXT NOT NULL DEFAULT '',
    fired_at TEXT NOT NULL DEFAULT (datetime('now')),
    outcome TEXT NOT NULL CHECK(outcome IN ('started','skipped','blocked','error')),
    reason TEXT NOT NULL DEFAULT '',
    run_id TEXT NOT NULL DEFAULT '',
    run_status TEXT NOT NULL DEFAULT '',
    actor TEXT NOT NULL DEFAULT ''
);
CREATE INDEX idx_schedule_runs_schedule ON schedule_runs(schedule_id, fired_at DESC);
CREATE TABLE ml_links (
    link_id TEXT PRIMARY KEY,
    ml_project_id TEXT NOT NULL UNIQUE,
    label TEXT NOT NULL DEFAULT '',
    origin TEXT NOT NULL CHECK(origin IN ('created_from_project','linked_existing')),
    sync_permissions INTEGER NOT NULL DEFAULT 0,
    role_map_json TEXT NOT NULL DEFAULT '[]',
    last_sync_at TEXT NOT NULL DEFAULT '',
    last_sync_result TEXT NOT NULL DEFAULT '',
    created_by TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
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

    /// Golden round-trip for the v3 12-step rebuild: a genuine v2 database
    /// with a run + items in every legacy status must migrate to v3 with all
    /// rows intact, the extended status CHECKs accepting the new
    /// 'error'/'running' values, the UNIQUE constraints and indexes
    /// recreated, and the new F3 tables/columns present.
    #[test]
    fn migration_v3_rebuilds_run_tables_preserving_data() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("proj");
        std::fs::create_dir_all(&dir).expect("create project dir");

        // Build a real v2 database the same way run_project_migrations does,
        // stopping after migration 2, then seed data that must survive.
        {
            let conn = Connection::open(dir.join("project.db")).expect("open v2");
            conn.execute_batch(
                "CREATE TABLE project_schema_version (
                    version INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL DEFAULT (datetime('now'))
                );",
            )
            .expect("version table");
            for (version, sql) in &MIGRATIONS_PROJECT[..2] {
                assert!(*version <= 2, "test must seed a v2 database");
                conn.execute_batch(sql).expect("apply v1/v2");
                conn.execute(
                    "INSERT INTO project_schema_version (version) VALUES (?1)",
                    rusqlite::params![version],
                )
                .expect("record version");
            }
            conn.execute(
                "INSERT INTO sources (source_id, kind, name, created_by) \
                 VALUES ('s1', 'git', 'repo', 'u1')",
                [],
            )
            .expect("insert source");
            conn.execute(
                "INSERT INTO test_runs (run_id, run_no, name, suite_id, assignment_mode, \
                 status, created_by) \
                 VALUES ('r1', 1, 'regression', 's1', 'pool', 'completed', 'u1')",
                [],
            )
            .expect("insert run");
            for (id, status) in [
                ("i1", "pending"),
                ("i2", "in_progress"),
                ("i3", "passed"),
                ("i4", "failed"),
                ("i5", "blocked"),
                ("i6", "skipped"),
            ] {
                conn.execute(
                    "INSERT INTO test_run_items (item_id, run_id, case_id, case_title, \
                     case_version, position, status, result_note) \
                     VALUES (?1, 'r1', 'c-' || ?1, 'title', 2, 7, ?2, 'note')",
                    rusqlite::params![id, status],
                )
                .expect("insert item");
            }
        }

        let (pool, version) = open_pool_at(&dir).expect("migrate to v3");
        assert_eq!(version, 4, "a seeded v2 database reaches the latest schema");
        let conn = pool.write().expect("write");

        // Every pre-rebuild row survived with its values intact.
        let (name, status, run_type): (String, String, String) = conn
            .query_row(
                "SELECT name, status, run_type FROM test_runs WHERE run_id = 'r1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("run row");
        assert_eq!(name, "regression");
        assert_eq!(status, "completed");
        assert_eq!(run_type, "manual");
        let items: i64 = conn
            .query_row("SELECT COUNT(*) FROM test_run_items", [], |r| r.get(0))
            .expect("item count");
        assert_eq!(items, 6);
        let (i4_status, i4_note, i4_version): (String, String, i64) = conn
            .query_row(
                "SELECT status, result_note, case_version FROM test_run_items \
                 WHERE item_id = 'i4'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("item row");
        assert_eq!(i4_status, "failed");
        assert_eq!(i4_note, "note");
        assert_eq!(i4_version, 2);

        // The extended CHECKs accept the new automated-run statuses…
        conn.execute(
            "INSERT INTO test_runs (run_id, run_no, name, assignment_mode, status, created_by) \
             VALUES ('r2', 2, 'auto', 'pool', 'error', 'u1')",
            [],
        )
        .expect("run status 'error' accepted");
        for (id, status) in [("a1", "running"), ("a2", "error")] {
            conn.execute(
                "INSERT INTO test_run_items (item_id, run_id, case_id, case_title, \
                 case_version, status) VALUES (?1, 'r2', 'c-' || ?1, 't', 1, ?2)",
                rusqlite::params![id, status],
            )
            .expect("item status accepted");
        }
        // …while junk statuses are still rejected.
        assert!(conn
            .execute(
                "INSERT INTO test_runs (run_id, run_no, name, assignment_mode, status, \
                 created_by) VALUES ('r3', 3, 'x', 'pool', 'bogus', 'u1')",
                [],
            )
            .is_err());
        assert!(conn
            .execute(
                "INSERT INTO test_run_items (item_id, run_id, case_id, case_title, \
                 case_version, status) VALUES ('a3', 'r2', 'c-a3', 't', 1, 'bogus')",
                [],
            )
            .is_err());

        // UNIQUE constraints survived the rebuild.
        assert!(conn
            .execute(
                "INSERT INTO test_runs (run_id, run_no, name, assignment_mode, created_by) \
                 VALUES ('r4', 1, 'dup-no', 'pool', 'u1')",
                [],
            )
            .is_err());
        assert!(conn
            .execute(
                "INSERT INTO test_run_items (item_id, run_id, case_id, case_title, \
                 case_version) VALUES ('a4', 'r1', 'c-i1', 't', 1)",
                [],
            )
            .is_err());

        // Explicit indexes were recreated after the drop+rename.
        for idx in ["idx_run_items_run", "idx_run_items_assignee"] {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
                    rusqlite::params![idx],
                    |r| r.get(0),
                )
                .expect("index lookup");
            assert_eq!(n, 1, "index {idx} missing after rebuild");
        }

        // New F3 tables exist and sources gained secret_enc.
        for table in [
            "environments",
            "build_profiles",
            "auto_run_meta",
            "run_artifacts",
        ] {
            let n: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .expect("new table queryable");
            assert_eq!(n, 0);
        }
        let secret: String = conn
            .query_row(
                "SELECT secret_enc FROM sources WHERE source_id = 's1'",
                [],
                |r| r.get(0),
            )
            .expect("secret_enc column");
        assert_eq!(secret, "");
    }

    /// v3 → v4 on a REAL v3 database: existing rows survive, the three new
    /// tables are queryable with their CHECKs and UNIQUE in force, and the
    /// tables F4 writes through (`test_runs.run_type = 'perf'`, the four
    /// kanban `tasks.status` values) still accept their values WITHOUT a
    /// rebuild — that is the whole reason this migration is purely additive.
    #[test]
    fn migration_v4_adds_schedule_and_ml_tables() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("proj");
        std::fs::create_dir_all(&dir).expect("create project dir");

        // Seed a genuine v3 database, then put rows in it that must survive.
        {
            let conn = Connection::open(dir.join("project.db")).expect("open v3");
            conn.execute_batch(
                "CREATE TABLE project_schema_version (
                    version INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL DEFAULT (datetime('now'))
                );",
            )
            .expect("version table");
            for (version, sql) in &MIGRATIONS_PROJECT[..3] {
                assert!(*version <= 3, "test must seed a v3 database");
                conn.execute_batch(sql).expect("apply v1..v3");
                conn.execute(
                    "INSERT INTO project_schema_version (version) VALUES (?1)",
                    rusqlite::params![version],
                )
                .expect("record version");
            }
            conn.execute(
                "INSERT INTO test_cases (case_id, title, created_by) \
                 VALUES ('c1', 'logowanie', 'u1')",
                [],
            )
            .expect("insert case");
            conn.execute(
                "INSERT INTO tasks (task_id, task_no, title, status, created_by) \
                 VALUES ('t1', 1, 'poprawka', 'review', 'u1')",
                [],
            )
            .expect("insert task");
            conn.execute(
                "INSERT INTO environments (environment_id, name, base_url, requested_by) \
                 VALUES ('e1', 'staging', 'https://example.test', 'u1')",
                [],
            )
            .expect("insert environment");
        }

        let (pool, version) = open_pool_at(&dir).expect("migrate to v4");
        assert_eq!(version, 4);
        let conn = pool.write().expect("write");

        // Pre-migration rows are untouched.
        let title: String = conn
            .query_row(
                "SELECT title FROM test_cases WHERE case_id = 'c1'",
                [],
                |r| r.get(0),
            )
            .expect("case row");
        assert_eq!(title, "logowanie");
        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE task_id = 't1'", [], |r| {
                r.get(0)
            })
            .expect("task row");
        assert_eq!(status, "review");
        let env_name: String = conn
            .query_row(
                "SELECT name FROM environments WHERE environment_id = 'e1'",
                [],
                |r| r.get(0),
            )
            .expect("environment row");
        assert_eq!(env_name, "staging");

        // The new tables exist and are queryable.
        for table in ["schedules", "schedule_runs", "ml_links"] {
            let n: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .expect("new table queryable");
            assert_eq!(n, 0);
        }

        conn.execute(
            "INSERT INTO schedules (schedule_id, name, run_type, schedule_kind, \
             schedule_expr, timezone, next_run_at, created_by) \
             VALUES ('sc1', 'nocny', 'perf', 'cron', '30 2 * * *', 'Europe/Warsaw', \
             '2026-08-01T00:30:00Z', 'u1')",
            [],
        )
        .expect("insert schedule");
        // A finished one-shot keeps next_run_at NULL, so the due query never
        // picks it up again (an empty string would sort before every date).
        conn.execute(
            "INSERT INTO schedules (schedule_id, name, run_type, schedule_kind, \
             schedule_expr, enabled, next_run_at, created_by) \
             VALUES ('sc2', 'jednorazowy', 'auto', 'once', '2026-01-01T00:00:00Z', 0, \
             NULL, 'u1')",
            [],
        )
        .expect("insert one-shot schedule");
        let due: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM schedules WHERE enabled = 1 AND auto_disabled = 0 \
                 AND next_run_at <= '2027-01-01T00:00:00Z'",
                [],
                |r| r.get(0),
            )
            .expect("due query");
        assert_eq!(due, 1, "only the enabled schedule with a date is due");

        // CHECK constraints reject junk in every constrained column.
        for sql in [
            "INSERT INTO schedules (schedule_id, name, run_type, schedule_kind, \
             schedule_expr, created_by) VALUES ('x1', 'n', 'bogus', 'cron', '* * * * *', 'u1')",
            "INSERT INTO schedules (schedule_id, name, schedule_kind, schedule_expr, \
             created_by) VALUES ('x2', 'n', 'weekly', '* * * * *', 'u1')",
            "INSERT INTO schedules (schedule_id, name, assignment_mode, schedule_kind, \
             schedule_expr, created_by) VALUES ('x3', 'n', 'nobody', 'cron', '* * * * *', 'u1')",
        ] {
            assert!(conn.execute(sql, []).is_err(), "CHECK accepted junk: {sql}");
        }

        conn.execute(
            "INSERT INTO schedule_runs (trigger_id, schedule_id, scheduled_for, outcome, \
             reason) VALUES ('tr1', 'sc1', '2026-08-01T00:30:00Z', 'blocked', \
             'srodowisko niezatwierdzone')",
            [],
        )
        .expect("insert trigger");
        assert!(
            conn.execute(
                "INSERT INTO schedule_runs (trigger_id, schedule_id, outcome) \
                 VALUES ('tr2', 'sc1', 'exploded')",
                [],
            )
            .is_err(),
            "schedule_runs.outcome CHECK accepted junk"
        );

        conn.execute(
            "INSERT INTO ml_links (link_id, ml_project_id, origin, created_by) \
             VALUES ('l1', 'ml1', 'linked_existing', 'u1')",
            [],
        )
        .expect("insert ml link");
        assert!(
            conn.execute(
                "INSERT INTO ml_links (link_id, ml_project_id, origin, created_by) \
                 VALUES ('l2', 'ml1', 'created_from_project', 'u1')",
                [],
            )
            .is_err(),
            "ml_links.ml_project_id UNIQUE not enforced"
        );
        assert!(
            conn.execute(
                "INSERT INTO ml_links (link_id, ml_project_id, origin, created_by) \
                 VALUES ('l3', 'ml2', 'stolen', 'u1')",
                [],
            )
            .is_err(),
            "ml_links.origin CHECK accepted junk"
        );

        for idx in ["idx_schedules_due", "idx_schedule_runs_schedule"] {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
                    rusqlite::params![idx],
                    |r| r.get(0),
                )
                .expect("index lookup");
            assert_eq!(n, 1, "index {idx} missing");
        }

        // No rebuild was needed: the tables F4 writes through already accept
        // every value it produces.
        conn.execute(
            "INSERT INTO test_runs (run_id, run_no, name, run_type, assignment_mode, \
             created_by) VALUES ('r1', 1, 'perf nocny', 'perf', 'pool', 'u1')",
            [],
        )
        .expect("test_runs.run_type accepts 'perf' without a rebuild");
        for (id, no, status) in [
            ("k1", 2, "todo"),
            ("k2", 3, "in_progress"),
            ("k3", 4, "review"),
            ("k4", 5, "done"),
        ] {
            conn.execute(
                "INSERT INTO tasks (task_id, task_no, title, status, created_by) \
                 VALUES (?1, ?2, 'karta', ?3, 'u1')",
                rusqlite::params![id, no, status],
            )
            .expect("tasks.status accepts every kanban column without a rebuild");
        }
    }
}
