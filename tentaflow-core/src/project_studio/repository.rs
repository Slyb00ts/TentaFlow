// ===== File: project_studio/repository.rs — CRUD for Project Studio registries =====
//
// Two data layers: the central registry (`projects.db`, reached through
// `db::pool()`) and per-project content (`project.db`, reached through a
// `DbPool` obtained from `project_db::open`). Core-directory lookups
// (`user_accounts`, `org_memberships`, `agents`) go through
// `crate::db::global_pool()` — separate SQLite files cannot be joined, so
// display names/emails are resolved per id.

use std::collections::HashMap;

use anyhow::{anyhow, bail, Result};
use rusqlite::{params, OptionalExtension};

use super::models::{
    ActivityRecord, ChatRecord, CreatorGrantRecord, IngestJobRecord, MemberRecord, ProjectKpis,
    ProjectRecord, ProjectRole, SourceFileRecord, SourceListItem, SourceRecord, TagRecord,
};
use crate::db::DbPool;

fn read_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("project_studio db read: {e}")
}

fn write_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("project_studio db write: {e}")
}

/// Escapes LIKE wildcards so user input matches literally. Every query using
/// the result must declare `ESCAPE '\'` on the LIKE clause.
fn escape_like(input: &str) -> String {
    input
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

// =============================================================================
// Central registry: projects
// =============================================================================

fn read_project(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectRecord> {
    Ok(ProjectRecord {
        project_id: row.get(0)?,
        org_id: row.get(1)?,
        name: row.get(2)?,
        description: row.get(3)?,
        status: row.get(4)?,
        template: row.get(5)?,
        modules_json: row.get(6)?,
        owner_user_id: row.get(7)?,
        dir_path: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

const PROJECT_COLS: &str = "project_id, org_id, name, description, status, template, \
     modules_json, owner_user_id, dir_path, created_at, updated_at";

/// Inserts the project row together with the owner membership and any initial
/// members in ONE transaction — a project can never exist without its owner.
#[allow(clippy::too_many_arguments)]
pub fn create_project(
    project_id: &str,
    org_id: &str,
    name: &str,
    description: &str,
    template: &str,
    modules_json: &str,
    owner_user_id: &str,
    dir_path: &str,
    members: &[(String, String)],
) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO projects (project_id, org_id, name, description, template, \
         modules_json, owner_user_id, dir_path) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            project_id,
            org_id,
            name,
            description,
            template,
            modules_json,
            owner_user_id,
            dir_path
        ],
    )?;
    tx.execute(
        "INSERT INTO project_members (project_id, user_id, role, invited_by) \
         VALUES (?1, ?2, 'owner', ?2)",
        params![project_id, owner_user_id],
    )?;
    for (user_id, role) in members {
        if user_id == owner_user_id {
            continue;
        }
        tx.execute(
            "INSERT OR IGNORE INTO project_members (project_id, user_id, role, invited_by) \
             VALUES (?1, ?2, ?3, ?4)",
            params![project_id, user_id, role, owner_user_id],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn get_project(org_id: &str, project_id: &str) -> Result<Option<ProjectRecord>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        &format!("SELECT {PROJECT_COLS} FROM projects WHERE org_id = ?1 AND project_id = ?2"),
        params![org_id, project_id],
        read_project,
    )
    .optional()
    .map_err(Into::into)
}

pub fn list_projects(org_id: &str, include_archived: bool) -> Result<Vec<ProjectRecord>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let sql = if include_archived {
        format!("SELECT {PROJECT_COLS} FROM projects WHERE org_id = ?1 ORDER BY updated_at DESC")
    } else {
        format!(
            "SELECT {PROJECT_COLS} FROM projects WHERE org_id = ?1 AND status = 'active' \
             ORDER BY updated_at DESC"
        )
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![org_id], read_project)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Renames the project. A `UNIQUE(org_id, name)` violation surfaces as
/// `Err` whose message contains "UNIQUE" — the dispatcher maps it to
/// BadRequest.
pub fn update_project_name_desc(
    org_id: &str,
    project_id: &str,
    name: &str,
    description: &str,
) -> Result<bool> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute(
        "UPDATE projects SET name = ?1, description = ?2, updated_at = datetime('now') \
         WHERE org_id = ?3 AND project_id = ?4",
        params![name, description, org_id, project_id],
    )?;
    Ok(n > 0)
}

/// Replaces the enabled-module set. Modules only gate which tabs/handlers a
/// project exposes, so switching one off keeps every row it produced — the data
/// reappears untouched once the module is switched back on.
pub fn update_project_modules(org_id: &str, project_id: &str, modules_json: &str) -> Result<bool> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute(
        "UPDATE projects SET modules_json = ?1, updated_at = datetime('now') \
         WHERE org_id = ?2 AND project_id = ?3",
        params![modules_json, org_id, project_id],
    )?;
    Ok(n > 0)
}

pub fn set_project_archived(org_id: &str, project_id: &str, archived: bool) -> Result<bool> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let status = if archived { "archived" } else { "active" };
    let n = conn.execute(
        "UPDATE projects SET status = ?1, updated_at = datetime('now') \
         WHERE org_id = ?2 AND project_id = ?3",
        params![status, org_id, project_id],
    )?;
    Ok(n > 0)
}

/// Removes every central-registry row of a project (project, memberships,
/// chats, notifications) in one transaction. Per-project data (`project.db`,
/// files, vectors) is removed by the caller BEFORE this step.
pub fn delete_project_rows(project_id: &str) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM project_members WHERE project_id = ?1",
        params![project_id],
    )?;
    tx.execute(
        "DELETE FROM project_chats WHERE project_id = ?1",
        params![project_id],
    )?;
    tx.execute(
        "DELETE FROM notifications WHERE project_id = ?1",
        params![project_id],
    )?;
    tx.execute(
        "DELETE FROM projects WHERE project_id = ?1",
        params![project_id],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn touch_project(project_id: &str) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "UPDATE projects SET updated_at = datetime('now') WHERE project_id = ?1",
        params![project_id],
    )?;
    Ok(())
}

// =============================================================================
// Central registry: members
// =============================================================================

pub fn member_role(project_id: &str, user_id: &str) -> Result<Option<String>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        "SELECT role FROM project_members WHERE project_id = ?1 AND user_id = ?2",
        params![project_id, user_id],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(Into::into)
}

pub fn list_members(project_id: &str) -> Result<Vec<MemberRecord>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(
        "SELECT project_id, user_id, role, invited_by, created_at FROM project_members \
         WHERE project_id = ?1 ORDER BY (role = 'owner') DESC, created_at, user_id",
    )?;
    let rows = stmt.query_map(params![project_id], |row| {
        Ok(MemberRecord {
            project_id: row.get(0)?,
            user_id: row.get(1)?,
            role: row.get(2)?,
            invited_by: row.get(3)?,
            created_at: row.get(4)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn member_count(project_id: &str) -> Result<u32> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM project_members WHERE project_id = ?1",
        params![project_id],
        |row| row.get(0),
    )?;
    Ok(n as u32)
}

/// All memberships of one user, keyed by project id — used to compose the
/// project list without one query per project.
pub fn member_roles_for_user(user_id: &str) -> Result<HashMap<String, String>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let mut stmt =
        conn.prepare("SELECT project_id, role FROM project_members WHERE user_id = ?1")?;
    let rows = stmt.query_map(params![user_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let (project_id, role) = row?;
        out.insert(project_id, role);
    }
    Ok(out)
}

/// Adds members, skipping users that already belong to the project. Returns
/// the number of rows actually inserted.
pub fn add_members(
    project_id: &str,
    members: &[(String, String)],
    invited_by: &str,
) -> Result<u32> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let mut added = 0u32;
    for (user_id, role) in members {
        let n = tx.execute(
            "INSERT OR IGNORE INTO project_members (project_id, user_id, role, invited_by) \
             VALUES (?1, ?2, ?3, ?4)",
            params![project_id, user_id, role, invited_by],
        )?;
        added += n as u32;
    }
    tx.commit()?;
    Ok(added)
}

pub fn set_member_role(project_id: &str, user_id: &str, role: &str) -> Result<bool> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute(
        "UPDATE project_members SET role = ?1 WHERE project_id = ?2 AND user_id = ?3",
        params![role, project_id, user_id],
    )?;
    Ok(n > 0)
}

pub fn remove_member(project_id: &str, user_id: &str) -> Result<bool> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute(
        "DELETE FROM project_members WHERE project_id = ?1 AND user_id = ?2",
        params![project_id, user_id],
    )?;
    Ok(n > 0)
}

/// Atomic ownership transfer: the old owner is demoted to manager, the new
/// owner promoted, and `projects.owner_user_id` updated — one transaction, so
/// the project can never have zero or two owners.
pub fn transfer_ownership(project_id: &str, old_owner: &str, new_owner: &str) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let demoted = tx.execute(
        "UPDATE project_members SET role = 'manager' \
         WHERE project_id = ?1 AND user_id = ?2 AND role = 'owner'",
        params![project_id, old_owner],
    )?;
    if demoted == 0 {
        bail!("current owner membership not found");
    }
    let promoted = tx.execute(
        "UPDATE project_members SET role = 'owner' WHERE project_id = ?1 AND user_id = ?2",
        params![project_id, new_owner],
    )?;
    if promoted == 0 {
        bail!("new owner is not a project member");
    }
    tx.execute(
        "UPDATE projects SET owner_user_id = ?1, updated_at = datetime('now') \
         WHERE project_id = ?2",
        params![new_owner, project_id],
    )?;
    tx.commit()?;
    Ok(())
}

// =============================================================================
// Central registry: creator grants
// =============================================================================

pub fn list_creator_grants(org_id: &str) -> Result<Vec<CreatorGrantRecord>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(
        "SELECT user_id, org_id, granted_by, created_at FROM project_creator_grants \
         WHERE org_id = ?1 ORDER BY created_at",
    )?;
    let rows = stmt.query_map(params![org_id], |row| {
        Ok(CreatorGrantRecord {
            user_id: row.get(0)?,
            org_id: row.get(1)?,
            granted_by: row.get(2)?,
            created_at: row.get(3)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn set_creator_grant(
    user_id: &str,
    org_id: &str,
    granted_by: &str,
    granted: bool,
) -> Result<bool> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let n = if granted {
        conn.execute(
            "INSERT OR REPLACE INTO project_creator_grants (user_id, org_id, granted_by) \
             VALUES (?1, ?2, ?3)",
            params![user_id, org_id, granted_by],
        )?
    } else {
        conn.execute(
            "DELETE FROM project_creator_grants WHERE user_id = ?1 AND org_id = ?2",
            params![user_id, org_id],
        )?
    };
    Ok(n > 0)
}

pub fn has_creator_grant(user_id: &str, org_id: &str) -> Result<bool> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM project_creator_grants WHERE user_id = ?1 AND org_id = ?2",
        params![user_id, org_id],
        |row| row.get(0),
    )?;
    Ok(n > 0)
}

// =============================================================================
// Central registry: chats (private per user — every query filters by user_id)
// =============================================================================

fn read_chat(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChatRecord> {
    Ok(ChatRecord {
        chat_id: row.get(0)?,
        project_id: row.get(1)?,
        user_id: row.get(2)?,
        title: row.get(3)?,
        session_id: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

const CHAT_COLS: &str = "chat_id, project_id, user_id, title, session_id, created_at, updated_at";

pub fn list_chats(project_id: &str, user_id: &str) -> Result<Vec<ChatRecord>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {CHAT_COLS} FROM project_chats \
         WHERE project_id = ?1 AND user_id = ?2 ORDER BY updated_at DESC"
    ))?;
    let rows = stmt.query_map(params![project_id, user_id], read_chat)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn get_chat(project_id: &str, chat_id: &str, user_id: &str) -> Result<Option<ChatRecord>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        &format!(
            "SELECT {CHAT_COLS} FROM project_chats \
             WHERE project_id = ?1 AND chat_id = ?2 AND user_id = ?3"
        ),
        params![project_id, chat_id, user_id],
        read_chat,
    )
    .optional()
    .map_err(Into::into)
}

pub fn create_chat(project_id: &str, user_id: &str, title: &str) -> Result<ChatRecord> {
    let chat_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "INSERT INTO project_chats (chat_id, project_id, user_id, title, session_id) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![chat_id, project_id, user_id, title, session_id],
    )?;
    conn.query_row(
        &format!("SELECT {CHAT_COLS} FROM project_chats WHERE chat_id = ?1"),
        params![chat_id],
        read_chat,
    )
    .map_err(Into::into)
}

pub fn rename_chat(project_id: &str, chat_id: &str, user_id: &str, title: &str) -> Result<bool> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute(
        "UPDATE project_chats SET title = ?1, updated_at = datetime('now') \
         WHERE project_id = ?2 AND chat_id = ?3 AND user_id = ?4",
        params![title, project_id, chat_id, user_id],
    )?;
    Ok(n > 0)
}

/// Bumps `updated_at` so the chat surfaces at the top of the (updated_at DESC)
/// conversation list after a new turn. Owner-scoped like every chat query.
pub fn touch_chat(project_id: &str, chat_id: &str, user_id: &str) -> Result<bool> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute(
        "UPDATE project_chats SET updated_at = datetime('now') \
         WHERE project_id = ?1 AND chat_id = ?2 AND user_id = ?3",
        params![project_id, chat_id, user_id],
    )?;
    Ok(n > 0)
}

pub fn delete_chat(project_id: &str, chat_id: &str, user_id: &str) -> Result<bool> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute(
        "DELETE FROM project_chats WHERE project_id = ?1 AND chat_id = ?2 AND user_id = ?3",
        params![project_id, chat_id, user_id],
    )?;
    Ok(n > 0)
}

pub fn count_chats(project_id: &str, user_id: &str) -> Result<u32> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM project_chats WHERE project_id = ?1 AND user_id = ?2",
        params![project_id, user_id],
        |row| row.get(0),
    )?;
    Ok(n as u32)
}

// =============================================================================
// Core-directory lookups (tentaflow.db — separate pool, no cross-file JOINs)
// =============================================================================

/// Resolves display name + email for a set of user ids from the CORE user
/// directory. Missing ids / unavailable core DB are skipped (frontend falls
/// back to the raw UUID). Never panics.
pub fn resolve_user_refs(user_ids: &[String]) -> HashMap<String, (String, String)> {
    let mut out = HashMap::new();
    let Some(core) = crate::db::global_pool() else {
        return out;
    };
    let Ok(conn) = core.read() else {
        return out;
    };
    for id in user_ids {
        let row: Option<(String, String)> = conn
            .query_row(
                "SELECT COALESCE(NULLIF(display_name, ''), NULLIF(username, ''), id), \
                        COALESCE(email, '') \
                 FROM user_accounts WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .ok()
            .flatten();
        if let Some(pair) = row {
            out.insert(id.clone(), pair);
        }
    }
    out
}

/// Active users of the org matching `query` (against username/display
/// name/email), for the member pickers. Existing-member exclusion happens in
/// the dispatcher (memberships live in a different SQLite file).
pub fn list_org_user_candidates(
    org_id: &str,
    query: &str,
    limit: u32,
) -> Result<Vec<(String, String, String)>> {
    let core = crate::db::global_pool().ok_or_else(|| anyhow!("core directory unavailable"))?;
    let conn = core.read().map_err(read_err)?;
    let like = format!("%{}%", escape_like(query.trim()));
    let mut stmt = conn.prepare(
        "SELECT u.id, COALESCE(NULLIF(u.display_name, ''), NULLIF(u.username, ''), u.id), \
                COALESCE(u.email, '') \
         FROM user_accounts u \
         JOIN org_memberships m ON m.user_id = u.id \
         WHERE m.org_id = ?1 AND u.is_active = 1 \
           AND (u.username LIKE ?2 ESCAPE '\\' OR u.display_name LIKE ?2 ESCAPE '\\' \
                OR u.email LIKE ?2 ESCAPE '\\') \
         ORDER BY u.display_name, u.username LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![org_id, like, limit as i64], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn is_org_member(org_id: &str, user_id: &str) -> Result<bool> {
    let core = crate::db::global_pool().ok_or_else(|| anyhow!("core directory unavailable"))?;
    let conn = core.read().map_err(read_err)?;
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM org_memberships WHERE org_id = ?1 AND user_id = ?2",
        params![org_id, user_id],
        |row| row.get(0),
    )?;
    Ok(n > 0)
}

/// Resolves an agent's display name + model label from the core `agents`
/// table for the settings screen. Missing agent → `None` (the binding then
/// falls back to the platform default).
pub fn resolve_agent_label(agent_id: &str) -> Option<(String, String)> {
    let core = crate::db::global_pool()?;
    let conn = core.read().ok()?;
    conn.query_row(
        "SELECT COALESCE(NULLIF(display_name, ''), name), COALESCE(model, '') \
         FROM agents WHERE id = ?1",
        params![agent_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )
    .optional()
    .ok()
    .flatten()
}

// =============================================================================
// Per-project: sources
// =============================================================================

fn read_source(row: &rusqlite::Row<'_>) -> rusqlite::Result<SourceRecord> {
    Ok(SourceRecord {
        source_id: row.get(0)?,
        kind: row.get(1)?,
        name: row.get(2)?,
        status: row.get(3)?,
        config_json: row.get(4)?,
        error: row.get(5)?,
        created_by: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

const SOURCE_COLS: &str =
    "source_id, kind, name, status, config_json, error, created_by, created_at, updated_at";

pub fn create_source(
    pool: &DbPool,
    source_id: &str,
    kind: &str,
    name: &str,
    config_json: &str,
    created_by: &str,
) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "INSERT INTO sources (source_id, kind, name, config_json, created_by) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![source_id, kind, name, config_json, created_by],
    )?;
    Ok(())
}

pub fn get_source(pool: &DbPool, source_id: &str) -> Result<Option<SourceRecord>> {
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        &format!("SELECT {SOURCE_COLS} FROM sources WHERE source_id = ?1"),
        params![source_id],
        read_source,
    )
    .optional()
    .map_err(Into::into)
}

pub fn list_sources(pool: &DbPool) -> Result<Vec<SourceListItem>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {SOURCE_COLS} FROM sources ORDER BY created_at DESC"
    ))?;
    let sources = stmt
        .query_map([], read_source)?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let mut out = Vec::with_capacity(sources.len());
    for record in sources {
        let (file_count, chunk_count): (i64, i64) = conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(chunk_count), 0) FROM source_files \
             WHERE source_id = ?1",
            params![record.source_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let last_job = conn
            .query_row(
                &format!(
                    "SELECT {JOB_COLS} FROM ingest_jobs WHERE source_id = ?1 \
                     ORDER BY started_at DESC, job_id DESC LIMIT 1"
                ),
                params![record.source_id],
                read_job,
            )
            .optional()?;
        out.push(SourceListItem {
            record,
            file_count: file_count as u32,
            chunk_count: chunk_count as u32,
            last_job,
        });
    }
    Ok(out)
}

pub fn update_source_meta(
    pool: &DbPool,
    source_id: &str,
    name: &str,
    config_json: &str,
) -> Result<bool> {
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute(
        "UPDATE sources SET name = ?1, config_json = ?2, updated_at = datetime('now') \
         WHERE source_id = ?3",
        params![name, config_json, source_id],
    )?;
    Ok(n > 0)
}

pub fn set_source_status(pool: &DbPool, source_id: &str, status: &str, error: &str) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "UPDATE sources SET status = ?1, error = ?2, updated_at = datetime('now') \
         WHERE source_id = ?3",
        params![status, error, source_id],
    )?;
    Ok(())
}

/// Removes the source row together with its files and jobs. Vector/blob
/// cleanup happens in the dispatcher BEFORE this call.
pub fn delete_source_rows(pool: &DbPool, source_id: &str) -> Result<bool> {
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM source_files WHERE source_id = ?1",
        params![source_id],
    )?;
    tx.execute(
        "DELETE FROM ingest_jobs WHERE source_id = ?1",
        params![source_id],
    )?;
    let n = tx.execute(
        "DELETE FROM sources WHERE source_id = ?1",
        params![source_id],
    )?;
    tx.commit()?;
    Ok(n > 0)
}

// =============================================================================
// Per-project: source files
// =============================================================================

fn read_file(row: &rusqlite::Row<'_>) -> rusqlite::Result<SourceFileRecord> {
    Ok(SourceFileRecord {
        file_id: row.get(0)?,
        source_id: row.get(1)?,
        path: row.get(2)?,
        sha256: row.get(3)?,
        size_bytes: row.get::<_, i64>(4)? as u64,
        mime: row.get(5)?,
        status: row.get(6)?,
        error: row.get(7)?,
        chunk_count: row.get::<_, i64>(8)? as u32,
        updated_at: row.get(9)?,
    })
}

const FILE_COLS: &str =
    "file_id, source_id, path, sha256, size_bytes, mime, status, error, chunk_count, updated_at";

/// Inserts (or refreshes, on `UNIQUE(source_id, path)` conflict) a file row
/// and resets it to `pending`. Returns the effective `file_id` — a re-upload
/// of the same path keeps the original id so its vectors are replaced, not
/// duplicated.
pub fn upsert_source_file(
    pool: &DbPool,
    source_id: &str,
    path: &str,
    sha256: &str,
    size_bytes: u64,
    mime: &str,
) -> Result<String> {
    let conn = pool.write().map_err(write_err)?;
    let existing: Option<String> = conn
        .query_row(
            "SELECT file_id FROM source_files WHERE source_id = ?1 AND path = ?2",
            params![source_id, path],
            |row| row.get(0),
        )
        .optional()?;
    match existing {
        Some(file_id) => {
            conn.execute(
                "UPDATE source_files SET sha256 = ?1, size_bytes = ?2, mime = ?3, \
                 status = 'pending', error = '', updated_at = datetime('now') \
                 WHERE file_id = ?4",
                params![sha256, size_bytes as i64, mime, file_id],
            )?;
            Ok(file_id)
        }
        None => {
            let file_id = uuid::Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO source_files (file_id, source_id, path, sha256, size_bytes, mime) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![file_id, source_id, path, sha256, size_bytes as i64, mime],
            )?;
            Ok(file_id)
        }
    }
}

pub fn get_source_file(pool: &DbPool, file_id: &str) -> Result<Option<SourceFileRecord>> {
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        &format!("SELECT {FILE_COLS} FROM source_files WHERE file_id = ?1"),
        params![file_id],
        read_file,
    )
    .optional()
    .map_err(Into::into)
}

pub fn list_source_files(
    pool: &DbPool,
    source_id: &str,
    offset: u32,
    limit: u32,
    filter: &str,
) -> Result<(Vec<SourceFileRecord>, u32)> {
    let conn = pool.read().map_err(read_err)?;
    let like = format!("%{}%", escape_like(filter.trim()));
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM source_files WHERE source_id = ?1 AND path LIKE ?2 ESCAPE '\\'",
        params![source_id, like],
        |row| row.get(0),
    )?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {FILE_COLS} FROM source_files WHERE source_id = ?1 AND path LIKE ?2 ESCAPE '\\' \
         ORDER BY path LIMIT ?3 OFFSET ?4"
    ))?;
    let rows = stmt.query_map(
        params![source_id, like, limit as i64, offset as i64],
        read_file,
    )?;
    let files = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok((files, total as u32))
}

/// Files to process in an ingest job: the whole source or a single file.
pub fn files_for_ingest(
    pool: &DbPool,
    source_id: &str,
    only_file: Option<&str>,
) -> Result<Vec<SourceFileRecord>> {
    let conn = pool.read().map_err(read_err)?;
    match only_file {
        Some(file_id) => {
            let mut stmt = conn.prepare(&format!(
                "SELECT {FILE_COLS} FROM source_files WHERE source_id = ?1 AND file_id = ?2"
            ))?;
            let rows = stmt.query_map(params![source_id, file_id], read_file)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Into::into)
        }
        None => {
            let mut stmt = conn.prepare(&format!(
                "SELECT {FILE_COLS} FROM source_files WHERE source_id = ?1 ORDER BY path"
            ))?;
            let rows = stmt.query_map(params![source_id], read_file)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Into::into)
        }
    }
}

pub fn delete_source_file_row(pool: &DbPool, file_id: &str) -> Result<bool> {
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute(
        "DELETE FROM source_files WHERE file_id = ?1",
        params![file_id],
    )?;
    Ok(n > 0)
}

/// How many rows still reference the blob `files/<sha256>`: source_files rows
/// PLUS attachment entries embedded in `attachments_json` of test cases, run
/// items, run steps and tasks (json_each over the arrays). The blob may only
/// be removed when this drops to zero — counting source_files alone would let
/// the GC and the source-delete paths eat tester screenshots (risk F.6).
pub fn blob_ref_count(pool: &DbPool, sha256: &str) -> Result<u32> {
    let conn = pool.read().map_err(read_err)?;
    let n: i64 = conn.query_row(
        "SELECT (SELECT COUNT(*) FROM source_files WHERE sha256 = ?1) \
              + (SELECT COUNT(*) FROM test_cases t, json_each(t.attachments_json) j \
                 WHERE json_extract(j.value, '$.sha256') = ?1) \
              + (SELECT COUNT(*) FROM test_run_items t, json_each(t.attachments_json) j \
                 WHERE json_extract(j.value, '$.sha256') = ?1) \
              + (SELECT COUNT(*) FROM test_run_steps t, json_each(t.attachments_json) j \
                 WHERE json_extract(j.value, '$.sha256') = ?1) \
              + (SELECT COUNT(*) FROM tasks t, json_each(t.attachments_json) j \
                 WHERE json_extract(j.value, '$.sha256') = ?1)",
        params![sha256],
        |row| row.get(0),
    )?;
    Ok(n as u32)
}

/// Every sha256 referenced anywhere (source_files + the four attachments_json
/// tables), for the files-dir GC — one pass over the tables instead of a
/// per-blob `blob_ref_count` query for each on-disk file.
pub fn referenced_blob_sha256s(pool: &DbPool) -> Result<std::collections::HashSet<String>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(
        "SELECT sha256 FROM source_files \
         UNION \
         SELECT json_extract(j.value, '$.sha256') \
           FROM test_cases t, json_each(t.attachments_json) j \
         UNION \
         SELECT json_extract(j.value, '$.sha256') \
           FROM test_run_items t, json_each(t.attachments_json) j \
         UNION \
         SELECT json_extract(j.value, '$.sha256') \
           FROM test_run_steps t, json_each(t.attachments_json) j \
         UNION \
         SELECT json_extract(j.value, '$.sha256') \
           FROM tasks t, json_each(t.attachments_json) j",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, Option<String>>(0))?;
    let mut set = std::collections::HashSet::new();
    for row in rows {
        if let Some(sha) = row? {
            set.insert(sha);
        }
    }
    Ok(set)
}

/// Resolves the MIME type recorded for an attachment blob: the first matching
/// attachment entry across the four attachment-bearing tables, falling back to
/// source_files (an attachment may share content with an uploaded source).
pub fn attachment_mime(pool: &DbPool, sha256: &str) -> Result<Option<String>> {
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        "SELECT mime FROM ( \
            SELECT json_extract(j.value, '$.mime') AS mime \
              FROM test_cases t, json_each(t.attachments_json) j \
             WHERE json_extract(j.value, '$.sha256') = ?1 \
            UNION ALL \
            SELECT json_extract(j.value, '$.mime') \
              FROM test_run_items t, json_each(t.attachments_json) j \
             WHERE json_extract(j.value, '$.sha256') = ?1 \
            UNION ALL \
            SELECT json_extract(j.value, '$.mime') \
              FROM test_run_steps t, json_each(t.attachments_json) j \
             WHERE json_extract(j.value, '$.sha256') = ?1 \
            UNION ALL \
            SELECT json_extract(j.value, '$.mime') \
              FROM tasks t, json_each(t.attachments_json) j \
             WHERE json_extract(j.value, '$.sha256') = ?1 \
            UNION ALL \
            SELECT mime FROM source_files WHERE sha256 = ?1 \
         ) WHERE mime IS NOT NULL AND mime <> '' LIMIT 1",
        params![sha256],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(Into::into)
}

// =============================================================================
// Per-project: ingest jobs
// =============================================================================

fn read_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<IngestJobRecord> {
    Ok(IngestJobRecord {
        job_id: row.get(0)?,
        source_id: row.get(1)?,
        status: row.get(2)?,
        files_total: row.get::<_, i64>(3)? as u32,
        files_done: row.get::<_, i64>(4)? as u32,
        chunks_done: row.get::<_, i64>(5)? as u32,
        error: row.get(6)?,
        started_by: row.get(7)?,
        started_at: row.get(8)?,
        finished_at: row.get(9)?,
    })
}

const JOB_COLS: &str = "job_id, source_id, status, files_total, files_done, chunks_done, \
     error, started_by, started_at, finished_at";

pub fn create_ingest_job(
    pool: &DbPool,
    job_id: &str,
    source_id: &str,
    files_total: u32,
    started_by: &str,
) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "INSERT INTO ingest_jobs (job_id, source_id, files_total, started_by) \
         VALUES (?1, ?2, ?3, ?4)",
        params![job_id, source_id, files_total as i64, started_by],
    )?;
    Ok(())
}

pub fn get_ingest_job(pool: &DbPool, job_id: &str) -> Result<Option<IngestJobRecord>> {
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        &format!("SELECT {JOB_COLS} FROM ingest_jobs WHERE job_id = ?1"),
        params![job_id],
        read_job,
    )
    .optional()
    .map_err(Into::into)
}

pub fn running_job_ids(pool: &DbPool) -> Result<Vec<String>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare("SELECT job_id FROM ingest_jobs WHERE status = 'running'")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// One batch of per-file progress: the file row and the job counters commit
/// in a SINGLE transaction so a crash never shows a done file without the
/// matching job progress (or vice versa).
pub fn record_file_progress(
    pool: &DbPool,
    job_id: &str,
    file_id: &str,
    file_status: &str,
    file_error: &str,
    chunk_count: u32,
    chunks_done_inc: u32,
) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE source_files SET status = ?1, error = ?2, chunk_count = ?3, \
         updated_at = datetime('now') WHERE file_id = ?4",
        params![file_status, file_error, chunk_count as i64, file_id],
    )?;
    tx.execute(
        "UPDATE ingest_jobs SET files_done = files_done + 1, chunks_done = chunks_done + ?1 \
         WHERE job_id = ?2",
        params![chunks_done_inc as i64, job_id],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn mark_file_indexing(pool: &DbPool, file_id: &str) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "UPDATE source_files SET status = 'indexing', updated_at = datetime('now') \
         WHERE file_id = ?1",
        params![file_id],
    )?;
    Ok(())
}

pub fn finish_ingest_job(pool: &DbPool, job_id: &str, status: &str, error: &str) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "UPDATE ingest_jobs SET status = ?1, error = ?2, finished_at = datetime('now') \
         WHERE job_id = ?3",
        params![status, error, job_id],
    )?;
    Ok(())
}

// =============================================================================
// Per-project: activity, settings, tags, KPIs
// =============================================================================

pub fn insert_activity(
    pool: &DbPool,
    actor_user_id: &str,
    actor_kind: &str,
    action: &str,
    object_type: &str,
    object_id: &str,
    details_json: &str,
) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "INSERT INTO activity_log (actor_user_id, actor_kind, action, object_type, \
         object_id, details_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            actor_user_id,
            actor_kind,
            action,
            object_type,
            object_id,
            details_json
        ],
    )?;
    Ok(())
}

/// Keyset pagination newest-first: `before_id = None` starts at the top,
/// otherwise only entries with `id < before_id` are returned.
pub fn list_activity(
    pool: &DbPool,
    before_id: Option<i64>,
    limit: u32,
) -> Result<(Vec<ActivityRecord>, bool)> {
    let conn = pool.read().map_err(read_err)?;
    let fetch = (limit as i64) + 1;
    let mut stmt = conn.prepare(
        "SELECT id, actor_user_id, actor_kind, action, object_type, object_id, \
         details_json, created_at FROM activity_log \
         WHERE (?1 IS NULL OR id < ?1) ORDER BY id DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![before_id, fetch], |row| {
        Ok(ActivityRecord {
            id: row.get(0)?,
            actor_user_id: row.get(1)?,
            actor_kind: row.get(2)?,
            action: row.get(3)?,
            object_type: row.get(4)?,
            object_id: row.get(5)?,
            details_json: row.get(6)?,
            created_at: row.get(7)?,
        })
    })?;
    let mut entries = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    let has_more = entries.len() as i64 > limit as i64;
    entries.truncate(limit as usize);
    Ok((entries, has_more))
}

pub fn get_setting(pool: &DbPool, key: &str) -> Result<Option<String>> {
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        params![key],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(Into::into)
}

pub fn set_setting(pool: &DbPool, key: &str, value: &str) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub fn list_tags(pool: &DbPool) -> Result<Vec<TagRecord>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare("SELECT tag_id, name FROM tags ORDER BY name COLLATE NOCASE")?;
    let rows = stmt.query_map([], |row| {
        Ok(TagRecord {
            tag_id: row.get(0)?,
            name: row.get(1)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Creates or renames a tag. A `UNIQUE COLLATE NOCASE` clash surfaces as an
/// `Err` with "UNIQUE" in the message (dispatcher maps it to BadRequest).
pub fn upsert_tag(
    pool: &DbPool,
    tag_id: Option<&str>,
    name: &str,
    created_by: &str,
) -> Result<String> {
    let conn = pool.write().map_err(write_err)?;
    match tag_id {
        Some(id) => {
            let n = conn.execute(
                "UPDATE tags SET name = ?1 WHERE tag_id = ?2",
                params![name, id],
            )?;
            if n == 0 {
                bail!("tag not found");
            }
            Ok(id.to_string())
        }
        None => {
            let id = uuid::Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO tags (tag_id, name, created_by) VALUES (?1, ?2, ?3)",
                params![id, name, created_by],
            )?;
            Ok(id)
        }
    }
}

pub fn delete_tag(pool: &DbPool, tag_id: &str) -> Result<bool> {
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute("DELETE FROM tags WHERE tag_id = ?1", params![tag_id])?;
    Ok(n > 0)
}

pub fn project_kpis(pool: &DbPool) -> Result<ProjectKpis> {
    let conn = pool.read().map_err(read_err)?;
    let (sources_total, sources_ready): (i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(status = 'ready'), 0) FROM sources",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let (files_total, chunks_total): (i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(chunk_count), 0) FROM source_files",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let open_jobs: i64 = conn.query_row(
        "SELECT COUNT(*) FROM ingest_jobs WHERE status = 'running'",
        [],
        |row| row.get(0),
    )?;
    Ok(ProjectKpis {
        sources_total: sources_total as u32,
        sources_ready: sources_ready as u32,
        files_total: files_total as u32,
        chunks_total: chunks_total as u32,
        open_ingest_jobs: open_jobs as u32,
    })
}

/// F2 KPI counters for the overview screen. Pending agent output is excluded
/// from case counters (same visibility rule as every case query);
/// `my_run_items_pending` counts items of RUNNING runs assigned to the caller
/// or claimable from the pool.
pub fn project_f2_kpis(pool: &DbPool, user_id: &str) -> Result<super::models::ProjectF2Kpis> {
    let conn = pool.read().map_err(read_err)?;
    let (cases_total, cases_approved): (i64, i64) = conn.query_row(
        &format!(
            "SELECT COUNT(*), COALESCE(SUM(status = 'approved'), 0) FROM test_cases WHERE {}",
            super::tests::VISIBLE_CASES_PREDICATE
        ),
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let suites_total: i64 =
        conn.query_row("SELECT COUNT(*) FROM test_suites", [], |row| row.get(0))?;
    let runs_open: i64 = conn.query_row(
        "SELECT COUNT(*) FROM test_runs WHERE status = 'running'",
        [],
        |row| row.get(0),
    )?;
    let my_pending: i64 = conn.query_row(
        "SELECT COUNT(*) FROM test_run_items i \
         JOIN test_runs r ON r.run_id = i.run_id AND r.status = 'running' \
         WHERE i.status = 'pending' AND (i.assigned_to = ?1 OR i.assigned_to = '')",
        params![user_id],
        |row| row.get(0),
    )?;
    let (tasks_open, defects_open): (i64, i64) = conn.query_row(
        "SELECT COALESCE(SUM(task_type = 'task'), 0), COALESCE(SUM(task_type = 'defect'), 0) \
         FROM tasks WHERE status <> 'done'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let generations_running: i64 = conn.query_row(
        "SELECT COUNT(*) FROM generation_runs WHERE status = 'running'",
        [],
        |row| row.get(0),
    )?;
    Ok(super::models::ProjectF2Kpis {
        cases_total: cases_total as u32,
        cases_approved: cases_approved as u32,
        suites_total: suites_total as u32,
        runs_open: runs_open as u32,
        my_run_items_pending: my_pending as u32,
        tasks_open: tasks_open as u32,
        defects_open: defects_open as u32,
        generations_running: generations_running as u32,
    })
}

/// F3 KPI counters (environments + open automated runs) for the overview.
pub fn project_f3_kpis(pool: &DbPool) -> Result<super::models::ProjectF3Kpis> {
    let conn = pool.read().map_err(read_err)?;
    let (approved, pending): (i64, i64) = conn.query_row(
        "SELECT COALESCE(SUM(approval_status = 'approved'), 0), \
                COALESCE(SUM(approval_status = 'pending'), 0) FROM environments",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let auto_runs_open: i64 = conn.query_row(
        "SELECT COUNT(*) FROM test_runs WHERE status = 'running' \
         AND run_id IN (SELECT run_id FROM auto_run_meta)",
        [],
        |row| row.get(0),
    )?;
    Ok(super::models::ProjectF3Kpis {
        environments_approved: approved as u32,
        environments_pending: pending as u32,
        auto_runs_open: auto_runs_open as u32,
    })
}

/// Stores the encrypted access token of a git source (input-only on the wire).
pub fn set_source_secret(pool: &DbPool, source_id: &str, secret_enc: &str) -> Result<bool> {
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute(
        "UPDATE sources SET secret_enc = ?1, updated_at = datetime('now') WHERE source_id = ?2",
        params![secret_enc, source_id],
    )?;
    Ok(n > 0)
}

/// Encrypted access token of a source; `""` when none is stored.
pub fn get_source_secret_enc(pool: &DbPool, source_id: &str) -> Result<String> {
    let conn = pool.read().map_err(read_err)?;
    Ok(conn
        .query_row(
            "SELECT secret_enc FROM sources WHERE source_id = ?1",
            params![source_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .unwrap_or_default())
}

/// Sets the language of a case. Separate statement on purpose: F2's
/// `CaseContentInput` predates code cases and carries no language field, and
/// widening it would churn every manual-test call site.
pub fn set_case_language(pool: &DbPool, case_id: &str, language: &str) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "UPDATE test_cases SET language = ?1 WHERE case_id = ?2",
        params![language, case_id],
    )?;
    Ok(())
}

/// Replaces the file rows of a code source with the freshly collected tree and
/// returns the ids of the files that need (re-)embedding plus the ids of the
/// rows that were dropped (their vectors are deleted by the caller). ONE
/// transaction, so a crash never leaves the file list half-rewritten.
pub fn sync_tree_files(
    pool: &DbPool,
    source_id: &str,
    files: &[super::ingest::CollectedFile],
    delta: &super::ingest::TreeDelta,
) -> Result<(Vec<String>, Vec<String>)> {
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let mut removed_ids = Vec::with_capacity(delta.removed.len());
    for path in &delta.removed {
        let file_id: Option<String> = tx
            .query_row(
                "SELECT file_id FROM source_files WHERE source_id = ?1 AND path = ?2",
                params![source_id, path],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(file_id) = file_id {
            tx.execute(
                "DELETE FROM source_files WHERE file_id = ?1",
                params![file_id],
            )?;
            removed_ids.push(file_id);
        }
    }
    let touched: std::collections::HashSet<&str> = delta
        .added
        .iter()
        .chain(delta.changed.iter())
        .map(|p| p.as_str())
        .collect();
    let mut work_ids = Vec::with_capacity(touched.len());
    for file in files {
        if !touched.contains(file.rel_path.as_str()) {
            continue;
        }
        let existing: Option<String> = tx
            .query_row(
                "SELECT file_id FROM source_files WHERE source_id = ?1 AND path = ?2",
                params![source_id, file.rel_path],
                |row| row.get(0),
            )
            .optional()?;
        let file_id = match existing {
            Some(file_id) => {
                tx.execute(
                    "UPDATE source_files SET sha256 = ?1, size_bytes = ?2, mime = ?3, \
                     status = 'pending', error = '', updated_at = datetime('now') \
                     WHERE file_id = ?4",
                    params![file.sha256, file.size_bytes as i64, file.mime, file_id],
                )?;
                file_id
            }
            None => {
                let file_id = uuid::Uuid::new_v4().to_string();
                tx.execute(
                    "INSERT INTO source_files (file_id, source_id, path, sha256, size_bytes, mime) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        file_id,
                        source_id,
                        file.rel_path,
                        file.sha256,
                        file.size_bytes as i64,
                        file.mime
                    ],
                )?;
                file_id
            }
        };
        work_ids.push(file_id);
    }
    tx.commit()?;
    Ok((work_ids, removed_ids))
}

/// Read-only source/file counters for the project LIST screen. Opens the
/// project.db file directly (read-only, no pool) so listing many projects
/// does not churn the LRU pool cache; a missing/unreadable file counts as
/// zero (freshly created or already deleted project).
pub fn read_source_counts(dir_path: &str) -> (u32, u32) {
    let db_path = std::path::Path::new(dir_path).join("project.db");
    let Ok(conn) =
        rusqlite::Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return (0, 0);
    };
    conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(status = 'ready'), 0) FROM sources",
        [],
        |row| Ok((row.get::<_, i64>(0)? as u32, row.get::<_, i64>(1)? as u32)),
    )
    .unwrap_or((0, 0))
}

// =============================================================================
// Role gate helper shared by dispatcher + stream handler
// =============================================================================

/// Effective role of `user_id` in the project, or `None` for a non-member.
pub fn effective_role(project_id: &str, user_id: &str) -> Result<Option<ProjectRole>> {
    Ok(member_role(project_id, user_id)?
        .as_deref()
        .and_then(ProjectRole::from_slug))
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    fn pool() -> DbPool {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("proj");
        std::fs::create_dir_all(&dir).expect("dir");
        let (pool, _) = super::super::project_db::open_pool_at(&dir).expect("open");
        std::mem::forget(tmp);
        pool
    }

    fn file(path: &str, sha: &str) -> super::super::ingest::CollectedFile {
        super::super::ingest::CollectedFile {
            rel_path: path.to_string(),
            sha256: sha.to_string(),
            size_bytes: 10,
            mime: "text/plain".to_string(),
        }
    }

    /// (c) A git refresh re-embeds ONLY the delta: an unchanged file keeps its
    /// row (and its file_id, so its vectors are reused), a changed one is
    /// returned for re-ingest, a new one is inserted and a vanished one is
    /// deleted with its id handed back for vector cleanup.
    #[test]
    fn sync_tree_files_returns_only_the_delta() {
        let pool = pool();
        create_source(&pool, "s1", "git", "repo", "{}", "u1").expect("source");

        let initial = vec![file("a.rs", "aaa"), file("b.rs", "bbb")];
        let full = super::super::ingest::TreeDelta {
            added: vec!["a.rs".to_string(), "b.rs".to_string()],
            ..Default::default()
        };
        let (work, removed) = sync_tree_files(&pool, "s1", &initial, &full).expect("initial sync");
        assert_eq!(work.len(), 2);
        assert!(removed.is_empty());
        let stored = super::super::git_source::stored_file_hashes(&pool, "s1").expect("hashes");
        assert_eq!(stored.get("a.rs").map(String::as_str), Some("aaa"));

        // b.rs changed, c.rs is new, a.rs is untouched.
        let current = vec![file("a.rs", "aaa"), file("b.rs", "BBB2"), file("c.rs", "ccc")];
        let delta = super::super::ingest::diff_tree(&stored, &current);
        assert_eq!(delta.added, vec!["c.rs".to_string()]);
        assert_eq!(delta.changed, vec!["b.rs".to_string()]);
        assert!(delta.removed.is_empty());

        let unchanged_id = {
            let conn = pool.read().expect("read");
            conn.query_row(
                "SELECT file_id FROM source_files WHERE source_id = 's1' AND path = 'a.rs'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("a.rs id")
        };
        let (work, removed) = sync_tree_files(&pool, "s1", &current, &delta).expect("delta sync");
        assert_eq!(work.len(), 2, "only the changed + added files are re-ingested");
        assert!(removed.is_empty());
        assert!(
            !work.contains(&unchanged_id),
            "an unchanged file must not be re-embedded"
        );

        // Now b.rs disappears from the tree.
        let stored = super::super::git_source::stored_file_hashes(&pool, "s1").expect("hashes");
        let current = vec![file("a.rs", "aaa"), file("c.rs", "ccc")];
        let delta = super::super::ingest::diff_tree(&stored, &current);
        assert_eq!(delta.removed, vec!["b.rs".to_string()]);
        let (work, removed) = sync_tree_files(&pool, "s1", &current, &delta).expect("removal sync");
        assert!(work.is_empty(), "nothing changed, so nothing is re-embedded");
        assert_eq!(removed.len(), 1, "the vanished file id feeds vector cleanup");
        let stored = super::super::git_source::stored_file_hashes(&pool, "s1").expect("hashes");
        assert_eq!(stored.len(), 2);
        assert!(!stored.contains_key("b.rs"));
    }
}
