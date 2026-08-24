// ===== File: code_studio/operations.rs — journal of EFFECTS, and how a crash is classified =====
//
// An event's `idempotency_key` protects against writing the same EVENT twice.
// It says nothing about performing the same EFFECT twice, which is the failure
// that destroys work: a file written twice is harmless, a `git push` or a `cargo
// publish` performed twice is not. Every effect therefore opens a row here
// before it happens and closes it afterwards (§13.1).
//
// **Identity comes from where the effect was requested, not from the tool.**
// `op_id = H(session_id, origin_kind, origin_id, logical_step)` — a pure
// function, so the same request re-issued after a reconnect, a retried flow
// block or a replayed terminal command resolves to the SAME row. Operations
// arrive from six origins (tool call, terminal, UI, shim, flow block,
// coordinator) and `(run_id, tool_call_id)` would only cover the first.
//
// **Idempotence is a property of the operation kind, never of the request.**
// `exec`, `git_push` and `git_merge` are non-idempotent by definition: they can
// have effects outside anything a postcondition can observe. `fs_write`,
// `fs_edit`, `fs_mkdir` and building a commit from artifacts are idempotent,
// because their entire effect is described by a hash or an object id. The
// column is derived from `op_kind` here so a caller cannot claim otherwise.
//
// **Reconciliation classifies; it never re-runs.** `reconcile` reports what a
// restart found and marks what it can prove. A safe retry is REPORTED as
// retryable and left `pending` for the coordinator to re-issue; anything else
// unresolved becomes `unknown`, which only a human closes. There is deliberately
// no code path from `unknown` to "just run it again".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::artifacts;
use super::events::{self, EventPayload, SessionEvent};
use super::git_broker::{Broker, RepoHandle};
use super::pep::{Capability, SandboxProfile};
use super::sandbox::{mount_slug, network_slug};
use crate::db::DbPool;

/// Where an effect was requested from. Part of the operation identity, because
/// the same logical step issued from the terminal and from a tool call are two
/// different effects that must not collapse into one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OriginKind {
    ToolCall,
    Terminal,
    Ui,
    Shim,
    FlowBlock,
    Coordinator,
}

impl OriginKind {
    pub fn slug(self) -> &'static str {
        match self {
            OriginKind::ToolCall => "tool_call",
            OriginKind::Terminal => "terminal",
            OriginKind::Ui => "ui",
            OriginKind::Shim => "shim",
            OriginKind::FlowBlock => "flow_block",
            OriginKind::Coordinator => "coordinator",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        match slug {
            "tool_call" => Some(OriginKind::ToolCall),
            "terminal" => Some(OriginKind::Terminal),
            "ui" => Some(OriginKind::Ui),
            "shim" => Some(OriginKind::Shim),
            "flow_block" => Some(OriginKind::FlowBlock),
            "coordinator" => Some(OriginKind::Coordinator),
            _ => None,
        }
    }
}

/// What kind of effect this is. The list is closed on purpose: a new effect has
/// to declare its idempotence and its verifiable postcondition here, where the
/// reconciliation rules can see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpKind {
    FsWrite,
    FsEdit,
    FsMkdir,
    FsDelete,
    Exec,
    GitBranch,
    GitCommit,
    GitPush,
    GitMerge,
    GitMergeFinalize,
    GitWorktree,
}

impl OpKind {
    pub fn slug(self) -> &'static str {
        match self {
            OpKind::FsWrite => "fs_write",
            OpKind::FsEdit => "fs_edit",
            OpKind::FsMkdir => "fs_mkdir",
            OpKind::FsDelete => "fs_delete",
            OpKind::Exec => "exec",
            OpKind::GitBranch => "git_branch",
            OpKind::GitCommit => "git_commit",
            OpKind::GitPush => "git_push",
            OpKind::GitMerge => "git_merge",
            OpKind::GitMergeFinalize => "git_merge_finalize",
            OpKind::GitWorktree => "git_worktree",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        match slug {
            "fs_write" => Some(OpKind::FsWrite),
            "fs_edit" => Some(OpKind::FsEdit),
            "fs_mkdir" => Some(OpKind::FsMkdir),
            "fs_delete" => Some(OpKind::FsDelete),
            "exec" => Some(OpKind::Exec),
            "git_branch" => Some(OpKind::GitBranch),
            "git_commit" => Some(OpKind::GitCommit),
            "git_push" => Some(OpKind::GitPush),
            "git_merge" => Some(OpKind::GitMerge),
            "git_merge_finalize" => Some(OpKind::GitMergeFinalize),
            "git_worktree" => Some(OpKind::GitWorktree),
            _ => None,
        }
    }

    /// Whether repeating the effect after an unconfirmed attempt is safe.
    ///
    /// `Exec` can send a request, charge a card or delete a remote resource —
    /// none of which a local postcondition can see. The merge family stays
    /// non-idempotent for the same reason: a merge also moves a private ref and
    /// an integration worktree, and re-running one after a partial failure is a
    /// decision, not a detail. Everything else is fully described by a content
    /// hash or an object id.
    pub fn idempotent(self) -> bool {
        !matches!(
            self,
            OpKind::Exec | OpKind::GitPush | OpKind::GitMerge | OpKind::GitMergeFinalize
        )
    }
}

/// Lifecycle of one effect (§4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationStatus {
    Pending,
    Completed,
    Failed,
    Unknown,
}

impl OperationStatus {
    pub fn slug(self) -> &'static str {
        match self {
            OperationStatus::Pending => "pending",
            OperationStatus::Completed => "completed",
            OperationStatus::Failed => "failed",
            OperationStatus::Unknown => "unknown",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        match slug {
            "pending" => Some(OperationStatus::Pending),
            "completed" => Some(OperationStatus::Completed),
            "failed" => Some(OperationStatus::Failed),
            "unknown" => Some(OperationStatus::Unknown),
            _ => None,
        }
    }
}

/// What must hold before the effect may be repeated. Checked only for
/// idempotent operations — for the rest it says nothing useful (§13.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Precondition {
    None,
    /// The file still holds exactly the content the operation was planned on.
    ///
    /// The digest is a GIT BLOB id (`code_studio::fs::blob_sha`), not a hash of
    /// the raw bytes — the same value the CAS, the patch sets and the commit
    /// builder compare, which is why `WorktreeProbe::blob_is` computes it with
    /// that function. The field is spelled `sha256` because it is also the CBOR
    /// key of every `precondition_cbor` / `postcondition_cbor` row already
    /// written; renaming it would make those rows undecodable.
    FileBlobIs {
        path: String,
        sha256: String,
    },
    FileAbsent {
        path: String,
    },
    RefEquals {
        refname: String,
        oid: String,
    },
}

/// What proves the effect happened. Every variant is verifiable from outside
/// the journal, except `ExitCodeRecorded`, which is a fact of the journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Postcondition {
    /// No verifiable outcome. Such an operation can never be reconciled to
    /// `completed`; it becomes `unknown` and waits for a person.
    None,
    /// The file holds the content the operation was going to leave behind,
    /// named by its GIT BLOB id — see `Precondition::FileBlobIs`.
    FileBlobIs {
        path: String,
        sha256: String,
    },
    FileAbsent {
        path: String,
    },
    RefEquals {
        refname: String,
        oid: String,
    },
    /// A commit with this tree and parent exists AND a branch points at it.
    ///
    /// Both halves are required, because they are different evidence. The
    /// commit id is not known when the operation opens, so the check runs over
    /// the object ids the executor journalled with `record_oids` as soon as git
    /// produced them.
    CommitExists {
        tree: String,
        parent: Option<String>,
    },
    /// The exit code of a command was recorded — that is, the operation was
    /// closed with the artifact holding the command's outcome. A `pending` exec
    /// by definition has none, which is exactly why an interrupted exec ends up
    /// `unknown`.
    ExitCodeRecorded,
}

impl Postcondition {
    /// Refuses a postcondition nothing could ever satisfy.
    ///
    /// A condition that is false by construction is worse than no condition at
    /// all: `None` is at least honest and sends the operation to a person,
    /// while `CommitExists { tree: String::new(), .. }` compares every candidate
    /// commit's tree against the empty string, can never match, and reports
    /// "nobody knows" about work that was fully verifiable. The check lives at
    /// `begin`, so the mistake is a refused journal entry — visible while the
    /// code that made it is running — instead of a silent `unknown` discovered
    /// after the next crash.
    fn validate(&self, op_kind: OpKind) -> Result<()> {
        match self {
            // §13.1 lists a commit's outcome as the tree, the parent and the
            // value of the reference, all three of them determined by the base
            // commit and the accepted blobs before git is even invoked. An
            // operation that can be verified may not be journalled as one that
            // cannot.
            Postcondition::None if op_kind == OpKind::GitCommit => Err(anyhow!(
                "git_commit has a verifiable outcome and must be journalled with \
                 CommitExists {{ tree, parent }} — the tree of the accepted blobs on top of the \
                 base commit, and the base commit as the parent — plus record_oids of the commit \
                 id as soon as git returns it"
            )),
            Postcondition::None | Postcondition::ExitCodeRecorded => Ok(()),
            Postcondition::FileBlobIs { path, sha256 } => {
                require_named("path", path)?;
                // The value compared at verification time is what the
                // filesystem layer computes — `git hash-object`, i.e. a 40-hex
                // sha1 in a sha1 repository — so refusing that length would
                // refuse every real write. 64 stays for a sha256 repository.
                require_digest("sha256", sha256, &[40, 64])
            }
            Postcondition::FileAbsent { path } => require_named("path", path),
            Postcondition::RefEquals { refname, oid } => {
                require_named("refname", refname)?;
                require_digest("oid", oid, &[40, 64])
            }
            Postcondition::CommitExists { tree, parent } => {
                require_digest("tree", tree, &[40, 64])?;
                match parent {
                    Some(parent) => require_digest("parent", parent, &[40, 64]),
                    None => Ok(()),
                }
            }
        }
    }
}

fn require_named(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(anyhow!("a postcondition without a {field} can never hold"));
    }
    Ok(())
}

/// An object id has to be one git could actually produce. An empty or truncated
/// one compares unequal to everything, which is the silent-falsehood shape this
/// whole check exists to catch.
fn require_digest(field: &str, value: &str, lengths: &[usize]) -> Result<()> {
    if !lengths.contains(&value.len()) || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(anyhow!(
            "a postcondition's {field} must be a hexadecimal object id, got '{value}'"
        ));
    }
    Ok(())
}

/// Canonical, structured, REDACTED description of what the operation was going
/// to do (§7.8). Stored as an artifact, so after a crash we know what was meant
/// without keeping a raw command line anywhere.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OperationInput {
    None,
    Exec {
        argv: Vec<String>,
        cwd: String,
        timeout_secs: u64,
    },
    /// Target content of a write, named by the digest of the blob that already
    /// lives in the CAS — the same bytes are not stored twice.
    FileContent {
        path: String,
        content_sha256: String,
        size_bytes: u64,
    },
    Git {
        operation: String,
        refname: Option<String>,
        remote: Option<String>,
        oids: Vec<String>,
    },
    Params(BTreeMap<String, String>),
}

impl OperationInput {
    fn redacted(self) -> Self {
        match self {
            OperationInput::Exec {
                argv,
                cwd,
                timeout_secs,
            } => OperationInput::Exec {
                argv: super::redact::redact_argv(&argv),
                cwd,
                timeout_secs,
            },
            OperationInput::Git {
                operation,
                refname,
                remote,
                oids,
            } => OperationInput::Git {
                operation,
                refname,
                remote: remote.map(|r| super::redact::redact_url(&r)),
                oids,
            },
            OperationInput::Params(params) => OperationInput::Params(
                params
                    .into_iter()
                    .map(|(key, value)| {
                        let value = super::redact::redact_text(&value);
                        (key, value)
                    })
                    .collect(),
            ),
            other => other,
        }
    }
}

/// Everything an effect declares before it happens.
#[derive(Debug, Clone)]
pub struct OperationRequest {
    pub workspace_id: String,
    pub session_id: String,
    pub run_id: Option<String>,
    pub origin_kind: OriginKind,
    /// `tool_call_id | pty_handle+seq | request_id | shim_call_id | node+iteration | saga_step`.
    pub origin_id: String,
    /// Which step of that origin — one tool call may perform several effects.
    pub logical_step: String,
    pub op_kind: OpKind,
    pub capability: Capability,
    pub input: OperationInput,
    pub precondition: Precondition,
    pub postcondition: Postcondition,
    /// Sandbox profile the PEP resolved this effect to run in, or `None` for an
    /// effect that never enters a sandbox. It is known at `begin` — the caller
    /// had to be told which profile it got before it could run — so the journal
    /// records it with the operation instead of leaving the operations list to
    /// re-derive it from the timeline (§19).
    pub profile: Option<SandboxProfile>,
}

/// One row of the effect journal.
#[derive(Debug, Clone)]
pub struct Operation {
    pub op_id: String,
    pub session_id: String,
    pub run_id: Option<String>,
    pub origin_kind: OriginKind,
    pub origin_id: String,
    pub logical_step: String,
    pub op_kind: OpKind,
    pub capability: String,
    pub idempotent: bool,
    pub input_ref: Option<String>,
    pub precondition: Precondition,
    pub postcondition: Postcondition,
    pub result_oids: Vec<String>,
    pub status: OperationStatus,
    pub result_ref: Option<String>,
    pub error: Option<String>,
    /// 'ro' | 'cow' | 'rw'; absent for an effect that never entered a sandbox.
    pub mount_access: Option<String>,
    /// 'none' | 'gateway'.
    pub network_access: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

/// What a probe can say about a condition. `Inconclusive` is not a synonym for
/// `Unsatisfied`: a probe that cannot answer must not push an operation toward
/// a conclusion it has no evidence for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Satisfied,
    Unsatisfied,
    Inconclusive,
}

/// Verifies conditions against the world outside the database. Journal-only
/// conditions (`None`, `ExitCodeRecorded`) are decided by `reconcile` itself,
/// so an implementation never has to reach back into SQLite.
pub trait PostconditionProbe {
    fn precondition(&self, condition: &Precondition) -> Result<Verdict>;

    /// `result_oids` carries the object ids the executor journalled before the
    /// crash — the only way to identify a commit whose id was not known when
    /// the operation opened.
    fn postcondition(&self, condition: &Postcondition, result_oids: &[String]) -> Result<Verdict>;
}

/// Filesystem half of a session probe, rooted at one worktree.
pub struct WorktreeProbe {
    root: PathBuf,
}

impl WorktreeProbe {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Journal paths are worktree-relative. Anything absolute or climbing out
    /// of the root is refused rather than resolved — a probe must not be the
    /// place where a traversal succeeds.
    fn resolve(&self, path: &str) -> Result<PathBuf> {
        let candidate = Path::new(path);
        if candidate.is_absolute() {
            return Err(anyhow!("operation path '{path}' must be worktree-relative"));
        }
        for component in candidate.components() {
            match component {
                std::path::Component::Normal(_) | std::path::Component::CurDir => {}
                _ => return Err(anyhow!("operation path '{path}' escapes the worktree")),
            }
        }
        Ok(self.root.join(candidate))
    }

    /// The digest a `FileBlobIs` condition names is the GIT BLOB id of the
    /// content, so the verifier computes it with the SAME function the producer
    /// used — `code_studio::fs::blob_sha`, one definition. That is the value the
    /// rest of the module already compares: the CAS a caller supplies as
    /// `expected_blob_sha`, the patch set's `patch_base_blob_sha` /
    /// `current_blob_sha`, and the blobs a commit is assembled from (§13.2).
    /// Any other digest is false against every real write, which would turn a
    /// fully verifiable effect into a question for a person.
    fn blob_is(&self, path: &str, blob_sha: &str) -> Result<Verdict> {
        let resolved = self.resolve(path)?;
        let bytes = match std::fs::read(&resolved) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Verdict::Unsatisfied),
            Err(e) => return Err(anyhow!("read {}: {e}", resolved.display())),
        };
        Ok(
            if super::fs::blob_sha(&bytes) == blob_sha.to_ascii_lowercase() {
                Verdict::Satisfied
            } else {
                Verdict::Unsatisfied
            },
        )
    }

    fn absent(&self, path: &str) -> Result<Verdict> {
        let resolved = self.resolve(path)?;
        Ok(if resolved.exists() {
            Verdict::Unsatisfied
        } else {
            Verdict::Satisfied
        })
    }
}

impl PostconditionProbe for WorktreeProbe {
    fn precondition(&self, condition: &Precondition) -> Result<Verdict> {
        match condition {
            Precondition::FileBlobIs { path, sha256 } => self.blob_is(path, sha256),
            Precondition::FileAbsent { path } => self.absent(path),
            _ => Ok(Verdict::Inconclusive),
        }
    }

    fn postcondition(&self, condition: &Postcondition, _result_oids: &[String]) -> Result<Verdict> {
        match condition {
            Postcondition::FileBlobIs { path, sha256 } => self.blob_is(path, sha256),
            Postcondition::FileAbsent { path } => self.absent(path),
            _ => Ok(Verdict::Inconclusive),
        }
    }
}

/// Git half of a session probe. Every query runs through the broker, so it
/// inherits the hardened invocation — a probe must not be the one place that
/// runs `git` with a weaker config.
pub struct GitProbe {
    broker: Broker,
    handle: RepoHandle,
}

impl GitProbe {
    pub fn new(broker: Broker, handle: RepoHandle) -> Self {
        Self { broker, handle }
    }

    fn ref_equals(&self, refname: &str, oid: &str) -> Result<Verdict> {
        Ok(match self.broker.rev_parse(&self.handle, refname)? {
            Some(actual) if actual.eq_ignore_ascii_case(oid) => Verdict::Satisfied,
            _ => Verdict::Unsatisfied,
        })
    }

    /// A commit landed when the object exists with the right shape AND a branch
    /// points at it (§13.1).
    ///
    /// The second half is not a formality. `build_commit` writes the object
    /// with `commit-tree` and only THEN moves the branch with `update-ref`, so
    /// a crash between the two leaves a perfectly well-formed commit that no
    /// reference names — work nobody will ever see. Checking the object alone
    /// would report exactly that state as "completed".
    fn commit_exists(
        &self,
        tree: &str,
        parent: Option<&str>,
        result_oids: &[String],
    ) -> Result<Verdict> {
        if result_oids.is_empty() {
            return Ok(Verdict::Unsatisfied);
        }
        for oid in result_oids {
            let Some(meta) = self.broker.commit_metadata(&self.handle, oid)? else {
                continue;
            };
            let tree_matches = meta.tree.eq_ignore_ascii_case(tree);
            let parent_matches = match parent {
                Some(parent) => meta.parents.iter().any(|p| p.eq_ignore_ascii_case(parent)),
                None => meta.parents.is_empty(),
            };
            if tree_matches && parent_matches && self.is_a_branch_head(oid)? {
                return Ok(Verdict::Satisfied);
            }
        }
        Ok(Verdict::Unsatisfied)
    }

    /// Whether a branch of the repository currently HAS this object id as its
    /// value — the "value of the reference" half of a commit's evidence.
    ///
    /// Branch heads, not every ref and not reachability: a session's commit
    /// lands on a branch, and "an ancestor of something" is not the question
    /// reconciliation is asking. A commit that is merely reachable leaves the
    /// operation `unknown`, which is a person's decision rather than a guess.
    fn is_a_branch_head(&self, oid: &str) -> Result<bool> {
        for branch in self.broker.branches(&self.handle)? {
            if let Some(head) = self.broker.rev_parse(&self.handle, &branch.name)? {
                if head.eq_ignore_ascii_case(oid) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

impl PostconditionProbe for GitProbe {
    fn precondition(&self, condition: &Precondition) -> Result<Verdict> {
        match condition {
            Precondition::RefEquals { refname, oid } => self.ref_equals(refname, oid),
            _ => Ok(Verdict::Inconclusive),
        }
    }

    fn postcondition(&self, condition: &Postcondition, result_oids: &[String]) -> Result<Verdict> {
        match condition {
            Postcondition::RefEquals { refname, oid } => self.ref_equals(refname, oid),
            Postcondition::CommitExists { tree, parent } => {
                self.commit_exists(tree, parent.as_deref(), result_oids)
            }
            _ => Ok(Verdict::Inconclusive),
        }
    }
}

/// The probe a live session reconciles against: its worktree and its
/// repository, dispatched per condition.
pub struct SessionProbe {
    files: WorktreeProbe,
    git: GitProbe,
}

impl SessionProbe {
    pub fn new(files: WorktreeProbe, git: GitProbe) -> Self {
        Self { files, git }
    }

    /// Probe of a session on this node. Both halves are derived from the
    /// workspace id, never from a path in a request.
    pub fn for_session(workspace_id: &str, session_id: &str) -> Result<Self> {
        let broker = Broker::for_workspace(workspace_id)?;
        let handle = broker.session(session_id)?;
        let root = broker.session_worktree(session_id)?;
        Ok(Self {
            files: WorktreeProbe::new(root),
            git: GitProbe::new(broker, handle),
        })
    }
}

impl PostconditionProbe for SessionProbe {
    fn precondition(&self, condition: &Precondition) -> Result<Verdict> {
        match condition {
            Precondition::RefEquals { .. } => self.git.precondition(condition),
            _ => self.files.precondition(condition),
        }
    }

    fn postcondition(&self, condition: &Postcondition, result_oids: &[String]) -> Result<Verdict> {
        match condition {
            Postcondition::RefEquals { .. } | Postcondition::CommitExists { .. } => {
                self.git.postcondition(condition, result_oids)
            }
            _ => self.files.postcondition(condition, result_oids),
        }
    }
}

/// Identity of an effect. Pure and deterministic: the same origin tuple always
/// yields the same id, on any node and after any restart. Fields are length
/// prefixed so `("a", "bc")` and `("ab", "c")` cannot collide.
pub fn op_id(
    session_id: &str,
    origin_kind: OriginKind,
    origin_id: &str,
    logical_step: &str,
) -> String {
    let mut hasher = Sha256::new();
    for field in [session_id, origin_kind.slug(), origin_id, logical_step] {
        hasher.update((field.len() as u64).to_le_bytes());
        hasher.update(field.as_bytes());
    }
    hex::encode(hasher.finalize())
}

/// Opens the journal row for an effect that is about to happen.
///
/// Re-issuing the same request returns the EXISTING row instead of failing.
/// That is the whole of operation idempotency: a caller that cannot tell
/// whether it already asked simply asks again.
pub fn begin(pool: &DbPool, request: &OperationRequest) -> Result<Operation> {
    request.postcondition.validate(request.op_kind)?;
    let op_id = op_id(
        &request.session_id,
        request.origin_kind,
        &request.origin_id,
        &request.logical_step,
    );
    if let Some(existing) = get(pool, &op_id)? {
        return Ok(existing);
    }

    // The canonical input is redacted and content-addressed before the row
    // exists, so the row never points at an artifact that is not there yet.
    let input_ref = match &request.input {
        OperationInput::None => None,
        input => {
            let canonical = events::to_cbor(&input.clone().redacted())?;
            Some(artifacts::put(pool, &request.workspace_id, &canonical, "operation_input")?.sha256)
        }
    };

    let idempotent = request.op_kind.idempotent();
    let mut conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    let tx = conn.transaction()?;
    if let Some(existing) = read_operation(&tx, &op_id)? {
        return Ok(existing);
    }
    tx.execute(
        "INSERT INTO session_operations \
          (op_id, session_id, run_id, origin_kind, origin_id, logical_step, op_kind, capability, \
           idempotent, input_ref, precondition_cbor, postcondition_cbor, result_oids, status, \
           started_at, mount_access, network_access) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, '[]', 'pending', \
                 datetime('now'), ?13, ?14)",
        rusqlite::params![
            op_id,
            request.session_id,
            request.run_id,
            request.origin_kind.slug(),
            request.origin_id,
            request.logical_step,
            request.op_kind.slug(),
            request.capability.slug(),
            i64::from(idempotent),
            input_ref,
            events::to_cbor(&request.precondition)?,
            events::to_cbor(&request.postcondition)?,
            request.profile.map(|p| mount_slug(p.mount)),
            request.profile.map(|p| network_slug(p.network)),
        ],
    )?;
    if let Some(sha256) = &input_ref {
        artifacts::retain_in_tx(&tx, sha256)?;
    }
    let mut event = SessionEvent::new(
        format!("op:{op_id}:started"),
        EventPayload::OperationStarted {
            op_id: op_id.clone(),
            op_kind: request.op_kind.slug().to_string(),
            capability: request.capability.slug().to_string(),
        },
    );
    if let Some(run_id) = &request.run_id {
        event = event.with_run(run_id.clone());
    }
    if let Some(sha256) = &input_ref {
        event = event.with_artifact(sha256.clone());
    }
    events::append_in_tx(&tx, &request.session_id, event)?;

    let operation = read_operation(&tx, &op_id)?
        .ok_or_else(|| anyhow!("operation {op_id} vanished right after insert"))?;
    tx.commit()?;
    Ok(operation)
}

/// Journals object ids as soon as the executor learns them, BEFORE the
/// operation is closed. Without this a commit interrupted between "git wrote
/// the object" and "we recorded the outcome" would be unidentifiable, and its
/// postcondition unverifiable.
pub fn record_oids(pool: &DbPool, op_id: &str, result_oids: &[String]) -> Result<()> {
    let conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    let changed = conn.execute(
        "UPDATE session_operations SET result_oids = ?2 WHERE op_id = ?1 AND status = 'pending'",
        rusqlite::params![op_id, serde_json::to_string(result_oids)?],
    )?;
    if changed == 0 {
        return Err(anyhow!("no pending operation {op_id}"));
    }
    Ok(())
}

/// Closes an operation that succeeded. The status change and the event that
/// records it commit together.
pub fn complete(
    pool: &DbPool,
    op_id: &str,
    result_oids: &[String],
    result_ref: Option<&str>,
) -> Result<Operation> {
    finish(
        pool,
        op_id,
        OperationStatus::Completed,
        result_oids,
        result_ref,
        None,
    )
}

/// Closes an operation that failed. A failure is a KNOWN outcome — the effect
/// did not happen — and is not the same as `unknown`.
pub fn fail(pool: &DbPool, op_id: &str, error: &str) -> Result<Operation> {
    finish(pool, op_id, OperationStatus::Failed, &[], None, Some(error))
}

fn finish(
    pool: &DbPool,
    op_id: &str,
    status: OperationStatus,
    result_oids: &[String],
    result_ref: Option<&str>,
    error: Option<&str>,
) -> Result<Operation> {
    let mut conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    let tx = conn.transaction()?;
    let operation =
        read_operation(&tx, op_id)?.ok_or_else(|| anyhow!("unknown operation {op_id}"))?;
    match operation.status {
        OperationStatus::Pending => {}
        OperationStatus::Unknown => {
            return Err(anyhow!(
                "operation {op_id} is unknown; it needs an explicit resolution, not a retry"
            ))
        }
        already => {
            if already == status {
                return Ok(operation);
            }
            return Err(anyhow!("operation {op_id} is already {}", already.slug()));
        }
    }

    // `ExitCodeRecorded` is satisfied by the RESULT ARTIFACT and by nothing
    // else. Closing such an operation without one produces a row whose own
    // postcondition is false — reconciliation would call it `unknown` while the
    // journal calls it `completed` — and no artifact of the command's outcome
    // ever exists. The caller has to store the outcome and pass its digest.
    if status == OperationStatus::Completed
        && operation.postcondition == Postcondition::ExitCodeRecorded
        && result_ref.is_none()
    {
        return Err(anyhow!(
            "operation {op_id} promises a recorded exit code, so it cannot be completed without \
             the artifact holding the command's outcome"
        ));
    }

    let oids = if result_oids.is_empty() {
        operation.result_oids.clone()
    } else {
        result_oids.to_vec()
    };
    tx.execute(
        "UPDATE session_operations SET status = ?2, result_oids = ?3, result_ref = ?4, \
          error = ?5, finished_at = datetime('now') WHERE op_id = ?1",
        rusqlite::params![
            op_id,
            status.slug(),
            serde_json::to_string(&oids)?,
            result_ref,
            error,
        ],
    )?;
    if let Some(sha256) = result_ref {
        artifacts::retain_in_tx(&tx, sha256)?;
    }
    let mut event = SessionEvent::new(
        format!("op:{op_id}:finished"),
        EventPayload::OperationFinished {
            op_id: op_id.to_string(),
            op_kind: operation.op_kind.slug().to_string(),
            status: status.slug().to_string(),
            error: error.map(str::to_string),
        },
    );
    if let Some(run_id) = &operation.run_id {
        event = event.with_run(run_id.clone());
    }
    if let Some(sha256) = result_ref {
        event = event.with_artifact(sha256.to_string());
    }
    events::append_in_tx(&tx, &operation.session_id, event)?;

    let updated = read_operation(&tx, op_id)?
        .ok_or_else(|| anyhow!("operation {op_id} vanished while being closed"))?;
    tx.commit()?;
    Ok(updated)
}

/// How a restart classified one interrupted operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileOutcome {
    /// The postcondition holds: the effect landed before the crash. The row is
    /// now `completed`.
    Completed,
    /// The effect did not land, the operation is idempotent and its
    /// precondition still holds. The row stays `pending` and the coordinator
    /// may re-issue it — reconciliation does not execute anything itself.
    Retryable,
    /// Nothing could be proven. The row is `unknown` and waits for a person.
    Unknown,
}

#[derive(Debug, Clone)]
pub struct ReconcileEntry {
    pub op_id: String,
    pub op_kind: OpKind,
    pub outcome: ReconcileOutcome,
}

#[derive(Debug, Clone, Default)]
pub struct ReconcileReport {
    pub entries: Vec<ReconcileEntry>,
}

impl ReconcileReport {
    fn count(&self, outcome: ReconcileOutcome) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.outcome == outcome)
            .count()
    }

    /// Operations a person has to decide about. Reported as a metric (§22) and
    /// alerted on when one lingers.
    pub fn unknown(&self) -> usize {
        self.count(ReconcileOutcome::Unknown)
    }

    pub fn completed(&self) -> usize {
        self.count(ReconcileOutcome::Completed)
    }

    pub fn retryable(&self) -> usize {
        self.count(ReconcileOutcome::Retryable)
    }
}

/// Classifies every `pending` operation of a session against the world (§13.1).
pub fn reconcile(
    pool: &DbPool,
    session_id: &str,
    probe: &dyn PostconditionProbe,
) -> Result<ReconcileReport> {
    let mut report = ReconcileReport::default();
    for operation in list_by_status(pool, session_id, OperationStatus::Pending, None)? {
        let (outcome, reason) = classify(&operation, probe)?;
        match outcome {
            ReconcileOutcome::Completed => {
                settle(pool, &operation, OperationStatus::Completed, reason)?;
            }
            ReconcileOutcome::Unknown => {
                settle(pool, &operation, OperationStatus::Unknown, reason)?;
            }
            // A retryable operation stays `pending` on purpose: the row is the
            // coordinator's instruction to re-issue it, and rewriting the
            // status here would lose that.
            ReconcileOutcome::Retryable => {}
        }
        report.entries.push(ReconcileEntry {
            op_id: operation.op_id,
            op_kind: operation.op_kind,
            outcome,
        });
    }
    Ok(report)
}

/// The reconciliation table of §13.1, in order. The reason travels with the
/// outcome because it is what the recorded event says, and "why did this become
/// unknown" is the first question anyone asks about one.
fn classify(
    operation: &Operation,
    probe: &dyn PostconditionProbe,
) -> Result<(ReconcileOutcome, &'static str)> {
    if postcondition_verdict(operation, probe)? == Verdict::Satisfied {
        return Ok((ReconcileOutcome::Completed, "postcondition holds"));
    }
    // The idempotence gate comes BEFORE the precondition on purpose: for a
    // non-idempotent operation a satisfied precondition proves nothing, because
    // the effect may have reached somewhere the precondition cannot see.
    if operation.idempotent && probe.precondition(&operation.precondition)? == Verdict::Satisfied {
        return Ok((
            ReconcileOutcome::Retryable,
            "precondition holds and the operation is idempotent",
        ));
    }
    // There is deliberately no third chance here.
    //
    // What used to sit at this point was "the objects this operation recorded
    // exist locally, so call it completed", and that is not evidence of
    // anything: git writes objects before it moves references, so a commit
    // interrupted between `commit-tree` and `update-ref` leaves exactly that
    // picture — the object on disk, the branch untouched, the work invisible.
    // A push is worse still: its objects are local by definition and say
    // nothing about the remote. Everything an unattended pass may conclude is
    // stated in the postcondition, where the reference is part of the claim.
    Ok((
        ReconcileOutcome::Unknown,
        "no condition could be proven; a person has to decide",
    ))
}

fn postcondition_verdict(operation: &Operation, probe: &dyn PostconditionProbe) -> Result<Verdict> {
    match &operation.postcondition {
        Postcondition::None => Ok(Verdict::Inconclusive),
        Postcondition::ExitCodeRecorded => Ok(if operation.result_ref.is_some() {
            Verdict::Satisfied
        } else {
            Verdict::Unsatisfied
        }),
        condition => probe.postcondition(condition, &operation.result_oids),
    }
}

fn settle(
    pool: &DbPool,
    operation: &Operation,
    status: OperationStatus,
    reason: &str,
) -> Result<()> {
    let mut conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    let tx = conn.transaction()?;
    tx.execute(
        "UPDATE session_operations SET status = ?2, finished_at = \
          CASE WHEN ?2 = 'completed' THEN datetime('now') ELSE finished_at END \
         WHERE op_id = ?1 AND status = 'pending'",
        rusqlite::params![operation.op_id, status.slug()],
    )?;
    let mut event = SessionEvent::new(
        format!("op:{}:reconciled:{}", operation.op_id, status.slug()),
        EventPayload::OperationReconciled {
            op_id: operation.op_id.clone(),
            op_kind: operation.op_kind.slug().to_string(),
            from: OperationStatus::Pending.slug().to_string(),
            to: status.slug().to_string(),
            reason: reason.to_string(),
        },
    );
    if let Some(run_id) = &operation.run_id {
        event = event.with_run(run_id.clone());
    }
    events::append_in_tx(&tx, &operation.session_id, event)?;
    tx.commit()?;
    Ok(())
}

/// How a person closed an `unknown` operation.
#[derive(Debug, Clone)]
pub enum UnknownDecision {
    /// "It happened" — with the object ids the user confirmed, if any.
    Completed { result_oids: Vec<String> },
    /// "It did not happen."
    Failed { error: String },
}

/// Closes an `unknown` operation by an explicit human decision. This is the
/// ONLY transition out of `unknown`; there is no automatic one, by design.
pub fn resolve_unknown(
    pool: &DbPool,
    op_id: &str,
    decision: &UnknownDecision,
    decided_by: &str,
) -> Result<Operation> {
    let mut conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    let tx = conn.transaction()?;
    let operation =
        read_operation(&tx, op_id)?.ok_or_else(|| anyhow!("unknown operation {op_id}"))?;
    if operation.status != OperationStatus::Unknown {
        return Err(anyhow!(
            "operation {op_id} is {}, not unknown",
            operation.status.slug()
        ));
    }

    let (status, oids, error) = match decision {
        UnknownDecision::Completed { result_oids } => {
            (OperationStatus::Completed, result_oids.clone(), None)
        }
        UnknownDecision::Failed { error } => {
            (OperationStatus::Failed, Vec::new(), Some(error.clone()))
        }
    };
    tx.execute(
        "UPDATE session_operations SET status = ?2, result_oids = ?3, error = ?4, \
          finished_at = datetime('now') WHERE op_id = ?1",
        rusqlite::params![op_id, status.slug(), serde_json::to_string(&oids)?, error,],
    )?;
    let mut event = SessionEvent::new(
        format!("op:{op_id}:resolved"),
        EventPayload::OperationReconciled {
            op_id: op_id.to_string(),
            op_kind: operation.op_kind.slug().to_string(),
            from: OperationStatus::Unknown.slug().to_string(),
            to: status.slug().to_string(),
            reason: format!("resolved by {decided_by}"),
        },
    );
    if let Some(run_id) = &operation.run_id {
        event = event.with_run(run_id.clone());
    }
    events::append_in_tx(&tx, &operation.session_id, event)?;

    let updated = read_operation(&tx, op_id)?
        .ok_or_else(|| anyhow!("operation {op_id} vanished while being resolved"))?;
    tx.commit()?;
    Ok(updated)
}

pub fn get(pool: &DbPool, op_id: &str) -> Result<Option<Operation>> {
    let conn = pool.read().map_err(|e| anyhow!("workspace db read: {e}"))?;
    read_operation(&conn, op_id)
}

/// Operations of a session in one status — `unknown` feeds the UI list of
/// decisions a person still owes.
pub fn list_by_status(
    pool: &DbPool,
    session_id: &str,
    status: OperationStatus,
    limit: Option<u32>,
) -> Result<Vec<Operation>> {
    let conn = pool.read().map_err(|e| anyhow!("workspace db read: {e}"))?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {SELECT_COLUMNS} FROM session_operations \
         WHERE session_id = ?1 AND status = ?2 ORDER BY started_at, op_id LIMIT ?3"
    ))?;
    // The bound belongs to the QUERY: every row carries a CBOR input blob that
    // is decoded on the way out, so a page of twenty must not cost the decode
    // of a whole journal. `None` is for the two internal scans that genuinely
    // have to see every pending operation.
    let rows = stmt.query_map(
        rusqlite::params![
            session_id,
            status.slug(),
            limit.map(i64::from).unwrap_or(-1)
        ],
        map_operation,
    )?;
    rows.collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .map(decode_operation)
        .collect()
}

const SELECT_COLUMNS: &str = "op_id, session_id, run_id, origin_kind, origin_id, logical_step, \
     op_kind, capability, idempotent, input_ref, precondition_cbor, postcondition_cbor, \
     result_oids, status, result_ref, error, started_at, finished_at, \
     mount_access, network_access";

/// Raw row shape, decoded into typed fields by `decode_operation`.
type RawOperation = (
    String,
    String,
    Option<String>,
    String,
    String,
    String,
    String,
    String,
    i64,
    Option<String>,
    Vec<u8>,
    Vec<u8>,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn map_operation(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawOperation> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
        row.get(14)?,
        row.get(15)?,
        row.get(16)?,
        row.get(17)?,
        row.get(18)?,
        row.get(19)?,
    ))
}

fn decode_operation(raw: RawOperation) -> Result<Operation> {
    let (
        op_id,
        session_id,
        run_id,
        origin_kind,
        origin_id,
        logical_step,
        op_kind,
        capability,
        idempotent,
        input_ref,
        precondition,
        postcondition,
        result_oids,
        status,
        result_ref,
        error,
        started_at,
        finished_at,
        mount_access,
        network_access,
    ) = raw;
    Ok(Operation {
        origin_kind: OriginKind::from_slug(&origin_kind)
            .ok_or_else(|| anyhow!("operation {op_id} has unknown origin '{origin_kind}'"))?,
        op_kind: OpKind::from_slug(&op_kind)
            .ok_or_else(|| anyhow!("operation {op_id} has unknown kind '{op_kind}'"))?,
        status: OperationStatus::from_slug(&status)
            .ok_or_else(|| anyhow!("operation {op_id} has unknown status '{status}'"))?,
        op_id,
        session_id,
        run_id,
        origin_id,
        logical_step,
        capability,
        idempotent: idempotent != 0,
        input_ref,
        precondition: events::from_cbor(&precondition)?,
        postcondition: events::from_cbor(&postcondition)?,
        result_oids: serde_json::from_str(&result_oids)?,
        result_ref,
        error,
        mount_access,
        network_access,
        started_at,
        finished_at,
    })
}

fn read_operation(conn: &rusqlite::Connection, op_id: &str) -> Result<Option<Operation>> {
    let raw = conn
        .query_row(
            &format!("SELECT {SELECT_COLUMNS} FROM session_operations WHERE op_id = ?1"),
            rusqlite::params![op_id],
            map_operation,
        )
        .optional()?;
    raw.map(decode_operation).transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_studio::{events::EventKind, paths, workspace_db};

    struct Fixture {
        _data: tempfile::TempDir,
        worktree: tempfile::TempDir,
        pool: DbPool,
        workspace_id: String,
    }

    fn fixture(workspace_id: &str) -> Fixture {
        let data = tempfile::tempdir().expect("data dir");
        crate::paths::set_category_override(
            crate::paths::StorageCategory::Data,
            Some(data.path().to_string_lossy().to_string()),
        );
        let root = paths::create_workspace_layout(workspace_id).expect("layout");
        let (pool, _version) = workspace_db::open_pool_at(&root).expect("open workspace.db");
        {
            let conn = pool.write().unwrap();
            conn.execute(
                "INSERT INTO sessions (id, workspace_id, user_id, title, branch, autonomy_mode, \
                  flow_id, flow_version_id, status, created_at, updated_at) \
                 VALUES ('s-1', ?1, 'u-1', 'Session', 'cs/u/1', 'normal', 'flow', 'v1', 'idle', \
                  datetime('now'), datetime('now'))",
                rusqlite::params![workspace_id],
            )
            .unwrap();
        }
        Fixture {
            _data: data,
            worktree: tempfile::tempdir().expect("worktree"),
            pool,
            workspace_id: workspace_id.to_string(),
        }
    }

    fn release_paths() {
        crate::paths::set_category_override(crate::paths::StorageCategory::Data, None);
    }

    /// Digest of the CAS artifact holding the content — a plain sha256 of the
    /// bytes, which is what `artifacts::put` returns.
    fn sha256_hex(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hex::encode(hasher.finalize())
    }

    /// Digest a `FileBlobIs` condition is written with. It is deliberately the
    /// production function rather than a re-implementation: the bug this pins
    /// was exactly a verifier and a producer that each hashed their own way.
    fn blob_sha(bytes: &[u8]) -> String {
        crate::code_studio::fs::blob_sha(bytes)
    }

    fn write_request(
        fx: &Fixture,
        step: &str,
        content: &[u8],
        base: Option<&[u8]>,
    ) -> OperationRequest {
        OperationRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: "s-1".into(),
            run_id: Some("r-1".into()),
            origin_kind: OriginKind::ToolCall,
            origin_id: "call-1".into(),
            logical_step: step.into(),
            op_kind: OpKind::FsWrite,
            capability: Capability::FsWrite,
            input: OperationInput::FileContent {
                path: "src/main.rs".into(),
                content_sha256: sha256_hex(content),
                size_bytes: content.len() as u64,
            },
            precondition: match base {
                Some(base) => Precondition::FileBlobIs {
                    path: "src/main.rs".into(),
                    sha256: blob_sha(base),
                },
                None => Precondition::FileAbsent {
                    path: "src/main.rs".into(),
                },
            },
            postcondition: Postcondition::FileBlobIs {
                path: "src/main.rs".into(),
                sha256: blob_sha(content),
            },
            profile: None,
        }
    }

    fn put_file(fx: &Fixture, relative: &str, content: &[u8]) {
        let path = fx.worktree.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn probe(fx: &Fixture) -> WorktreeProbe {
        WorktreeProbe::new(fx.worktree.path())
    }

    #[test]
    fn the_operation_id_is_a_pure_function_of_the_origin_tuple() {
        let terminal = op_id("s-1", OriginKind::Terminal, "pty-1#7", "write");
        assert_eq!(
            terminal,
            op_id("s-1", OriginKind::Terminal, "pty-1#7", "write"),
            "the same origin tuple produced two identities"
        );

        // Every origin kind is a separate identity space.
        let mut seen = std::collections::BTreeSet::new();
        for origin in [
            OriginKind::ToolCall,
            OriginKind::Terminal,
            OriginKind::Ui,
            OriginKind::Shim,
            OriginKind::FlowBlock,
            OriginKind::Coordinator,
        ] {
            assert!(
                seen.insert(op_id("s-1", origin, "same-id", "write")),
                "two origin kinds collided"
            );
        }

        assert_ne!(
            op_id("s-1", OriginKind::ToolCall, "call-1", "write"),
            op_id("s-1", OriginKind::ToolCall, "call-1", "chmod"),
            "two logical steps of one call share an identity"
        );
        assert_ne!(
            op_id("s-1", OriginKind::ToolCall, "call-1", "write"),
            op_id("s-2", OriginKind::ToolCall, "call-1", "write")
        );
        // Length prefixing: ("ab","c") must not equal ("a","bc").
        assert_ne!(
            op_id("s-1", OriginKind::Ui, "ab", "c"),
            op_id("s-1", OriginKind::Ui, "a", "bc")
        );
    }

    #[test]
    fn beginning_the_same_operation_twice_returns_the_same_row() {
        let _guard = crate::code_studio::paths::test_data_dir_guard();
        let fx = fixture("ws-op-begin");
        let request = write_request(&fx, "write", b"fn main() {}", None);

        let first = begin(&fx.pool, &request).unwrap();
        let second = begin(&fx.pool, &request).unwrap();
        assert_eq!(first.op_id, second.op_id);
        assert!(first.idempotent, "fs_write must be idempotent");

        let rows: i64 = {
            let conn = fx.pool.read().unwrap();
            conn.query_row("SELECT COUNT(*) FROM session_operations", [], |row| {
                row.get(0)
            })
            .unwrap()
        };
        assert_eq!(rows, 1, "a retried begin created a second effect");
        // One journal row, one timeline entry.
        let started = events::read_after(&fx.pool, "s-1", 0, 100)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == EventKind::OperationStarted)
            .count();
        assert_eq!(started, 1);
        release_paths();
    }

    #[test]
    fn an_effect_that_landed_before_the_crash_is_completed() {
        let _guard = crate::code_studio::paths::test_data_dir_guard();
        let fx = fixture("ws-op-landed");
        let content = b"fn main() { println!(\"hi\"); }";
        begin(&fx.pool, &write_request(&fx, "write", content, None)).unwrap();
        // The write happened; the process died before it could be recorded.
        put_file(&fx, "src/main.rs", content);

        let report = reconcile(&fx.pool, "s-1", &probe(&fx)).unwrap();
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].outcome, ReconcileOutcome::Completed);
        assert_eq!(report.completed(), 1);

        let stored = get(&fx.pool, &report.entries[0].op_id).unwrap().unwrap();
        assert_eq!(stored.status, OperationStatus::Completed);
        assert!(stored.finished_at.is_some());
        release_paths();
    }

    #[test]
    fn an_idempotent_effect_interrupted_before_it_happened_is_retryable() {
        let _guard = crate::code_studio::paths::test_data_dir_guard();
        let fx = fixture("ws-op-retry");
        let base = b"fn main() {}";
        let target = b"fn main() { work(); }";
        put_file(&fx, "src/main.rs", base);
        let request = write_request(&fx, "write", target, Some(base));
        let opened = begin(&fx.pool, &request).unwrap();

        let report = reconcile(&fx.pool, "s-1", &probe(&fx)).unwrap();
        assert_eq!(report.entries[0].outcome, ReconcileOutcome::Retryable);
        assert_eq!(report.retryable(), 1);

        // Retryable means "the coordinator may re-issue it" — the row stays
        // open, and reconciliation itself changed nothing on disk.
        let stored = get(&fx.pool, &opened.op_id).unwrap().unwrap();
        assert_eq!(stored.status, OperationStatus::Pending);
        assert_eq!(
            std::fs::read(fx.worktree.path().join("src/main.rs")).unwrap(),
            base
        );
        release_paths();
    }

    #[test]
    fn an_idempotent_effect_whose_precondition_moved_on_is_unknown() {
        let _guard = crate::code_studio::paths::test_data_dir_guard();
        let fx = fixture("ws-op-moved");
        let base = b"fn main() {}";
        let target = b"fn main() { work(); }";
        // Neither condition holds: someone else rewrote the file.
        put_file(&fx, "src/main.rs", b"something entirely different");
        begin(&fx.pool, &write_request(&fx, "write", target, Some(base))).unwrap();

        let report = reconcile(&fx.pool, "s-1", &probe(&fx)).unwrap();
        assert_eq!(report.entries[0].outcome, ReconcileOutcome::Unknown);
        release_paths();
    }

    #[test]
    fn an_unconfirmed_exec_is_unknown_even_when_its_precondition_holds() {
        let _guard = crate::code_studio::paths::test_data_dir_guard();
        let fx = fixture("ws-op-exec");
        put_file(&fx, "Cargo.toml", b"[package]\n");

        for (step, kind) in [
            ("run-tests", OpKind::Exec),
            ("push-branch", OpKind::GitPush),
            ("merge-branch", OpKind::GitMerge),
        ] {
            let request = OperationRequest {
                workspace_id: fx.workspace_id.clone(),
                session_id: "s-1".into(),
                run_id: None,
                origin_kind: OriginKind::Terminal,
                origin_id: "pty-1#3".into(),
                logical_step: step.into(),
                op_kind: kind,
                capability: Capability::Exec,
                input: OperationInput::Exec {
                    argv: vec!["cargo".into(), "test".into()],
                    cwd: ".".into(),
                    timeout_secs: 300,
                },
                // The precondition holds throughout — it must not matter.
                precondition: Precondition::FileBlobIs {
                    path: "Cargo.toml".into(),
                    sha256: blob_sha(b"[package]\n"),
                },
                postcondition: if kind == OpKind::Exec {
                    Postcondition::ExitCodeRecorded
                } else {
                    Postcondition::None
                },
                profile: (kind == OpKind::Exec).then_some(SandboxProfile {
                    mount: crate::code_studio::pep::MountAccess::CopyOnWrite,
                    network: crate::code_studio::pep::NetworkAccess::None,
                }),
            };
            let opened = begin(&fx.pool, &request).unwrap();
            assert!(!opened.idempotent, "{} must be non-idempotent", kind.slug());
        }

        let report = reconcile(&fx.pool, "s-1", &probe(&fx)).unwrap();
        assert_eq!(report.entries.len(), 3);
        assert_eq!(
            report.unknown(),
            3,
            "a non-idempotent effect was treated as retryable: {:?}",
            report.entries
        );
        release_paths();
    }

    #[test]
    fn an_unknown_operation_is_never_retried_and_only_a_person_closes_it() {
        let _guard = crate::code_studio::paths::test_data_dir_guard();
        let fx = fixture("ws-op-unknown");
        put_file(&fx, "Cargo.toml", b"[package]\n");
        let request = OperationRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: "s-1".into(),
            run_id: None,
            origin_kind: OriginKind::Shim,
            origin_id: "shim-9".into(),
            logical_step: "push".into(),
            op_kind: OpKind::GitPush,
            capability: Capability::GitPush,
            input: OperationInput::Git {
                operation: "push".into(),
                refname: Some("refs/heads/cs/piotr/s1".into()),
                remote: Some("https://user:hunter2@github.com/o/r.git".into()),
                oids: vec![],
            },
            precondition: Precondition::None,
            postcondition: Postcondition::RefEquals {
                refname: "refs/remotes/origin/cs/piotr/s1".into(),
                oid: "a".repeat(40),
            },
            profile: None,
        };
        let opened = begin(&fx.pool, &request).unwrap();

        let first = reconcile(&fx.pool, "s-1", &probe(&fx)).unwrap();
        assert_eq!(first.unknown(), 1);
        assert_eq!(
            get(&fx.pool, &opened.op_id).unwrap().unwrap().status,
            OperationStatus::Unknown
        );

        // A second pass must not touch it: `unknown` is not a work queue.
        let second = reconcile(&fx.pool, "s-1", &probe(&fx)).unwrap();
        assert!(
            second.entries.is_empty(),
            "an unknown operation was picked up again"
        );
        assert!(
            complete(&fx.pool, &opened.op_id, &[], None).is_err(),
            "an unknown operation was silently completed"
        );

        let resolved = resolve_unknown(
            &fx.pool,
            &opened.op_id,
            &UnknownDecision::Failed {
                error: "the remote never received it".into(),
            },
            "u-1",
        )
        .unwrap();
        assert_eq!(resolved.status, OperationStatus::Failed);
        release_paths();
    }

    #[test]
    fn the_canonical_input_of_an_operation_is_redacted_in_the_artifact_and_the_event() {
        let _guard = crate::code_studio::paths::test_data_dir_guard();
        let fx = fixture("ws-op-redact");
        let token = "ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8";
        let request = OperationRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: "s-1".into(),
            run_id: None,
            origin_kind: OriginKind::Terminal,
            origin_id: "pty-1#1".into(),
            logical_step: "login".into(),
            op_kind: OpKind::Exec,
            capability: Capability::Exec,
            input: OperationInput::Exec {
                argv: vec![
                    "docker".into(),
                    "login".into(),
                    "--password".into(),
                    token.into(),
                    "ghcr.io".into(),
                ],
                cwd: ".".into(),
                timeout_secs: 60,
            },
            precondition: Precondition::None,
            postcondition: Postcondition::ExitCodeRecorded,
            profile: None,
        };
        let opened = begin(&fx.pool, &request).unwrap();
        let input_ref = opened.input_ref.clone().expect("input artifact");

        let bytes = artifacts::get(&fx.pool, &fx.workspace_id, &input_ref).unwrap();
        assert!(
            !String::from_utf8_lossy(&bytes).contains(token),
            "the artifact kept the password"
        );
        let stored: OperationInput = events::from_cbor(&bytes).unwrap();
        match stored {
            OperationInput::Exec { argv, .. } => {
                assert_eq!(argv[3], crate::code_studio::redact::REDACTED);
                assert_eq!(argv[4], "ghcr.io", "an ordinary argument was destroyed");
            }
            other => panic!("wrong input: {other:?}"),
        }

        // The same operation, recorded as an exec event, is redacted too.
        events::append(
            &fx.pool,
            "s-1",
            SessionEvent::new(
                "exec-1",
                EventPayload::Exec {
                    op_id: opened.op_id.clone(),
                    argv: vec![
                        "docker".into(),
                        "login".into(),
                        "--password".into(),
                        token.into(),
                    ],
                    cwd: ".".into(),
                    exit_code: Some(0),
                    requested_mount_access: "cow".into(),
                    writes_discarded: true,
                },
            ),
        )
        .unwrap();
        let raw: Vec<u8> = {
            let conn = fx.pool.read().unwrap();
            conn.query_row(
                "SELECT payload_cbor FROM session_events WHERE idempotency_key='exec-1'",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert!(!String::from_utf8_lossy(&raw).contains(token));
        release_paths();
    }

    #[test]
    fn completing_an_operation_records_its_result_and_the_matching_event() {
        let _guard = crate::code_studio::paths::test_data_dir_guard();
        let fx = fixture("ws-op-complete");
        let content = b"fn main() {}";
        let opened = begin(&fx.pool, &write_request(&fx, "write", content, None)).unwrap();
        let output = artifacts::put(&fx.pool, &fx.workspace_id, b"exit=0", "exec_result").unwrap();

        let done = complete(
            &fx.pool,
            &opened.op_id,
            &["9f2a1c4b0e5d4a779c318a2b6d4e1f0012345678".to_string()],
            Some(output.sha256.as_str()),
        )
        .unwrap();
        assert_eq!(done.status, OperationStatus::Completed);
        assert_eq!(done.result_oids.len(), 1);
        assert_eq!(done.result_ref.as_deref(), Some(output.sha256.as_str()));

        let refcount: i64 = {
            let conn = fx.pool.read().unwrap();
            conn.query_row(
                "SELECT refcount FROM artifacts WHERE sha256 = ?1",
                rusqlite::params![output.sha256],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(refcount, 1, "the result artifact was not retained");

        let finished = events::read_after(&fx.pool, "s-1", 0, 100)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == EventKind::OperationFinished)
            .count();
        assert_eq!(finished, 1);

        // A completed operation is not reconciled again.
        assert!(reconcile(&fx.pool, "s-1", &probe(&fx))
            .unwrap()
            .entries
            .is_empty());
        release_paths();
    }

    /// A postcondition that cannot hold is refused where it is written down,
    /// not discovered as an `unknown` after the next crash.
    #[test]
    fn a_postcondition_that_can_never_hold_is_refused_when_the_operation_opens() {
        let _guard = crate::code_studio::paths::test_data_dir_guard();
        let fx = fixture("ws-op-impossible");
        let mut request = write_request(&fx, "write", b"fn main() {}", None);

        // The exact shape that was reaching the journal: an empty tree, which
        // `commit_exists` compares against every candidate and never matches.
        request.op_kind = OpKind::GitCommit;
        request.capability = Capability::GitCommit;
        request.postcondition = Postcondition::CommitExists {
            tree: String::new(),
            parent: Some("b".repeat(40)),
        };
        let error = begin(&fx.pool, &request).expect_err("an empty tree must be refused");
        assert!(
            error.to_string().contains("hexadecimal object id"),
            "{error}"
        );

        // And a commit journalled with nothing to verify at all, which §13.1
        // does not allow because a commit's outcome is fully determined.
        request.postcondition = Postcondition::None;
        let error = begin(&fx.pool, &request).expect_err("git_commit needs a postcondition");
        assert!(error.to_string().contains("verifiable outcome"), "{error}");

        // Truncated and non-hex ids are the same mistake in another spelling.
        for bad in ["", "not-hex-at-all", &"a".repeat(39)] {
            request.postcondition = Postcondition::RefEquals {
                refname: "refs/heads/main".into(),
                oid: bad.to_string(),
            };
            assert!(
                begin(&fx.pool, &request).is_err(),
                "accepted an oid of '{bad}'"
            );
        }
        request.postcondition = Postcondition::FileBlobIs {
            path: String::new(),
            sha256: blob_sha(b"x"),
        };
        assert!(begin(&fx.pool, &request).is_err(), "accepted an empty path");

        // Nothing of the refused attempts reached the journal.
        let rows: i64 = {
            let conn = fx.pool.read().unwrap();
            conn.query_row("SELECT COUNT(*) FROM session_operations", [], |row| {
                row.get(0)
            })
            .unwrap()
        };
        assert_eq!(rows, 0, "a refused postcondition still opened a row");
        release_paths();
    }

    /// `ExitCodeRecorded` is satisfied by the result artifact and by nothing
    /// else. Completing without one produced a row that called itself
    /// `completed` while its own postcondition read false, and no artifact of
    /// the command's outcome ever existed.
    #[test]
    fn an_exec_cannot_be_completed_without_the_artifact_that_records_its_outcome() {
        let _guard = crate::code_studio::paths::test_data_dir_guard();
        let fx = fixture("ws-op-exitcode");
        let request = OperationRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: "s-1".into(),
            run_id: None,
            origin_kind: OriginKind::ToolCall,
            origin_id: "call-7".into(),
            logical_step: "run".into(),
            op_kind: OpKind::Exec,
            capability: Capability::Exec,
            input: OperationInput::Exec {
                argv: vec!["cargo".into(), "test".into()],
                cwd: ".".into(),
                timeout_secs: 300,
            },
            precondition: Precondition::None,
            postcondition: Postcondition::ExitCodeRecorded,
            profile: None,
        };
        let opened = begin(&fx.pool, &request).unwrap();

        let error = complete(&fx.pool, &opened.op_id, &[], None)
            .expect_err("an exec completed without recording anything");
        assert!(error.to_string().contains("recorded exit code"), "{error}");
        assert_eq!(
            get(&fx.pool, &opened.op_id).unwrap().unwrap().status,
            OperationStatus::Pending,
            "the row was closed anyway"
        );

        // With the outcome stored, the same call closes the operation and the
        // postcondition it promised is now true.
        let outcome = artifacts::put(
            &fx.pool,
            &fx.workspace_id,
            b"{\"exit_code\":0}",
            "exec_result",
        )
        .unwrap();
        let done = complete(&fx.pool, &opened.op_id, &[], Some(&outcome.sha256)).unwrap();
        assert_eq!(done.status, OperationStatus::Completed);
        assert_eq!(done.result_ref.as_deref(), Some(outcome.sha256.as_str()));
        release_paths();
    }

    #[test]
    fn a_commit_built_before_the_crash_is_recognised_by_its_tree_and_parent() {
        let _guard = crate::code_studio::paths::test_data_dir_guard();
        if !git_available() {
            eprintln!("skipping: git is not installed");
            return;
        }
        let fx = fixture("ws-op-git");
        let root = paths::workspace_dir(&fx.workspace_id).unwrap();
        let broker = Broker::at(&root);
        broker.init_repository("main").expect("init repository");
        let handle = broker.reference();
        let head = broker.head_commit(&handle).unwrap();
        let meta = broker
            .commit_metadata(&handle, &head)
            .unwrap()
            .expect("the initial commit exists");

        let request = OperationRequest {
            workspace_id: fx.workspace_id.clone(),
            session_id: "s-1".into(),
            run_id: None,
            origin_kind: OriginKind::Coordinator,
            origin_id: "saga-commit".into(),
            logical_step: "commit-from-blobs".into(),
            op_kind: OpKind::GitCommit,
            capability: Capability::GitCommit,
            input: OperationInput::Git {
                operation: "commit".into(),
                refname: Some("refs/heads/main".into()),
                remote: None,
                oids: vec![],
            },
            precondition: Precondition::None,
            postcondition: Postcondition::CommitExists {
                tree: meta.tree.clone(),
                parent: None,
            },
            profile: None,
        };
        let opened = begin(&fx.pool, &request).unwrap();
        // The executor journalled the object id git had already produced.
        record_oids(&fx.pool, &opened.op_id, &[head.clone()]).unwrap();

        let git_probe = GitProbe::new(Broker::at(&root), handle.clone());
        let report = reconcile(&fx.pool, "s-1", &git_probe).unwrap();
        assert_eq!(report.entries[0].outcome, ReconcileOutcome::Completed);

        // A ref that does not exist is evidence of absence, not an error.
        assert_eq!(
            git_probe
                .postcondition(
                    &Postcondition::RefEquals {
                        refname: "refs/heads/never-created".into(),
                        oid: head.clone(),
                    },
                    &[],
                )
                .unwrap(),
            Verdict::Unsatisfied
        );
        assert_eq!(
            git_probe
                .postcondition(
                    &Postcondition::RefEquals {
                        refname: "refs/heads/main".into(),
                        oid: head.clone(),
                    },
                    &[],
                )
                .unwrap(),
            Verdict::Satisfied
        );

        // The other half of §13.1, demonstrated on the same repository: move
        // the branch on, and the very same object — still present, still with
        // the right tree and parent — stops being evidence that anything
        // landed. `git commit-tree` writes the object and `git update-ref`
        // moves the branch, so a crash between the two leaves precisely this
        // picture, and reading it as "completed" reports work nobody can see.
        let spec = crate::code_studio::git_broker::CommitSpec {
            base_commit: head.clone(),
            extra_parent: None,
            branch: "main".into(),
            expected_old: Some(head.clone()),
            message: "second".into(),
            author: identity(),
            committer: identity(),
            files: vec![crate::code_studio::git_broker::CommitFile {
                path: "later.txt".into(),
                old_path: None,
                mode: "100644".into(),
                change: crate::code_studio::git_broker::CommitChange::Write {
                    content: b"later\n".to_vec(),
                },
            }],
        };
        Broker::at(&root)
            .build_commit(&handle, &spec)
            .expect("a second commit");
        assert_eq!(
            git_probe
                .postcondition(
                    &Postcondition::CommitExists {
                        tree: meta.tree.clone(),
                        parent: None,
                    },
                    &[head],
                )
                .unwrap(),
            Verdict::Unsatisfied,
            "a commit object no reference points at was still called landed"
        );
        release_paths();
    }

    fn identity() -> crate::code_studio::git_broker::CommitIdentity {
        crate::code_studio::git_broker::CommitIdentity {
            name: "Code Studio".into(),
            email: "code-studio@tentaflow.invalid".into(),
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
    fn idempotence_follows_the_kind_and_every_slug_round_trips() {
        for kind in [
            OpKind::FsWrite,
            OpKind::FsEdit,
            OpKind::FsMkdir,
            OpKind::FsDelete,
            OpKind::Exec,
            OpKind::GitBranch,
            OpKind::GitCommit,
            OpKind::GitPush,
            OpKind::GitMerge,
            OpKind::GitMergeFinalize,
            OpKind::GitWorktree,
        ] {
            assert_eq!(OpKind::from_slug(kind.slug()), Some(kind));
        }
        for kind in [OpKind::Exec, OpKind::GitPush, OpKind::GitMerge] {
            assert!(!kind.idempotent(), "{} must not be idempotent", kind.slug());
        }
        for kind in [
            OpKind::FsWrite,
            OpKind::FsEdit,
            OpKind::FsMkdir,
            OpKind::GitCommit,
        ] {
            assert!(kind.idempotent(), "{} must be idempotent", kind.slug());
        }
        for origin in [
            OriginKind::ToolCall,
            OriginKind::Terminal,
            OriginKind::Ui,
            OriginKind::Shim,
            OriginKind::FlowBlock,
            OriginKind::Coordinator,
        ] {
            assert_eq!(OriginKind::from_slug(origin.slug()), Some(origin));
        }
    }

    /// The producer of a `FileBlobIs` condition and the probe that verifies it
    /// must share ONE definition of the digest. While they did not, every
    /// interrupted write went to a person even though its content was provably
    /// on disk, and the CAS half of the retry path was dead too.
    #[test]
    fn a_file_condition_is_verified_with_the_digest_its_producer_wrote() {
        let root = tempfile::tempdir().unwrap();
        let content = b"fn main() { work(); }";
        std::fs::write(root.path().join("main.rs"), content).unwrap();
        let probe = WorktreeProbe::new(root.path());

        let condition = Postcondition::FileBlobIs {
            path: "main.rs".into(),
            sha256: blob_sha(content),
        };
        assert_eq!(
            probe.postcondition(&condition, &[]).unwrap(),
            Verdict::Satisfied,
            "the probe rejected the very value `code_studio::fs::blob_sha` produces"
        );
        assert_eq!(
            probe
                .precondition(&Precondition::FileBlobIs {
                    path: "main.rs".into(),
                    sha256: blob_sha(content),
                })
                .unwrap(),
            Verdict::Satisfied,
            "the CAS retry path uses the same comparison and must agree"
        );
        // A digest of the raw bytes is a different value and stays a mismatch.
        assert_eq!(
            probe
                .postcondition(
                    &Postcondition::FileBlobIs {
                        path: "main.rs".into(),
                        sha256: sha256_hex(content),
                    },
                    &[]
                )
                .unwrap(),
            Verdict::Unsatisfied
        );
    }

    #[test]
    fn a_worktree_probe_refuses_a_path_that_leaves_the_worktree() {
        let root = tempfile::tempdir().unwrap();
        let probe = WorktreeProbe::new(root.path());
        for bad in ["../outside", "/etc/passwd", "a/../../b"] {
            assert!(
                probe
                    .precondition(&Precondition::FileAbsent { path: bad.into() })
                    .is_err(),
                "accepted path {bad}"
            );
        }
        assert_eq!(
            probe
                .precondition(&Precondition::FileAbsent {
                    path: "src/main.rs".into()
                })
                .unwrap(),
            Verdict::Satisfied
        );
    }
}
