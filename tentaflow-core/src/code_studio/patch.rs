// ===== File: code_studio/patch.rs — patch sets, compare-and-swap, and the accepted-blob commit =====
//
// This module is the integrity boundary of §2.2 pkt 5: what gets committed is
// what a human accepted, and nothing else. Three ideas carry that promise.
//
// **The base is frozen, the present moves.** `patch_base_blob_sha` is the
// content the file had in `base_commit` when the set opened and never changes;
// `current_blob_sha` moves with every recorded edit. A conflict is then an
// observation ("the file is no longer what your operation expected"), not a
// timestamp comparison. The compare-and-swap table of §13.2 falls out of that
// split, including the correction it makes to revision 1.2: only the FIRST edit
// of a file expects the base — every later one expects the current value, so a
// second edit of the same file cannot report a phantom conflict.
//
// **Content lives in the git object database.** A patch set needs a durable,
// content-addressed store for blobs and hunks; the workspace already has one,
// shared by every worktree and surviving restarts. So `patch_base_blob_sha`,
// `current_blob_sha`, `accepted_blob_sha` and `patch_hunks.content_ref` are all
// git object ids written through the broker. There is no second store to keep
// consistent, and `build_commit` re-hashing the accepted content is what proves
// the commit carries exactly the reviewed bytes.
//
// **A decision names the tree it was made about.** `patch_sets.scope` records
// which worktree a set describes — the session's own (§11.5) or the integration
// worktree of ONE merge operation (§11.6 step 4), whose op id the row carries —
// and every selector here filters on it. Without that column "the accepted set
// of this session" had a single answer, so a finalize published the work review
// onto the target branch and the merge result nobody had reviewed came with it.
//
// **The worktree is not a party to the commit.** Deciding a review writes no
// file, and committing reads none. An agent editing during a review neither
// blocks the commit nor enters it: the divergence simply becomes the material
// of the next patch set (§11.5 step 5). Where a decision does imply a file
// change — a partial acceptance drops the rejected hunks from disk — this
// module RETURNS the intent as a `Rewrite` carrying its own precondition,
// instead of reaching into a worktree it does not own.

use anyhow::{anyhow, Result};

use super::git_broker::{
    validate_oid, validate_repo_path, Broker, CommitChange, CommitFile, CommitIdentity,
    CommitOutcome, CommitSpec, RepoHandle, TreeEntry,
};
use crate::db::DbPool;

/// Which worktree a patch set describes. A merge is reviewed on the integration
/// worktree (§11.6 step 4), an ordinary change on the session's own.
///
/// `Merge` carries the id of the merge operation that opened that worktree
/// rather than leaving it beside the scope: a merge review that cannot name the
/// merge it belongs to is exactly what lets a finalize publish some other
/// decision onto the target branch. The scope is stored on the row and every
/// selector filters by it, so "the accepted set" is never ambiguous.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchScope {
    Work,
    Merge { op_id: String },
}

impl PatchScope {
    /// Stored form of the scope, i.e. `patch_sets.scope`.
    pub fn as_str(&self) -> &'static str {
        match self {
            PatchScope::Work => "work",
            PatchScope::Merge { .. } => "merge",
        }
    }

    /// Merge operation this scope belongs to, i.e. `patch_sets.op_id`.
    pub fn op_id(&self) -> Option<&str> {
        match self {
            PatchScope::Work => None,
            PatchScope::Merge { op_id } => Some(op_id.as_str()),
        }
    }

    /// Rebuilds the scope from a row. A merge row without its operation is a
    /// row nothing can bind a finalize to, so it is an error rather than a
    /// silently unscoped set.
    fn from_row(scope: &str, op_id: Option<String>) -> Result<PatchScope> {
        match (scope, op_id) {
            ("work", _) => Ok(PatchScope::Work),
            ("merge", Some(op_id)) if !op_id.is_empty() => Ok(PatchScope::Merge { op_id }),
            ("merge", _) => Err(anyhow!("merge patch set carries no operation id")),
            other => Err(anyhow!("unknown patch set scope '{}'", other.0)),
        }
    }
}

/// State a path is expected to be in before an operation touches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Precondition {
    Absent,
    BlobIs(String),
}

impl Precondition {
    fn describe(&self) -> String {
        match self {
            Precondition::Absent => "absent".to_string(),
            Precondition::BlobIs(oid) => format!("blob {oid}"),
        }
    }
}

/// The operations of the §13.2 table. `Rename` carries its destination because
/// a rename is one operation with two paths, not a delete plus a create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditKind {
    Create,
    Write,
    Delete,
    Rename { new_path: String },
}

#[derive(Debug, Clone)]
pub struct PatchHunk {
    pub id: String,
    pub idx: i64,
    pub header: String,
    /// Object id of the hunk text, including its `@@` header line.
    pub content_ref: String,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct PatchFile {
    pub id: String,
    pub path: String,
    pub change_kind: String,
    pub old_path: Option<String>,
    pub patch_base_blob_sha: Option<String>,
    pub current_blob_sha: Option<String>,
    pub accepted_blob_sha: Option<String>,
    pub git_blob_oid: Option<String>,
    pub mode: String,
    pub status: String,
    pub hunks: Vec<PatchHunk>,
}

impl PatchFile {
    /// State the path is in right now, as the compare-and-swap sees it.
    fn state(&self) -> Precondition {
        if self.change_kind == "delete" {
            return Precondition::Absent;
        }
        match &self.current_blob_sha {
            Some(oid) => Precondition::BlobIs(oid.clone()),
            None => Precondition::Absent,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PatchSet {
    pub id: String,
    pub session_id: String,
    pub run_id: Option<String>,
    pub base_commit: String,
    pub status: String,
    /// The worktree this set describes, read back from the row — a set never
    /// answers "which change did the human accept?" without it.
    pub scope: PatchScope,
    pub files: Vec<PatchFile>,
}

impl PatchSet {
    pub fn file(&self, path: &str) -> Option<&PatchFile> {
        self.files.iter().find(|f| f.path == path)
    }
}

/// What the reviewer decided about one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileVerdict {
    Accept,
    Reject,
    /// Indices of the hunks that were accepted; every other hunk is rejected.
    Hunks(Vec<i64>),
}

#[derive(Debug, Clone)]
pub struct Decisions {
    pub decided_by: String,
    pub files: Vec<(String, FileVerdict)>,
}

/// A file change the decision implies for the worktree. It carries its own
/// precondition so the filesystem layer applies it under the same
/// compare-and-swap the patch set uses, instead of overwriting blindly.
#[derive(Debug, Clone)]
pub struct Rewrite {
    pub path: String,
    /// `None` means the path should end up absent.
    pub blob_oid: Option<String>,
    pub expect: Precondition,
}

#[derive(Debug, Clone)]
pub struct DecisionOutcome {
    pub status: String,
    pub accepted: Vec<String>,
    pub rejected: Vec<String>,
    pub conflicted: Vec<String>,
    pub rewrites: Vec<Rewrite>,
}

/// Everything a commit needs beyond the accepted content itself.
#[derive(Debug, Clone)]
pub struct CommitRequest {
    pub branch: String,
    pub expected_old: Option<String>,
    pub message: String,
    pub author: CommitIdentity,
    pub committer: CommitIdentity,
    /// Second parent, set only when finalising a merge.
    pub extra_parent: Option<String>,
}

const ZERO_OID: &str = "0000000000000000000000000000000000000000";

/// The worktree a scope describes. A merge without its operation id is refused
/// here rather than silently reviewed as the session's own work.
fn worktree_handle(broker: &Broker, session_id: &str, scope: &PatchScope) -> Result<RepoHandle> {
    match scope {
        PatchScope::Work => broker.session(session_id),
        PatchScope::Merge { op_id } => {
            if op_id.is_empty() {
                return Err(anyhow!(
                    "a merge patch set must name the merge operation it reviews"
                ));
            }
            broker.integration(session_id)
        }
    }
}

/// Everything the worktree currently differs from `base_commit` by, as
/// undecided patch-file rows with their hunks.
///
/// The worktree is frozen into a tree object first (`snapshot_worktree`), so
/// the whole scan compares two immutable trees; an agent that keeps editing
/// changes the next snapshot, never this one.
fn scan_worktree(
    broker: &Broker,
    handle: &RepoHandle,
    base_commit: &str,
) -> Result<Vec<PatchFile>> {
    let tree = broker.snapshot_worktree(handle, base_commit)?;
    let entries = broker.diff_name_status(handle, base_commit, &tree)?;

    let snapshot: Vec<_> = broker.list_tree(handle, &tree)?;
    let base_tree: Vec<_> = broker.list_tree(handle, base_commit)?;
    let find = |list: &[TreeEntry], path: &str| {
        list.iter()
            .find(|e| e.path == path)
            .map(|e| (e.mode.clone(), e.oid.clone()))
    };

    let mut files: Vec<PatchFile> = Vec::new();
    for entry in &entries {
        validate_repo_path(&entry.path)?;
        let (change_kind, mode, base_blob, current_blob) = match entry.status {
            'A' => {
                let (mode, oid) = find(&snapshot, &entry.path)
                    .ok_or_else(|| anyhow!("added path {} is not in the snapshot", entry.path))?;
                ("add", mode, None, Some(oid))
            }
            'D' => {
                let (mode, oid) = find(&base_tree, &entry.path).ok_or_else(|| {
                    anyhow!("deleted path {} is not in the base tree", entry.path)
                })?;
                ("delete", mode, Some(oid), None)
            }
            'M' | 'T' => {
                let (mode, oid) = find(&snapshot, &entry.path).ok_or_else(|| {
                    anyhow!("modified path {} is not in the snapshot", entry.path)
                })?;
                let base = find(&base_tree, &entry.path).map(|(_, oid)| oid);
                ("modify", mode, base, Some(oid))
            }
            other => {
                return Err(anyhow!(
                    "diff status '{other}' is not reviewable for {}",
                    entry.path
                ))
            }
        };
        // A symlink or a submodule pointer is content whose meaning is resolved
        // outside the bytes under review, so it is refused here rather than
        // shown as text and committed as a link.
        if mode != "100644" && mode != "100755" {
            return Err(anyhow!(
                "{} has mode {mode}; Code Studio reviews regular files only",
                entry.path
            ));
        }

        let hunks = if change_kind == "delete" {
            Vec::new()
        } else {
            let diff = broker.diff_patch(handle, base_commit, &tree, &entry.path)?;
            split_hunks(&diff)
                .into_iter()
                .enumerate()
                .map(|(idx, text)| -> Result<PatchHunk> {
                    let header = text.lines().next().unwrap_or_default().to_string();
                    let content_ref = broker.hash_object(handle, text.as_bytes())?;
                    Ok(PatchHunk {
                        id: uuid::Uuid::new_v4().to_string(),
                        idx: idx as i64,
                        header,
                        content_ref,
                        status: "pending".into(),
                    })
                })
                .collect::<Result<Vec<_>>>()?
        };

        files.push(PatchFile {
            id: uuid::Uuid::new_v4().to_string(),
            path: entry.path.clone(),
            change_kind: change_kind.to_string(),
            old_path: None,
            patch_base_blob_sha: base_blob,
            current_blob_sha: current_blob,
            accepted_blob_sha: None,
            git_blob_oid: None,
            mode,
            status: "pending".into(),
            hunks,
        });
    }
    Ok(files)
}

/// Recomputes an undecided set against the worktree it describes, keeping its
/// id and its frozen `base_commit`.
///
/// `record_edit` journals the effects that go THROUGH our tools. A vendor CLI
/// driven by `delegate_cli` writes to the worktree with its own file calls, so
/// nothing journals them and the set opened before the turn stays empty — the
/// delegated work would reach a review with nothing in it. Rescanning is how
/// that work becomes reviewable, and it has to keep the base: the base was
/// frozen before the delegation started precisely so the set describes what the
/// delegation changed rather than whatever the branch has drifted to since.
///
/// Only `pending` rows are recomputed. A file a person already accepted or
/// rejected keeps that verdict and its `accepted_blob_sha` — §11.5 step 5 says
/// a later divergence is the material of the NEXT set, not a reason to rewrite
/// a decision — and a `conflicted` row is an unresolved review question, not an
/// observation to refresh away. A pending row whose path no longer differs from
/// the base is dropped: the change was undone before anyone looked at it.
pub fn rescan_patch_set(pool: &DbPool, broker: &Broker, patch_set_id: &str) -> Result<PatchSet> {
    let set = load_patch_set(pool, patch_set_id)?;
    if !matches!(set.status.as_str(), "open" | "in_review" | "conflicted") {
        return Err(anyhow!(
            "patch set {patch_set_id} is '{}' and is not open to new material; open a new one",
            set.status
        ));
    }
    let handle = worktree_handle(broker, &set.session_id, &set.scope)?;
    let scanned = scan_worktree(broker, &handle, &set.base_commit)?;

    let decided: Vec<&PatchFile> = set.files.iter().filter(|f| f.status != "pending").collect();

    let mut conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM patch_files WHERE patch_set_id = ?1 AND status = 'pending'",
        rusqlite::params![patch_set_id],
    )?;
    for file in &scanned {
        if decided.iter().any(|kept| kept.path == file.path) {
            continue;
        }
        tx.execute(
            "INSERT INTO patch_files (id, patch_set_id, path, change_kind, old_path, \
              patch_base_blob_sha, current_blob_sha, mode, status) \
             VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, 'pending')",
            rusqlite::params![
                file.id,
                patch_set_id,
                file.path,
                file.change_kind,
                file.patch_base_blob_sha,
                file.current_blob_sha,
                file.mode,
            ],
        )?;
        for hunk in &file.hunks {
            tx.execute(
                "INSERT INTO patch_hunks (id, patch_file_id, idx, header, content_ref, status) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 'pending')",
                rusqlite::params![hunk.id, file.id, hunk.idx, hunk.header, hunk.content_ref],
            )?;
        }
    }
    // A set nobody has decided anything in stays `open`; one that already
    // carries verdicts is re-derived, because dropping and adding pending rows
    // can be what completes or reopens it.
    if set.status != "open" {
        let status = set_status(&tx, patch_set_id)?;
        tx.execute(
            "UPDATE patch_sets SET status = ?2 WHERE id = ?1",
            rusqlite::params![patch_set_id, status],
        )?;
    }
    tx.commit()?;
    drop(conn);

    load_patch_set(pool, patch_set_id)
}

/// Opens a patch set over the difference between `base_commit` and the current
/// worktree.
///
/// The worktree is frozen into a tree object first (`snapshot_worktree`), so
/// the review compares two immutable trees; an agent that keeps editing changes
/// the next snapshot, never this one. Any still-open set of the SAME SCOPE is
/// superseded — two live sets would both claim to describe the same directory —
/// while a set of the other scope is left alone, because a merge under review
/// and the session's own work are two different trees waiting for two different
/// decisions. An ACCEPTED set is left alone too: it is waiting for its commit,
/// and that commit no longer depends on the worktree at all.
pub fn open_patch_set(
    pool: &DbPool,
    broker: &Broker,
    session_id: &str,
    run_id: Option<&str>,
    base_commit: &str,
    scope: &PatchScope,
) -> Result<PatchSet> {
    let handle = worktree_handle(broker, session_id, scope)?;
    let files = scan_worktree(broker, &handle, base_commit)?;
    let patch_set_id = uuid::Uuid::new_v4().to_string();

    // A clean tree gets a TRANSIENT set: nothing is written. The harness runs a
    // review at the end of every turn, and most turns change nothing (a
    // question, an explanation, a failed search) — persisting those would fill
    // the Changes tab with empty rows and supersede an accepted set still
    // waiting for its commit. Callers already treat an empty set as "nothing to
    // review" and return early, so the row buys nobody anything.
    if files.is_empty() {
        return Ok(PatchSet {
            id: patch_set_id,
            session_id: session_id.to_string(),
            run_id: run_id.map(str::to_string),
            base_commit: base_commit.to_string(),
            status: "open".to_string(),
            scope: scope.clone(),
            files,
        });
    }

    let mut conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    let tx = conn.transaction()?;
    tx.execute(
        "UPDATE patch_sets SET status='superseded' \
         WHERE session_id = ?1 AND scope = ?2 AND op_id IS ?3 \
           AND status IN ('open','in_review')",
        rusqlite::params![session_id, scope.as_str(), scope.op_id()],
    )?;
    tx.execute(
        "INSERT INTO patch_sets \
          (id, session_id, run_id, base_commit, status, created_at, scope, op_id) \
         VALUES (?1, ?2, ?3, ?4, 'open', datetime('now'), ?5, ?6)",
        rusqlite::params![
            patch_set_id,
            session_id,
            run_id,
            base_commit,
            scope.as_str(),
            scope.op_id(),
        ],
    )?;
    for file in &files {
        tx.execute(
            "INSERT INTO patch_files (id, patch_set_id, path, change_kind, old_path, \
              patch_base_blob_sha, current_blob_sha, mode, status) \
             VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, 'pending')",
            rusqlite::params![
                file.id,
                patch_set_id,
                file.path,
                file.change_kind,
                file.patch_base_blob_sha,
                file.current_blob_sha,
                file.mode,
            ],
        )?;
        for hunk in &file.hunks {
            tx.execute(
                "INSERT INTO patch_hunks (id, patch_file_id, idx, header, content_ref, status) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 'pending')",
                rusqlite::params![hunk.id, file.id, hunk.idx, hunk.header, hunk.content_ref],
            )?;
        }
    }
    tx.commit()?;
    drop(conn);

    load_patch_set(pool, &patch_set_id)
}

/// Reads a patch set with its files and hunks.
pub fn load_patch_set(pool: &DbPool, patch_set_id: &str) -> Result<PatchSet> {
    let conn = pool.read().map_err(|e| anyhow!("workspace db read: {e}"))?;
    let (session_id, run_id, base_commit, status, scope, op_id) = conn
        .query_row(
            "SELECT session_id, run_id, base_commit, status, scope, op_id \
             FROM patch_sets WHERE id = ?1",
            rusqlite::params![patch_set_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            },
        )
        .map_err(|_| anyhow!("patch set {patch_set_id} not found"))?;
    let scope = PatchScope::from_row(&scope, op_id)?;

    let mut stmt = conn.prepare(
        "SELECT id, path, change_kind, old_path, patch_base_blob_sha, current_blob_sha, \
          accepted_blob_sha, git_blob_oid, mode, status \
         FROM patch_files WHERE patch_set_id = ?1 ORDER BY path",
    )?;
    let mut files = stmt
        .query_map(rusqlite::params![patch_set_id], |row| {
            Ok(PatchFile {
                id: row.get(0)?,
                path: row.get(1)?,
                change_kind: row.get(2)?,
                old_path: row.get(3)?,
                patch_base_blob_sha: row.get(4)?,
                current_blob_sha: row.get(5)?,
                accepted_blob_sha: row.get(6)?,
                git_blob_oid: row.get(7)?,
                mode: row.get(8)?,
                status: row.get(9)?,
                hunks: Vec::new(),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut hunk_stmt = conn.prepare(
        "SELECT id, idx, header, content_ref, status FROM patch_hunks \
         WHERE patch_file_id = ?1 ORDER BY idx",
    )?;
    for file in &mut files {
        file.hunks = hunk_stmt
            .query_map(rusqlite::params![file.id], |row| {
                Ok(PatchHunk {
                    id: row.get(0)?,
                    idx: row.get(1)?,
                    header: row.get(2)?,
                    content_ref: row.get(3)?,
                    status: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
    }

    Ok(PatchSet {
        id: patch_set_id.to_string(),
        session_id,
        run_id,
        base_commit,
        status,
        scope,
        files,
    })
}

/// Loads a set the caller named by id, refusing one that belongs to another
/// session or to another scope.
///
/// The dashboard and the mesh both carry a patch set id on the wire, and an id
/// is not an authorisation: a work review of this session — or any review of
/// another one — must not be actionable where a merge review is required.
pub fn load_patch_set_for(
    pool: &DbPool,
    session_id: &str,
    scope: &PatchScope,
    patch_set_id: &str,
) -> Result<PatchSet> {
    let set = load_patch_set(pool, patch_set_id)?;
    if set.session_id != session_id || &set.scope != scope {
        return Err(anyhow!(
            "patch set {patch_set_id} does not belong to this session's {} review",
            scope.as_str()
        ));
    }
    Ok(set)
}

/// The still-open set of ONE scope, or `None` when that scope has none.
///
/// A session can hold a work set and a merge set at the same time — the agent
/// keeps editing its worktree while a merge result waits for review — so "the
/// open set of this session" is not a question with one answer, and answering
/// it unscoped is what let a merge finalize act on a work review.
pub fn open_patch_set_for_scope(
    pool: &DbPool,
    session_id: &str,
    scope: &PatchScope,
) -> Result<Option<PatchSet>> {
    let id: Option<String> = {
        let conn = pool.read().map_err(|e| anyhow!("workspace db read: {e}"))?;
        conn.query_row(
            "SELECT id FROM patch_sets WHERE session_id = ?1 AND scope = ?2 AND op_id IS ?3 \
             AND status IN ('open','in_review','conflicted') ORDER BY created_at DESC LIMIT 1",
            rusqlite::params![session_id, scope.as_str(), scope.op_id()],
            |row| row.get(0),
        )
        .ok()
    };
    match id {
        Some(id) => Ok(Some(load_patch_set(pool, &id)?)),
        None => Ok(None),
    }
}

/// The decision a commit may act on (gate 5a of §9.3), for ONE scope.
///
/// Scoped to the session because an acceptance given elsewhere is another
/// person's decision about another branch, and to the scope because an
/// acceptance given for the session branch is a decision about a different tree
/// than the merge result §11.6 step 5 requires a human to see. For a merge the
/// scope carries the operation id, so a finalize resolves its decision from the
/// merge it holds instead of from an id on the wire.
pub fn accepted_patch_set_for_scope(
    pool: &DbPool,
    session_id: &str,
    scope: &PatchScope,
) -> Result<PatchSet> {
    let id: Option<String> = {
        let conn = pool.read().map_err(|e| anyhow!("workspace db read: {e}"))?;
        conn.query_row(
            "SELECT id FROM patch_sets WHERE session_id = ?1 AND scope = ?2 AND op_id IS ?3 \
             AND status IN ('accepted','partially_accepted') ORDER BY created_at DESC LIMIT 1",
            rusqlite::params![session_id, scope.as_str(), scope.op_id()],
            |row| row.get(0),
        )
        .ok()
    };
    let id = id.ok_or_else(|| {
        anyhow!(
            "no accepted {} review is available for this session; the change must be reviewed first",
            scope.as_str()
        )
    })?;
    load_patch_set(pool, &id)
}

/// Records one filesystem operation against a patch set under the
/// compare-and-swap of §13.2.
///
/// The expectation is DERIVED, not trusted: it is the state the path is in
/// according to the patch set, falling back to the frozen base for a file the
/// set has not touched yet. `expect` is what the caller believed, and the two
/// disagreeing is exactly the concurrent-edit case — the file becomes
/// `conflicted` and the decision moves to the whole-file level.
///
/// The broker is needed because `Absent` has to mean absent from the BASE TREE
/// too. `patch_files` only lists changed paths, so a table-only check would
/// happily let a create land on top of an untouched tracked file.
pub fn record_edit(
    pool: &DbPool,
    broker: &Broker,
    patch_set_id: &str,
    path: &str,
    kind: EditKind,
    new_blob_sha: Option<&str>,
    expect: &Precondition,
) -> Result<()> {
    validate_repo_path(path)?;
    if let Some(oid) = new_blob_sha {
        validate_oid(oid)?;
    }
    let set = load_patch_set(pool, patch_set_id)?;
    if set.status == "superseded" {
        return Err(anyhow!(
            "patch set {patch_set_id} was already consumed; open a new one"
        ));
    }
    let handle = broker.reference();

    let existing = set.file(path).cloned();
    let actual = match &existing {
        Some(file) => file.state(),
        None => match broker.blob_in_commit(&handle, &set.base_commit, path)? {
            Some(oid) => Precondition::BlobIs(oid),
            None => Precondition::Absent,
        },
    };
    if actual != *expect {
        if let Some(file) = &existing {
            mark_conflicted(pool, patch_set_id, &file.id)?;
        }
        return Err(anyhow!(
            "precondition failed for {path}: expected {}, found {}",
            expect.describe(),
            actual.describe()
        ));
    }

    let creating = matches!(kind, EditKind::Create);
    match kind {
        EditKind::Create | EditKind::Write => {
            let oid = new_blob_sha
                .ok_or_else(|| anyhow!("a write needs the resulting blob id"))?
                .to_string();
            if creating && actual != Precondition::Absent {
                return Err(anyhow!("{path} already exists; create expects it absent"));
            }
            if !creating && actual == Precondition::Absent {
                return Err(anyhow!("{path} is absent; write expects existing content"));
            }
            match existing {
                Some(file) => update_current(pool, &file, Some(&oid))?,
                None => {
                    let base = broker.blob_in_commit(&handle, &set.base_commit, path)?;
                    let change_kind = if base.is_some() { "modify" } else { "add" };
                    insert_file(pool, patch_set_id, path, change_kind, None, base, Some(oid))?;
                }
            }
        }
        EditKind::Delete => match existing {
            // A file this set created has nothing to delete in the base tree:
            // dropping the row leaves the set proposing nothing for that path.
            Some(file) if file.patch_base_blob_sha.is_none() => {
                delete_file_row(pool, &file.id)?;
            }
            Some(file) => {
                let conn = pool
                    .write()
                    .map_err(|e| anyhow!("workspace db write: {e}"))?;
                conn.execute(
                    "UPDATE patch_files SET current_blob_sha = NULL, change_kind = 'delete', \
                      accepted_blob_sha = NULL, status = 'pending' WHERE id = ?1",
                    rusqlite::params![file.id],
                )?;
            }
            None => {
                let base = broker.blob_in_commit(&handle, &set.base_commit, path)?;
                insert_file(pool, patch_set_id, path, "delete", None, base, None)?;
            }
        },
        EditKind::Rename { new_path } => {
            validate_repo_path(&new_path)?;
            if new_path == path {
                return Err(anyhow!("a rename must change the path"));
            }
            let target = set.file(&new_path).cloned();
            let target_state = match &target {
                Some(file) => file.state(),
                None => match broker.blob_in_commit(&handle, &set.base_commit, &new_path)? {
                    Some(oid) => Precondition::BlobIs(oid),
                    None => Precondition::Absent,
                },
            };
            if target_state != Precondition::Absent {
                return Err(anyhow!(
                    "rename target {new_path} is not absent: it is {}",
                    target_state.describe()
                ));
            }
            // The only row that can sit on an absent target is a pending
            // delete; the rename landing there subsumes it.
            if let Some(file) = target {
                delete_file_row(pool, &file.id)?;
            }
            match existing {
                Some(file) => {
                    let content = new_blob_sha
                        .map(str::to_string)
                        .or_else(|| file.current_blob_sha.clone());
                    // A file created inside this set has no base entry to
                    // remove, so moving it is still an add under a new name.
                    let (change_kind, old_path) = match &file.patch_base_blob_sha {
                        Some(_) => ("rename", file.old_path.clone().or(Some(file.path.clone()))),
                        None => (file.change_kind.as_str(), None),
                    };
                    let conn = pool
                        .write()
                        .map_err(|e| anyhow!("workspace db write: {e}"))?;
                    conn.execute(
                        "UPDATE patch_files SET path = ?2, old_path = ?3, change_kind = ?4, \
                          current_blob_sha = ?5, accepted_blob_sha = NULL, status = 'pending' \
                         WHERE id = ?1",
                        rusqlite::params![file.id, new_path, old_path, change_kind, content],
                    )?;
                }
                None => {
                    let base = broker.blob_in_commit(&handle, &set.base_commit, path)?;
                    let content = new_blob_sha.map(str::to_string).or_else(|| base.clone());
                    insert_file(
                        pool,
                        patch_set_id,
                        &new_path,
                        "rename",
                        Some(path),
                        base,
                        content,
                    )?;
                }
            }
        }
    }
    Ok(())
}

/// Applies the reviewer's verdicts.
///
/// A whole-file acceptance takes `current_blob_sha` as-is. A partial one
/// composes the accepted hunks THREE-WAY onto the frozen `patch_base_blob_sha`,
/// in a temporary index, and stores the result as `accepted_blob_sha`. An
/// unclean composition — overlapping contexts — is reported as `conflicted` and
/// pushed back to a whole-file decision, never guessed at (§13.2).
pub fn decide(
    pool: &DbPool,
    broker: &Broker,
    patch_set_id: &str,
    decisions: &Decisions,
) -> Result<DecisionOutcome> {
    let set = load_patch_set(pool, patch_set_id)?;
    if set.status == "superseded" {
        return Err(anyhow!(
            "patch set {patch_set_id} was already consumed and cannot be re-decided"
        ));
    }
    let handle = broker.reference();

    struct Resolved {
        file: PatchFile,
        status: &'static str,
        accepted: Option<String>,
        accepted_hunks: Vec<i64>,
    }

    let mut resolved: Vec<Resolved> = Vec::new();
    for (path, verdict) in &decisions.files {
        let file = set
            .file(path)
            .cloned()
            .ok_or_else(|| anyhow!("{path} is not part of patch set {patch_set_id}"))?;
        let all: Vec<i64> = file.hunks.iter().map(|h| h.idx).collect();
        let verdict = normalise(verdict, &all);
        resolved.push(match verdict {
            FileVerdict::Accept => Resolved {
                status: "accepted",
                accepted: file.current_blob_sha.clone(),
                accepted_hunks: all,
                file,
            },
            FileVerdict::Reject => Resolved {
                status: "rejected",
                accepted: None,
                accepted_hunks: Vec::new(),
                file,
            },
            FileVerdict::Hunks(selected) => {
                match compose_partial(broker, &handle, &set, &file, &selected)? {
                    Some(oid) => Resolved {
                        status: "partially_accepted",
                        accepted: Some(oid),
                        accepted_hunks: selected,
                        file,
                    },
                    None => Resolved {
                        status: "conflicted",
                        accepted: None,
                        accepted_hunks: Vec::new(),
                        file,
                    },
                }
            }
        });
    }

    let mut outcome = DecisionOutcome {
        status: set.status.clone(),
        accepted: Vec::new(),
        rejected: Vec::new(),
        conflicted: Vec::new(),
        rewrites: Vec::new(),
    };

    let mut conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    let tx = conn.transaction()?;
    for item in &resolved {
        // The write is conditional on the value the decision was computed
        // from: a file that moved between the review and the verdict must not
        // silently inherit somebody else's content.
        let changed = tx.execute(
            "UPDATE patch_files SET accepted_blob_sha = ?2, status = ?3 \
             WHERE id = ?1 AND current_blob_sha IS ?4",
            rusqlite::params![
                item.file.id,
                item.accepted,
                item.status,
                item.file.current_blob_sha,
            ],
        )?;
        if changed == 0 {
            tx.execute(
                "UPDATE patch_files SET accepted_blob_sha = NULL, status = 'conflicted' \
                 WHERE id = ?1",
                rusqlite::params![item.file.id],
            )?;
            outcome.conflicted.push(item.file.path.clone());
            continue;
        }

        for hunk in &item.file.hunks {
            let status = if item.status == "conflicted" {
                "pending"
            } else if item.accepted_hunks.contains(&hunk.idx) {
                "accepted"
            } else {
                "rejected"
            };
            tx.execute(
                "UPDATE patch_hunks SET status = ?2 WHERE id = ?1",
                rusqlite::params![hunk.id, status],
            )?;
        }

        match item.status {
            "accepted" => outcome.accepted.push(item.file.path.clone()),
            "rejected" => {
                outcome.rejected.push(item.file.path.clone());
                // A rejected whole file leaves the disk for the same reason a
                // rejected hunk does. The worktree is what the NEXT patch set
                // is measured against (§11.5 step 5), so a rejected change left
                // lying there is re-proposed after every commit — the reviewer
                // says no once and the same change comes back for as long as
                // the session lasts.
                outcome.rewrites.extend(undo_rewrites(&item.file));
            }
            "conflicted" => outcome.conflicted.push(item.file.path.clone()),
            _ => {
                outcome.accepted.push(item.file.path.clone());
                // A partial acceptance keeps the file and drops the hunks
                // nobody took, so the tree has to end up holding the composed
                // content. The intent is returned, not executed — this module
                // does not own the worktree.
                outcome.rewrites.push(Rewrite {
                    path: item.file.path.clone(),
                    blob_oid: item.accepted.clone(),
                    expect: item.file.state(),
                });
            }
        }
    }

    let status = set_status(&tx, patch_set_id)?;
    if status == "open" || status == "in_review" {
        tx.execute(
            "UPDATE patch_sets SET status = ?2 WHERE id = ?1",
            rusqlite::params![patch_set_id, status],
        )?;
    } else {
        tx.execute(
            "UPDATE patch_sets SET status = ?2, decided_by = ?3, decided_at = datetime('now') \
             WHERE id = ?1",
            rusqlite::params![patch_set_id, status, decisions.decided_by],
        )?;
    }
    tx.commit()?;
    outcome.status = status;
    Ok(outcome)
}

/// Turns the accepted files into the material `build_commit` assembles.
///
/// Rejected and still-pending files are simply absent, so the base tree keeps
/// its content for them. Nothing here reads the worktree.
pub fn accepted_commit_spec(
    pool: &DbPool,
    broker: &Broker,
    patch_set_id: &str,
    request: &CommitRequest,
) -> Result<CommitSpec> {
    let set = load_patch_set(pool, patch_set_id)?;
    let handle = broker.reference();
    let mut files = Vec::new();
    for file in &set.files {
        if file.status != "accepted" && file.status != "partially_accepted" {
            continue;
        }
        if file.change_kind == "delete" {
            files.push(CommitFile {
                path: file.path.clone(),
                old_path: None,
                mode: file.mode.clone(),
                change: CommitChange::Delete,
            });
            continue;
        }
        let accepted = file
            .accepted_blob_sha
            .as_deref()
            .ok_or_else(|| anyhow!("{} is accepted but carries no blob", file.path))?;
        files.push(CommitFile {
            path: file.path.clone(),
            old_path: file.old_path.clone(),
            mode: file.mode.clone(),
            change: CommitChange::Write {
                content: broker.cat_file(&handle, accepted)?,
            },
        });
    }
    Ok(CommitSpec {
        base_commit: set.base_commit,
        extra_parent: request.extra_parent.clone(),
        branch: request.branch.clone(),
        expected_old: request.expected_old.clone(),
        message: request.message.clone(),
        author: request.author.clone(),
        committer: request.committer.clone(),
        files,
    })
}

/// Gate 5a of §9.3: does this session hold a decision the commit of THIS scope
/// may act on?
///
/// Scoped to the session on purpose — an acceptance given in another session is
/// another person's decision about another branch — and to the scope, because
/// unlocking a merge finalize with a work acceptance publishes a tree nobody
/// reviewed onto the target branch.
pub fn has_accepted_patch_set(
    pool: &DbPool,
    session_id: &str,
    scope: &PatchScope,
) -> Result<bool> {
    let conn = pool.read().map_err(|e| anyhow!("workspace db read: {e}"))?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM patch_sets \
         WHERE session_id = ?1 AND scope = ?2 AND op_id IS ?3 \
           AND status IN ('accepted','partially_accepted')",
        rusqlite::params![session_id, scope.as_str(), scope.op_id()],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Closes a patch set after its commit landed, recording the object id each
/// path actually got. From here on the worktree's divergence from the new HEAD
/// is the material of the NEXT patch set (§11.5 step 5).
pub fn mark_consumed(pool: &DbPool, patch_set_id: &str, outcome: &CommitOutcome) -> Result<()> {
    let mut conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    let tx = conn.transaction()?;
    for (path, oid) in &outcome.blob_oids {
        tx.execute(
            "UPDATE patch_files SET git_blob_oid = ?3 WHERE patch_set_id = ?1 AND path = ?2",
            rusqlite::params![patch_set_id, path, oid],
        )?;
    }
    let changed = tx.execute(
        "UPDATE patch_sets SET status = 'superseded' WHERE id = ?1 \
         AND status IN ('accepted','partially_accepted')",
        rusqlite::params![patch_set_id],
    )?;
    if changed == 0 {
        return Err(anyhow!(
            "patch set {patch_set_id} was not in an accepted state"
        ));
    }
    tx.commit()?;
    Ok(())
}

/// Drops one path's proposal and restores the frozen base content.
///
/// The returned rewrites carry the precondition the filesystem layer must hold
/// to: reverting a file somebody has since changed again is a conflict, not an
/// overwrite.
pub fn revert(
    pool: &DbPool,
    broker: &Broker,
    patch_set_id: &str,
    path: &str,
) -> Result<Vec<Rewrite>> {
    validate_repo_path(path)?;
    let set = load_patch_set(pool, patch_set_id)?;
    if set.status == "superseded" {
        return Err(anyhow!(
            "patch set {patch_set_id} was already consumed; there is nothing to revert"
        ));
    }
    let file = set
        .file(path)
        .cloned()
        .ok_or_else(|| anyhow!("{path} is not part of patch set {patch_set_id}"))?;
    // Promising a restore whose content is no longer in the object database
    // would leave the caller writing nothing and believing it reverted.
    if let Some(base) = &file.patch_base_blob_sha {
        if !broker.object_exists(&broker.reference(), base)? {
            return Err(anyhow!(
                "the frozen base content of {path} is no longer in the repository"
            ));
        }
    }

    let rewrites = undo_rewrites(&file);
    delete_file_row(pool, &file.id)?;
    Ok(rewrites)
}

/// The worktree writes that undo one proposal: the path goes back to the
/// content frozen when the set opened, or disappears when the base had none.
///
/// Shared by an explicit revert and by a rejection, because they ask the
/// filesystem for exactly the same thing — the difference between them is who
/// decided and what the journal records, not what the tree ends up holding.
fn undo_rewrites(file: &PatchFile) -> Vec<Rewrite> {
    let mut rewrites = vec![Rewrite {
        path: file.path.clone(),
        blob_oid: file.patch_base_blob_sha.clone(),
        expect: file.state(),
    }];
    if let Some(old_path) = &file.old_path {
        // A rename has to be undone on both sides: the new path goes away and
        // the original one comes back with its frozen content.
        rewrites[0].blob_oid = None;
        rewrites.push(Rewrite {
            path: old_path.clone(),
            blob_oid: file.patch_base_blob_sha.clone(),
            expect: Precondition::Absent,
        });
    }
    rewrites
}

// ----- internals -----------------------------------------------------------

/// Collapses a hunk selection that covers everything or nothing into the
/// equivalent whole-file verdict, so the composition step only runs when a
/// genuine subset was chosen.
fn normalise(verdict: &FileVerdict, all: &[i64]) -> FileVerdict {
    let FileVerdict::Hunks(selected) = verdict else {
        return verdict.clone();
    };
    let mut selected: Vec<i64> = selected.clone();
    selected.sort_unstable();
    selected.dedup();
    if selected.is_empty() {
        return FileVerdict::Reject;
    }
    if !all.is_empty() && selected.len() == all.len() && all.iter().all(|i| selected.contains(i)) {
        return FileVerdict::Accept;
    }
    FileVerdict::Hunks(selected)
}

/// Composes the selected hunks onto the frozen base. `None` is the unclean
/// composition of §13.2.
fn compose_partial(
    broker: &Broker,
    handle: &RepoHandle,
    set: &PatchSet,
    file: &PatchFile,
    selected: &[i64],
) -> Result<Option<String>> {
    // A delete or a rename has no meaningful subset: the change is the path
    // itself, not a set of lines. Rather than guess which half was meant, the
    // file goes back to a whole-file decision.
    if file.change_kind == "delete" || file.change_kind == "rename" || file.hunks.is_empty() {
        return Ok(None);
    }
    let current = match &file.current_blob_sha {
        Some(oid) => oid.clone(),
        None => return Ok(None),
    };

    let mut patch = match &file.patch_base_blob_sha {
        Some(base) => format!(
            "diff --git a/{path} b/{path}\nindex {base}..{current} {mode}\n--- a/{path}\n+++ b/{path}\n",
            path = file.path,
            mode = file.mode,
        ),
        None => format!(
            "diff --git a/{path} b/{path}\nnew file mode {mode}\nindex {ZERO_OID}..{current}\n\
             --- /dev/null\n+++ b/{path}\n",
            path = file.path,
            mode = file.mode,
        ),
    };
    for hunk in &file.hunks {
        if !selected.contains(&hunk.idx) {
            continue;
        }
        let text = broker.cat_file(handle, &hunk.content_ref)?;
        patch.push_str(&String::from_utf8_lossy(&text));
    }
    broker.apply_hunks(handle, &set.base_commit, &file.path, &patch)
}

/// Splits a unified diff into hunks, each one keeping its `@@` header line.
/// Text before the first `@@` is the file header, which is rebuilt from the
/// stored blob ids rather than replayed; a binary difference has no `@@` at all
/// and therefore no hunks, leaving only whole-file decisions.
fn split_hunks(diff: &str) -> Vec<String> {
    let mut hunks: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    for line in diff.split_inclusive('\n') {
        if line.starts_with("@@") {
            if let Some(done) = current.take() {
                hunks.push(done);
            }
            current = Some(String::new());
        }
        if let Some(buffer) = current.as_mut() {
            buffer.push_str(line);
        }
    }
    if let Some(done) = current {
        hunks.push(done);
    }
    hunks
}

/// Status of the whole set, derived from its files. An empty set stays `open`:
/// there is nothing to accept, and calling it accepted would open gate 5a for a
/// commit with no content.
fn set_status(tx: &rusqlite::Transaction<'_>, patch_set_id: &str) -> Result<String> {
    let mut stmt = tx.prepare(
        "SELECT status, COUNT(*) FROM patch_files WHERE patch_set_id = ?1 GROUP BY status",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![patch_set_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let count = |name: &str| {
        rows.iter()
            .find(|(status, _)| status == name)
            .map(|(_, n)| *n)
            .unwrap_or(0)
    };
    let total: i64 = rows.iter().map(|(_, n)| *n).sum();
    if total == 0 {
        return Ok("open".to_string());
    }
    if count("conflicted") > 0 {
        return Ok("conflicted".to_string());
    }
    if count("pending") > 0 {
        return Ok("in_review".to_string());
    }
    if count("accepted") == total {
        return Ok("accepted".to_string());
    }
    if count("rejected") == total {
        return Ok("rejected".to_string());
    }
    Ok("partially_accepted".to_string())
}

fn insert_file(
    pool: &DbPool,
    patch_set_id: &str,
    path: &str,
    change_kind: &str,
    old_path: Option<&str>,
    base: Option<String>,
    current: Option<String>,
) -> Result<()> {
    let conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    conn.execute(
        "INSERT INTO patch_files (id, patch_set_id, path, change_kind, old_path, \
          patch_base_blob_sha, current_blob_sha, mode, status) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '100644', 'pending')",
        rusqlite::params![
            uuid::Uuid::new_v4().to_string(),
            patch_set_id,
            path,
            change_kind,
            old_path,
            base,
            current,
        ],
    )?;
    Ok(())
}

fn update_current(pool: &DbPool, file: &PatchFile, current: Option<&str>) -> Result<()> {
    let conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    // A new decision is needed once the content moves, so any earlier
    // acceptance of THIS file is dropped along with it.
    conn.execute(
        "UPDATE patch_files SET current_blob_sha = ?2, accepted_blob_sha = NULL, \
          status = 'pending' WHERE id = ?1",
        rusqlite::params![file.id, current],
    )?;
    Ok(())
}

fn delete_file_row(pool: &DbPool, file_id: &str) -> Result<()> {
    let conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    conn.execute(
        "DELETE FROM patch_files WHERE id = ?1",
        rusqlite::params![file_id],
    )?;
    Ok(())
}

fn mark_conflicted(pool: &DbPool, patch_set_id: &str, file_id: &str) -> Result<()> {
    let conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    conn.execute(
        "UPDATE patch_files SET status = 'conflicted', accepted_blob_sha = NULL WHERE id = ?1",
        rusqlite::params![file_id],
    )?;
    conn.execute(
        "UPDATE patch_sets SET status = 'conflicted' WHERE id = ?1",
        rusqlite::params![patch_set_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_studio::git_broker::MergeOutcome;
    use crate::code_studio::workspace_db;

    /// Everything below drives the REAL git binary through the broker. A
    /// missing git is a broken test environment, not a passing test, so it
    /// fails loudly instead of returning early and reporting green — a build
    /// agent without git would otherwise run this whole module as a no-op.
    fn require_git() {
        let available = std::process::Command::new("git")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        assert!(
            available,
            "git is not installed; the patch set tests drive the real git binary"
        );
    }

    fn identity() -> CommitIdentity {
        CommitIdentity {
            name: "TentaFlow Code Studio".into(),
            email: "code-studio@tentaflow.local".into(),
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        broker: Broker,
        pool: DbPool,
        base: String,
        worktree: std::path::PathBuf,
    }

    impl Fixture {
        fn write(&self, path: &str, content: &str) {
            let target = self.worktree.join(path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(target, content).unwrap();
        }

        fn read(&self, path: &str) -> String {
            std::fs::read_to_string(self.worktree.join(path)).unwrap()
        }

        fn blob(&self, content: &str) -> String {
            self.broker
                .hash_object(&self.broker.reference(), content.as_bytes())
                .unwrap()
        }

        fn open(&self) -> PatchSet {
            open_patch_set(
                &self.pool,
                &self.broker,
                "s-1",
                Some("r-1"),
                &self.base,
                &PatchScope::Work,
            )
            .unwrap()
        }

        fn accept_all(&self, set: &PatchSet) -> DecisionOutcome {
            let files = set
                .files
                .iter()
                .map(|f| (f.path.clone(), FileVerdict::Accept))
                .collect();
            decide(
                &self.pool,
                &self.broker,
                &set.id,
                &Decisions {
                    decided_by: "u-1".into(),
                    files,
                },
            )
            .unwrap()
        }

        fn commit(&self, set_id: &str, expected_old: Option<&str>) -> Result<CommitOutcome> {
            let spec = accepted_commit_spec(
                &self.pool,
                &self.broker,
                set_id,
                &CommitRequest {
                    branch: "cs/u/s1".into(),
                    expected_old: expected_old.map(str::to_string),
                    message: "reviewed change".into(),
                    author: identity(),
                    committer: identity(),
                    extra_parent: None,
                },
            )?;
            self.broker
                .build_commit(&self.broker.session("s-1").unwrap(), &spec)
        }

        fn tree_blob(&self, commit: &str, path: &str) -> Option<String> {
            self.broker
                .blob_in_commit(&self.broker.reference(), commit, path)
                .unwrap()
        }
    }

    /// A workspace with a repository, a session worktree on its own branch and
    /// the runtime database the patch tables live in.
    fn fixture(seed: &[(&str, &str)]) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let broker = Broker::at(dir.path());
        let root = broker.init_repository("main").unwrap();

        let files = seed
            .iter()
            .map(|(path, content)| CommitFile {
                path: (*path).to_string(),
                old_path: None,
                mode: "100644".into(),
                change: CommitChange::Write {
                    content: content.as_bytes().to_vec(),
                },
            })
            .collect();
        let seeded = broker
            .build_commit(
                &broker.reference(),
                &CommitSpec {
                    base_commit: root.head_commit.clone(),
                    extra_parent: None,
                    branch: "main".into(),
                    expected_old: Some(root.head_commit.clone()),
                    message: "seed".into(),
                    author: identity(),
                    committer: identity(),
                    files,
                },
            )
            .unwrap();

        let worktree = broker
            .add_session_worktree("s-1", "cs/u/s1", "main")
            .unwrap();
        let (pool, _version) = workspace_db::open_pool_at(dir.path()).unwrap();
        seed_session(&pool, "s-1");
        {
            // `patch_sets.run_id` is a foreign key, so the runs a test attaches
            // a patch set to have to exist.
            let conn = pool.write().unwrap();
            for (run_id, ordinal) in [("r-1", 1), ("r-2", 2)] {
                conn.execute(
                    "INSERT INTO session_runs (run_id, session_id, ordinal, kind, trigger, status) \
                     VALUES (?1, 's-1', ?2, 'root', 'user', 'running')",
                    rusqlite::params![run_id, ordinal],
                )
                .unwrap();
            }
        }
        Fixture {
            _dir: dir,
            broker,
            pool,
            base: seeded.commit_oid,
            worktree,
        }
    }

    fn seed_session(pool: &DbPool, id: &str) {
        let conn = pool.write().unwrap();
        conn.execute(
            "INSERT INTO sessions (id, workspace_id, user_id, title, branch, autonomy_mode, \
              flow_id, flow_version_id, status, created_at, updated_at) \
             VALUES (?1, 'ws-1', 'u-1', 'Session', 'cs/u/s1', 'normal', 'flow', 'v1', \
              'idle', datetime('now'), datetime('now'))",
            rusqlite::params![id],
        )
        .unwrap();
    }

    // ----- integrity of the commit ------------------------------------------

    #[test]
    fn adversarial_a_patch_set_never_records_which_worktree_it_came_from() {
        // `PatchScope` is threaded through `open_patch_set` and
        // `tools::current_patch_set`, and the comment at the `apply_hunks` call
        // site says "the two could be confused; the query is scoped, so it
        // follows the row". There is no such column and no such query: the
        // scope only picks WHICH worktree gets snapshotted
        // (`broker.session` vs `broker.integration`) and is then thrown away.
        //
        // Both selectors are therefore blind to it:
        //   * `tools::current_patch_set` reuses any open/in_review/conflicted
        //     set of the session, whichever worktree produced it;
        //   * `tools::accepted_patch_set` returns the newest accepted set of the
        //     session, and BOTH `git_commit` (session branch) and
        //     `git_merge_finalize` (workspace target branch) call it.
        //
        // So a work patch set the human accepted for `cs/<user>/<session>` is
        // what `git_merge_finalize` publishes onto the target branch, and the
        // merge result §11.6 step 5 requires a human to see is never reviewed.
        // The dashboard path is looser still: `git_merge_finalize_v1` takes
        // `patch_set_id` straight off the wire and never checks that it belongs
        // to this session, to this merge op, or that its set-level status is
        // accepted at all.
        let dir = tempfile::tempdir().unwrap();
        let (pool, _version) = workspace_db::open_pool_at(dir.path()).expect("workspace.db");
        let conn = pool.read().unwrap();
        let mut stmt = conn.prepare("PRAGMA table_info(patch_sets)").unwrap();
        let columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(
            columns.iter().any(|c| c == "scope"),
            "patch_sets has no scope column, so nothing can tell a work patch set \
             from a merge patch set: {columns:?}"
        );
    }

    #[test]
    fn a_work_acceptance_is_not_an_answer_for_a_merge_review() {
        require_git();
        let fx = fixture(&[("f.txt", "a\n")]);
        fx.write("f.txt", "b\n");
        let set = fx.open();
        fx.accept_all(&set);

        let merge = PatchScope::Merge {
            op_id: "op-1".into(),
        };
        assert_eq!(load_patch_set(&fx.pool, &set.id).unwrap().scope, PatchScope::Work);
        assert!(has_accepted_patch_set(&fx.pool, "s-1", &PatchScope::Work).unwrap());
        assert_eq!(
            accepted_patch_set_for_scope(&fx.pool, "s-1", &PatchScope::Work)
                .unwrap()
                .id,
            set.id
        );

        // The same acceptance must not resolve a merge finalize, and naming the
        // set explicitly on the wire must not resolve it either.
        assert!(
            !has_accepted_patch_set(&fx.pool, "s-1", &merge).unwrap(),
            "a work acceptance opened the merge gate"
        );
        assert!(accepted_patch_set_for_scope(&fx.pool, "s-1", &merge).is_err());
        assert!(load_patch_set_for(&fx.pool, "s-1", &merge, &set.id).is_err());
        assert!(load_patch_set_for(&fx.pool, "s-2", &PatchScope::Work, &set.id).is_err());
    }

    #[test]
    fn a_merge_review_and_the_session_s_own_review_stay_two_open_decisions() {
        require_git();
        let fx = fixture(&[("f.txt", "a\nb\nc\n")]);
        fx.write("f.txt", "a\nSESSION\nc\n");
        let work = fx.open();

        let target_before = fx
            .broker
            .read_ref(&fx.broker.reference(), "refs/heads/main")
            .unwrap()
            .unwrap();
        fx.broker
            .add_integration_worktree("s-1", "op-1", &target_before)
            .unwrap();
        fx.broker.merge_into_integration("s-1", "cs/u/s1").unwrap();

        let merge = PatchScope::Merge {
            op_id: "op-1".into(),
        };
        let merge_set = open_patch_set(
            &fx.pool,
            &fx.broker,
            "s-1",
            Some("r-2"),
            &target_before,
            &merge,
        )
        .unwrap();

        // Two worktrees, two reviews: opening the merge review must not
        // supersede the one the agent's own changes are waiting in.
        assert_ne!(merge_set.id, work.id);
        assert_eq!(
            open_patch_set_for_scope(&fx.pool, "s-1", &PatchScope::Work)
                .unwrap()
                .map(|s| s.id),
            Some(work.id)
        );
        assert_eq!(
            open_patch_set_for_scope(&fx.pool, "s-1", &merge)
                .unwrap()
                .map(|s| s.id),
            Some(merge_set.id)
        );
        assert!(open_patch_set_for_scope(
            &fx.pool,
            "s-1",
            &PatchScope::Merge {
                op_id: "op-2".into()
            }
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn a_change_made_after_the_acceptance_never_reaches_the_commit() {
        require_git();
        let fx = fixture(&[("f.txt", "a\nb\nc\n")]);
        fx.write("f.txt", "a\nREVIEWED\nc\n");
        let set = fx.open();
        fx.accept_all(&set);
        let accepted = load_patch_set(&fx.pool, &set.id).unwrap().files[0]
            .accepted_blob_sha
            .clone()
            .unwrap();

        // The agent keeps working while the commit is being prepared.
        fx.write("f.txt", "a\nSNEAKED IN\nc\n");

        let head = fx
            .broker
            .read_ref(&fx.broker.reference(), "refs/heads/cs/u/s1")
            .unwrap()
            .unwrap();
        let outcome = fx.commit(&set.id, Some(&head)).unwrap();

        assert_eq!(
            outcome.blob_oid("f.txt"),
            Some(accepted.as_str()),
            "the commit did not carry the accepted blob"
        );
        assert_eq!(
            fx.tree_blob(&outcome.commit_oid, "f.txt"),
            Some(accepted.clone())
        );
        assert_eq!(
            String::from_utf8(
                fx.broker
                    .cat_file(&fx.broker.reference(), &accepted)
                    .unwrap()
            )
            .unwrap(),
            "a\nREVIEWED\nc\n"
        );
        // The worktree is not force-synchronised: what the agent wrote is still
        // on disk, it simply is not in the commit.
        assert_eq!(fx.read("f.txt"), "a\nSNEAKED IN\nc\n");
    }

    #[test]
    fn a_stale_expected_old_fails_the_commit_and_keeps_the_acceptance() {
        require_git();
        let fx = fixture(&[("f.txt", "a\n")]);
        fx.write("f.txt", "b\n");
        let set = fx.open();
        fx.accept_all(&set);

        // Something else already moved the branch on.
        let head = fx
            .broker
            .read_ref(&fx.broker.reference(), "refs/heads/cs/u/s1")
            .unwrap()
            .unwrap();
        let intruder = fx
            .broker
            .build_commit(
                &fx.broker.reference(),
                &CommitSpec {
                    base_commit: head.clone(),
                    extra_parent: None,
                    branch: "cs/u/s1".into(),
                    expected_old: Some(head.clone()),
                    message: "someone else".into(),
                    author: identity(),
                    committer: identity(),
                    files: vec![CommitFile {
                        path: "other.txt".into(),
                        old_path: None,
                        mode: "100644".into(),
                        change: CommitChange::Write {
                            content: b"x\n".to_vec(),
                        },
                    }],
                },
            )
            .unwrap();

        let err = fx.commit(&set.id, Some(&head)).unwrap_err();
        assert!(err.to_string().contains("update-ref"), "got {err}");
        assert_eq!(
            fx.broker
                .read_ref(&fx.broker.reference(), "refs/heads/cs/u/s1")
                .unwrap(),
            Some(intruder.commit_oid),
            "a stale compare-and-swap overwrote the branch"
        );
        // The decision survives the failed attempt: the human accepted content,
        // not a particular attempt at publishing it.
        assert!(has_accepted_patch_set(&fx.pool, "s-1", &PatchScope::Work).unwrap());
    }

    // ----- the commit does not depend on the worktree -----------------------

    #[test]
    fn a_parallel_edit_neither_blocks_the_commit_nor_enters_it_and_becomes_the_next_set() {
        require_git();
        let fx = fixture(&[("f.txt", "a\n")]);
        fx.write("f.txt", "reviewed\n");
        let set = fx.open();
        fx.accept_all(&set);
        fx.write("f.txt", "later\n");

        let head = fx
            .broker
            .read_ref(&fx.broker.reference(), "refs/heads/cs/u/s1")
            .unwrap()
            .unwrap();
        let outcome = fx.commit(&set.id, Some(&head)).unwrap();
        mark_consumed(&fx.pool, &set.id, &outcome).unwrap();
        assert!(
            !has_accepted_patch_set(&fx.pool, "s-1", &PatchScope::Work).unwrap(),
            "a consumed patch set still unlocks a commit"
        );

        // Everything the agent did during the review is the NEXT patch set.
        let next = open_patch_set(
            &fx.pool,
            &fx.broker,
            "s-1",
            Some("r-2"),
            &outcome.commit_oid,
            &PatchScope::Work,
        )
        .unwrap();
        assert_eq!(next.files.len(), 1);
        assert_eq!(next.files[0].path, "f.txt");
        assert_eq!(
            next.files[0].current_blob_sha,
            Some(fx.blob("later\n")),
            "the parallel edit did not become the next patch set"
        );
        assert_eq!(
            next.files[0].patch_base_blob_sha,
            Some(fx.blob("reviewed\n")),
            "the next set is not based on what was committed"
        );
    }

    #[test]
    fn a_rename_leaves_the_old_path_and_appears_under_the_new_one() {
        require_git();
        let fx = fixture(&[("old/name.txt", "content\n")]);
        let set = open_patch_set(
            &fx.pool,
            &fx.broker,
            "s-1",
            None,
            &fx.base,
            &PatchScope::Work,
        )
        .unwrap();
        assert!(set.files.is_empty(), "a clean worktree has no changes");

        // The filesystem layer moved the file and told the patch set about it.
        std::fs::create_dir_all(fx.worktree.join("new")).unwrap();
        std::fs::rename(
            fx.worktree.join("old/name.txt"),
            fx.worktree.join("new/name.txt"),
        )
        .unwrap();
        let content = fx.blob("content\n");
        record_edit(
            &fx.pool,
            &fx.broker,
            &set.id,
            "old/name.txt",
            EditKind::Rename {
                new_path: "new/name.txt".into(),
            },
            Some(&content),
            &Precondition::BlobIs(content.clone()),
        )
        .unwrap();

        let set = load_patch_set(&fx.pool, &set.id).unwrap();
        assert_eq!(set.files.len(), 1);
        assert_eq!(set.files[0].path, "new/name.txt");
        assert_eq!(set.files[0].old_path.as_deref(), Some("old/name.txt"));
        assert_eq!(
            set.files[0].patch_base_blob_sha,
            Some(content.clone()),
            "the rename lost the frozen base content"
        );

        fx.accept_all(&set);
        let head = fx
            .broker
            .read_ref(&fx.broker.reference(), "refs/heads/cs/u/s1")
            .unwrap()
            .unwrap();
        let outcome = fx.commit(&set.id, Some(&head)).unwrap();

        let paths: Vec<String> = fx
            .broker
            .list_tree(&fx.broker.reference(), &outcome.tree_oid)
            .unwrap()
            .into_iter()
            .map(|e| e.path)
            .collect();
        assert_eq!(paths, vec!["new/name.txt".to_string()]);
        assert_eq!(fx.tree_blob(&outcome.commit_oid, "old/name.txt"), None);
        assert_eq!(
            fx.tree_blob(&outcome.commit_oid, "new/name.txt"),
            Some(content)
        );
    }

    // ----- compare-and-swap --------------------------------------------------

    #[test]
    fn the_second_edit_of_a_file_expects_the_current_value_not_the_base() {
        require_git();
        let fx = fixture(&[("f.txt", "base\n")]);
        let set = fx.open();
        let base = fx.blob("base\n");
        let first = fx.blob("first\n");
        let second = fx.blob("second\n");

        record_edit(
            &fx.pool,
            &fx.broker,
            &set.id,
            "f.txt",
            EditKind::Write,
            Some(&first),
            &Precondition::BlobIs(base.clone()),
        )
        .unwrap();

        // The rule "apply expects base" was right only for the FIRST edit.
        let err = record_edit(
            &fx.pool,
            &fx.broker,
            &set.id,
            "f.txt",
            EditKind::Write,
            Some(&second),
            &Precondition::BlobIs(base),
        )
        .unwrap_err();
        assert!(err.to_string().contains("precondition failed"), "got {err}");
        assert_eq!(
            load_patch_set(&fx.pool, &set.id)
                .unwrap()
                .file("f.txt")
                .unwrap()
                .status,
            "conflicted"
        );

        // Expecting the CURRENT value goes through on the same patch set, and
        // the file is decidable again.
        record_edit(
            &fx.pool,
            &fx.broker,
            &set.id,
            "f.txt",
            EditKind::Write,
            Some(&second),
            &Precondition::BlobIs(first),
        )
        .unwrap();
        let stored = load_patch_set(&fx.pool, &set.id).unwrap();
        assert_eq!(stored.file("f.txt").unwrap().current_blob_sha, Some(second));
        assert_eq!(stored.file("f.txt").unwrap().status, "pending");
    }

    #[test]
    fn delete_and_rename_carry_their_own_preconditions() {
        require_git();
        let fx = fixture(&[("f.txt", "base\n"), ("g.txt", "other\n")]);
        let set = fx.open();
        let base = fx.blob("base\n");

        // Delete expects the current content, not "anything".
        let err = record_edit(
            &fx.pool,
            &fx.broker,
            &set.id,
            "f.txt",
            EditKind::Delete,
            None,
            &Precondition::BlobIs(fx.blob("wrong\n")),
        )
        .unwrap_err();
        assert!(err.to_string().contains("precondition failed"), "got {err}");

        let set = fx.open();
        record_edit(
            &fx.pool,
            &fx.broker,
            &set.id,
            "f.txt",
            EditKind::Delete,
            None,
            &Precondition::BlobIs(base.clone()),
        )
        .unwrap();
        let stored = load_patch_set(&fx.pool, &set.id).unwrap();
        let deleted = stored.file("f.txt").unwrap();
        assert_eq!(deleted.change_kind, "delete");
        assert!(deleted.current_blob_sha.is_none());
        assert_eq!(deleted.patch_base_blob_sha, Some(base.clone()));

        // A rename onto a path that exists in the base is refused: the target
        // has to be absent, and `patch_files` alone could not tell.
        let err = record_edit(
            &fx.pool,
            &fx.broker,
            &set.id,
            "g.txt",
            EditKind::Rename {
                new_path: "h.txt".into(),
            },
            None,
            &Precondition::BlobIs(base),
        )
        .unwrap_err();
        assert!(err.to_string().contains("precondition failed"), "got {err}");

        let other = fx.blob("other\n");
        let set = fx.open();
        let err = record_edit(
            &fx.pool,
            &fx.broker,
            &set.id,
            "g.txt",
            EditKind::Rename {
                new_path: "f.txt".into(),
            },
            Some(&other),
            &Precondition::BlobIs(other.clone()),
        )
        .unwrap_err();
        assert!(err.to_string().contains("not absent"), "got {err}");

        record_edit(
            &fx.pool,
            &fx.broker,
            &set.id,
            "g.txt",
            EditKind::Rename {
                new_path: "h.txt".into(),
            },
            Some(&other),
            &Precondition::BlobIs(other.clone()),
        )
        .unwrap();
        let stored = load_patch_set(&fx.pool, &set.id).unwrap();
        assert!(stored.file("g.txt").is_none());
        assert_eq!(
            stored.file("h.txt").unwrap().old_path.as_deref(),
            Some("g.txt")
        );
    }

    #[test]
    fn an_agent_and_a_human_writing_at_once_end_in_conflicted() {
        require_git();
        let fx = fixture(&[("f.txt", "base\n")]);
        let set = fx.open();
        let base = fx.blob("base\n");

        record_edit(
            &fx.pool,
            &fx.broker,
            &set.id,
            "f.txt",
            EditKind::Write,
            Some(&fx.blob("agent\n")),
            &Precondition::BlobIs(base.clone()),
        )
        .unwrap();
        // The human's editor still believed the file held the base content.
        let err = record_edit(
            &fx.pool,
            &fx.broker,
            &set.id,
            "f.txt",
            EditKind::Write,
            Some(&fx.blob("human\n")),
            &Precondition::BlobIs(base),
        )
        .unwrap_err();
        assert!(err.to_string().contains("precondition failed"), "got {err}");

        let stored = load_patch_set(&fx.pool, &set.id).unwrap();
        assert_eq!(stored.status, "conflicted");
        assert_eq!(stored.file("f.txt").unwrap().status, "conflicted");
        assert!(
            !has_accepted_patch_set(&fx.pool, "s-1", &PatchScope::Work).unwrap(),
            "a conflicted set must not open the commit gate"
        );
    }

    #[test]
    fn a_partial_acceptance_composes_deterministically_on_the_frozen_base() {
        require_git();
        let original: String = (1..=20).map(|n| format!("{n}\n")).collect();
        let fx = fixture(&[("f.txt", &original)]);
        let edited: String = (1..=20)
            .map(|n| match n {
                2 => "TWO\n".to_string(),
                19 => "NINETEEN\n".to_string(),
                _ => format!("{n}\n"),
            })
            .collect();
        fx.write("f.txt", &edited);

        let set = fx.open();
        assert_eq!(set.files.len(), 1);
        assert_eq!(
            set.files[0].hunks.len(),
            2,
            "two distant edits must be two hunks"
        );

        let outcome = decide(
            &fx.pool,
            &fx.broker,
            &set.id,
            &Decisions {
                decided_by: "u-1".into(),
                files: vec![("f.txt".into(), FileVerdict::Hunks(vec![1]))],
            },
        )
        .unwrap();
        assert_eq!(outcome.status, "partially_accepted");

        // Only the second hunk survives, and the result is exactly the content
        // that describes — the composition is deterministic, not approximate.
        let expected: String = (1..=20)
            .map(|n| {
                if n == 19 {
                    "NINETEEN\n".to_string()
                } else {
                    format!("{n}\n")
                }
            })
            .collect();
        let stored = load_patch_set(&fx.pool, &set.id).unwrap();
        let accepted = stored
            .file("f.txt")
            .unwrap()
            .accepted_blob_sha
            .clone()
            .unwrap();
        assert_eq!(accepted, fx.blob(&expected));
        assert_eq!(stored.file("f.txt").unwrap().hunks[0].status, "rejected");
        assert_eq!(stored.file("f.txt").unwrap().hunks[1].status, "accepted");

        // The rejected hunk has to leave the disk, and the intent carries the
        // precondition the filesystem layer must honour.
        assert_eq!(outcome.rewrites.len(), 1);
        assert_eq!(outcome.rewrites[0].blob_oid, Some(accepted));
        assert_eq!(
            outcome.rewrites[0].expect,
            Precondition::BlobIs(fx.blob(&edited))
        );
    }

    #[test]
    fn hunks_with_overlapping_contexts_are_conflicted_rather_than_guessed_at() {
        require_git();
        let original: String = (1..=20).map(|n| format!("{n}\n")).collect();
        let fx = fixture(&[("f.txt", &original)]);
        let first: String = (1..=20)
            .map(|n| match n {
                2 => "TWO\n".to_string(),
                19 => "NINETEEN\n".to_string(),
                _ => format!("{n}\n"),
            })
            .collect();
        fx.write("f.txt", &first);
        let set = fx.open();
        assert_eq!(set.files[0].hunks.len(), 2);

        // A later edit of the SAME region produced a third recorded hunk. It and
        // hunk 0 both describe lines 1-5 of the frozen base, so accepting both
        // cannot compose: that is the overlapping-context case of §13.2.
        let second: String = (1..=20)
            .map(|n| {
                if n == 2 {
                    "DEUX\n".into()
                } else {
                    format!("{n}\n")
                }
            })
            .collect();
        fx.write("f.txt", &second);
        let handle = fx.broker.session("s-1").unwrap();
        let tree = fx.broker.snapshot_worktree(&handle, &fx.base).unwrap();
        let diff = fx
            .broker
            .diff_patch(&handle, &fx.base, &tree, "f.txt")
            .unwrap();
        let overlapping = split_hunks(&diff).remove(0);
        let content_ref = fx
            .broker
            .hash_object(&handle, overlapping.as_bytes())
            .unwrap();
        {
            let conn = fx.pool.write().unwrap();
            conn.execute(
                "INSERT INTO patch_hunks (id, patch_file_id, idx, header, content_ref, status) \
                 VALUES (?1, ?2, 2, ?3, ?4, 'pending')",
                rusqlite::params![
                    uuid::Uuid::new_v4().to_string(),
                    set.files[0].id,
                    overlapping.lines().next().unwrap(),
                    content_ref
                ],
            )
            .unwrap();
        }
        // Put the reviewed content back so `current_blob_sha` still describes
        // the state the hunks were cut from.
        fx.write("f.txt", &first);

        let outcome = decide(
            &fx.pool,
            &fx.broker,
            &set.id,
            &Decisions {
                decided_by: "u-1".into(),
                files: vec![("f.txt".into(), FileVerdict::Hunks(vec![0, 2]))],
            },
        )
        .unwrap();
        assert_eq!(outcome.status, "conflicted");
        assert_eq!(outcome.conflicted, vec!["f.txt".to_string()]);
        let stored = load_patch_set(&fx.pool, &set.id).unwrap();
        assert!(
            stored.file("f.txt").unwrap().accepted_blob_sha.is_none(),
            "an unclean composition must not produce an accepted blob"
        );
        assert!(!has_accepted_patch_set(&fx.pool, "s-1", &PatchScope::Work).unwrap());
    }

    #[test]
    fn reverting_restores_the_frozen_base_under_its_own_precondition() {
        require_git();
        let fx = fixture(&[("f.txt", "base\n")]);
        fx.write("f.txt", "changed\n");
        fx.write("added.txt", "new\n");
        let set = fx.open();

        let reverted = revert(&fx.pool, &fx.broker, &set.id, "f.txt").unwrap();
        assert_eq!(reverted.len(), 1);
        assert_eq!(reverted[0].blob_oid, Some(fx.blob("base\n")));
        assert_eq!(
            reverted[0].expect,
            Precondition::BlobIs(fx.blob("changed\n"))
        );

        // Reverting a file the set created removes it instead of restoring it.
        let dropped = revert(&fx.pool, &fx.broker, &set.id, "added.txt").unwrap();
        assert_eq!(dropped[0].blob_oid, None);

        let stored = load_patch_set(&fx.pool, &set.id).unwrap();
        assert!(stored.files.is_empty(), "the set still proposes changes");
    }

    // ----- gate 5a -----------------------------------------------------------

    #[test]
    fn an_acceptance_in_another_session_does_not_open_the_commit_gate() {
        require_git();
        let fx = fixture(&[("f.txt", "a\n")]);
        seed_session(&fx.pool, "s-2");
        fx.write("f.txt", "b\n");
        let set = fx.open();
        fx.accept_all(&set);

        assert!(has_accepted_patch_set(&fx.pool, "s-1", &PatchScope::Work).unwrap());
        assert!(
            !has_accepted_patch_set(&fx.pool, "s-2", &PatchScope::Work).unwrap(),
            "an acceptance leaked across sessions"
        );

        // Re-pointing the same accepted set at another session is the only way
        // the two could be confused; the query filters on the session AND the
        // scope columns of the row, so it follows the row.
        {
            let conn = fx.pool.write().unwrap();
            conn.execute(
                "UPDATE patch_sets SET session_id = 's-2' WHERE id = ?1",
                rusqlite::params![set.id],
            )
            .unwrap();
        }
        assert!(!has_accepted_patch_set(&fx.pool, "s-1", &PatchScope::Work).unwrap());
        assert!(has_accepted_patch_set(&fx.pool, "s-2", &PatchScope::Work).unwrap());
    }

    // ----- merge -------------------------------------------------------------

    #[test]
    fn a_merge_is_finalised_from_accepted_blobs_not_from_the_integration_worktree() {
        require_git();
        let fx = fixture(&[("f.txt", "a\nb\nc\n")]);
        // The session changes the file and commits on its own branch.
        fx.write("f.txt", "a\nSESSION\nc\n");
        let set = fx.open();
        fx.accept_all(&set);
        let head = fx
            .broker
            .read_ref(&fx.broker.reference(), "refs/heads/cs/u/s1")
            .unwrap()
            .unwrap();
        let session_commit = fx.commit(&set.id, Some(&head)).unwrap();
        mark_consumed(&fx.pool, &set.id, &session_commit).unwrap();

        let target_before = fx
            .broker
            .read_ref(&fx.broker.reference(), "refs/heads/main")
            .unwrap()
            .unwrap();
        fx.broker
            .add_integration_worktree("s-1", "op-1", &target_before)
            .unwrap();
        let MergeOutcome::Clean { merge_head, .. } =
            fx.broker.merge_into_integration("s-1", "cs/u/s1").unwrap()
        else {
            panic!("expected a clean merge");
        };
        fx.broker.write_private_ref("op-1", &merge_head).unwrap();
        assert_eq!(
            fx.broker
                .read_ref(&fx.broker.reference(), "refs/heads/main")
                .unwrap(),
            Some(target_before.clone()),
            "the merge moved the target branch before anyone accepted it"
        );

        // The merge result is reviewed on the integration worktree, and the
        // agent's leftovers there must not reach the commit.
        let merge_set = open_patch_set(
            &fx.pool,
            &fx.broker,
            "s-1",
            Some("r-2"),
            &target_before,
            &PatchScope::Merge {
                op_id: "op-1".into(),
            },
        )
        .unwrap();
        assert_eq!(merge_set.files.len(), 1);
        assert_eq!(merge_set.files[0].path, "f.txt");
        fx.accept_all(&merge_set);
        let accepted = load_patch_set(&fx.pool, &merge_set.id).unwrap().files[0]
            .accepted_blob_sha
            .clone()
            .unwrap();

        // The decision belongs to THIS merge operation: a finalize resolves it
        // from the integration worktree it holds, another operation's finalize
        // finds nothing, and the session's commit gate is not opened by it.
        let merge_scope = PatchScope::Merge {
            op_id: "op-1".into(),
        };
        assert_eq!(
            accepted_patch_set_for_scope(&fx.pool, "s-1", &merge_scope)
                .unwrap()
                .id,
            merge_set.id
        );
        assert!(accepted_patch_set_for_scope(
            &fx.pool,
            "s-1",
            &PatchScope::Merge {
                op_id: "op-2".into()
            }
        )
        .is_err());
        assert!(
            !has_accepted_patch_set(&fx.pool, "s-1", &PatchScope::Work).unwrap(),
            "a merge acceptance opened the session's own commit gate"
        );

        let integration = fx.broker.integration_worktree("s-1").unwrap();
        std::fs::write(integration.join("f.txt"), "a\nLEFTOVER\nc\n").unwrap();

        let spec = accepted_commit_spec(
            &fx.pool,
            &fx.broker,
            &merge_set.id,
            &CommitRequest {
                branch: "main".into(),
                expected_old: Some(target_before.clone()),
                message: "merge".into(),
                author: identity(),
                committer: identity(),
                extra_parent: Some(session_commit.commit_oid.clone()),
            },
        )
        .unwrap();
        let finalised = fx.broker.finalize_merge(&spec).unwrap();

        assert_eq!(fx.tree_blob(&finalised.commit_oid, "f.txt"), Some(accepted));
        assert_eq!(
            fx.broker
                .read_ref(&fx.broker.reference(), "refs/heads/main")
                .unwrap(),
            Some(finalised.commit_oid.clone())
        );
        let meta = fx
            .broker
            .commit_metadata(&fx.broker.reference(), &finalised.commit_oid)
            .unwrap()
            .unwrap();
        assert_eq!(
            meta.parents,
            vec![target_before, session_commit.commit_oid],
            "a merge commit must record both sides"
        );
    }

    // ----- work that never went through our tools ----------------------------

    /// D5: a delegated turn writes to the worktree with the vendor's own file
    /// calls, so `record_edit` never sees it. The set opened before the turn
    /// therefore describes nothing, and the review it feeds is empty while
    /// `git diff` shows the change. Rescanning on the SAME frozen base is what
    /// turns that work into something a person can accept hunk by hunk.
    #[test]
    fn work_written_past_our_tools_becomes_reviewable_per_hunk_after_a_rescan() {
        require_git();
        let original: String = (1..=20).map(|n| format!("{n}\n")).collect();
        let fx = fixture(&[("f.txt", &original)]);

        // The set is opened BEFORE the turn, on the pre-delegation HEAD.
        let opened = fx.open();
        assert!(
            opened.files.is_empty(),
            "nothing has been written yet, so the set starts empty"
        );

        // The delegation writes. Nothing journals it — this is the vendor's own
        // file call, not `fs_write`.
        let edited: String = (1..=20)
            .map(|n| match n {
                2 => "TWO\n".to_string(),
                19 => "NINETEEN\n".to_string(),
                _ => format!("{n}\n"),
            })
            .collect();
        fx.write("f.txt", &edited);
        fx.write("new.txt", "fresh\n");

        // Without the rescan this is what the review still sees.
        assert!(load_patch_set(&fx.pool, &opened.id).unwrap().files.is_empty());

        let refreshed = rescan_patch_set(&fx.pool, &fx.broker, &opened.id).unwrap();
        assert_eq!(refreshed.id, opened.id, "the set keeps its identity");
        assert_eq!(
            refreshed.base_commit, opened.base_commit,
            "the base was frozen before the turn and must stay frozen"
        );
        assert_eq!(refreshed.files.len(), 2);
        let touched = refreshed.file("f.txt").expect("the edited file");
        assert_eq!(touched.change_kind, "modify");
        assert_eq!(touched.hunks.len(), 2, "two distant edits, two hunks");
        assert_eq!(
            refreshed.file("new.txt").expect("the new file").change_kind,
            "add"
        );

        // And it is decidable per hunk, which is the whole point.
        let outcome = decide(
            &fx.pool,
            &fx.broker,
            &refreshed.id,
            &Decisions {
                decided_by: "u-1".into(),
                files: vec![
                    ("f.txt".into(), FileVerdict::Hunks(vec![1])),
                    ("new.txt".into(), FileVerdict::Reject),
                ],
            },
        )
        .unwrap();
        assert_eq!(outcome.status, "partially_accepted");

        let expected: String = (1..=20)
            .map(|n| {
                if n == 19 {
                    "NINETEEN\n".to_string()
                } else {
                    format!("{n}\n")
                }
            })
            .collect();
        let stored = load_patch_set(&fx.pool, &refreshed.id).unwrap();
        assert_eq!(
            stored.file("f.txt").unwrap().accepted_blob_sha,
            Some(fx.blob(&expected))
        );

        // The commit is built from the accepted blob, so the reviewed bytes are
        // the committed bytes.
        let head = fx
            .broker
            .read_ref(&fx.broker.reference(), "refs/heads/cs/u/s1")
            .unwrap()
            .unwrap();
        let committed = fx.commit(&refreshed.id, Some(&head)).unwrap();
        assert_eq!(
            fx.tree_blob(&committed.commit_oid, "f.txt"),
            Some(fx.blob(&expected))
        );
        assert_eq!(fx.tree_blob(&committed.commit_oid, "new.txt"), None);
    }

    /// A rescan is an observation of undecided material, never a rewrite of a
    /// verdict: §11.5 step 5 makes a later divergence the NEXT set's business.
    /// A pending row whose change was undone before anyone looked at it goes
    /// away instead of lingering as a phantom file.
    #[test]
    fn a_rescan_keeps_decisions_and_drops_a_change_that_was_undone() {
        require_git();
        let fx = fixture(&[("kept.txt", "base\n"), ("undone.txt", "base\n")]);
        fx.write("kept.txt", "reviewed\n");
        fx.write("undone.txt", "scratch\n");
        let set = fx.open();
        assert_eq!(set.files.len(), 2);

        decide(
            &fx.pool,
            &fx.broker,
            &set.id,
            &Decisions {
                decided_by: "u-1".into(),
                files: vec![("kept.txt".into(), FileVerdict::Accept)],
            },
        )
        .unwrap();

        // The turn keeps working: it rewrites the accepted file and undoes the
        // other one.
        fx.write("kept.txt", "moved on\n");
        fx.write("undone.txt", "base\n");

        let refreshed = rescan_patch_set(&fx.pool, &fx.broker, &set.id).unwrap();
        let kept = refreshed.file("kept.txt").expect("the decided file");
        assert_eq!(kept.status, "accepted");
        assert_eq!(
            kept.accepted_blob_sha,
            Some(fx.blob("reviewed\n")),
            "a rescan must not rewrite what a person already accepted"
        );
        assert!(
            refreshed.file("undone.txt").is_none(),
            "a pending change that was undone is no longer material"
        );
        assert_eq!(refreshed.status, "accepted");

        // A set that has been consumed is not open to new material at all.
        let head = fx
            .broker
            .read_ref(&fx.broker.reference(), "refs/heads/cs/u/s1")
            .unwrap()
            .unwrap();
        let committed = fx.commit(&set.id, Some(&head)).unwrap();
        mark_consumed(&fx.pool, &set.id, &committed).unwrap();
        assert!(rescan_patch_set(&fx.pool, &fx.broker, &set.id).is_err());
    }

    // ----- pure helpers ------------------------------------------------------

    #[test]
    fn a_unified_diff_splits_into_hunks_and_a_binary_one_into_none() {
        let diff = "diff --git a/f b/f\nindex aa..bb 100644\n--- a/f\n+++ b/f\n\
                    @@ -1,2 +1,2 @@\n a\n-b\n+B\n@@ -9,2 +9,2 @@\n x\n-y\n+Y\n";
        let hunks = split_hunks(diff);
        assert_eq!(hunks.len(), 2);
        assert!(hunks[0].starts_with("@@ -1,2 +1,2 @@"));
        assert!(hunks[1].ends_with("+Y\n"));

        assert!(split_hunks("Binary files a/f and b/f differ\n").is_empty());
        assert!(split_hunks("").is_empty());
    }

    #[test]
    fn a_full_or_empty_hunk_selection_collapses_to_a_whole_file_verdict() {
        assert_eq!(
            normalise(&FileVerdict::Hunks(vec![0, 1]), &[0, 1]),
            FileVerdict::Accept
        );
        assert_eq!(
            normalise(&FileVerdict::Hunks(vec![]), &[0, 1]),
            FileVerdict::Reject
        );
        assert_eq!(
            normalise(&FileVerdict::Hunks(vec![1, 1]), &[0, 1]),
            FileVerdict::Hunks(vec![1])
        );
        // A file with no hunks at all (binary) can only be decided as a whole.
        assert_eq!(
            normalise(&FileVerdict::Hunks(vec![0]), &[]),
            FileVerdict::Hunks(vec![0])
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_symlink_is_refused_instead_of_being_reviewed_as_text() {
        require_git();
        let fx = fixture(&[("f.txt", "a\n")]);
        std::os::unix::fs::symlink("f.txt", fx.worktree.join("link.txt")).unwrap();
        let err = open_patch_set(
            &fx.pool,
            &fx.broker,
            "s-1",
            None,
            &fx.base,
            &PatchScope::Work,
        )
        .unwrap_err();
        assert!(err.to_string().contains("regular files only"), "got {err}");
    }
}
