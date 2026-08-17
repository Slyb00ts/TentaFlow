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
        let out = self.run(
            Some(&self.reference()),
            &[
                "worktree",
                "remove",
                "--force",
                &worktree.display().to_string(),
            ],
            &GitAuth::None,
        )?;
        ok_or_stderr(out, "git worktree remove")?;
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

    pub fn head_commit(&self, handle: &RepoHandle) -> Result<String> {
        let out = self.run(Some(handle), &["rev-parse", "HEAD"], &GitAuth::None)?;
        ok_or_stderr(out, "git rev-parse HEAD")
    }

    /// Runs one git invocation with the full hardening in place.
    fn run(&self, handle: Option<&RepoHandle>, args: &[&str], auth: &GitAuth) -> Result<Output> {
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
        for setting in hardening_args(&hooks) {
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
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in &inv.envs {
            command.env(key, value);
        }
        command
            .output()
            .map_err(|e| anyhow!("git is not available on this node: {e}"))
    }

    /// Prepares the environment for one authenticated call.
    ///
    /// The token never reaches argv, the URL or the process environment: it is
    /// written to a 0600 file that a 0700 `GIT_ASKPASS` helper prints, and both
    /// are deleted when the invocation is dropped — whatever the outcome. This
    /// is the transitional variant named in §11.3; the durable one is a broker
    /// socket, which arrives with the sandbox shim.
    fn apply_auth(&self, auth: &GitAuth, inv: &mut Invocation) -> Result<()> {
        match auth {
            GitAuth::None => {}
            GitAuth::Token(token) => {
                let dir = self.broker_dir()?;
                let secret = dir.join("askpass.secret");
                let helper = dir.join("askpass.sh");
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
                let key = dir.join("id_session");
                write_private_file(&key, private_key, false)?;
                inv.scratch.push(key.clone());

                // `accept-new` is refused on purpose: it trusts whatever
                // answers the first time. The fingerprint is pinned when the
                // repository is added, shown to the user, and enforced after.
                let known_host = known_host.as_ref().ok_or_else(|| {
                    anyhow!("ssh remote has no pinned host key; pin it before cloning")
                })?;
                let known = dir.join("known_hosts");
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
        "diff.external=".to_string(),
        "core.pager=cat".to_string(),
        "core.attributesFile=/dev/null".to_string(),
        // `ext::` executes a command; `file` is limited to user-initiated use.
        "protocol.ext.allow=never".to_string(),
        "protocol.file.allow=user".to_string(),
        // A redirect can move an authenticated fetch to another host.
        "http.followRedirects=false".to_string(),
        // A submodule is another remote, fetched implicitly. Never automatic.
        "submodule.recurse=false".to_string(),
        "gc.auto=0".to_string(),
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

#[cfg(unix)]
fn write_private_file(path: &Path, contents: &str, executable: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, contents).map_err(|e| anyhow!("write {}: {e}", path.display()))?;
    let mode = if executable { 0o700 } else { 0o600 };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|e| anyhow!("chmod {}: {e}", path.display()))
}

#[cfg(not(unix))]
fn write_private_file(path: &Path, contents: &str, _executable: bool) -> Result<()> {
    // Windows inherits the ACL of the broker directory, which is created under
    // the service account's data root.
    std::fs::write(path, contents).map_err(|e| anyhow!("write {}: {e}", path.display()))
}

fn shell_single_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

fn ok_or_stderr(output: Output, what: &str) -> Result<String> {
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(anyhow!("{what} failed: {}", stderr.trim()))
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
    fn the_hardening_list_covers_every_setting_a_hostile_repo_could_use() {
        let joined = hardening_args(Path::new("/tmp/no-hooks")).join(" ");
        for expected in [
            "core.hooksPath=/tmp/no-hooks",
            "credential.helper=",
            "core.fsmonitor=false",
            "diff.external=",
            "core.pager=cat",
            "protocol.ext.allow=never",
            "protocol.file.allow=user",
            "http.followRedirects=false",
            "submodule.recurse=false",
        ] {
            assert!(joined.contains(expected), "missing hardening: {expected}");
        }
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

    #[test]
    fn secret_material_does_not_survive_the_invocation() {
        let dir = tempfile::tempdir().unwrap();
        let broker = Broker::at(dir.path());
        let secret = broker.broker_dir().unwrap().join("askpass.secret");
        {
            let mut inv = Invocation::default();
            broker
                .apply_auth(&GitAuth::Token("super-secret".into()), &mut inv)
                .unwrap();
            assert!(
                secret.exists(),
                "the helper needs the secret while git runs"
            );
            assert_eq!(
                std::fs::read_to_string(&secret).unwrap(),
                "super-secret",
                "the token must reach the helper unchanged"
            );
            // It is not on the command line and not in the process environment
            // either — only GIT_ASKPASS, which is a path.
            assert!(inv.envs.iter().all(|(_, v)| !v.contains("super-secret")));
        }
        assert!(!secret.exists(), "the token outlived the git invocation");
    }

    #[test]
    fn an_ssh_clone_without_a_pinned_host_key_is_refused() {
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
    }

    #[test]
    fn init_then_worktree_then_status_works_against_real_git() {
        if !git_available() {
            eprintln!("skipping: git is not installed");
            return;
        }
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
        if !git_available() {
            eprintln!("skipping: git is not installed");
            return;
        }
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
        assert!(!dir.path().join("HOOK_RAN").exists(), "the hook executed");
    }

    #[test]
    fn an_inherited_git_dir_cannot_redirect_the_broker() {
        if !git_available() {
            eprintln!("skipping: git is not installed");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let broker = Broker::at(dir.path());
        broker.init_repository("main").unwrap();

        // A hostile environment aiming git at another repository is stripped:
        // the broker's explicit --git-dir is what counts.
        std::env::set_var("GIT_DIR", "/nonexistent/elsewhere.git");
        let head = broker.head_commit(&broker.reference());
        std::env::remove_var("GIT_DIR");
        assert!(head.is_ok(), "inherited GIT_DIR leaked into the broker");
    }
}
