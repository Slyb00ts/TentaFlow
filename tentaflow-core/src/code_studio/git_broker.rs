// ===== File: code_studio/git_broker.rs — every git process of a workspace runs here =====
//
// The broker runs on the owner node, OUTSIDE any sandbox. A session never has
// git metadata, never has a route for ssh and never sees secret material: it
// asks the broker, the broker decides and executes.
//
// Two rules shape this module.
//
// **The broker never trusts the worktree.** A worktree's `.git` file is a
// pointer living in a tree the agent may write, so a swapped pointer could aim
// a privileged `git` process at another repository — including one outside the
// workspace. Every invocation therefore passes explicit `--git-dir` and
// `--work-tree` derived from the broker's own map, and nothing is ever read out
// of the working tree to locate a repository. This holds in BOTH execution
// modes, so the rule does not depend on which one is configured.
//
// **The hardened config applies from the first clone.** A clone pulls content
// from an untrusted remote, so protocol policy, redirect refusal, disabled
// helpers and `submodule.recurse=false` have to be in force at that moment, not
// from some later phase.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use anyhow::{anyhow, Result};

use super::paths;
use super::remote_policy::{self, RemoteScheme, RemoteTarget};

/// Canonical location of a repository, established when the workspace or
/// worktree is created and never re-derived from disk contents.
#[derive(Debug, Clone)]
pub struct RepoHandle {
    pub git_dir: PathBuf,
    pub work_tree: PathBuf,
}

/// How the remote authenticates. The material itself lives in the vault and is
/// handed to a single call, never stored in the workspace.
pub enum GitAuth {
    None,
    /// HTTPS token, delivered through `GIT_ASKPASS`.
    Token(String),
    /// SSH private key plus the pinned host key line.
    SshKey {
        private_key: String,
        known_host: Option<String>,
    },
}

/// Result of creating or cloning a repository.
#[derive(Debug, Clone)]
pub struct CloneOutcome {
    pub default_branch: String,
    pub head_commit: String,
}

/// Header of one commit, as the operation journal needs it to decide whether an
/// interrupted commit actually landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitMeta {
    pub tree: String,
    pub parents: Vec<String>,
}

/// Who a commit is attributed to. The server stamps this per invocation: a
/// workspace tree can carry a `.git/config` with any identity in it, and a
/// commit built by Code Studio must never inherit one.
#[derive(Debug, Clone)]
pub struct CommitIdentity {
    pub name: String,
    pub email: String,
}

/// What happens to one path in a commit. `Write` carries the CONTENT rather
/// than an object id: the commit is assembled from the accepted material, and
/// hashing it here is what makes the resulting oid provably that material.
#[derive(Debug, Clone)]
pub enum CommitChange {
    Write { content: Vec<u8> },
    Delete,
}

#[derive(Debug, Clone)]
pub struct CommitFile {
    pub path: String,
    /// Set only for a rename. The old path is REMOVED from the index in the
    /// same step the new one is added — `--cacheinfo` alone would leave the
    /// file under both names.
    pub old_path: Option<String>,
    pub mode: String,
    pub change: CommitChange,
}

/// Everything a commit is built from. There is no worktree in here on purpose
/// (§11.5): the tree comes from `base_commit` plus the accepted files, so what
/// the agent did to the disk during the review cannot reach the commit.
#[derive(Debug, Clone)]
pub struct CommitSpec {
    /// Commit whose tree seeds the temporary index, and the first parent.
    pub base_commit: String,
    /// Second parent of a merge commit. `None` for an ordinary commit.
    pub extra_parent: Option<String>,
    /// Branch the new commit is published on, without `refs/heads/`.
    pub branch: String,
    /// Value the branch must still have. `None` means the branch must not
    /// exist yet — never "whatever is there now".
    pub expected_old: Option<String>,
    pub message: String,
    pub author: CommitIdentity,
    pub committer: CommitIdentity,
    pub files: Vec<CommitFile>,
}

/// Every object id the commit produced. This is what makes the operation
/// verifiable after a crash (§11.5): the blob per path, the tree, the commit,
/// and the reference before and after the atomic update.
#[derive(Debug, Clone)]
pub struct CommitOutcome {
    pub blob_oids: Vec<(String, String)>,
    pub tree_oid: String,
    pub commit_oid: String,
    pub ref_name: String,
    pub ref_before: Option<String>,
    pub ref_after: String,
}

impl CommitOutcome {
    pub fn blob_oid(&self, path: &str) -> Option<&str> {
        self.blob_oids
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, oid)| oid.as_str())
    }
}

/// Result of merging a branch into the integration worktree. A conflict is an
/// OUTCOME, not an error: the worktree stays, holding the half-merged tree the
/// next revision run works on (§11.6 step 3).
#[derive(Debug, Clone)]
pub enum MergeOutcome {
    Clean {
        merge_head: String,
        fast_forward: bool,
    },
    Conflict {
        paths: Vec<String>,
    },
}

/// One entry of a name-status diff.
#[derive(Debug, Clone)]
pub struct DiffEntry {
    pub status: char,
    pub path: String,
    pub old_path: Option<String>,
}

/// One commit of a history listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub oid: String,
    pub short_oid: String,
    pub author: String,
    /// Author date in RFC 3339, as `%aI` renders it.
    pub date: String,
    pub subject: String,
}

/// One local branch with its upstream relationship.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchLine {
    pub name: String,
    pub is_current: bool,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
}

/// Ceiling on one history request. A session timeline that asks for "the whole
/// log" of a large repository would otherwise turn one click into a megabyte.
const MAX_LOG_ENTRIES: u32 = 500;

/// One entry of a recursive tree listing.
#[derive(Debug, Clone)]
pub struct TreeEntry {
    pub mode: String,
    pub oid: String,
    pub path: String,
}

/// Git operations of ONE workspace. Holds the workspace root explicitly so the
/// same code serves production (root derived from the workspace id) and tests
/// (root in a temporary directory) — the derivation is the only difference.
pub struct Broker {
    root: PathBuf,
}

impl Broker {
    /// Broker of a real workspace. The root is derived from the id behind the
    /// path guard, so no caller can aim it elsewhere.
    pub fn for_workspace(workspace_id: &str) -> Result<Self> {
        Ok(Self {
            root: paths::workspace_dir(workspace_id)?,
        })
    }

    /// Broker over an explicit workspace root.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Handle of the workspace's reference repository (`repo/`).
    pub fn reference(&self) -> RepoHandle {
        let work_tree = self.root.join("repo");
        RepoHandle {
            git_dir: work_tree.join(".git"),
            work_tree,
        }
    }

    /// Handle of a session worktree. Its git directory lives INSIDE the
    /// reference repository (`repo/.git/worktrees/<session>`), which is exactly
    /// why the pointer file in the worktree is never consulted.
    pub fn session(&self, session_id: &str) -> Result<RepoHandle> {
        paths::validate_session_id(session_id)?;
        let reference = self.reference();
        Ok(RepoHandle {
            git_dir: reference.git_dir.join("worktrees").join(session_id),
            work_tree: self.session_worktree(session_id)?,
        })
    }

    pub fn session_worktree(&self, session_id: &str) -> Result<PathBuf> {
        paths::validate_session_id(session_id)?;
        Ok(self.root.join("worktrees").join(session_id))
    }

    /// Private directory of the broker: hook stub, askpass helper, transient
    /// key material. Never inside a worktree, never mounted.
    fn broker_dir(&self) -> Result<PathBuf> {
        let dir = self.root.join("git-broker");
        std::fs::create_dir_all(&dir).map_err(|e| anyhow!("git broker dir: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| anyhow!("git broker dir permissions: {e}"))?;
        }
        Ok(dir)
    }

    fn empty_hooks_dir(&self) -> Result<PathBuf> {
        let dir = self.broker_dir()?.join("no-hooks");
        std::fs::create_dir_all(&dir).map_err(|e| anyhow!("git hooks stub dir: {e}"))?;
        Ok(dir)
    }

    /// Creates an empty repository with one empty commit, so a session has a
    /// branch to fork from before anyone has written a line.
    pub fn init_repository(&self, default_branch: &str) -> Result<CloneOutcome> {
        validate_branch(default_branch)?;
        let handle = self.reference();
        std::fs::create_dir_all(&handle.work_tree).map_err(|e| anyhow!("create repo dir: {e}"))?;

        let out = self.run(
            None,
            &[
                "init",
                "--initial-branch",
                default_branch,
                &handle.work_tree.display().to_string(),
            ],
            &GitAuth::None,
        )?;
        ok_or_stderr(out, "git init")?;

        // The author identity is set by the server per invocation — never taken
        // from a config file the workspace could carry.
        let out = self.run(
            Some(&handle),
            &[
                "-c",
                "user.name=TentaFlow Code Studio",
                "-c",
                "user.email=code-studio@tentaflow.local",
                "commit",
                "--allow-empty",
                "-m",
                "Initial commit",
            ],
            &GitAuth::None,
        )?;
        ok_or_stderr(out, "git commit --allow-empty")?;

        Ok(CloneOutcome {
            default_branch: default_branch.to_string(),
            head_commit: self.head_commit(&handle)?,
        })
    }

    /// Clones a remote into the reference repository. The policy check runs
    /// here rather than at the call site, so no path reaches `git clone`
    /// without it.
    pub fn clone_repository(
        &self,
        remote_url: &str,
        auth: &GitAuth,
    ) -> Result<(CloneOutcome, RemoteTarget)> {
        let target = remote_policy::validate_remote(remote_url)?;
        if target.scheme == RemoteScheme::Ssh && matches!(auth, GitAuth::Token(_)) {
            return Err(anyhow!("an ssh remote cannot authenticate with a token"));
        }
        if target.scheme == RemoteScheme::Https && matches!(auth, GitAuth::SshKey { .. }) {
            return Err(anyhow!(
                "an https remote cannot authenticate with an ssh key"
            ));
        }
        let handle = self.reference();
        std::fs::create_dir_all(&handle.work_tree).map_err(|e| anyhow!("create repo dir: {e}"))?;

        let out = self.run(
            None,
            &[
                "clone",
                "--no-checkout",
                "--",
                &target.url,
                &handle.work_tree.display().to_string(),
            ],
            auth,
        )?;
        ok_or_stderr(out, "git clone")?;

        let out = self.run(
            Some(&handle),
            &["symbolic-ref", "--short", "HEAD"],
            &GitAuth::None,
        )?;
        let default_branch = ok_or_stderr(out, "git symbolic-ref")?;
        Ok((
            CloneOutcome {
                default_branch,
                head_commit: self.head_commit(&handle)?,
            },
            target,
        ))
    }

    /// Adds the working worktree of a session on its own branch. `repo/` itself
    /// is never handed to a session.
    pub fn add_session_worktree(
        &self,
        session_id: &str,
        branch: &str,
        start_point: &str,
    ) -> Result<PathBuf> {
        validate_branch(branch)?;
        validate_start_point(start_point)?;
        let worktree = self.session_worktree(session_id)?;
        if let Some(parent) = worktree.parent() {
            std::fs::create_dir_all(parent).map_err(|e| anyhow!("create worktrees dir: {e}"))?;
        }
        let out = self.run(
            Some(&self.reference()),
            &[
                "worktree",
                "add",
                "-b",
                branch,
                &worktree.display().to_string(),
                start_point,
            ],
            &GitAuth::None,
        )?;
        ok_or_stderr(out, "git worktree add")?;
        Ok(worktree)
    }

    /// Removes a session worktree and forgets it. The branch survives — it
    /// carries the session's work and is subject to retention, not to this call.
    pub fn remove_session_worktree(&self, session_id: &str) -> Result<()> {
        let worktree = self.session_worktree(session_id)?;
        self.discard_worktree(&worktree)
    }

    /// Deletes a worktree directory and forgets its administrative entry.
    ///
    /// `git worktree remove` is NOT used: it validates the `.git` pointer file
    /// inside the worktree, and that file belongs to the agent. One line
    /// written there ("gitdir: /elsewhere") makes git refuse with "does not
    /// point back to", which would leave the session unclosable and its disk
    /// quota held forever — and, after a finalised merge, would report a merge
    /// that DID happen as a failure. The removal therefore uses the broker's
    /// own map: delete the directory we placed, then `git worktree prune`,
    /// which drops every administrative entry whose directory is gone.
    fn discard_worktree(&self, worktree: &Path) -> Result<()> {
        match std::fs::remove_dir_all(worktree) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(anyhow!("remove worktree {}: {e}", worktree.display()));
            }
        }
        let out = self.run(
            Some(&self.reference()),
            &["worktree", "prune"],
            &GitAuth::None,
        )?;
        ok_or_stderr(out, "git worktree prune")?;
        Ok(())
    }

    /// Porcelain status of a session worktree, one entry per change.
    pub fn status(&self, session_id: &str) -> Result<Vec<String>> {
        let handle = self.session(session_id)?;
        let out = self.run(
            Some(&handle),
            &["status", "--porcelain=v1", "-z"],
            &GitAuth::None,
        )?;
        let raw = ok_or_stderr(out, "git status")?;
        Ok(raw
            .split('\0')
            .filter(|entry| !entry.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// History of the handle's current branch, newest first.
    ///
    /// The fields are separated by US (0x1f) and the records by NUL, because a
    /// commit subject may contain anything a person can type — including the
    /// newline and the pipe a "readable" separator would rely on. `-z` with a
    /// `format:` pretty string is git's own way of saying that.
    pub fn log(&self, handle: &RepoHandle, path: &str, limit: u32) -> Result<Vec<LogEntry>> {
        let limit = limit.clamp(1, MAX_LOG_ENTRIES);
        let max_count = format!("--max-count={limit}");
        let mut args = vec![
            "log",
            "--no-color",
            "--no-decorate",
            "-z",
            max_count.as_str(),
            "--pretty=format:%H\x1f%h\x1f%an\x1f%aI\x1f%s",
        ];
        if !path.is_empty() {
            validate_repo_path(path)?;
            args.push("--");
            args.push(path);
        }
        let out = self.run(Some(handle), &args, &GitAuth::None)?;
        if !out.status.success() {
            let stderr = stderr_text(&out.stderr);
            // A branch with no commits is not an error: an empty repository has
            // an unborn HEAD and git says so on stderr.
            if stderr.contains("does not have any commits") {
                return Ok(Vec::new());
            }
            return Err(anyhow!("git log failed: {stderr}"));
        }
        let raw = String::from_utf8_lossy(&out.stdout);
        let mut entries = Vec::new();
        for record in raw.split('\0').filter(|r| !r.trim().is_empty()) {
            let mut fields = record.trim_start_matches('\n').splitn(5, '\x1f');
            let (Some(oid), Some(short_oid), Some(author), Some(date), Some(subject)) = (
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
            ) else {
                continue;
            };
            entries.push(LogEntry {
                oid: oid.to_string(),
                short_oid: short_oid.to_string(),
                author: author.to_string(),
                date: date.to_string(),
                subject: subject.to_string(),
            });
        }
        Ok(entries)
    }

    /// Local branches with their upstream and divergence.
    ///
    /// `%(upstream:track)` is asked for rather than `%(ahead-behind:HEAD)`:
    /// the latter needs git 2.41, and a node with an older git would silently
    /// report zeros instead of failing. Fields are one per line, which is safe
    /// because git forbids a newline in a ref name.
    pub fn branches(&self, handle: &RepoHandle) -> Result<Vec<BranchLine>> {
        let out = self.run(
            Some(handle),
            &[
                "for-each-ref",
                "--format=%(refname:short)\n%(HEAD)\n%(upstream:short)\n%(upstream:track)",
                "refs/heads/",
            ],
            &GitAuth::None,
        )?;
        if !out.status.success() {
            return Err(anyhow!(
                "git for-each-ref failed: {}",
                stderr_text(&out.stderr)
            ));
        }
        let raw = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = raw.lines().collect();
        let mut branches = Vec::new();
        for record in lines.chunks(4) {
            if record.len() < 4 || record[0].is_empty() {
                continue;
            }
            let (ahead, behind) = parse_track(record[3]);
            branches.push(BranchLine {
                name: record[0].to_string(),
                is_current: record[1].trim() == "*",
                upstream: (!record[2].is_empty()).then(|| record[2].to_string()),
                ahead,
                behind,
            });
        }
        Ok(branches)
    }

    pub fn head_commit(&self, handle: &RepoHandle) -> Result<String> {
        let out = self.run(Some(handle), &["rev-parse", "HEAD"], &GitAuth::None)?;
        ok_or_stderr(out, "git rev-parse HEAD")
    }

    /// Resolves a revision to its object id. `Ok(None)` means git does not know
    /// the name — for the operation journal that is evidence, not a failure:
    /// "the ref this operation was going to create does not exist".
    pub fn rev_parse(&self, handle: &RepoHandle, revision: &str) -> Result<Option<String>> {
        validate_start_point(revision)?;
        self.resolve(handle, revision)
    }

    /// Resolution itself, shared by every caller that has already validated its
    /// own kind of name (a revision, a reference, a `<commit>:<path>` pair).
    fn resolve(&self, handle: &RepoHandle, spec: &str) -> Result<Option<String>> {
        let out = self.run(
            Some(handle),
            &["rev-parse", "--verify", "--quiet", spec],
            &GitAuth::None,
        )?;
        if out.status.success() {
            return Ok(Some(
                String::from_utf8_lossy(&out.stdout).trim().to_string(),
            ));
        }
        let stderr = stderr_text(&out.stderr);
        if stderr.is_empty() {
            Ok(None)
        } else {
            Err(anyhow!("git rev-parse failed: {stderr}"))
        }
    }

    /// Tree and parents of a commit. This is what makes `CommitExists { tree,
    /// parent }` a verifiable postcondition rather than a guess (§13.1).
    pub fn commit_metadata(&self, handle: &RepoHandle, commit: &str) -> Result<Option<CommitMeta>> {
        validate_start_point(commit)?;
        let out = self.run(
            Some(handle),
            &["cat-file", "commit", commit],
            &GitAuth::None,
        )?;
        if !out.status.success() {
            return Ok(None);
        }
        let body = String::from_utf8_lossy(&out.stdout);
        let mut meta = CommitMeta {
            tree: String::new(),
            parents: Vec::new(),
        };
        for line in body.lines() {
            // The header ends at the first blank line; the message may contain
            // anything, including a line starting with "parent".
            if line.is_empty() {
                break;
            }
            if let Some(tree) = line.strip_prefix("tree ") {
                meta.tree = tree.trim().to_string();
            } else if let Some(parent) = line.strip_prefix("parent ") {
                meta.parents.push(parent.trim().to_string());
            }
        }
        if meta.tree.is_empty() {
            return Err(anyhow!("git cat-file returned a commit without a tree"));
        }
        Ok(Some(meta))
    }

    /// Whether an object is present in the repository — the `result_oids` probe
    /// of an interrupted commit or merge.
    pub fn object_exists(&self, handle: &RepoHandle, oid: &str) -> Result<bool> {
        validate_start_point(oid)?;
        let out = self.run(Some(handle), &["cat-file", "-e", oid], &GitAuth::None)?;
        Ok(out.status.success())
    }

    // ----- object database -------------------------------------------------

    /// Writes `content` into the object database and returns its blob id. This
    /// is the only content store Code Studio needs for patch material: the odb
    /// is already content-addressed, already shared by every worktree of the
    /// workspace, and already survives a restart.
    pub fn hash_object(&self, handle: &RepoHandle, content: &[u8]) -> Result<String> {
        let out = self.exec(
            Some(handle),
            &["hash-object", "-w", "--no-filters", "--stdin"],
            &GitAuth::None,
            &[],
            Some(content),
        )?;
        let oid = ok_or_stderr(out, "git hash-object")?;
        validate_oid(&oid)?;
        Ok(oid)
    }

    /// Reads an object back verbatim. Bytes, not text: repository content is
    /// not required to be UTF-8 and must round-trip through a commit unchanged.
    pub fn cat_file(&self, handle: &RepoHandle, oid: &str) -> Result<Vec<u8>> {
        validate_oid(oid)?;
        // The size is asked for FIRST. `exec` caps what it keeps (§7.8), and a
        // silently truncated blob is worse than no blob at all: it would be
        // committed back as the file's new content. An oversized object is a
        // refusal with the size in it, not half a file.
        let sized = self.run(Some(handle), &["cat-file", "-s", oid], &GitAuth::None)?;
        let size: usize = ok_or_stderr(sized, "git cat-file -s")?
            .trim()
            .parse()
            .map_err(|e| anyhow!("git cat-file -s returned no size: {e}"))?;
        if size > MAX_BLOB_BYTES {
            return Err(anyhow!(
                "object {oid} is {size} bytes, over the {MAX_BLOB_BYTES}-byte ceiling for one \
                 broker read"
            ));
        }
        let out = self.exec_capped(
            Some(handle),
            &["cat-file", "blob", oid],
            &GitAuth::None,
            &[],
            None,
            MAX_BLOB_BYTES,
        )?;
        if !out.status.success() {
            return Err(anyhow!(
                "git cat-file failed: {}",
                stderr_text(&out.stderr)
            ));
        }
        Ok(out.stdout)
    }

    /// Blob id of `path` in `commit`, or `None` when the path is not in that
    /// tree. This is how `Absent` is decided against the frozen base rather
    /// than against a table that only lists CHANGED files.
    pub fn blob_in_commit(
        &self,
        handle: &RepoHandle,
        commit: &str,
        path: &str,
    ) -> Result<Option<String>> {
        validate_start_point(commit)?;
        validate_repo_path(path)?;
        let Some(oid) = self.resolve(handle, &format!("{commit}:{path}"))? else {
            return Ok(None);
        };
        validate_oid(&oid)?;
        Ok(Some(oid))
    }

    /// Recursive listing of a tree: mode, blob id and path per entry.
    pub fn list_tree(&self, handle: &RepoHandle, tree: &str) -> Result<Vec<TreeEntry>> {
        validate_start_point(tree)?;
        let out = self.run(Some(handle), &["ls-tree", "-r", "-z", tree], &GitAuth::None)?;
        if !out.status.success() {
            return Err(anyhow!(
                "git ls-tree failed: {}",
                stderr_text(&out.stderr)
            ));
        }
        let raw = String::from_utf8_lossy(&out.stdout).to_string();
        let mut entries = Vec::new();
        for record in raw.split('\0').filter(|r| !r.is_empty()) {
            let (meta, path) = record
                .split_once('\t')
                .ok_or_else(|| anyhow!("unparsable ls-tree record"))?;
            let mut fields = meta.split_whitespace();
            let mode = fields.next().unwrap_or_default().to_string();
            let _kind = fields.next();
            let oid = fields.next().unwrap_or_default().to_string();
            if mode.is_empty() || oid.is_empty() {
                return Err(anyhow!("unparsable ls-tree record"));
            }
            entries.push(TreeEntry {
                mode,
                oid,
                path: path.to_string(),
            });
        }
        Ok(entries)
    }

    /// Freezes the current worktree content as a tree object, WITHOUT touching
    /// the worktree's own index. The staging happens in a temporary index in
    /// the broker directory, so an agent editing during a review never sees a
    /// staged state it did not create, and the review always compares two
    /// immutable trees instead of a tree against a moving directory.
    pub fn snapshot_worktree(&self, handle: &RepoHandle, base_commit: &str) -> Result<String> {
        validate_start_point(base_commit)?;
        let index = self.temp_index()?;
        let env = index.env();
        let out = self.exec(
            Some(handle),
            &["read-tree", base_commit],
            &GitAuth::None,
            &env,
            None,
        )?;
        ok_or_stderr(out, "git read-tree")?;
        let out = self.exec(Some(handle), &["add", "-A"], &GitAuth::None, &env, None)?;
        ok_or_stderr(out, "git add -A")?;
        let out = self.exec(Some(handle), &["write-tree"], &GitAuth::None, &env, None)?;
        let tree = ok_or_stderr(out, "git write-tree")?;
        validate_oid(&tree)?;
        Ok(tree)
    }

    /// Name-status difference between two committishes or trees. Rename
    /// detection is deliberately OFF: a rename in Code Studio is an explicit
    /// recorded edit, not a similarity guess made at review time.
    pub fn diff_name_status(
        &self,
        handle: &RepoHandle,
        base: &str,
        head: &str,
    ) -> Result<Vec<DiffEntry>> {
        validate_start_point(base)?;
        validate_start_point(head)?;
        let out = self.run(
            Some(handle),
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-renames",
                "--name-status",
                "-z",
                base,
                head,
            ],
            &GitAuth::None,
        )?;
        if !out.status.success() {
            return Err(anyhow!(
                "git diff --name-status failed: {}",
                stderr_text(&out.stderr)
            ));
        }
        let raw = String::from_utf8_lossy(&out.stdout).to_string();
        let mut tokens = raw.split('\0').filter(|t| !t.is_empty());
        let mut entries = Vec::new();
        while let Some(status) = tokens.next() {
            let code = status
                .chars()
                .next()
                .ok_or_else(|| anyhow!("empty diff status"))?;
            // R and C carry two paths even with rename detection off, because a
            // caller may pass its own diff arguments through this parser later.
            let (old_path, path) = if code == 'R' || code == 'C' {
                let old = tokens
                    .next()
                    .ok_or_else(|| anyhow!("diff status without a source path"))?;
                let new = tokens
                    .next()
                    .ok_or_else(|| anyhow!("diff status without a target path"))?;
                (Some(old.to_string()), new.to_string())
            } else {
                let path = tokens
                    .next()
                    .ok_or_else(|| anyhow!("diff status without a path"))?;
                (None, path.to_string())
            };
            entries.push(DiffEntry {
                status: code,
                path,
                old_path,
            });
        }
        Ok(entries)
    }

    /// Unified diff of ONE path between two committishes or trees.
    ///
    /// `--no-textconv` is load-bearing rather than cosmetic: a `.gitattributes`
    /// textconv would show converted text, and hunks cut from converted text
    /// would never apply back to the real blob.
    pub fn diff_patch(
        &self,
        handle: &RepoHandle,
        base: &str,
        head: &str,
        path: &str,
    ) -> Result<String> {
        validate_start_point(base)?;
        validate_start_point(head)?;
        validate_repo_path(path)?;
        let out = self.run(
            Some(handle),
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-renames",
                "--no-color",
                "--unified=3",
                base,
                head,
                "--",
                path,
            ],
            &GitAuth::None,
        )?;
        if !out.status.success() {
            return Err(anyhow!(
                "git diff failed: {}",
                stderr_text(&out.stderr)
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    /// Composes a patch onto the content the path has in `base_commit`, using
    /// git's own three-way merge on a TEMPORARY index. Nothing is written to
    /// the worktree, so a partial acceptance cannot race the agent's editor.
    ///
    /// `Ok(None)` is the unclean composition of §13.2 — overlapping contexts.
    /// It is reported, never resolved by guessing.
    pub fn apply_hunks(
        &self,
        handle: &RepoHandle,
        base_commit: &str,
        path: &str,
        patch: &str,
    ) -> Result<Option<String>> {
        validate_start_point(base_commit)?;
        validate_repo_path(path)?;
        let index = self.temp_index()?;
        let env = index.env();
        let out = self.exec(
            Some(handle),
            &["read-tree", base_commit],
            &GitAuth::None,
            &env,
            None,
        )?;
        ok_or_stderr(out, "git read-tree")?;

        let out = self.exec(
            Some(handle),
            &["apply", "--cached", "--3way", "--whitespace=nowarn"],
            &GitAuth::None,
            &env,
            Some(patch.as_bytes()),
        )?;
        if !out.status.success() {
            return Ok(None);
        }
        // A three-way apply can succeed and still leave conflict stages; git's
        // own messages are localised, so the index is what we believe.
        let unmerged = self.exec(
            Some(handle),
            &["ls-files", "-u", "-z"],
            &GitAuth::None,
            &env,
            None,
        )?;
        let unmerged = ok_or_stderr(unmerged, "git ls-files -u")?;
        if !unmerged.is_empty() {
            return Ok(None);
        }

        let staged = self.exec(
            Some(handle),
            &["ls-files", "-s", "-z"],
            &GitAuth::None,
            &env,
            None,
        )?;
        if !staged.status.success() {
            return Err(anyhow!(
                "git ls-files -s failed: {}",
                stderr_text(&staged.stderr)
            ));
        }
        let raw = String::from_utf8_lossy(&staged.stdout).to_string();
        for record in raw.split('\0').filter(|r| !r.is_empty()) {
            let Some((meta, entry_path)) = record.split_once('\t') else {
                continue;
            };
            if entry_path != path {
                continue;
            }
            let oid = meta.split_whitespace().nth(1).unwrap_or_default();
            validate_oid(oid)?;
            return Ok(Some(oid.to_string()));
        }
        // The patch applied but left no entry at the path: that is a delete,
        // which partial acceptance does not express.
        Ok(None)
    }

    // ----- commits ---------------------------------------------------------

    /// The tree a commit built from this spec WOULD have, without publishing
    /// anything.
    ///
    /// A commit's outcome — its tree and its parent — is determined by the
    /// accepted blobs and the base commit before git is asked to do anything,
    /// so the journal can state it up front and a crash between "operation
    /// opened" and "commit published" is resolvable by looking for that exact
    /// commit instead of asking a person (§13.1).
    pub fn plan_tree(&self, handle: &RepoHandle, spec: &CommitSpec) -> Result<String> {
        Ok(self.stage_accepted_tree(handle, spec)?.1)
    }

    /// Writes every accepted blob and composes them onto the base tree in a
    /// TEMPORARY index, returning the blob ids and the resulting tree. Git
    /// object writes are content addressed, so running this before the commit
    /// and again inside it produces the same ids and stores nothing twice.
    fn stage_accepted_tree(
        &self,
        handle: &RepoHandle,
        spec: &CommitSpec,
    ) -> Result<(Vec<(String, String)>, String)> {
        validate_oid(&spec.base_commit)?;
        // 1. Every accepted file becomes an object first. The returned id is
        //    the proof of what went in, so it is reported even when a later
        //    step fails.
        let mut blob_oids: Vec<(String, String)> = Vec::new();
        for file in &spec.files {
            validate_repo_path(&file.path)?;
            if let Some(old) = &file.old_path {
                validate_repo_path(old)?;
            }
            if let CommitChange::Write { content } = &file.change {
                validate_file_mode(&file.mode)?;
                let oid = self.hash_object(handle, content)?;
                blob_oids.push((file.path.clone(), oid));
            }
        }

        // 2. A temporary index seeded with the base tree. The worktree's own
        //    index belongs to the agent and is never involved.
        let index = self.temp_index()?;
        let env = index.env();
        let out = self.exec(
            Some(handle),
            &["read-tree", &spec.base_commit],
            &GitAuth::None,
            &env,
            None,
        )?;
        ok_or_stderr(out, "git read-tree")?;

        // TWO PHASES, and the order is load-bearing. A rename is BOTH
        // operations: dropping the old path and adding the new one. Applied per
        // file in list order, a patch set that renames `a` to `b` AND creates a
        // new `a` loses the new `a` — the rename entry, coming second, removes
        // the path the earlier entry had just added. Every removal therefore
        // happens before any addition, so a path that the accepted patch set
        // recreates survives (§11.5: what is committed is what the human saw).
        for file in &spec.files {
            let removals = file
                .old_path
                .iter()
                .map(String::as_str)
                .chain(matches!(file.change, CommitChange::Delete).then_some(file.path.as_str()));
            for path in removals {
                let out = self.exec(
                    Some(handle),
                    &["update-index", "--force-remove", path],
                    &GitAuth::None,
                    &env,
                    None,
                )?;
                ok_or_stderr(out, "git update-index --force-remove")?;
            }
        }
        for file in &spec.files {
            if !matches!(file.change, CommitChange::Write { .. }) {
                continue;
            }
            let oid = blob_oids
                .iter()
                .find(|(p, _)| *p == file.path)
                .map(|(_, oid)| oid.clone())
                .ok_or_else(|| anyhow!("no blob was written for {}", file.path))?;
            let cacheinfo = format!("{},{},{}", file.mode, oid, file.path);
            let out = self.exec(
                Some(handle),
                &["update-index", "--add", "--cacheinfo", &cacheinfo],
                &GitAuth::None,
                &env,
                None,
            )?;
            ok_or_stderr(out, "git update-index --cacheinfo")?;
        }

        // 3. The tree of exactly what was accepted.
        let out = self.exec(Some(handle), &["write-tree"], &GitAuth::None, &env, None)?;
        let tree_oid = ok_or_stderr(out, "git write-tree")?;
        validate_oid(&tree_oid)?;
        Ok((blob_oids, tree_oid))
    }

    /// Builds a commit from CONTENT and publishes it with an atomic
    /// compare-and-swap on the branch — the four steps of §11.5.
    ///
    /// The worktree is not read and not synchronised afterwards. Whatever the
    /// agent changed on disk during the review is simply the material of the
    /// NEXT patch set.
    pub fn build_commit(&self, handle: &RepoHandle, spec: &CommitSpec) -> Result<CommitOutcome> {
        validate_branch(&spec.branch)?;
        // Parents are OIDs, never revision expressions. `validate_start_point`
        // accepts `main@{5}` and `HEAD^{}`, which resolve differently depending
        // on when git happens to look — a commit built against a moving target
        // is not the commit that was reviewed (§11.5).
        validate_oid(&spec.base_commit)?;
        if let Some(parent) = &spec.extra_parent {
            validate_oid(parent)?;
        }
        if let Some(old) = &spec.expected_old {
            validate_oid(old)?;
        }
        validate_identity(&spec.author)?;
        validate_identity(&spec.committer)?;

        // 1-3. Content becomes objects and a tree. Separated out because the
        //       JOURNAL needs that tree before the commit is published: §13.1
        //       makes a commit's outcome verifiable, and an operation opened
        //       with an outcome nobody can check is an operation that reports
        //       "nobody knows" after a crash.
        let (blob_oids, tree_oid) = self.stage_accepted_tree(handle, spec)?;

        // 4. The commit itself. The identity comes from the server, never from
        //    a config file that travelled with the repository.
        let mut args: Vec<String> = vec!["commit-tree".into(), tree_oid.clone()];
        args.push("-p".into());
        args.push(spec.base_commit.clone());
        if let Some(parent) = &spec.extra_parent {
            args.push("-p".into());
            args.push(parent.clone());
        }
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = self.exec(
            Some(handle),
            &argv,
            &GitAuth::None,
            &identity_env(spec),
            Some(spec.message.as_bytes()),
        )?;
        let commit_oid = ok_or_stderr(out, "git commit-tree")?;
        validate_oid(&commit_oid)?;

        // 5. Atomic compare-and-swap. A stale `expected_old` is an error, never
        //    an overwrite: the accepted result was reviewed against THAT base.
        let ref_name = format!("refs/heads/{}", spec.branch);
        let ref_before = self.read_ref(handle, &ref_name)?;
        let expected = spec.expected_old.clone().unwrap_or_default();
        let out = self.run(
            Some(handle),
            &["update-ref", &ref_name, &commit_oid, &expected],
            &GitAuth::None,
        )?;
        ok_or_stderr(out, "git update-ref")?;

        Ok(CommitOutcome {
            blob_oids,
            tree_oid,
            commit_oid: commit_oid.clone(),
            ref_name,
            ref_before,
            ref_after: commit_oid,
        })
    }

    /// Value of a reference, or `None` when it does not exist.
    pub fn read_ref(&self, handle: &RepoHandle, ref_name: &str) -> Result<Option<String>> {
        validate_ref_name(ref_name)?;
        let Some(oid) = self.resolve(handle, ref_name)? else {
            return Ok(None);
        };
        validate_oid(&oid)?;
        Ok(Some(oid))
    }

    // ----- merge (§11.6) ---------------------------------------------------

    pub fn integration_worktree(&self, session_id: &str) -> Result<PathBuf> {
        paths::validate_session_id(session_id)?;
        Ok(self.root.join("worktrees").join(integration_name(session_id)))
    }

    /// Handle of the integration worktree. As everywhere else, the git
    /// directory is derived from the broker's own layout, never from the
    /// pointer file sitting inside the worktree.
    pub fn integration(&self, session_id: &str) -> Result<RepoHandle> {
        paths::validate_session_id(session_id)?;
        let reference = self.reference();
        Ok(RepoHandle {
            git_dir: reference
                .git_dir
                .join("worktrees")
                .join(integration_name(session_id)),
            work_tree: self.integration_worktree(session_id)?,
        })
    }

    /// Creates the integration worktree DETACHED at `expected_old`.
    ///
    /// Never `git worktree add <path> <target_branch>`: that form checks the
    /// target branch out into the worktree, so `git merge` would move the
    /// target reference immediately — before the tests and before anyone
    /// accepted the result. Detached, `merge` moves only the worktree's HEAD.
    ///
    /// The private ref is written here, at the base, rather than only after a
    /// successful merge: a crash between `worktree add` and `merge` would
    /// otherwise leave an attempt whose base nobody can name.
    pub fn add_integration_worktree(
        &self,
        session_id: &str,
        op_id: &str,
        expected_old: &str,
    ) -> Result<PathBuf> {
        paths::validate_session_id(session_id)?;
        validate_op_id(op_id)?;
        // The tip a merge is verified against is a commit, not a name. A branch
        // name here would check out a MOVING target: `--detach` still detaches,
        // but the private ref and the later compare-and-swap would be anchored
        // to whatever the name meant at two different moments.
        validate_oid(expected_old)?;
        let worktree = self.integration_worktree(session_id)?;
        if worktree.exists() {
            return Err(anyhow!(
                "session {session_id} already has an integration worktree; \
                 finish or abandon that merge first"
            ));
        }
        if let Some(parent) = worktree.parent() {
            std::fs::create_dir_all(parent).map_err(|e| anyhow!("create worktrees dir: {e}"))?;
        }
        let out = self.run(
            Some(&self.reference()),
            &[
                "worktree",
                "add",
                "--detach",
                &worktree.display().to_string(),
                expected_old,
            ],
            &GitAuth::None,
        )?;
        ok_or_stderr(out, "git worktree add --detach")?;
        self.write_private_ref(op_id, expected_old)?;
        Ok(worktree)
    }

    /// Merges a session branch into the integration worktree. A conflict is a
    /// RESULT: the worktree keeps the half-merged tree so the revision run of
    /// §16.3 has something to work on.
    pub fn merge_into_integration(
        &self,
        session_id: &str,
        source_branch: &str,
    ) -> Result<MergeOutcome> {
        validate_branch(source_branch)?;
        let handle = self.integration(session_id)?;
        if !handle.work_tree.exists() {
            return Err(anyhow!("session {session_id} has no integration worktree"));
        }
        // Both tips as they are BEFORE the merge: what the merge did to them is
        // the only honest way to tell a fast-forward from a composed merge.
        let base = self.resolve(&handle, "HEAD")?;
        let source_tip = self
            .resolve(&handle, &format!("refs/heads/{source_branch}"))?
            .ok_or_else(|| anyhow!("branch {source_branch} does not exist"))?;
        let message = format!("Merge {source_branch}");
        let identity = CommitIdentity {
            name: "TentaFlow Code Studio".into(),
            email: "code-studio@tentaflow.local".into(),
        };
        let env = merge_identity_env(&identity);
        let out = self.exec(
            Some(&handle),
            &["merge", "--no-edit", "-m", &message, source_branch],
            &GitAuth::None,
            &env,
            None,
        )?;
        if out.status.success() {
            let merge_head = self.head_commit(&handle)?;
            // Fast-forward is a property of THE MERGE — "the base was already an
            // ancestor of the source" — not of the source commit. Asking whether
            // HEAD has a second parent answered a different question: a session
            // branch whose own tip is a merge commit reported every genuine
            // fast-forward as a non-fast-forward, and §11.6 turns that into a
            // question for a human that never needed asking.
            // Nothing was composed when HEAD ended up at one of the two tips it
            // started from: at the source (git fast-forwarded) or where it was
            // (already up to date).
            let fast_forward = merge_head == source_tip || Some(&merge_head) == base.as_ref();
            return Ok(MergeOutcome::Clean {
                fast_forward,
                merge_head,
            });
        }

        let unmerged = self.run(
            Some(&handle),
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--name-only",
                "--diff-filter=U",
                "-z",
            ],
            &GitAuth::None,
        )?;
        let listed = ok_or_stderr(unmerged, "git diff --diff-filter=U")?;
        let paths: Vec<String> = listed
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect();
        if paths.is_empty() {
            // No conflicted path means the merge failed for another reason —
            // a dirty worktree, an unreadable ref. That IS an error.
            return Err(anyhow!(
                "git merge failed: {}",
                stderr_text(&out.stderr)
            ));
        }
        Ok(MergeOutcome::Conflict { paths })
    }

    /// Anchors an integration result under `refs/code-studio/integration/<op>`
    /// so it survives a restart and garbage collection (§11.6 step 2).
    pub fn write_private_ref(&self, op_id: &str, commit: &str) -> Result<()> {
        validate_op_id(op_id)?;
        validate_start_point(commit)?;
        let ref_name = integration_ref(op_id);
        let out = self.run(
            Some(&self.reference()),
            &["update-ref", &ref_name, commit],
            &GitAuth::None,
        )?;
        ok_or_stderr(out, "git update-ref")?;
        Ok(())
    }

    pub fn delete_private_ref(&self, op_id: &str) -> Result<()> {
        validate_op_id(op_id)?;
        let ref_name = integration_ref(op_id);
        let out = self.run(
            Some(&self.reference()),
            &["update-ref", "-d", &ref_name],
            &GitAuth::None,
        )?;
        ok_or_stderr(out, "git update-ref -d")?;
        Ok(())
    }

    /// Publishes a merge from the ACCEPTED blobs, exactly the way an ordinary
    /// commit is built (§11.6 step 5). Building it from the integration
    /// worktree instead would ship whatever the agent left there while
    /// resolving the conflict — bypassing the review, against §2.2 pkt 5.
    ///
    /// The spec must carry two parents and an `expected_old`: a merge that
    /// cannot say which target tip it was verified against has nothing to
    /// compare-and-swap on.
    pub fn finalize_merge(&self, spec: &CommitSpec) -> Result<CommitOutcome> {
        if spec.extra_parent.is_none() {
            return Err(anyhow!(
                "a merge commit needs a second parent; use build_commit for an ordinary commit"
            ));
        }
        if spec.expected_old.is_none() {
            return Err(anyhow!(
                "a merge must name the target tip it was verified against"
            ));
        }
        self.build_commit(&self.reference(), spec)
    }

    /// Removes the integration worktree and its private ref. Call this ONLY
    /// after a committed `update-ref`, after the user explicitly abandoned the
    /// merge, or when the session closes — removing it on a conflict or a
    /// rejection would take away the state the next run is supposed to fix.
    pub fn remove_integration_worktree(&self, session_id: &str, op_id: &str) -> Result<()> {
        paths::validate_session_id(session_id)?;
        validate_op_id(op_id)?;
        let worktree = self.integration_worktree(session_id)?;
        self.discard_worktree(&worktree)?;
        self.delete_private_ref(op_id)
    }

    // ----- remotes ---------------------------------------------------------

    /// Pushes one branch to a remote. Never forced: a Code Studio session may
    /// publish its own work, not overwrite somebody else's.
    pub fn push_branch(
        &self,
        handle: &RepoHandle,
        remote: &str,
        branch: &str,
        auth: &GitAuth,
    ) -> Result<()> {
        let target = self.resolve_remote(handle, remote)?;
        validate_branch(branch)?;
        let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
        let out = self.run(
            Some(handle),
            &["push", "--atomic", &target.url, &refspec],
            auth,
        )?;
        ok_or_stderr(out, "git push")?;
        Ok(())
    }

    /// Fetches one branch and returns the commit it points at. Nothing local
    /// moves — the caller decides what to do with it.
    pub fn fetch_branch(
        &self,
        handle: &RepoHandle,
        remote: &str,
        branch: &str,
        auth: &GitAuth,
    ) -> Result<String> {
        let target = self.resolve_remote(handle, remote)?;
        validate_branch(branch)?;
        let out = self.run(
            Some(handle),
            &["fetch", "--no-tags", &target.url, branch],
            auth,
        )?;
        ok_or_stderr(out, "git fetch")?;
        let out = self.run(Some(handle), &["rev-parse", "FETCH_HEAD"], &GitAuth::None)?;
        let oid = ok_or_stderr(out, "git rev-parse FETCH_HEAD")?;
        validate_oid(&oid)?;
        Ok(oid)
    }

    /// Fetches and fast-forwards the checked-out branch. Fast-forward ONLY:
    /// anything else is a merge, and a merge is a user decision that goes
    /// through the integration worktree (§11.6), not through a pull.
    pub fn pull_branch(
        &self,
        handle: &RepoHandle,
        remote: &str,
        branch: &str,
        auth: &GitAuth,
    ) -> Result<String> {
        let fetched = self.fetch_branch(handle, remote, branch, auth)?;
        let out = self.run(
            Some(handle),
            &["merge", "--ff-only", &fetched],
            &GitAuth::None,
        )?;
        ok_or_stderr(out, "git merge --ff-only")?;
        self.head_commit(handle)
    }

    /// Turns whatever the caller called the remote into a policy-checked
    /// target.
    ///
    /// A URL is checked and used. A NAME (`origin`, which is what the agent
    /// tool surface passes by default) is looked up in the repository's own
    /// configuration first — the broker keeps no remotes of its own, so
    /// refusing a name outright made `core.git_push` and `core.git_sync`
    /// impossible to call with their own defaults. The URL that comes back is
    /// then put through the identical policy: config is data the repository
    /// carries, so it is never trusted, only resolved.
    ///
    /// Public because the PEP has to judge the address this call will really
    /// dial (§11.4) BEFORE the operation runs, and asking a second copy of the
    /// rules is how the authorized remote and the dialed remote drift apart.
    pub fn resolve_remote(
        &self,
        handle: &RepoHandle,
        remote: &str,
    ) -> Result<remote_policy::RemoteTarget> {
        let remote = remote.trim();
        if remote.is_empty() {
            return Err(anyhow!("no remote was named"));
        }
        if !is_remote_name(remote) {
            return remote_policy::validate_remote(remote);
        }
        let key = format!("remote.{remote}.url");
        let out = self.run(Some(handle), &["config", "--get", &key], &GitAuth::None)?;
        if !out.status.success() {
            return Err(anyhow!(
                "this workspace has no remote called {remote}; pass the repository url instead"
            ));
        }
        let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if url.is_empty() {
            return Err(anyhow!("remote {remote} has no url configured"));
        }
        remote_policy::validate_remote(&url)
    }

    /// A temporary index in the BROKER directory. Never inside a worktree: the
    /// agent may write there, and an index is a file git trusts.
    fn temp_index(&self) -> Result<TempIndex> {
        let dir = self.broker_dir()?.join("index");
        std::fs::create_dir_all(&dir).map_err(|e| anyhow!("git broker index dir: {e}"))?;
        Ok(TempIndex {
            path: dir.join(format!("{}.idx", uuid::Uuid::new_v4())),
        })
    }

    /// Runs one git invocation with the full hardening in place.
    fn run(&self, handle: Option<&RepoHandle>, args: &[&str], auth: &GitAuth) -> Result<Output> {
        self.exec(handle, args, auth, &[], None)
    }

    /// The single place a git process is started. `extra_env` and `stdin` exist
    /// so plumbing calls (temporary index, author identity, object content)
    /// reuse this hardening instead of building their own `Command`.
    fn exec(
        &self,
        handle: Option<&RepoHandle>,
        args: &[&str],
        auth: &GitAuth,
        extra_env: &[(String, String)],
        stdin: Option<&[u8]>,
    ) -> Result<Output> {
        self.exec_capped(handle, args, auth, extra_env, stdin, MAX_STDOUT_BYTES)
    }

    /// `exec` with an explicit ceiling on what is kept from stdout. Only the
    /// object reader raises it: a blob is CONTENT, and §7.8's 1 MiB is a limit
    /// on how much a command may say, not on how large a file may be.
    #[allow(clippy::too_many_arguments)]
    fn exec_capped(
        &self,
        handle: Option<&RepoHandle>,
        args: &[&str],
        auth: &GitAuth,
        extra_env: &[(String, String)],
        stdin: Option<&[u8]>,
        stdout_cap: usize,
    ) -> Result<Output> {
        let hooks = self.empty_hooks_dir()?;
        let mut inv = Invocation::default();
        self.apply_auth(auth, &mut inv)?;

        let mut argv: Vec<String> = Vec::new();
        if let Some(handle) = handle {
            argv.push("--git-dir".into());
            argv.push(handle.git_dir.display().to_string());
            argv.push("--work-tree".into());
            argv.push(handle.work_tree.display().to_string());
        }
        let git_dir = handle
            .map(|handle| handle.git_dir.clone())
            .unwrap_or_else(|| self.reference().git_dir);
        for setting in hardening_args(&hooks)
            .into_iter()
            .chain(repository_driver_overrides(&git_dir)?)
        {
            argv.push("-c".into());
            argv.push(setting);
        }
        argv.extend(args.iter().map(|a| a.to_string()));

        let mut command = Command::new("git");
        command
            .args(argv.iter().map(OsStr::new))
            // Global and system config could re-enable a helper or a proxy.
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ALLOW_PROTOCOL", "https:ssh")
            // An inherited GIT_DIR/GIT_WORK_TREE would silently override the
            // explicit flags above.
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
            .env_remove("GIT_CONFIG")
            .env_remove("GIT_CONFIG_COUNT")
            // An inherited index would make a plumbing call operate on state
            // the broker did not create, and an inherited identity would sign
            // a commit with whoever launched the process.
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_AUTHOR_NAME")
            .env_remove("GIT_AUTHOR_EMAIL")
            .env_remove("GIT_AUTHOR_DATE")
            .env_remove("GIT_COMMITTER_NAME")
            .env_remove("GIT_COMMITTER_EMAIL")
            .env_remove("GIT_COMMITTER_DATE")
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in &inv.envs {
            command.env(key, value);
        }
        for (key, value) in extra_env {
            command.env(key, value);
        }
        let mut child = command
            .spawn()
            .map_err(|e| anyhow!("git is not available on this node: {e}"))?;

        // Three concurrent pipes, three concurrent movers. Writing the whole
        // patch to stdin before reading anything deadlocks the moment git
        // answers with more than a pipe buffer (64 KiB) while we are still
        // writing — `git apply --3way` on a large conflicting patch does
        // exactly that, and it would hang a request thread forever.
        let writer = stdin.map(|input| {
            let mut pipe = child.stdin.take();
            let input = input.to_vec();
            std::thread::spawn(move || {
                if let Some(pipe) = pipe.as_mut() {
                    use std::io::Write;
                    let _ = pipe.write_all(&input);
                }
                // Dropping the handle closes the pipe; without it git waits for
                // an end of input that never comes.
                drop(pipe);
            })
        });
        let stdout = child
            .stdout
            .take()
            .map(|pipe| std::thread::spawn(move || drain_capped(pipe, stdout_cap)));
        let stderr = child.stderr.take().map(|pipe| {
            std::thread::spawn(move || drain_capped(pipe, MAX_STDERR_BYTES))
        });

        if let Some(writer) = writer {
            writer.join().map_err(|_| anyhow!("git stdin writer panicked"))?;
        }
        let stdout = match stdout {
            Some(handle) => handle
                .join()
                .map_err(|_| anyhow!("git stdout reader panicked"))?
                .map_err(|e| anyhow!("read git stdout: {e}"))?,
            None => Vec::new(),
        };
        let stderr = match stderr {
            Some(handle) => handle
                .join()
                .map_err(|_| anyhow!("git stderr reader panicked"))?
                .map_err(|e| anyhow!("read git stderr: {e}"))?,
            None => Vec::new(),
        };
        let status = child.wait().map_err(|e| anyhow!("wait for git: {e}"))?;
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }

    /// Prepares the environment for one authenticated call.
    ///
    /// The token never reaches argv, the URL or the process environment: it is
    /// written to a 0600 file that a 0700 `GIT_ASKPASS` helper prints, and both
    /// are deleted when the invocation is dropped — whatever the outcome. This
    /// is the transitional variant named in §11.3; the durable one is a broker
    /// socket, which arrives with the sandbox shim.
    fn apply_auth(&self, auth: &GitAuth, inv: &mut Invocation) -> Result<()> {
        // Every scratch file is named per invocation, the way `temp_index`
        // already is. With fixed names two authenticated operations on one
        // workspace — a push while a fetch runs, two sessions, a retry —
        // overwrite each other's material, and the first to finish deletes the
        // file the other one is still authenticating with.
        let nonce = uuid::Uuid::new_v4();
        match auth {
            GitAuth::None => {}
            GitAuth::Token(token) => {
                let dir = self.broker_dir()?;
                let secret = dir.join(format!("askpass-{nonce}.secret"));
                let helper = dir.join(format!("askpass-{nonce}.sh"));
                write_private_file(&secret, token, false)?;
                write_private_file(
                    &helper,
                    &format!("#!/bin/sh\ncat {}\n", shell_single_quote(&secret)),
                    true,
                )?;
                inv.envs
                    .push(("GIT_ASKPASS".into(), helper.display().to_string()));
                inv.scratch.push(secret);
                inv.scratch.push(helper);
            }
            GitAuth::SshKey {
                private_key,
                known_host,
            } => {
                let dir = self.broker_dir()?;
                // `accept-new` is refused on purpose: it trusts whatever
                // answers the first time. The fingerprint is pinned when the
                // repository is added, shown to the user, and enforced after.
                // The refusal comes BEFORE the private key is written, so a
                // remote without a pinned host key never puts key material on
                // disk at all — not even for the instant until `Invocation` is
                // dropped.
                let known_host = known_host.as_ref().ok_or_else(|| {
                    anyhow!("ssh remote has no pinned host key; pin it before cloning")
                })?;
                let key = dir.join(format!("id-{nonce}"));
                write_private_file(&key, private_key, false)?;
                inv.scratch.push(key.clone());

                let known = dir.join(format!("known_hosts-{nonce}"));
                write_private_file(&known, &format!("{known_host}\n"), false)?;
                let ssh = format!(
                    "ssh -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes -o BatchMode=yes \
                     -i {} -o UserKnownHostsFile={}",
                    shell_single_quote(&key),
                    shell_single_quote(&known)
                );
                inv.scratch.push(known);
                inv.envs.push(("GIT_SSH_COMMAND".into(), ssh));
            }
        }
        Ok(())
    }
}

/// Neutralises every command the REPOSITORY's own configuration could ask git
/// to run.
///
/// `.gitattributes` inside a repository selects drivers by name — `diff=x`,
/// `filter=x`, `merge=x` — and the driver's COMMAND comes from configuration.
/// Global and system config are `/dev/null` here, but `repo/.git/config` is not
/// something the broker can point elsewhere: git needs it. A repository that
/// carries `[filter "x"] smudge = ...` therefore ran that command on the first
/// checkout, and `[diff "x"] textconv = ...` on the first diff — §11.2 says
/// neither may happen.
///
/// So every driver the config NAMES is overridden on the command line, where
/// `-c` beats the file, with an empty command: git then fails the driver closed
/// instead of executing it. A driver whose name is not a plain git key is not
/// silently skipped — the invocation is refused, because a name that cannot be
/// neutralised is a name that would still run.
fn repository_driver_overrides(git_dir: &Path) -> Result<Vec<String>> {
    let mut text = String::new();
    for path in config_paths(git_dir) {
        if let Ok(part) = std::fs::read_to_string(&path) {
            text.push_str(&part);
            text.push('\n');
        }
    }
    let mut overrides = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(section) = line.strip_prefix('[').and_then(|r| r.split(']').next()) else {
            continue;
        };
        let mut parts = section.splitn(2, '"');
        let kind = parts.next().unwrap_or("").trim().to_ascii_lowercase();
        let Some(name) = parts.next().and_then(|rest| rest.split('"').next()) else {
            continue;
        };
        let keys: &[&str] = match kind.as_str() {
            "filter" => &["clean", "smudge", "process"],
            "diff" => &["textconv", "command"],
            "merge" => &["driver"],
            _ => continue,
        };
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            return Err(anyhow!(
                "this repository declares a {kind} driver whose name cannot be disabled; \
                 refusing to run git against it"
            ));
        }
        for key in keys {
            overrides.push(format!("{kind}.{name}.{key}="));
        }
    }
    Ok(overrides)
}

/// Every configuration file the invocation will read from the repository.
///
/// A WORKTREE's git directory (`repo/.git/worktrees/<id>`) holds no `config` of
/// its own: git reads the one in the common directory, which the `commondir`
/// file points at. Looking only next to `git_dir` therefore found nothing for
/// every worktree operation — which is where a clean/smudge filter actually
/// runs — and neutralised nothing.
fn config_paths(git_dir: &Path) -> Vec<PathBuf> {
    let common = match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(raw) => {
            let raw = raw.trim();
            let path = Path::new(raw);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                git_dir.join(path)
            }
        }
        Err(_) => git_dir.to_path_buf(),
    };
    vec![
        common.join("config"),
        // `extensions.worktreeConfig` moves some settings here, per worktree.
        git_dir.join("config.worktree"),
    ]
}

/// Ceiling on what one git invocation may hand back (§7.8). The pipe is still
/// drained to the end — the child must never block on a full pipe — but only
/// this much is kept, so `git status` in a repository with a million untracked
/// files cannot take the node's memory with it.
const MAX_STDOUT_BYTES: usize = 1024 * 1024;
const MAX_STDERR_BYTES: usize = 64 * 1024;

/// The largest object the broker will materialise in one read. Above it the
/// read is REFUSED: half a blob is worse than none, because it would be
/// committed back as the file's new content.
const MAX_BLOB_BYTES: usize = 8 * 1024 * 1024;

/// Reads `reader` to the end, keeping at most `cap` bytes.
fn drain_capped<R: std::io::Read>(mut reader: R, cap: usize) -> std::io::Result<Vec<u8>> {
    let mut kept: Vec<u8> = Vec::new();
    let mut buf = [0u8; 32 * 1024];
    loop {
        let read = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(read) => read,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if kept.len() < cap {
            let take = (cap - kept.len()).min(read);
            kept.extend_from_slice(&buf[..take]);
        }
    }
    Ok(kept)
}

/// Config every invocation carries. Kept as one list so a new call site cannot
/// accidentally run git with a weaker policy than a clone does.
fn hardening_args(hooks_dir: &Path) -> Vec<String> {
    vec![
        // Hooks are code from the repository. They are never executed: the
        // path points at an empty directory we own.
        format!("core.hooksPath={}", hooks_dir.display()),
        // No credential helper may run, and no terminal prompt may block a
        // background operation forever.
        "credential.helper=".to_string(),
        "core.fsmonitor=false".to_string(),
        // A repository must not be able to select an external diff or pager.
        // An EMPTY `diff.external` is fail-closed, not disabled: git tries to
        // run "" and aborts the whole command. Every diff invocation therefore
        // passes `--no-ext-diff`, which refuses external drivers outright and
        // never consults this value.
        "diff.external=".to_string(),
        "core.pager=cat".to_string(),
        // `core.attributesFile` only silences the USER's attributes file; the
        // `.gitattributes` INSIDE a repository is read regardless and can name
        // a diff driver, a textconv or a clean/smudge filter. Those are refused
        // at the invocation instead, because a driver has no generic "off"
        // switch: every diff carries `--no-ext-diff --no-textconv` and every
        // object is hashed with `--no-filters`, and a driver can only be
        // SELECTED here — never DEFINED — because global and system config are
        // /dev/null and the broker writes no driver into the repository's own.
        "core.attributesFile=/dev/null".to_string(),
        // `ext::` executes a command; `file` is limited to user-initiated use.
        "protocol.ext.allow=never".to_string(),
        "protocol.file.allow=user".to_string(),
        // A redirect can move an authenticated fetch to another host.
        "http.followRedirects=false".to_string(),
        // `core.sshCommand` in a repository's config is a command git runs for
        // every fetch and push. Empty is fail-closed; our own ssh invocations
        // pass `GIT_SSH_COMMAND`, which takes precedence over this value.
        "core.sshCommand=".to_string(),
        // A submodule is another remote, fetched implicitly. Never automatic.
        "submodule.recurse=false".to_string(),
        "gc.auto=0".to_string(),
    ]
}

/// A temporary git index, removed when the operation that owns it ends —
/// whatever the outcome, so a failed commit build never leaves a stale index
/// that a later call could pick up.
struct TempIndex {
    path: PathBuf,
}

impl TempIndex {
    fn env(&self) -> Vec<(String, String)> {
        vec![(
            "GIT_INDEX_FILE".to_string(),
            self.path.display().to_string(),
        )]
    }
}

impl Drop for TempIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Whether the string is a git REMOTE NAME rather than a location. Anything
/// carrying a scheme, a path separator, a colon or an `@` is a URL or the
/// scp-like form, and goes to the policy unchanged.
fn is_remote_name(remote: &str) -> bool {
    !remote.is_empty()
        && remote.len() <= 100
        && remote
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && !remote.starts_with('-')
        && !remote.contains("..")
}

/// Directory name of a session's integration worktree.
///
/// The separator is `_`, which `validate_session_id` forbids in a session id.
/// With a `-` suffix the WORKING worktree of a session called `x-int` and the
/// INTEGRATION worktree of session `x` were the same directory — and the same
/// entry under `repo/.git/worktrees`, so one session could take over the
/// other's merge.
fn integration_name(session_id: &str) -> String {
    format!("{session_id}_int")
}

fn integration_ref(op_id: &str) -> String {
    format!("refs/code-studio/integration/{op_id}")
}

fn identity_env(spec: &CommitSpec) -> Vec<(String, String)> {
    vec![
        ("GIT_AUTHOR_NAME".into(), spec.author.name.clone()),
        ("GIT_AUTHOR_EMAIL".into(), spec.author.email.clone()),
        ("GIT_COMMITTER_NAME".into(), spec.committer.name.clone()),
        ("GIT_COMMITTER_EMAIL".into(), spec.committer.email.clone()),
    ]
}

fn merge_identity_env(identity: &CommitIdentity) -> Vec<(String, String)> {
    vec![
        ("GIT_AUTHOR_NAME".into(), identity.name.clone()),
        ("GIT_AUTHOR_EMAIL".into(), identity.email.clone()),
        ("GIT_COMMITTER_NAME".into(), identity.name.clone()),
        ("GIT_COMMITTER_EMAIL".into(), identity.email.clone()),
    ]
}

#[derive(Default)]
struct Invocation {
    envs: Vec<(String, String)>,
    /// Files holding secret material, removed as soon as git exits — whatever
    /// the outcome.
    scratch: Vec<PathBuf>,
}

impl Drop for Invocation {
    fn drop(&mut self) {
        for path in &self.scratch {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Writes credential material to a file that is private FROM THE FIRST BYTE.
///
/// `write` + `chmod` leaves a window in which the file exists with the process
/// umask on it, and on a shared node that window is all another local account
/// needs. The mode is therefore passed to `open(2)` itself, and `create_new`
/// refuses to write through a path that already exists — a symlink an attacker
/// planted included.
#[cfg(unix)]
fn write_private_file(path: &Path, contents: &str, executable: bool) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mode = if executable { 0o700 } else { 0o600 };
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .map_err(|e| anyhow!("create {}: {e}", path.display()))?;
    file.write_all(contents.as_bytes())
        .map_err(|e| anyhow!("write {}: {e}", path.display()))?;
    file.sync_all()
        .map_err(|e| anyhow!("flush {}: {e}", path.display()))
}

/// Windows has no `mode` to pass to `open`, and this build links no ACL API.
/// Confidentiality rests on the broker directory, which lives under the service
/// account's own data root; `create_new` still refuses to write through an
/// existing path, so a planted file cannot be filled in with a credential.
#[cfg(not(unix))]
fn write_private_file(path: &Path, contents: &str, _executable: bool) -> Result<()> {
    use std::io::Write;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| anyhow!("create {}: {e}", path.display()))?;
    file.write_all(contents.as_bytes())
        .map_err(|e| anyhow!("write {}: {e}", path.display()))?;
    file.sync_all()
        .map_err(|e| anyhow!("flush {}: {e}", path.display()))
}

fn shell_single_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

/// Parses `%(upstream:track)`: `[ahead 3]`, `[behind 2]`, `[ahead 3, behind 2]`,
/// `[gone]` or empty. Anything unrecognised counts as no divergence, which is
/// the only safe reading — inventing a number would make the UI offer a push
/// or a pull that has nothing to move.
fn parse_track(track: &str) -> (u32, u32) {
    let mut ahead = 0;
    let mut behind = 0;
    for part in track.trim_matches(|c| c == '[' || c == ']').split(',') {
        let part = part.trim();
        if let Some(value) = part.strip_prefix("ahead ") {
            ahead = value.trim().parse().unwrap_or(0);
        } else if let Some(value) = part.strip_prefix("behind ") {
            behind = value.trim().parse().unwrap_or(0);
        }
    }
    (ahead, behind)
}

fn ok_or_stderr(output: Output, what: &str) -> Result<String> {
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }
    Err(anyhow!("{what} failed: {}", stderr_text(&output.stderr)))
}

/// Git's own diagnostics, made safe to keep.
///
/// An authentication failure quotes the FULL remote URL back at the caller
/// ("fatal: Authentication failed for 'https://<token>@host/repo.git/'"), and
/// this text does not stay in the error object: provisioning writes it into
/// `code_workspaces.status_detail` and `code_workspace_saga_steps.detail`, from
/// where it reaches the registry, the dashboard and every workspace member.
/// Redacting at the one place git's stderr becomes a Rust string covers every
/// caller, including the ones that only log it (§13.4).
fn stderr_text(stderr: &[u8]) -> String {
    super::redact::redact_text(String::from_utf8_lossy(stderr).trim())
}

/// Branch names reach a command line and a ref path. Anything that could be
/// read as an option or escape the refs namespace is refused here rather than
/// relying on `--` placement at every call site.
fn validate_branch(branch: &str) -> Result<()> {
    if branch.is_empty() || branch.len() > 200 {
        return Err(anyhow!("invalid branch name"));
    }
    if branch.starts_with('-')
        || branch.starts_with('/')
        || branch.ends_with('/')
        || branch.contains("..")
        || branch.contains("//")
        || branch.contains('\\')
        || branch.contains('~')
        || branch.contains('^')
        || branch.contains(':')
        || branch.contains('?')
        || branch.contains('*')
        || branch.contains('[')
        || branch.contains('@')
        || branch.ends_with(".lock")
    {
        return Err(anyhow!("invalid branch name"));
    }
    if branch
        .bytes()
        .any(|b| b.is_ascii_control() || b == b' ' || b == 0x7f)
    {
        return Err(anyhow!("invalid branch name"));
    }
    Ok(())
}

/// A start point is a committish, so it is looser than a branch name — but it
/// still must not look like an option.
fn validate_start_point(start_point: &str) -> Result<()> {
    if start_point.is_empty() || start_point.len() > 200 || start_point.starts_with('-') {
        return Err(anyhow!("invalid start point"));
    }
    if start_point
        .bytes()
        .any(|b| b.is_ascii_control() || b == b' ')
    {
        return Err(anyhow!("invalid start point"));
    }
    Ok(())
}

/// Object ids reach command lines and `--cacheinfo` triples. Only a bare hex
/// digest is ever legitimate, so anything else is refused before it can be read
/// as a revision expression or an option.
pub(super) fn validate_oid(oid: &str) -> Result<()> {
    if (oid.len() != 40 && oid.len() != 64) || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(anyhow!("invalid object id"));
    }
    Ok(())
}

/// Repository-relative paths reach `--cacheinfo`, pathspecs and patch headers.
/// The guard is a whitelist of shape rather than a blacklist of tricks:
/// relative, no traversal, nothing that could be read as an option, and never
/// inside `.git`.
///
/// What a COMPONENT may be called is not decided here: `fs::validate_component`
/// already owns that rule for the session filesystem, and a second list would
/// mean the broker committing names the session layer refuses to write (or the
/// other way round). The broker adds only what is specific to git's argv — the
/// leading `-`, the absolute forms and the traversal segments.
pub(super) fn validate_repo_path(path: &str) -> Result<()> {
    if path.is_empty() || path.len() > 4096 {
        return Err(anyhow!("invalid repository path"));
    }
    if path.starts_with('-') || path.starts_with('/') || path.starts_with('\\') {
        return Err(anyhow!("invalid repository path"));
    }
    if path.ends_with('/') || path.contains('\\') {
        return Err(anyhow!("invalid repository path"));
    }
    for segment in path.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(anyhow!("invalid repository path"));
        }
        if super::fs::is_git_metadata(segment) {
            return Err(anyhow!("a path inside .git is never repository content"));
        }
        super::fs::validate_component(segment)
            .map_err(|reason| anyhow!("invalid repository path: {reason}"))?;
    }
    Ok(())
}

/// Only regular files reach a Code Studio commit. A symlink entry (`120000`)
/// or a gitlink (`160000`) is content whose meaning is resolved OUTSIDE the
/// reviewed bytes, so it is refused rather than reviewed as text.
fn validate_file_mode(mode: &str) -> Result<()> {
    match mode {
        "100644" | "100755" => Ok(()),
        _ => Err(anyhow!(
            "mode {mode} is not a regular file; Code Studio commits regular files only"
        )),
    }
}

/// Full reference names, including the private integration namespace.
fn validate_ref_name(ref_name: &str) -> Result<()> {
    if !ref_name.starts_with("refs/") {
        return Err(anyhow!("a reference must be fully qualified"));
    }
    let rest = &ref_name["refs/".len()..];
    if rest.is_empty() || ref_name.len() > 255 {
        return Err(anyhow!("invalid reference name"));
    }
    validate_branch(rest)
}

/// Operation ids name a private reference, so they share the alphabet the
/// filesystem guard uses for session ids.
fn validate_op_id(op_id: &str) -> Result<()> {
    paths::validate_session_id(op_id).map_err(|_| anyhow!("invalid operation id"))
}

/// A commit identity ends up inside the object header, where `<`, `>` and a
/// newline are structural.
fn validate_identity(identity: &CommitIdentity) -> Result<()> {
    for field in [&identity.name, &identity.email] {
        if field.trim().is_empty() || field.len() > 200 {
            return Err(anyhow!("invalid commit identity"));
        }
        if field
            .bytes()
            .any(|b| b.is_ascii_control() || b == b'<' || b == b'>')
        {
            return Err(anyhow!("invalid commit identity"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// For the tests that assert a SECURITY property of a real git process.
    /// Skipping those on a machine without git reports "ok" for a guarantee
    /// nobody checked; the broker cannot work without git either way, so its
    /// absence is a broken test environment, not a reason to pass.
    fn require_git() {
        assert!(
            git_available(),
            "git is required: this test asserts what a real git process does"
        );
    }

    #[test]
    fn branch_names_that_could_become_options_or_escape_refs_are_refused() {
        for bad in [
            "",
            "--upload-pack=sh",
            "-x",
            "/leading",
            "trailing/",
            "a..b",
            "a//b",
            "a\\b",
            "a~1",
            "a^",
            "a:b",
            "a?b",
            "a*b",
            "a[b",
            "a@{b",
            "with space",
            "ctrl\x01char",
            "thing.lock",
            &"x".repeat(201),
        ] {
            assert!(validate_branch(bad).is_err(), "accepted branch {bad:?}");
        }
        for good in ["main", "cs/piotr/9f2a1c4b", "release-1.2"] {
            assert!(validate_branch(good).is_ok(), "refused branch {good:?}");
        }
    }

    #[test]
    fn a_start_point_cannot_look_like_an_option() {
        assert!(validate_start_point("--upload-pack=sh").is_err());
        assert!(validate_start_point("").is_err());
        assert!(validate_start_point("HEAD").is_ok());
        assert!(validate_start_point("main").is_ok());
    }

    #[test]
    fn the_hardening_list_reaches_the_git_process_and_not_just_the_list() {
        // Asserting the function against its own literals stayed green with the
        // `-c` loop deleted from `exec`. This asks GIT what configuration it is
        // running under, which is the property that matters.
        require_git();
        let dir = tempfile::tempdir().unwrap();
        let broker = Broker::at(dir.path());
        broker.init_repository("main").unwrap();

        let out = broker
            .run(Some(&broker.reference()), &["config", "--list"], &GitAuth::None)
            .unwrap();
        let effective = String::from_utf8_lossy(&out.stdout).to_ascii_lowercase();
        for expected in [
            "credential.helper=",
            "core.fsmonitor=false",
            "diff.external=",
            "core.pager=cat",
            "core.attributesfile=/dev/null",
            "protocol.ext.allow=never",
            "protocol.file.allow=user",
            "http.followredirects=false",
            "submodule.recurse=false",
            "gc.auto=0",
        ] {
            assert!(
                effective.contains(expected),
                "git ran without {expected}; effective config = {effective}"
            );
        }
        let hooks = broker.empty_hooks_dir().unwrap();
        assert!(
            effective.contains(&format!("core.hookspath={}", hooks.display()).to_ascii_lowercase()),
            "hooks were not redirected to the empty directory: {effective}"
        );
    }

    #[test]
    fn a_session_handle_points_inside_the_reference_repository() {
        let broker = Broker::at("/data/code-studio/ws-1");
        let reference = broker.reference();
        let session = broker.session("s-1").unwrap();
        assert!(session.git_dir.starts_with(&reference.git_dir));
        assert_eq!(session.work_tree, broker.session_worktree("s-1").unwrap());
        // The pointer file inside the worktree is never what we use.
        assert_ne!(session.git_dir, session.work_tree.join(".git"));
    }

    #[test]
    fn a_session_id_that_could_escape_the_workspace_is_refused() {
        let broker = Broker::at("/data/code-studio/ws-1");
        assert!(broker.session("../../etc").is_err());
        assert!(broker.session_worktree("..").is_err());
    }

    #[test]
    fn shell_quoting_survives_a_path_with_a_quote() {
        assert_eq!(
            shell_single_quote(Path::new("/tmp/it's here")),
            "'/tmp/it'\\''s here'"
        );
    }

    #[test]
    fn credential_kind_must_match_the_remote_scheme() {
        let dir = tempfile::tempdir().unwrap();
        let broker = Broker::at(dir.path());
        let err = broker
            .clone_repository(
                "ssh://git@github.com/org/repo.git",
                &GitAuth::Token("t".into()),
            )
            .unwrap_err();
        assert!(err.to_string().contains("cannot authenticate with a token"));
    }

    /// Files in the broker directory, so a test can assert on what a credential
    /// left behind without knowing the per-invocation name.
    fn broker_files(broker: &Broker) -> Vec<PathBuf> {
        let dir = broker.broker_dir().unwrap();
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .collect()
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn a_token_is_readable_only_by_us_and_dies_with_the_invocation() {
        let dir = tempfile::tempdir().unwrap();
        let broker = Broker::at(dir.path());
        {
            let mut inv = Invocation::default();
            broker
                .apply_auth(&GitAuth::Token("super-secret".into()), &mut inv)
                .unwrap();
            let secret = inv
                .scratch
                .iter()
                .find(|path| path.extension().is_some_and(|e| e == "secret"))
                .expect("the helper needs the secret while git runs")
                .clone();
            assert_eq!(
                std::fs::read_to_string(&secret).unwrap(),
                "super-secret",
                "the token must reach the helper unchanged"
            );
            #[cfg(unix)]
            {
                assert_eq!(mode_of(&secret), 0o600, "the token file is world readable");
                let helper = inv
                    .scratch
                    .iter()
                    .find(|path| path.extension().is_some_and(|e| e == "sh"))
                    .expect("no askpass helper");
                assert_eq!(mode_of(helper), 0o700, "the helper is writable by others");
            }
            // It is not on the command line and not in the process environment
            // either — only GIT_ASKPASS, which is a path.
            assert!(inv.envs.iter().all(|(_, v)| !v.contains("super-secret")));
        }
        assert!(
            broker_files(&broker).is_empty(),
            "the token outlived the git invocation"
        );
    }

    #[test]
    fn an_ssh_key_is_private_and_leaves_nothing_behind_either() {
        let dir = tempfile::tempdir().unwrap();
        let broker = Broker::at(dir.path());
        {
            let mut inv = Invocation::default();
            broker
                .apply_auth(
                    &GitAuth::SshKey {
                        private_key: "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n".into(),
                        known_host: Some("github.com ssh-ed25519 AAAAC3Nz".into()),
                    },
                    &mut inv,
                )
                .unwrap();
            let key = inv
                .scratch
                .first()
                .expect("no key was written for an ssh clone")
                .clone();
            assert!(std::fs::read_to_string(&key).unwrap().contains("PRIVATE KEY"));
            #[cfg(unix)]
            assert_eq!(mode_of(&key), 0o600, "the private key is world readable");
            assert!(
                inv.envs
                    .iter()
                    .all(|(_, value)| !value.contains("BEGIN OPENSSH")),
                "the key body reached the environment"
            );
            assert!(
                inv.envs
                    .iter()
                    .any(|(name, value)| name == "GIT_SSH_COMMAND"
                        && value.contains("StrictHostKeyChecking=yes")),
                "host-key checking was not enforced"
            );
        }
        assert!(
            broker_files(&broker).is_empty(),
            "key material outlived the git invocation"
        );
    }

    #[test]
    fn a_git_process_runs_with_the_credential_and_cleans_up_after_itself() {
        require_git();
        let dir = tempfile::tempdir().unwrap();
        let broker = Broker::at(dir.path());
        broker.init_repository("main").unwrap();

        // A real invocation, through `exec`, with a credential attached: the
        // scratch files must be gone by the time it returns whatever git said.
        let out = broker
            .run(
                Some(&broker.reference()),
                &["rev-parse", "HEAD"],
                &GitAuth::Token("super-secret".into()),
            )
            .unwrap();
        let printed = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!printed.contains("super-secret"), "{printed}");
        assert!(
            broker_files(&broker).is_empty(),
            "an authenticated invocation left credential material behind: {:?}",
            broker_files(&broker)
        );
    }

    #[test]
    fn two_authenticated_invocations_do_not_share_one_secret_file() {
        // A push while a fetch runs, two sessions, a retry: with fixed file
        // names the second call overwrote the first call's token and the first
        // `Invocation` to drop deleted the file the other still needed.
        let dir = tempfile::tempdir().unwrap();
        let broker = Broker::at(dir.path());

        let mut first = Invocation::default();
        broker
            .apply_auth(&GitAuth::Token("token-of-session-one".into()), &mut first)
            .unwrap();
        let first_secret = first
            .scratch
            .iter()
            .find(|path| path.extension().is_some_and(|e| e == "secret"))
            .expect("no secret")
            .clone();

        let mut second = Invocation::default();
        broker
            .apply_auth(&GitAuth::Token("token-of-session-two".into()), &mut second)
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(&first_secret).unwrap(),
            "token-of-session-one",
            "the second invocation overwrote the first invocation's credential"
        );
        drop(second);
        assert!(
            first_secret.exists(),
            "dropping one invocation deleted the credential another one is still using"
        );
        assert_eq!(
            std::fs::read_to_string(&first_secret).unwrap(),
            "token-of-session-one"
        );
        drop(first);
        assert!(broker_files(&broker).is_empty());
    }

    #[test]
    fn an_ssh_clone_without_a_pinned_host_key_is_refused_before_the_key_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let broker = Broker::at(dir.path());
        let mut inv = Invocation::default();
        let err = broker
            .apply_auth(
                &GitAuth::SshKey {
                    private_key: "KEY".into(),
                    known_host: None,
                },
                &mut inv,
            )
            .unwrap_err();
        assert!(err.to_string().contains("no pinned host key"));
        // The refusal has to come first: writing the key and relying on `Drop`
        // to remove it puts private material on disk for an unpinned host.
        assert!(
            inv.scratch.is_empty(),
            "a key was written for a remote we refuse to talk to"
        );
        assert!(
            broker_files(&broker).is_empty(),
            "the refused clone left key material on disk: {:?}",
            broker_files(&broker)
        );
    }

    #[test]
    fn init_then_worktree_then_status_works_against_real_git() {
        require_git();
        let dir = tempfile::tempdir().unwrap();
        let broker = Broker::at(dir.path());

        let created = broker.init_repository("main").unwrap();
        assert_eq!(created.default_branch, "main");
        assert_eq!(created.head_commit.len(), 40, "expected a full sha");

        let worktree = broker
            .add_session_worktree("s-1", "cs/piotr/s-1", "main")
            .unwrap();
        assert!(worktree.join(".git").exists(), "worktree was not created");

        // A clean worktree reports nothing; a new file shows up as untracked.
        assert!(broker.status("s-1").unwrap().is_empty());
        std::fs::write(worktree.join("hello.txt"), "hi").unwrap();
        let status = broker.status("s-1").unwrap();
        assert_eq!(status.len(), 1);
        assert!(status[0].ends_with("hello.txt"), "got {:?}", status);

        broker.remove_session_worktree("s-1").unwrap();
        assert!(!worktree.exists(), "worktree survived removal");
    }

    #[test]
    fn hooks_from_the_repository_are_never_executed() {
        require_git();
        let dir = tempfile::tempdir().unwrap();
        let broker = Broker::at(dir.path());
        broker.init_repository("main").unwrap();

        // A pre-commit hook that would fail the commit if it ran at all.
        let hooks = broker.reference().git_dir.join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("pre-commit");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(
                &hook,
                "#!/bin/sh\ntouch \"$(dirname \"$0\")/../../HOOK_RAN\"\nexit 1\n",
            )
            .unwrap();
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let handle = broker.reference();
        let out = broker
            .run(
                Some(&handle),
                &[
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@example.invalid",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "second",
                ],
                &GitAuth::None,
            )
            .unwrap();
        assert!(
            out.status.success(),
            "the commit was blocked, so the hook ran: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        // The hook writes next to the repository (`$0/../../HOOK_RAN`), which is
        // `repo/`, not the workspace root — the old assertion looked at a path
        // the hook could never have created and would have passed even if the
        // hook had run.
        assert!(
            !broker.reference().work_tree.join("HOOK_RAN").exists(),
            "the hook executed"
        );
    }

    /// A repository is DATA. `.gitattributes` and `.git/config` travel with it,
    /// and both can name a command for git to run — a textconv, a clean/smudge
    /// filter, an external diff driver, a pager. §11.2 requires them off, and
    /// this asserts it against a real git process rather than against the text
    /// of `hardening_args`: with the `-c` loop deleted from `exec`, every other
    /// broker test but the hook one stayed green.
    #[test]
    #[cfg(unix)]
    fn a_hostile_repository_cannot_make_git_run_a_command() {
        use std::os::unix::fs::PermissionsExt;
        require_git();
        let dir = tempfile::tempdir().unwrap();
        let broker = Broker::at(dir.path());
        let root = broker.init_repository("main").unwrap();
        let handle = broker.reference();
        let marker = dir.path().join("EXECUTED");

        // The payload every driver below points at.
        let script = dir.path().join("payload.sh");
        std::fs::write(
            &script,
            format!("#!/bin/sh\ntouch {}\ncat \"$1\" 2>/dev/null\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        // Config that travelled with the repository, naming the drivers.
        let config = handle.git_dir.join("config");
        let mut existing = std::fs::read_to_string(&config).unwrap_or_default();
        existing.push_str(&format!(
            "[diff \"evil\"]\n\ttextconv = {script}\n\tcommand = {script}\n\
             [filter \"evil\"]\n\tsmudge = {script}\n\tclean = {script}\n\
             [core]\n\tpager = {script}\n",
            script = script.display()
        ));
        std::fs::write(&config, existing).unwrap();

        // Attributes that select them, committed INSIDE the repository so no
        // user-level attributes file is involved.
        let base = commit_file(
            &broker,
            "main",
            &root.head_commit,
            Some(&root.head_commit),
            ".gitattributes",
            "* diff=evil filter=evil\n",
        )
        .commit_oid;
        let with_file = commit_file(
            &broker,
            "main",
            &base,
            Some(&base),
            "secret.bin",
            "content\n",
        )
        .commit_oid;

        // Everything the broker does with a repository's content.
        let _ = broker.diff_patch(&handle, &base, &with_file, "secret.bin");
        let _ = broker.diff_name_status(&handle, &base, &with_file);
        let _ = broker.list_tree(&handle, &with_file);
        let _ = broker.log(&handle, ".", 5);
        let _ = broker.add_session_worktree("s-1", "cs/u/s1", "main");
        let _ = broker.status("s-1");
        let _ = broker.remove_session_worktree("s-1");

        assert!(
            !marker.exists(),
            "a driver named by the repository was executed by git"
        );
    }

    fn identity() -> CommitIdentity {
        CommitIdentity {
            name: "TentaFlow Code Studio".into(),
            email: "code-studio@tentaflow.local".into(),
        }
    }

    /// Publishes one file on a branch through the same path production uses.
    fn commit_file(
        broker: &Broker,
        branch: &str,
        base: &str,
        expected_old: Option<&str>,
        path: &str,
        content: &str,
    ) -> CommitOutcome {
        broker
            .build_commit(
                &broker.reference(),
                &CommitSpec {
                    base_commit: base.to_string(),
                    extra_parent: None,
                    branch: branch.to_string(),
                    expected_old: expected_old.map(str::to_string),
                    message: format!("write {path}"),
                    author: identity(),
                    committer: identity(),
                    files: vec![CommitFile {
                        path: path.to_string(),
                        old_path: None,
                        mode: "100644".into(),
                        change: CommitChange::Write {
                            content: content.as_bytes().to_vec(),
                        },
                    }],
                },
            )
            .expect("build commit")
    }

    /// main with one file, plus a session branch that changed it.
    fn repo_with_a_session_branch(dir: &Path) -> (Broker, String, String) {
        let broker = Broker::at(dir);
        let root = broker.init_repository("main").unwrap();
        let first = commit_file(
            &broker,
            "main",
            &root.head_commit,
            Some(&root.head_commit),
            "f.txt",
            "a\nb\nc\n",
        );
        broker
            .add_session_worktree("s-1", "cs/u/s1", "main")
            .unwrap();
        let session = commit_file(
            &broker,
            "cs/u/s1",
            &first.commit_oid,
            Some(&first.commit_oid),
            "f.txt",
            "a\nSESSION\nc\n",
        );
        (broker, first.commit_oid, session.commit_oid)
    }

    #[test]
    fn object_content_survives_a_round_trip_through_the_broker() {
        require_git();
        let dir = tempfile::tempdir().unwrap();
        let broker = Broker::at(dir.path());
        broker.init_repository("main").unwrap();
        let handle = broker.reference();

        // Bytes, not text: repository content is not required to be UTF-8.
        let content: Vec<u8> = vec![0x00, 0xff, b'\n', 0x80, b'x'];
        let oid = broker.hash_object(&handle, &content).unwrap();
        assert_eq!(broker.cat_file(&handle, &oid).unwrap(), content);
        // Content addressing: the same bytes hash to the same object.
        assert_eq!(broker.hash_object(&handle, &content).unwrap(), oid);
    }

    #[test]
    fn a_rename_leaves_no_trace_of_the_old_path_in_the_tree() {
        require_git();
        let dir = tempfile::tempdir().unwrap();
        let broker = Broker::at(dir.path());
        let root = broker.init_repository("main").unwrap();
        let first = commit_file(
            &broker,
            "main",
            &root.head_commit,
            Some(&root.head_commit),
            "old/name.txt",
            "content\n",
        );

        let renamed = broker
            .build_commit(
                &broker.reference(),
                &CommitSpec {
                    base_commit: first.commit_oid.clone(),
                    extra_parent: None,
                    branch: "main".into(),
                    expected_old: Some(first.commit_oid.clone()),
                    message: "rename".into(),
                    author: identity(),
                    committer: identity(),
                    files: vec![CommitFile {
                        path: "new/name.txt".into(),
                        old_path: Some("old/name.txt".into()),
                        mode: "100644".into(),
                        change: CommitChange::Write {
                            content: b"content\n".to_vec(),
                        },
                    }],
                },
            )
            .unwrap();

        let tree = broker
            .list_tree(&broker.reference(), &renamed.tree_oid)
            .unwrap();
        let paths: Vec<&str> = tree.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["new/name.txt"],
            "the rename left the file under both names"
        );
    }

    #[test]
    fn a_commit_refuses_to_overwrite_a_branch_that_moved() {
        require_git();
        let dir = tempfile::tempdir().unwrap();
        let broker = Broker::at(dir.path());
        let root = broker.init_repository("main").unwrap();
        let first = commit_file(
            &broker,
            "main",
            &root.head_commit,
            Some(&root.head_commit),
            "f.txt",
            "one\n",
        );

        // A stale expected_old is an error, never an overwrite.
        let err = broker
            .build_commit(
                &broker.reference(),
                &CommitSpec {
                    base_commit: first.commit_oid.clone(),
                    extra_parent: None,
                    branch: "main".into(),
                    expected_old: Some(root.head_commit.clone()),
                    message: "stale".into(),
                    author: identity(),
                    committer: identity(),
                    files: vec![CommitFile {
                        path: "f.txt".into(),
                        old_path: None,
                        mode: "100644".into(),
                        change: CommitChange::Write {
                            content: b"two\n".to_vec(),
                        },
                    }],
                },
            )
            .unwrap_err();
        assert!(err.to_string().contains("update-ref"), "got {err}");
        assert_eq!(
            broker
                .read_ref(&broker.reference(), "refs/heads/main")
                .unwrap(),
            Some(first.commit_oid),
            "a stale compare-and-swap moved the branch anyway"
        );
    }

    #[test]
    fn a_snapshot_of_a_worktree_leaves_the_agents_own_index_alone() {
        require_git();
        let dir = tempfile::tempdir().unwrap();
        let (broker, base, _session) = repo_with_a_session_branch(dir.path());
        let handle = broker.session("s-1").unwrap();
        std::fs::write(handle.work_tree.join("added.txt"), "fresh\n").unwrap();
        let before = broker.status("s-1").unwrap();
        assert!(
            before.iter().any(|entry| entry.starts_with("??")),
            "the new file is missing from status: {before:?}"
        );

        let tree = broker.snapshot_worktree(&handle, &base).unwrap();
        let paths: Vec<String> = broker
            .list_tree(&handle, &tree)
            .unwrap()
            .into_iter()
            .map(|e| e.path)
            .collect();
        assert!(paths.contains(&"added.txt".to_string()));

        // The agent's index is byte-for-byte where it was: the snapshot staged
        // into a temporary index in the broker directory, not into this one.
        assert_eq!(
            broker.status("s-1").unwrap(),
            before,
            "the snapshot changed what the agent's own index reports"
        );
    }

    #[test]
    fn the_integration_worktree_is_detached_so_a_merge_cannot_move_the_target() {
        require_git();
        let dir = tempfile::tempdir().unwrap();
        let (broker, base, session_head) = repo_with_a_session_branch(dir.path());
        // The target moves on independently, so the merge is a real merge.
        let target = commit_file(
            &broker,
            "main",
            &base,
            Some(&base),
            "other.txt",
            "target side\n",
        );

        let before = broker
            .read_ref(&broker.reference(), "refs/heads/main")
            .unwrap()
            .unwrap();
        broker
            .add_integration_worktree("s-1", "op-1", &before)
            .unwrap();
        let outcome = broker.merge_into_integration("s-1", "cs/u/s1").unwrap();
        let MergeOutcome::Clean {
            merge_head,
            fast_forward,
        } = outcome
        else {
            panic!("expected a clean merge");
        };
        assert!(!fast_forward, "the branches diverged, this is not a ff");
        assert_ne!(merge_head, before);
        assert_ne!(merge_head, session_head);

        let after = broker
            .read_ref(&broker.reference(), "refs/heads/main")
            .unwrap()
            .unwrap();
        assert_eq!(
            after, target.commit_oid,
            "merging into the integration worktree moved the target branch"
        );
        assert_eq!(before, after);

        // The case that carries the guarantee: `git worktree add <path> <sha>`
        // detaches whether or not `--detach` is passed, so removing the flag
        // changes nothing above. What WOULD move the target is being handed the
        // branch NAME — the worktree would check the branch out and the merge
        // would move it. A name is refused before git is reached.
        broker.remove_integration_worktree("s-1", "op-1").unwrap();
        let by_name = broker.add_integration_worktree("s-1", "op-2", "main");
        assert!(
            by_name.is_err(),
            "the merge base was accepted as a branch name, which is a moving target"
        );
        assert!(!broker.integration_worktree("s-1").unwrap().exists());
    }

    #[test]
    fn a_fast_forward_merge_is_reported_as_one() {
        require_git();
        let dir = tempfile::tempdir().unwrap();
        let (broker, _base, session_head) = repo_with_a_session_branch(dir.path());
        let before = broker
            .read_ref(&broker.reference(), "refs/heads/main")
            .unwrap()
            .unwrap();
        broker
            .add_integration_worktree("s-1", "op-1", &before)
            .unwrap();

        let outcome = broker.merge_into_integration("s-1", "cs/u/s1").unwrap();
        let MergeOutcome::Clean {
            merge_head,
            fast_forward,
        } = outcome
        else {
            panic!("expected a clean fast-forward");
        };
        assert!(fast_forward);
        assert_eq!(merge_head, session_head);
        assert_eq!(
            broker
                .read_ref(&broker.reference(), "refs/heads/main")
                .unwrap(),
            Some(before),
            "even a fast-forward must not move the target on its own"
        );
    }

    #[test]
    fn a_conflict_is_a_result_and_the_worktree_stays_for_the_next_run() {
        require_git();
        let dir = tempfile::tempdir().unwrap();
        let (broker, base, _session) = repo_with_a_session_branch(dir.path());
        // Both sides touch the same line.
        commit_file(
            &broker,
            "main",
            &base,
            Some(&base),
            "f.txt",
            "a\nTARGET\nc\n",
        );

        let before = broker
            .read_ref(&broker.reference(), "refs/heads/main")
            .unwrap()
            .unwrap();
        let worktree = broker
            .add_integration_worktree("s-1", "op-1", &before)
            .unwrap();
        let outcome = broker.merge_into_integration("s-1", "cs/u/s1").unwrap();
        let MergeOutcome::Conflict { paths } = outcome else {
            panic!("two edits of the same line must conflict");
        };
        assert_eq!(paths, vec!["f.txt".to_string()]);

        // Everything the revision run needs is still there: the directory with
        // the half-merged tree, and the private ref naming the base it started
        // from. Neither is cleaned up by a conflict.
        assert!(worktree.exists(), "the conflict removed the worktree");
        assert_eq!(
            broker
                .read_ref(&broker.reference(), "refs/code-studio/integration/op-1")
                .unwrap(),
            Some(before.clone())
        );
        assert!(
            broker.integration("s-1").unwrap().git_dir.exists(),
            "the integration worktree is no longer registered"
        );
        assert_eq!(
            broker
                .read_ref(&broker.reference(), "refs/heads/main")
                .unwrap(),
            Some(before),
            "a conflicting merge moved the target branch"
        );
    }

    #[test]
    fn a_verified_result_that_is_not_finalised_never_reaches_the_target() {
        require_git();
        let dir = tempfile::tempdir().unwrap();
        let (broker, base, _session) = repo_with_a_session_branch(dir.path());
        let before = broker
            .read_ref(&broker.reference(), "refs/heads/main")
            .unwrap()
            .unwrap();
        broker
            .add_integration_worktree("s-1", "op-1", &before)
            .unwrap();
        let MergeOutcome::Clean { merge_head, .. } =
            broker.merge_into_integration("s-1", "cs/u/s1").unwrap()
        else {
            panic!("expected a clean merge");
        };
        broker.write_private_ref("op-1", &merge_head).unwrap();

        // The tests came back red: the merge result exists, is anchored, and
        // the target branch has not moved a single commit.
        assert_eq!(
            broker
                .read_ref(&broker.reference(), "refs/heads/main")
                .unwrap(),
            Some(before.clone())
        );
        assert_eq!(before, base, "the target branch is still at its base");
        assert_eq!(
            broker
                .read_ref(&broker.reference(), "refs/code-studio/integration/op-1")
                .unwrap(),
            Some(merge_head)
        );

        // Abandoning the attempt takes the worktree AND the private ref.
        broker.remove_integration_worktree("s-1", "op-1").unwrap();
        assert!(!broker.integration_worktree("s-1").unwrap().exists());
        assert_eq!(
            broker
                .read_ref(&broker.reference(), "refs/code-studio/integration/op-1")
                .unwrap(),
            None
        );
    }

    #[test]
    fn a_target_branch_that_moved_aborts_the_finalisation_instead_of_overwriting() {
        require_git();
        let dir = tempfile::tempdir().unwrap();
        let (broker, base, session_head) = repo_with_a_session_branch(dir.path());
        let expected_old = broker
            .read_ref(&broker.reference(), "refs/heads/main")
            .unwrap()
            .unwrap();
        broker
            .add_integration_worktree("s-1", "op-1", &expected_old)
            .unwrap();
        broker.merge_into_integration("s-1", "cs/u/s1").unwrap();

        // Somebody else pushes to the target while the merge is being reviewed.
        let intruder = commit_file(
            &broker,
            "main",
            &base,
            Some(&base),
            "intruder.txt",
            "not mine\n",
        );

        let err = broker
            .finalize_merge(&CommitSpec {
                base_commit: expected_old.clone(),
                extra_parent: Some(session_head),
                branch: "main".into(),
                expected_old: Some(expected_old),
                message: "merge".into(),
                author: identity(),
                committer: identity(),
                files: vec![CommitFile {
                    path: "f.txt".into(),
                    old_path: None,
                    mode: "100644".into(),
                    change: CommitChange::Write {
                        content: b"a\nSESSION\nc\n".to_vec(),
                    },
                }],
            })
            .unwrap_err();
        assert!(err.to_string().contains("update-ref"), "got {err}");
        assert_eq!(
            broker
                .read_ref(&broker.reference(), "refs/heads/main")
                .unwrap(),
            Some(intruder.commit_oid),
            "the finalisation overwrote a target branch that had moved"
        );
    }

    #[test]
    fn a_merge_commit_without_two_parents_or_an_expected_old_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let broker = Broker::at(dir.path());
        let base = "a".repeat(40);
        let mut spec = CommitSpec {
            base_commit: base.clone(),
            extra_parent: None,
            branch: "main".into(),
            expected_old: Some(base.clone()),
            message: "merge".into(),
            author: identity(),
            committer: identity(),
            files: Vec::new(),
        };
        assert!(broker
            .finalize_merge(&spec)
            .unwrap_err()
            .to_string()
            .contains("second parent"));
        spec.extra_parent = Some(base);
        spec.expected_old = None;
        assert!(broker
            .finalize_merge(&spec)
            .unwrap_err()
            .to_string()
            .contains("target tip"));
    }

    #[test]
    fn paths_modes_and_ids_that_could_escape_a_commit_are_refused() {
        for bad in [
            "",
            "-x",
            "/etc/passwd",
            "a/../b",
            "a/./b",
            "a//b",
            "a\\b",
            ".git/config",
            ".GIT/hooks/pre-commit",
            "trailing/",
            "ctrl\x01char",
        ] {
            assert!(validate_repo_path(bad).is_err(), "accepted path {bad:?}");
        }
        for good in ["f.txt", "src/main.rs", "a b/c d.txt", "ünïcode.txt"] {
            assert!(validate_repo_path(good).is_ok(), "refused path {good:?}");
        }

        // One rule, two doors: a name the session filesystem refuses to write
        // must not be a name the broker agrees to commit. Both guards read the
        // same `fs::validate_component`, so this asserts they still do.
        for name in [
            "COM1", "com1.log", "NUL", "aux.txt", "CONIN$", "x.", "x ", ".GIT", ".Git", "a:b",
            "star*name", "pipe|name",
        ] {
            let by_fs = crate::code_studio::fs::RelPath::parse(&format!("src/{name}")).is_err();
            let by_broker = validate_repo_path(&format!("src/{name}")).is_err();
            assert!(
                by_fs && by_broker,
                "{name:?}: fs refused {by_fs}, broker refused {by_broker}"
            );
        }

        assert!(validate_file_mode("100644").is_ok());
        assert!(validate_file_mode("100755").is_ok());
        for bad in ["120000", "160000", "040000", "", "100644 "] {
            assert!(validate_file_mode(bad).is_err(), "accepted mode {bad:?}");
        }

        assert!(validate_oid(&"a".repeat(40)).is_ok());
        assert!(validate_oid(&"a".repeat(64)).is_ok());
        for bad in ["", "zz", &"a".repeat(39), &"g".repeat(40), "--upload-pack"] {
            assert!(validate_oid(bad).is_err(), "accepted oid {bad:?}");
        }

        assert!(validate_ref_name("refs/heads/main").is_ok());
        assert!(validate_ref_name("refs/code-studio/integration/op-1").is_ok());
        for bad in ["heads/main", "refs/", "refs/heads/../../x", "-refs/heads/x"] {
            assert!(validate_ref_name(bad).is_err(), "accepted ref {bad:?}");
        }

        assert!(validate_identity(&identity()).is_ok());
        assert!(validate_identity(&CommitIdentity {
            name: "a\nb".into(),
            email: "x@y".into()
        })
        .is_err());
        assert!(validate_identity(&CommitIdentity {
            name: "n".into(),
            email: "<x@y>".into()
        })
        .is_err());
    }

    #[test]
    fn an_inherited_git_dir_cannot_redirect_the_broker() {
        require_git();
        // `set_var` is unsound in a multi-threaded program (edition 2024 makes
        // that explicit) and `Command` walks the environment while spawning, so
        // an unserialised mutation here made OTHER modules' tests fail at
        // random. The same guard every test that touches process-global state
        // takes is taken here.
        let _guard = paths::test_data_dir_guard();
        let dir = tempfile::tempdir().unwrap();
        let broker = Broker::at(dir.path());
        broker.init_repository("main").unwrap();

        let ours = broker.head_commit(&broker.reference()).unwrap();

        // A real OTHER repository, so the test can tell "the variable was
        // ignored" from "the variable pointed at nothing". Aiming git at a
        // non-existent path only proves the command did not fail.
        let elsewhere = tempfile::tempdir().unwrap();
        let foreign = Broker::at(elsewhere.path());
        let foreign_root = foreign.init_repository("main").unwrap().head_commit;
        // Content of its own, or the two empty initial commits hash identically
        // and the assertion below could not tell the repositories apart.
        let theirs = commit_file(
            &foreign,
            "main",
            &foreign_root,
            Some(&foreign_root),
            "foreign.txt",
            "not ours\n",
        )
        .commit_oid;
        assert_ne!(ours, theirs);

        std::env::set_var("GIT_DIR", foreign.reference().git_dir.display().to_string());
        std::env::set_var(
            "GIT_WORK_TREE",
            foreign.reference().work_tree.display().to_string(),
        );
        let head = broker.head_commit(&broker.reference());
        std::env::remove_var("GIT_DIR");
        std::env::remove_var("GIT_WORK_TREE");
        assert_eq!(
            head.unwrap(),
            ours,
            "the broker answered from the repository an inherited GIT_DIR pointed at"
        );

        // The other half of the environment hardening: an inherited identity
        // would sign a commit with whoever started the service, and an
        // inherited index would build it from state the broker never wrote.
        std::env::set_var("GIT_AUTHOR_NAME", "Inherited Author");
        std::env::set_var("GIT_AUTHOR_EMAIL", "inherited@example.invalid");
        std::env::set_var("GIT_COMMITTER_NAME", "Inherited Committer");
        std::env::set_var("GIT_COMMITTER_EMAIL", "inherited@example.invalid");
        std::env::set_var("GIT_INDEX_FILE", "/nonexistent/inherited.idx");
        let committed = commit_file(&broker, "main", &ours, Some(&ours), "f.txt", "content\n");
        std::env::remove_var("GIT_AUTHOR_NAME");
        std::env::remove_var("GIT_AUTHOR_EMAIL");
        std::env::remove_var("GIT_COMMITTER_NAME");
        std::env::remove_var("GIT_COMMITTER_EMAIL");
        std::env::remove_var("GIT_INDEX_FILE");

        let shown = broker
            .run(
                Some(&broker.reference()),
                &["show", "-s", "--format=%ae|%ce", &committed.commit_oid],
                &GitAuth::None,
            )
            .unwrap();
        let attribution = String::from_utf8_lossy(&shown.stdout).trim().to_string();
        assert_eq!(
            attribution,
            format!("{email}|{email}", email = identity().email),
            "the commit was signed with an inherited identity"
        );
    }
}

// ===== Regressions — one test per defect found in review =====
//
// Each of these failed against the code as first written. They stay because the
// property each one pins is easy to lose again: the shape of a rename, what a
// fast-forward means, where a worktree lives and who decides whether it can be
// removed.
#[cfg(test)]
mod regression_tests {
    use super::*;

    fn git_here() {
        assert!(
            Command::new("git")
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success()),
            "git is required: this test asserts what a real git process does"
        );
    }

    fn ident() -> CommitIdentity {
        CommitIdentity {
            name: "TentaFlow Code Studio".into(),
            email: "code-studio@tentaflow.local".into(),
        }
    }

    fn write_commit(
        broker: &Broker,
        branch: &str,
        base: &str,
        expected_old: Option<&str>,
        extra_parent: Option<&str>,
        files: Vec<CommitFile>,
    ) -> CommitOutcome {
        broker
            .build_commit(
                &broker.reference(),
                &CommitSpec {
                    base_commit: base.to_string(),
                    extra_parent: extra_parent.map(str::to_string),
                    branch: branch.to_string(),
                    expected_old: expected_old.map(str::to_string),
                    message: "critic".into(),
                    author: ident(),
                    committer: ident(),
                    files,
                },
            )
            .expect("build_commit")
    }

    fn file(path: &str, content: &str) -> CommitFile {
        CommitFile {
            path: path.into(),
            old_path: None,
            mode: "100644".into(),
            change: CommitChange::Write {
                content: content.as_bytes().to_vec(),
            },
        }
    }

    /// Regression — `merge_into_integration` decides "fast forward" by asking
    /// whether HEAD has a second parent. That is a property of the SOURCE
    /// COMMIT, not of the merge. When the session branch tip is itself a merge
    /// commit, a genuine fast-forward is reported as a non-fast-forward, and
    /// §11.6 makes a non-fast-forward an explicit user decision.
    #[test]
    fn a_fast_forward_onto_a_merge_tip_is_reported_as_a_fast_forward() {
        git_here();
        let dir = tempfile::tempdir().unwrap();
        let broker = Broker::at(dir.path());
        let root = broker.init_repository("main").unwrap();
        let base = write_commit(
            &broker,
            "main",
            &root.head_commit,
            Some(&root.head_commit),
            None,
            vec![file("f.txt", "base\n")],
        );

        // Two independent lines of work, both descending from `base`.
        let left = write_commit(
            &broker,
            "cs/u/s1",
            &base.commit_oid,
            None,
            None,
            vec![file("left.txt", "l\n")],
        );
        let right = write_commit(
            &broker,
            "side",
            &base.commit_oid,
            None,
            None,
            vec![file("right.txt", "r\n")],
        );
        // The session branch tip becomes a merge commit — perfectly ordinary
        // once an agent has merged anything into its own branch.
        let session_tip = write_commit(
            &broker,
            "cs/u/s1",
            &left.commit_oid,
            Some(&left.commit_oid),
            Some(&right.commit_oid),
            vec![file("both.txt", "b\n")],
        );

        // main never moved, so `base` is an ancestor of the session tip: this
        // merge IS a fast-forward.
        let target_before = broker
            .read_ref(&broker.reference(), "refs/heads/main")
            .unwrap()
            .unwrap();
        assert_eq!(target_before, base.commit_oid);
        broker
            .add_integration_worktree("s-1", "op-1", &target_before)
            .unwrap();
        let MergeOutcome::Clean {
            merge_head,
            fast_forward,
        } = broker.merge_into_integration("s-1", "cs/u/s1").unwrap()
        else {
            panic!("expected a clean merge");
        };

        assert_eq!(
            merge_head, session_tip.commit_oid,
            "HEAD landed exactly on the source tip, which is what a fast-forward means"
        );
        assert!(
            fast_forward,
            "a fast-forward was reported as a merge because the SOURCE tip has two parents"
        );
    }

    /// Regression — `build_commit` applies `--force-remove <old_path>` while
    /// walking `files` in order. A patch set that renames `a` to `b` AND
    /// creates a new file at `a` loses the new `a` whenever the rename entry
    /// comes second. `patch::accepted_commit_spec` emits the entries in the
    /// patch set's own row order, so the caller cannot prevent this.
    #[test]
    fn a_rename_keeps_a_file_recreated_at_the_old_path() {
        git_here();
        let dir = tempfile::tempdir().unwrap();
        let broker = Broker::at(dir.path());
        let root = broker.init_repository("main").unwrap();
        let base = write_commit(
            &broker,
            "main",
            &root.head_commit,
            Some(&root.head_commit),
            None,
            vec![file("a.txt", "original\n")],
        );

        // "Rename a.txt to b.txt, and put a fresh a.txt in its place."
        let out = write_commit(
            &broker,
            "main",
            &base.commit_oid,
            Some(&base.commit_oid),
            None,
            vec![
                file("a.txt", "brand new\n"),
                CommitFile {
                    path: "b.txt".into(),
                    old_path: Some("a.txt".into()),
                    mode: "100644".into(),
                    change: CommitChange::Write {
                        content: b"original\n".to_vec(),
                    },
                },
            ],
        );

        let paths: Vec<String> = broker
            .list_tree(&broker.reference(), &out.tree_oid)
            .unwrap()
            .into_iter()
            .map(|e| e.path)
            .collect();
        assert!(
            paths.contains(&"a.txt".to_string()),
            "the accepted new a.txt was removed by the rename step; tree = {paths:?}"
        );
        assert!(paths.contains(&"b.txt".to_string()), "tree = {paths:?}");
    }

    /// Regression — `push_branch` runs its `remote` argument through
    /// `remote_policy::validate_remote`, which only understands a URL. The one
    /// caller on the agent tool surface (`tools.rs:1684`) defaults that
    /// argument to the NAME `"origin"`, so `core.git_push` — one of the three
    /// `mandatory_interactive` operations of §9.3 — cannot succeed with its own
    /// default. Nothing in the suite pushes, so nothing notices.
    #[test]
    fn push_accepts_the_remote_name_its_caller_defaults_to() {
        git_here();
        let dir = tempfile::tempdir().unwrap();
        let broker = Broker::at(dir.path());
        broker.init_repository("main").unwrap();
        let handle = broker.reference();

        // A name the repository does not know is a plain "no such remote", not
        // a parse error about a url nobody wrote.
        let pushed = broker.push_branch(&handle, "origin", "main", &GitAuth::None);
        let message = pushed.as_ref().err().map(|e| e.to_string()).unwrap_or_default();
        assert!(
            !message.contains("invalid remote url"),
            "core.git_push's default remote is refused before git is ever reached: {message}"
        );
        assert!(message.contains("no remote called origin"), "{message}");

        // A name the repository DOES know resolves to its url — and the url
        // then goes through the identical policy, because repository config is
        // data the repository carries.
        broker
            .run(
                Some(&handle),
                &["config", "remote.origin.url", "https://127.0.0.1/repo.git"],
                &GitAuth::None,
            )
            .unwrap();
        let refused = broker
            .push_branch(&handle, "origin", "main", &GitAuth::None)
            .expect_err("a loopback remote must be refused however it was named");
        assert!(
            refused.to_string().contains("forbidden address")
                || refused.to_string().contains("metadata"),
            "the resolved url skipped the policy: {refused}"
        );
    }

    /// Regression — `Broker::exec` read every git process's stdout into memory
    /// with no ceiling, so `cat_file` on a large blob, or `status` in a
    /// repository with a million untracked files, was an out-of-memory away.
    /// §7.8 caps one command's output at 1 MiB.
    #[test]
    fn an_oversized_object_is_refused_rather_than_truncated() {
        git_here();
        let dir = tempfile::tempdir().unwrap();
        let broker = Broker::at(dir.path());
        broker.init_repository("main").unwrap();
        let handle = broker.reference();

        let big = vec![b'x'; 9 * 1024 * 1024];
        let oid = broker.hash_object(&handle, &big).unwrap();
        let error = broker
            .cat_file(&handle, &oid)
            .expect_err("an 8 MiB blob came back through a 1 MiB ceiling");
        assert!(
            error.to_string().contains("ceiling"),
            "refused for the wrong reason: {error}"
        );

        // Half a blob is worse than no blob — it would be committed back as the
        // file's new content — so the ceiling refuses instead of truncating,
        // while everything under it still round-trips byte for byte.
        let ordinary = vec![b'y'; 64 * 1024];
        let oid = broker.hash_object(&handle, &ordinary).unwrap();
        assert_eq!(broker.cat_file(&handle, &oid).unwrap(), ordinary);
    }

    /// Regression — the writer filled git's stdin to the end before anything
    /// read its output, so a command answering with more than one pipe buffer
    /// while the parent was still writing deadlocked a request thread.
    #[test]
    fn a_command_that_answers_while_being_written_to_does_not_deadlock() {
        git_here();
        let dir = tempfile::tempdir().unwrap();
        let broker = Broker::at(dir.path());
        broker.init_repository("main").unwrap();
        let handle = broker.reference();

        // 4 MiB of input, far past any pipe buffer, on a command that writes
        // back as it reads.
        let payload = vec![b'z'; 4 * 1024 * 1024];
        let oid = broker.hash_object(&handle, &payload).unwrap();
        assert_eq!(oid.len(), 40);
    }

    /// Regression — §24 requires a test proving a swapped `gitdir:` pointer in
    /// the worktree does not move the broker. The suite has none: the closest
    /// tests check path arithmetic and an inherited `GIT_DIR` env var. This is
    /// that test.
    #[test]
    fn a_swapped_gitdir_pointer_does_not_move_the_broker() {
        git_here();
        let dir = tempfile::tempdir().unwrap();
        let broker = Broker::at(dir.path());
        broker.init_repository("main").unwrap();
        let worktree = broker
            .add_session_worktree("s-1", "cs/u/s1", "main")
            .unwrap();

        // A foreign repository the agent would like the broker to operate on.
        let elsewhere = tempfile::tempdir().unwrap();
        let foreign = Broker::at(elsewhere.path());
        foreign.init_repository("main").unwrap();

        // The agent owns the worktree, so it owns this pointer file.
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", foreign.reference().git_dir.display()),
        )
        .unwrap();

        std::fs::write(worktree.join("planted.txt"), "agent\n").unwrap();
        let status = broker.status("s-1").expect("status after the pointer swap");
        assert!(
            status.iter().any(|e| e.ends_with("planted.txt")),
            "the broker followed the swapped pointer instead of its own map: {status:?}"
        );

        // Reading was never the whole risk. `git worktree remove` VALIDATES the
        // pointer file and refuses when it does not point back, which left the
        // session unclosable and — after a finalised merge — reported a merge
        // that had happened as a failure.
        broker
            .remove_session_worktree("s-1")
            .expect("worktree removal must not depend on the pointer file either");
        assert!(!worktree.exists(), "the worktree directory survived");
        let admin = broker
            .reference()
            .git_dir
            .join("worktrees")
            .join("s-1");
        assert!(
            !admin.exists(),
            "the administrative entry was left behind, so the id cannot be reused"
        );
        // The branch is the session's work and is NOT removed with the worktree.
        assert!(broker
            .read_ref(&broker.reference(), "refs/heads/cs/u/s1")
            .unwrap()
            .is_some());
    }

    /// Regression — `paths::the_integration_worktree_never_collides_with_the_working_one`
    /// asserts `worktrees/<id>` != `worktrees/<id>-int`, which `format!` makes
    /// true for free. The collision that actually exists is between the WORKING
    /// worktree of session `<x>-int` and the INTEGRATION worktree of session
    /// `<x>`; `validate_session_id` permits both ids, and nothing separates the
    /// two namespaces.
    #[test]
    fn a_session_named_x_int_has_its_own_worktree() {
        let broker = Broker::at("/data/code-studio/ws-1");
        assert_ne!(
            broker.session_worktree("s-1-int").unwrap(),
            broker.integration_worktree("s-1").unwrap(),
            "the working worktree of one session is the integration worktree of another"
        );
    }
}
