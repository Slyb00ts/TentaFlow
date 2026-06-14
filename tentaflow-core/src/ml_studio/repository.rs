// ===== File: ml_studio/repository.rs — SQL access for ML Studio projects =====

use anyhow::{bail, Result};
use rusqlite::{params, OptionalExtension};

use super::models::{MemberStatus, Project, ProjectMember, ProjectRole, ProjectSummary, ProjectType};

/// Lists projects the user is an active member of (owner or invited-and-accepted),
/// newest first, each with its per-project KPIs (dataset/model count) plus the
/// user's role and an `is_owner` flag. Membership is the access boundary: a
/// project the user is not a member of is invisible here.
pub fn list_projects(user_id: &str) -> Result<Vec<ProjectSummary>> {
    let pool = super::db::pool()?;
    let conn = pool.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT p.project_id, p.name, p.description, p.project_type, p.status, \
                p.owner_user_id, p.org_id, p.created_at, p.updated_at, \
                (SELECT COUNT(*) FROM models m WHERE m.project_id = p.project_id), \
                (SELECT COUNT(*) FROM datasets d WHERE d.project_id = p.project_id), \
                pm.role \
         FROM projects p \
         JOIN project_members pm ON pm.project_id = p.project_id \
         WHERE pm.user_id = ?1 AND pm.status IN ('active', 'invited') \
         ORDER BY p.updated_at DESC, p.name",
    )?;
    let rows = stmt.query_map(params![user_id], read_summary)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Creates a new project owned by the calling user inside their organization.
/// Validates the name and project type; returns the created summary.
pub fn create_project(
    owner_user_id: &str,
    org_id: &str,
    name: &str,
    description: &str,
    project_type: &str,
) -> Result<ProjectSummary> {
    let name = name.trim();
    if name.is_empty() {
        bail!("project name is required");
    }
    if name.chars().count() > 128 {
        bail!("project name must be at most 128 characters");
    }
    if description.chars().count() > 4096 {
        bail!("project description must be at most 4096 characters");
    }
    let kind = ProjectType::from_slug(project_type)
        .ok_or_else(|| anyhow::anyhow!("unknown project_type '{}'", project_type))?;

    let project_id = uuid::Uuid::new_v4().to_string();
    let pool = super::db::pool()?;
    let conn = pool.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO projects \
             (project_id, name, description, project_type, status, owner_user_id, org_id) \
         VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?6)",
        params![
            project_id,
            name,
            description,
            kind.slug(),
            owner_user_id,
            org_id
        ],
    )?;
    tx.execute(
        "INSERT INTO project_members \
             (project_id, user_id, role, status, invited_by) \
         VALUES (?1, ?2, 'owner', 'active', ?2)",
        params![project_id, owner_user_id],
    )?;
    tx.commit()?;
    drop(conn);

    get_project(owner_user_id, &project_id)?
        .ok_or_else(|| anyhow::anyhow!("project not found after create"))
}

/// Fetches a single project (with KPIs and the user's role) scoped to the user's
/// active membership. Returns `None` when the user is not an active member, so a
/// non-member cannot probe a project's existence by id.
pub fn get_project(user_id: &str, project_id: &str) -> Result<Option<ProjectSummary>> {
    let pool = super::db::pool()?;
    let conn = pool.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    conn.query_row(
        "SELECT p.project_id, p.name, p.description, p.project_type, p.status, \
                p.owner_user_id, p.org_id, p.created_at, p.updated_at, \
                (SELECT COUNT(*) FROM models m WHERE m.project_id = p.project_id), \
                (SELECT COUNT(*) FROM datasets d WHERE d.project_id = p.project_id), \
                pm.role \
         FROM projects p \
         JOIN project_members pm ON pm.project_id = p.project_id \
         WHERE pm.user_id = ?1 AND pm.status IN ('active', 'invited') AND p.project_id = ?2",
        params![user_id, project_id],
        read_summary,
    )
    .optional()
    .map_err(Into::into)
}

/// Returns the membership role slug of `user_id` in `project_id`, or `None` when
/// the user has no membership row. Used as the authorization primitive for
/// owner-only project actions. Includes `invited` rows so a pending invitation
/// is distinguishable from no membership at all.
pub fn member_role(project_id: &str, user_id: &str) -> Result<Option<String>> {
    let pool = super::db::pool()?;
    let conn = pool.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    conn.query_row(
        "SELECT role FROM project_members WHERE project_id = ?1 AND user_id = ?2",
        params![project_id, user_id],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(Into::into)
}

/// Lists every member of a project (active and invited), owner first then by
/// creation time. No authorization is enforced here; callers gate visibility.
pub fn list_members(project_id: &str) -> Result<Vec<ProjectMember>> {
    let pool = super::db::pool()?;
    let conn = pool.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT project_id, user_id, role, status, invited_by, created_at \
         FROM project_members \
         WHERE project_id = ?1 \
         ORDER BY (role = 'owner') DESC, created_at, user_id",
    )?;
    let rows = stmt.query_map(params![project_id], read_member)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Invites a user to a project with a grantable role (`editor`/`viewer`). Only
/// the project owner may invite. The new row is created with `status = invited`.
pub fn invite_member(
    project_id: &str,
    inviter_user_id: &str,
    invitee_user_id: &str,
    role: &str,
) -> Result<ProjectMember> {
    let role = ProjectRole::from_grantable_slug(role)
        .ok_or_else(|| anyhow::anyhow!("role must be 'editor' or 'viewer'"))?;
    let invitee_user_id = invitee_user_id.trim();
    if invitee_user_id.is_empty() {
        bail!("invitee user id is required");
    }
    if invitee_user_id == inviter_user_id {
        bail!("cannot invite yourself");
    }

    let pool = super::db::pool()?;
    let conn = pool.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    require_owner(&conn, project_id, inviter_user_id)?;

    let existing: Option<String> = conn
        .query_row(
            "SELECT role FROM project_members WHERE project_id = ?1 AND user_id = ?2",
            params![project_id, invitee_user_id],
            |row| row.get(0),
        )
        .optional()?;
    if existing.is_some() {
        bail!("user is already a member of this project");
    }

    conn.execute(
        "INSERT INTO project_members \
             (project_id, user_id, role, status, invited_by) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            project_id,
            invitee_user_id,
            role.slug(),
            MemberStatus::Invited.slug(),
            inviter_user_id
        ],
    )?;

    conn.query_row(
        "SELECT project_id, user_id, role, status, invited_by, created_at \
         FROM project_members WHERE project_id = ?1 AND user_id = ?2",
        params![project_id, invitee_user_id],
        read_member,
    )
    .map_err(Into::into)
}

/// Removes a member from a project. Only the owner may remove, and the owner
/// row itself cannot be removed.
pub fn remove_member(
    project_id: &str,
    requester_user_id: &str,
    target_user_id: &str,
) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    require_owner(&conn, project_id, requester_user_id)?;

    let target_role = conn
        .query_row(
            "SELECT role FROM project_members WHERE project_id = ?1 AND user_id = ?2",
            params![project_id, target_user_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or_else(|| anyhow::anyhow!("target user is not a member of this project"))?;
    if target_role == ProjectRole::Owner.slug() {
        bail!("project owner cannot be removed");
    }

    conn.execute(
        "DELETE FROM project_members WHERE project_id = ?1 AND user_id = ?2",
        params![project_id, target_user_id],
    )?;
    Ok(())
}

/// Changes a member's role. Only the owner may change roles; the role may only
/// be set to a grantable role (`editor`/`viewer`) and the owner row is immutable.
pub fn set_member_role(
    project_id: &str,
    requester_user_id: &str,
    target_user_id: &str,
    role: &str,
) -> Result<ProjectMember> {
    let role = ProjectRole::from_grantable_slug(role)
        .ok_or_else(|| anyhow::anyhow!("role must be 'editor' or 'viewer'"))?;

    let pool = super::db::pool()?;
    let conn = pool.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    require_owner(&conn, project_id, requester_user_id)?;

    let target_role = conn
        .query_row(
            "SELECT role FROM project_members WHERE project_id = ?1 AND user_id = ?2",
            params![project_id, target_user_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or_else(|| anyhow::anyhow!("target user is not a member of this project"))?;
    if target_role == ProjectRole::Owner.slug() {
        bail!("project owner role cannot be changed");
    }

    conn.execute(
        "UPDATE project_members SET role = ?3 \
         WHERE project_id = ?1 AND user_id = ?2",
        params![project_id, target_user_id, role.slug()],
    )?;

    conn.query_row(
        "SELECT project_id, user_id, role, status, invited_by, created_at \
         FROM project_members WHERE project_id = ?1 AND user_id = ?2",
        params![project_id, target_user_id],
        read_member,
    )
    .map_err(Into::into)
}

/// Asserts that `user_id` is the owner of `project_id`, returning an error
/// otherwise. The single repository-side authorization gate for owner-only
/// actions (invite/remove/role change).
fn require_owner(
    conn: &rusqlite::Connection,
    project_id: &str,
    user_id: &str,
) -> Result<()> {
    let role: Option<String> = conn
        .query_row(
            "SELECT role FROM project_members WHERE project_id = ?1 AND user_id = ?2",
            params![project_id, user_id],
            |row| row.get(0),
        )
        .optional()?;
    match role.as_deref() {
        Some(r) if r == ProjectRole::Owner.slug() => Ok(()),
        _ => bail!("only the project owner may perform this action"),
    }
}

/// Returns the number of registered models for a project.
pub fn count_models_per_project(project_id: &str) -> Result<u32> {
    let pool = super::db::pool()?;
    let conn = pool.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM models WHERE project_id = ?1",
        params![project_id],
        |row| row.get(0),
    )?;
    Ok(count.max(0) as u32)
}

fn read_summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectSummary> {
    let project = Project {
        project_id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        project_type: row.get(3)?,
        status: row.get(4)?,
        owner_user_id: row.get(5)?,
        org_id: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    };
    let model_count = row.get::<_, i64>(9)?.max(0) as u32;
    let dataset_count = row.get::<_, i64>(10)?.max(0) as u32;
    let role: String = row.get(11)?;
    let is_owner = role == ProjectRole::Owner.slug();
    Ok(ProjectSummary {
        project,
        model_count,
        dataset_count,
        role,
        is_owner,
    })
}

fn read_member(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectMember> {
    Ok(ProjectMember {
        project_id: row.get(0)?,
        user_id: row.get(1)?,
        role: row.get(2)?,
        status: row.get(3)?,
        invited_by: row.get(4)?,
        created_at: row.get(5)?,
    })
}
