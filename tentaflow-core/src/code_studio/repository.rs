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
use super::{paths, pep, sync_capture, workspace_db};
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

/// Programs a new workspace may run under `core.exec` without asking anyone.
///
/// Without a seed the table starts empty, and an empty allowlist makes the
/// `autonomous` mode ask about `pwd` — a mode that asks about everything is a
/// mode nobody uses. The line is drawn where Codex draws it (`is_safe_command`)
/// and then pulled TIGHTER, because our grant grammar and its check are not the
/// same shape: `pep::Target::Program` names argv[0] and nothing else, so a
/// pattern cannot say "this program with these arguments". Only the programs
/// Codex treats as safe REGARDLESS of their arguments can be expressed here at
/// all.
///
/// Everything Codex admits conditionally is therefore absent, and each absence
/// is an argument that cannot be checked:
///   * `git` — safe for `status|log|diff|show|branch`, and a seeded `git` would
///     equally cover `git push`. Git belongs to the broker anyway (§11.1);
///   * `find` — `-exec`, `-delete` and `-fprintf` execute and write;
///   * `rg` — `--pre` runs a command per match;
///   * `base64` — `-o` writes a file;
///   * `sed` — safe only in the exact shape `sed -n <N,M>p`.
///
/// `cd` is absent for a different reason: `core.exec` runs an argv through
/// `execve`, where `cd` is a shell builtin and not a program. A command's
/// directory is `ExecRequest::cwd_rel`, so seeding it would grant nothing.
///
/// Nothing here writes, and nothing here opens a socket. An operator who wants
/// more adds it as a standing grant from the workspace's permission list
/// (`add_allowlist_entry`), which is the same table this seeds — so widening is
/// a recorded, revocable admin decision rather than an edit to this list.
const DEFAULT_EXEC_ALLOWLIST: &[&str] = &[
    "cat", "cut", "echo", "expr", "false", "grep", "head", "id", "ls", "nl", "numfmt", "paste",
    "pwd", "rev", "seq", "stat", "tac", "tail", "tr", "true", "uname", "uniq", "wc", "which",
    "whoami",
];

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
    if new.repo_kind == "local" {
        let candidate = std::path::Path::new(
            new.repo_url
                .as_deref()
                .ok_or_else(|| anyhow!("local directory missing"))?,
        );
        let mut stmt = tx.prepare("SELECT repo_url FROM code_workspaces WHERE node_id = ?1 AND repo_kind = 'local' AND status != 'deleted'")?;
        let paths = stmt.query_map([&new.node_id], |row| row.get::<_, String>(0))?;
        for path in paths {
            let path = std::path::PathBuf::from(path?);
            if candidate.starts_with(&path) || path.starts_with(candidate) {
                return Err(anyhow!(
                    "directory overlaps an existing project; grant access to that project instead"
                ));
            }
        }
    }
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
    for program in DEFAULT_EXEC_ALLOWLIST {
        tx.execute(
            "INSERT INTO code_workspace_allowlist \
               (workspace_id, capability, pattern, created_by, created_at) \
             VALUES (?1, 'exec', ?2, ?3, datetime('now'))",
            params![new.id, program, new.owner_user_id],
        )
        .map_err(write_err)?;
        sync_capture::capture_allowlist_entry(&tx, &new.id, "exec", program)?;
    }
    // Both rows and both captures commit together: the org must never see a
    // workspace whose owner membership did not travel with it.
    sync_capture::capture_workspace(&tx, &new.id)?;
    sync_capture::capture_member(&tx, &new.id, &new.owner_user_id)?;
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
    let mut conn = db.write().map_err(write_err)?;
    let tx = conn.transaction().map_err(write_err)?;
    let changed = tx
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
    // The status is what a node that cannot run the workspace shows instead of
    // it, so 'error' and 'archived' have to reach the whole org.
    sync_capture::capture_workspace(&tx, workspace_id)?;
    tx.commit().map_err(write_err)?;
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
    let mut conn = db.write().map_err(write_err)?;
    let tx = conn.transaction().map_err(write_err)?;
    tx.execute(
        "UPDATE code_workspaces SET default_branch = ?2, target_branch = ?3, \
         updated_at = datetime('now') WHERE id = ?1",
        params![workspace_id, default_branch, target_branch],
    )
    .map_err(write_err)?;
    sync_capture::capture_workspace(&tx, workspace_id)?;
    tx.commit().map_err(write_err)?;
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
    let mut conn = db.write().map_err(write_err)?;
    let tx = conn.transaction().map_err(write_err)?;
    tx.execute(
        "INSERT INTO code_workspace_members (workspace_id, user_id, role, added_by, added_at) \
         VALUES (?1, ?2, ?3, ?4, datetime('now')) \
         ON CONFLICT(workspace_id, user_id) DO UPDATE SET role = excluded.role",
        params![workspace_id, user_id, role.slug(), added_by],
    )
    .map_err(write_err)?;
    sync_capture::capture_member(&tx, workspace_id, user_id)?;
    tx.commit().map_err(write_err)?;
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
    // A removal replicates as a tombstone; without it a stale grant still in
    // flight would put the member back on the other nodes.
    sync_capture::capture_member(&tx, workspace_id, user_id)?;
    tx.commit().map_err(write_err)?;
    Ok(())
}

/// Platform administrators provision projects; other users need an explicit
/// organization-scoped grant because a workspace can execute code on a node.
pub fn may_create_workspace(db: &DbPool, org_id: &str, user_id: &str) -> Result<bool> {
    let conn = db.read().map_err(read_err)?;
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 WHERE EXISTS(SELECT 1 FROM user_accounts WHERE id=?2 AND is_active=1 AND (is_admin=1 OR role='admin')) OR EXISTS(SELECT 1 FROM code_workspace_creator_grants WHERE org_id=?1 AND user_id=?2)",
            params![org_id, user_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(read_err)?;
    Ok(found.is_some())
}

pub fn grant_creator(db: &DbPool, org_id: &str, user_id: &str, granted_by: &str) -> Result<()> {
    let mut conn = db.write().map_err(write_err)?;
    let tx = conn.transaction().map_err(write_err)?;
    tx.execute(
        "INSERT OR IGNORE INTO code_workspace_creator_grants (org_id, user_id, granted_by, created_at) \
         VALUES (?1, ?2, ?3, datetime('now'))",
        params![org_id, user_id, granted_by],
    )
    .map_err(write_err)?;
    // The grant is what lets a user create a workspace AT ALL, so it has to
    // reach the node they happen to be sitting at.
    sync_capture::capture_creator_grant(&tx, org_id, user_id)?;
    tx.commit().map_err(write_err)?;
    Ok(())
}

pub fn revoke_creator(db: &DbPool, org_id: &str, user_id: &str) -> Result<()> {
    let mut conn = db.write().map_err(write_err)?;
    let tx = conn.transaction().map_err(write_err)?;
    tx.execute(
        "DELETE FROM code_workspace_creator_grants WHERE org_id = ?1 AND user_id = ?2",
        params![org_id, user_id],
    )
    .map_err(write_err)?;
    sync_capture::capture_creator_grant(&tx, org_id, user_id)?;
    tx.commit().map_err(write_err)?;
    Ok(())
}

// =============================================================================
// Standing capability grants (§9.1)
// =============================================================================

/// Settings an administrator edits after the workspace exists (§25.4).
///
/// It lives here rather than in the handler because every registry write has to
/// capture for the sync ledger in its own transaction: a workspace whose
/// settings changed on one node and nowhere else is precisely the failure the
/// ledger exists to prevent.
#[allow(clippy::too_many_arguments)]
pub fn set_settings(
    db: &DbPool,
    workspace_id: &str,
    name: &str,
    autonomy_ceiling: &str,
    egress_policy: &str,
    target_branch: Option<&str>,
    index_enabled: bool,
    quota_disk_bytes: Option<i64>,
    quota_sessions: Option<i64>,
) -> Result<()> {
    let mut conn = db.write().map_err(write_err)?;
    let tx = conn.transaction().map_err(write_err)?;
    let changed = tx
        .execute(
            "UPDATE code_workspaces SET name = ?2, autonomy_ceiling = ?3, egress_policy = ?4, \
              target_branch = ?5, index_enabled = ?6, quota_disk_bytes = ?7, \
              quota_sessions = ?8, updated_at = datetime('now') WHERE id = ?1",
            params![
                workspace_id,
                name,
                autonomy_ceiling,
                egress_policy,
                target_branch,
                i64::from(index_enabled),
                quota_disk_bytes,
                quota_sessions,
            ],
        )
        .map_err(write_err)?;
    if changed == 0 {
        return Err(anyhow!("workspace not found"));
    }
    sync_capture::capture_workspace(&tx, workspace_id)?;
    tx.commit().map_err(write_err)?;

    // The registry column DECLARES the allowance; the node holding the bytes
    // enforces the RESERVATION, and the filesystem layer reads only the latter
    // (§13.5, `workspace_db::disk_quota`). Refreshing it at the next session
    // open would leave the old number enforced until then — a raised quota that
    // does not take effect and a lowered one that is not applied are both wrong
    // — so the declaration and the reservation move together.
    //
    // A node that does not host this workspace has no reservation to refresh:
    // `workspace_db::open` refuses a directory that was never provisioned, and
    // materialising one here would fake a runtime database for a workspace that
    // lives elsewhere.
    if paths::workspace_dir(workspace_id).is_ok_and(|dir| dir.is_dir()) {
        let pool = workspace_db::open(workspace_id)?;
        workspace_db::set_disk_quota(&pool, quota_disk_bytes)?;
    }
    Ok(())
}

/// Records which kind of credential the repository uses and the handle to it.
///
/// `secret_ref` replicates as a HANDLE and the material never does: the secret
/// is sealed with a per-node key and belongs on the node that runs git (§5.2).
pub fn set_repo_auth(
    db: &DbPool,
    workspace_id: &str,
    repo_auth_kind: &str,
    ssh_host_fingerprint: Option<&str>,
) -> Result<()> {
    let mut conn = db.write().map_err(write_err)?;
    let tx = conn.transaction().map_err(write_err)?;
    tx.execute(
        "UPDATE code_workspaces SET repo_auth_kind = ?2, ssh_host_fingerprint = ?3, \
         updated_at = datetime('now') WHERE id = ?1",
        params![workspace_id, repo_auth_kind, ssh_host_fingerprint],
    )
    .map_err(write_err)?;
    sync_capture::capture_workspace(&tx, workspace_id)?;
    tx.commit().map_err(write_err)?;
    Ok(())
}

/// Adds a standing `always` grant. Identity is the (workspace, capability,
/// pattern) triple — the table's AUTOINCREMENT `id` is node-local and plays no
/// part in either the lookup or the replication.
pub fn add_allowlist_entry(
    db: &DbPool,
    workspace_id: &str,
    capability: &str,
    pattern: &str,
    created_by: &str,
) -> Result<()> {
    // The durable choke point for every standing grant, so the pattern rule is
    // enforced here and not only in the handlers that happen to call it.
    pep::validate_grant_pattern(pattern).map_err(|e| anyhow!("{e}"))?;
    let mut conn = db.write().map_err(write_err)?;
    let tx = conn.transaction().map_err(write_err)?;
    tx.execute(
        "INSERT INTO code_workspace_allowlist \
           (workspace_id, capability, pattern, created_by, created_at) \
         VALUES (?1, ?2, ?3, ?4, datetime('now')) \
         ON CONFLICT(workspace_id, capability, pattern) DO NOTHING",
        params![workspace_id, capability, pattern, created_by],
    )
    .map_err(write_err)?;
    sync_capture::capture_allowlist_entry(&tx, workspace_id, capability, pattern)?;
    tx.commit().map_err(write_err)?;
    Ok(())
}

/// Withdraws a standing grant. Returns true when a row was removed.
pub fn remove_allowlist_entry(
    db: &DbPool,
    workspace_id: &str,
    capability: &str,
    pattern: &str,
) -> Result<bool> {
    let mut conn = db.write().map_err(write_err)?;
    let tx = conn.transaction().map_err(write_err)?;
    let removed = tx
        .execute(
            "DELETE FROM code_workspace_allowlist \
             WHERE workspace_id = ?1 AND capability = ?2 AND pattern = ?3",
            params![workspace_id, capability, pattern],
        )
        .map_err(write_err)?;
    // Withdrawing a standing permission must replicate as a tombstone, or a node
    // that still holds the older grant keeps executing on it.
    sync_capture::capture_allowlist_entry(&tx, workspace_id, capability, pattern)?;
    tx.commit().map_err(write_err)?;
    Ok(removed > 0)
}

// =============================================================================
// Provisioning saga
// =============================================================================
//
// Saga state is node-local run state (plan-01 §6): it lives in the instance
// content database (`code_studio::db`), not in the registry, so `local` here
// is that pool and never the main one. The durable outcome a remote UI needs
// travels on the workspace row (`status`, `status_detail`).

/// Records the outcome of one provisioning step. Idempotent per (workspace,
/// step) so a resumed saga overwrites its own history instead of appending a
/// second row nobody can order.
pub fn record_saga_step(
    local: &DbPool,
    workspace_id: &str,
    step: &str,
    status: SagaStepStatus,
    detail: Option<&str>,
) -> Result<()> {
    let conn = local.write().map_err(write_err)?;
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

pub fn list_saga_steps(local: &DbPool, workspace_id: &str) -> Result<Vec<SagaStepRecord>> {
    let conn = local.read().map_err(read_err)?;
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
pub fn step_is_done(local: &DbPool, workspace_id: &str, step: &str) -> Result<bool> {
    let conn = local.read().map_err(read_err)?;
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
    fn platform_administrator_can_provision_but_deactivated_admin_cannot() {
        let (_dir, db) = test_db();
        let id = crate::db::repository::create_user_account(&db,"project-admin","hash","Admin","").unwrap();
        db.write().unwrap().execute("UPDATE user_accounts SET is_admin=1 WHERE id=?1",[&id]).unwrap();
        assert!(may_create_workspace(&db,"org-1",&id).unwrap());
        db.write().unwrap().execute("UPDATE user_accounts SET is_active=0 WHERE id=?1",[&id]).unwrap();
        assert!(!may_create_workspace(&db,"org-1",&id).unwrap());
    }

    #[test]
    fn saga_steps_are_resumable_and_overwrite_their_own_history() {
        // Saga state is keyed by the workspace id but lives in the content
        // database; no registry row is needed to record it.
        let local = crate::code_studio::db::test_pool();

        record_saga_step(&local, "ws-1", "layout", SagaStepStatus::Done, None).unwrap();
        record_saga_step(
            &local,
            "ws-1",
            "clone",
            SagaStepStatus::Failed,
            Some("auth"),
        )
        .unwrap();
        assert!(step_is_done(&local, "ws-1", "layout").unwrap());
        assert!(!step_is_done(&local, "ws-1", "clone").unwrap());
        assert!(!step_is_done(&local, "ws-1", "never-ran").unwrap());

        record_saga_step(&local, "ws-1", "clone", SagaStepStatus::Done, None).unwrap();
        let steps = list_saga_steps(&local, "ws-1").unwrap();
        assert_eq!(
            steps.len(),
            2,
            "a retried step must not append a second row"
        );
        assert!(step_is_done(&local, "ws-1", "clone").unwrap());
    }

    /// Latest core capture for a resource, as (action, fields). Empty means the
    /// write never reached the outbox — the registry would be node-local again.
    fn latest_capture(
        db: &DbPool,
        resource_type: &str,
        resource_id: &str,
    ) -> Option<(
        String,
        std::collections::BTreeMap<String, crate::sync::ledger::FieldValue>,
    )> {
        let conn = db.read().expect("db");
        conn.query_row(
            "SELECT action, changed_fields_blob FROM __tentaflow_core_sync_captures \
             WHERE resource_type = ?1 AND resource_id = ?2 \
             ORDER BY hlc_wall DESC, hlc_logical DESC LIMIT 1",
            params![resource_type, resource_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .optional()
        .expect("capture query")
        .map(|(action, blob)| (action, crate::sync::ledger::decode(&blob).unwrap()))
    }

    fn text_field(
        fields: &std::collections::BTreeMap<String, crate::sync::ledger::FieldValue>,
        key: &str,
    ) -> Option<String> {
        match fields.get(key) {
            Some(crate::sync::ledger::FieldValue::String(value)) => Some(value.clone()),
            _ => None,
        }
    }

    #[test]
    fn a_new_workspace_and_its_owner_membership_both_reach_the_outbox() {
        let (_dir, db) = test_db();
        create_workspace(&db, &sample("ws-1", "u-owner")).unwrap();

        let (action, fields) =
            latest_capture(&db, "core.code_workspace", "ws-1").expect("workspace capture");
        assert_eq!(action, "insert");
        // The owner node travels with the row — that is what makes `is_local`
        // answerable on a node that did not create the workspace.
        assert_eq!(text_field(&fields, "node_id").as_deref(), Some("dev-ryzen"));
        assert_eq!(text_field(&fields, "org_id").as_deref(), Some("org-1"));
        assert_eq!(
            text_field(&fields, "status").as_deref(),
            Some("provisioning")
        );

        let member_id = crate::sync::resource_id::composite_resource_id(&["ws-1", "u-owner"]);
        let (action, fields) =
            latest_capture(&db, "core.code_workspace_member", &member_id).expect("member capture");
        assert_eq!(action, "insert");
        assert_eq!(text_field(&fields, "role").as_deref(), Some("owner"));
    }

    #[test]
    fn a_status_change_replicates_the_whole_row() {
        let (_dir, db) = test_db();
        create_workspace(&db, &sample("ws-1", "u-owner")).unwrap();
        set_status(&db, "ws-1", WorkspaceStatus::Error, Some("clone refused")).unwrap();

        let (action, fields) =
            latest_capture(&db, "core.code_workspace", "ws-1").expect("status capture");
        assert_eq!(action, "insert", "a status change ships the full row");
        assert_eq!(text_field(&fields, "status").as_deref(), Some("error"));
        assert_eq!(
            text_field(&fields, "status_detail").as_deref(),
            Some("clone refused")
        );
    }

    #[test]
    fn a_removed_member_and_a_revoked_grant_replicate_as_tombstones() {
        let (_dir, db) = test_db();
        create_workspace(&db, &sample("ws-1", "u-owner")).unwrap();
        upsert_member(&db, "ws-1", "u-dev", WorkspaceRole::Editor, "u-owner").unwrap();
        remove_member(&db, "ws-1", "u-dev").unwrap();

        let member_id = crate::sync::resource_id::composite_resource_id(&["ws-1", "u-dev"]);
        let (action, _) =
            latest_capture(&db, "core.code_workspace_member", &member_id).expect("capture");
        assert_eq!(
            action, "delete",
            "a removal must not be undone by an older grant"
        );

        grant_creator(&db, "org-1", "u-dev", "u-admin").unwrap();
        revoke_creator(&db, "org-1", "u-dev").unwrap();
        let grant_id = crate::sync::resource_id::composite_resource_id(&["org-1", "u-dev"]);
        let (action, _) =
            latest_capture(&db, "core.code_workspace_creator_grant", &grant_id).expect("capture");
        assert_eq!(action, "delete");
    }

    #[test]
    fn an_allowlist_entry_is_replicated_by_its_triple_not_by_its_rowid() {
        let (_dir, db) = test_db();
        create_workspace(&db, &sample("ws-1", "u-owner")).unwrap();
        add_allowlist_entry(&db, "ws-1", "net_egress", "api.example.com", "u-owner").unwrap();

        let rowid: i64 = {
            let conn = db.read().expect("db");
            conn.query_row(
                "SELECT id FROM code_workspace_allowlist \
                 WHERE workspace_id = 'ws-1' AND capability = 'net_egress'",
                [],
                |row| row.get(0),
            )
            .expect("row")
        };
        let resource_id = crate::sync::resource_id::composite_resource_id(&[
            "ws-1",
            "net_egress",
            "api.example.com",
        ]);
        let (action, fields) = latest_capture(&db, "core.code_workspace_allowlist", &resource_id)
            .expect("allowlist capture");
        assert_eq!(action, "insert");
        assert!(
            latest_capture(&db, "core.code_workspace_allowlist", &rowid.to_string()).is_none(),
            "the node-local AUTOINCREMENT id must never be the replicated identity"
        );
        assert_eq!(
            text_field(&fields, "pattern").as_deref(),
            Some("api.example.com")
        );

        assert!(remove_allowlist_entry(&db, "ws-1", "net_egress", "api.example.com").unwrap());
        let (action, _) = latest_capture(&db, "core.code_workspace_allowlist", &resource_id)
            .expect("removal capture");
        assert_eq!(action, "delete");
    }

    /// A new workspace starts with the read-only programs already permitted, or
    /// `autonomous` asks about `pwd` and is unusable. What it must NOT start
    /// with is anything that writes, reaches the network, or whose safety
    /// depends on arguments the grant grammar cannot see.
    #[test]
    fn a_new_workspace_starts_with_read_only_programs_permitted_and_nothing_else() {
        let (_dir, db) = test_db();
        create_workspace(&db, &sample("ws-1", "u-owner")).unwrap();

        let seeded: Vec<(String, String)> = {
            let conn = db.read().expect("db");
            let mut stmt = conn
                .prepare("SELECT capability, pattern FROM code_workspace_allowlist WHERE workspace_id = 'ws-1'")
                .unwrap();
            let rows = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap();
            rows.collect::<rusqlite::Result<Vec<_>>>().unwrap()
        };

        assert!(
            seeded.iter().all(|(cap, _)| cap == "exec"),
            "only `exec` may be seeded: no standing grant is created for writes or the network"
        );
        let programs: Vec<&str> = seeded.iter().map(|(_, p)| p.as_str()).collect();
        for expected in ["ls", "cat", "grep", "wc", "pwd"] {
            assert!(programs.contains(&expected), "{expected} is not permitted");
        }
        // Every one of these is either a writer, a network client, or safe only
        // for arguments a `Target::Program` pattern cannot constrain.
        for refused in [
            "git", "rg", "find", "sed", "base64", "sh", "bash", "cargo", "npm", "curl", "rm", "mv",
            "*",
        ] {
            assert!(
                !programs.contains(&refused),
                "{refused} must not be permitted without a person deciding it"
            );
        }

        // The seed replicates like any other standing grant, so a second node of
        // the org sees the same permissions.
        let resource_id = crate::sync::resource_id::composite_resource_id(&["ws-1", "exec", "ls"]);
        let (action, fields) = latest_capture(&db, "core.code_workspace_allowlist", &resource_id)
            .expect("the seeded grant must reach the outbox");
        assert_eq!(action, "insert");
        assert_eq!(text_field(&fields, "capability").as_deref(), Some("exec"));

        // And it is a normal row: an operator can withdraw it.
        assert!(remove_allowlist_entry(&db, "ws-1", "exec", "ls").unwrap());
    }

    #[test]
    fn the_vault_never_reaches_the_outbox() {
        let (_dir, db) = test_db();
        create_workspace(&db, &sample("ws-1", "u-owner")).unwrap();
        let conn = db.read().expect("db");
        let leaked: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM __tentaflow_core_sync_captures \
                 WHERE table_name IN ('code_workspace_secrets', 'code_agent_credentials', \
                                      'session_assertion_jti', 'code_workspace_saga_steps')",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(leaked, 0);
        // The vault and the saga state are not merely unlisted for sync: they
        // are not in the main database at all (plan-01 §6), so nothing the
        // capture layer could ever be pointed at holds them.
        let present: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN \
                 ('code_workspace_secrets', 'code_agent_credentials', 'code_workspace_saga_steps')",
                [],
                |row| row.get(0),
            )
            .expect("schema query");
        assert_eq!(
            present, 0,
            "node-local content tables are back in the main database"
        );
    }

    /// §13.5: the registry DECLARES the allowance, the owner node RESERVES it,
    /// and the filesystem layer enforces only the reservation. A change that
    /// stopped at the registry left the old number in force until somebody
    /// opened a session — so a raised quota did not raise anything, and the
    /// operator had no way to tell.
    #[test]
    fn a_changed_quota_reaches_the_reservation_that_actually_enforces_it() {
        let _guard = crate::code_studio::paths::test_data_dir_guard();
        let data = tempfile::tempdir().expect("data dir");
        crate::paths::set_category_override(
            crate::paths::StorageCategory::Data,
            Some(data.path().to_string_lossy().to_string()),
        );
        let (_dir, db) = test_db();
        create_workspace(&db, &sample("ws-1", "u-owner")).unwrap();
        std::fs::create_dir_all(crate::code_studio::paths::workspace_dir("ws-1").unwrap())
            .expect("workspace layout");
        let pool = workspace_db::open("ws-1").expect("workspace.db");
        workspace_db::set_disk_quota(&pool, Some(1_000)).unwrap();

        let raise = |bytes: Option<i64>| {
            set_settings(
                &db,
                "ws-1",
                "TentaFlow Core",
                "normal",
                "org_approved",
                None,
                false,
                bytes,
                None,
            )
            .unwrap();
        };

        raise(Some(8_000));
        assert_eq!(
            workspace_db::disk_quota("ws-1").unwrap(),
            Some(8_000),
            "the file layer still enforces the quota the workspace had before"
        );
        // Lowering lands the same way; the reservation follows the declaration
        // in both directions.
        raise(Some(2_000));
        assert_eq!(workspace_db::disk_quota("ws-1").unwrap(), Some(2_000));
        raise(None);
        assert_eq!(workspace_db::disk_quota("ws-1").unwrap(), None);

        workspace_db::close("ws-1");
        crate::paths::set_category_override(crate::paths::StorageCategory::Data, None);
    }

    /// A pattern the matcher gives no reading to is refused where it would be
    /// stored, not quietly kept for a later reader to interpret.
    #[test]
    fn a_standing_grant_needs_a_pattern_that_means_something() {
        let (_dir, db) = test_db();
        create_workspace(&db, &sample("ws-1", "u-owner")).unwrap();
        assert!(add_allowlist_entry(&db, "ws-1", "exec", "", "u-owner").is_err());
        assert!(add_allowlist_entry(&db, "ws-1", "exec", "cargo\u{7}", "u-owner").is_err());
        assert!(add_allowlist_entry(&db, "ws-1", "exec", "*", "u-owner").is_ok());
    }

    #[test]
    fn the_same_slug_can_be_reused_by_a_different_owner_but_not_by_the_same_one() {
        let (_dir, db) = test_db();
        create_workspace(&db, &sample("ws-1", "u-a")).unwrap();
        assert!(create_workspace(&db, &sample("ws-2", "u-a")).is_err());
        assert!(create_workspace(&db, &sample("ws-3", "u-b")).is_ok());
    }
}
