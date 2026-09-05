// ===== File: tentaquant/db.rs — schema and rows of one lab's tentaquant.db =====
//
// One INSTANCE of the package is one laboratory, and every laboratory keeps its
// whole content in its own `<instance data dir>/tentaquant.db` (plan §9.2). The
// main database holds only the platform layer — the `addons` row, the package
// catalog and the permission matrix — so two labs on one node share nothing but
// the code, and uninstalling one wipes exactly one directory.
//
// There is deliberately NO member table: who belongs to a lab is the instance's
// permission matrix INTERSECTED with that instance's Visibility (§10.1/§10.2) —
// `quant.read` is `default = "allow"`, so the matrix alone admits the whole
// organization and Visibility is what scopes the lab to its group.
// `project_shares` is a different thing entirely — it is the ML-Studio
// ownership model INSIDE a lab (§18 decision 15), and a share row is dormant
// until that same intersection also admits the person it names.
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
/// Step 1 creates exactly ten tables: `user_settings`, `projects`,
/// `project_shares`, `files`, `notebooks`, `notebook_versions`, `cell_outputs`,
/// `runs`, `kata_progress` and `settings`. All but two are written here:
/// `runs` and `cell_outputs` carry the T1 execution tier — one row per run, one
/// row per mime bundle it produced (plan §9.2, §4.3). `user_settings` and
/// `kata_progress` are created unwritten: they complete the per-user shape the
/// written tables already belong to, so the phase that starts filling them adds
/// rows rather than a schema.
///
/// A whole SUBSYSTEM, by contrast, stays out until the phase that owns it:
/// providers, QPU budgets, ledgers, approvals, examples and kernel sessions get
/// no tables here, because a schema for a feature nothing implements is a
/// promise the code cannot keep.
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
///
/// `cell_outputs` is keyed by run and cell rather than by project, so no
/// foreign key reaches it: without this delete its rows would outlive the runs
/// that produced them, unreachable and uncounted.
pub fn delete_project(pool: &DbPool, project_id: &str) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction().map_err(write_err)?;
    tx.execute(
        "DELETE FROM cell_outputs WHERE run_id IN (SELECT id FROM runs WHERE project_id = ?1)",
        params![project_id],
    )
    .map_err(write_err)?;
    tx.execute("DELETE FROM projects WHERE id = ?1", params![project_id])
        .map_err(write_err)?;
    tx.commit().map_err(write_err)?;
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

// =============================================================================
// Runs and their outputs
// =============================================================================

/// One row of `runs` (plan §9.2). The status machine is
/// `created → queued → running → { succeeded | failed | cancelled }`; a target
/// is `core:<node_id>` for the T1 tier this phase executes.
#[derive(Debug, Clone, PartialEq)]
pub struct RunRecord {
    pub id: String,
    pub project_id: Option<String>,
    pub notebook_id: Option<String>,
    pub cell_id: Option<String>,
    pub kind: String,
    pub target: String,
    pub node_id: Option<String>,
    pub status: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub error: Option<String>,
    pub metrics_json: Option<String>,
    pub user_id: String,
    pub pinned_at: Option<String>,
    pub thumbnail_sha256: Option<String>,
    pub keyframes_sha256: Option<String>,
}

/// What a run needs to exist. Written once, at `created`; everything else
/// about a run is an update to the row this inserts.
#[derive(Debug, Clone, PartialEq)]
pub struct NewRun {
    pub id: String,
    pub project_id: Option<String>,
    pub notebook_id: Option<String>,
    pub cell_id: Option<String>,
    pub kind: String,
    pub target: String,
    pub node_id: Option<String>,
    pub user_id: String,
}

const RUN_COLS: &str = "id, project_id, notebook_id, cell_id, kind, target, node_id, status, \
     started_at, ended_at, error, metrics_json, user_id, pinned_at, thumbnail_sha256, \
     keyframes_sha256";

fn read_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunRecord> {
    Ok(RunRecord {
        id: row.get(0)?,
        project_id: row.get(1)?,
        notebook_id: row.get(2)?,
        cell_id: row.get(3)?,
        kind: row.get(4)?,
        target: row.get(5)?,
        node_id: row.get(6)?,
        status: row.get(7)?,
        started_at: row.get(8)?,
        ended_at: row.get(9)?,
        error: row.get(10)?,
        metrics_json: row.get(11)?,
        user_id: row.get(12)?,
        pinned_at: row.get(13)?,
        thumbnail_sha256: row.get(14)?,
        keyframes_sha256: row.get(15)?,
    })
}

/// Terminal states: a run in one of these is finished and nothing will move it
/// again — the orphan sweep and the stream both key off this predicate.
pub fn is_terminal_status(status: &str) -> bool {
    matches!(status, "succeeded" | "failed" | "cancelled")
}

/// Inserts a run in `created`. The row exists BEFORE the work is queued, so a
/// run is never invisible while it waits for a slot.
pub fn create_run(pool: &DbPool, run: &NewRun) -> Result<RunRecord> {
    {
        let conn = pool.write().map_err(write_err)?;
        conn.execute(
            "INSERT INTO runs (id, project_id, notebook_id, cell_id, kind, target, node_id, \
                               status, user_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'created', ?8)",
            params![
                run.id,
                run.project_id,
                run.notebook_id,
                run.cell_id,
                run.kind,
                run.target,
                run.node_id,
                run.user_id,
            ],
        )
        .map_err(write_err)?;
    }
    run_row(pool, &run.id)?.ok_or_else(|| anyhow!("run '{}' vanished after insert", run.id))
}

/// One run by id, whatever its access. Callers that answer a client go through
/// [`visible_run`] instead.
pub fn run_row(pool: &DbPool, run_id: &str) -> Result<Option<RunRecord>> {
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        &format!("SELECT {RUN_COLS} FROM runs WHERE id = ?1"),
        params![run_id],
        read_run,
    )
    .optional()
    .map_err(read_err)
}

/// The SQL predicate for "runs this caller may see" (plan §10.3): their own
/// runs, and runs of a project they can open — owned, shared with them or
/// published to the laboratory. A supervisor (`quant.instruct`) sees the
/// METADATA of every run, which is why the caller passes `supervisor` rather
/// than this deciding it: the same rows, a wider filter, never wider content.
fn run_visibility_sql(supervisor: bool) -> &'static str {
    if supervisor {
        "1 = 1"
    } else {
        "(runs.user_id = ?1 OR EXISTS (SELECT 1 FROM projects p WHERE p.id = runs.project_id \
           AND (p.owner_user_id = ?1 OR p.visibility = 'lab' \
                OR EXISTS (SELECT 1 FROM project_shares s \
                           WHERE s.project_id = p.id AND s.user_id = ?1))))"
    }
}

/// One run, or `None` when the caller may not see it — indistinguishable from
/// a run that does not exist, which is what the handler answers with.
pub fn visible_run(
    pool: &DbPool,
    run_id: &str,
    user_id: &str,
    supervisor: bool,
) -> Result<Option<RunRecord>> {
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        &format!(
            "SELECT {RUN_COLS} FROM runs WHERE id = ?2 AND {}",
            run_visibility_sql(supervisor)
        ),
        params![user_id, run_id],
        read_run,
    )
    .optional()
    .map_err(read_err)
}

/// Runs the caller may see, newest first.
pub fn list_runs(
    pool: &DbPool,
    user_id: &str,
    supervisor: bool,
    project_id: Option<&str>,
    pinned_only: bool,
    limit: u32,
) -> Result<Vec<RunRecord>> {
    let conn = pool.read().map_err(read_err)?;
    let mut sql = format!(
        "SELECT {RUN_COLS} FROM runs WHERE {}",
        run_visibility_sql(supervisor)
    );
    if project_id.is_some() {
        sql.push_str(" AND runs.project_id = ?3");
    }
    if pinned_only {
        sql.push_str(" AND runs.pinned_at IS NOT NULL");
    }
    sql.push_str(" ORDER BY runs.started_at DESC, runs.id DESC LIMIT ?2");
    let mut stmt = conn.prepare(&sql).map_err(read_err)?;
    let rows = match project_id {
        Some(project) => stmt.query_map(params![user_id, limit as i64, project], read_run),
        None => stmt.query_map(params![user_id, limit as i64], read_run),
    }
    .map_err(read_err)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(read_err)?);
    }
    Ok(out)
}

/// Moves a run to a non-terminal state (`queued`, `running`).
pub fn set_run_status(pool: &DbPool, run_id: &str, status: &str) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "UPDATE runs SET status = ?2 WHERE id = ?1 \
         AND status NOT IN ('succeeded','failed','cancelled')",
        params![run_id, status],
    )
    .map_err(write_err)?;
    Ok(())
}

/// Closes a run with its outcome. Conditional on the row still being open, so
/// a late finish can never overwrite a cancellation that already landed.
pub fn finish_run(
    pool: &DbPool,
    run_id: &str,
    status: &str,
    error: Option<&str>,
    metrics_json: Option<&str>,
) -> Result<bool> {
    let conn = pool.write().map_err(write_err)?;
    let changed = conn
        .execute(
            "UPDATE runs SET status = ?2, error = ?3, metrics_json = ?4, \
                             ended_at = datetime('now') \
             WHERE id = ?1 AND status NOT IN ('succeeded','failed','cancelled')",
            params![run_id, status, error, metrics_json],
        )
        .map_err(write_err)?;
    Ok(changed > 0)
}

/// Records where the recorded evolution of a run landed in the content store.
pub fn set_run_keyframes(pool: &DbPool, run_id: &str, sha256: &str) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "UPDATE runs SET keyframes_sha256 = ?2 WHERE id = ?1",
        params![run_id, sha256],
    )
    .map_err(write_err)?;
    Ok(())
}

/// Records where the run's gallery tile landed in the content store.
pub fn set_run_thumbnail(pool: &DbPool, run_id: &str, sha256: &str) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "UPDATE runs SET thumbnail_sha256 = ?2 WHERE id = ?1",
        params![run_id, sha256],
    )
    .map_err(write_err)?;
    Ok(())
}

/// Pins or unpins a run for the project's results gallery (plan §13.6).
pub fn set_run_pinned(pool: &DbPool, run_id: &str, pinned: bool) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        if pinned {
            "UPDATE runs SET pinned_at = datetime('now') WHERE id = ?1 AND pinned_at IS NULL"
        } else {
            "UPDATE runs SET pinned_at = NULL WHERE id = ?1"
        },
        params![run_id],
    )
    .map_err(write_err)?;
    Ok(())
}

/// One stored output of a run (`cell_outputs`, plan §9.2): a mime bundle, with
/// the bytes in the content store when they were too large to keep inline.
#[derive(Debug, Clone, PartialEq)]
pub struct CellOutputRecord {
    pub run_id: String,
    pub cell_id: String,
    pub seq: u32,
    pub mime_json: String,
    pub artifact_sha256: Option<String>,
}

/// Appends one output. Idempotent per `(run, cell, seq)`, so a retry after a
/// failed write cannot produce two rows for one output.
pub fn append_cell_output(pool: &DbPool, output: &CellOutputRecord) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "INSERT INTO cell_outputs (run_id, cell_id, seq, mime_json, artifact_sha256) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(run_id, cell_id, seq) DO UPDATE SET \
             mime_json = excluded.mime_json, artifact_sha256 = excluded.artifact_sha256",
        params![
            output.run_id,
            output.cell_id,
            output.seq,
            output.mime_json,
            output.artifact_sha256,
        ],
    )
    .map_err(write_err)?;
    Ok(())
}

/// Every output of one run, in the order it was produced.
pub fn cell_outputs(pool: &DbPool, run_id: &str) -> Result<Vec<CellOutputRecord>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn
        .prepare(
            "SELECT run_id, cell_id, seq, mime_json, artifact_sha256 FROM cell_outputs \
             WHERE run_id = ?1 ORDER BY cell_id, seq",
        )
        .map_err(read_err)?;
    let rows = stmt
        .query_map(params![run_id], |row| {
            Ok(CellOutputRecord {
                run_id: row.get(0)?,
                cell_id: row.get(1)?,
                seq: row.get::<_, i64>(2)? as u32,
                mime_json: row.get(3)?,
                artifact_sha256: row.get(4)?,
            })
        })
        .map_err(read_err)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(read_err)?);
    }
    Ok(out)
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

    /// Deleting a project must not leave its runs' outputs behind. The run rows
    /// go with the foreign key; `cell_outputs` is keyed by run and cell, so
    /// nothing but the delete itself reaches them, and a row nobody can reach
    /// from a run is a row nobody will ever sweep.
    #[test]
    fn deleting_a_project_takes_its_runs_outputs_with_it() {
        let db = pool();
        let project = create_project(&db, "anna", "P", "", "private", None).unwrap();
        let run = create_run(
            &db,
            &NewRun {
                id: "run-out".to_string(),
                project_id: Some(project.clone()),
                notebook_id: None,
                cell_id: Some("cell-1".to_string()),
                kind: "circuit".to_string(),
                target: "core:node-a".to_string(),
                node_id: Some("node-a".to_string()),
                user_id: "anna".to_string(),
            },
        )
        .unwrap();
        append_cell_output(
            &db,
            &CellOutputRecord {
                run_id: run.id.clone(),
                cell_id: "cell-1".to_string(),
                seq: 0,
                mime_json: "{\"application/json\":{}}".to_string(),
                artifact_sha256: None,
            },
        )
        .unwrap();
        assert_eq!(cell_outputs(&db, &run.id).unwrap().len(), 1);

        delete_project(&db, &project).unwrap();
        assert!(run_row(&db, &run.id).unwrap().is_none());
        assert!(cell_outputs(&db, &run.id).unwrap().is_empty());
    }
}
