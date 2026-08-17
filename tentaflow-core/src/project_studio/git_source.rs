// ===== File: project_studio/git_source.rs — git-backed knowledge sources (F3) =====
//
// Clones a repository with the SYSTEM `git` binary (same approach as
// `deploy::python_venv`, no libgit2 in the tree) into
// `<cache>/project-studio/<project_id>/sources/<source_id>/` and keeps the
// working tree there so a refresh can fast-forward instead of re-downloading.
//
// SECRET HANDLING: the access token is stored encrypted in `sources.secret_enc`
// and is only ever materialised inside the credential URL handed to the child
// process. The command line is never logged — every log line prints the
// redacted url — and `GIT_TERMINAL_PROMPT=0` plus an empty credential helper
// make a wrong token fail fast instead of hanging on a prompt.
//
// SSRF: a repository url is a server-side fetch of a caller-supplied address,
// so `clone`/`refresh` refuse a host that reaches a private/LAN/loopback
// address — the same classification the test environments use, but without the
// admin-approval escape hatch (a code source has no approval queue).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Result};
use url::Url;

/// Time budget of a single git invocation. A clone of a source that big is not
/// a knowledge base — the cap keeps a hung transfer from pinning a worker.
const GIT_TIMEOUT_SECS: u64 = 600;

/// Root of the working trees of one project's code sources.
pub fn sources_root(project_id: &str) -> PathBuf {
    crate::paths::cache_dir()
        .join("project-studio")
        .join(project_id)
        .join("sources")
}

/// Working tree of one source. `source_id` is a server-minted UUID; the guard
/// keeps a hostile value from escaping the root even so.
pub fn source_dir(project_id: &str, source_id: &str) -> Result<PathBuf> {
    super::project_db::validate_project_id(project_id)?;
    if source_id.is_empty()
        || source_id.len() > 64
        || !source_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        bail!("invalid source_id");
    }
    Ok(sources_root(project_id).join(source_id))
}

/// Removes the working tree of a source (called on source delete).
pub fn remove_source_dir(project_id: &str, source_id: &str) {
    if let Ok(dir) = source_dir(project_id, source_id) {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Parsed `config_json` of a git source.
#[derive(Debug, Clone)]
pub struct GitConfig {
    pub repo_url: String,
    pub branch: String,
    pub subdir: String,
}

/// Validates the `{repo_url, branch?, subdir?}` config of a git source. Only
/// http/https are accepted: ssh would need key material this module has no
/// contract for, and `file://` would turn a knowledge source into a local file
/// read of the server's disk.
pub fn parse_config(config_json: &str) -> Result<GitConfig> {
    let value: serde_json::Value =
        serde_json::from_str(config_json).map_err(|e| anyhow!("invalid config_json: {e}"))?;
    let repo_url = value
        .get("repo_url")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow!("git source requires config_json {{\"repo_url\": ...}}"))?;
    let parsed = Url::parse(repo_url).map_err(|e| anyhow!("invalid repo_url: {e}"))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(anyhow!("repo_url must use http or https"));
    }
    if parsed.host_str().is_none() {
        return Err(anyhow!("repo_url has no host"));
    }
    let branch = value
        .get("branch")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("main")
        .to_string();
    // The branch reaches git as an argument, never a shell word, but a leading
    // dash would still be parsed as an option.
    if branch.starts_with('-') || branch.contains(char::is_whitespace) {
        return Err(anyhow!("invalid branch name"));
    }
    let subdir = value
        .get("subdir")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("")
        .trim_matches('/')
        .to_string();
    if subdir.split('/').any(|p| p == "..") {
        return Err(anyhow!("subdir must not traverse out of the repository"));
    }
    Ok(GitConfig {
        repo_url: repo_url.to_string(),
        branch,
        subdir,
    })
}

/// Whether the repository host reaches a non-public address. Blocking (DNS) —
/// call from `spawn_blocking`.
pub fn repo_url_is_private(repo_url: &str) -> Result<bool> {
    let url = Url::parse(repo_url).map_err(|e| anyhow!("invalid repo_url: {e}"))?;
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("repo_url has no host"))?
        .to_ascii_lowercase();
    let port = url
        .port_or_known_default()
        .ok_or_else(|| anyhow!("repo_url has no port"))?;
    Ok(super::environments::host_is_private(&host, port))
}

/// Refuses a repository on a private address. Enforced at the moment the URL is
/// actually fetched, so a config edited after creation cannot slip past.
fn ensure_public_repo_url(repo_url: &str) -> Result<()> {
    if repo_url_is_private(repo_url)? {
        bail!(
            "repo_url points at a private, LAN or loopback address — use a publicly \
             reachable repository or ask an administrator to mirror it"
        );
    }
    Ok(())
}

/// Splits an incoming git `config_json` into `(config without the token,
/// token)`. A private repository is configured in one step, but the token is
/// NEVER stored in `config_json` (which is read back by every source list) —
/// it moves into the encrypted `sources.secret_enc` column instead.
pub fn split_token(config_json: &str) -> Result<(String, String)> {
    let mut value: serde_json::Value =
        serde_json::from_str(config_json).map_err(|e| anyhow!("invalid config_json: {e}"))?;
    let token = value
        .as_object_mut()
        .and_then(|map| map.remove("token"))
        .and_then(|t| t.as_str().map(|s| s.trim().to_string()))
        .unwrap_or_default();
    Ok((value.to_string(), token))
}

/// Injects the access token into the clone url. Returned value is SECRET —
/// never log it, never persist it.
fn authenticated_url(repo_url: &str, token: &str) -> Result<String> {
    if token.is_empty() {
        return Ok(repo_url.to_string());
    }
    let mut url = Url::parse(repo_url).map_err(|e| anyhow!("invalid repo_url: {e}"))?;
    // GitHub/GitLab/Bitbucket all accept a PAT as the password of an arbitrary
    // user; `x-access-token` is the portable spelling.
    url.set_username("x-access-token")
        .map_err(|_| anyhow!("repo_url does not accept credentials"))?;
    url.set_password(Some(token))
        .map_err(|_| anyhow!("repo_url does not accept credentials"))?;
    Ok(url.to_string())
}

/// Upper bound on what one git invocation may hand back on a pipe. `git` is
/// only asked for a commit hash and progress text; anything beyond this is
/// dropped so a hostile remote cannot make the server buffer its output.
const MAX_PIPE_BYTES: usize = 1024 * 1024;

/// Drains a child pipe on its own thread, keeping at most `MAX_PIPE_BYTES`.
fn drain_pipe<R: std::io::Read + Send + 'static>(
    mut pipe: R,
) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if out.len() < MAX_PIPE_BYTES {
                        let room = MAX_PIPE_BYTES - out.len();
                        out.extend_from_slice(&chunk[..n.min(room)]);
                    }
                }
            }
        }
        out
    })
}

/// Runs one git invocation, capturing stdout+stderr. `redacted` is what goes
/// into the error message — the real argv may contain the token.
///
/// Both pipes are drained CONCURRENTLY with the wait loop: git writes its
/// progress to stderr, and a full pipe buffer would block the child until the
/// timeout instead of letting it finish.
fn run_git(args: &[&str], cwd: Option<&Path>, redacted: &str) -> Result<String> {
    let mut command = Command::new("git");
    command
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .env("GCM_INTERACTIVE", "never")
        // No global/system config: a developer machine's insteadOf rewrites or
        // credential helpers must not change what the server clones.
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    let mut child = command
        .spawn()
        .map_err(|e| anyhow!("git is not available on this node: {e}"))?;
    let stdout_pipe = child.stdout.take().map(drain_pipe);
    let stderr_pipe = child.stderr.take().map(drain_pipe);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(GIT_TIMEOUT_SECS);
    let mut status = None;
    loop {
        match child.try_wait() {
            Ok(Some(code)) => {
                status = Some(code);
                break;
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("git {redacted} failed: {e}");
            }
        }
    }
    // On a timeout the drain threads are left detached on purpose: a lingering
    // helper process (git-remote-https) could still hold the write end, and
    // joining would move the hang from the child onto this worker. They exit on
    // their own once the pipe closes.
    let Some(status) = status else {
        bail!("git {redacted} timed out after {GIT_TIMEOUT_SECS}s");
    };
    let stdout = stdout_pipe
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();
    let stderr = stderr_pipe
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        let tail: String = stderr.chars().rev().take(1000).collect::<String>().chars().rev().collect();
        bail!("git {redacted} failed: {}", tail.trim());
    }
    Ok(String::from_utf8_lossy(&stdout).to_string())
}

/// Removes the access token from text that reaches the client. git echoes the
/// remote url in most of its errors, and the url carries the credential.
pub fn scrub_token(text: &str, token: &str) -> String {
    if token.len() < 4 {
        return text.to_string();
    }
    text.replace(token, "***")
}

/// Result of a clone/refresh: the checked-out tree root (honouring `subdir`)
/// and the resolved commit.
#[derive(Debug, Clone)]
pub struct Checkout {
    pub tree_root: PathBuf,
    pub commit: String,
}

fn resolve_tree_root(clone_dir: &Path, subdir: &str) -> Result<PathBuf> {
    if subdir.is_empty() {
        return Ok(clone_dir.to_path_buf());
    }
    let candidate = clone_dir.join(subdir);
    let canonical = candidate
        .canonicalize()
        .map_err(|_| anyhow!("subdir '{subdir}' does not exist in the repository"))?;
    let root = clone_dir
        .canonicalize()
        .map_err(|e| anyhow!("clone dir unreadable: {e}"))?;
    if !canonical.starts_with(&root) {
        bail!("subdir '{subdir}' escapes the repository");
    }
    Ok(canonical)
}

fn head_commit(clone_dir: &Path) -> Result<String> {
    Ok(run_git(&["rev-parse", "HEAD"], Some(clone_dir), "rev-parse")?
        .trim()
        .to_string())
}

/// Shallow-clones the repository into the source's working tree, replacing any
/// previous content. Blocking — call from `spawn_blocking`.
pub fn clone(project_id: &str, source_id: &str, config: &GitConfig, token: &str) -> Result<Checkout> {
    ensure_public_repo_url(&config.repo_url)?;
    let dir = source_dir(project_id, source_id)?;
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    let url = authenticated_url(&config.repo_url, token)?;
    let dir_str = dir.to_string_lossy().to_string();
    run_git(
        &[
            "clone",
            "--depth",
            "1",
            "--single-branch",
            "--branch",
            &config.branch,
            &url,
            &dir_str,
        ],
        None,
        &format!("clone {} ({})", config.repo_url, config.branch),
    )?;
    Ok(Checkout {
        commit: head_commit(&dir)?,
        tree_root: resolve_tree_root(&dir, &config.subdir)?,
    })
}

/// Fast-forwards an existing working tree. A diverged branch (force push,
/// rebase) or a missing/corrupt tree cannot be fast-forwarded, so the source is
/// re-cloned from scratch — the delta re-ingest then simply sees a large diff.
pub fn refresh(
    project_id: &str,
    source_id: &str,
    config: &GitConfig,
    token: &str,
) -> Result<Checkout> {
    ensure_public_repo_url(&config.repo_url)?;
    let dir = source_dir(project_id, source_id)?;
    if !dir.join(".git").is_dir() {
        return clone(project_id, source_id, config, token);
    }
    let url = authenticated_url(&config.repo_url, token)?;
    let redacted = format!("fetch {} ({})", config.repo_url, config.branch);
    let fetched = run_git(
        &[
            "fetch",
            "--depth",
            "1",
            "--no-tags",
            &url,
            &format!("+refs/heads/{0}:refs/remotes/tf/{0}", config.branch),
        ],
        Some(&dir),
        &redacted,
    );
    if fetched.is_err() {
        return clone(project_id, source_id, config, token);
    }
    // `--ff-only` is the whole point: it fails on divergence instead of writing
    // a merge commit into a tree nobody will ever push.
    let target = format!("refs/remotes/tf/{}", config.branch);
    if run_git(&["merge", "--ff-only", &target], Some(&dir), "merge --ff-only").is_err() {
        // Diverged history (force push / rebase) — a full re-clone is the only
        // correct recovery for a depth-1 tree.
        return clone(project_id, source_id, config, token);
    }
    Ok(Checkout {
        commit: head_commit(&dir)?,
        tree_root: resolve_tree_root(&dir, &config.subdir)?,
    })
}

/// `(path -> sha256)` of the files currently recorded for a source; the base
/// side of the refresh delta.
pub fn stored_file_hashes(
    pool: &crate::db::DbPool,
    source_id: &str,
) -> Result<HashMap<String, String>> {
    let conn = pool
        .read()
        .map_err(|e| anyhow!("project_studio git read: {e}"))?;
    let mut stmt =
        conn.prepare("SELECT path, sha256 FROM source_files WHERE source_id = ?1")?;
    let rows = stmt.query_map(rusqlite::params![source_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let (path, sha) = row?;
        out.insert(path, sha);
    }
    Ok(out)
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn parse_config_rejects_non_http_schemes_and_option_branches() {
        let ok = parse_config(r#"{"repo_url":"https://example.com/org/repo.git"}"#).expect("ok");
        assert_eq!(ok.branch, "main");
        assert_eq!(ok.subdir, "");

        let with_subdir = parse_config(
            r#"{"repo_url":"https://example.com/r.git","branch":"develop","subdir":"/api/src/"}"#,
        )
        .expect("subdir");
        assert_eq!(with_subdir.branch, "develop");
        assert_eq!(with_subdir.subdir, "api/src");

        assert!(parse_config(r#"{"repo_url":"ssh://git@example.com/r.git"}"#).is_err());
        assert!(parse_config(r#"{"repo_url":"file:///etc"}"#).is_err());
        assert!(parse_config("{}").is_err());
        assert!(parse_config(r#"{"repo_url":"https://e.com/r","branch":"--upload-pack=x"}"#).is_err());
        assert!(parse_config(r#"{"repo_url":"https://e.com/r","subdir":"../../etc"}"#).is_err());
    }

    #[test]
    fn authenticated_url_embeds_the_token_only_when_present() {
        assert_eq!(
            authenticated_url("https://example.com/r.git", "").expect("plain"),
            "https://example.com/r.git"
        );
        let with_token =
            authenticated_url("https://example.com/r.git", "ghp_secret").expect("token");
        assert!(with_token.contains("x-access-token"));
        assert!(with_token.contains("ghp_secret"));
    }

    /// CR-006: a clone is a server-side fetch of a caller-supplied address, so
    /// a repository on the LAN, on loopback or on the cloud metadata address is
    /// refused BEFORE git is spawned — including on a refresh, where the config
    /// may have been edited after the source was created.
    #[test]
    fn private_repo_urls_are_refused() {
        for private in [
            "http://127.0.0.1/git/secret.git",
            "http://10.0.0.5/git/secret.git",
            "http://192.168.1.4:3000/team/repo.git",
            "http://169.254.169.254/latest/meta-data",
            "http://localhost:7990/scm/x.git",
            "http://gitea.local/x.git",
        ] {
            assert!(
                repo_url_is_private(private).expect(private),
                "{private} must classify as private"
            );
            let config = GitConfig {
                repo_url: private.to_string(),
                branch: "main".to_string(),
                subdir: String::new(),
            };
            let project = "0a1b2c3d-4e5f-4a6b-8c9d-0e1f2a3b4c5d";
            let err = clone(project, "src-1", &config, "").expect_err(private);
            assert!(
                err.to_string().contains("private"),
                "clone must refuse {private}, got: {err}"
            );
            let err = refresh(project, "src-1", &config, "").expect_err(private);
            assert!(err.to_string().contains("private"), "refresh must refuse {private}");
        }
        assert!(!repo_url_is_private("https://1.1.1.1/x.git").expect("public"));
    }

    #[test]
    fn scrub_token_removes_the_credential_from_client_text() {
        let stderr = "fatal: authentication failed for \
                      'https://x-access-token:ghp_topsecret@example.com/r.git'";
        let scrubbed = scrub_token(stderr, "ghp_topsecret");
        assert!(!scrubbed.contains("ghp_topsecret"));
        assert!(scrubbed.contains("***"));
        // An empty/short token must not turn the message into confetti.
        assert_eq!(scrub_token(stderr, ""), stderr);
    }

    #[test]
    fn source_dir_rejects_traversal() {
        assert!(source_dir("0a1b2c3d-4e5f-4a6b-8c9d-0e1f2a3b4c5d", "../evil").is_err());
        assert!(source_dir("0a1b2c3d-4e5f-4a6b-8c9d-0e1f2a3b4c5d", "a/b").is_err());
        assert!(source_dir("../etc", "s1").is_err());
        assert!(source_dir("0a1b2c3d-4e5f-4a6b-8c9d-0e1f2a3b4c5d", "s1").is_ok());
    }
}
