// ===== File: project_studio/ml_link.rs — links between a project and ML Studio (F4, X02) =====
//
// A link binds one Project Studio project to one ML Studio project so the
// project screen can show training state and (optionally) mirror its member
// list into ML Studio. The sync is ONE-WAY (project → ML) and idempotent: the
// project's member list is the source of truth, ML Studio only receives it.
//
// Two access boundaries meet here and neither is bypassed:
//   * ML Studio writes go through `ml_studio::repository`, whose membership
//     calls re-check `require_owner`. The link therefore acts strictly as the
//     ML project's OWNER — an owner that is gone stops the sync loudly
//     ('owner_unavailable') instead of drifting silently.
//   * The Power-User gate on the ML wire handlers is NOT waived here. Calling
//     the repository directly is what makes the difference: the identity of a
//     path is the module that walks it, not a flag on the wire.

use std::collections::{HashMap, HashSet};
use tentaflow_protocol::project_studio::access::{
    ProjectAccessWire, ProjectArea, ProjectPermissionLevel,
};

use anyhow::{anyhow, bail, Result};
use rusqlite::{params, OptionalExtension};

use super::models::MlLinkRecord;
use crate::db::DbPool;

/// Upper bound of links per project. The project screen renders one card per
/// link and each card costs an ML Studio query.
pub const MAX_LINKS_PER_PROJECT: u32 = 10;
/// Model names shown as chips on the card.
const MAX_SUMMARY_MODELS: usize = 6;
/// ML Studio knows exactly these two grantable roles.
pub const ML_ROLES: &[&str] = &["editor", "viewer"];

fn read_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("project_studio ml_link read: {e}")
}

fn write_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("project_studio ml_link write: {e}")
}

const LINK_COLS: &str = "link_id, ml_project_id, label, origin, sync_permissions, \
     role_map_json, last_sync_at, last_sync_result, created_by, created_at, updated_at";

fn read_link(row: &rusqlite::Row<'_>) -> rusqlite::Result<MlLinkRecord> {
    Ok(MlLinkRecord {
        link_id: row.get(0)?,
        ml_project_id: row.get(1)?,
        label: row.get(2)?,
        origin: row.get(3)?,
        sync_permissions: row.get::<_, i64>(4)? != 0,
        role_map_json: row.get(5)?,
        last_sync_at: row.get(6)?,
        last_sync_result: row.get(7)?,
        created_by: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

// =============================================================================
// SQL
// =============================================================================

pub fn list(pool: &DbPool) -> Result<Vec<MlLinkRecord>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {LINK_COLS} FROM ml_links ORDER BY created_at, link_id"
    ))?;
    let rows = stmt.query_map([], read_link)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn get(pool: &DbPool, link_id: &str) -> Result<Option<MlLinkRecord>> {
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        &format!("SELECT {LINK_COLS} FROM ml_links WHERE link_id = ?1"),
        params![link_id],
        read_link,
    )
    .optional()
    .map_err(Into::into)
}

pub fn count(pool: &DbPool) -> Result<u32> {
    let conn = pool.read().map_err(read_err)?;
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM ml_links", [], |row| row.get(0))?;
    Ok(n as u32)
}

/// Inserts a link. A `UNIQUE` clash means the ML project is already linked
/// (from this project or, in a shared registry, another one).
#[allow(clippy::too_many_arguments)]
pub fn insert(
    pool: &DbPool,
    link_id: &str,
    ml_project_id: &str,
    label: &str,
    origin: &str,
    sync_permissions: bool,
    role_map_json: &str,
    created_by: &str,
) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "INSERT INTO ml_links (link_id, ml_project_id, label, origin, sync_permissions, \
            role_map_json, created_by) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            link_id,
            ml_project_id,
            label,
            origin,
            i64::from(sync_permissions),
            role_map_json,
            created_by
        ],
    )?;
    drop(conn);
    super::repository::set_setting(pool, CAPABILITY_MAP_MIGRATED, "1")?;
    Ok(())
}

pub fn update(
    pool: &DbPool,
    link_id: &str,
    label: &str,
    sync_permissions: bool,
    role_map_json: &str,
) -> Result<bool> {
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute(
        "UPDATE ml_links SET label = ?1, sync_permissions = ?2, role_map_json = ?3, \
            updated_at = datetime('now') WHERE link_id = ?4",
        params![label, i64::from(sync_permissions), role_map_json, link_id],
    )?;
    Ok(n > 0)
}

pub fn delete(pool: &DbPool, link_id: &str) -> Result<bool> {
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute("DELETE FROM ml_links WHERE link_id = ?1", params![link_id])?;
    Ok(n > 0)
}

fn record_sync_result(pool: &DbPool, link_id: &str, result: &str) {
    let Ok(conn) = pool.write() else {
        return;
    };
    let _ = conn.execute(
        "UPDATE ml_links SET last_sync_at = datetime('now'), last_sync_result = ?1, \
            updated_at = datetime('now') WHERE link_id = ?2",
        params![result, link_id],
    );
}

/// Ledger of the ML memberships THIS link granted, kept in the project's own
/// key/value settings. Without it the sync cannot tell a membership it created
/// from one an ML Studio owner made directly: mirroring "everyone who is not the
/// ML owner" would evict the ML project's own team on the first pass of a
/// `linked_existing` link, and dropping the removal branch instead would mean a
/// user who LOSES project membership keeps their ML access forever.
fn granted_key(link_id: &str) -> String {
    format!("ml_link_granted:{link_id}")
}

fn granted_users(pool: &DbPool, link_id: &str) -> Result<HashSet<String>> {
    let Some(raw) = super::repository::get_setting(pool, &granted_key(link_id))? else {
        return Ok(HashSet::new());
    };
    Ok(serde_json::from_str::<Vec<String>>(&raw)?
        .into_iter()
        .collect())
}

fn set_granted_users(pool: &DbPool, link_id: &str, users: &HashSet<String>) -> Result<()> {
    let mut list: Vec<&String> = users.iter().collect();
    list.sort();
    super::repository::set_setting(pool, &granted_key(link_id), &serde_json::to_string(&list)?)
}

fn clear_granted_users(pool: &DbPool, link_id: &str) -> Result<()> {
    super::repository::set_setting(pool, &granted_key(link_id), "[]")
}

/// Switches the sync off after an unrecoverable authorization failure. An
/// explicit stop beats a loop that quietly writes nothing every time.
fn disable_sync(pool: &DbPool, link_id: &str) {
    let Ok(conn) = pool.write() else {
        return;
    };
    let _ = conn.execute(
        "UPDATE ml_links SET sync_permissions = 0, last_sync_at = datetime('now'), \
            last_sync_result = 'owner_unavailable', updated_at = datetime('now') \
         WHERE link_id = ?1",
        params![link_id],
    );
}

// =============================================================================
// Role mapping
// =============================================================================

/// Default project-role → ML-role mapping: everyone who may change project
/// content becomes an ML editor, everyone else a viewer.
pub fn default_role_map() -> Vec<(String, String)> {
    [("read", "viewer"), ("write", "editor"), ("admin", "editor")]
        .into_iter()
        .map(|(level, role)| (level.to_string(), role.to_string()))
        .collect()
}

pub fn role_map_from_json(json: &str) -> Vec<(String, String)> {
    serde_json::from_str::<Vec<(String, String)>>(json)
        .ok()
        .filter(|map| validate_role_map(map).is_ok())
        .unwrap_or_default()
}

pub fn role_map_to_json(map: &[(String, String)]) -> String {
    serde_json::to_string(map).unwrap_or_else(|_| "[]".to_string())
}

pub fn validate_role_map(map: &[(String, String)]) -> Result<()> {
    let mut seen = HashSet::new();
    for (level, role) in map {
        if !["read", "write", "admin"].contains(&level.as_str()) {
            bail!("unknown project permission level '{level}'");
        }
        if !ML_ROLES.contains(&role.as_str()) || (level == "read" && role != "viewer") {
            bail!("ML role exceeds project capability");
        }
        if !seen.insert(level) {
            bail!("duplicate mapping for permission level '{level}'");
        }
    }
    Ok(())
}

fn ml_role_for(map: &[(String, String)], access: &ProjectAccessWire) -> Option<String> {
    let level = if access.allows(ProjectArea::Knowledge, ProjectPermissionLevel::Write) {
        if access.level(ProjectArea::Knowledge) == ProjectPermissionLevel::Admin {
            "admin"
        } else {
            "write"
        }
    } else if access.allows(ProjectArea::Knowledge, ProjectPermissionLevel::Read) {
        "read"
    } else {
        return None;
    };
    map.iter()
        .find(|(key, _)| key == level)
        .map(|(_, role)| role.clone())
}

const CAPABILITY_MAP_MIGRATED: &str = "ml_capability_map_migrated";

pub fn migrate_capability_maps(pool: &DbPool) -> Result<()> {
    if super::repository::get_setting(pool, CAPABILITY_MAP_MIGRATED)?.is_some() {
        return Ok(());
    }
    for link in list(pool)? {
        let parsed = serde_json::from_str::<Vec<(String, String)>>(&link.role_map_json).ok();
        let map = match parsed {
            Some(old) if old.is_empty() => default_role_map(),
            Some(old)
                if old.iter().any(|(key, _)| {
                    ["owner", "manager", "editor", "tester", "viewer"].contains(&key.as_str())
                }) =>
            {
                let mut migrated = Vec::new();
                if old
                    .iter()
                    .any(|(key, _)| key == "tester" || key == "viewer")
                {
                    migrated.push(("read".to_string(), "viewer".to_string()));
                }
                if let Some((_, role)) = old.iter().find(|(key, _)| key == "editor") {
                    if ML_ROLES.contains(&role.as_str()) {
                        migrated.push(("write".to_string(), role.clone()));
                    }
                }
                let admin_roles = old.iter().filter(|(key, role)| {
                    (key == "owner" || key == "manager") && ML_ROLES.contains(&role.as_str())
                });
                if let Some(role) = admin_roles
                    .map(|(_, role)| role)
                    .max_by_key(|role| role.as_str() == "editor")
                {
                    migrated.push(("admin".to_string(), role.clone()));
                }
                migrated
            }
            Some(map) if validate_role_map(&map).is_ok() => map,
            _ => Vec::new(),
        };
        let conn = pool.write().map_err(write_err)?;
        conn.execute(
            "UPDATE ml_links SET role_map_json = ?2 WHERE link_id = ?1",
            params![link.link_id, role_map_to_json(&map)],
        )?;
    }
    super::repository::set_setting(pool, CAPABILITY_MAP_MIGRATED, "1")?;
    Ok(())
}

pub fn restore_grant_index() -> Result<()> {
    let registry = super::db::pool()?;
    let projects = {
        let conn = registry.read().map_err(read_err)?;
        let mut stmt = conn.prepare(
            "SELECT p.project_id,p.dir_path FROM projects p WHERE NOT EXISTS (\
             SELECT 1 FROM project_admissions a WHERE a.project_id=p.project_id AND a.kind='delete') \
             ORDER BY p.project_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (project_id, dir_path) in projects {
        if !std::path::Path::new(&dir_path).join("project.db").is_file() {
            bail!("project '{project_id}' database missing during ML grant index restore");
        }
        let (pool, _) = super::project_db::open_pool_at(std::path::Path::new(&dir_path))?;
        migrate_capability_maps(&pool)?;
        for link in list(&pool)? {
            let users = granted_users(&pool, &link.link_id)?
                .into_iter()
                .collect::<Vec<_>>();
            super::repository::replace_ml_grants(
                &project_id,
                &link.link_id,
                &link.ml_project_id,
                &users,
            )?;
        }
    }
    Ok(())
}

pub fn relinquish_membership(ml_project_id: &str, user_id: &str) -> Result<()> {
    let origins = super::repository::ml_grant_origins(ml_project_id, user_id)?;
    for (project_id, link_id) in origins {
        let pool = super::project_db::open(&project_id)?;
        let mut conn = pool.write().map_err(write_err)?;
        let tx = conn.transaction()?;
        let raw: Option<String> = tx
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                [granted_key(&link_id)],
                |row| row.get(0),
            )
            .optional()?;
        let mut users = match raw {
            Some(raw) => serde_json::from_str::<Vec<String>>(&raw)?,
            None => Vec::new(),
        };
        users.retain(|user| user != user_id);
        tx.execute("INSERT INTO settings(key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![granted_key(&link_id), serde_json::to_string(&users)?])?;
        tx.execute("INSERT INTO settings(key,value) VALUES (?1,'1') ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [format!("ml_link_manual:{link_id}:{user_id}")])?;
        tx.commit()?;
    }
    super::repository::remove_ml_grant(ml_project_id, user_id)
}

pub fn current_member_role(
    ml_project_id: &str,
    user_id: &str,
    stored: Option<String>,
) -> Result<Option<String>> {
    let Some(stored) = stored else {
        return Ok(None);
    };
    if stored == "owner" {
        return Ok(Some(stored));
    }
    let origins = super::repository::ml_grant_origins(ml_project_id, user_id)?;
    if origins.is_empty() {
        return Ok(Some(stored));
    }
    let org_id: Option<String> = {
        let ml = crate::ml_studio::db::pool()?;
        let conn = ml.read().map_err(read_err)?;
        conn.query_row(
            "SELECT org_id FROM projects WHERE project_id = ?1",
            [ml_project_id],
            |row| row.get(0),
        )
        .optional()?
    };
    let Some(org_id) = org_id else {
        return Ok(None);
    };
    let mut desired = None;
    for (project_id, link_id) in origins {
        let Some(project) = super::repository::get_project(&org_id, &project_id)? else {
            continue;
        };
        let access = super::repository::project_access(&project, user_id, false)?;
        let pool = super::project_db::open(&project_id)?;
        let Some(link) = get(&pool, &link_id)? else {
            continue;
        };
        if link.ml_project_id != ml_project_id {
            continue;
        }
        if let Some(role) = ml_role_for(&role_map_from_json(&link.role_map_json), &access) {
            if role == "editor" || desired.is_none() {
                desired = Some(role);
            }
        }
    }
    Ok(desired.map(|role| {
        if stored == "viewer" {
            stored.clone()
        } else {
            role
        }
    }))
}

// =============================================================================
// ML Studio snapshot
// =============================================================================

/// Read-only ML project snapshot for the project card. Read straight from the
/// ML Studio database: the caller's authorization is membership in the PROJECT,
/// so an ML membership check here would hide the card from exactly the people
/// the link exists for.
#[derive(Debug, Clone)]
pub struct MlProjectSummary {
    pub ml_project_id: String,
    pub name: String,
    pub project_type: String,
    pub project_type_label: String,
    pub status: String,
    pub dataset_count: u32,
    pub model_count: u32,
    pub models: Vec<String>,
    pub last_training_run_id: String,
    pub last_training_status: String,
    pub last_training_started_at: String,
    pub last_training_finished_at: String,
    pub last_training_metrics_json: String,
    pub training_in_progress: bool,
}

/// Newest training run of an ML project; `model_id` is NULL until the run
/// produces a model, which is also where the metrics live.
struct LastTraining {
    run_id: String,
    status: String,
    started_at: Option<String>,
    finished_at: Option<String>,
    model_id: Option<String>,
}

pub fn summary(ml_project_id: &str) -> Result<Option<MlProjectSummary>> {
    let pool = crate::ml_studio::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let row: Option<(String, String, String)> = conn
        .query_row(
            "SELECT name, project_type, status FROM projects WHERE project_id = ?1",
            params![ml_project_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((name, project_type, status)) = row else {
        return Ok(None);
    };
    let dataset_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM datasets WHERE project_id = ?1",
        params![ml_project_id],
        |row| row.get(0),
    )?;
    let model_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM models WHERE project_id = ?1",
        params![ml_project_id],
        |row| row.get(0),
    )?;
    let models: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT name FROM models WHERE project_id = ?1 ORDER BY created_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![ml_project_id, MAX_SUMMARY_MODELS as i64], |row| {
            row.get::<_, String>(0)
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let last: Option<LastTraining> = conn
        .query_row(
            "SELECT run_id, status, started_at, finished_at, model_id FROM training_runs \
             WHERE project_id = ?1 ORDER BY COALESCE(finished_at, started_at) DESC, run_id \
             LIMIT 1",
            params![ml_project_id],
            |row| {
                Ok(LastTraining {
                    run_id: row.get(0)?,
                    status: row.get(1)?,
                    started_at: row.get(2)?,
                    finished_at: row.get(3)?,
                    model_id: row.get(4)?,
                })
            },
        )
        .optional()?;
    let running: i64 = conn.query_row(
        "SELECT COUNT(*) FROM training_runs WHERE project_id = ?1 \
         AND status IN ('running', 'pending', 'queued')",
        params![ml_project_id],
        |row| row.get(0),
    )?;
    // Metrics belong to the model the run produced; a run without a model has
    // none yet.
    let metrics_json = match last.as_ref().and_then(|l| l.model_id.clone()) {
        Some(model_id) => conn
            .query_row(
                "SELECT metrics_json FROM models WHERE model_id = ?1",
                params![model_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .unwrap_or_default(),
        None => String::new(),
    };
    let type_label = crate::ml_studio::models::ProjectType::from_slug(&project_type)
        .map(|t| t.label_pl().to_string())
        .unwrap_or_else(|| project_type.clone());

    Ok(Some(MlProjectSummary {
        ml_project_id: ml_project_id.to_string(),
        name,
        project_type,
        project_type_label: type_label,
        status,
        dataset_count: dataset_count as u32,
        model_count: model_count as u32,
        models,
        last_training_run_id: last.as_ref().map(|l| l.run_id.clone()).unwrap_or_default(),
        last_training_status: last.as_ref().map(|l| l.status.clone()).unwrap_or_default(),
        last_training_started_at: last
            .as_ref()
            .and_then(|l| l.started_at.clone())
            .unwrap_or_default(),
        last_training_finished_at: last
            .as_ref()
            .and_then(|l| l.finished_at.clone())
            .unwrap_or_default(),
        last_training_metrics_json: metrics_json,
        training_in_progress: running > 0,
    }))
}

/// ML projects the caller OWNS and that no link points at yet. Attaching
/// requires ownership: the sync writes through owner-only repository calls, so
/// a link created by a non-owner could never apply anything.
pub fn owned_candidates(pool: &DbPool, user_id: &str) -> Result<Vec<MlProjectSummary>> {
    let linked: Vec<String> = list(pool)?.into_iter().map(|l| l.ml_project_id).collect();
    let mut out = Vec::new();
    for project in crate::ml_studio::repository::list_projects(user_id)? {
        if !project.is_owner || linked.contains(&project.project.project_id) {
            continue;
        }
        if let Some(summary) = summary(&project.project.project_id)? {
            out.push(summary);
        }
    }
    Ok(out)
}

/// ML membership role of one user, used for the "open in ML Studio" gate.
pub fn ml_member_role(ml_project_id: &str, user_id: &str) -> Option<String> {
    crate::ml_studio::repository::member_role(ml_project_id, user_id)
        .ok()
        .flatten()
}

// =============================================================================
// Permission sync (project → ML Studio)
// =============================================================================

/// Result of one sync pass.
#[derive(Debug, Clone, Default)]
pub struct SyncOutcome {
    pub applied_add: u32,
    pub applied_update: u32,
    pub applied_remove: u32,
    pub skipped: u32,
    pub errors: Vec<String>,
    /// 'ok' | 'partial' | 'owner_unavailable' | 'ml_project_missing'.
    pub result: String,
}

/// Owner of the ML project, verified to still exist in the core user directory.
/// `None` means the link can no longer write anything.
fn available_owner(ml_project_id: &str) -> Result<Option<String>> {
    let Some(owner) = crate::ml_studio::repository::list_members(ml_project_id)?
        .into_iter()
        .find(|member| member.role == "owner" && member.status == "active")
        .map(|member| member.user_id)
    else {
        return Ok(None);
    };
    let core = crate::db::global_pool().ok_or_else(|| anyhow!("core database unavailable"))?;
    Ok(crate::db::repository::get_user_role(&core, &owner)?.map(|_| owner))
}

/// Applies the project's member list to ONE link. Idempotent: it computes the
/// desired ML membership from the project roles and issues only the deltas.
pub fn sync_link(project_id: &str, pool: &DbPool, link: &MlLinkRecord) -> SyncOutcome {
    let mut outcome = SyncOutcome::default();
    if summary(&link.ml_project_id).ok().flatten().is_none() {
        outcome.result = "ml_project_missing".to_string();
        outcome.errors.push("projekt ML nie istnieje".to_string());
        record_sync_result(pool, &link.link_id, &outcome.result);
        return outcome;
    }
    let owner = match available_owner(&link.ml_project_id) {
        Ok(owner) => owner,
        Err(error) => {
            outcome.result = "partial".into();
            outcome.errors.push(error.to_string());
            record_sync_result(pool, &link.link_id, &outcome.result);
            return outcome;
        }
    };
    let Some(owner) = owner else {
        // A link that cannot write must say so: silent drift is worse than a
        // stopped sync, because the project list would keep implying access
        // that ML Studio never granted.
        outcome.result = "owner_unavailable".to_string();
        outcome
            .errors
            .push("wlasciciel projektu ML jest niedostepny".to_string());
        disable_sync(pool, &link.link_id);
        return outcome;
    };

    let role_map = role_map_from_json(&link.role_map_json);
    let members = match super::repository::effective_principals(
        project_id,
        ProjectArea::Knowledge,
        ProjectPermissionLevel::Read,
    ) {
        Ok(members) => members,
        Err(e) => {
            outcome.result = "partial".to_string();
            outcome.errors.push(e.to_string());
            record_sync_result(pool, &link.link_id, &outcome.result);
            return outcome;
        }
    };
    let _project = match crate::ml_studio::repository::get_project(&owner, &link.ml_project_id)
        .and_then(|ml| ml.ok_or_else(|| anyhow!("ML project missing")))
        .and_then(|ml| super::repository::get_project(&ml.project.org_id, project_id))
    {
        Ok(Some(project)) => project,
        _ => {
            outcome.result = "partial".into();
            outcome.errors.push("project unavailable".into());
            return outcome;
        }
    };
    let mut desired: HashMap<String, String> = HashMap::new();
    for principal in members {
        if principal.user_id == owner {
            continue;
        }
        let manual = super::repository::get_setting(
            pool,
            &format!("ml_link_manual:{}:{}", link.link_id, principal.user_id),
        )
        .ok()
        .flatten()
        .is_some();
        if manual {
            continue;
        }
        match ml_role_for(&role_map, &principal.access) {
            Some(role) => {
                desired.insert(principal.user_id, role);
            }
            None => outcome.skipped += 1,
        }
    }

    let current: Vec<crate::ml_studio::models::ProjectMember> =
        match crate::ml_studio::repository::list_members(&link.ml_project_id) {
            Ok(members) => members,
            Err(e) => {
                outcome.result = "partial".to_string();
                outcome.errors.push(e.to_string());
                record_sync_result(pool, &link.link_id, &outcome.result);
                return outcome;
            }
        };
    let current_map: HashMap<String, String> = current
        .iter()
        .filter(|m| m.role != "owner")
        .map(|m| (m.user_id.clone(), m.role.clone()))
        .collect();

    let mut granted = match granted_users(pool, &link.link_id) {
        Ok(users) => users,
        Err(error) => {
            outcome.result = "partial".into();
            outcome.errors.push(error.to_string());
            return outcome;
        }
    };
    // Persist provenance before an external membership can become observable.
    // A failed grant is harmlessly reserved; a successful untracked grant is not.
    granted.extend(
        desired
            .keys()
            .filter(|user| !current_map.contains_key(*user))
            .cloned(),
    );
    if let Err(error) = set_granted_users(pool, &link.link_id, &granted).and_then(|_| {
        super::repository::replace_ml_grants(
            project_id,
            &link.link_id,
            &link.ml_project_id,
            &granted.iter().cloned().collect::<Vec<_>>(),
        )
    }) {
        outcome.result = "partial".into();
        outcome.errors.push(error.to_string());
        record_sync_result(pool, &link.link_id, &outcome.result);
        return outcome;
    }
    for (user_id, role) in &desired {
        match current_map.get(user_id) {
            Some(_) if !granted.contains(user_id) => {
                outcome.skipped += 1;
            }
            Some(existing) if existing == role => {
                granted.insert(user_id.clone());
            }
            Some(_) => match crate::ml_studio::repository::set_member_role(
                &link.ml_project_id,
                &owner,
                user_id,
                role,
            ) {
                Ok(_) => {
                    outcome.applied_update += 1;
                    granted.insert(user_id.clone());
                }
                Err(e) => outcome.errors.push(format!("{user_id}: {e}")),
            },
            None => match crate::ml_studio::repository::invite_member(
                &link.ml_project_id,
                &owner,
                user_id,
                role,
            ) {
                Ok(_) => {
                    outcome.applied_add += 1;
                    granted.insert(user_id.clone());
                }
                Err(e) => outcome.errors.push(format!("{user_id}: {e}")),
            },
        }
    }
    // Losing project membership (or a role the map does not cover) revokes ML
    // access immediately — but only for memberships this link granted. An ML
    // member the ML owner invited directly is none of the link's business.
    for user_id in current_map.keys() {
        if desired.contains_key(user_id) || !granted.contains(user_id) {
            continue;
        }
        match crate::ml_studio::repository::remove_member(&link.ml_project_id, &owner, user_id) {
            Ok(()) => {
                outcome.applied_remove += 1;
                granted.remove(user_id);
            }
            Err(e) => outcome.errors.push(format!("{user_id}: {e}")),
        }
    }
    // A user who is no longer an ML member at all (removed in ML Studio) leaves
    // the ledger too, so a later re-invite there is not treated as ours.
    granted.retain(|user_id| desired.contains_key(user_id) || current_map.contains_key(user_id));
    if let Err(error) = set_granted_users(pool, &link.link_id, &granted) {
        outcome.result = "partial".into();
        outcome.errors.push(error.to_string());
        record_sync_result(pool, &link.link_id, &outcome.result);
        return outcome;
    }
    if let Err(error) = super::repository::replace_ml_grants(
        project_id,
        &link.link_id,
        &link.ml_project_id,
        &granted.iter().cloned().collect::<Vec<_>>(),
    ) {
        outcome.errors.push(error.to_string());
    }

    outcome.result = if outcome.errors.is_empty() {
        "ok".to_string()
    } else {
        "partial".to_string()
    };
    record_sync_result(pool, &link.link_id, &outcome.result);
    outcome
}

/// Applies the project's member list to EVERY link that asked for it. Spawned
/// after a membership mutation, so it must never propagate a failure: the
/// member change already succeeded and must not be rolled back by an ML Studio
/// problem.
pub fn sync_project_memberships(project_id: String) {
    let pool = match super::project_db::open(&project_id) {
        Ok(pool) => pool,
        Err(e) => {
            tracing::warn!(project_id = %project_id, "ml link sync skipped, project db unavailable: {e}");
            return;
        }
    };
    let links = match list(&pool) {
        Ok(links) => links,
        Err(e) => {
            tracing::warn!(project_id = %project_id, "ml link sync skipped, link list failed: {e}");
            return;
        }
    };
    for link in links.iter().filter(|l| l.sync_permissions) {
        let outcome = sync_link(&project_id, &pool, link);
        if !outcome.errors.is_empty() {
            tracing::warn!(
                project_id = %project_id,
                ml_project_id = %link.ml_project_id,
                result = %outcome.result,
                "ml link permission sync reported errors: {:?}",
                outcome.errors
            );
        }
    }
}

/// Creates an ML project owned by the caller and links it. The caller becomes
/// the ML owner, so every later sync writes as an owner.
#[allow(clippy::too_many_arguments)]
pub fn create_from_project(
    pool: &DbPool,
    project_id: &str,
    org_id: &str,
    creator_user_id: &str,
    ml_name: &str,
    project_type: &str,
    label: &str,
    sync_permissions: bool,
    role_map: &[(String, String)],
) -> Result<(String, String, u32, u32)> {
    if count(pool)? >= MAX_LINKS_PER_PROJECT {
        bail!("a project holds at most {MAX_LINKS_PER_PROJECT} ML Studio links");
    }
    validate_role_map(role_map)?;
    if crate::ml_studio::models::ProjectType::from_slug(project_type).is_none() {
        bail!("unknown ML project type '{project_type}'");
    }
    let created = crate::ml_studio::repository::create_project(
        creator_user_id,
        org_id,
        ml_name,
        "",
        project_type,
    )?;
    let ml_project_id = created.project.project_id.clone();
    let link_id = uuid::Uuid::new_v4().to_string();
    if let Err(e) = insert(
        pool,
        &link_id,
        &ml_project_id,
        label,
        "created_from_project",
        sync_permissions,
        &role_map_to_json(role_map),
        creator_user_id,
    ) {
        // The ML project exists but is unreachable from here — say so instead of
        // leaving a link row that points nowhere.
        return Err(anyhow!("ML project created but linking failed: {e}"));
    }

    let (mapped, skipped, granted) = apply_role_map(
        pool,
        &link_id,
        project_id,
        &ml_project_id,
        creator_user_id,
        role_map,
    )?;
    set_granted_users(pool, &link_id, &granted)?;
    super::repository::replace_ml_grants(
        project_id,
        &link_id,
        &ml_project_id,
        &granted.iter().cloned().collect::<Vec<_>>(),
    )?;
    record_sync_result(pool, &link_id, "ok");
    Ok((link_id, ml_project_id, mapped, skipped))
}

/// Grants every project member their mapped ML role. The creator is skipped —
/// they are already the ML owner, and ML Studio refuses a self-invite. Returns
/// the users actually granted, which seeds the link's grant ledger.
fn apply_role_map(
    pool: &DbPool,
    link_id: &str,
    project_id: &str,
    ml_project_id: &str,
    creator_user_id: &str,
    role_map: &[(String, String)],
) -> Result<(u32, u32, HashSet<String>)> {
    let mut granted = HashSet::new();
    let members = super::repository::effective_principals(
        project_id,
        ProjectArea::Knowledge,
        ProjectPermissionLevel::Read,
    )?;
    let ml = crate::ml_studio::repository::get_project(creator_user_id, ml_project_id)?
        .ok_or_else(|| anyhow!("ML project missing"))?;
    let _project = super::repository::get_project(&ml.project.org_id, project_id)?
        .ok_or_else(|| anyhow!("project unavailable"))?;
    let mut mapped = 0u32;
    let mut skipped = 0u32;
    for principal in members {
        if principal.user_id == creator_user_id {
            skipped += 1;
            continue;
        }
        let Some(role) = ml_role_for(role_map, &principal.access) else {
            skipped += 1;
            continue;
        };
        if crate::ml_studio::repository::member_role(ml_project_id, &principal.user_id)?.is_some() {
            skipped += 1;
            continue;
        }
        granted.insert(principal.user_id.clone());
        set_granted_users(pool, link_id, &granted)?;
        super::repository::replace_ml_grants(
            project_id,
            link_id,
            ml_project_id,
            &granted.iter().cloned().collect::<Vec<_>>(),
        )?;
        match crate::ml_studio::repository::invite_member(
            ml_project_id,
            creator_user_id,
            &principal.user_id,
            &role,
        ) {
            Ok(_) => mapped += 1,
            Err(error) => {
                granted.remove(&principal.user_id);
                set_granted_users(pool, link_id, &granted)?;
                super::repository::replace_ml_grants(
                    project_id,
                    link_id,
                    ml_project_id,
                    &granted.iter().cloned().collect::<Vec<_>>(),
                )?;
                return Err(error);
            }
        }
    }
    Ok((mapped, skipped, granted))
}

/// Removes the ML memberships this link granted (never the ML owner) and drops
/// the link. The ML project itself is never deleted — it may hold datasets and
/// trained models that outlive the link.
pub fn detach(
    pool: &DbPool,
    project_id: &str,
    link: &MlLinkRecord,
    revoke_members: bool,
) -> Result<u32> {
    let mut removed = 0u32;
    if revoke_members && summary(&link.ml_project_id)?.is_some() {
        let granted = granted_users(pool, &link.link_id)?;
        let current = crate::ml_studio::repository::list_members(&link.ml_project_id)?;
        let owned = current
            .into_iter()
            .filter(|member| member.role != "owner" && granted.contains(&member.user_id))
            .collect::<Vec<_>>();
        if !owned.is_empty() {
            let owner = available_owner(&link.ml_project_id)?.ok_or_else(|| {
                anyhow!("ML project owner unavailable; mirrored grants cannot be revoked")
            })?;
            for member in owned {
                crate::ml_studio::repository::remove_member(
                    &link.ml_project_id,
                    &owner,
                    &member.user_id,
                )?;
                removed += 1;
            }
        }
    }
    delete(pool, &link.link_id)?;
    clear_granted_users(pool, &link.link_id)?;
    super::repository::clear_ml_grants(project_id, &link.link_id)?;
    Ok(removed)
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

    fn knowledge_access(level: ProjectPermissionLevel) -> ProjectAccessWire {
        ProjectAccessWire {
            has_access: true,
            areas: vec![
                tentaflow_protocol::project_studio::access::ProjectAreaAccessWire {
                    area: ProjectArea::Knowledge,
                    level,
                    enabled: true,
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn capability_mapping_is_bounded_and_empty_or_invalid_maps_deny() {
        let map = default_role_map();
        assert_eq!(map.len(), 3);
        for (level, expected) in [
            (ProjectPermissionLevel::Read, "viewer"),
            (ProjectPermissionLevel::Write, "editor"),
            (ProjectPermissionLevel::Admin, "editor"),
        ] {
            assert_eq!(
                ml_role_for(&map, &knowledge_access(level)).as_deref(),
                Some(expected)
            );
        }
        let partial = vec![("admin".to_string(), "editor".to_string())];
        assert_eq!(
            ml_role_for(&partial, &knowledge_access(ProjectPermissionLevel::Admin)).as_deref(),
            Some("editor")
        );
        assert!(ml_role_for(&partial, &knowledge_access(ProjectPermissionLevel::Write)).is_none());
        for bad in [
            vec![("owner".into(), "editor".into())],
            vec![("read".into(), "editor".into())],
            vec![("write".into(), "owner".into())],
            vec![
                ("write".into(), "editor".into()),
                ("write".into(), "viewer".into()),
            ],
        ] {
            assert!(validate_role_map(&bad).is_err());
        }
        assert_eq!(role_map_from_json(&role_map_to_json(&map)), map);
        assert!(role_map_from_json("nonsense").is_empty());
        assert!(role_map_from_json("[]").is_empty());
        assert!(ml_role_for(&[], &knowledge_access(ProjectPermissionLevel::Admin)).is_none());
    }

    #[test]
    fn legacy_mapping_upgrade_runs_once_and_preserves_new_explicit_deny() {
        let pool = pool();
        insert(
            &pool,
            "legacy",
            "ml-legacy",
            "",
            "linked_existing",
            true,
            r#"[["owner","viewer"],["manager","editor"],["editor","viewer"],["tester","viewer"]]"#,
            "u",
        )
        .expect("link");
        pool.write()
            .expect("write")
            .execute(
                "DELETE FROM settings WHERE key = ?1",
                [CAPABILITY_MAP_MIGRATED],
            )
            .expect("old marker");
        migrate_capability_maps(&pool).expect("upgrade");
        let map = role_map_from_json(
            &get(&pool, "legacy")
                .expect("get")
                .expect("link")
                .role_map_json,
        );
        assert!(map.contains(&("read".into(), "viewer".into())));
        assert!(map.contains(&("write".into(), "viewer".into())));
        assert!(map.contains(&("admin".into(), "editor".into())));
        update(&pool, "legacy", "", true, "[]").expect("explicit deny");
        migrate_capability_maps(&pool).expect("already upgraded");
        assert_eq!(
            get(&pool, "legacy")
                .expect("get")
                .expect("link")
                .role_map_json,
            "[]"
        );
    }

    /// Link rows: the per-project cap is countable, `ml_project_id` is unique
    /// and an update rewrites exactly the three mutable fields.
    #[test]
    fn link_rows_are_unique_per_ml_project() {
        let pool = pool();
        insert(
            &pool,
            "l1",
            "ml1",
            "wizja",
            "linked_existing",
            true,
            &role_map_to_json(&default_role_map()),
            "u1",
        )
        .expect("insert");
        assert_eq!(count(&pool).expect("count"), 1);
        assert!(
            insert(&pool, "l2", "ml1", "", "linked_existing", false, "[]", "u1").is_err(),
            "the same ML project cannot be linked twice"
        );

        let link = get(&pool, "l1").expect("get").expect("row");
        assert!(link.sync_permissions);
        assert_eq!(link.origin, "linked_existing");

        update(&pool, "l1", "nowa", false, "[]").expect("update");
        let link = get(&pool, "l1").expect("get").expect("row");
        assert_eq!(link.label, "nowa");
        assert!(!link.sync_permissions);
        assert_eq!(link.created_by, "u1", "identity columns stay untouched");

        // An unreachable ML owner switches the sync off explicitly.
        update(&pool, "l1", "nowa", true, "[]").expect("re-enable");
        disable_sync(&pool, "l1");
        let link = get(&pool, "l1").expect("get").expect("row");
        assert!(!link.sync_permissions);
        assert_eq!(link.last_sync_result, "owner_unavailable");
        assert!(!link.last_sync_at.is_empty());

        assert!(delete(&pool, "l1").expect("delete"));
        assert_eq!(count(&pool).expect("count"), 0);
    }

    /// Registers a core identity so `available_owner` can confirm the ML owner
    /// still exists — the sync writes as that owner and must refuse to run when
    /// it is gone.
    fn seed_core_user(user_id: &str) {
        let pool = crate::db::global_pool().expect("core pool initialised by AppState::for_test");
        let conn = pool.write().expect("core write");
        let _ = conn.execute(
            "INSERT OR IGNORE INTO user_accounts (id, username, password_hash, role) \
             VALUES (?1, ?1, 'x', 'power_user')",
            params![user_id],
        );
    }

    /// End-to-end permission mirror against a REAL ML Studio database: the five
    /// project roles collapse onto two, the creator is skipped (they are already
    /// the ML owner), a role change propagates as an update, and losing project
    /// membership revokes the ML membership immediately.
    #[test]
    fn sync_mirrors_project_membership_into_ml_studio() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _ = super::super::db::init(&tmp.path().join("projects.db"));
        let _ = crate::ml_studio::db::init(&tmp.path().join("ml_studio.db"));
        let state = crate::dispatch::state::AppState::for_test();
        drop(state);

        let creator = format!("owner-{}", uuid::Uuid::new_v4());
        seed_core_user(&creator);
        let project_id = format!("mlsync-{}", uuid::Uuid::new_v4());
        let manager = format!("manager-{}", uuid::Uuid::new_v4());
        let tester = format!("tester-{}", uuid::Uuid::new_v4());
        let viewer = format!("viewer-{}", uuid::Uuid::new_v4());
        super::super::repository::create_project(
            &project_id,
            "org-ml",
            &format!("Projekt {project_id}"),
            "",
            "tests",
            "[\"knowledge\"]",
            &creator,
            &tmp.path().join(&project_id).to_string_lossy(),
            "",
            None,
            false,
            false,
            false,
            &[
                super::super::models::MemberInput {
                    user_id: manager.clone(),
                    functions: vec!["pm".into()],
                    project_admin: true,
                    expires_at: None,
                },
                super::super::models::MemberInput {
                    user_id: tester.clone(),
                    functions: vec!["tester".into()],
                    project_admin: false,
                    expires_at: None,
                },
                super::super::models::MemberInput {
                    user_id: viewer.clone(),
                    functions: vec!["observer".into()],
                    project_admin: false,
                    expires_at: None,
                },
            ],
        )
        .expect("create project");

        std::fs::create_dir_all(tmp.path().join(&project_id)).expect("project dir");
        let pool = super::super::project_db::open(&project_id).expect("project db");
        let (link_id, ml_project_id, mapped, skipped) = create_from_project(
            &pool,
            &project_id,
            "org-ml",
            &creator,
            &format!("ML {project_id}"),
            "recognition",
            "wizja",
            true,
            &default_role_map(),
        )
        .expect("create ml project");
        assert_eq!(mapped, 3, "manager + tester + viewer are granted");
        assert_eq!(skipped, 1, "the creator is already the ML owner");

        let ml_role = |user: &str| {
            crate::ml_studio::repository::member_role(&ml_project_id, user).expect("role")
        };
        assert_eq!(ml_role(&creator).as_deref(), Some("owner"));
        assert_eq!(
            ml_role(&manager).as_deref(),
            Some("editor"),
            "content roles map to editor"
        );
        assert_eq!(ml_role(&tester).as_deref(), Some("viewer"));
        assert_eq!(ml_role(&viewer).as_deref(), Some("viewer"));

        // A promoted tester is UPDATED, not re-invited.
        super::super::repository::set_member_access(
            &project_id,
            &tester,
            &["developer".into()],
            false,
            None,
        )
        .expect("promote");
        assert_eq!(
            ml_role(&tester).as_deref(),
            Some("editor"),
            "committed access changes reconcile the ML mirror immediately"
        );
        let link = get(&pool, &link_id).expect("get").expect("row");
        let outcome = sync_link(&project_id, &pool, &link);
        assert_eq!(outcome.result, "ok", "errors: {:?}", outcome.errors);
        assert_eq!(outcome.applied_update, 0, "repeat sync is idempotent");
        assert_eq!(outcome.applied_add, 0);
        assert_eq!(outcome.applied_remove, 0);
        assert_eq!(ml_role(&tester).as_deref(), Some("editor"));

        // A committed membership removal reconciles the mirror immediately.
        super::super::repository::remove_member(&project_id, &viewer).expect("remove");
        assert!(ml_role(&viewer).is_none(), "removal propagates immediately");
        let outcome = sync_link(&project_id, &pool, &link);
        assert_eq!(outcome.result, "ok", "errors: {:?}", outcome.errors);
        assert_eq!(outcome.applied_remove, 0, "repeat sync is idempotent");
        assert_eq!(
            ml_role(&creator).as_deref(),
            Some("owner"),
            "the ML owner row is never touched"
        );
        let refreshed = get(&pool, &link_id).expect("get").expect("row");
        assert_eq!(refreshed.last_sync_result, "ok");
        assert!(!refreshed.last_sync_at.is_empty());

        // A role left OUT of the map grants nothing and is counted as skipped.
        let narrow = vec![("admin".to_string(), "editor".to_string())];
        update(&pool, &link_id, "wizja", true, &role_map_to_json(&narrow)).expect("narrow map");
        let link = get(&pool, &link_id).expect("get").expect("row");
        let outcome = sync_link(&project_id, &pool, &link);
        assert!(outcome.skipped >= 1);
        assert!(ml_role(&tester).is_none(), "an unmapped role loses access");
        assert_eq!(ml_role(&manager).as_deref(), Some("editor"));

        // The card summary reads ML Studio directly — the project membership is
        // the authorization, so a viewer without Power User still sees it.
        let card = summary(&ml_project_id).expect("summary").expect("row");
        assert_eq!(card.project_type, "recognition");
        assert_eq!(card.project_type_label, "Rozpoznawanie obrazu");
        assert_eq!(card.dataset_count, 0);
        assert!(!card.training_in_progress);

        // Detaching with revoke removes what the link granted, never the owner,
        // and leaves the ML project itself in place.
        let link = get(&pool, &link_id).expect("get").expect("row");
        let removed = detach(&pool, &project_id, &link, true).expect("detach");
        assert_eq!(removed, 1, "only the mapped manager membership is revoked");
        assert!(ml_role(&manager).is_none());
        assert_eq!(ml_role(&creator).as_deref(), Some("owner"));
        assert!(get(&pool, &link_id).expect("get").is_none());
        assert!(
            summary(&ml_project_id).expect("summary").is_some(),
            "the ML project outlives the link"
        );
    }

    /// An ML owner that no longer exists in the core directory stops the sync
    /// LOUDLY: `sync_permissions` is switched off and the reason is recorded, so
    /// the project list never implies access ML Studio did not grant.
    #[test]
    fn unavailable_ml_owner_switches_the_sync_off() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _ = super::super::db::init(&tmp.path().join("projects.db"));
        let _ = crate::ml_studio::db::init(&tmp.path().join("ml_studio.db"));
        let state = crate::dispatch::state::AppState::for_test();
        drop(state);

        // An owner that was never registered in the core user directory.
        let ghost = format!("ghost-{}", uuid::Uuid::new_v4());
        let project_id = format!("mlghost-{}", uuid::Uuid::new_v4());
        super::super::repository::create_project(
            &project_id,
            "org-ml",
            &format!("Projekt {project_id}"),
            "",
            "tests",
            "[]",
            &ghost,
            &tmp.path().join(&project_id).to_string_lossy(),
            "",
            None,
            false,
            false,
            false,
            &[],
        )
        .expect("create project");
        let created = crate::ml_studio::repository::create_project(
            &ghost,
            "org-ml",
            &format!("ML {project_id}"),
            "",
            "recognition",
        )
        .expect("ml project");

        std::fs::create_dir_all(tmp.path().join(&project_id)).expect("project dir");
        let pool = super::super::project_db::open(&project_id).expect("project db");
        let link_id = uuid::Uuid::new_v4().to_string();
        insert(
            &pool,
            &link_id,
            &created.project.project_id,
            "wizja",
            "linked_existing",
            true,
            &role_map_to_json(&default_role_map()),
            &ghost,
        )
        .expect("insert link");

        let link = get(&pool, &link_id).expect("get").expect("row");
        let outcome = sync_link(&project_id, &pool, &link);
        assert_eq!(outcome.result, "owner_unavailable");
        assert!(!outcome.errors.is_empty());
        assert_eq!(outcome.applied_add, 0);

        let after = get(&pool, &link_id).expect("get").expect("row");
        assert!(
            !after.sync_permissions,
            "a link that cannot write must stop trying"
        );
        assert_eq!(after.last_sync_result, "owner_unavailable");

        // A link pointing at a deleted ML project reports that instead.
        let missing_id = uuid::Uuid::new_v4().to_string();
        insert(
            &pool,
            &missing_id,
            "ml-nieistniejacy",
            "",
            "linked_existing",
            true,
            "[]",
            &ghost,
        )
        .expect("insert link");
        let link = get(&pool, &missing_id).expect("get").expect("row");
        let outcome = sync_link(&project_id, &pool, &link);
        assert_eq!(outcome.result, "ml_project_missing");
    }

    /// A link to an EXISTING ML project must not evict that project's own team:
    /// the first sync only grants what the role map says and leaves a member the
    /// ML owner invited directly alone. Revocation still applies to what the
    /// link itself granted.
    #[test]
    fn linked_existing_keeps_the_ml_projects_own_members() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _ = super::super::db::init(&tmp.path().join("projects.db"));
        let _ = crate::ml_studio::db::init(&tmp.path().join("ml_studio.db"));
        let state = crate::dispatch::state::AppState::for_test();
        drop(state);

        let owner = format!("owner-{}", uuid::Uuid::new_v4());
        seed_core_user(&owner);
        let outsider = format!("obcy-{}", uuid::Uuid::new_v4());
        let mate = format!("kolega-{}", uuid::Uuid::new_v4());
        let project_id = format!("mlexist-{}", uuid::Uuid::new_v4());
        super::super::repository::create_project(
            &project_id,
            "org-ml",
            &format!("Projekt {project_id}"),
            "",
            "tests",
            "[\"knowledge\"]",
            &owner,
            &tmp.path().join(&project_id).to_string_lossy(),
            "",
            None,
            false,
            false,
            false,
            &[super::super::models::MemberInput {
                user_id: mate.clone(),
                functions: vec!["pm".into()],
                project_admin: true,
                expires_at: None,
            }],
        )
        .expect("create project");

        // An ML project with a team of its own, built in ML Studio.
        let created = crate::ml_studio::repository::create_project(
            &owner,
            "org-ml",
            "Wizja",
            "",
            "recognition",
        )
        .expect("ml project");
        let ml_project_id = created.project.project_id.clone();
        crate::ml_studio::repository::invite_member(&ml_project_id, &owner, &outsider, "editor")
            .expect("invite outsider");

        std::fs::create_dir_all(tmp.path().join(&project_id)).expect("project dir");
        let pool = super::super::project_db::open(&project_id).expect("project db");
        let link_id = uuid::Uuid::new_v4().to_string();
        insert(
            &pool,
            &link_id,
            &ml_project_id,
            "wizja",
            "linked_existing",
            true,
            &role_map_to_json(&default_role_map()),
            &owner,
        )
        .expect("insert link");

        let ml_role = |user: &str| {
            crate::ml_studio::repository::member_role(&ml_project_id, user).expect("role")
        };
        let link = get(&pool, &link_id).expect("get").expect("row");
        let outcome = sync_link(&project_id, &pool, &link);
        assert_eq!(outcome.result, "ok", "errors: {:?}", outcome.errors);
        assert_eq!(outcome.applied_add, 1, "only the project member is granted");
        assert_eq!(
            outcome.applied_remove, 0,
            "a member the ML owner invited is not the link's business"
        );
        assert_eq!(ml_role(&outsider).as_deref(), Some("editor"));
        assert_eq!(ml_role(&mate).as_deref(), Some("editor"));

        // What the link granted IS revoked when project membership goes away.
        super::super::repository::remove_member(&project_id, &mate).expect("remove");
        assert!(
            ml_role(&mate).is_none(),
            "committed removal revokes the mirror"
        );
        let outcome = sync_link(&project_id, &pool, &link);
        assert_eq!(outcome.result, "ok", "errors: {:?}", outcome.errors);
        assert_eq!(outcome.applied_remove, 0, "repeat sync is idempotent");
        assert_eq!(
            ml_role(&outsider).as_deref(),
            Some("editor"),
            "the ML project's own member survives every pass"
        );

        // Detaching takes back nothing else either.
        let link = get(&pool, &link_id).expect("get").expect("row");
        assert_eq!(detach(&pool, &project_id, &link, true).expect("detach"), 0);
        assert_eq!(ml_role(&outsider).as_deref(), Some("editor"));
        assert_eq!(ml_role(&owner).as_deref(), Some("owner"));
    }
    #[tokio::test]
    async fn mirrored_ml_requests_recheck_expiry_and_manual_grants_relinquish_provenance() {
        use crate::dispatch::{HandlerContext, RequestOrigin};
        use crate::services::rbac::OrgContext;
        use tentaflow_protocol::{
            MessageBody, MlStudioPayload, MlStudioProjectDetailRequest, ProtocolErrorCode,
            SessionAuth,
        };
        let tmp = tempfile::tempdir().expect("tempdir");
        let _ = super::super::db::init(&tmp.path().join("projects.db"));
        let _ = crate::ml_studio::db::init(&tmp.path().join("ml_studio.db"));
        let state = crate::dispatch::state::AppState::for_test();
        let owner = format!("ml-owner-{}", uuid::Uuid::new_v4());
        let user = format!("ml-member-{}", uuid::Uuid::new_v4());
        let newcomer = format!("ml-new-{}", uuid::Uuid::new_v4());
        seed_core_user(&owner);
        let project_id = format!("ml-expiry-{}", uuid::Uuid::new_v4());
        let dir = tmp.path().join(&project_id);
        std::fs::create_dir_all(&dir).expect("project directory");
        super::super::repository::create_project(
            &project_id,
            "org-ml",
            &project_id,
            "",
            "custom",
            "[\"knowledge\"]",
            &owner,
            &dir.to_string_lossy(),
            "",
            None,
            false,
            false,
            false,
            &[super::super::models::MemberInput {
                user_id: user.clone(),
                functions: vec!["developer".into()],
                project_admin: false,
                expires_at: Some((chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339()),
            }],
        )
        .expect("project");
        let pool = super::super::project_db::open(&project_id).expect("content db");
        let (link_id, ml_id, _, _) = create_from_project(
            &pool,
            &project_id,
            "org-ml",
            &owner,
            &project_id,
            "recognition",
            "",
            true,
            &default_role_map(),
        )
        .expect("ML link");
        crate::dispatch::app_gate::test_support::install_app(
            &state,
            "ml-studio",
            &["mlstudio.read", "mlstudio.write"],
        );
        let ctx = HandlerContext {
            session: SessionAuth::UserSession {
                user_id: [0x33; 16],
                role: None,
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state: state.clone(),
            origin: RequestOrigin::Local,
            org_context: Some(OrgContext {
                user_id: user.clone(),
                org_id: "org-ml".into(),
                role_id: "role-test".into(),
                permissions: Default::default(),
            }),
        };
        let request = MessageBody::MlStudioBody(MlStudioPayload::ProjectDetailRequest(
            MlStudioProjectDetailRequest {
                project_id: ml_id.clone(),
            },
        ));
        assert!(crate::dispatch::ml_studio::ml_studio_project_detail(&request, &ctx).is_ok());
        let dataset = crate::ml_studio::repository::create_dataset(
            &user,
            &ml_id,
            "Current access",
            "distill",
            1,
            2,
            "{}",
            b"question,answer\nA,B\n",
        )
        .expect("real dataset");
        let run_id = crate::ml_studio::repository::create_training_run(
            &ml_id,
            "{}",
            Some(&dataset.dataset_id),
        )
        .expect("real training run");
        let status_request =
            MessageBody::MlStudioBody(MlStudioPayload::DistillGenerateStatusRequest(
                tentaflow_protocol::MlStudioDistillGenerateStatusRequest {
                    dataset_id: dataset.dataset_id.clone(),
                },
            ));
        let jobs_request = MessageBody::MlStudioBody(MlStudioPayload::JobsOverviewRequest(
            tentaflow_protocol::MlStudioJobsOverviewRequest {},
        ));
        assert!(
            crate::dispatch::ml_studio::ml_studio_distill_generate_status(&status_request, &ctx)
                .await
                .is_ok()
        );
        let jobs = crate::dispatch::ml_studio::ml_studio_jobs_overview(&jobs_request, &ctx)
            .await
            .expect("jobs before expiry");
        let MessageBody::MlStudioBody(MlStudioPayload::JobsOverviewResponse(jobs)) = jobs else {
            panic!("jobs response");
        };
        assert!(jobs.jobs.iter().any(|job| job.run_id == run_id));

        super::super::db::pool().expect("registry").write().expect("write")
            .execute("UPDATE project_members SET expires_at = '2000-01-01T00:00:00Z' WHERE project_id = ?1 AND user_id = ?2", params![project_id, user]).expect("expire");
        assert_eq!(
            crate::ml_studio::db::pool()
                .expect("ML pool")
                .read()
                .expect("read")
                .query_row(
                    "SELECT role FROM project_members WHERE project_id = ?1 AND user_id = ?2",
                    params![ml_id, user],
                    |row| row.get::<_, String>(0)
                )
                .expect("cached grant"),
            "editor"
        );
        assert_eq!(
            crate::dispatch::ml_studio::ml_studio_project_detail(&request, &ctx)
                .expect_err("current expiry overrides cached grant")
                .code,
            ProtocolErrorCode::NotFound
        );
        assert!(crate::ml_studio::repository::member_role(&ml_id, &user)
            .expect("current grant")
            .is_none());
        assert!(
            crate::ml_studio::repository::get_dataset(&user, &dataset.dataset_id)
                .expect("dataset guard")
                .is_none()
        );
        assert!(crate::ml_studio::repository::list_projects(&user)
            .expect("projects")
            .iter()
            .all(|summary| summary.project.project_id != ml_id));
        assert_eq!(
            crate::dispatch::ml_studio::ml_studio_distill_generate_status(&status_request, &ctx)
                .await
                .expect_err("expired distill status")
                .code,
            ProtocolErrorCode::NotFound
        );
        let jobs = crate::dispatch::ml_studio::ml_studio_jobs_overview(&jobs_request, &ctx)
            .await
            .expect("jobs after expiry");
        let MessageBody::MlStudioBody(MlStudioPayload::JobsOverviewResponse(jobs)) = jobs else {
            panic!("jobs response");
        };
        assert!(jobs.jobs.iter().all(|job| job.run_id != run_id));
        assert_eq!(
            current_member_role(&ml_id, &owner, Some("owner".into()))
                .expect("owner remains independent")
                .as_deref(),
            Some("owner")
        );

        pool.write().expect("write").execute_batch("CREATE TRIGGER deny_ml_ledger BEFORE INSERT ON settings WHEN NEW.key LIKE 'ml_link_granted:%' BEGIN SELECT RAISE(ABORT, 'ledger write denied'); END;").expect("deny ledger writes");
        super::super::repository::add_members(
            &project_id,
            &[super::super::models::MemberInput {
                user_id: newcomer.clone(),
                functions: vec!["developer".into()],
                project_admin: false,
                expires_at: None,
            }],
            &owner,
        )
        .expect("new member");
        assert!(crate::ml_studio::repository::member_role(&ml_id, &newcomer)
            .expect("automatic mirror result")
            .is_none());
        assert_eq!(
            get(&pool, &link_id)
                .expect("automatic result")
                .expect("link")
                .last_sync_result,
            "partial"
        );
        let link = get(&pool, &link_id).expect("get").expect("link");
        let failed = sync_link(&project_id, &pool, &link);
        assert_eq!(
            failed.applied_add, 0,
            "an untracked grant must not be issued"
        );
        assert!(!failed.errors.is_empty());
        assert!(crate::ml_studio::repository::member_role(&ml_id, &newcomer)
            .expect("new membership")
            .is_none());
        assert!(
            super::super::repository::ml_grant_origins(&ml_id, &newcomer)
                .expect("origins")
                .is_empty()
        );
        assert!(
            relinquish_membership(&ml_id, &user).is_err(),
            "manual marker and ledger commit together"
        );
        assert_eq!(
            super::super::repository::ml_grant_origins(&ml_id, &user)
                .expect("old provenance")
                .len(),
            1
        );
        assert!(super::super::repository::get_setting(
            &pool,
            &format!("ml_link_manual:{link_id}:{user}")
        )
        .expect("marker")
        .is_none());
        pool.write()
            .expect("write")
            .execute_batch("DROP TRIGGER deny_ml_ledger;")
            .expect("allow writes");

        crate::ml_studio::repository::set_member_role(&ml_id, &owner, &user, "editor")
            .expect("manual grant");
        relinquish_membership(&ml_id, &user).expect("relinquish");
        assert!(super::super::repository::ml_grant_origins(&ml_id, &user)
            .expect("origins")
            .is_empty());
        assert!(!granted_users(&pool, &link_id)
            .expect("ledger")
            .contains(&user));
        assert!(
            crate::dispatch::ml_studio::ml_studio_project_detail(&request, &ctx).is_ok(),
            "a real manual grant stays independent of project expiry"
        );
        let result = sync_link(&project_id, &pool, &link);
        assert_eq!(result.result, "ok", "{:?}", result.errors);
        assert_eq!(
            crate::ml_studio::repository::member_role(&ml_id, &user)
                .expect("manual membership")
                .as_deref(),
            Some("editor")
        );
        assert!(super::super::repository::ml_grant_origins(&ml_id, &user)
            .expect("manual origins")
            .is_empty());
        let missing = tmp.path().join("missing-content");
        super::super::repository::create_project(
            "0",
            "org-ml",
            "Missing content database",
            "",
            "custom",
            "[]",
            &owner,
            &missing.to_string_lossy(),
            "",
            None,
            false,
            false,
            false,
            &[],
        )
        .expect("broken registry fixture");
        let restore =
            restore_grant_index().expect_err("startup cannot use a partially restored index");
        assert!(restore.to_string().contains("project '0' database missing"));
        assert!(
            !missing.exists(),
            "backfill must not create a fresh content database"
        );
        assert_eq!(
            super::super::repository::ml_grant_origins(&ml_id, &newcomer)
                .expect("valid persisted origin")
                .len(),
            1
        );
        let delete_id = super::super::repository::prepare_project_delete("org-ml", "0", &owner)
            .expect("prepare broken fixture cleanup");
        super::super::repository::delete_project_rows("0", &delete_id)
            .expect("remove broken fixture");
        std::mem::forget(tmp);
    }
}
