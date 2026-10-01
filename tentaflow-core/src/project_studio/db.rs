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

use anyhow::{anyhow, Result};
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
            if *version == 3 {
                migrate_project_functions(&tx)?;
            }
            if *version == 4 {
                assign_project_key_prefixes(&tx)?;
            }
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
const MIGRATIONS: &[(i64, &str)] = &[
    (1, INITIAL_SCHEMA),
    (2, CENTRAL_SCHEMA_V2),
    (3, CENTRAL_SCHEMA_V3),
    (4, CENTRAL_SCHEMA_V4),
];

const CENTRAL_SCHEMA_V4: &str = "
ALTER TABLE projects ADD COLUMN key_prefix TEXT NOT NULL DEFAULT '';
";

pub(crate) fn normalize_key_prefix(input: &str) -> Option<String> {
    let prefix = input.trim().to_ascii_uppercase();
    (prefix.len() >= 2
        && prefix.len() <= 8
        && prefix.as_bytes()[0].is_ascii_uppercase()
        && prefix
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit()))
    .then_some(prefix)
}

pub(crate) fn suggest_key_prefix(name: &str) -> String {
    let mut prefix: String = name
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .take(8)
        .map(|ch| ch.to_ascii_uppercase())
        .collect();
    if prefix.is_empty() || !prefix.as_bytes()[0].is_ascii_uppercase() {
        prefix.insert(0, 'P');
    }
    if prefix.len() == 1 {
        prefix.push('P');
    }
    prefix.truncate(8);
    prefix
}

pub(crate) fn reserve_key_prefix(
    tx: &rusqlite::Transaction<'_>,
    org_id: &str,
    name: &str,
    requested: &str,
) -> Result<String> {
    use rusqlite::OptionalExtension;

    if !requested.trim().is_empty() {
        let prefix = normalize_key_prefix(requested)
            .ok_or_else(|| anyhow!("key prefix must match [A-Z][A-Z0-9]{{1,7}}"))?;
        let taken: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM projects WHERE org_id = ?1 AND key_prefix = ?2",
                rusqlite::params![org_id, prefix],
                |row| row.get(0),
            )
            .optional()?;
        if taken.is_some() {
            return Err(anyhow!("key prefix already exists in organization"));
        }
        return Ok(prefix);
    }
    let base = suggest_key_prefix(name);
    for number in 1..=1_000_000 {
        let candidate = if number == 1 {
            base.clone()
        } else {
            let suffix = number.to_string();
            format!("{}{}", &base[..base.len().min(8 - suffix.len())], suffix)
        };
        let taken: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM projects WHERE org_id = ?1 AND key_prefix = ?2",
                rusqlite::params![org_id, candidate],
                |row| row.get(0),
            )
            .optional()?;
        if taken.is_none() {
            return Ok(candidate);
        }
    }
    Err(anyhow!("organization has exhausted project key prefixes"))
}

fn assign_project_key_prefixes(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let mut stmt = tx.prepare(
        "SELECT project_id,org_id,name FROM projects ORDER BY org_id,created_at,project_id",
    )?;
    let projects = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    for (project_id, org_id, name) in projects {
        let prefix = reserve_key_prefix(tx, &org_id, &name, "")?;
        tx.execute(
            "UPDATE projects SET key_prefix = ?1 WHERE project_id = ?2",
            rusqlite::params![prefix, project_id],
        )?;
    }
    tx.execute_batch(
        "CREATE UNIQUE INDEX idx_projects_org_key_prefix ON projects(org_id,key_prefix);",
    )?;
    Ok(())
}

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

/// F4: denormalised "when does this project fire next" hint for the schedule
/// loop. It duplicates state that lives in each `project.db`, and it has to:
/// the pool cache holds at most 16 open per-project databases, so a loop that
/// opened every project once per tick would thrash the LRU and starve the rest
/// of the module. With the hint the tick is ONE query here, and only projects
/// that are actually due get opened. Rows are refreshed on every schedule
/// save/delete/toggle/trigger; a NULL `next_run_at` means "nothing pending"
/// and never matches the due comparison.
const CENTRAL_SCHEMA_V2: &str = "
CREATE TABLE project_schedule_hints (
    project_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL,
    next_run_at TEXT,
    enabled_count INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_schedule_hints_due ON project_schedule_hints(next_run_at);
";

const CENTRAL_SCHEMA_V3: &str = "
DROP INDEX idx_project_members_user;
ALTER TABLE project_members RENAME TO project_members_legacy;
CREATE TABLE project_members (
    project_id TEXT NOT NULL, user_id TEXT NOT NULL,
    project_admin INTEGER NOT NULL DEFAULT 0 CHECK(project_admin IN (0,1)),
    expires_at TEXT,
    invited_by TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY(project_id, user_id),
    FOREIGN KEY(project_id) REFERENCES projects(project_id) ON DELETE CASCADE
);
CREATE INDEX idx_project_members_user ON project_members(user_id);
INSERT INTO project_members(project_id,user_id,project_admin,invited_by,created_at)
    SELECT project_id,user_id,role IN ('owner','manager'),invited_by,created_at
    FROM project_members_legacy;
INSERT OR IGNORE INTO project_members(project_id,user_id,project_admin,invited_by)
    SELECT project_id,owner_user_id,1,owner_user_id FROM projects;
UPDATE project_members SET project_admin = 1 WHERE EXISTS (
    SELECT 1 FROM projects p WHERE p.project_id = project_members.project_id
        AND p.owner_user_id = project_members.user_id
);
CREATE TABLE project_functions (
    project_id TEXT NOT NULL,
    function_id TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    builtin INTEGER NOT NULL DEFAULT 0 CHECK(builtin IN (0,1)),
    grants_json TEXT NOT NULL,
    position INTEGER NOT NULL,
    PRIMARY KEY(project_id,function_id),
    FOREIGN KEY(project_id) REFERENCES projects(project_id) ON DELETE CASCADE
);
CREATE TABLE project_member_functions (
    project_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    function_id TEXT NOT NULL,
    PRIMARY KEY(project_id,user_id,function_id),
    FOREIGN KEY(project_id,user_id) REFERENCES project_members(project_id,user_id) ON DELETE CASCADE,
    FOREIGN KEY(project_id,function_id) REFERENCES project_functions(project_id,function_id) ON DELETE CASCADE
);
CREATE TABLE project_ml_grants (
    ml_project_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    link_id TEXT NOT NULL,
    PRIMARY KEY(ml_project_id,user_id,project_id,link_id),
    FOREIGN KEY(project_id) REFERENCES projects(project_id) ON DELETE CASCADE
);
CREATE INDEX idx_project_ml_grants_link ON project_ml_grants(project_id,link_id);
";

pub(super) fn seed_project_functions(conn: &Connection, project_id: &str) -> Result<()> {
    for (position, function) in super::models::default_project_functions()
        .iter()
        .enumerate()
    {
        conn.execute(
            "INSERT INTO project_functions \
             (project_id,function_id,name,description,builtin,grants_json,position) \
             VALUES (?1,?2,?3,?4,1,?5,?6)",
            rusqlite::params![
                project_id,
                function.function_id,
                function.name,
                function.description,
                serde_json::to_string(&function.grants)?,
                position as i64
            ],
        )?;
    }
    Ok(())
}

fn migrate_project_functions(conn: &Connection) -> Result<()> {
    let projects = {
        let mut stmt = conn.prepare("SELECT project_id,owner_user_id FROM projects")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (project_id, owner_user_id) in projects {
        seed_project_functions(conn, &project_id)?;
        let members = {
            let mut stmt = conn.prepare(
                "SELECT m.user_id,COALESCE(l.role,'owner') FROM project_members m \
                 LEFT JOIN project_members_legacy l ON l.project_id=m.project_id AND l.user_id=m.user_id \
                 WHERE m.project_id=?1",
            )?;
            let rows = stmt.query_map(rusqlite::params![project_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (user_id, role) in members {
            let functions = if user_id == owner_user_id || role == "owner" {
                super::models::default_project_functions()
                    .into_iter()
                    .map(|function| function.function_id)
                    .collect::<Vec<_>>()
            } else {
                vec![match role.as_str() {
                    "manager" => "pm",
                    "editor" => "developer",
                    "tester" => "tester",
                    "viewer" => "observer",
                    _ => anyhow::bail!("invalid legacy project role during migration"),
                }
                .to_string()]
            };
            for function_id in functions {
                conn.execute(
                    "INSERT INTO project_member_functions(project_id,user_id,function_id) VALUES (?1,?2,?3)",
                    rusqlite::params![project_id,user_id,function_id],
                )?;
            }
        }
    }
    conn.execute_batch("DROP TABLE project_members_legacy;")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_assigns_stable_unique_project_prefixes_per_organization() {
        let conn = Connection::open_in_memory().expect("registry");
        conn.execute_batch(
            "CREATE TABLE project_studio_schema_version(version INTEGER PRIMARY KEY);",
        )
        .expect("versions");
        conn.execute_batch(INITIAL_SCHEMA).expect("v1");
        conn.execute(
            "INSERT INTO project_studio_schema_version(version) VALUES (1)",
            [],
        )
        .expect("version");
        for (id, org, name) in [
            ("p1", "o1", "Alpha"),
            ("p2", "o1", "Alpha!"),
            ("p3", "o2", "Alpha"),
        ] {
            conn.execute(
                "INSERT INTO projects(project_id,org_id,name,owner_user_id,dir_path,created_at) \
                 VALUES (?1,?2,?3,'u1','/unused','2026-01-01 00:00:00')",
                rusqlite::params![id, org, name],
            )
            .expect("project");
        }
        run_migrations(&conn).expect("migrate");
        let prefixes: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT key_prefix FROM projects ORDER BY project_id")
                .expect("query");
            stmt.query_map([], |row| row.get(0))
                .expect("rows")
                .collect::<rusqlite::Result<_>>()
                .expect("values")
        };
        assert_eq!(prefixes, ["ALPHA", "ALPHA2", "ALPHA"]);
        run_migrations(&conn).expect("reopen");
        let unchanged: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT key_prefix FROM projects ORDER BY project_id")
                .expect("query");
            stmt.query_map([], |row| row.get(0))
                .expect("rows")
                .collect::<rusqlite::Result<_>>()
                .expect("values")
        };
        assert_eq!(unchanged, prefixes);
    }

    /// v1 → v2 on a REAL registry database: the seeded rows survive, the hint
    /// table is queryable with its index, a NULL `next_run_at` stays out of
    /// the due query (an empty string would sort before every timestamp) and
    /// re-running the migrations is a no-op.
    #[test]
    fn migration_v2_adds_schedule_hints() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let conn = Connection::open(tmp.path().join("projects.db")).expect("open registry");

        // Seed a genuine v1 registry the same way run_migrations would.
        conn.execute_batch(
            "CREATE TABLE project_studio_schema_version (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .expect("version table");
        conn.execute_batch(INITIAL_SCHEMA).expect("apply v1");
        conn.execute(
            "INSERT INTO project_studio_schema_version (version) VALUES (1)",
            [],
        )
        .expect("record version");
        conn.execute(
            "INSERT INTO projects (project_id, org_id, name, owner_user_id, dir_path) \
             VALUES ('p1', 'o1', 'Projekt QA', 'u1', '/tmp/p1')",
            [],
        )
        .expect("insert project");

        run_migrations(&conn).expect("migrate to v2");

        let name: String = conn
            .query_row(
                "SELECT name FROM projects WHERE project_id = 'p1'",
                [],
                |r| r.get(0),
            )
            .expect("project row");
        assert_eq!(name, "Projekt QA");

        conn.execute(
            "INSERT INTO project_schedule_hints (project_id, org_id, next_run_at, enabled_count) \
             VALUES ('p1', 'o1', '2026-08-01T00:30:00Z', 2)",
            [],
        )
        .expect("insert hint");
        conn.execute(
            "INSERT INTO project_schedule_hints (project_id, org_id, next_run_at, enabled_count) \
             VALUES ('p2', 'o1', NULL, 0)",
            [],
        )
        .expect("insert idle hint");

        let due: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM project_schedule_hints h \
                 JOIN projects p ON p.project_id = h.project_id \
                 WHERE p.status = 'active' AND h.enabled_count > 0 \
                 AND h.next_run_at <= '2027-01-01T00:00:00Z'",
                [],
                |r| r.get(0),
            )
            .expect("due query");
        assert_eq!(due, 1, "only the project with a pending schedule is due");

        let idx: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' \
                 AND name = 'idx_schedule_hints_due'",
                [],
                |r| r.get(0),
            )
            .expect("index lookup");
        assert_eq!(idx, 1);

        // Idempotent: a second pass applies nothing and records nothing twice.
        run_migrations(&conn).expect("re-run migrations");
        let versions: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM project_studio_schema_version",
                [],
                |r| r.get(0),
            )
            .expect("version count");
        assert_eq!(versions, MIGRATIONS.len() as i64);
        let hints: i64 = conn
            .query_row("SELECT COUNT(*) FROM project_schedule_hints", [], |r| {
                r.get(0)
            })
            .expect("hint count");
        assert_eq!(hints, 2, "re-running migrations must not drop data");
    }

    #[test]
    fn migration_v3_replaces_every_role_and_preserves_ownership() {
        let conn = Connection::open_in_memory().expect("registry");
        conn.execute_batch("PRAGMA foreign_keys=ON;")
            .expect("foreign keys");
        conn.execute_batch(INITIAL_SCHEMA).expect("v1");
        conn.execute_batch(CENTRAL_SCHEMA_V2).expect("v2");
        conn.execute_batch(
            "CREATE TABLE project_studio_schema_version(version INTEGER PRIMARY KEY); \
             INSERT INTO project_studio_schema_version VALUES(1),(2); \
             INSERT INTO projects(project_id,org_id,name,owner_user_id,dir_path) \
                 VALUES('p','o','Migration','u-owner','unused');",
        )
        .expect("legacy project");
        for role in ["owner", "manager", "editor", "tester", "viewer"] {
            conn.execute(
                "INSERT INTO project_members(project_id,user_id,role,invited_by) VALUES('p',?1,?2,'u-owner')",
                rusqlite::params![format!("u-{role}"),role],
            ).expect("legacy member");
        }
        run_migrations(&conn).expect("migrate");
        let owner: String = conn
            .query_row("SELECT owner_user_id FROM projects", [], |row| row.get(0))
            .expect("owner");
        assert_eq!(owner, "u-owner");
        for (user, expected_admin, expected_functions) in [
            ("u-owner", true, 9),
            ("u-manager", true, 1),
            ("u-editor", false, 1),
            ("u-tester", false, 1),
            ("u-viewer", false, 1),
        ] {
            let admin: bool = conn
                .query_row(
                    "SELECT project_admin FROM project_members WHERE user_id=?1",
                    [user],
                    |row| row.get(0),
                )
                .expect("admin");
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM project_member_functions WHERE user_id=?1",
                    [user],
                    |row| row.get(0),
                )
                .expect("functions");
            assert_eq!(admin, expected_admin, "{user}");
            assert_eq!(count, expected_functions, "{user}");
        }
        for (user, function) in [
            ("u-manager", "pm"),
            ("u-editor", "developer"),
            ("u-tester", "tester"),
            ("u-viewer", "observer"),
        ] {
            let actual: String = conn
                .query_row(
                    "SELECT function_id FROM project_member_functions WHERE user_id=?1",
                    [user],
                    |row| row.get(0),
                )
                .expect("mapped function");
            assert_eq!(actual, function);
        }
        let old_role_columns: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('project_members') WHERE name='role'",
                [],
                |row| row.get(0),
            )
            .expect("schema");
        assert_eq!(old_role_columns, 0);
        let function_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM project_functions", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("catalogue");
        assert_eq!(function_count, 9);
        conn.execute(
            "UPDATE project_members SET project_admin=0 WHERE user_id='u-owner'",
            [],
        )
        .expect("owner may be separate from admin");
        conn.execute("INSERT INTO project_members(project_id,user_id,invited_by,expires_at) VALUES('p','custom','u-owner','2099-01-01T00:00:00Z')", []).expect("role-free member");
        run_migrations(&conn).expect("idempotent");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM project_members", [], |row| row.get(0))
            .expect("members preserved");
        assert_eq!(count, 6);
        let admin: bool = conn
            .query_row(
                "SELECT project_admin FROM project_members WHERE user_id='u-owner'",
                [],
                |row| row.get(0),
            )
            .expect("owner admin");
        assert!(
            !admin,
            "idempotent migration must not restore a changed admin flag"
        );
    }
}
