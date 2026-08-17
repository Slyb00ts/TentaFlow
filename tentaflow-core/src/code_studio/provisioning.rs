// ===== File: code_studio/provisioning.rs — building a workspace as a resumable saga =====
//
// Creating a workspace touches four places that cannot be updated atomically:
// the registry row, the directory tree, the node-local vault and a git
// repository fetched over the network. A crash halfway through must therefore
// leave something honest behind, and the honest answer is `error` with a
// resumable step list — never an `active` workspace that is missing its clone.
//
// Every step is idempotent and identified by `(workspace_id, step)` in
// `code_workspace_saga_steps`, so a resume skips what is already `done` and
// redoes the rest. Compensation runs in reverse on an unrecoverable failure.
//
// One decision worth naming: for an empty repository the saga makes an initial
// EMPTY COMMIT. A fresh `git init` has no HEAD, so `git worktree add` fails and
// `base_commit` would be undefined — which would break the compare-and-swap on
// the branch ref and the "commit is built from the accepted blobs of a known
// base tree" rule. With the empty commit, "every workspace has a base_commit"
// holds without exceptions.

use anyhow::{anyhow, Result};
use tracing::warn;

use super::git_broker::{Broker, GitAuth};
use super::models::{SagaStepStatus, WorkspaceRecord, WorkspaceStatus};
use super::{paths, repository, workspace_db};
use crate::db::DbPool;

/// Steps of the saga, in order. The names are persisted, so they are part of
/// the on-disk contract: renaming one would orphan the recorded progress of a
/// workspace that is mid-provisioning.
pub const STEP_LAYOUT: &str = "layout";
pub const STEP_RUNTIME_DB: &str = "runtime_db";
pub const STEP_SECRET: &str = "secret";
pub const STEP_REPOSITORY: &str = "repository";
pub const STEP_ACTIVATE: &str = "activate";

/// Credential material for the initial clone. It is passed in rather than read
/// here, because the vault is the caller's concern and the material must not
/// linger in this module.
pub enum ProvisionAuth {
    None,
    Token(String),
    SshKey {
        private_key: String,
        known_host: Option<String>,
    },
}

impl ProvisionAuth {
    fn to_git_auth(&self) -> GitAuth {
        match self {
            ProvisionAuth::None => GitAuth::None,
            ProvisionAuth::Token(token) => GitAuth::Token(token.clone()),
            ProvisionAuth::SshKey {
                private_key,
                known_host,
            } => GitAuth::SshKey {
                private_key: private_key.clone(),
                known_host: known_host.clone(),
            },
        }
    }
}

/// What provisioning produced. Returned so the caller can report the branch
/// without re-reading the registry.
#[derive(Debug, Clone)]
pub struct ProvisionOutcome {
    pub default_branch: String,
    pub base_commit: String,
}

/// Runs (or resumes) provisioning of a workspace that already has its registry
/// row. Safe to call again after a failure: completed steps are skipped.
pub fn provision(
    db: &DbPool,
    workspace: &WorkspaceRecord,
    auth: &ProvisionAuth,
) -> Result<ProvisionOutcome> {
    match run_steps(db, workspace, auth) {
        Ok(outcome) => Ok(outcome),
        Err(error) => {
            // The workspace stays visible and explicitly broken, with the
            // reason attached. Nothing here pretends the workspace is usable.
            let detail = format!("{error:#}");
            if let Err(nested) =
                repository::set_status(db, &workspace.id, WorkspaceStatus::Error, Some(&detail))
            {
                warn!(workspace_id = %workspace.id, "cannot record provisioning failure: {nested:#}");
            }
            Err(error)
        }
    }
}

fn run_steps(
    db: &DbPool,
    workspace: &WorkspaceRecord,
    auth: &ProvisionAuth,
) -> Result<ProvisionOutcome> {
    step(db, &workspace.id, STEP_LAYOUT, || {
        paths::create_workspace_layout(&workspace.id).map(|_| ())
    })?;

    step(db, &workspace.id, STEP_RUNTIME_DB, || {
        let dir = paths::workspace_dir(&workspace.id)?;
        workspace_db::open_pool_at(&dir).map(|_| ())
    })?;

    // The secret is written by the caller before provisioning starts (it holds
    // the cipher); this step only asserts that the handle the registry points
    // at actually resolves on THIS node. A workspace whose secret lives on
    // another node must fail here rather than at the first fetch.
    step(db, &workspace.id, STEP_SECRET, || {
        match (&workspace.repo_auth_kind, &workspace.secret_ref) {
            (Some(kind), None) if kind != "none" => Err(anyhow!(
                "repository needs {kind} credentials but no secret is stored"
            )),
            _ => Ok(()),
        }
    })?;

    let outcome = repository_step(db, workspace, auth)?;

    step(db, &workspace.id, STEP_ACTIVATE, || {
        repository::set_branches(
            db,
            &workspace.id,
            &outcome.default_branch,
            &outcome.default_branch,
        )?;
        repository::set_status(db, &workspace.id, WorkspaceStatus::Active, None)
    })?;

    Ok(outcome)
}

/// S4: create or clone the repository. This is the only step that reaches the
/// network, and the only one whose compensation removes real content.
fn repository_step(
    db: &DbPool,
    workspace: &WorkspaceRecord,
    auth: &ProvisionAuth,
) -> Result<ProvisionOutcome> {
    let broker = Broker::for_workspace(&workspace.id)?;

    if repository::step_is_done(db, &workspace.id, STEP_REPOSITORY)? {
        // Resuming: read the branch back from the repository instead of
        // trusting a registry field that may predate the clone.
        let handle = broker.reference();
        let head = broker.head_commit(&handle)?;
        let branch = workspace
            .default_branch
            .clone()
            .ok_or_else(|| anyhow!("repository step is done but no default branch was recorded"))?;
        return Ok(ProvisionOutcome {
            default_branch: branch,
            base_commit: head,
        });
    }

    repository::record_saga_step(
        db,
        &workspace.id,
        STEP_REPOSITORY,
        SagaStepStatus::Pending,
        None,
    )?;

    let result = match workspace.repo_kind.as_str() {
        "empty" => {
            let branch = workspace.default_branch.as_deref().unwrap_or("main");
            broker
                .init_repository(branch)
                .map(|created| ProvisionOutcome {
                    default_branch: created.default_branch,
                    base_commit: created.head_commit,
                })
        }
        "git" => {
            let url = workspace
                .repo_url
                .as_deref()
                .ok_or_else(|| anyhow!("a git workspace needs a repository url"))?;
            broker
                .clone_repository(url, &auth.to_git_auth())
                .map(|(cloned, _target)| ProvisionOutcome {
                    default_branch: cloned.default_branch,
                    base_commit: cloned.head_commit,
                })
        }
        other => Err(anyhow!("unknown repository kind {other}")),
    };

    match result {
        Ok(outcome) => {
            repository::record_saga_step(
                db,
                &workspace.id,
                STEP_REPOSITORY,
                SagaStepStatus::Done,
                Some(&outcome.base_commit),
            )?;
            Ok(outcome)
        }
        Err(error) => {
            // Compensate: a half-written `repo/` would make a resumed clone
            // fail on a non-empty target, so it is removed and the step is
            // marked compensated — which a resume treats as "redo", unlike a
            // plain failure that may still hold a partial effect to inspect.
            let detail = format!("{error:#}");
            if let Ok(dir) = paths::repo_dir(&workspace.id) {
                if let Err(cleanup) = std::fs::remove_dir_all(&dir) {
                    if cleanup.kind() != std::io::ErrorKind::NotFound {
                        warn!(workspace_id = %workspace.id, "cannot clean partial repo: {cleanup}");
                    }
                }
            }
            repository::record_saga_step(
                db,
                &workspace.id,
                STEP_REPOSITORY,
                SagaStepStatus::Compensated,
                Some(&detail),
            )?;
            Err(error)
        }
    }
}

/// Runs one idempotent step unless it is already recorded as done.
fn step<F>(db: &DbPool, workspace_id: &str, name: &str, body: F) -> Result<()>
where
    F: FnOnce() -> Result<()>,
{
    if repository::step_is_done(db, workspace_id, name)? {
        return Ok(());
    }
    repository::record_saga_step(db, workspace_id, name, SagaStepStatus::Pending, None)?;
    match body() {
        Ok(()) => {
            repository::record_saga_step(db, workspace_id, name, SagaStepStatus::Done, None)?;
            Ok(())
        }
        Err(error) => {
            let detail = format!("{error:#}");
            repository::record_saga_step(
                db,
                workspace_id,
                name,
                SagaStepStatus::Failed,
                Some(&detail),
            )?;
            Err(error)
        }
    }
}

/// Reconciles workspaces this node owns that were left mid-provisioning by a
/// restart. A `provisioning` row with no live saga is not going to finish by
/// itself, so it is moved to `error` with an explicit reason — the user gets a
/// "retry", not a workspace stuck in a spinner forever.
pub fn reconcile_interrupted(db: &DbPool, node_id: &str) -> Result<usize> {
    let mut touched = 0;
    for workspace in repository::list_workspaces_on_node(db, node_id)? {
        if workspace.status != WorkspaceStatus::Provisioning.slug() {
            continue;
        }
        repository::set_status(
            db,
            &workspace.id,
            WorkspaceStatus::Error,
            Some("provisioning was interrupted by a restart; retry to resume"),
        )?;
        touched += 1;
    }
    Ok(touched)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_studio::models::{AutonomyMode, EgressEnforcement, ExecMode, NewWorkspace};

    /// The saga writes into the real workspace root, which is a global path.
    /// These tests redirect the Data category, so they must not run next to
    /// each other or to anything else reading that category.
    static PATHS_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct Fixture {
        _data: tempfile::TempDir,
        _registry: tempfile::TempDir,
        db: DbPool,
    }

    fn fixture() -> Fixture {
        let data = tempfile::tempdir().expect("data dir");
        crate::paths::set_category_override(
            crate::paths::StorageCategory::Data,
            Some(data.path().to_string_lossy().to_string()),
        );
        let registry = tempfile::tempdir().expect("registry dir");
        let db = crate::db::init(&registry.path().join("tentaflow.db")).expect("init db");
        Fixture {
            _data: data,
            _registry: registry,
            db,
        }
    }

    fn release_paths() {
        crate::paths::set_category_override(crate::paths::StorageCategory::Data, None);
    }

    fn new_workspace(id: &str, repo_kind: &str, repo_url: Option<&str>) -> NewWorkspace {
        NewWorkspace {
            id: id.to_string(),
            org_id: "org-1".into(),
            owner_user_id: "u-1".into(),
            name: "Workspace".into(),
            slug: id.to_string(),
            node_id: "node-1".into(),
            exec_mode: ExecMode::TrustedNative,
            container_image: None,
            egress_enforcement: EgressEnforcement::Unrestricted,
            repo_kind: repo_kind.to_string(),
            repo_url: repo_url.map(str::to_string),
            repo_auth_kind: Some("none".into()),
            secret_ref: None,
            ssh_host_fingerprint: None,
            default_branch: Some("main".into()),
            target_branch: None,
            autonomy_ceiling: AutonomyMode::Normal,
            egress_policy: "org_approved".into(),
            index_enabled: false,
            quota_disk_bytes: None,
            quota_sessions: None,
        }
    }

    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    #[test]
    fn an_empty_workspace_ends_active_with_a_base_commit_to_branch_from() {
        let _guard = PATHS_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        if !git_available() {
            eprintln!("skipping: git is not installed");
            return;
        }
        let fx = fixture();
        let ws = repository::create_workspace(&fx.db, &new_workspace("ws-empty", "empty", None))
            .unwrap();

        let outcome = provision(&fx.db, &ws, &ProvisionAuth::None).unwrap();
        assert_eq!(outcome.default_branch, "main");
        assert_eq!(
            outcome.base_commit.len(),
            40,
            "an empty init would have left no HEAD to branch from"
        );

        let row = repository::get_workspace(&fx.db, "ws-empty")
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "active");
        assert_eq!(row.default_branch.as_deref(), Some("main"));
        assert_eq!(row.target_branch.as_deref(), Some("main"));

        // The runtime database and the layout really exist.
        assert!(paths::workspace_db_path("ws-empty").unwrap().is_file());
        assert!(paths::repo_dir("ws-empty").unwrap().join(".git").exists());
        release_paths();
    }

    #[test]
    fn a_failed_clone_leaves_error_and_no_half_written_repository() {
        let _guard = PATHS_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let fx = fixture();
        // A syntactically valid remote that policy refuses: nothing is fetched.
        let ws = repository::create_workspace(
            &fx.db,
            &new_workspace("ws-bad", "git", Some("https://127.0.0.1/repo.git")),
        )
        .unwrap();

        let err = provision(&fx.db, &ws, &ProvisionAuth::None).unwrap_err();
        assert!(
            format!("{err:#}").contains("forbidden") || format!("{err:#}").contains("metadata")
        );

        let row = repository::get_workspace(&fx.db, "ws-bad")
            .unwrap()
            .unwrap();
        assert_eq!(
            row.status, "error",
            "a broken workspace must not look usable"
        );
        assert!(row.status_detail.is_some(), "no reason was recorded");

        let steps = repository::list_saga_steps(&fx.db, "ws-bad").unwrap();
        let repo_step = steps.iter().find(|s| s.step == STEP_REPOSITORY).unwrap();
        assert_eq!(
            repo_step.status, "compensated",
            "a partial clone must be marked for redo, not left as a failure to inspect"
        );
        assert!(
            !paths::repo_dir("ws-bad").unwrap().exists(),
            "a partial repo/ survived and would block a retry"
        );
        release_paths();
    }

    #[test]
    fn resuming_skips_completed_steps_and_finishes_the_rest() {
        let _guard = PATHS_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        if !git_available() {
            eprintln!("skipping: git is not installed");
            return;
        }
        let fx = fixture();
        let ws = repository::create_workspace(&fx.db, &new_workspace("ws-resume", "empty", None))
            .unwrap();

        // Simulate a crash after the layout: only that step is recorded.
        paths::create_workspace_layout("ws-resume").unwrap();
        repository::record_saga_step(&fx.db, "ws-resume", STEP_LAYOUT, SagaStepStatus::Done, None)
            .unwrap();

        provision(&fx.db, &ws, &ProvisionAuth::None).unwrap();
        let steps = repository::list_saga_steps(&fx.db, "ws-resume").unwrap();
        assert!(steps.iter().all(|s| s.status == "done"), "{steps:?}");
        assert_eq!(steps.len(), 5, "every step must be accounted for");
        release_paths();
    }

    #[test]
    fn provisioning_twice_is_a_no_op_rather_than_a_second_clone() {
        let _guard = PATHS_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        if !git_available() {
            eprintln!("skipping: git is not installed");
            return;
        }
        let fx = fixture();
        let ws = repository::create_workspace(&fx.db, &new_workspace("ws-twice", "empty", None))
            .unwrap();

        let first = provision(&fx.db, &ws, &ProvisionAuth::None).unwrap();
        let reread = repository::get_workspace(&fx.db, "ws-twice")
            .unwrap()
            .unwrap();
        let second = provision(&fx.db, &reread, &ProvisionAuth::None).unwrap();
        assert_eq!(
            first.base_commit, second.base_commit,
            "the repository was rebuilt on a resume"
        );
        release_paths();
    }

    #[test]
    fn a_repository_needing_credentials_without_a_stored_secret_fails_before_the_network() {
        let _guard = PATHS_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let fx = fixture();
        let mut new = new_workspace("ws-nosecret", "git", Some("https://example.invalid/r.git"));
        new.repo_auth_kind = Some("token".into());
        new.secret_ref = None;
        let ws = repository::create_workspace(&fx.db, &new).unwrap();

        let err = provision(&fx.db, &ws, &ProvisionAuth::None).unwrap_err();
        assert!(format!("{err:#}").contains("no secret is stored"));
        let steps = repository::list_saga_steps(&fx.db, "ws-nosecret").unwrap();
        assert!(
            steps.iter().all(|s| s.step != STEP_REPOSITORY),
            "the network step ran despite a missing credential"
        );
        release_paths();
    }

    #[test]
    fn a_workspace_interrupted_by_a_restart_is_reported_as_retryable() {
        let _guard = PATHS_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let fx = fixture();
        repository::create_workspace(&fx.db, &new_workspace("ws-stuck", "empty", None)).unwrap();

        assert_eq!(reconcile_interrupted(&fx.db, "node-1").unwrap(), 1);
        let row = repository::get_workspace(&fx.db, "ws-stuck")
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "error");
        assert!(row.status_detail.unwrap().contains("retry"));

        // Running it again finds nothing left to reconcile.
        assert_eq!(reconcile_interrupted(&fx.db, "node-1").unwrap(), 0);
        release_paths();
    }
}
