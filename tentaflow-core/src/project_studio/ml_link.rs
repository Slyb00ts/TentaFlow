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

fn granted_users(pool: &DbPool, link_id: &str) -> HashSet<String> {
    super::repository::get_setting(pool, &granted_key(link_id))
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
        .map(|list| list.into_iter().collect())
        .unwrap_or_default()
}

fn set_granted_users(pool: &DbPool, link_id: &str, users: &HashSet<String>) {
    let mut list: Vec<&String> = users.iter().collect();
    list.sort();
    if let Err(e) = super::repository::set_setting(
        pool,
        &granted_key(link_id),
        &serde_json::to_string(&list).unwrap_or_else(|_| "[]".to_string()),
    ) {
        tracing::warn!(link_id, "ml link grant ledger write failed: {e}");
    }
}

fn clear_granted_users(pool: &DbPool, link_id: &str) {
    if let Err(e) = super::repository::set_setting(pool, &granted_key(link_id), "[]") {
        tracing::warn!(link_id, "ml link grant ledger clear failed: {e}");
    }
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
    [
        ("owner", "editor"),
        ("manager", "editor"),
        ("editor", "editor"),
        ("tester", "viewer"),
        ("viewer", "viewer"),
    ]
    .into_iter()
    .map(|(p, m)| (p.to_string(), m.to_string()))
    .collect()
}

pub fn role_map_from_json(json: &str) -> Vec<(String, String)> {
    match serde_json::from_str::<Vec<(String, String)>>(json) {
        Ok(map) if !map.is_empty() => map,
        _ => default_role_map(),
    }
}

pub fn role_map_to_json(map: &[(String, String)]) -> String {
    serde_json::to_string(map).unwrap_or_else(|_| "[]".to_string())
}

/// Validates a wire-supplied mapping: known project roles, ML roles limited to
/// editor/viewer, no duplicates.
pub fn validate_role_map(map: &[(String, String)]) -> Result<()> {
    let mut seen: Vec<&str> = Vec::with_capacity(map.len());
    for (project_role, ml_role) in map {
        if super::models::ProjectRole::from_slug(project_role).is_none() {
            bail!("unknown project role '{project_role}'");
        }
        if !ML_ROLES.contains(&ml_role.as_str()) {
            bail!("ML Studio only grants 'editor' or 'viewer' (got '{ml_role}')");
        }
        if seen.contains(&project_role.as_str()) {
            bail!("duplicate mapping for role '{project_role}'");
        }
        seen.push(project_role);
    }
    Ok(())
}

/// ML role for one project role, or `None` when the mapping drops it (a role
/// left out of the map gets NO ML access).
fn ml_role_for(map: &[(String, String)], project_role: &str) -> Option<String> {
    map.iter()
        .find(|(p, _)| p == project_role)
        .map(|(_, m)| m.clone())
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
    let linked: Vec<String> = list(pool)?
        .into_iter()
        .map(|l| l.ml_project_id)
        .collect();
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
fn available_owner(ml_project_id: &str) -> Option<String> {
    let owner = crate::ml_studio::repository::list_members(ml_project_id)
        .ok()?
        .into_iter()
        .find(|m| m.role == "owner")?
        .user_id;
    let core = crate::db::global_pool()?;
    crate::db::repository::get_user_role(&core, &owner)
        .ok()
        .flatten()?;
    Some(owner)
}

/// Applies the project's member list to ONE link. Idempotent: it computes the
/// desired ML membership from the project roles and issues only the deltas.
pub fn sync_link(project_id: &str, pool: &DbPool, link: &MlLinkRecord) -> SyncOutcome {
    let mut outcome = SyncOutcome::default();
    if summary(&link.ml_project_id).ok().flatten().is_none() {
        outcome.result = "ml_project_missing".to_string();
        outcome
            .errors
            .push("projekt ML nie istnieje".to_string());
        record_sync_result(pool, &link.link_id, &outcome.result);
        return outcome;
    }
    let Some(owner) = available_owner(&link.ml_project_id) else {
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
    let members = match super::repository::list_members(project_id) {
        Ok(members) => members,
        Err(e) => {
            outcome.result = "partial".to_string();
            outcome.errors.push(e.to_string());
            record_sync_result(pool, &link.link_id, &outcome.result);
            return outcome;
        }
    };
    let mut desired: HashMap<String, String> = HashMap::new();
    for member in members {
        if member.user_id == owner {
            // The ML owner row is never touched — demoting it would lock the
            // link out of its own project.
            continue;
        }
        match ml_role_for(&role_map, &member.role) {
            Some(role) => {
                desired.insert(member.user_id, role);
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

    let mut granted = granted_users(pool, &link.link_id);
    for (user_id, role) in &desired {
        match current_map.get(user_id) {
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
    set_granted_users(pool, &link.link_id, &granted);

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

    let (mapped, skipped, granted) =
        apply_role_map(project_id, &ml_project_id, creator_user_id, role_map);
    set_granted_users(pool, &link_id, &granted);
    record_sync_result(pool, &link_id, "ok");
    Ok((link_id, ml_project_id, mapped, skipped))
}

/// Grants every project member their mapped ML role. The creator is skipped —
/// they are already the ML owner, and ML Studio refuses a self-invite. Returns
/// the users actually granted, which seeds the link's grant ledger.
fn apply_role_map(
    project_id: &str,
    ml_project_id: &str,
    creator_user_id: &str,
    role_map: &[(String, String)],
) -> (u32, u32, HashSet<String>) {
    let mut granted = HashSet::new();
    let Ok(members) = super::repository::list_members(project_id) else {
        return (0, 0, granted);
    };
    let mut mapped = 0u32;
    let mut skipped = 0u32;
    for member in members {
        if member.user_id == creator_user_id {
            skipped += 1;
            continue;
        }
        let Some(role) = ml_role_for(role_map, &member.role) else {
            skipped += 1;
            continue;
        };
        let existing =
            crate::ml_studio::repository::member_role(ml_project_id, &member.user_id).ok().flatten();
        let applied = match existing {
            Some(current) if current == role => Ok(()),
            Some(_) => crate::ml_studio::repository::set_member_role(
                ml_project_id,
                creator_user_id,
                &member.user_id,
                &role,
            )
            .map(|_| ()),
            None => crate::ml_studio::repository::invite_member(
                ml_project_id,
                creator_user_id,
                &member.user_id,
                &role,
            )
            .map(|_| ()),
        };
        match applied {
            Ok(()) => {
                mapped += 1;
                granted.insert(member.user_id);
            }
            Err(e) => {
                skipped += 1;
                tracing::warn!(
                    ml_project_id,
                    user_id = %member.user_id,
                    "ml role mapping failed: {e}"
                );
            }
        }
    }
    (mapped, skipped, granted)
}

/// Removes the ML memberships this link granted (never the ML owner) and drops
/// the link. The ML project itself is never deleted — it may hold datasets and
/// trained models that outlive the link.
pub fn detach(pool: &DbPool, link: &MlLinkRecord, revoke_members: bool) -> Result<u32> {
    let mut removed = 0u32;
    if revoke_members {
        if let Some(owner) = available_owner(&link.ml_project_id) {
            let granted = granted_users(pool, &link.link_id);
            if let Ok(current) = crate::ml_studio::repository::list_members(&link.ml_project_id) {
                for member in current {
                    // Only what this link granted is taken back; the ML
                    // project's own team outlives the link, like its datasets.
                    if member.role == "owner" || !granted.contains(&member.user_id) {
                        continue;
                    }
                    if crate::ml_studio::repository::remove_member(
                        &link.ml_project_id,
                        &owner,
                        &member.user_id,
                    )
                    .is_ok()
                    {
                        removed += 1;
                    }
                }
            }
        }
    }
    delete(pool, &link.link_id)?;
    clear_granted_users(pool, &link.link_id);
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

    /// The five project roles collapse onto ML Studio's two, a role left out of
    /// the map grants NOTHING, and a bogus mapping is refused.
    #[test]
    fn role_map_collapses_five_project_roles_onto_two() {
        let map = default_role_map();
        assert_eq!(map.len(), 5);
        for (project_role, expected) in [
            ("owner", "editor"),
            ("manager", "editor"),
            ("editor", "editor"),
            ("tester", "viewer"),
            ("viewer", "viewer"),
        ] {
            assert_eq!(
                ml_role_for(&map, project_role).as_deref(),
                Some(expected),
                "role {project_role} maps wrong"
            );
        }
        validate_role_map(&map).expect("default map is valid");

        // A partial map drops the roles it omits.
        let partial = vec![("manager".to_string(), "editor".to_string())];
        assert_eq!(ml_role_for(&partial, "manager").as_deref(), Some("editor"));
        assert!(ml_role_for(&partial, "tester").is_none());

        for bad in [
            vec![("root".to_string(), "editor".to_string())],
            vec![("manager".to_string(), "owner".to_string())],
            vec![
                ("manager".to_string(), "editor".to_string()),
                ("manager".to_string(), "viewer".to_string()),
            ],
        ] {
            assert!(validate_role_map(&bad).is_err(), "accepted {bad:?}");
        }

        // The JSON round-trip keeps the mapping, and junk falls back to the default.
        let json = role_map_to_json(&map);
        assert_eq!(role_map_from_json(&json), map);
        assert_eq!(role_map_from_json("nonsense"), default_role_map());
        assert_eq!(role_map_from_json("[]"), default_role_map());
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
            "[]",
            &creator,
            "/tmp/none",
            &[
                (manager.clone(), "manager".to_string()),
                (tester.clone(), "tester".to_string()),
                (viewer.clone(), "viewer".to_string()),
            ],
        )
        .expect("create project");

        let pool = pool();
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

        let ml_role = |user: &str| crate::ml_studio::repository::member_role(&ml_project_id, user)
            .expect("role");
        assert_eq!(ml_role(&creator).as_deref(), Some("owner"));
        assert_eq!(
            ml_role(&manager).as_deref(),
            Some("editor"),
            "content roles map to editor"
        );
        assert_eq!(ml_role(&tester).as_deref(), Some("viewer"));
        assert_eq!(ml_role(&viewer).as_deref(), Some("viewer"));

        // A promoted tester is UPDATED, not re-invited.
        super::super::repository::set_member_role(&project_id, &tester, "editor")
            .expect("promote");
        let link = get(&pool, &link_id).expect("get").expect("row");
        let outcome = sync_link(&project_id, &pool, &link);
        assert_eq!(outcome.result, "ok", "errors: {:?}", outcome.errors);
        assert_eq!(outcome.applied_update, 1);
        assert_eq!(outcome.applied_add, 0);
        assert_eq!(outcome.applied_remove, 0);
        assert_eq!(ml_role(&tester).as_deref(), Some("editor"));

        // Losing project membership revokes ML access on the next pass.
        super::super::repository::remove_member(&project_id, &viewer).expect("remove");
        let outcome = sync_link(&project_id, &pool, &link);
        assert_eq!(outcome.applied_remove, 1, "errors: {:?}", outcome.errors);
        assert!(ml_role(&viewer).is_none(), "removal propagates immediately");
        assert_eq!(ml_role(&creator).as_deref(), Some("owner"), "the ML owner row is never touched");
        let refreshed = get(&pool, &link_id).expect("get").expect("row");
        assert_eq!(refreshed.last_sync_result, "ok");
        assert!(!refreshed.last_sync_at.is_empty());

        // A role left OUT of the map grants nothing and is counted as skipped.
        let narrow = vec![("manager".to_string(), "editor".to_string())];
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
        let removed = detach(&pool, &link, true).expect("detach");
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
            "/tmp/none",
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

        let pool = pool();
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
            "[]",
            &owner,
            "/tmp/none",
            &[(mate.clone(), "manager".to_string())],
        )
        .expect("create project");

        // An ML project with a team of its own, built in ML Studio.
        let created =
            crate::ml_studio::repository::create_project(&owner, "org-ml", "Wizja", "", "recognition")
                .expect("ml project");
        let ml_project_id = created.project.project_id.clone();
        crate::ml_studio::repository::invite_member(&ml_project_id, &owner, &outsider, "editor")
            .expect("invite outsider");

        let pool = pool();
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
        let outcome = sync_link(&project_id, &pool, &link);
        assert_eq!(outcome.applied_remove, 1, "errors: {:?}", outcome.errors);
        assert!(ml_role(&mate).is_none());
        assert_eq!(
            ml_role(&outsider).as_deref(),
            Some("editor"),
            "the ML project's own member survives every pass"
        );

        // Detaching takes back nothing else either.
        let link = get(&pool, &link_id).expect("get").expect("row");
        assert_eq!(detach(&pool, &link, true).expect("detach"), 0);
        assert_eq!(ml_role(&outsider).as_deref(), Some("editor"));
        assert_eq!(ml_role(&owner).as_deref(), Some("owner"));
    }
}
