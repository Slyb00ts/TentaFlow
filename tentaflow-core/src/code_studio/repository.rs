// ===== File: code_studio/repository.rs — CRUD over the Code Studio registry =====
//
// The registry lives in the main database (migration 125) and is carried by the
// Sync Ledger, so a workspace is visible from every node of the org. Only its
// OWNER node can run it; the others show it and say so.
//
// Two invariants this module enforces rather than documents:
//   1. a workspace never exists without its owner membership — both rows are
//      written in ONE transaction;
//   2. a non-member gets `Ok(None)`, never a distinguishable error, so the
//      existence of someone else's workspace does not leak.

use anyhow::{anyhow, Result};
use rusqlite::{params, OptionalExtension};

use super::models::{
    NewWorkspace, SagaStepRecord, SagaStepStatus, WorkspaceMemberRecord, WorkspaceRecord,
    WorkspaceRole, WorkspaceStatus,
};
use crate::db::DbPool;

fn read_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("code_studio db read: {e}")
}

fn write_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("code_studio db write: {e}")
}

const WORKSPACE_COLS: &str = "id, org_id, owner_user_id, name, slug, node_id, exec_mode, \
     container_image, egress_enforcement, repo_kind, repo_url, repo_auth_kind, secret_ref, \
     ssh_host_fingerprint, default_branch, target_branch, autonomy_ceiling, egress_policy, \
     index_enabled, quota_disk_bytes, quota_sessions, status, status_detail, created_at, updated_at";

fn read_workspace(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkspaceRecord> {
    Ok(WorkspaceRecord {
        id: row.get(0)?,
        org_id: row.get(1)?,
        owner_user_id: row.get(2)?,
        name: row.get(3)?,
        slug: row.get(4)?,
        node_id: row.get(5)?,
        exec_mode: row.get(6)?,
        container_image: row.get(7)?,
        egress_enforcement: row.get(8)?,
        repo_kind: row.get(9)?,
        repo_url: row.get(10)?,
        repo_auth_kind: row.get(11)?,
        secret_ref: row.get(12)?,
        ssh_host_fingerprint: row.get(13)?,
        default_branch: row.get(14)?,
        target_branch: row.get(15)?,
        autonomy_ceiling: row.get(16)?,
        egress_policy: row.get(17)?,
        index_enabled: row.get::<_, i64>(18)? != 0,
        quota_disk_bytes: row.get(19)?,
        quota_sessions: row.get(20)?,
        status: row.get(21)?,
        status_detail: row.get(22)?,
        created_at: row.get(23)?,
        updated_at: row.get(24)?,
    })
}

/// Inserts the workspace together with its owner membership in one
/// transaction. The row starts as `provisioning`: the directory, runtime
/// database and clone are the saga's job, and until it finishes the workspace
/// must not look usable.
pub fn create_workspace(db: &DbPool, new: &NewWorkspace) -> Result<WorkspaceRecord> {
    if new.exec_mode == super::models::ExecMode::Container && new.container_image.is_none() {
        return Err(anyhow!("container mode requires a container image"));
    }
    let mut conn = db.write().map_err(write_err)?;
    let tx = conn.transaction().map_err(write_err)?;
    tx.execute(
        "INSERT INTO code_workspaces (id, org_id, owner_user_id, name, slug, node_id, exec_mode, \
          container_image, egress_enforcement, repo_kind, repo_url, repo_auth_kind, secret_ref, \
          ssh_host_fingerprint, default_branch, target_branch, autonomy_ceiling, egress_policy, \
          index_enabled, quota_disk_bytes, quota_sessions, status, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, \
          ?19, ?20, ?21, 'provisioning', datetime('now'), datetime('now'))",
        params![
            new.id,
            new.org_id,
            new.owner_user_id,
            new.name,
            new.slug,
            new.node_id,
            new.exec_mode.slug(),
            new.container_image,
            new.egress_enforcement.slug(),
            new.repo_kind,
            new.repo_url,
            new.repo_auth_kind,
            new.secret_ref,
            new.ssh_host_fingerprint,
            new.default_branch,
            new.target_branch,
            new.autonomy_ceiling.slug(),
            new.egress_policy,
            i64::from(new.index_enabled),
            new.quota_disk_bytes,
            new.quota_sessions,
        ],
    )
    .map_err(write_err)?;
    tx.execute(
        "INSERT INTO code_workspace_members (workspace_id, user_id, role, added_by, added_at) \
         VALUES (?1, ?2, 'owner', ?2, datetime('now'))",
        params![new.id, new.owner_user_id],
    )
    .map_err(write_err)?;
    tx.commit().map_err(write_err)?;
    drop(conn);

    get_workspace(db, &new.id)?.ok_or_else(|| anyhow!("workspace vanished right after insert"))
}

pub fn get_workspace(db: &DbPool, workspace_id: &str) -> Result<Option<WorkspaceRecord>> {
    let conn = db.read().map_err(read_err)?;
    conn.query_row(
        &format!("SELECT {WORKSPACE_COLS} FROM code_workspaces WHERE id = ?1"),
        params![workspace_id],
        read_workspace,
    )
    .optional()
    .map_err(read_err)
}

/// Returns the workspace only when the caller is a member. A non-member gets
/// `None` — the same answer as "does not exist", so probing ids reveals
/// nothing.
pub fn get_workspace_for_member(
    db: &DbPool,
    workspace_id: &str,
    user_id: &str,
) -> Result<Option<WorkspaceRecord>> {
    let conn = db.read().map_err(read_err)?;
    conn.query_row(
        &format!(
            "SELECT {WORKSPACE_COLS} FROM code_workspaces w \
             WHERE w.id = ?1 AND EXISTS ( \
                SELECT 1 FROM code_workspace_members m \
                WHERE m.workspace_id = w.id AND m.user_id = ?2)"
        ),
        params![workspace_id, user_id],
        read_workspace,
    )
    .optional()
    .map_err(read_err)
}

/// Workspaces the user is a member of, newest first. `deleted` rows are never
/// listed; `archived` ones are, because they stay readable.
pub fn list_workspaces_for_user(
    db: &DbPool,
    org_id: &str,
    user_id: &str,
) -> Result<Vec<WorkspaceRecord>> {
    let conn = db.read().map_err(read_err)?;
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {WORKSPACE_COLS} FROM code_workspaces w \
             WHERE w.org_id = ?1 AND w.status <> 'deleted' AND EXISTS ( \
                SELECT 1 FROM code_workspace_members m \
                WHERE m.workspace_id = w.id AND m.user_id = ?2) \
             ORDER BY w.created_at DESC"
        ))
        .map_err(read_err)?;
    let rows = stmt
        .query_map(params![org_id, user_id], read_workspace)
        .map_err(read_err)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(read_err)
}

/// Workspaces this node owns and must therefore supervise. Used at boot to
/// reconcile provisioning left unfinished by a restart.
pub fn list_workspaces_on_node(db: &DbPool, node_id: &str) -> Result<Vec<WorkspaceRecord>> {
    let conn = db.read().map_err(read_err)?;
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {WORKSPACE_COLS} FROM code_workspaces \
             WHERE node_id = ?1 AND status NOT IN ('deleted','archived') \
             ORDER BY created_at"
        ))
        .map_err(read_err)?;
    let rows = stmt
        .query_map(params![node_id], read_workspace)
        .map_err(read_err)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(read_err)
}

/// Moves the workspace to a new status. `status_detail` carries the reason for
/// `error` and is cleared on every other transition, so a stale failure message
/// cannot outlive the failure.
pub fn set_status(
    db: &DbPool,
    workspace_id: &str,
    status: WorkspaceStatus,
    detail: Option<&str>,
) -> Result<()> {
    let conn = db.write().map_err(write_err)?;
    let changed = conn
        .execute(
            "UPDATE code_workspaces SET status = ?2, status_detail = ?3, \
             updated_at = datetime('now') WHERE id = ?1",
            params![
                workspace_id,
                status.slug(),
                if status == WorkspaceStatus::Error {
                    detail
                } else {
                    None
                }
            ],
        )
        .map_err(write_err)?;
    if changed == 0 {
        return Err(anyhow!("workspace not found"));
    }
    Ok(())
}

/// Records the branch names discovered by the clone. Kept separate from the
/// creation payload because the caller cannot know them before the repository
/// is fetched.
pub fn set_branches(
    db: &DbPool,
    workspace_id: &str,
    default_branch: &str,
    target_branch: &str,
) -> Result<()> {
    let conn = db.write().map_err(write_err)?;
    conn.execute(
        "UPDATE code_workspaces SET default_branch = ?2, target_branch = ?3, \
         updated_at = datetime('now') WHERE id = ?1",
        params![workspace_id, default_branch, target_branch],
    )
    .map_err(write_err)?;
    Ok(())
}

// =============================================================================
// Members and creator grants
// =============================================================================

pub fn role_of(db: &DbPool, workspace_id: &str, user_id: &str) -> Result<Option<WorkspaceRole>> {
    let conn = db.read().map_err(read_err)?;
    let slug: Option<String> = conn
        .query_row(
            "SELECT role FROM code_workspace_members WHERE workspace_id = ?1 AND user_id = ?2",
            params![workspace_id, user_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(read_err)?;
    Ok(slug.as_deref().and_then(WorkspaceRole::from_slug))
}

pub fn list_members(db: &DbPool, workspace_id: &str) -> Result<Vec<WorkspaceMemberRecord>> {
    let conn = db.read().map_err(read_err)?;
    let mut stmt = conn
        .prepare(
            "SELECT workspace_id, user_id, role, added_by, added_at \
             FROM code_workspace_members WHERE workspace_id = ?1 ORDER BY added_at",
        )
        .map_err(read_err)?;
    let rows = stmt
        .query_map(params![workspace_id], |row| {
            Ok(WorkspaceMemberRecord {
                workspace_id: row.get(0)?,
                user_id: row.get(1)?,
                role: row.get(2)?,
                added_by: row.get(3)?,
                added_at: row.get(4)?,
            })
        })
        .map_err(read_err)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(read_err)
}

pub fn upsert_member(
    db: &DbPool,
    workspace_id: &str,
    user_id: &str,
    role: WorkspaceRole,
    added_by: &str,
) -> Result<()> {
    let conn = db.write().map_err(write_err)?;
    conn.execute(
        "INSERT INTO code_workspace_members (workspace_id, user_id, role, added_by, added_at) \
         VALUES (?1, ?2, ?3, ?4, datetime('now')) \
         ON CONFLICT(workspace_id, user_id) DO UPDATE SET role = excluded.role",
        params![workspace_id, user_id, role.slug(), added_by],
    )
    .map_err(write_err)?;
    Ok(())
}

/// Removes a member. The last owner cannot be removed — a workspace without an
/// owner would be unreachable for everyone, including an administrator.
pub fn remove_member(db: &DbPool, workspace_id: &str, user_id: &str) -> Result<()> {
    let mut conn = db.write().map_err(write_err)?;
    let tx = conn.transaction().map_err(write_err)?;
    let owners: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM code_workspace_members \
             WHERE workspace_id = ?1 AND role = 'owner'",
            params![workspace_id],
            |row| row.get(0),
        )
        .map_err(read_err)?;
    let is_owner: bool = tx
        .query_row(
            "SELECT role FROM code_workspace_members WHERE workspace_id = ?1 AND user_id = ?2",
            params![workspace_id, user_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(read_err)?
        .is_some_and(|role| role == "owner");
    if is_owner && owners <= 1 {
        return Err(anyhow!("a workspace cannot lose its last owner"));
    }
    tx.execute(
        "DELETE FROM code_workspace_members WHERE workspace_id = ?1 AND user_id = ?2",
        params![workspace_id, user_id],
    )
    .map_err(write_err)?;
    tx.commit().map_err(write_err)?;
    Ok(())
}

/// Creating a workspace costs disk and grants the ability to execute code, so
/// it is not implied by any role — it needs an explicit per-user grant.
pub fn may_create_workspace(db: &DbPool, org_id: &str, user_id: &str) -> Result<bool> {
    let conn = db.read().map_err(read_err)?;
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM code_workspace_creator_grants WHERE org_id = ?1 AND user_id = ?2",
            params![org_id, user_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(read_err)?;
    Ok(found.is_some())
}

pub fn grant_creator(db: &DbPool, org_id: &str, user_id: &str, granted_by: &str) -> Result<()> {
    let conn = db.write().map_err(write_err)?;
    conn.execute(
        "INSERT OR IGNORE INTO code_workspace_creator_grants (org_id, user_id, granted_by, created_at) \
         VALUES (?1, ?2, ?3, datetime('now'))",
        params![org_id, user_id, granted_by],
    )
    .map_err(write_err)?;
    Ok(())
}

pub fn revoke_creator(db: &DbPool, org_id: &str, user_id: &str) -> Result<()> {
    let conn = db.write().map_err(write_err)?;
    conn.execute(
        "DELETE FROM code_workspace_creator_grants WHERE org_id = ?1 AND user_id = ?2",
        params![org_id, user_id],
    )
    .map_err(write_err)?;
    Ok(())
}

// =============================================================================
// Provisioning saga
// =============================================================================

/// Records the outcome of one provisioning step. Idempotent per (workspace,
/// step) so a resumed saga overwrites its own history instead of appending a
/// second row nobody can order.
pub fn record_saga_step(
    db: &DbPool,
    workspace_id: &str,
    step: &str,
    status: SagaStepStatus,
    detail: Option<&str>,
) -> Result<()> {
    let conn = db.write().map_err(write_err)?;
    conn.execute(
        "INSERT INTO code_workspace_saga_steps (workspace_id, step, status, detail, updated_at) \
         VALUES (?1, ?2, ?3, ?4, datetime('now')) \
         ON CONFLICT(workspace_id, step) DO UPDATE SET \
            status = excluded.status, detail = excluded.detail, updated_at = excluded.updated_at",
        params![workspace_id, step, status.slug(), detail],
    )
    .map_err(write_err)?;
    Ok(())
}

pub fn list_saga_steps(db: &DbPool, workspace_id: &str) -> Result<Vec<SagaStepRecord>> {
    let conn = db.read().map_err(read_err)?;
    let mut stmt = conn
        .prepare(
            "SELECT workspace_id, step, status, detail, updated_at \
             FROM code_workspace_saga_steps WHERE workspace_id = ?1 ORDER BY updated_at, step",
        )
        .map_err(read_err)?;
    let rows = stmt
        .query_map(params![workspace_id], |row| {
            Ok(SagaStepRecord {
                workspace_id: row.get(0)?,
                step: row.get(1)?,
                status: row.get(2)?,
                detail: row.get(3)?,
                updated_at: row.get(4)?,
            })
        })
        .map_err(read_err)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(read_err)
}

/// True when the step already completed, so a resumed saga can skip it.
pub fn step_is_done(db: &DbPool, workspace_id: &str, step: &str) -> Result<bool> {
    let conn = db.read().map_err(read_err)?;
    let status: Option<String> = conn
        .query_row(
            "SELECT status FROM code_workspace_saga_steps WHERE workspace_id = ?1 AND step = ?2",
            params![workspace_id, step],
            |row| row.get(0),
        )
        .optional()
        .map_err(read_err)?;
    Ok(status.as_deref() == Some("done"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_studio::models::{AutonomyMode, EgressEnforcement, ExecMode, WorkspaceStatus};

    fn test_db() -> (tempfile::TempDir, DbPool) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = crate::db::init(&dir.path().join("tentaflow.db")).expect("init db");
        (dir, db)
    }

    fn sample(id: &str, owner: &str) -> NewWorkspace {
        NewWorkspace {
            id: id.to_string(),
            org_id: "org-1".into(),
            owner_user_id: owner.to_string(),
            name: "TentaFlow Core".into(),
            slug: "tentaflow-core".into(),
            node_id: "dev-ryzen".into(),
            exec_mode: ExecMode::TrustedNative,
            container_image: None,
            egress_enforcement: EgressEnforcement::Unrestricted,
            repo_kind: "git".into(),
            repo_url: Some("https://example.invalid/repo.git".into()),
            repo_auth_kind: Some("none".into()),
            secret_ref: None,
            ssh_host_fingerprint: None,
            default_branch: None,
            target_branch: None,
            autonomy_ceiling: AutonomyMode::Normal,
            egress_policy: "org_approved".into(),
            index_enabled: false,
            quota_disk_bytes: None,
            quota_sessions: None,
        }
    }

    #[test]
    fn a_new_workspace_starts_provisioning_and_already_has_its_owner() {
        let (_dir, db) = test_db();
        let created = create_workspace(&db, &sample("ws-1", "u-owner")).unwrap();
        assert_eq!(created.status, "provisioning");
        assert_eq!(
            role_of(&db, "ws-1", "u-owner").unwrap(),
            Some(WorkspaceRole::Owner)
        );
        assert_eq!(list_members(&db, "ws-1").unwrap().len(), 1);
    }

    #[test]
    fn a_non_member_cannot_tell_the_workspace_from_a_missing_one() {
        let (_dir, db) = test_db();
        create_workspace(&db, &sample("ws-1", "u-owner")).unwrap();
        assert!(get_workspace_for_member(&db, "ws-1", "u-stranger")
            .unwrap()
            .is_none());
        assert!(get_workspace_for_member(&db, "ws-missing", "u-stranger")
            .unwrap()
            .is_none());
        assert!(get_workspace_for_member(&db, "ws-1", "u-owner")
            .unwrap()
            .is_some());
    }

    #[test]
    fn container_mode_without_an_image_is_refused_before_it_reaches_the_table() {
        let (_dir, db) = test_db();
        let mut new = sample("ws-1", "u-owner");
        new.exec_mode = ExecMode::Container;
        assert!(create_workspace(&db, &new).is_err());

        new.container_image = Some("ghcr.io/example/dev:1".into());
        assert!(create_workspace(&db, &new).is_ok());
    }

    #[test]
    fn a_workspace_cannot_lose_its_last_owner() {
        let (_dir, db) = test_db();
        create_workspace(&db, &sample("ws-1", "u-owner")).unwrap();
        assert!(remove_member(&db, "ws-1", "u-owner").is_err());

        upsert_member(&db, "ws-1", "u-second", WorkspaceRole::Owner, "u-owner").unwrap();
        assert!(remove_member(&db, "ws-1", "u-owner").is_ok());
        assert_eq!(
            role_of(&db, "ws-1", "u-second").unwrap(),
            Some(WorkspaceRole::Owner)
        );
    }

    #[test]
    fn an_editor_can_be_removed_and_a_role_can_be_changed_in_place() {
        let (_dir, db) = test_db();
        create_workspace(&db, &sample("ws-1", "u-owner")).unwrap();
        upsert_member(&db, "ws-1", "u-dev", WorkspaceRole::Viewer, "u-owner").unwrap();
        upsert_member(&db, "ws-1", "u-dev", WorkspaceRole::Editor, "u-owner").unwrap();
        assert_eq!(
            role_of(&db, "ws-1", "u-dev").unwrap(),
            Some(WorkspaceRole::Editor)
        );
        assert_eq!(list_members(&db, "ws-1").unwrap().len(), 2);

        remove_member(&db, "ws-1", "u-dev").unwrap();
        assert_eq!(role_of(&db, "ws-1", "u-dev").unwrap(), None);
    }

    #[test]
    fn listing_hides_deleted_workspaces_but_keeps_archived_ones() {
        let (_dir, db) = test_db();
        create_workspace(&db, &sample("ws-1", "u-owner")).unwrap();
        create_workspace(&db, &{
            let mut other = sample("ws-2", "u-owner");
            other.slug = "second".into();
            other
        })
        .unwrap();

        set_status(&db, "ws-2", WorkspaceStatus::Archived, None).unwrap();
        assert_eq!(
            list_workspaces_for_user(&db, "org-1", "u-owner")
                .unwrap()
                .len(),
            2
        );

        set_status(&db, "ws-2", WorkspaceStatus::Deleted, None).unwrap();
        let listed = list_workspaces_for_user(&db, "org-1", "u-owner").unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "ws-1");
    }

    #[test]
    fn a_failure_reason_does_not_outlive_the_failure() {
        let (_dir, db) = test_db();
        create_workspace(&db, &sample("ws-1", "u-owner")).unwrap();

        set_status(&db, "ws-1", WorkspaceStatus::Error, Some("clone refused")).unwrap();
        let row = get_workspace(&db, "ws-1").unwrap().unwrap();
        assert_eq!(row.status, "error");
        assert_eq!(row.status_detail.as_deref(), Some("clone refused"));

        set_status(&db, "ws-1", WorkspaceStatus::Active, None).unwrap();
        let row = get_workspace(&db, "ws-1").unwrap().unwrap();
        assert_eq!(row.status, "active");
        assert_eq!(row.status_detail, None, "stale error text survived");
    }

    #[test]
    fn creating_needs_an_explicit_grant_that_can_be_revoked() {
        let (_dir, db) = test_db();
        assert!(!may_create_workspace(&db, "org-1", "u-dev").unwrap());

        grant_creator(&db, "org-1", "u-dev", "u-admin").unwrap();
        grant_creator(&db, "org-1", "u-dev", "u-admin").unwrap();
        assert!(may_create_workspace(&db, "org-1", "u-dev").unwrap());

        revoke_creator(&db, "org-1", "u-dev").unwrap();
        assert!(!may_create_workspace(&db, "org-1", "u-dev").unwrap());
    }

    #[test]
    fn saga_steps_are_resumable_and_overwrite_their_own_history() {
        let (_dir, db) = test_db();
        create_workspace(&db, &sample("ws-1", "u-owner")).unwrap();

        record_saga_step(&db, "ws-1", "layout", SagaStepStatus::Done, None).unwrap();
        record_saga_step(&db, "ws-1", "clone", SagaStepStatus::Failed, Some("auth")).unwrap();
        assert!(step_is_done(&db, "ws-1", "layout").unwrap());
        assert!(!step_is_done(&db, "ws-1", "clone").unwrap());
        assert!(!step_is_done(&db, "ws-1", "never-ran").unwrap());

        record_saga_step(&db, "ws-1", "clone", SagaStepStatus::Done, None).unwrap();
        let steps = list_saga_steps(&db, "ws-1").unwrap();
        assert_eq!(
            steps.len(),
            2,
            "a retried step must not append a second row"
        );
        assert!(step_is_done(&db, "ws-1", "clone").unwrap());
    }

    #[test]
    fn the_same_slug_can_be_reused_by_a_different_owner_but_not_by_the_same_one() {
        let (_dir, db) = test_db();
        create_workspace(&db, &sample("ws-1", "u-a")).unwrap();
        assert!(create_workspace(&db, &sample("ws-2", "u-a")).is_err());
        assert!(create_workspace(&db, &sample("ws-3", "u-b")).is_ok());
    }
}
