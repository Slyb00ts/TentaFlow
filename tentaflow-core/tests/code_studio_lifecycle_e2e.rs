// ===== File: tests/code_studio_lifecycle_e2e.rs — the whole Code Studio cycle, from nothing =====
//
// The acceptance criterion of the feature, stated as one sentence by the person
// who ordered it:
//
//   "from an empty state one can: create a workspace on a chosen node, clone a
//    repository through the broker, open a session, have the agent change code,
//    see the change as a patch set, accept it per hunk, commit, push after an
//    explicit confirmation and merge through the integration worktree — and a
//    Core failure at any moment leaves a resumable state, not a half-dead one."
//
// Everything below runs against the REAL `git` binary and a REAL bare
// repository, and goes through `dispatch::code_studio` — the same path the
// dashboard uses — so a step that only works when called from inside the crate
// fails here.
//
// Two properties of the environment are worth stating up front, because they
// shape the test:
//
// 1. The "remote" is a bare repository in a temporary directory, reached over
//    the `ssh` scheme with a `GIT_SSH_COMMAND` shim that runs `git-upload-pack`
//    / `git-receive-pack` locally. A `file://` remote is IMPOSSIBLE by design:
//    `remote_policy::validate_remote` accepts only `https` and `ssh`, and the
//    broker additionally pins `GIT_ALLOW_PROTOCOL=https:ssh`, so git itself
//    would refuse it. Nothing about the transfer is faked — the git wire
//    protocol really runs and the bare repository really receives the objects.
//
// 2. `code_studio::paths::test_data_dir_guard()` is `#[cfg(test)] pub(crate)`
//    and therefore invisible to an integration test. The guard below is this
//    binary's own; it serialises the tests of THIS file against each other,
//    which is all a separate test process needs, because the category override
//    it protects is process-global rather than machine-global.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};

use tempfile::TempDir;

use tentaflow_core::code_studio::fs as cs_fs;
use tentaflow_core::code_studio::git_broker::{Broker, GitAuth};
use tentaflow_core::code_studio::operations::{
    self, OpKind, OperationInput, OperationRequest, OperationStatus, OriginKind, Postcondition,
    Precondition, SessionProbe,
};
use tentaflow_core::code_studio::patch::{self, PatchScope};
use tentaflow_core::code_studio::pep::Capability;
use tentaflow_core::code_studio::tools;
use tentaflow_core::code_studio::{artifacts, paths, repository, workspace_db};
use tentaflow_core::db::DbPool;
use tentaflow_core::dispatch::code_studio::code_studio_dispatch;
use tentaflow_core::dispatch::{AppState, HandlerContext};
use tentaflow_core::paths::{set_category_override, StorageCategory};
use tentaflow_core::services::rbac::middleware::OrgContext;
use tentaflow_protocol::code_studio::{
    CodeStudioPayload, PatchFileDecision, PatchFileInfo, PatchHunkDecision,
};
use tentaflow_protocol::{MessageBody, ProtocolError, SessionAuth};

const ORG: &str = "org-1";
const USER: &str = "u-owner";
const PERM_READ: &str = "code_studio.read";
const REPO_FILE: &str = "src/lib.rs";

// =============================================================================
// Environment
// =============================================================================

/// Serialises everything in this file: `set_category_override` and the git
/// environment are process-global, so two tests running at once would resolve
/// the workspace root under a temporary directory the other one just dropped.
fn env_guard() -> MutexGuard<'static, ()> {
    static GUARD: Mutex<()> = Mutex::new(());
    GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The ssh transport the broker will use. Written once per test process and
/// exported before any git command runs, so every later invocation — including
/// the ones the broker spawns from a blocking thread — inherits it.
///
/// The shim drops the `[user@]host` argument and executes the command git asked
/// for. That is a LOCAL hop for a REAL git wire conversation: the objects still
/// travel through `git-upload-pack` / `git-receive-pack`.
#[cfg(unix)]
fn ssh_transport() -> Option<&'static Path> {
    static SHIM: OnceLock<Option<(TempDir, PathBuf)>> = OnceLock::new();
    let entry = SHIM.get_or_init(|| {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().ok()?;
        let script = dir.path().join("ssh-shim.sh");
        std::fs::write(&script, "#!/bin/sh\nshift\nexec /bin/sh -c \"$*\"\n").ok()?;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).ok()?;
        std::env::set_var("GIT_SSH_COMMAND", &script);
        Some((dir, script))
    });
    entry.as_ref().map(|(_, script)| script.as_path())
}

#[cfg(not(unix))]
fn ssh_transport() -> Option<&'static Path> {
    None
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Runs `git` outside the broker. Used ONLY to read the truth back: an
/// assertion about what a commit contains must not be answered by the code
/// under test.
fn git(args: &[&str]) -> std::process::Output {
    Command::new("git")
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git is not available")
}

fn git_ok(args: &[&str]) -> String {
    let out = git(args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The file the "agent" will edit: enough unchanged lines between the two
/// edited regions that git renders them as two SEPARATE hunks (default context
/// is three lines on each side).
fn seed_content() -> String {
    (1..=24)
        .map(|n| format!("line {n}\n"))
        .collect::<Vec<_>>()
        .concat()
}

/// The same file after a two-region change. Line 2 and line 23 are far enough
/// apart to stay two hunks.
fn edited_content() -> String {
    seed_content()
        .replace("line 2\n", "line 2 CHANGED BY THE AGENT\n")
        .replace("line 23\n", "line 23 CHANGED BY THE AGENT\n")
}

/// Creates the bare repository that plays the role of the company git server,
/// with one commit on `main`.
fn create_remote(root: &Path) -> PathBuf {
    let bare = root.join("remote.git");
    git_ok(&[
        "init",
        "--bare",
        "--quiet",
        "-b",
        "main",
        &bare.display().to_string(),
    ]);

    let seed = root.join("seed");
    git_ok(&["init", "--quiet", "-b", "main", &seed.display().to_string()]);
    std::fs::create_dir_all(seed.join("src")).expect("seed src dir");
    std::fs::write(seed.join(REPO_FILE), seed_content()).expect("seed file");
    let seed_git = format!("--git-dir={}", seed.join(".git").display());
    let seed_tree = format!("--work-tree={}", seed.display());
    git_ok(&[&seed_git, &seed_tree, "add", "-A"]);
    git_ok(&[
        &seed_git,
        &seed_tree,
        "-c",
        "user.name=Seed",
        "-c",
        "user.email=seed@example.invalid",
        "commit",
        "--quiet",
        "-m",
        "initial",
    ]);
    git_ok(&[
        &seed_git,
        &seed_tree,
        "push",
        "--quiet",
        &bare.display().to_string(),
        "main",
    ]);
    bare
}

/// A remote URL the policy accepts and the shim resolves. The host is a private
/// RFC1918 literal: loopback is refused outright (§11.4) and a LAN address is
/// exactly the case Code Studio's policy is written to allow.
fn remote_url(bare: &Path) -> String {
    format!("ssh://git@10.244.0.7{}", bare.display())
}

// =============================================================================
// Fixture
// =============================================================================

struct Fixture {
    _guard: MutexGuard<'static, ()>,
    _data: TempDir,
    _remote_root: TempDir,
    bare: PathBuf,
    remote_url: String,
    ctx: HandlerContext,
    workspace_id: String,
    session_id: String,
    branch: String,
}

impl Fixture {
    fn pool(&self) -> DbPool {
        workspace_db::open(&self.workspace_id).expect("workspace runtime db")
    }

    fn repo_git_dir(&self) -> String {
        format!(
            "--git-dir={}",
            paths::repo_dir(&self.workspace_id)
                .expect("repo dir")
                .join(".git")
                .display()
        )
    }

    fn bare_git_dir(&self) -> String {
        format!("--git-dir={}", self.bare.display())
    }

    fn worktree(&self) -> PathBuf {
        paths::session_worktree_dir(&self.workspace_id, &self.session_id).expect("worktree path")
    }

    /// A request from the same connection carrying its own correlation id.
    ///
    /// The journal derives an effect's identity from the origin of the request
    /// that caused it (§13.1), so two turns sharing one correlation id are ONE
    /// operation as far as the journal is concerned. Every real client sends a
    /// fresh correlation id per request, and a multi-turn test that did not
    /// would be exercising a client that does not exist.
    fn turn(&self, correlation_id: u64) -> HandlerContext {
        HandlerContext {
            correlation_id,
            ..self.ctx.clone()
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // The override is process-global; leaving it pointing at a directory
        // this fixture is about to delete would break whatever runs next.
        set_category_override(StorageCategory::Data, None);
    }
}

fn org() -> OrgContext {
    OrgContext {
        user_id: USER.to_string(),
        org_id: ORG.to_string(),
        role_id: "role-1".to_string(),
        permissions: [PERM_READ.to_string()].into_iter().collect(),
    }
}

fn context() -> HandlerContext {
    HandlerContext {
        session: SessionAuth::UserSession {
            user_id: [9u8; 16],
            role: None,
        },
        correlation_id: 1,
        connection_id: 0,
        resume_secret: None,
        state: AppState::for_test(),
        org_context: Some(org()),
    }
}

async fn call(
    ctx: &HandlerContext,
    payload: CodeStudioPayload,
) -> Result<CodeStudioPayload, ProtocolError> {
    match code_studio_dispatch(&MessageBody::CodeStudioBody(payload), ctx).await {
        Ok(MessageBody::CodeStudioBody(response)) => Ok(response),
        Ok(other) => panic!("code studio answered with {other:?}"),
        Err(error) => Err(error),
    }
}

async fn ok(ctx: &HandlerContext, payload: CodeStudioPayload) -> CodeStudioPayload {
    let described = format!("{payload:?}");
    call(ctx, payload)
        .await
        .unwrap_or_else(|error| panic!("{described} was refused: {:?} / {}", error.code, error.message))
}

/// The pending approval of a capability, as the UI would find it.
async fn pending_approval(fx: &Fixture, capability: &str) -> String {
    let response = ok(
        &fx.ctx,
        CodeStudioPayload::ApprovalsListRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            status: "pending".into(),
        },
    )
    .await;
    let CodeStudioPayload::ApprovalsListResponse { approvals, .. } = response else {
        panic!("expected ApprovalsListResponse");
    };
    approvals
        .into_iter()
        .find(|a| a.capability == capability)
        .unwrap_or_else(|| panic!("no pending approval for {capability}"))
        .approval_id
}

/// Answers a pending question the way an operator would, one shot only.
async fn allow_once(fx: &Fixture, capability: &str) {
    let approval_id = pending_approval(fx, capability).await;
    ok(
        &fx.ctx,
        CodeStudioPayload::ApprovalDecideRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            approval_id,
            decision: "allow_once".into(),
        },
    )
    .await;
}

/// Builds a workspace cloned from a real bare repository and opens a session in
/// it — through the handlers, exactly as the dashboard does.
async fn fixture() -> Option<Fixture> {
    let guard = env_guard();
    if !git_available() {
        eprintln!("skipping: git is not installed");
        return None;
    }
    let Some(_shim) = ssh_transport() else {
        eprintln!("skipping: the ssh transport shim needs a unix shell");
        return None;
    };

    let remote_root = TempDir::new().expect("remote root");
    let bare = create_remote(remote_root.path());
    let url = remote_url(&bare);

    let data = TempDir::new().expect("data dir");
    set_category_override(
        StorageCategory::Data,
        Some(data.path().to_string_lossy().to_string()),
    );

    let ctx = context();
    repository::grant_creator(&ctx.state.db, ORG, USER, USER).expect("creator grant");

    let response = ok(
        &ctx,
        CodeStudioPayload::WorkspaceCreateRequest {
            name: "lifecycle".into(),
            node_id: ctx.state.local_node_id.to_string(),
            exec_mode: "trusted_native".into(),
            container_image: None,
            repo_kind: "git".into(),
            repo_url: Some(url.clone()),
            repo_auth_kind: None,
            secret_material: None,
            ssh_host_fingerprint: None,
            default_branch: Some("main".into()),
            // `auto_edit` is the ceiling a trusted_native workspace may hold,
            // and it is what makes `fs_write` run without a prompt — the agent
            // edits, the human reviews (§9.5).
            autonomy_ceiling: "auto_edit".into(),
            egress_policy: "org_approved".into(),
            index_enabled: false,
            members: Vec::new(),
        },
    )
    .await;
    let CodeStudioPayload::WorkspaceCreateResponse { workspace_id, .. } = response else {
        panic!("expected WorkspaceCreateResponse");
    };

    // Provisioning is a saga on a blocking thread; the UI follows it with
    // `WorkspaceGetRequest` and so does this test.
    let mut status = String::new();
    let mut detail = None;
    for _ in 0..200 {
        let response = ok(
            &ctx,
            CodeStudioPayload::WorkspaceGetRequest {
                workspace_id: workspace_id.clone(),
            },
        )
        .await;
        let CodeStudioPayload::WorkspaceGetResponse { workspace, .. } = response else {
            panic!("expected WorkspaceGetResponse");
        };
        status = workspace.status.clone();
        detail = workspace.status_detail.clone();
        if status != "provisioning" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(
        status, "active",
        "the clone through the broker did not finish: {detail:?}"
    );

    let response = ok(
        &ctx,
        CodeStudioPayload::SessionOpenRequest {
            workspace_id: workspace_id.clone(),
            title: "lifecycle".into(),
            autonomy_mode: "auto_edit".into(),
        },
    )
    .await;
    let CodeStudioPayload::SessionOpenResponse { session } = response else {
        panic!("expected SessionOpenResponse");
    };

    Some(Fixture {
        _guard: guard,
        _data: data,
        _remote_root: remote_root,
        bare,
        remote_url: url,
        ctx,
        workspace_id,
        session_id: session.session_id,
        branch: session.branch,
    })
}

// =============================================================================
// 1. The whole cycle
// =============================================================================

#[tokio::test]
async fn the_whole_cycle_runs_from_an_empty_state_to_a_merged_target_branch() {
    let Some(fx) = fixture().await else { return };

    // ---- the clone is a real clone of the real bare repository -------------
    let remote_head = git_ok(&[&fx.bare_git_dir(), "rev-parse", "refs/heads/main"]);
    let local_head = git_ok(&[&fx.repo_git_dir(), "rev-parse", "refs/heads/main"]);
    assert_eq!(
        local_head, remote_head,
        "the workspace repository is not the remote's history"
    );
    assert_eq!(
        git_ok(&[&fx.repo_git_dir(), "show", &format!("main:{REPO_FILE}")]),
        seed_content().trim_end(),
        "the clone does not carry the remote's content"
    );

    // ---- the session has its own worktree on its own branch ---------------
    assert!(
        fx.worktree().join(REPO_FILE).is_file(),
        "the session worktree was not checked out"
    );
    assert!(
        fx.branch.starts_with("cs/"),
        "unexpected session branch '{}'",
        fx.branch
    );

    // ---- the agent changes code -------------------------------------------
    // Two edits, far enough apart that the review sees two hunks.
    ok(
        &fx.ctx,
        CodeStudioPayload::FileWriteRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            path: REPO_FILE.into(),
            content: edited_content(),
            expected_blob_sha: Some(cs_fs::blob_sha(seed_content().as_bytes())),
        },
    )
    .await;

    // ---- the change appears as a patch set --------------------------------
    let response = ok(
        &fx.ctx,
        CodeStudioPayload::GitCommitRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            message: "the agent's change".into(),
            patch_set_id: None,
        },
    )
    .await;
    let CodeStudioPayload::GitCommitResponse {
        status,
        patch_set_id,
        ..
    } = response
    else {
        panic!("expected GitCommitResponse");
    };
    assert_eq!(
        status, "review_required",
        "a commit without an accepted review must open the review, not commit"
    );
    let patch_set_id = patch_set_id.expect("the review names the set it decides");

    let response = ok(
        &fx.ctx,
        CodeStudioPayload::PatchSetGetRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            patch_set_id: patch_set_id.clone(),
        },
    )
    .await;
    let CodeStudioPayload::PatchSetGetResponse { files, .. } = response else {
        panic!("expected PatchSetGetResponse");
    };
    assert_eq!(files.len(), 1, "the review shows {files:?}");
    let file = &files[0];
    assert_eq!(file.path, REPO_FILE);
    assert!(
        file.hunks.len() >= 2,
        "the change has to be reviewable per hunk, but the set has {} hunk(s)",
        file.hunks.len()
    );
    // The header is metadata and travels in its own field. A body that repeated
    // it forced every renderer to recognise row 0 and drop it — and to keep it
    // out of the line numbering, which is where a diff view breaks silently.
    for hunk in &file.hunks {
        assert!(
            hunk.header.starts_with("@@"),
            "the hunk header is not a header: {}",
            hunk.header
        );
        assert!(
            !hunk.content.contains("@@"),
            "the hunk body repeats its own header:\n{}",
            hunk.content
        );
        assert!(
            hunk.content
                .starts_with([' ', '+', '-', '\\'].as_slice()),
            "the first body row is not a diff row:\n{}",
            hunk.content
        );
    }

    // ---- accept ONE hunk, reject the other --------------------------------
    let accepted = file
        .hunks
        .iter()
        .find(|hunk| hunk.content.contains("line 2 CHANGED"))
        .expect("the first edit has its own hunk");
    let rejected = file
        .hunks
        .iter()
        .find(|hunk| hunk.content.contains("line 23 CHANGED"))
        .expect("the second edit has its own hunk");
    assert_ne!(
        accepted.patch_hunk_id, rejected.patch_hunk_id,
        "both edits landed in one hunk; the fixture no longer tests per-hunk acceptance"
    );

    let response = ok(
        &fx.ctx,
        CodeStudioPayload::PatchDecideRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            patch_set_id: patch_set_id.clone(),
            files: vec![PatchFileDecision {
                patch_file_id: file.patch_file_id.clone(),
                decision: "accept".into(),
                note: None,
                hunks: vec![
                    PatchHunkDecision {
                        patch_hunk_id: accepted.patch_hunk_id.clone(),
                        decision: "accept".into(),
                    },
                    PatchHunkDecision {
                        patch_hunk_id: rejected.patch_hunk_id.clone(),
                        decision: "reject".into(),
                    },
                ],
            }],
        },
    )
    .await;
    let CodeStudioPayload::PatchDecideResponse {
        status,
        conflicted_paths,
        ..
    } = response
    else {
        panic!("expected PatchDecideResponse");
    };
    assert!(
        conflicted_paths.is_empty(),
        "the partial acceptance did not compose: {conflicted_paths:?}"
    );
    assert_eq!(
        status, "partially_accepted",
        "a set with one accepted and one rejected hunk is partially accepted"
    );

    // The invariant under test: the commit is built from the ACCEPTED BLOBS,
    // not from the worktree. So the worktree is poisoned behind the handlers'
    // back — exactly what an agent that kept typing would do — and the commit
    // must be unaffected by it.
    let poison = "POISON FROM THE WORKTREE\n";
    std::fs::write(fx.worktree().join(REPO_FILE), poison).expect("poison the worktree");

    // ---- commit ------------------------------------------------------------
    let refused = call(
        &fx.ctx,
        CodeStudioPayload::GitCommitRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            message: "the accepted hunk".into(),
            patch_set_id: Some(patch_set_id.clone()),
        },
    )
    .await;
    assert!(
        refused.is_err(),
        "a commit in 'auto_edit' still asks before it publishes"
    );
    allow_once(&fx, "git_commit").await;

    let response = ok(
        &fx.ctx,
        CodeStudioPayload::GitCommitRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            message: "the accepted hunk".into(),
            patch_set_id: Some(patch_set_id.clone()),
        },
    )
    .await;
    let CodeStudioPayload::GitCommitResponse {
        status, commit_oid, ..
    } = response
    else {
        panic!("expected GitCommitResponse");
    };
    assert_eq!(status, "committed", "the commit did not happen");
    let commit_oid = commit_oid.expect("a commit reports its object id");

    // Read the COMMIT'S OWN TREE with git, not our database.
    let committed = git_ok(&[
        &fx.repo_git_dir(),
        "show",
        &format!("{commit_oid}:{REPO_FILE}"),
    ]);
    assert!(
        committed.contains("line 2 CHANGED BY THE AGENT"),
        "the accepted hunk is missing from the commit:\n{committed}"
    );
    assert!(
        !committed.contains("line 23 CHANGED BY THE AGENT"),
        "the REJECTED hunk was committed:\n{committed}"
    );
    assert!(
        !committed.contains("POISON"),
        "the commit was built from the worktree instead of the accepted blobs:\n{committed}"
    );
    assert_eq!(
        git_ok(&[
            &fx.repo_git_dir(),
            "rev-parse",
            &format!("refs/heads/{}", fx.branch)
        ]),
        commit_oid,
        "the session branch does not point at the commit"
    );

    // ---- push needs an explicit confirmation -------------------------------
    let branch_ref = format!("refs/heads/{}", fx.branch);
    let refused = call(
        &fx.ctx,
        CodeStudioPayload::GitPushRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            remote: String::new(),
            set_upstream: false,
        },
    )
    .await
    .expect_err("a push must ask every time (§9.3 rule 5)");
    assert!(
        refused.message.starts_with("approval_required:"),
        "a suspended push answers like every other gated operation: {}",
        refused.message
    );
    assert!(
        !git(&[&fx.bare_git_dir(), "rev-parse", "--verify", &branch_ref])
            .status
            .success(),
        "the branch reached the remote WITHOUT a confirmation"
    );

    allow_once(&fx, "git_push").await;
    let response = ok(
        &fx.ctx,
        CodeStudioPayload::GitPushRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            remote: String::new(),
            set_upstream: false,
        },
    )
    .await;
    let CodeStudioPayload::GitPushResponse { status, error, .. } = response else {
        panic!("expected GitPushResponse");
    };
    assert_eq!(status, "pushed", "the confirmed push failed: {error:?}");
    assert_eq!(
        git_ok(&[&fx.bare_git_dir(), "rev-parse", &branch_ref]),
        commit_oid,
        "the bare repository did not receive the commit"
    );

    // ---- merge through the INTEGRATION worktree ----------------------------
    let main_before = git_ok(&[&fx.repo_git_dir(), "rev-parse", "refs/heads/main"]);
    let refused = call(
        &fx.ctx,
        CodeStudioPayload::GitMergeRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            source_branch: fx.branch.clone(),
            target_branch: "main".into(),
        },
    )
    .await;
    assert!(refused.is_err(), "a merge must ask before it runs");
    allow_once(&fx, "git_merge").await;

    let response = ok(
        &fx.ctx,
        CodeStudioPayload::GitMergeRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            source_branch: fx.branch.clone(),
            target_branch: "main".into(),
        },
    )
    .await;
    let CodeStudioPayload::GitMergeResponse {
        op_id,
        outcome,
        conflict_files,
        patch_set_id: merge_set,
        integration_worktree,
        steps,
        ..
    } = response
    else {
        panic!("expected GitMergeResponse");
    };
    assert_eq!(outcome, "clean", "the merge conflicted on {conflict_files:?}");
    let merge_set = merge_set.expect("a clean merge opens its review");
    // ONE list describes the whole saga, merge half and publish half alike.
    assert_eq!(
        steps.iter().map(|s| s.step.as_str()).collect::<Vec<_>>(),
        vec![
            "integration_worktree",
            "private_ref",
            "merge",
            "patch_set",
            "tests",
            "review",
            "approval",
            "update_ref",
        ],
    );
    // The merge does not run tests (§11.6 pkt 4), and the answer says so with a
    // reason code instead of borrowing another step's outcome.
    let tests = steps.iter().find(|s| s.step == "tests").expect("tests step");
    assert_eq!(tests.status, "pending");
    assert_eq!(tests.detail.as_deref(), Some("tests_not_run_by_merge"));
    for step in &steps {
        if step.status == "done" {
            assert!(
                step.detail.is_none(),
                "a finished step owes no reason: {step:?}"
            );
        }
    }
    let integration = integration_worktree.expect("a merge records its integration worktree");
    assert_eq!(integration.purpose, "integration");

    let integration_dir =
        paths::integration_worktree_dir(&fx.workspace_id, &fx.session_id).expect("integration path");
    assert!(
        integration_dir.is_dir(),
        "the merge did not run in an integration worktree"
    );
    assert_ne!(
        integration_dir,
        fx.worktree(),
        "the merge ran in the session's own worktree"
    );
    assert_eq!(
        git_ok(&[&fx.repo_git_dir(), "rev-parse", "refs/heads/main"]),
        main_before,
        "the target branch moved before anybody reviewed the merge"
    );

    // The merge review is a decision about the TARGET branch and is accepted
    // whole; the per-hunk decision already happened on the session branch.
    let response = ok(
        &fx.ctx,
        CodeStudioPayload::PatchSetGetRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            patch_set_id: merge_set.clone(),
        },
    )
    .await;
    let CodeStudioPayload::PatchSetGetResponse { patch_set, files } = response else {
        panic!("expected PatchSetGetResponse");
    };
    // The set says WHICH tree it decides and WHICH merge it belongs to: a
    // reader that had to pick the newest set of scope 'merge' would decide the
    // wrong one the moment a session holds two.
    assert_eq!(patch_set.scope, "merge");
    assert_eq!(patch_set.op_id.as_deref(), Some(op_id.as_str()));
    let decisions: Vec<PatchFileDecision> = files
        .iter()
        .map(|file| PatchFileDecision {
            patch_file_id: file.patch_file_id.clone(),
            decision: "accept".into(),
            note: None,
            hunks: Vec::new(),
        })
        .collect();
    assert!(!decisions.is_empty(), "the merge review has nothing in it");
    ok(
        &fx.ctx,
        CodeStudioPayload::PatchDecideRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            patch_set_id: merge_set.clone(),
            files: decisions,
        },
    )
    .await;

    let refused = call(
        &fx.ctx,
        CodeStudioPayload::GitMergeFinalizeRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            op_id: op_id.clone(),
            patch_set_id: merge_set.clone(),
        },
    )
    .await
    .expect_err("publishing a merge must ask before it moves the target branch");
    assert!(
        refused.message.starts_with("approval_required:"),
        "a suspended finalize answers like every other gated operation: {}",
        refused.message
    );
    allow_once(&fx, "git_merge_finalize").await;

    let response = ok(
        &fx.ctx,
        CodeStudioPayload::GitMergeFinalizeRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            op_id: op_id.clone(),
            patch_set_id: merge_set,
        },
    )
    .await;
    let CodeStudioPayload::GitMergeFinalizeResponse {
        status,
        merge_commit_oid,
        error,
        steps,
        ..
    } = response
    else {
        panic!("expected GitMergeFinalizeResponse");
    };
    assert_eq!(status, "merged", "the merge was not published: {error:?}");
    // The publish half of the SAME list, now decided.
    let publish: Vec<(&str, &str)> = steps
        .iter()
        .filter(|s| matches!(s.step.as_str(), "review" | "approval" | "update_ref"))
        .map(|s| (s.step.as_str(), s.status.as_str()))
        .collect();
    assert_eq!(
        publish,
        vec![("review", "done"), ("approval", "done"), ("update_ref", "done")],
    );
    let merge_commit = merge_commit_oid.expect("a published merge names its commit");

    // ---- the target branch really carries the reviewed content -------------
    assert_eq!(
        git_ok(&[&fx.repo_git_dir(), "rev-parse", "refs/heads/main"]),
        merge_commit,
        "the target branch does not point at the merge commit"
    );
    let parents = git_ok(&[
        &fx.repo_git_dir(),
        "rev-list",
        "--parents",
        "-n",
        "1",
        &merge_commit,
    ]);
    let parents: Vec<&str> = parents.split_whitespace().collect();
    assert_eq!(
        parents.len(),
        3,
        "a merge commit has two parents, got {parents:?}"
    );
    assert!(parents.contains(&main_before.as_str()));
    assert!(parents.contains(&commit_oid.as_str()));

    let merged = git_ok(&[
        &fx.repo_git_dir(),
        "show",
        &format!("{merge_commit}:{REPO_FILE}"),
    ]);
    assert!(merged.contains("line 2 CHANGED BY THE AGENT"));
    assert!(
        !merged.contains("line 23 CHANGED BY THE AGENT"),
        "the rejected hunk reached the target branch:\n{merged}"
    );
    assert!(
        !merged.contains("POISON"),
        "the merge published the worktree instead of the accepted blobs:\n{merged}"
    );
    assert!(
        !integration_dir.exists(),
        "the integration worktree survived a finalised merge"
    );

    // ---- the session closes cleanly ---------------------------------------
    ok(
        &fx.ctx,
        CodeStudioPayload::SessionCloseRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
        },
    )
    .await;
}

// =============================================================================
// 1b. A session that commits more than once
// =============================================================================
//
// One turn is not the interesting case. A session commits, keeps working and
// commits again, and everything the second turn does is measured against what
// the first one published — so these two tests take the branch apart with real
// git afterwards and ask history, not the database, whether that happened.

/// Writes one file through the handlers, the way the agent's `fs_write` does.
async fn write_file(
    fx: &Fixture,
    ctx: &HandlerContext,
    path: &str,
    content: &str,
    expect_sha: Option<String>,
) {
    ok(
        ctx,
        CodeStudioPayload::FileWriteRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            path: path.into(),
            content: content.into(),
            expected_blob_sha: expect_sha,
        },
    )
    .await;
}

/// Asks for a commit without an accepted review, which is what opens the review
/// (gate 5a), and returns the set it opened with its files.
async fn open_review(
    fx: &Fixture,
    ctx: &HandlerContext,
    message: &str,
) -> (String, Vec<PatchFileInfo>) {
    let response = ok(
        ctx,
        CodeStudioPayload::GitCommitRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            message: message.into(),
            patch_set_id: None,
        },
    )
    .await;
    let CodeStudioPayload::GitCommitResponse {
        status,
        patch_set_id,
        ..
    } = response
    else {
        panic!("expected GitCommitResponse");
    };
    assert_eq!(status, "review_required", "the commit skipped its review");
    let patch_set_id = patch_set_id.expect("the review names the set it decides");

    let response = ok(
        ctx,
        CodeStudioPayload::PatchSetGetRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            patch_set_id: patch_set_id.clone(),
        },
    )
    .await;
    let CodeStudioPayload::PatchSetGetResponse { files, .. } = response else {
        panic!("expected PatchSetGetResponse");
    };
    (patch_set_id, files)
}

/// Decides a whole set per file and publishes it.
async fn decide_and_commit(
    fx: &Fixture,
    ctx: &HandlerContext,
    patch_set_id: &str,
    files: &[PatchFileInfo],
    accepted: &[&str],
    message: &str,
) -> String {
    ok(
        ctx,
        CodeStudioPayload::PatchDecideRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            patch_set_id: patch_set_id.to_string(),
            files: files
                .iter()
                .map(|file| PatchFileDecision {
                    patch_file_id: file.patch_file_id.clone(),
                    decision: if accepted.contains(&file.path.as_str()) {
                        "accept".into()
                    } else {
                        "reject".into()
                    },
                    note: None,
                    hunks: Vec::new(),
                })
                .collect(),
        },
    )
    .await;

    allow_once(fx, "git_commit").await;
    let response = ok(
        ctx,
        CodeStudioPayload::GitCommitRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            message: message.into(),
            patch_set_id: Some(patch_set_id.to_string()),
        },
    )
    .await;
    let CodeStudioPayload::GitCommitResponse {
        status, commit_oid, ..
    } = response
    else {
        panic!("expected GitCommitResponse");
    };
    assert_eq!(status, "committed", "the commit did not happen");
    commit_oid.expect("a commit reports its object id")
}

/// D1. Two turns, two commits. The second one has to sit ON the first: a commit
/// parented on the session's original base publishes a branch that has lost
/// every commit in between, and the compare-and-swap reports success while it
/// happens. The second turn's review must likewise describe only the second
/// turn, because its base is the first turn's commit.
#[tokio::test]
async fn a_second_commit_builds_on_the_first_instead_of_re_parenting_the_session_base() {
    let Some(fx) = fixture().await else { return };
    let branch_ref = format!("refs/heads/{}", fx.branch);
    let base = git_ok(&[&fx.repo_git_dir(), "rev-parse", &branch_ref]);

    // ---- turn one ----------------------------------------------------------
    let first_turn = fx.turn(11);
    write_file(
        &fx,
        &first_turn,
        REPO_FILE,
        &edited_content(),
        Some(cs_fs::blob_sha(seed_content().as_bytes())),
    )
    .await;
    let (first_set, first_files) = open_review(&fx, &first_turn, "turn one").await;
    assert_eq!(
        first_files.iter().map(|f| f.path.clone()).collect::<Vec<_>>(),
        vec![REPO_FILE.to_string()],
        "the first review does not describe the first turn"
    );
    let first = decide_and_commit(
        &fx,
        &first_turn,
        &first_set,
        &first_files,
        &[REPO_FILE],
        "turn one",
    )
    .await;

    // ---- turn two ----------------------------------------------------------
    let second_turn = fx.turn(22);
    write_file(
        &fx,
        &second_turn,
        "docs/second.md",
        "written in the second turn\n",
        None,
    )
    .await;
    let (second_set, second_files) = open_review(&fx, &second_turn, "turn two").await;
    assert_eq!(
        second_files.iter().map(|f| f.path.clone()).collect::<Vec<_>>(),
        vec!["docs/second.md".to_string()],
        "the second review re-proposes the first turn, so its base is stale"
    );
    let second = decide_and_commit(
        &fx,
        &second_turn,
        &second_set,
        &second_files,
        &["docs/second.md"],
        "turn two",
    )
    .await;

    // ---- what the repository actually holds --------------------------------
    assert_eq!(
        git_ok(&[&fx.repo_git_dir(), "rev-parse", &format!("{second}^")]),
        first,
        "the second commit is not a child of the first"
    );
    for (name, commit) in [("the first", &first), ("the second", &second)] {
        assert!(
            git(&[
                &fx.repo_git_dir(),
                "merge-base",
                "--is-ancestor",
                commit,
                &branch_ref,
            ])
            .status
            .success(),
            "{name} commit is not reachable from the session branch"
        );
    }
    assert_eq!(
        git_ok(&[&fx.repo_git_dir(), "rev-list", "--count", &branch_ref]),
        git_ok(&[&fx.repo_git_dir(), "rev-list", "--count", &base])
            .parse::<u32>()
            .map(|n| (n + 2).to_string())
            .expect("a commit count"),
        "the branch does not carry both turns"
    );
    // Both changes are in the tree the branch points at, which is the whole
    // point of a history: nothing published earlier was dropped on the way.
    let head = git_ok(&[&fx.repo_git_dir(), "show", &format!("{second}:{REPO_FILE}")]);
    assert!(
        head.contains("line 2 CHANGED BY THE AGENT"),
        "the second commit lost the first turn's content:\n{head}"
    );
    assert_eq!(
        git_ok(&[&fx.repo_git_dir(), "show", &format!("{second}:docs/second.md")]),
        "written in the second turn",
    );
}

/// D1, the reviewer's half. A file turned down in one turn must not be back in
/// the next one's review: the worktree is what the next patch set is measured
/// against, so a rejected change left on disk is re-proposed after every commit
/// until somebody accepts it by accident.
#[tokio::test]
async fn a_rejected_file_is_gone_from_the_worktree_and_from_the_next_review() {
    let Some(fx) = fixture().await else { return };

    let first_turn = fx.turn(11);
    write_file(
        &fx,
        &first_turn,
        REPO_FILE,
        &edited_content(),
        Some(cs_fs::blob_sha(seed_content().as_bytes())),
    )
    .await;
    write_file(
        &fx,
        &first_turn,
        "scratch/unwanted.txt",
        "nobody asked for this\n",
        None,
    )
    .await;

    let (set, files) = open_review(&fx, &first_turn, "turn one").await;
    assert_eq!(files.len(), 2, "the review shows {files:?}");
    let commit = decide_and_commit(&fx, &first_turn, &set, &files, &[REPO_FILE], "turn one").await;

    assert!(
        !fx.worktree().join("scratch/unwanted.txt").exists(),
        "the rejected file is still in the worktree, so the next set will propose it again"
    );
    assert!(
        !git(&[
            &fx.repo_git_dir(),
            "cat-file",
            "-e",
            &format!("{commit}:scratch/unwanted.txt"),
        ])
        .status
        .success(),
        "the rejected file was committed"
    );

    // The next turn touches something else entirely.
    let second_turn = fx.turn(22);
    write_file(&fx, &second_turn, "docs/second.md", "the second turn\n", None).await;
    let (_, next) = open_review(&fx, &second_turn, "turn two").await;
    assert_eq!(
        next.iter().map(|f| f.path.clone()).collect::<Vec<_>>(),
        vec!["docs/second.md".to_string()],
        "the rejected file came back in the next review"
    );
}

// =============================================================================
// 2. A Core failure leaves a resumable state
// =============================================================================
//
// A crash cannot be produced in-process, so the state a `kill -9` leaves is
// reproduced exactly: the journal row is opened with the SAME shape the handler
// opens it with, the real effect is performed, and the row is never closed.
// Reconciliation is then asked what it makes of it — which is the thing under
// test, not `begin`.

/// The journal entry `dispatch::code_studio::file_write_v1` opens for a new
/// file, left `pending` with the write already on disk.
fn interrupted_file_write(fx: &Fixture, pool: &DbPool, path: &str, bytes: &[u8]) -> String {
    let stored = artifacts::put(pool, &fx.workspace_id, bytes, "file_content").expect("input blob");
    let operation = operations::begin(
        pool,
        &OperationRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            run_id: None,
            origin_kind: OriginKind::Ui,
            origin_id: "crash-probe".into(),
            logical_step: format!("fs_write:{path}"),
            op_kind: OpKind::FsWrite,
            capability: Capability::FsWrite,
            input: OperationInput::FileContent {
                path: path.to_string(),
                content_sha256: stored.sha256,
                size_bytes: bytes.len() as u64,
            },
            precondition: Precondition::FileAbsent {
                path: path.to_string(),
            },
            postcondition: Postcondition::FileBlobIs {
                path: path.to_string(),
                sha256: cs_fs::blob_sha(bytes),
            },
            profile: None,
        },
    )
    .expect("open the operation");

    // The effect really lands, and THEN the process dies.
    std::fs::write(fx.worktree().join(path), bytes).expect("write the file");
    operation.op_id
}

/// The journal entry `dispatch::code_studio::git_push_v1` opens, left `pending`
/// with the push already delivered to the bare repository.
fn interrupted_push(fx: &Fixture, pool: &DbPool) -> String {
    let broker = Broker::for_workspace(&fx.workspace_id).expect("broker");
    let refname = format!("refs/heads/{}", fx.branch);
    let tip = broker
        .read_ref(&broker.reference(), &refname)
        .expect("read ref")
        .expect("the session branch exists");

    let operation = operations::begin(
        pool,
        &OperationRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            run_id: None,
            origin_kind: OriginKind::Ui,
            origin_id: "crash-probe".into(),
            logical_step: format!("git_push:{}", fx.branch),
            op_kind: OpKind::GitPush,
            capability: Capability::GitPush,
            input: OperationInput::Git {
                operation: "push".into(),
                refname: Some(refname.clone()),
                remote: Some(fx.remote_url.clone()),
                oids: vec![tip.clone()],
            },
            precondition: Precondition::RefEquals {
                refname,
                oid: tip.clone(),
            },
            // A remote reference is not verifiable from this node — which is
            // exactly why a push is journalled without a postcondition.
            postcondition: Postcondition::None,
            profile: None,
        },
    )
    .expect("open the operation");

    let handle = broker.session(&fx.session_id).expect("session handle");
    broker
        .push_branch(&handle, &fx.remote_url, &fx.branch, &GitAuth::None)
        .expect("the push itself must succeed");
    operation.op_id
}

/// §13.1: an interrupted `fs_write` is fully described by the content it was
/// going to leave behind, so a restart must be able to PROVE it happened and
/// close the row — never hand it to a person.
#[tokio::test]
async fn an_interrupted_file_write_is_settled_by_reconciliation() {
    let Some(fx) = fixture().await else { return };
    let pool = fx.pool();

    let op_id = interrupted_file_write(&fx, &pool, "crash.txt", b"written before the crash\n");
    assert_eq!(
        operations::get(&pool, &op_id).unwrap().unwrap().status,
        OperationStatus::Pending,
        "the crash did not leave an open operation"
    );

    let probe = SessionProbe::for_session(&fx.workspace_id, &fx.session_id).expect("probe");
    let report = operations::reconcile(&pool, &fx.session_id, &probe).expect("reconcile");

    let settled = operations::get(&pool, &op_id).unwrap().unwrap();
    assert_eq!(
        settled.status,
        OperationStatus::Completed,
        "an fs_write whose content is on disk was reconciled to '{}' \
         (completed={}, retryable={}, unknown={}). Its postcondition carries a git blob SHA-1 \
         (`code_studio::fs::blob_sha`, 40 hex), while `WorktreeProbe::blob_is` compares a plain \
         SHA-256 of the file bytes — the two can never be equal, so every interrupted write is \
         handed to a human",
        settled.status.slug(),
        report.completed(),
        report.retryable(),
        report.unknown()
    );
}

/// A push cannot be verified from this node, so §13.1 sends it to a PERSON —
/// and the person's decision is the only way out. That is the resumable state:
/// an explicit question, not a row nobody will ever look at again.
#[tokio::test]
async fn an_interrupted_push_becomes_a_question_a_person_can_close() {
    let Some(fx) = fixture().await else { return };
    let pool = fx.pool();

    let op_id = interrupted_push(&fx, &pool);
    assert!(
        git(&[
            &fx.bare_git_dir(),
            "rev-parse",
            "--verify",
            &format!("refs/heads/{}", fx.branch)
        ])
        .status
        .success(),
        "the push under test did not reach the bare repository"
    );

    let probe = SessionProbe::for_session(&fx.workspace_id, &fx.session_id).expect("probe");
    operations::reconcile(&pool, &fx.session_id, &probe).expect("reconcile");

    let settled = operations::get(&pool, &op_id).unwrap().unwrap();
    assert_eq!(
        settled.status,
        OperationStatus::Unknown,
        "a non-idempotent operation with no verifiable outcome must end 'unknown', not '{}'",
        settled.status.slug()
    );

    // The dashboard closes it through the protocol, and that is the ONLY
    // transition out of `unknown`.
    let response = ok(
        &fx.ctx,
        CodeStudioPayload::OperationResolveRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            op_id: op_id.clone(),
            resolution: "completed".into(),
            note: "the branch is on the remote".into(),
        },
    )
    .await;
    let CodeStudioPayload::OperationResolveResponse { status, .. } = response else {
        panic!("expected OperationResolveResponse");
    };
    assert_eq!(status, "completed");
    assert_eq!(
        operations::get(&pool, &op_id).unwrap().unwrap().status,
        OperationStatus::Completed,
        "the human decision did not close the operation"
    );

    // And there is no second way out: the same call must now refuse.
    assert!(
        call(
            &fx.ctx,
            CodeStudioPayload::OperationResolveRequest {
                workspace_id: fx.workspace_id.clone(),
                session_id: fx.session_id.clone(),
                op_id,
                resolution: "failed".into(),
                note: String::new(),
            },
        )
        .await
        .is_err(),
        "a settled operation can be re-decided"
    );
}

/// The promise is about a RESTART, not about a function nobody calls.
///
/// `api::dashboard::server::reconcile_code_studio` is the one pass a booting
/// Core runs before it serves anything. Whatever it leaves `pending` stays
/// `pending` for the life of the node, because there is no other sweep.
#[tokio::test]
async fn the_boot_recovery_pass_settles_the_operation_journal() {
    let Some(fx) = fixture().await else { return };
    let pool = fx.pool();

    let write_op = interrupted_file_write(&fx, &pool, "boot.txt", b"written before the crash\n");
    let push_op = interrupted_push(&fx, &pool);

    // The restart.
    let node_id = fx.ctx.state.local_node_id.to_string();
    let db = fx.ctx.state.db.clone();
    tokio::task::spawn_blocking(move || {
        tentaflow_core::api::dashboard::server::reconcile_code_studio(&db, &node_id)
    })
    .await
    .expect("recovery pass");

    let still_open: Vec<String> = [&write_op, &push_op]
        .into_iter()
        .filter(|op_id| {
            operations::get(&pool, op_id)
                .unwrap()
                .unwrap()
                .status
                == OperationStatus::Pending
        })
        .cloned()
        .collect();
    assert!(
        still_open.is_empty(),
        "the boot recovery pass left {} operation(s) open: {still_open:?}. \
         `reconcile_code_studio` runs provisioning, projection, session and terminal recovery but \
         never calls `code_studio::operations::reconcile`, which has no production caller at all — \
         so every effect interrupted by a crash stays 'pending' forever and no operation ever \
         reaches the 'unknown' queue a person is supposed to work through",
        still_open.len()
    );
}

// =============================================================================
// 3. What survives the connection that produced it
// =============================================================================

/// Runs `git` with extra environment. Used ONLY to build history outside the
/// broker; the broker's own isolated config (§11.2) is not this test's business.
fn git_env(args: &[&str], extra: &[(&str, &str)]) -> std::process::Output {
    let mut command = Command::new("git");
    command
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0");
    for (key, value) in extra {
        command.env(key, value);
    }
    command.output().expect("git is not available")
}

fn git_env_ok(args: &[&str], extra: &[(&str, &str)]) -> String {
    let out = git_env(args, extra);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Moves `main` on with a commit this session did not make — a colleague
/// pushing to the shared branch. Plumbing only, so no worktree of the workspace
/// is touched: the merge under test needs two sides that changed the SAME lines,
/// and that is the only way to reach a genuine conflict.
fn diverge_main(fx: &Fixture, content: &str) -> String {
    let scratch = TempDir::new().expect("scratch dir");
    let blob_source = scratch.path().join("content");
    std::fs::write(&blob_source, content).expect("scratch content");
    let index = scratch.path().join("index");
    let index_env = [("GIT_INDEX_FILE", index.display().to_string())];
    let index_env: Vec<(&str, &str)> = index_env
        .iter()
        .map(|(k, v)| (*k, v.as_str()))
        .collect();

    let git_dir = fx.repo_git_dir();
    let blob = git_ok(&[
        &git_dir,
        "hash-object",
        "-w",
        &blob_source.display().to_string(),
    ]);
    git_env_ok(&[&git_dir, "read-tree", "refs/heads/main"], &index_env);
    git_env_ok(
        &[
            &git_dir,
            "update-index",
            "--cacheinfo",
            &format!("100644,{blob},{REPO_FILE}"),
        ],
        &index_env,
    );
    let tree = git_env_ok(&[&git_dir, "write-tree"], &index_env);
    let commit = git_env_ok(
        &[
            &git_dir,
            "commit-tree",
            &tree,
            "-p",
            "refs/heads/main",
            "-m",
            "a colleague's change",
        ],
        &[
            ("GIT_AUTHOR_NAME", "Colleague"),
            ("GIT_AUTHOR_EMAIL", "colleague@example.invalid"),
            ("GIT_COMMITTER_NAME", "Colleague"),
            ("GIT_COMMITTER_EMAIL", "colleague@example.invalid"),
        ],
    );
    git_ok(&[&git_dir, "update-ref", "refs/heads/main", &commit]);
    commit
}

/// Puts one reviewed commit on the session branch, through the handlers: write
/// → review opens → accept the whole file → confirm → commit.
async fn commit_on_session_branch(fx: &Fixture, content: &str, expected_blob_sha: &str) -> String {
    ok(
        &fx.ctx,
        CodeStudioPayload::FileWriteRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            path: REPO_FILE.into(),
            content: content.to_string(),
            expected_blob_sha: Some(expected_blob_sha.to_string()),
        },
    )
    .await;

    let response = ok(
        &fx.ctx,
        CodeStudioPayload::GitCommitRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            message: "the agent's change".into(),
            patch_set_id: None,
        },
    )
    .await;
    let CodeStudioPayload::GitCommitResponse { patch_set_id, .. } = response else {
        panic!("expected GitCommitResponse");
    };
    let patch_set_id = patch_set_id.expect("the review names the set it decides");

    let response = ok(
        &fx.ctx,
        CodeStudioPayload::PatchSetGetRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            patch_set_id: patch_set_id.clone(),
        },
    )
    .await;
    let CodeStudioPayload::PatchSetGetResponse { files, .. } = response else {
        panic!("expected PatchSetGetResponse");
    };
    let decisions: Vec<PatchFileDecision> = files
        .iter()
        .map(|file| PatchFileDecision {
            patch_file_id: file.patch_file_id.clone(),
            decision: "accept".into(),
            note: None,
            hunks: Vec::new(),
        })
        .collect();
    assert!(!decisions.is_empty(), "the review has nothing in it");
    ok(
        &fx.ctx,
        CodeStudioPayload::PatchDecideRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            patch_set_id: patch_set_id.clone(),
            files: decisions,
        },
    )
    .await;

    allow_once(fx, "git_commit").await;
    let response = ok(
        &fx.ctx,
        CodeStudioPayload::GitCommitRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            message: "the agent's change".into(),
            patch_set_id: Some(patch_set_id),
        },
    )
    .await;
    let CodeStudioPayload::GitCommitResponse {
        status, commit_oid, ..
    } = response
    else {
        panic!("expected GitCommitResponse");
    };
    assert_eq!(status, "committed", "the commit did not happen");
    commit_oid.expect("a commit reports its object id")
}

/// A reload: the workspace pool is dropped so the next read reopens `workspace.
/// db` from disk, and the request arrives on a new connection. The process-wide
/// state (registry database, node identity) is shared, exactly as it is when a
/// browser is refreshed against a running Core.
fn reconnect(fx: &Fixture) -> HandlerContext {
    workspace_db::close(&fx.workspace_id);
    HandlerContext {
        session: SessionAuth::UserSession {
            user_id: [9u8; 16],
            role: None,
        },
        correlation_id: 2,
        connection_id: 1,
        resume_secret: None,
        state: fx.ctx.state.clone(),
        org_context: Some(org()),
    }
}

/// §11.6 pkt 3: a conflicted merge holds its worktree WITH the conflicting files
/// recorded. They used to exist only in the answer to the merge call, so a
/// reload knew a conflict was open — the integration worktree is `held` — and
/// had nothing to put under the heading.
#[tokio::test]
async fn the_conflicting_paths_of_a_merge_survive_a_reload() {
    let Some(fx) = fixture().await else { return };

    // Two sides of the same line: main moves under the session, the session
    // commits its own version of it.
    diverge_main(
        &fx,
        &seed_content().replace("line 2\n", "line 2 CHANGED BY A COLLEAGUE\n"),
    );
    commit_on_session_branch(
        &fx,
        &seed_content().replace("line 2\n", "line 2 CHANGED BY THE AGENT\n"),
        &cs_fs::blob_sha(seed_content().as_bytes()),
    )
    .await;

    assert!(
        call(
            &fx.ctx,
            CodeStudioPayload::GitMergeRequest {
                workspace_id: fx.workspace_id.clone(),
                session_id: fx.session_id.clone(),
                source_branch: fx.branch.clone(),
                target_branch: "main".into(),
            },
        )
        .await
        .is_err(),
        "a merge must ask before it runs"
    );
    allow_once(&fx, "git_merge").await;

    let response = ok(
        &fx.ctx,
        CodeStudioPayload::GitMergeRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            source_branch: fx.branch.clone(),
            target_branch: "main".into(),
        },
    )
    .await;
    let CodeStudioPayload::GitMergeResponse {
        op_id,
        outcome,
        conflict_files,
        integration_worktree,
        ..
    } = response
    else {
        panic!("expected GitMergeResponse");
    };
    assert_eq!(
        outcome, "conflict",
        "the fixture no longer produces a conflicting merge"
    );
    assert_eq!(
        conflict_files,
        vec![REPO_FILE.to_string()],
        "the merge did not name the conflicting path"
    );
    let integration = integration_worktree.expect("a merge records its integration worktree");
    assert_eq!(integration.state, "held");
    assert_eq!(
        integration.conflict_files, conflict_files,
        "the merge answer and the worktree row disagree"
    );

    // ---- the reload --------------------------------------------------------
    let fresh = reconnect(&fx);
    let response = ok(
        &fresh,
        CodeStudioPayload::WorktreesListRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
        },
    )
    .await;
    let CodeStudioPayload::WorktreesListResponse { worktrees, .. } = response else {
        panic!("expected WorktreesListResponse");
    };
    let reloaded = worktrees
        .iter()
        .find(|wt| wt.purpose == "integration" && wt.op_id.as_deref() == Some(op_id.as_str()))
        .expect("the held integration worktree is gone after a reload");
    assert_eq!(reloaded.state, "held");
    assert_eq!(
        reloaded.conflict_files, conflict_files,
        "the conflicting paths did not survive the reload"
    );
}

/// §13.3: what the operator wrote is a timeline fact from the moment it is
/// accepted, and a run that could not start is a second fact next to it.
///
/// No agent run manager is installed in a test binary, so the harness run really
/// cannot start — the exact situation in which the sentence used to disappear:
/// it lived in a local variable, `start_session_run` answered `NotAvailable`,
/// and the timeline never learnt that a person had said anything.
#[tokio::test]
async fn a_turn_reaches_the_timeline_even_when_no_run_starts() {
    let Some(fx) = fixture().await else { return };

    const TURN: &str = "move the branch validation into the broker";
    let error = call(
        &fx.ctx,
        CodeStudioPayload::SessionMessageSendRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            message: TURN.into(),
        },
    )
    .await
    .expect_err("a test binary has no agent run manager, so no run can start");

    let fresh = reconnect(&fx);
    let response = ok(
        &fresh,
        CodeStudioPayload::SessionTimelineRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            after_seq: 0,
            limit: 500,
        },
    )
    .await;
    let CodeStudioPayload::SessionTimelineResponse { events, .. } = response else {
        panic!("expected SessionTimelineResponse");
    };

    let messages: Vec<&str> = events
        .iter()
        .filter(|event| event.kind == "agent_message")
        .map(|event| event.payload_json.as_str())
        .collect();
    assert!(
        messages.iter().any(|json| json.contains(TURN)),
        "the operator's turn is not on the timeline after a failed start; the timeline holds: \
         {messages:?} (the run was refused with: {})",
        error.message
    );
    assert!(
        messages
            .iter()
            .any(|json| json.contains("no run started")),
        "a failed start left no trace at all; the timeline holds: {messages:?}"
    );
}

// =============================================================================
// 6. Work a delegated CLI wrote past our tools
// =============================================================================

/// `delegate_cli` opens a patch set BEFORE the vendor CLI starts, so the set's
/// base is the pre-delegation HEAD and a review sees exactly what the turn
/// changed. What nothing did was FILL it: the vendor writes with its own file
/// calls, so no `fs_write` and no `record_work_edit` ever ran, and the set
/// reached the review with no files in it while `GitDiffRequest` showed the
/// real difference. Delegated work was outside per-hunk review entirely.
///
/// The vendor process is the one part of that path a build machine cannot run,
/// so what is reproduced here is its EFFECT — bytes appearing in the session
/// worktree with no tool call behind them. Everything after that is the real
/// path: the recomputation the block now performs when the turn ends, the wire
/// read the dashboard does, and the per-hunk decision.
#[tokio::test]
async fn work_a_delegation_wrote_straight_to_disk_reaches_the_review() {
    let Some(fx) = fixture().await else { return };
    let pool = fx.pool();
    let broker = Broker::for_workspace(&fx.workspace_id).expect("workspace broker");

    // What the block does before it starts the CLI: freeze the base.
    let opened = tools::current_patch_set(&pool, &broker, &fx.session_id, &PatchScope::Work)
        .expect("open the pre-delegation patch set");
    assert!(
        opened.files.is_empty(),
        "the turn has not run yet, so there is nothing to review"
    );

    // The turn itself.
    std::fs::write(fx.worktree().join(REPO_FILE), edited_content()).expect("the vendor's write");

    // This is what a reviewer used to be shown for the whole delegation.
    let response = ok(
        &fx.ctx,
        CodeStudioPayload::PatchSetGetRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            patch_set_id: opened.id.clone(),
        },
    )
    .await;
    let CodeStudioPayload::PatchSetGetResponse { files, .. } = response else {
        panic!("expected PatchSetGetResponse");
    };
    assert!(
        files.is_empty(),
        "nothing has journalled the vendor's write yet, so the set is still empty"
    );

    // What the block now does when the turn ends — same set, same frozen base.
    let refreshed =
        patch::rescan_patch_set(&pool, &broker, &opened.id).expect("settle the delegation's work");
    assert_eq!(refreshed.id, opened.id);
    assert_eq!(
        refreshed.base_commit, opened.base_commit,
        "the base is frozen before the turn so the set describes the turn, not the branch"
    );

    let response = ok(
        &fx.ctx,
        CodeStudioPayload::PatchSetGetRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            patch_set_id: opened.id.clone(),
        },
    )
    .await;
    let CodeStudioPayload::PatchSetGetResponse { files, .. } = response else {
        panic!("expected PatchSetGetResponse");
    };
    assert_eq!(files.len(), 1, "the review shows {files:?}");
    let file = &files[0];
    assert_eq!(file.path, REPO_FILE);
    assert!(
        file.hunks.len() >= 2,
        "the delegation's change has to be reviewable per hunk, but the set has {} hunk(s)",
        file.hunks.len()
    );

    let accepted = file
        .hunks
        .iter()
        .find(|hunk| hunk.content.contains("line 2 CHANGED"))
        .expect("the first edit has its own hunk");
    let rejected = file
        .hunks
        .iter()
        .find(|hunk| hunk.content.contains("line 23 CHANGED"))
        .expect("the second edit has its own hunk");

    let response = ok(
        &fx.ctx,
        CodeStudioPayload::PatchDecideRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: fx.session_id.clone(),
            patch_set_id: opened.id.clone(),
            files: vec![PatchFileDecision {
                patch_file_id: file.patch_file_id.clone(),
                decision: "accept".into(),
                note: None,
                hunks: vec![
                    PatchHunkDecision {
                        patch_hunk_id: accepted.patch_hunk_id.clone(),
                        decision: "accept".into(),
                    },
                    PatchHunkDecision {
                        patch_hunk_id: rejected.patch_hunk_id.clone(),
                        decision: "reject".into(),
                    },
                ],
            }],
        },
    )
    .await;
    let CodeStudioPayload::PatchDecideResponse {
        status,
        conflicted_paths,
        ..
    } = response
    else {
        panic!("expected PatchDecideResponse");
    };
    assert!(
        conflicted_paths.is_empty(),
        "the partial acceptance did not compose: {conflicted_paths:?}"
    );
    assert_eq!(
        status, "partially_accepted",
        "delegated work must be decidable hunk by hunk, like any other change"
    );
}
