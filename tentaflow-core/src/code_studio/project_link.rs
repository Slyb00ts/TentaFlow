// ===== File: code_studio/project_link.rs — links between a workspace and projects (§20) =====
//
// A link binds one Code Studio workspace to one Project Studio project. The
// relation is N:M — a workspace serves several projects, a project draws on
// several workspaces — and it is what makes the workspace visible to the
// project at all: without a link the project cannot see it, resolve a commit
// from it, or read its tree.
//
// Three rules, each of them a decision rather than a detail:
//
// **The permission mirror is one-way and only ever revokes its own grants.**
// Project roles flow into workspace membership (`project_studio/ml_link.rs` is
// the precedent), and every row the mirror creates is stamped with
// `added_by = 'project:<project_id>'`. That stamp IS the ledger: unlinking
// removes exactly the rows carrying it, so a member an owner added by hand
// keeps their access, and a member the mirror granted loses it the moment the
// project no longer says otherwise. A row the mirror does not own is never
// touched — not its role, not its existence.
//
// **The runner gets a COMMIT, never a worktree.** `test-runner` keeps its own
// sandbox (`SandboxLimits::test_runner`); mounting a session worktree into it
// would trade the isolation for the ability to test uncommitted work. §20 makes
// that trade explicitly, in favour of reproducibility: what a project tests is
// a resolved object id, and `CodeSourceRef` has no field a host path could
// travel in.
//
// **Reading is structure, not content.** A linked project can list the tree of
// a commit. File bodies, session timelines, terminals and patch sets stay
// inside Code Studio, where membership decides who sees them.

use std::collections::HashMap;

use anyhow::{anyhow, bail, Result};
use rusqlite::{params, OptionalExtension};

use super::git_broker::Broker;
use super::models::{WorkspaceRecord, WorkspaceRole};
use super::repository;
use super::sync_capture;
use crate::db::DbPool;

/// Upper bound per side of the relation. Each link costs the project screen one
/// workspace query and the workspace screen one project query, and a list that
/// grows without a bound stops being a list anyone reads.
pub const MAX_LINKS_PER_WORKSPACE: usize = 10;
pub const MAX_LINKS_PER_PROJECT: usize = 10;

/// Ceiling on one tree listing handed to a project.
pub const MAX_TREE_ENTRIES: usize = 5_000;

/// `added_by` value of every membership the mirror created. A user id can never
/// collide with it: user ids do not carry this prefix, and the mirror writes no
/// other shape.
pub const MIRROR_ORIGIN_PREFIX: &str = "project:";

pub fn mirror_origin(project_id: &str) -> String {
    format!("{MIRROR_ORIGIN_PREFIX}{project_id}")
}

fn read_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("code_studio project link read: {e}")
}

fn write_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("code_studio project link write: {e}")
}

/// One row of `code_workspace_project_links`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectLinkRecord {
    pub workspace_id: String,
    pub project_id: String,
    pub linked_by: String,
    pub created_at: String,
}

fn read_link(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectLinkRecord> {
    Ok(ProjectLinkRecord {
        workspace_id: row.get(0)?,
        project_id: row.get(1)?,
        linked_by: row.get(2)?,
        created_at: row.get(3)?,
    })
}

const LINK_COLS: &str = "workspace_id, project_id, linked_by, created_at";

// =============================================================================
// Links
// =============================================================================

/// Creates the link. The workspace must exist, belong to the same organisation
/// as the caller's project context and not be deleted — a link to a workspace
/// nobody may open would only produce confusing failures later.
pub fn link(
    db: &DbPool,
    org_id: &str,
    workspace_id: &str,
    project_id: &str,
    linked_by: &str,
) -> Result<ProjectLinkRecord> {
    let workspace = repository::get_workspace(db, workspace_id)?
        .ok_or_else(|| anyhow!("workspace not found"))?;
    if workspace.org_id != org_id {
        bail!("workspace not found");
    }
    if workspace.status == "deleted" {
        bail!("a deleted workspace cannot be linked");
    }
    if links_of_workspace(db, workspace_id)?.len() >= MAX_LINKS_PER_WORKSPACE {
        bail!("a workspace holds at most {MAX_LINKS_PER_WORKSPACE} project links");
    }
    if links_of_project(db, project_id)?.len() >= MAX_LINKS_PER_PROJECT {
        bail!("a project holds at most {MAX_LINKS_PER_PROJECT} workspace links");
    }
    {
        let mut conn = db.write().map_err(write_err)?;
        let tx = conn.transaction().map_err(write_err)?;
        tx.execute(
            "INSERT INTO code_workspace_project_links \
                (workspace_id, project_id, linked_by, created_at) \
             VALUES (?1, ?2, ?3, datetime('now')) \
             ON CONFLICT(workspace_id, project_id) DO NOTHING",
            params![workspace_id, project_id, linked_by],
        )
        .map_err(write_err)?;
        sync_capture::capture_project_link(&tx, workspace_id, project_id)?;
        tx.commit().map_err(write_err)?;
    }
    get_link(db, workspace_id, project_id)?.ok_or_else(|| anyhow!("link vanished after insert"))
}

/// Removes the link and, with it, every membership the mirror granted for this
/// project. A membership somebody granted by hand survives — the mirror never
/// had it to give back.
pub fn unlink(db: &DbPool, workspace_id: &str, project_id: &str) -> Result<bool> {
    revoke_mirror(db, workspace_id, project_id)?;
    let mut conn = db.write().map_err(write_err)?;
    let tx = conn.transaction().map_err(write_err)?;
    let removed = tx
        .execute(
            "DELETE FROM code_workspace_project_links WHERE workspace_id = ?1 AND project_id = ?2",
            params![workspace_id, project_id],
        )
        .map_err(write_err)?;
    sync_capture::capture_project_link(&tx, workspace_id, project_id)?;
    tx.commit().map_err(write_err)?;
    Ok(removed > 0)
}

pub fn get_link(
    db: &DbPool,
    workspace_id: &str,
    project_id: &str,
) -> Result<Option<ProjectLinkRecord>> {
    let conn = db.read().map_err(read_err)?;
    conn.query_row(
        &format!(
            "SELECT {LINK_COLS} FROM code_workspace_project_links \
             WHERE workspace_id = ?1 AND project_id = ?2"
        ),
        params![workspace_id, project_id],
        read_link,
    )
    .optional()
    .map_err(read_err)
}

pub fn is_linked(db: &DbPool, workspace_id: &str, project_id: &str) -> Result<bool> {
    Ok(get_link(db, workspace_id, project_id)?.is_some())
}

pub fn links_of_workspace(db: &DbPool, workspace_id: &str) -> Result<Vec<ProjectLinkRecord>> {
    let conn = db.read().map_err(read_err)?;
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {LINK_COLS} FROM code_workspace_project_links \
             WHERE workspace_id = ?1 ORDER BY created_at, project_id"
        ))
        .map_err(read_err)?;
    let rows = stmt
        .query_map(params![workspace_id], read_link)
        .map_err(read_err)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(read_err)
}

pub fn links_of_project(db: &DbPool, project_id: &str) -> Result<Vec<ProjectLinkRecord>> {
    let conn = db.read().map_err(read_err)?;
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {LINK_COLS} FROM code_workspace_project_links \
             WHERE project_id = ?1 ORDER BY created_at, workspace_id"
        ))
        .map_err(read_err)?;
    let rows = stmt
        .query_map(params![project_id], read_link)
        .map_err(read_err)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(read_err)
}

/// Workspaces this project may use. A workspace without a link is not listed —
/// which is the whole of its visibility to the project.
pub fn workspaces_for_project(
    db: &DbPool,
    org_id: &str,
    project_id: &str,
) -> Result<Vec<WorkspaceRecord>> {
    let mut out = Vec::new();
    for link in links_of_project(db, project_id)? {
        let Some(workspace) = repository::get_workspace(db, &link.workspace_id)? else {
            continue;
        };
        if workspace.org_id == org_id && workspace.status != "deleted" {
            out.push(workspace);
        }
    }
    Ok(out)
}

// =============================================================================
// Permission mirror (project → workspace)
// =============================================================================

/// Workspace role a project role earns. Everyone who may change project content
/// or run its tests becomes an editor; a viewer stays a viewer. `None` means the
/// role grants no workspace access at all — the mirror never invents one.
///
/// The mirror never grants `owner`: ownership is a Code Studio decision, and a
/// project must not be able to hand out the right to delete a workspace.
pub fn workspace_role_for(project_role: &str) -> Option<WorkspaceRole> {
    match project_role {
        "owner" | "manager" | "editor" | "tester" => Some(WorkspaceRole::Editor),
        "viewer" => Some(WorkspaceRole::Viewer),
        _ => None,
    }
}

/// What one mirror pass changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MirrorOutcome {
    pub granted: u32,
    pub updated: u32,
    pub revoked: u32,
    /// Members whose workspace row belongs to someone else — left untouched.
    pub skipped_manual: u32,
    /// Project members whose role maps to no workspace access.
    pub unmapped: u32,
}

/// Applies an explicit project member list to the workspace. The list is a
/// parameter rather than a lookup so the rule (who gets what, and what the
/// mirror may touch) is verifiable on its own, and so the caller decides which
/// registry it read.
pub fn apply_mirror(
    db: &DbPool,
    workspace_id: &str,
    project_id: &str,
    members: &[(String, String)],
) -> Result<MirrorOutcome> {
    let origin = mirror_origin(project_id);
    let mut outcome = MirrorOutcome::default();

    let mut desired: HashMap<&str, WorkspaceRole> = HashMap::new();
    for (user_id, project_role) in members {
        match workspace_role_for(project_role) {
            Some(role) => {
                // The same person can appear twice in one member list only
                // through data drift; the role lattice decides, so the outcome
                // never depends on iteration order.
                let entry = desired.entry(user_id.as_str()).or_insert(role);
                *entry = (*entry).max(role);
            }
            None => outcome.unmapped += 1,
        }
    }

    let mut conn = db.write().map_err(write_err)?;
    let tx = conn.transaction().map_err(write_err)?;
    let current: Vec<(String, String, String)> = {
        let mut stmt = tx
            .prepare(
                "SELECT user_id, role, added_by FROM code_workspace_members WHERE workspace_id = ?1",
            )
            .map_err(read_err)?;
        let rows = stmt
            .query_map(params![workspace_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(read_err)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(read_err)?
    };
    let current_map: HashMap<&str, (&str, &str)> = current
        .iter()
        .map(|(user, role, added_by)| (user.as_str(), (role.as_str(), added_by.as_str())))
        .collect();

    for (user_id, role) in &desired {
        match current_map.get(user_id) {
            // Someone else's grant. The mirror neither raises nor lowers it —
            // it does not own that membership and never will.
            Some((_, added_by)) if *added_by != origin => outcome.skipped_manual += 1,
            Some((existing, _)) if *existing == role.slug() => {}
            Some(_) => {
                tx.execute(
                    "UPDATE code_workspace_members SET role = ?3 \
                     WHERE workspace_id = ?1 AND user_id = ?2 AND added_by = ?4",
                    params![workspace_id, user_id, role.slug(), origin],
                )
                .map_err(write_err)?;
                sync_capture::capture_member(&tx, workspace_id, user_id)?;
                outcome.updated += 1;
            }
            None => {
                tx.execute(
                    "INSERT INTO code_workspace_members \
                        (workspace_id, user_id, role, added_by, added_at) \
                     VALUES (?1, ?2, ?3, ?4, datetime('now')) \
                     ON CONFLICT(workspace_id, user_id) DO NOTHING",
                    params![workspace_id, user_id, role.slug(), origin],
                )
                .map_err(write_err)?;
                sync_capture::capture_member(&tx, workspace_id, user_id)?;
                outcome.granted += 1;
            }
        }
    }

    for (user_id, _, added_by) in &current {
        if added_by != &origin || desired.contains_key(user_id.as_str()) {
            continue;
        }
        tx.execute(
            "DELETE FROM code_workspace_members \
             WHERE workspace_id = ?1 AND user_id = ?2 AND added_by = ?3",
            params![workspace_id, user_id, origin],
        )
        .map_err(write_err)?;
        sync_capture::capture_member(&tx, workspace_id, user_id)?;
        outcome.revoked += 1;
    }
    tx.commit().map_err(write_err)?;
    Ok(outcome)
}

/// Removes every membership this link granted. Used when the link goes away and
/// when a project is archived — in both cases the project stopped speaking for
/// those users, and nothing else the mirror wrote is left behind.
pub fn revoke_mirror(db: &DbPool, workspace_id: &str, project_id: &str) -> Result<u32> {
    let origin = mirror_origin(project_id);
    let mut conn = db.write().map_err(write_err)?;
    let tx = conn.transaction().map_err(write_err)?;
    // The affected users are read before the delete: a revocation replicates as
    // one tombstone per membership, and after the DELETE there is nothing left
    // to name them.
    let revoked: Vec<String> = {
        let mut stmt = tx
            .prepare(
                "SELECT user_id FROM code_workspace_members \
                 WHERE workspace_id = ?1 AND added_by = ?2",
            )
            .map_err(read_err)?;
        let rows = stmt
            .query_map(params![workspace_id, origin], |row| row.get::<_, String>(0))
            .map_err(read_err)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(read_err)?
    };
    tx.execute(
        "DELETE FROM code_workspace_members WHERE workspace_id = ?1 AND added_by = ?2",
        params![workspace_id, origin],
    )
    .map_err(write_err)?;
    for user_id in &revoked {
        sync_capture::capture_member(&tx, workspace_id, user_id)?;
    }
    tx.commit().map_err(write_err)?;
    Ok(revoked.len() as u32)
}

/// Mirrors ONE link, reading the member list from the Project Studio registry.
pub fn sync_link(db: &DbPool, workspace_id: &str, project_id: &str) -> Result<MirrorOutcome> {
    if !is_linked(db, workspace_id, project_id)? {
        bail!("workspace is not linked to this project");
    }
    let members: Vec<(String, String)> =
        crate::project_studio::repository::list_members(project_id)?
            .into_iter()
            .map(|member| (member.user_id, member.role))
            .collect();
    apply_mirror(db, workspace_id, project_id, &members)
}

/// Mirrors every workspace linked to one project. Spawned after a project
/// membership change, so it reports failures instead of propagating them: the
/// membership change already succeeded and must not be rolled back because a
/// workspace on this node could not be updated.
pub fn sync_project(db: &DbPool, project_id: &str) {
    let links = match links_of_project(db, project_id) {
        Ok(links) => links,
        Err(e) => {
            tracing::warn!(project_id, "code studio link sync skipped: {e}");
            return;
        }
    };
    for link in links {
        if let Err(e) = sync_link(db, &link.workspace_id, project_id) {
            tracing::warn!(
                project_id,
                workspace_id = %link.workspace_id,
                "code studio permission mirror failed: {e}"
            );
        }
    }
}

// =============================================================================
// Code source for a project test run
// =============================================================================

/// What a project run pins as its code source. Everything here is a NAME:
/// a branch, an object id, and at most the remote the repository came from.
/// There is deliberately no path — the runner has its own sandbox and never
/// sees this node's filesystem (§20).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeSourceRef {
    pub workspace_id: String,
    pub project_id: String,
    pub branch: String,
    /// Fully resolved object id. A branch name would let the tested code drift
    /// between the decision and the run.
    pub commit: String,
    /// Remote the workspace was cloned from, with any credentials stripped.
    /// `None` for a workspace with no remote, which no runner can fetch — the
    /// submission path has to refuse such a run rather than pretend.
    pub repo_url: Option<String>,
}

impl CodeSourceRef {
    /// The exact object a run submission carries. Kept as an explicit, closed
    /// set of keys: a serializer over a wider struct is how a host path ends up
    /// on the wire by accident.
    pub fn to_runner_json(&self) -> serde_json::Value {
        serde_json::json!({
            "kind": "code_studio_commit",
            "workspace_id": self.workspace_id,
            "project_id": self.project_id,
            "branch": self.branch,
            "commit": self.commit,
            "repo_url": self.repo_url,
        })
    }
}

/// Removes `user:password@` from a remote URL. The vault never lets credentials
/// into `repo_url`, and this makes a hand-edited registry row unable to change
/// that.
fn scrub_credentials(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    match rest.split_once('@') {
        Some((_, host)) => format!("{scheme}://{host}"),
        None => url.to_string(),
    }
}

/// Resolves a revision of a linked workspace into a commit-pinned source
/// reference. `None` — for a missing workspace, a workspace of another
/// organisation and an unlinked one alike — is the uniform answer that keeps a
/// project from probing which workspaces exist.
pub fn resolve_code_source(
    db: &DbPool,
    org_id: &str,
    project_id: &str,
    workspace_id: &str,
    revision: &str,
) -> Result<Option<CodeSourceRef>> {
    let Some(workspace) = visible_workspace(db, org_id, project_id, workspace_id)? else {
        return Ok(None);
    };
    let branch = if revision.trim().is_empty() {
        workspace
            .target_branch
            .clone()
            .or_else(|| workspace.default_branch.clone())
            .ok_or_else(|| anyhow!("workspace has no branch to resolve"))?
    } else {
        revision.trim().to_string()
    };
    let broker = Broker::for_workspace(&workspace.id)?;
    let handle = broker.reference();
    let commit = broker
        .rev_parse(&handle, &branch)?
        .ok_or_else(|| anyhow!("revision '{branch}' does not exist in this workspace"))?;
    Ok(Some(CodeSourceRef {
        workspace_id: workspace.id,
        project_id: project_id.to_string(),
        branch,
        commit,
        repo_url: workspace.repo_url.as_deref().map(scrub_credentials),
    }))
}

/// One entry of a repository listing handed to a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoEntry {
    pub path: String,
    pub mode: String,
    pub blob_oid: String,
}

/// Structure of a commit, optionally under a path prefix. Content is NOT part
/// of this: a project reads what the repository contains, not what the files
/// say.
pub fn repo_tree(
    db: &DbPool,
    org_id: &str,
    project_id: &str,
    workspace_id: &str,
    commit: &str,
    prefix: &str,
    limit: usize,
) -> Result<Option<Vec<RepoEntry>>> {
    let Some(workspace) = visible_workspace(db, org_id, project_id, workspace_id)? else {
        return Ok(None);
    };
    let broker = Broker::for_workspace(&workspace.id)?;
    let handle = broker.reference();
    let entries = broker
        .list_tree(&handle, commit)?
        .into_iter()
        .filter(|entry| prefix.is_empty() || entry.path.starts_with(prefix))
        .take(limit.min(MAX_TREE_ENTRIES))
        .map(|entry| RepoEntry {
            path: entry.path,
            mode: entry.mode,
            blob_oid: entry.oid,
        })
        .collect();
    Ok(Some(entries))
}

fn visible_workspace(
    db: &DbPool,
    org_id: &str,
    project_id: &str,
    workspace_id: &str,
) -> Result<Option<WorkspaceRecord>> {
    if !is_linked(db, workspace_id, project_id)? {
        return Ok(None);
    }
    let Some(workspace) = repository::get_workspace(db, workspace_id)? else {
        return Ok(None);
    };
    if workspace.org_id != org_id || workspace.status == "deleted" {
        return Ok(None);
    }
    Ok(Some(workspace))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_studio::models::{
        AutonomyMode, EgressEnforcement, ExecMode, NewWorkspace, WorkspaceStatus,
    };
    use crate::code_studio::paths;

    const ORG: &str = "org-1";

    fn test_db() -> (tempfile::TempDir, DbPool) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = crate::db::init(&dir.path().join("tentaflow.db")).expect("init db");
        (dir, db)
    }

    fn workspace(db: &DbPool, id: &str, slug: &str) -> WorkspaceRecord {
        let created = repository::create_workspace(
            db,
            &NewWorkspace {
                id: id.to_string(),
                org_id: ORG.into(),
                owner_user_id: "u-owner".into(),
                name: slug.to_string(),
                slug: slug.to_string(),
                node_id: "node-1".into(),
                exec_mode: ExecMode::TrustedNative,
                container_image: None,
                egress_enforcement: EgressEnforcement::Unrestricted,
                repo_kind: "git".into(),
                repo_url: Some("https://piotr:secret@example.invalid/app.git".into()),
                repo_auth_kind: Some("token".into()),
                secret_ref: None,
                ssh_host_fingerprint: None,
                default_branch: Some("main".into()),
                target_branch: Some("main".into()),
                autonomy_ceiling: AutonomyMode::Normal,
                egress_policy: "org_approved".into(),
                index_enabled: false,
                quota_disk_bytes: None,
                quota_sessions: None,
            },
        )
        .expect("create workspace");
        repository::set_status(db, id, WorkspaceStatus::Active, None).unwrap();
        created
    }

    fn members(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(user, role)| (user.to_string(), role.to_string()))
            .collect()
    }

    fn role(db: &DbPool, workspace_id: &str, user_id: &str) -> Option<WorkspaceRole> {
        repository::role_of(db, workspace_id, user_id).unwrap()
    }

    fn added_by(db: &DbPool, workspace_id: &str, user_id: &str) -> Option<String> {
        repository::list_members(db, workspace_id)
            .unwrap()
            .into_iter()
            .find(|member| member.user_id == user_id)
            .map(|member| member.added_by)
    }

    #[test]
    fn the_mirror_grants_the_role_the_mapping_says() {
        let (_dir, db) = test_db();
        workspace(&db, "ws-1", "app");
        link(&db, ORG, "ws-1", "p-1", "u-owner").unwrap();

        let outcome = apply_mirror(
            &db,
            "ws-1",
            "p-1",
            &members(&[
                ("u-a", "owner"),
                ("u-b", "manager"),
                ("u-c", "editor"),
                ("u-d", "tester"),
                ("u-e", "viewer"),
                ("u-f", "auditor"),
            ]),
        )
        .unwrap();

        assert_eq!(outcome.granted, 5);
        assert_eq!(outcome.unmapped, 1, "an unknown project role was mapped");
        for user in ["u-a", "u-b", "u-c", "u-d"] {
            assert_eq!(
                role(&db, "ws-1", user),
                Some(WorkspaceRole::Editor),
                "{user}"
            );
        }
        assert_eq!(role(&db, "ws-1", "u-e"), Some(WorkspaceRole::Viewer));
        assert_eq!(role(&db, "ws-1", "u-f"), None);
        assert_eq!(
            added_by(&db, "ws-1", "u-a").as_deref(),
            Some("project:p-1"),
            "the mirror did not stamp its own grant"
        );
    }

    #[test]
    fn a_second_pass_changes_nothing_and_follows_a_role_change() {
        let (_dir, db) = test_db();
        workspace(&db, "ws-1", "app");
        link(&db, ORG, "ws-1", "p-1", "u-owner").unwrap();

        apply_mirror(&db, "ws-1", "p-1", &members(&[("u-a", "viewer")])).unwrap();
        let again = apply_mirror(&db, "ws-1", "p-1", &members(&[("u-a", "viewer")])).unwrap();
        assert_eq!(again, MirrorOutcome::default(), "an idempotent pass wrote");

        let promoted = apply_mirror(&db, "ws-1", "p-1", &members(&[("u-a", "editor")])).unwrap();
        assert_eq!(promoted.updated, 1);
        assert_eq!(role(&db, "ws-1", "u-a"), Some(WorkspaceRole::Editor));

        let dropped = apply_mirror(&db, "ws-1", "p-1", &members(&[])).unwrap();
        assert_eq!(dropped.revoked, 1);
        assert_eq!(role(&db, "ws-1", "u-a"), None);
    }

    #[test]
    fn unlinking_takes_back_only_what_the_mirror_granted() {
        let (_dir, db) = test_db();
        workspace(&db, "ws-1", "app");
        link(&db, ORG, "ws-1", "p-1", "u-owner").unwrap();

        // Granted by a person, not by the mirror.
        repository::upsert_member(&db, "ws-1", "u-hand", WorkspaceRole::Editor, "u-owner").unwrap();
        // The same person is also a project member — the mirror must still not
        // take over a row it did not create.
        let outcome = apply_mirror(
            &db,
            "ws-1",
            "p-1",
            &members(&[("u-hand", "viewer"), ("u-mirror", "editor")]),
        )
        .unwrap();
        assert_eq!(outcome.skipped_manual, 1);
        assert_eq!(
            role(&db, "ws-1", "u-hand"),
            Some(WorkspaceRole::Editor),
            "the mirror overwrote a manual grant"
        );

        assert!(unlink(&db, "ws-1", "p-1").unwrap());
        assert_eq!(
            role(&db, "ws-1", "u-hand"),
            Some(WorkspaceRole::Editor),
            "unlinking removed a membership the mirror never granted"
        );
        assert_eq!(role(&db, "ws-1", "u-mirror"), None);
        assert_eq!(
            role(&db, "ws-1", "u-owner"),
            Some(WorkspaceRole::Owner),
            "unlinking touched the workspace owner"
        );
        assert!(!is_linked(&db, "ws-1", "p-1").unwrap());
    }

    #[test]
    fn two_links_of_the_same_workspace_do_not_revoke_each_other() {
        let (_dir, db) = test_db();
        workspace(&db, "ws-1", "app");
        link(&db, ORG, "ws-1", "p-1", "u-owner").unwrap();
        link(&db, ORG, "ws-1", "p-2", "u-owner").unwrap();

        apply_mirror(&db, "ws-1", "p-1", &members(&[("u-a", "editor")])).unwrap();
        let second = apply_mirror(&db, "ws-1", "p-2", &members(&[("u-b", "viewer")])).unwrap();
        assert_eq!(second.revoked, 0, "one project revoked another's grant");
        assert_eq!(role(&db, "ws-1", "u-a"), Some(WorkspaceRole::Editor));
        assert_eq!(role(&db, "ws-1", "u-b"), Some(WorkspaceRole::Viewer));

        unlink(&db, "ws-1", "p-2").unwrap();
        assert_eq!(role(&db, "ws-1", "u-a"), Some(WorkspaceRole::Editor));
        assert_eq!(role(&db, "ws-1", "u-b"), None);
    }

    #[test]
    fn the_relation_is_many_to_many() {
        let (_dir, db) = test_db();
        workspace(&db, "ws-1", "app");
        workspace(&db, "ws-2", "tools");
        link(&db, ORG, "ws-1", "p-1", "u-owner").unwrap();
        link(&db, ORG, "ws-1", "p-2", "u-owner").unwrap();
        link(&db, ORG, "ws-2", "p-1", "u-owner").unwrap();

        assert_eq!(links_of_workspace(&db, "ws-1").unwrap().len(), 2);
        let for_project: Vec<String> = workspaces_for_project(&db, ORG, "p-1")
            .unwrap()
            .into_iter()
            .map(|w| w.id)
            .collect();
        assert_eq!(for_project, vec!["ws-1".to_string(), "ws-2".to_string()]);

        // Linking twice is not a second link.
        link(&db, ORG, "ws-1", "p-1", "u-owner").unwrap();
        assert_eq!(links_of_project(&db, "p-1").unwrap().len(), 2);
    }

    #[test]
    fn a_workspace_of_another_organisation_cannot_be_linked() {
        let (_dir, db) = test_db();
        workspace(&db, "ws-1", "app");
        assert!(link(&db, "org-other", "ws-1", "p-1", "u-owner").is_err());
        assert!(!is_linked(&db, "ws-1", "p-1").unwrap());
    }

    #[test]
    fn an_unlinked_workspace_is_invisible_to_the_project() {
        let (_dir, db) = test_db();
        workspace(&db, "ws-1", "app");

        assert!(workspaces_for_project(&db, ORG, "p-1").unwrap().is_empty());
        assert!(
            resolve_code_source(&db, ORG, "p-1", "ws-1", "main")
                .unwrap()
                .is_none(),
            "an unlinked workspace resolved a commit"
        );
        assert!(repo_tree(&db, ORG, "p-1", "ws-1", "HEAD", "", 10)
            .unwrap()
            .is_none());
        // A workspace that does not exist answers exactly the same way.
        assert!(resolve_code_source(&db, ORG, "p-1", "ws-absent", "main")
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_run_is_pinned_to_a_commit_and_carries_no_host_path() {
        if std::process::Command::new("git")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_err()
        {
            return;
        }
        let _guard = paths::test_data_dir_guard();
        let data = tempfile::tempdir().expect("data dir");
        crate::paths::set_category_override(
            crate::paths::StorageCategory::Data,
            Some(data.path().to_string_lossy().to_string()),
        );
        let workspace_id = "2f2a1c4b-0e5d-4a77-9c31-8a2b6d4e1f22";
        let (_dir, db) = test_db();
        workspace(&db, workspace_id, "app");
        link(&db, ORG, workspace_id, "p-1", "u-owner").unwrap();
        let root = paths::create_workspace_layout(workspace_id).expect("layout");
        Broker::at(root.clone())
            .init_repository("main")
            .expect("init repo");

        let source = resolve_code_source(&db, ORG, "p-1", workspace_id, "")
            .unwrap()
            .expect("a linked workspace resolves");
        assert_eq!(source.branch, "main");
        assert_eq!(source.commit.len(), 40, "the branch was not resolved");
        assert_eq!(
            source.repo_url.as_deref(),
            Some("https://example.invalid/app.git"),
            "credentials survived into the run descriptor"
        );

        let payload = source.to_runner_json();
        let text = payload.to_string();
        assert!(text.contains(&source.commit));
        for forbidden in [
            root.to_string_lossy().to_string(),
            data.path().to_string_lossy().to_string(),
            "worktrees".to_string(),
            "/repo".to_string(),
        ] {
            assert!(
                !text.contains(&forbidden),
                "a host path reached the runner payload: {forbidden} in {text}"
            );
        }
        // Structure is readable, content is not part of the answer.
        let tree = repo_tree(&db, ORG, "p-1", workspace_id, &source.commit, "", 100)
            .unwrap()
            .expect("a linked workspace lists its tree");
        assert!(tree.is_empty(), "the initial commit is empty");

        crate::paths::set_category_override(crate::paths::StorageCategory::Data, None);
    }
}
