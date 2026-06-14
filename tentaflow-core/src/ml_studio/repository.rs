// ===== File: ml_studio/repository.rs — SQL access for ML Studio projects =====

use anyhow::{bail, Result};
use rusqlite::{params, OptionalExtension};

use super::models::{Project, ProjectSummary, ProjectType};

/// Lists projects shared across an organization, newest first, each with its
/// per-project KPIs (dataset count, model count). Scoped by `org_id`; ownership
/// is not filtered so every member of the org sees the shared project list.
pub fn list_projects(org_id: &str) -> Result<Vec<ProjectSummary>> {
    let pool = super::db::pool()?;
    let conn = pool.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT p.project_id, p.name, p.description, p.project_type, p.status, \
                p.owner_user_id, p.org_id, p.created_at, p.updated_at, \
                (SELECT COUNT(*) FROM models m WHERE m.project_id = p.project_id), \
                (SELECT COUNT(*) FROM datasets d WHERE d.project_id = p.project_id) \
         FROM projects p \
         WHERE p.org_id = ?1 \
         ORDER BY p.updated_at DESC, p.name",
    )?;
    let rows = stmt.query_map(params![org_id], read_summary)?;
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
    conn.execute(
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
    drop(conn);

    get_project(org_id, &project_id)?
        .ok_or_else(|| anyhow::anyhow!("project not found after create"))
}

/// Fetches a single project (with model count) scoped to its organization.
pub fn get_project(org_id: &str, project_id: &str) -> Result<Option<ProjectSummary>> {
    let pool = super::db::pool()?;
    let conn = pool.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    conn.query_row(
        "SELECT p.project_id, p.name, p.description, p.project_type, p.status, \
                p.owner_user_id, p.org_id, p.created_at, p.updated_at, \
                (SELECT COUNT(*) FROM models m WHERE m.project_id = p.project_id), \
                (SELECT COUNT(*) FROM datasets d WHERE d.project_id = p.project_id) \
         FROM projects p \
         WHERE p.org_id = ?1 AND p.project_id = ?2",
        params![org_id, project_id],
        read_summary,
    )
    .optional()
    .map_err(Into::into)
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
    Ok(ProjectSummary {
        project,
        model_count,
        dataset_count,
    })
}
