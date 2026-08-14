// ===== File: code_studio/session.rs — opening, listing and closing a work session =====
//
// A session is one person working on one branch of one workspace. Opening it
// creates a git worktree and the runtime rows that describe it; closing it
// removes the worktree but NEVER the branch — the branch carries the work and
// is subject to retention, not to a window being closed.
//
// The same "resumable saga" shape as provisioning applies, for the same reason:
// a worktree on disk and a row in `workspace.db` cannot be created atomically.
// Here it is small enough to handle inline, but the ordering is deliberate —
// the row is written LAST, so a crash leaves an orphan directory (harmless,
// detected and cleaned by `reconcile_orphans`) rather than a session row
// pointing at a worktree that does not exist (which every later call would trip
// over).

use anyhow::{anyhow, Result};
use tracing::warn;

use super::git_broker::Broker;
use super::models::{AutonomyMode, WorkspaceRecord, WorkspaceRole};
use super::workspace_db;
use crate::db::DbPool;

/// A session as the UI and the coordinator see it.
#[derive(Debug, Clone)]
pub struct SessionRecord {
    pub id: String,
    pub workspace_id: String,
    pub user_id: String,
    pub title: String,
    pub branch: String,
    pub autonomy_mode: String,
    pub flow_id: String,
    pub flow_version_id: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
}

/// What the caller decides when opening a session. The branch is NOT among
/// them: it is derived, so two sessions of the same user cannot collide and a
/// request cannot aim a session at an arbitrary branch.
#[derive(Debug, Clone)]
pub struct NewSession {
    pub id: String,
    pub user_id: String,
    /// Short human-readable slug of the user, used in the branch name.
    pub user_slug: String,
    pub title: String,
    pub autonomy_mode: AutonomyMode,
    pub flow_id: String,
    /// Version of the harness flow this session is pinned to. A session must
    /// not change shape mid-flight because someone edited the flow.
    pub flow_version_id: String,
}

/// Branch of a session: `cs/<user>/<session prefix>`. Derived rather than
/// chosen, and short enough to read in a git log.
pub fn session_branch(user_slug: &str, session_id: &str) -> String {
    let slug = sanitize_slug(user_slug);
    let short: String = session_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect();
    format!("cs/{slug}/{short}")
}

fn sanitize_slug(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "user".to_string()
    } else {
        trimmed.chars().take(24).collect()
    }
}

/// Opens a session: worktree first, runtime rows second.
///
/// The autonomy mode is clamped to the workspace ceiling here rather than
/// trusted from the request — the ceiling is the whole point of the setting,
/// and a wire value that exceeds it is a request to be corrected, not obeyed.
pub fn open_session(
    workspace: &WorkspaceRecord,
    role: WorkspaceRole,
    new: &NewSession,
) -> Result<SessionRecord> {
    if role < WorkspaceRole::Editor {
        return Err(anyhow!("opening a session requires the editor role"));
    }
    if workspace.status != "active" {
        return Err(anyhow!(
            "workspace is {}, not active; sessions cannot be opened",
            workspace.status
        ));
    }
    let ceiling = AutonomyMode::from_slug(&workspace.autonomy_ceiling)
        .ok_or_else(|| anyhow!("workspace has an unknown autonomy ceiling"))?;
    let autonomy = new.autonomy_mode.min(ceiling);

    let start_point = workspace
        .default_branch
        .as_deref()
        .ok_or_else(|| anyhow!("workspace has no default branch; provisioning did not finish"))?;
    let branch = session_branch(&new.user_slug, &new.id);

    let broker = Broker::for_workspace(&workspace.id)?;
    let worktree = broker.add_session_worktree(&new.id, &branch, start_point)?;
    let head = broker.head_commit(&broker.session(&new.id)?)?;

    let pool = workspace_db::open(&workspace.id)?;
    let written = insert_session_rows(
        &pool,
        workspace,
        new,
        &branch,
        autonomy,
        &worktree.display().to_string(),
        &head,
    );
    if written.is_err() {
        // The row is what makes a session real; without it the worktree is
        // garbage that would confuse the next reconcile. Remove it now rather
        // than leaving cleanup to a sweeper.
        if let Err(cleanup) = broker.remove_session_worktree(&new.id) {
            warn!(session_id = %new.id, "cannot remove worktree of a failed session: {cleanup:#}");
        }
    }
    written
}

#[allow(clippy::too_many_arguments)]
fn insert_session_rows(
    pool: &DbPool,
    workspace: &WorkspaceRecord,
    new: &NewSession,
    branch: &str,
    autonomy: AutonomyMode,
    worktree_path: &str,
    head_commit: &str,
) -> Result<SessionRecord> {
    let mut conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO sessions (id, workspace_id, user_id, title, branch, autonomy_mode, \
          flow_id, flow_version_id, status, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'idle', datetime('now'), datetime('now'))",
        rusqlite::params![
            new.id,
            workspace.id,
            new.user_id,
            new.title,
            branch,
            autonomy.slug(),
            new.flow_id,
            new.flow_version_id,
        ],
    )?;
    tx.execute(
        "INSERT INTO worktrees (id, session_id, purpose, op_id, path, branch, head_commit, \
          base_commit, state, created_at) \
         VALUES (?1, ?1, 'work', NULL, ?2, ?3, ?4, ?4, 'ready', datetime('now'))",
        rusqlite::params![new.id, worktree_path, branch, head_commit],
    )?;
    tx.commit()?;
    drop(conn);

    get_session(pool, &new.id)?.ok_or_else(|| anyhow!("session vanished right after insert"))
}

pub fn get_session(pool: &DbPool, session_id: &str) -> Result<Option<SessionRecord>> {
    let conn = pool.read().map_err(|e| anyhow!("workspace db read: {e}"))?;
    let row = conn
        .query_row(
            "SELECT id, workspace_id, user_id, title, branch, autonomy_mode, flow_id, \
              flow_version_id, status, created_at, updated_at, closed_at \
             FROM sessions WHERE id = ?1",
            rusqlite::params![session_id],
            read_session,
        )
        .ok();
    Ok(row)
}

/// Sessions of one user, newest first. Sessions are private: the server filters
/// by user id here, and there is no administrator bypass — a session holds the
/// person's unfinished work and their conversation with the agent.
pub fn list_sessions_for_user(pool: &DbPool, user_id: &str) -> Result<Vec<SessionRecord>> {
    let conn = pool.read().map_err(|e| anyhow!("workspace db read: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT id, workspace_id, user_id, title, branch, autonomy_mode, flow_id, \
          flow_version_id, status, created_at, updated_at, closed_at \
         FROM sessions WHERE user_id = ?1 ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map(rusqlite::params![user_id], read_session)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn read_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRecord> {
    Ok(SessionRecord {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        user_id: row.get(2)?,
        title: row.get(3)?,
        branch: row.get(4)?,
        autonomy_mode: row.get(5)?,
        flow_id: row.get(6)?,
        flow_version_id: row.get(7)?,
        status: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
        closed_at: row.get(11)?,
    })
}

/// Closes a session and removes its working worktree. The BRANCH survives: it
/// holds the work, and deleting it here would destroy unmerged commits because
/// someone closed a tab.
///
/// An integration worktree in state `held` is also kept — a merge waiting for a
/// conflict resolution is exactly what a later revision run needs.
pub fn close_session(workspace_id: &str, pool: &DbPool, session_id: &str) -> Result<()> {
    let held = held_integration_worktrees(pool, session_id)?;
    if !held.is_empty() {
        return Err(anyhow!(
            "session has {} unresolved merge worktree(s); resolve or abandon the merge first",
            held.len()
        ));
    }

    let broker = Broker::for_workspace(workspace_id)?;
    match broker.remove_session_worktree(session_id) {
        Ok(()) => {}
        Err(error) => {
            // A worktree that is already gone must not block closing; anything
            // else is reported, because silently marking a session closed while
            // its directory lives on is how disk quota disappears.
            let path = broker.session_worktree(session_id)?;
            if path.exists() {
                return Err(error);
            }
        }
    }

    let conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    conn.execute(
        "UPDATE worktrees SET state='removed', removed_at=datetime('now') \
         WHERE session_id = ?1 AND purpose='work'",
        rusqlite::params![session_id],
    )?;
    let changed = conn.execute(
        "UPDATE sessions SET status='closed', closed_at=datetime('now'), \
         updated_at=datetime('now') WHERE id = ?1",
        rusqlite::params![session_id],
    )?;
    if changed == 0 {
        return Err(anyhow!("session not found"));
    }
    Ok(())
}

fn held_integration_worktrees(pool: &DbPool, session_id: &str) -> Result<Vec<String>> {
    let conn = pool.read().map_err(|e| anyhow!("workspace db read: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT id FROM worktrees \
         WHERE session_id = ?1 AND purpose='integration' AND state='held'",
    )?;
    let rows = stmt.query_map(rusqlite::params![session_id], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Marks sessions left `running` or `waiting_user` by a restart as
/// `interrupted`. A process that was executing their runs no longer exists, so
/// leaving them "running" would show a spinner nobody is turning.
pub fn reconcile_interrupted(pool: &DbPool) -> Result<usize> {
    let conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    let changed = conn.execute(
        "UPDATE sessions SET status='interrupted', updated_at=datetime('now') \
         WHERE status IN ('creating','running','waiting_user','closing')",
        [],
    )?;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_studio::models::{EgressEnforcement, ExecMode, NewWorkspace, WorkspaceStatus};
    use crate::code_studio::{paths, provisioning, repository};

    static PATHS_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct Fixture {
        _data: tempfile::TempDir,
        _registry: tempfile::TempDir,
        db: DbPool,
        workspace: WorkspaceRecord,
        pool: DbPool,
    }

    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    fn fixture(workspace_id: &str) -> Fixture {
        let data = tempfile::tempdir().expect("data dir");
        crate::paths::set_category_override(
            crate::paths::StorageCategory::Data,
            Some(data.path().to_string_lossy().to_string()),
        );
        let registry = tempfile::tempdir().expect("registry dir");
        let db = crate::db::init(&registry.path().join("tentaflow.db")).expect("init db");
        let workspace = repository::create_workspace(
            &db,
            &NewWorkspace {
                id: workspace_id.to_string(),
                org_id: "org-1".into(),
                owner_user_id: "u-1".into(),
                name: "Workspace".into(),
                slug: workspace_id.to_string(),
                node_id: "node-1".into(),
                exec_mode: ExecMode::TrustedNative,
                container_image: None,
                egress_enforcement: EgressEnforcement::Unrestricted,
                repo_kind: "empty".into(),
                repo_url: None,
                repo_auth_kind: Some("none".into()),
                secret_ref: None,
                ssh_host_fingerprint: None,
                default_branch: Some("main".into()),
                target_branch: None,
                autonomy_ceiling: AutonomyMode::AutoEdit,
                egress_policy: "org_approved".into(),
                index_enabled: false,
                quota_disk_bytes: None,
                quota_sessions: None,
            },
        )
        .expect("create workspace");
        provisioning::provision(&db, &workspace, &provisioning::ProvisionAuth::None)
            .expect("provision");
        let workspace = repository::get_workspace(&db, workspace_id)
            .unwrap()
            .unwrap();
        let pool = workspace_db::open(workspace_id).expect("open workspace db");
        Fixture {
            _data: data,
            _registry: registry,
            db,
            workspace,
            pool,
        }
    }

    fn release() {
        crate::paths::set_category_override(crate::paths::StorageCategory::Data, None);
    }

    fn new_session(id: &str) -> NewSession {
        NewSession {
            id: id.to_string(),
            user_id: "u-1".into(),
            user_slug: "Piotr".into(),
            title: "Add the embeddings endpoint".into(),
            autonomy_mode: AutonomyMode::Normal,
            flow_id: "cs-harness".into(),
            flow_version_id: "v1".into(),
        }
    }

    #[test]
    fn a_branch_name_is_derived_and_always_safe_for_git() {
        assert_eq!(
            session_branch("Piotr", "9f2a1c4b-0e5d-4a77"),
            "cs/piotr/9f2a1c4b"
        );
        // Anything a user id might contain is flattened rather than trusted.
        assert_eq!(
            session_branch("../../etc passwd", "abcd1234-x"),
            "cs/etc-passwd/abcd1234"
        );
        assert_eq!(session_branch("", "abcd1234"), "cs/user/abcd1234");
        assert!(!session_branch("a..b", "abcd1234").contains(".."));
    }

    #[test]
    fn opening_a_session_creates_a_worktree_and_the_rows_that_describe_it() {
        let _guard = PATHS_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        if !git_available() {
            eprintln!("skipping: git is not installed");
            return;
        }
        let fx = fixture("ws-open");
        let session = open_session(&fx.workspace, WorkspaceRole::Editor, &new_session("s-1"))
            .expect("open session");

        assert_eq!(session.status, "idle");
        assert_eq!(session.branch, "cs/piotr/s1");
        let worktree = paths::session_worktree_dir("ws-open", "s-1").unwrap();
        assert!(worktree.join(".git").exists(), "no worktree on disk");

        let listed = list_sessions_for_user(&fx.pool, "u-1").unwrap();
        assert_eq!(listed.len(), 1);
        assert!(list_sessions_for_user(&fx.pool, "u-other")
            .unwrap()
            .is_empty());
        release();
    }

    #[test]
    fn the_workspace_ceiling_wins_over_the_requested_autonomy() {
        let _guard = PATHS_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        if !git_available() {
            eprintln!("skipping: git is not installed");
            return;
        }
        let fx = fixture("ws-ceiling");
        let mut request = new_session("s-1");
        request.autonomy_mode = AutonomyMode::Autonomous;

        let session = open_session(&fx.workspace, WorkspaceRole::Editor, &request).unwrap();
        assert_eq!(
            session.autonomy_mode, "auto_edit",
            "a wire value above the ceiling was obeyed"
        );
        release();
    }

    #[test]
    fn a_viewer_cannot_open_a_session_and_neither_can_anyone_on_a_broken_workspace() {
        let _guard = PATHS_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        if !git_available() {
            eprintln!("skipping: git is not installed");
            return;
        }
        let fx = fixture("ws-role");
        assert!(open_session(&fx.workspace, WorkspaceRole::Viewer, &new_session("s-1")).is_err());

        repository::set_status(&fx.db, "ws-role", WorkspaceStatus::Error, Some("broken")).unwrap();
        let broken = repository::get_workspace(&fx.db, "ws-role")
            .unwrap()
            .unwrap();
        assert!(open_session(&broken, WorkspaceRole::Owner, &new_session("s-2")).is_err());
        release();
    }

    #[test]
    fn closing_removes_the_worktree_but_keeps_the_branch() {
        let _guard = PATHS_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        if !git_available() {
            eprintln!("skipping: git is not installed");
            return;
        }
        let fx = fixture("ws-close");
        let session =
            open_session(&fx.workspace, WorkspaceRole::Editor, &new_session("s-1")).unwrap();
        let worktree = paths::session_worktree_dir("ws-close", "s-1").unwrap();

        close_session("ws-close", &fx.pool, "s-1").unwrap();
        assert!(!worktree.exists(), "the worktree survived closing");

        let closed = get_session(&fx.pool, "s-1").unwrap().unwrap();
        assert_eq!(closed.status, "closed");
        assert!(closed.closed_at.is_some());

        // The branch is still there: it holds the work.
        let broker = Broker::for_workspace("ws-close").unwrap();
        let branches = broker
            .status("s-1")
            .err()
            .map(|_| ())
            .expect("status on a removed worktree must fail");
        let _ = branches;
        let reference = broker.reference();
        let listed = std::process::Command::new("git")
            .args([
                "--git-dir",
                &reference.git_dir.display().to_string(),
                "branch",
                "--list",
                &session.branch,
            ])
            .output()
            .expect("git branch");
        assert!(
            String::from_utf8_lossy(&listed.stdout).contains(&session.branch),
            "closing a session deleted the branch with the work on it"
        );
        release();
    }

    #[test]
    fn a_session_with_an_unresolved_merge_cannot_be_closed_away() {
        let _guard = PATHS_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        if !git_available() {
            eprintln!("skipping: git is not installed");
            return;
        }
        let fx = fixture("ws-held");
        open_session(&fx.workspace, WorkspaceRole::Editor, &new_session("s-1")).unwrap();

        {
            let conn = fx.pool.write().unwrap();
            conn.execute(
                "INSERT INTO worktrees (id, session_id, purpose, op_id, path, branch, \
                  head_commit, base_commit, state, created_at) \
                 VALUES ('wt-int', 's-1', 'integration', 'op-1', '/i', NULL, 'abc', 'abc', \
                  'held', datetime('now'))",
                [],
            )
            .unwrap();
        }

        let err = close_session("ws-held", &fx.pool, "s-1").unwrap_err();
        assert!(err.to_string().contains("unresolved merge"));
        let still_open = get_session(&fx.pool, "s-1").unwrap().unwrap();
        assert_eq!(still_open.status, "idle");
        release();
    }

    #[test]
    fn sessions_left_running_by_a_restart_are_marked_interrupted() {
        let _guard = PATHS_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        if !git_available() {
            eprintln!("skipping: git is not installed");
            return;
        }
        let fx = fixture("ws-restart");
        open_session(&fx.workspace, WorkspaceRole::Editor, &new_session("s-1")).unwrap();
        {
            let conn = fx.pool.write().unwrap();
            conn.execute("UPDATE sessions SET status='running' WHERE id='s-1'", [])
                .unwrap();
        }

        assert_eq!(reconcile_interrupted(&fx.pool).unwrap(), 1);
        assert_eq!(
            get_session(&fx.pool, "s-1").unwrap().unwrap().status,
            "interrupted"
        );
        // Running it again touches nothing: an already-interrupted session is
        // not a live one, and a second boot must not rewrite its timestamp.
        assert_eq!(reconcile_interrupted(&fx.pool).unwrap(), 0);
        release();
    }
}
