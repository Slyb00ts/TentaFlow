// ===== File: tentaquant/db.rs — schema and rows of one lab's tentaquant.db =====
//
// One INSTANCE of the package is one laboratory, and every laboratory keeps its
// whole content in its own `<instance data dir>/tentaquant.db` (plan §9.2). The
// main database holds only the platform layer — the `addons` row, the package
// catalog and the permission matrix — so two labs on one node share nothing but
// the code, and uninstalling one wipes exactly one directory.
//
// There is deliberately NO member table: who belongs to a lab is the instance's
// permission matrix (§10.1). `project_shares` is a different thing entirely —
// it is the ML-Studio ownership model INSIDE a lab (§18 decision 15), and a
// share row is dormant until the matrix also grants that person `quant.read`.
//
// The schema is append-only: a change is a new migration step, never an edit to
// an applied one.

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use rusqlite::{params, Connection, OptionalExtension};

use crate::addon::app_db;
use crate::db::DbPool;

/// Package id of the TentaQuant native app, as declared in `app-manifest.toml`.
pub const PACKAGE_ID: &str = "tentaquant";

/// Schema steps, applied once each by `app_db::run_versioned_migrations`.
///
/// Only the tables the current phase actually writes exist here. Providers,
/// QPU budgets, ledgers, approvals, examples and kernel sessions arrive with
/// the phases that own them — an empty table is a promise the code cannot keep.
const STEPS: &[(i64, &str)] = &[(
    1,
    "
CREATE TABLE user_settings (
    user_id TEXT PRIMARY KEY,
    settings_json TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE projects (
    id TEXT PRIMARY KEY,
    owner_user_id TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    visibility TEXT NOT NULL CHECK(visibility IN ('private','lab')),
    linked_project_id TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    archived_at TEXT
);
CREATE INDEX idx_tq_projects_owner ON projects(owner_user_id);
CREATE INDEX idx_tq_projects_visibility ON projects(visibility);
CREATE TABLE project_shares (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL,
    role TEXT NOT NULL CHECK(role IN ('editor','viewer')),
    granted_by TEXT NOT NULL,
    granted_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (project_id, user_id)
);
CREATE INDEX idx_tq_shares_user ON project_shares(user_id);
CREATE TABLE files (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('notebook','py','qasm','data','md')),
    sha256 TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (project_id, path)
);
CREATE INDEX idx_tq_files_project ON files(project_id);
CREATE TABLE notebooks (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    file_id TEXT NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    current_version INTEGER NOT NULL,
    updated_by TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_tq_notebooks_project ON notebooks(project_id);
CREATE TABLE notebook_versions (
    notebook_id TEXT NOT NULL REFERENCES notebooks(id) ON DELETE CASCADE,
    version INTEGER NOT NULL,
    cells_json TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    author TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (notebook_id, version)
);
CREATE TABLE cell_outputs (
    run_id TEXT NOT NULL,
    cell_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    mime_json TEXT NOT NULL,
    artifact_sha256 TEXT,
    PRIMARY KEY (run_id, cell_id, seq)
);
CREATE TABLE runs (
    id TEXT PRIMARY KEY,
    project_id TEXT REFERENCES projects(id) ON DELETE CASCADE,
    notebook_id TEXT,
    cell_id TEXT,
    kind TEXT NOT NULL CHECK(kind IN ('cell','circuit','program','kata','flow')),
    target TEXT NOT NULL,
    node_id TEXT,
    status TEXT NOT NULL,
    started_at TEXT NOT NULL DEFAULT (datetime('now')),
    ended_at TEXT,
    error TEXT,
    metrics_json TEXT,
    user_id TEXT NOT NULL,
    pinned_at TEXT,
    thumbnail_sha256 TEXT,
    keyframes_sha256 TEXT
);
CREATE INDEX idx_tq_runs_project ON runs(project_id);
CREATE INDEX idx_tq_runs_user_started ON runs(user_id, started_at);
CREATE TABLE kata_progress (
    user_id TEXT NOT NULL,
    kata_id TEXT NOT NULL,
    status TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    best_score REAL,
    points INTEGER NOT NULL DEFAULT 0,
    last_run_id TEXT,
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (user_id, kata_id)
);
CREATE TABLE settings (
    key TEXT PRIMARY KEY,
    value_json TEXT NOT NULL
);
",
)];

/// Brings a lab's content database up to date. Idempotent: the versioned
/// runner skips applied steps, so the install hook and every first open of the
/// process may call it.
pub fn migrate(conn: &Connection) -> Result<()> {
    app_db::run_versioned_migrations(conn, PACKAGE_ID, STEPS)
}

fn read_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("tentaquant db read: {e}")
}

fn write_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("tentaquant db write: {e}")
}

// =============================================================================
// Settings
// =============================================================================

/// Keys of the two settings documents. Two rows, not one, because the halves
/// have different readers: everyone may read the operational document (the
/// `device="auto"` rule needs the qubit ceilings), while the admin document —
/// isolation, retention, the trusted-native acknowledgement — is `quant.admin`
/// alone (§10.2). Each half is stored whole: the form edits a document, and a
/// per-field layout would let a partial write leave a lab half-configured.
const SETTINGS_KEY: &str = "lab";
const ADMIN_SETTINGS_KEY: &str = "lab_admin";

/// One stored settings document, or its defaults when nothing has been saved
/// yet. A row that fails to parse (written by a newer build) also falls back to
/// the defaults rather than failing the whole screen.
fn settings_doc<T: serde::de::DeserializeOwned + Default>(pool: &DbPool, key: &str) -> Result<T> {
    let conn = pool.read().map_err(read_err)?;
    let stored: Option<String> = conn
        .query_row(
            "SELECT value_json FROM settings WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()
        .map_err(read_err)?;
    Ok(stored
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default())
}

fn set_settings_doc<T: serde::Serialize>(pool: &DbPool, key: &str, value: &T) -> Result<()> {
    let json = serde_json::to_string(value)?;
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "INSERT INTO settings (key, value_json) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
        params![key, json],
    )
    .map_err(write_err)?;
    Ok(())
}

pub fn settings(pool: &DbPool) -> Result<tentaflow_protocol::tentaquant::LabSettings> {
    settings_doc(pool, SETTINGS_KEY)
}

pub fn set_settings(
    pool: &DbPool,
    value: &tentaflow_protocol::tentaquant::LabSettings,
) -> Result<()> {
    set_settings_doc(pool, SETTINGS_KEY, value)
}

pub fn admin_settings(pool: &DbPool) -> Result<tentaflow_protocol::tentaquant::LabAdminSettings> {
    settings_doc(pool, ADMIN_SETTINGS_KEY)
}

pub fn set_admin_settings(
    pool: &DbPool,
    value: &tentaflow_protocol::tentaquant::LabAdminSettings,
) -> Result<()> {
    set_settings_doc(pool, ADMIN_SETTINGS_KEY, value)
}

// =============================================================================
// Projects
// =============================================================================

/// A project row as stored. Access is NOT part of it — it is derived per
/// caller by [`access`].
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectRecord {
    pub id: String,
    pub owner_user_id: String,
    pub name: String,
    pub description: String,
    pub visibility: String,
    pub linked_project_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
}

const PROJECT_COLS: &str = "id, owner_user_id, name, description, visibility, \
     linked_project_id, created_at, updated_at, archived_at";

fn read_project(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectRecord> {
    Ok(ProjectRecord {
        id: row.get(0)?,
        owner_user_id: row.get(1)?,
        name: row.get(2)?,
        description: row.get(3)?,
        visibility: row.get(4)?,
        linked_project_id: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
        archived_at: row.get(8)?,
    })
}

/// What a caller may do with one project (plan §3.1, §18 decision 15).
///
/// There is no supervisor or admin entry: `quant.instruct` sees run metadata
/// and course progress, never the content of someone else's private project,
/// and this enum is the only thing standing between a request and that content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectRole {
    Owner,
    Editor,
    Viewer,
}

impl ProjectRole {
    pub fn as_str(self) -> &'static str {
        match self {
            ProjectRole::Owner => "owner",
            ProjectRole::Editor => "editor",
            ProjectRole::Viewer => "viewer",
        }
    }

    /// Whether the role may change project content (files, notebooks).
    /// A `viewer` may read and run in the browser, never write (§10.3).
    pub fn may_write(self) -> bool {
        matches!(self, ProjectRole::Owner | ProjectRole::Editor)
    }

    pub fn parse_share(role: &str) -> Option<Self> {
        match role {
            "editor" => Some(ProjectRole::Editor),
            "viewer" => Some(ProjectRole::Viewer),
            _ => None,
        }
    }
}

/// The caller's role on one project, or `None` when the project does not exist
/// FOR THEM — the caller must not be able to tell those two apart.
pub fn access(pool: &DbPool, project_id: &str, user_id: &str) -> Result<Option<ProjectRole>> {
    let conn = pool.read().map_err(read_err)?;
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT owner_user_id, visibility FROM projects WHERE id = ?1",
            params![project_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(read_err)?;
    let Some((owner, visibility)) = row else {
        return Ok(None);
    };
    let share: Option<String> = conn
        .query_row(
            "SELECT role FROM project_shares WHERE project_id = ?1 AND user_id = ?2",
            params![project_id, user_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(read_err)?;
    Ok(resolve_role(
        &owner,
        &visibility,
        user_id,
        share.as_deref().and_then(ProjectRole::parse_share),
    ))
}

/// THE ownership rule of a project (§18 decision 15), in one place: the owner
/// outranks a share, a share outranks publication, and a project published to
/// the lab is read-only for every member — which is what the plan's
/// "supervisor's course material" is. Both [`access`] (one project, one share
/// query) and [`role_of`] (a listing, one share query for all of them) apply
/// exactly this, so a listing can never show a role a request would refuse.
fn resolve_role(
    owner_user_id: &str,
    visibility: &str,
    user_id: &str,
    share: Option<ProjectRole>,
) -> Option<ProjectRole> {
    if owner_user_id == user_id {
        return Some(ProjectRole::Owner);
    }
    if share.is_some() {
        return share;
    }
    if visibility == "lab" {
        return Some(ProjectRole::Viewer);
    }
    None
}

/// The caller's role on a record already in hand, given their share rows from
/// [`shared_roles`].
pub fn role_of(
    record: &ProjectRecord,
    user_id: &str,
    shares: &HashMap<String, ProjectRole>,
) -> Option<ProjectRole> {
    resolve_role(
        &record.owner_user_id,
        &record.visibility,
        user_id,
        shares.get(&record.id).copied(),
    )
}

/// One project by id, regardless of access — callers must have resolved
/// [`access`] first.
pub fn project(pool: &DbPool, project_id: &str) -> Result<Option<ProjectRecord>> {
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        &format!("SELECT {PROJECT_COLS} FROM projects WHERE id = ?1"),
        params![project_id],
        read_project,
    )
    .optional()
    .map_err(read_err)
}

/// Projects the caller may see: own ∪ shared with them ∪ published to the lab.
pub fn list_projects(
    pool: &DbPool,
    user_id: &str,
    include_archived: bool,
) -> Result<Vec<ProjectRecord>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {PROJECT_COLS} FROM projects p \
             WHERE (p.owner_user_id = ?1 \
                    OR p.visibility = 'lab' \
                    OR EXISTS (SELECT 1 FROM project_shares s \
                               WHERE s.project_id = p.id AND s.user_id = ?1)) \
               AND (?2 = 1 OR p.archived_at IS NULL) \
             ORDER BY p.updated_at DESC"
        ))
        .map_err(read_err)?;
    let rows = stmt
        .query_map(
            params![user_id, if include_archived { 1 } else { 0 }],
            read_project,
        )
        .map_err(read_err)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(read_err)?;
    Ok(rows)
}

/// Counts for the dashboard KPIs, in one pass over the caller's visible set:
/// `(owned, shared with me, published to the lab)`. The three PARTITION that
/// set — a project both shared with the caller and published to the lab counts
/// once, as shared — so the KPI row adds up to what [`list_projects`] returns.
pub fn project_counts(pool: &DbPool, user_id: &str) -> Result<(u32, u32, u32)> {
    let conn = pool.read().map_err(read_err)?;
    let row = conn
        .query_row(
            "SELECT \
               (SELECT COUNT(*) FROM projects \
                 WHERE owner_user_id = ?1 AND archived_at IS NULL), \
               (SELECT COUNT(*) FROM projects p JOIN project_shares s ON s.project_id = p.id \
                 WHERE s.user_id = ?1 AND p.owner_user_id <> ?1 AND p.archived_at IS NULL), \
               (SELECT COUNT(*) FROM projects p \
                 WHERE p.visibility = 'lab' AND p.owner_user_id <> ?1 \
                   AND p.archived_at IS NULL \
                   AND NOT EXISTS (SELECT 1 FROM project_shares s \
                                   WHERE s.project_id = p.id AND s.user_id = ?1))",
            params![user_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)? as u32,
                    row.get::<_, i64>(1)? as u32,
                    row.get::<_, i64>(2)? as u32,
                ))
            },
        )
        .map_err(read_err)?;
    Ok(row)
}

/// The caller's runs of the last seven days: `(total, succeeded, failed,
/// running)`. "Running" is every state that is neither terminal outcome, so a
/// run parked in `awaiting_approval` still shows up as in flight.
pub fn run_counts_7d(pool: &DbPool, user_id: &str) -> Result<(u32, u32, u32, u32)> {
    let conn = pool.read().map_err(read_err)?;
    let row = conn
        .query_row(
            "SELECT COUNT(*), \
                    SUM(status = 'succeeded'), \
                    SUM(status = 'failed'), \
                    SUM(status NOT IN ('succeeded','failed','cancelled')) \
             FROM runs WHERE user_id = ?1 AND started_at >= datetime('now', '-7 days')",
            params![user_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)? as u32,
                    row.get::<_, Option<i64>>(1)?.unwrap_or(0) as u32,
                    row.get::<_, Option<i64>>(2)?.unwrap_or(0) as u32,
                    row.get::<_, Option<i64>>(3)?.unwrap_or(0) as u32,
                ))
            },
        )
        .map_err(read_err)?;
    Ok(row)
}

/// Latest change to any project the caller can see — the "last activity" line
/// of the lab tile.
pub fn last_activity(pool: &DbPool, user_id: &str) -> Result<Option<String>> {
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        "SELECT MAX(p.updated_at) FROM projects p \
         WHERE p.owner_user_id = ?1 OR p.visibility = 'lab' \
            OR EXISTS (SELECT 1 FROM project_shares s \
                       WHERE s.project_id = p.id AND s.user_id = ?1)",
        params![user_id],
        |row| row.get::<_, Option<String>>(0),
    )
    .optional()
    .map(Option::flatten)
    .map_err(read_err)
}

/// Counters one project's row shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProjectStats {
    pub shares: u32,
    pub files: u32,
    pub notebooks: u32,
    pub runs: u32,
}

/// Counters of ONE project — the answer to a create/update/get, where a single
/// row is all the caller asked for.
pub fn project_stats(pool: &DbPool, project_id: &str) -> Result<ProjectStats> {
    let conn = pool.read().map_err(read_err)?;
    let row = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM project_shares WHERE project_id = ?1), \
                    (SELECT COUNT(*) FROM files WHERE project_id = ?1), \
                    (SELECT COUNT(*) FROM notebooks WHERE project_id = ?1), \
                    (SELECT COUNT(*) FROM runs WHERE project_id = ?1)",
            params![project_id],
            |row| {
                Ok(ProjectStats {
                    shares: row.get::<_, i64>(0)? as u32,
                    files: row.get::<_, i64>(1)? as u32,
                    notebooks: row.get::<_, i64>(2)? as u32,
                    runs: row.get::<_, i64>(3)? as u32,
                })
            },
        )
        .map_err(read_err)?;
    Ok(row)
}

/// Counters of every project of the lab, keyed by project id: FOUR grouped
/// queries whatever the list length, instead of one query per listed row.
/// Projects with no content of a kind are simply absent from that pass, so the
/// caller reads a [`ProjectStats::default`] for them.
pub fn project_stats_all(pool: &DbPool) -> Result<HashMap<String, ProjectStats>> {
    let conn = pool.read().map_err(read_err)?;
    let mut out: HashMap<String, ProjectStats> = HashMap::new();
    let passes: [(&str, fn(&mut ProjectStats, u32)); 4] = [
        (
            "SELECT project_id, COUNT(*) FROM project_shares GROUP BY project_id",
            |s, n| s.shares = n,
        ),
        (
            "SELECT project_id, COUNT(*) FROM files GROUP BY project_id",
            |s, n| s.files = n,
        ),
        (
            "SELECT project_id, COUNT(*) FROM notebooks GROUP BY project_id",
            |s, n| s.notebooks = n,
        ),
        (
            "SELECT project_id, COUNT(*) FROM runs WHERE project_id IS NOT NULL \
             GROUP BY project_id",
            |s, n| s.runs = n,
        ),
    ];
    for (sql, apply) in passes {
        let mut stmt = conn.prepare(sql).map_err(read_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u32))
            })
            .map_err(read_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(read_err)?;
        for (project_id, count) in rows {
            apply(out.entry(project_id).or_default(), count);
        }
    }
    Ok(out)
}

/// The caller's share roles across the whole lab, keyed by project id — the
/// list view resolves a row's role from this map plus the owner and visibility
/// it already holds, instead of re-reading the project per row.
pub fn shared_roles(pool: &DbPool, user_id: &str) -> Result<HashMap<String, ProjectRole>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn
        .prepare("SELECT project_id, role FROM project_shares WHERE user_id = ?1")
        .map_err(read_err)?;
    let rows = stmt
        .query_map(params![user_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(read_err)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(read_err)?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, role)| ProjectRole::parse_share(&role).map(|r| (id, r)))
        .collect())
}

pub fn create_project(
    pool: &DbPool,
    owner_user_id: &str,
    name: &str,
    description: &str,
    visibility: &str,
    linked_project_id: Option<&str>,
) -> Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "INSERT INTO projects (id, owner_user_id, name, description, visibility, linked_project_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, owner_user_id, name, description, visibility, linked_project_id],
    )
    .map_err(write_err)?;
    Ok(id)
}

pub fn update_project(
    pool: &DbPool,
    project_id: &str,
    name: &str,
    description: &str,
    visibility: &str,
    linked_project_id: Option<&str>,
) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "UPDATE projects SET name = ?1, description = ?2, visibility = ?3, \
            linked_project_id = ?4, updated_at = datetime('now') WHERE id = ?5",
        params![name, description, visibility, linked_project_id, project_id],
    )
    .map_err(write_err)?;
    Ok(())
}

pub fn set_project_archived(pool: &DbPool, project_id: &str, archived: bool) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "UPDATE projects SET archived_at = CASE WHEN ?1 = 1 THEN datetime('now') ELSE NULL END, \
            updated_at = datetime('now') WHERE id = ?2",
        params![if archived { 1 } else { 0 }, project_id],
    )
    .map_err(write_err)?;
    Ok(())
}

/// Hands the project over. The new owner's share row (if any) is dropped: a
/// person cannot be both owner and shared-with, and leaving the row would make
/// `access` answer with the weaker role on the next lookup.
pub fn transfer_project(pool: &DbPool, project_id: &str, new_owner_user_id: &str) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction().map_err(write_err)?;
    tx.execute(
        "DELETE FROM project_shares WHERE project_id = ?1 AND user_id = ?2",
        params![project_id, new_owner_user_id],
    )
    .map_err(write_err)?;
    tx.execute(
        "UPDATE projects SET owner_user_id = ?1, updated_at = datetime('now') WHERE id = ?2",
        params![new_owner_user_id, project_id],
    )
    .map_err(write_err)?;
    tx.commit().map_err(write_err)?;
    Ok(())
}

/// Removes the project and everything hanging off it. The CAS blobs are NOT
/// removed here: they are content-addressed and may be referenced by another
/// project of the same lab. Their lifetime belongs to the retention sweep,
/// which does not exist yet — see the note in `cas.rs`.
pub fn delete_project(pool: &DbPool, project_id: &str) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute("DELETE FROM projects WHERE id = ?1", params![project_id])
        .map_err(write_err)?;
    Ok(())
}

// =============================================================================
// Shares
// =============================================================================

#[derive(Debug, Clone, PartialEq)]
pub struct ShareRecord {
    pub user_id: String,
    pub role: String,
    pub granted_by: String,
    pub granted_at: String,
}

pub fn list_shares(pool: &DbPool, project_id: &str) -> Result<Vec<ShareRecord>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn
        .prepare(
            "SELECT user_id, role, granted_by, granted_at FROM project_shares \
             WHERE project_id = ?1 ORDER BY granted_at",
        )
        .map_err(read_err)?;
    let rows = stmt
        .query_map(params![project_id], |row| {
            Ok(ShareRecord {
                user_id: row.get(0)?,
                role: row.get(1)?,
                granted_by: row.get(2)?,
                granted_at: row.get(3)?,
            })
        })
        .map_err(read_err)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(read_err)?;
    Ok(rows)
}

pub fn set_share(
    pool: &DbPool,
    project_id: &str,
    user_id: &str,
    role: &str,
    granted_by: &str,
) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "INSERT INTO project_shares (project_id, user_id, role, granted_by) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(project_id, user_id) DO UPDATE \
            SET role = excluded.role, granted_by = excluded.granted_by, \
                granted_at = datetime('now')",
        params![project_id, user_id, role, granted_by],
    )
    .map_err(write_err)?;
    Ok(())
}

pub fn remove_share(pool: &DbPool, project_id: &str, user_id: &str) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "DELETE FROM project_shares WHERE project_id = ?1 AND user_id = ?2",
        params![project_id, user_id],
    )
    .map_err(write_err)?;
    Ok(())
}

// =============================================================================
// Files
// =============================================================================

#[derive(Debug, Clone, PartialEq)]
pub struct FileRecord {
    pub id: String,
    pub project_id: String,
    pub path: String,
    pub kind: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub updated_at: String,
}

const FILE_COLS: &str = "id, project_id, path, kind, sha256, size_bytes, updated_at";

fn read_file(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileRecord> {
    Ok(FileRecord {
        id: row.get(0)?,
        project_id: row.get(1)?,
        path: row.get(2)?,
        kind: row.get(3)?,
        sha256: row.get(4)?,
        size_bytes: row.get::<_, i64>(5)? as u64,
        updated_at: row.get(6)?,
    })
}

pub fn list_files(pool: &DbPool, project_id: &str) -> Result<Vec<FileRecord>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {FILE_COLS} FROM files WHERE project_id = ?1 ORDER BY path"
        ))
        .map_err(read_err)?;
    let rows = stmt
        .query_map(params![project_id], read_file)
        .map_err(read_err)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(read_err)?;
    Ok(rows)
}

/// Whether a project path is a notebook's backing row. The upload handler asks
/// BEFORE accepting a transfer, so a refusal costs the client nothing;
/// [`upsert_file`] asks again at the write, which is where the guarantee has to
/// hold.
pub fn path_backs_notebook(pool: &DbPool, project_id: &str, path: &str) -> Result<bool> {
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM notebooks n JOIN files f ON f.id = n.file_id \
         WHERE f.project_id = ?1 AND f.path = ?2)",
        params![project_id, path],
        |row| row.get(0),
    )
    .map_err(read_err)
}

/// Outcome of [`upsert_file`].
#[derive(Debug, Clone, PartialEq)]
pub enum FileUpsert {
    /// The row as stored, so the caller answers with the real `updated_at`
    /// rather than a value it made up.
    Stored(FileRecord),
    /// That path is a notebook's backing row. Overwriting it would leave
    /// `notebooks.file_id` pointing at a kind, sha256 and size that describe
    /// an unrelated blob — the same hole [`delete_file`] refuses, entered from
    /// the writing side. A notebook's content is changed by saving the
    /// notebook, never by uploading over its file.
    NotebookBacking,
}

/// Records a blob under `path`, replacing whatever that path pointed at.
pub fn upsert_file(
    pool: &DbPool,
    project_id: &str,
    path: &str,
    kind: &str,
    sha256: &str,
    size_bytes: u64,
) -> Result<FileUpsert> {
    let conn = pool.write().map_err(write_err)?;
    let backs_notebook: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM notebooks n JOIN files f ON f.id = n.file_id \
             WHERE f.project_id = ?1 AND f.path = ?2)",
            params![project_id, path],
            |row| row.get(0),
        )
        .map_err(read_err)?;
    if backs_notebook {
        return Ok(FileUpsert::NotebookBacking);
    }
    conn.execute(
        "INSERT INTO files (id, project_id, path, kind, sha256, size_bytes) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT(project_id, path) DO UPDATE \
            SET kind = excluded.kind, sha256 = excluded.sha256, \
                size_bytes = excluded.size_bytes, updated_at = datetime('now')",
        params![
            uuid::Uuid::new_v4().to_string(),
            project_id,
            path,
            kind,
            sha256,
            size_bytes as i64
        ],
    )
    .map_err(write_err)?;
    let record = conn
        .query_row(
            &format!("SELECT {FILE_COLS} FROM files WHERE project_id = ?1 AND path = ?2"),
            params![project_id, path],
            read_file,
        )
        .map_err(read_err)?;
    Ok(FileUpsert::Stored(record))
}

/// Outcome of [`delete_file`], so the caller can tell a missing file from a
/// refusal without a second query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileDeletion {
    Deleted,
    Missing,
    /// The row backs a notebook. `notebooks.file_id` cascades, so deleting it
    /// here would take the notebook AND its append-only version history with
    /// it — silently, since the caller asked about a file. A notebook is
    /// removed through a notebook-scoped request, never as a side effect.
    NotebookBacking,
}

pub fn delete_file(pool: &DbPool, project_id: &str, file_id: &str) -> Result<FileDeletion> {
    let conn = pool.write().map_err(write_err)?;
    let backs_notebook: Option<bool> = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM notebooks WHERE file_id = f.id) \
             FROM files f WHERE f.id = ?1 AND f.project_id = ?2",
            params![file_id, project_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(read_err)?;
    match backs_notebook {
        None => Ok(FileDeletion::Missing),
        Some(true) => Ok(FileDeletion::NotebookBacking),
        Some(false) => {
            conn.execute(
                "DELETE FROM files WHERE id = ?1 AND project_id = ?2",
                params![file_id, project_id],
            )
            .map_err(write_err)?;
            Ok(FileDeletion::Deleted)
        }
    }
}

// =============================================================================
// Notebooks
// =============================================================================

#[derive(Debug, Clone, PartialEq)]
pub struct NotebookRecord {
    pub id: String,
    pub project_id: String,
    pub file_id: String,
    pub name: String,
    pub current_version: u32,
    pub updated_by: String,
    pub updated_at: String,
}

const NOTEBOOK_COLS: &str =
    "id, project_id, file_id, name, current_version, updated_by, updated_at";

fn read_notebook(row: &rusqlite::Row<'_>) -> rusqlite::Result<NotebookRecord> {
    Ok(NotebookRecord {
        id: row.get(0)?,
        project_id: row.get(1)?,
        file_id: row.get(2)?,
        name: row.get(3)?,
        current_version: row.get::<_, i64>(4)? as u32,
        updated_by: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

pub fn list_notebooks(pool: &DbPool, project_id: &str) -> Result<Vec<NotebookRecord>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {NOTEBOOK_COLS} FROM notebooks WHERE project_id = ?1 ORDER BY name"
        ))
        .map_err(read_err)?;
    let rows = stmt
        .query_map(params![project_id], read_notebook)
        .map_err(read_err)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(read_err)?;
    Ok(rows)
}

pub fn notebook(
    pool: &DbPool,
    project_id: &str,
    notebook_id: &str,
) -> Result<Option<NotebookRecord>> {
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        &format!("SELECT {NOTEBOOK_COLS} FROM notebooks WHERE id = ?1 AND project_id = ?2"),
        params![notebook_id, project_id],
        read_notebook,
    )
    .optional()
    .map_err(read_err)
}

/// Cells of one version. `None` when that version was never written.
pub fn notebook_cells(pool: &DbPool, notebook_id: &str, version: u32) -> Result<Option<String>> {
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        "SELECT cells_json FROM notebook_versions WHERE notebook_id = ?1 AND version = ?2",
        params![notebook_id, version as i64],
        |row| row.get(0),
    )
    .optional()
    .map_err(read_err)
}

#[derive(Debug, Clone, PartialEq)]
pub struct NotebookVersionRecord {
    pub version: u32,
    pub sha256: String,
    pub author: String,
    pub created_at: String,
}

pub fn notebook_versions(pool: &DbPool, notebook_id: &str) -> Result<Vec<NotebookVersionRecord>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn
        .prepare(
            "SELECT version, sha256, author, created_at FROM notebook_versions \
             WHERE notebook_id = ?1 ORDER BY version DESC",
        )
        .map_err(read_err)?;
    let rows = stmt
        .query_map(params![notebook_id], |row| {
            Ok(NotebookVersionRecord {
                version: row.get::<_, i64>(0)? as u32,
                sha256: row.get(1)?,
                author: row.get(2)?,
                created_at: row.get(3)?,
            })
        })
        .map_err(read_err)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(read_err)?;
    Ok(rows)
}

/// Outcome of [`create_notebook`].
#[derive(Debug, Clone, PartialEq)]
pub enum NotebookCreation {
    Created(NotebookRecord),
    /// Another file of the project already occupies the derived path. Two
    /// names can slug to one stem ("Bell" and "bell!"), so this is an ordinary
    /// user collision, answered as such, not the `UNIQUE` violation it would
    /// otherwise become deep in the insert.
    PathTaken,
}

/// Creates a notebook at version 1 together with the `files` row that carries
/// it, in one transaction — a notebook without its file row would be invisible
/// to the file list and unreachable by the CAS.
pub fn create_notebook(
    pool: &DbPool,
    project_id: &str,
    name: &str,
    path: &str,
    cells_json: &str,
    author: &str,
) -> Result<NotebookCreation> {
    let sha256 = sha256_hex(cells_json);
    let file_id = uuid::Uuid::new_v4().to_string();
    let notebook_id = uuid::Uuid::new_v4().to_string();
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction().map_err(write_err)?;
    // Inside the transaction, and the pool has one writer, so the check and
    // the insert cannot be separated by another creation.
    let taken: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM files WHERE project_id = ?1 AND path = ?2)",
            params![project_id, path],
            |row| row.get(0),
        )
        .map_err(read_err)?;
    if taken {
        return Ok(NotebookCreation::PathTaken);
    }
    tx.execute(
        "INSERT INTO files (id, project_id, path, kind, sha256, size_bytes) \
         VALUES (?1, ?2, ?3, 'notebook', ?4, ?5)",
        params![file_id, project_id, path, sha256, cells_json.len() as i64],
    )
    .map_err(write_err)?;
    tx.execute(
        "INSERT INTO notebooks (id, project_id, file_id, name, current_version, updated_by) \
         VALUES (?1, ?2, ?3, ?4, 1, ?5)",
        params![notebook_id, project_id, file_id, name, author],
    )
    .map_err(write_err)?;
    tx.execute(
        "INSERT INTO notebook_versions (notebook_id, version, cells_json, sha256, author) \
         VALUES (?1, 1, ?2, ?3, ?4)",
        params![notebook_id, cells_json, sha256, author],
    )
    .map_err(write_err)?;
    tx.execute(
        "UPDATE projects SET updated_at = datetime('now') WHERE id = ?1",
        params![project_id],
    )
    .map_err(write_err)?;
    tx.commit().map_err(write_err)?;
    // The write guard has to go before the read below: an in-memory pool hands
    // out the WRITER connection for reads too, so holding both deadlocks.
    drop(conn);
    let record = notebook(pool, project_id, &notebook_id)?
        .ok_or_else(|| anyhow!("notebook vanished right after creation"))?;
    Ok(NotebookCreation::Created(record))
}

/// Outcome of a notebook save under optimistic locking.
#[derive(Debug, PartialEq, Eq)]
pub enum SaveOutcome {
    /// Saved; carries the resulting head version.
    Saved(u32),
    /// `expected_version` was stale — the editor must reload before retrying.
    Conflict,
    NotFound,
}

/// Appends a new notebook version under optimistic locking. The lock is ONE
/// conditional UPDATE (`WHERE current_version = expected`): zero affected rows
/// is a conflict, so two editors can never interleave a read and a write.
pub fn save_notebook(
    pool: &DbPool,
    project_id: &str,
    notebook_id: &str,
    cells_json: &str,
    expected_version: u32,
    author: &str,
) -> Result<SaveOutcome> {
    let sha256 = sha256_hex(cells_json);
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction().map_err(write_err)?;
    let file_id: Option<String> = tx
        .query_row(
            "SELECT file_id FROM notebooks WHERE id = ?1 AND project_id = ?2",
            params![notebook_id, project_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(read_err)?;
    let Some(file_id) = file_id else {
        return Ok(SaveOutcome::NotFound);
    };
    let new_version = expected_version + 1;
    let affected = tx
        .execute(
            "UPDATE notebooks SET current_version = ?1, updated_by = ?2, \
                updated_at = datetime('now') \
             WHERE id = ?3 AND project_id = ?4 AND current_version = ?5",
            params![
                new_version as i64,
                author,
                notebook_id,
                project_id,
                expected_version as i64
            ],
        )
        .map_err(write_err)?;
    if affected == 0 {
        return Ok(SaveOutcome::Conflict);
    }
    tx.execute(
        "INSERT INTO notebook_versions (notebook_id, version, cells_json, sha256, author) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![notebook_id, new_version as i64, cells_json, sha256, author],
    )
    .map_err(write_err)?;
    tx.execute(
        "UPDATE files SET sha256 = ?1, size_bytes = ?2, updated_at = datetime('now') \
         WHERE id = ?3",
        params![sha256, cells_json.len() as i64, file_id],
    )
    .map_err(write_err)?;
    tx.execute(
        "UPDATE projects SET updated_at = datetime('now') WHERE id = ?1",
        params![project_id],
    )
    .map_err(write_err)?;
    tx.commit().map_err(write_err)?;
    Ok(SaveOutcome::Saved(new_version))
}

/// Hex SHA-256 of a string — the notebook version fingerprint.
fn sha256_hex(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The record a creation returns, or a panic naming the collision — every
    /// test here creates notebooks at paths it chose itself.
    fn created(outcome: NotebookCreation) -> NotebookRecord {
        match outcome {
            NotebookCreation::Created(record) => record,
            NotebookCreation::PathTaken => panic!("path unexpectedly taken"),
        }
    }

    fn pool() -> DbPool {
        let conn = Connection::open_in_memory().expect("open mem");
        conn.execute_batch("PRAGMA foreign_keys=ON;").expect("fk");
        migrate(&conn).expect("migrate");
        std::sync::Arc::new(crate::db::Db::from_connection(conn))
    }

    #[test]
    fn migrations_are_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        let applied: i64 = conn
            .query_row("SELECT COUNT(*) FROM app_schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(applied, STEPS.len() as i64);
    }

    /// A private project is the owner's alone: a stranger sees nothing, and a
    /// share or a lab publication is the only way in.
    #[test]
    fn access_follows_ownership_shares_and_visibility() {
        let db = pool();
        let p = create_project(&db, "owner", "P", "", "private", None).unwrap();
        assert_eq!(access(&db, &p, "owner").unwrap(), Some(ProjectRole::Owner));
        assert_eq!(access(&db, &p, "stranger").unwrap(), None);

        set_share(&db, &p, "friend", "editor", "owner").unwrap();
        assert_eq!(
            access(&db, &p, "friend").unwrap(),
            Some(ProjectRole::Editor)
        );

        update_project(&db, &p, "P", "", "lab", None).unwrap();
        assert_eq!(
            access(&db, &p, "stranger").unwrap(),
            Some(ProjectRole::Viewer)
        );
        // The explicit share still wins over the weaker lab-wide role.
        assert_eq!(
            access(&db, &p, "friend").unwrap(),
            Some(ProjectRole::Editor)
        );
    }

    /// The transfer must not leave the new owner holding a weaker share row,
    /// which `access` would find first on the next lookup.
    #[test]
    fn transfer_drops_the_new_owners_share_row() {
        let db = pool();
        let p = create_project(&db, "owner", "P", "", "private", None).unwrap();
        set_share(&db, &p, "next", "viewer", "owner").unwrap();
        transfer_project(&db, &p, "next").unwrap();
        assert_eq!(access(&db, &p, "next").unwrap(), Some(ProjectRole::Owner));
        assert!(list_shares(&db, &p).unwrap().is_empty());
    }

    /// Two editors holding the same version: the second save loses instead of
    /// overwriting the first, and the version history stays append-only.
    #[test]
    fn notebook_save_is_optimistically_locked() {
        let db = pool();
        let p = create_project(&db, "owner", "P", "", "private", None).unwrap();
        let nb =
            created(create_notebook(&db, &p, "N", "notebooks/n.ipynb", "[]", "owner").unwrap());
        assert_eq!(nb.current_version, 1);

        assert_eq!(
            save_notebook(&db, &p, &nb.id, "[1]", 1, "a").unwrap(),
            SaveOutcome::Saved(2)
        );
        assert_eq!(
            save_notebook(&db, &p, &nb.id, "[2]", 1, "b").unwrap(),
            SaveOutcome::Conflict
        );
        assert_eq!(
            notebook_cells(&db, &nb.id, 2).unwrap().as_deref(),
            Some("[1]")
        );
        assert_eq!(notebook_versions(&db, &nb.id).unwrap().len(), 2);
    }

    /// Deleting a project takes its files, notebooks and versions with it —
    /// the cascade is what makes "delete" a complete act.
    #[test]
    fn deleting_a_project_cascades_to_its_content() {
        let db = pool();
        let p = create_project(&db, "owner", "P", "", "private", None).unwrap();
        let nb =
            created(create_notebook(&db, &p, "N", "notebooks/n.ipynb", "[]", "owner").unwrap());
        delete_project(&db, &p).unwrap();
        assert!(notebook(&db, &p, &nb.id).unwrap().is_none());
        assert!(list_files(&db, &p).unwrap().is_empty());
        assert!(notebook_versions(&db, &nb.id).unwrap().is_empty());
    }

    /// A notebook's backing row is off limits from BOTH sides: an upload must
    /// not retarget it (its kind, sha256 and size would then describe an
    /// unrelated blob) and a second notebook must not take its path.
    #[test]
    fn a_notebooks_file_row_is_neither_overwritten_nor_taken_twice() {
        let db = pool();
        let p = create_project(&db, "owner", "P", "", "private", None).unwrap();
        let nb = created(
            create_notebook(&db, &p, "Bell", "notebooks/bell.ipynb", "[]", "owner").unwrap(),
        );

        assert_eq!(
            upsert_file(&db, &p, "notebooks/bell.ipynb", "data", "deadbeef", 4).unwrap(),
            FileUpsert::NotebookBacking
        );
        assert_eq!(
            create_notebook(&db, &p, "bell!", "notebooks/bell.ipynb", "[]", "owner").unwrap(),
            NotebookCreation::PathTaken
        );

        // The original row is untouched: still a notebook, still its content.
        let files = list_files(&db, &p).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].kind, "notebook");
        assert_eq!(files[0].id, nb.file_id);

        // An ordinary path still stores, and re-uploading it replaces it.
        assert!(matches!(
            upsert_file(&db, &p, "data/counts.json", "data", "aa", 2).unwrap(),
            FileUpsert::Stored(_)
        ));
        assert!(matches!(
            upsert_file(&db, &p, "data/counts.json", "data", "bb", 3).unwrap(),
            FileUpsert::Stored(_)
        ));
        assert_eq!(list_files(&db, &p).unwrap().len(), 2);
    }

    /// The three KPI counters partition the visible set: a project that is both
    /// shared with the caller and published to the lab is counted once, so the
    /// dashboard row adds up to what the project list returns.
    #[test]
    fn project_counts_partition_the_visible_set() {
        let db = pool();
        let mine = create_project(&db, "anna", "Moj", "", "private", None).unwrap();
        let shared = create_project(&db, "opiekun", "Udostepniony", "", "lab", None).unwrap();
        let published = create_project(&db, "opiekun", "Materialy", "", "lab", None).unwrap();
        set_share(&db, &shared, "anna", "editor", "opiekun").unwrap();

        let (owned, shared_with_me, lab_projects) = project_counts(&db, "anna").unwrap();
        assert_eq!((owned, shared_with_me, lab_projects), (1, 1, 1));
        assert_eq!(
            (owned + shared_with_me + lab_projects) as usize,
            list_projects(&db, "anna", false).unwrap().len()
        );

        // And the role a listing shows matches what `access` would enforce.
        let shares = shared_roles(&db, "anna").unwrap();
        for record in list_projects(&db, "anna", false).unwrap() {
            assert_eq!(
                role_of(&record, "anna", &shares),
                access(&db, &record.id, "anna").unwrap()
            );
        }
        assert_eq!(
            role_of(&project(&db, &mine).unwrap().unwrap(), "anna", &shares),
            Some(ProjectRole::Owner)
        );
        assert_eq!(
            role_of(&project(&db, &published).unwrap().unwrap(), "anna", &shares),
            Some(ProjectRole::Viewer)
        );
    }

    /// The bulk counters a listing reads must equal the per-project ones a
    /// single answer reads, or the two views of one project disagree.
    #[test]
    fn bulk_stats_match_the_single_project_stats() {
        let db = pool();
        let p = create_project(&db, "owner", "P", "", "private", None).unwrap();
        let empty = create_project(&db, "owner", "Pusty", "", "private", None).unwrap();
        created(create_notebook(&db, &p, "N", "notebooks/n.ipynb", "[]", "owner").unwrap());
        upsert_file(&db, &p, "data/x.json", "data", "aa", 2).unwrap();
        set_share(&db, &p, "friend", "viewer", "owner").unwrap();

        let all = project_stats_all(&db).unwrap();
        assert_eq!(
            all.get(&p).copied().unwrap(),
            project_stats(&db, &p).unwrap()
        );
        assert_eq!(
            all.get(&empty).copied().unwrap_or_default(),
            project_stats(&db, &empty).unwrap()
        );
    }

    #[test]
    fn settings_default_until_written() {
        let db = pool();
        let stored = settings(&db).unwrap();
        assert!(stored.ranking_enabled);
        assert_eq!(stored.max_qubits_core, 28);

        let mut changed = stored.clone();
        changed.ranking_enabled = false;
        changed.max_qubits_gpu = 33;
        set_settings(&db, &changed).unwrap();
        assert_eq!(settings(&db).unwrap(), changed);

        // The admin half is a separate row: writing one never rewrites the
        // other, which is what lets the two be read by different people.
        let admin = admin_settings(&db).unwrap();
        assert_eq!(admin.retention_days, 180);
        assert_eq!(admin.isolation_mode, "container");
        let mut admin_changed = admin.clone();
        admin_changed.retention_days = 30;
        set_admin_settings(&db, &admin_changed).unwrap();
        assert_eq!(admin_settings(&db).unwrap(), admin_changed);
        assert_eq!(settings(&db).unwrap(), changed);
    }
}
